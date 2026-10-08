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
//! (`FRAGMENT_DOCKER_SKIP_BUILD=1` reuses images already built;
//! `FRAGMENT_DOCKER_HERMES_TAG` names the Hermes image's tag). CI runs it
//! (.github/workflows/images.yml, `docker`) on pull requests and master's
//! pushes that touch images/hermes, images/bridge or images/stub: the images
//! built first, then this with `FRAGMENT_DOCKER_SKIP_BUILD=1`.

mod support;

use std::collections::BTreeMap;
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

/// The Hermes image's tag (`FRAGMENT_DOCKER_HERMES_TAG`, so two checkouts
/// on one Docker never build over each other's).
fn hermes_tag() -> String {
    std::env::var("FRAGMENT_DOCKER_HERMES_TAG").unwrap_or_else(|_| "fragment-hermes:test".into())
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

/// Where the container runs: Docker as it is, or as Cloudflare Containers
/// runs an image, where Docker's defaults differ.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Runtime {
    Docker,
    /// No `/dev/shm` (`--ipc=none`; Docker mounts a 64 MB one in every
    /// container, Containers none), and no `/.dockerenv`, Docker's marker,
    /// which Hermes reads as "in a container": to start Chromium with
    /// `--no-sandbox --disable-dev-shm-usage` (p5, 2026-10-05: the agent's
    /// browser died for want of `/dev/shm`), and for the rest of its
    /// container guesses (docs/computers.md). Without it Hermes takes this
    /// for a host.
    Hosted,
}

impl Container {
    fn run(tag: &str, api: u16, model: u16, extra: &[(&str, &str)]) -> Container {
        Container::run_on(Runtime::Docker, tag, api, model, extra)
    }

