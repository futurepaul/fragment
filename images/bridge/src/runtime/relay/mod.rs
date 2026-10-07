//! The `relay` runtime: the bridge as Hermes' Relay connector (decision 21;
//! the wire in `wire`). Hermes' one multiplexed gateway dials
//! `ws://<listen>/relay` with its token; the bridge hands it each turn as an
//! inbound message routed to the agent's profile, and turns what Hermes
//! sends back into the bridge's runtime events:
//!
//! - a reply streams as `draft` frames, then arrives as a `send` that
//!   answers the turn's message: a reply part;
//! - its tool progress is a `send` answering nothing whose lines grow by
//!   `edit`s: each new line a step;
//! - the model's text beside a tool call arrives as a reply too (Hermes
//!   ends a draft segment at every tool boundary with such a `send`): the
//!   step that follows takes it back (`Event::Retract`) as its words, and
//!   one whose drafts began before a step is that step's words when it
//!   comes. A turn that ends idle having said nothing since says the last
//!   words a step took as its reply;
//! - an approval is a `prompt` frame; the owner's answer goes back as an
//!   inbound `prompt_response`, at once, mid-turn;
//! - a question to answer in words (an open `clarify`, `❓ …`, or the
//!   `✏️ Type your answer:` after "Other") is a `send`: a reply part, and
//!   the turn asks (`Event::Asked`); the asker's next message comes back as
//!   an inbound in the same chat at once (`Command::Tell`), which Hermes'
//!   clarify intercept takes as the answer mid-turn. A Stop while it asks
//!   interrupts, then answers "Stop." so the waiting clarify lets go;
//! - `👀` on, then off as the turn ends, then `✅` or `❌` (Hermes'
//!   processing hooks; Relay has no other end): a turn ends at its `❌`, at
//!   its `✅` once it said something, and, stopped, at its `👀` off (a
//!   cancelled bracket gets neither). A message Hermes took while its
//!   gateway was starting is bracketed twice, first empty (queued behind
//!   its startup restore, then run): that `✅` ends nothing, and Hermes
//!   ends no person's turn without saying something. No clock: a bracket
//!   that never ends is the engine's idle bound's;
//! - a file is uploaded to `/relay/media`, then sent by `send_media`;
//!   a message's attachments are re-hosted at `/relay/media/<id>`.
//!
//! Every inbound is kept until Hermes acks it and handed again on its next
//! dial (Hermes drops one it saw, by message id). A new dial replaces the
//! one before.

pub mod wire;

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Limited};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::Message;

use crate::limits;
use crate::net::{self, Body};
use crate::records::{Outcome, Step};
use crate::runtime::{Command, Event, LocalFile, Runtime, RuntimeError, RuntimeFuture, RuntimeIo, TurnStart};
use wire::{Action, FromGateway};

/// Files kept for Hermes (uploads and re-hosted attachments), at most; past
/// it the oldest go.
pub const MEDIA_KEPT_MAX: usize = 256;
/// Messages of a turn taken as its steps' words that the bridge remembers,
/// at most (only so their edits change nothing; Hermes edits none once sent).
pub const TAKEN_KEPT_MAX: usize = 64;

#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// Where Hermes dials (`GATEWAY_RELAY_URL` is `http://<listen>`).
    pub listen: SocketAddr,
    /// `GATEWAY_RELAY_ID` and `GATEWAY_RELAY_SECRET`, the same on both sides.
    pub gateway_id: String,
    pub secret: String,
    /// Where uploads land (scratch, not `/data`).
    pub media_dir: PathBuf,
}

pub struct Relay {
    pub config: RelayConfig,
}

/// A file Hermes may fetch, or one it uploaded.
#[derive(Debug, Clone)]
struct Media {
    file: LocalFile,
}

#[derive(Default)]
struct MediaTable {
    by_id: HashMap<String, Media>,
    order: Vec<String>,
    uploads: u64,
}

impl MediaTable {
    fn insert(&mut self, id: String, m: Media) {
        if self.by_id.insert(id.clone(), m).is_none() {
            self.order.push(id);
        }
        // bounded by MEDIA_KEPT_MAX: each pass removes one
        while self.order.len() > MEDIA_KEPT_MAX {
            let old = self.order.remove(0);
            if let Some(m) = self.by_id.remove(&old) {
                if old.starts_with("up-") {
                    let _ = std::fs::remove_file(&m.file.path);
                }
            }
        }
    }
}

