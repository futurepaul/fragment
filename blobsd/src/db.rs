//! Descriptor store: one embedded SQLite database (`data/blobsd.db`) holding
//! upload ownership rows exactly as specced:
//!
//! `uploads(pubkey TEXT NOT NULL, sha256 TEXT PRIMARY KEY, size INTEGER NOT
//! NULL, mime TEXT NOT NULL DEFAULT '', uploaded_at INTEGER NOT NULL)`
//!
//! Foreign-key-free and WAL-mode. Bytes live in the bucket; this database is
//! only the ownership/list index, so losing it degrades /list, never reads
//! (documented debt-ledger caveat). Corruption fails closed through
//! [`StoreError::CorruptState`]; nothing retries around corrupt pages.

use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::error::StoreError;

/// One uploads row. `pubkey`/`sha256` are lowercase hex strings, `mime` the
/// stored content-type (may be empty), `uploaded_at` unix seconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRow {
    pub pubkey: String,
    pub sha256: String,
    pub size: i64,
    pub mime: String,
    pub uploaded_at: i64,
}

/// Handle shared across tasks. The connection lives behind a std Mutex and
/// every query runs on a blocking thread: SQLite calls are short but they are
/// still blocking syscalls, and tokio worker threads must not wait on them.
#[derive(Clone)]
pub struct Db {
    inner: Arc<DbInner>,
}

struct DbInner {
    conn: Mutex<Connection>,
}

impl std::fmt::Debug for Db {
    /// Debug prints the type only: the connection inside is notinspectable
    /// and its contents must never end up in logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Db")
    }
}

impl Db {
    /// Open (creating if needed) the database at `path`, force WAL mode, and
    /// run a bounded `quick_check` before serving anything.
    pub fn open(path: &str) -> Result<Db, StoreError> {
        let conn = Connection::open(path).map_err(StoreError::classify)?;
        // Durability-first WAL setup: NORMAL checkpoint fsyncs are skipped in
        // return for throughput, but descriptor commits are tiny and rare
        // next to object traffic, so FULL costs nothing observable here.
        let journal: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(StoreError::classify)?;
        if journal != "wal" {
            // WAL was specced for this store; anything else means we are not
            // looking at the storage shape we think we are.
            return Err(StoreError::CorruptState);
        }
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(StoreError::classify)?;
        conn.busy_timeout(std::time::Duration::from_millis(5_000))
            .map_err(StoreError::classify)?;

        // Boot-time integrity gate. quick_check(1) stops at the first error,
        // which bounds the scan cost while still answering "is this file
        // trustworthy enough to write into" — fail closed on anything but ok.
        let check: String = conn
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
            .map_err(StoreError::classify)?;
        if check != "ok" {
            return Err(StoreError::CorruptState);
        }

        // Fresh stores get the table; existing stores are validated to still
        // have the exact specced columns (a silently drifted schema would
        // otherwise surface as runtime insert errors far from the cause).
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS uploads(
                pubkey TEXT NOT NULL,
                sha256 TEXT PRIMARY KEY,
                size INTEGER NOT NULL,
                mime TEXT NOT NULL DEFAULT '',
                uploaded_at INTEGER NOT NULL
            );",
        )
        .map_err(StoreError::classify)?;
        {
            let cols: Vec<String> = {
                let mut stmt = conn
                    .prepare("PRAGMA table_info(uploads)")
                    .map_err(StoreError::classify)?;
                let names = stmt
                    .query_map([], |row| row.get::<_, String>(1))
                    .map_err(StoreError::classify)?;
                names.collect::<Result<Vec<_>, _>>().map_err(StoreError::classify)?
            };
            let expected = ["pubkey", "sha256", "size", "mime", "uploaded_at"];
            if cols != expected {
                return Err(StoreError::CorruptState);
            }
        }

