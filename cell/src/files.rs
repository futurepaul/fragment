//! The app's files (docs/MODEL.md: every file is in git; phase 2 slice E).
//!
//! Reads come from `main`, the working copy, at its pin: through the
//! `FILES` capability the app facet holds (`this.files` in queries,
//! `App.fetch`, and jobs), and as job steps (`job.files.read`). Writes land
//! on `main` as commits the cell makes: a mutation's file effects (one
//! commit per mutation, once the facet has committed it) and a job's
//! `job.files.write` steps, which may compare-and-swap on a file's blob
//! sha. Each commit is keyed by its mutation or step, so applying it again
//! commits nothing twice. A move of `main` that the cell's own commit made
//! carries that write's depth, so file triggers count toward the hop budget.

use std::collections::BTreeMap;

use fragment_core::codestorage::{commit_pack, FileChange};
use fragment_core::{blob, npub};
use fragment_proto::{limits, valid_repo_path, ErrorCode};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::FragmentCell;
use crate::js;

/// Set by the `Files` capability (entry.mjs); the router never sets or passes it.
pub const CAP_HEADER: &str = "x-fragment-cap";
/// Commits tried against a `main` that keeps moving.
const WRITE_ATTEMPTS: usize = 5;
/// Write keys (a commit or a push happens once per key) and own commits
/// are remembered this long; a pending mutation's last try comes well
/// inside it (channels.rs).
pub(crate) const WRITES_KEPT_MS: i64 = 7 * 24 * 3600 * 1000;

pub(crate) struct FileWrite {
    pub path: String,
    /// `None` removes the file.
    pub bytes: Option<Vec<u8>>,
}

pub(crate) enum Wrote {
    Commit(String),
    /// A compare-and-swap found the file changed.
    Conflict(String),
}

fn describe(sha: &Option<String>) -> String {
    match sha {
        Some(s) => format!("blob {}", &s[..s.len().min(12)]),
        None => "absent".into(),
    }
}

/// A commit's author: the principal's display form, and an address that
/// is never mail.
pub(crate) fn author(principal: &str) -> (String, String) {
    let name = npub::display(principal);
    let email = format!("{}@fragment.invalid", name.chars().take(64).collect::<String>());
    (name, email)
}

/// Bytes as a step or capability answer: text when it is UTF-8.
pub(crate) fn content_json(bytes: Vec<u8>) -> Value {
    match String::from_utf8(bytes) {
        Ok(text) => json!({ "text": text }),
        Err(e) => {
            use base64::Engine;
            json!({ "base64": base64::engine::general_purpose::STANDARD.encode(e.into_bytes()) })
        }
    }
}

/// Bytes from `{text}` or `{base64}` (a job step's write).
pub(crate) fn content_of(v: &Value) -> Result<Vec<u8>, String> {
    let obj = v.as_object().ok_or("a file's content is text or base64")?;
    fragment_core::effects::file_content(obj)?.ok_or_else(|| "a file's content is text or base64".into())
}

impl FragmentCell {
    /// Writes to `main` as one commit, once per `key`. `expect` maps a path
    /// to the blob sha it must have (`None`: it must not exist) at the head
    /// the commit lands on.
    pub(crate) async fn commit_files(
        &self,
        key: &str,
        writes: &[FileWrite],
        expect: &BTreeMap<String, Option<String>>,
        message: &str,
        principal: &str,
        depth: u32,
    ) -> CellResult<Wrote> {
        if writes.len() > limits::FILE_WRITES_MAX {
            return Err(CellError::invalid(format!("at most {} files per write", limits::FILE_WRITES_MAX)));
        }
        let total: usize = writes.iter().filter_map(|w| w.bytes.as_ref().map(Vec::len)).sum();
        if total > limits::FILE_WRITE_MAX_BYTES {
            return Err(CellError::too_large("the files written", total, limits::FILE_WRITE_MAX_BYTES));
        }
        self.commit(key, writes, expect, message, principal, depth).await
    }

    /// [`Self::commit_files`] without a write's limits: the platform's own
    /// commits (a template).
    pub(crate) async fn commit(
        &self,
        key: &str,
        writes: &[FileWrite],
        expect: &BTreeMap<String, Option<String>>,
        message: &str,
        principal: &str,
        depth: u32,
    ) -> CellResult<Wrote> {
        if let Some(r) = self.rows("SELECT sha FROM file_commits WHERE key = ?", vec![key.into()])?.first() {
            return Ok(Wrote::Commit(r["sha"].as_str().unwrap_or("").to_string()));
        }
        for w in writes {
            if !valid_repo_path(&w.path) {
                return Err(CellError::invalid(format!("{:?} is not a file path (relative, no . or .. segments)", w.path)));
            }
        }
        let repo = self.must("repo")?;
        let cs = self.cs()?;
        let (author, email) = author(principal);
        for _ in 0..WRITE_ATTEMPTS {
            let head = cs.branch_head(&repo, "main").await?;
            for (path, want) in expect {
                let current = match &head {
                    Some(h) => cs.head(&repo, h, path).await?.map(|f| f.blob_sha),
                    None => None,
                };
                if current != *want {
                    return Ok(Wrote::Conflict(format!("{path} is {} (expected {})", describe(&current), describe(want))));
                }
            }
            let changes: Vec<FileChange> = writes
                .iter()
                .map(|w| match &w.bytes {
                    Some(b) => FileChange::Upsert { path: &w.path, bytes: b },
                    None => FileChange::Delete { path: &w.path },
                })
                .collect();
            let pack = commit_pack("main", head.as_deref(), message, (&author, &email), &changes);
            // Held until the commit is recorded as ours: its push webhook can
            // arrive before `commit` returns, and the pin move it causes must
            // find the write's depth.
            let held = self.plane.lock().await;
            let Some(tip) = cs.commit(&repo, pack).await? else { continue };
            let now = SqlStorageValue::Integer(js::now_ms());
            self.exec("INSERT OR IGNORE INTO file_commits (key, sha, at) VALUES (?, ?, ?)", vec![key.into(), tip.as_str().into(), now.clone()])?;
            self.exec("INSERT OR IGNORE INTO own_commits (sha, depth, at) VALUES (?, ?, ?)", vec![tip.as_str().into(), SqlStorageValue::Integer(depth.into()), now])?;
            drop(held);
            // The pin follows now (the webhook and the poll would, later).
            if let Err(e) = self.interpret(&["main"]).await {
                self.event("files.refresh-failed", &e.message, json!({ "commit": tip }));
            }
            return Ok(Wrote::Commit(tip));
        }
        Err(CellError::new(ErrorCode::UpstreamFailed, format!("main kept moving under {WRITE_ATTEMPTS} commits; try again")))
    }

