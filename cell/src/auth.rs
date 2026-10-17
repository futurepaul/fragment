//! Sign-in at the router (phase 4 slice B; the registry's half is
//! `registry/signin.rs`). WorkOS AuthKit authenticates people on the
//! platform origin; the platform holds no key for them, only sessions.
//!
//! Platform origin (`/`, `/settings` and `/welcome` are the shell's page: shell.rs):
//!
//!   GET  /auth/login?return=&login_hint=&invitation_token=   → WorkOS (a state cookie binds the
//!                                 round trip; an invitation's token lets its invitee sign up, since
//!                                 sign-up is off: WorkOS's "User invitation URL" points here)
//!   GET  /auth/link?return=       the same, adding a second sign-in to the signed-in person
//!   GET  /auth/callback           WorkOS → the code exchanged here → a session cookie
//!   GET  /auth/logout             a button; POST ends the session (and its site sessions), and
//!                                 WorkOS's, which it was signed in with
//!   POST /auth/switch?return=     "Use another account": the sign-in again, where WorkOS's
//!                                 answer from the session it already holds in the browser
//!                                 (its own cookie, on its own domain) ends that session
//!                                 instead, so the next sign-in asks who is signing in
//!   GET  /auth/fragment?name=&return=      a single-use redemption for one fragment's origin; for
//!                                 a fragment that is not theirs, nor shared with them, a
//!                                 question first ("Continue to X as you?"), asked once
//!   POST /auth/fragment?name=&return=      the yes (remembered), then the redemption
//!   GET  /auth/frame?name=&return=         the same for a frame of the platform's own page (the
//!                                 shell's tabs), as Fetch Metadata says it is: a frame redemption
//!                                 bound to the platform's origin; a note in the frame, and a word
//!                                 to the page, where a tab must ask first or no one is signed in
//!   POST /cli/begin               a CLI's new key (signing this, NIP-98) asks for a code
//!                                 (`fragment_core::cli_code`): ten minutes good, once
//!   GET  /cli                     the signed-in person types that code; the page reads
//!                                 nothing from its URL, so no link approves a key
//!   POST /cli                     (the code typed) → "A CLI wants to act as you", the
//!                                 key's ending, and a press to add it
//!   POST /cli/approve             (that press) the code spent, the key the person's at
//!                                 once; mailed to them (`mail_key_added`)
//!
//! Every round trip that leaves the platform and comes back (a sign-in, a
//! link, a switch, an own key's sign-in: connections.rs) finishes only in
//! the browser that began it: its state is bound by a cookie
//! (`trip_begun`), and its callback finishes it only through
//! `trip_bound`'s `Bound` (`fragment_core::roundtrip`).
//!
//! The rest of a person's account (their picture, email, fragments,
//! credit) is the API's, which the shell calls with this session
//! (docs/api.md, The shell).
//!
//! On fragment.club the platform is cross-site from every fragment (they
//! are on fragment.boats), so its session cookie reaches a fragment's page
//! only on a top-level visit; a fleet whose platform shares the fragments'
//! domain puts them on one site, where the pages' forms, fetches, and
//! frames carry it. Either way every page here refuses frames (`unframed`;
//! the share sheet and the frame mint's notes alone are framed, by the
//! platform's own page: share.rs, `frame_note`)
//! and severs a window that opened it (`unopened`), and every form is
//! refused from another origin (`same_origin`). Sharing's pages (share.rs) add a form token and armed
//! buttons.
//!
//! Each platform page asks the registry once: a call that needs the
//! signed-in person carries the session's token, and the registry checks
//! it in the same turn (`calls::By::Session`, `Mint`, `ApproveKey`,
//! `Begin`'s `link_to`); a page that shows them gets their email with the
//! session.
//!
//! A fragment's origin: `__signin?token=&return=` redeems the platform's
//! redemption for this fragment only and sets its own session cookie (a
//! frame's partitioned one, in a frame, from a frame redemption:
//! `/auth/frame`'s); `__signin` without a
//! token starts at the platform, from a navigation of a page of its own
//! only; `__signout` (a POST from the fragment's own page) ends its
//! sessions in the registry and drops the cookies. Which cookies count on
//! a request is the router's to say (`crate::Fetched`); each is looked up
//! live on every request whose answer depends on who is asking (the
//! fragment decides which: `routed::Credential`). A refusal a browser
//! navigates to there is a page (`refused`): a top-level visit no session
//! admits goes through `/auth/fragment` and back.

use std::collections::BTreeMap;

use fragment_core::roundtrip::{self, Trip};
use fragment_core::{cli_code, form, npub, site};
use fragment_proto::ErrorCode;
use worker::*;

use crate::ask_registry;
use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls::{self, Consent};
use crate::routed::{Credential, Signed};
use crate::{share, Fetched};

pub const SESSION_COOKIE: &str = "fragment_session";
pub const SITE_COOKIE: &str = "fragment_site";
/// A frame's session cookie: partitioned, for the page that framed it.
pub const FRAME_COOKIE: &str = "fragment_frame";
const LOGIN_HINT_MAX: usize = 320;
const INVITATION_TOKEN_MAX: usize = 256;
/// `/cli`'s forms: a form token and a code, or what a person typed.
const CLI_FORM_MAX_BYTES: usize = 1024;
/// `POST /cli/begin`'s body: the CLI sends `{}`.
const CLI_BEGIN_MAX_BYTES: usize = 1024;
/// The form purposes of `/cli`'s two pages: typing a code, then adding the
/// key it names (bound to that code, so a token for one code adds no other).
const CLI_TYPED: &str = "cli";
fn cli_adding(code: &str) -> String {
    format!("cli-approve:{code}")
}

/// A round trip begun (`fragment_core::roundtrip`): the cookie that binds
/// its state to this browser, for the answer that sends it away.
pub(crate) fn trip_begun(trip: Trip, state: &str, url: &Url) -> String {
    assert!(roundtrip::valid_state(state), "a round trip's state is one made here");
    set_cookie(trip.cookie(), state, "/", roundtrip::TTL_S, secure(url))
}

/// The cookie that ends a round trip's binding, once its callback came.
pub(crate) fn trip_spent(trip: Trip, url: &Url) -> String {
    set_cookie(trip.cookie(), "", "/", 0, secure(url))
}

