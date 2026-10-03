//! The images, built and run in Docker (linux/amd64) against the fake
//! fragment API and a scripted model on this host, reached from the
//! container as `api.fragment.internal` and `model.fragment.internal`
//! (`--add-host …:host-gateway`), as the Computer DO's intercepts would be.
//!
//! Lower rung, labeled so (docs/cloudflare-v1.md, Evaluation): the real
//! images and the real Hermes, but local Docker in place of Containers, and
//! fakes at the platform's boundary.
//!
//! Run: `cargo test -p fragment-bridge --test docker -- --ignored --nocapture`
//! (`FRAGMENT_DOCKER_SKIP_BUILD=1` reuses images already built).

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

fn build(dockerfile: &str, tag: &str) -> Duration {
    let t = Instant::now();
    if std::env::var("FRAGMENT_DOCKER_SKIP_BUILD").is_ok() {
        return Duration::ZERO;
    }
    let status = Command::new(docker()).args(["build", "--platform", "linux/amd64", "-f", dockerfile, "-t", tag, "."]).current_dir(images_dir()).status().expect("docker runs");
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
        Command::new(docker()).arg("exec").arg(&self.id).args(cmd).status().is_ok_and(|s| s.success())
    }

    fn logs(&self) -> String {
        let out = Command::new(docker()).args(["logs", &self.id]).output().expect("docker runs");
        format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    }

    /// SIGTERM, as the Computer DO's sleep sends it: how long until it is
    /// gone, and its exit code.
    fn sigterm(&self) -> (Duration, i64) {
        let t = Instant::now();
        assert!(Command::new(docker()).args(["kill", "--signal", "TERM", &self.id]).status().expect("docker runs").success());
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
    let built = build("stub/Dockerfile", "fragment-stub:test");
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
    let (took, code) = c.sigterm();
    eprintln!("stub: SIGTERM to exit: {} ms (code {code})", took.as_millis());
    assert!(took < Duration::from_secs(5), "{took:?}\n{}", c.logs());
    assert_eq!(code, 0);

    // The restore gate: nothing is asked of the platform until the marker.
    let calls_before = fake.with(|w| w.calls.len());
    let gated = Container::run("fragment-stub:test", fake.addr.port(), model.addr.port(), &[("RESTORE_PENDING", "1")]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(fake.with(|w| w.calls.len()), calls_before, "the gate holds: {}", gated.logs());
    let opened = Instant::now();
    assert!(gated.exec(&["touch", "/run/computer/restored"]), "the marker, as the DO touches it");
    fake.until(30_000, "the gated stub to follow", |w| w.live_sockets() >= 2).await;
    eprintln!("stub: marker to following: {} ms", opened.elapsed().as_millis());
}

/// Goal: the real Hermes image answers a chat message through its bridge
/// with the scripted model, as the agent's profile, with every model call
/// naming its agent; it holds at its restore gate; and SIGTERM ends it in
/// under 5 s.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn the_hermes_image() {
    let built = build("hermes/Dockerfile", "fragment-hermes:test");
    eprintln!("hermes: built in {:.1} s, {} MB", built.as_secs_f64(), size("fragment-hermes:test") / 1_000_000);
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    fake.with(|w| {
        let f = w.fragments.get_mut("juniper.paul").unwrap();
        f.files.insert("SOUL.md".into(), bytes::Bytes::from_static(b"You are Juniper, a careful gardener.\n"));
        f.files.insert("memories/MEMORY.md".into(), bytes::Bytes::from_static(b"Paul likes tomatoes.\n"));
        f.files.insert("agent.json".into(), bytes::Bytes::from_static(br#"{"tier":"cheap"}"#));
    });
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("talk", &["juniper"]);

    let t = Instant::now();
    let c = Container::run("fragment-hermes:test", fake.addr.port(), model.addr.port(), &[("RESTORE_PENDING", "1")]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(fake.with(|w| w.calls.is_empty()), "the gate holds: {:?}", fake.with(|w| w.calls.clone()));
    let gate = Instant::now();
    assert!(c.exec(&["touch", "/run/computer/restored"]));
    fake.until(120_000, "Hermes' bridge to follow its chat", |w| w.live_sockets() >= 2).await;
    eprintln!("hermes: marker to the bridge following: {} ms ({} ms since docker run)", gate.elapsed().as_millis(), t.elapsed().as_millis());

    // Ready means Hermes answers: the first message waits for its gateway.
    let first = fake.say(&chat, &person("paul"), json!({ "text": "hello hermes" }));
    let t1 = fragment_bridge::records::turn_id("juniper.paul", &chat, "chat", first["seq"].as_u64().unwrap());
    let asked = Instant::now();
    let answered = |w: &support::fake::World, turn: &str| w.bodies(&chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn);
    let replied = tokio::time::timeout(Duration::from_secs(180), fake.until(600_000, "Hermes' reply", |w| answered(w, &t1).is_some()));
    if replied.await.is_err() {
        let logs = c.logs();
        let _ = std::fs::write(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("hermes-container.log"), &logs);
        let records = fake.with(|w| format!("chat: {:?}\nwork: {:?}", w.bodies(&chat, "chat", "reply"), w.records(&chat, "work").iter().map(|r| r["body"].clone()).collect::<Vec<_>>()));
        panic!("no reply; the fake holds\n{records}\nthe container said:\n{}", logs.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    eprintln!("hermes: first message to reply: {} ms ({} ms since the marker)", asked.elapsed().as_millis(), gate.elapsed().as_millis());
    fake.until(30_000, "the turn's end", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t1)).await;
    fake.with(|w| {
        let reply = answered(w, &t1).unwrap();
        assert!(reply["text"].as_str().unwrap().contains("hello hermes"), "the scripted model's answer: {reply}");
        let by = w.records(&chat, "chat").into_iter().find(|r| r["body"]["turn"] == t1).unwrap()["principal"].clone();
        assert_eq!(by, "id:juniper");
        assert_eq!(w.bodies(&chat, "work", "turn.end").into_iter().find(|e| e["turn"] == t1).unwrap()["outcome"], "idle");
        let all: Vec<String> = w.bodies(&chat, "chat", "reply").iter().map(|r| r["text"].as_str().unwrap_or("").to_string()).collect();
        assert!(all.iter().all(|t| !t.contains("home channel")), "no notice from Hermes about its home: {all:?}");
    });
    {
        // Hermes also probes `/api/show` (Ollama's model metadata) with no
        // agent; the model intercept answers only the completions routes,
        // and Hermes falls back to its configured context length.
        let calls: Vec<_> = model.calls.lock().unwrap().iter().filter(|c| c.path.ends_with("/chat/completions")).cloned().collect();
        assert!(!calls.is_empty());
        assert!(calls.iter().all(|c| c.agent.as_deref() == Some("juniper.paul")), "every model call names its agent: {calls:?}");
        assert!(calls.iter().any(|c| c.model == "cheap"), "the agent's tier from agent.json: {calls:?}");
    }
    assert!(c.exec(&["test", "-f", "/data/hermes/profiles/juniper-paul/SOUL.md"]), "the agent's repo is in its profile");
    assert!(c.exec(&["grep", "-q", "Juniper", "/data/hermes/profiles/juniper-paul/SOUL.md"]));

    // A second message, warm.
    let second = fake.say(&chat, &person("paul"), json!({ "text": "again" }));
    let t2 = fragment_bridge::records::turn_id("juniper.paul", &chat, "chat", second["seq"].as_u64().unwrap());
    let warm = Instant::now();
    fake.until(120_000, "the second reply", |w| answered(w, &t2).is_some()).await;
    eprintln!("hermes: warm message to reply: {} ms", warm.elapsed().as_millis());

    let (took, code) = c.sigterm();
    eprintln!("hermes: SIGTERM to exit: {} ms (code {code})", took.as_millis());
    if took >= Duration::from_secs(5) {
        panic!("SIGTERM took {took:?}; the container said:\n{}", c.logs());
    }
}
