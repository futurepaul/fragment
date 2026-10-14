//! Restart, a person's way back to working (docs/computers.md, "What its
//! owner is told"), on real infrastructure: a branch preview's Hermes
//! computer on Cloudflare Containers, its real model. The hosted lane runs
//! it by name (`cargo xtask e2e --hosted … --only agent-restart`); a local
//! run never has a real agent (needs.rs `RealAgent`): the computers and
//! shell-ui lanes prove the same on the stub, with failing saves.
//!
//! An e2e person gets one agent and its chat. Its first reply comes from a
//! computer awake; once the turn has let go of its keepalive, its owner
//! presses Restart, naming the start they saw: its sleep saves `/data`
//! (held), and it starts again at once, fresh from the image and that save,
//! never a snapshot, and no rollback. The same press again restarts
//! nothing more. Then the agent answers a second message, and nothing is
//! left to tell its owner. Two replies, a few paid calls. It sleeps at the
//! end (`--sweep` deletes the run's `e2e-<run>-` fragments).
//!
//! Given a round's candidate (`--candidate`: docs/computers.md, "Stable and
//! canary"), the restart is an upgrade: the computer, awake on the
//! deployment's default after its first reply (which was given a word to
//! keep), is pinned to the candidate and told an update is ready; the
//! Restart takes it, and the agent names the word in the same chat. Then it
//! is pinned back (a rollback), told again, restarted onto the default, and
//! names the word once more. Three replies.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::computers::{agent_replies, lever, newest_save, notice, phase, turn_of, work_of, QUEUE_DRAIN};
use super::jobs::records;
use super::ledger::entries;
use crate::api::{now_s, Api};
use crate::{Need, Suite};

pub const SECTION: &str = "agent-restart";

/// The paid calls the run lends the section's person: two replies, each a
/// model call or two, and Hermes' title.
const PAID_CALLS: u64 = 10;
/// With a candidate: a third reply, its rollback's.
const PAID_CALLS_ROUND: u64 = 16;
/// A new computer's first start on a preview (the image pulled to the
/// host, Hermes booted), to its agent following its chat.
const FIRST_START: Duration = Duration::from_secs(10 * 60);
/// A turn that answers in words.
const REPLY: Duration = Duration::from_secs(5 * 60);
/// A turn's last model call (Hermes may make one after its reply) to its
/// keepalive let go.
const LET_GO: Duration = Duration::from_secs(3 * 60);
/// A restart: its sleep (the hold, the save, the stop) and its start from
/// the image and the save, to running; its request waits this long for
/// its answer (`Api::patient`).
const RESTART: Duration = Duration::from_secs(8 * 60);
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

/// What the person's paid calls were, from their ledger.
fn paid(api: &Api, identity: &str) -> usize {
    entries(api, identity, "aig:").len() + entries(api, identity, "step:").len()
}

