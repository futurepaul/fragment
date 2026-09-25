//! Deliveries (phase 2 slice F): HTTP requests the platform sends on a
//! fragment's behalf, outside any request: records to channel
//! subscribers, web push, and `notifyUrls`. Each is first written to the
//! fragment's delivery outbox (`delivery_outbox`), in the same turn as
//! what caused it: the record, or the push effect as it is accepted. The
//! outbox is drained right away and again from the alarm: each delivery is
//! built whole (a push is encrypted and signed for its browser) and put on
//! the `fragment-deliveries` queue, and its row goes once the queue has it,
//! so a failure between the record and the queue delays a delivery but
//! never loses it. Receivers see a delivery at least once. The consumer
//! here, a stateless isolate, sends it. A push service that answers 404 or
//! 410 has dropped the subscription, so the fragment drops it too; a 429,
//! 5xx, or network failure is retried with a delay that grows with the
//! message's age; one that runs out of retries lands on the dead-letter
//! queue and in the fragment's event log.

use std::time::Duration;

use fragment_proto::limits;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::cs::FetchError;
use crate::error::{CellError, CellResult};
use crate::fragment::{FragmentCell, MetaKey};
use crate::js;

pub const DEAD_QUEUE: &str = "fragment-deliveries-dead";
/// Set by the consumer on its reports; the router never sets or passes it.
pub const REPORT_HEADER: &str = "x-fragment-delivery";
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_MAX_S: u32 = 3600;
/// Outbox rows one drain takes.
const DRAIN_ROWS: i64 = 100;
/// Messages per `sendBatch` (the queue's cap), and so per push batch.
const QUEUE_BATCH: usize = 100;
/// Batches one push may take: its subscriptions are capped.
const PUSH_BATCHES_MAX: usize = limits::PUSH_SUBS_MAX as usize / QUEUE_BATCH + 1;
/// How long a drain holds the rows it is sending: another drain (a
/// request's, the alarm's) skips them meanwhile.
const CLAIM_MS: i64 = 60_000;
/// Tries at queuing one row before it is dropped, with an event: with the
/// wait doubling to ten minutes, about two and a half hours of a queue
/// that will not take it.
const OUTBOX_ATTEMPTS_MAX: i64 = 20;

const _: () = assert!(QUEUE_BATCH * (PUSH_BATCHES_MAX - 1) >= limits::PUSH_SUBS_MAX as usize, "a push's batches cover every subscription");

/// One drain's failures. The first asks once whether any row already
/// waited: if none did, an outage begins here and `delivery.deferred`
/// says so (one event an outage, and one indexed lookup a drain, never a
/// count of the outbox for each row that fails).
#[derive(Default)]
struct Failures {
    seen: bool,
}

/// One outbox row, decoded once.
enum Pending {
    /// Record `seq` of `channel`, to the channel subscription `sub`.
    Record { sub: i64, channel: String, seq: i64 },
    Push(PendingPush),
    /// A `changed` frame to one of `notifyUrls`.
    Notify { url: String, frame: String },
}

/// A push to the subscriptions tagged `who` (`*`: all) with ids in
/// (`after`, `upto`]: `after` moves as each batch is queued.
struct PendingPush {
    who: String,
    payload: String,
    after: i64,
    upto: i64,
}

/// What a delivery carries: a record to a channel subscriber, a web push,
/// or a `changed` frame to one of `notifyUrls`. The outbox's `kind` column,
/// a queued delivery, and the consumer's report all name it this way.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKind {
    Record,
    Push,
    Notify,
}

impl DeliveryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryKind::Record => "record",
            DeliveryKind::Push => "push",
            DeliveryKind::Notify => "notify",
        }
    }

    fn parse(s: &str) -> Option<DeliveryKind> {
        [DeliveryKind::Record, DeliveryKind::Push, DeliveryKind::Notify].into_iter().find(|k| k.as_str() == s)
    }
}

/// Only this module and its callers write the outbox: a row that does not
/// decode is corruption, not input.
fn decode(row: &Value) -> Pending {
    let text = |k: &str| row[k].as_str().unwrap_or_else(|| panic!("a delivery_outbox row has no {k}: {row}")).to_string();
    let int = |k: &str| row[k].as_i64().unwrap_or_else(|| panic!("a delivery_outbox row has no {k}: {row}"));
    let kind = row["kind"].as_str().and_then(DeliveryKind::parse);
    match kind.unwrap_or_else(|| panic!("a delivery_outbox row of kind {}", row["kind"])) {
        DeliveryKind::Record => Pending::Record { sub: int("sub"), channel: text("channel"), seq: int("seq") },
        DeliveryKind::Push => Pending::Push(PendingPush { who: text("who"), payload: text("body"), after: int("after_sub"), upto: int("upto_sub") }),
        DeliveryKind::Notify => Pending::Notify { url: text("url"), frame: text("body") },
    }
}

