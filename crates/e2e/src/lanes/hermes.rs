//! A fragment's own Hermes on the fleet's sandcastle node
//! (docs/hermes-chat.md, docs/runtime-seam.md), on the sandcastle fake: a
//! deploy that declares `"computer": {"preset": "hermes"}` has the platform
//! make a key for it
//! (in `KEYS`), register it as a computer its owner owns, grant it one
//! computer with the platform's grantor key, and make that computer:
//! Hermes in loopback mode behind its bridge, reached by its key over iroh,
//! with no login and no password. The fragment's owner alone gets an
//! admission for their page's iroh key (`POST /__hermes/access`), signed
//! by the computer's key, and Hermes' session token (docs/one-home.md,
//! decision 5); anyone else, an editor too, and anyone signed out, is
//! refused. Its page shows its screen beside the chat to its owner, live,
//! with Take over and Give back (phase 4). Its model calls are billed to the
//! fragment's owner. The template's page does all of that in headless
//! Chrome, with the platform's computer client (served on every host at
//! `/__computer/`). A deploy that drops the block removes the computer and
//! revokes its key.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use iroh::endpoint::{presets, Connection};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::api::{Api, Call, Reply};
use crate::Suite;

fn soon(s: &Suite, f: impl FnMut() -> bool) -> bool {
    s.eventually(Duration::from_secs(30), f)
}

fn out(o: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
}

/// `POST /__hermes/access` `{peer}` on the fragment's own host, as `keys`
/// (none: signed out).
pub(crate) fn access(api: &Api, name: &str, keys: Option<&Keys>, peer: &str) -> Result<Reply> {
    let body = json!({ "peer": peer }).to_string().into_bytes();
    api.call(Call { method: "POST", url: api.site_url(name, "__hermes/access"), body: Some(body), content_type: Some("application/json"), keys, ..Call::default() })
}

/// A page's own iroh key, as the platform's client makes one: its endpoint
/// binds at the first connect, homed on that computer's relay (a page names
/// its key to get the admission that names the relay).
pub(crate) struct Peer {
    secret: SecretKey,
    ep: std::sync::OnceLock<Endpoint>,
    rt: tokio::runtime::Handle,
}

impl Peer {
    pub(crate) fn new(rt: tokio::runtime::Handle) -> Peer {
        Peer { secret: SecretKey::generate(), ep: std::sync::OnceLock::new(), rt }
    }

    pub(crate) fn id(&self) -> String {
        self.secret.public().to_string()
    }

    /// Connects to the computer an access names and presents its admission:
    /// the connection and the node's answer.
    pub(crate) fn connect(&self, access: &Value) -> Result<(Connection, Value)> {
        let endpoint = access["endpoint"].as_str().context("an endpoint")?.parse()?;
        let relay: RelayUrl = access["relay"].as_str().context("a relay")?.parse()?;
        let admission = access["admission"].as_str().context("an admission")?.to_string();
        let ep = match self.ep.get() {
            Some(ep) => ep,
            None => {
                let bound = self.rt.block_on(Endpoint::builder(presets::Minimal).secret_key(self.secret.clone()).relay_mode(RelayMode::Custom(relay.clone().into())).bind())?;
                self.ep.get_or_init(|| bound)
            }
        };
        self.rt.block_on(async {
            let conn = ep.connect(EndpointAddr::new(endpoint).with_relay_url(relay), b"sandcastle/1").await?;
            let (mut send, mut recv) = conn.open_bi().await?;
            send.write_all(b"A").await?;
            send.write_u16(u16::try_from(admission.len())?).await?;
            send.write_all(admission.as_bytes()).await?;
            send.finish()?;
            let len = recv.read_u16().await?;
            let mut body = vec![0u8; usize::from(len)];
            recv.read_exact(&mut body).await?;
            Ok((conn, serde_json::from_slice(&body)?))
        })
    }

    /// A loopback port whose each TCP connection is one stream on `conn`:
    /// the blocking HTTP and WebSocket clients reach Hermes through it, with
    /// a loopback Host, as the platform's client does.
    pub(crate) fn tunnel(&self, conn: Connection) -> Result<u16> {
        let listener = self.rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
        let port = listener.local_addr()?.port();
        self.rt.spawn(async move {
            // Bounded by its runtime: the fake's ends with the suite, the hosted run's with the run.
            while let Ok((mut tcp, _)) = listener.accept().await {
                let conn = conn.clone();
                tokio::spawn(async move {
                    let Ok((mut send, recv)) = conn.open_bi().await else { return };
                    if send.write_all(b"H").await.is_err() {
                        return;
                    }
                    let mut stream = tokio::io::join(recv, send);
                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut stream).await;
                });
            }
        });
        Ok(port)
    }
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

