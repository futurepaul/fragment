//! The wire contract between the fragment cell, the CLI, and the e2e.
//!
//! Every request body, response body, error code, and limit that crosses
//! the HTTP boundary is defined here once. The cell compiles this crate to
//! wasm32, so it holds data and pure functions only.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Limits every side enforces the same way (docs/MODEL.md, Limits).
pub mod limits {
    /// A fragment name: `^[a-z0-9][a-z0-9-]{0,62}$`.
    pub const NAME_MAX_BYTES: usize = 63;
    /// An operation id: `^[A-Za-z0-9._:-]{1,128}$`.
    pub const OP_ID_MAX_BYTES: usize = 128;
    /// An operation name: `^[a-z][a-z0-9_]{0,63}$`.
    pub const OP_NAME_MAX_BYTES: usize = 64;
    /// Operation input is a request, not an upload.
    pub const INPUT_MAX_BYTES: usize = 256 * 1024;
    /// Operation results must fit a Workflows step result.
    pub const RESULT_MAX_BYTES: usize = 1024 * 1024;
    /// The app's source (`app.mjs`) while code arrives by `PUT code`.
    pub const SOURCE_MAX_BYTES: usize = 1024 * 1024;
    /// Declared operations per fragment.
    pub const OPERATIONS_MAX: usize = 256;
    /// CPU per call into author code.
    pub const APP_CPU_MS: u32 = 30_000;
    /// Subrequests per call into author code.
    pub const APP_SUBREQUESTS: u32 = 50;
    /// A NIP-98 event's `created_at` may differ from the cell's clock by this much.
    pub const AUTH_WINDOW_S: i64 = 60;
    /// Request bodies the router reads (to hash for NIP-98).
    pub const BODY_MAX_BYTES: usize = 2 * 1024 * 1024;
}

pub fn valid_fragment_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= limits::NAME_MAX_BYTES
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

pub fn valid_op_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= limits::OP_ID_MAX_BYTES
        && id.bytes().all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
}

pub fn valid_op_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= limits::OP_NAME_MAX_BYTES
        && b[0].is_ascii_lowercase()
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Every refusal the platform answers with, as `{"error": code, "message"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 400: the request is malformed or breaks a rule.
    InvalidRequest,
    /// 401: no valid NIP-98 signature.
    Unauthenticated,
    /// 403: signed, but this principal may not do this.
    Forbidden,
    /// 404: no such fragment or route.
    NotFound,
    /// 404: the fragment has no operation by that name.
    UnknownOperation,
    /// 404: the fragment has no app code installed.
    NoCode,
    /// 409: the fragment already exists.
    AlreadyExists,
    /// 409: this operation id was used with a different input.
    ConflictingBody,
    /// 413: a body, input, or result is over its limit.
    TooLarge,
    /// 422: the author's code refused or failed.
    AppFailed,
    /// 500: the platform failed.
    HostFailed,
}

impl ErrorCode {
    pub fn status(self) -> u16 {
        match self {
            ErrorCode::InvalidRequest => 400,
            ErrorCode::Unauthenticated => 401,
            ErrorCode::Forbidden => 403,
            ErrorCode::NotFound | ErrorCode::UnknownOperation | ErrorCode::NoCode => 404,
            ErrorCode::AlreadyExists | ErrorCode::ConflictingBody => 409,
            ErrorCode::TooLarge => 413,
            ErrorCode::AppFailed => 422,
            ErrorCode::HostFailed => 500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorCode,
    pub message: String,
}

/// `POST /api/fragments`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateFragment {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FragmentStatus {
    pub name: String,
    /// The owner's public key, hex.
    pub owner: String,
    /// The installed code's sha, if any.
    pub code_sha: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpKind {
    Query,
    Mutation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpDecl {
    pub kind: OpKind,
}

/// `PUT /api/f/<name>/code`. Slice A only: from slice B the code comes
/// from the fragment's `live` pin in git (docs/technical-debt-ledger.md).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallCode {
    pub sha: String,
    pub source: String,
    pub operations: std::collections::BTreeMap<String, OpDecl>,
    /// Lower than `limits::APP_CPU_MS` for tests; never higher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_ms: Option<u32>,
}

/// `POST /api/f/<name>/ops/<op>`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpCall {
    pub id: String,
    #[serde(default)]
    pub input: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpResult {
    pub result: Value,
    /// True when this operation id already ran and the stored result is returned.
    pub replayed: bool,
}

/// The canonical form of an operation input: object keys sorted at every
/// level, so equal inputs serialize equally. Explicit rather than relying on
/// serde_json's map order, which a `preserve_order` feature anywhere in the
/// dependency graph would change.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical(&map[k.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// The declared operations as a JSON object (what the facet's platform code reads).
pub fn operations_json(ops: &std::collections::BTreeMap<String, OpDecl>) -> Map<String, Value> {
    ops.iter()
        .map(|(k, d)| (k.clone(), serde_json::to_value(d).expect("OpDecl serializes")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(valid_fragment_name("todo-1"));
        assert!(!valid_fragment_name("-todo"));
        assert!(!valid_fragment_name("Todo"));
        assert!(!valid_fragment_name(&"a".repeat(64)));
        assert!(valid_op_name("add_todo"));
        assert!(!valid_op_name("__mutate"));
        assert!(!valid_op_name("Add"));
        assert!(valid_op_id("turn-1:tool-0"));
        assert!(!valid_op_id("bad id"));
        assert!(!valid_op_id(""));
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let a: Value = serde_json::from_str(r#"{"b":1,"a":{"d":2,"c":3}}"#).unwrap();
        let b: Value = serde_json::from_str(r#"{"a":{"c":3,"d":2},"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":{"c":3,"d":2},"b":1}"#);
    }

    #[test]
    fn error_codes_serialize_snake_case() {
        let body = ErrorBody { error: ErrorCode::ConflictingBody, message: "x".into() };
        assert_eq!(serde_json::to_string(&body).unwrap(), r#"{"error":"conflicting_body","message":"x"}"#);
        assert_eq!(ErrorCode::ConflictingBody.status(), 409);
    }
}
