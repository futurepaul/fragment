//! The `relay` runtime against a scripted Hermes gateway (tests/support/
//! hermes.rs, which dials and turns as Hermes v0.21.5's does) and the fake
//! fragment API. Method: the scripted gateway dials the bridge's Relay with
//! its token; people speak in chats; the chats' records show what Hermes
//! did, translated.

mod support;

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{json, Value};

use fragment_bridge::records;
use fragment_bridge::runtime::relay::{wire, Relay, RelayConfig};
use support::fake::{person, Fake, World};
use support::hermes::Hermes;

const WAIT: u64 = 10_000;
const SECRET: &str = "0123456789abcdef0123456789abcdef";

fn free_port() -> SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap()
}

fn relay(listen: SocketAddr, dir: &std::path::Path) -> Box<Relay> {
    Box::new(Relay { config: RelayConfig { listen, gateway_id: "computer-test".into(), secret: SECRET.into(), media_dir: dir.join("relay-media"), end_settle_ms: 300, empty_settle_ms: 2_000 } })
}

fn replies(w: &World, chat: &str) -> Vec<Value> {
    w.bodies(chat, "chat", "reply")
}

async fn setup(name: &str, agents: &[&str]) -> (Fake, support::Running, Hermes, std::path::PathBuf) {
    let fake = Fake::start("127.0.0.1:0", agents).await;
    let dir = support::dir(name);
    let listen = free_port();
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), relay(listen, &dir));
    let hermes = Hermes::spawn(listen, "computer-test", SECRET);
    (fake, bridge, hermes, dir)
}

/// Goal: Hermes' turn reaches the chat as the bridge's records: its tool
/// progress lines as steps, its draft frames as the chat's drafts, its
/// final send as the reply, its reactions as the turn's end.
#[tokio::test]
async fn a_reply_with_steps_and_drafts() {
    let (fake, bridge, hermes, _dir) = setup("relay-reply", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let said = fake.say(&chat, &person("paul"), json!({ "text": "use a tool please" }));
    let turn = records::turn_id("juniper.paul", &chat, "chat", said["seq"].as_u64().unwrap());
    fake.until(WAIT, "the turn's end", |w| !w.bodies(&chat, "work", "turn.end").is_empty()).await;
    fake.with(|w| {
        assert_eq!(replies(w, &chat), vec![json!({ "text": "echo: [paul] use a tool please", "turn": turn })]);
        let steps: Vec<(String, String)> = w.bodies(&chat, "work", "turn.step").iter().map(|s| (s["tool"].as_str().unwrap().into(), s["args"].as_str().unwrap().into())).collect();
        assert_eq!(steps, vec![("terminal".into(), "`ls`".into()), ("web_search".into(), "\"x\"".into())]);
        let reply = "echo: [paul] use a tool please";
        let partial = w.drafts.iter().filter(|d| d.2 == turn).filter_map(|d| d.3.as_deref()).any(|t| t.len() < reply.len() && reply.starts_with(t));
        assert!(partial, "Hermes' draft frames are the chat's drafts: {:?}", w.drafts);
        assert_eq!(w.bodies(&chat, "work", "turn.end")[0]["outcome"], "idle");
    });
    hermes.with(|s| {
        let e = &s.heard[0];
        assert_eq!(e["source"]["profile"], "juniper-paul", "routed to the agent's profile");
        assert_eq!(e["source"]["chat_id"], wire::chat_id(&chat, "juniper.paul"));
        assert_eq!(e["source"]["user_name"], "paul");
        assert_eq!(e["message_id"], turn);
    });
    bridge.stop().await;
}

/// Goal: an approval is Hermes' `prompt` op as a card; the owner's answer
/// goes back as a structured `prompt_response` mid-turn, and the turn
/// finishes with it.
#[tokio::test]
async fn an_approval_through_the_relay() {
    let (fake, bridge, hermes, _dir) = setup("relay-approval", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "something risky" }));
    fake.until(WAIT, "the prompt card", |w| !w.bodies(&chat, "work", "turn.prompt").is_empty()).await;
    let p = fake.with(|w| w.bodies(&chat, "work", "turn.prompt")[0].clone());
    let options: Vec<&str> = p["options"].as_array().unwrap().iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(options, vec!["once", "session", "deny"]);
    fake.until(WAIT, "the keepalive dropped while it waits", |w| w.keepalive_open == 0).await;
    fake.say(&chat, &person("paul"), json!({ "kind": "prompt_response", "prompt": p["prompt"], "option": "once" }));
    fake.until(WAIT, "the reply", |w| !replies(w, &chat).is_empty()).await;
    fake.with(|w| {
        assert!(replies(w, &chat)[0]["text"].as_str().unwrap().ends_with("(approved)"));
        assert_eq!(w.bodies(&chat, "work", "turn.prompt.closed")[0]["option"], "once");
        // Hermes' own confirmation ("✅ once", an interim send) is a step
        assert!(w.bodies(&chat, "work", "turn.step").iter().any(|s| s["tool"] == "once"), "{:?}", w.bodies(&chat, "work", "turn.step"));
    });
    hermes.with(|s| {
        let answer = s.heard.iter().find(|e| e.get("prompt_response").is_some()).expect("a structured answer");
        assert_eq!(answer["prompt_response"]["option_id"], "once");
        assert_eq!(answer["prompt_response"]["prompt_id"], p["prompt"]);
        assert_ne!(answer["message_id"], s.heard[0]["message_id"], "its own message id, so Hermes never drops it as a replay");
    });
    bridge.stop().await;
}