/// Hermes' socket, `/api/ws?token=`, through a tunnel's port, offering
/// `hermes-gateway-v1` as the page does. Hermes in loopback mode upgrades
/// without naming a subprotocol (it names one only for its ticket), which
/// tungstenite's client refuses; so the upgrade is read here, a byte at a
/// time (the gateway's first frame may follow at once), and the socket
/// taken as it is, as the page's client takes it.
fn gateway(port: u16, token: &str) -> Result<Ws> {
    use std::io::{Read, Write};
    /// The longest upgrade answer read.
    const HEAD_MAX: usize = 16 * 1024;
    let mut tcp = std::net::TcpStream::connect(("127.0.0.1", port))?;
    // a cold wake and a real model's first words fit
    tcp.set_read_timeout(Some(Duration::from_secs(60)))?;
    let key = tungstenite::handshake::client::generate_key();
    write!(
        tcp,
        "GET /api/ws?token={token} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Protocol: hermes-gateway-v1\r\n\r\n"
    )?;
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        anyhow::ensure!(head.len() < HEAD_MAX, "an upgrade answer over {HEAD_MAX} bytes");
        tcp.read_exact(&mut byte)?;
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head);
    anyhow::ensure!(head.starts_with("HTTP/1.1 101"), "the socket answered {}", head.lines().next().unwrap_or(""));
    Ok(tungstenite::WebSocket::from_raw_socket(tungstenite::stream::MaybeTlsStream::Plain(tcp), tungstenite::protocol::Role::Client, None))
}

/// One turn in a new chat over Hermes' socket, through a tunnel's port: the
/// reply's text.
pub(crate) fn turn(port: u16, token: &str, text: &str) -> Result<String> {
    let mut ws = gateway(port, token)?;
    let ready = next(&mut ws)?;
    anyhow::ensure!(ready["params"]["type"] == "gateway.ready", "{ready}");
    let (made, _) = rpc(&mut ws, 1, "session.create", json!({ "title": "e2e" }))?;
    let live = made["session_id"].as_str().context("a live session")?.to_string();
    let (_, mut events) = rpc(&mut ws, 2, "prompt.submit", json!({ "session_id": live, "text": text }))?;
    // bounded: a real model's short reply is a few hundred deltas at most
    for _ in 0..2000 {
        if let Some(done) = events.iter().find(|e| e["params"]["type"] == "message.complete") {
            return Ok(done["params"]["payload"]["text"].as_str().unwrap_or("").to_string());
        }
        events.push(next(&mut ws)?);
    }
    anyhow::bail!("no message.complete in {events:?}")
}

