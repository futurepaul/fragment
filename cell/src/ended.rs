//! A deleted fragment's end (docs/api.md, `DELETE /api/f/{name}`). The
//! delete ends the life in one synchronous step and answers once one round
//! of its members' lists has been told (`INDEX_FLUSH_MAX`, its owner's
//! first). What it leaves (the rest of those lists, the app facet's
//! database, the blobs) the alarm cleans after, each part idempotent and
//! retried with a backoff, so a delete answers as soon at `MEMBERS_MAX`
//! members as at one. Telling 1000 lists in turn before answering took
//! 300 s on the e2e preview (2026-10-06), and the delete then wiped the
//! outbox that held any it failed to tell.
//!
//! The ended life's rows live in tables of their own (`SCHEMA`, here),
//! which the delete keeps while it drops the life's own and makes them
//! again empty: the name is free at once, and a life made again meanwhile
//! is untouched by the old one's cleanup (each list's change names its
//! incarnation, and a list keeps the newer life's row). Once nothing is
//! left to clean, with no life or claim, the object's storage goes whole
//! (`deleteAll`).

use fragment_core::backoff::outbox_retry_ms;
use futures_util::future::join_all;
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{missing, FragmentCell, MetaKey, CLAIM_TTL_MS};
use crate::js;
use crate::members::INDEX_FLUSH_MAX;

