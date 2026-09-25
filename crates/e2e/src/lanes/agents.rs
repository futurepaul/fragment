//! Agents (phase 5): an agent is a key with goose's loop and its
//! conversations in its own cell. Its tools are the operations of the
//! fragments it belongs to and the platform's verbs, called through the
//! signed API; the model is the OpenRouter fake, scripted. Turns steer,
//! stop, and survive a killed node without running an operation twice (the
//! spike's checks, on the product). An agent acts for whoever asked,
//! capped (ROADMAP decision 17): at the platform, through its turns, and
//! in chats (phase 7, slice A).

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_fakes::openrouter::Reply;
use fragment_nip98::Keys;
use fragment_proto::limits;
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{url_enc, Api, Reply as Answer};
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");
const ROOM_JSON: &[u8] = include_bytes!("../../fixtures/room.json");

pub(super) fn view(agents: &Api, owner: &Keys, name: &str) -> Value {
    agents.signed(owner, "GET", &format!("/api/a/{name}"), None).map(|r| r.body).unwrap_or_default()
}

/// The agent's view once its turn has ended (or the last view seen).
pub(super) fn settle(s: &Suite, agents: &Api, owner: &Keys, name: &str, wait: Duration) -> Value {
    let mut last = Value::Null;
    s.eventually(wait, || {
        last = view(agents, owner, name);
        last["active"] == false && last["driving"] == false
    });
    last
}