/// A GET through a tunnel's port, with Hermes' session token or none.
pub(crate) fn get(port: u16, path: &str, token: Option<&str>) -> Result<(u16, Value)> {
    let mut req = reqwest::blocking::Client::new().get(format!("http://127.0.0.1:{port}{path}"));
    if let Some(t) = token {
        req = req.header("x-hermes-session-token", t);
    }
    let r = req.send()?;
    let status = r.status().as_u16();
    Ok((status, r.json().unwrap_or(Value::Null)))
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
    let view = made["viewToken"].as_str().unwrap_or("").to_string();
    s.hook(api, &made);
    let site = s.dir("hermes-site");
    std::fs::write(site.join("index.html"), "<h1>chat</h1>")?;
    std::fs::write(site.join("fragment.json"), HERMES)?;
    let computers_before = s.sandcastle.computers();
    let page = Peer::new(s.sandcastle.runtime());
    // a Hermes being made answers "starting", however soon it is asked
    s.sandcastle.serving_after(3);
    let o = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().expect("a UTF-8 path")]);
    s.ok("a deploy that declares a Hermes goes live", o.status.success(), out(&o));
    let r = access(api, &name, Some(&owner), &page.id())?;
    s.ok("while it is being made, an admission is not ready yet", r.status == 409 && r.code() == Some(ErrorCode::NotReady), &r);

    let events = || events_of(api, &owner, &name);
    let ready = soon(s, || events().iter().any(|e| e.starts_with("hermes.ready")));
    s.ok("the platform makes it, and the fragment's events say it serves", ready, format!("{:?}", events()));
    let kind = || {
        let listed = api.signed(&owner, "GET", "/api/fragments", None).map(|r| r.body).unwrap_or_default();
        listed["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == name.as_str()).map(|f| f["kind"].clone()).unwrap_or_default()
    };
    s.ok("its owner's list says it is a Hermes computer", soon(s, || kind() == json!({ "computer": "hermes" })), kind());
    let computers = s.sandcastle.computers();
    s.ok("one computer on the node", computers.len() == computers_before.len() + 1, format!("{:?}", computers.keys()));
    let (computer, fake) = computers.into_iter().find(|(n, _)| !computers_before.contains_key(n)).context("its computer")?;
    let key = fake.owner.clone();
    let grants = s.sandcastle.grants();
    s.ok("granted by the platform's key, to a key of its own", grants.get(&key).is_some_and(|g| g["computers_max"] == 1) && key != owner.pubkey_hex(), format!("{grants:?}"));
    let spec = &fake.spec;
    let env = &spec["service"]["env"];
    let token = env["HERMES_DASHBOARD_SESSION_TOKEN"].as_str().unwrap_or("").to_string();
    s.ok(
        "Hermes in loopback mode behind its bridge: no login, no password, its URL its owner's",
        env["HERMES_DASHBOARD_HOST"] == "127.0.0.1"
            && token.len() == 64
            && env.as_object().is_some_and(|e| e.keys().all(|k| !k.contains("BASIC_AUTH")))
            && spec["service"]["init"]["argv"][1] == "/bin/sh"
            && spec["url_auth"] == "owner"
            && spec["service"]["busy"]["field"] == "active_agents",
        spec["service"]["init"]["argv"][0].to_string(),
    );

    // its key is a computer its owner owns: the model route bills them
    let r = api.signed(&owner, "GET", "/api/identities/me", None)?;
    let listed = r.body["computers"].as_array().is_some_and(|c| c.iter().any(|c| c["name"].as_str().is_some_and(|n| n.contains("hermes"))));
    s.ok("its key is registered as a computer its owner owns", listed, &r);
    let node = Keys::from_secret_hex(&s.sandcastle_node.secret_hex()).expect("a key's own secret");
    let ask = json!({ "computer": computer, "id": "0123456789abcdef", "node": "e2e", "owner": key });
    let r = api.signed(&node, "POST", "/api/sandcastle/credentials", Some(&ask))?;
    s.ok("the node's ask for its credentials is answered: its owner pays", r.status == 200 && r.body["credentials"][0]["name"] == "OPENAI_API_KEY", &r);

    // who may be admitted, and what the admission is
    let r = access(api, &name, Some(&owner), &page.id())?;
    let granted = r.body.clone();
    let now = crate::api::now_s();
    let verified = granted["admission"].as_str().map(|a| fragment_nip98::verify_admission(a, now, 60));
    let admitted_right = verified.as_ref().is_some_and(|v| {
        v.as_ref().is_ok_and(|a| a.signer == key && a.peer == page.id() && a.computer == computer && a.node == s.sandcastle.node && a.expires_at > now && a.expires_at <= now + 300)
    });
    s.ok(
        "its owner's page gets an admission for its own key, signed by the computer's key, for this computer and node, five minutes long",
        r.status == 200 && admitted_right && granted["endpoint"] == fake.endpoint.as_str() && granted["relay"] == s.sandcastle.relay.as_str() && granted["node"] == s.sandcastle.node.as_str(),
        format!("{} {verified:?}", r.status),
    );
    s.ok("and Hermes' session token, and a loopback Host", granted["token"] == token.as_str() && granted["host"] == "127.0.0.1", &r);
    let r = access(api, &name, Some(&owner), "not a key")?;
    s.ok("a peer that is not an iroh key is refused", r.status == 400, &r);
    let stranger = api.person()?;
    let r = access(api, &name, Some(&stranger), &page.id())?;
    s.ok("someone else signed in is refused", r.status == 403, &r);
    let r = access(api, &name, None, &page.id())?;
    s.ok("and anyone signed out", r.status == 401, &r);
    let editor = api.person()?;
    let editor_id = api.identity(&editor)?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{editor_id}"), Some(&json!({ "role": "editor" })))?;
    let r = access(api, &name, Some(&editor), &page.id())?;
    s.ok("an editor is refused: a Hermes, its screen and logins with it, is its owner's alone", r.status == 403 && r.body["admission"].is_null(), &r);

    // the page talks to Hermes by its key, through the relay
    let (conn, answer) = page.connect(&granted)?;
    s.ok("the page's key connects through the relay and is admitted", answer["admitted"] == true, &answer);
    let port = page.tunnel(conn)?;
    let (status, _) = get(port, "/api/sessions", None)?;
    s.ok("without its session token, Hermes refuses a read", status == 401, status);
    let (status, body) = get(port, "/api/sessions", Some(&token))?;
    s.ok("with it, Hermes answers: no login, the admission its gate", status == 200 && body["sessions"].is_array(), &body);
    let reply = turn(port, &token, "hello hermes");
    s.ok("and chats over its socket", reply.as_deref().is_ok_and(|t| t.contains("hello hermes")), format!("{reply:?}"));
    let other = Peer::new(s.sandcastle.runtime());
    let refused = s.sandcastle.refused();
    let (_, answer) = other.connect(&granted)?;
    s.ok("another key with the page's admission is refused by the node", answer["admitted"] == false && s.sandcastle.refused() > refused, &answer);

    // dropping the block removes it
    std::fs::write(site.join("fragment.json"), r#"{}"#)?;
    let o = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().expect("a UTF-8 path")]);
    s.ok("a deploy that drops the block goes live", o.status.success(), out(&o));
    let gone = soon(s, || !s.sandcastle.computers().contains_key(&computer));
    s.ok("its computer is removed from the node", gone, format!("{:?}", events()));
    let r = api.signed(&node, "POST", "/api/sandcastle/credentials", Some(&ask))?;
    s.ok("and its key revoked: the node's ask gets nothing", r.status == 403 && r.body["credentials"].is_null(), &r);
    let r = access(api, &name, Some(&owner), &page.id())?;
    s.ok("and no admission is given", r.status == 404, &r);

    // the node crashes while one is being made: made once when it is back
    let grants_before = s.sandcastle.grants().len();
    s.sandcastle.serving_after(u32::MAX);
    std::fs::write(site.join("fragment.json"), HERMES)?;
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
    let r = access(&api, &name, Some(&owner), &page.id())?;
    s.ok("its owner's page is admitted to the new one, with a new token", r.status == 200 && r.body["token"] != token.as_str() && r.body["endpoint"] != fake.endpoint.as_str(), &r);
    let (api, chat) = chats(s, api, &home, &owner, &name)?;
    chat_page(s, &api, &home, &owner, &name, &view)?;
    ends(s, &api, &home, &owner, &name, &page.id(), &chat)
}

/// The manifest that declares a Hermes.
const HERMES: &str = r#"{"computer":{"preset":"hermes"}}"#;

