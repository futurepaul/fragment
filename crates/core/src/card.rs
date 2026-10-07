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
//! - the schedule (`Cards`): the fragment's alarm takes each try itself,
//!   so one is out at a time (a Durable Object runs one alarm at a time);
//!   the newest live wins (a shot of an older one is dropped as it lands);
//!   a try counts as failed from when it begins until it lands, so one
//!   the object lost mid-shot (a restart) is a failed try; a failure is
//!   tried again after a wait that doubles, at most `ATTEMPTS_MAX` tries
//!   in all, and then given up quietly: the card before stays, and
//!   nothing else changes;
//! - what the page reported as it loaded (`Heard`): its uncaught
//!   exceptions, console errors, failed loads and security refusals, the
//!   first `ERRORS_MAX` kept (the rest counted), each cut to
//!   `ERROR_BYTES_MAX`; the try that ends its live's tries makes it the
//!   page's report (`Cards::report`), which status shows.

use fragment_proto::{FragmentKind, PageError, PageErrorKind, PageReport, Visibility};
use serde::{Deserialize, Serialize};
use serde_json::Value;

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

/// A shot keeps the first this many errors the page reports, and counts
/// the rest.
pub const ERRORS_MAX: usize = 10;
/// An error's text, and its source, are each cut to this many bytes.
pub const ERROR_BYTES_MAX: usize = 1024;

const _: () = assert!(BYTES_MAX.div_ceil(3) * 4 < 1024 * 1024, "a card's base64 fits one 1 MiB WebSocket message");
// `cut` leaves no control character but a newline or a tab, so each byte
// is at most two in JSON; the summary is at most an error's text and 100 bytes
const _: () = assert!(
    (ERRORS_MAX + 1) * (2 * 2 * ERROR_BYTES_MAX + 100) < fragment_proto::limits::RECORD_BODY_MAX_BYTES,
    "a report is one `page.errors` event"
);
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
    /// Its owner pays for nothing now (a guest, or an unclaimed draft's
    /// maker), or is past the overdraft.
    OwnerPays,
}

impl Skip {
    pub fn message(self) -> &'static str {
        match self {
            Skip::NotAnApp => "a chat or an agent is not an app: it has no card",
            Skip::MembersOnly => "a members-only fragment has no card: a visitor without an account sees only its refusal",
            Skip::OwnerPays => "its owner's ledger takes no shot now (a guest, or a draft no one claimed, pays for nothing; past the overdraft, nothing new is made)",
        }
    }
}

