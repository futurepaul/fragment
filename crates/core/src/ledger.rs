//! A person's usage ledger (docs/ledger.md; docs/cloudflare-v1.md,
//! decisions 24–27 and 36): credit, plans, reservations, meters, caps,
//! and the standing that says what may still spend.
//!
//! One ledger per payer. The caller decides who pays (an agent's model
//! and compute bill the agent's owner; a fragment's hosting bills the
//! fragment's owner) and sends each row to that payer's ledger, which
//! keys on nothing else. Money is integer micro-dollars, months are UTC,
//! and time is an argument, never read here: the ledger is a pure state
//! machine that the cell's Ledger Durable Object runs and the tests
//! simulate.
//!
//! The state has two parts. `Ledger` is the head: small and bounded (the
//! plan, the balance's parts, the reservations held, the price book),
//! kept whole. A `Store` holds what grows with use, by id: each reference
//! charged, each batch and command applied, each fragment's spend by
//! month, and caps. The Durable Object keeps those in SQLite tables, and
//! runs each mutation in one transaction with the head; tests keep them
//! in memory (`Memory`). A mutation decides before it writes, so a
//! refusal writes nothing.
//!
//! Every mutation is idempotent by its own id (a command's, a reference,
//! a batch's): a replay changes nothing and answers as the first did, and
//! the same id with another body is refused. A refusal is not remembered,
//! so the same id may be tried again (a step after a top-up). Every
//! mutation first brings the ledger to its time's month.
//!
//! Credit (decision 25): a seat's included credit arrives each UTC month
//! and expires at its end; purchased credit stays until spent. A charge
//! draws included credit first. Included credit that arrives while the
//! person owes pays the debt first, so a debt is paid once.
//!
//! Zero (decision 27): at a balance of zero or less, agents stop (no
//! turns, no wakes, no AI steps). Fragments keep taking writes down to the
//! overdraft; at it they go read-only, and stay read-only until the
//! balance is above zero again or an operator changes the overdraft.
//! Meters always record: what happened is charged, even past zero.
//!
//! Guests (decision 25; Paul, 2026-10-03): a guest pays for nothing, so a
//! guest makes no fragment either, whose hosting would bill them. They
//! edit the fragments shared with them, which bill those fragments'
//! owners.

use std::collections::BTreeMap;

use fragment_proto::ledger::{FragmentSpend, GrantCredit, LedgerStatus, Plan, SeatState, SetFragmentCap, SetOverdraft, SetPlan, SetSeat, Standing, Why};
use fragment_proto::ErrorCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::price::{self, dollars, BookFault, PriceBook, PriceError, Priced, Usage, UsageFault, USD};

#[cfg(test)]
mod sim;
#[cfg(test)]
mod tests;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// A seat's included credit each month, and the always-on seat's
/// (decision 25).
pub const SEAT_INCLUDED: i64 = 50 * USD;
pub const SEAT_ALWAYS_ON_INCLUDED: i64 = 100 * USD;
/// How far below zero a person's fragments keep taking writes, until an
/// operator sets another (decision 27).
pub const OVERDRAFT_DEFAULT: i64 = 2 * USD;
/// A fragment's monthly cap until its owner sets one (decision 26).
pub const CAP_DEFAULT: i64 = 5 * USD;

/// Rows in one batch: a batch is one request and one transaction, and
/// 1,000 rows of about 300 bytes stay far under the request body limit.
pub const BATCH_ROWS_MAX: usize = 1_000;
/// One reservation: the dearest call the tiers allow is about $10 (Opus
/// with a million tokens in and 128K out, with the fee and margin), so
/// $25 leaves room for media steps and still refuses a typo.
pub const RESERVATION_MAX: i64 = 25 * USD;
/// Reservations held at once: a computer's agents and a person's jobs in
/// flight together hold a few dozen; past this, something leaks them.
pub const HOLDS_MAX: usize = 256;
/// One grant: a purchase or an operator's gift; more is a typo.
pub const GRANT_MAX: i64 = 10_000 * USD;
/// A fragment's cap: more is no cap at all, and a typo.
pub const CAP_MAX: i64 = 10_000 * USD;
/// An overdraft an operator may set: someone trusted with more debt than
/// this is someone to invoice.
pub const OVERDRAFT_MAX: i64 = 1_000 * USD;
/// An id (a reference, a batch, a command, who granted) or a name (a
/// fragment, an agent, a computer): printable ASCII, at most this long.
/// Sources prefix their references (`aig:<log id>`, `step:<fragment>@…`)
/// so two sources never collide.
pub const ID_MAX_BYTES: usize = 256;
/// A grant's note for people.
pub const NOTE_MAX_BYTES: usize = 512;
/// A reservation older than this is settled at its worst case by the next
/// sweep. No call holds for hours (a long model answer takes minutes), so its
/// caller died, and whether the call used anything is unknown: the money
/// path fails closed, as it does for a call that reports no usage.
pub const HOLD_MAX_MS: i64 = 6 * HOUR_MS;
/// How long references and batches are remembered (the audit records'
/// retention). A row from before this is refused as stale: its reference
/// may have been forgotten, so it could be a replay. Sources retry within
/// minutes or hours.
pub const REF_KEEP_MS: i64 = 90 * DAY_MS;
/// How far ahead of the ledger's clock a row may say it happened: its
/// source is another Durable Object, on a clock of its own.
pub const CLOCK_SKEW_MS: i64 = 5 * 60_000;
/// The sweep runs at least this often, to forget old references.
pub const SWEEP_EVERY_MS: i64 = DAY_MS;
/// Fragments a status lists, the largest spend first.
pub const STATUS_FRAGMENTS: usize = 50;

const _: () = assert!(HOLD_MAX_MS < REF_KEEP_MS, "a hold expires long before its reference could be forgotten");
const _: () = assert!(RESERVATION_MAX <= price::CHARGE_MAX, "a reservation is a charge it could become");
const _: () = assert!(SEAT_INCLUDED <= SEAT_ALWAYS_ON_INCLUDED, "the larger seat includes more");
const _: () = assert!(OVERDRAFT_DEFAULT <= OVERDRAFT_MAX && CAP_DEFAULT <= CAP_MAX, "the defaults are within their limits");
const _: () = assert!((HOLDS_MAX as i64) * RESERVATION_MAX < i64::MAX / 1024, "what is held sums in an i64");

/// A UTC month, counted from January 1970 (month 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Month(pub u32);

