//! Local wire fixture for the native app, never a production authentication service.
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path, Query, State,
    },
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::{get, post, put},
    Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::broadcast;

const PERSON: &str = "npub1localperson";
const AGENT: &str = "npub1localagent";
const MAX_RECORDS: usize = 2_000;
type Shared = Arc<Mutex<Fixture>>;
type Failure = (StatusCode, Json<Value>);

struct Room {
    summary: Value,
    records: Vec<Value>,
    posts: HashMap<String, (Value, Value)>,
    tx: broadcast::Sender<Value>,
    turns: HashMap<String, bool>,
    seq: HashMap<String, u64>,
}

impl Room {
    fn new(name: &str, kind: &str, title: &str) -> Self {
        Self {
            summary: json!({"name": name, "kind": kind, "title": title, "role": "owner", "agents": [AGENT]}),
            records: Vec::new(),
            posts: HashMap::new(),
            tx: broadcast::channel(256).0,
            turns: HashMap::new(),
            seq: HashMap::new(),
        }
    }
    fn append(&mut self, channel: &str, principal: &str, body: Value) -> Value {
        let seq = self.seq.entry(channel.into()).or_default();
        *seq += 1;
        let record = json!({"channel": channel, "seq": *seq, "at": now(), "principal": principal, "kind": "message", "body": body});
        self.records.push(record.clone());
        if self.records.len() > MAX_RECORDS {
            self.records.remove(0);
        }
        let mut frame = record.clone();
        frame["type"] = json!("record");
        let _ = self.tx.send(frame);
        if channel == "chat" {
            if let Some(text) = record["body"]["text"].as_str() {
                self.summary["preview"] = json!(text.chars().take(100).collect::<String>());
                self.summary["previewAt"] = json!(now());
            }
        }
        record
    }
}

struct Fixture {
    rooms: HashMap<String, Room>,
    counter: u64,
    base: String,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as u64
}
fn failure(code: StatusCode, message: &str) -> Failure {
    (code, Json(json!({"error": "fixture", "message": message})))
}

#[tokio::main]
async fn main() {
    let address = std::env::var("FRAGMENT_IOS_LISTEN").unwrap_or_else(|_| "127.0.0.1:8910".into());
    let base =
        std::env::var("FRAGMENT_IOS_BASE_URL").unwrap_or_else(|_| "http://localhost:8910".into());
    let state = Arc::new(Mutex::new(seed(base)));
    let app = Router::new()
        .route(
            "/health",
            get(|| async { Json(json!({"ok": true, "fixture": true})) }),
        )
        .route("/api/fragments", get(list).post(create))
        .route("/api/search", get(search))
        .route("/api/f/{name}/status", get(status))
        .route("/api/f/{name}/members/{identity}", put(member))
        .route(
            "/api/f/{name}/channels/chat",
            get(history).post(post_message),
        )
        .route("/f/{name}/__live", get(live))
        .route("/f/{name}", get(page))
        .route("/test/reset", post(reset))
        .layer(DefaultBodyLimit::max(65_536))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(&address)
        .await
        .expect("fixture port available");
    println!("Fragment iOS fixture listening at {address} (no real accounts or models)");
    axum::serve(listener, app).await.expect("fixture server");
}