/// What the connections tell the runtime's loop.
enum Wire {
    Connected { conn: u64, tx: mpsc::Sender<Message> },
    Frame { conn: u64, text: String },
    Closed { conn: u64 },
}

/// A turn Hermes was handed.
struct Inflight {
    start: TurnStart,
    chat: String,
    profile: String,
    order: u64,
    frame: String,
    acked: bool,
    /// Its inbound went out on a greeted connection at least once.
    sent: bool,
    /// Reply messages, by the id the bridge gave them, and their parts.
    parts: HashMap<String, u32>,
    /// Progress messages, and how many of their lines are steps already.
    progress: HashMap<String, usize>,
    next_part: u32,
    /// It said something: a reply, a draft, a step, a prompt, a file (so
    /// its next `✅` is its end, not the startup gate's empty bracket's).
    said: bool,
    stopped: bool,
    /// It asked its asker something to answer in words, not yet told.
    asking: bool,
    /// The reply part last emitted, while the engine holds it open (no
    /// step, part, prompt or end since): its message id, part and text.
    /// Hermes ends a draft segment at every tool boundary with a `send`
    /// answering the turn, so the model's text before a tool call arrives
    /// as a reply; a step that follows takes it back as its words.
    open_reply: Option<(String, u32, String)>,
    /// Draft frames came since its last reply `send`, and whether a step
    /// came meanwhile: the `send` ending those drafts is then that step's
    /// words, not a reply (its tool progress can reach the bridge before
    /// the `send` that ends the segment before it).
    drafting: Option<bool>,
    /// The words a step took, while nothing was said after them: a turn
    /// that ends idle having said nothing since says them as its reply
    /// (Hermes' answer beside a housekeeping tool, which it sends once).
    narration: Option<String>,
    /// Messages taken as a step's words: an edit of one changes nothing.
    taken: Vec<String>,
}

impl Inflight {
    /// A message taken as a step's words (its edits change nothing), the
    /// oldest let go past `TAKEN_KEPT_MAX`.
    fn take_message(&mut self, id: String) {
        if self.taken.len() >= TAKEN_KEPT_MAX {
            self.taken.remove(0);
        }
        self.taken.push(id);
    }

    /// The end a reaction on its message says (the module's doc): `❌`;
    /// `✅` once it said something; once stopped, its `👀` off or either.
    fn ended_by(&self, emoji: &str, remove: bool) -> Option<Outcome> {
        match (emoji, remove) {
            (wire::STARTED, true) | (wire::DONE | wire::FAILED, false) if self.stopped => Some(Outcome::Stopped),
            (wire::FAILED, false) => Some(Outcome::Error("Hermes' turn failed".into())),
            (wire::DONE, false) if self.said => Some(Outcome::Idle),
            _ => None,
        }
    }
}

/// A prompt's answer, kept until Hermes acks it.
struct PendingAnswer {
    buffer: String,
    frame: String,
}

impl Runtime for Relay {
    fn name(&self) -> &'static str {
        "relay"
    }

    fn run(self: Box<Self>, io: RuntimeIo) -> RuntimeFuture {
        Box::pin(run(self.config, io))
    }
}

async fn run(cfg: RelayConfig, mut io: RuntimeIo) -> Result<(), RuntimeError> {
    std::fs::create_dir_all(&cfg.media_dir).map_err(|e| RuntimeError::Setup(format!("{}: {e}", cfg.media_dir.display())))?;
    let listener = tokio::net::TcpListener::bind(cfg.listen).await.map_err(|e| RuntimeError::Setup(format!("listen {}: {e}", cfg.listen)))?;
    crate::ev!("relay.listening", { "listen": cfg.listen.to_string() });
    let media = Arc::new(Mutex::new(MediaTable::default()));
    let (wire_tx, mut wire_rx) = mpsc::channel::<Wire>(256);
    let conns = Arc::new(std::sync::atomic::AtomicU64::new(0));
    {
        let (cfg, media, wire_tx, conns) = (cfg.clone(), media.clone(), wire_tx.clone(), conns.clone());
        let handler = move |req: Request<Incoming>, _peer: SocketAddr| {
            let (cfg, media, wire_tx, conns) = (cfg.clone(), media.clone(), wire_tx.clone(), conns.clone());
            async move { handle(req, cfg, media, wire_tx, conns).await }
        };
        tokio::spawn(net::serve(listener, handler, io.shutdown.clone()));
    }

    let mut st = Loop { cfg, events: io.events.clone(), media, inflight: HashMap::new(), by_chat: HashMap::new(), answers: Vec::new(), conn: None, greeted: false, next_message: 0, next_order: 0 };
    let mut shutdown = io.shutdown.clone();
    // bounded by the bridge's life: one input per pass, ended by shutdown
    loop {
        tokio::select! {
            c = io.commands.recv() => match c {
                Some(c) => st.command(c).await,
                None => return Ok(()),
            },
            w = wire_rx.recv() => match w {
                Some(w) => st.wire(w).await,
                None => return Err(RuntimeError::Failed("the relay listener ended".into())),
            },
            _ = crate::net::stopped(&mut shutdown) => {
                if let Some((_, tx)) = st.conn.take() {
                    let _ = tx.try_send(Message::Close(Some(CloseFrame { code: CloseCode::Away, reason: "the computer is stopping".into() })));
                }
                return Ok(());
            }
        }
    }
}