impl Month {
    pub fn of(ms: i64) -> Month {
        assert!(ms >= 0, "the ledger's times are after 1970");
        let (year, month, _) = crate::cron::civil(ms.div_euclid(DAY_MS));
        let index = (year - 1970) * 12 + i64::from(month) - 1;
        Month(u32::try_from(index).expect("a month since 1970 fits in 32 bits"))
    }

    /// `YYYY-MM`, for people.
    pub fn label(self) -> String {
        format!("{:04}-{:02}", 1970 + self.0 / 12, self.0 % 12 + 1)
    }

    /// The month `label` (`YYYY-MM`, `label`'s inverse) names.
    pub fn parse(label: &str) -> Option<Month> {
        let (y, m) = label.split_once('-')?;
        if y.len() != 4 || m.len() != 2 || !y.bytes().chain(m.bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (y, m): (u32, u32) = (y.parse().ok()?, m.parse().ok()?);
        ((1970..=9999).contains(&y) && (1..=12).contains(&m)).then(|| Month((y - 1970) * 12 + m - 1))
    }
}

/// What a spend is, for the questions the platform asks before it lets
/// one happen (`Ledger::gate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Spend {
    /// An agent's turn: its model calls and operator-key calls (through
    /// the computer's intercept), or the in-fragment agent's.
    AgentTurn,
    /// A fragment's AI step (a job's `ai.*`, voice input).
    AiStep,
    /// A computer's wake (its awake time is metered after).
    Wake,
    /// A write to a fragment, whose hosting its owner pays (metered after).
    Write,
    /// Making a fragment, whose hosting its maker will pay: a write to a
    /// fragment of their own, and a guest makes none.
    Create,
}

/// A paid call's worst case, held before the call is made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reserve {
    /// The call's reference, stable across its retries (a job step's
    /// `step:<fragment>@<incarnation>/run/<run>/step/<index>`, or the
    /// intercept's id for a model call).
    #[serde(rename = "ref")]
    pub reference: String,
    /// `AgentTurn` or `AiStep`: wakes, writes and creates reserve nothing.
    pub spend: Spend,
    /// The call's worst case, priced with the ledger's book.
    pub worst: Usage,
    /// The fragment the call is in: its month's spend counts it.
    pub fragment: Option<String>,
    /// The agent making the call, for the record.
    pub agent: Option<String>,
    /// The fragment's cap applies: this payer owns `fragment`, and the
    /// spender is not its owner (nor its owner's agent acting for them).
    pub capped: bool,
}

/// A held call's end: what it used (`None`: it reported nothing, and is
/// charged its reservation, failing closed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settle {
    #[serde(rename = "ref")]
    pub reference: String,
    pub usage: Option<Usage>,
}

/// A held call that failed for good before it used anything (bug 3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Release {
    #[serde(rename = "ref")]
    pub reference: String,
}

/// Rows a source metered since its last flush, under an id of the batch's
/// own: a batch retried after a crash between meter and flush is applied
/// once, and each row is charged once by its reference whatever batch
/// carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meter {
    pub batch: String,
    pub rows: Vec<MeterRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeterRow {
    /// The usage's reference (the gateway's log id, a run id, a computer's
    /// awake interval, a storage sample): the ledger's replay key.
    #[serde(rename = "ref")]
    pub reference: String,
    pub usage: Usage,
    pub fragment: Option<String>,
    pub agent: Option<String>,
    pub computer: Option<String>,
    /// When it happened (a sample's time), Unix ms.
    pub at_ms: i64,
}

/// A new price book from the deploy's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetPriceBook {
    pub id: String,
    pub book: PriceBook,
}

/// Every command, as its record keeps it (to tell a replay from another
/// body under the same id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Grant(GrantCredit),
    SetPlan(SetPlan),
    SetSeat(SetSeat),
    SetOverdraft(SetOverdraft),
    SetCap(SetFragmentCap),
    SetBook(SetPriceBook),
}

/// A reservation's answer, from where its reference stands: a replay
/// changes nothing, and a retried step learns that its call was paid
/// (bug 2), so it never makes the call again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reserved", rename_all = "snake_case")]
pub enum Reserved {
    /// Held: make the call, then settle it, or release it if it fails for
    /// good having used nothing.
    Held { amount: i64 },
    /// Settled before: the call was made and charged.
    Settled { charge: i64 },
    /// Released before: the call failed for good.
    Released,
}

/// What a settled call was charged, and from what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settled {
    pub charge: i64,
    pub basis: Basis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Basis {
    /// The usage it reported, priced (even past its reservation: it
    /// happened).
    Usage,
    /// Its reservation: it reported no usage, or one the book has no price
    /// for.
    Reservation,
    /// Its reservation: it was held past `HOLD_MAX_MS`.
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "released", rename_all = "snake_case")]
pub enum Released {
    /// The reservation went back (now, or by this release's earlier try).
    Back,
    /// It had settled: its charge stands.
    Settled { charge: i64 },
}