/// A round trip coming back to the browser that began it: the only thing
/// that finishes one. What finishes a round trip takes it by value
/// (`calls::Exchange::of`, `computer::SignedIn::of`), never a state read
/// from a URL. Its fields are private to this module, it is neither
/// `Clone` nor `Deserialize`, and only `trip_bound` makes one, from the
/// request's own cookie; xtask's `a_round_trip_is_bound_only_by_its_request`
/// holds the rest of the cell to that.
pub(crate) struct Bound {
    trip: Trip,
    state: String,
}

impl Bound {
    pub(crate) fn trip(&self) -> Trip {
        self.trip
    }

    /// The state the server keeps the round trip under.
    pub(crate) fn state(&self) -> &str {
        &self.state
    }
}

/// The round trip a callback's request began, of one of `trips`: the state
/// its URL brings, when the request's own cookie for that kind holds it
/// (`roundtrip::same_state`). The only place a `Bound` is made.
pub(crate) fn trip_bound(req: &Request, url: &Url, trips: &[Trip]) -> CellResult<Option<Bound>> {
    assert!(!trips.is_empty() && trips.len() <= Trip::ALL.len(), "a callback finishes one kind of round trip or a few");
    let brought = query(url, "state").unwrap_or_default();
    // bounded: at most every kind of round trip, one cookie each
    for &trip in trips {
        let Some(held) = cookie_of(req, trip.cookie(), secure(url), "/")? else { continue };
        if roundtrip::same_state(&brought, &held) {
            let bound = Bound { trip, state: brought };
            assert!(roundtrip::valid_state(&bound.state), "a bound state is one made here");
            return Ok(Some(bound));
        }
    }
    Ok(None)
}

pub(crate) fn secure(url: &Url) -> bool {
    url.scheme() == "https"
}

/// A session cookie's name: `__Host-` first wherever it can be (https,
/// `Path=/`). Every fragment's page shares its domain (`fragment.boats`)
/// with the others, and may set a cookie for all of it; a browser takes a
/// `__Host-` cookie only host-only and Secure, so none such can stand in
/// for the platform's session or another fragment's. The session is read
/// under that name only.
fn cookie_name(base: &str, secure: bool, path: &str) -> String {
    if secure && path == "/" {
        format!("__Host-{base}")
    } else {
        base.to_string()
    }
}

pub fn set_cookie(base: &str, value: &str, path: &str, max_age_s: i64, secure: bool) -> String {
    let name = cookie_name(base, secure, path);
    let s = if secure { "; Secure" } else { "" };
    format!("{name}={value}; Path={path}; Max-Age={max_age_s}; HttpOnly; SameSite=Lax{s}")
}

/// A frame's session cookie (`base`: a fragment's frame's): `SameSite=None`,
/// so it is sent in a frame of
/// another site's page, and `Partitioned` (CHIPS), so a browser that
/// blocks third-party cookies keeps it, in that page's partition only.
/// Both need `Secure`, which browsers take from `localhost` over http too.
pub(crate) fn frame_cookie(base: &str, value: &str, path: &str, max_age_s: i64, secure: bool) -> String {
    let name = cookie_name(base, secure, path);
    format!("{name}={value}; Path={path}; Max-Age={max_age_s}; HttpOnly; Secure; SameSite=None; Partitioned")
}

pub(crate) fn cookie_of(req: &Request, base: &str, secure: bool, path: &str) -> CellResult<Option<String>> {
    let cookies = req.headers().get("cookie")?.unwrap_or_default();
    let name = cookie_name(base, secure, path);
    Ok(site::cookie(&cookies, &name).filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_string))
}

pub(crate) fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

pub(crate) fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Where a browser that signed in goes back to: the path it brought, on
/// `base`'s origin and never another (`site::return_url`).
fn back_to(base: &str, raw: Option<&str>) -> CellResult<String> {
    let base = Url::parse(base).map_err(|e| CellError::host(format!("{base}: {e}")))?;
    Ok(site::return_url(&base, raw).into())
}

pub(crate) fn redirect(to: &str, cookies: &[String]) -> CellResult<Response> {
    let mut resp = Response::empty()?.with_status(302);
    resp.headers_mut().set("location", to)?;
    resp.headers_mut().set("cache-control", "no-store")?;
    for c in cookies {
        resp.headers_mut().append("set-cookie", c)?;
    }
    Ok(resp)
}

/// No page may frame one that acts for its person: every one of the
/// platform's. Where a fragment's origin is one site with the platform (a
/// fleet whose platform shares the fragments' domain), a session cookie
/// rides into a frame, and a page there could lay the button under a click
/// of its own. A redirect stays framable: it shows nothing,
/// and a frame's `__signin` refuses what the platform's sign-in mints (a
/// frame signs in only with a frame redemption).
pub(crate) fn unframed(h: &Headers) -> worker::Result<()> {
    h.set("content-security-policy", "frame-ancestors 'none'")?;
    h.set("x-frame-options", "DENY")
}

/// No page that opens one of the platform's in a window of its own (a
/// fragment's page: its author's code, or an agent's) keeps a hold on it:
/// the window is severed from its opener, whose handle reads `closed` and
/// can neither navigate nor message it.
pub(crate) fn unopened(h: &Headers) -> worker::Result<()> {
    h.set("cross-origin-opener-policy", "same-origin")
}

/// The platform's look, shared by its pages (`page`, and share.rs's).
pub(crate) const STYLE: &str = "body{font:16px/1.5 system-ui,sans-serif;max-width:34rem;margin:12vh auto;padding:0 16px;color:#1d2126;background:#f6f7f8}
h1{font-size:1.4rem}code{background:#e8eaed;padding:1px 5px;border-radius:4px}button{font:inherit;padding:.5em 1.1em;border-radius:8px;border:1px solid #1d2126;background:#1d2126;color:#fff;cursor:pointer}
a{color:#2a5bd7}pre code{display:block;padding:.6em .8em;white-space:pre-wrap;word-break:break-all}@media (prefers-color-scheme:dark){body{background:#15181b;color:#e6e8ea}code{background:#262b30}button{background:#e6e8ea;color:#15181b;border-color:#e6e8ea}a{color:#7aa2f7}}";

fn html(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{t}</title>
<style>{STYLE}</style>
<h1>{t}</h1>{body}"#,
        t = esc(title)
    )
}

pub(crate) fn page(status: u16, title: &str, body: &str) -> CellResult<Response> {
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "no-store")?;
    unframed(&h)?;
    unopened(&h)?;
    Ok(Response::ok(html(title, body))?.with_status(status).with_headers(h))
}

