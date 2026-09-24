//! Sign-in (phase 4 slice B), against the WorkOS fake: people are keyed by
//! their `(issuer, subject)`, browsers hold sessions (the platform's, then
//! one per fragment origin through a single-use redemption), a CLI key
//! joins a person through a browser approval, and a browser and the CLI
//! get the same answers from a fragment. A sign-in never sends a browser
//! off the platform, and its rows in the registry stay bounded and are
//! swept off the request path.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fragment_core::npub;
use fragment_nip98::Keys;
use fragment_proto::limits;
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{url_enc, Api, Call, Reply};
use crate::{Suite, SIGNINS_PENDING_MAX};

const CHAT_APP: &[u8] = include_bytes!("../../fixtures/chat.mjs");
const CHAT_JSON: &[u8] = include_bytes!("../../fixtures/chat.json");
const FORMAT_MJS: &[u8] = include_bytes!("../../fixtures/format.mjs");

pub(super) fn with_session(api: &Api, method: &str, path: &str, session: &str) -> Result<Reply> {
    api.call(Call { method, url: format!("{}{path}", api.base), cookie: Some(format!("fragment_session={session}")), ..Call::default() })
}

fn cookie_line(r: &Reply, name: &str) -> String {
    r.headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|c| c.starts_with(&format!("{name}="))).unwrap_or("").to_string()
}

