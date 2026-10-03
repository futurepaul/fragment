//! Hermes' Relay wire, as Hermes v0.21.5 (tag `v2026.9.24`, contract
//! version 1) speaks it, read from its `gateway/relay/` (docs/hermes-relay.md;
//! its code wins over its contract document). Pure: frames in, frames out.
//!
//! Frames are JSON objects, one per line, each ending with `\n`, each under
//! `limits::FRAME_MAX_BYTES`. Hermes dials; the bridge is its connector.

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use crate::records::PromptOption;

/// The contract this connector speaks.
pub const CONTRACT_VERSION: u64 = 1;
/// The platform the descriptor names: Hermes' own generic `relay`, so none
/// of its per-platform branches (Slack's threads, Telegram's emoji) apply.
pub const PLATFORM: &str = "relay";
/// A dial's token is good this much past its `exp` (the guest's own clock,
/// so this is only a safety margin).
pub const TOKEN_SKEW_S: i64 = 120;
const TOKEN_MAX_BYTES: usize = 1024;
/// The longest reply Hermes sends in one message (it splits longer ones).
pub const MESSAGE_MAX_CHARS: u64 = 16_000;
/// What Hermes appends to a message while it streams by edits.
pub const CURSOR: &str = " ▉";
/// The reactions that bracket a turn: `👀` on as it starts, off as it
/// ends, then `✅` or `❌` (Hermes has no other end-of-turn signal).
pub const STARTED: &str = "👀";
pub const DONE: &str = "✅";
pub const FAILED: &str = "❌";

/// The ops this connector takes (the descriptor's `supported_ops`). Of
/// decision 21's list, `task_card` exists in v0.21.5 but Hermes sends it
/// for Slack chats only (`run_turn.py`: `source.platform == SLACK`), so
/// tool steps arrive as progress text instead; `follow_up` is Discord's
/// interaction tokens. Neither is advertised.
pub const SUPPORTED_OPS: [&str; 9] = ["send", "edit", "delete", "typing", "react", "draft", "prompt", "send_media", "get_chat_info"];

/// The Authorization a gateway dials with: `Bearer
/// base64url(id:exp:hex(HMAC-SHA256(secret, "id:exp")))`, unpadded (its
/// `gateway/relay/auth.py`).
pub fn token(gateway_id: &str, secret: &str, exp: i64) -> String {
    let signed = format!("{gateway_id}:{exp}");
    let sig = crate::records::hex(&mac(secret, &signed));
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{signed}:{sig}"))
}

