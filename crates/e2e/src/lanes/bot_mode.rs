//! Hermes' Bot Mode with two real agents (docs/computers.md, "Bot Mode in
//! our Hermes image"): on a branch preview, a Hermes computer running two
//! agents of one person, their real model, and Chrome. The hosted lane runs
//! it by name (`cargo xtask e2e --hosted … --only bot-mode`); a local run
//! never has a real agent (needs.rs `RealAgent`): the images' Docker lane
//! runs the same path on the scripted model (`bots_message_each_other`).
//!
//! An e2e person gets two agents, each made as the shell makes one (its
//! fragment, its SOUL, its own chat `<label>-chat`): Juniper, who talks
//! with them, and Maple, who builds apps. Each is said hello to in its own
//! chat, its Bot Chat. Then Juniper is asked to have Maple make and publish
//! a todo app and show it in her browser: Juniper's turn runs
//! `message_agent`; the image posts the message into Maple's chat as
//! Juniper, naming Maple; Maple's turn there makes the app, which goes
//! live, and opens it on her desktop; and her answer comes back to Juniper,
//! who tells the person in her own chat. Both chats and both screens are
//! shot for the evidence (Maple's screen checked to connect). Then the
//! computer sleeps (`--sweep` deletes the run's `e2e-<run>-` fragments).
//!
//! A check that depends on what the model chooses to do says so in its text.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::agent_smoke::{live_app, open_screen};
use super::computers::{agent_replies, phase, turn_of, work_of, QUEUE_DRAIN};
use super::jobs::records;
use super::ledger::entries;
use crate::api::{now_s, Api};
use crate::{Need, Suite};

pub const SECTION: &str = "bot-mode";

/// The paid calls the run lends the section's person: two hellos, Juniper's
/// turn (its message, and its answer once woken), and Maple's (the app made
/// and deployed, her browser opened: a dozen tool calls or so), Hermes'
/// titles and guardian among them.
const PAID_CALLS: u64 = 50;
/// A new computer's first start on a preview, to its agents following.
const FIRST_START: Duration = Duration::from_secs(10 * 60);
/// A turn that answers in words.
const REPLY: Duration = Duration::from_secs(5 * 60);
/// A turn that does things: an app made and deployed, a browser opened.
const WORK: Duration = Duration::from_secs(15 * 60);
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

