//! The bridge's core, as a pure state machine: records and runtime events
//! in, posts and runtime commands out. Nothing here does I/O or reads the
//! clock; the driver (driver.rs) feeds it, persists its state whenever a
//! step says so, and only then carries out the step's effects ("validate,
//! persist, then interpret", docs/engineering-style.md).
//!
//! What it keeps (lesson 1, authority): a cursor per followed channel and
//! the turns it admitted that have not ended. The chat channel owns what was
//! said; the runtime owns the in-flight turn; the bridge only translates.
//!
//! Its rules, in one place:
//! - A record is admitted once: the cursor of its `(agent, fragment,
//!   channel)` passes it before anything it causes is done, so a catch-up
//!   after a restart, a wake, or a reconnect starts nothing twice (lesson 2).
//! - One turn of an agent runs in a chat at a time; the rest wait in order.
//! - In a chat with several agents, a message is for the agents it names
//!   (`to`, else `@mentions` of this computer's agents), else for the lead,
//!   the first agent added (decision 8). An agent's reply is for another
//!   agent only when it names it, at most `HOPS_MAX` hand-offs deep.
//! - Only a turn's asker stops it; only an agent's owner answers its
//!   prompts, the first answer wins, and an unanswered prompt expires
//!   (decision 42).
//! - The computer is kept awake while a turn waits to run or runs, and not
//!   while every open turn waits on a person.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::limits;
use crate::records::{self, AttachmentRef, Cause, Closed, Message, Outcome, PromptOption, Record, Said, Task};
use crate::runtime::{Agent, Command, Event, LocalFile, TurnStart};

/// The state file's format.
pub const STATE_VERSION: u32 = 1;

/// What the bridge keeps across restarts (`/data/bridge/state.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// How many times this state was loaded: a turn from an earlier boot was
    /// in the runtime that died with it.
    pub boot: u64,
    /// The last seq handled, per `agent|fragment|channel`.
    pub cursors: BTreeMap<String, u64>,
    /// The turns admitted and not ended, by id.
    pub turns: BTreeMap<String, Turn>,
    /// Admissions so far: a turn's place in its chat's queue.
    pub admitted: u64,
    /// Messages the runtime said on its own so far (their turns' ids).
    pub said: u64,
}

impl Default for State {
    fn default() -> State {
        State { version: STATE_VERSION, boot: 0, cursors: BTreeMap::new(), turns: BTreeMap::new(), admitted: 0, said: 0 }
    }
}

/// Where a turn is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Admitted; another turn of its agent runs in its chat.
    Queued,
    /// Given to the runtime, not yet taken (a restart gives it again).
    Handed,
    /// The runtime's (a restart ends it).
    Running,
    /// Waiting on a prompt's answer: the computer may sleep.
    Waiting,
}

/// A prompt a turn asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub options: Vec<String>,
    pub expires_at: u64,
    pub closed: bool,
}

/// The reply a turn is writing: shown as the chat's draft, posted once a
/// later part, a step, a prompt, or the end comes. Not kept across a
/// restart, which ends the turn.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OpenReply {
    pub part: u32,
    pub text: String,
    pub files: Vec<LocalFile>,
}

/// A turn the bridge admitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub id: String,
    /// The agent fragment whose turn it is.
    pub agent: String,
    /// The chat it answers in.
    pub fragment: String,
    /// The record that started it.
    pub cause: Cause,
    pub asker: String,
    pub asker_name: String,
    pub text: String,
    pub attachments: Vec<AttachmentRef>,
    pub routine: bool,
    /// Agent hand-offs that led to it.
    pub hop: u32,
    /// Its admission's number: the order turns of a chat run in.
    pub order: u64,
    pub phase: Phase,
    /// The boot that admitted or last handed it.
    pub boot: u64,
    /// When the runtime was last heard about it (or it was handed).
    pub last_ms: u64,
    pub steps: u32,
    /// Replies posted so far, and the highest part among them.
    pub replies: u32,
    pub last_part: u32,
    pub prompts: Vec<Prompt>,
    pub stop_requested: bool,
    #[serde(skip)]
    pub open: Option<OpenReply>,
}

impl Turn {
    fn active(&self) -> bool {
        matches!(self.phase, Phase::Handed | Phase::Running | Phase::Waiting)
    }
}

/// Who is in a chat, as the driver read it (members and `__people`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatView {
    /// The agents among its members, in the order they were added: the
    /// first is the lead.
    pub agents: Vec<String>,
    /// What to call its writers, by identity.
    pub names: BTreeMap<String, String>,
}

