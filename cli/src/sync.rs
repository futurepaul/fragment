// Bidirectional folder sync (obsidian-style): last writer wins, conflicts are
// written out as <path>.remote-<ts> and reported, never silently merged.
use crate::api::{encode_q, Client};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Default)]
pub struct SyncState {
    pub name: String,
    pub files: HashMap<String, FileState>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FileState {
    pub rev: u64,
    pub sha256: String,
}

#[derive(Default)]
pub struct Report {
    pub pushed: Vec<String>,
    pub pulled: Vec<String>,
    pub deleted_remote: Vec<String>,
    pub deleted_local: Vec<String>,
    pub conflicts: Vec<String>,
}

impl Report {
    pub fn print(&self) {
        let p = |label: &str, v: &Vec<String>| {
            if !v.is_empty() {
                println!("  {label}: {}", v.len());
                for f in v {
                    println!("    {f}");
                }
            }
        };
        p("pushed", &self.pushed);
        p("pulled", &self.pulled);
        p("deleted remotely", &self.deleted_remote);
        p("deleted locally", &self.deleted_local);
        p("CONFLICTS (remote copy saved next to local)", &self.conflicts);
        if self.pushed.is_empty()
            && self.pulled.is_empty()
            && self.conflicts.is_empty()
            && self.deleted_remote.is_empty()
            && self.deleted_local.is_empty()
        {
            println!("  up to date");
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join(".fragment").join("state.json")
}

pub fn load_state(dir: &Path, name: &str) -> Result<SyncState> {
    let p = state_path(dir);
    if p.exists() {
        let s: SyncState = serde_json::from_slice(&fs::read(&p)?).context("bad .fragment/state.json")?;
        if s.name != name {
            anyhow::bail!("directory is synced to fragment '{}', not '{}'", s.name, name);
        }
        Ok(s)
    } else {
        Ok(SyncState { name: name.to_string(), files: HashMap::new() })
    }
}

pub fn save_state(dir: &Path, state: &SyncState) -> Result<()> {
    let p = state_path(dir);
    fs::create_dir_all(p.parent().unwrap())?;
    fs::write(p, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

/// Local files: path (relative, /-separated) -> (sha256, bytes length). Skips
/// dotfiles, .fragment/, symlinks, and conflict artifacts (*.remote-*).
pub fn scan_local(dir: &Path) -> Result<HashMap<String, (String, u64)>> {
    let mut out = HashMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if !d.exists() {
            continue;
        }
        for entry in fs::read_dir(&d)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let ft = entry.file_type()?;
            let path = entry.path();
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if name.contains(".remote-") {
                continue;
            }
            let rel = path.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
            let bytes = fs::read(&path)?;
            out.insert(rel, (sha256_hex(&bytes), bytes.len() as u64));
        }
    }
    Ok(out)
}

struct RemoteFile {
    rev: u64,
    sha256: String,
    deleted: bool,
}

pub fn sync_once(client: &Client, name: &str, dir: &Path) -> Result<Report> {
    let mut state = load_state(dir, name)?;
    let local = scan_local(dir)?;
    let remote_resp = client.call(client.get(&format!("/api/f/{name}/files?since_rev=0"))?)?;
    let mut remote: HashMap<String, RemoteFile> = HashMap::new();
    for f in remote_resp["files"].as_array().cloned().unwrap_or_default() {
        remote.insert(
            f["path"].as_str().unwrap_or("").to_string(),
            RemoteFile {
                rev: f["rev"].as_u64().unwrap_or(0),
                sha256: f["sha256"].as_str().unwrap_or("").to_string(),
                deleted: f["deleted"].as_bool().unwrap_or(false),
            },
        );
    }

    let mut report = Report::default();
    let mut paths: Vec<String> = Vec::new();
    for k in local.keys() {
        paths.push(k.clone());
    }
    for k in remote.keys() {
        if !paths.contains(k) {
            paths.push(k.clone());
        }
    }
    for k in state.files.keys() {
        if !paths.contains(k) {
            paths.push(k.clone());
        }
    }
    paths.sort();

    for path in paths {
        let l = local.get(&path);
        let r = remote.get(&path).filter(|r| !r.deleted);
        let r_tomb = remote.get(&path).filter(|r| r.deleted);
        let s = state.files.get(&path).cloned();

        match (l, r) {
            (Some((lhash, _)), Some(rf)) => {
                let local_changed = s.as_ref().map(|st| st.sha256 != *lhash).unwrap_or(true);
                let remote_changed = s.as_ref().map(|st| st.rev != rf.rev).unwrap_or(true);
                match (local_changed, remote_changed) {
                    (false, false) => {}
                    (true, false) => {
                        push_file(client, name, dir, &path, s.as_ref().map(|x| x.rev).unwrap_or(0), &mut state, &mut report)?;
                    }
                    (false, true) => {
                        pull_file(client, name, dir, &path, &mut state, &mut report)?;
                    }
                    (true, true) => {
                        // identical content on both sides is not a conflict
                        // (e.g. a racing sync already pushed our change):
                        // adopt the remote rev instead of spawning artifacts
                        if rf.sha256 == *lhash {
                            state.files.insert(path.clone(), FileState { rev: rf.rev, sha256: rf.sha256.clone() });
                        } else {
                            conflict_file(client, name, dir, &path, &mut report)?;
                        }
                    }
                }
            }
            (Some((lhash, _)), None) => {
                // remote absent or tombstoned
                if let (Some(tomb), Some(st)) = (r_tomb, &s) {
                    if tomb.rev == st.rev && st.sha256 == *lhash {
                        // we already synced this deletion; accept it locally
                        fs::remove_file(dir.join(&path)).ok();
                        state.files.remove(&path);
                        report.deleted_local.push(path.clone());
                        continue;
                    }
                }
                let base = r_tomb.map(|t| t.rev).unwrap_or(0);
                push_file(client, name, dir, &path, base, &mut state, &mut report)?;
            }
            (None, Some(rf)) => match &s {
                Some(st) if st.rev == rf.rev => {
                    // deleted locally, unchanged remotely -> delete remotely
                    let resp = client.delete(&format!("/api/f/{name}/file?path={}", encode_q(&path)))?;
                    if resp.ok() {
                        state.files.remove(&path);
                        report.deleted_remote.push(path.clone());
                    } else {
                        anyhow::bail!("delete {} failed: {}", path, resp.err_summary());
                    }
                }
                _ => {
                    pull_file(client, name, dir, &path, &mut state, &mut report)?;
                }
            },
            (None, None) => {
                state.files.remove(&path);
            }
        }
    }

    save_state(dir, &state)?;
    Ok(report)
}

fn push_file(
    client: &Client,
    name: &str,
    dir: &Path,
    path: &str,
    base_rev: u64,
    state: &mut SyncState,
    report: &mut Report,
) -> Result<()> {
    let bytes = fs::read(dir.join(path))?;
    let resp = client.put_bytes(
        &format!("/api/f/{name}/file?path={}&base_rev={base_rev}", encode_q(path)),
        bytes.clone(),
    )?;
    if resp.status == 409 {
        conflict_file(client, name, dir, path, report)?;
        return Ok(());
    }
    if !resp.ok() {
        anyhow::bail!("push {} failed: {}", path, resp.err_summary());
    }
    let v = resp.json()?;
    state.files.insert(path.to_string(), FileState { rev: v["rev"].as_u64().unwrap_or(0), sha256: sha256_hex(&bytes) });
    report.pushed.push(path.to_string());
    Ok(())
}

fn pull_file(client: &Client, name: &str, dir: &Path, path: &str, state: &mut SyncState, report: &mut Report) -> Result<()> {
    let resp = client.get(&format!("/api/f/{name}/file?path={}", encode_q(path)))?;
    if !resp.ok() {
        anyhow::bail!("pull {} failed: {}", path, resp.err_summary());
    }
    let target = dir.join(path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&target, &resp.body)?;
    state.files.insert(
        path.to_string(),
        FileState { rev: resp.rev.unwrap_or(0), sha256: sha256_hex(&resp.body) },
    );
    report.pulled.push(path.to_string());
    Ok(())
}

fn conflict_file(client: &Client, name: &str, dir: &Path, path: &str, report: &mut Report) -> Result<()> {
    let resp = client.get(&format!("/api/f/{name}/file?path={}", encode_q(path)))?;
    if resp.ok() {
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        let conflict_path = format!("{path}.remote-{ts}");
        fs::write(dir.join(&conflict_path), &resp.body)?;
    }
    report.conflicts.push(path.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_skips_dotfiles_and_conflicts() {
        let dir = std::env::temp_dir().join(format!("fragment-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("site")).unwrap();
        fs::create_dir_all(dir.join(".fragment")).unwrap();
        fs::write(dir.join("site/index.html"), b"hi").unwrap();
        fs::write(dir.join(".hidden"), b"x").unwrap();
        fs::write(dir.join(".fragment/state.json"), b"{}").unwrap();
        fs::write(dir.join("a.md.remote-123"), b"x").unwrap();
        let got = scan_local(&dir).unwrap();
        assert!(got.contains_key("site/index.html"));
        assert_eq!(got.len(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn state_roundtrip() {
        let dir = std::env::temp_dir().join(format!("fragment-state-{}", std::process::id()));
        let mut st = SyncState { name: "x".into(), files: HashMap::new() };
        st.files.insert("a".into(), FileState { rev: 3, sha256: "beef".into() });
        save_state(&dir, &st).unwrap();
        let back = load_state(&dir, "x").unwrap();
        assert_eq!(back.files["a"].rev, 3);
        assert!(load_state(&dir, "y").is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
