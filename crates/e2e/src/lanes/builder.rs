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
//! share a session, and the owner's memory and skills reach every chat
//! (`remembers`).

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
/// The Cua Driver tools the pet's goose config names (its `TOOLS`).
pub(super) const CUA_TOOLS: [&str; 8] = ["launch_app", "list_windows", "get_window_state", "click", "type_text", "press_key", "hotkey", "scroll"];
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
    // a fragment of the owner's own named memory, from before any memory of theirs
    let username = api.username(&owner)?;
    let own = format!("memory.{username}");
    let r = api.create_with(&owner, json!({ "name": own }))?;
    anyhow::ensure!(r.status == 200, "the owner's own memory fragment: {r}");
    let r = api.signed(&owner, "POST", &format!("/api/f/{own}/files"), Some(&json!({ "files": [{ "path": "notes.md", "text": "mine" }] })))?;
    anyhow::ensure!(r.status == 200, "a file in it: {r}");
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
    recorded(s, api, (&owner, &home), &own, (&computer, &computer_id))?;
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
    s.ok("goose's hints are the CLI's guide, and how to keep its owner's memory", hints.contains("fragment deploy") && hints.contains("# Your owner's memory\n~/memory is"), hints.chars().take(200).collect::<String>());
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

/// goose's shell command that gets `name`'s files, makes its page say `text`, and deploys it.
fn changes(name: &str, text: &str) -> String {
    format!("fragment sync {name} --dir changing && printf '<h1>{text}</h1>' > changing/site/index.html && fragment deploy {name} --dir changing")
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
/// built kept; asked to change that app, naming it in `fragments`, a
/// throwaway made an editor of it (once it paired) changes and deploys it,
/// then goes, and the app's members are as they were; handed to a named
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

    // changing that app, handed off naming it (a bare label is the
    // owner's): a throwaway, made its editor, changes and deploys it
    let members = || api.signed(owner, "GET", &format!("/api/f/{built}/members"), None).map(|r| r.body["members"].as_array().map_or(0, Vec::len)).unwrap_or_default();
    let (deleted, before) = (s.sprites.deleted().len(), members());
    s.openrouter.clear_script();
    s.openrouter.script_agent(&[Reply::Tools(vec![(HAND_OFF.into(), json!({ "task": "Make the page say changed on a throwaway.", "fragments": [label] }))])]);
    s.openrouter.script(&[
        Reply::Tools(vec![("shell".into(), json!({ "command": changes(&built, "changed on a throwaway") }))]),
        Reply::Text("Changed it and deployed it live.".into()),
    ]);
    say(api, owner, &chat, "h2c", &format!("make {label} say changed on a throwaway"))?;
    let mut changer = String::new();
    s.eventually(wait, || {
        changer = owned(api, owner, "handoff-").pop().unwrap_or_default();
        !changer.is_empty()
    });
    let result = s.eventually(long, || from_computer(&changer, "Changed it and deployed it live.").is_some());
    let token = api.status(owner, &built).ok().and_then(|r| r.body["viewToken"].as_str().map(str::to_string)).unwrap_or_default();
    let page = api.page(&built, &format!("?view={token}"), None)?;
    s.ok(
        "asked to change the owner's app, naming it in `fragments`, the agent hands off to a throwaway, which is made its editor and changes and deploys it",
        result && fragment_core::tools::is_throwaway(&changer) && page.text.contains("changed on a throwaway"),
        json!({ "throwaway": changer, "page": page.text.chars().take(300).collect::<String>(), "chat": chat_records(api, owner, &chat) }),
    );
    let gone = s.eventually(wait, || api.status(owner, &changer).is_ok_and(|r| r.status == 404) && s.sprites.deleted().len() == deleted + 1 && members() == before);
    s.ok(
        "then the throwaway is gone, and with its computer, its place among the app's members",
        gone,
        json!({ "throwaway": api.status(owner, &changer)?.status, "members": api.signed(owner, "GET", &format!("/api/f/{built}/members"), None)?.body }),
    );

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
    remembers(s, api, owner, &chat, (builder, computer, builder_home))?;

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
        "a guest's turn is offered no hand-off and no remember; a hand-off it calls anyway is refused, and no computer is made",
        answered && !chats.get(asked).map(offered).unwrap_or_default().iter().any(|t| t == HAND_OFF || t == REMEMBER) && refused && owned(api, owner, "handoff-").is_empty() && s.sprites.sprites().len() == sprites,
        json!({ "requests": chats.len() - asked }),
    );
    let told = chats.get(asked).map(|c| c["messages"][0]["content"].to_string()).unwrap_or_default();
    s.ok("and it is told nothing of the owner's memory", !told.is_empty() && !told.contains("Your owner's memory") && !told.contains("tea"), &told);
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