impl ChatView {
    pub fn lead(&self) -> Option<&str> {
        self.agents.first().map(String::as_str)
    }
}

/// What the engine is told.
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    /// The computer's agents, as `GET /api/computer` lists them now.
    Agents(Vec<Agent>),
    /// A record of a followed channel, read as `agent`, with the chat's view
    /// (a `chat` record's admission needs it; a `tasks` record's does not).
    /// A record from before `since` (ms; when the agent joined the chat) is
    /// history: its cursor passes it, and it starts nothing.
    Record { agent: String, fragment: String, record: Record, view: Option<ChatView>, since: i64 },
    Runtime(Event),
    /// Time passed: expiries and quiet turns.
    Tick,
    /// The agent is no longer in the fragment (403 or 404 there).
    Gone { agent: String, fragment: String },
}

/// What the engine asks the driver to do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// A record, as `agent`; `files` are uploaded as the fragment's blobs and
    /// listed in the body's `attachments` first.
    Post { agent: String, fragment: String, channel: &'static str, id: String, body: Value, files: Vec<LocalFile> },
    /// The chat's draft for a turn (`None` stops it).
    Draft { agent: String, fragment: String, turn: String, text: Option<String> },
    Runtime(Command),
    /// Hold the keepalive socket (true) or drop it.
    Keepalive(bool),
    /// List the agent's fragments again (it joined one: `joined`, whose
    /// members changed, so every view of it read before is stale).
    Discover { agent: String, joined: Option<String> },
}

/// One step's result.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub effects: Vec<Effect>,
    /// The state changed: persist it before carrying out the effects.
    pub dirty: bool,
}

/// A state file that contradicts itself: refused, never repaired in place.
#[derive(Debug, Clone, PartialEq)]
pub struct Corrupt(pub String);

/// The engine's settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Settings {
    pub prompt_ttl_ms: u64,
    pub turn_idle_ms: u64,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { prompt_ttl_ms: limits::PROMPT_TTL_MS_DEFAULT, turn_idle_ms: limits::TURN_IDLE_MS_MAX }
    }
}

pub struct Engine {
    state: State,
    settings: Settings,
    agents: Vec<Agent>,
    views: HashMap<String, ChatView>,
    keepalive: bool,
    // A step's scratch, cleared at each step's start.
    out: Vec<Effect>,
    dirty: bool,
    now: u64,
}

pub fn cursor_key(agent: &str, fragment: &str, channel: &str) -> String {
    format!("{agent}|{fragment}|{channel}")
}

fn label(fragment: &str) -> &str {
    fragment.split('.').next().unwrap_or(fragment)
}

impl Engine {
    /// An engine over a loaded state. It checks the state agrees with
    /// itself; then `recover` must run before any other step.
    pub fn new(state: State, settings: Settings) -> Result<Engine, Corrupt> {
        check(&state)?;
        Ok(Engine { state, settings, agents: Vec::new(), views: HashMap::new(), keepalive: false, out: Vec::new(), dirty: false, now: 0 })
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    pub fn cursor(&self, agent: &str, fragment: &str, channel: &str) -> u64 {
        self.state.cursors.get(&cursor_key(agent, fragment, channel)).copied().unwrap_or(0)
    }

    pub fn keepalive(&self) -> bool {
        self.keepalive
    }

    /// The first step of a boot: a new boot number; turns the runtime held
    /// when the last boot ended are ended (it died with them), turns handed
    /// but never taken are handed again, and every chat's queue moves.
    pub fn recover(&mut self, now: u64) -> Step {
        self.begin(now);
        self.state.boot += 1;
        self.dirty = true;
        let boot = self.state.boot;
        let ids: Vec<String> = self.state.turns.keys().cloned().collect();
        for id in &ids {
            let phase = self.state.turns[id].phase;
            match phase {
                Phase::Queued => {}
                Phase::Handed => {
                    let t = self.state.turns.get_mut(id).expect("listed");
                    t.phase = Phase::Queued;
                }
                Phase::Running | Phase::Waiting => self.end(id, Outcome::Error("lost when the computer restarted".into()), Closed::Expired),
            }
        }
        for t in self.state.turns.values_mut() {
            t.boot = boot;
        }
        let chats: Vec<(String, String)> = self.state.turns.values().map(|t| (t.agent.clone(), t.fragment.clone())).collect();
        for (agent, fragment) in chats {
            self.pump(&agent, &fragment);
        }
        self.finish()
    }

    /// One input, its effects, and whether to persist first.
    pub fn step(&mut self, input: Input, now: u64) -> Step {
        self.begin(now);
        match input {
            Input::Agents(agents) => self.set_agents(agents),
            Input::Record { agent, fragment, record, view, since } => self.record(&agent, &fragment, record, view, since),
            Input::Runtime(event) => self.runtime(event),
            Input::Tick => self.tick(),
            Input::Gone { agent, fragment } => self.gone(&agent, &fragment),
        }
        self.finish()
    }

    fn begin(&mut self, now: u64) {
        assert!(self.out.is_empty(), "a step's effects were taken");
        self.dirty = false;
        // The clock may step back (the guest's); the engine never does.
        self.now = self.now.max(now);
    }

    fn finish(&mut self) -> Step {
        let busy = self.state.turns.values().any(|t| matches!(t.phase, Phase::Queued | Phase::Handed | Phase::Running));
        if busy != self.keepalive {
            self.keepalive = busy;
            self.out.push(Effect::Keepalive(busy));
        }
        assert!(self.state.turns.len() <= limits::TURNS_OPEN_MAX, "admission bounds the open turns");
        debug_assert!(check(&self.state).is_ok(), "a step keeps the state whole");
        Step { effects: std::mem::take(&mut self.out), dirty: self.dirty }
    }

    fn agent(&self, fragment: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.fragment == fragment)
    }

