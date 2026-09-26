//! `cargo xtask <command>`: the repo's tooling, in Rust.
//!
//!   build            build cell/ and agent/ for wasm32 (worker-build 0.8.5)
//!   celld            build the pinned celld fork into target/celld/bin
//!   dev [--clean]    build, then run the stack in the foreground: the cell on
//!                    :8790 (fragments at <label>--<username>.fragment.localhost:8790), the
//!                    code.storage fake on :8792, and agents co-hosted on the
//!                    cell's node (their turns spend their owner's budget: a
//!                    real OpenRouter key per person, minted with the
//!                    management key OPENROUTER_MANAGEMENT_KEY_FILE names)
//!   try <template> [name]
//!                    on the running dev stack: a fragment from a template
//!                    (todo, inbox, notes, chat), scaffolded under target/devstack/try
//!                    so nothing lands in the repo; prints what to open and paste
//!   e2e [args...]    build, then run crates/e2e (args pass through: --only <section>[,...]
//!                    or --except <section>[,...]);
//!                    with --fleet <fleet> first, run its hosted sections against
//!                    that fleet instead
//!   e2e-kit <file> [cell] [agent] [native]
//!                    build what the e2e runs and pack it into one .tar.gz, for
//!                    CI's shards: unpacked at the repo root, they run
//!                    target/release/fragment-e2e with no toolchain; with parts
//!                    named, those alone (CI builds them on two machines)
//!   check            host tests and clippy, warnings denied
//!   deploy <fleet> [--nodes | --secrets]
//!                    ship the cell to a hosted fleet (fleets/<fleet>.json), or
//!                    with --nodes roll its Machines to a new node image (and
//!                    its secrets), with --secrets set its secrets alone
//!   fleet <fleet> <celld command...>
//!                    a celld operator command against the fleet's bucket
//!                    (diagnose, cell list, queue info <queue>, ...)

use std::net::TcpStream;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;

mod deploy;

const WORKER_BUILD_VERSION: &str = "0.8.5";
const DEV_PORT: u16 = 8790;
const DEV_CODESTORAGE_PORT: u16 = 8792;
const DEV_WORKOS_PORT: u16 = 8794;
/// The WorkOS fake's environment in dev.
const DEV_WORKOS_CLIENT: &str = "client_fragment_dev";
const DEV_WORKOS_KEY: &str = "sk_test_fragment_dev";
const DEV_ORG: &str = "fragment-dev";

/// A command as errors show it: the program and its arguments only
/// (`{cmd:?}` would print the environment set on it, and a deploy sets
/// tokens there).
fn shown(cmd: &Command) -> String {
    std::iter::once(cmd.get_program()).chain(cmd.get_args()).map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ")
}

fn run(cmd: &mut Command) -> Result<()> {
    let shown = shown(cmd);
    let status = cmd.status().with_context(|| format!("could not start {shown}"))?;
    if !status.success() {
        bail!("{shown} failed: {status}");
    }
    Ok(())
}

fn build() -> Result<()> {
    build_worker(&devstack::cell_dir())?;
    build_worker(&devstack::agent_dir())
}

/// One celld project (`cell/` or `agent/`) for wasm32, into its `build/`.
/// One at a time: worker-build fetches its tools into one shared cache,
/// and two at once race there.
fn build_worker(dir: &Path) -> Result<()> {
    let out = Command::new("worker-build").arg("--version").output().context(
        "worker-build is not installed: cargo install worker-build --version 0.8.5 --locked",
    )?;
    let version = String::from_utf8_lossy(&out.stdout);
    if !version.contains(WORKER_BUILD_VERSION) {
        bail!("worker-build {WORKER_BUILD_VERSION} is required, found {}", version.trim());
    }
    run(Command::new("worker-build").arg("--release").current_dir(dir))
}

