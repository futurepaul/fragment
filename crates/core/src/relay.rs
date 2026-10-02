//! Hermes' Relay, with the platform its connector (docs/one-home.md,
//! decision 2; the wire: docs/hermes-relay.md): what a Hermes cell decides
//! apart from the calls it makes. A Hermes' gateway dials its cell over a
//! WebSocket, authenticated by a per-gateway secret; the cell hands it its
//! chats' messages, each with who wrote it, and turns its sends, edits, and
//! reactions into the chat's records.
//!
//! Pinned to Hermes v0.21.5's Relay, contract version 1, read from its code
//! (`gateway/relay/`): frames are JSON objects, one per line, each ending
//! with a newline.

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

/// The contract this connector speaks.
pub const CONTRACT_VERSION: u64 = 1;
/// The platform name the gateway keys its sessions by.
pub const PLATFORM: &str = "relay";
/// A gateway's token is good this much past its `exp`: its clock is the
/// guest's.
pub const TOKEN_SKEW_S: i64 = 120;
/// The longest token an Authorization header may carry.
const TOKEN_MAX_BYTES: usize = 1024;
/// What the gateway appends to a reply while it streams.
pub const CURSOR: &str = " ▉";
/// The ops this connector answers; any other is refused (the gateway
/// degrades). `react` carries a turn's start and end (`👀`, then `✅`/`❌`);
/// `delete` takes back a reply that streamed (a stopped turn's, or a
/// silence marker's).
pub const SUPPORTED_OPS: [&str; 5] = ["send", "edit", "typing", "react", "delete"];
/// The longest reply the gateway sends in one message (it splits longer).
pub const MESSAGE_MAX_CHARS: u64 = 16_000;
/// A frame from the gateway is at most this long.
pub const FRAME_MAX_BYTES: usize = 1024 * 1024;
/// Hermes' reactions that bracket a turn.
pub const STARTED: &str = "👀";
pub const DONE: &str = "✅";
pub const FAILED: &str = "❌";

/// The Authorization a gateway dials with: `Bearer
/// base64url(id:exp:hex(HMAC-SHA256(secret, "id:exp")))`, unpadded (its
/// `gateway/relay/auth.py`). The fake's gateway mints it; the cell checks it.
pub fn token(gateway_id: &str, secret: &str, exp: i64) -> String {
    let signed = format!("{gateway_id}:{exp}");
    let sig = hex::encode(mac(secret, &signed));
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{signed}:{sig}"))
}

fn mac(secret: &str, msg: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    m.update(msg.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// Why a dial is refused: it closes 4401, `expired` for a token past its
/// time (the gateway retries), `unauthorized` otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    Expired,
    Unauthorized,
}

impl Refused {
    /// The close reason the gateway reads (`expired` is never a revocation).
    pub fn reason(self) -> &'static str {
        match self {
            Refused::Expired => "expired",
            Refused::Unauthorized => "unauthorized",
        }
    }
}

/// Checks a dial's `Authorization` header: a token for `gateway_id` under
/// `secret`, not past its `exp` by more than the skew.
pub fn verify(header: Option<&str>, gateway_id: &str, secret: &str, now_s: i64) -> Result<(), Refused> {
    let token = header.and_then(|h| h.strip_prefix("Bearer ")).filter(|t| t.len() <= TOKEN_MAX_BYTES).ok_or(Refused::Unauthorized)?;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token.trim_end_matches('=')).map_err(|_| Refused::Unauthorized)?;
    let raw = String::from_utf8(raw).map_err(|_| Refused::Unauthorized)?;
    let mut parts = raw.rsplitn(3, ':');
    let (sig, exp, payload) = match (parts.next(), parts.next(), parts.next()) {
        (Some(s), Some(e), Some(p)) => (s, e, p),
        _ => return Err(Refused::Unauthorized),
    };
    let exp: i64 = exp.parse().map_err(|_| Refused::Unauthorized)?;
    let sig = hex::decode(sig).map_err(|_| Refused::Unauthorized)?;
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    m.update(format!("{payload}:{exp}").as_bytes());
    if m.verify_slice(&sig).is_err() || payload != gateway_id {
        return Err(Refused::Unauthorized);
    }
    if exp != 0 && now_s > exp + TOKEN_SKEW_S {
        return Err(Refused::Expired);
    }
    Ok(())
}

