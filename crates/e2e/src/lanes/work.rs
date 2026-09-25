//! Phase 7, slice C: the new chat, in the `chat` section. A chat from the
//! template is two channels and no app code. Its agent posts a turn's
//! progress to `work` (a start naming who asked, one record per tool call,
//! an end) and its answer to `chat`, naming the turn; viewers read the
//! work, strangers do not; a replayed step posts nothing new; the turn's
//! starter stops it from the chat, and no one else does. The page the
//! platform serves (`__chat.js`) groups a turn's steps above its answer,
//! shows the working line while it runs, and a Stop button to its starter
//! only. A chat made before (with `say`) still gets its answers.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_fakes::openrouter::Reply;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::agents::{chat_records, runs_of, said_by, say, settle, site_say, view};
use super::app::ship;
use super::signin::site_cookie;
use super::templates::person;
use crate::api::{Api, Reply as Answer};
use crate::browser::{Browser, Page};
use crate::Suite;

/// A chat as the template made one before phase 7: a `say` operation.
const OLD_CHAT_APP: &[u8] = include_bytes!("../../fixtures/old_chat.mjs");
const OLD_CHAT_JSON: &[u8] = include_bytes!("../../fixtures/old_chat.json");
const LIST: &str = "platform__list_fragments";

fn channel(api: &Api, keys: &Keys, chat: &str, name: &str) -> Result<Answer> {
    api.signed(keys, "GET", &format!("/api/f/{chat}/channels/{name}?after=0"), None)
}

fn work_records(api: &Api, keys: &Keys, chat: &str) -> Vec<Value> {
    channel(api, keys, chat, "work").ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default()
}

/// The work records of one turn, and their kinds in order.
fn of_turn(work: &[Value], turn: &str) -> (Vec<Value>, Vec<String>) {
    let mine: Vec<Value> = work.iter().filter(|r| r["body"]["turn"] == turn).cloned().collect();
    let kinds = mine.iter().map(|r| r["body"]["kind"].as_str().unwrap_or("").to_string()).collect();
    (mine, kinds)
}

/// The answer an agent posted with `text`, if it has.
fn answer(api: &Api, keys: &Keys, chat: &str, agent: &str, text: &str) -> Option<Value> {
    chat_records(api, keys, chat).into_iter().find(|r| r["principal"] == agent && r["body"]["text"] == text)
}