/// A batch's answer, kept with the batch so a replay answers the same.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Metered {
    /// What the batch's new rows charged, together.
    pub charged: i64,
    pub rows_new: u32,
    /// Rows an earlier batch charged with the same body.
    pub rows_before: u32,
    /// Rows refused, by their index: they changed nothing, and a later
    /// batch may carry them again (once the book prices them, say).
    pub refused: Vec<RowRefused>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowRefused {
    pub index: u32,
    pub why: Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Swept {
    /// Holds past `HOLD_MAX_MS`, settled at their reservations.
    pub expired: Vec<Expired>,
    /// References and batches forgotten (past `REF_KEEP_MS`).
    pub forgotten: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expired {
    #[serde(rename = "ref")]
    pub reference: String,
    pub charge: i64,
}

/// Why a mutation or a question was refused. A refusal changes nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "refused", rename_all = "snake_case")]
pub enum Refused {
    /// The request breaks a rule of its shape.
    Invalid { what: Invalid },
    /// This id was used with another body.
    ConflictingBody,
    /// No reservation by this reference (never made, or forgotten).
    UnknownRef,
    /// A settle for a reservation released before: a call that used
    /// anything settles; only one that used nothing is released.
    Ended,
    /// `HOLDS_MAX` reservations are held.
    TooManyHolds,
    /// A price book no newer than the ledger's.
    StaleBook { current: u32 },
    /// The book prices no such model, instance type or key.
    NoPrice,
    /// More than one reservation (`RESERVATION_MAX`) or one row
    /// (`price::CHARGE_MAX`) may cost.
    TooLarge,
    /// A row from before what the ledger remembers (`REF_KEEP_MS`).
    Stale,
    /// A guest pays for nothing (decisions 25 and 36).
    GuestPayer,
    /// A guest makes no fragment: its hosting would bill them, and a guest
    /// pays for nothing (Paul, 2026-10-03).
    GuestCreates,
    AgentsStopped { why: Why },
    ReadOnly { why: Why },
    /// The worst case does not fit what is available (the balance less
    /// what is held).
    CreditShort { available: i64, needed: i64 },
    /// The fragment spent its cap this month (counting what is held in
    /// it): only its owner spends in it until the next month.
    CapReached { cap: i64, spent: i64 },
}

/// The rule a request broke.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Invalid {
    /// An id or a name that is empty, too long, or not printable ASCII.
    Id,
    Usage(UsageFault),
    Book(BookFault),
    /// An amount outside its limit (a grant, a cap, an overdraft).
    Amount,
    /// A note longer than `NOTE_MAX_BYTES`.
    Note,
    /// A batch with no rows, or more than `BATCH_ROWS_MAX`.
    Rows,
    /// A wake, a write or a create, which reserve nothing.
    Spend,
    /// A capped reservation that names no fragment.
    Capped,
    /// A row from before 1970, or past `CLOCK_SKEW_MS` ahead.
    Time,
    /// A seat change at order 0 (orders start at 1).
    Order,
}

impl From<PriceError> for Refused {
    fn from(e: PriceError) -> Refused {
        match e {
            PriceError::Invalid(fault) => Refused::Invalid { what: Invalid::Usage(fault) },
            PriceError::NoPrice => Refused::NoPrice,
            PriceError::TooLarge => Refused::TooLarge,
        }
    }
}

fn why_text(why: Why) -> &'static str {
    match why {
        Why::Guest => "a guest has no agents",
        Why::SeatCanceled => "the seat was canceled",
        Why::NoCredit => "the credit is used up; adding credit starts them again",
        Why::Overdrawn => "the balance reached the overdraft; credit that brings it above zero ends it",
    }
}

impl Refused {
    /// The platform's error code for the refusal: every money refusal is
    /// `budget_used_up` (402), with the reason in the message.
    pub fn code(self) -> ErrorCode {
        match self {
            Refused::Invalid { .. } | Refused::Ended | Refused::StaleBook { .. } | Refused::NoPrice | Refused::Stale => ErrorCode::InvalidRequest,
            Refused::ConflictingBody => ErrorCode::ConflictingBody,
            Refused::UnknownRef => ErrorCode::NotFound,
            Refused::TooManyHolds => ErrorCode::RateLimited,
            Refused::TooLarge => ErrorCode::TooLarge,
            // what a guest's plan does not allow, as a role does not: no
            // amount of credit is short
            Refused::GuestPayer | Refused::GuestCreates => ErrorCode::Forbidden,
            Refused::AgentsStopped { .. } | Refused::ReadOnly { .. } | Refused::CreditShort { .. } | Refused::CapReached { .. } => ErrorCode::BudgetUsedUp,
        }
    }

    /// The refusal for people (the shell shows it).
    pub fn message(self) -> String {
        match self {
            Refused::Invalid { what } => format!("the request breaks a rule ({what:?})"),
            Refused::ConflictingBody => "this id was used with a different body".into(),
            Refused::UnknownRef => "no reservation by that reference".into(),
            Refused::Ended => "that reservation was released; only a call that used nothing is released".into(),
            Refused::TooManyHolds => format!("{HOLDS_MAX} reservations are held already; try again as they end"),
            Refused::StaleBook { current } => format!("the ledger charges with price book {current}; a new book has a higher version"),
            Refused::NoPrice => "the price book has no price for that".into(),
            Refused::TooLarge => "that is more than one call or row may cost".into(),
            Refused::Stale => "that row is older than the ledger remembers".into(),
            Refused::GuestPayer => "a guest pays for nothing: no agents, no AI, nothing billed".into(),
            Refused::GuestCreates => {
                "guests can't create fragments: a fragment's hosting bills its owner, and a guest pays for nothing; \
                 a guest still edits the fragments shared with them, and a seat makes fragments of its own"
                    .into()
            }
            Refused::AgentsStopped { why } => format!("agents are stopped: {}", why_text(why)),
            Refused::ReadOnly { why } => format!("fragments are read-only: {}", why_text(why)),
            Refused::CreditShort { available, needed } => {
                format!("not enough credit: this needs up to {}, and {} is available", dollars(needed), dollars(available.max(0)))
            }
            Refused::CapReached { cap, spent } => {
                format!("this fragment spent {} of its {} cap this month: only its owner spends in it until next month", dollars(spent), dollars(cap))
            }
        }
    }
}

fn invalid(what: Invalid) -> Refused {
    Refused::Invalid { what }
}

fn valid_id(s: &str) -> bool {
    price::printable(s, ID_MAX_BYTES)
}

fn valid_names(names: [&Option<String>; 3]) -> bool {
    names.iter().all(|n| n.as_deref().is_none_or(valid_id))
}

/// A reference's record in the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "entry", rename_all = "snake_case")]
pub enum Entry {
    /// A reservation, and its end once it has one.
    Reservation { reserve: Reserve, amount: i64, at_ms: i64, end: Option<End> },
    /// A meter row: its price under the book of the time, and its charge
    /// (the price's, or nothing for a waived meter).
    Row { row: MeterRow, priced: Priced, charge: i64, book: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "end", rename_all = "snake_case")]
pub enum End {
    /// `priced` is the usage's price when it had one: its list price is
    /// what the gateway's log reports, for reconciliation.
    Settled { usage: Option<Usage>, priced: Option<Priced>, charge: i64, basis: Basis },
    Released,
}

impl Entry {
    /// When it happened: `Store::prune` forgets by this.
    pub fn at_ms(&self) -> i64 {
        match self {
            Entry::Reservation { at_ms, .. } => *at_ms,
            Entry::Row { row, .. } => row.at_ms,
        }
    }

    /// What it charged, once it has.
    pub fn charge(&self) -> Option<i64> {
        match self {
            Entry::Reservation { end: Some(End::Settled { charge, .. }), .. } | Entry::Row { charge, .. } => Some(*charge),
            Entry::Reservation { .. } => None,
        }
    }

