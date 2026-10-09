//! The bridge's core, as a pure state machine: records and runtime events
//! in, posts and runtime commands out. Nothing here does I/O or reads the
//! clock; the driver (driver.rs) feeds it, persists its state whenever a
//! step says so, and only then carries out the step's effects ("validate,
//! persist, then interpret", docs/engineering-style.md).
//!
//! What it keeps (lesson 1, authority): a cursor per followed channel and
//! the turns it admitted that have not ended. The chat channel owns what was
//! said; the chat's `work` channel owns which turns have started (the
//! journal); the runtime owns the in-flight turn; the bridge only
//! translates. Its state is a cache: lost, or restored from any earlier
//! save, it costs reads, never a second run (docs/explorations/
//! pi-durable.md, P1).
//!
//! Its rules, in one place:
//! - A record is admitted once in a life: the cursor of its `(agent,
//!   fragment, channel)` passes it before anything it causes is done, so a
//!   catch-up after a reconnect starts nothing twice (lesson 2).
//! - A turn runs only in the life that claimed it. Each bridge process is a
//!   life, with 128 random bits of its own that are never written to
//!   `/data`. A turn's claim is its `turn.start` on `work`, naming the
//!   life; the runtime is given the turn only once the platform answers
//!   the claim as this life's (appended, or a replay of this life's own
//!   retry). A 409 is another life's claim: never run here, the turn is
//!   ended as lost (a 409 itself when that life ended it) and forgotten. No
//!   answer starts nothing: the turn stays queued and is claimed again. So
//!   a cursor a rollback sent back reads a record again and runs nothing
//!   twice (one life per turn, at most once).
//! - A life's first step ends every turn its state holds that is not
//!   queued: an earlier life claimed it, and that life is gone. The agent's
//!   next turn in that chat is told what was cut (P5): its note is built
//!   from the chat's journal alone, before its claim (note.rs), so it is
//!   said once, and the same in any life.
//! - A turn another life claimed (409) ran after this life's `/data` was
//!   saved, so its runtime remembers none of it: the agent's next turn in
//!   that chat is told what those turns were (`TurnStart::forgotten`,
//!   note.rs "forgotten"), once in this life.
//! - A turn is claimed only while the runtime can take it (`Connected`),
//!   so a turn waits, unclaimed, for a runtime that is still starting.
//! - Every turn gets both records: a turn refused, or stopped while it
//!   waited, posts its `turn.start` and then its `turn.end`.
//! - A turn leaves the state only once its last records are answered: an
//!   ended turn keeps the records it owes `work` (its end; and its start,
//!   when it never ran) until the lane is done with them, and a life that
//!   ends first leaves them to the next, which posts them again under the
//!   same ids and bodies (a replay when they had landed). So a crash
//!   between the state and the platform leaves no turn open.
//! - One turn of an agent runs in a chat at a time; the rest wait in order.
//! - In a chat with several agents, a message is for the agents it names
//!   (`to`, else `@mentions` of this computer's agents), else for the lead,
//!   the first agent added (decision 8). An agent's reply is for another
//!   agent only when it names it, at most `HOPS_MAX` hand-offs deep.
//! - The hop is the answering bridge's to count (`hop_of`), never fewer
//!   than the record claims: a record by an agent of this computer is one
//!   hop past the turn that agent is in (here, or the one just ended here
//!   that its reply names; and its deepest elsewhere; in none, the last hop
//!   allowed), so a post made
//!   around the bridge (the CLI, the API) resets nothing. A person's agents
//!   all run on one computer (decision 13), so every hand-off between them
//!   is counted here; another computer's agent is held by its claim.
//! - A chat's agents start at most `AGENT_TURNS_PER_CHAT_MAX` turns of each
//!   other in `AGENT_TURNS_WINDOW_MS` (the causing records' times, kept in
//!   the state); past it a hand-off is refused, its end saying why.
//! - An agent's `tasks` hears only its own fragment (its cron, the
//!   platform's `joined`) and its owner: another agent acting for the owner
//!   starts no routine there.
//! - Only a turn's asker stops it; only an agent's owner answers its
//!   prompts, the first answer wins, and an unanswered prompt expires
//!   (decision 42).
//! - A turn that asks its asker something in words (`Asked`) takes their
//!   next message in the chat as its answer (`Tell`), never as a turn
//!   behind it. It is a running turn of this life: a restart ends it as
//!   lost, and that message, read by the next life, is a turn of its own.
//! - The computer is kept awake while a turn waits to run, runs, or waits
//!   on its card (at most the card's life), so a card expires with its
//!   runtime there and its turn ends as the runtime ends it, never cut by an
//!   idle sleep (docs/bridge.md, "A card keeps its computer awake": a cut
//!   one left Hermes to meet the next message with the cut request).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::limits;
use crate::records::{self, AttachmentRef, Cause, Closed, Message, Outcome, PromptOption, Record, Said, Task};
use crate::runtime::{Agent, Command, Event, LocalFile, TurnStart};

/// The state file's format. 2: one life per turn (no `handed` phase).
pub const STATE_VERSION: u32 = 2;

/// Why a turn an earlier life claimed is ended.
pub const LOST: &str = "lost when the computer restarted";

/// Why a message is refused a turn: too many wait for its agent in its chat,
/// or the computer holds as many turns as it may. A refused turn posts its
/// start and its end and never runs (the note's rule passes over it:
/// note.rs).
pub const REFUSED_QUEUED: &str = "too many messages are waiting for this agent; send it again once it answers";
pub const REFUSED_BUSY: &str = "this computer is too busy right now";

