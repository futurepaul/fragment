//! `cargo xtask <command>`: the repo's tooling, in Rust.
//!
//!   build            build cell/ and agent/ for wasm32 (worker-build 0.8.5)
//!   celld            build the pinned celld fork into target/celld/bin
//!   dev [--clean]    build, then run the stack in the foreground: the cell on
//!                    :8790 (fragments at <name>.fragment.localhost:8790), the
//!                    code.storage fake on :8792, and agents on :8793 (their
//!                    model key from the file OPENROUTER_API_KEY_FILE names)
//!   try <template> [name]
//!                    on the running dev stack: a fragment from a template
//!                    (todo, inbox, notes), scaffolded under target/devstack/try
//!                    so nothing lands in the repo; prints what to open and paste
//!   e2e [args...]    build, then run crates/e2e (args pass through: --only <case>);
//!                    with --fleet <fleet> first, run its hosted sections against
//!                    that fleet instead
//!   check            host tests and clippy, warnings denied
//!   deploy <fleet> [--nodes]
//!                    ship the cell to a hosted fleet (fleets/<fleet>.json), or
//!                    with --nodes roll its Machines to a new node image
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
const DEV_AGENT_PORT: u16 = 8793;
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
    let out = Command::new("worker-build").arg("--version").output().context(
        "worker-build is not installed: cargo install worker-build --version 0.8.5 --locked",
    )?;
    let version = String::from_utf8_lossy(&out.stdout);
    if !version.contains(WORKER_BUILD_VERSION) {
        bail!("worker-build {WORKER_BUILD_VERSION} is required, found {}", version.trim());
    }
    run(Command::new("worker-build").arg("--release").current_dir(devstack::cell_dir()))?;
    run(Command::new("worker-build").arg("--release").current_dir(devstack::agent_dir()))
}

fn celld() -> Result<()> {
    let root = devstack::repo_root().join("target/celld");
    let src = root.join("src");
    let stamp = root.join("bin/REV");
    if std::fs::read_to_string(&stamp).ok().as_deref() == Some(devstack::CELLD_FORK_REV) && devstack::fork_celld_path().is_file() {
        println!("celld fork {} is built", &devstack::CELLD_FORK_REV[..7]);
        return Ok(());
    }
    if !src.join(".git").is_dir() {
        run(Command::new("git").args(["clone", "--quiet", devstack::CELLD_FORK_URL]).arg(&src))?;
    }
    run(Command::new("git").args(["fetch", "--quiet", "origin"]).current_dir(&src))?;
    run(Command::new("git").args(["checkout", "--quiet", "--detach", devstack::CELLD_FORK_REV]).current_dir(&src))?;
    run(Command::new("cargo").args(["build", "--release", "-p", "celld"]).current_dir(&src))?;
    std::fs::create_dir_all(root.join("bin"))?;
    std::fs::copy(src.join("target/release/celld"), devstack::fork_celld_path())?;
    std::fs::write(&stamp, devstack::CELLD_FORK_REV)?;
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
    devstack::Fleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        codestorage_org: DEV_ORG.into(),
        codestorage_key_pem: key,
        codestorage_url: fake.url.clone(),
        host_suffix: Some("fragment.localhost".into()),
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
        // budgets pay for AI only with a management key (real OpenRouter: real money)
        openrouter_management: read("OPENROUTER_MANAGEMENT_KEY_FILE")?,
        budget_usd: None,
        operators: None,
        test_hooks: false,
    }
    .write_vars(&devstack::cell_dir())?;
    let opts = devstack::NodeOptions { project: devstack::cell_dir(), port: DEV_PORT, clean, watch: true, env: vec![] };
    let (node, took) = devstack::Node::start(&tools, &opts)?;
    // agents act on the dev fragments; their model key is a file's (never the repo's)
    let model_key = match std::env::var_os("OPENROUTER_API_KEY_FILE") {
        Some(path) => std::fs::read_to_string(&path).with_context(|| format!("reading {}", Path::new(&path).display()))?.trim().to_string(),
        None => "unset: point OPENROUTER_API_KEY_FILE at a key file".to_string(),
    };
    devstack::AgentFleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        fragment_api: format!("http://127.0.0.1:{DEV_PORT}"),
        agent_url: format!("http://127.0.0.1:{DEV_AGENT_PORT}"),
        openrouter_url: None,
        openrouter_key: model_key,
        test_hooks: false,
        egress_local: true,
    }
    .write_vars(&devstack::agent_dir())?;
    let agent_opts = devstack::NodeOptions { project: devstack::agent_dir(), port: DEV_AGENT_PORT, clean, watch: true, env: vec![] };
    let (agents, _) = devstack::Node::start(&tools, &agent_opts)?;
    println!("fragment dev: {} (ready in {took:.1?}; Ctrl-C stops it)", node.base);
    println!("  fragments:    http://<name>.fragment.localhost:{DEV_PORT}/");
    println!("  agents:       {} (POST /api/agents, signed)", agents.base);
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
    build()?;
    // the e2e drives the CLI too, computers included
    run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-cli", "--features", "computer"]))?;
    run(Command::new("cargo")
        .args(["run", "--quiet", "--release", "--manifest-path"])
        .arg(&manifest)
        .args(["-p", "fragment-e2e", "--"])
        .args(args))
}

fn check() -> Result<()> {
    let root = devstack::repo_root();
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

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => build(),
        Some("celld") => celld(),
        Some("dev") => dev(&args[1..]),
        Some("try") => try_template(&args[1..]),
        Some("e2e") => e2e(&args[1..]),
        Some("check") => check(),
        Some("deploy") => deploy::run(&args[1..]),
        Some("fleet") => deploy::operate(&args[1..]),
        _ => bail!("usage: cargo xtask build | celld | dev [--clean] | try <template> [name] | e2e [--fleet <fleet>] [--only <case>] | check | deploy <fleet> [--nodes] | fleet <fleet> <celld command...>"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_failed_command_never_shows_its_environment() {
        let mut cmd = std::process::Command::new("flyctl");
        cmd.args(["deploy", "--remote-only"]).env("FLY_API_TOKEN", "FlyV1 not-a-real-token");
        assert_eq!(super::shown(&cmd), "flyctl deploy --remote-only");
    }
}
