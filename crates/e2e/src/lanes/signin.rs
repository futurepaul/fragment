//! Sign-in (docs/api.md, Sign-in), against the WorkOS fake: people are keyed by
//! their `(issuer, subject)`, browsers hold sessions (the platform's, then
//! one per fragment origin through a single-use redemption), a CLI key
//! joins a person through a browser approval, and a browser and the CLI
//! get the same answers from a fragment. A sign-in never sends a browser
//! off the platform, and its rows in the registry stay bounded and are
//! swept off the request path. A site file answers alike for everyone who
//! may see the fragment, so a signed-in page view there costs the registry
//! nothing, and one it must vouch for fails closed when it cannot.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fragment_core::{cli_code, form, npub};
use fragment_nip98::Keys;
use fragment_proto::{limits, ErrorCode};
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{url_enc, Api, Call, Reply, Socket};
use crate::{Suite, SIGNINS_PENDING_MAX};

const CHAT_APP: &[u8] = include_bytes!("../../fixtures/chat.mjs");
const CHAT_JSON: &[u8] = include_bytes!("../../fixtures/chat.json");
const FORMAT_MJS: &[u8] = include_bytes!("../../fixtures/format.mjs");

pub(super) fn with_session(api: &Api, method: &str, path: &str, session: &str) -> Result<Reply> {
    api.call(Call { method, url: format!("{}{path}", api.base), cookie: Some(format!("fragment_session={session}")), ..Call::default() })
}

/// Who the platform says a browser's session is, as the shell asks it
/// (`GET /api/identities/me` with the session: lib.rs `shell_session`):
/// their identity, or 401's `Null`.
pub(super) fn who(api: &Api, session: &str) -> Result<Value> {
    api.who(session)
}

/// The shell's page, which `/` and `/settings` are for everyone.
fn is_shell(r: &Reply) -> bool {
    r.status == 200 && r.text.contains("/__shell/shell.js")
}

/// `/settings` is the shell's page for everyone, as `/` is: its script
/// asks who is signed in (`who`) and so asks the signed-out to sign in,
/// and shows someone new their first agent being made (the shell-ui
/// section shows both in a browser).
fn settings_cases(s: &mut Suite, api: &Api) -> Result<()> {
    let r = api.unsigned("GET", "/settings", None)?;
    s.ok("signed out, settings is the shell, which asks them to sign in (the platform knows no one)", is_shell(&r) && who(api, "f".repeat(64).as_str())?.is_null(), &r);
    let fresh = api.sign_in("home-fresh@e2e.test")?;
    let r = with_session(api, "GET", "/settings", &fresh)?;
    let asked = who(api, &fresh)?;
    s.ok("someone new's settings is the shell, which knows them: a person, by their email", is_shell(&r) && asked["kind"] == "person" && asked["email"] == "home-fresh@e2e.test", &r);
    Ok(())
}

/// A request as the shell's page sends it (`x-fragment-shell`, same-origin
/// Fetch Metadata) with the platform session, from `origin`.
fn from_shell(api: &Api, session: &str, method: &'static str, path: &str, body: Option<(Vec<u8>, &'static str)>, origin: &str, site: &str) -> Result<Reply> {
    let (body, content_type) = body.map_or((None, None), |(b, t)| (Some(b), Some(t)));
    api.call(Call {
        method,
        url: format!("{}{path}", api.base),
        body,
        content_type,
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("x-fragment-shell", "1".into()), ("sec-fetch-site", site.into()), ("origin", origin.to_string())],
        ..Call::default()
    })
}

/// An origin a fragment's page posts from: one site with the platform (a
/// SameSite=Lax session cookie rides along), but not its origin.
const A_FRAGMENTS_PAGE: &str = "http://page--k3x9.fragment.localhost";

/// Whether a page refuses every frame (`frame-ancestors 'none'`, and
/// `X-Frame-Options: DENY` for browsers without it). Every fragment's
/// origin is one site with the platform, so the platform's session rides
/// into a frame, and a page there could lay a button under a click.
pub(super) fn unframed(r: &Reply) -> bool {
    r.header("content-security-policy").contains("frame-ancestors 'none'") && r.header("x-frame-options") == "DENY"
}

fn framing(r: &Reply) -> String {
    format!("{} content-security-policy {:?}, x-frame-options {:?}", r.status, r.header("content-security-policy"), r.header("x-frame-options"))
}

fn cookie_line(r: &Reply, name: &str) -> String {
    r.headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|c| c.starts_with(&format!("{name}="))).unwrap_or("").to_string()
}

/// A browser signed in on the platform walks to a fragment's origin:
/// `/auth/fragment` mints a redemption (saying yes first, on a fragment
/// that is not the person's nor shared with them), `__signin` redeems it.
/// Answers the fragment's own session cookie value.
pub(super) fn site_cookie(api: &Api, session: &str, name: &str) -> Result<String> {
    let mut r = with_session(api, "GET", &format!("/auth/fragment?name={name}&return=/"), session)?;
    if r.status == 200 {
        r = consent(api, session, name, &api.base)?;
    }
    anyhow::ensure!(matches!(r.status, 302 | 303), "/auth/fragment: {r}");
    let r = api.call(Call { method: "GET", url: r.header("location"), ..Call::default() })?;
    anyhow::ensure!(r.status == 302, "__signin: {r}");
    r.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_site=").map(str::to_string)).context("a site cookie")
}

/// The yes to "Continue to <name>?", posted from `origin` with the page's
/// form token (made for `session`'s own cookie, as its page would hold it,
/// long enough ago that its button has armed).
pub(super) fn consent(api: &Api, session: &str, name: &str, origin: &str) -> Result<Reply> {
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
    let token = form::issue(session, &format!("consent:{name}"), now_ms - form::DELAY_MS - 50);
    api.call(Call {
        method: "POST",
        url: format!("{}/auth/fragment?name={name}&return=/", api.base),
        body: Some(format!("form={}", url_enc(&token)).into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("origin", origin.to_string())],
        ..Call::default()
    })
}

/// `__signout`, posted from the fragment's own page with its cookies.
pub(super) fn signout(api: &Api, name: &str, cookies: &str) -> Result<Reply> {
    api.call(Call { method: "POST", url: api.site_url(name, "__signout"), cookie: Some(cookies.to_string()), extra: vec![("origin", api.site_origin(name))], ..Call::default() })
}

/// A browser's sign-in that began at `/auth/login?return=<raw>` (`raw` as
/// the query carries it), finished: where the callback sends the browser.
fn signed_in_to(api: &Api, raw: &str) -> Result<String> {
    let r = api.unsigned("GET", &format!("/auth/login?return={raw}&login_hint=back@e2e.test"), None)?;
    anyhow::ensure!(r.status == 302, "/auth/login: {r}");
    let bound = r.cookies().into_iter().find(|c| c.starts_with("fragment_login=")).context("a login cookie")?;
    let back = api.external(&r.header("location"))?;
    let done = api.call(Call { method: "GET", url: back.header("location"), cookie: Some(bound), ..Call::default() })?;
    anyhow::ensure!(done.status == 302, "the callback: {done}");
    Ok(done.header("location"))
}

/// Whether `to` is on `base`'s origin (scheme, host, and port).
fn same_origin(to: &str, base: &str) -> bool {
    match (reqwest::Url::parse(to), reqwest::Url::parse(base)) {
        (Ok(to), Ok(base)) => to.origin() == base.origin(),
        _ => false,
    }
}

/// How many calls the registry has had since it started: a request's round
/// trips are the difference across it.
pub(super) fn registry_calls(api: &Api) -> Result<u64> {
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "calls": null })))?;
    r.body["calls"].as_u64().with_context(|| format!("the calls test hook: {r}"))
}

/// What `f` answered, and the registry calls it made.
pub(super) fn calls_of<T>(api: &Api, f: impl FnOnce() -> Result<T>) -> Result<(T, u64)> {
    let before = registry_calls(api)?;
    let out = f()?;
    Ok((out, registry_calls(api)? - before))
}

/// `f`, run while the registry answers 503 to everything (a test hook),
/// which it answers again after, whatever `f` did.
pub(super) fn registry_down<T>(api: &Api, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "down": true })))?;
    anyhow::ensure!(r.status == 200, "taking the registry down: {r}");
    let out = f();
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "down": false })))?;
    anyhow::ensure!(r.status == 200, "bringing the registry back: {r}");
    out
}

/// The registry's count of sign-in rows, after a test hook (`count`,
/// `expire`, or `sweep`).
fn signins(api: &Api, hook: &str) -> Result<Value> {
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "signins": hook })))?;
    anyhow::ensure!(r.status == 200 && r.body["logins"].is_u64(), "the signins test hook ({hook}): {r}");
    Ok(r.body)
}

/// A median of `n` signed requests' times, in milliseconds.
fn signed_median_ms(api: &Api, keys: &Keys, n: usize) -> Result<f64> {
    let mut times = vec![];
    for _ in 0..n {
        let t0 = Instant::now();
        let r = api.signed(keys, "GET", "/api/identities/me", None)?;
        anyhow::ensure!(r.status == 200, "a timed request: {r}");
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.total_cmp(b));
    Ok(times[n / 2])
}

