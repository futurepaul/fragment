//! Deterministic simulation of the ledger (docs/cloudflare-v1.md,
//! Evaluation). Goal: the ledger's money stays right under what a real
//! deployment does to it. Method: a seeded generator of interleaved
//! operations from many callers (reservations and their ends, meter
//! batches, grants, plan and seat changes, overdrafts, caps, price books,
//! sweeps), with injected crashes (an effect lost before it is applied,
//! or its answer lost after, then retried later), duplicates, reordering,
//! month rollovers and restarts (the state serialized and read back).
//! After every step it checks the ledger against a model of its own:
//! - the balance is what moved it: purchased grants (each once) plus
//!   included credit, less expired credit, less every charge, and every
//!   charge passed through the store;
//! - no reference is charged twice, and a charge never changes;
//! - holds are never negative, each is its entry's, and none outlives a
//!   sweep past `HOLD_MAX_MS`;
//! - the standing is the pure function of the plan, the seat, the balance
//!   and the read-only latch, and the latch follows the balance and the
//!   overdraft;
//! - included credit never carries past its month: at each rollover what
//!   was left expires, and a month grants its plan's credit once;
//! - a retry answers as its first delivery did (a reservation, as its
//!   reference now stands), and a conflicting body changes nothing;
//! - a twin fed the same operations at the same times, never restarted,
//!   answers the same and ends in the same state.

use std::collections::{BTreeMap, BTreeSet};

use fragment_proto::ledger::{GrantCredit, Plan, SeatState, SetFragmentCap, SetOverdraft, SetPlan, SetSeat, Standing};

use super::*;
use crate::price::{PriceBook, StorageClass, Usage, USD};

/// Every mutation, for the simulation and the restart tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Op {
    Grant(GrantCredit),
    Plan(SetPlan),
    Seat(SetSeat),
    Overdraft(SetOverdraft),
    Cap(SetFragmentCap),
    Book(SetPriceBook),
    Reserve(Reserve),
    Settle(Settle),
    Release(Release),
    Meter(Meter),
    Sweep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Outcome {
    Done,
    Reserved(Reserved),
    Settled(Settled),
    Released(Released),
    Metered(Metered),
    Swept(Swept),
}

pub(super) fn apply(ledger: &mut Ledger, store: &mut impl Store, op: &Op, now_ms: i64) -> Result<Outcome, Refused> {
    match op {
        Op::Grant(c) => ledger.grant(store, c, now_ms).map(|()| Outcome::Done),
        Op::Plan(c) => ledger.set_plan(store, c, now_ms).map(|()| Outcome::Done),
        Op::Seat(c) => ledger.set_seat(store, c, now_ms).map(|()| Outcome::Done),
        Op::Overdraft(c) => ledger.set_overdraft(store, c, now_ms).map(|()| Outcome::Done),
        Op::Cap(c) => ledger.set_cap(store, c, now_ms).map(|()| Outcome::Done),
        Op::Book(c) => ledger.set_book(store, c, now_ms).map(|()| Outcome::Done),
        Op::Reserve(r) => ledger.reserve(store, r, now_ms).map(Outcome::Reserved),
        Op::Settle(s) => ledger.settle(store, s, now_ms).map(Outcome::Settled),
        Op::Release(r) => ledger.release(store, r, now_ms).map(Outcome::Released),
        Op::Meter(m) => ledger.meter(store, m, now_ms).map(Outcome::Metered),
        Op::Sweep => Ok(Outcome::Swept(ledger.sweep(store, now_ms))),
    }
}

/// SplitMix64: small, seeded, and the same everywhere.
pub(super) struct Rng(u64);

impl Rng {
    pub(super) fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub(super) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number in `0..n`.
    pub(super) fn below(&mut self, n: u64) -> u64 {
        assert!(n > 0, "a range with something in it");
        self.next() % n
    }

    pub(super) fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }

    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        assert!(lo <= hi, "a range from low to high");
        lo + self.below(hi - lo + 1)
    }
}

/// The store, audited: each reference's first charge is counted, and a
/// second charge of a reference (or a changed one) fails the run.
#[derive(Debug, Default)]
struct Audited {
    inner: Memory,
    charges: BTreeMap<String, u32>,
    charged: i64,
}

impl Store for Audited {
    fn entry(&self, reference: &str) -> Option<Entry> {
        self.inner.entry(reference)
    }

    fn put_entry(&mut self, reference: &str, entry: Entry) {
        let before = self.inner.entry(reference).and_then(|e| e.charge());
        match (before, entry.charge()) {
            (None, Some(charge)) => {
                let times = self.charges.entry(reference.to_string()).or_insert(0);
                *times += 1;
                assert_eq!(*times, 1, "reference {reference} charged twice");
                self.charged += charge;
            }
            (Some(was), now) => assert_eq!(now, Some(was), "the charge of {reference} changed"),
            (None, None) => {}
        }
        self.inner.put_entry(reference, entry);
    }

