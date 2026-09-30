//! A fragment's own Hermes on the fleet's sandcastle node
//! (docs/hermes-chat.md, docs/runtime-seam.md), on the sandcastle fake: a
//! deploy that declares `"computer": {"preset": "hermes"}` has the platform
//! make a key for it
//! (in `KEYS`), register it as a computer its owner owns, grant it one
//! computer with the platform's grantor key, and make that computer:
//! Hermes in loopback mode behind its bridge, reached by its key over iroh,
//! with no login and no password. A viewer who owns or edits the fragment
//! gets an admission for their page's iroh key (`POST /__hermes/access`),
//! signed by the computer's key, and Hermes' session token; anyone else,
//! and anyone signed out, is refused. Its model calls are billed to the
//! fragment's owner. The template's page does all of that in headless
//! Chrome, with the platform's computer client (served on every host at
//! `/__computer/`). A deploy that drops the block removes the computer and
//! revokes its key.

use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use fragment_proto::ErrorCode;
use iroh::endpoint::{presets, Connection};
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl};
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
fn access(api: &Api, name: &str, keys: Option<&Keys>, peer: &str) -> Result<Reply> {
    let body = json!({ "peer": peer }).to_string().into_bytes();
    api.call(Call { method: "POST", url: api.site_url(name, "__hermes/access"), body: Some(body), content_type: Some("application/json"), keys, ..Call::default() })
}

/// A page's own iroh endpoint, as the platform's client makes one, on the
/// fake node's relay.
struct Peer {
    ep: Endpoint,
    rt: tokio::runtime::Handle,
}

impl Peer {
    fn new(s: &Suite) -> Result<Peer> {
        let rt = s.sandcastle.runtime();
        let relay: RelayUrl = s.sandcastle.relay.parse()?;
        let ep = rt.block_on(Endpoint::builder(presets::Minimal).relay_mode(RelayMode::Custom(relay.into())).bind())?;
        Ok(Peer { ep, rt })
    }

    fn id(&self) -> String {
        self.ep.id().to_string()
    }

