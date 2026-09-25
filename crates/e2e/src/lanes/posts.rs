//! Posts (ROADMAP decision 18): a channel fragment.json declares with a
//! `post` role takes records the platform appends for the poster, with no
//! app code at all. Checked in the `channels` section (the rules: the role,
//! the replay, the size, a channel without one, the deploy check, the
//! public call budget, and who hears a post) and the `browser` section
//! (`fragment.post` from a page).

use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use fragment_proto::{limits, ErrorCode};
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{now_s, Api, Call, Reply, Socket};
use crate::browser::Lease;
use crate::Suite;

/// Channels and nothing else: no app.mjs.
const ROOM_JSON: &[u8] = include_bytes!("../../fixtures/room.json");
/// Channels an app hears through a trigger, beside a public query.
const HEARD_APP: &[u8] = include_bytes!("../../fixtures/heard.mjs");
const HEARD_JSON: &[u8] = include_bytes!("../../fixtures/heard.json");
/// A postable channel, and a mutation that fills it 64 records a call.
const WALL_APP: &[u8] = include_bytes!("../../fixtures/wall.mjs");
const WALL_JSON: &[u8] = include_bytes!("../../fixtures/wall.json");

fn post(api: &Api, keys: &Keys, name: &str, channel: &str, id: &str, body: Value) -> Result<Reply> {
    api.signed(keys, "POST", &format!("/api/f/{name}/channels/{channel}"), Some(&json!({ "id": id, "body": body })))
}

/// A browser's post, as `fragment.post` sends it: the call's door on the site.
fn site_post(api: &Api, name: &str, channel: &str, id: &str, body: Value, cookie: Option<&str>, content_type: &'static str) -> Result<Reply> {
    api.call(Call {
        method: "POST",
        url: api.site_url(name, &format!("__op/channels/{channel}")),
        body: Some(json!({ "id": id, "input": body }).to_string().into_bytes()),
        content_type: Some(content_type),
        cookie: cookie.map(str::to_string),
        ..Call::default()
    })
}

/// The records of a channel, read by `keys`.
fn records(api: &Api, keys: &Keys, name: &str, channel: &str) -> Vec<Value> {
    api.signed(keys, "GET", &format!("/api/f/{name}/channels/{channel}?after=0"), None)
        .ok()
        .and_then(|r| r.body["records"].as_array().cloned())
        .unwrap_or_default()
}