/// The ended lives (one row each, until all is cleaned) and the lists each
/// has still to tell.
pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS ended (
  incarnation INTEGER PRIMARY KEY, name TEXT NOT NULL, npub TEXT NOT NULL, facet TEXT NOT NULL, ended_at INTEGER NOT NULL,
  stored INTEGER NOT NULL DEFAULT 1, attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ended_index (
  incarnation INTEGER NOT NULL, principal TEXT NOT NULL, version INTEGER NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL, PRIMARY KEY (incarnation, principal));
CREATE INDEX IF NOT EXISTS ended_index_due ON ended_index (next_at);
";

/// Ended lives one pass clears the storage of.
const CLEARED_PER_PASS: i64 = 4;
/// Pages of blobs (R2's, up to 1000 keys each) one pass deletes of one life.
const BLOB_PAGES_PER_PASS: usize = 10;

/// A life a delete ended: whose list it tells first.
pub(crate) struct Ended {
    pub(crate) owner: String,
}

/// A list an ended life has still to tell.
#[derive(Deserialize)]
struct Untold {
    incarnation: i64,
    principal: String,
    version: i64,
    attempts: i64,
    name: String,
}

/// An ended life whose facet's database or blobs are still to go.
#[derive(Deserialize)]
struct Stored {
    incarnation: i64,
    name: String,
    npub: String,
    facet: String,
    attempts: i64,
}

impl FragmentCell {
    /// Ends this life in one synchronous step, with no await: a crash
    /// leaves the life whole, or ended with all its cleanup recorded, and
    /// a delete asked again after an error records nothing twice.
    /// Records what the alarm cleans after, stops the app, closes the
    /// sockets, and drops the life's tables, made again empty.
    pub(crate) fn end_life(&self) -> CellResult<Ended> {
        let [name, created_at, owner, npub, version] =
            self.metas([MetaKey::Name, MetaKey::CreatedAt, MetaKey::Owner, MetaKey::Npub, MetaKey::IndexVersion])?;
        let name = name.ok_or_else(|| missing(MetaKey::Name))?;
        let incarnation: i64 = created_at.and_then(|c| c.parse().ok()).ok_or_else(|| missing(MetaKey::CreatedAt))?;
        let owner = owner.ok_or_else(|| missing(MetaKey::Owner))?;
        let npub = npub.ok_or_else(|| missing(MetaKey::Npub))?;
        // newer than every change this life sent: each list takes it
        let version = version.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1;
        let facet = self.app_facet()?;
        let now = SqlStorageValue::Integer(js::now_ms());
        self.exec(
            "INSERT INTO ended (incarnation, name, npub, facet, ended_at, next_at) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (incarnation) DO NOTHING",
            vec![SqlStorageValue::Integer(incarnation), name.as_str().into(), npub.into(), facet.as_str().into(), now.clone(), now.clone()],
        )?;
        // every member's list, and any a change still waits for (a member
        // removed whose list was not told yet)
        self.exec(
            "INSERT INTO ended_index (incarnation, principal, version, next_at)
             SELECT ?, principal, ?, ? FROM (SELECT principal FROM members UNION SELECT principal FROM index_outbox) WHERE true
             ON CONFLICT (incarnation, principal) DO NOTHING",
            vec![SqlStorageValue::Integer(incarnation), SqlStorageValue::Integer(version), now],
        )?;
        // its database goes with the alarm; nothing runs in it from now
        if let Err(e) = js::abort_app_facet(&self.raw, &facet, "the fragment was deleted") {
            console_warn!("{name}: stopping the app facet {facet}: {}", e.message);
        }
        for ws in self.state.get_websockets() {
            let _ = ws.close(Some(4004), Some("the fragment was deleted"));
        }
        for table in fragment_core::ddl::tables(crate::fragment::SCHEMA) {
            self.exec(&format!("DROP TABLE IF EXISTS {table}"), vec![])?;
        }
        self.sql().exec(crate::fragment::SCHEMA, None)?;
        assert!(self.meta(MetaKey::CreatedAt)?.is_none(), "an ended life leaves no fragment");
        Ok(Ended { owner })
    }

    /// When an ended life's cleanup is next due, if one has any left.
    pub(crate) fn ended_due_at(&self) -> CellResult<Option<i64>> {
        let rows = self.rows(
            "SELECT MIN(at) AS at FROM (SELECT MIN(next_at) AS at FROM ended_index UNION ALL SELECT MIN(next_at) FROM ended WHERE stored = 1)",
            vec![],
        )?;
        Ok(rows.first().and_then(|r| r["at"].as_i64()))
    }

    /// The alarm's part: tells a batch of lists, clears what storage is
    /// due, and forgets each life with nothing left. Nothing here fails
    /// the alarm: what fails waits, doubling, and is tried again.
    pub(crate) async fn drain_ended(&self) {
        self.tell_ended(None).await;
        self.clear_ended().await;
        self.forget_ended().await;
    }

    /// Tells due lists their ended life is gone, at most `INDEX_FLUSH_MAX`,
    /// all at once, `first`'s list first.
    pub(crate) async fn tell_ended(&self, first: Option<&str>) {
        let now = js::now_ms();
        let due: Vec<Untold> = match self.typed(
            "SELECT i.incarnation, i.principal, i.version, i.attempts, e.name FROM ended_index i JOIN ended e USING (incarnation)
             WHERE i.next_at <= ? ORDER BY i.principal = ? DESC, i.next_at LIMIT ?",
            vec![SqlStorageValue::Integer(now), first.unwrap_or("").into(), SqlStorageValue::Integer(INDEX_FLUSH_MAX)],
        ) {
            Ok(due) => due,
            Err(e) => return console_error!("the ended lives' lists did not read ({:?}): {}", e.code, e.message),
        };
        assert!(due.len() as i64 <= INDEX_FLUSH_MAX, "a flush tells a bounded batch");
        let told = join_all(due.iter().map(|u| {
            let body = json!({ "fragment": u.name, "role": null, "incarnation": u.incarnation, "version": u.version });
            async move { self.send_index(&u.principal, &body).await }
        }))
        .await;
        for (u, told) in due.iter().zip(told) {
            let key = vec![SqlStorageValue::Integer(u.incarnation), u.principal.as_str().into()];
            let done = if told {
                self.exec("DELETE FROM ended_index WHERE incarnation = ? AND principal = ?", key)
            } else {
                let attempts = u.attempts + 1;
                let at = SqlStorageValue::Integer(js::now_ms() + outbox_retry_ms(attempts));
                self.exec(
                    "UPDATE ended_index SET attempts = ?, next_at = ? WHERE incarnation = ? AND principal = ?",
                    [vec![SqlStorageValue::Integer(attempts), at], key].concat(),
                )
            };
            if let Err(e) = done {
                console_error!("{}: the ended life's row for {} did not write ({:?}): {}", u.name, u.principal, e.code, e.message);
            }
        }
    }

    /// Deletes the app facet's database and the blobs of each ended life
    /// due, at most `BLOB_PAGES_PER_PASS` pages of blobs each: one with
    /// more is due again at once.
    async fn clear_ended(&self) {
        let due: Vec<Stored> = match self.typed(
            "SELECT incarnation, name, npub, facet, attempts FROM ended WHERE stored = 1 AND next_at <= ? ORDER BY next_at LIMIT ?",
            vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(CLEARED_PER_PASS)],
        ) {
            Ok(due) => due,
            Err(e) => return console_error!("the ended lives did not read ({:?}): {}", e.code, e.message),
        };
        for s in due {
            let cleared = async {
                js::delete_app_facet(&self.raw, &s.facet).await?;
                self.delete_blobs_under(&format!("{}/", s.npub), BLOB_PAGES_PER_PASS).await
            }
            .await;
            let inc = SqlStorageValue::Integer(s.incarnation);
            let wrote = match cleared {
                Ok(true) => self.exec("UPDATE ended SET stored = 0 WHERE incarnation = ?", vec![inc]),
                Ok(false) => Ok(()),
                Err(e) => {
                    console_error!("{}: the ended life's facet or blobs did not go ({:?}): {}", s.name, e.code, e.message);
                    let attempts = s.attempts + 1;
                    let at = SqlStorageValue::Integer(js::now_ms() + outbox_retry_ms(attempts));
                    self.exec("UPDATE ended SET attempts = ?, next_at = ? WHERE incarnation = ?", vec![SqlStorageValue::Integer(attempts), at, inc])
                }
            };
            if let Err(e) = wrote {
                console_error!("{}: the ended life's row did not write ({:?}): {}", s.name, e.code, e.message);
            }
        }
    }

    /// Forgets each ended life with nothing left to clean. The last one
    /// forgotten, with no life made since and no create claiming the name,
    /// lets the object's storage go whole, as a delete's did before it
    /// answered.
    async fn forget_ended(&self) {
        let forgot = self.rows(
            "DELETE FROM ended WHERE stored = 0 AND NOT EXISTS (SELECT 1 FROM ended_index i WHERE i.incarnation = ended.incarnation)
             RETURNING name, incarnation, ended_at",
            vec![],
        );
        let forgot = match forgot {
            Ok(f) => f,
            Err(e) => return console_error!("the ended lives were not forgotten ({:?}): {}", e.code, e.message),
        };
        let now = js::now_ms();
        for f in &forgot {
            let (name, ended_at) = (f["name"].as_str().unwrap_or("?"), f["ended_at"].as_i64().unwrap_or(now));
            console_log!("{name}: its life ended at {} is cleaned up, {} ms after its delete", f["incarnation"], now - ended_at);
        }
        if forgot.is_empty() {
            return;
        }
        let released = async {
            let [created_at, claimed_at] = self.metas([MetaKey::CreatedAt, MetaKey::ClaimedAt])?;
            let claimed = claimed_at.and_then(|c| c.parse::<i64>().ok()).is_some_and(|at| now - at < CLAIM_TTL_MS);
            if created_at.is_some() || claimed || !self.rows("SELECT 1 FROM ended LIMIT 1", vec![])?.is_empty() {
                return Ok(false);
            }
            self.state.storage().delete_all().await?;
            self.sql().exec(crate::fragment::SCHEMA, None)?;
            self.sql().exec(SCHEMA, None)?;
            Ok::<bool, CellError>(true)
        }
        .await;
        if let Err(e) = released {
            console_error!("an ended fragment's storage was not released ({:?}): {}", e.code, e.message);
        }
    }

    /// The test lever `ended` (ops.rs): each ended life, and what is left
    /// of its cleanup. It answers on an object with no life.
    pub(crate) fn ended_view(&self) -> CellResult<Value> {
        let lives = self.rows(
            "SELECT e.incarnation, e.name, e.stored, e.attempts,
               (SELECT COUNT(*) FROM ended_index i WHERE i.incarnation = e.incarnation) AS lists
             FROM ended e ORDER BY e.incarnation",
            vec![],
        )?;
        Ok(json!({ "ended": lives, "dueAt": self.ended_due_at()? }))
    }
}
