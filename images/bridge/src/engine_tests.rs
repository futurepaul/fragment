//! The engine's rules, each a valid path and its invalid, replay, and
//! restart paths. Method: drive the pure engine with records and runtime
//! events at chosen times, and read the effects it asks for. The rules of
//! one turn answer each claim as this life's (`answered`); the rules across
//! lives drive the engine inside a model world (below, "across lives").

use std::collections::VecDeque;

use serde_json::{json, Value};

use super::*;
use crate::records::Step as ToolStep;

const T0: u64 = 1_000_000;

/// The life of a test's engine.
const LIFE: &str = "0123456789abcdef0123456789abcdef";

fn agent(label: &str) -> Agent {
    Agent { fragment: format!("{label}.paul"), identity: format!("id:{label}"), name: label.to_string(), owner: "id:paul".into(), credentials: vec![] }
}

/// A fresh engine whose runtime can take turns.
fn engine(agents: &[Agent]) -> Engine {
    let mut e = Engine::new(State::default(), Settings { prompt_ttl_ms: 60_000, turn_idle_ms: 600_000 }, LIFE).expect("a fresh state");
    e.step(Input::Agents(agents.to_vec()), T0);
    e.recover(T0);
    e.step(Input::Runtime(Event::Connected(true)), T0);
    e
}

/// `s`, with each claim it makes answered as this life's and each record it
/// owes answered, and what each answer does (the turn handed to the
/// runtime, an ended turn let go) added to it.
fn answered(e: &mut Engine, mut s: Step, now: u64) -> Step {
    let claims: Vec<String> = s.effects.iter().filter_map(|x| match x { Effect::Claim { turn, .. } => Some(turn.clone()), _ => None }).collect();
    for turn in claims {
        let more = e.step(Input::Claimed { turn, answer: ClaimAnswer::Ours { seq: None } }, now);
        s.dirty |= more.dirty;
        s.effects.extend(more.effects);
    }
    let owed: Vec<(String, String)> = s.effects.iter().filter_map(|x| match x { Effect::Owed { turn, id, .. } => Some((turn.clone(), id.clone())), _ => None }).collect();
    for (turn, id) in owed {
        let more = e.step(Input::Posted { turn, id }, now);
        s.dirty |= more.dirty;
        s.effects.extend(more.effects);
    }
    s
}

fn rec(seq: u64, principal: &str, body: Value) -> Record {
    Record { channel: "chat".into(), seq, at: 0, principal: principal.into(), kind: "message".into(), body }
}

fn view(agents: &[&Agent]) -> ChatView {
    let mut v = ChatView { agents: agents.iter().map(|a| a.identity.clone()).collect(), ..ChatView::default() };
    v.names.insert("id:paul".into(), "paul".into());
    v.names.insert("id:skyler".into(), "skyler".into());
    v
}

fn said(e: &mut Engine, a: &Agent, v: &ChatView, seq: u64, principal: &str, body: Value, now: u64) -> Step {
    let s = e.step(Input::Record { agent: a.fragment.clone(), fragment: "talk.paul".into(), record: rec(seq, principal, body), view: Some(v.clone()), since: 0 }, now);
    answered(e, s, now)
}

/// What a step posts, claims and owed records included.
fn posts(s: &Step) -> Vec<(String, Value)> {
    s.effects.iter().filter_map(|e| match e { Effect::Post { id, body, .. } | Effect::Claim { id, body, .. } | Effect::Owed { id, body, .. } => Some((id.clone(), body.clone())), _ => None }).collect()
}

fn kinds(s: &Step) -> Vec<String> {
    posts(s).iter().map(|(_, b)| b.get("kind").and_then(Value::as_str).unwrap_or("reply").to_string()).collect()
}

fn started(s: &Step) -> Option<TurnStart> {
    s.effects.iter().find_map(|e| match e { Effect::Runtime(Command::Start(t)) => Some((**t).clone()), _ => None })
}

fn commands(s: &Step) -> Vec<Command> {
    s.effects.iter().filter_map(|e| match e { Effect::Runtime(c) => Some(c.clone()), _ => None }).collect()
}

fn keepalive(s: &Step) -> Option<bool> {
    s.effects.iter().find_map(|e| match e { Effect::Keepalive(k) => Some(*k), _ => None })
}

fn ev(e: &mut Engine, event: Event, now: u64) -> Step {
    let s = e.step(Input::Runtime(event), now);
    answered(e, s, now)
}

/// Goal: a person's message to the lead starts exactly one turn, which is
/// handed to the runtime with who asked, and keeps the computer awake.
#[test]
fn a_message_starts_one_turn() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let s = said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "hi" }), T0);
    assert!(s.dirty, "admission is persisted before it is acted on");
    let t = started(&s).expect("handed to the runtime");
    assert_eq!((t.text.as_str(), t.asker.as_str(), t.asker_name.as_str(), t.seq), ("hi", "id:paul", "paul", 1));
    assert_eq!(t.turn, records::turn_id(&a.fragment, "talk.paul", "chat", 1));
    assert_eq!(kinds(&s), vec!["turn.start"]);
    assert_eq!(posts(&s)[0].0, records::work_id(&t.turn, "start"));
    assert_eq!(posts(&s)[0].1["life"], LIFE, "claimed as this life");
    assert_eq!(keepalive(&s), Some(true));
    assert_eq!(e.cursor(&a.fragment, "talk.paul", "chat"), 1);

    // replay: the same record again (a catch-up, a reconnect) does nothing
    let again = said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "hi" }), T0 + 1);
    assert_eq!(again, Step::default());
    // and an older one is behind the cursor too
    assert_eq!(said(&mut e, &a, &v, 0, "id:paul", json!({ "text": "old" }), T0 + 2).effects, vec![]);
}

/// Invalid: the agent's own records, anonymous visitors, page-only kinds,
/// and broken bodies start nothing (but still move the cursor past them).
#[test]
fn what_starts_nothing() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let cases = [
        ("id:juniper", json!({ "text": "my own reply", "turn": "abc" })),
        ("anon:7", json!({ "text": "hello?" })),
        ("id:paul", json!({ "kind": "typing" })),
        ("id:paul", json!({ "text": 12 })),
        ("id:paul", json!({ "kind": "prompt_response", "prompt": "nope", "option": "once" })),
        ("id:paul", json!({ "kind": "stop" })),
    ];
    for (i, (who, body)) in cases.into_iter().enumerate() {
        let seq = i as u64 + 1;
        let s = said(&mut e, &a, &v, seq, who, body.clone(), T0);
        assert!(started(&s).is_none(), "{who} {body}");
        assert!(posts(&s).is_empty(), "{who} {body}");
        assert_eq!(e.cursor(&a.fragment, "talk.paul", "chat"), seq, "the cursor passes it anyway");
    }
    // a record for an agent this computer does not run is refused whole
    let stranger = agent("rowan");
    let s = said(&mut e, &stranger, &v, 99, "id:paul", json!({ "text": "hi" }), T0);
    assert_eq!(s, Step::default());
}

/// Goal: a streamed reply shows as drafts, then posts as one record naming
/// its turn (which replaces the draft), then the turn ends and the draft is
/// stopped, and the computer may sleep again.
#[test]
fn a_reply_streams_then_posts() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "hi" }), T0)).expect("started").turn;
    let d = ev(&mut e, Event::Draft { turn: turn.clone(), text: "He".into() }, T0 + 2);
    assert_eq!(d.effects, vec![Effect::Draft { agent: a.fragment.clone(), fragment: "talk.paul".into(), turn: turn.clone(), text: Some("He".into()) }]);
    assert!(!d.dirty, "drafts are never stored");
    let r = ev(&mut e, Event::Reply { turn: turn.clone(), part: 1, text: "Hello".into() }, T0 + 3);
    assert!(posts(&r).is_empty(), "a reply waits for its turn's end, or a later part");
    let end = e.step(Input::Runtime(Event::End { turn: turn.clone(), outcome: Outcome::Idle }), T0 + 4);
    let p = posts(&end);
    assert_eq!(p[0], (records::reply_id(&turn, 1), json!({ "text": "Hello", "turn": turn })));
    assert_eq!(p[1], (records::work_id(&turn, "end"), json!({ "kind": "turn.end", "turn": turn, "outcome": "idle" })));
    assert!(end.effects.contains(&Effect::Draft { agent: a.fragment.clone(), fragment: "talk.paul".into(), turn: turn.clone(), text: None }));
    assert_eq!(keepalive(&end), Some(false));
    // its end is owed: the turn is kept, ended, until the lane is done with it
    let t = &e.state().turns[&turn];
    assert_eq!((t.phase, t.owed.len()), (Phase::Ended, 1));
    assert!(e.step(Input::Runtime(Event::Draft { turn: turn.clone(), text: "late".into() }), T0 + 4).effects.is_empty(), "an ended turn hears its runtime no more");
    let done = e.step(Input::Posted { turn: turn.clone(), id: records::work_id(&turn, "end") }, T0 + 4);
    assert!(done.dirty && done.effects.is_empty());
    assert!(e.state().turns.is_empty(), "let go once its end is answered");
    // replay: a late event for the ended turn is dropped
    assert_eq!(ev(&mut e, Event::Reply { turn: turn.clone(), part: 2, text: "late".into() }, T0 + 5), Step::default());
}

/// Goal: text, then a tool call, then text: the first reply posts before the
/// step, steps number from 1, and a late edit of a posted part is dropped.
#[test]
fn steps_split_replies() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "look it up" }), T0)).expect("started").turn;
    ev(&mut e, Event::Reply { turn: turn.clone(), part: 1, text: "Looking.".into() }, T0 + 1);
    let step = ToolStep { tool: "web_search".into(), args: "{\"q\":\"x\"}".into(), ok: true, excerpt: "3 results".into(), text: String::new() };
    let s = ev(&mut e, Event::Step { turn: turn.clone(), step: step.clone() }, T0 + 2);
    assert_eq!(kinds(&s), vec!["reply", "turn.step"]);
    assert_eq!(posts(&s)[1].0, records::work_id(&turn, "1"));
    assert_eq!(posts(&s)[1].1["step"], 1);
    let late = ev(&mut e, Event::Reply { turn: turn.clone(), part: 1, text: "Looking!".into() }, T0 + 3);
    assert!(posts(&late).is_empty() && late.effects.is_empty(), "part 1 was posted");
    ev(&mut e, Event::Step { turn: turn.clone(), step }, T0 + 4);
    ev(&mut e, Event::Reply { turn: turn.clone(), part: 2, text: "Found it.".into() }, T0 + 5);
    let end = ev(&mut e, Event::End { turn: turn.clone(), outcome: Outcome::Idle }, T0 + 6);
    assert_eq!(posts(&end)[0].0, records::reply_id(&turn, 2));
    // a retracted part never posts
    let t2 = started(&said(&mut e, &a, &v, 2, "id:paul", json!({ "text": "again" }), T0 + 7)).expect("started").turn;
    ev(&mut e, Event::Reply { turn: t2.clone(), part: 1, text: "oops".into() }, T0 + 8);
    ev(&mut e, Event::Retract { turn: t2.clone(), part: 1 }, T0 + 9);
    let end2 = ev(&mut e, Event::End { turn: t2.clone(), outcome: Outcome::Stopped }, T0 + 10);
    assert_eq!(kinds(&end2), vec!["turn.end"]);
}

/// Goal: one turn of an agent runs in a chat at a time, the rest in order;
/// past the queue's bound a message is told so, not queued: its claim and
/// its end, both records as every turn has, and nothing run.
#[test]
fn one_turn_at_a_time() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let first = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "one" }), T0)).expect("started").turn;
    let s2 = said(&mut e, &a, &v, 2, "id:paul", json!({ "text": "two" }), T0);
    assert!(started(&s2).is_none() && posts(&s2).is_empty(), "queued, silently");
    said(&mut e, &a, &v, 3, "id:paul", json!({ "text": "three" }), T0);
    let next = started(&ev(&mut e, Event::End { turn: first, outcome: Outcome::Idle }, T0 + 1)).expect("the next one");
    assert_eq!(next.text, "two");
    // the bound: the running one plus QUEUED_PER_CHAT_MAX waiting
    for seq in 4..(3 + limits::QUEUED_PER_CHAT_MAX as u64) {
        let s = said(&mut e, &a, &v, seq, "id:paul", json!({ "text": "more" }), T0);
        assert!(posts(&s).is_empty());
    }
    let over = said(&mut e, &a, &v, 100, "id:paul", json!({ "text": "too many" }), T0);
    assert_eq!(kinds(&over), vec!["turn.start", "turn.end"]);
    let refused = records::turn_id(&a.fragment, "talk.paul", "chat", 100);
    assert_eq!(posts(&over)[0], (records::work_id(&refused, "start"), json!({ "kind": "turn.start", "turn": refused, "asker": "id:paul", "agent": "id:juniper", "cause": { "fragment": "talk.paul", "channel": "chat", "seq": 100 }, "life": LIFE })));
    assert_eq!(posts(&over)[1].1["outcome"], "error");
    assert!(started(&over).is_none());
    assert!(!e.state().turns.contains_key(&refused), "held only until both its records were answered");
}

