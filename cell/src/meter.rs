//! How a fragment meets its owner's ledger (docs/ledger.md, "How each
//! meter reaches it"; docs/cloudflare-v1.md, decisions 24, 26 and 27).
//!
//! A fragment's hosting bills its owner. Three meters, each a row the
//! fragment keeps in its own outbox (`meter_rows`) before anything is sent:
//!
//! - requests: each request the router hands it, counted per minute and
//!   closed into a row once that minute has passed (`req:<f>@<life>:<minute>`);
//! - dynamic workers: the first time a code version runs on a UTC day
//!   (`dw:<f>@<life>:<version>:<day>`), the unit Cloudflare bills;
//! - storage: a daily sample of its SQLite, its app's, and its blobs' bytes,
//!   as byte-hours since the sample before (`store:<f>@<life>:<class>:<at>`).
//!
//! References carry the fragment's life (`@<incarnation>`), so a name made
//! again never meets its earlier life's rows. The alarm flushes the outbox
//! one batch at a time, through the `fragment-ledger` queue, under the
//! batch's own id (`frag:<f>@<life>:<n>`); the consumer here applies it on
//! the owner's ledger and tells the fragment, which forgets the batch. One
//! not acknowledged is sent again, the same id and the same rows in the
//! same order, and the ledger answers it as before: a crash between its
//! apply and the acknowledgement charges nothing twice.
//!
//! The owner's standing gates the fragment's writes (decision 27): past
//! the overdraft, the owner's fragments refuse mutations, posts, file
//! writes, deploys, blob uploads and replays with a 402 that says why, and
//! keep serving reads; their cron and triggers start no runs, each one
//! recorded `blocked` with the reason instead (jobs.rs `start_run`). The
//! fragment asks the owner's ledger at most once a `STANDING_CACHE_MS`,
//! rather than on every write.

use fragment_core::ledger::{Meter, MeterRow, Metered, Refused, Spend};
use fragment_core::price::{StorageClass, Usage};
use fragment_proto::ledger::{PutCap, SetFragmentCap, Why};
use fragment_proto::{ErrorCode, Role};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell, MetaKey};
use crate::ledger::{self, MaySpend};
use crate::{js, routed};

/// The queue a fragment's meter batches travel on (wrangler.jsonc; a
/// branch deployment's is named for its branch), and its binding.
pub const QUEUE: &str = "fragment-ledger";
pub const QUEUE_BINDING: &str = "METERS";
/// Set on the internal routes below by platform code; the router never
/// sets or passes it.
pub const METER_HEADER: &str = "x-fragment-meter";
/// How long a fragment trusts what it last heard of its owner's standing.
/// A cache, so its contract: its source is the owner's ledger
/// (`may_spend(write)`), nothing invalidates it but its age, and a stale
/// read lets a write through for at most this long past the overdraft, or
/// refuses one for at most this long after a top-up.
pub const STANDING_CACHE_MS: i64 = 60_000;
/// A batch not acknowledged this long after it was sent is sent again.
const RESEND_AFTER_MS: i64 = 5 * 60_000;
/// Storage is sampled at most this often (from the alarm's daily pass).
pub const STORAGE_EVERY_MS: i64 = 24 * HOUR_MS;
/// Rows one batch carries: a queue message is at most 128 KB, and a row is
/// about 250 bytes of JSON.
pub const BATCH_ROWS: i64 = 200;
/// Messages one consumer batch holds at most (wrangler.jsonc's
/// `max_batch_size` for the ledger's queue is this or less).
const CONSUME_BATCH_MAX: usize = 100;
/// A ledger that did not answer is asked again this much later.
const RETRY_DELAY_S: u32 = 5;
/// A cap change's id, as its owner names it.
const CAP_ID_MAX_BYTES: usize = 64;
const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

const _: () = assert!(BATCH_ROWS as usize <= fragment_core::ledger::BATCH_ROWS_MAX, "a fragment's batch is one the ledger takes");
const _: () = assert!(STANDING_CACHE_MS <= 60_000, "a fragment learns its owner's standing within a minute");