pub(crate) fn query(url: &Url, k: &str) -> Option<String> {
    url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned())
}

/// The platform session cookie's token, unresolved.
pub(crate) fn platform_session_token(req: &Request, url: &Url) -> CellResult<Option<String>> {
    cookie_of(req, SESSION_COOKIE, secure(url), "/")
}

/// The signed-in person on the platform origin, with their email, and
/// their session's token, or `None`.
pub(crate) async fn platform_session(req: &Request, env: &Env, url: &Url) -> CellResult<Option<(String, calls::LiveSession)>> {
    let Some(token) = cookie_of(req, SESSION_COOKIE, secure(url), "/")? else { return Ok(None) };
    match ask_registry(env, &calls::Session { token: token.clone(), fragment: None, frame: false }).await {
        Ok(live) => Ok(Some((token, live))),
        Err(e) if e.code == ErrorCode::Unauthenticated => Ok(None),
        Err(e) => Err(e),
    }
}

/// The token of a fragment origin's own session cookie, unresolved (the
/// registry says whom it names, for that fragment only). It lives on the
/// whole host.
pub fn site_token(req: &Request, url: &Url) -> CellResult<Option<String>> {
    cookie_of(req, SITE_COOKIE, secure(url), "/")
}

/// The token of its frame cookie, unresolved.
pub fn frame_token(req: &Request, url: &Url) -> CellResult<Option<String>> {
    cookie_of(req, FRAME_COOKIE, secure(url), "/")
}

/// A POST from a browser comes from the platform's own pages: where a
/// fragment's page is one site with the platform, its form or fetch carries
/// the session cookie, and only the Origin tells them apart. A browser sends
/// Origin with every POST (`null` from a page that hides its referrer), so
/// a POST without one is no browser's.
pub(crate) fn same_origin(req: &Request, platform: &str) -> CellResult<()> {
    match req.headers().get("origin")? {
        Some(o) if o.trim_end_matches('/') != platform => Err(CellError::new(ErrorCode::Forbidden, "this form posts from the platform's own page")),
        _ => Ok(()),
    }
}

pub(crate) fn to_login(platform: &str, back: &str) -> CellResult<Response> {
    redirect(&format!("{platform}/auth/login?return={}", enc(back)), &[])
}

/// Where WorkOS ends its session `sid` (the access token's), then sends
/// the browser to the platform's root: the environment's Sign-out URI.
fn workos_logout(api: &str, sid: &str, platform: &str) -> String {
    format!("{api}/user_management/sessions/logout?session_id={}&return_to={}", enc(sid), enc(&format!("{platform}/")))
}

// Commit a page before leaving: Chromium applies the shell's
// form-action 'self' to a POST's HTTP redirects.
fn leave_form(to: &str, title: &str, cookies: &[String]) -> CellResult<Response> {
    let mut resp = page(200, title, &format!("<p><a href=\"{}\">Continue</a></p>", esc(to)))?;
    resp.headers_mut().set("refresh", &format!("0; url={to}"))?;
    resp.headers_mut().set("referrer-policy", "no-referrer")?;
    for cookie in cookies {
        resp.headers_mut().append("set-cookie", cookie)?;
    }
    Ok(resp)
}

