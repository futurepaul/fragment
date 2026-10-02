//! A sandcastle node (sandcastle/README.md), as much of it as the platform
//! touches, with a Hermes behind each computer's key (docs/hermes-chat.md,
//! docs/runtime-seam.md):
//!
//! - The API: `PUT /v1/grants/{pubkey}` (the grantor's only), `PUT|GET|
//!   DELETE /v1/computers/{name}` (a key with a grant; its own computers
//!   only), and `GET /v1/health` (unsigned: the node's key). Every signed
//!   call is NIP-98 checked as the node checks it, the URL its own, and a
//!   replayed header is refused (the node's cache keys on the event id).
//! - A computer is reached by its key, as `sandcastled --iroh-relay` serves
//!   it: an iroh endpoint of its own (ALPN `sandcastle/1`, through an
//!   in-process relay over plain HTTP, which a browser on a dev fleet's
//!   http page reaches too). A connection opens with an admission, signed
//!   by the computer's owner for this peer, computer, and node; each
//!   further stream is piped to the computer's Hermes.
//! - Its Hermes is Hermes in loopback mode on a port of its own: a loopback
//!   Host only (else 400), its session token (the spec's
//!   `HERMES_DASHBOARD_SESSION_TOKEN`) on `api/sessions` and a session's
//!   `messages` (else 401), `api/status` open, and `api/ws?token=`, whose
//!   upgrade names no subprotocol (Hermes v0.21.5 names one only for its
//!   ticket), JSON-RPC (`session.create`, `session.resume`,
//!   `prompt.submit`, whose turn echoes the prompt in two deltas,
//!   `gateway.ping`, and its screen's `display.*`), and its screen's
//!   `api/display/ws?display_ticket=` (hermes_screen.rs).
//!
//! - Its Hermes' gateway dials the platform's Relay when its spec names
//!   `GATEWAY_RELAY_URL` (relay_gateway.rs); the owner's `POST
//!   /v1/computers/{name}/wake` brings an `away` gateway back.
//!
//! A computer serves after `serving_after` looks at it (a platform waits
//! for it), and never sleeps. A turn's second delta waits `slow_turns` (a
//! page's heartbeat is heard meanwhile: `pings`), and a Relay turn's reply
//! as much between its two parts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use fragment_nip98::Keys;
use iroh::endpoint::{presets, Connection, RecvStream, SendStream};
use iroh::{Endpoint, RelayMode, RelayUrl};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::hermes_screen::{self, Screens};
use crate::http::{Request, Response, Server};
use crate::relay_gateway;

const ALPN: &[u8] = b"sandcastle/1";
const STREAM_ADMISSION: u8 = b'A';
const STREAM_HTTP: u8 = b'H';

#[derive(Clone, Debug)]
pub struct Computer {
    pub owner: String,
    pub spec: Value,
    /// GETs of its view so far: it serves once they reach `serving_after`.
    pub looks: u32,
    /// Its iroh key (64 hex).
    pub endpoint: String,
    /// Where its Hermes listens.
    pub port: u16,
}

#[derive(Default)]
struct State {
    grants: BTreeMap<String, Value>,
    computers: BTreeMap<String, Computer>,
    seen: HashSet<String>,
    /// Each computer's endpoint and its Hermes' server, while it lives.
    running: HashMap<String, (Endpoint, Server)>,
    /// Stored chats per computer: id → (title, messages).
    chats: HashMap<String, BTreeMap<String, (String, Vec<Value>)>>,
    serving_after: u32,
    /// How long a turn's reply takes between its two deltas.
    turn_ms: u64,
    /// How long a grant takes to be written (a Hermes being made waits).
    grant_ms: u64,
    /// `gateway.ping`s heard, on every socket.
    pings: u64,
    /// Admissions taken and refused, on every endpoint.
    admitted: u64,
    refused: u64,
    next: u64,
    /// The computers whose gateway runs.
    gateways: HashSet<String>,
    /// Their screens (each computer's own, by name).
    screens: Arc<Mutex<Screens>>,
}