/// A batch on its way to its payer's ledger: from fragment `source`, its
/// outbox's batch `seq`.
#[derive(Debug, Serialize, Deserialize)]
pub struct MeterBatch {
    pub payer: String,
    pub source: String,
    pub seq: i64,
    pub meter: Meter,
}

/// What the consumer tells the fragment its batch came to.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Acked {
    pub seq: i64,
    pub batch: String,
    /// The ledger's answer, or why it never takes the batch (a guest
    /// pays for nothing: their fragments are not billed).
    pub metered: Option<Metered>,
    pub refused: Option<String>,
}

/// What the fragment last heard of its owner's standing: when (ms), and
/// why its writes are refused, if they are.
pub type StandingSeen = Option<(i64, Option<Why>)>;

#[derive(Deserialize)]
struct Minute {
    minute: i64,
    count: i64,
}

#[derive(Deserialize)]
struct Waiting {
    id: i64,
    sent_at: Option<i64>,
}

impl FragmentCell {
    /// `<name>@<incarnation>`: what the fragment's references start with.
    pub(crate) fn meter_key(&self) -> CellResult<String> {
        Ok(format!("{}@{}", self.name()?, self.must(MetaKey::CreatedAt)?))
    }

    /// Keeps a row in the outbox, once by its reference.
    pub(crate) fn outbox(&self, reference: &str, usage: Usage, at_ms: i64) -> CellResult<()> {
        let row = MeterRow { reference: reference.to_string(), usage, fragment: Some(self.name()?), agent: None, computer: None, at_ms };
        assert!(fragment_core::price::printable(reference, fragment_core::ledger::ID_MAX_BYTES), "a meter's reference is one the ledger takes: {reference}");
        let text = serde_json::to_string(&row).expect("a meter row serializes");
        self.exec("INSERT OR IGNORE INTO meter_rows (ref, row, batch) VALUES (?, ?, NULL)", vec![reference.into(), text.into()])
    }

    /// Counts one request the router handed this created fragment: in this
    /// minute's count, or past the last minute closed into a row (a test's
    /// flush closes the minute it is in), so a reference is never sent with
    /// two counts. A count that fails is lost, never the request.
    pub(crate) fn count_request(&self) {
        let minute = js::now_ms() / MINUTE_MS;
        let counted = self.exec(
            "INSERT INTO meter_requests (minute, count)
             SELECT MAX(?, COALESCE((SELECT CAST(value AS INTEGER) FROM meta WHERE key = ?), -1) + 1), 1
             WHERE EXISTS (SELECT 1 FROM meta WHERE key = ?)
             ON CONFLICT (minute) DO UPDATE SET count = count + 1",
            vec![SqlStorageValue::Integer(minute), MetaKey::MeterClosed.key().into(), MetaKey::CreatedAt.key().into()],
        );
        if let Err(e) = counted {
            console_error!("{}", json!({ "event": "meter.count-failed", "message": e.message }));
        }
    }

    /// The alarm set to close this minute's count once it is over, once a
    /// minute at most (the activation remembers the minute it armed for).
    pub(crate) async fn meter_soon(&self) {
        let minute = js::now_ms() / MINUTE_MS;
        if self.meter_armed.get() >= minute {
            return;
        }
        self.meter_armed.set(minute);
        if let Err(e) = self.schedule_by((minute + 1) * MINUTE_MS).await {
            console_error!("{}", json!({ "event": "meter.arm-failed", "message": e.message }));
        }
    }

    /// The first time `version` (a loader id: one fragment's code, as the
    /// runtime runs it) runs on a UTC day: one dynamic worker. The
    /// activation remembers the last it noted, so a call costs no write.
    pub(crate) fn note_dynamic_worker(&self, version: &str) {
        let now = js::now_ms();
        let day = now / DAY_MS;
        if self.dw_noted.borrow().as_ref().is_some_and(|(v, d)| v == version && *d == day) {
            return;
        }
        let id = &hex::encode(<sha2::Sha256 as sha2::Digest>::digest(version.as_bytes()))[..16];
        let noted = self.meter_key().and_then(|key| self.outbox(&format!("dw:{key}:{id}:{day}"), Usage::DynamicWorkers { count: 1 }, now));
        match noted {
            Ok(()) => *self.dw_noted.borrow_mut() = Some((version.to_string(), day)),
            Err(e) => console_error!("{}", json!({ "event": "meter.dw-failed", "message": e.message })),
        }
    }

