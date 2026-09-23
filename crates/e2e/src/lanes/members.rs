//! Membership as live cell state: grants, invites, leaving, the
//! per-person list, and secrets sealed in the cell.

use anyhow::Result;
use fragment_core::npub;
use fragment_nip98::Keys;
use serde_json::json;

use crate::api::{self, Api};
use crate::Suite;

fn listed(api: &Api, keys: &Keys, name: &str) -> Result<Option<String>> {
    let r = api.signed(keys, "GET", "/api/fragments", None)?;
    Ok(r.body["fragments"].as_array().and_then(|a| a.iter().find(|f| f["name"] == name)).and_then(|f| f["role"].as_str().map(str::to_string)))
}

pub fn members(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("members") {
        return Ok(());
    }
    let owner = Keys::generate();
    let (bob, carol, dave, erin) = (Keys::generate(), Keys::generate(), Keys::generate(), Keys::generate());
    let name = s.name("members");
    s.create(api, &owner, &name)?;
    let path = |rest: &str| format!("/api/f/{name}/{rest}");
    let npub_of = |k: &Keys| npub::encode(k.pubkey_hex());

    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", npub_of(&bob))), Some(&json!({ "role": "viewer" })))?;
    s.ok("the owner adds a viewer by npub", r.status == 200 && r.body["role"] == "viewer" && r.body["principal"] == npub_of(&bob), &r);
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

    // invites
    let r = api.signed(&bob, "POST", &path("invites"), Some(&json!({ "role": "viewer" })))?;
    s.ok("an editor cannot make invites", r.status == 403, &r);
    let r = api.signed(&owner, "POST", &path("invites"), Some(&json!({ "role": "viewer", "ttlS": 10 })))?;
    s.ok("an invite shorter than a minute is 400", r.status == 400, &r);
    let r = api.signed(&owner, "POST", &path("invites"), Some(&json!({ "role": "viewer", "uses": 2 })))?;
    let token = r.body["token"].as_str().unwrap_or("").to_string();
    s.ok("the owner makes a two-use invite and sees its token once", r.status == 200 && token.len() == 48 && r.body["usesLeft"] == 2, &r);
    let r = api.signed(&owner, "GET", &path("invites"), None)?;
    s.ok("listed invites never show tokens", r.status == 200 && !r.text.contains(&token) && r.text.contains("usesLeft"), &r);
    let join = |k: &Keys, t: &str| api.signed(k, "POST", &path("join"), Some(&json!({ "token": t })));
    let r = join(&carol, &token)?;
    s.ok("a stranger joins with the invite", r.status == 200 && r.body["joined"] == true && r.body["role"] == "viewer", &r);
    let r = join(&carol, &token)?;
    s.ok("joining twice does not spend a use", r.status == 200 && r.body["joined"] == false, &r);
    let r = join(&dave, &token)?;
    s.ok("the second person joins", r.status == 200 && r.body["joined"] == true, &r);
    let r = join(&erin, &token)?;
    s.ok("a used-up invite is 404", r.status == 404, &r);
    let r = join(&erin, "not-a-token")?;
    s.ok("an unknown invite is 404", r.status == 404, &r);
    let r = api.signed(&owner, "POST", &path("invites"), Some(&json!({ "role": "editor" })))?;
    let (upgrade, upgrade_id) = (r.body["token"].as_str().unwrap_or("").to_string(), r.body["id"].as_str().unwrap_or("").to_string());
    let r = join(&carol, &upgrade)?;
    s.ok("an editor invite upgrades a viewer", r.status == 200 && r.body["role"] == "editor", &r);
    let r = api.signed(&owner, "POST", &path("invites"), Some(&json!({ "role": "viewer" })))?;
    let (revoked, revoked_id) = (r.body["token"].as_str().unwrap_or("").to_string(), r.body["id"].as_str().unwrap_or("").to_string());
    let r = api.signed(&owner, "DELETE", &path(&format!("invites/{revoked_id}")), None)?;
    s.ok("the owner revokes an invite", r.status == 200, &r);
    let r = join(&erin, &revoked)?;
    s.ok("a revoked invite is 404", r.status == 404, &r);
    let r = api.signed(&owner, "DELETE", &path(&format!("invites/{upgrade_id}")), None)?;
    s.ok("revoking a spent invite is 404", r.status == 404, &r);

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
    Ok(())
}

pub fn secrets(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("secrets") {
        return Ok(());
    }
    let owner = Keys::generate();
    let viewer = Keys::generate();
    let name = s.name("secrets");
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
    let r = put(&owner, "HUGE", &vec![b'x'; 65 * 1024])?;
    s.ok("a value over 64 KiB is 413", r.status == 413, &r);
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
