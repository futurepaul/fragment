//! Membership as live cell state: grants, invites by email, leaving, the
//! per-person list, and secrets sealed in the cell.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_core::npub;
use fragment_nip98::Keys;
use fragment_proto::{limits, ErrorCode};
use serde_json::json;

use crate::api::{self, Api};
use crate::Suite;

fn listed(api: &Api, keys: &Keys, name: &str) -> Result<Option<String>> {
    let r = api.signed(keys, "GET", "/api/fragments", None)?;
    Ok(r.body["fragments"].as_array().and_then(|a| a.iter().find(|f| f["name"] == name)).and_then(|f| f["role"].as_str().map(str::to_string)))
}

pub fn members(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("members", &[crate::Need::Levers]) {
        return Ok(());
    }
    let owner = api.person()?;
    let (bob, carol, dave, erin) = (api.person()?, api.person()?, api.person()?, api.person()?);
    let name = s.named(api, &owner, "members")?;
    s.create(api, &owner, &name)?;
    let path = |rest: &str| format!("/api/f/{name}/{rest}");
    let npub_of = |k: &Keys| npub::encode(k.pubkey_hex());

    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", npub_of(&bob))), Some(&json!({ "role": "viewer" })))?;
    s.ok("the owner adds a viewer by npub: the member is the identity holding it", r.status == 200 && r.body["role"] == "viewer" && r.body["principal"] == api.identity(&bob)?.as_str() && r.body["kind"] == "person", &r);
    let r = api.status(&bob, &name)?;
    s.ok("the viewer reads status", r.status == 200 && r.body["role"] == "viewer", &r);
    s.ok("a viewer does not see the inbox token", r.body["inboxToken"].is_null(), &r);
    s.ok("the viewer's list has the fragment", listed(api, &bob, &name)?.as_deref() == Some("viewer"), "");
    let r = api.signed(&bob, "PUT", &path(&format!("members/{}", npub_of(&carol))), Some(&json!({ "role": "viewer" })))?;
    s.ok("a viewer cannot add members", r.status == 403, &r);
    let r = api.signed(&bob, "GET", &path("storage-token"), None)?;
    s.ok("a viewer cannot mint a storage token", r.status == 403, &r);
    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", bob.pubkey_hex())), Some(&json!({ "role": "editor" })))?;
    s.ok("the owner promotes by hex key", r.status == 200 && r.body["role"] == "editor", &r);
    let r = api.signed(&bob, "GET", &path("storage-token"), None)?;
    s.ok("an editor mints a storage token", r.status == 200, &r);
    s.ok("the list follows the promotion", listed(api, &bob, &name)?.as_deref() == Some("editor"), "");
    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", npub_of(&owner))), Some(&json!({ "role": "viewer" })))?;
    s.ok("the owner's own role cannot change", r.status == 400, &r);
    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", npub_of(&carol))), Some(&json!({ "role": "owner" })))?;
    s.ok("owner cannot be granted", r.status == 400, &r);
    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", npub_of(&carol))), Some(&json!({ "role": "public" })))?;
    s.ok("public cannot be granted", r.status == 400, &r);
    let r = api.signed(&owner, "PUT", &path("members/npub1nope"), Some(&json!({ "role": "viewer" })))?;
    s.ok("a malformed npub is 400", r.status == 400, &r);
    let r = api.signed(&carol, "GET", &path("members"), None)?;
    s.ok("a stranger cannot list members", r.status == 403, &r);
    let r = api.signed(&bob, "GET", &path("members"), None)?;
    s.ok("a member lists members", r.status == 200 && r.body["members"].as_array().map_or(0, |a| a.len()) == 2, &r);

    // invites by email (decision 48)
    let invite = |email: &str, role: &str| api.signed(&owner, "POST", &path("invites"), Some(&json!({ "email": email, "role": role })));
    let waiting = |email: &str| -> Result<bool> {
        let r = api.signed(&owner, "GET", &path("invites"), None)?;
        Ok(r.body["invites"].as_array().is_some_and(|a| a.iter().any(|i| i["email"] == email)))
    };
    // a person who signs in for the first time as `keys`' email
    let first_sign_in = |keys: &Keys| -> Result<()> {
        let session = api.sign_in(&Api::email_of(keys))?;
        api.approve(&session, keys)?;
        Ok(())
    };
    let r = api.signed(&bob, "POST", &path("invites"), Some(&json!({ "email": Api::email_of(&carol), "role": "viewer" })))?;
    s.ok("an editor cannot invite", r.status == 403, &r);
    let refused = [invite("not an email", "viewer")?, invite("x@e2e.test", "owner")?, api.signed(&owner, "POST", &path("invites"), Some(&json!({ "role": "viewer" })))?];
    s.ok("an invite names an email and a role it may grant (400 otherwise)", refused.iter().all(|r| r.status == 400), format!("{} / {} / {}", refused[0], refused[1], refused[2]));
    let r = invite(&Api::email_of(&carol), "viewer")?;
    s.ok(
        "an invite to an email someone signs in as makes them a member at once",
        r.status == 200 && r.body["member"]["principal"] == api.identity(&carol)?.as_str() && r.body["member"]["role"] == "viewer" && listed(api, &carol, &name)?.as_deref() == Some("viewer"),
        &r,
    );
    let r = invite(&Api::email_of(&carol), "editor")?;
    s.ok("again with another role, it changes their role, as a PUT does", r.status == 200 && r.body["member"]["role"] == "editor", &r);
    let newcomer = Keys::generate();
    let r = invite(&Api::email_of(&newcomer), "viewer")?;
    s.ok(
        "an invite to an email no one signs in as yet waits on it",
        r.status == 200 && r.body["invited"]["email"] == Api::email_of(&newcomer).as_str() && r.body["invited"]["role"] == "viewer" && waiting(&Api::email_of(&newcomer))?,
        &r,
    );
    first_sign_in(&newcomer)?;
    s.ok(
        "their first sign-in as it makes them a member, and the invite waits no more",
        listed(api, &newcomer, &name)?.as_deref() == Some("viewer") && !waiting(&Api::email_of(&newcomer))?,
        "",
    );
    let gone = Keys::generate();
    invite(&Api::email_of(&gone), "editor")?;
    let r = api.signed(&owner, "DELETE", &path(&format!("invites/{}", Api::email_of(&gone))), None)?;
    s.ok("the owner revokes an invite", r.status == 200 && r.body["revoked"] == Api::email_of(&gone).as_str() && !waiting(&Api::email_of(&gone))?, &r);
    let r = api.signed(&owner, "DELETE", &path(&format!("invites/{}", Api::email_of(&gone))), None)?;
    s.ok("revoking it again is 404", r.status == 404, &r);
    first_sign_in(&gone)?;
    s.ok("a revoked invite is met by no one: their sign-in makes them no member", listed(api, &gone, &name)?.is_none(), "");
    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", Api::email_of(&dave))), Some(&json!({ "role": "viewer" })))?;
    s.ok("a PUT names a person by their email too", r.status == 200 && r.body["principal"] == api.identity(&dave)?.as_str(), &r);
    let r = api.signed(&owner, "PUT", &path("members/nobody-yet@e2e.test"), Some(&json!({ "role": "viewer" })))?;
    s.ok("and one no one signs in as is 404 (an invite waits on it)", r.status == 404, &r);

    // revoking closes the member's change feed
    let mut feed = api::watch(api, &name, "", Some(&dave))?;
    let hello = feed.read()?;
    s.ok("a member opens the change feed", hello.to_text().unwrap_or("").contains("hello"), &hello);
    let r = api.signed(&owner, "DELETE", &path(&format!("members/{}", npub_of(&dave))), None)?;
    s.ok("the owner removes a member", r.status == 200, &r);
    let closed = match feed.read() {
        Ok(tungstenite::Message::Close(Some(f))) => u16::from(f.code) == 4003,
        other => {
            println!("      feed after removal: {other:?}");
            false
        }
    };
    s.ok("removal closes the member's change feed (4003)", closed, "");
    s.ok("a removed member leaves the list", listed(api, &dave, &name)?.is_none(), "");
    let r = api.status(&dave, &name)?;
    s.ok("a removed member is refused on the next request", r.status == 403, &r);
    let r = api.signed(&bob, "DELETE", &path(&format!("members/{}", npub_of(&bob))), None)?;
    s.ok("a member leaves", r.status == 200, &r);
    s.ok("leaving drops the fragment from the list", listed(api, &bob, &name)?.is_none(), "");
    let r = api.signed(&carol, "DELETE", &path(&format!("members/{}", npub_of(&owner))), None)?;
    s.ok("a member cannot remove the owner", r.status == 403, &r);
    let r = api.signed(&owner, "DELETE", &path(&format!("members/{}", npub_of(&owner))), None)?;
    s.ok("the owner cannot leave", r.status == 400, &r);
    let r = api.signed(&owner, "DELETE", &path(&format!("members/{}", npub_of(&erin))), None)?;
    s.ok("removing a non-member is 404", r.status == 404, &r);
    let r = api.signed(&owner, "GET", &path("events"), None)?;
    let kinds: Vec<&str> = r.body["events"].as_array().map(|a| a.iter().filter_map(|e| e["kind"].as_str()).collect()).unwrap_or_default();
    s.ok(
        "every change is in the events",
        ["member.set", "invite.created", "member.joined", "invite.revoked", "member.removed"].iter().all(|k| kinds.contains(k)),
        format!("{kinds:?}"),
    );

    // the member cap holds for invites as it does for grants
    let (at_cap, over) = (Keys::generate(), Keys::generate());
    invite(&Api::email_of(&at_cap), "viewer")?;
    invite(&Api::email_of(&over), "viewer")?;
    let below = limits::MEMBERS_MAX as u64 - 1;
    let r = api.signed(&owner, "POST", &path("test/members"), Some(&json!({ "fill": below })))?;
    s.ok("(a fragment's test levers are the router's /api/test/fragment alone: no signed route)", r.status == 404, &r);
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "members", "fill": below })))?;
    s.ok("(a test hook fills the fragment to one below the member cap)", r.status == 200 && r.body["members"] == below, &r);
    first_sign_in(&at_cap)?;
    s.ok("an invite admits the member that reaches the cap", listed(api, &at_cap, &name)?.as_deref() == Some("viewer"), "");
    first_sign_in(&over)?;
    s.ok(
        "at the member cap a sign-in meets the invite and is refused, and the invite waits on for their next sign-in",
        listed(api, &over, &name)?.is_none() && waiting(&Api::email_of(&over))?,
        "",
    );
    let r = api.signed(&owner, "GET", &path("members"), None)?;
    s.ok("the fragment holds exactly the cap", r.body["members"].as_array().map(Vec::len) == Some(limits::MEMBERS_MAX), r.status);
    let r = api.status(&over, &name)?;
    s.ok("the refused person is not a member", r.status == 403, &r);

    // a delete answers promptly, whatever its members: it tells one round
    // of their lists, its owner's first, and its alarm tells the rest
    let t0 = Instant::now();
    let r = api.signed(&owner, "DELETE", &format!("/api/f/{name}"), None)?;
    let took = t0.elapsed();
    println!("      (the delete answered in {took:.2?})");
    s.ok(
        &format!("the owner deletes a fragment of {} members within {DELETE_ANSWERS_IN:?}", limits::MEMBERS_MAX),
        r.status == 200 && r.body["deleted"] == name.as_str() && took < DELETE_ANSWERS_IN,
        format!("{took:.2?}: {r}"),
    );
    s.ok("its owner's list drops it at once", listed(api, &owner, &name)?.is_none(), "");
    let r = api.status(&carol, &name)?;
    s.ok("and it is gone at once, for its members too (404)", r.status == 404, &r);
    // the name is free at once, while the ended life's cleanup goes on
    s.create(api, &owner, &name)?;
    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", npub_of(&at_cap))), Some(&json!({ "role": "viewer" })))?;
    s.ok("the name is made again at once, and shared with a member of the ended life", r.status == 200, &r);
    let (cleaned, last) = s.ended_cleaned(api, &name);
    s.ok("the ended life's cleanup completes: every member's list told, the app's database and the blobs gone", cleaned, last);
    s.ok("a member of the ended life alone has lost it from their list", listed(api, &carol, &name)?.is_none(), "");
    s.ok(
        "and the one shared with again keeps the new life's row: the ended life's change never undoes it",
        listed(api, &at_cap, &name)?.as_deref() == Some("viewer"),
        "",
    );
    Ok(())
}