    /// A reservation still held.
    pub fn held(&self) -> bool {
        matches!(self, Entry::Reservation { end: None, .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchRecord {
    /// SHA-256 of the batch's rows, to tell a replay from another body.
    pub digest: String,
    pub answer: Metered,
    /// The latest of its time and its rows': the record outlives its
    /// rows' references, so a replay answers from it while any is kept.
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRecord {
    pub command: Command,
    pub at_ms: i64,
}

/// What grows with use, by id. The Durable Object keeps each in a SQLite
/// table and runs a mutation and the head's write in one transaction;
/// `Memory` keeps them in maps. Commands are never forgotten (a grant's id
/// is Stripe's, and may come again any time; they are few); references
/// and batches are forgotten after `REF_KEEP_MS`.
pub trait Store {
    fn entry(&self, reference: &str) -> Option<Entry>;
    fn put_entry(&mut self, reference: &str, entry: Entry);
    fn batch(&self, id: &str) -> Option<BatchRecord>;
    fn put_batch(&mut self, id: &str, record: BatchRecord);
    fn command(&self, id: &str) -> Option<CommandRecord>;
    fn put_command(&mut self, id: &str, record: CommandRecord);
    /// A fragment's charges in a month.
    fn spent(&self, month: Month, fragment: &str) -> i64;
    fn add_spent(&mut self, month: Month, fragment: &str, micros: i64);
    /// A month's spend by fragment, the largest first, at most `limit`.
    fn spends(&self, month: Month, limit: usize) -> Vec<(String, i64)>;
    fn cap(&self, fragment: &str) -> Option<i64>;
    fn set_cap(&mut self, fragment: &str, micros: Option<i64>);
    /// Forgets the entries and batches from before `before_ms` (never a
    /// held reservation: the sweep expired those first) and the months of
    /// spend before `before_month`; answers how many entries and batches
    /// it forgot.
    fn prune(&mut self, before_ms: i64, before_month: Month) -> u64;
}

/// A store in memory, for tests and the simulation.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Memory {
    entries: BTreeMap<String, Entry>,
    batches: BTreeMap<String, BatchRecord>,
    commands: BTreeMap<String, CommandRecord>,
    spent: BTreeMap<Month, BTreeMap<String, i64>>,
    caps: BTreeMap<String, i64>,
}

impl Store for Memory {
    fn entry(&self, reference: &str) -> Option<Entry> {
        self.entries.get(reference).cloned()
    }

    fn put_entry(&mut self, reference: &str, entry: Entry) {
        self.entries.insert(reference.to_string(), entry);
    }

    fn batch(&self, id: &str) -> Option<BatchRecord> {
        self.batches.get(id).cloned()
    }

    fn put_batch(&mut self, id: &str, record: BatchRecord) {
        self.batches.insert(id.to_string(), record);
    }

    fn command(&self, id: &str) -> Option<CommandRecord> {
        self.commands.get(id).cloned()
    }

    fn put_command(&mut self, id: &str, record: CommandRecord) {
        self.commands.insert(id.to_string(), record);
    }

    fn spent(&self, month: Month, fragment: &str) -> i64 {
        self.spent.get(&month).and_then(|m| m.get(fragment)).copied().unwrap_or(0)
    }

    fn add_spent(&mut self, month: Month, fragment: &str, micros: i64) {
        *self.spent.entry(month).or_default().entry(fragment.to_string()).or_insert(0) += micros;
    }

    fn spends(&self, month: Month, limit: usize) -> Vec<(String, i64)> {
        let mut all: Vec<(String, i64)> = self.spent.get(&month).map(|m| m.iter().map(|(f, s)| (f.clone(), *s)).collect()).unwrap_or_default();
        all.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        all.truncate(limit);
        all
    }

    fn cap(&self, fragment: &str) -> Option<i64> {
        self.caps.get(fragment).copied()
    }

    fn set_cap(&mut self, fragment: &str, micros: Option<i64>) {
        match micros {
            Some(m) => self.caps.insert(fragment.to_string(), m),
            None => self.caps.remove(fragment),
        };
    }

    fn prune(&mut self, before_ms: i64, before_month: Month) -> u64 {
        let (entries, batches) = (self.entries.len(), self.batches.len());
        self.entries.retain(|_, e| {
            let keep = e.at_ms() >= before_ms;
            assert!(keep || !e.held(), "a held reservation is expired before it could be forgotten");
            keep
        });
        self.batches.retain(|_, b| b.at_ms >= before_ms);
        self.spent.retain(|m, _| *m >= before_month);
        ((entries - self.entries.len()) + (batches - self.batches.len())) as u64
    }
}

/// What a seat includes each month: nothing unless it is active (a seat
/// past due keeps this month's credit, and gets next month's when it is
/// active again).
pub fn included(plan: Plan, seat: SeatState) -> i64 {
    match (plan, seat) {
        (Plan::Seat, SeatState::Active) => SEAT_INCLUDED,
        (Plan::SeatAlwaysOn, SeatState::Active) => SEAT_ALWAYS_ON_INCLUDED,
        _ => 0,
    }
}

/// A person's standing, from their plan, seat, balance, and whether the
/// overdraft made their fragments read-only (decision 27).
pub fn standing_of(plan: Plan, seat: SeatState, balance: i64, read_only: bool) -> Standing {
    if read_only {
        return Standing::ReadOnly { why: Why::Overdrawn };
    }
    match (plan, seat) {
        (Plan::Guest, _) => Standing::AgentsStopped { why: Why::Guest },
        (_, SeatState::Canceled) => Standing::AgentsStopped { why: Why::SeatCanceled },
        _ if balance > 0 => Standing::Ok,
        _ => Standing::AgentsStopped { why: Why::NoCredit },
    }
}

/// Everything that ever moved the balance: the balance is always
/// `purchased + included - expired - charged`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Totals {
    pub purchased: i64,
    pub included: i64,
    pub expired: i64,
    pub charged: i64,
}

/// The balance's parts and what decides them. Small and `Copy`: a read
/// rolls a copy to its own month, so it sees that month's credit before
/// any mutation has rolled the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Money {
    plan: Plan,
    seat: SeatState,
    /// The last seat change's order (`SetSeat::seq`).
    seat_seq: u64,
    overdraft: i64,
    month: Month,
    /// This month's included credit as granted, and what is left of it.
    included_granted: i64,
    included_left: i64,
    /// Purchased credit left; below zero while the person owes.
    purchased_left: i64,
    /// Set at the overdraft, cleared above zero.
    read_only: bool,
    totals: Totals,
}

impl Money {
    fn balance(&self) -> i64 {
        self.included_left + self.purchased_left
    }

    fn standing(&self) -> Standing {
        standing_of(self.plan, self.seat, self.balance(), self.read_only)
    }

    /// Brings the money to `month`. Months only move forward: a late time
    /// rolls nothing back. Skipped months grant nothing (they are over).
    fn roll(&mut self, month: Month) {
        if month <= self.month {
            return;
        }
        // the old month's included credit expires: it never carries over
        self.totals.expired += self.included_left;
        self.included_left = 0;
        self.included_granted = 0;
        self.month = month;
        self.entitle();
        self.latch();
    }

    /// Grants what is missing of this month's included credit for the plan
    /// and seat now: a seat that becomes active, or a plan that grows,
    /// mid-month gets the difference, once; nothing is taken back when one
    /// shrinks (it expires at the month's end anyway). A debt is paid from
    /// it first.
    fn entitle(&mut self) {
        let target = included(self.plan, self.seat);
        if target <= self.included_granted {
            return;
        }
        let grant = target - self.included_granted;
        let pay = grant.min((-self.purchased_left).max(0));
        self.included_granted = target;
        self.totals.included += grant;
        self.purchased_left += pay;
        self.included_left += grant - pay;
    }

    /// A charge, drawn from included credit first.
    fn draw(&mut self, amount: i64) {
        assert!((0..=price::CHARGE_MAX).contains(&amount), "a charge was priced within CHARGE_MAX");
        let from_included = amount.min(self.included_left);
        self.included_left -= from_included;
        self.purchased_left -= amount - from_included;
        self.totals.charged += amount;
    }

    /// Read-only at the overdraft, writable again above zero.
    fn latch(&mut self) {
        let balance = self.balance();
        if balance <= -self.overdraft {
            self.read_only = true;
        } else if balance > 0 {
            self.read_only = false;
        }
    }

    fn assert_valid(&self) {
        assert!(self.included_left >= 0, "included credit is never overspent");
        assert!(self.included_left <= self.included_granted, "included credit never carries past its month");
        assert!(self.included_granted <= SEAT_ALWAYS_ON_INCLUDED, "a month includes at most the largest seat's credit");
        assert!(self.purchased_left >= 0 || self.included_left == 0, "a debt is paid from included credit as it arrives");
        assert!((0..=OVERDRAFT_MAX).contains(&self.overdraft), "an overdraft is within its limit");
        let balance = self.balance();
        assert!(!self.read_only || balance <= 0, "read-only ends above zero");
        assert!(self.read_only || balance > -self.overdraft, "the overdraft makes fragments read-only");
        let t = self.totals;
        assert_eq!(balance, t.purchased + t.included - t.expired - t.charged, "the balance is what moved it");
    }
}

/// Whether `spend` may start on these means. Making a fragment is a write
/// to one of the payer's own: it goes as a write does, but a guest, whose
/// fragments would be billed nothing, makes none.
fn gate(money: &Money, spend: Spend) -> Result<(), Refused> {
    if money.plan == Plan::Guest {
        return Err(match spend {
            Spend::Create => Refused::GuestCreates,
            Spend::AgentTurn | Spend::AiStep | Spend::Wake | Spend::Write => Refused::GuestPayer,
        });
    }
    match (spend, money.standing()) {
        (_, Standing::ReadOnly { why }) => Err(Refused::ReadOnly { why }),
        (Spend::Write | Spend::Create, Standing::AgentsStopped { .. } | Standing::Ok) => Ok(()),
        (_, Standing::AgentsStopped { why }) => Err(Refused::AgentsStopped { why }),
        (_, Standing::Ok) => Ok(()),
    }
}

/// A held reservation, by its reference in `Ledger::holds`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Hold {
    amount: i64,
    fragment: Option<String>,
    at_ms: i64,
}

/// A person's ledger: the head of its state (see the module's docs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ledger {
    money: Money,
    book: PriceBook,
    /// At most `HOLDS_MAX`, so the head stays small.
    holds: BTreeMap<String, Hold>,
}

