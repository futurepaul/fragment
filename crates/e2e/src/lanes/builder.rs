//! The builder template, in the `builder` section: a fragment that
//! declares a computer, whose `build({task})` job hands the task to the
//! computer's hands (`goose serve`, a session per chat), and goose makes
//! and deploys a new fragment with the `fragment` CLI. The Sprites fake
//! runs each exec and service on this machine, so the goose here is a
//! stand-in (`stand_in_goose`, this binary run as `goose`, seeded where the
//! hands install the pinned release): real goose is a Linux binary from
//! GitHub, and the model here is the OpenRouter fake's script, so this
//! proves the plumbing, not goose. The coordinator runs the real one on a
//! Sprite. Checked: the hands write goose's hints and run it on `fragment
//! model --serve`; each model call goes through that, signed as the
//! computer, streamed, billed to the owner, and logged; goose's tool call
//! makes and deploys a fragment that is the owner's; the run answers its
//! URL and goose's last message, and the page's query lists it; no model
//! key is on the computer's disk. Then hand-offs (`hand_offs`): the owner's
//! own agent hands building to a computer, a throwaway builder, this one
//! by name, and this one as its home computer, where one chat's hand-offs
//! share a session.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use fragment_fakes::openrouter::Reply;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::agents::{chat_records, say, view};
use super::jobs::{settle, started};
use crate::api::Api;
use crate::Suite;

/// The goose every computer's hands run.
pub(super) use fragment_core::computer::GOOSE_VERSION;
/// The Cua Driver the pet pins (templates/pet/app.mjs).
pub(super) const CUA_VERSION: &str = "0.28.3";
/// A 1×1 PNG, the stand-in Cua Driver's screenshot.
pub(super) const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

/// This binary as `role` (a stand-in), at `bin` under a Sprite's home, where
/// a template installs the pinned release.
pub(super) fn stand_in(home: &Path, bin: &str, role: &str) -> Result<()> {
    let path = home.join(bin);
    std::fs::create_dir_all(path.parent().context("a parent")?)?;
    std::fs::write(&path, format!("#!/bin/sh\nexec '{}' {role} \"$@\"\n", std::env::current_exe()?.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

pub fn builder(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("builder") {
        return Ok(());
    }
    let home = s.dir("builder-owner");
    s.login(api, &home);
    let owner = s.cli_keys(&home).context("the owner's CLI logged in")?;
    let owner_id = api.identity(&owner)?;
    let dir = s.dir("builder").join("site");
    let dir_s = dir.to_str().context("a UTF-8 path")?.to_string();
    let out = s.cli(api, &home, &["new", &dir_s, "--template", "builder"]);
    let made = s.cli_json(api, &home, &["create", &s.name("builder"), "--json"])?;
    let name = made["name"].as_str().unwrap_or("").to_string();
    s.hook(api, &made);
    let sprites_before: Vec<String> = s.sprites.sprites().into_keys().collect();
    let deployed = s.cli(api, &home, &["deploy", &name, "--dir", &dir_s]);
    s.ok("the builder template scaffolds and deploys", out.status.success() && deployed.status.success(), String::from_utf8_lossy(&deployed.stderr));

    // its computer: made, paired, and ready
    let ready = s.eventually(Duration::from_secs(30), || {
        let events = api.signed(&owner, "GET", &format!("/api/f/{name}/events?tail=50"), None).map(|r| r.body).unwrap_or_default();
        events["events"].as_array().is_some_and(|e| e.iter().any(|e| e["kind"] == "computer.ready"))
    });
    let sprite = s.sprites.sprites().into_keys().find(|k| !sprites_before.contains(k)).unwrap_or_default();
    let sprite_home = s.scratch.join("sprites/sprites").join(&sprite);
    let computer = s.cli_keys(&sprite_home).context("the Sprite's CLI holds its key")?;
    let computer_id = api.identity(&computer)?;
    s.ok("it declares a computer, which is made and paired", ready && !computer_id.is_empty(), &sprite);
    let up = s.eventually(Duration::from_secs(30), || sprite_home.join(".fragment/agent/port").exists());
    let hands = std::fs::read_to_string(sprite_home.join(".fragment/agent/hands.log")).unwrap_or_default();
    s.ok("its hands run: goose serve (the pinned one, here the stand-in) on its own model --serve", up && hands.contains(&format!("up: goose {GOOSE_VERSION}")), &hands);

    // the model's script: goose's shell makes and deploys a fragment, then it answers
    let label = s.name("built");
    let command = format!(
        "mkdir -p {label}/site && printf '<h1>built by goose</h1>' > {label}/site/index.html && fragment create {label} && fragment deploy {label} --dir {label}"
    );
    let said = "Built it and deployed it live.";
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Tools(vec![("shell".into(), json!({ "command": command }))]), Reply::Text(said.into())]);
    let chats_before = s.openrouter.chats().len();
    let r = api.op(&owner, &name, "build", "b1", json!({ "task": "a page that says built by goose" }))?;
    let run: Value = settle(api, &owner, &name, started(&r), &["succeeded", "held"], Duration::from_secs(90));
    let output = &run["output"];
    let built_name = format!("{label}.{}", api.username(&owner)?);
    s.ok(
        "build runs goose on the computer: it made and deployed a fragment, and the run answers its URL and goose's last message",
        run["status"] == "succeeded"
            && output["built"].as_array().is_some_and(|b| b.len() == 1 && b[0]["name"] == built_name.as_str() && b[0]["live"] == true)
            && output["url"].as_str().is_some_and(|u| u.contains(&label))
            && output["message"].as_str().is_some_and(|m| m.contains(said))
            && output["code"] == 0,
        &run,
    );
    let (r, its) = (api.status(&owner, &built_name)?, api.status(&computer, &built_name)?);
    s.ok(
        "what goose made is its owner's, with the computer an editor",
        r.body["role"] == "owner" && r.body["owner"] == owner_id.as_str() && its.body["role"] == "editor",
        json!([r.body, its.body]),
    );
    let view = r.body["viewToken"].as_str().unwrap_or("").to_string();
    let r = api.page(&built_name, &format!("?view={view}"), None)?;
    s.ok("and live", r.text.contains("built by goose"), &r);
    let r = api.op(&owner, &name, "builds", "q1", json!({}))?;
    let listed = &r.body["result"]["builds"][0];
    s.ok("the page's query lists the build, built, with its link", listed["status"] == "built" && listed["built"][0]["name"] == built_name.as_str(), &r);

    // goose's model: the platform's, through the run's own model --serve
    let chats: Vec<Value> = s.openrouter.chats().into_iter().skip(chats_before).collect();
    s.ok(
        "each of goose's model calls went through the platform, streamed, on the platform's model, which the template names",
        chats.len() == 2 && chats.iter().all(|c| c["stream"] == true && c["model"] == fragment_proto::AGENT_MODEL && c["tools"][0]["function"]["name"] == "shell"),
        json!(chats),
    );
    let usage = api.signed(&owner, "GET", "/api/budget/usage", None)?;
    let billed = usage.body["usage"].as_array().into_iter().flatten().filter(|u| u["kind"] == "computer.text" && u["fragment"] == computer_id.as_str() && u["state"] == "settled").count();
    s.ok("billed to its owner, naming the computer", billed == 2, &usage);
    let hints = std::fs::read_to_string(sprite_home.join(".config/goose/.goosehints")).unwrap_or_default();
    s.ok("goose's hints are the CLI's guide", hints.contains("fragment deploy"), hints.chars().take(200).collect::<String>());
    let log = std::fs::read_to_string(sprite_home.join(".fragment/agent/model.log")).unwrap_or_default();
    let logged: Vec<Value> = log.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()).filter(|l| l["call"]["status"] == 200).collect();
    s.ok(
        "each call is a line of model.log: how long it took, and its usage (for the provider and the cached tokens)",
        logged.len() >= 2 && logged.iter().all(|l| l["call"]["ms"].is_u64() && l["call"]["prompt_tokens"].is_u64()),
        &log,
    );
    let keys: Vec<String> = s.openrouter.minted().into_iter().map(|m| m.key).collect();
    let leaked = files_containing(&sprite_home, &keys);
    s.ok("no model key is on the computer's disk", !keys.is_empty() && leaked.is_empty(), format!("{leaked:?}"));
    hand_offs(s, api, (&owner, &home), (&name, &computer_id, &sprite_home))
}

