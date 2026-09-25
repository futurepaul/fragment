//! Identity at the router (NIP-98), fragment create and delete, who may
//! create, and what the router refuses to let through.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use fragment_nip98::Keys;
use fragment_proto::{limits, routed, ErrorCode};
use serde_json::json;

use crate::api::{now_s, second_start, Api, Call};
use crate::Suite;

/// A POST whose body is sent chunked (no length) and never finished:
/// `total` bytes in 64 KiB chunks, then silence. Answers the status line
/// the node sent while the body was still open, if one came within `wait`
/// (a router that buffers a body before measuring it sends none).
fn unfinished_chunked(api: &Api, path: &str, total: usize, wait: Duration) -> Result<Option<String>> {
    let mut sock = TcpStream::connect(("127.0.0.1", api.port))?;
    sock.set_read_timeout(Some(wait))?;
    sock.set_write_timeout(Some(wait))?;
    write!(sock, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/x-www-form-urlencoded\r\nTransfer-Encoding: chunked\r\n\r\n", api.port)?;
    let chunk = vec![b'x'; 64 * 1024];
    let mut sent = 0;
    // bounded: `total` bytes at most; a node that answered and closed early stops it sooner
    while sent < total {
        let wrote = write!(sock, "{:x}\r\n", chunk.len()).and_then(|_| sock.write_all(&chunk)).and_then(|_| sock.write_all(b"\r\n"));
        if wrote.is_err() {
            break;
        }
        sent += chunk.len();
    }
    let mut head = [0u8; 512];
    match sock.read(&mut head) {
        Ok(n) if n > 0 => Ok(String::from_utf8_lossy(&head[..n]).lines().next().map(str::to_string)),
        _ => Ok(None),
    }
}

pub fn auth(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("auth") {
        return Ok(());
    }
    let keys = api.person()?;
    let name = s.name("auth");
    let body = json!({ "name": name });
    let bytes = body.to_string().into_bytes();
    let r = api.unsigned("POST", "/api/fragments", Some(&body))?;
    s.ok("an unsigned create is 401", r.status == 401 && r.error() == "unauthenticated", &r);
    // behind a proxy that ends TLS (Fly's), the client signed the https URL
    let proxied = api.person()?;
    let https = format!("{}/api/fragments", api.base.replacen("http://", "https://", 1));
    let pbody = json!({ "name": s.name("proxied") });
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
    // the window's edges, on either side of now: the one ahead is taken and
    // the one past it behind refused however late the node reads its clock
    let me = format!("{}/api/identities/me", api.base);
    let stamped = |t: i64| api.call(Call { method: "GET", url: me.clone(), extra: vec![("authorization", keys.header("GET", &me, b"", t))], ..Call::default() });
    let now = second_start();
    let (ahead, behind) = (stamped(now + limits::AUTH_WINDOW_S)?, stamped(now - limits::AUTH_WINDOW_S - 1)?);
    s.ok(
        "a signature stamped at the edge of the window is taken, and one a second past it is 401",
        ahead.status == 200 && behind.code() == Some(ErrorCode::Unauthenticated),
        format!("{ahead} {behind}"),
    );
    let r = send(signed_as(format!("{}/api/fragments", api.base), br#"{"name":"x"}"#, now_s()))?;
    s.ok("a signature over another body is 401", r.status == 401, &r);
    // a signature over a body, replayed without it (to a route that reads
    // no body before it authenticates): the payload tag binds an empty body too
    let refresh = format!("{}/api/f/{}/refresh", api.base, s.named(api, &keys, "auth")?);
    let r = api.call(Call { method: "POST", url: refresh.clone(), extra: vec![("authorization", signed_as(refresh, b"{}", now_s()))], ..Call::default() })?;
    s.ok("a signature over a body, sent without one, is 401", r.status == 401 && r.message().contains("payload"), &r);
    // a signature too short to be one is refused, and the router lives on (k256 panicked on it)
    let b64 = base64::engine::general_purpose::STANDARD;
    let good = signed_as(format!("{}/api/fragments", api.base), &bytes, now_s());
    let mut event: serde_json::Value = serde_json::from_slice(&b64.decode(&good["Nostr ".len()..])?)?;
    event["sig"] = json!("abcd");
    let r = send(format!("Nostr {}", b64.encode(event.to_string())))?;
    s.ok("a two-byte signature is 401", r.status == 401 && r.message().contains("signature"), &r);
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
    // a person takes a username before they make anything (decision 16)
    let nameless = api.person_without_username()?;
    let r = api.create(&nameless, &s.name("nameless"))?;
    s.ok("a person without a username cannot create", r.status == 400 && r.message().contains("username"), &r);
    let r = api.signed(&nameless, "PUT", "/api/identities/me/username", Some(&json!({ "username": "www" })))?;
    s.ok("a reserved word is not a username", r.status == 400, &r);
    let r = api.signed(&nameless, "PUT", "/api/identities/me/username", Some(&json!({ "username": "to--do" })))?;
    s.ok("a username has no double dash", r.status == 400, &r);
    let owner = api.person()?;
    let other = api.person()?;
    let (owner_u, other_u) = (api.username(&owner)?, api.username(&other)?);
    let r = api.signed(&nameless, "PUT", "/api/identities/me/username", Some(&json!({ "username": owner_u })))?;
    s.ok("a taken username is 409", r.status == 409, &r);
    let r = api.signed(&owner, "PUT", "/api/identities/me/username", Some(&json!({ "username": format!("{owner_u}x") })))?;
    s.ok("a username is chosen once", r.status == 409 && r.message().contains("once"), &r);
    let r = api.signed(&owner, "PUT", "/api/identities/me/username", Some(&json!({ "username": owner_u })))?;
    s.ok("taking your own username again is a no-op", r.status == 200 && r.body["claimed"] == false, &r);

    // an operator undoes a username taken by mistake, while its person owns nothing under it
    let mistaken = api.person()?;
    let mistaken_u = api.username(&mistaken)?;
    let r = api.signed(&owner, "DELETE", &format!("/api/users/{mistaken_u}"), None)?;
    s.ok("only the fleet's operators release a username", r.status == 403, &r);
    let op_session = api.sign_in("operator@e2e.test")?;
    api.approve(&op_session, &s.operator)?;
    let owned = s.named(api, &mistaken, "mine")?;
    s.create(api, &mistaken, &owned)?;
    let r = api.signed(&s.operator, "DELETE", &format!("/api/users/{mistaken_u}"), None)?;
    s.ok("not while its person owns a fragment under it (its URLs name it)", r.status == 409 && r.message().contains(&owned), &r);
    api.signed(&mistaken, "DELETE", &format!("/api/f/{owned}"), None)?;
    let r = api.signed(&s.operator, "DELETE", &format!("/api/users/{mistaken_u}"), None)?;
    let chosen = format!("{mistaken_u}b");
    let again = api.signed(&mistaken, "PUT", "/api/identities/me/username", Some(&json!({ "username": chosen })))?;
    s.ok("released, its person chooses again", r.status == 200 && r.body["released"] == true && again.status == 200 && again.body["username"] == chosen.as_str(), format!("{r} {again}"));
    let r = api.unsigned("GET", &format!("/api/users/{owner_u}"), None)?;
    s.ok("anyone sees who a username is", r.status == 200 && r.body["id"] == api.identity(&owner)?.as_str() && r.body["picture"].is_null(), &r);
    let png: &[u8] = b"\x89PNG\r\n\x1a\n-a-tiny-picture";
    let r = api.call(Call { method: "PUT", url: format!("{}/api/identities/me/picture", api.base), body: Some(png.to_vec()), keys: Some(&owner), ..Call::default() })?;
    s.ok("a person sets a picture", r.status == 200 && r.body["mime"] == "image/png", &r);
    let r = api.unsigned("GET", &format!("/api/users/{owner_u}/picture"), None)?;
    s.ok("and anyone sees it", r.status == 200 && r.bytes == png && r.header("content-type") == "image/png", &r);
    let r = api.call(Call { method: "PUT", url: format!("{}/api/identities/me/picture", api.base), body: Some(b"<svg/>".to_vec()), keys: Some(&owner), ..Call::default() })?;
    s.ok("a picture that is not an image is refused", r.status == 400, &r);

    let label = s.name("create");
    let name = fragment_proto::fragment_name(&label, &owner_u);
    let r = api.create(&owner, &label)?;
    let c = &r.body;
    s.ok("a signed create succeeds", r.status == 200, &r);
    s.ok("a bare label goes under the creator's username", c["name"] == name.as_str(), &r);
    s.ok("create names the owner by identity", c["owner"] == api.identity(&owner)?.as_str(), &r);
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
    s.ok(
        "the repo exists in code.storage as <label>--<username>",
        s.fake.repo_url(&format!("{label}--{owner_u}")).as_deref() == Some(repo.as_str()),
        &repo,
    );
    s.ok(
        "create returns the fragment's own origin",
        c["canonical"] == api.site_url(&name, ""),
        &r,
    );
    s.hook(api, c);

    let r = api.create(&owner, &label)?;
    s.ok("creating an existing name is 409", r.status == 409 && r.error() == "already_exists", &r);
    let r = api.create(&other, &name)?;
    s.ok("no one makes a fragment under someone else's username", r.status == 403, &r);
    let r = api.create(&other, &label)?;
    s.ok(
        "two people each make the same label: two fragments",
        r.status == 200 && r.body["name"] == fragment_proto::fragment_name(&label, &other_u).as_str(),
        &r,
    );
    let r = api.create(&owner, "Bad_Name")?;
    s.ok("an invalid name is 400", r.status == 400 && r.error() == "invalid_request", &r);
    // the fragment's own key is made by the node's KEYS; no client sends one
    let r = api.create_with(&owner, json!({ "name": s.name("oldsecret"), "fragmentSecret": Keys::generate().secret_hex() }))?;
    s.ok("a create that sends a fragmentSecret is refused naming it", r.status == 400 && r.message().contains("fragmentSecret"), &r);
    let public = fragment_proto::fragment_name(&s.name("create-pub"), &owner_u);
    let r = api.create_with(&owner, json!({ "name": public, "visibility": "public" }))?;
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
    let r = api.create(&owner, &label)?;
    s.ok("a deleted name can be created again", r.status == 200 && r.body["owner"] == api.identity(&owner)?.as_str(), &r);
    s.ok("created again, it keeps its repo (found past the list's first page)", r.body["repo"] == repo.as_str(), &r);
    Ok(())
}

pub fn lockdown(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("lockdown") {
        return Ok(());
    }
    let owner = api.person()?;
    let stranger = api.person()?;
    let name = s.named(api, &owner, "lock")?;
    s.create(api, &owner, &name)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
    // every header the router sets for a fragment, forged to name the owner
    let owner_id = api.identity(&owner)?;
    let username = name.split_once('.').map_or("", |(_, u)| u);
    let claims_owner = json!({ "id": owner_id, "kind": "person", "owner": null, "username": username, "key": owner.pubkey_hex() }).to_string();
    let owner_key_unsigned = json!({ "key": owner.pubkey_hex() }).to_string();
    let forged_headers = || {
        routed::ALL
            .iter()
            .map(|h| {
                let v = match *h {
                    routed::NAME => name.clone(),
                    routed::URL => format!("https://{name}.{}/", crate::SUFFIX),
                    routed::MODE => "host".to_string(),
                    routed::CREDENTIAL => owner_key_unsigned.clone(),
                    _ => claims_owner.clone(),
                };
                (*h, v)
            })
            .collect::<Vec<_>>()
    };
    let forged = |keys: Option<&Keys>, url: String| api.call(Call { method: "GET", url, keys, extra: forged_headers(), ..Call::default() });
    let status_url = format!("{}/api/f/{name}/status", api.base);
    let r = forged(Some(&stranger), status_url.clone())?;
    s.ok("a client's routing headers are dropped: the router's own say who signed (403)", r.status == 403, &r);
    let r = forged(None, status_url)?;
    s.ok("and an unsigned request stays unsigned (401)", r.status == 401, &r);
    let r = forged(None, api.site_url(&name, ""))?;
    s.ok("and a browser on the fragment's own origin is nobody (members only: 401)", r.status == 401, &r);
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/create"), Some(&json!({ "name": name })))?;
    s.ok("the supervisor's create is not a public route", r.status == 404, &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}"), None)?;
    s.ok("GET on a fragment's root is 404", r.status == 404, &r);
    let r = api.call(Call { method: "GET", url: format!("http://evil.example.com:{}/index.html", api.port), ..Call::default() })?;
    s.ok("a host outside the suffix is the platform, not a fragment", r.status == 404 && r.message().contains("no route"), &r);
    let label = name.split('.').next().unwrap_or("");
    // a fragment's host is one label, <label>--<username>; any other name
    // under the suffix is no one's (never the platform's)
    for host in ["Bad_Name", "a.b", label, name.as_str(), "a--b"] {
        let r = api.call(Call { method: "GET", url: format!("http://{host}.{}:{}/index.html", crate::SUFFIX, api.port), ..Call::default() })?;
        s.ok(&format!("host {host}.<suffix> is not a fragment, nor the platform"), r.status == 404 && r.message().contains("no fragment here"), &r);
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
    // A body of exactly the limit is read (and refused for what it says: a
    // create takes no padding). One over it is refused from its declared
    // length before it is read: the client may see the 413, or the
    // connection closing while it is still sending. That one is a create
    // that would succeed but for its size (whitespace is JSON).
    let big = s.name("big");
    let padded = |n: usize| json!({ "name": big, "padding": "x".repeat(n) });
    let edge = limits::BODY_MAX_BYTES - padded(0).to_string().len();
    let r = api.create_with(&owner, padded(edge))?;
    s.ok("a body of exactly the limit is read", r.code() == Some(ErrorCode::InvalidRequest), &r);
    let body = format!("{{\"name\":\"{big}\"}}{}", " ".repeat(limits::BODY_MAX_BYTES));
    let sent = api.call(Call { method: "POST", url: format!("{}/api/fragments", api.base), body: Some(body.into_bytes()), content_type: Some("application/json"), keys: Some(&owner), ..Call::default() });
    let refused = match sent {
        Ok(r) => r.status == 413 && r.code() == Some(ErrorCode::TooLarge),
        Err(e) => format!("{e:#}").contains("reset") || format!("{e:#}").contains("Broken pipe"),
    };
    s.ok("a create over the limit is refused unread (413)", refused, "");
    // a connection that closed because the router fell over would pass as a
    // reset too: the node answers after it, and made nothing from the body
    let (alive, made) = (api.unsigned("GET", "/healthz", None)?, api.status(&owner, &api.qualified(&owner, &big)?)?);
    s.ok("after it the node answers, and no fragment was made", alive.status == 200 && made.status == 404 && made.code() == Some(ErrorCode::NotFound), format!("{alive} / {made}"));
    // a body without a length is measured as it arrives: refused at the
    // chunk that crosses the limit, not after it has all been buffered
    let wait = Duration::from_secs(15);
    let line = unfinished_chunked(api, "/api/fragments", limits::BODY_MAX_BYTES + 128 * 1024, wait)?;
    s.ok("a chunked body over 2 MiB is refused as it arrives (413 before it ends)", line.as_deref().is_some_and(|l| l.contains(" 413 ")), format!("{line:?}"));
    let line = unfinished_chunked(api, "/cli/approve", 64 * 1024, wait)?;
    s.ok("and so is a chunked approval form past its limit, before anyone is signed in", line.as_deref().is_some_and(|l| l.contains(" 413 ")), format!("{line:?}"));
    let r = api.unsigned("GET", "/healthz", None)?;
    s.ok("the node stays healthy", r.status == 200, &r);
    let r = api.call(Call {
        method: "GET",
        url: api.site_url(&name, ""),
        extra: vec![("authorization", "Nostr garbage".into())],
        ..Call::default()
    })?;
    s.ok("a bad signature on a site request is 401, not anonymous", r.status == 401, &r);
    Ok(())
}
