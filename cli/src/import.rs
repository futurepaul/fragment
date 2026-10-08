//! `fragment mind import`: other agents' chats played into a mind
//! (docs/optchat.md, "Importing chats"; OptChat's spec §10). Each format's
//! parser reads its files into conversations of the person's words and the
//! agent's final replies: tool calls and their results, system and meta
//! records, the agent's narration between tool calls, repeated pastes and
//! empty turns are dropped, and a message is capped at the mind's CAP, its
//! head and tail kept. The conversations go to the mind oldest first, each
//! in parts of the `import` mutation, which a rerun resumes where the mind
//! says it stopped (`imported`).
//!
//! Formats:
//! - `claude-code`: Claude Code's sessions, `~/.claude/projects/*/<session>.jsonl`;
//! - `claude-export`: claude.ai's data export, `conversations.json`;
//! - `codex`: Codex's rollouts, `~/.codex/sessions/**/rollout-*.jsonl`
//!   (each generation of the format: its first, with no record types; its
//!   `response_item`s and `event_msg`s; and its turn items);
//! - `hermes`: Hermes Agent's `hermes sessions export` lines, and its
//!   older `sessions/session_<id>.json` snapshots and `sessions/<id>.jsonl`
//!   transcripts. Its `state.db` is SQLite, which this CLI does not read:
//!   export it first.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// A message's most characters, as the mind caps one at logging
/// (templates/mind/applib/optmem.mjs `CAP`, the spec's).
pub const CAP: usize = 30_000;
/// A summary line's target size (the spec's NODE): a message whose
/// `kind: text` fits is its own line, with no model call.
pub const NODE: usize = 512;
/// A user message at least this long, the same as an earlier one, is a
/// repeated paste.
const PASTE_MIN_BYTES: usize = 1024;
/// The room a capped text's note of what was cut takes (optmem.mjs).
const CAP_NOTE_ROOM: usize = 80;
/// One part of a conversation, as the `import` mutation takes it: at most
/// this many messages, in at most this much JSON (an operation's input is
/// at most 256 KiB).
pub const PART_MESSAGES_MAX: usize = 64;
pub const PART_MAX_BYTES: usize = 192 * 1024;
/// A conversation's id and title, at most (the `import` operation's schema).
const ID_MAX_CHARS: usize = 200;
const TITLE_MAX_CHARS: usize = 200;
/// A line of a file this reads: longer ones are skipped (a Codex rollout's
/// lines can be a few MiB; nothing in a chat needs more).
const LINE_MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum Source {
    #[serde(rename = "claude-code")]
    ClaudeCode,
    #[serde(rename = "claude-export")]
    ClaudeExport,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "hermes")]
    Hermes,
}

