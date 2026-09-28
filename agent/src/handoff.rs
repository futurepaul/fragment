//! Hand-offs (docs/api.md, Agents; Paul, 2026-09-27; docs/agent-computer.md):
//! a person's agent does light work itself and hands the rest to a
//! computer's hands, goose in a long-lived session per chat. In its owner's
//! turns only (a guest's would spend the owner's budget on a computer),
//! `platform__hand_off({task, computer?, throwaway?})` starts a job on a
//! computer and answers at once:
//!
//! - by default on the chat's computer: the one it is bound to, else its
//!   owner's home computer (`PUT /api/a/{name}/home`), which the chat is
//!   bound to from then on, so its session there carries over;
//! - with `computer`, one of the owner's fragments whose job `do` (or
//!   `build`) takes `{task, chat?}` (a pet, a builder);
//! - with `throwaway`, with `fragments`, or with no home, on a throwaway: a
//!   private fragment of the owner's from the builder template
//!   (`fragment_core::tools::throwaway_label`), whose `build` runs goose on
//!   the fragment's own new computer: extra hands, and fresh ones for
//!   changing the owner's apps (Paul, 2026-09-28: "ideally it hands off to
//!   an ephemeral computer not the pet").
//!
//! `fragments` names the owner's existing fragments the work changes, and
//! the computer is made an editor of each (`let_edit`: the owner-only grant
//! the chat's is, cell/src/agents.rs `add_computer`), before the work starts
//! or, a throwaway's, once it paired (the builder waits for it). Without it
//! a computer could change only what it made (Paul's chat, 2026-09-28: the
//! pet, asked to update starship-countdown, had no role there).
//!
//! The job is told the asking chat (`chat`: its session, and where its steps
//! go), and the computer is made an editor of the chat, so it posts each
//! step there as its owner's computer (`grant`; a throwaway's once it is
//! paired). Both are calls for the asker, capped as every call is
//! (decision 17), and the computer spends its owner's budget, as everything
//! a fragment does. (It is an editor of its owner's memory already: the
//! platform makes it one as it pairs, cell/src/memory.rs.) No turn waits
//! for the work: a turn that starts one ends there, the platform saying it
//! is on its way (`acknowledgement`), so the model is never asked to go on
//! and cannot write a result it has not received (Paul's chat, 2026-09-27:
//! "on its way… The computer is done." and an invented answer). The computer posts its steps and its answer in
//! the chat, as itself (computer/task.mjs), and the agent's alarm watches
//! the run between turns (`watch`). When it ends, its result reaches the
//! conversation that asked as a note (`note`): input the agent reads, never
//! a message in its own voice to imitate. The agent says in the chat only
//! what the computer could not (a run that failed, an answer it could not
//! post). Then a throwaway is removed, its computer first (the Sprite
//! destroyed), keeping what it built.

use std::time::Duration;

use anyhow::anyhow;
use fragment_core::tools::{is_throwaway, op_id, reply_id, throwaway_label};
use fragment_core::work::{cut, handoff_turn};
use fragment_proto::{split_fragment_name, valid_fragment_name, FragmentStatus, OpKind, Run, RunStatus};
use goose_provider_types::conversation::message::{Message, MessageContent};
use rmcp::model::Role;
use serde::Deserialize;
use serde_json::{json, Value};
use worker::{Method, SqlStorage, Storage};

use crate::fleet::{self, Fleet};
use crate::js;
use crate::store::{self, kv_get, kv_set, kv_u64};

pub const TOOL: &str = "platform__hand_off";
/// A task's length (the builder's `build` takes at most this).
const TASK_MAX: usize = 4000;
/// Hand-offs one agent watches at once.
const RUNNING_MAX: i64 = 8;
/// How often a run is looked at, and how long it is waited for: a builder's
/// goose is capped at 10 minutes, a job's command at 60.
const WATCH_EVERY_MS: i64 = 10_000;
const WATCH_MAX_MS: i64 = 90 * 60 * 1000;
/// The fragments one hand-off may change.
const FRAGMENTS_MAX: usize = 8;
/// The most of a result said in the conversation, and of its task.
const SAID_MAX_CHARS: usize = 1500;
const TASK_SAID_CHARS: usize = 300;

