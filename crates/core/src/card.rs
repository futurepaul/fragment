//! A fragment app's preview card (docs/cloudflare-v1.md, decision 31):
//! after each move of `live`, the platform renders the app's page with
//! Browser Rendering, as a visitor without an account sees it, and keeps
//! the image as one of the fragment's blobs, which `GET
//! /api/f/{name}/card` serves its members (docs/api.md, Cards).
//!
//! This is the card's pure part, run by the cell (cell/src/card.rs):
//!
//! - who is shot, and as whom (`visitor`): an app or a brain, never a
//!   chat or an agent (they are not in the shell's Apps list); a public
//!   fragment as anyone sees it, a link fragment as anyone holding its
//!   share link sees it, and a members-only fragment not at all (an
//!   anonymous visitor gets its refusal, which says nothing of the app);
//! - what a shot must be (`check_image`): a JPEG of `WIDTH` × `HEIGHT`, at
//!   most `BYTES_MAX`;
//! - its meter's reference (`meter_ref`) and billed time (`billed_ms`);
//! - the schedule (`Cards`): one shot out at a time per fragment, the
//!   newest live wins (a shot of an older one is dropped as it lands), a
//!   failure is tried again after a wait that doubles, at most
//!   `ATTEMPTS_MAX` tries in all, and then given up quietly: the card
//!   before stays, and nothing else changes.

use fragment_proto::{FragmentKind, Visibility};
use serde::{Deserialize, Serialize};

/// The viewport a page is shot at, at a device scale of 1: a laptop's.
pub const WIDTH: u16 = 1280;
pub const HEIGHT: u16 = 800;
pub const SCALE: u8 = 1;
/// What a card is stored and served as.
pub const MEDIA_TYPE: &str = "image/jpeg";
/// The JPEG qualities a shot tries, in turn: the next when the image is
/// over `BYTES_MAX`.
pub const QUALITIES: [u8; 2] = [70, 40];
/// A card's largest size. Its base64 (4/3 of it) stays under the 1 MiB a
/// Workers WebSocket message has held, so a shot crosses as one message.
pub const BYTES_MAX: usize = 512 * 1024;
/// Tries at one live's card, the first included: four retries, as a job
/// step gets.
pub const ATTEMPTS_MAX: u32 = 5;
/// One shot's deadline, from acquiring the browser to the image.
pub const SHOT_TIMEOUT_MS: u64 = 45_000;
/// A page's load event is waited for at most this; then it is shot as it is.
pub const LOAD_TIMEOUT_MS: u64 = 15_000;
/// After its load, a page's scripts render for this long before the shot
/// (an app reads its data over its socket after the load).
pub const SETTLE_MS: u64 = 1_000;
/// The browser session's inactivity timeout, Browser Rendering's least
/// (`keep_alive`): a session whose close was lost ends this long after its
/// last use, and is billed for it (`billed_ms`).
pub const KEEP_ALIVE_MS: u64 = 10_000;
/// A shot out this long without word has been lost (its consumer died, or
/// its message went): it counts as a failed try.
pub const LEASE_MS: i64 = 3 * 60_000;

const _: () = assert!(2 * (SHOT_TIMEOUT_MS + KEEP_ALIVE_MS) < LEASE_MS as u64, "a shot reports well within its lease");
const _: () = assert!(BYTES_MAX.div_ceil(3) * 4 < 1024 * 1024, "a card's base64 fits one 1 MiB WebSocket message");
const _: () = assert!(LOAD_TIMEOUT_MS + SETTLE_MS < SHOT_TIMEOUT_MS, "a slow page is still shot within the deadline");

/// As whom the renderer opens the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visitor {
    /// A public fragment: as anyone sees it.
    Anyone,
    /// A link fragment: as anyone holding its share link sees it (what a
    /// member of it can already see; the card goes to members only).
    LinkHolder,
}

