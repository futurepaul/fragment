//! Agents (phase 5): an agent is a key with goose's loop and its
//! conversations in its own cell. Its tools are the operations of the
//! fragments it belongs to and the platform's verbs, called through the
//! signed API; its model calls go through the platform's model route to the
//! Workers AI fake, scripted, and its owner's ledger pays. Turns steer,
//! stop, and survive a killed node without running an operation twice (the
//! spike's checks, on the product). An agent acts for whoever asked,
//! capped (decision R17): at the platform and through its turns.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_fakes::workers_ai::Reply;
use fragment_nip98::Keys;
use fragment_proto::limits;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::records;
use crate::api::{url_enc, Api, Reply as Answer};
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");
const ROOM_JSON: &[u8] = include_bytes!("../../fixtures/room.json");
/// A chat from before postable channels: its `chat` takes no posts, and
/// `say` publishes there.
const SAID_APP: &[u8] = include_bytes!("../../fixtures/said.mjs");
const SAID_JSON: &[u8] = include_bytes!("../../fixtures/said.json");

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
    if !s.section("agents", &[crate::Need::Fakes, crate::Need::Node, crate::Need::Levers]) {
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
        r.status == 200 && agent_npub.starts_with("npub1") && r.body["name"] == full.as_str() && r.body["model"] == fragment_proto::DEFAULT_TIER.as_str() && agent_id.starts_with("id:"),
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
    // sharing is its owner's, and theirs for them (Paul, 2026-10-04: the
    // delegation section shares): for anyone else it shares nothing, and
    // even for its owner it never deletes a fragment nor sets its cap
    let sharing = [
        ("PUT", format!("/api/f/{other}/members/{stranger_id}"), Some(json!({ "role": "editor" }))),
        ("PUT", format!("/api/f/{other}/visibility"), Some(json!({ "visibility": "public" }))),
        ("POST", format!("/api/f/{other}/rotate"), None),
        ("POST", format!("/api/f/{other}/invites"), Some(json!({ "role": "editor" }))),
    ];
    let owner_only = [("DELETE", format!("/api/f/{other}"), None), ("PUT", format!("/api/f/{other}/cap"), Some(json!({ "id": "agent-cap", "micros": 1 })))];
    let ask = |asker: &str, (method, path, body): &(&str, String, Option<Value>)| {
        (format!("{method} {path} for {asker}"), api.signed(&hand, method, &format!("{path}?{}", acting(asker)), body.as_ref()).map_or(0, |r| r.status))
    };
    let refused: Vec<(String, u16)> = sharing.iter().map(|r| ask(&stranger_id, r)).chain(owner_only.iter().map(|r| ask(&owner_id, r))).collect();
    let st = api.status(&owner, &other)?;
    let members = api.signed(&owner, "GET", &format!("/api/f/{other}/members"), None)?;
    s.ok(
        "an agent shares nothing of its owner's acting for anyone else, and never deletes nor sets a cap, even for its owner",
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
    // each row's name and role (its face, kind and title, rides along)
    let roles: Vec<Value> = r.body["fragments"].as_array().into_iter().flatten().map(|f| json!({ "name": f["name"], "role": f["role"] })).collect();
    s.ok("and for someone else, only what they are in too, at their own role", r.status == 200 && roles == vec![json!({ "name": room, "role": "viewer" })], &r);

    // a turn: the model calls the operation, then answers
    s.ai.clear_script();
    s.ai.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "milk" }))]), Reply::Text("Added milk.".into())]);
    let calls_before = s.ai.calls().len();
    let metered = || {
        let r = api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": owner_id, "op": "entries", "prefix": "aig:" })));
        r.map(|r| r.body["entries"].as_array().map_or(0, Vec::len)).unwrap_or(0)
    };
    let metered_before = metered();
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
    // each model call went through the model route, metered on the owner's ledger
    let calls = s.ai.calls()[calls_before..].to_vec();
    let paid = metered() - metered_before;
    s.ok(
        "its turn's model calls went through the model route on its tier, each metered on its owner's ledger, its agent an opaque id",
        calls.len() == 2
            && paid == 2
            && calls.iter().all(|c| c.model == "@cf/zai-org/glm-5.3-flash" && c.metadata["agent_id"].as_str().is_some_and(|a| a.len() == 16) && c.affinity.as_deref() == c.metadata["agent_id"].as_str()),
        format!("{} calls, {paid} metered: {calls:?}", calls.len()),
    );

    let chats = s.ai.chats();
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
    s.ai.clear_script();
    s.ai.script(&[
        Reply::Tools(vec![("platform__read_file".into(), json!({ "fragment": other, "path": "notes.txt" }))]),
        Reply::Tools(vec![("platform__call".into(), json!({ "fragment": other, "operation": "add_todo", "input": { "text": "by my agent" } }))]),
        Reply::Text("Added it to your other list.".into()),
    ]);
    let asked = s.ai.chats().len();
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "add to my other list" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    let read = s.ai.chats().get(asked + 1).is_some_and(|c| c["messages"].to_string().contains(&marker));
    s.ok("the owner's turn reads a file of an app the owner made, which its agent is not in", read && v["outcome"] == "idle", json!({ "outcome": v["outcome"], "error": v["error"] }));
    let ops = api.signed(&owner, "GET", &format!("/api/f/{other}/channels/ops"), None)?;
    let by_agent = ops.body["records"].as_array().into_iter().flatten().any(|r| r["body"]["op"] == "add_todo" && r["principal"] == agent_id.as_str());
    s.ok("and calls its operation, as the agent", todos(api, &owner, &other).iter().any(|t| t == "by my agent") && by_agent, &ops);

    // it makes a fragment from a template for its owner, as they could
    let label = s.name("counter");
    let app = format!("{label}.{username}");
    s.ai.clear_script();
    s.ai.script(&[
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
    s.ai.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "eggs" }))]), Reply::Text("Added eggs and noted bread.".into())]);
    let before = runs_of(&view(&agents, &owner, &name), &add).len();
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "add eggs" })))?;
    s.eventually(wait, || runs_of(&view(&agents, &owner, &name), &add).len() > before);
    let r = agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "and bread" })))?;
    s.ok("a message during a turn steers it", r.body["steered"] == true, &r);
    let v = settle(s, &agents, &owner, &name, wait);
    let steered = v["messages"].as_array().into_iter().flatten().any(|m| m["steer"] == true && m["text"] == "and bread");
    let seen = s.ai.chats().last().is_some_and(|c| c["messages"].to_string().contains("and bread"));
    s.ok("the steer joins the conversation after the tool, and the model reads it", steered && seen && v["outcome"] == "idle", &v);

    // stop: a held tool call ends at once, interrupted
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "hold_in_tool_ms": 8000 })))?;
    s.ai.script(&[Reply::Tools(vec![(fragment_core::tools::tool_name(&todo, "list").expect("a tool name"), json!({}))]), Reply::Text("unused".into())]);
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
    s.ai.clear_script();

    // a kill after the operation ran, before its result was saved: the
    // watchdog replays the call with the same id, and it runs once
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "hold_in_tool_ms": 4000, "watchdog_ms": 3000 })))?;
    s.ai.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "once" }))]), Reply::Text("Added it once.".into())]);
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
    s.ai.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "between" }))]), Reply::Text("Done between.".into())]);
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
    s.ai.clear_script();
    s.ai.script(&[Reply::Tools(vec![(list.clone(), json!({}))]), Reply::Text("Your list is long.".into())]);
    let asked = s.ai.chats().len();
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "what is on my list?" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    let chats = s.ai.chats();
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
    s.ai.clear_script();
    s.ai.script(&[Reply::Tools(vec![(list.clone(), json!({}))]), Reply::Text("unused".into())]);
    agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "read it again" })))?;
    let v = settle(s, &agents, &owner, &name, wait);
    s.ok("a turn longer than the window ends in an error that says so", v["outcome"] == "error" && v["error"].as_str().is_some_and(|e| e.contains("outgrew")), json!({ "outcome": v["outcome"], "error": v["error"] }));
    s.ai.clear_script();
    s.ai.script(&[Reply::Text("Still here.".into())]);
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
    s.ai.clear_script();

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
    reply_operation(s, api, &owner, &name, &agent_npub, &agent_id, &stranger)
}

