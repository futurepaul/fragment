//! Request handlers: the entire blobsd surface (docs/blob-tier.md).
//!
//! - `PUT /upload`      — authenticated write, idempotent on hash
//! - `GET/HEAD /<sha>`  — data-plane streamed reads, single-range support
//! - `DELETE /<sha>`    — authenticated delete of descriptor row AND object
//! - `GET /list/<npub>` — authenticated self-list, bounded
//! - `GET /`            — health

use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use http_body_util::BodyExt as _;
use futures_util::StreamExt as _;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::app::AppState;
use crate::auth::{self, Verb};
use crate::bucket::PutCreate;
use crate::db::UploadRow;
use crate::error::ApiError;
use crate::logging::RequestMetrics;

/// Chunk granularity for staged-upload reads and staged-file payloads.
/// 256 KiB keeps syscalls cheap while buffers stay flat.
const CHUNK_BYTES: usize = 256 * 1024;

/// After losing a put-if-absent race, how long we wait for the winner's
/// descriptor row before reclaiming an orphaned object commit. The winner
/// inserts its row immediately after its object PUT succeeds, so this window
/// covers scheduler skew between two tasks; bounded iterations per style.
const CONVERGE_POLLS: usize = 50;
const CONVERGE_SLEEP_MS: u64 = 25;

const MAX_MIME_LEN: usize = 255;

/// Wire DTO exactly as specced: `{sha256, size, type, uploaded}`.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct BlobDescriptor {
    pub sha256: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub blob_type: String,
    /// Unix seconds.
    pub uploaded: i64,
}

impl BlobDescriptor {
    fn from_row(row: &UploadRow) -> Self {
        BlobDescriptor {
            sha256: row.sha256.clone(),
            size: u64::try_from(row.size.max(0)).unwrap_or(0),
            blob_type: row.mime.clone(),
            uploaded: row.uploaded_at,
        }
    }
}

fn descriptor_response(row: UploadRow) -> Response {
    // Egress pairing: response identity echoes the stored row verbatim so
    // replays observe byte-identical descriptors.
    debug_assert_eq!(row.sha256.len(), 64);
    (StatusCode::OK, Json(BlobDescriptor::from_row(&row))).into_response()
}

// ---------------------------------------------------------------- health ---

pub async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "name": "blobsd",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ---------------------------------------------------------------- helpers ---

fn hex_64_lower(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
}

/// Constant-time-enough bearer comparison: fixed workload across the padded
/// length so probe timing does not reveal match progress on private-read
/// instances.
fn token_matches(provided: &str, expected: &str) -> bool {
    const MAX_TOKEN_BYTES: usize = 256;
    let provided = provided.as_bytes();
    let expected = expected.as_bytes();
    let common_len = provided.len().min(expected.len());
    let mut diff = u64::try_from(provided.len() ^ expected.len()).unwrap_or(u64::MAX);
    // Loop bound fixed at MAX_TOKEN_BYTES regardless of input sizes.
    for i in 0..MAX_TOKEN_BYTES {
        let bit = match (provided.get(i), expected.get(i)) {
            (Some(a), Some(b)) => u64::from(a ^ b),
            _ => {
                if i >= common_len {
                    continue;
                }
                1
            }
        };
        diff |= bit;
    }
    diff == 0
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs() as i64
}

pub(crate) fn authorize(
    state: &AppState,
    headers: &HeaderMap,
    verb: Verb,
) -> Result<auth::Verified, ApiError> {
    // Internal callers (the fragment runtime) authenticate with the shared
    // bearer on every verb — they hold no owner key to sign events, and
    // bytes-first ordering must not force owner secrets into workers.
    // Attributed to the fleet's first allowlisted pubkey so /list stays
    // coherent for internally-placed blobs.
    if let Some(value) = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(provided) = value.strip_prefix("Bearer ") {
            if token_matches(provided, &state.cfg.internal_token) {
                let pubkey_hex = state
                    .cfg
                    .allow_hex_pubkeys
                    .iter()
                    .next()
                    .cloned()
                    // wildcard mode leaves the set empty by design; internal
                    // blobs attribute to a stable synthetic identity instead
                    .unwrap_or_else(|| "0".repeat(64));
                return Ok(auth::Verified { pubkey_hex });
            }
            return Err(ApiError::BadReadToken);
        }
    }
    let header_value = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::MissingAuth)?;
    auth::verify_authorization(
        header_value,
        &state.cfg.public_url,
        verb,
        &state.cfg.allow_hex_pubkeys,
        state.cfg.allow_all,
        now_unix_secs(),
    )
}