const HAND_OFF: &str = "platform__hand_off";

/// The tools a model request offered.
fn offered(chat: &Value) -> Vec<String> {
    chat["tools"].as_array().into_iter().flatten().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect()
}

/// What the agent's model was scripted to say after a hand-off, had it been asked again.
const INVENTED: &str = "The computer is done.\n\nThe stadium opened in 1908.";

/// The notes in an agent's view that say how a hand-off to `computer` ended.
fn notes(v: &Value, computer: &str) -> Vec<String> {
    let head = format!("[The result of a hand-off to {computer} ");
    v["messages"].as_array().into_iter().flatten().filter(|m| m["role"] == "user").filter_map(|m| m["text"].as_str()).filter(|t| t.starts_with(&head)).map(str::to_string).collect()
}

/// The owner's fragments whose names start with `prefix`.
fn owned(api: &Api, owner: &Keys, prefix: &str) -> Vec<String> {
    let mine = api.signed(owner, "GET", "/api/fragments", None).map(|r| r.body).unwrap_or_default();
    mine["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).filter(|n| n.starts_with(prefix)).map(str::to_string).collect()
}

/// goose's shell command that makes and deploys `label`, saying `text`.
fn builds(label: &str, text: &str) -> String {
    format!("mkdir -p {label}/site && printf '<h1>{text}</h1>' > {label}/site/index.html && fragment create {label} && fragment deploy {label} --dir {label}")
}

/// Hand-offs (agent/src/handoff.rs; Paul, 2026-09-27;
/// docs/agent-computer.md): the owner's own agent, in a chat, does light
/// work itself and hands building to a computer. The OpenRouter fake is
/// scripted for the agent and for goose apart (the stand-in, seeded into
/// every Sprite). Checked: asked to add a todo, it calls the list's
/// operation, no computer; its tools hand work off and none writes files or
/// deploys; asked to build, with no home computer, it hands off and its
/// turn ends at once, the platform saying so, a throwaway builder runs
/// goose, whose answer its computer posts in the chat without being asked
/// again, the built URL reaches the agent's conversation as a note, and
/// the throwaway's computer (its Sprite) and fragment are gone, what it
/// built kept; handed to a named
/// computer (the builder above), the work runs there and that computer
/// stays; with the builder as the home computer, two hand-offs naming none
/// reach the chat's one session there (`home_sessions`); a guest's turn
/// has no hand-off, and one it calls anyway makes nothing.
fn hand_offs(s: &mut Suite, api: &Api, (owner, owner_cli): (&Keys, &Path), (builder, computer, builder_home): (&str, &str, &Path)) -> Result<()> {
    let agents = s.agents()?;
    let (wait, long) = (Duration::from_secs(30), Duration::from_secs(150));
    let username = api.username(owner)?;
    let chat = s.named(api, owner, "ho-chat")?;
    let made = api.create_with(owner, json!({ "name": chat, "template": "chat" }))?;
    anyhow::ensure!(made.status == 200, "a chat from the template: {made}");
    s.hook(api, &made.body);
    let agent_of = || -> Option<String> {
        let members = api.signed(owner, "GET", &format!("/api/f/{chat}/members"), None).ok()?;
        members.body["members"].as_array()?.iter().find(|m| m["kind"] == "agent").and_then(|m| m["principal"].as_str().map(str::to_string))
    };
    let listening = || api.signed(owner, "GET", &format!("/api/f/{chat}/subscriptions"), None).map_or(0, |r| r.body["subscriptions"].as_array().map_or(0, Vec::len));
    anyhow::ensure!(s.eventually(wait, || agent_of().is_some() && listening() == 1), "the owner's agent joins the chat and listens");
    let agent = agent_of().unwrap_or_default();
    let from_agent = |start: &str| chat_records(api, owner, &chat).into_iter().find(|r| r["principal"] == agent.as_str() && r["body"]["text"].as_str().is_some_and(|t| t.starts_with(start)));
    // a computer's answer: its post under a hand-off's turn on `computer` (a fragment)
    let from_computer = |on: &str, text: &str| {
        let turn = format!("hand-off:{on}:");
        chat_records(api, owner, &chat).into_iter().find(|r| r["principal"] != agent.as_str() && r["body"]["turn"].as_str().is_some_and(|t| t.starts_with(&turn)) && r["body"]["text"] == text)
    };
    let canonical = |name: &str| api.status(owner, name).ok().and_then(|r| r.body["urls"]["canonical"].as_str().map(str::to_string)).unwrap_or_default();

    // light work, itself: a todo added through the list's operation
    let todo = s.named(api, owner, "ho-todo")?;
    let r = api.create_with(owner, json!({ "name": todo, "template": "todo" }))?;
    anyhow::ensure!(r.status == 200, "a todo list from the template: {r}");
    let sprites = s.sprites.sprites().len();
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__call".into(), json!({ "fragment": todo, "operation": "add", "input": { "text": "milk" } }))]),
        Reply::Text("Added milk to your list.".into()),
    ]);
    let asked = s.openrouter.chats().len();
    say(api, owner, &chat, "h1", &format!("add milk to my list {todo}"))?;
    let added = s.eventually(wait, || from_agent("Added milk to your list.").is_some());
    let list = api.op(owner, &todo, "list", "q1", json!({}))?;
    let tools = s.openrouter.chats().get(asked).map(offered).unwrap_or_default();
    s.ok(
        "asked to add a todo, the agent calls the list's operation itself: no computer",
        added && list.body["result"]["todos"][0]["text"] == "milk" && s.sprites.sprites().len() == sprites && !tools.is_empty(),
        json!({ "list": list.body, "sprites": s.sprites.sprites().len() }),
    );
    let writes = ["platform__write_file", "platform__append_file", "platform__write_files", "platform__deploy"];
    s.ok(
        "its tools hand work off, and none writes a fragment's files or deploys it",
        tools.iter().any(|t| t == HAND_OFF) && !tools.iter().any(|t| writes.contains(&t.as_str())),
        json!(tools),
    );

    // building, handed off: a throwaway builder runs goose, whose computer
    // answers in the chat; the throwaway goes, what it built stays
    let label = s.name("ho-built");
    let built = format!("{label}.{username}");
    s.openrouter.clear_script();
    s.openrouter.script_agent(&[Reply::Tools(vec![(HAND_OFF.into(), json!({ "task": format!("Make a page that says built on a throwaway, as the fragment {label}.") }))])]);
    s.openrouter.script(&[
        Reply::Tools(vec![("shell".into(), json!({ "command": builds(&label, "built on a throwaway") }))]),
        Reply::Text("Built it and deployed it live.".into()),
    ]);
    let deleted = s.sprites.deleted().len();
    say(api, owner, &chat, "h2", "build me a page that says built on a throwaway")?;
    let on_it = s.eventually(wait, || from_agent("On its way: handoff-").is_some());
    let mut throwaway = String::new();
    s.eventually(wait, || {
        throwaway = owned(api, owner, "handoff-").pop().unwrap_or_default();
        !throwaway.is_empty()
    });
    s.ok(
        "asked to build, the agent hands off to a throwaway (a private builder of the owner's) and its turn ends at once, the platform saying so",
        on_it && fragment_core::tools::is_throwaway(&throwaway) && api.status(owner, &throwaway).is_ok_and(|r| r.body["visibility"] == "members"),
        json!({ "throwaway": throwaway, "chat": chat_records(api, owner, &chat) }),
    );
    let result = s.eventually(long, || from_computer(&throwaway, "Built it and deployed it live.").is_some());
    let url = canonical(&built);
    s.ok(
        "a throwaway builder runs goose, and its computer posts goose's answer in the chat under the hand-off's turn, without being asked again",
        result && !url.is_empty(),
        json!({ "chat": chat_records(api, owner, &chat), "url": url }),
    );
    let page = api.status(owner, &built).ok().and_then(|r| r.body["viewToken"].as_str().map(str::to_string)).unwrap_or_default();
    let page = api.page(&built, &format!("?view={page}"), None)?;
    let computers = || api.signed(owner, "GET", "/api/identities/me", None).map(|r| r.body["computers"].to_string()).unwrap_or_default();
    let gone = s.eventually(wait, || {
        api.status(owner, &throwaway).is_ok_and(|r| r.status == 404) && s.sprites.deleted().len() == deleted + 1 && s.sprites.sprites().len() == sprites && !computers().contains(&throwaway)
    });
    s.ok(
        "then the throwaway's computer (its Sprite destroyed) and fragment are gone, and what it built stays, the owner's and live",
        gone && page.text.contains("built on a throwaway") && api.status(owner, &built).is_ok_and(|r| r.body["role"] == "owner"),
        json!({ "throwaway": api.status(owner, &throwaway)?.status, "deleted": s.sprites.deleted(), "computers": computers(), "page": page.status }),
    );
    let v = view(&agents, owner, &format!("agent.{username}"));
    let kept = notes(&v, &throwaway).iter().any(|t| t.contains(&format!("It built {url}.")) && t.contains("The computer said: Built it and deployed it live."));
    s.ok("the result is in the chat's conversation as a note, with the URL, for the turns after, and nothing is left to watch", kept && v["handoffs"] == json!([]), json!(notes(&v, &throwaway)));

    // handed to a named computer: the builder above does it, and stays
    let label = s.name("ho-named");
    s.openrouter.clear_script();
    s.openrouter.script_agent(&[Reply::Tools(vec![(
        HAND_OFF.into(),
        json!({ "task": format!("Make a page that says built on my builder, as the fragment {label}."), "computer": builder }),
    )])]);
    s.openrouter.script(&[
        Reply::Tools(vec![("shell".into(), json!({ "command": builds(&label, "built on my builder") }))]),
        Reply::Text("Built it on the builder.".into()),
    ]);
    let (sprites, deleted) = (s.sprites.sprites().len(), s.sprites.deleted().len());
    say(api, owner, &chat, "h3", &format!("build it on {builder}"))?;
    let named = format!("{label}.{username}");
    let result = s.eventually(long, || !canonical(&named).is_empty() && from_computer(builder, "Built it on the builder.").is_some_and(|r| r["principal"] == computer));
    s.ok(
        "handed to a named computer (the builder above), the work runs there, and that computer stays",
        result && s.sprites.sprites().len() == sprites && s.sprites.deleted().len() == deleted && api.status(owner, builder).is_ok_and(|r| r.status == 200),
        json!({ "chat": chat_records(api, owner, &chat), "sprites": s.sprites.sprites().len() }),
    );

    home_sessions(s, api, (owner, owner_cli), &chat, (builder, computer, builder_home))?;

    // a guest's turn: no hand-off offered, and one it calls anyway makes nothing
    let guest = api.person()?;
    let guest_id = api.identity(&guest)?;
    api.signed(owner, "PUT", &format!("/api/f/{chat}/members/{guest_id}"), Some(&json!({ "role": "viewer" })))?;
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![(HAND_OFF.into(), json!({ "task": "Make a page for the guest." }))]),
        Reply::Text("Only the owner can hand work to a computer.".into()),
    ]);
    let (asked, sprites) = (s.openrouter.chats().len(), s.sprites.sprites().len());
    say(api, &guest, &chat, "g1", "build me a page too")?;
    let answered = s.eventually(wait, || from_agent("Only the owner").is_some());
    let chats = s.openrouter.chats();
    let refused = chats.get(asked + 1).is_some_and(|c| c["messages"].to_string().contains("no tool named platform__hand_off"));
    s.ok(
        "a guest's turn is offered no hand-off; one it calls anyway is refused, and no computer is made",
        answered && !chats.get(asked).map(offered).unwrap_or_default().iter().any(|t| t == HAND_OFF) && refused && owned(api, owner, "handoff-").is_empty() && s.sprites.sprites().len() == sprites,
        json!({ "requests": chats.len() - asked }),
    );
    s.openrouter.clear_script();
    Ok(())
}

