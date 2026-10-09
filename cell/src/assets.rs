//! The release's files in the Worker's Static Assets (`ASSETS`,
//! cell/wrangler.jsonc): the templates, each file named by its SHA-256.
//! The cell carries only their index
//! (crates/templates, made by its build), so an isolate holds a file's
//! bytes only while it reads one, and none of them in its memory from the
//! start; `cargo xtask build` writes the files beside the Worker
//! (`fragment_templates::write_assets`), and a deploy uploads them with it.
//!
//! Nothing is served from the assets but by the router: `run_worker_first`
//! hands every request to the cell, which reads `ASSETS` only here, each
//! file by the hash its own index names.

use fragment_core::manifest::Manifest;
use fragment_templates::{blessed, File};
use sha2::{Digest, Sha256};
use worker::*;

use crate::error::{CellError, CellResult};

/// A file's asset, its answer checked.
async fn fetch(env: &Env, f: &File) -> CellResult<Response> {
    let resp = env.assets("ASSETS")?.fetch(format!("https://assets.invalid/{}", f.sha256), None).await?;
    match resp.status_code() {
        200 => Ok(resp),
        404 => Err(CellError::host(format!("the release's {} ({}) is not among its Static Assets: a cell build writes them (cargo xtask build)", f.path, f.sha256))),
        s => Err(CellError::host(format!("the release's {} from its Static Assets: {s}", f.path))),
    }
}

/// A file's bytes, checked against its size and hash.
pub(crate) async fn read(env: &Env, f: &File) -> CellResult<Vec<u8>> {
    let bytes = fetch(env, f).await?.bytes().await?;
    if bytes.len() as u64 != f.size || hex::encode(Sha256::digest(&bytes)) != f.sha256 {
        return Err(CellError::host(format!("the release's {} in its Static Assets is not the bytes its index names ({})", f.path, f.sha256)));
    }
    Ok(bytes)
}

/// Files' bytes, read side by side, in their order.
pub(crate) async fn read_all(env: &Env, files: &[&'static File]) -> CellResult<Vec<Vec<u8>>> {
    futures_util::future::try_join_all(files.iter().map(|f| read(env, f))).await
}

/// A blessed template's manifest, read from the release, or why it is no
/// manifest a fragment on it runs (it names no blessed template, or the
/// template's does not parse).
pub(crate) async fn blessed_manifest(env: &Env, name: &str) -> CellResult<Result<Manifest, String>> {
    let file = match blessed::manifest_file(name) {
        Ok(f) => f,
        Err(why) => return Ok(Err(why)),
    };
    Ok(blessed::manifest(name, &read(env, file).await?))
}

/// A file's bytes as an answer with `headers`, streamed from the assets
/// (never held whole), its length its size.
pub(crate) async fn body(env: &Env, f: &File, headers: Headers) -> CellResult<Response> {
    let (_, body) = fetch(env, f).await?.into_parts();
    headers.set("content-length", &f.size.to_string())?;
    Ok(Response::from_body(body)?.with_headers(headers))
}