/// One request, ready to send.
#[derive(Serialize, Deserialize, Clone)]
pub struct Delivery {
    pub fragment: String,
    pub incarnation: String,
    pub kind: DeliveryKind,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// base64
    pub body: String,
    /// The push or channel subscription it goes to (dropped on 404 or 410).
    #[serde(default)]
    pub sub: Option<i64>,
}

impl FragmentCell {
    /// Puts deliveries on the queue (at most `QUEUE_BATCH` a call).
    async fn enqueue(&self, deliveries: &[Delivery]) -> CellResult<()> {
        assert!(deliveries.len() <= QUEUE_BATCH, "a queue send takes at most {QUEUE_BATCH} messages");
        if deliveries.is_empty() {
            return Ok(());
        }
        if self.cfg.test_hooks {
            let failures: i64 = self.meta(MetaKey::TestFailDeliveries)?.and_then(|n| n.parse().ok()).unwrap_or(0);
            if failures > 0 {
                self.set_meta(MetaKey::TestFailDeliveries, &(failures - 1).to_string())?;
                return Err(CellError::host("the queue send failed (a test hook)"));
            }
        }
        let bodies: Vec<Value> = deliveries.iter().map(|d| serde_json::to_value(d).expect("a delivery serializes")).collect();
        js::queue_send(self.env.as_ref(), "DELIVERIES", &bodies).await
    }

    /// When the outbox next has a row due (for the alarm): the MIN of the
    /// `next_at` index, not a scan.
    pub(crate) fn outbox_due_at(&self) -> CellResult<Option<i64>> {
        Ok(self.rows("SELECT MIN(next_at) AS at FROM delivery_outbox", vec![])?.first().and_then(|r| r["at"].as_i64()))
    }