pub(super) fn work(s: &mut Suite, api: &Api) -> Result<()> {
    let mut agents = s.agents()?;
    let wait = Duration::from_secs(30);
    let (owner, owner_session) = person(api)?;
    let (guest, guest_session) = person(api)?;
    let stranger = api.person()?;
    let (owner_id, guest_id) = (api.identity(&owner)?, api.identity(&guest)?);
    let chat = s.named(api, &owner, "work-chat")?;
    let made = api.create_with(&owner, json!({ "name": chat, "template": "chat" }))?;
    anyhow::ensure!(made.status == 200, "a chat from the template: {made}");
    s.hook(api, &made.body);
    let link = format!("fragview={}", made.body["viewToken"].as_str().unwrap_or(""));
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat}/members/{guest_id}"), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "the guest joins: {r}");
    // the owner's own agent joins the chat made from the template
    let agent_of = || -> Option<String> {
        let members = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None).ok()?;
        members.body["members"].as_array()?.iter().find(|m| m["kind"] == "agent").and_then(|m| m["principal"].as_str().map(str::to_string))
    };
    let listening = || api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None).map_or(0, |r| r.body["subscriptions"].as_array().map_or(0, Vec::len));
    let joined = s.eventually(wait, || agent_of().is_some() && listening() == 1);
    let agent = agent_of().unwrap_or_default();
    s.ok("(the owner's agent joins a chat made from the template, and listens)", joined, &agent);
    let test = |agents: &Api, controls: Value| agents.signed(&owner, "POST", "/api/a/agent/test", Some(&controls));

    // a turn that calls a tool: its progress on `work`, its answer on `chat`
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![(LIST.into(), json!({}))]), Reply::Text("You have a chat.".into())]);
    say(api, &owner, &chat, "w1", "what do I have?")?;
    let turn_of = |text: &str| answer(api, &owner, &chat, &agent, text).and_then(|a| a["body"]["turn"].as_str().map(str::to_string));
    let answered = s.eventually(wait, || turn_of("You have a chat.").is_some());
    let turn = turn_of("You have a chat.").unwrap_or_default();
    let ended = s.eventually(wait, || of_turn(&work_records(api, &owner, &chat), &turn).1.last().is_some_and(|k| k == "turn.end"));
    let (records, kinds) = of_turn(&work_records(api, &owner, &chat), &turn);
    let step = records.iter().find(|r| r["body"]["kind"] == "turn.step").map(|r| r["body"].clone()).unwrap_or_default();
    s.ok(
        "a tool-using turn posts its progress to work: its start (who asked), its tool call (the tool, ok, a short excerpt), its end",
        answered
            && ended
            && kinds == ["turn.start", "turn.step", "turn.end"]
            && records.iter().all(|r| r["principal"] == agent.as_str())
            && records[0]["body"]["asker"] == owner_id.as_str()
            && step["tool"] == LIST
            && step["step"] == 1
            && step["ok"] == true
            && step["excerpt"].as_str().is_some_and(|e| !e.is_empty() && e.chars().count() <= fragment_core::work::EXCERPT_MAX_CHARS)
            && records[2]["body"]["outcome"] == "idle",
        json!({ "turn": turn, "work": records }),
    );
    let answers: Vec<Value> = chat_records(api, &owner, &chat).into_iter().filter(|r| r["principal"] == agent.as_str()).collect();
    let step_at = records.get(1).and_then(|r| r["at"].as_i64()).unwrap_or(i64::MAX);
    s.ok(
        "one answer to the message, on chat, naming its turn, after its steps",
        answers.len() == 1 && answers[0]["body"]["turn"] == turn.as_str() && answers[0]["at"].as_i64().is_some_and(|at| at >= step_at),
        json!(answers),
    );

    // who reads the work: a viewer does; on a public chat, a stranger reads
    // the chat and not the work
    let theirs = of_turn(&work_records(api, &guest, &chat), &turn).1;
    s.ok("a viewer reads the work", theirs == kinds && !theirs.is_empty(), json!(theirs));
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat}/visibility"), Some(&json!({ "visibility": "public" })))?;
    anyhow::ensure!(r.status == 200, "making the chat public: {r}");
    let (read_chat, read_work) = (channel(api, &stranger, &chat, "chat")?, channel(api, &stranger, &chat, "work")?);
    s.ok("a stranger reads a public chat, and not its work (403)", read_chat.status == 200 && read_work.status == 403, format!("{read_chat} | {read_work}"));
    // who posts: link holders (viewers) do; anyone else does not
    let without = site_say(api, &chat, "anon-1", "no link", None)?;
    let with = site_say(api, &chat, "anon-2", "with the link", Some(&link))?;
    let signed = say(api, &stranger, &chat, "s1", "a stranger")?;
    let posted: Vec<String> = chat_records(api, &owner, &chat).iter().filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).collect();
    s.ok(
        "an anonymous visitor without the link cannot post (401: sign in), nor a signed-in stranger (403); a link holder can",
        without.status == 401 && signed.status == 403 && with.status == 200 && !posted.iter().any(|t| t == "no link" || t == "a stranger"),
        format!("{without} | {signed} | {with}"),
    );
    let r = api.signed(&stranger, "POST", &format!("/api/f/{chat}/channels/work"), Some(&json!({ "id": "x", "body": { "kind": "turn.end", "turn": turn } })))?;
    let g = api.signed(&guest, "POST", &format!("/api/f/{chat}/channels/work"), Some(&json!({ "id": "x", "body": { "kind": "turn.end", "turn": turn } })))?;
    s.ok("only editors post to work (the owner and their agent)", r.status == 403 && g.status == 403, format!("{r} | {g}"));

    page(s, api, &mut agents, &chat, (&owner, &owner_session), (&guest, &guest_session))?;

    // a step replayed after a crash posts nothing new: the node dies while
    // the tool runs; the watchdog replays the call (by its id) and the turn
    // posts its start, its one step, and its end, once each
    test(&agents, json!({ "hold_in_tool_ms": 4000, "watchdog_ms": 3000 }))?;
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![(LIST.into(), json!({}))]), Reply::Text("Once, after a crash.".into())]);
    let before = runs_of(&view(&agents, &owner, "agent"), LIST).len();
    say(api, &owner, &chat, "w-replay", "list them, whatever happens")?;
    s.eventually(wait, || runs_of(&view(&agents, &owner, "agent"), LIST).len() > before);
    std::thread::sleep(Duration::from_millis(1000));
    s.crash()?;
    agents = s.agents()?;
    let v = settle(s, &agents, &owner, "agent", Duration::from_secs(60));
    let runs = runs_of(&v, LIST);
    let replayed = runs.len() >= before + 2 && runs[runs.len() - 1] == runs[runs.len() - 2];
    let answered = s.eventually(wait, || turn_of("Once, after a crash.").is_some());
    let replay = turn_of("Once, after a crash.").unwrap_or_default();
    s.eventually(wait, || of_turn(&work_records(api, &owner, &chat), &replay).1.last().is_some_and(|k| k == "turn.end"));
    let (records, kinds) = of_turn(&work_records(api, &owner, &chat), &replay);
    s.ok(
        "a step replayed after a crash posts nothing new: one start, one step, one end",
        replayed && answered && kinds == ["turn.start", "turn.step", "turn.end"],
        json!({ "replayed": replayed, "runs": runs, "work": records }),
    );
    test(&agents, json!({}))?;

    // a chat made before (a `say` operation) still gets its answers there
    let old = s.named(api, &owner, "old-chat")?;
    let c = s.create(api, &owner, &old)?;
    ship(s, &c, OLD_CHAT_APP, OLD_CHAT_JSON);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{old}/members/{agent}"), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "the agent joins the old chat: {r}");
    let r = agents.signed(&owner, "POST", "/api/a/agent/listen", Some(&json!({ "fragment": old })))?;
    anyhow::ensure!(r.status == 200 && r.body["reply"] == "say", "the agent listens to the old chat: {r}");
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Text("Old chats still get answers.".into())]);
    // the deploy lands by the webhook: said again (the same id) until it has
    s.eventually(wait, || api.op(&owner, &old, "say", "o1", json!({ "text": "hello, old chat" })).is_ok_and(|r| r.status == 200));
    let who = [agent.as_str()];
    let answered = s.eventually(wait, || said_by(&chat_records(api, &owner, &old), &who, "Old chats still get answers."));
    let records = chat_records(api, &owner, &old);
    let work = channel(api, &owner, &old, "work")?;
    s.ok(
        "a chat made before, with say, still gets the agent's answer, through say (no turn, no work)",
        answered && records.iter().filter(|r| r["principal"] == agent.as_str()).all(|r| r["body"].get("turn").is_none()) && work.status == 404,
        json!({ "records": records, "work": work.status }),
    );
    Ok(())
}

