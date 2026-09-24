//! Sign-in (phase 4 slice B), against the WorkOS fake: people are keyed by
//! their `(issuer, subject)`, browsers hold sessions (the platform's, then
//! one per fragment origin through a single-use redemption), a CLI key
//! joins a person through a browser approval, and a browser and the CLI
//! get the same answers from a fragment.

use anyhow::{Context, Result};
use fragment_core::npub;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{url_enc, Api, Call, Reply};
use crate::Suite;

const CHAT_APP: &[u8] = include_bytes!("../../fixtures/chat.mjs");
const CHAT_JSON: &[u8] = include_bytes!("../../fixtures/chat.json");
const FORMAT_MJS: &[u8] = include_bytes!("../../fixtures/format.mjs");

fn with_session(api: &Api, method: &str, path: &str, session: &str) -> Result<Reply> {
    api.call(Call { method, url: format!("{}{path}", api.base), cookie: Some(format!("fragment_session={session}")), ..Call::default() })
}

fn cookie_line(r: &Reply, name: &str) -> String {
    r.headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|c| c.starts_with(&format!("{name}="))).unwrap_or("").to_string()
}

/// A browser signed in on the platform walks to a fragment's origin:
/// `/auth/fragment` mints a redemption, `__signin` redeems it. Answers the
/// fragment's own session cookie value.
fn site_cookie(api: &Api, session: &str, name: &str) -> Result<String> {
    let r = with_session(api, "GET", &format!("/auth/fragment?name={name}&return=/"), session)?;
    anyhow::ensure!(r.status == 302, "/auth/fragment: {r}");
    let r = api.call(Call { method: "GET", url: r.header("location"), ..Call::default() })?;
    anyhow::ensure!(r.status == 302, "__signin: {r}");
    r.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_site=").map(str::to_string)).context("a site cookie")
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
    s.ok("the state is bound to this browser by a cookie (HttpOnly, SameSite=Lax)", login.contains("HttpOnly") && login.contains("SameSite=Lax") && login.contains("Path=/auth"), &login);
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
    let (f, g) = (s.name("signin-f"), s.name("signin-g"));
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
    let r = with_session(api, "GET", &format!("/auth/fragment?name={f}&return=/a%20b"), &member_session)?;
    let redeem = r.header("location");
    s.ok("signed in, the platform hands the fragment a single-use redemption", r.status == 302 && redeem.starts_with(&api.site_url(&f, "__signin?token=")), &r);
    let other_host = redeem.replace(&format!("{f}."), &format!("{g}."));
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

    // who makes fragments: people
    let agent = Keys::generate();
    let reg = "/api/identities";
    api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &owner) })))?;
    let r = api.create(&agent, &s.name("by-an-agent"))?;
    s.ok("an agent cannot make a fragment: people do", r.status == 403, &r);
    Ok(())
}