/// The members of `fragment`: (principal, role).
fn members(api: &Api, owner: &Keys, fragment: &str) -> Vec<(String, String)> {
    let r = api.signed(owner, "GET", &format!("/api/f/{fragment}/members"), None).map(|r| r.body).unwrap_or_default();
    r["members"].as_array().into_iter().flatten().map(|m| (m["principal"].as_str().unwrap_or("").to_string(), m["role"].as_str().unwrap_or("").to_string())).collect()
}

/// The owner's memory, as the platform records it (`GET /api/memory`).
fn memory_of(api: &Api, keys: &Keys) -> Value {
    api.signed(keys, "GET", "/api/memory", None).map(|r| r.body["name"].clone()).unwrap_or_default()
}

/// The platform's record of a person's memory (cell/src/memory.rs): their
/// computer's pairing made it, before any hand-off, under the next free name
/// (`own`, the owner's own fragment named memory, is never taken over),
/// the owner's, members only, with the computer an editor there. Asked
/// again, the same one; a stranger is told of none of theirs. Then the
/// owner names their own as their memory (`fragment memory use`, as one an
/// agent of theirs made before memories were recorded): refused until they
/// say `--replace`, never the computer's to do; then it is theirs, with the
/// computer an editor, and the one before is left as it was.
fn recorded(s: &mut Suite, api: &Api, (owner, owner_cli): (&Keys, &Path), own: &str, (computer_keys, computer): (&Keys, &str)) -> Result<()> {
    let memory = memory_of(api, owner).as_str().unwrap_or("").to_string();
    let username = api.username(owner)?;
    let status = api.status(owner, &memory)?;
    s.ok(
        "a computer's pairing makes its owner's memory, recorded by the platform: theirs, members only, with the computer an editor, and a fragment of theirs named memory is left as it was",
        memory == format!("memory-2.{username}")
            && status.body["owner"] == api.identity(owner)?.as_str()
            && status.body["visibility"] == "members"
            && members(api, owner, &memory).contains(&(computer.to_string(), "editor".to_string()))
            && members(api, owner, own).len() == 1
            && api.signed(owner, "GET", &format!("/api/f/{own}/file?path=notes.md"), None).is_ok_and(|r| r.text == "mine"),
        json!({ "memory": memory, "status": status.body, "own": members(api, owner, own) }),
    );
    let (again, stranger) = (memory_of(api, owner), memory_of(api, &api.person()?));
    s.ok("asked again, the same memory; someone else has none yet", again == memory.as_str() && stranger.is_null(), json!([again, stranger]));
    let label = own.split('.').next().unwrap_or("");
    let shown = s.cli_json(api, owner_cli, &["memory", "--json"]);
    let unasked = s.cli_json(api, owner_cli, &["memory", "use", label, "--json"]);
    let by_computer = api.signed(computer_keys, "PUT", "/api/memory", Some(&json!({ "fragment": own, "replace": true })))?;
    let named = s.cli_json(api, owner_cli, &["memory", "use", label, "--replace", "--json"]);
    s.ok(
        "its owner names their own fragment as their memory (`fragment memory use`): refused without --replace, never by a computer, then recorded, the computer an editor there, the one before left as it was",
        shown.as_ref().is_ok_and(|v| v["name"] == memory.as_str())
            && unasked.is_err()
            && by_computer.status == 403
            && named.as_ref().is_ok_and(|v| v["name"] == own)
            && memory_of(api, owner) == own
            && members(api, owner, own).contains(&(computer.to_string(), "editor".to_string()))
            && members(api, owner, &memory).contains(&(computer.to_string(), "editor".to_string())),
        json!({ "shown": format!("{shown:?}"), "unasked": format!("{unasked:?}"), "by computer": by_computer.status, "named": format!("{named:?}") }),
    );
    Ok(())
}

