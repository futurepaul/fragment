//! Channels (docs/MODEL.md): append-only, ordered, per-fragment logs. The
//! platform writes `events` (the audit trail) and `ops` (one record per
//! applied mutation); mutations publish to the app channels their
//! fragment.json declares. Clients read: a page at a time over the API, or
//! live from a cursor over the fragment's `__live` socket.
//!
//! A mutation's effects are applied after the facet commits it. Before it
//! calls the facet, the supervisor records the mutation as pending: its
//! ledger id, operation, principal, and depth are the supervisor's own
//! facts, never read back from the app's tables (which the app can write).
//! The row's `seq` is the run: the facet stores it beside its ledger row,
//! and it keys the run's effects (`<ledger id>#<run>`, index), so applying
//! them again (a retry after a passing failure, or the sweep after a crash
//! between the facet's commit and the supervisor's) appends nothing twice,
//! while the same id run again after the ledger forgot it is a new run
//! with keys of its own. The `ops` record is written last, in the same
//! step that drops the pending row.
//!
//! The effects are decoded and checked here before any is applied
//! (`fragment_core::effects`). A refusal is for good: all of the
//! mutation's effects are dropped, `events` says why, and the mutation
//! still counts as applied, so one bad effect never blocks the app. A
//! passing failure (code.storage, the delivery queue) leaves the row
//! pending, and the alarm tries it again later. A try again appends no
//! record twice, and for a record an earlier try appended it writes no
//! delivery twice but starts whatever of its triggered runs did not start
//! (`published`).

use std::collections::BTreeMap;
use std::sync::Arc;

use fragment_core::effects::{self, Effect};
use fragment_core::npub;
use fragment_proto::{limits, valid_channel_name, ChannelRecord, ErrorCode, Role, BUILTIN_CHANNELS};
use futures_util::lock::{Mutex, OwnedMutexGuard};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::js;

/// Due pending mutations one sweep settles.
const SWEEP_ROWS: i64 = 64;
/// Mutations waiting for their effects; past this, new mutations are
/// refused until they drain.
const PENDING_MAX: u64 = 10_000;
/// A passing failure tries again after 10 s, the wait doubling up to an
/// hour, at most 60 times: about two days in all.
const RETRY_FIRST_MS: i64 = 10_000;
const RETRY_WAIT_MAX_MS: i64 = 3_600_000;
const TRIES_MAX: i64 = 60;

/// The wait before try number `tries` (from 1).
const fn retry_after_ms(tries: i64) -> i64 {
    let mut wait = RETRY_FIRST_MS;
    let mut i = 1;
    while i < tries && wait < RETRY_WAIT_MAX_MS {
        wait *= 2;
        i += 1;
    }
    if wait > RETRY_WAIT_MAX_MS {
        RETRY_WAIT_MAX_MS
    } else {
        wait
    }
}

const fn retry_horizon_ms() -> i64 {
    let mut total = 0;
    let mut tries = 1;
    while tries <= TRIES_MAX {
        total += retry_after_ms(tries);
        tries += 1;
    }
    total
}

// A late try must still find what its first try left: the keys that make
// a commit or a push happen once, and the facet's ledger row.
const _: () = assert!(retry_horizon_ms() < crate::files::WRITES_KEPT_MS);
const _: () = assert!(retry_horizon_ms() < crate::ops::LEDGER_KEPT_MS);

/// A mutation the supervisor recorded before the facet ran it, not yet settled.
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    /// The run: new for each run of the ledger id, stored by the facet with its row.
    pub seq: i64,
    /// `<principal>/<the caller's operation id>`: the facet's ledger key.
    pub ledger_id: String,
    pub op: String,
    pub principal: String,
    /// How deep in a chain of triggered runs the mutation ran.
    pub depth: u32,
    /// Passing failures so far.
    pub tries: i64,
}

impl Pending {
    /// What keys this run's effects: its records, its commit, its pushes, its `ops` record.
    pub fn key(&self) -> String {
        format!("{}#{}", self.ledger_id, self.seq)
    }

