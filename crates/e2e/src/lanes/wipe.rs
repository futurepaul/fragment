//! An operator's wipe of a person (docs/api.md, Operators), the whole round
//! trip: a person with a username, an agent and its computer (the stub
//! locally, the deployment's own image hosted), a saved `/data`, an app, a
//! picture, a membership in someone else's fragment, and a CLI key; the
//! dry run, which changes nothing; the refusals; the person made as p5's
//! were (2026-10-08): their list and their agent's from before a list's
//! table named the channels searched, so they refuse every change, and
//! (locally) their app's repo named as before repos were their owner's; a
//! wipe stopped after its first step (the person locked meanwhile, locally
//! across a node's crash, a code.storage refusal its report names, and an
//! outage), then finished from the CLI with an operator key no one holds,
//! never waiting on the lists it empties; and the same sign-in again, a new
//! person: onboarding
//! asks for a username, the old one is free, and a new agent and computer
//! start empty, the agent's repo a fresh one under the same name.
//!
//! The wipe takes only what was the person's: the other person's fragment
//! keeps itself and its other members (an agent of theirs among them), and
//! loses only the wiped person's and their agent's memberships.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::computers::{agent_replies, lever, newest_save, phase, AGENT_JSON, CHAT_JSON, QUEUE_DRAIN};
use super::jobs::records;
use crate::api::{now_s, Api, Call};
use crate::{Need, Suite};

/// A stub's start, its restore, and its bridge's first follow (computers.rs).
const WAKE: Duration = Duration::from_secs(90);
/// A first start on a preview pulls the deployment's image and boots it.
const HOSTED_WAKE: Duration = Duration::from_secs(300);
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-a-wiped-picture";

