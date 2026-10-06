//! State survives a graceful restart and a crash of the node (a sleeping
//! job, sealed secrets and keys, sessions, and a channel's sequence
//! included; on a run with sandcastle nodes, a computer's placement, awake
//! through the restart); then the node runs without hostnames and serves
//! fragments from `/f/<name>/`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use fragment_fakes::http::{Handler, Response, Server};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs;
use super::signin::{site_cookie, who};
use crate::api::{Api, Call};
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");
const LEDGER_APP: &[u8] = include_bytes!("../../fixtures/ledger.mjs");
const LEDGER_JSON: &[u8] = include_bytes!("../../fixtures/ledger.json");
const SECRET: &str = "sk-e2e-restart-5b2e07";
const MEMBER_EMAIL: &str = "restart-member@e2e.test";
/// A person WorkOS signed in before sign-in was OpenID Connect.
const KEPT_EMAIL: &str = "restart-kept@e2e.test";
/// A node that dials in is back once it dials again: after a run's many
/// restarts its backoff nears its ceiling (a minute, and jitter), and a
/// dial may wait out its own bound (sandcastle's docs/node.md, The uplink).
const NODE_BACK: Duration = Duration::from_secs(180);

/// Its owner's wake of a computer placed on a node, again while its node is
/// not back (each such answer `node_down`, typed): the last answer.
fn woken(s: &Suite, api: &Api, who: &Keys, id: &str) -> crate::api::Reply {
    let wake = || api.signed(who, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})));
    let mut last = None;
    s.eventually(NODE_BACK, || {
        let r = wake();
        let done = r.as_ref().is_ok_and(|r| r.status == 200 && r.body["phase"] == "awake");
        if let Ok(r) = r {
            last = Some(r);
        }
        done
    });
    last.unwrap_or_else(|| wake().unwrap_or_else(|e| panic!("a wake: {e:#}")))
}

/// The checks of a computer placed on a sandcastle node, across the restarts.
const PLACED: [&str; 3] = [
    "after a restart a computer on a sandcastle node is still placed there, and awake: its new object finds its container on the node, and puts it to sleep",
    "woken after the restart, it wakes on the node it was placed on",
    "after a crash it is still placed on its node, and wakes there",
];

fn count(api: &Api, keys: &Keys, name: &str) -> i64 {
    api.op(keys, name, "count", "q", json!({})).ok().and_then(|r| r.body["result"]["n"].as_i64()).unwrap_or(-1)
}