/// A sign-in starts at WorkOS, its state bound to this browser as
/// `trip`'s: a sign-in's (or a link's), or a switch's.
async fn begin(env: &Env, cfg: &Config, url: &Url, link: Option<String>, trip: Trip) -> CellResult<Response> {
    assert!(matches!(trip, Trip::SignIn | Trip::Switch), "a sign-in is begun as one, or as a switch");
    let workos = crate::keys::workos(env, cfg).await?;
    let platform = cfg.platform();
    let return_to = site::return_path(query(url, "return").as_deref());
    let began = ask_registry(env, &calls::Begin { return_to, link_to: link }).await?;
    let state = began.state;
    let mut to = format!(
        "{}/user_management/authorize?client_id={}&redirect_uri={}&response_type=code&provider=authkit&state={state}",
        workos.api,
        enc(&workos.client_id),
        enc(&format!("{platform}/auth/callback"))
    );
    if let Some(hint) = query(url, "login_hint").filter(|h| !h.is_empty() && h.len() <= LOGIN_HINT_MAX) {
        to += &format!("&login_hint={}", enc(&hint));
    }
    let token = query(url, "invitation_token")
        .filter(|t| !t.is_empty() && t.len() <= INVITATION_TOKEN_MAX && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    if let Some(token) = token {
        to += &format!("&invitation_token={token}");
    }
    let cookies = [trip_begun(trip, &state, url)];
    match trip {
        Trip::Switch => leave_form(&to, "Switching accounts", &cookies),
        _ => redirect(&to, &cookies),
    }
}

async fn callback(req: &Request, env: &Env, cfg: &Config, url: &Url) -> CellResult<Response> {
    let workos = crate::keys::workos(env, cfg).await?;
    if let Some(error) = query(url, "error") {
        let why = query(url, "error_description").unwrap_or_default();
        return page(400, "Sign-in did not finish", &format!("<p>WorkOS said <code>{}</code>: {}</p><p><a href=\"/auth/login\">Try again</a></p>", esc(&error), esc(&why)));
    }
    let Some(bound) = trip_bound(req, url, &[Trip::SignIn, Trip::Switch])? else {
        return page(400, "Sign-in did not start here", "<p>This sign-in began in another browser, or too long ago.</p><p><a href=\"/auth/login\">Start again</a></p>");
    };
    let spent = trip_spent(bound.trip(), url);
    let code = query(url, "code").ok_or_else(|| CellError::invalid("the callback carries no code"))?;
    // the registry exchanges the code: WorkOS's API key is the node's (KEYS)
    let issuer = workos.issuer();
    let done = match ask_registry(env, &calls::Exchange::of(bound, code, workos.client_id, issuer)).await {
        Ok(done) => done,
        // a sign-in the registry refuses for who it is (an email WorkOS has
        // not verified, or another person's; a person being wiped): said on
        // a page, for a browser
        Err(e) if matches!(e.code, ErrorCode::Forbidden | ErrorCode::AlreadyExists) => {
            return page(e.code.status(), "Sign-in did not finish", &format!("<p>{}</p><p><a href=\"/auth/login\">Try again</a></p>", esc(&e.message)));
        }
        Err(e) => return Err(e),
    };
    match done {
        calls::Exchanged::SignedIn { token, return_to } => redirect(
            &back_to(&format!("{}/", cfg.platform()), Some(&return_to))?,
            &[set_cookie(SESSION_COOKIE, &token, "/", crate::registry::SESSION_TTL_MS / 1000, secure(url)), spent],
        ),
        // WorkOS signed the browser in again as whoever it last was: that
        // session ends there, and the next sign-in asks who is signing in
        calls::Exchanged::Held { workos_sid } => redirect(&workos_logout(workos.api, &workos_sid, &cfg.platform()), &[spent]),
    }
}

/// Sign-in's routes on the platform origin (the router sends only these).
pub async fn platform(mut req: Request, env: &Env, ctx: &Context, cfg: &Config, url: &Url, segments: &[&str]) -> CellResult<Response> {
    let method = req.method();
    let platform = cfg.platform();
    {
        match (method, segments) {
            (Method::Get, ["auth", "login"]) => begin(env, cfg, url, None, Trip::SignIn).await,
            // the registry checks the session is live as the sign-in begins
            (Method::Get, ["auth", "link"]) => match cookie_of(&req, SESSION_COOKIE, secure(url), "/")? {
                Some(token) => match begin(env, cfg, url, Some(token), Trip::SignIn).await {
                    Err(e) if e.code == ErrorCode::Unauthenticated => to_login(&platform, "/auth/link"),
                    begun => begun,
                },
                None => to_login(&platform, "/auth/link"),
            },
            (Method::Get, ["auth", "callback"]) => callback(&req, env, cfg, url).await,
            (Method::Get, ["auth", "logout"]) => page(200, "Sign out", "<form method=\"post\" action=\"/auth/logout\"><button>Sign out</button></form>"),
            (Method::Post, ["auth", "logout"]) => {
                same_origin(&req, &platform)?;
                let clear = set_cookie(SESSION_COOKIE, "", "/", 0, secure(url));
                let Some(token) = cookie_of(&req, SESSION_COOKIE, secure(url), "/")? else { return redirect("/", &[clear]) };
                let sid = match ask_registry(env, &calls::Logout { token }).await {
                    Ok(out) => out.workos_sid,
                    Err(e) if e.code == ErrorCode::Unauthenticated => None,
                    Err(e) => return Err(e),
                };
                match (sid, cfg.workos()) {
                    (Some(sid), Ok(w)) => leave_form(&workos_logout(&w.api, &sid, &platform), "Signing out", &[clear]),
                    _ => redirect("/", &[clear]),
                }
            }
            // another account: not this browser's session here either (one
            // a tab signed in to since), then the sign-in, as a switch
            (Method::Post, ["auth", "switch"]) => {
                same_origin(&req, &platform)?;
                if let Some(token) = cookie_of(&req, SESSION_COOKIE, secure(url), "/")? {
                    match ask_registry(env, &calls::Logout { token }).await {
                        Err(e) if e.code != ErrorCode::Unauthenticated => return Err(e),
                        _ => {}
                    }
                }
                let mut begun = begin(env, cfg, url, None, Trip::Switch).await?;
                begun.headers_mut().append("set-cookie", &set_cookie(SESSION_COOKIE, "", "/", 0, secure(url)))?;
                Ok(begun)
            }
            (Method::Get, ["auth", "fragment"]) => {
                let (name, back) = fragment_asked(url)?;
                let signed_out = || to_login(&platform, &format!("/auth/fragment?name={name}&return={}", enc(&back)));
                let Some(token) = cookie_of(&req, SESSION_COOKIE, secure(url), "/")? else { return signed_out() };
                match mint_for(env, url, &token, &name, &back, None).await? {
                    Minting::Redeem(redeem) => to_signin(cfg, url, &name, &redeem),
                    Minting::Ask(who, email) => consent_page(&token, &who, email.as_deref(), &name, &back),
                    Minting::SignedOut => signed_out(),
                }
            }
            (Method::Post, ["auth", "fragment"]) => {
                let (name, back) = fragment_asked(url)?;
                let (session, _, _) = match share::poster(&mut req, env, url, &platform, &format!("consent:{name}")).await? {
                    Ok(posted) => posted,
                    Err(page) => return Ok(page),
                };
                let given = calls::Mint { token: session, fragment: name.clone(), return_to: back, consent: Consent::Given, embedder: None };
                let minted = ask_registry(env, &given).await?;
                Ok(to_signin(cfg, url, &name, minted.redeem.as_deref().ok_or_else(|| CellError::host("a given mint answered no redemption"))?)?.with_status(303))
            }
            (Method::Get, ["auth", "frame"]) => frame(&req, env, cfg, url).await,
            // a CLI's new key asks for the code its person types: signed by
            // that key (which no one holds yet, so `signer` would refuse it)
            (Method::Post, ["cli", "begin"]) => {
                let body = crate::read_body(&mut req, CLI_BEGIN_MAX_BYTES).await?;
                let key = crate::authenticate(&req, url, fragment_nip98::Payload::Read(&body))?;
                // what the platform knows of who asked: shown to whoever types it
                let cf = req.cf();
                let agent = req.headers().get("user-agent")?;
                let asked = cli_code::Asked::of(crate::js::now_ms(), cf.and_then(|c| c.city()).as_deref(), cf.and_then(|c| c.country()).as_deref(), agent.as_deref());
                let begun = ask_registry(env, &calls::CliBegin { key, from: client_address(&req)?, asked }).await?;
                crate::fragment::json_response(&fragment_proto::CliCode { code: cli_code::display(&begun.code), url: format!("{platform}/cli"), expires_at: begun.expires_at })
            }
            // the page reads nothing from its URL: no link fills in a code
            (Method::Get, ["cli"]) => match platform_session(&req, env, url).await? {
                Some((token, _)) => cli_page(&token, None),
                None => to_login(&platform, "/cli"),
            },
            (Method::Post, ["cli"]) => {
                let (token, fields) = match cli_posted(&mut req, url, &platform, |_| CLI_TYPED.to_string()).await? {
                    Ok(posted) => posted,
                    Err(page) => return Ok(page),
                };
                // the registry checks the session is live as it reads the code
                match ask_registry(env, &calls::CliCodeOf { token: token.clone(), code: field(&fields, "code").to_string() }).await {
                    Ok(found) => cli_confirm(&token, &found),
                    Err(e) => cli_refused(&platform, &token, e),
                }
            }
            (Method::Post, ["cli", "approve"]) => {
                let (token, fields) = match cli_posted(&mut req, url, &platform, |f| cli_adding(&cli_code::normalize(field(f, "code")).unwrap_or_default())).await? {
                    Ok(posted) => posted,
                    Err(page) => return Ok(page),
                };
                let code = cli_code::normalize(field(&fields, "code")).unwrap_or_default();
                // the registry checks the session is live as it spends the code
                // and adds its key: one call
                let added = match ask_registry(env, &calls::ApproveKey { token: token.clone(), code }).await {
                    Ok(added) => added,
                    Err(e) => return cli_refused(&platform, &token, e),
                };
                mail_key_added(ctx, env, added.email.as_deref(), &added.key, fragment_core::mail::KeyHow::Code(&added.asked));
                page(200, "Key added", "<p>This key is yours now. The <code>fragment login</code> waiting in your terminal finishes on its own.</p>")
            }
            (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
        }
    }
}

fn field<'a>(fields: &'a BTreeMap<String, String>, name: &str) -> &'a str {
    fields.get(name).map_or("", String::as_str)
}

