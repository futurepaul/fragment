//! The fragment's live socket (`__live`), one per page: channel
//! subscriptions resumed from a cursor, presence, and change signals for
//! live queries. Each socket's state lives in its hibernation attachment,
//! so a hibernated object still knows who subscribed to what.
//!
//! Client → server:
//!   {type: "subscribe", channel, after}   a page of records with seq > after
//!   {type: "subscribe", channel, last}    a page of the last `last` records
//!   {type: "unsubscribe", channel}
//!   {type: "presence", data}             null clears it
//!   {type: "ping"}
//! Server → client:
//!   {type: "hello", id, principal, role}
//!   {type: "record", channel, seq, at, principal, kind, body}
//!   {type: "subscribed", channel, next, more}   after each page
//!
//! A subscribe answers one page: at most `CHANNEL_PAGE` records and about
//! `CHANNEL_PAGE_MAX_BYTES` of frames. With `more`, the socket is not live
//! on the channel yet, and the client subscribes again from `next`; the
//! page that reaches the end makes it live. A record appended while a
//! client pages comes in a later page, never live ahead of the records
//! before it, so a client's cursor never jumps a gap.
//!   {type: "presence", list: [{id, principal, data}]}
//!   {type: "changed", op}                 a mutation applied: re-run live queries
//!   {type: "error", message}
//!
//! A socket's role is fixed when it connects; removing a member closes
//! theirs, and rotating the share link closes link holders'.

use fragment_core::npub;
use fragment_proto::{limits, ChannelRecord, Role};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{Caller, FragmentCell};
use crate::js;

/// Channels one socket may follow.
const SUBSCRIPTIONS_MAX: usize = 16;
/// Records read at a time while a page fills: a page stops at its byte
/// budget long before a thousand records of 64 KiB are in memory.
const PAGE_READ_BATCH: usize = 50;

#[derive(Serialize, Deserialize, Clone)]
pub struct LiveState {
    pub id: String,
    pub principal: String,
    pub role: Role,
    pub subs: Vec<String>,
    pub presence: Option<Value>,
}

fn state_of(ws: &WebSocket) -> Option<LiveState> {
    ws.deserialize_attachment::<LiveState>().ok().flatten()
}

fn send(ws: &WebSocket, v: &Value) {
    let _ = ws.send_with_str(v.to_string());
}

fn record_frame(r: &ChannelRecord) -> Value {
    json!({ "type": "record", "channel": r.channel, "seq": r.seq, "at": r.at, "principal": r.principal, "kind": r.kind, "body": r.body })
}

