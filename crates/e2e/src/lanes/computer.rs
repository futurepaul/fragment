//! Computers (phase 8, first part): `fragment computer serve` as a local
//! process, attached to an agent. The agent's loop stays in its cell; the
//! computer runs goose's developer tools, journaled by tool-call id. The
//! spike's checks on the product: a shell call and a fragment operation in
//! one turn; stop kills the running command; a killed agent node replays
//! the call and the command runs once; a killed computer answers
//! "interrupted" and nothing runs twice.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};
use fragment_fakes::openrouter::Reply;
use serde_json::{json, Value};

use super::agents::{runs_of, settle, todos, view};
use super::app::ship;
use crate::api::Api;
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");

/// `fragment computer serve`, as a child process on a fixed port.
struct Computer {
    child: Option<Child>,
    cli: PathBuf,
    dir: PathBuf,
    port: u16,
    url: String,
}

impl Computer {
    fn start(cli: &Path, dir: &Path, port: u16) -> Result<Computer> {
        let mut c = Computer { child: None, cli: cli.to_path_buf(), dir: dir.to_path_buf(), port, url: format!("http://127.0.0.1:{port}") };
        c.spawn()?;
        Ok(c)
    }

    fn spawn(&mut self) -> Result<()> {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.dir.join("serve.log"))?;
        let child = Command::new(&self.cli)
            .args(["computer", "serve", "--listen", &format!("127.0.0.1:{}", self.port), "--work"])
            .arg(self.dir.join("work"))
            .arg("--state")
            .arg(self.dir.join("state"))
            .arg("--token-file")
            .arg(self.token_file())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()?;
        self.child = Some(child);
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(10) {
            if reqwest::blocking::get(format!("{}/health", self.url)).is_ok_and(|r| r.status() == 200) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("fragment computer serve did not answer on {} (see {})", self.url, self.dir.join("serve.log").display())
    }

    /// SIGKILL, as a machine dying would.
    fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    fn token_file(&self) -> PathBuf {
        self.dir.join("token")
    }

    fn token(&self) -> String {
        std::fs::read_to_string(self.token_file()).unwrap_or_default().trim().to_string()
    }

    fn get(&self, path: &str, token: Option<&str>) -> (u16, Value) {
        let mut req = reqwest::blocking::Client::new().get(format!("{}{path}", self.url));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        match req.send() {
            Ok(r) => (r.status().as_u16(), r.json().unwrap_or_default()),
            Err(_) => (0, Value::Null),
        }
    }

    fn project(&self, cwd: &str) -> PathBuf {
        self.dir.join("work").join(cwd)
    }
}