fn bearer_ok(req: &Request<Incoming>, cfg: &RelayConfig) -> Result<(), wire::Refused> {
    let header = req.headers().get("authorization").and_then(|v| v.to_str().ok());
    wire::verify(header, &cfg.gateway_id, &cfg.secret, i64::try_from(crate::log::now_ms() / 1000).expect("seconds fit"))
}

async fn handle(mut req: Request<Incoming>, cfg: RelayConfig, media: Arc<Mutex<MediaTable>>, wire_tx: mpsc::Sender<Wire>, conns: Arc<std::sync::atomic::AtomicU64>) -> Response<Body> {
    let path = req.uri().path().to_string();
    if path == "/relay" {
        let checked = bearer_ok(&req, &cfg);
        let Some((response, socket)) = net::accept_ws(&mut req) else {
            return net::refusal(StatusCode::BAD_REQUEST, "invalid", "the relay is a WebSocket");
        };
        tokio::spawn(async move {
            let Some(ws) = socket.await else { return };
            match checked {
                Err(refused) => {
                    crate::ev!("relay.refused", { "why": refused.reason() });
                    let (mut sink, _) = ws.split();
                    let _ = sink.send(Message::Close(Some(CloseFrame { code: CloseCode::from(4401), reason: refused.reason().into() }))).await;
                }
                Ok(()) => connection(ws, wire_tx, conns.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1).await,
            }
        });
        return response;
    }
    if path == "/relay/media" && req.method() == Method::POST {
        if bearer_ok(&req, &cfg).is_err() {
            return net::refusal(StatusCode::UNAUTHORIZED, "unauthenticated", "the relay's token");
        }
        let media_type = req.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("application/octet-stream").to_string();
        let name = req.headers().get("x-media-filename").and_then(|v| v.to_str().ok()).unwrap_or("file").chars().filter(|c| !c.is_control() && *c != '/').take(200).collect::<String>();
        let max = usize::try_from(limits::ATTACHMENT_MAX_BYTES).expect("fits");
        let bytes = match Limited::new(req.into_body(), max).collect().await {
            Ok(b) => b.to_bytes(),
            Err(_) => return net::refusal(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "at most 25 MiB"),
        };
        if bytes.is_empty() {
            return net::refusal(StatusCode::BAD_REQUEST, "invalid", "an empty file");
        }
        let id = {
            let mut t = media.lock().expect("the media table");
            t.uploads += 1;
            format!("up-{}", t.uploads)
        };
        let path = cfg.media_dir.join(&id);
        if let Err(e) = tokio::fs::write(&path, &bytes).await {
            return net::refusal(StatusCode::INTERNAL_SERVER_ERROR, "host", &e.to_string());
        }
        let file = LocalFile { path, media_type, name: if name.is_empty() { "file".into() } else { name }, size: bytes.len() as u64 };
        media.lock().expect("the media table").insert(id.clone(), Media { file });
        crate::ev!("relay.media_uploaded", { "id": id, "size": bytes.len() });
        return net::json_answer(StatusCode::OK, &json!({ "id": id }));
    }
    if let Some(id) = path.strip_prefix("/relay/media/") {
        if bearer_ok(&req, &cfg).is_err() {
            return net::refusal(StatusCode::UNAUTHORIZED, "unauthenticated", "the relay's token");
        }
        let found = media.lock().expect("the media table").by_id.get(id).cloned();
        let Some(m) = found else { return net::refusal(StatusCode::NOT_FOUND, "not_found", "no such media") };
        return match tokio::fs::read(&m.file.path).await {
            Ok(bytes) => Response::builder()
                .status(StatusCode::OK)
                .header("content-type", m.file.media_type.as_str())
                .header("content-disposition", format!("attachment; filename=\"{}\"", m.file.name.replace('"', "")))
                .body(http_body_util::Full::new(Bytes::from(bytes)))
                .expect("a well-formed answer"),
            Err(e) => net::refusal(StatusCode::NOT_FOUND, "not_found", &e.to_string()),
        };
    }
    net::refusal(StatusCode::NOT_FOUND, "not_found", "the relay answers /relay and /relay/media")
}