/// The owner's home computer (the builder), set once with the CLI: two
/// hand-offs from one chat, naming no computer, reach the same session
/// there. The second's first model request carries the first's context:
/// the first's last request is its prefix, message for message, under the same
/// `session_id` (nothing between goose and the model edits a request).
/// Each step is posted in the chat's `work` by the computer, which the
/// hand-off made an editor there, under the hand-off's turn, and so is its
/// answer, on `chat`. No invented result (Paul's chat, 2026-09-27: the
/// agent said "on its way", then "The computer is done." and a made-up
/// answer): the agent's model is scripted to write one after each
/// hand-off, and the second is a follow-up with the first's result in its
/// history; each turn ends on the platform's acknowledgement, the model
/// never asked again, and the agent's conversation holds each result as a
/// note, input its model reads, never as its own text.
fn home_sessions(s: &mut Suite, api: &Api, (owner, owner_cli): (&Keys, &Path), chat: &str, (builder, computer, builder_home): (&str, &str, &Path)) -> Result<()> {
    let agents = s.agents()?;
    let username = api.username(owner)?;
    let agent = format!("agent.{username}");
    // bare labels, as a person types them (`fragment agent home agent pet` once named no fragment)
    let set = s.cli_json(api, owner_cli, &["agent", "home", "agent", builder.split('.').next().unwrap_or(""), "--json"]);
    let v = view(&agents, owner, &agent);
    s.ok(
        "its owner sets a home computer once (`fragment agent home`, with bare labels: theirs): a fragment of theirs that does work",
        set.as_ref().is_ok_and(|a| a["home"] == builder) && v["home"] == builder,
        format!("{set:?} / {}", v["home"]),
    );
    let canonical = |name: &str| api.status(owner, name).ok().and_then(|r| r.body["urls"]["canonical"].as_str().map(str::to_string)).unwrap_or_default();
    // the computer's answer: its post under one of its hand-offs' turns
    let answered = |text: &str| {
        let turn = format!("hand-off:{builder}:");
        chat_records(api, owner, chat).into_iter().find(|r| r["principal"] == computer && r["body"]["turn"].as_str().is_some_and(|t| t.starts_with(&turn)) && r["body"]["text"] == text)
    };
    let me = v["id"].as_str().unwrap_or("").to_string();
    let agent_said = || -> Vec<String> { chat_records(api, owner, chat).into_iter().filter(|r| r["principal"] == me.as_str()).filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).collect() };
    let build_runs = || api.signed(owner, "GET", &format!("/api/f/{builder}/runs?op=build"), None).map_or(0, |r| r.body["runs"].as_array().map_or(0, Vec::len));
    let (sprites, runs, asked) = (s.sprites.sprites().len(), build_runs(), s.openrouter.chats().len());
    let (mut done, mut acks, mut asks) = (vec![], vec![], vec![]);
    for (n, (text, task)) in [("first at home", "Make a page that says first at home, as the fragment {label}."), ("second at home", "Make {label} too: a page that says second at home.")].into_iter().enumerate() {
        let label = s.name(&format!("ho-home{n}"));
        s.openrouter.clear_script();
        // the agent's model hands off, then, if asked again, invents the result
        s.openrouter.script_agent(&[Reply::Tools(vec![(HAND_OFF.into(), json!({ "task": task.replace("{label}", &label) }))]), Reply::Text(INVENTED.into())]);
        s.openrouter.script(&[Reply::Tools(vec![("shell".into(), json!({ "command": builds(&label, text) }))]), Reply::Text(format!("Built {text}."))]);
        let (before, from) = (agent_said().len(), s.openrouter.chats().len());
        say(api, owner, chat, &format!("hh{n}"), &format!("build {text}"))?;
        let (name, built) = (format!("{label}.{username}"), format!("Built {text}."));
        done.push(s.eventually(Duration::from_secs(150), || answered(&built).is_some() && !canonical(&name).is_empty()));
        // its result reaches the agent's conversation (the alarm's next look) before the follow-up is asked
        done.push(s.eventually(Duration::from_secs(30), || notes(&view(&agents, owner, &agent), builder).iter().any(|t| t.contains(&built))));
        acks.extend(agent_said().into_iter().skip(before));
        asks.push(s.openrouter.chats().into_iter().skip(from).filter(|c| c["session_id"].is_null()).count());
    }
    // goose's requests (its own carry a session_id): the first's last, and the second's first
    let chats: Vec<Value> = s.openrouter.chats().into_iter().skip(asked).filter(|c| c["session_id"].is_string()).collect();
    let said = |c: &Value, what: &str| c["messages"].to_string().contains(what);
    let first = chats.iter().rev().find(|c| said(c, "first at home") && !said(c, "second at home"));
    let second = chats.iter().find(|c| said(c, "second at home"));
    let session = std::fs::read_to_string(builder_home.join(format!(".fragment/agent/sessions/{chat}/chat"))).unwrap_or_default();
    s.ok(
        "two hand-offs from one chat, naming no computer, go to the home computer (no new one) and its chat's one session there",
        done == [true; 4] && build_runs() == runs + 2 && s.sprites.sprites().len() == sprites && !session.is_empty(),
        json!({ "done": done, "runs": build_runs() - runs, "session": session }),
    );
    let (first, second) = (first.cloned().unwrap_or_default(), second.cloned().unwrap_or_default());
    let (a, b) = (first["messages"].as_array().cloned().unwrap_or_default(), second["messages"].as_array().cloned().unwrap_or_default());
    let prefix = !a.is_empty() && b.len() > a.len() && a[..] == b[..a.len()];
    s.ok(
        "the second's first request carries the first's context: the first's last request is its prefix, message for message, on the same session_id",
        prefix && first["session_id"] == session.as_str() && second["session_id"] == session.as_str(),
        json!({ "first": a.len(), "second": b.len(), "sessions": [first["session_id"], second["session_id"], session] }),
    );
    // the agent's side: its replies, what its model was told, what it keeps
    let ack = format!("On its way: {builder} has it, and its answer will show up here when it's done.");
    let told: Vec<Value> = s.openrouter.chats().into_iter().skip(asked).filter(|c| c["session_id"].is_null()).collect();
    let holds = |c: &Value, role: &str, what: &str| c["messages"].as_array().into_iter().flatten().any(|m| m["role"] == role && m["content"].to_string().contains(what));
    let knew = told.get(1).is_some_and(|c| holds(c, "user", "Built first at home.") && !holds(c, "assistant", "Built first at home."));
    let invented = chat_records(api, owner, chat).iter().any(|r| r["body"]["text"].as_str().is_some_and(|t| t.contains("1908")));
    s.ok(
        "a follow-up, with a finished hand-off in the agent's history, hands off again: each reply is the platform's acknowledgement, the model (scripted to invent a result after the call) never asked again, nothing invented said",
        acks == [ack.clone(), ack] && asks == [1, 1] && knew && !invented,
        json!({ "acks": acks, "asks": asks, "knew": knew, "invented": invented }),
    );
    let v = view(&agents, owner, &agent);
    let own = |m: &&Value| m["role"] == "assistant" && m["text"].as_str().is_some_and(|t| t.contains("at home.") || t.contains("The computer is done"));
    let kept = ["first at home", "second at home"].map(|t| notes(&v, builder).iter().any(|n| n.contains("\nTask: Make") && n.contains(&format!("The computer said: Built {t}."))));
    s.ok(
        "the computer's answers are its own posts, none the agent's; the agent's conversation holds each result as a note naming the task, input its model reads, never as its own text",
        kept == [true, true] && !v["messages"].as_array().into_iter().flatten().any(|m| own(&m)) && !agent_said().iter().any(|t| t.contains("at home.")),
        json!({ "notes": notes(&v, builder), "agent said": agent_said() }),
    );
    // its steps, in the chat, as the computer, under the hand-off's turn its answer names
    let records = chat_records(api, owner, chat);
    let turn = records.iter().rev().find_map(|r| r["body"]["turn"].as_str().filter(|t| t.starts_with(&format!("hand-off:{builder}:"))).map(str::to_string)).unwrap_or_default();
    let work = api.signed(owner, "GET", &format!("/api/f/{chat}/channels/work"), None)?;
    let steps: Vec<Value> = work.body["records"].as_array().into_iter().flatten().filter(|r| r["body"]["turn"] == turn.as_str()).cloned().collect();
    let members = api.signed(owner, "GET", &format!("/api/f/{chat}/members"), None)?;
    let editor = members.body["members"].as_array().into_iter().flatten().any(|m| m["principal"] == computer && m["role"] == "editor");
    s.ok(
        "each step is the computer's post in the chat's work (the hand-off made it an editor there), under the turn its answer names",
        !turn.is_empty() && editor && steps.len() == 1 && steps.iter().all(|r| r["principal"] == computer && r["body"]["kind"] == "turn.step" && r["body"]["tool"] == "shell" && r["body"]["ok"] == true),
        json!({ "turn": turn, "steps": steps, "editor": editor }),
    );
    Ok(())
}