/// `/cli`: where a person types the code `fragment login` printed (docs/api.md,
/// Sign-in). A sharing page's protections (share.rs): unframed, a form
/// token, a button that arms after a moment; and it reads nothing from its
/// URL, so no link anyone sends fills in what a click would approve.
fn cli_page(session: &str, flash: Option<&str>) -> CellResult<Response> {
    let flash = flash.map_or(String::new(), |f| format!("<p class=\"flash error\">{}</p>", esc(f)));
    let body = format!(
        "{flash}<p>Type the code your terminal shows after <code>fragment login</code>.</p>\
         <form method=\"post\" action=\"/cli\" class=\"add\"><input type=\"hidden\" name=\"form\" value=\"{f}\">\
         <input name=\"code\" required autocomplete=\"off\" autocapitalize=\"characters\" spellcheck=\"false\" maxlength=\"{max}\" placeholder=\"XXXX-XXXX\" aria-label=\"The code\" autofocus>\
         <button data-arm disabled>Continue</button></form>\
         <p class=\"hint\"><b>{never}</b> Didn't run <code>fragment login</code>? Close this page.</p>",
        f = esc(&form::issue(session, CLI_TYPED, crate::js::now_ms())),
        max = cli_code::TYPED_MAX_BYTES,
        never = esc(cli_code::NEVER_SENT),
    );
    share::sheet_page(if flash.is_empty() { 200 } else { 400 }, "Add a fragment CLI", &body, true)
}

/// The key a typed code names, to add with a second press: "A fragment CLI
/// wants to act as you", the key's ending to compare with the terminal, and
/// where and when its code was asked for, so a code someone else asked for
/// and sent them says so.
fn cli_confirm(session: &str, found: &calls::CliCoded) -> CellResult<Response> {
    let you = match &found.email {
        Some(e) => esc(e),
        None => format!("<code>{}</code>", esc(&found.identity)),
    };
    let npub = npub::encode(&found.key);
    let tail = &npub[npub.len() - 8..];
    let body = format!(
        "<p>A <code>fragment</code> CLI wants to act as <b>{you}</b>.</p>\
         <p>Its key ends in <code>{tail}</code>: check that your terminal shows the same ending. Its code was {asked}.</p>\
         <form method=\"post\" action=\"/cli/approve\"><input type=\"hidden\" name=\"form\" value=\"{f}\"><input type=\"hidden\" name=\"code\" value=\"{c}\">\
         <button data-arm disabled>Add this key</button></form>\
         <p class=\"hint\"><b>{never}</b> Didn't run <code>fragment login</code>? Close this page.</p>",
        asked = esc(&found.asked.line(crate::js::now_ms())),
        never = esc(cli_code::NEVER_SENT),
        f = esc(&form::issue(session, &cli_adding(&found.code), crate::js::now_ms())),
        c = esc(&found.code),
    );
    share::sheet_page(200, "Add this key to you?", &body, true)
}

/// A `/cli` form's POST, once its Origin, the session cookie and its form
/// token hold (`purpose`: the form's, from its own fields): the session's
/// token, unresolved (the registry checks it in the call that follows), and
/// the fields. Otherwise the page to show.
async fn cli_posted(req: &mut Request, url: &Url, platform: &str, purpose: fn(&BTreeMap<String, String>) -> String) -> CellResult<Result<(String, BTreeMap<String, String>), Response>> {
    same_origin(req, platform)?;
    // read before anyone is known to be signed in: bounded as it arrives
    let bytes = crate::read_body(req, CLI_FORM_MAX_BYTES).await?;
    let fields: BTreeMap<String, String> = url::form_urlencoded::parse(&bytes).map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let Some(token) = cookie_of(req, SESSION_COOKIE, secure(url), "/")? else {
        return Ok(Err(page(401, "Sign in first", "<p>This page's session ended.</p><p><a href=\"/auth/login?return=/cli\">Sign in</a>, then type the code again.</p>")?));
    };
    if let Err(refused) = form::check(&token, &purpose(&fields), field(&fields, "form"), crate::js::now_ms()) {
        return Ok(Err(page(403, "Not sent", &format!("<p>{}</p><p><a href=\"/cli\">Type the code again</a></p>", esc(refused.message())))?));
    }
    Ok(Ok((token, fields)))
}

/// A code the registry would not take, as a page: a wrong one (a miss) asks
/// again; too many misses, a key that cannot be added, or a session ended,
/// say why.
fn cli_refused(platform: &str, session: &str, e: CellError) -> CellResult<Response> {
    match e.code {
        ErrorCode::InvalidRequest => cli_page(session, Some(&e.message)),
        ErrorCode::Unauthenticated => to_login(platform, "/cli"),
        ErrorCode::RateLimited | ErrorCode::AlreadyExists | ErrorCode::Forbidden => page(e.code.status(), "Not added", &format!("<p>{}</p>", esc(&e.message))),
        _ => Err(e),
    }
}

