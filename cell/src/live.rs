//! The fragment's live socket (`__live`), one per page: channel
//! subscriptions resumed from a cursor, presence, change signals, and the
//! page's live queries. Each socket's state lives in its hibernation
//! attachment, so a hibernated object still knows who subscribed to what.
//! The frames are `fragment_proto::live::{LiveIn, LiveOut}`: every message
//! is decoded into one before anything reads it, and a frame that does not
//! decode is answered with an error naming why.
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
//! final: the browser library does not reconnect after them. A member
//! whose role changes has theirs closed with 4001 (`ROLE_CHANGED`), after
//! which the library reconnects, at the new role.
//!
//! A query over the socket runs as the socket's principal and role, as its
//! subscriptions read, and outside the public call budget (ops.rs): no
//! HTTP request, router, or registry lookup a run. Its bound is the
//! socket's own (`fragment_core::live::QueryBudget`).
//!
//! A fragment holds at most `LIVE_SOCKETS_MAX` sockets. Presence comes
//! whole in `hello`, then one change at a time, each encoded once and sent
//! to every socket (the O(N) bytes of a change, never O(N²)); a socket's
//! changes past `PRESENCE_PER_S` a second are dropped.
//!
//! What this activation knows of its sockets besides their attachments
//! (`LiveMemory`) is gathered from them again after the object wakes.

use std::collections::{BTreeMap, BTreeSet};

use fragment_core::live::{presence_admit, QueryBudget, QueryRefused};
use fragment_core::facet::Answered;
use fragment_core::npub;
use fragment_proto::live::{Answer, Cursor, LiveIn, LiveOut, Present, Query, Subscribe};
use fragment_proto::{limits, valid_op_id, ChannelRecord, ErrorBody, ErrorCode, OpKind, Role, Via};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{Caller, FragmentCell};
use crate::js;
use crate::ops::Invocation;

/// Channels one socket may follow.
const SUBSCRIPTIONS_MAX: usize = 16;
/// Records read at a time while a page fills: a page stops at its byte
/// budget long before a thousand records of 64 KiB are in memory.
const PAGE_READ_BATCH: usize = 50;
/// The close code for a socket whose member's role changed: not final, so
/// the page reconnects and hears its new role.
const ROLE_CHANGED: u16 = 4001;

#[derive(Serialize, Deserialize, Clone)]
pub struct LiveState {
    pub id: String,
    pub principal: String,
    pub role: Role,
    pub subs: Vec<String>,
    pub presence: Option<Value>,
    /// When the socket's next presence change is due at its steady pace
    /// (`presence_admit`); 0, as for a socket that connected before the
    /// pace was kept, lets its first changes through.
    #[serde(default)]
    pub presence_at: i64,
}

/// What this activation knows of its live sockets besides their
/// attachments, which are the source: after the object wakes, it starts
/// empty and is gathered from them again. Nothing here is authoritative.
#[derive(Default)]
pub(crate) struct LiveMemory {
    /// The sockets (ids) that may follow each channel, gathered from the
    /// attachments at the first append of the activation (`None` before).
    /// A subscribe that makes a socket live adds it in the same step that
    /// writes its attachment, so a follower is never missing; each append
    /// that scans puts its channel's set back to the followers it found,
    /// so one that left (or went without its close event) stays at most
    /// until then. An append to a channel whose set is empty scans nothing.
    followers: Option<BTreeMap<String, BTreeSet<String>>>,
    /// Mutations applied in this activation: each refills every socket's
    /// query budget.
    changes: u64,
    /// Each socket's query budget, by socket id.
    queries: BTreeMap<String, QueryBudget>,
}

fn state_of(ws: &WebSocket) -> Option<LiveState> {
    ws.deserialize_attachment::<LiveState>().ok().flatten()
}

fn send(ws: &WebSocket, frame: &LiveOut) {
    let _ = ws.send_with_str(frame.encode());
}

/// A query running on a socket: it is done when this drops, however its
/// run ended.
struct Running<'a> {
    cell: &'a FragmentCell,
    socket: String,
    id: String,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        // the socket's budget went with it if it closed meanwhile
        if let Some(budget) = self.cell.live.borrow_mut().queries.get_mut(&self.socket) {
            budget.done(&self.id);
        }
    }
}

/// Everyone on `sockets` who shares presence.
fn present_on(sockets: &[WebSocket]) -> Vec<Present> {
    sockets.iter().filter_map(state_of).filter_map(|s| Some(Present { id: s.id, principal: s.principal, data: s.presence? })).collect()
}

