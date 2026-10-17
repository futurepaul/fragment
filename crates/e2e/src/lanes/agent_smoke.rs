//! A real agent on real infrastructure (Paul, 2026-10-05: "are you testing
//! these features manually or just shipping and hoping it works"): the
//! flows a person uses, against a branch preview, on a Hermes computer on
//! Cloudflare Containers, its real model, and Chrome. The hosted lane runs
//! it by name (`cargo xtask e2e --hosted … --only agent-smoke`): it runs an
//! agent for about half an hour and spends up to `PAID_CALLS` of the run's
//! paid calls. A local run never has a real agent (needs.rs `RealAgent`):
//! the hermes section runs the same image on the scripted model.
//!
//! An e2e person gets their default agent as the shell makes it
//! (cell/shell/shell.js, `defaultAgent`), through the API, then:
//!
//! 1. "hello" is answered, timed from asleep;
//! 2. asked to list their fragments with the CLI, its turn runs a command
//!    naming `fragment` in its terminal (`ran`), and its managed skills
//!    (its owner's skills fragment's) are installed;
//! 3. asked for a todo app, the app is theirs and live, and an open shell
//!    shows it with no reload (the live sidebar, #158);
//! 4. its chat's agent's screen is the one served (the RFB stream's desktop
//!    is that agent's, the page names it, another agent's is refused);
//!    asked to open a browser, its screen (a ticket, in Chrome) connects,
//!    draws a frame that is not blank, follows the desktop while open,
//!    takes Take over, and shows the browser opened again;
//! 5. nothing technical asks its owner: asked to make a folder, then to
//!    delete it, the recursive delete, which Hermes flags, runs with no
//!    card, and the next message is answered;
//! 6. after its desktop ran, its work's end is saved awake, held (its
//!    guest answered the hold); asleep at its owner's first ask (nothing of
//!    its own, its guest's sockets included, starts it again), every save
//!    it keeps held, a message wakes it, its wake restored the sleep's
//!    save, and what it made before the sleep is still in `/data/work`;
//! 7. its paid calls stay within what the run lent it, and the run says
//!    what it cost;
//! 8. it sleeps at the end (`--sweep` deletes the run's `e2e-<run>-`
//!    fragments).
//!
//! A check that depends on what the model chooses to do says so in its
//! text. Every wait is a real model's: generous, a positive looked for
//! every few seconds, and one that runs out says what the model did not do.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::computers::{agent_replies, lever, phase, turn_of, work_of, QUEUE_DRAIN};
use super::jobs::records;
use super::ledger::entries;
use crate::api::{now_s, Api, Call, Socket};
use crate::browser::{Browser, Lease, Page};
use crate::{Need, Suite};

pub const SECTION: &str = "agent-smoke";

/// The paid calls the run lends the section's person: its turns' model
/// calls (each tool call is one, Hermes' titles one each; its guardian, one
/// a flagged command, is off since 2026-10-14). Measured on master
/// (2026-10-05): 49, 31 of them the browser's turn.
/// Its ledger refuses the one past it; the run's default budget is 60.
const PAID_CALLS: u64 = 56;
/// A new computer's first start on a preview (the 3.8 GB image pulled to
/// the host, Hermes booted) to its agent following its chat.
const FIRST_START: Duration = Duration::from_secs(10 * 60);
/// A turn that answers in words.
const REPLY: Duration = Duration::from_secs(5 * 60);
/// A turn that does things: an app made and deployed, a browser opened.
const WORK: Duration = Duration::from_secs(12 * 60);
/// A sleeping computer's wake, to its agent's reply.
const WAKE: Duration = Duration::from_secs(10 * 60);
/// Its owner's sleep (a save of `/data`), asked until it is asleep.
const SLEEP: Duration = Duration::from_secs(3 * 60);
/// A turn's end to its awake save: the settle, a model call Hermes makes
/// after its reply, the hold and the save of its `/data`.
const SAVED: Duration = Duration::from_secs(fragment_core::computer::SAVE_SETTLE_MS as u64 / 1000 + 150);
/// The shell's page, and the screen's, to connect and draw.
const PAGE: Duration = Duration::from_secs(90);
/// An open page to show what changed (a new app, the browser).
const FOLLOW: Duration = Duration::from_secs(60);
/// One look every few seconds: each is a request to the preview.
const POLL: Duration = Duration::from_secs(3);

/// The shell's first agent's soul (which never names it) and its first
/// chat's title (cell/shell/shell.js: `firstSoul`, `FIRST_CHAT`), so the
/// agent is made as a person's is.
const TITLE: &str = "Novatron DX";
const FIRST_CHAT: &str = "Your first chat";

fn first_soul(owner: &str) -> String {
    format!("You are {owner}'s main agent: the one they talk to, and their go-between with everything else here. You are capable: do most things yourself, and do them well.\n\nYou also lead their team of agents (the agents-finite skill). Give a job an agent of its own when that is better: work that lasts and has a context of its own (a newsletter, their finances, a side project), long work that can run while you keep talking with them, or a model they ask for. Make teammates on the same platform models you run on, unless they ask for another provider. Brief each one, check what comes back, and tell them the result in your own words, saying who did what. Keep the team small, keep each one's job current, and stop one nobody needs: they never have to manage agents themselves.\n\nNever give a teammate on another provider their private things: give it placeholders instead. When a job would be better as an app, offer to make it. The first time you talk, say hello briefly and ask what they'd like to start with.\n")
}

fn mins(d: Duration) -> String {
    match d.as_secs() {
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
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
            println!("      (a wait ran out its {} at {}:{})", mins(bound), at.file(), at.line());
            return None;
        }
        std::thread::sleep(POLL);
    }
}

/// The agent's chat, as its owner reads it.
struct Chat<'a> {
    api: &'a Api,
    owner: &'a Keys,
    /// The chat fragment, and the agent fragment and identity answering it.
    name: String,
    agent: String,
    identity: String,
    /// Each step's turn as it ended (`noted`): the run's evidence.
    turns: std::cell::RefCell<Vec<Value>>,
}

