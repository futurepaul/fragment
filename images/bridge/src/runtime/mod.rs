//! The runtime side of the bridge: whatever runs the agents. The bridge's
//! core speaks only these commands and events; each runtime translates them
//! into its own protocol. Two ship here:
//!
//! - `goose`: goose over ACP on its stdio, one `goose acp` per agent and a
//!   fresh session per turn (docs/optchat.md; our goose image).
//! - `script`: a deterministic scripted agent (the stub image), so the
//!   platform's lanes run without any agent runtime at all.
//!
//! Another runtime brings its own module here, or its own bridge: nothing
//! the platform does depends on which one runs (docs/cloudflare-v1.md, the
//! rule).
//!
//! A runtime owns the in-flight turn (lesson 1): the bridge hands it a turn
//! once, and hears back. It never talks to the fragment API: the bridge
//! downloads a turn's attachments before handing it, and uploads a reply's.
//! A turn is handed only in the life that claimed it (docs/chat-records.md,
//! `turn.start`), and a turn is claimed only while the runtime says it can
//! take one (`Event::Connected`).

pub mod goose;
pub mod script;
pub mod skills;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use tokio::sync::{mpsc, watch};

use crate::records::{PromptOption, Step};

/// An agent the computer runs (`GET /api/computer`'s `agents`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Agent {
    /// The agent fragment's name (`<label>.<username>`): who requests act as
    /// (`x-fragment-agent`).
    pub fragment: String,
    /// The agent's identity (`id:…`): who its records are by.
    pub identity: String,
    /// Its display name, which `@mentions` use.
    pub name: String,
    /// Its owner: the only one who answers its prompts (decision 42).
    pub owner: String,
    /// The credentials it may use now (docs/computers.md, "Connections and
    /// operator keys"): each a placeholder that names the agent, which an
    /// image puts in the environment variables named, for the computer's
    /// swap to fill on the way to the provider's hosts. The bridge sends
    /// none itself.
    #[serde(default)]
    pub credentials: Vec<Credential>,
}

/// One credential of an agent's (`GET /api/computer`'s `agents[].credentials`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Credential {
    pub provider: String,
    /// `connection`, `operator` or `own`.
    pub kind: String,
    /// The environment variables it goes in (the vendor's SDK's names).
    pub env: Vec<String>,
    /// `fcx_<provider>_<tag>` or `fck_<provider>_<tag>`.
    pub placeholder: String,
    /// The only hosts it is swapped for.
    pub hosts: Vec<String>,
}

/// The most credentials one agent is given (the platform's catalog holds
/// at most 64 providers).
pub const CREDENTIALS_MAX: usize = 64;

/// A file on the bridge's disk: an attachment, downloaded for the runtime
/// or written by it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalFile {
    pub path: PathBuf,
    pub media_type: String,
    pub name: String,
    pub size: u64,
}

/// A turn handed to a runtime.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnStart {
    pub turn: String,
    pub agent: Agent,
    /// The chat the turn answers in.
    pub fragment: String,
    /// The chat's label, for the runtime to call it by.
    pub chat_name: String,
    /// The record that started it (its seq in its channel).
    pub seq: u64,
    /// Who asked, and what the runtime calls them.
    pub asker: String,
    pub asker_name: String,
    pub text: String,
    /// The message's attachments, as the record names them.
    pub attachments: Vec<crate::records::AttachmentRef>,
    /// The same attachments on disk (the driver downloads them before the
    /// runtime hears of the turn).
    pub files: Vec<LocalFile>,
    /// A routine (its agent's cron) rather than a person's message.
    pub routine: bool,
    /// Where the turn's claim (its `turn.start`) is on its chat's `work`:
    /// the journal before it says what the turn is told (`note`). `None`
    /// when the platform's answer did not say.
    pub claim_seq: Option<u64>,
    /// What the runtime tells the agent before the message, once
    /// (docs/durable-computers.md, P5): its turn before this one in this
    /// chat was cut by a restart, what that turn was asked, the steps and
    /// replies the journal recorded of it, and to check what was done before
    /// doing any of it again. Built from the chat's journal alone
    /// (crate::note), by the driver, before the runtime hears of the turn;
    /// `None` when that turn ended any other way. Every runtime is handed
    /// it; the scripted agent echoes it.
    pub note: Option<String>,
}

