//! Agents (phase 5): an agent is a key with goose's loop and its
//! conversation in its own cell. Its tools are the operations of the
//! fragments it belongs to, called through the signed API; the model is the
//! OpenRouter fake, scripted. Turns steer, stop, and survive a killed node
//! without running an operation twice (the spike's checks, on the product).

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_fakes::openrouter::Reply;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use crate::api::Api;
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");

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
    let platform = ["platform__create_fragment", "platform__list_files", "platform__read_file", "platform__write_files", "platform__deploy"];
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

    // a turn: the model calls the operation, then answers
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![(add.clone(), json!({ "text": "milk" }))]), Reply::Text("Added milk.".into())]);
    let calls_before = s.openrouter.calls().len();
    let r = agents.signed(&owner, "POST", &format!("/api/a/{name}/turns"), Some(&json!({ "text": "add milk to my list" })))?;
    s.ok("a turn starts", r.status == 200 && r.body["started"] == true, &r);
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
    agents.signed(&owner, "POST", &format!("/api/a/{name}/test"), Some(&json!({})))?;
    s.openrouter.clear_script();
    Ok(())
}

fn chat_records(api: &Api, owner: &Keys, chat: &str) -> Vec<Value> {
    api.signed(owner, "GET", &format!("/api/f/{chat}/channels/chat"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default()
}

fn said_by(records: &[Value], who: &[&str], text: &str) -> bool {
    records.iter().any(|r| who.contains(&r["principal"].as_str().unwrap_or("")) && r["body"]["text"] == text)
}

/// Phase 7's chat: the chat template, an agent that follows its channel and
/// answers through `say`, and a chat that works an app through the agent.
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

    // a message: the agent answers in the chat, once
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Text("Hello! I'm here.".into())]);
    api.op(&owner, &chat, "say", "c1", json!({ "text": "hi bot" }))?;
    let who = [bot_id.as_str()];
    let answered = s.eventually(wait, || said_by(&chat_records(api, &owner, &chat), &who, "Hello! I'm here."));
    s.ok("a message in the chat gets the agent's answer there, as the agent", answered, json!(chat_records(api, &owner, &chat)));
    std::thread::sleep(Duration::from_secs(2));
    let records = chat_records(api, &owner, &chat);
    s.ok("the agent does not answer itself", records.len() == 2, json!(records));

    // a chat that works an app: the agent is also in a todo list
    let todo = s.named(api, &owner, "chat-todo")?;
    let c = s.create(api, &owner, &todo)?;
    ship(s, &c, TODO_APP, TODO_JSON);
    api.signed(&owner, "PUT", &format!("/api/f/{todo}/members/{bot_id}"), Some(&json!({ "role": "editor" })))?;
    s.openrouter.script(&[Reply::Tools(vec![(fragment_core::tools::tool_name(&todo, "add_todo").expect("a tool name"), json!({ "text": "bread" }))]), Reply::Text("Added bread to your list.".into())]);
    api.op(&owner, &chat, "say", "c2", json!({ "text": "please add bread to my todo list" }))?;
    let done = s.eventually(wait, || said_by(&chat_records(api, &owner, &chat), &who, "Added bread to your list."));
    s.ok("asked in the chat, the agent changes the todo list through its operation, and says so", done && todos(api, &owner, &todo) == ["bread"], json!(chat_records(api, &owner, &chat)));

    // the owner talks to the agent directly, from the CLI
    s.openrouter.script(&[Reply::Text("Just bread.".into())]);
    let r = s.cli_json(api, &home, &["agent", "say", &bot, "what is on my list?", "--json"]);
    s.ok("fragment agent say waits for the answer", r.as_ref().is_ok_and(|v| v["answer"] == "Just bread."), format!("{r:?}"));

    // removing the agent from the chat ends its subscription
    s.cli(api, &home, &["members", "rm", &chat, &bot_npub]);
    let subs = api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None)?;
    s.ok("a member removed loses its subscriptions", subs.body["subscriptions"] == json!([]), &subs);
    let before = chat_records(api, &owner, &chat).len();
    api.op(&owner, &chat, "say", "c3", json!({ "text": "anyone there?" }))?;
    std::thread::sleep(Duration::from_secs(3));
    let after = chat_records(api, &owner, &chat);
    s.ok("and hears nothing more", after.len() == before + 1, json!(after));

    // the page: the conversation so far, and a message sent from it
    let Some(mut chrome) = crate::browser::Browser::launch(&s.scratch)? else {
        s.ok("Chrome is installed for the chat page (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let page = chrome.open(&api.site_url(&chat, &format!("?view={}", room["viewToken"].as_str().unwrap_or(""))))?;
    let shows = chrome.until(&page, "document.getElementById('messages')?.textContent.includes('Added bread to your list.')", wait);
    let seen = chrome.eval(&page, "location.href + ' | ' + document.title + ' | ' + (document.body?.innerText || '').slice(0, 300)").unwrap_or_default();
    s.ok("the chat page shows the conversation", shows, seen);
    chrome.eval(&page, "document.getElementById('text').value = 'from the page'; document.getElementById('say').requestSubmit(); true")?;
    let landed = s.eventually(wait, || chat_records(api, &owner, &chat).iter().any(|r| r["body"]["text"] == "from the page"));
    s.ok("a message sent from the page lands in the channel, and shows", landed && chrome.until(&page, "document.getElementById('messages').textContent.includes('from the page')", wait), "");
    std::env::remove_var("FRAGMENT_AGENTS");
    Ok(())
}
