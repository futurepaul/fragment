//! The bridge against the fake fragment API, with the `script` runtime: the
//! platform's half of every behavior, with no agent runtime at all (the
//! stub image's shape). Method: start a fake and a bridge in process, say
//! things in a chat as people do, and read the chat's records, drafts, and
//! keepalive log back. Restarts stop the bridge and start it again over the
//! same state directory.

mod support;

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use fragment_bridge::records;
use fragment_bridge::runtime::script::{Script, ScriptConfig};
use support::fake::{person, Fake, World};

const WAIT: u64 = 8_000;

fn turn_of(agent: &str, chat: &str, seq: u64) -> String {
    records::turn_id(&format!("{agent}.paul"), chat, "chat", seq)
}

fn seq(r: &Value) -> u64 {
    r["seq"].as_u64().expect("a record's seq")
}

/// The bridge is following: the agent's tasks and each chat are live.
async fn following(fake: &Fake, sockets: usize) {
    fake.until(WAIT, "the bridge to follow its channels", |w| w.live_sockets() >= sockets).await;
}

fn replies(w: &World, chat: &str) -> Vec<Value> {
    w.bodies(chat, "chat", "reply")
}

/// Goal: a person's message is answered once, as the agent, naming its
/// turn; the turn starts and ends on `work`; the computer is kept awake for
/// the turn and let go after.
#[tokio::test]
async fn a_reply() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("a-reply");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 2).await;
    let said = fake.say(&chat, &person("paul"), json!({ "text": "hi" }));
    let turn = turn_of("juniper", &chat, seq(&said));
    fake.until(WAIT, "a reply", |w| !replies(w, &chat).is_empty() && !w.bodies(&chat, "work", "turn.end").is_empty()).await;
    fake.with(|w| {
        let r = replies(w, &chat);
        assert_eq!(r, vec![json!({ "text": "echo: [paul] hi", "turn": turn })]);
        let reply_record = w.records(&chat, "chat").into_iter().find(|x| x["body"]["turn"] == turn).expect("the reply");
        assert_eq!(reply_record["principal"], "id:juniper", "the agent answers, never its owner");
        let work: Vec<String> = w.records(&chat, "work").iter().map(|r| r["body"]["kind"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(work, vec!["turn.start", "turn.end"]);
        assert_eq!(w.bodies(&chat, "work", "turn.start")[0]["asker"], "id:paul");
        assert_eq!(w.bodies(&chat, "work", "turn.end")[0]["outcome"], "idle");
    });
    fake.until(WAIT, "the keepalive held, then let go", |w| w.keepalive_log.first() == Some(&true) && w.keepalive_open == 0).await;
    bridge.stop().await;
}

/// Goal: a reply streams as drafts, then posts as a record with the same
/// turn, after which its draft is stopped.
#[tokio::test]
async fn streaming_drafts_then_the_final() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("drafts");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 2).await;
    let said = fake.say(&chat, &person("paul"), json!({ "text": "tell me a long story please" }));
    let turn = turn_of("juniper", &chat, seq(&said));
    fake.until(WAIT, "the reply's draft stopped", |w| w.drafts.iter().any(|d| d.2 == turn && d.3.is_none()) && !replies(w, &chat).is_empty()).await;
    fake.with(|w| {
        let mine: Vec<&String> = w.log.iter().filter(|l| l.contains(&turn) || l.starts_with(&format!("record {chat} chat"))).collect();
        let first_draft = w.log.iter().position(|l| l == &format!("draft {chat} {turn} text")).expect("a draft with text");
        let reply_seq = w.records(&chat, "chat").iter().find(|r| r["body"]["turn"] == turn).map(seq).expect("the reply");
        let reply_at = w.log.iter().position(|l| l == &format!("record {chat} chat {reply_seq}")).expect("the reply in the log");
        let stopped = w.log.iter().rposition(|l| l == &format!("draft {chat} {turn} null")).expect("the draft stopped");
        assert!(first_draft < reply_at && reply_at < stopped, "drafts, then the record, then the draft stops: {mine:?}");
        let texts: Vec<&str> = w.drafts.iter().filter(|d| d.2 == turn).filter_map(|d| d.3.as_deref()).collect();
        assert!(texts.iter().all(|t| "echo: [paul] tell me a long story please".starts_with(t)), "each draft is the reply so far: {texts:?}");
    });
    bridge.stop().await;
}

/// Goal: a tool call is a step on `work`, before the reply.
#[tokio::test]
async fn tool_steps() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("steps");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 2).await;
    let said = fake.say(&chat, &person("paul"), json!({ "text": "use a tool" }));
    let turn = turn_of("juniper", &chat, seq(&said));
    fake.until(WAIT, "the turn's end", |w| !w.bodies(&chat, "work", "turn.end").is_empty()).await;
    fake.with(|w| {
        let steps = w.bodies(&chat, "work", "turn.step");
        assert_eq!(steps.len(), 1);
        assert_eq!((steps[0]["tool"].as_str(), steps[0]["step"].as_u64(), steps[0]["ok"].as_bool(), steps[0]["turn"].as_str()), (Some("search"), Some(1), Some(true), Some(turn.as_str())));
        assert_eq!(steps[0]["text"], "Let me look.");
        let ids: Vec<String> = w.records(&chat, "work").iter().map(|r| r["body"]["kind"].as_str().unwrap_or("").to_string()).collect();
        assert_eq!(ids, vec!["turn.start", "turn.step", "turn.end"]);
    });
    bridge.stop().await;
}