    fn batch(&self, id: &str) -> Option<BatchRecord> {
        self.inner.batch(id)
    }

    fn put_batch(&mut self, id: &str, record: BatchRecord) {
        self.inner.put_batch(id, record);
    }

    fn command(&self, id: &str) -> Option<CommandRecord> {
        self.inner.command(id)
    }

    fn put_command(&mut self, id: &str, record: CommandRecord) {
        self.inner.put_command(id, record);
    }

    fn spent(&self, month: Month, fragment: &str) -> i64 {
        self.inner.spent(month, fragment)
    }

    fn add_spent(&mut self, month: Month, fragment: &str, micros: i64) {
        self.inner.add_spent(month, fragment, micros);
    }

    fn spends(&self, month: Month, limit: usize) -> Vec<(String, i64)> {
        self.inner.spends(month, limit)
    }

    fn cap(&self, fragment: &str) -> Option<i64> {
        self.inner.cap(fragment)
    }

    fn set_cap(&mut self, fragment: &str, micros: Option<i64>) {
        self.inner.set_cap(fragment, micros);
    }

    fn prune(&mut self, before_ms: i64, before_month: Month) -> u64 {
        self.inner.prune(before_ms, before_month)
    }
}

/// 2026-10-01T00:00:00Z.
pub(super) const T0: i64 = 1_790_812_800_000;
const FRAGMENTS: [&str; 4] = ["todo.ann", "chat.ann", "site.ann", "juniper.ann"];
const FLASH: &str = "@cf/zai-org/glm-5.3-flash";
const GLM: &str = "@cf/zai-org/glm-5.3";
/// Recent rows and batches kept for duplicates and conflicts.
const RECENT_MAX: usize = 256;
/// A retry older than this is dropped: its source gave up (well inside
/// `REF_KEEP_MS`, so every retry is answered from what is remembered).
const RETRY_MAX_MS: i64 = REF_KEEP_MS / 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Sent, its answer not yet heard.
    Reserving,
    Held,
    /// Its settle or release sent, the answer not yet heard.
    Ending,
    Done,
}

#[derive(Debug, Clone, Copy)]
enum Fate {
    Normal,
    /// Applied, heard, and delivered again later.
    Duplicate,
    /// Applied, its answer lost: retried later.
    LostAfter,
    /// Lost before it was applied: retried later.
    LostBefore,
}

struct Pending {
    op: Op,
    /// The answer of its first application, when it had one.
    first: Option<Result<Outcome, Refused>>,
    sent_ms: i64,
}

/// What the simulation believes the ledger holds, from what it sent and
/// what it was answered, never from the ledger itself.
struct Model {
    plan: Plan,
    seat: SeatState,
    seat_seq: u64,
    overdraft: i64,
    month: Month,
    included_granted: i64,
    read_only: bool,
    purchased: i64,
}

/// What the runs exercised, so a run that tested nothing fails.
#[derive(Debug, Default, Clone, Copy)]
pub(super) struct Stats {
    steps: u64,
    rollovers: u64,
    held: u64,
    settled_over: u64,
    released: u64,
    expired: u64,
    rows_new: u64,
    rows_before: u64,
    rows_stale: u64,
    conflicts: u64,
    retries: u64,
    restarts: u64,
    read_only: u64,
    agents_stopped: u64,
    forgotten: u64,
    cap_reached: u64,
    credit_short: u64,
}

struct Sim {
    rng: Rng,
    now: i64,
    ledger: Ledger,
    store: Audited,
    twin: Ledger,
    twin_store: Memory,
    model: Model,
    /// Command ids applied (an answer `Ok` was seen), to tell new from replay.
    applied: BTreeSet<String>,
    calls: BTreeMap<String, (Reserve, Phase)>,
    pending: Vec<Pending>,
    rows: Vec<MeterRow>,
    batches: Vec<(Meter, i64)>,
    serial: u64,
    seat_seq: u64,
    book_version: u32,
    stats: Stats,
}

fn flash(input: u64, output: u64) -> Usage {
    Usage::Tokens { model: FLASH.into(), input, cached_input: 0, cache_write: 0, output }
}

impl Sim {
    fn new(seed: u64) -> Sim {
        let ledger = Ledger::new(T0, PriceBook::defaults());
        let model = Model {
            plan: Plan::Guest,
            seat: SeatState::Active,
            seat_seq: 0,
            overdraft: OVERDRAFT_DEFAULT,
            month: Month::of(T0),
            included_granted: 0,
            read_only: false,
            purchased: 0,
        };
        let mut sim = Sim {
            rng: Rng::new(seed),
            now: T0,
            twin: ledger.clone(),
            ledger,
            store: Audited::default(),
            twin_store: Memory::default(),
            model,
            applied: BTreeSet::new(),
            calls: BTreeMap::new(),
            pending: Vec::new(),
            rows: Vec::new(),
            batches: Vec::new(),
            serial: 0,
            seat_seq: 0,
            book_version: 1,
            stats: Stats::default(),
        };
        // most runs start as a seat; the rest find their plan as they go
        let plan = if sim.rng.chance(80) { Plan::Seat } else { Plan::SeatAlwaysOn };
        let id = sim.id("plan");
        let answer = sim.apply_checked(&Op::Plan(SetPlan { id, plan }));
        assert_eq!(answer, Ok(Outcome::Done));
        sim
    }