/// The files under `dir` that hold any of `needles`.
fn files_containing(dir: &Path, needles: &[String]) -> Vec<String> {
    let mut found = vec![];
    let Ok(entries) = std::fs::read_dir(dir) else { return found };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_containing(&path, needles));
        } else if let Ok(bytes) = std::fs::read(&path) {
            let text = String::from_utf8_lossy(&bytes);
            if needles.iter().any(|n| text.contains(n.as_str())) {
                found.push(path.display().to_string());
            }
        }
    }
    found
}

/// A stand-in for goose's CLI, where the hands run it (hands.sh, as a
/// Sprites service of the fake): `goose --version`, and `goose serve --port
/// P`, goose's ACP server cut to the plumbing. A WebSocket at `/acp` for a
/// client holding `$GOOSE_SERVER__SECRET_KEY` (`?token=`), and `/health`.
/// Its sessions are kept in `~/.local/share/goose/sessions/stand-in.json`,
/// so a new connection, or a restart, loads one. `session/prompt` asks
/// `$OPENROUTER_HOST/api/v1/chat/completions` as goose's OpenRouter
/// provider does (streaming, the session's id as `session_id`, `transforms`,
/// `OPENROUTER_PARAMETERS` merged in), sending the session's whole history
/// after a fixed system prompt, with a `shell` tool and the tools of the
/// stdio MCP servers goose's config enables (the pet's Cua Driver and
/// browser), named and filtered as goose does, each inheriting goose's
/// environment. Each tool call is a `tool_call` update, then
/// a `tool_call_update` once it ran (an image as goose's OpenAI format adds
/// it, a user message after the result); the model's text is an
/// `agent_message_chunk`.
pub fn stand_in_goose(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("goose {GOOSE_VERSION} (the e2e's stand-in)");
            return Ok(());
        }
        Some("serve") => {}
        _ => bail!("goose --version | serve --port <port>"),
    }
    let port: u16 = args.iter().position(|a| a == "--port").and_then(|i| args.get(i + 1)).context("serve --port <port>")?.parse()?;
    let secret = std::env::var("GOOSE_SERVER__SECRET_KEY").context("GOOSE_SERVER__SECRET_KEY")?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    let sessions = std::sync::Arc::new(std::sync::Mutex::new(()));
    for stream in listener.incoming() {
        let (stream, secret, sessions) = (stream?, secret.clone(), std::sync::Arc::clone(&sessions));
        std::thread::spawn(move || {
            if let Err(e) = acp(stream, &secret, &sessions) {
                eprintln!("goose (stand-in): {e:#}");
            }
        });
    }
    Ok(())
}