/// Why a live gets no card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Skip {
    /// A chat or an agent: not an app (the shell's Apps list has no place
    /// for it).
    NotAnApp,
    /// A members-only fragment: an anonymous visitor gets its refusal.
    MembersOnly,
    /// The deployment serves fragments by path, with no origin of their own
    /// (no `FRAGMENT_HOST_SUFFIX` and `FRAGMENT_PLATFORM_URL`).
    NoAddress,
    /// The deployment has no browser to shoot with: no `browser` binding,
    /// and no `FRAGMENT_BROWSER_URL` (docs/self-host.md, seam 7).
    NoBrowser,
    /// Its owner pays for nothing now (a guest), or is past the overdraft.
    OwnerPays,
}

impl Skip {
    pub fn message(self) -> &'static str {
        match self {
            Skip::NotAnApp => "a chat or an agent is not an app: it has no card",
            Skip::MembersOnly => "a members-only fragment has no card: a visitor without an account sees only its refusal",
            Skip::NoAddress => "this deployment serves fragments by path, so a visitor has no page of theirs to open",
            Skip::NoBrowser => "this deployment has no browser to shoot cards with (no `browser` binding, nor FRAGMENT_BROWSER_URL)",
            Skip::OwnerPays => "its owner's ledger takes no shot now (a guest pays for nothing; past the overdraft, nothing new is made)",
        }
    }
}

/// Whether a fragment of `kind` and `visibility` is shot, and as whom.
pub fn visitor(kind: FragmentKind, visibility: Visibility) -> Result<Visitor, Skip> {
    match (kind, visibility) {
        (FragmentKind::Chat | FragmentKind::Agent | FragmentKind::Skills, _) => Err(Skip::NotAnApp),
        (_, Visibility::Members) => Err(Skip::MembersOnly),
        (FragmentKind::App | FragmentKind::Brain, Visibility::Public) => Ok(Visitor::Anyone),
        (FragmentKind::App | FragmentKind::Brain, Visibility::Link) => Ok(Visitor::LinkHolder),
    }
}

/// The page the renderer opens: the fragment's front page on its own
/// origin (`scheme://host[:port]`), with its share link for a link holder.
pub fn page_url(origin: &str, visitor: Visitor, view_token: &str) -> String {
    assert!(!origin.ends_with('/'), "an origin has no path: {origin}");
    match visitor {
        Visitor::Anyone => format!("{origin}/"),
        Visitor::LinkHolder => {
            assert!(!view_token.is_empty() && view_token.bytes().all(|b| b.is_ascii_alphanumeric()), "a view token is URL-safe");
            format!("{origin}/?view={view_token}")
        }
    }
}

/// Why a shot's bytes are no card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageFault {
    Empty,
    TooLarge { size: usize },
    NotJpeg,
    Size { width: u16, height: u16 },
}

impl ImageFault {
    pub fn message(self) -> String {
        match self {
            ImageFault::Empty => "the shot is empty".into(),
            ImageFault::TooLarge { size } => format!("the shot is {size} bytes, over the {BYTES_MAX} a card holds"),
            ImageFault::NotJpeg => "the shot is no JPEG".into(),
            ImageFault::Size { width, height } => format!("the shot is {width}×{height}, not {WIDTH}×{HEIGHT}"),
        }
    }
}

/// Whether `bytes` are a card: a JPEG of the viewport's size, within the cap.
pub fn check_image(bytes: &[u8]) -> Result<(), ImageFault> {
    if bytes.is_empty() {
        return Err(ImageFault::Empty);
    }
    if bytes.len() > BYTES_MAX {
        return Err(ImageFault::TooLarge { size: bytes.len() });
    }
    match crate::media::jpeg_size(bytes) {
        None => Err(ImageFault::NotJpeg),
        Some((WIDTH, HEIGHT)) => Ok(()),
        Some((width, height)) => Err(ImageFault::Size { width, height }),
    }
}

/// The time a shot is billed for: its session's, from the acquire to the
/// close; a close that was lost leaves the session open until its
/// inactivity timeout, so that is billed too (the money path fails closed).
pub fn billed_ms(elapsed_ms: u64, closed: bool) -> u64 {
    elapsed_ms.saturating_add(if closed { 0 } else { KEEP_ALIVE_MS })
}

