//! `cargo xtask <command>`: the repo's tooling, in Rust.
//!
//!   build            build cell/ and agent/ for wasm32 (worker-build 0.8.5; in
//!                    parallel once worker-build has fetched its tools: build.rs)
//!   dev [--clean] [--port <p>]
//!                    build, then run the stack in the foreground under
//!                    `wrangler dev`: the cell on :8790 (fragments at
//!                    <label>--<username>.fragment.localhost:8790), the
//!                    code.storage fake on :8792, the Workers AI fake on :8796
//!                    behind the model route, and the agents' Worker beside it
//!                    (their turns spend their owner's ledger; new people are
//!                    seats with the month's included credit). Sign-in is the
//!                    WorkOS fake on :8794, or with FRAGMENT_SIGNIN=oidc the
//!                    OpenID Connect fake on :8798, or a real provider
//!                    (FRAGMENT_OIDC_ISSUER: `dev_oidc`). FRAGMENT_DEV_PORT
//!                    moves the cell and its fakes (`dev_ports`)
//!   try <template> [name]
//!                    on the running dev stack: a fragment from a template
//!                    (todo, inbox, notes), scaffolded under target/devstack/try
//!                    so nothing lands in the repo; prints what to open and paste
//!   e2e [args...]    build, then run crates/e2e (args pass through: --only <section>[,...],
//!                    --except <section>[,...], or --shard <k>/<n> (the table's, as CI
//!                    splits it); --summary <file> writes the run's summary; --rehearse
//!                    keeps the hosted lane's rules on the local node). The build
//!                    builds the computer images ahead of the node beside the Rust.
//!                    CI splits it in two steps, so the cache saves between them:
//!                    --build-only (builds, runs nothing), then --no-build (runs
//!                    what the build left)
//!   e2e-summary <dir>
//!                    CI's `e2e` check: the shards' summaries in <dir> make the
//!                    suite exactly once, and every check passed (summary.rs)
//!   e2e --hosted --config <file> --branch <name> [--only … | --except …]
//!       [--dry-run | --sweep] [--max-paid-calls <n>]
//!                    the suite against that branch deployment on its real vendors
//!                    (crates/e2e/src/hosted.rs); --dry-run prints its plan
//!   check            every first-party JavaScript file parses (node --check;
//!                    xtask/src/js_syntax.rs), then host tests and clippy,
//!                    warnings denied
//!   deploy --config <file> [--branch <name>]
//!                    build and deploy to Cloudflare from a deployment's config
//!                    (deploy/example.jsonc): a branch gets a complete copy of
//!                    its own at <branch>.<zone> (xtask/src/deploy.rs)
//!   teardown --config <file> --branch <name>
//!                    remove a branch deployment (irreversible)
//!
//! dev, e2e, deploy and teardown run wrangler and npm on the pinned Node,
//! and check runs `node --check` on it, fetched into target/tools on first
//! use, never a `node` from PATH (crates/devstack/src/node.rs;
//! FRAGMENT_NODE names another).
//!
//! fragment.club runs on celld from the `celld` branch (the tag celld-final)
//! until the cutover (docs/cloudflare-v1.md, decision 35): `deploy` never
//! reaches it.

use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;

mod build;
mod deploy;
mod dns;
mod js_syntax;
mod summary;

/// The dev stack's ports (`FRAGMENT_DEV_PORT` moves them all, so two stacks
/// share a machine): the cell, then its fakes above it.
const DEV_PORT: u16 = 8790;
const DEV_CODESTORAGE_OFFSET: u16 = 2;
const DEV_WORKOS_OFFSET: u16 = 4;
/// The Workers AI fake behind the model route (`FRAGMENT_AI_URL`): dev
/// never calls real models.
const DEV_AI_OFFSET: u16 = 6;
/// The OpenID Connect fake (`FRAGMENT_SIGNIN=oidc`).
const DEV_OIDC_OFFSET: u16 = 8;
/// The WorkOS fake's environment in dev.
const DEV_WORKOS_CLIENT: &str = "client_fragment_dev";
const DEV_WORKOS_KEY: &str = "sk_test_fragment_dev";
/// The OpenID Connect fake's client in dev.
const DEV_OIDC_CLIENT: &str = "fragment-dev";
const DEV_OIDC_SECRET: &str = "oidc-secret-fragment-dev";
const DEV_ORG: &str = "fragment-dev";