/// One dial: frames in to the loop, frames out from it, until either ends.
async fn connection(ws: net::ServerWs, wire_tx: mpsc::Sender<Wire>, conn: u64) {
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::channel::<Message>(256);
    if wire_tx.send(Wire::Connected { conn, tx }).await.is_err() {
        return;
    }
    crate::ev!("relay.connected", { "conn": conn });
    let writer = tokio::spawn(async move {
        // bounded by the connection: ends when the loop drops its sender
        while let Some(m) = rx.recv().await {
            let closing = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || closing {
                break;
            }
        }
        let _ = sink.close().await;
    });
    // bounded by the connection: ends when the peer closes or errs
    while let Some(m) = stream.next().await {
        match m {
            Ok(Message::Text(t)) => {
                if wire_tx.send(Wire::Frame { conn, text: t.to_string() }).await.is_err() {
                    break;
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
    writer.abort();
    let _ = wire_tx.send(Wire::Closed { conn }).await;
    crate::ev!("relay.closed", { "conn": conn });
}

struct Loop {
    cfg: RelayConfig,
    events: mpsc::Sender<Event>,
    media: Arc<Mutex<MediaTable>>,
    inflight: HashMap<String, Inflight>,
    by_chat: HashMap<String, String>,
    answers: Vec<PendingAnswer>,
    conn: Option<(u64, mpsc::Sender<Message>)>,
    greeted: bool,
    next_message: u64,
    next_order: u64,
}

impl Loop {
    async fn emit(&self, e: Event) {
        // The bridge's engine drains these; a closed queue means it is
        // shutting down, and the event no longer matters.
        let _ = self.events.send(e).await;
    }

    /// Hermes is (or is no longer) on a greeted connection: the bridge
    /// claims turns only while it is, so a turn is claimed by a life whose
    /// Hermes is there to take it (one handed meanwhile is kept until
    /// acked, and handed again on the next dial).
    async fn greet(&mut self, greeted: bool) {
        if self.greeted == greeted {
            return;
        }
        self.greeted = greeted;
        self.emit(Event::Connected(greeted)).await;
    }

    /// Sends a frame on the greeted connection: whether it went.
    fn send(&self, frame: String) -> bool {
        let Some((_, tx)) = &self.conn else { return false };
        if !self.greeted {
            return false;
        }
        let sent = tx.try_send(Message::text(frame)).is_ok();
        if !sent {
            crate::ev!("relay.send_dropped", { "why": "the connection's queue is full or closed; it is handed again on the next dial" });
        }
        sent
    }

    async fn command(&mut self, c: Command) {
        match c {
            Command::Start(ts) => self.start(*ts),
            Command::Stop { turn } => {
                let Some(f) = self.inflight.get_mut(&turn) else { return };
                if f.acked || f.sent {
                    // Hermes has it (or may): interrupt it now, and again at
                    // its ack or the next dial, should this one be lost.
                    f.stopped = true;
                    let frame = wire::interrupt(&f.profile, &f.chat);
                    // a clarify waiting on the person's words never sees
                    // the interrupt: words let it go, and the turn stops
                    let asking = f.asking.then(|| (f.start.asker.clone(), f.start.asker_name.clone()));
                    crate::ev!("relay.interrupt", { "turn": turn });
                    self.send(frame);
                    if let Some((by, by_name)) = asking {
                        self.tell(&turn, 0, &by, &by_name, wire::STOP_WORDS);
                    }
                } else {
                    // Never sent: it never starts.
                    self.forget(&turn);
                    self.emit(Event::End { turn, outcome: Outcome::Stopped }).await;
                }
            }
            Command::Answer { turn, prompt, option, seq, by } => {
                // An expiry needs no word to Hermes: its own approval
                // timeout (the image sets it to the bridge's prompt TTL)
                // fails the command closed.
                let (Some(option), Some(f)) = (option, self.inflight.get(&turn)) else { return };
                let message_id = format!("{turn}-a{seq}");
                let m = wire::Inbound { chat: &f.chat, chat_name: &f.start.chat_name, profile: &f.profile, message_id: &message_id, user_id: &by, user_name: "owner", text: "", media: &[], context: None };
                let buffer = format!("a-{turn}-{seq}");
                let frame = wire::prompt_answer(&m, &buffer, &prompt, &option);
                let _ = self.send(frame.clone());
                self.answers.push(PendingAnswer { buffer, frame });
            }
            Command::Forget { turn } => self.forget(&turn),
            Command::Tell { turn, seq, by, by_name, text } => self.tell(&turn, seq, &by, &by_name, &text),
        }
    }

    /// The asker's words for a turn that asked them (`Command::Tell`): an
    /// inbound in the turn's chat, kept until Hermes acks it.
    fn tell(&mut self, turn: &str, seq: u64, by: &str, by_name: &str, text: &str) {
        let Some(f) = self.inflight.get_mut(turn) else { return };
        f.asking = false;
        let message_id = format!("{turn}-t{seq}");
        let m = wire::Inbound { chat: &f.chat, chat_name: &f.start.chat_name, profile: &f.profile, message_id: &message_id, user_id: by, user_name: by_name, text, media: &[], context: None };
        let buffer = format!("t-{turn}-{seq}");
        let frame = wire::inbound(&m, &buffer);
        crate::ev!("relay.told", { "turn": turn, "seq": seq });
        let _ = self.send(frame.clone());
        self.answers.push(PendingAnswer { buffer, frame });
    }

    fn forget(&mut self, turn: &str) {
        if let Some(f) = self.inflight.remove(turn) {
            if self.by_chat.get(&f.chat).is_some_and(|t| t == turn) {
                self.by_chat.remove(&f.chat);
            }
        }
    }

    fn start(&mut self, ts: TurnStart) {
        let chat = wire::chat_id(&ts.fragment, &ts.agent.fragment);
        let profile = wire::profile(&ts.agent.fragment);
        if let Some(old) = self.by_chat.get(&chat).cloned() {
            // The engine runs one turn of an agent in a chat at a time; a
            // second one here means its Forget never came.
            crate::ev!("relay.replaced", { "turn": old, "by": ts.turn });
            self.forget(&old);
        }
        let mut hosted = Vec::new();
        {
            let mut t = self.media.lock().expect("the media table");
            for (i, f) in ts.files.iter().enumerate() {
                let id = format!("in-{}-{i}", ts.turn);
                let url = format!("http://{}/relay/media/{id}", self.cfg.listen);
                t.insert(id, Media { file: f.clone() });
                hosted.push((url, f.media_type.clone()));
            }
        }
        // a turn after a cut one is told so, as read-only context beside
        // the message (TurnStart::note); Hermes' own session was closed at
        // boot (hermes-boot, `close_cut_turns`), so the message is a turn of
        // its own, never folded into the cut one's
        let m = wire::Inbound { chat: &chat, chat_name: &ts.chat_name, profile: &profile, message_id: &ts.turn, user_id: &ts.asker, user_name: &ts.asker_name, text: &ts.text, media: &hosted, context: ts.note.as_deref() };
        let frame = wire::inbound(&m, &ts.turn);
        self.next_order += 1;
        let turn = ts.turn.clone();
        let sent = self.send(frame.clone());
        self.inflight.insert(turn.clone(), Inflight { start: ts, chat: chat.clone(), profile, order: self.next_order, frame, acked: false, sent, parts: HashMap::new(), progress: HashMap::new(), next_part: 1, said: false, stopped: false, asking: false, open_reply: None, drafting: None, narration: None, taken: Vec::new() });
        self.by_chat.insert(chat, turn.clone());
        crate::ev!("relay.inbound", { "turn": turn, "sent": sent });
    }

    async fn wire(&mut self, w: Wire) {
        match w {
            Wire::Connected { conn, tx } => {
                if let Some((old, old_tx)) = self.conn.replace((conn, tx)) {
                    crate::ev!("relay.replaced_dial", { "conn": old });
                    let _ = old_tx.try_send(Message::Close(Some(CloseFrame { code: CloseCode::from(4000), reason: "replaced by a new dial".into() })));
                }
                self.greet(false).await;
            }
            Wire::Closed { conn } => {
                if self.conn.as_ref().is_some_and(|(c, _)| *c == conn) {
                    self.conn = None;
                    self.greet(false).await;
                }
            }
            Wire::Frame { conn, text } => {
                if !self.conn.as_ref().is_some_and(|(c, _)| *c == conn) {
                    return;
                }
                if text.len() > limits::FRAME_MAX_BYTES {
                    crate::ev!("relay.frame_refused", { "bytes": text.len() });
                    return;
                }
                for f in wire::frames(&text) {
                    match f {
                        Ok(frame) => self.frame(frame).await,
                        Err(why) => crate::ev!("relay.frame_invalid", { "why": why }),
                    }
                }
            }
        }
    }

    async fn frame(&mut self, f: FromGateway) {
        match f {
            FromGateway::Hello => {
                self.greet(true).await;
                if let Some((_, tx)) = &self.conn {
                    let _ = tx.try_send(Message::text(wire::descriptor()));
                }
                // Every inbound not acked, in the order handed, then every
                // answer, then every Stop: Hermes drops what it already saw.
                let mut unacked: BTreeMap<u64, (String, String)> = BTreeMap::new();
                for (id, f) in self.inflight.iter().filter(|(_, f)| !f.acked) {
                    unacked.insert(f.order, (id.clone(), f.frame.clone()));
                }
                for (id, frame) in unacked.into_values() {
                    let sent = self.send(frame);
                    if let Some(f) = self.inflight.get_mut(&id) {
                        f.sent |= sent;
                    }
                }
                for a in &self.answers {
                    let _ = self.send(a.frame.clone());
                }
                let stops: Vec<String> = self.inflight.values().filter(|f| f.stopped && f.acked).map(|f| wire::interrupt(&f.profile, &f.chat)).collect();
                for frame in stops {
                    let _ = self.send(frame);
                }
            }
            FromGateway::InboundAck { buffer_id } => {
                if let Some(f) = self.inflight.get_mut(&buffer_id) {
                    if !f.acked {
                        f.acked = true;
                        // Stopped before Hermes took it: tell it now.
                        let stop = f.stopped.then(|| wire::interrupt(&f.profile, &f.chat));
                        if let Some(frame) = stop {
                            let _ = self.send(frame);
                        }
                        crate::ev!("relay.acked", { "turn": buffer_id });
                    }
                } else {
                    self.answers.retain(|a| a.buffer != buffer_id);
                }
            }
            FromGateway::GoingIdle => {
                let _ = self.send(wire::going_idle_ack());
            }
            FromGateway::Outbound { request_id, action } => {
                let answer = self.act(action).await;
                let _ = self.send(wire::result(&request_id, answer));
            }
            FromGateway::Other(_) => {}
        }
    }

    fn message_id(&mut self) -> String {
        self.next_message += 1;
        format!("m{}", self.next_message)
    }

    async fn act(&mut self, action: Action) -> Value {
        let ok = json!({ "success": true });
        // One line per op Hermes asks for: its kind and ids, never its text.
        match &action {
            Action::Send { chat, content, reply } => crate::ev!("relay.op", { "op": "send", "chat": chat, "reply": reply, "chars": content.chars().count(), "turn": self.by_chat.get(chat) }),
            Action::Edit { chat, message_id, content } => crate::ev!("relay.op", { "op": "edit", "chat": chat, "message": message_id, "chars": content.chars().count() }),
            Action::React { chat, message_id, emoji, remove } => crate::ev!("relay.op", { "op": "react", "chat": chat, "message": message_id, "emoji": emoji, "remove": remove, "turn": self.by_chat.get(chat) }),
            Action::Draft { chat, draft_id, content, done } => crate::ev!("relay.op", { "op": "draft", "chat": chat, "draft": draft_id, "final": done, "chars": content.chars().count() }),
            Action::Prompt { chat, prompt_id, options, .. } => crate::ev!("relay.op", { "op": "prompt", "chat": chat, "prompt": prompt_id, "options": options.len() }),
            Action::Unsupported { op } => crate::ev!("relay.op", { "op": op, "supported": false }),
            other => crate::ev!("relay.op", { "op": format!("{other:?}").split([' ', '{']).next().unwrap_or("").to_lowercase() }),
        }
        match &action {
            Action::Send { chat, .. } | Action::Edit { chat, .. } | Action::Draft { chat, .. } | Action::Prompt { chat, .. } | Action::SendMedia { chat, .. } => self.spoke(chat),
            _ => {}
        }
        let turn_of = |chat: &str, by_chat: &HashMap<String, String>| by_chat.get(chat).cloned();
        match action {
            Action::Send { chat, content, reply } => {
                let id = self.message_id();
                let (text, _) = wire::uncursored(&content);
                let text = text.to_string();
                let Some(turn) = turn_of(&chat, &self.by_chat) else {
                    let Some((fragment, agent)) = wire::split_chat_id(&chat) else { return json!({ "success": false, "error": "no chat by that id" }) };
                    self.emit(Event::Say { agent: agent.to_string(), fragment: fragment.to_string(), text }).await;
                    return json!({ "success": true, "message_id": id });
                };
                let f = self.inflight.get_mut(&turn).expect("by_chat names a held turn");
                if let Some(question) = wire::question(&text) {
                    // asked in words: the question shows as the agent's,
                    // and the asker's next message is its answer
                    let part = f.next_part;
                    f.next_part += 1;
                    f.parts.insert(id.clone(), part);
                    f.asking = true;
                    (f.open_reply, f.drafting, f.narration) = (None, None, None);
                    self.emit(Event::Reply { turn: turn.clone(), part, text: question }).await;
                    self.emit(Event::Asked { turn }).await;
                } else if reply && f.drafting.take() == Some(true) {
                    // its drafts began before a step: the send ending them
                    // is that step's words, already posted, never a reply
                    f.take_message(id.clone());
                    f.narration = Some(text);
                    crate::ev!("relay.narration", { "turn": turn, "after_step": true });
                } else if reply {
                    let part = f.next_part;
                    f.next_part += 1;
                    f.parts.insert(id.clone(), part);
                    f.open_reply = Some((id.clone(), part, text.clone()));
                    f.narration = None;
                    self.emit(Event::Reply { turn, part, text }).await;
                } else {
                    let steps = wire::new_steps(&content, 0);
                    f.progress.insert(id.clone(), steps.len());
                    self.steps(&turn, steps).await;
                }
                json!({ "success": true, "message_id": id })
            }
            Action::Edit { chat, message_id, content } => {
                let Some(turn) = turn_of(&chat, &self.by_chat) else { return ok };
                let f = self.inflight.get_mut(&turn).expect("by_chat names a held turn");
                if let Some(part) = f.parts.get(&message_id).copied() {
                    let (text, _) = wire::uncursored(&content);
                    if let Some(open) = f.open_reply.as_mut().filter(|o| o.0 == message_id) {
                        open.2 = text.to_string();
                    }
                    self.emit(Event::Reply { turn, part, text: text.to_string() }).await;
                } else if let Some(seen) = f.progress.get(&message_id).copied() {
                    let steps = wire::new_steps(&content, seen);
                    f.progress.insert(message_id, seen + steps.len());
                    self.steps(&turn, steps).await;
                } else if f.taken.contains(&message_id) {
                    return ok;
                } else {
                    return json!({ "success": false, "error": "no message of this turn by that id" });
                }
                ok
            }
            Action::Delete { chat, message_id } => {
                if let Some(turn) = turn_of(&chat, &self.by_chat) {
                    let part = self.inflight.get(&turn).and_then(|f| f.parts.get(&message_id).copied());
                    if let Some(part) = part {
                        self.emit(Event::Retract { turn, part }).await;
                    }
                }
                ok
            }
            Action::Typing { .. } => ok,
            Action::React { chat, message_id, emoji, remove } => {
                let Some(turn) = turn_of(&chat, &self.by_chat) else { return ok };
                if message_id != turn {
                    return ok;
                }
                let ended = self.inflight.get(&turn).expect("held").ended_by(&emoji, remove);
                if let Some(outcome) = ended {
                    self.end(&turn, outcome).await;
                }
                ok
            }
            Action::Draft { chat, content, .. } => {
                if let Some(turn) = turn_of(&chat, &self.by_chat) {
                    let f = self.inflight.get_mut(&turn).expect("by_chat names a held turn");
                    f.drafting.get_or_insert(false);
                    let (text, _) = wire::uncursored(&content);
                    self.emit(Event::Draft { turn, text: text.to_string() }).await;
                }
                ok
            }
            Action::Prompt { chat, prompt_id, content, options, timeout_s } => {
                let Some(turn) = turn_of(&chat, &self.by_chat) else { return json!({ "success": false, "error": "no turn runs in that chat" }) };
                let id = self.message_id();
                // the engine posts an open reply before the card
                self.inflight.get_mut(&turn).expect("by_chat names a held turn").open_reply = None;
                self.emit(Event::Prompt { turn, prompt: prompt_id, text: content, options, ttl_ms: timeout_s.map(|s| s.saturating_mul(1000)) }).await;
                json!({ "success": true, "message_id": id })
            }
            Action::SendMedia { chat, source_url, caption, filename, .. } => {
                let Some(turn) = turn_of(&chat, &self.by_chat) else { return json!({ "success": false, "error": "no turn runs in that chat" }) };
                let media_id = source_url.rsplit_once("/relay/media/").map(|(_, id)| id.to_string()).unwrap_or_default();
                let found = self.media.lock().expect("the media table").by_id.get(&media_id).cloned();
                let Some(mut m) = found else { return json!({ "success": false, "error": "only media uploaded to this connector is sent" }) };
                if let Some(name) = filename {
                    m.file.name = name;
                }
                let id = self.message_id();
                let f = self.inflight.get_mut(&turn).expect("held");
                let part = f.next_part;
                f.next_part += 1;
                // a part of its own: the engine posts the one open before it
                (f.open_reply, f.narration) = (None, None);
                if !caption.trim().is_empty() {
                    self.emit(Event::Reply { turn: turn.clone(), part, text: caption }).await;
                }
                self.emit(Event::Attachment { turn, part, file: m.file }).await;
                json!({ "success": true, "message_id": id })
            }
            Action::GetChatInfo { chat } => {
                let name = self.by_chat.get(&chat).and_then(|t| self.inflight.get(t)).map(|f| f.start.chat_name.clone()).or_else(|| wire::split_chat_id(&chat).map(|(f, _)| f.split('.').next().unwrap_or(f).to_string())).unwrap_or(chat);
                json!({ "success": true, "chat_info": { "name": name, "type": "group" } })
            }
            Action::Unsupported { op } => json!({ "success": false, "error": format!("unsupported op {op}") }),
        }
    }

    /// New lines of a turn's tool progress, each a step. The first takes
    /// the reply part still open before it as its words (Hermes' text
    /// before the call: a reply taken back, never posted); a step while
    /// drafts stream makes the `send` ending them its words too.
    async fn steps(&mut self, turn: &str, steps: Vec<(String, String)>) {
        if steps.is_empty() {
            return;
        }
        let Some(f) = self.inflight.get_mut(turn) else { return };
        if let Some(stepped) = f.drafting.as_mut() {
            *stepped = true;
        }
        let mut words = String::new();
        let mut retract = None;
        if let Some((id, part, text)) = f.open_reply.take() {
            f.parts.remove(&id);
            f.take_message(id);
            f.narration = Some(text.clone());
            (words, retract) = (text, Some(part));
        }
        if let Some(part) = retract {
            crate::ev!("relay.narration", { "turn": turn, "after_step": false });
            self.emit(Event::Retract { turn: turn.to_string(), part }).await;
        }
        for (tool, args) in steps {
            let text = std::mem::take(&mut words);
            self.emit(Event::Step { turn: turn.to_string(), step: Step { tool, args, ok: true, excerpt: String::new(), text } }).await;
        }
    }

    /// A turn that ends idle having said nothing since a step took its
    /// words says them as its reply: Hermes, its answer written beside a
    /// housekeeping call (`memory`), takes it as said and sends it once.
    async fn end(&mut self, turn: &str, outcome: Outcome) {
        let last = self.inflight.get_mut(turn).and_then(|f| {
            let words = f.narration.take().filter(|_| outcome == Outcome::Idle)?;
            let part = f.next_part;
            f.next_part += 1;
            Some((part, words))
        });
        self.forget(turn);
        if let Some((part, text)) = last {
            crate::ev!("relay.narration_said", { "turn": turn });
            self.emit(Event::Reply { turn: turn.to_string(), part, text }).await;
        }
        self.emit(Event::End { turn: turn.to_string(), outcome }).await;
    }

    /// The chat's turn said something (so its next `✅` is its end).
    fn spoke(&mut self, chat: &str) {
        if let Some(f) = self.by_chat.get(chat).and_then(|t| self.inflight.get_mut(t)) {
            f.said = true;
        }
    }
}