/// One ACP connection: `initialize`, `session/new`, `session/load`, and
/// `session/prompt`; a notification (a cancel) is let be.
fn acp(stream: std::net::TcpStream, secret: &str, sessions: &std::sync::Mutex<()>) -> Result<()> {
    use tungstenite::handshake::server::{ErrorResponse, Request, Response};
    let mut head = [0u8; 64];
    let n = stream.peek(&mut head)?;
    if head[..n].starts_with(b"GET /health") {
        return Ok((&stream).write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")?);
    }
    let token = format!("token={secret}");
    // tungstenite's own type for a refusal
    #[allow(clippy::result_large_err)]
    let check = |req: &Request, resp: Response| -> std::result::Result<Response, ErrorResponse> {
        match req.uri().path() == "/acp" && req.uri().query().is_some_and(|q| q.split('&').any(|kv| kv == token)) {
            true => Ok(resp),
            false => Err(tungstenite::http::Response::builder().status(401).body(None).expect("a response")),
        }
    };
    let mut ws = tungstenite::accept_hdr(stream, check).map_err(|e| anyhow::anyhow!("the handshake: {e}"))?;
    loop {
        let text = match ws.read() {
            Ok(tungstenite::Message::Text(t)) => t,
            Ok(tungstenite::Message::Close(_)) | Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => return Ok(()),
            Ok(_) => continue,
            Err(e) => return Err(e.into()),
        };
        let ask: Value = serde_json::from_str(&text)?;
        if ask["id"].is_null() {
            continue;
        }
        let params = &ask["params"];
        let _one = sessions.lock().expect("the sessions");
        let mut store = Sessions::read()?;
        let answer = match ask["method"].as_str().unwrap_or("") {
            "initialize" => Ok(json!({ "protocolVersion": 1, "agentCapabilities": { "loadSession": true } })),
            "session/new" => {
                let id = format!("s{}", store.0["sessions"].as_object().map_or(0, |s| s.len()) + 1);
                store.0["sessions"][&id] = json!({ "cwd": params["cwd"], "messages": [] });
                store.write()?;
                Ok(json!({ "sessionId": id }))
            }
            "session/load" => match store.0["sessions"][params["sessionId"].as_str().unwrap_or("")].is_object() {
                true => Ok(json!({})),
                false => Err((-32002, "Resource not found".to_string())),
            },
            "session/prompt" => {
                let id = params["sessionId"].as_str().unwrap_or("").to_string();
                let text = params["prompt"][0]["text"].as_str().unwrap_or("").to_string();
                match store.0["sessions"][&id]["messages"].as_array().cloned() {
                    Some(history) => {
                        let (history, stop) = prompt(&mut ws, &id, history, &text)?;
                        store.0["sessions"][&id]["messages"] = json!(history);
                        store.write()?;
                        Ok(json!({ "stopReason": stop }))
                    }
                    None => Err((-32002, "Resource not found".to_string())),
                }
            }
            method => Err((-32601, format!("{method} is not the stand-in's"))),
        };
        let reply = match answer {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": ask["id"], "result": result }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": ask["id"], "error": { "code": code, "message": message } }),
        };
        ws.send(tungstenite::Message::text(reply.to_string()))?;
    }
}

