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

use crate::api::{now_s, second_start, Api, Call, Reply};
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
    // the status line and what of the body follows it within the wait (an
    // answer may arrive in more than one packet)
    let mut answer = Vec::new();
    let mut buf = [0u8; 1024];
    while answer.len() < 2048 {
        match sock.read(&mut buf) {
            Ok(n) if n > 0 => answer.extend_from_slice(&buf[..n]),
            _ => break,
        }
        let text = String::from_utf8_lossy(&answer);
        // headers and a body as long as they declare: the whole answer
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let declared = head.lines().find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").and_then(|v| v.trim().parse::<usize>().ok()));
            if declared.is_none_or(|n| body.len() >= n) {
                break;
            }
        }
    }
    Ok((!answer.is_empty()).then(|| String::from_utf8_lossy(&answer).into_owned()))
}

/// Whether an answer to an unfinished chunked body refused it as it
/// arrived: the router's 413, or, under `wrangler dev`, the local proxy's
/// 500 when the Worker answered before the upload ended (miniflare's
/// "Network connection lost": the Worker stopped reading; Cloudflare's
/// edge has no such proxy, and the hosted lane wants the 413 itself).
fn refused_as_it_arrived(answer: Option<&str>) -> bool {
    answer.is_some_and(|a| {
        let line = a.lines().next().unwrap_or("");
        line.contains(" 413 ") || (line.contains(" 500 ") && a.contains("Network connection lost") && a.contains("miniflare"))
    })
}