    /// The app facet's database size, from its platform code (it shares a
    /// realm with the author's, so an app could understate it: by at most
    /// `limits::APP_DB_MAX_BYTES`, its cap).
    async fn app_db_bytes(&self) -> u64 {
        let Ok(facet) = self.facet() else { return 0 };
        match facet.call("__size", &[]).await {
            Ok(v) => v.as_u64().unwrap_or(0).min(fragment_proto::limits::APP_DB_MAX_BYTES),
            Err(e) => {
                self.event("meter.size-failed", &format!("the app's database size: {}", e.message), json!({ "code": e.code }));
                0
            }
        }
    }

    /// A storage sample when one is due (`STORAGE_EVERY_MS` since the last,
    /// or since the fragment was made), or now (`force`): its SQLite and its
    /// app's as `sqlite`, its blobs as `r2`, each its bytes × the hours
    /// since the last sample.
    pub(crate) async fn sample_storage(&self, force: bool) -> CellResult<()> {
        let [created, sampled] = self.metas([MetaKey::CreatedAt, MetaKey::StorageSampledAt])?;
        let Some(created) = created.and_then(|c| c.parse::<i64>().ok()) else { return Ok(()) };
        let since = sampled.and_then(|s| s.parse::<i64>().ok()).unwrap_or(created);
        let now = js::now_ms();
        if !force && now - since < STORAGE_EVERY_MS {
            return Ok(());
        }
        let elapsed = u128::try_from(now - since).unwrap_or(0);
        let sqlite = self.sql().database_size() as u64 + self.app_db_bytes().await;
        let blobs = self.rows("SELECT COALESCE(SUM(size), 0) AS n FROM blobs", vec![])?.first().and_then(|r| r["n"].as_u64()).unwrap_or(0);
        let key = self.meter_key()?;
        for (class, name, bytes) in [(StorageClass::Sqlite, "sqlite", sqlite), (StorageClass::R2, "r2", blobs)] {
            let byte_hours = u64::try_from(u128::from(bytes) * elapsed / HOUR_MS as u128).unwrap_or(u64::MAX);
            if byte_hours > 0 {
                self.outbox(&format!("store:{key}:{name}:{now}"), Usage::Storage { class, byte_hours }, now)?;
            }
        }
        self.set_meta(MetaKey::StorageSampledAt, &now.to_string())
    }