/// The meter row's reference for one try at a card: the fragment's meter
/// key (`<name>@<incarnation>`), the live it shows, when that live was
/// wanted (a live wanted again later is another card), and the try.
pub fn meter_ref(key: &str, ticket: &Ticket) -> String {
    assert!(valid_live(&ticket.live), "a ticket names a commit");
    format!("card:{key}:{}:{}:{}", &ticket.live[..12], ticket.since, ticket.attempt)
}

/// A commit's id, as code.storage names one (a git SHA-1 or SHA-256, hex).
pub fn valid_live(s: &str) -> bool {
    (12..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// How a fragment's failed tries back off: the wait after the first
/// failure, doubling, never past the longest (the deployment's delivery
/// retry settings, `FRAGMENT_DELIVERY_RETRY_S` and `_MAX_S`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pace {
    pub first_ms: i64,
    pub max_ms: i64,
}

impl Pace {
    /// The wait after `failures` failed tries (1 or more).
    pub fn wait_ms(self, failures: u32) -> i64 {
        assert!(failures >= 1, "a wait follows a failure");
        assert!(self.first_ms >= 1 && self.max_ms >= self.first_ms, "a pace waits a positive time, its longest no shorter than its first");
        let doubled = self.first_ms.saturating_mul(1i64 << (failures - 1).min(30));
        doubled.min(self.max_ms)
    }
}

/// A fragment's card and its schedule, kept whole (cell/src/card.rs keeps
/// it as one JSON value).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cards {
    /// The card kept now.
    pub card: Option<Card>,
    /// The newest live with no card yet, and its tries so far.
    pub wanted: Option<Wanted>,
    /// The shot out now: at most one per fragment.
    pub flight: Option<Flight>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    /// The image's blob (its SHA-256).
    pub blob: String,
    pub size: u64,
    /// The live commit it shows.
    pub live: String,
    /// The try that took it.
    pub attempt: u32,
    pub at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Wanted {
    pub live: String,
    /// When it was wanted: it names the card's tries apart from another
    /// time the same live is wanted.
    pub since: i64,
    pub failures: u32,
    /// When it is next tried.
    pub next_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Flight {
    pub live: String,
    pub since: i64,
    pub attempt: u32,
    /// Lost after this (`LEASE_MS`).
    pub until: i64,
}

/// One try at a card, as it is sent and reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ticket {
    pub live: String,
    pub since: i64,
    pub attempt: u32,
}

impl Flight {
    pub fn ticket(&self) -> Ticket {
        Ticket { live: self.live.clone(), since: self.since, attempt: self.attempt }
    }
}

/// Input the schedule refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// A live that is no commit id.
    Live,
    /// A shot whose blob is no SHA-256, or whose size is none.
    Blob,
}

/// What the schedule asks for now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Nothing to shoot.
    Idle,
    /// Nothing until then (a shot is out, or a retry waits).
    Wait(i64),
    /// Shoot this, now: it is out from here until its lease ends.
    Shoot(Ticket),
}

/// How a try ended, as its report says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// An image, checked (`check_image`) and stored as this blob.
    Shot { blob: String, size: u64 },
    /// No image; `retry` when another try may get one.
    Failed { retry: bool },
}

/// What a report (or a lost shot) did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The shot is the card now.
    Kept,
    /// A shot of a live that is no longer the newest: dropped.
    Stale,
    /// The try failed; the next waits until `at`.
    Retry { at: i64 },
    /// The last try failed: the live gets no card, and the one before stays.
    GaveUp { failures: u32 },
    /// No shot by that ticket is out (one reported twice, or after its
    /// lease): nothing changes.
    Late,
}

