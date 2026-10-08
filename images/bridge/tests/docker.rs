//! The images, built and run in Docker (linux/amd64) against the fake
//! fragment API and a scripted model on this host, reached from the
//! container as `api.fragment.internal` and `model.fragment.internal`
//! (`--add-host …:host-gateway`), as the Computer DO's intercepts would be.
//!
//! Lower rung, labeled so (docs/cloudflare-v1.md, Evaluation): the real
//! images and the real goose, but local Docker in place of Containers, and
//! fakes at the platform's boundary.
//!
//! Run: `cargo test -p fragment-bridge --test docker -- --ignored --nocapture`
//! (`FRAGMENT_DOCKER_SKIP_BUILD=1` reuses images already built;
//! `FRAGMENT_DOCKER_GOOSE_TAG` names the goose image's tag).

mod support;

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::json;

use support::fake::{person, Fake};
use support::model::Model;

fn docker() -> String {
    std::env::var("DOCKER").unwrap_or_else(|_| if std::path::Path::new("/usr/local/bin/docker").exists() { "/usr/local/bin/docker".into() } else { "docker".into() })
}

fn images_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("images/").to_path_buf()
}

/// The repo's root: the goose image's build context (it carries the
/// fragment CLI, of the root workspace).
fn repo_dir() -> PathBuf {
    images_dir().parent().expect("the repo").to_path_buf()
}

/// The goose image's tag (`FRAGMENT_DOCKER_GOOSE_TAG`, so two checkouts on
/// one Docker never build over each other's).
fn goose_tag() -> String {
    std::env::var("FRAGMENT_DOCKER_GOOSE_TAG").unwrap_or_else(|_| "fragment-goose:test".into())
}

/// Builds `dockerfile` (relative to `context`) as `tag`.
fn build(context: &std::path::Path, dockerfile: &str, tag: &str) -> Duration {
    let t = Instant::now();
    if std::env::var("FRAGMENT_DOCKER_SKIP_BUILD").is_ok() {
        return Duration::ZERO;
    }
    let status = Command::new(docker()).args(["build", "--platform", "linux/amd64", "-f", dockerfile, "-t", tag, "."]).current_dir(context).status().expect("docker runs");
    assert!(status.success(), "docker build {dockerfile}");
    t.elapsed()
}

fn size(tag: &str) -> u64 {
    let out = Command::new(docker()).args(["image", "inspect", tag, "--format", "{{.Size}}"]).output().expect("docker runs");
    String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0)
}

struct Container {
    id: String,
}

impl Drop for Container {
    fn drop(&mut self) {
        // Its log, kept for reading after the test (target/tmp/<id>.log).
        let _ = std::fs::write(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("container-{}.log", &self.id[..12.min(self.id.len())])), self.logs());
        let _ = Command::new(docker()).args(["rm", "-f", &self.id]).output();
    }
}

