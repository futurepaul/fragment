//! `cargo xtask <command>`: the repo's tooling, in Rust.
//!
//!   build            build cell/ for wasm32 (worker-build 0.8.5: build.rs)
//!   dev [--clean]    build, then run the stack in the foreground under
//!                    `wrangler dev`: the cell on :8790 (fragments at
//!                    <name>.fragment.localhost:8790), the
//!                    code.storage fake on :8792, and the Workers AI fake on :8796
//!                    behind the model route (its calls spend their payer's
//!                    ledger; new people are seats with the month's included
//!                    credit)
//!   try <template> [name]
//!                    on the running dev stack: a fragment from a template
//!                    (todo, inbox, notes), scaffolded under target/devstack/try
//!                    so nothing lands in the repo; prints what to open and paste
//!   e2e [args...]    build, then run crates/e2e (args pass through: --only <section>[,...],
//!                    --except <section>[,...], or --shard <k>/<n> (the table's, as CI
//!                    splits it); --rehearse keeps the hosted lane's rules on the
//!                    local node). The build builds the computer images ahead of
//!                    the node beside the Rust. CI splits it in two steps, so the
//!                    cache saves between them: --build-only (builds, runs
//!                    nothing), then --no-build (runs what the build left)
//!   e2e --hosted --config <file> --branch <name> [--only … | --except …]
//!       [--dry-run | --sweep] [--max-paid-calls <n>] [--operator-key-file <file>]
//!       [--candidate <image>]
//!                    the suite against that branch deployment on its real vendors
//!                    (crates/e2e/src/hosted.rs); --dry-run prints its plan;
//!                    --operator-key-file names an operator key the config's
//!                    operators list (its `wipe` section's); --candidate names
//!                    the config's canary, which the real-agent sections run
//!                    and agent-restart upgrades into and back
//!   check            every first-party JavaScript file parses (node --check;
//!                    xtask/src/js_syntax.rs), then host tests and clippy,
//!                    warnings denied
//!   secret set <name> --config <file> [--from-file <path>]
//!   secret gen <name> --config <file>
//!   secret list --config <file>
//!                    the deployment's secrets in its account's Cloudflare
//!                    Secrets Store: set one (wrangler's hidden prompt, standard
//!                    input, or a file once), make a host secret, or list names
//!                    and times; --local <state dir> for wrangler's local store
//!                    instead (xtask/src/secret.rs)
//!   deploy --config <file> [--branch <name>]
//!                    build and deploy to Cloudflare from a deployment's config
//!                    (deploy/example.jsonc), once its secrets are all in the
//!                    store: a branch gets a complete copy of its own at
//!                    <branch>.<zone> (xtask/src/deploy.rs)
//!   teardown --config <file> --branch <name>
//!                    remove a branch deployment (irreversible)
//!   stripe check|setup --config <file> [--branch <name>] [--webhook-secret-file <path>]
//!                    the deployment's Stripe as docs/billing.md wants it, on the
//!                    account its stripe.key_file names: check reads and says what
//!                    is wrong; setup makes what is missing (xtask/src/stripe.rs)
//!
//! dev, e2e, secret, deploy and teardown run wrangler and npm on the pinned Node,
//! and check runs `node --check` on it, fetched into target/tools on first
//! use, never a `node` from PATH (crates/devstack/src/node.rs).
//!
//! fragment.club runs on celld from the `celld` branch (the tag celld-final)
//! until the cutover (docs/cloudflare-v1.md, decision 35): `deploy` never
//! reaches it.

use std::net::TcpStream;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;

mod build;
mod deploy;
mod dns;
mod js_syntax;
mod secret;
mod stripe;