/// The mail to a person whose identity a key just joined, on every key
/// added to them: the key's ending, how it came (`how`), and how to revoke
/// it. Sent after the answer (`wait_until`), never in its way: the key is
/// added whatever becomes of it, and a mail that fails is logged.
pub(crate) fn mail_key_added(ctx: &Context, env: &Env, email: Option<&str>, key: &str, how: fragment_core::mail::KeyHow<'_>) {
    let npub = npub::encode(key);
    let Some(email) = email else {
        console_error!("{}", serde_json::json!({ "key-mail": npub, "failed": "the person has no email" }));
        return;
    };
    let cfg = Config::from_env(env);
    let mail = fragment_core::mail::key_added(email, &npub, &cfg.platform(), how, crate::js::now_ms());
    let env = env.clone();
    ctx.wait_until(async move {
        if let Err(e) = crate::mail::send(&env, cfg, &mail).await {
            console_error!("{}", serde_json::json!({ "key-mail": npub, "failed": e.message }));
        }
    });
}

/// The address a request came from, as Cloudflare names it
/// (`CF-Connecting-IP`, which it sets on every request and a client cannot
/// keep), bounded: what a limit counts by. A request with none (a local
/// fleet's test, which names one to be counted apart) counts as `unnamed`.
fn client_address(req: &Request) -> CellResult<String> {
    let named = req.headers().get("cf-connecting-ip")?.map(|a| a.trim().to_string()).filter(|a| !a.is_empty() && a.len() <= calls::CliBegin::FROM_MAX_BYTES);
    Ok(named.unwrap_or_else(|| "unnamed".to_string()))
}

/// The fragment and the way back `/auth/fragment` names.
fn fragment_asked(url: &Url) -> CellResult<(String, String)> {
    let name = query(url, "name").filter(|n| fragment_proto::valid_fragment_name(n)).ok_or_else(|| CellError::invalid("name a fragment"))?;
    Ok((name, site::return_path(query(url, "return").as_deref())))
}

fn to_signin(cfg: &Config, url: &Url, name: &str, redeem: &str) -> CellResult<Response> {
    redirect(&format!("{}__signin?token={redeem}", cfg.canonical(url, name)), &[])
}

/// What a platform session may sign a browser in to a fragment with.
enum Minting {
    /// A single-use redemption for its `__signin`.
    Redeem(String),
    /// Nothing yet: the fragment is not the person's, nor shared with them,
    /// and they have not said yes to it (whom to ask, and their email).
    Ask(Signed, Option<String>),
    /// The platform session ended, or never began.
    SignedOut,
}

/// A redemption of the platform session `token` for `name` (a frame's,
/// bound to `embedder`, when it names one), when the person may be known
/// there: they said yes to it before, or are in it. The registry checks
/// the session is live as it mints, and mints only for a fragment the
/// person said yes to unless the fragment knows them already.
async fn mint_for(env: &Env, url: &Url, token: &str, name: &str, back: &str, embedder: Option<&str>) -> CellResult<Minting> {
    let mint = |consent| calls::Mint { token: token.to_string(), fragment: name.to_string(), return_to: back.to_string(), consent, embedder: embedder.map(str::to_string) };
    let minted = match ask_registry(env, &mint(Consent::Remembered)).await {
        Err(e) if e.code == ErrorCode::Unauthenticated => return Ok(Minting::SignedOut),
        minted => minted?,
    };
    if let Some(redeem) = minted.redeem {
        return Ok(Minting::Redeem(redeem));
    }
    // silent on their own fragments and those shared with them, which know
    // them already (docs/api.md, Asking first)
    let who = Signed::new(minted.identity, None);
    match share::ask(env, url, name, &who, Method::Get, "/api/status", None).await {
        Ok(_) => {
            let minted = ask_registry(env, &mint(Consent::Member)).await?;
            Ok(Minting::Redeem(minted.redeem.ok_or_else(|| CellError::host("a member's mint answered no redemption"))?))
        }
        Err(e) if matches!(e.code, ErrorCode::Forbidden | ErrorCode::Unauthenticated) => Ok(Minting::Ask(who, minted.email)),
        Err(e) => Err(e),
    }
}

/// `GET /auth/frame?name=&return=`: a frame of the platform's own page (a
/// tab of the shell's) signs in to the fragment `name` (docs/api.md, Frame
/// sessions). Only such a frame mints: its Fetch Metadata, which no page's
/// script sets, must say it is a frame's navigation started by a page on
/// this origin (`fragment_core::frames::platform_frame`), and this origin
/// must be the platform's, so the frame redemption's embedder (the only
/// page the fragment's answers to it may show in) is the platform's origin
/// and nothing a request names. A page on any other origin, a fragment's
/// (its author's code) included, cannot start one.
///
/// The token travels only in the redirect: a page cannot read the URL its
/// cross-origin frame was sent to, nor `fetch` this route (its dest is not
/// a frame's), and the fragment spends it in a frame only. Each frame load
/// mints one: the registry keeps a session's newest
/// `REDEMPTIONS_PER_SESSION_MAX` unspent, and its newest
/// `SITE_SESSIONS_PER_FRAGMENT_MAX` frame sessions on each fragment, apart
/// from its top-level ones, so a shell reloading its tabs ends none of
/// those.
async fn frame(req: &Request, env: &Env, cfg: &Config, url: &Url) -> CellResult<Response> {
    let platform = cfg.platform();
    let header = |k: &str| req.headers().get(k).map(Option::unwrap_or_default);
    let framed = fragment_core::frames::platform_frame(&header("sec-fetch-dest")?, &header("sec-fetch-mode")?, &header("sec-fetch-site")?);
    if !framed || url.origin().ascii_serialization() != platform {
        return page(403, "Not a frame of the platform", "<p>This signs in a frame of the platform's own page, and nothing else. Open the fragment itself instead.</p>");
    }
    let (name, back) = fragment_asked(url)?;
    let Some(token) = cookie_of(req, SESSION_COOKIE, secure(url), "/")? else { return frame_note(&platform, &name, &back, Unsigned::SignedOut) };
    match mint_for(env, url, &token, &name, &back, Some(&platform)).await? {
        Minting::Redeem(redeem) => {
            let mut resp = to_signin(cfg, url, &name, &redeem)?;
            // the platform's page, by its path, is nothing the fragment hears
            resp.headers_mut().set("referrer-policy", "no-referrer")?;
            Ok(resp)
        }
        // asking is a tab's: the question is a page no frame shows (`consent_page`)
        Minting::Ask(..) => frame_note(&platform, &name, &back, Unsigned::Consent),
        Minting::SignedOut => frame_note(&platform, &name, &back, Unsigned::SignedOut),
    }
}