/// Whether a fragment of `kind` and `visibility` is shot, and as whom.
pub fn visitor(kind: FragmentKind, visibility: Visibility) -> Result<Visitor, Skip> {
    match (kind, visibility) {
        (FragmentKind::Chat | FragmentKind::Agent | FragmentKind::Skills | FragmentKind::Mind, _) => Err(Skip::NotAnApp),
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
pub fn meter_ref(key: &str, shot: &Shot) -> String {
    assert!(valid_live(&shot.live), "a shot names a commit");
    format!("card:{key}:{}:{}:{}", &shot.live[..12], shot.since, shot.attempt)
}

/// A commit's id, as code.storage names one (a git SHA-1 or SHA-256, hex).
pub fn valid_live(s: &str) -> bool {
    (12..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What one try heard the page report over CDP (`Runtime` and `Log`
/// enabled on its target), bounded as it is heard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heard {
    pub errors: Vec<PageError>,
    pub dropped: u32,
}

impl Heard {
    /// A message from the page's target, kept if it is one of the page's
    /// errors: `Runtime.exceptionThrown`, `Runtime.consoleAPICalled` of an
    /// error or a failed assert, `Log.entryAdded` of an error from the
    /// network or security. Anything else is not, nor a failed
    /// `/favicon.ico`: Chrome asks for it whether or not the page names it.
    pub fn hear(&mut self, msg: &Value) {
        let p = &msg["params"];
        let (kind, text, source) = match msg["method"].as_str() {
            Some("Runtime.exceptionThrown") => {
                let d = &p["exceptionDetails"];
                // the thrown value says it best ("TypeError: …" and its stack)
                let text = if d["exception"].is_object() { remote_text(&d["exception"]) } else { d["text"].as_str().unwrap_or_default().to_string() };
                (PageErrorKind::Exception, text, source_of(d))
            }
            Some("Runtime.consoleAPICalled") if matches!(p["type"].as_str(), Some("error" | "assert")) => {
                let args = p["args"].as_array().map(Vec::as_slice).unwrap_or_default();
                (PageErrorKind::Console, args.iter().map(remote_text).collect::<Vec<_>>().join(" "), source_of(&p["stackTrace"]["callFrames"][0]))
            }
            Some("Log.entryAdded") if p["entry"]["level"] == "error" => {
                let kind = match p["entry"]["source"].as_str() {
                    Some("network") if p["entry"]["url"].as_str().and_then(|u| url::Url::parse(u).ok()).is_some_and(|u| u.path() == "/favicon.ico") => return,
                    Some("network") => PageErrorKind::Network,
                    Some("security") => PageErrorKind::Security,
                    _ => return,
                };
                (kind, p["entry"]["text"].as_str().unwrap_or_default().to_string(), source_of(&p["entry"]))
            }
            _ => return,
        };
        self.keep(kind, &text, source.as_deref());
    }

    /// One error, cut to size; past `ERRORS_MAX`, only counted.
    pub fn keep(&mut self, kind: PageErrorKind, text: &str, source: Option<&str>) {
        if self.errors.len() >= ERRORS_MAX {
            self.dropped = self.dropped.saturating_add(1);
            return;
        }
        self.errors.push(PageError { kind, text: cut(text), source: source.map(cut) });
    }
}

/// A CDP `RemoteObject` as the console prints it, near enough: a string
/// as itself, an object by its description (an `Error`'s is its message
/// and stack), another value as JSON, `undefined` by its type.
fn remote_text(o: &Value) -> String {
    match (&o["value"], o["description"].as_str().or(o["unserializableValue"].as_str())) {
        (Value::String(s), _) => s.clone(),
        (_, Some(d)) => d.to_string(),
        (Value::Null, None) if o["subtype"] != "null" => o["type"].as_str().unwrap_or("undefined").to_string(),
        (v, None) => v.to_string(),
    }
}

/// Where a CDP exception, stack frame, or log entry points: its URL, with
/// its line and column (1-based; CDP's are 0-based) when it has both.
fn source_of(at: &Value) -> Option<String> {
    let url = at["url"].as_str().filter(|u| !u.is_empty())?;
    Some(match (at["lineNumber"].as_u64(), at["columnNumber"].as_u64()) {
        (Some(line), Some(column)) => format!("{url}:{}:{}", line + 1, column + 1),
        _ => url.to_string(),
    })
}

/// `s` with no control character but a newline or a tab (any other is a
/// space), cut on a character to at most `ERROR_BYTES_MAX` bytes, ending
/// `…` when cut.
pub fn cut(s: &str) -> String {
    let clean = |c: char| if c.is_control() && c != '\n' && c != '\t' { ' ' } else { c };
    // a control character is never shorter than a space
    if s.len() <= ERROR_BYTES_MAX {
        return s.chars().map(clean).collect();
    }
    let mut out = String::with_capacity(ERROR_BYTES_MAX);
    for c in s.chars().map(clean) {
        if out.len() + c.len_utf8() > ERROR_BYTES_MAX - '…'.len_utf8() {
            break;
        }
        out.push(c);
    }
    out.push('…');
    out
}

/// A `page.errors` event's summary: how many, of which live, and the
/// first one's first line.
pub fn summary(report: &PageReport) -> String {
    assert!(valid_live(&report.live), "a report names a commit");
    let n = report.errors.len() as u64 + u64::from(report.dropped);
    let first = report.errors.first().and_then(|e| e.text.lines().next()).unwrap_or_default();
    format!("the page reported {n} error{} as it loaded (live {}): {first}", if n == 1 { "" } else { "s" }, &report.live[..12])
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
/// it as one JSON value). Keys it does not know are ignored: the `flight`
/// a cell before the alarm took its own shots kept is one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cards {
    /// The card kept now.
    pub card: Option<Card>,
    /// The newest live with no card yet, and its tries so far.
    pub wanted: Option<Wanted>,
    /// What the page reported to the last try that ended its live's tries
    /// having opened it.
    pub page: Option<PageReport>,
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
    /// Its tries so far, the one out (if any) among them: a try counts as
    /// failed from when it begins until it lands.
    pub failures: u32,
    /// When it is next tried.
    pub next_at: i64,
}

/// One try at a card: what it shows, and which try it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shot {
    pub live: String,
    pub since: i64,
    pub attempt: u32,
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
    /// Nothing to shoot now (`due_at` says when to look again).
    Idle,
    /// Take this shot now: it counts as a failed try until it lands.
    Shoot(Shot),
    /// The live's last try never landed (the object restarted under it):
    /// the live is let go, and the card before stays.
    GaveUp { failures: u32 },
}

/// How a try ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// An image, checked (`check_image`) and stored as this blob.
    Image { blob: String, size: u64 },
    /// No image; `retry` when another try may get one.
    Failed { retry: bool },
}

/// What a try's landing did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The shot is the card now.
    Kept,
    /// The schedule moved on while the shot was out (a newer live, or the
    /// card's own live again): dropped.
    Stale,
    /// The try failed; the next waits until `at`.
    Retry { at: i64 },
    /// The last try failed: the live gets no card, and the one before stays.
    GaveUp { failures: u32 },
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

    /// When the schedule next needs a look: the wanted live's next try.
    pub fn due_at(&self) -> Option<i64> {
        self.wanted.as_ref().map(|w| w.next_at)
    }

    /// The live a shot would be of now, without beginning it: the cell asks
    /// whether it is shot at all (`visitor`, the owner's ledger) first.
    pub fn due(&self, now: i64) -> Option<&Wanted> {
        self.wanted.as_ref().filter(|w| w.next_at <= now)
    }

    /// The wanted live gets no card (`Skip`): it is let go, and the card
    /// before stays.
    pub fn skip(&mut self) -> Option<Wanted> {
        self.wanted.take()
    }

    /// What to do now. A shot it answers is counted as a failed try at
    /// once, and the next waits as after a failure: a try lost with its
    /// object is tried again then, and a live whose last try was lost is
    /// let go.
    pub fn next(&mut self, now: i64, pace: Pace) -> Next {
        let Some(w) = self.wanted.as_mut().filter(|w| w.next_at <= now) else { return Next::Idle };
        if w.failures >= ATTEMPTS_MAX {
            let failures = w.failures;
            self.wanted = None;
            return Next::GaveUp { failures };
        }
        w.failures += 1;
        w.next_at = now + pace.wait_ms(w.failures);
        Next::Shoot(Shot { live: w.live.clone(), since: w.since, attempt: w.failures })
    }

    /// Whether `shot` is the try the schedule waits on: its live is still
    /// the newest, and no later try of it began.
    pub fn current(&self, shot: &Shot) -> bool {
        self.wanted.as_ref().is_some_and(|w| w.live == shot.live && w.since == shot.since && w.failures == shot.attempt)
    }

    /// A try landed. Only the current one counts (newest wins); a failure
    /// waits from now, not from when it began.
    pub fn landed(&mut self, shot: &Shot, outcome: Outcome, now: i64, pace: Pace) -> Result<Verdict, Fault> {
        if let Outcome::Image { blob, size } = &outcome {
            if !crate::blob::valid_sha(blob) || *size == 0 || *size > BYTES_MAX as u64 {
                return Err(Fault::Blob);
            }
        }
        if !self.current(shot) {
            return Ok(Verdict::Stale);
        }
        let w = self.wanted.as_mut().expect("a current shot has its live wanted");
        Ok(match outcome {
            Outcome::Image { blob, size } => {
                self.card = Some(Card { blob, size, live: shot.live.clone(), attempt: shot.attempt, at_ms: now });
                self.wanted = None;
                Verdict::Kept
            }
            Outcome::Failed { retry } if !retry || w.failures >= ATTEMPTS_MAX => {
                let failures = w.failures;
                self.wanted = None;
                Verdict::GaveUp { failures }
            }
            Outcome::Failed { .. } => {
                w.next_at = now + pace.wait_ms(w.failures);
                Verdict::Retry { at: w.next_at }
            }
        })
    }

    /// What a try that opened the page heard, once it has landed: the
    /// page's report when the try ended its live's tries (`Kept`,
    /// `GaveUp`); a retry's and a stale try's are dropped. Answers the
    /// report it made.
    pub fn report(&mut self, verdict: &Verdict, live: &str, heard: Heard, now: i64) -> Option<&PageReport> {
        assert!(valid_live(live), "a shot names a commit");
        if !matches!(verdict, Verdict::Kept | Verdict::GaveUp { .. }) {
            return None;
        }
        self.page = Some(PageReport { live: live.to_string(), at: now, errors: heard.errors, dropped: heard.dropped });
        self.page.as_ref()
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

    fn image(n: u8) -> Outcome {
        Outcome::Image { blob: blob(n), size: 40_000 }
    }

    fn shoot(cards: &mut Cards, now: i64) -> Shot {
        match cards.next(now, PACE) {
            Next::Shoot(s) => s,
            other => panic!("expected a shot, got {other:?}"),
        }
    }

    /// Goal: a deploy is shot once, and its shot is the card. Method: want
    /// a live, begin its shot, land the image.
    #[test]
    fn a_deploy_is_shot_and_becomes_the_card() {
        let mut c = Cards::default();
        assert_eq!(c.next(T0, PACE), Next::Idle, "nothing deployed, nothing shot");
        assert_eq!(c.want(A, T0), Ok(true));
        assert_eq!(c.due_at(), Some(T0));
        let s = shoot(&mut c, T0);
        assert_eq!(s, Shot { live: A.into(), since: T0, attempt: 1 });
        assert_eq!(c.next(T0 + 1, PACE), Next::Idle, "a try out is not begun again before its wait");
        assert_eq!(c.landed(&s, image(1), T0 + 2_000, PACE), Ok(Verdict::Kept));
        assert_eq!(c.card, Some(Card { blob: blob(1), size: 40_000, live: A.into(), attempt: 1, at_ms: T0 + 2_000 }));
        assert_eq!((c.wanted.clone(), c.next(T0 + 3_000, PACE), c.due_at()), (None, Next::Idle, None));
    }

    /// Goal: the newest live wins. Method: two deploys before any shot
    /// (only the second is shot), then a deploy while a shot is out (the
    /// shot out lands stale, and the newest is shot next, from its first
    /// try).
    #[test]
    fn the_newest_live_wins() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        c.want(B, T0 + 1).unwrap();
        let s = shoot(&mut c, T0 + 2);
        assert_eq!(s.live, B, "a live replaced before its shot is never shot");
        c.want(C, T0 + 3).unwrap();
        assert_eq!(c.landed(&s, image(2), T0 + 5, PACE), Ok(Verdict::Stale), "a shot of a live no longer the newest is dropped");
        assert_eq!(c.card, None);
        let s = shoot(&mut c, T0 + 6);
        assert_eq!((s.live.as_str(), s.attempt), (C, 1));
        assert_eq!(c.landed(&s, image(3), T0 + 7, PACE), Ok(Verdict::Kept));
        assert_eq!(c.card.as_ref().map(|k| k.live.as_str()), Some(C));
        // a failure of a stale shot costs the newest no try
        c.want(A, T0 + 8).unwrap();
        let old = shoot(&mut c, T0 + 9);
        c.want(B, T0 + 10).unwrap();
        assert_eq!(c.landed(&old, Outcome::Failed { retry: true }, T0 + 11, PACE), Ok(Verdict::Stale));
        assert_eq!(c.wanted.as_ref().map(|w| (w.live.as_str(), w.failures, w.next_at)), Some((B, 0, T0 + 10)));
    }

    /// Goal: a second deploy replaces the card; the card before stays until
    /// the new one is kept. Method: two lives shot in turn.
    #[test]
    fn a_second_deploy_replaces_the_card() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let s = shoot(&mut c, T0);
        c.landed(&s, image(1), T0 + 1, PACE).unwrap();
        c.want(B, T0 + 2).unwrap();
        let s = shoot(&mut c, T0 + 2);
        assert_eq!(c.card.as_ref().map(|k| k.blob.clone()), Some(blob(1)), "the card before serves while the next is shot");
        c.landed(&s, image(2), T0 + 3, PACE).unwrap();
        assert_eq!(c.card.as_ref().map(|k| (k.blob.clone(), k.live.as_str())), Some((blob(2), B)));
        // back to the live the card shows (a forced push of live): nothing to shoot
        c.want(C, T0 + 4).unwrap();
        let out = shoot(&mut c, T0 + 4);
        assert_eq!(c.want(B, T0 + 5), Ok(false));
        assert_eq!(c.wanted, None);
        assert_eq!(c.landed(&out, image(3), T0 + 6, PACE), Ok(Verdict::Stale), "the shot of C, out meanwhile, lands stale");
        assert_eq!(c.card.as_ref().map(|k| k.live.as_str()), Some(B));
    }

    /// Goal: a failure is tried again with a doubling wait, counted from
    /// when it landed, and after the last try the live is let go quietly,
    /// the card before kept. Method: fail every try, each landing a second
    /// after it began; then a failure that says not to retry.
    #[test]
    fn failures_retry_with_backoff_then_give_up() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let s = shoot(&mut c, T0);
        c.landed(&s, image(1), T0, PACE).unwrap();
        c.want(B, T0 + 1).unwrap();
        let mut now = T0 + 1;
        let mut waits = vec![];
        for attempt in 1..=ATTEMPTS_MAX {
            let s = shoot(&mut c, now);
            assert_eq!(s.attempt, attempt);
            let landed_at = now + 1_000;
            match c.landed(&s, Outcome::Failed { retry: true }, landed_at, PACE).unwrap() {
                Verdict::Retry { at } => {
                    assert_eq!((c.next(at - 1, PACE), c.due_at()), (Next::Idle, Some(at)), "a retry waits its turn");
                    waits.push(at - landed_at);
                    now = at;
                }
                Verdict::GaveUp { failures } => assert_eq!((failures, attempt), (ATTEMPTS_MAX, ATTEMPTS_MAX)),
                v => panic!("{v:?}"),
            }
        }
        assert_eq!(waits, [10_000, 20_000, 40_000, 80_000]);
        assert_eq!((c.wanted.clone(), c.next(now + PACE.max_ms, PACE)), (None, Next::Idle), "given up, nothing more is tried");
        assert_eq!(c.card.as_ref().map(|k| k.live.as_str()), Some(A), "the card before stays");
        // a failure that no retry can mend gives up at once
        c.want(C, now).unwrap();
        let s = shoot(&mut c, now);
        assert_eq!(c.landed(&s, Outcome::Failed { retry: false }, now, PACE), Ok(Verdict::GaveUp { failures: 1 }));
        // the waits double to the longest, and stop there
        let pace = Pace { first_ms: 1_000, max_ms: 5_000 };
        assert_eq!((1..=6).map(|f| pace.wait_ms(f)).collect::<Vec<_>>(), [1_000, 2_000, 4_000, 5_000, 5_000, 5_000]);
        assert_eq!(Pace { first_ms: 1_000, max_ms: 1_000 }.wait_ms(u32::MAX), 1_000, "a huge count stays in bounds");
    }

    /// Goal: replays change nothing: the same live again, the same landing
    /// twice, a landing of a try a later one replaced. Method: each against
    /// a copy.
    #[test]
    fn replays_change_nothing() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let before = c.clone();
        assert_eq!(c.want(A, T0 + 5), Ok(false), "the same live wanted again");
        assert_eq!(c, before);
        let s = shoot(&mut c, T0);
        c.landed(&s, image(1), T0 + 1, PACE).unwrap();
        let kept = c.clone();
        assert_eq!(c.landed(&s, image(2), T0 + 2, PACE), Ok(Verdict::Stale), "a landing twice");
        assert_eq!(c.want(A, T0 + 3), Ok(false), "the live the card shows");
        assert_eq!(c, kept);
        // a try a later one replaced, and another wanting of the same live, are not current
        c.want(B, T0 + 4).unwrap();
        let first = shoot(&mut c, T0 + 4);
        let second = shoot(&mut c, T0 + 4 + PACE.first_ms);
        let other = Shot { since: second.since + 1, ..second.clone() };
        let before = c.clone();
        assert_eq!(c.landed(&first, image(3), T0 + 5 + PACE.first_ms, PACE), Ok(Verdict::Stale));
        assert_eq!(c.landed(&other, image(3), T0 + 5 + PACE.first_ms, PACE), Ok(Verdict::Stale));
        assert_eq!(c, before);
        assert_eq!(c.landed(&second, image(4), T0 + 6 + PACE.first_ms, PACE), Ok(Verdict::Kept));
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
        let s = shoot(&mut c, T0);
        let before = c.clone();
        for bad in [Outcome::Image { blob: "x".into(), size: 1 }, Outcome::Image { blob: blob(1), size: 0 }, Outcome::Image { blob: blob(1), size: BYTES_MAX as u64 + 1 }] {
            assert_eq!(c.landed(&s, bad, T0, PACE), Err(Fault::Blob));
        }
        assert_eq!(c, before, "a refused landing changes nothing");
        let decoded: Result<Cards, _> = serde_json::from_value(serde_json::json!({ "card": null, "wanted": { "live": A, "since": T0, "failures": 0, "next_at": T0, "extra": 1 } }));
        assert!(decoded.is_err(), "the kept state refuses fields it does not know");
    }

    /// Goal: the schedule survives a restart at any point: it is kept as
    /// JSON and read back the same, a try lost in the restart counts as
    /// failed and is tried again after its wait, and a live whose last try
    /// was lost is let go. Method: round-trip mid-shot; then lose every
    /// try. And the state a cell before this one kept (with its `flight`)
    /// reads back as its card and wanted live.
    #[test]
    fn a_restart_keeps_the_schedule_and_a_lost_try_counts() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let s = shoot(&mut c, T0);
        let mut back: Cards = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        assert_eq!(back.next(T0 + PACE.first_ms - 1, PACE), Next::Idle, "the lost try waits as a failure does");
        let again = shoot(&mut back, T0 + PACE.first_ms);
        assert_eq!((again.live.as_str(), again.since, again.attempt), (A, s.since, 2));
        assert_eq!(back.landed(&s, image(1), T0 + PACE.first_ms + 1, PACE), Ok(Verdict::Stale), "the lost try, should it land after all");
        // every try lost: the look after the last lets the live go
        let mut now = T0 + PACE.first_ms;
        for _ in 3..=ATTEMPTS_MAX {
            now = back.due_at().unwrap();
            shoot(&mut back, now);
        }
        assert_eq!(back.wanted.as_ref().map(|w| w.failures), Some(ATTEMPTS_MAX));
        assert_eq!(back.next(back.due_at().unwrap(), PACE), Next::GaveUp { failures: ATTEMPTS_MAX });
        assert_eq!((back.wanted.clone(), back.next(now + PACE.max_ms, PACE)), (None, Next::Idle));
        // what a cell before this one kept
        let old = serde_json::json!({
            "card": { "blob": blob(1), "size": 40_000, "live": A, "attempt": 1, "at_ms": T0 },
            "wanted": { "live": B, "since": T0, "failures": 1, "next_at": T0 },
            "flight": { "live": B, "since": T0, "attempt": 2, "until": T0 + 180_000 },
        });
        let mut old: Cards = serde_json::from_value(old).unwrap();
        assert_eq!(old.card.as_ref().map(|k| k.live.as_str()), Some(A));
        assert_eq!(shoot(&mut old, T0).attempt, 2, "its shot out was lost with the cut: tried again");
    }

    /// Goal: a live let go is let go: skipped, it is never shot, and the
    /// card before stays.
    #[test]
    fn a_skipped_live_is_never_shot() {
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        assert_eq!(c.due(T0).map(|w| w.live.as_str()), Some(A));
        assert_eq!(c.skip().map(|w| w.live), Some(A.to_string()));
        assert_eq!((c.next(T0, PACE), c.due(T0)), (Next::Idle, None));
    }

    /// Goal: chats and agents are not shot, members-only fragments are not
    /// shot, and the others are shot as their visitors see them.
    #[test]
    fn who_is_shot_and_as_whom() {
        use FragmentKind::*;
        use Visibility::*;
        for kind in [Chat, Agent, Skills, Mind] {
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

    fn heard(msgs: &[Value]) -> Heard {
        let mut h = Heard::default();
        msgs.iter().for_each(|m| h.hear(m));
        h
    }

    fn error(kind: PageErrorKind, text: &str, source: Option<&str>) -> PageError {
        PageError { kind, text: text.into(), source: source.map(str::to_string) }
    }

    /// Goal: what the page reports as it loads is heard as Chrome says it,
    /// and nothing else is. Method: each kind as CDP sends it, then the
    /// messages a load also sends that are no error.
    #[test]
    fn the_pages_errors_are_heard() {
        use serde_json::json;
        use PageErrorKind::*;
        let page = "http://a--ann.fragment.localhost:8790/";
        let h = heard(&[
            json!({ "method": "Runtime.exceptionThrown", "sessionId": "s", "params": { "exceptionDetails": {
                "text": "Uncaught", "lineNumber": 2, "columnNumber": 8, "url": page,
                "exception": { "type": "object", "subtype": "error", "className": "Error", "description": "Error: boom\n    at http://a/:3:9" } } } }),
            json!({ "method": "Runtime.exceptionThrown", "params": { "exceptionDetails": { "text": "Uncaught", "lineNumber": 0, "columnNumber": 0, "exception": { "type": "string", "value": "thrown" } } } }),
            json!({ "method": "Runtime.exceptionThrown", "params": { "exceptionDetails": { "text": "Uncaught SyntaxError: Unexpected token '}'", "lineNumber": 4, "columnNumber": 1, "url": "http://a/app.js" } } }),
            json!({ "method": "Runtime.consoleAPICalled", "params": { "type": "error", "args": [
                { "type": "string", "value": "failed:" }, { "type": "number", "value": 42, "description": "42" }, { "type": "boolean", "value": true },
                { "type": "object", "className": "Object", "description": "Object", "objectId": "1" }, { "type": "undefined" }, { "type": "object", "subtype": "null", "value": null },
                { "type": "number", "unserializableValue": "NaN" } ],
                "stackTrace": { "callFrames": [{ "functionName": "", "url": "http://a/app.js", "lineNumber": 9, "columnNumber": 4 }] } } }),
            json!({ "method": "Runtime.consoleAPICalled", "params": { "type": "assert", "args": [{ "type": "string", "value": "console.assert" }] } }),
            json!({ "method": "Log.entryAdded", "params": { "entry": { "source": "network", "level": "error", "text": "Failed to load resource: the server responded with a status of 404 (Not Found)", "url": "http://a/missing.js" } } }),
            json!({ "method": "Log.entryAdded", "params": { "entry": { "source": "security", "level": "error", "text": "Refused to load the image 'http://a/x.png' because it violates the following Content Security Policy directive: \"img-src 'none'\".", "url": page, "lineNumber": 3 } } }),
        ]);
        assert_eq!(
            h.errors,
            [
                error(Exception, "Error: boom\n    at http://a/:3:9", Some(&format!("{page}:3:9"))),
                error(Exception, "thrown", None),
                error(Exception, "Uncaught SyntaxError: Unexpected token '}'", Some("http://a/app.js:5:2")),
                error(Console, "failed: 42 true Object undefined null NaN", Some("http://a/app.js:10:5")),
                error(Console, "console.assert", None),
                error(Network, "Failed to load resource: the server responded with a status of 404 (Not Found)", Some("http://a/missing.js")),
                error(Security, "Refused to load the image 'http://a/x.png' because it violates the following Content Security Policy directive: \"img-src 'none'\".", Some(page)),
            ]
        );
        assert_eq!(h.dropped, 0);
        let quiet = heard(&[
            json!({ "method": "Runtime.consoleAPICalled", "params": { "type": "log", "args": [{ "type": "string", "value": "hi" }] } }),
            json!({ "method": "Runtime.consoleAPICalled", "params": { "type": "warning", "args": [] } }),
            json!({ "method": "Log.entryAdded", "params": { "entry": { "source": "network", "level": "warning", "text": "slow" } } }),
            json!({ "method": "Log.entryAdded", "params": { "entry": { "source": "deprecation", "level": "error", "text": "old" } } }),
            json!({ "method": "Log.entryAdded", "params": { "entry": { "source": "network", "level": "error", "text": "Failed to load resource: the server responded with a status of 404 (Not Found)", "url": "http://a--ann.fragment.localhost:8790/favicon.ico" } } }),
            json!({ "method": "Log.entryAdded", "params": { "entry": { "source": "security" } } }),
            json!({ "method": "Runtime.executionContextCreated", "params": { "context": {} } }),
            json!({ "method": "Page.loadEventFired", "params": { "timestamp": 1.0 } }),
            json!({ "id": 7, "result": { "exceptionDetails": { "text": "an answer, not an event" } } }),
            json!({ "method": 7 }),
            json!(null),
            json!("Runtime.exceptionThrown"),
        ]);
        assert_eq!(quiet, Heard::default(), "only errors are heard");
    }

    /// Goal: what a shot keeps of the page's errors is bounded: the first
    /// `ERRORS_MAX` (the rest counted), each text and source cut to
    /// `ERROR_BYTES_MAX` on a character, no control character but a
    /// newline or a tab; so a report at its largest is one event.
    #[test]
    fn what_a_shot_keeps_is_bounded() {
        let mut h = Heard::default();
        for i in 0..ERRORS_MAX + 3 {
            h.keep(PageErrorKind::Console, &format!("e{i}"), None);
        }
        assert_eq!((h.errors.len(), h.dropped, h.errors[ERRORS_MAX - 1].text.as_str()), (ERRORS_MAX, 3, "e9"), "the first are kept, the rest counted");
        let whole = "x".repeat(ERROR_BYTES_MAX);
        assert_eq!(cut(&whole), whole, "exactly the cap is kept whole");
        let over = cut(&format!("{whole}y"));
        assert!(over.len() <= ERROR_BYTES_MAX && over.ends_with('…') && over.starts_with("xxx"), "{}", over.len());
        let wide = cut(&"é".repeat(ERROR_BYTES_MAX));
        assert!(wide.len() <= ERROR_BYTES_MAX && wide.ends_with("é…"), "cut on a character: {}", wide.len());
        assert_eq!(cut("a\u{0}b\rc\u{1b}[31md\u{85}e\nf\tg"), "a b c [31md e\nf\tg");
        let mut h = Heard::default();
        h.keep(PageErrorKind::Network, "x", Some(&format!("http://a/{}", "p".repeat(2 * ERROR_BYTES_MAX))));
        assert!(h.errors[0].source.as_ref().is_some_and(|s| s.len() <= ERROR_BYTES_MAX && s.ends_with('…')), "a source is cut too");
        // the largest report: every error at its longest, every byte escaped
        let mut h = Heard { dropped: u32::MAX - 1, ..Heard::default() };
        for _ in 0..ERRORS_MAX + 1 {
            h.keep(PageErrorKind::Exception, &"\"".repeat(2 * ERROR_BYTES_MAX), Some(&"\\".repeat(2 * ERROR_BYTES_MAX)));
        }
        assert_eq!(h.dropped, u32::MAX, "the count saturates");
        let report = PageReport { live: "f".repeat(64), at: T0, errors: h.errors, dropped: h.dropped };
        let event = serde_json::json!({ "summary": summary(&report), "data": report });
        let size = serde_json::to_string(&event).unwrap().len();
        assert!(size <= fragment_proto::limits::RECORD_BODY_MAX_BYTES, "{size} bytes");
        let one = PageReport { live: A.into(), at: T0, errors: vec![error(PageErrorKind::Exception, "Error: boom\n    at x", None)], dropped: 0 };
        assert_eq!(summary(&one), "the page reported 1 error as it loaded (live aaaaaaaaaaaa): Error: boom");
    }

    /// Goal: the page's report is the one the try that ended its live's
    /// tries heard: kept or given up, not a retry's or a stale try's, and
    /// a replay changes nothing; it survives a restart, and a state kept
    /// without one reads back as none. Method: land tries with what they
    /// heard.
    #[test]
    fn the_report_is_the_ending_tries() {
        let boom = || Heard { errors: vec![error(PageErrorKind::Exception, "Error: boom", None)], dropped: 0 };
        let mut c = Cards::default();
        c.want(A, T0).unwrap();
        let s = shoot(&mut c, T0);
        let v = c.landed(&s, Outcome::Failed { retry: true }, T0 + 1, PACE).unwrap();
        assert_eq!(c.report(&v, A, boom(), T0 + 1), None, "a retry's is dropped");
        assert_eq!(c.page, None);
        let s = shoot(&mut c, T0 + 1 + PACE.first_ms);
        let v = c.landed(&s, image(1), T0 + PACE.first_ms + 2, PACE).unwrap();
        let made = c.report(&v, A, boom(), T0 + PACE.first_ms + 2).cloned();
        let want = PageReport { live: A.into(), at: T0 + PACE.first_ms + 2, errors: boom().errors, dropped: 0 };
        assert_eq!((made, c.page.clone()), (Some(want.clone()), Some(want.clone())), "the kept try's is the report");
        // a replayed landing is stale: the report stays
        let v = c.landed(&s, image(1), T0 + PACE.first_ms + 3, PACE).unwrap();
        assert_eq!((v.clone(), c.report(&v, A, Heard::default(), T0 + PACE.first_ms + 3)), (Verdict::Stale, None));
        assert_eq!(c.page, Some(want.clone()));
        // a clean page next replaces it with no errors
        c.want(B, T0 + 100_000).unwrap();
        let s = shoot(&mut c, T0 + 100_000);
        let v = c.landed(&s, image(2), T0 + 100_001, PACE).unwrap();
        c.report(&v, B, Heard::default(), T0 + 100_001);
        assert_eq!(c.page, Some(PageReport { live: B.into(), at: T0 + 100_001, errors: vec![], dropped: 0 }));
        // a try given up reports what it heard
        c.want(C, T0 + 200_000).unwrap();
        let s = shoot(&mut c, T0 + 200_000);
        let v = c.landed(&s, Outcome::Failed { retry: false }, T0 + 200_001, PACE).unwrap();
        assert_eq!(c.report(&v, C, boom(), T0 + 200_001).map(|p| p.live.as_str()), Some(C));
        let back: Cards = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c, "the report survives a restart");
        let old: Cards = serde_json::from_value(serde_json::json!({ "card": null, "wanted": null })).unwrap();
        assert_eq!(old.page, None);
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
        let s = Shot { live: A.into(), since: T0, attempt: 2 };
        assert_eq!(meter_ref("todo.ann@1790000000000", &s), format!("card:todo.ann@1790000000000:aaaaaaaaaaaa:{T0}:2"));
    }
}