/// An agent of the person's, made as the shell makes one: its fragment, its
/// SOUL, assigned to the computer, and its own chat with it in it. Its
/// fragment, identity and chat.
fn agent(api: &Api, keys: &Keys, computer: &str, label: &str, title: &str, soul: &str) -> Result<(String, String, String)> {
    let made = api.create_with(keys, json!({ "name": label, "template": "agent", "title": title }))?;
    anyhow::ensure!(made.status == 200, "the agent {label}: {made}");
    let name = made.body["name"].as_str().context("a fragment's name")?.to_string();
    let assigned = api.signed(keys, "PUT", &format!("/api/computers/{computer}/agents/{name}"), Some(&json!({})))?;
    let identity = assigned.body["agents"].as_array().and_then(|a| a.iter().find(|x| x["fragment"] == name.as_str())).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    anyhow::ensure!(identity.starts_with("id:"), "{name} assigned: {assigned}");
    let files = json!({ "key": "bot-mode", "message": "its job", "files": [{ "path": "SOUL.md", "text": soul }, { "path": "agent.json", "text": "{\n  \"tier\": \"medium\"\n}\n" }] });
    let wrote = api.signed(keys, "POST", &format!("/api/f/{name}/files"), Some(&files))?;
    let deployed = api.signed(keys, "POST", &format!("/api/f/{name}/deploy"), Some(&json!({})))?;
    anyhow::ensure!(wrote.status == 200 && deployed.status == 200, "{name}'s SOUL: {wrote} {deployed}");
    let chat = api.create_with(keys, json!({ "name": format!("{label}-chat"), "template": "chat", "title": title }))?;
    let chat_name = chat.body["name"].as_str().context("its chat's name")?.to_string();
    let joined = api.signed(keys, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(chat.status == 200 && joined.status == 200, "{name}'s chat: {chat} {joined}");
    Ok((name, identity, chat_name))
}

/// What the person's paid calls were, from their ledger.
fn paid(api: &Api, identity: &str) -> usize {
    entries(api, identity, "aig:").len() + entries(api, identity, "step:").len()
}

/// The owner's message in `chat`: the turn it starts for `agent`, and its seq.
fn say(api: &Api, keys: &Keys, chat: &str, agent: &str, id: &str, text: &str) -> Result<(String, i64)> {
    let r = api.signed(keys, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": id, "body": { "text": text } })))?;
    anyhow::ensure!(r.status == 200, "the owner's message {id}: {r}");
    let seq = r.body["record"]["seq"].as_i64().context("a post answers its record's seq")?;
    Ok((turn_of(agent, chat, "chat", seq), seq))
}

/// The turn's end on `chat`'s `work`.
fn ended(api: &Api, keys: &Keys, chat: &str, turn: &str) -> Option<Value> {
    work_of(&records(api, keys, chat, "work"), turn).into_iter().map(|r| r["body"].clone()).find(|b| b["kind"] == "turn.end")
}

pub fn bot_mode(s: &mut Suite, api: &Api) -> Result<()> {
    let why = format!("it runs two real agents on a Hermes computer for about half an hour, and spends up to {PAID_CALLS} of the run's paid calls");
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
    let skills = api.create_with(&keys, json!({ "name": s.name("skills"), "template": "skills" }))?;
    let made = api.signed(&keys, "POST", "/api/computers", Some(&json!({})))?;
    let id = made.body["computer"].as_str().unwrap_or("").to_string();
    let (juniper, juniper_id, jchat) = agent(api, &keys, &id, &s.name("juniper"), "Juniper", &format!("You are Juniper, {username}'s agent. You talk with {username} and hand building work to your teammates.\n"))?;
    let (maple, maple_id, mchat) = agent(api, &keys, &id, &s.name("maple"), "Maple", &format!("You are Maple, {username}'s builder. You make and publish small web apps (fragments) with the fragment CLI, and show them in the browser on your desktop.\n"))?;
    let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})));
    let set_up = made.status == 200 && skills.status == 200;
    s.ok("two agents of one person on one computer, each with its own chat, as the shell makes them", set_up, json!({ "computer": made.body, "juniper": juniper, "maple": maple }));
    if !set_up {
        return Ok(());
    }
    let following = |chat: &str, agent: &str| {
        api.signed(&keys, "GET", &format!("/api/f/{chat}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["principal"] == agent && x["channel"] == "chat")))
    };
    let t0 = Instant::now();
    let ready = within(FIRST_START, || (phase(api, &keys, &id) == "awake" && following(&jchat, &juniper_id) && following(&mchat, &maple_id)).then_some(())).is_some();
    println!("      (asleep to both following their chats: {:.1?})", t0.elapsed());
    let view = api.signed(&keys, "GET", &format!("/api/computers/{id}"), None).map(|r| r.body).unwrap_or(Value::Null);
    s.ok("its computer wakes, and each agent follows its own chat", ready, &view);
    if !ready {
        let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
        return Ok(());
    }

    // each agent's first turn in its own chat, its Bot Chat
    for (chat, agent, n) in [(&mchat, &maple, "1"), (&jchat, &juniper, "2")] {
        let (hello, _) = say(api, &keys, chat, agent, &format!("bot-hello-{n}"), "hello")?;
        let end = within(REPLY, || ended(api, &keys, chat, &hello));
        s.ok(&format!("\"hello\" in {agent}'s own chat is answered (model-dependent)"), end.as_ref().is_some_and(|e| e["outcome"] == "idle"), end.unwrap_or(Value::Null));
    }

    // Juniper has Maple make and publish an app, and show it on her desktop
    let calls = paid(api, &owner_id);
    let app_label = s.name("todo");
    let app_name = format!("{app_label}.{username}");
    let ask = format!(
        "Use message_agent to ask Maple to make me a todo app from the todo template called {app_label}, deploy it, and open it in the browser on her desktop. Then tell me what she says when she answers."
    );
    let (turn, asked_at) = say(api, &keys, &jchat, &juniper, "bot-ask", &ask)?;
    let t1 = Instant::now();
    let jend = within(REPLY, || ended(api, &keys, &jchat, &turn));
    let jwork: Vec<Value> = work_of(&records(api, &keys, &jchat, "work"), &turn).into_iter().map(|r| r["body"].clone()).collect();
    // a step's tool is as Hermes' progress line names it (`message_agent...`)
    let messaged = jwork.iter().any(|w| w["kind"] == "turn.step" && w["tool"].as_str().is_some_and(|t| t.starts_with("message_agent")));
    s.ok("asked to, Juniper's turn runs message_agent (model-dependent)", messaged && jend.is_some(), json!({ "work": jwork }));

    // the message, in Maple's own chat, as Juniper's, naming Maple
    let handed = within(REPLY, || records(api, &keys, &mchat, "chat").into_iter().find(|r| r["principal"] == juniper_id.as_str() && r["body"]["to"] == json!([maple_id])));
    println!("      (asked to Juniper's message in Maple's chat: {:.1?})", t1.elapsed());
    s.ok(
        "the message is in Maple's own chat, posted as Juniper and naming Maple, with Hermes' attribution",
        handed.as_ref().is_some_and(|r| r["body"]["text"].as_str().is_some_and(|t| t.starts_with("Message from"))),
        handed.clone().unwrap_or(Value::Null),
    );
    let Some(handed) = handed else {
        let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
        return Ok(());
    };
    let mturn = turn_of(&maple, &mchat, "chat", handed["seq"].as_i64().unwrap_or(0));
    let mend = within(WORK, || ended(api, &keys, &mchat, &mturn));
    let mwork: Vec<Value> = work_of(&records(api, &keys, &mchat, "work"), &mturn).into_iter().map(|r| r["body"].clone()).collect();
    let manswer: Vec<String> = agent_replies(&records(api, &keys, &mchat, "chat"), &maple_id).into_iter().filter(|r| r["body"]["turn"] == mturn.as_str()).filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).collect();
    println!("      (Maple's turn: {:.1?}, steps {:?})", t1.elapsed(), mwork.iter().filter(|w| w["kind"] == "turn.step").map(|w| w["tool"].clone()).collect::<Vec<_>>());
    s.ok("Maple takes it as a turn of hers in her chat, and answers there (model-dependent)", mend.is_some() && !manswer.is_empty(), json!({ "end": mend, "answer": manswer }));
    let live = within(REPLY, || live_app(api, &keys, &app_name));
    s.ok(&format!("the app {app_label} is theirs and live (model-dependent)"), live.is_some(), live.clone().unwrap_or(Value::Null));

    // Juniper says Maple's answer in her own chat, after her turn
    let told = within(REPLY, || {
        let said: Vec<String> = agent_replies(&records(api, &keys, &jchat, "chat"), &juniper_id).into_iter().filter(|r| r["body"]["turn"] != turn.as_str() && r["seq"].as_i64().is_some_and(|q| q > asked_at)).filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).collect();
        said.into_iter().find(|t| t.to_lowercase().contains("maple") || t.contains(&app_label))
    });
    println!("      (asked to Juniper telling the person: {:.1?}; Maple's turn spent {} paid calls with Juniper's)", t1.elapsed(), paid(api, &owner_id).saturating_sub(calls));
    s.ok("Maple's answer comes back to Juniper, who tells the person in her own chat (model-dependent)", told.is_some(), told.clone().unwrap_or_default());

    // both chats, and both screens, as the person sees them
    let mut shots = vec![];
    if let Some(mut b) = s.browser()? {
        // the shell, each agent's chat opened from its sidebar as a person opens it
        b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
        let shell = b.open(&format!("{}/", api.base))?;
        b.viewport(&shell, 1280, 900, false)?;
        let up = b.until(&shell, "!document.getElementById('layout').hidden && document.querySelectorAll('#chats .row').length > 1", Duration::from_secs(90));
        for (chat, title, file) in [(&jchat, "Juniper", "juniper-chat.png"), (&mchat, "Maple", "maple-chat.png")] {
            // its row, by the key the shell gives it (shell.js: `chat:<name>`)
            let row = format!("#chats .row[data-key=\"chat:{chat}\"]");
            let open = format!("(() => {{ const r = document.querySelector({row:?}); if (r) r.click(); return !!r; }})()");
            let opened = up && b.eval(&shell, &open).is_ok_and(|v| v == json!(true));
            std::thread::sleep(Duration::from_secs(10));
            let _ = b.screenshot(&shell, &evidence.join(file));
            s.ok(&format!("the shell opens {title}'s chat from its sidebar"), opened, file);
            shots.push(file.to_string());
        }
        // Maple's screen, where she opened the app, is checked; Juniper's,
        // a desktop she never used (started for its first viewer), only shot
        for (agent, file, checked) in [(&maple, "maple-screen.png", true), (&juniper, "juniper-screen.png", false)] {
            let (page, connected) = open_screen(&mut b, api, &keys, &id, agent)?;
            std::thread::sleep(Duration::from_secs(5));
            let _ = b.screenshot(&page, &evidence.join(file));
            if checked {
                s.ok(&format!("{agent}'s screen connects (its browser shows the app: the evidence's shot)"), connected, file);
            } else {
                println!("      ({agent}'s screen connected: {connected})");
            }
            shots.push(file.to_string());
        }
    } else {
        s.skip("both chats and both screens shot", "no Chrome is installed here (set CHROME_BIN)");
    }
    let detail = json!({ "juniperWork": jwork, "handed": handed, "mapleWork": mwork, "mapleAnswer": manswer, "told": told, "live": live, "shots": shots });
    std::fs::write(evidence.join("bot-mode.json"), serde_json::to_vec_pretty(&detail)?)?;
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
