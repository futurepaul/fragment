//! The bridge with its `goose` runtime against the fake fragment API and a
//! scripted goose on an in-process pipe (support/acp.rs): what goose is
//! told (each turn a fresh session, a mind's with its view and its MCP),
//! and what it says as the chat's records. The real goose, in its image,
//! is tests/docker.rs's.

mod support;

use std::sync::Arc;

use serde_json::{json, Value};

use fragment_bridge::records;
use fragment_bridge::runtime::goose::{Goose, GooseConfig, FRAMING, HANDS, VIEW_DOC};
use support::acp::FakeGoose;
use support::fake::{person, Fake, World};

const WAIT: u64 = 8_000;
const VIEW: &str = "<chat>\n0+1|user: I keep my notes in ~/notes\n</chat>";

fn config(api: &str, dir: &std::path::Path) -> GooseConfig {
    GooseConfig {
        command: "goose".into(),
        args: vec![],
        work: dir.join("work"),
        home: dir.join("work/home"),
        root: dir.join("goose"),
        api: api.to_string(),
        model: "http://127.0.0.1:9".into(),
        tier: "medium".into(),
        cli: Some("/usr/local/bin/fragment".into()),
        ca: None,
        desktop: None,
        skills: false,
    }
}

/// A bridge whose runtime is goose, scripted.
fn start(fake: &Fake, name: &str) -> (support::Running, FakeGoose, std::path::PathBuf) {
    start_with(fake, name, |_, _| {})
}

/// The same, its config changed by `change` (given its directory).
fn start_with(fake: &Fake, name: &str, change: impl FnOnce(&mut GooseConfig, &std::path::Path)) -> (support::Running, FakeGoose, std::path::PathBuf) {
    let dir = support::dir(name);
    let goose = FakeGoose::default();
    let mut cfg = config(&fake.url(), &dir);
    change(&mut cfg, &dir);
    let runtime = Goose { config: cfg, spawn: Arc::new(goose.clone()) };
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), Box::new(runtime));
    (bridge, goose, dir)
}

fn replies(w: &World, chat: &str) -> Vec<String> {
    w.bodies(chat, "chat", "reply").iter().map(|r| r["text"].as_str().unwrap_or("").to_string()).collect()
}

fn ends(w: &World, chat: &str) -> Vec<Value> {
    w.bodies(chat, "work", "turn.end")
}

