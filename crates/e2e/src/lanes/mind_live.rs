//! The mind on real vendors (docs/optchat.md): a preview's GLM through the
//! model route, Clef, and goose on a computer on Cloudflare Containers. The
//! hosted lane runs it by name alone (`cargo xtask e2e --hosted --config
//! <deploy config> --branch <b> --only mind-live`): a computer's first
//! start and a real model's turns take minutes, and it spends up to
//! `PAID_CALLS` of the run's paid calls. A local run and a rehearsal have no
//! real models (needs.rs `RealModels`): the mind section checks the same
//! loop on the fakes.
//!
//! An e2e person's first run is made through the API as the shell makes it
//! (cell/shell/shell.js `defaultAgent`), then:
//!
//! 1. a fact said in thread A is answered there;
//! 2. once the compactor settles, a fresh thread B asks for it back, and the
//!    answer names it (its zoom and search calls counted, its turn timed);
//! 3. a topic added sorts thread A into it (Clef, through `classify`);
//! 4. the persona with hands (Builder) hands shell work to goose in thread
//!    C; the task is done with the commands' outputs in its report, and
//!    the mind follows up in the thread;
//! 5. the run says its latencies, tool calls and paid calls.
//!
//! A model's words are checked only where the ask fixes them (a name, a
//! city, a command's output). Every wait is a real model's: generous, and
//! asked every few seconds.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::computers::phase;
use super::ledger::entries;
use crate::api::{now_ms, Api};
use crate::{Need, Suite};

pub const SECTION: &str = "mind-live";

/// The paid calls the run lends the section's person: the turns' model
/// calls (one a tool round), the compactor's, Clef's sorts and goose's
/// calls; about 40. The run's default budget is 60.
const PAID_CALLS: u64 = 60;
/// The mind's code installed from the release.
const INSTALL: Duration = Duration::from_secs(90);
/// A turn answered in words (each real model call takes 3–40 s).
const REPLY: Duration = Duration::from_secs(5 * 60);
/// The compactor building what the turns logged.
const SETTLE: Duration = Duration::from_secs(5 * 60);
/// Clef sorting the threads into a new topic.
const SORT: Duration = Duration::from_secs(5 * 60);
/// A hand-off's report: the computer's first start from cold (the image
/// pulled to a host, goose booted), then goose's turn.
const HANDOFF: Duration = Duration::from_secs(15 * 60);
/// One look every few seconds: each is a request to the preview.
const POLL: Duration = Duration::from_secs(3);

const FACT: &str = "Remember this: my sister's name is Ana and she lives in Porto.";
const RECALL: &str = "What's my sister's name, and where does she live?";
const WORK: &str = "Use the computer: run `uname -s` and `echo $((6*7))` in the shell and tell me both outputs exactly.";

/// The shell's agent colours and its first agent's soul (cell/shell/shell.js:
/// `COLORS`, `colorOf`, `firstSoul`), so the agent is made as a person's is.
const COLORS: [&str; 6] = ["#a88bea", "#62c8af", "#eda978", "#80afe9", "#dc91b6", "#b7c878"];
const TITLE: &str = "Novatron DX";

fn first_soul(username: &str) -> String {
    format!("You are {TITLE}, {username}'s default agent: the first one they talk to, and in charge of the rest. Help with whatever they ask. When a job would be better as an app, or as an agent of its own, say so and offer to set it up. The first time you talk, say hello briefly and ask what they'd like to start with.\n")
}

/// The shell's `colorOf`: FNV-1a over the identity's UTF-16 units.
fn color_of(id: &str) -> &'static str {
    let mut h: u32 = 0x811c_9dc5;
    for unit in id.encode_utf16() {
        h = (h ^ u32::from(unit)).wrapping_mul(0x0100_0193);
    }
    COLORS[(h % COLORS.len() as u32) as usize]
}

/// Asks `f` every `POLL` until it answers, or `bound` passes (said, with
/// where, as `Suite::eventually` says it).
#[track_caller]
fn within<T>(bound: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let at = std::panic::Location::caller();
    let t0 = Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if t0.elapsed() >= bound {
            println!("      (a wait ran out its {bound:.0?} at {}:{})", at.file(), at.line());
            return None;
        }
        std::thread::sleep(POLL);
    }
}