impl FragmentCell {
    /// Opens a live socket for a caller who can see the fragment.
    pub(crate) fn live(&self, req: &Request, caller: &Caller, principal: &str, link: bool) -> CellResult<Response> {
        if !req.headers().get("upgrade")?.is_some_and(|u| u.eq_ignore_ascii_case("websocket")) {
            return Err(CellError::invalid("__live is a WebSocket; send Upgrade: websocket"));
        }
        let role = self.require(caller, link, Role::Public)?;
        let sockets = self.state.get_websockets_with_tag("live");
        if sockets.len() >= limits::LIVE_SOCKETS_MAX {
            return Err(CellError::new(ErrorCode::RateLimited, format!("this fragment has {} live sockets open, its most; try again later", sockets.len())));
        }
        let presence = present_on(&sockets);
        let tag = match caller.principal() {
            Some(p) if self.has_standing(caller)? => format!("p:{p}"),
            _ if link => "view".to_string(),
            _ => "anon".to_string(),
        };
        let pair = WebSocketPair::new()?;
        self.state.accept_websocket_with_tags(&pair.server, &["live", &tag]);
        let st = LiveState { id: js::random_hex::<8>(), principal: npub::display(principal), role, subs: vec![], presence: None, presence_at: 0 };
        pair.server.serialize_attachment(&st)?;
        send(&pair.server, &LiveOut::Hello { id: st.id, principal: st.principal, role, presence });
        Ok(Response::from_websocket(pair.client)?)
    }

    /// One socket's presence changed, to every socket (its own too): one
    /// frame, encoded once, and no attachment read.
    fn broadcast_presence(&self, change: Present) {
        let frame = LiveOut::Presence(change).encode();
        for ws in self.state.get_websockets_with_tag("live") {
            let _ = ws.send_with_str(&frame);
        }
    }

    /// A record to the sockets following its channel: none are looked at
    /// when no socket may follow it (`LiveMemory::followers`).
    pub(crate) fn broadcast_record(&self, r: &ChannelRecord) {
        if !self.followed(&r.channel) {
            return;
        }
        let frame = LiveOut::record_frame(r);
        let mut found = BTreeSet::new();
        for ws in self.state.get_websockets_with_tag("live") {
            let Some(st) = state_of(&ws) else { continue };
            if st.subs.contains(&r.channel) {
                let _ = ws.send_with_str(&frame);
                found.insert(st.id);
            }
        }
        if let Some(followers) = self.live.borrow_mut().followers.as_mut() {
            followers.insert(r.channel.clone(), found);
        }
    }

    pub(crate) fn broadcast_changed(&self, op: &str) {
        self.live.borrow_mut().changes += 1;
        let frame = LiveOut::Changed { op: op.to_string() }.encode();
        for ws in self.state.get_websockets_with_tag("live") {
            let _ = ws.send_with_str(&frame);
        }
    }

    /// Whether any socket may follow `channel`, from the followers this
    /// activation gathered (the first call gathers them).
    fn followed(&self, channel: &str) -> bool {
        let mut memory = self.live.borrow_mut();
        let followers = memory.followers.get_or_insert_with(|| {
            let mut gathered: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for st in self.state.get_websockets_with_tag("live").iter().filter_map(state_of) {
                for c in st.subs {
                    gathered.entry(c).or_default().insert(st.id.clone());
                }
            }
            gathered
        });
        followers.get(channel).is_some_and(|sockets| !sockets.is_empty())
    }

    /// A socket goes live on a channel, or stops following it.
    fn set_follower(&self, channel: &str, socket: &str, follows: bool) {
        let mut memory = self.live.borrow_mut();
        let Some(followers) = memory.followers.as_mut() else { return };
        let sockets = followers.entry(channel.to_string()).or_default();
        if follows {
            sockets.insert(socket.to_string());
        } else {
            sockets.remove(socket);
        }
    }

    /// Test fleets: forgets what this activation knows of its sockets, as
    /// waking from hibernation does (`/api/test/fragment` `forget-live`).
    pub(crate) fn live_forget(&self) {
        assert!(self.cfg.test_hooks, "only a test fleet forgets its sockets on demand");
        *self.live.borrow_mut() = LiveMemory::default();
    }

    /// Closes the sockets of `tag` so that their pages reconnect: a
    /// member's role changed.
    pub(crate) fn reopen_sockets(&self, tag: &str, reason: &str) {
        for ws in self.state.get_websockets_with_tag(tag) {
            let _ = ws.close(Some(ROLE_CHANGED), Some(reason));
        }
    }