/// Goal: only the turn's asker stops it. A Stop from anyone else is
/// ignored; a Stop naming a queued turn removes it, with its claim and its
/// end (both records, nothing run).
#[test]
fn stop_is_the_askers() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "long" }), T0)).expect("started").turn;
    let other = said(&mut e, &a, &v, 2, "id:skyler", json!({ "kind": "stop", "turn": turn }), T0 + 1);
    assert!(commands(&other).is_empty(), "skyler did not ask it");
    let s = said(&mut e, &a, &v, 3, "id:paul", json!({ "kind": "stop" }), T0 + 2);
    assert_eq!(commands(&s), vec![Command::Stop { turn: turn.clone() }]);
    // replay: stopping again asks nothing more
    let again = said(&mut e, &a, &v, 4, "id:paul", json!({ "kind": "stop", "turn": turn }), T0 + 3);
    assert!(commands(&again).is_empty());
    let end = ev(&mut e, Event::End { turn: turn.clone(), outcome: Outcome::Stopped }, T0 + 4);
    assert_eq!(posts(&end).last().expect("its end").1["outcome"], "stopped");

    // a queued turn, stopped by name
    let running = started(&said(&mut e, &a, &v, 5, "id:paul", json!({ "text": "a" }), T0 + 5)).expect("started").turn;
    said(&mut e, &a, &v, 6, "id:paul", json!({ "text": "b" }), T0 + 5);
    let queued = records::turn_id(&a.fragment, "talk.paul", "chat", 6);
    let s = said(&mut e, &a, &v, 7, "id:paul", json!({ "kind": "stop", "turn": queued }), T0 + 6);
    assert_eq!(kinds(&s), vec!["turn.start", "turn.end"]);
    assert_eq!((posts(&s)[0].0.as_str(), &posts(&s)[0].1["life"]), (records::work_id(&queued, "start").as_str(), &json!(LIFE)));
    assert_eq!(posts(&s)[1], (records::work_id(&queued, "end"), json!({ "kind": "turn.end", "turn": queued, "outcome": "stopped" })));
    assert!(commands(&s).is_empty(), "the runtime never had it");
    let after = ev(&mut e, Event::End { turn: running, outcome: Outcome::Idle }, T0 + 7);
    assert!(started(&after).is_none(), "the stopped one never runs");
}

/// Goal: a prompt is a card for the agent's owner; while it waits the
/// computer is kept awake; the owner's answer (the first) resumes the turn.
/// Invalid: someone else's answer, an unknown option, a second answer.
#[test]
fn an_approval_answered() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "risky" }), T0)).expect("started").turn;
    let options = vec![PromptOption { id: "once".into(), label: "Allow once".into(), style: None }, PromptOption { id: "deny".into(), label: "Deny".into(), style: Some("danger".into()) }];
    let p = ev(&mut e, Event::Prompt { turn: turn.clone(), prompt: "ab12.0011".into(), text: "Run rm?".into(), options: options.clone(), ttl_ms: None }, T0 + 10);
    let (id, body) = posts(&p)[0].clone();
    assert_eq!(id, records::work_id(&turn, "p:ab12.0011"));
    assert_eq!(body["kind"], "turn.prompt");
    assert_eq!(body["asks"], "id:paul");
    assert_eq!(body["expiresAt"], T0 + 10 + 60_000);
    assert_eq!(keepalive(&p), None, "an open card keeps the computer awake");
    assert!(e.keepalive());
    // a repeat of the same prompt asks nothing
    assert_eq!(ev(&mut e, Event::Prompt { turn: turn.clone(), prompt: "ab12.0011".into(), text: "again".into(), options, ttl_ms: None }, T0 + 11).effects, vec![]);

    let skyler = said(&mut e, &a, &v, 2, "id:skyler", json!({ "kind": "prompt_response", "prompt": "ab12.0011", "option": "once" }), T0 + 20);
    assert!(commands(&skyler).is_empty() && posts(&skyler).is_empty(), "only the owner answers");
    let bad = said(&mut e, &a, &v, 3, "id:paul", json!({ "kind": "prompt_response", "prompt": "ab12.0011", "option": "always" }), T0 + 21);
    assert!(commands(&bad).is_empty(), "not an option of this prompt");
    let yes = said(&mut e, &a, &v, 4, "id:paul", json!({ "kind": "prompt_response", "prompt": "ab12.0011", "option": "once" }), T0 + 22);
    assert_eq!(commands(&yes), vec![Command::Answer { turn: turn.clone(), prompt: "ab12.0011".into(), option: Some("once".into()), seq: 4, by: "id:paul".into() }]);
    assert_eq!(posts(&yes)[0].1, json!({ "kind": "turn.prompt.closed", "turn": turn, "prompt": "ab12.0011", "outcome": "answered", "option": "once", "by": "id:paul" }));
    assert_eq!(keepalive(&yes), None, "still awake, running again");
    let second = said(&mut e, &a, &v, 5, "id:paul", json!({ "kind": "prompt_response", "prompt": "ab12.0011", "option": "deny" }), T0 + 23);
    assert!(commands(&second).is_empty(), "the first answer won");
}

/// Goal: an unanswered prompt expires: its card says so, and the runtime is
/// told it got no answer. An answer after that is ignored.
#[test]
fn an_approval_expires() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "risky" }), T0)).expect("started").turn;
    let options = vec![PromptOption { id: "once".into(), label: "Allow".into(), style: None }];
    ev(&mut e, Event::Prompt { turn: turn.clone(), prompt: "p1".into(), text: "ok?".into(), options, ttl_ms: Some(30_000) }, T0);
    assert!(e.step(Input::Tick, T0 + 29_999).effects.is_empty(), "not yet");
    let t = e.step(Input::Tick, T0 + 30_000);
    assert_eq!(posts(&t)[0].1["outcome"], "expired");
    assert_eq!(commands(&t), vec![Command::Answer { turn: turn.clone(), prompt: "p1".into(), option: None, seq: 0, by: String::new() }]);
    let late = said(&mut e, &a, &v, 2, "id:paul", json!({ "kind": "prompt_response", "prompt": "p1", "option": "once" }), T0 + 40_000);
    assert!(commands(&late).is_empty());
    // a prompt no card can show is expired at once
    let bad = ev(&mut e, Event::Prompt { turn: turn.clone(), prompt: "has space".into(), text: "?".into(), options: vec![], ttl_ms: None }, T0 + 41_000);
    assert!(posts(&bad).is_empty());
    assert_eq!(commands(&bad).len(), 1);
}

/// Goal (Paul on p5, 2026-10-05: a card missed, then no answers): an open
/// card keeps its computer awake its whole life, so it expires with its
/// runtime still there (an idle sleep under it cut its turn, and Hermes
/// then met the next message with the cut request asked again). At its
/// expiry the card is closed `expired` and the runtime told; the turn ends
/// as the runtime ends it; the message said meanwhile, which waited behind
/// it, is claimed and run; only then is the computer let go.
#[test]
fn an_open_card_keeps_its_computer_awake_until_it_expires() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "risky" }), T0)).expect("started").turn;
    let options = vec![PromptOption { id: "once".into(), label: "Allow".into(), style: None }];
    let p = ev(&mut e, Event::Prompt { turn: turn.clone(), prompt: "p1".into(), text: "ok?".into(), options, ttl_ms: None }, T0 + 10);
    assert_eq!(posts(&p)[0].1["expiresAt"], T0 + 10 + 60_000);
    assert_eq!(keepalive(&p), None, "the card holds the computer: no let-go");
    assert!(e.keepalive(), "held while the card is open");
    let hello = said(&mut e, &a, &v, 2, "id:paul", json!({ "text": "hello?" }), T0 + 20_000);
    assert!(started(&hello).is_none(), "a message meanwhile waits behind the card's turn");
    let quiet = e.step(Input::Tick, T0 + 10 + 59_999);
    assert!(quiet.effects.is_empty() && e.keepalive(), "held to the card's last moment");
    let x = e.step(Input::Tick, T0 + 10 + 60_000);
    assert_eq!(posts(&x), vec![(records::work_id(&turn, "pc:p1"), json!({ "kind": "turn.prompt.closed", "turn": turn, "prompt": "p1", "outcome": "expired" }))]);
    assert_eq!(commands(&x), vec![Command::Answer { turn: turn.clone(), prompt: "p1".into(), option: None, seq: 0, by: String::new() }]);
    assert_eq!((e.state().turns[&turn].phase, keepalive(&x), e.keepalive()), (Phase::Running, None, true), "running again, as its runtime goes on without the answer");
    // the runtime ends it (Hermes: its approval timed out with the card, the
    // command BLOCKED, then its reply): the message that waited is run
    let end = ev(&mut e, Event::End { turn: turn.clone(), outcome: Outcome::Idle }, T0 + 61_000);
    assert_eq!(posts(&end).iter().filter(|(id, _)| id == &records::work_id(&turn, "end")).count(), 1, "the card's turn ends once");
    let next = started(&end).expect("the message that waited is claimed and run");
    assert_eq!(next.text, "hello?");
    assert_eq!(keepalive(&end), None, "awake for it");
    let done = ev(&mut e, Event::End { turn: next.turn.clone(), outcome: Outcome::Idle }, T0 + 62_000);
    assert_eq!(keepalive(&done), Some(false), "nothing open: let go");
}

/// Goal: a restart keeps every admission: a turn the runtime held is ended
/// (one life per turn: its runtime died with the life before), a queued one
/// is claimed by the new life once its runtime can take it, and runs, and
/// catching up on the same records starts nothing. (A turn handed and never
/// taken is ended too: `a_restart_ends_what_was_handed`.)
#[test]
fn a_restart_starts_nothing_twice() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let first = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "one" }), T0)).expect("started").turn;
    said(&mut e, &a, &v, 2, "id:paul", json!({ "text": "two" }), T0);
    let b = agent("rowan");
    // the state as persisted, read back
    let saved = serde_json::to_string(e.state()).expect("serializes");
    let state: State = serde_json::from_str(&saved).expect("deserializes");
    assert_eq!(&state, e.state(), "a round trip changes nothing");
    assert!(!saved.contains(LIFE), "the life is never in the state");

    let next_life = "fedcba9876543210fedcba9876543210";
    let mut e2 = Engine::new(state, Settings::default(), next_life).expect("whole");
    e2.step(Input::Agents(vec![a.clone(), b]), T0 + 10);
    let r = e2.recover(T0 + 10);
    let p = posts(&r);
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0].1, json!({ "kind": "turn.end", "turn": first, "outcome": "error", "error": LOST }));
    assert!(started(&r).is_none(), "nothing runs before its runtime can take it");
    let c = e2.step(Input::Runtime(Event::Connected(true)), T0 + 10);
    assert_eq!(posts(&c)[0].1["life"], next_life, "claimed by the new life");
    let next = started(&answered(&mut e2, c, T0 + 10)).expect("the queued one runs");
    assert_eq!(next.text, "two");
    // the backlog again: nothing new
    for seq in 1..=2 {
        assert_eq!(said(&mut e2, &a, &v, seq, "id:paul", json!({ "text": "again" }), T0 + 11), Step::default());
    }
    assert_eq!(e2.state().boot, 2);
}

/// Invalid state: a turn past its channel's cursor, or a wrong id, is
/// refused at load rather than trusted.
#[test]
fn a_corrupt_state_is_refused() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    said(&mut e, &a, &v, 5, "id:paul", json!({ "text": "x" }), T0);
    let mut behind = e.state().clone();
    behind.cursors.insert(cursor_key(&a.fragment, "talk.paul", "chat"), 4);
    assert!(Engine::new(behind, Settings::default(), LIFE).is_err());
    let mut renamed = e.state().clone();
    let (id, mut t) = renamed.turns.pop_first().expect("one");
    t.id = "0".repeat(24);
    renamed.turns.insert(id, t);
    assert!(Engine::new(renamed, Settings::default(), LIFE).is_err());
    let mut old = e.state().clone();
    old.version = 0;
    assert!(Engine::new(old, Settings::default(), LIFE).is_err());
}

/// Goal (decision 8): in a group, an `@mention` (or `to`) picks who answers;
/// otherwise the lead, the first agent added, does. A reply that names
/// another agent hands off to it, a bounded number of hops deep.
#[test]
fn a_group_picks_who_answers() {
    let (j, r) = (agent("juniper"), agent("rowan"));
    let v = view(&[&j, &r]);
    let mut e = engine(&[j.clone(), r.clone()]);
    let both = |e: &mut Engine, seq: u64, who: &str, body: Value| -> (bool, bool) {
        let sj = said(e, &j, &v, seq, who, body.clone(), T0);
        let sr = said(e, &r, &v, seq, who, body, T0);
        (started(&sj).is_some(), started(&sr).is_some())
    };
    assert_eq!(both(&mut e, 1, "id:paul", json!({ "text": "hello all" })), (true, false), "the lead answers");
    e.step(Input::Runtime(Event::End { turn: records::turn_id(&j.fragment, "talk.paul", "chat", 1), outcome: Outcome::Idle }), T0);
    assert_eq!(both(&mut e, 2, "id:paul", json!({ "text": "@Rowan what do you think?" })), (false, true), "a mention picks rowan");
    e.step(Input::Runtime(Event::End { turn: records::turn_id(&r.fragment, "talk.paul", "chat", 2), outcome: Outcome::Idle }), T0);
    assert_eq!(both(&mut e, 3, "id:paul", json!({ "text": "you two", "to": ["id:juniper", "id:rowan"] })), (true, true), "to names both");

    // juniper hands off to rowan
    let jt = records::turn_id(&j.fragment, "talk.paul", "chat", 3);
    e.step(Input::Runtime(Event::End { turn: records::turn_id(&r.fragment, "talk.paul", "chat", 3), outcome: Outcome::Idle }), T0);
    e.step(Input::Runtime(Event::Reply { turn: jt.clone(), part: 1, text: "@rowan can take this".into() }), T0);
    let end = e.step(Input::Runtime(Event::End { turn: jt.clone(), outcome: Outcome::Idle }), T0);
    let reply = posts(&end)[0].1.clone();
    assert_eq!(reply["to"], json!(["id:rowan"]));
    assert_eq!(reply["hop"], 1);
    let (byj, byr) = both(&mut e, 4, "id:juniper", reply);
    assert_eq!((byj, byr), (false, true), "rowan takes the hand-off; juniper skips its own reply");
    assert_eq!(e.state().turns.values().find(|t| t.agent == r.fragment && t.phase != Phase::Ended).expect("rowan's").hop, 1);
    // too deep: an agent's message past HOPS_MAX starts nothing
    let deep = json!({ "text": "@juniper again", "turn": "x", "to": ["id:juniper"], "hop": limits::HOPS_MAX + 1 });
    assert!(started(&said(&mut e, &j, &v, 5, "id:rowan", deep, T0)).is_none());
    // an agent's message that names no one starts nothing, even for the lead
    assert!(started(&said(&mut e, &j, &v, 6, "id:rowan", json!({ "text": "done", "turn": "y" }), T0)).is_none());
}

