//! Blobs (docs/MODEL.md: a file of 1 MiB or more is a pointer in git;
//! phase 2 slice E). The bytes live in the fleet's blob store (`BLOBS`,
//! celld's R2 over the fleet bucket) under `<fragment npub>/<sha256>`.
//!
//! An editor's client uploads the bytes (`PUT blobs/<sha>`: the cell hashes
//! them on the way in and keeps them only if they are what they claim),
//! then commits the pointer. The cell tracks the pointers at each pin, so
//! serving a pointer's path streams its bytes, and a blob no pointer at
//! `main` or `live` has named for a grace period is deleted: only the
//! latest versions' bytes are kept.

use fragment_core::{blob, site};
use fragment_proto::{ErrorCode, Role};
use serde_json::json;
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::js;

/// A pointer file's size: 126 bytes plus the digits of the size it names.
const POINTER_MIN_BYTES: u64 = 126;
/// How often the collection runs at most (it runs with the poll backstop).
const GC_EVERY_MS: i64 = 24 * 3600 * 1000;

fn check_sha(sha: &str) -> CellResult<()> {
    if blob::valid_sha(sha) {
        Ok(())
    } else {
        Err(CellError::invalid("a blob is named by its SHA-256, 64 lowercase hex characters"))
    }
}

/// Whether a file of this size could be a pointer (only those are read to find out).
pub(crate) fn maybe_pointer(size: u64) -> bool {
    (POINTER_MIN_BYTES..=blob::POINTER_MAX_BYTES as u64).contains(&size)
}

impl FragmentCell {
    fn blob_key(&self, sha: &str) -> CellResult<String> {
        Ok(format!("{}/{sha}", self.must("npub")?))
    }

    fn record_blob(&self, sha: &str, size: u64) -> CellResult<()> {
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec(
            "INSERT INTO blobs (sha, size, uploaded_at, seen_at) VALUES (?, ?, ?, ?) ON CONFLICT (sha) DO UPDATE SET seen_at = excluded.seen_at",
            vec![sha.into(), SqlStorageValue::Integer(size as i64), now.clone(), now],
        )
    }