fn seed(base: String) -> Fixture {
    let mut rooms = HashMap::new();
    rooms.insert("juniper".into(), Room::new("juniper", "agent", "Juniper"));
    for (name, title, question, answer) in [
        ("morning", "A calmer morning", "Help me make a little more room in my mornings.",
         "Start with one small thing.\n\n**Ten quiet minutes**, before your phone. Make coffee, open a window, and let the day arrive.\n\nYou don’t need a perfect routine. Just a little space that belongs to you."),
        ("idea", "A small idea", "I want to keep track of the things I read.",
         "A reading list sounds like a good place to start.\n\nI’ve put an example in your apps. Open **Chats → Reading list** to try it without leaving the conversation."),
        ("week", "This week, in focus", "Let’s make this week feel less busy.",
         "Let’s choose three things that matter, and give the rest a little less room.\n\n- One thing to finish\n- One thing to explore\n- One thing just for you\n\nWhat’s the first one?"),
    ] {
        let mut room = Room::new(name, "chat", title);
        room.append("chat", PERSON, json!({"text": question}));
        room.append("chat", AGENT, json!({"text": answer, "turn": format!("seed-{name}")}));
        rooms.insert(name.into(), room);
    }
    rooms.insert(
        "reading".into(),
        Room::new("reading", "app", "Reading list"),
    );
    // A full first page followed by an earlier page exercises real backwards paging.
    let mut archive = Room::new("long-chat", "chat", "A longer conversation");
    for n in 1..=80 {
        archive.append("chat", if n % 2 == 0 { AGENT } else { PERSON }, json!({"text": format!("Earlier thought {n}. A little room to think, one message at a time.")}));
    }
    archive.summary["previewAt"] = json!(1);
    rooms.insert("long-chat".into(), archive);
    Fixture {
        rooms,
        counter: 0,
        base,
    }
}

