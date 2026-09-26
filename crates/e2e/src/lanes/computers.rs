//! Computers as identities: a machine its owner pairs (`fragment login
//! --computer`, approved on the platform's `/cli` page, which says what it
//! is) signs as a computer of its own, never as its owner. It reaches none
//! of its owner's fragments until they add it; then it calls, syncs, and
//! deploys there like any member. What it makes is its owner's, on their
//! budget; it approves no keys, shares nothing, and makes no agents. Its
//! owner lists and removes it, and a removed computer is refused from its
//! next request, having left every fragment it was in.

use std::path::Path;
use std::process::Output;
use std::time::Duration;

use anyhow::{Context, Result};
use fragment_core::npub;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::{settle, started};
use super::signin::with_session;
use crate::api::{now_s, url_enc, Api};
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");

/// The link `fragment login --computer <name>` prints for `keys`: its
/// proof names the computer in the URL it signs.
fn computer_link(api: &Api, keys: &Keys, name: &str) -> String {
    let proof = keys.header("POST", &format!("{}/cli/approve?computer={name}", api.base), b"", now_s());
    let proof = proof.strip_prefix("Nostr ").unwrap_or(&proof).to_string();
    format!("{}/cli?key={}&proof={}&computer={name}", api.base, npub::encode(keys.pubkey_hex()), url_enc(&proof))
}