    /// The hop depth of a move of `main` to `sha`: one past the write that
    /// made it, or 0 for anyone else's commit.
    pub(crate) fn commit_depth(&self, sha: Option<&str>) -> CellResult<u32> {
        let Some(sha) = sha else { return Ok(0) };
        let rows = self.rows("SELECT depth FROM own_commits WHERE sha = ?", vec![sha.into()])?;
        Ok(rows.first().and_then(|r| r["depth"].as_u64()).map_or(0, |d| d as u32 + 1))
    }

    /// From the alarm: forgets old write keys and own commits.
    pub(crate) fn trim_writes(&self) -> CellResult<()> {
        self.trim_writes_before(js::now_ms() - WRITES_KEPT_MS)
    }

    /// Forgets write keys and own commits made before `before_ms` (a test
    /// ages them by passing a later time).
    pub(crate) fn trim_writes_before(&self, before_ms: i64) -> CellResult<()> {
        let before = SqlStorageValue::Integer(before_ms);
        self.exec("DELETE FROM file_commits WHERE at < ?", vec![before.clone()])?;
        self.exec("DELETE FROM own_commits WHERE at < ?", vec![before.clone()])?;
        self.exec("DELETE FROM sent WHERE at < ?", vec![before])
    }

    /// A file's bytes at `main`'s pin, up to the read limit; `None` when absent.
    pub(crate) async fn read_main(&self, path: &str) -> CellResult<Option<Vec<u8>>> {
        let (Some(pin), Some(row)) = (self.pin("main")?, self.tree_row("main", path)?) else { return Ok(None) };
        let too_large = |size: u64| {
            CellError::too_large(&format!("{path} ({size} bytes; serve larger files from the site)"), size as usize, limits::FILE_READ_MAX_BYTES)
        };
        let size = row["size"].as_u64().unwrap_or(0);
        if size as usize > limits::FILE_READ_MAX_BYTES {
            return Err(too_large(size));
        }
        let bytes = self.cs()?.read(&self.must("repo")?, &pin, path, limits::FILE_READ_MAX_BYTES).await?;
        match bytes {
            Some(b) => match blob::parse(&b) {
                Some(p) => Err(too_large(p.size)),
                None => Ok(Some(b)),
            },
            None => Ok(None),
        }
    }

    /// Files at `main`'s pin under `prefix`: `[{path, size}]`.
    pub(crate) fn list_main(&self, prefix: &str) -> CellResult<Vec<Value>> {
        let blobs = self.pointer_sizes("main")?;
        Ok(self
            .tree_rows("main")?
            .into_iter()
            .filter(|r| r["path"].as_str().is_some_and(|p| p.starts_with(prefix)))
            .map(|r| match r["path"].as_str().and_then(|p| blobs.get(p)) {
                Some(size) => json!({ "path": r["path"], "size": size, "blob": true }),
                None => json!({ "path": r["path"], "size": r["size"] }),
            })
            .collect())
    }

    /// A file at `main`'s pin: `{path, size, sha, commit}` (`sha` is its git
    /// blob, what a compare-and-swap names); `None` when absent.
    pub(crate) async fn stat_main(&self, path: &str) -> CellResult<Option<Value>> {
        let (Some(pin), Some(_)) = (self.pin("main")?, self.tree_row("main", path)?) else { return Ok(None) };
        let head = self.cs()?.head(&self.must("repo")?, &pin, path).await?;
        Ok(head.map(|h| json!({ "path": path, "size": h.size, "sha": h.blob_sha, "commit": h.last_commit_sha })))
    }

    /// `POST /cap/files/<op>`: the app facet's `Files` capability.
    pub(crate) async fn cap_files(&self, op: &str, body: &Value) -> CellResult<Response> {
        self.name()?;
        let path = body["path"].as_str().unwrap_or("");
        match op {
            "read" => match self.read_main(path).await? {
                Some(bytes) => Ok(Response::from_bytes(bytes)?),
                None => Err(CellError::new(ErrorCode::NotFound, format!("no file {path}"))),
            },
            "list" => Ok(Response::from_json(&self.list_main(body["prefix"].as_str().unwrap_or(""))?)?),
            "stat" => Ok(Response::from_json(&self.stat_main(path).await?)?),
            _ => Err(CellError::new(ErrorCode::NotFound, format!("no capability files.{op}"))),
        }
    }
}
