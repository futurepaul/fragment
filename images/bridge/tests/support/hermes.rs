//! A scripted Hermes gateway's Relay, for the relay runtime's tests: it
//! dials and turns as Hermes v0.21.5's gateway does (ported from the deleted
//! `crates/fakes/src/relay_gateway.rs`, tag `celld-final`, and brought to
//! the ops the release actually sends: `draft`, `prompt`, `send_media`):
//!
//! - dials `<url>/relay` with its token, says `hello`, reads the descriptor,
//!   and dials again (with backoff) whenever the socket ends, unless away;
//! - each inbound is acked, then its turn runs concurrently with other
//!   chats': first an empty bracket (`👀`, off, `✅`: the real one's for a
//!   message it took while its gateway was starting, sent here before
//!   every turn), then `👀`; its tool progress (`tool`) as a send answering nothing
//!   and an edit adding a line; an approval (`risky`) as a `prompt` op whose
//!   `prompt_response` answer resolves it mid-turn, confirmed by an interim
//!   send, or that times out after `approval_ms` (its `approvals.timeout`)
//!   as Hermes' does: the card edited to say so, or the notice sent when the
//!   edit fails, and the turn goes on without the command; its reply as
//!   `draft` frames, then one `send` answering the
//!   message; `👀` off; `✅`. An `interrupt_inbound` mid-turn stops it: `👀`
//!   off only;
//! - `clarify` asks as Hermes' clarify with choices does: a `prompt` of
//!   `prompt_kind: "clarify"` (`❓ Which color?`: `c0` red, `c1` blue,
//!   `other`); a choice is acked `✅ <choice>`, `other` asks `✏️ Type your
//!   answer:`, and a message in the chat while it waits is its answer (its
//!   clarify intercept); the reply ends `(chose: <it>)`;
//! - `fail` fails as Hermes' handler does when it raises: `👀` off, `❌`
//!   with nothing said, then its notice as a send answering nothing;
//! - `narrate` says text beside a tool call as Hermes' stream consumer
//!   does: drafts, then a send answering the message at the tool boundary,
//!   and the tool's progress after it (`late`: before it); `only`, the
//!   answer beside a housekeeping call, and nothing after;
//! - `media` uploads a file to `/relay/media` and sends it; inbound media
//!   is downloaded with the token and its bytes counted in the reply;
//! - the reply echoes what it heard: `echo: [<user_name>] <text>`;
//! - `/steer <words>` and `/btw <question>` are commands taken beside a
//!   turn, as v0.21.6 takes them mid-run: answered at once with a send
//!   answering the command (`⏩ Steer queued …`, `💬 Side question: …`),
//!   and a side question's answer after, answering nothing (`💬 /btw:
//!   "<question>"\n\nit was main.rs`); no turn of their own;
//! - what its model is handed for a turn (`Seen::handed`) is the message as
//!   v0.21.5 renders it in a shared chat: `[<user_name>] <text>`, after an
//!   inbound's read-only `context` when it has one (`[Recent channel
//!   messages]\n<context>\n\n[New message]\n…`); the context is reference,
//!   never what it acts on;
//! - its session keeps each chat's message until its turn's reply (Hermes
//!   persists the message as its turn starts, the rest as it ends): a turn
//!   cut before its reply (its container gone, `dead`) leaves its message
//!   the session's last, which a gateway started on the same `/data`
//!   (`Hermes::after`) folds the chat's next message into, as v0.21.5 does
//!   with two user messages in a row (seen in the real image: the model was
//!   given `[paul] do the risky thing at bedtime\n\n[paul] good morning`),
//!   and answers both: a cut `risky` asks its approval again. Our image's
//!   boot closes such a turn before its gateway starts
//!   (`Hermes::after_boot`: hermes-boot's `close_cut_turns` writes Hermes'
//!   own failed-turn boundary after the cut message, proven against the
//!   real Hermes in tests/docker.rs), so nothing is folded.

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use fragment_bridge::net::{self, Base};
use fragment_bridge::runtime::relay::wire;

