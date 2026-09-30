//! A fragment's own Hermes on the fleet's sandcastle node
//! (docs/hermes-chat.md), on the sandcastle fake: a deploy that declares
//! `"hermes": {}` has the platform make a key for it (in `KEYS`), register
//! it as a computer its owner owns, grant it one computer with the
//! platform's grantor key, and make that computer: Hermes, public at the
//! node's router, its own login the gate. A viewer who owns or edits the
//! fragment gets a native session for it (`POST /__hermes/access`), as
//! Finite's dashboard does, and chats with Hermes directly; anyone else,
//! and anyone signed out, is refused, and no one ever sees its password.
//! Its model calls are billed to the fragment's owner. A deploy that drops
//! the block removes the computer and revokes its key.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use serde_json::{json, Value};

use crate::api::{Api, Call, Reply};
use crate::Suite;

fn soon(s: &Suite, f: impl FnMut() -> bool) -> bool {
    s.eventually(Duration::from_secs(30), f)
}

fn out(o: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// `POST /__hermes/access` on the fragment's own host, as `keys` (none:
/// signed out).
fn access(api: &Api, name: &str, keys: Option<&Keys>) -> Result<Reply> {
    api.call(Call { method: "POST", url: api.site_url(name, "__hermes/access"), body: Some(b"{}".to_vec()), content_type: Some("application/json"), keys, ..Call::default() })
}

/// A call to the Hermes a grant names, as a page makes it (its origin, the
/// session as a bearer).
fn native(base: &str, method: &str, path: &str, token: &str, origin: &str) -> Result<reqwest::blocking::Response> {
    let client = reqwest::blocking::Client::new();
    Ok(client.request(method.parse()?, format!("{base}{path}")).header("authorization", format!("Bearer {token}")).header("origin", origin).send()?)
}

type Ws = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>;

/// The next JSON text message.
fn next(ws: &mut Ws) -> Result<Value> {
    // bounded by the socket: the fake's turn is a handful of messages
    loop {
        if let tungstenite::Message::Text(t) = ws.read()? {
            return Ok(serde_json::from_str(&t)?);
        }
    }
}

/// A JSON-RPC call: its result, and the events before it.
fn rpc(ws: &mut Ws, id: u64, method: &str, params: Value) -> Result<(Value, Vec<Value>)> {
    ws.send(tungstenite::Message::text(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string()))?;
    let mut events = vec![];
    for _ in 0..50 {
        let v = next(ws)?;
        if v["id"] == json!(id) {
            return Ok((v["result"].clone(), events));
        }
        events.push(v);
    }
    anyhow::bail!("no answer to {method} in {events:?}")
}

/// One turn over Hermes' `/api/ws` with a single-use ticket, as the page
/// takes it: the reply's text.
fn turn(base: &str, token: &str, origin: &str, text: &str) -> Result<String> {
    let ticket: Value = native(base, "POST", "api/auth/ws-ticket", token, origin)?.json()?;
    let ticket = ticket["ticket"].as_str().context("a ticket")?;
    let url = format!("{}api/ws", base.replacen("http://", "ws://", 1));
    let mut req = tungstenite::client::IntoClientRequest::into_client_request(url.as_str())?;
    req.headers_mut().insert("sec-websocket-protocol", format!("hermes-gateway-v1, hermes-gateway-ticket.{ticket}").parse()?);
    let (mut ws, resp) = tungstenite::connect(req)?;
    anyhow::ensure!(resp.headers().get("sec-websocket-protocol").is_some_and(|p| p == "hermes-gateway-v1"), "the protocol chosen");
    let ready = next(&mut ws)?;
    anyhow::ensure!(ready["params"]["type"] == "gateway.ready", "{ready}");
    let (made, _) = rpc(&mut ws, 1, "session.create", json!({ "title": "e2e" }))?;
    let live = made["session_id"].as_str().context("a live session")?.to_string();
    let (_, mut events) = rpc(&mut ws, 2, "prompt.submit", json!({ "session_id": live, "text": text }))?;
    for _ in 0..20 {
        if let Some(done) = events.iter().find(|e| e["params"]["type"] == "message.complete") {
            return Ok(done["params"]["payload"]["text"].as_str().unwrap_or("").to_string());
        }
        events.push(next(&mut ws)?);
    }
    anyhow::bail!("no message.complete in {events:?}")
}

pub fn hermes(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("hermes") {
        return Ok(());
    }
    let home = s.dir("hermes-owner");
    s.login(api, &home);
    let owner = s.cli_keys(&home).context("the owner's CLI logged in")?;
    let made = s.cli_json(api, &home, &["create", "chat", "--json"])?;
    let name = made["name"].as_str().unwrap_or("").to_string();
    s.hook(api, &made);
    let site = s.dir("hermes-site");
    std::fs::write(site.join("index.html"), "<h1>chat</h1>")?;
    std::fs::write(site.join("fragment.json"), r#"{"hermes":{}}"#)?;
    let computers_before = s.sandcastle.computers();
    // a Hermes being made answers "starting", however soon it is asked
    s.sandcastle.serving_after(3);
    let o = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().expect("a UTF-8 path")]);
    s.ok("a deploy that declares a Hermes goes live", o.status.success(), out(&o));
    let r = access(api, &name, Some(&owner))?;
    s.ok("while it is being made, a session is not ready yet", r.status == 409 && r.code() == Some(ErrorCode::NotReady), &r);

    let events = || events_of(api, &owner, &name);
    let ready = soon(s, || events().iter().any(|e| e.starts_with("hermes.ready")));
    s.ok("the platform makes it, and the fragment's events say it serves", ready, format!("{:?}", events()));
    let computers = s.sandcastle.computers();
    s.ok("one computer on the node", computers.len() == computers_before.len() + 1, format!("{:?}", computers.keys()));
    let (computer, fake) = computers.into_iter().find(|(n, _)| !computers_before.contains_key(n)).context("its computer")?;
    let key = fake.owner.clone();
    let grants = s.sandcastle.grants();
    s.ok("granted by the platform's key, to a key of its own", grants.get(&key).is_some_and(|g| g["computers_max"] == 1) && key != owner.pubkey_hex(), format!("{grants:?}"));
    let spec = &fake.spec;
    let password = spec["service"]["env"]["HERMES_DASHBOARD_BASIC_AUTH_PASSWORD"].as_str().unwrap_or("").to_string();
    s.ok(
        "Hermes under its own init, public, its login the gate, awake while its gateway works",
        spec["url_auth"] == "public" && spec["service"]["init"]["argv"][0] == "/init" && password.len() == 64 && spec["service"]["busy"]["field"] == "active_agents",
        spec["service"]["init"].to_string(),
    );

    // its key is a computer its owner owns: the model route bills them
    let r = api.signed(&owner, "GET", "/api/identities/me", None)?;
    let listed = r.body["computers"].as_array().is_some_and(|c| c.iter().any(|c| c["name"].as_str().is_some_and(|n| n.contains("hermes"))));
    s.ok("its key is registered as a computer its owner owns", listed, &r);
    let node = Keys::from_secret_hex(&s.sandcastle_node.secret_hex()).expect("a key's own secret");
    let ask = json!({ "computer": computer, "id": "0123456789abcdef", "node": "e2e", "owner": key });
    let r = api.signed(&node, "POST", "/api/sandcastle/credentials", Some(&ask))?;
    s.ok("the node's ask for its credentials is answered: its owner pays", r.status == 200 && r.body["credentials"][0]["name"] == "OPENAI_API_KEY", &r);

    // who may have a session
    let r = access(api, &name, Some(&owner))?;
    let grant = r.body.clone();
    let base = grant["baseUrl"].as_str().unwrap_or("").to_string();
    let token = grant["accessToken"].as_str().unwrap_or("").to_string();
    s.ok("its owner gets a native session for it, and never its password", r.status == 200 && !token.is_empty() && base.ends_with(&format!("/c/{computer}/")) && !r.text.contains(&password), &r);
    let origin = api.site_origin(&name);
    let named = s.sandcastle.computers().get(&computer).map(|c| c.spec["cors_origins"].clone()).unwrap_or_default();
    s.ok("the page's origin is named on its computer", named.as_array().is_some_and(|o| o.iter().any(|x| x == origin.as_str())), &named);
    let r = native(&base, "GET", "api/sessions", &token, &origin)?;
    let allowed = r.headers().get("access-control-allow-origin").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    s.ok("with it, the page reads Hermes directly, across origins", r.status() == 200 && allowed == origin, format!("{} {allowed}", r.status()));
    let reply = turn(&base, &token, &origin, "hello hermes");
    s.ok("and chats with it over its socket, with a single-use ticket", reply.as_deref().is_ok_and(|t| t.contains("hello hermes")), format!("{reply:?}"));
    let again = access(api, &name, Some(&owner))?;
    s.ok("asked again, the same session while it lasts", again.body["accessToken"] == token.as_str(), &again);
    let stranger = api.person()?;
    let r = access(api, &name, Some(&stranger))?;
    s.ok("someone else signed in is refused", r.status == 403, &r);
    let r = access(api, &name, None)?;
    s.ok("and anyone signed out", r.status == 401, &r);
    let editor = api.person()?;
    let editor_id = api.identity(&editor)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{editor_id}"), Some(&json!({ "role": "editor" })))?;
    let r = access(api, &name, Some(&editor))?;
    s.ok("an editor gets one too", r.status == 200 && !r.body["accessToken"].as_str().unwrap_or("").is_empty(), &r);

    // dropping the block removes it
    std::fs::write(site.join("fragment.json"), r#"{}"#)?;
    let o = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().expect("a UTF-8 path")]);
    s.ok("a deploy that drops the block goes live", o.status.success(), out(&o));
    let gone = soon(s, || !s.sandcastle.computers().contains_key(&computer));
    s.ok("its computer is removed from the node", gone, format!("{:?}", events()));
    let r = api.signed(&node, "POST", "/api/sandcastle/credentials", Some(&ask))?;
    s.ok("and its key revoked: the node's ask gets nothing", r.status == 403 && r.body["credentials"].is_null(), &r);
    let r = access(api, &name, Some(&owner))?;
    s.ok("and no session is given", r.status == 404, &r);

    // the node crashes while one is being made: made once when it is back
    let grants_before = s.sandcastle.grants().len();
    s.sandcastle.serving_after(u32::MAX);
    std::fs::write(site.join("fragment.json"), r#"{"hermes":{}}"#)?;
    let o = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().expect("a UTF-8 path")]);
    s.ok("declared again, it goes live", o.status.success(), out(&o));
    let started = soon(s, || s.sandcastle.computers().len() > computers_before.len());
    s.ok("a new computer is being made on the node", started, format!("{:?}", s.sandcastle.computers().keys()));
    s.crash()?;
    let api = s.start(false, true)?;
    s.sandcastle.serving_after(1);
    let ready = soon(s, || events_of(&api, &owner, &name).iter().filter(|e| e.starts_with("hermes.ready")).count() == 2);
    s.ok("back up, the node finishes making it", ready, format!("{:?}", events_of(&api, &owner, &name)));
    let computers = s.sandcastle.computers();
    let again: Vec<&String> = computers.keys().filter(|n| !computers_before.contains_key(*n)).collect();
    s.ok("once: one computer, one new grant", again.len() == 1 && s.sandcastle.grants().len() == grants_before + 1, format!("{again:?} {:?}", s.sandcastle.grants().keys()));
    let r = api.signed(&owner, "GET", "/api/identities/me", None)?;
    let paired = r.body["computers"].as_array().map_or(0, |c| c.iter().filter(|c| c["name"].as_str().is_some_and(|n| n.contains("hermes"))).count());
    s.ok("and one computer of its owner's for it", paired == 1, &r);
    let r = access(&api, &name, Some(&owner))?;
    s.ok("its owner gets a session for the new one", r.status == 200 && r.body["accessToken"] != token.as_str(), &r);
    Ok(())
}

/// The fragment's events, `kind: summary`.
fn events_of(api: &Api, owner: &Keys, name: &str) -> Vec<String> {
    let r = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=50"), None).map(|r| r.body).unwrap_or_default();
    r["events"].as_array().into_iter().flatten().map(|e| format!("{}: {}", e["kind"].as_str().unwrap_or(""), e["summary"].as_str().unwrap_or(""))).collect()
}