    fn run_on(runtime: Runtime, tag: &str, api: u16, model: u16, extra: &[(&str, &str)]) -> Container {
        let mut args: Vec<String> = ["run", "-d", "--platform", "linux/amd64", "--add-host", "api.fragment.internal:host-gateway", "--add-host", "model.fragment.internal:host-gateway"].iter().map(|s| s.to_string()).collect();
        if runtime == Runtime::Hosted {
            // the marker goes before the image's own entrypoint runs, as PID 1
            args.extend(["--ipc=none", "--entrypoint", "/bin/sh"].map(String::from));
        }
        for (k, v) in [("FRAGMENT_API", format!("http://api.fragment.internal:{api}")), ("FRAGMENT_MODEL", format!("http://model.fragment.internal:{model}")), ("FRAGMENT_COMPUTER", "computer:00aa".into()), ("FRAGMENT_IMAGE", tag.into())] {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        for (k, v) in extra {
            args.push("-e".into());
            args.push(format!("{k}={v}"));
        }
        // its screen's port, on a port of this host's loopback (`port`)
        args.extend(["-p".into(), "127.0.0.1::6080".into()]);
        args.push(tag.into());
        if runtime == Runtime::Hosted {
            args.extend(["-c", "rm -f /.dockerenv && exec /opt/fragment/bin/hermes-boot pre-init"].map(String::from));
        }
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

    /// `cmd`'s exit code and what it printed.
    fn exec_code(&self, cmd: &[&str]) -> (i32, String) {
        let out = Command::new(docker()).arg("exec").arg(&self.id).args(cmd).output().expect("docker runs");
        (out.status.code().unwrap_or(-1), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    /// A container of `tag` that runs nothing but a sleep: a disk to restore
    /// into and check, as a fresh start's is before its gate opens.
    fn idle(tag: &str) -> Container {
        let out = Command::new(docker()).args(["run", "-d", "--platform", "linux/amd64", "--entrypoint", "/bin/sleep", tag, "infinity"]).output().expect("docker runs");
        assert!(out.status.success(), "docker run {tag}: {}", String::from_utf8_lossy(&out.stderr));
        Container { id: String::from_utf8_lossy(&out.stdout).trim().to_string() }
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

/// Goal: the real Hermes image answers a chat message through its bridge
/// with the scripted model, as the agent's profile, with every model call
/// naming its agent; it holds at its restore gate; and SIGTERM ends it in
/// under 5 s.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn the_hermes_image() {
    let tag = hermes_tag();
    let built = build(&repo_dir(), "images/hermes/Dockerfile", &tag);
    eprintln!("hermes: built in {:.1} s, {} MB", built.as_secs_f64(), size(&tag) / 1_000_000);
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    fake.with(|w| {
        let f = w.fragments.get_mut("juniper--k3x9").unwrap();
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
    let c = Container::run(&tag, fake.addr.port(), model.addr.port(), &[("RESTORE_PENDING", "1"), ("HERMES_BOOT_SYNC_MS", "2000"), ("HERMES_BOOT_SKILLS_MS", "2000")]);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(fake.with(|w| w.calls.is_empty()), "the gate holds: {:?}", fake.with(|w| w.calls.clone()));
    let gate = Instant::now();
    assert!(c.exec(&["touch", "/run/computer/restored"]));
    fake.until(120_000, "Hermes' bridge to follow its chat", |w| w.live_sockets() >= 2).await;
    eprintln!("hermes: marker to the bridge following: {} ms ({} ms since docker run)", gate.elapsed().as_millis(), t.elapsed().as_millis());

    // Ready means Hermes answers: the first message waits for its gateway.
    let first = fake.say(&chat, &person("paul"), json!({ "text": "hello hermes" }));
    let t1 = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", first["seq"].as_u64().unwrap());
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
        assert_eq!(by, "npub1juniper");
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
        assert!(calls.iter().all(|c| c.agent.as_deref() == Some("juniper--k3x9")), "every model call names its agent: {calls:?}");
        assert!(calls.iter().any(|c| c.model == "cheap"), "the agent's tier from agent.json: {calls:?}");
        // A fresh profile's first message carries no first-contact note of
        // Hermes' (v0.21.5 appended "This is the user's very first message
        // ever…", and the model introduced itself and named /help; since
        // v0.21.6 Hermes adds it only in a direct message, and the bridge's
        // chats are groups): the model is asked exactly what was said.
        let first: Vec<String> = calls.iter().map(|c| c.body["messages"].to_string()).filter(|m| m.contains("hello hermes")).collect();
        assert!(!first.is_empty() && first.iter().all(|m| !m.contains("very first message")), "no first-contact note in the first message's request: {first:?}");
    }
    // The screen: its viewer page, noVNC, and the socket's refusal of a
    // viewer that names no id and no agent (a display starts at a viewer).
    assert!(c.exec_out(&["curl", "-sf", "http://127.0.0.1:6080/"]).contains("Take over"));
    assert!(c.exec(&["curl", "-sf", "-o", "/dev/null", "http://127.0.0.1:6080/novnc/core/rfb.js"]));
    assert!(!c.exec(&["curl", "-sf", "-o", "/dev/null", "http://127.0.0.1:6080/websockify"]));
    assert!(c.exec(&["test", "-f", "/data/hermes/profiles/juniper--k3x9/SOUL.md"]), "the agent's repo is in its profile");
    // the seam (step 2 of docs/durable-computers.md): its work is its own
    assert!(c.exec(&["test", "-d", "/data/work/juniper--k3x9/browser-profile"]), "its work directory, with its browser's profile");
    assert_eq!(c.exec_out(&["readlink", "/data/hermes/profiles/juniper--k3x9/bot-desktop/browser-profile"]).trim(), "/data/work/juniper--k3x9/browser-profile", "Hermes' browser profile is a link into its work");
    assert!(c.exec(&["grep", "-q", "Juniper", "/data/hermes/profiles/juniper--k3x9/SOUL.md"]));
    // What Hermes writes in its profile is committed back to the agent's
    // fragment, as the agent.
    assert!(c.exec(&["sh", "-c", "printf 'Paul likes tomatoes.\\nAnd basil.\\n' > /data/hermes/profiles/juniper--k3x9/memories/MEMORY.md"]));
    fake.until(30_000, "the memory committed back", |w| w.fragments["juniper--k3x9"].files.get("memories/MEMORY.md").is_some_and(|b| b.as_ref() == b"Paul likes tomatoes.\nAnd basil.\n")).await;

    // The managed skills (decision 17): paul's skills fragment's `skills/`,
    // installed read-only where every profile looks after its own, read as
    // the agent acting for paul; its own skills win on a name.
    let (managed, view) = ("/data/hermes/managed-skills", "/var/lib/fragment-run/platform-skills");
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
        let reads: Vec<_> = w.requests.iter().filter(|r| r.0.starts_with("GET /api/f/skills--k3x9/") || r.0 == "GET /api/fragments" && r.1.contains("for=")).collect();
        assert!(!reads.is_empty() && reads.iter().all(|r| r.1.contains("for=npub1paul") && r.2.as_deref() == Some("juniper--k3x9") && !r.3), "read as the agent acting for its owner, unsigned: {reads:?}");
    });
    assert!(
        c.exec_out(&["cat", "/data/hermes/profiles/juniper--k3x9/config.yaml"]).contains(&format!("external_dirs: [\"{managed}\", \"{view}\"]")),
        "its profile names the managed skills and the platform skill's view"
    );
    // what Hermes itself finds for the profile: its own first, then the
    // managed set and the platform skill (the image's: its CLI's own)
    let hermes_finds = || {
        let found = c.exec_out(&[
            "/command/s6-setuidgid", "hermes", "env", "HERMES_HOME=/data/hermes/profiles/juniper--k3x9", "HOME=/data/hermes/profiles/juniper--k3x9/home",
            "/opt/hermes/.venv/bin/python", "-c", "import json, os; os.chdir('/opt/hermes'); from tools.skills_tool import _find_all_skills; print(json.dumps({s['name']: s['description'] for s in _find_all_skills(skip_disabled=True)}))",
        ]);
        serde_json::from_str::<serde_json::Value>(found.trim().lines().last().unwrap_or("{}")).unwrap_or_else(|e| panic!("Hermes' skills: {e}: {found}"))
    };
    let found = hermes_finds();
    eprintln!("hermes: {} skills for juniper's profile", found.as_object().map_or(0, |o| o.len()));
    assert!(found["arxiv-finite"].as_str().is_some_and(|d| d.contains("Search arXiv")), "a managed skill: {found}");
    assert!(found["garden-notes"].as_str().is_some_and(|d| d.contains("Juniper's own")), "its own skill: {found}");
    assert!(found["grill-me"].as_str().is_some_and(|d| d.contains("which wins")), "its own wins on a name: {found}");
    assert!(found["fragment"].as_str().is_some_and(|d| d.starts_with("You are an agent on a Fragment computer")), "the platform skill: {found}");
    let platform = c.exec_out(&["cat", "/opt/fragment/skills/platform/fragment/SKILL.md"]);
    assert_eq!(c.exec_out(&["cat", &format!("{view}/platform/fragment/SKILL.md")]), platform, "the view shows the image's platform skill");
    let cli = c.exec_out(&["fragment", "skill"]);
    let cli_body = cli.split_once("\n---\n").map_or("", |(_, body)| body.trim());
    assert!(platform.contains("# Your computer") && !cli_body.is_empty() && platform.contains(cli_body), "the platform skill is the image's CLI's own, after the computer's page:\n{platform}");
    assert!(!c.exec(&["/command/s6-setuidgid", "hermes", "sh", "-c", "echo x > /opt/fragment/skills/platform/fragment/SKILL.md"]), "the platform skill is read-only to the agents");
    assert!(!c.exec(&["/command/s6-setuidgid", "hermes", "sh", "-c", &format!("echo x > {view}/platform/fragment/SKILL.md")]), "and so is its view");

    let login = c.exec_out(&["sh", "-c", "env FRAGMENT_AS_AGENT=juniper--k3x9 FRAGMENT_FOR=npub1paul fragment login 2>&1; echo exit=$?"]);
    assert!(login.contains("needs no login") && login.contains("exit=2"), "an agent logs in to nothing: {login}");

    // a change to the managed set is followed while it runs: one skill
    // changed, a file gone, and a managed `fragment`, which shadows the
    // platform's (it leaves the view: Hermes finds neither of two of one
    // name in its external dirs)
    fake.with(|w| {
        let f = w.fragments.get_mut(&skills).unwrap();
        f.files.insert("skills/research/arxiv-finite/SKILL.md".into(), bytes::Bytes::from_static(b"---\nname: arxiv-finite\ndescription: Search arXiv, again.\n---\n"));
        f.files.remove("skills/research/arxiv-finite/scripts/search.py");
        f.files.insert("skills/fragment/SKILL.md".into(), bytes::Bytes::from_static(b"---\nname: fragment\ndescription: The managed fragment, which wins.\n---\n"));
    });
    let t_follow = Instant::now();
    let shown = || c.exec(&["test", "-e", &format!("{view}/platform/fragment/SKILL.md")]);
    while !(c.exec_out(&["cat", &format!("{managed}/research/arxiv-finite/SKILL.md")]).contains("again") && !installed("research/arxiv-finite/scripts/search.py") && installed("fragment/SKILL.md") && !shown()) {
        assert!(t_follow.elapsed() < Duration::from_secs(60), "the managed skills never followed the change; the container said:\n{}", c.logs());
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let found = hermes_finds();
    assert!(found["fragment"].as_str().is_some_and(|d| d.contains("which wins")), "a managed `fragment` shadows the platform skill: {found}");

    // A second message, warm.
    let second = fake.say(&chat, &person("paul"), json!({ "text": "again" }));
    let t2 = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", second["seq"].as_u64().unwrap());
    let warm = Instant::now();
    fake.until(120_000, "the second reply", |w| answered(w, &t2).is_some()).await;
    eprintln!("hermes: warm message to reply: {} ms", warm.elapsed().as_millis());

    // A tool call: Hermes' progress line is a step, then its answer. The
    // command sleeps 2 s first: a turn that ends before Hermes' progress
    // sender next polls (every 0.3 s) after its tool starts sends no
    // progress line (the debt ledger, "A quick tool's step can be lost in
    // Hermes").
    let third = fake.say(&chat, &person("paul"), json!({ "text": "please use the terminal" }));
    let t3 = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", third["seq"].as_u64().unwrap());
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

    // Two tool calls back to back, the second within Hermes' 1.5 s progress
    // edit interval of the first (a real model's quick call after a skill
    // read, seen on a preview): each is a step. Upstream's sender kept the
    // second line until a newer one came, and none did (the image's patch
    // of `send_progress_messages`, images/hermes/Dockerfile).
    let twice = fake.say(&chat, &person("paul"), json!({ "text": "please use the terminal twice" }));
    let t_twice = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", twice["seq"].as_u64().unwrap());
    fake.until(120_000, "the two-tool turn's end", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t_twice)).await;
    fake.with(|w| {
        let steps: Vec<_> = w.bodies(&chat, "work", "turn.step").into_iter().filter(|s| s["turn"] == t_twice).collect();
        let args: Vec<&str> = steps.iter().filter(|s| s["tool"] == "terminal").filter_map(|s| s["args"].as_str()).collect();
        assert_eq!(args, ["echo first-ran", "sleep 2 && echo tool-ran"], "a step for each terminal call, in order: {steps:?}; the reply {:?}", answered(w, &t_twice));
        assert!(answered(w, &t_twice).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("the tool ran")), "{:?}", answered(w, &t_twice));
    });

    // An approval: Hermes flags `rm -rf`, its guardian escalates, the card
    // is the owner's, the owner's answer runs it.
    let fourth = fake.say(&chat, &person("paul"), json!({ "text": "do the risky thing" }));
    let t4 = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", fourth["seq"].as_u64().unwrap());
    fake.until(120_000, "Hermes' approval card", |w| w.bodies(&chat, "work", "turn.prompt").iter().any(|p| p["turn"] == t4)).await;
    let card = fake.with(|w| w.bodies(&chat, "work", "turn.prompt").into_iter().find(|p| p["turn"] == t4).unwrap());
    eprintln!("hermes: approval card options {}", card["options"]);
    assert_eq!(card["asks"], "npub1paul");
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(fake.with(|w| w.keepalive_open), 1, "the card holds the computer awake");
    fake.say(&chat, &person("paul"), json!({ "kind": "prompt_response", "prompt": card["prompt"], "option": "once" }));
    fake.until(120_000, "the approved turn's answer", |w| answered(w, &t4).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("the tool ran"))).await;
    fake.with(|w| assert_eq!(w.bodies(&chat, "work", "turn.prompt.closed").into_iter().find(|p| p["turn"] == t4).unwrap()["outcome"], "answered"));

    // An agent assigned while it runs (docs/computers.md: a computer's
    // agents may change while it runs): hermes-boot writes its profile,
    // the gateway serves it, and it answers in its own chat as itself,
    // within seconds, while the other agent's slow turn runs on, whole.
    let fifth = fake.say(&chat, &person("paul"), json!({ "text": "please use the terminal slowly" }));
    let t5 = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", fifth["seq"].as_u64().unwrap());
    fake.until(60_000, "juniper's slow turn running", |w| w.keepalive_open == 1 && w.bodies(&chat, "work", "turn.step").iter().any(|s| s["turn"] == t5)).await;
    let calls_before = model.calls.lock().unwrap().len();
    let assigned = Instant::now();
    fake.add_agent("maple");
    fake.with(|w| {
        w.fragments.get_mut("maple--k3x9").unwrap().files.insert("SOUL.md".into(), bytes::Bytes::from_static(b"You are Maple, who tends the trees.\n"));
    });
    let grove = fake.chat("grove", &["maple"]);
    let hello = fake.say(&grove, &person("paul"), json!({ "text": "hello maple" }));
    let tm = fragment_bridge::records::turn_id("maple--k3x9", &grove, "chat", hello["seq"].as_u64().unwrap());
    let maple_replied = |w: &support::fake::World| w.bodies(&grove, "chat", "reply").into_iter().find(|r| r["turn"] == tm);
    if tokio::time::timeout(Duration::from_secs(60), fake.until(60_000, "maple's reply", |w| maple_replied(w).is_some())).await.is_err() {
        panic!("maple never answered; the container said:\n{}", c.logs().lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    eprintln!("hermes: an agent assigned while it runs to its first reply: {} ms", assigned.elapsed().as_millis());
    fake.until(60_000, "juniper's slow turn's end", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t5)).await;
    fake.with(|w| {
        let reply = maple_replied(w).unwrap();
        assert!(reply["text"].as_str().unwrap_or("").contains("hello maple"), "the scripted model's answer, as Maple's profile: {reply}");
        assert_eq!(w.records(&grove, "chat").into_iter().find(|r| r["body"]["turn"] == tm).unwrap()["principal"], "npub1maple");
        assert_eq!(w.bodies(&chat, "work", "turn.end").into_iter().find(|e| e["turn"] == t5).unwrap()["outcome"], "idle", "juniper's turn ran on through the change");
        assert!(answered(w, &t5).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("the tool ran")), "{:?}", answered(w, &t5));
    });
    {
        let calls: Vec<_> = model.calls.lock().unwrap()[calls_before..].iter().filter(|c| c.path.ends_with("/chat/completions")).cloned().collect();
        assert!(calls.iter().any(|c| c.agent.as_deref() == Some("maple--k3x9")), "Maple's calls name Maple: {calls:?}");
        assert!(calls.iter().all(|c| c.agent.is_some()), "no call without its agent (the 401 of a profile that does not exist): {calls:?}");
    }
    assert!(c.exec(&["grep", "-q", "Maple", "/data/hermes/profiles/maple--k3x9/SOUL.md"]), "its repo in its own profile");
    assert!(c.exec(&["grep", "-q", "maple--k3x9", "/var/lib/fragment-run/agents.json"]), "the bridge's ready file names it");

    // Its terminal runs the fragment CLI as itself, acting for paul: the
    // computer's API (here the fake) sees the agent named, no signature,
    // and `for` its owner; the answer is paul's fragments.
    let requests_before = fake.with(|w| w.requests.len());
    let listed = fake.say(&chat, &person("paul"), json!({ "text": "run: fragment list --json" }));
    let tl = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", listed["seq"].as_u64().unwrap());
    fake.until(120_000, "the fragment CLI's answer", |w| answered(w, &tl).is_some()).await;
    fake.with(|w| {
        let reply = answered(w, &tl).unwrap();
        let text = reply["text"].as_str().unwrap_or("");
        assert!(text.contains("the tool said") && text.contains("skills--k3x9") && text.contains("talk--k3x9"), "paul's fragments, from the CLI: {reply}");
        let cli: Vec<_> = w.requests[requests_before..].iter().filter(|r| r.0 == "GET /api/fragments").collect();
        assert!(cli.iter().any(|r| r.2.as_deref() == Some("juniper--k3x9") && r.1 == "for=npub1paul" && !r.3), "as juniper, for paul, unsigned: {cli:?}");
    });

    // Root for the session (docs/computers.md, "Root in our Hermes image"):
    // its terminal installs, offline, a package it builds through apt and a
    // program into /usr/local/bin, both with passwordless sudo, and runs
    // them.
    let install = fake.say(&chat, &person("paul"), json!({ "text": format!("run: {INSTALL}") }));
    let ti = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", install["seq"].as_u64().unwrap());
    fake.until(120_000, "the install's answer", |w| answered(w, &ti).is_some()).await;
    fake.with(|w| {
        let reply = answered(w, &ti).unwrap();
        assert!(reply["text"].as_str().unwrap_or("").contains("hello-from-apt hello-from-usr-local"), "installed as root, and run: {reply}");
    });
    assert!(c.exec(&["dpkg", "-s", "fragment-hello"]), "installed as a package, through apt");
    assert!(!c.exec(&["/command/s6-setuidgid", "hermes", "sh", "-c", "echo x > /usr/local/bin/fragment-hi"]), "the system's directories stay root's: an install goes through sudo");
    // and `sudo npm install -g` installs where its terminal runs programs
    // from, not into Hermes' tool store (npm's own prefix since Hermes' PM)
    let npm = fake.say(&chat, &person("paul"), json!({ "text": "run: echo npm-global=$(sudo npm prefix -g)" }));
    let tn = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", npm["seq"].as_u64().unwrap());
    fake.until(120_000, "npm's answer", |w| answered(w, &tn).is_some()).await;
    fake.with(|w| {
        let reply = answered(w, &tn).unwrap();
        assert!(reply["text"].as_str().unwrap_or("").contains("npm-global=/usr/local\""), "npm's global prefix, as root: {reply}");
    });
    // Hermes' file tools (write_file, patch) may write where its terminal
    // works, its home and /tmp, and nowhere else (HERMES_WRITE_SAFE_ROOT),
    // as Hermes' own check decides
    let denied = |path: &str| {
        let check = format!("from agent.file_safety import is_write_denied; import sys; sys.exit(1 if is_write_denied({path:?}) else 0)");
        !c.exec(&["/command/s6-setuidgid", "hermes", "/opt/hermes/.venv/bin/python", "-c", &check])
    };
    assert!(!denied("/data/work/juniper--k3x9/notes.txt"), "its file tools write its work directory");
    assert!(!denied("/data/hermes/profiles/juniper--k3x9/notes.txt") && !denied("/tmp/notes.txt"), "and its home and /tmp");
    assert!(denied("/usr/local/bin/notes"), "and not the system's directories");

    let (took, code) = c.sigterm();
    eprintln!("hermes: SIGTERM to exit: {} ms (code {code})", took.as_millis());
    if took >= Duration::from_secs(5) {
        panic!("SIGTERM took {took:?}; the container said:\n{}", c.logs());
    }
}

// ---- saves under the hold (docs/computers.md, "The hold"; rung 3 of
// docs/explorations/pi-durable.md) ----

/// The platform's hold of `/data`, as the Computer DO makes it, in a
/// running container: its answer (`held`, within 20 s) and what it names
/// as left out.
fn hold(c: &Container) -> Option<Vec<String>> {
    assert!(c.exec(&["sh", "-c", "rm -f /run/computer/held && mkdir -p /run/computer && touch /run/computer/hold"]), "the hold");
    let t = Instant::now();
    // bounded by the DO's 20 s
    while t.elapsed() < Duration::from_secs(20) {
        if c.exec(&["test", "-e", "/run/computer/held"]) {
            return Some(c.exec_out(&["cat", "/run/computer/held"]).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn unhold(c: &Container) {
    assert!(c.exec(&["rm", "-f", "/run/computer/hold", "/run/computer/held"]), "the hold let go");
}

/// One save, as the Computer DO takes it: `/data` read out whole as a tar
/// (the DirectoryBackup's archive, on this rung), leaving out what `left_out`
/// names (anchored patterns, `/` being `/data`, as gitignore reads them).
/// Its bytes.
fn save_data(c: &Container, left_out: &[String], to: &std::path::Path) -> u64 {
    let mut args = vec!["exec".to_string(), c.id.clone(), "tar".into(), "-C".into(), "/data".into(), "-cf".into(), "-".into(), "--anchored".into()];
    for p in left_out {
        assert!(p.starts_with('/'), "our image names exactly what it copied: {p}");
        args.push(format!("--exclude=.{p}"));
    }
    args.push(".".into());
    let out = Command::new(docker()).args(&args).output().expect("docker runs");
    // 1: a file changed as it was read, which a hot save is
    assert!(out.status.success() || out.status.code() == Some(1), "tar: {}", String::from_utf8_lossy(&out.stderr));
    std::fs::write(to, &out.stdout).expect("the save written");
    out.stdout.len() as u64
}

/// A save restored into a fresh container of `tag`, as a wake from the
/// image and the save does, before its gate: the image's check (its exit
/// code and what it said), then every SQLite file under `/data` with its
/// `quick_check`, as the hermes user.
fn restore_and_check(tag: &str, save: &std::path::Path) -> (i32, String, Vec<serde_json::Value>) {
    let c = Container::idle(tag);
    assert!(c.exec(&["mkdir", "/data"]), "the restore's target");
    let tar = std::fs::File::open(save).expect("the save");
    let cp = Command::new(docker()).args(["cp", "-a", "-", &format!("{}:/data", c.id)]).stdin(tar).output().expect("docker runs");
    assert!(cp.status.success(), "the restore: {}", String::from_utf8_lossy(&cp.stderr));
    let (code, said) = c.exec_code(&["/usr/local/bin/computer-check"]);
    let report = c.exec_out(&["/command/s6-setuidgid", "hermes", "/opt/fragment/bin/hermes-boot", "sqlite-report", "/data"]);
    let files = report.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
    (code, said, files)
}

/// The Hermes image, running against the fake API and the scripted model,
/// its agent answering in its chat.
async fn hermes_running() -> (Fake, Model, String, Container) {
    build(&repo_dir(), "images/hermes/Dockerfile", &hermes_tag());
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("talk", &["juniper"]);
    let c = Container::run(&hermes_tag(), fake.addr.port(), model.addr.port(), &[]);
    fake.until(120_000, "Hermes' bridge to follow its chat", |w| w.live_sockets() >= 2).await;
    let first = fake.say(&chat, &person("paul"), json!({ "text": "hello" }));
    let t = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", first["seq"].as_u64().unwrap());
    fake.until(180_000, "Hermes' first reply", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t)).await;
    (fake, model, chat, c)
}

/// A turn whose tool writes a SQLite database of its own for about six
/// seconds (a commit every few ms), while Hermes writes its own.
const WRITING: &str = "run: python3 -c \"import sqlite3,time;c=sqlite3.connect('tool.db');c.execute('pragma journal_mode=wal');c.execute('create table if not exists t(x)');[(c.execute('insert into t values(randomblob(8000))'),c.commit(),time.sleep(0.01)) for _ in range(500)];print('wrote')\"";

/// Whether `WRITING`'s tool is running in `c` (a process whose command line
/// names its `randomblob`).
fn writing(c: &Container) -> bool {
    c.exec(&["pgrep", "-f", "randomblob"])
}

/// Whether a reported file is one of Hermes' own databases (its home's
/// `*.db`), or another SQLite file.
fn hermes_db(file: &serde_json::Value) -> bool {
    file["path"].as_str().is_some_and(|p| p.starts_with("/data/hermes/") && p.ends_with(".db"))
}

/// Goal (I8, F4): a save taken under the hold while turns write opens:
/// every one of Hermes' databases in it passes `quick_check`, whatever was
/// writing as it was taken. Method: saves taken many times during turns
/// whose tool writes a database of its own (and Hermes its own), half under
/// the hold (the databases copied by SQLite's online backup, their live
/// files left out) and half hot (no hold: `/data` as it is, the platform's
/// save before the hold), each restored into a fresh container, checked by
/// the image's own check as the platform runs it, then every SQLite file
/// found checked. Asserts the held saves are whole; reports the hot ones'
/// tear rate, and any other SQLite file's (a measure, not an assertion).
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn a_save_taken_while_it_writes_opens() {
    const ROUNDS: u32 = 12;
    let (fake, _model, chat, c) = hermes_running().await;
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("saves");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut saves = vec![];
    for round in 0..ROUNDS {
        let said = fake.say(&chat, &person("paul"), json!({ "text": WRITING }));
        let t = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", said["seq"].as_u64().unwrap());
        fake.until(120_000, "the writing turn's tool asked for", |w| w.bodies(&chat, "work", "turn.step").iter().any(|s| s["turn"] == t)).await;
        // Hermes asks its owner before a script runs from `-c` ("script
        // execution via -e/-c flag"): allowed for the session, once. So the
        // tool waits on its card (the first round) or runs (the rest), and
        // which is waited for, not a fixed time: a slow runner shows the card
        // well after the step.
        let asked = Instant::now();
        let mut answered = false;
        // bounded: a minute
        while !writing(&c) {
            if let (false, Some(card)) = (answered, fake.with(|w| w.bodies(&chat, "work", "turn.prompt").into_iter().find(|p| p["turn"] == t))) {
                fake.say(&chat, &person("paul"), json!({ "kind": "prompt_response", "prompt": card["prompt"], "option": "session" }));
                answered = true;
            }
            assert!(asked.elapsed() < Duration::from_secs(60), "round {round}: its tool never wrote (its card answered: {answered}); the container said:\n{}", c.logs());
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        // the tool writing for about six seconds: the save is taken in its midst
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        let held = round % 2 == 0;
        let t0 = Instant::now();
        let left_out = if held { hold(&c).unwrap_or_else(|| panic!("no answer to the hold; the container said:\n{}", c.logs())) } else { vec![] };
        let answered_ms = t0.elapsed().as_millis();
        let path = dir.join(format!("save-{round}.tar"));
        let bytes = save_data(&c, &left_out, &path);
        let saved_ms = t0.elapsed().as_millis();
        if held {
            unhold(&c);
        }
        eprintln!("save {round}: {} {bytes} bytes, answered in {answered_ms} ms, saved in {saved_ms} ms", if held { "held" } else { "hot" });
        saves.push((held, path));
        fake.until(120_000, "the writing turn's end", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t)).await;
    }
    // the seam: the tool wrote in its agent's work directory, not in Hermes' home
    assert!(c.exec(&["test", "-f", "/data/work/juniper--k3x9/tool.db"]), "the tool's database is the agent's work: {}", c.exec_out(&["sh", "-c", "find /data -name tool.db"]));
    let (mut held_torn, mut held_dbs, mut hot_torn, mut hot_dbs, mut other_torn, mut others) = (0, 0, 0, 0, 0, 0);
    for (held, path) in &saves {
        let t = Instant::now();
        let (code, said, files) = restore_and_check(&hermes_tag(), path);
        let torn: Vec<&serde_json::Value> = files.iter().filter(|f| f["ok"] != true).collect();
        eprintln!("{}: check {code} in {} ms ({}); {} SQLite files, torn: {torn:?}", path.display(), t.elapsed().as_millis(), said.trim(), files.len());
        let ours = files.iter().filter(|f| hermes_db(f)).count();
        let ours_torn = torn.iter().filter(|f| hermes_db(f)).count();
        assert!(ours > 0, "a save holds Hermes' databases: {files:?}");
        if *held {
            assert_eq!(code, 0, "the image's check of a held save passes: {said}");
            (held_dbs, held_torn) = (held_dbs + ours, held_torn + ours_torn);
        } else {
            (hot_dbs, hot_torn) = (hot_dbs + ours, hot_torn + ours_torn);
        }
        others += files.len() - ours;
        other_torn += torn.len() - ours_torn;
    }
    eprintln!("tear rate: Hermes' databases held {held_torn}/{held_dbs}, hot {hot_torn}/{hot_dbs}; other SQLite files {other_torn}/{others}");
    assert_eq!(held_torn, 0, "a held save's databases are whole");
}

/// The files under `/data`, each path with its size and time, but those
/// `leave` names (the hold's answer: anchored paths, `/` being `/data`).
fn files(c: &Container, leave: &[String]) -> BTreeMap<String, String> {
    let listed = c.exec_out(&["sh", "-c", "find /data -type f -printf '%s %T@ %p\\n'"]);
    let mut all = BTreeMap::new();
    for line in listed.lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(size), Some(time), Some(path)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if !leave.iter().any(|p| path.strip_prefix("/data") == Some(p.as_str())) {
            all.insert(path.to_string(), format!("{size} {time}"));
        }
    }
    all
}

/// The paths added, removed or changed from `before` to `after`.
fn changed(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    let paths: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    paths.into_iter().filter(|p| before.get(*p) != after.get(*p)).cloned().collect()
}

/// What Hermes' gateway writes on its own timers, which the hold cannot
/// quiet (its gateway is not paused: P2), and which the save keeps as it
/// reads it (docs/durable-computers.md, "What changes under the hold"): each
/// file replaced whole (a temp file beside it, fsynced, renamed over it, so
/// a save has the old or the new, never a mix), appended (a log), an empty
/// lock, or a copy only a broken config would read; and those temp files as
/// they are written. Read from Hermes v0.21.5, measured idle:
/// - its home's loop heartbeat (`state/gateway.heartbeat`, every 30 s), its
///   runtime status (`gateway_state.json`, every 60 s) and its channel
///   directory (`channel_directory.json`, every 5 minutes);
/// - in its home and each profile's, its cron ticker's stamps and lock
///   (`cron/ticker_*`, `cron/.tick.lock`, every 60 s, though its cron tool is
///   off), its logs, and its last-known-good copy of the profile's config
///   (`backups/config/config.yaml.good.<time>`): made once, in place, by the
///   first read of a `config.yaml` whose bytes its newest copy lacks (on a
///   first start, the gateway's catalog watcher's tick 30 s after it starts,
///   which reads the config with the catalog off), and read only when that
///   `config.yaml` will not parse, which the boot's never fail to do (each
///   written whole at every start).
fn kept_hot(path: &str) -> bool {
    let Some(rel) = path.strip_prefix("/data/hermes/") else { return false };
    let (home, rel) = match rel.strip_prefix("profiles/") {
        Some(p) => (false, p.split_once('/').map_or("", |(_, r)| r)),
        None => (true, rel),
    };
    let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
    let temp = name.starts_with('.') && name.ends_with(".tmp");
    let home_only = home && (matches!(rel, "state/gateway.heartbeat" | "gateway_state.json" | "channel_directory.json") || (temp && matches!(dir, "" | "state")));
    let each_home = matches!(rel, "cron/ticker_heartbeat" | "cron/ticker_last_success" | "cron/ticker_last_error" | "cron/.tick.lock")
        || (temp && dir == "cron")
        || (dir == "logs" && name.contains(".log"))
        || (dir == "backups/config" && name.starts_with("config.yaml.good."));
    home_only || each_home
}

/// What changed under `/data` while held that `kept_hot` does not explain.
fn unexplained(changed: &[String]) -> Vec<&String> {
    changed.iter().filter(|p| !kept_hot(p)).collect()
}

/// Each of `paths` Hermes replaces whole, a state or a stamp, read now:
/// whole, it parses as JSON; a stamp is its epoch, a number, and the
/// cron ticker's heartbeat its writer's pid after it (`<epoch> <pid>`, since
/// Hermes v0.21.6).
fn whole(c: &Container, paths: &[String]) {
    let replaced = |p: &&String| p.ends_with(".json") || ["/gateway.heartbeat", "/ticker_heartbeat", "/ticker_last_success", "/ticker_last_error"].iter().any(|s| p.ends_with(s));
    for p in paths.iter().filter(replaced) {
        let (code, text) = c.exec_code(&["cat", p]);
        if code != 0 {
            continue;
        }
        let stamp = p.ends_with("/ticker_heartbeat") && text.split_once(' ').is_some_and(|(epoch, pid)| epoch.parse::<f64>().is_ok() && pid.trim().parse::<u32>().is_ok());
        assert!(stamp || serde_json::from_str::<serde_json::Value>(&text).is_ok(), "{p} is whole: {text:?}");
    }
}

/// The caches Hermes' model-catalog refresh writes into the gateway's home.
const CATALOG_CACHES: [&str; 4] = ["model_catalog.json", "openrouter_curated_catalog.json", "nous_recommended_cache.json", "reasoning_caps.json"];

/// A hold kept this long, and until Hermes' heartbeat (every 30 s) and its
/// cron ticker (every 60 s, as its status is) have both run inside it, sees
/// each of Hermes' own idle timers but its 5-minute one, and the image's own
/// repo sync (every 60 s) were the hold not to quiet it: every run sees the
/// same. Past `HELD_MAX_MS` without them, the test fails.
const HELD_MS: u64 = 65_000;
const HELD_MAX_MS: u64 = 150_000;
/// Two of Hermes' timers, each seen rewritten inside the hold.
const TIMERS: [&str; 2] = ["/data/hermes/state/gateway.heartbeat", "/data/hermes/cron/ticker_heartbeat"];

/// Goal (P2): once the guest says `held`, no file its save keeps changes
/// but what Hermes' gateway rewrites on its own timers, each whole
/// (`kept_hot`): the bridge claims nothing, the sync, the skills and the
/// agents' reads start no round, and the copy is done before the answer.
/// Hermes' gateway is not paused (P2: it cannot be asked), so the hold comes
/// between turns; what Hermes writes on its own while idle in a database
/// (its kanban dispatcher opens its board's on a timer, making its `-wal`
/// and `-shm`) is in the files its answer names, which the save leaves out
/// (their copies kept). Its model-catalog refresh, which on 2026-10-07
/// rewrote four caches inside this test's hold (then 5 s, landing in it
/// only in a slow run), is off, and Node's compile cache is out of `/data`.
/// Method: an idle Hermes held, a listing of the files under `/data` (each
/// path, size and time) but those the answer names, then another at least
/// `HELD_MS` later, once Hermes' heartbeat and cron ticker have both run:
/// past every one of those timers, so every run sees them inside the hold,
/// whenever it began. Then a message said while it is held is not
/// claimed until the hold goes (its bridge records it, unclaimed: its state
/// file is rewritten, whole).
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn held_nothing_under_data_changes() {
    let (fake, _model, chat, c) = hermes_running().await;
    // between turns: once Hermes' start-up writes are done (its lazy
    // packages; its kanban dispatcher makes its board's database some
    // seconds after the gateway starts), two windows of three seconds with
    // no file changed but a database's journal and what Hermes' timers keep
    let journals = |p: &String| [".db-wal", ".db-shm", ".db-journal"].iter().any(|s| p.ends_with(s));
    let settled = |all: BTreeMap<String, String>| all.into_iter().filter(|(p, _)| !journals(p) && !kept_hot(p)).collect::<BTreeMap<_, _>>();
    let quiet = Instant::now();
    // bounded: two minutes
    while !c.exec(&["test", "-f", "/data/hermes/kanban.db"]) {
        assert!(quiet.elapsed() < Duration::from_secs(120), "Hermes never made its kanban board's database");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let mut last = settled(files(&c, &[]));
    let mut still = 0;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let now = settled(files(&c, &[]));
        still = if now == last { still + 1 } else { 0 };
        if still >= 2 {
            break;
        }
        last = now;
    }
    eprintln!("hermes: /data quiet {} ms after its first reply", quiet.elapsed().as_millis());
    let left_out = hold(&c).unwrap_or_else(|| panic!("no answer to the hold; the container said:\n{}", c.logs()));
    assert!(left_out.iter().any(|l| l == "/hermes/profiles/juniper--k3x9/state.db") && left_out.iter().all(|l| l.starts_with("/hermes/") && l.contains(".db")), "it names exactly the databases it copied: {left_out:?}");
    assert!(c.exec(&["test", "-f", "/data/held-copies/manifest.json"]), "its copies are made before it answers");
    let before = files(&c, &left_out);
    let held = Instant::now();
    // bounded by HELD_MAX_MS
    let seen = loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let seen = changed(&before, &files(&c, &left_out));
        let fired = TIMERS.iter().all(|t| seen.iter().any(|p| p == t));
        if fired && held.elapsed() >= Duration::from_millis(HELD_MS) {
            break seen;
        }
        assert!(held.elapsed() < Duration::from_millis(HELD_MAX_MS), "Hermes' timers ran inside the hold ({TIMERS:?}): {seen:?}");
    };
    eprintln!("hermes: changed in {} ms held: {seen:?}", held.elapsed().as_millis());
    assert!(unexplained(&seen).is_empty(), "a file the save keeps changed while held, not one Hermes' timers rewrite whole: {:?} (all changed: {seen:?})", unexplained(&seen));
    whole(&c, &seen);
    // its catalogs, refreshed 30 s after the gateway started had they been
    // on, are not; Node's compile cache (its `npx --version` probes ran with
    // the first turn) is under /tmp
    for cache in CATALOG_CACHES {
        assert!(!c.exec(&["test", "-e", &format!("/data/hermes/cache/{cache}")]), "no catalog refresh wrote {cache}");
    }
    assert_eq!(c.exec_out(&["find", "/data", "-name", "node-compile-cache"]).trim(), "", "Node's compile cache is not under /data");
    assert!(!c.exec_out(&["find", "/tmp/node-compile-cache", "-type", "f"]).trim().is_empty(), "it is under /tmp, where npm's start put it");
    // a message as it is held: recorded, never claimed while held
    let said = fake.say(&chat, &person("paul"), json!({ "text": "said while held" }));
    let t = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", said["seq"].as_u64().unwrap());
    tokio::time::sleep(Duration::from_secs(5)).await;
    fake.with(|w| assert!(w.bodies(&chat, "work", "turn.start").iter().all(|s| s["turn"] != t), "held, the message is not claimed"));
    unhold(&c);
    fake.until(120_000, "the message answered once the hold goes", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t)).await;
    // the boot sees the hold go on its own poll (every 100 ms), apart from
    // the bridge's: waited for, bounded, not a fixed time
    let gone = Instant::now();
    while c.exec(&["test", "-e", "/data/held-copies"]) {
        assert!(gone.elapsed() < Duration::from_secs(10), "the copies go with the hold");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// What the agent's desktop looks like from inside the container, as the
/// hermes user: the pointer's position and the windows' titles, read from
/// the X server the profile publishes (`<profile>/bot-desktop/env`).
fn desk(c: &Container, profile: &str) -> serde_json::Value {
    const SCRIPT: &str = r#"import ctypes, json, os, re, subprocess, sys
env = dict(l.split("=", 1) for l in open(sys.argv[1]).read().splitlines() if "=" in l)
os.environ.update(env)
x = ctypes.cdll.LoadLibrary("libX11.so.6")
x.XOpenDisplay.restype, x.XOpenDisplay.argtypes = ctypes.c_void_p, [ctypes.c_char_p]
x.XDefaultRootWindow.restype, x.XDefaultRootWindow.argtypes = ctypes.c_ulong, [ctypes.c_void_p]
x.XQueryPointer.argtypes = [ctypes.c_void_p, ctypes.c_ulong] + [ctypes.c_void_p] * 7
d = x.XOpenDisplay(env["DISPLAY"].encode())
root = x.XDefaultRootWindow(d)
v = [ctypes.c_ulong(), ctypes.c_ulong(), ctypes.c_int(), ctypes.c_int(), ctypes.c_int(), ctypes.c_int(), ctypes.c_uint()]
x.XQueryPointer(d, root, *[ctypes.byref(a) for a in v])
ids = re.findall(r"0x[0-9a-f]+", subprocess.run(["xprop", "-root", "_NET_CLIENT_LIST"], capture_output=True, text=True).stdout)
names = [subprocess.run(["xprop", "-id", w, "_NET_WM_NAME"], capture_output=True, text=True).stdout.split("=", 1)[-1].strip().strip('"') for w in ids]
print(json.dumps({"pointer": [v[2].value, v[3].value], "windows": names}))
"#;
    let env = format!("/data/hermes/profiles/{profile}/bot-desktop/env");
    let out = c.exec_out(&["/command/s6-setuidgid", "hermes", "/opt/hermes/.venv/bin/python", "-c", SCRIPT, &env]);
    serde_json::from_str(out.trim().lines().last().unwrap_or("null")).unwrap_or(serde_json::Value::Null)
}

/// Waits until the pointer on `profile`'s desktop is at `at` (looked at every
/// 200 ms, for at most 15 s), as a viewer's move puts it there: through the
/// screen's socket, the bridge and Xvnc, which a loaded runner slows. How
/// long it took.
async fn pointer_at(c: &Container, profile: &str, at: [i64; 2], what: &str) -> Duration {
    let t = Instant::now();
    // bounded: 15 s
    loop {
        let seen = desk(c, profile)["pointer"].clone();
        if seen == json!(at) {
            return t.elapsed();
        }
        assert!(t.elapsed() < Duration::from_secs(15), "{what}: the pointer is at {seen}, not {at:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// How long a move that must not land is given to land before its absence
/// counts: three times what a move that did land took, and at least a
/// second, so a slower runner gives it longer.
fn settle(took: Duration) -> Duration {
    (took * 3).max(Duration::from_secs(1))
}

/// Where the window titled `title` is on the agent's desktop, `[x, y, w,
/// h]`, as its X server has it (`xwininfo`, as the hermes user).
fn window(c: &Container, profile: &str, title: &str) -> Option<[usize; 4]> {
    let env = format!("/data/hermes/profiles/{profile}/bot-desktop/env");
    let script = format!("set -a; . {env}; xwininfo -root -tree");
    let tree = c.exec_out(&["/command/s6-setuidgid", "hermes", "bash", "-c", &script]);
    // `0x200003 "Example Domain - …": ("chromium-browser" "Chromium-browser")  1004x748+10+10  +10+10`
    let line = tree.lines().find(|l| l.contains(&format!("\"{title}")))?;
    let mut fields = line.split_whitespace().rev();
    let at = fields.next()?.trim_start_matches('+');
    let size = fields.next()?;
    let (x, y) = at.split_once('+')?;
    let (w, rest) = size.split_once('x')?;
    let h = rest.split('+').next()?;
    Some([x.parse().ok()?, y.parse().ok()?, w.parse().ok()?, h.parse().ok()?])
}

/// The share of `rect`'s pixels in `frame` (`width` wide) that are
/// example.com's background (`#eee`, or `#222` in a dark scheme): the page
/// drawn where its window is, as a viewer sees it.
fn page_shown(frame: &[u32], width: usize, rect: [usize; 4]) -> f64 {
    let [x, y, w, h] = rect;
    let (mut page, mut all) = (0usize, 0usize);
    for row in y..y + h {
        for col in x..(x + w).min(width) {
            if let Some(&p) = frame.get(row * width + col) {
                all += 1;
                page += usize::from(p == 0x00ee_eeee || p == 0x0022_2222);
            }
        }
    }
    page as f64 / all.max(1) as f64
}

/// The command line of the browser's Chromium (its first process).
fn chromium(c: &Container) -> String {
    c.exec_out(&["sh", "-c", "for p in $(pgrep -f -- '/chrome(-headless-shell)? ' | head -1); do tr '\\0' ' ' < /proc/$p/cmdline; done"])
}

/// Waits until `viewer`'s frame shows the browser's page where its window
/// is (within 30 s): where it is, `[x, y, w, h]`.
async fn browser_shown(c: &Container, viewer: &mut support::rfb::Viewer, what: &str) -> [usize; 4] {
    let t = Instant::now();
    loop {
        let at = window(c, "juniper--k3x9", "Example Domain");
        let frame = viewer.frame().await.unwrap_or_else(|e| panic!("{what}: the viewer's frame: {e}"));
        let page = at.map(|r| page_shown(&frame, viewer.width as usize, r));
        if let (Some(rect), Some(p)) = (at, page) {
            if p > 0.4 {
                return rect;
            }
        }
        assert!(
            t.elapsed() < Duration::from_secs(30),
            "{what}: the browser never showed in the viewer's frame: its window at {at:?}, the page {page:?} of it; the desktop {}; chromium {}\n{}",
            desk(c, "juniper--k3x9"),
            chromium(c),
            c.logs().lines().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// An HTTP server that answers every request `{}` and keeps each one's
/// request line and headers: what a vendor's API would be sent.
struct Recorder {
    port: u16,
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Recorder {
    async fn start() -> Recorder {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.expect("the recorder listens");
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = seen.clone();
        tokio::spawn(async move {
            // bounded by the test: it ends with the runtime
            while let Ok((mut s, _)) = listener.accept().await {
                let kept = kept.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut b = [0u8; 4096];
                    // bounded: a request's head is at most 64 KiB
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65_536 {
                        match s.read(&mut b).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&b[..n]),
                        }
                    }
                    kept.lock().unwrap().push(String::from_utf8_lossy(&head).into_owned());
                    let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}").await;
                });
            }
        });
        Recorder { port, seen }
    }
}

/// The container's memory now, in MiB: its cgroup's anonymous memory (what
/// its processes hold, without the page cache).
fn anon_mib(c: &Container) -> u64 {
    let stat = c.exec_out(&["cat", "/sys/fs/cgroup/memory.stat"]);
    stat.lines().find_map(|l| l.strip_prefix("anon ")).and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(0) / 1_048_576
}

/// Goal: a person can watch and take over the agent's desktop, and the
/// agent operates it (docs/computers.md, Ports; Paul, 2026-10-05): before
/// any agent's call the screen's page, its control socket and its RFB
/// stream answer, the first agent's desktop started for its first viewer;
/// a viewer who takes over moves the pointer, and one who has not cannot;
/// the agent's `computer_use` is among its tools and captures the screen;
/// its browser opens on that desktop, in the frame of the viewer that has
/// watched since before it started; and the desktop restarted under its
/// viewers, a viewer that opens the screen again (the page does, on its
/// own) sees the browser on the new one. All run as Containers runs the
/// image (`Runtime::Hosted`: no `/dev/shm`, no Docker marker; p5,
/// 2026-10-05).
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn the_hermes_desktop() {
    use fragment_bridge::net::Base;
    use support::rfb::{colours, Control, Viewer};
    let tag = hermes_tag();
    build(&repo_dir(), "images/hermes/Dockerfile", &tag);
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    // its owner connected Google: the agent's placeholder, as the platform makes one
    let google = "fcx_google_0123456789abcdef0123456789abcdef";
    fake.with(|w| {
        w.computer["agents"][0]["credentials"] = json!([{ "provider": "google", "kind": "connection", "env": ["GOOGLE_OAUTH_ACCESS_TOKEN"], "placeholder": google, "hosts": ["www.googleapis.com"] }]);
        w.computer["credentialEnv"] = json!(["GOOGLE_OAUTH_ACCESS_TOKEN"]);
    });
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("desk", &["juniper"]);
    let c = Container::run_on(Runtime::Hosted, &tag, fake.addr.port(), model.addr.port(), &[]);
    assert!(!c.exec(&["test", "-e", "/dev/shm"]) && !c.exec(&["test", "-e", "/.dockerenv"]), "run as Containers runs it: no /dev/shm, no Docker marker");
    fake.until(180_000, "Hermes' bridge to follow its chat", |w| w.live_sockets() >= 2).await;
    let base = Base::parse(&format!("http://127.0.0.1:{}", c.port(6080))).unwrap();
    let before = anon_mib(&c);
    eprintln!("desktop: the container holds {before} MiB before any screen");

    // before any agent's call: the page, the control socket, the RFB stream
    assert!(c.exec_out(&["curl", "-sf", "http://127.0.0.1:6080/"]).contains("Take over"));
    let mut watching = Control::open(&base, "watcher", "juniper--k3x9").await.unwrap_or_else(|e| panic!("the control socket: {e}"));
    assert_eq!(watching.next().await.unwrap(), json!({ "type": "control", "agent": "juniper--k3x9", "name": "juniper", "holder": null }), "a viewer hears whose screen it is, and who holds control");
    let t = Instant::now();
    let opened = Viewer::open(&base, "watcher", "juniper--k3x9", Duration::from_secs(60)).await;
    let mut watcher = opened.unwrap_or_else(|e| panic!("the screen's RFB stream: {e}\n{}\nlauncher: {}", c.logs(), c.exec_out(&["cat", "/data/hermes/profiles/juniper--k3x9/bot-desktop/launcher.log"])));
    eprintln!("desktop: first viewer to the RFB greeting {} ms; {}x{} {:?}", t.elapsed().as_millis(), watcher.width, watcher.height, watcher.name);
    // the desktop drawn: its wallpaper and panel, not one colour
    let t = Instant::now();
    let mut frame = watcher.frame().await.unwrap();
    while colours(&frame) <= 16 && t.elapsed() < Duration::from_secs(30) {
        tokio::time::sleep(Duration::from_millis(500)).await;
        frame = watcher.frame().await.unwrap();
    }
    let after = anon_mib(&c);
    eprintln!("desktop: a frame of {} colours {} ms on; the container holds {after} MiB with the desktop (+{})", colours(&frame), t.elapsed().as_millis(), after.saturating_sub(before));
    assert!(colours(&frame) > 16, "the desktop shows something: {} colours\n{}", colours(&frame), c.exec_out(&["cat", "/data/hermes/profiles/juniper--k3x9/bot-desktop/launcher.log"]));

    // Take over: the holder's pointer moves the screen's; a watcher's does not
    let mut driving = Control::open(&base, "driver", "juniper--k3x9").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], json!(null));
    let mut driver = Viewer::open(&base, "driver", "juniper--k3x9", Duration::from_secs(30)).await.unwrap();
    assert!(driver.clipboard_caps, "Xvnc offers its extended clipboard, and the viewer answers it as noVNC does (a ClientCutText of negative length)");
    driving.say("take").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], "driver");
    assert_eq!(watching.next().await.unwrap()["holder"], "driver", "every viewer hears who took over");
    driver.pointer(101, 57, 0).await.unwrap();
    let took = pointer_at(&c, "juniper--k3x9", [101, 57], "the holder moves the pointer").await;
    watcher.pointer(301, 257, 0).await.unwrap();
    tokio::time::sleep(settle(took)).await;
    assert_eq!(desk(&c, "juniper--k3x9")["pointer"], json!([101, 57]), "a watcher does not (given {:?}, the holder's move having taken {took:?})", settle(took));
    driving.say("give").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], json!(null));