/// The dev stack's base port: `FRAGMENT_DEV_PORT`, else `DEV_PORT`; its
/// fakes take the ports above it, so it leaves room for them.
fn dev_port() -> Result<u16> {
    match std::env::var("FRAGMENT_DEV_PORT") {
        Err(_) => Ok(DEV_PORT),
        Ok(p) => match p.parse::<u16>() {
            Ok(port) if (1024..=u16::MAX - DEV_OIDC_OFFSET).contains(&port) => Ok(port),
            _ => bail!("FRAGMENT_DEV_PORT is a port from 1024 to {}, not {p:?}", u16::MAX - DEV_OIDC_OFFSET),
        },
    }
}

/// A command as errors show it: the program and its arguments only
/// (`{cmd:?}` would print the environment set on it, where a deploy may
/// hand wrangler its token).
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

/// cell/ and agent/ for wasm32 (build.rs).
fn build() -> Result<()> {
    build::workers()
}

fn dev(args: &[String]) -> Result<()> {
    // the runtime: wrangler's local workerd, or celld (docs/self-host.md, seam 1)
    let celld = match args.iter().position(|a| a == "--runtime").and_then(|i| args.get(i + 1)).map(String::as_str) {
        None | Some("wrangler") => false,
        Some("celld") => true,
        Some(other) => bail!("--runtime is wrangler or celld, not {other}"),
    };
    let port = dev_port()?;
    // the pinned Node and node_modules first: a refused FRAGMENT_NODE stops
    // the run before a build
    let tools = devstack::Tools::locate()?;
    build()?;
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
        port: port + DEV_CODESTORAGE_OFFSET,
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
            let fake = fragment_fakes::workos::WorkOs::start_on(port + DEV_WORKOS_OFFSET, DEV_WORKOS_CLIENT, DEV_WORKOS_KEY)?;
            (devstack::WorkOsVars { client_id: DEV_WORKOS_CLIENT.into(), api_key: DEV_WORKOS_KEY.into(), api_url: Some(fake.url.clone()) }, Some(fake))
        }
    };
    let ai = fragment_fakes::workers_ai::WorkersAi::start(port + DEV_AI_OFFSET)?;
    let (oidc, oidc_fake) = dev_oidc(port, &read)?;
    let (model_upstream, node) = self_host(&read)?;
    // computers run on the node when there is one, else in local Docker;
    // with neither, the stack runs without computers, and says so
    // (celld runs no containers of its own: its computers are a node's)
    let docker = !celld && node.is_none() && Command::new(std::env::var("WRANGLER_DOCKER_BIN").unwrap_or_else(|_| "docker".into())).arg("info").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    let computers = node.is_some() || docker;
    let signin_label = match (&oidc, &workos.api_url) {
        (Some(o), _) if oidc_fake.is_some() => format!("{} (the OpenID Connect fake)", o.issuer),
        (Some(o), _) => format!("the OpenID Connect provider {}", o.issuer),
        (None, Some(u)) => format!("{u} (the WorkOS fake)"),
        (None, None) => format!("WorkOS {}", workos.client_id),
    };
    devstack::Fleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        codestorage_org: DEV_ORG.into(),
        codestorage_key_pem: key,
        codestorage_url: fake.url.clone(),
        host_suffix: Some("fragment.localhost".into()),
        legacy_host_suffix: None,
        host_label_suffix: None,
        // No webhooks reach dev fragments (the CLI's refresh and this poll do).
        poll_interval_s: 10,
        egress_local: true,
        job_retry_delay_s: 2,
        blob_grace_s: None,
        // text and images: the Workers AI fake (dev never calls a real model)
        ai_url: Some(ai.url.clone()),
        ai_gateway: None,
        // a dev person is a seat, with the month's included credit
        default_plan: Some("seat".into()),
        delivery_retry_s: None,
        workos: Some(workos),
        oidc,
        // the CLI's host: sign-in and approvals happen where it points
        platform_url: Some(format!("http://127.0.0.1:{port}")),
        operators: None,
        signins_pending_max: None,
        test_secret: None,
        computer_image: computers.then(|| "stub".into()),
        computer_snapshots: false,
        providers: None,
        operator_key_values: vec![],
        swap_upstream: None,
        model_upstream,
        node,
    }
    .configure(&devstack::cell_dir())?;
    // the agents' Worker runs beside it, as a deployment runs it
    devstack::AgentFleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        fragment_api: format!("http://127.0.0.1:{port}"),
        agent_url: format!("http://127.0.0.1:{port}"),
        test_hooks: false,
    }
    .configure(&devstack::agent_dir())?;
    let opts = devstack::NodeOptions {
        project: devstack::cell_dir(),
        port,
        clean,
        with: vec![devstack::agent_dir()],
        log_dir: devstack::repo_root().join("target/devstack"),
        // wrangler's own debug logs, when asked for
        node_logs: std::env::var_os("FRAGMENT_NODE_LOGS").is_some(),
        // in the terminal's group: Ctrl-C stops it with xtask
        own_group: false,
        containers: docker,
    };
    enum Running {
        Wrangler(devstack::Node),
        Celld(devstack::celld::CelldNode),
    }
    let (running, base, log, took) = if celld {
        let tools = devstack::celld::CelldTools::locate()?;
        let copts = devstack::celld::CelldOptions { project: opts.project.clone(), with: opts.with.clone(), port: opts.port, clean: opts.clean, log_dir: opts.log_dir.clone() };
        let (n, took) = devstack::celld::CelldNode::start(&tools, &copts)?;
        let (base, log) = (n.base.clone(), n.log.clone());
        (Running::Celld(n), base, log, took)
    } else {
        let (n, took) = devstack::Node::start(&tools, &opts)?;
        let (base, log) = (n.base.clone(), n.log.clone());
        (Running::Wrangler(n), base, log, took)
    };
    println!("fragment dev: {base} (ready in {took:.1?}; Ctrl-C stops it)");
    println!("  runtime:      {}", if celld { "celld (CELLD_BIN)" } else { "wrangler dev (workerd)" });
    println!("  node log:     {}", log.display());
    println!("  fragments:    http://<label>--<username>.fragment.localhost:{port}/");
    println!("  agents:       {base}/api/agents (beside it; signed)");
    println!("  code.storage: {} (the fake)", fake.url);
    match std::env::var("FRAGMENT_MODEL_URL") {
        Ok(u) => println!("  models:       {u} (a self-hosted model server)"),
        Err(_) => println!("  models:       {} (the Workers AI fake: echoes, never a real model)", ai.url),
    }
    match (std::env::var("FRAGMENT_NODE_URL"), docker) {
        (Ok(u), _) => println!("  computers:    the sandcastle node at {u}"),
        (Err(_), true) => println!("  computers:    local Docker (the stub image)"),
        (Err(_), false) => println!("  computers:    none (no FRAGMENT_NODE_URL, and {})", if celld { "celld runs no containers" } else { "Docker is not reachable" }),
    }

    println!("  sign-in:      http://127.0.0.1:{port}/ via {signin_label}");
    println!("  try one:      cargo xtask try todo | inbox   (in another terminal)");
    let status = match running {
        Running::Wrangler(n) => n.wait()?,
        Running::Celld(n) => n.wait()?,
    };
    println!("the node exited: {status}");
    Ok(())
}