fn out(o: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

fn dir_of(p: &Path) -> String {
    p.to_str().expect("a UTF-8 path").to_string()
}

pub fn computers(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("computers") {
        return Ok(());
    }
    let owner_home = s.dir("computers-owner");
    s.login(api, &owner_home);
    let owner = s.cli_keys(&owner_home).context("the owner's CLI logged in")?;
    let owner_id = api.identity(&owner)?;
    let session = api.sign_in(&crate::cli_email(&npub::encode(owner.pubkey_hex()))?)?;

    // pairing: the machine makes its key and the link; its owner approves it
    let home = s.dir("computers-machine");
    let pending = s.cli_json(api, &home, &["login", "--computer", "builder", "--no-wait", "--json"])?;
    let link = pending["approve"].as_str().unwrap_or("").to_string();
    s.ok("login --computer makes a key and a link naming the computer", pending["pending"] == true && link.contains("&computer=builder"), &pending);
    let r = with_session(api, "GET", link.trim_start_matches(&api.base), &session)?;
    s.ok(
        "the page says plainly it is a computer named builder, which acts only where let, never as its owner",
        r.status == 200 && r.text.contains("your computer <b>builder</b>") && r.text.contains("never acts as you") && r.text.contains("Pair builder"),
        &r,
    );
    let r = api.approve_link(&session, &link.replace("&computer=builder", ""))?;
    s.ok("its proof names the computer: without the name, it joins no one", r.status == 400, &r);
    let r = api.approve_link(&session, &link)?;
    s.ok("its owner pairs it", r.status == 200 && r.text.contains("is your computer now"), &r);
    let done = s.cli_json(api, &home, &["login", "--computer", "builder", "--no-browser", "--json"])?;
    let computer_id = done["id"].as_str().unwrap_or("").to_string();
    s.ok(
        "the machine's login finishes as the computer, owned by them",
        done["kind"] == "computer" && done["name"] == "builder" && done["owner"] == owner_id.as_str(),
        &done,
    );
    let computer = s.cli_keys(&home).context("the machine has a key")?;
    let r = api.approve_link(&session, &link)?;
    s.ok("approving it again pairs nothing new", r.status == 200, &r);
    let r = api.signed(&computer, "GET", "/api/identities/me", None)?;
    s.ok(
        "its key signs as the computer, not the person",
        r.body["id"] == computer_id.as_str() && r.body["kind"] == "computer" && r.body["username"].is_null(),
        &r,
    );
    let r = api.approve_link(&session, &api.approval_link(&computer, 0))?;
    s.ok("and never joins the person too", r.status == 409, &r);
    let r = api.approve_link(&session, &computer_link(api, &Keys::generate(), "builder"))?;
    s.ok("a second computer named builder is refused", r.status == 409 && r.text.contains("computers rm builder"), &r);
    let listed = s.cli_json(api, &owner_home, &["computers", "--json"])?;
    s.ok("the owner lists it", listed["computers"].as_array().is_some_and(|c| c.len() == 1 && c[0]["name"] == "builder" && c[0]["id"] == computer_id.as_str()), &listed);

    // none of its owner's fragments until they add it
    let app = s.named(api, &owner, "computer-app")?;
    let c = s.create(api, &owner, &app)?;
    ship(s, &c, TODO_APP, TODO_JSON);
    let r = api.op(&computer, &app, "add_todo", "c1", json!({ "text": "from the computer" }))?;
    s.ok("it cannot call its owner's fragment before they add it", r.status == 403, &r);
    let work = s.dir("computers-work");
    let o = s.cli(api, &home, &["sync", &app, "--dir", &dir_of(&work)]);
    s.ok("nor sync it", !o.status.success(), out(&o));
    let added = s.cli_json(api, &owner_home, &["members", "add", &app, "builder", "--role", "editor", "--json"])?;
    s.ok(
        "its owner adds it by its name, as a member like anyone",
        added["principal"] == computer_id.as_str() && added["kind"] == "computer" && added["owner"] == owner_id.as_str(),
        &added,
    );
    let r = api.op(&computer, &app, "add_todo", "c1", json!({ "text": "from the computer" }))?;
    s.ok("then it calls there", r.status == 200, &r);
    let o = s.cli(api, &home, &["sync", &app, "--dir", &dir_of(&work)]);
    s.ok("and syncs", o.status.success() && work.join("app.mjs").exists(), out(&o));
    std::fs::write(work.join("index.html"), "<h1>from the computer</h1>")?;
    let before = api.status(&owner, &app)?.body["pins"]["live"].clone();
    let o = s.cli(api, &home, &["deploy", &app, "--dir", &dir_of(&work)]);
    let r = api.status(&owner, &app)?;
    s.ok("and deploys", o.status.success() && r.body["pins"]["live"] != before && r.body["pins"]["live"] == r.body["pins"]["main"], out(&o));

    // nothing else of its owner's
    let other = Keys::generate();
    let r = api.signed(&computer, "PUT", &format!("/api/f/{app}/members/{}", npub::encode(other.pubkey_hex())), Some(&json!({ "role": "viewer" })))?;
    s.ok("it changes no member of a fragment it edits", r.status == 403, &r);
    let own_keys = format!("/api/identities/{computer_id}/keys");
    let r = api.signed(&computer, "POST", &own_keys, Some(&json!({ "proof": api.proof(&other, "POST", &own_keys, &computer) })))?;
    s.ok("it approves no key: not its own", r.status == 403, &r);
    let owner_keys = format!("/api/identities/{owner_id}/keys");
    let r = api.signed(&computer, "POST", &owner_keys, Some(&json!({ "proof": api.proof(&other, "POST", &owner_keys, &computer) })))?;
    s.ok("nor its owner's", r.status == 403, &r);
    let r = api.signed(&computer, "POST", "/api/identities", Some(&json!({ "kind": "agent", "proof": api.proof(&other, "POST", "/api/identities", &computer) })))?;
    s.ok("it registers no agent", r.status == 403, &r);
    let r = api.signed(&computer, "POST", "/api/agents", Some(&json!({ "name": "helper" })))?;
    s.ok("nor makes one", r.status == 403, &r);
    let r = api.signed(&computer, "GET", &format!("/api/f/{app}/status?for={owner_id}"), None)?;
    s.ok("nor acts for anyone", r.status == 403, &r);

    // what it makes is its owner's, on their budget
    let made = s.cli_json(api, &home, &["create", "made-by-builder", "--json"])?;
    let made_name = made["name"].as_str().unwrap_or("").to_string();
    s.hook(api, &made);
    let r = api.status(&owner, &made_name)?;
    s.ok("it makes a fragment, which is its owner's, under their username", r.status == 200 && r.body["role"] == "owner" && r.body["owner"] == owner_id.as_str(), &r);
    let r = api.status(&computer, &made_name)?;
    s.ok("with the computer an editor", r.body["role"] == "editor", &r);
    let listed = s.eventually(Duration::from_secs(5), || {
        api.signed(&owner, "GET", "/api/fragments", None).is_ok_and(|r| r.body["fragments"].as_array().is_some_and(|a| a.iter().any(|f| f["name"] == made_name.as_str() && f["role"] == "owner")))
    });
    s.ok("in its owner's list", listed, &made_name);
    let site = s.dir("computers-made");
    std::fs::write(site.join("app.mjs"), include_bytes!("../../fixtures/budget.mjs"))?;
    std::fs::write(site.join("fragment.json"), include_bytes!("../../fixtures/budget.json"))?;
    let o = s.cli(api, &home, &["deploy", &made_name, "--dir", &dir_of(&site)]);
    s.ok("it deploys what it made", o.status.success(), out(&o));
    let spent = |keys: &Keys| api.signed(keys, "GET", "/api/budget", None).map(|r| r.body["spentMicros"].as_i64().unwrap_or(-1)).unwrap_or(-1);
    let was = spent(&owner);
    let r = api.op(&computer, &made_name, "summarize", "c-ai", json!({ "text": "one" }))?;
    let run: Value = settle(api, &computer, &made_name, started(&r), &["succeeded", "held"], Duration::from_secs(40));
    s.ok("its AI there bills its owner", run["status"] == "succeeded" && was >= 0 && spent(&owner) > was, &run);
    let r = api.signed(&computer, "GET", "/api/budget", None)?;
    s.ok("and the computer reads no budget", r.status == 403, &r);

    // removed: its keys revoked, it leaves every fragment, it is refused
    let removed = s.cli_json(api, &owner_home, &["computers", "rm", "builder", "--json"])?;
    let left = removed["left"].as_array().cloned().unwrap_or_default();
    s.ok(
        "its owner removes it, and it leaves every fragment it was in",
        removed["removed"] == true && left.contains(&json!(app)) && left.contains(&json!(made_name)) && removed["failed"] == json!([]),
        &removed,
    );
    let r = api.op(&computer, &app, "add_todo", "c2", json!({ "text": "after" }))?;
    s.ok("its next request is refused", r.status == 401 && r.message().contains("revoked"), &r);
    let o = s.cli(api, &home, &["list"]);
    s.ok("and its CLI's", !o.status.success(), out(&o));
    let r = api.signed(&owner, "GET", &format!("/api/f/{app}/members"), None)?;
    s.ok("no grant names it", r.body["members"].as_array().is_some_and(|m| m.iter().all(|m| m["principal"] != computer_id.as_str())), &r);
    let r = api.status(&owner, &made_name)?;
    s.ok("what it made stays its owner's", r.body["role"] == "owner", &r);
    let r = api.signed(&owner, "POST", &own_keys, Some(&json!({ "proof": api.proof(&other, "POST", &own_keys, &owner) })))?;
    s.ok("no key joins it again", r.status == 409, &r);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{app}/members/{computer_id}"), Some(&json!({ "role": "editor" })))?;
    s.ok("nor is it granted anything again", r.status == 404, &r);
    let r = api.signed(&owner, "DELETE", &format!("/api/identities/{computer_id}"), None)?;
    s.ok("removing it again changes nothing", r.status == 200 && r.body["removed"] == false, &r);
    let listed = s.cli_json(api, &owner_home, &["computers", "--json"])?;
    s.ok("and its owner lists it no more", listed["computers"] == json!([]), &listed);
    Ok(())
}