fn mac(secret: &str, msg: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    m.update(msg.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// Why a dial is refused: closed 4401, `expired` for a token past its time
/// (Hermes dials again), `unauthorized` otherwise (after a handshake once,
/// Hermes stops: its secret was revoked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    Expired,
    Unauthorized,
}

impl Refused {
    pub fn reason(self) -> &'static str {
        match self {
            Refused::Expired => "expired",
            Refused::Unauthorized => "unauthorized",
        }
    }
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// Checks an `Authorization` header: a token for `gateway_id` under
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
    let sig = unhex(sig).ok_or(Refused::Unauthorized)?;
    let mut m = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    m.update(format!("{payload}:{exp}").as_bytes());
    let signed_by_secret = m.verify_slice(&sig).is_ok();
    if !signed_by_secret || payload != gateway_id {
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
    /// One per fronted identity on each dial: answer with a descriptor.
    Hello,
    /// An action, answered by `outbound_result` under the same id (Hermes
    /// waits 30 s for it).
    Outbound { request_id: String, action: Action },
    /// It took a delivered inbound (its handler returned).
    InboundAck { buffer_id: String },
    /// It is going away on purpose: answer `going_idle_ack`.
    GoingIdle,
    /// A type this connector does not read (`interrupt`: a `/stop` typed in
    /// Hermes' own UI; the contract adds types additively).
    Other(String),
}

/// An action Hermes asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// A new message: a reply when it answers a message (`reply_to`, or
    /// the metadata's `reply_to_message_id`, or a final's `notify`), else
    /// its tool progress.
    Send { chat: String, content: String, reply: bool },
    /// The whole text of a message it sent, again.
    Edit { chat: String, message_id: String, content: String },
    /// A message it sent, taken back.
    Delete { chat: String, message_id: String },
    Typing { chat: String },
    /// A reaction on a message of the chat's.
    React { chat: String, message_id: String, emoji: String, remove: bool },
    /// A streamed reply's text so far; `final` seals it (only for Slack's
    /// stream-is-the-message chats: elsewhere the final is a `send`).
    Draft { chat: String, draft_id: u64, content: String, done: bool },
    /// A question with buttons; its answer is an inbound `prompt_response`.
    Prompt { chat: String, prompt_id: String, content: String, options: Vec<PromptOption>, timeout_s: Option<u64> },
    /// A file, uploaded to `/relay/media` first (`source_url`).
    SendMedia { chat: String, media_kind: String, source_url: String, caption: String, filename: Option<String> },
    GetChatInfo { chat: String },
    /// An op this connector does not take: answered `{success: false}`.
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

fn options(v: &Value) -> Vec<PromptOption> {
    let Some(items) = v.as_array() else { return Vec::new() };
    items
        .iter()
        .filter_map(|o| {
            let id = o["id"].as_str()?.to_string();
            let label = o["label"].as_str().unwrap_or(&id).to_string();
            let style = o["style"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
            Some(PromptOption { id, label, style })
        })
        .collect()
}

fn action(a: &Value) -> Action {
    let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
    let op = s("op");
    match op.as_str() {
        "send" => {
            let reply = a["reply_to"].is_string() || a["metadata"]["reply_to_message_id"].is_string() || a["metadata"]["notify"] == json!(true);
            Action::Send { chat: s("chat_id"), content: s("content"), reply }
        }
        "edit" => Action::Edit { chat: s("chat_id"), message_id: s("message_id"), content: s("content") },
        "delete" => Action::Delete { chat: s("chat_id"), message_id: s("message_id") },
        "typing" => Action::Typing { chat: s("chat_id") },
        "react" => Action::React { chat: s("chat_id"), message_id: s("message_id"), emoji: s("emoji"), remove: a["remove"] == json!(true) },
        "draft" => Action::Draft { chat: s("chat_id"), draft_id: a["draft_id"].as_u64().unwrap_or(0), content: s("content"), done: a["final"] == json!(true) },
        "prompt" => Action::Prompt { chat: s("chat_id"), prompt_id: s("prompt_id"), content: s("content"), options: options(&a["options"]), timeout_s: a["timeout_s"].as_u64() },
        "send_media" => Action::SendMedia {
            chat: s("chat_id"),
            media_kind: s("media_kind"),
            source_url: s("source_url"),
            caption: s("content"),
            filename: a["filename"].as_str().map(str::to_string),
        },
        "get_chat_info" => Action::GetChatInfo { chat: s("chat_id") },
        _ => Action::Unsupported { op },
    }
}

fn line(v: Value) -> String {
    let mut out = v.to_string();
    out.push('\n');
    out
}

/// The answer to each `hello`: the gateway's required keys and the ops
/// this connector takes. Draft streaming on, so a reply streams as `draft`
/// frames and its final arrives as one `send`.
pub fn descriptor() -> String {
    line(json!({ "type": "descriptor", "descriptor": {
        "contract_version": CONTRACT_VERSION,
        "platform": PLATFORM,
        "label": "Fragment",
        "max_message_length": MESSAGE_MAX_CHARS,
        "supports_draft_streaming": true,
        "supports_edit": true,
        "supports_threads": false,
        "markdown_dialect": "markdown",
        "len_unit": "chars",
        "supported_ops": SUPPORTED_OPS,
    } }))
}

pub fn result(request_id: &str, result: Value) -> String {
    line(json!({ "type": "outbound_result", "requestId": request_id, "result": result }))
}

pub fn going_idle_ack() -> String {
    line(json!({ "type": "going_idle_ack" }))
}

/// A Hermes profile id for an agent fragment (`^[a-z0-9][a-z0-9_-]{0,63}$`,
/// Hermes' `PROFILE_ID_RE`): its name with `.` as `-`, so `juniper.paul` is
/// `juniper-paul`. A name that does not fit is cut and keeps a hash of the
/// whole, so two agents never share a profile.
pub fn profile(agent_fragment: &str) -> String {
    let mut out: String = agent_fragment.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c.to_ascii_lowercase() } else { '-' }).collect();
    if !out.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        out.insert(0, 'a');
    }
    // Hermes reserves a few names; ours always hold a `-` (`label-user`),
    // but a hand-made one may not.
    if matches!(out.as_str(), "hermes" | "default" | "test" | "tmp" | "root" | "sudo" | "main") {
        out.push_str("-agent");
    }
    if out.len() > 64 {
        let digest = <Sha256 as sha2::Digest>::digest(agent_fragment.as_bytes());
        out.truncate(55);
        out.push('-');
        out.push_str(&crate::records::hex(&digest)[..8]);
    }
    assert!(valid_profile(&out), "a profile id: {out}");
    out
}

