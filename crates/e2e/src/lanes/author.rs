//! The author-facing API (slice C): input schemas, channels and the effects
//! that fill them, the live socket, custom routes, the CLI's call and
//! channel, and the todo template driven in a real browser.

use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use fragment_nip98::Keys;
use fragment_proto::live::LiveOut;
use fragment_proto::{limits, ErrorCode};
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{Api, Call, Socket};
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
    // the facet never calls a reserved name, so the manifest may not declare one
    s.commit(&c, &[("fragment.json", Some(br#"{"operations":{"constructor":{"kind":"mutation"}}}"#))]);
    let refused = s.deploy(&c);
    let r = api.status(&owner, &name)?;
    s.ok(
        "an operation named constructor is refused at deploy, not answered unknown later",
        r.body["code"]["sha"] == live && r.body["code"]["error"].as_str().is_some_and(|e| e.contains(&refused[..12]) && e.contains("\"constructor\" is reserved")),
        &r.body["code"],
    );
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
    // a record's body is bounded in bytes of JSON; `bulk`'s is its
    // padding and this much around it
    let edge = limits::RECORD_BODY_MAX_BYTES - r#"{"i":0,"pad":""}"#.len();
    let r = api.op(&owner, &name, "bulk", "over", json!({ "n": 1, "size": edge + 1 }))?;
    s.ok(
        "a record one byte over its limit refuses the mutation",
        r.code() == Some(ErrorCode::AppFailed) && r.message().contains(&limits::RECORD_BODY_MAX_BYTES.to_string()),
        &r,
    );
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
    let r = api.op(&owner, &name, "bulk", "edge", json!({ "n": 1, "size": edge }))?;
    let size = records(api, &owner, &name, "room", 3)?.body["records"][0]["body"].to_string().len();
    s.ok("and a record of exactly the limit is published", r.status == 200 && size == limits::RECORD_BODY_MAX_BYTES, format!("{r}; a body of {size} bytes"));
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
    a.send(&json!({ "type": "ping" }))?;
    let pong = a.until("pong", 5)?;
    s.ok("a ping is answered with a pong", pong == json!({ "type": "pong" }), &pong);
    // each malformed frame is refused by its decode, naming what is wrong
    let mut refusals = vec![];
    for (frame, says) in [
        (json!({ "type": "subscribe", "after": 0 }), "missing field `channel`"),
        (json!({ "type": "subscribe", "channel": "room" }), "after or last"),
        (json!({ "type": "subscribe", "channel": "room", "after": 0, "last": 5 }), "not both"),
        (json!({ "type": "unsubscribe" }), "missing field `channel`"),
        (json!({ "type": "shout" }), "unknown variant `shout`"),
    ] {
        a.send(&frame)?;
        let err = a.until("error", 5)?;
        refusals.push((err["message"].as_str().is_some_and(|m| m.contains(says)), err));
    }
    a.send(&json!({ "type": "ping" }))?;
    let still = a.until("pong", 5)?;
    s.ok(
        "a malformed frame is an error naming what is wrong, and the socket stays open",
        refusals.iter().all(|(ok, _)| *ok) && still["type"] == "pong",
        format!("{refusals:?}"),
    );
    let sent = [&hello, &first, &sub, &rec, &changed, &err, &pong];
    s.ok(
        "every frame the socket sent decodes as the protocol's LiveOut",
        sent.iter().all(|f| serde_json::from_value::<LiveOut>((*f).clone()).is_ok()) && refusals.iter().all(|(_, f)| serde_json::from_value::<LiveOut>(f.clone()).is_ok()),
        format!("{sent:?}"),
    );
    a.close();

    // resume from a cursor
    api.op(&owner, &name, "say", "l3", json!({ "text": "while away" }))?;
    let mut b = Socket::open(api, &name, "__live", None, None)?;
    b.send(&json!({ "type": "subscribe", "channel": "room", "after": 2 }))?;
    let rec = b.until("record", 5)?;
    let sub = b.until("subscribed", 5)?;
    s.ok("a reconnect resumes after its cursor", rec["body"]["text"] == "while away" && sub["next"] == 3, &sub);
    // the object forgets what it knew of its sockets beyond their
    // attachments, as when it wakes from hibernation: it gathers them
    // again, and the socket that follows still gets the next record
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "forget-live" })))?;
    s.ok("(the test fleet forgets the fragment's live sockets, as waking does)", r.status == 200, &r);
    api.op(&owner, &name, "say", "l3b", json!({ "text": "after waking" }))?;
    let rec = b.until("record", 5)?;
    s.ok("a socket that followed before the object woke still gets records", rec["body"]["text"] == "after waking" && rec["seq"] == 4, &rec);

    // presence: whole in hello, then one socket's change at a time
    let mut c = Socket::open(api, &name, "__live", Some(&owner), None)?;
    let c_hello = c.next()?;
    s.ok("a signed member's socket has their role, and its hello says no one shares presence yet", c_hello["role"] == "owner" && c_hello["presence"] == json!([]), &c_hello);
    c.send(&json!({ "type": "presence", "data": { "name": "owner" } }))?;
    let seen = b.until("presence", 10)?;
    let owner_here = json!({ "id": c_hello["id"], "principal": c_hello["principal"], "data": { "name": "owner" } });
    s.ok(
        "a presence change reaches the others as that one socket's change",
        seen == json!({ "type": "presence", "id": c_hello["id"], "principal": c_hello["principal"], "data": { "name": "owner" } }),
        &seen,
    );
    let mut d = Socket::open(api, &name, "__live", None, None)?;
    let d_hello = d.next()?;
    s.ok("a socket that connects later hears who is here in its hello", d_hello["presence"] == json!([owner_here]), &d_hello);
    // presence data is bounded in bytes of JSON: a string of n characters
    // is n + 2
    let edge = limits::PRESENCE_MAX_BYTES - 2;
    c.send(&json!({ "type": "presence", "data": "x".repeat(edge) }))?;
    let at_limit = d.until("presence", 5).map(|p| p["data"].as_str().map(str::len));
    s.ok("presence data of exactly its limit reaches the others", matches!(at_limit, Ok(Some(n)) if n == edge), format!("{at_limit:?}"));
    c.send(&json!({ "type": "presence", "data": "x".repeat(edge + 1) }))?;
    let err = c.until("error", 5);
    s.ok(
        "and a byte over it is refused",
        err.as_ref().is_ok_and(|e| e["message"].as_str().is_some_and(|m| m.contains(&limits::PRESENCE_MAX_BYTES.to_string()))),
        format!("{err:?}"),
    );
    // a flood of changes: a burst goes through at once, then ten a second;
    // the rest are dropped, each with an error, and the others hear only
    // the ones that went through
    let flood = 3 * limits::PRESENCE_BURST;
    for i in 0..flood {
        c.send(&json!({ "type": "presence", "data": { "n": i } }))?;
    }
    let (mut through, mut dropped) = (0, 0);
    for _ in 0..flood {
        let f = c.next()?;
        match f["type"].as_str() {
            Some("presence") => through += 1,
            Some("error") if f["message"].as_str().is_some_and(|m| m.contains("10 a second")) => dropped += 1,
            _ => anyhow::bail!("a flood of presence answered {f}"),
        }
    }
    // once the pace allows again, one last change marks the end of the flood
    std::thread::sleep(Duration::from_millis(1100));
    c.send(&json!({ "type": "presence", "data": { "name": "owner", "after": "the flood" } }))?;
    let mut heard = 0;
    for _ in 0..flood + 1 {
        let f = d.until("presence", 5)?;
        if f["data"]["after"] == "the flood" {
            break;
        }
        heard += 1;
    }
    s.ok(
        "presence faster than ten a second is dropped past a burst, and the others hear only what went through",
        through > 0 && dropped > 0 && through + dropped == flood && heard == through,
        format!("{through} went through, {dropped} dropped, the others heard {heard}"),
    );
    c.close();
    // past the flood's changes that went through (b heard them too)
    let mut gone = Value::Null;
    for _ in 0..flood + 2 {
        gone = b.until("presence", 5)?;
        if gone["data"].is_null() {
            break;
        }
    }
    s.ok("presence leaves when the socket closes", gone == json!({ "type": "presence", "id": c_hello["id"], "principal": c_hello["principal"], "data": null }), &gone);
    s.ok(
        "presence frames decode as LiveOut too",
        [&seen, &gone, &d_hello].iter().all(|f| serde_json::from_value::<LiveOut>((*f).clone()).is_ok()),
        format!("{seen} {gone} {d_hello}"),
    );
    d.close();

    // A page loaded before presence came one change a frame (its library
    // connects without `?v=2`) hears the whole list, as it did, while a
    // current page hears the change; and a visitor's socket without a
    // cookie gets one, as a call does, so its sockets are one principal
    let (mut old, set) = Socket::open_answered(api, &name, "__live?v=1", None, None)?;
    old.until("hello", 5)?;
    let first_list = old.next()?;
    let mut n = Socket::open(api, &name, "__live", None, None)?;
    let n_hello = n.until("hello", 5)?;
    n.send(&json!({ "type": "presence", "data": { "name": "new" } }))?;
    let own = n.until("presence", 5)?;
    let listed = old.until("presence", 5)?;
    s.ok(
        "a page from before the change frames hears the whole list, on connecting and on each change",
        first_list == json!({ "type": "presence", "list": [] })
            && listed == json!({ "type": "presence", "list": [{ "id": n_hello["id"], "principal": n_hello["principal"], "data": { "name": "new" } }] }),
        format!("{first_list} {listed}"),
    );
    s.ok("while a current page hears the one change", own == json!({ "type": "presence", "id": n_hello["id"], "principal": n_hello["principal"], "data": { "name": "new" } }), &own);
    n.close();
    let left = old.until("presence", 5)?;
    s.ok("and the old page's list drops a socket that left", left == json!({ "type": "presence", "list": [] }), &left);
    old.close();
    s.ok("a visitor's socket without a cookie gets one, as a call does", set.iter().any(|c| c.starts_with("fragment_anon=") && c.contains("HttpOnly")), format!("{set:?}"));
    // b heard the new page's presence arrive and leave: past those two
    for _ in 0..2 {
        b.until("presence", 5)?;
    }
    // the pong says the unsubscribe was read; then a mutation's first
    // frame is its change signal, which a record still followed would precede
    b.send(&json!({ "type": "unsubscribe", "channel": "room" }))?;
    b.send(&json!({ "type": "ping" }))?;
    let pong = b.expect("pong");
    api.op(&owner, &name, "say", "l4", json!({ "text": "after unsubscribe" }))?;
    let next = b.expect("changed");
    s.ok("unsubscribing stops the records: the next frame is the change signal", pong.is_ok() && next.is_ok(), format!("{pong:?} {next:?}"));

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
    live_queries(s, api, &owner)
}

