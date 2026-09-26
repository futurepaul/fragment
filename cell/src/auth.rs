//! Sign-in at the router (phase 4 slice B; the registry's half is
//! `registry/signin.rs`). WorkOS AuthKit authenticates people on the
//! platform origin; the platform holds no key for them, only sessions.
//!
//! Platform origin:
//!
//!   GET  /                        the home: your fragments, a new one, pairing your CLI; or sign in
//!   GET  /auth/login?return=&login_hint=&invitation_token=   → WorkOS (a state cookie binds the
//!                                 round trip; an invitation's token lets its invitee sign up, since
//!                                 sign-up is off: WorkOS's "User invitation URL" points here)
//!   GET  /auth/link?return=       the same, adding a second sign-in to the signed-in person
//!   GET  /auth/callback           WorkOS → the code exchanged here → a session cookie
//!   GET  /auth/logout             a button; POST ends the session (and its site sessions)
//!   GET  /auth/fragment?name=&return=      a single-use redemption for one fragment's origin; for
//!                                 a fragment that is not theirs, nor shared with them, a
//!                                 question first ("Continue to X as you?"), asked once
//!   POST /auth/fragment?name=&return=      the yes (remembered), then the redemption
//!   GET  /cli?key=<npub>&proof=   the signed-in person approves a CLI key: the link
//!                                 carries the key's own proof (a NIP-98 event by it for
//!                                 `POST <platform>/cli/approve`, ten minutes good), so
//!                                 approving adds the key at once
//!   GET  /cli?key=&proof=&computer=<name>   a machine's key, as the person's computer
//!                                 (an identity they own); its proof names it, in its URL
//!   POST /cli/approve             (the form)
//!
//! On fragment.club the platform is cross-site from every fragment (they
//! are on fragment.boats), so its session cookie reaches a fragment's page
//! only on a top-level visit; a fleet whose platform shares the fragments'
//! domain puts them on one site, where the pages' forms, fetches, and
//! frames carry it. Either way every page here refuses frames (`unframed`)
//! and severs a window that opened it (`unopened`), and every form is
//! refused from another origin (`same_origin`). Sharing's pages (share.rs) add a form token and armed
//! buttons.
//!
//! Each platform page asks the registry once: a call that needs the
//! signed-in person carries the session's token, and the registry checks
//! it in the same turn (`calls::By::Session`, `Mint`, `ApproveKey`,
//! `Begin`'s `link_to`); a page that shows them gets their email with the
//! session. Only a picture asks twice: its bytes are stored before the
//! registry names them, so the session is checked before they are.
//!
//! A fragment's origin: `__signin?token=&return=` redeems the platform's
//! redemption for this fragment only and sets its own session cookie (a
//! frame's partitioned one, in a frame: `__frame`); `__signin` without a
//! token starts at the platform, from a navigation of a page of its own
//! only; `__signout` (a POST from the fragment's own page) ends its
//! sessions in the registry and drops the cookies. Which cookies count on
//! a request is the router's to say (`crate::Fetched`); each is looked up
//! live on every request whose answer depends on who is asking (the
//! fragment decides which: `routed::Credential`). A refusal a browser
//! navigates to there is a page (`refused`): a top-level visit no session
//! admits goes through `/auth/fragment` and back.

use fragment_core::{form, npub, site};
use fragment_proto::{ErrorCode, FragmentList, IdentityKind, Role, Visibility};
use worker::*;

use crate::ask_registry;
use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls::{self, Consent};
use crate::routed::{Credential, Signed};
use crate::{share, Fetched};

pub const SESSION_COOKIE: &str = "fragment_session";
pub const SITE_COOKIE: &str = "fragment_site";
/// A frame's session cookie (`__frame`): partitioned, for the page that
/// framed it.
pub const FRAME_COOKIE: &str = "fragment_frame";
const LOGIN_COOKIE: &str = "fragment_login";
const LOGIN_HINT_MAX: usize = 320;
const INVITATION_TOKEN_MAX: usize = 256;
/// How long an approval link's proof is good.
const LINK_PROOF_WINDOW_S: i64 = 600;
const LINK_PROOF_MAX: usize = 4096;
/// The approval form: a key, and a proof of at most `LINK_PROOF_MAX` bytes
/// that form encoding may triple.
const APPROVE_FORM_MAX_BYTES: usize = 16 * 1024;
const _: () = assert!(APPROVE_FORM_MAX_BYTES >= 3 * LINK_PROOF_MAX + 256, "the form holds the longest proof, encoded");
/// The computer an approval names (`computer=`, a label), if any: `None`
/// for a person's own key.
fn computer_named(raw: Option<String>) -> CellResult<Option<String>> {
    match raw.filter(|n| !n.is_empty()) {
        Some(name) if fragment_proto::valid_label(&name) => Ok(Some(name)),
        Some(_) => Err(CellError::invalid("a computer's name is a label: lowercase letters, digits, and single dashes")),
        None => Ok(None),
    }
}