pub fn valid_profile(p: &str) -> bool {
    let first_ok = p.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    first_ok && p.len() <= 64 && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The chat id Hermes is given: the chat fragment and the agent, so two
/// agents in one chat are two chats to Hermes (its adapter keeps per-chat
/// state by this id alone).
pub fn chat_id(fragment: &str, agent_fragment: &str) -> String {
    format!("{fragment}/{agent_fragment}")
}

/// The chat fragment and agent of a chat id.
pub fn split_chat_id(chat: &str) -> Option<(&str, &str)> {
    chat.split_once('/').filter(|(f, a)| !f.is_empty() && !a.is_empty())
}

/// The session Hermes keeps for one agent's chat: one per chat, shared by
/// everyone in it (`group_sessions_per_user: false`), in its profile's
/// namespace (Hermes' `build_session_key`).
pub fn session_key(profile: &str, chat: &str) -> String {
    let ns = match profile {
        "" | "default" => "main".to_string(),
        "main" => "main~".to_string(),
        p => p.to_string(),
    };
    format!("agent:{ns}:{PLATFORM}:group:{chat}")
}

/// Stops a chat's running turn (never a `/stop` message: one while the
/// session is busy stalls Hermes' reader).
pub fn interrupt(profile: &str, chat: &str) -> String {
    line(json!({ "type": "interrupt_inbound", "session_key": session_key(profile, chat), "chat_id": chat }))
}

/// A message of a chat's, as Hermes reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct Inbound<'a> {
    pub chat: &'a str,
    pub chat_name: &'a str,
    /// The profile that answers it (Hermes' multiplexed gateway routes on it).
    pub profile: &'a str,
    /// Its id to Hermes, who drops one delivered twice by it.
    pub message_id: &'a str,
    pub user_id: &'a str,
    pub user_name: &'a str,
    pub text: &'a str,
    /// Re-hosted attachments: `(url, media type)`.
    pub media: &'a [(String, String)],
}

/// A delivery, buffered until the gateway acks it: a group message, so
/// Hermes reads it as `[name] …` in the chat's one session. A leading `/`
/// is a command to Hermes (`/new` would reset the session), so it is kept
/// from reading as one.
pub fn inbound(m: &Inbound, buffer_id: &str) -> String {
    let text = match m.text.trim_start().starts_with('/') {
        true => format!("\u{200b}{}", m.text),
        false => m.text.to_string(),
    };
    line(event(m, buffer_id, &text, None))
}

/// A prompt's answer: a structured `prompt_response` Hermes resolves
/// mid-turn, never dispatched as chat. `text` is the option as Hermes' own
/// buttons send it.
pub fn prompt_answer(m: &Inbound, buffer_id: &str, prompt_id: &str, option_id: &str) -> String {
    line(event(m, buffer_id, &format!("/{option_id}"), Some(json!({ "prompt_id": prompt_id, "option_id": option_id }))))
}

fn event(m: &Inbound, buffer_id: &str, text: &str, prompt_response: Option<Value>) -> Value {
    let mut event = json!({
        "text": text,
        "message_id": m.message_id,
        "source": {
            "platform": PLATFORM,
            "chat_id": m.chat,
            "chat_type": "group",
            "chat_name": m.chat_name,
            "user_id": m.user_id,
            "user_name": m.user_name,
            "profile": m.profile,
            "message_id": m.message_id,
        },
    });
    if !m.media.is_empty() {
        event["media_urls"] = json!(m.media.iter().map(|(u, _)| u).collect::<Vec<_>>());
        event["media"] = json!(m.media.iter().map(|(u, t)| json!({ "url": u, "mime": t })).collect::<Vec<_>>());
    }
    if let Some(pr) = prompt_response {
        event["prompt_response"] = pr;
    }
    json!({ "type": "inbound", "bufferId": buffer_id, "event": event })
}

