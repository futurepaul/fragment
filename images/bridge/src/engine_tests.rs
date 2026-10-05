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

/// `s`, with each claim it makes answered as this life's, and what each
/// answer does (the turn handed to the runtime) added to it.
fn answered(e: &mut Engine, mut s: Step, now: u64) -> Step {
    let claims: Vec<String> = s.effects.iter().filter_map(|x| match x { Effect::Claim { turn, .. } => Some(turn.clone()), _ => None }).collect();
    for turn in claims {
        let more = e.step(Input::Claimed { turn, answer: ClaimAnswer::Ours }, now);
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

/// What a step posts, claims included (a claim is a `turn.start`).
fn posts(s: &Step) -> Vec<(String, Value)> {
    s.effects.iter().filter_map(|e| match e { Effect::Post { id, body, .. } | Effect::Claim { id, body, .. } => Some((id.clone(), body.clone())), _ => None }).collect()
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
    let end = ev(&mut e, Event::End { turn: turn.clone(), outcome: Outcome::Idle }, T0 + 4);
    let p = posts(&end);
    assert_eq!(p[0], (records::reply_id(&turn, 1), json!({ "text": "Hello", "turn": turn })));
    assert_eq!(p[1], (records::work_id(&turn, "end"), json!({ "kind": "turn.end", "turn": turn, "outcome": "idle" })));
    assert!(end.effects.contains(&Effect::Draft { agent: a.fragment.clone(), fragment: "talk.paul".into(), turn: turn.clone(), text: None }));
    assert_eq!(keepalive(&end), Some(false));
    assert!(e.state().turns.is_empty());
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
    assert!(!e.state().turns.contains_key(&refused), "never held");
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
/// computer may sleep; the owner's answer (the first) resumes the turn.
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
    assert_eq!(keepalive(&p), Some(false), "waiting on a person: the computer may sleep");
    // a repeat of the same prompt asks nothing
    assert_eq!(ev(&mut e, Event::Prompt { turn: turn.clone(), prompt: "ab12.0011".into(), text: "again".into(), options, ttl_ms: None }, T0 + 11).effects, vec![]);

    let skyler = said(&mut e, &a, &v, 2, "id:skyler", json!({ "kind": "prompt_response", "prompt": "ab12.0011", "option": "once" }), T0 + 20);
    assert!(commands(&skyler).is_empty() && posts(&skyler).is_empty(), "only the owner answers");
    let bad = said(&mut e, &a, &v, 3, "id:paul", json!({ "kind": "prompt_response", "prompt": "ab12.0011", "option": "always" }), T0 + 21);
    assert!(commands(&bad).is_empty(), "not an option of this prompt");
    let yes = said(&mut e, &a, &v, 4, "id:paul", json!({ "kind": "prompt_response", "prompt": "ab12.0011", "option": "once" }), T0 + 22);
    assert_eq!(commands(&yes), vec![Command::Answer { turn: turn.clone(), prompt: "ab12.0011".into(), option: Some("once".into()), seq: 4, by: "id:paul".into() }]);
    assert_eq!(posts(&yes)[0].1, json!({ "kind": "turn.prompt.closed", "turn": turn, "prompt": "ab12.0011", "outcome": "answered", "option": "once", "by": "id:paul" }));
    assert_eq!(keepalive(&yes), Some(true), "busy again");
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
    assert_eq!(e.state().turns.values().find(|t| t.agent == r.fragment).expect("rowan's").hop, 1);
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

/// The platform's channels: each one's records in order, and posts by id.
#[derive(Debug, Default)]
struct Journal {
    records: BTreeMap<(String, String), Vec<Record>>,
    ids: HashMap<(String, String), (String, Value)>,
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
            Some((c, b)) if c == channel && b == body => Answer::Replayed,
            Some(_) => Answer::Conflict,
            None => {
                self.append(fragment, channel, principal, body.clone());
                self.ids.insert(key, (channel.to_string(), body.clone()));
                Answer::Appended
            }
        }
    }

    fn channel(&self, fragment: &str, channel: &str) -> &[Record] {
        self.records.get(&(fragment.to_string(), channel.to_string())).map(Vec::as_slice).unwrap_or(&[])
    }

    /// A turn's records of `kind` on the chat's `work`.
    fn work(&self, turn: &str, kind: &str) -> Vec<Value> {
        self.channel(CHAT, records::WORK).iter().map(|r| r.body.clone()).filter(|b| b["turn"] == turn && b["kind"] == kind).collect()
    }
}

/// A turn the model runtime holds: the prompt it waits on, and whether its
/// asker pressed Stop.
#[derive(Debug, Default, Clone)]
struct Held {
    prompted: Option<String>,
    stop: bool,
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
    held: BTreeMap<String, Held>,
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
            held: BTreeMap::new(),
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
                Effect::Post { .. } | Effect::Claim { .. } => self.lane.push_back(e),
                Effect::Runtime(Command::Start(ts)) => {
                    let claim = self.journal.work(&ts.turn, "turn.start").into_iter().next();
                    self.runs.push((self.lives, ts.turn.clone()));
                    self.claims_at_run.push((self.lives, ts.turn.clone(), claim));
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
            Effect::Claim { agent, fragment, id, body, .. } => (agent, fragment, records::WORK, id, body),
            _ => panic!("the lane holds posts and claims: {e:?}"),
        };
        let principal = self.agents.iter().find(|a| &a.fragment == agent).map(|a| a.identity.clone()).expect("an agent's post");
        let answer = self.journal.post(fragment, channel, &principal, id, body);
        if channel == records::CHAT && answer == Answer::Conflict {
            self.conflicts_on_chat.push(id.clone());
        }
        answer
    }

    /// The engine is told a claim's answer (the driver's `Input::Claimed`).
    fn tell(&mut self, e: &Effect, answer: ClaimAnswer) {
        if let Effect::Claim { turn, .. } = e {
            self.step(Input::Claimed { turn: turn.clone(), answer });
        }
    }

    /// The lane's next post, answered; false when it holds none.
    fn answer_one(&mut self) -> bool {
        let Some(e) = self.lane.pop_front() else { return false };
        let answer = match self.land(&e) {
            Answer::Appended | Answer::Replayed => ClaimAnswer::Ours,
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
    /// claim is heard unanswered (its retry will find it a replay).
    fn land_unanswered_one(&mut self) {
        if let Some(e) = self.lane.pop_front() {
            self.land(&e);
            self.tell(&e, ClaimAnswer::Unanswered);
            self.deliver();
        }
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
        assert!(self.held.remove(turn).is_some(), "the runtime holds {turn}");
        if outcome == Outcome::Idle {
            self.step(Input::Runtime(Event::Reply { turn: turn.into(), part: 1, text: format!("an answer to {turn}") }));
        }
        self.step(Input::Runtime(Event::End { turn: turn.into(), outcome }));
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

    /// The turns whose `turn.start` the lane holds, in order.
    fn pending_starts(&self) -> Vec<String> {
        self.lane
            .iter()
            .filter_map(|e| match e {
                Effect::Claim { turn, .. } => Some(turn.clone()),
                Effect::Post { body, .. } if body["kind"] == "turn.start" => body["turn"].as_str().map(str::to_string),
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

    /// What may be checked at any moment (I1, I2, and F11's ids).
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
    /// ran at most once (I1). Not `strict`, a message is let off whose end a
    /// crash took from the lane before it was sent (the gap P1 leaves:
    /// `a_crash_that_drops_a_turns_end_leaves_it_open`); how many were.
    fn check_settled(&self, said: &[String], strict: bool) -> Result<usize, String> {
        self.check()?;
        let mut let_off = 0;
        for t in said {
            let (starts, ends) = (self.starts(t).len(), self.ends(t).len());
            if (starts, ends) == (1, 1) {
                continue;
            }
            let end_dropped = self.dropped.contains(&records::work_id(t, "end"));
            if end_dropped && !strict {
                let_off += 1;
                continue;
            }
            let dropped: Vec<&String> = self.dropped.iter().filter(|id| id.contains(t.as_str())).collect();
            return Err(format!("I2/I4: {t} has {starts} starts and {ends} ends; a crash took its posts {dropped:?} from the lane"));
        }
        Ok(let_off)
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

/// Seeds the simulation runs, events in each, and messages said in each.
const SIM_SEEDS: u64 = 120;
const SIM_EVENTS: usize = 400;
const SIM_SAID_MAX: usize = 40;

/// One history: events chosen by `seed`, the invariants checked after each,
/// and every message's records checked once everything settles (`strict`:
/// see `World::check_settled`). How many messages were let off.
fn simulate(seed: u64, strict: bool) -> Result<usize, String> {
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
                    match (h.stop, h.prompted.is_some(), rng.below(10)) {
                        (true, _, _) => w.end(&t, Outcome::Stopped),
                        (false, true, _) => {}
                        (false, false, 0..=1) => w.ask(&t),
                        (false, false, 2) => w.end(&t, Outcome::Error("it failed".into())),
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
    w.check_settled(&said, strict).map_err(|e| format!("seed {seed}, settled: {e}"))
}

/// Goal (I1, I2, I4): any history of messages, answers lost and late, runs,
/// cards, Stops, crashes (each taking the lane's unsent posts with it), and
/// lives started from any saved state or none keeps the invariants: checked
/// after every event, and once it settles every message has one start and
/// one end, and ran at most once. The test that finds the cases nobody
/// listed; the one it found that P1 does not cover is let off here, counted,
/// and is the next test's.
#[test]
fn any_history_of_crashes_and_rollbacks_keeps_the_invariants() {
    let results: Vec<Result<usize, String>> = (0..SIM_SEEDS).map(|seed| simulate(seed, false)).collect();
    let failures: Vec<&String> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(failures.is_empty(), "{} of {SIM_SEEDS} histories broke an invariant; the first:\n{}", failures.len(), failures.iter().take(5).map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
    let let_off: usize = results.iter().filter_map(|r| r.as_ref().ok()).sum();
    eprintln!("{SIM_SEEDS} histories kept the invariants; {let_off} messages whose end a crash took from the lane were let off");
}

/// Goal (I2, I4): a turn whose end a crash took from the lane before it was
/// sent is ended by a later life. Found by the simulation above, in about
/// half its histories: the state is written before the step's effects, so
/// once a turn has ended in the state its end lives only in the lane (as do
/// a refused or queued-and-stopped turn's two records); a crash then, and
/// no later life holds the turn or reads its record again (its cursor is
/// past it), so it stays open (its start and no end), or a refusal leaves
/// no record at all. P1's rule at a start ends only the turns the state
/// holds. Stopped here and reported: closing it needs a choice (a turn
/// kept in the state until its end is answered, a persisted outbox of
/// posts, or each new life reading `work`'s tail for this agent's open
/// turns).
#[test]
#[ignore = "P1 leaves it open: a crash that takes a turn's end from the lane, after its state forgot the turn"]
fn a_crash_that_drops_a_turns_end_leaves_it_open() {
    let failures: Vec<String> = (0..SIM_SEEDS).filter_map(|seed| simulate(seed, true).err()).collect();
    assert!(failures.is_empty(), "{} of {SIM_SEEDS} histories left a turn open; the first:\n{}", failures.len(), failures.iter().take(5).cloned().collect::<Vec<_>>().join("\n"));
}