    fn id(&mut self, kind: &str) -> String {
        self.serial += 1;
        format!("{kind}:{}", self.serial)
    }

    fn run(&mut self, steps: u64) {
        for _ in 0..steps {
            self.step();
        }
        // the sources deliver what they still hold, then every hold expires
        while let Some(p) = self.pending.pop() {
            self.redeliver(p);
        }
        self.now += HOLD_MAX_MS + 1;
        self.apply_checked(&Op::Sweep).expect("a sweep is never refused");
        assert!(self.ledger.holds.is_empty(), "no hold outlives a sweep past HOLD_MAX_MS");
        assert_eq!(self.ledger, self.twin, "the twin ends where the restarted ledger does");
        assert_eq!(self.store.inner, self.twin_store);
    }

    fn step(&mut self) {
        self.stats.steps += 1;
        self.advance();
        match self.rng.below(100) {
            0..=23 => self.new_call(),
            24..=43 => self.end_call(),
            44..=59 => self.new_batch(),
            60..=69 => self.retry(),
            70..=71 => self.new_grant(),
            72 => self.new_plan(),
            73..=74 => self.new_seat(),
            75..=77 => self.new_overdraft(),
            78..=82 => self.new_cap(),
            83 => self.new_book(),
            84..=88 => {
                self.apply_checked(&Op::Sweep).expect("a sweep is never refused");
            }
            89 => self.restart(),
            90..=94 => self.conflict(),
            _ => self.reads(),
        }
    }

    /// Time moves on: mostly seconds, sometimes hours (holds expire),
    /// rarely weeks (months roll over, references are forgotten).
    fn advance(&mut self) {
        let dt = match self.rng.below(200) {
            0..=139 => self.rng.range(0, 30_000),
            140..=179 => self.rng.range(60_000, 3_600_000),
            180..=198 => self.rng.range(3_600_000, 8 * 3_600_000),
            _ => self.rng.range(2, 12) * DAY_MS as u64,
        };
        self.now += dt as i64;
    }

    fn fate(&mut self) -> Fate {
        match self.rng.below(100) {
            0..=69 => Fate::Normal,
            70..=79 => Fate::Duplicate,
            80..=89 => Fate::LostAfter,
            _ => Fate::LostBefore,
        }
    }

    /// Sends `op` as a caller does, through a fate; answers what the caller heard.
    fn send(&mut self, op: Op) -> Option<Result<Outcome, Refused>> {
        let fate = self.fate();
        if let Fate::LostBefore = fate {
            self.pending.push(Pending { op, first: None, sent_ms: self.now });
            return None;
        }
        let answer = self.apply_checked(&op);
        match fate {
            Fate::Normal => Some(answer),
            Fate::Duplicate => {
                self.pending.push(Pending { op, first: Some(answer.clone()), sent_ms: self.now });
                Some(answer)
            }
            Fate::LostAfter => {
                self.pending.push(Pending { op, first: Some(answer), sent_ms: self.now });
                None
            }
            Fate::LostBefore => unreachable!("handled above"),
        }
    }

    /// One retry, picked at random (so deliveries reorder).
    fn retry(&mut self) {
        if self.pending.is_empty() {
            return self.reads();
        }
        let at = self.rng.below(self.pending.len() as u64) as usize;
        let p = self.pending.swap_remove(at);
        self.redeliver(p);
    }

    fn redeliver(&mut self, p: Pending) {
        if p.sent_ms < self.now - RETRY_MAX_MS {
            return;
        }
        self.stats.retries += 1;
        let answer = self.apply_checked(&p.op);
        if let Some(Ok(first)) = &p.first {
            match (&p.op, first, &answer) {
                (Op::Reserve(r), _, Ok(Outcome::Reserved(now))) => self.stands(&r.reference, first, now),
                (Op::Reserve(_), _, other) => panic!("a reservation applied before was answered {other:?} on retry"),
                _ => assert_eq!(answer.as_ref(), Ok(first), "a retry of {:?} answers as the first did", p.op),
            }
        }
        self.heard(&p.op, &answer);
    }