    fn post(&mut self, agent: &str, fragment: &str, channel: &'static str, id: String, body: Value, files: Vec<LocalFile>) {
        self.out.push(Effect::Post { agent: agent.to_string(), fragment: fragment.to_string(), channel, id, body, files });
    }

    fn draft(&mut self, agent: &str, fragment: &str, turn: &str, text: Option<String>) {
        self.out.push(Effect::Draft { agent: agent.to_string(), fragment: fragment.to_string(), turn: turn.to_string(), text });
    }

    // ---- agents ----

    fn set_agents(&mut self, agents: Vec<Agent>) {
        assert!(agents.len() <= limits::AGENTS_MAX, "the driver bounds the agents it reads");
        self.agents = agents;
        let gone: Vec<String> = self.state.turns.values().filter(|t| self.agent(&t.agent).is_none()).map(|t| t.id.clone()).collect();
        for id in gone {
            let t = self.state.turns.remove(&id).expect("listed");
            self.dirty = true;
            crate::ev!("turn.dropped", { "turn": id, "agent": t.agent, "why": "the agent left this computer" });
            if t.active() {
                self.out.push(Effect::Runtime(Command::Forget { turn: id }));
            }
        }
    }

    // ---- records ----

    fn record(&mut self, agent_fragment: &str, fragment: &str, record: Record, view: Option<ChatView>, since: i64) {
        let Some(agent) = self.agent(agent_fragment).cloned() else {
            crate::ev!("record.skipped", { "agent": agent_fragment, "fragment": fragment, "seq": record.seq, "why": "not an agent of this computer" });
            return;
        };
        let key = cursor_key(agent_fragment, fragment, &record.channel);
        let cursor = self.state.cursors.get(&key).copied().unwrap_or(0);
        if record.seq <= cursor {
            // A catch-up, a reconnect, or a socket's page read again: done.
            return;
        }
        // The cursor passes the record before anything it causes is done.
        self.state.cursors.insert(key, record.seq);
        self.dirty = true;
        if let Some(v) = view {
            self.views.insert(fragment.to_string(), v);
        }
        if record.at < since {
            // Said before the agent joined: not for it.
            return;
        }
        if record.principal == agent.identity || record.principal.starts_with("anon:") {
            return;
        }
        match record.channel.as_str() {
            records::CHAT => self.said(&agent, fragment, &record),
            records::TASKS if fragment == agent.fragment => self.task(&agent, &record),
            _ => {}
        }
    }

    fn said(&mut self, agent: &Agent, fragment: &str, record: &Record) {
        match records::said(&record.body) {
            Said::Message(m) => {
                let view = self.views.get(fragment).cloned().unwrap_or_default();
                if let Some(hop) = self.addressed(agent, &view, &record.principal, &m) {
                    let asker_name = view.names.get(&record.principal).cloned().unwrap_or_else(|| "someone".into());
                    let cause = Cause { fragment: fragment.to_string(), channel: record.channel.clone(), seq: record.seq };
                    self.admit(agent, fragment, cause, &record.principal, asker_name, &m, hop, false);
                }
            }
            Said::Stop { turn } => self.stop(agent, fragment, &record.principal, turn.as_deref()),
            Said::PromptResponse { prompt, option } => self.answer(agent, fragment, &record.principal, &prompt, &option, record.seq),
            Said::Other => {}
        }
    }

