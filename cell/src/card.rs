//! A fragment app's preview card (docs/cloudflare-v1.md, decision 31;
//! docs/api.md, Cards). The schedule and every rule are pure, in
//! `fragment_core::card`; this is the plumbing around them.
//!
//! - **After live moves** (plane.rs `interpret`) the fragment wants a card
//!   of the new live, in the same turn, and nothing more: the deploy's
//!   request shoots nothing.
//! - **Its alarm** takes the shot, after the rest of its work: it asks
//!   whether the live is shot at all (an app or a brain, not members-only,
//!   an owner who pays), then takes it with the `BROWSER` binding: a
//!   Browser Rendering session driven over CDP, the page opened as a
//!   visitor without an account sees it (a link fragment's with its share
//!   link), at 1280×800, as a JPEG; the session closed whatever happens.
//!   One shot is out per fragment at a time (a Durable Object runs one
//!   alarm at a time); a deploy meanwhile is shot next, and the shot out
//!   lands stale (newest wins). A try counts as failed from when it
//!   begins, so one lost with the object (a restart) is tried again.
//! - **As it lands** the browser time is metered to the fragment's owner
//!   (a `browser` row in the fragment's meter outbox: meter.rs), and the
//!   image is checked, stored as one of the fragment's blobs, and made the
//!   card; a failure is tried again from the alarm with a doubling wait,
//!   then given up with one `card.failed` event, the card before kept.
//!
//! Each read-modify-write of the schedule is one synchronous stretch: a
//! deploy and the alarm interleave at their awaits, never inside one.

use std::time::Duration;

use fragment_core::card::{self, Cards, Next, Outcome, Pace, Skip, Verdict};
use fragment_core::ledger::{Refused, Spend};
use fragment_core::price::Usage;
use fragment_proto::{ErrorCode, FragmentKind, Role};
use futures_util::future::{select, Either};
use futures_util::StreamExt;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{Caller, FragmentCell, MetaKey};
use crate::js;
use crate::ledger::MaySpend;

/// What the binding's routes are addressed under (any host: the binding
/// answers them all).
const BROWSER_HOST: &str = "https://browser.binding";
/// A card is the same bytes for its blob forever, but which blob is the
/// card changes with each deploy: revalidated every time, by its tag.
const CARD_CACHE: &str = "private, no-cache";
/// A page nothing serves (Chrome refuses the port), for the `fail-cards`
/// lever: a shot of it fails as an unreachable page does.
const TEST_UNREACHABLE: &str = "http://127.0.0.1:9/";
/// A session id from the binding: what goes into its routes' paths.
const SESSION_ID_MAX: usize = 128;

impl FragmentCell {
    /// The card and its schedule, as kept (none yet: the default).
    pub(crate) fn cards(&self) -> CellResult<Cards> {
        match self.meta(MetaKey::Cards)? {
            None => Ok(Cards::default()),
            Some(text) => serde_json::from_str(&text).map_err(|e| CellError::host(format!("the kept cards do not decode: {e}"))),
        }
    }

    fn keep_cards(&self, cards: &Cards) -> CellResult<()> {
        self.set_meta(MetaKey::Cards, &serde_json::to_string(cards).expect("the cards serialize"))
    }

    /// Failed tries wait as deliveries do (`delivery_retry_s`, doubling
    /// to `delivery_retry_max_s`: config.rs).
    fn card_pace(&self) -> Pace {
        Pace { first_ms: i64::from(self.cfg.delivery_retry_s) * 1000, max_ms: i64::from(self.cfg.delivery_retry_max_s) * 1000 }
    }

    /// `live` moved to `live` (plane.rs `interpret`): its card is wanted,
    /// the newest winning. The alarm takes the shot; nothing waits here.
    pub(crate) fn card_wanted(&self, live: &str) -> CellResult<()> {
        let mut cards = self.cards()?;
        let before = cards.clone();
        cards.want(live, js::now_ms()).map_err(|_| CellError::host(format!("live moved to {live}, which is no commit id")))?;
        if cards != before {
            self.keep_cards(&cards)?;
        }
        Ok(())
    }

    /// When the alarm next has card work (fragment.rs `arm`).
    pub(crate) fn card_due_at(&self) -> CellResult<Option<i64>> {
        Ok(self.cards()?.due_at())
    }

