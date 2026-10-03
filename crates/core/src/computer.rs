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
//! - **Sleeping.** The DO's own sequence (save, snapshot, signal, wait,
//!   destroy), reported back by `Asleep`.
//! - **Failing.** A start that fails, or never reports ready, is tried
//!   again with a growing pause; after `STARTS_FAILED_MAX` in a row the
//!   computer "won't wake" until its owner asks (lesson 4): no more
//!   container starts are paid for.
//! - **Metering.** Awake time, in intervals that never overlap, every
//!   `METER_EVERY_MS` while awake and at sleep.

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
/// The DO's sleep sequence (save, snapshot, signal, five seconds of exit,
/// destroy) finishes within this, or it is asked again.
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
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
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
        self.phase = Phase::Starting { generation: self.generation, since_ms: now_ms };
        step.actions.push(Action::Start { generation: self.generation });
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
                self.metered_to_ms = now_ms;
                self.phase = Phase::Awake { generation, since_ms: now_ms };
            }
            (Phase::Starting { generation, .. }, Event::StartFailed { generation: g, why }) if g == generation => self.failed(why, now_ms),
            // a completion of an earlier start, or a report of one that is
            // not running: nothing it says is true of this one
            (_, Event::Ready { .. } | Event::StartFailed { .. } | Event::Asleep { .. } | Event::Exited { .. })
                if !matches!(&self.phase, Phase::Sleeping { .. } | Phase::Awake { .. } | Phase::Starting { .. }) => {}
            (Phase::Sleeping { generation, .. }, Event::Asleep { generation: g }) if g == generation => self.gone(now_ms, &mut step),
            (Phase::Awake { generation, .. } | Phase::Starting { generation, .. }, Event::Exited { generation: g }) if g == generation => {
                // a container that died under us counts against its starts
                self.failures += 1;
                if self.failures >= STARTS_FAILED_MAX {
                    self.meter(now_ms, &mut step);
                    self.phase = Phase::Failed { why: "its container kept stopping".into() };
                } else {
                    self.gone(now_ms, &mut step);
                }
            }
            (Phase::Sleeping { generation, .. }, Event::Exited { generation: g }) if g == generation => self.gone(now_ms, &mut step),
            (_, Event::Ready { .. } | Event::StartFailed { .. } | Event::Asleep { .. } | Event::Exited { .. }) => {}
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    /// Goal: under any interleaving of events (duplicates, late reports,
    /// reordering), the generation only grows, at most one container is
    /// ever running, metered intervals never overlap or run backwards, and
    /// a computer is never left asleep while it is wanted and not failed.
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
            let mut now = T;
            let mut metered: Vec<(i64, i64)> = vec![];
            let mut generation = 0;
            for _ in 0..4_000 {
                now += (rng(&mut seed) % 120_000) as i64;
                let g = l.generation().saturating_sub(rng(&mut seed) % 2);
                let event = match rng(&mut seed) % 11 {
                    0 => Event::Wake { why: [Wake::Record, Wake::Presence, Wake::Tab, Wake::Owner, Wake::Joined][(rng(&mut seed) % 5) as usize] },
                    1 => Event::Ready { generation: g },
                    2 => Event::StartFailed { generation: g, why: "sim".into() },
                    3 => Event::Opened { socket: if rng(&mut seed).is_multiple_of(2) { Socket::Tab } else { Socket::Keepalive } },
                    4 => Event::Closed { socket: if rng(&mut seed).is_multiple_of(2) { Socket::Tab } else { Socket::Keepalive } },
                    5 => Event::Asleep { generation: g },
                    6 => Event::Exited { generation: g },
                    7 => Event::AlwaysOn { on: rng(&mut seed).is_multiple_of(4) },
                    8 if rng(&mut seed).is_multiple_of(3) => Event::Sleep,
                    _ => Event::Alarm,
                };
                let s = l.apply(event, now);
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
                        Action::Sleep { generation: g } => assert_eq!(*g, l.generation(), "only the current start sleeps"),
                    }
                }
                if matches!(l.phase, Phase::Asleep) && l.wanted(now) && l.retry_at_ms.is_none() {
                    panic!("asleep while wanted, with no retry due (seed {seed0}): {l:?}");
                }
                // whatever is running has an alarm watching it (a past one fires at once)
                if !matches!(l.phase, Phase::Asleep | Phase::Failed { .. }) {
                    assert!(s.alarm_ms.is_some(), "a running computer has its alarm: {l:?}");
                }
            }
        }
    }
}