/// A frame from the gateway.
#[derive(Debug, Clone, PartialEq)]
pub enum FromGateway {
    /// Each dial opens with one: answer it with the descriptor.
    Hello,
    /// An action, answered by `result` under the same request id.
    Outbound { request_id: String, action: Action },
    /// It took a delivered message (its turn is scheduled, not done).
    InboundAck { buffer_id: String },
    /// It is going away on purpose: answer with `going_idle_ack`.
    GoingIdle,
    /// A type this connector does not read (the contract adds them).
    Other(String),
}

/// An action the gateway asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A new message in a chat: a reply when it answers a message
    /// (`reply_to`), else Hermes' tool progress.
    Send { chat: String, content: String, reply: bool },
    /// The whole text of a message it sent, again.
    Edit { chat: String, message_id: String, content: String },
    Typing { chat: String },
    /// A message it sent, taken back.
    Delete { chat: String, message_id: String },
    /// A reaction on a message of the chat's: on the one it answers, its
    /// turn's start (`👀`) and end (`👀` removed, then `✅` or `❌`).
    React { chat: String, message_id: String, emoji: String, remove: bool },
    /// An op this connector does not take.
    Unsupported { op: String },
}

/// The frames in one WebSocket message: one JSON object a line. A line that
/// is not one is an error of its own; the rest still count.
pub fn frames(text: &str) -> Vec<Result<FromGateway, String>> {
    text.split('\n').map(str::trim).filter(|l| !l.is_empty()).map(frame).collect()
}

fn frame(line: &str) -> Result<FromGateway, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    let s = |o: &Value, k: &str| o[k].as_str().map(str::to_string);
    Ok(match v["type"].as_str() {
        Some("hello") => FromGateway::Hello,
        Some("inbound_ack") => FromGateway::InboundAck { buffer_id: s(&v, "bufferId").ok_or("inbound_ack names no bufferId")? },
        Some("going_idle") => FromGateway::GoingIdle,
        Some("outbound") => {
            let request_id = s(&v, "requestId").ok_or("outbound names no requestId")?;
            FromGateway::Outbound { request_id, action: action(&v["action"]) }
        }
        Some(other) => FromGateway::Other(other.to_string()),
        None => return Err("a frame with no type".into()),
    })
}

fn action(a: &Value) -> Action {
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let op = s("op");
    match op.as_str() {
        "send" => {
            // a reply answers a message (`reply_to`, or the metadata's
            // `reply_to_message_id` on a stream's first send); tool progress
            // answers none
            let reply = a["reply_to"].is_string() || a["metadata"]["reply_to_message_id"].is_string() || a["metadata"]["notify"] == json!(true);
            Action::Send { chat: s("chat_id"), content: s("content"), reply }
        }
        "edit" => Action::Edit { chat: s("chat_id"), message_id: s("message_id"), content: s("content") },
        "typing" => Action::Typing { chat: s("chat_id") },
        "delete" => Action::Delete { chat: s("chat_id"), message_id: s("message_id") },
        "react" => Action::React { chat: s("chat_id"), message_id: s("message_id"), emoji: s("emoji"), remove: a["remove"] == json!(true) },
        _ => Action::Unsupported { op },
    }
}

fn line(v: Value) -> String {
    let mut out = v.to_string();
    out.push('\n');
    out
}

/// The answer to each `hello`: what this platform is (the gateway's nine
/// required keys, and the ops it takes).
pub fn descriptor() -> String {
    line(json!({ "type": "descriptor", "descriptor": {
        "contract_version": CONTRACT_VERSION,
        "platform": PLATFORM,
        "label": "Fragment",
        "max_message_length": MESSAGE_MAX_CHARS,
        "supports_draft_streaming": false,
        "supports_edit": true,
        "supports_threads": false,
        "markdown_dialect": "markdown",
        "len_unit": "chars",
        "supported_ops": SUPPORTED_OPS,
    } }))
}