    /// Closes the minutes that are over into rows (every counted minute,
    /// with `close_all`: a test's flush), then sends the waiting batch, or
    /// a new one: one at a time, each sent again until it is acknowledged.
    pub(crate) async fn flush_meters(&self, close_all: bool) -> CellResult<()> {
        if self.meta(MetaKey::CreatedAt)?.is_none() {
            return Ok(());
        }
        let key = self.meter_key()?;
        let now = js::now_ms();
        let upto = if close_all { i64::MAX } else { now / MINUTE_MS - 1 };
        let minutes: Vec<Minute> = self.typed("SELECT minute, count FROM meter_requests WHERE minute <= ? ORDER BY minute", vec![SqlStorageValue::Integer(upto)])?;
        // bounded: one row per minute counted since the last flush
        for m in &minutes {
            let count = u64::try_from(m.count).map_err(|_| CellError::host("a negative request count"))?;
            // a minute's row is at its end, or now for one that is not over (a test's)
            let at = ((m.minute + 1) * MINUTE_MS - 1).min(now);
            self.outbox(&format!("req:{key}:{}", m.minute), Usage::Requests { count }, at)?;
        }
        if let Some(last) = minutes.last() {
            self.exec("DELETE FROM meter_requests WHERE minute <= ?", vec![SqlStorageValue::Integer(last.minute)])?;
            self.set_meta(MetaKey::MeterClosed, &last.minute.to_string())?;
        }
        let waiting: Option<Waiting> = self.typed("SELECT id, sent_at FROM meter_batches ORDER BY id LIMIT 1", vec![])?.into_iter().next();
        let batch = match waiting {
            Some(b) => b,
            None => {
                let rows = self.count("SELECT COUNT(*) AS n FROM meter_rows WHERE batch IS NULL")?;
                if rows == 0 {
                    return Ok(());
                }
                let made = self.rows("INSERT INTO meter_batches (sent_at) VALUES (NULL) RETURNING id", vec![])?;
                let id = made.first().and_then(|r| r["id"].as_i64()).ok_or_else(|| CellError::host("a batch made has an id"))?;
                self.exec(
                    "UPDATE meter_rows SET batch = ? WHERE ref IN (SELECT ref FROM meter_rows WHERE batch IS NULL ORDER BY ref LIMIT ?)",
                    vec![SqlStorageValue::Integer(id), SqlStorageValue::Integer(BATCH_ROWS)],
                )?;
                Waiting { id, sent_at: None }
            }
        };
        if batch.sent_at.is_some_and(|at| now - at < RESEND_AFTER_MS) {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct Row {
            row: String,
        }
        // the same rows in the same order each time: the ledger knows a resend by its digest
        let rows: Vec<Row> = self.typed("SELECT row FROM meter_rows WHERE batch = ? ORDER BY ref", vec![SqlStorageValue::Integer(batch.id)])?;
        let rows: Vec<MeterRow> = rows.iter().map(|r| serde_json::from_str(&r.row)).collect::<Result<_, _>>().map_err(|e| CellError::host(format!("a meter row: {e}")))?;
        assert!(!rows.is_empty() && rows.len() <= BATCH_ROWS as usize, "a batch holds 1 to BATCH_ROWS rows");
        let message = MeterBatch { payer: self.must(MetaKey::Owner)?, source: self.name()?, seq: batch.id, meter: Meter { batch: format!("frag:{key}:{}", batch.id), rows } };
        let body = serde_json::to_value(&message).expect("a batch serializes");
        match js::queue_send(self.env.as_ref(), QUEUE_BINDING, &[body]).await {
            Ok(()) => self.exec("UPDATE meter_batches SET sent_at = ? WHERE id = ?", vec![SqlStorageValue::Integer(now), SqlStorageValue::Integer(batch.id)]),
            // tried again from the next alarm: the batch keeps its rows
            Err(e) => {
                console_error!("{}", json!({ "event": "meter.deferred", "batch": message.meter.batch, "message": e.message }));
                Ok(())
            }
        }
    }

    /// When the outbox next needs the alarm: the end of the oldest minute
    /// counted, now for rows not yet in a batch (when none waits), or a
    /// waiting batch's resend.
    pub(crate) fn meter_due_at(&self) -> CellResult<Option<i64>> {
        let rows = self.rows(
            "SELECT MIN(at) AS at FROM (
               SELECT (MIN(minute) + 1) * ? AS at FROM meter_requests
               UNION ALL SELECT ? AS at WHERE EXISTS (SELECT 1 FROM meter_rows WHERE batch IS NULL) AND NOT EXISTS (SELECT 1 FROM meter_batches)
               UNION ALL SELECT COALESCE(MIN(sent_at) + ?, ?) AS at FROM meter_batches HAVING COUNT(*) > 0)",
            vec![
                SqlStorageValue::Integer(MINUTE_MS),
                SqlStorageValue::Integer(js::now_ms()),
                SqlStorageValue::Integer(RESEND_AFTER_MS),
                SqlStorageValue::Integer(js::now_ms()),
            ],
        )?;
        Ok(rows.first().and_then(|r| r["at"].as_i64()))
    }