pub fn agent_restart(s: &mut Suite, api: &Api) -> Result<()> {
    let candidate = s.candidate.clone();
    let lent = if candidate.is_some() { PAID_CALLS_ROUND } else { PAID_CALLS };
    let why = format!("it restarts a real agent's Hermes computer on a preview, and spends up to {lent} of the run's paid calls");
    if !s.section_by_name(SECTION, &[Need::Levers, Need::Computers, Need::Models, Need::RealAgent], &why) {
        return Ok(());
    }
    anyhow::ensure!(api.signs_in_by_levers(), "a real agent's person signs in through a preview's levers");
    let started_s = now_s();
    let evidence = s.dir(SECTION);
    let keys = Keys::generate();
    let (session, owner_id) = api.e2e_sign_in(&Api::email_of(&keys), lent)?;
    let me = api.approve(&session, &keys)?;
    anyhow::ensure!(me.status == 200, "an approved key works: {me}");
    println!("      ({}, lent {lent} paid calls)", Api::email_of(&keys));

    // its computer, one agent, and their chat
    let made = api.signed(&keys, "POST", "/api/computers", Some(&json!({})))?;
    let id = made.body["computer"].as_str().unwrap_or("").to_string();
    let label = s.name("rowan");
    let agent = api.create_with(&keys, json!({ "label": label, "template": "agent", "title": "Rowan" }))?;
    let agent_name = agent.body["name"].as_str().unwrap_or("").to_string();
    let assigned = api.signed(&keys, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = assigned.body["agents"].as_array().and_then(|a| a.iter().find(|x| x["fragment"] == agent_name.as_str())).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    let soul = json!({ "key": SECTION, "message": "its job", "files": [{ "path": "SOUL.md", "text": format!("You are Rowan, {}'s agent. Answer in one short sentence.\n", Api::email_of(&keys)) }, { "path": "agent.json", "text": "{\n  \"tier\": \"medium\"\n}\n" }] });
    let wrote = api.signed(&keys, "POST", &format!("/api/f/{agent_name}/files"), Some(&soul))?;
    let deployed = api.signed(&keys, "POST", &format!("/api/f/{agent_name}/deploy"), Some(&json!({})))?;
    let chat = api.create_with(&keys, json!({ "label": format!("{label}-chat"), "template": "chat", "title": "Rowan" }))?;
    let chat_name = chat.body["name"].as_str().unwrap_or("").to_string();
    let joined = api.signed(&keys, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})));
    let set_up = made.status == 200 && agent.status == 200 && identity.starts_with("npub1") && wrote.status == 200 && deployed.status == 200 && chat.status == 200 && joined.status == 200;
    s.ok("a person's computer, an agent on it, and its chat", set_up, json!({ "computer": made.body, "agent": agent.body, "chat": chat.body }));
    if !set_up {
        return Ok(());
    }
    let view = || api.signed(&keys, "GET", &format!("/api/computers/{id}"), None).map(|r| r.body).unwrap_or(Value::Null);
    let following = || {
        let subs = api.signed(&keys, "GET", &format!("/api/f/{chat_name}/subscriptions"), None).ok()?;
        let follows = subs.body["subscriptions"].as_array()?.iter().any(|x| x["wake"] == true && x["principal"] == identity.as_str() && x["channel"] == "chat");
        (phase(api, &keys, &id) == "awake" && follows).then_some(())
    };
    let woken = Instant::now();
    let ready = within(FIRST_START, following).is_some();
    println!("      (first start: following its chat {:.1?} after the wake)", woken.elapsed());
    s.ok("its computer wakes, and the agent follows its chat", ready, view());
    if !ready {
        let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
        return Ok(());
    }

    // one reply before the restart; its turn ends and lets go of the computer
    let say = |n: u32, text: &str| api.signed(&keys, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": format!("restart-{n}"), "body": { "text": text } })));
    let ended = |turn: &str| work_of(&records(api, &keys, &chat_name, "work"), turn).into_iter().any(|r| r["body"]["kind"] == "turn.end");
    let reply_to = |turn: &str| agent_replies(&records(api, &keys, &chat_name, "chat"), &identity).into_iter().find(|r| r["body"]["turn"] == turn).and_then(|r| r["body"]["text"].as_str().map(str::to_string));
    // with a candidate, a word to keep: the same chat names it after the
    // upgrade, and after the rollback
    let word = format!("lantern{}", now_s() % 1000);
    let first = match &candidate {
        Some(_) => say(1, &format!("Hello! Please remember this word for me: {word}. And what's one word for a calm morning?"))?,
        None => say(1, "Hello! What's one word for a calm morning?")?,
    };
    let first_turn = turn_of(&agent_name, &chat_name, "chat", first.body["record"]["seq"].as_i64().unwrap_or(0));
    let asked = Instant::now();
    let replied = within(REPLY, || reply_to(&first_turn)).is_some();
    println!("      (hello to its reply: {:.1?})", asked.elapsed());
    let answered = replied && within(REPLY, || ended(&first_turn).then_some(())).is_some();
    let let_go = answered && within(LET_GO, || lever(api, &id, "saves").ok().filter(|r| r.body["keepalives"] == 0).map(|_| ())).is_some();
    s.ok("the agent answers, as itself, and its turn lets go of the computer", answered && let_go, json!({ "reply": reply_to(&first_turn), "work": work_of(&records(api, &keys, &chat_name, "work"), &first_turn) }));
    if !answered {
        let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
        return Ok(());
    }

    // a round's candidate, pinned while it is awake (as an operator puts a
    // person on the canary): its owner is told an update is ready, and the
    // Restart below is the notice's, the upgrade
    let first_image = made.body["image"].as_str().unwrap_or("").to_string();
    let mut upgraded = None;
    if let Some(image) = &candidate {
        let r = api.signed(&keys, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": image })))?;
        let latest = r.body["latest"].as_str().unwrap_or("").to_string();
        let label = format!("pinned to the candidate, {image}, while awake on {first_image}: its owner is told an update is ready, to the candidate's version");
        if r.status == 200 && r.body["version"] == latest.as_str() {
            // between rounds the canary is stable's digest: nothing to try
            s.skip(&label, &format!("{image} is the version {first_image} runs ({latest}): no upgrade to make, so the restart is a restart"));
        } else {
            let told = within(LET_GO, || (notice(&view(), "update")["version"] == latest.as_str()).then_some(())).is_some();
            s.ok(&label, r.status == 200 && r.body["image"] == image.as_str() && !latest.is_empty() && told, json!({ "pinned": r.body, "view": view() }));
            upgraded = Some(latest);
        }
    }

    // Restart, naming the start its owner saw
    std::thread::sleep(QUEUE_DRAIN);
    let before = view();
    let g = before["generation"].as_u64().unwrap_or(0);
    // its answer comes once it runs again (its save, then its start), as a
    // browser's fetch waits for it
    let patient = api.patient(RESTART);
    let restart = || patient.signed(&keys, "POST", &format!("/api/computers/{id}/restart"), Some(&json!({ "generation": g })));
    let t0 = Instant::now();
    let pressed = restart();
    let answered_in = t0.elapsed();
    let back = || {
        let v = view();
        (v["phase"] == "awake" && v["generation"].as_u64() == Some(g + 1)).then_some(v)
    };
    let came_back = within(RESTART, back);
    let back_in = t0.elapsed();
    let after = came_back.clone().unwrap_or_else(view);
    let newest = newest_save(api, &id);
    let restored = after["restored"].clone();
    println!(
        "      (restart: answered {} in {:.1?}, running again in {:.1?}; restored {})",
        pressed.as_ref().map(|r| r.status.to_string()).unwrap_or_else(|e| format!("nothing ({e:#})")),
        answered_in,
        back_in,
        restored
    );
    s.ok(
        "its owner's restart answers, and it comes back running: a new start",
        pressed.as_ref().is_ok_and(|r| r.status == 200) && came_back.is_some(),
        json!({ "answer": pressed.as_ref().map(|r| r.body.clone()).unwrap_or(Value::Null), "view": after }),
    );
    s.ok(
        "its start restored its newest save (the restart's own, held), from the image, never a snapshot, and no rollback",
        restored["generation"].as_u64() == Some(g + 1)
            && restored["from"] == "backup"
            && restored["save"] == newest["id"]
            && newest["generation"].as_u64() == Some(g)
            && newest["held"] == true
            && restored["rollback"] == false
            && restored["after"] == "sleep",
        json!({ "restored": restored, "newest": newest }),
    );
    let again = restart()?;
    s.ok(
        "the same press again (the same start named) restarts nothing more",
        again.status == 200 && again.body["generation"].as_u64() == Some(g + 1) && again.body["restored"] == restored,
        &again,
    );
    if let (Some(image), Some(latest)) = (&candidate, &upgraded) {
        s.ok(
            &format!("the restart took the update: it runs {image}'s version, its next start the same"),
            after["image"] == image.as_str() && after["version"] == latest.as_str() && after["latest"] == latest.as_str(),
            &after,
        );
    }

    // and the agent answers after it, with nothing left to tell its owner;
    // with a candidate, in the same chat, the word it was given before
    let names_word = |text: Option<String>| text.is_some_and(|t| t.to_lowercase().contains(&word));
    let second = match &candidate {
        Some(_) => say(2, "What was the word I asked you to remember? Answer with just the word.")?,
        None => say(2, "And one word for a quiet evening?")?,
    };
    let second_turn = turn_of(&agent_name, &chat_name, "chat", second.body["record"]["seq"].as_i64().unwrap_or(0));
    let asked = Instant::now();
    let answered = within(REPLY, || reply_to(&second_turn)).is_some();
    println!("      (after the restart, a message to its reply: {:.1?})", asked.elapsed());
    let v = view();
    s.ok(
        "then the agent answers a message, and nothing is left to tell its owner",
        answered && v["generation"].as_u64() == Some(g + 1) && v["notices"].as_array().is_none_or(|l| l.is_empty()),
        json!({ "reply": reply_to(&second_turn), "view": v }),
    );
    if candidate.is_some() {
        s.ok("after the upgrade its chat goes on: it names the word it was given before (model-dependent)", names_word(reply_to(&second_turn)), json!({ "word": word, "reply": reply_to(&second_turn) }));
    }

    // with a candidate, the way back: pinned to the default again (a
    // rollback), told, restarted onto it, and the chat goes on
    let mut rolled_back = Value::Null;
    if let (Some(image), Some(latest)) = (&candidate, &upgraded) {
        let ended_second = within(REPLY, || ended(&second_turn).then_some(())).is_some();
        let let_go = ended_second && within(LET_GO, || lever(api, &id, "saves").ok().filter(|r| r.body["keepalives"] == 0).map(|_| ())).is_some();
        let r = api.signed(&keys, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": first_image })))?;
        let stable = r.body["latest"].as_str().unwrap_or("").to_string();
        let told = within(LET_GO, || (notice(&view(), "update")["version"] == stable.as_str()).then_some(())).is_some();
        s.ok(
            &format!("pinned back to {first_image} from {image} (a rollback): its owner is told again, the version it ran before"),
            let_go && r.status == 200 && r.body["image"] == first_image.as_str() && !stable.is_empty() && stable != *latest && r.body["version"] == latest.as_str() && told,
            json!({ "pinned": r.body, "view": view() }),
        );
        std::thread::sleep(QUEUE_DRAIN);
        let g2 = view()["generation"].as_u64().unwrap_or(0);
        let pressed = patient.signed(&keys, "POST", &format!("/api/computers/{id}/restart"), Some(&json!({ "generation": g2 })));
        let back = within(RESTART, || {
            let v = view();
            (v["phase"] == "awake" && v["generation"].as_u64() == Some(g2 + 1)).then_some(v)
        })
        .unwrap_or_else(view);
        let newest = newest_save(api, &id);
        let restored = &back["restored"];
        s.ok(
            &format!("restarted, it runs {first_image}'s version again, from its newest save (the restart's own, held), and no rollback"),
            pressed.as_ref().is_ok_and(|r| r.status == 200)
                && back["generation"].as_u64() == Some(g2 + 1)
                && back["version"] == stable.as_str()
                && restored["from"] == "backup"
                && restored["save"] == newest["id"]
                && newest["generation"].as_u64() == Some(g2)
                && newest["held"] == true
                && restored["rollback"] == false,
            json!({ "view": back, "newest": newest }),
        );
        let third = say(3, "And the word once more? Just the word.")?;
        let third_turn = turn_of(&agent_name, &chat_name, "chat", third.body["record"]["seq"].as_i64().unwrap_or(0));
        let answered = within(REPLY, || reply_to(&third_turn)).is_some();
        let v = view();
        s.ok(
            "after the rollback its chat goes on: it answers, naming the word again (model-dependent), and nothing is left to tell its owner",
            answered && names_word(reply_to(&third_turn)) && v["notices"].as_array().is_none_or(|l| l.is_empty()),
            json!({ "word": word, "reply": reply_to(&third_turn), "view": v }),
        );
        rolled_back = json!({ "candidate": image, "upgradedTo": latest, "backTo": stable, "view": back });
    }
    let spent = paid(api, &owner_id);
    s.ok(&format!("its paid calls stay within the {lent} lent"), spent as u64 <= lent, spent);
    let detail = json!({ "before": before, "pressed": pressed.as_ref().map(|r| r.body.clone()).unwrap_or(Value::Null), "answeredInMs": answered_in.as_millis() as u64, "backInMs": back_in.as_millis() as u64, "after": after, "again": again.body, "newest": newest, "paid": spent, "round": rolled_back });
    std::fs::write(evidence.join("agent-restart.json"), serde_json::to_vec_pretty(&detail)?)?;

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
