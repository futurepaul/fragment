//! Web push (phase 2 slice F; the old runtime's contract, on the queue).
//! A page subscribes through the browser library (`fragment.push.register
//! (who)`), which registers `__sw.js`, reads the fragment's VAPID key
//! (`__push-key`), and stores the subscription (`__push-sub`), tagged with
//! a `who` of the page's choosing. `call.push(who, payload)` in a mutation
//! and `job.push(who, payload)` in a job send to that tag's subscriptions
//! (`*`: all): the push is written to the delivery outbox as it is
//! accepted, and as the outbox drains, each payload is encrypted for its
//! browser and signed with the fragment's VAPID key here, then queued
//! (`deliveries.rs`).

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use fragment_core::webpush::{self, Subscription, Vapid};
use fragment_core::egress;
use fragment_proto::{limits, ErrorCode};
use serde_json::{json, Value};
use worker::*;

use crate::deliveries::{Delivery, DeliveryKind};
use crate::error::{CellError, CellResult};
use crate::fragment::FragmentCell;
use crate::keys;
use crate::js;

/// How long a push service keeps a message for an offline browser.
const PUSH_TTL_S: &str = "86400";
const ENDPOINT_MAX_BYTES: usize = 1024;

pub const SW_JS: &str = include_str!("../sw.js");

impl FragmentCell {
    /// The fragment's VAPID key, made on first use and sealed like a secret
    /// (`KEYS` opens it for this cell alone).
    pub(crate) async fn vapid(&self) -> CellResult<Vapid> {
        if self.meta("vapid")?.is_none() {
            let key = Vapid::draw(js::random_bytes);
            let sealed = keys::seal(&self.env, &key.to_bytes()).await?;
            // two first uses at once: the first stored wins, and both use it
            self.exec("INSERT INTO meta (key, value) VALUES ('vapid', ?) ON CONFLICT (key) DO NOTHING", vec![sealed.into()])?;
        }
        let sealed = self.must("vapid")?;
        let opened = keys::open(&self.env, &sealed, &self.must("npub")?).await.map_err(|e| CellError::host(format!("the VAPID key: {}", e.message)))?;
        if let Some(fresh) = opened.resealed {
            self.exec("UPDATE meta SET value = ? WHERE key = 'vapid' AND value = ?", vec![fresh.into(), sealed.into()])?;
        }
        let bytes: [u8; 32] = opened.plaintext.try_into().map_err(|_| CellError::host("a stored VAPID key is not 32 bytes"))?;
        Vapid::from_bytes(bytes).ok_or_else(|| CellError::host("a stored VAPID key is out of range"))
    }

    /// `GET __push-key`: the key a browser subscribes with.
    pub(crate) async fn push_key(&self) -> CellResult<Value> {
        Ok(json!({ "key": self.vapid().await?.public_key() }))
    }