impl Ledger {
    /// A new person's ledger: a guest with no credit, charging with `book`
    /// (the deploy's configured book; passed, never assumed).
    pub fn new(now_ms: i64, book: PriceBook) -> Ledger {
        assert_eq!(book.validate(), Ok(()), "a ledger starts on a valid book");
        let money = Money {
            plan: Plan::Guest,
            seat: SeatState::Active,
            seat_seq: 0,
            overdraft: OVERDRAFT_DEFAULT,
            month: Month::of(now_ms),
            included_granted: 0,
            included_left: 0,
            purchased_left: 0,
            read_only: false,
            totals: Totals::default(),
        };
        let ledger = Ledger { money, book, holds: BTreeMap::new() };
        ledger.assert_valid();
        ledger
    }

    // Reads. Each sees the ledger as of `now_ms`, changing nothing.

    fn money_at(&self, now_ms: i64) -> Money {
        let mut money = self.money;
        money.roll(Month::of(now_ms));
        money
    }

    pub fn standing(&self, now_ms: i64) -> Standing {
        self.money_at(now_ms).standing()
    }

    /// Whether this payer's money lets `spend` start (decisions 25 and
    /// 27). A spend in a fragment someone else owns also asks that owner's
    /// ledger `fragment_open`.
    pub fn gate(&self, spend: Spend, now_ms: i64) -> Result<(), Refused> {
        gate(&self.money_at(now_ms), spend)
    }

    /// Whether `fragment`, which this ledger's person owns, is still open
    /// this month to spenders other than its owner (decision 26): its
    /// charges and what is held in it are under its cap.
    pub fn fragment_open(&self, store: &impl Store, fragment: &str, now_ms: i64) -> Result<(), Refused> {
        if !valid_id(fragment) {
            return Err(invalid(Invalid::Id));
        }
        let month = self.money_at(now_ms).month;
        let spent = store.spent(month, fragment) + self.held_in(fragment);
        let cap = store.cap(fragment).unwrap_or(CAP_DEFAULT);
        if spent < cap {
            Ok(())
        } else {
            Err(Refused::CapReached { cap, spent })
        }
    }