    // the agent's computer_use: among its tools, and it captures the screen
    let reply = |w: &support::fake::World, turn: &str| w.bodies(&chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn).and_then(|r| r["text"].as_str().map(str::to_string));
    let asked = fake.say(&chat, &person("paul"), json!({ "text": "look at your screen" }));
    let turn = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", asked["seq"].as_u64().unwrap());
    fake.until(240_000, "the capture's reply", |w| reply(w, &turn).is_some()).await;
    let offered = model.calls.lock().unwrap().iter().find(|c| c.path.ends_with("/chat/completions") && c.body["messages"].to_string().contains("look at your screen")).map(|c| c.body["tools"].clone()).unwrap_or_default();
    let tools: Vec<&str> = offered.as_array().into_iter().flatten().filter_map(|t| t["function"]["name"].as_str()).collect();
    let search = offered.as_array().into_iter().flatten().find(|t| t["function"]["name"] == "tool_search").and_then(|t| t["function"]["description"].as_str()).unwrap_or("");
    let listed = search.find("computer_use").map(|i| search[i..].chars().take(160).collect::<String>());
    eprintln!("desktop: the agent's tools: {tools:?}; computer_use in tool_search's listing: {listed:?}");
    let captured = fake.with(|w| reply(w, &turn)).unwrap_or_default();
    eprintln!("desktop: the capture's reply: {}", captured.chars().take(600).collect::<String>());
    assert!(tools.contains(&"computer_use") || listed.is_some(), "computer_use is the agent's, directly or through tool_search: {tools:?}");
    // the screenshot is described by the route's vision model (the profile's
    // `auxiliary.vision`), as the agent, its image in the call
    let looked: Vec<(String, Option<String>, bool)> = model
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| c.body["messages"].to_string().contains("\"image_url\""))
        .map(|c| (c.model.clone(), c.agent.clone(), c.path.ends_with("/v1/chat/completions") && c.body["messages"].to_string().contains("data:image/")))
        .collect();
    eprintln!("desktop: the calls shown an image (model, agent, a data: image on /v1/chat/completions): {looked:?}");
    assert!(
        !looked.is_empty() && looked.iter().all(|(m, a, ok)| m == "vision" && a.as_deref() == Some("juniper--k3x9") && *ok),
        "the capture's screenshot goes to the route's vision model, as the agent: {looked:?}"
    );
    assert!(tools.contains(&"browser_navigate") && !tools.contains(&"browser_exec"), "Hermes' built-in browser tools: {tools:?}");

    // its browser, on its desktop, seen by the viewer that has watched
    // since before the desktop started
    let browse = |fake: &Fake| {
        let asked = fake.say(&chat, &person("paul"), json!({ "text": "browse: https://example.com" }));
        fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", asked["seq"].as_u64().unwrap())
    };
    let turn = browse(&fake);
    fake.until(240_000, "the browser's reply", |w| reply(w, &turn).is_some()).await;
    eprintln!("desktop: the browser's reply: {:?}", fake.with(|w| reply(w, &turn)));
    let rect = browser_shown(&c, &mut watcher, "the first browse").await;
    let started = chromium(&c);
    eprintln!("desktop: the browser's window at {rect:?} in the watcher's frame; chromium {started}; the container holds {} MiB", anon_mib(&c));
    assert!(started.contains(" --no-sandbox --disable-dev-shm-usage "), "the image's Chromium, with the flags a container needs: {started}");
    // and the desktop's Browser icon, a person's after Take over, starts the same
    let panel = "/data/hermes/profiles/juniper--k3x9/bot-desktop/xdg/xfce4/panel";
    let icon = c.exec_out(&["sh", "-c", &format!("cat {panel}/launcher-$(cat {panel}/.hermes-browser-launcher)/hermes.desktop")]);
    assert!(icon.lines().any(|l| l.starts_with("Exec=\"/opt/fragment/bin/chromium\" ")), "the Browser icon starts the image's Chromium: {icon}");

    // The desktop restarts under its viewers (stopped as `hermes
    // computer-use screen stop` stops it): each viewer's stream ends with
    // its display, so a viewer knows to open it again (the screen's page
    // does, on its own); the one it opens starts the display at once, and
    // shows the browser on it at the agent's next call.
    let stopped = c.exec_code(&["env", "HOME=/data/hermes", "HERMES_HOME=/data/hermes", "/command/s6-setuidgid", "hermes", "/opt/hermes/.venv/bin/hermes", "-p", "juniper--k3x9", "computer-use", "screen", "stop"]);
    eprintln!("desktop: stopped: {stopped:?}");
    let ended = tokio::time::timeout(Duration::from_secs(15), watcher.frame()).await.map(|r| r.map(|f| f.len()));
    assert!(matches!(ended, Ok(Err(_))), "the watcher's stream ends with its display: {ended:?}");
    let t = Instant::now();
    let mut again = Viewer::open(&base, "watcher", "juniper--k3x9", Duration::from_secs(30)).await.unwrap_or_else(|e| panic!("the watcher, back: {e}\n{}", c.logs()));
    eprintln!("desktop: the watcher back on a desktop started for it in {} ms", t.elapsed().as_millis());
    let turn = browse(&fake);
    fake.until(240_000, "the browser's reply after the restart", |w| reply(w, &turn).is_some()).await;
    eprintln!("desktop: the browser's reply after the restart: {:?}", fake.with(|w| reply(w, &turn)));
    let rect = browser_shown(&c, &mut again, "after the desktop's restart").await;
    eprintln!("desktop: the browser's window at {rect:?} after the restart; chromium {}", chromium(&c));

    // gws, Google's Workspace CLI: in the image, and run in the agent's
    // terminal it sends the agent's Google placeholder as its bearer token
    // (the computer's egress swaps it on Google's hosts). Its Drive API is
    // pointed at a recorder (a discovery document in its cache) to see it.
    let version = c.exec_out(&["gws", "--version"]);
    assert!(version.contains("0.22.5"), "gws is pinned: {version}");
    let api = Recorder::start().await;
    let doc = json!({
        "name": "drive", "version": "v3", "rootUrl": format!("http://api.fragment.internal:{}/", api.port), "servicePath": "drive/v3/",
        "resources": { "files": { "methods": { "list": { "id": "drive.files.list", "httpMethod": "GET", "path": "files", "scopes": ["https://www.googleapis.com/auth/drive"] } } } },
    });
    let planted = format!("mkdir -p /tmp/gws/cache && printf '%s' '{doc}' > /tmp/gws/cache/drive_v3.json");
    assert!(c.exec(&["/command/s6-setuidgid", "hermes", "sh", "-c", &planted]));
    let asked = fake.say(&chat, &person("paul"), json!({ "text": "run: GOOGLE_WORKSPACE_CLI_CONFIG_DIR=/tmp/gws gws drive files list" }));
    let turn = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", asked["seq"].as_u64().unwrap());
    fake.until(240_000, "gws's reply", |w| reply(w, &turn).is_some()).await;
    let seen = api.seen.lock().unwrap().clone();
    eprintln!("desktop: gws in the agent's terminal said {:?}; its API saw {seen:?}", fake.with(|w| reply(w, &turn)));
    assert!(
        seen.iter().any(|r| r.starts_with("GET /drive/v3/files") && r.to_ascii_lowercase().contains(&format!("authorization: bearer {google}"))),
        "gws sends the agent's placeholder as its bearer token: {seen:?}; it said {:?}",
        fake.with(|w| reply(w, &turn))
    );
}