/// The platform's computer client as a fragment's host serves it: the
/// entry, and the build it names, kept a year (label, held, detail).
pub(crate) fn client_checks(api: &Api, name: &str) -> Result<Vec<(&'static str, bool, String)>> {
    let at = |path: &str| -> Result<crate::api::Reply> {
        let url = reqwest::Url::parse(&api.site_url(name, ""))?.join(path)?;
        api.call(Call { method: "GET", url: url.to_string(), ..Call::default() })
    };
    let header = |r: &crate::api::Reply, h: &str| r.headers.get(h).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let entry = at("/__computer/client.js")?;
    let build = entry.text.lines().find_map(|l| l.strip_prefix("import init from \"./")).and_then(|l| l.split('/').next()).unwrap_or("").to_string();
    let module = at(&format!("/__computer/{build}/sandcastle_web_bg.wasm.gz"))?;
    let glue = at(&format!("/__computer/{build}/sandcastle_web.js"))?;
    Ok(vec![
        (
            "a fragment's host serves the computer client's entry, revalidated on each load",
            entry.status == 200 && header(&entry, "content-type").starts_with("text/javascript") && header(&entry, "cache-control").contains("max-age=0") && build.len() == 16,
            format!("{} {:?} {:?}", entry.status, entry.headers, build),
        ),
        (
            "and its module, gzipped and declared so, under its digest, kept a year",
            module.status == 200
                && header(&module, "content-type") == "application/wasm"
                && header(&module, "content-encoding") == "gzip"
                && header(&module, "cache-control").contains("immutable")
                && module.bytes.starts_with(&[0x1f, 0x8b]),
            format!("{} {:?} {} bytes", module.status, module.headers, module.bytes.len()),
        ),
        ("and its glue", glue.status == 200 && header(&glue, "cache-control").contains("immutable"), format!("{} {:?}", glue.status, glue.headers)),
    ])
}

/// A chat's records on `channel`, as `keys` reads them.
fn records(api: &Api, keys: &Keys, chat: &str, channel: &str) -> Vec<Value> {
    let r = api.signed(keys, "GET", &format!("/api/f/{chat}/channels/{channel}?after=0&limit=500"), None).map(|r| r.body).unwrap_or_default();
    r["records"].as_array().cloned().unwrap_or_default()
}

/// A post to a chat's `chat` channel, as `keys`.
fn post(api: &Api, keys: &Keys, chat: &str, id: &str, body: Value) -> Result<crate::api::Reply> {
    api.signed(keys, "POST", &format!("/api/f/{chat}/channels/chat"), Some(&json!({ "id": id, "body": body })))
}

/// How often `by` said `text` in the chat (its answers carry a turn).
fn said(api: &Api, keys: &Keys, chat: &str, by: &str, text: &str) -> usize {
    records(api, keys, chat, "chat").iter().filter(|r| r["principal"] == by && r["body"]["text"] == text && r["body"]["turn"].is_string()).count()
}

/// A page that is the platform's chat.
const CHAT_PAGE: &str = r#"<!doctype html><html><head><link rel="stylesheet" href="./__chat.css"></head><body><script type="module">import { mount } from "./__chat.js"; mount(document.body);</script></body></html>"#;