pub struct Sandcastle {
    pub url: String,
    pub grantor: String,
    /// The node's own key (64 hex): what an admission names.
    pub node: String,
    /// The in-process relay's URL (plain HTTP).
    pub relay: String,
    state: Arc<Mutex<State>>,
    gateways: Arc<Mutex<relay_gateway::Shared>>,
    _server: Server,
    _relay: iroh_relay::server::Server,
    runtime: tokio::runtime::Runtime,
}

fn now_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after 1970").as_secs() as i64
}

fn problem(status: u16, code: &str, message: &str) -> Response {
    Response::json(status, &json!({ "code": code, "message": message }))
}

/// What the API's handler holds besides the state.
struct Node {
    base: String,
    grantor: String,
    node: String,
    relay: RelayUrl,
    runtime: tokio::runtime::Handle,
    /// What the computers' Relay gateways share with the fake.
    gateways: Arc<Mutex<relay_gateway::Shared>>,
}

impl Sandcastle {
    /// A node whose grants only `grantor` (64 hex) may write.
    pub fn start(grantor: &str) -> std::io::Result<Sandcastle> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
        let relay = runtime
            .block_on(async {
                let mut config = iroh_relay::server::ServerConfig::default();
                config.relay = Some(iroh_relay::server::RelayConfig::new(std::net::SocketAddr::from(([127, 0, 0, 1], 0))));
                iroh_relay::server::Server::spawn(config).await
            })
            .map_err(|e| std::io::Error::other(format!("the relay: {e}")))?;
        let addr = relay.http_addr().ok_or_else(|| std::io::Error::other("the relay serves no HTTP"))?;
        let relay_url: RelayUrl = format!("http://{addr}").parse().map_err(|e| std::io::Error::other(format!("the relay's URL: {e}")))?;
        let node = Keys::generate().pubkey_hex().to_string();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let state = Arc::new(Mutex::new(State { serving_after: 1, ..State::default() }));
        let gateways = Arc::new(Mutex::new(relay_gateway::Shared::default()));
        let n = Arc::new(Node { base: url.clone(), grantor: grantor.to_string(), node: node.clone(), relay: relay_url.clone(), runtime: runtime.handle().clone(), gateways: Arc::clone(&gateways) });
        let s = Arc::clone(&state);
        let server = Server::serve(listener, Arc::new(move |req: &Request| handle(&s, &n, req)))?;
        Ok(Sandcastle { url, grantor: grantor.to_string(), node, relay: relay_url.to_string(), state, gateways, _server: server, _relay: relay, runtime })
    }

    /// Its computers now (a test lever).
    pub fn computers(&self) -> BTreeMap<String, Computer> {
        self.state.lock().unwrap().computers.clone()
    }

    pub fn grants(&self) -> BTreeMap<String, Value> {
        self.state.lock().unwrap().grants.clone()
    }

    /// Looks a computer's view takes before it serves (default 1).
    pub fn serving_after(&self, looks: u32) {
        self.state.lock().unwrap().serving_after = looks;
    }

    /// How long each grant takes (default none): a Hermes stays being made
    /// that long after its identity is registered, while its chats may join.
    pub fn slow_grants(&self, ms: u64) {
        self.state.lock().unwrap().grant_ms = ms;
    }

    /// How long each turn takes between its two deltas (default none).
    pub fn slow_turns(&self, ms: u64) {
        self.state.lock().unwrap().turn_ms = ms;
        self.gateways.lock().unwrap().turn_ms = ms;
    }

    /// Each message its Hermes' gateways were handed, as Hermes read it.
    pub fn relay_heard(&self) -> Vec<Value> {
        self.gateways.lock().unwrap().heard.clone()
    }

    /// The close codes its gateways' sockets got.
    pub fn relay_closes(&self) -> Vec<u16> {
        self.gateways.lock().unwrap().closes.clone()
    }

    /// Its gateways' dials that opened.
    pub fn relay_dials(&self) -> u64 {
        self.gateways.lock().unwrap().dials
    }

    /// Approval answers its gateways took mid-turn: (who, the command).
    pub fn relay_approvals(&self) -> Vec<(String, String)> {
        self.gateways.lock().unwrap().approvals.clone()
    }

    /// Turns a Stop cut short.
    pub fn relay_interrupted(&self) -> u64 {
        self.gateways.lock().unwrap().interrupted
    }

    /// Sends every gateway away (its computer asleep) until its wake URL
    /// is fetched.
    pub fn relay_away(&self) {
        self.gateways.lock().unwrap().away = true;
    }

    /// Its computers' wakes, by their owners.
    pub fn wakes(&self) -> u64 {
        self.gateways.lock().unwrap().wakes
    }

    /// The `gateway.ping`s its Hermes have heard.
    pub fn pings(&self) -> u64 {
        self.state.lock().unwrap().pings
    }

    /// The admissions its endpoints have taken.
    pub fn admitted(&self) -> u64 {
        self.state.lock().unwrap().admitted
    }

    /// The admissions its endpoints have refused.
    pub fn refused(&self) -> u64 {
        self.state.lock().unwrap().refused
    }

    /// The runtime its endpoints run on, for a test's own iroh peer.
    /// What reached a screen's desktop, past its lease's filter, in order.
    pub fn screen_input(&self) -> Vec<Value> {
        self.screens().lock().unwrap().input.clone()
    }

    /// Input its filter dropped (a viewer not holding the lease).
    pub fn screen_dropped(&self) -> u64 {
        self.screens().lock().unwrap().dropped
    }

    /// Display sockets a ticket opened.
    pub fn screen_sockets(&self) -> u64 {
        self.screens().lock().unwrap().sockets
    }

    fn screens(&self) -> Arc<Mutex<Screens>> {
        Arc::clone(&self.state.lock().unwrap().screens)
    }

    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }
}