/// Builds the pinned fork. Its seam builds our native services
/// (crates/native) from a fragment-next checkout beside it named
/// `fragment/`: here, a link to this repository, so the node's `KEYS` is
/// always this tree's. `CELLD_FORK_DIR` names a local clone of the fork to
/// fetch the pinned commit from before it is pushed.
fn celld() -> Result<()> {
    let root = devstack::repo_root().join("target/celld");
    let src = root.join("src");
    std::fs::create_dir_all(&root)?;
    let beside = root.join("fragment");
    if std::fs::symlink_metadata(&beside).is_err() {
        std::os::unix::fs::symlink("../..", &beside).context("link target/celld/fragment to this repository")?;
    }
    if !src.join(".git").is_dir() {
        run(Command::new("git").args(["clone", "--quiet", devstack::CELLD_FORK_URL]).arg(&src))?;
    }
    let head = Command::new("git").args(["rev-parse", "HEAD"]).current_dir(&src).output()?;
    if String::from_utf8_lossy(&head.stdout).trim() != devstack::CELLD_FORK_REV {
        match std::env::var_os("CELLD_FORK_DIR") {
            Some(dir) => run(Command::new("git").args(["fetch", "--quiet"]).arg(&dir).arg("+refs/heads/*:refs/remotes/local/*").current_dir(&src))?,
            None => run(Command::new("git").args(["fetch", "--quiet", "origin"]).current_dir(&src))?,
        }
        run(Command::new("git").args(["checkout", "--quiet", "--detach", devstack::CELLD_FORK_REV]).current_dir(&src))?;
    }
    // every time: our crates in it may have changed (a no-op build is quick)
    run(Command::new("cargo").args(["build", "--release", "--locked", "-p", "celld"]).current_dir(&src))?;
    std::fs::create_dir_all(root.join("bin"))?;
    std::fs::copy(src.join("target/release/celld"), devstack::fork_celld_path())?;
    println!("celld fork {} built", &devstack::CELLD_FORK_REV[..7]);
    Ok(())
}

fn dev(args: &[String]) -> Result<()> {
    build()?;
    let tools = devstack::Tools::locate()?;
    let clean = args.iter().any(|a| a == "--clean");
    let state = devstack::repo_root().join("target/devstack/codestorage.json");
    if clean {
        let _ = std::fs::remove_file(&state);
    }
    let key = devstack::dev_secret("codestorage-org-key.pem", fragment_fakes::codestorage::generate_org_key_pem)?;
    let fake = fragment_fakes::codestorage::CodeStorage::start(fragment_fakes::codestorage::Options {
        org: DEV_ORG.into(),
        org_key_pem: Some(key.clone()),
        state_file: Some(state),
        port: DEV_CODESTORAGE_PORT,
        ..Default::default()
    })?;
    // sign-in: a real WorkOS environment when its files are named (its
    // redirect URI must include http://127.0.0.1:8790/auth/callback), else the fake
    let read = |var: &str| -> Result<Option<String>> {
        match std::env::var_os(var) {
            Some(path) => Ok(Some(std::fs::read_to_string(&path).with_context(|| format!("reading {}", Path::new(&path).display()))?.trim().to_string())),
            None => Ok(None),
        }
    };
    let (workos, _workos_fake) = match (read("WORKOS_CLIENT_ID_FILE")?, read("WORKOS_API_KEY_FILE")?) {
        (Some(client_id), Some(api_key)) => (devstack::WorkOsVars { client_id, api_key, api_url: None }, None),
        _ => {
            let fake = fragment_fakes::workos::WorkOs::start_on(DEV_WORKOS_PORT, DEV_WORKOS_CLIENT, DEV_WORKOS_KEY)?;
            (devstack::WorkOsVars { client_id: DEV_WORKOS_CLIENT.into(), api_key: DEV_WORKOS_KEY.into(), api_url: Some(fake.url.clone()) }, Some(fake))
        }
    };
    let workos_label = match &workos.api_url {
        Some(u) => format!("{u} (the fake)"),
        None => format!("WorkOS {}", workos.client_id),
    };
    let node_env = devstack::Fleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        codestorage_org: DEV_ORG.into(),
        codestorage_key_pem: key,
        codestorage_url: fake.url.clone(),
        host_suffix: Some("fragment.localhost".into()),
        legacy_host_suffix: None,
        // No webhooks reach dev fragments (the CLI's refresh and this poll do).
        poll_interval_s: 10,
        egress_local: true,
        job_retry_delay_s: 2,
        blob_grace_s: None,
        openrouter_url: None,
        delivery_retry_s: None,
        workos: Some(workos),
        // the CLI's host: sign-in and approvals happen where it points
        platform_url: Some(format!("http://127.0.0.1:{DEV_PORT}")),
        computers: None,
        // budgets pay for AI only with a management key (real OpenRouter: real money)
        openrouter_management: read("OPENROUTER_MANAGEMENT_KEY_FILE")?,
        budget_usd: None,
        operators: None,
        signins_pending_max: None,
        test_hooks: false,
    }
    .configure(&devstack::cell_dir())?;
    // the agents' script is co-hosted on the same node, as the fleet runs it
    devstack::AgentFleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        fragment_api: format!("http://127.0.0.1:{DEV_PORT}"),
        agent_url: format!("http://127.0.0.1:{DEV_PORT}"),
        openrouter_url: None,
        test_hooks: false,
        egress_local: true,
    }
    .configure(&devstack::agent_dir())?;
    let opts = devstack::NodeOptions {
        project: devstack::cell_dir(),
        port: DEV_PORT,
        clean,
        watch: true,
        env: node_env,
        with: vec![devstack::agent_dir()],
        log_dir: devstack::repo_root().join("target/devstack"),
        // the node's own warnings and information, when asked for
        node_logs: std::env::var_os("FRAGMENT_NODE_LOGS").is_some(),
    };
    let (node, took) = devstack::Node::start(&tools, &opts)?;
    println!("fragment dev: {} (ready in {took:.1?}; Ctrl-C stops it)", node.base);
    println!("  node log:     {}", node.log.display());
    println!("  fragments:    http://<label>--<username>.fragment.localhost:{DEV_PORT}/");
    println!("  agents:       {}/api/agents (co-hosted; signed)", node.base);
    println!("  code.storage: {} (the fake)", fake.url);
    println!("  sign-in:      http://127.0.0.1:{DEV_PORT}/ via {workos_label}");
    println!("  try one:      cargo xtask try todo | inbox   (in another terminal)");
    let status = node.wait()?;
    println!("celld dev exited: {status}");
    Ok(())
}

