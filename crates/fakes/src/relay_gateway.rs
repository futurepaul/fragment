//! A Hermes gateway's Relay, as the sandcastle fake's computers run it
//! (docs/one-home.md, phase 2; the wire: docs/hermes-relay.md): one per
//! computer whose spec names `GATEWAY_RELAY_URL`, dialing `<url>/relay` with
//! its token as Hermes v0.21.5 mints it, saying `hello`, and then, for each
//! message it is handed, a turn as Hermes takes one: the delivery acked,
//! `👀`, its tool progress (when the message asks for a tool), its reply
//! streamed (a first send with the cursor, the whole text as an edit),
//! `👀` off, `✅`. Its reply echoes what it was told as Hermes reads a
//! shared group message: `echo: [name] text`. A Stop mid-turn
//! (`interrupt_inbound`) cancels it: no reply, only `👀` off.
//!
//! Levers (the fake's): `away` closes every gateway's socket and keeps it
//! away until its computer is woken (its owner's wake); `turn_ms` slows a
//! turn between its reply's two parts.

use std::collections::VecDeque;
use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// What every gateway shares with the fake.
#[derive(Default)]
pub struct Shared {
    /// Each message a gateway was handed, as its event (in order).
    pub heard: Vec<Value>,
    /// Closes a gateway's socket got (their codes).
    pub closes: Vec<u16>,
    /// Successful dials.
    pub dials: u64,
    /// Every gateway is away until its computer is woken.
    pub away: bool,
    /// Its computers' wakes.
    pub wakes: u64,
    /// A turn's wait between its reply's two parts.
    pub turn_ms: u64,
    /// Turns a Stop cut short.
    pub interrupted: u64,
}

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;
/// How long a gateway waits for an action's answer.
const ANSWER_WAIT: Duration = Duration::from_secs(10);
/// How often its reader looks up from the socket (to go away when told).
const POLL: Duration = Duration::from_millis(50);

/// Runs a computer's gateway until the computer is gone (`env` answers
/// `None`): dials while it is not away, and again after each close.
pub fn run(shared: Arc<Mutex<Shared>>, env: impl Fn() -> Option<(String, String, String)> + Send + 'static) {
    std::thread::spawn(move || {
        let mut backoff = Duration::from_millis(100);
        // bounded by the computer's life: `env` is None once it is removed
        while let Some((url, id, secret)) = env() {
            if shared.lock().unwrap().away {
                std::thread::sleep(POLL);
                continue;
            }
            match dial(&url, &id, &secret) {
                Ok(ws) => {
                    backoff = Duration::from_millis(100);
                    shared.lock().unwrap().dials += 1;
                    serve(&shared, ws);
                }
                Err(_) => {
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(2));
                }
            }
        }
    });
}

fn dial(url: &str, id: &str, secret: &str) -> Result<Ws, String> {
    let at = url.trim_end_matches('/').replacen("http://", "ws://", 1).replacen("https://", "wss://", 1);
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after 1970").as_secs() as i64 + 300;
    let mut req = tungstenite::client::IntoClientRequest::into_client_request(format!("{at}/relay")).map_err(|e| e.to_string())?;
    let bearer = format!("Bearer {}", fragment_core::relay::token(id, secret, exp));
    req.headers_mut().insert("authorization", bearer.parse().map_err(|_| "a header")?);
    let (ws, _) = tungstenite::connect(req).map_err(|e| e.to_string())?;
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(POLL)).map_err(|e| e.to_string())?;
    }
    Ok(ws)
}

/// One frame from the connector, or none yet (`Ok(None)`); `Err` once the
/// socket ended.
fn read(shared: &Arc<Mutex<Shared>>, ws: &mut Ws, lines: &mut VecDeque<Value>) -> Result<(), ()> {
    match ws.read() {
        Ok(Message::Text(t)) => {
            lines.extend(t.split('\n').filter(|l| !l.trim().is_empty()).filter_map(|l| serde_json::from_str::<Value>(l).ok()));
            Ok(())
        }
        Ok(Message::Close(frame)) => {
            shared.lock().unwrap().closes.push(frame.map_or(1005, |f| u16::from(f.code)));
            Err(())
        }
        Ok(_) => Ok(()),
        Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Ok(()),
        Err(_) => Err(()),
    }
}

fn send(ws: &mut Ws, v: Value) -> Result<(), ()> {
    ws.send(Message::text(format!("{v}\n"))).map_err(|_| ())
}