fn handle(state: &Arc<Mutex<State>>, n: &Arc<Node>, req: &Request) -> Response {
    if (req.method.as_str(), req.path.as_str()) == ("GET", "/v1/health") {
        return Response::json(200, &json!({ "ok": true, "node_key": n.node }));
    }
    if req.method == "PUT" && req.path.starts_with("/v1/grants/") {
        let wait = state.lock().unwrap().grant_ms;
        std::thread::sleep(std::time::Duration::from_millis(wait));
    }
    let url = format!("{}{}", n.base, req.path);
    let signer = match fragment_nip98::verify(req.header("authorization"), &req.method, &url, &req.body, now_s(), 60) {
        Ok(k) => k,
        Err(e) => return problem(401, "unauthorized", &e.to_string()),
    };
    let mut s = state.lock().unwrap();
    if !s.seen.insert(req.header("authorization").unwrap_or_default().to_string()) {
        return problem(401, "replay", "this signed request was already used");
    }
    let segments: Vec<&str> = req.path.trim_start_matches('/').split('/').collect();
    match (req.method.as_str(), segments.as_slice()) {
        ("PUT", ["v1", "grants", key]) => {
            if signer != n.grantor {
                return problem(403, "not_grantor", "only the node's grantors write grants");
            }
            let Ok(spec) = serde_json::from_slice::<Value>(&req.body) else { return problem(400, "invalid", "the body") };
            s.grants.insert(key.to_string(), spec.clone());
            Response::json(200, &json!({ "pubkey": key, "spec": spec, "granted_by": signer }))
        }
        ("PUT", ["v1", "computers", name]) => {
            let Some(grant) = s.grants.get(&signer).cloned() else { return problem(403, "no_grant", "this key holds no grant on this node") };
            let Ok(spec) = serde_json::from_slice::<Value>(&req.body) else { return problem(400, "invalid", "the body") };
            let serving_after = s.serving_after;
            match s.computers.get_mut(*name) {
                Some(c) if c.owner != signer => problem(409, "name_taken", "that name is taken"),
                Some(c) => {
                    c.spec = spec;
                    let v = view(n, name, c, serving_after);
                    drop(s);
                    relay(state, n, name);
                    Response::json(200, &v)
                }
                None => {
                    let mine = s.computers.values().filter(|c| c.owner == signer).count() as u64;
                    if mine >= grant["computers_max"].as_u64().unwrap_or(0) {
                        return problem(403, "over_grant", "this key's grant allows no more computers");
                    }
                    drop(s);
                    let (ep, hermes) = match run_computer(state, n, name) {
                        Ok(pair) => pair,
                        Err(e) => return problem(500, "engine", &e),
                    };
                    let c = Computer { owner: signer.clone(), spec, looks: 0, endpoint: ep.id().to_string(), port: hermes.port };
                    let v = view(n, name, &c, serving_after);
                    let mut s = state.lock().unwrap();
                    s.computers.insert(name.to_string(), c);
                    s.running.insert(name.to_string(), (ep, hermes));
                    drop(s);
                    relay(state, n, name);
                    Response::json(201, &v)
                }
            }
        }
        ("GET", ["v1", "computers", name]) => {
            let serving_after = s.serving_after;
            match s.computers.get_mut(*name) {
                Some(c) if c.owner == signer => {
                    c.looks += 1;
                    Response::json(200, &view(n, name, c, serving_after))
                }
                _ => problem(404, "not_found", "no such computer"),
            }
        }
        // its owner wakes it: its gateway, away, comes back
        ("POST", ["v1", "computers", name, "wake"]) => match s.computers.get(*name) {
            Some(c) if c.owner == signer => {
                let v = view(n, name, c, s.serving_after);
                let mut g = n.gateways.lock().unwrap();
                (g.away, g.wakes) = (false, g.wakes + 1);
                Response::json(202, &v)
            }
            _ => problem(404, "not_found", "no such computer"),
        },
        ("DELETE", ["v1", "computers", name]) => match s.computers.get(*name) {
            Some(c) if c.owner == signer => {
                // its disk goes with it, and Hermes' chats on it; its key answers no one
                s.computers.remove(*name);
                s.chats.remove(*name);
                if let Some((ep, _hermes)) = s.running.remove(*name) {
                    n.runtime.spawn(async move { ep.close().await });
                }
                Response::json(202, &json!({ "deleting": name }))
            }
            _ => problem(404, "not_found", "no such computer"),
        },
        _ => problem(404, "no_route", &format!("no route {} {}", req.method, req.path)),
    }
}

