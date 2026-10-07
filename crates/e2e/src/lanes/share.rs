//! Sharing (docs/api.md, Sharing): the
//! share sheet and the join page on the platform's origin, on a node
//! shaped as fragment.club is (the platform cross-site from every fragment,
//! so its session reaches a fragment's page only on a top-level visit), in
//! Chrome where the browser is the point.
//!
//! The owner shares a todo list by username; the guest accepts at `/join`,
//! sees it live, and adds to it; the owner removes them, and their socket
//! closes and the list answers them 403. A member sees who is in and
//! changes nothing. A fragment's page (its author's code, or an agent's)
//! cannot share: not by fetching the sheet or the API, not by framing the
//! sheet, not with a signed call (it holds no key), and not by scripting
//! the window it opens on the sheet (only the platform's own page frames
//! it, in a dialog). Every form needs the page's own token, bound to the
//! session, and arms only after a moment.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use fragment_core::form;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::signin::{site_cookie, unframed, with_session};
use super::templates::person;
use crate::api::{url_enc, Api, Call, Reply};
use crate::browser::{Browser, Page};
use crate::Suite;

/// Runs on a node restarted with the platform and the fragments on two domains
/// (as fragment.club is), then restarts it as it was for the lanes after.
pub fn share(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("share", &[crate::Need::Node, crate::Need::Chrome, crate::Need::Levers]) {
        return Ok(());
    }
    s.stop()?;
    let api = s.start_as_browsers_see_it()?;
    let result = run(s, &api);
    drop(api);
    s.stop()?;
    s.start(false)?;
    result
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock after 1970").as_millis() as i64
}

/// A page's value for an input named or with the id `key`.
fn value_of(r: &Reply, attr: &str) -> String {
    let at = r.text.find(attr).map(|i| i + attr.len());
    at.and_then(|i| r.text[i..].split_once("value=\"")).and_then(|(_, rest)| rest.split('"').next()).unwrap_or("").replace("&amp;", "&")
}

/// The form token a sharing page holds.
fn form_of(r: &Reply) -> String {
    value_of(r, "name=\"form\"")
}

/// Whether only the platform's own pages may frame a page: `'self'` and
/// nothing else, so no fragment's origin (another origin, one site with the
/// platform or not) may.
fn framed_by_platform(r: &Reply) -> bool {
    let csp = r.header("content-security-policy");
    csp.split(';').map(str::trim).filter(|d| d.starts_with("frame-ancestors")).eq(["frame-ancestors 'self'"]) && r.header("x-frame-options") == "SAMEORIGIN"
}

/// Whether a page severs a window that opened it.
fn unopened(r: &Reply) -> bool {
    r.header("cross-origin-opener-policy") == "same-origin"
}

fn headers(r: &Reply) -> String {
    format!(
        "{} csp {:?} xfo {:?} coop {:?}",
        r.status,
        r.header("content-security-policy"),
        r.header("x-frame-options"),
        r.header("cross-origin-opener-policy")
    )
}

/// A form a sharing page posts, from `origin`, with `session`'s cookie.
fn post(api: &Api, path: &str, session: &str, origin: &str, fields: &[(&str, &str)]) -> Result<Reply> {
    let body = fields.iter().map(|(k, v)| format!("{k}={}", url_enc(v))).collect::<Vec<_>>().join("&");
    api.call(Call {
        method: "POST",
        url: format!("{}{path}", api.base),
        body: Some(body.into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("origin", origin.to_string())],
        ..Call::default()
    })
}

/// A page's form token once its buttons have armed.
fn armed(api: &Api, path: &str, session: &str) -> Result<String> {
    let r = with_session(api, "GET", path, session)?;
    anyhow::ensure!(r.status == 200, "GET {path}: {r}");
    std::thread::sleep(Duration::from_millis(form::DELAY_MS as u64 + 100));
    Ok(form_of(&r))
}

