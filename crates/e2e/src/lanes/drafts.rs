//! Drafts (docs/api.md, Drafts): a first fragment before an account. A key
//! no one holds makes a draft, which keeps to its own loop within its caps
//! and is refused everything that spends, holds a secret, reaches out or
//! shares; a person claims it at its link (signed in, its code, the key
//! approved) and it becomes theirs with its limits lifted; a second claim
//! is refused; one no one claims ends; and a create or a claim sent again
//! changes nothing. Its addresses are forged (`CF-Connecting-IP`, which the
//! local node takes as sent and Cloudflare sets itself), so it runs on a
//! local node alone.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_core::{drafts, form, npub};
use fragment_nip98::Keys;
use fragment_proto::{limits, ErrorCode};
use serde_json::{json, Value};

use super::jobs::{settle, started};
use super::signin::{who, with_session};
use crate::api::{now_ms, url_enc, Api, Call, Reply};
use crate::Suite;

const DRAFT_APP: &[u8] = include_bytes!("../../fixtures/draft.mjs");
const DRAFT_JSON: &[u8] = include_bytes!("../../fixtures/draft.json");

/// `POST /api/drafts` signed by `keys`, from `address`.
fn make(api: &Api, keys: &Keys, body: Value, address: &str) -> Result<Reply> {
    api.call(Call {
        method: "POST",
        url: format!("{}/api/drafts", api.base),
        body: Some(body.to_string().into_bytes()),
        content_type: Some("application/json"),
        keys: Some(keys),
        extra: vec![("cf-connecting-ip", address.to_string())],
        ..Call::default()
    })
}

