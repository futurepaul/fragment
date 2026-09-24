//! The fragment's live socket (`__live`), one per page: channel
//! subscriptions resumed from a cursor, presence, and change signals for
//! live queries. Each socket's state lives in its hibernation attachment,
//! so a hibernated object still knows who subscribed to what. The frames
//! are `fragment_proto::live::{LiveIn, LiveOut}`: every message is decoded
//! into one before anything reads it, and a frame that does not decode is
//! answered with an error naming why.
//!
//! A subscribe answers one page: at most `CHANNEL_PAGE` records and about
//! `CHANNEL_PAGE_MAX_BYTES` of frames. With `more`, the socket is not live
//! on the channel yet, and the client subscribes again from `next`; the
//! page that reaches the end makes it live. A record appended while a
//! client pages comes in a later page, never live ahead of the records
//! before it, so a client's cursor never jumps a gap.
//!
//! A socket's role is fixed when it connects; removing a member closes
//! theirs, and rotating the share link closes link holders' (4003);
//! deleting the fragment closes every socket (4004). Both codes are
//! final: the browser library does not reconnect after them.

use fragment_core::npub;
use fragment_proto::live::{Cursor, LiveIn, LiveOut, Present, Subscribe};
use fragment_proto::{limits, ChannelRecord, ErrorCode, Role};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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

fn send(ws: &WebSocket, frame: &LiveOut) {
    let _ = ws.send_with_str(frame.encode());
}

impl FragmentCell {
    /// Opens a live socket for a caller who can see the fragment.
    pub(crate) fn live(&self, req: &Request, caller: &Caller, principal: &str, link: bool) -> CellResult<Response> {
        if !req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("__live is a WebSocket; send Upgrade: websocket"));
        }
        let role = self.require(caller, link, Role::Public)?;
        let tag = match caller.principal() {
            Some(p) if self.has_standing(caller)? => format!("p:{p}"),
            _ if link => "view".to_string(),
            _ => "anon".to_string(),
        };
        let pair = WebSocketPair::new()?;
        self.state.accept_websocket_with_tags(&pair.server, &["live", &tag]);
        let st = LiveState { id: js::random_hex::<8>(), principal: npub::display(principal), role, subs: vec![], presence: None };
        pair.server.serialize_attachment(&st)?;
        send(&pair.server, &LiveOut::Hello { id: st.id, principal: st.principal, role });
        send(&pair.server, &LiveOut::Presence { list: self.presence_list(None) });
        Ok(Response::from_websocket(pair.client)?)
    }

    fn presence_list(&self, leaving: Option<&str>) -> Vec<Present> {
        self.state
            .get_websockets_with_tag("live")
            .iter()
            .filter_map(state_of)
            .filter(|s| Some(s.id.as_str()) != leaving)
            .filter_map(|s| Some(Present { id: s.id, principal: s.principal, data: s.presence? }))
            .collect()
    }

    fn broadcast_presence(&self, leaving: Option<&str>) {
        let frame = LiveOut::Presence { list: self.presence_list(leaving) }.encode();
        for ws in self.state.get_websockets_with_tag("live") {
            if state_of(&ws).is_some_and(|s| Some(s.id.as_str()) != leaving) {
                let _ = ws.send_with_str(&frame);
            }
        }
    }

    pub(crate) fn broadcast_record(&self, r: &ChannelRecord) {
        let frame = LiveOut::Record(r.clone()).encode();
        for ws in self.state.get_websockets_with_tag("live") {
            if state_of(&ws).is_some_and(|s| s.subs.contains(&r.channel)) {
                let _ = ws.send_with_str(&frame);
            }
        }
    }

    pub(crate) fn broadcast_changed(&self, op: &str) {
        let frame = LiveOut::Changed { op: op.to_string() }.encode();
        for ws in self.state.get_websockets_with_tag("live") {
            let _ = ws.send_with_str(&frame);
        }
    }

    /// One message on a live socket: decoded, then answered. A refusal
    /// goes back on the socket as an error frame; the socket stays open.
    pub(crate) fn live_message(&self, ws: &WebSocket, text: &str) {
        let Some(st) = state_of(ws) else { return };
        let answered = match serde_json::from_str::<LiveIn>(text) {
            Ok(frame) => self.live_frame(ws, st, frame),
            Err(e) => Err(CellError::invalid(format!("a live frame: {e}"))),
        };
        if let Err(e) = answered {
            send(ws, &LiveOut::Error { message: e.message });
        }
    }

    fn live_frame(&self, ws: &WebSocket, mut st: LiveState, frame: LiveIn) -> CellResult<()> {
        match frame {
            LiveIn::Ping => send(ws, &LiveOut::Pong),
            LiveIn::Subscribe(Subscribe { channel, from }) => {
                let read = self.channel_read_role(&channel)?;
                if st.role < read {
                    return Err(CellError::new(ErrorCode::Forbidden, format!("channel {channel} needs the {} role", read.as_str())));
                }
                let live = st.subs.contains(&channel);
                if !live && st.subs.len() >= SUBSCRIPTIONS_MAX {
                    return Err(CellError::invalid(format!("a socket follows at most {SUBSCRIPTIONS_MAX} channels")));
                }
                let after = match from {
                    // a page opening near the end of a long channel
                    Cursor::Last(n) => (self.channel_head(&channel)? - n.clamp(0, limits::CHANNEL_PAGE as i64)).max(0),
                    Cursor::After(n) => n.max(0),
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
                send(ws, &LiveOut::Subscribed { channel, next, more });
            }
            LiveIn::Unsubscribe { channel } => {
                st.subs.retain(|c| c != &channel);
                ws.serialize_attachment(&st)?;
            }
            LiveIn::Presence { data } => {
                let size = data.to_string().len();
                if size > limits::PRESENCE_MAX_BYTES {
                    return Err(CellError::too_large("presence data", size, limits::PRESENCE_MAX_BYTES));
                }
                st.presence = (!data.is_null()).then_some(data);
                ws.serialize_attachment(&st)?;
                self.broadcast_presence(None);
            }
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
                let seq = r.seq;
                let frame = LiveOut::Record(r).encode();
                if !frames.is_empty() && bytes + frame.len() > limits::CHANNEL_PAGE_MAX_BYTES {
                    return Ok((frames, next, true));
                }
                bytes += frame.len();
                next = seq;
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