    /// From the alarm, after the rest of its work: a live due is let go
    /// (`Skip`) or shot, here, and the shot lands.
    pub(crate) async fn drain_card(&self) -> CellResult<()> {
        let now = js::now_ms();
        let Some(due) = self.cards()?.due(now).map(|w| (w.live.clone(), w.since)) else { return Ok(()) };
        // asked between the reads: what it learns is applied to the schedule as it is then
        let page = self.card_page().await?;
        let mut cards = self.cards()?;
        if cards.due(now).map(|w| (w.live.clone(), w.since)) != Some(due.clone()) {
            // a deploy came meanwhile: the next look decides
            return Ok(());
        }
        let page = match page {
            Ok(_) if self.test_countdown(MetaKey::TestFailCards, "a card's shot").is_err() => TEST_UNREACHABLE.to_string(),
            Ok(page) => page,
            Err(skip) => {
                cards.skip();
                self.keep_cards(&cards)?;
                self.event("card.skipped", skip.message(), json!({ "live": due.0, "why": skip }));
                return Ok(());
            }
        };
        let shot = match cards.next(now, self.card_pace()) {
            Next::Shoot(shot) => shot,
            Next::GaveUp { failures } => {
                self.keep_cards(&cards)?;
                self.card_failed(failures, "its last try was lost with the fragment's object");
                return Ok(());
            }
            Next::Idle => unreachable!("a live due is shot or let go"),
        };
        self.keep_cards(&cards)?;
        // the life it is billed to and kept in: a delete meanwhile ends it
        let (life, meter) = (self.must(MetaKey::CreatedAt)?, card::meter_ref(&self.meter_key()?, &shot));
        let t0 = js::now_ms();
        let (closed, image) = take(&self.env, &page).await;
        // no session acquired: nothing to bill
        let ms = closed.map_or(0, |closed| card::billed_ms(u64::try_from(js::now_ms() - t0).unwrap_or(0), closed));
        if self.meta(MetaKey::CreatedAt)?.as_deref() != Some(life.as_str()) {
            return Ok(());
        }
        if ms > 0 {
            self.outbox(&meter, Usage::Browser { ms }, js::now_ms())?;
        }
        let image = image.and_then(|bytes| card::check_image(&bytes).map(|()| bytes).map_err(|f| Failure { why: f.message(), retry: false }));
        let (outcome, why) = match image {
            Ok(bytes) => {
                let (blob, size) = (hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes)), bytes.len() as u64);
                // stored before the schedule says it is the card: a shot
                // that then lands stale leaves a blob the collection takes
                if self.cards()?.current(&shot) {
                    self.put_blob_typed(&blob, bytes, Some(card::MEDIA_TYPE)).await?;
                }
                (Outcome::Image { blob, size }, String::new())
            }
            Err(f) => (Outcome::Failed { retry: f.retry }, f.why),
        };
        let mut cards = self.cards()?;
        let verdict = cards.landed(&shot, outcome, js::now_ms(), self.card_pace()).map_err(|_| CellError::host("a card's blob is its SHA-256"))?;
        self.keep_cards(&cards)?;
        console_log!("{}", json!({ "card": self.name()?, "attempt": shot.attempt, "ms": ms, "verdict": format!("{verdict:?}") }));
        match verdict {
            Verdict::Kept => {
                let card = cards.card.as_ref().expect("a kept shot is the card");
                self.event("card.made", &format!("a card of live {} ({} bytes)", &card.live[..12], card.size), json!({ "live": card.live, "blob": card.blob, "attempt": card.attempt }));
            }
            Verdict::GaveUp { failures } => self.card_failed(failures, &why),
            // a retry and a stale shot are quiet
            Verdict::Retry { .. } | Verdict::Stale => {}
        }
        Ok(())
    }

    /// The page a shot of this fragment opens now, or why it gets none: its
    /// kind and visibility (`card::visitor`), its own origin, and its
    /// owner's ledger, asked as a create is (a guest makes nothing; past the
    /// overdraft, nothing new is made). A ledger that does not answer
    /// refuses none, as for writes.
    async fn card_page(&self) -> CellResult<Result<String, Skip>> {
        let kind = self
            .meta(MetaKey::Face)?
            .and_then(|f| serde_json::from_str::<Value>(&f).ok())
            .and_then(|f| f["kind"].as_str().and_then(FragmentKind::parse))
            .unwrap_or_default();
        let visitor = match card::visitor(kind, self.visibility()?) {
            Ok(v) => v,
            Err(skip) => return Ok(Err(skip)),
        };
        let origin = self.cfg.outside_origin(&self.name()?);
        let owner = self.must(MetaKey::Owner)?;
        match crate::ledger::ask(&self.env, &owner, &MaySpend { spend: Spend::Create, fragment: None, by_owner: true }).await {
            Ok(_) => {}
            Err(e) if matches!(e.refused, Some(Refused::GuestCreates | Refused::GuestPayer | Refused::ReadOnly { .. })) => return Ok(Err(Skip::OwnerPays)),
            Err(e) => console_error!("{}", json!({ "event": "card.standing-unknown", "message": e.message })),
        }
        Ok(Ok(card::page_url(&origin, visitor, &self.must(MetaKey::ViewToken)?)))
    }

    /// A live given up, said once in the fragment's event log.
    fn card_failed(&self, failures: u32, why: &str) {
        self.event("card.failed", &format!("no card for this deploy after {failures} tries: {why}"), json!({ "failures": failures }));
    }

    /// `GET /api/f/{name}/card` (viewers and up): the card's JPEG, tagged by
    /// its blob, or 404 before the first is made.
    pub(crate) async fn card_api(&self, caller: &Caller, req: &Request) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let Some(card) = self.cards()?.card else {
            return Err(CellError::new(ErrorCode::NotFound, "no card yet: one is taken after each deploy"));
        };
        let etag = format!("\"{}\"", card.blob);
        if let Some(mut resp) = crate::serve::not_modified(req, &etag, CARD_CACHE)? {
            resp.headers_mut().set("x-fragment-ref", &card.live)?;
            return Ok(resp);
        }
        let mut resp = self.stream_blob(&card.blob, card::MEDIA_TYPE, None).await?;
        let h = resp.headers_mut();
        h.set("cache-control", CARD_CACHE)?;
        h.set("x-content-type-options", "nosniff")?;
        h.set("x-fragment-ref", &card.live)?;
        Ok(resp)
    }
}