pub(super) fn todos(api: &Api, owner: &Keys, name: &str) -> Vec<String> {
    api.op(owner, name, "list", "q", json!({}))
        .ok()
        .and_then(|r| r.body["result"]["todos"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|t| t["text"].as_str().map(str::to_string))
        .collect()
}

pub(super) fn runs_of(v: &Value, tool: &str) -> Vec<String> {
    v["toolRuns"].as_array().into_iter().flatten().filter(|r| r["tool"] == tool).filter_map(|r| r["tool_call_id"].as_str().map(str::to_string)).collect()
}

pub fn agents(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("agents") {
        return Ok(());
    }
    let agents = s.agents()?;
    let owner = api.person()?;
    let name = s.name("bot");
    let wait = Duration::from_secs(30);

    let r = agents.unsigned("POST", "/api/agents", Some(&json!({ "name": name })))?;
    s.ok("an unsigned create is 401", r.status == 401, &r);
    let r = agents.signed(&owner, "POST", "/api/agents", Some(&json!({ "name": name })))?;
    let agent_npub = r.body["npub"].as_str().unwrap_or("").to_string();
    let agent_id = r.body["id"].as_str().unwrap_or("").to_string();
    let owner_id = api.identity(&owner)?;
    let username = api.username(&owner)?;
    let full = format!("{name}.{username}");
    s.ok(
        "the owner makes an agent under their username: its own key, registered as theirs at once",
        r.status == 200 && agent_npub.starts_with("npub1") && r.body["name"] == full.as_str() && r.body["model"] == "z-ai/glm-5.3-flash" && agent_id.starts_with("id:"),
        &r,
    );
    let reg = api.signed(&owner, "GET", &format!("/api/identities/{agent_id}"), None)?;
    s.ok("an agent identity its owner owns", reg.status == 200 && reg.body["kind"] == "agent" && reg.body["owner"] == owner_id.as_str(), &reg);
    let again = agents.signed(&owner, "POST", "/api/agents", Some(&json!({ "name": name })))?;
    s.ok("its owner asking again gets the same agent", again.status == 200 && again.body["replayed"] == true && again.body["id"] == agent_id.as_str(), &again);
    let stranger = api.person()?;
    let r = agents.signed(&stranger, "POST", "/api/agents", Some(&json!({ "name": full })))?;
    s.ok("no one else makes an agent under that username", r.status == 403, &r);
    let r = agents.signed(&stranger, "GET", &format!("/api/a/{full}"), None)?;
    s.ok("only its owner may see it", r.status == 403, &r);
    let r = agents.signed(&Keys::generate(), "GET", &format!("/api/a/{full}"), None)?;
    s.ok("nor a key no one registered", r.status == 401, &r);
    let r = agents.signed(&owner, "GET", &format!("/api/a/{name}/tools"), None)?;
    let platform = [
        "platform__create_fragment",
        "platform__list_fragments",
        "platform__operations",
        "platform__call",
        "platform__list_files",
        "platform__read_file",
        "platform__write_file",
        "platform__append_file",
        "platform__write_files",
        "platform__deploy",
    ];
    s.ok("an agent in no fragment has only the platform's verbs", r.status == 200 && r.body["tools"] == json!(platform), &r);

    // a todo fragment the owner makes, with the agent as an editor
    let todo = s.named(api, &owner, "agent-todo")?;
    let c = s.create(api, &owner, &todo)?;
    ship(s, &c, TODO_APP, TODO_JSON);
    let other = s.named(api, &owner, "agent-other")?;
    let o = s.create(api, &owner, &other)?;
    ship(s, &o, TODO_APP, TODO_JSON);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{todo}/members/{agent_npub}"), Some(&json!({ "role": "editor" })))?;
    s.ok(
        "its key names the agent as a member, and the answer says whose it is",
        r.status == 200 && r.body["principal"] == agent_id.as_str() && r.body["kind"] == "agent" && r.body["owner"] == owner_id.as_str(),
        &r,
    );
    let add = fragment_core::tools::tool_name(&todo, "add_todo").expect("a tool name");
    let r = agents.signed(&owner, "GET", &format!("/api/a/{name}/tools"), None)?;
    let tools: Vec<String> = r.body["tools"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(str::to_string)).collect();
    s.ok("a membership gives it that fragment's operations as tools", tools.contains(&add) && tools.contains(&fragment_core::tools::tool_name(&todo, "list").expect("a tool name")), &r);
    s.ok("and nothing of a fragment it is not in", !tools.iter().any(|t| t.starts_with(&format!("{}__", other.replace('.', "--")))), &r);

    // decision 17 at the platform: an agent acts for whoever asked, capped.
    // A key the owner registered as an agent of theirs signs `for` itself
    // here (the agent above signs in its cell; its turns are checked below).
    let hand = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, &owner) })))?;
    s.ok("the owner registers a second agent of theirs", r.status == 200 && r.body["kind"] == "agent", &r);
    let hand_id = r.body["id"].as_str().unwrap_or("").to_string();
    let stranger_id = api.identity(&stranger)?;
    let acting = |who: &str| format!("for={}", url_enc(who));
    let r = api.signed(&owner, "GET", &format!("/api/f/{todo}/status?{}", acting(&owner_id)), None)?;
    s.ok("a person's request naming someone in `for` is refused", r.status == 403, &r);
    let r = api.signed(&hand, "GET", &format!("/api/f/{other}/status"), None)?;
    s.ok("an agent reaches nothing of an app it is not in, as itself", r.status == 403, &r);
    let r = api.signed(&hand, "GET", &format!("/api/f/{other}/status?{}", acting(&owner_id)), None)?;
    s.ok("for its owner, it reads the owner's app, as an editor at most", r.status == 200 && r.body["role"] == "editor", &r);
    let r = api.signed(&hand, "POST", &format!("/api/f/{other}/ops/add_todo?{}", acting(&owner_id)), Some(&json!({ "id": "for-1", "input": { "text": "for the owner" } })))?;
    s.ok("and changes it", r.status == 200 && todos(api, &owner, &other) == ["for the owner"], &r);
    let r = api.signed(&hand, "GET", &format!("/api/f/{other}/status?{}", acting(&stranger_id)), None)?;
    s.ok("for someone with no role there, nothing", r.status == 403, &r);
    // owner-only actions never go through an agent, whomever it acts for
    let owner_only = [
        ("PUT", format!("/api/f/{other}/members/{stranger_id}"), Some(json!({ "role": "editor" }))),
        ("PUT", format!("/api/f/{other}/visibility"), Some(json!({ "visibility": "public" }))),
        ("POST", format!("/api/f/{other}/rotate"), None),
        ("POST", format!("/api/f/{other}/invites"), Some(json!({ "role": "editor" }))),
        ("DELETE", format!("/api/f/{other}"), None),
    ];
    let refused: Vec<(String, u16)> = owner_only
        .iter()
        .map(|(method, path, body)| (format!("{method} {path}"), api.signed(&hand, method, &format!("{path}?{}", acting(&owner_id)), body.as_ref()).map_or(0, |r| r.status)))
        .collect();
    let st = api.status(&owner, &other)?;
    let members = api.signed(&owner, "GET", &format!("/api/f/{other}/members"), None)?;
    s.ok(
        "an agent never changes members, visibility, links, or invites, nor deletes, even for its owner",
        refused.iter().all(|(_, status)| *status == 403) && st.status == 200 && st.body["visibility"] == "link" && members.body["members"].as_array().map(Vec::len) == Some(1),
        json!({ "refused": refused, "visibility": st.body["visibility"], "members": members.body }),
    );
    // a post to a postable channel is decided as a call is: for its
    // asker, capped (`notes` takes an editor's posts)
    let room = s.named(api, &owner, "agent-room")?;
    let c = s.create(api, &owner, &room)?;
    s.commit(&c, &[("fragment.json", Some(ROOM_JSON))]);
    s.deploy(&c);
    api.signed(&owner, "PUT", &format!("/api/f/{room}/members/{stranger_id}"), Some(&json!({ "role": "viewer" })))?;
    let post = |who: &str, id: &str| api.signed(&hand, "POST", &format!("/api/f/{room}/channels/notes?{}", acting(who)), Some(&json!({ "id": id, "body": { "text": "a note" } })));
    // the deploy lands by the webhook: posted again (the same id) until it has
    s.eventually(wait, || post(&owner_id, "n1").is_ok_and(|r| r.status == 200));
    let for_owner = post(&owner_id, "n1")?;
    let for_viewer = post(&stranger_id, "n2")?;
    s.ok(
        "an agent's post acts for its asker, capped: for the owner, a note, as the agent; for a viewer, none",
        for_owner.status == 200 && for_owner.body["record"]["principal"] == hand_id.as_str() && for_viewer.status == 403,
        json!({ "owner": for_owner.body, "viewer": for_viewer.body }),
    );
    let listed = |r: &Answer, name: &str| r.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == name).map(|f| f["role"].clone());
    let r = api.signed(&hand, "GET", &format!("/api/fragments?{}", acting(&owner_id)), None)?;
    s.ok("listed for its owner: the owner's fragments, each as an editor at most", listed(&r, &todo) == Some(json!("editor")) && listed(&r, &other) == Some(json!("editor")), &r);
    let r = api.signed(&hand, "GET", &format!("/api/fragments?{}", acting(&stranger_id)), None)?;
    s.ok(
        "and for someone else, only what they are in too, at their own role",
        r.status == 200 && r.body["fragments"] == json!([{ "name": room, "role": "viewer" }]),
        &r,
    );

    // a turn: the model calls the operation, then answers
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "milk" }))]), Reply::Text("Added milk.".into())]);
    let calls_before = s.openrouter.calls().len();
    let r = agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "add milk to my list" })))?;
    s.ok("a turn starts", r.status == 200 && r.body["started"] == true, &r);
    // one read that waits in the agent's cell, not a view read twice a second
    let st = agents.signed(&owner, "GET", &format!("/api/a/{name}/state?wait_ms={}", limits::AGENT_STATE_WAIT_MS_MAX), None)?;
    s.ok(
        "one state read waits for the turn to end, and answers its outcome and answer",
        st.status == 200 && st.body["active"] == false && st.body["outcome"] == "idle" && st.body["answer"] == "Added milk.",
        &st,
    );
    let r = agents.signed(&owner, "GET", &format!("/api/a/{name}/state?wait_ms={}", limits::AGENT_STATE_WAIT_MS_MAX + 1), None)?;
    s.ok("a state read waits at most 25 s", r.status == 400, &r);
    let v = settle(s, &agents, &owner, &name, wait);
    let last = v["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or_default();
    s.ok("it ends with the model's answer", v["outcome"] == "idle" && last["role"] == "assistant" && last["text"] == "Added milk.", &v);
    s.ok("the operation ran: the list has milk", todos(api, &owner, &todo) == ["milk"], json!(todos(api, &owner, &todo)));
    let ops = api.signed(&owner, "GET", &format!("/api/f/{todo}/channels/ops"), None)?;
    let by_agent = ops.body["records"].as_array().into_iter().flatten().any(|r| r["body"]["op"] == "add_todo" && r["principal"] == agent_id.as_str());
    s.ok("as the agent (its key, through the signed API)", by_agent, &ops);
    // the owner's org key, minted by the platform's Ledger with their allowance as its limit
    let org = format!("fragment org:{}", owner_id.trim_start_matches("id:"));
    let owners = s.openrouter.minted().into_iter().find(|m| m.name == org).map(|m| format!("Bearer {}", m.key));
    let spent: Vec<String> = s.openrouter.calls()[calls_before..].iter().filter(|c| c.1 == "/api/v1/chat/completions").map(|c| c.3.clone()).collect();
    s.ok(
        "its turn spent its owner's month: every model call carried the owner's own key",
        owners.is_some() && !spent.is_empty() && spent.iter().all(|a| Some(a) == owners.as_ref()),
        format!("{} calls; owner's key minted: {}", spent.len(), owners.is_some()),
    );
    let chats = s.openrouter.chats();
    let schema = chats
        .iter()
        .flat_map(|c| c["tools"].as_array().cloned().unwrap_or_default())
        .find(|t| t["function"]["name"] == add.as_str())
        .map(|t| t["function"]["parameters"].clone());
    s.ok("the model was offered the operation, with its input schema", schema.as_ref().is_some_and(|p| p["type"] == "object"), json!(schema));

    // the owner's turn reaches an app the owner made that the agent is not
    // in (the 403 of 2026-09-25): through the platform's verbs, for the owner
    let marker = format!("marker-{}", &agent_id[3..11]);
    api.signed(&owner, "POST", &format!("/api/f/{other}/files"), Some(&json!({ "files": [{ "path": "notes.txt", "text": marker }] })))?;
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__read_file".into(), json!({ "fragment": other, "path": "notes.txt" }))]),
        Reply::Tools(vec![("platform__call".into(), json!({ "fragment": other, "operation": "add_todo", "input": { "text": "by my agent" } }))]),
        Reply::Text("Added it to your other list.".into()),
    ]);
    let asked = s.openrouter.chats().len();
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "add to my other list" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    let read = s.openrouter.chats().get(asked + 1).is_some_and(|c| c["messages"].to_string().contains(&marker));
    s.ok("the owner's turn reads a file of an app the owner made, which its agent is not in", read && v["outcome"] == "idle", json!({ "outcome": v["outcome"], "error": v["error"] }));
    let ops = api.signed(&owner, "GET", &format!("/api/f/{other}/channels/ops"), None)?;
    let by_agent = ops.body["records"].as_array().into_iter().flatten().any(|r| r["body"]["op"] == "add_todo" && r["principal"] == agent_id.as_str());
    s.ok("and calls its operation, as the agent", todos(api, &owner, &other).iter().any(|t| t == "by my agent") && by_agent, &ops);

    // it makes an app for its owner: a fragment, its page, a deploy
    let label = s.name("counter");
    let app = format!("{label}.{username}");
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__create_fragment".into(), json!({ "label": label, "template": "blank" }))]),
        Reply::Tools(vec![("platform__write_files".into(), json!({ "fragment": app, "files": [{ "path": "site/index.html", "text": "<h1>Counter, by an agent</h1>" }] }))]),
        Reply::Tools(vec![("platform__deploy".into(), json!({ "fragment": app, "note": "first" }))]),
        Reply::Text("Your counter is up.".into()),
    ]);
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "make me a counter app" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    s.ok("asked for an app, it makes one and says so", v["outcome"] == "idle" && v["messages"].as_array().and_then(|m| m.last()).is_some_and(|m| m["text"] == "Your counter is up."), &v);
    let mine = api.signed(&owner, "GET", "/api/fragments", None)?;
    s.ok(
        "the app is its owner's, under their username",
        mine.body["fragments"].as_array().is_some_and(|a| a.iter().any(|f| f["name"] == app.as_str() && f["role"] == "owner")),
        &mine,
    );
    let members = api.signed(&owner, "GET", &format!("/api/f/{app}/members"), None)?;
    s.ok("and the agent is its editor", members.body["members"].as_array().is_some_and(|a| a.iter().any(|m| m["principal"] == agent_id.as_str() && m["role"] == "editor")), &members);
    let st = api.status(&owner, &app)?;
    let page = api.page(&app, "", Some(&format!("fragview={}", st.body["viewToken"].as_str().unwrap_or(""))))?;
    s.ok("its page, written and deployed by the agent, is live", page.status == 200 && page.text.contains("Counter, by an agent"), &page);

    // steer mid-tool: the message waits for the tool, then joins the turn
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "hold_in_tool_ms": 3000 })))?;
    s.openrouter.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "eggs" }))]), Reply::Text("Added eggs and noted bread.".into())]);
    let before = runs_of(&view(&agents, &owner, &name), &add).len();
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "add eggs" })))?;
    s.eventually(wait, || runs_of(&view(&agents, &owner, &name), &add).len() > before);
    let r = agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "and bread" })))?;
    s.ok("a message during a turn steers it", r.body["steered"] == true, &r);
    let v = settle(s, &agents, &owner, &name, wait);
    let steered = v["messages"].as_array().into_iter().flatten().any(|m| m["steer"] == true && m["text"] == "and bread");
    let seen = s.openrouter.chats().last().is_some_and(|c| c["messages"].to_string().contains("and bread"));
    s.ok("the steer joins the conversation after the tool, and the model reads it", steered && seen && v["outcome"] == "idle", &v);

    // stop: a held tool call ends at once, interrupted
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "hold_in_tool_ms": 8000 })))?;
    s.openrouter.script(&[Reply::Tools(vec![(fragment_core::tools::tool_name(&todo, "list").expect("a tool name"), json!({}))]), Reply::Text("unused".into())]);
    let list = fragment_core::tools::tool_name(&todo, "list").expect("a tool name");
    let before = runs_of(&view(&agents, &owner, &name), &list).len();
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "read my list slowly" })))?;
    s.eventually(wait, || runs_of(&view(&agents, &owner, &name), &list).len() > before);
    let v = view(&agents, &owner, &name);
    s.ok("a new turn drops the steers the model already read", v["steer"] == json!([]), &v["steer"]);
    let t0 = Instant::now();
    let r = agents.signed(&owner, "POST", &format!("/api/a/{name}/stop"), None)?;
    let v = settle(s, &agents, &owner, &name, wait);
    let took = t0.elapsed();
    s.ok("stop ends a turn mid-tool, well before the tool would", r.status == 200 && v["outcome"] == "stopped" && took < Duration::from_secs(4), format!("{took:?} {}", v["outcome"]));
    s.openrouter.clear_script();

    // a kill after the operation ran, before its result was saved: the
    // watchdog replays the call with the same id, and it runs once
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "hold_in_tool_ms": 4000, "watchdog_ms": 3000 })))?;
    s.openrouter.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "once" }))]), Reply::Text("Added it once.".into())]);
    let before = runs_of(&view(&agents, &owner, &name), &add).len();
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "add once" })))?;
    s.eventually(wait, || runs_of(&view(&agents, &owner, &name), &add).len() > before);
    std::thread::sleep(Duration::from_millis(1000));
    s.crash()?;
    let agents = s.agents()?;
    let v = settle(s, &agents, &owner, &name, Duration::from_secs(60));
    let runs = runs_of(&v, &add);
    let replayed = runs.len() >= before + 2 && runs[runs.len() - 1] == runs[runs.len() - 2];
    s.ok("killed mid-tool, the watchdog replays the call by its id", replayed && v["watchdogRestarts"].as_u64() >= Some(1) && v["outcome"] == "idle", &v);
    let list = todos(api, &owner, &todo);
    s.ok("and the operation ran once", list.iter().filter(|t| *t == "once").count() == 1, json!(list));

    // a kill after the tool's result was saved: nothing runs again
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "hold_after_tool_ms": 4000, "watchdog_ms": 3000 })))?;
    s.openrouter.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "between" }))]), Reply::Text("Done between.".into())]);
    let tools_steps = |v: &Value| v["steps"].as_array().into_iter().flatten().filter(|st| st["step"] == "tools").count();
    let steps_before = tools_steps(&view(&agents, &owner, &name));
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "add between" })))?;
    s.eventually(wait, || tools_steps(&view(&agents, &owner, &name)) > steps_before);
    s.crash()?;
    let agents = s.agents()?;
    let v = settle(s, &agents, &owner, &name, Duration::from_secs(60));
    let list = todos(api, &owner, &todo);
    let calls = runs_of(&v, &add);
    let last_call_once = calls.last().is_some_and(|last| calls.iter().filter(|c| *c == last).count() == 1);
    s.ok("killed between steps, the turn resumes and nothing runs again", v["outcome"] == "idle" && last_call_once && list.iter().filter(|t| *t == "between").count() == 1, &v);

    // a bounded conversation: each step loads the newest messages, cut at
    // a turn's start, so what the model is sent stops growing with the
    // agent's age (a window of 6 here, set by the test controls, which the
    // history above already outgrew)
    let window = 6;
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "window_messages": window })))?;
    let list = fragment_core::tools::tool_name(&todo, "list").expect("a tool name");
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![(list.clone(), json!({}))]), Reply::Text("Your list is long.".into())]);
    let asked = s.openrouter.chats().len();
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "what is on my list?" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    let chats = s.openrouter.chats();
    let sent: Vec<usize> = chats.iter().skip(asked).map(|c| c["messages"].as_array().map_or(0, |m| m.len())).collect();
    let stored = v["messages"].as_array().map_or(0, |m| m.len());
    let last = v["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or_default();
    s.ok(
        "past the window, each model request carries at most the window (and the system prompt), and the turn ends with its answer",
        stored > 2 * window && sent.len() == 2 && sent.iter().all(|n| *n <= window + 1) && v["outcome"] == "idle" && last["text"] == "Your list is long.",
        json!({ "stored": stored, "sent": sent, "outcome": v["outcome"], "last": last["text"] }),
    );
    let oldest_sent = chats.iter().skip(asked).any(|c| c["messages"].to_string().contains("add milk to my list"));
    s.ok("the oldest turns stay out of the request", !oldest_sent, "");

    // a turn that alone outgrows the window ends in an error that says so;
    // the agent is not broken: the next message starts a turn that fits
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "window_messages": 2 })))?;
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![(list.clone(), json!({}))]), Reply::Text("unused".into())]);
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "read it again" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    s.ok("a turn longer than the window ends in an error that says so", v["outcome"] == "error" && v["error"].as_str().is_some_and(|e| e.contains("outgrew")), json!({ "outcome": v["outcome"], "error": v["error"] }));
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Text("Still here.".into())]);
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "are you there?" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    let last = v["messages"].as_array().and_then(|m| m.last()).cloned().unwrap_or_default();
    s.ok("and the next message starts a turn that fits", v["outcome"] == "idle" && last["text"] == "Still here.", json!({ "outcome": v["outcome"], "error": v["error"], "last": last["text"] }));

    // the owner's view is bounded: the newest rows of each list (4 here,
    // set by the test controls; the product's is 256)
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "view_rows": 4 })))?;
    let v = view(&agents, &owner, &name);
    let lens: Vec<usize> = ["messages", "toolRuns", "steps"].iter().map(|k| v[*k].as_array().map_or(usize::MAX, Vec::len)).collect();
    let newest = v["messages"].as_array().and_then(|m| m.last()).map(|m| m["text"].clone());
    s.ok(
        "the view shows the newest rows of each list, and no more than its bound",
        lens == [4, 4, 4] && newest == Some(json!("Still here.")),
        json!({ "lens": lens, "newest": newest }),
    );
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({})))?;
    s.openrouter.clear_script();

    // an agent follows more than 16 chats: the owner's own agent joins each
    // chat made from the template, the 17th too; and a chat deleted is
    // dropped from what it follows
    let many: Vec<String> = (0..17).map(|i| s.named(api, &owner, &format!("many-{i}"))).collect::<Result<_>>()?;
    for chat in &many {
        let r = api.create_with(&owner, json!({ "name": chat, "template": "chat" }))?;
        anyhow::ensure!(r.status == 200, "a chat from the template: {r}");
    }
    let listening = |chat: &str| api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None).map_or(0, |r| r.body["subscriptions"].as_array().map_or(0, Vec::len));
    let default = view(&agents, &owner, "agent");
    let joined = s.eventually(wait, || listening(&many[16]) == 1);
    let follows = |v: &Value| v["listens"]["newest"].as_array().into_iter().flatten().filter_map(|l| l["fragment"].as_str().map(str::to_string)).collect::<Vec<_>>();
    let v = view(&agents, &owner, "agent");
    s.ok("the owner's agent follows a 17th chat", joined && v["listens"]["count"] == 17 && many.iter().all(|c| follows(&v).contains(c)), json!({ "count": v["listens"]["count"], "name": default["name"] }));
    let r = api.signed(&owner, "DELETE", &format!("/api/f/{}", many[0]), None)?;
    let last = s.named(api, &owner, "many-17")?;
    let made = api.create_with(&owner, json!({ "name": last, "template": "chat" }))?;
    let joined = r.status == 200 && made.status == 200 && s.eventually(wait, || listening(&last) == 1);
    let v = view(&agents, &owner, "agent");
    s.ok(
        "a deleted chat's listen is dropped when the agent follows another",
        joined && v["listens"]["count"] == 17 && !follows(&v).contains(&many[0]) && follows(&v).contains(&last),
        json!({ "count": v["listens"]["count"], "follows": follows(&v) }),
    );
    Ok(())
}