/// A chat that names the Hermes as who answers (docs/one-home.md, phase
/// 2), through its Relay on the fake's gateway: its owner and a guest
/// answered by name; someone not in it unheard; one message at a time; its
/// tool steps; a Stop; a Hermes away woken for a message, and one kept
/// across a node restart, each answered once. Answers the node's API
/// after its restart, and the chat.
fn chats(s: &mut Suite, api: Api, home: &std::path::Path, owner: &Keys, hermes: &str) -> Result<(Api, String)> {
    let made = s.cli_json(&api, home, &["create", &s.name("hchat"), "--json"])?;
    let chat = made["name"].as_str().unwrap_or("").to_string();
    s.hook(&api, &made);
    let dir = s.dir("hermes-chat");
    std::fs::write(dir.join("index.html"), CHAT_PAGE)?;
    let manifest = json!({
        "channels": { "chat": { "read": "viewer", "post": "viewer", "signedIn": true }, "work": { "read": "viewer", "post": "editor" } },
        "agent": { "channel": "chat", "computer": hermes },
    });
    std::fs::write(dir.join("fragment.json"), manifest.to_string())?;
    let o = s.cli(&api, home, &["deploy", &chat, "--dir", dir.to_str().expect("a UTF-8 path")]);
    s.ok("a chat that names its owner's Hermes as who answers deploys", o.status.success(), out(&o));
    let member = || {
        let r = api.signed(owner, "GET", &format!("/api/f/{chat}/members"), None).map(|r| r.body).unwrap_or_default();
        r["members"].as_array().into_iter().flatten().find(|m| m["kind"] == "computer" && m["role"] == "editor").and_then(|m| m["principal"].as_str().map(str::to_string))
    };
    s.ok("its Hermes joins it: its computer identity an editor there", soon(s, || member().is_some()), api.signed(owner, "GET", &format!("/api/f/{chat}/members"), None)?);
    let by = member().unwrap_or_default();
    s.ok("and its gateway has dialed the platform's Relay", soon(s, || s.sandcastle.relay_dials() > 0), s.sandcastle.relay_dials());
    let me = api.username(owner)?;

    // the owner's message: Hermes reads it as a group message, with their name
    let heard = s.sandcastle.relay_heard().len();
    post(&api, owner, &chat, "h1", json!({ "text": "hello hermes" }))?;
    let reply = format!("echo: [{me}] hello hermes");
    s.ok("the owner's message is answered there, by its Hermes", soon(s, || said(&api, owner, &chat, &by, &reply) == 1), json!(records(&api, owner, &chat, "chat")));
    let ev = s.sandcastle.relay_heard().get(heard).cloned().unwrap_or_default();
    s.ok(
        "Hermes read it as the chat's group message, with its writer's name",
        ev["source"]["chat_id"] == chat.as_str() && ev["source"]["chat_type"] == "group" && ev["source"]["user_name"] == me.as_str() && ev["text"] == "hello hermes",
        &ev,
    );
    let turn = records(&api, owner, &chat, "chat").iter().find(|r| r["body"]["text"] == reply.as_str()).map(|r| r["body"]["turn"].clone()).unwrap_or_default();
    let owner_id = api.identity(owner)?;
    // its end is written just after its answer
    let bracketed = soon(s, || {
        let work = records(&api, owner, &chat, "work");
        work.iter().any(|r| r["body"]["kind"] == "turn.start" && r["body"]["turn"] == turn && r["body"]["asker"] == owner_id.as_str())
            && work.iter().any(|r| r["body"]["kind"] == "turn.end" && r["body"]["turn"] == turn && r["body"]["outcome"] == "done")
    });
    s.ok("its turn starts and ends in work, asked by the owner, answered", bracketed, json!(records(&api, owner, &chat, "work")));

    // a guest the owner invites: answered too, by name
    let guest = api.person()?;
    let (guest_id, guest_name) = (api.identity(&guest)?, api.username(&guest)?);
    api.signed(owner, "PUT", &format!("/api/f/{chat}/members/{guest_id}"), Some(&json!({ "role": "viewer" })))?;
    post(&api, &guest, &chat, "g1", json!({ "text": "hi from a guest" }))?;
    let guest_reply = format!("echo: [{guest_name}] hi from a guest");
    s.ok("an invited guest's message is answered too, with their name", soon(s, || said(&api, owner, &chat, &by, &guest_reply) == 1), json!(records(&api, owner, &chat, "chat")));
    let stranger = api.person()?;
    let r = post(&api, &stranger, &chat, "x1", json!({ "text": "let me in" }))?;
    let unheard = !s.sandcastle.relay_heard().iter().any(|e| e["text"] == "let me in");
    s.ok("someone not in the chat cannot post there, and Hermes never hears them", r.status == 403 && unheard, &r);

    // one message of the chat's at a time: Hermes in queue mode drops a third writer's mid-turn
    s.sandcastle.slow_turns(2500);
    let heard = s.sandcastle.relay_heard().len();
    post(&api, owner, &chat, "q1", json!({ "text": "first of two" }))?;
    let handed = soon(s, || s.sandcastle.relay_heard().len() == heard + 1);
    post(&api, &guest, &chat, "q2", json!({ "text": "second of two" }))?;
    std::thread::sleep(Duration::from_millis(1200));
    let one = handed && s.sandcastle.relay_heard().len() == heard + 1;
    s.ok("a message that comes during a turn waits: Hermes has one of the chat's at a time", one, json!(s.sandcastle.relay_heard()[heard..]));
    let (a, b) = (format!("echo: [{me}] first of two"), format!("echo: [{guest_name}] second of two"));
    let both = soon(s, || said(&api, owner, &chat, &by, &a) == 1 && said(&api, owner, &chat, &by, &b) == 1);
    let texts: Vec<String> = records(&api, owner, &chat, "chat").iter().filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).collect();
    let in_order = texts.iter().position(|t| *t == a) < texts.iter().position(|t| *t == b);
    s.ok("then the next: both answered, in order", both && in_order, json!(texts));
    s.sandcastle.slow_turns(0);

    // its tool progress, as the turn's steps
    post(&api, owner, &chat, "t1", json!({ "text": "use a tool please" }))?;
    let steps = || records(&api, owner, &chat, "work").iter().filter(|r| r["body"]["kind"] == "turn.step").filter_map(|r| r["body"]["tool"].as_str().map(str::to_string)).collect::<Vec<_>>();
    s.ok(
        "its tool progress shows as the turn's steps",
        soon(s, || {
            let st = steps();
            st.iter().any(|t| t == "💻 terminal") && st.iter().any(|t| t == "🔍 Searching the web")
        }),
        json!(steps()),
    );

    // a Stop cuts a turn short
    s.sandcastle.slow_turns(8000);
    let interrupted = s.sandcastle.relay_interrupted();
    let long = post(&api, owner, &chat, "s1", json!({ "text": "a long one" }))?;
    let stopped_turn = format!("hermes:{}", long.body["record"]["seq"]);
    let started = soon(s, || records(&api, owner, &chat, "work").iter().any(|r| r["body"]["kind"] == "turn.start" && r["body"]["turn"] == stopped_turn.as_str()));
    post(&api, owner, &chat, "stop-s1", json!({ "kind": "stop", "turn": stopped_turn }))?;
    let ended = soon(s, || records(&api, owner, &chat, "work").iter().any(|r| r["body"]["kind"] == "turn.end" && r["body"]["turn"] == stopped_turn.as_str() && r["body"]["outcome"] == "stopped"));
    let unanswered = !records(&api, owner, &chat, "chat").iter().any(|r| r["principal"] == by.as_str() && r["body"]["turn"] == stopped_turn.as_str());
    s.ok("a Stop interrupts Hermes, and its turn ends with no answer", started && ended && unanswered && s.sandcastle.relay_interrupted() > interrupted, json!(records(&api, owner, &chat, "work")));
    s.sandcastle.slow_turns(0);

    // its Hermes away (its computer asleep): woken, and answers once
    s.sandcastle.relay_away();
    std::thread::sleep(Duration::from_millis(500));
    let wakes = s.sandcastle.wakes();
    post(&api, owner, &chat, "w1", json!({ "text": "are you awake?" }))?;
    s.ok("with its Hermes away, the message is kept and its computer woken", soon(s, || s.sandcastle.wakes() > wakes), s.sandcastle.wakes());
    let woke = format!("echo: [{me}] are you awake?");
    let once = |s: &Suite, text: &str, reply: &str| s.sandcastle.relay_heard().iter().filter(|e| e["text"] == text).count() == 1 && said(&api, owner, &chat, &by, reply) == 1;
    s.ok("it comes back for it, and answers once", soon(s, || once(s, "are you awake?", &woke)), json!(records(&api, owner, &chat, "chat")));

    // kept across a node restart
    s.sandcastle.relay_away();
    std::thread::sleep(Duration::from_millis(500));
    post(&api, owner, &chat, "r1", json!({ "text": "after a restart?" }))?;
    s.crash()?;
    let api = s.start(false, true)?;
    let restarted = format!("echo: [{me}] after a restart?");
    let kept = soon(s, || {
        let heard = s.sandcastle.relay_heard().iter().filter(|e| e["text"] == "after a restart?").count() == 1;
        heard && said(&api, owner, &chat, &by, &restarted) == 1
    });
    s.ok("a message kept for it across a node restart is answered once", kept, json!(records(&api, owner, &chat, "chat")));
    Ok((api, chat))
}