    fn task(&mut self, agent: &Agent, record: &Record) {
        match records::task(&record.body) {
            Task::Routine { text, chat } => {
                let cause = Cause { fragment: agent.fragment.clone(), channel: record.channel.clone(), seq: record.seq };
                let m = Message { text, ..Message::default() };
                let owner = agent.owner.clone();
                self.admit(agent, &chat, cause, &owner, "your routine".into(), &m, 0, true);
            }
            Task::Joined { fragment } => self.out.push(Effect::Discover { agent: agent.fragment.clone(), joined: fragment }),
            Task::Other => {}
        }
    }

    /// Whether a message is for `agent`, and the hop its turn is at.
    fn addressed(&self, agent: &Agent, view: &ChatView, principal: &str, m: &Message) -> Option<u32> {
        let from_agent = view.agents.iter().any(|a| a == principal) || self.agents.iter().any(|a| a.identity == principal);
        if from_agent {
            let named = m.to.iter().any(|t| t == &agent.identity);
            return (named && m.hop <= limits::HOPS_MAX).then_some(m.hop);
        }
        if !m.to.is_empty() {
            return m.to.iter().any(|t| t == &agent.identity).then_some(0);
        }
        let words = records::mentions(&m.text);
        let named: Vec<&Agent> = self.agents.iter().filter(|a| view.agents.contains(&a.identity) && words.iter().any(|w| answers_to(a, w))).collect();
        if !named.is_empty() {
            return named.iter().any(|a| a.identity == agent.identity).then_some(0);
        }
        (view.lead() == Some(agent.identity.as_str())).then_some(0)
    }

    #[allow(clippy::too_many_arguments)]
    fn admit(&mut self, agent: &Agent, fragment: &str, cause: Cause, asker: &str, asker_name: String, m: &Message, hop: u32, routine: bool) {
        let id = records::turn_id(&agent.fragment, &cause.fragment, &cause.channel, cause.seq);
        if self.state.turns.contains_key(&id) {
            // The cursor makes this impossible; a state that says otherwise
            // is not trusted to go on.
            panic!("turn {id} admitted twice: the cursor of {} is behind its turns", cursor_key(&agent.fragment, &cause.fragment, &cause.channel));
        }
        let waiting = self.state.turns.values().filter(|t| t.agent == agent.fragment && t.fragment == fragment && t.phase == Phase::Queued).count();
        let refusal = if waiting >= limits::QUEUED_PER_CHAT_MAX {
            Some("too many messages are waiting for this agent; send it again once it answers")
        } else if self.state.turns.len() >= limits::TURNS_OPEN_MAX {
            Some("this computer is too busy right now")
        } else {
            None
        };
        if let Some(why) = refusal {
            crate::ev!("turn.refused", { "turn": id, "agent": agent.fragment, "fragment": fragment, "why": why });
            self.post(&agent.fragment, fragment, records::WORK, records::work_id(&id, "end"), records::turn_end(&id, &Outcome::Error(why.into())), Vec::new());
            return;
        }
        self.state.admitted += 1;
        let turn = Turn {
            id: id.clone(),
            agent: agent.fragment.clone(),
            fragment: fragment.to_string(),
            cause,
            asker: asker.to_string(),
            asker_name,
            text: records::cut_bytes(&m.text, limits::MESSAGE_TEXT_MAX_BYTES),
            attachments: m.attachments.clone(),
            routine,
            hop,
            order: self.state.admitted,
            phase: Phase::Queued,
            boot: self.state.boot,
            last_ms: self.now,
            steps: 0,
            replies: 0,
            last_part: 0,
            prompts: Vec::new(),
            stop_requested: false,
            open: None,
        };
        crate::ev!("turn.admitted", { "turn": id, "agent": agent.fragment, "fragment": fragment, "seq": turn.cause.seq, "hop": hop });
        self.state.turns.insert(id, turn);
        self.dirty = true;
        self.pump(&agent.fragment, fragment);
    }

