//! Blobs (docs/MODEL.md: a file of 1 MiB or more is a pointer in git;
//! phase 2 slice E). The bytes live in the deployment's R2 bucket
//! (`BLOBS`) under `<fragment npub>/<sha256>`.
//!
//! An editor's client uploads the bytes (`PUT blobs/<sha>`: the cell hashes
//! them on the way in and keeps them only if they are what they claim),
//! then commits the pointer. The cell tracks the pointers at each pin, so
//! serving a pointer's path streams its bytes, and a blob no pointer at
//! `main` or `live` has named for a grace period is deleted: only the
//! latest versions' bytes are kept. A channel record that names one
//! (`attachments[].sha256`) keeps it too, while the record is kept.
//!
//! A page reads one of its fragment's blobs by hash at `__blob/<sha>` (a
//! step's screenshot, a chat's attachment), typed as its upload declared,
//! and an editor's page uploads one there (`PUT`, as the API's). The
//! fragment's preview card is one of its blobs too (card.rs), kept while
//! it is the card.

use fragment_core::{blob, site};
use fragment_proto::{ErrorCode, Role};
use serde_json::json;
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell, MetaKey};
use crate::js;

/// A pointer file's size: 126 bytes plus the digits of the size it names.
const POINTER_MIN_BYTES: u64 = 126;
/// How often the collection runs at most (it runs with the poll backstop).
const GC_EVERY_MS: i64 = 24 * 3600 * 1000;
/// `__blob` answers the same bytes for a hash forever.
const IMMUTABLE: &str = "private, max-age=31536000, immutable";

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
    pub(crate) fn blob_key(&self, sha: &str) -> CellResult<String> {
        Ok(format!("{}/{sha}", self.must(MetaKey::Npub)?))
    }

    /// A blob seen now; its type the latest upload's that declared one.
    fn record_blob(&self, sha: &str, size: u64, mime: Option<&str>) -> CellResult<()> {
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec(
            "INSERT INTO blobs (sha, size, uploaded_at, seen_at, mime) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (sha) DO UPDATE SET seen_at = excluded.seen_at, mime = coalesce(excluded.mime, blobs.mime)",
            vec![sha.into(), SqlStorageValue::Integer(size as i64), now.clone(), now, mime.map_or(SqlStorageValue::Null, Into::into)],
        )
    }

    /// `PUT /api/f/<name>/blobs/<sha256>`, or `PUT __blob/<sha256>` from
    /// the fragment's own page (editor): the body, streamed in and hashed
    /// on the way; bytes that are not what they claim are deleted. Its
    /// `content-type` is what `__blob` serves it as, when that is passive
    /// media.
    pub(crate) async fn put_blob(&self, caller: &Caller, sha: &str, req: &Request) -> CellResult<Response> {
        let answer = self.store_blob(caller, sha, req).await;
        // bytes this answer did not need are read all the same (js::drain)
        js::drain(req).await?;
        answer
    }

    async fn store_blob(&self, caller: &Caller, sha: &str, req: &Request) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        self.writable().await?;

        check_sha(sha)?;
        let mime = req.headers().get("content-type")?.as_deref().and_then(blob::served_type);
        let key = self.blob_key(sha)?;
        let (size, stored) = match js::blob_head(self.env.as_ref(), &key).await? {
            Some(size) => (size, false),
            None => {
                let body = req.inner().body().ok_or_else(|| CellError::invalid("a blob upload carries the bytes as its body"))?;
                let (size, digest) = js::blob_put(self.env.as_ref(), &key, body.into()).await?;
                if digest != sha {
                    js::blob_delete(self.env.as_ref(), &[key]).await?;
                    return Err(CellError::invalid(format!("the bytes hash to {digest}, not {sha}")));
                }
                (size, true)
            }
        };
        self.record_blob(sha, size, mime)?;
        json_response(&json!({ "ok": true, "sha": sha, "size": size, "stored": stored }))
    }

    /// `GET|HEAD __blob/<sha256>` on the fragment's origin (viewers and up:
    /// serve.rs): one of this fragment's blobs, as the type its upload
    /// declared, the same bytes forever. Another fragment's hash is not
    /// found here: this fragment's blobs are under its own npub.
    pub(crate) async fn serve_blob(&self, req: &Request, sha: &str) -> CellResult<Response> {
        if !blob::valid_sha(sha) {
            return Err(CellError::new(ErrorCode::NotFound, "a blob is named by its SHA-256, 64 lowercase hex characters"));
        }
        let etag = format!("\"{sha}\"");
        if let Some(resp) = crate::serve::not_modified(req, &etag, IMMUTABLE)? {
            return Ok(resp);
        }
        let rows = self.rows("SELECT mime FROM blobs WHERE sha = ?", vec![sha.into()])?;
        let mime = rows.first().and_then(|r| r["mime"].as_str()).unwrap_or("application/octet-stream").to_string();
        let mut resp = if req.method() == Method::Head {
            let size = js::blob_head(self.env.as_ref(), &self.blob_key(sha)?).await?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no blob {sha}")))?;
            let h = Headers::new();
            h.set("content-type", &mime)?;
            h.set("content-length", &size.to_string())?;
            h.set("etag", &etag)?;
            Response::empty()?.with_headers(h)
        } else {
            self.stream_blob(sha, &mime, req.headers().get("range")?.as_deref()).await?
        };
        resp.headers_mut().set("cache-control", IMMUTABLE)?;
        resp.headers_mut().set("x-content-type-options", "nosniff")?;
        Ok(resp)
    }

    /// Stores bytes the platform made (generated media) as a blob.
    pub(crate) async fn put_blob_bytes(&self, sha: &str, bytes: Vec<u8>) -> CellResult<()> {
        self.put_blob_typed(sha, bytes, None).await
    }

    /// Stores bytes the platform made as a blob served as `mime` (a card's
    /// JPEG: card.rs), which must be passive media.
    pub(crate) async fn put_blob_typed(&self, sha: &str, bytes: Vec<u8>, mime: Option<&str>) -> CellResult<()> {
        assert!(mime.is_none_or(|m| blob::served_type(m) == Some(m)), "a blob is typed as passive media, not {mime:?}");
        let key = self.blob_key(sha)?;
        let size = bytes.len() as u64;
        if js::blob_head(self.env.as_ref(), &key).await?.is_none() {
            js::blob_put_bytes(self.env.as_ref(), &key, &bytes).await?;
        }
        self.record_blob(sha, size, mime)
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
        let repo = self.must(MetaKey::Repo)?;
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

    /// From the alarm: deletes blobs no pointer at `main` or `live`, nor
    /// any kept record, has named for the grace period (uploads never
    /// committed or posted included).
    pub(crate) async fn collect_blobs(&self) -> CellResult<()> {
        let now = js::now_ms();
        let due: i64 = self.meta(MetaKey::BlobsGcAt)?.and_then(|s| s.parse().ok()).unwrap_or(0);
        if due > now {
            return Ok(());
        }
        let now_v = SqlStorageValue::Integer(now);
        self.exec("UPDATE blobs SET seen_at = ? WHERE sha IN (SELECT sha FROM pointers)", vec![now_v.clone()])?;
        // a record that names a blob (`attachments[].sha256`: a chat's files)
        // keeps it while the channel keeps the record
        self.exec(
            "UPDATE blobs SET seen_at = ? WHERE sha IN (
               SELECT json_extract(a.value, '$.sha256') FROM records r, json_each(r.body, '$.attachments') a
               WHERE json_valid(r.body) AND json_type(r.body, '$.attachments') = 'array' AND a.type = 'object')",
            vec![now_v.clone()],
        )?;
        // the preview card is named by the fragment itself (card.rs)
        if let Some(card) = self.cards()?.card {
            self.exec("UPDATE blobs SET seen_at = ? WHERE sha = ?", vec![now_v, card.blob.as_str().into()])?;
        }
        let count = self.drop_blobs("SELECT sha FROM blobs WHERE seen_at < ? LIMIT 1000", now - self.cfg.blob_grace_ms).await?;
        if count > 0 {
            self.event("blobs.collected", &format!("{count} blob(s) no branch has named for the grace period"), json!({ "count": count }));
        }
        self.set_meta(MetaKey::BlobsGcAt, &(now + self.cfg.blob_grace_ms.min(GC_EVERY_MS)).to_string())
    }

    /// Deletes the blobs `query` selects (it takes one parameter: the time
    /// they were last seen before), bytes first, then rows; how many.
    async fn drop_blobs(&self, query: &str, before: i64) -> CellResult<usize> {
        let stale: Vec<String> = self.rows(query, vec![SqlStorageValue::Integer(before)])?.into_iter().filter_map(|r| r["sha"].as_str().map(str::to_string)).collect();
        if !stale.is_empty() {
            let keys = stale.iter().map(|sha| self.blob_key(sha)).collect::<CellResult<Vec<_>>>()?;
            js::blob_delete(self.env.as_ref(), &keys).await?;
            for sha in &stale {
                self.exec("DELETE FROM blobs WHERE sha = ?", vec![sha.as_str().into()])?;
            }
        }
        Ok(stale.len())
    }

    /// A deleted fragment's blobs go after it (ended.rs): those under
    /// `prefix` (its key's), at most `pages` pages of them a call. Whether
    /// none is left.
    pub(crate) async fn delete_blobs_under(&self, prefix: &str, pages: usize) -> CellResult<bool> {
        assert!(prefix.ends_with('/') && pages > 0, "a life's blobs, a bounded number of pages");
        for _ in 0..pages {
            // each page listed is deleted, so the next list starts afresh
            let (keys, next) = js::blob_list(self.env.as_ref(), prefix, None).await?;
            if !keys.is_empty() {
                js::blob_delete(self.env.as_ref(), &keys).await?;
            }
            if next.is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Serves a file at a pin that is a pointer: its bytes, typed by the path.
    pub(crate) async fn serve_pointer(&self, which: &str, path: &str, range: Option<&str>) -> CellResult<Option<Response>> {
        match self.pointer(which, path)? {
            Some((sha, _)) => Ok(Some(self.stream_blob(&sha, site::mime_for_path(path), range).await?)),
            None => Ok(None),
        }
    }
}