#[derive(Default)]
pub struct Seen {
    /// Each inbound event, in order (prompt answers too).
    pub heard: Vec<Value>,
    /// Successful dials, and close codes heard.
    pub dials: u64,
    pub closes: Vec<u16>,
    pub interrupted: u64,
    /// Ops it sent, in order.
    pub ops: Vec<String>,
    /// While away it does not dial.
    pub away: bool,
    /// While deaf it stays on its socket and drops every inbound unheard
    /// and unacked (a gateway that dies before it takes a turn).
    pub deaf: bool,
    /// Its turns' wait between drafts (ms).
    pub turn_ms: u64,
    /// How long an approval waits for its answer (Hermes'
    /// `approvals.timeout`), and the approvals that timed out.
    pub approval_ms: u64,
    pub timed_out: u64,
    /// Gone with its container: it dials no more, and its turns stop where
    /// they are, finishing nothing.
    pub dead: bool,
    /// Its session, as its `/data` keeps it: by chat, the message whose
    /// turn has not replied yet (one cut short stays), as its model was
    /// handed it.
    pub session: HashMap<String, (String, String)>,
    /// What its model was handed for each turn, in order: `(message id,
    /// the turn's user message)`, a cut one's folded in when the session
    /// held it.
    pub handed: Vec<(String, String)>,
    /// Cut turns the boot closed before this gateway started.
    pub closed: usize,
    /// The words of each `/steer` it took into a running turn.
    pub steered: Vec<String>,
}

pub struct Hermes {
    pub seen: Arc<Mutex<Seen>>,
}

impl Hermes {
    pub fn spawn(addr: std::net::SocketAddr, id: &str, secret: &str) -> Hermes {
        Hermes::spawn_with(addr, id, secret, Seen { turn_ms: 30, approval_ms: 10_000, ..Seen::default() })
    }

    /// The gateway of the next life of its computer, on the same `/data`:
    /// this one is gone with its container (`dead`), and the new one starts
    /// with its sessions and its settings.
    pub fn after(&self, addr: std::net::SocketAddr, id: &str, secret: &str) -> Hermes {
        let seen = self.with(|s| {
            s.dead = true;
            Seen { turn_ms: s.turn_ms, approval_ms: s.approval_ms, session: s.session.clone(), ..Seen::default() }
        });
        Hermes::spawn_with(addr, id, secret, seen)
    }

    /// The next life as our image starts it: on the same `/data`, its boot
    /// first closes each turn a restart cut (a session whose last message
    /// has no answer gets Hermes' failed-turn boundary, so it is no longer
    /// the session's open tail), then its gateway starts.
    pub fn after_boot(&self, addr: std::net::SocketAddr, id: &str, secret: &str) -> Hermes {
        let seen = self.with(|s| {
            s.dead = true;
            Seen { turn_ms: s.turn_ms, approval_ms: s.approval_ms, closed: s.session.len(), ..Seen::default() }
        });
        Hermes::spawn_with(addr, id, secret, seen)
    }

    fn spawn_with(addr: std::net::SocketAddr, id: &str, secret: &str, seen: Seen) -> Hermes {
        let seen = Arc::new(Mutex::new(seen));
        let (s, id, secret) = (seen.clone(), id.to_string(), secret.to_string());
        tokio::spawn(async move {
            let mut backoff = 50u64;
            // bounded by the test's runtime
            loop {
                if s.lock().unwrap().dead {
                    return;
                }
                if s.lock().unwrap().away {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    continue;
                }
                match dial(addr, &id, &secret).await {
                    Ok(ws) => {
                        backoff = 50;
                        s.lock().unwrap().dials += 1;
                        serve(s.clone(), ws, addr, id.clone(), secret.clone()).await;
                    }
                    Err(_) => {
                        tokio::time::sleep(Duration::from_millis(backoff)).await;
                        backoff = (backoff * 2).min(500);
                    }
                }
            }
        });
        Hermes { seen }
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut Seen) -> T) -> T {
        f(&mut self.seen.lock().unwrap())
    }
}

