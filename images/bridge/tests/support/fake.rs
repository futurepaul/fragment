//! A fake fragment API for the bridge's tests, in process: the routes the
//! bridge calls (src/api.rs), with the platform's semantics where the bridge
//! depends on them: channels with seq, idempotent posts by id (409 for
//! another body), drafts fanned out on `__live`, `__live` paging from a
//! cursor, wake subscriptions, members, `GET /api/computer`, the keepalive
//! socket, blobs, and a fragment's files. Levers drop every socket (a
//! deploy), take the API down, and fail posts.
//!
//! A request acts as the agent its `x-fragment-agent` names (the intercept's
//! job, faked): one not assigned to the computer is refused.

#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::Message;

use fragment_bridge::net::{self, Body};

#[derive(Debug, Clone)]
pub struct Member {
    pub principal: String,
    pub role: String,
    pub kind: String,
    pub added_at: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Chan {
    pub post: Option<String>,
    pub records: Vec<Value>,
    pub ids: HashMap<String, (u64, Value)>,
}

#[derive(Debug, Clone, Default)]
pub struct Frag {
    /// What it is (`chat`, `agent`, `skills`, …), as its list says.
    pub kind: String,
    /// Its title, as its list says (an agent fragment's: its label,
    /// capitalized, as the shell titles one by the agent's name).
    pub title: String,
    pub members: Vec<Member>,
    pub channels: BTreeMap<String, Chan>,
    pub subscriptions: Vec<Value>,
    pub blobs: HashMap<String, (String, Bytes)>,
    pub files: BTreeMap<String, Bytes>,
    pub commits: Vec<Value>,
}

struct LiveSock {
    id: u64,
    fragment: String,
    principal: String,
    tx: mpsc::UnboundedSender<Message>,
    live: Vec<String>,
}

#[derive(Default)]
pub struct World {
    pub computer: Value,
    pub fragments: BTreeMap<String, Frag>,
    pub names: BTreeMap<String, String>,
    /// Drafts as they came: (fragment, principal, turn, text).
    pub drafts: Vec<(String, String, String, Option<String>)>,
    /// Keepalive sockets open now, and each open (true) and close (false).
    pub keepalive_open: u32,
    pub keepalive_log: Vec<bool>,
    /// Every request, as `METHOD path`.
    pub calls: Vec<String>,
    /// Every request as an agent: `METHOD path`, its query, the agent it
    /// names, and whether it carried an authorization of its own (the
    /// intercept signs; the guest never does).
    pub requests: Vec<(String, String, Option<String>, bool)>,
    /// Records and drafts in the order they happened: `record <fragment>
    /// <channel> <seq>`, `draft <fragment> <turn> text|null`.
    pub log: Vec<String>,
    /// While down every request is 503 and every upgrade refused.
    pub down: bool,
    pub fail_posts: u32,
    /// Each post to a `work` channel (a claim, among others) is answered
    /// this much later: a claim held in flight.
    pub work_post_delay_ms: u64,
    /// Records per `__live` page (the platform's is 1000).
    pub page: usize,
    live: Vec<LiveSock>,
    keepalives: Vec<(u64, mpsc::UnboundedSender<Message>)>,
    next_sock: u64,
    clock: i64,
}

impl World {
    fn agent_identity(&self, agent: &str) -> Option<String> {
        self.computer["agents"].as_array()?.iter().find(|a| a["fragment"] == agent).and_then(|a| a["identity"].as_str()).map(str::to_string)
    }

    fn agent_owner(&self, agent: &str) -> Option<String> {
        self.computer["agents"].as_array()?.iter().find(|a| a["fragment"] == agent).and_then(|a| a["owner"].as_str()).map(str::to_string)
    }

    /// The role `me` holds in `f`, acting for `person` when it names one: its
    /// own membership, else that person's, held at most at editor (the
    /// platform's cap for an agent acting for someone).
    fn role_in(f: &Frag, me: &str, person: Option<&str>) -> Option<String> {
        let own = f.members.iter().find(|m| m.principal == me).map(|m| m.role.clone());
        own.or_else(|| person.and_then(|p| f.members.iter().find(|m| m.principal == p)).map(|_| "editor".to_string()))
    }