pub fn restart(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("restart", &[crate::Need::Node, crate::Need::Deployment, crate::Need::Levers]) {
        return Ok(());
    }
    let api = s.api();
    let owner = api.person()?;
    // the member signs in with a browser too: its sessions must outlive the restart
    let member_session = api.sign_in(MEMBER_EMAIL)?;
    let member = Keys::generate();
    api.approve(&member_session, &member)?;
    let name = s.named(&api, &owner, "restart")?;
    let c = s.create(&api, &owner, &name)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", member.pubkey_hex()), Some(&json!({ "role": "editor" })))?;
    let live = ship(s, &c, TODO_APP, TODO_JSON);
    // a site session on the fragment, and the fragment's VAPID key (sealed like a secret)
    let member_site = site_cookie(&api, &member_session, &name)?;
    let push_key = |api: &Api| api.page(&name, "__push-key", Some(&format!("fragment_site={member_site}")));
    let r = push_key(&api)?;
    anyhow::ensure!(r.status == 200 && r.body["key"].is_string(), "push key setup: {r}");
    let vapid = r.body["key"].clone();
    let first = api.op(&owner, &name, "add_todo", "r1", json!({ "text": "survives" }))?;
    // the registry: a key added, and one revoked, before the restart
    let (added, revoked) = (Keys::generate(), api.person()?);
    let add = "/api/identities/me/keys";
    let r = api.signed(&revoked, "POST", add, Some(&json!({ "proof": api.proof(&added, "POST", add, &revoked) })))?;
    let r2 = api.signed(&added, "DELETE", &format!("/api/identities/me/keys/{}", revoked.pubkey_hex()), None)?;
    anyhow::ensure!(r.status == 200 && r2.status == 200, "registry setup: {r} {r2}");
    // a ledger: a month with something spent
    let paid = s.named(&api, &owner, "restart-paid")?;
    let pc = s.create(&api, &owner, &paid)?;
    ship(s, &pc, LEDGER_APP, LEDGER_JSON);
    // the deploy lands by the webhook: a query answers once it has
    s.eventually(Duration::from_secs(30), || api.op(&owner, &paid, "notes", "q", json!({})).is_ok_and(|r| r.status == 200));
    let r = api.op(&owner, &paid, "summarize", "before", json!({ "text": "before the restart" }))?;
    jobs::settle(&api, &owner, &paid, jobs::started(&r), &["succeeded"], Duration::from_secs(40));
    // an operator's grant and the fragment's cap: commands the ledger keeps
    let op_session = api.sign_in("operator@e2e.test")?;
    let op_id = api.approve(&op_session, &s.operator)?.body["id"].as_str().unwrap_or("").to_string();
    let owner_id = api.identity(&owner)?;
    let grant = json!({ "id": "restart-g", "micros": 1_000_000, "by": op_id, "why": "before the restart" });
    let r = api.signed(&s.operator, "POST", &format!("/api/ledger/{owner_id}/grant"), Some(&grant))?;
    let cap = json!({ "id": "restart-c", "micros": 7_000_000 });
    let r2 = api.signed(&owner, "PUT", &format!("/api/f/{paid}/cap"), Some(&cap))?;
    anyhow::ensure!(r.status == 200 && r2.status == 200, "ledger setup: {r} {r2}");
    let balance = api.signed(&owner, "GET", "/api/ledger", None)?.body["balanceMicros"].clone();
    // a paused operation: its pause is a row, not a cached list
    let r = api.signed(&owner, "POST", &format!("/api/f/{paid}/pause"), Some(&json!({ "op": "summarize", "paused": true })))?;
    anyhow::ensure!(r.status == 200, "pause setup: {r}");
    // a fragment with code, triggers, and a channel
    let (stored, sc) = jobs::jobs_fragment(s, &api, &owner, "restart-code", |_| {})?;
    // a sealed secret its job fetches with, and a channel with records in it
    let r = api.call(Call { method: "PUT", url: format!("{}/api/f/{stored}/secrets/API_KEY", api.base), body: Some(SECRET.as_bytes().to_vec()), keys: Some(&owner), ..Call::default() })?;
    anyhow::ensure!(r.status == 200, "secret setup: {r}");
    let r = api.op(&owner, &stored, "save", "before", json!({ "texts": ["one", "two"], "source": "restart" }))?;
    let seq_before = jobs::records(&api, &owner, &stored, "feed").last().and_then(|r| r["seq"].as_i64()).unwrap_or(0);
    anyhow::ensure!(r.status == 200 && seq_before == 2, "channel setup: {r} (feed at {seq_before})");
    let code_before = api.status(&owner, &stored)?.body["code"].clone();
    anyhow::ensure!(code_before["operations"]["save"]["kind"] == "mutation", "code setup: {code_before}");
    // a person's search and archiving (principal.rs), and a chat's search outbox (search.rs)
    let talk = s.named(&api, &owner, "restart-talk")?;
    let r = api.create_with(&owner, json!({ "name": talk, "template": "chat" }))?;
    anyhow::ensure!(r.status == 200, "chat setup: {r}");
    let say = |api: &Api, id: &str, text: &str| api.signed(&owner, "POST", &format!("/api/f/{talk}/channels/chat"), Some(&json!({ "id": id, "body": { "text": text } })));
    let found = |api: &Api, q: &str| {
        let r = api.signed(&owner, "GET", &format!("/api/search?q={q}"), None);
        r.ok().and_then(|r| r.body["messages"].as_array().map(|l| l.iter().filter(|m| m["fragment"] == talk.as_str()).count())).unwrap_or(0)
    };
    let said = s.eventually(Duration::from_secs(30), || say(&api, "k1", "kale outlives restarts").is_ok_and(|r| r.status == 200));
    let r = api.signed(&owner, "PUT", &format!("/api/fragments/{talk}/archived"), Some(&json!({ "archived": true })))?;
    let searched = s.eventually(Duration::from_secs(30), || found(&api, "kale") == 1);
    anyhow::ensure!(said && r.status == 200 && searched, "search setup: {r}");

    // a person WorkOS signed in before sign-in was OpenID Connect, kept as
    // that sign-in kept them (docs/self-host.md, seam 4): AuthKit's runs
    let kept = match s.oidc_signin() || s.hosted() {
        false => Some(super::signin::kept_workos_person(s, &api, KEPT_EMAIL)?),
        true => None,
    };
    // a sign-in begun and answered by the provider, finished only after the
    // restart (it keeps its verifier and nonce in the registry; its
    // provider's metadata and keys are fetched again)
    let pending = api.unsigned("GET", "/auth/login?return=/after-restart&login_hint=restart-pending@e2e.test", None)?;
    let pending_cookie = pending.cookies().into_iter().find(|c| c.starts_with("fragment_login=")).unwrap_or_default();
    let pending_back = api.external(&pending.header("location"))?;
    anyhow::ensure!(pending.status == 302 && pending_back.status == 302, "a pending sign-in: {pending} / {pending_back}");

    // a computer placed on a sandcastle node, awake as the platform stops
    // (docs/self-host.md, seam 2): its container is the node's
    let placed = match s.has_nodes() {
        true => {
            let who = api.person()?;
            let r = api.signed(&who, "POST", "/api/computers", Some(&json!({})))?;
            let id = r.body["computer"].as_str().unwrap_or("").to_string();
            let r = api.signed(&who, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
            let node = r.body["node"].as_str().unwrap_or("").to_string();
            anyhow::ensure!(r.status == 200 && r.body["phase"] == "awake" && !node.is_empty(), "a computer on a node: {r}");
            Some((who, id, node))
        }
        false => {
            for label in PLACED {
                s.skip(label, "it needs sandcastle nodes, and the run started none (FRAGMENT_E2E_NODES=two)");
            }
            None
        }
    };

    s.stop()?;
    let api = s.start(false, true)?;
    if let Some((who, id, node)) = &placed {
        let v = api.signed(who, "GET", &format!("/api/computers/{id}"), None)?;
        let r = api.signed(who, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
        s.ok(PLACED[0], v.body["node"] == node.as_str() && v.body["phase"] == "awake" && r.status == 200 && r.body["phase"] == "asleep", format!("{v} / {r}"));
        let r = woken(s, &api, who, id);
        s.ok(PLACED[1], r.status == 200 && r.body["phase"] == "awake" && r.body["node"] == node.as_str(), &r);
    }
    let r = api.call(Call { method: "GET", url: pending_back.header("location"), cookie: Some(pending_cookie), ..Call::default() })?;
    let signed = r.cookies().into_iter().find_map(|c| c.strip_prefix("fragment_session=").map(str::to_string));
    let shown = match &signed {
        Some(session) => who(&api, session)?,
        None => Value::Null,
    };
    s.ok(
        "a sign-in begun before a restart finishes after it, as the person it began for",
        r.status == 302 && r.header("location").ends_with("/after-restart") && shown.to_string().contains("restart-pending@e2e.test"),
        format!("{r} / {shown}"),
    );
    match &kept {
        Some(kept) => {
            let session = api.sign_in(KEPT_EMAIL)?;
            let me = who(&api, &session)?;
            s.ok("after a restart a person kept from before sign-in was OpenID Connect signs in through AuthKit as themselves", me["id"] == kept.as_str(), &me);
        }
        None => s.skip(
            "after a restart a person kept from before sign-in was OpenID Connect signs in as themselves",
            "they are WorkOS's, made by the registry's levers: this run signs people in through the strict OpenID Connect fake, or keeps the hosted lane's rules",
        ),
    }
    let r = api.op(&owner, &name, "add_todo", "r1", json!({ "text": "survives" }))?;
    s.ok("after a restart the replay returns the stored result", r.body["replayed"] == true && r.body["result"] == first.body["result"], &r);
    s.ok("after a restart the app's rows survive", count(&api, &owner, &name) == 1, "count");
    let r = api.status(&member, &name)?;
    s.ok("after a restart members survive", r.status == 200 && r.body["role"] == "editor", &r);
    let r = api.signed(&added, "GET", "/api/identities/me", None)?;
    s.ok("after a restart the registry still knows an added key", r.status == 200, &r);
    let r = api.signed(&revoked, "GET", "/api/fragments", None)?;
    s.ok("and a revoked key stays revoked", r.status == 401, &r);
    let r = api.signed(&owner, "GET", "/api/ledger", None)?;
    let steps = api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": owner_id, "op": "entries", "prefix": format!("step:{paid}@") })))?;
    let settled = steps.body["entries"].as_array().is_some_and(|e| e.len() == 1 && e[0]["entry"]["end"]["end"] == "settled");
    // the fragments' meters land when their batches do: requests and a few
    // dynamic workers' days (3000 µ$ each) may have charged the owner since
    let kept = r.body["balanceMicros"].as_i64().zip(balance.as_i64()).is_some_and(|(now, then)| now <= then && then - now < 50_000 && then < 51_000_000 && then > 50_000_000);
    s.ok("after a restart the ledger is what it was: the step's charge settled, the balance less it", r.status == 200 && settled && kept, json!({ "ledger": r.body, "then": balance, "steps": steps.body }));
    let capped = r.body["fragments"].as_array().is_some_and(|f| f.iter().any(|f| f["fragment"] == paid.as_str() && f["capMicros"] == 7_000_000));
    let again = api.signed(&s.operator, "POST", &format!("/api/ledger/{owner_id}/grant"), Some(&grant))?;
    let other = api.signed(&s.operator, "POST", &format!("/api/ledger/{owner_id}/grant"), Some(&json!({ "id": "restart-g", "micros": 2_000_000, "by": op_id, "why": "" })))?;
    let cap_again = api.signed(&owner, "PUT", &format!("/api/f/{paid}/cap"), Some(&cap))?;
    let after = api.signed(&owner, "GET", "/api/ledger", None)?;
    s.ok(
        "and its commands: the cap holds, a grant or a cap again changes nothing, another body under a grant's id is 409",
        capped && again.status == 200 && other.status == 409 && cap_again.status == 200 && after.body["purchasedMicros"] == r.body["purchasedMicros"],
        format!("{again} | {other} | {cap_again} | {}", after.body),
    );

    let r = api.signed(&owner, "GET", &format!("/api/f/{paid}/runs?limit=1"), None)?;
    s.ok("after a restart a paused operation is still paused", r.status == 200 && r.body["paused"] == json!(["summarize"]), &r);
    let r = api.status(&owner, &stored)?;
    s.ok("after a restart the installed code is the same: its operations and schemas", r.body["code"] == code_before, &r.body["code"]);
    let r = api.signed(&owner, "GET", &format!("/api/f/{stored}/triggers"), None)?;
    let on: Vec<Value> = r.body["triggers"].as_array().into_iter().flatten().map(|t| t["run"].clone()).collect();
    s.ok("and its triggers, in their order", on == vec![json!("ingest"), json!("ping"), json!("boom"), json!("tick")], &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{stored}/channels/feed"), None)?;
    s.ok("and its channels", r.status == 200, &r);
    let r = api.op(&owner, &stored, "save", "after", json!({ "texts": ["three"], "source": "restart" }))?;
    let next = jobs::records(&api, &owner, &stored, "feed").iter().find(|r| r["body"]["text"] == "three").and_then(|r| r["seq"].as_i64());
    s.ok("after a restart a channel's sequence goes on: the next record is n + 1", r.status == 200 && next == Some(seq_before + 1), format!("{next:?} after {seq_before}"));
    let token = sc["inboxToken"].as_str().unwrap_or("");
    let r = jobs::inbox(&api, &stored, token, &json!({ "source": "migrated", "payload": { "items": ["after the move"] } }), None)?;
    let run = r.body["runs"][0].as_i64().unwrap_or(0);
    let ran = jobs::settle(&api, &owner, &stored, run, &["succeeded", "held"], Duration::from_secs(40));
    let items = api.op(&owner, &stored, "items", "q", json!({}))?;
    s.ok(
        "and its channel trigger starts its run",
        ran["status"] == "succeeded" && items.body["result"].as_array().is_some_and(|a| a.iter().any(|i| i["text"] == "after the move" && i["source"] == "migrated")),
        &items,
    );
    // a job's fetch opens the secret sealed before the restart
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = Arc::clone(&seen);
    let handler: Handler = Arc::new(move |req| {
        log.lock().expect("upstream log").push(req.header("authorization").unwrap_or("").to_string());
        Response::json(200, &json!({ "items": ["fetched after the restart"] }))
    });
    let upstream = Server::start(0, handler)?;
    let r = api.op(&owner, &stored, "digest", "after", json!({ "url": format!("{}/data", upstream.url) }))?;
    let ran = jobs::settle(&api, &owner, &stored, jobs::started(&r), &["succeeded", "held"], Duration::from_secs(40));
    let got = seen.lock().expect("upstream log").clone();
    s.ok("after a restart a sealed secret opens: the job's fetch carries it", ran["status"] == "succeeded" && got == [format!("Bearer {SECRET}")], format!("{ran} {got:?}"));
    // a browser's sessions, and the key its push subscriptions were made with
    let r = who(&api, &member_session)?;
    s.ok("after a restart a browser's platform session still signs it in", r.to_string().contains(MEMBER_EMAIL), &r);
    let r = push_key(&api)?;
    s.ok("and its site session on a fragment still works", r.status == 200, &r);
    s.ok("the fragment's VAPID key is the one it had (sealed, and opened again)", r.body["key"].is_string() && r.body["key"] == vapid, format!("{} vs {vapid}", r.body["key"]));
    s.ok("after a restart a person's search finds what it found", found(&api, "kale") == 1, "");
    let r = api.signed(&owner, "GET", "/api/fragments", None)?;
    let held = r.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"] == talk.as_str() && f["archived"] == true));
    s.ok("and their archiving holds", held, &r);
    let r = say(&api, "k2", "kale and chard")?;
    let next = s.eventually(Duration::from_secs(30), || found(&api, "kale") == 2);
    s.ok("and a chat's new message reaches it: its search outbox goes on", r.status == 200 && next, &r);

    let r = api.op(&owner, &name, "add_todo", "r2", json!({ "text": "before the crash" }))?;
    s.ok("a mutation before the crash", r.status == 200, &r);
    // Local workerd's Workflows keep a sleep as a timer in the process
    // (miniflare's engine: no alarm behind it), so one sleeping through a
    // crash of `wrangler dev` never wakes; Cloudflare's do, and celld's
    // (each instance a cell, its sleep an alarm: docs/self-host.md).
    let nap = match s.durable_workflows() {
        true => {
            let (jobs, _) = jobs::jobs_fragment(s, &api, &owner, "restart-jobs", |_| {})?;
            let r = api.op(&owner, &jobs, "nap", "through-the-crash", json!({ "ms": 4000 }))?;
            let started = jobs::started(&r);
            std::thread::sleep(Duration::from_millis(1000));
            Some((jobs, started))
        }
        false => {
            s.skip("a job sleeping through a crash wakes and finishes, once", "local Workflows do not outlive their process");
            None
        }
    };
    s.crash()?;
    let api = s.start(false, true)?;
    if let Some((who, id, node)) = &placed {
        // awake as the platform crashed: its new object adopts the container, or starts it again, there
        let r = woken(s, &api, who, id);
        s.ok(PLACED[2], r.status == 200 && r.body["phase"] == "awake" && r.body["node"] == node.as_str(), &r);
        api.signed(who, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    }
    if let Some((jobs, started)) = nap {
        let woke = jobs::settle(&api, &owner, &jobs, started, &["succeeded", "held"], Duration::from_secs(60));
        let naps = jobs::records(&api, &owner, &jobs, "feed").iter().filter(|r| r["kind"] == "nap").count();
        s.ok("a job sleeping through a crash wakes and finishes, once", woke["status"] == "succeeded" && naps == 1, format!("{woke} ({naps} nap records)"));
    }
    let r = api.op(&owner, &name, "add_todo", "r2", json!({ "text": "before the crash" }))?;
    s.ok("after a crash an acknowledged mutation replays", r.body["replayed"] == true, &r);
    s.ok("after a crash no acknowledged write is lost", count(&api, &owner, &name) == 2, "count");
    let r = api.status(&owner, &name)?;
    s.ok("after a crash pins and code survive", r.body["pins"]["live"] == live.as_str() && r.body["code"]["sha"] == live.as_str(), &r);
    let r = api.signed(&member, "GET", "/api/fragments", None)?;
    s.ok("after a crash the member's list survives", r.text.contains(&name), &r);
    s.ok("and a person's search, both messages in it", found(&api, "kale") == 2, "");
    s.commit(&c, &[("after.md", Some(b"webhooks still land"))]);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/file?path=after.md"), None)?;
    s.ok("after a crash webhooks still move the pins", r.status == 200 && r.text == "webhooks still land", &r);
    openbao_restart(s)
}

/// The checks of OpenBao behind the cell's secrets, across its stop and
/// start (docs/self-host.md, seam 12).
const OPENBAO: [&str; 2] = [
    "with OpenBao down, a sign-in's callback fails at once, naming OpenBao: the cell's read of the client's secret is an error, never a hang",
    "OpenBao started again unseals itself from its key file, initialised once, and the next sign-in reads the secret: the cell not restarted",
];

/// A sign-in through the provider, to its callback's answer.
fn callback(api: &Api, email: &str) -> Result<crate::api::Reply> {
    let start = api.unsigned("GET", &format!("/auth/login?return=/&login_hint={}", crate::api::url_enc(email)), None)?;
    let bound = start.cookies().into_iter().find(|c| c.starts_with("fragment_login=")).unwrap_or_default();
    let back = api.external(&start.header("location"))?;
    anyhow::ensure!(start.status == 302 && back.status == 302, "a sign-in begun: {start} / {back}");
    api.call(Call { method: "GET", url: back.header("location"), cookie: Some(bound), ..Call::default() })
}

/// OpenBao down, then up, under a node started fresh: no isolate holds the
/// sign-in client's secret, which only a sign-in's code exchange reads.
fn openbao_restart(s: &mut Suite) -> Result<()> {
    if !s.has_openbao() {
        for label in OPENBAO {
            s.skip(label, "the run's secrets are not OpenBao's (FRAGMENT_E2E_SECRETS=vars, or wrangler's own store)");
        }
        return Ok(());
    }
    s.stop()?;
    let api = s.start(false, true)?;
    s.openbao_down()?;
    let t0 = std::time::Instant::now();
    let r = callback(&api, "openbao-down@e2e.test")?;
    let took = t0.elapsed();
    s.ok(OPENBAO[0], r.status >= 500 && r.text.contains("OpenBao") && r.text.contains("is not answering") && took < Duration::from_secs(10), format!("{r} in {took:?}"));
    let up = s.openbao_up()?;
    let r = callback(&api, "openbao-up@e2e.test")?;
    let signed = r.cookies().into_iter().any(|c| c.starts_with("fragment_session=") && c.len() > "fragment_session=".len() + 1);
    let once = s.openbao_initialisations()?;
    s.ok(OPENBAO[1], r.status == 302 && signed && once == 1, format!("{r} (OpenBao up again in {up:?}, initialised {once} time(s))"));
    Ok(())
}

/// A fleet without a hostname suffix serves fragments under `/f/<name>/`.
pub fn pathmode(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("pathmode", &[crate::Need::Node]) {
        return Ok(());
    }
    s.stop()?;
    let api = s.start(false, false)?;
    let owner = api.person()?;
    let name = s.named(&api, &owner, "paths")?;
    let c = s.create(&api, &owner, &name)?;
    s.ok("without a suffix the canonical URL is a path", c["canonical"] == format!("{}/f/{name}/", api.base), &c);
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "public" })))?;
    s.commit(&c, &[("site/index.html", Some(b"<p>by path</p>")), ("app.mjs", Some(include_bytes!("../../fixtures/guestbook.mjs"))), ("fragment.json", Some(include_bytes!("../../fixtures/guestbook.json")))]);
    s.deploy(&c);
    let r = api.page(&name, "", None)?;
    s.ok("a page is served by path", r.status == 200 && r.text.contains("by path"), &r);
    let session = api.sign_in(&Api::email_of(&owner))?;
    let frame = vec![("sec-fetch-dest", "iframe".to_string()), ("sec-fetch-mode", "navigate".to_string()), ("sec-fetch-site", "same-origin".to_string())];
    let url = format!("{}/auth/frame?name={name}&return=/", api.base);
    let r = api.call(Call { method: "GET", url, cookie: Some(format!("fragment_session={session}")), extra: frame, ..Call::default() })?;
    s.ok(
        "the frame mint refuses (403): every fragment shares the platform's origin here, so a fragment's page is the platform's own",
        r.status == 403 && !r.header("location").contains("__signin"),
        &r,
    );
    let r = api.browser_op(&name, "sign", "p1", json!({ "text": "hi" }), None)?;
    s.ok("a browser call works by path", r.status == 200, &r);
    s.ok("its cookie is scoped to the fragment's path", r.header("set-cookie").contains(&format!("Path=/f/{name}/;")), r.header("set-cookie"));
    let r = api.call(Call { method: "GET", url: format!("{}/f/{name}", api.base), ..Call::default() })?;
    s.ok("the bare path redirects to the trailing slash", r.status == 308 && r.header("location") == format!("{}/f/{name}/", api.base), &r);
    s.stop()?;
    Ok(())
}
