//! Typed errors for blobsd. No `anyhow` crosses this crate's boundary:
//! callers (handlers, main) match on the variants below instead of parsing
//! strings, and client-facing responses render exactly one machine-readable
//! `code` plus the HTTP status.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// A request was rejected. Every variant carries a stable wire code rendered
/// as `{"error": "<code>"}`; the status is part of the contract per variant.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    // ---- 401: the caller failed to authenticate at all -------------------
    #[error("missing or malformed Authorization header")]
    MissingAuth,
    #[error("auth event is not decodable/parseable")]
    MalformedAuth,
    #[error("auth event id does not hash its contents")]
    BadAuthId,
    #[error("schnorr signature invalid")]
    BadAuthSignature,
    #[error("auth event expired")]
    AuthExpired,
    #[error("not a kind-24242 auth event")]
    WrongAuthKind,
    #[error("bearer token required for data-plane reads on this instance")]
    ReadTokenRequired,
    #[error("bearer token mismatch")]
    BadReadToken,

    // ---- 403: authenticated but not permitted ----------------------------
    #[error("auth event t tag does not permit this verb")]
    ForbiddenVerb,
    #[error("auth event x tag does not match this server URL")]
    ForbiddenUrl,
    #[error("pubkey not allowlisted")]
    ForbiddenNpub,
    #[error("/list is self-list only")]
    ListNotSelf,

    // ---- 400 / 404 / 413 / 416 ------------------------------------------
    #[error("path sha256 must be 64 lowercase hex chars")]
    BadSha,
    #[error("declared sha256 does not match body bytes")]
    BadHash,
    #[error("unsatisfiable single-range request only; multi-interval refused")]
    MultiRange,
    #[error("Range header unparseable")]
    BadRange,
    /// Carries the authoritative object size for the RFC-required
    /// `Content-Range: bytes */<size>` answer.
    #[error("requested range start beyond object size ({0})")]
    RangeNotSatisfiable(u64),
    #[error("missing Content-Length")]
    MissingLength,
    #[error("body exceeds BLOBSD_MAX_BLOB_BYTES ({0})")]
    TooLarge(u64),
    #[error("content-type header unusable")]
    BadContentType,
    #[error("limit query parameter exceeds 500")]
    LimitTooLarge,
    #[error("query parameter unusable: {0}")]
    BadRequest(&'static str),
    #[error("object not found")]
    NotFound,

    // ---- 5xx --------------------------------------------------------------
    /// SQLite reported corruption (boot quick_check or DatabaseCorrupt code).
    /// Fail closed: never retry around it, surface it.
    #[error("descriptor store is corrupt")]
    CorruptState,
    #[error("internal storage error")]
    Storage,
}

impl ApiError {
    /// HTTP status and machine-readable wire code for each variant.
    fn status_and_code(&self) -> (StatusCode, &'static str) {
        match self {
            ApiError::MissingAuth => (StatusCode::UNAUTHORIZED, "invalid_auth"),
            ApiError::MalformedAuth => (StatusCode::UNAUTHORIZED, "malformed_event"),
            ApiError::BadAuthId => (StatusCode::UNAUTHORIZED, "bad_id"),
            ApiError::BadAuthSignature => (StatusCode::UNAUTHORIZED, "bad_signature"),
            ApiError::AuthExpired => (StatusCode::UNAUTHORIZED, "expired"),
            ApiError::WrongAuthKind => (StatusCode::UNAUTHORIZED, "wrong_kind"),
            ApiError::ReadTokenRequired => (StatusCode::UNAUTHORIZED, "read_token_required"),
            ApiError::BadReadToken => (StatusCode::UNAUTHORIZED, "bad_token"),
            ApiError::ForbiddenVerb => (StatusCode::FORBIDDEN, "forbidden_t"),
            ApiError::ForbiddenUrl => (StatusCode::FORBIDDEN, "forbidden_url"),
            ApiError::ForbiddenNpub => (StatusCode::FORBIDDEN, "forbidden_npub"),
            ApiError::ListNotSelf => (StatusCode::FORBIDDEN, "list_not_self"),
            ApiError::BadSha => (StatusCode::BAD_REQUEST, "bad_sha"),
            ApiError::BadHash => (StatusCode::BAD_REQUEST, "bad_hash"),
            ApiError::MultiRange => (StatusCode::BAD_REQUEST, "multi_range"),
            ApiError::BadRange => (StatusCode::BAD_REQUEST, "bad_range"),
            ApiError::RangeNotSatisfiable(_) => {
                (StatusCode::RANGE_NOT_SATISFIABLE, "range_not_satisfiable")
            }
            ApiError::MissingLength => (StatusCode::BAD_REQUEST, "length_required"),
            ApiError::TooLarge(_) => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
            ApiError::BadContentType => (StatusCode::BAD_REQUEST, "bad_content_type"),
            ApiError::LimitTooLarge => (StatusCode::BAD_REQUEST, "bad_limit"),
            ApiError::BadRequest(code) => (StatusCode::BAD_REQUEST, code),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
            ApiError::CorruptState => (StatusCode::INTERNAL_SERVER_ERROR, "corrupt_state"),
            ApiError::Storage => (StatusCode::INTERNAL_SERVER_ERROR, "storage_error"),
        }
    }

}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.status_and_code();
        let mut response = (status, axum::Json(json!({ "error": code }))).into_response();
        if let ApiError::RangeNotSatisfiable(size) = &self {
            // RFC 9110: an unsatisfiable range answer names the real size so
            // the client can resync without probing.
            if let Ok(value) = http::HeaderValue::from_str(&format!("bytes */{size}")) {
                response.headers_mut().insert(http::header::CONTENT_RANGE, value);
            }
        }
        response
    }
}

/// Boot-stage failures. These print once to stderr with a human reason and
/// exit non-zero; they are not request-path errors.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum BootError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("data dir: {0}")]
    DataDir(String),
    #[error("descriptor store corrupt (quick_check failed): {0}")]
    CorruptState(String),
    #[error("database: {0}")]
    Db(String),
    #[error("bucket: {0}")]
    Bucket(String),
}

/// Store-layer failure taxonomy shared by db.rs callers. rusqlite errors stay
/// typed inside [`StoreError::Sqlite`]; corruption gets its own fail-closed
/// variant so no caller can accidentally treat it as a transient miss.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("corrupt descriptor store")]
    CorruptState,
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

impl StoreError {
    /// Classify a raw rusqlite failure. `DatabaseCorrupt` maps to
    /// [`StoreError::CorruptState`] — that code means page-level corruption,
    /// and style says such state must be surfaced, not retried or masked.
    pub fn classify(err: rusqlite::Error) -> StoreError {
        if matches!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseCorrupt)
                | Some(rusqlite::ErrorCode::NotADatabase)
        ) {
            // "Not a database" IS corrupt state from this store's point of
            // view: the file cannot be trusted, so fail closed.
            StoreError::CorruptState
        } else {
            StoreError::Sqlite(err)
        }
    }
}