/// How soon a delete answers, at any count of members: one round of
/// Principal calls, at most (it took 300 s at 1000 on the e2e preview,
/// telling each list in turn, 2026-10-06).
const DELETE_ANSWERS_IN: Duration = Duration::from_secs(5);

pub fn secrets(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("secrets", &[]) {
        return Ok(());
    }
    let owner = api.person()?;
    let viewer = api.person()?;
    let name = s.named(api, &owner, "secrets")?;
    s.create(api, &owner, &name)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", viewer.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    let value = "sk-or-v1-e2e-never-shown-again";
    let put = |keys: &Keys, key: &str, body: &[u8]| {
        api.call(crate::api::Call {
            method: "PUT",
            url: format!("{}/api/f/{name}/secrets/{key}", api.base),
            body: Some(body.to_vec()),
            keys: Some(keys),
            ..Default::default()
        })
    };
    let r = put(&owner, "OPENROUTER_API_KEY", value.as_bytes())?;
    s.ok("an editor sets a secret", r.status == 200 && !r.text.contains(value), &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/secrets"), None)?;
    s.ok("secrets list by name only", r.body["names"] == json!(["OPENROUTER_API_KEY"]) && !r.text.contains(value), &r);
    let events = api.signed(&owner, "GET", &format!("/api/f/{name}/events"), None)?;
    let status = api.status(&owner, &name)?;
    s.ok("the value is in no answer", !events.text.contains(value) && !status.text.contains(value), "events or status held it");
    let r = put(&owner, "lower_case", b"v")?;
    s.ok("a lower-case name is 400", r.status == 400, &r);
    let r = put(&owner, "EMPTY", b"")?;
    s.ok("an empty value is 400", r.status == 400, &r);
    let edge = put(&owner, "EDGE", &vec![b'x'; limits::SECRET_MAX_BYTES])?;
    let r = put(&owner, "HUGE", &vec![b'x'; limits::SECRET_MAX_BYTES + 1])?;
    s.ok("a value of exactly the limit is set, and a byte over it is 413", edge.status == 200 && r.code() == Some(ErrorCode::TooLarge), format!("{edge} {r}"));
    let r = api.signed(&viewer, "GET", &format!("/api/f/{name}/secrets"), None)?;
    s.ok("a viewer cannot list secrets", r.status == 403, &r);
    let r = put(&viewer, "X", b"v")?;
    s.ok("a viewer cannot set secrets", r.status == 403, &r);
    let r = api.signed(&owner, "DELETE", &format!("/api/f/{name}/secrets/OPENROUTER_API_KEY"), None)?;
    s.ok("an editor deletes a secret", r.status == 200 && r.body["removed"] == true, &r);
    let r = api.signed(&owner, "DELETE", &format!("/api/f/{name}/secrets/OPENROUTER_API_KEY"), None)?;
    s.ok("deleting it again removes nothing", r.status == 200 && r.body["removed"] == false, &r);
    Ok(())
}