/// The hermes template in headless Chrome: the platform's chat, answered
/// by this fragment's own Hermes, its reply streaming as a draft before it
/// lands; and no chat for someone signed out. Its client still serves for
/// the computer's screen (phase 4).
fn chat_page(s: &mut Suite, api: &Api, home: &std::path::Path, owner: &Keys, name: &str, view: &str) -> Result<()> {
    let dir = s.dir("hermes-page");
    let dir_s = dir.to_str().expect("a UTF-8 path").to_string();
    let o = s.cli(api, home, &["new", &dir_s, "--template", "hermes"]);
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("fragment.json")).unwrap_or_default()).unwrap_or_default();
    s.ok(
        "the hermes template scaffolds a chat its own Hermes answers",
        o.status.success() && manifest["computer"] == json!({ "preset": "hermes" }) && manifest["agent"] == json!({ "channel": "chat", "computer": true }),
        out(&o),
    );
    let o = s.cli(api, home, &["deploy", name, "--dir", &dir_s]);
    s.ok("and deploys onto the fragment that has one", o.status.success(), out(&o));
    for (label, held, detail) in client_checks(api, name)? {
        s.ok(label, held, detail);
    }
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the hermes page (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = Duration::from_secs(30);
    let session = api.sign_in(&crate::cli_email(&fragment_core::npub::encode(owner.pubkey_hex()))?)?;
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let tab = chrome.open(&api.site_url(name, "__signin?return=/"))?;
    let said = |chrome: &mut crate::browser::Browser, tab: &crate::browser::Page| chrome.eval(tab, "(document.getElementById('messages')?.innerText || '').slice(-600)").unwrap_or_default();
    let ready = chrome.until(&tab, "document.getElementById('say')?.dataset.ready === '1'", wait);
    s.ok("signed in, the page is the platform's chat, ready", ready, said(&mut chrome, &tab));
    // its own Hermes joined its chat before a message is asked of it
    let joined = soon(s, || {
        let r = api.signed(owner, "GET", &format!("/api/f/{name}/members"), None).map(|r| r.body).unwrap_or_default();
        r["members"].as_array().into_iter().flatten().any(|m| m["kind"] == "computer" && m["role"] == "editor")
    });
    s.ok("its own Hermes answers its chat: a member there", joined, "");
    s.sandcastle.slow_turns(4000);
    let me = api.username(owner)?;
    chrome.eval(&tab, "document.getElementById('text').value = 'hello from the page'; document.getElementById('say').requestSubmit(); true")?;
    let half = "echo: [";
    let drafting = format!("[...document.querySelectorAll('.msg.agent.streaming')].some(m => m.textContent.includes({half:?}) && !!m.querySelector('.cursor') && !!m.querySelector('.who .face.hermes') && m.querySelector('.who')?.textContent.endsWith('Hermes'))");
    s.ok("its reply streams in as a draft, by Hermes, as it writes", chrome.until(&tab, &drafting, wait), said(&mut chrome, &tab));
    let landed = format!("!document.querySelector('.msg.agent.streaming') && [...document.querySelectorAll('.msg.agent .md')].some(m => m.textContent === {:?})", format!("echo: [{me}] hello from the page"));
    s.ok("and gives way to its answer once the turn ends", chrome.until(&tab, &landed, wait), said(&mut chrome, &tab));
    s.sandcastle.slow_turns(0);
    chrome.reload(&tab)?;
    s.ok("a reload shows it from the chat's own records", chrome.until(&tab, &landed, wait), said(&mut chrome, &tab));
    screen(s, &mut chrome, &tab)?;

    // signed out, with its link (this one's fragment opens to its link): the
    // chat reads, and writing in it asks them to sign in (a member's alone)
    let context = chrome.another_context()?;
    let stranger = chrome.open_in(&context, &api.site_url(name, &format!("?view={view}")))?;
    let asked = chrome.until(&stranger, "document.getElementById('say')?.dataset.ready === '1' && !!document.querySelector('#note a[href*=\"__signin\"]')", wait);
    s.ok("signed out, with its link, the page asks them to sign in to write", asked, chrome.eval(&stranger, "document.body.innerText.slice(0, 300)").unwrap_or_default());
    let unseen = chrome.until(&stranger, "document.getElementById('screen').dataset.access === 'refused' && document.getElementById('screen').hidden", wait);
    s.ok("and shows no screen: it is its owner's", unseen, chrome.eval(&stranger, SCREEN_STATE).unwrap_or_default());
    Ok(())
}