/// The operator key's file, as `fragment operator key` writes one: its
/// secret's 64 hex, readable by its owner only.
fn key_file(s: &Suite, keys: &Keys) -> Result<std::path::PathBuf> {
    let dir = s.dir("wipe-operator");
    let path = dir.join("operator.key");
    std::fs::write(&path, format!("{}\n", keys.secret_hex()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

/// The `data` of `fragment operator wipe …` signed with the operator key's
/// file, or why it failed.
fn cli_wipe(s: &Suite, api: &Api, file: &std::path::Path, args: &[&str]) -> Result<Value> {
    let home = s.dir("wipe-cli");
    let path = file.to_string_lossy().to_string();
    let mut all = vec!["operator", "wipe"];
    all.extend_from_slice(args);
    all.extend(["--key-file", &path, "--json"]);
    s.cli_json(api, &home, &all)
}

/// A wipe's call (`POST /api/people/{person}/wipe`), signed by `keys`.
fn wipe_call(api: &Api, keys: &Keys, person: &str, body: &Value) -> Result<crate::api::Reply> {
    api.signed(keys, "POST", &format!("/api/people/{person}/wipe"), Some(body))
}

/// The dry run, signed by `keys`.
fn dry_run(api: &Api, keys: &Keys, person: &str) -> Result<crate::api::Reply> {
    api.signed(keys, "GET", &format!("/api/people/{person}/wipe"), None)
}

fn names(v: &Value) -> Vec<String> {
    v["names"].as_array().map(|l| l.iter().filter_map(|n| n.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

pub fn wipe(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("wipe", &[Need::Computers, Need::Operator]) {
        return Ok(());
    }
    let wiper = s.wiper.clone().context("Need::Operator lends an operator key")?;
    let file = key_file(s, &wiper)?;
    let scripted = !s.hosted();
    let wake = if scripted { WAKE } else { HOSTED_WAKE };

    // ---- a person, everything of theirs, and someone else (lent no paid
    // call: the run's spend reads no ledger of theirs once they are wiped)
    let paul = api.person()?;
    let email = Api::email_of(&paul);
    let (paul_id, username) = (api.identity(&paul)?, api.username(&paul)?);
    let bob = api.person()?;
    let bob_id = api.identity(&bob)?;
    // bob's own agent (a CLI's), in bob's fragment and in paul's app
    let bob_agent = Keys::generate();
    let proof = bob_agent.proof("POST", &format!("{}/api/identities", api.base), bob.pubkey_hex(), now_s());
    let r = api.signed(&bob, "POST", "/api/identities", Some(&json!({ "kind": "agent", "proof": proof })))?;
    let bob_agent_id = r.body["id"].as_str().unwrap_or("").to_string();
    s.ok("someone else, with an agent of theirs", r.status == 200 && fragment_core::npub::is_identity(&bob_agent_id), &r);
    let shared = s.named(api, &bob, "shared")?;
    s.create(api, &bob, &shared)?;

    let r = api.signed(&paul, "POST", "/api/computers", Some(&json!({})))?;
    let computer = r.body["computer"].as_str().unwrap_or("").to_string();
    let agent_name = s.named(api, &paul, "juniper")?;
    let agent = s.create(api, &paul, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON)), ("SOUL.md", Some(b"the old soul\n"))]);
    s.deploy(&agent);
    let r = api.signed(&paul, "PUT", &format!("/api/computers/{computer}/agents/{agent_name}"), Some(&json!({})))?;
    let agent_id = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    let chat_name = s.named(api, &paul, "chat")?;
    let chat = s.create(api, &paul, &chat_name)?;
    s.commit(&chat, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&chat);
    api.signed(&paul, "PUT", &format!("/api/f/{chat_name}/members/{agent_id}"), Some(&json!({ "role": "editor" })))?;
    let woke = s.eventually(wake, || phase(api, &paul, &computer) == "awake");
    s.ok("the person has a computer, awake, running their agent", woke && fragment_core::npub::is_identity(&agent_id), phase(api, &paul, &computer));
    if scripted {
        // the stub's scripted runtime writes a file under its /data
        let said = api.signed(&paul, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "w1", "body": { "text": "write notes/keep.txt kept by the old person" } })))?;
        let wrote = || agent_replies(&records(api, &paul, &chat_name, "chat"), &agent_id).into_iter().filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).find(|t| t.starts_with("wrote "));
        let answered = said.status == 200 && s.eventually(wake, || wrote().is_some());
        s.ok("its agent writes into its /data", answered && wrote().as_deref() == Some("wrote notes/keep.txt"), format!("{said} {:?}", wrote()));
    } else {
        s.skip("its agent writes into its /data", "it needs the stub's scripted runtime: the deployment's own image keeps its own state in /data");
    }
    std::thread::sleep(QUEUE_DRAIN);
    let slept = api.signed(&paul, "POST", &format!("/api/computers/{computer}/sleep"), Some(&json!({})))?;
    let saved = newest_save(api, &computer);
    s.ok("put to sleep, its /data is saved (R2)", slept.body["phase"] == "asleep" && saved["records"].as_array().is_some_and(|r| !r.is_empty()), format!("{slept} / {saved}"));
    let app_name = s.named(api, &paul, "app")?;
    let app = s.create(api, &paul, &app_name)?;
    s.commit(&app, &[("index.html", Some(b"<p>the old app</p>\n"))]);
    s.deploy(&app);
    let r = api.call(Call { method: "PUT", url: format!("{}/api/identities/me/picture", api.base), body: Some(PNG.to_vec()), keys: Some(&paul), ..Call::default() })?;
    s.ok("a picture", r.status == 200, &r);
    // their memberships elsewhere, and someone else's agent in theirs
    let joined = [
        api.signed(&bob, "PUT", &format!("/api/f/{shared}/members/{paul_id}"), Some(&json!({ "role": "editor" })))?,
        api.signed(&bob, "PUT", &format!("/api/f/{shared}/members/{agent_id}"), Some(&json!({ "role": "viewer" })))?,
        api.signed(&bob, "PUT", &format!("/api/f/{shared}/members/{bob_agent_id}"), Some(&json!({ "role": "viewer" })))?,
        api.signed(&paul, "PUT", &format!("/api/f/{app_name}/members/{bob_agent_id}"), Some(&json!({ "role": "viewer" })))?,
    ];
    s.ok("they and their agent are in someone else's fragment, and that one's agent in their app", joined.iter().all(|r| r.status == 200), json!(joined.iter().map(|r| r.status).collect::<Vec<_>>()));
    let ours = [&agent_name, &chat_name, &app_name];
    let mut repos: Vec<String> = [&agent, &chat, &app].iter().map(|c| c["repo"].as_str().unwrap_or("").to_string()).collect();

    // ---- the dry run: what a wipe deletes, and nothing changes
    let dry = cli_wipe(s, api, &file, &[&username, "--dry-run"]);
    let d = dry.as_ref().map(Value::clone).unwrap_or(Value::Null);
    let f = &d["found"];
    let owned = names(&f["fragments"]);
    let elsewhere = names(&f["memberships"]);
    s.ok(
        "the dry run names the person, their agent, fragments, memberships elsewhere, keys, sessions, sign-in, picture, computer and ledger",
        d["identity"] == paul_id.as_str()
            && d["state"] == "live"
            && d["done"] == false
            && ours.iter().all(|n| owned.contains(n))
            && f["fragments"]["count"] == ours.len()
            && elsewhere.iter().any(|m| m.starts_with(&format!("{shared} (editor, {paul_id})")))
            && elsewhere.iter().any(|m| m.starts_with(&format!("{shared} (viewer, {agent_id})")))
            && f["agents"] == json!([agent_id])
            && f["keys"].as_u64().is_some_and(|k| k >= 2)
            && f["sessions"].as_u64().is_some_and(|n| n >= 1)
            && f["signIns"] == 1
            && f["pictures"] == 1
            && f["computer"]["computer"] == computer.as_str()
            && f["computer"]["saves"].as_u64().is_some_and(|n| n >= 1)
            && f["computer"]["backups"].as_u64().is_some_and(|n| n >= 1)
            && f["ledger"] == true,
        format!("{dry:?}"),
    );
    let me = api.signed(&paul, "GET", "/api/identities/me", None)?;
    let app_status = api.status(&paul, &app_name)?;
    s.ok("the dry run changed nothing", me.status == 200 && me.body["username"] == username.as_str() && app_status.status == 200, format!("{me} {app_status}"));

    // ---- refusals, each changing nothing
    let not_operator = [dry_run(api, &bob, &username)?, wipe_call(api, &bob, &username, &json!({ "confirm": paul_id }))?];
    s.ok("someone not an operator is refused (403)", not_operator.iter().all(|r| r.status == 403), json!(not_operator.iter().map(|r| r.to_string()).collect::<Vec<_>>()));
    let r = api.unsigned("GET", &format!("/api/people/{username}/wipe"), None)?;
    s.ok("unsigned is 401", r.status == 401, &r);
    let r = wipe_call(api, &wiper, &username, &json!({ "confirm": bob_id }))?;
    s.ok("a wipe confirmed for someone else is refused (409)", r.status == 409 && r.message().contains("dry run"), &r);
    let r = wipe_call(api, &wiper, &username, &json!({}))?;
    s.ok("a wipe with no confirmation is refused (400)", r.status == 400, &r);
    let r = dry_run(api, &wiper, &agent_id)?;
    s.ok("an agent is wiped with its person, never alone (400)", r.status == 400 && r.message().contains("agent"), &r);
    let r = dry_run(api, &wiper, "nobody-here")?;
    s.ok("no one by that name is 404", r.status == 404, &r);
    let me = api.signed(&paul, "GET", "/api/identities/me", None)?;
    s.ok("and the person is as they were", me.status == 200, &me);

    // ---- made as p5's were (2026-10-08): their list and their agent's
    // from before a list's table named the channels searched (#186, its
    // migration gone with #197), so every newer change to them is refused,
    // the ended fragments' changes included; and (locally) their app's
    // repo named as before repos were their owner's. Their wipe still
    // finishes: it never waits on the lists it empties
    let made_old: Vec<crate::api::Reply> =
        [&paul_id, &agent_id].iter().map(|who| api.unsigned("POST", "/api/test/list", Some(&json!({ "identity": who, "op": "before-searched" })))).collect::<Result<_>>()?;
    s.ok(
        "their list and their agent's are lists from before `searched` (a lever)",
        made_old.iter().all(|r| r.status == 200 && r.body["columns"].as_array().is_some_and(|c| !c.contains(&json!("searched")))),
        json!(made_old.iter().map(|r| r.to_string()).collect::<Vec<_>>()),
    );
    let r = api.signed(&bob, "PUT", &format!("/api/f/{shared}/members/{paul_id}"), Some(&json!({ "role": "viewer" })))?;
    let role = |r: &crate::api::Reply| r.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == shared.as_str())).map(|f| f["role"].clone());
    let theirs_now = api.signed(&paul, "GET", "/api/fragments", None)?;
    s.ok(
        "such a list takes no change (their role elsewhere changed, their list still says the old one), as on p5",
        r.status == 200 && role(&theirs_now) == Some(json!("editor")),
        format!("{r} / {theirs_now}"),
    );
    if scripted {
        // the repo a fragment made before 2026-10-07 kept: `<label>--<username>`
        let old = fragment_proto::flat_name(&app_name).context("a fragment's flat name")?;
        s.fake.seed_repo(&old, &[("index.html", b"<p>the old app</p>\n")]);
        let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": app_name, "op": "repo", "repo": old })))?;
        s.ok("their app's repo is one named as before repos were their owner's (a lever)", r.status == 200 && r.body["repo"] == old.as_str(), &r);
        repos[2] = old;
    } else {
        s.skip("their app's repo is one named as before repos were their owner's (a lever)", "it seeds the old repo at code.storage, the fake's");
    }

    // ---- a wipe stopped after one step: the person is locked meanwhile
    let r = wipe_call(api, &wiper, &username, &json!({ "confirm": paul_id, "steps": 1 }))?;
    s.ok(
        "one step (its computer), and it stops: the person is being wiped",
        r.status == 200 && r.body["state"] == "wiping" && r.body["next"] == "fragments" && r.body["ran"][0]["step"] == "computer" && r.body["ran"][0]["done"] == true,
        &r,
    );
    let me = api.signed(&paul, "GET", "/api/identities/me", None)?;
    s.ok("their CLI key is no one's at once (401)", me.status == 401, &me);
    let signed_in = api.sign_in(&email);
    s.ok("their sign-in signs in no one while it runs", signed_in.is_err(), format!("{signed_in:?}"));
    let nameless = api.person_without_username()?;
    let r = api.signed(&nameless, "PUT", "/api/identities/me/username", Some(&json!({ "username": username })))?;
    s.ok("no one takes their username while it runs", r.status == 409, &r);
    let r = api.signed(&bob, "PUT", &format!("/api/f/{shared}/members/{agent_id}"), Some(&json!({ "role": "editor" })))?;
    s.ok("no one adds them or their agent to a fragment while it runs", r.status == 404, &r);
    let r = lever(api, &computer, "saves")?;
    s.ok("their computer is no computer from its step", r.status == 404, &r);

    // locally: the node crashes between two calls, code.storage refuses
    // the repo deletes for a call, then fails the next one: the wipe goes
    // on, and finishes
    let restarted;
    let api = match scripted {
        true => {
            s.crash()?;
            restarted = s.start(false)?;
            &restarted
        }
        false => api,
    };

    api.wiped(&paul_id);
    // locally: code.storage refuses every repo delete (a key without the
    // right): the cleanup holds each at its first refusal, and the report
    // names each fragment, what it has left, and why, never an endless
    // "still cleaning"
    if scripted {
        s.fake.refuse_repo_deletes(true);
        let r = wipe_call(api, &wiper, &username, &json!({ "confirm": paul_id }))?;
        let cleanup = r.body["ran"].as_array().and_then(|l| l.iter().find(|x| x["step"] == "cleanup")).cloned().unwrap_or(Value::Null);
        let cleaning = cleanup["cleaning"].as_array().cloned().unwrap_or_default();
        let named: Vec<&str> = cleaning.iter().filter_map(|c| c["fragment"].as_str()).collect();
        s.ok(
            "a repo delete code.storage refuses holds the cleanup, and the report says so: each fragment held, its repo left, the 403",
            r.status == 200
                && r.body["next"] == "cleanup"
                && cleanup["done"] == false
                && cleanup["note"].as_str().is_some_and(|n| n.starts_with("failed:") && n.contains("403"))
                && ours.iter().all(|n| named.contains(&n.as_str()))
                && cleaning.iter().all(|c| c["held"] == true && c["repo"] == true && c["lists"] == 0 && c["stored"] == false && c["error"].as_str().is_some_and(|e| e.starts_with("repo: ") && e.contains("403"))),
            &r,
        );
        let ended = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": app_name, "op": "ended" })))?;
        let life = &ended.body["ended"][0];
        s.ok(
            "the ended life says the same (the `ended` lever): its repo held, its lists told or never waited on",
            life["repos"] == 1 && life["lists"] == 0 && life["held"] == true && life["error"].as_str().is_some_and(|e| e.contains("403")),
            &ended,
        );
        s.fake.refuse_repo_deletes(false);
        s.fake.fail_repo_deletes(1);
    }

    // ---- the wipe, from the CLI, with an operator key no one holds
    let wiped = cli_wipe(s, api, &file, &[&username, "--yes"]);
    let w = wiped.as_ref().map(Value::clone).unwrap_or(Value::Null);
    // locally the outage holds one repo's delete back a call or more: the
    // cleanup waits for it, and the CLI calls until it is done
    let calls_min = if scripted { 2 } else { 1 };
    s.ok(
        "the CLI's wipe goes on where it stopped (through a crash, a refusal and an outage, locally; their lists from before `searched` hold nothing), and finishes with nothing left",
        w["report"]["done"] == true && w["report"]["state"] == "wiped" && w["report"]["identity"] == paul_id.as_str() && w["calls"].as_u64().is_some_and(|c| c >= calls_min),
        format!("{wiped:?}"),
    );
    let after = dry_run(api, &wiper, &paul_id)?;
    let empty = json!({ "count": 0, "names": [], "more": false });
    let f = &after.body["found"];
    s.ok(
        "wiped: nothing of theirs is left (the dry run by identity)",
        after.status == 200
            && after.body["done"] == true
            && after.body["username"].is_null()
            && f["fragments"] == empty
            && f["memberships"] == empty
            && f["keys"] == 0
            && f["sessions"] == 0
            && f["signIns"] == 0
            && f["pictures"] == 0
            && f["lists"] == 0
            && f["ledger"] == false
            && f["agents"] == json!([])
            && f["computer"]["saves"] == 0
            && f["computer"]["backups"] == 0
            && f["computer"]["snapshot"] == false,
        &after,
    );
    let again = cli_wipe(s, api, &file, &[&paul_id, "--yes"]);
    s.ok("a wipe again finds nothing left", again.as_ref().is_ok_and(|v| v["report"]["done"] == true), format!("{again:?}"));
    let r = dry_run(api, &wiper, &username)?;
    s.ok("their username names no one (404)", r.status == 404, &r);
    let r = api.unsigned("GET", &format!("/api/users/{username}"), None)?;
    s.ok("and anyone sees it is free", r.status == 404, &r);
    let cleaned: Vec<(bool, String)> = ours.iter().map(|n| s.ended_cleaned(api, n)).collect();
    s.ok("each of their fragments is gone, cleaned up (members' lists, app database, blobs, repo)", cleaned.iter().all(|c| c.0), json!(cleaned.iter().map(|c| c.1.clone()).collect::<Vec<_>>()));
    if scripted {
        s.ok("each of their repos is deleted at code.storage", repos.iter().all(|r| s.fake.repo_deleted(r)), json!(repos));
    } else {
        s.skip("each of their repos is deleted at code.storage", "it reads code.storage's own record, the fake's: the hosted lane holds no key to it");
    }
    let r = lever(api, &computer, "saves")?;
    s.ok("their computer is no computer", r.status == 404, &r);
    let members = api.signed(&bob, "GET", &format!("/api/f/{shared}/members"), None)?;
    let principals: Vec<String> = members.body["members"].as_array().map(|l| l.iter().filter_map(|m| m["principal"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    s.ok(
        "someone else's fragment keeps itself and its other members, and loses only theirs",
        members.status == 200 && principals.len() == 2 && principals.contains(&bob_id) && principals.contains(&bob_agent_id),
        &members,
    );
    let theirs = api.signed(&bob_agent, "GET", "/api/fragments", None)?;
    let listed: Vec<String> = theirs.body["fragments"].as_array().map(|l| l.iter().filter_map(|f| f["name"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    s.ok("someone else's agent keeps its key and its list, less the wiped app", theirs.status == 200 && listed.contains(&shared) && !listed.contains(&app_name), &theirs);
    let r = api.signed(&bob, "PUT", &format!("/api/f/{shared}/members/{agent_id}"), Some(&json!({ "role": "viewer" })))?;
    s.ok("their agent's identity names no one", r.status == 404, &r);

    // ---- the same sign-in again: a new person
    let session = api.sign_in(&email)?;
    let fresh = Keys::generate();
    let approved = api.approve_link(&session, &api.approval_link(&fresh, 0))?;
    let me = api.signed(&fresh, "GET", "/api/identities/me", None)?;
    let fresh_id = me.body["id"].as_str().unwrap_or("").to_string();
    s.ok(
        "the same sign-in is a new person, whom onboarding asks for a username",
        approved.status == 200 && me.status == 200 && fragment_core::npub::is_identity(&fresh_id) && fresh_id != paul_id && me.body["username"].is_null() && me.body["agents"].as_array().is_none_or(Vec::is_empty),
        &me,
    );
    let r = api.signed(&fresh, "PUT", "/api/identities/me/username", Some(&json!({ "username": username })))?;
    s.ok("their old username is free: they take it again", r.status == 200 && r.body["claimed"] == true, &r);
    let ledger = api.signed(&fresh, "GET", "/api/ledger", None)?;
    s.ok("a new ledger, on the deployment's plan for a new person", ledger.status == 200 && ledger.body["plan"] == crate::DEFAULT_PLAN && ledger.body["fragments"] == json!([]), &ledger);
    let r = api.signed(&fresh, "POST", "/api/computers", Some(&json!({})))?;
    let fresh_computer = r.body["computer"].as_str().unwrap_or("").to_string();
    s.ok(
        "a new computer, never saved",
        r.status == 200 && fresh_computer.starts_with("computer:") && fresh_computer != computer && r.body["saves"] == json!([]) && r.body["phase"] == "asleep",
        &r,
    );
    // the same agent fragment's name: a repo of its own, with nothing old in it
    let made = api.create(&fresh, &agent_name)?;
    let fresh_repo = made.body["repo"].as_str().unwrap_or("").to_string();
    let files = api.signed(&fresh, "GET", &format!("/api/f/{agent_name}/files"), None)?;
    s.ok(
        "the same agent's name again: a fresh repo, holding nothing of the old one",
        made.status == 200 && !fresh_repo.is_empty() && !repos.contains(&fresh_repo) && files.body["files"] == json!([]),
        format!("{made} {files}"),
    );
    if scripted {
        let named = fragment_core::codestorage::repo_name("", &agent_name, &fresh_id).unwrap_or_default();
        s.ok("its repo is named for its new owner", s.fake.repo_url(&named).as_deref() == Some(fresh_repo.as_str()), &named);
    } else {
        s.skip("its repo is named for its new owner", "it reads code.storage's own record, the fake's");
    }
    s.owned(&made.body, &fresh);
    s.commit(&made.body, &[("fragment.json", Some(AGENT_JSON)), ("SOUL.md", Some(b"a new soul\n"))]);
    s.deploy(&made.body);
    let soul = api.signed(&fresh, "GET", &format!("/api/f/{agent_name}/file?path=SOUL.md"), None)?;
    s.ok("its files are the new person's only", soul.status == 200 && soul.text == "a new soul\n", &soul);
    let r = api.signed(&fresh, "PUT", &format!("/api/computers/{fresh_computer}/agents/{agent_name}"), Some(&json!({})))?;
    let fresh_agent = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    s.ok("a new agent", r.status == 200 && fragment_core::npub::is_identity(&fresh_agent) && fresh_agent != agent_id, &r);
    let made_chat = api.create(&fresh, &chat_name)?;
    s.owned(&made_chat.body, &fresh);
    s.commit(&made_chat.body, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&made_chat.body);
    api.signed(&fresh, "PUT", &format!("/api/f/{chat_name}/members/{fresh_agent}"), Some(&json!({ "role": "editor" })))?;
    let woke = s.eventually(wake, || phase(api, &fresh, &fresh_computer) == "awake");
    let view = api.signed(&fresh, "GET", &format!("/api/computers/{fresh_computer}"), None)?;
    s.ok("its computer wakes having restored nothing: an empty /data", woke && view.body["restored"]["from"] == "nothing", &view);
    if scripted {
        let said = api.signed(&fresh, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "w2", "body": { "text": "read notes/keep.txt" } })))?;
        let read = || agent_replies(&records(api, &fresh, &chat_name, "chat"), &fresh_agent).into_iter().filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).find(|t| t.starts_with("read "));
        let answered = said.status == 200 && s.eventually(wake, || read().is_some());
        s.ok("the old person's file is in no /data of the new one's", answered && read().as_deref() == Some("read notes/keep.txt: none"), format!("{said} {:?}", read()));
    } else {
        s.skip("the old person's file is in no /data of the new one's", "it needs the stub's scripted runtime to read a file back");
    }
    std::thread::sleep(QUEUE_DRAIN);
    let r = api.signed(&fresh, "POST", &format!("/api/computers/{fresh_computer}/sleep"), Some(&json!({})))?;
    s.ok("and sleeps", r.body["phase"] == "asleep", &r);
    Ok(())
}
