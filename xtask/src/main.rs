//! `cargo xtask <command>`: the repo's tooling, in Rust.
//!
//!   build            build cell/ for wasm32 (worker-build 0.8.5)
//!   celld            build the pinned celld fork into target/celld/bin
//!   dev [--clean]    build, then run the stack in the foreground: the cell on
//!                    :8790 (fragments at <name>.fragment.localhost:8790) and
//!                    the code.storage fake on :8792
//!   e2e [args...]    build, then run crates/e2e (args pass through: --only <case>)
//!   check            host tests and clippy, warnings denied

use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;

const WORKER_BUILD_VERSION: &str = "0.8.5";
const DEV_PORT: u16 = 8790;
const DEV_CODESTORAGE_PORT: u16 = 8792;
const DEV_ORG: &str = "fragment-dev";

fn run(cmd: &mut Command) -> Result<()> {
    let shown = format!("{cmd:?}");
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
    run(Command::new("worker-build").arg("--release").current_dir(devstack::cell_dir()))
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
    devstack::Fleet {
        host_secret: devstack::dev_secret("host-secret", || devstack::random_hex(32))?,
        codestorage_org: DEV_ORG.into(),
        codestorage_key_pem: key,
        codestorage_url: fake.url.clone(),
        host_suffix: Some("fragment.localhost".into()),
        // No webhooks reach dev fragments (the CLI's refresh and this poll do).
        poll_interval_s: 10,
    }
    .write_vars()?;
    let opts = devstack::NodeOptions { port: DEV_PORT, clean, watch: true, env: vec![] };
    let (node, took) = devstack::Node::start(&tools, &opts)?;
    println!("fragment dev: {} (ready in {took:.1?}; Ctrl-C stops it)", node.base);
    println!("  fragments:    http://<name>.fragment.localhost:{DEV_PORT}/");
    println!("  code.storage: {} (the fake)", fake.url);
    let status = node.wait()?;
    println!("celld dev exited: {status}");
    Ok(())
}

fn e2e(args: &[String]) -> Result<()> {
    build()?;
    let manifest = devstack::repo_root().join("Cargo.toml");
    // the e2e drives the CLI too
    run(Command::new("cargo").args(["build", "--quiet", "--release", "--manifest-path"]).arg(&manifest).args(["-p", "fragment-cli"]))?;
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
        .current_dir(Path::new(&devstack::cell_dir())))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("build") => build(),
        Some("celld") => celld(),
        Some("dev") => dev(&args[1..]),
        Some("e2e") => e2e(&args[1..]),
        Some("check") => check(),
        _ => bail!("usage: cargo xtask build | celld | dev [--clean] | e2e [--only <case>] | check"),
    }
}