impl Chat<'_> {
    /// The turn of step `step` as it is now, kept for the evidence file
    /// and said in a line: its tools in order, how it ended, its paid calls.
    fn noted(&self, step: &str, turn: &str, paid_calls: usize) {
        let summary = self.summary(turn);
        let tools: Vec<String> = summary["work"].as_array().into_iter().flatten().filter(|w| w["kind"] == "turn.step").filter_map(|w| w["tool"].as_str().map(str::to_string)).collect();
        let end = summary["work"].as_array().into_iter().flatten().find(|w| w["kind"] == "turn.end").map(|w| w["outcome"].clone()).unwrap_or(Value::Null);
        println!("      ({step}: {} steps [{}], ended {end}, {paid_calls} paid calls)", tools.len(), tools.join(", "));
        self.turns.borrow_mut().push(json!({ "step": step, "turn": turn, "paidCalls": paid_calls, "summary": summary }));
    }

    /// The owner's message `id`: the turn it starts.
    fn say(&self, id: &str, text: &str) -> Result<String> {
        let r = self.api.signed(self.owner, "POST", &format!("/api/f/{}/channels/chat", self.name), Some(&json!({ "id": id, "body": { "text": text } })))?;
        anyhow::ensure!(r.status == 200, "the owner's message {id}: {r}");
        let seq = r.body["record"]["seq"].as_i64().context("a post answers its record's seq")?;
        Ok(turn_of(&self.agent, &self.name, "chat", seq))
    }

    fn replies(&self, turn: &str) -> Vec<String> {
        agent_replies(&records(self.api, self.owner, &self.name, "chat"), &self.identity)
            .into_iter()
            .filter(|r| r["body"]["turn"] == turn)
            .filter_map(|r| r["body"]["text"].as_str().map(str::to_string))
            .collect()
    }

    /// The turn's records on `work`, their bodies.
    fn work(&self, turn: &str) -> Vec<Value> {
        work_of(&records(self.api, self.owner, &self.name, "work"), turn).into_iter().map(|r| r["body"].clone()).collect()
    }

    fn of_kind(&self, turn: &str, kind: &str) -> Vec<Value> {
        self.work(turn).into_iter().filter(|b| b["kind"] == kind).collect()
    }

    fn ended(&self, turn: &str) -> Option<Value> {
        self.of_kind(turn, "turn.end").into_iter().next()
    }

    /// Waits for the turn to end; one that has not by `bound` is stopped
    /// (its asker's Stop), so the next message is not queued behind it.
    fn finish(&self, turn: &str, bound: Duration) -> Option<Value> {
        let end = within(bound, || self.ended(turn));
        if end.is_none() {
            let stop = json!({ "id": format!("stop-{}", &turn[..12.min(turn.len())]), "body": { "kind": "stop", "turn": turn } });
            let r = self.api.signed(self.owner, "POST", &format!("/api/f/{}/channels/chat", self.name), Some(&stop));
            println!("      (the turn did not end within {}: its asker stopped it, {})", mins(bound), r.map(|r| r.status.to_string()).unwrap_or_else(|e| format!("{e:#}")));
        }
        end
    }

    /// What the turn did, for a FAIL's detail: its steps, prompts, end and replies.
    fn summary(&self, turn: &str) -> Value {
        let work: Vec<Value> = self.work(turn).into_iter().filter(|b| b["kind"] != "turn.start").collect();
        json!({ "work": work, "replies": self.replies(turn) })
    }

    /// Why a wait for the model ran out, or the turn as it is.
    fn why(&self, turn: &str, done: bool, what: &str, bound: Duration) -> String {
        match done {
            true => self.summary(turn).to_string(),
            false => format!("the model did not {what} within {}: {}", mins(bound), self.summary(turn)),
        }
    }
}

/// A computer's view, as its owner reads it.
fn view(api: &Api, owner: &Keys, id: &str) -> Value {
    api.signed(owner, "GET", &format!("/api/computers/{id}"), None).map(|r| r.body).unwrap_or(Value::Null)
}

/// The owner's sleep, asked until it is asleep (a record a turn posted
/// last may wake it again): whether it slept, the last answer, and each
/// answer's phase in order.
fn sleep(api: &Api, owner: &Keys, id: &str) -> (bool, Value, Vec<String>) {
    std::thread::sleep(QUEUE_DRAIN);
    let mut last = Value::Null;
    let mut answers = vec![];
    let slept = within(SLEEP, || {
        let r = api.signed(owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({}))).ok()?;
        last = r.body;
        answers.push(last["phase"].as_str().unwrap_or("none").to_string());
        (last["phase"] == "asleep").then_some(())
    });
    (slept.is_some(), last, answers)
}

/// Every save its computer keeps (at most three: the sleep's just taken,
/// and the awake ones before it) was held: its guest, the deployment's own
/// image, answered each hold (docs/computers.md, "The hold"), so no save
/// carries a hot copy of a database Hermes was writing.
fn held_saves(s: &mut Suite, api: &Api, id: &str) {
    let saves = lever(api, id, "saves").map(|r| r.body["saves"].clone()).unwrap_or(Value::Null);
    let all = saves.as_array().cloned().unwrap_or_default();
    println!("      (its saves kept, newest first: {})", all.iter().map(|x| format!("{} held {}", x["number"], x["held"])).collect::<Vec<_>>().join(", "));
    s.ok("every save its computer keeps was held: its guest answered each hold", !all.is_empty() && all.iter().all(|x| x["held"] == true), &saves);
}

/// The app `name`, once it is its owner's in their list and live.
/// The person's own fragment labelled `label`, once it is live: its list
/// row and live pin (the agent made it from a label: its suffix is the
/// platform's).
pub(super) fn live_app(api: &Api, owner: &Keys, label: &str) -> Option<Value> {
    let listed = api.signed(owner, "GET", "/api/fragments", None).ok()?;
    let labelled = |f: &&Value| f["name"].as_str().and_then(fragment_proto::split_fragment_name).is_some_and(|(l, _)| l == label);
    let row = listed.body["fragments"].as_array()?.iter().filter(labelled).find(|f| f["role"] == "owner")?.clone();
    let name = row["name"].as_str()?.to_string();
    let status = api.signed(owner, "GET", &format!("/api/f/{name}/status"), None).ok()?;
    let live = status.body["pins"]["live"].as_str().filter(|l| !l.is_empty())?.to_string();
    Some(json!({ "listed": row, "live": live }))
}

/// What the person's paid calls were, from their ledger: model calls and AI steps.
fn paid(api: &Api, identity: &str) -> usize {
    entries(api, identity, "aig:").len() + entries(api, identity, "step:").len()
}

/// Whether a turn's step ran a command in the agent's terminal that `is`
/// holds: a `terminal` step, its arguments the command, or an
/// `execute_code` step whose script calls Hermes' `terminal(...)` with it
/// (`from hermes_tools import terminal; terminal("ls -l …")`, as canary
/// round 5's agent ran every command): either is the agent running it. A
/// step's arguments are Hermes' preview of its call, cut at the bridge's
/// `STEP_ARGS_MAX_CHARS`: a call cut short is read to the cut.
pub(super) fn ran(step: &Value, is: impl Fn(&str) -> bool) -> bool {
    let args = step["args"].as_str().unwrap_or("");
    match step["tool"].as_str() {
        Some("terminal") => is(args),
        Some("execute_code") => terminal_calls(args).into_iter().any(is),
        _ => false,
    }
}