    /// May this spend start: `spend` by this payer, in `fragment` (when
    /// it is in one), `by_owner` when the spender is the fragment's owner
    /// or their agent acting for them. When this payer owns the fragment
    /// (or there is none), this is the whole answer; when another person
    /// owns it, that owner's ledger answers `fragment_open` too (an agent
    /// working for Skyler in Paul's fragment: Skyler pays, Paul's cap
    /// applies; decisions 26 and 36). Caps stop AI steps and agent turns,
    /// never writes or wakes.
    pub fn may_spend(&self, store: &impl Store, spend: Spend, fragment: Option<&str>, by_owner: bool, now_ms: i64) -> Result<(), Refused> {
        self.gate(spend, now_ms)?;
        match (spend, fragment, by_owner) {
            (Spend::AgentTurn | Spend::AiStep, Some(f), false) => self.fragment_open(store, f, now_ms),
            _ => Ok(()),
        }
    }

    pub fn status(&self, store: &impl Store, now_ms: i64) -> LedgerStatus {
        let money = self.money_at(now_ms);
        let reserved = self.reserved();
        let fragments = store
            .spends(money.month, STATUS_FRAGMENTS)
            .into_iter()
            .map(|(fragment, spent)| {
                let cap = store.cap(&fragment).unwrap_or(CAP_DEFAULT);
                FragmentSpend { fragment, spent_micros: spent, cap_micros: cap }
            })
            .collect();
        LedgerStatus {
            plan: money.plan,
            seat: money.seat,
            month: money.month.label(),
            balance_micros: money.balance(),
            included_micros: money.included_left,
            included_granted_micros: money.included_granted,
            purchased_micros: money.purchased_left,
            reserved_micros: reserved,
            available_micros: money.balance() - reserved,
            overdraft_micros: money.overdraft,
            standing: money.standing(),
            price_book: self.book.version,
            fragments,
        }
    }

    pub fn totals(&self) -> Totals {
        self.money.totals
    }

    pub fn book(&self) -> &PriceBook {
        &self.book
    }

    /// What every hold holds together.
    pub fn reserved(&self) -> i64 {
        self.holds.values().map(|h| h.amount).sum()
    }

    /// When the Durable Object's alarm should run `sweep` next: when the
    /// oldest hold expires, and at least once a `SWEEP_EVERY_MS`.
    pub fn next_sweep_ms(&self, now_ms: i64) -> i64 {
        let oldest = self.holds.values().map(|h| h.at_ms + HOLD_MAX_MS).min();
        oldest.map_or(now_ms + SWEEP_EVERY_MS, |at| at.min(now_ms + SWEEP_EVERY_MS))
    }

    fn held_in(&self, fragment: &str) -> i64 {
        self.holds.values().filter(|h| h.fragment.as_deref() == Some(fragment)).map(|h| h.amount).sum()
    }

    fn roll(&mut self, now_ms: i64) {
        self.money.roll(Month::of(now_ms));
    }

    /// A usage's price, and its charge here: a $200 seat's awake time is
    /// not metered (decision 25; one computer per person, decision 13).
    fn charge_for(&self, usage: &Usage) -> Result<(Priced, i64), PriceError> {
        let priced = self.book.price(usage)?;
        let waived = matches!(usage, Usage::Awake { .. }) && self.money.plan == Plan::SeatAlwaysOn;
        Ok((priced, if waived { 0 } else { priced.charge }))
    }

    /// A charge to the balance, counted in its fragment's month.
    fn charge(&mut self, store: &mut impl Store, amount: i64, fragment: Option<&str>, month: Month) {
        self.money.draw(amount);
        if let (Some(f), true) = (fragment, amount > 0) {
            store.add_spent(month, f, amount);
        }
    }

    /// The head's invariants. Every mutation checks them as it ends; the
    /// Durable Object checks them again after reading the head back.
    pub fn assert_valid(&self) {
        self.money.assert_valid();
        assert!(self.holds.len() <= HOLDS_MAX, "at most HOLDS_MAX holds");
        for hold in self.holds.values() {
            assert!((0..=RESERVATION_MAX).contains(&hold.amount), "a hold is never negative, nor over RESERVATION_MAX");
        }
    }

    // Commands: each idempotent by its id.

    /// Whether command `id` is new (`true`) or a replay of the same
    /// command (`false`); the same id with another body is refused.
    fn admit(store: &impl Store, id: &str, command: &Command) -> Result<bool, Refused> {
        if !valid_id(id) {
            return Err(invalid(Invalid::Id));
        }
        match store.command(id) {
            None => Ok(true),
            Some(record) if record.command == *command => Ok(false),
            Some(_) => Err(Refused::ConflictingBody),
        }
    }

    fn done(&mut self, store: &mut impl Store, id: &str, command: Command, now_ms: i64) {
        self.money.latch();
        store.put_command(id, CommandRecord { command, at_ms: now_ms });
        self.assert_valid();
    }

    /// Purchased credit (Stripe's hook) or an operator's gift.
    pub fn grant(&mut self, store: &mut impl Store, g: &GrantCredit, now_ms: i64) -> Result<(), Refused> {
        self.roll(now_ms);
        let command = Command::Grant(g.clone());
        if !Self::admit(store, &g.id, &command)? {
            return Ok(());
        }
        if !(1..=GRANT_MAX).contains(&g.micros) {
            return Err(invalid(Invalid::Amount));
        }
        if !valid_id(&g.by) {
            return Err(invalid(Invalid::Id));
        }
        if g.why.len() > NOTE_MAX_BYTES {
            return Err(invalid(Invalid::Note));
        }
        self.money.purchased_left += g.micros;
        self.money.totals.purchased += g.micros;
        self.done(store, &g.id, command, now_ms);
        Ok(())
    }

    pub fn set_plan(&mut self, store: &mut impl Store, c: &SetPlan, now_ms: i64) -> Result<(), Refused> {
        self.roll(now_ms);
        let command = Command::SetPlan(c.clone());
        if !Self::admit(store, &c.id, &command)? {
            return Ok(());
        }
        self.money.plan = c.plan;
        self.money.entitle();
        self.done(store, &c.id, command, now_ms);
        Ok(())
    }

    /// A seat's state from its hook. A change no newer than the last one
    /// applied (by `seq`) is kept as applied and changes nothing, so a
    /// late `active` never undoes a `canceled`.
    pub fn set_seat(&mut self, store: &mut impl Store, c: &SetSeat, now_ms: i64) -> Result<(), Refused> {
        self.roll(now_ms);
        let command = Command::SetSeat(c.clone());
        if !Self::admit(store, &c.id, &command)? {
            return Ok(());
        }
        if c.seq == 0 {
            return Err(invalid(Invalid::Order));
        }
        if c.seq > self.money.seat_seq {
            self.money.seat = c.seat;
            self.money.seat_seq = c.seq;
            self.money.entitle();
        }
        self.done(store, &c.id, command, now_ms);
        Ok(())
    }