const DEV_PORT: u16 = 8790;
const DEV_CODESTORAGE_PORT: u16 = 8792;
const DEV_WORKOS_PORT: u16 = 8794;
/// The Workers AI fake behind the model route (`FRAGMENT_AI_URL`): dev
/// never calls real models.
const DEV_AI_PORT: u16 = 8796;
/// The mail fake (`FRAGMENT_MAIL_URL`): dev never sends real mail, and
/// prints each message instead.
const DEV_MAIL_PORT: u16 = 8798;
/// The Stripe fake (docs/billing.md): dev never charges a card; its
/// Checkout is a page with a Pay button.
const DEV_STRIPE_PORT: u16 = 8800;
const DEV_STRIPE_KEY: &str = "sk_test_fragment_dev";
const DEV_STRIPE_WEBHOOK: &str = "whsec_fragment_dev";
/// Where dev's mail says it comes from.
const DEV_MAIL_FROM: &str = "fragment <mail@fragment.localhost>";
/// The WorkOS fake's environment in dev.
const DEV_WORKOS_CLIENT: &str = "client_fragment_dev";
const DEV_WORKOS_KEY: &str = "sk_test_fragment_dev";
const DEV_ORG: &str = "fragment-dev";

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

/// Real models in dev, by choice (`FRAGMENT_DEV_MODELS_FILE`): the
/// cell's `models` (`FRAGMENT_MODELS`), the slots on an OpenAI-compatible
/// API, and the key they call with (`FRAGMENT_DEV_MODEL_KEY_FILE`).
struct RealModels {
    cell: String,
    slots: Vec<String>,
    key: String,
}

/// `FRAGMENT_DEV_MODELS_FILE`'s models block, as a deployment's config
/// names it (deploy/example.jsonc's: Finite Private's, or OpenRouter's),
/// checked as a deploy checks one, and `FRAGMENT_DEV_MODEL_KEY_FILE`'s key
/// for its slots on an OpenAI-compatible API: dev binds one model key, so
/// they name one. Its Workers AI slots stay the fake. `read` reads the
/// file a variable names.
fn real_models(read: impl Fn(&str) -> Result<Option<String>>) -> Result<Option<RealModels>> {
    let Some(text) = read("FRAGMENT_DEV_MODELS_FILE")? else { return Ok(None) };
    let models: serde_json::Value = serde_json::from_str(&devstack::strip_comments(&text)).context("FRAGMENT_DEV_MODELS_FILE holds a models block, as a deployment's config names it")?;
    let (cell, keys) = deploy::models_cell(&models)?;
    let mut names: Vec<&str> = keys.iter().map(|(_, k)| k.as_str()).collect();
    names.dedup();
    if names.len() > 1 {
        bail!("dev binds one model key: FRAGMENT_DEV_MODELS_FILE's slots on OpenAI-compatible APIs name one key, not {names:?}");
    }
    let key = match keys.is_empty() {
        true => String::new(),
        false => read("FRAGMENT_DEV_MODEL_KEY_FILE")?.context("FRAGMENT_DEV_MODEL_KEY_FILE names the file holding the key FRAGMENT_DEV_MODELS_FILE's slots call with")?,
    };
    Ok(Some(RealModels { cell: cell.to_string(), slots: keys.into_iter().map(|(slot, _)| slot).collect(), key }))
}

/// The people's own providers dev offers: deploy/example.jsonc's (`kind`
/// `own`: OpenRouter's), each person's key their own to set.
fn own_providers() -> Result<String> {
    let text = std::fs::read_to_string(devstack::repo_root().join("deploy/example.jsonc")).context("reading deploy/example.jsonc")?;
    let config: serde_json::Value = serde_json::from_str(&devstack::strip_comments(&text)).context("deploy/example.jsonc parses")?;
    let own: Vec<serde_json::Value> = config["providers"].as_array().into_iter().flatten().filter(|r| r["kind"] == "own").cloned().collect();
    let rows = serde_json::Value::Array(own).to_string();
    fragment_core::catalog::Catalog::parse(&rows).map_err(|e| anyhow::anyhow!("deploy/example.jsonc's own providers: {e}"))?;
    Ok(rows)
}

/// cell/ for wasm32 (build.rs).
fn build() -> Result<()> {
    build::cell()
}