    /// `POST __push-sub` `{who, endpoint, p256dh, auth}`: anyone who can see
    /// the fragment (the old runtime's body).
    pub(crate) fn push_subscribe(&self, body: &Value, principal: &str) -> CellResult<Value> {
        let endpoint = body["endpoint"].as_str().unwrap_or("");
        let who = body["who"].as_str().unwrap_or("");
        if endpoint.len() > ENDPOINT_MAX_BYTES || who.chars().count() > limits::PUSH_WHO_MAX_CHARS {
            return Err(CellError::invalid(format!("an endpoint is at most {ENDPOINT_MAX_BYTES} bytes, a who at most {} characters", limits::PUSH_WHO_MAX_CHARS)));
        }
        let url = egress::check(endpoint, self.cfg.egress_local).map_err(|e| CellError::invalid(format!("endpoint: {e}")))?;
        if url.scheme() != "https" && !self.cfg.egress_local {
            return Err(CellError::invalid("a push endpoint is https"));
        }
        let (p256dh, auth) = (body["p256dh"].as_str().unwrap_or(""), body["auth"].as_str().unwrap_or(""));
        webpush::subscription_keys(&Subscription { endpoint, p256dh, auth }).map_err(CellError::invalid)?;
        let exists = !self.rows("SELECT id FROM push_subs WHERE endpoint = ?", vec![endpoint.into()])?.is_empty();
        if !exists && self.count("SELECT COUNT(*) AS n FROM push_subs")? >= limits::PUSH_SUBS_MAX {
            return Err(CellError::new(ErrorCode::RateLimited, format!("a fragment holds at most {} push subscriptions", limits::PUSH_SUBS_MAX)));
        }
        self.exec(
            "INSERT INTO push_subs (endpoint, p256dh, auth, who, principal, created_at) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (endpoint) DO UPDATE SET p256dh = excluded.p256dh, auth = excluded.auth, who = excluded.who, principal = excluded.principal",
            vec![endpoint.into(), p256dh.into(), auth.into(), who.into(), principal.into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        Ok(json!({ "ok": true, "who": who }))
    }

    /// `POST __push-unsub` `{endpoint}`: the browser drops its own subscription.
    pub(crate) fn push_unsubscribe(&self, body: &Value) -> CellResult<Value> {
        let endpoint = body["endpoint"].as_str().unwrap_or("");
        let gone = self.rows("DELETE FROM push_subs WHERE endpoint = ? RETURNING id", vec![endpoint.into()])?;
        Ok(json!({ "ok": true, "removed": gone.len() }))
    }

    /// Accepts a push to every subscription tagged `who` (`*`: all), once
    /// per `key` (a mutation's effect or a job's step): written to the
    /// delivery outbox, then sent (deliveries.rs). Answers how many
    /// subscriptions it goes to.
    pub(crate) async fn send_push(&self, key: &str, who: &str, payload: &Value) -> CellResult<usize> {
        let n = self.outbox_push(key, who, payload)?;
        if n > 0 {
            self.drain_deliveries().await;
        }
        Ok(n)
    }

    /// The push's outbox row, in this turn: the subscriptions tagged `who`
    /// now (up to the newest id), each sent once however the sending goes.
    fn outbox_push(&self, key: &str, who: &str, payload: &Value) -> CellResult<usize> {
        let text = payload.to_string();
        if text.len() > webpush::PAYLOAD_MAX_BYTES {
            return Err(CellError::too_large("a push payload", text.len(), webpush::PAYLOAD_MAX_BYTES));
        }
        let now = SqlStorageValue::Integer(js::now_ms());
        let accepted = self.rows("INSERT INTO sent (key, at) VALUES (?, ?) ON CONFLICT (key) DO NOTHING RETURNING key", vec![key.into(), now.clone()])?;
        if accepted.is_empty() {
            return Ok(0);
        }
        let rows = self.rows(
            "SELECT COUNT(*) AS n, COALESCE(MAX(id), 0) AS upto FROM push_subs WHERE ? = '*' OR who = ?",
            vec![who.into(), who.into()],
        )?;
        let (n, upto) = match rows.first() {
            Some(r) => (r["n"].as_u64().expect("COUNT is an integer"), r["upto"].as_i64().expect("MAX(id) is an integer")),
            None => (0, 0),
        };
        if n > 0 {
            self.exec(
                "INSERT INTO delivery_outbox (kind, who, body, after_sub, upto_sub, next_at) VALUES ('push', ?, ?, 0, ?, ?)",
                vec![who.into(), text.into(), SqlStorageValue::Integer(upto), now],
            )?;
        }
        Ok(n as usize)
    }

    /// The deliveries of one push to `subs` (rows of `push_subs`), each
    /// encrypted for its browser and signed with the fragment's VAPID key.
    /// A subscription that no longer checks out is skipped, not fatal.
    pub(crate) fn push_deliveries(&self, vapid: &Vapid, subs: &[Value], payload: &str, fragment: &str, incarnation: &str) -> Vec<Delivery> {
        let now_s = js::now_ms() / 1000;
        let mut deliveries = Vec::with_capacity(subs.len());
        for s in subs {
            let sub = Subscription {
                endpoint: s["endpoint"].as_str().unwrap_or(""),
                p256dh: s["p256dh"].as_str().unwrap_or(""),
                auth: s["auth"].as_str().unwrap_or(""),
            };
            let Ok(auth) = vapid.authorization(sub.endpoint, &self.cfg.push_subject, now_s) else { continue };
            let ephemeral = webpush::Ephemeral::draw(js::random_bytes);
            let Ok(body) = webpush::encrypt(&sub, payload.as_bytes(), &ephemeral, js::random_bytes()) else { continue };
            deliveries.push(Delivery {
                fragment: fragment.to_string(),
                incarnation: incarnation.to_string(),
                kind: DeliveryKind::Push,
                url: sub.endpoint.to_string(),
                headers: vec![
                    ("content-type".into(), "application/octet-stream".into()),
                    ("content-encoding".into(), "aes128gcm".into()),
                    ("ttl".into(), PUSH_TTL_S.into()),
                    ("urgency".into(), "normal".into()),
                    ("authorization".into(), auth),
                ],
                body: B64.encode(body),
                sub: s["id"].as_i64(),
            });
        }
        deliveries
    }

    /// `notifyUrls`: a `changed` frame per move of `main`, as the old
    /// runtime sent, through the delivery outbox.
    pub(crate) async fn notify_urls(&self, sha: Option<&str>, paths: &[String]) -> CellResult<()> {
        let urls = self.notify_list()?;
        if urls.is_empty() {
            return Ok(());
        }
        let frame = json!({ "type": "changed", "fragment": self.must("name")?, "sha": sha, "paths": paths.iter().take(50).collect::<Vec<_>>() });
        let mut queued = 0;
        for url in urls {
            if egress::check(&url, self.cfg.egress_local).is_err() {
                self.event("notify.refused", &format!("{url}: not a public address"), Value::Null);
                continue;
            }
            self.exec(
                "INSERT INTO delivery_outbox (kind, url, body, next_at) VALUES ('notify', ?, ?, ?)",
                vec![url.into(), frame.to_string().into(), SqlStorageValue::Integer(js::now_ms())],
            )?;
            queued += 1;
        }
        if queued > 0 {
            self.drain_deliveries().await;
        }
        Ok(())
    }

    fn notify_list(&self) -> CellResult<Vec<String>> {
        let rows = self.rows("SELECT notify FROM code WHERE id = 1", vec![])?;
        Ok(rows.first().and_then(|r| r["notify"].as_str()).and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default())
    }
}