/// Goal (decision 42): an approval is a card for the agent's owner; while
/// it waits the keepalive is dropped; the owner's answer resumes the turn,
/// a second answer is ignored, and someone else's never counts.
#[tokio::test]
async fn an_approval_answered() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    fake.add_member(&chat, &person("skyler"), "editor");
    let dir = support::dir("approval");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "do something risky" }));
    fake.until(WAIT, "the prompt", |w| !w.bodies(&chat, "work", "turn.prompt").is_empty()).await;
    let prompt = fake.with(|w| w.bodies(&chat, "work", "turn.prompt")[0].clone());
    assert_eq!(prompt["asks"], "id:paul");
    assert_eq!(prompt["options"].as_array().map(Vec::len), Some(2));
    let id = prompt["prompt"].as_str().unwrap().to_string();
    fake.until(WAIT, "the keepalive dropped while it waits", |w| w.keepalive_open == 0).await;

    fake.say(&chat, &person("skyler"), json!({ "kind": "prompt_response", "prompt": id, "option": "deny" }));
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.with(|w| assert!(w.bodies(&chat, "work", "turn.prompt.closed").is_empty(), "only the owner answers"));
    fake.say(&chat, &person("paul"), json!({ "kind": "prompt_response", "prompt": id, "option": "once" }));
    fake.say(&chat, &person("paul"), json!({ "kind": "prompt_response", "prompt": id, "option": "deny" }));
    fake.until(WAIT, "the reply", |w| !replies(w, &chat).is_empty()).await;
    fake.with(|w| {
        assert!(replies(w, &chat)[0]["text"].as_str().unwrap().ends_with("(approved)"), "the first answer won");
        let closed = w.bodies(&chat, "work", "turn.prompt.closed");
        assert_eq!(closed.len(), 1);
        assert_eq!((closed[0]["outcome"].as_str(), closed[0]["option"].as_str(), closed[0]["by"].as_str()), (Some("answered"), Some("once"), Some("id:paul")));
    });
    fake.until(WAIT, "held, dropped while waiting, held again, dropped", |w| w.keepalive_log == [true, false, true, false]).await;
    bridge.stop().await;
}

/// Goal: an unanswered prompt expires: the card says so, the turn goes on
/// without the approval, and a late answer changes nothing.
#[tokio::test]
async fn an_approval_expired() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("expired");
    let settings = fragment_bridge::engine::Settings { prompt_ttl_ms: 400, ..support::settings() };
    let bridge = support::start(support::config(&fake.url(), &dir, settings), support::script());
    following(&fake, 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "risky business" }));
    fake.until(WAIT, "the reply", |w| !replies(w, &chat).is_empty()).await;
    let id = fake.with(|w| {
        let closed = w.bodies(&chat, "work", "turn.prompt.closed");
        assert_eq!(closed[0]["outcome"], "expired");
        assert!(replies(w, &chat)[0]["text"].as_str().unwrap().ends_with("(not approved)"));
        closed[0]["prompt"].as_str().unwrap().to_string()
    });
    fake.say(&chat, &person("paul"), json!({ "kind": "prompt_response", "prompt": id, "option": "once" }));
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.with(|w| assert_eq!(w.bodies(&chat, "work", "turn.prompt.closed").len(), 1, "a late answer closes nothing"));
    bridge.stop().await;
}

/// Goal: only the turn's asker stops it; a Stop from anyone else is
/// ignored and the turn answers.
#[tokio::test]
async fn stop_from_the_starter_and_from_someone_else() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    fake.add_member(&chat, &person("skyler"), "editor");
    let dir = support::dir("stop");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 2).await;

    let one = fake.say(&chat, &person("paul"), json!({ "text": "slow one" }));
    let t1 = turn_of("juniper", &chat, seq(&one));
    fake.until(WAIT, "the first turn to run", |w| w.drafts.iter().any(|d| d.2 == t1)).await;
    fake.say(&chat, &person("skyler"), json!({ "kind": "stop", "turn": t1 }));
    fake.until(WAIT, "the first turn's end", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t1)).await;
    fake.with(|w| {
        let end = w.bodies(&chat, "work", "turn.end").into_iter().find(|e| e["turn"] == t1).unwrap();
        assert_eq!(end["outcome"], "idle", "skyler's Stop was ignored");
        assert!(replies(w, &chat).iter().any(|r| r["turn"] == t1));
    });

    let two = fake.say(&chat, &person("paul"), json!({ "text": "slow two" }));
    let t2 = turn_of("juniper", &chat, seq(&two));
    fake.until(WAIT, "the second turn to run", |w| w.drafts.iter().any(|d| d.2 == t2)).await;
    fake.say(&chat, &person("paul"), json!({ "kind": "stop", "turn": t2 }));
    // its end, then (in the same lane, after it) its draft stopped
    fake.until(WAIT, "the second turn's end and its draft stopped", |w| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == t2) && w.drafts.iter().any(|d| d.2 == t2 && d.3.is_none())).await;
    fake.with(|w| {
        let end = w.bodies(&chat, "work", "turn.end").into_iter().find(|e| e["turn"] == t2).unwrap();
        assert_eq!(end["outcome"], "stopped");
        assert!(!replies(w, &chat).iter().any(|r| r["turn"] == t2), "a stopped turn posts no reply");
    });
    bridge.stop().await;
}