/// One hand-off being watched.
#[derive(Deserialize)]
struct Watched {
    fragment: String,
    run: i64,
    conv: String,
    throwaway: i64,
    started_at: i64,
    said: i64,
    granted: i64,
    /// the fragments its work changes, a JSON list
    fragments: String,
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
    let fragments = changed(fleet, sql, asker, &args["fragments"]).await?;
    let named = args["computer"].as_str();
    let (fragment, op, throwaway) = loop {
        let chosen = match (named, args["throwaway"] == true || !fragments.is_empty()) {
            (Some(named), _) => Some(named.to_string()),
            (None, true) => None,
            (None, false) => computer_for(sql, conv).map_err(|e| e.to_string())?,
        };
        let Some(computer) = chosen else { break (made(fleet, sql, request_id).await?, "build", true) };
        match offered(fleet, asker, &computer).await {
            Ok(op) => break (computer, op, false),
            // the chat's computer is gone, or does no work now: the chat
            // goes to the home computer (or a throwaway) instead
            Err(_) if named.is_none() && unbind(sql, conv, &computer).map_err(|e| e.to_string())? => continue,
            Err(e) => return Err(e),
        }
    };
    // by default, a chat is bound to the first computer that works for it
    if named.is_none() && !throwaway {
        let bind = "INSERT OR IGNORE INTO bound (conv, computer, at) VALUES (?, ?, ?)";
        sql.exec(bind, vec![conv.into(), fragment.as_str().into(), (js::now_ms() as i64).into()]).map_err(|e| e.to_string())?;
    }
    let chat = store::chat_of(conv).map(|(f, c)| format!("{f}/{c}"));
    let mut input = json!({ "task": task });
    if let Some(chat) = &chat {
        input["chat"] = json!(chat);
    }
    if !fragments.is_empty() {
        // a builder's `build` takes them apart; any other job reads them in its task
        match op {
            "build" => input["fragments"] = json!(fragments),
            _ => input["task"] = json!(format!("{}\n\n{task}", editing(&fragments))),
        }
        if input["task"].as_str().is_some_and(|t| t.len() > TASK_MAX) {
            return Err(format!("task is 1-{TASK_MAX} bytes, with the fragments it changes"));
        }
        // a throwaway's computer is not one until it paired: `look` lets it then
        if !throwaway {
            for f in &fragments {
                let_edit(fleet, f, &fragment).await?;
            }
        }
    }
    let granted = !throwaway && grant(fleet, conv, &fragment).await;
    let body = json!({ "id": op_id(request_id), "input": input });
    let (status, answer) = fleet.call(Method::Post, &format!("/api/f/{fragment}/ops/{op}"), Some(&body)).await.map_err(|e| e.to_string())?;
    let Some(run) = answer["result"]["run"].as_i64().filter(|_| status == 200) else {
        if throwaway {
            let _ = remove(fleet, &fragment).await;
        }
        return Err(format!("{fragment} did not start the work ({status}): {}", fleet::message(&answer)));
    };
    let now = js::now_ms() as i64;
    sql.exec(
        "INSERT OR IGNORE INTO handoffs (fragment, run, conv, throwaway, started_at, next_at, granted, fragments) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        vec![
            fragment.as_str().into(),
            run.into(),
            conv.into(),
            (throwaway as i64).into(),
            now.into(),
            (now + WATCH_EVERY_MS).into(),
            (granted as i64).into(),
            json!(fragments).to_string().into(),
        ],
    )
    .map_err(|e| e.to_string())?;
    let note = "It runs on its own now, for minutes, and this turn ends here. The computer answers in the chat itself; \
                its answer reaches you as a note when the work ends.";
    Ok(json!({ "started": true, "computer": fragment, "run": run, "note": note }).to_string())
}

/// The computer a conversation's work goes to by default: the one it is
/// bound to, else its owner's home computer; none: a throwaway.
fn computer_for(sql: &SqlStorage, conv: &str) -> anyhow::Result<Option<String>> {
    #[derive(Deserialize)]
    struct Bound {
        computer: String,
    }
    let rows: Vec<Bound> = sql.exec("SELECT computer FROM bound WHERE conv = ?", vec![conv.into()]).and_then(|c| c.to_array()).map_err(|e| anyhow!("{e}"))?;
    match rows.into_iter().next() {
        Some(b) => Ok(Some(b.computer)),
        None => Ok(kv_get(sql, HOME)?.filter(|h| !h.is_empty())),
    }
}

/// Frees a conversation from `computer`; whether it was bound to it.
fn unbind(sql: &SqlStorage, conv: &str, computer: &str) -> anyhow::Result<bool> {
    let dropped = sql.exec("DELETE FROM bound WHERE conv = ? AND computer = ?", vec![conv.into(), computer.into()]).map_err(|e| anyhow!("{e}"))?;
    Ok(dropped.rows_written() > 0)
}

/// The owner's home computer (`PUT /api/a/{name}/home`): a fragment of
/// theirs that offers `do` or `build`, or none.
pub const HOME: &str = "home";