async fn reset(State(state): State<Shared>) -> Json<Value> {
    let mut fixture = state.lock().expect("fixture lock");
    *fixture = seed(fixture.base.clone());
    Json(json!({"ok": true}))
}
async fn list(State(state): State<Shared>) -> Json<Value> {
    let fixture = state.lock().expect("fixture lock");
    let mut rows: Vec<_> = fixture.rooms.values().map(|r| r.summary.clone()).collect();
    rows.sort_by_key(|r| r["name"].as_str().unwrap_or_default().to_owned());
    Json(json!({"fragments": rows}))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

// This fixture uses literal word matching. Production search remains the
// person's permission-filtered FTS projection, behind the same API shape.
async fn search(
    Query(query): Query<SearchQuery>,
    State(state): State<Shared>,
) -> Result<Json<Value>, Failure> {
    let words: Vec<_> = query.q.split_whitespace().map(str::to_lowercase).collect();
    if query.q.len() > 256 || words.len() > 8 {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Search accepts at most eight words and 256 bytes.",
        ));
    }
    if words.is_empty() {
        return Ok(Json(json!({"fragments": [], "messages": []})));
    }
    let fixture = state.lock().expect("fixture lock");
    let matches = |text: &str| {
        let lower = text.to_lowercase();
        words.iter().all(|word| lower.contains(word))
    };
    let mut fragments: Vec<_> = fixture
        .rooms
        .values()
        .filter(|room| {
            matches(&format!(
                "{} {}",
                room.summary["name"].as_str().unwrap_or(""),
                room.summary["title"].as_str().unwrap_or("")
            ))
        })
        .map(|room| room.summary.clone())
        .collect();
    fragments.sort_by_key(|fragment| fragment["name"].as_str().unwrap_or("").to_owned());
    fragments.truncate(20);
    let mut messages = Vec::new();
    for (name, room) in &fixture.rooms {
        for record in &room.records {
            if record["channel"] != "chat" {
                continue;
            }
            let kind = record["body"]["kind"].as_str();
            if kind.is_some() && kind != Some("message") {
                continue;
            }
            let Some(text) = record["body"]["text"].as_str() else {
                continue;
            };
            if !matches(text) {
                continue;
            }
            let mut end = text.len().min(300);
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            messages.push(json!({"fragment": name, "channel": "chat", "seq": record["seq"], "at": record["at"], "snippet": &text[..end]}));
        }
    }
    messages.sort_by_key(|hit| {
        std::cmp::Reverse((
            hit["at"].as_u64().unwrap_or(0),
            hit["seq"].as_u64().unwrap_or(0),
        ))
    });
    messages.truncate(50);
    Ok(Json(json!({"fragments": fragments, "messages": messages})))
}
async fn create(
    State(state): State<Shared>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, Failure> {
    if body["template"] != "chat" {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Only chat creation is supported.",
        ));
    }
    let mut fixture = state.lock().expect("fixture lock");
    if fixture.rooms.len() >= 100 {
        return Err(failure(
            StatusCode::TOO_MANY_REQUESTS,
            "Reset the fixture to create more chats.",
        ));
    }
    fixture.counter += 1;
    let name = format!("mobile-chat-{}", fixture.counter);
    fixture
        .rooms
        .insert(name.clone(), Room::new(&name, "chat", "New chat"));
    Ok(Json(json!({"name": name})))
}
async fn member(
    Path((name, identity)): Path<(String, String)>,
    State(state): State<Shared>,
) -> Result<Json<Value>, Failure> {
    let fixture = state.lock().expect("fixture lock");
    if !fixture.rooms.contains_key(&name) || identity != AGENT {
        return Err(failure(StatusCode::NOT_FOUND, "No such agent or chat."));
    }
    Ok(Json(json!({"ok": true})))
}
async fn status(
    Path(name): Path<String>,
    State(state): State<Shared>,
) -> Result<Json<Value>, Failure> {
    let fixture = state.lock().expect("fixture lock");
    if !fixture.rooms.contains_key(&name) {
        return Err(failure(StatusCode::NOT_FOUND, "No such fragment."));
    }
    Ok(Json(
        json!({"npub": AGENT, "urls": {"canonical": format!("{}/f/{name}", fixture.base)}}),
    ))
}
#[derive(Deserialize)]
struct History {
    before: Option<u64>,
    after: Option<u64>,
    limit: Option<usize>,
}
async fn history(
    Path(name): Path<String>,
    Query(query): Query<History>,
    State(state): State<Shared>,
) -> Result<Json<Value>, Failure> {
    let fixture = state.lock().expect("fixture lock");
    let room = fixture
        .rooms
        .get(&name)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "No such chat."))?;
    let limit = query.limit.unwrap_or(50);
    if limit == 0 || limit > 1_000 || (query.before.is_some() && query.after.is_some()) {
        return Err(failure(
            StatusCode::BAD_REQUEST,
            "Invalid history cursor or limit.",
        ));
    }
    let mut records: Vec<_> = room
        .records
        .iter()
        .filter(|r| {
            r["channel"] == "chat"
                && query
                    .before
                    .is_none_or(|n| r["seq"].as_u64().unwrap_or(0) < n)
                && query
                    .after
                    .is_none_or(|n| r["seq"].as_u64().unwrap_or(0) > n)
        })
        .cloned()
        .collect();
    if query.before.is_some() {
        records = records.into_iter().rev().take(limit).collect();
        records.reverse();
    } else {
        records.truncate(limit);
    }
    let next = if query.before.is_some() {
        records.first()
    } else {
        records.last()
    }
    .and_then(|r| r["seq"].as_u64())
    .unwrap_or(0);
    Ok(Json(
        json!({"channel": "chat", "records": records, "next": next}),
    ))
}