/// A form of a few short fields: a new fragment's label and template, or a
/// username.
const SHORT_FORM_MAX_BYTES: usize = 4 * 1024;

/// The key an approval link's proof is by, if it is good: a NIP-98 event
/// by that key for `POST <platform>/cli/approve`, made within ten minutes;
/// a computer's names it in the URL (`?computer=<name>`), so a proof made
/// to pair a computer never adds its key to the person, nor the other way.
fn link_proof(platform: &str, key_hex: &str, proof: &str, computer: Option<&str>) -> CellResult<()> {
    let stale = || CellError::invalid("this approval link is not good (it is older than ten minutes, or for another key): run `fragment login` again for a fresh one");
    if proof.is_empty() || proof.len() > LINK_PROOF_MAX {
        return Err(stale());
    }
    assert!(computer.is_none_or(fragment_proto::valid_label), "a computer's name is checked before its proof");
    let approve = match computer {
        Some(name) => format!("{platform}/cli/approve?computer={name}"),
        None => format!("{platform}/cli/approve"),
    };
    let now_s = crate::js::now_ms() / 1000;
    let signer = fragment_nip98::verify(Some(&format!("Nostr {proof}")), "POST", &approve, &[], now_s, LINK_PROOF_WINDOW_S).map_err(|_| stale())?;
    if signer != key_hex {
        return Err(stale());
    }
    Ok(())
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

/// A frame's session cookie: `SameSite=None`, so it is sent in a frame of
/// another site's page, and `Partitioned` (CHIPS), so a browser that
/// blocks third-party cookies keeps it, in that page's partition only.
/// Both need `Secure`, which browsers take from `localhost` over http too.
fn frame_cookie(value: &str, path: &str, max_age_s: i64, secure: bool) -> String {
    let name = cookie_name(FRAME_COOKIE, secure, path);
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
/// frame signs in only through `__frame`).
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

/// Where a fragment's session cookie lives: its whole host, or its path
/// when fragments share the platform's origin (no suffix).
fn site_cookie_path(name: &str, path_mode: bool) -> String {
    if path_mode {
        format!("/f/{name}/")
    } else {
        "/".to_string()
    }
}

/// The token of a fragment origin's own session cookie, unresolved (the
/// registry says whom it names, for that fragment only).
pub fn site_token(req: &Request, name: &str, url: &Url, path_mode: bool) -> CellResult<Option<String>> {
    cookie_of(req, SITE_COOKIE, secure(url), &site_cookie_path(name, path_mode))
}

/// The token of its frame cookie, unresolved.
pub fn frame_token(req: &Request, name: &str, url: &Url, path_mode: bool) -> CellResult<Option<String>> {
    cookie_of(req, FRAME_COOKIE, secure(url), &site_cookie_path(name, path_mode))
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

/// The platform bar's budget: what is left of this month's allowance.
async fn budget_line(env: &Env, id: &str) -> String {
    use fragment_core::budget::dollars;
    let Some(org) = crate::ledger::org_of(id) else { return String::new() };
    match crate::ledger::ask(env, &org, &crate::ledger::Status {}).await {
        Ok(month) => {
            let warn = if month.warn { " <b>Most of it is used.</b>" } else { "" };
            format!(
                "<p>AI this month ({}): <b>{}</b> of {} left.{warn}</p>",
                esc(&month.period),
                dollars(month.remaining_micros.max(0)),
                dollars(month.allowance_micros)
            )
        }
        Err(_) => String::new(),
    }
}

/// The fragments a person belongs to: each one's link, whose it is (and,
/// on the owner's rows, which carry its sharing, who may open it), and its
/// share sheet, where a member sees who is in.
async fn fragments_list(env: &Env, cfg: &Config, url: &Url, id: &str) -> String {
    let asked = async {
        let list = Request::new("https://principal.internal/list", Method::Get)?;
        env.durable_object("PRINCIPAL")?.get_by_name(id)?.fetch_with_request(list).await?.json::<FragmentList>().await
    };
    let Ok(list) = asked.await else { return String::new() };
    let items: String = list
        .fragments
        .iter()
        .map(|f| {
            let link = cfg.canonical(url, &f.name);
            let whose = match (f.role, f.sharing.as_ref().map(|s| s.visibility)) {
                (Role::Owner, Some(Visibility::Public)) => "yours · anyone".to_string(),
                (Role::Owner, Some(Visibility::Link)) => "yours · anyone with the link".to_string(),
                (Role::Owner, Some(Visibility::Members)) => "yours · only the people in it".to_string(),
                (Role::Owner, None) => "yours".to_string(),
                (role, _) => format!("shared with you · {}", role.as_str()),
            };
            let shown = link.split("://").nth(1).unwrap_or(&link).trim_end_matches('/');
            format!("<li><a href=\"{}\">{}</a> <small>{whose} · <a href=\"/share/{}\">Share</a></small></li>", esc(&link), esc(shown), esc(&f.name))
        })
        .collect();
    match items.is_empty() {
        true => "<h2>Your fragments</h2><p>None yet: make one below, or with <code>fragment init</code> in your terminal.</p>".to_string(),
        false => format!("<h2>Your fragments</h2><ul>{items}</ul>"),
    }
}

/// What follows a label in a fragment's address: `--<username>.<suffix>`
/// (decision 16), or, on a fleet without a suffix, `.<username>`.
fn after_label(cfg: &Config, username: &str) -> String {
    match &cfg.host_suffix {
        Some(suffix) => format!("--{username}.{suffix}"),
        None => format!(".{username}"),
    }
}

/// The CLI's one-line install, for macOS and Linux: the latest release's
/// tarball for this machine (`.github/workflows/release.yml`). `cargo xtask
/// check` holds it to cli/SKILL.md's, and that to the release's assets.
const INSTALL: &str = "mkdir -p ~/.local/bin && curl -fsSL https://github.com/futurepaul/fragment/releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin";
/// A coding agent's skill, in Claude Code's folder (other agents take the
/// same file in their own).
const SKILL: &str = "mkdir -p ~/.claude/skills/fragment && fragment skill > ~/.claude/skills/fragment/SKILL.md";

/// Pairing a CLI, and a coding agent that drives it.
fn pair() -> String {
    format!(
        "<h2>Pair your CLI</h2><p>Install it (macOS or Linux; if <code>fragment</code> is not found after, put <code>~/.local/bin</code> on your PATH):</p><pre><code>{}</code></pre>\
         <p>Then run <code>fragment login</code>: it opens this site to approve its key. To have your coding agent (Claude Code, Codex) do the work, give it the skill:</p><pre><code>{}</code></pre>",
        esc(INSTALL),
        esc(SKILL)
    )
}

/// The "new fragment" form: a label, and one of the platform's templates.
fn new_form(after: &str) -> String {
    let choices: String = crate::publish::TEMPLATES
        .iter()
        .enumerate()
        .map(|(i, (name, t))| {
            let (title, description) = crate::publish::describe(t);
            format!(
                "<p><label><input type=\"radio\" name=\"template\" value=\"{n}\"{c}> <b>{t}</b> {d}{f}</label></p>",
                n = esc(name),
                c = if i == 0 { " checked" } else { "" },
                t = esc(if title.is_empty() { name } else { &title }),
                d = esc(&description),
                // making it here is the owner's frame grant (`/auth/new`), so the form says what it allows
                f = if crate::publish::frames(name) { "<br><small>It will show your fragments inside it, signed in as you: making it here allows that. You can stop it in its share sheet.</small>" } else { "" },
            )
        })
        .collect();
    format!(
        "<h2>New fragment</h2><form method=\"post\" action=\"/auth/new\">{choices}\
         <p><input name=\"label\" required maxlength=\"63\" pattern=\"[a-z0-9]([a-z0-9-]*[a-z0-9])?\" placeholder=\"name\" style=\"font:inherit;padding:.4em .6em;border-radius:8px;border:1px solid #aab\"><code>{a}</code> <button>Make it</button></p></form>",
        a = esc(after),
    )
}

pub(crate) fn to_login(platform: &str, back: &str) -> CellResult<Response> {
    redirect(&format!("{platform}/auth/login?return={}", enc(back)), &[])
}

async fn begin(env: &Env, cfg: &Config, url: &Url, link: Option<String>) -> CellResult<Response> {
    let workos = cfg.workos()?;
    let platform = cfg.platform(url);
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
    redirect(&to, &[set_cookie(LOGIN_COOKIE, &state, "/", 600, secure(url))])
}

async fn callback(req: &Request, env: &Env, cfg: &Config, url: &Url) -> CellResult<Response> {
    let workos = cfg.workos()?;
    if let Some(error) = query(url, "error") {
        let why = query(url, "error_description").unwrap_or_default();
        return page(400, "Sign-in did not finish", &format!("<p>WorkOS said <code>{}</code>: {}</p><p><a href=\"/auth/login\">Try again</a></p>", esc(&error), esc(&why)));
    }
    let state = query(url, "state").unwrap_or_default();
    let bound = cookie_of(req, LOGIN_COOKIE, secure(url), "/")?;
    if state.is_empty() || bound.as_deref() != Some(state.as_str()) {
        return page(400, "Sign-in did not start here", "<p>This sign-in began in another browser, or too long ago.</p><p><a href=\"/auth/login\">Start again</a></p>");
    }
    let code = query(url, "code").ok_or_else(|| CellError::invalid("the callback carries no code"))?;
    // the registry exchanges the code: WorkOS's API key is the node's (KEYS)
    let done = ask_registry(env, &calls::Exchange { state, code, client_id: workos.client_id.clone(), issuer: workos.issuer() }).await?;
    redirect(
        &back_to(&format!("{}/", cfg.platform(url)), Some(&done.return_to))?,
        &[
            set_cookie(SESSION_COOKIE, &done.token, "/", crate::registry::SESSION_TTL_MS / 1000, secure(url)),
            set_cookie(LOGIN_COOKIE, "", "/", 0, secure(url)),
        ],
    )
}

/// Sign-in's routes on the platform origin (the router sends only these).
pub async fn platform(mut req: Request, env: &Env, cfg: &Config, url: &Url, segments: &[&str]) -> CellResult<Response> {
    let method = req.method();
    let platform = cfg.platform(url);
    {
        match (method, segments) {
            (Method::Get, [""]) => {
                let body = match platform_session(&req, env, url).await? {
                    // a username first: fragments live under it (decision 16)
                    Some((_, live)) if live.identity.username.is_none() && live.identity.kind == IdentityKind::Person => {
                        let email = live.email.unwrap_or_default();
                        format!(
                            "<h1>Choose your username</h1><p>Signed in as <b>{}</b>. Your fragments will live at <code>&lt;label&gt;{}</code>; a username is chosen once.</p>\
                             <form method=\"post\" action=\"/auth/username\"><p><input name=\"username\" required minlength=\"3\" maxlength=\"32\" pattern=\"[a-z0-9]([a-z0-9-]*[a-z0-9])?\" autocomplete=\"username\" style=\"font:inherit;padding:.4em .6em;border-radius:8px;border:1px solid #aab\"> <button>Take it</button></p></form>\
                             <p><a href=\"/auth/logout\">Sign out</a></p>",
                            esc(&email),
                            esc(&after_label(cfg, "username")),
                        )
                    }
                    Some((_, live)) => {
                        let (who, email) = (live.identity, live.email.unwrap_or_default());
                        let username = who.username.clone().unwrap_or_default();
                        // the month and the fragments, asked of their cells at once
                        let (budget, fragments) = futures_util::future::join(budget_line(env, &who.id), fragments_list(env, cfg, url, &who.id)).await;
                        format!(
                            "<p><img src=\"/api/users/{u}/picture\" alt=\"\" width=\"48\" height=\"48\" style=\"border-radius:50%;vertical-align:middle;object-fit:cover\" onerror=\"this.remove()\"> Signed in as <b>{u}</b> ({e}).</p>{b}{f}{n}{p}\
                             <h2>You</h2><form method=\"post\" action=\"/auth/picture\" enctype=\"multipart/form-data\"><p>Picture: <input type=\"file\" name=\"picture\" accept=\"image/png,image/jpeg,image/webp,image/gif\" required> <button>Set</button></p></form>\
                             <p><a href=\"/auth/logout\">Sign out</a> · <a href=\"/auth/link\">Add another sign-in to you</a> · <code>{id}</code></p>",
                            u = esc(&username),
                            e = esc(&email),
                            id = esc(&who.id),
                            b = budget,
                            f = fragments,
                            n = new_form(&after_label(cfg, &username)),
                            p = pair(),
                        )
                    }
                    None => "<p>Small web apps that keep their state, live for everyone who opens them. Invite-only for now.</p><p><a href=\"/auth/login\">Sign in</a></p>".to_string(),
                };
                page(200, "fragment", &body)
            }
            (Method::Get, ["auth", "login"]) => begin(env, cfg, url, None).await,
            // the registry checks the session is live as the sign-in begins
            (Method::Get, ["auth", "link"]) => match cookie_of(&req, SESSION_COOKIE, secure(url), "/")? {
                Some(token) => match begin(env, cfg, url, Some(token)).await {
                    Err(e) if e.code == ErrorCode::Unauthenticated => to_login(&platform, "/auth/link"),
                    begun => begun,
                },
                None => to_login(&platform, "/auth/link"),
            },
            (Method::Get, ["auth", "callback"]) => callback(&req, env, cfg, url).await,
            (Method::Post, ["auth", "username"]) => {
                same_origin(&req, &platform)?;
                let Some(token) = cookie_of(&req, SESSION_COOKIE, secure(url), "/")? else { return to_login(&platform, "/") };
                // read before the registry says who is signed in: bounded as it arrives
                let bytes = crate::read_body(&mut req, SHORT_FORM_MAX_BYTES).await?;
                let username = url::form_urlencoded::parse(&bytes).find(|(k, _)| k == "username").map(|(_, v)| v.trim().to_string());
                let Some(username) = username else { return Err(CellError::invalid("choose a username")) };
                match ask_registry(env, &calls::ClaimUsername { by: calls::By::Session(token), username }).await {
                    Ok(_) => redirect("/", &[]),
                    Err(e) if e.code == ErrorCode::Unauthenticated => to_login(&platform, "/"),
                    Err(e) if matches!(e.code, ErrorCode::AlreadyExists | ErrorCode::InvalidRequest) => {
                        page(400, "Choose your username", &format!("<p>{}</p><p><a href=\"/\">Try another</a></p>", esc(&e.message)))
                    }
                    Err(e) => Err(e),
                }
            }
            (Method::Post, ["auth", "new"]) => {
                same_origin(&req, &platform)?;
                let Some((_, live)) = platform_session(&req, env, url).await? else { return to_login(&platform, "/") };
                let bytes = crate::read_body(&mut req, SHORT_FORM_MAX_BYTES).await?;
                let field = |name: &str| url::form_urlencoded::parse(&bytes).find(|(k, _)| k == name).map(|(_, v)| v.trim().to_string()).unwrap_or_default();
                let create = fragment_proto::CreateFragment { name: field("label"), visibility: None, template: Some(field("template")) };
                let owner = Signed::new(live.identity, None);
                let v: serde_json::Value = match crate::create_fragment(env, cfg, url, create, owner.clone()).await {
                    Ok(mut made) if made.status_code() == 200 => made.json().await?,
                    Ok(mut made) => {
                        let v: serde_json::Value = made.json().await.unwrap_or_default();
                        return page(400, "New fragment", &format!("<p>{}</p><p><a href=\"/\">Back</a></p>", esc(v["message"].as_str().unwrap_or("it could not be made"))));
                    }
                    Err(e) => return page(400, "New fragment", &format!("<p>{}</p><p><a href=\"/\">Back</a></p>", esc(&e.message))),
                };
                let name = v["name"].as_str().ok_or_else(|| CellError::host("the create answered no name"))?;
                // The form said it will show their fragments inside it: its submit is the owner's grant, the
                // share sheet's own. No page can send it (the Origin is the platform's). One that does not land
                // leaves the desktop asking, with its share sheet's button.
                if crate::publish::frames(&field("template")) {
                    let allow = Some(serde_json::json!({ "granted": true }));
                    if let Err(e) = crate::share::ask(env, url, name, &owner, Method::Put, "/api/grants/frame", allow).await {
                        console_error!("{name}: the form's frame grant did not land ({:?}): {}", e.code, e.message);
                    }
                }
                // signed in on its own origin, then there
                redirect(&format!("/auth/fragment?name={}&return=/", enc(name)), &[])
            }
            (Method::Post, ["auth", "picture"]) => {
                // Two round trips, on purpose: the bytes land in BLOBS before
                // the registry names them, so the session is checked first.
                same_origin(&req, &platform)?;
                let Some((token, _)) = platform_session(&req, env, url).await? else { return to_login(&platform, "/") };
                let form = req.form_data().await?;
                let Some(FormEntry::File(file)) = form.get("picture") else { return Err(CellError::invalid("choose a picture")) };
                let bytes = file.bytes().await?;
                if bytes.len() > fragment_proto::limits::PICTURE_MAX_BYTES {
                    return page(400, "Picture", &format!("<p>A picture is at most {} KiB.</p><p><a href=\"/\">Back</a></p>", fragment_proto::limits::PICTURE_MAX_BYTES / 1024));
                }
                let Some(mime) = crate::picture_type(&bytes) else {
                    return page(400, "Picture", "<p>A picture is a PNG, JPEG, WebP, or GIF.</p><p><a href=\"/\">Back</a></p>");
                };
                let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes));
                crate::js::blob_put_bytes(env.as_ref(), &format!("pictures/{sha}"), &bytes).await?;
                ask_registry(env, &calls::SetPicture { by: calls::By::Session(token), sha, mime: mime.to_string() }).await?;
                redirect("/", &[])
            }
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
                    (Some(sid), Ok(w)) => redirect(
                        &format!("{}/user_management/sessions/logout?session_id={}&return_to={}", w.api, enc(&sid), enc(&format!("{platform}/"))),
                        &[clear],
                    ),
                    _ => redirect("/", &[clear]),
                }
            }
            (Method::Get, ["auth", "fragment"]) => {
                let (name, back) = fragment_asked(url)?;
                let signed_out = || to_login(&platform, &format!("/auth/fragment?name={name}&return={}", enc(&back)));
                let Some(token) = cookie_of(&req, SESSION_COOKIE, secure(url), "/")? else { return signed_out() };
                // the registry checks the session is live as it mints, and
                // mints only for a fragment the person said yes to
                let mint = |consent| calls::Mint { token: token.clone(), fragment: name.clone(), return_to: back.clone(), consent };
                let minted = match ask_registry(env, &mint(Consent::Remembered)).await {
                    Err(e) if e.code == ErrorCode::Unauthenticated => return signed_out(),
                    minted => minted?,
                };
                if let Some(redeem) = minted.redeem {
                    return to_signin(cfg, url, &name, &redeem);
                }
                // silent on their own fragments and those shared with them,
                // which know them already (answer 1, docs/fragment-boats.md)
                let who = Signed::new(minted.identity, None);
                match share::ask(env, url, &name, &who, Method::Get, "/api/status", None).await {
                    Ok(_) => {
                        let minted = ask_registry(env, &mint(Consent::Member)).await?;
                        to_signin(cfg, url, &name, minted.redeem.as_deref().ok_or_else(|| CellError::host("a member's mint answered no redemption"))?)
                    }
                    Err(e) if matches!(e.code, ErrorCode::Forbidden | ErrorCode::Unauthenticated) => consent_page(&token, &who, &name, &back),
                    Err(e) => Err(e),
                }
            }
            (Method::Post, ["auth", "fragment"]) => {
                let (name, back) = fragment_asked(url)?;
                let (session, _, _) = match share::poster(&mut req, env, url, &platform, &format!("consent:{name}")).await? {
                    Ok(posted) => posted,
                    Err(page) => return Ok(page),
                };
                let minted = ask_registry(env, &calls::Mint { token: session, fragment: name.clone(), return_to: back, consent: Consent::Given }).await?;
                Ok(to_signin(cfg, url, &name, minted.redeem.as_deref().ok_or_else(|| CellError::host("a given mint answered no redemption"))?)?.with_status(303))
            }
            (Method::Get, ["cli"]) => {
                let key = query(url, "key").unwrap_or_default();
                let proof = query(url, "proof").unwrap_or_default();
                let Some(hex) = npub::parse(&key) else { return page(400, "Not a key", "<p>This link names no key. Run <code>fragment login</code> again.</p>") };
                let computer = match computer_named(query(url, "computer")) {
                    Ok(computer) => computer,
                    Err(e) => return page(400, "Not a computer's name", &format!("<p>{}</p>", esc(&e.message))),
                };
                if let Err(e) = link_proof(&platform, &hex, &proof, computer.as_deref()) {
                    return page(400, "This link has expired", &format!("<p>{}</p>", esc(&e.message)));
                }
                let Some((_, live)) = platform_session(&req, env, url).await? else {
                    let named = computer.as_deref().map(|c| format!("&computer={c}")).unwrap_or_default();
                    return to_login(&platform, &format!("/cli?key={}&proof={}{named}", enc(&key), enc(&proof)));
                };
                let (who, email) = (live.identity, live.email.unwrap_or_default());
                let you = esc(if email.is_empty() { &who.id } else { &email });
                let npub = npub::encode(&hex);
                let tail = &npub[npub.len() - 8..];
                // a computer is not its owner: its page says so, and what it may do, first
                let (title, lead, button, named) = match &computer {
                    Some(c) => (
                        "Pair a computer",
                        format!("<p>A machine wants to be your computer <b>{c}</b>: an identity of its own, owned by <b>{you}</b>, that never acts as you. It works only in the fragments you add it to (<code>fragment members add &lt;fragment&gt; {c} --role editor</code>) and in those it makes, which are yours, on your budget. It cannot add keys, share anything, or make agents; <code>fragment computers rm {c}</code> removes it.</p>"),
                        format!("Pair {c} as your computer"),
                        format!("<input type=\"hidden\" name=\"computer\" value=\"{c}\">"),
                    ),
                    None => ("Add a key to you", format!("<p>A <code>fragment</code> CLI wants to act as <b>{you}</b>.</p>"), "Add this key".to_string(), String::new()),
                };
                page(
                    200,
                    title,
                    &format!(
                        "{lead}<p>Its key ends in <code>{tail}</code>; check that its terminal shows the same ending.</p>
<form method=\"post\" action=\"/cli/approve\"><input type=\"hidden\" name=\"key\" value=\"{}\"><input type=\"hidden\" name=\"proof\" value=\"{}\">{named}<button>{button}</button></form>
<p>Didn't run <code>fragment login</code>? Close this page.</p>",
                        esc(&npub),
                        esc(&proof)
                    ),
                )
            }
            (Method::Post, ["cli", "approve"]) => {
                same_origin(&req, &platform)?;
                // read before anyone is known to be signed in: bounded as it arrives
                let bytes = crate::read_body(&mut req, APPROVE_FORM_MAX_BYTES).await?;
                let field = |name: &str| url::form_urlencoded::parse(&bytes).find(|(k, _)| k == name).map(|(_, v)| v.into_owned()).unwrap_or_default();
                let hex = npub::parse(&field("key")).ok_or_else(|| CellError::invalid("the form names no key"))?;
                let computer = computer_named(Some(field("computer")))?;
                let Some(token) = cookie_of(&req, SESSION_COOKIE, secure(url), "/")? else {
                    return Err(CellError::new(ErrorCode::Unauthenticated, "sign in first"));
                };
                link_proof(&platform, &hex, &field("proof"), computer.as_deref())?;
                // the registry checks the session is live as it adds the key
                let Some(name) = computer else {
                    ask_registry(env, &calls::ApproveKey { token, key: hex }).await?;
                    return page(200, "Key added", "<p>This key is yours now. A <code>fragment login</code> waiting in a terminal finishes on its own.</p>");
                };
                ask_registry(env, &calls::PairComputer { token, key: hex, name: name.clone() }).await?;
                let done = format!("<p><b>{name}</b> is your computer now, acting only where you let it. A <code>fragment login --computer</code> waiting on it finishes on its own.</p>");
                page(200, "Computer paired", &done)
            }
            (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
        }
    }
}