        Ok(Db {
            inner: Arc::new(DbInner {
                conn: Mutex::new(conn),
            }),
        })
    }

    /// Fetch one descriptor row by hash.
    pub async fn get_upload(&self, sha256: &str) -> Result<Option<UploadRow>, StoreError> {
        assert_sha_hex(sha256);
        let inner = self.inner.clone();
        let sha = sha256.to_string();
        spawn_store(move || {
            let conn = inner.lock_conn()?;
            let row = conn
                .query_row(
                    "SELECT pubkey, sha256, size, mime, uploaded_at
                     FROM uploads WHERE sha256 = ?1",
                    [&sha],
                    |row| {
                        Ok(UploadRow {
                            pubkey: row.get(0)?,
                            sha256: row.get(1)?,
                            size: row.get(2)?,
                            mime: row.get(3)?,
                            uploaded_at: row.get(4)?,
                        })
                    },
                )
                .map_err(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => None,
                    other => Some(StoreError::classify(other)),
                });
            match row {
                Ok(row) => Ok(Some(row)),
                Err(None) => Ok(None),
                Err(Some(err)) => Err(err),
            }
        })
        .await
    }

    /// Insert the ownership row unless the hash already exists. Returns true
    /// when THIS call inserted (first writer wins), false on idempotent hit.
    pub async fn insert_if_absent(&self, row: UploadRow) -> Result<bool, StoreError> {
        assert_sha_hex(&row.sha256);
        assert!(
            !row.pubkey.is_empty() && row.size >= 0 && row.uploaded_at > 0,
            "insert_if_absent precondition: sane row fields"
        );
        let inner = self.inner.clone();
        spawn_store(move || {
            let conn = inner.lock_conn()?;
            let inserted = conn
                .execute(
                    "INSERT OR IGNORE INTO uploads(pubkey, sha256, size, mime, uploaded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![row.pubkey, row.sha256, row.size, row.mime, row.uploaded_at],
                )
                .map_err(StoreError::classify)?;
            debug_assert!(inserted <= 1, "primary-key insert changed at most one row");
            Ok(inserted == 1)
        })
        .await
    }

    /// Delete one row; answers whether it existed.
    pub async fn delete_upload(&self, sha256: &str) -> Result<bool, StoreError> {
        assert_sha_hex(sha256);
        let inner = self.inner.clone();
        let sha = sha256.to_string();
        spawn_store(move || {
            let conn = inner.lock_conn()?;
            let deleted = conn
                .execute("DELETE FROM uploads WHERE sha256 = ?1", [&sha])
                .map_err(StoreError::classify)?;
            debug_assert!(deleted <= 1, "primary-key delete removed at most one row");
            Ok(deleted == 1)
        })
        .await
    }

    /// List rows owned by `pubkey` (hex), newest first, bounded by `limit`.
    /// `since`, when present, keeps rows with uploaded_at >= since.
    ///
    /// No unbounded loop exists here on purpose: LIMIT is bound-checked to
    /// 500 at the handler boundary, so the returned Vec cannot grow with the
    /// table.
    pub async fn list_uploads(
        &self,
        pubkey: &str,
        since: Option<i64>,
        limit: u16,
    ) -> Result<Vec<UploadRow>, StoreError> {
        const MAX_LIST_LIMIT: u16 = 500;
        assert!((1..=MAX_LIST_LIMIT).contains(&limit), "list limit pre-clamped");
        assert_sha_hex(pubkey);
        let inner = self.inner.clone();
        let owner = pubkey.to_string();
        spawn_store(move || {
            let conn = inner.lock_conn()?;
            let mut stmt = conn
                .prepare(
                    "SELECT pubkey, sha256, size, mime, uploaded_at
                     FROM uploads
                     WHERE pubkey = ?1 AND (?2 IS NULL OR uploaded_at >= ?2)
                     ORDER BY uploaded_at DESC, sha256 DESC
                     LIMIT ?3",
                )
                .map_err(StoreError::classify)?;
            let rows = stmt
                .query_map(rusqlite::params![owner, since, i64::from(limit)], map_row)
                .map_err(StoreError::classify)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StoreError::classify)?;
            debug_assert!(
                rows.len() <= usize::from(limit),
                "SQLite LIMIT honored"
            );
            Ok(rows)
        })
        .await
    }
}

impl DbInner {
    fn lock_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| {
            // A poisoned mutex means a previous query panicked mid-transaction;
            // that is internal contradiction territory -> fail closed rather
            // than continue against possibly half-written state.
            tracing::warn!("descriptor store mutex poisoned; refusing further queries");
            StoreError::CorruptState
        })
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<UploadRow> {
    Ok(UploadRow {
        pubkey: row.get(0)?,
        sha256: row.get(1)?,
        size: row.get(2)?,
        mime: row.get(3)?,
        uploaded_at: row.get(4)?,
    })
}

async fn spawn_store<T, F>(work: F) -> Result<T, StoreError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|join_err| {
            if join_err.is_panic() {
                // Query code itself has no panic paths except failed internal
                // assertions; treat either as unrecoverable state doubt.
                StoreError::CorruptState
            } else {
                StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    std::io::Error::other("store task cancelled"),
                )))
            }
        })?
}

