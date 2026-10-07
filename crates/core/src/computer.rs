//! A computer's lifecycle (docs/computers.md), as a pure state machine the
//! Computer Durable Object drives. The DO keeps `Lifecycle` whole, hands
//! it each `Event` with the time it happened, and performs the `Action`s
//! it answers. Every start has a generation, and every completion names
//! the start it completes, so a late answer from an earlier start (a
//! container that came up after it was put to sleep) changes nothing.
//!
//! What it decides:
//! - **Waking.** A record on a subscribed channel, an open port tab, a
//!   page's presence (a pre-wake), one of its agents added to a fragment,
//!   or the owner. A wake while asleep
//!   starts the container; one while it starts or is awake holds it
//!   longer; one while it is going to sleep starts it again once it is
//!   asleep (the newest push wins: lesson 3 of docs/cloudflare-v1.md).
//! - **Staying awake.** While the guest holds a keepalive socket or a
//!   port tab is open; with nothing open, until the latest hold runs out
//!   (twenty minutes after a record or the last socket closed, a minute
//!   after a pre-wake). An always-on computer never sleeps.
//! - **Saving** (docs/durable-computers.md, step 1). `/data` is saved when
//!   work ends (the guest's last keepalive closes, and `SAVE_SETTLE_MS`
//!   pass with none opened again, so turns back to back save once), every
//!   `SAVE_EVERY_MS` while a keepalive stays open, and at every sleep. A
//!   save is a hold (the DO touches `/run/computer/hold` and waits for the
//!   guest's `/run/computer/held`), the save, and, awake, the hold let go.
//!   A save that fails is tried again after a growing pause.
//! - **Sleeping.** Its hold and its save, then its stop (snapshot, signal,
//!   wait, destroy), reported back by `Asleep`. A keepalive that opens
//!   during an idle sleep's hold cancels the sleep: the guest took work as
//!   it was held, so it stays awake (P2 of docs/explorations/pi-durable.md).
//!   A sleep whose save fails keeps its container and is tried again, for
//!   at most `Rules::unsaved_max_ms`; then it sleeps unsaved, and says so.
//! - **Failing.** A start that fails, or never reports ready, is tried
//!   again with a growing pause; after `STARTS_FAILED_MAX` in a row the
//!   computer "won't wake" until its owner asks (lesson 4): no more
//!   container starts are paid for. A start whose save would not restore
//!   starts again at once from the save before it, no strike against it.
//! - **Metering.** Awake time, in intervals that never overlap, every
//!   `METER_EVERY_MS` while awake and at sleep.
//!
//! And, beside it, what the DO knows of its saves (`Saves`): the newest
//! `SAVES_KEPT` saves of `/data`, the snapshot that caches the current one,
//! what each start that came up restored, and whether that went back in
//! time (a rollback).

use fragment_proto::computer::{ComputerRestore, ComputerSave, LifeEnd, RestoreSource};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The id of a person's computer: one each for now (decision 13), so it is
/// derived from the owner and making it twice makes one. Later computers
/// (ephemeral ones, machines brought along) get ids of their own; nothing
/// but the person's first computer assumes this one.
pub fn default_computer_of(owner: &str) -> String {
    let digest = Sha256::digest(format!("fragment computer\0{owner}").as_bytes());
    format!("computer:{}", &hex::encode(digest)[..24])
}

/// The computers a socket of `by` on a fragment pre-wakes (decision 39):
/// each one the fragment's wake subscriptions (`(principal, computer)`)
/// name, but none that `by`'s own name. A computer's agents never pre-wake
/// their own computer, as what they post never wakes it: its guest opens
/// their sockets as it boots and again after one drops, also while it goes
/// to sleep, and a wake then would start it again from that sleep's save.
/// Each once, in the order first named.
pub fn prewoken<'a>(subs: &[(&'a str, &'a str)], by: &str) -> Vec<&'a str> {
    let own: Vec<&str> = subs.iter().filter(|(principal, _)| *principal == by).map(|(_, computer)| *computer).collect();
    let mut out: Vec<&str> = Vec::with_capacity(subs.len());
    for (_, computer) in subs {
        if !own.contains(computer) && !out.contains(computer) {
            out.push(computer);
        }
    }
    out
}

/// With nothing open, how long a record or a closed socket holds a
/// computer awake (decision 39).
pub const IDLE_MS: i64 = 20 * 60_000;
/// How long a pre-wake holds one that nothing else holds (decision 39:
/// "it stops again after 60 s if nothing arrives").
pub const PREWAKE_MS: i64 = 60_000;
/// Starts that fail in a row before the computer stops trying.
pub const STARTS_FAILED_MAX: u32 = 3;
/// A start that has not reported ready by then has failed: a new image's
/// first pull at a location takes 24–40 s (spike S3b), a Hermes boot on a
/// fresh disk up to 82 s.
pub const START_DEADLINE_MS: i64 = 3 * 60_000;
/// A sleep's stop (snapshot, signal, five seconds of exit, destroy)
/// finishes within this, or it is asked again.
pub const SLEEP_DEADLINE_MS: i64 = 60_000;
/// Awake time is metered at least this often.
pub const METER_EVERY_MS: i64 = 5 * 60_000;
/// The pause before a failed start is tried again, doubled for each
/// failure before it.
pub const RETRY_PAUSE_MS: i64 = 5_000;
/// After the guest's last keepalive closes, a save is asked for this much
/// later, unless one opens first: turns back to back save once.
pub const SAVE_SETTLE_MS: i64 = 30_000;
/// While a keepalive stays open, it is saved this often (Cloudflare's
/// auto-save guide: a sandbox that never goes idle is saved on a timer).
pub const SAVE_EVERY_MS: i64 = 15 * 60_000;
/// The pause before a failed save is tried again, doubled for each failure
/// in a row before it, up to `SAVE_EVERY_MS`.
pub const SAVE_RETRY_MS: i64 = 60_000;
/// How long a computer stays awake for a sleep whose save keeps failing,
/// by default (`Rules`, the deployment's `computer_unsaved_max_ms`): a
/// default for Paul to confirm (docs/durable-computers.md).
pub const UNSAVED_MAX_MS_DEFAULT: i64 = 30 * 60_000;
/// How long the DO waits for the guest's `held` after it touched the hold.
/// An image that never answers is saved anyway, not held.
pub const HOLD_WAIT_MS: i64 = 20_000;
/// A hold (the touch, the wait, the answer) reports within this, or it is
/// asked again.
pub const HOLD_DEADLINE_MS: i64 = 90_000;
/// A save reports within this, or it is asked again (spike S3: 52 MB in
/// 3.3 s, so a few GiB fit).
pub const SAVE_DEADLINE_MS: i64 = 10 * 60_000;
/// Saves of `/data` kept: a wake restores the newest that restores, and
/// falls back to the one before it (F5 of docs/explorations/pi-durable.md).
pub const SAVES_KEPT: usize = 3;

const _: () = assert!(PREWAKE_MS < IDLE_MS, "a pre-wake holds less than a record");
const _: () = assert!(RETRY_PAUSE_MS << STARTS_FAILED_MAX < START_DEADLINE_MS, "retries stay within a start's deadline");
const _: () = assert!(SAVE_SETTLE_MS < IDLE_MS, "work that ended is saved before an idle sleep would save it");
const _: () = assert!(SAVE_RETRY_MS < SAVE_EVERY_MS && SAVE_EVERY_MS < UNSAVED_MAX_MS_DEFAULT, "a failed save is tried again within its bound");
const _: () = assert!(HOLD_WAIT_MS < HOLD_DEADLINE_MS, "a hold's wait fits its deadline");
const _: () = assert!(SAVES_KEPT >= 2, "a save to fall back to");

/// The deployment's rules for its computers' lifecycles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// A sleep whose save keeps failing keeps its container this long at
    /// most, then sleeps unsaved (I5 of docs/explorations/pi-durable.md).
    pub unsaved_max_ms: i64,
}

impl Default for Rules {
    fn default() -> Rules {
        Rules { unsaved_max_ms: UNSAVED_MAX_MS_DEFAULT }
    }
}

/// What woke it, which says how long it holds the computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Wake {
    /// A record on a channel one of its agents subscribed to.
    Record,
    /// A page opened a fragment it subscribes to, or someone is typing.
    Presence,
    /// A port tab is opening.
    Tab,
    /// Its owner asked (the shell, the CLI): also lifts "won't wake".
    Owner,
    /// One of its agents was added to a fragment (Paul, 2026-10-03: agents
    /// are woken eagerly, to hide a wake's latency): the guest lists its
    /// agent's fragments and follows the new one before anyone speaks
    /// there, and is held as a record holds it, since someone is about to.
    Joined,
}

impl Wake {
    fn hold_ms(self) -> i64 {
        match self {
            Wake::Presence => PREWAKE_MS,
            Wake::Record | Wake::Tab | Wake::Owner | Wake::Joined => IDLE_MS,
        }
    }
}

/// A socket the Computer DO accepted that keeps it awake while open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Socket {
    /// The guest's own (`/api/computer/keepalive`): it is busy.
    Keepalive,
    /// A person's tab onto one of its ports.
    Tab,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    Asleep,
    Starting { generation: u64, since_ms: i64 },
    Awake { generation: u64, since_ms: i64 },
    Sleeping { generation: u64, since_ms: i64 },
    /// Its starts kept failing: it won't wake until its owner asks.
    Failed { why: String },
}

/// The save under way (awake, or a sleep's), by its step, and since when
/// that step began (its deadline counts from there).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Saving {
    /// The DO touched the hold and waits for the guest's `held`.
    Hold { since_ms: i64 },
    /// The DO saves `/data` (`held`: the guest answered), as save `seq`.
    Save { since_ms: i64, held: bool, seq: u64 },
    /// A sleep's last step: snapshot (`saved`: its save worked), signal,
    /// wait, destroy.
    Stop { since_ms: i64, saved: bool },
}

impl Saving {
    fn since_ms(self) -> i64 {
        match self {
            Saving::Hold { since_ms } | Saving::Save { since_ms, .. } | Saving::Stop { since_ms, .. } => since_ms,
        }
    }

