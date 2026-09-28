//! Sharing (phase 7, decision 4; the ROADMAP's phase 7 acceptance): the
//! share sheet and the join page on the platform's origin, on a node
//! shaped as fragment.club is (the platform cross-site from every fragment,
//! so its session reaches a fragment's page only on a top-level visit), in
//! Chrome where the browser is the point.
//!
//! The owner shares a chat by username; the guest accepts at `/join`, sees
//! it live, and posts in it; the owner removes them, and their socket
//! closes and the chat answers them 403. A member sees who is in and
//! changes nothing. A rewritten desktop (its owner's agent rewrites it)
//! cannot share: not by fetching the sheet or the API, not by framing the
//! sheet, not with a signed call (it holds no key), and not by scripting
//! the window it opens on the sheet (only the platform's home frames it, in
//! a dialog). Every form needs the page's own
//! token, bound to the session, and arms only after a moment.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fragment_core::form;
use serde_json::{json, Value};

use super::signin::{site_cookie, unframed, with_session};
use super::templates::person;
use crate::api::{url_enc, Api, Call, Reply};
use crate::browser::{Browser, Page};
use crate::Suite;

/// Runs on a node restarted with the platform and the fragments on two domains
/// (as fragment.club is), then restarts it as it was for the lanes after.
pub fn share(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("share") {
        return Ok(());
    }
    s.stop()?;
    let api = s.start_as_browsers_see_it()?;
    let result = run(s, &api);
    let embedded = in_a_desktop(s, &api);
    drop(api);
    s.stop()?;
    s.start(false, true)?;
    result.and(embedded)
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

/// A navigation as a browser sends it: a frame's (`iframe`) or a tab's
/// (`document`), from a page on `site` (`same-origin`, `cross-site`), with
/// `cookie`.
fn navigate(api: &Api, url: String, dest: &str, site: &str, cookie: Option<String>) -> Result<Reply> {
    let extra = vec![("sec-fetch-dest", dest.to_string()), ("sec-fetch-site", site.to_string()), ("sec-fetch-mode", "navigate".to_string())];
    api.call(Call { method: "GET", url, cookie, extra, ..Call::default() })
}

/// The `frame-ancestors` directives of a page's policy.
fn ancestors(r: &Reply) -> Vec<String> {
    r.header("content-security-policy").split(';').map(str::trim).filter(|d| d.starts_with("frame-ancestors")).map(str::to_string).collect()
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
    // the owner's agent is in every chat, and echoes what it hears
    s.openrouter.clear_script();
    let make = |label: &str, template: &str| -> Result<String> {
        let name = api.qualified(&owner, label)?;
        let r = api.create_with(&owner, json!({ "name": name, "template": template }))?;
        anyhow::ensure!(r.status == 200, "making {name}: {r}");
        Ok(name)
    };
    let chat = make(&s.name("chat"), "chat")?;
    let desk = make(&s.name("sdesk"), "desktop")?;
    // the owner's agent joins the chat on its alarm
    let agent_in = s.eventually(wait, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None).is_ok_and(|r| r.body["members"].as_array().is_some_and(|a| a.iter().any(|m| m["kind"] == "agent")))
    });
    anyhow::ensure!(agent_in, "the owner's agent did not join {chat}");
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
    // a desktop made without the platform's form (the API, as the CLI makes
    // one): its sheet asks, and says plainly what allowing it risks
    let r = with_session(api, "GET", &format!("/share/{desk}"), &owner_session)?;
    s.ok(
        "a desktop made without the platform's form asks in its sheet whether it may show your fragments inside it, saying plainly what that risks",
        r.status == 200
            && r.text.contains("Your fragments inside it")
            && r.text.contains("It asks to show your other fragments inside it, signed in as you")
            && r.text.contains("catch your clicks")
            && r.text.contains(">Allow<"),
        &r,
    );
    s.ok(
        "and a desktop is its owner's alone from the start: only the people in it may open it (Copy link copies its address, not a share link)",
        r.text.contains("value=\"members\" selected") && !r.text.contains("?view="),
        "",
    );

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
    let r = invite(&fresh, &owner_session, &api.site_origin(&desk))?;
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
    let landed = chrome.until(&page, &format!("location.host.startsWith({chat_host:?}) && document.title === 'Chat'"), wait);
    s.ok(
        "a moment later it arms; the guest accepts at /join and lands in the chat, signed in on its origin",
        armed_in_time && landed && role_of(&guest_id)?.as_deref() == Some("editor"),
        chrome.eval(&page, "location.href + ' ' + document.title").unwrap_or_default(),
    );
    // the chat's page (the platform's, __chat.js) is ready once it knows who it is
    let connected = chrome.until(&page, "document.getElementById('say')?.dataset.ready === '1'", wait);
    let r = super::agents::say(api, &owner, &chat, "share-owner-1", "hello from the owner")?;
    let live = chrome.until(&page, "document.getElementById('messages').textContent.includes('hello from the owner')", wait);
    s.ok("the guest sees the owner's message arrive live", connected && r.status == 200 && live, &r);
    chrome.eval(&page, "document.getElementById('text').value = 'hello from the guest'; document.getElementById('say').requestSubmit(); true")?;
    let posted = s.eventually(wait, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)
            .is_ok_and(|r| r.body["records"].as_array().is_some_and(|a| a.iter().any(|x| x["body"]["text"] == "hello from the guest" && x["principal"] == guest_id.as_str())))
    });
    s.ok("and posts in it, as themselves", posted, api.signed(&owner, "GET", &format!("/api/f/{chat}/channels/chat"), None)?);

    // ---- __fragments: who else is in each of the owner's fragments,
    // from the owner's list (the fragment sent it as the guest joined)
    let owner_site = site_cookie(api, &owner_session, &desk)?;
    let listed = |name: &str| -> Result<Value> {
        let r = api.call(Call { method: "GET", url: api.site_url(&desk, "__fragments"), cookie: Some(format!("fragment_site={owner_site}")), ..Call::default() })?;
        Ok(r.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == name).cloned().unwrap_or_default())
    };
    let in_list = |name: &str| -> Result<Value> {
        let r = api.signed(&owner, "GET", "/api/fragments", None)?;
        Ok(r.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == name).map(|f| f["sharing"].clone()).unwrap_or_default())
    };
    let entry = listed(&chat)?;
    let members = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None)?.body["members"].as_array().map_or(0, Vec::len);
    s.ok(
        "__fragments reports each fragment's member count and guests (not the owner or their agents), its visibility, and its share sheet",
        entry["sharing"] == json!({ "visibility": "link", "members": members, "guests": 1 }) && members == 3 && entry["share"] == format!("{platform}{sheet}"),
        &entry,
    );
    let whole = api.call(Call { method: "GET", url: api.site_url(&desk, "__fragments"), cookie: Some(format!("fragment_site={owner_site}")), ..Call::default() })?;
    s.ok(
        "and whether the desktop may show them inside it (not until its owner allows it)",
        whole.status == 200 && whole.body["frame"] == json!(false),
        &whole,
    );
    // Goal: a __fragments read wakes no fragment. Method: a test hook adds
    // members to a fragment without it telling anyone (its index is not
    // touched): the read still says what the owner's list says, not what
    // the fragment would; the fragment's next real change updates the list.
    let quiet = make(&s.name("squiet"), "blank")?;
    let before = listed(&quiet)?;
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": quiet, "op": "members", "fill": 4 })))?;
    let filled = api.status(&owner, &quiet)?;
    let (after, list) = (listed(&quiet)?, in_list(&quiet)?);
    s.ok(
        "a __fragments read wakes no fragment: its counts are the owner's list's, not the fragment's own",
        before["sharing"] == json!({ "visibility": "link", "members": 1, "guests": 0 })
            && r.status == 200
            && filled.body["counts"]["members"] == 4
            && after["sharing"] == before["sharing"]
            && list == before["sharing"],
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
    let said = "document.getElementById('banner-text')?.textContent ?? ''";
    let closed = chrome.until(&page, &format!("!document.getElementById('banner').hidden && ({said}).includes('access to this chat changed')"), wait);
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
    let agent = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None)?.body["members"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["kind"] == "agent")
        .and_then(|m| m["principal"].as_str().map(str::to_string))
        .context("the owner's agent is in the chat")?;
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

    // ---- the owner's desktop: badges and the Share item
    let Some(mut chrome) = s.browser()? else { return Ok(()) };
    chrome.set_cookie(&format!("{platform}/"), "fragment_session", &owner_session)?;
    let page = chrome.open(&api.site_url(&desk, "__signin?return=/"))?;
    chrome.viewport(&page, 1440, 900, false)?;
    // the chat (made elsewhere, named as New chat names one) is listed among its chats
    let row = format!("document.querySelector('#chats .row[data-key={:?}]')", format!("chat:{chat}"));
    let badged = chrome.until(&page, &format!("{row}?.querySelector('.shared')?.textContent === '1'"), wait);
    s.ok("the desktop badges a fragment shared with someone (the member: 1)", badged, chrome.eval(&page, "document.getElementById('chats').innerHTML").unwrap_or_default());
    let quiet_row = format!("document.querySelector('#apps .row[data-key={:?}]')", format!("app:{quiet}"));
    let public = chrome.until(&page, &format!("{quiet_row}?.querySelectorAll('.shared').length === 2"), wait);
    let unbadged = chrome.eval(&page, "[...document.querySelectorAll('#chats .row, #apps .row')].filter(r => r.querySelector('.shared')).length === 2")? == json!(true);
    s.ok("(the public one with three guests has both badges, and no other fragment has any: the owner's agent is not a guest)", public && unbadged, "");
    chrome.click(&page, &format!(".more[data-fragment={chat:?}]"))?;
    let menu = chrome.until(&page, "!document.getElementById('menu').hidden", wait);
    chrome.click(&page, "#menu-share")?;
    let mut opened = None;
    let shown = s.eventually(wait, || {
        opened = chrome.pages().ok().and_then(|p| p.into_iter().find(|(_, url)| url.ends_with(&sheet)));
        opened.is_some()
    });
    s.ok("its … menu's Share opens the platform's sheet in a window of its own", menu && shown, format!("{:?}", chrome.pages()?));
    if let Some((target, _)) = opened {
        chrome.close_target(&target)?;
    }

    // ---- a rewritten desktop cannot share: its code (as if an agent
    // wrote it) runs in the owner's browser, with the platform's session
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
        "a rewritten desktop cannot read the sheet (no CORS: it never holds the form's token), and what it posts to it invites no one",
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
        extra: vec![("origin", api.site_origin(&desk))],
        ..Call::default()
    })?;
    s.ok("(the signed API takes no session: a call it sends with the cookie and no key's signature is 401)", r.status == 401 && role_of(&stranger_id)?.is_none(), &r);

    // framing the sheet: the owner's session rides into the frame, but the
    // sheet refuses to show there, so there is nothing to lay a click under
    chrome.eval(&page, &format!("(() => {{ const f = document.createElement('iframe'); f.src = {:?}; document.body.append(f); return true; }})()", format!("{platform}{sheet}")))?;
    let framed = |chrome: &mut Browser| chrome.eval_in_frame(&page, "/share/", "document.body?.innerText ?? ''").ok().and_then(|v| v.as_str().map(str::to_string));
    let shown = s.eventually(Duration::from_secs(5), || framed(&mut chrome).is_some_and(|t| t.contains("General access")));
    s.ok("a rewritten desktop that frames the sheet gets a frame without it", !shown, format!("{:?}", framed(&mut chrome)));

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

    // ---- the platform's home, the one page that frames the sheet: its
    // Share opens it in a dialog, and Done closes it
    let home = chrome.open(&format!("{platform}/"))?;
    let link = format!("a[data-share][href={sheet:?}]");
    let listed = chrome.until(&home, &format!("!!document.querySelector({link:?})"), wait);
    if listed {
        chrome.click(&home, &link)?;
    }
    let inside = |chrome: &mut Browser| chrome.eval_in_frame(&home, "/share/", "document.body?.innerText ?? ''").ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
    let shown = listed && chrome.until(&home, "document.getElementById('share').open", wait) && s.eventually(wait, || inside(&mut chrome).contains("General access"));
    chrome.screenshot(&home, &s.scratch.join("share-dialog.png"))?;
    chrome.eval(&home, "window.before = true")?;
    let done = shown && chrome.eval_in_frame(&home, "/share/", "(document.querySelector('button[data-done]').click(), true)").is_ok();
    let closed = done && chrome.until(&home, "!window.before && !document.getElementById('share').open", wait);
    s.ok(
        "the platform's home opens the sheet in a dialog (a frame on its own origin), and its Done closes it (the home reloads, showing what changed)",
        shown && closed,
        format!("listed {listed} shown {shown} done {done}: {}", inside(&mut chrome)),
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

/// The share sheet in its owner's desktop (`__share`; docs/api.md, The
/// share sheet in a frame): the platform's session never reaches a frame
/// of a page on another site, so the desktop's own frame signs in on one
/// sheet of its owner's own fragments, for that desktop alone. Only its
/// owner, signed in there, once they allow it to frame; the sheet's cookie
/// opens that sheet, in a frame, and nothing else of the platform's; and a
/// desktop never changes its own frame grant from inside itself.
fn in_a_desktop(s: &mut Suite, api: &Api) -> Result<()> {
    let platform = api.base.clone();
    let (owner, owner_session) = person(api)?;
    let (other, other_session) = person(api)?;
    let make = |keys: &fragment_nip98::Keys, label: &str, template: &str| -> Result<String> {
        let name = api.qualified(keys, label)?;
        let r = api.create_with(keys, json!({ "name": name, "template": template }))?;
        anyhow::ensure!(r.status == 200, "making {name}: {r}");
        Ok(name)
    };
    // a desktop made without the platform's form: it asks for frame, not allowed yet
    let desk = make(&owner, &s.name("edesk"), "desktop")?;
    let app = make(&owner, &s.name("eapp"), "blank")?;
    let second = make(&owner, &s.name("esecond"), "blank")?;
    // in the owner's list, but not theirs; and not in it at all
    let shared = make(&other, &s.name("eshared"), "blank")?;
    let r = api.signed(&other, "PUT", &format!("/api/f/{shared}/members/{}", api.identity(&owner)?), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(r.status == 200, "sharing {shared} with the owner: {r}");
    let foreign = make(&other, &s.name("eforeign"), "blank")?;
    let in_list = s.eventually(Duration::from_secs(10), || {
        api.signed(&owner, "GET", "/api/fragments", None).is_ok_and(|r| r.body["fragments"].as_array().is_some_and(|a| a.iter().any(|f| f["name"] == shared.as_str())))
    });
    anyhow::ensure!(in_list, "{shared} did not reach the owner's list");
    let desk_origin = api.site_origin(&desk);
    let owner_site = site_cookie(api, &owner_session, &desk)?;
    // `__share` as the desktop's own page frames it
    let share = |name: &str, cookie: Option<&str>| {
        navigate(api, api.site_url(&desk, &format!("__share?name={}", url_enc(name))), "iframe", "same-origin", cookie.map(|c| format!("fragment_site={c}")))
    };
    let grant = |granted: bool| -> Result<()> {
        let r = api.signed(&owner, "PUT", &format!("/api/f/{desk}/grants/frame"), Some(&json!({ "granted": granted })))?;
        anyhow::ensure!(r.status == 200, "the desktop's frame grant: {r}");
        Ok(())
    };

    // ---- refused: no grant, not its owner, not framed, not the owner's own
    let r = share(&app, Some(&owner_site))?;
    s.ok("__share: a desktop its owner has not let frame their fragments is refused (403), and mints nothing", r.status == 403 && r.header("location").is_empty(), &r);
    grant(true)?;
    let r = share(&app, None)?;
    s.ok("allowed, it refuses someone signed out on the desktop's origin (401)", r.status == 401, &r);
    let r = share(&app, Some(&site_cookie(api, &other_session, &desk)?))?;
    s.ok("and someone signed in there who is not its owner (403)", r.status == 403 && r.header("location").is_empty(), &r);
    let top = navigate(api, api.site_url(&desk, &format!("__share?name={}", url_enc(&app))), "document", "cross-site", Some(format!("fragment_site={owner_site}")))?;
    let theirs = navigate(api, api.site_url(&desk, &format!("__share?name={}", url_enc(&app))), "iframe", "same-site", Some(format!("fragment_site={owner_site}")))?;
    let encoded = navigate(api, api.site_url(&desk, &format!("%5F%5Fshare?name={}", url_enc(&app))), "document", "same-origin", Some(format!("fragment_site={owner_site}")))?;
    s.ok(
        "and anything but a frame of the desktop's own page: a tab, another page's frame, a percent-encoded __share (no redemption)",
        top.status == 403 && theirs.status == 403 && !encoded.header("location").contains("token="),
        format!("{top} / {theirs} / {encoded}"),
    );
    let r = share(&shared, Some(&owner_site))?;
    let r2 = share(&foreign, Some(&owner_site))?;
    s.ok(
        "a fragment in the owner's list that is not theirs (shared with them), or one not in it, is refused (403)",
        r.status == 403 && r2.status == 403 && r.message().contains("its owner's own"),
        format!("{r} / {r2}"),
    );

    // ---- the owner's desktop: on to the sheet, its cookie, the sheet signed in
    let r = share(&app, Some(&owner_site))?;
    let embed_url = r.header("location");
    s.ok(
        "the owner's desktop's frame of __share goes on to the app's sheet on the platform (its embed, with a redemption), never reused from a cache",
        r.status == 302 && embed_url.starts_with(&format!("{platform}/share/{app}/embed?token=")) && ["no-store", "no-cache"].iter().any(|d| r.header("cache-control").contains(d)),
        format!("{r} location {embed_url:?} cache-control {:?}", r.header("cache-control")),
    );
    let r = navigate(api, embed_url.clone(), "iframe", "cross-site", None)?;
    let set = r.headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find(|c| c.starts_with("fragment_share=")).unwrap_or("").to_string();
    let token = set.split(';').next().and_then(|c| c.strip_prefix("fragment_share=")).unwrap_or("").to_string();
    let attrs: Vec<&str> = set.split(';').map(str::trim).collect();
    s.ok(
        "its embed sets the sheet's cookie (HttpOnly, Secure, SameSite=None, Partitioned, on the sheet's path alone) and goes on to the sheet, no token in its URL",
        r.status == 302
            && r.header("location") == format!("/share/{app}")
            && token.len() == 64
            && ["HttpOnly", "Secure", "SameSite=None", "Partitioned"].iter().all(|a| attrs.contains(a))
            && attrs.contains(&format!("Path=/share/{app}").as_str()),
        &set,
    );
    let r = navigate(api, embed_url.clone(), "iframe", "cross-site", None)?;
    s.ok("its redemption is spent: again, 401", r.status == 401, &r);
    let embed = format!("fragment_share={token}");
    let sheet = format!("{platform}/share/{app}");
    let r = navigate(api, sheet.clone(), "iframe", "cross-site", Some(embed.clone()))?;
    s.ok(
        "in the desktop's frame, the sheet is the owner's (their controls), and only that desktop may frame it: frame-ancestors its origin, no X-Frame-Options",
        r.status == 200
            && r.text.contains("General access")
            && r.text.contains("name=\"username\"")
            && ancestors(&r) == [format!("frame-ancestors {desk_origin}")]
            && r.header("x-frame-options").is_empty(),
        headers(&r),
    );
    s.ok(
        "it tells that origin alone its height and Done",
        r.text.contains(&format!("offsetHeight }}, {})", json!(desk_origin))) && !r.text.contains("offsetHeight }, location.origin)"),
        "",
    );
    let form = form_of(&r);

    // ---- that cookie opens that sheet, in a frame, and nothing else
    let top = navigate(api, sheet.clone(), "document", "cross-site", Some(embed.clone()))?;
    let fetched = api.call(Call {
        method: "GET",
        url: sheet.clone(),
        cookie: Some(embed.clone()),
        extra: vec![("sec-fetch-dest", "empty".to_string()), ("sec-fetch-site", "cross-site".to_string()), ("sec-fetch-mode", "cors".to_string())],
        ..Call::default()
    })?;
    let login = |r: &Reply| r.status == 302 && r.header("location").contains("/auth/login?");
    s.ok("with only the sheet's cookie, a tab of the sheet, or a fetch of it, is signed out (→ sign in)", login(&top) && login(&fetched), format!("{top} / {fetched}"));
    let other_sheet = navigate(api, format!("{platform}/share/{second}"), "iframe", "cross-site", Some(embed.clone()))?;
    let home = navigate(api, format!("{platform}/"), "iframe", "cross-site", Some(embed.clone()))?;
    let listed = api.call(Call { method: "GET", url: format!("{platform}/api/fragments"), cookie: Some(embed.clone()), ..Call::default() })?;
    let join = navigate(api, format!("{platform}/join/{app}?token={}", "0".repeat(48)), "iframe", "cross-site", Some(embed.clone()))?;
    s.ok(
        "another fragment's sheet, the home, /join, and the API take it for no one (→ sign in, signed out, 401)",
        login(&other_sheet) && home.status == 200 && home.text.contains("/auth/login") && !home.text.contains("Signed in as") && listed.status == 401 && login(&join),
        format!("{other_sheet} / {home} / {listed} / {join}"),
    );
    let both = navigate(api, sheet.clone(), "iframe", "same-origin", Some(format!("fragment_session={owner_session}; {embed}")))?;
    s.ok("a request with the platform's session is answered as before, whatever else it carries: only the platform frames it", framed_by_platform(&both), headers(&both));

    // ---- its form works in the frame, as the owner
    std::thread::sleep(Duration::from_millis(form::DELAY_MS as u64 + 100));
    let post = |form: &str, fields: &[(&str, &str)]| {
        let mut body = format!("form={}", url_enc(form));
        for (k, v) in fields {
            body += &format!("&{k}={}", url_enc(v));
        }
        let extra = vec![
            ("origin", platform.clone()),
            ("sec-fetch-dest", "iframe".to_string()),
            ("sec-fetch-site", "same-origin".to_string()),
            ("sec-fetch-mode", "navigate".to_string()),
        ];
        api.call(Call { method: "POST", url: sheet.clone(), body: Some(body.into_bytes()), content_type: Some("application/x-www-form-urlencoded"), cookie: Some(embed.clone()), extra, ..Call::default() })
    };
    let r = post(&form, &[("action", "visibility"), ("visibility", "public")])?;
    let now = api.status(&owner, &app)?;
    s.ok("the sheet's form in the frame changes who can open it, as its owner", r.status == 303 && r.header("location") == format!("/share/{app}") && now.body["visibility"] == "public", format!("{r} {now}"));
    let r = post(&format!("{form}x"), &[("action", "visibility"), ("visibility", "members")])?;
    s.ok("(a forged form token is refused there too: 403)", r.status == 403 && api.status(&owner, &app)?.body["visibility"] == "public", &r);

    // ---- the desktop's own sheet, inside it: no frame grant, and none changed
    let r = share(&desk, Some(&owner_site))?;
    let r = navigate(api, r.header("location"), "iframe", "cross-site", None)?;
    let own = r.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_share=").map(str::to_string)).unwrap_or_default();
    let own_sheet = format!("{platform}/share/{desk}");
    let r = navigate(api, own_sheet.clone(), "iframe", "cross-site", Some(format!("fragment_share={own}")))?;
    let shown = r.status == 200 && r.text.contains("General access") && !r.text.contains("Your fragments inside it") && !r.text.contains("name=\"granted\"");
    let form = form_of(&r);
    std::thread::sleep(Duration::from_millis(form::DELAY_MS as u64 + 100));
    let stop = api.call(Call {
        method: "POST",
        url: own_sheet.clone(),
        body: Some(format!("form={}&action=frame&granted=no", url_enc(&form)).into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_share={own}")),
        extra: vec![("origin", platform.clone()), ("sec-fetch-dest", "iframe".to_string()), ("sec-fetch-site", "same-origin".to_string()), ("sec-fetch-mode", "navigate".to_string())],
        ..Call::default()
    })?;
    let still = api.status(&owner, &desk)?;
    s.ok(
        "the desktop's own sheet inside it leaves out the frame grant, and refuses its change (403): a desktop never allows or stops itself",
        shown && stop.status == 403 && still.body["frame"] == json!(true),
        format!("{} / {stop} / {}", r.status, still.body["frame"]),
    );

    // ---- a redemption for the wrong place, or the wrong kind of page
    let r = share(&app, Some(&owner_site))?;
    let to = r.header("location");
    let at_signin = navigate(api, api.site_url(&app, &format!("__signin?{}", to.split('?').nth(1).unwrap_or(""))), "iframe", "same-origin", None)?;
    let then = navigate(api, to.clone(), "iframe", "cross-site", None)?;
    s.ok(
        "the sheet's redemption is refused by its fragment's own __signin (401, unspent: the sheet still takes it)",
        at_signin.status == 401 && then.status == 302 && then.cookies().iter().any(|c| c.starts_with("fragment_share=")),
        format!("{at_signin} / {then}"),
    );
    let r = share(&app, Some(&owner_site))?;
    let tab = navigate(api, r.header("location"), "document", "cross-site", None)?;
    let again = navigate(api, r.header("location"), "iframe", "cross-site", None)?;
    s.ok("shown to a tab it is refused (401), and spent", tab.status == 401 && again.status == 401, format!("{tab} / {again}"));
    let framed = navigate(api, api.site_url(&desk, &format!("__frame?name={}&return=/", url_enc(&app))), "iframe", "same-origin", Some(format!("fragment_site={owner_site}")))?;
    let token = framed.header("location").split("token=").nth(1).unwrap_or("").to_string();
    let r = navigate(api, format!("{platform}/share/{app}/embed?token={token}"), "iframe", "cross-site", None)?;
    s.ok("and a fragment's frame redemption (__frame) opens no sheet (401)", framed.status == 302 && token.len() == 64 && r.status == 401, &r);

    // ---- stopped, it mints no more
    grant(false)?;
    let r = share(&app, Some(&owner_site))?;
    s.ok("once its owner stops the grant, __share is refused again (403)", r.status == 403 && r.header("location").is_empty(), &r);
    Ok(())
}
