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
        args.push(tag.into());
        let out = Command::new(docker()).args(&args).output().expect("docker runs");
        assert!(out.status.success(), "docker run {tag}: {}", String::from_utf8_lossy(&out.stderr));
        Container { id: String::from_utf8_lossy(&out.stdout).trim().to_string() }
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
    assert!(c.exec_out(&["curl", "-sf", "http://127.0.0.1:6080/"]).contains("no screen yet"));
    let (took, code) = c.sigterm();
    eprintln!("goose: SIGTERM to exit: {} ms (code {code})", took.as_millis());
    assert!(took < Duration::from_secs(5), "{took:?}\n{}", c.logs());
    assert_eq!(code, 0);
}