/// A message in a chat, as `keys`: a post to its `chat` channel (the chat
/// template's chats have no `say` since phase 7, slice C).
pub(super) fn say(api: &Api, keys: &Keys, chat: &str, id: &str, text: &str) -> Result<Answer> {
    api.signed(keys, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": id, "body": { "text": text } })))
}

/// A message from a browser (`fragment.post`), with its cookie.
pub(super) fn site_say(api: &Api, chat: &str, id: &str, text: &str, cookie: Option<&str>) -> Result<Answer> {
    api.call(crate::api::Call {
        method: "POST",
        url: api.site_url(chat, "__op/channels/chat"),
        body: Some(json!({ "id": id, "input": { "text": text } }).to_string().into_bytes()),
        content_type: Some("application/json"),
        cookie: cookie.map(str::to_string),
        ..crate::api::Call::default()
    })
}

pub(super) fn chat_records(api: &Api, owner: &Keys, chat: &str) -> Vec<Value> {
    api.signed(owner, "GET", &format!("/api/f/{chat}/channels/chat"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default()
}

pub(super) fn said_by(records: &[Value], who: &[&str], text: &str) -> bool {
    records.iter().any(|r| who.contains(&r["principal"].as_str().unwrap_or("")) && r["body"]["text"] == text)
}

/// Phase 7's chat: the chat template, an agent that follows its channel and
/// answers there, and a chat that works an app through the agent.
/// Driven the way a person would, with the CLI (`fragment agent`).
pub fn chat(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("chat") {
        return Ok(());
    }
    let agents = s.agents()?;
    // the CLI finds the agent fleet here (the suite's children inherit it)
    std::env::set_var("FRAGMENT_AGENTS", &agents.base);
    let home = s.dir("chat-home");
    s.login(api, &home);
    let owner = s.cli_keys(&home).expect("the CLI logged in");
    let wait = Duration::from_secs(30);

    let dir = s.dir("chat").join("room");
    let dir_s = dir.to_str().expect("utf-8 path").to_string();
    let out = s.cli(api, &home, &["new", &dir_s, "--template", "chat"]);
    let room = s.cli_json(api, &home, &["create", &s.name("chat"), "--json"])?;
    let chat = room["name"].as_str().unwrap_or("").to_string();
    s.hook(api, &room);
    let deployed = s.cli(api, &home, &["deploy", &chat, "--dir", &dir_s]);
    s.ok("the chat template scaffolds and deploys", out.status.success() && deployed.status.success(), String::from_utf8_lossy(&deployed.stderr));
    let bot = s.name("chatbot");
    let made = s.cli_json(api, &home, &["agent", "create", &bot, "--json"])?;
    let bot_npub = made["npub"].as_str().unwrap_or("").to_string();
    let bot_id = made["id"].as_str().unwrap_or("").to_string();
    s.ok("fragment agent create makes an agent and registers it", bot_npub.starts_with("npub1") && bot_id.starts_with("id:"), &made);
    s.cli(api, &home, &["members", "add", &chat, &bot_npub, "--role", "editor"]);
    let r = s.cli_json(api, &home, &["agent", "listen", &bot, &chat, "--json"]);
    s.ok("the agent listens to the chat (a subscription on its channel)", r.as_ref().is_ok_and(|v| v["channel"] == "chat"), format!("{r:?}"));
    let subs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    s.ok("the owner sees the agent's subscription", subs.body["subscriptions"].as_array().is_some_and(|a| a.len() == 1 && a[0]["principal"] == bot_id.as_str()), &subs);
    // listening again (a retry, or a chat's alarm joining its owner's agent
    // once more) is the same listen: its inbox, and one subscription
    let again = s.cli_json(api, &home, &["agent", "listen", &bot, &chat, "--json"]);
    let resubs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    let same = matches!((&r, &again), (Ok(first), Ok(then)) if first["subscription"] == then["subscription"]);
    s.ok("listening again leaves the one subscription", same && resubs.body["subscriptions"] == subs.body["subscriptions"], json!({ "again": format!("{again:?}"), "subscriptions": resubs.body }));
    // the agent decodes a delivery whole: one whose record has no seq is
    // refused, never heard under the key `…/null` forever after
    let inbox = subs.body["subscriptions"][0]["url"].as_str().unwrap_or("").to_string();
    let deliver = |record: Value| -> Result<(u16, String)> {
        let body = json!({ "type": "record", "fragment": chat, "channel": "chat", "record": record });
        let resp = reqwest::blocking::Client::new().post(&inbox).header("content-type", "application/json").body(body.to_string()).send()?;
        Ok((resp.status().as_u16(), resp.text()?))
    };
    let (status, text) = deliver(json!({ "channel": "chat", "at": 1, "principal": owner.pubkey_hex(), "kind": "say", "body": { "text": "no seq" } }))?;
    s.ok("a delivery whose record has no seq is 400", status == 400 && text.contains("seq"), &text);
    let stranger = api.person()?;
    let r = api.signed(&stranger, "POST", &format!("/api/f/{chat}/subscriptions"), Some(&json!({ "channel": "chat", "url": "http://127.0.0.1:9/x" })))?;
    s.ok("someone who is not a member cannot subscribe", r.status == 403, &r);

    // a message: the agent answers in the chat, once. The answer is posted
    // for it, so no reply operation is among its tools (the template's chat
    // has none since phase 7; a chat made before has `say`); a model that
    // calls one anyway (from habit, or from an earlier turn) is told no
    // such tool is here, and its answer still lands once
    let say_tool = fragment_core::tools::tool_name(&chat, "say").expect("a tool name");
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![(say_tool.clone(), json!({ "text": "Hello from the tool." }))]), Reply::Text("Hello! I'm here.".into())]);
    let asked = s.openrouter.chats().len();
    say(api, &owner, &chat, "c1", "hi bot")?;
    let who = [bot_id.as_str()];
    let answered = s.eventually(wait, || said_by(&chat_records(api, &owner, &chat), &who, "Hello! I'm here."));
    s.ok("a message in the chat gets the agent's answer there, as the agent", answered, json!(chat_records(api, &owner, &chat)));
    let offered: Vec<String> = s.openrouter.chats().get(asked).and_then(|c| c["tools"].as_array().cloned()).unwrap_or_default().iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect();
    s.ok("the chat's reply operation is not among the agent's tools", offered.iter().any(|t| t == "platform__create_fragment") && !offered.contains(&say_tool), json!(offered));
    // the answer comes after anything the turn did, so it is all there now
    let records = chat_records(api, &owner, &chat);
    let texts: Vec<&str> = records.iter().filter_map(|r| r["body"]["text"].as_str()).collect();
    s.ok("the chat gains the message and one answer", texts == ["hi bot", "Hello! I'm here."], json!(records));
    // a sentinel through the same subscription: the owner's next message is
    // delivered after the agent's own answer, so once the agent has answered
    // it, it has heard its own and let it be (a turn for it would have asked
    // the model once more)
    s.openrouter.script(&[Reply::Text("Still here.".into())]);
    say(api, &owner, &chat, "c1b", "still there?")?;
    let sentinel = s.eventually(wait, || said_by(&chat_records(api, &owner, &chat), &who, "Still here."));
    let records = chat_records(api, &owner, &chat);
    let turns = s.openrouter.chats().len() - asked;
    s.ok("the agent does not answer itself", sentinel && records.len() == 4 && turns == 3, json!({ "model requests": turns, "records": records }));

    // a chat that works an app: the agent is also in a todo list
    let todo = s.named(api, &owner, "chat-todo")?;
    let c = s.create(api, &owner, &todo)?;
    ship(s, &c, TODO_APP, TODO_JSON);
    api.signed(&owner, "PUT", &format!("/api/f/{todo}/members/{bot_id}"), Some(&json!({ "role": "editor" })))?;
    s.openrouter.script(&[Reply::Tools(vec![(fragment_core::tools::tool_name(&todo, "add_todo").expect("a tool name"), json!({ "text": "bread" }))]), Reply::Text("Added bread to your list.".into())]);
    say(api, &owner, &chat, "c2", "please add bread to my todo list")?;
    let done = s.eventually(wait, || said_by(&chat_records(api, &owner, &chat), &who, "Added bread to your list."));
    s.ok("asked in the chat, the agent changes the todo list through its operation, and says so", done && todos(api, &owner, &todo) == ["bread"], json!(chat_records(api, &owner, &chat)));

    // the owner talks to the agent directly, from the CLI (-v logs each request)
    s.openrouter.script(&[Reply::Text("Just bread.".into())]);
    let out = s.cli(api, &home, &["-v", "agent", "say", &bot, "what is on my list?", "--json"]);
    let said: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let log = String::from_utf8_lossy(&out.stderr).to_string();
    s.ok("fragment agent say waits for the answer", said["ok"] == true && said["data"]["answer"] == "Just bread.", &said);
    let reads = log.lines().filter(|l| l.starts_with("GET /api/a/")).count();
    s.ok("with one read of the turn's state, which waits in the agent's cell", reads == 1, &log);

    // removing the agent from the chat ends its subscription
    s.cli(api, &home, &["members", "rm", &chat, &bot_npub]);
    let subs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    s.ok("a member removed loses its subscriptions", subs.body["subscriptions"] == json!([]), &subs);
    let before = chat_records(api, &owner, &chat).len();
    let asked = s.openrouter.chats().len();
    say(api, &owner, &chat, "c3", "anyone there?")?;
    // the sentinel is the same pipe opened again: the agent back in the chat,
    // listening anew, answers the next message; a delivery of the one
    // before would have reached it first, and started a turn with it
    s.cli(api, &home, &["members", "add", &chat, &bot_npub, "--role", "editor"]);
    let relisten = s.cli_json(api, &home, &["agent", "listen", &bot, &chat, "--json"]);
    s.openrouter.script(&[Reply::Text("Back again.".into())]);
    say(api, &owner, &chat, "c4", "are you back?")?;
    let back = s.eventually(wait, || said_by(&chat_records(api, &owner, &chat), &who, "Back again."));
    let heard = s.openrouter.chats()[asked..].iter().any(|c| c["messages"].to_string().contains("anyone there?"));
    let after = chat_records(api, &owner, &chat);
    s.ok("and hears nothing more", relisten.is_ok() && back && !heard && after.len() == before + 3, json!({ "heard": heard, "records": after }));
    // out again, so the page's message below goes unanswered
    s.cli(api, &home, &["members", "rm", &chat, &bot_npub]);

    let bot = Bot { name: &bot, id: &bot_id, npub: &bot_npub };
    chats_apart(s, api, &agents, &home, &owner, &bot, &todo)?;

    // the page: the conversation so far, and a message sent from it (by a
    // link holder, who is a viewer)
    if let Some(mut chrome) = s.browser()? {
        let page = chrome.open(&api.site_url(&chat, &format!("?view={}", room["viewToken"].as_str().unwrap_or(""))))?;
        let shows = chrome.until(&page, "document.getElementById('messages')?.textContent.includes('Added bread to your list.')", wait);
        let seen = chrome.eval(&page, "location.href + ' | ' + document.title + ' | ' + (document.body?.innerText || '').slice(0, 300)").unwrap_or_default();
        s.ok("the chat page shows the conversation", shows, seen);
        let ready = chrome.until(&page, "document.getElementById('say')?.dataset.ready === '1'", wait);
        chrome.eval(&page, "document.getElementById('text').value = 'from the page'; document.getElementById('say').requestSubmit(); true")?;
        let landed = ready && s.eventually(wait, || chat_records(api, &owner, &chat).iter().any(|r| r["body"]["text"] == "from the page"));
        s.ok("a message sent from the page lands in the channel, and shows", landed && chrome.until(&page, "document.getElementById('messages').textContent.includes('from the page')", wait), "");
    } else {
        s.ok("Chrome is installed for the chat page (set CHROME_BIN)", false, "no Chrome found");
    }
    std::env::remove_var("FRAGMENT_AGENTS");
    super::work::work(s, api)?;
    super::build::build(s, api)
}

