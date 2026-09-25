//! A chat turn's progress (docs/api.md, the chat template): the records an
//! agent posts to a chat's `work` channel while a turn started in that chat
//! runs, what the chat's own records say to the agent, and the ids that
//! make each record once. The agent reads its turn from its own SQL and
//! builds the records here (agent/src/progress.rs); the platform's chat
//! page renders them (cell/chat.mjs).
//!
//! A turn posts `turn.start`, then one `turn.step` per tool call once its
//! result is stored (the model's text before the call rides on the call's
//! first record), then `turn.end`. Its answer goes to the chat's own
//! channel as `{text, turn}`, so a page places the steps above it. Nothing
//! streams: each record is a whole step, as it was stored.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// The chat's channel for progress records: `viewer` reads, `editor` posts.
pub const WORK_CHANNEL: &str = "work";
/// A tool call's arguments, as one line of JSON, at most this many characters.
pub const ARGS_MAX_CHARS: usize = 140;
/// A tool's result, and the model's text between calls, at most this many
/// characters each: a result can hold what the asker reaches elsewhere.
pub const EXCERPT_MAX_CHARS: usize = 300;
/// A failed turn's error, at most this many characters.
pub const ERROR_MAX_CHARS: usize = 300;

/// A turn's id in its records: from its first message's id, so every
/// driver of the turn (one that replaces a dead one too) names it alike.
pub fn turn_id(kickoff_message_id: &str) -> String {
    hex::encode(Sha256::digest(kickoff_message_id.as_bytes()))[..24].to_string()
}

/// A progress record's post id (`[A-Za-z0-9._:-]`): its turn and its part
/// (`start`, a call's number, `end`), so a replayed step posts the same id
/// with the same body, and the platform appends nothing new.
pub fn record_id(turn: &str, part: &str) -> String {
    format!("wk:{turn}:{part}")
}

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

/// A tool call's arguments as one line of JSON, cut to `ARGS_MAX_CHARS`
/// (empty when it has none).
pub fn short_args(args: Option<&Map<String, Value>>) -> String {
    match args {
        None => String::new(),
        Some(a) if a.is_empty() => String::new(),
        Some(a) => cut(&Value::Object(a.clone()).to_string(), ARGS_MAX_CHARS),
    }
}

/// One tool call of a turn, as its records show it.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    /// `short_args` of its arguments.
    pub args: String,
    /// The model's text in the message that asked for it (the first call
    /// of a message carries it; the others none).
    pub text: String,
    /// Its result, once stored.
    pub result: Option<Outcome>,
}

/// A tool call's result: whether it worked, and its text.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    pub text: String,
}

/// `turn.start`: who asked (the only one who may stop it, or steer it).
pub fn start(turn: &str, asker: &str) -> Value {
    json!({ "kind": "turn.start", "turn": turn, "asker": asker })
}

/// `turn.step`: call number `n` (from 1) of the turn, with its result.
pub fn step(turn: &str, n: usize, call: &Call, result: &Outcome) -> Value {
    assert!(n >= 1, "calls number from 1");
    let mut body = json!({
        "kind": "turn.step",
        "turn": turn,
        "step": n,
        "tool": call.name,
        "args": call.args,
        "ok": result.ok,
        "excerpt": cut(&result.text, EXCERPT_MAX_CHARS),
    });
    let text = cut(&call.text, EXCERPT_MAX_CHARS);
    if !text.is_empty() {
        body["text"] = json!(text);
    }
    body
}

/// `turn.end`: how it ended (`idle`: answered, `stopped`, `yielded`,
/// `error`), and a failure's error, cut short.
pub fn end(turn: &str, outcome: &str, error: Option<&str>) -> Value {
    let mut body = json!({ "kind": "turn.end", "turn": turn, "outcome": outcome });
    if let Some(e) = error.map(|e| cut(e, ERROR_MAX_CHARS)).filter(|e| !e.is_empty()) {
        body["error"] = json!(e);
    }
    body
}

/// A turn's answer on the chat's own channel: its text, and its turn.
pub fn answer(text: &str, turn: Option<&str>) -> Value {
    match turn {
        Some(t) => json!({ "text": text, "turn": t }),
        None => json!({ "text": text }),
    }
}

/// What a record on a chat's channel says to its agent. A body without a
/// `kind` (or `kind: "message"`) is a message: its `text`, or else its
/// JSON; `kind: "stop"` asks to stop a turn (the page's Stop button); any
/// other kind is for the page alone, never a message.
#[derive(Debug, Clone, PartialEq)]
pub enum Said {
    Message(String),
    /// A stop, of the turn it names (or of the running one).
    Stop { turn: Option<String> },
    Other,
}