fn bearer(id: &str, secret: &str) -> String {
    let exp = i64::try_from(fragment_bridge::log::now_ms() / 1000).unwrap() + 300;
    format!("Bearer {}", wire::token(id, secret, exp))
}

async fn dial(addr: std::net::SocketAddr, id: &str, secret: &str) -> Result<net::ClientWs, String> {
    let base = Base { host: addr.ip().to_string(), port: addr.port(), prefix: String::new() };
    net::connect_ws(&base, "/relay", &[("authorization", bearer(id, secret))]).await
}

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>;

/// What a `narrate` turn says beside its tool call.
pub const NARRATION: &str = "Let me check that.";
/// What a `fail` turn says after its `❌`.
pub const FAILURE: &str = "Sorry, I encountered an error (RuntimeError).\nno details available\nTry again or use /reset to start a fresh session.";

/// What a turn hears while it runs.
enum Heard {
    Interrupt,
    Answer(String),
    /// A message in its chat while its clarify waits: the clarify's answer
    /// (Hermes' clarify intercept), never a turn of its own.
    Words(String),
}

struct Gateway {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    seen: Arc<Mutex<Seen>>,
    next: AtomicU64,
    addr: std::net::SocketAddr,
    id: String,
    secret: String,
    /// Running turns by chat, and prompts by id: who to tell.
    turns: Mutex<HashMap<String, mpsc::UnboundedSender<Heard>>>,
    /// Chats whose turn's clarify waits for an answer.
    clarifying: Mutex<std::collections::HashSet<String>>,
}

impl Gateway {
    fn send(&self, v: Value) {
        let _ = self.out.send(Message::text(format!("{v}\n")));
    }

    async fn act(&self, action: Value) -> Value {
        if self.seen.lock().unwrap().dead {
            return json!({ "success": false, "error": "gone with its container" });
        }
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        let request = format!("{n:032x}");
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(request.clone(), tx);
        self.seen.lock().unwrap().ops.push(action["op"].as_str().unwrap_or("").to_string());
        self.send(json!({ "type": "outbound", "requestId": request, "action": action }));
        tokio::time::timeout(Duration::from_secs(10), rx).await.ok().and_then(Result::ok).unwrap_or(json!({ "success": false, "error": "no answer" }))
    }
}