/// Goal (lesson 2): a restart catches up on what was said while it was
/// down and starts nothing twice; a turn running when it stopped is ended,
/// never run again.
#[tokio::test]
async fn catch_up_after_a_restart_without_a_duplicate() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("restart");
    let cfg = support::config(&fake.url(), &dir, support::settings());
    let bridge = support::start(cfg.clone(), support::script());
    following(&fake, 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "one" }));
    fake.until(WAIT, "one's reply", |w| replies(w, &chat).len() == 1).await;

    // a turn running when the bridge stops
    let slow = fake.say(&chat, &person("paul"), json!({ "text": "slow" }));
    let ts = turn_of("juniper", &chat, seq(&slow));
    fake.until(WAIT, "the slow turn to run", |w| w.drafts.iter().any(|d| d.2 == ts)).await;
    let took = bridge.stop().await;
    assert!(took < Duration::from_secs(3), "stopped in {took:?}");
    fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;

    // said while it was down
    let two = fake.say(&chat, &person("paul"), json!({ "text": "two" }));
    let bridge = support::start(cfg.clone(), support::script());
    fake.until(WAIT, "two's reply", |w| replies(w, &chat).iter().any(|r| r["turn"] == turn_of("juniper", &chat, seq(&two)))).await;
    fake.with(|w| {
        let end = w.bodies(&chat, "work", "turn.end").into_iter().find(|e| e["turn"] == ts).expect("the slow turn ended");
        assert_eq!(end["outcome"], "error");
        assert!(!replies(w, &chat).iter().any(|r| r["turn"] == ts), "never run again");
        assert_eq!(replies(w, &chat).len(), 2, "one and two, once each");
        assert_eq!(w.bodies(&chat, "work", "turn.start").len(), 3, "three turns, each started once");
    });
    // and a second restart, with nothing new, starts nothing
    bridge.stop().await;
    let bridge = support::start(cfg, support::script());
    following(&fake, 2).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    fake.with(|w| {
        assert_eq!(w.bodies(&chat, "work", "turn.start").len(), 3);
        let subs = &w.fragments[&chat].subscriptions;
        assert_eq!(subs.len(), 1, "one wake subscription, however many boots: {subs:?}");
        assert_eq!(subs[0]["wake"], true);
    });
    bridge.stop().await;
}

/// Goal (lesson 5): when the API drops every socket (a deploy) or goes
/// down a while, the bridge comes back on its own and answers what was
/// said meanwhile, once.
#[tokio::test]
async fn reconnect_after_the_api_drops() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("reconnect");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 2).await;
    fake.drop_sockets();
    fake.with(|w| w.down = true);
    fake.say(&chat, &person("paul"), json!({ "text": "while down" }));
    tokio::time::sleep(Duration::from_millis(700)).await;
    fake.with(|w| w.down = false);
    let started = Instant::now();
    fake.until(WAIT * 3, "the reply after it came back", |w| replies(w, &chat).len() == 1).await;
    assert!(started.elapsed() < Duration::from_secs(20));
    fake.say(&chat, &person("paul"), json!({ "text": "after" }));
    fake.until(WAIT, "a reply to after", |w| replies(w, &chat).len() == 2).await;
    fake.with(|w| assert_eq!(w.bodies(&chat, "work", "turn.start").len(), 2));
    // a post the platform fails is tried again, as the same record
    fake.with(|w| w.fail_posts = 2);
    fake.say(&chat, &person("paul"), json!({ "text": "flaky" }));
    fake.until(WAIT * 2, "a reply through failed posts", |w| replies(w, &chat).len() == 3).await;
    bridge.stop().await;
}

/// Goal: two agents on one computer, each in its own chat, answer as
/// themselves.
#[tokio::test]
async fn two_agents_on_one_computer() {
    let fake = Fake::start("127.0.0.1:0", &["juniper", "rowan"]).await;
    let talk = fake.chat("talk", &["juniper"]);
    let notes = fake.chat("notes", &["rowan"]);
    let dir = support::dir("two-agents");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 4).await;
    fake.say(&talk, &person("paul"), json!({ "text": "slow a" }));
    fake.say(&notes, &person("paul"), json!({ "text": "slow b" }));
    fake.until(WAIT, "both replies", |w| replies(w, &talk).len() == 1 && replies(w, &notes).len() == 1).await;
    fake.with(|w| {
        let by = |chat: &str| w.records(chat, "chat").into_iter().find(|r| r["body"].get("turn").is_some()).unwrap()["principal"].clone();
        assert_eq!((by(&talk), by(&notes)), (json!("id:juniper"), json!("id:rowan")));
    });
    bridge.stop().await;
}