    /// One message on a live socket: decoded, then answered. A refusal
    /// goes back on the socket as an error frame; the socket stays open.
    pub(crate) async fn live_message(&self, ws: &WebSocket, text: &str) {
        let Some(st) = state_of(ws) else { return };
        let answered = match serde_json::from_str::<LiveIn>(text) {
            Ok(LiveIn::Query(query)) => {
                send(ws, &LiveOut::Result(self.live_query(&st, query).await));
                Ok(())
            }
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
                    self.set_follower(&channel, &st.id, false);
                } else if !more && !live {
                    st.subs.push(channel.clone());
                    ws.serialize_attachment(&st)?;
                    self.set_follower(&channel, &st.id, true);
                }
                for f in &frames {
                    let _ = ws.send_with_str(f);
                }
                send(ws, &LiveOut::Subscribed { channel, next, more });
            }
            LiveIn::Unsubscribe { channel } => {
                st.subs.retain(|c| c != &channel);
                ws.serialize_attachment(&st)?;
                self.set_follower(&channel, &st.id, false);
            }
            LiveIn::Presence { data } => {
                let size = data.to_string().len();
                if size > limits::PRESENCE_MAX_BYTES {
                    return Err(CellError::too_large("presence data", size, limits::PRESENCE_MAX_BYTES));
                }
                let Some(at) = presence_admit(st.presence_at, js::now_ms()) else {
                    return Err(CellError::new(ErrorCode::RateLimited, format!("presence changes past {} a second are dropped", limits::PRESENCE_PER_S)));
                };
                let had = st.presence.is_some();
                st.presence = (!data.is_null()).then_some(data);
                st.presence_at = at;
                ws.serialize_attachment(&st)?;
                // clearing presence that was never shared changes nothing
                if had || st.presence.is_some() {
                    self.broadcast_presence(Present { id: st.id, principal: st.principal, data: st.presence.unwrap_or(Value::Null) });
                }
            }
            LiveIn::Query(_) => unreachable!("live_message answers a query"),
        }
        Ok(())
    }

    /// A query over the socket, admitted by the socket's budget and
    /// answered with its id: a refusal is the answer's, as `__op` would
    /// refuse it.
    async fn live_query(&self, st: &LiveState, query: Query) -> Answer {
        let Query { id, op, input } = query;
        let outcome = match self.admit_query(&st.id, &id) {
            Ok(running) => {
                let ran = self.socket_query(st, &op, &id, input).await;
                drop(running);
                ran.map(|r| r.result)
            }
            Err(e) => Err(e),
        };
        Answer { id, outcome: outcome.map_err(|e| ErrorBody { error: e.code, message: e.message }) }
    }

    fn admit_query(&self, socket: &str, id: &str) -> CellResult<Running<'_>> {
        if !valid_op_id(id) {
            return Err(CellError::invalid("a query's id must match ^[A-Za-z0-9._:-]{1,128}$"));
        }
        let mut memory = self.live.borrow_mut();
        let change = memory.changes;
        // One budget a socket. Past as many as there may be sockets, the
        // idle ones go (a socket gone without its close event leaves one);
        // a live socket's budget that goes starts afresh, still bounded.
        if memory.queries.len() >= limits::LIVE_SOCKETS_MAX && !memory.queries.contains_key(socket) {
            memory.queries.retain(|_, budget| !budget.idle());
        }
        let admitted = memory.queries.entry(socket.to_string()).or_default().admit(id, change);
        match admitted {
            Ok(()) => Ok(Running { cell: self, socket: socket.to_string(), id: id.to_string() }),
            Err(QueryRefused::InFlight) => Err(CellError::invalid(format!("query {id} is still running on this socket: one run at a time for each id"))),
            Err(QueryRefused::Spent) => Err(CellError::new(
                ErrorCode::RateLimited,
                format!("this socket ran {} queries since the fragment last changed: run it over HTTP, or after the next change", limits::LIVE_QUERIES_MAX),
            )),
        }
    }

    /// Runs query `op` as the socket's principal and role (both fixed when
    /// it connected), as `__op` runs one but for the public call budget.
    async fn socket_query(&self, st: &LiveState, op: &str, id: &str, input: Value) -> CellResult<Answered> {
        let decl = self.declared(op)?;
        if decl.kind != OpKind::Query {
            return Err(CellError::invalid(format!("{op} is not a query: only queries run over the live socket (call it through __op)")));
        }
        if st.role < decl.role {
            return Err(CellError::new(ErrorCode::Forbidden, format!("this needs the {} role", decl.role.as_str())));
        }
        let inv = Invocation { principal: &st.principal, role: st.role, op, decl, id: id.to_string(), input, depth: 0, via: Via::Call, trigger: None };
        self.invoke(inv).await
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
                let frame = LiveOut::record_frame(&r);
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

    /// A live socket closed: its presence goes, with its query budget and
    /// its place among followers.
    pub(crate) fn live_closed(&self, ws: &WebSocket) {
        let Some(st) = state_of(ws) else { return };
        {
            let mut memory = self.live.borrow_mut();
            memory.queries.remove(&st.id);
            if let Some(followers) = memory.followers.as_mut() {
                for c in &st.subs {
                    if let Some(sockets) = followers.get_mut(c) {
                        sockets.remove(&st.id);
                    }
                }
            }
        }
        if st.presence.is_some() {
            self.broadcast_presence(Present { id: st.id, principal: st.principal, data: Value::Null });
        }
    }
}