/// Goal: two agents on one computer, in two chats, run at once.
#[test]
fn two_agents_run_at_once() {
    let (j, r) = (agent("juniper"), agent("rowan"));
    let mut e = engine(&[j.clone(), r.clone()]);
    let s1 = said(&mut e, &j, &view(&[&j]), 1, "id:paul", json!({ "text": "a" }), T0);
    let s2 = e.step(Input::Record { agent: r.fragment.clone(), fragment: "notes.paul".into(), record: rec(1, "id:paul", json!({ "text": "b" })), view: Some(view(&[&r])), since: 0 }, T0);
    let s2 = answered(&mut e, s2, T0);
    let (t1, t2) = (started(&s1).expect("juniper"), started(&s2).expect("rowan"));
    assert_ne!(t1.turn, t2.turn);
    assert_eq!((t1.agent.fragment.as_str(), t2.agent.fragment.as_str()), ("juniper.paul", "rowan.paul"));
}

/// Goal: a routine on the agent's `tasks` channel is a turn in its chat, as
/// its owner; `joined` asks for a new listing.
#[test]
fn tasks_start_routines() {
    let a = agent("juniper");
    let mut e = engine(std::slice::from_ref(&a));
    let task = |seq, body| Record { channel: "tasks".into(), seq, at: 0, principal: "id:paul".into(), kind: "message".into(), body };
    let s = e.step(Input::Record { agent: a.fragment.clone(), fragment: a.fragment.clone(), record: task(1, json!({ "kind": "routine", "text": "water the plants", "chat": "talk.paul" })), view: None, since: 0 }, T0);
    let s = answered(&mut e, s, T0);
    let t = started(&s).expect("a routine turn");
    assert!(t.routine);
    assert_eq!((t.fragment.as_str(), t.asker.as_str()), ("talk.paul", "id:paul"));
    assert_eq!(t.turn, records::turn_id(&a.fragment, &a.fragment, "tasks", 1));
    let j = e.step(Input::Record { agent: a.fragment.clone(), fragment: a.fragment.clone(), record: task(2, json!({ "kind": "joined", "fragment": "new.paul" })), view: None, since: 0 }, T0);
    assert!(j.effects.contains(&Effect::Discover { agent: a.fragment.clone(), joined: Some("new.paul".into()) }));
    // a `tasks` record on another fragment is no task
    let other = e.step(Input::Record { agent: a.fragment.clone(), fragment: "talk.paul".into(), record: task(3, json!({ "kind": "joined" })), view: None, since: 0 }, T0);
    assert!(other.effects.is_empty());
}

/// Goal: a turn the runtime goes quiet on ends as an error; one that waits
/// on a person does not.
#[test]
fn a_quiet_turn_ends() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "x" }), T0)).expect("started").turn;
    assert!(e.step(Input::Tick, T0 + 600_000).effects.is_empty());
    let t = e.step(Input::Tick, T0 + 600_001);
    assert_eq!(commands(&t)[0], Command::Forget { turn: turn.clone() });
    assert_eq!(posts(&t).last().expect("its end").1["outcome"], "error");
}

/// Goal: an agent removed from a chat drops its turns there.
#[test]
fn gone_drops_turns() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "x" }), T0)).expect("started").turn;
    let g = e.step(Input::Gone { agent: a.fragment.clone(), fragment: "talk.paul".into() }, T0);
    assert_eq!(commands(&g), vec![Command::Forget { turn }]);
    assert!(posts(&g).is_empty(), "it can no longer post there");
    assert!(e.state().turns.is_empty());
}

/// Goal: what was said before the agent joined is history: it passes the
/// cursor and starts nothing; what was said since starts a turn.
#[test]
fn history_is_not_for_a_new_agent() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let at = |seq: u64, at: i64| Record { at, ..rec(seq, "id:paul", json!({ "text": "hi @juniper" })) };
    let old = e.step(Input::Record { agent: a.fragment.clone(), fragment: "talk.paul".into(), record: at(1, 500), view: Some(v.clone()), since: 1000 }, T0);
    assert!(started(&old).is_none());
    assert!(old.dirty, "its cursor still moves");
    assert_eq!(e.cursor(&a.fragment, "talk.paul", "chat"), 1);
    let new = e.step(Input::Record { agent: a.fragment.clone(), fragment: "talk.paul".into(), record: at(2, 1000), view: Some(v), since: 1000 }, T0);
    let new = answered(&mut e, new, T0);
    assert!(started(&new).is_some());
}

/// Goal: an attachment rides on its reply part; a message's attachments are
/// handed with its turn.
#[test]
fn attachments_ride_along() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let sha = "b".repeat(64);
    let s = said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "see", "attachments": [{ "sha256": sha, "size": 4, "type": "image/png", "name": "a.png" }] }), T0);
    let t = started(&s).expect("started");
    assert_eq!(t.attachments.len(), 1);
    let file = LocalFile { path: "/tmp/x.png".into(), media_type: "image/png".into(), name: "x.png".into(), size: 4 };
    ev(&mut e, Event::Attachment { turn: t.turn.clone(), part: 1, file: file.clone() }, T0);
    let end = ev(&mut e, Event::End { turn: t.turn.clone(), outcome: Outcome::Idle }, T0);
    let files = end.effects.iter().find_map(|e| match e { Effect::Post { files, .. } if !files.is_empty() => Some(files.clone()), _ => None });
    assert_eq!(files, Some(vec![file]));
}

/// Goal: a turn that asks its asker something in words (Hermes' open
/// clarify) shows the question at once, and the asker's next message is
/// handed to that turn as its answer, never queued behind it (where the
/// turn would wait for it forever). Invalid: someone else's message, an
/// empty one, or one to another agent is not the answer. Replay: the
/// message again (behind the cursor) tells nothing twice. Restart: the
/// turn ends as lost, as any running one does (P1; across lives,
/// `a_question_cut_by_a_restart_is_lost_and_its_answer_runs_once`).
#[test]
fn a_question_is_answered_by_the_next_message() {
    let a = agent("juniper");
    let b = agent("rowan");
    let v = view(&[&a, &b]);
    let mut e = engine(&[a.clone(), b.clone()]);
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "keep my garden notes" }), T0)).expect("started").turn;
    ev(&mut e, Event::Reply { turn: turn.clone(), part: 1, text: "What do you plant?".into() }, T0 + 1);
    let asked = ev(&mut e, Event::Asked { turn: turn.clone() }, T0 + 2);
    assert_eq!(posts(&asked), vec![(records::reply_id(&turn, 1), json!({ "text": "What do you plant?", "turn": turn }))], "the question shows at once");
    assert!(asked.dirty);
    assert!(e.state().turns[&turn].asking);
    assert_eq!(keepalive(&asked), None, "still running: the computer stays up");

    // invalid: another person's message, an empty one, one to the other agent
    let skyler = said(&mut e, &a, &v, 2, "id:skyler", json!({ "text": "tomatoes?" }), T0 + 3);
    assert!(commands(&skyler).is_empty(), "queued behind it, as any message is");
    let empty = said(&mut e, &a, &v, 3, "id:paul", json!({ "text": " ", "attachments": [{ "sha256": "c".repeat(64), "size": 1, "type": "image/png", "name": "p.png" }] }), T0 + 4);
    assert!(!commands(&empty).iter().any(|c| matches!(c, Command::Tell { .. })), "no words, no answer");
    let other = said(&mut e, &b, &v, 4, "id:paul", json!({ "text": "@rowan hi" }), T0 + 5);
    assert!(!commands(&other).iter().any(|c| matches!(c, Command::Tell { .. })));
    let to_rowan = said(&mut e, &a, &v, 4, "id:paul", json!({ "text": "@rowan hi" }), T0 + 5);
    assert!(commands(&to_rowan).is_empty(), "not addressed to juniper");

    // valid: the asker's next message is the answer
    let told = said(&mut e, &a, &v, 5, "id:paul", json!({ "text": "Tomatoes, at dawn" }), T0 + 6);
    assert_eq!(commands(&told), vec![Command::Tell { turn: turn.clone(), seq: 5, by: "id:paul".into(), by_name: "paul".into(), text: "Tomatoes, at dawn".into() }]);
    assert!(posts(&told).is_empty(), "no turn of its own");
    assert!(told.dirty && !e.state().turns[&turn].asking);
    // replay: behind the cursor, nothing
    assert_eq!(said(&mut e, &a, &v, 5, "id:paul", json!({ "text": "Tomatoes, at dawn" }), T0 + 7), Step::default());
    // asked no longer, the next message queues as before
    let next = said(&mut e, &a, &v, 6, "id:paul", json!({ "text": "and basil" }), T0 + 8);
    assert!(commands(&next).is_empty() && started(&next).is_none(), "queued");
    let end = ev(&mut e, Event::End { turn: turn.clone(), outcome: Outcome::Idle }, T0 + 9);
    let queued: Vec<String> = commands(&end).iter().filter_map(|c| match c { Command::Start(t) => Some(t.text.clone()), _ => None }).collect();
    assert_eq!(queued, vec!["tomatoes?".to_string()], "skyler's message runs next; then paul's empty one, then 'and basil'");

    // the state round-trips with a turn asking, and a restart ends it as
    // lost: a new life never tells a turn an earlier life ran
    let t2 = started(&said(&mut e, &a, &v, 7, "id:paul", json!({ "text": "x" }), T0 + 10));
    assert!(t2.is_none(), "behind the others");
    let skylers = records::turn_id(&a.fragment, "talk.paul", "chat", 2);
    ev(&mut e, Event::Reply { turn: skylers.clone(), part: 1, text: "Which tomatoes?".into() }, T0 + 11);
    ev(&mut e, Event::Asked { turn: skylers.clone() }, T0 + 11);
    let saved: State = serde_json::from_str(&serde_json::to_string(e.state()).expect("serializes")).expect("deserializes");
    assert_eq!(&saved, e.state());
    assert!(saved.turns[&skylers].asking, "kept asking");
    let mut e2 = Engine::new(saved, Settings::default(), "fedcba9876543210fedcba9876543210").expect("whole");
    e2.step(Input::Agents(vec![a.clone(), b.clone()]), T0 + 12);
    let r = e2.recover(T0 + 12);
    assert!(posts(&r).contains(&(records::work_id(&skylers, "end"), json!({ "kind": "turn.end", "turn": skylers, "outcome": "error", "error": LOST }))), "ended as lost: {:?}", posts(&r));
    assert!(!e2.state().turns[&skylers].asking, "over, it asks nothing");
    e2.step(Input::Runtime(Event::Connected(true)), T0 + 12);
    let answer = said(&mut e2, &a, &v, 8, "id:skyler", json!({ "text": "cherry ones" }), T0 + 13);
    assert!(!commands(&answer).iter().any(|c| matches!(c, Command::Tell { .. })), "never told to a turn of another life");
}

/// Goal: a turn waiting on its asker's words outlasts the idle bound up to
/// a prompt's lifetime, then ends as quiet; Stop still reaches it.
#[test]
fn a_question_waits_as_long_as_a_prompt() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = Engine::new(State::default(), Settings { prompt_ttl_ms: 3_600_000, turn_idle_ms: 600_000 }, LIFE).expect("a fresh state");
    e.step(Input::Agents(vec![a.clone()]), T0);
    e.recover(T0);
    e.step(Input::Runtime(Event::Connected(true)), T0);
    let turn = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "hi" }), T0)).expect("started").turn;
    ev(&mut e, Event::Reply { turn: turn.clone(), part: 1, text: "Which one?".into() }, T0);
    ev(&mut e, Event::Asked { turn: turn.clone() }, T0);
    assert!(commands(&e.step(Input::Tick, T0 + 600_001)).is_empty(), "past the idle bound, still asking");
    let quiet = e.step(Input::Tick, T0 + 3_600_001);
    assert_eq!(commands(&quiet), vec![Command::Forget { turn: turn.clone() }]);
    assert_eq!(posts(&quiet).last().expect("its end").1["outcome"], "error");

    let t2 = started(&said(&mut e, &a, &v, 2, "id:paul", json!({ "text": "again" }), T0 + 3_600_002)).expect("started").turn;
    ev(&mut e, Event::Asked { turn: t2.clone() }, T0 + 3_600_003);
    let stop = said(&mut e, &a, &v, 3, "id:paul", json!({ "kind": "stop" }), T0 + 3_600_004);
    assert_eq!(commands(&stop), vec![Command::Stop { turn: t2 }]);
    // the Stop answered the question (Relay says "Stop." to it): the
    // asker's next message is a turn of its own, queued behind it
    let after = said(&mut e, &a, &v, 4, "id:paul", json!({ "text": "never mind" }), T0 + 3_600_005);
    assert!(commands(&after).is_empty() && !e.state().turns.values().any(|t| t.asking), "queued, not told");
}

// ---- agents asking each other: the hop the bridge counts, the budget ----

fn rec_at(seq: u64, at: i64, principal: &str, body: Value) -> Record {
    Record { at, ..rec(seq, principal, body) }
}