    fn now(&mut self) -> i64 {
        // A clock that always moves, so `at` orders records.
        let wall = i64::try_from(fragment_bridge::log::now_ms()).expect("ms");
        self.clock = (self.clock + 1).max(wall);
        self.clock
    }

    /// Appends a record and sends it to every socket live on its channel.
    pub fn append(&mut self, fragment: &str, channel: &str, principal: &str, body: Value) -> Value {
        let at = self.now();
        let f = self.fragments.get_mut(fragment).expect("a fragment of the fake");
        let c = f.channels.get_mut(channel).expect("a channel of the fragment");
        let seq = c.records.len() as u64 + 1;
        let record = json!({ "channel": channel, "seq": seq, "at": at, "principal": principal, "kind": "message", "body": body });
        c.records.push(record.clone());
        self.log.push(format!("record {fragment} {channel} {seq}"));
        let mut frame = record.clone();
        frame["type"] = json!("record");
        for s in self.live.iter().filter(|s| s.fragment == fragment && s.live.iter().any(|l| l == channel)) {
            let _ = s.tx.send(Message::text(frame.to_string()));
        }
        record
    }

    pub fn records(&self, fragment: &str, channel: &str) -> Vec<Value> {
        self.fragments.get(fragment).and_then(|f| f.channels.get(channel)).map(|c| c.records.clone()).unwrap_or_default()
    }

    /// The bodies on a channel whose `kind` is `kind` (`"reply"`: a body
    /// with a `turn` and no kind).
    pub fn bodies(&self, fragment: &str, channel: &str, kind: &str) -> Vec<Value> {
        self.records(fragment, channel)
            .into_iter()
            .map(|r| r["body"].clone())
            .filter(|b| match kind {
                "reply" => b.get("kind").is_none() && b.get("turn").is_some(),
                k => b["kind"] == k,
            })
            .collect()
    }

    pub fn live_sockets(&self) -> usize {
        self.live.len()
    }
}

pub struct Fake {
    pub addr: SocketAddr,
    pub world: Arc<Mutex<World>>,
    stop: watch::Sender<bool>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// An agent's own fragment: its owner paul, the agent an editor of it,
/// and its `tasks`.
fn agent_fragment(identity: &str) -> Frag {
    let label = identity.trim_start_matches("npub1");
    let title = label.get(..1).map(|c| c.to_uppercase() + &label[1..]).unwrap_or_default();
    let mut f = Frag { kind: "agent".into(), title, ..Frag::default() };
    f.members.push(Member { principal: "npub1paul".into(), role: "owner".into(), kind: "person".into(), added_at: 1 });
    f.members.push(Member { principal: identity.into(), role: "editor".into(), kind: "agent".into(), added_at: 2 });
    f.channels.insert("tasks".into(), Chan { post: Some("editor".into()), ..Chan::default() });
    f
}

/// A person, by name: `npub1<name>`, an npub in shape.
pub fn person(name: &str) -> String {
    format!("npub1{name}")
}

impl Fake {
    /// A fake with `agents` (`label`s: `<label>-k3x9`, `npub1<label>`, owned by
    /// paul) on one computer, listening on `bind` (`127.0.0.1:0` for tests;
    /// `0.0.0.0:0` for a container to reach).
    pub async fn start(bind: &str, agents: &[&str]) -> Fake {
        let agents: Vec<Value> = agents.iter().map(|l| json!({ "fragment": format!("{l}--k3x9"), "identity": format!("npub1{l}"), "name": l, "owner": "npub1paul" })).collect();
        let mut world = World { computer: json!({ "computer": "computer:00aa", "owner": "npub1paul", "image": "test", "agents": agents }), page: 1000, ..World::default() };
        world.names.insert("npub1paul".into(), "paul".into());
        world.names.insert("npub1skyler".into(), "skyler".into());
        for a in world.computer["agents"].as_array().cloned().unwrap_or_default() {
            world.fragments.insert(a["fragment"].as_str().unwrap().to_string(), agent_fragment(a["identity"].as_str().unwrap()));
        }
        let world = Arc::new(Mutex::new(world));
        let listener = tokio::net::TcpListener::bind(bind).await.expect("the fake listens");
        let addr = listener.local_addr().expect("an address");
        let (stop, stop_rx) = watch::channel(false);
        let w = world.clone();
        let handler = move |req: Request<Incoming>, _peer: SocketAddr| {
            let w = w.clone();
            async move { handle(req, w).await }
        };
        tokio::spawn(net::serve(listener, handler, stop_rx));
        Fake { addr, world, stop }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
        f(&mut self.world.lock().expect("the world"))
    }

