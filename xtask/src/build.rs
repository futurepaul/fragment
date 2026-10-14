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

/// The most the cell's Wasm may be, raw: every isolate copies its data
/// into its memory, and every deploy uploads it. The files it serves are
/// the release's Static Assets, not its data (cell/src/assets.rs); what
/// grows it past this is a choice, made by moving this number: 6.5 MB
/// since launch week (2026-10-14, the debt ledger's "The cell's Wasm
/// budget was raised for launch").
pub const CELL_WASM_MAX_BYTES: u64 = 6_500_000;

/// cell/ for wasm32, into its `build/`, and the release's files beside it
/// in `build/assets`, the Worker's Static Assets (cell/wrangler.jsonc): what
/// a deploy uploads, `wrangler dev` serves and the e2e's staged copy holds.
pub fn cell() -> Result<()> {
    worker_build_installed()?;
    let t0 = Instant::now();
    let dir = devstack::cell_dir();
    run(Command::new("worker-build").arg("--release").current_dir(&dir))?;
    let wasm = dir.join("build/index_bg.wasm");
    let size = std::fs::metadata(&wasm).with_context(|| format!("{} after worker-build", wasm.display()))?.len();
    within_budget(size)?;
    let (files, bytes) = fragment_templates::write_assets(&dir.join("build/assets")).context("writing the release's files into cell/build/assets")?;
    println!(
        "built the cell for wasm32 in {:.1?}: {:.2} MB of Wasm (at most {:.2} MB), and {files} files of the release, {:.2} MB, in its Static Assets",
        t0.elapsed(),
        size as f64 / 1e6,
        CELL_WASM_MAX_BYTES as f64 / 1e6,
        bytes as f64 / 1e6
    );
    Ok(())
}