/// The stand-in's sessions, in its home as goose keeps its own.
struct Sessions(Value);

impl Sessions {
    fn path() -> Result<std::path::PathBuf> {
        Ok(Path::new(&std::env::var("HOME")?).join(".local/share/goose/sessions/stand-in.json"))
    }
    fn read() -> Result<Sessions> {
        let text = std::fs::read_to_string(Self::path()?).unwrap_or_default();
        Ok(Sessions(serde_json::from_str(&text).unwrap_or_else(|_| json!({ "sessions": {} }))))
    }
    fn write(&self) -> Result<()> {
        let path = Self::path()?;
        std::fs::create_dir_all(path.parent().context("a parent")?)?;
        Ok(std::fs::write(path, self.0.to_string())?)
    }
}

/// One prompt in a session: the model asked with its whole history, each
/// tool call run and said, until it answers without one (at most 10
/// rounds). The history after, and why it stopped.
fn prompt<S: std::io::Read + Write>(ws: &mut tungstenite::WebSocket<S>, session: &str, mut history: Vec<Value>, text: &str) -> Result<(Vec<Value>, &'static str)> {
    let mut update = |u: Value| ws.send(tungstenite::Message::text(json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": session, "update": u } }).to_string()));
    let env = |k: &str| std::env::var(k).with_context(|| k.to_string());
    let mut servers = Mcp::configured()?;
    let mut tools = vec![json!({ "type": "function", "function": {
        "name": "shell", "description": "Run a command with bash",
        "parameters": { "type": "object", "required": ["command"], "properties": { "command": { "type": "string" } } } } })];
    for server in &mut servers {
        tools.extend(server.tools()?);
    }
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(150)).build()?;
    history.push(json!({ "role": "user", "content": text }));
    for _ in 0..10 {
        let messages: Vec<Value> = [json!({ "role": "system", "content": "You are goose (the e2e's stand-in)." })].into_iter().chain(history.iter().cloned()).collect();
        let mut body = json!({
            "model": env("GOOSE_MODEL")?, "messages": messages, "tools": tools, "stream": true,
            "session_id": session, "user": session, "transforms": ["middle-out"], "usage": { "include": true },
        });
        if let Ok(Value::Object(extra)) = env("OPENROUTER_PARAMETERS").map(|p| serde_json::from_str(&p).unwrap_or_default()) {
            body.as_object_mut().expect("an object").extend(extra);
        }
        let url = format!("{}/api/v1/chat/completions", env("OPENROUTER_HOST")?);
        let answer = http.post(url).bearer_auth(env("OPENROUTER_API_KEY")?).json(&body).send()?.error_for_status()?.text()?;
        let (said, calls) = assemble(&answer);
        if !said.is_empty() {
            update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": said } }))?;
        }
        if calls.is_empty() {
            history.push(json!({ "role": "assistant", "content": said }));
            return Ok((history, "end_turn"));
        }
        history.push(json!({ "role": "assistant", "content": said, "tool_calls": calls }));
        for call in calls {
            let (name, args) = (call["function"]["name"].as_str().unwrap_or(""), call["function"]["arguments"].as_str().unwrap_or(""));
            let args: Value = serde_json::from_str(args).unwrap_or_default();
            let tool_call = json!({ "goose": { "toolCall": { "toolName": name } } });
            update(json!({ "sessionUpdate": "tool_call", "toolCallId": call["id"], "title": name, "status": "pending", "rawInput": args, "_meta": tool_call }))?;
            let (mut text, mut shots, mut content) = (vec![], vec![], vec![]);
            match servers.iter_mut().find(|s| name.strip_prefix(&s.name).is_some_and(|t| t.starts_with("__"))) {
                None => {
                    let o = Command::new("bash").args(["-c", args["command"].as_str().unwrap_or("")]).output()?;
                    text.push(format!("exit {}\n{}{}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr)));
                }
                Some(server) => {
                    let result = server.ask("tools/call", json!({ "name": &name[server.name.len() + 2..], "arguments": args }))?;
                    for part in result["content"].as_array().into_iter().flatten() {
                        if part["type"] == "image" {
                            text.push("This tool result included an image that is uploaded in the next message.".to_string());
                            let url = format!("data:{};base64,{}", part["mimeType"].as_str().unwrap_or(""), part["data"].as_str().unwrap_or(""));
                            shots.push(json!({ "role": "user", "content": [{ "type": "image_url", "image_url": { "url": url } }] }));
                            content.push(json!({ "type": "content", "content": part }));
                        } else {
                            text.push(part["text"].as_str().unwrap_or("").to_string());
                        }
                    }
                }
            }
            content.insert(0, json!({ "type": "content", "content": { "type": "text", "text": text.join(" ") } }));
            update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": call["id"], "status": "completed", "content": content }))?;
            history.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": text.join(" ") }));
            history.extend(shots);
        }
    }
    Ok((history, "max_turn_requests"))
}

/// A stdio MCP server, started as goose starts one from its config.
struct Mcp {
    name: String,
    only: Vec<String>,
    _child: Child,
    to: std::process::ChildStdin,
    from: BufReader<ChildStdout>,
    id: u64,
}

impl Mcp {
    /// The servers goose's config (`~/.config/goose/config.yaml`) enables
    /// (the pet writes it as JSON), each initialized with its `envs`.
    fn configured() -> Result<Vec<Mcp>> {
        let Ok(text) = std::fs::read_to_string(Path::new(&std::env::var("HOME")?).join(".config/goose/config.yaml")) else { return Ok(vec![]) };
        let config: Value = serde_json::from_str(&text)?;
        let strings = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
        let mut servers = vec![];
        for (name, x) in config["extensions"].as_object().into_iter().flatten().filter(|(_, x)| x["enabled"] == true && x["type"] == "stdio") {
            let envs = x["envs"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()));
            let mut child = Command::new(x["cmd"].as_str().context("its cmd")?).args(strings(&x["args"])).envs(envs).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
            let (to, from) = (child.stdin.take().context("stdin")?, BufReader::new(child.stdout.take().context("stdout")?));
            let mut server = Mcp { name: name.clone(), only: strings(&x["available_tools"]), _child: child, to, from, id: 0 };
            server.ask("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "goose", "version": GOOSE_VERSION } }))?;
            writeln!(server.to, "{}", json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
            servers.push(server);
        }
        Ok(servers)
    }

    fn ask(&mut self, method: &str, params: Value) -> Result<Value> {
        self.id += 1;
        writeln!(self.to, "{}", json!({ "jsonrpc": "2.0", "id": self.id, "method": method, "params": params }))?;
        loop {
            let mut line = String::new();
            if self.from.read_line(&mut line)? == 0 {
                bail!("{} ended before answering {method}", self.name);
            }
            let answer: Value = serde_json::from_str(&line)?;
            if answer["id"] == self.id {
                return Ok(answer["result"].clone());
            }
        }
    }

    /// Its tools as goose offers them: `<server>__<tool>`, only those its config names.
    fn tools(&mut self) -> Result<Vec<Value>> {
        let listed = self.ask("tools/list", json!({}))?;
        let offered = listed["tools"].as_array().into_iter().flatten().filter(|t| self.only.is_empty() || self.only.iter().any(|o| t["name"] == o.as_str()));
        Ok(offered.map(|t| json!({ "type": "function", "function": { "name": format!("{}__{}", self.name, t["name"].as_str().unwrap_or("")), "description": t["description"], "parameters": t["inputSchema"] } })).collect())
    }
}

/// A stand-in for Cua Driver's CLI: `--version`, and `mcp`, a stdio MCP
/// server with four tools: `get_window_state` answers a PNG, `click` says
/// where it clicked and on which display (`$DISPLAY`), and
/// `get_desktop_state` and `start_recording`, which the pet's goose config
/// leaves out.
pub fn stand_in_cua(args: &[String]) -> Result<()> {
    match args.first().map(String::as_str) {
        Some("--version") => {
            println!("cua-driver {CUA_VERSION} (the e2e's stand-in)");
            return Ok(());
        }
        Some("mcp") => {}
        _ => bail!("cua-driver --version | mcp"),
    }
    let display = std::env::var("DISPLAY").unwrap_or_default();
    let tool = |name: &str| json!({ "name": name, "description": format!("the stand-in's {name}"), "inputSchema": { "type": "object", "properties": { "x": { "type": "number" }, "y": { "type": "number" } } } });
    let tools: Vec<Value> = ["get_window_state", "click", "get_desktop_state", "start_recording"].into_iter().map(tool).collect();
    let mut out = std::io::stdout();
    for line in std::io::stdin().lines() {
        let ask: Value = serde_json::from_str(&line?)?;
        let params = &ask["params"];
        let result = match (ask["method"].as_str().unwrap_or(""), params["name"].as_str()) {
            _ if ask["id"].is_null() => continue,
            ("initialize", _) => json!({ "protocolVersion": params["protocolVersion"], "capabilities": { "tools": {} }, "serverInfo": { "name": "cua-driver", "version": CUA_VERSION } }),
            ("tools/list", _) => json!({ "tools": tools }),
            ("tools/call", Some("get_window_state")) => json!({ "content": [{ "type": "text", "text": "the window" }, { "type": "image", "data": PNG, "mimeType": "image/png" }] }),
            ("tools/call", Some("click")) => json!({ "content": [{ "type": "text", "text": format!("clicked at {}, {} on {display}", params["arguments"]["x"], params["arguments"]["y"]) }] }),
            _ => json!({ "content": [{ "type": "text", "text": "not a tool here" }], "isError": true }),
        };
        writeln!(out, "{}", json!({ "jsonrpc": "2.0", "id": ask["id"], "result": result }))?;
        out.flush()?;
    }
    Ok(())
}

/// A streamed answer's text and tool calls, from OpenAI's chunks.
fn assemble(events: &str) -> (String, Vec<Value>) {
    let (mut text, mut calls) = (String::new(), Vec::<Value>::new());
    for data in events.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]") {
        let Ok(chunk) = serde_json::from_str::<Value>(data) else { continue };
        let delta = &chunk["choices"][0]["delta"];
        text += delta["content"].as_str().unwrap_or("");
        for piece in delta["tool_calls"].as_array().into_iter().flatten() {
            let i = piece["index"].as_u64().unwrap_or(0) as usize;
            if calls.len() <= i {
                calls.push(json!({ "id": piece["id"], "type": "function", "function": { "name": piece["function"]["name"], "arguments": "" } }));
            }
            let args = format!("{}{}", calls[i]["function"]["arguments"].as_str().unwrap_or(""), piece["function"]["arguments"].as_str().unwrap_or(""));
            calls[i]["function"]["arguments"] = json!(args);
        }
    }
    (text, calls)
}