    /// The caller's operation id.
    fn id(&self) -> &str {
        self.ledger_id.split_once('/').map_or(self.ledger_id.as_str(), |(_, id)| id)
    }
}

const PENDING_COLUMNS: &str = "seq, ledger_id, op, principal, depth, tries";

fn pending_of(r: &Value) -> Pending {
    Pending {
        seq: r["seq"].as_i64().expect("pending.seq is INTEGER"),
        ledger_id: r["ledger_id"].as_str().expect("pending.ledger_id is TEXT").to_string(),
        op: r["op"].as_str().expect("pending.op is TEXT").to_string(),
        principal: r["principal"].as_str().expect("pending.principal is TEXT").to_string(),
        depth: r["depth"].as_u64().and_then(|d| u32::try_from(d).ok()).expect("pending.depth is a small INTEGER"),
        tries: r["tries"].as_i64().expect("pending.tries is INTEGER"),
    }
}

/// How a committed run settled.
pub(crate) enum Settled {
    Applied,
    /// The platform refused its effects, all of them, for good.
    Refused(String),
}

/// One ledger id held: while it is, no other call or sweep settles it.
pub(crate) struct Held<'a> {
    cell: &'a FragmentCell,
    ledger_id: String,
    guard: Option<OwnedMutexGuard<()>>,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        // The last holder, with no one waiting, takes the entry out, so the
        // map holds only ids in flight.
        let mut settling = self.cell.settling.borrow_mut();
        if settling.get(&self.ledger_id).is_some_and(|lock| Arc::strong_count(lock) == 1) {
            settling.remove(&self.ledger_id);
        }
    }
}

/// A failure that trying again cannot change: the effect's or the input's.
fn is_refusal(code: ErrorCode) -> bool {
    matches!(code, ErrorCode::InvalidRequest | ErrorCode::TooLarge)
}

/// A stored record as its row holds it.
#[derive(Deserialize)]
struct RecordRow {
    channel: String,
    seq: i64,
    at: i64,
    principal: String,
    kind: String,
    body: String,
}

impl RecordRow {
    /// The body stays the text the cell stored: checked to be JSON (a scan,
    /// no tree built), never parsed into a value only to be written again.
    fn record(self) -> CellResult<ChannelRecord> {
        let body = RawValue::from_string(self.body).map_err(|e| CellError::host(format!("record {}#{}: its stored body is not JSON: {e}", self.channel, self.seq)))?;
        Ok(ChannelRecord { principal: npub::display(&self.principal), channel: self.channel, seq: self.seq, at: self.at, kind: self.kind, body })
    }
}

impl FragmentCell {
    /// Appends a record; `op` = (ledger id, index) makes it idempotent.
    /// Answers the record, or `None` when that (op, index) already exists.
    /// The record answered is the body given, as it was stored: nothing is
    /// read back but its place.
    pub(crate) fn append(&self, channel: &str, principal: &str, kind: &str, body: &Value, op: Option<(&str, i64)>) -> CellResult<Option<ChannelRecord>> {
        let now = js::now_ms();
        let (op_id, idx) = match op {
            Some((id, i)) => (SqlStorageValue::from(id), SqlStorageValue::Integer(i)),
            None => (SqlStorageValue::Null, SqlStorageValue::Null),
        };
        let body = serde_json::value::to_raw_value(body).expect("a JSON value serializes");
        let rows = self.rows(
            "INSERT INTO records (channel, seq, at, principal, kind, body, op, idx, outboxed)
             VALUES (?, (SELECT COALESCE(MAX(seq), 0) + 1 FROM records WHERE channel = ?), ?, ?, ?, ?, ?, ?, 0)
             ON CONFLICT DO NOTHING RETURNING seq",
            vec![channel.into(), channel.into(), SqlStorageValue::Integer(now), principal.into(), kind.into(), body.get().into(), op_id, idx],
        )?;
        let Some(row) = rows.first() else { return Ok(None) };
        let seq = row["seq"].as_i64().expect("records.seq is INTEGER");
        assert!(seq > 0, "a channel's records number from 1");
        if BUILTIN_CHANNELS.contains(&channel) {
            self.exec(
                "DELETE FROM records WHERE channel = ? AND seq <= (SELECT MAX(seq) FROM records WHERE channel = ?) - ?",
                vec![channel.into(), channel.into(), SqlStorageValue::Integer(limits::AUDIT_KEPT)],
            )?;
        }
        let record = ChannelRecord { channel: channel.to_string(), seq, at: now, principal: npub::display(principal), kind: kind.to_string(), body };
        self.broadcast_record(&record);
        Ok(Some(record))
    }

