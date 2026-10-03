//! The engine's rules, each a valid path and its invalid, replay, and
//! restart paths. Method: drive the pure engine with records and runtime
//! events at chosen times, and read the effects it asks for.

use serde_json::{json, Value};

use super::*;
use crate::records::Step as ToolStep;

const T0: u64 = 1_000_000;

fn agent(label: &str) -> Agent {
    Agent { fragment: format!("{label}.paul"), identity: format!("id:{label}"), name: label.to_string(), owner: "id:paul".into(), credentials: vec![] }
}

fn engine(agents: &[Agent]) -> Engine {
    let mut e = Engine::new(State::default(), Settings { prompt_ttl_ms: 60_000, turn_idle_ms: 600_000 }).expect("a fresh state");
    e.step(Input::Agents(agents.to_vec()), T0);
    e.recover(T0);
    e
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
    e.step(Input::Record { agent: a.fragment.clone(), fragment: "talk.paul".into(), record: rec(seq, principal, body), view: Some(v.clone()), since: 0 }, now)
}

fn posts(s: &Step) -> Vec<(String, Value)> {
    s.effects.iter().filter_map(|e| match e { Effect::Post { id, body, .. } => Some((id.clone(), body.clone())), _ => None }).collect()
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
    e.step(Input::Runtime(event), now)
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
    assert!(ev(&mut e, Event::Accepted { turn: turn.clone() }, T0 + 1).dirty, "taken by the runtime: a restart ends it");
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
/// past the queue's bound a message is told so, not queued.
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
    assert_eq!(kinds(&over), vec!["turn.end"]);
    assert_eq!(posts(&over)[0].1["outcome"], "error");
    assert!(started(&over).is_none());
}

/// Goal: only the turn's asker stops it. A Stop from anyone else is
/// ignored; a Stop naming a queued turn removes it.
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
    assert_eq!(posts(&s), vec![(records::work_id(&queued, "end"), json!({ "kind": "turn.end", "turn": queued, "outcome": "stopped" }))]);
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
    ev(&mut e, Event::Accepted { turn: turn.clone() }, T0);
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
/// (it died with the old boot), one handed but never taken is handed again,
/// a queued one runs, and catching up on the same records starts nothing.
#[test]
fn a_restart_starts_nothing_twice() {
    let a = agent("juniper");
    let v = view(&[&a]);
    let mut e = engine(std::slice::from_ref(&a));
    let first = started(&said(&mut e, &a, &v, 1, "id:paul", json!({ "text": "one" }), T0)).expect("started").turn;
    ev(&mut e, Event::Accepted { turn: first.clone() }, T0);
    said(&mut e, &a, &v, 2, "id:paul", json!({ "text": "two" }), T0);
    let b = agent("rowan");
    // the state as persisted, read back
    let saved = serde_json::to_string(e.state()).expect("serializes");
    let state: State = serde_json::from_str(&saved).expect("deserializes");
    assert_eq!(&state, e.state(), "a round trip changes nothing");

    let mut e2 = Engine::new(state, Settings::default()).expect("whole");
    e2.step(Input::Agents(vec![a.clone(), b]), T0 + 10);
    let r = e2.recover(T0 + 10);
    let p = posts(&r);
    assert_eq!(p[0].1["kind"], "turn.end");
    assert_eq!(p[0].1["turn"], first);
    assert_eq!(p[0].1["outcome"], "error");
    let next = started(&r).expect("the queued one runs");
    assert_eq!(next.text, "two");
    // the backlog again: nothing new
    for seq in 1..=2 {
        assert_eq!(said(&mut e2, &a, &v, seq, "id:paul", json!({ "text": "again" }), T0 + 11), Step::default());
    }
    // handed, never taken: a restart hands it again (same turn id)
    let saved = serde_json::to_string(e2.state()).expect("serializes");
    let mut e3 = Engine::new(serde_json::from_str(&saved).expect("reads"), Settings::default()).expect("whole");
    e3.step(Input::Agents(vec![a.clone()]), T0 + 20);
    let r3 = e3.recover(T0 + 20);
    assert_eq!(started(&r3).expect("handed again").turn, next.turn);
    assert_eq!(e3.state().boot, 3);
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
    assert!(Engine::new(behind, Settings::default()).is_err());
    let mut renamed = e.state().clone();
    let (id, mut t) = renamed.turns.pop_first().expect("one");
    t.id = "0".repeat(24);
    renamed.turns.insert(id, t);
    assert!(Engine::new(renamed, Settings::default()).is_err());
    let mut old = e.state().clone();
    old.version = 0;
    assert!(Engine::new(old, Settings::default()).is_err());
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