/// The image's ready file (ready.rs), written whole and renamed into place
/// as an image's host writes it.
fn write_ready(file: &std::path::Path, agents: &[&str]) {
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, json!({ "agents": agents }).to_string()).unwrap();
    std::fs::rename(&tmp, file).unwrap();
}

/// Goal (docs/computers.md: a computer's agents may change while it runs):
/// with a ready file, the bridge runs only the agents the image made
/// ready. One assigned while the bridge runs is not followed, however often
/// the computer is read, until the file names it, and then within a tick
/// or two; taken out of the file, it is followed no more. Another agent's
/// turn runs on, whole, through both.
#[tokio::test]
async fn agents_the_image_makes_ready() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let talk = fake.chat("talk", &["juniper"]);
    let dir = support::dir("ready-file");
    let file = dir.join("agents.json");
    write_ready(&file, &["juniper.paul"]);
    let mut cfg = support::config(&fake.url(), &dir, support::settings());
    cfg.agents_file = Some(file.clone());
    // a pace that makes `slow` take five seconds: a turn that runs through the change
    let script = Box::new(Script { config: ScriptConfig { pace: Duration::from_millis(250), scratch: std::env::temp_dir().join("bridge-test-script") } });
    let bridge = support::start(cfg, script);
    following(&fake, 2).await;
    let slow = fake.say(&talk, &person("paul"), json!({ "text": "slow, while maple arrives" }));
    let slow_turn = turn_of("juniper", &talk, seq(&slow));
    fake.until(WAIT, "juniper's turn running", |w| w.keepalive_open == 1).await;

    fake.add_agent("maple");
    let grove = fake.chat("grove", &["maple"]);
    fake.say(&grove, &person("paul"), json!({ "text": "hello maple" }));
    tokio::time::sleep(Duration::from_millis(2_500)).await;
    fake.with(|w| {
        assert!(replies(w, &grove).is_empty(), "not ready, maple is not run: {:?}", replies(w, &grove));
        assert_eq!(w.live_sockets(), 2, "nor followed");
    });
    let t = Instant::now();
    write_ready(&file, &["juniper.paul", "maple.paul"]);
    fake.until(WAIT, "maple's reply, once the image made it ready", |w| replies(w, &grove).len() == 1).await;
    let took = t.elapsed();
    eprintln!("ready file to maple's reply: {} ms", took.as_millis());
    assert!(took < Duration::from_secs(4), "within a tick or two of the file: {took:?}");
    fake.until(WAIT, "juniper's slow turn's end", |w| w.bodies(&talk, "work", "turn.end").iter().any(|e| e["turn"] == slow_turn)).await;
    fake.with(|w| {
        let end = w.bodies(&talk, "work", "turn.end").into_iter().find(|e| e["turn"] == slow_turn).unwrap();
        assert_eq!(end["outcome"], "idle", "juniper's turn ran on through the change: {end}");
        assert!(replies(w, &talk).iter().any(|r| r["turn"] == slow_turn.as_str() && r["text"].as_str().is_some_and(|t| t.contains("while maple arrives"))));
        assert_eq!(w.records(&grove, "chat").iter().filter(|r| r["principal"] == "id:maple").count(), 1, "maple answered as itself");
    });

    // Replay: the file written again with the same agents changes nothing.
    write_ready(&file, &["juniper.paul", "maple.paul"]);
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    fake.with(|w| assert_eq!(w.live_sockets(), 4, "juniper's and maple's tasks and chats, once each"));

    // Out of the file: maple is followed no more, and its chat goes unanswered.
    write_ready(&file, &["juniper.paul"]);
    fake.until(WAIT, "maple's follows dropped", |w| w.live_sockets() == 2).await;
    fake.say(&grove, &person("paul"), json!({ "text": "still there?" }));
    tokio::time::sleep(Duration::from_millis(2_000)).await;
    fake.with(|w| assert_eq!(replies(w, &grove).len(), 1, "no longer run: {:?}", replies(w, &grove)));
    bridge.stop().await;
}

/// Goal: an agent of the computer added to a chat where another of its
/// agents leads answers its `@mention` at once, and the lead does not: the
/// `joined` notice makes the lead's view of the chat (read moments before,
/// within its 30 s life) stale, so it is read again before the mention.
#[tokio::test]
async fn an_agent_that_just_joined_answers_its_mention() {
    let fake = Fake::start("127.0.0.1:0", &["juniper", "rowan"]).await;
    let talk = fake.chat("talk", &["juniper"]);
    let dir = support::dir("joined-mention");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 3).await;
    fake.say(&talk, &person("paul"), json!({ "text": "hello juniper" }));
    fake.until(WAIT, "the lead's reply, its view of the chat read", |w| replies(w, &talk).len() == 1).await;
    fake.join(&talk, "rowan");
    fake.until(WAIT, "rowan following the chat it joined", |w| w.live_sockets() == 4).await;
    fake.say(&talk, &person("paul"), json!({ "text": "@rowan welcome" }));
    fake.until(WAIT, "rowan's reply", |w| replies(w, &talk).len() == 2).await;
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    fake.with(|w| {
        let by: Vec<Value> = w.records(&talk, "chat").into_iter().filter(|r| r["body"].get("turn").is_some()).map(|r| r["principal"].clone()).collect();
        assert_eq!(by, vec![json!("id:juniper"), json!("id:rowan")], "rowan answers its mention, the lead does not");
    });
    bridge.stop().await;
}