    /// Hands the chat's next turn to the runtime when none of its agent's
    /// runs there.
    fn pump(&mut self, agent: &str, fragment: &str) {
        let busy = self.state.turns.values().any(|t| t.agent == agent && t.fragment == fragment && t.active());
        if busy {
            return;
        }
        let next = self.state.turns.values().filter(|t| t.agent == agent && t.fragment == fragment && t.phase == Phase::Queued).min_by_key(|t| t.order).map(|t| t.id.clone());
        let Some(id) = next else { return };
        let Some(a) = self.agent(agent).cloned() else { return };
        let now = self.now;
        let t = self.state.turns.get_mut(&id).expect("found");
        t.phase = Phase::Handed;
        t.last_ms = now;
        let start = TurnStart {
            turn: id.clone(),
            agent: a.clone(),
            fragment: t.fragment.clone(),
            chat_name: label(&t.fragment).to_string(),
            seq: t.cause.seq,
            asker: t.asker.clone(),
            asker_name: t.asker_name.clone(),
            text: t.text.clone(),
            attachments: t.attachments.clone(),
            files: Vec::new(),
            routine: t.routine,
        };
        let body = records::turn_start(&id, &t.asker, &a.identity, &t.cause);
        let fragment = t.fragment.clone();
        self.dirty = true;
        crate::ev!("turn.handed", { "turn": id, "agent": agent, "fragment": fragment });
        self.post(agent, &fragment, records::WORK, records::work_id(&id, "start"), body, Vec::new());
        self.out.push(Effect::Runtime(Command::Start(Box::new(start))));
    }

    // ---- Stop and answers ----

    fn stop(&mut self, agent: &Agent, fragment: &str, principal: &str, named: Option<&str>) {
        let target = self
            .state
            .turns
            .values()
            .filter(|t| t.agent == agent.fragment && t.fragment == fragment)
            .find(|t| match named {
                Some(id) => t.id == id,
                None => t.active(),
            })
            .map(|t| (t.id.clone(), t.asker.clone(), t.phase));
        let Some((id, asker, phase)) = target else {
            crate::ev!("stop.ignored", { "agent": agent.fragment, "fragment": fragment, "why": "no such turn" });
            return;
        };
        if asker != principal {
            crate::ev!("stop.ignored", { "turn": id, "why": "only the turn's asker stops it" });
            return;
        }
        match phase {
            Phase::Queued => {
                self.state.turns.remove(&id);
                self.dirty = true;
                crate::ev!("turn.stopped", { "turn": id, "phase": "queued" });
                self.post(&agent.fragment, fragment, records::WORK, records::work_id(&id, "end"), records::turn_end(&id, &Outcome::Stopped), Vec::new());
            }
            Phase::Handed | Phase::Running | Phase::Waiting => {
                let t = self.state.turns.get_mut(&id).expect("found");
                if t.stop_requested {
                    return;
                }
                t.stop_requested = true;
                let open: Vec<String> = t.prompts.iter().filter(|p| !p.closed).map(|p| p.id.clone()).collect();
                for p in t.prompts.iter_mut() {
                    p.closed = true;
                }
                if t.phase == Phase::Waiting {
                    t.phase = Phase::Running;
                }
                self.dirty = true;
                crate::ev!("turn.stopping", { "turn": id });
                for p in open {
                    self.post(&agent.fragment, fragment, records::WORK, records::work_id(&id, &format!("pc:{p}")), records::turn_prompt_closed(&id, &p, Closed::Stopped, None), Vec::new());
                }
                self.out.push(Effect::Runtime(Command::Stop { turn: id }));
            }
        }
    }

    fn answer(&mut self, agent: &Agent, fragment: &str, principal: &str, prompt: &str, option: &str, seq: u64) {
        let found = self.state.turns.values().find(|t| t.agent == agent.fragment && t.fragment == fragment && t.prompts.iter().any(|p| p.id == prompt)).map(|t| t.id.clone());
        let Some(id) = found else {
            crate::ev!("prompt.ignored", { "agent": agent.fragment, "prompt": prompt, "why": "no open turn asked it" });
            return;
        };
        if principal != agent.owner {
            crate::ev!("prompt.ignored", { "turn": id, "prompt": prompt, "why": "only the agent's owner answers" });
            return;
        }
        let now = self.now;
        let t = self.state.turns.get_mut(&id).expect("found");
        let p = t.prompts.iter_mut().find(|p| p.id == prompt).expect("found");
        if p.closed {
            crate::ev!("prompt.ignored", { "turn": id, "prompt": prompt, "why": "already closed: the first answer won" });
            return;
        }
        if !p.options.iter().any(|o| o == option) {
            crate::ev!("prompt.ignored", { "turn": id, "prompt": prompt, "why": "not one of its options" });
            return;
        }
        p.closed = true;
        if t.prompts.iter().all(|p| p.closed) && t.phase == Phase::Waiting {
            t.phase = Phase::Running;
        }
        t.last_ms = now;
        self.dirty = true;
        crate::ev!("prompt.answered", { "turn": id, "prompt": prompt, "option": option });
        self.post(&agent.fragment, fragment, records::WORK, records::work_id(&id, &format!("pc:{prompt}")), records::turn_prompt_closed(&id, prompt, Closed::Answered, Some((option, principal))), Vec::new());
        self.out.push(Effect::Runtime(Command::Answer { turn: id, prompt: prompt.to_string(), option: Some(option.to_string()), seq, by: principal.to_string() }));
    }