/// The templates `try` scaffolds.
const TRY_TEMPLATES: [&str; 4] = ["todo", "inbox", "notes", "chat"];

fn try_template(args: &[String]) -> Result<()> {
    let usage = || format!("usage: cargo xtask try <{}> [name]", TRY_TEMPLATES.join("|"));
    let tpl = args.first().ok_or_else(|| anyhow::anyhow!(usage()))?;
    if !TRY_TEMPLATES.contains(&tpl.as_str()) {
        bail!("{}", usage());
    }
    if TcpStream::connect(("127.0.0.1", DEV_PORT)).is_err() {
        bail!("nothing answers on :{DEV_PORT}: start `cargo xtask dev` in another terminal, wait for `ready`, then try again");
    }
    let root = devstack::repo_root();
    run(Command::new("cargo").args(["build", "--quiet", "--manifest-path"]).arg(root.join("Cargo.toml")).args(["-p", "fragment-cli"]))?;
    let cli = root.join("target/debug/fragment");
    let host = format!("http://127.0.0.1:{DEV_PORT}");
    let name = args.get(1).cloned().unwrap_or_else(|| format!("{tpl}-{}", devstack::random_hex(2)));
    let dir = root.join("target/devstack/try");
    std::fs::create_dir_all(&dir)?;
    // your CLI key must be a person's on the dev fleet (phase 4: people sign in)
    let who = Command::new(&cli).args(["whoami"]).env("FRAGMENT_HOST", &host).output()?;
    if !who.status.success() {
        bail!(
            "your CLI key is no one's on the dev stack yet. Once:\n  FRAGMENT_HOST={host} {} login\n(the dev stack signs in through the WorkOS fake: any email)",
            cli.display()
        );
    }
    let out = Command::new(&cli).args(["init", &name, "--template", tpl]).env("FRAGMENT_HOST", &host).current_dir(&dir).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    print!("{text}");
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("fragment init failed (a name already taken? pass another: cargo xtask try {tpl} <name>)");
    }
    let line = |prefix: &str| text.lines().find_map(|l| l.strip_prefix(prefix)).map(str::trim).unwrap_or("").to_string();
    let alias = format!("alias fragment='FRAGMENT_HOST={host} {}'", cli.display());
    println!("\nnext:");
    println!("  open it      {}", line("share link:"));
    if tpl == "inbox" {
        let hook = line("webhook URL:");
        println!("  post to it   curl -s -X POST '{hook}' -H 'content-type: application/json' -d '{{\"payload\":{{\"text\":\"hi\"}}}}'");
        println!("  or a page    curl -s -X POST '{hook}' -H 'content-type: application/json' -d '{{\"payload\":{{\"url\":\"https://example.com\"}}}}'");
    }
    println!("  the CLI      {alias}");
    println!("               fragment runs {name} | fragment channel {name} | fragment call {name} list");
    println!("  change it    edit {}, then: fragment deploy {name} --dir {}", dir.join(&name).display(), dir.join(&name).display());
    Ok(())
}