#[derive(Deserialize)]
struct Post {
    id: String,
    body: Value,
}
async fn post_message(
    Path(name): Path<String>,
    State(state): State<Shared>,
    Json(post): Json<Post>,
) -> Result<Json<Value>, Failure> {
    let mut fixture = state.lock().expect("fixture lock");
    let room = fixture
        .rooms
        .get_mut(&name)
        .ok_or_else(|| failure(StatusCode::NOT_FOUND, "No such chat."))?;
    if post.id.is_empty() || post.id.len() > 128 {
        return Err(failure(StatusCode::BAD_REQUEST, "Invalid post id."));
    }
    if let Some((body, record)) = room.posts.get(&post.id) {
        if body != &post.body {
            return Err(failure(
                StatusCode::CONFLICT,
                "Post id already used for another body.",
            ));
        }
        return Ok(Json(json!({"record": record, "replayed": true})));
    }
    if room.posts.len() >= 1_000 {
        return Err(failure(
            StatusCode::TOO_MANY_REQUESTS,
            "Reset the fixture to send more messages.",
        ));
    }
    if post.body["text"]
        .as_str()
        .is_some_and(|text| text.len() > 32_768)
    {
        return Err(failure(StatusCode::PAYLOAD_TOO_LARGE, "Message too large."));
    }
    let record = room.append("chat", PERSON, post.body.clone());
    room.posts
        .insert(post.id.clone(), (post.body.clone(), record.clone()));
    if post.body["kind"] == "stop" {
        if let Some(turn) = post.body["turn"].as_str() {
            room.turns.insert(turn.into(), false);
        }
    } else if let Some(text) = post.body["text"].as_str() {
        let text = text.to_owned();
        let turn = format!("local-{}", post.id);
        room.turns.insert(turn.clone(), true);
        room.append(
            "work",
            AGENT,
            json!({"kind": "turn.start", "turn": turn, "asker": PERSON, "agent": AGENT}),
        );
        if room.summary["title"] == "New chat" {
            room.summary["title"] = json!(text.chars().take(48).collect::<String>());
        }
        tokio::spawn(reply(state.clone(), name, turn, text.clone()));
        // Deliberately lose the acknowledgement after commit. Retrying must
        // return that same record and never start a second turn.
        if text.contains("/fail-once") {
            return Err(failure(
                StatusCode::SERVICE_UNAVAILABLE,
                "Fixture: acknowledgement lost. Tap retry.",
            ));
        }
    }
    Ok(Json(json!({"record": record, "replayed": false})))
}

async fn reply(state: Shared, name: String, turn: String, prompt: String) {
    tokio::time::sleep(Duration::from_millis(450)).await;
    let answer = if prompt.contains("code") || prompt.contains("markdown") {
        "Here’s a small starting point.\n\n## Keep it simple\n\nOne clear idea is enough to begin.\n\n```swift\nstruct Idea: Identifiable {\n    let id: UUID\n    var title: String\n}\n```\n\n- Make the first version small\n- Try it with real people\n- Keep what feels useful\n\n**What would you like to try first?**"
    } else {
        "Let’s make a little room for that.\n\n**Start small.** Pick the part that matters most to you, and we can work through it together. There’s no need to have it all figured out yet.\n\nIf it helps, we can turn the idea into a simple app — something that fits the way you think and grows with you.\n\nWhat would a useful first version look like?"
    };
    let mut text = String::new();
    let mut disconnected = false;
    for word in answer.split_inclusive(' ') {
        tokio::time::sleep(Duration::from_millis(65)).await;
        let mut fixture = state.lock().expect("fixture lock");
        let Some(room) = fixture.rooms.get_mut(&name) else {
            return;
        };
        if room.turns.get(&turn) != Some(&true) {
            room.append(
                "work",
                AGENT,
                json!({"kind": "turn.end", "turn": turn, "status": "stopped"}),
            );
            let _ = room.tx.send(json!({"type": "draft", "channel": "chat", "principal": AGENT, "turn": turn, "text": null, "at": now()}));
            room.turns.remove(&turn);
            return;
        }
        text.push_str(word);
        let _ = room.tx.send(json!({"type": "draft", "channel": "chat", "principal": AGENT, "turn": turn, "text": text, "at": now()}));
        if prompt.contains("/disconnect") && !disconnected && text.len() > 40 {
            let _ = room.tx.send(json!({"type": "fixture_disconnect"}));
            disconnected = true;
        }
    }
    let mut fixture = state.lock().expect("fixture lock");
    if let Some(room) = fixture.rooms.get_mut(&name) {
        room.append("chat", AGENT, json!({"text": text, "turn": turn}));
        room.append(
            "work",
            AGENT,
            json!({"kind": "turn.end", "turn": turn, "status": "ok"}),
        );
        room.turns.remove(&turn);
    }
}

