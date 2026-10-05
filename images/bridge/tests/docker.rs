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

/// An install for the session, offline (as the e2e's hermes lane has it):
/// a package Hermes builds and installs through apt as root, and a program
/// it puts in /usr/local/bin as root; then both run, on one line.
const INSTALL: &str = r#"d=/tmp/fragment-hello && mkdir -p $d/DEBIAN $d/usr/bin && printf 'Package: fragment-hello\nVersion: 1.0\nArchitecture: all\nMaintainer: e2e <e2e@e2e.test>\nDescription: a package an agent installs\n' > $d/DEBIAN/control && printf '#!/bin/sh\necho hello-from-apt\n' > $d/usr/bin/fragment-hello && chmod 0755 $d $d/DEBIAN $d/usr/bin/fragment-hello && dpkg-deb --build --root-owner-group $d /tmp/fragment-hello.deb > /dev/null && sudo apt-get install -y /tmp/fragment-hello.deb > /dev/null 2>&1 && printf '#!/bin/sh\necho hello-from-usr-local\n' > /tmp/fragment-hi && sudo install -m 0755 /tmp/fragment-hi /usr/local/bin/fragment-hi && echo "$(fragment-hello) $(fragment-hi)""#;

fn docker() -> String {
    std::env::var("DOCKER").unwrap_or_else(|_| if std::path::Path::new("/usr/local/bin/docker").exists() { "/usr/local/bin/docker".into() } else { "docker".into() })
}

fn images_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("images/").to_path_buf()
}