impl Container {
    fn run(tag: &str, api: u16, model: u16, extra: &[(&str, &str)]) -> Container {
        let mut args: Vec<String> = ["run", "-d", "--platform", "linux/amd64", "--add-host", "api.fragment.internal:host-gateway", "--add-host", "model.fragment.internal:host-gateway"].iter().map(|s| s.to_string()).collect();
        for (k, v) in [("FRAGMENT_API", format!("http://api.fragment.internal:{api}")), ("FRAGMENT_MODEL", format!("http://model.fragment.internal:{model}")), ("FRAGMENT_COMPUTER", "computer:00aa".into()), ("FRAGMENT_IMAGE", tag.into())] {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        for (k, v) in extra {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        args.extend(["-p".into(), "127.0.0.1::6080".into()]);
        args.push(tag.into());
        let out = Command::new(docker()).args(&args).output().expect("docker runs");
        assert!(out.status.success(), "docker run {tag}: {}", String::from_utf8_lossy(&out.stderr));
        Container { id: String::from_utf8_lossy(&out.stdout).trim().to_string() }
    }

    /// The host's port its `port` is published on.
    fn port(&self, port: u16) -> u16 {
        let out = Command::new(docker()).args(["port", &self.id, &format!("{port}/tcp")]).output().expect("docker runs");
        let text = String::from_utf8_lossy(&out.stdout);
        text.lines().find_map(|l| l.rsplit_once(':').and_then(|(_, p)| p.trim().parse().ok())).unwrap_or_else(|| panic!("{port} is not published: {text}"))
    }

    fn exec(&self, cmd: &[&str]) -> bool {
        Command::new(docker()).arg("exec").arg(&self.id).args(cmd).output().is_ok_and(|o| o.status.success())
    }

    fn exec_out(&self, cmd: &[&str]) -> String {
        let out = Command::new(docker()).arg("exec").arg(&self.id).args(cmd).output().expect("docker runs");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn logs(&self) -> String {
        let out = Command::new(docker()).args(["logs", &self.id]).output().expect("docker runs");
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    /// SIGTERM, as the Computer DO's sleep sends it: how long until it is
    /// gone, and its exit code.
    fn sigterm(&self) -> (Duration, i64) {
        let t = Instant::now();
        assert!(Command::new(docker()).args(["kill", "--signal", "TERM", &self.id]).output().expect("docker runs").status.success());
        let out = Command::new(docker()).args(["wait", &self.id]).output().expect("docker runs");
        (t.elapsed(), String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(-1))
    }
}

/// Goal: the stub image starts in well under a second, answers a chat
/// message through its bridge, holds at its restore gate until the marker,
/// and exits within 5 s of SIGTERM.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn the_stub_image() {
    let built = build(&images_dir(), "stub/Dockerfile", "fragment-stub:test");
    eprintln!("stub: built in {:.1} s, {} MB", built.as_secs_f64(), size("fragment-stub:test") / 1_000_000);
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("talk", &["juniper"]);

    let t = Instant::now();
    let c = Container::run("fragment-stub:test", fake.addr.port(), model.addr.port(), &[]);
    fake.until(30_000, "the stub's bridge to follow its chat", |w| w.live_sockets() >= 2).await;
    let ready = t.elapsed();
    eprintln!("stub: docker run to following its chat: {} ms", ready.as_millis());
    fake.say(&chat, &person("paul"), json!({ "text": "hello stub" }));
    let asked = Instant::now();
    fake.until(20_000, "the stub's reply", |w| w.bodies(&chat, "chat", "reply").len() == 1).await;
    eprintln!("stub: message to reply: {} ms", asked.elapsed().as_millis());
    fake.with(|w| assert_eq!(w.bodies(&chat, "chat", "reply")[0]["text"], "echo: [paul] hello stub"));
    // Its turn's last posts (its end, then its draft stopped) answered, so
    // none is still in flight when the gate's count below begins (the
    // blocking `docker` calls hold this runtime, and the fake with it).
    fake.until(20_000, "the stub's turn to end and its draft to stop", |w| {
        let end = w.records(&chat, "work").iter().find(|r| r["body"]["kind"] == "turn.end").and_then(|r| r["seq"].as_u64());
        let ended_at = end.and_then(|seq| w.log.iter().position(|l| *l == format!("record {chat} work {seq}")));
        ended_at.is_some_and(|i| w.log[i + 1..].iter().any(|l| l.starts_with(&format!("draft {chat} ")) && l.ends_with(" null")))
    })
    .await;
    // Its screen port serves its page.
    assert!(c.exec_out(&["wget", "-qO-", "http://127.0.0.1:6080/"]).contains("This computer has no screen"));
    let (took, code) = c.sigterm();
    eprintln!("stub: SIGTERM to exit: {} ms (code {code})", took.as_millis());
    assert!(took < Duration::from_secs(5), "{took:?}\n{}", c.logs());
    assert_eq!(code, 0);

    // The restore gate: nothing is asked of the platform until the marker.
    let calls_before = fake.with(|w| w.calls.len());
    let gated = Container::run("fragment-stub:test", fake.addr.port(), model.addr.port(), &[("RESTORE_PENDING", "1")]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(fake.with(|w| w.calls.len()), calls_before, "the gate holds: {:?}\n{}", fake.with(|w| w.calls[calls_before.min(w.calls.len())..].to_vec()), gated.logs());
    let opened = Instant::now();
    assert!(gated.exec(&["touch", "/run/computer/restored"]), "the marker, as the DO touches it");
    fake.until(30_000, "the gated stub to follow", |w| w.live_sockets() >= 2).await;
    eprintln!("stub: marker to following: {} ms", opened.elapsed().as_millis());
}

/// Goal: the goose image holds at its restore gate, then answers a mind's
/// task through its bridge and the real goose with the scripted model: its
/// shell runs in the work directory, a step; it opens the mind's view with
/// `fragment mcp`, as the agent; every model call names the agent and the
/// tier; its screen port serves its page; SIGTERM ends it in under 5 s.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn the_goose_image() {
    let tag = goose_tag();
    let built = build(&repo_dir(), "images/goose/Dockerfile", &tag);
    eprintln!("goose: built in {:.1} s, {} MB", built.as_secs_f64(), size(&tag) / 1_000_000);
    let fake = Fake::start("0.0.0.0:0", &["hands"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let view = "<chat>\n0+1|user: I keep my notes in ~/notes\n</chat>";
    let mind = fake.mind("mind", &["hands"], view, "0+1|user: I keep my notes in ~/notes");

    let t = Instant::now();
    let c = Container::run(&tag, fake.addr.port(), model.addr.port(), &[("RESTORE_PENDING", "1")]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(fake.with(|w| w.calls.is_empty()), "the gate holds: {:?}\n{}", fake.with(|w| w.calls.clone()), c.logs());
    let gate = Instant::now();
    assert!(c.exec(&["touch", "/run/computer/restored"]));
    fake.until(60_000, "goose's bridge to follow the mind", |w| w.live_sockets() >= 2).await;
    eprintln!("goose: marker to following: {} ms ({} ms since docker run)", gate.elapsed().as_millis(), t.elapsed().as_millis());

    let ends = |w: &support::fake::World| w.bodies(&mind, "work", "turn.end");
    let replies = |w: &support::fake::World| w.bodies(&mind, "chat", "reply").iter().map(|r| r["text"].as_str().unwrap_or("").to_string()).collect::<Vec<_>>();
    let asked = Instant::now();
    fake.say(&mind, &person("paul"), json!({ "text": "Check the shell.\n\nrun: echo tool-ran > ran.txt && cat ran.txt", "to": ["id:hands"] }));
    let answered = tokio::time::timeout(Duration::from_secs(120), fake.until(600_000, "goose's answer", |w| !ends(w).is_empty()));
    if answered.await.is_err() {
        panic!("no answer; the container said:\n{}", c.logs().lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    eprintln!("goose: task to its end: {} ms", asked.elapsed().as_millis());
    fake.with(|w| {
        assert_eq!(ends(w)[0]["outcome"], "idle", "{:?}\n{}", ends(w), c.logs());
        assert_eq!(replies(w), vec!["scripted: the tool said: tool-ran"]);
        let steps = w.bodies(&mind, "work", "turn.step");
        assert!(steps.len() == 1 && steps[0]["tool"] == "shell" && steps[0]["ok"] == true, "{steps:?}");
        let by = w.records(&mind, "chat").into_iter().find(|r| r["body"]["turn"].is_string()).unwrap()["principal"].clone();
        assert_eq!(by, "id:hands");
    });
    assert_eq!(c.exec_out(&["cat", "/data/work/ran.txt"]).trim(), "tool-ran", "its shell works in /data/work");

    fake.say(&mind, &person("paul"), json!({ "text": "zoom: 0 1", "to": ["id:hands"] }));
    fake.until(120_000, "the zoom's answer", |w| ends(w).len() == 2).await;
    fake.with(|w| {
        assert_eq!(replies(w)[1], "scripted: the tool said: 0+1|user: I keep my notes in ~/notes", "{}", c.logs());
        let zooms: Vec<_> = w.requests.iter().filter(|r| r.0 == format!("POST /api/f/{mind}/ops/zoom")).collect();
        assert!(zooms.len() == 1 && zooms[0].2.as_deref() == Some("hands.paul") && !zooms[0].3, "fragment mcp, as the agent, unsigned: {zooms:?}");
    });
    {
        let calls = model.calls.lock().unwrap();
        let completions: Vec<_> = calls.iter().filter(|c| c.path == "/v1/chat/completions").collect();
        assert!(completions.len() >= 4, "{:?}", calls.iter().map(|c| c.path.clone()).collect::<Vec<_>>());
        assert!(completions.iter().all(|c| c.agent.as_deref() == Some("hands.paul") && c.model == "medium"), "every call is the agent's, at its tier");
        let (system, user) = (support::model::texts(&completions[0].body, "system"), support::model::texts(&completions[0].body, "user"));
        assert!(system.contains("You are a subagent of Mind") && user.contains(view), "the framing in the system prompt, the view in the prompt:\n{system}\n---\n{user}");
        let tools = support::model::tools(&completions[0].body);
        assert!(tools.iter().any(|t| t.ends_with("zoom")) && tools.iter().any(|t| t.ends_with("shell")), "{tools:?}");
    }
    assert!(c.exec_out(&["curl", "-sf", "http://127.0.0.1:6080/"]).contains("Take over"), "its screen's page");
    assert!(c.exec(&["curl", "-sf", "-o", "/dev/null", "http://127.0.0.1:6080/novnc/core/rfb.js"]), "and noVNC");
    assert!(!c.exec(&["test", "-e", "/run/desktop"]) || c.exec_out(&["sh", "-c", "pgrep -x Xvnc | wc -l"]).trim() == "0", "no desktop before its first use");
    let (took, code) = c.sigterm();
    eprintln!("goose: SIGTERM to exit: {} ms (code {code})", took.as_millis());
    assert!(took < Duration::from_secs(5), "{took:?}\n{}", c.logs());
    assert_eq!(code, 0);
}

/// An MCP server run in the container (`docker exec -i`), spoken to a line
/// at a time, as goose speaks to it.
struct Mcp {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    lines: std::sync::mpsc::Receiver<String>,
    next: u64,
    /// Its `initialize` answer.
    init: serde_json::Value,
}

impl Mcp {
    fn start(c: &Container, env: &[(&str, &str)], cmd: &[&str]) -> Mcp {
        use std::io::BufRead;
        let mut args: Vec<String> = vec!["exec".into(), "-i".into()];
        for (k, v) in env {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        args.push(c.id.clone());
        args.extend(cmd.iter().map(|s| s.to_string()));
        let mut child = Command::new(docker()).args(&args).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit()).spawn().expect("docker exec");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
        let mut m = Mcp { child, stdin, lines, next: 1, init: serde_json::Value::Null };
        let init = m.call("initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } }), Duration::from_secs(60));
        assert!(init["result"]["protocolVersion"].is_string(), "{init}");
        m.init = init;
        m.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        m
    }

    fn send(&mut self, v: &serde_json::Value) {
        use std::io::Write;
        writeln!(self.stdin, "{v}").expect("the server reads");
        self.stdin.flush().unwrap();
    }

    fn call(&mut self, method: &str, params: serde_json::Value, wait: Duration) -> serde_json::Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let t = Instant::now();
        // bounded by `wait`
        loop {
            let left = wait.saturating_sub(t.elapsed());
            let line = self.lines.recv_timeout(left).unwrap_or_else(|_| panic!("{method}: no answer within {wait:?}"));
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            if v["id"] == json!(id) {
                return v;
            }
        }
    }

    fn tools(&mut self) -> Vec<String> {
        let v = self.call("tools/list", json!({}), Duration::from_secs(60));
        v["result"]["tools"].as_array().map(|t| t.iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect()).unwrap_or_default()
    }

    /// A tool's text, whether it was an error, and how long it took.
    fn tool(&mut self, name: &str, args: serde_json::Value, wait: Duration) -> (String, bool, Duration) {
        let t = Instant::now();
        let v = self.call("tools/call", json!({ "name": name, "arguments": args }), wait);
        let r = &v["result"];
        let text = r["content"].as_array().map(|c| c.iter().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_else(|| v.to_string());
        (text, r["isError"] == json!(true) || v.get("error").is_some(), t.elapsed())
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The ref a snapshot gives the first element whose line holds `what`
/// (`- textbox "Customer name:" [ref=e12]`).
fn ref_of(snapshot: &str, what: &str) -> Option<String> {
    let line = snapshot.lines().find(|l| l.contains(what) && l.contains("[ref="))?;
    let at = line.find("[ref=")? + 5;
    Some(line[at..].split(']').next()?.to_string())
}

/// Goal: an agent's desktop and its tools, in the goose image, on real
/// sites. No desktop runs until its first use. Reading: web_search lists
/// results, web_read reads a docs page as Markdown and a listing as a page.
/// Browsing: the first browser call starts the agent's own desktop (Xvnc,
/// openbox, Chromium) and drives its Chromium over CDP: a list read from
/// one snapshot, a form filled and submitted. The screen: the agent's RFB
/// stream through the bridge is its desktop, drawn, and another agent's is
/// refused. Take over holds its browser and computer tools back
/// (human_has_control); given back, they act again. The computer tools
/// list cua-driver's and ours, and an image never reaches the model.
/// goose itself calls a browser tool through its session's MCP. Each
/// technique's time is printed. As Containers runs the image; needs the
/// internet.
// several threads: an MCP call here blocks its thread while the fakes
// (the model the screen tools call) answer on the others
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs Docker and the internet: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn the_goose_desktop_and_its_tools() {
    use fragment_bridge::net::Base;
    use support::rfb::{colours, Control, Viewer};
    let tag = goose_tag();
    build(&repo_dir(), "images/goose/Dockerfile", &tag);
    eprintln!("desktop: the image is {} MB", size(&tag) / 1_000_000);
    let fake = Fake::start("0.0.0.0:0", &["hands", "other"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("talk", &["hands"]);
    let c = Container::run(&tag, fake.addr.port(), model.addr.port(), &[]);
    fake.until(60_000, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    assert_eq!(c.exec_out(&["sh", "-c", "pgrep -x Xvnc | wc -l"]).trim(), "0", "no desktop before its first use");
    let agent = [("FRAGMENT_AS_AGENT", "hands.paul")];

    // reading, fast: no browser
    let mut web = Mcp::start(&c, &agent, &["fragment-desktop", "mcp", "web"]);
    assert_eq!(web.tools(), vec!["web_read", "web_search"]);
    let (found, err, took) = web.tool("web_search", json!({ "query": "rust programming language" }), Duration::from_secs(60));
    eprintln!("bench: web_search {} ms, error {err}:\n{}", took.as_millis(), found.chars().take(600).collect::<String>());
    let (page, err, took) = web.tool("web_read", json!({ "url": "https://doc.rust-lang.org/book/ch03-02-data-types.html" }), Duration::from_secs(60));
    eprintln!("bench: web_read (a docs page) {} ms, {} chars, error {err}", took.as_millis(), page.len());
    assert!(!err && page.contains("Scalar Types"), "{}", page.chars().take(800).collect::<String>());
    let (list, err, took) = web.tool("web_read", json!({ "url": "https://news.ycombinator.com/", "mode": "page", "links": true }), Duration::from_secs(60));
    let stories = list.matches("item?id=").count();
    eprintln!("bench: web_read (a listing, page mode) {} ms, {} chars, {stories} story links, error {err}", took.as_millis(), list.len());
    assert!(!err && stories >= 20, "{}", list.chars().take(800).collect::<String>());

    // browsing: the agent's own desktop starts at the first call
    let mut browser = Mcp::start(&c, &agent, &["fragment-desktop", "mcp", "browser", "hands.paul"]);
    let tools = browser.tools();
    eprintln!("desktop: the browser's tools: {tools:?}");
    assert!(tools.iter().any(|t| t == "browser_navigate") && tools.iter().any(|t| t == "browser_snapshot"), "{tools:?}");
    let (nav, err, took) = browser.tool("browser_navigate", json!({ "url": "https://news.ycombinator.com/" }), Duration::from_secs(90));
    eprintln!("bench: browser_navigate from cold (the desktop started) {} ms, error {err}", took.as_millis());
    assert!(!err, "{nav}\n{}", c.exec_out(&["sh", "-c", "cat /run/desktop/*/desktop.log | tail -40"]));
    assert_eq!(c.exec_out(&["sh", "-c", "pgrep -x Xvnc | wc -l"]).trim(), "1", "one desktop: the agent's");
    let (snap, err, took) = browser.tool("browser_snapshot", json!({}), Duration::from_secs(60));
    eprintln!("bench: browser_snapshot (a listing) {} ms, {} chars, {} story links", took.as_millis(), snap.len(), snap.matches("item?id=").count());
    assert!(!err && snap.contains("Hacker News"));
    let (form, err, took) = browser.tool("browser_navigate", json!({ "url": "https://httpbin.org/forms/post" }), Duration::from_secs(60));
    eprintln!("bench: browser_navigate (a form) {} ms", took.as_millis());
    assert!(!err, "{form}");
    let (snap, _, _) = browser.tool("browser_snapshot", json!({}), Duration::from_secs(60));
    let name = ref_of(&snap, "Customer name").unwrap_or_else(|| panic!("no customer name field:\n{snap}"));
    let large = ref_of(&snap, "Large").unwrap_or_else(|| panic!("no Large:\n{snap}"));
    let t = Instant::now();
    let (typed, err, _) = browser.tool("browser_type", json!({ "element": "Customer name", "target": name, "text": "Juniper Paul" }), Duration::from_secs(60));
    assert!(!err, "{typed}\n{snap}");
    let (clicked, err, _) = browser.tool("browser_click", json!({ "element": "Large", "target": large }), Duration::from_secs(60));
    assert!(!err, "{clicked}");
    let (snap, _, _) = browser.tool("browser_snapshot", json!({}), Duration::from_secs(60));
    let submit = ref_of(&snap, "Submit order").unwrap_or_else(|| panic!("no submit:\n{snap}"));
    let (done, err, _) = browser.tool("browser_click", json!({ "element": "Submit order", "target": submit }), Duration::from_secs(60));
    eprintln!("bench: a form filled and submitted (type, click, snapshot, click) {} ms", t.elapsed().as_millis());
    let (result, _, _) = browser.tool("browser_snapshot", json!({}), Duration::from_secs(60));
    assert!(!err && (done.contains("Juniper Paul") || result.contains("Juniper Paul")), "httpbin echoes the form:\n{done}\n{result}");
    let under_data = c.exec_out(&["sh", "-c", "find /data -path /data/bridge -prune -o -type f -print 2>/dev/null | head"]);
    assert!(under_data.trim().is_empty(), "nothing of a desktop or its browser is under /data (saved): {under_data}");

    // the screen: the agent's own desktop, drawn
    let base = Base::parse(&format!("http://127.0.0.1:{}", c.port(6080))).unwrap();
    let mut watching = Control::open(&base, "watcher", "hands.paul").await.unwrap();
    assert_eq!(watching.next().await.unwrap(), json!({ "type": "control", "agent": "hands.paul", "name": "hands", "holder": null }));
    let t = Instant::now();
    let mut viewer = Viewer::open(&base, "watcher", "hands.paul", Duration::from_secs(30)).await.unwrap_or_else(|e| panic!("the screen: {e}\n{}", c.logs()));
    let frame = viewer.frame().await.unwrap();
    eprintln!("desktop: the screen's first frame {} ms, {}x{} {:?}, {} colours", t.elapsed().as_millis(), viewer.width, viewer.height, viewer.name, colours(&frame));
    assert_eq!(viewer.name, "hands.paul", "the agent's own desktop");
    assert!(colours(&frame) > 16, "the browser drawn on it");
    let other = fragment_bridge::net::connect_ws(&base, "/websockify?viewer=w&agent=nobody.paul", &[]).await.err().unwrap_or_default();
    assert!(other.contains("404"), "an agent not on this computer: {other}");

    // Take over holds the agent's tools back
    let mut driving = Control::open(&base, "driver", "hands.paul").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], json!(null));
    driving.say("take").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], "driver");
    let (held, err, _) = browser.tool("browser_snapshot", json!({}), Duration::from_secs(30));
    assert!(err && held.contains("human_has_control"), "{held}");
    driving.say("give").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], json!(null));
    let (_, err, _) = browser.tool("browser_snapshot", json!({}), Duration::from_secs(30));
    assert!(!err, "given back, the browser acts again");

    // the computer: cua-driver's tools offered and ours
    let mut computer = Mcp::start(&c, &agent, &["fragment-desktop", "mcp", "computer", "hands.paul"]);
    let tools = computer.tools();
    eprintln!("desktop: the computer's tools: {tools:?}");
    for t in ["screen_look", "screen_click", "list_windows", "type_text", "hotkey", "press_key", "click"] {
        assert!(tools.iter().any(|n| n == t), "{t} among {tools:?}");
    }
    for t in ["get_desktop_state", "browser_navigate", "start_recording", "install_ffmpeg"] {
        assert!(!tools.iter().any(|n| n == t), "{t} is not offered: {tools:?}");
    }
    let told = computer.init["result"]["instructions"].as_str().unwrap_or("");
    assert!(told.contains("screen_look") && !told.contains("get_window_state"), "its instructions are ours, naming only what it offers: {told}");
    // cua-driver types into the desktop's Chromium: its address bar
    let (windows, err, took) = computer.tool("list_windows", json!({ "on_screen_only": true }), Duration::from_secs(30));
    eprintln!("bench: cua-driver list_windows {} ms: {}", took.as_millis(), windows.lines().take(3).collect::<Vec<_>>().join(" / "));
    let line = windows.lines().find(|l| l.contains("Chromium")).unwrap_or_else(|| panic!("no Chromium window (error {err}): {windows}"));
    let field = |k: &str| line.split_whitespace().find_map(|w| w.strip_prefix(k)).and_then(|v| v.parse::<u64>().ok()).unwrap();
    let (pid, window) = (field("pid="), field("window_id="));
    let t = Instant::now();
    for (tool, args) in [("hotkey", json!({ "keys": ["ctrl", "l"] })), ("type_text", json!({ "text": "example.org" })), ("press_key", json!({ "key": "Return" }))] {
        let mut args = args;
        args["pid"] = json!(pid);
        args["window_id"] = json!(window);
        let (said, err, _) = computer.tool(tool, args, Duration::from_secs(30));
        assert!(!err, "{tool}: {said}");
    }
    eprintln!("bench: cua-driver hotkey, type_text, press_key (an address typed) {} ms", t.elapsed().as_millis());
    let mut landed = String::new();
    for _ in 0..20 {
        landed = browser.tool("browser_snapshot", json!({ "depth": 3 }), Duration::from_secs(30)).0;
        if landed.contains("example.org") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(landed.contains("example.org"), "what cua-driver typed reached the browser:\n{landed}");

    // screen_look: the screen, to the vision model, through the intercept
    let (seen, err, took) = computer.tool("screen_look", json!({ "question": "What page is open?" }), Duration::from_secs(60));
    eprintln!("bench: screen_look (scripted vision) {} ms: {seen}", took.as_millis());
    assert!(!err && seen.starts_with("scripted:"), "{seen}");
    // screen_click: Clef chooses a cell of a numbered grid, then one of a
    // finer grid around it (the scripted Clef chooses the cell the target
    // names, then the first), and the click lands at that cell's centre
    let (clicked, err, took) = computer.tool("screen_click", json!({ "target": "the thing in cell 50" }), Duration::from_secs(60));
    eprintln!("bench: screen_click (scripted Clef) {} ms: {clicked}", took.as_millis());
    assert!(!err && clicked.contains("x=66, y=366"), "{clicked}");
    let display = c.exec_out(&["fragment-desktop", "display", "hands.paul"]);
    let at = c.exec_out(&["env", &format!("DISPLAY=:{}", display.trim()), "xdotool", "getmouselocation"]);
    assert!(at.starts_with("x:66 y:366"), "the pointer is where Clef found it: {at}");
    {
        let calls = model.calls.lock().unwrap();
        let vision: Vec<_> = calls.iter().filter(|c| c.model == "vision").collect();
        assert!(vision.len() == 1 && vision[0].agent.as_deref() == Some("hands.paul"), "one vision call, the agent's");
        assert!(vision[0].body.to_string().contains("data:image/jpeg;base64,"), "the screenshot went with it");
        let decides: Vec<_> = calls.iter().filter(|c| c.path == "/v1/decide").collect();
        assert_eq!(decides.len(), 2, "two choices: the grid, then the finer one");
        let options = |c: &support::model::Call| c.body["questions"]["cell"]["criteria"].as_object().map(|o| o.len()).unwrap_or(0);
        assert_eq!((options(decides[0]), options(decides[1])), (96, 48));
        assert!(decides.iter().all(|d| d.body["images"][0].as_str().is_some_and(|i| i.starts_with("data:image/jpeg;base64,")) && d.agent.as_deref() == Some("hands.paul")));
    }
    drop((viewer, watching, driving, web, browser, computer));

    // goose itself, through its session's browser
    let said = fake.say(&chat, &person("paul"), json!({ "text": "call: browser_navigate {\"url\": \"https://example.com/\"}" }));
    let turn = fragment_bridge::records::turn_id("hands.paul", &chat, "chat", said["seq"].as_u64().unwrap());
    let t = Instant::now();
    fake.until(180_000, "goose's browsing turn", |w| w.bodies(&chat, "chat", "reply").iter().any(|r| r["turn"] == turn)).await;
    let reply = fake.with(|w| w.bodies(&chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn).unwrap());
    eprintln!("bench: goose's turn, one browser call {} ms: {}", t.elapsed().as_millis(), reply["text"].as_str().unwrap_or("").chars().take(300).collect::<String>());
    assert!(reply["text"].as_str().unwrap_or("").contains("Example Domain"), "{reply}\n{}", c.logs());
    let steps = fake.with(|w| w.bodies(&chat, "work", "turn.step"));
    assert!(steps.iter().any(|s| s["tool"] == "browser_navigate"), "{steps:?}");
    {
        let calls = model.calls.lock().unwrap();
        let first = calls.iter().find(|c| c.path == "/v1/chat/completions" && c.model == "medium").unwrap();
        let tools = support::model::tools(&first.body);
        for t in ["browser__browser_navigate", "computer__screen_look", "web__web_read", "load_skill"] {
            assert!(tools.iter().any(|n| n == t), "{t} among {tools:?}");
        }
        let system = support::model::texts(&first.body, "system");
        assert!(system.contains("`fragment` CLI") && system.contains("fragment - You are an agent on your owner's Fragment computer"), "fragments top of mind, the platform skill listed:\n{system}");
    }
    let (took, code) = c.sigterm();
    eprintln!("desktop: SIGTERM to exit with a desktop up: {} ms (code {code})", took.as_millis());
    assert!(took < Duration::from_secs(5));
}

/// Goal: a desktop no one uses stops, and one someone watches does not.
/// Started for its first viewer (the bridge's screen runs its start), it
/// stays up while watched past the idle bound (here 12 s:
/// `FRAGMENT_DESKTOP_IDLE_MS`; the screen touches its activity every 10 s),
/// and stops once the viewer has gone that long; the next viewer starts it
/// again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn an_unused_desktop_stops() {
    use fragment_bridge::net::Base;
    use support::rfb::Viewer;
    let tag = goose_tag();
    build(&repo_dir(), "images/goose/Dockerfile", &tag);
    let fake = Fake::start("0.0.0.0:0", &["hands"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let c = Container::run(&tag, fake.addr.port(), model.addr.port(), &[("FRAGMENT_DESKTOP_IDLE_MS", "12000")]);
    fake.until(60_000, "the bridge to follow", |w| w.live_sockets() >= 1).await;
    let base = Base::parse(&format!("http://127.0.0.1:{}", c.port(6080))).unwrap();
    let up = || c.exec_out(&["sh", "-c", "pgrep -x Xvnc | wc -l"]).trim() == "1";
    assert!(!up(), "no desktop before its first use");
    let t = Instant::now();
    let mut viewer = Viewer::open(&base, "w", "hands.paul", Duration::from_secs(30)).await.unwrap_or_else(|e| panic!("the screen: {e}\n{}", c.logs()));
    eprintln!("idle: the first viewer's desktop up in {} ms", t.elapsed().as_millis());
    for _ in 0..25 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert!(up(), "a watched desktop stays up");
        let _ = viewer.frame().await;
    }
    drop(viewer);
    let t = Instant::now();
    // bounded: the idle bound, a look, and the stop's grace
    while up() && t.elapsed() < Duration::from_secs(30) {
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    eprintln!("idle: stopped {} ms after its viewer left", t.elapsed().as_millis());
    assert!(!up(), "an unused desktop stops\n{}", c.exec_out(&["sh", "-c", "tail -5 /run/desktop/*/desktop.log"]));
    let again = Viewer::open(&base, "w", "hands.paul", Duration::from_secs(30)).await;
    assert!(again.is_ok() && up(), "the next viewer starts it again: {:?}", again.err());
}