/// What a browser with a fragment's session gets, and what the CLI signing
/// the same request gets.
fn both(api: &Api, name: &str, method: &str, path: &str, body: Option<&Value>, cookie: Option<&str>, keys: Option<&Keys>) -> Result<(Reply, Reply)> {
    let call = |cookie: Option<String>, keys: Option<&Keys>| {
        api.call(Call {
            method,
            url: api.site_url(name, path),
            body: body.map(|b| b.to_string().into_bytes()),
            content_type: body.map(|_| "application/json"),
            cookie,
            keys,
            ..Call::default()
        })
    };
    let browser = call(cookie.map(|c| format!("fragment_site={c}")), None)?;
    let cli = call(None, keys)?;
    Ok((browser, cli))
}

/// The platform session a sign-in's callback answered set, if it set one.
fn session_set(r: &Reply) -> Option<String> {
    r.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_session=").filter(|v| !v.is_empty()).map(str::to_string))
}

fn form_destination(r: &Reply) -> Result<String> {
    r.header("refresh").strip_prefix("0; url=").map(str::to_string).context("the form's page navigates to WorkOS")
}

/// `POST /auth/switch?return=<back>` from the platform's own page, with
/// the browser's cookies.
fn switch(api: &Api, back: &str, cookie: Option<String>) -> Result<Reply> {
    api.call(Call { method: "POST", url: format!("{}/auth/switch?return={}", api.base, url_enc(back)), cookie, extra: vec![("origin", api.base.clone())], ..Call::default() })
}

/// AuthKit keeps its own session in the browser (a cookie on its own
/// domain, which clearing the platform's site data leaves) and answers a
/// sign-in from it without asking anyone (Alex, on finite.chat, 2026-10-16:
/// "even when clearing browser storage, the sign in button auto logs me
/// in"). Signing out ends it at WorkOS with the session id WorkOS signed the
/// browser in with; "Use another account" (`/auth/switch`) ends it when the
/// platform knows no session of the browser's.
fn switch_cases(s: &mut Suite, api: &Api) -> Result<()> {
    let workos = s.workos.url.clone();
    let to_logout = |sid: &str| format!("{workos}/user_management/sessions/logout?session_id={sid}&return_to={}", url_enc(&format!("{}/", api.base)));
    let home = format!("{}/", api.base);

    // Sign in, with WorkOS holding a session in the browser: no one is asked
    let held = s.workos.hold("switch-a@e2e.test", 3600);
    let start = api.unsigned("GET", "/auth/login?return=/", None)?;
    let bound = start.cookies().into_iter().find(|c| c.starts_with("fragment_login="));
    let back = api.external(&start.header("location"))?;
    let done = api.call(Call { method: "GET", url: back.header("location"), cookie: bound, ..Call::default() })?;
    let again = session_set(&done).context("the sign-in WorkOS answered from its session sets one")?;
    let r = who(api, &again)?;
    s.ok("WorkOS answers a sign-in from the session it holds in the browser, asking no one: Sign in is that account again", back.status == 302 && r["email"] == "switch-a@e2e.test", &r);

    // Sign out: WorkOS's logout, with that session's id
    let r = api.call(Call { method: "POST", url: format!("{}/auth/logout", api.base), cookie: Some(format!("fragment_session={again}")), extra: vec![("origin", api.base.clone())], ..Call::default() })?;
    let to = form_destination(&r)?;
    let out = api.external(&to)?;
    s.ok(
        "signing out sends the browser to WorkOS's logout with the session WorkOS signed it in with, which ends it there, and back to the platform",
        r.status == 200 && to == to_logout(&held) && out.status == 302 && out.header("location") == home && s.workos.held().is_none() && s.workos.ended().contains(&held),
        format!("{} {to} / {}", r.status, out.header("location")),
    );
    let start = api.unsigned("GET", "/auth/login?return=/", None)?;
    let asks = api.external(&start.header("location"))?;
    s.ok("then a sign-in asks who is signing in (WorkOS's form)", asks.status == 200 && asks.text.contains("sign in as"), &asks);

    // Use another account, from a browser the platform knows no session of
    // (its site data cleared) while WorkOS still holds one
    let held = s.workos.hold("switch-a@e2e.test", 3600);
    let r = switch(api, "/settings", None)?;
    let to = form_destination(&r)?;
    let bound = r.cookies().into_iter().find(|c| c.starts_with("fragment_switch="));
    let line = cookie_line(&r, "fragment_switch");
    s.ok(
        "Use another account starts a sign-in at WorkOS, its state bound to the browser by a cookie of its own (HttpOnly, SameSite=Lax)",
        r.status == 200 && to.contains("/user_management/authorize?") && !to.contains("login_hint") && line.contains("HttpOnly") && line.contains("SameSite=Lax") && cookie_line(&r, "fragment_login").is_empty(),
        &r,
    );
    let back = api.external(&to)?;
    let done = api.call(Call { method: "GET", url: back.header("location"), cookie: bound, ..Call::default() })?;
    let ends = done.header("location");
    s.ok(
        "WorkOS answering it from the session it holds, it signs no one in, and sends the browser to end that session at WorkOS",
        done.status == 302 && ends == to_logout(&held) && session_set(&done).is_none() && cookie_line(&done, "fragment_switch").contains("Max-Age=0"),
        &done,
    );
    let out = api.external(&ends)?;
    s.ok("which ends it, and comes back to the platform", out.status == 302 && out.header("location") == home && s.workos.held().is_none(), &out);

    // with none held, WorkOS asks, and whoever signs in is signed in
    let r = switch(api, "/settings", None)?;
    let bound = r.cookies().into_iter().find(|c| c.starts_with("fragment_switch="));
    let asks = api.external(&form_destination(&r)?)?;
    let back = api.external(&format!("{}&login_hint=switch-b@e2e.test", form_destination(&r)?))?;
    let done = api.call(Call { method: "GET", url: back.header("location"), cookie: bound, ..Call::default() })?;
    let b = session_set(&done).context("a switch someone signed in to sets a session")?;
    let r = who(api, &b)?;
    s.ok(
        "with no session held, WorkOS asks who is signing in, and whoever signs in is signed in, back where they began",
        asks.status == 200 && asks.text.contains("sign in as") && done.status == 302 && done.header("location").ends_with("/settings") && r["email"] == "switch-b@e2e.test",
        &r,
    );

    // a switch from a browser signed in here ends that session too
    let r = switch(api, "/", Some(format!("fragment_session={b}")))?;
    let after = who(api, &b)?;
    s.ok("a switch from a browser signed in here ends that session too", r.status == 200 && cookie_line(&r, "fragment_session").contains("Max-Age=0") && after.is_null(), &r);
    let r = api.call(Call { method: "POST", url: format!("{}/auth/switch?return=/", api.base), extra: vec![("origin", "https://evil.example".into())], ..Call::default() })?;
    s.ok("and a switch posted from another site is refused (403)", r.status == 403 && cookie_line(&r, "fragment_switch").is_empty(), &r);
    Ok(())
}