    /// An operator's overdraft for this person. It decides read-only
    /// afresh: an operator who raises it lets the fragments write again.
    pub fn set_overdraft(&mut self, store: &mut impl Store, c: &SetOverdraft, now_ms: i64) -> Result<(), Refused> {
        self.roll(now_ms);
        let command = Command::SetOverdraft(c.clone());
        if !Self::admit(store, &c.id, &command)? {
            return Ok(());
        }
        if !(0..=OVERDRAFT_MAX).contains(&c.micros) {
            return Err(invalid(Invalid::Amount));
        }
        self.money.overdraft = c.micros;
        self.money.read_only = self.money.balance() <= -c.micros;
        self.done(store, &c.id, command, now_ms);
        Ok(())
    }

    /// A fragment's cap, set by its owner on the owner's ledger.
    pub fn set_cap(&mut self, store: &mut impl Store, c: &SetFragmentCap, now_ms: i64) -> Result<(), Refused> {
        self.roll(now_ms);
        let command = Command::SetCap(c.clone());
        if !Self::admit(store, &c.id, &command)? {
            return Ok(());
        }
        if !valid_id(&c.fragment) {
            return Err(invalid(Invalid::Id));
        }
        if c.micros.is_some_and(|m| !(0..=CAP_MAX).contains(&m)) {
            return Err(invalid(Invalid::Amount));
        }
        store.set_cap(&c.fragment, c.micros);
        self.done(store, &c.id, command, now_ms);
        Ok(())
    }

    /// A newer price book. What was charged stays charged; held
    /// reservations keep the amounts they were priced at.
    pub fn set_book(&mut self, store: &mut impl Store, c: &SetPriceBook, now_ms: i64) -> Result<(), Refused> {
        self.roll(now_ms);
        let command = Command::SetBook(c.clone());
        if !Self::admit(store, &c.id, &command)? {
            return Ok(());
        }
        c.book.validate().map_err(|fault| invalid(Invalid::Book(fault)))?;
        if c.book.version <= self.book.version {
            return Err(Refused::StaleBook { current: self.book.version });
        }
        self.book = c.book.clone();
        self.done(store, &c.id, command, now_ms);
        Ok(())
    }

    // Paid calls: reserve before, then settle or release, each once by
    // the call's reference.

    pub fn reserve(&mut self, store: &mut impl Store, r: &Reserve, now_ms: i64) -> Result<Reserved, Refused> {
        self.roll(now_ms);
        if !valid_id(&r.reference) || !valid_names([&r.fragment, &r.agent, &None]) {
            return Err(invalid(Invalid::Id));
        }
        r.worst.validate().map_err(|fault| invalid(Invalid::Usage(fault)))?;
        if !matches!(r.spend, Spend::AgentTurn | Spend::AiStep) {
            return Err(invalid(Invalid::Spend));
        }
        if r.capped && r.fragment.is_none() {
            return Err(invalid(Invalid::Capped));
        }
        if let Some(entry) = store.entry(&r.reference) {
            return match entry {
                Entry::Reservation { reserve, amount, end, .. } if reserve == *r => Ok(match end {
                    None => Reserved::Held { amount },
                    Some(End::Settled { charge, .. }) => Reserved::Settled { charge },
                    Some(End::Released) => Reserved::Released,
                }),
                // another body under this reference, or a meter row's
                _ => Err(Refused::ConflictingBody),
            };
        }
        gate(&self.money, r.spend)?;
        let (_, amount) = self.charge_for(&r.worst)?;
        if amount > RESERVATION_MAX {
            return Err(Refused::TooLarge);
        }
        if self.holds.len() >= HOLDS_MAX {
            return Err(Refused::TooManyHolds);
        }
        let available = self.money.balance() - self.reserved();
        if amount > available {
            return Err(Refused::CreditShort { available, needed: amount });
        }
        if let (true, Some(fragment)) = (r.capped, r.fragment.as_deref()) {
            self.fragment_open(&*store, fragment, now_ms)?;
        }
        self.holds.insert(r.reference.clone(), Hold { amount, fragment: r.fragment.clone(), at_ms: now_ms });
        store.put_entry(&r.reference, Entry::Reservation { reserve: r.clone(), amount, at_ms: now_ms, end: None });
        self.assert_valid();
        assert!(self.money.balance() - self.reserved() >= available - amount, "a hold takes only its own amount");
        Ok(Reserved::Held { amount })
    }

    /// A held call's end, charged once: its usage priced (even past its
    /// reservation: it happened), or its reservation when it reported
    /// none.
    pub fn settle(&mut self, store: &mut impl Store, s: &Settle, now_ms: i64) -> Result<Settled, Refused> {
        self.roll(now_ms);
        if !valid_id(&s.reference) {
            return Err(invalid(Invalid::Id));
        }
        if let Some(usage) = &s.usage {
            usage.validate().map_err(|fault| invalid(Invalid::Usage(fault)))?;
        }
        let Some(entry) = store.entry(&s.reference) else { return Err(Refused::UnknownRef) };
        let Entry::Reservation { reserve, amount, at_ms, end } = entry else { return Err(Refused::ConflictingBody) };
        match end {
            None => {}
            // an expired hold's settlement stands, whatever this one says
            Some(End::Settled { charge, basis: Basis::Expired, .. }) => return Ok(Settled { charge, basis: Basis::Expired }),
            Some(End::Settled { usage, charge, basis, .. }) if usage == s.usage => return Ok(Settled { charge, basis }),
            Some(End::Settled { .. }) => return Err(Refused::ConflictingBody),
            Some(End::Released) => return Err(Refused::Ended),
        }
        let (priced, charge, basis) = match &s.usage {
            None => (None, amount, Basis::Reservation),
            Some(usage) => match self.charge_for(usage) {
                Ok((priced, charge)) => (Some(priced), charge, Basis::Usage),
                // the call was made, with a usage the book cannot price:
                // its worst case, failing closed
                Err(PriceError::NoPrice) => (None, amount, Basis::Reservation),
                Err(e) => return Err(e.into()),
            },
        };
        let hold = self.holds.remove(&s.reference).expect("a reservation that has not ended is held");
        assert_eq!(hold.amount, amount, "a hold holds its entry's amount");
        let month = self.money.month;
        self.charge(store, charge, reserve.fragment.as_deref(), month);
        let end = Some(End::Settled { usage: s.usage.clone(), priced, charge, basis });
        store.put_entry(&s.reference, Entry::Reservation { reserve, amount, at_ms, end });
        self.money.latch();
        self.assert_valid();
        Ok(Settled { charge, basis })
    }