/// Goal: a mind's turn is a fresh session in the work directory, with the
/// mind's MCP (`fragment mcp <mind>`, as the agent) and the framing and
/// VIEW_DOC under its system prompt, whose prompt is the view, then the
/// task; a chat's is the text alone, with neither. One goose serves both, a
/// session each, each closed after its turn; the answer is the turn's one
/// reply, as the agent.
#[tokio::test]
async fn a_mind_hands_goose_its_view_and_a_chat_its_text() {
    let fake = Fake::start("127.0.0.1:0", &["hands"]).await;
    let mind = fake.mind("mind", &["hands"], VIEW, "0+1|user: hi");
    let chat = fake.chat("talk", &["hands"]);
    let (bridge, goose, dir) = start(&fake, "goose-view");
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 3).await;

    let task = fake.say(&mind, &person("paul"), json!({ "text": "Find my notes\n\n(task k1, thread t_0123456789abcdef)", "to": ["id:hands"] }));
    fake.until(WAIT, "the mind's reply", |w| !replies(w, &mind).is_empty() && !ends(w, &mind).is_empty()).await;
    let turn = records::turn_id("hands.paul", &mind, "chat", task["seq"].as_u64().unwrap());
    fake.with(|w| {
        assert_eq!(replies(w, &mind), vec!["scripted: (task k1, thread t_0123456789abcdef)"]);
        let reply = w.records(&mind, "chat").into_iter().find(|r| r["body"]["turn"] == turn).expect("the reply names its turn");
        assert_eq!(reply["principal"], "id:hands");
        assert_eq!(ends(w, &mind)[0]["outcome"], "idle");
    });
    let said = fake.say(&chat, &person("paul"), json!({ "text": "hello goose" }));
    fake.until(WAIT, "the chat's reply", |w| !replies(w, &chat).is_empty() && !ends(w, &chat).is_empty()).await;
    fake.with(|w| assert_eq!(replies(w, &chat), vec!["scripted: hello goose"]));
    assert!(said["seq"].as_u64().is_some());
    // each turn's session closed; after each, one made ahead for the next
    // turn there (s2, the mind's, closed unused at the chat's turn; s4, the
    // chat's), its MCP servers started and its system prompt set
    support::until(WAIT, "the sessions closed, and one made ahead", || {
        let log = goose.log.lock().unwrap();
        log.closed.len() == 3 && log.sessions.len() == 4 && log.system.len() == 4
    })
    .await;

    {
        let log = goose.log.lock().unwrap();
        assert_eq!(log.spawned, vec!["hands.paul"], "one goose for the agent");
        assert_eq!(log.prompts, vec![vec![VIEW.to_string(), "Find my notes\n\n(task k1, thread t_0123456789abcdef)".to_string()], vec!["hello goose".to_string()]], "a mind's view, then the task; a chat's text alone");
        let (framed, hands) = (format!("{HANDS}\n\n{FRAMING}\n\n{VIEW_DOC}"), HANDS.to_string());
        let system = |s: &str, text: &str| json!({ "sessionId": s, "mode": "append", "key": "fragment", "text": text });
        assert_eq!(
            log.system,
            vec![system("s1", &framed), system("s2", &framed), system("s3", &hands), system("s4", &hands)],
            "every session is told of its computer and fragments; the mind's alone is framed as its subagent"
        );
        assert_eq!(log.prompted, vec!["s1", "s3"]);
        assert_eq!(log.closed, vec!["s1", "s2", "s3"]);
        let work = dir.join("work").display().to_string();
        assert_eq!(log.sessions[0]["cwd"], work.as_str());
        let minds = json!([{ "name": "mind", "command": "/usr/local/bin/fragment", "args": ["mcp", mind], "env": [{ "name": "FRAGMENT_AS_AGENT", "value": "hands.paul" }, { "name": "FRAGMENT_FOR", "value": "id:paul" }, { "name": "FRAGMENT_API", "value": fake.url() }] }]);
        assert_eq!(log.sessions[0]["mcpServers"], minds);
        assert_eq!(log.sessions[1]["mcpServers"], minds, "the mind's next session, made ahead, is the same");
        assert_eq!(log.sessions[2]["mcpServers"], json!([]), "a chat gets no mind");
        assert_ne!(log.sessions[0]["_meta"]["sessionTitle"], log.sessions[2]["_meta"]["sessionTitle"]);
        // the view was asked as the agent, once
        fake.with(|w| {
            let views: Vec<_> = w.requests.iter().filter(|r| r.0 == format!("POST /api/f/{mind}/ops/view")).collect();
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].2.as_deref(), Some("hands.paul"));
        });
    }
    // the chat's next turn takes the session made ahead for it: none made
    // at its turn, and the next one made ahead once it ends
    fake.say(&chat, &person("paul"), json!({ "text": "again" }));
    fake.until(WAIT, "the chat's second reply", |w| replies(w, &chat).len() == 2).await;
    support::until(WAIT, "its session closed, and the next made ahead", || {
        let log = goose.log.lock().unwrap();
        log.closed.len() == 4 && log.sessions.len() == 5
    })
    .await;
    {
        let log = goose.log.lock().unwrap();
        assert_eq!(log.prompted, vec!["s1", "s3", "s4"], "the session made ahead answered it");
        assert_eq!(log.closed, vec!["s1", "s2", "s3", "s4"]);
    }
    bridge.stop().await;
}

