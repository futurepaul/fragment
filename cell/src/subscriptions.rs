//! Channel subscriptions (phase 7): a member asks the fragment to deliver
//! each new record of a channel it may read to a URL, such as an agent's
//! inbox (MODEL.md, Agents: "the agent member subscribes"). Records go out
//! through the delivery queue (deliveries.rs) as `{type: "record",
//! fragment, channel, record}`, unsigned: the URL is the subscriber's
//! capability. A member's subscriptions end with its membership.

use fragment_core::{egress, npub};
use fragment_proto::{valid_channel_name, ChannelRecord, ErrorCode};
use serde_json::{json, Value};
use worker::*;

use crate::deliveries::Delivery;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};

/// The subscriptions one fragment holds, and a URL's length.
const SUBS_MAX: u64 = 32;
const SUB_URL_MAX_BYTES: usize = 1024;

impl FragmentCell {
    /// The subscriber: a member whose role may read `channel`.
    fn subscriber(&self, caller: &Caller, channel: &str) -> CellResult<String> {
        let principal = caller.principal.clone().ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "subscribing needs a signed request"))?;
        let Some(role) = self.member_role(&principal)? else {
            return Err(CellError::new(ErrorCode::Forbidden, "only a member subscribes to a channel"));
        };
        if !valid_channel_name(channel) {
            return Err(CellError::invalid("a channel name must match ^[a-z][a-z0-9_-]{0,63}$"));
        }
        if role < self.channel_read_role(channel)? {
            return Err(CellError::new(ErrorCode::Forbidden, format!("your role may not read {channel}")));
        }
        Ok(principal)
    }

    /// `POST /api/subscriptions` `{channel, url}` → `{id, channel, url}`.
    pub(crate) fn subscribe(&self, caller: &Caller, body: &Value) -> CellResult<Response> {
        let channel = body["channel"].as_str().unwrap_or("");
        let principal = self.subscriber(caller, channel)?;
        let url = body["url"].as_str().unwrap_or("");
        if url.len() > SUB_URL_MAX_BYTES {
            return Err(CellError::invalid(format!("a subscription URL is at most {SUB_URL_MAX_BYTES} bytes")));
        }
        egress::check(url, self.cfg.egress_local).map_err(|e| CellError::invalid(format!("url: {e}")))?;
        let existing = self.rows("SELECT id FROM subs WHERE principal = ? AND channel = ? AND url = ?", vec![principal.as_str().into(), channel.into(), url.into()])?;
        let id = match existing.first().and_then(|r| r["id"].as_i64()) {
            Some(id) => id,
            None => {
                if self.count("SELECT COUNT(*) AS n FROM subs")? >= SUBS_MAX {
                    return Err(CellError::new(ErrorCode::RateLimited, format!("a fragment holds at most {SUBS_MAX} subscriptions")));
                }
                let rows = self.rows(
                    "INSERT INTO subs (principal, channel, url, created_at) VALUES (?, ?, ?, ?) RETURNING id",
                    vec![principal.as_str().into(), channel.into(), url.into(), SqlStorageValue::Integer(crate::js::now_ms())],
                )?;
                let id = rows.first().and_then(|r| r["id"].as_i64()).ok_or_else(|| CellError::host("a subscription insert returned no id"))?;
                self.event("subscription.added", &format!("{} to {channel}", npub::encode(&principal)), json!({ "id": id, "channel": channel }));
                id
            }
        };
        json_response(&json!({ "id": id, "channel": channel, "url": url }))
    }

    /// `GET /api/subscriptions`: the caller's (the owner sees every one).
    pub(crate) fn subscriptions(&self, caller: &Caller) -> CellResult<Response> {
        let principal = caller.principal.clone().ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "listing subscriptions needs a signed request"))?;
        let owner = self.must("owner")? == principal;
        let rows = if owner {
            self.rows("SELECT id, principal, channel, url, created_at FROM subs ORDER BY id", vec![])?
        } else {
            self.rows("SELECT id, principal, channel, url, created_at FROM subs WHERE principal = ? ORDER BY id", vec![principal.as_str().into()])?
        };
        let subs: Vec<Value> = rows
            .iter()
            .map(|r| json!({ "id": r["id"], "principal": npub::encode(r["principal"].as_str().unwrap_or("")), "channel": r["channel"], "url": r["url"], "createdAt": r["created_at"] }))
            .collect();
        json_response(&json!({ "subscriptions": subs }))
    }

    /// `DELETE /api/subscriptions/{id}`: its subscriber or the owner.
    pub(crate) fn unsubscribe(&self, caller: &Caller, id: &str) -> CellResult<Response> {
        let principal = caller.principal.clone().ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "unsubscribing needs a signed request"))?;
        let id: i64 = id.parse().map_err(|_| CellError::invalid("a subscription id is a number"))?;
        let owner = self.must("owner")? == principal;
        let gone = self.rows("DELETE FROM subs WHERE id = ? AND (principal = ? OR ?) RETURNING id", vec![SqlStorageValue::Integer(id), principal.as_str().into(), SqlStorageValue::Integer(owner as i64)])?;
        json_response(&json!({ "ok": true, "removed": gone.len() }))
    }

    /// A member's subscriptions end with its membership.
    pub(crate) fn drop_subscriptions(&self, principal: &str) -> CellResult<()> {
        self.exec("DELETE FROM subs WHERE principal = ?", vec![principal.into()])
    }

    /// Queues a new record to the channel's subscribers.
    pub(crate) async fn deliver_record(&self, record: &ChannelRecord) -> CellResult<()> {
        let rows = self.rows("SELECT id, url FROM subs WHERE channel = ?", vec![record.channel.as_str().into()])?;
        if rows.is_empty() {
            return Ok(());
        }
        let (fragment, incarnation) = (self.must("name")?, self.must("created_at")?);
        let body = json!({ "type": "record", "fragment": fragment, "channel": record.channel, "record": record });
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, body.to_string());
        let deliveries = rows
            .iter()
            .map(|r| Delivery {
                fragment: fragment.clone(),
                incarnation: incarnation.clone(),
                kind: "record".into(),
                url: r["url"].as_str().unwrap_or("").to_string(),
                headers: vec![("content-type".into(), "application/json".into())],
                body: encoded.clone(),
                sub: r["id"].as_i64(),
            })
            .collect();
        self.enqueue(deliveries).await?;
        Ok(())
    }
}