/// One record of a chat read by each agent's follower there, as the driver
/// feeds it: the turns it started, with their hops.
fn to_all(e: &mut Engine, agents: &[&Agent], v: &ChatView, chat: &str, record: Record, now: u64) -> Vec<(TurnStart, u32)> {
    let mut out = Vec::new();
    for a in agents {
        let s = e.step(Input::Record { agent: a.fragment.clone(), fragment: chat.into(), record: record.clone(), view: Some(v.clone()), since: 0 }, now);
        if let Some(t) = started(&answered(e, s, now)) {
            let hop = e.state().turns[&t.turn].hop;
            out.push((t, hop));
        }
    }
    out
}

/// The turn says `text` and ends: the reply it posts, its end answered (so
/// the turn is let go before anyone reads the reply).
fn reply_and_end(e: &mut Engine, turn: &str, text: &str, now: u64) -> Value {
    ev(e, Event::Reply { turn: turn.into(), part: 1, text: text.into() }, now);
    let end = ev(e, Event::End { turn: turn.into(), outcome: Outcome::Idle }, now);
    assert!(!e.state().turns.contains_key(turn), "let go once its end was answered");
    posts(&end).into_iter().find(|(id, _)| id.starts_with("rp:")).expect("its reply").1
}

/// Two agents told to hand off to each other until something stops them:
/// the turns they ran, with their hops. Through the bridge, each turn's
/// reply names the other (read after its turn was let go); `around`, each
/// turn posts its hand-off itself while it runs, as the CLI or the API
/// does: no `hop`, no `turn`.
fn ping_pong(around: bool) -> Vec<(String, u32)> {
    let (j, r) = (agent("juniper"), agent("rowan"));
    let v = view(&[&j, &r]);
    let mut e = engine(&[j.clone(), r.clone()]);
    let mut ran = Vec::new();
    let mut next = to_all(&mut e, &[&j, &r], &v, "talk.paul", rec_at(1, 10, "id:paul", json!({ "text": "keep handing off", "to": ["id:juniper"] })), T0);
    // bounded: each pass reads one more record, and the hop cap ends it
    for seq in 2..20u64 {
        let Some((t, hop)) = next.pop() else { break };
        assert!(next.is_empty(), "one turn a record");
        ran.push((t.agent.name.clone(), hop));
        let other = if t.agent.name == "juniper" { "rowan" } else { "juniper" };
        let by = format!("id:{}", t.agent.name);
        let text = format!("over to you @{other}");
        next = if around {
            let posted = json!({ "text": text, "to": [format!("id:{other}")] });
            let started = to_all(&mut e, &[&j, &r], &v, "talk.paul", rec_at(seq, 10 + seq as i64, &by, posted), T0 + seq);
            ev(&mut e, Event::End { turn: t.turn.clone(), outcome: Outcome::Idle }, T0 + seq);
            started
        } else {
            let reply = reply_and_end(&mut e, &t.turn, &text, T0 + seq);
            assert_eq!(reply["to"], json!([format!("id:{other}")]), "a mention of the other agent hands off");
            to_all(&mut e, &[&j, &r], &v, "talk.paul", rec_at(seq, 10 + seq as i64, &by, reply), T0 + seq)
        };
    }
    ran
}

/// A turn just ended counts only for the reply that names it: an agent
/// handed back to in a chat of two, its turn there ended, then asked again
/// by its person elsewhere, asks the other with the CLI from that new turn
/// at one hop, not one past the turn it finished. Invalid: a post naming
/// the ended turn (a reply read late, or a forgery while it runs no turn)
/// counts from it.
#[test]
fn an_ended_turn_counts_only_for_the_reply_that_names_it() {
    let (j, r) = (agent("juniper"), agent("rowan"));
    let v = view(&[&j, &r]);
    let mut e = engine(&[j.clone(), r.clone()]);
    let (talk, pair) = ("talk.paul", "juniper-rowan.paul");
    // in the pair chat: the person to juniper, juniper to rowan, rowan back to juniper
    let j0 = to_all(&mut e, &[&j, &r], &v, pair, rec_at(1, 10, "id:paul", json!({ "text": "start", "to": ["id:juniper"] })), T0);
    let to_r = reply_and_end(&mut e, &j0[0].0.turn, "@rowan yours", T0 + 1);
    let r1 = to_all(&mut e, &[&j, &r], &v, pair, rec_at(2, 11, "id:juniper", to_r), T0 + 2);
    let to_j = reply_and_end(&mut e, &r1[0].0.turn, "@juniper back to you", T0 + 3);
    let j2 = to_all(&mut e, &[&j, &r], &v, pair, rec_at(3, 12, "id:rowan", to_j), T0 + 4);
    assert_eq!(j2[0].1, 2);
    let j2_turn = j2[0].0.turn.clone();
    reply_and_end(&mut e, &j2_turn, "thanks", T0 + 5);
    // its person asks it again in talk: a turn at hop 0 there
    let t0 = to_all(&mut e, &[&j, &r], &v, talk, rec_at(1, 13, "id:paul", json!({ "text": "ask rowan again", "to": ["id:juniper"] })), T0 + 6);
    assert_eq!(t0[0].1, 0);
    // from it, `fragment ask` into the pair chat: one hop, not three
    let asked = to_all(&mut e, &[&j, &r], &v, pair, rec_at(4, 14, "id:juniper", json!({ "text": "and now?", "to": ["id:rowan"] })), T0 + 7);
    assert_eq!(asked.iter().map(|(t, h)| (t.agent.name.as_str(), *h)).collect::<Vec<_>>(), vec![("rowan", 1)]);
    e.step(Input::Runtime(Event::End { turn: asked[0].0.turn.clone(), outcome: Outcome::Idle }), T0 + 8);
    e.step(Input::Runtime(Event::End { turn: t0[0].0.turn.clone(), outcome: Outcome::Idle }), T0 + 8);
    // naming the ended hop-2 turn (in no turn now) counts from it
    let named = to_all(&mut e, &[&j, &r], &v, pair, rec_at(5, 15, "id:juniper", json!({ "text": "late", "to": ["id:rowan"], "turn": j2_turn })), T0 + 9);
    assert_eq!(named[0].1, limits::HOPS_MAX, "one past the turn it names");
}

/// Goal (decision 8): two agents that hand off to each other stop at the
/// hop cap: A, B, A, B, and the fourth hand-off starts nothing. Invalid:
/// the same loop posted around the bridge (no `hop`, no `turn`: the CLI,
/// the API) stops at the same place, because the answering bridge counts
/// the hops from the turns it runs, not from what a record claims.
#[test]
fn a_hand_off_loop_stops_at_the_cap() {
    let want: Vec<(String, u32)> = vec![("juniper".into(), 0), ("rowan".into(), 1), ("juniper".into(), 2), ("rowan".into(), limits::HOPS_MAX)];
    assert_eq!(ping_pong(false), want, "through the bridge");
    assert_eq!(ping_pong(true), want, "around it: a reset that no longer resets");
}

/// Goal: a post an agent of this computer makes around the bridge counts
/// from the turn it is in. Valid: from its turn here, one past it; from a
/// turn in another chat (`fragment ask` into a chat of two agents), one past
/// that. Invalid: a claimed `hop: 0` from deep in a chain changes nothing,
/// and a claim deeper than the bridge's count is kept. Out of any turn
/// (something it left running), it is the last hop: answered once, and the
/// answer hands on nothing.
#[test]
fn a_post_around_the_bridge_counts_from_its_turn() {
    let (j, r) = (agent("juniper"), agent("rowan"));
    let v = view(&[&j, &r]);
    let mut e = engine(&[j.clone(), r.clone()]);
    let talk = "talk.paul";
    let pair = "juniper-rowan.paul";
    // juniper runs a turn in talk at hop 0 (its person asked)
    let t = to_all(&mut e, &[&j, &r], &v, talk, rec_at(1, 10, "id:paul", json!({ "text": "ask rowan for me", "to": ["id:juniper"] })), T0);
    let jt = t[0].0.turn.clone();
    // ... and asks rowan in their own chat with the CLI: no hop, no turn
    let asked = to_all(&mut e, &[&j, &r], &v, pair, rec_at(1, 11, "id:juniper", json!({ "text": "what's the weather?", "to": ["id:rowan"] })), T0 + 1);
    assert_eq!(asked.len(), 1);
    assert_eq!((asked[0].0.agent.name.as_str(), asked[0].1), ("rowan", 1), "one past juniper's turn in talk");
    // a claim of hop 0 from a deep turn resets nothing
    e.step(Input::Runtime(Event::End { turn: jt, outcome: Outcome::Idle }), T0 + 2);
    let rt = asked[0].0.turn.clone();
    reply_and_end(&mut e, &rt, "sunny", T0 + 3);
    let deep = to_all(&mut e, &[&j, &r], &v, pair, rec_at(2, 12, "id:paul", json!({ "text": "@juniper and @rowan, chat", "to": ["id:juniper", "id:rowan"] })), T0 + 4);
    assert_eq!(deep.iter().map(|(t, h)| (t.agent.name.as_str(), *h)).collect::<Vec<_>>(), vec![("juniper", 0), ("rowan", 0)], "a person's message is hop 0");
    // drive juniper to hop 3 in the pair chat: rowan's turn hands to juniper twice
    let (jt0, rt0) = (deep[0].0.turn.clone(), deep[1].0.turn.clone());
    e.step(Input::Runtime(Event::End { turn: jt0, outcome: Outcome::Idle }), T0 + 5);
    let to_j = reply_and_end(&mut e, &rt0, "@juniper yours", T0 + 6);
    let j1 = to_all(&mut e, &[&j, &r], &v, pair, rec_at(3, 13, "id:rowan", to_j), T0 + 7);
    assert_eq!(j1[0].1, 1);
    let to_r = reply_and_end(&mut e, &j1[0].0.turn, "@rowan back", T0 + 8);
    let r2 = to_all(&mut e, &[&j, &r], &v, pair, rec_at(4, 14, "id:juniper", to_r), T0 + 9);
    assert_eq!(r2[0].1, 2);
    ev(&mut e, Event::Reply { turn: r2[0].0.turn.clone(), part: 1, text: "@juniper last".into() }, T0 + 10);
    let end = ev(&mut e, Event::End { turn: r2[0].0.turn.clone(), outcome: Outcome::Idle }, T0 + 10);
    let to_j = posts(&end).into_iter().find(|(id, _)| id.starts_with("rp:")).expect("its reply").1;
    let j3 = to_all(&mut e, &[&j, &r], &v, pair, rec_at(5, 15, "id:rowan", to_j), T0 + 11);
    assert_eq!(j3[0].1, limits::HOPS_MAX);
    // juniper, in its hop-3 turn, posts to rowan with the API, claiming 0
    let reset = to_all(&mut e, &[&j, &r], &v, pair, rec_at(6, 16, "id:juniper", json!({ "text": "again?", "to": ["id:rowan"], "hop": 0 })), T0 + 12);
    assert!(reset.is_empty(), "past the cap whatever it claims: {reset:?}");
    // a claim deeper than the count is kept
    let claimed = to_all(&mut e, &[&j, &r], &v, pair, rec_at(7, 17, "id:juniper", json!({ "text": "deep", "to": ["id:rowan"], "hop": 9 })), T0 + 13);
    assert!(claimed.is_empty());
    e.step(Input::Runtime(Event::End { turn: j3[0].0.turn.clone(), outcome: Outcome::Idle }), T0 + 14);

    // in no turn, long after its last one here: the last hop, once
    let later = T0 + 14 + limits::ENDED_HOPS_MS + 1;
    let stray = to_all(&mut e, &[&j, &r], &v, pair, rec_at(8, 18, "id:juniper", json!({ "text": "from a loop I left running", "to": ["id:rowan"] })), later);
    assert_eq!(stray.len(), 1);
    assert_eq!(stray[0].1, limits::HOPS_MAX, "answered once");
    let back = reply_and_end(&mut e, &stray[0].0.turn, "@juniper ok", later + 1);
    assert!(to_all(&mut e, &[&j, &r], &v, pair, rec_at(9, 19, "id:rowan", back), later + 2).is_empty(), "its answer hands on nothing");
}

/// Restart: what the bridge knew of its ended turns goes with its life, so
/// a reply the next life reads first is the last hop (answered, handing on
/// nothing), never a reset to the first.
#[test]
fn a_reply_read_by_the_next_life_resets_nothing() {
    let (j, r) = (agent("juniper"), agent("rowan"));
    let v = view(&[&j, &r]);
    let mut e = engine(&[j.clone(), r.clone()]);
    let t = to_all(&mut e, &[&j, &r], &v, "talk.paul", rec_at(1, 10, "id:paul", json!({ "text": "hi", "to": ["id:juniper"] })), T0);
    let reply = reply_and_end(&mut e, &t[0].0.turn, "@rowan over to you", T0 + 1);
    // juniper's follower read it (its own); the life ends before rowan's did
    e.step(Input::Record { agent: j.fragment.clone(), fragment: "talk.paul".into(), record: rec_at(2, 11, "id:juniper", reply.clone()), view: Some(v.clone()), since: 0 }, T0 + 2);
    let saved = e.state().clone();
    let mut e2 = Engine::new(saved, Settings::default(), "fedcba9876543210fedcba9876543210").expect("whole");
    e2.step(Input::Agents(vec![j.clone(), r.clone()]), T0 + 3);
    e2.recover(T0 + 3);
    e2.step(Input::Runtime(Event::Connected(true)), T0 + 3);
    let s = e2.step(Input::Record { agent: r.fragment.clone(), fragment: "talk.paul".into(), record: rec_at(2, 11, "id:juniper", reply), view: Some(v.clone()), since: 0 }, T0 + 4);
    let rt = started(&answered(&mut e2, s, T0 + 4)).expect("rowan answers it");
    assert_eq!(e2.state().turns[&rt.turn].hop, limits::HOPS_MAX);
}