/// Hermes' own reading of `profile`'s Bot Desktop lease, as its
/// computer_use reads it before every action: `{holder, viewer_id, epoch,
/// refused}`, `refused` being what `assert_agent_may_act` raised (its
/// `human_has_control`), or null.
fn hermes_lease(c: &Container, profile: &str) -> serde_json::Value {
    const SCRIPT: &str = r#"import json, os, sys
os.chdir('/opt/hermes'); sys.path.insert(0, '/opt/hermes')
from tools.bot_desktop import lease
home = sys.argv[1]
l = lease.get(home)
try:
    lease.assert_agent_may_act(home); refused = None
except lease.HumanHasControl as e:
    refused = str(e)
print(json.dumps({"holder": l.holder, "viewer_id": l.viewer_id, "epoch": l.epoch, "refused": refused}))
"#;
    let home = format!("/data/hermes/profiles/{profile}");
    let out = c.exec_out(&["env", "HERMES_HOME=/data/hermes", "/command/s6-setuidgid", "hermes", "/opt/hermes/.venv/bin/python", "-c", SCRIPT, &home]);
    serde_json::from_str(out.trim().lines().last().unwrap_or("null")).unwrap_or(serde_json::Value::Null)
}

/// Whether `profile`'s desktop is up: its launcher's published environment.
fn desktop_up(c: &Container, profile: &str) -> bool {
    c.exec(&["test", "-e", &format!("/data/hermes/profiles/{profile}/bot-desktop/env")])
}