/// The person's mind, as its page reads and writes it.
struct Mind<'a> {
    api: &'a Api,
    owner: &'a Keys,
    name: String,
}

impl Mind<'_> {
    /// An operation's result (`Null` when it is refused).
    fn op(&self, op: &str, input: Value) -> Value {
        let r = self.api.op(self.owner, &self.name, op, &format!("{op}-{}", now_ms()), input);
        r.ok().filter(|r| r.status == 200).map(|r| r.body["result"].clone()).unwrap_or(Value::Null)
    }

    /// The person says `text` in `thread`, as the page posts it on `say`.
    fn say(&self, id: &str, thread: &str, text: &str, persona: Option<&str>) -> Result<()> {
        let mut body = json!({ "text": text, "thread": thread });
        if let Some(p) = persona {
            body["persona"] = json!(p);
        }
        let r = self.api.signed(self.owner, "POST", &format!("/api/f/{}/channels/say", self.name), Some(&json!({ "id": id, "body": body })))?;
        anyhow::ensure!(r.status == 200, "saying {id} on say: {r}");
        Ok(())
    }

    fn messages(&self, thread: &str) -> Vec<Value> {
        self.op("thread", json!({ "id": thread }))["messages"].as_array().cloned().unwrap_or_default()
    }

    /// The thread's messages once its turn is over: a `talk` after its last
    /// `user` message, and no turn running or queued.
    fn answered(&self, thread: &str) -> Option<Vec<Value>> {
        let said = self.messages(thread);
        let last = said.iter().rposition(|m| m["kind"] == "user")?;
        said[last..].iter().any(|m| m["kind"] == "talk").then_some(())?;
        let status = self.op("status", json!({}));
        (status["turn"].is_null() && status["queued"] == 0).then_some(said)
    }

    /// The thread's newest task (`done`: once it is no longer running).
    fn task(&self, thread: &str, done: bool) -> Option<Value> {
        let task = self.op("tasks", json!({ "thread": thread }))["tasks"].get(0).cloned()?;
        (!done || task["state"] != "running").then_some(task)
    }
}

/// The text of the thread's last `talk`.
fn last_talk(said: &[Value]) -> &str {
    said.iter().rev().find(|m| m["kind"] == "talk").and_then(|m| m["text"].as_str()).unwrap_or("")
}

/// Seconds from the thread's last `user` message to its last `talk`, on
/// the mind's clock.
fn turn_secs(said: &[Value]) -> f64 {
    let at = |kind: &str| said.iter().rev().find(|m| m["kind"] == kind).and_then(|m| m["at"].as_i64()).unwrap_or(0);
    (at("talk") - at("user")) as f64 / 1000.0
}

fn thread_id() -> String {
    format!("t_{}", fragment_devstack::random_hex(8))
}

