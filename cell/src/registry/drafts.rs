//! Drafts' starts (docs/api.md, Drafts): the one fleet-wide count a draft
//! needs. A key no one holds starts a draft (one at a time: its name is
//! its key's), and an address and the deployment start at most
//! `limits::DRAFTS_PER_ADDRESS_PER_DAY` and `limits::DRAFTS_PER_DAY` in a
//! day; a key's start again within its day (a create sent again, or its
//! draft made again) counts once. A start is kept a day, then swept with
//! sign-in's rows (signin.rs), so the table holds at most a day's cap.

use fragment_core::drafts;

use super::calls::StartDraft;
use super::*;

/// How long a start counts.
const DAY_MS: i64 = 24 * 3600 * 1000;

pub(super) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS draft_starts (key TEXT PRIMARY KEY, address TEXT NOT NULL, at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS draft_starts_address ON draft_starts (address, at);
CREATE INDEX IF NOT EXISTS draft_starts_at ON draft_starts (at);
";

/// An address as the router names it (`drafts::address`), or its bucket
/// for a request that named none.
const ADDRESS_MAX_BYTES: usize = 64;

impl RegistryCell {
    /// A key no one holds starts a draft from an address, under the day's
    /// caps; its own start again within the day passes as it is.
    pub(super) async fn start_draft(&self, b: StartDraft) -> CellResult<()> {
        check_key(&b.key)?;
        if b.address.is_empty() || b.address.len() > ADDRESS_MAX_BYTES {
            return Err(CellError::invalid("a draft's start names its address"));
        }
        match self.key_row(&b.key)? {
            Some(row) if row.active() => {
                return Err(conflict(format!(
                    "the key {} is someone's: it makes fragments of theirs (`fragment create <label>`), not drafts",
                    npub::encode(&b.key)
                )))
            }
            Some(_) => return Err(conflict("this key was revoked: make a new one")),
            None => {}
        }
        let now = js::now_ms();
        let since = SqlStorageValue::Integer(now - DAY_MS);
        if self.count("SELECT COUNT(*) AS n FROM draft_starts WHERE key = ? AND at > ?", vec![b.key.as_str().into(), since.clone()])? > 0 {
            return Ok(());
        }
        let by_address = self.count("SELECT COUNT(*) AS n FROM draft_starts WHERE address = ? AND at > ?", vec![b.address.as_str().into(), since.clone()])?;
        let in_all = self.count("SELECT COUNT(*) AS n FROM draft_starts WHERE at > ?", vec![since])?;
        if let Some(why) = drafts::start_refused(by_address, in_all) {
            return Err(CellError::new(ErrorCode::RateLimited, why));
        }
        // armed before the row is written, so no row goes unswept
        self.sweep_by(now + DAY_MS).await?;
        self.exec(
            "INSERT INTO draft_starts (key, address, at) VALUES (?, ?, ?) ON CONFLICT (key) DO UPDATE SET address = excluded.address, at = excluded.at",
            vec![b.key.as_str().into(), b.address.as_str().into(), SqlStorageValue::Integer(now)],
        )?;
        Ok(())
    }

    /// Deletes a batch of starts older than a day: how many went.
    pub(super) fn sweep_drafts(&self, now: i64, batch: i64) -> CellResult<usize> {
        let gone = self.rows::<serde::de::IgnoredAny>(
            "DELETE FROM draft_starts WHERE key IN (SELECT key FROM draft_starts WHERE at <= ? LIMIT ?) RETURNING key",
            vec![SqlStorageValue::Integer(now - DAY_MS), SqlStorageValue::Integer(batch)],
        )?;
        Ok(gone.len())
    }

    /// When the oldest start stops counting (`None`: there is none).
    pub(super) fn drafts_due(&self) -> CellResult<Option<i64>> {
        #[derive(Deserialize)]
        struct Row {
            at: Option<i64>,
        }
        let row = self.row::<Row>("SELECT MIN(at) AS at FROM draft_starts", vec![])?;
        Ok(row.and_then(|r| r.at).map(|at| at + DAY_MS))
    }
}