/// A dial: `hello`, the descriptor, then turns until the socket ends or the
/// fake sends it away.
fn serve(shared: &Arc<Mutex<Shared>>, mut ws: Ws) {
    let mut lines: VecDeque<Value> = VecDeque::new();
    if send(&mut ws, json!({ "type": "hello", "platform": "relay", "botId": "" })).is_err() {
        return;
    }
    let mut next_id = 0u64;
    // bounded by the socket's life
    loop {
        if shared.lock().unwrap().away {
            let _ = ws.close(None);
            let _ = ws.flush();
            return;
        }
        while let Some(frame) = lines.pop_front() {
            if frame["type"] == "inbound" && turn(shared, &mut ws, &mut lines, &mut next_id, &frame).is_err() {
                return;
            }
        }
        if read(shared, &mut ws, &mut lines).is_err() {
            return;
        }
    }
}

/// An action, and its answer (the frames that come meanwhile are kept).
fn act(shared: &Arc<Mutex<Shared>>, ws: &mut Ws, lines: &mut VecDeque<Value>, next_id: &mut u64, action: Value) -> Result<Value, ()> {
    *next_id += 1;
    let request = format!("{:032x}", *next_id);
    send(ws, json!({ "type": "outbound", "requestId": request, "action": action }))?;
    let deadline = Instant::now() + ANSWER_WAIT;
    // bounded by ANSWER_WAIT
    while Instant::now() < deadline {
        if let Some(i) = lines.iter().position(|f| f["type"] == "outbound_result" && f["requestId"] == request.as_str()) {
            return Ok(lines.remove(i).expect("found")["result"].clone());
        }
        read(shared, ws, lines)?;
    }
    Err(())
}

/// Waits `ms`, hearing frames; true when a Stop came for `chat` meanwhile.
fn wait(shared: &Arc<Mutex<Shared>>, ws: &mut Ws, lines: &mut VecDeque<Value>, ms: u64, chat: &str) -> Result<bool, ()> {
    let until = Instant::now() + Duration::from_millis(ms);
    // bounded by `ms`
    while Instant::now() < until {
        if let Some(i) = lines.iter().position(|f| f["type"] == "interrupt_inbound" && f["chat_id"] == chat) {
            lines.remove(i);
            return Ok(true);
        }
        read(shared, ws, lines)?;
    }
    Ok(false)
}

fn turn(shared: &Arc<Mutex<Shared>>, ws: &mut Ws, lines: &mut VecDeque<Value>, next_id: &mut u64, frame: &Value) -> Result<(), ()> {
    let event = &frame["event"];
    let (chat, mid) = (event["source"]["chat_id"].as_str().unwrap_or("").to_string(), event["message_id"].as_str().unwrap_or("").to_string());
    shared.lock().unwrap().heard.push(event.clone());
    if let Some(b) = frame["bufferId"].as_str() {
        send(ws, json!({ "type": "inbound_ack", "bufferId": b }))?;
    }
    let react = |emoji: &str, remove: bool| json!({ "op": "react", "chat_id": chat, "message_id": mid, "emoji": emoji, "remove": remove });
    act(shared, ws, lines, next_id, react("👀", false))?;
    let text = event["text"].as_str().unwrap_or("").trim_start_matches('\u{200b}').to_string();
    if text.contains("tool") {
        let sent = act(shared, ws, lines, next_id, json!({ "op": "send", "chat_id": chat, "content": "💻 terminal ▉", "reply_to": null, "metadata": {} }))?;
        let id = sent["message_id"].as_str().unwrap_or("").to_string();
        act(shared, ws, lines, next_id, json!({ "op": "edit", "chat_id": chat, "message_id": id, "content": "💻 terminal\n🔍 Searching the web", "metadata": {} }))?;
    }
    let reply = format!("echo: [{}] {text}", event["source"]["user_name"].as_str().unwrap_or("?"));
    let half: String = reply.chars().take(reply.chars().count() / 2).collect();
    let first = json!({ "op": "send", "chat_id": chat, "content": format!("{half} ▉"), "reply_to": mid,
        "metadata": { "reply_to_message_id": mid, "expect_edits": true } });
    let sent = act(shared, ws, lines, next_id, first)?;
    let id = sent["message_id"].as_str().unwrap_or("").to_string();
    let ms = shared.lock().unwrap().turn_ms;
    if wait(shared, ws, lines, ms, &chat)? {
        // stopped: what was written stays a draft's; the turn ends with no answer
        shared.lock().unwrap().interrupted += 1;
        act(shared, ws, lines, next_id, json!({ "op": "delete", "chat_id": chat, "message_id": id }))?;
        act(shared, ws, lines, next_id, react("👀", true))?;
        return Ok(());
    }
    act(shared, ws, lines, next_id, json!({ "op": "edit", "chat_id": chat, "message_id": id, "content": reply, "metadata": {} }))?;
    act(shared, ws, lines, next_id, react("👀", true))?;
    act(shared, ws, lines, next_id, react("✅", false))?;
    Ok(())
}
