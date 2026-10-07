//! The bridge with its `goose` runtime against the fake fragment API and a
//! scripted goose on an in-process pipe (support/acp.rs): what goose is
//! told (each turn a fresh session, a mind's with its view and its MCP),
//! and what it says as the chat's records. The real goose, in its image,
//! is tests/docker.rs's.

mod support;

use std::sync::Arc;

use serde_json::{json, Value};

use fragment_bridge::records;
use fragment_bridge::runtime::goose::{Goose, GooseConfig, FRAMING, VIEW_DOC};
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
    }
}

/// A bridge whose runtime is goose, scripted.
fn start(fake: &Fake, name: &str) -> (support::Running, FakeGoose, std::path::PathBuf) {
    let dir = support::dir(name);
    let goose = FakeGoose::default();
    let runtime = Goose { config: config(&fake.url(), &dir), spawn: Arc::new(goose.clone()) };
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
/// mind's MCP (`fragment mcp <mind>`, as the agent), whose first prompt is
/// the framing, VIEW_DOC, the view, then the task; a chat's is the text
/// alone, with no MCP. One goose serves both, a session each; the answer is
/// the turn's one reply, as the agent.
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

    {
        let log = goose.log.lock().unwrap();
        assert_eq!(log.spawned, vec!["hands.paul"], "one goose for the agent");
        assert_eq!(log.prompts.len(), 2);
        assert_eq!(log.prompts[0], format!("{FRAMING}\n\n{VIEW_DOC}\n\n{VIEW}\n\nFind my notes\n\n(task k1, thread t_0123456789abcdef)"));
        assert_eq!(log.prompts[1], "hello goose", "a chat's turn is its text alone");
        let work = dir.join("work").display().to_string();
        assert_eq!(log.sessions[0]["cwd"], work.as_str());
        assert_eq!(log.sessions[0]["mcpServers"], json!([{ "name": "mind", "command": "/usr/local/bin/fragment", "args": ["mcp", mind], "env": [{ "name": "FRAGMENT_AS_AGENT", "value": "hands.paul" }, { "name": "FRAGMENT_FOR", "value": "id:paul" }, { "name": "FRAGMENT_API", "value": fake.url() }] }]));
        assert_eq!(log.sessions[1]["mcpServers"], json!([]), "a chat gets no mind");
        assert_ne!(log.sessions[0]["_meta"]["sessionTitle"], log.sessions[1]["_meta"]["sessionTitle"]);
        // the view was asked as the agent, once
        fake.with(|w| {
            let views: Vec<_> = w.requests.iter().filter(|r| r.0 == format!("POST /api/f/{mind}/ops/view")).collect();
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].2.as_deref(), Some("hands.paul"));
        });
    }
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

/// Goal: Stop cancels goose's session, and the turn ends `stopped` with no
/// reply; the chat's next message is answered.
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
        assert!(replies(w, &chat).is_empty(), "a stopped turn says nothing more");
    });
    assert_eq!(goose.log.lock().unwrap().cancels, vec!["s1"]);
    fake.say(&chat, &person("paul"), json!({ "text": "still there?" }));
    fake.until(WAIT, "the next answer", |w| replies(w, &chat) == vec!["scripted: still there?"]).await;
    bridge.stop().await;
}

/// Goal: a goose that dies ends the turn it ran as an error, and the
/// agent's next turn starts another goose, which answers it.
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
    });
    fake.say(&chat, &person("paul"), json!({ "text": "again" }));
    fake.until(WAIT, "the answer of a new goose", |w| replies(w, &chat) == vec!["scripted: again"]).await;
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
    let first = support::model::text_of(&calls[0].body["messages"].as_array().unwrap().iter().find(|m| m["role"] == "user").unwrap()["content"]);
    assert!(first.contains(FRAMING) && first.contains(VIEW), "the first prompt: {first}");

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