    /// A chat `<label>-k3x9` with paul and `agents` (labels, in the order
    /// added: the first is the lead).
    pub fn chat(&self, label: &str, agents: &[&str]) -> String {
        self.chat_named(&format!("{label}--k3x9"), agents)
    }

    /// A chat named `name` in full, with paul and `agents`, as `chat`.
    pub fn chat_named(&self, name: &str, agents: &[&str]) -> String {
        let name = name.to_string();
        self.with(|w| {
            let mut f = Frag { kind: "chat".into(), ..Frag::default() };
            f.members.push(Member { principal: "npub1paul".into(), role: "owner".into(), kind: "person".into(), added_at: 1 });
            for (i, a) in agents.iter().enumerate() {
                f.members.push(Member { principal: format!("npub1{a}"), role: "editor".into(), kind: "agent".into(), added_at: 10 + i as i64 });
            }
            f.channels.insert("chat".into(), Chan { post: Some("viewer".into()), ..Chan::default() });
            f.channels.insert("work".into(), Chan { post: Some("editor".into()), ..Chan::default() });
            w.fragments.insert(name.clone(), f);
        });
        name
    }

    /// paul's skills fragment `<label>-k3x9` (the blessed `skills`
    /// template's: its agents reach it acting for paul), holding `files`.
    pub fn skills(&self, label: &str, files: &[(&str, &str)]) -> String {
        let name = format!("{label}--k3x9");
        self.with(|w| {
            let mut f = Frag { kind: "skills".into(), ..Frag::default() };
            f.members.push(Member { principal: "npub1paul".into(), role: "owner".into(), kind: "person".into(), added_at: 1 });
            for (path, text) in files {
                f.files.insert(path.to_string(), Bytes::from(text.to_string()));
            }
            w.fragments.insert(name.clone(), f);
        });
        name
    }

    /// Assigns the agent `label` to the computer while it runs (its
    /// fragment `<label>-k3x9`, identity `npub1<label>`), as its owner's
    /// `PUT /api/computers/{id}/agents/{fragment}` does.
    pub fn add_agent(&self, label: &str) {
        self.with(|w| {
            let (fragment, identity) = (format!("{label}--k3x9"), format!("npub1{label}"));
            w.fragments.insert(fragment.clone(), agent_fragment(&identity));
            let agent = json!({ "fragment": fragment, "identity": identity, "name": label, "owner": "npub1paul" });
            w.computer["agents"].as_array_mut().expect("the computer's agents").push(agent);
        });
    }

    /// Unassigns the agent `label` from the computer.
    pub fn remove_agent(&self, label: &str) {
        let fragment = format!("{label}--k3x9");
        self.with(|w| w.computer["agents"].as_array_mut().expect("the computer's agents").retain(|a| a["fragment"] != fragment.as_str()));
    }

    /// Adds the agent `label` to `fragment` as an editor, and posts `joined`
    /// on its own `tasks`, as the platform does for an agent added to a
    /// fragment (docs/computers.md).
    pub fn join(&self, fragment: &str, label: &str) {
        self.with(|w| {
            let at = w.now();
            w.fragments.get_mut(fragment).expect("a fragment").members.push(Member { principal: format!("npub1{label}"), role: "editor".into(), kind: "agent".into(), added_at: at });
            w.append(&format!("{label}--k3x9"), "tasks", "npub1paul", json!({ "kind": "joined", "fragment": fragment }));
        });
    }

    pub fn add_member(&self, fragment: &str, principal: &str, role: &str) {
        self.with(|w| {
            let at = w.now();
            w.fragments.get_mut(fragment).expect("a fragment").members.push(Member { principal: principal.into(), role: role.into(), kind: "person".into(), added_at: at });
        });
    }

    /// `principal` says `body` in `fragment`'s `chat`.
    pub fn say(&self, fragment: &str, principal: &str, body: Value) -> Value {
        self.with(|w| w.append(fragment, "chat", principal, body))
    }