/// How the page's screen pane stands (its data attributes and its line).
const SCREEN_STATE: &str = "(() => { const r = document.getElementById('screen'); return JSON.stringify({ hidden: r.hidden, ...r.dataset, line: r.querySelector('#screen-state')?.textContent, control: r.querySelector('#screen-control')?.textContent }); })()";

/// A press and release of the left button at the desktop's (x, y), as the
/// page maps it (the desktop fits the pane, centred).
fn click_at(x: u16, y: u16) -> String {
    format!(
        "(() => {{ const c = document.querySelector('#screen canvas'); const r = c.getBoundingClientRect();
          const scale = Math.min(r.width / c.width, r.height / c.height);
          const at = {{ clientX: r.left + (r.width - c.width * scale) / 2 + ({x} + 0.5) * scale, clientY: r.top + (r.height - c.height * scale) / 2 + ({y} + 0.5) * scale, bubbles: true }};
          c.dispatchEvent(new PointerEvent('pointerdown', {{ ...at, buttons: 1 }}));
          c.dispatchEvent(new PointerEvent('pointerup', {{ ...at, buttons: 0 }}));
          return true; }})()"
    )
}

/// The desktop's pixel at (x, y) on the page's canvas is `rgb`.
fn pixel_is(x: u16, y: u16, rgb: [u8; 3]) -> String {
    format!(
        "(() => {{ const d = document.querySelector('#screen canvas').getContext('2d').getImageData({x}, {y}, 1, 1).data; return d[0] === {} && d[1] === {} && d[2] === {}; }})()",
        rgb[0], rgb[1], rgb[2]
    )
}

/// Its screen beside the chat, on its owner's page (docs/one-home.md, phase
/// 4): live, Hermes driving; taken over, the owner's click and keys reach
/// the desktop; given back, they go nowhere.
fn screen(s: &mut Suite, chrome: &mut crate::browser::Browser, tab: &crate::browser::Page) -> Result<()> {
    use fragment_fakes::hermes_screen::{COLOURS, HEIGHT, WIDTH};
    let wait = Duration::from_secs(30);
    let state = |chrome: &mut crate::browser::Browser| chrome.eval(tab, SCREEN_STATE).unwrap_or_default();
    let (x, y) = (WIDTH / 2, HEIGHT / 2);
    let live = format!(
        "(() => {{ const r = document.getElementById('screen'); return !r.hidden && r.dataset.access === 'admitted' && r.dataset.size === '{WIDTH}x{HEIGHT}' && Number(r.dataset.frames) > 1 && r.dataset.lease === 'agent'; }})()"
    );
    s.ok("its screen shows beside the chat to its owner, live, Hermes driving", chrome.until(tab, &live, wait), state(chrome));
    s.ok("through a ticket of Hermes' own, on its display socket", s.sandcastle.screen_sockets() > 0, s.sandcastle.screen_sockets());
    s.ok("its pixels the desktop's", chrome.until(tab, &pixel_is(x, y, COLOURS[0]), wait), state(chrome));
    let before = s.sandcastle.screen_input().len();
    chrome.eval(tab, &click_at(x, y))?;
    std::thread::sleep(Duration::from_millis(800));
    s.ok("while Hermes drives, a click on it goes nowhere", s.sandcastle.screen_input().len() == before && s.sandcastle.screen_dropped() == 0, json!(s.sandcastle.screen_input()));

    chrome.eval(tab, "document.getElementById('screen-control').click(); true")?;
    let mine = "document.getElementById('screen').dataset.lease === 'mine' && document.getElementById('screen-control').textContent === 'Give back'";
    s.ok("Take over: the screen is its owner's", chrome.until(tab, mine, wait), state(chrome));
    chrome.eval(tab, &click_at(x, y))?;
    let clicked = soon(s, || s.sandcastle.screen_input()[before..].iter().any(|e| e["kind"] == "pointer" && e["mask"] == 1 && e["x"] == x && e["y"] == y));
    s.ok("their click reaches the desktop, where they clicked", clicked, json!(s.sandcastle.screen_input()));
    s.ok("and the desktop answers on their screen", chrome.until(tab, &pixel_is(x, y, COLOURS[1]), wait), state(chrome));
    chrome.eval(tab, "(() => { const c = document.querySelector('#screen canvas'); for (const t of ['keydown', 'keyup']) c.dispatchEvent(new KeyboardEvent(t, { key: 'h', bubbles: true, cancelable: true })); return true; })()")?;
    let keyed = soon(s, || {
        let keys: Vec<(bool, u64)> = s.sandcastle.screen_input()[before..].iter().filter(|e| e["kind"] == "key").map(|e| (e["down"] == true, e["keysym"].as_u64().unwrap_or(0))).collect();
        keys == [(true, 0x68), (false, 0x68)]
    });
    s.ok("and their keys, pressed and let go", keyed, json!(s.sandcastle.screen_input()));

    chrome.eval(tab, "document.getElementById('screen-control').click(); true")?;
    let back = "document.getElementById('screen').dataset.lease === 'agent' && document.getElementById('screen-control').textContent === 'Take over'";
    s.ok("Give back: Hermes drives again", chrome.until(tab, back, wait), state(chrome));
    let after = s.sandcastle.screen_input().len();
    chrome.eval(tab, &click_at(x, y))?;
    std::thread::sleep(Duration::from_millis(800));
    s.ok("and their click goes nowhere again", s.sandcastle.screen_input().len() == after, json!(s.sandcastle.screen_input()));
    Ok(())
}