impl Cards {
    /// `live` moved to `live`: it is the one to shoot (newest wins), unless
    /// the card shows it already. Answers whether a shot is now wanted
    /// that was not; the same live again changes nothing.
    pub fn want(&mut self, live: &str, now: i64) -> Result<bool, Fault> {
        if !valid_live(live) {
            return Err(Fault::Live);
        }
        if self.card.as_ref().is_some_and(|c| c.live == live) {
            // back to the live the card shows: an older one out lands stale
            self.wanted = None;
            return Ok(false);
        }
        if self.wanted.as_ref().is_some_and(|w| w.live == live) {
            return Ok(false);
        }
        self.wanted = Some(Wanted { live: live.to_string(), since: now, failures: 0, next_at: now });
        Ok(true)
    }

    /// When the schedule next needs a look: a shot's lease end, else the
    /// wanted live's next try.
    pub fn due_at(&self) -> Option<i64> {
        match (&self.flight, &self.wanted) {
            (Some(f), _) => Some(f.until),
            (None, Some(w)) => Some(w.next_at),
            (None, None) => None,
        }
    }

    /// The live a shot would be of now, without sending it: the cell asks
    /// whether it is shot at all (`visitor`, the owner's ledger) first.
    pub fn due(&self, now: i64) -> Option<&Wanted> {
        match (&self.flight, &self.wanted) {
            (None, Some(w)) if w.next_at <= now => Some(w),
            _ => None,
        }
    }

    /// The wanted live gets no card (`Skip`): it is let go, and the card
    /// before stays. Only while no shot is out.
    pub fn skip(&mut self) -> Option<Wanted> {
        assert!(self.flight.is_none(), "a live is let go only while no shot is out");
        self.wanted.take()
    }

    /// What to do now. A shot it answers is out until its lease ends:
    /// while one is out, no other goes (one per fragment).
    pub fn next(&mut self, now: i64) -> Next {
        if let Some(f) = &self.flight {
            return Next::Wait(f.until);
        }
        let Some(w) = &self.wanted else { return Next::Idle };
        if w.next_at > now {
            return Next::Wait(w.next_at);
        }
        assert!(w.failures < ATTEMPTS_MAX, "a live out of tries is let go");
        let flight = Flight { live: w.live.clone(), since: w.since, attempt: w.failures + 1, until: now + LEASE_MS };
        let ticket = flight.ticket();
        self.flight = Some(flight);
        Next::Shoot(ticket)
    }

    /// A shot was sent nowhere (the queue refused it): it is not out, and
    /// the next look tries again, costing no try.
    pub fn unsend(&mut self, ticket: &Ticket) {
        if self.flight.as_ref().is_some_and(|f| f.ticket() == *ticket) {
            self.flight = None;
        }
    }

    /// A shot of a live let go (`skip`) or lost: the lease ended without a
    /// report, which is a failed try.
    pub fn expire(&mut self, now: i64, pace: Pace) -> Option<Verdict> {
        let f = self.flight.as_ref().filter(|f| f.until <= now)?;
        let ticket = f.ticket();
        Some(self.landed(&ticket, Outcome::Failed { retry: true }, now, pace).expect("a failure carries nothing to refuse"))
    }