    fn deadline_ms(self) -> i64 {
        let ms = match self {
            Saving::Hold { .. } => HOLD_DEADLINE_MS,
            Saving::Save { .. } => SAVE_DEADLINE_MS,
            Saving::Stop { .. } => SLEEP_DEADLINE_MS,
        };
        self.since_ms() + ms
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Wake { why: Wake },
    /// The start `generation` finished: running, its intercepts set, its
    /// data restored.
    Ready { generation: u64 },
    StartFailed { generation: u64, why: String },
    /// The start `generation` was from a snapshot, and failed (or its
    /// container stopped before it came up). The DO has forgotten the
    /// snapshot, so the next start is from the image and the save: it is
    /// started again at once, and the first in a wake is no strike against
    /// the computer (the snapshot was at fault, not the computer).
    SnapshotFailed { generation: u64, why: String },
    /// The start `generation` restored a save that is unusable (its archive
    /// is gone or corrupt, or the image's check of what it restored
    /// failed): the DO marked it so. With an `older` save left to try, it
    /// starts again at once from that one, no strike against it (F5).
    RestoreFailed { generation: u64, why: String, older: bool },
    Opened { socket: Socket },
    Closed { socket: Socket },
    /// The DO touched the hold of start `generation` and waited: whether
    /// the guest answered `held` in time.
    Held { generation: u64, held: bool },
    /// Save `seq` of start `generation` worked: the DO keeps it.
    Saved { generation: u64, seq: u64 },
    /// Save `seq` of start `generation` failed (`seq` 0: its hold found no
    /// container to save).
    SaveFailed { generation: u64, seq: u64, why: String },
    /// The sleep `generation` finished: the container is gone.
    Asleep { generation: u64 },
    /// The container stopped on its own (a crash, the runtime's idle stop).
    Exited { generation: u64 },
    Alarm,
    /// Its owner put it to sleep: what held it is let go (its sockets close
    /// with its container).
    Sleep,
    /// The owner's plan changed: an always-on computer never sleeps.
    AlwaysOn { on: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Start the container (from a snapshot, or the image and a restore:
    /// the DO's choice), then report `Ready` or `StartFailed`.
    Start { generation: u64 },
    /// Begin its sleep: hold the guest, then report `Held` (or `Asleep`
    /// when its container is gone already).
    Sleep { generation: u64 },
    /// Hold the guest for a save while it stays awake, then report `Held`.
    Hold { generation: u64 },
    /// Save `/data` as save `seq` (leaving out what a held guest copied, when
    /// `held`), then report `Saved` or `SaveFailed`.
    Save { generation: u64, held: bool, seq: u64 },
    /// Let go of the guest's hold: an awake save's end, or a sleep that
    /// will not stop it.
    Unhold { generation: u64 },
    /// End its sleep: a snapshot (when its save worked: a cache of it), the
    /// signal, the wait, the destroy; then report `Asleep`.
    Stop { generation: u64, saved: bool },
    /// Meter awake time from `from_ms` to `to_ms` to the owner.
    Meter { from_ms: i64, to_ms: i64 },
}

/// What the computer says of its saves in its view (its `why`), as one
/// step changes it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SaveNote {
    /// As it was.
    #[default]
    Same,
    Says(String),
    /// A save worked: nothing to say.
    Clear,
}

/// What an event answered: the actions to perform, in order, and when the
/// DO's alarm should next fire (`None`: no alarm needed).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Step {
    pub actions: Vec<Action>,
    pub alarm_ms: Option<i64>,
    /// Why a wake started nothing (it won't wake).
    pub refused: Option<String>,
    /// A life (a start that came up) ended with this event: the DO tells
    /// its `Saves` before it performs the actions (the next start reads it).
    pub ended: Option<Ended>,
    /// What its view says of its saves from now on.
    pub note: SaveNote,
}

/// How one life ended: the start it was, by what, and whether its sleep
/// saved it (a sleep's stop after a save that worked).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    pub generation: u64,
    pub by: LifeEnd,
    #[serde(default)]
    pub saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lifecycle {
    pub phase: Phase,
    /// The last start's generation.
    generation: u64,
    keepalives: u32,
    tabs: u32,
    /// With nothing open, it is held awake until this.
    held_until_ms: i64,
    /// A wake came while it was going to sleep.
    wake_after_sleep: bool,
    /// Starts that failed in a row, and when the next try is due.
    failures: u32,
    retry_at_ms: Option<i64>,
    always_on: bool,
    /// Awake time is metered up to here (while awake).
    metered_to_ms: i64,
    /// The running start came up (it was ready): its end is a life's end.
    /// A lifecycle stored before this field reads `false`, so a computer
    /// awake across that deploy reports no end for that one life.
    #[serde(default)]
    ready: bool,
    /// This wake already fell back from a snapshot that would not start:
    /// a second fallback before a start comes up is a failure like any.
    #[serde(default)]
    fell_back: bool,
    /// The save under way, awake or a sleep's. A lifecycle stored before
    /// saves were its own reads none: a sleep then under way is in its stop.
    #[serde(default)]
    saving: Option<Saving>,
    /// The sleep under way is its owner's: no keepalive cancels it.
    #[serde(default)]
    owner_sleep: bool,
    /// A save is asked for at this time: the settle after work ended, or a
    /// failed save's next try.
    #[serde(default)]
    save_due_ms: Option<i64>,
    /// Since when a keepalive has stayed open while awake (none while idle).
    #[serde(default)]
    busy_since_ms: Option<i64>,
    /// When the last save of this life was asked for (its start, before any).
    #[serde(default)]
    save_asked_ms: i64,
    /// The last save's number (`Saving::Save`'s `seq`): a late answer to an
    /// earlier one changes nothing.
    #[serde(default)]
    save_seq: u64,
    /// Saves of this life that failed in a row (the retry's pause grows).
    #[serde(default)]
    save_failures: u32,
    /// A sleep's save has failed since then, and no save has worked since:
    /// it stays awake, within `Rules::unsaved_max_ms` of this.
    #[serde(default)]
    unsaved_since_ms: Option<i64>,
    /// Starts of this wake that fell back from a save that would not
    /// restore (at most `SAVES_KEPT`: each is from an older save).
    #[serde(default)]
    restore_fallbacks: u32,
}

impl Default for Lifecycle {
    fn default() -> Lifecycle {
        Lifecycle::new()
    }
}

impl Lifecycle {
    pub fn new() -> Lifecycle {
        Lifecycle {
            phase: Phase::Asleep,
            generation: 0,
            keepalives: 0,
            tabs: 0,
            held_until_ms: 0,
            wake_after_sleep: false,
            failures: 0,
            retry_at_ms: None,
            always_on: false,
            metered_to_ms: 0,
            ready: false,
            fell_back: false,
            saving: None,
            owner_sleep: false,
            save_due_ms: None,
            busy_since_ms: None,
            save_asked_ms: 0,
            save_seq: 0,
            save_failures: 0,
            unsaved_since_ms: None,
            restore_fallbacks: 0,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The running start, when one is running (starting, awake, or going
    /// to sleep): the start a container that runs now belongs to.
    pub fn running(&self) -> Option<u64> {
        match self.phase {
            Phase::Starting { generation, .. } | Phase::Awake { generation, .. } | Phase::Sleeping { generation, .. } => Some(generation),
            Phase::Asleep | Phase::Failed { .. } => None,
        }
    }

    /// Whether something open keeps it awake.
    pub fn open(&self) -> bool {
        self.keepalives > 0 || self.tabs > 0
    }

    pub fn held_until_ms(&self) -> i64 {
        self.held_until_ms
    }

    /// The save under way, if one is.
    pub fn saving(&self) -> Option<Saving> {
        self.saving
    }

    /// Since when a sleep's save has kept failing (it stays awake), if one has.
    pub fn unsaved_since_ms(&self) -> Option<i64> {
        self.unsaved_since_ms
    }

    fn hold(&mut self, until_ms: i64) {
        self.held_until_ms = self.held_until_ms.max(until_ms);
    }

    /// Whether anything still wants it awake at `now`.
    fn wanted(&self, now_ms: i64) -> bool {
        self.always_on || self.open() || now_ms < self.held_until_ms
    }

    fn start(&mut self, now_ms: i64, step: &mut Step) {
        self.generation += 1;
        self.retry_at_ms = None;
        self.ready = false;
        self.phase = Phase::Starting { generation: self.generation, since_ms: now_ms };
        step.actions.push(Action::Start { generation: self.generation });
    }

    /// A start came up: a life begins, saved from here on.
    fn begin_life(&mut self, now_ms: i64) {
        self.saving = None;
        self.owner_sleep = false;
        self.save_due_ms = None;
        self.busy_since_ms = (self.keepalives > 0).then_some(now_ms);
        self.save_asked_ms = now_ms;
        self.save_failures = 0;
        self.unsaved_since_ms = None;
        self.restore_fallbacks = 0;
    }

    /// The running start's container is gone: if it came up, that was a
    /// life, and it ended `by` this.
    fn end_life(&mut self, by: LifeEnd, step: &mut Step) {
        if std::mem::take(&mut self.ready) {
            assert!(step.ended.is_none(), "one event ends one life");
            let saved = by == LifeEnd::Sleep && matches!(self.saving, Some(Saving::Stop { saved: true, .. }));
            step.ended = Some(Ended { generation: self.generation, by, saved });
        }
    }

    /// Begins a sleep: its hold first (any save that was due, or under way,
    /// is the sleep's own).
    fn sleep(&mut self, generation: u64, now_ms: i64, owner: bool, step: &mut Step) {
        self.phase = Phase::Sleeping { generation, since_ms: now_ms };
        self.owner_sleep = owner;
        self.saving = Some(Saving::Hold { since_ms: now_ms });
        self.save_due_ms = None;
        self.busy_since_ms = None;
        self.save_asked_ms = now_ms;
        step.actions.push(Action::Sleep { generation });
    }

    /// Begins a save while it stays awake: its hold first.
    fn hold_for_save(&mut self, generation: u64, now_ms: i64, step: &mut Step) {
        assert!(matches!(self.phase, Phase::Awake { .. }) && self.saving.is_none(), "an awake save begins with none under way");
        self.saving = Some(Saving::Hold { since_ms: now_ms });
        self.save_due_ms = None;
        self.save_asked_ms = now_ms;
        step.actions.push(Action::Hold { generation });
    }

    /// The hold answered (or its wait ran out): the save itself.
    fn save(&mut self, generation: u64, held: bool, now_ms: i64, step: &mut Step) {
        self.save_seq += 1;
        self.saving = Some(Saving::Save { since_ms: now_ms, held, seq: self.save_seq });
        step.actions.push(Action::Save { generation, held, seq: self.save_seq });
    }

    /// A save worked: nothing of this life is unsaved since it was asked.
    fn saved(&mut self, step: &mut Step) {
        self.save_failures = 0;
        if self.unsaved_since_ms.take().is_some() {
            step.note = SaveNote::Clear;
        }
    }

    /// The pause before the next try of a save that failed `save_failures`
    /// times in a row.
    fn retry_pause_ms(&self) -> i64 {
        assert!(self.save_failures > 0, "a retry follows a failure");
        (SAVE_RETRY_MS << (self.save_failures - 1).min(8)).min(SAVE_EVERY_MS)
    }

    /// A sleep's save failed: within the bound it keeps its container (I5),
    /// awake again and held no more, and its sleep is tried again after a
    /// pause; past it, it sleeps unsaved, and says so.
    fn sleep_save_failed(&mut self, generation: u64, why: &str, now_ms: i64, rules: &Rules, step: &mut Step) {
        self.save_failures += 1;
        let since = *self.unsaved_since_ms.get_or_insert(now_ms);
        if now_ms - since < rules.unsaved_max_ms {
            self.phase = Phase::Awake { generation, since_ms: now_ms };
            self.saving = None;
            self.owner_sleep = false;
            self.save_due_ms = Some(now_ms + self.retry_pause_ms());
            step.actions.push(Action::Unhold { generation });
            step.note = SaveNote::Says(format!("its sleep could not save /data ({why}): it stays awake and tries again, for up to {} minutes", rules.unsaved_max_ms / 60_000));
        } else {
            self.unsaved_since_ms = None;
            self.saving = Some(Saving::Stop { since_ms: now_ms, saved: false });
            step.actions.push(Action::Stop { generation, saved: false });
            step.note = SaveNote::Says(format!(
                "it slept unsaved: its saves failed for {} minutes ({why}), so its next wake goes back to its last save",
                rules.unsaved_max_ms / 60_000
            ));
        }
    }

    /// A step of the save under way that ran past its deadline (its
    /// isolate died with it, say) is asked for again.
    fn ask_again(&mut self, generation: u64, saving: Saving, now_ms: i64, step: &mut Step) {
        let sleeping = matches!(self.phase, Phase::Sleeping { .. });
        match saving {
            Saving::Hold { .. } => {
                self.saving = Some(Saving::Hold { since_ms: now_ms });
                step.actions.push(if sleeping { Action::Sleep { generation } } else { Action::Hold { generation } });
            }
            Saving::Save { held, .. } => self.save(generation, held, now_ms, step),
            Saving::Stop { saved, .. } => {
                assert!(sleeping, "only a sleep stops");
                self.saving = Some(Saving::Stop { since_ms: now_ms, saved });
                step.actions.push(Action::Stop { generation, saved });
            }
        }
    }

    /// When it would sleep for want of anything holding it, if nothing
    /// opens: when its holds run out, or, while a sleep's failed save
    /// pauses before its next try, at that try.
    fn idle_at_ms(&self) -> Option<i64> {
        if self.always_on || self.open() {
            return None;
        }
        match (self.unsaved_since_ms, self.save_due_ms) {
            (Some(_), Some(due)) => Some(self.held_until_ms.max(due)),
            _ => Some(self.held_until_ms),
        }
    }

    /// When a save is due because a keepalive has stayed open: every
    /// `SAVE_EVERY_MS` of being busy, counted from the last save asked for.
    fn busy_save_due_ms(&self) -> Option<i64> {
        let since = self.busy_since_ms.filter(|_| self.keepalives > 0)?;
        Some(since.max(self.save_asked_ms) + SAVE_EVERY_MS)
    }

    /// Awake time up to `now`, once.
    fn meter(&mut self, now_ms: i64, step: &mut Step) {
        if now_ms > self.metered_to_ms {
            if !self.always_on {
                step.actions.push(Action::Meter { from_ms: self.metered_to_ms, to_ms: now_ms });
            }
            self.metered_to_ms = now_ms;
        }
    }

    /// A start that did not come up: tried again after a pause, or given
    /// up on.
    fn failed(&mut self, why: String, now_ms: i64) {
        self.failures += 1;
        if self.failures >= STARTS_FAILED_MAX {
            self.phase = Phase::Failed { why };
            self.retry_at_ms = None;
            self.wake_after_sleep = false;
        } else {
            self.phase = Phase::Asleep;
            self.retry_at_ms = Some(now_ms + (RETRY_PAUSE_MS << (self.failures - 1)));
        }
    }

    /// The container is gone: meter what it was awake, and start it again
    /// if something still wants it. Every socket into it went with it (a
    /// tab that comes back opens again, and wakes it), and so did any save
    /// under way.
    fn gone(&mut self, now_ms: i64, step: &mut Step) {
        self.meter(now_ms, step);
        self.phase = Phase::Asleep;
        self.keepalives = 0;
        self.tabs = 0;
        self.saving = None;
        self.owner_sleep = false;
        self.save_due_ms = None;
        self.busy_since_ms = None;
        self.unsaved_since_ms = None;
        if std::mem::take(&mut self.wake_after_sleep) || self.wanted(now_ms) {
            self.start(now_ms, step);
        }
    }

    fn next_alarm(&self) -> Option<i64> {
        match &self.phase {
            Phase::Asleep => self.retry_at_ms,
            Phase::Failed { .. } => None,
            Phase::Starting { since_ms, .. } => Some(since_ms + START_DEADLINE_MS),
            Phase::Sleeping { since_ms, .. } => Some(self.saving.unwrap_or(Saving::Stop { since_ms: *since_ms, saved: false }).deadline_ms()),
            Phase::Awake { .. } => {
                let meter = self.metered_to_ms + METER_EVERY_MS;
                let mut at = match self.idle_at_ms() {
                    Some(idle) => meter.min(idle),
                    None => meter,
                };
                match self.saving {
                    Some(s) => at = at.min(s.deadline_ms()),
                    None => {
                        if let Some(due) = self.save_due_ms.filter(|_| self.keepalives == 0) {
                            at = at.min(due);
                        }
                        if let Some(due) = self.busy_save_due_ms() {
                            at = at.min(due);
                        }
                    }
                }
                Some(at)
            }
        }
    }

    /// Applies `event` at `now_ms`, under the default rules.
    pub fn apply(&mut self, event: Event, now_ms: i64) -> Step {
        self.apply_with(event, now_ms, &Rules::default())
    }

    /// Applies `event` at `now_ms`, under the deployment's `rules`.
    pub fn apply_with(&mut self, event: Event, now_ms: i64, rules: &Rules) -> Step {
        assert!(rules.unsaved_max_ms >= 0, "a bound is a span of time");
        let mut step = Step::default();
        match (self.phase.clone(), event) {
            (phase, Event::Wake { why }) => {
                match phase {
                    Phase::Failed { why: failed } if why != Wake::Owner => {
                        step.refused = Some(format!("it won't wake: {failed} (its owner can try again)"));
                    }
                    Phase::Failed { .. } | Phase::Asleep => {
                        self.failures = 0;
                        self.restore_fallbacks = 0;
                        self.hold(now_ms + why.hold_ms());
                        self.start(now_ms, &mut step);
                    }
                    Phase::Sleeping { .. } => {
                        self.hold(now_ms + why.hold_ms());
                        self.wake_after_sleep = true;
                    }
                    Phase::Starting { .. } | Phase::Awake { .. } => self.hold(now_ms + why.hold_ms()),
                }
            }
            (Phase::Starting { generation, .. }, Event::Ready { generation: g }) if g == generation => {
                self.failures = 0;
                self.fell_back = false;
                self.ready = true;
                self.metered_to_ms = now_ms;
                self.phase = Phase::Awake { generation, since_ms: now_ms };
                self.begin_life(now_ms);
            }
            (Phase::Starting { generation, .. }, Event::StartFailed { generation: g, why }) if g == generation => self.failed(why, now_ms),
            (Phase::Starting { generation, .. }, Event::SnapshotFailed { generation: g, why }) if g == generation => {
                if std::mem::replace(&mut self.fell_back, true) {
                    // its snapshot went at the first: this one is the image's
                    self.failed(why, now_ms);
                } else {
                    // never awake: nothing since it last was is metered, and
                    // it starts again (from the image and the save) while wanted
                    self.metered_to_ms = now_ms;
                    self.gone(now_ms, &mut step);
                }
            }
            (Phase::Starting { generation, .. }, Event::RestoreFailed { generation: g, why, older }) if g == generation => {
                if older && (self.restore_fallbacks as usize) < SAVES_KEPT {
                    // the save was at fault, not the computer: the one
                    // before it, at once, metering nothing (it was never up)
                    self.restore_fallbacks += 1;
                    self.metered_to_ms = now_ms;
                    self.gone(now_ms, &mut step);
                } else {
                    self.failed(why, now_ms);
                }
            }
            // a completion of an earlier start, or a report of one that is
            // not running: nothing it says is true of this one
            (
                _,
                Event::Ready { .. }
                | Event::StartFailed { .. }
                | Event::SnapshotFailed { .. }
                | Event::RestoreFailed { .. }
                | Event::Held { .. }
                | Event::Saved { .. }
                | Event::SaveFailed { .. }
                | Event::Asleep { .. }
                | Event::Exited { .. },
            ) if !matches!(&self.phase, Phase::Sleeping { .. } | Phase::Awake { .. } | Phase::Starting { .. }) => {}
            (Phase::Sleeping { generation, .. }, Event::Asleep { generation: g }) if g == generation => {
                self.end_life(LifeEnd::Sleep, &mut step);
                self.gone(now_ms, &mut step);
            }
            (Phase::Awake { generation, .. } | Phase::Starting { generation, .. }, Event::Exited { generation: g }) if g == generation => {
                // one that died before it was ready was never awake: nothing
                // since it last was is metered (its sleep, its start)
                if matches!(self.phase, Phase::Starting { .. }) {
                    self.metered_to_ms = now_ms;
                }
                // a guest that died busy had turns open: its next life ends
                // them, so it is held as a record holds it, and starts again
                // however long the turn ran (F11 of
                // docs/explorations/pi-durable.md)
                if self.keepalives > 0 {
                    self.hold(now_ms + IDLE_MS);
                }
                self.end_life(LifeEnd::Exit, &mut step);
                // a container that died under us counts against its starts
                self.failures += 1;
                if self.failures >= STARTS_FAILED_MAX {
                    self.meter(now_ms, &mut step);
                    self.saving = None;
                    self.save_due_ms = None;
                    self.busy_since_ms = None;
                    self.unsaved_since_ms = None;
                    self.phase = Phase::Failed { why: "its container kept stopping".into() };
                } else {
                    self.gone(now_ms, &mut step);
                }
            }
            (Phase::Sleeping { generation, .. }, Event::Exited { generation: g }) if g == generation => {
                // its sleep ended it (signalled after the save, or dead
                // during it: whether that sleep saved is `Saves`' to know)
                self.end_life(LifeEnd::Sleep, &mut step);
                self.gone(now_ms, &mut step);
            }
            (Phase::Awake { generation, .. } | Phase::Sleeping { generation, .. }, Event::Held { generation: g, held }) if g == generation && matches!(self.saving, Some(Saving::Hold { .. })) => {
                self.save(generation, held, now_ms, &mut step);
            }
            (Phase::Awake { generation, .. }, Event::Saved { generation: g, seq }) if g == generation && matches!(self.saving, Some(Saving::Save { seq: s, .. }) if s == seq) => {
                self.saving = None;
                self.saved(&mut step);
                step.actions.push(Action::Unhold { generation });
            }
            (Phase::Sleeping { generation, .. }, Event::Saved { generation: g, seq }) if g == generation && matches!(self.saving, Some(Saving::Save { seq: s, .. }) if s == seq) => {
                self.saved(&mut step);
                self.saving = Some(Saving::Stop { since_ms: now_ms, saved: true });
                step.actions.push(Action::Stop { generation, saved: true });
            }
            (Phase::Awake { generation, .. }, Event::SaveFailed { generation: g, seq, .. }) if g == generation && self.answers(seq) => {
                // awake, it is tried again after a pause; a sleep saves anyway
                self.saving = None;
                self.save_failures += 1;
                self.save_due_ms = Some(now_ms + self.retry_pause_ms());
                step.actions.push(Action::Unhold { generation });
            }
            (Phase::Sleeping { generation, .. }, Event::SaveFailed { generation: g, seq, why }) if g == generation && self.answers(seq) => {
                self.sleep_save_failed(generation, &why, now_ms, rules, &mut step);
            }
            (
                _,
                Event::Ready { .. }
                | Event::StartFailed { .. }
                | Event::SnapshotFailed { .. }
                | Event::RestoreFailed { .. }
                | Event::Held { .. }
                | Event::Saved { .. }
                | Event::SaveFailed { .. }
                | Event::Asleep { .. }
                | Event::Exited { .. },
            ) => {}
            (_, Event::Opened { socket }) => {
                match socket {
                    Socket::Keepalive => self.keepalives += 1,
                    Socket::Tab => self.tabs += 1,
                }
                match self.phase {
                    Phase::Asleep => {
                        self.hold(now_ms + IDLE_MS);
                        self.start(now_ms, &mut step);
                    }
                    Phase::Awake { .. } if socket == Socket::Keepalive => {
                        if self.keepalives == 1 {
                            self.busy_since_ms = Some(now_ms);
                        }
                        // work began again: its end is what saves
                        self.save_due_ms = None;
                    }
                    Phase::Sleeping { generation, .. } if socket == Socket::Keepalive && !self.owner_sleep && matches!(self.saving, Some(Saving::Hold { .. })) => {
                        // the guest took work as its idle sleep held it: it
                        // stays awake, and nothing it claimed is cut (P2)
                        self.phase = Phase::Awake { generation, since_ms: now_ms };
                        self.saving = None;
                        self.wake_after_sleep = false;
                        self.busy_since_ms = Some(now_ms);
                        step.actions.push(Action::Unhold { generation });
                    }
                    _ => {}
                }
            }
            (_, Event::Closed { socket }) => {
                let count = match socket {
                    Socket::Keepalive => &mut self.keepalives,
                    Socket::Tab => &mut self.tabs,
                };
                let was = *count;
                *count = count.saturating_sub(1);
                // a socket that closes as its container goes down (or after)
                // holds nothing up
                if matches!(self.phase, Phase::Starting { .. } | Phase::Awake { .. }) {
                    self.hold(now_ms + IDLE_MS);
                }
                if socket == Socket::Keepalive && was == 1 && matches!(self.phase, Phase::Awake { .. }) {
                    // work ended: it is saved once it settles
                    self.busy_since_ms = None;
                    self.save_due_ms = Some(now_ms + SAVE_SETTLE_MS);
                }
            }
            (Phase::Starting { generation, .. } | Phase::Awake { generation, .. }, Event::Sleep) => {
                self.held_until_ms = now_ms;
                self.wake_after_sleep = false;
                self.keepalives = 0;
                self.tabs = 0;
                self.sleep(generation, now_ms, true, &mut step);
            }
            (Phase::Sleeping { .. }, Event::Sleep) => {
                self.wake_after_sleep = false;
                self.owner_sleep = true;
            }
            (Phase::Asleep | Phase::Failed { .. }, Event::Sleep) => {}
            (_, Event::AlwaysOn { on }) => {
                self.always_on = on;
                if on && matches!(self.phase, Phase::Asleep) {
                    self.start(now_ms, &mut step);
                }
            }
            (Phase::Asleep, Event::Alarm) => {
                if self.retry_at_ms.is_some_and(|at| now_ms >= at) {
                    if self.wanted(now_ms) {
                        self.start(now_ms, &mut step);
                    } else {
                        self.retry_at_ms = None;
                    }
                }
            }
            (Phase::Starting { generation, since_ms }, Event::Alarm) => {
                if now_ms >= since_ms + START_DEADLINE_MS {
                    self.failed(format!("start {generation} was not ready within {}s", START_DEADLINE_MS / 1000), now_ms);
                }
            }
            (Phase::Awake { generation, .. }, Event::Alarm) => {
                self.meter(now_ms, &mut step);
                if self.idle_at_ms().is_some_and(|idle| now_ms >= idle) {
                    // a sleep saves: any save that was due, or under way (its
                    // step lost with an isolate), is the sleep's own
                    self.sleep(generation, now_ms, false, &mut step);
                } else if let Some(s) = self.saving {
                    if now_ms >= s.deadline_ms() {
                        self.ask_again(generation, s, now_ms, &mut step);
                    }
                } else {
                    let settled = self.keepalives == 0 && self.save_due_ms.is_some_and(|due| now_ms >= due);
                    let busy = self.busy_save_due_ms().is_some_and(|due| now_ms >= due);
                    if settled || busy {
                        self.hold_for_save(generation, now_ms, &mut step);
                    }
                }
            }
            (Phase::Sleeping { generation, since_ms }, Event::Alarm) => {
                let s = self.saving.unwrap_or(Saving::Stop { since_ms, saved: false });
                if now_ms >= s.deadline_ms() {
                    self.ask_again(generation, s, now_ms, &mut step);
                }
            }
            (Phase::Failed { .. }, Event::Alarm) => {}
        }
        step.alarm_ms = self.next_alarm();
        self.assert_valid();
        step
    }

    /// Whether a save's failure `seq` answers the save under way: its own
    /// number, or 0 from its hold (which found no container to save).
    fn answers(&self, seq: u64) -> bool {
        match self.saving {
            Some(Saving::Save { seq: s, .. }) => s == seq,
            Some(Saving::Hold { .. }) => seq == 0,
            Some(Saving::Stop { .. }) | None => false,
        }
    }

    fn assert_valid(&self) {
        match &self.phase {
            Phase::Starting { generation, .. } | Phase::Awake { generation, .. } | Phase::Sleeping { generation, .. } => {
                assert_eq!(*generation, self.generation, "the running phase is the last start's");
            }
            Phase::Asleep => {}
            Phase::Failed { .. } => assert!(self.retry_at_ms.is_none(), "a computer that won't wake has no retry due"),
        }
        assert!(self.failures <= STARTS_FAILED_MAX, "failures stop counting at the limit");
        if self.ready {
            assert!(matches!(self.phase, Phase::Awake { .. } | Phase::Sleeping { .. }), "only a start that came up, and is not gone, is ready");
        }
        match self.saving {
            Some(Saving::Stop { .. }) => assert!(matches!(self.phase, Phase::Sleeping { .. }), "only a sleep stops"),
            Some(_) => assert!(matches!(self.phase, Phase::Awake { .. } | Phase::Sleeping { .. }), "a save is of a container that came up"),
            None => {}
        }
        if self.save_due_ms.is_some() || self.busy_since_ms.is_some() {
            assert!(matches!(self.phase, Phase::Awake { .. }), "only an awake computer has a save due");
        }
        if self.owner_sleep {
            assert!(matches!(self.phase, Phase::Sleeping { .. }), "an owner's sleep is a sleep");
        }
        if self.unsaved_since_ms.is_some() {
            assert!(matches!(self.phase, Phase::Awake { .. } | Phase::Sleeping { .. }), "a failed save keeps a running container");
        }
        assert!(self.restore_fallbacks as usize <= SAVES_KEPT, "each fallback is to an older save");
    }
}

/// Records one save holds at most: one per directory it saves.
pub const SAVE_RECORDS_MAX: usize = 4;
/// Records let go of and not deleted yet, at most (`Saves::let_go`): a few
/// saves' worth, since deletes that keep failing while saves work are not
/// expected (both are R2's). Past it the oldest is no longer tried, its
/// archive left in R2, as the DO logs.
pub const FORGETTING_MAX: usize = 4 * SAVE_RECORDS_MAX;
/// A guest's answer to the hold is at most this long, and names at most
/// `LEFT_OUT_MAX` patterns of at most `LEFT_OUT_PATTERN_MAX_BYTES` each:
/// room for an image that names each file it copied (ours: four lines a
/// database, for up to 128 databases).
pub const HELD_ANSWER_MAX_BYTES: usize = 64 * 1024;
pub const LEFT_OUT_MAX: usize = 512;
pub const LEFT_OUT_PATTERN_MAX_BYTES: usize = 256;
const _: () = assert!(LEFT_OUT_MAX * 2 <= HELD_ANSWER_MAX_BYTES, "the answer's bound fits its patterns' count");

/// What a guest's answer to the hold (`/run/computer/held`, docs/computers.md)
/// names as left out of its save: one gitignore pattern a line, relative to
/// `/data`, of letters, digits and `._-/*?[]` (empty lines skipped). These
/// are files the guest copied under the hold to names the save keeps (our
/// Hermes image: its SQLite databases, by SQLite's online backup), so a
/// hot copy of them, which could tear, is never what a wake restores. An
/// answer past its bounds, or with any other line, is refused: the guest is
/// then saved whole, losing nothing.
pub fn left_out(answer: &str) -> Result<Vec<String>, String> {
    if answer.len() > HELD_ANSWER_MAX_BYTES {
        return Err(format!("an answer of {} bytes, past {HELD_ANSWER_MAX_BYTES}", answer.len()));
    }
    let mut patterns = Vec::new();
    for line in answer.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let ok = line.len() <= LEFT_OUT_PATTERN_MAX_BYTES && line.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/*?[]".contains(&b));
        if !ok {
            return Err(format!("{line:?} is no pattern of letters, digits and ._-/*?[] of at most {LEFT_OUT_PATTERN_MAX_BYTES} bytes"));
        }
        patterns.push(line.to_string());
    }
    if patterns.len() > LEFT_OUT_MAX {
        return Err(format!("{} patterns, past {LEFT_OUT_MAX}", patterns.len()));
    }
    Ok(patterns)
}

/// What the DO keeps of a guest's word on a hold it has not answered.
pub const UNHELD_MAX_BYTES: usize = 1024;

/// What a guest says of a hold it has not answered (`/run/computer/unheld`,
/// docs/computers.md, "The hold"), as the Computer DO logs it once the
/// hold's wait runs out: its text, trimmed, its control characters but
/// newlines as spaces, at most `UNHELD_MAX_BYTES` (cut at a character);
/// `None` for none. Only a log line: nothing is decided by it.
pub fn unheld(text: &str) -> Option<String> {
    let clean: String = text.trim().chars().map(|c| if c.is_control() && c != '\n' { ' ' } else { c }).collect();
    let mut end = clean.len().min(UNHELD_MAX_BYTES);
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    let cut = clean[..end].trim_end();
    (!cut.is_empty()).then(|| cut.to_string())
}

/// A save of `/data`, as the Computer DO keeps it: the `DirectoryBackup`
/// records it was taken as (the authority on what a wake restores, handed
/// back to restore and to delete them), and what the platform knows of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Save {
    /// 1 for a computer's first save, one more for each after it.
    pub number: u64,
    /// Its first record's id (a UUID): what a snapshot names as its save,
    /// and what a wake says it restored.
    pub id: String,
    /// The start it is of.
    pub generation: u64,
    pub at_ms: i64,
    /// Whether the guest answered the hold before it was taken.
    pub held: bool,
    /// Its `DirectoryBackup` records, a directory's before any inside it
    /// (the order they restore in).
    pub records: Vec<Value>,
    /// Its restore failed as a whole save (an archive gone or corrupt), or
    /// the image's check of what it restored did: no start uses it again.
    #[serde(default)]
    pub unusable: bool,
}

impl Save {
    pub fn view(&self) -> ComputerSave {
        ComputerSave { number: self.number, id: self.id.clone(), at: self.at_ms, generation: self.generation, held: self.held, unusable: self.unusable }
    }
}

/// A container snapshot: a cache of one save, for one image (P3 of
/// docs/explorations/pi-durable.md). A wake uses it only while `save` is
/// the current save and `image` the pinned image's reference, so it only
/// ever makes a wake faster, never different.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub id: String,
    /// The image's reference it was taken of (a name can stand for another
    /// image after a redeploy).
    pub image: String,
    /// The id of the save it was taken with, in the same sleep.
    pub save: String,
}

/// What a start restores, chosen as it begins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// The snapshot `id`, of the current save.
    Snapshot { id: String },
    /// The image, then the current save.
    Backup,
    /// The image, with an empty `/data`: there is no save.
    Nothing,
}

impl Plan {
    pub fn source(&self) -> RestoreSource {
        match self {
            Plan::Snapshot { .. } => RestoreSource::Snapshot,
            Plan::Backup => RestoreSource::Backup,
            Plan::Nothing => RestoreSource::Nothing,
        }
    }
}

/// The start under way, and what it restores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Starting {
    pub generation: u64,
    pub from: RestoreSource,
}

/// What the Computer DO knows of its saves, kept whole beside its
/// `Lifecycle`. Each method is one fact the DO learned, applied as it
/// happened; nothing here reaches the container. A record stored before it
/// kept several saves (its one `backup`) is not read: a hard cut, so a
/// computer saved before then wakes with an empty `/data`, its agents'
/// selves coming back from their repos.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saves {
    /// The saves kept, newest first, at most `SAVES_KEPT`.
    #[serde(default)]
    saves: Vec<Save>,
    /// The last save's number.
    #[serde(default)]
    numbered: u64,
    /// The snapshot of the current save, when its sleep took one.
    snapshot: Option<Snapshot>,
    /// The start under way: an exit before it comes up is its own failure.
    starting: Option<Starting>,
    /// How the last life ended, until the next start that comes up reads it.
    ended: Option<Ended>,
    /// What the last start that came up restored.
    restored: Option<ComputerRestore>,
    /// The image the last start that came up runs: what a snapshot its
    /// sleep takes is of (a pin while it runs changes the next start's).
    running: Option<Running>,
    /// Starts that went back in time.
    rollbacks: u64,
    /// Records no save holds now that the DO has not deleted yet, oldest
    /// first: each stays until its delete worked (`forgot`), so one that
    /// failed is tried again by the next.
    #[serde(default)]
    forgetting: Vec<Value>,
}

/// A start that came up, and the image's reference it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    pub generation: u64,
    pub image: String,
}

impl Saves {
    /// The saves kept, newest first.
    pub fn all(&self) -> &[Save] {
        &self.saves
    }

    /// The save a start restores: the newest one not found unusable (or,
    /// with every one found so, the newest, tried again as any start is).
    pub fn current(&self) -> Option<&Save> {
        self.saves.iter().find(|s| !s.unusable).or(self.saves.first())
    }

    /// What a start restores: the snapshot only when it caches the current
    /// save for the pinned image (`image`, its reference: `None` with
    /// snapshots off); otherwise the image and the current save, or
    /// nothing when there is no save.
    pub fn plan(&self, image: Option<&str>) -> Plan {
        let Some(save) = self.current() else { return Plan::Nothing };
        match (&self.snapshot, image) {
            (Some(s), Some(image)) if s.save == save.id && s.image == image && !save.unusable => Plan::Snapshot { id: s.id.clone() },
            _ => Plan::Backup,
        }
    }

    /// The start `generation` begins, restoring `from`. A start older than
    /// the one recorded was superseded before it began: it records nothing.
    pub fn starting(&mut self, generation: u64, from: RestoreSource) {
        if self.starting.is_none_or(|s| s.generation <= generation) {
            self.starting = Some(Starting { generation, from });
        }
    }

    /// What the start `generation` under way restores, if it is the one.
    pub fn start_from(&self, generation: u64) -> Option<RestoreSource> {
        self.starting.filter(|s| s.generation == generation).map(|s| s.from)
    }

    /// A start from the snapshot failed: the snapshot is forgotten (it is
    /// only a cache), so the next start is from the image and the save
    /// (F7). Answers the snapshot forgotten.
    pub fn snapshot_failed(&mut self) -> Option<Snapshot> {
        self.snapshot.take()
    }

    /// The save `id` would not restore as a whole (F5): no start restores
    /// it again, nor a snapshot of it. Answers whether a save is left to
    /// try, which is older (each newer one was found unusable first).
    pub fn unusable(&mut self, id: &str) -> bool {
        if let Some(s) = self.saves.iter_mut().find(|s| s.id == id) {
            s.unusable = true;
        }
        if self.snapshot.as_ref().is_some_and(|s| s.save == id) {
            self.snapshot = None;
        }
        let left = self.saves.iter().any(|s| !s.unusable);
        assert!(!left || self.current().is_some_and(|c| c.id != id), "the save found unusable is never the current one while another is left");
        left
    }

    /// A life ended (the lifecycle's `Step::ended`).
    pub fn ended(&mut self, ended: Ended) {
        assert!(self.ended.is_none_or(|e| e.generation < ended.generation), "a life ends once, after the lives before it");
        self.ended = Some(ended);
    }

    /// The start `generation` came up on `image` (its reference), having
    /// restored `from` (`save`: the id of the save it restored): what it
    /// restored, and whether that went back in time, which is counted.
    /// `None` when another start began meanwhile (this one was superseded:
    /// nothing is recorded).
    pub fn came_up(&mut self, generation: u64, from: RestoreSource, save: Option<&str>, image: Option<&str>, now_ms: i64) -> Option<ComputerRestore> {
        if self.starting != Some(Starting { generation, from }) {
            return None;
        }
        self.starting = None;
        self.running = image.map(|image| Running { generation, image: image.to_string() });
        assert_eq!(from == RestoreSource::Nothing, save.is_none(), "a start restores a save when there is one");
        let restored_save = save.and_then(|id| self.saves.iter().find(|b| b.id == id));
        let ended = self.ended.take();
        // a life's work since its last save is in no save unless its sleep
        // saved it, and this start restored that save (its life's newest):
        // a crash loses it, and so does a sleep that slept unsaved
        let rollback = match ended {
            None => false,
            Some(Ended { by: LifeEnd::Exit, .. }) => true,
            Some(Ended { by: LifeEnd::Sleep, generation: life, saved }) => {
                let newest_of_life = self.saves.iter().find(|s| s.generation == life).map(|s| s.id.as_str());
                !(saved && restored_save.is_some_and(|s| s.generation == life) && newest_of_life == save)
            }
        };
        let restored = ComputerRestore {
            generation,
            from,
            save: save.map(str::to_string),
            saved_at: restored_save.map(|s| s.at_ms),
            age_ms: restored_save.map(|s| now_ms.saturating_sub(s.at_ms).max(0)),
            after: ended.map(|e| e.by),
            rollback,
            at: now_ms,
        };
        self.rollbacks += u64::from(rollback);
        self.restored = Some(restored.clone());
        Some(restored)
    }

    /// The start `generation` saved `/data` as `records` at `at_ms`
    /// (`held`: its guest answered the hold): the newest save, and the
    /// current one. A snapshot of any other save caches nothing a wake
    /// would use. The saves it pushed out of the newest `SAVES_KEPT` are
    /// let go of (`let_go`): answers what that pushed out unforgotten.
    pub fn saved(&mut self, generation: u64, records: Vec<Value>, at_ms: i64, held: bool) -> Vec<Value> {
        assert!(!records.is_empty() && records.len() <= SAVE_RECORDS_MAX, "a save is one record per directory, at most {SAVE_RECORDS_MAX}");
        let id = records[0]["id"].as_str().unwrap_or_default().to_string();
        assert!(!id.is_empty(), "a save has an id");
        let depth = |r: &Value| r["dir"].as_str().map_or(0, |d| d.matches('/').count());
        assert!(records.windows(2).all(|w| depth(&w[0]) <= depth(&w[1])), "a directory's record before any inside it: {records:?}");
        assert!(!self.saves.iter().any(|s| s.id == id), "a save is kept once");
        if self.snapshot.as_ref().is_some_and(|s| s.save != id) {
            self.snapshot = None;
        }
        self.numbered += 1;
        self.saves.insert(0, Save { number: self.numbered, id, generation, at_ms, held, records, unusable: false });
        let dropped = if self.saves.len() > SAVES_KEPT { self.saves.split_off(SAVES_KEPT) } else { vec![] };
        assert!(self.saves.windows(2).all(|w| w[0].number > w[1].number), "newest first");
        assert!(self.snapshot.as_ref().is_none_or(|s| self.saves[0].id == s.save), "a snapshot kept is of the current save");
        self.let_go(dropped.into_iter().flat_map(|s| s.records).collect())
    }

    /// `records` are no save's now (a save pushed out, or what a failed
    /// save took before it failed): the DO deletes each (`forgetting`),
    /// and each is kept here until that worked (`forgot`). Answers those
    /// pushed past `FORGETTING_MAX`, oldest first, which no one will delete.
    pub fn let_go(&mut self, records: Vec<Value>) -> Vec<Value> {
        assert!(records.iter().all(|r| r["id"].as_str().is_some_and(|id| !id.is_empty())), "a record has an id: {records:?}");
        assert!(!records.iter().any(|r| self.saves.iter().any(|s| s.records.contains(r))), "a record let go of is no kept save's");
        self.forgetting.extend(records);
        let over = self.forgetting.len().saturating_sub(FORGETTING_MAX);
        let lost: Vec<Value> = self.forgetting.drain(..over).collect();
        assert!(self.forgetting.len() <= FORGETTING_MAX, "at most FORGETTING_MAX records to delete");
        lost
    }

    /// The records let go of and not deleted yet, oldest first.
    pub fn forgetting(&self) -> &[Value] {
        &self.forgetting
    }

    /// The record `id` was deleted.
    pub fn forgot(&mut self, id: &str) {
        self.forgetting.retain(|r| r["id"] != id);
    }

    /// A sleep of `generation` could not save: whether that is news (no
    /// save of this start is kept). A slow sleep is asked for again
    /// (`SLEEP_DEADLINE_MS`), and the second ask's save fails once the
    /// first's destroy took the container: the first one saved.
    pub fn save_failed(&self, generation: u64) -> bool {
        !self.saves.iter().any(|b| b.generation == generation)
    }

    /// A sleep took snapshot `id` of `image` after its save `save`: kept
    /// when `save` is still the newest save (a second ask's save may have
    /// replaced it meanwhile). A sleep whose own save failed takes none,
    /// and a snapshot that fails forgets nothing: the one kept is still a
    /// cache of its own save (F11).
    pub fn snapshotted(&mut self, save: &str, id: &str, image: &str) -> bool {
        let current = self.saves.first().is_some_and(|b| b.id == save);
        if current {
            self.snapshot = Some(Snapshot { id: id.to_string(), image: image.to_string(), save: save.to_string() });
        }
        current
    }

    /// The image's reference the start `generation` came up on, if it did.
    pub fn image_of(&self, generation: u64) -> Option<&str> {
        self.running.as_ref().filter(|r| r.generation == generation).map(|r| r.image.as_str())
    }

    pub fn restored(&self) -> Option<&ComputerRestore> {
        self.restored.as_ref()
    }

    pub fn rollbacks(&self) -> u64 {
        self.rollbacks
    }

    /// The saves, as the computer's view shows them.
    pub fn views(&self) -> Vec<ComputerSave> {
        self.saves.iter().map(Save::view).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T: i64 = 1_800_000_000_000;

    #[test]
    fn a_persons_computer_has_one_stable_id() {
        let a = default_computer_of("id:aaaa");
        assert_eq!(a, default_computer_of("id:aaaa"));
        assert_ne!(a, default_computer_of("id:aaab"));
        assert!(a.starts_with("computer:") && a.len() == "computer:".len() + 24 && a[9..].bytes().all(|b| b.is_ascii_hexdigit()));
    }

    /// Goal: a person's page pre-wakes every computer that follows the
    /// fragment, each once; a computer's own agent's socket (its guest
    /// following the fragment, as it boots or after a drop, also while it
    /// goes to sleep) pre-wakes none of its own, only other computers'.
    #[test]
    fn a_page_prewakes_the_computers_but_an_agent_never_its_own() {
        let subs = [("id:juniper", "computer:a"), ("id:juniper", "computer:a"), ("id:maple", "computer:a"), ("id:oak", "computer:b")];
        assert_eq!(prewoken(&subs, "id:paul"), ["computer:a", "computer:b"], "a person's page");
        assert_eq!(prewoken(&subs, "anon:3f"), ["computer:a", "computer:b"], "a visitor's page");
        assert_eq!(prewoken(&subs, "id:juniper"), ["computer:b"], "an agent of computer a");
        assert_eq!(prewoken(&subs, "id:maple"), ["computer:b"], "another agent of the same computer");
        assert_eq!(prewoken(&subs, "id:oak"), ["computer:a"], "an agent of computer b");
        assert!(prewoken(&[], "id:paul").is_empty(), "no computer follows it");
    }

    fn started(l: &mut Lifecycle, at: i64) -> u64 {
        let s = l.apply(Event::Wake { why: Wake::Record }, at);
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("a wake starts it: {s:?}") };
        l.apply(Event::Ready { generation }, at + 3_000);
        generation
    }

    /// Goal: a record wakes a sleeping computer, which then sleeps twenty
    /// minutes after nothing holds it, metering exactly its awake time.
    #[test]
    fn a_record_wakes_it_and_idleness_puts_it_to_sleep() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        assert!(matches!(l.phase, Phase::Awake { .. }));
        let s = l.apply(Event::Alarm, T + 3_000 + METER_EVERY_MS);
        assert_eq!(s.actions, vec![Action::Meter { from_ms: T + 3_000, to_ms: T + 3_000 + METER_EVERY_MS }]);
        let s = l.apply(Event::Alarm, T + IDLE_MS);
        assert_eq!(s.actions, vec![Action::Meter { from_ms: T + 3_000 + METER_EVERY_MS, to_ms: T + IDLE_MS }, Action::Sleep { generation: g }]);
        let s = l.apply(Event::Asleep { generation: g }, T + IDLE_MS + 6_000);
        assert_eq!(s.actions, vec![Action::Meter { from_ms: T + IDLE_MS, to_ms: T + IDLE_MS + 6_000 }]);
        assert_eq!((l.phase.clone(), s.alarm_ms), (Phase::Asleep, None));
    }

    /// Goal: an open keepalive or tab keeps it awake past any hold, and
    /// closing it holds it twenty minutes more.
    #[test]
    fn open_sockets_keep_it_awake() {
        let mut l = Lifecycle::new();
        started(&mut l, T);
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 4_000);
        let s = l.apply(Event::Alarm, T + 2 * IDLE_MS);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Sleep { .. })), "{s:?}");
        l.apply(Event::Closed { socket: Socket::Keepalive }, T + 2 * IDLE_MS);
        let s = l.apply(Event::Alarm, T + 3 * IDLE_MS - 1);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Sleep { .. })));
        let s = l.apply(Event::Alarm, T + 3 * IDLE_MS);
        assert!(s.actions.iter().any(|a| matches!(a, Action::Sleep { .. })));
        // a tab opened on a sleeping computer starts it
        let mut l = Lifecycle::new();
        let s = l.apply(Event::Opened { socket: Socket::Tab }, T);
        assert!(matches!(s.actions[..], [Action::Start { generation: 1 }]));
        // a close with nothing open never underflows
        l.apply(Event::Closed { socket: Socket::Keepalive }, T);
        assert!(l.open(), "the tab is still open");
    }

    /// Goal: a pre-wake that nothing follows stops after a minute, and one
    /// a record follows holds the full twenty.
    #[test]
    fn a_prewake_stops_after_a_minute_unless_something_arrives() {
        let mut l = Lifecycle::new();
        let s = l.apply(Event::Wake { why: Wake::Presence }, T);
        let Action::Start { generation } = s.actions[0] else { panic!() };
        l.apply(Event::Ready { generation }, T + 2_000);
        let s = l.apply(Event::Alarm, T + PREWAKE_MS);
        assert!(s.actions.contains(&Action::Sleep { generation }), "{s:?}");
        let mut l = Lifecycle::new();
        l.apply(Event::Wake { why: Wake::Presence }, T);
        l.apply(Event::Ready { generation: 1 }, T + 2_000);
        l.apply(Event::Wake { why: Wake::Record }, T + 3_000);
        let s = l.apply(Event::Alarm, T + PREWAKE_MS);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Sleep { .. })));
        assert_eq!(l.held_until_ms(), T + 3_000 + IDLE_MS);
    }

    /// Goal: an agent added to a fragment wakes its sleeping computer and
    /// holds it as a record does; awake, it only holds it longer; a
    /// computer that won't wake is not started by it (only its owner lifts
    /// that).
    #[test]
    fn an_agent_joining_a_fragment_wakes_it() {
        let mut l = Lifecycle::new();
        let s = l.apply(Event::Wake { why: Wake::Joined }, T);
        assert_eq!(s.actions, vec![Action::Start { generation: 1 }]);
        l.apply(Event::Ready { generation: 1 }, T + 2_000);
        assert_eq!(l.held_until_ms(), T + IDLE_MS);
        let s = l.apply(Event::Wake { why: Wake::Joined }, T + 60_000);
        assert!(s.actions.is_empty(), "awake, a join only holds it: {s:?}");
        assert_eq!(l.held_until_ms(), T + 60_000 + IDLE_MS);
        let mut failed = Lifecycle { phase: Phase::Failed { why: "pull failed".into() }, ..Lifecycle::new() };
        let s = failed.apply(Event::Wake { why: Wake::Joined }, T);
        assert!(s.actions.is_empty() && s.refused.is_some(), "{s:?}");
    }

    /// Goal: a wake racing a sleep wins, and starts a new generation once
    /// the old one is gone; the old one's late reports change nothing.
    #[test]
    fn a_wake_during_sleep_starts_it_again_after() {
        let mut l = Lifecycle::new();
        let g1 = started(&mut l, T);
        l.apply(Event::Alarm, T + IDLE_MS);
        assert!(matches!(l.phase, Phase::Sleeping { .. }));
        assert!(l.apply(Event::Wake { why: Wake::Record }, T + IDLE_MS + 1_000).actions.is_empty());
        let s = l.apply(Event::Asleep { generation: g1 }, T + IDLE_MS + 5_000);
        assert!(s.actions.contains(&Action::Start { generation: g1 + 1 }), "{s:?}");
        // the old generation's late reports are ignored
        for e in [Event::Ready { generation: g1 }, Event::Asleep { generation: g1 }, Event::Exited { generation: g1 }, Event::StartFailed { generation: g1, why: "late".into() }] {
            let before = l.clone();
            assert!(l.apply(e, T + IDLE_MS + 6_000).actions.is_empty());
            assert_eq!(l, before);
        }
    }

    /// Goal: failed starts are retried with a growing pause, then the
    /// computer won't wake until its owner asks (lesson 4).
    #[test]
    fn failing_starts_give_up_until_the_owner_asks() {
        let mut l = Lifecycle::new();
        l.apply(Event::Wake { why: Wake::Record }, T);
        let s = l.apply(Event::StartFailed { generation: 1, why: "pull failed".into() }, T + 1_000);
        assert_eq!((l.phase.clone(), s.alarm_ms), (Phase::Asleep, Some(T + 1_000 + RETRY_PAUSE_MS)));
        let s = l.apply(Event::Alarm, T + 1_000 + RETRY_PAUSE_MS);
        assert_eq!(s.actions, vec![Action::Start { generation: 2 }]);
        // the next never reports ready: its deadline fails it
        let s = l.apply(Event::Alarm, T + 1_000 + RETRY_PAUSE_MS + START_DEADLINE_MS);
        assert_eq!(s.alarm_ms, Some(T + 1_000 + RETRY_PAUSE_MS + START_DEADLINE_MS + 2 * RETRY_PAUSE_MS));
        let at = s.alarm_ms.unwrap();
        l.apply(Event::Alarm, at);
        l.apply(Event::StartFailed { generation: 3, why: "pull failed".into() }, at + 1_000);
        assert!(matches!(&l.phase, Phase::Failed { why } if why == "pull failed"));
        let s = l.apply(Event::Wake { why: Wake::Record }, at + 2_000);
        assert!(s.actions.is_empty() && s.refused.is_some(), "a record does not wake it: {s:?}");
        let s = l.apply(Event::Wake { why: Wake::Owner }, at + 3_000);
        assert_eq!(s.actions, vec![Action::Start { generation: 4 }]);
    }

    /// Goal: a container that stops on its own is started again while it
    /// is wanted, and metered up to its stop.
    #[test]
    fn an_exit_restarts_it_while_wanted() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        let s = l.apply(Event::Exited { generation: g }, T + 60_000);
        assert_eq!(s.actions, vec![Action::Meter { from_ms: T + 3_000, to_ms: T + 60_000 }, Action::Start { generation: g + 1 }]);
        // once nothing wants it, an exit leaves it asleep
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        let s = l.apply(Event::Exited { generation: g }, T + IDLE_MS + 1);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Start { .. })));
        assert_eq!(l.phase, Phase::Asleep);
    }

    /// Goal: a container that dies while it starts, before it was ever
    /// ready, meters nothing: not the start, and not the sleep before it
    /// (the last awake interval was metered as it slept). Its start is tried
    /// again, as an exit of an awake one is, and the next awake time is
    /// metered from its ready.
    #[test]
    fn an_exit_while_starting_meters_nothing() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Sleep, T + 10_000);
        let s = l.apply(Event::Asleep { generation: g }, T + 12_000);
        assert_eq!(s.actions, vec![Action::Meter { from_ms: T + 3_000, to_ms: T + 12_000 }]);
        // asleep an hour, then woken: it dies as it starts
        let later = T + 3_600_000;
        let s = l.apply(Event::Wake { why: Wake::Owner }, later);
        let Some(Action::Start { generation: g2 }) = s.actions.first().cloned() else { panic!("a wake starts it: {s:?}") };
        let s = l.apply(Event::Exited { generation: g2 }, later + 2_000);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Meter { .. })), "nothing was awake: {s:?}");
        assert!(s.actions.contains(&Action::Start { generation: g2 + 1 }), "it is wanted, so it starts again: {s:?}");
        // the same exit again (a replay) changes nothing
        assert!(l.apply(Event::Exited { generation: g2 }, later + 2_500).actions.is_empty());
        l.apply(Event::Ready { generation: g2 + 1 }, later + 5_000);
        let s = l.apply(Event::Sleep, later + 65_000);
        assert!(s.actions.contains(&Action::Sleep { generation: g2 + 1 }));
        let s = l.apply(Event::Asleep { generation: g2 + 1 }, later + 66_000);
        assert_eq!(s.actions, vec![Action::Meter { from_ms: later + 5_000, to_ms: later + 66_000 }]);
        // its starts dying one after another: none of it metered, then it won't wake
        let mut l = Lifecycle::new();
        let mut at = T;
        let mut s = l.apply(Event::Wake { why: Wake::Owner }, at);
        for _ in 0..STARTS_FAILED_MAX {
            let Some(Action::Start { generation }) = s.actions.iter().find(|a| matches!(a, Action::Start { .. })).cloned() else { break };
            at += 1_000;
            s = l.apply(Event::Exited { generation }, at);
            assert!(!s.actions.iter().any(|a| matches!(a, Action::Meter { .. })), "{s:?}");
        }
        assert!(matches!(l.phase, Phase::Failed { .. }), "{:?}", l.phase);
    }

    /// Goal: the owner's sleep lets go of what held it, so it stays asleep
    /// until the next wake; the guest's keepalive closing as its container
    /// goes down holds nothing.
    #[test]
    fn the_owner_puts_it_to_sleep() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 4_000);
        let s = l.apply(Event::Sleep, T + 5_000);
        assert!(s.actions.contains(&Action::Sleep { generation: g }), "{s:?}");
        l.apply(Event::Closed { socket: Socket::Keepalive }, T + 6_000);
        // a guest busy again as it goes down: its socket dies with it
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 7_000);
        let s = l.apply(Event::Asleep { generation: g }, T + 9_000);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Start { .. })), "{s:?}");
        assert_eq!(l.phase, Phase::Asleep);
        assert!(l.apply(Event::Sleep, T + 10_000).actions.is_empty(), "asleep, a sleep does nothing");
    }

    /// Goal: an always-on computer never sleeps and its awake time is not
    /// metered (decision 25).
    #[test]
    fn always_on_never_sleeps_nor_meters() {
        let mut l = Lifecycle::new();
        let s = l.apply(Event::AlwaysOn { on: true }, T);
        assert_eq!(s.actions, vec![Action::Start { generation: 1 }]);
        l.apply(Event::Ready { generation: 1 }, T + 3_000);
        for i in 1..20 {
            let s = l.apply(Event::Alarm, T + i * IDLE_MS);
            assert!(s.actions.is_empty(), "{s:?}");
        }
    }

    /// Goal: the state survives a restart of its Durable Object (it is kept
    /// whole as JSON) and answers the same after.
    #[test]
    fn it_survives_a_restart() {
        let mut l = Lifecycle::new();
        started(&mut l, T);
        l.apply(Event::Opened { socket: Socket::Tab }, T + 5_000);
        let mut back: Lifecycle = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        assert_eq!(back, l);
        assert_eq!(back.apply(Event::Alarm, T + 2 * IDLE_MS), l.clone().apply(Event::Alarm, T + 2 * IDLE_MS));
        // a lifecycle stored before it knew which start came up reads as
        // one whose start did not: its end is no life's
        let mut old: Value = serde_json::to_value(&l).unwrap();
        old.as_object_mut().unwrap().retain(|k, _| k != "ready" && k != "fell_back");
        let mut old: Lifecycle = serde_json::from_value(old).unwrap();
        let s = old.apply(Event::Exited { generation: old.generation() }, T + 6_000);
        assert_eq!(s.ended, None, "{s:?}");
    }

    /// Goal (F11 of docs/explorations/pi-durable.md): a guest that dies
    /// while it holds its keepalive (a turn open, however long it has run)
    /// is started again, so its next life ends that turn. Method: a turn
    /// that runs past every hold, then a crash; on master `gone()` zeroed
    /// the keepalive before asking `wanted()`, and it stayed asleep.
    #[test]
    fn a_crash_while_busy_starts_it_again() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 4_000);
        // a long turn: the record's hold ran out long ago
        let late = T + 3 * IDLE_MS;
        // busy, it stays awake: it is metered, and (a keepalive open past
        // SAVE_EVERY_MS) saved, never put to sleep
        assert_eq!(l.apply(Event::Alarm, late - 1).actions, vec![Action::Meter { from_ms: T + 3_000, to_ms: late - 1 }, Action::Hold { generation: g }], "busy, it stays awake");
        let s = l.apply(Event::Exited { generation: g }, late);
        assert!(s.actions.contains(&Action::Start { generation: g + 1 }), "busy when it died, it starts again: {s:?}");
        assert_eq!(s.ended, Some(Ended { generation: g, by: LifeEnd::Exit, saved: false }));
        // the next life comes up and, its turns ended, is idle: it sleeps
        // once the crash's hold runs out, as after a record
        l.apply(Event::Ready { generation: g + 1 }, late + 3_000);
        let s = l.apply(Event::Alarm, late + IDLE_MS);
        assert!(s.actions.contains(&Action::Sleep { generation: g + 1 }), "{s:?}");
        // the keepalive's close reaching it first (the socket dies with its
        // container) holds it the same: the order of the two never matters
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 4_000);
        l.apply(Event::Closed { socket: Socket::Keepalive }, late);
        let s = l.apply(Event::Exited { generation: g }, late);
        assert!(s.actions.contains(&Action::Start { generation: g + 1 }), "{s:?}");
        // idle when it died, and nothing holding it: it stays asleep
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        let s = l.apply(Event::Exited { generation: g }, late);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Start { .. })), "{s:?}");
    }

    /// Goal (P3, F7): a start from a snapshot that fails starts again at
    /// once, from the image (the DO forgot the snapshot), with no strike
    /// against the computer and nothing metered; a second in one wake is a
    /// failure like any. Method: three snapshot failures in a row would be
    /// "won't wake" if each were a strike.
    #[test]
    fn a_failed_snapshot_start_falls_back_in_the_same_wake() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Sleep, T + 10_000);
        l.apply(Event::Asleep { generation: g }, T + 12_000);
        let s = l.apply(Event::Wake { why: Wake::Record }, T + 60_000);
        assert_eq!(s.actions, vec![Action::Start { generation: g + 1 }]);
        let s = l.apply(Event::SnapshotFailed { generation: g + 1, why: "no such snapshot".into() }, T + 61_000);
        assert_eq!(s.actions, vec![Action::Start { generation: g + 2 }], "the same wake, from the image, metering nothing");
        assert_eq!(l.failures, 0, "no strike");
        // a replay of it, or one from the start it superseded, changes nothing
        let before = l.clone();
        assert!(l.apply(Event::SnapshotFailed { generation: g + 1, why: "late".into() }, T + 62_000).actions.is_empty());
        assert_eq!(l, before);
        // the image's start fails too: that is a strike, with its pause
        let s = l.apply(Event::SnapshotFailed { generation: g + 2, why: "again".into() }, T + 63_000);
        assert_eq!((l.phase.clone(), l.failures, s.alarm_ms), (Phase::Asleep, 1, Some(T + 63_000 + RETRY_PAUSE_MS)));
        // and a start that comes up clears it, for the next wake's
        let s = l.apply(Event::Alarm, T + 63_000 + RETRY_PAUSE_MS);
        let Some(Action::Start { generation: g3 }) = s.actions.first().cloned() else { panic!("{s:?}") };
        l.apply(Event::Ready { generation: g3 }, T + 70_000);
        assert!(!l.fell_back && l.failures == 0);
        // with nothing wanting it any more, a fallback leaves it asleep
        let mut l = Lifecycle::new();
        let s = l.apply(Event::Wake { why: Wake::Presence }, T);
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("{s:?}") };
        let s = l.apply(Event::SnapshotFailed { generation, why: "slow".into() }, T + PREWAKE_MS + 1);
        assert!(s.actions.is_empty() && l.phase == Phase::Asleep, "{s:?}");
    }

    /// Goal: a life is a start that came up, and it ends once: by its sleep
    /// (the exit it is signalled into included) or on its own. A start that
    /// never came up ends no life. Method: each way out, read from the step.
    #[test]
    fn a_life_ends_once_and_says_how() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Sleep, T + 10_000);
        let s = l.apply(Event::Exited { generation: g }, T + 11_000);
        assert_eq!(s.ended, Some(Ended { generation: g, by: LifeEnd::Sleep, saved: false }), "signalled by its sleep, before it saved");
        assert!(l.apply(Event::Asleep { generation: g }, T + 12_000).ended.is_none(), "it ended once");
        // one that never came up
        let s = l.apply(Event::Wake { why: Wake::Owner }, T + 20_000);
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("{s:?}") };
        assert!(l.apply(Event::StartFailed { generation, why: "pull".into() }, T + 21_000).ended.is_none());
        // slept as it started, it never came up either
        let s = l.apply(Event::Wake { why: Wake::Owner }, T + 30_000);
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("{s:?}") };
        l.apply(Event::Sleep, T + 31_000);
        assert!(l.apply(Event::Asleep { generation }, T + 32_000).ended.is_none());
        // a crash that was its third strike still ends its life
        let mut l = Lifecycle { failures: STARTS_FAILED_MAX - 1, ..Lifecycle::new() };
        let s = l.apply(Event::Wake { why: Wake::Record }, T);
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("{s:?}") };
        l.apply(Event::Ready { generation }, T + 1_000);
        l.failures = STARTS_FAILED_MAX - 1;
        let s = l.apply(Event::Exited { generation }, T + 2_000);
        assert!(matches!(l.phase, Phase::Failed { .. }) && s.ended == Some(Ended { generation, by: LifeEnd::Exit, saved: false }), "{s:?} {:?}", l.phase);
    }

    const IMAGE: &str = "registry/stub@sha256:1";

    /// One save's records, as the DO's `DirectoryBackup` answers them.
    fn rec(id: &str) -> Vec<Value> {
        vec![json!({ "id": id, "dir": "/data" })]
    }

    /// Goal (P3): a wake uses the snapshot only while it caches the current
    /// save for the pinned image; any other is the image and the save, and
    /// a computer never saved starts empty. Method: each way the two can
    /// disagree: another image (a pin, a redeploy), snapshots off, a newer
    /// save, a current save that is another than the snapshot's (the one
    /// it cached would not restore), a snapshot of a save that has gone.
    #[test]
    fn a_snapshot_is_used_only_for_its_save_and_image() {
        let mut s = Saves::default();
        assert_eq!(s.plan(Some(IMAGE)), Plan::Nothing);
        s.saved(1, rec("b0"), T - 1, true);
        s.saved(1, rec("b1"), T, true);
        assert!(s.snapshotted("b1", "s1", IMAGE));
        assert_eq!(s.plan(Some(IMAGE)), Plan::Snapshot { id: "s1".into() });
        assert_eq!(s.plan(Some("registry/stub@sha256:2")), Plan::Backup, "another image's");
        assert_eq!(s.plan(None), Plan::Backup, "snapshots off");
        let mut other = s.clone();
        assert!(other.unusable("b1"), "b0 is left");
        assert_eq!((other.plan(Some(IMAGE)), other.current().map(|c| c.id.as_str())), (Plan::Backup, Some("b0")), "the current save is another than the snapshot's");
        // the next sleep saves and takes no snapshot: the old one is dropped
        s.saved(2, rec("b2"), T + 1, true);
        assert_eq!(s.plan(Some(IMAGE)), Plan::Backup);
        assert_eq!(s.snapshot, None);
        // a snapshot taken with a save that is no longer current is not kept
        assert!(!s.snapshotted("b1", "s1-late", IMAGE));
        assert_eq!(s.plan(Some(IMAGE)), Plan::Backup);
    }

    /// Goal (P3, decision 19): a snapshot is of the image its start ran, so
    /// an image pinned while it runs is taken at the next wake, from the
    /// save, never answered with a snapshot of the old image. On master the
    /// sleep named its snapshot's image by the pin at the time of the sleep.
    /// Method: a start on one image, a pin, its sleep, the next plan.
    #[test]
    fn a_pin_while_it_runs_wakes_on_the_new_image() {
        const NEXT: &str = "registry/stub@sha256:2";
        let mut s = Saves::default();
        s.starting(1, RestoreSource::Nothing);
        assert!(s.came_up(1, RestoreSource::Nothing, None, Some(IMAGE), T).is_some());
        // its owner pins NEXT while it runs; its sleep saves, then snapshots
        s.saved(1, rec("b1"), T + 10, true);
        let of = s.image_of(1).expect("the image it runs").to_string();
        assert_eq!(of, IMAGE);
        assert!(s.snapshotted("b1", "s1", &of));
        assert_eq!(s.plan(Some(NEXT)), Plan::Backup, "the new image, and the save");
        assert_eq!(s.plan(Some(IMAGE)), Plan::Snapshot { id: "s1".into() }, "pinned back, the snapshot serves again");
        assert_eq!(s.image_of(2), None, "a start that has not come up runs nothing yet");
    }

    /// Goal (F11 of docs/explorations/pi-durable.md): the second ask of a
    /// slow sleep (`SLEEP_DEADLINE_MS`) never forgets the snapshot the first
    /// stored, nor notes a failed save the first one made. On master a
    /// snapshot that failed forgot the stored one (cell/src/computer.rs,
    /// `sleep`). Method: both asks of one start, in each order their saves
    /// can land.
    #[test]
    fn the_second_ask_of_a_slow_sleep_keeps_the_first_ones_snapshot() {
        let mut s = Saves::default();
        s.saved(3, rec("b-old"), T, true);
        // the first ask saves and snapshots
        s.saved(4, rec("b1"), T + 70_000, true);
        assert!(s.snapshotted("b1", "s1", IMAGE));
        // the second: its save fails once the first's destroy took the
        // container, so it takes no snapshot, and that is not news
        assert!(!s.save_failed(4), "the first ask saved this start");
        assert!(s.save_failed(5), "a start whose sleep saved nothing is news");
        assert_eq!(s.plan(Some(IMAGE)), Plan::Snapshot { id: "s1".into() });
        // the other order: the second's save lands while the first snapshots
        let mut s = Saves::default();
        s.saved(4, rec("b1"), T, true);
        s.saved(4, rec("b2"), T + 1_000, true);
        assert!(!s.snapshotted("b1", "s1", IMAGE), "the first's snapshot caches a save that is no longer current");
        assert_eq!(s.plan(Some(IMAGE)), Plan::Backup);
        assert!(s.snapshotted("b2", "s2", IMAGE));
        assert_eq!(s.plan(Some(IMAGE)), Plan::Snapshot { id: "s2".into() });
    }


    /// The save a step asks for: whether it is held, and its number.
    fn save_asked(s: &Step) -> (u64, bool, u64) {
        match s.actions.iter().find(|a| matches!(a, Action::Save { .. })) {
            Some(Action::Save { generation, held, seq }) => (*generation, *held, *seq),
            _ => panic!("a save is asked for: {s:?}"),
        }
    }

    /// The lifecycle and its saves, as the Computer DO drives them: each
    /// step's end told to the saves, each start planned and recorded, each
    /// sleep's hold, save and stop reported back in turn.
    struct Computer {
        life: Lifecycle,
        saves: Saves,
        now: i64,
    }

    impl Computer {
        fn new() -> Computer {
            Computer { life: Lifecycle::new(), saves: Saves::default(), now: T }
        }

        fn apply(&mut self, e: Event) -> Step {
            self.apply_with(e, &Rules::default())
        }

        fn apply_with(&mut self, e: Event, rules: &Rules) -> Step {
            self.now += 1_000;
            let s = self.life.apply_with(e, self.now, rules);
            if let Some(ended) = s.ended {
                self.saves.ended(ended);
            }
            s
        }

        /// A wake that comes up: what it restored.
        fn wake(&mut self) -> ComputerRestore {
            let s = self.apply(Event::Wake { why: Wake::Owner });
            let Some(Action::Start { generation }) = s.actions.iter().find(|a| matches!(a, Action::Start { .. })).cloned() else { panic!("{s:?}") };
            self.come_up(generation)
        }

        fn come_up(&mut self, generation: u64) -> ComputerRestore {
            let plan = self.saves.plan(Some(IMAGE));
            let save = self.saves.current().map(|c| c.id.clone()).filter(|_| plan != Plan::Nothing);
            self.saves.starting(generation, plan.source());
            let r = self.saves.came_up(generation, plan.source(), save.as_deref(), Some(IMAGE), self.now).expect("the start under way");
            self.apply(Event::Ready { generation });
            r
        }

        /// The owner's sleep, held, saving `/data` as `save`; or failing to,
        /// past its bound (`unsaved_max_ms` 0), so it sleeps unsaved.
        fn sleep(&mut self, save: Option<&str>) {
            let g = self.life.generation();
            let s = self.apply(Event::Sleep);
            assert!(s.actions.contains(&Action::Sleep { generation: g }), "{s:?}");
            let (_, held, seq) = save_asked(&self.apply(Event::Held { generation: g, held: true }));
            assert!(held);
            let s = match save {
                Some(id) => {
                    self.saves.saved(g, rec(id), self.now, held);
                    self.apply(Event::Saved { generation: g, seq })
                }
                None => {
                    assert!(self.saves.save_failed(g));
                    self.apply_with(Event::SaveFailed { generation: g, seq, why: "no room".into() }, &Rules { unsaved_max_ms: 0 })
                }
            };
            assert!(s.actions.contains(&Action::Stop { generation: g, saved: save.is_some() }), "{s:?}");
            self.apply(Event::Asleep { generation: g });
        }
    }

    /// Goal (P7): each start that comes up says what it restored, how old
    /// that was, and how the life before it ended; a start that went back
    /// in time (after a crash, or a sleep that slept unsaved) is counted,
    /// and one after a sleep that saved is not. Method: one computer's
    /// history, read after each start.
    #[test]
    fn a_wake_says_what_it_restored() {
        let mut c = Computer::new();
        let r = c.wake();
        assert_eq!((r.from, r.save.clone(), r.after, r.rollback), (RestoreSource::Nothing, None, None, false), "its first start");
        c.sleep(Some("b1"));
        let saved_at = c.saves.current().map(|b| b.at_ms).expect("a save");
        let r = c.wake();
        assert_eq!((r.from, r.save.as_deref(), r.after, r.rollback), (RestoreSource::Backup, Some("b1"), Some(LifeEnd::Sleep), false), "{r:?}");
        assert_eq!((r.saved_at, r.age_ms), (Some(saved_at), Some(r.at - saved_at)));
        assert!(r.age_ms.is_some_and(|a| a > 0));
        // a crash: it comes back from the save before it, a rollback
        let g = c.life.generation();
        let s = c.apply(Event::Exited { generation: g });
        let Some(Action::Start { generation }) = s.actions.iter().find(|a| matches!(a, Action::Start { .. })).cloned() else { panic!("{s:?}") };
        let r = c.come_up(generation);
        assert_eq!((r.from, r.after, r.rollback, c.saves.rollbacks()), (RestoreSource::Backup, Some(LifeEnd::Exit), true, 1), "{r:?}");
        // a sleep that slept unsaved: the next wake is a rollback too (F6)
        c.sleep(None);
        let r = c.wake();
        assert_eq!((r.save.as_deref(), r.after, r.rollback, c.saves.rollbacks()), (Some("b1"), Some(LifeEnd::Sleep), true, 2), "{r:?}");
        // a crash, then a start that fails, then one that comes up: counted once
        let g = c.life.generation();
        c.apply(Event::Exited { generation: g });
        c.apply(Event::StartFailed { generation: g + 1, why: "pull".into() });
        let s = c.apply(Event::Wake { why: Wake::Owner });
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("{s:?}") };
        let r = c.come_up(generation);
        assert_eq!((r.rollback, c.saves.rollbacks()), (true, 3));
        // a sleep that saves: none
        c.sleep(Some("b2"));
        let r = c.wake();
        assert_eq!((r.save.as_deref(), r.rollback, c.saves.rollbacks()), (Some("b2"), false, 3));
        assert_eq!(c.saves.restored(), Some(&r));
        // a start superseded before it came up records nothing
        assert_eq!(c.saves.came_up(r.generation, r.from, Some("b2"), Some(IMAGE), c.now), None, "it came up once");
        // what it knows survives a restart of its Durable Object
        let back: Saves = serde_json::from_str(&serde_json::to_string(&c.saves).unwrap()).unwrap();
        assert_eq!(back, c.saves);
    }

    /// Goal (P3, F7): a start from a snapshot that fails forgets it, and
    /// the start the lifecycle makes at once restores the image and the
    /// save: one wake, no strike, the rollback rule unchanged. Method: the
    /// DO's two paths, its own failure and its container's exit before it
    /// came up, each reported once.
    #[test]
    fn a_broken_snapshot_wakes_from_the_save() {
        let mut c = Computer::new();
        c.wake();
        c.sleep(Some("b1"));
        assert!(c.saves.snapshotted("b1", "s1", IMAGE));
        let s = c.apply(Event::Wake { why: Wake::Record });
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("{s:?}") };
        let plan = c.saves.plan(Some(IMAGE));
        assert_eq!(plan, Plan::Snapshot { id: "s1".into() });
        c.saves.starting(generation, plan.source());
        assert_eq!(c.saves.start_from(generation), Some(RestoreSource::Snapshot), "its exit now is the start's own failure");
        assert_eq!(c.saves.snapshot_failed().map(|s| s.id), Some("s1".into()));
        let s = c.apply(Event::SnapshotFailed { generation, why: "expired".into() });
        let Some(Action::Start { generation: next }) = s.actions.first().cloned() else { panic!("{s:?}") };
        assert_eq!(c.saves.plan(Some(IMAGE)), Plan::Backup);
        let r = c.come_up(next);
        assert_eq!((r.from, r.save.as_deref(), r.rollback, c.life.failures), (RestoreSource::Backup, Some("b1"), false, 0), "{r:?}");
        assert_eq!(c.saves.came_up(generation, RestoreSource::Snapshot, Some("b1"), Some(IMAGE), c.now), None, "the broken start never comes up after");
    }

    /// Goal (P2, F1): awake, a computer is saved when its work ends: its
    /// last keepalive closes and `SAVE_SETTLE_MS` pass with none opened
    /// again; a keepalive that opens first cancels that save, so turns back
    /// to back save once; the save is a hold, the save, the hold let go.
    /// Method: the alarms the lifecycle asks for, around one turn and two.
    #[test]
    fn it_is_saved_when_work_ends() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 4_000);
        let s = l.apply(Event::Closed { socket: Socket::Keepalive }, T + 10_000);
        assert_eq!(s.alarm_ms, Some(T + 10_000 + SAVE_SETTLE_MS), "the settle is what it waits for");
        let s = l.apply(Event::Alarm, T + 10_000 + SAVE_SETTLE_MS - 1);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Hold { .. })), "not before it settles: {s:?}");
        let s = l.apply(Event::Alarm, T + 10_000 + SAVE_SETTLE_MS);
        assert_eq!(s.actions.last(), Some(&Action::Hold { generation: g }), "{s:?}");
        let s = l.apply(Event::Held { generation: g, held: true }, T + 41_000);
        assert_eq!(s.actions, vec![Action::Save { generation: g, held: true, seq: 1 }]);
        // a late or repeated answer to the hold asks nothing twice
        assert!(l.apply(Event::Held { generation: g, held: true }, T + 41_500).actions.is_empty());
        let s = l.apply(Event::Saved { generation: g, seq: 1 }, T + 42_000);
        assert_eq!(s.actions, vec![Action::Unhold { generation: g }]);
        assert!(matches!(l.phase, Phase::Awake { .. }) && l.saving().is_none(), "awake, held no more");
        assert_eq!(s.alarm_ms, Some((T + 10_000 + SAVE_SETTLE_MS + METER_EVERY_MS).min(T + 10_000 + IDLE_MS)), "nothing more to save until more work");
        assert!(l.apply(Event::Saved { generation: g, seq: 1 }, T + 43_000).actions.is_empty(), "a save is answered once");

        // two turns back to back: the first's settle is cancelled by the
        // second's keepalive, and its end saves once
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 4_000);
        l.apply(Event::Closed { socket: Socket::Keepalive }, T + 10_000);
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 20_000);
        let s = l.apply(Event::Alarm, T + 10_000 + SAVE_SETTLE_MS);
        assert!(!s.actions.iter().any(|a| matches!(a, Action::Hold { .. })), "a keepalive that opened first cancelled it: {s:?}");
        l.apply(Event::Closed { socket: Socket::Keepalive }, T + 50_000);
        let s = l.apply(Event::Alarm, T + 50_000 + SAVE_SETTLE_MS);
        assert_eq!(s.actions.iter().filter(|a| matches!(a, Action::Hold { .. })).count(), 1, "{s:?}");
        assert_eq!(s.actions.last(), Some(&Action::Hold { generation: g }));
        // a socket that closes when none is open ends no work
        let mut l = Lifecycle::new();
        started(&mut l, T);
        let s = l.apply(Event::Closed { socket: Socket::Keepalive }, T + 10_000);
        assert_eq!(s.alarm_ms, Some((T + 3_000 + METER_EVERY_MS).min(T + 10_000 + IDLE_MS)), "{s:?}");
    }

    /// Goal (F1): an always-on computer, which never sleeps, is saved when
    /// its work ends and every `SAVE_EVERY_MS` while it stays busy, its
    /// awake time never metered. On master it was never saved. Method: a
    /// day of 40-minute turns, an hour apart, every alarm it asks for
    /// answered as the DO would.
    #[test]
    fn always_on_is_saved_without_sleeping() {
        /// Every alarm `l` asks for before `until`, each save answered:
        /// the saves made.
        fn alarms_until(l: &mut Lifecycle, until: i64) -> u32 {
            let mut saves = 0;
            for _ in 0..64 {
                let Some(at) = l.next_alarm().filter(|at| *at < until) else { return saves };
                let s = l.apply(Event::Alarm, at);
                assert!(!s.actions.iter().any(|a| matches!(a, Action::Sleep { .. } | Action::Meter { .. })), "it never sleeps, nor is metered: {s:?}");
                if s.actions.contains(&Action::Hold { generation: 1 }) {
                    let (_, _, seq) = save_asked(&l.apply(Event::Held { generation: 1, held: true }, at + 500));
                    assert_eq!(l.apply(Event::Saved { generation: 1, seq }, at + 1_000).actions, vec![Action::Unhold { generation: 1 }]);
                    saves += 1;
                }
            }
            panic!("an hour asks for fewer than 64 alarms");
        }
        let mut l = Lifecycle::new();
        let s = l.apply(Event::AlwaysOn { on: true }, T);
        assert_eq!(s.actions, vec![Action::Start { generation: 1 }]);
        l.apply(Event::Ready { generation: 1 }, T + 3_000);
        let mut saves = 0;
        for hour in 0..24 {
            let turn = T + hour * 3_600_000 + 10_000;
            assert!(l.apply(Event::Opened { socket: Socket::Keepalive }, turn).actions.is_empty());
            saves += alarms_until(&mut l, turn + 40 * 60_000);
            l.apply(Event::Closed { socket: Socket::Keepalive }, turn + 40 * 60_000);
            saves += alarms_until(&mut l, turn + 3_600_000);
            assert_eq!(saves, 3 * (hour as u32 + 1), "two while busy (its timer), one as its work ended, hour {hour}");
        }
        assert!(matches!(l.phase, Phase::Awake { generation: 1, .. }));
    }

    /// Goal (P2): a sleep holds the guest, then saves (held, or not when
    /// the guest never answered), then stops; its life's end says it saved.
    /// A step that never reports is asked for again at its deadline.
    #[test]
    fn a_sleep_holds_then_saves_then_stops() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        let s = l.apply(Event::Alarm, T + IDLE_MS);
        assert_eq!(s.actions.last(), Some(&Action::Sleep { generation: g }), "first its hold");
        assert_eq!(s.alarm_ms, Some(T + IDLE_MS + HOLD_DEADLINE_MS));
        let s = l.apply(Event::Held { generation: g, held: true }, T + IDLE_MS + 500);
        assert_eq!(s.actions, vec![Action::Save { generation: g, held: true, seq: 1 }], "then its save");
        assert_eq!(s.alarm_ms, Some(T + IDLE_MS + 500 + SAVE_DEADLINE_MS));
        let s = l.apply(Event::Saved { generation: g, seq: 1 }, T + IDLE_MS + 4_000);
        assert_eq!(s.actions, vec![Action::Stop { generation: g, saved: true }], "then its stop, a snapshot of the save first");
        assert_eq!(s.alarm_ms, Some(T + IDLE_MS + 4_000 + SLEEP_DEADLINE_MS));
        let s = l.apply(Event::Asleep { generation: g }, T + IDLE_MS + 12_000);
        assert_eq!(s.actions, vec![Action::Meter { from_ms: T + IDLE_MS, to_ms: T + IDLE_MS + 12_000 }]);
        assert_eq!(s.ended, Some(Ended { generation: g, by: LifeEnd::Sleep, saved: true }));
        assert_eq!((l.phase.clone(), s.alarm_ms), (Phase::Asleep, None));

        // an image that never answers is saved anyway, recorded as not held;
        // each step that never reports is asked for again at its deadline
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Sleep, T + 10_000);
        let s = l.apply(Event::Alarm, T + 10_000 + HOLD_DEADLINE_MS);
        assert_eq!(s.actions, vec![Action::Sleep { generation: g }], "its hold, again");
        let s = l.apply(Event::Held { generation: g, held: false }, T + 10_000 + HOLD_DEADLINE_MS + HOLD_WAIT_MS);
        assert_eq!(s.actions, vec![Action::Save { generation: g, held: false, seq: 1 }]);
        let at = T + 10_000 + HOLD_DEADLINE_MS + HOLD_WAIT_MS;
        let s = l.apply(Event::Alarm, at + SAVE_DEADLINE_MS);
        assert_eq!(s.actions, vec![Action::Save { generation: g, held: false, seq: 2 }], "its save, again, a new one");
        assert!(l.apply(Event::Saved { generation: g, seq: 1 }, at + SAVE_DEADLINE_MS + 1).actions.is_empty(), "the first one's late answer changes nothing");
        let s = l.apply(Event::Saved { generation: g, seq: 2 }, at + SAVE_DEADLINE_MS + 2);
        assert_eq!(s.actions, vec![Action::Stop { generation: g, saved: true }]);
        let s = l.apply(Event::Alarm, at + SAVE_DEADLINE_MS + 2 + SLEEP_DEADLINE_MS);
        assert_eq!(s.actions, vec![Action::Stop { generation: g, saved: true }], "its stop, again");
        // its container exits as it is signalled: the sleep ended it, saved
        let s = l.apply(Event::Exited { generation: g }, at + SAVE_DEADLINE_MS + SLEEP_DEADLINE_MS + 3);
        assert_eq!(s.ended, Some(Ended { generation: g, by: LifeEnd::Sleep, saved: true }));
        // a lifecycle stored mid-sleep before saves were its own reads as in
        // its stop: its deadline asks for the stop, unsaved
        let mut v = serde_json::to_value(Lifecycle { phase: Phase::Sleeping { generation: 1, since_ms: T }, generation: 1, ..Lifecycle::new() }).unwrap();
        v.as_object_mut().unwrap().retain(|k, _| k != "saving");
        let mut old: Lifecycle = serde_json::from_value(v).unwrap();
        assert_eq!(old.apply(Event::Alarm, T + SLEEP_DEADLINE_MS).actions, vec![Action::Stop { generation: 1, saved: false }]);
    }

    /// Goal (P2's race): a keepalive that opens while an idle sleep holds
    /// its guest (it took work as it was held) cancels the sleep: it stays
    /// awake, held no more, and is saved when that work ends. Once its save
    /// is under way, or for its owner's sleep, it does not: the sleep goes
    /// on, and the next life runs what it had not claimed.
    #[test]
    fn a_keepalive_during_the_hold_cancels_the_sleep() {
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Alarm, T + IDLE_MS);
        // a record wakes it as it holds: the newest push wins either way
        l.apply(Event::Wake { why: Wake::Record }, T + IDLE_MS + 100);
        let s = l.apply(Event::Opened { socket: Socket::Keepalive }, T + IDLE_MS + 200);
        assert_eq!(s.actions, vec![Action::Unhold { generation: g }]);
        assert_eq!(l.phase, Phase::Awake { generation: g, since_ms: T + IDLE_MS + 200 });
        assert!(l.saving().is_none() && !l.wake_after_sleep);
        // its hold's late answer changes nothing
        let before = l.clone();
        assert!(l.apply(Event::Held { generation: g, held: true }, T + IDLE_MS + 300).actions.is_empty());
        assert_eq!(l, before);
        // the work ends: saved as any work is, then idle it sleeps again
        l.apply(Event::Closed { socket: Socket::Keepalive }, T + IDLE_MS + 60_000);
        let s = l.apply(Event::Alarm, T + IDLE_MS + 60_000 + SAVE_SETTLE_MS);
        assert_eq!(s.actions.last(), Some(&Action::Hold { generation: g }));
        // once the sleep saves, the keepalive dies with its container
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Alarm, T + IDLE_MS);
        l.apply(Event::Held { generation: g, held: true }, T + IDLE_MS + 100);
        let s = l.apply(Event::Opened { socket: Socket::Keepalive }, T + IDLE_MS + 200);
        assert!(s.actions.is_empty() && matches!(l.phase, Phase::Sleeping { .. }), "{s:?}");
        // and its owner's sleep is never cancelled
        let mut l = Lifecycle::new();
        started(&mut l, T);
        l.apply(Event::Sleep, T + 10_000);
        let s = l.apply(Event::Opened { socket: Socket::Keepalive }, T + 10_100);
        assert!(s.actions.is_empty() && matches!(l.phase, Phase::Sleeping { .. }), "{s:?}");
        // nor an idle sleep its owner asked for again as it held
        let mut l = Lifecycle::new();
        started(&mut l, T);
        l.apply(Event::Alarm, T + IDLE_MS);
        l.apply(Event::Sleep, T + IDLE_MS + 50);
        let s = l.apply(Event::Opened { socket: Socket::Keepalive }, T + IDLE_MS + 100);
        assert!(s.actions.is_empty() && matches!(l.phase, Phase::Sleeping { .. }), "{s:?}");
    }

    /// Goal (I5): a sleep whose save fails keeps its container (awake, held
    /// no more, saying so) and tries again after a growing pause; past
    /// `unsaved_max_ms` it sleeps unsaved, saying so, and that life's end
    /// says it did not save. A save that works on a try sleeps it, and
    /// clears what it said. Method: one computer's tries, every one
    /// failing, up to the bound; then one that works.
    #[test]
    fn a_sleep_whose_save_fails_keeps_the_container() {
        let rules = Rules::default();
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        let mut at = T + IDLE_MS;
        let mut s = l.apply_with(Event::Alarm, at, &rules);
        let first = at;
        let mut tries = 0;
        // bounded: a try a pause apart, the pause growing, for the bound
        let stopped = loop {
            assert!(tries < 40, "the bound ends the tries");
            assert_eq!(s.actions.last(), Some(&Action::Sleep { generation: g }), "try {tries}: {s:?}");
            let (_, _, seq) = save_asked(&l.apply_with(Event::Held { generation: g, held: true }, at + 500, &rules));
            at += 2_000;
            s = l.apply_with(Event::SaveFailed { generation: g, seq, why: "R2 is down".into() }, at, &rules);
            tries += 1;
            if s.actions.iter().any(|a| matches!(a, Action::Stop { .. })) {
                break s;
            }
            assert_eq!(s.actions, vec![Action::Unhold { generation: g }], "it keeps its container: {s:?}");
            assert!(matches!(l.phase, Phase::Awake { generation, .. } if generation == g), "the same container, awake");
            assert!(matches!(&s.note, SaveNote::Says(why) if why.contains("could not save") && why.contains("R2 is down")), "{:?}", s.note);
            assert_eq!(l.unsaved_since_ms(), Some(first + 2_000));
            let due = l.save_due_ms.expect("its next try is due");
            assert_eq!(due - at, (SAVE_RETRY_MS << (tries - 1).min(8)).min(SAVE_EVERY_MS), "try {tries}'s pause");
            // the alarms before it meter, and try nothing: unwanted, it waits
            for _ in 0..8 {
                let Some(next) = s.alarm_ms.filter(|next| *next < due) else { break };
                s = l.apply_with(Event::Alarm, next, &rules);
                assert!(s.actions.iter().all(|a| matches!(a, Action::Meter { .. })), "before its try: {s:?}");
            }
            assert_eq!(s.alarm_ms, Some(due), "its try is the alarm it asks for");
            at = due;
            s = l.apply_with(Event::Alarm, at, &rules);
        };
        assert!(at - first >= rules.unsaved_max_ms && at - first < rules.unsaved_max_ms + SAVE_EVERY_MS + 10_000, "it gave up past its bound, not long after: {} ms", at - first);
        assert_eq!(stopped.actions, vec![Action::Stop { generation: g, saved: false }]);
        assert!(matches!(&stopped.note, SaveNote::Says(why) if why.contains("slept unsaved")), "{:?}", stopped.note);
        let s = l.apply_with(Event::Asleep { generation: g }, at + 5_000, &rules);
        assert_eq!(s.ended, Some(Ended { generation: g, by: LifeEnd::Sleep, saved: false }), "its next wake is a rollback");

        // a try that works: it sleeps, and the note goes
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Sleep, T + 10_000);
        let (_, _, seq) = save_asked(&l.apply(Event::Held { generation: g, held: true }, T + 10_500));
        let s = l.apply(Event::SaveFailed { generation: g, seq, why: "R2 is down".into() }, T + 11_000);
        assert_eq!(s.actions, vec![Action::Unhold { generation: g }], "an owner's sleep keeps its container too");
        assert_eq!(s.alarm_ms, Some(T + 11_000 + SAVE_RETRY_MS));
        let s = l.apply(Event::Alarm, T + 11_000 + SAVE_RETRY_MS);
        assert_eq!(s.actions.last(), Some(&Action::Sleep { generation: g }), "unwanted still, its retry is a sleep");
        let (_, _, seq) = save_asked(&l.apply(Event::Held { generation: g, held: true }, T + 72_000));
        let s = l.apply(Event::Saved { generation: g, seq }, T + 73_000);
        assert_eq!((s.actions.clone(), s.note.clone()), (vec![Action::Stop { generation: g, saved: true }], SaveNote::Clear));
        assert_eq!(l.unsaved_since_ms(), None);
        // and awake, a save that fails is tried again later, held no more
        let mut l = Lifecycle::new();
        let g = started(&mut l, T);
        l.apply(Event::Opened { socket: Socket::Tab }, T + 4_000);
        l.apply(Event::Opened { socket: Socket::Keepalive }, T + 4_000);
        l.apply(Event::Closed { socket: Socket::Keepalive }, T + 10_000);
        l.apply(Event::Alarm, T + 40_000);
        let s = l.apply(Event::SaveFailed { generation: g, seq: 0, why: "no container".into() }, T + 41_000);
        assert_eq!((s.actions, s.alarm_ms), (vec![Action::Unhold { generation: g }], Some(T + 41_000 + SAVE_RETRY_MS)));
        let s = l.apply(Event::Alarm, T + 41_000 + SAVE_RETRY_MS);
        assert_eq!(s.actions.last(), Some(&Action::Hold { generation: g }), "wanted (a tab is open), it saves awake: {s:?}");
    }

    /// Goal (F5): a start whose save will not restore (its archive gone or
    /// corrupt, or the image's check of it failed) starts again at once from
    /// the save before it, no strike against the computer, until none is
    /// left; then it is a strike like any. A start that fails for another
    /// reason is a strike, and tries the same save again. Method: three
    /// saves, each found unusable in turn, as the DO finds them.
    #[test]
    fn a_start_that_keeps_failing_tries_the_save_before() {
        let mut c = Computer::new();
        c.wake();
        c.sleep(Some("b1"));
        c.wake();
        c.sleep(Some("b2"));
        c.wake();
        c.sleep(Some("b3"));
        assert_eq!(c.saves.all().iter().map(|s| s.number).collect::<Vec<_>>(), vec![3, 2, 1], "three kept, newest first");
        let s = c.apply(Event::Wake { why: Wake::Record });
        let Some(Action::Start { generation: mut g }) = s.actions.first().cloned() else { panic!("{s:?}") };
        // a transient failure (a pull) is a strike, and the same save next
        let s = c.apply(Event::StartFailed { generation: g, why: "pull".into() });
        assert_eq!(c.life.failures, 1);
        assert_eq!(c.saves.current().map(|s| s.id.as_str()), Some("b3"), "the same save, next");
        c.now = s.alarm_ms.expect("its retry is due") - 1_000;
        let s = c.apply(Event::Alarm);
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("its retry starts it: {s:?}") };
        g = generation;
        // b3, then b2, would not restore: each starts again at once
        for (bad, next) in [("b3", "b2"), ("b2", "b1")] {
            assert_eq!(c.saves.current().map(|s| s.id.as_str()), Some(bad));
            let older = c.saves.unusable(bad);
            assert!(older);
            let s = c.apply(Event::RestoreFailed { generation: g, why: format!("{bad}'s archive is corrupt"), older });
            assert_eq!(s.actions, vec![Action::Start { generation: g + 1 }], "at once, from the save before it");
            assert_eq!(c.life.failures, 1, "no strike");
            assert_eq!(c.saves.current().map(|s| s.id.as_str()), Some(next));
            g += 1;
        }
        // b1 comes up: a rollback, since its life's newest save was b3
        let r = c.come_up(g);
        assert_eq!((r.save.as_deref(), r.rollback, c.life.failures), (Some("b1"), true, 0), "{r:?}");
        // with no save left, a save that will not restore is a strike
        let mut c = Computer::new();
        c.wake();
        c.sleep(Some("b1"));
        let s = c.apply(Event::Wake { why: Wake::Owner });
        let Some(Action::Start { generation }) = s.actions.first().cloned() else { panic!("{s:?}") };
        let older = c.saves.unusable("b1");
        assert!(!older);
        let s = c.apply(Event::RestoreFailed { generation, why: "corrupt".into(), older });
        assert!(s.actions.is_empty() && c.life.failures == 1 && c.life.phase == Phase::Asleep, "{s:?}");
        assert_eq!(c.saves.current().map(|s| s.id.as_str()), Some("b1"), "tried again, as any start is");
        // and the lifecycle bounds what the DO says: no more fallbacks than saves
        let mut l = Lifecycle::new();
        l.apply(Event::Wake { why: Wake::Owner }, T);
        for i in 0..SAVES_KEPT as u64 {
            let s = l.apply(Event::RestoreFailed { generation: i + 1, why: "x".into(), older: true }, T + i as i64);
            assert_eq!(s.actions, vec![Action::Start { generation: i + 2 }]);
        }
        let s = l.apply(Event::RestoreFailed { generation: SAVES_KEPT as u64 + 1, why: "x".into(), older: true }, T + 10);
        assert!(s.actions.is_empty() && l.failures == 1, "a fallback past the saves kept is a strike: {s:?}");
    }

    /// Goal: three saves are kept, newest first, each numbered; a fourth
    /// pushes out the oldest, whose records the DO deletes; a save says
    /// whether it was held, and the view shows them so.
    #[test]
    fn three_saves_are_kept_newest_first() {
        let mut s = Saves::default();
        assert!(s.saved(1, rec("a"), T, true).is_empty());
        assert!(s.saved(1, rec("b"), T + 1, false).is_empty());
        assert!(s.saved(2, rec("c"), T + 2, true).is_empty());
        assert!(s.forgetting().is_empty());
        let lost = s.saved(2, vec![json!({ "id": "d", "dir": "/data" }), json!({ "id": "d-work", "dir": "/data/work" })], T + 3, true);
        assert!(lost.is_empty());
        assert_eq!(s.forgetting(), rec("a").as_slice(), "the oldest's records, to delete");
        assert_eq!(s.all().iter().map(|s| (s.number, s.id.as_str())).collect::<Vec<_>>(), vec![(4, "d"), (3, "c"), (2, "b")]);
        assert_eq!(s.current().map(|c| c.records.len()), Some(2));
        let views = s.views();
        assert_eq!(views[2], ComputerSave { number: 2, id: "b".into(), at: T + 1, generation: 1, held: false, unusable: false });
        let v = serde_json::to_value(&views[0]).unwrap();
        assert_eq!(v, json!({ "number": 4, "id": "d", "at": T + 3, "generation": 2, "held": true }));
        // a record stored before it kept several saves is not read: a hard cut
        let old: Saves = serde_json::from_value(json!({ "backup": { "id": "x", "generation": 1, "atMs": T }, "snapshot": null, "starting": null, "ended": null, "restored": null, "running": null, "rollbacks": 0 })).unwrap();
        assert_eq!((old.current(), old.plan(Some(IMAGE))), (None, Plan::Nothing));
    }

    /// Goal (#156, problem 10): a record let go of is kept until its delete
    /// worked, so one that failed is tried again by the next save's pass,
    /// across a restart of the DO (the saves are stored as JSON); a failed
    /// save's records are let go of the same way; past `FORGETTING_MAX` the
    /// oldest are answered as lost, never silently dropped.
    #[test]
    fn a_record_let_go_of_is_kept_until_its_delete_worked() {
        let ids = |s: &Saves| s.forgetting().iter().map(|r| r["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let mut s = Saves::default();
        for (i, id) in ["a", "b", "c", "d"].into_iter().enumerate() {
            s.saved(1, rec(id), T + i as i64, true);
        }
        assert_eq!(ids(&s), ["a"]);
        // a's delete failed: the next save's pass finds it still, beside b
        s.saved(1, rec("e"), T + 10, true);
        assert_eq!(ids(&s), ["a", "b"]);
        // a restart reads them back; one stored before this field reads none
        let mut s: Saves = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(ids(&s), ["a", "b"]);
        let mut stored = serde_json::to_value(&s).unwrap();
        stored.as_object_mut().unwrap().remove("forgetting");
        assert!(serde_json::from_value::<Saves>(stored).unwrap().forgetting().is_empty());
        // b's delete worked, then a's; a delete answered twice changes nothing
        s.forgot("b");
        s.forgot("a");
        s.forgot("a");
        assert!(s.forgetting().is_empty());
        assert_eq!(s.all().iter().map(|k| k.id.as_str()).collect::<Vec<_>>(), ["e", "d", "c"], "the saves kept are untouched");
        // a failed save's records, taken before it failed
        assert!(s.let_go(vec![json!({ "id": "f", "dir": "/data" })]).is_empty());
        assert_eq!(ids(&s), ["f"]);
        // bounded: the oldest past the bound are answered, in order
        let many: Vec<Value> = (0..FORGETTING_MAX).map(|i| json!({ "id": format!("m{i}"), "dir": "/data" })).collect();
        let lost = s.let_go(many);
        assert_eq!(lost, vec![json!({ "id": "f", "dir": "/data" })]);
        assert_eq!(s.forgetting().len(), FORGETTING_MAX);
        assert_eq!(ids(&s)[0], "m0");
    }

    /// Invalid: a record a kept save holds is never let go of (its delete
    /// would take what a wake restores).
    #[test]
    #[should_panic(expected = "no kept save's")]
    fn a_kept_saves_record_is_never_let_go_of() {
        let mut s = Saves::default();
        s.saved(1, rec("a"), T, true);
        s.let_go(rec("a"));
    }

    /// Valid and invalid answers to the hold: the patterns a guest names
    /// (an empty answer names none), and every answer the platform refuses,
    /// which saves the guest whole.
    #[test]
    fn a_guest_names_what_its_save_leaves_out() {
        assert_eq!(left_out(""), Ok(vec![]));
        assert_eq!(left_out("*.db\n*.db-wal\n\n  *.db-shm\n*.db-journal\n"), Ok(vec!["*.db".into(), "*.db-wal".into(), "*.db-shm".into(), "*.db-journal".into()]));
        assert_eq!(left_out("/work/\nprofiles/*/cache/[ab]?.tmp"), Ok(vec!["/work/".into(), "profiles/*/cache/[ab]?.tmp".into()]));
        let too_long = "x".repeat(LEFT_OUT_PATTERN_MAX_BYTES + 1);
        let too_many = "a\n".repeat(LEFT_OUT_MAX + 1);
        let too_big = "a\n".repeat(HELD_ANSWER_MAX_BYTES);
        for bad in ["!keep.db", "a b", "$(rm -rf /)", "*.db;rm", "é", too_long.as_str(), too_many.as_str(), too_big.as_str()] {
            assert!(left_out(bad).is_err(), "{bad:?}");
        }
    }

    /// A guest's word on a hold it has not answered, as logged: none for
    /// nothing said, its text trimmed, its control characters spaces, and
    /// never past its bound, cut at a character.
    #[test]
    fn a_guest_says_why_it_has_not_answered_within_a_bound() {
        assert_eq!(unheld(""), None);
        assert_eq!(unheld(" \n\t"), None);
        assert_eq!(unheld("copy refused: copying /data/a.db: file is not a database\n"), Some("copy refused: copying /data/a.db: file is not a database".into()));
        assert_eq!(unheld("a\u{1b}[31mb\nc"), Some("a [31mb\nc".into()));
        let long = format!("{}é{}", "x".repeat(UNHELD_MAX_BYTES - 1), "y".repeat(100));
        let cut = unheld(&long).unwrap();
        assert!(cut.len() <= UNHELD_MAX_BYTES && cut.chars().all(|c| c == 'x'), "{} bytes", cut.len());
    }

    #[test]
    #[should_panic(expected = "a directory's record before any inside it")]
    fn a_save_whose_records_are_out_of_order_is_a_bug() {
        let mut s = Saves::default();
        s.saved(1, vec![json!({ "id": "w", "dir": "/data/work" }), json!({ "id": "d", "dir": "/data" })], T, true);
    }

    /// Goal: under any interleaving of events (duplicates, late reports,
    /// reordering, crashes, snapshots that will not start, saves that will
    /// not restore, holds the guest answers or not, saves that fail), the
    /// generation only grows, at most one container is ever running,
    /// metered intervals never overlap or run backwards, a computer is
    /// never left asleep while it is wanted and not failed, one that dies
    /// busy starts again unless it gave up, and each life (a start that
    /// came up) ends once. A computer is never asleep with work newer than
    /// its newest save unless a crash or the bounded failure put it there,
    /// and awake and idle with work newer than its newest save, a save is
    /// under way or due (I7). Its saves ride along as the DO keeps them, so
    /// their own assertions run too. Method: the DO's reports for the
    /// actions it was handed are delivered in any order, late, twice, or
    /// never, among events of any kind.
    #[test]
    fn simulated_interleavings_keep_the_invariants() {
        // a small xorshift: the sequence is a pure function of the seed
        fn rng(seed: &mut u64) -> u64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            *seed
        }
        // the sim's bound is shorter than the default, so it is reached
        let rules = Rules { unsaved_max_ms: 10 * 60_000 };
        let mut reached = [0u32; 6];
        for seed0 in 1..=8u64 {
            let mut seed = seed0.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut l = Lifecycle::new();
            let mut saves = Saves::default();
            let mut now = T;
            let mut metered: Vec<(i64, i64)> = vec![];
            let mut generation = 0;
            let mut came_up = std::collections::BTreeSet::new();
            let mut ended = std::collections::BTreeSet::new();
            // what the DO would report next, for the actions it was handed
            let mut pending: Vec<Event> = vec![];
            // each save asked for, by its number: its start, when, and held
            let mut taken = std::collections::BTreeMap::new();
            let mut newest_save_ms = i64::MIN;
            let mut last_work_ms = i64::MIN;
            // half the seeds' saves fail often enough to reach the bound
            let fails_one_in = if seed0 % 2 == 0 { 2 } else { 5 };
            for step in 0..6_000u32 {
                now += (rng(&mut seed) % 120_000) as i64;
                let g = l.generation().saturating_sub(rng(&mut seed) % 2);
                let event = if !pending.is_empty() && !rng(&mut seed).is_multiple_of(3) {
                    let i = (rng(&mut seed) % pending.len() as u64) as usize;
                    pending.remove(i)
                } else {
                    let seq = l.save_seq.saturating_sub(rng(&mut seed) % 2);
                    match rng(&mut seed) % 15 {
                        0 => Event::Wake { why: [Wake::Record, Wake::Presence, Wake::Tab, Wake::Owner, Wake::Joined][(rng(&mut seed) % 5) as usize] },
                        1 => Event::Ready { generation: g },
                        2 => Event::StartFailed { generation: g, why: "sim".into() },
                        3 => Event::Opened { socket: if rng(&mut seed).is_multiple_of(2) { Socket::Tab } else { Socket::Keepalive } },
                        4 => Event::Closed { socket: if rng(&mut seed).is_multiple_of(2) { Socket::Tab } else { Socket::Keepalive } },
                        5 => Event::Asleep { generation: g },
                        6 => Event::Exited { generation: g },
                        7 => Event::AlwaysOn { on: rng(&mut seed).is_multiple_of(4) },
                        8 if rng(&mut seed).is_multiple_of(3) => Event::Sleep,
                        9 => Event::SnapshotFailed { generation: g, why: "sim".into() },
                        10 => {
                            // the DO found the save a start restores unusable
                            let starting = matches!(l.phase, Phase::Starting { generation: s, .. } if s == g);
                            let current = saves.current().map(|c| c.id.clone()).filter(|_| starting);
                            let older = current.is_some_and(|id| saves.unusable(&id));
                            Event::RestoreFailed { generation: g, why: "sim".into(), older }
                        }
                        // late or repeated answers of the DO's
                        11 => Event::Held { generation: g, held: rng(&mut seed).is_multiple_of(2) },
                        12 => Event::Saved { generation: g, seq },
                        13 => Event::SaveFailed { generation: g, seq, why: "sim".into() },
                        _ => Event::Alarm,
                    }
                };
                // work: a guest holding its keepalive while awake
                if matches!(l.phase, Phase::Awake { .. }) && l.keepalives > 0 {
                    last_work_ms = now;
                }
                // a crash of the running start while its guest holds a keepalive
                let busy_crash = matches!(&event, Event::Exited { generation: e } if matches!(l.phase, Phase::Awake { generation, .. } | Phase::Starting { generation, .. } if generation == *e)) && l.keepalives > 0;
                let coming_up = matches!(&event, Event::Ready { generation: e } if matches!(l.phase, Phase::Starting { generation, .. } if generation == *e));
                let kept = match &event {
                    Event::Saved { generation: e, seq } if l.running() == Some(*e) => matches!(l.saving, Some(Saving::Save { seq: s, .. }) if s == *seq).then_some(*seq),
                    _ => None,
                };
                let cancels = matches!(&event, Event::Opened { socket: Socket::Keepalive }) && matches!(l.phase, Phase::Sleeping { .. }) && !l.owner_sleep && matches!(l.saving, Some(Saving::Hold { .. }));
                let s = l.apply_with(event, now, &rules);
                if let Some(seq) = kept {
                    // the save the DO keeps, as it reports it kept
                    let (of, at, held) = taken[&seq];
                    saves.saved(of, rec(&format!("b{seed0}-{seq}")), now, held);
                    newest_save_ms = at;
                    reached[0] += 1;
                }
                reached[3] += u32::from(cancels);
                if let Some(e) = s.ended {
                    assert!(came_up.contains(&e.generation), "only a start that came up ends a life (seed {seed0})");
                    assert!(ended.insert(e.generation), "a life ends once (seed {seed0})");
                    if e.by == LifeEnd::Sleep && e.saved {
                        assert!(last_work_ms <= newest_save_ms, "asleep with work newer than its newest save (seed {seed0}, step {step}): {last_work_ms} > {newest_save_ms}");
                    }
                    saves.ended(e);
                }
                if coming_up {
                    assert!(matches!(l.phase, Phase::Awake { .. }));
                    came_up.insert(l.generation());
                    let plan = saves.plan(Some(IMAGE));
                    let from = plan.source();
                    let save = saves.current().map(|c| c.id.clone()).filter(|_| plan != Plan::Nothing);
                    saves.starting(l.generation(), from);
                    assert!(saves.came_up(l.generation(), from, save.as_deref(), Some(IMAGE), now).is_some(), "the start that came up records what it restored");
                    // a new life's /data is its save's: no work of its own yet
                    last_work_ms = i64::MIN;
                }
                if busy_crash && !matches!(l.phase, Phase::Failed { .. }) {
                    assert!(s.actions.iter().any(|a| matches!(a, Action::Start { .. })), "busy when it died, it starts again (seed {seed0}): {l:?}");
                }
                if matches!(s.note, SaveNote::Says(ref why) if why.contains("slept unsaved")) {
                    reached[2] += 1;
                }
                for a in &s.actions {
                    match a {
                        Action::Start { generation: g } => {
                            assert!(*g > generation, "a start's generation grows");
                            generation = *g;
                        }
                        Action::Meter { from_ms, to_ms } => {
                            assert!(from_ms < to_ms && *to_ms <= now, "an interval runs forward, up to now");
                            assert!(metered.last().is_none_or(|(_, last)| from_ms >= last), "intervals never overlap");
                            metered.push((*from_ms, *to_ms));
                        }
                        Action::Sleep { generation: g } | Action::Hold { generation: g } => {
                            assert_eq!(*g, l.generation(), "only the current start holds");
                            // most guests answer the hold
                            pending.push(Event::Held { generation: *g, held: !rng(&mut seed).is_multiple_of(4) });
                            // and some take work as a sleep holds them (P2's race)
                            if matches!(a, Action::Sleep { .. }) && rng(&mut seed).is_multiple_of(3) {
                                pending.push(Event::Opened { socket: Socket::Keepalive });
                            }
                        }
                        Action::Save { generation: g, held, seq } => {
                            assert!(taken.insert(*seq, (*g, now, *held)).is_none(), "a save's number is asked for once");
                            if rng(&mut seed).is_multiple_of(fails_one_in) {
                                pending.push(Event::SaveFailed { generation: *g, seq: *seq, why: "sim".into() });
                                reached[1] += 1;
                            } else {
                                pending.push(Event::Saved { generation: *g, seq: *seq });
                            }
                        }
                        Action::Unhold { generation: g } => assert_eq!(*g, l.generation(), "only the current start is let go"),
                        Action::Stop { generation: g, .. } => {
                            assert_eq!(*g, l.generation(), "only the current start stops");
                            pending.push(if rng(&mut seed).is_multiple_of(5) { Event::Exited { generation: *g } } else { Event::Asleep { generation: *g } });
                        }
                    }
                }
                assert!(pending.len() < 64, "the DO's reports are bounded by the actions it is handed");
                assert!(saves.rollbacks() <= ended.len() as u64, "each rollback is one life's loss");
                if matches!(l.phase, Phase::Asleep) && l.wanted(now) && l.retry_at_ms.is_none() {
                    panic!("asleep while wanted, with no retry due (seed {seed0}): {l:?}");
                }
                // I7: awake and idle, work newer than its newest save is saved soon
                if matches!(l.phase, Phase::Awake { .. }) && l.keepalives == 0 && last_work_ms > newest_save_ms {
                    assert!(l.saving.is_some() || l.save_due_ms.is_some(), "awake with work unsaved and no save due (seed {seed0}, step {step}): {l:?}");
                    reached[4] += 1;
                }
                if l.unsaved_since_ms.is_some() && matches!(l.phase, Phase::Awake { .. }) {
                    reached[5] += 1;
                }
                // whatever is running has an alarm watching it (a past one fires at once)
                if !matches!(l.phase, Phase::Asleep | Phase::Failed { .. }) {
                    assert!(s.alarm_ms.is_some(), "a running computer has its alarm: {l:?}");
                }
            }
            assert!(!came_up.is_empty() && !ended.is_empty() && saves.rollbacks() > 0, "the simulation reached lives, their ends, and rollbacks (seed {seed0})");
        }
        // every kind of save the lifecycle has was reached, across the seeds
        assert!(reached.iter().all(|n| *n > 0), "saves kept, saves failed, sleeps unsaved, sleeps cancelled, unsaved work due, failed sleeps kept awake: {reached:?}");
    }
}
