//! A sandcastle node (sandcastle/README.md), as much of it as the platform
//! touches, with a Hermes behind each computer's URL (docs/hermes-chat.md):
//!
//! - The API: `PUT /v1/grants/{pubkey}` (the grantor's only), `PUT|GET|
//!   DELETE /v1/computers/{name}` (a key with a grant; its own computers
//!   only). Every call is NIP-98 checked as the node checks it, the URL its
//!   own, and a replayed header is refused (the node's cache keys on the
//!   event id).
//! - A computer answers at `<url>/c/<name>/` like Hermes' web server: a
//!   native login (`auth/password-login`, against the spec's basic-auth
//!   env) giving a session cookie, `api/auth/me`, `api/sessions` and a
//!   session's `messages`, a single-use `api/auth/ws-ticket`, and `api/ws`,
//!   JSON-RPC (`session.create`, `session.resume`, `prompt.submit`, whose
//!   turn echoes the prompt in two deltas, `gateway.ping`). The router's
//!   CORS policy for the spec's `cors_origins` (any scheme: a dev fleet's
//!   pages are http).
//!
//! A computer serves after `serving_after` looks at it (a platform waits
//! for it), and never sleeps.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::http::{Request, Response, Server};

/// Seconds a native session lives, as a fleet sets Hermes' TTL.
const SESSION_TTL_S: i64 = 3600;

#[derive(Clone, Debug)]
pub struct Computer {
    pub owner: String,
    pub spec: Value,
    /// GETs of its view so far: it serves once they reach `serving_after`.
    pub looks: u32,
}

#[derive(Default)]
struct State {
    grants: BTreeMap<String, Value>,
    computers: BTreeMap<String, Computer>,
    seen: HashSet<String>,
    /// Native sessions (token → (computer, expires_at)) and tickets.
    sessions: HashMap<String, (String, i64)>,
    tickets: HashMap<String, String>,
    /// Stored chats per computer: id → (title, messages).
    chats: HashMap<String, BTreeMap<String, (String, Vec<Value>)>>,
    serving_after: u32,
    next: u64,
}

pub struct Sandcastle {
    pub url: String,
    pub grantor: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

fn now_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock after 1970").as_secs() as i64
}

fn problem(status: u16, code: &str, message: &str) -> Response {
    Response::json(status, &json!({ "code": code, "message": message }))
}

impl Sandcastle {
    /// A node whose grants only `grantor` (64 hex) may write.
    pub fn start(grantor: &str) -> std::io::Result<Sandcastle> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let url = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let state = Arc::new(Mutex::new(State { serving_after: 1, ..State::default() }));
        let (s, base, g) = (Arc::clone(&state), url.clone(), grantor.to_string());
        let server = Server::serve(listener, Arc::new(move |req: &Request| handle(&s, &base, &g, req)))?;
        Ok(Sandcastle { url, grantor: grantor.to_string(), state, _server: server })
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
}

fn handle(state: &Arc<Mutex<State>>, base: &str, grantor: &str, req: &Request) -> Response {
    if let Some(rest) = req.path.strip_prefix("/c/") {
        let (name, path) = rest.split_once('/').unwrap_or((rest, ""));
        return hermes(state, name, path, req);
    }
    let url = format!("{base}{}", req.path);
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
            if signer != grantor {
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
                    Response::json(200, &view(base, name, c, serving_after))
                }
                None => {
                    let mine = s.computers.values().filter(|c| c.owner == signer).count() as u64;
                    if mine >= grant["computers_max"].as_u64().unwrap_or(0) {
                        return problem(403, "over_grant", "this key's grant allows no more computers");
                    }
                    let c = Computer { owner: signer.clone(), spec, looks: 0 };
                    let v = view(base, name, &c, serving_after);
                    s.computers.insert(name.to_string(), c);
                    Response::json(201, &v)
                }
            }
        }
        ("GET", ["v1", "computers", name]) => {
            let serving_after = s.serving_after;
            match s.computers.get_mut(*name) {
                Some(c) if c.owner == signer => {
                    c.looks += 1;
                    Response::json(200, &view(base, name, c, serving_after))
                }
                _ => problem(404, "not_found", "no such computer"),
            }
        }
        ("DELETE", ["v1", "computers", name]) => match s.computers.get(*name) {
            Some(c) if c.owner == signer => {
                s.computers.remove(*name);
                Response::json(202, &json!({ "deleting": name }))
            }
            _ => problem(404, "not_found", "no such computer"),
        },
        _ => problem(404, "no_route", &format!("no route {} {}", req.method, req.path)),
    }
}

fn view(base: &str, name: &str, c: &Computer, serving_after: u32) -> Value {
    let serving = c.looks >= serving_after;
    json!({
        "name": name,
        "owner": c.owner,
        "observed": { "state": if serving { "serving" } else { "starting" } },
        "pending": !serving,
        "url": format!("{base}/c/{name}/"),
    })
}

