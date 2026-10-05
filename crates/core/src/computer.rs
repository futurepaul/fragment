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
//! - **Sleeping.** The DO's own sequence (hold, save, snapshot, signal,
//!   wait, destroy), reported back by `Asleep`.
//! - **Failing.** A start that fails, or never reports ready, is tried
//!   again with a growing pause; after `STARTS_FAILED_MAX` in a row the
//!   computer "won't wake" until its owner asks (lesson 4): no more
//!   container starts are paid for.
//! - **Metering.** Awake time, in intervals that never overlap, every
//!   `METER_EVERY_MS` while awake and at sleep.
//!
//! And, beside it, what the DO knows of its saves (`Saves`): which save of
//! `/data` is current, the snapshot that caches it, what each start that
//! came up restored, and whether that went back in time (a rollback).

use fragment_proto::computer::{ComputerRestore, LifeEnd, RestoreSource};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The id of a person's computer: one each for now (decision 13), so it is
/// derived from the owner and making it twice makes one. Later computers
/// (ephemeral ones, machines brought along) get ids of their own; nothing
/// but the person's first computer assumes this one.
pub fn default_computer_of(owner: &str) -> String {
    let digest = Sha256::digest(format!("fragment computer\0{owner}").as_bytes());
    format!("computer:{}", &hex::encode(digest)[..24])
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
/// The DO's sleep sequence (hold, save, snapshot, signal, five seconds of
/// exit, destroy) finishes within this, or it is asked again.
pub const SLEEP_DEADLINE_MS: i64 = 60_000;
/// Awake time is metered at least this often.
pub const METER_EVERY_MS: i64 = 5 * 60_000;
/// The pause before a failed start is tried again, doubled for each
/// failure before it.
pub const RETRY_PAUSE_MS: i64 = 5_000;

const _: () = assert!(PREWAKE_MS < IDLE_MS, "a pre-wake holds less than a record");
const _: () = assert!(RETRY_PAUSE_MS << STARTS_FAILED_MAX < START_DEADLINE_MS, "retries stay within a start's deadline");

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
    Opened { socket: Socket },
    Closed { socket: Socket },
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
    /// Put it to sleep, then report `Asleep`.
    Sleep { generation: u64 },
    /// Meter awake time from `from_ms` to `to_ms` to the owner.
    Meter { from_ms: i64, to_ms: i64 },
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
}