    /// A retried reservation answers as its reference stands now.
    fn stands(&self, reference: &str, first: &Outcome, now: &Reserved) {
        let entry = self.store.entry(reference).expect("a reservation applied before has its entry");
        let Entry::Reservation { amount, end, .. } = entry else { panic!("{reference} is a reservation") };
        let expected = match end {
            None => Reserved::Held { amount },
            Some(End::Settled { charge, .. }) => Reserved::Settled { charge },
            Some(End::Released) => Reserved::Released,
        };
        assert_eq!(*now, expected, "a retried reservation answers as it stands");
        if let Outcome::Reserved(Reserved::Held { amount: first_amount }) = first {
            assert_eq!(*first_amount, amount, "a reservation keeps the amount it was held at");
        }
    }

    /// What a caller does with an answer it heard.
    fn heard(&mut self, op: &Op, answer: &Result<Outcome, Refused>) {
        let reference = match op {
            Op::Reserve(r) => &r.reference,
            Op::Settle(s) => &s.reference,
            Op::Release(r) => &r.reference,
            _ => return,
        };
        let Some((reserve, phase)) = self.calls.get(reference).cloned() else { return };
        let next = match (op, answer, phase) {
            (Op::Reserve(_), Ok(Outcome::Reserved(Reserved::Held { .. })), Phase::Reserving) => Phase::Held,
            (Op::Reserve(_), Ok(Outcome::Reserved(Reserved::Held { .. })), p) => p,
            (Op::Reserve(_), Ok(Outcome::Reserved(_)), _) => Phase::Done,
            // a refusal is not remembered: the caller may try the same reference again
            (Op::Reserve(_), Err(_), Phase::Reserving) => {
                if self.rng.chance(30) {
                    self.pending.push(Pending { op: op.clone(), first: None, sent_ms: self.now });
                    Phase::Reserving
                } else {
                    Phase::Done
                }
            }
            // a duplicate of a refused reservation, refused again
            (Op::Reserve(_), Err(_), p) => p,
            (Op::Settle(_) | Op::Release(_), Ok(_), _) => Phase::Done,
            (_, Err(e), _) => panic!("{op:?} for a call {phase:?} was refused: {e:?}"),
            (_, _, p) => p,
        };
        self.calls.insert(reference.clone(), (reserve, next));
    }

    /// Applies `op` to the ledger and its twin, and checks everything.
    fn apply_checked(&mut self, op: &Op) -> Result<Outcome, Refused> {
        let before = self.ledger.money;
        let reserved_before = self.ledger.reserved();
        let granted_before = self.model.included_granted;
        let month = Month::of(self.now);
        let rolled = month > self.model.month;
        if rolled {
            // the rollover the ledger makes first: the month's credit, for
            // the plan and seat as they are
            self.stats.rollovers += 1;
            self.model.month = month;
            self.model.included_granted = included(self.model.plan, self.model.seat);
        }
        // a replayed batch answers from its record, as of its first time
        let replayed = matches!(op, Op::Meter(m) if self.store.batch(&m.batch).is_some());
        let answer = apply(&mut self.ledger, &mut self.store, op, self.now);
        let twin = apply(&mut self.twin, &mut self.twin_store, op, self.now);
        assert_eq!(answer, twin, "the twin answers {op:?} the same");
        let overdraft_set = self.learn(op, &answer);
        if !replayed {
            self.justify(op, &answer, &before, reserved_before);
        }
        let after = self.ledger.money;
        // included credit never carries past its month
        if rolled {
            assert_eq!(after.totals.expired - before.totals.expired, before.included_left, "a rollover expires what was left of the month");
            assert_eq!(after.totals.included - before.totals.included, self.model.included_granted, "a month grants its credit once");
        } else {
            assert_eq!(after.totals.expired, before.totals.expired, "credit expires only at a rollover");
            assert_eq!(after.totals.included - before.totals.included, self.model.included_granted - granted_before, "credit is granted once a month");
        }
        // the latch, from the balance and the overdraft alone
        let balance = after.balance();
        if overdraft_set {
            self.model.read_only = balance <= -self.model.overdraft;
        }
        if balance <= -self.model.overdraft {
            self.model.read_only = true;
        } else if balance > 0 {
            self.model.read_only = false;
        }
        self.check();
        answer
    }

    /// Updates the model from an applied command; answers whether it set
    /// the overdraft.
    fn learn(&mut self, op: &Op, answer: &Result<Outcome, Refused>) -> bool {
        let id = match op {
            Op::Grant(c) => &c.id,
            Op::Plan(c) => &c.id,
            Op::Seat(c) => &c.id,
            Op::Overdraft(c) => &c.id,
            Op::Cap(c) => &c.id,
            Op::Book(c) => &c.id,
            _ => return false,
        };
        if answer.is_err() || !self.applied.insert(id.clone()) {
            return false;
        }
        let m = &mut self.model;
        match op {
            Op::Grant(c) => m.purchased += c.micros,
            Op::Plan(c) => m.plan = c.plan,
            Op::Seat(c) if c.seq > m.seat_seq => (m.seat, m.seat_seq) = (c.seat, c.seq),
            Op::Overdraft(c) => m.overdraft = c.micros,
            _ => {}
        }
        m.included_granted = m.included_granted.max(included(m.plan, m.seat));
        matches!(op, Op::Overdraft(_))
    }

