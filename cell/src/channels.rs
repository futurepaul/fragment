//! Channels (docs/MODEL.md): append-only, ordered, per-fragment logs. The
//! platform writes `events` (the audit trail) and `ops` (one record per
//! applied mutation); mutations publish to the app channels their
//! fragment.json declares. Clients read: a page at a time over the API, or
//! live from a cursor over the fragment's `__live` socket.
//!
//! A mutation's effects are applied after the facet commits it, keyed by
//! (ledger id, index), so applying them again (a replay, or the sweep after
//! a crash between the facet's commit and the supervisor's) appends
//! nothing twice. The `ops` record is written last: it marks the mutation
//! applied.

use std::collections::BTreeMap;

use fragment_core::npub;
use fragment_proto::{limits, valid_channel_name, ChannelDecl, ChannelRecord, ErrorCode, Role, BUILTIN_CHANNELS};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::js;

/// Sweep this many of the newest ledger rows for unapplied effects.
const SWEEP_ROWS: u32 = 200;
/// `events` and `ops` keep at most this many records each.
const AUDIT_KEPT: i64 = 10_000;

/// What a mutation asked for, applied once it committed: a record, or a
/// file written to `main` (all of one mutation's files are one commit).
#[derive(Deserialize)]
#[serde(untagged)]
pub enum Effect {
    Record { channel: String, kind: String, body: Value },
    File { file: String, #[serde(default)] text: Option<String>, #[serde(default)] base64: Option<String> },
}

fn record_json(r: &Value) -> ChannelRecord {
    ChannelRecord {
        channel: r["channel"].as_str().unwrap_or("").to_string(),
        seq: r["seq"].as_i64().unwrap_or(0),
        at: r["at"].as_i64().unwrap_or(0),
        principal: r["principal"].as_str().map(npub::display).unwrap_or_default(),
        kind: r["kind"].as_str().unwrap_or("").to_string(),
        body: r["body"].as_str().and_then(|b| serde_json::from_str(b).ok()).unwrap_or(Value::Null),
    }
}

impl FragmentCell {
    /// Appends a record; `op` = (ledger id, index) makes it idempotent.
    /// Answers the record, or `None` when that (op, index) already exists.
    pub(crate) fn append(&self, channel: &str, principal: &str, kind: &str, body: &Value, op: Option<(&str, i64)>) -> CellResult<Option<ChannelRecord>> {
        let now = js::now_ms();
        let (op_id, idx) = match op {
            Some((id, i)) => (SqlStorageValue::from(id), SqlStorageValue::Integer(i)),
            None => (SqlStorageValue::Null, SqlStorageValue::Null),
        };
        let rows = self.rows(
            "INSERT INTO records (channel, seq, at, principal, kind, body, op, idx)
             VALUES (?, (SELECT COALESCE(MAX(seq), 0) + 1 FROM records WHERE channel = ?), ?, ?, ?, ?, ?, ?)
             ON CONFLICT DO NOTHING RETURNING channel, seq, at, principal, kind, body",
            vec![channel.into(), channel.into(), SqlStorageValue::Integer(now), principal.into(), kind.into(), body.to_string().into(), op_id, idx],
        )?;
        let Some(row) = rows.first() else { return Ok(None) };
        if BUILTIN_CHANNELS.contains(&channel) {
            self.exec(
                "DELETE FROM records WHERE channel = ? AND seq <= (SELECT MAX(seq) FROM records WHERE channel = ?) - ?",
                vec![channel.into(), channel.into(), SqlStorageValue::Integer(AUDIT_KEPT)],
            )?;
        }
        let record = record_json(row);
        self.broadcast_record(&record);
        Ok(Some(record))
    }

    /// Drops audit records older than their retention (from the alarm).
    pub(crate) fn trim_audit(&self) -> CellResult<()> {
        self.exec(
            "DELETE FROM records WHERE channel IN ('events', 'ops') AND at < ?",
            vec![SqlStorageValue::Integer(js::now_ms() - limits::AUDIT_RETENTION_MS)],
        )
    }

    /// The app channels the live code declares.
    pub(crate) fn declared_channels(&self) -> CellResult<BTreeMap<String, ChannelDecl>> {
        let rows = self.rows("SELECT channels FROM code WHERE id = 1", vec![])?;
        Ok(rows.first().and_then(|r| r["channels"].as_str()).and_then(|c| serde_json::from_str(c).ok()).unwrap_or_default())
    }

    /// Who may read a channel, or 404 when there is no such channel.
    pub(crate) fn channel_read_role(&self, channel: &str) -> CellResult<Role> {
        if BUILTIN_CHANNELS.contains(&channel) {
            return Ok(Role::Viewer);
        }
        self.declared_channels()?
            .get(channel)
            .map(|d| d.read)
            .ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no channel named {channel:?}")))
    }