impl Source {
    pub const ALL: [Source; 4] = [Source::ClaudeCode, Source::ClaudeExport, Source::Codex, Source::Hermes];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::ClaudeCode => "claude-code",
            Source::ClaudeExport => "claude-export",
            Source::Codex => "codex",
            Source::Hermes => "hermes",
        }
    }

    pub fn parse(s: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    /// The kind the mind logs it as.
    pub fn kind(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "talk",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
    /// When it was said, in ms since the epoch.
    pub at: i64,
    /// The record's own id, where the format has one: a record copied into
    /// another session (a resumed one) is the first one's.
    #[serde(skip)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    pub source: Source,
    pub id: String,
    pub title: Option<String>,
    /// When it began, in ms since the epoch: its first message's time.
    pub started: i64,
    pub messages: Vec<Message>,
}

// ---------- times ----------

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (i64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Milliseconds since the epoch of an ISO 8601 time,
/// `YYYY-MM-DD[(T| )HH:MM[:SS[.frac]]][Z|±HH[:]MM]`. One with no zone (Hermes's
/// older files write local time so) is read as UTC.
pub fn parse_time(s: &str) -> Option<i64> {
    let b = s.trim().as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let digits = b.get(from..from + len)?;
        digits.iter().all(u8::is_ascii_digit).then(|| digits.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let mut ms = days_from_civil(y, mo as u32, d as u32) * 86_400_000;
    let mut k = 10;
    if matches!(b.get(k), Some(b'T' | b't' | b' ')) {
        let (h, mi) = (num(k + 1, 2)?, num(k + 4, 2)?);
        if b.get(k + 3) != Some(&b':') || h > 23 || mi > 59 {
            return None;
        }
        ms += (h * 60 + mi) * 60_000;
        k += 6;
        if b.get(k) == Some(&b':') {
            let sec = num(k + 1, 2)?;
            if sec > 60 {
                return None;
            }
            ms += sec * 1000;
            k += 3;
            if matches!(b.get(k), Some(b'.' | b',')) {
                k += 1;
                let start = k;
                while b.get(k).is_some_and(u8::is_ascii_digit) {
                    k += 1;
                }
                let frac = &b[start..k];
                if frac.is_empty() {
                    return None;
                }
                let mut f = 0i64;
                for i in 0..3 {
                    f = f * 10 + frac.get(i).map_or(0, |d| i64::from(d - b'0'));
                }
                ms += f;
            }
        }
        match b.get(k) {
            None => {}
            Some(b'Z' | b'z') if k + 1 == b.len() => {}
            Some(sign @ (b'+' | b'-')) => {
                let oh = num(k + 1, 2)?;
                let om = if b.get(k + 3) == Some(&b':') { num(k + 4, 2)? } else { num(k + 3, 2)? };
                let end = if b.get(k + 3) == Some(&b':') { k + 6 } else { k + 5 };
                if end != b.len() || oh > 23 || om > 59 {
                    return None;
                }
                let off = (oh * 60 + om) * 60_000;
                ms += if *sign == b'+' { -off } else { off };
            }
            _ => return None,
        }
    } else if k != b.len() {
        return None;
    }
    Some(ms)
}

/// A time a format writes: an ISO 8601 string, or a number of seconds
/// (Hermes's `time.time()`) or milliseconds since the epoch.
fn time_of(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => parse_time(s),
        Value::Number(n) => {
            let x = n.as_f64()?;
            if !x.is_finite() || x <= 0.0 {
                return None;
            }
            // seconds until the year 5138; milliseconds after 2001-09-09
            Some(if x < 1e11 { (x * 1000.0).round() as i64 } else { x.round() as i64 })
        }
        _ => None,
    }
}

// ---------- texts ----------

/// At most `max` characters of `text`, its head and tail kept with a note
/// of what was cut between them: the mind's `capText`, so a message the
/// CLI capped is the one the mind logs.
pub fn cap_text(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    assert!(max > CAP_NOTE_ROOM * 2, "a cap leaves room for its note");
    let room = max - CAP_NOTE_ROOM;
    let head = room.div_ceil(2);
    let tail = room - head;
    let cut = n - head - tail;
    let start: String = text.chars().take(head).collect();
    let end: String = text.chars().skip(n - tail).collect();
    format!("{start}\n\n[… {cut} characters cut here …]\n\n{end}")
}

/// `text` without each `<tag>…</tag>` block (and an unclosed one's opening
/// to the end).
fn strip_blocks(text: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&open) {
        out.push_str(&rest[..at]);
        match rest[at..].find(&close) {
            Some(end) => rest = &rest[at + end + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text between `<tag>` and `</tag>`, the first such.
fn between<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{tag}>"))?;
    Some(&text[start..start + end])
}

/// Whether a text is one block of markup a harness wrapped around what it
/// told the model (`<environment_context>…</environment_context>`), not
/// words a person typed.
fn wrapped(text: &str) -> bool {
    let t = text.trim();
    let Some(rest) = t.strip_prefix('<') else { return false };
    let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).collect();
    !name.is_empty() && name.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && t.ends_with('>')
}

/// The texts of a content list's parts of these types (`text`, or one
/// given), each trimmed, joined by a blank line.
fn parts_text(content: &Value, types: &[&str]) -> String {
    match content {
        Value::String(s) => s.trim().to_string(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"].as_str().is_some_and(|t| types.contains(&t)))
            .filter_map(|p| p["text"].as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

// ---------- turns ----------

/// A turn's last words, while it runs: the agent's final reply is its last
/// text after its last tool call.
struct Reply {
    text: String,
    at: i64,
    group: Option<String>,
    /// A tool call came after it: it was narration, not the reply.
    stale: bool,
}

/// Messages as a format's records arrive: the person's words, and of each
/// turn's agent output only its final reply.
#[derive(Default)]
struct Turns {
    out: Vec<Message>,
    reply: Option<Reply>,
}

impl Turns {
    fn user(&mut self, text: String, at: i64, key: Option<String>) {
        self.flush();
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        // the same words again at once: a queued prompt recorded twice
        if self.out.last().is_some_and(|m| m.role == Role::User && m.text == text) {
            return;
        }
        self.out.push(Message { role: Role::User, text, at, key });
    }

    /// The agent's text; `group` joins the texts of one model message.
    fn said(&mut self, text: &str, at: i64, group: Option<&str>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        match &mut self.reply {
            Some(r) if !r.stale && group.is_some() && r.group.as_deref() == group => {
                r.text.push_str("\n\n");
                r.text.push_str(text);
            }
            _ => self.reply = Some(Reply { text: text.to_string(), at, group: group.map(str::to_string), stale: false }),
        }
    }

    /// The turn's reply when it has none (Codex's `task_complete`).
    fn said_if_none(&mut self, text: &str, at: i64) {
        if self.reply.as_ref().is_none_or(|r| r.stale) {
            self.said(text, at, None);
        }
    }

    fn tool(&mut self) {
        if let Some(r) = &mut self.reply {
            r.stale = true;
        }
    }

    fn flush(&mut self) {
        if let Some(r) = self.reply.take().filter(|r| !r.stale) {
            self.out.push(Message { role: Role::Assistant, text: r.text, at: r.at, key: None });
        }
    }

    fn finish(mut self) -> Vec<Message> {
        self.flush();
        self.out
    }
}

/// A conversation of these messages, or none when it has no words of the
/// person's (a run with only metadata, or a subagent's).
fn conversation(source: Source, id: &str, title: Option<String>, started: Option<i64>, messages: Vec<Message>) -> Option<Conversation> {
    if !messages.iter().any(|m| m.role == Role::User) {
        return None;
    }
    let first = messages.first().map(|m| m.at);
    let started = match (started, first) {
        (Some(s), Some(f)) => s.min(f),
        (s, f) => s.or(f)?,
    };
    let title = title.map(|t| t.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|t| !t.is_empty()).map(|t| t.chars().take(TITLE_MAX_CHARS).collect());
    Some(Conversation { source, id: id.chars().take(ID_MAX_CHARS).collect(), title, started, messages })
}

// ---------- Claude Code ----------

/// A person's words in a Claude Code user record, or none for what the
/// harness wrote there: slash commands with no arguments, their output,
/// interruptions, a shell's echo, task notifications, system reminders.
fn claude_code_user_text(text: &str) -> Option<String> {
    let text = strip_blocks(text, "system-reminder");
    let t = text.trim();
    if t.contains("<command-name>") {
        let name = between(t, "command-name").unwrap_or("").trim();
        let args = between(t, "command-args").unwrap_or("").trim();
        return (!args.is_empty()).then(|| format!("{name} {args}"));
    }
    const HARNESS: [&str; 11] = [
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<local-command-caveat>",
        "<command-message>",
        "Caveat: The messages below were generated",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<task-notification>",
        "[Request interrupted",
        "This session is being continued from a previous conversation",
    ];
    if t.is_empty() || HARNESS.iter().any(|h| t.starts_with(h)) {
        return None;
    }
    Some(t.to_string())
}

/// Whether a record's `origin` is a person's (or names none, as older
/// records do).
fn human_origin(r: &Value) -> bool {
    match &r["origin"] {
        Value::Null => true,
        o => o["kind"] == "human",
    }
}

/// A Claude Code session as its records arrive.
#[derive(Default)]
struct Session {
    turns: Turns,
    title: Option<String>,
    summary: Option<String>,
}

/// One Claude Code session file (`<session>.jsonl`, `session` its name): a
/// conversation for each session its records name. A session forked or
/// resumed from another starts with that one's records copied in, under its
/// id: they are that conversation's, merged with its other copies
/// (`prepare`).
pub fn claude_code(session: &str, lines: impl Iterator<Item = Value>) -> Vec<Conversation> {
    let mut sessions: BTreeMap<String, Session> = BTreeMap::new();
    for r in lines {
        if r["isSidechain"] == true {
            continue;
        }
        let s = sessions.entry(r["sessionId"].as_str().unwrap_or(session).to_string()).or_default();
        let turns = &mut s.turns;
        let at = time_of(&r["timestamp"]);
        match r["type"].as_str().unwrap_or("") {
            "custom-title" => s.title = r["customTitle"].as_str().map(str::to_string).or(s.title.take()),
            "summary" if s.summary.is_none() => s.summary = r["summary"].as_str().map(str::to_string),
            "user" => {
                let content = &r["message"]["content"];
                if r["isMeta"] == true || r["isVisibleInTranscriptOnly"] == true || !r["toolUseResult"].is_null() || content.as_array().is_some_and(|parts| parts.iter().any(|p| p["type"] == "tool_result")) {
                    continue;
                }
                let raw = parts_text(content, &["text"]);
                // a turn the harness started (a subagent's or a task's report, a
                // peer's message, a compaction): its words are not the person's,
                // but the agent's reply to it is a turn's
                if r["isCompactSummary"] == true || !human_origin(&r) || raw.trim_start().starts_with("<task-notification>") {
                    turns.flush();
                    continue;
                }
                let (Some(at), Some(text)) = (at, claude_code_user_text(&raw)) else { continue };
                turns.user(text, at, r["uuid"].as_str().map(str::to_string));
            }
            "assistant" => {
                let m = &r["message"];
                if r["isApiErrorMessage"] == true || m["model"] == "<synthetic>" {
                    continue;
                }
                let (Some(at), Some(parts)) = (at, m["content"].as_array()) else { continue };
                let group = m["id"].as_str();
                for p in parts {
                    match p["type"].as_str().unwrap_or("") {
                        "text" => turns.said(p["text"].as_str().unwrap_or(""), at, group),
                        "tool_use" | "server_tool_use" | "mcp_tool_use" => turns.tool(),
                        _ => {}
                    }
                }
            }
            // a prompt typed while the agent worked, delivered between its tool calls
            "attachment" => {
                let a = &r["attachment"];
                if a["type"] != "queued_command" || a["commandMode"] != "prompt" || a["isMeta"] == true || r["isMeta"] == true || !human_origin(a) {
                    continue;
                }
                let (Some(at), Some(text)) = (at.or_else(|| time_of(&a["timestamp"])), claude_code_user_text(&parts_text(&a["prompt"], &["text"]))) else { continue };
                turns.user(text, at, a["source_uuid"].as_str().map(|u| format!("q:{u}")));
            }
            _ => {}
        }
    }
    sessions.into_iter().filter_map(|(id, s)| conversation(Source::ClaudeCode, &id, s.title.or(s.summary), None, s.turns.finish())).collect()
}

// ---------- claude.ai's export ----------

/// claude.ai's `conversations.json`: every conversation of the account.
pub fn claude_export(v: &Value) -> Vec<Conversation> {
    let mut out = vec![];
    for c in v.as_array().into_iter().flatten() {
        let Some(id) = c["uuid"].as_str() else { continue };
        let mut turns = Turns::default();
        for m in c["chat_messages"].as_array().into_iter().flatten() {
            let Some(at) = time_of(&m["created_at"]) else { continue };
            // the message's parts, when it has them (tool use, thinking), else its text
            let text = match &m["content"] {
                Value::Array(parts) if !parts.is_empty() => parts_text(&m["content"], &["text"]),
                _ => m["text"].as_str().unwrap_or("").trim().to_string(),
            };
            match m["sender"].as_str() {
                Some("human") => turns.user(text, at, m["uuid"].as_str().map(str::to_string)),
                Some("assistant") => {
                    turns.said(&text, at, None);
                    turns.flush();
                }
                _ => {}
            }
        }
        out.extend(conversation(Source::ClaudeExport, id, c["name"].as_str().map(str::to_string), time_of(&c["created_at"]), turns.finish()));
    }
    out
}

// ---------- Codex ----------

/// A person's words in a Codex user message, or none for what the harness
/// put there (its environment, AGENTS.md, a browser's state, an abort).
fn codex_user_text(text: &str) -> Option<String> {
    // Codex Desktop puts the files and pages a person points at above their words
    let t = match text.find("## My request for Codex:") {
        Some(at) => &text[at + "## My request for Codex:".len()..],
        None => text,
    };
    let t = t.trim();
    if t.is_empty() || wrapped(t) || t.starts_with("# AGENTS.md instructions") || t.starts_with("<environment_context>") || t.starts_with("<user_instructions>") {
        return None;
    }
    Some(t.to_string())
}

/// One Codex rollout. A subagent's (a thread another spawned, or a guardian)
/// is none: its words are an agent's.
pub fn codex(file_id: &str, lines: impl Iterator<Item = Value>) -> Option<Conversation> {
    let lines: Vec<Value> = lines.collect();
    let first = lines.first()?;
    let (meta, old) = if first["type"] == "session_meta" { (&first["payload"], false) } else { (first, first.get("type").is_none()) };
    if meta["source"].get("subagent").is_some() || meta["thread_source"].get("subagent").is_some() {
        return None;
    }
    let id = meta["id"].as_str().unwrap_or(file_id).to_string();
    let started = time_of(&meta["timestamp"]);
    let items = lines.iter().any(|r| r["type"] == "event_msg" && r["payload"]["type"] == "item_completed" && matches!(r["payload"]["item"]["type"].as_str(), Some("UserMessage" | "AgentMessage")));
    let events = lines.iter().any(|r| r["type"] == "event_msg" && matches!(r["payload"]["type"].as_str(), Some("user_message" | "agent_message")));
    let mut turns = Turns::default();
    // the old format's records carry no time: the session's, a ms apart, keeps their order
    let mut tick = started.unwrap_or(0);
    for r in &lines {
        tick += 1;
        let at = time_of(&r["timestamp"]).unwrap_or(tick);
        if items {
            codex_item(&mut turns, r, &id, at);
        } else if events {
            codex_event(&mut turns, r, at);
        } else if old {
            codex_response(&mut turns, r, at);
        } else if r["type"] == "response_item" {
            codex_response(&mut turns, &r["payload"], at);
        }
    }
    conversation(Source::Codex, &id, None, started, turns.finish())
}

/// A turn item (`event_msg` `item_completed`), the newest format's.
fn codex_item(turns: &mut Turns, r: &Value, thread: &str, at: i64) {
    let p = &r["payload"];
    // a fork's or a parent's items copied in are theirs
    if r["type"] != "event_msg" || p["type"] != "item_completed" || p["thread_id"].as_str().is_some_and(|t| t != thread) {
        return;
    }
    let item = &p["item"];
    let at = time_of(&p["completed_at_ms"]).unwrap_or(at);
    match item["type"].as_str().unwrap_or("") {
        "UserMessage" => {
            if let Some(text) = codex_user_text(&parts_text(&item["content"], &["text"])) {
                turns.user(text, at, None);
            }
        }
        // narration between tool calls is `commentary`
        "AgentMessage" => {
            if matches!(item["phase"].as_str(), Some("final_answer") | None) {
                turns.said(&parts_text(&item["content"], &["Text", "text"]), at, None);
            }
        }
        "Reasoning" | "ContextCompaction" => {}
        _ => turns.tool(),
    }
}

/// An event (`event_msg`), the middle format's.
fn codex_event(turns: &mut Turns, r: &Value, at: i64) {
    let p = &r["payload"];
    if r["type"] != "event_msg" {
        return;
    }
    match p["type"].as_str().unwrap_or("") {
        "user_message" => {
            if let Some(text) = codex_user_text(p["message"].as_str().unwrap_or("")) {
                turns.user(text, at, None);
            }
        }
        "agent_message" => turns.said(p["message"].as_str().unwrap_or(""), at, None),
        "task_complete" => turns.said_if_none(p["last_agent_message"].as_str().unwrap_or(""), at),
        t if t.ends_with("_begin") || t.ends_with("_end") => turns.tool(),
        _ => {}
    }
}

/// A response item: the model's input and output as the API saw them (the
/// first format's records are these alone).
fn codex_response(turns: &mut Turns, p: &Value, at: i64) {
    match p["type"].as_str().unwrap_or("") {
        "message" => match p["role"].as_str() {
            Some("user") => {
                let words: Vec<String> = p["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["type"] == "input_text")
                    .filter_map(|c| codex_user_text(c["text"].as_str().unwrap_or("")))
                    .collect();
                if !words.is_empty() {
                    turns.user(words.join("\n\n"), at, None);
                }
            }
            Some("assistant") => turns.said(&parts_text(&p["content"], &["output_text"]), at, None),
            _ => {}
        },
        "function_call" | "custom_tool_call" | "local_shell_call" | "web_search_call" | "tool_search_call" | "image_generation_call" => turns.tool(),
        _ => {}
    }
}

// ---------- Hermes ----------

/// A person's words in a Hermes user row, or none for what Hermes wrote
/// there (compaction summaries, system notes, a skill's scaffold without
/// the person's instruction).
fn hermes_user_text(text: &str) -> Option<String> {
    let text = strip_blocks(text, "memory-context");
    let t = text.trim();
    const SKILL: &str = "The user has provided the following instruction alongside the skill invocation: ";
    if t.starts_with("[IMPORTANT: The ") {
        return t.find(SKILL).map(|at| t[at + SKILL.len()..].trim().to_string()).filter(|s| !s.is_empty());
    }
    const MACHINE: [&str; 9] = [
        "[CONTEXT COMPACTION",
        "[CONTEXT SUMMARY]",
        "[PRIOR CONTEXT",
        "[System note:",
        "[Runtime note:",
        "[SYSTEM]",
        "[SYSTEM:",
        "[System: ",
        "[END OF PRIOR CONTEXT",
    ];
    if t.is_empty() || MACHINE.iter().any(|m| t.starts_with(m)) {
        return None;
    }
    Some(t.to_string())
}

/// A flag as Hermes writes one: an integer (SQLite's) or a boolean.
fn truthy(v: &Value) -> bool {
    v.as_bool().unwrap_or(false) || v.as_i64().is_some_and(|n| n != 0)
}

/// A Hermes message's content: a string, or `\0json:` and parts.
fn hermes_content(v: &Value) -> String {
    match v {
        Value::String(s) => match s.strip_prefix("\u{0}json:") {
            Some(j) => serde_json::from_str::<Value>(j).map(|p| parts_text(&p, &["text"])).unwrap_or_default(),
            None => s.clone(),
        },
        other => parts_text(other, &["text"]),
    }
}

/// One session's messages into `turns`, in their order; `at` is a
/// message's time when it has none of its own.
fn hermes_messages(turns: &mut Turns, messages: &[Value], mut at: impl FnMut(&Value) -> i64, seen: &mut HashSet<String>) {
    for m in messages {
        // undone or rewound rows are not what was said; compacted ones were
        let active = m.get("active").is_none_or(truthy) || truthy(&m["compacted"]);
        let metadata = match &m["display_metadata"] {
            Value::String(d) => serde_json::from_str(d).unwrap_or(Value::Null),
            other => other.clone(),
        };
        if !active || truthy(&m["_compressed_summary"]) || metadata["model_only"] == true || !m["display_kind"].is_null() {
            continue;
        }
        if let Some(uid) = m["message_uid"].as_str() {
            if !seen.insert(uid.to_string()) {
                continue;
            }
        }
        let t = at(m);
        match m["role"].as_str().unwrap_or("") {
            "user" => {
                if let Some(text) = hermes_user_text(&hermes_content(&m["content"])) {
                    turns.user(text, t, m["message_uid"].as_str().map(str::to_string));
                }
            }
            "assistant" => {
                let text = strip_blocks(&hermes_content(&m["content"]), "think");
                turns.said(&text, t, None);
                if m["tool_calls"].as_array().is_some_and(|c| !c.is_empty()) || m["tool_calls"].as_str().is_some_and(|c| c.trim_start().starts_with("[{")) {
                    turns.tool();
                }
            }
            "tool" => turns.tool(),
            _ => {}
        }
    }
}

/// Sessions that are an agent's, not a person's chat.
const HERMES_AGENT_SOURCES: [&str; 5] = ["subagent", "delegate", "tool", "cron", "acp"];

/// The conversation a session belongs to: its compaction chain's first
/// session (a cycle, which Hermes never writes, stops at 1000 hops).
fn hermes_root<'a>(by_id: &HashMap<&str, &'a Value>, mut s: &'a Value) -> String {
    for _ in 0..1000 {
        match s["parent_session_id"].as_str().and_then(|p| by_id.get(p)) {
            Some(p) if p["end_reason"] == "compression" => s = p,
            _ => break,
        }
    }
    s["id"].as_str().unwrap_or("").to_string()
}

/// `hermes sessions export` lines: one session each. A session that
/// continues one compaction ended is the same conversation.
pub fn hermes_export(sessions: &[Value]) -> Vec<Conversation> {
    let by_id: HashMap<&str, &Value> = sessions.iter().filter_map(|s| Some((s["id"].as_str()?, s))).collect();
    let mut chains: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for s in sessions {
        if s["id"].as_str().is_none() || HERMES_AGENT_SOURCES.contains(&s["source"].as_str().unwrap_or("")) {
            continue;
        }
        chains.entry(hermes_root(&by_id, s)).or_default().push(s);
    }
    let mut out = vec![];
    for (id, mut chain) in chains {
        chain.sort_by(|a, b| time_of(&a["started_at"]).cmp(&time_of(&b["started_at"])));
        let mut turns = Turns::default();
        let mut seen = HashSet::new();
        let mut title = None;
        for s in &chain {
            title = title.or_else(|| s["title"].as_str().map(str::to_string));
            let start = time_of(&s["started_at"]).unwrap_or(0);
            let messages = s["messages"].as_array().map(Vec::as_slice).unwrap_or_default();
            hermes_messages(&mut turns, messages, |m| time_of(&m["timestamp"]).unwrap_or(start), &mut seen);
        }
        out.extend(conversation(Source::Hermes, &id, title, chain.first().and_then(|s| time_of(&s["started_at"])), turns.finish()));
    }
    out
}

/// Hermes's older `session_<id>.json` snapshot: its messages carry no time,
/// so each is the session's start, a ms apart.
pub fn hermes_snapshot(v: &Value) -> Option<Conversation> {
    let id = v["session_id"].as_str()?;
    if HERMES_AGENT_SOURCES.contains(&v["platform"].as_str().unwrap_or("")) {
        return None;
    }
    let start = time_of(&v["session_start"])?;
    let mut turns = Turns::default();
    let mut tick = start;
    let messages = v["messages"].as_array().map(Vec::as_slice).unwrap_or_default();
    hermes_messages(
        &mut turns,
        messages,
        |_| {
            tick += 1;
            tick
        },
        &mut HashSet::new(),
    );
    conversation(Source::Hermes, id, None, Some(start), turns.finish())
}

/// Hermes's older gateway transcript, `sessions/<id>.jsonl`.
pub fn hermes_transcript(id: &str, lines: impl Iterator<Item = Value>) -> Option<Conversation> {
    let lines: Vec<Value> = lines.filter(|l| l["role"] != "session_meta").collect();
    let mut turns = Turns::default();
    let mut last = 0;
    hermes_messages(
        &mut turns,
        &lines,
        |m| {
            // a turn's messages share its time: a ms apart keeps their order
            last = time_of(&m["timestamp"]).unwrap_or(last).max(last + 1);
            last
        },
        &mut HashSet::new(),
    );
    conversation(Source::Hermes, id, None, None, turns.finish())
}

// ---------- files ----------

/// What a file holds, read from its start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    ClaudeCode,
    ClaudeExport,
    Codex,
    HermesExport,
    HermesSnapshot,
    HermesTranscript,
    /// SQLite (a Hermes `state.db`): not read here.
    Sqlite,
}

impl Format {
    pub fn source(self) -> Option<Source> {
        match self {
            Format::ClaudeCode => Some(Source::ClaudeCode),
            Format::ClaudeExport => Some(Source::ClaudeExport),
            Format::Codex => Some(Source::Codex),
            Format::HermesExport | Format::HermesSnapshot | Format::HermesTranscript => Some(Source::Hermes),
            Format::Sqlite => None,
        }
    }
}

/// Claude Code's record types (any one of which starts a session file).
const CLAUDE_CODE_TYPES: [&str; 12] =
    ["user", "assistant", "summary", "system", "attachment", "file-history-snapshot", "queue-operation", "custom-title", "agent-name", "last-prompt", "mode", "permission-mode"];

/// A JSONL file's format, from its first record.
fn jsonl_format(first: &Value) -> Option<Format> {
    let t = first["type"].as_str();
    if matches!(t, Some("session_meta" | "response_item" | "event_msg" | "turn_context" | "compacted")) || (t.is_none() && first["id"].is_string() && first.get("instructions").is_some()) {
        return Some(Format::Codex);
    }
    if first["sessionId"].is_string() || t.is_some_and(|t| CLAUDE_CODE_TYPES.contains(&t)) && first.get("role").is_none() {
        return Some(Format::ClaudeCode);
    }
    if first["messages"].is_array() && first["id"].is_string() && first.get("started_at").is_some() {
        return Some(Format::HermesExport);
    }
    if first["role"] == "session_meta" || (first["role"].is_string() && first.get("content").is_some() && first["timestamp"].is_string()) {
        return Some(Format::HermesTranscript);
    }
    None
}

/// A file's format, or none for a file of no format this reads (Codex's
/// prompt history, Claude Code's, anything else).
pub fn detect(path: &Path) -> Result<Option<Format>> {
    let mut f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut head = [0u8; 16];
    let n = f.read(&mut head)?;
    if head[..n].starts_with(b"SQLite format 3\0") {
        return Ok(Some(Format::Sqlite));
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if name.ends_with(".jsonl") {
        let first = lines(path)?.next();
        return Ok(first.as_ref().and_then(jsonl_format));
    }
    if name.ends_with(".json") {
        // the two JSON formats: a whole account's export, a session's snapshot
        if name == "conversations.json" {
            return Ok(Some(Format::ClaudeExport));
        }
        if name.starts_with("session_") {
            return Ok(Some(Format::HermesSnapshot));
        }
    }
    Ok(None)
}

/// A JSONL file's records, each line that is JSON (a line cut by a crash,
/// or past LINE_MAX_BYTES, is skipped).
fn lines(path: &Path) -> Result<impl Iterator<Item = Value>> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut r = BufReader::with_capacity(1 << 20, f);
    let mut buf = Vec::new();
    Ok(std::iter::from_fn(move || loop {
        buf.clear();
        match r.by_ref().take(LINE_MAX_BYTES as u64 + 1).read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {}
        }
        if buf.len() > LINE_MAX_BYTES && buf.last() != Some(&b'\n') {
            // the rest of an overlong line
            let mut rest = Vec::new();
            let _ = r.read_until(b'\n', &mut rest);
            continue;
        }
        if let Ok(v) = serde_json::from_slice::<Value>(&buf) {
            return Some(v);
        }
    }))
}

fn read_json(path: &Path) -> Result<Value> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    serde_json::from_reader(BufReader::with_capacity(1 << 20, f)).with_context(|| format!("{} is not JSON", path.display()))
}

/// The files under `paths`: each file named, and each `.json`, `.jsonl` and `.db`
/// under a folder named (not in a `subagents` folder, whose sessions are a
/// Claude Code agent's own), in name order.
pub fn files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    let mut stack: Vec<PathBuf> = paths.iter().rev().cloned().collect();
    while let Some(p) = stack.pop() {
        let meta = std::fs::metadata(&p).with_context(|| format!("reading {}", p.display()))?;
        if meta.is_file() {
            out.push(p);
            continue;
        }
        if !meta.is_dir() || p.file_name().is_some_and(|n| n == "subagents") {
            continue;
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&p).with_context(|| format!("listing {}", p.display()))?.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for e in entries.into_iter().rev() {
            let is_dir = e.is_dir();
            let named = e.extension().is_some_and(|x| x == "json" || x == "jsonl" || x == "db");
            if is_dir || named {
                stack.push(e);
            }
        }
    }
    Ok(out)
}

/// What reading the files found, per source, and what it skipped.
#[derive(Debug, Default, Serialize)]
pub struct Found {
    pub files: usize,
    /// Files of no format this reads, or of another than `--from`.
    pub skipped_files: usize,
    /// Files with no words of a person's (metadata, a subagent's run).
    pub empty: Vec<String>,
    /// Hermes `state.db` files found (export them first).
    pub sqlite: Vec<String>,
}

/// Every conversation in `paths` of the format `from` (or each file's own).
pub fn read(paths: &[PathBuf], from: Option<Source>) -> Result<(Vec<Conversation>, Found)> {
    let mut found = Found::default();
    let mut out = vec![];
    let mut hermes_sessions = vec![];
    for path in files(paths)? {
        found.files += 1;
        let Some(format) = detect(&path)? else {
            found.skipped_files += 1;
            continue;
        };
        if format == Format::Sqlite {
            found.sqlite.push(path.display().to_string());
            continue;
        }
        if from.is_some_and(|f| Some(f) != format.source()) {
            found.skipped_files += 1;
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let got: Vec<Conversation> = match format {
            Format::ClaudeCode => claude_code(&stem, lines(&path)?),
            Format::Codex => {
                // a rollout's name ends with its thread's id
                let id = if stem.starts_with("rollout-") { stem.get(stem.len().saturating_sub(36)..).unwrap_or(&stem) } else { &stem }.to_string();
                codex(&id, lines(&path)?).into_iter().collect()
            }
            Format::ClaudeExport => claude_export(&read_json(&path)?),
            Format::HermesSnapshot => hermes_snapshot(&read_json(&path)?).into_iter().collect(),
            Format::HermesTranscript => hermes_transcript(&stem, lines(&path)?).into_iter().collect(),
            Format::HermesExport => {
                // a chain of sessions spans lines: read them all, then sort them out
                hermes_sessions.extend(lines(&path)?);
                continue;
            }
            Format::Sqlite => unreachable!("taken above"),
        };
        if got.is_empty() {
            found.empty.push(path.display().to_string());
        }
        out.extend(got);
    }
    if !hermes_sessions.is_empty() {
        out.extend(hermes_export(&hermes_sessions));
    }
    Ok((out, found))
}

/// One conversation of its copies (read from several files, as a Claude
/// Code session copied into each session forked from it): every message any
/// copy holds, once, in time order.
fn merge(copies: Vec<Conversation>) -> Conversation {
    let mut copies = copies.into_iter();
    let mut c = copies.next().expect("a conversation has a copy");
    let mut seen: HashSet<(Role, i64, [u8; 32])> = c.messages.iter().map(|m| (m.role, m.at, Sha256::digest(m.text.as_bytes()).into())).collect();
    for other in copies {
        c.title = c.title.or(other.title);
        c.started = c.started.min(other.started);
        for m in other.messages {
            if seen.insert((m.role, m.at, Sha256::digest(m.text.as_bytes()).into())) {
                c.messages.push(m);
            }
        }
    }
    // stable: a turn's messages that share a time keep their order
    c.messages.sort_by_key(|m| m.at);
    c
}

/// The conversations as they go to the mind: oldest first; the copies of one
/// merged; a record copied into another conversation and a repeated paste
/// dropped; each message capped; those begun before `since` (ms) left out,
/// and then all but the first `limit`.
pub fn prepare(convs: Vec<Conversation>, since: Option<i64>, limit: Option<usize>) -> Vec<Conversation> {
    let mut by_id: BTreeMap<(Source, String), Vec<Conversation>> = BTreeMap::new();
    for c in convs {
        by_id.entry((c.source, c.id.clone())).or_default().push(c);
    }
    let mut convs: Vec<Conversation> = by_id.into_values().map(merge).collect();
    convs.sort_by(|a, b| (a.started, a.source, &a.id).cmp(&(b.started, b.source, &b.id)));
    let mut keys: HashSet<String> = HashSet::new();
    let mut pastes: HashSet<[u8; 32]> = HashSet::new();
    for c in &mut convs {
        c.messages.retain(|m| m.key.as_ref().is_none_or(|k| keys.insert(k.clone())));
        c.messages.retain(|m| {
            if m.role != Role::User || m.text.len() < PASTE_MIN_BYTES {
                return true;
            }
            let words: Vec<&str> = m.text.split_whitespace().collect();
            pastes.insert(Sha256::digest(words.join(" ").as_bytes()).into())
        });
        for m in &mut c.messages {
            m.text = cap_text(&m.text, CAP);
        }
    }
    convs.retain(|c| c.messages.iter().any(|m| m.role == Role::User) && since.is_none_or(|s| c.started >= s));
    if let Some(n) = limit {
        convs.truncate(n);
    }
    convs
}

// ---------- the mind's side ----------

/// One `import` call: messages `from` on of a conversation.
pub fn part_input(c: &Conversation, from: usize, messages: &[Message]) -> Value {
    json!({
        "source": c.source.as_str(),
        "conversation": { "id": c.id, "title": c.title, "started": c.started },
        "from": from,
        "total": c.messages.len(),
        "messages": messages,
    })
}

/// A conversation's messages `from` on, as parts: at most
/// PART_MESSAGES_MAX messages and PART_MAX_BYTES of input each (a message
/// is capped, so one always fits).
pub fn parts(c: &Conversation, from: usize) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut start = from;
    while start < c.messages.len() {
        let mut end = start;
        let mut bytes = 512 + c.id.len() + c.title.as_deref().map_or(0, str::len);
        while end < c.messages.len() && end - start < PART_MESSAGES_MAX {
            let size = serde_json::to_string(&c.messages[end]).map_or(0, |s| s.len()) + 1;
            if end > start && bytes + size > PART_MAX_BYTES {
                break;
            }
            bytes += size;
            end += 1;
        }
        out.push((start, end));
        start = end;
    }
    out
}

// ---------- what it costs ----------

/// What compacting an import costs, roughly (`estimate`).
#[derive(Debug, Default, Serialize)]
pub struct Estimate {
    pub messages: usize,
    pub bytes: usize,
    /// Messages whose line is over NODE: each needs a model call.
    pub level0_calls: usize,
    /// Level-0 calls when consecutive ones go 8 to a call (the batched path).
    pub level0_batches: usize,
    /// Merges that need a model call, and those that are free.
    pub merge_calls: usize,
    pub merges_free: usize,
    pub merge_batches: usize,
    pub input_tokens: u64,
    pub context_tokens: u64,
    pub output_tokens: u64,
    /// Micro-dollars at list price, with no context cached and with all of it.
    pub list_uncached: i64,
    pub list_cached: i64,
    /// What the ledger charges (list, the credits fee, the margin), cached.
    pub charge_cached: i64,
    pub charge_uncached: i64,
    pub hours: f64,
}

/// The spec's budget of the view a compactor call sees as its context.
const VIEW: usize = 128_000;
/// A model's summary line, bytes, on average (the spec measured about 250
/// on real lines; a line the compactor writes is near NODE).
const LINE_BYTES: usize = 420;
/// Bytes a token holds, on average, for chat text and summary lines.
const BYTES_PER_TOKEN: usize = 3;
/// COMPACT and SCALE, the fixed part of every call, in tokens.
const PROMPT_TOKENS: u64 = 1_300;
/// A call's reasoning and its overhead, in output tokens, and a line's.
const CALL_OUTPUT_TOKENS: u64 = 300;
const LINE_OUTPUT_TOKENS: u64 = 160;
/// Calls a line takes, on average, with the cut-at-limit retries.
const RETRY_FACTOR: f64 = 1.25;
/// A call's time, one after another (a job's steps run one at a time).
const SECONDS_PER_CALL: f64 = 8.0;
/// A batch's nodes, at most (the template's BATCH), and its messages'
/// bytes, at most (BATCH_MAX_BYTES).
const BATCH: usize = 8;
const BATCH_MAX_BYTES: usize = 192 * 1024;

/// The compactor's work for these conversations played into an empty mind,
/// simulated: a level-0 node is free when its line fits NODE, a merge when
/// its two children's lines fit together; every other node is a call whose
/// line is LINE_BYTES. Each call sees the view so far (at most VIEW) as its
/// context. Priced at the cheap tier's prices in the default price book.
pub fn estimate(convs: &[Conversation]) -> Estimate {
    let mut e = Estimate::default();
    let mut sizes: Vec<usize> = vec![];
    let mut view = 0usize;
    let mut batch = (0usize, 0usize);
    let mut step_bytes = 0u64;
    for m in convs.iter().flat_map(|c| &c.messages) {
        let line = m.role.kind().len() + 2 + m.text.len();
        e.messages += 1;
        e.bytes += m.text.len();
        if line <= NODE {
            sizes.push(line);
            view += line;
            continue;
        }
        e.level0_calls += 1;
        step_bytes += line as u64;
        if batch.0 == BATCH || batch.1 + line > BATCH_MAX_BYTES {
            batch = (0, 0);
        }
        if batch.0 == 0 {
            e.level0_batches += 1;
            e.context_tokens += (view.min(VIEW) / BYTES_PER_TOKEN) as u64;
        }
        batch = (batch.0 + 1, batch.1 + line);
        sizes.push(LINE_BYTES);
        view += LINE_BYTES;
    }
    // merges, level by level, over complete pairs
    let mut level = sizes;
    let mut merge_context = 0u64;
    let mut done = 0usize;
    while level.len() >= 2 {
        let mut up = Vec::with_capacity(level.len() / 2);
        for pair in level.chunks_exact(2) {
            let joined = pair[0] + 1 + pair[1];
            if joined <= NODE {
                e.merges_free += 1;
                up.push(joined);
            } else {
                e.merge_calls += 1;
                step_bytes += joined as u64;
                up.push(LINE_BYTES);
                done += 1;
                if done % BATCH == 1 {
                    merge_context += (VIEW.min(e.bytes) / BYTES_PER_TOKEN) as u64;
                }
            }
        }
        level = up;
    }
    e.merge_batches = e.merge_calls.div_ceil(BATCH);
    e.context_tokens += merge_context;
    let calls = ((e.level0_batches + e.merge_batches) as f64 * RETRY_FACTOR).ceil() as u64;
    let lines = ((e.level0_calls + e.merge_calls) as f64 * RETRY_FACTOR).ceil() as u64;
    e.context_tokens = (e.context_tokens as f64 * RETRY_FACTOR).ceil() as u64;
    e.input_tokens = calls * PROMPT_TOKENS + step_bytes / BYTES_PER_TOKEN as u64;
    e.output_tokens = calls * CALL_OUTPUT_TOKENS + lines * LINE_OUTPUT_TOKENS;
    let book = fragment_core::price::PriceBook::defaults();
    let model = fragment_core::models::CHEAP_MODEL.to_string();
    let priced = |cached: bool| {
        let (input, cached_input) = if cached { (e.input_tokens, e.context_tokens) } else { (e.input_tokens + e.context_tokens, 0) };
        let usage = fragment_core::price::Usage::Tokens { model: model.clone(), input, cached_input, cache_write: 0, output: e.output_tokens };
        book.price(&usage).map(|p| (p.list, p.charge)).unwrap_or((0, 0))
    };
    (e.list_uncached, e.charge_uncached) = priced(false);
    (e.list_cached, e.charge_cached) = priced(true);
    e.hours = calls as f64 * SECONDS_PER_CALL / 3600.0;
    e
}

/// Per source: conversations, messages (the person's, the agent's), bytes.
pub fn counts(convs: &[Conversation]) -> BTreeMap<Source, (usize, usize, usize, usize)> {
    let mut out: BTreeMap<Source, (usize, usize, usize, usize)> = BTreeMap::new();
    for c in convs {
        let e = out.entry(c.source).or_default();
        e.0 += 1;
        for m in &c.messages {
            match m.role {
                Role::User => e.1 += 1,
                Role::Assistant => e.2 += 1,
            }
            e.3 += m.text.len();
        }
    }
    out
}

/// A date the CLI takes (`--since`): `YYYY-MM-DD` or a full time.
pub fn since(s: &str) -> Result<i64> {
    match parse_time(s) {
        Some(ms) => Ok(ms),
        None => bail!("--since is a date (YYYY-MM-DD) or an ISO 8601 time, not {s:?}"),
    }
}

#[cfg(test)]
mod tests;