fn view(n: &Node, name: &str, c: &Computer, serving_after: u32) -> Value {
    let serving = c.looks >= serving_after;
    json!({
        "name": name,
        "owner": c.owner,
        "observed": { "state": if serving { "serving" } else { "starting" } },
        "pending": !serving,
        "url": format!("{}/c/{name}/", n.base),
        "iroh": { "endpoint": c.endpoint, "relay": n.relay.to_string() },
    })
}

/// Its Hermes' gateway, once its spec names a Relay: one a computer, for
/// its life, dialing with its spec's settings as they are at each dial.
fn relay(state: &Arc<Mutex<State>>, n: &Arc<Node>, name: &str) {
    let relay_env = |s: &State, name: &str| -> Option<(String, String, String)> {
        let env = &s.computers.get(name)?.spec["service"]["env"];
        let text = |k: &str| env[k].as_str().map(str::to_string);
        Some((text("GATEWAY_RELAY_URL")?, text("GATEWAY_RELAY_ID")?, text("GATEWAY_RELAY_SECRET")?))
    };
    let mut s = state.lock().unwrap();
    if relay_env(&s, name).is_none() || !s.gateways.insert(name.to_string()) {
        return;
    }
    drop(s);
    let (st, who) = (Arc::clone(state), name.to_string());
    relay_gateway::run(Arc::clone(&n.gateways), move || {
        let mut s = st.lock().unwrap();
        let env = relay_env(&s, &who);
        if env.is_none() {
            s.gateways.remove(&who);
        }
        env
    });
}

/// A new computer's Hermes (on a port of its own) and its endpoint.
fn run_computer(state: &Arc<Mutex<State>>, n: &Arc<Node>, name: &str) -> Result<(Endpoint, Server), String> {
    let (s, who) = (Arc::clone(state), name.to_string());
    let hermes = Server::start(0, Arc::new(move |req: &Request| native(&s, &who, req))).map_err(|e| e.to_string())?;
    let relay = n.relay.clone();
    let ep = n
        .runtime
        .block_on(Endpoint::builder(presets::Minimal).alpns(vec![ALPN.to_vec()]).relay_mode(RelayMode::Custom(relay.into())).bind())
        .map_err(|e| format!("binding an endpoint: {e}"))?;
    let (s, n2, who, port, accepting) = (Arc::clone(state), Arc::clone(n), name.to_string(), hermes.port, ep.clone());
    n.runtime.spawn(async move {
        // Bounded by the endpoint: None once it closes.
        while let Some(incoming) = accepting.accept().await {
            let (s, n2, who) = (Arc::clone(&s), Arc::clone(&n2), who.clone());
            tokio::spawn(async move {
                if let Ok(conn) = incoming.await {
                    connection(&s, &n2, &who, port, conn).await;
                }
            });
        }
    });
    Ok((ep, hermes))
}