/// The arguments of each `terminal(...)` call a Python script makes: its
/// text between the call's parentheses, or to the script's end where it
/// was cut. A string is read as one: a `)` in a command ends no call.
fn terminal_calls(code: &str) -> Vec<&str> {
    const CALL: &str = "terminal(";
    let mut calls = vec![];
    let mut from = 0;
    // bounded by the script: each pass moves past one `terminal(`
    while let Some(at) = code[from..].find(CALL).map(|i| from + i) {
        let start = at + CALL.len();
        from = start;
        // Hermes' `terminal`, not `my_terminal`
        if code[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let (mut depth, mut quote, mut escaped, mut end) = (1usize, None::<char>, false, code.len());
        for (i, c) in code[start..].char_indices() {
            match quote {
                Some(_) if escaped => escaped = false,
                Some(_) if c == '\\' => escaped = true,
                Some(q) if c == q => quote = None,
                Some(_) => {}
                None if c == '"' || c == '\'' => quote = Some(c),
                None if c == '(' => depth += 1,
                None if c == ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i;
                        break;
                    }
                }
                None => {}
            }
        }
        calls.push(&code[start..end]);
    }
    calls
}

// ---- the screen, as its page draws it

/// The screen's page's status line, as the person reads it.
const STATUS: &str = "document.getElementById('status')?.textContent ?? ''";
/// The page is connected (its RFB stream open), whoever holds control.
const CONNECTED: &str = "['watching', 'mine', 'theirs'].includes(document.body.dataset.state ?? '')";
/// A grid of the noVNC canvas's pixels (its framebuffer's), or null before
/// it has one.
const FRAME_JS: &str = "(() => { const c = document.querySelector('#screen canvas'); if (!c || !c.width || !c.height) return null; \
    const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data; const px = []; \
    for (let y = 0; y < 30; y++) for (let x = 0; x < 48; x++) { \
    const i = (Math.floor((y + 0.5) * c.height / 30) * c.width + Math.floor((x + 0.5) * c.width / 48)) * 4; px.push([d[i], d[i + 1], d[i + 2]]); } \
    return { width: c.width, height: c.height, px }; })()";
/// Two samples within this of each other in every channel are the same
/// colour (an RFB stream may be lossy).
const TOLERANCE: u8 = 24;
/// A frame whose most common colour covers this much of it is blank.
const BLANK_SHARE: f64 = 0.98;
/// A frame unlike another in this much of it shows something new (a
/// browser's window over a desktop); like it in all but this, the same.
const CHANGED: f64 = 0.10;
const SAME: f64 = 0.25;
/// A sample this light in every channel is a light page's (example.com's
/// is #f0f0f2 on #fdfdff); the desktop is a dark gradient (#0d1015 most).
const LIGHT: u8 = 200;
/// The browser shows: light samples cover this much more than before.
const LIGHTER: f64 = 0.10;

/// A sampled frame of the screen.
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    width: u64,
    height: u64,
    px: Vec<[u8; 3]>,
}

impl Frame {
    fn read(chrome: &mut Browser, page: &Page) -> Option<Frame> {
        let v = chrome.eval(page, FRAME_JS).ok()?;
        Frame::parse(&v)
    }

    fn parse(v: &Value) -> Option<Frame> {
        let px: Vec<[u8; 3]> = v["px"].as_array()?.iter().filter_map(|p| Some([p[0].as_u64()? as u8, p[1].as_u64()? as u8, p[2].as_u64()? as u8])).collect();
        let (width, height) = (v["width"].as_u64()?, v["height"].as_u64()?);
        (width > 0 && height > 0 && !px.is_empty()).then_some(Frame { width, height, px })
    }

    fn alike(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= TOLERANCE)
    }

    /// The most common colour, and the share of samples alike to it.
    fn dominant(&self) -> ([u8; 3], f64) {
        let mut counts: std::collections::BTreeMap<[u8; 3], usize> = std::collections::BTreeMap::new();
        for p in &self.px {
            *counts.entry(*p).or_default() += 1;
        }
        let mode = counts.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c).unwrap_or([0, 0, 0]);
        let alike = self.px.iter().filter(|p| Frame::alike(**p, mode)).count();
        (mode, alike as f64 / self.px.len().max(1) as f64)
    }

    /// One colour, nearly everywhere: nothing drawn on it.
    fn blank(&self) -> bool {
        self.dominant().1 >= BLANK_SHARE
    }

    /// The share of light samples (a light page's).
    fn light(&self) -> f64 {
        self.px.iter().filter(|p| p.iter().all(|c| *c >= LIGHT)).count() as f64 / self.px.len().max(1) as f64
    }

    /// Whether it shows a light page (a browser's) where `desktop` showed
    /// none: not blank, unlike it, and lighter.
    fn shows_a_page_over(&self, desktop: Option<&Frame>) -> bool {
        let base = desktop.map_or(0.0, Frame::light);
        !self.blank() && desktop.is_none_or(|d| self.unlike(d) >= CHANGED) && self.light() >= base + LIGHTER
    }

    /// The share of samples unlike `other`'s (a frame of another size is
    /// wholly unlike).
    fn unlike(&self, other: &Frame) -> f64 {
        if (self.width, self.height, self.px.len()) != (other.width, other.height, other.px.len()) {
            return 1.0;
        }
        let differ = self.px.iter().zip(&other.px).filter(|(a, b)| !Frame::alike(**a, **b)).count();
        differ as f64 / self.px.len() as f64
    }

    fn describe(&self) -> String {
        let (mode, share) = self.dominant();
        format!("{}x{}, {:.0}% #{:02x}{:02x}{:02x}, {:.0}% light", self.width, self.height, share * 100.0, mode[0], mode[1], mode[2], self.light() * 100.0)
    }
}

fn describe(frame: &Option<Frame>) -> String {
    frame.as_ref().map(Frame::describe).unwrap_or_else(|| "no frame".into())
}

/// A one-time ticket to `agent`'s screen (port 6080, at `?agent=`), as
/// the shell's `openScreen` mints it.
fn ticket(api: &Api, owner: &Keys, id: &str, agent: &str) -> Result<String> {
    let r = api.signed(owner, "POST", &format!("/api/computers/{id}/ports/6080/ticket"), Some(&json!({ "path": format!("/?agent={agent}") })))?;
    anyhow::ensure!(r.status == 200, "a ticket to its screen: {r}");
    Ok(r.body["url"].as_str().unwrap_or("").to_string())
}