/// Goal (decision 8): in a group, the lead (the first agent added) answers
/// a message naming no one; an `@mention` picks who answers; a reply that
/// names another agent hands off to it.
#[tokio::test]
async fn a_group_mention_picks_the_agent() {
    let fake = Fake::start("127.0.0.1:0", &["juniper", "rowan"]).await;
    let group = fake.chat("group", &["juniper", "rowan"]);
    let dir = support::dir("group");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 4).await;
    fake.say(&group, &person("paul"), json!({ "text": "hello everyone" }));
    fake.until(WAIT, "the lead's reply", |w| replies(w, &group).len() == 1).await;
    fake.say(&group, &person("paul"), json!({ "text": "@rowan your turn" }));
    fake.until(WAIT, "rowan's reply", |w| replies(w, &group).len() == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.with(|w| {
        let who: Vec<String> = w.records(&group, "chat").into_iter().filter(|r| r["body"].get("turn").is_some()).map(|r| r["principal"].as_str().unwrap().to_string()).collect();
        assert_eq!(who, vec!["id:juniper", "id:rowan"]);
    });
    // juniper's reply names rowan (the script echoes what it was told)
    fake.say(&group, &person("paul"), json!({ "text": "juniper, ask @rowan too", "to": ["id:juniper"] }));
    fake.until(WAIT, "juniper's hand-off, and rowan's answer to it", |w| replies(w, &group).len() == 4).await;
    fake.with(|w| {
        let r = replies(w, &group);
        assert_eq!(r[2]["to"], json!(["id:rowan"]), "juniper's reply hands off");
        assert_eq!(r[2]["hop"], 1);
        assert!(r[3]["text"].as_str().unwrap().starts_with("echo: [someone]"), "rowan answers juniper's message");
    });
    bridge.stop().await;
}

/// Goal: attachments both ways: a message's files reach the runtime, and a
/// reply's files are uploaded as the chat's blobs and listed on it.
#[tokio::test]
async fn attachments_both_ways() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let bytes = bytes::Bytes::from_static(b"hello file");
    let sha = records::hex(&<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    fake.with(|w| w.fragments.get_mut(&chat).unwrap().blobs.insert(sha.clone(), ("text/plain".into(), bytes.clone())));
    let dir = support::dir("attachments");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "see this", "attachments": [{ "sha256": sha, "size": bytes.len(), "type": "text/plain", "name": "a.txt" }] }));
    fake.until(WAIT, "its reply", |w| replies(w, &chat).len() == 1).await;
    fake.with(|w| assert!(replies(w, &chat)[0]["text"].as_str().unwrap().ends_with("[got 1: a.txt]")));
    fake.say(&chat, &person("paul"), json!({ "text": "draw me something" }));
    fake.until(WAIT, "a reply with a file", |w| replies(w, &chat).len() == 2).await;
    fake.with(|w| {
        let r = &replies(w, &chat)[1];
        let a = &r["attachments"][0];
        assert_eq!((a["name"].as_str(), a["type"].as_str()), (Some("drawing.txt"), Some("text/plain")));
        assert!(w.fragments[&chat].blobs.contains_key(a["sha256"].as_str().unwrap()), "its bytes are the chat's blob");
    });
    bridge.stop().await;
}

/// Goal (decision 38): a routine (a record on the agent's `tasks`) is a
/// turn in its chat, asked as its owner.
#[tokio::test]
async fn a_routine_is_a_turn() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("routine");
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::script());
    following(&fake, 2).await;
    fake.with(|w| w.append("juniper.paul", "tasks", "id:paul", json!({ "kind": "routine", "text": "water the plants", "chat": chat })));
    fake.until(WAIT, "the routine's reply", |w| replies(w, &chat).len() == 1).await;
    fake.with(|w| {
        assert_eq!(replies(w, &chat)[0]["text"], "echo: [your routine] water the plants");
        assert_eq!(w.bodies(&chat, "work", "turn.start")[0]["cause"]["channel"], "tasks");
        assert_eq!(w.fragments["juniper.paul"].subscriptions.len(), 1, "the tasks channel wakes it too");
    });
    bridge.stop().await;
}

/// Goal: SIGTERM means stop now: the real binary exits within 5 s of it,
/// even mid-turn, with its keepalive and sockets closed.
#[tokio::test]
async fn sigterm_exits_within_five_seconds() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("sigterm");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_fragment-bridge"))
        .arg("run")
        .env("FRAGMENT_API", fake.url())
        .env("BRIDGE_RUNTIME", "script")
        .env("BRIDGE_SCRIPT_PACE_MS", "200")
        .env("BRIDGE_STATE_DIR", dir.join("bridge"))
        .env("BRIDGE_MEDIA_DIR", dir.join("media"))
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the bridge binary runs");
    following(&fake, 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "slow" }));
    fake.until(WAIT, "a turn running", |w| w.keepalive_open == 1).await;
    let t = Instant::now();
    let killed = std::process::Command::new("kill").arg("-TERM").arg(child.id().to_string()).status().expect("kill runs");
    assert!(killed.success());
    // bounded: 5 s of polls
    let status = loop {
        if let Some(s) = child.try_wait().expect("waits") {
            break s;
        }
        assert!(t.elapsed() < Duration::from_secs(5), "still running {:?} after SIGTERM", t.elapsed());
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    let took = t.elapsed();
    assert!(status.success(), "a clean exit: {status:?}");
    assert!(took < Duration::from_secs(5), "exited in {took:?}");
    eprintln!("sigterm: exited in {} ms", took.as_millis());
    fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0 && w.keepalive_open == 0).await;
}