/// An action's answer.
pub fn result(request_id: &str, result: Value) -> String {
    line(json!({ "type": "outbound_result", "requestId": request_id, "result": result }))
}

pub fn going_idle_ack() -> String {
    line(json!({ "type": "going_idle_ack" }))
}

/// The session Hermes keeps for a chat: one, shared by everyone in it
/// (`group_sessions_per_user: false`).
pub fn session_key(chat: &str) -> String {
    format!("agent:main:{PLATFORM}:group:{chat}")
}

/// Stops the chat's running turn (never a `/stop` message: one while its
/// session is busy stalls the gateway's reader).
pub fn interrupt(chat: &str) -> String {
    line(json!({ "type": "interrupt_inbound", "session_key": session_key(chat), "chat_id": chat }))
}

/// A message of a chat's, as Hermes reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound<'a> {
    /// The chat: its fragment's name.
    pub chat: &'a str,
    pub chat_name: &'a str,
    /// The record's seq in the chat's channel: its id to Hermes, who keeps
    /// the last few to drop one delivered twice.
    pub seq: i64,
    /// Who wrote it: their identity, and the name Hermes calls them by.
    pub user_id: &'a str,
    pub user_name: &'a str,
    pub text: &'a str,
}

/// A message's delivery, buffered until the gateway acks it: a group
/// message, so Hermes reads it as `[name] …` in the chat's one session. A
/// leading `/` is a command to Hermes (`/new` would reset everyone's
/// session), so it is kept from reading as one.
pub fn inbound(m: &Inbound, buffer_id: &str) -> String {
    let text = match m.text.trim_start().starts_with('/') {
        true => format!("\u{200b}{}", m.text),
        false => m.text.to_string(),
    };
    event_line(m, buffer_id, &text)
}

/// The answers to a command Hermes asks the chat to approve before it runs
/// it (its `manual` approvals): the only commands the connector passes on,
/// and only from the chat's owner and editors, at once, even mid-turn
/// (Hermes reads them while it waits; anyone else's go as text, escaped,
/// in their turn).
pub fn approval(text: &str) -> bool {
    matches!(text.trim().to_ascii_lowercase().as_str(), "/approve" | "/approve session" | "/approve always" | "/deny")
}

/// An approval answer as Hermes reads it: the command itself, unescaped.
pub fn approval_inbound(m: &Inbound, buffer_id: &str) -> String {
    assert!(approval(m.text), "only an approval answer goes unescaped");
    event_line(m, buffer_id, &m.text.trim().to_ascii_lowercase())
}

fn event_line(m: &Inbound, buffer_id: &str, text: &str) -> String {
    line(json!({ "type": "inbound", "bufferId": buffer_id, "event": {
        "text": text,
        "message_id": m.seq.to_string(),
        "source": {
            "platform": PLATFORM,
            "chat_id": m.chat,
            "chat_type": "group",
            "chat_name": m.chat_name,
            "user_id": m.user_id,
            "user_name": m.user_name,
        },
    } }))
}

/// A message's text without the stream's cursor, and whether it still
/// streams (an interim edit ends with it; a final one does not).
pub fn uncursored(content: &str) -> (&str, bool) {
    match content.strip_suffix(CURSOR) {
        Some(text) => (text, true),
        None => (content, false),
    }
}

/// Hermes' tool progress is one message whose lines grow: the lines past
/// the first `seen`, which become the turn's steps.
pub fn new_lines(content: &str, seen: usize) -> Vec<String> {
    let (text, _) = uncursored(content);
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("```")).skip(seen).map(str::to_string).collect()
}

/// A chat's turn for the message it answers: its records' `turn`.
pub fn turn(seq: i64) -> String {
    format!("hermes:{seq}")
}