/// Why a frame of the platform's page did not sign in to a fragment.
#[derive(Clone, Copy)]
enum Unsigned {
    /// The fragment does not know the person, and they have not said yes.
    Consent,
    /// No one is signed in to the platform.
    SignedOut,
}

impl Unsigned {
    /// The word the page around the frame hears (`why`).
    fn why(self) -> &'static str {
        match self {
            Unsigned::Consent => "consent",
            Unsigned::SignedOut => "signed-out",
        }
    }
}

/// What a frame of the platform's page shows in place of a fragment it
/// cannot sign in to, and the word it sends the page around it
/// (`{fragment: "signin-blocked", name, why}`), to the platform's origin
/// alone. Only the platform's own pages may frame it; it holds a link and
/// nothing else, and runs only its own inline script.
fn frame_note(platform: &str, name: &str, back: &str, why: Unsigned) -> CellResult<Response> {
    assert!(fragment_core::frames::is_origin(platform), "a message's target is the platform's origin");
    assert!(fragment_proto::valid_fragment_name(name), "a frame's note names a fragment by its checked name");
    let label = share::label(name);
    let l = esc(label);
    let (title, body) = match why {
        Unsigned::Consent => (
            format!("Open {label} to continue"),
            format!(
                "<p><b>{l}</b> is not yours, and no one shared it with you: it learns who you are only once you say so, in a tab of its own.</p>\
                 <p><a href=\"/auth/fragment?name={qn}&amp;return={qb}\" target=\"_blank\" rel=\"noopener\">Open {l} in a tab</a></p>",
                qn = esc(&enc(name)),
                qb = esc(&enc(back)),
            ),
        ),
        Unsigned::SignedOut => (format!("Sign in to see {label}"), format!("<p>You're signed out, so <b>{l}</b> can't show here as you.</p><p><a href=\"/auth/login\" target=\"_top\">Sign in</a></p>")),
    };
    let body = format!(
        "{body}<script>parent.postMessage({{ fragment: \"signin-blocked\", name: {name:?}, why: {w:?} }}, {platform:?})</script>",
        w = why.why(),
    );
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "no-store")?;
    h.set("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'self'")?;
    h.set("x-frame-options", "SAMEORIGIN")?;
    Ok(Response::ok(html(&title, &body))?.with_headers(h))
}

/// "Continue to X as you?": a fragment that is not the person's, nor
/// shared with them, learns who they are only once they say yes. A
/// sharing page's protections (share.rs): unframed, a form token, a button
/// that arms after a moment. Its form redirects on to the fragment's
/// origin, so it has no forms-here rule.
fn consent_page(session: &str, who: &Signed, email: Option<&str>, name: &str, back: &str) -> CellResult<Response> {
    let label = share::label(name);
    let you = match email {
        Some(e) => esc(e),
        None => format!("<code>{}</code>", esc(&who.id)),
    };
    let body = format!(
        "<p><b>{l}</b> (<code>{n}</code>) is not yours, and no one shared it with you. Continuing lets its author's code know who you are: {you}.</p>\
         <form method=\"post\" action=\"/auth/fragment?name={qn}&amp;return={qb}\"><input type=\"hidden\" name=\"form\" value=\"{f}\"><button data-arm disabled>Continue as {you}</button></form>\
         <p class=\"hint\">Asked once for each fragment; signing out of it there makes it ask again. Or <a href=\"/\">go back</a>.</p>",
        l = esc(label),
        n = esc(name),
        qn = esc(&enc(name)),
        qb = esc(&enc(back)),
        f = esc(&form::issue(session, &format!("consent:{name}"), crate::js::now_ms())),
    );
    share::sheet_page(200, &format!("Continue to {label}?"), &body, false)
}

/// What a frame shows when it cannot sign in: its browser kept the frame
/// cookie out, or a page framed its sign-in without the platform's mint
/// (`/auth/frame`). A link to open the fragment in a tab of its own, and a
/// word to the page around it (`{fragment: "signin-blocked", name, why:
/// "cookies"}`; the platform's own notes say the other whys). It holds
/// nothing but the link, so any page may frame it, and the word goes to
/// whichever page that is: it names only the fragment that page framed.
fn blocked(cfg: &Config, url: &Url, name: &str, back: &str) -> CellResult<Response> {
    let label = esc(share::label(name));
    let open = format!("{}__signin?return={}", cfg.canonical(url, name), enc(back));
    let body = format!(
        "<p><b>{label}</b> can't sign you in inside this page.</p><p><a href=\"{o}\" target=\"_blank\" rel=\"noopener\">Open {label} in a tab</a></p>\
         <script>parent.postMessage({{ fragment: \"signin-blocked\", name: {n:?}, why: \"cookies\" }}, \"*\")</script>",
        o = esc(&open),
        n = name,
    );
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "no-store")?;
    Ok(Response::ok(html(&format!("{} is signed out here", share::label(name)), &body))?.with_headers(h))
}

/// Whether a refusal says who may do something (401, 403): what a page
/// answers a browser's navigation with (`refused`).
pub(crate) fn is_refusal(code: ErrorCode) -> bool {
    matches!(code, ErrorCode::Unauthenticated | ErrorCode::Forbidden)
}