async fn serve(seen: Arc<Mutex<Seen>>, ws: net::ClientWs, addr: std::net::SocketAddr, id: String, secret: String) {
    let (mut sink, mut stream) = ws.split();
    let (out, mut out_rx) = mpsc::unbounded_channel::<Message>();
    let gw = Arc::new(Gateway { out, pending: Arc::new(Mutex::new(HashMap::new())), seen: seen.clone(), next: AtomicU64::new(1), addr, id, secret, turns: Mutex::new(HashMap::new()), clarifying: Mutex::new(Default::default()) });
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if sink.send(m).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    gw.send(json!({ "type": "hello", "platform": "relay", "botId": "" }));
    // bounded by the socket
    loop {
        let m = tokio::select! {
            m = stream.next() => m,
            _ = tokio::time::sleep(Duration::from_millis(20)) => {
                let gone = seen.lock().map(|s| s.away || s.dead).unwrap();
                if gone {
                    break;
                }
                continue;
            }
        };
        let text = match m {
            Some(Ok(Message::Text(t))) => t.to_string(),
            Some(Ok(Message::Close(f))) => {
                seen.lock().unwrap().closes.push(f.map_or(1005, |f| u16::from(f.code)));
                break;
            }
            Some(Ok(_)) => continue,
            _ => break,
        };
        for line in text.split('\n').filter(|l| !l.trim().is_empty()) {
            let Ok(f) = serde_json::from_str::<Value>(line) else { continue };
            match f["type"].as_str() {
                Some("outbound_result") => {
                    if let Some(tx) = gw.pending.lock().unwrap().remove(f["requestId"].as_str().unwrap_or("")) {
                        let _ = tx.send(f["result"].clone());
                    }
                }
                Some("interrupt_inbound") => {
                    let chat = f["chat_id"].as_str().unwrap_or("").to_string();
                    if let Some(tx) = gw.turns.lock().unwrap().get(&chat) {
                        let _ = tx.send(Heard::Interrupt);
                    }
                }
                Some("inbound") => {
                    if seen.lock().unwrap().deaf {
                        continue;
                    }
                    let event = f["event"].clone();
                    seen.lock().unwrap().heard.push(event.clone());
                    if let Some(b) = f["bufferId"].as_str() {
                        gw.send(json!({ "type": "inbound_ack", "bufferId": b }));
                    }
                    let chat = event["source"]["chat_id"].as_str().unwrap_or("").to_string();
                    if let Some(pr) = event.get("prompt_response").filter(|p| p.is_object()) {
                        // a structured answer: resolved mid-turn, never chat
                        if let Some(tx) = gw.turns.lock().unwrap().get(&chat) {
                            let _ = tx.send(Heard::Answer(pr["option_id"].as_str().unwrap_or("").to_string()));
                        }
                        continue;
                    }
                    let said = event["text"].as_str().unwrap_or("").to_string();
                    let mid = event["message_id"].as_str().unwrap_or("").to_string();
                    if let Some(words) = said.strip_prefix("/steer ").map(str::to_string) {
                        // v0.21.6's `_busy_steer_command`: into the running
                        // turn, answered at once, answering the command
                        gw.seen.lock().unwrap().steered.push(words.clone());
                        let gw = gw.clone();
                        tokio::spawn(async move {
                            let word = format!("⏩ Steer queued into current run — arrives after the next tool call: '{words}'");
                            gw.act(json!({ "op": "send", "chat_id": chat, "content": word, "reply_to": mid, "metadata": {} })).await;
                        });
                        continue;
                    }
                    if let Some(question) = said.strip_prefix("/btw ").map(str::to_string) {
                        // v0.21.6's `_handle_btw_command`: its start answering
                        // the command, then the answer answering nothing
                        let gw = gw.clone();
                        tokio::spawn(async move {
                            let started = format!("💬 Side question: \"{question}\"\nAnswering from a snapshot of this conversation — the current work continues.");
                            gw.act(json!({ "op": "send", "chat_id": chat, "content": started, "reply_to": mid, "metadata": {} })).await;
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            gw.act(json!({ "op": "send", "chat_id": chat, "content": format!("💬 /btw: \"{question}\"\n\nit was main.rs"), "reply_to": null, "metadata": {} })).await;
                        });
                        continue;
                    }
                    if gw.clarifying.lock().unwrap().contains(&chat) {
                        // its clarify intercept: the words are the clarify's answer
                        if let Some(tx) = gw.turns.lock().unwrap().get(&chat) {
                            let _ = tx.send(Heard::Words(event["text"].as_str().unwrap_or("").to_string()));
                        }
                        continue;
                    }
                    let (tx, rx) = mpsc::unbounded_channel();
                    gw.turns.lock().unwrap().insert(chat.clone(), tx);
                    let gw = gw.clone();
                    tokio::spawn(async move {
                        turn(&gw, event, rx).await;
                        gw.turns.lock().unwrap().remove(&chat);
                    });
                }
                _ => {}
            }
        }
    }
    writer.abort();
}

/// Waits `ms` for an interrupt (true when one came).
async fn wait(ms: u64, rx: &mut mpsc::UnboundedReceiver<Heard>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(ms)) => false,
        h = rx.recv() => matches!(h, Some(Heard::Interrupt) | None),
    }
}

