//! Channel subscriptions (phase 7): a member asks the fragment to deliver
//! each new record of a channel it may read to a URL, such as an agent's
//! inbox (MODEL.md, Agents: "the agent member subscribes"). Records go out
//! through the delivery outbox and queue (deliveries.rs) as `{type:
//! "record", fragment, channel, record}` (`fragment_proto::Delivery`, the
//! type the agents' inbox decodes), unsigned: the URL is the subscriber's
//! capability. A member's subscriptions end with its
//! membership.

use fragment_core::{egress, npub};
use fragment_proto::{valid_channel_name, ChannelRecord, DeliveryType, ErrorCode};
use serde_json::{json, Value};
use worker::*;

use crate::deliveries::{Delivery, DeliveryKind};
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell, MetaKey};

/// The subscriptions one fragment holds, and a URL's length.
const SUBS_MAX: u64 = 32;
const SUB_URL_MAX_BYTES: usize = 1024;

impl FragmentCell {
    /// The subscriber: a member whose role may read `channel`.
    fn subscriber(&self, caller: &Caller, channel: &str) -> CellResult<String> {
        let principal = caller.principal().map(str::to_string).ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "subscribing needs a signed request"))?;
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
                self.event("subscription.added", &format!("{} to {channel}", npub::display(&principal)), json!({ "id": id, "channel": channel }));
                id
            }
        };
        json_response(&json!({ "id": id, "channel": channel, "url": url }))
    }

    /// `GET /api/subscriptions`: the caller's (the owner sees every one).
    pub(crate) fn subscriptions(&self, caller: &Caller) -> CellResult<Response> {
        let principal = caller.principal().map(str::to_string).ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "listing subscriptions needs a signed request"))?;
        let owner = self.must(MetaKey::Owner)? == principal;
        let rows = self.rows(
            "SELECT id, principal, channel, url, created_at FROM subs WHERE principal = ? OR ? ORDER BY id",
            vec![principal.as_str().into(), SqlStorageValue::Integer(owner as i64)],
        )?;
        let subs: Vec<Value> = rows
            .iter()
            .map(|r| json!({ "id": r["id"], "principal": npub::display(r["principal"].as_str().unwrap_or("")), "channel": r["channel"], "url": r["url"], "createdAt": r["created_at"] }))
            .collect();
        json_response(&json!({ "subscriptions": subs }))
    }

    /// `DELETE /api/subscriptions/{id}`: its subscriber or the owner.
    pub(crate) fn unsubscribe(&self, caller: &Caller, id: &str) -> CellResult<Response> {
        let principal = caller.principal().map(str::to_string).ok_or_else(|| CellError::new(ErrorCode::Unauthenticated, "unsubscribing needs a signed request"))?;
        let id: i64 = id.parse().map_err(|_| CellError::invalid("a subscription id is a number"))?;
        let owner = self.must(MetaKey::Owner)? == principal;
        let gone = self.rows("DELETE FROM subs WHERE id = ? AND (principal = ? OR ?) RETURNING id", vec![SqlStorageValue::Integer(id), principal.as_str().into(), SqlStorageValue::Integer(owner as i64)])?;
        json_response(&json!({ "ok": true, "removed": gone.len() }))
    }

    /// A member's subscriptions end with its membership.
    pub(crate) fn drop_subscriptions(&self, principal: &str) -> CellResult<()> {
        self.exec("DELETE FROM subs WHERE principal = ?", vec![principal.into()])
    }

    /// A new record's deliveries, one per subscription to its channel,
    /// written to the delivery outbox in the same step as the record (the
    /// caller appends, then calls this, with no await between; channels.rs
    /// `published`). Answers whether there are any to drain.
    pub(crate) fn outbox_record(&self, record: &ChannelRecord) -> CellResult<bool> {
        self.test_countdown(MetaKey::TestFailOutbox, "the record's outbox write failed after its append")?;
        let rows = self.rows(
            "INSERT INTO delivery_outbox (kind, sub, channel, seq, next_at) SELECT 'record', id, channel, ?, ? FROM subs WHERE channel = ? RETURNING id",
            vec![SqlStorageValue::Integer(record.seq), SqlStorageValue::Integer(crate::js::now_ms()), record.channel.as_str().into()],
        )?;
        self.exec(
            "UPDATE records SET outboxed = 1 WHERE channel = ? AND seq = ?",
            vec![record.channel.as_str().into(), SqlStorageValue::Integer(record.seq)],
        )?;
        Ok(!rows.is_empty())
    }

    /// Whether a record's deliveries were written (`outbox_record`).
    pub(crate) fn outboxed(&self, record: &ChannelRecord) -> CellResult<bool> {
        let rows = self.rows(
            "SELECT outboxed FROM records WHERE channel = ? AND seq = ?",
            vec![record.channel.as_str().into(), SqlStorageValue::Integer(record.seq)],
        )?;
        let row = rows.first().ok_or_else(|| CellError::host(format!("record {} of {:?} is gone", record.seq, record.channel)))?;
        Ok(row["outboxed"].as_i64().expect("records.outboxed is INTEGER NOT NULL") != 0)
    }

    /// The delivery of record `seq` of `channel` to subscription `sub`, or
    /// `None` when there is nothing to send: the subscription ended, or the
    /// record is past its retention.
    pub(crate) fn record_delivery(&self, sub: i64, channel: &str, seq: i64, fragment: &str, incarnation: &str) -> CellResult<Option<Delivery>> {
        let subs = self.rows("SELECT url FROM subs WHERE id = ?", vec![SqlStorageValue::Integer(sub)])?;
        let Some(url) = subs.first().map(|r| r["url"].as_str().expect("subs.url is TEXT").to_string()) else { return Ok(None) };
        let Some(record) = self.read_channel(channel, seq - 1, 1)?.into_iter().next().filter(|r| r.seq == seq) else { return Ok(None) };
        // serialized as it is: the record's body goes out as the text the cell stored
        let delivery = fragment_proto::Delivery { kind: DeliveryType::Record, fragment: fragment.to_string(), channel: channel.to_string(), record };
        let body = serde_json::to_string(&delivery).expect("a delivery serializes");
        Ok(Some(Delivery::json(fragment, incarnation, DeliveryKind::Record, url, &body, Some(sub))))
    }
}