/// The claim page's form, posted with `session` and its form token (made
/// long enough ago that its button has armed).
fn claim(api: &Api, session: &str, name: &str, code: &str) -> Result<Reply> {
    let token = form::issue(session, &format!("claim:{name}"), now_ms() - form::DELAY_MS - 50);
    api.call(Call {
        method: "POST",
        url: format!("{}/claim/{name}", api.base),
        body: Some(format!("form={}&code={}", url_enc(&token), url_enc(code)).into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: Some(format!("fragment_session={session}")),
        extra: vec![("origin", api.base.clone())],
        ..Call::default()
    })
}

/// A file write to main through the API, signed by `keys`.
fn write(api: &Api, keys: &Keys, name: &str, path: &str, bytes: &[u8]) -> Result<Reply> {
    use base64::Engine;
    let body = json!({ "files": [{ "path": path, "base64": base64::engine::general_purpose::STANDARD.encode(bytes) }] });
    api.signed(keys, "POST", &format!("/api/f/{name}/files"), Some(&body))
}

/// The claim link's code (`?code=`).
fn code_of(created: &Value) -> String {
    let claim = created["draft"]["claim"].as_str().unwrap_or("");
    claim.split_once("?code=").map(|(_, c)| c.to_string()).unwrap_or_default()
}

pub fn drafts(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("drafts", &[crate::Need::Node, crate::Need::Levers]) {
        return Ok(());
    }
    let maker = Keys::generate();
    let maker_hex = maker.pubkey_hex().to_string();

    // made with no account
    let r = make(api, &maker, json!({ "template": "todo" }), "198.51.100.1")?;
    let name = r.body["name"].as_str().unwrap_or("").to_string();
    let until = r.body["draft"]["expiresAt"].as_i64().unwrap_or(0);
    let day = limits::DRAFT_TTL_MS;
    s.ok(
        "a key no one holds makes a draft: the platform names it from the key, its maker is no one's identity, and it ends a day after unless claimed",
        r.status == 200 && name == drafts::name(&maker_hex) && r.body["owner"] == drafts::maker(&maker_hex).as_str() && (until - now_ms() - day).abs() < 60_000,
        &r,
    );
    let code = code_of(&r.body);
    s.ok("its claim link names the platform's claim page and carries its code", r.body["draft"]["claim"].as_str().is_some_and(|c| c.starts_with(&format!("{}/claim/{name}?code=", api.base))) && code.len() == 9, &r);
    let view = r.body["viewToken"].as_str().unwrap_or("").to_string();
    let again = make(api, &maker, json!({ "template": "todo" }), "198.51.100.1")?;
    s.ok("the same create again answers the same draft (one key, one draft)", again.status == 200 && again.body["name"] == name.as_str() && again.body["viewToken"] == view.as_str() && code_of(&again.body) == code, &again);
    let r = make(api, &maker, json!({ "template": "blank" }), "198.51.100.1")?;
    s.ok("and with another template it is a conflict, naming the draft it has", r.status == 409 && r.code() == Some(ErrorCode::ConflictingBody) && r.message().contains(&name), &r);
    let r = make(api, &Keys::generate(), json!({ "template": "chat" }), "198.51.100.1")?;
    s.ok("a draft starts from a template that is copied, never a blessed one", r.status == 400, &r);
    let r = api.unsigned("POST", "/api/drafts", Some(&json!({})))?;
    s.ok("an unsigned create is no draft (a draft is its key's)", r.status == 401, &r);
    let person = api.person()?;
    let r = make(api, &person, json!({}), "198.51.100.1")?;
    s.ok("a key someone holds makes fragments of theirs, not drafts", r.status == 409 && r.message().contains("someone's"), &r);

    // it works, within its limits
    let r = api.status(&maker, &name)?;
    s.ok(
        "its maker's key reads it as its owner, and its status says it is a draft, with the claim link and its code",
        r.status == 200 && r.body["role"] == "owner" && r.body["draft"]["claim"].as_str().is_some_and(|c| c.ends_with(&format!("?code={code}"))),
        &r,
    );
    let page = api.page(&name, &format!("?view={view}"), None)?;
    s.ok(
        "its page says it is a draft, when it ends, and where it is claimed (without the code), and no cache keeps it",
        page.status == 200 && page.text.contains("id=\"fragment-draft\"") && page.text.contains(&format!("{}/claim/{name}\"", api.base)) && !page.text.contains(&code) && page.header("cache-control") == "no-store",
        &page,
    );
    let r = write(api, &maker, &name, "app.mjs", DRAFT_APP)?;
    let r2 = write(api, &maker, &name, "fragment.json", DRAFT_JSON)?;
    let d = api.signed(&maker, "POST", &format!("/api/f/{name}/deploy"), Some(&json!({})))?;
    s.ok("its maker writes its files and deploys them, through the platform", r.status == 200 && r2.status == 200 && d.status == 200, format!("{r} / {r2} / {d}"));
    let r = api.op(&maker, &name, "note", "n1", json!({ "text": "one" }))?;
    let q = api.browser_op(&name, "notes", "q1", json!({}), Some(&format!("fragview={view}")))?;
    s.ok("its operations run, its maker's and a link holder's", r.status == 200 && q.status == 200 && q.body["result"]["notes"] == json!(["one"]), format!("{r} / {q}"));
    let look = api.op(&maker, &name, "look", "j1", json!({ "url": "https://example.com/" }))?;
    let looked = settle(api, &maker, &name, started(&look), &["succeeded", "held"], Duration::from_secs(30));
    s.ok("a job's fetch is refused, saying why: a draft reaches nothing outside", looked["status"] == "held" && looked["error"].as_str().is_some_and(|e| e.contains("a draft fetches nothing")), &looked);
    let ask = api.op(&maker, &name, "ask", "j2", json!({ "text": "hello" }))?;
    let asked = settle(api, &maker, &name, started(&ask), &["succeeded", "held"], Duration::from_secs(30));
    s.ok("and an AI step: a draft pays for nothing", asked["status"] == "held" && asked["error"].as_str().is_some_and(|e| e.contains("runs no AI step")), &asked);
    let mut refused = vec![];
    for (method, path, body) in [
        ("PUT", "secrets/KEY", None),
        ("GET", "secrets", None),
        ("GET", "storage-token", None),
        ("POST", "subscriptions", Some(json!({ "channel": "events", "url": "https://example.com/hook" }))),
        ("PUT", &format!("members/{}", npub::encode(person.pubkey_hex())) as &str, Some(json!({ "role": "editor" }))),
        ("POST", "invites", Some(json!({ "role": "viewer" }))),
        ("PUT", "visibility", Some(json!({ "visibility": "public" }))),
        ("POST", "rotate", Some(json!({}))),
        ("PUT", "cap", Some(json!({ "id": "c1", "micros": 1 }))),
    ] {
        let r = api.signed(&maker, method, &format!("/api/f/{name}/{path}"), body.as_ref())?;
        if r.status != 403 || r.code() != Some(ErrorCode::Forbidden) {
            refused.push(format!("{method} {path}: {r}"));
        }
    }
    s.ok("secrets, storage tokens, subscriptions, sharing and its cap wait for its claim (403, saying why)", refused.is_empty(), format!("{refused:?}"));
    let sha = fragment_core::blob::sha256_hex(b"bytes");
    let r = api.call(Call { method: "PUT", url: format!("{}/api/f/{name}/blobs/{sha}", api.base), body: Some(b"bytes".to_vec()), keys: Some(&maker), ..Call::default() })?;
    s.ok("and blobs: the key that made it signs for its draft's routes, never a blob's (401)", r.status == 401, &r);
    let r = api.call(Call {
        method: "POST",
        url: api.site_url(&name, &format!("__push-sub?view={view}")),
        body: Some(json!({ "endpoint": "https://push.example/x", "keys": { "p256dh": "k", "auth": "a" }, "who": "*" }).to_string().into_bytes()),
        content_type: Some("application/json"),
        ..Call::default()
    })?;
    s.ok("a page subscribes to no push on a draft", r.status == 403 && r.message().contains("a draft takes no push"), &r);
    let other = Keys::generate();
    let r = api.status(&other, &name)?;
    s.ok("another key no one holds reaches no draft but its own (401)", r.status == 401, &r);
    let elsewhere = api.qualified(&person, &s.name("theirs"))?;
    s.create(api, &person, &elsewhere)?;
    let r = api.status(&maker, &elsewhere)?;
    s.ok("and its maker's key reaches no fragment but its draft", r.status == 401, &r);
    let mut skipped = None;
    s.eventually(Duration::from_secs(20), || {
        let r = api.signed(&maker, "GET", &format!("/api/f/{name}/events?tail=50"), None).ok();
        skipped = r.and_then(|r| r.body["events"].as_array().and_then(|e| e.iter().find(|e| e["kind"] == "card.skipped").cloned()));
        skipped.is_some()
    });
    s.ok("its deploys are not shot for a card: no one pays", skipped.as_ref().is_some_and(|e| e["data"]["why"] == "owner_pays"), format!("{skipped:?}"));

    // its caps
    let chunk = vec![b'x'; 700 * 1024];
    let one = write(api, &maker, &name, "a.bin", &chunk)?;
    let two = write(api, &maker, &name, "b.bin", &chunk)?;
    let three = write(api, &maker, &name, "c.bin", &chunk)?;
    s.ok(
        "its files at main hold at most 2 MiB in all (413 past it)",
        one.status == 200 && two.status == 200 && three.status == 413 && three.code() == Some(ErrorCode::TooLarge),
        format!("{one} / {two} / {three}"),
    );
    let mut writes = (0, None);
    // bounded: a minute's cap, twice (the minute may turn once meanwhile)
    for i in 0..2 * limits::DRAFT_WRITES_PER_MIN + 5 {
        let r = api.op(&maker, &name, "note", &format!("w{i}"), json!({ "text": "w" }))?;
        if r.status != 200 {
            writes.1 = Some(r);
            break;
        }
        writes.0 += 1;
    }
    let (ran, stopped) = writes;
    s.ok(
        "it takes a minute's writes, then refuses them (429), saying why",
        stopped.as_ref().is_some_and(|r| r.status == 429 && r.message().contains("writes a minute")) && ran <= 2 * limits::DRAFT_WRITES_PER_MIN,
        format!("{ran} writes, then {stopped:?}", stopped = stopped.map(|r| r.to_string())),
    );
    let mut started_here = vec![];
    for _ in 0..limits::DRAFTS_PER_ADDRESS_PER_DAY {
        let k = Keys::generate();
        started_here.push((make(api, &k, json!({}), "203.0.113.7")?.status, k));
    }
    let past = make(api, &Keys::generate(), json!({}), "203.0.113.7")?;
    let v6 = make(api, &Keys::generate(), json!({}), "2001:db8:1:2::9")?;
    let replay = make(api, &started_here[0].1, json!({}), "203.0.113.7")?;
    s.ok(
        "an address starts its day's drafts, then none (429), while another starts its own and a draft made again counts once",
        started_here.iter().all(|(status, _)| *status == 200) && past.status == 429 && past.code() == Some(ErrorCode::RateLimited) && v6.status == 200 && replay.status == 200,
        format!("{:?} / {past} / {v6} / {replay}", started_here.iter().map(|(s, _)| s).collect::<Vec<_>>()),
    );

    // claimed
    let r = api.unsigned("GET", &format!("/claim/{name}?code={code}"), None)?;
    s.ok("its claim link sends a browser signed out to sign in first, and back", r.status == 302 && r.header("location").contains("/auth/login?return=") && r.header("location").contains("code"), &r);
    let paula = api.sign_in(&format!("claim-{}@e2e.test", &maker_hex[..10]))?;
    let paula_id = who(api, &paula)?["id"].as_str().unwrap_or("").to_string();
    let r = with_session(api, "GET", &format!("/claim/{name}?code={code}"), &paula)?;
    let tail = npub::encode(&maker_hex);
    s.ok(
        "signed in, the page says what claiming does, shows the ending of the key that made it, and has its code filled in",
        r.status == 200 && r.text.contains(&tail[tail.len() - 8..]) && r.text.contains(&format!("value=\"{code}\"")) && r.header("content-security-policy").contains("frame-ancestors 'none'"),
        &r,
    );
    let r = claim(api, &paula, &name, "ZZZZ-ZZZZ")?;
    let still = api.status(&maker, &name)?;
    s.ok("another code claims nothing (403)", r.status == 403 && still.body["draft"].is_object(), &r);
    let r = claim(api, &paula, &name, &code.to_lowercase())?;
    s.ok("its code claims it, typed as a person types it, and the browser goes on to it", r.status == 303 && r.header("location").contains(&format!("/auth/fragment?name={name}")), &r);
    let me = api.signed(&maker, "GET", "/api/identities/me", None)?;
    s.ok("in the same step the key that made it is the claimer's", me.status == 200 && me.body["id"] == paula_id.as_str(), &me);
    let r = api.status(&maker, &name)?;
    s.ok("and the draft is theirs: its owner, under its own name, no draft any more", r.status == 200 && r.body["owner"] == paula_id.as_str() && r.body["role"] == "owner" && r.body["draft"].is_null(), &r);
    let listed = api.signed(&maker, "GET", "/api/fragments", None)?;
    s.ok("it is on their list", listed.body["fragments"].as_array().is_some_and(|f| f.iter().any(|f| f["name"] == name.as_str() && f["role"] == "owner")), &listed);
    let secret = api.call(Call {
        method: "PUT",
        url: format!("{}/api/f/{name}/secrets/KEY", api.base),
        body: Some(b"v".to_vec()),
        keys: Some(&maker),
        ..Call::default()
    })?;
    let token = api.signed(&maker, "GET", &format!("/api/f/{name}/storage-token"), None)?;
    let page = api.page(&name, &format!("?view={view}"), None)?;
    s.ok("its limits lift: a secret, a storage token, and a page with no banner", secret.status == 200 && token.status == 200 && page.status == 200 && !page.text.contains("fragment-draft"), format!("{secret} / {token} / {page}"));
    let r = api.signed(&maker, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": started(&ask) })))?;
    let asked = settle(api, &maker, &name, started(&ask), &["succeeded"], Duration::from_secs(30));
    s.ok("and its held AI step, replayed, runs on its owner's ledger", r.status == 200 && asked["status"] == "succeeded" && asked["output"]["text"] == "echo: hello", &asked);
    let r = claim(api, &paula, &name, &code)?;
    s.ok("the claim again changes nothing: it is theirs, and on to it", r.status == 303, &r);
    let r = with_session(api, "GET", &format!("/claim/{name}"), &paula)?;
    s.ok("(its claim page says it is theirs)", r.status == 200 && r.text.contains("It is yours"), &r);
    let bob = api.sign_in(&format!("claim2-{}@e2e.test", &maker_hex[..10]))?;
    let r = claim(api, &bob, &name, &code)?;
    s.ok("a second claim, by someone else, is refused (409)", r.status == 409, &r);
    let r = api.status(&maker, &name)?;
    s.ok("(and it stays the first claimer's)", r.body["owner"] == paula_id.as_str(), &r);

    // one no one claims ends, as a delete ends it
    let late = Keys::generate();
    let r = make(api, &late, json!({ "template": "blank" }), "198.51.100.2")?;
    let lapsed = r.body["name"].as_str().unwrap_or("").to_string();
    let lever = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": lapsed, "op": "expire-draft" })))?;
    let gone = s.eventually(Duration::from_secs(20), || api.status(&late, &lapsed).is_ok_and(|r| r.status == 404));
    s.ok("an unclaimed draft ends at its end: from then it is 404", r.status == 200 && lever.status == 200 && gone, &lever);
    let (cleaned, last) = s.ended_cleaned(api, &lapsed);
    s.ok("and its life is cleaned up as a delete's is", cleaned, last);
    let r = make(api, &late, json!({ "template": "blank" }), "198.51.100.2")?;
    s.ok(
        "its key makes it again: the same name, a new life with a new end",
        r.status == 200 && r.body["name"] == lapsed.as_str() && r.body["draft"]["expiresAt"].as_i64().is_some_and(|at| at > now_ms() + day - 60_000),
        &r,
    );

    // the CLI, with no login
    let home = s.dir("drafts-cli");
    let made = s.cli_json(api, &home, &["create", "--draft", "--template", "todo", "--json"]).context("fragment create --draft")?;
    let made_name = made["name"].as_str().unwrap_or("").to_string();
    let status = s.cli_json(api, &home, &["status", &made_name, "--json"]);
    s.ok(
        "`fragment create --draft` needs no login: it makes this machine's key and its draft, and prints the claim link",
        fragment_proto::is_draft_name(&made_name) && made["draft"]["claim"].as_str().is_some_and(|c| c.contains("?code=")) && status.as_ref().is_ok_and(|v| v["draft"].is_object()),
        format!("{made} / {status:?}"),
    );
    Ok(())
}
