//! The builds `build`, `dev`, `deploy` and `e2e` share: the cell for
//! wasm32 (worker-build), and the computer images `wrangler dev` builds at
//! boot, built ahead beside the Rust so its build finds every layer cached.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;

use crate::{run, shown};

pub const WORKER_BUILD_VERSION: &str = "0.8.5";

/// cell/ for wasm32, into its `build/`.
pub fn cell() -> Result<()> {
    worker_build_installed()?;
    goose()?;
    let t0 = Instant::now();
    run(Command::new("worker-build").arg("--release").current_dir(devstack::cell_dir()))?;
    println!("built the cell for wasm32 in {:.1?}", t0.elapsed());
    Ok(())
}

/// The generic Goose GDK module that the platform loads into app facets.
/// worker-build owns wasm-bindgen and esbuild, as it does for the cell.
pub fn goose() -> Result<()> {
    let root = devstack::repo_root();
    run(Command::new("worker-build")
        .args(["--release", "--locked"])
        .env("CUSTOM_SHIM", root.join("goose/shim.js"))
        .current_dir(root.join("goose")))
}

fn worker_build_installed() -> Result<()> {
    let out = Command::new("worker-build")
        .arg("--version")
        .output()
        .context("worker-build is not installed: cargo install worker-build --version 0.8.5 --locked")?;
    let version = String::from_utf8_lossy(&out.stdout);
    if !version.contains(WORKER_BUILD_VERSION) {
        bail!("worker-build {WORKER_BUILD_VERSION} is required, found {}", version.trim());
    }
    Ok(())
}

/// The computer images the cell's config names (`containers[].images`),
/// each built as `wrangler dev` builds it at boot (wrangler's
/// `constructBuildCommand`: `docker build --load --platform linux/amd64
/// --provenance=false [--build-arg …] -f - <context>`, the Dockerfile on
/// stdin), so the boot's build finds every layer cached. A cold build of
/// the stub compiles its bridge in Docker: on a CI runner, minutes that now
/// pass beside the Rust and Wasm builds instead of after them.
pub fn images() -> Result<()> {
    let cell = devstack::cell_dir();
    let mut config = devstack::read_config(&cell)?;
    devstack::absolute_images(&mut config, &cell)?;
    let containers = config["containers"].as_array().cloned().unwrap_or_default();
    // bounded: the images one config names
    for container in &containers {
        for (name, image) in container["images"].as_object().into_iter().flatten() {
            let t0 = Instant::now();
            image_build(image).with_context(|| format!("building the {name} image ahead of the node"))?;
            println!("image {name} built ahead of the node in {:.1?}", t0.elapsed());
        }
    }
    Ok(())
}

/// One image, as wrangler builds it; its output shown only when it fails.
fn image_build(image: &serde_json::Value) -> Result<()> {
    let dockerfile = image["dockerfile"].as_str().context("an image names its dockerfile")?;
    let context = image["build_context"].as_str().context("an image names its build context")?;
    let text = std::fs::read(dockerfile).with_context(|| format!("read {dockerfile}"))?;
    let mut cmd = Command::new("docker");
    cmd.args(["build", "--load", "--platform", "linux/amd64", "--provenance=false"]);
    for (k, v) in image["build_vars"].as_object().into_iter().flatten() {
        let v = v.as_str().with_context(|| format!("build var {k} is a string"))?;
        cmd.arg("--build-arg").arg(format!("{k}={v}"));
    }
    cmd.args(["-f", "-", context]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let shown = shown(&cmd);
    let mut child = cmd.spawn().with_context(|| format!("could not start {shown} (is Docker running?)"))?;
    child.stdin.take().context("docker's stdin")?.write_all(&text)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("{shown} failed: {}\n{}{}", out.status, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}