/// Invalid: a corrupt state file is refused, not repaired, and not run on.
#[tokio::test]
async fn a_corrupt_state_is_refused() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let dir = support::dir("corrupt");
    std::fs::create_dir_all(dir.join("bridge")).unwrap();
    std::fs::write(dir.join("bridge/state.json"), b"{\"version\": 1, \"nonsense\": true").unwrap();
    let (_stop, rx) = tokio::sync::watch::channel(false);
    let r = fragment_bridge::driver::run(support::config(&fake.url(), &dir, support::settings()), support::script(), rx).await;
    assert!(matches!(r, Err(fragment_bridge::driver::BridgeError::Corrupt(_))), "{r:?}");
    assert!(dir.join("bridge/state.json").exists(), "left as it was, for a person to read");
}

/// Goal: the restore gate holds the bridge (and its reading of `/data`)
/// until the platform's marker appears.
#[tokio::test]
async fn the_restore_gate_holds() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let _chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("gate");
    let mut cfg = support::config(&fake.url(), &dir, support::settings());
    cfg.restore_pending = true;
    let bridge = support::start(cfg, support::script());
    tokio::time::sleep(Duration::from_millis(400)).await;
    fake.with(|w| assert!(w.calls.is_empty(), "nothing asked before the restore: {:?}", w.calls));
    std::fs::write(dir.join("restored"), b"").unwrap();
    following(&fake, 2).await;
    bridge.stop().await;
}

// ---- crashes and rollbacks (docs/explorations/pi-durable.md, F2, F3 and
// P1). Counted in runs (support::Runs: each turn the runtime was given),
// never in records: a second run's posts are replays of the first's, or
// 409s. A negative is proved by a later positive, never by a sleep: turns
// of a chat run in order, and a new life reads the old records before the
// new, so once one more message's turn has ended, every older turn has been
// run again or fenced. ----

fn starts_of(w: &World, chat: &str, turn: &str) -> Vec<Value> {
    w.bodies(chat, "work", "turn.start").into_iter().filter(|s| s["turn"] == turn).collect()
}

fn ends_of(w: &World, chat: &str, turn: &str) -> Vec<Value> {
    w.bodies(chat, "work", "turn.end").into_iter().filter(|e| e["turn"] == turn).collect()
}

fn work_posts(w: &World, chat: &str) -> usize {
    let route = format!("POST /api/f/{chat}/channels/work");
    w.calls.iter().filter(|c| **c == route).count()
}

/// How many times the keepalive was opened: once more is the proof that the
/// bridge has a new turn in hand.
fn keepalive_opens(w: &World) -> usize {
    w.keepalive_log.iter().filter(|open| **open).count()
}

/// paul says `text` in `chat`; its turn, once that has ended.
async fn said_and_ended(fake: &Fake, chat: &str, text: &str) -> String {
    let said = fake.say(chat, &person("paul"), json!({ "text": text }));
    let turn = turn_of("juniper", chat, seq(&said));
    fake.until(WAIT, &format!("{text:?}'s end"), |w| !ends_of(w, chat, &turn).is_empty()).await;
    turn
}

/// Two turns, a save after the first, a crash after the second; then the
/// save put back (or the state removed), a new life, and a third message.
async fn rollback_case(name: &str, lose_it_all: bool) {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir(name);
    let cfg = support::config(&fake.url(), &dir, support::settings());
    let runs = support::Runs::default();

    let bridge = support::start(cfg.clone(), support::counting(support::script(), &runs));
    following(&fake, 2).await;
    let t1 = said_and_ended(&fake, &chat, "one").await;
    // the save: /data as it is after turn one (its end was written to the
    // state before it was posted)
    let saved = support::save_state(&dir);
    let t2 = said_and_ended(&fake, &chat, "two").await;
    bridge.kill().await;
    fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;

    // the crash's wake: /data goes back to the save, or is gone
    if lose_it_all {
        support::lose_state(&dir);
    } else {
        support::restore_state(&dir, &saved);
    }
    let bridge = support::start(cfg, support::counting(support::script(), &runs));
    let t3 = said_and_ended(&fake, &chat, "three").await;
    assert_eq!(runs.all(), vec![t1.clone(), t2.clone(), t3.clone()], "each turn ran once");
    fake.with(|w| {
        for t in [&t1, &t2, &t3] {
            let ends = ends_of(w, &chat, t);
            assert_eq!(ends.len(), 1, "{t}: one end");
            assert_eq!(ends[0]["outcome"], "idle", "{t}: its own life's end stands");
        }
        assert_eq!(replies(w, &chat).len(), 3, "one reply each");
    });
    bridge.stop().await;
}