/// The agent a chat lane drives: its name, identity, and key.
struct Bot<'a> {
    name: &'a str,
    id: &'a str,
    npub: &'a str,
}

/// A chat for `bot`, as the chat lane makes one with the CLI (the chat
/// template scaffolded in a folder of its own, created, and deployed), the
/// agent an editor that listens. Answers its name and its create answer.
fn open_chat(s: &Suite, api: &Api, agents: &Api, home: &std::path::Path, owner: &Keys, bot: &Bot, label: &str) -> Result<(String, Value)> {
    let dir = s.dir(&format!("chat-{label}")).join("room");
    let dir = dir.to_str().expect("utf-8 path");
    let scaffolded = s.cli(api, home, &["new", dir, "--template", "chat"]);
    anyhow::ensure!(scaffolded.status.success(), "scaffold {label}: {}", String::from_utf8_lossy(&scaffolded.stderr));
    let room = s.cli_json(api, home, &["create", &s.name(label), "--json"])?;
    let name = room["name"].as_str().unwrap_or("").to_string();
    s.hook(api, &room);
    let deployed = s.cli(api, home, &["deploy", &name, "--dir", dir]);
    anyhow::ensure!(deployed.status.success(), "deploy {name}: {}", String::from_utf8_lossy(&deployed.stderr));
    let r = api.signed(owner, "PUT", &format!("/api/f/{name}/members/{}", bot.npub), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "the agent joins {name}: {r}");
    let r = agents.signed(owner, "POST", &format!("/api/a/{}/listen", bot.name), Some(&json!({ "fragment": name })))?;
    anyhow::ensure!(r.status == 200, "the agent listens to {name}: {r}");
    Ok((name, room))
}