/// Goal: two agents on one computer, two desktops (Hermes gives each
/// profile its own), each its own screen: a socket naming juniper is
/// juniper's desktop and one naming fred is fred's, each started for its
/// own first viewer; an agent not on the computer, or no name, refused.
/// Take over of juniper's screen is juniper's Bot Desktop lease, as Hermes
/// reads it (`human`, the viewer, `human_has_control`), and juniper's
/// computer_use refuses while it holds, fred's working on its own desktop;
/// Give back is juniper's again, and its computer_use works. A desktop no
/// one watches or uses stops after the idle bound (here 30 s), one watched
/// does not, and the next viewer starts it again, the browser's profile
/// (in the agent's work) kept. As Containers runs the image.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn two_agents_two_desktops() {
    use fragment_bridge::net::Base;
    use support::rfb::{Control, Viewer};
    let tag = hermes_tag();
    build(&repo_dir(), "images/hermes/Dockerfile", &tag);
    let fake = Fake::start("0.0.0.0:0", &["juniper", "fred"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let (jchat, fchat) = (fake.chat("juniper-desk", &["juniper"]), fake.chat("fred-desk", &["fred"]));
    let c = Container::run_on(Runtime::Hosted, &tag, fake.addr.port(), model.addr.port(), &[("HERMES_BOOT_SCREEN_IDLE_MS", "30000")]);
    fake.until(180_000, "Hermes' bridge to follow both agents' chats", |w| w.live_sockets() >= 4).await;
    let base = Base::parse(&format!("http://127.0.0.1:{}", c.port(6080))).unwrap();
    let screens: serde_json::Value = serde_json::from_str(&c.exec_out(&["cat", "/var/lib/fragment-run/screens.json"])).unwrap_or_default();
    eprintln!("two desktops: the screens file {screens}");
    assert!(!c.exec(&["test", "-e", "/var/lib/fragment-run/screen.sock"]), "no one screen for every agent");

    // each agent's own screen, refusals included
    let mut jc = Control::open(&base, "watcher", "juniper--k3x9").await.unwrap();
    assert_eq!(jc.next().await.unwrap(), json!({ "type": "control", "agent": "juniper--k3x9", "name": "juniper", "holder": null }));
    let mut fc = Control::open(&base, "watcher", "fred--k3x9").await.unwrap();
    assert_eq!(fc.next().await.unwrap(), json!({ "type": "control", "agent": "fred--k3x9", "name": "fred", "holder": null }));
    let nobody = fragment_bridge::net::connect_ws(&base, "/websockify?viewer=w&agent=nobody--k3x9", &[]).await.err().unwrap_or_default();
    assert!(nobody.contains("404"), "an agent not on this computer: {nobody}");
    let unnamed = fragment_bridge::net::connect_ws(&base, "/websockify?viewer=w&agent=../fred--k3x9", &[]).await.err().unwrap_or_default();
    assert!(unnamed.contains("400"), "no agent's name: {unnamed}");
    let t = Instant::now();
    let jv = Viewer::open(&base, "watcher", "juniper--k3x9", Duration::from_secs(60)).await.unwrap_or_else(|e| panic!("juniper's screen: {e}\n{}", c.logs()));
    let mut fv = Viewer::open(&base, "watcher", "fred--k3x9", Duration::from_secs(60)).await.unwrap_or_else(|e| panic!("fred's screen: {e}\n{}", c.logs()));
    eprintln!("two desktops: both up for their viewers in {} ms; the container holds {} MiB", t.elapsed().as_millis(), anon_mib(&c));
    assert_eq!((jv.name.as_str(), fv.name.as_str()), ("hermes:juniper--k3x9", "hermes:fred--k3x9"), "each socket shows its own agent's desktop");
    let xvnc = c.exec_out(&["sh", "-c", "pgrep -x Xvnc | wc -l"]);
    assert_eq!(xvnc.trim(), "2", "two desktops, two X servers");

    // Take over of juniper's screen is juniper's lease
    let mut driving = Control::open(&base, "driver", "juniper--k3x9").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], json!(null));
    let mut driver = Viewer::open(&base, "driver", "juniper--k3x9", Duration::from_secs(30)).await.unwrap();
    driving.say("take").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], "driver");
    assert_eq!(jc.next().await.unwrap()["holder"], "driver", "juniper's watcher hears it");
    let (jl, fl) = (hermes_lease(&c, "juniper--k3x9"), hermes_lease(&c, "fred--k3x9"));
    eprintln!("two desktops: juniper's lease as Hermes reads it {jl}; fred's {fl}");
    assert!(jl["holder"] == "human" && jl["viewer_id"] == "driver" && jl["refused"].is_string(), "juniper's lease is the person's, and Hermes refuses juniper's screen actions: {jl}");
    assert!(fl["holder"] == "agent" && fl["refused"].is_null(), "fred's is fred's: {fl}");
    driver.pointer(101, 57, 0).await.unwrap();
    pointer_at(&c, "juniper--k3x9", [101, 57], "the person drives juniper's desktop").await;
    assert_ne!(desk(&c, "fred--k3x9")["pointer"], json!([101, 57]), "and not fred's");

    // each agent's computer_use, as the lease has it
    let look = |chat: &str, agent: &str| {
        let said = fake.say(chat, &person("paul"), json!({ "text": "look at your screen" }));
        fragment_bridge::records::turn_id(agent, chat, "chat", said["seq"].as_u64().unwrap())
    };
    let reply = |w: &support::fake::World, chat: &str, turn: &str| w.bodies(chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn).and_then(|r| r["text"].as_str().map(str::to_string));
    let jturn = look(&jchat, "juniper--k3x9");
    within(&fake, &jchat, &c, 240_000, "juniper's look while a person holds its screen", |w| reply(w, &jchat, &jturn).is_some()).await;
    let jsaid = fake.with(|w| reply(w, &jchat, &jturn)).unwrap_or_default();
    eprintln!("two desktops: juniper, held: {}", jsaid.chars().take(400).collect::<String>());
    assert!(jsaid.contains("human_has_control"), "juniper's computer_use refuses while a person holds its screen: {jsaid}");
    let fturn = look(&fchat, "fred--k3x9");
    within(&fake, &fchat, &c, 240_000, "fred's look", |w| reply(w, &fchat, &fturn).is_some()).await;
    let fsaid = fake.with(|w| reply(w, &fchat, &fturn)).unwrap_or_default();
    eprintln!("two desktops: fred, meanwhile: {}", fsaid.chars().take(400).collect::<String>());
    assert!(!fsaid.contains("human_has_control") && !fsaid.contains("no computer_use"), "fred's computer_use works on its own desktop: {fsaid}");
    let fred_looked = model.calls.lock().unwrap().iter().any(|m| m.agent.as_deref() == Some("fred--k3x9") && m.body["messages"].to_string().contains("\"image_url\""));
    assert!(fred_looked, "fred's capture went to the vision model, as fred");

    // Give back: juniper's lease again, and its computer_use works
    driving.say("give").await.unwrap();
    assert_eq!(driving.next().await.unwrap()["holder"], json!(null));
    let jl = hermes_lease(&c, "juniper--k3x9");
    assert!(jl["holder"] == "agent" && jl["refused"].is_null() && jl["epoch"].as_u64() >= Some(2), "given back: {jl}");
    let jturn = look(&jchat, "juniper--k3x9");
    within(&fake, &jchat, &c, 240_000, "juniper's look, given back", |w| reply(w, &jchat, &jturn).is_some()).await;
    let jsaid = fake.with(|w| reply(w, &jchat, &jturn)).unwrap_or_default();
    assert!(!jsaid.contains("human_has_control"), "given back, juniper's computer_use works: {jsaid}");

    // idle: juniper's desktop, unwatched and unused, stops; fred's, watched, does not
    let marker = "/data/work/juniper--k3x9/browser-profile/fragment-marker";
    assert!(c.exec(&["/command/s6-setuidgid", "hermes", "sh", "-c", &format!("echo kept > {marker}")]));
    drop((jc, driving, jv, driver));
    let t = Instant::now();
    // bounded: the 30 s bound, a look every 7.5 s, and Hermes' own stop
    while desktop_up(&c, "juniper--k3x9") && t.elapsed() < Duration::from_secs(120) {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = fv.frame().await;
    }
    eprintln!("two desktops: juniper's stopped {} ms after its last viewer left", t.elapsed().as_millis());
    assert!(!desktop_up(&c, "juniper--k3x9"), "an unwatched, unused desktop stops: {}", c.logs().lines().filter(|l| l.contains("screen.")).collect::<Vec<_>>().join("\n"));
    assert!(desktop_up(&c, "fred--k3x9"), "one watched does not");
    let stopped = c.logs().lines().filter(|l| l.contains("\"screen.idle_stopped\"") && l.contains("juniper--k3x9")).map(str::to_string).collect::<Vec<_>>();
    assert!(!stopped.is_empty() && stopped.iter().all(|l| l.contains("\"code\":0")), "Hermes' own stop: {stopped:?}");
    assert_eq!(c.exec_out(&["sh", "-c", "pgrep -x Xvnc | wc -l"]).trim(), "1", "its X server gone");
    drop((fc, fv));
    let t = Instant::now();
    while desktop_up(&c, "fred--k3x9") && t.elapsed() < Duration::from_secs(120) {
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(!desktop_up(&c, "fred--k3x9"), "fred's too, once no one watches it");

    // and starts again on demand, its browser's profile kept
    let back = Viewer::open(&base, "watcher", "juniper--k3x9", Duration::from_secs(60)).await.unwrap_or_else(|e| panic!("juniper's screen again: {e}\n{}", c.logs()));
    assert_eq!(back.name, "hermes:juniper--k3x9");
    assert_eq!(c.exec_out(&["cat", marker]).trim(), "kept", "its browser's profile, in its work, kept");
    assert_eq!(c.exec_out(&["readlink", "/data/hermes/profiles/juniper--k3x9/bot-desktop/browser-profile"]).trim(), "/data/work/juniper--k3x9/browser-profile");
}

// ---- what Hermes would decide by guessing it runs in a container (its
// `is_container()`: Docker's marker `/.dockerenv`, which Containers has
// not; images/hermes/boot/src/hermes.rs, `RUNTIME_ENV`) ----

/// What one runtime shows of an agent's home.
#[derive(Debug)]
struct Homes {
    /// Whether Hermes takes the runtime for a container.
    container: bool,
    /// The agent's terminal's `HOME`, and where it leads.
    terminal: String,
    terminal_real: String,
    /// Where Hermes' write_file put `~/fragment-home.txt`, as it said, and
    /// where that leads.
    written: String,
    written_real: String,
    /// The modes of Hermes' home and its directories (`<mode> <path>`).
    modes: String,
}

/// The value after `name=` in what a tool said, up to a space or a quote.
fn said(text: &str, name: &str) -> String {
    let pat = format!("{name}=");
    text.split_once(&pat).map(|(_, rest)| rest.chars().take_while(|c| !c.is_whitespace() && !matches!(c, '"' | '\\' | ',')).collect()).unwrap_or_default()
}

/// The Hermes image on `runtime`: juniper's terminal says its `HOME`, and
/// its write_file writes under `~`.
async fn homes(runtime: Runtime) -> Homes {
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("home", &["juniper"]);
    let c = Container::run_on(runtime, &hermes_tag(), fake.addr.port(), model.addr.port(), &[]);
    within(&fake, &chat, &c, 180_000, "Hermes' bridge to follow its chat", |w| w.live_sockets() >= 2).await;
    let guess = c.exec_out(&["/command/s6-setuidgid", "hermes", "/opt/hermes/.venv/bin/python", "-c", "import os; os.chdir('/opt/hermes'); from hermes_platform.host.runtime import is_container; print(is_container())"]);
    let ask = |text: &str| {
        let said = fake.say(&chat, &person("paul"), json!({ "text": text }));
        fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", said["seq"].as_u64().unwrap())
    };
    let reply = |turn: &str| fake.with(|w| w.bodies(&chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn)).map(|r| r["text"].as_str().unwrap_or("").to_string());
    let turn = ask("run: echo home=$HOME real=$HERMES_REAL_HOME path=$PATH");
    within(&fake, &chat, &c, 240_000, "the terminal's reply", |w| w.bodies(&chat, "chat", "reply").iter().any(|r| r["turn"] == turn)).await;
    let terminal = reply(&turn).unwrap_or_default();
    eprintln!("home: on {runtime:?} the terminal said {terminal:?}");
    let turn = ask("write: ~/fragment-home.txt");
    within(&fake, &chat, &c, 240_000, "write_file's reply", |w| w.bodies(&chat, "chat", "reply").iter().any(|r| r["turn"] == turn)).await;
    let wrote = reply(&turn).unwrap_or_default();
    // the absolute path it wrote, of those it names
    let written = wrote.match_indices("fragment-home.txt").filter_map(|(at, _)| wrote[..at].rsplit(['"', ' ', '\'']).next()).find(|dir| dir.starts_with('/')).map(|dir| format!("{dir}fragment-home.txt")).unwrap_or_default();
    eprintln!("home: on {runtime:?} write_file said {wrote:?}");
    if !written.is_empty() {
        assert_eq!(c.exec_out(&["cat", &written]), support::model::WRITTEN, "write_file wrote where it said");
    }
    let dirs = ["/data/hermes", "/data/hermes/sessions", "/data/hermes/logs", "/data/hermes/memories", "/data/hermes/cron", "/data/hermes/cache/scratch", "/data/hermes/profiles/juniper--k3x9", "/data/hermes/profiles/juniper--k3x9/sessions", "/data/work/juniper--k3x9/tmp", "/data/work/juniper--k3x9/home"];
    let modes = c.exec_out(&["sh", "-c", &format!("stat -c '%a %n' {} 2>/dev/null", dirs.join(" "))]);
    let real = |p: &str| if p.is_empty() { String::new() } else { c.exec_out(&["realpath", p]).trim().to_string() };
    let terminal = said(&terminal, "home");
    Homes { container: guess.trim() == "True", terminal_real: real(&terminal), terminal, written_real: real(&written), written, modes }
}

/// Goal: an agent's home is the same on Docker and on Containers, though
/// Hermes takes only Docker for a container: its terminal's `HOME` and its
/// file tools' `~` are its profile's own `home` (each agent its own, as
/// Hermes gives a container), a link to its home in its work, and Hermes
/// leaves its home's modes as the image makes them. Pinned by the image
/// (`RUNTIME_ENV`), not guessed.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn an_agents_home_is_the_same_on_either_runtime() {
    build(&repo_dir(), "images/hermes/Dockerfile", &hermes_tag());
    let (docker, hosted) = tokio::join!(homes(Runtime::Docker), homes(Runtime::Hosted));
    eprintln!("home: Docker {docker:#?}\nhome: Containers' way {hosted:#?}");
    assert!(docker.container && !hosted.container, "Hermes takes Docker for a container and the hosted rung for none (else this proves nothing): {docker:?} {hosted:?}");
    let (profile_home, home) = ("/data/hermes/profiles/juniper--k3x9/home", "/data/work/juniper--k3x9/home");
    for (on, h) in [("Docker", &docker), ("Containers' way", &hosted)] {
        assert_eq!(h.terminal, profile_home, "on {on}, the agent's terminal's HOME is its profile's: {h:#?}");
        assert_eq!(h.terminal_real, home, "on {on}, which is its home in its work: {h:#?}");
        assert!(h.written.ends_with("/fragment-home.txt"), "on {on}, write_file said where it wrote: {h:#?}");
        assert_eq!(h.written_real, format!("{home}/fragment-home.txt"), "on {on}, its file tools' `~` is the same home: {h:#?}");
    }
    assert_eq!(docker.modes, hosted.modes, "Hermes leaves its home's modes alike on both");
}

/// The image's last `held` event: what its last hold copied, and kept hot.
fn held_event(c: &Container) -> Option<serde_json::Value> {
    c.logs().lines().rev().filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok()).find(|v| v["event"] == "held")
}