impl Drop for Computer {
    fn drop(&mut self) {
        self.kill();
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

pub fn computer(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("computer") {
        return Ok(());
    }
    let agents = s.agents()?;
    std::env::set_var("FRAGMENT_AGENTS", &agents.base);
    let home = s.dir("computer-home");
    s.login(api, &home);
    let owner = s.cli_keys(&home).expect("the CLI logged in");
    let wait = Duration::from_secs(30);

    let dir = s.dir("computer");
    let mut computer = Computer::start(&s.cli, &dir, fragment_devstack::free_port()?)?;
    let token = computer.token();
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(computer.token_file()).map(|m| m.permissions().mode() & 0o777).unwrap_or(0)
    };
    s.ok("fragment computer serve makes its token, readable only by its user", token.len() == 64 && mode == 0o600, format!("{} bytes, mode {mode:o}", token.len()));
    let (open, _) = computer.get("/health", None);
    let (refused, _) = computer.get("/tools", Some("not-the-token-0000"));
    s.ok("its health is open; its tools need the token", open == 200 && refused == 401, format!("{open} {refused}"));

    let bot = s.name("builder");
    let made = s.cli_json(api, &home, &["agent", "create", &bot, "--json"])?;
    let bot_id = made["id"].as_str().unwrap_or("").to_string();
    let wrong = dir.join("wrong-token");
    std::fs::write(&wrong, "0123456789abcdef0123456789abcdef")?;
    let wrong_s = wrong.to_str().expect("utf-8 path");
    let token_s = computer.token_file().to_str().expect("utf-8 path").to_string();
    let r = s.cli_json(api, &home, &["agent", "computer", &bot, "--url", &computer.url, "--token-file", wrong_s, "--json"]);
    s.ok("attaching with the wrong token is refused", r.as_ref().is_err_and(|e| e.to_string().contains("refused this token")), format!("{r:?}"));
    let r = s.cli_json(api, &home, &["agent", "computer", &bot, "--url", &computer.url, "--token-file", &token_s, "--cwd", "site", "--json"]);
    let attached = r.as_ref().is_ok_and(|v| v["cwd"] == "site" && v["tools"].as_array().is_some_and(|t| t.iter().any(|t| t == "shell")));
    s.ok("fragment agent computer attaches it, and names its tools", attached, format!("{r:?}"));

    // the agent is also in a todo list: both kinds of tool, one turn
    let todo = s.named(api, &owner, "computer-todo")?;
    let c = s.create(api, &owner, &todo)?;
    ship(s, &c, TODO_APP, TODO_JSON);
    api.signed(&owner, "PUT", &format!("/api/f/{todo}/members/{bot_id}"), Some(&json!({ "role": "editor" })))?;
    let add = fragment_core::tools::tool_name(&todo, "add_todo").expect("a tool name");
    let r = agents.signed(&owner, "GET", &format!("/api/a/{bot}/tools"), None)?;
    let tools: Vec<&str> = r.body["tools"].as_array().into_iter().flatten().filter_map(|t| t.as_str()).collect();
    s.ok("its tools are its fragments' operations and its computer's", tools.contains(&add.as_str()) && tools.contains(&"shell") && tools.contains(&"write"), &r);

    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![("shell".into(), json!({ "command": "echo hello from the computer > hello.txt && cat hello.txt" })), (add.clone(), json!({ "text": "ship the site" }))]),
        Reply::Text("Wrote hello.txt and added the todo.".into()),
    ]);
    agents.signed(&owner, "POST", &format!("/api/a/{bot}/turns"), Some(&json!({ "text": "write hello.txt and add a todo" })))?;
    let v = settle(s, &agents, &owner, &bot, wait);
    let file = read(&computer.project("site").join("hello.txt"));
    let seen = s.openrouter.chats().last().is_some_and(|c| c["messages"].to_string().contains("hello from the computer"));
    s.ok("a turn runs the shell on the computer, in the attached project", v["outcome"] == "idle" && file == "hello from the computer\n" && seen, format!("{:?} {}", file, v["outcome"]));
    s.ok("and the fragment operation in the same step", todos(api, &owner, &todo) == ["ship the site"], json!(todos(api, &owner, &todo)));

    // stop mid-command: the command's process is killed
    s.openrouter.script(&[Reply::Tools(vec![("shell".into(), json!({ "command": "sleep 21.5 && echo late > late.txt" }))]), Reply::Text("unused".into())]);
    let before = runs_of(&view(&agents, &owner, &bot), "shell").len();
    agents.signed(&owner, "POST", &format!("/api/a/{bot}/turns"), Some(&json!({ "text": "take your time" })))?;
    s.eventually(wait, || runs_of(&view(&agents, &owner, &bot), "shell").len() > before);
    std::thread::sleep(Duration::from_millis(500));
    let t0 = Instant::now();
    agents.signed(&owner, "POST", &format!("/api/a/{bot}/stop"), None)?;
    let v = settle(s, &agents, &owner, &bot, wait);
    let took = t0.elapsed();
    let call = fragment_core::tools::call_id(runs_of(&v, "shell").last().map(String::as_str).unwrap_or(""));
    let ended = s.eventually(Duration::from_secs(5), || computer.get(&format!("/calls/{call}"), Some(&token)).1["status"] == "done");
    let alive = Command::new("pgrep").args(["-f", "sleep 21.5"]).output().map(|o| o.status.success()).unwrap_or(true);
    s.ok("stop ends the turn and kills the running command", v["outcome"] == "stopped" && took < Duration::from_secs(5) && ended && !alive, format!("{took:?} {} ended={ended} alive={alive}", v["outcome"]));
    s.openrouter.clear_script();

    // the agent's node dies mid-command: the watchdog replays the call by
    // its id, re-attaches to the same run, and the command runs once
    agents.signed(&owner, "POST", &format!("/api/a/{bot}/test"), Some(&json!({ "watchdog_ms": 3000 })))?;
    s.openrouter.script(&[Reply::Tools(vec![("shell".into(), json!({ "command": "sleep 3 && echo ran >> runs.txt" }))]), Reply::Text("Ran it once.".into())]);
    let before = runs_of(&view(&agents, &owner, &bot), "shell").len();
    agents.signed(&owner, "POST", &format!("/api/a/{bot}/turns"), Some(&json!({ "text": "run it once" })))?;
    s.eventually(wait, || runs_of(&view(&agents, &owner, &bot), "shell").len() > before);
    std::thread::sleep(Duration::from_millis(1000));
    s.crash()?;
    let agents = s.agents()?;
    let v = settle(s, &agents, &owner, &bot, Duration::from_secs(60));
    let runs = runs_of(&v, "shell");
    let replayed = runs.len() >= before + 2 && runs[runs.len() - 1] == runs[runs.len() - 2];
    let record = computer.get(&format!("/calls/{}", fragment_core::tools::call_id(runs.last().map(String::as_str).unwrap_or(""))), Some(&token)).1;
    s.ok("the agent's node killed mid-command, the watchdog replays the call by its id", replayed && v["outcome"] == "idle", &v);
    s.ok("and the command ran once", read(&computer.project("site").join("runs.txt")) == "ran\n" && record["runs"] == 1, format!("{:?} {record}", read(&computer.project("site").join("runs.txt"))));

    // the computer dies mid-command: its journal answers "interrupted" on
    // restart, the model hears so, and nothing runs again
    s.openrouter.script(&[
        Reply::Tools(vec![("shell".into(), json!({ "command": "echo started >> log.txt; sleep 4; echo finished >> log.txt" }))]),
        Reply::Text("It was interrupted; I will check before running it again.".into()),
    ]);
    agents.signed(&owner, "POST", &format!("/api/a/{bot}/turns"), Some(&json!({ "text": "run the long one" })))?;
    let log = computer.project("site").join("log.txt");
    s.eventually(wait, || read(&log).contains("started"));
    computer.kill();
    computer.spawn()?;
    let v = settle(s, &agents, &owner, &bot, Duration::from_secs(60));
    let heard = s.openrouter.chats().last().is_some_and(|c| c["messages"].to_string().contains("interrupted"));
    s.ok("a computer killed mid-command answers interrupted after its restart, and the turn goes on", v["outcome"] == "idle" && heard, &v);
    s.ok("and the command did not run again", read(&log).matches("started").count() == 1, format!("{:?}", read(&log)));

    // detached, the computer's tools are gone from the agent's turns
    let r = s.cli_json(api, &home, &["agent", "computer", &bot, "--detach", "--json"]);
    let r2 = agents.signed(&owner, "GET", &format!("/api/a/{bot}/tools"), None)?;
    let gone = r2.body["tools"].as_array().is_some_and(|t| !t.iter().any(|t| t == "shell") && t.iter().any(|t| t == add.as_str()));
    s.ok("fragment agent computer --detach removes its tools", r.as_ref().is_ok_and(|v| v["detached"] == true) && gone, &r2);

    computer.kill();
    std::env::remove_var("FRAGMENT_AGENTS");
    Ok(())
}