/// Sign-in on OpenID Connect for dev (docs/self-host.md, seam 4), when
/// asked for: a real provider (`FRAGMENT_OIDC_ISSUER`, with
/// `FRAGMENT_OIDC_CLIENT_ID`, the secret's file
/// `FRAGMENT_OIDC_CLIENT_SECRET_FILE`, and optionally `FRAGMENT_OIDC_SCOPES`,
/// `FRAGMENT_OIDC_CLAIMS`, `FRAGMENT_OIDC_AUTH`; its redirect URIs must
/// include `http://127.0.0.1:<port>/auth/callback`), else with
/// `FRAGMENT_SIGNIN=oidc` the fake (any email or username), else none:
/// WorkOS signs people in.
fn dev_oidc(port: u16, read: ReadFile) -> Result<(Option<devstack::OidcVars>, Option<fragment_fakes::oidc::Oidc>)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    if let Some(issuer) = var("FRAGMENT_OIDC_ISSUER") {
        let vars = devstack::OidcVars {
            issuer,
            client_id: var("FRAGMENT_OIDC_CLIENT_ID").context("FRAGMENT_OIDC_ISSUER needs FRAGMENT_OIDC_CLIENT_ID")?,
            client_secret: read("FRAGMENT_OIDC_CLIENT_SECRET_FILE")?,
            scopes: var("FRAGMENT_OIDC_SCOPES"),
            claims: var("FRAGMENT_OIDC_CLAIMS"),
            auth: var("FRAGMENT_OIDC_AUTH"),
        };
        return Ok((Some(vars), None));
    }
    match var("FRAGMENT_SIGNIN").as_deref() {
        None | Some("workos") => Ok((None, None)),
        Some("oidc") => {
            let fake = fragment_fakes::oidc::Oidc::start_on(port + DEV_OIDC_OFFSET, DEV_OIDC_CLIENT, DEV_OIDC_SECRET)?;
            let vars = devstack::OidcVars {
                issuer: fake.url.clone(),
                client_id: DEV_OIDC_CLIENT.into(),
                client_secret: Some(DEV_OIDC_SECRET.into()),
                scopes: None,
                claims: None,
                auth: None,
            };
            Ok((Some(vars), Some(fake)))
        }
        Some(other) => bail!("FRAGMENT_SIGNIN is workos or oidc, not {other:?}"),
    }
}

