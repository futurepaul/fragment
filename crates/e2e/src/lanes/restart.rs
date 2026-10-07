//! State survives a graceful restart and a crash of the node (a sleeping
//! job, sealed secrets and keys, sessions, and a channel's sequence
//! included).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use fragment_fakes::http::{Handler, Response, Server};
use fragment_nip98::Keys;
use fragment_proto::limits;
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
    // the deploy lands: a query answers once it has
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

    s.stop()?;
    let api = s.start(false)?;
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
    // crash of `wrangler dev` never wakes; Cloudflare's do.
    s.skip("a job sleeping through a crash wakes and finishes, once", "local Workflows do not outlive their process");
    // a delete whose cleanup the crash cuts short (cell ended.rs): the
    // fragment's alarm finishes it after
    let ended = s.named(&api, &owner, "restart-ended")?;
    s.create(&api, &owner, &ended)?;
    api.signed(&owner, "PUT", &format!("/api/f/{ended}/members/{}", member.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": ended, "op": "members", "fill": limits::MEMBERS_MAX })))?;
    anyhow::ensure!(r.status == 200, "members setup: {r}");
    let r = api.signed(&owner, "DELETE", &format!("/api/f/{ended}"), None)?;
    anyhow::ensure!(r.status == 200, "delete setup: {r}");
    let left = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": ended, "op": "ended" })))?;
    let lists_left = left.body["ended"][0]["lists"].as_i64().unwrap_or(0);
    s.crash()?;
    let api = s.start(false)?;
    let r = api.op(&owner, &name, "add_todo", "r2", json!({ "text": "before the crash" }))?;
    s.ok("after a crash an acknowledged mutation replays", r.body["replayed"] == true, &r);
    s.ok("after a crash no acknowledged write is lost", count(&api, &owner, &name) == 2, "count");
    let r = api.status(&owner, &name)?;
    s.ok("after a crash pins and code survive", r.body["pins"]["live"] == live.as_str() && r.body["code"]["sha"] == live.as_str(), &r);
    let r = api.signed(&member, "GET", "/api/fragments", None)?;
    s.ok("after a crash the member's list survives", r.text.contains(&name), &r);
    s.ok("and a person's search, both messages in it", found(&api, "kale") == 2, "");
    s.commit(&c, &[("after.md", Some(b"refreshes still land"))]);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/file?path=after.md"), None)?;
    s.ok("after a crash a refresh still moves the pins", r.status == 200 && r.text == "refreshes still land", &r);
    let (cleaned, last) = s.ended_cleaned(&api, &ended);
    s.ok(&format!("after a crash a delete's cleanup it cut short finishes ({lists_left} lists were left to tell)"), lists_left > 0 && cleaned, last);
    let r = api.signed(&member, "GET", "/api/fragments", None)?;
    s.ok("and the deleted fragment has left its member's list", r.status == 200 && !r.text.contains(&format!("\"{ended}\"")), &r);
    Ok(())
}