/// Sets (or, with `None`, clears) the home computer; `fleet` acts for the owner.
pub async fn set_home(fleet: &Fleet, sql: &SqlStorage, owner: &str, computer: Option<&str>) -> Result<Value, String> {
    if let Some(computer) = computer {
        offered(fleet, owner, computer).await?;
    }
    kv_set(sql, HOME, computer.unwrap_or_default()).map_err(|e| e.to_string())?;
    Ok(json!({ "home": computer }))
}

/// Makes the hand-off's computer an editor of the asking chat, so it posts
/// its steps there as its owner's computer (the platform takes this from an
/// agent for its owner's own computer and chat only: cell/src/agents.rs
/// `add_computer`). Whether that is settled: it is, or never will be (a
/// chat not its owner's); a throwaway's computer is not one until it paired.
async fn grant(fleet: &Fleet, conv: &str, computer: &str) -> bool {
    let Some((chat, _)) = store::chat_of(conv) else { return true };
    match fleet.call(Method::Put, &format!("/api/f/{chat}/members/{computer}"), Some(&json!({ "role": "editor" }))).await {
        Ok((200, _)) => true,
        Ok((status, answer)) => {
            worker::console_warn!("{computer} is not let post in {chat} ({status}): {}", fleet::message(&answer));
            status == 403
        }
        Err(e) => {
            worker::console_warn!("{computer} is not let post in {chat}: {e:#}");
            false
        }
    }
}