fn dev(args: &[String]) -> Result<()> {
    // the pinned Node and node_modules first: one that cannot be had stops
    // the run before a build
    let tools = devstack::Tools::locate()?;
    build()?;
    let clean = args.iter().any(|a| a == "--clean");
    let state = devstack::repo_root().join("target/devstack/codestorage.json");
    if clean {
        let _ = std::fs::remove_file(&state);
        // the node's state too, before the fleet seeds its local store again
        devstack::clear_state(&devstack::cell_dir())?;
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
    // real models and our Hermes image, by choice: dev's tiers are the
    // Workers AI fake, and its computers the stub, otherwise
    let real = real_models(read)?;
    let hermes = args.iter().any(|a| a == "--hermes");
    let ai = fragment_fakes::workers_ai::WorkersAi::start(DEV_AI_PORT)?;
    let mailer = fragment_fakes::mail::Mailer::start(DEV_MAIL_PORT, true)?;
    let stripe = fragment_fakes::stripe::Stripe::start(DEV_STRIPE_PORT, DEV_STRIPE_KEY)?;
    stripe.set_endpoint(&format!("http://127.0.0.1:{DEV_PORT}/api/stripe/webhook"), DEV_STRIPE_WEBHOOK);
    let workos_label = match &workos.api_url {
        Some(u) => format!("{u} (the fake)"),
        None => format!("WorkOS {}", workos.client_id),
    };
    let fleet = devstack::Fleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        codestorage_org: DEV_ORG.into(),
        codestorage_key_pem: key,
        codestorage_url: fake.url.clone(),
        host_suffix: "fragment.localhost".into(),
        host_label_suffix: None,
        // A dev fragment's pins move by its own moves, the CLI's refresh, and this poll.
        poll_interval_s: 10,
        egress_local: true,
        job_retry_delay_s: 2,
        blob_grace_s: None,
        // text and images: the Workers AI fake, every slot on it, unless
        // FRAGMENT_DEV_MODELS_FILE puts some on a real model's API
        ai_url: Some(ai.url.clone()),
        ai_gateway: None,
        models: real.as_ref().map(|r| r.cell.clone()),
        model_key_slots: real.as_ref().map(|r| r.slots.clone()).unwrap_or_default(),
        model_key_value: real.as_ref().filter(|r| !r.slots.is_empty()).map(|r| r.key.clone()),
        // the mail fake, which prints each message (dev never sends real mail)
        mail_url: Some(mailer.url.clone()),
        mail_from: Some(DEV_MAIL_FROM.into()),
        // a dev person is a seat, with the month's included credit
        default_plan: Some("seat".into()),
        delivery_retry_s: None,
        workos: Some(workos),
        stripe: Some(devstack::StripeVars {
            key: DEV_STRIPE_KEY.into(),
            webhook_secret: DEV_STRIPE_WEBHOOK.into(),
            api_url: Some(stripe.url.clone()),
            portal: None,
            tax: false,
        }),
        // the CLI's host: sign-in and approvals happen where it points
        platform_url: format!("http://127.0.0.1:{DEV_PORT}"),
        operators: None,
        signins_pending_max: None,
        test_secret: None,
        computer_image: Some(if hermes { "hermes" } else { "stub" }.into()),
        computer_snapshots: false,
        computer_unsaved_max_ms: None,
        // people's own providers (OpenRouter's): nothing is called until a
        // person sets their key
        providers: Some(own_providers()?),
        operator_key_values: vec![],
        swap_upstream: None,
        images: match hermes {
            // built from the repo's root: it carries the fragment CLI
            true => vec![("hermes".into(), serde_json::json!({ "dockerfile": devstack::repo_root().join("images/hermes/Dockerfile"), "build_context": devstack::repo_root() }))],
            false => vec![],
        },
    };
    // its secrets go to wrangler's local store under cell/.wrangler/state
    fleet.configure(&tools, &devstack::cell_dir())?;
    let opts = devstack::NodeOptions {
        project: devstack::cell_dir(),
        port: DEV_PORT,
        log_dir: devstack::repo_root().join("target/devstack"),
        // wrangler's own debug logs, when asked for
        node_logs: std::env::var_os("FRAGMENT_NODE_LOGS").is_some(),
        // in the terminal's group: Ctrl-C stops it with xtask
        own_group: false,
    };
    let (node, took) = devstack::Node::start(&tools, &opts)?;
    println!("fragment dev: {} (ready in {took:.1?}; Ctrl-C stops it)", node.base);
    println!("  node log:     {}", node.log.display());
    println!("  fragments:    http://<name>.fragment.localhost:{DEV_PORT}/");
    println!("  code.storage: {} (the fake)", fake.url);
    match &real {
        Some(r) => println!("  models:       {} (FRAGMENT_DEV_MODELS_FILE: real models, paid by its key), the rest on the Workers AI fake at {}", r.slots.join(", "), ai.url),
        None => println!("  models:       {} (the Workers AI fake: echoes, never a real model)", ai.url),
    }
    println!("  computers:    {}", if hermes { "our Hermes image (--hermes; its first build takes a while)" } else { "the stub image (--hermes runs ours)" });
    println!("  seats:        {} (the Stripe fake: its Checkout is a Pay button)", stripe.url);

    println!("  sign-in:      http://127.0.0.1:{DEV_PORT}/ via {workos_label}");
    println!("  try one:      cargo xtask try todo | inbox   (in another terminal)");
    // Ctrl-C stops the node, and xtask outlives it to remove the containers
    // it left (wrangler's teardown removes its computers, not their sidecars)
    devstack::signals::outlive_interrupt()?;
    let status = node.wait()?;
    println!("wrangler dev exited: {status}");
    let removed = devstack::containers::remove(&devstack::cell_dir())?;
    println!("removed {} containers the dev node left", removed.containers);
    Ok(())
}

