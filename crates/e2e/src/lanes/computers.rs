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
use crate::api::{now_s, url_enc, Api, Socket};
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
    let o = s.cli(api, &owner_home, &["members", "list", &app]);
    s.ok("members list says it is a computer, and whose", out(&o).contains(&format!("{computer_id}\tcomputer of {owner_id}")), out(&o));
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

    // the model, through the platform: the agents' model, its owner's key and budget
    let o = s.cli(api, &home, &["model", "hello from the computer"]);
    s.ok("it asks the model through the platform", o.status.success() && out(&o).contains("echo: hello from the computer"), out(&o));
    let org = format!("org:{}", owner_id.trim_start_matches("id:"));
    let key = s.openrouter.minted().into_iter().find(|k| k.name.contains(&org)).map(|k| format!("Bearer {}", k.key)).unwrap_or_default();
    let call = s.openrouter.calls().into_iter().rev().find(|c| c.1 == "/api/v1/chat/completions").unwrap_or_default();
    s.ok("on the agents' model, with its owner's OpenRouter key", call.2 == fragment_proto::AGENT_MODEL && !key.is_empty() && call.3 == key, format!("{call:?}"));
    let usage = api.signed(&owner, "GET", "/api/budget/usage", None)?;
    let billed = usage.body["usage"].as_array().is_some_and(|u| u.iter().any(|u| u["kind"] == "computer.text" && u["fragment"] == computer_id.as_str() && u["state"] == "settled"));
    s.ok("billed to its owner, naming the computer", billed, &usage);
    let r = api.signed(&owner, "POST", "/api/model/chat/completions", Some(&json!({ "messages": [{ "role": "user", "content": "hi" }] })))?;
    s.ok("a person does not call the model here", r.status == 403, &r);

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

/// Whether `f` holds within the suite's wait for a computer.
fn soon(s: &Suite, f: impl FnMut() -> bool) -> bool {
    s.eventually(Duration::from_secs(20), f)
}

