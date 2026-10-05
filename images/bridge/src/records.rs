//! A chat's records (docs/chat-records.md): what the bridge reads from a
//! chat's `chat` channel and an agent's `tasks` channel, what it posts to
//! `chat` and `work`, and the ids that make every post once.
//!
//! Reading is strict at the edges: a body that does not hold its kind's
//! shape is `Said::Other` (a page's own record, or a broken one), never a
//! guess. Building is total: every body here is one the doc defines.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::limits;

/// The chat's channel for what is said: people's messages, agents' replies,
/// Stop, and prompt answers.
pub const CHAT: &str = "chat";
/// The chat's channel for an agent's progress (editors post: so a person
/// cannot forge a step or a prompt).
pub const WORK: &str = "work";
/// An agent fragment's channel for what is asked of it outside a chat.
pub const TASKS: &str = "tasks";

/// A record as the platform gives it (`GET …/channels/{c}`, `__live`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub channel: String,
    pub seq: u64,
    #[serde(default)]
    pub at: i64,
    pub principal: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub body: Value,
}

/// A file a record carries: one of the fragment's blobs, by hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub sha256: String,
    pub size: u64,
    #[serde(rename = "type")]
    pub media_type: String,
    pub name: String,
}

/// A message: a person's, or an agent's reply (it names its `turn`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Message {
    pub text: String,
    /// Whom it is for: agents' identities. Empty: whoever the chat's rules
    /// pick (an `@mention`, else the lead).
    pub to: Vec<String>,
    pub attachments: Vec<AttachmentRef>,
    /// An agent's reply names the turn it answers.
    pub turn: Option<String>,
    /// How many agent hand-offs led here (0 for a person's message).
    pub hop: u32,
}

/// What a `chat` record says to an agent.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    Message(Message),
    /// Stop the turn it names (or the asker's running one).
    Stop { turn: Option<String> },
    /// An answer to a prompt: its id, and the option picked.
    PromptResponse { prompt: String, option: String },
    /// Anything else: a page's own kind, or a body that is not a message.
    Other,
}

/// What a `tasks` record asks of an agent.
#[derive(Debug, Clone, PartialEq)]
pub enum Task {
    /// A routine fired (the agent fragment's cron posted it): a turn in
    /// `chat`, as the agent's owner asked it.
    Routine { text: String, chat: String },
    /// The agent was added to a fragment: list them again now. That
    /// fragment's members changed, so every view of it read before is stale
    /// (another agent of this computer in it would miss the new one).
    Joined { fragment: Option<String> },
    Other,
}

fn text_field(o: &Map<String, Value>, key: &str) -> Option<String> {
    match o.get(key) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Whether `s` is an id of the alphabet records use for prompts and
/// options (`^[A-Za-z0-9._-]{1,max}$`).
pub fn valid_token(s: &str, max: usize) -> bool {
    let len_ok = !s.is_empty() && s.len() <= max;
    len_ok && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `s` is a post id the platform takes (`^[A-Za-z0-9._:-]{1,128}$`).
pub fn valid_post_id(s: &str) -> bool {
    let len_ok = !s.is_empty() && s.len() <= 128;
    len_ok && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn attachment(v: &Value) -> Option<AttachmentRef> {
    let a: AttachmentRef = serde_json::from_value(v.clone()).ok()?;
    let sha_ok = a.sha256.len() == 64 && a.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
    let size_ok = a.size > 0 && a.size <= limits::ATTACHMENT_MAX_BYTES;
    let names_ok = !a.media_type.is_empty() && a.media_type.len() <= 100 && !a.name.is_empty() && a.name.len() <= 200;
    if sha_ok && size_ok && names_ok {
        Some(a)
    } else {
        None
    }
}

/// Reads a `chat` record's body.
pub fn said(body: &Value) -> Said {
    let o = match body {
        Value::String(text) => return Said::Message(Message { text: text.clone(), ..Message::default() }),
        Value::Object(o) => o,
        _ => return Said::Other,
    };
    match o.get("kind") {
        None => message(o),
        Some(Value::String(k)) if k == "message" => message(o),
        Some(Value::String(k)) if k == "stop" => match o.get("turn") {
            None | Some(Value::Null) => Said::Stop { turn: None },
            Some(Value::String(t)) if valid_post_id(t) => Said::Stop { turn: Some(t.clone()) },
            Some(_) => Said::Other,
        },
        Some(Value::String(k)) if k == "prompt_response" => {
            let (prompt, option) = (text_field(o, "prompt"), text_field(o, "option"));
            match (prompt, option) {
                (Some(p), Some(opt)) if valid_token(&p, 64) && valid_token(&opt, 32) => Said::PromptResponse { prompt: p, option: opt },
                _ => Said::Other,
            }
        }
        Some(_) => Said::Other,
    }
}

fn message(o: &Map<String, Value>) -> Said {
    let attachments = match o.get("attachments") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) if items.len() <= limits::ATTACHMENTS_MAX => {
            let parsed: Vec<AttachmentRef> = items.iter().filter_map(attachment).collect();
            if parsed.len() != items.len() {
                return Said::Other;
            }
            parsed
        }
        Some(_) => return Said::Other,
    };
    let text = match o.get("text") {
        Some(Value::String(t)) => t.clone(),
        None if !attachments.is_empty() => String::new(),
        _ => return Said::Other,
    };
    let to = match o.get("to") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(ids)) if ids.len() <= 16 => {
            let parsed: Vec<String> = ids.iter().filter_map(|v| v.as_str()).filter(|s| s.starts_with("id:") && s.len() <= 128).map(str::to_string).collect();
            if parsed.len() != ids.len() {
                return Said::Other;
            }
            parsed
        }
        Some(_) => return Said::Other,
    };
    let turn = match o.get("turn") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) if valid_post_id(t) => Some(t.clone()),
        Some(_) => return Said::Other,
    };
    let hop = match o.get("hop") {
        None | Some(Value::Null) => 0,
        Some(v) => match v.as_u64().and_then(|h| u32::try_from(h).ok()) {
            Some(h) => h,
            None => return Said::Other,
        },
    };
    Said::Message(Message { text, to, attachments, turn, hop })
}

