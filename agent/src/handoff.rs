//! Hand-offs (docs/api.md, Agents; Paul, 2026-09-27): a person's agent does
//! light work itself and hands the rest to a computer. In its owner's turns
//! only (a guest's would spend the owner's budget on a computer),
//! `platform__hand_off({task, computer?})` starts a job on a computer and
//! answers at once:
//!
//! - with `computer`, one of the owner's fragments whose job `do` (or
//!   `build`) takes `{task}` (a pet, a builder), it runs there;
//! - without, on a throwaway: a private fragment of the owner's from the
//!   builder template (`fragment_core::tools::throwaway_label`), whose
//!   `build` runs goose on the fragment's own new computer.
//!
//! Both are calls for the asker, capped as every call is (decision 17), and
//! the computer spends its owner's budget, as everything a fragment does.
//! No turn waits for a build: the turn ends saying the work is on its way,
//! and the agent's alarm watches the run between turns (`watch`). When it
//! ends, its result is said in the conversation that asked (stored there,
//! and posted to its chat as an answer is), then a throwaway is removed,
//! its computer first (the Sprite destroyed), keeping what it built.

use std::time::Duration;

use anyhow::anyhow;
use fragment_core::tools::{is_throwaway, op_id, reply_id, throwaway_label};
use fragment_proto::{split_fragment_name, valid_fragment_name, FragmentStatus, OpKind, Run, RunStatus};
use goose_provider_types::conversation::message::Message;
use serde::Deserialize;
use serde_json::{json, Value};
use worker::{Method, SqlStorage, Storage};

use crate::fleet::{self, Fleet};
use crate::js;
use crate::store::{self, kv_get, kv_u64};

pub const TOOL: &str = "platform__hand_off";
/// A task's length (the builder's `build` takes at most this).
const TASK_MAX: usize = 4000;
/// Hand-offs one agent watches at once.
const RUNNING_MAX: i64 = 8;
/// How often a run is looked at, and how long it is waited for: a builder's
/// goose is capped at 10 minutes, a job's command at 60.
const WATCH_EVERY_MS: i64 = 10_000;
const WATCH_MAX_MS: i64 = 90 * 60 * 1000;
/// The most of a result said in the conversation.
const SAID_MAX_CHARS: usize = 1500;

/// One hand-off being watched.
#[derive(Deserialize)]
struct Watched {
    fragment: String,
    run: i64,
    conv: String,
    throwaway: i64,
    started_at: i64,
    said: i64,
}

/// `platform__hand_off`: the work started on a computer, or why not.
/// `fleet` acts for the turn's asker, its owner.
pub async fn start(fleet: &Fleet, sql: &SqlStorage, conv: &str, request_id: &str, args: &Value) -> Result<String, String> {
    let task = args["task"].as_str().map(str::trim).filter(|t| !t.is_empty() && t.len() <= TASK_MAX).ok_or(format!("task is 1-{TASK_MAX} bytes"))?;
    let rows: Vec<Value> = sql.exec("SELECT COUNT(*) AS n FROM handoffs", None).and_then(|c| c.to_array()).map_err(|e| e.to_string())?;
    if rows.first().and_then(|r| r["n"].as_i64()).unwrap_or(0) >= RUNNING_MAX {
        return Err(format!("{RUNNING_MAX} hand-offs are running already: wait for one to end"));
    }
    let asker = fleet.acting_for.as_deref().expect("a turn's tools act for its asker");
    let (fragment, op, throwaway) = match args["computer"].as_str() {
        Some(named) => (named.to_string(), offered(fleet, asker, named).await?, false),
        None => (made(fleet, sql, request_id).await?, "build", true),
    };
    let body = json!({ "id": op_id(request_id), "input": { "task": task } });
    let (status, answer) = fleet.call(Method::Post, &format!("/api/f/{fragment}/ops/{op}"), Some(&body)).await.map_err(|e| e.to_string())?;
    let Some(run) = answer["result"]["run"].as_i64().filter(|_| status == 200) else {
        if throwaway {
            let _ = remove(fleet, &fragment).await;
        }
        return Err(format!("{fragment} did not start the work ({status}): {}", fleet::message(&answer)));
    };
    let now = js::now_ms() as i64;
    sql.exec(
        "INSERT OR IGNORE INTO handoffs (fragment, run, conv, throwaway, started_at, next_at) VALUES (?, ?, ?, ?, ?, ?)",
        vec![fragment.as_str().into(), run.into(), conv.into(), (throwaway as i64).into(), now.into(), (now + WATCH_EVERY_MS).into()],
    )
    .map_err(|e| e.to_string())?;
    let note = "It runs on its own now, for minutes: its result is said in this conversation when it ends. Tell the person \
                it is on its way, and end your turn.";
    Ok(json!({ "started": true, "computer": fragment, "run": run, "note": note }).to_string())
}