/// A public fragment with no app code: `room.json`'s channels (and, with
/// `page`, a page to open them from). Answers its name and its creation.
fn room(s: &Suite, api: &Api, owner: &Keys, base: &str, page: bool) -> Result<(String, Value)> {
    let name = s.named(api, owner, base)?;
    let c = s.create(api, owner, &name)?;
    api.signed(owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    let mut files: Vec<(&str, Option<&[u8]>)> = vec![("fragment.json", Some(ROOM_JSON))];
    if page {
        files.push(("site/index.html", Some(b"<!doctype html><meta charset=\"utf-8\"><title>room</title><p>a room</p>")));
    }
    s.commit(&c, &files);
    s.deploy(&c);
    Ok((name, c))
}

/// The `channels` section's posts.
pub fn posts(s: &mut Suite, api: &Api) -> Result<()> {
    let owner = api.person()?;
    let editor = api.person()?;
    let viewer = api.person()?;
    let stranger = api.person()?;
    let (name, c) = room(s, api, &owner, "room", false)?;
    for (who, role) in [(&editor, "editor"), (&viewer, "viewer")] {
        api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", who.pubkey_hex()), Some(&json!({ "role": role })))?;
    }
    let (owner_id, editor_id) = (api.identity(&owner)?, api.identity(&editor)?);

    // the role
    let r = post(api, &editor, &name, "notes", "n1", json!({ "text": "from the editor" }))?;
    let record = &r.body["record"];
    s.ok(
        "a post at the channel's role is appended by the platform, naming its poster",
        r.status == 200
            && r.body["replayed"] == false
            && record["seq"] == 1
            && record["principal"] == editor_id.as_str()
            && record["kind"] == "message"
            && record["body"] == json!({ "text": "from the editor" }),
        &r,
    );
    let status = api.status(&owner, &name)?;
    let builds = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "code-builds" })))?;
    s.ok(
        "with no app code: the fragment has none, and loads no worker",
        status.body["code"]["sha"].is_null() && builds.body["builds"] == 0,
        format!("{} {}", status.body["code"], builds.body),
    );
    let held = records(api, &viewer, &name, "notes");
    s.ok("the channel holds the record, for its readers", held.len() == 1 && held[0]["principal"] == editor_id.as_str(), json!(held));
    let r = post(api, &viewer, &name, "notes", "n2", json!({ "text": "from a viewer" }))?;
    s.ok(
        "below the post role is 403, and appends nothing",
        r.status == 403 && r.message().contains("editor") && records(api, &owner, &name, "notes").len() == 1,
        &r,
    );
    let r = api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", viewer.pubkey_hex()), Some(&json!({ "role": "editor" })))?;
    let promoted = post(api, &viewer, &name, "notes", "n2", json!({ "text": "from a viewer" }))?;
    s.ok("and the same post at the role goes through", r.status == 200 && promoted.status == 200 && promoted.body["record"]["seq"] == 2, &promoted);

    // the replay
    let r = post(api, &editor, &name, "notes", "n1", json!({ "text": "from the editor" }))?;
    s.ok(
        "the same id and body again answer the record, and append nothing",
        r.status == 200 && r.body["replayed"] == true && r.body["record"]["seq"] == 1 && records(api, &owner, &name, "notes").len() == 2,
        &r,
    );
    let r = post(api, &editor, &name, "notes", "n1", json!({ "text": "changed my mind" }))?;
    s.ok(
        "the same id with another body is 409, and appends nothing",
        r.code() == Some(ErrorCode::ConflictingBody) && records(api, &owner, &name, "notes").len() == 2,
        &r,
    );
    let r = post(api, &editor, &name, "chat", "n1", json!({ "text": "from the editor" }))?;
    s.ok("and so is the same id on another channel (a post is keyed by its poster and id)", r.code() == Some(ErrorCode::ConflictingBody) && records(api, &owner, &name, "chat").is_empty(), &r);
    let r = post(api, &owner, &name, "notes", "n1", json!({ "text": "from the editor" }))?;
    s.ok(
        "another poster's same id is their own: a new record",
        r.status == 200 && r.body["replayed"] == false && r.body["record"]["seq"] == 3 && r.body["record"]["principal"] == owner_id.as_str(),
        &r,
    );

    // the size: a string of n characters is n + 2 bytes of JSON
    let edge = limits::RECORD_BODY_MAX_BYTES - 2;
    let r = post(api, &editor, &name, "chat", "edge", json!("x".repeat(edge)))?;
    s.ok(
        "a body of exactly 64 KiB of JSON is appended",
        r.status == 200 && r.body["record"]["body"].to_string().len() == limits::RECORD_BODY_MAX_BYTES,
        format!("status {}", r.status),
    );
    let r = post(api, &editor, &name, "chat", "over", json!("x".repeat(edge + 1)))?;
    s.ok(
        "a byte over it is 413, and appends nothing",
        r.status == 413 && r.code() == Some(ErrorCode::TooLarge) && records(api, &owner, &name, "chat").len() == 1,
        &r,
    );

    // a channel that takes no posts, and ones that are not the app's
    let r = post(api, &owner, &name, "quiet", "q1", json!({ "text": "hush" }))?;
    s.ok(
        "a channel without a post role refuses every post, the owner's too (403)",
        r.status == 403 && r.message().contains("takes no posts") && records(api, &owner, &name, "quiet").is_empty(),
        &r,
    );
    let r = post(api, &owner, &name, "events", "e1", json!({ "summary": "forged" }))?;
    let unknown = post(api, &owner, &name, "nowhere", "u1", json!({}))?;
    s.ok("a platform channel refuses posts (403), and an undeclared one is 404", r.status == 403 && unknown.status == 404, format!("{r} {unknown}"));
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/channels"), None)?;
    let list = r.body["channels"].as_array().cloned().unwrap_or_default();
    let post_of = |n: &str| list.iter().find(|c| c["name"] == n).map(|c| c["post"].clone()).unwrap_or(json!("missing"));
    s.ok(
        "the channel list names who may post to each",
        post_of("chat") == "public" && post_of("notes") == "editor" && post_of("quiet").is_null() && post_of("events").is_null(),
        &r,
    );

    // the site's door: a visitor posts as their anonymous principal
    let r = site_post(api, &name, "chat", "v1", json!({ "text": "from a visitor" }), None, "application/json")?;
    let cookie = r.cookies().into_iter().find(|c| c.starts_with("fragment_anon=")).unwrap_or_default();
    s.ok(
        "a visitor posts from the site, as their anonymous principal (and gets its cookie)",
        r.status == 200 && r.body["result"]["principal"].as_str().is_some_and(|p| p.starts_with("anon:")) && !cookie.is_empty(),
        &r,
    );
    let again = site_post(api, &name, "chat", "v1", json!({ "text": "from a visitor" }), Some(&cookie), "application/json")?;
    s.ok("the visitor's retry is the same record", again.body["replayed"] == true && again.body["result"]["seq"] == r.body["result"]["seq"], &again);
    let form = site_post(api, &name, "chat", "v2", json!({ "text": "a form" }), Some(&cookie), "application/x-www-form-urlencoded")?;
    let signed = api.signed(&owner, "POST", &format!("/api/f/{name}/ops/channels%2Fchat"), Some(&json!({ "id": "o1", "input": {} })))?;
    s.ok(
        "a site post that is not JSON is refused (no cross-site forms), and the door is the site's alone",
        form.status == 400 && signed.code() == Some(ErrorCode::NoCode) && records(api, &owner, &name, "chat").len() == 2,
        format!("{form} {signed}"),
    );

    // the deploy check
    s.commit(&c, &[("fragment.json", Some(br#"{"channels":{"chat":{"read":"viewer","post":"public"}}}"#))]);
    s.deploy(&c);
    let status = api.status(&owner, &name)?;
    s.ok(
        "a post role looser than the channel's read is refused at deploy, saying why",
        status.body["code"]["error"].as_str().is_some_and(|e| e.contains("channels.chat.post (public) is looser than its read (viewer)")),
        &status.body["code"],
    );
    let r = post(api, &stranger, &name, "chat", "s1", json!({ "text": "still open" }))?;
    s.ok("and the channels deployed before it stand", r.status == 200, &r);

    heard(s, api, &owner, &stranger)?;
    retention(s, api, &owner)
}

/// A channel people may post to keeps its newest `limits::POSTED_KEPT`
/// records (the oldest go, whoever appended them), as `events` does: a
/// public chat would otherwise grow for good. Filled by a mutation, 64
/// records a call, then one post more.
fn retention(s: &mut Suite, api: &Api, owner: &Keys) -> Result<()> {
    let name = s.named(api, owner, "wall")?;
    let c = s.create(api, owner, &name)?;
    ship(s, &c, WALL_APP, WALL_JSON);
    let fill = |id: &str, n: i64| api.op(owner, &name, "fill", id, json!({ "n": n }));
    // the deploy lands by the webhook: the first fill again until it has
    s.eventually(Duration::from_secs(15), || fill("f0", 64).is_ok_and(|r| r.status == 200));
    let kept = limits::POSTED_KEPT;
    let calls = kept / 64 + 1;
    let mut failed = None;
    for i in 1..calls {
        let r = fill(&format!("f{i}"), 64)?;
        if r.status != 200 {
            failed = Some(r.to_string());
            break;
        }
    }
    let appended = calls * 64;
    let first = |api: &Api| records(api, owner, &name, "wall").first().and_then(|r| r["seq"].as_i64());
    let newest = || -> Option<i64> {
        let r = api.signed(owner, "GET", &format!("/api/f/{name}/channels"), None).ok()?;
        r.body["channels"].as_array()?.iter().find(|c| c["name"] == "wall")?["seq"].as_i64()
    };
    s.ok(
        &format!("a postable channel keeps its newest {kept} records: past them, the oldest go"),
        failed.is_none() && newest() == Some(appended) && first(api) == Some(appended - kept + 1),
        format!("{failed:?}; newest {:?}, oldest kept {:?}", newest(), first(api)),
    );
    let r = post(api, owner, &name, "wall", "last", json!({ "text": "one more" }))?;
    s.ok(
        "a post past them drops one more of the oldest",
        r.status == 200 && r.body["record"]["seq"] == appended + 1 && first(api) == Some(appended - kept + 2),
        format!("{} oldest kept {:?}", r.status, first(api)),
    );
    Ok(())
}

/// Who hears a post (a subscriber's socket, a subscribed URL, the triggers
/// on its channel), and the public call budget a visitor's posts spend.
fn heard(s: &mut Suite, api: &Api, owner: &Keys, stranger: &Keys) -> Result<()> {
    let name = s.named(api, owner, "heard")?;
    let c = s.create(api, owner, &name)?;
    api.signed(owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    ship(s, &c, HEARD_APP, HEARD_JSON);
    let stranger_id = api.identity(stranger)?;
    let mut socket = Socket::open(api, &name, "__live", None, None)?;
    socket.until("hello", 5)?;
    socket.send(&json!({ "type": "subscribe", "channel": "chat", "after": 0 }))?;
    // (on to the checks, which say what is missing, even if it never subscribes)
    let _ = socket.until("subscribed", 5);
    let r = api.signed(owner, "POST", &format!("/api/f/{name}/subscriptions"), Some(&json!({ "channel": "chat", "url": format!("{}/notify", s.push.url) })))?;
    s.ok("(the owner subscribes a URL to the channel)", r.status == 200, &r);

    let r = post(api, stranger, &name, "chat", "hello", json!({ "text": "hello, room" }))?;
    let live = socket.until("record", 5).unwrap_or_else(|e| json!({ "error": e.to_string() }));
    s.ok(
        "a posted record reaches a subscriber's socket live, naming its poster",
        r.status == 200 && live["channel"] == "chat" && live["body"]["text"] == "hello, room" && live["principal"] == stranger_id.as_str(),
        &live,
    );
    let delivered = s.eventually(Duration::from_secs(15), || s.push.notified().iter().any(|f| f["type"] == "record" && f["record"]["body"]["text"] == "hello, room"));
    s.ok("and a URL subscribed to the channel (as an agent's is)", delivered, "");
    let log = || api.op(owner, &name, "log", "q", json!({})).map(|r| r.body["result"]["heard"].clone()).unwrap_or_default();
    let triggered = s.eventually(Duration::from_secs(15), || {
        let heard = log();
        heard.as_array().is_some_and(|h| h.len() == 1 && h[0]["body"]["text"] == "hello, room" && h[0]["principal"] == stranger_id.as_str())
    });
    s.ok("and starts the run its channel's trigger names, with the record", triggered, log());
    let runs = || api.signed(owner, "GET", &format!("/api/f/{name}/runs?op=heard"), None).map(|r| r.body["runs"].clone()).unwrap_or_default();
    let r = post(api, stranger, &name, "chat", "hello", json!({ "text": "hello, room" }))?;
    std::thread::sleep(Duration::from_secs(1));
    let started = runs();
    s.ok(
        "a replayed post starts no run again",
        r.body["replayed"] == true && started.as_array().is_some_and(|a| a.len() == 1 && a[0]["via"] == "channel" && a[0]["depth"] == 1),
        &started,
    );

    // A visitor's posts spend the public call budget with their calls:
    // exactly PUBLIC_CALLS_PER_MIN a clock minute, then 429. A burst that
    // crosses a minute's boundary counts across two budgets and proves
    // nothing exact, so it runs once more with another visitor.
    let limit = limits::PUBLIC_CALLS_PER_MIN as usize;
    let mut burst = None;
    for attempt in 0..2 {
        let minute = now_s() / 60;
        let visitor = api.person()?;
        let mut allowed = 0;
        let mut refusal = None;
        for i in 0..=limit {
            // posts and calls by turns: one budget
            let r = if i % 2 == 0 {
                post(api, &visitor, &name, "lobby", &format!("b{attempt}-{i}"), json!({ "i": i }))?
            } else {
                api.op(&visitor, &name, "log", &format!("b{attempt}-{i}"), json!({}))?
            };
            if r.status != 200 {
                refusal = Some((i % 2 == 0, r));
                break;
            }
            allowed += 1;
        }
        if now_s() / 60 == minute {
            burst = Some((allowed, refusal));
            break;
        }
    }
    let exact = burst.as_ref().is_some_and(|(allowed, refusal)| {
        *allowed == limit && refusal.as_ref().is_some_and(|(posted, r)| *posted && r.code() == Some(ErrorCode::RateLimited))
    });
    let detail = match &burst {
        None => "both bursts crossed a minute's boundary".to_string(),
        Some((allowed, refusal)) => format!("{allowed} allowed within one minute, then {:?}", refusal.as_ref().map(|(posted, r)| (posted, r.to_string()))),
    };
    s.ok(&format!("a visitor's posts spend the public call budget with their calls: {limit} a minute, then a post is 429"), exact, detail);
    let mut member = 0;
    for i in 0..=limit {
        if post(api, owner, &name, "notes", &format!("m{i}"), json!({ "i": i }))?.status == 200 {
            member += 1;
        }
    }
    s.ok(&format!("a member's posts spend none of it ({} in a row)", limit + 1), member == limit + 1, member);
    Ok(())
}

/// The `browser` section's posts: `fragment.post` from a page of a room
/// with no app code, heard live by another page.
pub fn browser(s: &mut Suite, api: &Api, chrome: &mut Lease) -> Result<()> {
    let owner = api.person()?;
    let (name, _) = room(s, api, &owner, "room-page", true)?;
    let wait = Duration::from_secs(15);
    let url = api.site_url(&name, "");
    let a = chrome.open(&url)?;
    let b = chrome.open(&url)?;
    let loaded = chrome.until(&a, "document.readyState === 'complete'", wait) && chrome.until(&b, "document.readyState === 'complete'", wait);
    s.ok("(two pages of a room with no app code open)", loaded, "");
    let lib = "import(new URL('./__fragment.js', location.href).href)";
    chrome.eval(&b, &format!("{lib}.then((f) => {{ window.__heard = []; f.subscribe('chat', (r) => window.__heard.push(r)); return f.me(); }})"))?;
    let record = chrome.eval(&a, &format!("{lib}.then((f) => f.post('chat', {{ text: 'from a page' }}, {{ id: 'page-1' }}))"))?;
    s.ok(
        "a page posts with fragment.post, as its visitor, and gets the record",
        record["channel"] == "chat" && record["body"]["text"] == "from a page" && record["principal"].as_str().is_some_and(|p| p.starts_with("anon:")),
        &record,
    );
    s.ok("another page following the channel gets it live", chrome.until(&b, "window.__heard.some((r) => r.body.text === 'from a page')", wait), "");
    let again = chrome.eval(&a, &format!("{lib}.then((f) => f.post('chat', {{ text: 'from a page' }}, {{ id: 'page-1' }}))"))?;
    let held = records(api, &owner, &name, "chat");
    s.ok("the same id again is the same record, appended once", again["seq"] == record["seq"] && held.len() == 1, json!(held));
    let refused = chrome.eval(&a, &format!("{lib}.then((f) => f.post('quiet', {{ text: 'hush' }}).then(() => 'posted', (e) => `${{e.status}} ${{e.code}}`))"))?;
    s.ok("a post to a channel without a post role rejects with the refusal", refused == "403 forbidden", &refused);
    chrome.close(a)?;
    chrome.close(b)?;
    Ok(())
}