/// Goal: each turn installs its agent's skills where its goose reads them:
/// the platform skill (the computer's page, then the CLI's), goose's
/// `web-search` replaced, and the owner's managed set as the agent is
/// offered it, `${SKILL_DIR}` its own directory; a Hermes-only skill and a
/// provider's the agent has no key for left out. Replay: the next turn
/// fetches nothing. A skill gone from the fragment goes.
#[tokio::test]
async fn each_turn_installs_its_agents_skills() {
    let fake = Fake::start("127.0.0.1:0", &["hands"]).await;
    let chat = fake.chat("talk", &["hands"]);
    fake.skills(&[
        ("skills/research/arxiv-finite/SKILL.md", "---\nname: arxiv-finite\ndescription: papers\n---\npython3 ${SKILL_DIR}/scripts/s.py; web_extract it"),
        ("skills/research/arxiv-finite/scripts/s.py", "print(1)"),
        ("skills/software-development/subagent-driven-development-finite/SKILL.md", "---\nname: subagent-driven-development-finite\n---\ndelegate_task"),
        ("skills/productivity/linear-finite/SKILL.md", "---\nname: linear-finite\n---\nlinear"),
        ("fragment.json", "{}"),
    ]);
    let (bridge, _goose, dir) = start_with(&fake, "goose-skills", |cfg, dir| {
        let cli = dir.join("fragment");
        std::fs::write(&cli, "#!/bin/sh\nprintf -- '---\\nname: fragment\\ndescription: the cli\\n---\\n\\n# fragment\\n\\nMake apps.\\n'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        cfg.cli = Some(cli);
        cfg.skills = true;
    });
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "hello" }));
    fake.until(WAIT, "the reply", |w| !ends(w, &chat).is_empty()).await;
    let skills = dir.join("goose/hands.paul/config/skills");
    let platform = std::fs::read_to_string(skills.join("fragment/SKILL.md")).unwrap();
    assert!(platform.starts_with("---\nname: fragment\n") && platform.contains("# Your computer") && platform.contains("Make apps."), "{platform}");
    assert!(std::fs::read_to_string(skills.join("web-search/SKILL.md")).unwrap().contains("web_read"));
    let arxiv = skills.join("managed/research/arxiv-finite");
    assert_eq!(std::fs::read_to_string(arxiv.join("SKILL.md")).unwrap(), format!("---\nname: arxiv-finite\ndescription: papers\n---\npython3 {}/scripts/s.py; web_read it", arxiv.display()));
    assert_eq!(std::fs::read_to_string(arxiv.join("scripts/s.py")).unwrap(), "print(1)");
    assert!(!skills.join("managed/software-development").exists(), "a Hermes-only skill is left out");
    assert!(!skills.join("managed/productivity").exists(), "a provider's skill without its key is left out");
    let fetched = |w: &World| w.requests.iter().filter(|r| r.0.ends_with("/file")).count();
    assert_eq!(fake.with(|w| fetched(w)), 2);

    // replay: nothing fetched again; a skill gone goes
    fake.with(|w| w.fragments.get_mut("skills.paul").unwrap().files.remove("skills/research/arxiv-finite/scripts/s.py"));
    fake.say(&chat, &person("paul"), json!({ "text": "again" }));
    fake.until(WAIT, "the second reply", |w| ends(w, &chat).len() == 2).await;
    assert_eq!(fake.with(|w| fetched(w)), 2, "nothing fetched again");
    assert!(!arxiv.join("scripts").exists(), "what went is removed");
    assert!(arxiv.join("SKILL.md").exists());
    bridge.stop().await;
}

/// Goal: the words before a tool call are its step's, not a reply (their
/// draft stopped), the call is a step once its result is in, and the words
/// after it are the reply.
#[tokio::test]
async fn a_tool_call_is_a_step_with_its_words() {
    let fake = Fake::start("127.0.0.1:0", &["hands"]).await;
    let chat = fake.chat("talk", &["hands"]);
    let (bridge, _goose, _dir) = start(&fake, "goose-tool");
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "tool" }));
    fake.until(WAIT, "the reply", |w| !ends(w, &chat).is_empty()).await;
    fake.with(|w| {
        assert_eq!(replies(w, &chat), vec!["Found a.txt."]);
        let steps = w.bodies(&chat, "work", "turn.step");
        assert_eq!(steps.len(), 1);
        let s = &steps[0];
        assert_eq!((s["tool"].as_str(), s["args"].as_str(), s["ok"].as_bool(), s["excerpt"].as_str(), s["text"].as_str()), (Some("shell"), Some("ls"), Some(true), Some("a.txt"), Some("Let me look.")));
        // the step's words are never the reply's draft (drafts are paced:
        // the mapper's own test has each one)
        let drafts: Vec<Option<String>> = w.drafts.iter().map(|d| d.3.clone()).collect();
        assert!(drafts.iter().flatten().all(|d| !(d.contains("Let me look.") && d.contains("Found"))), "{drafts:?}");
        assert_eq!(drafts.last(), Some(&None), "the draft stopped at the end: {drafts:?}");
    });
    bridge.stop().await;
}

