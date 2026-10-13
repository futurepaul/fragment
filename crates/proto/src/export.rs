//! A person's export of everything that is theirs, on the wire (docs/api.md,
//! Export: `POST|GET /api/export`, `GET /api/export/{id}/download`). The
//! archive's layout, its limits and its build are `fragment_core::export`'s.

use serde::{Deserialize, Serialize};

/// Where an export is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportState {
    /// Its archive is being written, in the background.
    Building,
    /// Its archive is whole, and downloads until `expiresAt`.
    Ready,
    /// It stopped, saying why (`error`); nothing is kept.
    Failed,
    /// Its archive was deleted, its days up (or another export replaced it).
    Expired,
}

/// Why an export failed: a limit it would pass, a part of it that kept
/// failing, something it was reading that changed under it, or its time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportFailure {
    /// The archive, or one fragment's part of it, would pass its size.
    TooLarge,
    /// More fragments, files, blobs, tables or entries than it takes.
    TooMany,
    /// A part kept failing for a reason that may pass (an outage): export again.
    Unavailable,
    /// A computer's save or a blob it was copying went away as it read it
    /// (the computer saved three times since): export again.
    SourceChanged,
    /// It did not finish within its time.
    TimedOut,
}

/// An export's failure, as its view says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportError {
    pub code: ExportFailure,
    pub message: String,
}

/// How far a build got: `done` of `total` parts (its README and manifest,
/// each fragment, each computer), and the archive's bytes so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportProgress {
    pub done: u32,
    pub total: u32,
    pub bytes: u64,
}

/// One export, as its person sees it (`POST /api/export`'s answer, and
/// `GET /api/export`'s `export`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportView {
    /// 16 hex.
    pub id: String,
    pub state: ExportState,
    pub started_at: i64,
    pub progress: ExportProgress,
    /// The archive's size, once it is whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_at: Option<i64>,
    /// When its archive is deleted (seven days after it was ready).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ExportError>,
    /// Where it downloads, while it is ready: a path on the platform's
    /// origin, for its person alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<String>,
}

/// `GET /api/export`: the person's export (their newest), or none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportStatus {
    pub export: Option<ExportView>,
}

/// The path an export downloads at (`ExportView::download`).
pub fn download_path(id: &str) -> String {
    format!("/api/export/{id}/download")
}

/// An export's id: 16 lowercase hex.
pub fn valid_export_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: the view says what the shell and the CLI read, camelCase, and
    /// leaves out what an export does not have yet.
    #[test]
    fn view_shape() {
        let v = ExportView {
            id: "0123456789abcdef".into(),
            state: ExportState::Building,
            started_at: 5,
            progress: ExportProgress { done: 1, total: 4, bytes: 2048 },
            size: None,
            ready_at: None,
            expires_at: None,
            error: None,
            download: None,
        };
        let j = serde_json::to_value(&v).unwrap();
        assert_eq!(j, serde_json::json!({ "id": "0123456789abcdef", "state": "building", "startedAt": 5, "progress": { "done": 1, "total": 4, "bytes": 2048 } }));
        let failed = ExportView { state: ExportState::Failed, error: Some(ExportError { code: ExportFailure::TooLarge, message: "m".into() }), ..v };
        assert_eq!(serde_json::to_value(&failed).unwrap()["error"], serde_json::json!({ "code": "too_large", "message": "m" }));
        let back: ExportView = serde_json::from_value(serde_json::to_value(&failed).unwrap()).unwrap();
        assert_eq!(back, failed);
    }

    /// Goal: an id is the 16 hex the platform makes, nothing else.
    #[test]
    fn ids() {
        assert!(valid_export_id("0123456789abcdef"));
        for bad in ["", "0123456789abcde", "0123456789ABCDEF", "0123456789abcdeg", "0123456789abcdef0", "../../etc/passwd"] {
            assert!(!valid_export_id(bad), "{bad:?}");
        }
        assert_eq!(download_path("0123456789abcdef"), "/api/export/0123456789abcdef/download");
    }
}