/// Goal (I3): a turn that ran is never run again after a rollback: the
/// next life wakes with a save from before it.
#[tokio::test]
#[ignore = "P1: a rolled-back cursor reads the record again and hands its turn to the runtime again (F2)"]
async fn a_rollback_runs_nothing_twice() {
    rollback_case("rollback", false).await;
}

/// Goal (I3): a turn that ran is never run again after `/data` is lost:
/// the next life reads every chat from the start.
#[tokio::test]
#[ignore = "P1: a lost state reads every record again and hands every turn to the runtime again (F2)"]
async fn a_lost_state_runs_nothing_twice() {
    rollback_case("lost-state", true).await;
}

/// Goal (I1, I2): a turn a crash cut short ends once, as an error, and is
/// never run again, whether the next life wakes with the state from before
/// the turn, the state the crash left (the turn running), or none.
#[tokio::test]
#[ignore = "P1: a state from before the turn reads its record again and runs it again"]
async fn a_turn_cut_by_a_crash_ends_once_whatever_state_wakes() {
    for wakes in ["before", "during", "none"] {
        let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
        let chat = fake.chat("talk", &["juniper"]);
        let dir = support::dir(&format!("cut-{wakes}"));
        let cfg = support::config(&fake.url(), &dir, support::settings());
        let runs = support::Runs::default();
        // drafts 100 ms apart: `slow` runs two seconds, time to kill it in
        let pace = Duration::from_millis(100);

        let bridge = support::start(cfg.clone(), support::counting(support::script_paced(pace), &runs));
        following(&fake, 2).await;
        let t1 = said_and_ended(&fake, &chat, "one").await;
        let before = support::save_state(&dir);
        let slow = fake.say(&chat, &person("paul"), json!({ "text": "slow" }));
        let ts = turn_of("juniper", &chat, seq(&slow));
        fake.until(WAIT, "the slow turn running", |w| w.drafts.iter().any(|d| d.2 == ts)).await;
        bridge.kill().await;
        fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;
        match wakes {
            "before" => support::restore_state(&dir, &before),
            "during" => {}
            _ => support::lose_state(&dir),
        }

        let bridge = support::start(cfg, support::counting(support::script_paced(pace), &runs));
        let t3 = said_and_ended(&fake, &chat, "after").await;
        assert_eq!(runs.all(), vec![t1.clone(), ts.clone(), t3.clone()], "{wakes}: the cut turn ran once, in the life it was cut in");
        fake.with(|w| {
            let ends = ends_of(w, &chat, &ts);
            assert_eq!(ends.len(), 1, "{wakes}: one end");
            assert_eq!(ends[0]["outcome"], "error", "{wakes}: ended as lost: {ends:?}");
            assert!(!replies(w, &chat).iter().any(|r| r["turn"] == ts.as_str()), "{wakes}: a cut turn gives no answer");
            assert_eq!(ends_of(w, &chat, &t3)[0]["outcome"], "idle", "{wakes}: the next message is answered");
        });
        bridge.stop().await;
    }
}

/// Goal (I4): a message said while the computer was down is answered once
/// by the next life, even one that wakes with an older save, and the turn
/// that save does not know is not run again for it.
#[tokio::test]
#[ignore = "P1: the older save's cursor runs the turn it does not know again before the new message"]
async fn said_while_it_was_down_is_answered_once_after_a_rollback() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("down-rollback");
    let cfg = support::config(&fake.url(), &dir, support::settings());
    let runs = support::Runs::default();

    let bridge = support::start(cfg.clone(), support::counting(support::script(), &runs));
    following(&fake, 2).await;
    let t1 = said_and_ended(&fake, &chat, "one").await;
    let saved = support::save_state(&dir);
    let t2 = said_and_ended(&fake, &chat, "two").await;
    bridge.kill().await;
    fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;

    // said while it was down; it wakes with the save from before turn two
    let three = fake.say(&chat, &person("paul"), json!({ "text": "three" }));
    let t3 = turn_of("juniper", &chat, seq(&three));
    support::restore_state(&dir, &saved);
    let bridge = support::start(cfg, support::counting(support::script(), &runs));
    fake.until(WAIT, "three's end", |w| !ends_of(w, &chat, &t3).is_empty()).await;
    assert_eq!(runs.all(), vec![t1, t2.clone(), t3.clone()], "three ran once; two was not run again");
    fake.with(|w| {
        let r: Vec<Value> = replies(w, &chat).into_iter().filter(|r| r["turn"] == t3.as_str()).collect();
        assert_eq!(r, vec![json!({ "text": "echo: [paul] three", "turn": t3 })], "answered once");
        assert_eq!(ends_of(w, &chat, &t2)[0]["outcome"], "idle", "two's end stands");
    });
    bridge.stop().await;
}