impl FragmentCell {
    /// Opens a live socket for a caller who can see the fragment.
    pub(crate) fn live(&self, req: &Request, caller: &Caller, principal: &str, link: bool) -> CellResult<Response> {
        if !req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("__live is a WebSocket; send Upgrade: websocket"));
        }
        let role = self.require(caller, link, Role::Public)?;
        let tag = match &caller.principal {
            Some(p) if self.has_standing(caller)? => format!("p:{p}"),
            _ if link => "view".to_string(),
            _ => "anon".to_string(),
        };
        let pair = WebSocketPair::new()?;
        self.state.accept_websocket_with_tags(&pair.server, &["live", &tag]);
        let st = LiveState { id: js::random_hex::<8>(), principal: npub::display(principal), role, subs: vec![], presence: None };
        pair.server.serialize_attachment(&st)?;
        send(&pair.server, &json!({ "type": "hello", "id": st.id, "principal": st.principal, "role": role }));
        send(&pair.server, &json!({ "type": "presence", "list": self.presence_list(None) }));
        Ok(Response::from_websocket(pair.client)?)
    }

    fn presence_list(&self, leaving: Option<&str>) -> Vec<Value> {
        self.state
            .get_websockets_with_tag("live")
            .iter()
            .filter_map(state_of)
            .filter(|s| s.presence.is_some() && Some(s.id.as_str()) != leaving)
            .map(|s| json!({ "id": s.id, "principal": s.principal, "data": s.presence }))
            .collect()
    }

    fn broadcast_presence(&self, leaving: Option<&str>) {
        let frame = json!({ "type": "presence", "list": self.presence_list(leaving) });
        for ws in self.state.get_websockets_with_tag("live") {
            if state_of(&ws).is_some_and(|s| Some(s.id.as_str()) != leaving) {
                send(&ws, &frame);
            }
        }
    }

    pub(crate) fn broadcast_record(&self, r: &ChannelRecord) {
        let frame = record_frame(r);
        for ws in self.state.get_websockets_with_tag("live") {
            if state_of(&ws).is_some_and(|s| s.subs.contains(&r.channel)) {
                send(&ws, &frame);
            }
        }
    }

    pub(crate) fn broadcast_changed(&self, op: &str) {
        let frame = json!({ "type": "changed", "op": op });
        for ws in self.state.get_websockets_with_tag("live") {
            send(&ws, &frame);
        }
    }

    /// One message on a live socket.
    pub(crate) fn live_message(&self, ws: &WebSocket, text: &str) -> CellResult<()> {
        let Some(mut st) = state_of(ws) else { return Ok(()) };
        let msg: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let error = |m: String| -> CellResult<()> {
            send(ws, &json!({ "type": "error", "message": m }));
            Ok(())
        };
        match msg["type"].as_str() {
            Some("ping") => send(ws, &json!({ "type": "pong" })),
            Some("subscribe") => {
                let channel = msg["channel"].as_str().unwrap_or("").to_string();
                let read = match self.channel_read_role(&channel) {
                    Ok(r) => r,
                    Err(e) => return error(e.message),
                };
                if st.role < read {
                    return error(format!("channel {channel} needs the {} role", read.as_str()));
                }
                let live = st.subs.contains(&channel);
                if !live && st.subs.len() >= SUBSCRIPTIONS_MAX {
                    return error(format!("a socket follows at most {SUBSCRIPTIONS_MAX} channels"));
                }
                let after = match msg["last"].as_i64() {
                    // a page opening near the end of a long channel
                    Some(n) => (self.channel_head(&channel)? - n.clamp(0, limits::CHANNEL_PAGE as i64)).max(0),
                    None => msg["after"].as_i64().unwrap_or(0).max(0),
                };
                let (frames, next, more) = self.live_page(&channel, after)?;
                // Live only with the last page: a record appended while the
                // client pages would otherwise arrive ahead of the pages it
                // has not read, and its cursor would jump them.
                if more && live {
                    st.subs.retain(|c| c != &channel);
                    ws.serialize_attachment(&st)?;
                } else if !more && !live {
                    st.subs.push(channel.clone());
                    ws.serialize_attachment(&st)?;
                }
                for f in &frames {
                    let _ = ws.send_with_str(f);
                }
                send(ws, &json!({ "type": "subscribed", "channel": channel, "next": next, "more": more }));
            }
            Some("unsubscribe") => {
                let channel = msg["channel"].as_str().unwrap_or("");
                st.subs.retain(|c| c != channel);
                ws.serialize_attachment(&st)?;
            }
            Some("presence") => {
                let data = msg.get("data").cloned().unwrap_or(Value::Null);
                if data.to_string().len() > limits::PRESENCE_MAX_BYTES {
                    return error(format!("presence data is at most {} bytes", limits::PRESENCE_MAX_BYTES));
                }
                st.presence = (!data.is_null()).then_some(data);
                ws.serialize_attachment(&st)?;
                self.broadcast_presence(None);
            }
            _ => return error("unknown message type".into()),
        }
        Ok(())
    }

    /// The newest record's seq on a channel (0 when it has none).
    fn channel_head(&self, channel: &str) -> CellResult<i64> {
        let rows = self.rows("SELECT MAX(seq) AS n FROM records WHERE channel = ?", vec![channel.into()])?;
        Ok(rows.first().and_then(|r| r["n"].as_i64()).unwrap_or(0))
    }

    /// One page of a channel after `after`, as record frames: at most
    /// `CHANNEL_PAGE` records, and no more than `CHANNEL_PAGE_MAX_BYTES` of
    /// frames past the first. Answers the frames, the cursor after them,
    /// and whether more may follow.
    fn live_page(&self, channel: &str, after: i64) -> CellResult<(Vec<String>, i64, bool)> {
        let mut frames: Vec<String> = vec![];
        let (mut next, mut bytes) = (after, 0usize);
        for _ in 0..limits::CHANNEL_PAGE.div_ceil(PAGE_READ_BATCH) {
            let batch = self.read_channel(channel, next, PAGE_READ_BATCH)?;
            let end = batch.len() < PAGE_READ_BATCH;
            for r in batch {
                assert!(r.seq > next, "a channel reads in order");
                let frame = record_frame(&r).to_string();
                if !frames.is_empty() && bytes + frame.len() > limits::CHANNEL_PAGE_MAX_BYTES {
                    return Ok((frames, next, true));
                }
                bytes += frame.len();
                next = r.seq;
                frames.push(frame);
                if frames.len() == limits::CHANNEL_PAGE {
                    return Ok((frames, next, true));
                }
            }
            if end {
                return Ok((frames, next, false));
            }
        }
        panic!("a page read {} full batches without filling {} records", limits::CHANNEL_PAGE.div_ceil(PAGE_READ_BATCH), limits::CHANNEL_PAGE);
    }

    /// A live socket closed: its presence goes.
    pub(crate) fn live_closed(&self, ws: &WebSocket) {
        if let Some(st) = state_of(ws) {
            if st.presence.is_some() {
                self.broadcast_presence(Some(&st.id));
            }
        }
    }
}