/// Why an agent's hand-off is refused a turn: the chat's agents started as
/// many turns of each other as they may in the window
/// (`limits::AGENT_TURNS_PER_CHAT_MAX`).
pub fn refused_budget() -> String {
    format!(
        "agents in this chat started {} turns of each other in {} minutes, the most they may; ask again in a few minutes",
        limits::AGENT_TURNS_PER_CHAT_MAX,
        limits::AGENT_TURNS_WINDOW_MS / 60_000
    )
}

/// What the bridge keeps across restarts (`/data/bridge/state.json`): a
/// cache of where to read from and what it has in hand, never the
/// authority on which turns have run (the journal is).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    /// How many times this state was loaded (a restored one counts from its
    /// save's number again): for the log.
    pub boot: u64,
    /// The last seq handled, per `agent|fragment|channel`.
    pub cursors: BTreeMap<String, u64>,
    /// The turns admitted and not over, by id: queued, running, waiting,
    /// or ended and still owing `work` their last records.
    pub turns: BTreeMap<String, Turn>,
    /// Admissions so far: a turn's place in its chat's queue.
    pub admitted: u64,
    /// Messages the runtime said on its own so far (their turns' ids, with
    /// the life).
    pub said: u64,
    /// Per chat, the times (the causing records' `at`, ms, ascending) of
    /// the turns its agents started of each other here lately: its budget
    /// (`limits::AGENT_TURNS_PER_CHAT_MAX` in `AGENT_TURNS_WINDOW_MS`).
    /// Kept, so a restart spends none of it again. A state without it (an
    /// earlier bridge's) starts each chat's count afresh.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub agent_turns: BTreeMap<String, Vec<i64>>,
}

impl Default for State {
    fn default() -> State {
        State { version: STATE_VERSION, boot: 0, cursors: BTreeMap::new(), turns: BTreeMap::new(), admitted: 0, said: 0, agent_turns: BTreeMap::new() }
    }
}

/// Where a turn is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Admitted, and not claimed as this life's: another turn of its agent
    /// runs in its chat, its runtime cannot take it yet, or its claim is
    /// unanswered (in flight, held, or lost). Any life may claim it.
    Queued,
    /// Its claim was answered as this life's, and the runtime has it (a
    /// restart ends it: one life per turn).
    Running,
    /// Waiting on a prompt's answer, or its expiry: the computer is kept
    /// awake for it.
    Waiting,
    /// Over (run and ended, or ended without running: refused, stopped
    /// while it waited, or another life's), and its last records are owed
    /// to `work` until the platform answers them (`Turn::owed`).
    Ended,
}

/// One of a turn's last records, kept with the turn until the lane is done
/// with it: its `turn.end`, and for a turn that never ran its `turn.start`
/// too. A life that ends first leaves it to the next, which posts it again
/// at its start under the same id and body (a replay when it had landed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Owed {
    pub id: String,
    pub body: Value,
}

/// The platform's answer to a claim (`Effect::Claim`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimAnswer {
    /// Appended, or a replay of this life's own retry: this life runs it.
    /// `seq` is the claim's place on the chat's `work`, as the platform's
    /// answer names it: the journal before it says what the turn is told of
    /// the agent's turn before it there (note.rs).
    Ours { seq: Option<u64> },
    /// 409: another life's claim holds the id.
    Theirs,
    /// 403 or 404: the agent may not post on the chat's `work` (it left
    /// the chat, or its owner holds it below editor). Nothing runs, nothing
    /// can be recorded there, and the turn is dropped, as a fragment the
    /// agent is gone from drops its turns.
    Refused,
    /// No answer after the lane's tries, or never sent (the computer is
    /// held): nothing is known, so nothing starts.
    Unanswered,
}

/// A prompt a turn asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub options: Vec<String>,
    /// Its options answered in words.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<String>,
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
    /// When the runtime was last heard about it (or it was handed).
    pub last_ms: u64,
    pub steps: u32,
    /// Replies posted so far, and the highest part among them.
    pub replies: u32,
    pub last_part: u32,
    pub prompts: Vec<Prompt>,
    pub stop_requested: bool,
    /// What an ended turn still owes `work`, in the order posted.
    pub owed: Vec<Owed>,
    /// It asked its asker something to answer in words (`Event::Asked`):
    /// their next message in the chat is its answer, not a turn. Only a
    /// turn this life runs asks: a restart ends it as lost, as it ends every
    /// turn not queued, and a turn's end clears this.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asking: bool,
    #[serde(skip)]
    pub open: Option<OpenReply>,
}