    /// Each refusal has its reason in the state before it.
    fn justify(&mut self, op: &Op, answer: &Result<Outcome, Refused>, before: &Money, reserved_before: i64) {
        let before_month = {
            let mut m = *before;
            m.roll(Month::of(self.now));
            m
        };
        match (op, answer) {
            (Op::Reserve(_), Err(Refused::CreditShort { available, needed })) => {
                self.stats.credit_short += 1;
                assert!(needed > available, "a short reservation needs more than is available");
                assert_eq!(*available, before_month.balance() - reserved_before, "available is the balance less what is held");
            }
            (Op::Reserve(_), Err(Refused::GuestPayer)) => assert_eq!(before_month.plan, Plan::Guest),
            (Op::Reserve(_), Err(Refused::AgentsStopped { why })) => {
                self.stats.agents_stopped += 1;
                assert_eq!(before_month.standing(), Standing::AgentsStopped { why: *why });
            }
            (Op::Reserve(_), Err(Refused::ReadOnly { why })) => assert_eq!(before_month.standing(), Standing::ReadOnly { why: *why }),
            (Op::Reserve(_), Err(Refused::CapReached { cap, spent })) => {
                self.stats.cap_reached += 1;
                assert!(spent >= cap, "a cap is reached when its spend is at it");
            }
            (Op::Reserve(_), Ok(Outcome::Reserved(Reserved::Held { .. }))) => self.stats.held += 1,
            (Op::Settle(s), Ok(Outcome::Settled(settled))) => {
                if let Some(Entry::Reservation { amount, .. }) = self.store.entry(&s.reference) {
                    if settled.charge > amount {
                        self.stats.settled_over += 1;
                    }
                }
            }
            (Op::Release(_), Ok(Outcome::Released(Released::Back))) => self.stats.released += 1,
            (Op::Meter(m), Ok(Outcome::Metered(metered))) => {
                self.stats.rows_new += u64::from(metered.rows_new);
                self.stats.rows_before += u64::from(metered.rows_before);
                for refused in &metered.refused {
                    let row = &m.rows[refused.index as usize];
                    match refused.why {
                        Refused::Stale => {
                            self.stats.rows_stale += 1;
                            assert!(row.at_ms < self.now - REF_KEEP_MS, "only a row older than the ledger remembers is stale");
                        }
                        Refused::NoPrice => assert_eq!(self.ledger.book.price(&row.usage), Err(PriceError::NoPrice)),
                        Refused::ConflictingBody => self.stats.conflicts += 1,
                        other => panic!("row {row:?} refused {other:?}"),
                    }
                }
            }
            (Op::Sweep, Ok(Outcome::Swept(swept))) => {
                self.stats.expired += swept.expired.len() as u64;
                self.stats.forgotten += swept.forgotten;
            }
            _ => {}
        }
        if let Standing::ReadOnly { .. } = self.ledger.money.standing() {
            self.stats.read_only += 1;
        }
    }

    fn check(&self) {
        let l = &self.ledger;
        let m = l.money;
        l.assert_valid();
        let model = &self.model;
        assert_eq!(m.totals.charged, self.store.charged, "every charge passed through the store, once");
        assert_eq!(m.totals.purchased, model.purchased, "each grant counts once");
        assert_eq!(m.balance(), model.purchased + m.totals.included - m.totals.expired - self.store.charged, "the balance is grants less charges");
        assert_eq!((m.plan, m.seat, m.seat_seq, m.overdraft), (model.plan, model.seat, model.seat_seq, model.overdraft));
        assert_eq!((m.month, m.included_granted, m.read_only), (model.month, model.included_granted, model.read_only));
        assert!(m.included_left <= m.included_granted, "included credit never carries past its month");
        assert_eq!(l.standing(self.now), standing_of(model.plan, model.seat, m.balance(), model.read_only), "the standing is a pure function");
        assert!(l.reserved() >= 0, "what is held is never negative");
        for (reference, hold) in &l.holds {
            assert!(hold.amount >= 0, "a hold is never negative");
            match self.store.entry(reference) {
                Some(Entry::Reservation { amount, end: None, .. }) => assert_eq!(amount, hold.amount),
                other => panic!("hold {reference} has entry {other:?}"),
            }
        }
    }