async fn live(
    Path(name): Path<String>,
    State(state): State<Shared>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| serve_socket(socket, state, name))
}
async fn send(socket: &mut WebSocket, value: Value) -> bool {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .is_ok()
}
async fn serve_socket(mut socket: WebSocket, state: Shared, name: String) {
    let mut receiver = {
        let fixture = state.lock().expect("fixture lock");
        let Some(room) = fixture.rooms.get(&name) else {
            return;
        };
        room.tx.subscribe()
    };
    if !send(&mut socket, json!({"type": "hello", "id": "local", "principal": PERSON, "role": "owner", "platform": "http://localhost:8910", "presence": []})).await { return; }
    let mut subscriptions: HashMap<String, u64> = HashMap::new();
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { return };
                let bytes = match message { Message::Text(text) => text.as_bytes().to_vec(), Message::Binary(data) => data.to_vec(), Message::Close(_) => return, _ => continue };
                let Ok(frame) = serde_json::from_slice::<Value>(&bytes) else { return };
                if frame["type"] == "ping" { if !send(&mut socket, json!({"type": "pong"})).await { return; } }
                else if frame["type"] == "subscribe" {
                    let channel = frame["channel"].as_str().unwrap_or("").to_owned();
                    if !["chat", "work"].contains(&channel.as_str()) { return; }
                    let records = {
                        let fixture = state.lock().expect("fixture lock");
                        let Some(room) = fixture.rooms.get(&name) else { return };
                        let mut records: Vec<_> = room.records.iter().filter(|r| r["channel"] == channel).cloned().collect();
                        if let Some(after) = frame["after"].as_u64() { records.retain(|r| r["seq"].as_u64().unwrap_or(0) > after); }
                        else if let Some(last) = frame["last"].as_u64() { records = records.into_iter().rev().take(last.min(1_000) as usize).collect(); records.reverse(); }
                        else { return; }
                        records
                    };
                    let mut next = frame["after"].as_u64().unwrap_or(0);
                    for mut record in records {
                        next = record["seq"].as_u64().unwrap_or(next);
                        record["type"] = json!("record");
                        if !send(&mut socket, record).await { return; }
                    }
                    subscriptions.insert(channel.clone(), next);
                    if !send(&mut socket, json!({"type": "subscribed", "channel": channel, "next": next, "more": false})).await { return; }
                }
            }
            outgoing = receiver.recv() => {
                let Ok(frame) = outgoing else { return };
                if frame["type"] == "fixture_disconnect" { let _ = socket.send(Message::Close(None)).await; return; }
                let channel = frame["channel"].as_str().unwrap_or("");
                let Some(cursor) = subscriptions.get_mut(channel) else { continue };
                if frame["type"] == "record" {
                    let seq = frame["seq"].as_u64().unwrap_or(0);
                    if seq <= *cursor { continue; }
                    *cursor = seq;
                }
                if !send(&mut socket, frame).await { return; }
            }
        }
    }
}

async fn page(Path(name): Path<String>) -> Result<Html<&'static str>, StatusCode> {
    if name != "reading" {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Html(
        r#"<!doctype html><html><meta name="viewport" content="width=device-width, initial-scale=1"><meta name="color-scheme" content="light dark"><style>
    :root{font:17px/1.6 system-ui;color-scheme:light dark}body{margin:32px 24px;background:light-dark(#f7f6f3,#181818);color:light-dark(#171717,#ececec)}h1{font:36px Georgia}label{display:flex;gap:14px;padding:18px 0;border-bottom:1px solid #8884}input{width:20px;accent-color:#a88bea}small{color:#888}p{max-width:30em}
    </style><small>YOUR READING LIST</small><h1>A few good things.</h1><p>A place for the things you want to come back to.</p><label><input type="checkbox">A book that changes your mind</label><label><input type="checkbox">An essay worth sitting with</label><label><input type="checkbox">Something a friend sent you</label><p><small>Local preview · Example fragment</small></p></html>"#,
    ))
}