/// Goal: Stop cancels goose's session, and the turn ends `stopped`, its one
/// reply saying so and nothing of its drafts; the chat's next message is
/// answered.
#[tokio::test]
async fn a_stop_cancels_the_session() {
    let fake = Fake::start("127.0.0.1:0", &["hands"]).await;
    let chat = fake.chat("talk", &["hands"]);
    let (bridge, goose, _dir) = start(&fake, "goose-stop");
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "slow" }));
    fake.until(WAIT, "a draft", |w| w.drafts.iter().any(|d| d.3.as_deref().is_some_and(|t| t.starts_with("more")))).await;
    fake.say(&chat, &person("paul"), json!({ "kind": "stop" }));
    fake.until(WAIT, "the stopped end", |w| !ends(w, &chat).is_empty()).await;
    fake.with(|w| {
        assert_eq!(ends(w, &chat)[0]["outcome"], "stopped");
        assert_eq!(replies(w, &chat), vec!["(ended: stopped: its asker stopped it)"], "a stopped turn's one reply");
    });
    assert_eq!(goose.log.lock().unwrap().cancels, vec!["s1"]);
    fake.say(&chat, &person("paul"), json!({ "text": "still there?" }));
    fake.until(WAIT, "the next answer", |w| replies(w, &chat).len() == 2).await;
    fake.with(|w| assert_eq!(replies(w, &chat)[1], "scripted: still there?"));
    bridge.stop().await;
}

/// Goal: a goose that dies ends the turn it ran as an error, its one reply
/// saying why, and the agent's next turn starts another goose, which
/// answers it.
#[tokio::test]
async fn a_goose_that_dies_is_started_again() {
    let fake = Fake::start("127.0.0.1:0", &["hands"]).await;
    let chat = fake.chat("talk", &["hands"]);
    let (bridge, goose, _dir) = start(&fake, "goose-dies");
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "die" }));
    fake.until(WAIT, "the failed end", |w| !ends(w, &chat).is_empty()).await;
    fake.with(|w| {
        let end = &ends(w, &chat)[0];
        assert_eq!(end["outcome"], "error");
        assert_eq!(end["error"], "goose: it stopped");
        assert_eq!(replies(w, &chat), vec!["(ended: error: goose: it stopped)"]);
    });
    fake.say(&chat, &person("paul"), json!({ "text": "again" }));
    fake.until(WAIT, "the answer of a new goose", |w| replies(w, &chat).len() == 2).await;
    fake.with(|w| assert_eq!(replies(w, &chat)[1], "scripted: again"));
    assert_eq!(goose.log.lock().unwrap().spawned, vec!["hands.paul", "hands.paul"]);
    bridge.stop().await;
}

/// Goal: goose asks nothing a person answers: a permission it asks anyway
/// is refused at once, and its turn goes on to its answer.
#[tokio::test]
async fn a_permission_goose_asks_is_refused() {
    let fake = Fake::start("127.0.0.1:0", &["hands"]).await;
    let chat = fake.chat("talk", &["hands"]);
    let (bridge, goose, _dir) = start(&fake, "goose-permission");
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "permission" }));
    fake.until(WAIT, "the answer", |w| !ends(w, &chat).is_empty()).await;
    fake.with(|w| {
        assert_eq!(replies(w, &chat), vec!["scripted: asked"]);
        assert!(w.bodies(&chat, "work", "turn.prompt").is_empty(), "no card");
    });
    assert_eq!(goose.log.lock().unwrap().permissions, vec![json!({ "outcome": { "outcome": "selected", "optionId": "reject_once" } })]);
    bridge.stop().await;
}

