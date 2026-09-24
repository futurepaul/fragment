//! Deliveries (phase 2 slice F): HTTP requests the platform sends on a
//! fragment's behalf, outside any request: web push and `notifyUrls`. The
//! supervisor builds each request whole (a push is encrypted and signed
//! before it is queued) and puts it on the `fragment-deliveries` queue;
//! the consumer here, a stateless isolate, sends it. A push service that
//! answers 404 or 410 has dropped the subscription, so the fragment drops
//! it too; a 429, 5xx, or network failure is retried with a delay that
//! grows with the message's age; one that runs out of retries lands on
//! the dead-letter queue and in the fragment's event log.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::cs::FetchError;
use crate::error::CellResult;
use crate::fragment::FragmentCell;
use crate::js;

pub const DEAD_QUEUE: &str = "fragment-deliveries-dead";
/// Set by the consumer on its reports; the router never sets or passes it.
pub const REPORT_HEADER: &str = "x-fragment-delivery";
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_MAX_S: u32 = 3600;

/// One request, ready to send.
#[derive(Serialize, Deserialize, Clone)]
pub struct Delivery {
    pub fragment: String,
    pub incarnation: String,
    /// `push` or `notify`
    pub kind: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// base64
    pub body: String,
    /// The push or channel subscription it goes to (dropped on 404 or 410).
    #[serde(default)]
    pub sub: Option<i64>,
}

impl FragmentCell {
    /// Queues deliveries (at most 100 a call to the queue).
    pub(crate) async fn enqueue(&self, deliveries: Vec<Delivery>) -> CellResult<usize> {
        let bodies: Vec<Value> = deliveries.iter().map(|d| serde_json::to_value(d).expect("a delivery serializes")).collect();
        js::queue_send(self.env.as_ref(), "DELIVERIES", &bodies).await?;
        Ok(bodies.len())
    }

    /// `POST /deliver/report` from the consumer: a subscription is gone, or
    /// a delivery failed for good.
    pub(crate) fn delivery_report(&self, body: &Value) -> CellResult<Value> {
        if body["incarnation"].as_str() != self.meta("created_at")?.as_deref() {
            return Ok(json!({ "ok": true }));
        }
        let (kind, url) = (body["kind"].as_str().unwrap_or(""), body["url"].as_str().unwrap_or(""));
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
        match body["outcome"].as_str() {
            Some("gone") => {
                if let Some(sub) = body["sub"].as_i64() {
                    let (table, event) = if kind == "record" { ("subs", "subscription.gone") } else { ("push_subs", "push.gone") };
                    self.exec(&format!("DELETE FROM {table} WHERE id = ?"), vec![SqlStorageValue::Integer(sub)])?;
                    self.event(event, &format!("a subscription at {host} is gone; dropped"), json!({ "sub": sub }));
                }
            }
            _ => self.event(
                "delivery.failed",
                &format!("{kind} to {host}: {}", body["error"].as_str().unwrap_or("failed")),
                json!({ "kind": kind, "status": body["status"] }),
            ),
        }
        Ok(json!({ "ok": true }))
    }
}

async fn report(env: &Env, d: &Delivery, outcome: &str, status: u16, error: &str) -> Result<()> {
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    headers.set(REPORT_HEADER, "1")?;
    headers.set(crate::fragment::NAME_HEADER, &d.fragment)?;
    headers.set(crate::fragment::URL_HEADER, "https://fragment.internal/")?;
    let body = json!({ "incarnation": d.incarnation, "kind": d.kind, "url": d.url, "sub": d.sub, "outcome": outcome, "status": status, "error": error });
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.to_string().into()));
    let req = Request::new_with_init("https://fragment.internal/deliver/report", &init)?;
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
            report(env, d, "failed", 0, &why).await?;
            return Ok(None);
        }
    };
    let status = resp.status_code();
    Ok(match status {
        200..=299 => None,
        404 | 410 if d.kind == "push" || d.kind == "record" => {
            report(env, d, "gone", status, "").await?;
            None
        }
        429 | 500..=599 => Some(format!("answered {status}")),
        _ => {
            report(env, d, "failed", status, &format!("answered {status}")).await?;
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
            let _ = report(&env, &d, "failed", 0, "out of retries").await;
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