pub fn signin(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("signin", &[crate::Need::Fakes, crate::Need::Deployment, crate::Need::Levers, crate::Need::Chrome]) {
        return Ok(());
    }

    // the round trip
    let r = api.unsigned("GET", "/auth/login?return=/x", None)?;
    let to = r.header("location");
    s.ok(
        "sign-in starts at WorkOS: this environment, a callback here, and a state",
        r.status == 302 && to.contains("/user_management/authorize?") && to.contains("client_id=client_fragment_e2e") && to.contains(&url_enc(&format!("{}/auth/callback", api.base))) && to.contains("state="),
        &r,
    );
    let login = cookie_line(&r, "fragment_login");
    s.ok("the state is bound to this browser by a cookie (HttpOnly, SameSite=Lax)", login.contains("HttpOnly") && login.contains("SameSite=Lax") && login.contains("Path=/;"), &login);
    let bound = r.cookies().into_iter().find(|c| c.starts_with("fragment_login=")).unwrap_or_default();
    let back = api.external(&format!("{to}&login_hint=round@e2e.test"))?;
    let callback = back.header("location");
    let r = api.call(Call { method: "GET", url: callback.clone(), ..Call::default() })?;
    s.ok("a callback without the browser's state cookie is refused", r.status == 400 && cookie_line(&r, "fragment_session").is_empty(), &r);
    let r = api.call(Call { method: "GET", url: callback.clone(), cookie: Some("fragment_login=".to_string() + &"0".repeat(64)), ..Call::default() })?;
    s.ok("and with another sign-in's state", r.status == 400, &r);
    let state = reqwest::Url::parse(&to).ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "state").map(|(_, v)| v.into_owned())).unwrap_or_default();
    let r = api.call(Call { method: "GET", url: callback.clone(), cookie: Some(format!("fragment_own_key={state}")), ..Call::default() })?;
    s.ok(
        "and with its state in another kind of round trip's cookie (an own key's): each kind's cookie binds that kind alone",
        fragment_core::roundtrip::valid_state(&state) && r.status == 400 && cookie_line(&r, "fragment_session").is_empty(),
        &r,
    );
    let r = api.call(Call { method: "GET", url: callback.clone(), cookie: Some(bound.clone()), ..Call::default() })?;
    let session_line = cookie_line(&r, "fragment_session");
    s.ok(
        "the callback exchanges the code and sets a session (HttpOnly, SameSite=Lax), back where it began",
        r.status == 302 && r.header("location").ends_with("/x") && session_line.contains("HttpOnly") && session_line.contains("SameSite=Lax"),
        &r,
    );
    let r = api.call(Call { method: "GET", url: callback, cookie: Some(bound), ..Call::default() })?;
    // refused as a spent sign-in (400), never as a fault: a 500 signs no one in either
    s.ok(
        "the same callback again signs no one in (400: that sign-in was used)",
        r.status == 400 && r.code() == Some(ErrorCode::InvalidRequest) && cookie_line(&r, "fragment_session").is_empty(),
        &r,
    );
    let r = api.unsigned("GET", "/auth/login?invitation_token=Z1uX3Rbw_cIl-5fIG", None)?;
    s.ok("an invitation's token rides along to WorkOS (it lets its invitee sign up)", r.header("location").contains("&invitation_token=Z1uX3Rbw_cIl-5fIG"), &r);
    let r = api.unsigned("GET", "/auth/login?invitation_token=%22%3E%3Cscript%3E", None)?;
    s.ok("and a malformed one does not", r.status == 302 && !r.header("location").contains("invitation_token"), &r);
    s.workos.fail_next("access_denied");
    let r = api.unsigned("GET", "/auth/login", None)?;
    let back = api.external(&format!("{}&login_hint=denied@e2e.test", r.header("location")))?;
    let r = api.call(Call { method: "GET", url: back.header("location"), cookie: r.cookies().into_iter().find(|c| c.starts_with("fragment_login=")), ..Call::default() })?;
    s.ok("a refusal at WorkOS is shown, and signs no one in", r.status == 400 && r.text.contains("access_denied"), &r);

    // the way back never leaves the platform (audit R1: the first three once
    // landed on https://evil.example/ after a real sign-in)
    let mut escaped = vec![];
    for raw in ["/%20//evil.example", "/+//evil.example", "/%09//evil.example", "/%0D%0A//evil.example", "%2F%2Fevil.example", "/%5Cevil.example", "/http:evil.example"] {
        let to = signed_in_to(api, raw)?;
        if !same_origin(&to, &api.base) {
            escaped.push(format!("{raw} -> {to}"));
        }
    }
    s.ok("a sign-in's return never sends the browser to another origin", escaped.is_empty(), format!("{escaped:?}"));
    let to = signed_in_to(api, "/%20//evil.example")?;
    s.ok("(a return it will not keep comes back to the platform's root)", to == format!("{}/", api.base), &to);

    // who a person is: their (issuer, subject), never their email
    let paul = api.sign_in("paul@e2e.test")?;
    let r = who(api, &paul)?;
    s.ok("the platform says who is signed in", r.to_string().contains("paul@e2e.test"), &r);
    let keys = Keys::generate();
    let claimed = api.approve(&paul, &keys)?;
    let paul_id = claimed.body["id"].as_str().unwrap_or("").to_string();
    let again = api.sign_in("paul@e2e.test")?;
    let k2 = Keys::generate();
    let r = api.approve(&again, &k2)?;
    s.ok("signing in again is the same person", r.body["id"] == paul_id.as_str(), &r);
    let (r, calls) = calls_of(api, || who(api, &paul))?;
    s.ok("the shell asks the registry once who is signed in, and gets their email with it", r.to_string().contains("paul@e2e.test") && calls == 1, format!("{calls} calls: {r}"));
    // someone new is someone at once: nothing to choose before the shell
    let chooser = api.sign_in("chooser@e2e.test")?;
    let asked = who(api, &chooser)?;
    let chooser_id = asked["id"].as_str().unwrap_or("").to_string();
    s.ok(
        "a person new to the platform is someone at once: named by their own key, shown by their email",
        asked["kind"] == "person" && fragment_core::npub::is_identity(&chooser_id) && asked["email"] == "chooser@e2e.test",
        &asked,
    );
    let home = with_session(api, "GET", "/", &chooser)?;
    s.ok("home is the shell", is_shell(&home), &home);
    let settings = with_session(api, "GET", "/settings", &chooser)?;
    let theirs = from_shell(api, &chooser, "GET", "/api/fragments", None, &api.base, "same-origin")?;
    let script = api.unsigned("GET", "/__shell/shell.js", None)?;
    s.ok(
        "their settings are the shell's: it knows who they are, that they have no fragments yet, and hands a coding agent this platform's llms.txt (the install folded beside it)",
        is_shell(&settings)
            && who(api, &chooser)?["email"] == "chooser@e2e.test"
            && theirs.status == 200
            && theirs.body["fragments"].as_array().is_some_and(|f| f.is_empty())
            && script.text.contains("Build with Claude Code or Codex")
            && script.text.contains("/llms.txt and help me build: ")
            && script.text.contains("releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz")
            && script.text.contains("fragment login --host ${location.origin}")
            && script.text.contains("fragment skill > ~/.claude/skills/fragment/SKILL.md"),
        format!("{settings} / {theirs}"),
    );
    settings_cases(s, api)?;
    let newcomer = api.sign_in("newcomer@e2e.test")?;
    // a picture, as the shell sets it (the API, with the platform session)
    let picture = |origin: &str, site: &str| {
        let png = b"\x89PNG\r\n\x1a\n-a-tiny-picture".to_vec();
        from_shell(api, &chooser, "PUT", "/api/identities/me/picture", Some((png, "image/png")), origin, site)
    };
    let r = picture(A_FRAGMENTS_PAGE, "same-site")?;
    let unset = api.unsigned("GET", &format!("/api/identities/{chooser_id}/picture"), None)?;
    s.ok("so is a picture: a fragment's page is not the shell, and its session is no one's there (401)", r.status == 401 && unset.status == 404, format!("{r} / {unset}"));
    let r = picture(&api.base, "same-origin")?;
    let shown = api.unsigned("GET", &format!("/api/identities/{chooser_id}/picture"), None)?;
    s.ok("(from the platform's own page, the shell, the picture is set)", r.status == 200 && shown.status == 200, format!("{r} / {shown}"));
    let user = s.workos.user("paul@e2e.test");
    s.workos.set_email(&user.id, "paul@renamed.test");
    let renamed = api.sign_in("paul@renamed.test")?;
    let k3 = Keys::generate();
    let r = api.approve(&renamed, &k3)?;
    let me = api.signed(&k3, "GET", "/api/identities/me", None)?;
    s.ok("an email change keeps the person, and the new email shows", r.body["id"] == paul_id.as_str() && me.body["subjects"][0]["email"] == "paul@renamed.test", &me);
    let other = api.sign_in("paul@renamed.test.example")?;
    let k4 = Keys::generate();
    let r = api.approve(&other, &k4)?;
    s.ok("another WorkOS user is another person, whatever their email looks like", r.status == 200 && r.body["id"] != paul_id.as_str(), &r);
    // an email names one person (decision 45), and WorkOS must have verified it
    let twin = s.workos.user("paul-twin@e2e.test");
    let start = api.unsigned("GET", "/auth/login?return=/&login_hint=paul-twin%40e2e.test", None)?;
    let bound = start.cookies().into_iter().find(|c| c.starts_with("fragment_login="));
    let back = api.external(&start.header("location"))?;
    s.workos.set_email(&twin.id, "paul@renamed.test");
    let r = api.call(Call { method: "GET", url: back.header("location"), cookie: bound, ..Call::default() })?;
    let page = |r: &Reply, says: &str| r.header("content-type").starts_with("text/html") && String::from_utf8_lossy(&r.bytes).contains(says);
    s.ok("another WorkOS user with a person's email is refused, never merged into them (409, on a page)", r.status == 409 && page(&r, "another account"), &r);
    s.workos.set_email(&twin.id, "paul-twin@e2e.test");
    s.workos.set_verified(&twin.id, false);
    let r = api.workos_callback("paul-twin@e2e.test")?;
    s.ok("an email WorkOS has not verified signs no one in (403, on a page)", r.status == 403 && page(&r, "not verified"), &r);
    s.workos.set_verified(&twin.id, true);
    let r = api.workos_callback("paul-twin@e2e.test")?;
    s.ok("verified, it signs in: a new person", r.status == 302, &r);

    // linking a second sign-in: explicit, from a signed-in session
    let r = with_session(api, "GET", "/auth/link", &renamed)?;
    let bound = r.cookies().into_iter().find(|c| c.starts_with("fragment_login="));
    let back = api.external(&format!("{}&login_hint=paul-work@e2e.test", r.header("location")))?;
    let r = api.call(Call { method: "GET", url: back.header("location"), cookie: bound, ..Call::default() })?;
    s.ok("linking a second sign-in finishes with a session", r.status == 302, &r);
    let me = api.signed(&k3, "GET", "/api/identities/me", None)?;
    s.ok("the person now has two sign-ins", me.body["subjects"].as_array().map_or(0, |a| a.len()) == 2, &me);
    let work = api.sign_in("paul-work@e2e.test")?;
    let k5 = Keys::generate();
    let r = api.approve(&work, &k5)?;
    s.ok("and either one signs in as them", r.body["id"] == paul_id.as_str(), &r);
    let r = with_session(api, "GET", "/auth/link", &renamed)?;
    let bound = r.cookies().into_iter().find(|c| c.starts_with("fragment_login="));
    let back = api.external(&format!("{}&login_hint=paul@renamed.test.example", r.header("location")))?;
    let r = api.call(Call { method: "GET", url: back.header("location"), cookie: bound, ..Call::default() })?;
    s.ok("a sign-in that is someone else's cannot be linked", r.status == 409, &r);

    switch_cases(s, api)?;

    // Goal: a CLI's key joins only the person who typed its code, from the
    // terminal it printed in, at a page no link fills in (the typed device
    // code: Paul, 2026-10-17; audit M6). Method: the code asked for by the
    // key, the page read and posted as a browser would (its tokens from the
    // pages, its buttons waited out); then each way around it: no
    // signature, an old one, a key someone holds, a link, the old approval
    // form, too soon, another session, another origin, a wrong code, a
    // replay, an expired code (with the sweep, below), and too many misses.
    let cli = Keys::generate();
    let cli_npub = npub::encode(cli.pubkey_hex());
    let tail = cli_npub[cli_npub.len() - 8..].to_string();
    let cli_email = "cli-person@e2e.test";
    let person = api.sign_in(cli_email)?;
    let person_id = who(api, &person)?["id"].as_str().unwrap_or("").to_string();
    let begun = api.cli_begin(&cli)?;
    let code = begun.body["code"].as_str().unwrap_or("").to_string();
    let kept = cli_code::normalize(&code).unwrap_or_default();
    s.ok(
        "a new key asks for a code, signed by itself: eight letters shown as two fours, the plain /cli page to type it at, good for ten minutes",
        begun.status == 200
            && code.len() == cli_code::LEN + 1
            && cli_code::valid(&kept)
            && cli_code::display(&kept) == code
            && begun.body["url"] == format!("{}/cli", api.base)
            && begun.body["expiresAt"].as_i64().is_some_and(|t| (t - crate::api::now_ms() - cli_code::TTL_MS).abs() < 60_000),
        &begun,
    );
    let r = api.unsigned("POST", "/cli/begin", Some(&json!({})))?;
    s.ok("unsigned, it gets none (401)", r.status == 401 && r.body["code"].is_null(), &r);
    let late = cli.header("POST", &format!("{}/cli/begin", api.base), b"{}", crate::api::now_s() - 120);
    let r = api.call(Call { method: "POST", url: format!("{}/cli/begin", api.base), body: Some(b"{}".to_vec()), content_type: Some("application/json"), extra: vec![("authorization", late)], ..Call::default() })?;
    s.ok("nor signed two minutes ago: a signature's usual window holds (401)", r.status == 401 && r.body["code"].is_null(), &r);
    let r = api.cli_begin(&k4)?;
    s.ok("nor does a key someone holds (409)", r.status == 409 && r.body["code"].is_null(), &r);

    let r = api.unsigned("GET", "/cli", None)?;
    s.ok("the page sends a signed-out browser to sign in first, and back to /cli", r.status == 302 && r.header("location").ends_with(&format!("/auth/login?return={}", url_enc("/cli"))), &r);
    let page = with_session(api, "GET", "/cli", &person)?;
    s.ok(
        "signed in, it asks for the code: one field, a button that arms after a moment, and nothing of any key",
        page.status == 200 && page.text.contains("name=\"code\"") && page.text.contains("data-arm disabled") && !page.text.contains(&tail) && !page.text.contains("Add this key"),
        &page,
    );
    s.ok("and no page may frame it: a fragment's page could lay its button under a click", unframed(&page), framing(&page));
    let linked = with_session(api, "GET", &format!("/cli?key={cli_npub}&code={}&proof=x", url_enc(&code)), &person)?;
    s.ok(
        "no link fills it in: a key and a code in its address show the same empty page",
        linked.status == 200 && !linked.text.contains(&tail) && !linked.text.contains(&code) && !linked.text.contains(&kept) && !linked.text.contains("Add this key"),
        &linked,
    );
    let r = api.cli_form(&person, "/cli/approve", &[("key", cli_npub.as_str()), ("proof", "x")])?;
    s.ok("and the old approval's form adds nothing: a key and a proof are no code (403)", r.status == 403, &r);
    let r = api.signed(&cli, "GET", "/api/identities/me", None)?;
    s.ok("until its code is typed and pressed, the key is no one's", r.status == 401, &r);

    // the page as a browser uses it: its own tokens, its buttons waited out
    let armed = || std::thread::sleep(Duration::from_millis(form::DELAY_MS as u64 + 100));
    armed();
    let confirm = api.cli_form(&person, "/cli", &[("form", &super::share::form_of(&page)), ("code", &code.to_lowercase())])?;
    s.ok(
        "typed (in any case), it shows whom the CLI would act as, by their email, the key's ending to compare with the terminal, where and when its code was asked for and by what, and never to type a code someone sent; then asks a press",
        confirm.status == 200
            && confirm.text.contains(cli_email)
            && confirm.text.contains(&tail)
            && confirm.text.contains("Its code was asked just now from ")
            && confirm.text.contains(&format!("by {}", crate::api::CLI_AGENT))
            && confirm.text.contains(cli_code::NEVER_SENT)
            && confirm.text.contains("Add this key")
            && confirm.text.contains("data-arm disabled")
            && unframed(&confirm),
        &confirm,
    );
    let r = api.signed(&cli, "GET", "/api/identities/me", None)?;
    s.ok("seeing whom it would act as adds nothing yet", r.status == 401, &r);
    let pressed = super::share::form_of(&confirm);
    let press = |session: &str, origin: &str| {
        let fields = format!("form={}&code={}", url_enc(&pressed), url_enc(&kept));
        api.call(Call {
            method: "POST",
            url: format!("{}/cli/approve", api.base),
            body: Some(fields.into_bytes()),
            content_type: Some("application/x-www-form-urlencoded"),
            cookie: Some(format!("fragment_session={session}")),
            extra: vec![("origin", origin.to_string())],
            ..Call::default()
        })
    };
    let r = press(&person, &api.base)?;
    s.ok("pressed sooner than its button arms, it is refused (403): the click that opened the page cannot add the key", r.status == 403, &r);
    armed();
    let r = press(&paul, &api.base)?;
    s.ok("another person's session cannot press it (403: the page's form is not theirs)", r.status == 403, &r);
    let r = press(&person, A_FRAGMENTS_PAGE)?;
    s.ok("nor can a page on another origin (403)", r.status == 403, &r);
    let r = press(&"0".repeat(64), &api.base)?;
    s.ok("nor a browser signed in to no one (403: no session's form)", r.status == 403, &r);
    let r = api.signed(&cli, "GET", "/api/identities/me", None)?;
    s.ok("(none of those added it)", r.status == 401, &r);
    let mails_before = s.mail.sent_to(cli_email).len();
    let (added_page, calls) = calls_of(api, || press(&person, &api.base))?;
    let me = api.signed(&cli, "GET", "/api/identities/me", None)?;
    s.ok(
        "pressed, the key is the person's at once, and says so by their email: nothing waits to claim it",
        added_page.status == 200 && added_page.text.contains("Key added") && me.status == 200 && me.body["id"] == person_id.as_str() && me.body["email"] == cli_email,
        &me,
    );
    s.ok("(adding asks the registry once)", calls == 1, calls);
    // sent after the answer (wait_until), never in its way
    let mailed = s.eventually(Duration::from_secs(10), || s.mail.sent_to(cli_email).len() == mails_before + 1);
    let mail = s.mail.sent_to(cli_email);
    s.ok(
        "and the person is mailed that a key was added: its ending, the whole key, that its code was typed, where and when it was asked for, never to type one someone sent, and how to revoke it",
        mailed
            && mail.len() == mails_before + 1
            && mail.last().is_some_and(|m| {
                m.subject.contains(&tail)
                    && m.text.contains("added when its code was typed at")
                    && m.text.contains("The code was asked ")
                    && m.text.contains(&format!("by {}", crate::api::CLI_AGENT))
                    && m.text.contains(cli_code::NEVER_SENT)
                    && m.text.contains(&format!("fragment keys revoke {cli_npub}"))
            }),
        format!("{mail:?}"),
    );
    let again = press(&person, &api.base)?;
    s.ok("the same code again adds nothing: a code is spent once (400, the page asks again)", again.status == 400 && again.text.contains("is waiting on that code") && again.text.contains("name=\"code\""), &again);
    let r = api.cli_begin(&cli)?;
    s.ok("and a key added asks for no code again (409)", r.status == 409, &r);

    // a wrong code, typed: a miss, counted; past the bound, the person waits
    let other = Keys::generate();
    let waiting = api.cli_begin(&other)?.body["code"].as_str().unwrap_or("").to_string();
    let wrong = if waiting.starts_with('B') { "CCCC-CCCC" } else { "BBBB-BBBB" };
    let typed = api.type_code(&person, wrong)?;
    s.ok("a wrong code finds no key: the page says so and asks again (400)", typed.status == 400 && typed.text.contains("is waiting on that code") && !typed.text.contains("Add this key"), &typed);
    let typed = api.type_code(&person, "not a code")?;
    s.ok("and so does what is no code at all", typed.status == 400 && typed.text.contains("is waiting on that code"), &typed);
    // the replay above and these two were misses already
    let missed_before = 3;
    let mut statuses = vec![];
    for _ in 0..cli_code::MISSES_MAX {
        statuses.push(api.type_code(&person, wrong)?.status);
    }
    let late = api.type_code(&person, &waiting)?;
    let wrong_then = statuses.iter().take_while(|st| **st == 400).count();
    s.ok(
        &format!("past {} wrong codes in ten minutes, the person waits: even a right code is refused then (429)", cli_code::MISSES_MAX),
        missed_before + wrong_then == cli_code::MISSES_MAX as usize && statuses[wrong_then..].iter().all(|st| *st == 429) && late.status == 429,
        format!("{statuses:?} then {late}"),
    );
    let r = api.signed(&other, "GET", "/api/identities/me", None)?;
    s.ok("(and adds nothing)", r.status == 401, &r);
    let (typed, added) = (api.type_code(&paul, &waiting)?, api.add_code(&paul, &waiting)?);
    let theirs = api.signed(&other, "GET", "/api/identities/me", None)?;
    s.ok(
        "another person's misses are theirs alone: whoever types a code adds its key to themselves, and their terminal says whose",
        typed.status == 200 && added.status == 200 && theirs.status == 200 && theirs.body["id"] == paul_id.as_str(),
        &theirs,
    );
    // a key keeps its newest few codes: one more pushes out its oldest, so
    // no flood of one key's begins pushes out anyone else's
    let many = Keys::generate();
    let many_tail = npub::encode(many.pubkey_hex())[npub::encode(many.pubkey_hex()).len() - 8..].to_string();
    let mut codes = vec![];
    for _ in 0..=cli_code::CODES_PER_KEY_MAX {
        codes.push(api.cli_begin(&many)?.body["code"].as_str().unwrap_or("").to_string());
    }
    let (oldest, newest) = (api.type_code(&paul, &codes[0])?, api.type_code(&paul, &codes[cli_code::CODES_PER_KEY_MAX])?);
    s.ok(
        &format!("a key keeps its newest {} codes: one more pushes out its oldest (400), and the newest still names it", cli_code::CODES_PER_KEY_MAX),
        codes.iter().all(|c| cli_code::normalize(c).is_some()) && oldest.status == 400 && newest.status == 200 && newest.text.contains(&many_tail),
        json!({ "oldest": oldest.to_string(), "newest": newest.status }),
    );
    // every platform page refuses frames; its redirects need not (a
    // redirect shows nothing)
    let pages = [
        ("the shell's settings", "/__shell/shell.js", with_session(api, "GET", "/settings", &paul)?),
        ("the shell, for someone new", "/__shell/shell.js", with_session(api, "GET", "/", &newcomer)?),
        ("the shell, signed out", "/__shell/shell.js", api.unsigned("GET", "/", None)?),
        ("the sign-out button", "Sign out", with_session(api, "GET", "/auth/logout", &paul)?),
        ("the CLI code page", "name=\"code\"", with_session(api, "GET", "/cli", &paul)?),
        ("a CLI key to confirm", "Add this key", confirm),
        ("a key added", "Key added", added_page),
    ];
    let framable: Vec<String> = pages.iter().filter(|(_, shows, r)| !(r.text.contains(shows) && unframed(r))).map(|(page, _, r)| format!("{page}: {}", framing(r))).collect();
    s.ok(&format!("and so does every other platform page ({} of them)", pages.len()), framable.is_empty(), format!("{framable:?}"));
    // a page that opens one of them in a window of its own keeps no hold on it
    let held: Vec<&str> = pages.iter().filter(|(_, _, r)| r.header("cross-origin-opener-policy") != "same-origin").map(|(page, _, _)| *page).collect();
    s.ok("and every one severs a window that opened it (Cross-Origin-Opener-Policy: same-origin)", held.is_empty(), format!("{held:?}"));
    // a person who signs in may hold no key at all
    let lone = api.sign_in("lone@e2e.test")?;
    let lone_key = Keys::generate();
    api.approve(&lone, &lone_key)?;
    let r = api.signed(&lone_key, "DELETE", &format!("/api/identities/me/keys/{}", lone_key.pubkey_hex()), None)?;
    s.ok("a person who signs in may revoke their last key", r.status == 200, &r);
    let r = api.signed(&lone_key, "GET", "/api/fragments", None)?;
    s.ok("which is then refused", r.status == 401, &r);

    // fragment origins: a session of their own, for that fragment only
    let owner = api.person()?;
    let member = Keys::generate();
    let member_session = api.sign_in("member@e2e.test")?;
    api.approve(&member_session, &member)?;
    let outsider = Keys::generate();
    let outsider_session = api.sign_in("outsider@e2e.test")?;
    api.approve(&outsider_session, &outsider)?;
    let (f, g) = (s.named(api, &owner, "signin-f")?, s.named(api, &owner, "signin-g")?);
    for name in [&f, &g] {
        let c = s.create(api, &owner, name)?;
        s.commit(
            &c,
            &[
                ("applib/format.mjs", Some(FORMAT_MJS)),
                ("site/index.html", Some(b"<p>inside</p><script src=app.js></script>")),
                ("site/app.js", Some(b"console.log(1)")),
                ("site/style.css", Some(b"p{margin:0}")),
            ],
        );
        ship(s, &c, CHAT_APP, CHAT_JSON);
        api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
        api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", member.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    }
    let r = api.page(&f, "__signin?return=/", None)?;
    s.ok("a fragment's sign-in starts at the platform", r.status == 302 && r.header("location").starts_with(&format!("{}/auth/fragment?name={f}", api.base)), &r);
    let r = api.unsigned("GET", &format!("/auth/fragment?name={f}&return=/"), None)?;
    s.ok("which sends a signed-out browser to sign in, and back", r.status == 302 && r.header("location").contains("/auth/login?return="), &r);
    let (r, calls) = calls_of(api, || with_session(api, "GET", &format!("/auth/fragment?name={f}&return=%2Fa%2520b"), &member_session))?;
    let redeem = r.header("location");
    s.ok("signed in, the platform hands the fragment a single-use redemption", r.status == 302 && redeem.starts_with(&api.site_url(&f, "__signin?token=")), &r);
    s.ok("(asking the registry twice for a member: whether they said yes to it, then, once the fragment says they are in it, the redemption)", calls == 2, calls);
    let other_host = redeem.replace(&api.site_url(&f, ""), &api.site_url(&g, ""));
    let r = api.call(Call { method: "GET", url: other_host, ..Call::default() })?;
    s.ok("a redemption for one fragment is refused on another", r.status == 401, &r);
    let r = api.call(Call { method: "GET", url: redeem.clone(), ..Call::default() })?;
    s.ok("(it was refused without being spent: it is for this fragment)", r.status == 302, &r);
    let site_line = cookie_line(&r, "fragment_site");
    s.ok(
        "redeemed on its fragment, it sets that origin's own cookie (HttpOnly, host-only) and returns",
        site_line.contains("HttpOnly") && site_line.contains("Path=/") && !site_line.to_ascii_lowercase().contains("domain=") && r.header("location").ends_with("/a%20b"),
        &site_line,
    );
    let member_f = r.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_site=").map(str::to_string)).unwrap_or_default();
    let r = api.call(Call { method: "GET", url: redeem, ..Call::default() })?;
    s.ok("a redemption works once", r.status == 401, &r);
    let r = api.page(&f, "", Some(&format!("fragment_site={member_f}")))?;
    s.ok("the member's browser reads the members-only fragment", r.status == 200 && r.text.contains("inside"), &r);
    let r = api.page(&g, "", Some(&format!("fragment_site={member_f}")))?;
    s.ok("the same cookie on another fragment's origin is nobody (members only: 401)", r.status == 401, &r);

    // A socket has no CORS, and every fragment's origin is one site with
    // the others: a page on g's origin (its author's code, or an agent's)
    // opens one to f's, and the member's f cookie rides along. Only the
    // upgrade's Origin says whose page asks.
    let on_f = format!("fragment_site={member_f}");
    let as_member = format!("opened as {} (viewer)", api.identity(&member)?);
    let opened = |socket: Result<Socket>| match socket.and_then(|mut socket| socket.until("hello", 5)) {
        Ok(hello) => format!("opened as {} ({})", hello["principal"].as_str().unwrap_or("?"), hello["role"].as_str().unwrap_or("?")),
        Err(e) => format!("refused: {e:#}"),
    };
    let from_g = opened(Socket::on_host(api, &f, "__live", None, Some(&on_f), Some(&api.site_origin(&g))));
    s.ok("a socket to f from g's page, with the member's f cookie, is refused (403): it would read f as them", from_g.contains("403"), &from_g);
    let watch_from_g = match Socket::on_host(api, &f, "__watch", None, Some(&on_f), Some(&api.site_origin(&g))).and_then(|mut w| w.until("hello", 5)) {
        Ok(hello) => format!("opened: {hello}"),
        Err(e) => format!("refused: {e:#}"),
    };
    s.ok("and so is its change feed (__watch)", watch_from_g.contains("403"), &watch_from_g);
    let from_null = opened(Socket::on_host(api, &f, "__live", None, Some(&on_f), Some("null")));
    s.ok("and one from a page whose origin is hidden (null)", from_null.contains("403"), &from_null);
    let from_f = opened(Socket::on_host(api, &f, "__live", None, Some(&on_f), Some(&api.site_origin(&f))));
    s.ok("from f's own page, the same socket is the member's", from_f == as_member, &from_f);
    let unnamed = opened(Socket::on_host(api, &f, "__live", None, Some(&on_f), None));
    s.ok("an upgrade that names no page is no browser's: its cookie is nobody (members only: 401)", unnamed.contains("401"), &unnamed);
    let cli = opened(Socket::open(api, &f, "__live", Some(&member), None));
    s.ok("the CLI's socket names no page and signs: it is the member's", cli == as_member, &cli);
    let mut escaped = vec![];
    for raw in ["/%20//evil.example", "/%09//evil.example", "/http:evil.example"] {
        let r = with_session(api, "GET", &format!("/auth/fragment?name={f}&return={raw}"), &member_session)?;
        let r = api.call(Call { method: "GET", url: r.header("location"), ..Call::default() })?;
        if r.status != 302 || !same_origin(&r.header("location"), &api.site_url(&f, "")) {
            escaped.push(format!("{raw} -> {r} {}", r.header("location")));
        }
    }
    s.ok("nor does a fragment origin's sign-in", escaped.is_empty(), format!("{escaped:?}"));

    // the browser and the CLI decide alike, and as the model says (docs/MODEL.md,
    // Visibility): public, link, members; a page, a file, the file list, an app
    // route (which sees who is asking), operations
    let member_f2 = site_cookie(api, &member_session, &f)?;
    let outsider_f = site_cookie(api, &outsider_session, &f)?;
    // Each asker's answer on each route, in the order `paths` names them: a
    // page, its HEAD, a file, the file list, an app route, a public query, a
    // public mutation, and a mutation that needs an editor (the member is a
    // viewer). Whoever may see the fragment gets 200; short of it, a signed
    // asker is 403 and an anonymous one 401. A public fragment is everyone's
    // to see; a share link counts as a viewer on a link or public fragment,
    // and for nothing on a members one.
    const SEES_SIGNED: [u16; 8] = [200, 200, 200, 200, 200, 200, 200, 403];
    const SEES_ANON: [u16; 8] = [200, 200, 200, 200, 200, 200, 200, 401];
    let expected = [
        ("public", "member", SEES_SIGNED),
        ("public", "outsider", SEES_SIGNED),
        ("public", "outsider with the link", SEES_SIGNED),
        ("public", "nobody", SEES_ANON),
        ("public", "nobody with the link", SEES_ANON),
        ("link", "member", SEES_SIGNED),
        ("link", "outsider", [403; 8]),
        ("link", "outsider with the link", SEES_SIGNED),
        ("link", "nobody", [401; 8]),
        ("link", "nobody with the link", SEES_ANON),
        ("members", "member", SEES_SIGNED),
        ("members", "outsider", [403; 8]),
        ("members", "outsider with the link", [403; 8]),
        ("members", "nobody", [401; 8]),
        ("members", "nobody with the link", [401; 8]),
    ];
    let mut mismatches = vec![];
    let mut wrong = vec![];
    let mut decided = 0;
    // the share link as it is now: opened again from members, a new one
    let link_of = |f: &str| -> Result<String> { Ok(format!("?view={}", api.status(&owner, f)?.body["viewToken"].as_str().unwrap_or(""))) };
    for visibility in ["public", "link", "members"] {
        api.signed(&owner, "PUT", &format!("/api/f/{f}/visibility"), Some(&json!({ "visibility": visibility })))?;
        let f_view = link_of(&f)?;
        let askers = [
            ("member", Some(member_f2.as_str()), Some(&member), ""),
            ("outsider", Some(outsider_f.as_str()), Some(&outsider), ""),
            ("outsider with the link", Some(outsider_f.as_str()), Some(&outsider), f_view.as_str()),
            ("nobody", None, None, ""),
            ("nobody with the link", None, None, f_view.as_str()),
        ];
        for (who, cookie, keys, view) in askers {
            let want = expected.iter().find(|(v, w, _)| *v == visibility && *w == who).map(|(.., statuses)| *statuses).context("an expected row per asker")?;
            // an operation id holds no spaces: an asker's name in one is dashed
            let (n, tag) = (decided, who.replace(' ', "-"));
            let say = json!({ "id": format!("say-{visibility}-{tag}-{n}"), "input": { "text": "hi" } });
            let count = json!({ "id": "c", "input": {} });
            let note = json!({ "id": format!("note-{visibility}-{tag}-{n}"), "input": { "text": "n" } });
            let paths = [
                ("GET", "", None),
                ("HEAD", "", None),
                ("GET", "app.js", None),
                ("GET", "__files", None),
                ("GET", "hello", None),
                ("POST", "__op/count", Some(&count)),
                ("POST", "__op/say", Some(&say)),
                ("POST", "__op/note", Some(&note)),
            ];
            for ((method, path, body), want) in paths.into_iter().zip(want) {
                let (browser, cli) = both(api, &f, method, &format!("{path}{view}"), body, cookie, keys)?;
                decided += 1;
                // an anonymous call is rate limited per cookie, and a signed one is not: equal status is the check;
                // the app's route says whom it saw, and as what
                let saw_alike = path != "hello" || browser.text == cli.text;
                if browser.status != cli.status || !saw_alike {
                    mismatches.push(format!("{visibility} {who} {method} /{path}: browser {browser}, CLI {cli}"));
                }
                // a wrong answer both give alike is still wrong
                if browser.status != want || cli.status != want {
                    wrong.push(format!("{visibility} {who} {method} /{path}: want {want}; browser {browser}, CLI {cli}"));
                }
            }
        }
    }
    s.ok(&format!("the browser and the CLI decide alike ({decided} decisions across public, link, and members)"), mismatches.is_empty(), format!("{mismatches:?}"));
    s.ok(&format!("and each of the {decided} is the model's answer for its visibility, asker, and route"), wrong.is_empty(), format!("{wrong:?}"));
    api.signed(&owner, "PUT", &format!("/api/f/{f}/visibility"), Some(&json!({ "visibility": "public" })))?;
    let member_id = api.identity(&member)?;
    let (browser, cli) = both(api, &f, "GET", "hello", None, Some(&member_f2), Some(&member))?;
    s.ok(
        "an app's route on a public fragment sees the signed-in person, not a visitor",
        browser.text.contains(&member_id) && browser.text.contains("as viewer") && cli.text == browser.text,
        format!("{browser} / {cli}"),
    );

    // what a page view costs the registry: a file answers alike for everyone
    // who may see the fragment, so the registry is asked only where it decides
    let page = ["", "app.js", "style.css", "__fragment.js", "__fragment.css"];
    let mut costs = vec![];
    for visibility in ["public", "link", "members"] {
        api.signed(&owner, "PUT", &format!("/api/f/{f}/visibility"), Some(&json!({ "visibility": visibility })))?;
        let view = if visibility == "link" { link_of(&f)? } else { String::new() };
        let view = view.as_str();
        let viewed = |cookie: Option<String>, keys: Option<&Keys>| {
            calls_of(api, || {
                for path in page {
                    let r = api.call(Call { method: "GET", url: api.site_url(&f, &format!("{path}{view}")), cookie: cookie.clone(), keys, ..Call::default() })?;
                    anyhow::ensure!(r.status == 200, "{visibility} {path}: {r}");
                }
                Ok(())
            })
            .map(|(_, calls)| calls)
        };
        costs.push((visibility, viewed(Some(format!("fragment_site={member_f2}")), None)?, viewed(None, Some(&member))?));
    }
    println!("      registry calls for a page view of {} requests, (visibility, browser, CLI): {costs:?}", page.len());
    s.ok(
        "a signed-in page view on a public or shared fragment asks the registry nothing",
        costs[0].1 == 0 && costs[0].2 == 0 && costs[1].1 == 0 && costs[1].2 == 0,
        format!("{costs:?}"),
    );
    s.ok("on a members-only fragment, once a request: membership decides", costs[2].1 == page.len() as u64 && costs[2].2 == page.len() as u64, format!("{costs:?}"));
    api.signed(&owner, "PUT", &format!("/api/f/{f}/visibility"), Some(&json!({ "visibility": "public" })))?;
    let op = json!({ "id": "c", "input": {} });
    let (_, calls) = calls_of(api, || {
        let r = api.browser_op(&f, "count", "c", json!({}), Some(&format!("fragment_site={member_f2}")))?;
        anyhow::ensure!(r.status == 200, "an operation: {r}");
        let (browser, _) = both(api, &f, "GET", "hello", None, Some(&member_f2), None)?;
        anyhow::ensure!(browser.status == 200, "an app route: {browser}");
        Ok(())
    })?;
    s.ok("an operation and an app route, which see who is asking, ask it once each", calls == 2, calls);

    // the registry down: what needs no one is served, and what it must vouch for is 503
    let public = registry_down(api, || {
        let mut got = vec![];
        for (label, method, path, body) in [("a page", "GET", "", None), ("a file", "GET", "app.js", None), ("an app route", "GET", "hello", None), ("an operation", "POST", "__op/count", Some(&op))] {
            let (browser, cli) = both(api, &f, method, path, body, Some(&member_f2), Some(&member))?;
            got.push((label, browser.status, cli.status));
        }
        Ok(got)
    })?;
    s.ok(
        "registry down, a public fragment's page and files are still served to a signed-in browser and the CLI",
        public[..2].iter().all(|(_, b, c)| *b == 200 && *c == 200),
        format!("{public:?}"),
    );
    s.ok("and its app route and operations, which see who asks, are 503 (never anonymous)", public[2..].iter().all(|(_, b, c)| *b == 503 && *c == 503), format!("{public:?}"));
    api.signed(&owner, "PUT", &format!("/api/f/{f}/visibility"), Some(&json!({ "visibility": "members" })))?;
    let members = registry_down(api, || {
        let (browser, cli) = both(api, &f, "GET", "app.js", None, Some(&member_f2), Some(&member))?;
        let nobody = api.page(&f, "app.js", None)?;
        Ok((browser.status, cli.status, nobody.status))
    })?;
    s.ok("a members-only fragment's file is 503 to a member while the registry cannot vouch for them", members.0 == 503 && members.1 == 503, format!("{members:?}"));
    s.ok("(and 401 to nobody: there is no one to ask about)", members.2 == 401, format!("{members:?}"));

    // a browser's sessions on a fragment: its newest few, and signing out ends one
    let mut on_g = vec![];
    for _ in 0..=limits::SITE_SESSIONS_PER_FRAGMENT_MAX {
        on_g.push(site_cookie(api, &member_session, &g)?);
    }
    let reads = |cookie: &str| api.page(&g, "", Some(&format!("fragment_site={cookie}"))).map(|r| r.status);
    let (oldest, newest) = (reads(&on_g[0])?, reads(&on_g[on_g.len() - 1])?);
    s.ok(
        &format!("a browser keeps its newest {} sessions on a fragment: the one before them ends (401)", limits::SITE_SESSIONS_PER_FRAGMENT_MAX),
        oldest == 401 && newest == 200 && reads(&on_g[1])? == 200,
        format!("oldest {oldest}, newest {newest}"),
    );
    let newest = on_g[on_g.len() - 1].clone();
    let r = signout(api, &g, &format!("fragment_site={newest}"))?;
    s.ok(
        "__signout, posted from the fragment's own page, clears both its cookies",
        r.status == 303 && cookie_line(&r, "fragment_site").contains("Max-Age=0") && cookie_line(&r, "fragment_frame").contains("Max-Age=0"),
        &r,
    );
    let after = reads(&newest)?;
    s.ok("and ends its session: a copy of the cookie is nobody (members only: 401)", after == 401, after);
    let (other, platform) = (reads(&on_g[1])?, who(api, &member_session)?);
    s.ok("(the browser's other sessions, and its platform session, stay)", other == 200 && platform.to_string().contains("member@e2e.test"), other);
    let r = signout(api, &g, &format!("fragment_site={newest}"))?;
    s.ok("signing out again is no error", r.status == 303, &r);

    // the registry down: the browser is signed out all the same
    let kept = site_cookie(api, &member_session, &g)?;
    api.unsigned("POST", "/api/test/registry", Some(&json!({ "down": true })))?;
    let r = signout(api, &g, &format!("fragment_site={kept}"));
    api.unsigned("POST", "/api/test/registry", Some(&json!({ "down": false })))?;
    let r = r?;
    s.ok(
        "__signout clears the cookie even when the registry cannot answer",
        r.status == 303 && cookie_line(&r, "fragment_site").contains("Max-Age=0"),
        &r,
    );
    let copy = reads(&kept)?;
    s.ok("(the registry never heard: a copy of that cookie lasts until its session ends)", copy == 200, copy);
    signout(api, &g, &format!("fragment_site={kept}"))?;

    // revoking: a member removed, a person signed out
    api.signed(&owner, "DELETE", &format!("/api/f/{f}/members/{}", member.pubkey_hex()), None)?;
    let r = api.page(&f, "", Some(&format!("fragment_site={member_f2}")))?;
    s.ok("a removed member's browser is refused on the next request", r.status == 403, &r);
    let member_on_g = site_cookie(api, &member_session, &g)?;
    let r = api.page(&g, "", Some(&format!("fragment_site={member_on_g}")))?;
    s.ok("(the member still reads the other fragment)", r.status == 200, &r);
    let ended = s.workos.ended().len();
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/auth/logout", api.base),
        cookie: Some(format!("fragment_session={member_session}")),
        extra: vec![("origin", A_FRAGMENTS_PAGE.into())],
        ..Call::default()
    })?;
    s.ok("another origin cannot sign the browser out", r.status == 403 && !who(api, &member_session)?.is_null(), &r);
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/auth/logout", api.base),
        cookie: Some(format!("fragment_session={member_session}")),
        extra: vec![("origin", api.base.clone())],
        ..Call::default()
    })?;
    let to = form_destination(&r)?;
    let (status, cleared) = (r.status, cookie_line(&r, "fragment_session"));
    let back = api.external(&to)?;
    s.ok(
        "signing out ends the session, and a non-cacheable, unframed page sends the browser to end WorkOS's",
        status == 200
            && r.header("cache-control") == "no-store"
            && unframed(&r)
            && r.header("referrer-policy") == "no-referrer"
            && to.contains("/user_management/sessions/logout?session_id=")
            && r.text.contains(">Continue</a>")
            && cleared.contains("Max-Age=0")
            && back.status == 302
            && s.workos.ended().len() == ended + 1,
        format!("{status} {to} {cleared}"),
    );
    let r = api.page(&g, "", Some(&format!("fragment_site={member_on_g}")))?;
    s.ok("and every fragment session made from it", r.status == 401, &r);
    let r = who(api, &member_session)?;
    s.ok("the platform no longer knows the browser", r.is_null(), &r);
    for (label, cookie) in [("repeated", Some(format!("fragment_session={member_session}"))), ("already signed out", None)] {
        let r = api.call(Call { method: "POST", url: format!("{}/auth/logout", api.base), cookie, extra: vec![("origin", api.base.clone())], ..Call::default() })?;
        s.ok(
            &format!("logout {label} clears the cookie and returns home"),
            r.status == 302 && r.header("location") == "/" && cookie_line(&r, "fragment_session").contains("Max-Age=0"),
            &r,
        );
    }
    browser_logout(s, api)?;

    // sign-in's rows: bounded, and swept on the registry's alarm, never on a request
    // (paul is not in f: he says yes to it once, and its redemptions follow)
    let r = consent(api, &paul, &f, &api.base)?;
    anyhow::ensure!(r.status == 303, "paul's yes to {f}: {r}");
    let mut minted = vec![];
    for _ in 0..=limits::REDEMPTIONS_PER_SESSION_MAX {
        minted.push(with_session(api, "GET", &format!("/auth/fragment?name={f}&return=/"), &paul)?.header("location"));
    }
    let (oldest, newest) = (
        api.call(Call { method: "GET", url: minted[0].clone(), ..Call::default() })?,
        api.call(Call { method: "GET", url: minted[minted.len() - 1].clone(), ..Call::default() })?,
    );
    s.ok(
        &format!("a platform session keeps its newest {} unspent redemptions: the one before them is refused", limits::REDEMPTIONS_PER_SESSION_MAX),
        oldest.status == 401 && newest.status == 302,
        format!("oldest {oldest}, newest {newest}"),
    );
    // a redemption, and a sign-in back from WorkOS, each left unfinished
    let late_redeem = with_session(api, "GET", &format!("/auth/fragment?name={f}&return=/"), &paul)?.header("location");
    let late = api.unsigned("GET", "/auth/login?login_hint=late@e2e.test", None)?;
    let late_cookie = late.cookies().into_iter().find(|c| c.starts_with("fragment_login=")).context("a login cookie")?;
    let late_back = api.external(&late.header("location"))?;
    // and a CLI's code, left untyped
    let untyped_key = Keys::generate();
    let untyped = api.cli_begin(&untyped_key)?.body["code"].as_str().unwrap_or("").to_string();
    let expired = signins(api, "expire")?;
    s.ok(
        "(the test hook expires every pending sign-in, unspent redemption, CLI code, and person's wrong codes)",
        expired["logins"].as_u64() >= Some(1) && expired["redemptions"].as_u64() >= Some(1) && expired["cli_codes"].as_u64() >= Some(1) && expired["cli_misses"].as_u64() >= Some(1),
        &expired,
    );
    let r = api.type_code(&paul, &untyped)?;
    let still = api.signed(&untyped_key, "GET", "/api/identities/me", None)?;
    s.ok("a CLI code past its ten minutes finds no key, swept or not (400), and adds none", r.status == 400 && r.text.contains("is waiting on that code") && still.status == 401, &r);
    api.unsigned("GET", "/auth/login", None)?;
    let r = signins(api, "count")?;
    s.ok(
        "starting a sign-in sweeps nothing: the expired rows wait for the alarm",
        r["logins"].as_u64() == expired["logins"].as_u64().map(|n| n + 1) && r["redemptions"] == expired["redemptions"],
        &r,
    );
    // expired rows the sweep has not reached yet are refused all the same
    let r = api.call(Call { method: "GET", url: late_redeem, ..Call::default() })?;
    s.ok("a redemption past its time is refused (401), unspent or not", r.status == 401 && r.code() == Some(ErrorCode::Unauthenticated), &r);
    let r = api.call(Call { method: "GET", url: late_back.header("location"), cookie: Some(late_cookie), ..Call::default() })?;
    s.ok(
        "and so is finishing a sign-in past its time (400: start again)",
        r.status == 400 && r.code() == Some(ErrorCode::InvalidRequest) && cookie_line(&r, "fragment_session").is_empty(),
        &r,
    );
    signins(api, "sweep")?;
    let mut last = Value::Null;
    let swept = s.eventually(Duration::from_secs(10), || {
        last = signins(api, "count").unwrap_or(Value::Null);
        last["logins"] == 1 && last["redemptions"] == 0 && last["cli_codes"] == 0 && last["cli_misses"] == 1
    });
    // paul's miss on the expired code is new, so it stays; the others go
    s.ok("the alarm's sweep takes every expired row (CLI codes and wrong codes past their window too), and keeps the live sign-in and a fresh miss", swept, &last);
    s.ok("and every live session", last["sessions"] == expired["sessions"], &last);

    // Goal: a session past its time is nobody, whether the sweep has taken
    // its row or not. Method: a test hook moves one session's expiry to now.
    // A site session ends alone; a platform session takes every site
    // session made from it (they live only while it does).
    let again = api.sign_in("member@e2e.test")?;
    let (one, two) = (site_cookie(api, &again, &g)?, site_cookie(api, &again, &g)?);
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "signins": { "expireSession": one } })))?;
    anyhow::ensure!(r.status == 200, "expiring a site session: {r}");
    let (ended, kept, platform) = (reads(&one)?, reads(&two)?, who(api, &again)?);
    s.ok(
        "a site session past its time is nobody (members only: 401); the browser's others, and its platform session, stay",
        ended == 401 && kept == 200 && platform.to_string().contains("member@e2e.test"),
        format!("{ended} {kept}"),
    );
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "signins": { "expireSession": again } })))?;
    anyhow::ensure!(r.status == 200, "expiring a platform session: {r}");
    let (kept, platform) = (reads(&two)?, who(api, &again)?);
    s.ok(
        "a platform session past its time is nobody, and so is every site session made from it",
        kept == 401 && platform.is_null(),
        kept,
    );

    let before_ms = signed_median_ms(api, &keys, 21)?;
    let first = api.unsigned("GET", "/auth/login?login_hint=oldest@e2e.test", None)?;
    let first_cookie = first.cookies().into_iter().find(|c| c.starts_with("fragment_login=")).context("a login cookie")?;
    let first_back = api.external(&first.header("location"))?;
    let t0 = Instant::now();
    // the fleet's FRAGMENT_SIGNINS_PENDING_MAX, set small (crate::SIGNINS_PENDING_MAX)
    for _ in 0..SIGNINS_PENDING_MAX {
        let r = api.unsigned("GET", "/auth/login", None)?;
        anyhow::ensure!(r.status == 302, "/auth/login: {r}");
    }
    let began_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let after_ms = signed_median_ms(api, &keys, 21)?;
    println!("      {SIGNINS_PENDING_MAX} sign-ins began in {began_ms:.0} ms; a signed request: {before_ms:.1} ms median before, {after_ms:.1} ms after");
    let r = signins(api, "count")?;
    s.ok(&format!("pending sign-ins stay at the fleet's cap ({SIGNINS_PENDING_MAX})"), r["logins"].as_u64() == Some(SIGNINS_PENDING_MAX), &r);
    let r = api.call(Call { method: "GET", url: first_back.header("location"), cookie: Some(first_cookie), ..Call::default() })?;
    s.ok("past the cap, the oldest sign-in went first: finishing it is refused", r.status == 400 && r.message().contains("start again"), &r);
    let r = signed_in_to(api, "/after-the-flood")?;
    s.ok("and the newest finishes", r == format!("{}/after-the-flood", api.base), &r);

    // over https (a proxy in front says so), sessions are __Host- cookies,
    // which no fragment's page can set for the whole domain: a plain one,
    // as a page could plant it, is nobody's
    let https = |path: &str, cookie: Option<String>| {
        api.call(Call { method: "GET", url: format!("{}{path}", api.base), cookie, extra: vec![("x-forwarded-proto", "https".into())], ..Call::default() })
    };
    let session = api.sign_in("host-prefix@e2e.test")?;
    // the shell's question, who is signed in, over https
    let me = |cookie: String| {
        api.call(Call {
            method: "GET",
            url: format!("{}/api/identities/me", api.base),
            cookie: Some(cookie),
            extra: vec![("x-forwarded-proto", "https".into()), ("x-fragment-shell", "1".into()), ("sec-fetch-site", "same-origin".into())],
            ..Call::default()
        })
    };
    let planted = me(format!("fragment_session={session}"))?;
    let hosted = me(format!("__Host-fragment_session={session}"))?;
    s.ok(
        "over https the platform reads its session only from a __Host- cookie (a planted plain one is nobody)",
        planted.status == 401 && hosted.status == 200 && hosted.body["kind"] == "person",
        format!("{} / {}", planted.status, hosted.status),
    );
    let start = https("/auth/login?return=/", None)?;
    let cookie = start.headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|c| c.contains("fragment_login=")).unwrap_or("").to_string();
    s.ok("and sets them so: __Host-, the whole host, Secure", cookie.starts_with("__Host-fragment_login=") && cookie.contains("Path=/;") && cookie.contains("Secure"), &cookie);

    // who makes fragments: people, and agents for their owners
    let agent = Keys::generate();
    let reg = "/api/identities";
    let registered = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &owner) })))?;
    let label = s.name("by-an-agent");
    let r = api.create_with(&agent, json!({ "label": label }))?;
    let owner_id = api.identity(&owner)?;
    let name = r.body["name"].as_str().unwrap_or("").to_string();
    let members = api.signed(&owner, "GET", &format!("/api/f/{name}/members"), None)?;
    s.ok(
        "an agent makes a fragment for its owner: theirs, named from its label, with the agent an editor",
        r.status == 200
            && r.body["owner"] == owner_id.as_str()
            && fragment_proto::split_fragment_name(&name).is_some_and(|(l, _)| l == label)
            && members.body["members"].as_array().is_some_and(|a| a.iter().any(|m| m["principal"] == registered.body["id"] && m["role"] == "editor")),
        format!("{r} {members}"),
    );

    // Goal: an address asks for at most the fleet's CLI codes a minute
    // (FRAGMENT_CLI_BEGINS_PER_IP_MINUTE: crate::CLI_BEGINS_PER_IP_MINUTE
    // here), counted apart from every other address. Method: one address's
    // begins until it is refused; within one minute that is at the limit,
    // across a minute's turn at most twice it; another address is not
    // refused. Last in the section: nothing after it here asks for a code
    // from those addresses.
    let flood = Keys::generate();
    let limit = crate::CLI_BEGINS_PER_IP_MINUTE as usize;
    let mut statuses = vec![];
    // bounded: two minutes' worth at most
    while statuses.len() <= 2 * limit && statuses.last() != Some(&429) {
        statuses.push(api.cli_begin_from(&flood, Some("203.0.113.7"))?.status);
    }
    let other = api.cli_begin_from(&flood, Some("203.0.113.8"))?;
    let first_refused = statuses.iter().position(|st| *st == 429);
    s.ok(
        &format!("an address asks for at most {limit} CLI codes a minute (429 past them), counted apart from any other address"),
        first_refused.is_some_and(|i| (limit..=2 * limit).contains(&i)) && statuses.iter().all(|st| *st == 200 || *st == 429) && other.status == 200,
        format!("refused first at {first_refused:?} of {} begins; another address: {other}", statuses.len()),
    );
    Ok(())
}