/// The fragments a hand-off's work changes (`fragments`): names of the
/// asker's own (a bare label is under the asker's username), each checked.
async fn changed(fleet: &Fleet, sql: &SqlStorage, asker: &str, named: &Value) -> Result<Vec<String>, String> {
    let Some(named) = named.as_array() else {
        return match named.is_null() {
            true => Ok(Vec::new()),
            false => Err("fragments is a list of fragments' names".into()),
        };
    };
    let me = kv_get(sql, "name").map_err(|e| e.to_string())?.unwrap_or_default();
    let username = split_fragment_name(&me).map(|(_, u)| u).ok_or("this agent has no name")?;
    let mut names = Vec::new();
    for name in named {
        let name = name.as_str().map(str::trim).ok_or("fragments is a list of fragments' names")?;
        let name = if name.contains('.') { name.to_string() } else { format!("{name}.{username}") };
        if !valid_fragment_name(&name) {
            return Err(format!("{name:?} is not a fragment's name (<label>.<username>)"));
        }
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.len() > FRAGMENTS_MAX {
        return Err(format!("a hand-off changes at most {FRAGMENTS_MAX} fragments"));
    }
    for name in &names {
        let status: FragmentStatus = fleet.get_as(&format!("/api/f/{name}/status")).await.map_err(|e| format!("{name}: {e}"))?;
        if status.owner != asker {
            return Err(format!("{name} is not the person's own: a computer is made an editor only of their own fragments"));
        }
    }
    Ok(names)
}

/// Makes `computer` an editor of `fragment`, its owner's, which the work
/// changes (cell/src/agents.rs `add_computer`, as the chat's `grant`).
async fn let_edit(fleet: &Fleet, fragment: &str, computer: &str) -> Result<(), String> {
    let path = format!("/api/f/{fragment}/members/{computer}");
    match fleet.call(Method::Put, &path, Some(&json!({ "role": "editor" }))).await.map_err(|e| e.to_string())? {
        (200, _) => Ok(()),
        (status, answer) => Err(format!("{computer} was not made an editor of {fragment} ({status}): {}", fleet::message(&answer))),
    }
}

/// What a computer's `do` reads of the fragments its work changes.
fn editing(fragments: &[String]) -> String {
    format!(
        "You are an editor of the fragments this changes: {}. Get one's files into a new folder with `mkdir <folder> && fragment sync <name> --dir <folder>`, \
         change them, put them live with `fragment deploy <name> --dir <folder>`, and check its page answers.",
        fragments.join(", ")
    )
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
        .exec(
            "SELECT fragment, run, conv, throwaway, started_at, said, granted, fragments FROM handoffs WHERE next_at <= ? ORDER BY next_at LIMIT ?",
            vec![now.into(), RUNNING_MAX.into()],
        )
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
        // a throwaway's computer, once it paired, is let post its steps
        // too, and change the fragments its work changes
        if h.granted == 0 {
            let fleet = fleet.acting_for(owner);
            let mut settled = grant(&fleet, &h.conv, &h.fragment).await;
            for f in serde_json::from_str::<Vec<String>>(&h.fragments).unwrap_or_default() {
                if let Err(e) = let_edit(&fleet, &f, &h.fragment).await {
                    worker::console_warn!("{e}");
                    settled = false;
                }
            }
            if settled {
                sql.exec("UPDATE handoffs SET granted = 1 WHERE fragment = ? AND run = ?", vec![h.fragment.as_str().into(), h.run.into()]).map_err(|e| anyhow!("{e}"))?;
            }
        }
        return later(sql, h, now, false);
    }
    if h.said == 0 {
        // never inside a turn of the conversation it lands in: no await
        // between this look and the note stored
        let running = kv_u64(sql, "active")? == 1 && kv_get(sql, "turn_conv")?.as_deref() == Some(h.conv.as_str());
        if running {
            return later(sql, h, now, false);
        }
        let (id, (answered, text)) = (format!("msg_handoff_{}_{}", h.fragment, h.run), came_to(&h.fragment, run.as_ref()));
        if store::message_seq(sql, &id).is_err() {
            store::append_message(sql, &h.conv, &note(&h.fragment, h.run, run.as_ref(), &text).with_id(&id))?;
        }
        if !answered {
            crate::post_answer(sql, fleet, &h.conv, &reply_id(&id), &text, None, Some(&handoff_turn(&h.fragment, h.run))).await?;
        }
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

/// What a hand-off's run came to: what the computer said last (the
/// builder answers `{url, message}` too), why it could not finish, or that
/// it is still going; and whether the computer answered in the chat itself,
/// as its task client does before it exits 0 (computer/task.mjs; the pet's
/// and the builder's runs answer its `code`).
fn came_to(fragment: &str, run: Option<&Run>) -> (bool, String) {
    let Some(run) = run else { return (false, format!("The work on {fragment} did not come back: it is gone.")) };
    let (answered, text) = match run.status {
        RunStatus::Succeeded => {
            let out = run.output.clone().unwrap_or_default();
            let said = match &out {
                Value::String(s) => s.as_str(),
                o => o["message"].as_str().or(o["text"].as_str()).unwrap_or(""),
            };
            let built = out["url"].as_str().map_or(String::new(), |url| format!("It built {url}.\n"));
            (out["code"] == 0, format!("{built}The computer said: {}", said.trim()))
        }
        RunStatus::Held | RunStatus::Blocked => (false, format!("The computer could not finish: {}", run.error.as_deref().unwrap_or("its run stopped"))),
        _ => (false, format!("It is still going on {fragment} after {} minutes, and is no longer watched.", WATCH_MAX_MS / 60_000)),
    };
    (answered, cut(&text, SAID_MAX_CHARS))
}

/// A finished hand-off in the conversation that asked: a note to the agent,
/// input labeled as the computer's and naming the task, so later turns know
/// what was done, with nothing in the agent's own voice to imitate.
fn note(fragment: &str, run_id: i64, run: Option<&Run>, came_to: &str) -> Message {
    let task = run.and_then(|r| r.input.as_ref()?["task"].as_str()).map_or(String::new(), |t| cut(t, TASK_SAID_CHARS));
    let text = format!("[The result of a hand-off to {fragment} (run {run_id}): the computer's words, not yours]\nTask: {task}\n{came_to}");
    Message::user().with_text(cut(&text, SAID_MAX_CHARS)).agent_only()
}

/// What a turn that started a hand-off says, as it ends there (turn.rs
/// `HandedOff`): the computers its model's last message handed work to,
/// from their results stored after it. `None` when that message started
/// none (or is this acknowledgement already).
pub fn acknowledgement(turn: &[Message]) -> Option<String> {
    let asked = turn.iter().rposition(|m| m.role == Role::Assistant)?;
    let calls: Vec<&str> = turn[asked]
        .content
        .iter()
        .filter_map(MessageContent::as_tool_request)
        .filter(|r| r.tool_call.as_ref().is_ok_and(|c| c.name == TOOL))
        .map(|r| r.id.as_str())
        .collect();
    let started = |c: &&MessageContent| c.as_tool_response().is_some_and(|r| calls.contains(&r.id.as_str()) && r.tool_result.as_ref().is_ok_and(|r| r.is_error != Some(true)));
    let computers: Vec<String> = turn[asked + 1..]
        .iter()
        .flat_map(|m| &m.content)
        .filter(started)
        .filter_map(|c| serde_json::from_str::<Value>(&c.as_tool_response_text()?).ok()?["computer"].as_str().map(str::to_string))
        .collect();
    (!computers.is_empty()).then(|| format!("On its way: {} has it, and its answer will show up here when it's done.", computers.join(" and ")))
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
