//! The author-facing API (slice C): input schemas, channels and the effects
//! that fill them, the live socket, custom routes, the CLI's call and
//! channel, and the todo template driven in a real browser.

use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use fragment_proto::limits;
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{Api, Call, Socket};
use crate::browser::Browser;
use crate::Suite;

const CHAT_APP: &[u8] = include_bytes!("../../fixtures/chat.mjs");
const CHAT_JSON: &[u8] = include_bytes!("../../fixtures/chat.json");
const FORMAT_MJS: &[u8] = include_bytes!("../../fixtures/format.mjs");

/// A public chat fragment with its applib module deployed.
fn chat(s: &Suite, api: &Api, owner: &Keys, base: &str) -> Result<(String, Value)> {
    let name = s.named(api, owner, base)?;
    let c = s.create(api, owner, &name)?;
    api.signed(owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    s.commit(&c, &[("applib/format.mjs", Some(FORMAT_MJS))]);
    ship(s, &c, CHAT_APP, CHAT_JSON);
    Ok((name, c))
}

fn records(api: &Api, keys: &Keys, name: &str, channel: &str, after: i64) -> Result<crate::api::Reply> {
    api.signed(keys, "GET", &format!("/api/f/{name}/channels/{channel}?after={after}"), None)
}

pub fn schemas(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("schemas") {
        return Ok(());
    }
    let owner = api.person()?;
    let (name, c) = chat(s, api, &owner, "schemas")?;
    let r = api.op(&owner, &name, "say", "v1", json!({ "text": "hi" }))?;
    s.ok("an input that fits the schema runs", r.status == 200, &r);
    let r = api.op(&owner, &name, "say", "v2", json!({}))?;
    s.ok("a missing required field is 400 naming it", r.status == 400 && r.message() == "input /text: is required", &r);
    let r = api.op(&owner, &name, "say", "v3", json!({ "text": 7 }))?;
    s.ok("a wrong type is 400", r.status == 400 && r.message().contains("/text: must be string"), &r);
    let r = api.op(&owner, &name, "say", "v4", json!({ "text": "hi", "loud": true }))?;
    s.ok("an extra property is 400", r.status == 400 && r.message().contains("/loud: is not an allowed property"), &r);
    let r = api.op(&owner, &name, "say", "v5", json!({ "text": "x".repeat(101) }))?;
    s.ok("a string over maxLength is 400", r.status == 400 && r.message().contains("at most 100 characters"), &r);
    let r = api.op(&owner, &name, "count", "q", json!({}))?;
    s.ok("refused inputs write nothing", r.body["result"]["n"] == 1, &r);
    let r = api.status(&owner, &name)?;
    s.ok("status shows each operation's schema", r.body["code"]["operations"]["say"]["input"]["required"] == json!(["text"]), &r.body["code"]);
    let live = r.body["code"]["sha"].clone();
    s.commit(&c, &[("fragment.json", Some(br#"{"operations":{"say":{"kind":"mutation","input":{"type":"object","properties":{"text":{"pattern":"^a"}}}}}}"#))]);
    s.deploy(&c);
    let r = api.status(&owner, &name)?;
    s.ok(
        "a schema outside the subset is refused at deploy, naming the keyword",
        r.body["code"]["sha"] == live && r.body["code"]["error"].as_str().is_some_and(|e| e.contains("pattern")),
        &r.body["code"],
    );
    s.commit(&c, &[("fragment.json", Some(br#"{"operations":{"fetch":{"kind":"query"}}}"#))]);
    s.deploy(&c);
    let r = api.status(&owner, &name)?;
    s.ok("an operation named fetch is refused (it is the App's route handler)", r.body["code"]["error"].as_str().is_some_and(|e| e.contains("reserved")), &r.body["code"]);
    Ok(())
}

pub fn channels(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("channels") {
        return Ok(());
    }
    let owner = api.person()?;
    let viewer = api.person()?;
    let stranger = api.person()?;
    let (name, _) = chat(s, api, &owner, "channels")?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", viewer.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    let owner_id = api.identity(&owner)?;

    let r = api.op(&owner, &name, "say", "c1", json!({ "text": "one" }))?;
    s.ok("a mutation publishes", r.status == 200, &r);
    api.op(&owner, &name, "say", "c2", json!({ "text": "two" }))?;
    let r = records(api, &owner, &name, "room", 0)?;
    let recs = r.body["records"].as_array().cloned().unwrap_or_default();
    s.ok("the channel holds its records in order", recs.len() == 2 && recs[0]["seq"] == 1 && recs[1]["body"]["text"] == "two", &r);
    s.ok("a record names who published it and its kind", recs[0]["principal"] == owner_id.as_str() && recs[0]["kind"] == "said", &r);
    let r = records(api, &owner, &name, "room", 1)?;
    s.ok("after= reads from a cursor", r.body["records"].as_array().map_or(0, |a| a.len()) == 1 && r.body["next"] == 2, &r);
    let r = api.op(&owner, &name, "say", "c1", json!({ "text": "one" }))?;
    s.ok("a replay publishes nothing again", r.body["replayed"] == true && records(api, &owner, &name, "room", 0)?.body["records"].as_array().map_or(0, |a| a.len()) == 2, &r);
    let r = records(api, &owner, &name, "ops", 0)?;
    let ops: Vec<&Value> = r.body["records"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    s.ok("each applied mutation is one ops record", ops.len() == 2 && ops[0]["body"] == json!({ "op": "say", "id": "c1" }), &r);

    let r = api.op(&owner, &name, "undeclared", "u1", json!({ "text": "lost" }))?;
    s.ok("publishing to an undeclared channel refuses the mutation", r.status == 422 && r.message().contains("not declared"), &r);
    let r = api.op(&owner, &name, "huge", "h1", json!({}))?;
    s.ok("a record over 64 KiB refuses the mutation", r.status == 422 && r.message().contains("at most 65536 bytes"), &r);
    let r = api.op(&owner, &name, "count", "q", json!({}))?;
    s.ok("a refused mutation's writes roll back", r.body["result"]["n"] == 2, &r);

    // author code shares a realm with the in-app checks; the supervisor's hold
    let r = api.op(&owner, &name, "reach", "x1", json!({}))?;
    s.ok("author code gets no reference to the effects list (the mutation fails and rolls back)", r.status == 422 && r.error() == "app_failed", &r);
    let r = api.op(&owner, &name, "count", "q", json!({}))?;
    s.ok("and writes nothing", r.body["result"]["n"] == 2, &r);
    let r = api.op(&owner, &name, "patched", "x2", json!({}))?;
    s.ok(
        "a record for events that passes a patched in-app check is refused by the supervisor",
        r.status == 422 && r.message().contains("refused its effects") && r.message().contains("\"events\" is the platform's"),
        &r,
    );
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/events"), None)?;
    let events = r.body["events"].as_array().cloned().unwrap_or_default();
    s.ok(
        "nothing forged reaches the audit trail, and the refusal is in it",
        !events.iter().any(|e| e["kind"] == "forged" || e["summary"] == "forged")
            && events.iter().any(|e| e["kind"] == "effects.refused" && e["data"]["op"] == "patched" && e["data"]["id"] == "x2"),
        &r,
    );
    let r = api.op(&owner, &name, "patched", "x2", json!({}))?;
    s.ok("its replay answers the committed result and refuses nothing again", r.status == 200 && r.body["replayed"] == true, &r);
    let r = api.op(&owner, &name, "say", "c3", json!({ "text": "still here" }))?;
    s.ok("the app goes on after a refusal", r.status == 200 && records(api, &owner, &name, "room", 2)?.body["records"][0]["body"]["text"] == "still here", &r);

    api.op(&owner, &name, "note", "n1", json!({ "text": "staff only" }))?;
    let r = records(api, &viewer, &name, "staff", 0)?;
    s.ok("an editors channel is refused to a viewer", r.status == 403, &r);
    let r = records(api, &owner, &name, "staff", 0)?;
    s.ok("and read by an editor", r.status == 200 && r.body["records"][0]["body"]["text"] == "staff only", &r);
    let r = records(api, &stranger, &name, "room", 0)?;
    s.ok("a public channel on a public fragment is read by anyone", r.status == 200, &r);
    let r = records(api, &stranger, &name, "ops", 0)?;
    s.ok("the ops channel is for viewers", r.status == 403, &r);
    let r = records(api, &owner, &name, "nope", 0)?;
    s.ok("an unknown channel is 404", r.status == 404, &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/channels"), None)?;
    let list = r.body["channels"].as_array().cloned().unwrap_or_default();
    let read_of = |n: &str| list.iter().find(|c| c["name"] == n).map(|c| c["read"].clone()).unwrap_or(Value::Null);
    s.ok(
        "the channel list names each channel's reader and position",
        read_of("room") == "public" && read_of("staff") == "editor" && read_of("events") == "viewer" && list.iter().any(|c| c["name"] == "room" && c["seq"] == 3),
        &r,
    );
    let r = api.op(&viewer, &name, "whoami", "w", json!({}))?;
    s.ok("a method sees who called it and their role", r.body["result"] == json!({ "principal": api.identity(&viewer)?, "role": "viewer" }), &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/events"), None)?;
    s.ok("the event log reads from the events channel", r.status == 200 && r.body["events"][0]["kind"] == "create", &r);
    Ok(())
}

pub fn live(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("live") {
        return Ok(());
    }
    let owner = api.person()?;
    let (name, _) = chat(s, api, &owner, "live")?;
    api.op(&owner, &name, "say", "l1", json!({ "text": "before" }))?;

    let mut a = Socket::open(api, &name, "__live", None, None)?;
    let hello = a.next()?;
    s.ok("a visitor's socket says hello with its role", hello["type"] == "hello" && hello["role"] == "public" && hello["principal"].as_str().is_some_and(|p| p.starts_with("anon:")), &hello);
    a.send(&json!({ "type": "subscribe", "channel": "room", "after": 0 }))?;
    let first = a.until("record", 5)?;
    let sub = a.until("subscribed", 5)?;
    s.ok("subscribing replays the backlog, then says where it is", first["body"]["text"] == "before" && sub["next"] == 1 && sub["more"] == false, &sub);
    api.op(&owner, &name, "say", "l2", json!({ "text": "live one" }))?;
    let rec = a.until("record", 5)?;
    s.ok("a new record arrives live", rec["body"]["text"] == "live one" && rec["seq"] == 2, &rec);
    let changed = a.until("changed", 5)?;
    s.ok("and a change signal for live queries", changed["op"] == "say", &changed);
    a.send(&json!({ "type": "subscribe", "channel": "staff", "after": 0 }))?;
    let err = a.until("error", 5)?;
    s.ok("a channel above the socket's role is refused", err["message"].as_str().is_some_and(|m| m.contains("editor")), &err);
    a.close();

    // resume from a cursor
    api.op(&owner, &name, "say", "l3", json!({ "text": "while away" }))?;
    let mut b = Socket::open(api, &name, "__live", None, None)?;
    b.send(&json!({ "type": "subscribe", "channel": "room", "after": 2 }))?;
    let rec = b.until("record", 5)?;
    let sub = b.until("subscribed", 5)?;
    s.ok("a reconnect resumes after its cursor", rec["body"]["text"] == "while away" && sub["next"] == 3, &sub);

    // presence
    let mut c = Socket::open(api, &name, "__live", Some(&owner), None)?;
    let c_hello = c.next()?;
    s.ok("a signed member's socket has their role", c_hello["role"] == "owner", &c_hello);
    c.send(&json!({ "type": "presence", "data": { "name": "owner" } }))?;
    let seen = b.until("presence", 10)?;
    s.ok("presence reaches the others", seen["list"].as_array().is_some_and(|l| l.iter().any(|p| p["data"]["name"] == "owner")), &seen);
    c.send(&json!({ "type": "presence", "data": "x".repeat(5000) }))?;
    let err = c.until("error", 5)?;
    s.ok("presence data over 4 KiB is refused", err["message"].as_str().is_some_and(|m| m.contains("4096")), &err);
    c.close();
    let gone = b.until("presence", 10)?;
    s.ok("presence leaves when the socket closes", gone["list"] == json!([]), &gone);
    b.send(&json!({ "type": "unsubscribe", "channel": "room" }))?;
    api.op(&owner, &name, "say", "l4", json!({ "text": "after unsubscribe" }))?;
    let next = b.until("changed", 5)?;
    s.ok("unsubscribing stops the records (the change signal still comes)", next["type"] == "changed", &next);

    // a visitor's socket closes when the fragment stops being public
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "link" })))?;
    let closed = loop {
        match b.next() {
            Ok(_) => continue,
            Err(e) => break e.to_string(),
        }
    };
    s.ok("a visitor's socket closes when the fragment stops being public (4003)", closed.contains("4003"), &closed);
    let r = Socket::open(api, &name, "__live", None, None);
    s.ok("and a new anonymous socket is refused", r.is_err(), "");

    // a backlog past a page, by count and then by bytes: page after page
    // until the last, then live; a record appended while the client pages
    // comes in its turn, never ahead of the pages it has not read
    let (paged, _) = chat(s, api, &owner, "pages")?;
    let total = fill_past_a_page(api, &owner, &paged, true)?;
    let mut p = Socket::open(api, &paged, "__live", None, None)?;
    p.until("hello", 5)?;
    p.send(&json!({ "type": "subscribe", "channel": "room", "after": 0 }))?;
    let (mut seqs, mut pages, mut appended) = (vec![], 0, false);
    for _ in 0..total as usize + 100 {
        let f = p.next()?;
        match f["type"].as_str() {
            Some("record") => seqs.push(f["seq"].as_i64().unwrap_or(0)),
            Some("subscribed") => {
                pages += 1;
                if f["more"] != true {
                    break;
                }
                if !appended {
                    api.op(&owner, &paged, "say", "between", json!({ "text": "between pages" }))?;
                    appended = true;
                }
                p.send(&json!({ "type": "subscribe", "channel": "room", "after": f["next"] }))?;
            }
            _ => {}
        }
    }
    api.op(&owner, &paged, "say", "after", json!({ "text": "live after the pages" }))?;
    let rec = p.until("record", 10)?;
    seqs.push(rec["seq"].as_i64().unwrap_or(0));
    let want: Vec<i64> = (1..=total + 2).collect();
    let gap = seqs.iter().zip(&want).position(|(a, b)| a != b);
    s.ok(
        "a backlog past a page comes a page at a time, then live, with no gap and nothing twice",
        seqs == want && pages >= 3 && rec["body"]["text"] == "live after the pages",
        format!("{pages} pages, {} records, first difference at {gap:?}", seqs.len()),
    );
    let mut q = Socket::open(api, &paged, "__live", None, None)?;
    q.until("hello", 5)?;
    q.send(&json!({ "type": "subscribe", "channel": "room", "last": 5 }))?;
    let mut tail = vec![];
    let sub = loop {
        let f = q.until("record", 10).and_then(|f| if f["type"] == "record" { Ok(f) } else { anyhow::bail!("{f}") });
        match f {
            Ok(f) => tail.push(f["seq"].as_i64().unwrap_or(0)),
            Err(_) => break Value::Null,
        }
        if tail.len() == 5 {
            break q.until("subscribed", 5)?;
        }
    };
    s.ok(
        "last: N starts that many records before the end, then live",
        tail == (total - 2..=total + 2).collect::<Vec<_>>() && sub["more"] == false && sub["next"] == total + 2,
        format!("{tail:?} {sub}"),
    );
    Ok(())
}

/// Fills `room` past a page by count (more records than `CHANNEL_PAGE`)
/// and, when `bytes`, then past a page by size; answers how many records.
fn fill_past_a_page(api: &Api, keys: &Keys, name: &str, bytes: bool) -> Result<i64> {
    let mut total = 0;
    for i in 0..limits::CHANNEL_PAGE / limits::EFFECTS_MAX + 1 {
        let r = api.op(keys, name, "bulk", &format!("small-{i}"), json!({ "n": limits::EFFECTS_MAX, "size": 16 }))?;
        anyhow::ensure!(r.status == 200, "bulk: {r}");
        total += limits::EFFECTS_MAX as i64;
    }
    // records of 60 KiB, past the byte budget of one page
    let size = 60 * 1024;
    for i in 0..if bytes { limits::CHANNEL_PAGE_MAX_BYTES / size + 3 } else { 0 } {
        let r = api.op(keys, name, "bulk", &format!("big-{i}"), json!({ "n": 1, "size": size }))?;
        anyhow::ensure!(r.status == 200, "bulk: {r}");
        total += 1;
    }
    Ok(total)
}

pub fn routes(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("routes") {
        return Ok(());
    }
    let owner = api.person()?;
    let (name, c) = chat(s, api, &owner, "routes")?;
    s.commit(&c, &[("site/index.html", Some(b"<p>the site</p>"))]);
    s.deploy(&c);
    let r = api.page(&name, "", None)?;
    s.ok("a site file wins over the app's routes", r.status == 200 && r.text.contains("the site"), &r);
    let r = api.page(&name, "hello", None)?;
    s.ok("any other path goes to the App's fetch", r.status == 200 && r.text.contains("HELLO!"), &r);
    s.ok("which imports applib/ modules", r.text.contains("HELLO!"), &r);
    s.ok("and sees who is asking", r.text.contains("anonymous as public at /hello"), &r);
    let r = api.call(Call { method: "GET", url: api.site_url(&name, "hello"), keys: Some(&owner), ..Call::default() })?;
    s.ok("a signed member is named to the app", r.text.contains(&api.identity(&owner)?) && r.text.contains("as owner"), &r);
    let r = api.call(Call { method: "POST", url: api.site_url(&name, "echo"), body: Some(b"ping".to_vec()), content_type: Some("text/plain"), ..Call::default() })?;
    s.ok("the app's routes take other methods", r.status == 200 && r.text == "echo: ping", &r);
    let r = api.page(&name, "missing", None)?;
    s.ok("the app answers its own 404s", r.status == 404 && r.text.contains("no such route"), &r);
    let r = api.page(&name, "__nothing", None)?;
    s.ok("__ paths never reach the app", r.status == 404 && !r.text.contains("no such route"), &r);
    Ok(())
}

pub fn cli(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("cli") {
        return Ok(());
    }
    let home = s.dir("cli-home");
    s.login(api, &home);
    let keys = s.cli_keys(&home).expect("the CLI logged in");
    let (name, c) = chat(s, api, &keys, "cli")?;
    let r = s.cli_json(api, &home, &["call", &name, "say", "--input", r#"{"text":"from the cli"}"#, "--id", "cli-1", "--json"])?;
    s.ok("fragment call runs an operation", r["result"]["id"] == 1 && r["replayed"] == false, &r);
    let r = s.cli_json(api, &home, &["call", &name, "say", "--input", r#"{"text":"from the cli"}"#, "--id", "cli-1", "--json"])?;
    s.ok("the same --id replays", r["replayed"] == true, &r);
    let out = s.cli(api, &home, &["call", &name, "say", "--input", "{}"]);
    s.ok("a schema refusal is reported", !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("/text: is required"), String::from_utf8_lossy(&out.stderr));
    let r = s.cli_json(api, &home, &["channel", &name, "--json"])?;
    s.ok("fragment channel lists the channels", r["channels"].as_array().is_some_and(|a| a.iter().any(|c| c["name"] == "room")), &r);
    let r = s.cli_json(api, &home, &["channel", &name, "room", "--json"])?;
    s.ok("fragment channel reads one", r["records"][0]["body"]["text"] == "from the cli", &r);
    let log = s.scratch.join(format!("follow-{name}.log"));
    let mut follow = Command::new(&s.cli)
        .args(["channel", &name, "room", "--follow", "--after", "1"])
        .env("HOME", &home)
        .env("FRAGMENT_HOST", &api.base)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&log)?)
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::sleep(Duration::from_millis(1500));
    api.op(&keys, &name, "say", "cli-2", json!({ "text": "followed" }))?;
    let seen = s.eventually(Duration::from_secs(10), || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("followed")));
    let _ = follow.kill();
    let _ = follow.wait();
    s.ok("fragment channel --follow streams new records", seen, std::fs::read_to_string(&log).unwrap_or_default());

    // --follow from the start of a channel past a page: every record in
    // order, page after page, then the live one
    fill_past_a_page(api, &keys, &name, false)?;
    let listed = s.cli_json(api, &home, &["channel", &name, "--json"])?;
    let head = listed["channels"].as_array().and_then(|a| a.iter().find(|c| c["name"] == "room")).and_then(|c| c["seq"].as_i64()).unwrap_or(0);
    let log = s.scratch.join(format!("follow-all-{name}.log"));
    let mut follow = Command::new(&s.cli)
        .args(["channel", &name, "room", "--follow"])
        .env("HOME", &home)
        .env("FRAGMENT_HOST", &api.base)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&log)?)
        .stderr(Stdio::null())
        .spawn()?;
    let lines = |log: &std::path::Path| std::fs::read_to_string(log).unwrap_or_default().lines().filter(|l| l.contains(r#""type":"record""#)).count() as i64;
    let backlog = s.eventually(Duration::from_secs(20), || lines(&log) >= head);
    api.op(&keys, &name, "say", "cli-3", json!({ "text": "after the pages" }))?;
    let live = s.eventually(Duration::from_secs(10), || lines(&log) > head);
    let _ = follow.kill();
    let _ = follow.wait();
    let seqs: Vec<i64> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter_map(|v| v["seq"].as_i64())
        .collect();
    s.ok(
        "fragment channel --follow pages through a long backlog, then streams, with no gap",
        backlog && live && head > limits::CHANNEL_PAGE as i64 && seqs == (1..=head + 1).collect::<Vec<_>>(),
        format!("head {head}, {} lines", seqs.len()),
    );

    // rotate renews what the CLI names; the webhook secret is code.storage's and changes only when asked
    let r = s.cli_json(api, &home, &["rotate", &name, "--json"])?;
    s.ok(
        "fragment rotate renews the inbox token and the share link, not the webhook secret",
        r["rotated"] == json!(["inbox", "view"]) && r["view_token"] != c["viewToken"] && r["webhook_secret"] == c["webhookSecret"],
        json!({ "rotated": r["rotated"], "view_changed": r["view_token"] != c["viewToken"], "webhook_kept": r["webhook_secret"] == c["webhookSecret"] }),
    );
    Ok(())
}

const TODO_FILES: [(&str, &[u8]); 4] = [
    ("fragment.json", include_bytes!("../../../../templates/todo/fragment.json")),
    ("app.mjs", include_bytes!("../../../../templates/todo/app.mjs")),
    ("site/index.html", include_bytes!("../../../../templates/todo/site/index.html")),
    ("README.md", include_bytes!("../../../../templates/todo/README.md")),
];

pub fn browser(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("browser") {
        return Ok(());
    }
    // the template's list is bounded to its newest 500, oldest first as the page shows them
    let owner = api.person()?;
    let name = s.name("todo-cap");
    let c = s.create(api, &owner, &name)?;
    let changes: Vec<(&str, Option<&[u8]>)> = TODO_FILES.iter().map(|(p, b)| (*p, Some(*b))).collect();
    s.commit(&c, &changes);
    s.deploy(&c);
    for i in 1..=501 {
        let r = api.op(&owner, &name, "add", &format!("t{i}"), json!({ "text": format!("todo {i}") }))?;
        anyhow::ensure!(r.status == 200, "add {i}: {r}");
    }
    let r = api.op(&owner, &name, "list", "q", json!({}))?;
    let texts: Vec<&str> = r.body["result"]["todos"].as_array().map(|t| t.iter().filter_map(|t| t["text"].as_str()).collect()).unwrap_or_default();
    s.ok(
        "the todo template lists its newest 500, oldest first: the 501st todo shows, the first does not",
        texts.len() == 500 && texts.first() == Some(&"todo 2") && texts.last() == Some(&"todo 501"),
        format!("{} todos, first {:?}, last {:?}", texts.len(), texts.first(), texts.last()),
    );

    let Some(mut chrome) = Browser::launch(&s.scratch)? else {
        s.ok("Chrome is installed for the browser lane (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let name = s.named(api, &owner, "todo")?;
    let c = s.create(api, &owner, &name)?;
    s.commit(&c, &changes);
    s.deploy(&c);
    let url = api.site_url(&name, &format!("?view={}", c["viewToken"].as_str().unwrap_or("")));
    let wait = Duration::from_secs(15);

    let a = chrome.open(&url)?;
    s.ok("the todo page loads and connects", chrome.until(&a, "document.getElementById('here')?.textContent === 'just you here'", wait), "");
    let b = chrome.open(&url)?;
    s.ok("a second page sees the first", chrome.until(&b, "document.getElementById('here').textContent === 'you and 1 other here'", wait), "");
    s.ok("and the first sees the second", chrome.until(&a, "document.getElementById('here').textContent === 'you and 1 other here'", wait), "");
    chrome.eval(&a, "document.getElementById('text').value = 'milk'; document.getElementById('add').requestSubmit(); true")?;
    s.ok("adding in one page shows it there", chrome.until(&a, "[...document.querySelectorAll('#todos li span')].some(s => s.textContent === 'milk')", wait), "");
    s.ok("and live in the other", chrome.until(&b, "[...document.querySelectorAll('#todos li span')].some(s => s.textContent === 'milk')", wait), "");
    s.ok("the activity feed follows the channel", chrome.until(&b, "document.getElementById('activity').textContent.includes('added “milk”')", wait), "");
    chrome.eval(&b, "document.querySelector('#todos li input').click(); true")?;
    s.ok("toggling in the other page crosses it off in the first", chrome.until(&a, "document.querySelector('#todos li')?.className === 'done'", wait), "");
    chrome.eval(&a, "document.getElementById('text').value = ''.padEnd(201, 'x'); document.getElementById('text').removeAttribute('maxlength'); document.getElementById('add').requestSubmit(); true")?;
    s.ok("a refused input shows the schema's reason", chrome.until(&a, "document.getElementById('error').textContent.includes('at most 200 characters')", wait), "");
    chrome.close(b)?;
    s.ok("closing a page leaves the other alone", chrome.until(&a, "document.getElementById('here').textContent === 'just you here'", wait), "");
    chrome.reload(&a)?;
    s.ok("a reload finds the list and the activity where they were", chrome.until(&a, "document.querySelectorAll('#todos li.done').length === 1 && document.getElementById('activity').children.length === 2", wait), "");
    let n = api.op(&owner, &name, "list", "q", json!({}))?;
    s.ok("the browser's writes are the app's data", n.body["result"]["todos"].as_array().is_some_and(|t| t.len() == 1 && t[0]["done"] == true), &n);

    // the server drops the page's socket: it comes back by itself and
    // catches up on what it missed (counted from here on)
    chrome.eval(&a, "(() => { window.__sockets = 0; const W = window.WebSocket; window.WebSocket = class extends W { constructor(...args) { super(...args); window.__sockets += 1; } }; return true; })()")?;
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "drop-live", "code": 4000 })))?;
    s.ok("(the test fleet drops the fragment's live sockets)", r.status == 200, &r);
    api.op(&owner, &name, "add", "while-dropped", json!({ "text": "bread" }))?;
    let bread = "[...document.querySelectorAll('#todos li span')].some(s => s.textContent === 'bread') && document.getElementById('activity').textContent.includes('added “bread”')";
    s.ok("a page whose socket the server dropped reconnects by itself and catches up", chrome.until(&a, &format!("window.__sockets === 1 && {bread}"), wait), "");
    let lines = chrome.eval(&a, "[...document.getElementById('activity').children].filter((d) => d.textContent === 'added “bread”').length")?;
    s.ok("and shows the record it missed once", lines == 1, &lines);

    // a close the fragment means for good (4003: rotating the share link
    // ends link holders' sockets): the page stops and says why
    chrome.eval(&a, "import(new URL('./__fragment.js', location.href).href).then((f) => { f.closed((e) => { window.__ended = e.code; }); return true; })")?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/rotate"), Some(&json!({ "scopes": ["view"] })))?;
    s.ok("(the share link rotates)", r.status == 200, &r);
    s.ok("a page closed for good (4003) is told so", chrome.until(&a, "window.__ended === 4003 && document.getElementById('here').textContent.includes('reload')", wait), "");
    // following a channel would open a socket at once if the page still tried
    let sockets = chrome.eval(&a, "import(new URL('./__fragment.js', location.href).href).then((f) => { f.subscribe('activity', () => {}); return window.__sockets; })")?;
    s.ok("and opens no socket again, even to follow a channel", sockets == 1, &sockets);
    Ok(())
}