    fn new_call(&mut self) {
        let reference = self.id("call");
        let spend = if self.rng.chance(70) { Spend::AgentTurn } else { Spend::AiStep };
        let worst = match self.rng.below(20) {
            0 => Usage::Tokens { model: "@cf/unknown".into(), input: 10, cached_input: 0, cache_write: 0, output: 10 },
            1..=9 => flash(self.rng.range(1_000, 200_000), self.rng.range(0, 8_192)),
            _ => Usage::Tokens { model: GLM.into(), input: self.rng.range(1_000, 120_000), cached_input: self.rng.range(0, 50_000), cache_write: 0, output: self.rng.range(0, 4_096) },
        };
        let fragment = self.rng.chance(70).then(|| FRAGMENTS[self.rng.below(4) as usize].to_string());
        let capped = fragment.is_some() && self.rng.chance(40);
        let reserve = Reserve { reference: reference.clone(), spend, worst, fragment, agent: Some("juniper.ann".into()), capped };
        self.calls.insert(reference, (reserve.clone(), Phase::Reserving));
        if let Some(answer) = self.send(Op::Reserve(reserve.clone())) {
            self.heard(&Op::Reserve(reserve), &answer);
        }
    }

    fn end_call(&mut self) {
        let held: Vec<String> = self.calls.iter().filter(|(_, (_, p))| *p == Phase::Held).map(|(r, _)| r.clone()).collect();
        if held.is_empty() {
            return self.new_call();
        }
        let reference = held[self.rng.below(held.len() as u64) as usize].clone();
        let (reserve, _) = self.calls[&reference].clone();
        let op = if self.rng.chance(75) {
            let Usage::Tokens { model, input, output, .. } = reserve.worst.clone() else { panic!("calls reserve tokens") };
            let share = self.rng.range(5, 100);
            let usage = match self.rng.below(20) {
                // a call that used more than its worst case (it happened)
                0..=2 => Some(Usage::Tokens { model, input: input * 2, cached_input: 0, cache_write: 0, output: output * 2 + 1 }),
                3..=4 => None,
                5 => Some(Usage::Tokens { model: "@cf/unknown".into(), input: 5, cached_input: 0, cache_write: 0, output: 5 }),
                6..=7 => Some(Usage::Neurons { milli: self.rng.range(0, 3_000_000) }),
                _ => Some(Usage::Tokens { model, input: input * share / 100, cached_input: 0, cache_write: 0, output: output * share / 100 }),
            };
            Op::Settle(Settle { reference: reference.clone(), usage })
        } else {
            Op::Release(Release { reference: reference.clone() })
        };
        self.calls.insert(reference, (reserve, Phase::Ending));
        if let Some(answer) = self.send(op.clone()) {
            self.heard(&op, &answer);
        }
    }

    fn new_row(&mut self) -> MeterRow {
        let pick = self.rng.below(100);
        if pick < 8 && !self.rows.is_empty() {
            // the same row again, from another batch
            return self.rows[self.rng.below(self.rows.len() as u64) as usize].clone();
        }
        let usage = match self.rng.below(9) {
            0 => flash(self.rng.range(0, 30_000), self.rng.range(0, 1_000)),
            1 => Usage::Neurons { milli: self.rng.range(0, 100_000) },
            2 => {
                let instance = if self.rng.chance(95) { "2vcpu-6gib" } else { "8vcpu-32gib" };
                Usage::Awake { instance: instance.into(), ms: self.rng.range(0, 24 * 3_600_000) }
            }
            3 => {
                let class = [StorageClass::R2, StorageClass::Sqlite, StorageClass::Git][self.rng.below(3) as usize];
                Usage::Storage { class, byte_hours: self.rng.range(0, 8_000_000_000_000) }
            }
            4 => Usage::Requests { count: self.rng.range(1, 200_000) },
            5 => Usage::DynamicWorkers { count: self.rng.range(1, 3) },
            6 => Usage::Browser { ms: self.rng.range(500, 6_000) },
            7 => Usage::Images { count: self.rng.range(1, 10) },
            _ => Usage::Key { key: "search".into(), units: 1 },
        };
        // A source that reuses a reference for other usage is a buggy one:
        // the ledger refuses it while it remembers the reference, and these
        // reuse only references it does (a reuse past `REF_KEEP_MS` is
        // beyond what any ledger could tell from new usage).
        let reused = match pick {
            // a reservation's reference
            8..=10 if !self.calls.is_empty() => {
                let refs: Vec<&String> = self.calls.keys().collect();
                Some(refs[self.rng.below(refs.len() as u64) as usize].clone())
            }
            // an earlier row's reference with another body
            11..=12 if !self.rows.is_empty() => Some(self.rows[self.rng.below(self.rows.len() as u64) as usize].reference.clone()),
            _ => None,
        };
        let remembered = reused.filter(|r| self.store.entry(r).is_some_and(|e| e.at_ms() >= self.now - RETRY_MAX_MS));
        let reference = match remembered {
            Some(r) => r,
            None => self.id("row"),
        };
        let fragment = self.rng.chance(80).then(|| FRAGMENTS[self.rng.below(4) as usize].to_string());
        let at_ms = self.now - self.rng.range(0, 600_000) as i64;
        let row = MeterRow { reference, usage, fragment, agent: None, computer: Some("computer:0a1b".into()), at_ms };
        if self.rows.len() < RECENT_MAX {
            self.rows.push(row.clone());
        } else {
            let at = self.rng.below(RECENT_MAX as u64) as usize;
            self.rows[at] = row.clone();
        }
        row
    }