/// A conversation's row in the agent's view (`None` before it has ended a turn).
fn conversation(v: &Value, conv: &str) -> Option<Value> {
    v["conversations"].as_array().into_iter().flatten().find(|c| c["conversation"] == conv).cloned()
}

/// The reply operation: a chat from before postable channels (`said.json`:
/// its `chat` takes no posts, and `say` publishes there) gets its answers
/// through the operation its listen names (agent/src/lib.rs `post_answer`).
///
/// Valid: a message said through `say` is answered through `say`, as the
/// agent, once, and the model is never offered `say` itself. Replay: the
/// same message said again (its id) starts nothing; a driver killed after
/// the answer was said says it again under its id, which the operation
/// replays. Invalid: a listen naming no operation is refused; an answer the
/// operation refuses, or an operation the chat does not have, ends the turn
/// in an error that says so, says nothing, and keeps the listen.
fn reply_operation(s: &mut Suite, api: &Api, owner: &Keys, name: &str, agent_npub: &str, agent_id: &str, stranger: &Keys) -> Result<()> {
    let agents = s.agents()?;
    let wait = Duration::from_secs(30);
    let chat = s.named(api, owner, "agent-said")?;
    let c = s.create(api, owner, &chat)?;
    ship(s, &c, SAID_APP, SAID_JSON);
    for (who, role) in [(agent_npub.to_string(), "editor"), (api.identity(stranger)?, "viewer")] {
        let r = api.signed(owner, "PUT", &format!("/api/f/{chat}/members/{who}"), Some(&json!({ "role": role })))?;
        anyhow::ensure!(r.status == 200, "{who} joins {chat} as its {role}: {r}");
    }
    let listen = |body: Value| agents.signed(owner, "POST", &format!("/api/a/{name}/listen"), Some(&body));
    let follows = |v: &Value| v["listens"]["newest"].as_array().into_iter().flatten().any(|l| l["fragment"] == chat.as_str() && l["channel"] == "chat");
    let count_before = view(&agents, owner, name)["listens"]["count"].clone();
    let r = listen(json!({ "fragment": chat, "reply": "say it" }))?;
    let v = view(&agents, owner, name);
    s.ok("a listen whose reply names no operation is refused (400), and follows nothing", r.status == 400 && v["listens"]["count"] == count_before && !follows(&v), &r);
    // the deploy lands by the webhook: the same listen, again, until its channel is there
    s.eventually(wait, || listen(json!({ "fragment": chat })).is_ok_and(|r| r.status == 200));
    let r = listen(json!({ "fragment": chat }))?;
    let sub = r.body["subscription"].clone();
    s.ok(
        "unless told otherwise a listen follows `chat` and answers through `say`, as chats from before postable channels do",
        r.status == 200 && r.body["channel"] == "chat" && r.body["reply"] == "say" && sub.is_i64(),
        &r,
    );

    let said = |text: &str, id: &str| api.op(stranger, &chat, "say", id, json!({ "text": text }));
    let answers = |text: &str| records(api, owner, &chat, "chat").into_iter().filter(|r| r["principal"] == agent_id && r["body"]["text"] == text).count();
    let by_agent = || records(api, owner, &chat, "chat").into_iter().filter(|r| r["principal"] == agent_id).count();
    let calls = || -> Vec<String> {
        let ops = records(api, owner, &chat, "ops");
        ops.into_iter().filter(|r| r["principal"] == agent_id && r["body"]["op"] == "say").filter_map(|r| r["body"]["id"].as_str().map(str::to_string)).collect()
    };
    let conv = format!("{chat}/chat");
    // a turn in the chat that ended after `at`: its row in the view
    let ended_after = |at: &Value| -> Option<Value> {
        let row = conversation(&view(&agents, owner, name), &conv)?;
        (row["at"].as_i64() > at.as_i64().or(Some(0))).then_some(row)
    };

    // valid: a message said through `say`, answered through `say`
    let answer = "Hello from the reply path.";
    s.ai.clear_script();
    s.ai.script(&[Reply::Text(answer.into())]);
    let asked = s.ai.chats().len();
    let r = said("hello, agent", "m1")?;
    anyhow::ensure!(r.status == 200, "the stranger says hello: {r}");
    // the `say`'s `ops` record lands after the answer's (`mark_applied`
    // follows its deliveries' await): wait for both
    let landed = s.eventually(wait, || answers(answer) == 1 && !calls().is_empty());
    let ids = calls();
    s.ok(
        "its answer is said through the listen's operation: one `say`, as the agent, under the answer's id, its record on the chat",
        landed && ids.len() == 1 && ids[0].starts_with("rp:") && by_agent() == 1,
        json!({ "calls": ids, "chat": records(api, owner, &chat, "chat") }),
    );
    let n = api.op(owner, &chat, "count", "q", json!({}))?;
    s.ok("the app said each once: the message and the answer", n.body["result"]["n"] == 2, &n);
    let offered: Vec<String> = s.ai.chats().get(asked).and_then(|c| c["tools"].as_array().cloned()).unwrap_or_default().iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect();
    let tool = |op: &str| fragment_core::tools::tool_name(&chat, op).expect("a tool name");
    s.ok(
        "the model is offered the chat's other operations, never its reply operation (the answer would be said twice)",
        offered.contains(&tool("count")) && !offered.contains(&tool("say")),
        json!(offered),
    );
    // the turn's end is recorded just after its answer is said
    let v = settle(s, &agents, owner, name, wait);
    let at = conversation(&v, &conv).map(|c| c["at"].clone()).unwrap_or_default();
    s.ok("and its turn ended as answered", at.is_i64() && conversation(&v, &conv).is_some_and(|c| c["outcome"] == "idle"), json!(conversation(&v, &conv)));

    // replay: the same message said again is the same record, and starts nothing
    let r = said("hello, agent", "m1")?;
    std::thread::sleep(Duration::from_secs(2));
    let v = settle(s, &agents, owner, name, wait);
    s.ok(
        "the same message said again (its id) replays: nothing new on the chat, and no turn",
        r.status == 200 && r.body["replayed"] == true && records(api, owner, &chat, "chat").len() == 2 && s.ai.chats().len() == asked + 1 && ended_after(&at).is_none(),
        json!({ "say": r.body, "outcome": v["outcome"] }),
    );

    // invalid: an answer the operation refuses (`say` takes 100 characters)
    let long = "An answer longer than the chat's `say` takes: ".to_string() + &"x".repeat(100);
    s.ai.script(&[Reply::Text(long.clone())]);
    said("say something long", "m2")?;
    let mut row = None;
    s.eventually(wait, || {
        row = ended_after(&at);
        row.is_some()
    });
    let row = row.unwrap_or_default();
    s.ok(
        "an answer the reply operation refuses ends the turn in an error naming the refusal (400), and nothing is said",
        row["outcome"] == "error" && row["error"].as_str().is_some_and(|e| e.contains("/ops/say") && e.contains("400")) && answers(&long) == 0 && by_agent() == 1,
        &row,
    );
    let at = row["at"].clone();

    // invalid: a reply operation the chat does not have
    let r = listen(json!({ "fragment": chat, "reply": "shout" }))?;
    s.ok("listening again with another reply operation is the same listen, answering through it", r.status == 200 && r.body["reply"] == "shout" && r.body["subscription"] == sub, &r);
    s.ai.script(&[Reply::Text("Heard you.".into())]);
    said("anyone there?", "m3")?;
    let mut row = None;
    s.eventually(wait, || {
        row = ended_after(&at);
        row.is_some()
    });
    let row = row.unwrap_or_default();
    s.ok(
        "a reply operation the chat does not have ends the turn in an error naming it (404), and nothing is said",
        row["outcome"] == "error" && row["error"].as_str().is_some_and(|e| e.contains("/ops/shout") && e.contains("404")) && answers("Heard you.") == 0,
        &row,
    );
    let v = view(&agents, owner, name);
    s.ok("and the agent still follows the chat: the chat is there, only that operation is not", follows(&v), &v["listens"]);
    let r = listen(json!({ "fragment": chat }))?;
    s.ai.script(&[Reply::Text("Heard you now.".into())]);
    said("anyone there now?", "m4")?;
    let mended = s.eventually(wait, || answers("Heard you now.") == 1);
    s.ok("its listen naming `say` again, the next message is answered", r.status == 200 && r.body["reply"] == "say" && mended && by_agent() == 2, &r);

    // replay after a kill: the driver that replaces a dead one says the
    // answer again under its id, and the operation replays it
    let restarts = view(&agents, owner, name)["watchdogRestarts"].as_u64().unwrap_or(0);
    agents.signed(owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({ "hold_after_answer_ms": 6000, "watchdog_ms": 3000 })))?;
    let answer = "Said once, though the node was killed.";
    s.ai.script(&[Reply::Text(answer.into())]);
    let asked = s.ai.chats().len();
    said("one more, please", "m5")?;
    let posted = s.eventually(wait, || answers(answer) == 1);
    // held after its answer, the turn has not ended: only the driver that
    // replaces it ends it, and only by saying the answer again first
    let killed_at = crate::api::now_s() * 1000;
    s.crash()?;
    let agents = s.agents()?;
    let v = settle(s, &agents, owner, name, Duration::from_secs(60));
    std::thread::sleep(Duration::from_secs(2));
    let ids = calls();
    let unique: std::collections::HashSet<&String> = ids.iter().collect();
    let row = conversation(&v, &conv).unwrap_or_default();
    s.ok(
        "killed after its answer was said, the turn resumes and says it again under its id: the operation replays it, and the chat has it once",
        posted
            && v["watchdogRestarts"].as_u64().is_some_and(|n| n > restarts)
            && row["outcome"] == "idle"
            && row["at"].as_i64().is_some_and(|at| at >= killed_at)
            && answers(answer) == 1
            && by_agent() == 3
            && ids.len() == 3
            && unique.len() == 3
            && s.ai.chats().len() == asked + 1,
        json!({ "calls": ids, "conversation": row, "watchdogRestarts": v["watchdogRestarts"], "model requests": s.ai.chats().len() - asked }),
    );
    agents.signed(owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({})))?;
    s.ai.clear_script();
    Ok(())
}