    // ---- the runtime ----

    fn runtime(&mut self, event: Event) {
        let turn_of = |e: &Event| -> Option<String> {
            match e {
                Event::Accepted { turn } | Event::Draft { turn, .. } | Event::Reply { turn, .. } | Event::Attachment { turn, .. } | Event::Retract { turn, .. } => Some(turn.clone()),
                Event::Step { turn, .. } | Event::Prompt { turn, .. } | Event::End { turn, .. } => Some(turn.clone()),
                Event::Say { .. } => None,
            }
        };
        if let Some(id) = turn_of(&event) {
            match self.state.turns.get_mut(&id) {
                Some(t) if t.active() => t.last_ms = self.now,
                _ => {
                    crate::ev!("runtime.stale", { "turn": id, "why": "the bridge holds no running turn by that id" });
                    return;
                }
            }
        }
        match event {
            Event::Accepted { turn } => {
                let t = self.state.turns.get_mut(&turn).expect("checked");
                if t.phase == Phase::Handed {
                    t.phase = Phase::Running;
                    self.dirty = true;
                }
            }
            Event::Draft { turn, text } => {
                let t = &self.state.turns[&turn];
                let (agent, fragment) = (t.agent.clone(), t.fragment.clone());
                self.draft(&agent, &fragment, &turn, Some(records::cut_bytes(&text, limits::DRAFT_TEXT_MAX_BYTES)));
            }
            Event::Reply { turn, part, text } => self.reply(&turn, part, Some(text), None),
            Event::Attachment { turn, part, file } => self.reply(&turn, part, None, Some(file)),
            Event::Retract { turn, part } => {
                let t = self.state.turns.get_mut(&turn).expect("checked");
                if t.open.as_ref().is_some_and(|o| o.part == part) {
                    t.open = None;
                    let (agent, fragment) = (t.agent.clone(), t.fragment.clone());
                    self.draft(&agent, &fragment, &turn, None);
                }
            }
            Event::Step { turn, step } => {
                self.seal(&turn);
                let t = self.state.turns.get_mut(&turn).expect("checked");
                t.steps = t.steps.saturating_add(1);
                self.dirty = true;
                if t.steps <= limits::STEPS_PER_TURN_MAX {
                    let (agent, fragment, n) = (t.agent.clone(), t.fragment.clone(), t.steps);
                    self.post(&agent, &fragment, records::WORK, records::work_id(&turn, &n.to_string()), records::turn_step(&turn, n, &step), Vec::new());
                }
            }
            Event::Prompt { turn, prompt, text, options, ttl_ms } => self.prompt(&turn, prompt, text, options, ttl_ms),
            Event::End { turn, outcome } => self.end(&turn, outcome, Closed::Expired),
            Event::Say { agent, fragment, text } => {
                if self.agent(&agent).is_none() || text.trim().is_empty() {
                    return;
                }
                self.state.said += 1;
                self.dirty = true;
                let turn = records::said_turn_id(&agent, self.state.said);
                crate::ev!("said", { "agent": agent, "fragment": fragment, "turn": turn });
                self.post(&agent, &fragment, records::CHAT, records::reply_id(&turn, 1), records::reply(&text, &turn, &[], 0), Vec::new());
            }
        }
    }

    /// Reply `part` of a turn changed: its text, or a file it carries.
    fn reply(&mut self, id: &str, part: u32, text: Option<String>, file: Option<LocalFile>) {
        let t = &self.state.turns[id];
        if part == 0 || part <= t.last_part {
            crate::ev!("reply.late", { "turn": id, "part": part });
            return;
        }
        if t.open.as_ref().is_some_and(|o| o.part > part) {
            crate::ev!("reply.late", { "turn": id, "part": part });
            return;
        }
        if t.open.as_ref().is_some_and(|o| o.part != part) {
            self.seal(id);
        }
        let t = self.state.turns.get_mut(id).expect("checked");
        let open = t.open.get_or_insert_with(|| OpenReply { part, ..OpenReply::default() });
        if let Some(text) = text {
            open.text = text;
        }
        if let Some(file) = file {
            if open.files.len() < limits::ATTACHMENTS_MAX {
                open.files.push(file);
            } else {
                crate::ev!("attachment.dropped", { "turn": id, "why": "a reply carries at most so many" });
            }
        }
        let shown = records::cut_bytes(&open.text, limits::DRAFT_TEXT_MAX_BYTES);
        let (agent, fragment) = (t.agent.clone(), t.fragment.clone());
        if !shown.trim().is_empty() {
            self.draft(&agent, &fragment, id, Some(shown));
        }
    }