    /// Test fleets: the outbox as it stands (`/test/fragment meter`).
    pub(crate) fn meter_state(&self) -> CellResult<Value> {
        let counted = self.rows("SELECT minute, count FROM meter_requests ORDER BY minute", vec![])?;
        #[derive(Deserialize)]
        struct Row {
            r#ref: String,
            row: String,
            batch: Option<i64>,
        }
        let rows: Vec<Row> = self.typed("SELECT ref, row, batch FROM meter_rows ORDER BY ref", vec![])?;
        let rows: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let row: MeterRow = serde_json::from_str(&r.row).expect("a meter row is JSON the fragment wrote");
                json!({ "ref": r.r#ref, "batch": r.batch, "usage": row.usage })
            })
            .collect();
        let batches = self.rows("SELECT id, sent_at FROM meter_batches ORDER BY id", vec![])?;
        Ok(json!({ "counted": counted, "rows": rows, "batches": batches, "key": self.meter_key()? }))
    }

    /// `POST meter/acked`: the consumer's word on batch `seq`. Its rows go
    /// (the ledger has them, or never takes them), and the next batch is
    /// due. A batch of another life of this name, or one acknowledged
    /// before, changes nothing.
    pub(crate) async fn meter_acked(&self, acked: &Acked) -> CellResult<Value> {
        let key = self.meter_key()?;
        if acked.batch != format!("frag:{key}:{}", acked.seq) {
            return Ok(json!({ "acked": 0, "why": "another life's batch" }));
        }
        self.test_countdown(MetaKey::TestFailMeterAcks, "the batch's acknowledgement was lost")?;
        let rows = self.count_of("SELECT COUNT(*) AS n FROM meter_rows WHERE batch = ?", vec![SqlStorageValue::Integer(acked.seq)])?;
        self.exec("DELETE FROM meter_rows WHERE batch = ?", vec![SqlStorageValue::Integer(acked.seq)])?;
        self.exec("DELETE FROM meter_batches WHERE id = ?", vec![SqlStorageValue::Integer(acked.seq)])?;
        match (&acked.metered, &acked.refused) {
            (Some(m), _) if !m.refused.is_empty() => {
                self.event("meter.rows-refused", &format!("the owner's ledger refused {} of batch {}'s rows", m.refused.len(), acked.batch), json!({ "batch": acked.batch, "refused": m.refused }))
            }
            (None, Some(why)) => self.event("meter.refused", &format!("the owner's ledger takes no batch {}: {why}", acked.batch), json!({ "batch": acked.batch })),
            _ => {}
        }
        self.schedule().await?;
        Ok(json!({ "acked": rows }))
    }

    /// `PUT /api/f/<name>/cap {id, micros}` (the owner): the fragment's
    /// monthly cap on its owner's ledger (decision 26), `null` for the
    /// default. Its `id` makes the change once; the ledger keeps it under
    /// `cap:<fragment>:<id>`, apart from every other command's.
    pub(crate) async fn put_cap(&self, caller: &Caller, body: PutCap) -> CellResult<Response> {
        self.require(caller, false, Role::Owner)?;
        if !fragment_core::price::printable(&body.id, CAP_ID_MAX_BYTES) {
            return Err(CellError::invalid(format!("a cap's id is 1-{CAP_ID_MAX_BYTES} printable ASCII characters, no spaces")));
        }
        let (name, owner) = (self.name()?, self.must(MetaKey::Owner)?);
        let set = SetFragmentCap { id: format!("cap:{name}:{}", body.id), fragment: name.clone(), micros: body.micros };
        ledger::ask(&self.env, &owner, &set).await?;
        self.event("cap.set", &format!("{name}'s monthly cap: {}", body.micros.map_or_else(|| "the default".into(), fragment_core::price::dollars)), json!({ "micros": body.micros }));
        json_response(&json!({ "fragment": name, "capMicros": body.micros.unwrap_or(fragment_core::ledger::CAP_DEFAULT), "default": body.micros.is_none() }))
    }

    /// `POST meter/whose {principal}`: the fragment's owner, and whether
    /// `principal` is a member (the model route: a call names a fragment its
    /// agent is in).
    pub(crate) fn whose(&self, principal: &str) -> CellResult<Value> {
        let owner = self.must(MetaKey::Owner)?;
        Ok(json!({ "owner": owner, "member": self.member_role(principal)?.is_some() }))
    }

