//! The builder template, in the `builder` section: a fragment that
//! declares a computer, whose `build({task})` job runs goose on it, and
//! goose makes and deploys a new fragment with the `fragment` CLI. The
//! Sprites fake runs each exec on this machine, so the goose here is a
//! stand-in (`stand_in_goose`, this binary run as `goose`), put where the
//! pinned release would be installed: real goose is a Linux binary from
//! GitHub, and the model here is the OpenRouter fake's script, so this
//! proves the plumbing, not goose. The coordinator runs the real one on a
//! Sprite. Checked: the job installs goose's hints and runs `fragment model
//! --serve` for it; each model call goes through it, signed as the
//! computer, streamed, and billed to the owner; goose's tool call makes and
//! deploys a fragment that is the owner's; the run answers its URL and
//! goose's last message, and the page's query lists it; no model key is on
//! the computer's disk.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use fragment_fakes::openrouter::Reply;
use serde_json::{json, Value};

use super::jobs::{settle, started};
use crate::api::Api;
use crate::Suite;

/// The goose the template pins (templates/builder/app.mjs).
const GOOSE_VERSION: &str = "1.50.0";

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
    let goose = sprite_home.join(format!(".local/bin/goose-{GOOSE_VERSION}"));
    std::fs::create_dir_all(goose.parent().context("a parent")?)?;
    std::fs::write(&goose, format!("#!/bin/sh\nexec '{}' goose \"$@\"\n", std::env::current_exe()?.display()))?;
    std::fs::set_permissions(&goose, std::fs::Permissions::from_mode(0o755))?;

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
        "each of goose's model calls went through the platform, streamed, on the agents' model",
        chats.len() == 2 && chats.iter().all(|c| c["stream"] == true && c["model"] == fragment_proto::AGENT_MODEL && c["tools"][0]["function"]["name"] == "shell"),
        json!(chats),
    );
    let usage = api.signed(&owner, "GET", "/api/budget/usage", None)?;
    let billed = usage.body["usage"].as_array().into_iter().flatten().filter(|u| u["kind"] == "computer.text" && u["fragment"] == computer_id.as_str() && u["state"] == "settled").count();
    s.ok("billed to its owner, naming the computer", billed == 2, &usage);
    let hints = std::fs::read_to_string(sprite_home.join(".config/goose/.goosehints")).unwrap_or_default();
    s.ok("goose's hints are the CLI's guide", hints.contains("fragment deploy"), hints.chars().take(200).collect::<String>());
    let keys: Vec<String> = s.openrouter.minted().into_iter().map(|m| m.key).collect();
    let leaked = files_containing(&sprite_home, &keys);
    s.ok("no model key is on the computer's disk", !keys.is_empty() && leaked.is_empty(), format!("{leaked:?}"));
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

/// A stand-in for goose's CLI, run where the Sprites fake runs an exec:
/// `goose --version`, and `goose run … --text <prompt>` as goose's loop cut
/// to the plumbing. It asks `$OPENAI_BASE_URL` (the run's `fragment model
/// --serve`) with one `shell` tool, streaming as goose does; runs each tool
/// call's command with bash in its folder, answering the output; and prints
/// the model's text once it calls no tool.
pub fn stand_in_goose(args: &[String]) -> Result<()> {
    if args.first().map(String::as_str) == Some("--version") {
        println!("goose {GOOSE_VERSION} (the e2e's stand-in)");
        return Ok(());
    }
    let prompt = args.iter().position(|a| a == "--text").and_then(|i| args.get(i + 1)).context("goose run --text <prompt>")?;
    let base = std::env::var("OPENAI_BASE_URL").context("OPENAI_BASE_URL")?;
    let tool = json!({ "type": "function", "function": {
        "name": "shell", "description": "Run a command with bash",
        "parameters": { "type": "object", "required": ["command"], "properties": { "command": { "type": "string" } } } } });
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(150)).build()?;
    let mut messages = vec![json!({ "role": "user", "content": prompt })];
    for _ in 0..8 {
        let body = json!({ "model": std::env::var("GOOSE_MODEL")?, "messages": messages, "tools": [tool], "stream": true });
        let events = http.post(format!("{base}/chat/completions")).json(&body).send()?.error_for_status()?.text()?;
        let (text, calls) = assemble(&events);
        if calls.is_empty() {
            println!("{text}");
            return Ok(());
        }
        messages.push(json!({ "role": "assistant", "content": text, "tool_calls": calls }));
        for call in calls {
            let args: Value = serde_json::from_str(call["function"]["arguments"].as_str().unwrap_or("")).unwrap_or_default();
            let o = Command::new("bash").args(["-c", args["command"].as_str().unwrap_or("")]).output()?;
            let answer = format!("exit {}\n{}{}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            messages.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": answer }));
        }
    }
    bail!("the model called tools 8 turns running")
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