/// Goal: a chat's agents start at most AGENT_TURNS_PER_CHAT_MAX turns of
/// each other in the window, and the one past it is refused with both its
/// records, its end saying why (the chat shows it). A person is never
/// counted, nor refused. Replay: a record read again spends nothing.
/// Restart: the count is kept. The window slides by the records' times.
#[test]
fn a_chats_agents_have_a_budget() {
    let (j, r) = (agent("juniper"), agent("rowan"));
    let v = view(&[&j, &r]);
    let mut e = engine(&[j.clone(), r.clone()]);
    let max = limits::AGENT_TURNS_PER_CHAT_MAX as u64;
    let at0 = 1_000_000i64;
    // rowan, in no turn, asks juniper again and again (each the last hop)
    let ask = |seq: u64, at: i64| rec_at(seq, at, "id:rowan", json!({ "text": "again", "to": ["id:juniper"] }));
    for seq in 1..=max {
        let s = to_all(&mut e, &[&j], &v, "talk.paul", ask(seq, at0 + seq as i64), T0 + seq);
        assert_eq!(s.len(), 1, "within the budget: {seq}");
        e.step(Input::Runtime(Event::End { turn: s[0].0.turn.clone(), outcome: Outcome::Idle }), T0 + seq);
    }
    assert_eq!(e.state().agent_turns["talk.paul"].len() as u64, max);
    // replay: the last one read again spends nothing
    assert!(to_all(&mut e, &[&j], &v, "talk.paul", ask(max, at0 + max as i64), T0 + max).is_empty());
    assert_eq!(e.state().agent_turns["talk.paul"].len() as u64, max);
    // restart: the count is kept
    let saved: State = serde_json::from_str(&serde_json::to_string(e.state()).expect("serializes")).expect("deserializes");
    let mut e = Engine::new(saved, Settings::default(), "fedcba9876543210fedcba9876543210").expect("whole");
    e.step(Input::Agents(vec![j.clone(), r.clone()]), T0 + 100);
    e.recover(T0 + 100);
    e.step(Input::Runtime(Event::Connected(true)), T0 + 100);
    // past it: refused, with both records, saying why
    let over = e.step(Input::Record { agent: j.fragment.clone(), fragment: "talk.paul".into(), record: ask(max + 1, at0 + max as i64 + 1), view: Some(v.clone()), since: 0 }, T0 + 101);
    let over = answered(&mut e, over, T0 + 101);
    assert!(started(&over).is_none());
    assert_eq!(kinds(&over), vec!["turn.start", "turn.end"]);
    assert_eq!(posts(&over)[1].1["error"], refused_budget());
    // a person is never counted, nor refused
    let person = to_all(&mut e, &[&j], &v, "talk.paul", rec_at(max + 2, at0 + max as i64 + 2, "id:paul", json!({ "text": "still there?" })), T0 + 102);
    assert_eq!(person.len(), 1);
    assert_eq!(e.state().agent_turns["talk.paul"].len() as u64, max);
    e.step(Input::Runtime(Event::End { turn: person[0].0.turn.clone(), outcome: Outcome::Idle }), T0 + 102);
    // another chat has a budget of its own
    let other = to_all(&mut e, &[&j], &v, "other.paul", ask(1, at0 + max as i64 + 3), T0 + 103);
    assert_eq!(other.len(), 1);
    e.step(Input::Runtime(Event::End { turn: other[0].0.turn.clone(), outcome: Outcome::Idle }), T0 + 103);
    // the window slides: once the first is older than it, one more runs
    let later = at0 + 1 + limits::AGENT_TURNS_WINDOW_MS;
    let slid = to_all(&mut e, &[&j], &v, "talk.paul", ask(max + 3, later), T0 + 104);
    assert_eq!(slid.len(), 1, "one ran out of the window");
    assert_eq!(e.state().agent_turns["talk.paul"].len() as u64, max, "and is let go");
    assert!(e.state().agent_turns["talk.paul"].iter().all(|t| *t > later - limits::AGENT_TURNS_WINDOW_MS));
}

/// Invalid state: a budget out of order or past its bound, or a routine at
/// a hop, is refused at load.
#[test]
fn a_corrupt_budget_is_refused() {
    let mut s = State::default();
    s.agent_turns.insert("talk.paul".into(), vec![3, 2]);
    assert!(Engine::new(s, Settings::default(), LIFE).is_err(), "out of order");
    let mut s = State::default();
    s.agent_turns.insert("talk.paul".into(), (0..=limits::AGENT_TURNS_PER_CHAT_MAX as i64).collect());
    assert!(Engine::new(s, Settings::default(), LIFE).is_err(), "past the bound");
    let mut s = State::default();
    s.agent_turns.insert("talk.paul".into(), vec![]);
    assert!(Engine::new(s, Settings::default(), LIFE).is_err(), "an empty one is never kept");
    // a state from before the budget loads, with none
    let mut old = serde_json::to_value(State::default()).expect("serializes");
    old.as_object_mut().expect("an object").remove("agent_turns");
    let old: State = serde_json::from_value(old).expect("an earlier bridge's state loads");
    assert!(old.agent_turns.is_empty());
}

/// Goal: an agent's `tasks` hears its own fragment (its cron) and its
/// owner. Invalid: another agent, acting for the owner (who may post there),
/// starts no routine and announces no join.
#[test]
fn tasks_hear_only_the_owner_and_the_fragment() {
    let a = agent("juniper");
    let mut e = engine(std::slice::from_ref(&a));
    let task = |seq, principal: &str, body| Record { channel: "tasks".into(), seq, at: 0, principal: principal.into(), kind: "message".into(), body };
    let routine = json!({ "kind": "routine", "text": "water the plants", "chat": "talk.paul" });
    let feed = |e: &mut Engine, r: Record| {
        let s = e.step(Input::Record { agent: a.fragment.clone(), fragment: a.fragment.clone(), record: r, view: None, since: 0 }, T0);
        answered(e, s, T0)
    };
    let cron = feed(&mut e, task(1, "npub1juniperfragmentkey", routine.clone()));
    let t = started(&cron).expect("its cron's routine runs");
    assert_eq!(t.asker, "id:paul", "asked by its owner");
    e.step(Input::Runtime(Event::End { turn: t.turn, outcome: Outcome::Idle }), T0);
    let rowan = feed(&mut e, task(2, "id:rowan", routine.clone()));
    assert!(rowan.effects.is_empty(), "another agent starts no routine: {:?}", rowan.effects);
    assert_eq!(e.cursor(&a.fragment, &a.fragment, "tasks"), 2, "the cursor passes it");
    let joined = feed(&mut e, task(3, "id:rowan", json!({ "kind": "joined", "fragment": "x.paul" })));
    assert!(joined.effects.is_empty());
    let owner = feed(&mut e, task(4, "id:paul", routine));
    assert!(started(&owner).is_some(), "its owner's runs");
}

// ---- across lives (docs/explorations/pi-durable.md, rung 1) ----
//
// The engine inside a model of what is around it: the platform's channels
// with their replay and 409 rule (the journal), the lane that posts to them
// in order, the runtime, and every state the engine ever persisted (a save
// may be any of them). A crash drops the engine, its lane and its runtime;
// a new life starts from any state saved, or from none. Runs
// (`Command::Start`) are counted, never records: a second run's records
// are replays of the first's.

const CHAT: &str = "talk.paul";

/// The platform's answer to a post (docs/api.md: the same id and body again
/// is a replay; another body, or another channel, is 409).
#[derive(Debug, Clone, Copy, PartialEq)]
enum Answer {
    Appended,
    Replayed,
    Conflict,
}

/// The platform's channels: each one's records in order, and posts by id
/// (with the seq of the record each one made).
#[derive(Debug, Default)]
struct Journal {
    records: BTreeMap<(String, String), Vec<Record>>,
    ids: HashMap<(String, String), (String, Value, u64)>,
}

impl Journal {
    fn append(&mut self, fragment: &str, channel: &str, principal: &str, body: Value) -> u64 {
        let list = self.records.entry((fragment.to_string(), channel.to_string())).or_default();
        let seq = list.len() as u64 + 1;
        list.push(Record { channel: channel.to_string(), seq, at: 0, principal: principal.to_string(), kind: "message".into(), body });
        seq
    }

    fn post(&mut self, fragment: &str, channel: &str, principal: &str, id: &str, body: &Value) -> Answer {
        let key = (fragment.to_string(), id.to_string());
        match self.ids.get(&key) {
            Some((c, b, _)) if c == channel && b == body => Answer::Replayed,
            Some(_) => Answer::Conflict,
            None => {
                let seq = self.append(fragment, channel, principal, body.clone());
                self.ids.insert(key, (channel.to_string(), body.clone(), seq));
                Answer::Appended
            }
        }
    }

    /// The seq of the record a post made (the platform's answer names it).
    fn seq_of(&self, fragment: &str, id: &str) -> Option<u64> {
        self.ids.get(&(fragment.to_string(), id.to_string())).map(|(_, _, seq)| *seq)
    }

    /// The note a turn claimed at `claim` (a seq on `work`) carries, read
    /// from the journal as the driver reads it (driver.rs, `note_for`): the
    /// cut turn it tells of, and its text.
    fn note(&self, agent: &str, claim: Option<u64>) -> Option<(String, String)> {
        let work = self.channel(CHAT, records::WORK);
        let before = &work[..usize::try_from(claim? - 1).expect("a seq")];
        let crate::note::Before::Cut(cut) = crate::note::previous(before, agent, true) else { return None };
        let chat = self.channel(CHAT, records::CHAT);
        let asked = chat.iter().find(|r| r.seq == cut.cause.seq).and_then(crate::note::asked);
        Some((cut.turn.clone(), crate::note::text(&cut, asked.as_deref(), &crate::note::replies(chat, agent, &cut.turn))))
    }

    fn channel(&self, fragment: &str, channel: &str) -> &[Record] {
        self.records.get(&(fragment.to_string(), channel.to_string())).map(Vec::as_slice).unwrap_or(&[])
    }

    /// A turn's records of `kind` on the chat's `work`.
    fn work(&self, turn: &str, kind: &str) -> Vec<Value> {
        self.channel(CHAT, records::WORK).iter().map(|r| r.body.clone()).filter(|b| b["turn"] == turn && b["kind"] == kind).collect()
    }
}

/// A turn the model runtime holds: the prompt it waits on, whether it asked
/// its asker in words and waits for their answer (`Event::Asked`), whether
/// its asker pressed Stop, and the reply parts it has said.
#[derive(Debug, Default, Clone)]
struct Held {
    prompted: Option<String>,
    asking: bool,
    stop: bool,
    parts: u32,
}

/// A message the engine told a turn as its answer (`Command::Tell`): the
/// life that told it, the turn, the message's seq, and, at that moment, the
/// turn's claim on `work` and whether this life's runtime held the turn.
#[derive(Debug, Clone)]
struct Told {
    life: u64,
    turn: String,
    seq: u64,
    claim: Option<Value>,
    held: bool,
}

struct World {
    agents: Vec<Agent>,
    view: ChatView,
    journal: Journal,
    engine: Option<Engine>,
    lives: u64,
    /// Every state the engine persisted, in order.
    saves: Vec<State>,
    /// The lane: posts not yet answered, in order (one chat, one lane).
    lane: VecDeque<Effect>,
    /// Each run: the life that ran it, and its turn.
    runs: Vec<(u64, String)>,
    /// At each run, the turn's `turn.start` on `work` as it stood then.
    claims_at_run: Vec<(u64, String, Option<Value>)>,
    /// At each run, the note it was handed (P5), as the driver reads it from
    /// the journal before the claim the engine names: the cut turn it tells
    /// of, and its text.
    notes: Vec<(String, Option<(String, String)>)>,
    held: BTreeMap<String, Held>,
    /// Each message told to a turn as its answer, in order.
    tells: Vec<Told>,
    /// Posts on `chat` the platform answered 409: one id, two bodies.
    conflicts_on_chat: Vec<String>,
    /// The ids of the posts a crash took from the lane before they were sent.
    dropped: Vec<String>,
    now: u64,
}

impl World {
    fn new(labels: &[&str]) -> World {
        let agents: Vec<Agent> = labels.iter().map(|l| agent(l)).collect();
        let view = view(&agents.iter().collect::<Vec<&Agent>>());
        World {
            agents,
            view,
            journal: Journal::default(),
            engine: None,
            lives: 0,
            saves: Vec::new(),
            lane: VecDeque::new(),
            runs: Vec::new(),
            claims_at_run: Vec::new(),
            notes: Vec::new(),
            held: BTreeMap::new(),
            tells: Vec::new(),
            conflicts_on_chat: Vec::new(),
            dropped: Vec::new(),
            now: T0,
        }
    }

    fn lead(&self) -> &Agent {
        &self.agents[0]
    }

    /// Life `n`'s id, as a bridge process makes its own (128 bits, hex).
    fn life_of(n: u64) -> String {
        format!("{n:032x}")
    }

    /// This life's id.
    fn life(&self) -> String {
        World::life_of(self.lives)
    }

    fn alive(&self) -> bool {
        self.engine.is_some()
    }

    fn engine(&self) -> &Engine {
        self.engine.as_ref().expect("a live engine")
    }

    /// The state as the engine last persisted it.
    fn latest(&self) -> Option<State> {
        self.saves.last().cloned()
    }

    /// A new life from `state` (a save, or none), its runtime connected.
    fn start(&mut self, state: Option<State>) {
        self.start_unconnected(state);
        self.connect();
    }