async fn turn(gw: &Gateway, event: Value, mut rx: mpsc::UnboundedReceiver<Heard>) {
    let chat = event["source"]["chat_id"].as_str().unwrap_or("").to_string();
    let mid = event["message_id"].as_str().unwrap_or("").to_string();
    let react = |emoji: &str, remove: bool| json!({ "op": "react", "chat_id": chat, "message_id": mid, "emoji": emoji, "remove": remove });
    // A message Hermes takes while its gateway starts is bracketed once,
    // with nothing inside, as its startup restore queues it, before the
    // turn's own bracket when it runs (v0.21.5's `_handle_message` and
    // `_drain_startup_restore_queue`; seen in the real image for the first
    // turn of a boot only: 👀, 👀 off, ✅, then the turn 1.3 s later). Here
    // every turn has it, so every test passes the rule that ends a turn.
    gw.act(react("👀", false)).await;
    gw.act(react("👀", true)).await;
    gw.act(react("✅", false)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    gw.act(react("👀", false)).await;
    let message = event["text"].as_str().unwrap_or("").trim_start_matches('\u{200b}').to_string();
    if message.contains("fail") {
        // v0.21.5 when its handler raises (`_process_message_background`):
        // `❌` with nothing said, then its notice as a send answering nothing
        // (`_notify_turn_error`)
        gw.act(react("👀", true)).await;
        gw.act(react("❌", false)).await;
        gw.act(json!({ "op": "send", "chat_id": chat, "content": FAILURE, "reply_to": null, "metadata": {} })).await;
        return;
    }
    let user = event["source"]["user_name"].as_str().unwrap_or("?").to_string();
    // as v0.21.5 renders a shared chat's message: the sender, after the
    // inbound's read-only context when it has one
    let context: Vec<&str> = event["context"].as_array().map(|c| c.iter().filter_map(|i| i["text"].as_str()).collect()).unwrap_or_default();
    let rendered = match context.is_empty() {
        true => format!("[{user}] {message}"),
        false => format!("[Recent channel messages]\n{}\n\n[New message]\n[{user}] {message}", context.join("\n")),
    };
    // its session: a message whose turn was cut before its reply is still
    // the last one, and this one is folded into it; this one stays there
    // until its own reply. `text` is what its model acts on (the context
    // is reference only), `rendered` what it is handed.
    let text = {
        let mut s = gw.seen.lock().unwrap();
        let (text, rendered) = match s.session.get(&chat) {
            Some((cut, cut_rendered)) => (format!("{cut}\n\n{message}"), format!("{cut_rendered}\n\n{rendered}")),
            None => (message, rendered),
        };
        s.session.insert(chat.clone(), (text.clone(), rendered.clone()));
        s.handed.push((mid.clone(), rendered));
        text
    };
    let done = || {
        gw.seen.lock().unwrap().session.remove(&chat);
    };
    let mut reply = format!("echo: [{user}] {text}");
    if text.contains("narrate") {
        // The model's text beside a tool call, as Hermes' stream consumer
        // delivers it: drafts, then, at the tool boundary, a send answering
        // the turn that ends the segment; its tool progress reaches the
        // connector after that send, or (`late`) before it. `only`: the
        // answer said beside a housekeeping call, which Hermes sends once.
        let words = if text.contains("only") { reply.clone() } else { NARRATION.to_string() };
        gw.act(json!({ "op": "draft", "chat_id": chat, "draft_id": 7, "content": words, "final": false, "metadata": { "reply_to_message_id": mid } })).await;
        let said = json!({ "op": "send", "chat_id": chat, "content": words, "reply_to": mid, "metadata": { "reply_to_message_id": mid, "notify": true } });
        let tool = if text.contains("only") { "🧠 memory: \"saved\"" } else { "💻 terminal: `echo hi`" };
        let progress = json!({ "op": "send", "chat_id": chat, "content": tool, "reply_to": null, "metadata": {} });
        if text.contains("late") {
            gw.act(progress).await;
            gw.act(said).await;
        } else {
            gw.act(said).await;
            gw.act(progress).await;
        }
        if text.contains("only") {
            gw.act(react("👀", true)).await;
            gw.act(react("✅", false)).await;
            return;
        }
    }
    if text.contains("tool") {
        let sent = gw.act(json!({ "op": "send", "chat_id": chat, "content": "💻 terminal: `ls`", "reply_to": null, "metadata": {} })).await;
        let id = sent["message_id"].as_str().unwrap_or("").to_string();
        gw.act(json!({ "op": "edit", "chat_id": chat, "message_id": id, "content": "💻 terminal: `ls`\n🔍 Searching the web for x", "metadata": {} })).await;
    }
    if let Some(urls) = event["media_urls"].as_array() {
        let mut bytes = 0usize;
        for u in urls.iter().filter_map(Value::as_str) {
            bytes += download(gw, u).await.map_or(0, |b| b.len());
        }
        reply = format!("{reply} [media: {} files, {bytes} bytes]", urls.len());
    }
    if text.contains("risky") {
        let options = json!([{ "id": "once", "label": "Allow Once", "style": "primary" }, { "id": "session", "label": "Allow Session" }, { "id": "deny", "label": "Deny", "style": "danger" }]);
        let prompt = format!("f00d.{:08x}", gw.next.fetch_add(1, Ordering::Relaxed));
        let asked = gw.act(json!({ "op": "prompt", "chat_id": chat, "content": "⚠️ **Dangerous command** `rm -rf x`", "prompt_kind": "approval", "prompt_id": prompt, "options": options, "reply_to": null, "metadata": {} })).await;
        let approval_ms = gw.seen.lock().unwrap().approval_ms;
        let answered = tokio::select! {
            h = rx.recv() => h,
            _ = tokio::time::sleep(Duration::from_millis(approval_ms)) => None,
        };
        if answered.is_none() {
            // Hermes v0.21.5 on its approval's timeout (gateway/
            // run_turn_runner_approval_settle.py): the card edited to say so,
            // or the notice sent as a message of its own when the edit fails;
            // the command is BLOCKED, and the turn goes on without it
            gw.seen.lock().unwrap().timed_out += 1;
            let notice = "⌛ Approval timed out after 1 hour — the command was NOT run.";
            let card = asked["message_id"].as_str().unwrap_or("").to_string();
            let edited = gw.act(json!({ "op": "edit", "chat_id": chat, "message_id": card, "content": notice, "metadata": {} })).await;
            if edited["success"] != json!(true) {
                gw.act(json!({ "op": "send", "chat_id": chat, "content": notice, "reply_to": null, "metadata": {} })).await;
            }
        }
        let said = match answered {
            Some(Heard::Answer(o)) => {
                // Hermes confirms in the chat, as an interim send.
                gw.act(json!({ "op": "send", "chat_id": chat, "content": format!("✅ {o}"), "reply_to": null, "metadata": {} })).await;
                if o == "deny" { "denied" } else { "approved" }
            }
            Some(Heard::Interrupt) => {
                gw.seen.lock().unwrap().interrupted += 1;
                done();
                gw.act(react("👀", true)).await;
                return;
            }
            Some(Heard::Words(_)) | None => "not approved",
        };
        reply = format!("{reply} ({said})");
    }
    if text.contains("clarify") {
        // Hermes' clarify with choices over Relay (`send_clarify`): a prompt
        // of `prompt_kind: "clarify"`, its options positional and "Other";
        // a choice is acked `✅ <choice>`, "Other" asks `✏️ Type your
        // answer:` and takes the chat's next message as the answer
        let options = json!([{ "id": "c0", "label": "red" }, { "id": "c1", "label": "blue" }, { "id": "other", "label": "✏️ Other (type answer)" }]);
        let prompt = format!("c1a.{:08x}", gw.next.fetch_add(1, Ordering::Relaxed));
        gw.clarifying.lock().unwrap().insert(chat.clone());
        gw.act(json!({ "op": "prompt", "chat_id": chat, "content": "❓ Which color?", "prompt_kind": "clarify", "prompt_id": prompt, "options": options, "reply_to": null, "metadata": {} })).await;
        let mut chose = None;
        // bounded: an answer, then at most its words
        while chose.is_none() {
            match rx.recv().await {
                Some(Heard::Answer(o)) if o == "other" => {
                    gw.act(json!({ "op": "send", "chat_id": chat, "content": "✏️ Type your answer:", "reply_to": null, "metadata": {} })).await;
                }
                Some(Heard::Answer(o)) => {
                    let label = if o == "c0" { "red" } else { "blue" };
                    gw.act(json!({ "op": "send", "chat_id": chat, "content": format!("✅ {label}"), "reply_to": null, "metadata": {} })).await;
                    chose = Some(label.to_string());
                }
                Some(Heard::Words(w)) => chose = Some(w),
                Some(Heard::Interrupt) | None => {
                    gw.clarifying.lock().unwrap().remove(&chat);
                    done();
                    gw.act(react("👀", true)).await;
                    return;
                }
            }
        }
        gw.clarifying.lock().unwrap().remove(&chat);
        reply = format!("{reply} (chose: {})", chose.unwrap_or_default());
    }
    if gw.seen.lock().unwrap().dead {
        // cut with its container: its message stays its session's last
        return;
    }
    let ms = gw.seen.lock().unwrap().turn_ms;
    let drafts = if text.contains("slow") { 20 } else { 2 };
    for i in 1..=drafts {
        let n = reply.chars().count() * i / (drafts + 1);
        gw.act(json!({ "op": "draft", "chat_id": chat, "draft_id": 1, "content": reply.chars().take(n.max(1)).collect::<String>(), "final": false, "metadata": { "reply_to_message_id": mid } })).await;
        if wait(ms, &mut rx).await {
            gw.seen.lock().unwrap().interrupted += 1;
            done();
            gw.act(react("👀", true)).await;
            return;
        }
    }
    if gw.seen.lock().unwrap().dead {
        return;
    }
    gw.act(json!({ "op": "send", "chat_id": chat, "content": reply, "reply_to": mid, "metadata": { "reply_to_message_id": mid, "notify": true } })).await;
    done();
    if text.contains("media") {
        if let Some(url) = upload(gw, b"a picture of a cat").await {
            gw.act(json!({ "op": "send_media", "chat_id": chat, "media_kind": "image", "source_url": url, "content": "a cat", "reply_to": mid, "filename": "cat.png", "metadata": {} })).await;
        }
    }
    gw.act(react("👀", true)).await;
    gw.act(react("✅", false)).await;
}

async fn http(gw: &Gateway, method: &str, path: &str, content_type: Option<&str>, body: &[u8]) -> Option<Vec<u8>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(gw.addr).await.ok()?;
    let mut head = format!("{method} {path} HTTP/1.1\r\nhost: {}\r\nauthorization: {}\r\ncontent-length: {}\r\nconnection: close\r\n", gw.addr, bearer(&gw.id, &gw.secret), body.len());
    if let Some(t) = content_type {
        head.push_str(&format!("content-type: {t}\r\nx-media-filename: cat.png\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes()).await.ok()?;
    s.write_all(body).await.ok()?;
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).await.ok()?;
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let status_ok = buf.starts_with(b"HTTP/1.1 200");
    status_ok.then(|| buf[split + 4..].to_vec())
}

async fn upload(gw: &Gateway, bytes: &[u8]) -> Option<String> {
    let body = http(gw, "POST", "/relay/media", Some("image/png"), bytes).await?;
    let v: Value = serde_json::from_slice(&body).ok()?;
    Some(format!("http://{}/relay/media/{}", gw.addr, v["id"].as_str()?))
}

async fn download(gw: &Gateway, url: &str) -> Option<Vec<u8>> {
    let path = url.split_once("/relay/media/").map(|(_, id)| format!("/relay/media/{id}"))?;
    http(gw, "GET", &path, None, &[]).await
}