    /// A page of records after `after`.
    pub(crate) fn read_channel(&self, channel: &str, after: i64, limit: usize) -> CellResult<Vec<ChannelRecord>> {
        let rows = self.rows(
            "SELECT channel, seq, at, principal, kind, body FROM records WHERE channel = ? AND seq > ? ORDER BY seq LIMIT ?",
            vec![channel.into(), SqlStorageValue::Integer(after), SqlStorageValue::Integer(limit.clamp(1, limits::CHANNEL_PAGE) as i64)],
        )?;
        Ok(rows.iter().map(record_json).collect())
    }

    /// `GET /api/f/<name>/channels`
    pub(crate) fn channels(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let mut out: Vec<Value> = vec![];
        let mut add = |name: &str, read: Role| -> CellResult<()> {
            let last = self.rows("SELECT MAX(seq) AS n FROM records WHERE channel = ?", vec![name.into()])?;
            out.push(json!({ "name": name, "read": read, "seq": last.first().and_then(|r| r["n"].as_i64()).unwrap_or(0) }));
            Ok(())
        };
        for b in BUILTIN_CHANNELS {
            add(b, Role::Viewer)?;
        }
        for (name, decl) in self.declared_channels()? {
            add(&name, decl.read)?;
        }
        json_response(&json!({ "channels": out }))
    }

    /// `GET /api/f/<name>/channels/<channel>?after=&limit=`
    pub(crate) fn channel(&self, caller: &Caller, channel: &str, after: i64, limit: usize) -> CellResult<Response> {
        self.require(caller, false, Role::Public)?;
        if !valid_channel_name(channel) {
            return Err(CellError::invalid("a channel name must match ^[a-z][a-z0-9_-]{0,63}$"));
        }
        let read = self.channel_read_role(channel)?;
        self.require(caller, false, read)?;
        let records = self.read_channel(channel, after, limit)?;
        let next = records.last().map_or(after, |r| r.seq);
        json_response(&json!({ "channel": channel, "records": records, "next": next }))
    }

    /// Applies a committed mutation's effects, then its `ops` record. Each
    /// new record starts the runs its channel triggers, one hop deeper than
    /// the mutation (`depth`).
    pub(crate) async fn apply(&self, ledger_id: &str, op: &str, effects: &[Effect], depth: u32) -> CellResult<()> {
        let principal = ledger_id.split_once('/').map(|(p, _)| p).unwrap_or("unknown");
        let id = ledger_id.split_once('/').map(|(_, id)| id).unwrap_or(ledger_id);
        let mut writes = vec![];
        for (i, e) in effects.iter().enumerate() {
            match e {
                Effect::Record { channel, kind, body } => {
                    if let Some(record) = self.append(channel, principal, kind, body, Some((ledger_id, i as i64)))? {
                        self.fire_channel(&record, depth + 1)?;
                    }
                }
                Effect::File { file, text, base64 } => {
                    let bytes = match (text, base64) {
                        (None, None) => None,
                        _ => Some(crate::files::content_of(&serde_json::json!({ "text": text, "base64": base64 })).map_err(CellError::host)?),
                    };
                    writes.push(crate::files::FileWrite { path: file.clone(), bytes });
                }
            }
        }
        if !writes.is_empty() {
            let message = format!("{op} {id}");
            if let Err(e) = self.commit_files(ledger_id, &writes, &BTreeMap::new(), &message, principal, depth).await {
                // not applied: the next activation's sweep tries again
                self.swept.set(false);
                return Err(e);
            }
        }
        if self.append("ops", principal, "mutation", &json!({ "op": op, "id": id }), Some((ledger_id, -1)))?.is_some() {
            self.broadcast_changed(op);
        }
        Ok(())
    }

    /// After an activation, applies effects the facet committed but this
    /// object may never have applied (it died between the two commits).
    pub(crate) async fn sweep(&self, facet: &js::Facet) -> CellResult<()> {
        if self.swept.get() {
            return Ok(());
        }
        let answer = facet.call("__recent", &[SWEEP_ROWS.into()]).await.map_err(|m| CellError::host(format!("sweep: {m}")))?;
        let rows = answer["result"].as_array().cloned().unwrap_or_default();
        let mut applied = 0;
        for row in rows.iter().rev() {
            let id = row["id"].as_str().unwrap_or("");
            let done = !self.rows("SELECT seq FROM records WHERE op = ? AND idx = -1", vec![id.into()])?.is_empty();
            if !done && !id.is_empty() {
                let effects: Vec<Effect> = serde_json::from_value(row["effects"].clone()).unwrap_or_default();
                // The depth the mutation ran at is not kept; a swept chain restarts at 0.
                self.apply(id, row["name"].as_str().unwrap_or(""), &effects, 0).await?;
                applied += 1;
            }
        }
        if applied > 0 {
            self.event("effects.swept", &format!("{applied} mutation(s) committed before a restart applied now"), json!({ "applied": applied }));
        }
        self.swept.set(true);
        Ok(())
    }
}