fn e2e(args: &[String]) -> Result<()> {
    let manifest = devstack::repo_root().join("Cargo.toml");
    // against a hosted fleet: its deployed cell, the CLI built here
    if let [flag, fleet, rest @ ..] = args {
        if flag == "--fleet" {
            run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-cli"]))?;
            return run(Command::new("cargo")
                .args(["run", "--quiet", "--release", "--manifest-path"])
                .arg(&manifest)
                .args(["-p", "fragment-e2e", "--", "--hosted"])
                .args(rest)
                .envs(deploy::hosted_e2e_env(fleet)?));
        }
    }
    build_e2e()?;
    run(Command::new(devstack::repo_root().join(E2E_BIN)).args(args))
}

/// The suite, as `build_e2e` leaves it.
const E2E_BIN: &str = "target/release/fragment-e2e";
/// Where the kit keeps worker-build's esbuild (the node bundles with it).
const KIT_ESBUILD: &str = "target/e2e-kit/esbuild";

/// What the e2e runs: the workers, the CLI (the e2e drives it too,
/// computers included), and the suite.
fn build_e2e() -> Result<()> {
    build_parts(&KitPart::ALL)
}

/// The CLI and the suite, for this machine.
fn build_native() -> Result<()> {
    let manifest = devstack::repo_root().join("Cargo.toml");
    run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-cli", "--features", "computer"]))?;
    run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-e2e"]))
}

/// The kit's parts: unpacked together, the whole kit. CI builds the rest
/// on macOS while Linux builds the cell (with worker-build alone: this
/// workspace would first fetch goose, which the cell does not use).
#[derive(Clone, Copy, PartialEq, Debug)]
enum KitPart {
    /// `cell/` for wasm32 (wasm is wasm: it builds anywhere).
    Cell,
    /// `agent/` for wasm32.
    Agent,
    /// This machine's binaries: the node (`xtask celld` first), the CLI, the
    /// suite, and the esbuild the node bundles with (worker-build's: a
    /// worker has been built on this machine).
    Native,
}

impl KitPart {
    const ALL: [KitPart; 3] = [KitPart::Cell, KitPart::Agent, KitPart::Native];

    fn parse(name: &str) -> Option<KitPart> {
        KitPart::ALL.into_iter().find(|p| p.name() == name)
    }

    fn name(self) -> &'static str {
        match self {
            KitPart::Cell => "cell",
            KitPart::Agent => "agent",
            KitPart::Native => "native",
        }
    }
}

/// Builds the parts: the native binaries alongside the workers, which build
/// one at a time (worker-build fetches its tools into one shared cache, and
/// two at once race there).
fn build_parts(parts: &[KitPart]) -> Result<()> {
    std::thread::scope(|s| {
        let native = parts.contains(&KitPart::Native).then(|| s.spawn(build_native));
        let workers = [(KitPart::Cell, devstack::cell_dir()), (KitPart::Agent, devstack::agent_dir())];
        let workers = workers.iter().filter(|(part, _)| parts.contains(part)).try_for_each(|(_, dir)| build_worker(dir));
        let native = native.map_or(Ok(()), |build| build.join().expect("the native build does not panic"));
        workers.and(native)
    })
}