/// The computer's router CORS, then Hermes.
fn hermes(state: &Arc<Mutex<State>>, name: &str, path: &str, req: &Request) -> Response {
    let origins: Vec<String> = {
        let s = state.lock().unwrap();
        let Some(c) = s.computers.get(name) else { return problem(404, "not_found", "No such computer.") };
        c.spec["cors_origins"].as_array().into_iter().flatten().filter_map(|o| o.as_str().map(str::to_string)).collect()
    };
    let origin = req.header("origin").filter(|o| origins.iter().any(|a| a == o)).map(str::to_string);
    if req.method == "OPTIONS" && req.header("access-control-request-method").is_some() {
        return match origin {
            Some(o) => Response::bytes(204, "text/plain", vec![])
                .with_header("access-control-allow-origin", &o)
                .with_header("access-control-allow-methods", "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS")
                .with_header("access-control-allow-headers", "Authorization, Content-Type")
                .with_header("vary", "Origin"),
            None => Response::bytes(403, "text/plain", b"This origin may not read this computer.\n".to_vec()),
        };
    }
    let resp = native(state, name, path, req);
    match (resp.upgrade.is_some(), origin) {
        (false, Some(o)) => resp.with_header("access-control-allow-origin", &o).with_header("vary", "Origin"),
        _ => resp,
    }
}

fn native(state: &Arc<Mutex<State>>, name: &str, path: &str, req: &Request) -> Response {
    let mut s = state.lock().unwrap();
    let env = s.computers[name].spec["service"]["env"].clone();
    if (req.method.as_str(), path) == ("POST", "auth/password-login") {
        let Ok(body) = serde_json::from_slice::<Value>(&req.body) else { return problem(400, "invalid", "the body") };
        let ok = body["provider"] == "basic" && body["username"] == env["HERMES_DASHBOARD_BASIC_AUTH_USERNAME"] && body["password"] == env["HERMES_DASHBOARD_BASIC_AUTH_PASSWORD"];
        if !ok || body["password"].as_str().is_none_or(str::is_empty) {
            return problem(401, "unauthorized", "Invalid credentials");
        }
        s.next += 1;
        let token = format!("hs{:016x}{}", s.next, "0".repeat(16));
        s.sessions.insert(token.clone(), (name.to_string(), now_s() + SESSION_TTL_S));
        return Response::json(200, &json!({ "ok": true })).with_header("set-cookie", &format!("hermes_session_at={token}; Path=/; HttpOnly; SameSite=Lax"));
    }
    if path == "api/ws" {
        drop(s);
        return websocket(state, name, req);
    }
    let bearer = req.header("authorization").and_then(|h| h.strip_prefix("Bearer ")).unwrap_or("");
    let authed = s.sessions.get(bearer).is_some_and(|(c, exp)| c == name && *exp > now_s());
    if !authed {
        return problem(401, "unauthorized", "Unauthorized");
    }
    let expires_at = s.sessions[bearer].1;
    match (req.method.as_str(), path) {
        ("GET", "api/auth/me") => Response::json(200, &json!({ "provider": "basic", "user": { "id": "owner" }, "expires_at": expires_at })),
        ("POST", "api/auth/ws-ticket") => {
            s.next += 1;
            let ticket = format!("t{:016x}{}", s.next, "1".repeat(16));
            s.tickets.insert(ticket.clone(), name.to_string());
            Response::json(200, &json!({ "ticket": ticket, "ttl_seconds": 30 }))
        }
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

/// `api/ws`: the ticket in the subprotocols (single use), then JSON-RPC.
fn websocket(state: &Arc<Mutex<State>>, name: &str, req: &Request) -> Response {
    let protocols: Vec<&str> = req.header("sec-websocket-protocol").unwrap_or("").split(',').map(str::trim).collect();
    let ticket = protocols.iter().find_map(|p| p.strip_prefix("hermes-gateway-ticket.")).unwrap_or("");
    let valid = state.lock().unwrap().tickets.remove(ticket).is_some_and(|c| c == name);
    let Some(key) = req.header("sec-websocket-key").filter(|_| valid && protocols.contains(&"hermes-gateway-v1")) else {
        return problem(403, "forbidden", "a socket needs a valid ticket");
    };
    let (state, name) = (Arc::clone(state), name.to_string());
    Response::websocket(key, Some("hermes-gateway-v1"), move |stream| gateway(stream, &state, &name))
}

fn event(kind: &str, session: &str, payload: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": "event", "params": { "type": kind, "session_id": session, "payload": payload } })
}

/// Hermes' JSON-RPC gateway, as a chat client uses it: a turn echoes its
/// prompt (`echo: <text>`) in two deltas, and both are stored.
fn gateway(mut stream: TcpStream, state: &Arc<Mutex<State>>, name: &str) {
    let send = |stream: &mut TcpStream, v: &Value| write_frame(stream, 0x1, v.to_string().as_bytes());
    if send(&mut stream, &event("gateway.ready", "", json!({}))).is_err() {
        return;
    }
    // live handles (this connection's) → stored ids
    let mut live: HashMap<String, String> = HashMap::new();
    // Bounded by the connection: it ends when the client closes.
    while let Some((op, payload)) = read_frame(&mut stream) {
        match op {
            0x8 => return,
            0x9 => {
                let _ = write_frame(&mut stream, 0xA, &payload);
                continue;
            }
            0x1 => {}
            _ => continue,
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&payload) else { continue };
        let (id, params) = (msg["id"].clone(), &msg["params"]);
        let mut after: Vec<Value> = vec![];
        let answer = {
            let mut s = state.lock().unwrap();
            match msg["method"].as_str() {
                Some("gateway.ping") => Ok(json!({ "ok": true })),
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
        if send(&mut stream, &reply).is_err() {
            return;
        }
        for e in &after {
            if send(&mut stream, e).is_err() {
                return;
            }
        }
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