/// Goal (Paul, 2026-10-07: an agent's `~` is its work): a Chromium the
/// agent runs from its terminal, headless with no profile named, the full
/// one as its desktop runs it, keeps its databases out of Hermes' home, so
/// a hold while it runs keeps none of them hot (`locked` empty), and none
/// is under Hermes' home for a restore's check to find. Before, its profile
/// was under Hermes' home (on Containers `/data/hermes/.config/…`; with the
/// home pinned, the profile's `home/.config/…`), and its
/// `declarative_performance_observer.db` was held locked. Chrome 153 (Hermes
/// v0.21.5's image) kept that temporary profile under `~`, in its work;
/// Chrome 145 (Hermes v0.21.6) keeps it in `TMPDIR`, which Hermes points at
/// its profile's scratch (a link into its work, saved with it), so the
/// image's Chromium gives it the container's `/tmp` (hermes.rs,
/// `CHROMIUM_TMP`).
/// The agent starts it as Hermes' background process (`start:`).
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn a_browser_the_agent_runs_keeps_its_databases_in_its_work() {
    build(&repo_dir(), "images/hermes/Dockerfile", &hermes_tag());
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("browse", &["juniper"]);
    let c = Container::run_on(Runtime::Hosted, &hermes_tag(), fake.addr.port(), model.addr.port(), &[]);
    within(&fake, &chat, &c, 180_000, "Hermes' bridge to follow its chat", |w| w.live_sockets() >= 2).await;
    let said = fake.say(&chat, &person("paul"), json!({ "text": "start: DISPLAY=:99 /opt/fragment/bin/chromium --headless=new about:blank" }));
    let turn = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", said["seq"].as_u64().unwrap());
    within(&fake, &chat, &c, 240_000, "the terminal's reply", |w| w.bodies(&chat, "chat", "reply").iter().any(|r| r["turn"] == turn)).await;
    let dbs = |under: &str| c.exec_out(&["sh", "-c", &format!("find {under} \\( -name '*.db' -o -name History -o -name Cookies \\) -path '*chrom*' 2>/dev/null")]);
    let t = Instant::now();
    while dbs("/data/ /tmp/").trim().is_empty() {
        assert!(t.elapsed() < Duration::from_secs(60), "the agent's Chromium made no databases; the terminal said {:?}", fake.with(|w| w.bodies(&chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn)));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(c.exec(&["pgrep", "-f", "chrome-linux64/chrome"]), "the agent's Chromium runs into the hold");
    let left_out = hold(&c).unwrap_or_else(|| panic!("no answer to the hold; the container said:\n{}", c.logs()));
    let held = held_event(&c).unwrap_or_else(|| panic!("no held event; the container said:\n{}", c.logs()));
    eprintln!("browse: held {held}; left out {left_out:?}\nbrowse: its Chromium's databases:\n{}", dbs("/data/ /tmp/"));
    assert!(c.exec(&["pgrep", "-f", "chrome-linux64/chrome"]), "and through it");
    assert_eq!(held["locked"], json!([]), "no database kept hot for being locked: {held}");
    assert_eq!(dbs("/data/hermes/").trim(), "", "none of its databases is under Hermes' home");
    assert!(!dbs("/tmp/").trim().is_empty() || !dbs("/data/work/juniper--k3x9/home/").trim().is_empty(), "its temporary profile is the container's, or in its home, in its work");
    unhold(&c);
}

/// Says `text` and waits until `done` holds of its turn, any card the turn
/// asks allowed for the session: the turn.
async fn asked(fake: &Fake, chat: &str, c: &Container, text: &str, done: impl Fn(&support::fake::World, &str) -> bool) -> String {
    let said = fake.say(chat, &person("paul"), json!({ "text": text }));
    let turn = fragment_bridge::records::turn_id("juniper--k3x9", chat, "chat", said["seq"].as_u64().unwrap());
    let (t, mut allowed) = (Instant::now(), false);
    // bounded: four minutes
    loop {
        if fake.with(|w| done(w, &turn)) {
            return turn;
        }
        if !allowed {
            if let Some(card) = fake.with(|w| w.bodies(chat, "work", "turn.prompt").into_iter().find(|p| p["turn"] == turn)) {
                fake.say(chat, &person("paul"), json!({ "kind": "prompt_response", "prompt": card["prompt"], "option": "session" }));
                allowed = true;
            }
        }
        assert!(t.elapsed() < Duration::from_secs(240), "{text:?} not done in 240 s; {}", told(fake, chat, c));
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// What the tool of `text`'s turn said, as the scripted model quotes it
/// (`scripted: the tool said: …`).
async fn tool_said(fake: &Fake, chat: &str, c: &Container, text: &str) -> String {
    let reply = |w: &support::fake::World, turn: &str| w.bodies(chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn);
    let turn = asked(fake, chat, c, text, |w, turn| reply(w, turn).is_some()).await;
    fake.with(|w| reply(w, &turn)).and_then(|r| r["text"].as_str().map(str::to_string)).unwrap_or_default()
}

/// Goal (the seam, step 2 of docs/durable-computers.md): a temp file an
/// agent's tools make is its work (`/data/work/<profile>/tmp`), saved with
/// it, never Hermes' home. Hermes points every child's `TMPDIR` at its
/// home's `cache/scratch` (`hermes_constants.apply_scratch_tmp_env`), for an
/// agent's the profile's, which until 2026-10-07 was a directory under
/// Hermes' home (`/data/hermes/profiles/juniper--k3x9/cache/scratch`); the
/// image makes it a link into the work (`hermes::tmp_dir`). Each way a tool
/// runs is given its environment its own way, so each is asked: its
/// terminal (`mktemp`), a background process it starts (`mktemp`, its path
/// written to a file), and execute_code (Python's `tempfile`), whose
/// scripts Hermes gives the gateway's own temp directory instead
/// (docs/technical-debt-ledger.md, "An agent's execute_code makes its temp
/// files in Hermes' home"): pinned here, so the day Hermes fixes it this
/// fails and the entry goes. Then Hermes' own use of the scratch: a file
/// made there and named in the reply's `MEDIA:` tag, as Hermes' prompts
/// suggest, is still sent, read through the link.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn a_tools_temp_files_are_its_work() {
    build(&repo_dir(), "images/hermes/Dockerfile", &hermes_tag());
    let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
    let model = Model::start("0.0.0.0:0").await;
    let chat = fake.chat("temp", &["juniper"]);
    let c = Container::run(&hermes_tag(), fake.addr.port(), model.addr.port(), &[]);
    within(&fake, &chat, &c, 180_000, "Hermes' bridge to follow its chat", |w| w.live_sockets() >= 2).await;
    let (work, real) = ("/data/work/juniper--k3x9/tmp", |p: &str| c.exec_out(&["realpath", "-e", p]).trim().to_string());
    let terminal = tool_said(&fake, &chat, &c, "run: echo tmpdir=$TMPDIR made=$(mktemp)").await;
    let started = "/data/work/juniper--k3x9/started-temp.txt";
    let background = tool_said(&fake, &chat, &c, &format!("start: echo made=$(mktemp) > {started}")).await;
    let t = Instant::now();
    // bounded: a minute
    while !c.exec(&["test", "-s", started]) {
        assert!(t.elapsed() < Duration::from_secs(60), "the background process wrote nothing; the terminal said {background:?}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let background = c.exec_out(&["cat", started]);
    let code = tool_said(&fake, &chat, &c, "code: import tempfile; print('made=' + tempfile.mkstemp()[1])").await;
    let made: Vec<(&str, String, String)> = [("its terminal", &terminal), ("a background process", &background), ("execute_code", &code)].into_iter().map(|(tool, text)| (tool, said(text, "made"), text.clone())).collect();
    for (tool, path, _) in &made {
        eprintln!("temp: {tool} made {path:?}, in {:?}", real(path));
    }
    eprintln!("temp: the gateway's own scratch holds {:?}", c.exec_out(&["ls", "-A", "/data/hermes/cache/scratch"]));
    let tmpdir = said(&terminal, "tmpdir");
    assert_eq!(real(&tmpdir), work, "its terminal's TMPDIR ({tmpdir:?}) is its work's: {terminal}");
    for (tool, path, text) in &made {
        assert!(path.starts_with('/'), "{tool} said where it made its temp file: {text:?}");
        let in_fact = real(path);
        if *tool == "execute_code" {
            assert!(
                in_fact.starts_with("/data/hermes/cache/scratch/"),
                "execute_code's temp file is no longer the gateway's own but {in_fact:?}: Hermes now keeps its scratch marker for execute_code's scripts, so drop the ledger's entry (\"An agent's execute_code makes its temp files in Hermes' home\") and hold it to its work with the rest"
            );
        } else {
            assert!(in_fact.starts_with(&format!("{work}/")), "{tool}'s temp file {path:?} is in its work, not {in_fact:?}");
        }
    }
    let attached = |w: &support::fake::World, turn: &str| w.bodies(&chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn && r.get("attachments").is_some());
    let turn = asked(&fake, &chat, &c, "send: f=$(mktemp --suffix=.txt) && echo temp-sent > $f && echo made=$f", |w, turn| attached(w, turn).is_some()).await;
    fake.with(|w| {
        let reply = attached(w, &turn).unwrap();
        let sha = reply["attachments"][0]["sha256"].as_str().unwrap_or("");
        let sent = w.fragments[&chat].blobs.get(sha).map(|(_, b)| b.clone());
        assert_eq!(sent.as_deref(), Some(&b"temp-sent\n"[..]), "the temp file Hermes sent is the one in its work: {reply}");
    });
}

// ---- an approval nobody answers (Paul on p5, 2026-10-05: "I missed the
// 1hr window and now it's not responding to chats") ----

/// Hermes' approval timeout in the expiry tests, and with it the card's life
/// (`HERMES_BOOT_APPROVAL_TIMEOUT_S`: the image gives the bridge the same).
const APPROVAL_S: u64 = 20;

/// What the chat and its `work` hold, and the container's last lines: a
/// failure's detail.
fn told(fake: &Fake, chat: &str, c: &Container) -> String {
    let records = fake.with(|w| format!("chat: {:?}\nwork: {:?}", w.records(chat, "chat").iter().map(|r| r["body"].clone()).collect::<Vec<_>>(), w.records(chat, "work").iter().map(|r| r["body"].clone()).collect::<Vec<_>>()));
    let logs = c.logs();
    let _ = std::fs::write(PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("expired-{}.log", &c.id[..12.min(c.id.len())])), &logs);
    format!("the fake holds\n{records}\nthe container said:\n{}", logs.lines().rev().take(80).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"))
}

/// Waits up to `ms` for `cond`, failing with what the chat and the
/// container say.
async fn within(fake: &Fake, chat: &str, c: &Container, ms: u64, what: &str, cond: impl Fn(&support::fake::World) -> bool) {
    if tokio::time::timeout(Duration::from_millis(ms), fake.until(ms + 60_000, what, &cond)).await.is_err() {
        panic!("waited {ms} ms for {what}; {}", told(fake, chat, c));
    }
}

/// A chat with Hermes on the line: the image with a short approval, its
/// agent's first reply given.
struct Expiry {
    fake: Fake,
    model: Model,
    chat: String,
}

impl Expiry {
    async fn start() -> (Expiry, Container) {
        build(&repo_dir(), "images/hermes/Dockerfile", &hermes_tag());
        let fake = Fake::start("0.0.0.0:0", &["juniper"]).await;
        let model = Model::start("0.0.0.0:0").await;
        let chat = fake.chat("talk", &["juniper"]);
        let x = Expiry { fake, model, chat };
        let c = x.container(&[]);
        within(&x.fake, &x.chat, &c, 120_000, "Hermes' bridge to follow its chat", |w| w.live_sockets() >= 2).await;
        let first = x.say("hello");
        within(&x.fake, &x.chat, &c, 180_000, "Hermes' first reply", |w| !x.ends(w, &first).is_empty()).await;
        (x, c)
    }

    fn container(&self, extra: &[(&str, &str)]) -> Container {
        let approval = APPROVAL_S.to_string();
        let mut env = vec![("HERMES_BOOT_APPROVAL_TIMEOUT_S", approval.as_str())];
        env.extend_from_slice(extra);
        Container::run(&hermes_tag(), self.fake.addr.port(), self.model.addr.port(), &env)
    }

    /// Paul says `text`: its turn.
    fn say(&self, text: &str) -> String {
        let said = self.fake.say(&self.chat, &person("paul"), json!({ "text": text }));
        fragment_bridge::records::turn_id("juniper--k3x9", &self.chat, "chat", said["seq"].as_u64().unwrap())
    }

    fn ends(&self, w: &support::fake::World, turn: &str) -> Vec<serde_json::Value> {
        w.bodies(&self.chat, "work", "turn.end").into_iter().filter(|e| e["turn"] == turn).collect()
    }

    fn cards(&self, w: &support::fake::World, turn: &str) -> Vec<serde_json::Value> {
        w.bodies(&self.chat, "work", "turn.prompt").into_iter().filter(|p| p["turn"] == turn).collect()
    }

    fn closed(&self, w: &support::fake::World, turn: &str) -> Vec<serde_json::Value> {
        w.bodies(&self.chat, "work", "turn.prompt.closed").into_iter().filter(|p| p["turn"] == turn).collect()
    }

    fn reply(&self, w: &support::fake::World, turn: &str) -> Option<serde_json::Value> {
        w.bodies(&self.chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn)
    }

    /// The model's calls since `from`: each one's last few messages (role,
    /// and the start of its text or its tool calls).
    fn model_saw(&self, from: usize) -> Vec<Vec<String>> {
        let calls = self.model.calls.lock().unwrap();
        calls[from.min(calls.len())..]
            .iter()
            .filter(|c| c.path.ends_with("/chat/completions"))
            .map(|c| {
                let messages = c.body["messages"].as_array().cloned().unwrap_or_default();
                messages[messages.len().saturating_sub(5)..].iter().map(|m| format!("{}: {} {}", m["role"], m["content"].to_string().chars().take(140).collect::<String>(), m["tool_calls"].to_string().chars().take(140).collect::<String>())).collect()
            })
            .collect()
    }
}


/// Goal (Paul on p5, 2026-10-05: "the agent asked me for permission for
/// something but I missed the 1hr window and now it's not responding to
/// chats"), with real Hermes: an approval card nobody answers is closed
/// `expired` once its `expiresAt` passes, its turn ends once, as Hermes ends
/// it (its approval times out with the card: the command is not run), and
/// the chat's next message is claimed and answered, not met by the card's
/// command asked again; and a message said while a card is open waits
/// behind its turn, then is answered the same way.
///
/// Method: the image with a 20 s approval (`HERMES_BOOT_APPROVAL_TIMEOUT_S`;
/// the bridge's card lives as long), the scripted model asking for a
/// command Hermes flags, and the Computer DO's idle rule in a test's time
/// (crates/core/src/computer.rs, `IDLE_MS`: a computer with no keepalive
/// open sleeps 20 minutes after its last record; here, at once): if the
/// bridge lets its keepalive go under the card, the computer is put to sleep
/// as the DO does it (the hold, a save of `/data`, SIGTERM) and woken from
/// that save by the next message, after the card's `expiresAt`; otherwise it
/// stays awake through the expiry. On master it is let go (decision 42),
/// and the woken Hermes folds the next message into the cut request (its
/// model is given `[paul] do the risky thing\n\n[paul] good morning`), so
/// the next message asks the cut command's approval again.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn an_expired_approval_ends_its_turn() {
    let (x, c) = Expiry::start().await;
    let (fake, chat) = (&x.fake, x.chat.as_str());

    // 1. a card nobody answers; then the next message
    let risky = x.say("do the risky thing");
    within(fake, chat, &c, 120_000, "the approval card", |w| !x.cards(w, &risky).is_empty()).await;
    let asked = fake.with(|w| x.cards(w, &risky)[0].clone());
    let expires_at = asked["expiresAt"].as_u64().unwrap();
    assert!(expires_at.saturating_sub(fragment_bridge::log::now_ms()) <= APPROVAL_S * 1000 + 2_000, "the card lives as long as Hermes waits: {asked}");
    // the DO's idle rule: what the keepalive does while the card waits
    tokio::time::sleep(Duration::from_secs(1)).await;
    let let_go = fake.with(|w| w.keepalive_open == 0);
    let calls_before = x.model.calls.lock().unwrap().len();
    let (c, next) = if let_go {
        let left_out = hold(&c).unwrap_or_else(|| panic!("no answer to the hold; {}", told(fake, chat, &c)));
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("expired");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let save = dir.join("asleep.tar");
        save_data(&c, &left_out, &save);
        let (took, _) = c.sigterm();
        eprintln!("expired: the keepalive let go under the card; held, saved and stopped ({} ms to exit)", took.as_millis());
        let _ = told(fake, chat, &c);
        drop(c);
        fake.until(30_000, "its sockets closed", |w| w.live_sockets() == 0).await;
        // bounded: the card's life
        while fragment_bridge::log::now_ms() < expires_at + 3_000 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // the next message wakes it from the save
        let next = x.say("good morning");
        let c = x.container(&[("RESTORE_PENDING", "1")]);
        let tar = std::fs::File::open(&save).expect("the save");
        let cp = Command::new(docker()).args(["cp", "-a", "-", &format!("{}:/data", c.id)]).stdin(tar).output().expect("docker runs");
        assert!(cp.status.success(), "the restore: {}", String::from_utf8_lossy(&cp.stderr));
        let (code, said) = c.exec_code(&["/usr/local/bin/computer-check"]);
        assert_eq!(code, 0, "the save checks: {said}");
        assert!(c.exec(&["touch", "/run/computer/restored"]));
        (c, next)
    } else {
        within(fake, chat, &c, APPROVAL_S * 1000 + 60_000, "the card expired, awake, and its turn's end", |w| x.closed(w, &risky).iter().any(|p| p["outcome"] == "expired") && !x.ends(w, &risky).is_empty()).await;
        fake.with(|w| eprintln!("expired, awake: its end {:?}, its reply {:?}", x.ends(w, &risky), x.reply(w, &risky)));
        (c, x.say("good morning"))
    };
    within(fake, chat, &c, 240_000, "the next message's turn to end", |w| !x.ends(w, &next).is_empty()).await;
    let saw = x.model_saw(calls_before);
    fake.with(|w| {
        let asked_again = x.cards(w, &next);
        assert!(asked_again.is_empty(), "the next message asks nothing again (the keepalive let go under the card: {let_go}); the model saw {saw:#?}; it asked {asked_again:?}");
        assert!(x.reply(w, &next).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("good morning")), "the next message is answered: {:?}; the model saw {saw:#?}", x.reply(w, &next));
        assert_eq!((x.ends(w, &risky).len(), x.closed(w, &risky).len()), (1, 1), "the card's turn ends once, its card closed once");
        assert_eq!(x.closed(w, &risky)[0]["outcome"], "expired");
        assert_eq!(x.ends(w, &risky)[0]["outcome"], "idle", "Hermes ended it, its approval timed out with the card");
    });
    assert!(!let_go, "the card holds the computer awake");

    // 2. a message while a card is open waits behind it, then is answered
    let risky2 = x.say("do the risky thing once more");
    within(fake, chat, &c, 120_000, "the second card", |w| !x.cards(w, &risky2).is_empty()).await;
    let meanwhile = x.say("hello while you wait");
    within(fake, chat, &c, APPROVAL_S * 1000 + 90_000, "the second card expired, and its turn's end", |w| x.closed(w, &risky2).iter().any(|p| p["outcome"] == "expired") && !x.ends(w, &risky2).is_empty()).await;
    within(fake, chat, &c, 120_000, "the message said while it waited answered", |w| x.reply(w, &meanwhile).is_some() && !x.ends(w, &meanwhile).is_empty()).await;
    fake.with(|w| {
        assert_eq!((x.ends(w, &risky2).len(), x.closed(w, &risky2).len()), (1, 1), "each once");
        assert!(x.cards(w, &meanwhile).is_empty(), "it asks nothing again: {:?}", x.cards(w, &meanwhile));
        assert!(x.reply(w, &meanwhile).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("hello while you wait")), "{:?}", x.reply(w, &meanwhile));
    });
}

// ---- a turn a restart cuts, then the next message (P5; F10) ----

/// The text of a model message (its content, or its parts' text).
fn content_text(m: &serde_json::Value) -> String {
    match &m["content"] {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
        _ => String::new(),
    }
}

/// Of the first model request whose last user message says `said`: that
/// message's text, and the message before it (its role and text).
fn asked_with(calls: &[support::model::Call], said: &str) -> Option<(String, (String, String))> {
    calls.iter().filter(|c| c.path.ends_with("/chat/completions")).find_map(|c| {
        let messages = c.body["messages"].as_array()?;
        let at = messages.iter().rposition(|m| m["role"] == "user")?;
        let last = content_text(&messages[at]);
        if !last.contains(said) {
            return None;
        }
        let before = at.checked_sub(1).map(|i| (messages[i]["role"].as_str().unwrap_or("").to_string(), content_text(&messages[i]))).unwrap_or_default();
        Some((last, before))
    })
}

/// Goal (P5; F10, with real Hermes v0.21.5): a turn a restart cuts while its
/// card waits (here an owner's sleep: the hold, a save of `/data`, SIGTERM)
/// is never redone by the chat's next message. Woken from that save, the
/// image's boot closes the cut turn in Hermes' session with Hermes' own
/// failed-turn boundary, and the bridge tells the next turn what was cut,
/// from the journal. So the model's request for "good morning" ends with a
/// user message of its own (the note as its channel context, then the
/// message) after an assistant message (the boundary), never joined to the
/// cut request; no request after the wake has the model call the cut
/// command again, no card is shown again, "good morning" is answered, and
/// the message after it is told nothing.
///
/// Method: the image with a 20 s approval, the scripted model asking for a
/// command Hermes flags (`risky`), and the DO's owner's sleep under the
/// card, taken as the expiry test above takes an idle one; then a wake from
/// that save. On master (no closing at boot, no note) Hermes joins the next
/// message to the cut request (its model is given `[paul] do the risky
/// thing\n\n[paul] good morning`), the scripted model calls the cut command
/// again, and its card is shown again (`FRAGMENT_DOCKER_SKIP_BUILD=1
/// FRAGMENT_DOCKER_HERMES_TAG=<an image of master's>` runs this against it).
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn a_turn_cut_by_a_restart_is_closed_and_told() {
    let (x, c) = Expiry::start().await;
    let (fake, chat) = (&x.fake, x.chat.as_str());
    let risky = x.say("do the risky thing");
    within(fake, chat, &c, 120_000, "the approval card", |w| !x.cards(w, &risky).is_empty()).await;
    // the owner's sleep under the card: held, saved, stopped
    let left_out = hold(&c).unwrap_or_else(|| panic!("no answer to the hold; {}", told(fake, chat, &c)));
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cut-told");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let save = dir.join("asleep.tar");
    save_data(&c, &left_out, &save);
    let (took, _) = c.sigterm();
    eprintln!("cut: held, saved and stopped under the card ({} ms to exit)", took.as_millis());
    let _ = told(fake, chat, &c);
    drop(c);
    fake.until(30_000, "its sockets closed", |w| w.live_sockets() == 0).await;
    let calls_before = x.model.calls.lock().unwrap().len();

    // the next message wakes it from the save
    let next = x.say("good morning");
    let c = x.container(&[("RESTORE_PENDING", "1")]);
    let tar = std::fs::File::open(&save).expect("the save");
    let cp = Command::new(docker()).args(["cp", "-a", "-", &format!("{}:/data", c.id)]).stdin(tar).output().expect("docker runs");
    assert!(cp.status.success(), "the restore: {}", String::from_utf8_lossy(&cp.stderr));
    let (code, said) = c.exec_code(&["/usr/local/bin/computer-check"]);
    assert_eq!(code, 0, "the save checks: {said}");
    assert!(c.exec(&["touch", "/run/computer/restored"]));
    // its end, or the cut command's card shown again (master: the fold)
    within(fake, chat, &c, 240_000, "the next message's turn to end", |w| !x.ends(w, &next).is_empty() || !x.cards(w, &next).is_empty()).await;
    let again = fake.with(|w| x.cards(w, &next));
    assert!(again.is_empty(), "the cut command's card is not shown again: {again:?}; the model saw {:#?}", x.model_saw(calls_before));
    let after = x.say("and after that");
    within(fake, chat, &c, 120_000, "the turn after's end", |w| !x.ends(w, &after).is_empty()).await;

    let calls: Vec<support::model::Call> = x.model.calls.lock().unwrap()[calls_before..].to_vec();
    let redone: Vec<String> = calls.iter().filter(|c| c.path.ends_with("/chat/completions")).filter_map(|c| support::model::answer(&c.body).1).map(|call| call.to_string()).filter(|call| call.contains("rm -rf")).collect();
    let logs = c.logs();
    let closed = logs.lines().find(|l| l.contains("boot.cut_turns_closed") || l.contains("boot.cut_turns_failed")).unwrap_or("no word from the boot's closer").to_string();
    let saw = x.model_saw(calls_before);
    fake.with(|w| {
        assert_eq!(x.ends(w, &risky).len(), 1, "the cut turn ends once");
        assert_eq!(x.ends(w, &risky)[0]["error"], "lost when the computer restarted");
        assert!(redone.is_empty(), "no request after the wake has the model call the cut command again: {redone:?}; the model saw {saw:#?}");
        assert!(x.cards(w, &next).is_empty() && x.cards(w, &after).is_empty(), "no card shown again: {:?}; the model saw {saw:#?}", x.cards(w, &next));
        assert!(x.reply(w, &next).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("good morning")), "good morning is answered: {:?}; the model saw {saw:#?}", x.reply(w, &next));
    });
    let (asked, (before_role, before)) = asked_with(&calls, "good morning").unwrap_or_else(|| panic!("a model request for good morning; it saw {saw:#?}"));
    assert!(asked.starts_with("[Recent channel messages]\n"), "the note first, nothing joined before it: {asked:?}");
    for said in ["Your previous turn in this chat was cut short: your computer restarted before it finished.", "Check what it already did before you do any of it again", "It was answering: “do the risky thing”", "[New message]\n[paul] good morning"] {
        assert!(asked.contains(said), "{said:?} in the model's request: {asked:?}");
    }
    assert!(!asked.contains("[paul] do the risky thing"), "the cut request is never joined to it: {asked:?}");
    assert_eq!(before_role, "assistant", "the cut turn is closed before it (Hermes' failed-turn boundary; {closed}): {before:?}");
    let (asked_after, _) = asked_with(&calls, "and after that").expect("a request for the message after");
    assert!(!asked_after.contains("cut short"), "the turn after is told nothing: {asked_after:?}");
    eprintln!("cut: {closed}\ncut: the boundary the model saw: {before:?}\ncut: the message: {asked:?}");
}