/// Queries over the live socket: run as the socket's principal and role,
/// refused as `__op` would refuse them, and bounded by their principal's
/// budget between changes, never by the public call budget.
fn live_queries(s: &mut Suite, api: &Api, owner: &Keys) -> Result<()> {
    let (name, _) = chat(s, api, owner, "queries")?;
    let mut v = Socket::open(api, &name, "__live", None, None)?;
    let v_hello = v.until("hello", 5)?;
    let ask = |sock: &mut Socket, id: &str, op: &str, input: Value| -> Result<Value> {
        sock.send(&json!({ "type": "query", "id": id, "op": op, "input": input }))?;
        sock.until("result", 10)
    };
    let who = ask(&mut v, "who", "whoami", json!({}))?;
    s.ok(
        "a query over a visitor's socket answers as the socket's principal and role",
        who == json!({ "type": "result", "id": "who", "result": { "principal": v_hello["principal"], "role": "public" } }),
        &who,
    );
    let mut o = Socket::open(api, &name, "__live", Some(owner), None)?;
    let o_hello = o.until("hello", 5)?;
    let who_owner = ask(&mut o, "who", "whoami", json!({}))?;
    s.ok(
        "and over a member's socket, as the member and their role",
        who_owner["result"] == json!({ "principal": o_hello["principal"], "role": "owner" }) && o_hello["principal"] == api.identity(owner)?.as_str(),
        &who_owner,
    );
    let above = ask(&mut v, "staff", "backstage", json!({}))?;
    s.ok(
        "a query above the socket's role is refused as __op refuses it",
        above["id"] == "staff" && above["error"] == "forbidden" && above["status"] == 403 && above.get("result").is_none(),
        &above,
    );
    let act = ask(&mut v, "act", "say", json!({ "text": "over the socket" }))?;
    let count = ask(&mut v, "count", "count", json!({}))?;
    s.ok(
        "only a query runs over the socket: a mutation is refused there, and not run",
        act["error"] == "invalid_request" && act["status"] == 400 && count["result"] == json!({ "n": 0 }),
        format!("{act} {count}"),
    );
    let bad = ask(&mut v, "not an id!", "count", json!({}))?;
    let schema = ask(&mut v, "unknown", "no_such_query", json!({}))?;
    s.ok(
        "a bad id or an unknown query is refused, with the id it came with",
        bad["error"] == "invalid_request" && bad["id"] == "not an id!" && schema["error"] == "unknown_operation" && schema["status"] == 404,
        format!("{bad} {schema}"),
    );
    // the principal's budget: LIVE_QUERIES_MAX between two changes
    let r = api.op(owner, &name, "say", "refill-1", json!({ "text": "a change" }))?;
    let mut answered = vec![];
    for i in 0..=limits::LIVE_QUERIES_MAX {
        answered.push(ask(&mut v, &format!("n{i}"), "count", json!({}))?);
    }
    let ran = answered.iter().filter(|a| a["result"] == json!({ "n": 1 })).count();
    let last = answered.last().cloned().unwrap_or_default();
    s.ok(
        "a socket runs 16 queries between two changes; the 17th is refused (429)",
        r.status == 200 && ran == limits::LIVE_QUERIES_MAX as usize && last["error"] == "rate_limited" && last["status"] == 429,
        format!("{ran} ran; the last answered {last}"),
    );
    api.op(owner, &name, "say", "refill-2", json!({ "text": "another change" }))?;
    let after = ask(&mut v, "after", "count", json!({}))?;
    s.ok("and the next change refills it", after["result"] == json!({ "n": 2 }), &after);
    s.ok(
        "result frames decode as LiveOut",
        [&who, &who_owner, &above, &act, &bad, &last, &after].iter().all(|f| serde_json::from_value::<LiveOut>((*f).clone()).is_ok()),
        "",
    );

    // A principal's sockets share its budget: a socket opened again gets
    // no fresh one. (A cookie names the visitor, as a browser's does.)
    let visitor = format!("fragment_anon={}", "ab".repeat(32));
    api.op(owner, &name, "say", "refill-3", json!({ "text": "a third change" }))?;
    let mut w1 = Socket::open(api, &name, "__live", None, Some(&visitor))?;
    w1.until("hello", 5)?;
    let mut spent = 0;
    for i in 0..limits::LIVE_QUERIES_MAX {
        spent += usize::from(ask(&mut w1, &format!("w{i}"), "count", json!({}))?["result"] == json!({ "n": 3 }));
    }
    let mut w2 = Socket::open(api, &name, "__live", None, Some(&visitor))?;
    w2.until("hello", 5)?;
    let more = ask(&mut w2, "w-more", "count", json!({}))?;
    s.ok(
        "a visitor's sockets share one budget: a second socket runs nothing once the first spent it (429)",
        spent == limits::LIVE_QUERIES_MAX as usize && more["error"] == "rate_limited" && more["status"] == 429,
        format!("{spent} ran on the first; the second answered {more}"),
    );
    w1.close();
    w2.close();

    // a principal holds at most LIVE_SOCKETS_PER_PRINCIPAL sockets
    let tabs = format!("fragment_anon={}", "cd".repeat(32));
    let mut held = vec![];
    for _ in 0..limits::LIVE_SOCKETS_PER_PRINCIPAL {
        let mut t = Socket::open(api, &name, "__live", None, Some(&tabs))?;
        t.until("hello", 5)?;
        held.push(t);
    }
    let over = Socket::open(api, &name, "__live", None, Some(&tabs)).err().map(|e| e.to_string()).unwrap_or_default();
    s.ok(
        &format!("a principal holds at most {} sockets on a fragment; one more is refused (429)", limits::LIVE_SOCKETS_PER_PRINCIPAL),
        over.contains("429"),
        &over,
    );
    held.into_iter().for_each(Socket::close);

    // opening a socket at the public role is a public call: what a
    // visitor opens comes out of the calls it may make that minute
    let minute = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 60).unwrap_or(0);
    let opens = 3;
    let mut calls = (0, false);
    for attempt in 0..2 {
        let payer = format!("fragment_anon={}", format!("e{attempt}").repeat(32));
        let at = minute();
        for _ in 0..opens {
            Socket::open(api, &name, "__live", None, Some(&payer))?.close();
        }
        let mut went = 0;
        for i in 0..=limits::PUBLIC_CALLS_PER_MIN {
            if api.browser_op(&name, "count", &format!("pay-{attempt}-{i}"), json!({}), Some(&payer))?.status != 200 {
                break;
            }
            went += 1;
        }
        calls = (went, minute() == at);
        if calls.1 {
            break;
        }
    }
    s.ok(
        &format!("a visitor's {opens} socket opens are public calls: {} calls go through that minute, not {}", limits::PUBLIC_CALLS_PER_MIN - opens, limits::PUBLIC_CALLS_PER_MIN),
        calls == (limits::PUBLIC_CALLS_PER_MIN - opens, true),
        format!("{} went through (within one minute: {})", calls.0, calls.1),
    );

    // Who a signed-in socket is gets asked again once LIVE_IDENTITY_MS
    // passed (a test lever ages the check): the owner's key is still
    // theirs, and a key revoked meanwhile closes its socket (4001)
    let signer = api.person()?;
    let spare = Keys::generate();
    let add = "/api/identities/me/keys";
    let r = api.signed(&signer, "POST", add, Some(&json!({ "proof": api.proof(&spare, "POST", add, &signer) })))?;
    let mut k = Socket::open(api, &name, "__live", Some(&spare), None)?;
    let k_hello = k.until("hello", 5)?;
    s.ok("(a person's second key opens a socket as them)", r.status == 200 && k_hello["principal"] == api.identity(&signer)?.as_str(), &k_hello);
    let r = api.signed(&signer, "DELETE", &format!("/api/identities/me/keys/{}", fragment_core::npub::encode(spare.pubkey_hex())), None)?;
    let aged = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "age-live", "ms": limits::LIVE_IDENTITY_MS })))?;
    s.ok("(the test fleet ages the sockets' identity checks)", r.status == 200 && aged.status == 200 && aged.body["aged"].as_u64().is_some_and(|n| n >= 2), &aged);
    o.send(&json!({ "type": "ping" }))?;
    let still = o.until("pong", 5)?;
    s.ok("a signed-in socket asked about again goes on while its key is still its person's", still["type"] == "pong", &still);
    k.send(&json!({ "type": "ping" }))?;
    let closed = loop {
        match k.next() {
            Ok(_) => continue,
            Err(e) => break e.to_string(),
        }
    };
    s.ok("a socket whose key was revoked is closed at its next frame once asked again (4001)", closed.contains("4001"), &closed);
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "forget-live" })))?;
    api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "age-live", "ms": limits::LIVE_IDENTITY_MS })))?;
    o.send(&json!({ "type": "ping" }))?;
    let closed = loop {
        match o.next() {
            Ok(_) => continue,
            Err(e) => break e.to_string(),
        }
    };
    s.ok(
        "one the object holds no credential for (it woke) is closed to reconnect, never trusted as it was (4001)",
        r.status == 200 && closed.contains("4001"),
        &closed,
    );

    // a socket's role is fixed while it is open: a member whose role
    // changes has theirs reopened (4001, which the library reconnects
    // after), and the new one runs queries at the new role
    let member = api.person()?;
    api.signed(owner, "PUT", &format!("/api/f/{name}/members/{}", member.pubkey_hex()), Some(&json!({ "role": "editor" })))?;
    let mut m = Socket::open(api, &name, "__live", Some(&member), None)?;
    m.until("hello", 5)?;
    let as_editor = ask(&mut m, "staff", "backstage", json!({}))?;
    api.signed(owner, "PUT", &format!("/api/f/{name}/members/{}", member.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    let mut closed = String::new();
    for _ in 0..20 {
        match m.next() {
            Ok(_) => continue,
            Err(e) => {
                closed = e.to_string();
                break;
            }
        }
    }
    s.ok(
        "an editor's queries run as an editor; their role changing closes their socket with 4001",
        as_editor["result"] == json!({ "role": "editor" }) && closed.contains("4001"),
        format!("{as_editor}; closed: {closed}"),
    );
    let mut m = Socket::open(api, &name, "__live", Some(&member), None)?;
    let again = m.until("hello", 5)?;
    let as_viewer = ask(&mut m, "staff", "backstage", json!({}))?;
    s.ok("their new socket has the new role, and its queries run at it", again["role"] == "viewer" && as_viewer["error"] == "forbidden", format!("{again} {as_viewer}"));
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
    s.ok("any other path goes to the App's fetch, which imports applib/ modules (HELLO! is theirs)", r.status == 200 && r.text.contains("HELLO!"), &r);
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
    // --json names a refusal by the host's own code (these were server_error)
    let refused = |s: &Suite, args: &[&str]| {
        let out = s.cli(api, &home, args);
        (out.status.code(), serde_json::from_slice::<Value>(&out.stdout).unwrap_or_default())
    };
    let (exit, v) = refused(s, &["call", &name, "say", "--input", "{}", "--json"]);
    s.ok("with --json it is invalid_request (exit 1)", exit == Some(1) && v["error"]["code"] == "invalid_request", &v);
    let (exit, v) = refused(s, &["call", &name, "say", "--input", r#"{"text":"other"}"#, "--id", "cli-1", "--json"]);
    s.ok("an --id reused with another input is conflicting_body", exit == Some(1) && v["error"]["code"] == "conflicting_body", &v);
    let (exit, v) = refused(s, &["call", &name, "say", "--input", "{not json", "--json"]);
    s.ok("input that is not JSON is invalid_usage (exit 2)", exit == Some(2) && v["error"]["code"] == "invalid_usage", &v);
    let r = s.cli_json(api, &home, &["channel", &name, "--json"])?;
    s.ok("fragment channel lists the channels", r["channels"].as_array().is_some_and(|a| a.iter().any(|c| c["name"] == "room")), &r);
    let r = s.cli_json(api, &home, &["channel", &name, "room", "--json"])?;
    s.ok("fragment channel reads one", r["records"][0]["body"]["text"] == "from the cli", &r);
    // the follower has read the channel's one record before the next is
    // published, so the next can only reach it live
    let log = s.scratch.join(format!("follow-{name}.log"));
    let mut follow = Command::new(&s.cli)
        .args(["channel", &name, "room", "--follow"])
        .env("HOME", &home)
        .env("FRAGMENT_HOST", &api.base)
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&log)?)
        .stderr(Stdio::null())
        .spawn()?;
    let caught_up = s.eventually(Duration::from_secs(10), || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("from the cli")));
    api.op(&keys, &name, "say", "cli-2", json!({ "text": "followed" }))?;
    let seen = s.eventually(Duration::from_secs(10), || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("followed")));
    let _ = follow.kill();
    let _ = follow.wait();
    s.ok("fragment channel --follow streams new records", caught_up && seen, std::fs::read_to_string(&log).unwrap_or_default());

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
        r["rotated"] == json!(["inbox", "view"]) && r["viewToken"] != c["viewToken"] && r["webhookSecret"] == c["webhookSecret"],
        json!({ "rotated": r["rotated"], "view_changed": r["viewToken"] != c["viewToken"], "webhook_kept": r["webhookSecret"] == c["webhookSecret"] }),
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

    let Some(mut chrome) = s.browser()? else {
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

    // a public list changing past 60 times a minute: an anonymous
    // visitor's live view re-runs over its socket, not as HTTP calls (which
    // the public rate limit counts), so it keeps up with every change
    let busy = s.named(api, &owner, "busy")?;
    let c = s.create(api, &owner, &busy)?;
    api.signed(&owner, "PUT", &format!("/api/f/{busy}/visibility"), Some(&json!({ "visibility": "public" })))?;
    s.commit(&c, &changes);
    s.deploy(&c);
    let v = chrome.open(&api.site_url(&busy, ""))?;
    s.ok("(an anonymous visitor opens the public list)", chrome.until(&v, "document.getElementById('here')?.textContent === 'just you here'", wait), "");
    let t0 = std::time::Instant::now();
    let mut kept_up = 0;
    for i in 1..=125 {
        let r = api.op(&owner, &busy, "add", &format!("busy-{i}"), json!({ "text": format!("busy {i}") }))?;
        let shown = format!("[...document.querySelectorAll('#todos li span')].some((s) => s.textContent === 'busy {i}')");
        if r.status != 200 || !chrome.until(&v, &shown, Duration::from_secs(5)) {
            break;
        }
        kept_up = i;
    }
    let took = t0.elapsed();
    println!("      the visitor's page kept up with {kept_up} changes in {took:?}");
    let error = chrome.eval(&v, "document.getElementById('error').textContent")?;
    s.ok(
        "an anonymous visitor's live view keeps up with 125 changes, one after another",
        kept_up == 125 && error == "",
        format!("kept up with {kept_up} in {took:?}; the page says {error}"),
    );
    let calls = chrome.eval(&v, "performance.getEntriesByType('resource').filter((e) => e.name.includes('__op/list')).length")?;
    println!("      its page made {calls} HTTP calls to the query");
    s.ok("and asked the query over HTTP once, before its socket opened: every re-run went over the socket", calls == 1, format!("{calls} HTTP calls to list"));
    Ok(())
}