/// What the e2e runs (all of it, or the parts named), packed at its paths
/// under the repo root: each CI shard unpacks the parts and runs the suite
/// (`CELLD_ESBUILD` set to `KIT_ESBUILD`), no toolchain.
fn e2e_kit(args: &[String]) -> Result<()> {
    let usage = || anyhow::anyhow!("usage: cargo xtask e2e-kit <file.tar.gz> [cell] [agent] [native]");
    let [out, names @ ..] = args else { return Err(usage()) };
    let mut parts = names.iter().map(|n| KitPart::parse(n).ok_or_else(usage)).collect::<Result<Vec<_>>>()?;
    if parts.is_empty() {
        parts = KitPart::ALL.to_vec();
    }
    anyhow::ensure!(KitPart::ALL.iter().all(|p| parts.iter().filter(|q| *q == p).count() <= 1), "a part is named twice");
    build_parts(&parts)?;
    let root = devstack::repo_root();
    let mut paths = vec![];
    for part in parts {
        match part {
            KitPart::Cell => paths.push("cell/build".to_string()),
            KitPart::Agent => paths.push("agent/build".to_string()),
            KitPart::Native => {
                let esbuild = devstack::esbuild()?;
                std::fs::create_dir_all(root.join(KIT_ESBUILD).parent().expect("the kit's directory"))?;
                std::fs::copy(&esbuild, root.join(KIT_ESBUILD)).with_context(|| format!("copy {}", esbuild.display()))?;
                let celld = devstack::fork_celld_path();
                anyhow::ensure!(celld.is_file(), "no node at {} (run `cargo xtask celld` first)", celld.display());
                let celld = celld.strip_prefix(&root).context("the node's binary sits under the repo")?;
                paths.extend(["target/release/fragment", E2E_BIN, KIT_ESBUILD].map(String::from));
                paths.push(celld.to_string_lossy().into_owned());
            }
        }
    }
    run(Command::new("tar").arg("-czf").arg(std::path::absolute(out)?).current_dir(&root).args(paths))
}

/// A merge conflict's markers (git's diff3 style too), at the start of a
/// line: none may be committed.
fn conflict_marker(line: &str) -> bool {
    ["<<<<<<< ", "||||||| ", ">>>>>>> "].iter().any(|m| line.starts_with(m)) || line == "======="
}

/// Fails on a tracked text file holding conflict markers: a rebase that
/// committed one compiles when it lands in docs or a lockfile.
fn no_conflict_markers(root: &Path) -> Result<()> {
    let out = Command::new("git").args(["ls-files", "-z"]).current_dir(root).output().context("git ls-files")?;
    anyhow::ensure!(out.status.success(), "git ls-files: {}", String::from_utf8_lossy(&out.stderr));
    let mut found = vec![];
    for path in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path).into_owned();
        // a file that is not text (a font, an image) holds no markers
        let Ok(text) = std::fs::read_to_string(root.join(&path)) else { continue };
        let at = text.lines().enumerate().filter(|(_, l)| conflict_marker(l)).map(|(i, _)| format!("{path}:{}", i + 1));
        found.extend(at);
    }
    anyhow::ensure!(found.is_empty(), "conflict markers are committed:\n{}", found.join("\n"));
    Ok(())
}

/// CI's e2e shards (`.github/workflows/ci.yml`, each shard's `args:`):
/// the sections the `--only` shards name, each once, are exactly those the
/// one `--except` shard leaves out. Otherwise a section taken off its shard
/// but still left out of the rest would never run, and nothing would say.
fn shards_cover(workflow: &str) -> Result<()> {
    let mut named: Vec<&str> = vec![];
    let mut except: Option<Vec<&str>> = None;
    for args in workflow.lines().filter_map(|l| l.trim().strip_prefix("args: ")) {
        match args.split_once(' ') {
            Some(("--only", names)) => named.extend(names.split(',')),
            Some(("--except", names)) if except.is_none() => except = Some(names.split(',').collect()),
            _ => bail!("a shard runs --only or (one shard) --except <section,...>, not: {args}"),
        }
    }
    let mut except = except.context("no --except shard: a new section would run in none")?;
    let count = named.len();
    named.sort_unstable();
    named.dedup();
    anyhow::ensure!(named.len() == count, "a section is named by two --only shards");
    except.sort_unstable();
    anyhow::ensure!(named == except, "the --only shards name {named:?}, but the --except shard leaves out {except:?}");
    Ok(())
}