/// A peer's connection: an admission first, then its streams piped to Hermes.
async fn connection(state: &Arc<Mutex<State>>, n: &Node, name: &str, port: u16, conn: Connection) {
    let peer = conn.remote_id().to_string();
    let mut until: i64 = 0;
    // Bounded by the connection.
    loop {
        let Ok((send, mut recv)) = conn.accept_bi().await else { return };
        let mut kind = [0u8; 1];
        if recv.read_exact(&mut kind).await.is_err() {
            continue;
        }
        match kind[0] {
            STREAM_ADMISSION => match admit(state, n, name, &peer, send, recv).await {
                Some(exp) => until = until.max(exp),
                None if until == 0 => {
                    conn.close(403u32.into(), b"not admitted");
                    return;
                }
                None => {}
            },
            STREAM_HTTP if now_s() < until => {
                tokio::spawn(async move {
                    let Ok(mut tcp) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else { return };
                    let mut stream = tokio::io::join(recv, send);
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut tcp).await;
                });
            }
            _ => {
                conn.close(403u32.into(), b"not admitted");
                return;
            }
        }
    }
}

/// An admission stream, as `sandcastled`'s iroh endpoint reads one: its end
/// when it admits.
async fn admit(state: &Arc<Mutex<State>>, n: &Node, name: &str, peer: &str, mut send: SendStream, mut recv: RecvStream) -> Option<i64> {
    let decided: Result<i64, String> = async {
        let len = recv.read_u16().await.map_err(|_| "a length".to_string())?;
        let mut raw = vec![0u8; usize::from(len)];
        recv.read_exact(&mut raw).await.map_err(|_| "the admission".to_string())?;
        let raw = String::from_utf8(raw).map_err(|_| "not text".to_string())?;
        let a = fragment_nip98::verify_admission(&raw, now_s(), 60).map_err(|e| e.to_string())?;
        let owner = state.lock().unwrap().computers.get(name).map(|c| c.owner.clone()).ok_or("no such computer")?;
        if a.signer != owner {
            return Err("the admission's signer admits no one here".into());
        }
        if a.peer != peer || a.computer != name || a.node != n.node {
            return Err("the admission is for another peer, computer, or node".into());
        }
        Ok(a.expires_at)
    }
    .await;
    // counted before it is answered: a test reads the count once it has the answer
    let (until, answer) = match decided {
        Ok(until) => {
            state.lock().unwrap().admitted += 1;
            (Some(until), json!({ "admitted": true, "until": until }))
        }
        Err(why) => {
            state.lock().unwrap().refused += 1;
            (None, json!({ "admitted": false, "reason": why }))
        }
    };
    let body = answer.to_string();
    let _ = send.write_u16(u16::try_from(body.len()).expect("a short answer")).await;
    let _ = send.write_all(body.as_bytes()).await;
    let _ = send.finish();
    if until.is_none() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send.stopped()).await;
    }
    until
}