/// The cell's Wasm within `CELL_WASM_MAX_BYTES`, or why not.
fn within_budget(size: u64) -> Result<()> {
    if size > CELL_WASM_MAX_BYTES {
        bail!(
            "cell/build/index_bg.wasm is {size} bytes, over its budget of {CELL_WASM_MAX_BYTES} (xtask/src/build.rs, CELL_WASM_MAX_BYTES): every isolate copies the Wasm's data into its memory. A file the cell serves belongs in its Static Assets (crates/templates, cell/src/assets.rs), not in an include_bytes!; anything else that grows it this far is a choice to make, and this number with it"
        );
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    /// The budget holds the cell to its number, and its refusal says what
    /// to do and where the number is.
    #[test]
    fn the_cell_is_held_to_its_budget() {
        assert!(super::within_budget(super::CELL_WASM_MAX_BYTES).is_ok());
        let refused = super::within_budget(super::CELL_WASM_MAX_BYTES + 1).unwrap_err().to_string();
        assert!(refused.contains("CELL_WASM_MAX_BYTES") && refused.contains("Static Assets"), "{refused}");
    }

    /// Goal: the Hermes image's inputs are what it runs (docs/computers.md,
    /// "Stable and canary": what changes a version), so a change to the
    /// cell, the shell, a blessed template, the e2e, xtask or the docs
    /// builds the same image. Method: its CLI stage
    /// (images/hermes/Dockerfile) against cargo's own account of the
    /// workspace. It copies, whole, the CLI and each workspace crate it
    /// builds from (its normal and build dependencies by path, and theirs);
    /// each other member's manifest alone, and stubs exactly those; the
    /// root manifest and lock; and each template but the platform's own,
    /// which the CLI builds without. Invalid: a new dependency of the CLI's,
    /// a new member or a new template fails here, naming what the stage
    /// lacks, before it fails the image's build or silently leaves it out.
    #[test]
    fn the_cli_stage_copies_what_the_cli_builds_from() {
        use std::collections::{BTreeMap, BTreeSet};
        use std::path::{Path, PathBuf};

        let root = super::devstack::repo_root().canonicalize().expect("the repo");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let out = super::Command::new(cargo).args(["metadata", "--format-version", "1", "--no-deps", "--offline"]).current_dir(&root).output().expect("cargo metadata");
        assert!(out.status.success(), "cargo metadata: {}", String::from_utf8_lossy(&out.stderr));
        let meta: serde_json::Value = serde_json::from_slice(&out.stdout).expect("its JSON");
        let rel = |p: &str| -> String { Path::new(p).strip_prefix(&root).unwrap_or_else(|_| panic!("{p} is in the repo")).to_string_lossy().into_owned() };
        // each member's directory, by name, and the members it builds from
        let mut dirs = BTreeMap::new();
        let mut needs: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in meta["packages"].as_array().expect("its packages") {
            let name = p["name"].as_str().expect("a name").to_string();
            let manifest = PathBuf::from(p["manifest_path"].as_str().expect("its manifest"));
            dirs.insert(name.clone(), rel(manifest.parent().expect("its directory").to_str().expect("a path")));
            let deps = p["dependencies"].as_array().into_iter().flatten();
            let built = deps.filter(|d| d["kind"].is_null() || d["kind"] == "build").filter(|d| d["path"].is_string());
            needs.insert(name, built.map(|d| d["name"].as_str().expect("a name").to_string()).collect());
        }
        let mut closure = BTreeSet::from(["fragment-cli".to_string()]);
        let mut next = vec!["fragment-cli".to_string()];
        // bounded: each member enters once
        while let Some(n) = next.pop() {
            for d in needs.get(&n).into_iter().flatten() {
                if closure.insert(d.clone()) {
                    next.push(d.clone());
                }
            }
        }
        let whole: BTreeSet<String> = closure.iter().map(|n| dirs[n].clone()).collect();
        let stubbed: BTreeSet<String> = dirs.iter().filter(|(n, _)| !closure.contains(*n)).map(|(_, d)| d.clone()).collect();
        assert!(whole.contains("cli") && whole.contains("crates/templates") && stubbed.contains("crates/e2e"), "{whole:?} {stubbed:?}");

        let dockerfile = std::fs::read_to_string(root.join("images/hermes/Dockerfile")).expect("the Dockerfile");
        let stage: Vec<&str> = dockerfile.lines().skip_while(|l| !l.starts_with("FROM rust:") || !l.ends_with(" AS cli")).skip(1).take_while(|l| !l.starts_with("FROM ")).collect();
        assert!(!stage.is_empty(), "the Dockerfile has its CLI stage (`FROM rust:… AS cli`)");
        let copies: Vec<Vec<&str>> = stage.iter().filter_map(|l| l.strip_prefix("COPY ")).map(|l| l.split_whitespace().collect()).collect();
        let lock = ["Cargo.toml", "Cargo.lock", "./"];
        assert!(copies.iter().any(|c| c.as_slice() == lock), "the root manifest and lock: {copies:?}");
        let paths: Vec<&str> = copies.iter().filter(|c| c.as_slice() != lock).map(|c| {
            assert!(c.len() == 2 && c[0] == c[1] && !c[0].starts_with("--"), "each copy keeps its path: {c:?}");
            c[0]
        }).collect();
        let copied_whole: BTreeSet<String> = paths.iter().filter(|p| !p.ends_with("/Cargo.toml") && !p.starts_with("templates/")).map(|p| p.to_string()).collect();
        let copied_manifests: BTreeSet<String> = paths.iter().filter_map(|p| p.strip_suffix("/Cargo.toml")).map(str::to_string).collect();
        assert_eq!(copied_whole, whole, "the stage copies the CLI and each crate it builds from, whole");
        assert_eq!(copied_manifests, stubbed, "and each other member's manifest alone");
        let stubs = stage.iter().find_map(|l| l.trim().strip_prefix("for m in ").and_then(|l| l.split_once(';'))).map(|(list, _)| list.split_whitespace().map(str::to_string).collect::<BTreeSet<_>>());
        assert_eq!(stubs, Some(stubbed), "and stubs exactly those members' sources (`for m in …;`)");

        let mut templates: BTreeSet<String> = BTreeSet::new();
        for entry in std::fs::read_dir(root.join("templates")).expect("templates/").flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.path().is_dir() && !fragment_templates::blessed::BLESSED.contains(&name.as_str()) {
                templates.insert(format!("templates/{name}"));
            }
        }
        let copied_templates: BTreeSet<String> = paths.iter().filter(|p| p.starts_with("templates/")).map(|p| p.to_string()).collect();
        assert_eq!(copied_templates, templates, "and each template the CLI scaffolds: every one but the platform's own");
    }
}