/// The job a named computer offers for work: `do`, else `build`, on a
/// fragment of the asker's own.
async fn offered(fleet: &Fleet, asker: &str, named: &str) -> Result<&'static str, String> {
    if !valid_fragment_name(named) {
        return Err(format!("{named:?} is not a fragment's name (<label>.<username>)"));
    }
    let status: FragmentStatus = fleet.get_as(&format!("/api/f/{named}/status")).await.map_err(|e| e.to_string())?;
    if status.owner != asker {
        return Err(format!("{named} is not the person's own: hand work only to their computers"));
    }
    let job = |op: &str| status.code.operations.get(op).is_some_and(|d| matches!(d.kind, OpKind::Job));
    ["do", "build"].into_iter().find(|op| job(op)).ok_or_else(|| format!("{named} offers no `do` or `build` job to hand work to"))
}

/// The throwaway this call makes: a private fragment of the owner's (under
/// their username, as this agent is) from the builder template, which the
/// platform records as this agent's throwaway (only so may it delete it),
/// named from the call, so a replayed call finds it made (409).
async fn made(fleet: &Fleet, sql: &SqlStorage, request_id: &str) -> Result<String, String> {
    let me = kv_get(sql, "name").map_err(|e| e.to_string())?.unwrap_or_default();
    let username = split_fragment_name(&me).map(|(_, u)| u).ok_or("this agent has no name")?;
    let name = format!("{}.{username}", throwaway_label(request_id));
    let create = json!({ "name": name, "template": "builder", "visibility": "members", "throwaway": true });
    match fleet.call(Method::Post, "/api/fragments", Some(&create)).await.map_err(|e| e.to_string())? {
        (200 | 409, _) => Ok(name),
        (status, answer) => Err(format!("making a computer for it ({status}): {}", fleet::message(&answer))),
    }
}

/// Removes a throwaway this agent made (the platform checks its record):
/// its computer (the Sprite destroyed, its keys revoked), then the
/// fragment. What it built stays its owner's. One gone already is removed.
async fn remove(fleet: &Fleet, fragment: &str) -> anyhow::Result<()> {
    assert!(is_throwaway(fragment), "an agent removes only a throwaway");
    match fleet.call(Method::Delete, &format!("/api/f/{fragment}"), None).await? {
        (200 | 404, _) => Ok(()),
        (status, answer) => Err(anyhow!("removing {fragment} ({status}): {}", fleet::message(&answer))),
    }
}

/// Looks at each hand-off whose time came, from the agent's alarm. `fleet`
/// is the agent's own; runs are read for its owner.
pub async fn watch(fleet: &Fleet, sql: &SqlStorage, owner: &str) -> anyhow::Result<()> {
    let now = js::now_ms() as i64;
    let due: Vec<Watched> = sql
        .exec("SELECT fragment, run, conv, throwaway, started_at, said FROM handoffs WHERE next_at <= ? ORDER BY next_at LIMIT ?", vec![now.into(), RUNNING_MAX.into()])
        .and_then(|c| c.to_array())
        .map_err(|e| anyhow!("{e}"))?;
    for h in due {
        if let Err(e) = look(fleet, sql, owner, &h, now).await {
            worker::console_warn!("hand-off {} run {}: {e:#}", h.fragment, h.run);
            // looked at again later; past twice the wait, let be (a throwaway left is named as one)
            let done = now - h.started_at > 2 * WATCH_MAX_MS;
            later(sql, &h, now, done)?;
        }
    }
    Ok(())
}