/// Goal: Stop is `interrupt_inbound` to Hermes; its turn ends stopped with
/// no reply, and the chat's draft is stopped.
#[tokio::test]
async fn stop_interrupts_hermes() {
    let (fake, bridge, hermes, _dir) = setup("relay-stop", &["juniper"]).await;
    hermes.with(|s| s.turn_ms = 200);
    let chat = fake.chat("talk", &["juniper"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let said = fake.say(&chat, &person("paul"), json!({ "text": "slow please" }));
    let turn = records::turn_id("juniper.paul", &chat, "chat", said["seq"].as_u64().unwrap());
    fake.until(WAIT, "a draft", |w| w.drafts.iter().any(|d| d.2 == turn)).await;
    fake.say(&chat, &person("paul"), json!({ "kind": "stop", "turn": turn }));
    fake.until(WAIT, "the turn's end, then its draft stopped", |w| !w.bodies(&chat, "work", "turn.end").is_empty() && w.drafts.last().is_some_and(|d| d.3.is_none())).await;
    fake.with(|w| {
        assert_eq!(w.bodies(&chat, "work", "turn.end")[0]["outcome"], "stopped");
        assert!(replies(w, &chat).is_empty());
    });
    hermes.with(|s| assert_eq!(s.interrupted, 1));
    bridge.stop().await;
}

/// Goal: a message is kept until Hermes acks it: one handed while Hermes is
/// away reaches it when it dials again, and is answered once.
#[tokio::test]
async fn kept_until_acked() {
    let (fake, bridge, hermes, _dir) = setup("relay-away", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    // bounded: Hermes dials within its first backoffs
    for _ in 0..200 {
        if hermes.with(|s| s.dials >= 1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    hermes.with(|s| s.away = true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    fake.say(&chat, &person("paul"), json!({ "text": "are you there" }));
    fake.until(WAIT, "the turn handed", |w| !w.bodies(&chat, "work", "turn.start").is_empty()).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.with(|w| assert!(replies(w, &chat).is_empty(), "Hermes is away"));
    hermes.with(|s| s.away = false);
    fake.until(WAIT, "the reply once Hermes is back", |w| replies(w, &chat).len() == 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.with(|w| assert_eq!(replies(w, &chat).len(), 1));
    hermes.with(|s| {
        assert!(s.dials >= 2);
        assert_eq!(s.heard.iter().filter(|e| e["text"] == "are you there").count(), 1, "handed once to the gateway that acked it");
    });
    bridge.stop().await;
}

/// Goal: two agents on one computer are two Hermes profiles on one
/// gateway: each message is routed to its agent's profile, and each reply
/// is posted as its agent.
#[tokio::test]
async fn two_profiles_on_one_gateway() {
    let (fake, bridge, hermes, _dir) = setup("relay-profiles", &["juniper", "rowan"]).await;
    let talk = fake.chat("talk", &["juniper"]);
    let notes = fake.chat("notes", &["rowan"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 4).await;
    fake.say(&talk, &person("paul"), json!({ "text": "a" }));
    fake.say(&notes, &person("paul"), json!({ "text": "b" }));
    fake.until(WAIT, "both replies", |w| replies(w, &talk).len() == 1 && replies(w, &notes).len() == 1).await;
    hermes.with(|s| {
        let mut profiles: Vec<&str> = s.heard.iter().map(|e| e["source"]["profile"].as_str().unwrap()).collect();
        profiles.sort();
        assert_eq!(profiles, vec!["juniper-paul", "rowan-paul"]);
    });
    fake.with(|w| {
        let by = |chat: &str| w.records(chat, "chat").into_iter().find(|r| r["body"].get("turn").is_some()).unwrap()["principal"].clone();
        assert_eq!((by(&talk), by(&notes)), (json!("id:juniper"), json!("id:rowan")));
    });
    bridge.stop().await;
}

/// Goal: files both ways through Relay's media plane: a message's
/// attachment is re-hosted for Hermes (behind its token), and a file Hermes
/// uploads and sends is the chat's blob on its reply.
#[tokio::test]
async fn media_both_ways() {
    let (fake, bridge, _hermes, _dir) = setup("relay-media", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let bytes = bytes::Bytes::from_static(b"twelve bytes");
    let sha = records::hex(&<sha2::Sha256 as sha2::Digest>::digest(&bytes));
    fake.with(|w| w.fragments.get_mut(&chat).unwrap().blobs.insert(sha.clone(), ("image/png".into(), bytes.clone())));
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "look", "attachments": [{ "sha256": sha, "size": 12, "type": "image/png", "name": "a.png" }] }));
    fake.until(WAIT, "its reply", |w| replies(w, &chat).len() == 1).await;
    fake.with(|w| assert!(replies(w, &chat)[0]["text"].as_str().unwrap().ends_with("[media: 1 files, 12 bytes]"), "{:?}", replies(w, &chat)));
    fake.say(&chat, &person("paul"), json!({ "text": "send media" }));
    fake.until(WAIT, "a reply with a file", |w| replies(w, &chat).iter().any(|r| r.get("attachments").is_some())).await;
    fake.with(|w| {
        let r = replies(w, &chat).into_iter().find(|r| r.get("attachments").is_some()).unwrap();
        assert_eq!(r["text"], "a cat");
        assert_eq!(r["attachments"][0]["name"], "cat.png");
        assert!(w.fragments[&chat].blobs.contains_key(r["attachments"][0]["sha256"].as_str().unwrap()));
    });
    bridge.stop().await;
}

/// Invalid: a dial with another secret is closed 4401 `unauthorized`, and
/// no message reaches it.
#[tokio::test]
async fn a_wrong_token_is_refused() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("relay-refused");
    let listen = free_port();
    let bridge = support::start(support::config(&fake.url(), &dir, support::settings()), relay(listen, &dir));
    let impostor = Hermes::spawn(listen, "computer-test", "not-the-secret-not-the-secret-00");
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    fake.say(&chat, &person("paul"), json!({ "text": "hi" }));
    tokio::time::sleep(Duration::from_millis(800)).await;
    impostor.with(|s| {
        assert!(s.closes.contains(&4401), "{:?}", s.closes);
        assert!(s.heard.is_empty());
    });
    bridge.stop().await;
}