    /// Posts the turn's open reply, if it says anything.
    fn seal(&mut self, id: &str) {
        let view = self.views.get(&self.state.turns[id].fragment).cloned().unwrap_or_default();
        let t = self.state.turns.get_mut(id).expect("sealing a held turn");
        let Some(open) = t.open.take() else { return };
        t.last_part = t.last_part.max(open.part);
        let empty = open.text.trim().is_empty() && open.files.is_empty();
        if empty || t.replies >= limits::REPLIES_PER_TURN_MAX {
            return;
        }
        t.replies += 1;
        self.dirty = true;
        let (agent, fragment, n, hop) = (t.agent.clone(), t.fragment.clone(), t.replies, t.hop);
        // A reply that names another agent of the chat hands off to it.
        let words = records::mentions(&open.text);
        let to: Vec<String> = self.agents.iter().filter(|a| a.fragment != agent && view.agents.contains(&a.identity) && words.iter().any(|w| answers_to(a, w))).map(|a| a.identity.clone()).collect();
        let body = records::reply(&open.text, id, &to, hop + 1);
        crate::ev!("reply.posted", { "turn": id, "n": n, "files": open.files.len(), "to": to.len() });
        self.post(&agent, &fragment, records::CHAT, records::reply_id(id, n), body, open.files);
        self.draft(&agent, &fragment, id, None);
    }

    fn prompt(&mut self, id: &str, prompt: String, text: String, options: Vec<PromptOption>, ttl_ms: Option<u64>) {
        let t = &self.state.turns[id];
        let valid = records::valid_token(&prompt, 64)
            && !options.is_empty()
            && options.len() <= limits::PROMPT_OPTIONS_MAX
            && options.iter().all(|o| records::valid_token(&o.id, 32) && !o.label.trim().is_empty())
            && t.prompts.len() < limits::PROMPTS_PER_TURN_MAX;
        let repeat = t.prompts.iter().any(|p| p.id == prompt);
        if repeat {
            return;
        }
        if !valid {
            // A prompt no card can show is expired at once, so the runtime
            // never waits on it.
            crate::ev!("prompt.refused", { "turn": id, "why": "not a prompt a card can show" });
            self.out.push(Effect::Runtime(Command::Answer { turn: id.to_string(), prompt, option: None, seq: 0, by: String::new() }));
            return;
        }
        self.seal(id);
        // A runtime's own lifetime is held within bounds; the operator's
        // setting is theirs (tests run it short).
        let ttl = match ttl_ms {
            Some(t) => t.clamp(limits::PROMPT_TTL_MS_MIN, limits::PROMPT_TTL_MS_MAX),
            None => self.settings.prompt_ttl_ms,
        };
        let expires_at = self.now + ttl;
        let t = self.state.turns.get_mut(id).expect("checked");
        let Some(owner) = self.agents.iter().find(|a| a.fragment == t.agent).map(|a| a.owner.clone()) else { return };
        t.prompts.push(Prompt { id: prompt.clone(), options: options.iter().map(|o| o.id.clone()).collect(), expires_at, closed: false });
        t.phase = Phase::Waiting;
        self.dirty = true;
        let (agent, fragment) = (t.agent.clone(), t.fragment.clone());
        crate::ev!("prompt.asked", { "turn": id, "prompt": prompt, "expiresAt": expires_at });
        self.post(&agent, &fragment, records::WORK, records::work_id(id, &format!("p:{prompt}")), records::turn_prompt(id, &prompt, &text, &options, &owner, expires_at), Vec::new());
    }