/// What the bridge asks of a runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Start(Box<TurnStart>),
    /// The turn's asker pressed Stop.
    Stop { turn: String },
    /// A prompt's answer (`option`), or its expiry (`None`): only ever once
    /// per prompt. `seq` is the answering record's (a fresh message id).
    Answer { turn: String, prompt: String, option: Option<String>, seq: u64, by: String },
    /// The bridge ended the turn itself (it went quiet too long, or its
    /// agent left): forget it.
    Forget { turn: String },
    /// The asker's next message in the turn's chat, while the turn asked
    /// them something to answer in words (`Event::Asked`): the answer,
    /// handed to the running turn at once, never a turn of its own. `seq`
    /// is the message's record (a fresh message id).
    Tell { turn: String, seq: u64, by: String, by_name: String, text: String },
}

/// What a runtime tells the bridge. Every event but `Connected` and `Say`
/// names its turn; one for a turn the bridge no longer holds is dropped.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The runtime can take turns now (true), or cannot (false). The bridge
    /// claims a turn only while it can, so a turn claimed is one the runtime
    /// is there to run. A runtime that can always take one says so as it
    /// starts.
    Connected(bool),
    /// The reply being written, its whole text so far (shown live, never
    /// stored); empty, the draft stops.
    Draft { turn: String, text: String },
    /// Reply `part` (from 1, in order) of the turn, its whole text now. A
    /// later part, a step, a prompt, or the end posts it.
    Reply { turn: String, part: u32, text: String },
    /// A file reply `part` carries (opened empty if it was not).
    Attachment { turn: String, part: u32, file: LocalFile },
    /// Reply `part` taken back before it was posted.
    Retract { turn: String, part: u32 },
    /// A tool call.
    Step { turn: String, step: Step },
    /// A question with buttons; the turn waits for its answer.
    Prompt { turn: String, prompt: String, text: String, options: Vec<PromptOption>, ttl_ms: Option<u64> },
    /// The turn asked its asker something to answer in words (the question
    /// itself is a reply part before this): it waits, running, for their
    /// next message in its chat, which the bridge hands it (`Command::Tell`)
    /// instead of queueing it as a turn behind this one.
    Asked { turn: String },
    /// How long the turn's phases took, in milliseconds by name (a
    /// runtime's own: docs/chat-records.md, `turn.timing`), said once, just
    /// before its end.
    Timing { turn: String, timing: serde_json::Map<String, serde_json::Value> },
    /// The turn is over.
    End { turn: String, outcome: crate::records::Outcome },
    /// Something the runtime said with no turn running (a reminder it set):
    /// posted as a turn of its own in that chat.
    Say { agent: String, fragment: String, text: String },
}

/// Why a runtime stopped.
#[derive(Debug)]
pub enum RuntimeError {
    /// It could not start, or read its configuration.
    Setup(String),
    /// Its loop failed for good.
    Failed(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Setup(m) => write!(f, "runtime setup: {m}"),
            RuntimeError::Failed(m) => write!(f, "runtime failed: {m}"),
        }
    }
}

/// A runtime's two queues, and the bridge's shutdown signal.
pub struct RuntimeIo {
    pub commands: mpsc::Receiver<Command>,
    pub events: mpsc::Sender<Event>,
    pub shutdown: watch::Receiver<bool>,
}

pub type RuntimeFuture = Pin<Box<dyn Future<Output = Result<(), RuntimeError>> + Send>>;

/// An agent runtime the bridge drives.
pub trait Runtime: Send + 'static {
    /// Its name in the log and in `--runtime`.
    fn name(&self) -> &'static str;
    /// Runs until `shutdown` turns true (then returns promptly: the whole
    /// bridge has `limits::SHUTDOWN_MS_MAX`), or fails for good.
    fn run(self: Box<Self>, io: RuntimeIo) -> RuntimeFuture;
}