/// Data-plane gate: PUBLIC_GET instances serve anyone; private ones demand
/// the internal bearer token on reads so an accidentally exposed instance
/// never leaks cell bytes silently.
pub(crate) fn authorize_read(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    if state.cfg.public_get {
        return Ok(());
    }
    let provided = headers
        .get(http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(ApiError::ReadTokenRequired)?;
    if token_matches(provided, &state.cfg.internal_token) {
        Ok(())
    } else {
        Err(ApiError::BadReadToken)
    }
}

/// RFC 9110 single-interval byte ranges only. Resolved inclusive [start, end]
/// against `size`; unknown units answer BadRange; multi-interval is refused
/// outright rather than silently degraded to v1 behavior.
pub(crate) fn resolve_single_range(header_value: &str, size: u64) -> Result<(u64, u64), ApiError> {
    let rest = match header_value.split_once('=') {
        Some((unit, rest)) if unit.eq_ignore_ascii_case("bytes") => rest,
        _ => return Err(ApiError::BadRange),
    };
    if rest.contains(',') {
        return Err(ApiError::MultiRange);
    }
    let trimmed = rest.trim();
    if let Some(suffix_text) = trimmed.strip_prefix('-') {
        // "-N": final N bytes. N larger than the object yields the object.
        let n: u64 = suffix_text.trim().parse().map_err(|_| ApiError::BadRange)?;
        if n == 0 || size == 0 {
            return Err(ApiError::RangeNotSatisfiable(size));
        }
        let start = size.saturating_sub(n.min(size));
        return Ok((start, size - 1));
    }
    let (start_text, end_text) = trimmed.split_once('-').ok_or(ApiError::BadRange)?;
    let start: u64 = start_text.trim().parse().map_err(|_| ApiError::BadRange)?;
    if start >= size || size == 0 {
        return Err(ApiError::RangeNotSatisfiable(size));
    }
    match end_text.trim() {
        "" => Ok((start, size - 1)),
        end => {
            let end: u64 = end.parse().map_err(|_| ApiError::BadRange)?;
            Ok((start, end.min(size - 1)))
        }
    }
}

// ----------------------------------------------------------------- upload ---

#[derive(Debug, Default, serde::Deserialize)]
pub struct UploadQuery {
    sha256: Option<String>,
}

/// Stream one request body into a fresh stage file, hashing while writing.
/// Memory stays flat regardless of Content-Length lies: every pass consumes
/// exactly the frames available now; the loop exits ONLY on EOF, an error,
/// or the ceiling. Consumes its file handle and flushes before returning.
async fn stage_body(
    mut body: Body,
    mut file: tokio::fs::File,
    metrics: Arc<RequestMetrics>,
    max_bytes: u64,
) -> Result<(Sha256, u64), ApiError> {
    use tokio::io::AsyncWriteExt;

    let mut hasher = Sha256::new();
    let mut total: u64 = 0;
    loop {
        let frame = body
            .frame()
            .await
            .transpose()
            .map_err(|_| ApiError::BadRequest("body_read"))?;
        let bytes = match frame {
            None => break, // EOF: only normal exit
            Some(frame) => frame.into_data().unwrap_or_default(),
        };
        total += bytes.len() as u64;
        if total > max_bytes {
            return Err(ApiError::TooLarge(max_bytes));
        }
        hasher.update(&bytes);
        metrics.add_bytes_in(bytes.len() as u64);
        file.write_all(&bytes).await.map_err(io_stage_fail)?;
    }
    file.flush().await.map_err(io_stage_fail)?;
    Ok((hasher, total))
}

pub async fn upload(
    State(state): State<AppState>,
    Extension(metrics): Extension<Arc<RequestMetrics>>,
    headers: HeaderMap,
    Query(query): Query<UploadQuery>,
    body: Body,
) -> Result<Response, ApiError> {
    let verified = authorize(&state, &headers, Verb::Upload)?;

    // Size contract enforced BEFORE reading: Content-Length mandatory, and
    // declared oversize answers without touching disk.
    let content_length: u64 = headers
        .get(http::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse().ok())
        .ok_or(ApiError::MissingLength)?;
    if content_length > state.cfg.max_blob_bytes {
        return Err(ApiError::TooLarge(state.cfg.max_blob_bytes));
    }

    let mime = match headers.get(http::header::CONTENT_TYPE) {
        None => "application/octet-stream".to_string(),
        Some(value) => {
            let text = value.to_str().map_err(|_| ApiError::BadContentType)?.to_string();
            if text.is_empty() || text.len() > MAX_MIME_LEN || !text.is_ascii() {
                return Err(ApiError::BadContentType);
            }
            text
        }
    };

    // Optional declared-hash cross-checks: query parameter (BUD-02 style)
    // and/or x-sha256 header ("sha256 header/body mismatch" in the spec).
    let declared_sha = match (&query.sha256, headers.get("x-sha256")) {
        (Some(q), _) => q.clone(),
        (None, Some(h)) => h
            .to_str()
            .map_err(|_| ApiError::BadRequest("bad_declared_hash"))?
            .to_string(),
        (None, None) => String::new(),
    };
    if !declared_sha.is_empty() && !hex_64_lower(&declared_sha) {
        return Err(ApiError::BadRequest("bad_declared_hash"));
    }

    // Staging dir existence is boot-time work; failing to create a stage file
    // here means an operator misconfigured mounts, not a client problem.
    //
    // The NamedTempFile stays the OWNER of the stage file for this whole
    // handler: on every early-return path its Drop deletes the partial bytes,
    // so no abort can strand debris in the staging dir.
    let stage = tempfile::NamedTempFile::new_in(state.cfg.staging_dir()).map_err(|e| {
        tracing::warn!(error = %e, "staging create failed");
        ApiError::Storage
    })?;
    let write_handle =
        tokio::fs::File::from_std(stage.as_file().try_clone().map_err(io_stage_fail)?);

    let (hasher, staged_size) =
        stage_body(body, write_handle, metrics.clone(), state.cfg.max_blob_bytes).await?;
    // Pairing: what we counted agrees with the contract we enforced and with
    // the descriptor shape we are about to persist.
    assert!(
        staged_size > 0 && staged_size <= state.cfg.max_blob_bytes,
        "staged size within declared bounds"
    );
    let staged_sha = hex::encode(hasher.finalize());
    debug_assert_eq!(staged_sha.len(), 64);

    // Explicit loop bound below: pass count == ceil(staged_size / CHUNK)+EOF.
    // The payload is a bounded list of chunks read back off the stage file:
    // object_store's CAS dialect has no streaming payload, so publish memory
    // is one transient copy of staged_size, capped by BLOBSD_MAX_BLOB_BYTES
    // and the publish semaphore (see AppState::publish_permits). Ingest stays
    // fully streamed; this is the only buffered hop and the debt ledger
    // carries it.
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
    let mut read_handle =
        tokio::fs::File::from_std(stage.as_file().try_clone().map_err(io_stage_fail)?);
    read_handle.sync_data().await.map_err(io_stage_fail)?;
    read_handle
        .seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(io_stage_fail)?;
    let sha_hex = staged_sha;

    if !declared_sha.is_empty() && declared_sha != sha_hex {
        return Err(ApiError::BadHash);
    }

    // Idempotent replay fast-path: existing descriptor wins verbatim; no
    // object rewrite, identical timestamps returned.
    if let Some(existing) = state.db.get_upload(&sha_hex).await.map_err(map_store)? {
        tracing::info!(sha = %sha_hex, pubkey_prefix = &existing.pubkey[..16.min(existing.pubkey.len())], "upload replayed");
        return Ok(descriptor_response(existing));
    }

    let mut chunks: Vec<bytes::Bytes> = Vec::with_capacity(
        usize::try_from(staged_size / CHUNK_BYTES as u64 + 1).unwrap_or(usize::MAX),
    );
    // Loop bound: exactly ceil(staged_size / CHUNK) full passes + one 0-read.
    loop {
        let mut buffer = vec![0u8; CHUNK_BYTES];
        let n = read_handle.read(&mut buffer).await.map_err(io_stage_fail)?;
        if n == 0 {
            break;
        }
        buffer.truncate(n);
        chunks.push(bytes::Bytes::from(buffer));
    }
    let payload = object_store::PutPayload::from_iter(chunks);
    let _publish_lease = state
        .publish_permits
        .acquire()
        .await
        .map_err(|_| ApiError::Storage)?;

    match state.bucket.put_create(&sha_hex, payload, &mime).await {
        Ok(PutCreate::Applied) => commit_descriptor_row(
            &state,
            UploadRow {
                pubkey: verified.pubkey_hex.clone(),
                sha256: sha_hex.clone(),
                size: i64::try_from(staged_size).unwrap_or(i64::MAX),
                mime: mime.clone(),
                uploaded_at: now_unix_secs(),
            },
        )
        .await,
        Ok(PutCreate::AlreadyExists) => reconcile_lost_race(
            &state,
            UploadRow {
                pubkey: verified.pubkey_hex.clone(),
                sha256: sha_hex.clone(),
                size: i64::try_from(staged_size).unwrap_or(i64::MAX),
                mime: mime.clone(),
                uploaded_at: now_unix_secs(),
            },
        )
        .await,
        Err(err) => {
            // Ambiguous failure: this PUT may have committed remotely. We do
            // not retry content ourselves (celld's CAS discipline); a retried
            // identical upload converges through the replay path instead.
            tracing::warn!(error = %err, sha = %sha_hex, "conditional object put failed ambiguously");
            Err(ApiError::Storage)
        }
    }
}

fn io_stage_fail(error: std::io::Error) -> ApiError {
    tracing::warn!(error = %error, "stage file operation failed");
    ApiError::Storage
}

pub(crate) fn map_store(err: crate::error::StoreError) -> ApiError {
    match err {
        crate::error::StoreError::CorruptState => {
            tracing::error!("descriptor store corrupt; failing closed");
            ApiError::CorruptState
        }
        other => {
            tracing::warn!(error = %other, "descriptor store failure");
            ApiError::Storage
        }
    }
}

async fn commit_descriptor_row(state: &AppState, row: UploadRow) -> Result<Response, ApiError> {
    state.db.insert_if_absent(row.clone()).await.map_err(map_store)?;
    let stored = state
        .db
        .get_upload(&row.sha256)
        .await
        .map_err(map_store)?
        // Row was just written or already existed (first-writer-wins race);
        // both leave it readable, so absence is an internal contradiction.
        .ok_or(ApiError::Storage)?;
    assert_eq!(stored.size, row.size, "committed size durable");
    assert_eq!(stored.sha256, row.sha256, "committed hash durable");
    tracing::info!(
        sha = %stored.sha256,
        pubkey_prefix = &stored.pubkey[..16.min(stored.pubkey.len())],
        size = stored.size,
        mime = %stored.mime,
        "upload committed"
    );
    Ok(descriptor_response(stored))
}

async fn reconcile_lost_race(state: &AppState, our_row: UploadRow) -> Result<Response, ApiError> {
    // Someone else won the identical-content race. Their descriptor lands
    // immediately after their object PUT; poll briefly, then claim an
    // orphaned object (winner crashed between PUT and INSERT) so bytes never
    // strand unowned.
    for _ in 0..CONVERGE_POLLS {
        tokio::time::sleep(std::time::Duration::from_millis(CONVERGE_SLEEP_MS)).await;
        if let Some(winner) = state.db.get_upload(&our_row.sha256).await.map_err(map_store)? {
            tracing::info!(sha = %our_row.sha256, "upload converged onto concurrent winner");
            return Ok(descriptor_response(winner));
        }
    }
    tracing::warn!(sha = %our_row.sha256, "reclaiming orphaned object commit");
    commit_descriptor_row(state, our_row).await
}

// ------------------------------------------------------------ get/head ---

fn etag_for(sha_hex: &str) -> String {
    format!("\"{sha_hex}\"")
}

fn cache_control() -> &'static str {
    // Content hashes address these objects: any byte differs => different
    // key, so immutability is structural, not conventional.
    "public, max-age=31536000, immutable"
}

