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
    let owners = api.status(&owner, &name)?;
    s.ok(
        "nor the share link's: it is its owner's to give, as the share sheet has it",
        r.body["viewToken"].is_null() && owners.body["viewToken"].as_str().is_some_and(|t| t.len() == 24),
        json!([r.body["viewToken"], owners.body["viewToken"]]),
    );
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
    s.ok(
        "again, at another role, it is refused (400), saying a role changes with PUT members: an invite never changes one",
        r.status == 400 && r.message().contains("is in it already, as a viewer") && listed(api, &carol, &name)?.as_deref() == Some("viewer"),
        &r,
    );
    let r = api.signed(&owner, "PUT", &path(&format!("members/{}", Api::email_of(&carol))), Some(&json!({ "role": "editor" })))?;
    s.ok("PUT members, by their email, changes it", r.status == 200 && r.body["role"] == "editor" && listed(api, &carol, &name)?.as_deref() == Some("editor"), &r);
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
    // revoked, then invited again: the new invite is met
    let back = Keys::generate();
    let (r1, r2, r3) = (invite(&Api::email_of(&back), "viewer")?, api.signed(&owner, "DELETE", &path(&format!("invites/{}", Api::email_of(&back))), None)?, invite(&Api::email_of(&back), "editor")?);
    first_sign_in(&back)?;
    s.ok(
        "an invite revoked, then made again, is met: their sign-in makes them a member at the new invite's role",
        r1.status == 200 && r2.status == 200 && r3.status == 200 && listed(api, &back, &name)?.as_deref() == Some("editor"),
        format!("{r1} {r2} {r3}"),
    );
    // an email is an email in any case: an invite typed in capitals waits on
    // it in lower case, and a sign-in as it meets it
    let shouted = Keys::generate();
    let r = invite(&Api::email_of(&shouted).to_uppercase(), "viewer")?;
    let waits = waiting(&Api::email_of(&shouted))?;
    first_sign_in(&shouted)?;
    s.ok(
        "an invite to an email in capitals waits on it in lower case, and their sign-in meets it",
        r.status == 200 && r.body["invited"]["email"] == Api::email_of(&shouted).as_str() && waits && listed(api, &shouted, &name)?.as_deref() == Some("viewer"),
        &r,
    );
    // the owner's list counts the invites waiting
    let counted = |api: &Api| -> Result<serde_json::Value> {
        let r = api.signed(&owner, "GET", "/api/fragments", None)?;
        Ok(r.body["fragments"].as_array().and_then(|a| a.iter().find(|f| f["name"] == name)).map(|f| f["sharing"]["invites"].clone()).unwrap_or_default())
    };
    let pending = Keys::generate();
    invite(&Api::email_of(&pending), "viewer")?;
    let one = counted(api)?;
    api.signed(&owner, "DELETE", &path(&format!("invites/{}", Api::email_of(&pending))), None)?;
    let none = counted(api)?;
    s.ok("the owner's list counts the invites waiting: one made, then revoked", one == 1 && none == 0, format!("{one} then {none}"));
    // one that runs out waits no more: the fragment's alarm drops it at its
    // expiry (a lever ages it 30 days), and the owner's list is told unasked
    let lapsing = Keys::generate();
    invite(&Api::email_of(&lapsing), "viewer")?;
    let one = counted(api)?;
    let age = |api: &Api| api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": "age-invites", "ms": limits::INVITE_TTL_S * 1000 })));
    let aged = age(api)?;
    let dropped = s.eventually(Duration::from_secs(10), || counted(api).is_ok_and(|n| n == 0));
    s.ok(
        "an invite that runs out waits no more: the fragment drops it at its expiry, and its owner's list counts it no more",
        one == 1 && aged.status == 200 && aged.body["invites"] == 1 && dropped && !waiting(&Api::email_of(&lapsing))?,
        format!("{one}, then aged {aged}, then {}", counted(api)?),
    );
    let again = age(api)?;
    first_sign_in(&lapsing)?;
    s.ok(
        "aged again, nothing is left to run out; and a sign-in as its email meets no invite (the registry's copy is forgotten)",
        again.status == 200 && again.body["invites"] == 0 && counted(api)? == 0 && listed(api, &lapsing, &name)?.is_none(),
        &again,
    );

    // restricted, then opened by link again: a new share link, the old one
    // dead (a link holder read `__members`, as the share link lets them)
    let token = |api: &Api| -> Result<String> { Ok(api.status(&owner, &name)?.body["viewToken"].as_str().unwrap_or("").to_string()) };
    let by_link = |token: &str| api.page(&name, &format!("__members?view={token}"), None).map(|r| r.status);
    let before = token(api)?;
    let opened = by_link(&before)?;
    let r1 = api.signed(&owner, "PUT", &path("visibility"), Some(&json!({ "visibility": "members" })))?;
    let r2 = api.signed(&owner, "PUT", &path("visibility"), Some(&json!({ "visibility": "link" })))?;
    let after = token(api)?;
    let (old, new) = (by_link(&before)?, by_link(&after)?);
    s.ok(
        "restricted, then opened by link again, it has a new share link: the old one stays dead",
        opened == 200 && r1.status == 200 && r2.status == 200 && !after.is_empty() && after != before && old == 401 && new == 200,
        json!({ "opened": opened, "old": old, "new": new, "renewed": after != before }),
    );
    let r1 = api.signed(&owner, "PUT", &path("visibility"), Some(&json!({ "visibility": "public" })))?;
    let r2 = api.signed(&owner, "PUT", &path("visibility"), Some(&json!({ "visibility": "link" })))?;
    s.ok("public and back keeps the link: it never stopped working", r1.status == 200 && r2.status == 200 && token(api)? == after, "");

    // a chat's own agent (its lead, the first agent added) keeps its place:
    // its replies and steps make the chat
    let talk = s.named(api, &owner, "members-talk")?;
    let r = api.create_with(&owner, json!({ "name": talk, "template": "chat" }))?;
    anyhow::ensure!(r.status == 200 && r.body["visibility"] == "members", "the chat: {r}");
    let is_chat = s.eventually(Duration::from_secs(20), || api.status(&owner, &talk).is_ok_and(|r| r.body["kind"] == "chat"));
    anyhow::ensure!(is_chat, "{talk} never said it is a chat");
    let reg = "/api/identities";
    let agent = || -> Result<(Keys, String)> {
        let keys = Keys::generate();
        let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&keys, "POST", reg, &owner) })))?;
        anyhow::ensure!(r.status == 200, "an agent of the owner's: {r}");
        let id = r.body["id"].as_str().unwrap_or("").to_string();
        let r = api.signed(&owner, "PUT", &format!("/api/f/{talk}/members/{id}"), Some(&json!({ "role": "editor" })))?;
        anyhow::ensure!(r.status == 200, "{id} joins {talk}: {r}");
        Ok((keys, id))
    };
    let ((lead, lead_id), (_, second_id)) = (agent()?, agent()?);
    let talk_path = |rest: &str| format!("/api/f/{talk}/{rest}");
    let demoted = api.signed(&owner, "PUT", &talk_path(&format!("members/{lead_id}")), Some(&json!({ "role": "viewer" })))?;
    let removed = api.signed(&owner, "DELETE", &talk_path(&format!("members/{lead_id}")), None)?;
    let r = api.signed(&owner, "GET", &talk_path("members"), None)?;
    let leads = r.body["members"].as_array().into_iter().flatten().find(|m| m["principal"] == lead_id.as_str()).map(|m| m["role"].clone());
    s.ok(
        "its owner can neither lower a chat's own agent below editor nor remove it (400), saying why",
        demoted.status == 400 && removed.status == 400 && demoted.message().contains("this chat's own agent") && removed.message().contains("this chat's own agent") && leads == Some(json!("editor")),
        format!("{demoted} {removed}"),
    );
    let lowered = api.signed(&owner, "PUT", &talk_path(&format!("members/{second_id}")), Some(&json!({ "role": "viewer" })))?;
    let gone = api.signed(&owner, "DELETE", &talk_path(&format!("members/{second_id}")), None)?;
    s.ok("another agent in it is lowered and removed as any member is", lowered.status == 200 && gone.status == 200, format!("{lowered} {gone}"));
    let left = api.signed(&lead, "DELETE", &talk_path("members/me"), None)?;
    s.ok("the chat's own agent may leave it itself (as its owner's wipe has it do)", left.status == 200, &left);

    // a name deleted and made again is a new life: its owner's list shows
    // that life's face (none yet, made from no template), never the face
    // of the life before it
    let remade = s.named(api, &owner, "members-remade")?;
    let row_of = |api: &Api| -> Result<serde_json::Value> {
        let r = api.signed(&owner, "GET", "/api/fragments", None)?;
        Ok(r.body["fragments"].as_array().and_then(|a| a.iter().find(|f| f["name"] == remade.as_str())).cloned().unwrap_or_default())
    };
    let made = api.create_with(&owner, json!({ "name": remade, "template": "chat", "title": "The first life" }))?;
    s.owned(&made.body, &owner);
    let first = row_of(api)?;
    let deleted = api.signed(&owner, "DELETE", &format!("/api/f/{remade}"), None)?;
    s.create(api, &owner, &remade)?;
    let second = row_of(api)?;
    s.ok(
        "a name deleted and made again shows its new life's face in its owner's list (an app, untitled), not the face of the life before",
        made.status == 200 && first["title"] == "The first life" && first["kind"] == "chat" && deleted.status == 200 && second["role"] == "owner" && second["title"].is_null() && second["kind"] == "app",
        json!({ "first": first, "second": second }),
    );

    // who has access to what: the owner's view of all they own (GET /api/access)
    let mine = s.named(api, &owner, "members-alone")?;
    s.create(api, &owner, &mine)?;
    let waits_on = format!("waits-{}@e2e.test", &Keys::generate().pubkey_hex()[..10]);
    invite(&waits_on, "viewer")?;
    let r = api.signed(&owner, "GET", "/api/access", None)?;
    let row = |n: &str| r.body["fragments"].as_array().and_then(|a| a.iter().find(|f| f["name"] == n)).cloned().unwrap_or_default();
    let (shared, alone) = (row(&name), row(&mine));
    let carol_row = shared["members"].as_array().and_then(|m| m.iter().find(|m| m["principal"] == api.identity(&carol).unwrap_or_default().as_str())).cloned().unwrap_or_default();
    s.ok(
        "the owner's access view lists each fragment they own: who is in by email and role (not the owner), and the invites waiting",
        r.status == 200
            && carol_row["email"] == Api::email_of(&carol).as_str()
            && carol_row["role"] == "editor"
            && carol_row["kind"] == "person"
            && !shared["members"].as_array().is_some_and(|m| m.iter().any(|m| m["principal"] == api.identity(&owner).unwrap_or_default().as_str()))
            && shared["invites"].as_array().is_some_and(|i| i.iter().any(|i| i["email"] == waits_on.as_str() && i["role"] == "viewer"))
            && shared["sharing"]["visibility"] == "link",
        &shared,
    );
    s.ok("one no one else is in wakes for nothing: its list row's counts alone, no members asked", alone["name"] == mine.as_str() && alone["members"].is_null() && alone["sharing"]["guests"] == 0, &alone);
    let r = api.signed(&carol, "GET", "/api/access", None)?;
    s.ok("a member's own view lists what they own, not what is shared with them", r.status == 200 && !r.text.contains(&name), &r);
    let r = api.unsigned("GET", "/api/access", None)?;
    s.ok("unsigned, the access view is 401", r.status == 401, &r);
    let hand = Keys::generate();
    let reg = "/api/identities";
    let made = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, &owner) })))?;
    let r = api.signed(&hand, "GET", "/api/access", None)?;
    let r2 = api.signed(&hand, "GET", &format!("/api/access?for={}", api.identity(&owner)?), None)?;
    s.ok("an agent's is 403 (it is a person's view), and one naming `for` 400", made.status == 200 && r.status == 403 && r2.status == 400, format!("{r} {r2}"));

    // the mails a person can have sent are capped by the day: the cap's
    // last is mailed, one more is refused (429) and waits on no one; an
    // invite to someone who signs in already mails no one, and is made
    let sharer = api.person()?;
    let many = s.named(api, &sharer, "members-mails")?;
    s.create(api, &sharer, &many)?;
    let to = |n: u32| format!("mail-{n}-{}@e2e.test", &sharer.pubkey_hex()[..8]);
    let mut mailed = 0;
    for n in 0..limits::INVITE_MAILS_DAILY_MAX {
        let r = api.signed(&sharer, "POST", &format!("/api/f/{many}/invites"), Some(&json!({ "email": to(n), "role": "viewer" })))?;
        mailed += u32::from(r.status == 200 && !r.body["invited"].is_null());
    }
    let over = api.signed(&sharer, "POST", &format!("/api/f/{many}/invites"), Some(&json!({ "email": to(limits::INVITE_MAILS_DAILY_MAX), "role": "viewer" })))?;
    let held = api.signed(&sharer, "GET", &format!("/api/f/{many}/invites"), None)?.body["invites"].as_array().map_or(0, Vec::len);
    let known = api.signed(&sharer, "POST", &format!("/api/f/{many}/invites"), Some(&json!({ "email": Api::email_of(&carol), "role": "viewer" })))?;
    s.ok(
        &format!("a day's {} mailed invites are sent; one more is 429, saying so, and waits on no one; one to someone in already is made, mailing no one", limits::INVITE_MAILS_DAILY_MAX),
        mailed == limits::INVITE_MAILS_DAILY_MAX
            && over.code() == Some(ErrorCode::RateLimited)
            && over.text.contains("invite more tomorrow")
            && held == limits::INVITE_MAILS_DAILY_MAX as usize
            && known.status == 200
            && known.body["member"]["role"] == "viewer",
        format!("{mailed} mailed; {over}; {held} held; {known}"),
    );
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
        "at the member cap a sign-in meets the invite and is refused, and the invite waits on",
        listed(api, &over, &name)?.is_none() && waiting(&Api::email_of(&over))?,
        "",
    );
    let r = api.signed(&owner, "GET", &path("members"), None)?;
    s.ok("the fragment holds exactly the cap", r.body["members"].as_array().map(Vec::len) == Some(limits::MEMBERS_MAX), r.status);
    let r = api.status(&over, &name)?;
    s.ok("the refused person is not a member", r.status == 403, &r);
    // the claim the sign-in could not make is the registry's alarm's to try
    // again, with a backoff: a place freed, it is made, with no sign-in
    let r = api.signed(&owner, "DELETE", &path(&format!("members/{}", npub_of(&shouted))), None)?;
    let met = s.eventually(Duration::from_secs(45), || listed(api, &over, &name).ok().flatten().as_deref() == Some("viewer"));
    s.ok(
        "a place freed, the registry's alarm tries the kept claim again: they are a member with no sign-in, and the invite waits no more",
        r.status == 200 && met && !waiting(&Api::email_of(&over))?,
        json!({ "removed": r.body, "listed": listed(api, &over, &name)? }),
    );
    let r = api.signed(&owner, "GET", &path("members"), None)?;
    s.ok("the fragment holds the cap again", r.body["members"].as_array().map(Vec::len) == Some(limits::MEMBERS_MAX), r.status);
    // another refused at the cap, then made a member by their email (a place
    // freed for them): their invite, met with a stronger role held (by the
    // alarm's try, or their next sign-in's, whichever comes first; one that
    // made them a viewer first is outranked by the editor's role), is spent
    // all the same, and the owner's list counts it no more
    let late = Keys::generate();
    invite(&Api::email_of(&late), "viewer")?;
    first_sign_in(&late)?;
    let refused = listed(api, &late, &name)?.is_none() && waiting(&Api::email_of(&late))?;
    let held = counted(api)?;
    let freed = api.signed(&owner, "DELETE", &path(&format!("members/{}", npub_of(&back))), None)?;
    let added = api.signed(&owner, "PUT", &path(&format!("members/{}", Api::email_of(&late))), Some(&json!({ "role": "editor" })))?;
    api.sign_in(&Api::email_of(&late))?;
    let spent = s.eventually(Duration::from_secs(10), || counted(api).is_ok_and(|n| n.as_u64().is_some_and(|n| Some(n + 1) == held.as_u64())));
    s.ok(
        "an invite met by a member who holds a stronger role is spent: it waits no more, their role is kept, and the owner's list counts it no more",
        refused && freed.status == 200 && added.status == 200 && spent && !waiting(&Api::email_of(&late))? && listed(api, &late, &name)?.as_deref() == Some("editor"),
        format!("refused {refused}; freed {freed}; added {added}; {held} counted, then {}", counted(api)?),
    );

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
