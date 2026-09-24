//! Large files on the Rust cell (phase 2 slice E): a file of 1 MiB or more
//! is kept in git as a git-lfs pointer, and its bytes in the fragment's
//! blob store. Sync uploads the bytes before it commits the pointer, and
//! downloads a pointer's bytes when it pulls one, so a folder always holds
//! real files. Only a host that says so (`blobMinBytes` in status) gets
//! pointers: the TypeScript runtime keeps large files in git.

use std::cell::OnceCell;

use anyhow::{anyhow, bail, Result};
use fragment_core::blob;

use crate::api::Client;
use crate::sync::sha256_hex;

pub struct Blobs<'a> {
    client: &'a Client,
    name: &'a str,
    min: OnceCell<Option<u64>>,
}

impl<'a> Blobs<'a> {
    pub fn new(client: &'a Client, name: &'a str) -> Blobs<'a> {
        Blobs { client, name, min: OnceCell::new() }
    }

    /// The host's blob threshold, asked once (`None`: it keeps everything
    /// in git, as a host without a status route does).
    fn min_bytes(&self) -> Result<Option<u64>> {
        if let Some(m) = self.min.get() {
            return Ok(*m);
        }
        let resp = self.client.get(&format!("/api/f/{}/status", self.name))?;
        let m = if resp.status == 404 { None } else { self.client.call(resp)?["blobMinBytes"].as_u64() };
        Ok(*self.min.get_or_init(|| m))
    }

    fn path(&self, sha: &str) -> String {
        format!("/api/f/{}/blobs/{sha}", self.name)
    }

    /// What git holds for these local bytes: the bytes themselves, or, for
    /// a large file on a host with blobs, its pointer once the bytes are
    /// uploaded (an upload the store already has is skipped).
    pub fn store(&self, bytes: Vec<u8>) -> Result<Vec<u8>> {
        if bytes.len() < blob::BLOB_MIN_BYTES {
            return Ok(bytes);
        }
        match self.min_bytes()? {
            Some(min) if bytes.len() as u64 >= min => {}
            _ => return Ok(bytes),
        }
        let sha = sha256_hex(&bytes);
        let size = bytes.len() as u64;
        let head = self.client.head(&self.path(&sha))?;
        if head.status == 404 {
            self.client.call(self.client.put_blob(&self.path(&sha), bytes)?)?;
        } else if !head.ok() {
            bail!("checking blob {sha}: http {}", head.status);
        }
        Ok(blob::pointer(&sha, size).into_bytes())
    }

    /// A file's bytes from what git holds: a pointer's come from the blob
    /// store, checked against the hash it names.
    pub fn resolve(&self, bytes: Vec<u8>) -> Result<Vec<u8>> {
        let Some(p) = blob::parse(&bytes) else { return Ok(bytes) };
        let resp = self.client.get_sized(&self.path(&p.sha256), p.size)?;
        if !resp.ok() {
            bail!("the bytes of blob {} (http {}: {})", p.sha256, resp.status, resp.err_summary());
        }
        if sha256_hex(&resp.body) != p.sha256 {
            return Err(anyhow!("blob {} arrived with other bytes", p.sha256));
        }
        Ok(resp.body)
    }
}

/// The hash a local file is compared by: a pointer names its bytes' hash.
pub fn content_sha(git_bytes: &[u8]) -> String {
    match blob::parse(git_bytes) {
        Some(p) => p.sha256,
        None => sha256_hex(git_bytes),
    }
}

/// Whether a file this size in git could be a pointer to a local file this size.
pub fn could_point(git_size: u64, local_size: u64) -> bool {
    git_size <= blob::POINTER_MAX_BYTES as u64 && local_size >= blob::BLOB_MIN_BYTES as u64
}