fn assert_sha_hex(value: &str) {
    assert!(
        value.len() == 64
            && value.bytes().all(|b| b.is_ascii_digit()
                || (b.is_ascii_lowercase() && b.is_ascii_hexdigit())),
        "store keys must be 64-char lowercase hex"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sha(n: u8) -> String {
        hex::encode([n; 32])
    }

    fn row(sha_hex: String, size: i64, uploaded_at: i64) -> UploadRow {
        UploadRow {
            pubkey: "aa11".repeat(16),
            sha256: sha_hex,
            size,
            mime: "text/plain".to_string(),
            uploaded_at,
        }
    }

    fn sha_i(i: usize) -> String {
        format!("{:064x}", i)
    }

    #[tokio::test]
    async fn insert_get_delete_roundtrip_with_replay_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&format!("{}/blobsd.db", dir.path().display())).unwrap();

        // First insert wins...
        assert!(db.insert_if_absent(row(sha(1), 5, 1_700_000_000)).await.unwrap());
        // ...replay is a no-op...
        assert!(!db.insert_if_absent(row(sha(1), 5, 1_700_000_999)).await.unwrap());
        // ...and read-back shows the FIRST row, not the retry's fields.
        let stored = db.get_upload(&sha(1)).await.unwrap().unwrap();
        assert_eq!(stored.size, 5);
        assert_eq!(stored.uploaded_at, 1_700_000_000);

        assert!(db.delete_upload(&sha(1)).await.unwrap());
        assert!(!db.delete_upload(&sha(1)).await.unwrap(), "second delete misses");
        assert!(db.get_upload(&sha(1)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn list_orders_desc_and_respects_since_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&format!("{}/blobsd.db", dir.path().display())).unwrap();

        for (n, ts) in [(2u8, 1_700_000_002i64), (3, 1_700_000_003), (4, 1_700_000_004)] {
            db.insert_if_absent(row(sha(n), 1, ts)).await.unwrap();
        }
        // Bounded page + newest-first ordering.
        let page = db.list_uploads("aa11".repeat(16).as_str(), None, 2).await.unwrap();
        assert_eq!(page.len(), 2, "limit caps the page");
        assert_eq!(page[0].uploaded_at, 1_700_000_004, "newest first");
        let all = db.list_uploads("aa11".repeat(16).as_str(), Some(1_700_000_002), 500).await.unwrap();
        assert_eq!(all.len(), 3, "since at the oldest kept row keeps everything (>=)");
        let two = db.list_uploads("aa11".repeat(16).as_str(), Some(1_700_000_003), 500).await.unwrap();
        assert_eq!(two.len(), 2, "since excludes strictly older rows");
        let none = db.list_uploads("aa11".repeat(16).as_str(), Some(1_800_000_000), 500).await.unwrap();
        assert!(none.is_empty(), "future since excludes everything");

        // Restart on the same file: WAL-persisted rows survive the handle.
        drop(db);
        let db2 = Db::open(&format!("{}/blobsd.db", dir.path().display())).unwrap();
        assert_eq!(
            db2.list_uploads("aa11".repeat(16).as_str(), None, 500).await.unwrap().len(),
            3,
            "rows survive restart"
        );
    }

    #[tokio::test]
    async fn junk_file_fails_closed_as_corrupt_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = format!("{}/blobsd.db", dir.path().display());
        std::fs::write(&path, b"definitely not sqlite format at all").unwrap();
        match Db::open(&path) {
            Err(StoreError::CorruptState) => {}
            other => panic!("expected CorruptState, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn flipped_page_fails_quick_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = format!("{}/blobsd.db", dir.path().display());
        {
            let db = Db::open(&path).unwrap();
            // Enough rows to span several btree pages.
            for i in 0..300usize {
                db.insert_if_absent(row(sha_i(i), 1, 1)).await.unwrap();
            }
        }
        // Keep the magic header (first 100 bytes) intact but trash the first
        // table page: the header probe passes, the btree scan must not.
        let mut bytes = std::fs::read(&path).unwrap();
        assert!(bytes.len() > 8192, "fixture spans multiple pages");
        bytes[2048..4096].fill(0xFF);
        std::fs::write(&path, &bytes).unwrap();
        match Db::open(&path) {
            Err(StoreError::CorruptState) => {}
            other => panic!("expected CorruptState, got {other:?}"),
        }
    }
}