/// A fragment's own computer, on the Sprites fake: a deploy that declares
/// it makes a Sprite, which installs the CLI's release and pairs itself
/// (its key made there, with a single-use token on its stdin) as a
/// computer its owner owns, an editor of the fragment. A page viewer keeps
/// it awake, each tick charged to the owner; after the last one leaves it
/// sleeps. Dropping the block keeps it, asleep; `computers rm` destroys it.
pub fn sprites(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("sprites") {
        return Ok(());
    }
    let home = s.dir("sprites-owner");
    s.login(api, &home);
    let owner = s.cli_keys(&home).context("the owner's CLI logged in")?;
    let owner_id = api.identity(&owner)?;
    let made = s.cli_json(api, &home, &["create", "pet", "--json"])?;
    let name = made["name"].as_str().unwrap_or("").to_string();
    s.hook(api, &made);
    let site = s.dir("sprites-site");
    std::fs::write(site.join("index.html"), "<h1>pet</h1>")?;
    std::fs::write(site.join("fragment.json"), r#"{"computer":{}}"#)?;
    let before = s.sprites.sprites().len();
    let usage = |kind: &str| -> usize {
        let r = api.signed(&owner, "GET", "/api/budget/usage", None).map(|r| r.body).unwrap_or_default();
        r["usage"].as_array().map_or(0, |u| u.iter().filter(|u| u["kind"] == kind && u["fragment"] == name.as_str()).count())
    };
    // Sprites refusing (a bad token): each boot fails before a Sprite runs, and charges nothing
    s.sprites.refuse(true);
    let o = s.cli(api, &home, &["deploy", &name, "--dir", &dir_of(&site)]);
    s.ok("a deploy that declares a computer goes live", o.status.success(), out(&o));
    let failed = soon(s, || {
        let events = api.signed(&owner, "GET", &format!("/api/f/{name}/events?tail=50"), None).map(|r| r.body).unwrap_or_default();
        events["events"].as_array().map_or(0, |e| e.iter().filter(|e| e["kind"] == "computer.failed").count()) >= 2
    });
    s.ok("boots Sprites refuses fail, say so, and charge nothing", failed && usage("computer.awake") == 0, usage("computer.awake"));
    s.sprites.refuse(false);

    // made, booted, and paired: the owner's, an editor here
    let members = || api.signed(&owner, "GET", &format!("/api/f/{name}/members"), None).map(|r| r.body["members"].clone()).unwrap_or_default();
    let paired = soon(s, || members().as_array().is_some_and(|m| m.iter().any(|m| m["kind"] == "computer" && m["role"] == "editor")));
    let sprites = s.sprites.sprites();
    let (sprite, fake) = sprites.iter().last().map(|(n, f)| (n.clone(), f.clone())).unwrap_or_default();
    let events = api.signed(&owner, "GET", &format!("/api/f/{name}/events?tail=50"), None)?;
    let said: Vec<String> = events.body["events"].as_array().into_iter().flatten().map(|e| format!("{}: {}", e["kind"], e["summary"])).collect();
    s.ok("it makes one Sprite, which installs the CLI and pairs as an editor of the fragment", paired && sprites.len() == before + 1, format!("{sprites:?} {} {said:?}", members()));
    let listed = s.cli_json(api, &home, &["computers", "--json"])?;
    let computer = listed["computers"].as_array().and_then(|c| c.iter().find(|c| c["name"] == name.as_str())).cloned().unwrap_or_default();
    let computer_id = computer["id"].as_str().unwrap_or("").to_string();
    s.ok("its owner's, named by its fragment", !computer_id.is_empty(), &listed);
    let its_home = s.scratch.join("sprites/sprites").join(&sprite);
    let key = s.cli_keys(&its_home).context("the Sprite's CLI holds its key")?;
    let r = api.signed(&key, "GET", "/api/identities/me", None)?;
    s.ok("its key, made on it, signs as the computer", r.body["id"] == computer_id.as_str() && r.body["owner"] == owner_id.as_str(), &r);
    let token = fake.stdin.trim().to_string();
    let pair = |keys: &Keys| api.signed(keys, "POST", "/api/computers/pair", Some(&json!({ "token": token })));
    let r = pair(&Keys::generate())?;
    s.ok("its pairing token is spent: another key is refused", r.status == 401, &r);
    let r = pair(&key)?;
    s.ok("the same key again pairs the same computer", r.status == 200 && r.body["id"] == computer_id.as_str(), &r);
    let ready = soon(s, || {
        let events = api.signed(&owner, "GET", &format!("/api/f/{name}/events?tail=50"), None).map(|r| r.body).unwrap_or_default();
        events["events"].as_array().is_some_and(|e| e.iter().any(|e| e["kind"] == "computer.ready" && e["data"]["sprite"] == sprite.as_str()))
    });
    s.ok("the fragment's events say so, naming the Sprite KEYS reaches", ready, &sprite);

    // awake while a page is open, each tick charged; asleep after
    let held = |s: &Suite| s.sprites.sprites().get(&sprite).is_some_and(|f| f.held);
    s.ok("with no page open it is not held awake", !held(s), "");
    let own = Socket::open(api, &name, "__live", Some(&key), None)?;
    std::thread::sleep(Duration::from_secs(2 * crate::COMPUTER_TICK_S as u64 + 1));
    s.ok("nor with its own socket open (a computer's, as a pet follows its channel): that is no page", !held(s), "");
    own.close();
    std::thread::sleep(Duration::from_secs(2 * crate::COMPUTER_TICK_S as u64 + 1));
    s.ok("and its close wakes it no more than its opening", !held(s), "");
    let page = Socket::open(api, &name, "__live", Some(&owner), None)?;
    s.ok("a page open holds it awake", soon(s, || held(s)), "");
    s.ok("each tick awake is charged to its owner at list price", soon(s, || usage("computer.awake") >= 3), usage("computer.awake"));
    let events = api.signed(&owner, "GET", &format!("/api/f/{name}/events?tail=50"), None)?;
    let unheld = events.body["events"].as_array().is_some_and(|e| e.iter().any(|e| e["kind"] == "computer.unheld"));
    s.ok("its Tasks API shows the hold", !unheld, &events);
    // open longer than the idle wait: its viewer, not its opening, keeps it
    std::thread::sleep(Duration::from_secs(u64::from(crate::COMPUTER_IDLE_S + 2 * crate::COMPUTER_TICK_S)));
    let fake = s.sprites.sprites().get(&sprite).cloned().unwrap_or_default();
    s.ok("a page open past the idle wait still holds it", fake.held && fake.releases == 0, format!("{fake:?}"));
    page.close();
    let closed = std::time::Instant::now();
    s.ok("the page closed, it sleeps after the idle wait", soon(s, || !held(s) && s.sprites.sprites().get(&sprite).is_some_and(|f| f.releases >= 1)), "");
    let (grace, idle) = (closed.elapsed(), Duration::from_secs(u64::from(crate::COMPUTER_IDLE_S)));
    s.ok("the idle wait after the last viewer left, not a tick more", grace >= idle && grace < idle + Duration::from_millis(1500), format!("{grace:?}"));
    let calls = s.sprites.sprites().get(&sprite).map(|f| f.calls);
    std::thread::sleep(Duration::from_secs(u64::from(2 * crate::COMPUTER_TICK_S)));
    s.ok("asleep, the platform calls its Sprite no more", s.sprites.sprites().get(&sprite).map(|f| f.calls) == calls, format!("{calls:?}"));

    // dropped from fragment.json: kept, asleep; removed: destroyed
    std::fs::write(site.join("fragment.json"), "{}")?;
    s.cli(api, &home, &["deploy", &name, "--dir", &dir_of(&site)]);
    let page = Socket::open(api, &name, "__live", Some(&owner), None)?;
    std::thread::sleep(Duration::from_secs(2 * crate::COMPUTER_TICK_S as u64 + 1));
    s.ok("dropped from fragment.json, it is kept and not woken", s.sprites.sprites().contains_key(&sprite) && !held(s), "");
    page.close();
    let removed = s.cli_json(api, &home, &["computers", "rm", &name, "--json"])?;
    s.ok("computers rm destroys its Sprite", soon(s, || s.sprites.deleted().contains(&sprite)), &removed);
    let r = api.signed(&key, "GET", "/api/fragments", None)?;
    s.ok("and its key is refused", r.status == 401, &r);
    Ok(())
}