pub fn auth(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("auth", &[]) {
        return Ok(());
    }
    let keys = api.person()?;
    let name = s.name("auth");
    let body = json!({ "label": name });
    let bytes = body.to_string().into_bytes();
    let r = api.unsigned("POST", "/api/fragments", Some(&body))?;
    s.ok("an unsigned create is 401", r.status == 401 && r.error() == "unauthenticated", &r);
    // behind a proxy that ends TLS (Fly's), the client signed the https URL
    let proxied = api.person()?;
    let https = format!("{}/api/fragments", api.base.replacen("http://", "https://", 1));
    let pbody = json!({ "label": s.name("proxied") });
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
    // (the node's clock is this machine's; a preview's is not)
    let me = format!("{}/api/identities/me", api.base);
    let stamped = |t: i64| api.call(Call { method: "GET", url: me.clone(), extra: vec![("authorization", keys.header("GET", &me, b"", t))], ..Call::default() });
    if s.hosted() {
        s.skip("a signature stamped at the edge of the window is taken, and one a second past it is 401", "the edge is a second, and a preview's clock is not this machine's");
    } else {
        let now = second_start();
        let (ahead, behind) = (stamped(now + limits::AUTH_WINDOW_S)?, stamped(now - limits::AUTH_WINDOW_S - 1)?);
        s.ok(
            "a signature stamped at the edge of the window is taken, and one a second past it is 401",
            ahead.status == 200 && behind.code() == Some(ErrorCode::Unauthenticated),
            format!("{ahead} {behind}"),
        );
    }
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
    if !s.section("create", &[crate::Need::Fakes, crate::Need::Deployment]) {
        return Ok(());
    }
    let owner = api.person()?;
    let other = api.person()?;
    // a person's picture, served by their identity
    let owner_id = api.identity(&owner)?;
    let picture_url = format!("/api/identities/{owner_id}/picture");
    let r = api.unsigned("GET", &picture_url, None)?;
    s.ok("a person with no picture has none (404)", r.status == 404, &r);
    let png: &[u8] = b"\x89PNG\r\n\x1a\n-a-tiny-picture";
    let r = api.call(Call { method: "PUT", url: format!("{}/api/identities/me/picture", api.base), body: Some(png.to_vec()), keys: Some(&owner), ..Call::default() })?;
    s.ok("a person sets a picture", r.status == 200 && r.body["mime"] == "image/png", &r);
    let r = api.unsigned("GET", &picture_url, None)?;
    s.ok("and anyone sees it, by their identity", r.status == 200 && r.bytes == png && r.header("content-type") == "image/png", &r);
    let me = api.signed(&owner, "GET", "/api/identities/me", None)?;
    s.ok("their identity names it, with their email", me.body["picture"].as_str().is_some_and(|p| p.starts_with(&picture_url)) && me.body["email"] == Api::email_of(&owner).as_str(), &me);
    let r = api.call(Call { method: "PUT", url: format!("{}/api/identities/me/picture", api.base), body: Some(b"<svg/>".to_vec()), keys: Some(&owner), ..Call::default() })?;
    s.ok("a picture that is not an image is refused", r.status == 400, &r);

    // a name is a label and a random suffix (decision 47)
    let label = s.name("create");
    let r = api.create_with(&owner, json!({ "label": label }))?;
    let c = &r.body;
    let name = c["name"].as_str().unwrap_or("").to_string();
    s.ok("a signed create succeeds", r.status == 200, &r);
    s.ok("a label is made a name: the label, and a random suffix", fragment_proto::split_fragment_name(&name).is_some_and(|(l, _)| l == label), &r);
    s.ok("create names the owner by identity", c["owner"] == owner_id.as_str(), &r);
    s.ok("create returns the fragment's own npub", c["npub"].as_str().is_some_and(|n| n.starts_with("npub1")), &r);
    s.ok("create defaults to link visibility", c["visibility"] == "link", &r);
    s.ok(
        "create returns the share and inbox tokens",
        c["viewToken"].as_str().is_some_and(|t| t.len() == 24) && c["inboxToken"].as_str().is_some_and(|t| t.len() == 32),
        &r,
    );
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    s.ok("create returns the url-form repo identity", repo.len() == 36 && repo.matches('-').count() == 4, &r);
    // the one derivation of a repo's name (crates/core codestorage.rs):
    // `<name>--<12 hex of its owner>`; a local node has no prefix
    let named = fragment_core::codestorage::repo_name("", &name, &owner_id).unwrap_or_default();
    s.ok(
        "the repo exists in code.storage as <name>--<its owner's 12 hex>",
        named.starts_with(&format!("{name}--")) && s.fake.repo_url(&named).as_deref() == Some(repo.as_str()),
        format!("{repo} ({named})"),
    );
    s.ok("create returns the fragment's own origin", c["canonical"] == api.site_url(&name, ""), &r);

    let r = api.create_with(&owner, json!({ "label": label }))?;
    s.ok("the same label again is another fragment, under another suffix", r.status == 200 && r.body["name"] != name.as_str(), &r);
    let r = api.create(&owner, &name)?;
    s.ok("creating an existing name is 409", r.status == 409 && r.error() == "already_exists", &r);
    let r = api.create(&other, &name)?;
    s.ok("a name someone made is theirs: anyone else's create of it is 409", r.status == 409, &r);
    let exact = s.named(api, &other, "create")?;
    let r = api.create(&other, &exact)?;
    s.ok("a name made in full is made as it is", r.status == 200 && r.body["name"] == exact.as_str(), &r);
    let refused: Vec<Reply> = [
        api.create_with(&owner, json!({ "name": "Bad_Name--k3x9" }))?,
        api.create_with(&owner, json!({ "name": label }))?,
        api.create_with(&owner, json!({ "label": "Bad_Name" }))?,
        api.create_with(&owner, json!({ "label": label, "name": exact }))?,
        api.create_with(&owner, json!({}))?,
    ]
    .into();
    s.ok(
        "a create names a name in full or a label, one of the two: an invalid one, a label as a name, both, or neither is 400",
        refused.iter().all(|r| r.status == 400 && r.error() == "invalid_request"),
        refused.iter().map(ToString::to_string).collect::<Vec<_>>().join(" / "),
    );
    let r = api.status(&owner, &label)?;
    s.ok("a path names a fragment in full: a bare label there is 404, saying so", r.status == 404 && r.message().contains("in full"), &r);

    // a fragment's host label, its name and a branch's mark, is one DNS
    // label (docs/api.md, Names): a label as long as this deployment
    // leaves room for makes one of exactly 63 bytes, and a byte more is
    // refused, never cut
    let mark = api.host_mark();
    let room = fragment_proto::label_room(&mark);
    s.ok("this deployment leaves a label at least the room every deployment does", room >= limits::LABEL_ROOM_MIN_BYTES, room);
    let base = s.name("edge");
    let edge = |len: usize| format!("{base}-{}", "x".repeat(len - base.len() - 1));
    let (fits, over) = (edge(room), edge(room + 1));
    let r = api.create_with(&owner, json!({ "label": fits }))?;
    let made = r.body["name"].as_str().unwrap_or_default().to_string();
    let host = r.body["canonical"].as_str().and_then(|c| reqwest::Url::parse(c).ok()).and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
    let host_label = host.split('.').next().unwrap_or_default();
    s.ok(
        "a label whose host label is exactly 63 bytes is made",
        r.status == 200 && host_label.len() == limits::HOST_LABEL_MAX_BYTES && host_label == format!("{made}{mark}"),
        format!("{host_label} ({} bytes): {r}", host_label.len()),
    );
    let view = r.body["viewToken"].as_str().unwrap_or_default().to_string();
    let r = api.page(&made, &format!("?view={view}"), None)?;
    s.ok("and its host reaches it (the fragment answers: nothing deployed yet)", r.status == 404 && r.message().contains("deploy first"), &r);
    let refused = |r: &Reply| r.status == 400 && r.error() == "invalid_request" && r.message().contains(&format!("at most {room} bytes"));
    let r = api.create_with(&owner, json!({ "label": over }))?;
    s.ok("one byte longer is refused (400), saying why: never cut to fit", refused(&r), &r);
    if fragment_proto::valid_label(&over) {
        // on a branch: a label that fits a name, not the name's host
        let unmade = api.qualified(&owner, &over)?;
        let again = api.create(&owner, &unmade)?;
        let status = api.status(&owner, &unmade)?;
        s.ok(
            "asked in full it is refused the same, naming the address and its 63 bytes, and nothing is made",
            refused(&again) && again.message().contains(&format!("{unmade}{mark}")) && again.message().contains("at most 63 bytes") && status.status == 404,
            format!("{again} {status}"),
        );
    }
    // the fragment's own key is made by the node's KEYS; no client sends one
    let r = api.create_with(&owner, json!({ "label": s.name("oldsecret"), "fragmentSecret": Keys::generate().secret_hex() }))?;
    s.ok("a create that sends a fragmentSecret is refused naming it", r.status == 400 && r.message().contains("fragmentSecret"), &r);
    let public = s.named(api, &owner, "create-pub")?;
    let r = api.create_with(&owner, json!({ "name": public, "visibility": "public" }))?;
    s.ok("create takes a visibility", r.status == 200 && r.body["visibility"] == "public", &r);
    // a name deleted, made again by someone else: a repo of their own,
    // never its earlier maker's (a repo's name carries its owner)
    let reused = s.named(api, &owner, "reused")?;
    let first = s.create(api, &owner, &reused)?;
    let deleted = api.signed(&owner, "DELETE", &format!("/api/f/{reused}"), None)?;
    let remade = api.create(&other, &reused)?;
    let theirs = fragment_core::codestorage::repo_name("", &reused, &api.identity(&other)?).unwrap_or_default();
    s.ok(
        "a name deleted and made again by someone else: a fresh repo, named for its new owner",
        deleted.status == 200 && remade.status == 200 && remade.body["repo"] != first["repo"] && s.fake.repo_url(&theirs).as_deref() == remade.body["repo"].as_str(),
        format!("{deleted} {remade} (the earlier maker's repo {})", first["repo"]),
    );

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
    let r = api.create(&owner, &name)?;
    s.ok("a deleted name can be created again", r.status == 200 && r.body["owner"] == api.identity(&owner)?.as_str(), &r);
    s.ok("created again, it keeps its repo (found past the list's first page)", r.body["repo"] == repo.as_str(), &r);
    Ok(())
}