    /// A new life whose runtime cannot take a turn yet.
    fn start_unconnected(&mut self, state: Option<State>) {
        assert!(!self.alive(), "one life at a time");
        self.lives += 1;
        let settings = Settings { prompt_ttl_ms: 60_000, turn_idle_ms: 600_000 };
        let life = self.life();
        self.engine = Some(Engine::new(state.unwrap_or_default(), settings, &life).expect("a saved state is whole"));
        self.step(Input::Agents(self.agents.clone()));
        let now = self.now;
        let s = self.engine.as_mut().expect("alive").recover(now);
        self.absorb(s);
        self.deliver();
    }

    /// Its runtime can take turns now.
    fn connect(&mut self) {
        self.step(Input::Runtime(Event::Connected(true)));
    }

    /// Its runtime can take no turn now (Hermes gone from its socket).
    fn disconnect(&mut self) {
        self.step(Input::Runtime(Event::Connected(false)));
    }

    /// The life ends where it is: the post in flight lands (`landed`) or
    /// not, and the rest of its lane and its runtime are gone with it.
    fn crash(&mut self, landed: bool) {
        assert!(self.engine.take().is_some(), "a live engine crashes");
        if landed {
            if let Some(e) = self.lane.pop_front() {
                self.land(&e);
            }
        }
        for e in self.lane.drain(..) {
            if let Effect::Post { id, .. } | Effect::Claim { id, .. } = e {
                self.dropped.push(id);
            }
        }
        self.held.clear();
    }

    fn step(&mut self, input: Input) {
        let now = self.now;
        let s = self.engine.as_mut().expect("a live engine").step(input, now);
        self.absorb(s);
    }

    /// A step's effects, carried out as the driver does: its state saved
    /// first, its posts onto the lane, its commands to the runtime.
    fn absorb(&mut self, s: Step) {
        if s.dirty {
            self.saves.push(self.engine().state().clone());
        }
        for e in s.effects {
            match e {
                Effect::Post { .. } | Effect::Claim { .. } | Effect::Owed { .. } => self.lane.push_back(e),
                Effect::Runtime(Command::Start(ts)) => {
                    let claim = self.journal.work(&ts.turn, "turn.start").into_iter().next();
                    self.runs.push((self.lives, ts.turn.clone()));
                    self.claims_at_run.push((self.lives, ts.turn.clone(), claim));
                    assert!(ts.note.is_none(), "the engine names the claim; the driver reads the note");
                    self.notes.push((ts.turn.clone(), self.journal.note(&ts.agent.identity, ts.claim_seq)));
                    self.held.insert(ts.turn.clone(), Held::default());
                }
                Effect::Runtime(Command::Stop { turn }) => {
                    if let Some(h) = self.held.get_mut(&turn) {
                        h.stop = true;
                    }
                }
                Effect::Runtime(Command::Answer { turn, .. }) => {
                    if let Some(h) = self.held.get_mut(&turn) {
                        h.prompted = None;
                    }
                }
                Effect::Runtime(Command::Forget { turn }) => {
                    self.held.remove(&turn);
                }
                Effect::Runtime(Command::Tell { turn, seq, .. }) => {
                    let claim = self.journal.work(&turn, "turn.start").into_iter().next();
                    let held = match self.held.get_mut(&turn) {
                        Some(h) => {
                            h.asking = false;
                            true
                        }
                        None => false,
                    };
                    self.tells.push(Told { life: self.lives, turn, seq, claim, held });
                }
                Effect::Draft { .. } | Effect::Keepalive(_) | Effect::Discover { .. } => {}
            }
        }
    }

    /// Every record of the chat the engine has not read, as its followers
    /// feed them (from its cursor).
    fn deliver(&mut self) {
        if !self.alive() {
            return;
        }
        for a in self.agents.clone() {
            let cursor = self.engine().cursor(&a.fragment, CHAT, records::CHAT);
            let unread: Vec<Record> = self.journal.channel(CHAT, records::CHAT).iter().filter(|r| r.seq > cursor).cloned().collect();
            for r in unread {
                self.step(Input::Record { agent: a.fragment.clone(), fragment: CHAT.into(), record: r, view: Some(self.view.clone()), since: 0 });
            }
        }
    }

    /// A lane's post or claim, landed: the platform's answer.
    fn land(&mut self, e: &Effect) -> Answer {
        let (agent, fragment, channel, id, body) = match e {
            Effect::Post { agent, fragment, channel, id, body, .. } => (agent, fragment, *channel, id, body),
            Effect::Claim { agent, fragment, id, body, .. } | Effect::Owed { agent, fragment, id, body, .. } => (agent, fragment, records::WORK, id, body),
            _ => panic!("the lane holds posts, claims and owed records: {e:?}"),
        };
        let principal = self.agents.iter().find(|a| &a.fragment == agent).map(|a| a.identity.clone()).expect("an agent's post");
        let answer = self.journal.post(fragment, channel, &principal, id, body);
        if channel == records::CHAT && answer == Answer::Conflict {
            self.conflicts_on_chat.push(id.clone());
        }
        answer
    }

    /// The engine is told a claim's answer (the driver's `Input::Claimed`),
    /// or that the lane is done with an owed record (`Input::Posted`).
    fn tell(&mut self, e: &Effect, answer: ClaimAnswer) {
        match e {
            Effect::Claim { turn, .. } => self.step(Input::Claimed { turn: turn.clone(), answer }),
            Effect::Owed { turn, id, .. } => self.step(Input::Posted { turn: turn.clone(), id: id.clone() }),
            _ => {}
        }
    }

    /// The lane's next post, answered; false when it holds none.
    fn answer_one(&mut self) -> bool {
        let Some(e) = self.lane.pop_front() else { return false };
        let seq = match &e {
            Effect::Claim { fragment, id, .. } => Some((fragment.clone(), id.clone())),
            _ => None,
        };
        let answer = match self.land(&e) {
            Answer::Appended | Answer::Replayed => ClaimAnswer::Ours { seq: seq.and_then(|(f, id)| self.journal.seq_of(&f, &id)) },
            Answer::Conflict => ClaimAnswer::Theirs,
        };
        self.tell(&e, answer);
        self.deliver();
        true
    }

    /// Every post answered, and every record read.
    fn answer_all(&mut self) {
        // bounded: each pass answers one post, and a world makes few
        for _ in 0..10_000 {
            if !self.answer_one() {
                return;
            }
        }
        panic!("a lane that never empties");
    }

    /// The lane gives up on its next claim: it never reaches the platform,
    /// and the engine hears it unanswered. Any other post the model's lane
    /// tries until it lands (the lane's own bound of tries is a property of
    /// every post, out of this model; a crash is in it).
    fn lose_one(&mut self) {
        match self.lane.front() {
            Some(Effect::Claim { .. }) => {
                let e = self.lane.pop_front().expect("a claim");
                self.tell(&e, ClaimAnswer::Unanswered);
            }
            Some(_) => {
                self.answer_one();
            }
            None => {}
        }
    }

    /// The lane's next post reaches the platform, and its answer is lost: a
    /// claim is heard unanswered (the next tick's claim will find it a
    /// replay); any other post stays at the lane's head, for its retry to
    /// find it a replay.
    fn land_unanswered_one(&mut self) {
        let Some(e) = self.lane.front().cloned() else { return };
        self.land(&e);
        if matches!(e, Effect::Claim { .. }) {
            self.lane.pop_front();
            self.tell(&e, ClaimAnswer::Unanswered);
        }
        self.deliver();
    }

    /// `who` says `body` in the chat, read at once: its seq.
    fn say_body(&mut self, who: &str, body: Value) -> u64 {
        let seq = self.journal.append(CHAT, records::CHAT, who, body);
        self.deliver();
        seq
    }

    /// `who` says `text`: the lead's turn of it.
    fn say(&mut self, who: &str, text: &str) -> String {
        let seq = self.say_body(who, json!({ "text": text }));
        self.turn_of(seq)
    }

    fn turn_of(&self, seq: u64) -> String {
        records::turn_id(&self.lead().fragment, CHAT, records::CHAT, seq)
    }

    /// The runtime ends a turn it holds: with an answer (idle), or without.
    fn end(&mut self, turn: &str, outcome: Outcome) {
        let h = self.held.remove(turn).unwrap_or_else(|| panic!("the runtime holds {turn}"));
        if outcome == Outcome::Idle {
            self.step(Input::Runtime(Event::Reply { turn: turn.into(), part: h.parts + 1, text: format!("an answer to {turn}") }));
        }
        self.step(Input::Runtime(Event::End { turn: turn.into(), outcome }));
    }

    /// The runtime asks its asker something to answer in words (Hermes'
    /// open clarify): the question is a reply part, and the turn waits,
    /// running, for their next message (`Command::Tell`).
    fn ask_in_words(&mut self, turn: &str) {
        let h = self.held.get_mut(turn).expect("held");
        h.asking = true;
        h.parts += 1;
        let part = h.parts;
        self.step(Input::Runtime(Event::Reply { turn: turn.into(), part, text: format!("a question of {turn}") }));
        self.step(Input::Runtime(Event::Asked { turn: turn.into() }));
    }

    /// Whether the message whose turn (were it one) is `turn` was told to a
    /// turn as its answer, in any life.
    fn was_told(&self, turn: &str) -> bool {
        self.tells.iter().any(|t| self.turn_of(t.seq) == turn)
    }

    /// The runtime asks its owner something; the turn waits.
    fn ask(&mut self, turn: &str) {
        let prompt = format!("p-{}", &turn[..12]);
        self.held.get_mut(turn).expect("held").prompted = Some(prompt.clone());
        let options = vec![PromptOption { id: "once".into(), label: "Allow once".into(), style: None }, PromptOption { id: "deny".into(), label: "Deny".into(), style: None }];
        self.step(Input::Runtime(Event::Prompt { turn: turn.into(), prompt, text: "ok?".into(), options, ttl_ms: None }));
    }

    fn tick(&mut self, ms: u64) {
        self.now += ms;
        if self.alive() {
            self.step(Input::Tick);
        }
    }

    /// The lead's runtime says something with no turn running.
    fn say_unasked(&mut self, text: &str) {
        let agent = self.lead().fragment.clone();
        self.step(Input::Runtime(Event::Say { agent, fragment: CHAT.into(), text: text.into() }));
    }

    fn runs_of(&self, turn: &str) -> usize {
        self.runs.iter().filter(|(_, t)| t == turn).count()
    }

    fn starts(&self, turn: &str) -> Vec<Value> {
        self.journal.work(turn, "turn.start")
    }

    fn ends(&self, turn: &str) -> Vec<Value> {
        self.journal.work(turn, "turn.end")
    }

    /// The note `turn` was handed at its run (it ran once).
    fn note_of(&self, turn: &str) -> Option<String> {
        let at: Vec<&Option<(String, String)>> = self.notes.iter().filter(|(t, _)| t == turn).map(|(_, n)| n).collect();
        assert_eq!(at.len(), 1, "{turn} ran once");
        at[0].as_ref().map(|(_, text)| text.clone())
    }

    /// A step of a turn the runtime holds, as the runtime reports it.
    fn did(&mut self, turn: &str, tool: &str, args: &str) {
        self.step(Input::Runtime(Event::Step { turn: turn.into(), step: ToolStep { tool: tool.into(), args: args.into(), ok: true, excerpt: String::new(), text: String::new() } }));
    }

    /// The turns whose `turn.start` the lane holds, in order.
    fn pending_starts(&self) -> Vec<String> {
        self.lane
            .iter()
            .filter_map(|e| match e {
                Effect::Claim { turn, .. } => Some(turn.clone()),
                Effect::Post { body, .. } | Effect::Owed { body, .. } if body["kind"] == "turn.start" => body["turn"].as_str().map(str::to_string),
                _ => None,
            })
            .collect()
    }

    /// Another life's claim of `turn` (the turn of chat record `seq`),
    /// already on `work`.
    fn claimed_by_another_life(&mut self, turn: &str, seq: u64) {
        let lead = self.lead().clone();
        let body = json!({ "kind": "turn.start", "turn": turn, "asker": "id:paul", "agent": lead.identity,
            "cause": { "fragment": CHAT, "channel": "chat", "seq": seq }, "life": "f".repeat(32) });
        assert_eq!(self.journal.post(CHAT, records::WORK, &lead.identity, &records::work_id(turn, "start"), &body), Answer::Appended);
    }