/// The self-hosted lane's settings for dev (docs/self-host.md), each from
/// the environment, secrets from files: a model server
/// (`FRAGMENT_MODEL_URL`, `FRAGMENT_MODELS`, `FRAGMENT_MODEL_KEY_FILE`) and
/// a sandcastle node for computers (`FRAGMENT_NODE_URL`,
/// `FRAGMENT_NODE_SECRET_FILE`, `FRAGMENT_NODE_IMAGES`).
type ReadFile<'a> = &'a dyn Fn(&str) -> Result<Option<String>>;

fn self_host(read: ReadFile) -> Result<(Option<devstack::ModelUpstreamVars>, Option<devstack::NodeVars>)> {
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let model = match var("FRAGMENT_MODEL_URL") {
        Some(url) => Some(devstack::ModelUpstreamVars {
            url,
            models: var("FRAGMENT_MODELS").context("FRAGMENT_MODEL_URL needs FRAGMENT_MODELS: {\"<catalog id>\": \"<the server's model>\"}")?,
            key: read("FRAGMENT_MODEL_KEY_FILE")?,
        }),
        None => None,
    };
    let node = match var("FRAGMENT_NODE_URL") {
        Some(url) => Some(devstack::NodeVars {
            url,
            secret: read("FRAGMENT_NODE_SECRET_FILE")?.context("FRAGMENT_NODE_URL needs FRAGMENT_NODE_SECRET_FILE")?,
            images: var("FRAGMENT_NODE_IMAGES").context("FRAGMENT_NODE_URL needs FRAGMENT_NODE_IMAGES: {\"<name>\": \"<reference>\"}")?,
        }),
        None => None,
    };
    Ok((model, node))
}

/// The templates `try` scaffolds.
const TRY_TEMPLATES: [&str; 3] = ["todo", "inbox", "notes"];