fn check() -> Result<()> {
    let root = devstack::repo_root();
    no_conflict_markers(&root)?;
    shards_cover(&std::fs::read_to_string(root.join(".github/workflows/ci.yml")).context("read the CI workflow")?)?;
    let read = |path: &str| std::fs::read_to_string(root.join(path)).with_context(|| format!("read {path}"));
    let copies = ["cli/GUIDE.md", "README.md", "cell/src/auth.rs"].map(|path| read(path).map(|text| (path, text)));
    let copies: Vec<(&str, String)> = copies.into_iter().collect::<Result<_>>()?;
    skill_installs_release(&read("cli/SKILL.md")?, &read(".github/workflows/release.yml")?, &copies)?;
    run(Command::new("cargo").args(["test", "--workspace", "--all-features"]).current_dir(&root))?;
    run(Command::new("cargo")
        .args(["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"])
        .current_dir(&root))?;
    run(Command::new("cargo")
        .args(["clippy", "--target", "wasm32-unknown-unknown", "--", "-D", "warnings"])
        .current_dir(Path::new(&devstack::cell_dir())))?;
    run(Command::new("cargo")
        .args(["clippy", "--target", "wasm32-unknown-unknown", "--", "-D", "warnings"])
        .current_dir(Path::new(&devstack::agent_dir())))
}

/// Where the one-line install fetches the CLI from.
const RELEASES: &str = "https://github.com/futurepaul/fragment/releases/latest/download/";
/// Each target the release builds, and its asset's name: the `uname -s` and
/// `uname -m` of the machines it runs on, which the install command names.
const RELEASE_ASSETS: [(&str, &str); 3] =
    [("aarch64-apple-darwin", "Darwin-arm64"), ("x86_64-apple-darwin", "Darwin-x86_64"), ("x86_64-unknown-linux-musl", "Linux-x86_64")];

/// cli/SKILL.md (`fragment skill`) is a skill, and its one-line install,
/// which the platform's home page, GUIDE.md, and the README copy, fetches
/// the latest release's `fragment-$(uname -s)-$(uname -m).tar.gz`: the
/// release workflow (`.github/workflows/release.yml`) must build exactly
/// those assets, each a tarball of `fragment` alone. Otherwise a renamed
/// asset or a stale copy breaks the install, and nothing says so until
/// someone runs it.
fn skill_installs_release(skill: &str, workflow: &str, copies: &[(&str, String)]) -> Result<()> {
    anyhow::ensure!(skill.starts_with("---\nname: fragment\ndescription: "), "cli/SKILL.md opens with a skill's name and description");
    let install = skill.lines().map(str::trim).find(|l| l.contains(RELEASES)).context("cli/SKILL.md shows no install command")?;
    let fetched = format!("{RELEASES}fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin");
    anyhow::ensure!(install.ends_with(&fetched), "the install command unpacks {fetched}, not: {install}");
    for (path, text) in copies {
        anyhow::ensure!(text.contains(install), "{path} does not show cli/SKILL.md's install command: {install}");
    }
    let field = |key: &str| workflow.lines().filter_map(|l| l.trim().strip_prefix(key)).collect::<Vec<_>>();
    let built: Vec<(&str, &str)> = field("target: ").into_iter().zip(field("asset: ")).collect();
    anyhow::ensure!(built == RELEASE_ASSETS, "the release builds {built:?}, not the assets the install fetches: {RELEASE_ASSETS:?}");
    let packed = r#"tar -czf "$RUNNER_TEMP/fragment-${{ matrix.asset }}.tar.gz" -C "target/${{ matrix.target }}/release" fragment"#;
    anyhow::ensure!(workflow.contains(packed), "each release asset is a tarball of `fragment` alone: {packed}");
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => build(),
        Some("celld") => celld(),
        Some("dev") => dev(&args[1..]),
        Some("try") => try_template(&args[1..]),
        Some("e2e") => e2e(&args[1..]),
        Some("e2e-kit") => e2e_kit(&args[1..]),
        Some("check") => check(),
        Some("deploy") => deploy::run(&args[1..]),
        Some("fleet") => deploy::operate(&args[1..]),
        _ => bail!("usage: cargo xtask build | celld | dev [--clean] | try <template> [name] | e2e [--fleet <fleet>] [--only | --except <section>[,...]] | e2e-kit <file> [cell] [agent] [native] | check | deploy <fleet> [--nodes | --secrets] | fleet <fleet> <celld command...>"),
    }
}