/// Why a try got no image, and whether another may.
struct Failure {
    why: String,
    retry: bool,
}

impl Failure {
    fn retry(why: impl Into<String>) -> Failure {
        Failure { why: why.into(), retry: true }
    }
}

/// One shot: a Browser Rendering session (`POST /v1/devtools/browser`),
/// CDP over its WebSocket (`GET /v1/devtools/browser/{id}`), then the
/// session closed (`DELETE`), as `@cloudflare/puppeteer` speaks to the
/// binding. Answers whether a session was acquired and then closed
/// (`None`: none was), and the image or why there is none.
async fn take(env: &Env, page: &str) -> (Option<bool>, Result<Vec<u8>, Failure>) {
    let deadline = js::now_ms() + card::SHOT_TIMEOUT_MS as i64;
    let session = match within(deadline, acquire(env)).await {
        Some(Ok(s)) => s,
        Some(Err(f)) => return (None, Err(f)),
        // a session may have been made that no one will close: billed as one
        None => return (Some(false), Err(Failure::retry("Browser Rendering gave no session in time"))),
    };
    let image = within(deadline, drive(env, &session, page, deadline)).await.unwrap_or_else(|| Err(Failure::retry("the shot did not finish in time")));
    let closed = within(js::now_ms() + RELEASE_TIMEOUT_MS, release(env, &session)).await.unwrap_or(false);
    (Some(closed), image)
}

/// How long the session's close is waited for.
const RELEASE_TIMEOUT_MS: i64 = 10_000;

/// `f`, unless `until` passes first (then it is dropped).
async fn within<T>(until: i64, f: impl std::future::Future<Output = T>) -> Option<T> {
    let left = u64::try_from(until - js::now_ms()).unwrap_or(0);
    let timer = Delay::from(Duration::from_millis(left));
    futures_util::pin_mut!(f, timer);
    match select(f, timer).await {
        Either::Left((v, _)) => Some(v),
        Either::Right(_) => None,
    }
}