/// The fact goose remembers, the other, and the skill it writes (`remembers`).
const TEA: &str = "- Paul drinks tea, never coffee.";
/// What the owner's agent keeps itself, and corrects TEA to (`platform__remember`).
const REMEMBER: &str = "platform__remember";
const MILK: &str = "Paul takes his tea with milk.";
const GREEN: &str = "- Paul drinks green tea, never coffee.";
const WORK: &str = "- Paul builds fragments.";
const SKILL: &str = "• tea - How Paul likes his tea made.";

/// A hand-off from `chat` to its home computer: the agent's model hands off
/// `task`, goose's answers `goose`. Whether the computer then answered
/// `said` in the chat, and goose's requests.
fn handed(s: &Suite, api: &Api, owner: &Keys, chat: &str, (id, task): (&str, &str), goose: &[Reply], said: &str) -> Result<(bool, Vec<Value>)> {
    s.openrouter.clear_script();
    s.openrouter.script_agent(&[Reply::Tools(vec![(HAND_OFF.into(), json!({ "task": task }))])]);
    s.openrouter.script(goose);
    let from = s.openrouter.chats().len();
    say(api, owner, chat, id, task)?;
    let answered = || chat_records(api, owner, chat).iter().any(|r| r["body"]["turn"].as_str().is_some_and(|t| t.starts_with("hand-off:")) && r["body"]["text"] == said);
    let done = s.eventually(Duration::from_secs(150), answered);
    Ok((done, s.openrouter.chats().into_iter().skip(from).filter(|c| c["session_id"].is_string()).collect()))
}