/// Goal: the real goose (`FRAGMENT_GOOSE_BIN`, a v1.53.0 binary for this
/// host), on this host, with the scripted model: a mind's task runs its
/// shell and is answered, its step recorded; each model call names the
/// agent and the tier at the intercept's path; the first carries the
/// framing and the view; with `FRAGMENT_CLI_BIN` (the fragment CLI), the
/// mind's zoom is a tool goose calls through `fragment mcp`, as the agent.
/// Run: `FRAGMENT_GOOSE_BIN=… cargo test -p fragment-bridge --test goose -- --ignored`.
#[tokio::test]
#[ignore = "needs a goose binary: FRAGMENT_GOOSE_BIN"]
async fn the_real_goose_on_this_host() {
    let bin = std::env::var("FRAGMENT_GOOSE_BIN").expect("FRAGMENT_GOOSE_BIN names goose");
    let fake = Fake::start("127.0.0.1:0", &["hands"]).await;
    let model = support::model::Model::start("127.0.0.1:0").await;
    let mind = fake.mind("mind", &["hands"], VIEW, "0+1|user: I keep my notes in ~/notes");
    let dir = support::dir("goose-real");
    let mut cfg = config(&fake.url(), &dir);
    cfg.command = bin.into();
    cfg.args = ["acp", "--with-builtin", "developer"].map(String::from).to_vec();
    cfg.model = format!("http://{}", model.addr);
    cfg.cli = std::env::var("FRAGMENT_CLI_BIN").ok().map(Into::into);
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), Box::new(Goose::new(cfg.clone())));
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;

    fake.say(&mind, &person("paul"), json!({ "text": "Check the shell.\n\nrun: echo tool-ran", "to": ["id:hands"] }));
    fake.until(60_000, "goose's answer", |w| !ends(w, &mind).is_empty()).await;
    fake.with(|w| {
        assert_eq!(ends(w, &mind)[0]["outcome"], "idle", "{:?}", ends(w, &mind));
        assert_eq!(replies(w, &mind), vec!["scripted: the tool said: tool-ran"]);
        let steps = w.bodies(&mind, "work", "turn.step");
        assert!(steps.len() == 1 && steps[0]["tool"] == "shell" && steps[0]["args"] == "echo tool-ran" && steps[0]["ok"] == true, "{steps:?}");
    });
    // goose also lists the provider's models (`GET /v1/models`) as it makes
    // a session: the intercept answers 404, unmetered (docs/computers.md,
    // "Models"), as the scripted model does
    let all = model.calls.lock().unwrap().clone();
    eprintln!("model calls: {:?}", all.iter().map(|c| c.path.as_str()).collect::<Vec<_>>());
    let calls: Vec<_> = all.iter().filter(|c| c.path != "/v1/models").cloned().collect();
    assert!(!calls.is_empty());
    for c in &calls {
        assert_eq!((c.path.as_str(), c.model.as_str(), c.agent.as_deref()), ("/v1/chat/completions", "medium", Some("hands.paul")), "every call is the agent's, at the intercept's path");
    }
    let (system, user) = (support::model::texts(&calls[0].body, "system"), support::model::texts(&calls[0].body, "user"));
    assert!(system.contains(&format!("{FRAMING}\n\n{VIEW_DOC}")), "the framing in the system prompt: {system}");
    assert!(user.contains(VIEW) && !user.contains(FRAMING), "the view in the prompt: {user}");
    let tools = support::model::tools(&calls[0].body);
    eprintln!("tools: {tools:?}");
    let mine = |t: &String| t.starts_with("developer__") || t.starts_with("mind__") || ["shell", "write", "edit", "tree", "read_image"].contains(&t.as_str());
    assert!(tools.iter().all(mine), "developer's tools and the mind's alone: {tools:?}");

    if cfg.cli.is_some() {
        fake.say(&mind, &person("paul"), json!({ "text": "zoom: 0 1", "to": ["id:hands"] }));
        fake.until(60_000, "the zoom's answer", |w| ends(w, &mind).len() == 2).await;
        fake.with(|w| {
            assert_eq!(replies(w, &mind)[1], "scripted: the tool said: 0+1|user: I keep my notes in ~/notes");
            let zooms: Vec<_> = w.requests.iter().filter(|r| r.0 == format!("POST /api/f/{mind}/ops/zoom")).collect();
            assert!(zooms.len() == 1 && zooms[0].2.as_deref() == Some("hands.paul") && !zooms[0].3, "as the agent, unsigned: {zooms:?}");
        });
    }
    bridge.stop().await;
}
