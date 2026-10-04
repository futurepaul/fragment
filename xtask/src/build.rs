//! The builds `build`, `dev`, `deploy` and `e2e` share: the two Workers
//! for wasm32 (worker-build), in parallel once worker-build has its
//! tools, and the computer images `wrangler dev` builds at boot, built
//! ahead beside the Rust so its build finds every layer cached.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;

use crate::{run, shown};

pub const WORKER_BUILD_VERSION: &str = "0.8.5";
/// The tools worker-build 0.8.5 fetches into its cache beside wasm-bindgen
/// (whose version is the lockfiles'), each as `<tool>-<target>-<version>`
/// (its src/versions.rs). A new worker-build may fetch others: then they
/// look absent here, and the Workers build one at a time, as is safe.
const WORKER_BUILD_TOOLS: [(&str, &str); 2] = [("wasm-opt", "130"), ("esbuild", "0.28.1")];

/// cell/ and agent/ for wasm32, each into its `build/`. Two worker-builds
/// race only on a first fetch of their tools into the one cache they share
/// (each removes a tool's other versions, then downloads it), so the two
/// build at once when every tool is there, and one at a time otherwise.
pub fn workers() -> Result<()> {
    worker_build_installed()?;
    let (cell, agent) = (devstack::cell_dir(), devstack::agent_dir());
    if !tools_present() {
        println!("worker-build fetches its tools first: the cell and the agent build one at a time");
        worker(&cell)?;
        return worker(&agent);
    }
    std::thread::scope(|s| {
        let agent = s.spawn(|| worker(&agent));
        let cell = worker(&cell);
        cell.and(agent.join().expect("the agent's build does not panic"))
    })
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

/// One Worker project (`cell/` or `agent/`) for wasm32, into its `build/`.
fn worker(dir: &Path) -> Result<()> {
    let t0 = Instant::now();
    run(Command::new("worker-build").arg("--release").current_dir(dir))?;
    println!("built {} for wasm32 in {:.1?}", dir.file_name().unwrap_or_default().to_string_lossy(), t0.elapsed());
    Ok(())
}

/// Where worker-build keeps its tools: `dirs_next::cache_dir()`'s
/// `worker-build`, which is `~/Library/Caches` on macOS, and
/// `$XDG_CACHE_HOME` (absolute) or `~/.cache` elsewhere.
fn tools_cache() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let base = match (cfg!(target_os = "macos"), std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from)) {
        (true, _) => home.join("Library/Caches"),
        (false, Some(xdg)) if xdg.is_absolute() => xdg,
        (false, _) => home.join(".cache"),
    };
    Some(base.join("worker-build"))
}

/// The version of `crate` a Cargo.lock names (its first entry).
fn locked(lockfile: &str, krate: &str) -> Option<String> {
    let entry = format!("[[package]]\nname = \"{krate}\"\nversion = \"");
    let at = lockfile.find(&entry)? + entry.len();
    lockfile[at..].split('"').next().map(str::to_string)
}

/// Whether worker-build has every tool it fetches for both Workers: the
/// wasm-bindgen both lockfiles name (one version, or two builds would
/// remove each other's), and `WORKER_BUILD_TOOLS`, each with its binary.
fn tools_present() -> bool {
    let read = |dir: PathBuf| std::fs::read_to_string(dir.join("Cargo.lock")).ok();
    let bindgen = match (read(devstack::cell_dir()), read(devstack::agent_dir())) {
        (Some(cell), Some(agent)) => match (locked(&cell, "wasm-bindgen"), locked(&agent, "wasm-bindgen")) {
            (Some(c), Some(a)) if c == a => c,
            _ => return false,
        },
        _ => return false,
    };
    let Some(cache) = tools_cache() else { return false };
    let Ok(entries) = std::fs::read_dir(&cache) else { return false };
    let dirs: Vec<(String, PathBuf)> = entries.flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), e.path())).collect();
    let fetched = |tool: &str, version: &str| {
        let (prefix, suffix) = (format!("{tool}-"), format!("-{version}"));
        // a directory made for a download that did not finish has no binary
        dirs.iter().any(|(name, path)| name.starts_with(&prefix) && name.ends_with(&suffix) && (path.join(tool).is_file() || path.join("bin").join(tool).is_file()))
    };
    fetched("wasm-bindgen", &bindgen) && WORKER_BUILD_TOOLS.iter().all(|(tool, version)| fetched(tool, version))
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
    use super::*;

    #[test]
    fn a_lockfile_names_a_crates_version() {
        let lock = "[[package]]\nname = \"wasm-bindgen-shared\"\nversion = \"0.2.1\"\n\n[[package]]\nname = \"wasm-bindgen\"\nversion = \"0.2.128\"\nsource = \"registry\"\n";
        assert_eq!(locked(lock, "wasm-bindgen").as_deref(), Some("0.2.128"));
        assert_eq!(locked(lock, "wasm-bindgen-shared").as_deref(), Some("0.2.1"));
        assert_eq!(locked(lock, "worker"), None);
        // the repo's own: the cell and the agent lock one wasm-bindgen
        let read = |dir: PathBuf| std::fs::read_to_string(dir.join("Cargo.lock")).unwrap();
        let (cell, agent) = (read(devstack::cell_dir()), read(devstack::agent_dir()));
        assert!(locked(&cell, "wasm-bindgen").is_some());
        assert_eq!(locked(&cell, "wasm-bindgen"), locked(&agent, "wasm-bindgen"), "two wasm-bindgens would make worker-build remove each other's");
    }
}
