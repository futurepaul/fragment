//! Identity at the router (NIP-98), fragment create and delete, who may
//! create, and what the router refuses to let through.

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::json;

use crate::api::{now_s, Api, Call};
use crate::Suite;

/// Until sign-in exists, a fleet lets only the keys it lists create
/// fragments (`FRAGMENT_CREATORS`); a list that does not parse lets nobody.
/// Runs last: it restarts the node twice.
pub fn creators(s: &mut Suite) -> Result<()> {
    if !s.section("creators") {
        return Ok(());
    }
    if s.node.is_some() {
        s.stop()?;
    }
    let (invited, other) = (Keys::generate(), Keys::generate());
    s.creators = Some(format!("{}, {}", fragment_core::npub::encode(other.pubkey_hex()), invited.pubkey_hex()));
    let api = s.start(false, true)?;
    let name = s.name("invited");
    let r = api.create(&invited, &name)?;
    s.ok("a listed key creates a fragment (npubs and hex both name keys)", r.status == 200, &r);
    let stranger = Keys::generate();
    let r = api.create(&stranger, &s.name("uninvited"))?;
    s.ok("anyone else is refused, saying why", r.status == 403 && r.message().contains("by invitation"), &r);
    let r = api.signed(&invited, "PUT", &format!("/api/f/{name}/members/{}", stranger.pubkey_hex()), Some(&json!({ "role": "editor" })))?;
    let joined = api.status(&stranger, &name)?;
    s.ok("the list governs creating only: a stranger can still be a member", r.status == 200 && joined.status == 200, &joined);
    s.stop()?;
    s.creators = Some("npub1notakey".into());
    let api = s.start(false, true)?;
    let r = api.create(&invited, &s.name("misconfigured"))?;
    s.ok("a list that does not parse lets nobody create", r.status == 500 && r.message().contains("FRAGMENT_CREATORS"), &r);
    s.stop()?;
    s.creators = None;
    Ok(())
}