impl Turn {
    /// This life's claim was answered, and the runtime has it.
    fn active(&self) -> bool {
        matches!(self.phase, Phase::Running | Phase::Waiting)
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
    /// The platform's answer to a turn's claim.
    Claimed { turn: String, answer: ClaimAnswer },
    /// The lane is done with an owed record (`Effect::Owed`): answered
    /// (appended, a replay, or refused), or given up after its tries.
    Posted { turn: String, id: String },
    /// Time passed: expiries, quiet turns, and claims to try again.
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
    /// A turn's claim: its `turn.start` on `work` as this life, posted in
    /// its fragment's order unless the computer is held, and answered back
    /// as `Input::Claimed`. The runtime hears of the turn only after.
    Claim { agent: String, fragment: String, turn: String, id: String, body: Value },
    /// One of a turn's last records on `work` (its end; and its start, for
    /// a turn that never ran), kept in the state until the lane is done
    /// with it and says so (`Input::Posted`).
    Owed { agent: String, fragment: String, turn: String, id: String, body: Value },
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
    /// This bridge process's life: never written to the state, so no save
    /// carries it into another life.
    life: String,
    /// The runtime can take turns now.
    connected: bool,
    /// Turns whose claim is posted and not answered yet: queued in the
    /// state, so a crash leaves them for the next life to claim.
    claiming: BTreeSet<String>,
    agents: Vec<Agent>,
    views: HashMap<String, ChatView>,
    keepalive: bool,
    /// Turns that ran here and ended lately, oldest first (at most
    /// `ENDED_HOPS_MAX`, each for `ENDED_HOPS_MS`): a reply of one read after
    /// it was let go is one hop past it (`hop_of`). Never written to `/data`.
    ended: VecDeque<EndedTurn>,
    /// Per agent and chat, its turns there another life ran since this
    /// life's `/data` was saved (each claim answered 409), the newest at
    /// most `NOTE_FORGOTTEN_MAX`, and how many more: its runtime remembers
    /// none of them, so its next turn there is told (`TurnStart::forgotten`)
    /// and the entry goes. Never written to `/data`: a life that restores
    /// the same old `/data` reads them again, and is told again.
    forgotten: BTreeMap<(String, String), (VecDeque<String>, u32)>,
    // A step's scratch, cleared at each step's start.
    out: Vec<Effect>,
    dirty: bool,
    now: u64,
}

/// A turn that ran here and ended: whose, where, how deep, and when (the
/// engine's clock).
#[derive(Debug, Clone, PartialEq)]
struct EndedTurn {
    id: String,
    agent: String,
    fragment: String,
    hop: u32,
    at: u64,
}

pub fn cursor_key(agent: &str, fragment: &str, channel: &str) -> String {
    format!("{agent}|{fragment}|{channel}")
}

fn label(fragment: &str) -> &str {
    fragment.split('.').next().unwrap_or(fragment)
}

impl Engine {
    /// An engine over a loaded state, in the life `life` (this process's,
    /// 32 lowercase hex). It checks the state agrees with itself; then
    /// `recover` must run before any other step.
    pub fn new(state: State, settings: Settings, life: &str) -> Result<Engine, Corrupt> {
        assert!(records::valid_life(life), "a life is 32 lowercase hex: {life}");
        check(&state)?;
        Ok(Engine {
            state,
            settings,
            life: life.to_string(),
            connected: false,
            claiming: BTreeSet::new(),
            agents: Vec::new(),
            views: HashMap::new(),
            keepalive: false,
            ended: VecDeque::new(),
            forgotten: BTreeMap::new(),
            out: Vec::new(),
            dirty: false,
            now: 0,
        })
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn life(&self) -> &str {
        &self.life
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

    /// The first step of a life: a new boot number; every record an ended
    /// turn still owes is posted again, under the same id and body (a
    /// replay when the life before had landed it, so the turn is let go);
    /// and every turn running or waiting is ended as lost. An earlier life
    /// claimed it (its claim was answered, or the state would hold it
    /// queued), and the runtime that had it died with that life: one life
    /// per turn, never handed again. Queued turns stay queued, for this
    /// life to claim once its runtime can take them.
    pub fn recover(&mut self, now: u64) -> Step {
        self.begin(now);
        assert!(self.claiming.is_empty(), "a life recovers before it claims");
        self.state.boot += 1;
        self.dirty = true;
        let owed: Vec<(String, String, String, Owed)> = self.state.turns.values().flat_map(|t| t.owed.iter().map(|o| (t.agent.clone(), t.fragment.clone(), t.id.clone(), o.clone()))).collect();
        for (agent, fragment, turn, o) in owed {
            crate::ev!("owed.again", { "turn": turn, "id": o.id });
            self.owe(&agent, &fragment, &turn, o);
        }
        let earlier: Vec<String> = self.state.turns.values().filter(|t| t.active()).map(|t| t.id.clone()).collect();
        for id in &earlier {
            self.end(id, Outcome::Error(LOST.into()), Closed::Expired);
        }
        assert!(self.state.turns.values().all(|t| matches!(t.phase, Phase::Queued | Phase::Ended)), "a recovered life holds only queued and ended turns");
        self.pump_all();
        self.finish()
    }

    /// One input, its effects, and whether to persist first.
    pub fn step(&mut self, input: Input, now: u64) -> Step {
        self.begin(now);
        match input {
            Input::Agents(agents) => self.set_agents(agents),
            Input::Record { agent, fragment, record, view, since } => self.record(&agent, &fragment, record, view, since),
            Input::Runtime(event) => self.runtime(event),
            Input::Claimed { turn, answer } => self.claimed(&turn, answer),
            Input::Posted { turn, id } => self.posted(&turn, &id),
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
        // a turn waiting on its card holds the computer too, until the card
        // is answered or expires: an idle sleep would cut it
        let busy = self.state.turns.values().any(|t| matches!(t.phase, Phase::Queued | Phase::Running | Phase::Waiting));
        if busy != self.keepalive {
            self.keepalive = busy;
            self.out.push(Effect::Keepalive(busy));
        }
        assert!(self.state.turns.len() <= limits::TURNS_OPEN_MAX, "admission bounds the open turns");
        for id in &self.claiming {
            assert!(self.state.turns.get(id).is_some_and(|t| t.phase == Phase::Queued), "a turn being claimed is held, queued: {id}");
        }
        debug_assert!(check(&self.state).is_ok(), "a step keeps the state whole");
        Step { effects: std::mem::take(&mut self.out), dirty: self.dirty }
    }

    fn agent(&self, fragment: &str) -> Option<&Agent> {
        self.agents.iter().find(|a| a.fragment == fragment)
    }

    fn post(&mut self, agent: &str, fragment: &str, channel: &'static str, id: String, body: Value, files: Vec<LocalFile>) {
        self.out.push(Effect::Post { agent: agent.to_string(), fragment: fragment.to_string(), channel, id, body, files });
    }

    /// Posts a record the turn keeps owing until the lane is done with it.
    fn owe(&mut self, agent: &str, fragment: &str, turn: &str, o: Owed) {
        self.out.push(Effect::Owed { agent: agent.to_string(), fragment: fragment.to_string(), turn: turn.to_string(), id: o.id, body: o.body });
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
            self.claiming.remove(&id);
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
        if record.principal.starts_with("anon:") {
            return;
        }
        match record.channel.as_str() {
            // the agent's own words are not for it
            records::CHAT if record.principal != agent.identity => self.said(&agent, fragment, &record),
            records::TASKS if fragment == agent.fragment => {
                // Only the agent's own fragment (its cron, the platform's
                // `joined`: by the fragment's key, whose npub is the agent's
                // identity) and its owner ask it anything here. Another
                // agent, though it acts for the owner and may post here,
                // starts no routine: it would start a turn no hop counts.
                if record.principal != agent.owner && record.principal != agent.identity {
                    crate::ev!("task.ignored", { "agent": agent.fragment, "seq": record.seq, "principal": record.principal, "why": "only the agent's owner, or its own fragment, asks it on tasks" });
                    return;
                }
                self.task(&agent, &record)
            }
            _ => {}
        }
    }

    fn said(&mut self, agent: &Agent, fragment: &str, record: &Record) {
        match records::said(&record.body) {
            Said::Message(m) => {
                let view = self.views.get(fragment).cloned().unwrap_or_default();
                let hop = self.addressed(agent, fragment, &view, &record.principal, &m);
                if hop == Some(0) && self.told(agent, fragment, &record.principal, &view, &m, record.seq) {
                    return;
                }
                if let Some(hop) = hop {
                    let asker_name = view.names.get(&record.principal).cloned().unwrap_or_else(|| "someone".into());
                    let cause = Cause { fragment: fragment.to_string(), channel: record.channel.clone(), seq: record.seq };
                    self.admit(agent, fragment, cause, record.at, &record.principal, asker_name, &m, hop, false);
                }
            }
            Said::Stop { turn } => self.stop(agent, fragment, &record.principal, turn.as_deref()),
            Said::PromptResponse { prompt, option, text } => self.answer(agent, fragment, &record.principal, &prompt, &option, text, record.seq),
            Said::Other => {}
        }
    }

    fn task(&mut self, agent: &Agent, record: &Record) {
        match records::task(&record.body) {
            Task::Routine { text, chat } => {
                let cause = Cause { fragment: agent.fragment.clone(), channel: record.channel.clone(), seq: record.seq };
                let m = Message { text, ..Message::default() };
                let owner = agent.owner.clone();
                self.admit(agent, &chat, cause, record.at, &owner, "your routine".into(), &m, 0, true);
            }
            Task::Joined { fragment } => self.out.push(Effect::Discover { agent: agent.fragment.clone(), joined: fragment }),
            Task::Other => {}
        }
    }

    /// Whether a message is for `agent`, and the hop its turn is at.
    fn addressed(&self, agent: &Agent, fragment: &str, view: &ChatView, principal: &str, m: &Message) -> Option<u32> {
        let from_agent = view.agents.iter().any(|a| a == principal) || self.agents.iter().any(|a| a.identity == principal);
        if from_agent {
            if !m.to.iter().any(|t| t == &agent.identity) {
                return None;
            }
            let hop = self.hop_of(fragment, principal, m.turn.as_deref(), m.hop);
            if hop > limits::HOPS_MAX {
                crate::ev!("handoff.too_deep", { "agent": agent.fragment, "fragment": fragment, "from": principal, "hop": hop, "claimed": m.hop });
                return None;
            }
            return Some(hop);
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

    /// How many hand-offs led to a record an agent posted in `fragment`:
    /// this bridge's count, never fewer than the record `claimed`. For an
    /// agent of this computer it is one past the turn the agent is in: its
    /// turn running in this chat, or the turn here the record names (`turn`)
    /// when that ended within `ENDED_HOPS_MS` (a reply read after its turn
    /// was let go); and its deepest turn running elsewhere (a post from a
    /// turn into another chat, as `fragment ask` makes), the deeper of the
    /// two; in no turn at all, the last hop allowed: answered once, handing
    /// on nothing. So a post made around the bridge (the CLI, the API: no
    /// `hop`, or `hop: 0`, and no `turn`) counts as the reply would. Another
    /// computer's agent, whose turns this bridge cannot see, is one hop at
    /// least, as it claims; the chat's budget holds it too.
    fn hop_of(&self, fragment: &str, principal: &str, named: Option<&str>, claimed: u32) -> u32 {
        let Some(poster) = self.agents.iter().find(|a| a.identity == principal) else {
            return claimed.max(1);
        };
        let running = |t: &&Turn| t.agent == poster.fragment && t.active();
        let here = self.state.turns.values().filter(running).find(|t| t.fragment == fragment).map(|t| t.hop).or_else(|| {
            let fresh = |e: &&EndedTurn| self.now.saturating_sub(e.at) <= limits::ENDED_HOPS_MS;
            let named = named?;
            self.ended.iter().rev().filter(fresh).find(|e| e.id == named && e.agent == poster.fragment && e.fragment == fragment).map(|e| e.hop)
        });
        let elsewhere = self.state.turns.values().filter(running).filter(|t| t.fragment != fragment).map(|t| t.hop).max();
        let from = here.into_iter().chain(elsewhere).max().unwrap_or(limits::HOPS_MAX - 1);
        from.saturating_add(1).max(claimed)
    }

    /// Whether the chat's agents may start another turn of each other at
    /// `at` (a causing record's time): fewer than
    /// `AGENT_TURNS_PER_CHAT_MAX` in the window before it. Times older than
    /// the window are let go; a later one (another follower's, ahead of
    /// this one) still counts.
    fn agent_turn_allowed(&mut self, chat: &str, at: i64) -> bool {
        let Some(times) = self.state.agent_turns.get_mut(chat) else { return true };
        let before = times.len();
        times.retain(|t| *t > at.saturating_sub(limits::AGENT_TURNS_WINDOW_MS));
        if times.len() != before {
            self.dirty = true;
        }
        if times.is_empty() {
            self.state.agent_turns.remove(chat);
            return true;
        }
        times.len() < limits::AGENT_TURNS_PER_CHAT_MAX
    }

    /// Counts a turn the chat's agents started of each other at `at`.
    fn spend_agent_turn(&mut self, chat: &str, at: i64) {
        let times = self.state.agent_turns.entry(chat.to_string()).or_default();
        let place = times.partition_point(|t| *t <= at);
        times.insert(place, at);
        // the newest only: the oldest past the cap could never refuse one
        if times.len() > limits::AGENT_TURNS_PER_CHAT_MAX {
            let over = times.len() - limits::AGENT_TURNS_PER_CHAT_MAX;
            times.drain(..over);
        }
        self.dirty = true;
        // bounded: the chat counted least lately goes
        while self.state.agent_turns.len() > limits::AGENT_TURN_CHATS_MAX {
            let stalest = self.state.agent_turns.iter().min_by_key(|(_, t)| t.last().copied().unwrap_or(i64::MIN)).map(|(c, _)| c.clone()).expect("over the cap, so not empty");
            self.state.agent_turns.remove(&stalest);
        }
    }

    /// A turn for `agent` in `fragment`, caused by a record at `at` (the
    /// platform's time); `hop` past zero is a hand-off from another agent,
    /// which the chat's budget counts.
    #[allow(clippy::too_many_arguments)]
    fn admit(&mut self, agent: &Agent, fragment: &str, cause: Cause, at: i64, asker: &str, asker_name: String, m: &Message, hop: u32, routine: bool) {
        let id = records::turn_id(&agent.fragment, &cause.fragment, &cause.channel, cause.seq);
        if self.state.turns.contains_key(&id) {
            // The cursor makes this impossible; a state that says otherwise
            // is not trusted to go on.
            panic!("turn {id} admitted twice: the cursor of {} is behind its turns", cursor_key(&agent.fragment, &cause.fragment, &cause.channel));
        }
        assert!(!routine || hop == 0, "a routine is its owner's ask, no hand-off");
        let waiting = self.state.turns.values().filter(|t| t.agent == agent.fragment && t.fragment == fragment && t.phase == Phase::Queued).count();
        let refusal = if hop > 0 && !self.agent_turn_allowed(fragment, at) {
            Some(refused_budget())
        } else if waiting >= limits::QUEUED_PER_CHAT_MAX {
            Some(REFUSED_QUEUED.to_string())
        } else if self.state.turns.len() >= limits::TURNS_OPEN_MAX {
            Some(REFUSED_BUSY.to_string())
        } else {
            None
        };
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
            last_ms: self.now,
            steps: 0,
            replies: 0,
            last_part: 0,
            prompts: Vec::new(),
            stop_requested: false,
            owed: Vec::new(),
            asking: false,
            open: None,
        };
        self.dirty = true;
        if let Some(why) = refusal {
            crate::ev!("turn.refused", { "turn": id, "agent": agent.fragment, "fragment": fragment, "why": why });
            let outcome = Outcome::Error(why);
            if self.state.turns.len() < limits::TURNS_OPEN_MAX {
                self.state.turns.insert(id.clone(), turn);
                self.end_unrun(&id, outcome);
            } else {
                // Past the bound of turns held, its two records are posted
                // and not kept: a crash before they land loses them.
                crate::ev!("owed.unkept", { "turn": id, "why": "the computer holds as many turns as it may" });
                let start = records::turn_start(&id, asker, &agent.identity, &turn.cause, &self.life);
                self.post(&agent.fragment, fragment, records::WORK, records::work_id(&id, "start"), start, Vec::new());
                self.post(&agent.fragment, fragment, records::WORK, records::work_id(&id, "end"), records::turn_end(&id, &outcome), Vec::new());
            }
            return;
        }
        crate::ev!("turn.admitted", { "turn": id, "agent": agent.fragment, "fragment": fragment, "seq": turn.cause.seq, "hop": hop });
        if hop > 0 {
            self.spend_agent_turn(fragment, at);
        }
        self.state.turns.insert(id, turn);
        self.pump(&agent.fragment, fragment);
    }

    /// A held turn ends without running here (refused, or stopped while it
    /// waited): it owes its claim, then its end, so it has both records as
    /// every turn does, and it is kept until both are answered. When
    /// another life claimed it, the first is a 409, and so is the second if
    /// that life ended it.
    fn end_unrun(&mut self, id: &str, outcome: Outcome) {
        self.claiming.remove(id);
        let agent = self.state.turns[id].agent.clone();
        let identity = self.agent(&agent).expect("a held turn's agent is on this computer").identity.clone();
        let life = self.life.clone();
        let t = self.state.turns.get_mut(id).expect("held");
        assert_eq!(t.phase, Phase::Queued, "a turn ends unrun only from the queue");
        let start = Owed { id: records::work_id(id, "start"), body: records::turn_start(id, &t.asker, &identity, &t.cause, &life) };
        let end = Owed { id: records::work_id(id, "end"), body: records::turn_end(id, &outcome) };
        t.phase = Phase::Ended;
        t.owed = vec![start.clone(), end.clone()];
        let fragment = t.fragment.clone();
        self.dirty = true;
        self.owe(&agent, &fragment, id, start);
        self.owe(&agent, &fragment, id, end);
    }

    /// The lane is done with one of a turn's owed records; a turn that owes
    /// none is let go. Done means answered (appended, a replay, or refused:
    /// a 409 is another life's end of the turn, a 403/404 a chat it may no
    /// longer post in) or given up after the lane's tries (docs/bridge.md:
    /// `POST_TRIES_MAX`, with jitter, on a transport error, 429 or 5xx), so
    /// a record that keeps failing holds its turn for one life's tries at
    /// most, then goes.
    fn posted(&mut self, id: &str, post: &str) {
        let Some(t) = self.state.turns.get_mut(id) else {
            crate::ev!("owed.stale", { "turn": id, "id": post, "why": "no turn held by that id" });
            return;
        };
        if t.phase != Phase::Ended || !t.owed.iter().any(|o| o.id == post) {
            crate::ev!("owed.stale", { "turn": id, "id": post, "why": "the turn does not owe it" });
            return;
        }
        t.owed.retain(|o| o.id != post);
        self.dirty = true;
        if t.owed.is_empty() {
            self.state.turns.remove(id);
            crate::ev!("turn.closed", { "turn": id });
        }
    }

    /// Claims the chat's next turn when its runtime can take one and none
    /// of its agent's runs, or is being claimed, there. Nothing is persisted:
    /// the turn stays queued in the state until its claim is answered, so a
    /// life that ends meanwhile leaves it for the next to claim (a 409 then,
    /// if this claim had landed).
    fn pump(&mut self, agent: &str, fragment: &str) {
        if !self.connected {
            return;
        }
        let busy = self.state.turns.values().any(|t| t.agent == agent && t.fragment == fragment && (t.active() || self.claiming.contains(&t.id)));
        if busy {
            return;
        }
        let next = self.state.turns.values().filter(|t| t.agent == agent && t.fragment == fragment && t.phase == Phase::Queued).min_by_key(|t| t.order).map(|t| t.id.clone());
        let Some(id) = next else { return };
        let identity = self.agent(agent).expect("a held turn's agent is on this computer").identity.clone();
        let t = &self.state.turns[&id];
        let body = records::turn_start(&id, &t.asker, &identity, &t.cause, &self.life);
        let fragment = t.fragment.clone();
        self.claiming.insert(id.clone());
        crate::ev!("turn.claiming", { "turn": id, "agent": agent, "fragment": fragment });
        self.out.push(Effect::Claim { agent: agent.to_string(), fragment, turn: id.clone(), id: records::work_id(&id, "start"), body });
    }

    /// Every chat's queue moves (a claim may be due in any).
    fn pump_all(&mut self) {
        let chats: BTreeSet<(String, String)> = self.state.turns.values().map(|t| (t.agent.clone(), t.fragment.clone())).collect();
        for (agent, fragment) in chats {
            self.pump(&agent, &fragment);
        }
    }

    /// A claim's answer: this life runs the turn; or it is another life's,
    /// and ends as lost; or nothing is known, and it waits for the next
    /// tick's claim (never sooner, so a held computer is asked once a tick).
    fn claimed(&mut self, id: &str, answer: ClaimAnswer) {
        if !self.claiming.remove(id) {
            // Stopped, or its agent gone, while its claim was in flight.
            crate::ev!("claim.stale", { "turn": id, "answer": format!("{answer:?}") });
            return;
        }
        assert!(self.state.turns.get(id).is_some_and(|t| t.phase == Phase::Queued), "a turn being claimed is held, queued: {id}");
        match answer {
            ClaimAnswer::Ours { seq } => self.hand(id, seq),
            ClaimAnswer::Theirs => {
                // It owes its end as lost (a 409 itself when that life ended
                // it, and let go then like any answered end).
                let t = self.state.turns.get_mut(id).expect("checked");
                let end = Owed { id: records::work_id(id, "end"), body: records::turn_end(id, &Outcome::Error(LOST.into())) };
                t.phase = Phase::Ended;
                t.owed = vec![end.clone()];
                let (agent, fragment) = (t.agent.clone(), t.fragment.clone());
                self.dirty = true;
                crate::ev!("turn.lost", { "turn": id, "agent": agent, "why": "another life claimed it" });
                // that life ran it after this /data was saved: the runtime
                // remembers nothing of it, and its next turn here is told
                let (newest, more) = self.forgotten.entry((agent.clone(), fragment.clone())).or_default();
                newest.push_back(id.to_string());
                if newest.len() > limits::NOTE_FORGOTTEN_MAX {
                    newest.pop_front();
                    *more = more.saturating_add(1);
                }
                assert!(newest.len() <= limits::NOTE_FORGOTTEN_MAX, "a note names its newest forgotten turns");
                self.owe(&agent, &fragment, id, end);
                self.pump(&agent, &fragment);
            }
            ClaimAnswer::Refused => {
                let t = self.state.turns.remove(id).expect("checked");
                self.dirty = true;
                crate::ev!("turn.dropped", { "turn": id, "agent": t.agent, "why": "the agent may not post on the chat's work" });
                self.pump(&t.agent, &t.fragment);
            }
            ClaimAnswer::Unanswered => crate::ev!("claim.unanswered", { "turn": id }),
        }
    }

    /// Hands a turn this life claimed to the runtime: from here a restart
    /// ends it. Its note (what a restart cut of the agent's turn before it
    /// in this chat) is the driver's to read from the journal before the
    /// runtime hears of the turn, as its attachments are.
    fn hand(&mut self, id: &str, claim_seq: Option<u64>) {
        let agent = self.state.turns[id].agent.clone();
        let a = self.agent(&agent).cloned().expect("a held turn's agent is on this computer");
        let now = self.now;
        let fragment = self.state.turns[id].fragment.clone();
        // what another life ran here that the runtime does not remember,
        // told once: this turn takes it
        let (forgotten, forgotten_more) = self.forgotten.remove(&(agent.clone(), fragment)).map(|(newest, more)| (Vec::from(newest), more)).unwrap_or_default();
        let t = self.state.turns.get_mut(id).expect("checked");
        t.phase = Phase::Running;
        t.last_ms = now;
        self.dirty = true;
        let start = TurnStart {
            turn: id.to_string(),
            agent: a,
            fragment: t.fragment.clone(),
            chat_name: label(&t.fragment).to_string(),
            seq: t.cause.seq,
            asker: t.asker.clone(),
            asker_name: t.asker_name.clone(),
            text: t.text.clone(),
            attachments: t.attachments.clone(),
            files: Vec::new(),
            routine: t.routine,
            claim_seq,
            note: None,
            forgotten,
            forgotten_more,
        };
        crate::ev!("turn.handed", { "turn": id, "agent": agent, "fragment": t.fragment });
        self.out.push(Effect::Runtime(Command::Start(Box::new(start))));
    }

    // ---- Stop and answers ----

    /// A message from the asker of the agent's turn in this chat that asked
    /// them something (`asking`): handed to that turn as its answer (true),
    /// never queued behind it, where the turn would wait for it forever.
    /// Only a turn this life runs asks (its claim was answered as this
    /// life's; a restart ends every turn an earlier life ran), so an answer
    /// goes only to the life that claimed its turn. Read by a later life, it
    /// asks nothing, and the message is a turn of its own, claimed and run
    /// as any other.
    fn told(&mut self, agent: &Agent, fragment: &str, principal: &str, view: &ChatView, m: &Message, seq: u64) -> bool {
        if m.text.trim().is_empty() {
            return false;
        }
        let found = self.state.turns.values().find(|t| t.agent == agent.fragment && t.fragment == fragment && t.asking && t.active()).map(|t| (t.id.clone(), t.asker.clone()));
        let Some((id, asker)) = found else { return false };
        if asker != principal {
            return false;
        }
        let t = self.state.turns.get_mut(&id).expect("found");
        t.asking = false;
        t.last_ms = self.now;
        self.dirty = true;
        crate::ev!("turn.told", { "turn": id, "seq": seq });
        let by_name = view.names.get(principal).cloned().unwrap_or_else(|| "someone".into());
        self.out.push(Effect::Runtime(Command::Tell { turn: id, seq, by: principal.to_string(), by_name, text: records::cut_bytes(&m.text, limits::MESSAGE_TEXT_MAX_BYTES) }));
        true
    }

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
                // Never run here (a claim in flight is answered to no one).
                crate::ev!("turn.stopped", { "turn": id, "phase": "queued" });
                self.end_unrun(&id, Outcome::Stopped);
                self.pump(&agent.fragment, fragment);
            }
            Phase::Ended => crate::ev!("stop.ignored", { "turn": id, "why": "it is over" }),
            Phase::Running | Phase::Waiting => {
                let t = self.state.turns.get_mut(&id).expect("found");
                if t.stop_requested {
                    return;
                }
                t.stop_requested = true;
                // a Stop is its question's answer too (Relay says "Stop." to
                // a clarify waiting on words): the next message is a turn
                t.asking = false;
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

    /// An answer to a turn's prompt, from the agent's owner. Words go with
    /// it only for an option answered in words (the runtime asks for them
    /// otherwise, as its question in words).
    #[allow(clippy::too_many_arguments)]
    fn answer(&mut self, agent: &Agent, fragment: &str, principal: &str, prompt: &str, option: &str, text: Option<String>, seq: u64) {
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
        let words = text.filter(|_| p.words.iter().any(|w| w == option));
        if t.prompts.iter().all(|p| p.closed) && t.phase == Phase::Waiting {
            t.phase = Phase::Running;
        }
        t.last_ms = now;
        self.dirty = true;
        crate::ev!("prompt.answered", { "turn": id, "prompt": prompt, "option": option, "words": words.is_some() });
        let closed = records::turn_prompt_closed(&id, prompt, Closed::Answered, Some((option, principal, words.as_deref())));
        self.post(&agent.fragment, fragment, records::WORK, records::work_id(&id, &format!("pc:{prompt}")), closed, Vec::new());
        self.out.push(Effect::Runtime(Command::Answer { turn: id, prompt: prompt.to_string(), option: Some(option.to_string()), seq, by: principal.to_string(), words }));
    }

    // ---- the runtime ----

    fn runtime(&mut self, event: Event) {
        let turn_of = |e: &Event| -> Option<String> {
            match e {
                Event::Draft { turn, .. } | Event::Reply { turn, .. } | Event::Attachment { turn, .. } | Event::Retract { turn, .. } => Some(turn.clone()),
                Event::Step { turn, .. } | Event::Prompt { turn, .. } | Event::Asked { turn } | Event::End { turn, .. } => Some(turn.clone()),
                Event::Connected(_) | Event::Say { .. } => None,
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
            Event::Connected(connected) => {
                if connected != self.connected {
                    crate::ev!("runtime.connected", { "connected": connected });
                }
                self.connected = connected;
                self.pump_all();
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
            Event::Asked { turn } => {
                // the question shows before the answer it waits for
                self.seal(&turn);
                let t = self.state.turns.get_mut(&turn).expect("checked");
                if !t.asking {
                    t.asking = true;
                    self.dirty = true;
                    crate::ev!("turn.asked", { "turn": turn });
                }
            }
            Event::End { turn, outcome } => self.end(&turn, outcome, Closed::Expired),
            Event::Say { agent, fragment, text } => {
                if self.agent(&agent).is_none() || text.trim().is_empty() {
                    return;
                }
                self.state.said += 1;
                self.dirty = true;
                let turn = records::said_turn_id(&agent, &self.life, self.state.said);
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
            self.out.push(Effect::Runtime(Command::Answer { turn: id.to_string(), prompt, option: None, seq: 0, by: String::new(), words: None }));
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
        t.prompts.push(Prompt { id: prompt.clone(), options: options.iter().map(|o| o.id.clone()).collect(), words: options.iter().filter(|o| o.words).map(|o| o.id.clone()).collect(), expires_at, closed: false });
        t.phase = Phase::Waiting;
        self.dirty = true;
        let (agent, fragment) = (t.agent.clone(), t.fragment.clone());
        crate::ev!("prompt.asked", { "turn": id, "prompt": prompt, "expiresAt": expires_at });
        self.post(&agent, &fragment, records::WORK, records::work_id(id, &format!("p:{prompt}")), records::turn_prompt(id, &prompt, &text, &options, &owner, expires_at), Vec::new());
    }

    /// The turn is over: its last reply, its open prompts closed as
    /// `unanswered`, its end, and its chat's next turn.
    /// The turn's run is over: its last reply, its open prompts closed as
    /// `unanswered`, its end (owed: kept with the turn until answered), and
    /// its chat's next turn.
    fn end(&mut self, id: &str, outcome: Outcome, unanswered: Closed) {
        self.seal(id);
        let t = self.state.turns.get_mut(id).expect("ending a held turn");
        assert!(t.active(), "a turn's run ends once: {id} is {:?}", t.phase);
        let prompts = std::mem::take(&mut t.prompts);
        let end = Owed { id: records::work_id(id, "end"), body: records::turn_end(id, &outcome) };
        t.phase = Phase::Ended;
        t.owed = vec![end.clone()];
        // over, it asks nothing: its asker's next message is a turn
        t.asking = false;
        let (agent, fragment, hop) = (t.agent.clone(), t.fragment.clone(), t.hop);
        self.dirty = true;
        // its replies may be read after it is let go: one hop past it
        self.ended.push_back(EndedTurn { id: id.to_string(), agent: agent.clone(), fragment: fragment.clone(), hop, at: self.now });
        while self.ended.len() > limits::ENDED_HOPS_MAX {
            self.ended.pop_front();
        }
        crate::ev!("turn.end", { "turn": id, "agent": agent, "fragment": fragment, "outcome": match &outcome { Outcome::Idle => "idle", Outcome::Stopped => "stopped", Outcome::Error(_) => "error" } });
        for p in prompts.iter().filter(|p| !p.closed) {
            self.post(&agent, &fragment, records::WORK, records::work_id(id, &format!("pc:{}", p.id)), records::turn_prompt_closed(id, &p.id, unanswered, None), Vec::new());
        }
        self.owe(&agent, &fragment, id, end);
        self.draft(&agent, &fragment, id, None);
        self.pump(&agent, &fragment);
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
            // a turn asking its person waits as long as a prompt would
            let bound = if t.asking { self.settings.turn_idle_ms.max(self.settings.prompt_ttl_ms) } else { self.settings.turn_idle_ms };
            let idle = t.phase == Phase::Running && now.saturating_sub(t.last_ms) > bound;
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
            self.out.push(Effect::Runtime(Command::Answer { turn: id, prompt, option: None, seq: 0, by: String::new(), words: None }));
        }
        for id in quiet {
            self.out.push(Effect::Runtime(Command::Forget { turn: id.clone() }));
            self.end(&id, Outcome::Error("the agent stopped answering".into()), Closed::Expired);
        }
        // A claim left unanswered (no answer, or the computer held) is
        // claimed again.
        self.pump_all();
    }

    fn gone(&mut self, agent: &str, fragment: &str) {
        let ids: Vec<String> = self.state.turns.values().filter(|t| t.agent == agent && t.fragment == fragment).map(|t| t.id.clone()).collect();
        for id in ids {
            let t = self.state.turns.remove(&id).expect("listed");
            self.claiming.remove(&id);
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
        if t.asking && !t.active() {
            return Err(Corrupt(format!("turn {id} is {:?} and asks its asker", t.phase)));
        }
        let ended = t.phase == Phase::Ended;
        if ended == t.owed.is_empty() {
            return Err(Corrupt(format!("turn {id} is {:?} and owes {} records", t.phase, t.owed.len())));
        }
        if t.owed.len() > 2 {
            return Err(Corrupt(format!("turn {id} owes {} records: at most its start and its end", t.owed.len())));
        }
        let own = format!("wk:{id}:");
        if let Some(o) = t.owed.iter().find(|o| !o.id.starts_with(&own) || o.body["turn"] != id.as_str()) {
            return Err(Corrupt(format!("turn {id} owes {}, not one of its own", o.id)));
        }
        if t.routine && t.hop != 0 {
            return Err(Corrupt(format!("turn {id} is a routine at hop {}", t.hop)));
        }
    }
    if state.agent_turns.len() > limits::AGENT_TURN_CHATS_MAX {
        return Err(Corrupt(format!("{} chats' agent turns is past the bound", state.agent_turns.len())));
    }
    for (chat, times) in &state.agent_turns {
        if times.is_empty() || times.len() > limits::AGENT_TURNS_PER_CHAT_MAX || !times.windows(2).all(|w| w[0] <= w[1]) {
            return Err(Corrupt(format!("chat {chat}'s agent turns are not 1 to {} times in order", limits::AGENT_TURNS_PER_CHAT_MAX)));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