/// Reads a `tasks` record's body.
pub fn task(body: &Value) -> Task {
    let Value::Object(o) = body else { return Task::Other };
    match o.get("kind").and_then(Value::as_str) {
        Some("routine") => match (text_field(o, "text"), text_field(o, "chat")) {
            (Some(text), Some(chat)) if !chat.is_empty() && chat.len() <= 128 => Task::Routine { text, chat },
            _ => Task::Other,
        },
        Some("joined") => Task::Joined { fragment: text_field(o, "fragment").filter(|f| !f.is_empty() && f.len() <= 128) },
        _ => Task::Other,
    }
}

/// The `@name` words of a message, lowercased, in order (a word is
/// `[A-Za-z0-9_-]+` right after an `@` that does not follow a word
/// character, so an email address is no mention).
pub fn mentions(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // bounded by the text's length: each pass moves `i` forward
    while i < bytes.len() {
        let after_word = i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if bytes[i] == b'@' && !after_word {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_' || bytes[end] == b'-') {
                end += 1;
            }
            if end > start {
                out.push(text[start..end].to_ascii_lowercase());
            }
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }
    out
}

// ---- ids ----

/// A turn's id: what one agent does about one record, so a record caught up
/// on twice (after a restart, a wake, a deploy) is one turn (lesson 2), and
/// two agents answering one message are two turns.
pub fn turn_id(agent: &str, fragment: &str, channel: &str, seq: u64) -> String {
    let digest = Sha256::digest(format!("{agent}|{fragment}/{channel}/{seq}").as_bytes());
    hex(&digest)[..24].to_string()
}

/// A turn of an agent's own (a message its runtime sent with no turn
/// running): numbered by the bridge's own counter within its life. The
/// counter is kept in `/data`, which a rollback sends back, so the life is
/// in the id too: a number said again in a later life names a new turn,
/// never one an earlier life posted with other text (a 409).
pub fn said_turn_id(agent: &str, life: &str, n: u64) -> String {
    assert!(valid_life(life), "a life is 32 lowercase hex: {life}");
    let digest = Sha256::digest(format!("{agent}|said/{life}/{n}").as_bytes());
    hex(&digest)[..24].to_string()
}

/// Whether `s` is a life: one bridge process's 128 random bits, as 32
/// lowercase hex (docs/chat-records.md, `turn.start`).
pub fn valid_life(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    out
}

/// A progress record's post id: `wk:<turn>:<part>`.
pub fn work_id(turn: &str, part: &str) -> String {
    let id = format!("wk:{turn}:{part}");
    assert!(valid_post_id(&id), "a work id is a post id: {id}");
    id
}