    fn new_batch(&mut self) {
        let n = self.rng.range(1, 12);
        let rows = (0..n).map(|_| self.new_row()).collect();
        let meter = Meter { batch: self.id("batch"), rows };
        if self.batches.len() >= RECENT_MAX {
            self.batches.remove(0);
        }
        self.batches.push((meter.clone(), self.now));
        self.send(Op::Meter(meter));
    }

    fn new_grant(&mut self) {
        let micros = if self.rng.chance(5) { 0 } else { self.rng.range(1, 6) as i64 * USD / 2 };
        let id = self.id("grant");
        let answer = self.send(Op::Grant(GrantCredit { id, micros, by: "stripe".into(), why: "credit".into() }));
        if micros == 0 {
            assert!(matches!(answer, None | Some(Err(Refused::Invalid { what: Invalid::Amount }))), "a grant of nothing is refused: {answer:?}");
        }
    }

    fn new_plan(&mut self) {
        let plan = match self.rng.below(10) {
            0 => Plan::Guest,
            1..=6 => Plan::Seat,
            _ => Plan::SeatAlwaysOn,
        };
        let id = self.id("plan");
        self.send(Op::Plan(SetPlan { id, plan }));
    }

    fn new_seat(&mut self) {
        let seat = match self.rng.below(10) {
            0..=5 => SeatState::Active,
            6..=7 => SeatState::PastDue,
            _ => SeatState::Canceled,
        };
        self.seat_seq += 1;
        let (id, seq) = (self.id("seat"), self.seat_seq);
        self.send(Op::Seat(SetSeat { id, seat, seq }));
    }

    fn new_overdraft(&mut self) {
        let micros = self.rng.range(0, 10) as i64 * USD / 2;
        let id = self.id("overdraft");
        self.send(Op::Overdraft(SetOverdraft { id, micros }));
    }

    fn new_cap(&mut self) {
        let fragment = FRAGMENTS[self.rng.below(4) as usize].to_string();
        let micros = self.rng.chance(80).then(|| self.rng.range(0, 6) as i64 * USD / 2);
        let id = self.id("cap");
        self.send(Op::Cap(SetFragmentCap { id, fragment, micros }));
    }

    fn new_book(&mut self) {
        self.book_version += 1;
        let mut book = PriceBook::defaults();
        book.version = self.book_version;
        book.margin_bp = [4_000, 5_000, 6_000][self.rng.below(3) as usize];
        if self.rng.chance(50) {
            book.keys.push(crate::price::KeyPrice { key: "search".into(), micros: 5_000_000, per: 1_000 });
        }
        let id = self.id("book");
        // a late book (reordered behind a newer one) is refused as stale
        self.send(Op::Book(SetPriceBook { id, book }));
    }

    fn restart(&mut self) {
        self.stats.restarts += 1;
        let ledger: Ledger = serde_json::from_str(&serde_json::to_string(&self.ledger).unwrap()).unwrap();
        let store: Memory = serde_json::from_str(&serde_json::to_string(&self.store.inner).unwrap()).unwrap();
        assert_eq!(ledger, self.ledger, "the head reads back as it was written");
        assert_eq!(store, self.store.inner, "the store reads back as it was written");
        self.ledger = ledger;
        self.store.inner = store;
    }

    /// The same id with another body: refused, and nothing changes.
    fn conflict(&mut self) {
        let op = match self.rng.below(3) {
            0 => {
                let Some(id) = self.applied.iter().nth(self.rng.below(self.applied.len().max(1) as u64) as usize).cloned() else { return };
                Op::Grant(GrantCredit { id, micros: 7_777_777, by: "operator".into(), why: "conflict".into() })
            }
            1 => {
                let recent: Vec<&Meter> = self.batches.iter().filter(|(m, at)| *at >= self.now - RETRY_MAX_MS && self.store.batch(&m.batch).is_some()).map(|(m, _)| m).collect();
                let Some(m) = recent.first().map(|m| (*m).clone()) else { return };
                let mut rows = m.rows.clone();
                rows[0].at_ms -= 1;
                Op::Meter(Meter { batch: m.batch, rows })
            }
            _ => {
                let ended: Vec<String> = self.calls.iter().filter(|(_, (_, p))| *p == Phase::Done).map(|(r, _)| r.clone()).collect();
                let Some(reference) = ended.last().cloned() else { return };
                if self.store.entry(&reference).is_none_or(|e| e.held()) {
                    return;
                }
                Op::Settle(Settle { reference, usage: Some(flash(123_456_789, 1)) })
            }
        };
        let rolls = Month::of(self.now) > self.ledger.money.month;
        let (ledger, store) = (self.ledger.clone(), self.store.inner.clone());
        let answer = self.apply_checked(&op);
        match (&op, &answer) {
            (_, Err(Refused::ConflictingBody)) => self.stats.conflicts += 1,
            // a settle of a released call, or one an expiry settled
            (Op::Settle(_), Err(Refused::Ended) | Ok(Outcome::Settled(Settled { basis: Basis::Expired, .. }))) => {}
            // a grant id that a command of another kind used, with a body that fits it: still another body
            other => panic!("a conflicting {op:?} was answered {other:?}"),
        }
        if !rolls {
            assert_eq!((&self.ledger, &self.store.inner), (&ledger, &store), "a conflicting body changes nothing");
        }
    }