/// The fragment and the way back `/auth/fragment` names.
fn fragment_asked(url: &Url) -> CellResult<(String, String)> {
    let name = query(url, "name").filter(|n| fragment_proto::valid_fragment_name(n)).ok_or_else(|| CellError::invalid("name a fragment"))?;
    Ok((name, site::return_path(query(url, "return").as_deref())))
}

fn to_signin(cfg: &Config, url: &Url, name: &str, redeem: &str) -> CellResult<Response> {
    redirect(&format!("{}__signin?token={redeem}", cfg.canonical(url, name)), &[])
}

/// "Continue to X as you?": a fragment that is not the person's, nor
/// shared with them, learns who they are only once they say yes. A
/// sharing page's protections (share.rs): unframed, a form token, a button
/// that arms after a moment. Its form redirects on to the fragment's
/// origin, so it has no forms-here rule.
fn consent_page(session: &str, who: &Signed, name: &str, back: &str) -> CellResult<Response> {
    let label = share::label(name);
    let you = match who.username.as_deref() {
        Some(u) => format!("@{}", esc(u)),
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
/// cookie out, or a page framed its sign-in instead of going through
/// `__frame`. A link to open the fragment in a tab of its own, and a word
/// to the page around it (`{fragment: "signin-blocked", name}`). It holds
/// nothing but the link, so any page may frame it.
fn blocked(cfg: &Config, url: &Url, name: &str, back: &str) -> CellResult<Response> {
    let label = esc(share::label(name));
    let open = format!("{}__signin?return={}", cfg.canonical(url, name), enc(back));
    let body = format!(
        "<p><b>{label}</b> can't sign you in inside this page.</p><p><a href=\"{o}\" target=\"_blank\" rel=\"noopener\">Open {label} in a tab</a></p>\
         <script>parent.postMessage({{ fragment: \"signin-blocked\", name: {n:?} }}, \"*\")</script>",
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
/// platform's page instead of the API's JSON (ROADMAP decision 4). A
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
        return redirect(&format!("{}/auth/fragment?name={name}&return={}", cfg.platform(url), enc(&back)), &[]);
    }
    let (label, ask) = match fragment_proto::split_fragment_name(name) {
        Some((label, owner)) => (label, format!("its owner, <b>@{}</b>,", esc(owner))),
        None => (name, "its owner".to_string()),
    };
    let base = cfg.canonical(url, name);
    let tab = if framed { " target=\"_blank\" rel=\"noopener\"" } else { "" };
    let a = |href: &str, text: &str| format!("<a href=\"{}\"{tab}>{}</a>", esc(href), esc(text));
    let sign_in = a(&format!("{base}__signin?return={}", enc(&back)), &format!("Sign in to {label}"));
    let home = a(&format!("{}/", cfg.platform(url)), "Your fragments");
    let why = format!("<p style=\"opacity:.7;font-size:.9em\">{}</p>", esc(&e.message));
    let l = esc(label);
    let (title, body) = if rest == "__frame" {
        // a frame's own refusal, whose page's owner acts on it
        ("This can't be shown here".to_string(), format!("<p>{}</p><p>{home}</p>", esc(&e.message)))
    } else if link_changed {
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
async fn signed_in_here(req: &Request, env: &Env, name: &str, url: &Url, path_mode: bool) -> CellResult<bool> {
    match site_token(req, name, url, path_mode)? {
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
#[allow(clippy::too_many_arguments)]
pub async fn fragment(req: &Request, env: &Env, cfg: &Config, url: &Url, name: &str, rest: &str, path_mode: bool, fetched: Fetched) -> CellResult<Response> {
    let cookie_path = site_cookie_path(name, path_mode);
    let base = cfg.canonical(url, name);
    let back = site::return_path(query(url, "return").as_deref());
    let ttl_s = crate::registry::SESSION_TTL_MS / 1000;
    match (rest, req.method()) {
        ("__signin", Method::Get) => {
            if let Some(redeem) = query(url, "token") {
                let redeemed = ask_registry(env, &calls::Redeem { redeem, fragment: name.to_string(), framed: fetched.framed }).await?;
                return match fetched.framed {
                    true => redirect(&format!("{base}__signin?check=frame&return={}", enc(&redeemed.return_to)), &[frame_cookie(&redeemed.token, &cookie_path, ttl_s, secure(url))]),
                    false => redirect(&back_to(&base, Some(&redeemed.return_to))?, &[set_cookie(SITE_COOKIE, &redeemed.token, &cookie_path, ttl_s, secure(url))]),
                };
            }
            if fetched.framed {
                let kept = query(url, "check").as_deref() == Some("frame") && fetched.frame && frame_token(req, name, url, path_mode)?.is_some();
                return if kept { redirect(&back_to(&base, Some(&back))?, &[]) } else { blocked(cfg, url, name, &back) };
            }
            if !fetched.navigation {
                return Err(CellError::new(ErrorCode::Forbidden, "sign in by opening this page: not from another page's image, script, or fetch"));
            }
            // signed in here already: straight back
            if fetched.site && signed_in_here(req, env, name, url, path_mode).await? {
                return redirect(&back_to(&base, Some(&back))?, &[]);
            }
            redirect(&format!("{}/auth/fragment?name={name}&return={}", cfg.platform(url), enc(&back)), &[])
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
            let (site, frame) = (cookie_of(req, SITE_COOKIE, secure(url), &cookie_path)?, frame_token(req, name, url, path_mode)?);
            if site.is_some() || frame.is_some() {
                if let Err(e) = ask_registry(env, &calls::EndSession { site, frame, fragment: name.to_string() }).await {
                    console_error!("__signout on {name}: the registry did not end the sessions ({:?}): {}", e.code, e.message);
                }
            }
            Ok(redirect(&base, &[set_cookie(SITE_COOKIE, "", &cookie_path, 0, secure(url)), frame_cookie("", &cookie_path, 0, secure(url))])?.with_status(303))
        }
        (_, m) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {rest}", m.as_ref()))),
    }
}