/// The seq of the message a turn answers, from Hermes' message id.
pub fn seq_of(message_id: &str) -> Option<i64> {
    message_id.parse().ok().filter(|s: &i64| *s > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gateway's own test vector (docs/hermes-relay.md): its token is
    /// ours, and ours is checked as the gateway mints it.
    #[test]
    fn a_token_is_the_gateways() {
        let t = token("gw-test", "s3cr3t", 1_790_000_000);
        assert_eq!(t, "Z3ctdGVzdDoxNzkwMDAwMDAwOjc4OGFkOWU3NmUyNWQzMzQ0NGJjZWJhOGQ2ZWRkYzM0MmIzZjVjYjliZmU5MTQ5NzNjZjYzNzZhMzdhMWNlYWQ");
        let bearer = format!("Bearer {t}");
        assert_eq!(verify(Some(&bearer), "gw-test", "s3cr3t", 1_790_000_000), Ok(()));
        assert_eq!(verify(Some(&bearer), "gw-test", "s3cr3t", 1_790_000_000 + TOKEN_SKEW_S), Ok(()), "within the skew");
        assert_eq!(verify(Some(&bearer), "gw-test", "s3cr3t", 1_790_000_000 + TOKEN_SKEW_S + 1), Err(Refused::Expired));
        assert_eq!(verify(Some(&bearer), "gw-test", "another", 1_790_000_000), Err(Refused::Unauthorized), "another secret");
        assert_eq!(verify(Some(&bearer), "gw-other", "s3cr3t", 1_790_000_000), Err(Refused::Unauthorized), "another gateway's");
        for bad in [None, Some(""), Some("Bearer"), Some("Bearer !!"), Some("Basic Z3c="), Some("Bearer Z3ctdGVzdDox")] {
            assert_eq!(verify(bad, "gw-test", "s3cr3t", 1_790_000_000), Err(Refused::Unauthorized), "{bad:?}");
        }
        // padded, as some encoders write it
        assert_eq!(verify(Some(&format!("{bearer}=")), "gw-test", "s3cr3t", 1_790_000_000), Ok(()));
        // an id with colons splits from the right
        let t = token("a:b", "k", 10);
        assert_eq!(verify(Some(&format!("Bearer {t}")), "a:b", "k", 10), Ok(()));
    }

    #[test]
    fn frames_read_one_a_line() {
        let text = "{\"type\":\"hello\",\"platform\":\"relay\",\"botId\":\"\"}\n{\"type\":\"inbound_ack\",\"bufferId\":\"7\"}\n\nnot json\n{\"type\":\"going_idle\"}\n{\"type\":\"interrupt\"}";
        let got = frames(text);
        assert_eq!(got.len(), 5);
        assert_eq!(got[0], Ok(FromGateway::Hello));
        assert_eq!(got[1], Ok(FromGateway::InboundAck { buffer_id: "7".into() }));
        assert!(got[2].is_err());
        assert_eq!(got[3], Ok(FromGateway::GoingIdle));
        assert_eq!(got[4], Ok(FromGateway::Other("interrupt".into())));
        let out = |a: Value| frames(&json!({ "type": "outbound", "requestId": "r1", "action": a }).to_string());
        assert_eq!(
            out(json!({ "op": "send", "chat_id": "c.a", "content": "Hi ▉", "reply_to": "12", "metadata": { "expect_edits": true } })),
            vec![Ok(FromGateway::Outbound { request_id: "r1".into(), action: Action::Send { chat: "c.a".into(), content: "Hi ▉".into(), reply: true } })]
        );
        assert_eq!(
            out(json!({ "op": "send", "chat_id": "c.a", "content": "💻 terminal", "reply_to": null, "metadata": { "user_id": "id:x" } })),
            vec![Ok(FromGateway::Outbound { request_id: "r1".into(), action: Action::Send { chat: "c.a".into(), content: "💻 terminal".into(), reply: false } })]
        );
        assert_eq!(
            out(json!({ "op": "react", "chat_id": "c.a", "message_id": "12", "emoji": "👀", "remove": true })),
            vec![Ok(FromGateway::Outbound { request_id: "r1".into(), action: Action::React { chat: "c.a".into(), message_id: "12".into(), emoji: "👀".into(), remove: true } })]
        );
        assert_eq!(out(json!({ "op": "send_media" })), vec![Ok(FromGateway::Outbound { request_id: "r1".into(), action: Action::Unsupported { op: "send_media".into() } })]);
        assert!(frames("{\"type\":\"outbound\"}")[0].is_err(), "an outbound with no request id");
    }

    #[test]
    fn what_is_sent_ends_each_frame_with_a_newline() {
        let d = descriptor();
        assert!(d.ends_with('\n'));
        let v: Value = serde_json::from_str(d.trim_end()).unwrap();
        for k in ["contract_version", "platform", "label", "max_message_length", "supports_draft_streaming", "supports_edit", "supports_threads", "markdown_dialect", "len_unit"] {
            assert!(!v["descriptor"][k].is_null(), "the gateway requires {k}");
        }
        assert_eq!(v["descriptor"]["supported_ops"], json!(SUPPORTED_OPS));
        let m = Inbound { chat: "talk.alice", chat_name: "talk", seq: 12, user_id: "id:bob", user_name: "bob", text: "/new please" };
        let i: Value = serde_json::from_str(inbound(&m, "4").trim_end()).unwrap();
        assert_eq!(i["bufferId"], "4");
        assert_eq!(i["event"]["message_id"], "12");
        assert_eq!(i["event"]["source"], json!({ "platform": "relay", "chat_id": "talk.alice", "chat_type": "group", "chat_name": "talk", "user_id": "id:bob", "user_name": "bob" }));
        assert_eq!(i["event"]["text"], "\u{200b}/new please", "a leading slash is kept from reading as a command");
        let plain = Inbound { text: "hi /there", ..m };
        assert_eq!(serde_json::from_str::<Value>(inbound(&plain, "5").trim_end()).unwrap()["event"]["text"], "hi /there");
        let s: Value = serde_json::from_str(interrupt("talk.alice").trim_end()).unwrap();
        assert_eq!(s, json!({ "type": "interrupt_inbound", "session_key": "agent:main:relay:group:talk.alice", "chat_id": "talk.alice" }));
        assert!(result("r1", json!({ "success": true })).ends_with('\n'));
    }

    /// Goal: an approval answer reaches Hermes as the command it is; every
    /// other slash stays text. Invalid: a near miss, or one with more after.
    #[test]
    fn only_approval_answers_go_as_commands() {
        for yes in ["/approve", "/approve session", " /Approve Always ", "/deny"] {
            assert!(approval(yes), "{yes:?}");
        }
        for no in ["/approved", "/approve now please", "approve", "/new", "/deny; /new", "/approve\n/new", ""] {
            assert!(!approval(no), "{no:?}");
        }
        let m = Inbound { chat: "talk.alice", chat_name: "talk", seq: 3, user_id: "id:alice", user_name: "alice", text: " /Approve Always " };
        let i: Value = serde_json::from_str(approval_inbound(&m, "cmd-3").trim_end()).unwrap();
        assert_eq!((i["bufferId"].as_str(), i["event"]["text"].as_str()), (Some("cmd-3"), Some("/approve always")));
        assert_eq!(serde_json::from_str::<Value>(inbound(&m, "3").trim_end()).unwrap()["event"]["text"], "\u{200b} /Approve Always ", "in its turn it is text");
    }

    #[test]
    #[should_panic(expected = "only an approval answer goes unescaped")]
    fn another_command_never_goes_unescaped() {
        let m = Inbound { chat: "talk.alice", chat_name: "talk", seq: 3, user_id: "id:bob", user_name: "bob", text: "/new" };
        approval_inbound(&m, "cmd-3");
    }

    #[test]
    fn a_reply_streams_and_progress_grows() {
        assert_eq!(uncursored("Hello wo ▉"), ("Hello wo", true));
        assert_eq!(uncursored("Hello world"), ("Hello world", false));
        let progress = "💻 terminal\n```\nls\n```\n🔍 Searching the web ▉";
        assert_eq!(new_lines(progress, 0), vec!["💻 terminal", "ls", "🔍 Searching the web"]);
        assert_eq!(new_lines(progress, 2), vec!["🔍 Searching the web"]);
        assert!(new_lines(progress, 3).is_empty());
        assert_eq!(turn(12), "hermes:12");
        assert_eq!((seq_of("12"), seq_of("0"), seq_of("m1")), (Some(12), None, None));
    }
}