/// The person's first run as the shell makes it (cell/shell/shell.js
/// `defaultAgent`): their skills fragment, their computer, the agent with
/// its SOUL assigned to it, and their mind (members), the agent an editor
/// there, then a wake. Answers the computer and the mind's name.
fn first_run(s: &Suite, api: &Api, owner: &Keys) -> Result<(Value, String)> {
    let username = api.username(owner)?;
    let skills = api.create_with(owner, json!({ "name": s.name("skills"), "template": "skills" }))?;
    anyhow::ensure!(skills.status == 200, "their skills fragment: {skills}");
    let computer = api.signed(owner, "POST", "/api/computers", Some(&json!({})))?;
    let id = computer.body["computer"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(computer.status == 200 && id.starts_with("computer:"), "their computer: {computer}");
    let agent = api.create_with(owner, json!({ "name": s.name("agent"), "template": "agent", "title": TITLE }))?;
    anyhow::ensure!(agent.status == 200, "their agent: {agent}");
    let agent = agent.body["name"].as_str().unwrap_or("").to_string();
    // assigned before its computer's first start, which then runs it
    let assigned = api.signed(owner, "PUT", &format!("/api/computers/{id}/agents/{agent}"), Some(&json!({})))?;
    let of = assigned.body["agents"].as_array().and_then(|a| a.iter().find(|x| x["fragment"] == agent.as_str()).cloned()).unwrap_or_default();
    let identity = of["identity"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(assigned.status == 200 && identity.starts_with("id:"), "assigning the agent: {assigned}");
    let agent_json = format!("{}\n", serde_json::to_string_pretty(&json!({ "tier": "medium", "color": color_of(&identity) }))?);
    let files = json!({ "key": "agent-default", "message": "the default agent", "files": [{ "path": "SOUL.md", "text": first_soul(&username) }, { "path": "agent.json", "text": agent_json }] });
    let r = api.signed(owner, "POST", &format!("/api/f/{agent}/files"), Some(&files))?;
    anyhow::ensure!(r.status == 200, "the agent's SOUL: {r}");
    let r = api.signed(owner, "POST", &format!("/api/f/{agent}/deploy"), Some(&json!({})))?;
    anyhow::ensure!(r.status == 200, "the agent's deploy: {r}");
    let mind = api.create_with(owner, json!({ "name": s.name("mind"), "template": "mind", "visibility": "members", "title": "Mind" }))?;
    anyhow::ensure!(mind.status == 200, "their mind: {mind}");
    let name = mind.body["name"].as_str().unwrap_or("").to_string();
    // adding it to the mind is what wakes the computer (the platform's `joined`)
    let joined = api.signed(owner, "PUT", &format!("/api/f/{name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(joined.status == 200, "the agent joining the mind: {joined}");
    let _ = api.signed(owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})));
    Ok((computer.body, name))
}

pub fn mind_live(s: &mut Suite, api: &Api) -> Result<()> {
    let why = format!("it runs the mind on real models and goose on a computer from cold, for up to half an hour, and spends up to {PAID_CALLS} of the run's paid calls");
    if !s.section_by_name(SECTION, &[Need::Levers, Need::Computers, Need::Models, Need::RealModels], &why) {
        return Ok(());
    }
    let owner = api.person_paying(PAID_CALLS)?;
    let identity = api.identity(&owner)?;
    let (computer, name) = first_run(s, api, &owner)?;
    let id = computer["computer"].as_str().unwrap_or("").to_string();
    let image = computer["image"].as_str().unwrap_or("").to_string();
    println!("      ({identity}, lent {PAID_CALLS} paid calls: their mind {}, their computer on {image:?})", api.site_url(&name, ""));
    let m = Mind { api, owner: &owner, name };
    let personas = within(INSTALL, || Some(m.op("personas", json!({}))).filter(|p| p["personas"].as_array().is_some_and(|l| !l.is_empty())));
    s.ok(
        "their first run, as the shell makes it: their computer on the deployment's own image, their agent on it, and their mind running the template, the agent an editor",
        personas.is_some() && !image.is_empty() && image != "stub",
        json!({ "computer": computer, "personas": personas }),
    );
    let Some(personas) = personas else { return Ok(()) };

    // ---- thread A: a fact, answered
    let (a, b, c) = (thread_id(), thread_id(), thread_id());
    m.say("a1", &a, FACT, None)?;
    let said_a = within(REPLY, || m.answered(&a));
    s.ok("thread A: the mind answers a fact it is told", said_a.is_some(), json!(m.messages(&a)));
    let Some(said_a) = said_a else { return Ok(()) };
    let reply_a = turn_secs(&said_a);

    // ---- the compactor settles; thread B asks for the fact back
    let t0 = Instant::now();
    let settled = within(SETTLE, || Some(m.op("status", json!({}))).filter(|st| st["unbuilt"] == 0 && st["turn"].is_null() && st["queued"] == 0));
    let settle = t0.elapsed().as_secs_f64();
    s.ok("the compactor settles: nothing in the view is left unbuilt", settled.is_some(), m.op("status", json!({})));
    m.say("b1", &b, RECALL, None)?;
    let said_b = within(REPLY, || m.answered(&b)).unwrap_or_else(|| m.messages(&b));
    let answer = last_talk(&said_b);
    let tools: Vec<&str> = said_b.iter().filter(|x| x["kind"] == "tool").filter_map(|x| x["text"].as_str()?.split_whitespace().next()).collect();
    let looked = tools.iter().filter(|t| matches!(**t, "zoom" | "search")).count();
    let reply_b = turn_secs(&said_b);
    println!("      (thread B: answered in {reply_b:.1} s, {looked} zoom/search calls of its tool calls [{}])", tools.join(", "));
    s.ok("thread B, a fresh one: the mind recalls the fact from its memory, Ana in Porto", answer.contains("Ana") && answer.contains("Porto"), json!(said_b));

    // ---- a topic: Clef sorts thread A into it
    let added = m.op("topic_add", json!({ "name": "Family", "description": "relatives, family members" }));
    let topic = added["id"].as_str().unwrap_or("").to_string();
    let t0 = Instant::now();
    let sorted = !topic.is_empty() && within(SORT, || m.op("threads", json!({ "topic": topic }))["threads"].as_array()?.iter().any(|t| t["id"] == a.as_str()).then_some(())).is_some();
    let sort = t0.elapsed().as_secs_f64();
    s.ok(
        "a topic added, Family: Clef sorts thread A into it",
        sorted,
        json!({ "added": added, "threads": m.op("threads", json!({ "topic": topic })), "topics": m.op("topics", json!({})) }),
    );

    // ---- thread C: Builder hands shell work to goose
    let builder = personas["personas"].as_array().and_then(|l| l.iter().find(|p| p["hands"] == true)).and_then(|p| p["id"].as_str()).unwrap_or("").to_string();
    let status = m.op("status", json!({}));
    s.ok("a persona has hands (Builder), and the mind sees its agent", !builder.is_empty() && status["hands"] == true, json!({ "personas": personas, "status": status }));
    m.say("c1", &c, WORK, Some(builder.as_str()).filter(|b| !b.is_empty()))?;
    // a turn over with no task answered in words: no need to wait for one
    let opened = within(REPLY, || m.task(&c, false).or_else(|| m.answered(&c).map(|_| Value::Null))).unwrap_or_default();
    let then = phase(api, &owner, &id);
    s.ok("thread C, as Builder: the mind hands the work to its computer, a task", opened.is_object(), json!({ "messages": m.messages(&c), "computer": then }));
    if !opened.is_object() {
        return Ok(());
    }
    // the ask as the mind logged it, on its clock
    let posted = m.messages(&c).iter().find(|x| x["kind"] == "user").and_then(|x| x["at"].as_i64()).unwrap_or(0);
    let task = opened["id"].as_str().unwrap_or("").to_string();
    println!("      (task {task} opened {:.1} s after the ask, the computer {then})", (opened["started"].as_i64().unwrap_or(0) - posted) as f64 / 1000.0);
    let ended = within(HANDOFF, || m.task(&c, true)).unwrap_or(opened);
    let report = ended["report"].as_str().unwrap_or("");
    let handoff = (ended["ended"].as_i64().unwrap_or(0) - posted) as f64 / 1000.0;
    s.ok(
        "goose runs the commands on the computer, and the task is done, its report with both outputs (Linux, 42)",
        ended["state"] == "done" && report.contains("Linux") && report.contains("42"),
        json!({ "task": ended, "computer": phase(api, &owner, &id) }),
    );
    let said_c = within(REPLY, || m.answered(&c).filter(|said| said.iter().any(|x| x["kind"] == "user" && x["task"] == task.as_str())));
    let follow = said_c.as_deref().map_or(0.0, turn_secs);
    s.ok("the report comes back to thread C, and the mind follows up there", said_c.is_some(), json!(m.messages(&c)));

    // ---- what it took
    let (models, steps) = (entries(api, &identity, "aig:").len(), entries(api, &identity, "step:").len());
    println!(
        "      (mind-live: thread A answered in {reply_a:.1} s; settled {settle:.0} s later; thread B answered in {reply_b:.1} s with {looked} zoom/search calls; \
         Family sorted thread A in {sort:.0} s; the hand-off reported {handoff:.0} s after the ask, followed up in {follow:.1} s; \
         paid calls: {models} model calls (goose's) and {steps} AI steps (the mind's) of {PAID_CALLS} lent)"
    );
    let _ = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})));
    Ok(())
}