    /// Connects to the computer an access names and presents its admission:
    /// the connection and the node's answer.
    fn connect(&self, access: &Value) -> Result<(Connection, Value)> {
        let endpoint = access["endpoint"].as_str().context("an endpoint")?.parse()?;
        let relay: RelayUrl = access["relay"].as_str().context("a relay")?.parse()?;
        let admission = access["admission"].as_str().context("an admission")?.to_string();
        self.rt.block_on(async {
            let conn = self.ep.connect(EndpointAddr::new(endpoint).with_relay_url(relay), b"sandcastle/1").await?;
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
    fn tunnel(&self, conn: Connection) -> Result<u16> {
        let listener = self.rt.block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
        let port = listener.local_addr()?.port();
        self.rt.spawn(async move {
            // Bounded by the lane: the fake's runtime ends with the suite.
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

/// One turn in a new chat over Hermes' `/api/ws?token=`, through a tunnel's
/// port: the reply's text.
fn turn(port: u16, token: &str, text: &str) -> Result<String> {
    let url = format!("ws://127.0.0.1:{port}/api/ws?token={token}");
    let mut req = tungstenite::client::IntoClientRequest::into_client_request(url.as_str())?;
    req.headers_mut().insert("sec-websocket-protocol", "hermes-gateway-v1".parse()?);
    let (mut ws, _) = tungstenite::connect(req)?;
    if let tungstenite::stream::MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(30)))?;
    }
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

/// A GET through a tunnel's port, with Hermes' session token or none.
fn get(port: u16, path: &str, token: Option<&str>) -> Result<(u16, Value)> {
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
    let page = Peer::new(s)?;
    // a Hermes being made answers "starting", however soon it is asked
    s.sandcastle.serving_after(3);
    let o = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().expect("a UTF-8 path")]);
    s.ok("a deploy that declares a Hermes goes live", o.status.success(), out(&o));
    let r = access(api, &name, Some(&owner), &page.id())?;
    s.ok("while it is being made, an admission is not ready yet", r.status == 409 && r.code() == Some(ErrorCode::NotReady), &r);

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
    s.ok("an editor is admitted too", r.status == 200 && r.body["admission"].is_string(), &r);

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
    let other = Peer::new(s)?;
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
    chat_page(s, &api, &home, &owner, &name, &view)?;
    ends(s, &api, &home, &owner, &name, &page.id())
}

/// The manifest that declares a Hermes.
const HERMES: &str = r#"{"computer":{"preset":"hermes"}}"#;

/// The platform's computer client as a fragment's host serves it: the
/// entry, and the build it names, kept a year.
fn client(s: &mut Suite, api: &Api, name: &str) -> Result<()> {
    let at = |path: &str| -> Result<crate::api::Reply> {
        let url = reqwest::Url::parse(&api.site_url(name, ""))?.join(path)?;
        api.call(Call { method: "GET", url: url.to_string(), ..Call::default() })
    };
    let header = |r: &crate::api::Reply, h: &str| r.headers.get(h).and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let entry = at("/__computer/client.js")?;
    let build = entry.text.lines().find_map(|l| l.strip_prefix("import init from \"./")).and_then(|l| l.split('/').next()).unwrap_or("").to_string();
    s.ok(
        "a fragment's host serves the computer client's entry, revalidated on each load",
        entry.status == 200 && header(&entry, "content-type").starts_with("text/javascript") && header(&entry, "cache-control").contains("max-age=0") && build.len() == 16,
        format!("{} {:?} {:?}", entry.status, entry.headers, build),
    );
    let module = at(&format!("/__computer/{build}/sandcastle_web_bg.wasm.gz"))?;
    s.ok(
        "and its module, gzipped and declared so, under its digest, kept a year",
        module.status == 200
            && header(&module, "content-type") == "application/wasm"
            && header(&module, "content-encoding") == "gzip"
            && header(&module, "cache-control").contains("immutable")
            && module.bytes.starts_with(&[0x1f, 0x8b]),
        format!("{} {:?} {} bytes", module.status, module.headers, module.bytes.len()),
    );
    let glue = at(&format!("/__computer/{build}/sandcastle_web.js"))?;
    s.ok("and its glue", glue.status == 200 && header(&glue, "cache-control").contains("immutable"), format!("{} {:?}", glue.status, glue.headers));
    Ok(())
}

/// The hermes template's page in headless Chrome: the computer client,
/// an admission for the page's key, and Hermes by its key through the
/// relay: a chat streamed, listed, and read again from Hermes' history; a
/// long turn pinged; and no chat for someone signed out.
fn chat_page(s: &mut Suite, api: &Api, home: &std::path::Path, owner: &Keys, name: &str, view: &str) -> Result<()> {
    let dir = s.dir("hermes-page");
    let dir_s = dir.to_str().expect("a UTF-8 path").to_string();
    let o = s.cli(api, home, &["new", &dir_s, "--template", "hermes"]);
    let manifest: Value = serde_json::from_slice(&std::fs::read(dir.join("fragment.json")).unwrap_or_default()).unwrap_or_default();
    s.ok("the hermes template scaffolds, declaring a Hermes computer", o.status.success() && manifest["computer"] == json!({ "preset": "hermes" }), out(&o));
    let o = s.cli(api, home, &["deploy", name, "--dir", &dir_s]);
    s.ok("and deploys onto the fragment that has one", o.status.success(), out(&o));
    client(s, api, name)?;
    let Some(mut chrome) = s.browser()? else {
        s.ok("Chrome is installed for the hermes page (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let wait = Duration::from_secs(30);
    let session = api.sign_in(&crate::cli_email(&fragment_core::npub::encode(owner.pubkey_hex()))?)?;
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let tab = chrome.open(&api.site_url(name, "__signin?return=/"))?;
    let said = |chrome: &mut crate::browser::Browser, tab: &crate::browser::Page| chrome.eval(tab, "document.body.dataset.state + ': ' + document.body.innerText.slice(0, 400)").unwrap_or_default();
    let admitted = s.sandcastle.admitted();
    let ready = chrome.until(&tab, "document.body.dataset.state === 'ready'", wait);
    s.ok("signed in, the page reaches its Hermes by its key: the client, an admission, the relay", ready && s.sandcastle.admitted() > admitted, said(&mut chrome, &tab));
    let submit = |text: &str| format!("document.getElementById('text').value = {text:?}; document.getElementById('composer').requestSubmit(); true");
    let replied = |text: &str| format!("[...document.querySelectorAll('.row.assistant')].some(r => r.textContent === {:?} && !r.classList.contains('streaming'))", format!("echo: {text}"));
    chrome.eval(&tab, &submit("hello from the page"))?;
    s.ok("a message from the page gets Hermes' reply, streamed over its socket", chrome.until(&tab, &replied("hello from the page"), wait), said(&mut chrome, &tab));
    let listed = "document.querySelector('#chats li.open')?.textContent === 'hello from the page'";
    s.ok("the new chat is listed and open", chrome.until(&tab, listed, wait), said(&mut chrome, &tab));
    chrome.reload(&tab)?;
    let history = "document.body.dataset.state === 'ready' && document.querySelectorAll('.row.user').length === 1 && [...document.querySelectorAll('.row.assistant')].some(r => r.textContent === 'echo: hello from the page')";
    s.ok("a reload opens it again, from Hermes' history", chrome.until(&tab, history, wait) && chrome.until(&tab, listed, wait), said(&mut chrome, &tab));

    // a long turn: the page pings while it runs (the node's sign of a turn)
    s.sandcastle.slow_turns(17_000);
    let pinged = s.sandcastle.pings();
    chrome.eval(&tab, &submit("take your time"))?;
    let long = chrome.until(&tab, &replied("take your time"), Duration::from_secs(40));
    s.sandcastle.slow_turns(0);
    s.ok("a long turn in the same chat, resumed on a new socket, is answered", long, said(&mut chrome, &tab));
    s.ok("and the page pinged its Hermes while it ran", s.sandcastle.pings() > pinged, format!("{} pings before, {} after", pinged, s.sandcastle.pings()));
    let one = chrome.eval(&tab, "document.querySelectorAll('#chats li').length === 1 && document.querySelectorAll('.row.user').length === 2")?;
    s.ok("one chat holds both turns", one == json!(true), said(&mut chrome, &tab));

    // signed out, with its link: no chat, and a way to sign in
    let context = chrome.another_context()?;
    let stranger = chrome.open_in(&context, &api.site_url(name, &format!("?view={view}")))?;
    let signin = chrome.until(&stranger, "document.body.dataset.state === 'signin' && !!document.querySelector('#status a[href*=\"__signin\"]')", wait);
    s.ok("signed out, the page offers sign-in and no chat", signin, said(&mut chrome, &stranger));
    Ok(())
}

/// The ways a Hermes ends: its owner removes its key's computer (a later
/// deploy that declares it makes a new one), and its fragment is deleted.
fn ends(s: &mut Suite, api: &Api, home: &std::path::Path, owner: &Keys, name: &str, peer: &str) -> Result<()> {
    let hermes_of = |api: &Api| -> Vec<String> {
        let r = api.signed(owner, "GET", "/api/identities/me", None).map(|r| r.body).unwrap_or_default();
        r["computers"].as_array().into_iter().flatten().filter_map(|c| c["name"].as_str()).filter(|n| n.contains("hermes")).map(str::to_string).collect()
    };
    let serving = |s: &Suite| s.sandcastle.computers().into_iter().filter(|(_, c)| c.spec["service"]["env"]["HERMES_DASHBOARD"] == "1").count();
    let [identity] = hermes_of(api).try_into().map_err(|v| anyhow::anyhow!("one Hermes computer of its owner's: {v:?}"))?;
    let o = s.cli(api, home, &["computers", "rm", &identity]);
    s.ok("its owner removes its key's computer", o.status.success(), out(&o));
    let gone = soon(s, || serving(s) == 0);
    s.ok("and its computer leaves the node", gone, format!("{:?}", s.sandcastle.computers().keys()));
    let r = access(api, name, Some(owner), peer)?;
    s.ok("with no Hermes, no session", r.status == 404, &r);
    let dir = s.dir("hermes-template");
    let o = s.cli(api, home, &["new", dir.to_str().expect("a UTF-8 path"), "--template", "hermes"]);
    std::fs::write(dir.join("site/notes.txt"), "declared again")?;
    let deployed = s.cli(api, home, &["deploy", name, "--dir", dir.to_str().expect("a UTF-8 path")]);
    s.ok("a deploy that declares it again goes live", o.status.success() && deployed.status.success(), out(&deployed));
    let again = soon(s, || serving(s) == 1 && hermes_of(api).len() == 1 && access(api, name, Some(owner), peer).is_ok_and(|r| r.status == 200));
    s.ok("and makes a new one", again, format!("{:?} {:?}", s.sandcastle.computers().keys(), hermes_of(api)));
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