/// A message's text without the edit stream's cursor, and whether it still
/// streams.
pub fn uncursored(content: &str) -> (&str, bool) {
    match content.strip_suffix(CURSOR) {
        Some(text) => (text, true),
        None => (content, false),
    }
}

/// Hermes' tool progress is one message whose lines grow: the lines past
/// the first `seen`.
pub fn new_lines(content: &str, seen: usize) -> Vec<String> {
    let (text, _) = uncursored(content);
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("```")).skip(seen).map(str::to_string).collect()
}

/// A progress line as a step: its tool (the first word after the emoji)
/// and the rest as its arguments (`💻 terminal: \`ls\``).
pub fn step_of(line: &str) -> (String, String) {
    let body = line.trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).trim();
    if body.is_empty() {
        return (line.trim().to_string(), String::new());
    }
    let end = body.find(|c: char| c == ':' || c.is_whitespace() || c == '(').unwrap_or(body.len());
    let tool = body[..end].to_string();
    let rest = body[end..].trim_start_matches([':', ' ']).trim();
    (tool, rest.to_string())
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
        assert_eq!(verify(Some(&bearer), "gw-test", "another", 1_790_000_000), Err(Refused::Unauthorized));
        assert_eq!(verify(Some(&bearer), "gw-other", "s3cr3t", 1_790_000_000), Err(Refused::Unauthorized));
        for bad in [None, Some(""), Some("Bearer"), Some("Bearer !!"), Some("Basic Z3c="), Some("Bearer Z3ctdGVzdDox")] {
            assert_eq!(verify(bad, "gw-test", "s3cr3t", 1_790_000_000), Err(Refused::Unauthorized), "{bad:?}");
        }
        assert_eq!(verify(Some(&format!("{bearer}=")), "gw-test", "s3cr3t", 1_790_000_000), Ok(()), "padded");
        let t = token("a:b", "k", 10);
        assert_eq!(verify(Some(&format!("Bearer {t}")), "a:b", "k", 10), Ok(()), "an id with colons splits from the right");
        // exp 0 never expires (Hermes' make_token with no ttl)
        let forever = token("gw", "k", 0);
        assert_eq!(verify(Some(&format!("Bearer {forever}")), "gw", "k", i64::MAX - TOKEN_SKEW_S), Ok(()));
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
        let out = |a: Value| frames(&json!({ "type": "outbound", "requestId": "r1", "action": a }).to_string()).remove(0).expect("a frame");
        assert_eq!(out(json!({ "op": "send", "chat_id": "c", "content": "Hi", "reply_to": "12" })), FromGateway::Outbound { request_id: "r1".into(), action: Action::Send { chat: "c".into(), content: "Hi".into(), reply: true } });
        assert_eq!(out(json!({ "op": "send", "chat_id": "c", "content": "💻 terminal", "reply_to": null, "metadata": {} })), FromGateway::Outbound { request_id: "r1".into(), action: Action::Send { chat: "c".into(), content: "💻 terminal".into(), reply: false } });
        assert_eq!(out(json!({ "op": "send", "chat_id": "c", "content": "x", "metadata": { "notify": true } })), FromGateway::Outbound { request_id: "r1".into(), action: Action::Send { chat: "c".into(), content: "x".into(), reply: true } });
        assert_eq!(out(json!({ "op": "draft", "chat_id": "c", "draft_id": 3, "content": "He", "final": false })), FromGateway::Outbound { request_id: "r1".into(), action: Action::Draft { chat: "c".into(), draft_id: 3, content: "He".into(), done: false } });
        let p = out(json!({ "op": "prompt", "chat_id": "c", "content": "Run?", "prompt_kind": "approval", "prompt_id": "ab.12", "options": [{ "id": "once", "label": "Allow Once", "style": "primary" }, { "id": "deny", "label": "Deny" }] }));
        let FromGateway::Outbound { action: Action::Prompt { prompt_id, options, .. }, .. } = p else { panic!("a prompt") };
        assert_eq!(prompt_id, "ab.12");
        assert_eq!(options[0], PromptOption { id: "once".into(), label: "Allow Once".into(), style: Some("primary".into()) });
        assert_eq!(options[1].style, None);
        assert_eq!(out(json!({ "op": "task_card" })), FromGateway::Outbound { request_id: "r1".into(), action: Action::Unsupported { op: "task_card".into() } });
        assert!(frames("{\"type\":\"outbound\"}")[0].is_err(), "an outbound with no request id");
    }

    #[test]
    fn what_is_sent_is_one_frame_a_line() {
        let d = descriptor();
        assert!(d.ends_with('\n'));
        let v: Value = serde_json::from_str(d.trim_end()).unwrap();
        for k in ["contract_version", "platform", "label", "max_message_length", "supports_draft_streaming", "supports_edit", "supports_threads", "markdown_dialect", "len_unit"] {
            assert!(!v["descriptor"][k].is_null(), "the gateway requires {k}");
        }
        assert_eq!(v["descriptor"]["supported_ops"], json!(SUPPORTED_OPS));
        let media = vec![("http://127.0.0.1:1/relay/media/m1".to_string(), "image/png".to_string())];
        let m = Inbound { chat: "talk.paul/juniper.paul", chat_name: "talk", profile: "juniper-paul", message_id: "12", user_id: "id:bob", user_name: "bob", text: "/new please", media: &media };
        let i: Value = serde_json::from_str(inbound(&m, "b4").trim_end()).unwrap();
        assert_eq!(i["bufferId"], "b4");
        assert_eq!(i["event"]["message_id"], "12");
        assert_eq!(i["event"]["source"]["profile"], "juniper-paul");
        assert_eq!(i["event"]["source"]["chat_type"], "group");
        assert_eq!(i["event"]["text"], "\u{200b}/new please", "a leading slash is kept from reading as a command");
        assert_eq!(i["event"]["media"][0]["mime"], "image/png");
        let a: Value = serde_json::from_str(prompt_answer(&Inbound { media: &[], ..m.clone() }, "a1", "ab.12", "once").trim_end()).unwrap();
        assert_eq!(a["event"]["prompt_response"], json!({ "prompt_id": "ab.12", "option_id": "once" }));
        assert_eq!(a["event"]["text"], "/once");
        let s: Value = serde_json::from_str(interrupt("juniper-paul", "talk.paul/juniper.paul").trim_end()).unwrap();
        assert_eq!(s, json!({ "type": "interrupt_inbound", "session_key": "agent:juniper-paul:relay:group:talk.paul/juniper.paul", "chat_id": "talk.paul/juniper.paul" }));
        assert_eq!(session_key("default", "c"), "agent:main:relay:group:c");
        assert_eq!(session_key("main", "c"), "agent:main~:relay:group:c");
    }

    #[test]
    fn profiles_are_hermes_ids() {
        assert_eq!(profile("juniper.paul"), "juniper-paul");
        assert_eq!(profile("Juniper.Paul"), "juniper-paul");
        assert_eq!(profile("-x.y"), "a-x-y");
        assert_eq!(profile("default"), "default-agent");
        let long = format!("{}.paul", "a".repeat(80));
        let p = profile(&long);
        assert!(p.len() <= 64 && valid_profile(&p), "{p}");
        assert_ne!(p, profile(&format!("{}.paul", "a".repeat(81))), "two long names, two profiles");
        assert_eq!(split_chat_id(&chat_id("talk.paul", "juniper.paul")), Some(("talk.paul", "juniper.paul")));
        assert_eq!(split_chat_id("nochat"), None);
    }

    #[test]
    fn progress_grows_into_steps() {
        assert_eq!(uncursored("Hello wo ▉"), ("Hello wo", true));
        assert_eq!(uncursored("Hello world"), ("Hello world", false));
        let progress = "💻 terminal: `ls`\n```\nls\n```\n🔍 web_search: \"rust\" ▉";
        assert_eq!(new_lines(progress, 0), vec!["💻 terminal: `ls`", "ls", "🔍 web_search: \"rust\""]);
        assert_eq!(new_lines(progress, 2), vec!["🔍 web_search: \"rust\""]);
        assert!(new_lines(progress, 3).is_empty());
        assert_eq!(step_of("💻 terminal: `ls -la`"), ("terminal".into(), "`ls -la`".into()));
        assert_eq!(step_of("🔍 web_search(\"q\")"), ("web_search".into(), "(\"q\")".into()));
        assert_eq!(step_of("⚙️ thinking..."), ("thinking...".into(), String::new()));
        assert_eq!(step_of("✅"), ("✅".into(), String::new()));
    }
}
