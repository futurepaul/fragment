//! Two real agents asking each other (docs/cloudflare-v1.md, decision 8;
//! docs/chat-records.md): on a branch preview, a Hermes computer running two
//! agents of one person, their real model. The hosted lane runs it by name
//! (`cargo xtask e2e --hosted … --only agent-ask`); a local run never has a
//! real agent (needs.rs `RealAgent`): the bridge's tests run the hand-offs
//! with the scripted runtime, and shell-ui runs `fragment ask` as a person.
//!
//! An e2e person gets two agents on one computer: Juniper, who talks with
//! them, and Fred, who keeps a secret word in his SOUL. Asked to get the
//! word from Fred with `fragment ask … --wait`, Juniper's turn runs it in its
//! terminal: the CLI, as Juniper, makes a chat of the two agents and their
//! owner, adds Fred, and asks him there; Fred's bridge takes the question as
//! a turn, and he answers in that chat; the CLI prints his answer, and
//! Juniper tells the person the word. Then the computer sleeps (`--sweep`
//! deletes the run's `e2e-<run>-` fragments, the agents' chat among them).
//!
//! A check that depends on what the model chooses to do says so in its text.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::computers::{agent_replies, phase, turn_of, work_of, QUEUE_DRAIN};
use super::jobs::records;
use super::ledger::entries;
use crate::api::{now_s, Api};
use crate::{Need, Suite};

pub const SECTION: &str = "agent-ask";

/// The paid calls the run lends the section's person: Juniper's turn (a
/// model call before and after its terminal step, Hermes' guardian and
/// title), and Fred's (a call or two, and his title).
const PAID_CALLS: u64 = 24;
/// A new computer's first start on a preview, to its agents following.
const FIRST_START: Duration = Duration::from_secs(10 * 60);
/// A turn that runs a tool and waits on another agent's turn.
const WORK: Duration = Duration::from_secs(10 * 60);
/// Its owner's sleep, asked until it is asleep.
const SLEEP: Duration = Duration::from_secs(3 * 60);
const POLL: Duration = Duration::from_secs(3);

/// Asks `f` every `POLL` until it answers, or `bound` passes.
#[track_caller]
fn within<T>(bound: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let at = std::panic::Location::caller();
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if t0.elapsed() >= bound {
            println!("      (a wait ran out its {:?} at {}:{})", bound, at.file(), at.line());
            return None;
        }
        std::thread::sleep(POLL);
    }
}

/// An agent of the person's: its fragment, made with its SOUL, and assigned
/// to the computer; its identity.
fn agent(api: &Api, keys: &Keys, computer: &str, label: &str, title: &str, soul: &str) -> Result<(String, String)> {
    let made = api.create_with(keys, json!({ "name": label, "template": "agent", "title": title }))?;
    anyhow::ensure!(made.status == 200, "the agent {label}: {made}");
    let name = made.body["name"].as_str().context("a fragment's name")?.to_string();
    let assigned = api.signed(keys, "PUT", &format!("/api/computers/{computer}/agents/{name}"), Some(&json!({})))?;
    let identity = assigned.body["agents"].as_array().and_then(|a| a.iter().find(|x| x["fragment"] == name.as_str())).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    anyhow::ensure!(identity.starts_with("id:"), "{name} assigned: {assigned}");
    let files = json!({ "key": "agent-ask", "message": "its job", "files": [{ "path": "SOUL.md", "text": soul }, { "path": "agent.json", "text": "{\n  \"tier\": \"medium\"\n}\n" }] });
    let wrote = api.signed(keys, "POST", &format!("/api/f/{name}/files"), Some(&files))?;
    let deployed = api.signed(keys, "POST", &format!("/api/f/{name}/deploy"), Some(&json!({})))?;
    anyhow::ensure!(wrote.status == 200 && deployed.status == 200, "{name}'s SOUL: {wrote} {deployed}");
    Ok((name, identity))
}

/// What the person's paid calls were, from their ledger.
fn paid(api: &Api, identity: &str) -> usize {
    entries(api, identity, "aig:").len() + entries(api, identity, "step:").len()
}