/// The templates `try` scaffolds.
const TRY_TEMPLATES: [&str; 4] = ["todo", "inbox", "notes", "doc"];

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
    let query = if tpl == "doc" { "notes" } else { "list" };
    println!("               fragment runs {name} | fragment channel {name} | fragment call {name} {query}");
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
/// What the e2e runs: the cell, the CLI (the e2e drives it too) and the
/// suite beside it, and the computer images the node's boot builds, built
/// ahead beside them all so its build finds every layer cached.
fn build_e2e() -> Result<()> {
    let t0 = std::time::Instant::now();
    let built = std::thread::scope(|s| {
        let images = s.spawn(build::images);
        let native = s.spawn(build_native);
        let cell = build::cell();
        cell.and(native.join().expect("the native build does not panic")).and(images.join().expect("the images' build does not panic"))
    });
    println!("built the cell, the CLI, the suite and the images in {:.1?}", t0.elapsed());
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
    println!("JavaScript: {} files parse (node --check on Node v{}, {:.1?})", files.len(), devstack::node::NODE_VERSION, started.elapsed());
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
        .current_dir(Path::new(&devstack::cell_dir())))
}

/// Where the one-line install fetches the CLI from.
const RELEASES: &str = "https://github.com/finitecomputer/fragment/releases/latest/download/";
/// Each target the release builds, and its asset's name: the `uname -s` and
/// `uname -m` of the machines it runs on, which the install command names.
const RELEASE_ASSETS: [(&str, &str); 4] = [
    ("aarch64-apple-darwin", "Darwin-arm64"),
    ("x86_64-apple-darwin", "Darwin-x86_64"),
    ("x86_64-unknown-linux-musl", "Linux-x86_64"),
    ("aarch64-unknown-linux-musl", "Linux-aarch64"),
];

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
        Some("check") => check(),
        Some("secret") => secret::secret(&args[1..]),
        Some("deploy") => deploy::deploy(&args[1..]),
        Some("teardown") => deploy::teardown(&args[1..]),
        Some("stripe") => deploy::stripe(&args[1..]),
        _ => bail!("usage: cargo xtask build | dev [--clean] | try <template> [name] | e2e [--build-only | --no-build] [--only | --except <section>[,...] | --shard <k>/<n>] [--rehearse] | e2e --hosted --config <file> --branch <name> [--dry-run | --sweep] | check | secret set <name> | gen <name> | list --config <file> | deploy --config <file> [--branch <name>] | teardown --config <file> --branch <name>"),
    }
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    /// The files dev reads by the variables naming them, for `real_models`.
    fn files(models: Option<&str>, key: Option<&str>) -> impl Fn(&str) -> Result<Option<String>> {
        let (models, key) = (models.map(str::to_string), key.map(str::to_string));
        move |var: &str| {
            Ok(match var {
                "FRAGMENT_DEV_MODELS_FILE" => models.clone(),
                "FRAGMENT_DEV_MODEL_KEY_FILE" => key.clone(),
                other => panic!("dev reads no {other} for its models"),
            })
        }
    }

    /// Goal: dev calls a real model only by choice, and only as a deploy
    /// would. Method: no file, a deployment's two shapes (OpenRouter's,
    /// Finite Private's, comments and all), and what a deploy refuses.
    #[test]
    fn dev_runs_real_models_by_choice_and_as_a_deploy_would() {
        assert!(super::real_models(files(None, None)).unwrap().is_none(), "no file: the fake");
        let routed = r#"{ // OpenRouter's, as example.jsonc has it
            "cheap": { "provider": "openai", "base_url": "https://openrouter.ai/api/v1", "key": "fragment-openrouter-key", "model": "z-ai/glm-5.3-flash" },
            "medium": { "provider": "openai", "base_url": "https://openrouter.ai/api/v1", "key": "fragment-openrouter-key", "model": "z-ai/glm-5.3-flash" } }"#;
        let r = super::real_models(files(Some(routed), Some("sk-or-test"))).unwrap().expect("its models");
        assert_eq!((r.slots, r.key.as_str()), (vec!["cheap".to_string(), "medium".into()], "sk-or-test"));
        assert!(!r.cell.contains("fragment-openrouter-key"), "the cell's models name no key: {}", r.cell);
        let private = r#"{ "cheap": { "provider": "openai", "base_url": "https://finite-private.finite.containers.tinfoil.dev/v1", "key": "fragment-finite-private-key", "model": "glm-5-3-flash" } }"#;
        let r = super::real_models(files(Some(private), Some("fpk_live_test"))).unwrap().expect("its models");
        assert_eq!(r.slots, ["cheap"]);
        // Workers AI slots alone name no key, and dev reads none
        let flash = r#"{ "cheap": { "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3-flash" } }"#;
        assert!(super::real_models(files(Some(flash), None)).unwrap().is_some_and(|r| r.slots.is_empty() && r.key.is_empty()));

        // invalid: two keys (dev binds one), a key not given, a deploy's refusals, no JSON
        let two = r#"{ "cheap": { "provider": "openai", "base_url": "https://a.example/v1", "key": "k1", "model": "glm-5-3-flash" },
                       "medium": { "provider": "openai", "base_url": "https://openrouter.ai/api/v1", "key": "k2", "model": "z-ai/glm-5.3-flash" } }"#;
        assert!(super::real_models(files(Some(two), Some("x"))).err().is_some_and(|e| e.to_string().contains("one model key")));
        assert!(super::real_models(files(Some(private), None)).err().is_some_and(|e| e.to_string().contains("FRAGMENT_DEV_MODEL_KEY_FILE")));
        let high = r#"{ "high": { "provider": "workers_ai", "model": "@cf/zai-org/glm-5.3" } }"#;
        let unpriced = r#"{ "cheap": { "provider": "openai", "base_url": "https://a.example/v1", "key": "k", "model": "no-such-model" } }"#;
        for bad in [high, unpriced, "{ not json", "[]"] {
            assert!(super::real_models(files(Some(bad), Some("k"))).is_err(), "{bad}");
        }
    }

    /// Dev offers people's own providers, example.jsonc's: OpenRouter's,
    /// with its sign-in and models, and no operator key (dev holds none).
    #[test]
    fn dev_offers_peoples_own_providers() {
        let rows: serde_json::Value = serde_json::from_str(&super::own_providers().unwrap()).unwrap();
        let rows = rows.as_array().expect("a catalog");
        assert!(rows.iter().all(|r| r["kind"] == "own"), "{rows:?}");
        assert!(rows.iter().any(|r| r["name"] == "openrouter" && r["models"]["base_url"] == "https://openrouter.ai/api/v1"), "{rows:?}");
    }

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