/// A browser signed in on the platform walks to a fragment's origin:
/// `/auth/fragment` mints a redemption, `__signin` redeems it. Answers the
/// fragment's own session cookie value.
pub(super) fn site_cookie(api: &Api, session: &str, name: &str) -> Result<String> {
    let r = with_session(api, "GET", &format!("/auth/fragment?name={name}&return=/"), session)?;
    anyhow::ensure!(r.status == 302, "/auth/fragment: {r}");
    let r = api.call(Call { method: "GET", url: r.header("location"), ..Call::default() })?;
    anyhow::ensure!(r.status == 302, "__signin: {r}");
    r.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_site=").map(str::to_string)).context("a site cookie")
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
fn both(api: &Api, name: &str, method: &str, path: &str, body: Option<&Value>, cookie: Option<&str>, keys: Option<&Keys>) -> Result<(u16, u16)> {
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
    Ok((browser.status, cli.status))
}

pub fn signin(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("signin") {
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
    let r = api.call(Call { method: "GET", url: callback.clone(), cookie: Some(bound.clone()), ..Call::default() })?;
    let session_line = cookie_line(&r, "fragment_session");
    s.ok(
        "the callback exchanges the code and sets a session (HttpOnly, SameSite=Lax), back where it began",
        r.status == 302 && r.header("location").ends_with("/x") && session_line.contains("HttpOnly") && session_line.contains("SameSite=Lax"),
        &r,
    );
    let r = api.call(Call { method: "GET", url: callback, cookie: Some(bound), ..Call::default() })?;
    s.ok("the same callback again signs no one in", r.status != 302 && cookie_line(&r, "fragment_session").is_empty(), &r);
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
    let r = with_session(api, "GET", "/", &paul)?;
    s.ok("the platform says who is signed in", r.status == 200 && r.text.contains("paul@e2e.test"), &r);
    let keys = Keys::generate();
    let claimed = api.approve(&paul, &keys)?;
    let paul_id = claimed.body["id"].as_str().unwrap_or("").to_string();
    let again = api.sign_in("paul@e2e.test")?;
    let k2 = Keys::generate();
    let r = api.approve(&again, &k2)?;
    s.ok("signing in again is the same person", r.body["id"] == paul_id.as_str(), &r);
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

    // a CLI key: its link carries its own proof; approving it adds it at once
    let cli = Keys::generate();
    let cli_npub = npub::encode(cli.pubkey_hex());
    let link = api.approval_link(&cli, 0);
    let path = link.trim_start_matches(&api.base).to_string();
    let r = api.unsigned("GET", &path, None)?;
    s.ok(
        "the approval page sends a signed-out browser to sign in first, keeping the link",
        r.status == 302 && r.header("location").contains("/auth/login?return=") && r.header("location").contains("proof"),
        &r,
    );
    let r = with_session(api, "GET", &path, &paul)?;
    s.ok("signed in, it shows the key's ending to compare with the terminal", r.status == 200 && r.text.contains(&cli_npub[cli_npub.len() - 8..]), &r);
    let r = with_session(api, "GET", &format!("/cli?key={cli_npub}"), &paul)?;
    s.ok("a link without the key's proof is refused", r.status == 400, &r);
    let stale = api.approval_link(&cli, 11 * 60);
    let r = with_session(api, "GET", stale.trim_start_matches(&api.base), &paul)?;
    s.ok("and one whose proof is older than ten minutes", r.status == 400 && r.text.contains("fragment login"), &r);
    let forged = link.replace(&cli_npub, &npub::encode(Keys::generate().pubkey_hex()));
    let r = api.approve_link(&paul, &forged)?;
    s.ok("and a proof by another key than the one named", r.status == 400, &r);
    let r = api.approve_link(&"0".repeat(64), &link)?;
    s.ok("approving without a session is 401", r.status == 401, &r);
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/cli/approve", api.base),
        body: Some(format!("key={cli_npub}").into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_session={paul}")),
        extra: vec![("origin", "https://evil.example".into())],
        ..Call::default()
    })?;
    s.ok("a form posted from another site is refused", r.status == 403, &r);
    let r = api.approve_link(&paul, &api.approval_link(&k4, 0))?;
    s.ok("a key someone else holds cannot be approved", r.status == 409, &r);
    let r = api.signed(&cli, "GET", "/api/identities/me", None)?;
    s.ok("until approved, the key is no one's", r.status == 401, &r);
    let r = api.approve_link(&paul, &link)?;
    let me = api.signed(&cli, "GET", "/api/identities/me", None)?;
    s.ok("approved, the key is the person's at once: nothing waits to claim it", r.status == 200 && me.status == 200 && me.body["id"] == paul_id.as_str(), &me);
    let r = api.approve_link(&paul, &link)?;
    s.ok("approving it again changes nothing", r.status == 200, &r);
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
        s.commit(&c, &[("applib/format.mjs", Some(FORMAT_MJS)), ("site/index.html", Some(b"<p>inside</p>"))]);
        ship(s, &c, CHAT_APP, CHAT_JSON);
        api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
        api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", member.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    }
    let r = api.page(&f, "__signin?return=/", None)?;
    s.ok("a fragment's sign-in starts at the platform", r.status == 302 && r.header("location").starts_with(&format!("{}/auth/fragment?name={f}", api.base)), &r);
    let r = api.unsigned("GET", &format!("/auth/fragment?name={f}&return=/"), None)?;
    s.ok("which sends a signed-out browser to sign in, and back", r.status == 302 && r.header("location").contains("/auth/login?return="), &r);
    let r = with_session(api, "GET", &format!("/auth/fragment?name={f}&return=%2Fa%2520b"), &member_session)?;
    let redeem = r.header("location");
    s.ok("signed in, the platform hands the fragment a single-use redemption", r.status == 302 && redeem.starts_with(&api.site_url(&f, "__signin?token=")), &r);
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
    let mut escaped = vec![];
    for raw in ["/%20//evil.example", "/%09//evil.example", "/http:evil.example"] {
        let r = with_session(api, "GET", &format!("/auth/fragment?name={f}&return={raw}"), &member_session)?;
        let r = api.call(Call { method: "GET", url: r.header("location"), ..Call::default() })?;
        if r.status != 302 || !same_origin(&r.header("location"), &api.site_url(&f, "")) {
            escaped.push(format!("{raw} -> {r} {}", r.header("location")));
        }
    }
    s.ok("nor does a fragment origin's sign-in", escaped.is_empty(), format!("{escaped:?}"));

    // the browser and the CLI decide alike: public, link, members
    let member_f2 = site_cookie(api, &member_session, &f)?;
    let outsider_f = site_cookie(api, &outsider_session, &f)?;
    let mut mismatches = vec![];
    let mut decided = 0;
    for visibility in ["public", "link", "members"] {
        api.signed(&owner, "PUT", &format!("/api/f/{f}/visibility"), Some(&json!({ "visibility": visibility })))?;
        for (who, cookie, keys) in [("member", Some(member_f2.as_str()), Some(&member)), ("outsider", Some(outsider_f.as_str()), Some(&outsider)), ("nobody", None, None)] {
            let n = decided;
            let say = json!({ "id": format!("say-{visibility}-{who}-{n}"), "input": { "text": "hi" } });
            let count = json!({ "id": "c", "input": {} });
            let note = json!({ "id": format!("note-{visibility}-{who}-{n}"), "input": { "text": "n" } });
            for (method, path, body) in [("GET", "", None), ("POST", "__op/count", Some(&count)), ("POST", "__op/say", Some(&say)), ("POST", "__op/note", Some(&note))] {
                let (browser, cli) = both(api, &f, method, path, body, cookie, keys)?;
                decided += 1;
                // an anonymous call is rate limited per cookie, and a signed one is not: equal status is the check
                if browser != cli {
                    mismatches.push(format!("{visibility} {who} {method} /{path}: browser {browser}, CLI {cli}"));
                }
            }
        }
    }
    s.ok(&format!("the browser and the CLI decide alike ({decided} decisions across public, link, and members)"), mismatches.is_empty(), format!("{mismatches:?}"));
    api.signed(&owner, "PUT", &format!("/api/f/{f}/visibility"), Some(&json!({ "visibility": "members" })))?;

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
    let r = api.page(&g, "__signout", Some(&format!("fragment_site={newest}")))?;
    s.ok("__signout clears the fragment's cookie", r.status == 302 && cookie_line(&r, "fragment_site").contains("Max-Age=0"), &r);
    let after = reads(&newest)?;
    s.ok("and ends its session: a copy of the cookie is nobody (members only: 401)", after == 401, after);
    let (other, platform) = (reads(&on_g[1])?, with_session(api, "GET", "/", &member_session)?);
    s.ok("(the browser's other sessions, and its platform session, stay)", other == 200 && platform.text.contains("member@e2e.test"), other);
    let r = api.page(&g, "__signout", Some(&format!("fragment_site={newest}")))?;
    s.ok("signing out again is no error", r.status == 302, &r);

    // the registry down: the browser is signed out all the same
    let kept = site_cookie(api, &member_session, &g)?;
    api.unsigned("POST", "/api/test/registry", Some(&json!({ "down": true })))?;
    let r = api.page(&g, "__signout", Some(&format!("fragment_site={kept}")));
    api.unsigned("POST", "/api/test/registry", Some(&json!({ "down": false })))?;
    let r = r?;
    s.ok(
        "__signout clears the cookie even when the registry cannot answer",
        r.status == 302 && cookie_line(&r, "fragment_site").contains("Max-Age=0"),
        &r,
    );
    let copy = reads(&kept)?;
    s.ok("(the registry never heard: a copy of that cookie lasts until its session ends)", copy == 200, copy);
    api.page(&g, "__signout", Some(&format!("fragment_site={kept}")))?;

    // revoking: a member removed, a person signed out
    api.signed(&owner, "DELETE", &format!("/api/f/{f}/members/{}", member.pubkey_hex()), None)?;
    let r = api.page(&f, "", Some(&format!("fragment_site={member_f2}")))?;
    s.ok("a removed member's browser is refused on the next request", r.status == 403, &r);
    let member_on_g = site_cookie(api, &member_session, &g)?;
    let r = api.page(&g, "", Some(&format!("fragment_site={member_on_g}")))?;
    s.ok("(the member still reads the other fragment)", r.status == 200, &r);
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/auth/logout", api.base),
        cookie: Some(format!("fragment_session={member_session}")),
        extra: vec![("origin", api.base.clone())],
        ..Call::default()
    })?;
    let to = r.header("location");
    let (status, cleared) = (r.status, cookie_line(&r, "fragment_session"));
    let back = api.external(&to)?;
    s.ok(
        "signing out ends the session, and sends the browser to end WorkOS's",
        status == 302 && to.contains("/user_management/sessions/logout?session_id=") && cleared.contains("Max-Age=0") && back.status == 302 && s.workos.ended().len() == 1,
        format!("{status} {to} {cleared}"),
    );
    let r = api.page(&g, "", Some(&format!("fragment_site={member_on_g}")))?;
    s.ok("and every fragment session made from it", r.status == 401, &r);
    let r = with_session(api, "GET", "/", &member_session)?;
    s.ok("the platform no longer knows the browser", r.text.contains("Sign in") && !r.text.contains("member@e2e.test"), &r);

    // invites in the browser
    let r = api.signed(&owner, "POST", &format!("/api/f/{f}/invites"), Some(&json!({ "role": "viewer" })))?;
    let invite = r.body["token"].as_str().unwrap_or("").to_string();
    let r = api.page(&f, &format!("__join?invite={invite}"), None)?;
    s.ok("an invite link sends a signed-out browser to sign in first", r.status == 302 && r.header("location").contains("__signin?return="), &r);
    let r = api.page(&f, &format!("__join?invite={invite}"), Some(&format!("fragment_site={outsider_f}")))?;
    s.ok("signed in, it offers to join", r.status == 200 && r.text.contains("Join"), &r);
    let r = api.call(Call {
        method: "POST",
        url: api.site_url(&f, "__join"),
        body: Some(format!("invite={invite}").into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_site={outsider_f}")),
        ..Call::default()
    })?;
    let r2 = api.page(&f, "", Some(&format!("fragment_site={outsider_f}")))?;
    s.ok("joining in the browser makes them a member", r.status == 302 && r2.status == 200, &r2);

    // sign-in's rows: bounded, and swept on the registry's alarm, never on a request
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
    with_session(api, "GET", &format!("/auth/fragment?name={f}&return=/"), &paul)?;
    api.unsigned("GET", "/auth/login", None)?;
    let expired = signins(api, "expire")?;
    s.ok(
        "(the test hook expires every pending sign-in and unspent redemption)",
        expired["logins"].as_u64() >= Some(1) && expired["redemptions"].as_u64() >= Some(1),
        &expired,
    );
    api.unsigned("GET", "/auth/login", None)?;
    let r = signins(api, "count")?;
    s.ok(
        "starting a sign-in sweeps nothing: the expired rows wait for the alarm",
        r["logins"].as_u64() == expired["logins"].as_u64().map(|n| n + 1) && r["redemptions"] == expired["redemptions"],
        &r,
    );
    signins(api, "sweep")?;
    let mut last = Value::Null;
    let swept = s.eventually(Duration::from_secs(10), || {
        last = signins(api, "count").unwrap_or(Value::Null);
        last["logins"] == 1 && last["redemptions"] == 0
    });
    s.ok("the alarm's sweep takes every expired row, and keeps the live sign-in", swept, &last);
    s.ok("and every live session", last["sessions"] == expired["sessions"], &last);

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
    let planted = https("/", Some(format!("fragment_session={session}")))?;
    let hosted = https("/", Some(format!("__Host-fragment_session={session}")))?;
    s.ok(
        "over https the platform reads its session only from a __Host- cookie (a planted plain one is nobody)",
        planted.status == 200 && !planted.text.contains("Sign out") && hosted.status == 200 && hosted.text.contains("Sign out"),
        format!("{} / {}", planted.text.chars().take(120).collect::<String>(), hosted.text.chars().take(120).collect::<String>()),
    );
    let start = https("/auth/login?return=/", None)?;
    let cookie = start.headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|c| c.contains("fragment_login=")).unwrap_or("").to_string();
    s.ok("and sets them so: __Host-, the whole host, Secure", cookie.starts_with("__Host-fragment_login=") && cookie.contains("Path=/;") && cookie.contains("Secure"), &cookie);

    // who makes fragments: people, and agents for their owners
    let agent = Keys::generate();
    let reg = "/api/identities";
    let registered = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &owner) })))?;
    let r = api.create(&agent, &s.name("by-an-agent"))?;
    let owner_id = api.identity(&owner)?;
    let name = r.body["name"].as_str().unwrap_or("").to_string();
    let members = api.signed(&owner, "GET", &format!("/api/f/{name}/members"), None)?;
    s.ok(
        "an agent makes a fragment for its owner: theirs, under their username, with the agent an editor",
        r.status == 200
            && r.body["owner"] == owner_id.as_str()
            && name == api.qualified(&owner, &s.name("by-an-agent"))?
            && members.body["members"].as_array().is_some_and(|a| a.iter().any(|m| m["principal"] == registered.body["id"] && m["role"] == "editor")),
        format!("{r} {members}"),
    );
    Ok(())
}