/// The names of the tools the model was offered in a request.
fn offered(chat: &Value) -> Vec<String> {
    chat["tools"].as_array().into_iter().flatten().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect()
}

/// Phase 7, slice A, in chats: one conversation per chat, answered there;
/// a turn acts for whoever started it (ROADMAP decision 17); only its
/// starter steers it; an anonymous visitor's message starts nothing.
/// `todo` is an app of the owner's the agent is an editor of.
fn chats_apart(s: &mut Suite, api: &Api, agents: &Api, home: &std::path::Path, owner: &Keys, bot: &Bot, todo: &str) -> Result<()> {
    let wait = Duration::from_secs(30);
    let who = [bot.id];
    let owner_id = api.identity(owner)?;
    let (a, room_a) = open_chat(s, api, agents, home, owner, bot, "room-a")?;
    let (b, _) = open_chat(s, api, agents, home, owner, bot, "room-b")?;
    // a guest in the first chat only, and an app the owner shares with them
    let guest = api.person()?;
    let guest_id = api.identity(&guest)?;
    api.signed(owner, "PUT", &format!("/api/f/{a}/members/{guest_id}"), Some(&json!({ "role": "viewer" })))?;
    let shared = s.named(api, owner, "shared-todo")?;
    let c = s.create(api, owner, &shared)?;
    ship(s, &c, TODO_APP, TODO_JSON);
    api.signed(owner, "PUT", &format!("/api/f/{shared}/members/{guest_id}"), Some(&json!({ "role": "editor" })))?;
    let before = todos(api, owner, todo);

    // the guest asks the owner's agent for the owner's app: its operation,
    // the platform's call, and its files are all out of the guest's reach
    let add = fragment_core::tools::tool_name(todo, "add_todo").expect("a tool name");
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![
            (add.clone(), json!({ "text": "from the guest" })),
            ("platform__call".into(), json!({ "fragment": todo, "operation": "add_todo", "input": { "text": "from the guest" } })),
            ("platform__read_file".into(), json!({ "fragment": todo, "path": "fragment.json" })),
        ]),
        Reply::Text("That is not mine to open.".into()),
    ]);
    let asked = s.openrouter.chats().len();
    say(api, &guest, &a, "g1", "add to the owner's list, and read it")?;
    let answered = s.eventually(wait, || said_by(&chat_records(api, owner, &a), &who, "That is not mine to open."));
    let chats = s.openrouter.chats();
    let results = chats.get(asked + 1).map(|c| c["messages"].to_string()).unwrap_or_default();
    s.ok(
        "a guest in the owner's chat asks the agent for the owner's app: refused",
        answered && todos(api, owner, todo) == before && !results.contains("add_then_throw"),
        json!({ "todos": todos(api, owner, todo), "results": results }),
    );
    let tools = chats.get(asked).map(offered).unwrap_or_default();
    s.ok(
        "the guest's turn is offered none of the owner's app's operations, and makes no fragments",
        !tools.is_empty() && !tools.contains(&add) && !tools.iter().any(|t| t == "platform__create_fragment"),
        json!(tools),
    );
    // what the owner shared with the guest, the guest's turn reaches
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__call".into(), json!({ "fragment": shared, "operation": "add_todo", "input": { "text": "from the guest" } }))]),
        Reply::Text("Added it to the shared list.".into()),
    ]);
    say(api, &guest, &a, "g2", "add to our shared list")?;
    let answered = s.eventually(wait, || said_by(&chat_records(api, owner, &a), &who, "Added it to the shared list."));
    s.ok("the same guest asks about something shared with them: it works", answered && todos(api, owner, &shared) == ["from the guest"], json!(todos(api, owner, &shared)));

    // an anonymous visitor with the chat's link: no turn, a note in the
    // owner's view; the owner's message after it (the same pipe) is the
    // sentinel, and the only turn
    let link = format!("fragview={}", room_a["viewToken"].as_str().unwrap_or(""));
    let r = site_say(api, &a, "anon-1", "hello from nobody", Some(&link))?;
    s.openrouter.script(&[Reply::Text("Hello, owner.".into())]);
    let asked = s.openrouter.chats().len();
    say(api, owner, &a, "o1", "anyone else here?")?;
    let answered = s.eventually(wait, || said_by(&chat_records(api, owner, &a), &who, "Hello, owner."));
    let turns: Vec<String> = s.openrouter.chats()[asked..].iter().map(|c| c["messages"].to_string()).collect();
    let v = view(agents, owner, bot.name);
    let noted = v["ignored"].as_array().into_iter().flatten().any(|i| i["fragment"] == a.as_str() && i["principal"].as_str().is_some_and(|p| p.starts_with("anon:")));
    s.ok(
        "an anonymous link holder's message starts no turn, and the owner's view notes it",
        r.status == 200 && answered && turns.len() == 1 && !turns[0].contains("hello from nobody") && noted,
        json!({ "said": r.status, "turns": turns.len(), "ignored": v["ignored"] }),
    );

    // a message from chat B during chat A's turn: its own turn, in B's
    // conversation, answered in B; A's turn never reads it
    let hold = |ms: u64| agents.signed(owner, "POST", &format!("/api/a/{}/test", bot.name), Some(&json!({ "hold_in_tool_ms": ms })));
    hold(5000)?;
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__list_fragments".into(), json!({}))]),
        Reply::Text("Answer in A.".into()),
        Reply::Text("Answer in B.".into()),
    ]);
    let asked = s.openrouter.chats().len();
    let runs = |v: &Value| runs_of(v, "platform__list_fragments").len();
    let ran = runs(&view(agents, owner, bot.name));
    say(api, owner, &a, "o2", "a question for room A")?;
    s.eventually(wait, || runs(&view(agents, owner, bot.name)) > ran);
    say(api, owner, &b, "o3", "a question for room B")?;
    // it waits for a turn of its own while A's runs (held in its tool)
    let waiting = |conv: &str, asker: &str| {
        let v = view(agents, owner, bot.name);
        v["waiting"].as_array().into_iter().flatten().any(|w| w["conversation"] == conv && w["asker"] == asker)
    };
    let queued = s.eventually(Duration::from_secs(4), || waiting(&format!("{b}/chat"), &owner_id));
    let in_a = s.eventually(wait, || said_by(&chat_records(api, owner, &a), &who, "Answer in A."));
    let in_b = s.eventually(wait, || said_by(&chat_records(api, owner, &b), &who, "Answer in B."));
    let (records_a, records_b) = (chat_records(api, owner, &a), chat_records(api, owner, &b));
    s.ok(
        "a message from chat B during chat A's turn is answered in B, and only there",
        queued && in_a && in_b && !said_by(&records_a, &who, "Answer in B.") && !said_by(&records_b, &who, "Answer in A."),
        json!({ "queued": queued, "a": records_a, "b": records_b }),
    );
    let turns: Vec<String> = s.openrouter.chats()[asked..].iter().map(|c| c["messages"].to_string()).collect();
    let a_turn: Vec<&String> = turns.iter().filter(|m| m.contains("a question for room A")).collect();
    let b_turn: Vec<&String> = turns.iter().filter(|m| m.contains("a question for room B")).collect();
    s.ok(
        "A's turn never reads B's message, and B's conversation holds nothing of A's",
        a_turn.len() == 2 && a_turn.iter().all(|m| !m.contains("a question for room B")) && b_turn.len() == 1 && !b_turn[0].contains("room A") && !b_turn[0].contains("Answer in A."),
        json!({ "requests": turns.len(), "a": a_turn.len(), "b": b_turn.len() }),
    );

    // only the turn's starter steers it: the owner's next message joins the
    // owner's turn; the guest's waits for a turn of its own
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__list_fragments".into(), json!({}))]),
        Reply::Text("The owner's answer.".into()),
        Reply::Text("The guest's answer.".into()),
    ]);
    let asked = s.openrouter.chats().len();
    let ran = runs(&view(agents, owner, bot.name));
    say(api, owner, &a, "o4", "the owner starts")?;
    s.eventually(wait, || runs(&view(agents, owner, bot.name)) > ran);
    say(api, owner, &a, "o5", "the owner adds this")?;
    say(api, &guest, &a, "g3", "the guest cuts in")?;
    let queued = s.eventually(Duration::from_secs(4), || waiting(&format!("{a}/chat"), &guest_id));
    let owners = s.eventually(wait, || said_by(&chat_records(api, owner, &a), &who, "The owner's answer."));
    let guests = s.eventually(wait, || said_by(&chat_records(api, owner, &a), &who, "The guest's answer."));
    let turns: Vec<String> = s.openrouter.chats()[asked..].iter().map(|c| c["messages"].to_string()).collect();
    let v = view(agents, owner, bot.name);
    let message = |text: &str| v["messages"].as_array().into_iter().flatten().find(|m| m["text"].as_str().is_some_and(|t| t.contains(text))).cloned().unwrap_or_default();
    s.ok(
        "only the turn's starter steers it: the owner's next message joins the owner's turn, the guest's gets its own",
        queued
            && owners
            && guests
            && turns.len() == 3
            && turns[1].contains("the owner adds this")
            && !turns[1].contains("the guest cuts in")
            && turns[2].contains("the guest cuts in")
            && message("the owner adds this")["steer"] == true
            && message("the guest cuts in")["steer"] != true,
        json!({ "queued": queued, "requests": turns.len(), "steer": message("the owner adds this"), "guest": message("the guest cuts in") }),
    );
    hold(0)?;
    s.openrouter.clear_script();
    Ok(())
}