fn run(s: &mut Suite, api: &Api) -> Result<()> {
    let wait = Duration::from_secs(20);
    let platform = api.base.clone();
    let (owner, owner_session) = person(api)?;
    let (guest, guest_session) = person(api)?;
    let (stranger, stranger_session) = person(api)?;
    let (member, member_session) = person(api)?;
    let (owner_name, guest_name, stranger_name) = (api.username(&owner)?, api.username(&guest)?, api.username(&stranger)?);
    let (guest_id, stranger_id) = (api.identity(&guest)?, api.identity(&stranger)?);
    let make = |label: &str, template: &str| -> Result<String> {
        let name = api.qualified(&owner, label)?;
        let r = api.create_with(&owner, json!({ "name": name, "template": template }))?;
        anyhow::ensure!(r.status == 200, "making {name}: {r}");
        Ok(name)
    };
    let chat = make(&s.name("list"), "todo")?;
    // an agent of the owner's is in it too: it counts as no guest
    let hand = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, &owner) })))?;
    anyhow::ensure!(r.status == 200, "registering the owner's agent: {r}");
    let agent = r.body["id"].as_str().unwrap_or("").to_string();
    let r = api.signed(&owner, "PUT", &format!("/api/f/{chat}/members/{agent}"), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "the owner's agent joins {chat}: {r}");
    let chat_label = chat.split('.').next().unwrap_or("").to_string();
    let sheet = format!("/share/{chat}");
    let invites_for = |id: &str| -> Result<Vec<Value>> {
        let r = api.signed(&owner, "GET", &format!("/api/f/{chat}/invites"), None)?;
        Ok(r.body["invites"].as_array().into_iter().flatten().filter(|i| i["invitee"] == id).cloned().collect())
    };
    let role_of = |id: &str| -> Result<Option<String>> {
        let r = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None)?;
        Ok(r.body["members"].as_array().into_iter().flatten().find(|m| m["principal"] == id).and_then(|m| m["role"].as_str().map(str::to_string)))
    };

    // ---- the sheet
    let r = api.unsigned("GET", &sheet, None)?;
    s.ok("the share sheet sends a signed-out browser to sign in first, and back", r.status == 302 && r.header("location").contains(&format!("/auth/login?return={}", url_enc(&sheet))), &r);
    let r = with_session(api, "GET", &sheet, &owner_session)?;
    s.ok(
        "the owner's sheet shows who is in, by username (their agent too), and the owner's controls",
        r.status == 200 && r.text.contains(&format!("@{owner_name}")) && r.text.contains(&format!("{owner_name}'s agent")) && r.text.contains("name=\"username\"") && r.text.contains("General access"),
        &r,
    );
    s.ok(
        "only the platform's own pages may frame it (`frame-ancestors 'self'` alone: every fragment is another origin), and it severs a window that opened it",
        framed_by_platform(&r) && unopened(&r),
        headers(&r),
    );
    s.ok("its buttons and selects come disabled: they arm a moment after the page shows", r.text.contains("data-arm disabled") && !r.text.contains("data-arm>"), "");
    s.ok("and it offers the share link to copy", r.text.contains(&format!("{}?view=", api.site_url(&chat, ""))), "");
    let r = with_session(api, "GET", &format!("{sheet}?username={guest_name}&role=editor&action=invite&visibility=public&form=1"), &owner_session)?;
    s.ok(
        "it takes nothing from its URL: a link cannot prefill what a click would approve",
        r.status == 200 && !r.text.contains(&guest_name) && r.text.contains("value=\"link\" selected") && !r.text.contains("value=\"public\" selected"),
        &r,
    );
    let r = with_session(api, "GET", &sheet, &stranger_session)?;
    s.ok("someone who is not in it gets a 403 page", r.status == 403 && r.text.contains("Not yours to share") && !r.text.contains("name=\"form\"") && unframed(&r), &r);
    // ---- its forms: the page's own token, from the platform's origin, after a moment
    let invite = |form: &str, session: &str, origin: &str| {
        post(api, &sheet, session, origin, &[("form", form), ("action", "invite"), ("username", guest_name.as_str()), ("role", "editor")])
    };
    let fresh = form_of(&with_session(api, "GET", &sheet, &owner_session)?);
    let r = invite(&fresh, &owner_session, &platform)?;
    s.ok(
        "a form sent before its buttons arm is refused (403): the click that opened the sheet cannot confirm in it",
        r.status == 403 && r.text.contains("too quick") && invites_for(&guest_id)?.is_empty(),
        &r,
    );
    std::thread::sleep(Duration::from_millis(form::DELAY_MS as u64 + 100));
    let r = invite("", &owner_session, &platform)?;
    s.ok("a POST without the form's token is refused (403)", r.status == 403 && invites_for(&guest_id)?.is_empty(), &r);
    let other_session = api.sign_in(&format!("t-{}@e2e.test", &owner.pubkey_hex()[..12]))?;
    let others = armed(api, &sheet, &other_session)?;
    let r = invite(&others, &owner_session, &platform)?;
    s.ok("and one with another session's token (the same person's other browser)", r.status == 403 && invites_for(&guest_id)?.is_empty(), &r);
    let r = invite(&fresh, &owner_session, &api.site_origin(&chat))?;
    s.ok("a POST from a fragment's page is refused (403), with a good token too", r.status == 403 && invites_for(&guest_id)?.is_empty(), &r);
    let r = post(api, &sheet, &owner_session, &platform, &[("form", &fresh), ("action", "invite"), ("username", "nobody-here-e2e"), ("role", "viewer")])?;
    s.ok("inviting a username no one holds says so", r.status == 404 && r.text.contains("no one is @nobody-here-e2e"), &r);

    // ---- the owner shares the chat by username
    let r = invite(&fresh, &owner_session, &platform)?;
    let link = value_of(&r, "id=\"invite-link\"");
    let token = link.split("token=").nth(1).unwrap_or("").to_string();
    s.ok(
        "the owner shares the chat with a second person by username: the sheet answers the link to send them",
        r.status == 200 && link.starts_with(&format!("{platform}/join/{chat}?token=")) && token.len() == 48,
        &r,
    );
    let pending = invites_for(&guest_id)?;
    let r = with_session(api, "GET", &sheet, &owner_session)?;
    s.ok(
        "an invite for them alone, one use, pending on the sheet",
        pending.len() == 1 && pending[0]["role"] == "editor" && pending[0]["usesLeft"] == 1 && r.text.contains("invited as editor") && r.text.contains(&format!("@{guest_name}")),
        json!(pending),
    );

    // ---- the join page
    let join = link.trim_start_matches(&platform).to_string();
    let join_path = format!("/join/{chat}");
    let r = api.unsigned("GET", &join, None)?;
    s.ok("the join link sends a signed-out browser to sign in first, keeping the invite", r.status == 302 && r.header("location").contains("/auth/login?return=") && r.header("location").contains(&token), &r);
    let r = with_session(api, "GET", &join, &stranger_session)?;
    s.ok(
        "someone else who holds the link is told it is not theirs (403), and offered no button",
        r.status == 403 && r.text.contains("for someone else") && r.text.contains(&format!("@{guest_name}")) && !r.text.contains("name=\"token\""),
        &r,
    );
    let r = api.signed(&stranger, "POST", &format!("/api/f/{chat}/join"), Some(&json!({ "token": token })))?;
    s.ok("nor can they join with it through the API: an invite by username is its invitee's alone (403)", r.status == 403 && role_of(&stranger_id)?.is_none(), &r);
    let r = with_session(api, "GET", &join, &guest_session)?;
    s.ok(
        "the guest sees what it grants, from whom, and a Join button that arms after a moment",
        r.status == 200 && r.text.contains("as an editor") && r.text.contains(&format!("@{owner_name}")) && r.text.contains("data-arm disabled") && unframed(&r) && unopened(&r),
        &r,
    );
    let joining = form_of(&r);
    let r = post(api, &join_path, &guest_session, &platform, &[("form", &joining), ("token", &token)])?;
    s.ok("accepting before the button arms is refused (403)", r.status == 403 && role_of(&guest_id)?.is_none(), &r);
    std::thread::sleep(Duration::from_millis(form::DELAY_MS as u64 + 100));
    let r = post(api, &join_path, &guest_session, &api.site_origin(&chat), &[("form", &joining), ("token", &token)])?;
    s.ok("and accepting from a fragment's page (403)", r.status == 403 && role_of(&guest_id)?.is_none(), &r);
    let mut portless = reqwest::Url::parse(&platform)?;
    let _ = portless.set_port(None);
    let r = post(api, &join_path, &guest_session, &portless.origin().ascii_serialization(), &[("form", &joining), ("token", &token)])?;
    s.ok("and from the platform's host on another port (another origin, whose name begins the same)", r.status == 403 && role_of(&guest_id)?.is_none(), &r);
    let r = post(api, &join_path, &guest_session, &platform, &[("token", &token)])?;
    s.ok("and without the page's token (403)", r.status == 403 && role_of(&guest_id)?.is_none(), &r);
    let on_chat = site_cookie(api, &owner_session, &chat)?;
    let r = api.page(&chat, &format!("__join?invite={token}"), Some(&format!("fragment_site={on_chat}")))?;
    s.ok("a fragment's origin accepts no invites (its __join is gone; /join/<name> is the platform's)", r.status == 404 && !r.text.contains("Join"), &r);

    // ---- in Chrome: the guest accepts, sees the chat live, and posts in it
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the share lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    chrome.set_cookie(&format!("{platform}/"), "fragment_session", &guest_session)?;
    let page = chrome.open(&link)?;
    let button = "document.querySelector('button[data-arm]')";
    anyhow::ensure!(chrome.until(&page, &format!("!!{button}"), wait), "the join page did not show: {}", chrome.eval(&page, "document.body?.innerText ?? ''")?);
    // clicked at once, as a double-click's second half would be
    let early = chrome.eval(&page, &format!("{button}.disabled"))?;
    chrome.click(&page, "button[data-arm]")?;
    std::thread::sleep(Duration::from_millis(300));
    let stayed = chrome.eval(&page, "location.pathname.startsWith('/join/')")? == json!(true);
    s.ok("in a browser, the Join button is disabled as the page shows: a click then does nothing", early == json!(true) && stayed && role_of(&guest_id)?.is_none(), &early);
    let armed_in_time = chrome.until(&page, &format!("{button} && !{button}.disabled"), Duration::from_secs(3));
    chrome.screenshot(&page, &s.scratch.join("share-join.png"))?;
    chrome.click(&page, "button[data-arm]")?;
    let chat_host = format!("{chat_label}--");
    let landed = chrome.until(&page, &format!("location.host.startsWith({chat_host:?}) && document.title === 'Todo'"), wait);
    s.ok(
        "a moment later it arms; the guest accepts at /join and lands in the chat, signed in on its origin",
        armed_in_time && landed && role_of(&guest_id)?.as_deref() == Some("editor"),
        chrome.eval(&page, "location.href + ' ' + document.title").unwrap_or_default(),
    );
    // the list's page is live once it knows who is here
    let connected = chrome.until(&page, "document.getElementById('here')?.textContent.includes('here')", wait);
    let r = api.op(&owner, &chat, "add", "share-owner-1", json!({ "text": "from the owner" }))?;
    let live = chrome.until(&page, "document.getElementById('todos').textContent.includes('from the owner')", wait);
    s.ok("the guest sees the owner's change arrive live", connected && r.status == 200 && live, &r);
    chrome.eval(&page, "document.getElementById('text').value = 'from the guest'; document.getElementById('add').requestSubmit(); true")?;
    let ops = || api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/ops"), None);
    let added = s.eventually(wait, || {
        ops().is_ok_and(|r| r.body["records"].as_array().is_some_and(|a| a.iter().any(|x| x["body"]["op"] == "add" && x["principal"] == guest_id.as_str())))
    });
    s.ok("and adds to it, as themselves", added, ops()?);

    // ---- the owner's list: who else is in each of their fragments (the
    // fragment sent it as the guest joined), as the platform's page shows
    let listed = |name: &str| -> Result<Value> {
        let r = api.signed(&owner, "GET", "/api/fragments", None)?;
        Ok(r.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == name).cloned().unwrap_or_default())
    };
    let entry = listed(&chat)?;
    let members = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None)?.body["members"].as_array().map_or(0, Vec::len);
    s.ok(
        "the owner's list reports each fragment's member count and guests (not the owner or their agents), and its visibility",
        entry["sharing"] == json!({ "visibility": "link", "members": members, "guests": 1 }) && members == 3,
        &entry,
    );
    // Goal: the list reads no fragment. Method: a test hook adds members
    // to a fragment without it telling anyone (its index is not touched):
    // the list still says what it was sent, not what the fragment would;
    // the fragment's next real change updates it.
    let quiet = make(&s.name("squiet"), "blank")?;
    // its create told the list who is in it, and its template's install
    // its face (sending the sharing as it was then): real changes, so both
    // land before the fill
    let installed = s.eventually(wait, || listed(&quiet).is_ok_and(|f| f.get("sharing").is_some() && f["title"] == "Blank"));
    anyhow::ensure!(installed, "{quiet}'s create and its template's install never reached its owner's list");
    let before = listed(&quiet)?;
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": quiet, "op": "members", "fill": 4 })))?;
    let filled = api.status(&owner, &quiet)?;
    let after = listed(&quiet)?;
    s.ok(
        "the owner's list wakes no fragment: its counts are the list's, not the fragment's own",
        before["sharing"] == json!({ "visibility": "link", "members": 1, "guests": 0 })
            && r.status == 200
            && filled.body["counts"]["members"] == 4
            && after["sharing"] == before["sharing"],
        format!("{before} / {after} / {}", filled.body["counts"]),
    );
    let r = api.signed(&owner, "PUT", &format!("/api/f/{quiet}/visibility"), Some(&json!({ "visibility": "public" })))?;
    let after = listed(&quiet)?;
    s.ok(
        "and a change to who may open it sends the fragment's sharing to the list",
        r.status == 200 && after["sharing"] == json!({ "visibility": "public", "members": 4, "guests": 3 }),
        &after,
    );

    // ---- the owner removes the guest: their socket closes, and the chat is 403 to them
    let form = armed(api, &sheet, &owner_session)?;
    let r = post(api, &sheet, &owner_session, &platform, &[("form", &form), ("action", "role"), ("member", &guest_id), ("role", "remove")])?;
    s.ok("the owner removes the guest from the sheet (the last choice of their role's menu)", r.status == 303 && r.header("location") == sheet && role_of(&guest_id)?.is_none(), &r);
    let said = "document.getElementById('here')?.textContent ?? ''";
    let closed = chrome.until(&page, &format!("({said}).includes('access changed')"), wait);
    s.ok("the guest's socket closes (their page says its access changed)", closed, chrome.eval(&page, said).unwrap_or_default());
    let again = site_cookie(api, &guest_session, &chat)?;
    let r = api.page(&chat, "", Some(&format!("fragment_site={again}")))?;
    chrome.reload(&page)?;
    let refused = chrome.until(&page, "document.contentType === 'text/html' && document.body?.innerText.includes(\"You don't have access to\")", wait);
    s.ok("and the chat answers them 403: their reload, a page saying they have no access", r.status == 403 && refused, &r);
    drop(chrome);

    // ---- an invite anyone may use (the CLI's `fragment invite`) is
    // accepted on the same page
    let member_id = api.identity(&member)?;
    let open = api.signed(&owner, "POST", &format!("/api/f/{chat}/invites"), Some(&json!({ "role": "viewer" })))?;
    let open_token = open.body["token"].as_str().unwrap_or("").to_string();
    let r = with_session(api, "GET", &format!("{join_path}?token={open_token}"), &member_session)?;
    let shown = r.status == 200 && r.text.contains("as a viewer") && r.text.contains("name=\"token\"");
    std::thread::sleep(Duration::from_millis(form::DELAY_MS as u64 + 100));
    let r = post(api, &join_path, &member_session, &platform, &[("form", &form_of(&r)), ("token", &open_token)])?;
    s.ok(
        "an invite for anyone who holds it (the CLI's) is accepted at /join too, and sends them to the fragment",
        shown && r.status == 303 && r.header("location") == format!("/auth/fragment?name={chat}&return=/") && role_of(&member_id)?.as_deref() == Some("viewer"),
        &r,
    );

    // ---- a member sees who is in, and changes nothing
    let r = with_session(api, "GET", &sheet, &member_session)?;
    s.ok(
        "a member sees who is in, and no controls and no link",
        r.status == 200 && r.text.contains(&format!("@{owner_name}")) && !r.text.contains("name=\"action\"") && !r.text.contains("?view=") && !r.text.contains("name=\"form\""),
        &r,
    );
    // a good token for the member's own session (the harness holds the
    // cookie a browser would), so only the fragment's rule is left to refuse
    let token_of_member = form::issue(&member_session, &format!("share:{chat}"), now_ms() - form::DELAY_MS - 50);
    let open = api.signed(&owner, "POST", &format!("/api/f/{chat}/invites"), Some(&json!({ "role": "viewer" })))?;
    let open_id = open.body["id"].as_str().unwrap_or("").to_string();
    let before = api.status(&owner, &chat)?;
    let mut answers = vec![];
    for fields in [
        vec![("action", "invite"), ("username", stranger_name.as_str()), ("role", "editor")],
        vec![("action", "visibility"), ("visibility", "public")],
        vec![("action", "rotate")],
        vec![("action", "role"), ("member", agent.as_str()), ("role", "viewer")],
        vec![("action", "remove"), ("member", agent.as_str())],
        vec![("action", "uninvite"), ("invite", open_id.as_str())],
    ] {
        let action = fields[0].1;
        let mut all = vec![("form", token_of_member.as_str())];
        all.extend(fields);
        answers.push((action, post(api, &sheet, &member_session, &platform, &all)?.status));
    }
    let after = api.status(&owner, &chat)?;
    let invites = api.signed(&owner, "GET", &format!("/api/f/{chat}/invites"), None)?;
    s.ok(
        "a member's every change is refused (403), their own good form token and all, and nothing changes",
        answers.iter().all(|(_, status)| *status == 403)
            && after.body["visibility"] == before.body["visibility"]
            && after.body["viewToken"] == before.body["viewToken"]
            && role_of(&agent)?.as_deref() == Some("editor")
            && invites_for(&stranger_id)?.is_empty()
            && invites.body["invites"].as_array().is_some_and(|a| a.iter().any(|i| i["id"] == open_id.as_str())),
        format!("{answers:?}"),
    );

    // ---- the owner sets who can open it, and makes a new link
    let form = armed(api, &sheet, &owner_session)?;
    let r = post(api, &sheet, &owner_session, &platform, &[("form", &form), ("action", "rotate")])?;
    let rotated = api.status(&owner, &chat)?;
    let r2 = post(api, &sheet, &owner_session, &platform, &[("form", &form), ("action", "visibility"), ("visibility", "members")])?;
    let now = api.status(&owner, &chat)?;
    s.ok(
        "the owner makes a new link, and sets who can open it, from the sheet",
        r.status == 303 && rotated.body["viewToken"] != before.body["viewToken"] && r2.status == 303 && now.body["visibility"] == "members",
        format!("{r} {r2} {now}"),
    );
    let r = post(api, &sheet, &owner_session, &platform, &[("form", &form), ("action", "visibility"), ("visibility", "link")])?;
    anyhow::ensure!(r.status == 303, "visibility back to link: {r}");

    // ---- a fragment's page cannot share: its code (its author's, or an
    // agent's) runs in the owner's browser, with the platform's session
    let Some(mut chrome) = s.browser()? else { return Ok(()) };
    chrome.set_cookie(&format!("{platform}/"), "fragment_session", &owner_session)?;
    let page = chrome.open(&api.site_url(&chat, "__signin?return=/"))?;
    anyhow::ensure!(chrome.until(&page, "document.title === 'Todo'", wait), "the owner's list did not open: {}", chrome.eval(&page, "location.href")?);
    let attempt = |chrome: &mut Browser, page: &Page, js: &str| chrome.eval(page, js).unwrap_or_else(|e| json!(e.to_string()));
    let read = attempt(
        &mut chrome,
        &page,
        &format!("fetch({:?}, {{ credentials: 'include' }}).then(r => r.text()).then(t => 'read ' + t.length, e => 'refused ' + e.name)", format!("{platform}{sheet}")),
    );
    let forged = format!("action=invite&username={stranger_name}&role=editor&form={}", url_enc(&token_of_member));
    let posted = attempt(
        &mut chrome,
        &page,
        &format!(
            "fetch({:?}, {{ method: 'POST', mode: 'no-cors', credentials: 'include', headers: {{ 'content-type': 'application/x-www-form-urlencoded' }}, body: {forged:?} }}).then(() => 'sent', e => 'refused ' + e.name)",
            format!("{platform}{sheet}")
        ),
    );
    std::thread::sleep(Duration::from_millis(300));
    s.ok(
        "a fragment's page cannot read the sheet (no CORS: it never holds the form's token), and what it posts to it invites no one",
        read.as_str().is_some_and(|r| r.starts_with("refused")) && invites_for(&stranger_id)?.is_empty(),
        format!("{read} / {posted}"),
    );
    let members_api = format!("{platform}/api/f/{chat}/members");
    let listed = attempt(&mut chrome, &page, &format!("fetch({members_api:?}, {{ credentials: 'include' }}).then(r => r.text()).then(t => 'read ' + t, e => 'refused ' + e.name)"));
    let put = attempt(
        &mut chrome,
        &page,
        &format!(
            "fetch({:?}, {{ method: 'PUT', credentials: 'include', headers: {{ 'content-type': 'application/json' }}, body: '{{\"role\":\"editor\"}}' }}).then(r => 'status ' + r.status, e => 'refused ' + e.name)",
            format!("{members_api}/{stranger_id}")
        ),
    );
    let invited = attempt(
        &mut chrome,
        &page,
        &format!(
            "fetch({:?}, {{ method: 'POST', mode: 'no-cors', credentials: 'include', headers: {{ 'content-type': 'text/plain' }}, body: '{{\"role\":\"editor\",\"invitee\":\"{stranger_id}\"}}' }}).then(() => 'sent', e => 'refused ' + e.name)",
            format!("{platform}/api/f/{chat}/invites")
        ),
    );
    std::thread::sleep(Duration::from_millis(300));
    s.ok(
        "nor fetch the signed API: it cannot read who is in, and its calls change nothing",
        listed.as_str().is_some_and(|r| r.starts_with("refused")) && role_of(&stranger_id)?.is_none() && invites_for(&stranger_id)?.is_empty(),
        format!("{listed} / {put} / {invited}"),
    );
    let r = api.call(Call {
        method: "PUT",
        url: format!("{members_api}/{stranger_id}"),
        body: Some(br#"{"role":"editor"}"#.to_vec()),
        content_type: Some("application/json"),
        cookie: Some(format!("fragment_session={owner_session}")),
        extra: vec![("origin", api.site_origin(&chat))],
        ..Call::default()
    })?;
    s.ok("(the signed API takes no session: a call it sends with the cookie and no key's signature is 401)", r.status == 401 && role_of(&stranger_id)?.is_none(), &r);

    // framing the sheet: the owner's session rides into the frame, but the
    // sheet refuses to show there, so there is nothing to lay a click under
    chrome.eval(&page, &format!("(() => {{ const f = document.createElement('iframe'); f.src = {:?}; document.body.append(f); return true; }})()", format!("{platform}{sheet}")))?;
    let framed = |chrome: &mut Browser| chrome.eval_in_frame(&page, "/share/", "document.body?.innerText ?? ''").ok().and_then(|v| v.as_str().map(str::to_string));
    let shown = s.eventually(Duration::from_secs(5), || framed(&mut chrome).is_some_and(|t| t.contains("General access")));
    s.ok("a fragment's page that frames the sheet gets a frame without it", !shown, format!("{:?}", framed(&mut chrome)));

    // scripting the window it opens on the sheet (on a click of the
    // owner's, as a popup must be): the sheet severs it
    chrome.eval(
        &page,
        &format!(
            "(() => {{ const b = document.createElement('button'); b.id = 'taker'; b.textContent = 'x'; b.style.cssText = 'position:fixed;right:0;bottom:0;width:60px;height:40px;z-index:99'; b.onclick = () => {{ window.__sheet = window.open({:?}, 'sheet', 'popup,width=480,height=640'); }}; document.body.append(b); return true; }})()",
            format!("{platform}{sheet}")
        ),
    )?;
    chrome.click(&page, "#taker")?;
    let mut popup = None;
    let loaded = s.eventually(wait, || {
        popup = chrome.pages().ok().and_then(|p| p.into_iter().find(|(_, url)| url.ends_with(&sheet)));
        popup.is_some()
    });
    let severed = loaded && chrome.until(&page, "window.__sheet.closed === true", Duration::from_secs(5));
    chrome.eval(&page, "try { window.__sheet.location.href = location.origin + '/#taken'; } catch (e) {} true")?;
    std::thread::sleep(Duration::from_millis(500));
    let still = popup.as_ref().is_some_and(|(id, _)| chrome.pages().is_ok_and(|p| p.iter().any(|(t, url)| t == id && url.ends_with(&sheet))));
    s.ok(
        "a window it opens on the sheet is severed from it: its handle reads closed, and cannot navigate it away",
        severed && still,
        format!("{:?} {:?}", chrome.eval(&page, "window.__sheet.closed"), chrome.pages()?),
    );

    // ---- the shell, the one page that frames the sheet: an app's Share
    // opens it in a dialog, and Done closes it (at /settings, which the
    // shell opens whether or not the person has a chat yet)
    let home = chrome.open(&format!("{platform}/settings"))?;
    chrome.viewport(&home, 1280, 800, false)?;
    let row = format!("#apps .row[data-key={:?}]", format!("app:{chat}"));
    let share = "#stack button[aria-label='Share…']";
    // A hand's click lands where the button is drawn: only once the page has
    // loaded and its fonts are in, or a layout still moving puts the click
    // elsewhere.
    let settled = |selector: &str| format!("document.readyState === 'complete' && document.fonts.status === 'loaded' && !!document.querySelector({selector:?})");
    // (the sidebar laid out open, as a desktop's is)
    let listed = chrome.until(&home, &format!("{} && document.getElementById('layout').classList.contains('left-open')", settled(&row)), wait);
    if listed {
        chrome.click(&home, &row)?;
    }
    let windowed = listed && chrome.until(&home, &settled(share), wait);
    if windowed {
        chrome.click(&home, share)?;
    }
    let inside = |chrome: &mut Browser| chrome.eval_in_frame(&home, "/share/", "document.body?.innerText ?? ''").ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let shown = windowed && chrome.until(&home, "document.getElementById('sheet').open", wait) && s.eventually(wait, || inside(&mut chrome).contains("General access"));
    chrome.screenshot(&home, &s.scratch.join("share-dialog.png"))?;
    let done = shown && chrome.eval_in_frame(&home, "/share/", "(document.querySelector('button[data-done]').click(), true)").is_ok();
    let closed = done && chrome.until(&home, &format!("!document.getElementById('sheet').open && !document.querySelector('#sheet iframe') && !!document.querySelector({row:?})"), wait);
    s.ok(
        "the shell opens an app's share sheet in a dialog (a frame on its own origin), and its Done closes it (the shell reads the list again)",
        shown && closed,
        format!("listed {listed} windowed {windowed} shown {shown} done {done}: {}", inside(&mut chrome)),
    );
    chrome.close(home)?;

    // the sheet as the owner sees it, at a popup's size and a phone's, light
    // and dark (evidence for a person): no sideways scroll
    let shown = chrome.open(&format!("{platform}{sheet}"))?;
    let mut fits = vec![];
    for (width, mobile, scheme, shot) in [(480, false, "light", "share-sheet.png"), (390, true, "dark", "share-sheet-phone.png")] {
        chrome.viewport(&shown, width, 900, mobile)?;
        chrome.color_scheme(&shown, scheme)?;
        let ready = chrome.until(&shown, "document.readyState === 'complete' && !!document.querySelector('footer')", wait);
        fits.push((width, ready && chrome.eval(&shown, "document.documentElement.scrollWidth <= innerWidth")? == json!(true)));
        chrome.screenshot(&shown, &s.scratch.join(shot))?;
    }
    s.ok("the sheet fits a popup's width and a phone's, with no sideways scroll", fits.iter().all(|(_, fit)| *fit), format!("{fits:?}"));
    Ok(())
}