    /// A try's report. Only the shot out by that ticket counts; a shot of
    /// a live no longer the newest is dropped (newest wins).
    pub fn landed(&mut self, ticket: &Ticket, outcome: Outcome, now: i64, pace: Pace) -> Result<Verdict, Fault> {
        if let Outcome::Shot { blob, size } = &outcome {
            if !crate::blob::valid_sha(blob) || *size == 0 || *size > BYTES_MAX as u64 {
                return Err(Fault::Blob);
            }
        }
        if self.flight.as_ref().is_none_or(|f| f.ticket() != *ticket) {
            return Ok(Verdict::Late);
        }
        self.flight = None;
        let Some(w) = self.wanted.as_mut().filter(|w| w.live == ticket.live && w.since == ticket.since) else {
            return Ok(Verdict::Stale);
        };
        assert_eq!(w.failures + 1, ticket.attempt, "the shot out is the wanted live's next try");
        Ok(match outcome {
            Outcome::Shot { blob, size } => {
                self.card = Some(Card { blob, size, live: ticket.live.clone(), attempt: ticket.attempt, at_ms: now });
                self.wanted = None;
                Verdict::Kept
            }
            Outcome::Failed { retry } => {
                w.failures += 1;
                if !retry || w.failures >= ATTEMPTS_MAX {
                    let failures = w.failures;
                    self.wanted = None;
                    Verdict::GaveUp { failures }
                } else {
                    w.next_at = now + pace.wait_ms(w.failures);
                    Verdict::Retry { at: w.next_at }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::price::{PriceBook, Priced, Usage};

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const PACE: Pace = Pace { first_ms: 10_000, max_ms: 3_600_000 };
    const T0: i64 = 1_790_000_000_000;

    fn blob(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn shot(n: u8) -> Outcome {
        Outcome::Shot { blob: blob(n), size: 40_000 }
    }

    fn shoot(cards: &mut Cards, now: i64) -> Ticket {
        match cards.next(now) {
            Next::Shoot(t) => t,
            other => panic!("expected a shot, got {other:?}"),
        }
    }

    /// Goal: a deploy is shot once, and its shot is the card. Method: want
    /// a live, send its shot, report the image.
    #[test]
    fn a_deploy_is_shot_and_becomes_the_card() {
        let mut c = Cards::default();
        assert_eq!(c.next(T0), Next::Idle, "nothing deployed, nothing shot");
        assert_eq!(c.want(A, T0), Ok(true));
        assert_eq!(c.due_at(), Some(T0));
        let t = shoot(&mut c, T0);
        assert_eq!(t, Ticket { live: A.into(), since: T0, attempt: 1 });
        assert_eq!(c.due_at(), Some(T0 + LEASE_MS), "a shot out is looked at again when its lease ends");
        assert_eq!(c.landed(&t, shot(1), T0 + 2_000, PACE), Ok(Verdict::Kept));
        assert_eq!(c.card, Some(Card { blob: blob(1), size: 40_000, live: A.into(), attempt: 1, at_ms: T0 + 2_000 }));
        assert_eq!((c.wanted.clone(), c.flight.clone(), c.next(T0 + 3_000), c.due_at()), (None, None, Next::Idle, None));
    }

    /// Goal: the newest live wins, and one shot is out at a time. Method:
    /// two deploys before any shot (only the second is shot), then a
    /// deploy while a shot is out (no second shot goes; the first lands
    /// stale and the newest goes next).
    #[test]
    fn the_newest_live_wins_one_shot_at_a_time() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        c.want(B, T0 + 1).unwrap();
        let t = shoot(&mut c, T0 + 2);
        assert_eq!(t.live, B, "a live replaced before its shot is never shot");
        c.want(C, T0 + 3).unwrap();
        assert_eq!(c.next(T0 + 4), Next::Wait(T0 + 2 + LEASE_MS), "while a shot is out, no other goes");
        assert_eq!(c.landed(&t, shot(2), T0 + 5, PACE), Ok(Verdict::Stale), "a shot of a live no longer the newest is dropped");
        assert_eq!(c.card, None);
        let t = shoot(&mut c, T0 + 6);
        assert_eq!((t.live.as_str(), t.attempt), (C, 1));
        assert_eq!(c.landed(&t, shot(3), T0 + 7, PACE), Ok(Verdict::Kept));
        assert_eq!(c.card.as_ref().map(|k| k.live.as_str()), Some(C));
        // a failure of a stale shot costs the newest no try
        c.want(A, T0 + 8).unwrap();
        let old = shoot(&mut c, T0 + 9);
        c.want(B, T0 + 10).unwrap();
        assert_eq!(c.landed(&old, Outcome::Failed { retry: true }, T0 + 11, PACE), Ok(Verdict::Stale));
        assert_eq!(c.wanted.as_ref().map(|w| (w.live.as_str(), w.failures)), Some((B, 0)));
    }

    /// Goal: a second deploy replaces the card; the card before stays until
    /// the new one is kept. Method: two lives shot in turn.
    #[test]
    fn a_second_deploy_replaces_the_card() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let t = shoot(&mut c, T0);
        c.landed(&t, shot(1), T0 + 1, PACE).unwrap();
        c.want(B, T0 + 2).unwrap();
        let t = shoot(&mut c, T0 + 2);
        assert_eq!(c.card.as_ref().map(|k| k.blob.clone()), Some(blob(1)), "the card before serves while the next is shot");
        c.landed(&t, shot(2), T0 + 3, PACE).unwrap();
        assert_eq!(c.card.as_ref().map(|k| (k.blob.clone(), k.live.as_str())), Some((blob(2), B)));
        // back to the live the card shows (a forced push of live): nothing to shoot
        c.want(C, T0 + 4).unwrap();
        let out = shoot(&mut c, T0 + 4);
        assert_eq!(c.want(B, T0 + 5), Ok(false));
        assert_eq!(c.wanted, None);
        assert_eq!(c.landed(&out, shot(3), T0 + 6, PACE), Ok(Verdict::Stale), "the shot of C, out meanwhile, lands stale");
        assert_eq!(c.card.as_ref().map(|k| k.live.as_str()), Some(B));
    }

    /// Goal: a failure is tried again with a doubling wait, and after the
    /// last try the live is let go quietly, the card before kept. Method:
    /// fail every try; then a failure that says not to retry.
    #[test]
    fn failures_retry_with_backoff_then_give_up() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let t = shoot(&mut c, T0);
        c.landed(&t, shot(1), T0, PACE).unwrap();
        c.want(B, T0 + 1).unwrap();
        let mut now = T0 + 1;
        let mut waits = vec![];
        for attempt in 1..=ATTEMPTS_MAX {
            let t = shoot(&mut c, now);
            assert_eq!(t.attempt, attempt);
            match c.landed(&t, Outcome::Failed { retry: true }, now, PACE).unwrap() {
                Verdict::Retry { at } => {
                    assert_eq!(c.next(now), Next::Wait(at), "a retry waits its turn");
                    waits.push(at - now);
                    now = at;
                }
                Verdict::GaveUp { failures } => assert_eq!((failures, attempt), (ATTEMPTS_MAX, ATTEMPTS_MAX)),
                v => panic!("{v:?}"),
            }
        }
        assert_eq!(waits, [10_000, 20_000, 40_000, 80_000]);
        assert_eq!((c.wanted.clone(), c.flight.clone(), c.next(now)), (None, None, Next::Idle), "given up, nothing more is tried");
        assert_eq!(c.card.as_ref().map(|k| k.live.as_str()), Some(A), "the card before stays");
        // a failure that no retry can mend gives up at once
        c.want(C, now).unwrap();
        let t = shoot(&mut c, now);
        assert_eq!(c.landed(&t, Outcome::Failed { retry: false }, now, PACE), Ok(Verdict::GaveUp { failures: 1 }));
        // the waits double to the longest, and stop there
        let pace = Pace { first_ms: 1_000, max_ms: 5_000 };
        assert_eq!((1..=6).map(|f| pace.wait_ms(f)).collect::<Vec<_>>(), [1_000, 2_000, 4_000, 5_000, 5_000, 5_000]);
        assert_eq!(Pace { first_ms: 1_000, max_ms: 1_000 }.wait_ms(u32::MAX), 1_000, "a huge count stays in bounds");
    }

    /// Goal: replays change nothing: the same live again, the same report
    /// twice, a report after its lease. Method: each against a copy.
    #[test]
    fn replays_change_nothing() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let before = c.clone();
        assert_eq!(c.want(A, T0 + 5), Ok(false), "the same live wanted again");
        assert_eq!(c, before);
        let t = shoot(&mut c, T0);
        c.landed(&t, shot(1), T0 + 1, PACE).unwrap();
        let kept = c.clone();
        assert_eq!(c.landed(&t, shot(2), T0 + 2, PACE), Ok(Verdict::Late), "a report sent twice");
        assert_eq!(c.want(A, T0 + 3), Ok(false), "the live the card shows");
        assert_eq!(c, kept);
        // a report after its lease ended: the lost shot counted already
        c.want(B, T0 + 4).unwrap();
        let t = shoot(&mut c, T0 + 4);
        assert_eq!(c.expire(T0 + 4 + LEASE_MS - 1, PACE), None, "a lease not yet ended");
        assert!(matches!(c.expire(T0 + 4 + LEASE_MS, PACE), Some(Verdict::Retry { .. })));
        assert_eq!(c.landed(&t, shot(3), T0 + 5 + LEASE_MS, PACE), Ok(Verdict::Late));
        assert_eq!(c.card.as_ref().map(|k| k.live.as_str()), Some(A));
        // a ticket for another wanting of the same live is not this one's
        let other = Ticket { since: t.since + 1, ..t.clone() };
        let t2 = shoot(&mut c, T0 + 4 + LEASE_MS + PACE.first_ms);
        assert_eq!(c.landed(&other, shot(4), T0 + 6 + LEASE_MS, PACE), Ok(Verdict::Late));
        assert_eq!(t2.attempt, 2, "the lost shot cost a try");
    }

    /// Goal: what the schedule refuses, it refuses typed and unchanged.
    /// Method: lives and blobs that are none.
    #[test]
    fn invalid_input_is_refused() {
        let mut c = Cards::default();
        for bad in ["", "abc", "ABCDEFABCDEFABCDEF", "g".repeat(40).as_str(), "a".repeat(65).as_str()] {
            assert_eq!(c.want(bad, T0), Err(Fault::Live), "{bad:?}");
        }
        assert_eq!(c, Cards::default());
        c.want(A, T0).unwrap();
        let t = shoot(&mut c, T0);
        let before = c.clone();
        for bad in [Outcome::Shot { blob: "x".into(), size: 1 }, Outcome::Shot { blob: blob(1), size: 0 }, Outcome::Shot { blob: blob(1), size: BYTES_MAX as u64 + 1 }] {
            assert_eq!(c.landed(&t, bad, T0, PACE), Err(Fault::Blob));
        }
        assert_eq!(c, before, "a refused report changes nothing");
        let decoded: Result<Cards, _> = serde_json::from_value(serde_json::json!({ "card": null, "wanted": null, "flight": null, "extra": 1 }));
        assert!(decoded.is_err(), "the kept state refuses fields it does not know");
    }

    /// Goal: the schedule survives a restart at any point: it is kept as
    /// JSON and read back the same, and a shot lost in the restart counts
    /// as a failed try once its lease ends. Method: round-trip mid-flight,
    /// then expire the lease.
    #[test]
    fn a_restart_keeps_the_schedule_and_a_lost_shot_is_tried_again() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let t = shoot(&mut c, T0);
        let kept = serde_json::to_string(&c).unwrap();
        let mut back: Cards = serde_json::from_str(&kept).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.next(T0 + 1), Next::Wait(T0 + LEASE_MS), "the shot is still out after the restart");
        assert_eq!(back.expire(T0 + LEASE_MS, PACE), Some(Verdict::Retry { at: T0 + LEASE_MS + PACE.first_ms }));
        let again = shoot(&mut back, T0 + LEASE_MS + PACE.first_ms);
        assert_eq!((again.live.as_str(), again.since, again.attempt), (A, t.since, 2));
        assert_eq!(back.landed(&again, shot(1), T0 + LEASE_MS + PACE.first_ms + 1, PACE), Ok(Verdict::Kept));
        // a shot the queue never took is not out, and costs no try
        back.want(B, T0 + 2 * LEASE_MS).unwrap();
        let t = shoot(&mut back, T0 + 2 * LEASE_MS);
        back.unsend(&t);
        assert_eq!(shoot(&mut back, T0 + 2 * LEASE_MS + 1).attempt, 1);
    }