/// A refusal a browser navigated to on a fragment's origin, answered as the
/// platform's page instead of the API's JSON (decision R4). A
/// top-level visit that no session here admits (401) goes to the
/// platform's sign-in for this fragment, and back to the page it asked for:
/// at once on the person's own fragments and those shared with them, after
/// asking on anyone else's, and through sign-in first when signed out
/// (`/auth/fragment`). Anything else says what happened and what to do:
/// sign in, ask its owner, or, when a `?view=` opened nothing, ask for the
/// new link; the platform's own reason under it. From a frame, where
/// neither sign-in nor the platform's pages show, its links open a tab.
pub(crate) fn refused(cfg: &Config, url: &Url, name: &str, rest: &str, framed: bool, e: &CellError) -> CellResult<Response> {
    assert!(is_refusal(e.code), "only a refusal is answered as this page");
    let signing_in = is_fragment_route(rest);
    // the page asked for; sign-in's own way back for sign-in's routes
    let back = match (signing_in, url.query()) {
        (true, _) => site::return_path(query(url, "return").as_deref()),
        (false, Some(q)) => site::return_path(Some(&format!("/{rest}?{q}"))),
        (false, None) => site::return_path(Some(&format!("/{rest}"))),
    };
    let link_changed = !signing_in && query(url, "view").is_some();
    let signed_out = e.code == ErrorCode::Unauthenticated;
    if signed_out && !framed && !link_changed && !signing_in {
        return redirect(&format!("{}/auth/fragment?name={name}&return={}", cfg.platform(), enc(&back)), &[]);
    }
    // its owner is not named: who owns it is theirs to tell, to whom they share it with
    let (label, ask) = (crate::share::label(name), "its owner".to_string());
    let base = cfg.canonical(url, name);
    let tab = if framed { " target=\"_blank\" rel=\"noopener\"" } else { "" };
    let a = |href: &str, text: &str| format!("<a href=\"{}\"{tab}>{}</a>", esc(href), esc(text));
    let sign_in = a(&format!("{base}__signin?return={}", enc(&back)), &format!("Sign in to {label}"));
    let home = a(&format!("{}/", cfg.platform()), "Your fragments");
    let why = format!("<p style=\"opacity:.7;font-size:.9em\">{}</p>", esc(&e.message));
    let l = esc(label);
    let (title, body) = if link_changed {
        let links = if signed_out { format!("{sign_in} · {home}") } else { home };
        ("This link has changed".to_string(), format!("<p>The link you opened no longer opens <b>{l}</b>: ask {ask} for the new one.</p>{why}<p>{links}</p>"))
    } else if signed_out {
        ("You need to sign in".to_string(), format!("<p>Sign in to open <b>{l}</b>.</p>{why}<p>{sign_in}</p>"))
    } else {
        let sign_out = a(&format!("{base}__signout"), &format!("Sign out of {label}"));
        (format!("You don't have access to {label}"), format!("<p>Ask {ask} to share it with you.</p>{why}<p>{home} · {sign_out}</p>"))
    };
    let h = Headers::new();
    h.set("content-type", "text/html; charset=utf-8")?;
    h.set("cache-control", "no-store")?;
    Ok(Response::ok(html(&title, &body))?.with_status(e.code.status()).with_headers(h))
}

/// Whether this origin's session cookie names someone on `name`.
async fn signed_in_here(req: &Request, env: &Env, name: &str, url: &Url) -> CellResult<bool> {
    match site_token(req, url)? {
        Some(token) => Ok(Credential::Session(token).resolve(env, name).await?.is_some()),
        None => Ok(false),
    }
}

/// Whether a path on a fragment's origin is sign-in's.
pub fn is_fragment_route(rest: &str) -> bool {
    matches!(rest, "__signin" | "__signout")
}

/// `__signin` and `__signout` on a fragment's own origin, as the
/// request's Fetch Metadata allows (`fetched`, the router's):
///
/// - `__signin?token=` redeems in a frame only a frame redemption, into
///   the frame cookie, then looks whether the browser kept it
///   (`check=frame`); anywhere else, only a top-level one, into the site
///   cookie;
/// - `__signin` without one starts at the platform from a navigation of
///   a page only: in a frame it answers `blocked`, and from an image, a
///   script, or a fetch it is refused;
/// - `__signout` is a button (`GET`), and a POST from this origin's own
///   page only.
pub async fn fragment(req: &Request, env: &Env, cfg: &Config, url: &Url, name: &str, rest: &str, fetched: Fetched) -> CellResult<Response> {
    let base = cfg.canonical(url, name);
    let back = site::return_path(query(url, "return").as_deref());
    let ttl_s = crate::registry::SESSION_TTL_MS / 1000;
    match (rest, req.method()) {
        ("__signin", Method::Get) => {
            if let Some(redeem) = query(url, "token") {
                let redeemed = ask_registry(env, &calls::Redeem { redeem, fragment: name.to_string(), framed: fetched.framed }).await?;
                return match fetched.framed {
                    true => redirect(&format!("{base}__signin?check=frame&return={}", enc(&redeemed.return_to)), &[frame_cookie(FRAME_COOKIE, &redeemed.token, "/", ttl_s, secure(url))]),
                    false => redirect(&back_to(&base, Some(&redeemed.return_to))?, &[set_cookie(SITE_COOKIE, &redeemed.token, "/", ttl_s, secure(url))]),
                };
            }
            if fetched.framed {
                let kept = query(url, "check").as_deref() == Some("frame") && fetched.frame && frame_token(req, url)?.is_some();
                return if kept { redirect(&back_to(&base, Some(&back))?, &[]) } else { blocked(cfg, url, name, &back) };
            }
            if !fetched.navigation {
                return Err(CellError::new(ErrorCode::Forbidden, "sign in by opening this page: not from another page's image, script, or fetch"));
            }
            // signed in here already: straight back
            if fetched.site && signed_in_here(req, env, name, url).await? {
                return redirect(&back_to(&base, Some(&back))?, &[]);
            }
            redirect(&format!("{}/auth/fragment?name={name}&return={}", cfg.platform(), enc(&back)), &[])
        }
        ("__signout", Method::Get) => page(
            200,
            &format!("Sign out of {}", share::label(name)),
            "<form method=\"post\" action=\"__signout\"><button>Sign out</button></form><p>It also forgets that you let it know who you are: signing in again asks again.</p>",
        ),
        ("__signout", Method::Post) => {
            same_origin(req, &cfg.origin(url, name))?;
            // The sessions end in the registry, so a copy of a cookie is
            // nobody too. The browser is signed out whatever the registry
            // answers: a registry that cannot end them is logged, and they
            // last until they expire or the platform session ends
            // (`/auth/logout` ends every one).
            let (site, frame) = (site_token(req, url)?, frame_token(req, url)?);
            if site.is_some() || frame.is_some() {
                if let Err(e) = ask_registry(env, &calls::EndSession { site, frame, fragment: name.to_string() }).await {
                    console_error!("__signout on {name}: the registry did not end the sessions ({:?}): {}", e.code, e.message);
                }
            }
            Ok(redirect(&base, &[set_cookie(SITE_COOKIE, "", "/", 0, secure(url)), frame_cookie(FRAME_COOKIE, "", "/", 0, secure(url))])?.with_status(303))
        }
        (_, m) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {rest}", m.as_ref()))),
    }
}
