//! A scripted Hermes gateway's Relay, for the relay runtime's tests: it
//! dials and turns as Hermes v0.21.5's gateway does (ported from the deleted
//! `crates/fakes/src/relay_gateway.rs`, tag `celld-final`, and brought to
//! the ops the release actually sends: `draft`, `prompt`, `send_media`):
//!
//! - dials `<url>/relay` with its token, says `hello`, reads the descriptor,
//!   and dials again (with backoff) whenever the socket ends, unless away;
//! - each inbound is acked, then its turn runs concurrently with other
//!   chats': `👀`; its tool progress (`tool`) as a send answering nothing
//!   and an edit adding a line; an approval (`risky`) as a `prompt` op whose
//!   `prompt_response` answer resolves it mid-turn, confirmed by an interim
//!   send; its reply as `draft` frames, then one `send` answering the
//!   message; `👀` off; `✅`. An `interrupt_inbound` mid-turn stops it: `👀`
//!   off only;
//! - `media` uploads a file to `/relay/media` and sends it; inbound media
//!   is downloaded with the token and its bytes counted in the reply;
//! - the reply echoes what it heard: `echo: [<user_name>] <text>`.

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
    /// Its turns' wait between drafts (ms).
    pub turn_ms: u64,
}

pub struct Hermes {
    pub seen: Arc<Mutex<Seen>>,
}

impl Hermes {
    pub fn spawn(addr: std::net::SocketAddr, id: &str, secret: &str) -> Hermes {
        let seen = Arc::new(Mutex::new(Seen { turn_ms: 30, ..Seen::default() }));
        let (s, id, secret) = (seen.clone(), id.to_string(), secret.to_string());
        tokio::spawn(async move {
            let mut backoff = 50u64;
            // bounded by the test's runtime
            loop {
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

/// What a turn hears while it runs.
enum Heard {
    Interrupt,
    Answer(String),
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
}

impl Gateway {
    fn send(&self, v: Value) {
        let _ = self.out.send(Message::text(format!("{v}\n")));
    }

    async fn act(&self, action: Value) -> Value {
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
    let gw = Arc::new(Gateway { out, pending: Arc::new(Mutex::new(HashMap::new())), seen: seen.clone(), next: AtomicU64::new(1), addr, id, secret, turns: Mutex::new(HashMap::new()) });
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
                if seen.lock().unwrap().away {
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
    gw.act(react("👀", false)).await;
    let text = event["text"].as_str().unwrap_or("").trim_start_matches('\u{200b}').to_string();
    let mut reply = format!("echo: [{}] {text}", event["source"]["user_name"].as_str().unwrap_or("?"));
    if text.contains("tool") {
        let sent = gw.act(json!({ "op": "send", "chat_id": chat, "content": "💻 terminal: `ls`", "reply_to": null, "metadata": {} })).await;
        let id = sent["message_id"].as_str().unwrap_or("").to_string();
        gw.act(json!({ "op": "edit", "chat_id": chat, "message_id": id, "content": "💻 terminal: `ls`\n🔍 web_search: \"x\"", "metadata": {} })).await;
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
        gw.act(json!({ "op": "prompt", "chat_id": chat, "content": "⚠️ **Dangerous command** `rm -rf x`", "prompt_kind": "approval", "prompt_id": prompt, "options": options, "reply_to": null, "metadata": {} })).await;
        let answered = tokio::select! {
            h = rx.recv() => h,
            _ = tokio::time::sleep(Duration::from_secs(10)) => None,
        };
        let said = match answered {
            Some(Heard::Answer(o)) => {
                // Hermes confirms in the chat, as an interim send.
                gw.act(json!({ "op": "send", "chat_id": chat, "content": format!("✅ {o}"), "reply_to": null, "metadata": {} })).await;
                if o == "deny" { "denied" } else { "approved" }
            }
            Some(Heard::Interrupt) => {
                gw.seen.lock().unwrap().interrupted += 1;
                gw.act(react("👀", true)).await;
                return;
            }
            None => "not approved",
        };
        reply = format!("{reply} ({said})");
    }
    let ms = gw.seen.lock().unwrap().turn_ms;
    let drafts = if text.contains("slow") { 20 } else { 2 };
    for i in 1..=drafts {
        let n = reply.chars().count() * i / (drafts + 1);
        gw.act(json!({ "op": "draft", "chat_id": chat, "draft_id": 1, "content": reply.chars().take(n.max(1)).collect::<String>(), "final": false, "metadata": { "reply_to_message_id": mid } })).await;
        if wait(ms, &mut rx).await {
            gw.seen.lock().unwrap().interrupted += 1;
            gw.act(react("👀", true)).await;
            return;
        }
    }
    gw.act(json!({ "op": "send", "chat_id": chat, "content": reply, "reply_to": mid, "metadata": { "reply_to_message_id": mid, "notify": true } })).await;
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
