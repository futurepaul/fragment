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
    Box::new(Relay { config: RelayConfig { listen, gateway_id: "computer-test".into(), secret: SECRET.into(), media_dir: dir.join("relay-media") } })
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

/// A card's life in the expiry tests: the bridge's prompt life and the
/// scripted Hermes' approval timeout, the same, as the Hermes image sets
/// both (`HERMES_BOOT_APPROVAL_TIMEOUT_S`).
const CARD_MS: u64 = 1_500;

fn card_settings() -> fragment_bridge::engine::Settings {
    fragment_bridge::engine::Settings { prompt_ttl_ms: CARD_MS, ..support::settings() }
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

/// Goal: a turn ends at its `❌` though it said nothing (only a `✅`
/// waits for something said: the startup gate's empty bracket's), with no
/// clock; Hermes' notice after it is no step of the failed turn's but a
/// message of its own.
#[tokio::test]
async fn a_failed_turn_ends_at_its_cross() {
    let (fake, bridge, _hermes, _dir) = setup("relay-fail", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let said = fake.say(&chat, &person("paul"), json!({ "text": "fail please" }));
    let turn = records::turn_id("juniper.paul", &chat, "chat", said["seq"].as_u64().unwrap());
    let end = |w: &World| w.bodies(&chat, "work", "turn.end").into_iter().find(|e| e["turn"] == turn);
    fake.until(WAIT, "the turn's end, and the notice", |w| end(w).is_some() && !replies(w, &chat).is_empty()).await;
    fake.with(|w| {
        assert_eq!(end(w).unwrap()["outcome"], "error");
        assert!(w.bodies(&chat, "work", "turn.step").is_empty(), "the notice is no step");
        let notice = replies(w, &chat);
        assert_eq!(notice.len(), 1);
        assert_eq!(notice[0]["text"], support::hermes::FAILURE);
        assert_ne!(notice[0]["turn"], turn, "said after the turn's end, the notice is a turn of its own");
    });
    bridge.stop().await;
}

/// Goal: the model's text beside a tool call (Paul on p5, 2026-10-05: one
/// message got two replies) is the step's words, never a reply of its
/// own: Hermes' stream consumer ends the draft segment at the tool
/// boundary with a send answering the turn, whatever its interim
/// setting, and the turn's one reply is its answer, whether the tool's
/// progress reaches the bridge after that send or before it. An answer
/// said only beside a housekeeping call (Hermes sends nothing after it) is
/// still the turn's reply. Method: the scripted gateway's three orders.
#[tokio::test]
async fn text_beside_a_tool_call_is_the_steps_words() {
    let (fake, bridge, _hermes, _dir) = setup("relay-narrate", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let of = |w: &World, kind: &str, turn: &str| -> Vec<Value> { w.bodies(&chat, if kind == "reply" { "chat" } else { "work" }, kind).into_iter().filter(|b| b["turn"] == turn).collect() };
    // (what was said, the step's words): late, the step came before them
    let only = "echo: [paul] narrate only please";
    for (text, words) in [("narrate please", Some(support::hermes::NARRATION)), ("narrate late please", None), ("narrate only please", Some(only))] {
        let said = fake.say(&chat, &person("paul"), json!({ "text": text }));
        let turn = records::turn_id("juniper.paul", &chat, "chat", said["seq"].as_u64().unwrap());
        fake.until(WAIT, "the turn's end", |w| !of(w, "turn.end", &turn).is_empty()).await;
        fake.with(|w| {
            assert_eq!(of(w, "reply", &turn), vec![json!({ "text": format!("echo: [paul] {text}"), "turn": turn })], "{text}: one reply, its answer");
            let steps = of(w, "turn.step", &turn);
            assert_eq!(steps.len(), 1, "{text}: {steps:?}");
            assert_eq!(steps[0]["text"].as_str(), words, "{text}: the words before the call are the step's, when it comes after them: {steps:?}");
            assert_eq!(of(w, "turn.end", &turn)[0]["outcome"], "idle");
        });
    }
    // the chat's draft of the taken words was put away with them
    fake.with(|w| {
        let last = w.drafts.iter().rev().find(|d| d.3.as_deref() == Some(support::hermes::NARRATION)).map(|d| d.2.clone());
        let turn = last.expect("the words were drafted");
        assert_eq!(w.drafts.iter().rev().find(|d| d.2 == turn).and_then(|d| d.3.clone()), None, "{:?}", w.drafts);
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
    tokio::time::sleep(Duration::from_millis(300)).await;
    fake.with(|w| assert_eq!((w.keepalive_open, w.keepalive_log.clone()), (1, vec![true]), "the card holds the computer awake"));
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

/// Goal: Stop while a turn waits on an approval closes its card as
/// stopped and interrupts Hermes, which ends the turn with no reply.
#[tokio::test]
async fn stop_during_an_approval() {
    let (fake, bridge, hermes, _dir) = setup("relay-stop-approval", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let said = fake.say(&chat, &person("paul"), json!({ "text": "something risky" }));
    let turn = records::turn_id("juniper.paul", &chat, "chat", said["seq"].as_u64().unwrap());
    fake.until(WAIT, "the prompt card", |w| !w.bodies(&chat, "work", "turn.prompt").is_empty()).await;
    fake.say(&chat, &person("paul"), json!({ "kind": "stop", "turn": turn }));
    fake.until(WAIT, "the turn's end", |w| !w.bodies(&chat, "work", "turn.end").is_empty()).await;
    fake.with(|w| {
        assert_eq!(w.bodies(&chat, "work", "turn.prompt.closed")[0]["outcome"], "stopped");
        assert_eq!(w.bodies(&chat, "work", "turn.end")[0]["outcome"], "stopped");
        assert!(replies(w, &chat).is_empty());
    });
    hermes.with(|s| assert_eq!(s.interrupted, 1, "Hermes heard the Stop mid-approval"));
    bridge.stop().await;
}

/// The records of `turn` on `chat`'s `work` of `kind`.
fn work_of(w: &World, chat: &str, turn: &str, kind: &str) -> Vec<Value> {
    w.bodies(chat, "work", kind).into_iter().filter(|b| b["turn"] == turn).collect()
}

/// Goal (Paul on p5, 2026-10-05: "the agent asked me for permission for
/// something but I missed the 1hr window and now it's not responding to
/// chats"): a card nobody answers is closed `expired` at its `expiresAt`,
/// its turn ends once, as Hermes ends it (its approval times out with the
/// card: the command is not run, and it says so), and the next message is
/// claimed and answered: answered, not met by the card's request asked
/// again.
///
/// Method: the Computer DO's idle rule, in a test's time (crates/core/src/
/// computer.rs, `IDLE_MS`): a computer with no keepalive open sleeps 20
/// minutes after its last record; here, at once. If the bridge lets its
/// keepalive go while the card waits, the computer is put to sleep as the DO
/// does it (the hold, its answer, the container gone, Hermes with it) and
/// woken from its state by the next message, after the card's `expiresAt`;
/// otherwise it stays awake through the expiry. The scripted Hermes keeps
/// its session as v0.21.5 does: a turn cut before its reply leaves its
/// message the session's last, and the next message is folded into it. On
/// master the keepalive is let go (decision 42), the sleep cuts the turn,
/// and the next message asks the cut approval again.
#[tokio::test]
async fn an_expired_card_ends_its_turn_and_the_next_message_is_answered() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("relay-expired");
    let listen = free_port();
    let cfg = support::config(&fake.url(), &dir, card_settings());
    let bridge = support::start_killable(cfg.clone(), relay(listen, &dir));
    let hermes = Hermes::spawn(listen, "computer-test", SECRET);
    // Hermes' approval timeout is the card's life (the image sets both)
    hermes.with(|s| s.approval_ms = CARD_MS);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let say = |text: &str| records::turn_id("juniper.paul", &chat, "chat", fake.say(&chat, &person("paul"), json!({ "text": text }))["seq"].as_u64().unwrap());
    let risky = say("something risky");
    fake.until(WAIT, "the card", |w| !work_of(w, &chat, &risky, "turn.prompt").is_empty()).await;
    let expires_at = fake.with(|w| work_of(w, &chat, &risky, "turn.prompt")[0]["expiresAt"].as_u64().unwrap());
    // the DO's idle rule: what the keepalive does while the card waits
    tokio::time::sleep(Duration::from_millis(300)).await;
    let let_go = fake.with(|w| w.keepalive_open == 0);
    let (bridge, hermes, next) = if let_go {
        // asleep under the card: the hold and its answer, then the
        // container gone, its bridge and its Hermes with it
        support::hold(&dir);
        support::until(WAIT, "the bridge's answer to the hold", || support::held(&dir)).await;
        bridge.kill().await;
        hermes.with(|s| s.dead = true);
        fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;
        // bounded: the card's life
        while fragment_bridge::log::now_ms() < expires_at + 200 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // the next message wakes it: no hold, its /data as it slept
        let next = say("good morning");
        support::unhold(&dir);
        let bridge = support::start_killable(cfg, relay(listen, &dir));
        let woken = hermes.after(listen, "computer-test", SECRET);
        (bridge, woken, next)
    } else {
        fake.until(WAIT, "the card expired, awake, and its turn's end", |w| !work_of(w, &chat, &risky, "turn.prompt.closed").is_empty() && !work_of(w, &chat, &risky, "turn.end").is_empty()).await;
        assert!(fragment_bridge::log::now_ms() >= expires_at, "not before its expiresAt");
        hermes.with(|s| assert_eq!(s.timed_out, 1, "Hermes' own approval timed out with the card"));
        let next = say("good morning");
        (bridge, hermes, next)
    };
    fake.until(WAIT, "the next message's turn to end", |w| !work_of(w, &chat, &next, "turn.end").is_empty()).await;
    fake.with(|w| {
        let closed = work_of(w, &chat, &risky, "turn.prompt.closed");
        assert_eq!(closed.iter().map(|c| c["outcome"].clone()).collect::<Vec<_>>(), vec![json!("expired")], "the card closed once, expired");
        assert_eq!(work_of(w, &chat, &risky, "turn.end").len(), 1, "its turn ends once");
        let asked_again = work_of(w, &chat, &next, "turn.prompt");
        assert!(asked_again.is_empty(), "the next message asks nothing: the expired card's request is not asked again (the computer slept under the card: {let_go}): {asked_again:?}");
        let answer = replies(w, &chat).into_iter().find(|r| r["turn"] == next).map(|r| r["text"].clone());
        assert_eq!(answer, Some(json!("echo: [paul] good morning")), "the next message is answered, and only it");
        assert_eq!(work_of(w, &chat, &next, "turn.end")[0]["outcome"], "idle");
    });
    assert!(!let_go, "the card held the computer awake: an idle sleep under it is what cut its turn");
    hermes.with(|s| assert!(s.session.is_empty(), "nothing is left unanswered in its session: {:?}", s.session));
    bridge.stop().await;
}

/// Goal (P5; F10, bit on p5 on 2026-10-05): a turn a restart cuts (here an
/// owner's sleep while its card waits) is never redone by the chat's next
/// message. The next life hands Hermes that message as a turn of its own:
/// its words alone as the text, the new turn as its id, and the platform's
/// note on what was cut (from the journal) as the inbound's read-only
/// context. With the cut turn closed in Hermes' session by our image's
/// boot (`Hermes::after_boot`; the real Hermes in tests/docker.rs), its
/// model is handed the note and the message, never the cut request joined
/// to it: the cut command's approval is not asked again, and the message
/// after is told nothing.
#[tokio::test]
async fn a_turn_cut_by_a_restart_is_told_and_never_folded() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("relay-cut-told");
    let listen = free_port();
    let cfg = support::config(&fake.url(), &dir, support::settings());
    let bridge = support::start_killable(cfg.clone(), relay(listen, &dir));
    let hermes = Hermes::spawn(listen, "computer-test", SECRET);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let say = |text: &str| records::turn_id("juniper.paul", &chat, "chat", fake.say(&chat, &person("paul"), json!({ "text": text }))["seq"].as_u64().unwrap());
    let risky = say("something risky");
    fake.until(WAIT, "the card", |w| !work_of(w, &chat, &risky, "turn.prompt").is_empty()).await;
    // the owner's sleep under the card: the hold and its answer, then the
    // container gone, its bridge and its Hermes with it
    support::hold(&dir);
    support::until(WAIT, "the bridge's answer to the hold", || support::held(&dir)).await;
    bridge.kill().await;
    fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;
    // the next message wakes it, its /data as it slept
    let next = say("good morning");
    support::unhold(&dir);
    let bridge = support::start_killable(cfg, relay(listen, &dir));
    let woken = hermes.after_boot(listen, "computer-test", SECRET);
    fake.until(WAIT, "the next message's turn to end", |w| !work_of(w, &chat, &next, "turn.end").is_empty()).await;
    let after = say("and after that");
    fake.until(WAIT, "the turn after's end", |w| !work_of(w, &chat, &after, "turn.end").is_empty()).await;
    fake.with(|w| {
        assert_eq!(work_of(w, &chat, &risky, "turn.end")[0]["error"], "lost when the computer restarted", "the cut turn ends once, as lost");
        assert!(work_of(w, &chat, &next, "turn.prompt").is_empty(), "the cut approval is not asked again");
        let answer = |t: &str| replies(w, &chat).into_iter().find(|r| r["turn"] == t).map(|r| r["text"].clone());
        assert_eq!(answer(&next), Some(json!("echo: [paul] good morning")), "the next message is answered, and only it");
        assert_eq!(answer(&after), Some(json!("echo: [paul] and after that")));
    });
    woken.with(|s| {
        assert_eq!(s.closed, 1, "the boot closed the cut turn");
        let heard = |t: &str| s.heard.iter().find(|e| e["message_id"] == t).cloned().unwrap_or_else(|| panic!("Hermes heard {t}: {:?}", s.heard));
        let n = heard(&next);
        assert_eq!(n["text"], "good morning", "the message's words alone: {n}");
        let note = n["context"][0]["text"].as_str().unwrap_or_else(|| panic!("the note, as the inbound's context: {n}"));
        for said in ["Your previous turn in this chat was cut short: your computer restarted before it finished.", "Check what it already did before you do any of it again", "It was answering: “something risky”", "It asked: “⚠️ **Dangerous command** `rm -rf x`” (expired)"] {
            assert!(note.contains(said), "{said:?} in {note}");
        }
        assert!(heard(&after).get("context").is_none(), "the turn after is told nothing: {}", heard(&after));
        let handed: Vec<&str> = s.handed.iter().map(|(_, h)| h.as_str()).collect();
        assert_eq!(handed, vec![format!("[Recent channel messages]\n{note}\n\n[New message]\n[paul] good morning").as_str(), "[paul] and after that"], "its model is handed the note and the message, never the cut request joined to it");
    });
    bridge.stop().await;
}

/// Goal: a message said while a card is open waits behind its turn (one
/// turn of an agent in a chat at a time), and is claimed and answered once
/// the card expires and its turn ends: nothing queues behind an expired
/// card. Its asker is not the card's (only the owner answers a card).
#[tokio::test]
async fn a_message_behind_an_expired_card_is_answered() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let dir = support::dir("relay-expired-behind");
    let listen = free_port();
    let bridge = support::start(support::config(&fake.url(), &dir, card_settings()), relay(listen, &dir));
    let hermes = Hermes::spawn(listen, "computer-test", SECRET);
    hermes.with(|s| s.approval_ms = CARD_MS);
    let chat = fake.chat("talk", &["juniper"]);
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let risky = records::turn_id("juniper.paul", &chat, "chat", fake.say(&chat, &person("paul"), json!({ "text": "something risky" }))["seq"].as_u64().unwrap());
    fake.until(WAIT, "the card", |w| !work_of(w, &chat, &risky, "turn.prompt").is_empty()).await;
    let paul = records::turn_id("juniper.paul", &chat, "chat", fake.say(&chat, &person("paul"), json!({ "text": "hello?" }))["seq"].as_u64().unwrap());
    let skyler = records::turn_id("juniper.paul", &chat, "chat", fake.say(&chat, &person("skyler"), json!({ "text": "me too" }))["seq"].as_u64().unwrap());
    fake.until(WAIT, "both answered", |w| !work_of(w, &chat, &paul, "turn.end").is_empty() && !work_of(w, &chat, &skyler, "turn.end").is_empty()).await;
    fake.with(|w| {
        assert_eq!(work_of(w, &chat, &risky, "turn.prompt.closed")[0]["outcome"], "expired");
        let order: Vec<&str> = w.bodies(&chat, "work", "turn.start").iter().filter_map(|s| s["turn"].as_str()).map(|t| if t == risky { "risky" } else if t == paul { "paul" } else if t == skyler { "skyler" } else { "?" }).collect();
        assert_eq!(order, vec!["risky", "paul", "skyler"], "in order, each once");
        for t in [&risky, &paul, &skyler] {
            assert_eq!(work_of(w, &chat, t, "turn.end").len(), 1, "{t} ends once");
            assert!(replies(w, &chat).iter().any(|r| r["turn"] == t.as_str()), "{t} is answered: {:?}", replies(w, &chat));
        }
    });
    bridge.stop().await;
}

/// Goal (P1): a turn is claimed only while Hermes is on its socket. A
/// message said while Hermes is away waits, unclaimed, for any life to
/// claim (the bridge has it: it holds the computer awake for it), and is
/// claimed and answered once Hermes dials. Within a life, a turn handed is
/// kept until Hermes acks it: one handed to a Hermes that never took it is
/// handed again on its next dial, and answered once.
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
    let started = |w: &World, turn: &str| w.bodies(&chat, "work", "turn.start").iter().filter(|s| s["turn"] == turn).count();
    let say = |text: &str| {
        let said = fake.say(&chat, &person("paul"), json!({ "text": text }));
        records::turn_id("juniper.paul", &chat, "chat", said["seq"].as_u64().unwrap())
    };

    // away: its socket closes, and the bridge hears it go
    hermes.with(|s| s.away = true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let t1 = say("are you there");
    fake.until(WAIT, "the bridge holding the computer awake for it", |w| w.keepalive_open == 1).await;
    fake.with(|w| assert_eq!(started(w, &t1), 0, "Hermes is away: not claimed"));
    hermes.with(|s| s.away = false);
    fake.until(WAIT, "the reply once Hermes is back", |w| replies(w, &chat).len() == 1).await;
    fake.with(|w| assert_eq!(started(w, &t1), 1, "claimed once Hermes dialed"));

    // handed to a Hermes that drops it unacked, then dials again
    hermes.with(|s| s.deaf = true);
    let t2 = say("still there?");
    fake.until(WAIT, "the second message claimed and handed", |w| started(w, &t2) == 1).await;
    hermes.with(|s| s.away = true);
    tokio::time::sleep(Duration::from_millis(200)).await;
    hermes.with(|s| {
        s.deaf = false;
        s.away = false;
    });
    fake.until(WAIT, "its reply once Hermes dials again", |w| replies(w, &chat).len() == 2).await;
    // one more, so a second answer to either would have come before its own
    let t3 = say("and now?");
    fake.until(WAIT, "the third reply", |w| replies(w, &chat).len() == 3).await;
    fake.with(|w| {
        let turns: Vec<&str> = replies(w, &chat).iter().map(|r| r["turn"].as_str().unwrap_or("")).map(|t| if t == t1 { "one" } else if t == t2 { "two" } else if t == t3 { "three" } else { "?" }).collect();
        assert_eq!(turns, vec!["one", "two", "three"], "each answered once");
    });
    hermes.with(|s| {
        assert!(s.dials >= 3);
        for text in ["are you there", "still there?", "and now?"] {
            assert_eq!(s.heard.iter().filter(|e| e["text"] == text).count(), 1, "{text}: heard once, by the gateway that acked it");
        }
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

/// Goal (I3, through the relay): Hermes hears each message once across a
/// crash of the bridge and a rollback of its state. Method: two turns, a
/// save after the first, a kill after the second, the save put back and a
/// new life on the same port (Hermes dials it again); a third message's
/// end proves the older records were read again first. Hermes' own record
/// of what it heard is the count of runs.
#[tokio::test]
async fn hermes_hears_each_message_once_across_a_rollback() {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("relay-rollback");
    let listen = free_port();
    let cfg = support::config(&fake.url(), &dir, support::settings());
    let hermes = Hermes::spawn(listen, "computer-test", SECRET);
    let ended = |w: &World, turn: &str| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == turn);
    let say = |text: &str| {
        let said = fake.say(&chat, &person("paul"), json!({ "text": text }));
        records::turn_id("juniper.paul", &chat, "chat", said["seq"].as_u64().unwrap())
    };

    let bridge = support::start_killable(cfg.clone(), relay(listen, &dir));
    fake.until(WAIT, "the bridge to follow", |w| w.live_sockets() >= 2).await;
    let t1 = say("one");
    fake.until(WAIT, "one's end", |w| ended(w, &t1)).await;
    let saved = support::save_state(&dir);
    let t2 = say("two");
    fake.until(WAIT, "two's end", |w| ended(w, &t2)).await;
    bridge.kill().await;
    fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;

    support::restore_state(&dir, &saved);
    let bridge = support::start_killable(cfg, relay(listen, &dir));
    let t3 = say("three");
    fake.until(WAIT, "three's end, Hermes dialing the new life", |w| ended(w, &t3)).await;
    hermes.with(|s| {
        let heard: Vec<&str> = s.heard.iter().filter_map(|e| e["message_id"].as_str()).collect();
        assert_eq!(heard, vec![t1.as_str(), t2.as_str(), t3.as_str()], "each message once");
        assert!(s.dials >= 2, "it dialed the new life");
    });
    fake.with(|w| assert_eq!(replies(w, &chat).len(), 3, "one reply each"));
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
