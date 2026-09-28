//! The owner's memory (docs/agent-computer.md, slice 3): a private fragment
//! of theirs that the platform records (`/api/memory`, cell/src/memory.rs),
//! holding `memory/*.md` (facts and preferences) and
//! `skills/<name>/SKILL.md`, which their computers keep and write
//! (fragment_core's computer/task.mjs). The agent reads the facts into its
//! owner's turns, capped, and writes one fact at a time itself
//! (`platform__remember`: one commit to main, in its owner's turns only), so
//! "remember X" wakes no computer. Skills, and anything else, stay the
//! computers'.

use fragment_core::tools::op_id;
use serde_json::{json, Value};
use worker::{Method, SqlStorage};

use crate::fleet::{self, Fleet};
use crate::store::{kv_get, kv_set};

pub const TOOL: &str = "platform__remember";
/// The most of the facts an owner's turn is told (about 500 tokens).
const VIEW_MAX_CHARS: usize = 2000;
/// A fact, and what it replaces.
const FACT_MAX_CHARS: usize = 500;
/// A topic's file, after the write: past this, it is to be tidied first.
const FILE_MAX_BYTES: usize = 4096;
const TOPIC_MAX: usize = 40;

/// The owner's memory as the platform records it (with `make`, made first
/// if there is none), asked each time: its owner may name another
/// (`fragment memory use`). `fleet` acts for the owner.
async fn name(fleet: &Fleet, make: bool) -> anyhow::Result<Option<String>> {
    let (status, answer) = fleet.call(if make { Method::Post } else { Method::Get }, "/api/memory", None).await?;
    anyhow::ensure!(status == 200, "the owner's memory ({status}): {}", fleet::message(&answer));
    Ok(answer["name"].as_str().map(str::to_string))
}

/// A file of the memory at main: its text, `None` when there is none.
async fn file(fleet: &Fleet, memory: &str, path: &str) -> anyhow::Result<Option<String>> {
    let mut query = worker::Url::parse("https://q/")?;
    query.query_pairs_mut().append_pair("path", path);
    match fleet.call(Method::Get, &format!("/api/f/{memory}/file?{}", query.query().unwrap_or_default()), None).await? {
        (200, Value::String(s)) => Ok(Some(s)),
        (200, v) => Ok(Some(v.to_string())),
        (404, _) => Ok(None),
        (status, v) => anyhow::bail!("reading {memory}'s {path} ({status}): {}", fleet::message(&v)),
    }
}

/// What an owner's turn is told of the memory: its facts at main, at most
/// `VIEW_MAX_CHARS`, read again only when main moved (or it is another
/// memory). `fleet` acts for the owner.
pub async fn view(fleet: &Fleet, sql: &SqlStorage) -> anyhow::Result<String> {
    const EMPTY: &str = "\n\nYour owner's memory holds nothing yet (keep a fact with platform__remember).";
    let Some(memory) = name(fleet, false).await? else { return Ok(EMPTY.into()) };
    let (status, listed) = fleet.call(Method::Get, &format!("/api/f/{memory}/files"), None).await?;
    let at = match status {
        200 => format!("{memory}@{}", listed["ref"].as_str().unwrap_or_default()),
        403 | 404 => memory.clone(),
        _ => anyhow::bail!("reading {memory} ({status}): {}", fleet::message(&listed)),
    };
    if kv_get(sql, "memory_at")?.as_deref() == Some(at.as_str()) {
        if let Some(view) = kv_get(sql, "memory_view")? {
            return Ok(view);
        }
    }
    let mut paths: Vec<&str> = listed["files"].as_array().into_iter().flatten().filter_map(|f| f["path"].as_str()).collect();
    paths.retain(|p| p.strip_prefix("memory/").is_some_and(|f| f.ends_with(".md") && !f.contains('/')));
    paths.sort();
    let mut facts = String::new();
    for path in paths {
        if facts.chars().count() > VIEW_MAX_CHARS {
            break;
        }
        if let Some(text) = file(fleet, &memory, path).await? {
            facts.push_str(&format!("\n### {path}\n{}", text.trim()));
        }
    }
    if facts.chars().count() > VIEW_MAX_CHARS {
        facts = format!("{}… (more in {memory})", facts.chars().take(VIEW_MAX_CHARS).collect::<String>());
    }
    let view = match facts.is_empty() {
        true => EMPTY.to_string(),
        false => format!("\n\nYour owner's memory, their private fragment {memory}, which their computers keep too (keep or correct a fact with platform__remember):{facts}"),
    };
    kv_set(sql, "memory_at", &at)?;
    kv_set(sql, "memory_view", &view)?;
    Ok(view)
}