/// One hand-off: a run still going is looked at again; one that ended (or
/// was waited for long enough) is said once, then a throwaway is removed.
async fn look(fleet: &Fleet, sql: &SqlStorage, owner: &str, h: &Watched, now: i64) -> anyhow::Result<()> {
    let (status, answer) = fleet.acting_for(owner).call(Method::Get, &format!("/api/f/{}/runs/{}", h.fragment, h.run), None).await?;
    let run = match status {
        200 => Some(Run::deserialize(&answer)?),
        403 | 404 => None,
        _ => return Err(anyhow!("reading the run ({status}): {}", fleet::message(&answer))),
    };
    let ended = run.as_ref().is_none_or(|r| matches!(r.status, RunStatus::Succeeded | RunStatus::Held | RunStatus::Blocked));
    if !ended && now - h.started_at < WATCH_MAX_MS {
        return later(sql, h, now, false);
    }
    if h.said == 0 {
        // never inside a turn of the conversation it lands in: no await
        // between this look and the message stored
        let running = kv_u64(sql, "active")? == 1 && kv_get(sql, "turn_conv")?.as_deref() == Some(h.conv.as_str());
        if running {
            return later(sql, h, now, false);
        }
        let (id, text) = (format!("msg_handoff_{}_{}", h.fragment, h.run), result_text(&h.fragment, run.as_ref()));
        if store::message_seq(sql, &id).is_err() {
            store::append_message(sql, &h.conv, &Message::assistant().with_text(&text).with_id(&id))?;
        }
        crate::post_answer(sql, fleet, &h.conv, &reply_id(&id), &text, None, None).await?;
        sql.exec("UPDATE handoffs SET said = 1 WHERE fragment = ? AND run = ?", vec![h.fragment.as_str().into(), h.run.into()]).map_err(|e| anyhow!("{e}"))?;
    }
    if ended && h.throwaway == 1 {
        remove(fleet, &h.fragment).await?;
    }
    later(sql, h, now, true)
}

/// A hand-off looked at again in a while, or no longer (`done`).
fn later(sql: &SqlStorage, h: &Watched, now: i64, done: bool) -> anyhow::Result<()> {
    let (fragment, run) = (h.fragment.as_str().into(), h.run.into());
    let done = match done {
        true => sql.exec("DELETE FROM handoffs WHERE fragment = ? AND run = ?", vec![fragment, run]),
        false => sql.exec("UPDATE handoffs SET next_at = ? WHERE fragment = ? AND run = ?", vec![(now + WATCH_EVERY_MS).into(), fragment, run]),
    };
    done.map(|_| ()).map_err(|e| anyhow!("{e}"))
}

/// What a hand-off's run came to, as it is said: its URL and what the
/// computer said last (the builder answers `{url, message}`), why it could
/// not finish, or that it is still going.
fn result_text(fragment: &str, run: Option<&Run>) -> String {
    let Some(run) = run else { return format!("The work on {fragment} did not come back: it is gone.") };
    let text = match run.status {
        RunStatus::Succeeded => {
            let out = run.output.clone().unwrap_or_default();
            let said = match &out {
                Value::String(s) => s.as_str(),
                o => o["message"].as_str().or(o["text"].as_str()).unwrap_or(""),
            };
            let head = out["url"].as_str().map_or("The computer is done.".to_string(), |url| format!("Done: {url}"));
            format!("{head}\n\n{}", said.trim())
        }
        RunStatus::Held | RunStatus::Blocked => format!("The computer could not finish: {}", run.error.as_deref().unwrap_or("its run stopped")),
        _ => format!("It is still going on {fragment} after {} minutes; I stopped watching it.", WATCH_MAX_MS / 60_000),
    };
    fragment_core::work::cut(&text, SAID_MAX_CHARS)
}

/// The agent's alarm at rest: when the next hand-off is due, or none.
pub async fn arm(storage: &Storage, sql: &SqlStorage) -> anyhow::Result<()> {
    let next: Vec<Value> = sql.exec("SELECT MIN(next_at) AS at FROM handoffs", None).and_then(|c| c.to_array()).map_err(|e| anyhow!("{e}"))?;
    let result = match next.first().and_then(|r| r["at"].as_i64()) {
        Some(at) => storage.set_alarm(Duration::from_millis((at - js::now_ms() as i64).max(50) as u64)).await,
        None => storage.delete_alarm().await,
    };
    result.map_err(|e| anyhow!("{e}"))
}