    /// A held call that failed for good, having used nothing: its
    /// reservation goes back (bug 3).
    pub fn release(&mut self, store: &mut impl Store, r: &Release, now_ms: i64) -> Result<Released, Refused> {
        self.roll(now_ms);
        if !valid_id(&r.reference) {
            return Err(invalid(Invalid::Id));
        }
        let Some(entry) = store.entry(&r.reference) else { return Err(Refused::UnknownRef) };
        let Entry::Reservation { reserve, amount, at_ms, end } = entry else { return Err(Refused::ConflictingBody) };
        match end {
            None => {}
            Some(End::Released) => return Ok(Released::Back),
            Some(End::Settled { charge, .. }) => return Ok(Released::Settled { charge }),
        }
        let hold = self.holds.remove(&r.reference).expect("a reservation that has not ended is held");
        assert_eq!(hold.amount, amount, "a hold holds its entry's amount");
        store.put_entry(&r.reference, Entry::Reservation { reserve, amount, at_ms, end: Some(End::Released) });
        self.assert_valid();
        Ok(Released::Back)
    }

    // Meters: usage that already happened, charged once by reference.

    pub fn meter(&mut self, store: &mut impl Store, m: &Meter, now_ms: i64) -> Result<Metered, Refused> {
        self.roll(now_ms);
        if !valid_id(&m.batch) {
            return Err(invalid(Invalid::Id));
        }
        if m.rows.is_empty() || m.rows.len() > BATCH_ROWS_MAX {
            return Err(invalid(Invalid::Rows));
        }
        let digest = hex::encode(Sha256::digest(serde_json::to_vec(&m.rows).expect("meter rows serialize")));
        if let Some(record) = store.batch(&m.batch) {
            return if record.digest == digest { Ok(record.answer) } else { Err(Refused::ConflictingBody) };
        }
        if self.money.plan == Plan::Guest {
            return Err(Refused::GuestPayer);
        }
        let charged_before = self.money.totals.charged;
        let mut answer = Metered::default();
        let mut at_ms = now_ms;
        // bounded by BATCH_ROWS_MAX, checked above
        for (index, row) in m.rows.iter().enumerate() {
            match self.meter_row(store, row, now_ms) {
                Ok(Some(charge)) => {
                    answer.charged += charge;
                    answer.rows_new += 1;
                }
                Ok(None) => answer.rows_before += 1,
                Err(why) => answer.refused.push(RowRefused { index: index as u32, why }),
            }
            at_ms = at_ms.max(row.at_ms);
        }
        assert_eq!(self.money.totals.charged - charged_before, answer.charged, "a batch charges what its new rows did");
        assert_eq!(answer.rows_new as usize + answer.rows_before as usize + answer.refused.len(), m.rows.len(), "every row has an outcome");
        store.put_batch(&m.batch, BatchRecord { digest, answer: answer.clone(), at_ms });
        self.money.latch();
        self.assert_valid();
        Ok(answer)
    }

    /// One row: `Some(charge)` when charged now, `None` when an earlier
    /// batch charged it with the same body.
    fn meter_row(&mut self, store: &mut impl Store, row: &MeterRow, now_ms: i64) -> Result<Option<i64>, Refused> {
        if !valid_id(&row.reference) || !valid_names([&row.fragment, &row.agent, &row.computer]) {
            return Err(invalid(Invalid::Id));
        }
        row.usage.validate().map_err(|fault| invalid(Invalid::Usage(fault)))?;
        if row.at_ms < 0 || row.at_ms > now_ms + CLOCK_SKEW_MS {
            return Err(invalid(Invalid::Time));
        }
        if row.at_ms < now_ms - REF_KEEP_MS {
            return Err(Refused::Stale);
        }
        match store.entry(&row.reference) {
            None => {}
            Some(Entry::Row { row: first, .. }) if first == *row => return Ok(None),
            // another body under this reference, or a reservation's
            Some(_) => return Err(Refused::ConflictingBody),
        }
        let (priced, charge) = self.charge_for(&row.usage)?;
        // a row counts in its own month: a late one from last month does
        // not spend this month's cap
        self.charge(store, charge, row.fragment.as_deref(), Month::of(row.at_ms));
        store.put_entry(&row.reference, Entry::Row { row: row.clone(), priced, charge, book: self.book.version });
        Ok(Some(charge))
    }

    /// Expires holds past `HOLD_MAX_MS` (settled at their reservations)
    /// and forgets what is past `REF_KEEP_MS`. The Durable Object's alarm
    /// runs it at `next_sweep_ms`.
    pub fn sweep(&mut self, store: &mut impl Store, now_ms: i64) -> Swept {
        self.roll(now_ms);
        let deadline = now_ms - HOLD_MAX_MS;
        let stale: Vec<String> = self.holds.iter().filter(|(_, h)| h.at_ms <= deadline).map(|(r, _)| r.clone()).collect();
        let mut expired = Vec::with_capacity(stale.len());
        // at most HOLDS_MAX
        for reference in stale {
            let entry = store.entry(&reference);
            let Some(Entry::Reservation { reserve, amount, at_ms, end: None }) = entry else {
                panic!("held reservation {reference} has no held entry: {entry:?}");
            };
            self.holds.remove(&reference);
            let month = self.money.month;
            self.charge(store, amount, reserve.fragment.as_deref(), month);
            let end = Some(End::Settled { usage: None, priced: None, charge: amount, basis: Basis::Expired });
            store.put_entry(&reference, Entry::Reservation { reserve, amount, at_ms, end });
            expired.push(Expired { reference, charge: amount });
        }
        let horizon = now_ms - REF_KEEP_MS;
        let forgotten = if horizon > 0 { store.prune(horizon, Month::of(horizon)) } else { 0 };
        self.money.latch();
        self.assert_valid();
        assert!(self.holds.values().all(|h| h.at_ms > deadline), "no hold outlives HOLD_MAX_MS past a sweep");
        Swept { expired, forgotten }
    }
}