/// `agent`'s screen's page through a new ticket, connected or not by `PAGE`.
pub(super) fn open_screen(chrome: &mut Browser, api: &Api, owner: &Keys, id: &str, agent: &str) -> Result<(Page, bool)> {
    let page = chrome.open(&ticket(api, owner, id, agent)?)?;
    let connected = chrome.until(&page, CONNECTED, PAGE);
    Ok((page, connected))
}

/// Hermes' profile for an agent fragment (the bridge's `wire::profile`, for
/// a name of lowercase letters, digits, `-` and one `.`, as an e2e agent's
/// is): its desktop's name is `hermes:<profile>`.
fn hermes_profile(agent: &str) -> String {
    agent.replace('.', "-")
}

fn status(chrome: &mut Browser, page: &Page) -> String {
    chrome.eval(page, STATUS).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

pub fn agent_smoke(s: &mut Suite, api: &Api) -> Result<()> {
    let why = format!("it runs a real agent on a Hermes computer for about half an hour, and spends up to {PAID_CALLS} of the run's paid calls");
    if !s.section_by_name(SECTION, &[Need::Levers, Need::Computers, Need::Models, Need::RealAgent], &why) {
        return Ok(());
    }
    anyhow::ensure!(api.signs_in_by_levers(), "a real agent's person signs in through a preview's levers");
    let started_s = now_s();
    // its evidence: each step's turn (turns.json) and the screen's shots
    let evidence = s.dir(SECTION);

    // ---- an e2e person, and their default agent as the shell makes it
    let keys = Keys::generate();
    let (session, owner_id) = api.e2e_sign_in(&Api::email_of(&keys), PAID_CALLS)?;
    let me = api.approve(&session, &keys)?;
    let email = me.body["email"].as_str().context("the person has an email")?.to_string();
    println!("      ({email}, lent {PAID_CALLS} paid calls)");
    let skills = api.create_with(&keys, json!({ "label": s.name("skills"), "template": "skills" }))?;
    let made = api.signed(&keys, "POST", "/api/computers", Some(&json!({})))?;
    let t_asleep = Instant::now();
    let id = made.body["computer"].as_str().unwrap_or("").to_string();
    super::computers::on_candidate(s, api, &keys, &id)?;
    let agent_label = s.name("agent");
    let agent = api.create_with(&keys, json!({ "label": agent_label, "template": "agent", "title": TITLE }))?;
    let agent_name = agent.body["name"].as_str().unwrap_or("").to_string();
    // assigned before its computer's first start, which then runs it
    let assigned = api.signed(&keys, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = assigned.body["agents"].as_array().and_then(|a| a.iter().find(|x| x["fragment"] == agent_name.as_str())).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    let agent_json = format!("{}\n", serde_json::to_string_pretty(&json!({ "tier": "medium" }))?);
    let files = json!({ "key": "agent-default", "message": "the default agent", "files": [{ "path": "SOUL.md", "text": first_soul(&email) }, { "path": "agent.json", "text": agent_json }] });
    let soul = api.signed(&keys, "POST", &format!("/api/f/{agent_name}/files"), Some(&files))?;
    let deployed = api.signed(&keys, "POST", &format!("/api/f/{agent_name}/deploy"), Some(&json!({})))?;
    let chat_made = api.create_with(&keys, json!({ "label": format!("{agent_label}-chat"), "template": "chat", "title": FIRST_CHAT }))?;
    let chat_name = chat_made.body["name"].as_str().unwrap_or("").to_string();
    // adding it to its chat is what wakes the computer (the platform's `joined`)
    let joined = api.signed(&keys, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    let _ = api.signed(&keys, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})));
    let statuses = [&skills, &made, &agent, &assigned, &soul, &deployed, &chat_made, &joined].map(|r| r.status);
    let set_up = statuses.iter().all(|s| *s == 200) && made.body["phase"] == "asleep" && made.body["image"] != "stub" && fragment_core::npub::is_identity(&identity);
    s.ok(
        "an e2e person's default agent, made as the shell makes it: their skills fragment, their computer (asleep, on the deployment's own image), the agent fragment with its SOUL and tier, assigned to it, and its chat with the agent in it",
        set_up,
        json!({ "statuses": statuses, "computer": made.body, "assigned": assigned.body }),
    );
    if !set_up {
        println!("      (no agent: the section stops here)");
        return Ok(());
    }
    let c = Chat { api, owner: &keys, name: chat_name.clone(), agent: agent_name.clone(), identity: identity.clone(), turns: Default::default() };
    let ready = || {
        let following = || {
            api.signed(&keys, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
                .ok()
                .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["principal"] == identity.as_str() && x["channel"] == "chat")))
        };
        (phase(api, &keys, &id) == "awake" && following()).then_some(())
    };
    let is_ready = within(FIRST_START, ready).is_some();
    println!("      (asleep to ready, its first start: {:.1?})", t_asleep.elapsed());
    s.ok(
        &format!("adding the agent to its chat wakes its computer, and the agent follows its chat within {} (the shell's ready)", mins(FIRST_START)),
        is_ready,
        view(api, &keys, &id),
    );
    if !is_ready {
        println!("      (no computer up: the section stops here)");
        let _ = sleep(api, &keys, &id);
        return Ok(());
    }
    let mut spent: Vec<(&str, usize)> = vec![];
    let mut calls = paid(api, &owner_id);
    let mut count = |api: &Api, step: &'static str, turn: &str, spent: &mut Vec<(&str, usize)>| {
        let now = paid(api, &owner_id);
        spent.push((step, now.saturating_sub(calls)));
        c.noted(step, turn, now.saturating_sub(calls));
        calls = now;
    };

    // ---- 1. a first reply, timed from asleep
    let t1 = Instant::now();
    let hello = c.say("smoke-1", "hello")?;
    let reply = within(REPLY, || c.replies(&hello).into_iter().find(|t| !t.trim().is_empty()));
    println!("      (hello to its reply: {:.1?}; asleep to its first reply: {:.1?})", t1.elapsed(), t_asleep.elapsed());
    s.ok(&format!("\"hello\" gets a reply on its chat within {} of ready (model-dependent)", mins(REPLY)), reply.is_some(), c.why(&hello, reply.is_some(), "reply", REPLY));
    let end = c.finish(&hello, REPLY);
    let started = !c.of_kind(&hello, "turn.start").is_empty();
    s.ok("its turn starts and ends idle on work", started && end.as_ref().is_some_and(|e| e["outcome"] == "idle"), c.why(&hello, end.is_some(), "end its turn", REPLY));
    count(api, "hello", &hello, &mut spent);

    // ---- 2. the agent knows its platform: the fragment CLI, in its terminal
    let listing = c.say("smoke-2", "Use the fragment CLI to list my fragments.")?;
    let end = c.finish(&listing, WORK);
    let listed = c.of_kind(&listing, "turn.step").iter().any(|st| ran(st, |cmd| cmd.contains("fragment")));
    s.ok(
        "asked to list their fragments with the fragment CLI, its turn ran a command naming `fragment` in its terminal, a step on work (model-dependent)",
        listed,
        c.why(&listing, end.is_some(), "end its turn", WORK),
    );
    count(api, "list", &listing, &mut spent);
    // the managed set, installed from the owner's skills fragment at its
    // first start (decision 17; its hosted listing is #163's): only an ls
    // of it, or the skills index it feeds, names apps-finite
    let managed = c.say("smoke-2b", "Run `ls /data/hermes/managed-skills/*/` in your terminal and tell me the skill names it lists.")?;
    let end = c.finish(&managed, REPLY);
    let named = c.replies(&managed).iter().any(|t| t.contains("apps-finite"));
    s.ok(
        "its managed skills are installed from its owner's skills fragment: asked to list them, it names apps-finite, which only the managed set has (model-dependent)",
        named,
        c.why(&managed, end.is_some(), "end its turn", REPLY),
    );
    count(api, "skills", &managed, &mut spent);

    // ---- 3. the agent makes an app, which an open shell shows
    let mut chrome: Option<Lease> = s.browser()?;
    let shell = match chrome.as_mut() {
        None => None,
        Some(b) => {
            b.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
            let page = b.open(&format!("{}/", api.base))?;
            b.viewport(&page, 1280, 800, false)?;
            let up = b.until(&page, "!document.getElementById('layout').hidden && document.querySelectorAll('#chats .row').length > 0", PAGE);
            let said = b.eval(&page, "document.body.innerText.slice(0, 300)").unwrap_or_default();
            let _ = b.screenshot(&page, &evidence.join("0-shell.png"));
            s.ok("signed in, the shell opens with their agent's chat in its sidebar", up, said);
            up.then_some(page)
        }
    };
    let app_label = s.name("todo");
    let note = format!("{app_label}.txt");
    let making = c.say(
        "smoke-3",
        &format!("Make me a todo app from the todo template called {app_label}, and deploy it. When it's live, run `date +%s > {note}` in your working directory, so we have a note of when it went live."),
    )?;
    let t3 = Instant::now();
    let live = within(WORK, || live_app(api, &keys, &app_label));
    let app_name = live.as_ref().and_then(|v| v["listed"]["name"].as_str()).unwrap_or_default().to_string();
    println!("      (asked for the app to its live: {:.1?})", t3.elapsed());
    s.ok(
        &format!("asked for a todo app called {app_label}, it is theirs within {}: in their GET /api/fragments, and live (its live pin set) (model-dependent)", mins(WORK)),
        live.is_some(),
        format!("{}; {}", live.clone().unwrap_or(Value::Null), c.why(&making, live.is_some(), "make the app live", WORK)),
    );
    let sidebar_label = "an open shell shows the new app in its desktop's dock, with no reload";
    match (chrome.as_mut(), &shell, &live) {
        (None, _, _) => s.skip(sidebar_label, "no Chrome is installed here (set CHROME_BIN)"),
        (Some(_), None, _) => s.skip(sidebar_label, "the shell did not open"),
        (Some(_), Some(_), None) => s.skip(sidebar_label, "the agent made no app to show"),
        (Some(b), Some(page), Some(_)) => {
            let row = format!("!!document.querySelector('#apps [data-key=\"app:{app_name}\"]')");
            let shown = b.until(page, &row, FOLLOW);
            let apps = b.eval(page, "[...document.querySelectorAll('#apps [data-key]')].map((r) => r.dataset.key ?? r.textContent)").unwrap_or_default();
            let _ = b.screenshot(page, &evidence.join("0-shell-after-the-app.png"));
            s.ok(sidebar_label, shown, format!("not shown {} after it was live; the sidebar's apps: {apps}", mins(FOLLOW)));
        }
    }
    if let (Some(b), Some(page)) = (chrome.as_mut(), shell) {
        b.close(page)?;
    }
    let _ = c.finish(&making, WORK);
    count(api, "app", &making, &mut spent);

    // ---- 4. its desktop and its screen: the chat's agent's own
    let first = ticket(api, &keys, &id, &agent_name)?;
    let origin = first.split("/__ticket").next().unwrap_or("").to_string();
    let redeemed = api.call(Call { method: "GET", url: first, ..Call::default() })?;
    let cookie = redeemed.cookies().into_iter().find(|c| c.starts_with("fragment_computer=")).map(|c| c.split(';').next().unwrap_or("").to_string());
    s.ok(
        "a ticket to its chat's agent's screen lands on the computer's screen port at ?agent=<that agent>",
        redeemed.status == 303 && redeemed.header("location") == format!("/p/6080/?agent={agent_name}"),
        format!("{redeemed}"),
    );
    let t4 = Instant::now();
    let desktop = Socket::connect(api, &format!("{origin}/p/6080/websockify?viewer=e2e-smoke&agent={agent_name}"), None, cookie.as_deref(), Some(&origin)).and_then(|(mut rfb, _)| {
        rfb.patience(PAGE)?;
        let name = rfb.rfb_desktop()?;
        rfb.close();
        Ok(name)
    });
    println!("      (its screen's socket to the RFB greeting: {:.1?})", t4.elapsed());
    s.ok(
        "its agent's screen's websockify socket connects through its port (a ticket's session), and the desktop served is that agent's own (Hermes names each profile's)",
        desktop.as_deref().is_ok_and(|d| d == format!("hermes:{}", hermes_profile(&agent_name))),
        format!("{desktop:?}"),
    );
    let control = |agent: &str| Socket::connect(api, &format!("{origin}/p/6080/control?viewer=e2e-smoke&agent={agent}"), None, cookie.as_deref(), Some(&origin)).map(|(socket, _)| socket);
    let named = control(&agent_name).and_then(|mut c| {
        let first = c.next()?;
        c.close();
        Ok(first)
    });
    let other = control("nobody--k3x9").map(|_| ());
    s.ok(
        "its control socket names the chat's agent, and another agent's screen, not on this computer, is refused (404)",
        named.as_ref().is_ok_and(|w| w["agent"] == agent_name.as_str() && w["holder"].is_null()) && other.as_ref().is_err_and(|e| format!("{e:#}").contains("404")),
        format!("{named:?} / {other:?}"),
    );
    let browsing = "Open a browser on your desktop and go to example.com.";
    let browsed = match chrome.as_mut() {
        None => {
            let browsed = c.say("smoke-4", browsing)?;
            let _ = c.finish(&browsed, WORK);
            for label in [
                "the screen's page names its chat's agent, connects to that agent's desktop and draws a frame",
                "the screen left open follows the agent's desktop: with no reload it shows the browser",
                "opened now, the screen's page connects and draws a frame that is not blank",
                "Take over gives the person the screen",
                "closed and opened again, the screen shows the browser",
            ] {
                s.skip(label, "no Chrome is installed here (set CHROME_BIN)");
            }
            browsed
        }
        Some(b) => screen_checks(s, &c, b, api, &keys, &id, browsing, &evidence)?,
    };
    drop(chrome);
    count(api, "browser", &browsed, &mut spent);

    // ---- 5. nothing technical asks its owner (Paul, 2026-10-14): a
    // recursive delete, which Hermes flags, runs with no card. Its folder
    // is made first, in a turn of its own: an agent that looks before it
    // deletes (canary rounds 3 and 5) found none, and ran no `rm`.
    let scratch = s.name("scratch");
    let backups = format!("~/{scratch}-backups");
    let folder = c.say("smoke-5a", &format!("Please run this in your terminal: mkdir -p {backups}/2025 && touch {backups}/2025/notes.txt"))?;
    let end = c.finish(&folder, REPLY);
    let made = c.of_kind(&folder, "turn.step").iter().any(|st| ran(st, |cmd| cmd.contains("mkdir") && cmd.contains(&scratch)));
    s.ok(
        "asked to make a folder of backups, its turn ran the command in its terminal that makes it, and its turn ends (model-dependent)",
        made && end.is_some(),
        c.why(&folder, end.is_some(), "make it and end its turn", REPLY),
    );
    count(api, "backups", &folder, &mut spent);
    let risky = c.say("smoke-5", &format!("Please run this exact command in your terminal now: rm -rf {backups} (an old folder of backups I no longer need)"))?;
    let end = c.finish(&risky, WORK);
    let deleted = c.of_kind(&risky, "turn.step").iter().any(|st| ran(st, |cmd| cmd.contains("rm -rf") && cmd.contains(&scratch)));
    s.ok(
        "a command Hermes flags (a recursive delete) runs with no card asking its owner, and its turn ends (model-dependent: the agent runs it)",
        deleted && end.is_some() && c.of_kind(&risky, "turn.prompt").is_empty(),
        c.why(&risky, end.is_some(), "run it and end its turn", WORK),
    );
    count(api, "no approval", &risky, &mut spent);
    let next = c.say("smoke-6", "Thanks. Reply with one short sentence.")?;
    let answered = within(REPLY, || c.replies(&next).into_iter().find(|t| !t.trim().is_empty()));
    let _ = c.finish(&next, REPLY);
    s.ok("and the next message is answered (model-dependent)", answered.is_some(), c.why(&next, answered.is_some(), "reply", REPLY));
    count(api, "after", &next, &mut spent);

    // ---- its work's end is saved awake, its desktop used: the hold is
    // answered (before the hosted hold's fix, never once the desktop had
    // drawn: its Mesa cache is named `*.db`)
    let numbered = lever(api, &id, "saves").map(|r| r.body["numbered"].as_u64().unwrap_or(0)).unwrap_or(0);
    let t_saved = Instant::now();
    // what the DO said last, for a miss: a keepalive still open, a save
    // under way, or saves failing
    let mut said = Value::Null;
    let saved = within(SAVED, || {
        let r = lever(api, &id, "saves").ok()?;
        said = json!({ "keepalives": r.body["keepalives"], "saving": r.body["saving"], "unsavedSince": r.body["unsavedSince"], "generation": r.body["generation"], "numbered": r.body["numbered"] });
        r.body["numbered"].as_u64().is_some_and(|n| n > numbered).then(|| r.body["saves"][0].clone())
    });
    println!("      (its work's end to its awake save: {:.1?})", t_saved.elapsed());
    // and its owner's view, for a miss: saves failing awake are a notice
    let told = match saved.is_some() {
        true => Value::Null,
        false => {
            let v = view(api, &keys, &id);
            json!({ "phase": v["phase"], "why": v["why"], "notices": v["notices"] })
        }
    };
    s.ok(
        "after its desktop ran, its work's end is saved awake, and its guest answered the hold (held)",
        saved.as_ref().is_some_and(|x| x["held"] == true),
        json!({ "save": saved, "before": numbered, "last": said, "view": told }),
    );

    // ---- 6. asleep, a message wakes it, and its work is still there
    let rollbacks = view(api, &keys, &id)["rollbacks"].clone();
    let (slept, last, answers) = sleep(api, &keys, &id);
    let slept_s = now_s();
    println!("      (its owner's sleeps answered: {})", answers.join(", "));
    s.ok("its owner puts it to sleep (its /data saved)", slept, &last);
    held_saves(s, api, &id);
    // its guest's own sockets (opened as it boots, and again when one
    // drops, also during the sleep's save) pre-wake no computer it runs on
    s.ok(
        "its owner's first sleep answers asleep: nothing of its own starts it again from that sleep's save",
        answers.first().is_some_and(|p| p == "asleep"),
        json!({ "answers": answers, "last": last }),
    );
    let t6 = Instant::now();
    let back = c.say(
        "smoke-7",
        &format!("Are you back? Run `ls -l {note}` in your working directory, then `fragment write {app_label} site/made-at.txt --from {note}`, and tell me what they printed."),
    )?;
    let replied = within(WAKE, || c.replies(&back).into_iter().find(|t| !t.trim().is_empty()));
    println!("      (asleep, a message to its reply: {:.1?})", t6.elapsed());
    s.ok(&format!("asleep, a message wakes it, and the agent answers within {} (model-dependent)", mins(WAKE)), replied.is_some(), c.why(&back, replied.is_some(), "reply", WAKE));
    let end = c.finish(&back, WORK);
    let v = view(api, &keys, &id);
    let restored = &v["restored"];
    s.ok(
        "its wake restored the sleep's save: restored.from is a snapshot or the backup, after the sleep, with no rollback",
        ["snapshot", "backup"].contains(&restored["from"].as_str().unwrap_or("")) && restored["after"] == "sleep" && restored["rollback"] == false && v["rollbacks"] == rollbacks,
        json!({ "restored": restored, "rollbacks": v["rollbacks"], "before": rollbacks }),
    );
    let listed = c.of_kind(&back, "turn.step").iter().any(|st| ran(st, |cmd| cmd.contains("ls") && cmd.contains(&note)));
    s.ok(
        "its earlier work is still in /data/work: after the wake it lists the file it made before the sleep, a command in its terminal (model-dependent)",
        listed,
        c.why(&back, end.is_some(), "end its turn", WORK),
    );
    let file = api.signed(&keys, "GET", &format!("/api/f/{app_name}/file?path=site/made-at.txt"), None)?;
    let at = file.text.trim().parse::<i64>().ok();
    s.ok(
        "and that file, written before the sleep, reaches its app through the CLI after the wake: the time in it is before the sleep (model-dependent)",
        file.status == 200 && at.is_some_and(|t| t >= started_s - 60 && t <= slept_s + 60),
        json!({ "status": file.status, "text": file.text.chars().take(80).collect::<String>(), "sectionBegan": started_s, "slept": slept_s }),
    );
    count(api, "wake", &back, &mut spent);

    // ---- 7. what it spent
    // every paid call the person made, before the first message included
    let used = paid(api, &owner_id);
    let totals = api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": owner_id, "op": "totals" }))).map(|r| r.body).unwrap_or(Value::Null);
    let charged = totals["charged"].as_i64().unwrap_or(0) as f64 / fragment_core::price::USD as f64;
    let by_step = spent.iter().map(|(step, n)| format!("{step} {n}")).collect::<Vec<_>>().join(", ");
    println!("      (its paid calls by step: {by_step}; {used} of the {PAID_CALLS} lent; ${charged:.4} charged to the person, its computer's awake time included)");
    s.ok(&format!("its paid calls (model calls and AI steps, from its ledger) stayed within the {PAID_CALLS} the run lent it"), used as u64 <= PAID_CALLS, json!({ "used": used, "byStep": by_step, "totals": totals }));

    // ---- 8. asleep at the end; the run's sweep deletes its fragments
    let (slept, last, answers) = sleep(api, &keys, &id);
    println!("      (its owner's sleeps answered: {})", answers.join(", "));
    s.ok("it sleeps at the end", slept, &last);
    let turns = json!({ "person": Api::email_of(&keys), "computer": id, "chat": chat_name, "app": app_name, "paidCalls": used, "charged": charged, "turns": *c.turns.borrow() });
    std::fs::write(evidence.join("turns.json"), serde_json::to_vec_pretty(&turns)?)?;
    println!("      (its turns and the screen's shots: {})", evidence.display());
    Ok(())
}