async fn acquire(env: &Env) -> Result<String, Failure> {
    let mut init = RequestInit::new();
    init.with_method(Method::Post);
    let url = format!("{BROWSER_HOST}/v1/devtools/browser?keep_alive={}", card::KEEP_ALIVE_MS);
    let req = Request::new_with_init(&url, &init).map_err(|e| Failure::retry(e.to_string()))?;
    let mut resp = js::browser_fetch(env.as_ref(), req).await.map_err(|e| Failure::retry(format!("no browser: {}", e.message)))?;
    let status = resp.status_code();
    let text = resp.text().await.unwrap_or_default();
    if status != 200 {
        // a 429 or 5xx passes; anything else is the binding's say, tried again all the same
        return Err(Failure::retry(format!("Browser Rendering answered {status} to a new session")));
    }
    let id = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["sessionId"].as_str().map(str::to_string)).unwrap_or_default();
    if id.is_empty() || id.len() > SESSION_ID_MAX || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(Failure::retry("Browser Rendering named no session"));
    }
    Ok(id)
}

/// Closes the session: whether it is known closed (else it is billed to
/// its inactivity timeout).
async fn release(env: &Env, session: &str) -> bool {
    let mut init = RequestInit::new();
    init.with_method(Method::Delete);
    let Ok(req) = Request::new_with_init(&format!("{BROWSER_HOST}/v1/devtools/browser/{session}"), &init) else { return false };
    match js::browser_fetch(env.as_ref(), req).await {
        Ok(resp) => (200..300).contains(&resp.status_code()),
        Err(_) => false,
    }
}

/// The CDP half: a page target at the viewport, the page opened, its load
/// waited for (at most `LOAD_TIMEOUT_MS`), `SETTLE_MS` more, then a JPEG
/// at each of `QUALITIES` until one fits.
async fn drive(env: &Env, session: &str, page: &str, deadline: i64) -> Result<Vec<u8>, Failure> {
    let headers = Headers::new();
    headers.set("upgrade", "websocket").map_err(|e| Failure::retry(e.to_string()))?;
    let mut init = RequestInit::new();
    init.with_headers(headers);
    let req = Request::new_with_init(&format!("{BROWSER_HOST}/v1/devtools/browser/{session}"), &init).map_err(|e| Failure::retry(e.to_string()))?;
    let resp = js::browser_fetch(env.as_ref(), req).await.map_err(|e| Failure::retry(format!("no CDP socket: {}", e.message)))?;
    let status = resp.status_code();
    let ws = resp.websocket().ok_or_else(|| Failure::retry(format!("Browser Rendering answered {status}, not a CDP socket")))?;
    ws.accept().map_err(|e| Failure::retry(e.to_string()))?;
    let events = ws.events().map_err(|e| Failure::retry(e.to_string()))?;
    let mut cdp = Cdp { ws: &ws, events, next_id: 0, target_session: None, loaded: false };
    let out = shoot(&mut cdp, page, deadline).await;
    drop(cdp);
    let _ = ws.close(Some(1000), Some("shot"));
    out
}

