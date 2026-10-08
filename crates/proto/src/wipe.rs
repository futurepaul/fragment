//! An operator's wipe of a person on the wire (docs/api.md, Operators:
//! `GET|POST /api/people/{person}/wipe`). The steps and their rules are
//! `fragment_core::wipe`'s.

use serde::{Deserialize, Serialize};

/// `POST /api/people/{person}/wipe`'s body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WipeAsk {
    /// The identity the dry run answered (`WipeReport::identity`): a wipe
    /// of anyone else is refused (409), and nothing changes.
    pub confirm: String,
    /// The most steps this call runs (default: as many as its time allows):
    /// a wipe stopped between two steps on purpose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<u32>,
}

/// Where a person is: never wiped, being wiped (locked: no sign-in, no
/// key, no session), or wiped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WipeState {
    Live,
    Wiping,
    Wiped,
}

/// Names a report lists: how many in all, a bounded few, and whether there
/// are more than it shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub count: u64,
    pub names: Vec<String>,
    pub more: bool,
}

/// Their computer, as a wipe finds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerFound {
    pub computer: String,
    /// Its phase, while it is made (`None`: never made, or wiped).
    pub phase: Option<String>,
    /// The saves its record keeps.
    pub saves: u64,
    /// Objects under its saves' prefix in R2 (one page's count; `more`:
    /// past it).
    pub backups: u64,
    pub more: bool,
    /// Whether it keeps a snapshot's id (Cloudflare deletes no snapshot:
    /// forgotten, it expires in 30 days, never restored).
    pub snapshot: bool,
}

/// What a wipe finds of a person and their agents: a dry run's is what a
/// wipe deletes; once wiped, what is left (all of it nothing).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub sign_ins: u64,
    pub keys: u64,
    pub sessions: u64,
    pub pictures: u64,
    pub agents: Vec<String>,
    /// The fragments they own (their list's rows they own), and the agent
    /// fragments the registry names.
    pub fragments: Listed,
    /// Their and their agents' memberships in other people's fragments,
    /// as `<fragment> (<role>, <identity>)`.
    pub memberships: Listed,
    pub computer: Option<ComputerFound>,
    /// Whether their ledger holds anything.
    pub ledger: bool,
    /// The rows their and their agents' lists hold (each a fragment's,
    /// with a role or the one they left).
    pub lists: u64,
}

impl Found {
    /// Nothing of theirs is left.
    pub fn is_empty(&self) -> bool {
        let computer = self.computer.as_ref().is_none_or(|c| c.phase.is_none() && c.saves == 0 && c.backups == 0 && !c.snapshot);
        self.sign_ins == 0
            && self.keys == 0
            && self.sessions == 0
            && self.pictures == 0
            && self.agents.is_empty()
            && self.fragments.count == 0
            && self.memberships.count == 0
            && computer
            && !self.ledger
            && self.lists == 0
    }
}

/// One step a wipe call ran, and what it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Ran {
    /// `fragment_core::wipe::Step`'s name.
    pub step: String,
    /// The step finished (else the next call goes on with it).
    pub done: bool,
    /// What it deleted, ended or left this call.
    pub deleted: u64,
    /// What it says (what it skipped and why, what is still to clean).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The `cleanup` step's: each fragment whose cleanup is not done yet,
    /// and what it has left (at most `fragment_core::wipe::NAMES_SHOWN_MAX`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleaning: Vec<Cleaning>,
}

/// A fragment of the wiped person's, ended, whose cleanup is not done: what
/// its ended lives have left (docs/api.md, Operators: `cleanup`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cleaning {
    pub fragment: String,
    /// Members' lists still to tell.
    pub lists: u64,
    /// Its app's database or blobs remain.
    pub stored: bool,
    /// Its repo is still to delete.
    pub repo: bool,
    /// The most failed tries of a part left.
    pub tries: u64,
    /// A part failed past its tries, or was refused: held, tried again
    /// daily, and by each wipe call.
    pub held: bool,
    /// The last error of a part left, when one failed (`part: error`,
    /// bounded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A wipe's report: a dry run's (`GET`) or a call's (`POST`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WipeReport {
    pub identity: String,
    /// Their email, while the registry holds them (their latest sign-in's).
    pub email: Option<String>,
    pub state: WipeState,
    /// The step a wipe runs next, while it is wiping.
    pub next: Option<String>,
    pub found: Found,
    /// The steps this call ran (a dry run's: none).
    pub ran: Vec<Ran>,
    /// Wiped, and nothing of theirs is left.
    pub done: bool,
}