/// Reads a chat record's body (its stored JSON text).
pub fn said(body: &str) -> Said {
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(body) else { return Said::Message(body.to_string()) };
    match o.get("kind") {
        None => {}
        Some(Value::String(k)) if k == "message" => {}
        Some(Value::String(k)) if k == "stop" => {
            return Said::Stop { turn: o.get("turn").and_then(Value::as_str).map(str::to_string) };
        }
        Some(_) => return Said::Other,
    }
    match o.get("text") {
        Some(Value::String(t)) => Said::Message(t.clone()),
        _ => Said::Message(body.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let t = turn_id("msg_0b6e7e3c-1d2f-4c55-9d0e-4f7b1d2a3c4e");
        assert_eq!(t.len(), 24);
        assert_eq!(t, turn_id("msg_0b6e7e3c-1d2f-4c55-9d0e-4f7b1d2a3c4e"), "the same turn, the same id");
        assert_ne!(t, turn_id("msg_other"));
        for part in ["start", "1", "64", "end"] {
            let id = record_id(&t, part);
            assert!(fragment_proto::valid_op_id(&id), "{id} is a post id");
        }
        assert_ne!(record_id(&t, "1"), record_id(&t, "2"));
    }

    #[test]
    fn cuts() {
        assert_eq!(cut("  short  ", 10), "short");
        assert_eq!(cut("abcdef", 6), "abcdef");
        assert_eq!(cut("abcdefg", 6), "abcde…");
        assert_eq!(cut("abcdefg", 6).chars().count(), 6);
        // whole characters, never a split one
        let long = "é".repeat(400);
        let c = cut(&long, EXCERPT_MAX_CHARS);
        assert_eq!(c.chars().count(), EXCERPT_MAX_CHARS);
        assert!(c.ends_with('…'));
        assert_eq!(cut("", 1), "");
    }

    #[test]
    fn args() {
        assert_eq!(short_args(None), "");
        assert_eq!(short_args(Some(&Map::new())), "");
        let small = json!({ "text": "milk" });
        assert_eq!(short_args(small.as_object()), r#"{"text":"milk"}"#);
        let big = json!({ "text": "x".repeat(500) });
        let s = short_args(big.as_object());
        assert_eq!(s.chars().count(), ARGS_MAX_CHARS);
        assert!(s.starts_with(r#"{"text":"xxx"#) && s.ends_with('…'));
    }

    #[test]
    fn records() {
        let call = Call { name: "todo--paul__add".into(), args: r#"{"text":"milk"}"#.into(), text: "Adding it.".into(), result: None };
        let ok = Outcome { ok: true, text: "y".repeat(1000) };
        let s = step("t1", 1, &call, &ok);
        assert_eq!(s["kind"], "turn.step");
        assert_eq!(s["step"], 1);
        assert_eq!(s["tool"], "todo--paul__add");
        assert_eq!(s["ok"], true);
        assert_eq!(s["text"], "Adding it.");
        assert_eq!(s["excerpt"].as_str().unwrap().chars().count(), EXCERPT_MAX_CHARS, "a result is an excerpt");
        let quiet = Call { text: String::new(), ..call };
        let failed = step("t1", 2, &quiet, &Outcome { ok: false, text: "403: no".into() });
        assert!(failed.get("text").is_none(), "no text between calls, no text field");
        assert_eq!(failed["ok"], false);
        assert_eq!(start("t1", "id:abc"), json!({ "kind": "turn.start", "turn": "t1", "asker": "id:abc" }));
        assert_eq!(end("t1", "idle", None), json!({ "kind": "turn.end", "turn": "t1", "outcome": "idle" }));
        assert_eq!(end("t1", "error", Some(&"e".repeat(900)))["error"].as_str().unwrap().chars().count(), ERROR_MAX_CHARS);
        assert_eq!(end("t1", "error", Some("")), json!({ "kind": "turn.end", "turn": "t1", "outcome": "error" }));
        assert_eq!(answer("hi", Some("t1")), json!({ "text": "hi", "turn": "t1" }));
        assert_eq!(answer("hi", None), json!({ "text": "hi" }));
    }

    #[test]
    fn what_a_chat_record_says() {
        assert_eq!(said(r#"{"text":"hi"}"#), Said::Message("hi".into()));
        assert_eq!(said(r#"{"kind":"message","text":"hi"}"#), Said::Message("hi".into()));
        assert_eq!(said(r#"{"note":1}"#), Said::Message(r#"{"note":1}"#.into()), "a body without text is heard as its JSON");
        assert_eq!(said(r#""plain""#), Said::Message(r#""plain""#.into()));
        assert_eq!(said(r#"{"kind":"stop","turn":"t1"}"#), Said::Stop { turn: Some("t1".into()) });
        assert_eq!(said(r#"{"kind":"stop"}"#), Said::Stop { turn: None });
        assert_eq!(said(r#"{"kind":"typing","text":"not a message"}"#), Said::Other, "another kind is never a message");
        assert_eq!(said(r#"{"kind":7,"text":"x"}"#), Said::Other);
    }
}