/// The tool's name, description, and input schema.
pub fn tool() -> (&'static str, &'static str, Value) {
    (
        TOOL,
        "Keeps a lasting fact about your owner in their memory, which you and their computers read: a preference, \
         something about them or their work. It adds the fact as a line of memory/<topic>.md, or, with `replaces`, \
         puts it in place of the text given (to correct or update one). Facts only, one line each: code, skills, and \
         anything longer are the computer's.",
        json!({ "type": "object", "required": ["topic", "fact"], "additionalProperties": false, "properties": {
            "topic": { "type": "string", "description": "the file it goes in: lowercase letters, digits, and dashes (preferences, people, work)" },
            "fact": { "type": "string", "description": format!("the fact, one line, at most {FACT_MAX_CHARS} characters") },
            "replaces": { "type": "string", "description": "the text in that file it replaces, exactly as it is there" },
        } }),
    )
}

/// `platform__remember`: the fact in `memory/<topic>.md`, one commit to the
/// memory's main (made first, if the owner has none), keyed by the call so a
/// replayed call commits nothing again. `fleet` acts for the owner.
pub async fn remember(fleet: &Fleet, request_id: &str, args: &Value) -> Result<String, String> {
    let text = |k: &str| args[k].as_str().map(str::trim).filter(|s| !s.is_empty());
    let topic = text("topic").filter(|t| t.len() <= TOPIC_MAX && t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !t.starts_with('-'));
    let topic = topic.ok_or(format!("topic is 1-{TOPIC_MAX} lowercase letters, digits, and dashes"))?;
    let fact = text("fact").filter(|f| f.chars().count() <= FACT_MAX_CHARS && !f.contains('\n'));
    let fact = fact.ok_or(format!("fact is one line of 1-{FACT_MAX_CHARS} characters"))?;
    let replaces = text("replaces");
    if replaces.is_some_and(|r| r.chars().count() > FACT_MAX_CHARS) {
        return Err(format!("replaces is at most {FACT_MAX_CHARS} characters"));
    }
    let memory = name(fleet, true).await.map_err(|e| e.to_string())?.ok_or("the platform made no memory")?;
    let path = format!("memory/{topic}.md");
    let was = file(fleet, &memory, &path).await.map_err(|e| e.to_string())?.unwrap_or_default();
    let line = if fact.starts_with("- ") { fact.to_string() } else { format!("- {fact}") };
    let now = match replaces {
        Some(old) if !was.contains(old) => return Err(format!("{path} has no {old:?}: nothing replaced")),
        Some(old) => was.replacen(old, fact, 1),
        None if was.is_empty() || was.ends_with('\n') => format!("{was}{line}\n"),
        None => format!("{was}\n{line}\n"),
    };
    if now.len() > FILE_MAX_BYTES {
        return Err(format!("{path} would be over {FILE_MAX_BYTES} bytes: tidy it first (replace lines with shorter ones), or use another topic"));
    }
    let body = json!({ "files": [{ "path": path, "text": now }], "message": format!("remember: {topic}"), "key": op_id(request_id) });
    match fleet.call(Method::Post, &format!("/api/f/{memory}/files"), Some(&body)).await.map_err(|e| e.to_string())? {
        (200, answer) => Ok(json!({ "remembered": path, "commit": answer["commit"] }).to_string()),
        (status, answer) => Err(format!("{memory} did not take it ({status}): {}", fleet::message(&answer))),
    }
}
