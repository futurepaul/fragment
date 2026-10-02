//! Agents (phase 5): an agent is a key with goose's loop and its
//! conversations in its own cell. Its tools are the operations of the
//! fragments it belongs to and the platform's verbs, called through the
//! signed API; the model is the OpenRouter fake, scripted. Turns steer,
//! stop, and survive a killed node without running an operation twice (the
//! spike's checks, on the product). An agent acts for whoever asked,
//! capped (ROADMAP decision 17): at the platform and through its turns.

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
        r.status == 200 && agent_npub.starts_with("npub1") && r.body["name"] == full.as_str() && r.body["model"] == fragment_proto::AGENT_MODEL && agent_id.starts_with("id:"),
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
    ];
    s.ok(
        "an agent in no fragment has only the platform's verbs, none of which writes a fragment's files or deploys it",
        r.status == 200 && r.body["tools"] == json!(platform),
        &r,
    );

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

    // it makes a fragment from a template for its owner, as they could
    let label = s.name("counter");
    let app = format!("{label}.{username}");
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__create_fragment".into(), json!({ "label": label, "template": "blank" }))]),
        Reply::Text("Your page is up.".into()),
    ]);
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "make me a blank page" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    s.ok("asked for a page, it makes one from a template and says so", v["outcome"] == "idle" && v["messages"].as_array().and_then(|m| m.last()).is_some_and(|m| m["text"] == "Your page is up."), &v);
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
    s.ok("its page, the template's, is live", page.status == 200 && page.text.contains("A blank fragment"), &page);

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

    // an agent follows more than 16 channels: it listens to a 17th room's
    // chat, and a room deleted is dropped from what it follows as it
    // follows another
    let follow = |s: &Suite, room: &str| -> Result<bool> {
        let c = s.create(api, &owner, room)?;
        s.commit(&c, &[("fragment.json", Some(ROOM_JSON))]);
        s.deploy(&c);
        let r = api.signed(&owner, "PUT", &format!("/api/f/{room}/members/{agent_npub}"), Some(&json!({ "role": "editor" })))?;
        anyhow::ensure!(r.status == 200, "the agent joins {room}: {r}");
        // the deploy lands by the webhook: the same listen, again, until its channel is there
        let listen = || agents.signed(&owner, "POST", &format!("/api/a/{name}/listen"), Some(&json!({ "fragment": room }))).is_ok_and(|r| r.status == 200);
        Ok(s.eventually(wait, listen))
    };
    let follows = |v: &Value| v["listens"]["newest"].as_array().into_iter().flatten().filter_map(|l| l["fragment"].as_str().map(str::to_string)).collect::<Vec<_>>();
    let before = view(&agents, &owner, &name)["listens"]["count"].as_u64().unwrap_or(0);
    let many: Vec<String> = (0..17).map(|i| s.named(api, &owner, &format!("many-{i}"))).collect::<Result<_>>()?;
    let mut joined = true;
    for room in &many {
        joined &= follow(s, room)?;
    }
    let v = view(&agents, &owner, &name);
    s.ok("an agent follows a 17th room", joined && v["listens"]["count"] == before + 17 && many.iter().all(|c| follows(&v).contains(c)), json!({ "count": v["listens"]["count"] }));
    let r = api.signed(&owner, "DELETE", &format!("/api/f/{}", many[0]), None)?;
    let last = s.named(api, &owner, "many-17")?;
    let joined = r.status == 200 && follow(s, &last)?;
    let v = view(&agents, &owner, &name);
    s.ok(
        "a deleted room's listen is dropped when the agent follows another",
        joined && v["listens"]["count"] == before + 17 && !follows(&v).contains(&many[0]) && follows(&v).contains(&last),
        json!({ "count": v["listens"]["count"], "follows": follows(&v) }),
    );
    Ok(())
}