    /// Sends the outbox's due rows: each delivery built whole and queued,
    /// its row gone once the queue has it. A row the queue did not take
    /// waits, doubling, and is tried again from the alarm; after
    /// `OUTBOX_ATTEMPTS_MAX` tries it is dropped, with a `delivery.failed`
    /// event (never silently).
    pub(crate) async fn drain_deliveries(&self) {
        let now = js::now_ms();
        // claim the due rows in one statement: another drain skips them
        let Ok(mut rows) = self.rows(
            "UPDATE delivery_outbox SET next_at = ? WHERE id IN (SELECT id FROM delivery_outbox WHERE next_at <= ? ORDER BY id LIMIT ?) RETURNING *",
            vec![SqlStorageValue::Integer(now + CLAIM_MS), SqlStorageValue::Integer(now), SqlStorageValue::Integer(DRAIN_ROWS)],
        ) else {
            return;
        };
        if rows.is_empty() {
            return;
        }
        rows.sort_by_key(|r| r["id"].as_i64());
        let (Ok(fragment), Ok(incarnation)) = (self.must(MetaKey::Name), self.must(MetaKey::CreatedAt)) else { return };
        let mut failures = Failures::default();
        // records and frames go a queue batch at a time; each push goes on its own
        let mut singles: Vec<(i64, i64, Delivery)> = vec![];
        for row in &rows {
            let (id, attempts) = (row["id"].as_i64().expect("delivery_outbox.id"), row["attempts"].as_i64().expect("delivery_outbox.attempts"));
            match decode(row) {
                Pending::Record { sub, channel, seq } => match self.record_delivery(sub, &channel, seq, &fragment, &incarnation) {
                    Ok(Some(d)) => singles.push((id, attempts, d)),
                    Ok(None) => self.outbox_done(id),
                    Err(e) => self.outbox_failed(&mut failures, id, attempts, &e.message),
                },
                Pending::Notify { url, frame } => {
                    let body = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, frame);
                    let headers = vec![("content-type".into(), "application/json".into())];
                    let d = Delivery { fragment: fragment.clone(), incarnation: incarnation.clone(), kind: DeliveryKind::Notify, url, headers, body, sub: None };
                    singles.push((id, attempts, d));
                }
                Pending::Push(push) => self.drain_push(&mut failures, id, attempts, push, &fragment, &incarnation).await,
            }
        }
        let mut chunks = singles.chunks(QUEUE_BATCH);
        for chunk in chunks.by_ref() {
            let deliveries: Vec<Delivery> = chunk.iter().map(|(_, _, d)| d.clone()).collect();
            match self.enqueue(&deliveries).await {
                Ok(()) => chunk.iter().for_each(|(id, ..)| self.outbox_done(*id)),
                Err(e) => {
                    chunk.iter().for_each(|(id, attempts, _)| self.outbox_failed(&mut failures, *id, *attempts, &e.message));
                    break;
                }
            }
        }
        // after a failure the rest wait too: the queue is refusing
        for chunk in chunks {
            chunk.iter().for_each(|(id, attempts, _)| self.outbox_failed(&mut failures, *id, *attempts, "an earlier batch was refused"));
        }
        if failures.seen || rows.len() as i64 == DRAIN_ROWS {
            let _ = self.schedule().await;
        }
    }

    /// One push row: its subscriptions a batch at a time, the cursor
    /// moving as each batch is queued, so a failure re-sends none of the
    /// batches before it. A row not done is in `failures`.
    async fn drain_push(&self, failures: &mut Failures, id: i64, attempts: i64, push: PendingPush, fragment: &str, incarnation: &str) {
        let PendingPush { who, payload, mut after, upto } = push;
        let vapid = match self.vapid().await {
            Ok(v) => v,
            Err(e) => return self.outbox_failed(failures, id, attempts, &e.message),
        };
        for _ in 0..PUSH_BATCHES_MAX {
            let subs = match self.rows(
                "SELECT id, endpoint, p256dh, auth FROM push_subs WHERE id > ? AND id <= ? AND (? = '*' OR who = ?) ORDER BY id LIMIT ?",
                vec![SqlStorageValue::Integer(after), SqlStorageValue::Integer(upto), who.as_str().into(), who.as_str().into(), SqlStorageValue::Integer(QUEUE_BATCH as i64)],
            ) {
                Ok(s) => s,
                Err(e) => return self.outbox_failed(failures, id, attempts, &e.message),
            };
            let Some(last) = subs.last().map(|s| s["id"].as_i64().expect("push_subs.id")) else {
                return self.outbox_done(id);
            };
            assert!(last > after, "a push's cursor moves forward");
            let deliveries = self.push_deliveries(&vapid, &subs, &payload, fragment, incarnation);
            if let Err(e) = self.enqueue(&deliveries).await {
                return self.outbox_failed(failures, id, attempts, &e.message);
            }
            after = last;
            let _ = self.exec("UPDATE delivery_outbox SET after_sub = ? WHERE id = ?", vec![SqlStorageValue::Integer(after), SqlStorageValue::Integer(id)]);
        }
        panic!("a push took more than {PUSH_BATCHES_MAX} batches of {QUEUE_BATCH}: its subscriptions are capped at {}", limits::PUSH_SUBS_MAX);
    }

    fn outbox_done(&self, id: i64) {
        let _ = self.exec("DELETE FROM delivery_outbox WHERE id = ?", vec![SqlStorageValue::Integer(id)]);
    }

    /// A row the queue did not take: it waits, or after its last try it
    /// goes, with an event. The drain's first failure asks, before it
    /// writes, whether any row waits already (the partial index of waiting
    /// rows: one lookup, not a scan); when none does, this row's first
    /// failure begins an outage and says so once (an outage is one event,
    /// not one per delivery).
    fn outbox_failed(&self, failures: &mut Failures, id: i64, attempts: i64, why: &str) {
        let begins_outage = !failures.seen && self.rows("SELECT id FROM delivery_outbox WHERE attempts > 0 LIMIT 1", vec![]).is_ok_and(|w| w.is_empty());
        failures.seen = true;
        let attempts = attempts + 1;
        if attempts >= OUTBOX_ATTEMPTS_MAX {
            self.outbox_done(id);
            self.event("delivery.failed", &format!("a delivery the queue did not take in {attempts} tries was dropped: {why}"), json!({ "outbox": id }));
            return;
        }
        let _ = self.exec(
            "UPDATE delivery_outbox SET attempts = ?, next_at = ? WHERE id = ?",
            vec![SqlStorageValue::Integer(attempts), SqlStorageValue::Integer(js::now_ms() + fragment_core::backoff::outbox_retry_ms(attempts)), SqlStorageValue::Integer(id)],
        );
        if attempts == 1 && begins_outage {
            self.event("delivery.deferred", &format!("deliveries wait in the outbox: {why}"), json!({ "outbox": id }));
        }
    }

    /// `POST /deliver/report` from the consumer: a subscription is gone, or
    /// a delivery failed for good.
    pub(crate) fn delivery_report(&self, report: &Report) -> CellResult<Value> {
        if Some(report.incarnation.as_str()) != self.meta(MetaKey::CreatedAt)?.as_deref() {
            return Ok(json!({ "ok": true }));
        }
        let (kind, url) = (report.kind.as_str(), report.url.as_str());
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        match report.outcome {
            Outcome::Gone => {
                let (table, event) = match report.kind {
                    DeliveryKind::Record => ("subs", "subscription.gone"),
                    DeliveryKind::Push => ("push_subs", "push.gone"),
                    // a notify URL is no subscription; `send` never reports one gone
                    DeliveryKind::Notify => return Err(CellError::invalid("a notifyUrls delivery has no subscription to drop")),
                };
                let sub = report.sub.ok_or_else(|| CellError::invalid(format!("a {kind} delivery reported gone names its subscription")))?;
                self.exec(&format!("DELETE FROM {table} WHERE id = ?"), vec![SqlStorageValue::Integer(sub)])?;
                self.event(event, &format!("a subscription at {host} is gone; dropped"), json!({ "sub": sub }));
            }
            Outcome::Failed => self.event("delivery.failed", &format!("{kind} to {host}: {}", report.error), json!({ "kind": kind, "status": report.status })),
        }
        Ok(json!({ "ok": true }))
    }
}