/// A reply's post id: `rp:<turn>:<n>`, its replies numbered from 1.
pub fn reply_id(turn: &str, n: u32) -> String {
    assert!(n >= 1, "replies number from 1");
    let id = format!("rp:{turn}:{n}");
    assert!(valid_post_id(&id), "a reply id is a post id: {id}");
    id
}

// ---- what the bridge posts ----

/// `text` cut to at most `max` characters, a cut marked with `…`.
pub fn cut(text: &str, max: usize) -> String {
    assert!(max > 0, "a cut keeps something");
    let text = text.trim();
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((at, _)) => {
            let keep = text[..at].char_indices().nth(max - 1).map_or(at, |(i, _)| i);
            format!("{}…", &text[..keep])
        }
    }
}

/// `text` cut to at most `max` bytes on a character boundary, marked `…`.
pub fn cut_bytes(text: &str, max: usize) -> String {
    assert!(max > 4, "a cut keeps something");
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max - '…'.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

/// What started a turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cause {
    pub fragment: String,
    pub channel: String,
    pub seq: u64,
}

/// A turn's claim: "this life runs this turn" (docs/chat-records.md). The
/// platform answers the same id and body again as a replay and another
/// body as 409, so with the life in the body this life's own retry is a
/// replay and any other life's claim of the turn is a 409.
pub fn turn_start(turn: &str, asker: &str, agent: &str, cause: &Cause, life: &str) -> Value {
    assert!(valid_life(life), "a life is 32 lowercase hex: {life}");
    json!({ "kind": "turn.start", "turn": turn, "asker": asker, "agent": agent,
        "cause": { "fragment": cause.fragment, "channel": cause.channel, "seq": cause.seq }, "life": life })
}

/// One step of a turn (a tool call), numbered from 1.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub tool: String,
    pub args: String,
    pub ok: bool,
    pub excerpt: String,
    /// The model's text before the call, if any.
    pub text: String,
}

pub fn turn_step(turn: &str, n: u32, s: &Step) -> Value {
    assert!(n >= 1, "steps number from 1");
    let mut body = json!({
        "kind": "turn.step",
        "turn": turn,
        "step": n,
        "tool": cut(&s.tool, limits::STEP_TOOL_MAX_CHARS),
        "args": cut_or_empty(&s.args, limits::STEP_ARGS_MAX_CHARS),
        "ok": s.ok,
        "excerpt": cut_or_empty(&s.excerpt, limits::STEP_EXCERPT_MAX_CHARS),
    });
    let text = cut_or_empty(&s.text, limits::STEP_EXCERPT_MAX_CHARS);
    if !text.is_empty() {
        body["text"] = json!(text);
    }
    body
}

fn cut_or_empty(text: &str, max: usize) -> String {
    if text.trim().is_empty() {
        String::new()
    } else {
        cut(text, max)
    }
}

/// One choice of a prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
}

pub fn turn_prompt(turn: &str, prompt: &str, text: &str, options: &[PromptOption], asks: &str, expires_at: u64) -> Value {
    assert!(!options.is_empty() && options.len() <= limits::PROMPT_OPTIONS_MAX, "a prompt offers 1 to {} options", limits::PROMPT_OPTIONS_MAX);
    json!({ "kind": "turn.prompt", "turn": turn, "prompt": prompt, "text": cut(text, limits::PROMPT_TEXT_MAX_CHARS),
        "options": options, "asks": asks, "expiresAt": expires_at })
}

/// How a prompt closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Closed {
    Answered,
    Expired,
    Stopped,
}

impl Closed {
    pub fn as_str(self) -> &'static str {
        match self {
            Closed::Answered => "answered",
            Closed::Expired => "expired",
            Closed::Stopped => "stopped",
        }
    }
}

pub fn turn_prompt_closed(turn: &str, prompt: &str, closed: Closed, answer: Option<(&str, &str)>) -> Value {
    let mut body = json!({ "kind": "turn.prompt.closed", "turn": turn, "prompt": prompt, "outcome": closed.as_str() });
    match (closed, answer) {
        (Closed::Answered, Some((option, by))) => {
            body["option"] = json!(option);
            body["by"] = json!(by);
        }
        (Closed::Answered, None) => panic!("an answered prompt names its answer"),
        (_, _) => {}
    }
    body
}

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It answered (or finished without words).
    Idle,
    /// Its asker stopped it.
    Stopped,
    /// It failed, with what was wrong.
    Error(String),
}

