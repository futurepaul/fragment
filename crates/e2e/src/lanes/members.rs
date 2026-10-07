//! Membership as live cell state: grants, invites, leaving, the
//! per-person list, and secrets sealed in the cell.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_core::npub;
use fragment_nip98::Keys;
use fragment_proto::{limits, ErrorCode};
use serde_json::{json, Value};

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

    // the member cap holds for invites as it does for grants
    let r = api.signed(&owner, "POST", &path("invites"), Some(&json!({ "role": "viewer", "uses": 5 })))?;
    let (open, open_id) = (r.body["token"].as_str().unwrap_or("").to_string(), r.body["id"].as_str().unwrap_or("").to_string());
    let below = limits::MEMBERS_MAX as u64 - 1;
    let r = api.signed(&owner, "POST", &path("test/members"), Some(&json!({ "fill": below })))?;
    s.ok("(a fragment's test levers are the router's /api/test/fragment alone: no signed route)", r.status == 404, &r);
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "members", "fill": below })))?;
    s.ok("(a test hook fills the fragment to one below the member cap)", r.status == 200 && r.body["members"] == below, &r);
    let r = join(&erin, &open)?;
    s.ok("an invite admits the member that reaches the cap", r.status == 200 && r.body["joined"] == true, &r);
    let frank = api.person()?;
    let r = join(&frank, &open)?;
    s.ok(
        "at the member cap an invite is refused",
        r.status == 400 && r.message() == format!("a fragment has at most {} members", limits::MEMBERS_MAX),
        &r,
    );
    let r = api.signed(&owner, "GET", &path("invites"), None)?;
    let uses_left = r.body["invites"].as_array().and_then(|a| a.iter().find(|i| i["id"] == open_id.as_str())).map(|i| i["usesLeft"].clone());
    s.ok("and the refusal spends no use of it", uses_left == Some(json!(4)), &r);
    let r = api.signed(&owner, "GET", &path("members"), None)?;
    s.ok("the fragment holds exactly the cap", r.body["members"].as_array().map(Vec::len) == Some(limits::MEMBERS_MAX), r.status);
    let r = api.status(&frank, &name)?;
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
    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", npub_of(&erin))), Some(&json!({ "role": "viewer" })))?;
    s.ok("the name is made again at once, and shared with a member of the ended life", r.status == 200, &r);
    let (cleaned, last) = s.ended_cleaned(api, &name);
    s.ok("the ended life's cleanup completes: every member's list told, the app's database and the blobs gone", cleaned, last);
    s.ok("a member of the ended life alone has lost it from their list", listed(api, &carol, &name)?.is_none(), "");
    s.ok(
        "and the one shared with again keeps the new life's row: the ended life's change never undoes it",
        listed(api, &erin, &name)?.as_deref() == Some("viewer"),
        "",
    );
    contributor(s, api)
}

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
/// The todo fixture's app as a members-only app declares it: its writes
/// are its contributors', its whole list its editors'.
const USED_JSON: &[u8] = br#"{
  "operations": {
    "add_todo": { "kind": "mutation", "role": "contributor", "input": { "type": "object" } },
    "count": { "kind": "query" },
    "list": { "kind": "query", "role": "editor" }
  },
  "channels": { "notes": { "post": "contributor" } }
}"#;