    /// Reads change nothing and agree with the standing.
    fn reads(&mut self) {
        let standing = self.ledger.standing(self.now);
        for spend in [Spend::AgentTurn, Spend::AiStep, Spend::Wake, Spend::Write] {
            let gate = self.ledger.gate(spend, self.now);
            let expected = match (self.model.plan, standing, spend) {
                (Plan::Guest, _, _) => Err(Refused::GuestPayer),
                (_, Standing::ReadOnly { why }, _) => Err(Refused::ReadOnly { why }),
                (_, _, Spend::Write) | (_, Standing::Ok, _) => Ok(()),
                (_, Standing::AgentsStopped { why }, _) => Err(Refused::AgentsStopped { why }),
            };
            assert_eq!(gate, expected, "{spend:?} at {standing:?}");
        }
        let status = self.ledger.status(&self.store, self.now);
        assert_eq!(status.standing, standing);
        assert_eq!(status.reserved_micros, self.ledger.reserved());
        assert_eq!(status.available_micros, status.balance_micros - status.reserved_micros);
        for fragment in FRAGMENTS {
            let open = self.ledger.fragment_open(&self.store, fragment, self.now);
            let month = self.ledger.money_at(self.now).month;
            let spent = self.store.spent(month, fragment) + self.ledger.held_in(fragment);
            assert_eq!(open.is_ok(), spent < self.store.cap(fragment).unwrap_or(CAP_DEFAULT));
            for spend in [Spend::AgentTurn, Spend::Write] {
                let gate = self.ledger.gate(spend, self.now);
                assert_eq!(self.ledger.may_spend(&self.store, spend, Some(fragment), true, self.now), gate, "the owner answers to no cap");
                let capped = if spend == Spend::AgentTurn { gate.and(open) } else { gate };
                assert_eq!(self.ledger.may_spend(&self.store, spend, Some(fragment), false, self.now), capped, "others answer to the cap");
            }
        }
    }
}

/// Runs the simulation from `seed` and answers what it exercised.
pub(super) fn simulate(seed: u64, steps: u64) -> Stats {
    let mut sim = Sim::new(seed);
    sim.run(steps);
    sim.stats
}

impl Stats {
    fn named(&self) -> [(&'static str, u64); 17] {
        [
            ("steps", self.steps),
            ("rollovers", self.rollovers),
            ("holds", self.held),
            ("settles past their reservations", self.settled_over),
            ("releases", self.released),
            ("expired holds", self.expired),
            ("new rows", self.rows_new),
            ("rows seen before", self.rows_before),
            ("stale rows", self.rows_stale),
            ("conflicting bodies", self.conflicts),
            ("retries", self.retries),
            ("restarts", self.restarts),
            ("steps read-only", self.read_only),
            ("agents stopped", self.agents_stopped),
            ("references forgotten", self.forgotten),
            ("caps reached", self.cap_reached),
            ("reservations short of credit", self.credit_short),
        ]
    }
}

/// Eight seeds of 3,000 steps each (about five simulated months apiece),
/// one thread each.
#[test]
fn the_ledger_holds_its_invariants_under_crashes_duplicates_and_reordering() {
    let seeds = [1, 2, 3, 5, 8, 13, 21, 34];
    let runs: Vec<Stats> = std::thread::scope(|s| {
        let handles: Vec<_> = seeds.iter().map(|seed| s.spawn(move || simulate(*seed, 3_000))).collect();
        handles.into_iter().map(|h| h.join().expect("a seed's run passes")).collect()
    });
    let mut total: BTreeMap<&str, u64> = BTreeMap::new();
    for run in runs {
        for (what, n) in run.named() {
            *total.entry(what).or_insert(0) += n;
        }
    }
    // a run that exercised nothing proves nothing
    for (what, n) in &total {
        assert!(*n > 0, "the simulation never exercised {what}: {total:?}");
    }
}

/// The same seed runs the same: a failure replays exactly.
#[test]
fn a_seed_replays_exactly() {
    assert_eq!(simulate(99, 400).named(), simulate(99, 400).named());
}