#[cfg(test)]
mod tests {
    /// Git's markers, the diff3 base's included, are caught at a line's
    /// start; text that only mentions one, or a longer rule, is not.
    #[test]
    fn conflict_markers_are_caught_at_a_line_start() {
        for line in ["<<<<<<< HEAD", "||||||| parent of e8dd007 (e2e: …)", "=======", ">>>>>>> e8dd007 (e2e: …)"] {
            assert!(super::conflict_marker(line), "{line}");
        }
        for line in ["  <<<<<<< indented", "a ======= b", "========", "the `<<<<<<< ` marker", ">>>>>>>"] {
            assert!(!super::conflict_marker(line), "{line}");
        }
    }

    #[test]
    fn every_section_a_shard_leaves_out_another_names() {
        let shards = |args: &[&str]| args.iter().map(|a| format!("          - name: s\n            args: {a}\n")).collect::<String>();
        assert!(super::shards_cover(&shards(&["--only a,b", "--only c", "--except c,a,b"])).is_ok());
        // taken off its shard but still left out of the rest: it would run nowhere
        assert!(super::shards_cover(&shards(&["--only a", "--only c", "--except c,a,b"])).is_err());
        assert!(super::shards_cover(&shards(&["--only a,b", "--only b", "--except a,b"])).is_err());
        assert!(super::shards_cover(&shards(&["--only a,b"])).is_err());
        assert!(super::shards_cover(&shards(&["--except a", "--except a"])).is_err());
        assert!(super::shards_cover(&std::fs::read_to_string(super::devstack::repo_root().join(".github/workflows/ci.yml")).unwrap()).is_ok());
    }

    /// CI names the parts (`e2e-kit <file> <part>...`); each name is its
    /// part's, and no other name is one.
    #[test]
    fn a_kit_part_is_named_by_its_name_alone() {
        for part in super::KitPart::ALL {
            assert_eq!(super::KitPart::parse(part.name()), Some(part));
        }
        for name in ["", "all", "Cell", "workers", "native "] {
            assert_eq!(super::KitPart::parse(name), None, "{name:?}");
        }
    }

    /// The repo's own skill, release workflow, and copies agree; a renamed
    /// asset, a stale copy, or a skill without its frontmatter does not.
    #[test]
    fn the_install_fetches_what_the_release_builds() {
        let read = |path: &str| std::fs::read_to_string(super::devstack::repo_root().join(path)).unwrap();
        let (skill, workflow) = (read("cli/SKILL.md"), read(".github/workflows/release.yml"));
        let copies = [("README.md", read("README.md"))];
        assert!(super::skill_installs_release(&skill, &workflow, &copies).is_ok());
        assert!(super::skill_installs_release(&skill, &workflow.replace("asset: Darwin-arm64", "asset: Darwin-aarch64"), &copies).is_err());
        assert!(super::skill_installs_release(&skill, &workflow, &[("README.md", read("README.md").replace("uname -m", "arch"))]).is_err());
        assert!(super::skill_installs_release(&skill.replacen("name: fragment", "title: fragment", 1), &workflow, &copies).is_err());
    }

    #[test]
    fn a_failed_command_never_shows_its_environment() {
        let mut cmd = std::process::Command::new("flyctl");
        cmd.args(["deploy", "--remote-only"]).env("FLY_API_TOKEN", "FlyV1 not-a-real-token");
        assert_eq!(super::shown(&cmd), "flyctl deploy --remote-only");
    }
}