/// Goal: the role between viewer and editor (#232, item 2: the share
/// sheet's "Use"). A contributor calls what is declared for it and what a
/// viewer may, and nothing an editor's role opens; an agent capped at it
/// acts with no more. Method: Ann's app declares `add_todo` and the
/// `notes` channel's posts for contributors and `list` for editors. Bea, a
/// contributor, and Cy, a viewer, try each, and Bea the editor's routes;
/// then Ann's agent held at contributor, and Bea's agent acting for her;
/// last, an invite makes Cy a contributor.
fn contributor(s: &mut Suite, api: &Api) -> Result<()> {
    let (ann, bea, cy) = (api.person()?, api.person()?, api.person()?);
    let (ann_id, bea_id) = (api.identity(&ann)?, api.identity(&bea)?);
    let name = s.named(api, &ann, "used")?;
    let c = s.create(api, &ann, &name)?;
    super::app::ship(s, &c, TODO_APP, USED_JSON);
    let path = |rest: &str| format!("/api/f/{name}/{rest}");
    let grant = |who: &Keys, role: &str| api.signed(&ann, "PUT", &path(&format!("members/{}", npub::encode(who.pubkey_hex()))), Some(&json!({ "role": role })));
    let r = grant(&bea, "contributor")?;
    grant(&cy, "viewer")?;
    s.ok("the owner shares it with Bea to use: a contributor", r.status == 200 && r.body["role"] == "contributor", &r);
    let r = api.status(&bea, &name)?;
    s.ok(
        "Bea reads its status as a contributor, without the inbox token, and her list says so",
        r.status == 200 && r.body["role"] == "contributor" && r.body["inboxToken"].is_null() && listed(api, &bea, &name)?.as_deref() == Some("contributor"),
        &r,
    );
    let add = |k: &Keys, id: &str| api.op(k, &name, "add_todo", id, json!({ "text": "from a member" }));
    let note = |k: &Keys, id: &str| api.signed(k, "POST", &path("channels/notes"), Some(&json!({ "id": id, "body": { "text": "a note" } })));
    let (added, again, noted) = (add(&bea, "b1")?, add(&bea, "b1")?, note(&bea, "n1")?);
    let count = api.op(&bea, &name, "count", "q", json!({}))?;
    s.ok(
        "a contributor calls an operation declared for it (the same id again is its replay), posts to a channel that takes contributors' posts, and calls a viewer's query",
        added.status == 200 && again.body["replayed"] == true && noted.status == 200 && count.body["result"]["n"] == 1,
        json!({ "add": added.body, "again": again.body, "note": noted.body, "count": count.body }),
    );
    let r = api.op(&bea, &name, "list", "q", json!({}))?;
    s.ok("and is refused one declared for editors (403)", r.status == 403 && r.message() == "this needs the editor role", &r);
    let (added, noted, count) = (add(&cy, "c1")?, note(&cy, "n2")?, api.op(&cy, &name, "count", "q", json!({}))?);
    s.ok(
        "a viewer is refused what is declared for contributors, and still reads",
        added.status == 403 && noted.status == 403 && count.status == 200,
        json!({ "add": added.body, "note": noted.body, "count": count.status }),
    );
    let refused = [
        ("files", api.signed(&bea, "POST", &path("files"), Some(&json!({ "files": [{ "path": "site/index.html", "text": "mine now" }] })))?),
        ("deploy", api.signed(&bea, "POST", &path("deploy"), Some(&json!({})))?),
        ("secret set", api.signed(&bea, "PUT", &path("secrets/API_KEY"), Some(&json!("sk-not-hers")))?),
        ("secrets", api.signed(&bea, "GET", &path("secrets"), None)?),
        ("storage token", api.signed(&bea, "GET", &path("storage-token"), None)?),
    ];
    s.ok(
        "a contributor holds nothing of an editor's: no file writes, deploys, secrets, or storage token (403 each)",
        refused.iter().all(|(_, r)| r.status == 403),
        json!(refused.iter().map(|(what, r)| (what, r.status)).collect::<Vec<_>>()),
    );
    let r = api.op(&ann, &name, "list", "q", json!({}))?;
    s.ok("the owner's list holds Bea's todo", r.status == 200 && r.body["result"]["todos"].as_array().map(Vec::len) == Some(1), &r);

    // agents capped at it: Ann's, held there, and Bea's, acting for her
    let (hand, hand_id) = super::delegation::agent_of(api, &ann)?;
    let held = api.signed(&ann, "PUT", &format!("/api/identities/{hand_id}/held"), Some(&json!({ "held": "contributor" })))?;
    let for_ann = |rest: &str, body: Value| api.signed(&hand, "POST", &path(&format!("{rest}?for={ann_id}")), Some(&body));
    let (added, listed_all, wrote) = (
        for_ann("ops/add_todo", json!({ "id": "h1", "input": { "text": "from Ann's agent" } }))?,
        for_ann("ops/list", json!({ "id": "h2", "input": {} }))?,
        for_ann("files", json!({ "files": [{ "path": "site/index.html", "text": "the agent's" }] }))?,
    );
    s.ok(
        "Ann's agent held at contributor, acting for Ann (the owner), calls what contributors may and is refused an editor's operation and file writes",
        held.status == 200 && added.status == 200 && listed_all.status == 403 && wrote.status == 403,
        json!({ "held": held.status, "add": added.body, "list": listed_all.body, "files": wrote.body }),
    );
    let (juniper, _) = super::delegation::agent_of(api, &bea)?;
    let for_bea = |op: &str, id: &str, input: Value| api.signed(&juniper, "POST", &path(&format!("ops/{op}?for={bea_id}")), Some(&json!({ "id": id, "input": input })));
    let (added, listed_all) = (for_bea("add_todo", "j1", json!({ "text": "from Bea's agent" }))?, for_bea("list", "j2", json!({}))?);
    s.ok(
        "Bea's agent, acting for Bea, acts as the contributor she is: no further",
        added.status == 200 && listed_all.status == 403,
        json!({ "add": added.body, "list": listed_all.body }),
    );

    let r = api.signed(&ann, "POST", &path("invites"), Some(&json!({ "role": "contributor" })))?;
    let joined = api.signed(&cy, "POST", &path("join"), Some(&json!({ "token": r.body["token"] })))?;
    let added = add(&cy, "c2")?;
    s.ok(
        "an invite grants contributor: the viewer who joins with it now writes",
        r.status == 200 && r.body["role"] == "contributor" && joined.body["role"] == "contributor" && added.status == 200,
        json!({ "invite": r.body, "join": joined.body, "add": added.body }),
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