    /// `PUT /api/f/<name>/blobs/<sha256>` (editor): the body, streamed in and
    /// hashed on the way; bytes that are not what they claim are deleted.
    pub(crate) async fn put_blob(&self, caller: &Caller, sha: &str, req: &Request) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        check_sha(sha)?;
        let key = self.blob_key(sha)?;
        if let Some(size) = js::blob_head(self.env.as_ref(), &key).await? {
            self.record_blob(sha, size)?;
            return json_response(&json!({ "ok": true, "sha": sha, "size": size, "stored": false }));
        }
        let body = req.inner().body().ok_or_else(|| CellError::invalid("a blob upload carries the bytes as its body"))?;
        let (size, digest) = js::blob_put(self.env.as_ref(), &key, body.into()).await?;
        if digest != sha {
            js::blob_delete(self.env.as_ref(), &[key]).await?;
            return Err(CellError::invalid(format!("the bytes hash to {digest}, not {sha}")));
        }
        self.record_blob(sha, size)?;
        json_response(&json!({ "ok": true, "sha": sha, "size": size, "stored": true }))
    }

    /// `GET|HEAD /api/f/<name>/blobs/<sha256>` (viewer)
    pub(crate) async fn get_blob(&self, caller: &Caller, sha: &str, head: bool, range: Option<&str>) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        check_sha(sha)?;
        if head {
            let size = js::blob_head(self.env.as_ref(), &self.blob_key(sha)?).await?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no blob {sha}")))?;
            let h = Headers::new();
            h.set("content-length", &size.to_string())?;
            return Ok(Response::empty()?.with_headers(h));
        }
        self.stream_blob(sha, "application/octet-stream", range).await
    }

    /// A blob as a response: its bytes (or the asked range), immutable.
    pub(crate) async fn stream_blob(&self, sha: &str, mime: &str, range: Option<&str>) -> CellResult<Response> {
        let found = js::blob_get(self.env.as_ref(), &self.blob_key(sha)?, range).await?;
        let b = found.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("the bytes of blob {sha} are gone")))?;
        let h = Headers::new();
        h.set("content-type", mime)?;
        h.set("etag", &format!("\"{sha}\""))?;
        h.set("accept-ranges", "bytes")?;
        let status = match b.range {
            Some((offset, length)) => {
                h.set("content-range", &format!("bytes {offset}-{}/{}", offset + length.max(1) - 1, b.size))?;
                h.set("content-length", &length.to_string())?;
                206
            }
            None => {
                h.set("content-length", &b.size.to_string())?;
                200
            }
        };
        Ok(Response::from_body(ResponseBody::Stream(b.body))?.with_status(status).with_headers(h))
    }

    /// The blob a file at a pin points to: (sha, size).
    pub(crate) fn pointer(&self, which: &str, path: &str) -> CellResult<Option<(String, u64)>> {
        let rows = self.rows("SELECT sha, size FROM pointers WHERE ref = ? AND path = ?", vec![which.into(), path.into()])?;
        Ok(rows.first().map(|r| (r["sha"].as_str().unwrap_or("").to_string(), r["size"].as_u64().unwrap_or(0))))
    }

    /// Every pointer at a pin: path → the size of the bytes it names.
    pub(crate) fn pointer_sizes(&self, which: &str) -> CellResult<std::collections::BTreeMap<String, u64>> {
        Ok(self
            .rows("SELECT path, size FROM pointers WHERE ref = ?", vec![which.into()])?
            .into_iter()
            .filter_map(|r| Some((r["path"].as_str()?.to_string(), r["size"].as_u64()?)))
            .collect())
    }

    /// After a pin moved: re-reads the changed paths that could be pointers.
    /// `sizes` is the new tree's size by path; a changed path absent from it was deleted.
    pub(crate) async fn track_pointers(&self, which: &str, sha: &str, changed: &[String], sizes: &std::collections::HashMap<&str, u64>) -> CellResult<()> {
        let repo = self.must("repo")?;
        let cs = self.cs()?;
        for path in changed {
            self.exec("DELETE FROM pointers WHERE ref = ? AND path = ?", vec![which.into(), path.as_str().into()])?;
            let Some(size) = sizes.get(path.as_str()) else { continue };
            if !maybe_pointer(*size) {
                continue;
            }
            let Some(bytes) = cs.read(&repo, sha, path, blob::POINTER_MAX_BYTES).await? else { continue };
            if let Some(p) = blob::parse(&bytes) {
                self.exec(
                    "INSERT INTO pointers (ref, path, sha, size) VALUES (?, ?, ?, ?)",
                    vec![which.into(), path.as_str().into(), p.sha256.as_str().into(), SqlStorageValue::Integer(p.size as i64)],
                )?;
            }
        }
        Ok(())
    }

    /// From the alarm: deletes blobs no pointer at `main` or `live` has
    /// named for the grace period (uploads never committed included).
    pub(crate) async fn collect_blobs(&self) -> CellResult<()> {
        let now = js::now_ms();
        let due: i64 = self.meta("blobs_gc_at")?.and_then(|s| s.parse().ok()).unwrap_or(0);
        if due > now {
            return Ok(());
        }
        let now_v = SqlStorageValue::Integer(now);
        self.exec("UPDATE blobs SET seen_at = ? WHERE sha IN (SELECT sha FROM pointers)", vec![now_v])?;
        let stale: Vec<String> = self
            .rows("SELECT sha FROM blobs WHERE seen_at < ? LIMIT 1000", vec![SqlStorageValue::Integer(now - self.cfg.blob_grace_ms)])?
            .into_iter()
            .filter_map(|r| r["sha"].as_str().map(str::to_string))
            .collect();
        if !stale.is_empty() {
            let keys = stale.iter().map(|sha| self.blob_key(sha)).collect::<CellResult<Vec<_>>>()?;
            js::blob_delete(self.env.as_ref(), &keys).await?;
            for sha in &stale {
                self.exec("DELETE FROM blobs WHERE sha = ?", vec![sha.as_str().into()])?;
            }
            self.event("blobs.collected", &format!("{} blob(s) no branch has named for the grace period", stale.len()), json!({ "count": stale.len() }));
        }
        self.set_meta("blobs_gc_at", &(now + self.cfg.blob_grace_ms.min(GC_EVERY_MS)).to_string())
    }

    /// A deleted fragment's blobs go with it.
    pub(crate) async fn delete_blobs(&self) -> CellResult<()> {
        let prefix = format!("{}/", self.must("npub")?);
        let mut cursor: Option<String> = None;
        loop {
            let (keys, next) = js::blob_list(self.env.as_ref(), &prefix, cursor.as_deref()).await?;
            if !keys.is_empty() {
                js::blob_delete(self.env.as_ref(), &keys).await?;
            }
            match next {
                Some(c) => cursor = Some(c),
                None => return Ok(()),
            }
        }
    }

    /// Serves a file at a pin that is a pointer: its bytes, typed by the path.
    pub(crate) async fn serve_pointer(&self, which: &str, path: &str, range: Option<&str>) -> CellResult<Option<Response>> {
        match self.pointer(which, path)? {
            Some((sha, _)) => Ok(Some(self.stream_blob(&sha, site::mime_for_path(path), range).await?)),
            None => Ok(None),
        }
    }
}