pub fn lockdown(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("lockdown", &[crate::Need::Node]) {
        return Ok(());
    }
    let owner = api.person()?;
    let stranger = api.person()?;
    let name = s.named(api, &owner, "lock")?;
    s.create(api, &owner, &name)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
    // every header the router sets for a fragment, forged to name the owner
    let owner_id = api.identity(&owner)?;
    let claims_owner = json!({ "id": owner_id, "kind": "person", "owner": null, "key": owner.pubkey_hex() }).to_string();
    let owner_key_unsigned = json!({ "key": owner.pubkey_hex() }).to_string();
    let forged_headers = || {
        routed::ALL
            .iter()
            .map(|h| {
                let v = match *h {
                    routed::NAME => name.clone(),
                    routed::URL => format!("https://{name}.{}/", crate::SUFFIX),
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
    let label = fragment_proto::split_fragment_name(&name).map_or("", |(l, _)| l);
    // a fragment's host is its name, one label; any other label under the
    // suffix is no one's (never the platform's)
    for host in ["Bad_Name", "a.b", label, "a--b", "lock--k3l9", "lock---k3x9", "lock-k3x9"] {
        let r = api.call(Call { method: "GET", url: format!("http://{host}.{}:{}/index.html", crate::SUFFIX, api.port), ..Call::default() })?;
        s.ok(&format!("host {host}.<suffix> is not a fragment, nor the platform"), r.status == 404 && r.message().contains("no fragment here"), &r);
    }
    // A body of exactly the limit is read (and refused for what it says: a
    // create takes no padding). One over it is refused from its declared
    // length before it is read: the client may see the 413, or the
    // connection closing while it is still sending. That one is a create
    // that would succeed but for its size (whitespace is JSON).
    let big = s.name("big");
    let padded = |n: usize| json!({ "label": big, "padding": "x".repeat(n) });
    let edge = limits::BODY_MAX_BYTES - padded(0).to_string().len();
    let r = api.create_with(&owner, padded(edge))?;
    s.ok("a body of exactly the limit is read", r.code() == Some(ErrorCode::InvalidRequest), &r);
    let body = format!("{{\"label\":\"{big}\"}}{}", " ".repeat(limits::BODY_MAX_BYTES));
    let sent = api.call(Call { method: "POST", url: format!("{}/api/fragments", api.base), body: Some(body.into_bytes()), content_type: Some("application/json"), keys: Some(&owner), ..Call::default() });
    let refused = match sent {
        Ok(r) => r.status == 413 && r.code() == Some(ErrorCode::TooLarge),
        Err(e) => format!("{e:#}").contains("reset") || format!("{e:#}").contains("Broken pipe"),
    };
    s.ok("a create over the limit is refused unread (413)", refused, "");
    // a connection that closed because the router fell over would pass as a
    // reset too: the node answers after it, and made nothing from the body
    let (alive, listed) = (api.unsigned("GET", "/healthz", None)?, api.signed(&owner, "GET", "/api/fragments", None)?);
    let made = listed.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"].as_str().and_then(fragment_proto::split_fragment_name).is_some_and(|(l, _)| l == big)));
    s.ok("after it the node answers, and no fragment was made", alive.status == 200 && listed.status == 200 && !made, format!("{alive} / {listed}"));
    // a body without a length is measured as it arrives: refused at the
    // chunk that crosses the limit, not after it has all been buffered
    let wait = Duration::from_secs(15);
    let line = unfinished_chunked(api, "/api/fragments", limits::BODY_MAX_BYTES + 128 * 1024, wait)?;
    s.ok("a chunked body over 2 MiB is refused as it arrives (413 before it ends)", refused_as_it_arrived(line.as_deref()), format!("{line:?}"));
    let line = unfinished_chunked(api, "/cli/approve", 64 * 1024, wait)?;
    s.ok("and so is a chunked approval form past its limit, before anyone is signed in", refused_as_it_arrived(line.as_deref()), format!("{line:?}"));
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