    /// Closes every live and keepalive socket, as a deploy does.
    pub fn drop_sockets(&self) {
        self.with(|w| {
            for s in w.live.drain(..) {
                let _ = s.tx.send(Message::Close(None));
            }
            for (_, k) in w.keepalives.drain(..) {
                let _ = k.send(Message::Close(None));
            }
        });
    }

    /// Polls until `cond` holds, or panics naming `what` after `ms`.
    pub async fn until(&self, ms: u64, what: &str, cond: impl Fn(&World) -> bool) {
        let started = Instant::now();
        // bounded by `ms`
        loop {
            if cond(&self.world.lock().expect("the world")) {
                return;
            }
            if started.elapsed() > Duration::from_millis(ms) {
                let w = self.world.lock().expect("the world");
                let dump: Vec<String> = w.fragments.iter().flat_map(|(n, f)| f.channels.iter().map(move |(c, ch)| format!("{n}/{c}: {}", Value::Array(ch.records.iter().map(|r| r["body"].clone()).collect())))).collect();
                panic!("waited {ms} ms for {what}; the fake holds:\n{}\ndrafts: {:?}\nkeepalive: {:?}", dump.join("\n"), w.drafts, w.keepalive_log);
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }
}

fn answer(status: StatusCode, v: Value) -> Response<Body> {
    net::json_answer(status, &v)
}

fn refuse(status: StatusCode, message: &str) -> Response<Body> {
    let error = match status.as_u16() {
        401 => "unauthenticated",
        403 => "forbidden",
        404 => "not_found",
        409 => "conflict",
        429 => "rate_limited",
        _ => "unavailable",
    };
    net::refusal(status, error, message)
}

fn query(q: &str, key: &str) -> Vec<String> {
    q.split('&').filter_map(|kv| kv.split_once('=')).filter(|(k, _)| *k == key).map(|(_, v)| decode(v)).collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(x);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn handle(mut req: Request<Incoming>, world: Arc<Mutex<World>>) -> Response<Body> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let q = req.uri().query().unwrap_or("").to_string();
    let agent = req.headers().get("x-fragment-agent").and_then(|v| v.to_str().ok()).map(str::to_string);
    let delay_ms = {
        let mut w = world.lock().expect("the world");
        w.calls.push(format!("{method} {path}"));
        if w.down {
            return refuse(StatusCode::SERVICE_UNAVAILABLE, "the fake is down");
        }
        if method == Method::POST && path.ends_with("/channels/work") { w.work_post_delay_ms } else { 0 }
    };
    if delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    let parts: Vec<String> = path.trim_start_matches('/').split('/').map(str::to_string).collect();
    let p: Vec<&str> = parts.iter().map(String::as_str).collect();

    // The computer's own routes take no agent.
    match (method.clone(), p.as_slice()) {
        (Method::GET, ["api", "computer"]) => return answer(StatusCode::OK, world.lock().unwrap().computer.clone()),
        (Method::GET, ["api", "computer", "keepalive"]) => {
            let Some((response, socket)) = net::accept_ws(&mut req) else { return refuse(StatusCode::BAD_REQUEST, "a socket") };
            let world = world.clone();
            tokio::spawn(async move {
                let Some(ws) = socket.await else { return };
                let (tx, mut rx) = mpsc::unbounded_channel();
                let id = {
                    let mut w = world.lock().unwrap();
                    w.next_sock += 1;
                    let id = w.next_sock;
                    w.keepalives.push((id, tx));
                    w.keepalive_open += 1;
                    w.keepalive_log.push(true);
                    id
                };
                let (mut sink, mut stream) = ws.split();
                loop {
                    tokio::select! {
                        m = rx.recv() => match m {
                            Some(m) => { let close = matches!(m, Message::Close(_)); let _ = sink.send(m).await; if close { break; } }
                            None => break,
                        },
                        m = stream.next() => match m { Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break, _ => {} },
                    }
                }
                let mut w = world.lock().unwrap();
                w.keepalives.retain(|(i, _)| *i != id);
                w.keepalive_open -= 1;
                w.keepalive_log.push(false);
            });
            return response;
        }
        _ => {}
    }

    let Some(agent) = agent else { return refuse(StatusCode::UNAUTHORIZED, "a request acts as one of the computer's agents") };
    let Some(me) = world.lock().unwrap().agent_identity(&agent) else { return refuse(StatusCode::FORBIDDEN, "not an agent of this computer") };
    let signed = req.headers().contains_key("authorization");
    world.lock().unwrap().requests.push((format!("{method} {path}"), q.clone(), Some(agent.clone()), signed));
    // `for`: the agent acts for its owner (the platform's: only its owner)
    let acting_for = query(&q, "for").into_iter().next();
    if let Some(who) = &acting_for {
        if world.lock().unwrap().agent_owner(&agent).as_deref() != Some(who.as_str()) {
            return refuse(StatusCode::FORBIDDEN, "an agent acts for its owner");
        }
    }

    if p.first() == Some(&"f") && p.len() >= 3 {
        let fragment = p[1].to_string();
        let member = world.lock().unwrap().fragments.get(&fragment).is_some_and(|f| World::role_in(f, &me, acting_for.as_deref()).is_some());
        if !member {
            return refuse(StatusCode::FORBIDDEN, "not a member");
        }
        return match p[2] {
            "__live" => live(&mut req, world, fragment, me),
            "__people" => {
                let w = world.lock().unwrap();
                let mut profiles = serde_json::Map::new();
                for id in query(&q, "id") {
                    if let Some(u) = w.names.get(&id) {
                        profiles.insert(id.clone(), json!({ "kind": "person", "email": u }));
                    }
                }
                answer(StatusCode::OK, json!({ "profiles": profiles }))
            }
            _ => refuse(StatusCode::NOT_FOUND, "no such fragment route"),
        };
    }

    if p.as_slice() == ["api", "fragments"] {
        let w = world.lock().unwrap();
        // an agent's list `for` someone marks what that someone owns
        // (`owned`), as the platform's does: Hermes finds its owner's
        // skills fragment by it
        let owns = |f: &Frag| acting_for.as_deref().is_some_and(|p| f.members.iter().any(|m| m.principal == p && m.role == "owner"));
        let list: Vec<Value> = w
            .fragments
            .iter()
            .filter_map(|(n, f)| World::role_in(f, &me, acting_for.as_deref()).map(|role| json!({ "name": n, "role": role, "kind": f.kind, "owned": owns(f), "title": f.title })))
            .collect();
        return answer(StatusCode::OK, json!({ "fragments": list }));
    }
    if p.len() < 4 || p[0] != "api" || p[1] != "f" {
        return refuse(StatusCode::NOT_FOUND, "no such route");
    }
    let fragment = p[2].to_string();
    let body = req.into_body().collect().await.map(|b| b.to_bytes()).unwrap_or_default();
    let mut w = world.lock().unwrap();
    let Some(f) = w.fragments.get(&fragment) else { return refuse(StatusCode::NOT_FOUND, "no such fragment") };
    let Some(role) = World::role_in(f, &me, acting_for.as_deref()) else { return refuse(StatusCode::FORBIDDEN, "not a member") };
    match (method, &p[3..]) {
        (Method::GET, ["channels"]) => {
            let list: Vec<Value> = f.channels.iter().map(|(n, c)| json!({ "name": n, "read": "viewer", "post": c.post, "seq": c.records.len() })).collect();
            answer(StatusCode::OK, json!({ "channels": list }))
        }
        (Method::GET, ["members"]) => {
            let list: Vec<Value> = f.members.iter().map(|m| json!({ "principal": m.principal, "role": m.role, "kind": m.kind, "addedAt": m.added_at, "addedBy": "npub1paul" })).collect();
            answer(StatusCode::OK, json!({ "members": list }))
        }
        (Method::GET, ["subscriptions"]) => {
            let mine: Vec<Value> = f.subscriptions.iter().filter(|s| s["principal"] == me).cloned().collect();
            answer(StatusCode::OK, json!({ "subscriptions": mine }))
        }
        (Method::POST, ["subscriptions"]) => {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let channel = v["channel"].as_str().unwrap_or("").to_string();
            if v["wake"] != json!(true) || !f.channels.contains_key(&channel) {
                return refuse(StatusCode::BAD_REQUEST, "{channel, wake: true}");
            }
            let id = f.subscriptions.len() + 1;
            let s = json!({ "id": id, "principal": me, "channel": channel, "wake": true });
            w.fragments.get_mut(&fragment).unwrap().subscriptions.push(s.clone());
            answer(StatusCode::OK, s)
        }
        (Method::GET, ["channels", channel]) => {
            let Some(c) = f.channels.get(*channel) else { return refuse(StatusCode::NOT_FOUND, "no such channel") };
            let after: u64 = query(&q, "after").first().and_then(|a| a.parse().ok()).unwrap_or(0);
            let limit: usize = query(&q, "limit").first().and_then(|a| a.parse().ok()).unwrap_or(1000).min(1000);
            let page: Vec<Value> = c.records.iter().filter(|r| r["seq"].as_u64().unwrap_or(0) > after).take(limit).cloned().collect();
            let next = page.last().and_then(|r| r["seq"].as_u64()).unwrap_or(after);
            answer(StatusCode::OK, json!({ "channel": channel, "records": page, "next": next }))
        }
        (Method::POST, ["channels", channel]) => {
            let channel = channel.to_string();
            let Some(c) = f.channels.get(&channel) else { return refuse(StatusCode::NOT_FOUND, "no such channel") };
            if c.post.is_none() {
                return refuse(StatusCode::FORBIDDEN, "this channel takes no posts");
            }
            if c.post.as_deref() == Some("editor") && !matches!(role.as_str(), "editor" | "owner") {
                return refuse(StatusCode::FORBIDDEN, "editors post here");
            }
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let (Some(id), body) = (v["id"].as_str().map(str::to_string), v["body"].clone()) else { return refuse(StatusCode::BAD_REQUEST, "{id, body}") };
            if let Some((seq, old)) = c.ids.get(&id) {
                if *old == body {
                    let record = c.records[(*seq - 1) as usize].clone();
                    return answer(StatusCode::OK, json!({ "record": record, "replayed": true }));
                }
                return refuse(StatusCode::CONFLICT, "that id was posted with another body");
            }
            if w.fail_posts > 0 {
                w.fail_posts -= 1;
                return refuse(StatusCode::SERVICE_UNAVAILABLE, "a failed post, as asked");
            }
            let record = w.append(&fragment, &channel, &me, body.clone());
            let seq = record["seq"].as_u64().unwrap();
            w.fragments.get_mut(&fragment).unwrap().channels.get_mut(&channel).unwrap().ids.insert(id, (seq, body));
            answer(StatusCode::OK, json!({ "record": record, "replayed": false }))
        }
        (Method::PUT, ["channels", channel, "draft"]) => {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let turn = v["turn"].as_str().unwrap_or("").to_string();
            let text = v["text"].as_str().map(str::to_string);
            w.drafts.push((fragment.clone(), me.clone(), turn.clone(), text.clone()));
            w.log.push(format!("draft {fragment} {turn} {}", if text.is_some() { "text" } else { "null" }));
            let at = w.now();
            let frame = json!({ "type": "draft", "channel": channel, "principal": me, "turn": turn, "text": text, "at": at });
            for s in w.live.iter().filter(|s| s.fragment == fragment && s.live.iter().any(|l| l == *channel)) {
                let _ = s.tx.send(Message::text(frame.to_string()));
            }
            answer(StatusCode::OK, json!({ "ok": true }))
        }
        (Method::PUT, ["blobs", sha]) => {
            let got = fragment_bridge::records::hex(&<sha2::Sha256 as sha2::Digest>::digest(&body));
            if got != *sha {
                return refuse(StatusCode::BAD_REQUEST, "the bytes are not their hash");
            }
            let size = body.len();
            w.fragments.get_mut(&fragment).unwrap().blobs.insert(sha.to_string(), ("application/octet-stream".into(), body));
            answer(StatusCode::OK, json!({ "ok": true, "sha": sha, "size": size, "stored": true }))
        }
        (Method::GET, ["blobs", sha]) => match f.blobs.get(*sha) {
            Some((ty, bytes)) => net::respond(StatusCode::OK, ty, bytes.clone()),
            None => refuse(StatusCode::NOT_FOUND, "no such blob"),
        },
        (Method::GET, ["files"]) => {
            // a file's version moves with its bytes, as a commit's does
            let version = |b: &Bytes| format!("c{}", &fragment_bridge::records::hex(&<sha2::Sha256 as sha2::Digest>::digest(b))[..12]);
            let list: Vec<Value> = f.files.iter().map(|(p, b)| json!({ "path": p, "size": b.len(), "mode": "100644", "lastCommitSha": version(b), "machinery": false })).collect();
            answer(StatusCode::OK, json!({ "ref": "main", "files": list }))
        }
        (Method::GET, ["file"]) => {
            let path = query(&q, "path").into_iter().next().unwrap_or_default();
            match f.files.get(&path) {
                Some(b) => net::respond(StatusCode::OK, "application/octet-stream", b.clone()),
                None => refuse(StatusCode::NOT_FOUND, "no such file"),
            }
        }
        (Method::POST, ["files"]) => {
            let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let files = w.fragments.get_mut(&fragment).unwrap();
            for c in v["files"].as_array().cloned().unwrap_or_default() {
                let path = c["path"].as_str().unwrap_or("").to_string();
                if c["delete"] == json!(true) {
                    files.files.remove(&path);
                } else if let Some(t) = c["text"].as_str() {
                    files.files.insert(path, Bytes::from(t.to_string()));
                } else if let Some(b) = c["base64"].as_str() {
                    use base64::Engine;
                    files.files.insert(path, Bytes::from(base64::engine::general_purpose::STANDARD.decode(b).unwrap_or_default()));
                }
            }
            files.commits.push(v);
            answer(StatusCode::OK, json!({ "commit": format!("c{}", files.commits.len()) }))
        }
        _ => refuse(StatusCode::NOT_FOUND, "no such route"),
    }
}

/// `__live`: hello, subscribe paging from a cursor (`World::page` a page),
/// then live records and drafts; ping → pong.
fn live(req: &mut Request<Incoming>, world: Arc<Mutex<World>>, fragment: String, principal: String) -> Response<Body> {
    let Some((response, socket)) = net::accept_ws(req) else { return refuse(StatusCode::BAD_REQUEST, "a socket") };
    tokio::spawn(async move {
        let Some(ws) = socket.await else { return };
        let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
        let id = {
            let mut w = world.lock().unwrap();
            w.next_sock += 1;
            let id = w.next_sock;
            w.live.push(LiveSock { id, fragment: fragment.clone(), principal: principal.clone(), tx: tx.clone(), live: Vec::new() });
            id
        };
        let (mut sink, mut stream) = ws.split();
        let _ = sink.send(Message::text(json!({ "type": "hello", "id": format!("s{id}"), "principal": principal, "role": "editor", "presence": [] }).to_string())).await;
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(m) => { let close = matches!(m, Message::Close(_)); if sink.send(m).await.is_err() || close { break; } }
                    None => break,
                },
                m = stream.next() => {
                    let Some(Ok(Message::Text(t))) = m else {
                        if matches!(m, Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_)))) { continue; }
                        break;
                    };
                    let v: Value = serde_json::from_str(&t).unwrap_or(Value::Null);
                    match v["type"].as_str() {
                        Some("ping") => { let _ = tx.send(Message::text(json!({ "type": "pong" }).to_string())); }
                        Some("subscribe") => {
                            let channel = v["channel"].as_str().unwrap_or("").to_string();
                            let after = v["after"].as_u64().unwrap_or(0);
                            let mut w = world.lock().unwrap();
                            let page = w.page;
                            let records = w.records(&fragment, &channel);
                            let rest: Vec<Value> = records.into_iter().filter(|r| r["seq"].as_u64().unwrap_or(0) > after).collect();
                            let more = rest.len() > page;
                            let mut next = after;
                            for r in rest.iter().take(page) {
                                let mut frame = r.clone();
                                frame["type"] = json!("record");
                                next = r["seq"].as_u64().unwrap();
                                let _ = tx.send(Message::text(frame.to_string()));
                            }
                            if !more {
                                if let Some(s) = w.live.iter_mut().find(|s| s.id == id) {
                                    s.live.push(channel.clone());
                                }
                            }
                            let _ = tx.send(Message::text(json!({ "type": "subscribed", "channel": channel, "next": next, "more": more }).to_string()));
                        }
                        _ => { let _ = tx.send(Message::text(json!({ "type": "error", "message": "unknown frame" }).to_string())); }
                    }
                }
            }
        }
        world.lock().unwrap().live.retain(|s| s.id != id);
    });
    response
}