    /// What may be checked at any moment (I1, I2, F11's ids, and where an
    /// answer in words goes).
    fn check(&self) -> Result<(), String> {
        let mut seen = BTreeSet::new();
        for (_, t) in &self.runs {
            if !seen.insert(t) {
                return Err(format!("I1: {t} ran twice: {:?}", self.runs));
            }
        }
        for (life, t, claim) in &self.claims_at_run {
            let ours = claim.as_ref().is_some_and(|c| c["life"] == json!(World::life_of(*life)));
            if !ours {
                return Err(format!("I1: life {life} ran {t} without its claim answered as its own (work held {claim:?})"));
            }
        }
        let work = self.journal.channel(CHAT, records::WORK);
        let started: BTreeSet<&str> = work.iter().filter(|r| r.body["kind"] == "turn.start").filter_map(|r| r.body["turn"].as_str()).collect();
        for end in work.iter().filter(|r| r.body["kind"] == "turn.end") {
            let t = end.body["turn"].as_str().unwrap_or("");
            if !started.contains(t) {
                return Err(format!("I2: {t} ended with no start"));
            }
        }
        if !self.conflicts_on_chat.is_empty() {
            return Err(format!("an id on chat posted with two bodies: {:?}", self.conflicts_on_chat));
        }
        // An answer in words goes only to a turn this life claimed and its
        // runtime holds; a message is told at most once in a life, and never
        // also run there as a turn of its own.
        let mut told_in: BTreeSet<(u64, u64)> = BTreeSet::new();
        for t in &self.tells {
            if !t.held {
                return Err(format!("Tell: life {} told {} message {}, which its runtime does not hold", t.life, t.turn, t.seq));
            }
            if !self.runs.contains(&(t.life, t.turn.clone())) {
                return Err(format!("Tell: life {} told {} message {}, a turn it never ran", t.life, t.turn, t.seq));
            }
            if !t.claim.as_ref().is_some_and(|c| c["life"] == json!(World::life_of(t.life))) {
                return Err(format!("Tell: life {} told {} message {}, a turn not claimed as its own (work held {:?})", t.life, t.turn, t.seq, t.claim));
            }
            if !told_in.insert((t.life, t.seq)) {
                return Err(format!("Tell: life {} told message {} twice", t.life, t.seq));
            }
            let own = self.turn_of(t.seq);
            if self.runs.contains(&(t.life, own.clone())) {
                return Err(format!("Tell: life {} told message {} to {} and ran it as {own} too", t.life, t.seq, t.turn));
            }
        }
        // P5: a cut turn is told of once at most, by a turn after it, and only
        // a turn the journal ended as lost is told of
        let mut told_of: BTreeSet<&str> = BTreeSet::new();
        for (turn, note) in &self.notes {
            let Some((cut, _)) = note else { continue };
            if cut == turn || !told_of.insert(cut.as_str()) {
                return Err(format!("P5: {turn} was told of {cut}, already told or itself: {:?}", self.notes.iter().filter(|(_, n)| n.is_some()).collect::<Vec<_>>()));
            }
            if !self.ends(cut).iter().any(|e| e["error"] == crate::engine::LOST) {
                return Err(format!("P5: {turn} was told of {cut}, which did not end as lost: {:?}", self.ends(cut)));
            }
        }
        Ok(())
    }

    /// Everything settles: a life (the latest state's, if none runs), its
    /// runtime connected, every post answered, every turn it holds ended.
    fn settle(&mut self) {
        if !self.alive() {
            let s = self.latest();
            self.start(s);
        }
        self.connect();
        // bounded: each pass ends what the runtime holds, so the queues drain
        for _ in 0..1_000 {
            self.deliver();
            self.answer_all();
            let held: Vec<(String, Held)> = self.held.iter().map(|(t, h)| (t.clone(), h.clone())).collect();
            if held.is_empty() && self.lane.is_empty() && self.engine().state().turns.is_empty() {
                return;
            }
            for (t, h) in held {
                self.end(&t, if h.stop { Outcome::Stopped } else { Outcome::Idle });
            }
            // a claim left unanswered is claimed again at a tick
            self.tick(1_000);
        }
        panic!("a world that never settles");
    }

    /// Settled, every message said has one start and one end (I2, I4), and
    /// ran at most once (I1), or it was the answer a turn asked for in words
    /// (told to that turn, in the life that ran it) and started no turn of
    /// its own; and no turn is held any more.
    fn check_settled(&self, said: &[String]) -> Result<(), String> {
        self.check()?;
        for t in said {
            let (starts, ends) = (self.starts(t).len(), self.ends(t).len());
            if (starts, ends) == (0, 0) && self.was_told(t) {
                continue;
            }
            if (starts, ends) != (1, 1) {
                let dropped: Vec<&String> = self.dropped.iter().filter(|id| id.contains(t.as_str())).collect();
                return Err(format!("I2/I4: {t} has {starts} starts and {ends} ends; crashes took its posts {dropped:?} from the lane"));
            }
        }
        let held = &self.engine().state().turns;
        if !held.is_empty() {
            return Err(format!("settled, and still holding {:?}", held.keys().collect::<Vec<_>>()));
        }
        Ok(())
    }
}

/// Goal (I1, P1): a turn is handed to its runtime only once its claim (its
/// `turn.start`, as this life) is answered as this life's, and only while
/// its runtime can take it. A replay of the claim (the lane's own retry,
/// after an answer was lost) is this life's too, and runs it once.
#[test]
fn a_turn_starts_only_once_its_claim_is_answered() {
    let mut w = World::new(&["juniper"]);
    w.start_unconnected(None);
    let t = w.say("id:paul", "one");
    assert_eq!((w.runs_of(&t), w.pending_starts()), (0, vec![]), "its runtime cannot take it: neither claimed nor run");
    w.connect();
    assert_eq!(w.pending_starts(), vec![t.clone()], "claimed once its runtime can take it");
    assert_eq!(w.runs_of(&t), 0, "not run before its claim is answered");
    w.answer_one();
    assert_eq!(w.runs_of(&t), 1, "run once its claim is answered");
    assert_eq!(w.starts(&t)[0]["life"], json!(w.life()), "the claim names its life");

    // replay: the claim landed and its answer was lost; claimed again at the
    // next tick, its replay is this life's own: run once
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let t = w.say("id:paul", "one");
    w.land_unanswered_one();
    assert_eq!(w.runs_of(&t), 0, "an unanswered claim starts nothing");
    w.tick(1_000);
    assert_eq!(w.pending_starts(), vec![t.clone()], "claimed again");
    w.answer_one();
    assert_eq!((w.runs_of(&t), w.starts(&t).len()), (1, 1), "its own claim, replayed: run once");
}

/// Goal (I1): a claim another life holds (409) is never run here; the
/// turn gets its one end, and the next message runs.
#[test]
fn a_claim_another_life_holds_ends_the_turn() {
    let mut w = World::new(&["juniper"]);
    let t = w.turn_of(1);
    w.claimed_by_another_life(&t, 1);
    w.start(None);
    assert_eq!(w.say("id:paul", "one"), t);
    w.answer_one();
    assert_eq!(w.runs_of(&t), 0, "another life's turn is never run here");
    w.answer_all();
    let ends = w.ends(&t);
    assert_eq!(ends.len(), 1, "one end: {ends:?}");
    assert_eq!((ends[0]["outcome"].as_str(), ends[0]["error"].as_str()), (Some("error"), Some("lost when the computer restarted")));
    assert!(w.engine().state().turns.is_empty(), "forgotten");
    let next = w.say("id:paul", "two");
    w.answer_all();
    assert_eq!(w.runs_of(&next), 1, "the next message runs");
}

/// Goal (P1): a claim the platform refuses (403 or 404: the agent left the
/// chat, or its owner holds it below editor) runs nothing and is not asked
/// again: the turn is dropped, the chat's next turn is claimed, and the
/// computer is let go once none is left (never held awake for a claim that
/// cannot land).
#[test]
fn a_claim_the_agent_may_not_post_drops_the_turn() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let say = |e: &mut Engine, seq: u64, text: &str| e.step(Input::Record { agent: a.fragment.clone(), fragment: "talk.paul".into(), record: rec(seq, "id:paul", json!({ "text": text })), view: Some(v.clone()), since: 0 }, T0);
    let one = say(&mut e, 1, "one");
    let t1 = records::turn_id(&a.fragment, "talk.paul", "chat", 1);
    assert_eq!(kinds(&one), vec!["turn.start"]);
    assert!(say(&mut e, 2, "two").effects.iter().all(|x| !matches!(x, Effect::Claim { .. })), "two waits behind one's claim");
    let r = e.step(Input::Claimed { turn: t1.clone(), answer: ClaimAnswer::Refused }, T0 + 1);
    assert!(started(&r).is_none(), "refused: not run");
    let t2 = records::turn_id(&a.fragment, "talk.paul", "chat", 2);
    assert_eq!(posts(&r), vec![(records::work_id(&t2, "start"), posts(&r)[0].1.clone())], "no end for one (it cannot be posted), and two is claimed");
    assert!(r.dirty && !e.state().turns.contains_key(&t1), "one is dropped");
    let r2 = e.step(Input::Claimed { turn: t2.clone(), answer: ClaimAnswer::Refused }, T0 + 2);
    assert_eq!(keepalive(&r2), Some(false), "nothing left: the computer may sleep");
    assert!(e.state().turns.is_empty());
    assert!(e.step(Input::Tick, T0 + 3).effects.is_empty(), "and nothing is asked again");
}

/// Goal (P1): a claim the lane gave up on (no answer) starts nothing; the
/// turn stays queued, unclaimed, and is claimed again at the next tick.
#[test]
fn a_claim_with_no_answer_starts_nothing() {
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let t = w.say("id:paul", "one");
    w.lose_one();
    assert_eq!(w.runs_of(&t), 0, "no answer: not run");
    assert!(w.starts(&t).is_empty(), "nor claimed");
    assert_eq!(w.engine().state().turns[&t].phase, Phase::Queued, "kept queued");
    w.tick(1_000);
    assert_eq!(w.pending_starts(), vec![t.clone()], "claimed again at a tick");
    w.answer_one();
    assert_eq!(w.runs_of(&t), 1);
}

/// Goal (P1, one life per turn): a turn whose life ended after it was handed
/// to its runtime is ended as lost by the next life, never handed again,
/// whether or not the runtime had taken it.
#[test]
fn a_restart_ends_what_was_handed() {
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let t = w.say("id:paul", "one");
    w.answer_all();
    assert_eq!(w.runs_of(&t), 1, "handed");
    let state = w.latest();
    w.crash(false);
    w.start(state);
    w.answer_all();
    assert_eq!(w.runs_of(&t), 1, "never handed again");
    let ends = w.ends(&t);
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["error"], "lost when the computer restarted");
}

/// Goal (I3): a rollback starts nothing twice. Method: keep the state after
/// turn one, run turn two to its end, start a new life from the kept state,
/// feed it the records again against the journal, and count runs.
#[test]
fn a_rollback_starts_nothing_twice() {
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let t1 = w.say("id:paul", "one");
    w.answer_all();
    w.end(&t1, Outcome::Idle);
    w.answer_all();
    let kept = w.latest();
    let t2 = w.say("id:paul", "two");
    w.answer_all();
    w.end(&t2, Outcome::Idle);
    w.answer_all();
    w.crash(false);

    w.start(kept);
    w.answer_all();
    let t3 = w.say("id:paul", "three");
    w.answer_all();
    w.end(&t3, Outcome::Idle);
    w.answer_all();
    let runs: Vec<&str> = w.runs.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(runs, vec![t1.as_str(), t2.as_str(), t3.as_str()], "each turn ran once");
    for t in [&t1, &t2, &t3] {
        let ends = w.ends(t);
        assert_eq!(ends.len(), 1, "{t}: one end");
        assert_eq!(ends[0]["outcome"], "idle", "{t}: its own life's end stands");
    }
    w.check().unwrap();
}

/// Goal (I4): a message admitted and never started before a crash is run by
/// the next life, whether that life wakes with the state the crash left (it
/// queued there), a state from before it was said, or none.
#[test]
fn said_before_a_crash_and_never_started_runs_in_the_next_life() {
    for wake in ["latest", "before", "none"] {
        let mut w = World::new(&["juniper"]);
        w.start(None);
        let t1 = w.say("id:paul", "one");
        w.answer_all();
        let before = w.latest();
        let t2 = w.say("id:paul", "two");
        w.answer_all();
        assert_eq!(w.runs_of(&t2), 0, "{wake}: two waits behind one");
        let state = match wake {
            "latest" => w.latest(),
            "before" => before,
            _ => None,
        };
        w.crash(false);
        w.start(state);
        w.answer_all();
        assert_eq!(w.runs_of(&t2), 1, "{wake}: the next life runs it");
        w.end(&t2, Outcome::Idle);
        w.answer_all();
        assert_eq!(w.ends(&t2)[0]["outcome"], "idle", "{wake}");
        assert_eq!(w.ends(&t1).len(), 1, "{wake}: one ended once");
    }
}

/// Goal (F11): what an agent says with no turn running survives a
/// rollback: its id holds the life, so a counter that went back collides
/// with nothing, and both messages are posted.
#[test]
fn what_an_agent_says_unasked_survives_a_rollback() {
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let kept = w.latest();
    w.say_unasked("remember the milk");
    w.answer_all();
    w.crash(false);
    w.start(kept);
    w.say_unasked("water the plants");
    w.answer_all();
    let said: Vec<Value> = w.journal.channel(CHAT, records::CHAT).iter().map(|r| r.body["text"].clone()).collect();
    assert_eq!(said, vec![json!("remember the milk"), json!("water the plants")], "both posted");
    w.check().unwrap();
}