    /// Whether the fragment takes writes now (decision 27): refused, 402,
    /// once its owner's balance is past the overdraft, until a top-up
    /// brings it above zero. A guest's fragments are billed nothing and
    /// take writes.
    pub(crate) async fn writable(&self) -> CellResult<()> {
        match self.read_only().await? {
            None => Ok(()),
            Some(why) => Err(read_only_refusal(why)),
        }
    }

    /// Why the owner's fragments are read-only, when they are: what the
    /// fragment last heard (`STANDING_CACHE_MS`), or the owner's ledger
    /// now. A fresh answer waits on nothing, so a caller that asked a
    /// moment before (a write's check) reads it within its own turn. A
    /// ledger that does not answer says none: writes are the product, and
    /// an outage costs cents at most; that answer is not kept, so the next
    /// question asks again.
    pub(crate) async fn read_only(&self) -> CellResult<Option<Why>> {
        let now = js::now_ms();
        let seen = *self.standing.borrow();
        if let Some((at, why)) = seen.filter(|(at, _)| now - at < STANDING_CACHE_MS) {
            assert!(at <= now, "a standing is heard before it is read");
            return Ok(why);
        }
        let owner = self.must(MetaKey::Owner)?;
        let asked = ledger::ask(&self.env, &owner, &MaySpend { spend: Spend::Write, fragment: None, by_owner: true }).await;
        let why = match asked {
            Ok(_) => None,
            Err(e) => match e.refused {
                Some(Refused::ReadOnly { why }) => Some(why),
                Some(_) => None,
                None => {
                    console_error!("{}", json!({ "event": "meter.standing-unknown", "message": e.message }));
                    return Ok(None);
                }
            },
        };
        *self.standing.borrow_mut() = Some((now, why));
        Ok(why)
    }
}

/// A write refused past the owner's overdraft, saying why.
pub(crate) fn read_only_refusal(why: Why) -> CellError {
    CellError::new(ErrorCode::BudgetUsedUp, format!("this fragment's owner's {}", Refused::ReadOnly { why }.message()))
}

/// The ledger queue's consumer: each batch applied on its payer's ledger,
/// then its fragment told. A ledger that does not answer, or a fragment
/// that does not hear, gets the message again; a refusal is final (a
/// guest's fragments are billed nothing; a batch the ledger refuses as
/// another body is a source's bug, kept in its event log).
pub async fn consume(batch: MessageBatch<Value>, env: Env) -> Result<()> {
    let messages: Vec<RawMessage> = batch.raw_iter().collect();
    assert!(messages.len() <= CONSUME_BATCH_MAX, "a ledger batch holds at most {CONSUME_BATCH_MAX} messages, not {}", messages.len());
    futures_util::future::join_all(messages.into_iter().map(|m| consume_one(m, &env))).await;
    Ok(())
}

async fn consume_one(raw: RawMessage, env: &Env) {
    let message = match Message::<MeterBatch>::try_from(raw) {
        Ok(m) => m,
        Err(e) => {
            // not one a fragment sent: nothing can take it
            console_error!("{}", json!({ "event": "meter.unreadable", "message": e.to_string() }));
            return;
        }
    };
    let b = message.body();
    let (metered, refused) = match ledger::ask(env, &b.payer, &b.meter).await {
        Ok(metered) => (Some(metered), None),
        Err(e) if e.refused.is_some() => (None, Some(e.message)),
        Err(_) => {
            message.retry_with_options(&QueueRetryOptionsBuilder::new().with_delay_seconds(RETRY_DELAY_S).build());
            return;
        }
    };
    let acked = Acked { seq: b.seq, batch: b.meter.batch.clone(), metered, refused };
    let body = serde_json::to_string(&acked).expect("an acknowledgement serializes");
    let heard = async {
        let req = routed::internal_request("meter/acked", &body)?;
        let resp = env.durable_object("FRAGMENT")?.get_by_name(&b.source)?.fetch_with_request(req).await?;
        Ok::<u16, CellError>(resp.status_code())
    };
    match heard.await {
        // a fragment deleted since has nothing to forget
        Ok(200 | 404) => message.ack(),
        _ => message.retry_with_options(&QueueRetryOptionsBuilder::new().with_delay_seconds(RETRY_DELAY_S).build()),
    }
}