/// Step 4: the screen's page opened before the agent's browser (its
/// desktop as it starts), then the agent asked to open one; the page left
/// open follows it, one opened now draws it and takes Take over, and one
/// opened again after it closed shows it.
#[allow(clippy::too_many_arguments)]
fn screen_checks(s: &mut Suite, c: &Chat, chrome: &mut Browser, api: &Api, keys: &Keys, id: &str, browsing: &str, shots: &Path) -> Result<String> {
    let (open, connected) = open_screen(chrome, api, keys, id, &c.agent)?;
    let before = connected.then(|| within(FOLLOW, || Frame::read(chrome, &open))).flatten();
    let shows = chrome.eval(&open, "[document.getElementById('screen')?.dataset.agent, document.title]").unwrap_or_default();
    s.ok(
        "the screen's page (a ticket, in Chrome) names its chat's agent, connects to that agent's desktop and draws a frame",
        connected && before.is_some() && shows[0] == c.agent.as_str() && shows[1].as_str().is_some_and(|t| t.ends_with("'s screen")),
        json!({ "status": status(chrome, &open), "shows": shows, "frame": describe(&before) }),
    );
    let _ = chrome.screenshot(&open, &shots.join("1-desktop-before.png"));
    let browsed = c.say("smoke-4", browsing)?;
    let end = c.finish(&browsed, WORK);
    let steps = c.of_kind(&browsed, "turn.step");
    // Hermes names a browser step by what it does (`Browsing <url>`); an
    // agent may open one from its terminal too (`chromium <url>`), and its
    // computer_use steps name no URL: whichever it chose, a step names the
    // page, and the screen's checks below are what it shows
    s.ok(
        "asked to open a browser at example.com, its turn ran a step that opens it: Hermes' browser, or a command in its terminal (model-dependent)",
        end.is_some() && steps.iter().any(|st| st["args"].as_str().is_some_and(|a| a.contains("example.com"))),
        c.why(&browsed, end.is_some(), "end its turn", WORK),
    );
    // Chromium on Containers had no /dev/shm (Paul on p5, 2026-10-05): the
    // agent, given sudo, made one itself and tried again
    let said = |v: &Value, key: &str| v[key].as_str().is_some_and(|x| x.contains("/dev/shm"));
    let shm = steps.iter().any(|st| said(st, "args") || said(st, "text")) || c.replies(&browsed).iter().any(|r| r.contains("/dev/shm"));
    s.ok(
        "its browser started at once: nothing in its turn works around a missing /dev/shm (model-dependent: the agent says so when it does)",
        end.is_some() && !shm,
        c.why(&browsed, end.is_some(), "end its turn", WORK),
    );
    // the page left open, with no reload: a desktop that restarted under it is followed
    let new_frame = |f: &Frame| f.shows_a_page_over(before.as_ref());
    let followed = within(FOLLOW, || Frame::read(chrome, &open).filter(|f| new_frame(f) && chrome.eval(&open, CONNECTED).ok() == Some(Value::Bool(true))));
    let _ = chrome.screenshot(&open, &shots.join("2-left-open-after.png"));
    s.ok(
        "the screen left open follows the agent's desktop: with no reload it shows the browser (a light page over the dark desktop it showed before, pixels sampled)",
        followed.is_some(),
        json!({ "status": status(chrome, &open), "before": describe(&before), "now": describe(&Frame::read(chrome, &open)) }),
    );
    // a page opened now: connected, drawn, not blank; and Take over
    let (now, connected) = open_screen(chrome, api, keys, id, &c.agent)?;
    let drawn = connected.then(|| within(FOLLOW, || Frame::read(chrome, &now).filter(|f| !f.blank()))).flatten();
    let _ = chrome.screenshot(&now, &shots.join("3-opened-now.png"));
    s.ok(
        "opened now, the screen's page connects (its websockify socket) and draws a frame that is not blank (pixels sampled)",
        drawn.is_some(),
        json!({ "status": status(chrome, &now), "frame": describe(&Frame::read(chrome, &now)) }),
    );
    let taken = connected && chrome.eval(&now, "(document.getElementById('control').click(), true)").is_ok() && chrome.until(&now, "document.body.dataset.state === 'mine'", Duration::from_secs(20));
    s.ok("Take over gives the person the screen (its control socket grants control: the agent's lease)", taken, status(chrome, &now));
    // every page closed, then the screen opened again
    chrome.close(open)?;
    chrome.close(now)?;
    let (again, connected) = open_screen(chrome, api, keys, id, &c.agent)?;
    let same = |f: &Frame| new_frame(f) && drawn.as_ref().is_none_or(|d| f.unlike(d) <= SAME);
    let shown = connected.then(|| within(FOLLOW, || Frame::read(chrome, &again).filter(same))).flatten();
    let _ = chrome.screenshot(&again, &shots.join("4-opened-again.png"));
    s.ok(
        "closed and opened again, the screen shows the browser: connected, a light page over the desktop, the frame it showed before it closed",
        shown.is_some(),
        json!({ "status": status(chrome, &again), "before": describe(&drawn), "now": describe(&Frame::read(chrome, &again)), "desktop": describe(&before) }),
    );
    chrome.close(again)?;
    Ok(browsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(px: Vec<[u8; 3]>) -> Frame {
        Frame { width: 48, height: 30, px }
    }

    /// A black screen, or one colour with a little noise, is blank; a
    /// browser's page over a desktop is not, and is unlike the desktop.
    #[test]
    fn a_frame_is_blank_or_shows_something() {
        let black = frame(vec![[0, 0, 0]; 1440]);
        assert!(black.blank());
        let mut noisy = vec![[10, 12, 9]; 1440];
        noisy[3] = [200, 200, 200];
        assert!(frame(noisy).blank(), "one sample is noise");
        let mut page = vec![[240, 240, 242]; 1440];
        for p in page.iter_mut().take(300) {
            *p = [222, 225, 230];
        }
        for p in page.iter_mut().skip(700).take(40) {
            *p = [0, 0, 0];
        }
        let page = frame(page);
        assert!(!page.blank());
        assert!(page.unlike(&black) >= CHANGED);
        // a light page over a dark desktop is the browser; the desktop alone is not
        let mut desktop = vec![[13, 16, 21]; 1440];
        for (i, p) in desktop.iter_mut().enumerate() {
            *p = [13, 16, (21 + i % 90) as u8];
        }
        let desktop = frame(desktop);
        assert!(!desktop.blank(), "a gradient is drawn");
        assert!(!desktop.shows_a_page_over(Some(&desktop)), "the desktop alone shows no page");
        assert!(page.shows_a_page_over(Some(&desktop)) && page.shows_a_page_over(None));
        assert!(!black.shows_a_page_over(None), "black is no page");
        // lossy encoding moves a colour a little: still the same frame
        let jittered = frame(page.px.iter().map(|p| [p[0].saturating_sub(8), p[1], p[2].saturating_add(8)]).collect());
        assert!(jittered.unlike(&page) <= SAME);
        assert!(frame(vec![[0, 0, 0]; 10]).unlike(&page) == 1.0, "another size is wholly unlike");
    }

    /// Goal: a check that the agent ran a command takes it from its
    /// terminal, or from a script that calls its terminal with it; nothing
    /// else is a command run. Method: canary round 5's steps, as the bridge
    /// said them (Hermes' previews: lines joined, cut at 140 with `...`),
    /// and the scripts a check must not read as one.
    #[test]
    fn a_command_runs_in_its_terminal_or_a_script_calling_it() {
        let step = |tool: &str, args: &str| json!({ "kind": "turn.step", "tool": tool, "args": args, "ok": true });
        let fragment = |cmd: &str| cmd.contains("fragment");
        assert!(ran(&step("terminal", "fragment list"), fragment));
        assert!(ran(&step("execute_code", r#"from hermes_tools import terminal print(terminal("fragment list")["output"])"#), fragment));
        let listed = |cmd: &str| cmd.contains("ls") && cmd.contains("e2e-d2715e-todo.txt");
        assert!(ran(&step("execute_code", r#"from hermes_tools import terminal r1 = terminal("ls -l e2e-d2715e-todo.txt", workdir="/data/work/e2e-d2715e-agent--aymc") print("R1:", r1..."#), listed));
        let deleted = |cmd: &str| cmd.contains("rm -rf") && cmd.contains("e2e-d2715e-scratch");
        assert!(ran(&step("terminal", "rm -rf ~/e2e-d2715e-scratch-backups"), deleted));
        assert!(ran(&step("execute_code", r#"from hermes_tools import terminal r = terminal("rm -rf ~/e2e-d2715e-scratch-backups && ls -la ~/e2e-d2715e-scr..."#), deleted), "a call cut short, read to the cut");
        // round 5's look before the delete it was asked for: no rm ran
        assert!(!ran(&step("execute_code", r#"from hermes_tools import terminal r = terminal("ls -la ~/e2e-d2715e-scratch-backups 2>&1; du -sh ~/e2e-d2715e-scratch-backups 2>/dev/null..."#), deleted));
        // only a terminal call's arguments are its command
        assert!(!ran(&step("execute_code", r#"from hermes_tools import terminal print("fragment") terminal("date")"#), fragment));
        assert!(!ran(&step("execute_code", r#"import fragment; my_terminal("fragment list")"#), fragment));
        assert!(!ran(&step("skill_view", "fragment"), fragment), "a tool that runs no command");
        assert!(!ran(&step("write_file", r#"terminal("fragment list")"#), fragment));
        // a string is read as one: a `)` in a command ends no call
        assert!(ran(&step("execute_code", r#"terminal("echo ')' && fragment list")"#), fragment));
        assert_eq!(terminal_calls(r#"a = terminal("ls (x)"); b = terminal(command='rm -rf "y\'z"', timeout=5) c"#), [r#""ls (x)""#, r#"command='rm -rf "y\'z"', timeout=5"#]);
        assert_eq!(terminal_calls("no call here"), Vec::<&str>::new());
    }

    /// The page's sample parses; nothing drawn yet is no frame.
    #[test]
    fn a_frame_parses_from_the_page() {
        let f = Frame::parse(&json!({ "width": 1280, "height": 800, "px": [[1, 2, 3], [4, 5, 6]] })).expect("a frame");
        assert_eq!((f.width, f.px.len(), f.px[1]), (1280, 2, [4, 5, 6]));
        assert_eq!(Frame::parse(&Value::Null), None);
        assert_eq!(Frame::parse(&json!({ "width": 0, "height": 0, "px": [] })), None);
        assert!(f.describe().starts_with("1280x800"));
    }
}