/// Chromium applies a form's CSP to HTTP redirects too. Drive the shell's
/// real button so a successful POST alone cannot hide a blocked WorkOS logout.
fn browser_logout(s: &mut Suite, api: &Api) -> Result<()> {
    let Some(mut browser) = s.browser()? else {
        s.ok("Chrome is installed for sign-out (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let emails = ["logout-first@e2e.test", "logout-second@e2e.test"];
    let mut sessions = Vec::new();
    for email in emails {
        let session = api.sign_in(email)?;
        let person = who(api, &session)?;
        let id = person["id"].as_str().context("a signed-in person")?;
        let r = api.signed(&s.operator, "POST", &format!("/api/ledger/{id}/plan"), Some(&json!({ "id": email, "plan": "guest" })))?;
        anyhow::ensure!(r.status == 200, "making a guest: {r}");
        sessions.push(session);
    }
    let wait = Duration::from_secs(15);
    let policy = api.unsigned("GET", "/welcome", None)?.header("content-security-policy");
    s.ok("the shell still restricts forms to its own origin", policy.split(';').any(|p| p.trim() == "form-action 'self'"), &policy);
    browser.set_cookie(&format!("{}/", api.base), "fragment_session", &sessions[0])?;
    let page = browser.open(&format!("{}/welcome", api.base))?;
    let form = "form[action='/auth/logout'] button";
    anyhow::ensure!(
        browser.until(&page, &format!("document.querySelector({}) && document.body.innerText.includes({})", json!(form), json!(emails[0])), wait),
        "the first account's welcome loads"
    );
    let ended = s.workos.ended().len();
    browser.click(&page, form)?;
    let signed_out = browser.until(&page, "document.querySelector('#first-run-card a.primary')?.textContent === 'Sign in'", wait);
    s.ok(
        "the shell's Sign out button reaches WorkOS and returns signed out under its CSP",
        signed_out && s.workos.ended().len() == ended + 1,
        format!("returned: {signed_out}, provider logouts: {}", s.workos.ended().len() - ended),
    );
    s.ok(
        "the browser's platform cookie is cleared and its old session is revoked",
        !browser.cookies()?.iter().any(|c| c["name"] == "fragment_session") && who(api, &sessions[0])?.is_null(),
        "",
    );
    if !signed_out {
        return Ok(());
    }
    let held = s.workos.hold(emails[0], 3600);
    browser.eval(&page, "window.__beforeSwitch = true")?;
    browser.click(&page, "form[action^='/auth/switch'] button")?;
    let switched_out = browser.until(
        &page,
        "!window.__beforeSwitch && document.querySelector('#first-run-card a.primary')?.textContent === 'Sign in'",
        wait,
    );
    s.ok(
        "Use another account reaches WorkOS under the shell's CSP and ends its held session",
        switched_out && s.workos.held().is_none() && s.workos.ended().contains(&held) && !browser.cookies()?.iter().any(|c| c["name"] == "fragment_session"),
        format!("returned: {switched_out}"),
    );
    if !switched_out {
        return Ok(());
    }
    browser.click(&page, "#first-run-card a.primary")?;
    anyhow::ensure!(browser.until(&page, "!!document.querySelector('input[name=login_hint]')", wait), "WorkOS asks who to sign in as");
    browser.eval(&page, &format!("document.querySelector('input[name=login_hint]').value = {}; true", json!(emails[1])))?;
    browser.click(&page, "button")?;
    let switched = browser.until(&page, &format!("location.pathname === '/welcome' && document.body.innerText.includes({})", json!(emails[1])), wait);
    s.ok("the same browser can sign in as a different email after signing out", switched, "");
    browser.close(page)?;
    Ok(())
}