    /// Goal: a live let go is let go: skipped, it is never shot, and the
    /// card before stays.
    #[test]
    fn a_skipped_live_is_never_shot() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        assert_eq!(c.due(T0).map(|w| w.live.as_str()), Some(A));
        assert_eq!(c.skip().map(|w| w.live), Some(A.to_string()));
        assert_eq!((c.next(T0), c.due(T0)), (Next::Idle, None));
    }

    /// Goal: chats and agents are not shot, members-only fragments are not
    /// shot, and the others are shot as their visitors see them.
    #[test]
    fn who_is_shot_and_as_whom() {
        use FragmentKind::*;
        use Visibility::*;
        for kind in [Chat, Agent] {
            for v in [Public, Link, Members] {
                assert_eq!(visitor(kind, v), Err(Skip::NotAnApp));
            }
        }
        for kind in [App, Brain] {
            assert_eq!(visitor(kind, Public), Ok(Visitor::Anyone));
            assert_eq!(visitor(kind, Link), Ok(Visitor::LinkHolder));
            assert_eq!(visitor(kind, Members), Err(Skip::MembersOnly));
        }
        assert_eq!(page_url("https://todo--ann.fragment.boats", Visitor::Anyone, "t0k"), "https://todo--ann.fragment.boats/");
        assert_eq!(page_url("http://todo--ann.fragment.localhost:8790", Visitor::LinkHolder, "t0k3n"), "http://todo--ann.fragment.localhost:8790/?view=t0k3n");
    }

    /// A JPEG header of `w` × `h`: SOI, then SOF0.
    fn jpeg(w: u16, h: u16) -> Vec<u8> {
        let mut b = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        b.extend(h.to_be_bytes());
        b.extend(w.to_be_bytes());
        b.extend([0x03, 0x01, 0x22, 0x00, 0x02, 0x11, 0x01, 0x03, 0x11, 0x01, 0xFF, 0xD9]);
        b
    }

    /// Goal: a card is a JPEG of the viewport, within the cap; anything
    /// else is refused, saying what it is.
    #[test]
    fn a_card_is_a_jpeg_of_the_viewport() {
        assert_eq!(check_image(&jpeg(WIDTH, HEIGHT)), Ok(()));
        assert_eq!(check_image(&[]), Err(ImageFault::Empty));
        assert_eq!(check_image(b"\x89PNG\r\n\x1a\n"), Err(ImageFault::NotJpeg));
        assert_eq!(check_image(&jpeg(2560, 1600)), Err(ImageFault::Size { width: 2560, height: 1600 }));
        let mut big = jpeg(WIDTH, HEIGHT);
        big.resize(BYTES_MAX + 1, 0);
        assert_eq!(check_image(&big), Err(ImageFault::TooLarge { size: BYTES_MAX + 1 }));
        big.truncate(BYTES_MAX);
        assert_eq!(check_image(&big), Ok(()), "exactly the cap is a card");
    }

    /// Goal: a shot is metered as Browser Rendering bills it: its session's
    /// time at $0.09 a browser hour (the price book's source), plus the
    /// margin, and a lost close is billed its session's inactivity timeout.
    /// Method: a typical two-second shot, an hour, and one whose close was lost.
    #[test]
    fn a_shot_is_priced_by_the_browser_hour() {
        let book = PriceBook::defaults();
        let price = |ms| book.price(&Usage::Browser { ms }).unwrap();
        assert_eq!(price(billed_ms(2_000, true)), Priced { list: 50, cost: 50, charge: 75 }, "2 s at $0.09 an hour: 50 µ$, 75 with the margin");
        assert_eq!(price(3_600_000), Priced { list: 90_000, cost: 90_000, charge: 135_000 }, "an hour lists at exactly $0.09");
        assert_eq!(billed_ms(2_000, false), 12_000);
        assert_eq!(price(billed_ms(2_000, false)).list, 300, "12 s");
        assert_eq!(billed_ms(u64::MAX, false), u64::MAX, "saturates");
        let t = Ticket { live: A.into(), since: T0, attempt: 2 };
        assert_eq!(meter_ref("todo.ann@1790000000000", &t), format!("card:todo.ann@1790000000000:aaaaaaaaaaaa:{T0}:2"));
    }
}