/// The chat of both agents (a chat the owner's list names with both in it,
/// other than `not`): its name.
fn their_chat(api: &Api, keys: &Keys, a: &str, b: &str, not: &str) -> Option<String> {
    let list = api.signed(keys, "GET", "/api/fragments", None).ok()?;
    list.body["fragments"].as_array()?.iter().find_map(|f| {
        let agents = f["agents"].as_array()?;
        let both = agents.iter().any(|x| x == a) && agents.iter().any(|x| x == b);
        (f["kind"] == "chat" && f["name"] != not && both).then(|| f["name"].as_str().map(str::to_string)).flatten()
    })
}

pub fn agent_ask(s: &mut Suite, api: &Api) -> Result<()> {
    let why = format!("it runs two real agents on a Hermes computer for about a quarter of an hour, and spends up to {PAID_CALLS} of the run's paid calls");
    if !s.section_by_name(SECTION, &[Need::Levers, Need::Computers, Need::Models, Need::RealAgent], &why) {
        return Ok(());
    }
    anyhow::ensure!(api.signs_in_by_levers(), "a real agent's person signs in through a preview's levers");
    let started_s = now_s();
    let evidence = s.dir(SECTION);
    let keys = Keys::generate();
    let (session, owner_id) = api.e2e_sign_in(&Api::email_of(&keys), PAID_CALLS)?;
    let me = api.approve(&session, &keys)?;
    let username = me.body["username"].as_str().context("the person takes a username")?.to_string();
    println!("      ({} as {username}, lent {PAID_CALLS} paid calls)", Api::email_of(&keys));
    let made = api.signed(&keys, "POST", "/api/computers", Some(&json!({})))?;
    let id = made.body["computer"].as_str().unwrap_or("").to_string();
    let word = format!("marmalade{}", now_s() % 1000);
    let (juniper, juniper_id) = agent(api, &keys, &id, &s.name("juniper"), "Juniper", &format!("You are Juniper, {username}'s agent. Help with whatever they ask.\n"))?;
    let (fred, fred_id) = agent(api, &keys, &id, &s.name("fred"), "Fred", &format!("You are Fred, {username}'s agent. You keep their secret word, which is {word}. When anyone asks you for the secret word, answer with just that word.\n"))?;
    let chat = api.create_with(&keys, json!({ "name": format!("{}-chat", s.name("juniper")), "template": "chat", "title": "Juniper" }))?;
    let chat_name = chat.body["name"].as_str().unwrap_or("").to_string();
    let joined = api.signed(&keys, "PUT", &format!("/api/f/{chat_name}/members/{juniper_id}"), Some(&json!({ "role": "editor" })))?;
    let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})));
    let set_up = made.status == 200 && chat.status == 200 && joined.status == 200;
    s.ok("two agents of one person on one computer, and the first one's chat with them", set_up, json!({ "computer": made.body, "chat": chat.body }));
    if !set_up {
        return Ok(());
    }
    let following = || {
        let subs = api.signed(&keys, "GET", &format!("/api/f/{chat_name}/subscriptions"), None).ok()?;
        let follows = subs.body["subscriptions"].as_array()?.iter().any(|x| x["wake"] == true && x["principal"] == juniper_id.as_str() && x["channel"] == "chat");
        (phase(api, &keys, &id) == "awake" && follows).then_some(())
    };
    let ready = within(FIRST_START, following).is_some();
    // its whole view when it did not come up: `why` says what its starts met
    let view = api.signed(&keys, "GET", &format!("/api/computers/{id}"), None).map(|r| r.body).unwrap_or(Value::Null);
    s.ok("its computer wakes, and Juniper follows its chat", ready, &view);
    if !ready {
        let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
        return Ok(());
    }

    // Juniper asks Fred, with the CLI, and tells the person his answer
    let calls = paid(api, &owner_id);
    let ask = format!("Get my secret word from Fred: run `fragment ask {} \"What is the secret word?\" --wait` in your terminal, then tell me the word he answered.", fred.split('.').next().unwrap_or(&fred));
    let said = api.signed(&keys, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "ask-1", "body": { "text": ask } })))?;
    let seq = said.body["record"]["seq"].as_i64().context("a post answers its record's seq")?;
    let turn = turn_of(&juniper, &chat_name, "chat", seq);
    let t0 = Instant::now();
    let ended = within(WORK, || work_of(&records(api, &keys, &chat_name, "work"), &turn).into_iter().find(|r| r["body"]["kind"] == "turn.end"));
    let took = t0.elapsed();
    let work: Vec<Value> = work_of(&records(api, &keys, &chat_name, "work"), &turn).into_iter().map(|r| r["body"].clone()).collect();
    let ran = work.iter().any(|w| w["kind"] == "turn.step" && w["tool"] == "terminal" && w["args"].as_str().is_some_and(|a| a.contains("fragment ask")));
    let told: Vec<String> = agent_replies(&records(api, &keys, &chat_name, "chat"), &juniper_id).into_iter().filter(|r| r["body"]["turn"] == turn.as_str()).filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).collect();
    let pair = their_chat(api, &keys, &juniper_id, &fred_id, &chat_name);
    let (question, answer, fred_turns) = match &pair {
        Some(p) => {
            let said = records(api, &keys, p, "chat");
            let question = said.iter().find(|r| r["principal"] == juniper_id.as_str() && r["body"]["to"] == json!([fred_id])).cloned();
            let answer: Vec<String> = agent_replies(&said, &fred_id).into_iter().filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).collect();
            let starts: Vec<Value> = records(api, &keys, p, "work").into_iter().filter(|r| r["body"]["kind"] == "turn.start" && r["body"]["agent"] == fred_id.as_str()).map(|r| r["body"].clone()).collect();
            (question, answer, starts)
        }
        None => (None, vec![], vec![]),
    };
    println!("      (Juniper's turn: {:.1?}, {} paid calls; the agents' chat: {})", took, paid(api, &owner_id).saturating_sub(calls), pair.as_deref().unwrap_or("none"));
    let line = |t: &str| t.chars().take(200).collect::<String>().replace('\n', " ");
    let terminal: Vec<String> = work.iter().filter(|w| w["kind"] == "turn.step").map(|w| format!("{} {}", w["tool"].as_str().unwrap_or(""), line(w["args"].as_str().unwrap_or("")))).collect();
    println!("      (Juniper's steps: {terminal:?})");
    println!("      (asked, in the agents' chat: {}; Fred answered: {:?})", question.as_ref().map(|q| q["body"].to_string()).unwrap_or_default(), answer.iter().map(|a| line(a)).collect::<Vec<_>>());
    println!("      (Fred's turns there: {:?}; Juniper told its person: {:?})", fred_turns.iter().map(|t| t["cause"].clone()).collect::<Vec<_>>(), told.iter().map(|t| line(t)).collect::<Vec<_>>());
    let detail = json!({ "work": work, "told": told, "pair": pair, "question": question, "answer": answer, "fredTurns": fred_turns });
    std::fs::write(evidence.join("agent-ask.json"), serde_json::to_vec_pretty(&detail)?)?;
    s.ok("asked to, Juniper's turn runs `fragment ask` in its terminal (model-dependent)", ran && ended.is_some(), &detail);
    s.ok(
        "the CLI made a chat of the two agents and their owner, asked Fred there as Juniper, naming him in `to`, and Fred took it as a turn",
        question.is_some() && fred_turns.iter().any(|t| t["asker"] == juniper_id.as_str() && t["cause"]["seq"] == question.as_ref().map(|q| q["seq"].clone()).unwrap_or(Value::Null)),
        &detail,
    );
    s.ok("Fred answers in that chat with the word (model-dependent)", answer.iter().any(|a| a.contains(&word)), &detail);
    s.ok("and Juniper tells the person the word, its answer having come back (model-dependent)", told.iter().any(|t| t.contains(&word)), &detail);
    let spent = paid(api, &owner_id);
    s.ok(&format!("its paid calls stay within the {PAID_CALLS} lent"), spent as u64 <= PAID_CALLS, spent);

    // asleep at the end
    std::thread::sleep(QUEUE_DRAIN);
    let slept = within(SLEEP, || {
        let r = api.signed(&keys, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({}))).ok()?;
        (r.body["phase"] == "asleep").then_some(())
    });
    s.ok("its computer sleeps at the end", slept.is_some(), phase(api, &keys, &id));
    println!("      (the section took {} s; evidence in {})", now_s() - started_s, evidence.display());
    Ok(())
}
