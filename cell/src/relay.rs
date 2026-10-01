//! A Hermes cell as its Hermes' Relay connector (docs/one-home.md, phase 2;
//! the wire: docs/hermes-relay.md, `fragment_core::relay`). Hermes' gateway
//! dials `/api/hermes/<fragment>/relay`, authenticated by its secret, and
//! holds that socket (hibernating here between frames). The cell:
//!
//! - follows each chat that names its Hermes as who answers (a subscription
//!   on its channel, as the Hermes' computer identity, a member of the
//!   chat), and keeps each message it is delivered (`relay_inbox`);
//! - hands Hermes one message of a chat at a time, as a group message with
//!   its writer's name, and the next only once its turn ends (Hermes in
//!   queue mode drops a third writer's message mid-turn), each delivery
//!   kept until Hermes acks it and sent again on its next dial;
//! - turns what Hermes sends into the chat's records, as its computer
//!   identity: a reply's edits into a `draft` as it streams, then the
//!   reply itself (`{text, turn}`) once the turn ends or a later reply of
//!   the turn begins; its tool progress's lines into `turn.step`s on
//!   `work`, bracketed by `turn.start` (at delivery) and `turn.end` (when
//!   its `👀` comes off);
//! - wakes its computer (the node's wake, signed by its key) as it hands
//!   Hermes a message, and while Hermes is away, at most every
//!   `WAKE_EVERY_MS`: a paused guest keeps its socket open here, and hears
//!   nothing until it is resumed (a computer awake stays awake);
//! - stops a turn on the chat's Stop (`interrupt_inbound`, never a `/stop`
//!   message).

use fragment_core::relay::{self as wire, Action, FromGateway};
use fragment_core::work;
use fragment_proto::{Delivery, ErrorCode, Identity, IdentityKind};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::hermes::{HermesCell, Row};
use crate::registry::calls;
use crate::routed::Signed;
use crate::{js, keys};

