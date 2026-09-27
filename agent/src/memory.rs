//! The owner's memory (docs/agent-computer.md, slice 3): a private fragment
//! of theirs, `memory.<username>`, holding `memory/*.md` (facts and
//! preferences) and `skills/<name>/SKILL.md`, which their computers keep and
//! write (fragment_core's computer/task.mjs). The agent makes it at its
//! owner's first hand-off and makes each computer it hands work to an editor
//! there, as it does of the chat (handoff.rs). It reads the facts into its
//! owner's turns, capped and read-only: it writes no files, so what is to be
//! remembered is handed off.

use fragment_proto::split_fragment_name;
use serde_json::{json, Value};
use worker::{Method, SqlStorage};

use crate::fleet::{self, Fleet};
use crate::store::{kv_get, kv_set};

/// The most of the facts an owner's turn is told (about 500 tokens).
const VIEW_MAX_CHARS: usize = 2000;

/// `memory.<username>`, the owner's, under this agent's username.
fn name(sql: &SqlStorage) -> anyhow::Result<Option<String>> {
    let me = kv_get(sql, "name")?.unwrap_or_default();
    Ok(split_fragment_name(&me).map(|(_, username)| format!("memory.{username}")))
}

/// Makes the owner's computer `computer` an editor of their `fragment`
/// (cell/src/agents.rs `add_computer`): the platform's answer, 0 when none.
pub async fn add(fleet: &Fleet, fragment: &str, computer: &str) -> u16 {
    match fleet.call(Method::Put, &format!("/api/f/{fragment}/members/{computer}"), Some(&json!({ "role": "editor" }))).await {
        Ok((200, _)) => 200,
        Ok((status, answer)) => {
            worker::console_warn!("{computer} is not made an editor of {fragment} ({status}): {}", fleet::message(&answer));
            status
        }
        Err(e) => {
            worker::console_warn!("{computer} is not made an editor of {fragment}: {e:#}");
            0
        }
    }
}

/// The memory made, if it is not, and `computer` an editor there: whether
/// that is settled (a throwaway's computer is none until it paired).
/// `fleet` acts for the owner.
pub async fn grant(fleet: &Fleet, sql: &SqlStorage, computer: &str) -> bool {
    let Ok(Some(memory)) = name(sql) else { return true };
    match add(fleet, &memory, computer).await {
        200 | 403 => true,
        // no memory yet (or no such computer yet): made, members only, then again
        404 => {
            let made = fleet.call(Method::Post, "/api/fragments", Some(&json!({ "name": memory, "visibility": "members" }))).await;
            matches!(made, Ok((200 | 409, _))) && add(fleet, &memory, computer).await == 200
        }
        _ => false,
    }
}

/// What an owner's turn is told of the memory: its facts at main, at most
/// `VIEW_MAX_CHARS`, read again only when main moved. `fleet` acts for the
/// owner.
pub async fn view(fleet: &Fleet, sql: &SqlStorage) -> anyhow::Result<String> {
    let Some(memory) = name(sql)? else { return Ok(String::new()) };
    let (status, listed) = fleet.call(Method::Get, &format!("/api/f/{memory}/files"), None).await?;
    let at = match status {
        200 => listed["ref"].as_str().unwrap_or_default().to_string(),
        403 | 404 => String::new(),
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
        let mut query = worker::Url::parse("https://q/")?;
        query.query_pairs_mut().append_pair("path", path);
        let (status, text) = fleet.call(Method::Get, &format!("/api/f/{memory}/file?{}", query.query().unwrap_or_default()), None).await?;
        let text = match (status, text) {
            (200, Value::String(s)) => s,
            (200, v) => v.to_string(),
            (status, v) => anyhow::bail!("reading {memory}'s {path} ({status}): {}", fleet::message(&v)),
        };
        facts.push_str(&format!("\n### {path}\n{}", text.trim()));
    }
    if facts.chars().count() > VIEW_MAX_CHARS {
        facts = format!("{}… (more in {memory})", facts.chars().take(VIEW_MAX_CHARS).collect::<String>());
    }
    let view = format!(
        "\n\nYour owner's memory, which their computer keeps in {memory} (you only read it: to add to it or change it, hand it off):{}",
        if facts.is_empty() { " nothing yet." } else { &facts }
    );
    kv_set(sql, "memory_at", &at)?;
    kv_set(sql, "memory_view", &view)?;
    Ok(view)
}