/// Goal (P1): a turn whose claim (its `turn.start`) the platform does not
/// answer is not run. Method: every post fails as the message arrives;
/// once the fake has refused the claim twice (the lane tried it again),
/// nothing has run; then the platform answers, and the turn runs once.
#[tokio::test]
#[ignore = "P1: the runtime is handed the turn as its turn.start is posted, answered or not"]
async fn a_claim_the_platform_never_answers_runs_nothing() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("unanswered-claim");
    let runs = support::Runs::default();
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::counting(support::script(), &runs));
    following(&fake, 2).await;
    fake.with(|w| w.fail_posts = 1_000);
    let one = fake.say(&chat, &person("paul"), json!({ "text": "one" }));
    let t = turn_of("juniper", &chat, seq(&one));
    fake.until(WAIT, "its claim refused twice", |w| work_posts(w, &chat) >= 2).await;
    assert_eq!(runs.of(&t), 0, "not run while its claim is unanswered");
    fake.with(|w| assert!(starts_of(w, &chat, &t).is_empty(), "nor claimed"));
    fake.with(|w| w.fail_posts = 0);
    fake.until(WAIT * 2, "its end, once the platform answers", |w| !ends_of(w, &chat, &t).is_empty()).await;
    assert_eq!(runs.all(), vec![t.clone()], "run once its claim was answered");
    fake.with(|w| assert_eq!(ends_of(w, &chat, &t)[0]["outcome"], "idle"));
    bridge.stop().await;
}

/// Goal (P1's hold): while the platform holds the computer (the mark a
/// sleep makes before its save), the bridge claims nothing. A message is
/// kept, unclaimed, while the computer is held awake for it (its keepalive,
/// the proof the bridge has it), and is claimed and run once the hold goes.
#[tokio::test]
#[ignore = "P1: the bridge knows no hold"]
async fn held_it_claims_nothing() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("held");
    let runs = support::Runs::default();
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), support::counting(support::script(), &runs));
    following(&fake, 2).await;
    support::hold(&dir);
    let one = fake.say(&chat, &person("paul"), json!({ "text": "one" }));
    let t = turn_of("juniper", &chat, seq(&one));
    fake.until(WAIT, "the bridge holding the computer awake for it", |w| keepalive_opens(w) == 1).await;
    assert_eq!(runs.of(&t), 0, "held: not run");
    fake.with(|w| assert_eq!(work_posts(w, &chat), 0, "held: nothing claimed"));
    let unheld_at = fake.with(|w| w.log.len());
    support::unhold(&dir);
    fake.until(WAIT, "its end once the hold is gone", |w| !ends_of(w, &chat, &t).is_empty()).await;
    assert_eq!(runs.all(), vec![t.clone()], "run once, after the hold");
    fake.with(|w| {
        let start_seq = w.records(&chat, "work").iter().find(|r| r["body"]["kind"] == "turn.start" && r["body"]["turn"] == t.as_str()).map(seq).expect("its claim");
        let claimed_at = w.log.iter().position(|l| *l == format!("record {chat} work {start_seq}")).expect("its claim in the log");
        assert!(claimed_at >= unheld_at, "claimed after the hold went, not before");
    });
    bridge.stop().await;
}

/// Goal (P1's hold, F3): a message that arrives while a sleep holds the
/// computer is never claimed by the life the sleep ends: the next life,
/// woken from the save the sleep took, answers it once.
#[tokio::test]
#[ignore = "P1: the bridge knows no hold, so the dying life claims and runs the message"]
async fn a_message_during_a_hold_waits_for_the_next_life() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("hold-next-life");
    let cfg = support::config(&fake.url(), &dir, support::settings());
    let runs = support::Runs::default();

    let bridge = support::start(cfg.clone(), support::counting(support::script(), &runs));
    following(&fake, 2).await;
    let t1 = said_and_ended(&fake, &chat, "one").await;
    fake.until(WAIT, "the keepalive let go after one", |w| w.keepalive_open == 0 && w.keepalive_log.last() == Some(&false)).await;
    let opens = fake.with(|w| keepalive_opens(w));
    // the sleep: its hold, then its save
    support::hold(&dir);
    let saved = support::save_state(&dir);
    let claims_before = fake.with(|w| work_posts(w, &chat));
    let two = fake.say(&chat, &person("paul"), json!({ "text": "two" }));
    let t2 = turn_of("juniper", &chat, seq(&two));
    fake.until(WAIT, "the dying life holding the computer awake for it", |w| keepalive_opens(w) > opens).await;
    assert_eq!(runs.all(), vec![t1.clone()], "the dying life runs nothing more");
    fake.with(|w| assert_eq!(work_posts(w, &chat), claims_before, "nor claims it"));
    // the sleep's destroy; the next container's /run is fresh, its /data the save
    bridge.kill().await;
    fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;
    support::unhold(&dir);
    support::restore_state(&dir, &saved);

    let bridge = support::start(cfg, support::counting(support::script(), &runs));
    fake.until(WAIT, "two's end, in the next life", |w| !ends_of(w, &chat, &t2).is_empty()).await;
    assert_eq!(runs.all(), vec![t1, t2.clone()], "two ran once, in the next life");
    fake.with(|w| {
        assert_eq!(starts_of(w, &chat, &t2).len(), 1);
        assert_eq!(ends_of(w, &chat, &t2)[0]["outcome"], "idle");
        assert_eq!(replies(w, &chat).iter().filter(|r| r["turn"] == t2.as_str()).count(), 1, "answered once");
    });
    bridge.stop().await;
}
