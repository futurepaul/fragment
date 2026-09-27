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

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use fragment_fakes::openrouter::Reply;
use serde_json::{json, Value};

use super::jobs::{settle, started};
use crate::api::Api;
use crate::Suite;

/// The goose the templates pin (templates/builder/app.mjs, templates/pet/app.mjs).
pub(super) const GOOSE_VERSION: &str = "1.50.0";
/// The Cua Driver the pet pins (templates/pet/app.mjs).
pub(super) const CUA_VERSION: &str = "0.28.1";
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
    stand_in(&sprite_home, &format!(".local/bin/goose-{GOOSE_VERSION}"), "goose")?;

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
/// --serve`), streaming as goose does, with one `shell` tool (the builder),
/// or with the tools of the stdio MCP servers `$GOOSE_PATH_ROOT`'s config
/// enables (the pet's agent: Cua Driver), named and filtered as goose does;
/// answers each tool call, a result's image as the user message goose's
/// OpenAI format adds after it; and prints the model's text once it calls no
/// tool, or with `--output-format stream-json`, each tool call and that text
/// as goose's events.
pub fn stand_in_goose(args: &[String]) -> Result<()> {
    if args.first().map(String::as_str) == Some("--version") {
        println!("goose {GOOSE_VERSION} (the e2e's stand-in)");
        return Ok(());
    }
    let flag = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1));
    let prompt = flag("--text").context("goose run --text <prompt>")?;
    let events = flag("--output-format").is_some_and(|f| f == "stream-json");
    let say = |content: Value| println!("{}", json!({ "type": "message", "message": { "role": "assistant", "content": [content] } }));
    let base = std::env::var("OPENAI_BASE_URL").context("OPENAI_BASE_URL")?;
    let mut servers = Mcp::configured()?;
    let mut tools = vec![];
    for server in &mut servers {
        tools.extend(server.tools()?);
    }
    if servers.is_empty() {
        tools.push(json!({ "type": "function", "function": {
            "name": "shell", "description": "Run a command with bash",
            "parameters": { "type": "object", "required": ["command"], "properties": { "command": { "type": "string" } } } } }));
    }
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(150)).build()?;
    let mut messages = vec![json!({ "role": "user", "content": prompt })];
    for _ in 0..10 {
        let body = json!({ "model": std::env::var("GOOSE_MODEL")?, "messages": messages, "tools": tools, "stream": true });
        let answer = http.post(format!("{base}/chat/completions")).json(&body).send()?.error_for_status()?.text()?;
        let (text, calls) = assemble(&answer);
        if calls.is_empty() {
            if events {
                say(json!({ "type": "text", "text": text }));
                println!("{}", json!({ "type": "complete" }));
            } else {
                println!("{text}");
            }
            return Ok(());
        }
        messages.push(json!({ "role": "assistant", "content": text, "tool_calls": calls }));
        for call in calls {
            let (name, args) = (call["function"]["name"].as_str().unwrap_or(""), call["function"]["arguments"].as_str().unwrap_or(""));
            let args: Value = serde_json::from_str(args).unwrap_or_default();
            if events {
                say(json!({ "type": "toolRequest", "id": call["id"], "toolCall": { "status": "success", "value": { "name": name, "arguments": args } } }));
            }
            let server = servers.iter_mut().find(|s| name.strip_prefix(&s.name).is_some_and(|t| t.starts_with("__")));
            let Some(server) = server else {
                let o = Command::new("bash").args(["-c", args["command"].as_str().unwrap_or("")]).output()?;
                let answer = format!("exit {}\n{}{}", o.status.code().unwrap_or(-1), String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                messages.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": answer }));
                continue;
            };
            let tool = &name[server.name.len() + 2..];
            let result = server.ask("tools/call", json!({ "name": tool, "arguments": args }))?;
            let (mut text, mut shots) = (vec![], vec![]);
            for part in result["content"].as_array().into_iter().flatten() {
                if part["type"] == "image" {
                    text.push("This tool result included an image that is uploaded in the next message.".to_string());
                    let url = format!("data:{};base64,{}", part["mimeType"].as_str().unwrap_or(""), part["data"].as_str().unwrap_or(""));
                    shots.push(json!({ "role": "user", "content": [{ "type": "image_url", "image_url": { "url": url } }] }));
                } else {
                    text.push(part["text"].as_str().unwrap_or("").to_string());
                }
            }
            messages.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": text.join(" ") }));
            messages.extend(shots);
        }
    }
    bail!("the model called tools 10 turns running")
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
    /// The servers `$GOOSE_PATH_ROOT/config/config.yaml` enables (the pet's
    /// agent writes it as JSON), each initialized.
    fn configured() -> Result<Vec<Mcp>> {
        let Ok(root) = std::env::var("GOOSE_PATH_ROOT") else { return Ok(vec![]) };
        let config: Value = serde_json::from_str(&std::fs::read_to_string(Path::new(&root).join("config/config.yaml"))?)?;
        let strings = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
        let mut servers = vec![];
        for (name, x) in config["extensions"].as_object().into_iter().flatten().filter(|(_, x)| x["enabled"] == true && x["type"] == "stdio") {
            let mut child = Command::new(x["cmd"].as_str().context("its cmd")?).args(strings(&x["args"])).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
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
/// server with three tools: `get_desktop_state` answers a PNG, `click` says
/// where it clicked and on which display (`$DISPLAY`), and
/// `start_recording`, which the pet's goose config leaves out.
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
    let tools: Vec<Value> = ["get_desktop_state", "click", "start_recording"].into_iter().map(tool).collect();
    let mut out = std::io::stdout();
    for line in std::io::stdin().lines() {
        let ask: Value = serde_json::from_str(&line?)?;
        let params = &ask["params"];
        let result = match (ask["method"].as_str().unwrap_or(""), params["name"].as_str()) {
            _ if ask["id"].is_null() => continue,
            ("initialize", _) => json!({ "protocolVersion": params["protocolVersion"], "capabilities": { "tools": {} }, "serverInfo": { "name": "cua-driver", "version": CUA_VERSION } }),
            ("tools/list", _) => json!({ "tools": tools }),
            ("tools/call", Some("get_desktop_state")) => json!({ "content": [{ "type": "text", "text": "the screen" }, { "type": "image", "data": PNG, "mimeType": "image/png" }] }),
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