/// The tag of its Hermes' socket.
pub(crate) const RELAY_TAG: &str = "relay";
/// A turn that never ended ends after this long, and its chat's next
/// message goes.
const TURN_MAX_MS: i64 = 20 * 60_000;
/// Its computer's wake URL is poked at most this often (a poke is a GET
/// that only wakes it).
const WAKE_EVERY_MS: i64 = 5_000;
/// A chat's messages kept for Hermes, at most; past that, the oldest go.
const PENDING_MAX: i64 = 100;
/// The chats one Hermes answers, at most.
const CHATS_MAX: i64 = 200;
/// A message's text, at most (a record's body bounds it already).
const TEXT_MAX_BYTES: usize = 32 * 1024;
/// How often its alarm looks while anything waits on Hermes (a poke
/// again, while it is away).
const TICK_MS: i64 = 10_000;

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS relay_chats (chat TEXT PRIMARY KEY, channel TEXT NOT NULL, sub INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS relay_inbox (id INTEGER PRIMARY KEY AUTOINCREMENT, chat TEXT NOT NULL, seq INTEGER NOT NULL,
  principal TEXT NOT NULL, name TEXT NOT NULL, text TEXT NOT NULL, state TEXT NOT NULL, sent_at INTEGER, at INTEGER NOT NULL,
  UNIQUE (chat, seq));
CREATE TABLE IF NOT EXISTS relay_out (id TEXT PRIMARY KEY, chat TEXT NOT NULL, turn TEXT NOT NULL, reply INTEGER NOT NULL,
  text TEXT NOT NULL, lines INTEGER NOT NULL, posted INTEGER NOT NULL, n INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS relay_state (key TEXT PRIMARY KEY, value INTEGER NOT NULL);";

/// A message kept for Hermes: `pending` (not handed to it), `sent` (handed,
/// not acked), `acked` (its turn is Hermes').
#[derive(Deserialize, Clone)]
struct Kept {
    id: i64,
    chat: String,
    seq: i64,
    principal: String,
    name: String,
    text: String,
    state: String,
}

/// A message Hermes sent: a reply, or its tool progress.
#[derive(Deserialize, Clone)]
struct Sent {
    id: String,
    chat: String,
    turn: String,
    reply: i64,
    text: String,
    lines: i64,
}

impl HermesCell {
    fn relay_rows<T: for<'de> Deserialize<'de>>(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<T>> {
        Ok(self.sql().exec(q, binds)?.to_array::<T>()?)
    }

    fn relay_exec(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<()> {
        self.sql().exec(q, binds)?;
        Ok(())
    }

    fn counter(&self, key: &str) -> CellResult<i64> {
        #[derive(Deserialize)]
        struct V {
            value: i64,
        }
        let v: Vec<V> = self.relay_rows("INSERT INTO relay_state (key, value) VALUES (?, 1) ON CONFLICT (key) DO UPDATE SET value = value + 1 RETURNING value", vec![key.into()])?;
        Ok(v.first().map_or(1, |v| v.value))
    }

    fn state_value(&self, key: &str) -> CellResult<i64> {
        #[derive(Deserialize)]
        struct V {
            value: i64,
        }
        let v: Vec<V> = self.relay_rows("SELECT value FROM relay_state WHERE key = ?", vec![key.into()])?;
        Ok(v.first().map_or(0, |v| v.value))
    }

    fn set_state(&self, key: &str, value: i64) -> CellResult<()> {
        self.relay_exec("INSERT INTO relay_state (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value", vec![key.into(), SqlStorageValue::Integer(value)])
    }

    fn socket(&self) -> Option<WebSocket> {
        self.state.get_websockets_with_tag(RELAY_TAG).into_iter().next()
    }

    /// Its Hermes' computer identity, as a member of its chats: who its
    /// records are by.
    fn hermes_signed(&self, h: &Row) -> CellResult<Signed> {
        let id = h.identity.clone().ok_or_else(|| CellError::new(ErrorCode::NotReady, "its Hermes is starting; ask again shortly"))?;
        let (_, username) = fragment_proto::split_fragment_name(&h.fragment).ok_or_else(|| CellError::host(format!("{} is not <label>.<username>", h.fragment)))?;
        Ok(Signed::new(Identity { id, kind: IdentityKind::Computer, owner: Some(h.owner.clone()), username: Some(username.to_string()) }, None))
    }

    /// A call to a chat it answers, as its Hermes.
    async fn to_chat(&self, h: &Row, chat: &str, method: Method, inner: &str, body: Value) -> CellResult<Value> {
        let (_, platform) = self.cfg.sandcastle()?;
        let url = url::Url::parse(platform).map_err(|e| CellError::host(format!("FRAGMENT_PLATFORM_URL: {e}")))?;
        crate::share::ask(&self.env, &url, chat, &self.hermes_signed(h)?, method, inner, Some(body)).await
    }

    async fn post(&self, h: &Row, chat: &str, channel: &str, id: &str, body: Value) -> CellResult<()> {
        self.to_chat(h, chat, Method::Post, &format!("/api/channels/{channel}"), json!({ "id": id, "body": body })).await.map(|_| ())
    }

    /// Its reply as it streams, to the chat's readers; a draft past the
    /// chat's pace is dropped (the next carries the whole text).
    async fn draft(&self, h: &Row, chat: &str, turn: &str, text: Option<&str>) {
        let drafted = self.to_chat(h, chat, Method::Put, "/api/channels/chat/draft", json!({ "turn": turn, "text": text })).await;
        if let Err(e) = drafted.filter_err(|e| e.code != ErrorCode::RateLimited) {
            console_error!("{}: a draft in {chat}: {}", h.fragment, e.message);
        }
    }

    // ---- the chats it answers ----

    /// `Answerer {chat, owner}`: who answers there, once it is made: its
    /// computer identity (the chat adds it as an editor, then asks it to
    /// listen). Only its owner's chats.
    pub(crate) fn answerer(&self, owner: &str) -> CellResult<Value> {
        let Some(h) = self.row()? else { return Err(CellError::new(ErrorCode::NotFound, "this fragment declares no Hermes")) };
        if h.owner != owner {
            return Err(CellError::new(ErrorCode::Forbidden, "a Hermes answers its owner's chats alone"));
        }
        match (h.phase, &h.identity) {
            (fragment_core::hermes::Phase::Remove | fragment_core::hermes::Phase::Gone, _) => Err(CellError::new(ErrorCode::NotFound, "this fragment has no Hermes")),
            (_, None) => Err(CellError::new(ErrorCode::NotReady, "its Hermes is starting; the chat asks again")),
            (_, Some(identity)) => Ok(json!({ "identity": identity })),
        }
    }

    /// `Listen {chat, channel, owner}`: follows the chat's channel, as its
    /// Hermes (a member there already), its records delivered to its inbox.
    /// A Hermes made before it answered chats is given its Relay first: its
    /// computer is made again with it.
    pub(crate) async fn listen(&self, chat: &str, channel: &str, owner: &str) -> CellResult<Value> {
        self.answerer(owner)?;
        let mut h = self.row()?.expect("an answerer has its row");
        if h.relay_sealed.is_none() || h.inbox.is_none() {
            self.give_relay(&mut h).await?;
            h.made = false;
            h.phase = fragment_core::hermes::Phase::Make;
            h.tries = 0;
            self.save(&h)?;
            self.arm(0).await?;
        }
        let chats: Vec<Value> = self.relay_rows("SELECT chat FROM relay_chats", vec![])?;
        if chats.len() as i64 >= CHATS_MAX && !chats.iter().any(|c| c["chat"] == chat) {
            return Err(CellError::new(ErrorCode::TooLarge, format!("a Hermes answers at most {CHATS_MAX} chats")));
        }
        let (_, platform) = self.cfg.sandcastle()?;
        let inbox = h.inbox.clone().expect("given its relay");
        let url = format!("{}/inbox/{inbox}", fragment_core::hermes::relay_url(platform, &h.fragment));
        let sub = self.to_chat(&h, chat, Method::Post, "/api/subscriptions", json!({ "channel": channel, "url": url })).await?;
        let id = sub["id"].as_i64().ok_or_else(|| CellError::host("the chat's subscription has no id"))?;
        self.relay_exec(
            "INSERT INTO relay_chats (chat, channel, sub) VALUES (?, ?, ?) ON CONFLICT (chat) DO UPDATE SET channel = excluded.channel, sub = excluded.sub",
            vec![chat.into(), channel.into(), SqlStorageValue::Integer(id)],
        )?;
        Ok(json!({ "subscription": id }))
    }

    /// `Unlisten {chat}`: stops following it, and forgets what it kept of it.
    pub(crate) async fn unlisten(&self, chat: &str) -> CellResult<Value> {
        #[derive(Deserialize)]
        struct C {
            sub: i64,
        }
        let had: Vec<C> = self.relay_rows("SELECT sub FROM relay_chats WHERE chat = ?", vec![chat.into()])?;
        if let (Some(c), Some(h)) = (had.first(), self.row()?) {
            // the chat removing it as a member ended it already, most likely
            if let Err(e) = self.to_chat(&h, chat, Method::Delete, &format!("/api/subscriptions/{}", c.sub), json!({})).await {
                if !matches!(e.code, ErrorCode::NotFound | ErrorCode::Forbidden | ErrorCode::Unauthenticated) {
                    return Err(e);
                }
            }
        }
        for q in ["DELETE FROM relay_chats WHERE chat = ?", "DELETE FROM relay_inbox WHERE chat = ?", "DELETE FROM relay_out WHERE chat = ?"] {
            self.relay_exec(q, vec![chat.into()])?;
        }
        Ok(json!({ "ok": true }))
    }

    /// Its Relay's secret (sealed) and its inbox's token: made once, with
    /// its computer (or the first time it answers a chat, for one made before).
    pub(crate) async fn give_relay(&self, h: &mut Row) -> CellResult<()> {
        if h.relay_sealed.is_none() {
            h.relay_sealed = Some(keys::seal(&self.env, js::random_hex::<32>().as_bytes()).await?);
        }
        if h.inbox.is_none() {
            h.inbox = Some(js::random_hex::<16>());
        }
        Ok(())
    }

    /// Its Relay's secret, opened; stored again when `KEYS` resealed it.
    pub(crate) async fn relay_secret(&self, h: &mut Row) -> CellResult<Option<String>> {
        let Some(sealed) = h.relay_sealed.clone() else { return Ok(None) };
        let opened = keys::open(&self.env, &sealed, "").await?;
        if let Some(fresh) = opened.resealed {
            h.relay_sealed = Some(fresh);
            self.save_step(h)?;
        }
        String::from_utf8(opened.plaintext).map(Some).map_err(|_| CellError::host("its Relay secret is not text"))
    }

    // ---- what its chats deliver ----

    /// `POST /inbox/<token>`: a record of a chat it follows. A message is
    /// kept for Hermes and handed on in its turn; a Stop stops the chat's
    /// running turn; its own records, and anything else, are let be.
    pub(crate) async fn inbox(&self, token: &str, mut req: Request) -> CellResult<Response> {
        let Some(h) = self.row()? else { return Err(CellError::new(ErrorCode::NotFound, "no Hermes here")) };
        if h.inbox.as_deref() != Some(token) || token.is_empty() {
            return Err(CellError::new(ErrorCode::Forbidden, "not this Hermes' inbox"));
        }
        let d: Delivery = serde_json::from_slice(&req.bytes().await?).map_err(|e| CellError::invalid(format!("a delivery: {e}")))?;
        let followed: Vec<Value> = self.relay_rows("SELECT chat FROM relay_chats WHERE chat = ? AND channel = ?", vec![d.fragment.as_str().into(), d.channel.as_str().into()])?;
        if followed.is_empty() || Some(&d.record.principal) == h.identity.as_ref() {
            return Ok(Response::from_json(&json!({ "ok": true, "kept": false }))?);
        }
        match work::said(d.record.body.get()) {
            work::Said::Message(text) => {
                let text: String = text.chars().take(TEXT_MAX_BYTES).collect();
                let name = self.name_of(&d.record.principal).await;
                self.relay_exec(
                    "INSERT INTO relay_inbox (chat, seq, principal, name, text, state, at) VALUES (?, ?, ?, ?, ?, 'pending', ?) ON CONFLICT (chat, seq) DO NOTHING",
                    vec![d.fragment.as_str().into(), SqlStorageValue::Integer(d.record.seq), d.record.principal.as_str().into(), name.as_str().into(), text.into(), SqlStorageValue::Integer(js::now_ms())],
                )?;
                // past the bound, the oldest kept for Hermes go (none it has)
                self.relay_exec(
                    "DELETE FROM relay_inbox WHERE chat = ? AND state = 'pending' AND id NOT IN (SELECT id FROM relay_inbox WHERE chat = ? ORDER BY id DESC LIMIT ?)",
                    vec![d.fragment.as_str().into(), d.fragment.as_str().into(), SqlStorageValue::Integer(PENDING_MAX)],
                )?;
                self.pump(&h, &d.fragment).await?;
            }
            work::Said::Stop { turn } => {
                let running = self.running(&d.fragment)?;
                if let (Some(k), Some(ws)) = (running, self.socket()) {
                    if turn.as_deref().is_none_or(|t| t == wire::turn(k.seq)) {
                        let _ = ws.send_with_str(wire::interrupt(&d.fragment));
                    }
                }
            }
            work::Said::Other => {}
        }
        Ok(Response::from_json(&json!({ "ok": true, "kept": true }))?)
    }

    /// The name Hermes calls a writer by: a person's username, or "a visitor".
    async fn name_of(&self, principal: &str) -> String {
        if !principal.starts_with("id:") {
            return "a visitor".into();
        }
        let asked = crate::ask_registry(&self.env, &calls::Profiles { ids: vec![principal.to_string()] }).await;
        asked.ok().and_then(|a| a.profiles.get(principal).and_then(|p| p.username.clone())).unwrap_or_else(|| "someone".into())
    }

    /// The chat's message Hermes has now (handed on, its turn not ended).
    fn running(&self, chat: &str) -> CellResult<Option<Kept>> {
        Ok(self.relay_rows("SELECT * FROM relay_inbox WHERE chat = ? AND state IN ('sent', 'acked') ORDER BY id LIMIT 1", vec![chat.into()])?.pop())
    }

    /// Hands Hermes the chat's next message, when none of it runs: over its
    /// socket, or, with none, a poke at its computer to come back for it.
    async fn pump(&self, h: &Row, chat: &str) -> CellResult<()> {
        if self.running(chat)?.is_some() {
            return Ok(());
        }
        // the chat's order, whatever order its deliveries came in
        let next: Option<Kept> = self.relay_rows("SELECT * FROM relay_inbox WHERE chat = ? AND state = 'pending' ORDER BY seq LIMIT 1", vec![chat.into()])?.pop();
        let Some(k) = next else { return Ok(()) };
        let Some(ws) = self.socket() else {
            self.wake(h).await;
            return self.arm_within(TICK_MS).await;
        };
        self.hand(h, &ws, &k).await?;
        self.arm_within(TICK_MS).await
    }

    /// One message to Hermes, and its turn's start in the chat; its
    /// computer woken, should it be paused under its socket.
    async fn hand(&self, h: &Row, ws: &WebSocket, k: &Kept) -> CellResult<()> {
        let chat_name = fragment_proto::split_fragment_name(&k.chat).map_or(k.chat.as_str(), |(label, _)| label);
        let m = wire::Inbound { chat: &k.chat, chat_name, seq: k.seq, user_id: &k.principal, user_name: &k.name, text: &k.text };
        ws.send_with_str(wire::inbound(&m, &k.id.to_string()))?;
        if k.state == "pending" {
            self.wake(h).await;
            self.relay_exec("UPDATE relay_inbox SET state = 'sent', sent_at = ? WHERE id = ?", vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(k.id)])?;
            let turn = wire::turn(k.seq);
            if let Err(e) = self.post(h, &k.chat, work::WORK_CHANNEL, &work::record_id(&turn, "start"), work::start(&turn, &k.principal)).await {
                console_error!("{}: the turn's start in {}: {}", h.fragment, k.chat, e.message);
            }
        }
        Ok(())
    }

    /// Its computer woken, as its owner wakes it (the node's
    /// `POST /v1/computers/{name}/wake`, signed by its key: a sleeping one
    /// resumes, an awake one stays awake an idle time from now), at most
    /// every `WAKE_EVERY_MS`.
    async fn wake(&self, h: &Row) {
        if h.key_sealed.is_none() || !h.made {
            return;
        }
        let now = js::now_ms();
        if now - self.state_value("wake_at").unwrap_or(0) < WAKE_EVERY_MS {
            return;
        }
        let _ = self.set_state("wake_at", now);
        let mut h = h.clone();
        let path = format!("/v1/computers/{}/wake", h.computer);
        match self.node(&mut h, "POST", &path, None).await {
            Ok((202, _)) => {}
            Ok((status, answer)) => console_error!("{}: its wake: {status} {answer}", h.fragment),
            Err(e) => console_error!("{}: its wake: {}", h.fragment, e.message),
        }
    }

    async fn arm_within(&self, in_ms: i64) -> CellResult<()> {
        let due = self.state.storage().get_alarm().await?;
        if due.is_none_or(|t| t > js::now_ms() + in_ms) {
            self.arm(in_ms).await?;
        }
        Ok(())
    }

    // ---- its Hermes' socket ----

    /// `GET /relay`, a WebSocket: its Hermes' gateway, with its token. One
    /// at a time: a new dial replaces the one before. A refusal closes
    /// 4401 (`expired`: the gateway dials again; else it stops after two).
    pub(crate) async fn relay_upgrade(&self, req: Request) -> CellResult<Response> {
        let Some(mut h) = self.row()? else { return Err(CellError::new(ErrorCode::NotFound, "no Hermes here")) };
        let secret = self.relay_secret(&mut h).await?;
        let pair = WebSocketPair::new()?;
        let header = req.headers().get("authorization")?;
        let checked = match &secret {
            Some(s) if !matches!(h.phase, fragment_core::hermes::Phase::Remove | fragment_core::hermes::Phase::Gone) => wire::verify(header.as_deref(), &h.fragment, s, js::now_ms() / 1000),
            _ => Err(wire::Refused::Unauthorized),
        };
        if let Err(refused) = checked {
            pair.server.accept()?;
            pair.server.close(Some(4401), Some(refused.reason()))?;
            return Ok(Response::from_websocket(pair.client)?);
        }
        for before in self.state.get_websockets_with_tag(RELAY_TAG) {
            let _ = before.close(Some(4000), Some("replaced by a new dial"));
        }
        self.state.accept_websocket_with_tags(&pair.server, &[RELAY_TAG]);
        Ok(Response::from_websocket(pair.client)?)
    }

    /// Frames from its Hermes, one at a time.
    pub(crate) async fn relay_message(&self, ws: &WebSocket, text: &str) {
        let _one = self.relaying.lock().await;
        if text.len() > wire::FRAME_MAX_BYTES {
            let _ = ws.close(Some(1009), Some("a frame over the bound"));
            return;
        }
        let Ok(Some(h)) = self.row() else {
            let _ = ws.close(Some(4401), Some("unauthorized"));
            return;
        };
        for frame in wire::frames(text) {
            let done = match frame {
                Ok(FromGateway::Hello) => self.hello(&h, ws).await,
                Ok(FromGateway::InboundAck { buffer_id }) => match buffer_id.parse::<i64>() {
                    Ok(id) => self.relay_exec("UPDATE relay_inbox SET state = 'acked' WHERE id = ? AND state = 'sent'", vec![SqlStorageValue::Integer(id)]),
                    Err(_) => Ok(()),
                },
                Ok(FromGateway::GoingIdle) => ws.send_with_str(wire::going_idle_ack()).map_err(CellError::from),
                Ok(FromGateway::Outbound { request_id, action }) => {
                    let answer = match self.act(&h, action).await {
                        Ok(v) => v,
                        Err(e) => json!({ "success": false, "error": e.message }),
                    };
                    ws.send_with_str(wire::result(&request_id, answer)).map_err(CellError::from)
                }
                Ok(FromGateway::Other(_)) => Ok(()),
                Err(why) => {
                    console_error!("{}: a frame from its Hermes: {why}", h.fragment);
                    Ok(())
                }
            };
            if let Err(e) = done {
                console_error!("{}: its Hermes' frame: {}", h.fragment, e.message);
            }
        }
    }

    /// A dial: what this platform is, then each message Hermes was handed
    /// and has not acked (it drops one it has), then each chat's next.
    async fn hello(&self, h: &Row, ws: &WebSocket) -> CellResult<()> {
        ws.send_with_str(wire::descriptor())?;
        let unacked: Vec<Kept> = self.relay_rows("SELECT * FROM relay_inbox WHERE state = 'sent' ORDER BY id", vec![])?;
        for k in &unacked {
            self.hand(h, ws, k).await?;
        }
        let chats: Vec<Value> = self.relay_rows("SELECT DISTINCT chat FROM relay_inbox WHERE state = 'pending'", vec![])?;
        for c in chats.iter().filter_map(|c| c["chat"].as_str()) {
            self.pump(h, c).await?;
        }
        Ok(())
    }

    /// One action its Hermes asks for, answered.
    async fn act(&self, h: &Row, action: Action) -> CellResult<Value> {
        match action {
            Action::Send { chat, content, reply } => {
                if self.relay_rows::<Value>("SELECT chat FROM relay_chats WHERE chat = ?", vec![chat.as_str().into()])?.is_empty() {
                    return Ok(json!({ "success": false, "error": "no chat of this Hermes' by that id" }));
                }
                let n = self.counter("out")?;
                let turn = match self.running(&chat)? {
                    Some(k) => wire::turn(k.seq),
                    // a message of its own (a reminder it set): a turn of its own
                    None => format!("hermes:m{n}"),
                };
                // a reply after another of the turn's: the one before is done
                if reply {
                    self.seal_replies(h, &chat, &turn).await?;
                }
                let id = format!("m{n}");
                self.relay_exec(
                    "INSERT INTO relay_out (id, chat, turn, reply, text, lines, posted, n) VALUES (?, ?, ?, ?, '', 0, 0, ?)",
                    vec![id.as_str().into(), chat.as_str().into(), turn.as_str().into(), SqlStorageValue::Integer(i64::from(reply)), SqlStorageValue::Integer(n)],
                )?;
                let sent = self.sent(&id)?.expect("just written");
                self.content(h, &sent, &content).await?;
                Ok(json!({ "success": true, "message_id": id }))
            }
            Action::Edit { chat, message_id, content } => match self.sent(&message_id)?.filter(|s| s.chat == chat) {
                Some(sent) => {
                    self.content(h, &sent, &content).await?;
                    Ok(json!({ "success": true }))
                }
                None => Ok(json!({ "success": false, "error": "no message of this Hermes' by that id" })),
            },
            Action::Typing { .. } => Ok(json!({ "success": true })),
            Action::Delete { chat, message_id } => {
                if let Some(sent) = self.sent(&message_id)?.filter(|s| s.chat == chat) {
                    self.relay_exec("DELETE FROM relay_out WHERE id = ?", vec![message_id.as_str().into()])?;
                    if sent.reply != 0 {
                        self.draft(h, &chat, &sent.turn, None).await;
                    }
                }
                Ok(json!({ "success": true }))
            }
            Action::React { chat, message_id, emoji, remove } => {
                let Some(seq) = wire::seq_of(&message_id) else { return Ok(json!({ "success": true })) };
                if emoji == wire::STARTED && remove {
                    self.end_turn(h, &chat, seq, None).await?;
                } else if emoji == wire::FAILED && !remove {
                    let turn = wire::turn(seq);
                    self.post(h, &chat, work::WORK_CHANNEL, &work::record_id(&turn, "failed"), work::end(&turn, "error", Some("Hermes' turn failed"))).await?;
                }
                Ok(json!({ "success": true }))
            }
            Action::Unsupported { op } => Ok(json!({ "success": false, "error": format!("unsupported op {op}") })),
        }
    }

    fn sent(&self, id: &str) -> CellResult<Option<Sent>> {
        Ok(self.relay_rows("SELECT * FROM relay_out WHERE id = ?", vec![id.into()])?.pop())
    }

    /// A message's text now: a reply's as a draft, its tool progress's new
    /// lines as the turn's steps.
    async fn content(&self, h: &Row, sent: &Sent, content: &str) -> CellResult<()> {
        if sent.reply != 0 {
            let (text, streaming) = wire::uncursored(content);
            self.relay_exec("UPDATE relay_out SET text = ? WHERE id = ?", vec![text.into(), sent.id.as_str().into()])?;
            self.draft(h, &sent.chat, &sent.turn, Some(text)).await;
            // a reply no chat's turn runs (a reminder it set) stands once whole
            if !streaming && self.running(&sent.chat)?.is_none() {
                self.seal_replies(h, &sent.chat, &sent.turn).await?;
            }
            return Ok(());
        }
        let lines = wire::new_lines(content, usize::try_from(sent.lines).unwrap_or(0));
        if lines.is_empty() {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct N {
            n: i64,
        }
        let done: Vec<N> = self.relay_rows("SELECT COALESCE(SUM(lines), 0) AS n FROM relay_out WHERE turn = ? AND reply = 0", vec![sent.turn.as_str().into()])?;
        let mut step = usize::try_from(done.first().map_or(0, |d| d.n)).unwrap_or(0);
        for line in &lines {
            step += 1;
            let call = work::Call { name: work::cut(line, work::ARGS_MAX_CHARS), args: String::new(), text: String::new(), result: None };
            let outcome = work::Outcome { ok: true, text: String::new() };
            self.post(h, &sent.chat, work::WORK_CHANNEL, &work::record_id(&sent.turn, &step.to_string()), work::step(&sent.turn, step, &call, &outcome)).await?;
        }
        self.relay_exec("UPDATE relay_out SET lines = lines + ? WHERE id = ?", vec![SqlStorageValue::Integer(lines.len() as i64), sent.id.as_str().into()])
    }

    /// The turn's replies not yet in the chat, written there now (each a
    /// record with the turn), the draft cleared.
    async fn seal_replies(&self, h: &Row, chat: &str, turn: &str) -> CellResult<bool> {
        let open: Vec<Sent> = self.relay_rows("SELECT * FROM relay_out WHERE chat = ? AND turn = ? AND reply = 1 AND posted = 0 ORDER BY n", vec![chat.into(), turn.into()])?;
        let any = !open.is_empty();
        for s in open.iter().filter(|s| !s.text.trim().is_empty()) {
            self.post(h, chat, "chat", &format!("hm:{}:{}", s.turn, s.id), work::answer(&s.text, Some(&s.turn))).await?;
        }
        for s in &open {
            self.relay_exec("UPDATE relay_out SET posted = 1 WHERE id = ?", vec![s.id.as_str().into()])?;
        }
        if any {
            self.draft(h, chat, turn, None).await;
        }
        Ok(open.iter().any(|s| !s.text.trim().is_empty()))
    }

    /// The turn of the chat's message `seq` ended (its `👀` came off, or it
    /// ran past `TURN_MAX_MS`): its replies written, its end, and the chat's
    /// next message to Hermes.
    async fn end_turn(&self, h: &Row, chat: &str, seq: i64, outcome: Option<&str>) -> CellResult<()> {
        let turn = wire::turn(seq);
        let answered = self.seal_replies(h, chat, &turn).await?;
        let replied: Vec<Value> = self.relay_rows("SELECT id FROM relay_out WHERE turn = ? AND reply = 1 AND posted = 1 AND text != ''", vec![turn.as_str().into()])?;
        let outcome = outcome.unwrap_or(if answered || !replied.is_empty() { "done" } else { "stopped" });
        self.post(h, chat, work::WORK_CHANNEL, &work::record_id(&turn, "end"), work::end(&turn, outcome, None)).await?;
        self.relay_exec("DELETE FROM relay_inbox WHERE chat = ? AND seq = ?", vec![chat.into(), SqlStorageValue::Integer(seq)])?;
        self.relay_exec("DELETE FROM relay_out WHERE turn = ?", vec![turn.as_str().into()])?;
        self.pump(h, chat).await
    }

    /// The alarm's look: a turn past `TURN_MAX_MS` ends; a message waiting
    /// with no socket pokes its computer again. `Some(ms)` while anything
    /// waits on Hermes.
    pub(crate) async fn relay_tick(&self) -> CellResult<Option<i64>> {
        let _one = self.relaying.lock().await;
        let Some(h) = self.row()? else { return Ok(None) };
        let stale: Vec<Kept> = self.relay_rows(
            "SELECT * FROM relay_inbox WHERE state IN ('sent', 'acked') AND sent_at < ?",
            vec![SqlStorageValue::Integer(js::now_ms() - TURN_MAX_MS)],
        )?;
        for k in &stale {
            self.end_turn(&h, &k.chat, k.seq, Some("timeout")).await?;
        }
        let waiting: Vec<Value> = self.relay_rows("SELECT id FROM relay_inbox LIMIT 1", vec![])?;
        if waiting.is_empty() {
            return Ok(None);
        }
        if self.socket().is_none() {
            self.wake(&h).await;
        } else {
            let chats: Vec<Value> = self.relay_rows("SELECT DISTINCT chat FROM relay_inbox WHERE state = 'pending'", vec![])?;
            for c in chats.iter().filter_map(|c| c["chat"].as_str()) {
                self.pump(&h, c).await?;
            }
        }
        Ok(Some(TICK_MS))
    }

    /// Its Hermes went away (it was removed): its socket closed for good.
    pub(crate) fn relay_revoked(&self) {
        for ws in self.state.get_websockets_with_tag(RELAY_TAG) {
            let _ = ws.close(Some(4401), Some("unauthorized"));
        }
    }
}

trait FilterErr<T> {
    fn filter_err(self, keep: impl Fn(&CellError) -> bool) -> CellResult<()>;
}

impl<T> FilterErr<T> for CellResult<T> {
    /// An error `keep` says matters; any other is let be.
    fn filter_err(self, keep: impl Fn(&CellError) -> bool) -> CellResult<()> {
        match self {
            Err(e) if keep(&e) => Err(e),
            _ => Ok(()),
        }
    }
}