/// How one life ended: the start it was, and by what.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    pub generation: u64,
    pub by: LifeEnd,
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

    /// The running start's container is gone: if it came up, that was a
    /// life, and it ended `by` this.
    fn end_life(&mut self, by: LifeEnd, step: &mut Step) {
        if std::mem::take(&mut self.ready) {
            assert!(step.ended.is_none(), "one event ends one life");
            step.ended = Some(Ended { generation: self.generation, by });
        }
    }

    fn sleep(&mut self, generation: u64, now_ms: i64, step: &mut Step) {
        self.phase = Phase::Sleeping { generation, since_ms: now_ms };
        step.actions.push(Action::Sleep { generation });
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
    /// tab that comes back opens again, and wakes it).
    fn gone(&mut self, now_ms: i64, step: &mut Step) {
        self.meter(now_ms, step);
        self.phase = Phase::Asleep;
        self.keepalives = 0;
        self.tabs = 0;
        if std::mem::take(&mut self.wake_after_sleep) || self.wanted(now_ms) {
            self.start(now_ms, step);
        }
    }

    fn next_alarm(&self) -> Option<i64> {
        match &self.phase {
            Phase::Asleep => self.retry_at_ms,
            Phase::Failed { .. } => None,
            Phase::Starting { since_ms, .. } => Some(since_ms + START_DEADLINE_MS),
            Phase::Sleeping { since_ms, .. } => Some(since_ms + SLEEP_DEADLINE_MS),
            Phase::Awake { .. } => {
                let meter = self.metered_to_ms + METER_EVERY_MS;
                Some(if self.always_on || self.open() { meter } else { meter.min(self.held_until_ms) })
            }
        }
    }

    /// Applies `event` at `now_ms`.
    pub fn apply(&mut self, event: Event, now_ms: i64) -> Step {
        let mut step = Step::default();
        match (self.phase.clone(), event) {
            (phase, Event::Wake { why }) => {
                match phase {
                    Phase::Failed { why: failed } if why != Wake::Owner => {
                        step.refused = Some(format!("it won't wake: {failed} (its owner can try again)"));
                    }
                    Phase::Failed { .. } | Phase::Asleep => {
                        self.failures = 0;
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
            // a completion of an earlier start, or a report of one that is
            // not running: nothing it says is true of this one
            (_, Event::Ready { .. } | Event::StartFailed { .. } | Event::SnapshotFailed { .. } | Event::Asleep { .. } | Event::Exited { .. })
                if !matches!(&self.phase, Phase::Sleeping { .. } | Phase::Awake { .. } | Phase::Starting { .. }) => {}
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
            (_, Event::Ready { .. } | Event::StartFailed { .. } | Event::SnapshotFailed { .. } | Event::Asleep { .. } | Event::Exited { .. }) => {}
            (_, Event::Opened { socket }) => {
                match socket {
                    Socket::Keepalive => self.keepalives += 1,
                    Socket::Tab => self.tabs += 1,
                }
                if matches!(self.phase, Phase::Asleep) {
                    self.hold(now_ms + IDLE_MS);
                    self.start(now_ms, &mut step);
                }
            }
            (_, Event::Closed { socket }) => {
                let count = match socket {
                    Socket::Keepalive => &mut self.keepalives,
                    Socket::Tab => &mut self.tabs,
                };
                *count = count.saturating_sub(1);
                // a socket that closes as its container goes down (or after)
                // holds nothing up
                if matches!(self.phase, Phase::Starting { .. } | Phase::Awake { .. }) {
                    self.hold(now_ms + IDLE_MS);
                }
            }
            (Phase::Starting { generation, .. } | Phase::Awake { generation, .. }, Event::Sleep) => {
                self.held_until_ms = now_ms;
                self.wake_after_sleep = false;
                self.keepalives = 0;
                self.tabs = 0;
                self.sleep(generation, now_ms, &mut step);
            }
            (Phase::Sleeping { .. }, Event::Sleep) => self.wake_after_sleep = false,
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
                if !self.wanted(now_ms) {
                    self.sleep(generation, now_ms, &mut step);
                }
            }
            (Phase::Sleeping { generation, since_ms }, Event::Alarm) => {
                if now_ms >= since_ms + SLEEP_DEADLINE_MS {
                    // ask again: the DO's sequence ends in a destroy either way
                    self.sleep(generation, now_ms, &mut step);
                }
            }
            (Phase::Failed { .. }, Event::Alarm) => {}
        }
        step.alarm_ms = self.next_alarm();
        self.assert_valid();
        step
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
    }
}

/// A save of `/data`: the backup one sleep took. Its `DirectoryBackup`
/// record is the DO's (the authority on what a wake restores, handed back to
/// restore and to delete it); this is what the platform knows of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Save {
    /// The record's id (a UUID), which a snapshot names as its save.
    pub id: String,
    /// The start whose sleep took it.
    pub generation: u64,
    pub at_ms: i64,
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
/// happened; nothing here reaches the container.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Saves {
    /// The current save (`None` until a sleep saves, or for a record kept
    /// before the platform knew its time).
    backup: Option<Save>,
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
}

/// A start that came up, and the image's reference it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    pub generation: u64,
    pub image: String,
}