/// Goal (P1, with a question in flight): a turn that asks its asker in
/// words is a running turn of its life. A restart ends it as lost, whatever
/// state the next life wakes with (the one the crash left, one from before
/// the turn was said, or none), and the answer said after is never told to
/// a turn the new life does not own: it is a turn of its own, claimed and
/// run once. Rollback: an answer told in its turn's life, read again by a
/// life a rollback sent back before it, finds no turn of that life asking,
/// and is that life's own turn, run once; the asking turn's end stands.
#[test]
fn a_question_cut_by_a_restart_is_lost_and_its_answer_runs_once() {
    for wake in ["latest", "before", "none"] {
        let mut w = World::new(&["juniper"]);
        w.start(None);
        let before = w.latest();
        let asked = w.say("id:paul", "name my plant");
        w.answer_all();
        w.ask_in_words(&asked);
        w.answer_all();
        assert!(w.engine().state().turns[&asked].asking, "{wake}: it asks");
        let state = match wake {
            "latest" => w.latest(),
            "before" => before,
            _ => None,
        };
        w.crash(false);
        w.start(state);
        w.answer_all();
        assert_eq!(w.runs_of(&asked), 1, "{wake}: never run again");
        assert_eq!(w.ends(&asked), vec![json!({ "kind": "turn.end", "turn": asked, "outcome": "error", "error": LOST })], "{wake}: ended once, as lost");
        let answer = w.say("id:paul", "Fernando");
        w.answer_all();
        assert!(w.tells.is_empty(), "{wake}: never told to a turn of another life");
        assert_eq!(w.runs_of(&answer), 1, "{wake}: the answer runs as a turn of its own");
        w.end(&answer, Outcome::Idle);
        w.settle();
        assert_eq!((w.runs_of(&answer), w.starts(&answer).len(), w.ends(&answer)[0]["outcome"].clone()), (1, 1, json!("idle")), "{wake}: once");
        w.check_settled(&[asked, answer]).unwrap_or_else(|e| panic!("{wake}: {e}"));
    }

    // told in its turn's life, then a rollback to before it was read
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let asked = w.say("id:paul", "name my plant");
    w.answer_all();
    w.ask_in_words(&asked);
    w.answer_all();
    let asking = w.latest();
    let answer = w.say("id:paul", "Fernando");
    assert_eq!(w.tells.iter().map(|t| (t.life, t.turn.clone(), w.turn_of(t.seq))).collect::<Vec<_>>(), vec![(1, asked.clone(), answer.clone())], "told to the asking turn");
    w.end(&asked, Outcome::Idle);
    w.answer_all();
    assert!(w.starts(&answer).is_empty(), "the answer started no turn of its own");
    w.crash(false);
    w.start(asking);
    w.answer_all();
    assert_eq!(w.ends(&asked), vec![json!({ "kind": "turn.end", "turn": asked, "outcome": "idle" })], "its own end stands (the new life's, as lost, is a 409)");
    assert_eq!(w.runs_of(&asked), 1, "never run again");
    assert_eq!(w.runs_of(&answer), 1, "read again, the answer is the new life's own turn");
    assert_eq!(w.tells.len(), 1, "told once, in the life that ran the asking turn");
    w.settle();
    w.check_settled(&[asked, answer]).unwrap();
}

/// A small deterministic generator (splitmix64): one seed, one history.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "a choice among some");
        self.next() % n
    }
}

/// Goal (P5): the first turn the agent runs in the chat after one of its
/// turns there ended as lost carries a note, built from the journal alone:
/// what the cut turn was asked, the steps it recorded, what it had replied,
/// and that a restart cut it. The turn after carries none (said once), also
/// after a rollback that reads the noted turn's message again; and the note
/// is the same whatever state the next life wakes with: the one the crash
/// left (the cut turn running), one from before the cut turn was said, or
/// none.
#[test]
fn the_turn_after_a_lost_one_carries_the_note_once() {
    let mut notes: Vec<String> = Vec::new();
    for wake in ["latest", "before", "none"] {
        let mut w = World::new(&["juniper"]);
        w.start(None);
        let one = w.say("id:paul", "one");
        w.answer_all();
        w.end(&one, Outcome::Idle);
        w.answer_all();
        let before = w.latest();
        let cut = w.say("id:paul", "do the risky thing");
        w.answer_all();
        assert_eq!(w.note_of(&cut), None, "{wake}: the turn before it was answered");
        w.did(&cut, "terminal", "ls /tmp");
        w.step(Input::Runtime(Event::Reply { turn: cut.clone(), part: 1, text: "Checking first.".into() }));
        w.did(&cut, "terminal", "rm -rf /tmp/x");
        w.answer_all();
        let state = match wake {
            "latest" => w.latest(),
            "before" => before,
            _ => None,
        };
        w.crash(false);
        w.start(state);
        w.answer_all();
        assert_eq!(w.ends(&cut).len(), 1, "{wake}: the cut turn ended once");
        assert_eq!(w.ends(&cut)[0]["error"], LOST, "{wake}");
        let kept = w.latest();
        let next = w.say("id:paul", "good morning");
        w.answer_all();
        let note = w.note_of(&next).unwrap_or_else(|| panic!("{wake}: the turn after the lost one carries its note"));
        for said in ["cut short", "computer restarted", "Check what it already did", "“do the risky thing”", "terminal ls /tmp (ok); terminal rm -rf /tmp/x (ok)", "It had replied: “Checking first.”"] {
            assert!(note.contains(said), "{wake}: {said:?} in {note}");
        }
        w.end(&next, Outcome::Idle);
        w.answer_all();
        let after = w.say("id:paul", "and after that");
        w.answer_all();
        assert_eq!(w.note_of(&after), None, "{wake}: said once");
        w.end(&after, Outcome::Idle);
        w.answer_all();
        // a rollback to before the noted turn's message: read again, it is
        // another life's claim and runs nothing; the next turn is told nothing
        w.crash(false);
        w.start(kept);
        w.answer_all();
        let last = w.say("id:paul", "once more");
        w.answer_all();
        assert_eq!(w.runs_of(&next), 1, "{wake}: the noted turn ran once");
        assert_eq!(w.note_of(&last), None, "{wake}: said once, after a rollback too");
        w.check().unwrap();
        notes.push(note);
    }
    assert!(notes.windows(2).all(|p| p[0] == p[1]), "the journal's note, whatever state woke: {notes:#?}");
}

/// Seeds the simulation runs, events in each, and messages said in each.
const SIM_SEEDS: u64 = 120;
const SIM_EVENTS: usize = 400;
const SIM_SAID_MAX: usize = 40;

/// What a history reached, beyond its invariants: answers told to a turn
/// asking in words, and those a later life read again and ran as a turn of
/// their own (a rollback to before the life that told them).
#[derive(Debug, Default, Clone, Copy)]
struct Reached {
    told: usize,
    told_then_run: usize,
}

/// One history: events chosen by `seed`, the invariants checked after each,
/// and every message's records checked once everything settles.
fn simulate(seed: u64) -> Result<Reached, String> {
    // Its events would be millions of log lines.
    crate::log::set_quiet(true);
    let mut rng = Rng(seed);
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let mut said: Vec<String> = Vec::new();
    let mut unasked = 0;
    for n in 0..SIM_EVENTS {
        let what = if !w.alive() && rng.below(3) == 0 {
            // a new life: from the latest state, an earlier one, or none
            let state = match rng.below(4) {
                0 => None,
                1 => w.latest(),
                _ if w.saves.is_empty() => None,
                _ => Some(w.saves[rng.below(w.saves.len() as u64) as usize].clone()),
            };
            w.start(state);
            "start"
        } else {
            match rng.below(100) {
                0..=14 if said.len() < SIM_SAID_MAX => {
                    // said, and read when the followers next deliver
                    let seq = w.journal.append(CHAT, records::CHAT, "id:paul", json!({ "text": format!("message {}", said.len()) }));
                    said.push(w.turn_of(seq));
                    "say"
                }
                15..=27 => {
                    w.deliver();
                    "deliver"
                }
                28..=47 => {
                    w.answer_one();
                    "answer"
                }
                48..=51 => {
                    w.lose_one();
                    "lose"
                }
                52..=55 => {
                    w.land_unanswered_one();
                    "land unanswered"
                }
                56..=70 if !w.held.is_empty() => {
                    let held: Vec<(String, Held)> = w.held.iter().map(|(t, h)| (t.clone(), h.clone())).collect();
                    let (t, h) = held[rng.below(held.len() as u64) as usize].clone();
                    // a turn waits on its card's answer, or on its asker's
                    // words (the next message paul says is them)
                    match (h.stop, h.prompted.is_some() || h.asking, rng.below(10)) {
                        (true, _, _) => w.end(&t, Outcome::Stopped),
                        (false, true, _) => {}
                        (false, false, 0..=1) => w.ask(&t),
                        (false, false, 2) => w.end(&t, Outcome::Error("it failed".into())),
                        (false, false, 3..=4) => w.ask_in_words(&t),
                        (false, false, _) => w.end(&t, Outcome::Idle),
                    }
                    "the runtime"
                }
                71..=75 => {
                    // paul answers a card the journal shows open
                    let asked: Vec<Value> = w.journal.channel(CHAT, records::WORK).iter().map(|r| r.body.clone()).filter(|b| b["kind"] == "turn.prompt").collect();
                    if !asked.is_empty() {
                        let p = &asked[rng.below(asked.len() as u64) as usize];
                        let option = if rng.below(2) == 0 { "once" } else { "deny" };
                        w.journal.append(CHAT, records::CHAT, "id:paul", json!({ "kind": "prompt_response", "prompt": p["prompt"], "option": option }));
                    }
                    "answer a card"
                }
                76..=79 => {
                    let ms = [1_000, 30_000, 120_000, 1_800_000][rng.below(4) as usize];
                    w.tick(ms);
                    "tick"
                }
                80..=83 if !said.is_empty() => {
                    let t = said[rng.below(said.len() as u64) as usize].clone();
                    w.journal.append(CHAT, records::CHAT, "id:paul", json!({ "kind": "stop", "turn": t }));
                    "stop"
                }
                84..=88 if w.alive() => {
                    w.crash(rng.below(2) == 0);
                    "crash"
                }
                89..=92 if w.alive() => {
                    unasked += 1;
                    w.say_unasked(&format!("unasked {unasked}"));
                    "say unasked"
                }
                93..=94 if w.alive() => {
                    w.disconnect();
                    "the runtime goes"
                }
                95..=97 if w.alive() => {
                    w.connect();
                    "the runtime comes back"
                }
                _ => "nothing",
            }
        };
        w.check().map_err(|e| format!("seed {seed}, event {n} ({what}): {e}"))?;
    }
    w.settle();
    w.check_settled(&said).map_err(|e| format!("seed {seed}, settled: {e}"))?;
    let told_then_run = w.tells.iter().filter(|t| w.runs.iter().any(|(life, run)| *life > t.life && *run == w.turn_of(t.seq))).count();
    Ok(Reached { told: w.tells.len(), told_then_run })
}

/// Goal (I1, I2, I4): any history of messages, answers lost and late, runs,
/// cards, questions in words and their answers (`Asked`, `Tell`), Stops,
/// crashes (each taking the lane's unsent posts with it), and lives started
/// from any saved state or none keeps the invariants: checked after every
/// event (an answer in words told only to a turn its life claimed and
/// runs), and once it settles every message has one start and one end, ran
/// at most once, or was an answer told to a turn and started none of its
/// own; and no turn is held. No case is let off. The test that finds the
/// cases nobody listed.
#[test]
fn any_history_of_crashes_and_rollbacks_keeps_the_invariants() {
    let histories: Vec<Result<Reached, String>> = (0..SIM_SEEDS).map(simulate).collect();
    let failures: Vec<String> = histories.iter().filter_map(|h| h.clone().err()).collect();
    assert!(failures.is_empty(), "{} of {SIM_SEEDS} histories broke an invariant; the first:\n{}", failures.len(), failures.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
    let reached = histories.iter().filter_map(|h| h.as_ref().ok()).fold(Reached::default(), |a, r| Reached { told: a.told + r.told, told_then_run: a.told_then_run + r.told_then_run });
    assert!(reached.told > 0 && reached.told_then_run > 0, "the histories answer questions in words, and a rollback reads some answers again: {reached:?}");
}

/// Goal (I2, I4): the case the simulation found, each way it happens: a
/// crash takes a turn's last records from the lane after the state was
/// written without them, and the next life (from the state the crash
/// left) posts them again, under the same ids and bodies, so the turn ends
/// once and as itself, never as lost. A turn that ran (its end taken; or
/// its end landed and its answer lost, so the next life's is a replay), a
/// refused one, and one stopped while it waited (their start and end
/// taken). Was `a_crash_that_drops_a_turns_end_leaves_it_open`, ignored
/// while P1 left it so.
#[test]
fn a_crash_that_drops_a_turns_end_leaves_nothing_open() {
    // a turn that ran: its end taken by the crash
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let t = w.say("id:paul", "one");
    w.answer_all();
    w.end(&t, Outcome::Idle);
    assert_eq!(w.engine().state().turns[&t].phase, Phase::Ended, "ended, owing its end");
    let state = w.latest();
    w.crash(false);
    assert!(w.ends(&t).is_empty(), "the crash took its end");
    w.start(state);
    w.answer_all();
    assert_eq!(w.ends(&t), vec![json!({ "kind": "turn.end", "turn": t, "outcome": "idle" })], "its own end, posted again");
    assert_eq!(w.runs_of(&t), 1);
    assert!(w.engine().state().turns.is_empty(), "let go once answered");

    // its end landed, and the answer was lost with the life: a replay
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let t = w.say("id:paul", "one");
    w.answer_all();
    w.end(&t, Outcome::Idle);
    w.answer_one(); // its reply
    let state = w.latest();
    w.crash(true); // its end lands; no one hears
    assert_eq!(w.ends(&t).len(), 1);
    w.start(state);
    w.answer_all();
    assert_eq!(w.ends(&t).len(), 1, "posted again: a replay, so one end");
    assert!(w.engine().state().turns.is_empty());

    // a refused turn, and one stopped while it waited: their two records taken
    let mut w = World::new(&["juniper"]);
    w.start(None);
    let running = w.say("id:paul", "running");
    w.answer_all();
    let stopped = w.say("id:paul", "stopped");
    // the queue full: `stopped` and these wait, the next is refused
    for n in 1..limits::QUEUED_PER_CHAT_MAX {
        w.say("id:paul", &format!("waiting {n}"));
    }
    let refused = w.say("id:paul", "refused");
    w.say_body("id:paul", json!({ "kind": "stop", "turn": stopped }));
    assert_eq!(w.pending_starts(), vec![refused.clone(), stopped.clone()], "both owe their start and end");
    let state = w.latest();
    w.crash(false);
    w.start(state);
    w.answer_all();
    for (t, outcome) in [(&refused, "error"), (&stopped, "stopped")] {
        assert_eq!((w.starts(t).len(), w.ends(t).len()), (1, 1), "{t}: both records, once");
        assert_eq!(w.ends(t)[0]["outcome"], outcome);
        assert_eq!(w.runs_of(t), 0, "{t}: never run");
    }
    assert_eq!(w.ends(&running)[0]["error"], LOST, "the one the crash cut is lost");
    w.check().unwrap();
}