/// Hermes in loopback mode (its DNS-rebinding guard, its session token).
fn native(state: &Arc<Mutex<State>>, name: &str, req: &Request) -> Response {
    let host = req.header("host").unwrap_or("");
    let hostname = host.rsplit_once(':').map_or(host, |(h, p)| if p.bytes().all(|b| b.is_ascii_digit()) { h } else { host });
    if !matches!(hostname, "127.0.0.1" | "localhost" | "[::1]" | "::1") {
        return problem(400, "invalid_host", "Invalid Host header. Dashboard requests must use the bound hostname.");
    }
    let path = req.path.trim_start_matches('/');
    let token = {
        let s = state.lock().unwrap();
        let Some(c) = s.computers.get(name) else { return problem(404, "not_found", "No such computer.") };
        c.spec["service"]["env"]["HERMES_DASHBOARD_SESSION_TOKEN"].as_str().unwrap_or("").to_string()
    };
    if (req.method.as_str(), path) == ("GET", "api/status") {
        return Response::json(200, &json!({ "gateway_running": true, "active_agents": 0 }));
    }
    if path == "api/display/ws" {
        // its ticket is its only gate (no session token), as Hermes'
        let Some(key) = req.header("sec-websocket-key") else { return problem(400, "bad_request", "a WebSocket upgrade") };
        let (screens, name) = (Arc::clone(&state.lock().unwrap().screens), name.to_string());
        let ticket = req.query.get("display_ticket").cloned().unwrap_or_default();
        return Response::websocket(key, None, move |stream| hermes_screen::display(stream, &screens, &name, &ticket));
    }
    if path == "api/ws" {
        let given = req.query.get("token").map(String::as_str).unwrap_or("");
        let key = req.header("sec-websocket-key").filter(|_| !token.is_empty() && given == token);
        let Some(key) = key else { return problem(403, "forbidden", "a socket needs the session token") };
        // as Hermes in loopback mode answers: no subprotocol named, whatever was offered
        let (state, name) = (Arc::clone(state), name.to_string());
        return Response::websocket(key, None, move |stream| gateway(stream, &state, &name));
    }
    let given = req.header("x-hermes-session-token").or_else(|| req.header("authorization").and_then(|h| h.strip_prefix("Bearer "))).unwrap_or("");
    if token.is_empty() || given != token {
        return problem(401, "unauthorized", "Unauthorized");
    }
    let s = state.lock().unwrap();
    match (req.method.as_str(), path) {
        ("GET", "api/sessions") => {
            let chats = s.chats.get(name).cloned().unwrap_or_default();
            let rows: Vec<Value> = chats.iter().rev().map(|(id, (title, m))| json!({ "id": id, "title": title, "source": "web", "message_count": m.len(), "preview": m.last().and_then(|x| x["content"].as_str()).unwrap_or("") })).collect();
            Response::json(200, &json!({ "sessions": rows, "total": chats.len(), "limit": 20, "offset": 0 }))
        }
        ("GET", p) if p.starts_with("api/sessions/") && p.ends_with("/messages") => {
            let id = &p["api/sessions/".len()..p.len() - "/messages".len()];
            match s.chats.get(name).and_then(|c| c.get(id)) {
                Some((_, m)) => Response::json(200, &json!({ "session_id": id, "messages": m })),
                None => problem(404, "not_found", "no such session"),
            }
        }
        _ => problem(404, "not_found", &format!("no route {} /{path}", req.method)),
    }
}

fn event(kind: &str, session: &str, payload: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": "event", "params": { "type": kind, "session_id": session, "payload": payload } })
}