// ---- Hermes' catalog refresh under the hold (2026-10-07:
// `held_nothing_under_data_changes` failed once, in a parallel run, when the
// refresh landed inside its hold) ----

/// Python in Hermes' own environment, as its gateway runs it (the hermes
/// user, its home), with `env` added: the JSON of its last line.
fn hermes_python(c: &Container, env: &[&str], code: &str) -> serde_json::Value {
    let mut cmd = vec!["/command/s6-setuidgid", "hermes", "env", "HOME=/data/hermes", "HERMES_HOME=/data/hermes"];
    cmd.extend_from_slice(env);
    cmd.extend_from_slice(&["/opt/hermes/.venv/bin/python", "-c", code]);
    let (code, said) = c.exec_code(&cmd);
    assert_eq!(code, 0, "Hermes' Python: {said}");
    let last = said.lines().rev().find(|l| l.starts_with('{')).unwrap_or_else(|| panic!("no JSON from Hermes' Python: {said}"));
    serde_json::from_str(last).unwrap_or_else(|e| panic!("{e}: {last}"))
}

/// What the gateway's `_model_catalog_refresh_watcher` (Hermes'
/// gateway/run_watchers.py) runs at each tick, 30 s after it starts and
/// every 20 minutes: `refresh_catalogs`, which first refreshes the manifest
/// (`get_catalog(force_refresh=True)`), then OpenRouter's and Nous' lists.
const REFRESH: &str = "import json\nfrom hermes_cli import model_catalog as m\nenabled = m._load_catalog_config()['enabled']\nrefreshed = m.refresh_catalogs()\nprint(json.dumps({'enabled': enabled, 'refreshed': refreshed, 'catalog': m.get_catalog(force_refresh=True)}))";

/// Goal: Hermes' model-catalog refresh, which wrote four caches in its home
/// whenever its timer landed inside a hold (2026-10-07), writes nothing in
/// our image, held or not: `model_catalog.enabled: false` in the managed
/// overlay (docs/durable-computers.md, "What changes under the hold"); and a
/// wake from a save whose catalog caches are torn answers as any other, so
/// a torn cache never breaks Hermes on wake.
/// Method: Hermes held; the watcher's own refresh forced inside the hold,
/// as the gateway runs it, with the image's overlay: it reads the catalog
/// off, fetches and writes nothing, and nothing the save keeps changes but
/// what Hermes' timers rewrite whole (`kept_hot`). Then the same manifest
/// refresh with the catalog on (a managed overlay of this test's, the
/// manifest a local file, so no network): the write the failure saw, a
/// cache file the save keeps, rewritten under the hold. Then each of the
/// four torn as an older save could carry them (invalid UTF-8 cut short,
/// JSON cut short, empty, not JSON, and a temp file caught mid-write), the
/// save taken, the computer stopped and woken from it as the platform does
/// (`RESTORE_PENDING`, its check, the gate), and a message answered.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn a_catalog_refresh_forced_under_the_hold_writes_nothing() {
    let (x, c) = Expiry::start().await;
    let (fake, chat) = (&x.fake, x.chat.as_str());
    // held once Hermes' start-up writes are done: its kanban dispatcher
    // makes its board's database some seconds after the gateway starts, and
    // until its first page is written (a WAL database's file starts empty)
    // it is no database the hold copies, so its `-wal` and `-shm` would
    // change inside the hold, named by nothing (seen under load, 2026-10-08)
    let t = Instant::now();
    // bounded: two minutes
    while c.exec_out(&["head", "-c", "15", "/data/hermes/kanban.db"]) != "SQLite format 3" {
        assert!(t.elapsed() < Duration::from_secs(120), "Hermes never wrote its kanban board's database");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    eprintln!("catalog: Hermes' kanban board written {} ms after its first reply", t.elapsed().as_millis());
    let left_out = hold(&c).unwrap_or_else(|| panic!("no answer to the hold; {}", told(fake, chat, &c)));
    let before = files(&c, &left_out);
    let forced = hermes_python(&c, &[], REFRESH);
    let after = files(&c, &left_out);
    let changed_off = changed(&before, &after);
    eprintln!("catalog: forced under the hold, off: {forced}; changed: {changed_off:?}");
    assert_eq!(forced["enabled"], false, "Hermes reads its catalog off, from the image's overlay: {forced}");
    assert_eq!(forced["refreshed"], false, "the refresh fetched nothing: {forced}");
    assert_eq!(forced["catalog"], json!({}), "and has no catalog: {forced}");
    assert!(unexplained(&changed_off).is_empty(), "nothing the save keeps changed but what Hermes' timers rewrite whole: {:?}", unexplained(&changed_off));
    for cache in CATALOG_CACHES {
        assert!(!c.exec(&["test", "-e", &format!("/data/hermes/cache/{cache}")]), "no {cache}");
    }

    // what the failure saw: the catalog on, its manifest a local file
    let manifest = json!({ "version": 1, "providers": { "openrouter": { "models": [{ "id": "fragment/test", "description": "a test's" }] } } });
    let overlay = "model_catalog:\n  enabled: true\n  url: \"file:///tmp/catalog-on/model-catalog.json\"\n";
    let setup = "mkdir -p /tmp/catalog-on && printf '%s' \"$1\" > /tmp/catalog-on/config.yaml && printf '%s' \"$2\" > /tmp/catalog-on/model-catalog.json && chmod -R a+rX /tmp/catalog-on";
    assert!(c.exec(&["sh", "-c", setup, "sh", overlay, &manifest.to_string()]), "the test's overlay");
    let before = files(&c, &left_out);
    let on = hermes_python(&c, &["HERMES_MANAGED_DIR=/tmp/catalog-on"], "import json\nfrom hermes_cli import model_catalog as m\nprint(json.dumps({'enabled': m._load_catalog_config()['enabled'], 'catalog': m.get_catalog(force_refresh=True)}))");
    let after = files(&c, &left_out);
    let changed_on = changed(&before, &after);
    eprintln!("catalog: forced under the hold, on: {on}; changed: {changed_on:?}");
    assert_eq!(on["enabled"], true, "{on}");
    assert_eq!(on["catalog"], manifest, "the manifest fetched: {on}");
    assert_eq!(unexplained(&changed_on), vec!["/data/hermes/cache/model_catalog.json"], "on, the refresh rewrites a cache the save keeps, under the hold");
    whole(&c, &changed_on);

    // the four torn, as an older save could carry them; then a sleep and a wake
    let tear = "cd /data/hermes/cache && printf '{\"version\": 1, \"providers\": {\"openrouter\": {\"mo\\377\\376' > model_catalog.json && printf '{\"fetched_at\": 17' > openrouter_curated_catalog.json && : > nous_recommended_cache.json && printf 'not json' > reasoning_caps.json && printf '{\"version\": 1, \"prov' > .model_catalog_torn.tmp";
    let (code, said) = c.exec_code(&["/command/s6-setuidgid", "hermes", "sh", "-c", tear]);
    assert_eq!(code, 0, "the caches torn: {said}");
    let torn = c.exec_out(&["sh", "-c", "cd /data/hermes/cache && sha256sum model_catalog.json openrouter_curated_catalog.json nous_recommended_cache.json reasoning_caps.json .model_catalog_torn.tmp"]);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("catalog-torn");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let save = dir.join("asleep.tar");
    save_data(&c, &left_out, &save);
    let (took, _) = c.sigterm();
    eprintln!("catalog: held, saved with torn caches, stopped ({} ms to exit)", took.as_millis());
    drop(c);
    fake.until(30_000, "its sockets closed", |w| w.live_sockets() == 0).await;
    let next = x.say("good morning");
    let c = x.container(&[("RESTORE_PENDING", "1")]);
    let tar = std::fs::File::open(&save).expect("the save");
    let cp = Command::new(docker()).args(["cp", "-a", "-", &format!("{}:/data", c.id)]).stdin(tar).output().expect("docker runs");
    assert!(cp.status.success(), "the restore: {}", String::from_utf8_lossy(&cp.stderr));
    let (code, said) = c.exec_code(&["/usr/local/bin/computer-check"]);
    assert_eq!(code, 0, "the save checks: {said}");
    assert_eq!(c.exec_out(&["sh", "-c", "cd /data/hermes/cache && sha256sum model_catalog.json openrouter_curated_catalog.json nous_recommended_cache.json reasoning_caps.json .model_catalog_torn.tmp"]), torn, "the wake has the torn caches");
    assert!(c.exec(&["touch", "/run/computer/restored"]));
    within(fake, chat, &c, 240_000, "the message after the wake answered", |w| x.reply(w, &next).is_some()).await;
    let woke = hermes_python(&c, &[], REFRESH);
    assert_eq!(woke["catalog"], json!({}), "woken, its catalog is still off, the torn caches unread: {woke}");
    fake.with(|w| assert!(x.reply(w, &next).is_some_and(|r| r["text"].as_str().unwrap_or("").contains("good morning")), "good morning is answered: {:?}", x.reply(w, &next)));
}