fn try_template(args: &[String]) -> Result<()> {
    let usage = || format!("usage: cargo xtask try <{}> [name]", TRY_TEMPLATES.join("|"));
    let tpl = args.first().ok_or_else(|| anyhow::anyhow!(usage()))?;
    if !TRY_TEMPLATES.contains(&tpl.as_str()) {
        bail!("{}", usage());
    }
    let port = dev_port()?;
    if TcpStream::connect(("127.0.0.1", port)).is_err() {
        bail!("nothing answers on :{port}: start `cargo xtask dev` in another terminal, wait for `ready`, then try again");
    }
    let root = devstack::repo_root();
    run(Command::new("cargo").args(["build", "--quiet", "--manifest-path"]).arg(root.join("Cargo.toml")).args(["-p", "fragment-cli"]))?;
    let cli = root.join("target/debug/fragment");
    let host = format!("http://127.0.0.1:{port}");
    let name = args.get(1).cloned().unwrap_or_else(|| format!("{tpl}-{}", devstack::random_hex(2)));
    let dir = root.join("target/devstack/try");
    std::fs::create_dir_all(&dir)?;
    // your CLI key must be a person's on the dev fleet (phase 4: people sign in)
    let who = Command::new(&cli).args(["whoami"]).env("FRAGMENT_HOST", &host).output()?;
    if !who.status.success() {
        bail!(
            "your CLI key is no one's on the dev stack yet. Once:\n  FRAGMENT_HOST={host} {} login\n(the dev stack's sign-in fakes take any email)",
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
    // the hosted lane: a branch deployment, read from its config; it needs
    // the CLI and the suite, and no Worker built here (a dry run, the suite alone)
    if args.iter().any(|a| a == "--hosted") {
        let suite_args = deploy::hosted_e2e_args(args)?;
        match suite_args.iter().any(|a| a == "--dry-run") {
            true => build_suite()?,
            false => build_native()?,
        }
        return run(Command::new(devstack::repo_root().join(E2E_BIN)).args(suite_args));
    }
    // CI's split: `--build-only` builds (and the cache is saved after it,
    // so a red run still saves), then `--no-build` runs what it left
    let build_only = args.iter().any(|a| a == "--build-only");
    let no_build = args.iter().any(|a| a == "--no-build");
    if build_only && no_build {
        bail!("--build-only and --no-build are two steps of one run, not one");
    }
    let suite_args: Vec<&String> = args.iter().filter(|a| *a != "--build-only" && *a != "--no-build").collect();
    if build_only && !suite_args.is_empty() {
        bail!("--build-only builds what every run needs, and takes no suite arguments");
    }
    // the pinned Node and node_modules, before the build; the suite finds
    // them in place
    devstack::Tools::locate()?;
    if !no_build {
        build_e2e()?;
    }
    if build_only {
        return Ok(());
    }
    run(Command::new(devstack::repo_root().join(E2E_BIN)).args(suite_args))
}

/// The suite alone, for this machine.
fn build_suite() -> Result<()> {
    let manifest = devstack::repo_root().join("Cargo.toml");
    run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-e2e"]))
}

/// The suite, as `build_e2e` leaves it.
const E2E_BIN: &str = "target/release/fragment-e2e";
/// What the e2e runs: the workers (build.rs: in parallel once worker-build
/// has its tools), the CLI (the e2e drives it too) and the suite beside
/// them, and the computer images the node's boot builds, built ahead
/// beside them all so its build finds every layer cached.
fn build_e2e() -> Result<()> {
    let t0 = std::time::Instant::now();
    let built = std::thread::scope(|s| {
        let images = s.spawn(build::images);
        let native = s.spawn(build_native);
        let workers = build::workers();
        workers.and(native.join().expect("the native build does not panic")).and(images.join().expect("the images' build does not panic"))
    });
    println!("built the workers, the CLI, the suite and the images in {:.1?}", t0.elapsed());
    built
}

/// The CLI and the suite, for this machine.
fn build_native() -> Result<()> {
    let t0 = std::time::Instant::now();
    let manifest = devstack::repo_root().join("Cargo.toml");
    run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-cli"]))?;
    build_suite()?;
    println!("built the CLI and the suite in {:.1?}", t0.elapsed());
    Ok(())
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

/// Every first-party JavaScript file parses, on the pinned Node (fetched
/// into target/tools on first use; no node_modules needed): seconds here,
/// where the e2e would find it most of an hour in (xtask/src/js_syntax.rs).
fn javascript_parses(root: &Path) -> Result<()> {
    let node = devstack::node::locate(&root.join(devstack::TOOLS_DIR))?;
    let started = std::time::Instant::now();
    let files = js_syntax::files(root)?;
    js_syntax::check(&node, &root.join(devstack::CACHE_DIR), root, &files)?;
    println!("JavaScript: {} files parse (node --check on Node {}, {:.1?})", files.len(), node.release, started.elapsed());
    Ok(())
}

fn check() -> Result<()> {
    let root = devstack::repo_root();
    no_conflict_markers(&root)?;
    let read = |path: &str| std::fs::read_to_string(root.join(path)).with_context(|| format!("read {path}"));
    let copies = ["cli/GUIDE.md", "README.md", "cell/shell/shell.js"].map(|path| read(path).map(|text| (path, text)));
    let copies: Vec<(&str, String)> = copies.into_iter().collect::<Result<_>>()?;
    skill_installs_release(&read("cli/SKILL.md")?, &read(".github/workflows/release.yml")?, &copies)?;
    javascript_parses(&root)?;
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
/// which the shell's settings, GUIDE.md, and the README copy, fetches
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
        Some("dev") => dev(&args[1..]),
        Some("try") => try_template(&args[1..]),
        Some("e2e") => e2e(&args[1..]),
        Some("e2e-summary") => summary::e2e_summary(&args[1..]),
        Some("check") => check(),
        Some("deploy") => deploy::deploy(&args[1..]),
        Some("teardown") => deploy::teardown(&args[1..]),
        _ => bail!("usage: cargo xtask build | dev [--clean] | try <template> [name] | e2e [--build-only | --no-build] [--only | --except <section>[,...] | --shard <k>/<n>] [--summary <file>] [--rehearse] | e2e --hosted --config <file> --branch <name> [--dry-run | --sweep] | e2e-summary <dir> | check | deploy --config <file> [--branch <name>] | teardown --config <file> --branch <name>"),
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
        let mut cmd = std::process::Command::new("wrangler");
        cmd.args(["dev", "--port", "8790"]).env("FRAGMENT_KEYS_HOST_SECRET", "not-a-real-secret");
        assert_eq!(super::shown(&cmd), "wrangler dev --port 8790");
    }
}