/// The owner's memory (docs/agent-computer.md, slice 3; cell/src/memory.rs,
/// agent/src/memory.rs, the task client): the one the platform recorded as
/// the builder paired (`recorded`), the home computer an editor there. In
/// `chat`, goose remembers a fact and writes a skill (through
/// `~/.agents/skills`): both land in the memory as a commit on main. A
/// second chat's new session on the same computer is told the fact in its
/// first task and offered the skill (goose lists skills in its system
/// prompt; the stand-in does as it does), and its goose remembers another.
/// The first chat's next task is told that one at its start, and its first
/// request carries that chat's last one as its prefix: nothing loaded is
/// rewritten. The owner's agent reads the facts itself (asked what they
/// drink, it answers, no computer), keeps one itself (`platform__remember`,
/// no computer), refuses one past the cap, and corrects one. Then the
/// computer, taken out of the memory and without its name on its disk (as
/// one paired before memories were), is given both at its next sync, and
/// its next task hears the agent's facts. A guest's turn is offered no
/// remember and told nothing (`hand_offs`).
fn remembers(s: &mut Suite, api: &Api, owner: &Keys, chat: &str, (builder, computer, builder_home): (&str, &str, &Path)) -> Result<()> {
    let memory = memory_of(api, owner).as_str().unwrap_or("").to_string();
    let file = |path: &str| api.signed(owner, "GET", &format!("/api/f/{memory}/file?path={path}"), None).ok().filter(|r| r.status == 200).map(|r| r.text);
    let main = || api.status(owner, &memory).map(|r| r.body["pins"]["main"].clone()).unwrap_or_default();

    // goose remembers a fact, and writes a skill: a commit on the memory's main
    let (before, remember) = (main(), format!(
        "mkdir -p ~/memory/memory ~/.agents/skills/tea && printf -- '{TEA}\\n' > ~/memory/memory/preferences.md && \
         printf -- '---\\nname: tea\\ndescription: How Paul likes his tea made.\\n---\\nSteep it for three minutes.\\n' > ~/.agents/skills/tea/SKILL.md"
    ));
    let goose = [Reply::Tools(vec![("shell".into(), json!({ "command": remember }))]), Reply::Text("Remembered.".into())];
    let (done, first) = handed(s, api, owner, chat, ("m1", "Remember that I drink tea, never coffee, and how I make it."), &goose, "Remembered.")?;
    let landed = s.eventually(Duration::from_secs(30), || file("memory/preferences.md").is_some_and(|t| t.contains(TEA)) && file("skills/tea/SKILL.md").is_some());
    s.ok(
        "a hand-off whose goose remembers a fact and writes a skill commits both to the memory's main",
        done && landed && main() != before,
        json!({ "main": [before, main()], "preferences": file("memory/preferences.md") }),
    );

    // a second chat: its new session is told the fact, and offered the skill
    let other = s.named(api, owner, "mem-chat")?;
    let made = api.create_with(owner, json!({ "name": other, "template": "chat" }))?;
    anyhow::ensure!(made.status == 200, "a second chat from the template: {made}");
    s.hook(api, &made.body);
    let listening = || api.signed(owner, "GET", &format!("/api/f/{other}/subscriptions"), None).map_or(0, |r| r.body["subscriptions"].as_array().map_or(0, Vec::len));
    anyhow::ensure!(s.eventually(Duration::from_secs(30), || listening() == 1), "the owner's agent listens in the second chat");
    let goose = [Reply::Tools(vec![("shell".into(), json!({ "command": format!("printf -- '{WORK}\\n' > ~/memory/memory/work.md") }))]), Reply::Text("Noted.".into())];
    let (done, asks) = handed(s, api, owner, &other, ("m2", "Note that I build fragments."), &goose, "Noted.")?;
    let landed = s.eventually(Duration::from_secs(30), || file("memory/work.md").is_some_and(|t| t.contains(WORK)));
    let (fresh, then) = (asks.first().cloned().unwrap_or_default(), first.last().cloned().unwrap_or_default());
    let prompt = |c: &Value| c["messages"].as_array().into_iter().flatten().rev().find(|m| m["role"] == "user").map(|m| m["content"].to_string()).unwrap_or_default();
    let system = |c: &Value| c["messages"][0]["content"].to_string();
    s.ok(
        "another chat's new session on the computer is told the fact in its first task, and offered the skill written in the first chat",
        done && landed && fresh["session_id"] != then["session_id"] && prompt(&fresh).contains(&format!("Your owner's memory ({memory}, at ~/memory):")) && prompt(&fresh).contains(TEA) && system(&fresh).contains(SKILL),
        json!({ "prompt": prompt(&fresh), "system": system(&fresh) }),
    );

    // the first chat's next task: told what changed at its start, its loaded history kept whole
    let (done, asks) = handed(s, api, owner, chat, ("m3", "Anything else to note?"), &[Reply::Text("Nothing else.".into())], "Nothing else.")?;
    let next = asks.first().cloned().unwrap_or_default();
    let (a, b) = (then["messages"].as_array().cloned().unwrap_or_default(), next["messages"].as_array().cloned().unwrap_or_default());
    s.ok(
        "the first chat's next task starts with what changed in the memory, and its first request carries that chat's last as its prefix: nothing loaded is rewritten",
        done && !a.is_empty() && b.len() > a.len() && a[..] == b[..a.len()] && next["session_id"] == then["session_id"]
            && prompt(&next).contains("Your owner's memory changed since your earlier work here:") && prompt(&next).contains(WORK),
        json!({ "then": a.len(), "next": b.len(), "prompt": prompt(&next) }),
    );

    // the owner's agent reads the facts itself: no computer
    s.openrouter.clear_script();
    s.openrouter.script_agent(&[Reply::Text("Tea, never coffee.".into())]);
    let from = s.openrouter.chats().len();
    say(api, owner, &other, "m4", "what do I drink?")?;
    let answered = s.eventually(Duration::from_secs(30), || chat_records(api, owner, &other).iter().any(|r| r["body"]["text"] == "Tea, never coffee."));
    let asked: Vec<Value> = s.openrouter.chats().into_iter().skip(from).collect();
    let told = asked.first().map(system).unwrap_or_default();
    s.ok(
        "the owner's agent is told their memory's facts, so it answers a personal question itself, with no computer",
        answered && asked.len() == 1 && asked[0]["session_id"].is_null() && told.contains(&format!("their private fragment {memory}")) && told.contains(TEA) && told.contains(WORK),
        &told,
    );

    // the owner's agent keeps a fact itself (platform__remember): one
    // commit, no computer; a fact past the cap is refused, then it corrects one
    let build_runs = || api.signed(owner, "GET", &format!("/api/f/{builder}/runs?op=build"), None).map_or(0, |r| r.body["runs"].as_array().map_or(0, Vec::len));
    let (runs, before, from) = (build_runs(), main(), s.openrouter.chats().len());
    let long = "x".repeat(600);
    let keep = |fact: &str| (REMEMBER.into(), json!({ "topic": "preferences", "fact": fact }));
    s.openrouter.clear_script();
    s.openrouter.script_agent(&[Reply::Tools(vec![keep(MILK), keep(&long)]), Reply::Text("I'll remember that.".into())]);
    say(api, owner, &other, "m5", "remember that I take my tea with milk")?;
    let answered = s.eventually(Duration::from_secs(30), || chat_records(api, owner, &other).iter().any(|r| r["body"]["text"] == "I'll remember that."));
    let asked: Vec<Value> = s.openrouter.chats().into_iter().skip(from).collect();
    let refused = asked.get(1).is_some_and(|c| c["messages"].to_string().contains("fact is one line of 1-500 characters"));
    let kept = file("memory/preferences.md").unwrap_or_default();
    s.ok(
        "told to remember a fact, the owner's agent keeps it itself: one commit to the memory's main, the fact past the cap refused, no computer woken",
        answered && refused && kept == format!("{TEA}\n- {MILK}\n") && main() != before && build_runs() == runs && asked.iter().all(|c| c["session_id"].is_null()),
        json!({ "preferences": kept, "requests": asked.len(), "runs": build_runs() - runs }),
    );
    s.openrouter.clear_script();
    let fix = json!({ "topic": "preferences", "fact": GREEN, "replaces": TEA });
    s.openrouter.script_agent(&[Reply::Tools(vec![(REMEMBER.into(), fix)]), Reply::Text("Updated.".into())]);
    say(api, owner, &other, "m6", "actually, I drink green tea")?;
    let fixed = s.eventually(Duration::from_secs(30), || file("memory/preferences.md").is_some_and(|t| t == format!("{GREEN}\n- {MILK}\n")));
    s.ok("and corrects one in place (`replaces`)", fixed, file("memory/preferences.md").unwrap_or_default());

    // a computer that is not an editor there, nor knows its name (as one
    // paired before memories were): its next sync makes it both, and its
    // next task hears what the agent kept
    let r = api.signed(owner, "DELETE", &format!("/api/f/{memory}/members/{computer}"), None)?;
    anyhow::ensure!(r.status == 200, "the computer taken out of the memory: {r}");
    std::fs::remove_file(builder_home.join(".fragment/agent/memory"))?;
    let r = api.signed(owner, "POST", &format!("/api/f/{builder}/files"), Some(&json!({ "files": [{ "path": "note.txt", "text": "a deploy" }] })))?;
    let d = api.signed(owner, "POST", &format!("/api/f/{builder}/deploy"), Some(&json!({})))?;
    anyhow::ensure!(r.status == 200 && d.status == 200, "the builder deployed again: {r} {d}");
    // awake, it syncs now (a task that came meanwhile would race it); asleep, as it wakes for the task
    s.eventually(Duration::from_secs(5), || builder_home.join(".fragment/agent/memory").exists());
    let (done, asks) = handed(s, api, owner, chat, ("m7", "Anything new?"), &[Reply::Text("Green tea, with milk.".into())], "Green tea, with milk.")?;
    let named = std::fs::read_to_string(builder_home.join(".fragment/agent/memory")).unwrap_or_default();
    let told = asks.first().map(prompt).unwrap_or_default();
    s.ok(
        "a computer no longer an editor of its owner's memory, without its name, is made both at its next sync, and its next task starts with the facts the agent kept",
        done && named == memory && members(api, owner, &memory).contains(&(computer.to_string(), "editor".to_string())) && told.contains(GREEN) && told.contains(MILK),
        json!({ "named": named, "prompt": told }),
    );
    let log = std::fs::read_to_string(builder_home.join(".fragment/agent/hands.log")).unwrap_or_default();
    s.ok("and no sync of the memory failed", !log.contains("was not synced"), log.lines().filter(|l| l.contains("[task]")).collect::<Vec<_>>().join("\n"));
    s.openrouter.clear_script();
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
/// after a fixed system prompt (and the skills goose would list:
/// `skills_listed`), with the session's tools: a `shell` (its
/// built-in `developer`) and those of its stdio MCP servers (the pet's Cua
/// Driver and browser), named and filtered as goose does, each inheriting
/// goose's environment. As goose does, a session keeps the extensions its
/// config enabled when it was made, and a load keeps them; goose's ACP
/// methods list the config's and a session's (a stdio one's `envs` left
/// out), and add (replacing a same-named one; one with an inline env
/// refused) or remove a session's, each counted in the session's
/// `changes`, and list the tools the model is offered. Each tool call is a
/// `tool_call` update, then a `tool_call_update` once it ran (an image as
/// goose's OpenAI format adds it, a user message after the result); the
/// model's text is an `agent_message_chunk`.
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

/// One ACP connection: `initialize`, `session/new`, `session/load`,
/// `session/prompt`, and goose's methods for extensions; a notification (a
/// cancel) is let be.
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
        let (method, id) = (ask["method"].as_str().unwrap_or(""), params["sessionId"].as_str().unwrap_or("").to_string());
        let known = store.0["sessions"][&id].is_object();
        let answer = match method {
            "initialize" => Ok(json!({ "protocolVersion": 1, "agentCapabilities": { "loadSession": true } })),
            "session/new" => {
                let id = format!("s{}", store.0["sessions"].as_object().map_or(0, |s| s.len()) + 1);
                let mut extensions = vec![json!({ "type": "builtin", "name": "developer" })];
                extensions.extend(configured()?.into_iter().filter(|x| x["enabled"] == true));
                store.0["sessions"][&id] = json!({ "cwd": params["cwd"], "messages": [], "extensions": extensions, "changes": 0 });
                store.write()?;
                Ok(json!({ "sessionId": id }))
            }
            "_goose/unstable/config/extensions/list" => {
                let entries: Vec<Value> = configured()?.iter().map(|x| json!({ "extension": said(x), "enabled": x["enabled"] == true, "configKey": x["name"] })).collect();
                Ok(json!({ "extensions": entries }))
            }
            _ if !known => Err((-32002, "Resource not found".to_string())),
            "session/load" => Ok(json!({})),
            "_goose/unstable/session/extensions/list" => {
                let entries = store.0["sessions"][&id]["extensions"].as_array().into_iter().flatten().map(|x| json!({ "extension": said(x), "extensionKey": x["name"] }));
                Ok(json!({ "extensions": entries.collect::<Vec<_>>() }))
            }
            "_goose/unstable/tools/list" => {
                let (_, tools) = session_tools(&store.0["sessions"][&id]["extensions"])?;
                let mut tools: Vec<Value> = tools.iter().map(|t| json!({ "name": t["function"]["name"], "description": t["function"]["description"], "inputSchema": t["function"]["parameters"] })).collect();
                tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                Ok(json!({ "tools": tools }))
            }
            "_goose/unstable/session/extensions/add" | "_goose/unstable/session/extensions/remove" => {
                let session = &mut store.0["sessions"][&id];
                let extensions = session["extensions"].as_array_mut().context("a session's extensions")?;
                let changed = match method.ends_with("/add") {
                    true => added(&params["extension"]).map(|x| {
                        extensions.retain(|y| y["name"] != x["name"]);
                        extensions.push(x);
                    }),
                    false => {
                        let before = extensions.len();
                        extensions.retain(|x| x["name"] != params["extensionKey"]);
                        (extensions.len() < before).then_some(()).ok_or_else(|| format!("Extension {} not found", params["extensionKey"]))
                    }
                };
                if changed.is_ok() {
                    session["changes"] = json!(session["changes"].as_u64().unwrap_or(0) + 1);
                    store.write()?;
                }
                changed.map(|()| json!({})).map_err(|e| (-32602, e))
            }
            "session/prompt" => {
                let text = params["prompt"][0]["text"].as_str().unwrap_or("").to_string();
                let session = &store.0["sessions"][&id];
                let history = session["messages"].as_array().cloned().unwrap_or_default();
                let (history, stop) = prompt(&mut ws, &id, &session["extensions"].clone(), history, &text)?;
                store.0["sessions"][&id]["messages"] = json!(history);
                store.write()?;
                Ok(json!({ "stopReason": stop }))
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

/// The extensions goose's config (`~/.config/goose/config.yaml`, which the
/// pet writes as JSON) names, each with its name.
fn configured() -> Result<Vec<Value>> {
    let Ok(text) = std::fs::read_to_string(Path::new(&std::env::var("HOME")?).join(".config/goose/config.yaml")) else { return Ok(vec![]) };
    let config: Value = serde_json::from_str(&text)?;
    let named = |(name, x): (&String, &Value)| {
        let mut x = x.clone();
        x["name"] = x.get("name").cloned().unwrap_or_else(|| json!(name));
        x
    };
    Ok(config["extensions"].as_object().into_iter().flatten().map(named).collect())
}

/// An extension as goose's ACP says it (`GooseExtension`): a stdio one's
/// `envs` left out, as goose leaves them out.
fn said(x: &Value) -> Value {
    if x["type"] != "stdio" {
        return json!({ "type": x["type"], "name": x["name"] });
    }
    let mut said = json!({ "type": "mcp", "server": { "name": x["name"], "command": x["cmd"], "args": x["args"], "env": [] } });
    for k in ["description", "timeout", "available_tools"] {
        if ![Value::Null, json!(""), json!([])].contains(&x[k]) {
            said[k] = x[k].clone();
        }
    }
    said
}

/// An extension goose's ACP adds to a session, as a config holds one: with
/// no inline env, which goose refuses.
fn added(e: &Value) -> std::result::Result<Value, String> {
    let server = &e["server"];
    match (e["type"].as_str(), server["command"].is_string(), server["env"].as_array().map_or(0, Vec::len)) {
        (Some("mcp"), true, 0) => Ok(json!({
            "enabled": true, "type": "stdio", "name": server["name"], "cmd": server["command"], "args": server["args"],
            "description": e["description"], "timeout": e["timeout"], "available_tools": e["available_tools"],
        })),
        (Some("mcp"), true, _) => Err("extension env values must be passed via envKeys referencing stored secrets, not inline env".into()),
        _ => Err(format!("the stand-in adds stdio MCP extensions, not {e}")),
    }
}

/// A session's tools as goose offers them, with its stdio MCP servers,
/// running: a `shell` (its built-in `developer`) and those servers' own.
fn session_tools(extensions: &Value) -> Result<(Vec<Mcp>, Vec<Value>)> {
    let extensions = extensions.as_array().map(Vec::as_slice).unwrap_or_default();
    let mut servers = Mcp::start(extensions)?;
    let shell = json!({ "type": "function", "function": {
        "name": "shell", "description": "Run a command with bash",
        "parameters": { "type": "object", "required": ["command"], "properties": { "command": { "type": "string" } } } } });
    let mut tools: Vec<Value> = extensions.iter().filter(|x| x["type"] == "builtin" && x["name"] == "developer").map(|_| shell.clone()).collect();
    for server in &mut servers {
        tools.extend(server.tools()?);
    }
    Ok((servers, tools))
}

/// One prompt in a session: the model asked with its whole history, each
/// tool call run and said, until it answers without one (at most 10
/// rounds). The history after, and why it stopped.
fn prompt<S: std::io::Read + Write>(ws: &mut tungstenite::WebSocket<S>, session: &str, extensions: &Value, mut history: Vec<Value>, text: &str) -> Result<(Vec<Value>, &'static str)> {
    let mut update = |u: Value| ws.send(tungstenite::Message::text(json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": session, "update": u } }).to_string()));
    let env = |k: &str| std::env::var(k).with_context(|| k.to_string());
    let (mut servers, tools) = session_tools(extensions)?;
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(150)).build()?;
    history.push(json!({ "role": "user", "content": text }));
    for _ in 0..10 {
        let system = format!("You are goose (the e2e's stand-in).{}", skills_listed());
        let messages: Vec<Value> = [json!({ "role": "system", "content": system })].into_iter().chain(history.iter().cloned()).collect();
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

/// What goose's Skills extension adds to its system prompt at each model
/// call (goose v1.52.0, crates/goose/src/skills/client.rs
/// `get_instructions`): each `~/.agents/skills/<dir>/SKILL.md` whose
/// frontmatter names it, sorted, as `• name - description`.
fn skills_listed() -> String {
    let dir = Path::new(&std::env::var("HOME").unwrap_or_default()).join(".agents/skills");
    let skill = |e: std::fs::DirEntry| -> Option<(String, String)> {
        let text = std::fs::read_to_string(e.path().join("SKILL.md")).ok()?;
        let head = text.strip_prefix("---\n")?.split_once("\n---")?.0;
        let field = |k: &str| head.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(':')).map(|v| v.trim().trim_matches('\'').to_string());
        Some((field("name")?, field("description").unwrap_or_default()))
    };
    let mut skills: Vec<(String, String)> = std::fs::read_dir(dir).into_iter().flatten().flatten().filter_map(skill).collect();
    skills.sort();
    let listed: String = skills.iter().map(|(name, what)| format!("\n• {name} - {what}")).collect();
    match listed.is_empty() {
        true => listed,
        false => format!("\n\nYou have these skills at your disposal, when it is clear they can help you solve a problem or you are asked to use them:{listed}"),
    }
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
    /// A session's stdio servers, each started with its `envs`, as goose
    /// starts them.
    fn start(extensions: &[Value]) -> Result<Vec<Mcp>> {
        let strings = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
        let mut servers = vec![];
        for x in extensions.iter().filter(|x| x["type"] == "stdio") {
            let envs = x["envs"].as_object().into_iter().flatten().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()));
            let mut child = Command::new(x["cmd"].as_str().context("its cmd")?).args(strings(&x["args"])).envs(envs).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
            let (to, from) = (child.stdin.take().context("stdin")?, BufReader::new(child.stdout.take().context("stdout")?));
            let name = x["name"].as_str().context("its name")?.to_string();
            let mut server = Mcp { name, only: strings(&x["available_tools"]), _child: child, to, from, id: 0 };
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
/// server with the eight tools the pet's goose config names
/// (`get_window_state` answers a PNG, `click` says where it clicked and on
/// which display, `$DISPLAY`; the others only listed), and
/// `get_desktop_state` and `start_recording`, which it leaves out.
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
    let tools: Vec<Value> = CUA_TOOLS.into_iter().chain(["get_desktop_state", "start_recording"]).map(tool).collect();
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