pub fn auth(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("auth") {
        return Ok(());
    }
    let keys = Keys::generate();
    let name = s.name("auth");
    let body = json!({ "name": name, "fragmentSecret": Keys::generate().secret_hex() });
    let bytes = body.to_string().into_bytes();
    let r = api.unsigned("POST", "/api/fragments", Some(&body))?;
    s.ok("an unsigned create is 401", r.status == 401 && r.error() == "unauthenticated", &r);
    // behind a proxy that ends TLS (Fly's), the client signed the https URL
    let proxied = Keys::generate();
    let https = format!("{}/api/fragments", api.base.replacen("http://", "https://", 1));
    let pbody = json!({ "name": s.name("proxied"), "fragmentSecret": Keys::generate().secret_hex() });
    let pbytes = pbody.to_string().into_bytes();
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/api/fragments", api.base),
        body: Some(pbytes.clone()),
        content_type: Some("application/json"),
        extra: vec![("authorization", proxied.header("POST", &https, &pbytes, now_s())), ("x-forwarded-proto", "https".into())],
        ..Call::default()
    })?;
    s.ok(
        "behind a TLS proxy (x-forwarded-proto) the https URL is the signed one, and links are https",
        r.status == 200 && r.body["canonical"].as_str().is_some_and(|c| c.starts_with("https://")),
        &r,
    );
    let signed_as = |url: String, body: &[u8], at: i64| keys.header("POST", &url, body, at);
    let send = |auth: String| {
        api.call(Call {
            method: "POST",
            url: format!("{}/api/fragments", api.base),
            body: Some(bytes.clone()),
            content_type: Some("application/json"),
            extra: vec![("authorization", auth)],
            ..Call::default()
        })
    };
    let r = send(signed_as(format!("{}/api/f/{name}/status", api.base), &bytes, now_s()))?;
    s.ok("a signature for another URL is 401", r.status == 401, &r);
    let r = send(signed_as(format!("{}/api/fragments", api.base), &bytes, now_s() - 120))?;
    s.ok("a two-minute-old signature is 401", r.status == 401, &r);
    let r = send(signed_as(format!("{}/api/fragments", api.base), br#"{"name":"x"}"#, now_s()))?;
    s.ok("a signature over another body is 401", r.status == 401, &r);
    let r = api.unsigned("GET", "/api/fragments", None)?;
    s.ok("an unsigned list is 401", r.status == 401, &r);
    let r = api.signed(&keys, "GET", "/api/fragments", None)?;
    s.ok("a new key lists no fragments", r.status == 200 && r.body["fragments"] == json!([]), &r);
    Ok(())
}

pub fn create(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("create") {
        return Ok(());
    }
    let owner = Keys::generate();
    let other = Keys::generate();
    let name = s.name("create");
    let r = api.create(&owner, &name)?;
    let c = &r.body;
    s.ok("a signed create succeeds", r.status == 200, &r);
    s.ok("create names the owner by npub", c["owner"] == fragment_core::npub::encode(owner.pubkey_hex()), &r);
    s.ok("create returns the fragment's own npub", c["npub"].as_str().is_some_and(|n| n.starts_with("npub1")), &r);
    s.ok("create defaults to link visibility", c["visibility"] == "link", &r);
    s.ok(
        "create returns the share, inbox, and webhook tokens",
        c["viewToken"].as_str().is_some_and(|t| t.len() == 24)
            && c["inboxToken"].as_str().is_some_and(|t| t.len() == 32)
            && c["webhookSecret"].as_str().is_some_and(|t| t.len() == 32),
        &r,
    );
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    s.ok("create returns the url-form repo identity", repo.len() == 36 && repo.matches('-').count() == 4, &r);
    s.ok("the repo exists in code.storage under the name", s.fake.repo_url(&name).as_deref() == Some(repo.as_str()), &repo);
    s.ok(
        "create returns the fragment's own origin",
        c["canonical"] == format!("http://{name}.{}:{}/", crate::SUFFIX, api.port),
        &r,
    );
    s.hook(api, c);

    let r = api.create(&other, &name)?;
    s.ok("creating an existing name is 409", r.status == 409 && r.error() == "already_exists", &r);
    let r = api.create(&owner, "Bad_Name")?;
    s.ok("an invalid name is 400", r.status == 400 && r.error() == "invalid_request", &r);
    let r = api.create_with(&owner, json!({ "name": s.name("nosecret") }))?;
    s.ok("a create without fragmentSecret is refused naming it", r.status == 400 && r.message().contains("fragmentSecret"), &r);
    let r = api.create_with(&owner, json!({ "name": s.name("badsecret"), "fragmentSecret": "0".repeat(64) }))?;
    s.ok("an invalid fragment secret is 400", r.status == 400 && r.message().contains("fragmentSecret"), &r);
    let public = s.name("create-pub");
    let r = api.create_with(&owner, json!({ "name": public, "fragmentSecret": Keys::generate().secret_hex(), "visibility": "public" }))?;
    s.ok("create takes a visibility", r.status == 200 && r.body["visibility"] == "public", &r);

    let r = api.status(&owner, &name)?;
    s.ok(
        "the owner reads status",
        r.status == 200 && r.body["role"] == "owner" && r.body["pins"]["main"].is_null() && r.body["code"]["sha"].is_null()
            && r.body["counts"]["members"] == 1,
        &r,
    );
    s.ok("the owner sees the inbox token", r.body["inboxToken"] == c["inboxToken"], &r);
    let r = api.status(&other, &name)?;
    s.ok("another key reading status is 403", r.status == 403 && r.error() == "forbidden", &r);
    let r = api.status(&owner, &s.name("nobody"))?;
    s.ok("an unknown fragment is 404", r.status == 404 && r.error() == "not_found", &r);
    let r = api.signed(&owner, "GET", "/api/fragments", None)?;
    let listed = r.body["fragments"].as_array().cloned().unwrap_or_default();
    s.ok(
        "the owner's list has both fragments as owner",
        [&name, &public].iter().all(|n| listed.iter().any(|f| f["name"] == **n && f["role"] == "owner")),
        &r,
    );
    let r = api.signed(&owner, "PUT", &format!("/api/f/{name}/code"), Some(&json!({ "sha": "x", "source": "", "operations": {} })))?;
    s.ok("installing code by PUT is gone (code comes from live)", r.status == 404, &r);
    let r = api.op(&owner, &name, "add_todo", "a1", json!({ "text": "x" }))?;
    s.ok("an operation before any deploy is 404 no_code", r.status == 404 && r.error() == "no_code", &r);

    let r = api.signed(&other, "DELETE", &format!("/api/f/{name}"), None)?;
    s.ok("another key deleting is 403", r.status == 403, &r);
    let r = api.signed(&owner, "DELETE", &format!("/api/f/{name}"), None)?;
    s.ok("the owner deletes", r.status == 200 && r.body["deleted"] == name.as_str(), &r);
    let r = api.status(&owner, &name)?;
    s.ok("a deleted fragment is 404", r.status == 404, &r);
    let r = api.signed(&owner, "GET", "/api/fragments", None)?;
    s.ok("a deleted fragment leaves the owner's list", !r.text.contains(&format!("\"{name}\"")), &r);
    // a busy org: the repo is on a later page of the org's newest-first list
    s.fake.seed_filler(150);
    let r = api.create(&other, &name)?;
    s.ok("a deleted name can be created again", r.status == 200 && r.body["owner"] == fragment_core::npub::encode(other.pubkey_hex()), &r);
    s.ok("created again, it keeps its repo (found past the list's first page)", r.body["repo"] == repo.as_str(), &r);
    Ok(())
}

pub fn lockdown(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("lockdown") {
        return Ok(());
    }
    let owner = Keys::generate();
    let stranger = Keys::generate();
    let name = s.name("lock");
    s.create(api, &owner, &name)?;
    let forged = |keys: Option<&Keys>| {
        api.call(Call {
            method: "GET",
            url: format!("{}/api/f/{name}/status", api.base),
            keys,
            extra: vec![("x-fragment-principal", owner.pubkey_hex().to_string())],
            ..Call::default()
        })
    };
    let r = forged(Some(&stranger))?;
    s.ok("a client's x-fragment-principal is ignored (signed: 403)", r.status == 403, &r);
    let r = forged(None)?;
    s.ok("a client's x-fragment-principal is ignored (unsigned: 401)", r.status == 401, &r);
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/create"), Some(&json!({ "name": name })))?;
    s.ok("the supervisor's create is not a public route", r.status == 404, &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}"), None)?;
    s.ok("GET on a fragment's root is 404", r.status == 404, &r);
    let r = api.call(Call { method: "GET", url: format!("http://evil.example.com:{}/", api.port), ..Call::default() })?;
    s.ok("a host outside the suffix is the platform, not a fragment", r.status == 404 && r.message().contains("no route"), &r);
    for host in ["Bad_Name", "a.b"] {
        let r = api.call(Call { method: "GET", url: format!("http://{host}.{}:{}/", crate::SUFFIX, api.port), ..Call::default() })?;
        s.ok(&format!("host {host}.<suffix> is not a fragment"), r.status == 404 && r.message().contains("no route"), &r);
    }
    let r = api.call(Call {
        method: "POST",
        url: format!("{}/api/f/{name}/webhook", api.base),
        body: Some(br#"{"ref":"refs/heads/main"}"#.to_vec()),
        content_type: Some("application/json"),
        extra: vec![("x-pierre-event", "push".into())],
        ..Call::default()
    })?;
    s.ok("an unsigned webhook is 401", r.status == 401, &r);
    // Refused from its declared length before it is read: the client may see
    // the 413, or the connection closing while it is still sending.
    let big = json!({ "name": s.name("big"), "fragmentSecret": "x".repeat(3 * 1024 * 1024) });
    let refused = match api.create_with(&owner, big) {
        Ok(r) => r.status == 413 && r.error() == "too_large",
        Err(e) => format!("{e:#}").contains("reset") || format!("{e:#}").contains("Broken pipe"),
    };
    s.ok("a body over 2 MiB is refused unread (413)", refused, "");
    let r = api.call(Call {
        method: "GET",
        url: api.site_url(&name, ""),
        extra: vec![("authorization", "Nostr garbage".into())],
        ..Call::default()
    })?;
    s.ok("a bad signature on a site request is 401, not anonymous", r.status == 401, &r);
    Ok(())
}