async fn shoot(cdp: &mut Cdp<'_>, page: &str, deadline: i64) -> Result<Vec<u8>, Failure> {
    let target = cdp.call("Target.createTarget", json!({ "url": "about:blank" }), deadline).await?;
    let target = target["targetId"].as_str().ok_or_else(|| Failure::retry("no page target"))?.to_string();
    let attached = cdp.call("Target.attachToTarget", json!({ "targetId": target, "flatten": true }), deadline).await?;
    cdp.target_session = Some(attached["sessionId"].as_str().ok_or_else(|| Failure::retry("no target session"))?.to_string());
    let viewport = json!({ "width": card::WIDTH, "height": card::HEIGHT, "deviceScaleFactor": card::SCALE, "mobile": false });
    cdp.call("Emulation.setDeviceMetricsOverride", viewport, deadline).await?;
    cdp.call("Page.enable", json!({}), deadline).await?;
    let opened = cdp.call("Page.navigate", json!({ "url": page }), deadline).await?;
    if let Some(e) = opened["errorText"].as_str() {
        // Chrome's net error names no URL (the page's may hold the share link)
        return Err(Failure::retry(format!("the page did not open: {e}")));
    }
    let load_by = deadline.min(js::now_ms() + card::LOAD_TIMEOUT_MS as i64);
    cdp.loaded(load_by).await?;
    Delay::from(Duration::from_millis(card::SETTLE_MS)).await;
    let mut size = 0;
    for quality in card::QUALITIES {
        let shot = cdp.call("Page.captureScreenshot", json!({ "format": "jpeg", "quality": quality, "fromSurface": true }), deadline).await?;
        let data = shot["data"].as_str().ok_or_else(|| Failure::retry("the capture held no image"))?;
        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).map_err(|_| Failure::retry("the capture was not base64"))?;
        if bytes.len() <= card::BYTES_MAX {
            return Ok(bytes);
        }
        size = bytes.len();
    }
    Err(Failure { why: card::ImageFault::TooLarge { size }.message(), retry: false })
}

/// A CDP connection to one browser, flattened: commands to the page target
/// carry its session.
struct Cdp<'ws> {
    ws: &'ws WebSocket,
    events: EventStream<'ws>,
    next_id: u64,
    target_session: Option<String>,
    /// The page target fired its load event.
    loaded: bool,
}

/// A CDP connection says at most this many things between a command and
/// its answer (events of a busy page included): past it, something is off.
const CDP_MESSAGES_MAX: usize = 10_000;

impl Cdp<'_> {
    /// The next message, or `None` once `until` passes.
    async fn recv(&mut self, until: i64) -> Result<Option<Value>, Failure> {
        let left = until - js::now_ms();
        if left <= 0 {
            return Ok(None);
        }
        let timer = Delay::from(Duration::from_millis(left as u64));
        let next = self.events.next();
        futures_util::pin_mut!(timer);
        match select(next, timer).await {
            Either::Left((Some(Ok(WebsocketEvent::Message(m))), _)) => {
                let text = m.text().or_else(|| m.bytes().map(|b| String::from_utf8_lossy(&b).into_owned())).unwrap_or_default();
                let v: Value = serde_json::from_str(&text).map_err(|_| Failure::retry("CDP said something that is not JSON"))?;
                if v["method"] == "Page.loadEventFired" && v["sessionId"].as_str() == self.target_session.as_deref() {
                    self.loaded = true;
                }
                Ok(Some(v))
            }
            Either::Left((Some(Ok(WebsocketEvent::Close(c))), _)) => Err(Failure::retry(format!("the CDP socket closed ({})", c.code()))),
            Either::Left((Some(Err(e)), _)) => Err(Failure::retry(format!("the CDP socket failed: {e}"))),
            Either::Left((None, _)) => Err(Failure::retry("the CDP socket ended")),
            Either::Right(_) => Ok(None),
        }
    }

    /// A command, to the page target once attached; its result.
    async fn call(&mut self, method: &str, params: Value, until: i64) -> Result<Value, Failure> {
        self.next_id += 1;
        let id = self.next_id;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = &self.target_session {
            msg["sessionId"] = json!(s);
        }
        self.ws.send_with_str(msg.to_string()).map_err(|e| Failure::retry(format!("{method}: {e}")))?;
        for _ in 0..CDP_MESSAGES_MAX {
            let Some(v) = self.recv(until).await? else { return Err(Failure::retry(format!("{method} had no answer in time"))) };
            if v["id"].as_u64() == Some(id) {
                if let Some(e) = v.get("error") {
                    return Err(Failure::retry(format!("{method}: {}", e["message"].as_str().unwrap_or("refused"))));
                }
                return Ok(v["result"].clone());
            }
        }
        Err(Failure::retry(format!("{method}: no answer in {CDP_MESSAGES_MAX} messages")))
    }

    /// Waits for the page's load until `until`; a page that never loads is
    /// shot as it is then.
    async fn loaded(&mut self, until: i64) -> Result<(), Failure> {
        for _ in 0..CDP_MESSAGES_MAX {
            if self.loaded || self.recv(until).await?.is_none() {
                return Ok(());
            }
        }
        Ok(())
    }
}