/// Hermes' JSON-RPC gateway, as a chat client uses it: a turn echoes its
/// prompt (`echo: <text>`) in two deltas, and both are stored. A turn's
/// events go out from a thread of their own, as Hermes streams a turn while
/// it answers other requests (a page's pings).
fn gateway(mut stream: TcpStream, state: &Arc<Mutex<State>>, name: &str) {
    let Ok(writer) = stream.try_clone() else { return };
    let writer = Arc::new(Mutex::new(writer));
    let send = |v: &Value| write_frame(&mut writer.lock().unwrap(), 0x1, v.to_string().as_bytes());
    if send(&event("gateway.ready", "", json!({}))).is_err() {
        return;
    }
    // live handles (this connection's) → stored ids
    let mut live: HashMap<String, String> = HashMap::new();
    // the screen's viewer ids this connection minted
    let mut minted: HashSet<String> = HashSet::new();
    // Bounded by the connection: it ends when the client closes.
    while let Some((op, payload)) = read_frame(&mut stream) {
        match op {
            0x8 => return,
            0x9 => {
                let _ = write_frame(&mut writer.lock().unwrap(), 0xA, &payload);
                continue;
            }
            0x1 => {}
            _ => continue,
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&payload) else { continue };
        let (id, params) = (msg["id"].clone(), &msg["params"]);
        let mut after: Vec<Value> = vec![];
        let mut told: Option<Value> = None;
        let answer = {
            let mut s = state.lock().unwrap();
            match msg["method"].as_str() {
                Some(m) if m.starts_with("display.") => {
                    let screens = Arc::clone(&s.screens);
                    let mut screens = screens.lock().unwrap();
                    hermes_screen::rpc(&mut screens, name, &mut minted, m, params).map(|(result, event)| {
                        told = event;
                        result
                    })
                }
                Some("gateway.ping") => {
                    s.pings += 1;
                    Ok(json!({ "ok": true }))
                }
                Some("session.create") => {
                    s.next += 1;
                    let (stored, handle) = (format!("s{}", s.next), format!("live{}", s.next));
                    let title = params["title"].as_str().unwrap_or("").to_string();
                    s.chats.entry(name.to_string()).or_default().insert(stored.clone(), (title, vec![]));
                    live.insert(handle.clone(), stored.clone());
                    Ok(json!({ "session_id": handle, "stored_session_id": stored, "messages": [], "info": {} }))
                }
                Some("session.resume") => {
                    let stored = params["session_id"].as_str().unwrap_or("").to_string();
                    match s.chats.get(name).and_then(|c| c.get(&stored)).map(|(_, m)| m.clone()) {
                        Some(messages) => {
                            s.next += 1;
                            let handle = format!("live{}", s.next);
                            live.insert(handle.clone(), stored.clone());
                            Ok(json!({ "session_id": handle, "resumed": stored, "messages": messages }))
                        }
                        None => Err("no such session"),
                    }
                }
                Some("prompt.submit") => {
                    let handle = params["session_id"].as_str().unwrap_or("").to_string();
                    match (live.get(&handle).cloned(), params["text"].as_str()) {
                        (Some(stored), Some(text)) => {
                            let reply = format!("echo: {text}");
                            let (first, rest) = reply.split_at(reply.len() / 2);
                            if let Some((_, m)) = s.chats.get_mut(name).and_then(|c| c.get_mut(&stored)) {
                                m.push(json!({ "role": "user", "content": text }));
                                m.push(json!({ "role": "assistant", "content": reply }));
                            }
                            after = vec![
                                event("message.start", &handle, json!({})),
                                event("message.delta", &handle, json!({ "text": first })),
                                event("message.delta", &handle, json!({ "text": rest })),
                                event("message.complete", &handle, json!({ "text": reply, "status": "complete" })),
                            ];
                            Ok(json!({ "status": "streaming" }))
                        }
                        _ => Err("no such live session, or no text"),
                    }
                }
                Some("session.interrupt") => Ok(json!({ "ok": true })),
                _ => Err("method not found"),
            }
        };
        let reply = match answer {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32000, "message": message } }),
        };
        if send(&reply).is_err() || told.is_some_and(|e| send(&e).is_err()) {
            return;
        }
        if after.is_empty() {
            continue;
        }
        let (writer, wait) = (Arc::clone(&writer), std::time::Duration::from_millis(state.lock().unwrap().turn_ms));
        std::thread::spawn(move || {
            for (i, e) in after.iter().enumerate() {
                if i == 2 {
                    std::thread::sleep(wait);
                }
                if write_frame(&mut writer.lock().unwrap(), 0x1, e.to_string().as_bytes()).is_err() {
                    return;
                }
            }
        });
    }
}

/// One client frame (masked): (opcode, payload); None at the end.
pub fn read_frame(stream: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).ok()?;
    let len = match head[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            stream.read_exact(&mut b).ok()?;
            u64::from(u16::from_be_bytes(b))
        }
        127 => {
            let mut b = [0u8; 8];
            stream.read_exact(&mut b).ok()?;
            u64::from_be_bytes(b)
        }
        n => u64::from(n),
    };
    if len > 16 * 1024 * 1024 {
        return None;
    }
    let mut mask = [0u8; 4];
    if head[1] & 0x80 != 0 {
        stream.read_exact(&mut mask).ok()?;
    }
    let mut payload = vec![0u8; len as usize];
    stream.read_exact(&mut payload).ok()?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Some((head[0] & 0x0f, payload))
}

/// One server frame (unmasked).
pub fn write_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut f = vec![0x80 | opcode];
    match payload.len() {
        n if n < 126 => f.push(n as u8),
        n if n <= 0xffff => {
            f.push(126);
            f.extend((n as u16).to_be_bytes());
        }
        n => {
            f.push(127);
            f.extend((n as u64).to_be_bytes());
        }
    }
    f.extend(payload);
    stream.write_all(&f)
}