    /// Appends a keyed record, or finds the one an earlier try of the same
    /// (op, index) appended. Answers the record and whether this call
    /// appended it.
    pub(crate) fn append_once(&self, channel: &str, principal: &str, kind: &str, body: &Value, key: &str, index: i64) -> CellResult<(ChannelRecord, bool)> {
        if let Some(record) = self.append(channel, principal, kind, body, Some((key, index)))? {
            return Ok((record, true));
        }
        let rows: Vec<RecordRow> = self.typed(
            "SELECT channel, seq, at, principal, kind, body FROM records WHERE op = ? AND idx = ?",
            vec![key.into(), SqlStorageValue::Integer(index)],
        )?;
        // the insert conflicted on (op, idx), so a row holds them, and one effect names one channel
        let row = rows.into_iter().next().ok_or_else(|| CellError::host(format!("record ({key}, {index}) conflicted, and no record holds it")))?;
        let record = row.record()?;
        if record.channel != channel {
            return Err(CellError::host(format!("record ({key}, {index}) is on {:?}, not {channel:?}", record.channel)));
        }
        Ok((record, false))
    }

    /// What follows a record, in order. Its deliveries are written to the
    /// outbox in the same step as its append, before anything that can
    /// fail (a record found already appended has them). Then the runs its
    /// channel's triggers start, each once per record (`fire_channel`), so
    /// a try after a failure here starts only what did not start; then the
    /// outbox is drained, whatever the triggers did. Answers the runs.
    pub(crate) async fn published(&self, record: &ChannelRecord, appended: bool, depth: u32) -> CellResult<Vec<i64>> {
        // A retry finds the record appended; its deliveries are written
        // then only if the try that appended it never wrote them (the mark
        // goes with them), so none is lost and none is sent twice.
        let queued = if appended || !self.outboxed(record)? { self.outbox_record(record)? } else { false };
        let fired = self.fire_channel(record, depth);
        if queued {
            self.drain_deliveries().await;
        }
        fired
    }

    /// Drops audit records older than their retention (from the alarm).
    pub(crate) fn trim_audit(&self) -> CellResult<()> {
        self.exec(
            "DELETE FROM records WHERE channel IN ('events', 'ops') AND at < ?",
            vec![SqlStorageValue::Integer(js::now_ms() - limits::AUDIT_RETENTION_MS)],
        )
    }

    /// Who may read a channel, or 404 when there is no such channel.
    pub(crate) fn channel_read_role(&self, channel: &str) -> CellResult<Role> {
        if BUILTIN_CHANNELS.contains(&channel) {
            return Ok(Role::Viewer);
        }
        self.declared_channel(channel)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no channel named {channel:?}")))
    }

    /// A page of records after `after`.
    pub(crate) fn read_channel(&self, channel: &str, after: i64, limit: usize) -> CellResult<Vec<ChannelRecord>> {
        let rows: Vec<RecordRow> = self.typed(
            "SELECT channel, seq, at, principal, kind, body FROM records WHERE channel = ? AND seq > ? ORDER BY seq LIMIT ?",
            vec![channel.into(), SqlStorageValue::Integer(after), SqlStorageValue::Integer(limit.clamp(1, limits::CHANNEL_PAGE) as i64)],
        )?;
        rows.into_iter().map(RecordRow::record).collect()
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

    /// Waits for, then holds, one ledger id.
    pub(crate) async fn hold(&self, ledger_id: &str) -> Held<'_> {
        let lock: Arc<Mutex<()>> = self.settling.borrow_mut().entry(ledger_id.to_string()).or_default().clone();
        let guard = lock.lock_owned().await;
        Held { cell: self, ledger_id: ledger_id.to_string(), guard: Some(guard) }
    }