/// The repo's root: the Hermes image's build context (it carries the
/// fragment CLI, of the root workspace).
fn repo_dir() -> PathBuf {
    images_dir().parent().expect("the repo").to_path_buf()
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
    let built = build(&repo_dir(), "images/hermes/Dockerfile", "fragment-hermes:test");
    eprintln!("hermes: built in {:.1} s, {} MB", built.as_secs_f64(), size("fragment-hermes:test") / 1_000_000);
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    fake.with(|w| {
        let f = w.fragments.get_mut("juniper.paul").unwrap();
        f.files.insert("SOUL.md".into(), bytes::Bytes::from_static(b"You are Juniper, a careful gardener.\n"));
        f.files.insert("memories/MEMORY.md".into(), bytes::Bytes::from_static(b"Paul likes tomatoes.\n"));
        f.files.insert("agent.json".into(), bytes::Bytes::from_static(br#"{"tier":"cheap"}"#));
        // its own skills: one of its own, and one a managed skill also names
        f.files.insert("skills/garden-notes/SKILL.md".into(), bytes::Bytes::from_static(b"---\nname: garden-notes\ndescription: Juniper's own notes on the garden.\n---\n# Garden notes\n"));
        f.files.insert("skills/grill-me/SKILL.md".into(), bytes::Bytes::from_static(b"---\nname: grill-me\ndescription: Juniper's own grilling, which wins.\n---\n# Grill\n"));
    });
    // paul's skills fragment: the managed set (the blessed template's release, served as its files)
    let skills = fake.skills(
        "skills",
        &[
            ("fragment.json", r#"{"template":"skills"}"#),
            ("skills/research/arxiv-finite/SKILL.md", "---\nname: arxiv-finite\ndescription: Search arXiv.\n---\n# arXiv\n"),
            ("skills/research/arxiv-finite/scripts/search.py", "print('search')\n"),
            ("skills/grill-me/SKILL.md", "---\nname: grill-me\ndescription: The managed grilling.\n---\n# Grill\n"),
        ],
    );
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("talk", &["juniper"]);

    let t = Instant::now();
    let c = Container::run("fragment-hermes:test", fake.addr.port(), model.addr.port(), &[("RESTORE_PENDING", "1"), ("HERMES_BOOT_SYNC_MS", "2000"), ("HERMES_BOOT_SKILLS_MS", "2000")]);
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
    // The screen: its viewer page, noVNC, and the socket's refusal of a
    // viewer that names no id (the display itself starts at a viewer).
    assert!(c.exec_out(&["curl", "-sf", "http://127.0.0.1:6080/"]).contains("Take over"));
    assert!(c.exec(&["curl", "-sf", "-o", "/dev/null", "http://127.0.0.1:6080/novnc/core/rfb.js"]));
    assert!(!c.exec(&["curl", "-sf", "-o", "/dev/null", "http://127.0.0.1:6080/websockify"]));
    assert!(c.exec(&["test", "-f", "/data/hermes/profiles/juniper-paul/SOUL.md"]), "the agent's repo is in its profile");
    assert!(c.exec(&["grep", "-q", "Juniper", "/data/hermes/profiles/juniper-paul/SOUL.md"]));
    // What Hermes writes in its profile is committed back to the agent's
    // fragment, as the agent.
    assert!(c.exec(&["sh", "-c", "printf 'Paul likes tomatoes.\\nAnd basil.\\n' > /data/hermes/profiles/juniper-paul/memories/MEMORY.md"]));
    fake.until(30_000, "the memory committed back", |w| w.fragments["juniper.paul"].files.get("memories/MEMORY.md").is_some_and(|b| b.as_ref() == b"Paul likes tomatoes.\nAnd basil.\n")).await;

    // The managed skills (decision 17): paul's skills fragment's `skills/`,
    // installed read-only where every profile looks after its own, read as
    // the agent acting for paul; its own skills win on a name.
    let managed = "/data/hermes/managed-skills";
    let installed = |path: &str| c.exec(&["test", "-f", &format!("{managed}/{path}")]);
    let t_skills = Instant::now();
    while !(installed("research/arxiv-finite/SKILL.md") && installed("grill-me/SKILL.md")) {
        assert!(t_skills.elapsed() < Duration::from_secs(60), "the managed skills never installed; the container said:\n{}", c.logs());
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(installed("research/arxiv-finite/scripts/search.py") && !installed("fragment.json"), "the managed set is the fragment's skills/, nothing else of it");
    assert!(c.exec(&["/command/s6-setuidgid", "hermes", "sh", "-c", "true"]), "a command runs as Hermes' user");
    assert!(!c.exec(&["/command/s6-setuidgid", "hermes", "sh", "-c", &format!("echo x > {managed}/research/arxiv-finite/SKILL.md")]), "the managed set is read-only to the agents");
    fake.with(|w| {
        let reads: Vec<_> = w.requests.iter().filter(|r| r.0.starts_with("GET /api/f/skills.paul/") || r.0 == "GET /api/fragments" && r.1.contains("for=")).collect();
        assert!(!reads.is_empty() && reads.iter().all(|r| r.1.contains("for=id%3Apaul") && r.2.as_deref() == Some("juniper.paul") && !r.3), "read as the agent acting for its owner, unsigned: {reads:?}");
    });
    assert!(c.exec_out(&["cat", "/data/hermes/profiles/juniper-paul/config.yaml"]).contains(&format!("external_dirs: [\"{managed}\"]")), "its profile names the managed skills");
    // what Hermes itself finds for the profile: its own first, then the managed set
    let found = c.exec_out(&[
        "/command/s6-setuidgid", "hermes", "env", "HERMES_HOME=/data/hermes/profiles/juniper-paul", "HOME=/data/hermes/profiles/juniper-paul/home",
        "/opt/hermes/.venv/bin/python", "-c", "import json, os; os.chdir('/opt/hermes'); from tools.skills_tool import _find_all_skills; print(json.dumps({s['name']: s['description'] for s in _find_all_skills(skip_disabled=True)}))",
    ]);
    let found: serde_json::Value = serde_json::from_str(found.trim().lines().last().unwrap_or("{}")).unwrap_or_else(|e| panic!("Hermes' skills: {e}: {found}"));
    eprintln!("hermes: {} skills for juniper's profile", found.as_object().map_or(0, |o| o.len()));
    assert!(found["arxiv-finite"].as_str().is_some_and(|d| d.contains("Search arXiv")), "a managed skill: {found}");
    assert!(found["garden-notes"].as_str().is_some_and(|d| d.contains("Juniper's own")), "its own skill: {found}");
    assert!(found["grill-me"].as_str().is_some_and(|d| d.contains("which wins")), "its own wins on a name: {found}");

    let login = c.exec_out(&["sh", "-c", "env FRAGMENT_AS_AGENT=juniper.paul FRAGMENT_FOR=id:paul fragment login 2>&1; echo exit=$?"]);
    assert!(login.contains("needs no login") && login.contains("exit=2"), "an agent logs in to nothing: {login}");

    // a change to the managed set is followed while it runs
    fake.with(|w| {
        let f = w.fragments.get_mut(&skills).unwrap();
        f.files.insert("skills/research/arxiv-finite/SKILL.md".into(), bytes::Bytes::from_static(b"---\nname: arxiv-finite\ndescription: Search arXiv, again.\n---\n"));
        f.files.remove("skills/research/arxiv-finite/scripts/search.py");
    });
    let t_follow = Instant::now();
    while !(c.exec_out(&["cat", &format!("{managed}/research/arxiv-finite/SKILL.md")]).contains("again") && !installed("research/arxiv-finite/scripts/search.py")) {
        assert!(t_follow.elapsed() < Duration::from_secs(60), "the managed skills never followed the change; the container said:\n{}", c.logs());
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // A second message, warm.
    let second = fake.say(&chat, &person("paul"), json!({ "text": "again" }));
    let t2 = fragment_bridge::records::turn_id("juniper.paul", &chat, "chat", second["seq"].as_u64().unwrap());
    let warm = Instant::now();
    fake.until(120_000, "the second reply", |w| answered(w, &t2).is_some()).await;
    eprintln!("hermes: warm message to reply: {} ms", warm.elapsed().as_millis());

    // A tool call: Hermes' progress line is a step, then its answer.
    let third = fake.say(&chat, &person("paul"), json!({ "text": "please use the terminal" }));
    let t3 = fragment_bridge::records::turn_id("juniper.paul", &chat, "chat", third["seq"].as_u64().unwrap());
    fake.until(120_000, "the tool turn's end", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t3)).await;
    // what the model was given last: each message's role and the start of its text
    let model_saw = || {
        let calls = model.calls.lock().unwrap();
        calls.iter().rev().take(2).map(|c| c.body["messages"].as_array().map(|m| m.iter().rev().take(6).map(|m| format!("{}: {}", m["role"], m["content"].to_string().chars().take(160).collect::<String>())).collect::<Vec<_>>()).unwrap_or_default()).collect::<Vec<_>>()
    };
    fake.with(|w| {
        let steps: Vec<_> = w.bodies(&chat, "work", "turn.step").into_iter().filter(|s| s["turn"] == t3).collect();
        assert!(steps.iter().any(|s| s["tool"] == "terminal"), "a terminal step: {steps:?}; the reply {:?}; the model saw (newest first) {:#?}", answered(w, &t3), model_saw());
        assert!(answered(w, &t3).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("the tool ran")), "{:?}", answered(w, &t3));
    });

    // An approval: Hermes flags `rm -rf`, its guardian escalates, the card
    // is the owner's, the owner's answer runs it.
    let fourth = fake.say(&chat, &person("paul"), json!({ "text": "do the risky thing" }));
    let t4 = fragment_bridge::records::turn_id("juniper.paul", &chat, "chat", fourth["seq"].as_u64().unwrap());
    fake.until(120_000, "Hermes' approval card", |w| w.bodies(&chat, "work", "turn.prompt").iter().any(|p| p["turn"] == t4)).await;
    let card = fake.with(|w| w.bodies(&chat, "work", "turn.prompt").into_iter().find(|p| p["turn"] == t4).unwrap());
    eprintln!("hermes: approval card options {}", card["options"]);
    assert_eq!(card["asks"], "id:paul");
    fake.until(30_000, "the keepalive dropped while it waits", |w| w.keepalive_open == 0).await;
    fake.say(&chat, &person("paul"), json!({ "kind": "prompt_response", "prompt": card["prompt"], "option": "once" }));
    fake.until(120_000, "the approved turn's answer", |w| answered(w, &t4).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("the tool ran"))).await;
    fake.with(|w| assert_eq!(w.bodies(&chat, "work", "turn.prompt.closed").into_iter().find(|p| p["turn"] == t4).unwrap()["outcome"], "answered"));

    // An agent assigned while it runs (docs/computers.md: a computer's
    // agents may change while it runs): hermes-boot writes its profile,
    // the gateway serves it, and it answers in its own chat as itself,
    // within seconds, while the other agent's slow turn runs on, whole.
    let fifth = fake.say(&chat, &person("paul"), json!({ "text": "please use the terminal slowly" }));
    let t5 = fragment_bridge::records::turn_id("juniper.paul", &chat, "chat", fifth["seq"].as_u64().unwrap());
    fake.until(60_000, "juniper's slow turn running", |w| w.keepalive_open == 1 && w.bodies(&chat, "work", "turn.step").iter().any(|s| s["turn"] == t5)).await;
    let calls_before = model.calls.lock().unwrap().len();
    let assigned = Instant::now();
    fake.add_agent("maple");
    fake.with(|w| {
        w.fragments.get_mut("maple.paul").unwrap().files.insert("SOUL.md".into(), bytes::Bytes::from_static(b"You are Maple, who tends the trees.\n"));
    });
    let grove = fake.chat("grove", &["maple"]);
    let hello = fake.say(&grove, &person("paul"), json!({ "text": "hello maple" }));
    let tm = fragment_bridge::records::turn_id("maple.paul", &grove, "chat", hello["seq"].as_u64().unwrap());
    let maple_replied = |w: &support::fake::World| w.bodies(&grove, "chat", "reply").into_iter().find(|r| r["turn"] == tm);
    if tokio::time::timeout(Duration::from_secs(60), fake.until(60_000, "maple's reply", |w| maple_replied(w).is_some())).await.is_err() {
        panic!("maple never answered; the container said:\n{}", c.logs().lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    eprintln!("hermes: an agent assigned while it runs to its first reply: {} ms", assigned.elapsed().as_millis());
    fake.until(60_000, "juniper's slow turn's end", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t5)).await;
    fake.with(|w| {
        let reply = maple_replied(w).unwrap();
        assert!(reply["text"].as_str().unwrap_or("").contains("hello maple"), "the scripted model's answer, as Maple's profile: {reply}");
        assert_eq!(w.records(&grove, "chat").into_iter().find(|r| r["body"]["turn"] == tm).unwrap()["principal"], "id:maple");
        assert_eq!(w.bodies(&chat, "work", "turn.end").into_iter().find(|e| e["turn"] == t5).unwrap()["outcome"], "idle", "juniper's turn ran on through the change");
        assert!(answered(w, &t5).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("the tool ran")), "{:?}", answered(w, &t5));
    });
    {
        let calls: Vec<_> = model.calls.lock().unwrap()[calls_before..].iter().filter(|c| c.path.ends_with("/chat/completions")).cloned().collect();
        assert!(calls.iter().any(|c| c.agent.as_deref() == Some("maple.paul")), "Maple's calls name Maple: {calls:?}");
        assert!(calls.iter().all(|c| c.agent.is_some()), "no call without its agent (the 401 of a profile that does not exist): {calls:?}");
    }
    assert!(c.exec(&["grep", "-q", "Maple", "/data/hermes/profiles/maple-paul/SOUL.md"]), "its repo in its own profile");
    assert!(c.exec(&["grep", "-q", "maple.paul", "/var/lib/fragment-run/agents.json"]), "the bridge's ready file names it");

    // Its terminal runs the fragment CLI as itself, acting for paul: the
    // computer's API (here the fake) sees the agent named, no signature,
    // and `for` its owner; the answer is paul's fragments.
    let requests_before = fake.with(|w| w.requests.len());
    let listed = fake.say(&chat, &person("paul"), json!({ "text": "run: fragment list --json" }));
    let tl = fragment_bridge::records::turn_id("juniper.paul", &chat, "chat", listed["seq"].as_u64().unwrap());
    fake.until(120_000, "the fragment CLI's answer", |w| answered(w, &tl).is_some()).await;
    fake.with(|w| {
        let reply = answered(w, &tl).unwrap();
        let text = reply["text"].as_str().unwrap_or("");
        assert!(text.contains("the tool said") && text.contains("skills.paul") && text.contains("talk.paul"), "paul's fragments, from the CLI: {reply}");
        let cli: Vec<_> = w.requests[requests_before..].iter().filter(|r| r.0 == "GET /api/fragments").collect();
        assert!(cli.iter().any(|r| r.2.as_deref() == Some("juniper.paul") && r.1 == "for=id%3Apaul" && !r.3), "as juniper, for paul, unsigned: {cli:?}");
    });

    // Root for the session (docs/computers.md, "Root in our Hermes image"):
    // its terminal installs, offline, a package it builds through apt and a
    // program into /usr/local/bin, both with passwordless sudo, and runs
    // them.
    let install = fake.say(&chat, &person("paul"), json!({ "text": format!("run: {INSTALL}") }));
    let ti = fragment_bridge::records::turn_id("juniper.paul", &chat, "chat", install["seq"].as_u64().unwrap());
    fake.until(120_000, "the install's answer", |w| answered(w, &ti).is_some()).await;
    fake.with(|w| {
        let reply = answered(w, &ti).unwrap();
        assert!(reply["text"].as_str().unwrap_or("").contains("hello-from-apt hello-from-usr-local"), "installed as root, and run: {reply}");
    });
    assert!(c.exec(&["dpkg", "-s", "fragment-hello"]), "installed as a package, through apt");
    assert!(!c.exec(&["/command/s6-setuidgid", "hermes", "sh", "-c", "echo x > /usr/local/bin/fragment-hi"]), "the system's directories stay root's: an install goes through sudo");

    let (took, code) = c.sigterm();
    eprintln!("hermes: SIGTERM to exit: {} ms (code {code})", took.as_millis());
    if took >= Duration::from_secs(5) {
        panic!("SIGTERM took {took:?}; the container said:\n{}", c.logs());
    }
}
