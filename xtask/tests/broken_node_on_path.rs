//! The repo's JavaScript never runs on a `node` from PATH. With the first
//! `node` on PATH one that exits 1 (as a machine's broken one would), the
//! e2e's deploy, site and shell sections pass, preview cards included
//! (wrangler's local Browser Rendering), nothing runs that node, Browser
//! Rendering's Chrome is the one under target/cache, and nothing lands in
//! the system's wrangler cache.
//!
//! It runs the e2e (minutes; Docker, and the network on first use), so it
//! runs only by name:
//! `cargo test -p xtask --test broken_node_on_path -- --ignored --nocapture`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// Entries the check of the system's cache walks at most.
const WALK_ENTRIES_MAX: u32 = 100_000;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask sits below the repo root").to_path_buf()
}

/// Files and directories under `dir` modified after `since` (none if
/// `dir` is absent).
fn changed_since(dir: &Path, since: SystemTime) -> Vec<PathBuf> {
    let (mut changed, mut pending, mut seen) = (vec![], vec![dir.to_path_buf()], 0u32);
    // bounded: WALK_ENTRIES_MAX entries
    while let Some(next) = pending.pop() {
        let Ok(entries) = fs::read_dir(&next) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            assert!(seen <= WALK_ENTRIES_MAX, "{} holds more than {WALK_ENTRIES_MAX} entries", dir.display());
            let Ok(meta) = entry.metadata() else { continue };
            if meta.modified().is_ok_and(|m| m > since) {
                changed.push(entry.path());
            }
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    changed
}

/// The system's wrangler cache, where wrangler and miniflare would cache
/// without `XDG_CACHE_HOME`.
fn system_wrangler_cache() -> PathBuf {
    let home = PathBuf::from(std::env::var_os("HOME").expect("HOME is set"));
    match std::env::consts::OS {
        "macos" => home.join("Library/Caches/.wrangler"),
        _ => home.join(".cache/.wrangler"),
    }
}

#[test]
#[ignore = "runs the e2e's deploy, site and shell sections"]
fn the_e2e_passes_with_a_broken_node_first_on_path() {
    let root = repo_root();
    let dir = std::env::temp_dir().join(format!("broken-node-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("make the broken node's directory");
    let ran = dir.join("ran");
    let node = dir.join("node");
    let mut f = fs::File::create(&node).expect("write the broken node");
    writeln!(f, "#!/bin/sh\necho \"$0 $*\" >> '{}'\necho 'the node on PATH ran; it is broken' >&2\nexit 1", ran.display()).expect("write the broken node");
    drop(f);
    fs::set_permissions(&node, fs::Permissions::from_mode(0o755)).expect("make it executable");
    let refused = Command::new(&node).arg("--version").output().expect("run the broken node");
    assert!(!refused.status.success(), "the stand-in is broken");
    fs::remove_file(&ran).expect("forget the stand-in's own check");

    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(std::iter::once(dir.clone()).chain(std::env::split_paths(&inherited))).expect("a PATH");
    let which = Command::new("/bin/sh").args(["-c", "command -v node"]).env("PATH", &path).output().expect("ask the shell for node");
    assert_eq!(String::from_utf8_lossy(&which.stdout).trim(), node.display().to_string(), "the broken node is first on PATH");

    let started = SystemTime::now();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    println!("PATH={}", path.to_string_lossy());
    let status = Command::new(cargo).args(["xtask", "e2e", "--only", "deploy,site,shell"]).env("PATH", &path).current_dir(&root).status().expect("run the e2e");
    assert!(status.success(), "the e2e passed with a broken node first on PATH: {status}");
    assert!(!ran.exists(), "nothing ran the node on PATH: {}", fs::read_to_string(&ran).unwrap_or_default());

    let chrome = root.join("target/cache/.wrangler/chrome");
    let installs: Vec<PathBuf> = fs::read_dir(&chrome).expect("Browser Rendering's Chrome is under target/cache").flatten().map(|e| e.path()).collect();
    assert!(!installs.is_empty(), "{} holds a Chrome", chrome.display());
    println!("Browser Rendering's Chrome: {installs:?}");
    let system = system_wrangler_cache();
    let touched = changed_since(&system, started);
    assert!(touched.is_empty(), "nothing in {} changed during the run: {touched:?}", system.display());
    fs::remove_dir_all(&dir).expect("remove the broken node");
}