impl Saves {
    /// What a start restores: the snapshot only when it caches the current
    /// save (`backup`, the id of the record the DO holds) for the pinned
    /// image (`image`, its reference: `None` with snapshots off); otherwise
    /// the image and the save, or nothing when there is no save.
    pub fn plan(&self, backup: Option<&str>, image: Option<&str>) -> Plan {
        let Some(save) = backup else { return Plan::Nothing };
        match (&self.snapshot, image) {
            (Some(s), Some(image)) if s.save == save && s.image == image => Plan::Snapshot { id: s.id.clone() },
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

    /// A life ended (the lifecycle's `Step::ended`).
    pub fn ended(&mut self, ended: Ended) {
        assert!(self.ended.is_none_or(|e| e.generation < ended.generation), "a life ends once, after the lives before it");
        self.ended = Some(ended);
    }

    /// The start `generation` came up on `image` (its reference), having
    /// restored `from` (`backup`: the id of the record the DO holds): what
    /// it restored, and whether that went back in time, which is counted.
    /// `None` when another start began meanwhile (this one was superseded:
    /// nothing is recorded).
    pub fn came_up(&mut self, generation: u64, from: RestoreSource, backup: Option<&str>, image: Option<&str>, now_ms: i64) -> Option<ComputerRestore> {
        if self.starting != Some(Starting { generation, from }) {
            return None;
        }
        self.starting = None;
        self.running = image.map(|image| Running { generation, image: image.to_string() });
        assert_eq!(from == RestoreSource::Nothing, backup.is_none(), "a start restores the save there is");
        let save = backup.and_then(|id| self.backup.as_ref().filter(|b| b.id == id));
        let ended = self.ended.take();
        // a life's work since its own start is in no save unless its sleep
        // saved it: a crash loses it, and so does a sleep whose save failed
        let rollback = match ended {
            None => false,
            Some(Ended { by: LifeEnd::Exit, .. }) => true,
            Some(Ended { by: LifeEnd::Sleep, generation: life }) => save.is_none_or(|s| s.generation != life),
        };
        let restored = ComputerRestore {
            generation,
            from,
            save: backup.map(str::to_string),
            saved_at: save.map(|s| s.at_ms),
            age_ms: save.map(|s| now_ms.saturating_sub(s.at_ms).max(0)),
            after: ended.map(|e| e.by),
            rollback,
            at: now_ms,
        };
        self.rollbacks += u64::from(rollback);
        self.restored = Some(restored.clone());
        Some(restored)
    }

    /// A sleep of the start `generation` saved `/data` as the backup `id`
    /// at `at_ms`: it is the current save, and a snapshot of any other save
    /// caches nothing a wake would use.
    pub fn saved(&mut self, generation: u64, id: &str, at_ms: i64) {
        assert!(!id.is_empty(), "a save has an id");
        if self.snapshot.as_ref().is_some_and(|s| s.save != id) {
            self.snapshot = None;
        }
        self.backup = Some(Save { id: id.to_string(), generation, at_ms });
        assert!(self.snapshot.as_ref().is_none_or(|s| s.save == id), "a snapshot kept is of the current save");
    }

    /// A sleep of `generation` could not save: whether that is news (no
    /// save of this start is kept). A slow sleep is asked for again
    /// (`SLEEP_DEADLINE_MS`), and the second ask's save fails once the
    /// first's destroy took the container: the first one saved.
    pub fn save_failed(&self, generation: u64) -> bool {
        !self.backup.as_ref().is_some_and(|b| b.generation == generation)
    }

    /// A sleep took snapshot `id` of `image` after its save `save`: kept
    /// when `save` is still the current save (a second ask's save may have
    /// replaced it meanwhile). A sleep whose own save failed takes none,
    /// and a snapshot that fails forgets nothing: the one kept is still a
    /// cache of its own save (F11).
    pub fn snapshotted(&mut self, save: &str, id: &str, image: &str) -> bool {
        let current = self.backup.as_ref().is_some_and(|b| b.id == save);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const T: i64 = 1_800_000_000_000;

    #[test]
    fn a_persons_computer_has_one_stable_id() {
        let a = default_computer_of("id:aaaa");
        assert_eq!(a, default_computer_of("id:aaaa"));
        assert_ne!(a, default_computer_of("id:aaab"));
        assert!(a.starts_with("computer:") && a.len() == "computer:".len() + 24 && a[9..].bytes().all(|b| b.is_ascii_hexdigit()));
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
        assert!(l.apply(Event::Alarm, late - 1).actions.iter().all(|a| matches!(a, Action::Meter { .. })), "busy, it stays awake");
        let s = l.apply(Event::Exited { generation: g }, late);
        assert!(s.actions.contains(&Action::Start { generation: g + 1 }), "busy when it died, it starts again: {s:?}");
        assert_eq!(s.ended, Some(Ended { generation: g, by: LifeEnd::Exit }));
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
        assert_eq!(s.ended, Some(Ended { generation: g, by: LifeEnd::Sleep }), "signalled by its sleep");
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
        assert!(matches!(l.phase, Phase::Failed { .. }) && s.ended == Some(Ended { generation, by: LifeEnd::Exit }), "{s:?} {:?}", l.phase);
    }

    const IMAGE: &str = "registry/stub@sha256:1";

    /// Goal (P3): a wake uses the snapshot only while it caches the current
    /// save for the pinned image; any other is the image and the save, and
    /// a computer never saved starts empty. Method: each way the two can
    /// disagree: another image (a pin, a redeploy), snapshots off, a newer
    /// save, a snapshot of a save that has gone.
    #[test]
    fn a_snapshot_is_used_only_for_its_save_and_image() {
        let mut s = Saves::default();
        assert_eq!(s.plan(None, Some(IMAGE)), Plan::Nothing);
        s.saved(1, "b1", T);
        assert!(s.snapshotted("b1", "s1", IMAGE));
        assert_eq!(s.plan(Some("b1"), Some(IMAGE)), Plan::Snapshot { id: "s1".into() });
        assert_eq!(s.plan(Some("b1"), Some("registry/stub@sha256:2")), Plan::Backup, "another image's");
        assert_eq!(s.plan(Some("b1"), None), Plan::Backup, "snapshots off");
        assert_eq!(s.plan(Some("b0"), Some(IMAGE)), Plan::Backup, "the record held names another save");
        // the next sleep saves and takes no snapshot: the old one is dropped
        s.saved(2, "b2", T + 1);
        assert_eq!(s.plan(Some("b2"), Some(IMAGE)), Plan::Backup);
        assert_eq!(s.snapshot, None);
        // a snapshot taken with a save that is no longer current is not kept
        assert!(!s.snapshotted("b1", "s1-late", IMAGE));
        assert_eq!(s.plan(Some("b2"), Some(IMAGE)), Plan::Backup);
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
        s.saved(1, "b1", T + 10);
        let of = s.image_of(1).expect("the image it runs").to_string();
        assert_eq!(of, IMAGE);
        assert!(s.snapshotted("b1", "s1", &of));
        assert_eq!(s.plan(Some("b1"), Some(NEXT)), Plan::Backup, "the new image, and the save");
        assert_eq!(s.plan(Some("b1"), Some(IMAGE)), Plan::Snapshot { id: "s1".into() }, "pinned back, the snapshot serves again");
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
        s.saved(3, "b-old", T);
        // the first ask saves and snapshots
        s.saved(4, "b1", T + 70_000);
        assert!(s.snapshotted("b1", "s1", IMAGE));
        // the second: its save fails once the first's destroy took the
        // container, so it takes no snapshot, and that is not news
        assert!(!s.save_failed(4), "the first ask saved this start");
        assert!(s.save_failed(5), "a start whose sleep saved nothing is news");
        assert_eq!(s.plan(Some("b1"), Some(IMAGE)), Plan::Snapshot { id: "s1".into() });
        // the other order: the second's save lands while the first snapshots
        let mut s = Saves::default();
        s.saved(4, "b1", T);
        s.saved(4, "b2", T + 1_000);
        assert!(!s.snapshotted("b1", "s1", IMAGE), "the first's snapshot caches a save that is no longer current");
        assert_eq!(s.plan(Some("b2"), Some(IMAGE)), Plan::Backup);
        assert!(s.snapshotted("b2", "s2", IMAGE));
        assert_eq!(s.plan(Some("b2"), Some(IMAGE)), Plan::Snapshot { id: "s2".into() });
    }

    /// The lifecycle and its saves, as the Computer DO drives them: each
    /// step's end told to the saves, each start planned and recorded.
    struct Computer {
        life: Lifecycle,
        saves: Saves,
        /// The record the DO holds (its id).
        backup: Option<String>,
        now: i64,
    }

    impl Computer {
        fn new() -> Computer {
            Computer { life: Lifecycle::new(), saves: Saves::default(), backup: None, now: T }
        }

        fn apply(&mut self, e: Event) -> Step {
            self.now += 1_000;
            let s = self.life.apply(e, self.now);
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
            let plan = self.saves.plan(self.backup.as_deref(), Some(IMAGE));
            self.saves.starting(generation, plan.source());
            let r = self.saves.came_up(generation, plan.source(), self.backup.as_deref(), Some(IMAGE), self.now).expect("the start under way");
            self.apply(Event::Ready { generation });
            r
        }

        /// The owner's sleep, saving `/data` as `save` (or failing to).
        fn sleep(&mut self, save: Option<&str>) {
            let g = self.life.generation();
            self.apply(Event::Sleep);
            match save {
                Some(id) => {
                    self.saves.saved(g, id, self.now);
                    self.backup = Some(id.to_string());
                }
                None => assert!(self.saves.save_failed(g)),
            }
            self.apply(Event::Asleep { generation: g });
        }
    }

    /// Goal (P7): each start that comes up says what it restored, how old
    /// that was, and how the life before it ended; a start that went back
    /// in time (after a crash, or a sleep whose save failed) is counted, and
    /// one after a sleep that saved is not. Method: one computer's history,
    /// read after each start.
    #[test]
    fn a_wake_says_what_it_restored() {
        let mut c = Computer::new();
        let r = c.wake();
        assert_eq!((r.from, r.save.clone(), r.after, r.rollback), (RestoreSource::Nothing, None, None, false), "its first start");
        c.sleep(Some("b1"));
        let saved_at = c.saves.backup.as_ref().map(|b| b.at_ms).expect("a save");
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
        // a sleep whose save failed: the next wake is a rollback too (F6)
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
        let plan = c.saves.plan(c.backup.as_deref(), Some(IMAGE));
        assert_eq!(plan, Plan::Snapshot { id: "s1".into() });
        c.saves.starting(generation, plan.source());
        assert_eq!(c.saves.start_from(generation), Some(RestoreSource::Snapshot), "its exit now is the start's own failure");
        assert_eq!(c.saves.snapshot_failed().map(|s| s.id), Some("s1".into()));
        let s = c.apply(Event::SnapshotFailed { generation, why: "expired".into() });
        let Some(Action::Start { generation: next }) = s.actions.first().cloned() else { panic!("{s:?}") };
        assert_eq!(c.saves.plan(c.backup.as_deref(), Some(IMAGE)), Plan::Backup);
        let r = c.come_up(next);
        assert_eq!((r.from, r.save.as_deref(), r.rollback, c.life.failures), (RestoreSource::Backup, Some("b1"), false, 0), "{r:?}");
        assert_eq!(c.saves.came_up(generation, RestoreSource::Snapshot, Some("b1"), Some(IMAGE), c.now), None, "the broken start never comes up after");
    }

    /// Goal: under any interleaving of events (duplicates, late reports,
    /// reordering, crashes, snapshots that will not start), the generation
    /// only grows, at most one container is ever running, metered intervals
    /// never overlap or run backwards, a computer is never left asleep while
    /// it is wanted and not failed, one that dies busy starts again unless
    /// it gave up, and each life (a start that came up) ends once. Its saves
    /// ride along as the DO keeps them, so their own assertions run too.
    #[test]
    fn simulated_interleavings_keep_the_invariants() {
        // a small xorshift: the sequence is a pure function of the seed
        fn rng(seed: &mut u64) -> u64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            *seed
        }
        for seed0 in 1..=8u64 {
            let mut seed = seed0.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let mut l = Lifecycle::new();
            let mut saves = Saves::default();
            let mut backup: Option<String> = None;
            let mut now = T;
            let mut metered: Vec<(i64, i64)> = vec![];
            let mut generation = 0;
            let mut came_up = std::collections::BTreeSet::new();
            let mut ended = std::collections::BTreeSet::new();
            for step in 0..4_000u32 {
                now += (rng(&mut seed) % 120_000) as i64;
                let g = l.generation().saturating_sub(rng(&mut seed) % 2);
                let event = match rng(&mut seed) % 12 {
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
                    _ => Event::Alarm,
                };
                // a crash of the running start while its guest holds a keepalive
                let busy_crash = matches!(&event, Event::Exited { generation: e } if matches!(l.phase, Phase::Awake { generation, .. } | Phase::Starting { generation, .. } if generation == *e)) && l.keepalives > 0;
                let coming_up = matches!(&event, Event::Ready { generation: e } if matches!(l.phase, Phase::Starting { generation, .. } if generation == *e));
                let s = l.apply(event, now);
                if let Some(e) = s.ended {
                    assert!(came_up.contains(&e.generation), "only a start that came up ends a life (seed {seed0})");
                    assert!(ended.insert(e.generation), "a life ends once (seed {seed0})");
                    saves.ended(e);
                }
                if coming_up {
                    assert!(matches!(l.phase, Phase::Awake { .. }));
                    came_up.insert(l.generation());
                    let from = saves.plan(backup.as_deref(), Some(IMAGE)).source();
                    saves.starting(l.generation(), from);
                    assert!(saves.came_up(l.generation(), from, backup.as_deref(), Some(IMAGE), now).is_some(), "the start that came up records what it restored");
                }
                if busy_crash && !matches!(l.phase, Phase::Failed { .. }) {
                    assert!(s.actions.iter().any(|a| matches!(a, Action::Start { .. })), "busy when it died, it starts again (seed {seed0}): {l:?}");
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
                        Action::Sleep { generation: g } => {
                            assert_eq!(*g, l.generation(), "only the current start sleeps");
                            // most sleeps save; some fail, and some are asked twice
                            if !rng(&mut seed).is_multiple_of(4) {
                                let id = format!("b{seed0}-{step}");
                                saves.saved(*g, &id, now);
                                backup = Some(id);
                            }
                        }
                    }
                }
                assert!(saves.rollbacks() <= ended.len() as u64, "each rollback is one life's loss");
                if matches!(l.phase, Phase::Asleep) && l.wanted(now) && l.retry_at_ms.is_none() {
                    panic!("asleep while wanted, with no retry due (seed {seed0}): {l:?}");
                }
                // whatever is running has an alarm watching it (a past one fires at once)
                if !matches!(l.phase, Phase::Asleep | Phase::Failed { .. }) {
                    assert!(s.alarm_ms.is_some(), "a running computer has its alarm: {l:?}");
                }
            }
            assert!(!came_up.is_empty() && !ended.is_empty() && saves.rollbacks() > 0, "the simulation reached lives, their ends, and rollbacks (seed {seed0})");
        }
    }
}