    /// The turn is over: its last reply, its open prompts closed as
    /// `unanswered`, its end, and its chat's next turn.
    fn end(&mut self, id: &str, outcome: Outcome, unanswered: Closed) {
        self.seal(id);
        let t = self.state.turns.remove(id).expect("ending a held turn");
        self.dirty = true;
        crate::ev!("turn.end", { "turn": id, "agent": t.agent, "fragment": t.fragment, "outcome": match &outcome { Outcome::Idle => "idle", Outcome::Stopped => "stopped", Outcome::Error(_) => "error" } });
        for p in t.prompts.iter().filter(|p| !p.closed) {
            self.post(&t.agent, &t.fragment, records::WORK, records::work_id(id, &format!("pc:{}", p.id)), records::turn_prompt_closed(id, &p.id, unanswered, None), Vec::new());
        }
        self.post(&t.agent, &t.fragment, records::WORK, records::work_id(id, "end"), records::turn_end(id, &outcome), Vec::new());
        self.draft(&t.agent, &t.fragment, id, None);
        self.pump(&t.agent, &t.fragment);
    }

    // ---- time ----

    fn tick(&mut self) {
        let now = self.now;
        let mut expired: Vec<(String, String)> = Vec::new();
        let mut quiet: Vec<String> = Vec::new();
        for t in self.state.turns.values() {
            for p in t.prompts.iter().filter(|p| !p.closed && p.expires_at <= now) {
                expired.push((t.id.clone(), p.id.clone()));
            }
            let idle = matches!(t.phase, Phase::Handed | Phase::Running) && now.saturating_sub(t.last_ms) > self.settings.turn_idle_ms;
            if idle {
                quiet.push(t.id.clone());
            }
        }
        for (id, prompt) in expired {
            let t = self.state.turns.get_mut(&id).expect("listed");
            t.prompts.iter_mut().find(|p| p.id == prompt).expect("listed").closed = true;
            if t.prompts.iter().all(|p| p.closed) && t.phase == Phase::Waiting {
                t.phase = Phase::Running;
            }
            t.last_ms = now;
            self.dirty = true;
            let (agent, fragment) = (t.agent.clone(), t.fragment.clone());
            crate::ev!("prompt.expired", { "turn": id, "prompt": prompt });
            self.post(&agent, &fragment, records::WORK, records::work_id(&id, &format!("pc:{prompt}")), records::turn_prompt_closed(&id, &prompt, Closed::Expired, None), Vec::new());
            self.out.push(Effect::Runtime(Command::Answer { turn: id, prompt, option: None, seq: 0, by: String::new() }));
        }
        for id in quiet {
            self.out.push(Effect::Runtime(Command::Forget { turn: id.clone() }));
            self.end(&id, Outcome::Error("the agent stopped answering".into()), Closed::Expired);
        }
    }

    fn gone(&mut self, agent: &str, fragment: &str) {
        let ids: Vec<String> = self.state.turns.values().filter(|t| t.agent == agent && t.fragment == fragment).map(|t| t.id.clone()).collect();
        for id in ids {
            let t = self.state.turns.remove(&id).expect("listed");
            self.dirty = true;
            crate::ev!("turn.dropped", { "turn": id, "why": "the agent is no longer in this fragment" });
            if t.active() {
                self.out.push(Effect::Runtime(Command::Forget { turn: id }));
            }
        }
        self.views.remove(fragment);
    }
}

/// Whether `agent` answers to `@word`: its name, or its fragment's label.
fn answers_to(agent: &Agent, word: &str) -> bool {
    agent.name.to_ascii_lowercase() == word || label(&agent.fragment).to_ascii_lowercase() == word
}

/// A loaded state agrees with itself, or is corrupt.
pub fn check(state: &State) -> Result<(), Corrupt> {
    if state.version != STATE_VERSION {
        return Err(Corrupt(format!("state version {} is not {STATE_VERSION}", state.version)));
    }
    if state.turns.len() > limits::TURNS_OPEN_MAX {
        return Err(Corrupt(format!("{} open turns is past the bound", state.turns.len())));
    }
    for (id, t) in &state.turns {
        if &t.id != id {
            return Err(Corrupt(format!("turn {id} holds the id {}", t.id)));
        }
        if records::turn_id(&t.agent, &t.cause.fragment, &t.cause.channel, t.cause.seq) != t.id {
            return Err(Corrupt(format!("turn {id} is not its cause's")));
        }
        let cursor = state.cursors.get(&cursor_key(&t.agent, &t.cause.fragment, &t.cause.channel)).copied().unwrap_or(0);
        if cursor < t.cause.seq {
            return Err(Corrupt(format!("turn {id} is past its channel's cursor {cursor}")));
        }
        if t.order == 0 || t.order > state.admitted {
            return Err(Corrupt(format!("turn {id}'s order {} is not an admission", t.order)));
        }
        if t.phase == Phase::Waiting && t.prompts.iter().all(|p| p.closed) {
            return Err(Corrupt(format!("turn {id} waits on no prompt")));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