pub async fn get_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sha_param): Path<String>,
) -> Result<Response, ApiError> {
    authorize_read(&state, &headers)?;
    if !hex_64_lower(&sha_param) {
        return Err(ApiError::BadSha);
    }

    let info = state
        .bucket
        .head(&sha_param)
        .await
        .map_err(bucket_to_api)?
        .ok_or(ApiError::NotFound)?;

    // Single interval resolved against authoritative size before opening a
    // stream, so HEAD-equivalent probes never race a changing object.
    let raw_range = headers.get(http::header::RANGE).and_then(|v| v.to_str().ok());
    let status;
    let byte_interval = match raw_range {
        None => {
            status = StatusCode::OK;
            None
        }
        Some(raw) => {
            let (start, end_incl) = resolve_single_range(raw, info.size)?;
            status = StatusCode::PARTIAL_CONTENT;
            Some(start..(end_incl + 1)) // half-open for object_store
        }
    };

    let result = state
        .bucket
        .get_range(&sha_param, byte_interval.clone())
        .await
        .map_err(bucket_to_api)?
        .ok_or(ApiError::NotFound)?;

    // Content-type preference: stored object attribute wins; then the
    // descriptor row (the fs backend stores no attributes, but the upload's
    // declared mime lives in the descriptor); conservative fallback last.
    let attribute_mime = result
        .attributes
        .get(&object_store::Attribute::ContentType)
        .map(|value| value.to_string())
        .or_else(|| info.content_type.clone());
    let content_type = match attribute_mime {
        Some(mime) => mime,
        None => state
            .db
            .get_upload(&sha_param)
            .await
            .ok()
            .flatten()
            .filter(|row| !row.mime.is_empty())
            .map(|row| row.mime)
            .unwrap_or_else(|| "application/octet-stream".to_string()),
    };

    let served_len = byte_interval
        .as_ref()
        .map(|interval| interval.end - interval.start)
        .unwrap_or(info.size);

    let mut builder = Response::builder()
        .status(status)
        .header(http::header::ETAG, etag_for(&sha_param))
        .header(http::header::CACHE_CONTROL, cache_control())
        .header(http::header::ACCEPT_RANGES, "bytes")
        .header(http::header::CONTENT_TYPE, content_type)
        .header(http::header::CONTENT_LENGTH, served_len);
    if let (Some(interval), true) = (
        byte_interval.as_ref(),
        status == StatusCode::PARTIAL_CONTENT,
    ) {
        builder = builder.header(
            http::header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", interval.start, interval.end - 1, info.size),
        );
    }
    if info.size == 0 {
        return builder.body(Body::empty()).map_err(into_storage);
    }

    // Stream frames off the wire; nothing accumulates. Errors past this line
    // abort the connection (client sees truncation) and surface in logs.
    let body_stream = crate::bucket::Bucket::payload_stream(result.payload).map(|frame_result| {
        frame_result
            .map(http_body::Frame::data)
            .map_err(axum::Error::new)
    });
    let body = http_body_util::StreamBody::new(body_stream);
    builder.body(Body::new(body)).map_err(into_storage)
}