pub fn turn_end(turn: &str, outcome: &Outcome) -> Value {
    match outcome {
        Outcome::Idle => json!({ "kind": "turn.end", "turn": turn, "outcome": "idle" }),
        Outcome::Stopped => json!({ "kind": "turn.end", "turn": turn, "outcome": "stopped" }),
        Outcome::Error(e) => json!({ "kind": "turn.end", "turn": turn, "outcome": "error", "error": cut(e, limits::ERROR_MAX_CHARS) }),
    }
}

/// An agent's reply on `chat`: its text and turn; `to` and `hop` when it
/// hands off to another agent. Attachments are added by whoever uploads them.
pub fn reply(text: &str, turn: &str, to: &[String], hop: u32) -> Value {
    let mut body = json!({ "text": cut_bytes(text, limits::REPLY_TEXT_MAX_BYTES), "turn": turn });
    if !to.is_empty() {
        body["to"] = json!(to);
        body["hop"] = json!(hop);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_read_strictly() {
        assert_eq!(said(&json!({ "text": "hi" })), Said::Message(Message { text: "hi".into(), ..Message::default() }));
        assert_eq!(said(&json!({ "kind": "message", "text": "hi" })), Said::Message(Message { text: "hi".into(), ..Message::default() }));
        assert_eq!(said(&json!("plain")), Said::Message(Message { text: "plain".into(), ..Message::default() }));
        let sha = "a".repeat(64);
        let m = said(&json!({ "text": "look", "to": ["id:aa"], "attachments": [{ "sha256": sha, "size": 3, "type": "image/png", "name": "a.png" }] }));
        let Said::Message(m) = m else { panic!("a message") };
        assert_eq!(m.to, vec!["id:aa".to_string()]);
        assert_eq!(m.attachments.len(), 1);
        // a reply names its turn and may hand off
        let Said::Message(r) = said(&json!({ "text": "ok", "turn": "abc", "to": ["id:b"], "hop": 2 })) else { panic!() };
        assert_eq!((r.turn.as_deref(), r.hop), (Some("abc"), 2));
        // invalid shapes are never guessed at
        for bad in [
            json!(12),
            json!({ "text": 3 }),
            json!({}),
            json!({ "text": "x", "to": "id:a" }),
            json!({ "text": "x", "to": ["someone"] }),
            json!({ "text": "x", "attachments": [{ "sha256": "zz", "size": 1, "type": "a", "name": "b" }] }),
            json!({ "text": "x", "attachments": [{ "sha256": "A".repeat(64), "size": 1, "type": "a", "name": "b" }] }),
            json!({ "text": "x", "attachments": [{ "sha256": "a".repeat(64), "size": 0, "type": "a", "name": "b" }] }),
            json!({ "text": "x", "turn": "has space" }),
            json!({ "text": "x", "hop": -1 }),
            json!({ "kind": "draft", "text": "x" }),
            json!({ "kind": 3 }),
        ] {
            assert_eq!(said(&bad), Said::Other, "{bad}");
        }
    }

    #[test]
    fn stops_and_answers() {
        assert_eq!(said(&json!({ "kind": "stop" })), Said::Stop { turn: None });
        assert_eq!(said(&json!({ "kind": "stop", "turn": "t1" })), Said::Stop { turn: Some("t1".into()) });
        assert_eq!(said(&json!({ "kind": "stop", "turn": 1 })), Said::Other);
        assert_eq!(said(&json!({ "kind": "prompt_response", "prompt": "ab.12", "option": "once" })), Said::PromptResponse { prompt: "ab.12".into(), option: "once".into() });
        for bad in [json!({ "kind": "prompt_response", "prompt": "ab" }), json!({ "kind": "prompt_response", "prompt": "a b", "option": "x" }), json!({ "kind": "prompt_response", "prompt": "a", "option": "" })] {
            assert_eq!(said(&bad), Said::Other, "{bad}");
        }
    }

    #[test]
    fn tasks_read() {
        assert_eq!(task(&json!({ "kind": "routine", "text": "water", "chat": "c.paul" })), Task::Routine { text: "water".into(), chat: "c.paul".into() });
        assert_eq!(task(&json!({ "kind": "joined", "fragment": "c.paul" })), Task::Joined { fragment: Some("c.paul".into()) });
        assert_eq!(task(&json!({ "kind": "joined" })), Task::Joined { fragment: None }, "a join naming no fragment still lists them again");
        assert_eq!(task(&json!({ "kind": "joined", "fragment": "" })), Task::Joined { fragment: None });
        assert_eq!(task(&json!({ "kind": "routine", "text": "water" })), Task::Other);
        assert_eq!(task(&json!("routine")), Task::Other);
    }

    #[test]
    fn mentions_are_words_after_an_at() {
        assert_eq!(mentions("@Juniper can you, @bob-2?"), vec!["juniper", "bob-2"]);
        assert_eq!(mentions("mail me at a@b.com"), Vec::<String>::new());
        assert_eq!(mentions("@ alone, @@x"), vec!["x"]);
        assert_eq!(mentions(""), Vec::<String>::new());
        assert_eq!(mentions("é@ana"), vec!["ana"], "a non-ASCII letter is no word character here");
    }

    #[test]
    fn ids_are_stable_and_valid() {
        let t = turn_id("juniper.paul", "talk.paul", "chat", 12);
        assert_eq!(t.len(), 24);
        assert_eq!(t, turn_id("juniper.paul", "talk.paul", "chat", 12), "the same record, the same turn");
        assert_ne!(t, turn_id("rowan.paul", "talk.paul", "chat", 12), "another agent, another turn");
        assert_ne!(t, turn_id("juniper.paul", "talk.paul", "chat", 13));
        for id in [work_id(&t, "start"), work_id(&t, "7"), work_id(&t, "p:ab12cd.0011aabb"), work_id(&t, "end"), reply_id(&t, 1)] {
            assert!(valid_post_id(&id), "{id}");
        }
        let (one, two) = ("0".repeat(32), "1".repeat(32));
        assert_ne!(said_turn_id("a", &one, 1), said_turn_id("a", &one, 2));
        assert_ne!(said_turn_id("a", &one, 1), said_turn_id("a", &two, 1), "a counter said again in another life is another turn");
        assert_eq!(said_turn_id("a", &one, 1), said_turn_id("a", &one, 1));
        for bad in ["", "0", &"A".repeat(32), &"g".repeat(32), &"0".repeat(33)] {
            assert!(!valid_life(bad), "{bad}");
        }
    }

    #[test]
    fn bodies_are_the_docs() {
        let c = Cause { fragment: "talk.paul".into(), channel: "chat".into(), seq: 4 };
        let life = "0123456789abcdef0123456789abcdef";
        assert_eq!(turn_start("t", "id:p", "id:a", &c, life), json!({ "kind": "turn.start", "turn": "t", "asker": "id:p", "agent": "id:a", "cause": { "fragment": "talk.paul", "channel": "chat", "seq": 4 }, "life": life }));
        let s = Step { tool: "terminal".into(), args: "ls".into(), ok: true, excerpt: String::new(), text: String::new() };
        assert_eq!(turn_step("t", 1, &s), json!({ "kind": "turn.step", "turn": "t", "step": 1, "tool": "terminal", "args": "ls", "ok": true, "excerpt": "" }));
        assert_eq!(turn_end("t", &Outcome::Error("x".repeat(400))).get("error").and_then(Value::as_str).map(|e| e.chars().count()), Some(limits::ERROR_MAX_CHARS));
        assert_eq!(turn_prompt_closed("t", "p", Closed::Answered, Some(("once", "id:p"))), json!({ "kind": "turn.prompt.closed", "turn": "t", "prompt": "p", "outcome": "answered", "option": "once", "by": "id:p" }));
        assert_eq!(turn_prompt_closed("t", "p", Closed::Expired, None)["outcome"], "expired");
        assert_eq!(reply("hi", "t", &[], 0), json!({ "text": "hi", "turn": "t" }));
        assert_eq!(reply("hi @b", "t", &["id:b".to_string()], 1), json!({ "text": "hi @b", "turn": "t", "to": ["id:b"], "hop": 1 }));
    }

    #[test]
    #[should_panic(expected = "an answered prompt names its answer")]
    fn an_answer_names_its_option() {
        turn_prompt_closed("t", "p", Closed::Answered, None);
    }

    #[test]
    fn cuts() {
        assert_eq!(cut("abcdefg", 6), "abcde…");
        assert_eq!(cut_bytes("abc", 10), "abc");
        let long = "é".repeat(100);
        let c = cut_bytes(&long, 21);
        assert!(c.len() <= 21 && c.ends_with('…'), "{c}");
    }
}