/// The page, in headless Chrome: the owner's and the guest's, each signed
/// in on the chat's origin.
fn page(s: &mut Suite, api: &Api, agents: &mut Api, chat: &str, (owner, owner_session): (&Keys, &str), (guest, guest_session): (&Keys, &str)) -> Result<()> {
    let wait = Duration::from_secs(30);
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the chat's page (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let url = api.site_url(chat, "");
    let owner_cookie = site_cookie(api, owner_session, chat)?;
    let guest_cookie = site_cookie(api, guest_session, chat)?;
    // one browser context: a page's socket is who it connected as, and its
    // posts are whoever the cookie names when they go
    let sign_in = |chrome: &mut Browser, cookie: &str| chrome.set_cookie(&url, "fragment_site", cookie);
    sign_in(&mut chrome, &owner_cookie)?;
    let mine = chrome.open(&url)?;
    chrome.viewport(&mine, 1100, 820, false)?;
    // as a person's dark system shows it (headless Chrome says light)
    chrome.color_scheme(&mine, "dark")?;
    let ready = chrome.until(&mine, "document.getElementById('say')?.dataset.ready === '1'", wait);
    let dump = |chrome: &mut Browser, page: &Page| chrome.eval(page, "document.getElementById('messages').innerText.slice(-600)").unwrap_or_default();
    let above = "(() => { const g = document.querySelector('details.tools'); const a = [...document.querySelectorAll('.msg.agent')].find(m => m.textContent.includes('You have a chat.')); \
                 return !!g && !!a && !g.open && g.textContent.includes('Worked through 1 step') && !!(g.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_FOLLOWING); })()";
    s.ok("the page shows a turn's steps as one folded group, above its answer", ready && chrome.until(&mine, above, wait), dump(&mut chrome, &mine));

    // mid-turn: held after its tool's result is stored (and posted)
    let hold = |agents: &Api, ms: u64| agents.signed(owner, "POST", "/api/a/agent/test", Some(&json!({ "hold_after_tool_ms": ms })));
    hold(agents, 12_000)?;
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![(LIST.into(), json!({}))]),
        Reply::Text("Listed them again. You have **one chat** and `no apps` yet.".into()),
    ]);
    chrome.eval(&mine, "document.getElementById('text').value = 'list them again'; document.getElementById('say').requestSubmit(); true")?;
    let working = "(() => { const g = [...document.querySelectorAll('details.tools')].pop(); \
                   return !!g && g.open && g.textContent.includes('Working · 1 step') && g.textContent.includes('platform__list_fragments') \
                   && !!document.querySelector('.msg.working') && !document.getElementById('stop').hidden; })()";
    let shown = chrome.until(&mine, working, wait);
    s.ok("mid-turn, the page shows the running group open, the working line, and Stop, to its starter", shown, dump(&mut chrome, &mine));
    chrome.screenshot(&mine, &s.scratch.join("chat-mid-dark.png"))?;
    chrome.color_scheme(&mine, "light")?;
    chrome.screenshot(&mine, &s.scratch.join("chat-mid-light.png"))?;
    chrome.color_scheme(&mine, "dark")?;
    sign_in(&mut chrome, &guest_cookie)?;
    let theirs = chrome.open(&url)?;
    let sees = "!!document.querySelector('.msg.working') && !!document.querySelector('details.tools[open]') && document.getElementById('stop').hidden";
    let seen = chrome.until(&theirs, sees, wait);
    s.ok("someone else in the chat sees the work and the working line, and no Stop", seen && shown, dump(&mut chrome, &theirs));
    chrome.close(theirs)?;
    sign_in(&mut chrome, &owner_cookie)?;
    let done = "(() => { const a = [...document.querySelectorAll('.msg.agent')].find(m => m.textContent.includes('Listed them again.')); const g = [...document.querySelectorAll('details.tools')].pop(); \
                return !!a && !!g && !document.querySelector('.msg.working') && document.getElementById('stop').hidden && g.textContent.includes('Worked through 1 step') \
                && !!(g.compareDocumentPosition(a) & Node.DOCUMENT_POSITION_FOLLOWING) && !!a.querySelector('strong'); })()";
    s.ok("answered: the group says it worked through its step, above the answer (markdown); the working line and Stop are gone", chrome.until(&mine, done, wait), dump(&mut chrome, &mine));
    // the group opened, for the screenshots
    chrome.eval(&mine, "(() => { const g = [...document.querySelectorAll('details.tools')].pop(); if (g) g.open = true; return true; })()")?;
    std::thread::sleep(Duration::from_millis(300));
    chrome.screenshot(&mine, &s.scratch.join("chat-after-dark.png"))?;
    chrome.color_scheme(&mine, "light")?;
    chrome.screenshot(&mine, &s.scratch.join("chat-after-light.png"))?;
    chrome.color_scheme(&mine, "dark")?;
    hold(agents, 0)?;

    // stop: someone else's stop is ignored; the starter's Stop ends the turn
    agents.signed(owner, "POST", "/api/a/agent/test", Some(&json!({ "hold_in_tool_ms": 10_000 })))?;
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![(LIST.into(), json!({}))]), Reply::Text("unused".into())]);
    chrome.eval(&mine, "document.getElementById('text').value = 'take your time'; document.getElementById('say').requestSubmit(); true")?;
    let stoppable = chrome.until(&mine, "!document.getElementById('stop').hidden", wait);
    let turn = chrome.eval(&mine, "document.getElementById('stop').dataset.turn")?.as_str().unwrap_or("").to_string();
    let r = api.signed(guest, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": "guest-stop", "body": { "kind": "stop", "turn": turn } })))?;
    std::thread::sleep(Duration::from_millis(1500));
    let v = view(agents, owner, "agent");
    let ended = |kind_of: &str| work_records(api, owner, chat).iter().any(|w| w["body"]["turn"] == turn.as_str() && w["body"]["kind"] == "turn.end" && w["body"]["outcome"] == kind_of);
    s.ok(
        "a stop posted by someone who did not start the turn is ignored: it runs on",
        stoppable && r.status == 200 && v["active"] == true && !ended("stopped") && !ended("idle"),
        json!({ "stop": r.status, "active": v["active"], "outcome": v["outcome"] }),
    );
    let t0 = Instant::now();
    chrome.eval(&mine, "document.getElementById('stop').click(); true")?;
    let v = settle(s, agents, owner, "agent", wait);
    let took = t0.elapsed();
    let noted = chrome.until(&mine, "!!document.querySelector('.msg.notice.stopped') && document.getElementById('stop').hidden && !document.querySelector('.msg.working')", wait);
    s.ok(
        "its starter's Stop, from the page, ends the turn mid-tool: stopped, said on work, and on the page",
        v["outcome"] == "stopped" && took < Duration::from_secs(5) && s.eventually(wait, || ended("stopped")) && noted,
        format!("{took:?} {} {}", v["outcome"], dump(&mut chrome, &mine)),
    );
    agents.signed(owner, "POST", "/api/a/agent/test", Some(&json!({})))?;
    s.openrouter.clear_script();
    Ok(())
}