fn into_storage(err: axum::http::Error) -> ApiError {
    tracing::warn!(error = %err, "response assembly failed");
    ApiError::Storage
}

fn bucket_to_api(err: crate::bucket::BucketError) -> ApiError {
    tracing::warn!(error = %err, "bucket operation failed");
    ApiError::Storage
}

// -------------------------------------------------------------- delete ---

pub async fn delete_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(sha_param): Path<String>,
) -> Result<Response, ApiError> {
    let verified = authorize(&state, &headers, Verb::Delete)?;
    if !hex_64_lower(&sha_param) {
        return Err(ApiError::BadSha);
    }

    let existed = state.db.delete_upload(&sha_param).await.map_err(map_store)?;
    if !existed {
        // Unknown hash: refuse to touch the bucket (a DELETE for someone
        // else's still-claimed object could otherwise orphan an owner row).
        return Err(ApiError::NotFound);
    }
    // Row first, then bytes: ownership dies first, so a crash mid-delete
    // degrades toward the documented re-upload-scan caveat rather than a
    // claimed-but-gone object.
    state.bucket.delete(&sha_param).await.map_err(bucket_to_api)?;
    tracing::info!(
        sha = %sha_param,
        pubkey_prefix = &verified.pubkey_hex[..16.min(verified.pubkey_hex.len())],
        "blob deleted"
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------- list ---

#[derive(Debug, Default, serde::Deserialize)]
pub struct ListQuery {
    limit: Option<u64>,
    since: Option<i64>,
}

/// `/list` derives entirely from the SQLite index (bounded page).
pub async fn list_blobs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(npub): Path<String>,
    Query(query): Query<ListQuery>,
) -> Result<Response, ApiError> {
    let verified = authorize(&state, &headers, Verb::List)?;
    let requested_hex =
        auth::npub_to_hex(&npub).map_err(|_| ApiError::BadRequest("bad_npub"))?;
    // Self-list only: an authenticated caller enumerates its own uploads,
    // nobody else's.
    if verified.pubkey_hex != requested_hex {
        return Err(ApiError::ListNotSelf);
    }

    const DEFAULT_LIST_LIMIT: u64 = 100;
    const MAX_LIST_LIMIT: u64 = 500;
    let limit = query.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if limit == 0 || limit > MAX_LIST_LIMIT {
        return Err(ApiError::LimitTooLarge);
    }
    if let Some(since) = query.since {
        if since < 0 {
            return Err(ApiError::BadRequest("bad_since"));
        }
    }

    let rows = state
        .db
        .list_uploads(
            &requested_hex,
            query.since,
            u16::try_from(limit).map_err(|_| ApiError::LimitTooLarge)?,
        )
        .await
        .map_err(map_store)?;
    // Egress check: rows came back owned by the requester, within cap.
    assert!(rows.len() <= usize::try_from(limit).unwrap_or(usize::MAX));
    assert!(
        rows.iter().all(|row| row.pubkey == requested_hex),
        "listing leaked foreign ownership"
    );

    let descriptors: Vec<BlobDescriptor> = rows.iter().map(BlobDescriptor::from_row).collect();
    Ok(Json(descriptors).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_parser_accepts_single_intervals_and_clamps() {
        // Whole-object ranges of various shapes.
        assert_eq!(resolve_single_range("bytes=0-4", 10).unwrap(), (0, 4));
        assert_eq!(resolve_single_range("bytes=3-", 10).unwrap(), (3, 9));
        assert_eq!(resolve_single_range("bytes=-4", 10).unwrap(), (6, 9));
        // End past the object clamps to the last byte.
        assert_eq!(resolve_single_range("bytes=8-99", 10).unwrap(), (8, 9));
        // Suffix longer than the object yields the whole object (RFC 9110).
        assert_eq!(resolve_single_range("bytes=-50", 10).unwrap(), (0, 9));
        // Unit is case-insensitive.
        assert_eq!(resolve_single_range("BYTES=0-0", 10).unwrap(), (0, 0));
    }

    #[test]
    fn range_parser_rejects_multi_garbage_and_unsatisfiable() {
        assert_eq!(
            resolve_single_range("bytes=0-1,5-6", 10).unwrap_err(),
            ApiError::MultiRange
        );
        assert_eq!(
            resolve_single_range("bytes=abc", 10).unwrap_err(),
            ApiError::BadRange
        );
        assert_eq!(
            resolve_single_range("bytes=", 10).unwrap_err(),
            ApiError::BadRange
        );
        assert_eq!(
            resolve_single_range("chunks=0-1", 10).unwrap_err(),
            ApiError::BadRange
        );
        // Start at/after size, empty suffix, and zero-length suffix all 416.
        assert_eq!(
            resolve_single_range("bytes=10-", 10).unwrap_err(),
            ApiError::RangeNotSatisfiable(10)
        );
        assert_eq!(
            resolve_single_range("bytes=-0", 10).unwrap_err(),
            ApiError::RangeNotSatisfiable(10)
        );
        assert_eq!(
            resolve_single_range("bytes=0-", 0).unwrap_err(),
            ApiError::RangeNotSatisfiable(0)
        );
    }

    #[test]
    fn sha_validation_is_exact() {
        let ok = "a".repeat(64);
        assert!(hex_64_lower(&ok));
        assert!(!hex_64_lower(&"A".repeat(64)), "uppercase refused");
        assert!(!hex_64_lower(&ok[..63]), "short refused");
        assert!(!hex_64_lower(&format!("{ok}g")), "non-hex refused");
    }

    #[test]
    fn bearer_compare_is_exact_for_equal_strings_and_refuses_others() {
        assert!(token_matches("token-abcdef0123456789", "token-abcdef0123456789"));
        assert!(!token_matches("token-abcdef012345678", "token-abcdef0123456789"));
        assert!(!token_matches("token-abcdef012345678X", "token-abcdef0123456789"));
        assert!(!token_matches("", "token-abcdef0123456789"));
    }
}