/// What the consumer tells a fragment about one of its deliveries
/// (`POST /deliver/report`).
#[derive(Serialize, Deserialize)]
pub(crate) struct Report {
    incarnation: String,
    kind: DeliveryKind,
    url: String,
    sub: Option<i64>,
    outcome: Outcome,
    /// The receiver's answer (0: none came).
    status: u16,
    error: String,
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    /// The receiver dropped the subscription (404 or 410): the fragment drops it too.
    Gone,
    /// The delivery failed for good.
    Failed,
}

async fn report(env: &Env, d: &Delivery, outcome: Outcome, status: u16, error: &str) -> Result<()> {
    let body = Report { incarnation: d.incarnation.clone(), kind: d.kind, url: d.url.clone(), sub: d.sub, outcome, status, error: error.to_string() };
    let body = serde_json::to_string(&body).map_err(|e| Error::RustError(e.to_string()))?;
    // marked as the supervisor's `deliver/report` route expects (routed.rs)
    let req = crate::routed::internal_request("deliver/report", &body).map_err(|e| Error::RustError(e.message))?;
    env.durable_object("FRAGMENT")?.get_by_name(&d.fragment)?.fetch_with_request(req).await?;
    Ok(())
}

/// Sends one delivery: `Ok(None)` done, `Ok(Some(why))` worth retrying.
async fn send(env: &Env, d: &Delivery) -> Result<Option<String>> {
    use base64::Engine;
    let body = base64::engine::general_purpose::STANDARD.decode(&d.body).unwrap_or_default();
    let headers = Headers::new();
    for (k, v) in &d.headers {
        headers.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(js_sys::Uint8Array::from(body.as_slice()).into()));
    let req = Request::new_with_init(&d.url, &init)?;
    let resp = match crate::cs::fetch(req, SEND_TIMEOUT).await {
        Ok(r) => r,
        Err(FetchError::Failed(why)) => return Ok(Some(why)),
        // the node refused the address: no retry passes
        Err(FetchError::Refused(why)) => {
            report(env, d, Outcome::Failed, 0, &why).await?;
            return Ok(None);
        }
    };
    let status = resp.status_code();
    Ok(match status {
        200..=299 => None,
        404 | 410 if matches!(d.kind, DeliveryKind::Push | DeliveryKind::Record) => {
            report(env, d, Outcome::Gone, status, "").await?;
            None
        }
        429 | 500..=599 => Some(format!("answered {status}")),
        _ => {
            report(env, d, Outcome::Failed, status, &format!("answered {status}")).await?;
            None
        }
    })
}

/// The queue consumer: `fragment-deliveries` sends, its dead-letter queue reports.
pub async fn consume(batch: MessageBatch<Delivery>, env: Env) -> Result<()> {
    let cfg = Config::from_env(&env);
    let dead = batch.queue() == DEAD_QUEUE;
    for message in batch.messages()? {
        let d = message.body().clone();
        if dead {
            let _ = report(&env, &d, Outcome::Failed, 0, "out of retries").await;
            message.ack();
            continue;
        }
        match send(&env, &d).await {
            Ok(None) => message.ack(),
            Ok(Some(_)) | Err(_) => {
                // no attempt count in workers-rs 0.8.5: the delay grows with the message's age
                let age_s = ((js::now_ms() - message.timestamp().as_millis() as i64) / 1000).max(0) as u32;
                let delay = age_s.clamp(cfg.delivery_retry_s, RETRY_MAX_S);
                message.retry_with_options(&QueueRetryOptionsBuilder::new().with_delay_seconds(delay).build());
            }
        }
    }
    Ok(())
}