// ---- nothing installed at run time (Paul, 2026-10-07) ----

/// Goal: Hermes installs nothing at run time. Its lazy installs are off, as
/// Hermes itself reads them, both in the gateway's own scope and in an
/// agent's profile (upstream's image turns them on, into its home's
/// `installs`). Its `text_to_speech` is still offered, Edge's SDK being in
/// the image (Hermes' image leaves it to a first-use install). A feature
/// asked for at run time (local Whisper, as a voice note's local fallback
/// would) is refused. And nothing an install leaves is under `/data`.
/// Method: Hermes' own PM (`lazy_installs_allowed`, `extras.available`,
/// `ensure_import`, as its tools call it) and `check_tts_requirements`, run
/// as its gateway runs them, in each scope; then `/data` searched.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn nothing_is_installed_at_run_time() {
    let (_fake, _model, _chat, c) = hermes_running().await;
    const PROBE: &str = "import json, os\nos.chdir('/opt/hermes')\nfrom pm import lazy_installs_allowed, ensure_import\nfrom pm.extras import available\nfrom tools.tts_tool import check_tts_requirements\ntry:\n    ensure_import('stt-whisper')\n    forced = 'installed'\nexcept Exception as e:\n    forced = str(e)\nprint(json.dumps({'allow': lazy_installs_allowed(), 'edge': available('edge-tts'), 'whisper': available('stt-whisper'), 'speaks': check_tts_requirements(), 'forced': forced}))";
    for home in ["/data/hermes", "/data/hermes/profiles/juniper--k3x9"] {
        let seen = hermes_python(&c, &[&format!("HERMES_HOME={home}")], PROBE);
        eprintln!("lazy: {home}: {seen}");
        assert_eq!(seen["allow"], json!(false), "lazy installs are off in {home}: {seen}");
        assert_eq!(seen["edge"], json!(true), "Edge's SDK is in the image: {seen}");
        assert_eq!(seen["speaks"], json!(true), "text_to_speech is offered in {home}: {seen}");
        assert_eq!(seen["whisper"], json!(false), "local Whisper is not: {seen}");
        assert!(seen["forced"].as_str().is_some_and(|f| f.contains("lazy installs are disabled")), "an install asked for at run time is refused in {home}: {seen}");
    }
    // no package, generation or installer cache of an install under /data
    let left = c.exec_out(&["sh", "-c", "find /data -maxdepth 7 \\( -path '*/lazy-packages/*' -o -path '*/installs/*/site-packages' -o -path '*/.cache/uv' -o -iname 'edge_tts*' -o -iname '*faster_whisper*' \\) | head -5"]);
    assert_eq!(left.trim(), "", "no install left anything under /data");
}

// ---- a voice memo (decision 9: a voice memo is one the agent transcribes
// itself, through the platform's model route) ----

/// Goal (decision 9; Paul, 2026-10-07): a voice note a person attaches in
/// the chat reaches Hermes, which transcribes it through the model route's
/// `whisper` (OpenAI's transcription shape, its agent named by its key,
/// `agent:<name>`, since Hermes' STT client sends no header of ours; no
/// language forced, so Whisper detects it), and answers once, having heard
/// it: no transcript echoed as a message of its own, and no local Whisper
/// installed for it. Method: a memo (a WAV that says its words, as the
/// scripted model reads them) put in the chat's blobs and said as an
/// attachment; the scripted model's calls, the turn's reply, and `/data`.
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn a_voice_memo_is_transcribed_through_the_route() {
    use sha2::Digest;
    let (fake, model, chat, c) = hermes_running().await;
    let words = "please remember the blue door";
    let audio = support::model::memo(words);
    let sha = fragment_bridge::records::hex(&sha2::Sha256::digest(&audio));
    fake.with(|w| w.fragments.get_mut(&chat).expect("the chat").blobs.insert(sha.clone(), ("audio/wav".into(), bytes::Bytes::from(audio.clone()))));
    let calls_before = model.calls.lock().unwrap().len();
    let said = fake.say(&chat, &person("paul"), json!({ "attachments": [{ "sha256": sha, "size": audio.len(), "type": "audio/wav", "name": "memo.wav" }] }));
    let t = fragment_bridge::records::turn_id("juniper--k3x9", &chat, "chat", said["seq"].as_u64().unwrap());
    fake.until(180_000, "the memo's turn to end", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t)).await;
    let calls: Vec<support::model::Call> = model.calls.lock().unwrap()[calls_before..].to_vec();
    let heard: Vec<&support::model::Call> = calls.iter().filter(|c| c.path.ends_with("/audio/transcriptions")).collect();
    eprintln!("memo: transcriptions {:?}", heard.iter().map(|c| (&c.body, &c.authorization)).collect::<Vec<_>>());
    assert_eq!(heard.len(), 1, "one transcription through the route: {:?}", calls.iter().map(|c| &c.path).collect::<Vec<_>>());
    let h = heard[0];
    assert_eq!(h.authorization.as_deref(), Some("Bearer agent:juniper--k3x9"), "its key names its agent");
    assert_eq!((h.body["model"].clone(), h.body["language"].clone(), h.body["audio_bytes"].clone()), (json!("whisper"), serde_json::Value::Null, json!(audio.len())), "the route's whisper, the memo whole, no language forced");
    let asked = calls.iter().filter(|c| c.path.ends_with("/chat/completions")).any(|c| c.body["messages"].to_string().contains(words));
    assert!(asked, "the turn's model call carries what the memo said: {:#?}", calls.iter().map(|c| c.body["messages"].to_string().chars().take(400).collect::<String>()).collect::<Vec<_>>());
    let replies = fake.with(|w| w.bodies(&chat, "chat", "reply").into_iter().filter(|r| r["turn"] == t).collect::<Vec<_>>());
    eprintln!("memo: replies {replies:?}");
    assert_eq!(replies.len(), 1, "one reply, no transcript echoed as a message of its own: {replies:?}");
    assert!(!replies[0]["text"].as_str().unwrap_or("").contains('🎙'), "{replies:?}");
    assert_eq!(c.exec_out(&["sh", "-c", "find /data -iname '*faster_whisper*' -o -iname 'ctranslate2*' | head -3"]).trim(), "", "no local Whisper installed for it");
}

// ---- Hermes' Bot Mode (images/hermes/boot/src/bots.rs, images/hermes/botmode.py) ----

/// The model calls made as `agent` whose messages hold `text`.
fn calls_of(model: &Model, agent: &str, text: &str) -> Vec<support::model::Call> {
    model.calls.lock().unwrap().iter().filter(|m| m.agent.as_deref() == Some(agent) && m.body["messages"].to_string().contains(text)).cloned().collect()
}

/// What Hermes keeps in `profile`'s Bot Chat: its key, and its messages'
/// texts (each cut short).
fn bot_chat_of(c: &Container, profile: &str) -> serde_json::Value {
    hermes_python(
        c,
        &[],
        &format!(
            "import json\nfrom pathlib import Path\nfrom hermes_state import SessionDB\ndb = SessionDB(db_path=Path('/data/hermes/profiles/{profile}/state.db'), read_only=True)\ns = db.get_session_by_title('Bot Chat') or {{}}\ntexts = [str(m.get('content'))[:300] for m in db.get_messages_as_conversation(s['id'])] if s else []\nprint(json.dumps({{'key': s.get('session_key'), 'texts': texts}}))"
        ),
    )
}

/// Goal (Hermes' Bot Mode, docs/computers.md): each agent is a bot whose
/// own chat is its Bot Chat, so there its model has Hermes' teammate roster
/// (each teammate's name and role, and the gateway's own `@hermes`, said to
/// be no agent) and the `message_agent` tool; a message one bot sends
/// another is a hand-off in the other's own chat, posted as the sender, so
/// the person sees it and the other's answer is a turn of the bridge's
/// there (its Bot Chat, as that bot, metered to it); that answer comes back
/// to the first bot, which says it in its own chat.
/// Method: two agents, each with its own chat (`<label>-chat`); a first
/// message in each makes its session, which the image's hook titles as the
/// turn starts, and the image's keeper then holds as Hermes' live owner;
/// then juniper is asked to message maple (the scripted model's `dm:`).
#[tokio::test]
#[ignore = "needs Docker: cargo test -p fragment-bridge --test docker -- --ignored"]
async fn bots_message_each_other() {
    let tag = hermes_tag();
    build(&repo_dir(), "images/hermes/Dockerfile", &tag);
    let fake = Fake::start("0.0.0.0:0", &["juniper", "maple"]).await;
    let model = Model::start("0.0.0.0:0").await;
    // each its own chat, labelled for it, its suffix its own: the boot finds it
    let (jchat, mchat) = (fake.chat_named("juniper-chat--h6j7", &["juniper"]), fake.chat_named("maple-chat--z8w6", &["maple"]));
    let c = Container::run(&tag, fake.addr.port(), model.addr.port(), &[]);
    fake.until(180_000, "Hermes' bridge to follow both agents' chats", |w| w.live_sockets() >= 4).await;
    let yaml = c.exec_out(&["cat", "/data/hermes/profiles/maple--k3x9/profile.yaml"]);
    assert!(yaml.contains("display_name: \"maple\"\n") && yaml.contains("ui_meta:\n  hermes-bots:\n    title: \"maple\"\n"), "maple is a bot: {yaml}");
    // each bot's first message in its own chat makes its session there
    for (chat, agent) in [(&jchat, "juniper--k3x9"), (&mchat, "maple--k3x9")] {
        let said = fake.say(chat, &person("paul"), json!({ "text": "hello" }));
        let turn = fragment_bridge::records::turn_id(agent, chat, "chat", said["seq"].as_u64().unwrap());
        within(&fake, chat, &c, 180_000, "a bot's first reply", |w| w.bodies(chat, "work", "turn.end").iter().any(|e| e["turn"] == turn)).await;
    }
    // titled as their first turns started, so those turns had the roster
    assert_eq!(c.logs().matches("\"botmode.titled\"").count(), 2, "each bot's own chat titled Bot Chat once: {}", told(&fake, &jchat, &c));
    for agent in ["juniper--k3x9", "maple--k3x9"] {
        let first = calls_of(&model, agent, "hello").into_iter().next().unwrap_or_else(|| panic!("{agent}'s first turn"));
        assert!(first.body["messages"][0]["content"].to_string().contains("## Messaging other agents"), "{agent}'s first turn in its Bot Chat has the roster");
    }
    assert_eq!(bot_chat_of(&c, "maple--k3x9")["key"], "agent:maple--k3x9:relay:group:maple-chat--z8w6/maple--k3x9", "maple's Bot Chat is her own chat's session");
    let hooked = c.exec_out(&["readlink", "/data/hermes/profiles/maple--k3x9/hooks/fragment-bot-chat"]);
    assert_eq!(hooked.trim(), "/opt/fragment/hooks/fragment-bot-chat");

    // juniper, asked in her Bot Chat, messages maple
    let reply = |w: &support::fake::World, chat: &str, turn: &str| w.bodies(chat, "chat", "reply").into_iter().find(|r| r["turn"] == turn).and_then(|r| r["text"].as_str().map(str::to_string));
    let jturn = asked(&fake, &jchat, &c, "dm: maple: ping from juniper", |w, turn| reply(w, &jchat, turn).is_some()).await;
    let ack = fake.with(|w| reply(w, &jchat, &jturn)).unwrap_or_default();
    eprintln!("bots: juniper's message_agent said: {}", ack.chars().take(400).collect::<String>());
    assert!(ack.contains("queued"), "message_agent took juniper's message: {ack}");
    let asking = calls_of(&model, "juniper--k3x9", "dm: maple").into_iter().next().expect("juniper's model was asked");
    let system = asking.body["messages"][0]["content"].to_string();
    let offered = asking.body["tools"].to_string();
    assert!(offered.contains("\"message_agent\""), "juniper's Bot Chat has message_agent: {offered}");
    assert!(system.contains("## Messaging other agents") && system.contains("`@maple--k3x9`"), "and the roster, maple on it: {system}");
    assert!(system.contains("`@hermes` — this computer's gateway, not an agent"), "the gateway's own profile, said to be no agent: {system}");
    assert!(!system.contains("`@juniper--k3x9` —"), "juniper is not her own teammate: {system}");

    assert!(ack.contains("live Bot Chat owner"), "queued for maple's live owner, the keeper: {ack}");
    // the message is juniper's, in maple's own chat, for maple: a hand-off
    let t = Instant::now();
    let asked_maple = |w: &support::fake::World| w.records(&mchat, "chat").into_iter().find(|r| r["principal"] == "npub1juniper" && r["body"]["to"] == json!(["npub1maple"]));
    within(&fake, &mchat, &c, 120_000, "juniper's message in maple's chat", |w| asked_maple(w).is_some()).await;
    let record = fake.with(|w| asked_maple(w)).unwrap();
    eprintln!("bots: juniper's message in maple's chat {} ms after her turn: {}", t.elapsed().as_millis(), record["body"]);
    let text = record["body"]["text"].as_str().unwrap_or_default();
    assert!(text.starts_with("Message from 🤖 juniper (@juniper--k3x9): ") && text.ends_with("ping from juniper"), "as Hermes attributes it: {text}");
    // maple's answer is a turn of the bridge's there, as maple
    let mturn = fragment_bridge::records::turn_id("maple--k3x9", &mchat, "chat", record["seq"].as_u64().unwrap());
    within(&fake, &mchat, &c, 180_000, "maple's turn", |w| w.bodies(&mchat, "work", "turn.end").iter().any(|e| e["turn"] == mturn)).await;
    let manswer = fake.with(|w| reply(w, &mchat, &mturn)).unwrap_or_default();
    eprintln!("bots: maple answered in her chat: {manswer}");
    assert!(manswer.contains("ping from juniper"), "maple answered juniper's message: {manswer}");
    // and juniper says it in her own chat
    within(&fake, &jchat, &c, 180_000, "juniper to say maple's answer", |w| w.bodies(&jchat, "chat", "reply").iter().any(|r| r["text"].as_str().is_some_and(|t| t.contains("relayed:") && t.contains("ping from juniper")))).await;
    eprintln!("bots: juniper said maple's answer {} ms after her turn", t.elapsed().as_millis());
    let kept = bot_chat_of(&c, "maple--k3x9");
    let texts = kept["texts"].to_string();
    assert!(texts.contains("Message from") && texts.contains("ping from juniper"), "the message is in maple's Bot Chat: {kept}");
    let logs = c.logs();
    let delivered: Vec<&str> = logs.lines().filter(|l| l.contains("\"botmode.delivered\"")).collect();
    assert!(delivered.len() == 1 && delivered[0].contains("\"status\": \"settled\"") && delivered[0].contains("\"sender\": \"juniper--k3x9\""), "one delivery, settled: {delivered:?}");
    // no turn of Hermes' own answered it: maple's every model call that was handed it is the bridge's turn's
    let handed: Vec<_> = calls_of(&model, "maple--k3x9", "ping from juniper").into_iter().filter(|m| m.body["messages"].as_array().and_then(|ms| ms.iter().rev().find(|x| x["role"] == "user")).is_some_and(|u| u.to_string().contains("ping from juniper"))).collect();
    assert_eq!(handed.len(), 1, "one turn of maple's answered it");
}