/// The ways a Hermes ends: its owner removes its key's computer (a later
/// deploy that declares it makes a new one), and its fragment is deleted.
fn ends(s: &mut Suite, api: &Api, home: &std::path::Path, owner: &Keys, name: &str, peer: &str, chat: &str) -> Result<()> {
    let hermes_of = |api: &Api| -> Vec<String> {
        let r = api.signed(owner, "GET", "/api/identities/me", None).map(|r| r.body).unwrap_or_default();
        r["computers"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str()).filter(|n| n.contains("hermes")).map(str::to_string).collect()
    };
    let serving = |s: &Suite| s.sandcastle.computers().into_iter().filter(|(_, c)| c.spec["service"]["env"]["HERMES_DASHBOARD"] == "1").count();
    let [identity] = hermes_of(api).try_into().map_err(|v| anyhow::anyhow!("one Hermes computer of its owner's: {v:?}"))?;
    let closes = s.sandcastle.relay_closes().len();
    let o = s.cli(api, home, &["computers", "rm", &identity]);
    s.ok("its owner removes its key's computer", o.status.success(), out(&o));
    let gone = soon(s, || serving(s) == 0);
    s.ok("and its computer leaves the node", gone, format!("{:?}", s.sandcastle.computers().keys()));
    s.ok("its gateway's socket is closed, refused (4401)", soon(s, || s.sandcastle.relay_closes()[closes..].contains(&4401)), json!(s.sandcastle.relay_closes()));
    let r = access(api, name, Some(owner), peer)?;
    s.ok("with no Hermes, no session", r.status == 404, &r);
    // said while there is none: the new one hears it as it joins
    post(api, owner, chat, "n0", json!({ "text": "anyone there?" }))?;
    let dir = s.dir("hermes-template");
    let o = s.cli(api, home, &["new", dir.to_str().expect("a UTF-8 path"), "--template", "hermes"]);
    std::fs::write(dir.join("site/notes.txt"), "declared again")?;
    let deployed = s.cli(api, home, &["deploy", name, "--dir", dir.to_str().expect("a UTF-8 path")]);
    s.ok("a deploy that declares it again goes live", o.status.success() && deployed.status.success(), out(&deployed));
    let again = soon(s, || serving(s) == 1 && hermes_of(api).len() == 1 && access(api, name, Some(owner), peer).is_ok_and(|r| r.status == 200));
    s.ok("and makes a new one", again, format!("{:?} {:?}", s.sandcastle.computers().keys(), hermes_of(api)));
    // the chat that named it joins the new one: a new identity
    let me = api.username(owner)?;
    let new_by = || {
        let r = api.signed(owner, "GET", &format!("/api/f/{chat}/members"), None).map(|r| r.body).unwrap_or_default();
        r["members"].as_array().into_iter().flatten().filter(|m| m["kind"] == "computer").filter_map(|m| m["principal"].as_str().map(str::to_string)).collect::<Vec<_>>()
    };
    let rejoined = Duration::from_secs(60);
    let one = s.eventually(rejoined, || new_by().len() == 1 && hermes_of(api).len() == 1);
    let by = new_by().pop().unwrap_or_default();
    post(api, owner, chat, "n1", json!({ "text": "are you new?" }))?;
    let answered = s.eventually(rejoined, || said(api, owner, chat, &by, &format!("echo: [{me}] are you new?")) == 1);
    s.ok("a chat that named it joins the new one, which answers there", one && answered, json!({ "members": new_by(), "chat": records(api, owner, chat, "chat") }));
    let caught_up = s.eventually(rejoined, || said(api, owner, chat, &by, &format!("echo: [{me}] anyone there?")) == 1);
    s.ok("and answers what was said while there was none, once", caught_up, json!(records(api, owner, chat, "chat")));
    let o = s.cli(api, home, &["rm", name]);
    s.ok("its owner deletes the fragment", o.status.success(), out(&o));
    let gone = soon(s, || serving(s) == 0 && hermes_of(api).is_empty());
    s.ok("and its Hermes goes with it: its computer, and its key's computer", gone, format!("{:?} {:?}", s.sandcastle.computers().keys(), hermes_of(api)));
    Ok(())
}

/// The fragment's events, `kind: summary`.
fn events_of(api: &Api, owner: &Keys, name: &str) -> Vec<String> {
    let r = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=50"), None).map(|r| r.body).unwrap_or_default();
    r["events"].as_array().into_iter().flatten().map(|e| format!("{}: {}", e["kind"].as_str().unwrap_or(""), e["summary"].as_str().unwrap_or(""))).collect()
}