    /// Holds one ledger id if no one else does.
    fn try_hold(&self, ledger_id: &str) -> Option<Held<'_>> {
        let lock: Arc<Mutex<()>> = self.settling.borrow_mut().entry(ledger_id.to_string()).or_default().clone();
        let guard = lock.try_lock_owned()?;
        Some(Held { cell: self, ledger_id: ledger_id.to_string(), guard: Some(guard) })
    }

    fn pending(&self, ledger_id: &str) -> CellResult<Option<Pending>> {
        let rows = self.rows(&format!("SELECT {PENDING_COLUMNS} FROM pending WHERE ledger_id = ?"), vec![ledger_id.into()])?;
        Ok(rows.first().map(pending_of))
    }

    /// Records a mutation as pending before the facet runs it (the caller
    /// holds its ledger id). Answers the row, and whether this call made
    /// it: a row already there is an earlier attempt's, not yet settled.
    ///
    /// The facet call that follows is an outgoing message, which the
    /// runtime's output gate holds until this write is stored, so a run the
    /// facet commits always has its pending row.
    pub(crate) fn begin(&self, ledger_id: &str, op: &str, principal: &str, depth: u32) -> CellResult<(Pending, bool)> {
        if let Some(p) = self.pending(ledger_id)? {
            return Ok((p, false));
        }
        if self.count("SELECT COUNT(*) AS n FROM pending")? >= PENDING_MAX {
            return Err(CellError::new(ErrorCode::RateLimited, format!("{PENDING_MAX} mutations are waiting for their effects to apply; retry shortly")));
        }
        let now = SqlStorageValue::Integer(js::now_ms());
        let rows = self.rows(
            &format!("INSERT INTO pending (ledger_id, op, principal, depth, tries, next_at, at) VALUES (?, ?, ?, ?, 0, ?, ?) RETURNING {PENDING_COLUMNS}"),
            vec![ledger_id.into(), op.into(), principal.into(), SqlStorageValue::Integer(depth.into()), now.clone(), now],
        )?;
        let p = pending_of(rows.first().expect("an insert answers its row"));
        assert!(p.ledger_id == ledger_id && p.tries == 0, "the new row is this mutation's");
        Ok((p, true))
    }

    /// A run that committed under a row an earlier attempt left: the row
    /// now describes this call.
    pub(crate) fn restate(&self, p: &mut Pending, op: &str, depth: u32) -> CellResult<()> {
        self.exec("UPDATE pending SET op = ?, depth = ? WHERE seq = ?", vec![op.into(), SqlStorageValue::Integer(depth.into()), SqlStorageValue::Integer(p.seq)])?;
        p.op = op.to_string();
        p.depth = depth;
        Ok(())
    }

    /// Drops a pending row: its run settled, or never committed.
    pub(crate) fn forget(&self, p: &Pending) -> CellResult<()> {
        self.exec("DELETE FROM pending WHERE seq = ?", vec![SqlStorageValue::Integer(p.seq)])
    }

    /// When the next try after a passing failure is due (for the alarm). A
    /// row a call is settling now is that call's: counting it would re-arm
    /// the alarm every 50 ms until the call ends. At most as many rows as
    /// there are ids in flight are passed over, so one more is enough.
    pub(crate) fn pending_due_at(&self) -> CellResult<Option<i64>> {
        let settling = self.settling.borrow();
        let limit = i64::try_from(settling.len() + 1).expect("ids in flight are few");
        let rows = self.rows("SELECT ledger_id, next_at FROM pending WHERE tries > 0 ORDER BY next_at LIMIT ?", vec![SqlStorageValue::Integer(limit)])?;
        Ok(rows
            .iter()
            .find(|r| !settling.contains_key(r["ledger_id"].as_str().expect("pending.ledger_id is TEXT")))
            .map(|r| r["next_at"].as_i64().expect("pending.next_at is INTEGER")))
    }

    /// Settles a run the facet committed: applies its effects, or refuses
    /// them all, then writes its `ops` record and drops its pending row.
    /// A passing failure leaves the row for a later try (and answers the
    /// failure). Each new record starts the runs its channel triggers, one
    /// hop deeper than the mutation.
    pub(crate) async fn apply(&self, p: &Pending, effects: &Value) -> CellResult<Settled> {
        let declared = match self.declared_channels() {
            Ok(d) => d,
            Err(e) => return self.after_failure(p, e).await,
        };
        let list = match effects::decode(effects, &declared) {
            Ok(list) => list,
            Err(why) => return self.refuse(p, &why),
        };
        let key = p.key();
        let mut writes = vec![];
        for (i, effect) in list.into_iter().enumerate() {
            let index = i64::try_from(i).expect("a mutation has at most EFFECTS_MAX effects");
            let done = match effect {
                Effect::Record { channel, kind, body } => self.apply_record(p, &key, index, &channel, &kind, &body).await,
                Effect::File { path, bytes } => {
                    writes.push(crate::files::FileWrite { path, bytes });
                    Ok(())
                }
                Effect::Push { who, payload } => self.send_push(&format!("{key}:{i}"), &who, &payload).await.map(|_| ()),
            };
            if let Err(e) = done {
                return self.after_failure(p, e).await;
            }
        }
        if !writes.is_empty() {
            let message = format!("{} {}", p.op, p.id());
            match self.commit_files(&key, &writes, &BTreeMap::new(), &message, &p.principal, p.depth).await {
                Ok(crate::files::Wrote::Commit(_)) => {}
                Ok(crate::files::Wrote::Conflict(why)) => unreachable!("a mutation's commit expects nothing, so nothing conflicts: {why}"),
                Err(e) => return self.after_failure(p, e).await,
            }
        }
        self.mark_applied(p)?;
        Ok(Settled::Applied)
    }

    async fn apply_record(&self, p: &Pending, key: &str, index: i64, channel: &str, kind: &str, body: &Value) -> CellResult<()> {
        let (record, appended) = self.append_once(channel, &p.principal, kind, body, key, index)?;
        self.published(&record, appended, p.depth + 1).await?;
        Ok(())
    }

    async fn after_failure(&self, p: &Pending, e: CellError) -> CellResult<Settled> {
        if is_refusal(e.code) {
            return self.refuse(p, &e.message);
        }
        self.retry_later(p, &e.message).await?;
        Err(e)
    }

    /// Drops every effect of a run for good, says why in `events`, and
    /// settles it: the app goes on.
    fn refuse(&self, p: &Pending, why: &str) -> CellResult<Settled> {
        let data = json!({ "op": p.op, "id": p.id(), "principal": npub::display(&p.principal), "why": why });
        self.event("effects.refused", &format!("{} {}: {why}", p.op, p.id()), data);
        self.mark_applied(p)?;
        Ok(Settled::Refused(why.to_string()))
    }

    /// Marks a run applied: its `ops` record and its pending row's removal
    /// are one step (no await between them), so they are stored together.
    fn mark_applied(&self, p: &Pending) -> CellResult<()> {
        if self.append("ops", &p.principal, "mutation", &json!({ "op": p.op, "id": p.id() }), Some((&p.key(), -1)))?.is_some() {
            self.broadcast_changed(&p.op);
        }
        self.forget(p)
    }

    /// A passing failure: the run stays pending, and the alarm tries it
    /// again, later each time, until it gives up (said in `events`).
    pub(crate) async fn retry_later(&self, p: &Pending, why: &str) -> CellResult<()> {
        let tries = p.tries + 1;
        let data = |wait: Option<i64>| json!({ "op": p.op, "id": p.id(), "why": why, "tries": tries, "retryInMs": wait });
        if tries > TRIES_MAX {
            self.event("effects.abandoned", &format!("{} {}: gave up after {TRIES_MAX} tries: {why}", p.op, p.id()), data(None));
            return self.forget(p);
        }
        let wait = retry_after_ms(tries);
        let next_at = js::now_ms() + wait;
        self.exec(
            "UPDATE pending SET tries = ?, next_at = ? WHERE seq = ?",
            vec![SqlStorageValue::Integer(tries), SqlStorageValue::Integer(next_at), SqlStorageValue::Integer(p.seq)],
        )?;
        if tries == 1 {
            self.event("effects.delayed", &format!("{} {}: {why}; trying again", p.op, p.id()), data(Some(wait)));
        }
        // By this row's own time: while its caller still holds it, the
        // alarm's own reckoning passes it over.
        self.schedule_by(next_at).await
    }

    /// Settles one pending run from the facet's ledger: its effects when
    /// the facet committed this run, nothing (the row goes) when it never
    /// did. Answers whether it applied or refused effects; a passing
    /// failure is scheduled again, not answered.
    pub(crate) async fn settle_from_ledger(&self, facet: &js::Facet, p: &Pending) -> CellResult<bool> {
        let row = match facet.ledger(&p.ledger_id).await {
            Ok(row) => row,
            Err(e) => {
                self.retry_later(p, &format!("the app did not answer: {}", e.message)).await?;
                return Ok(false);
            }
        };
        // Only this run's row: an older run of the same id holds its own
        // number (or none, from before runs were numbered).
        match row {
            Some(row) if row.run == Some(p.seq) => Ok(self.apply(p, &row.effects).await.is_ok()),
            _ => {
                self.forget(p)?;
                Ok(false)
            }
        }
    }

    /// Pending runs no call is settling: those a call left behind (a crash,
    /// a lost answer) and those whose next try is due.
    fn due_pending(&self) -> CellResult<Vec<Pending>> {
        let rows = self.rows(
            &format!("SELECT {PENDING_COLUMNS} FROM pending WHERE tries = 0 OR next_at <= ? ORDER BY next_at LIMIT ?"),
            vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(SWEEP_ROWS)],
        )?;
        let settling = self.settling.borrow();
        Ok(rows.iter().map(pending_of).filter(|p| !settling.contains_key(&p.ledger_id)).collect())
    }

    /// The first call of an activation settles what earlier activations
    /// left pending.
    pub(crate) async fn sweep(&self, facet: &js::Facet) -> CellResult<()> {
        if self.swept.replace(true) {
            return Ok(());
        }
        let due = self.due_pending()?;
        self.settle_listed(facet, due).await
    }

    /// From the alarm: settles what is due, loading the app only when
    /// something is.
    pub(crate) async fn sweep_due(&self) -> CellResult<()> {
        let due = self.due_pending()?;
        if due.is_empty() {
            return Ok(());
        }
        let facet = match self.facet() {
            Ok(f) => f,
            Err(e) => {
                for p in &due {
                    self.retry_later(p, &e.message).await?;
                }
                return Ok(());
            }
        };
        self.swept.set(true);
        self.settle_listed(&facet, due).await
    }

    /// Settles each listed run on its own: one that fails waits for its
    /// next try and never fails the call or the alarm that swept it.
    async fn settle_listed(&self, facet: &js::Facet, due: Vec<Pending>) -> CellResult<()> {
        assert!(due.len() <= SWEEP_ROWS as usize, "a sweep is bounded");
        let mut settled = 0;
        for listed in due {
            // A call that started meanwhile settles its own; one that
            // finished meanwhile may have settled this run already.
            let Some(_held) = self.try_hold(&listed.ledger_id) else { continue };
            let Some(p) = self.pending(&listed.ledger_id)?.filter(|p| p.seq == listed.seq) else { continue };
            if self.settle_from_ledger(facet, &p).await? {
                settled += 1;
            }
        }
        if settled > 0 {
            self.event("effects.swept", &format!("{settled} mutation(s) committed earlier settled now"), json!({ "settled": settled }));
        }
        Ok(())
    }
}
