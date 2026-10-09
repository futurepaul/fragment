//! The proxy against a fake platform (fragment_fakes' HTTP server) that
//! checks each request's NIP-98 signature as the router does: the agent's
//! routes, its model calls (streamed), a redirect followed, a socket
//! relayed, and what it answers itself.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fragment_fakes::http::{Handler, Request as FakeRequest, Response as FakeResponse, Server};
use serde_json::{json, Value};

use super::*;

/// What the fake platform saw of one request.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    /// The key its signature names, when it verified for the URL it came to.
    signer: Option<String>,
    agent_header: bool,
    body: Vec<u8>,
}

fn now_s() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// The platform: every request recorded with its signature checked.
fn platform() -> (Server, Arc<Mutex<Vec<Seen>>>) {
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let base = Arc::new(Mutex::new(String::new()));
    let (log, at) = (seen.clone(), base.clone());
    let handler: Handler = Arc::new(move |req: &FakeRequest| {
        let query: Vec<String> = req.pairs.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let url = format!("{}{}{}", at.lock().unwrap(), req.path, if query.is_empty() { String::new() } else { format!("?{}", query.join("&")) });
        let signer = fragment_nip98::verify(req.header("authorization"), &req.method, &url, &req.body, now_s(), 60).ok();
        log.lock().unwrap().push(Seen { method: req.method.clone(), path: req.path.clone(), signer, agent_header: req.header("x-fragment-agent").is_some(), body: req.body.clone() });
        match req.path.as_str() {
            "/api/fragments" => FakeResponse::json(200, &json!({ "fragments": [] })),
            "/api/revoked" => FakeResponse::json(401, &json!({ "error": "unauthenticated", "message": "the key was revoked" })),
            "/api/f/x.paul/channels/chat" => FakeResponse::bytes(200, "application/json", req.body.clone()),
            "/f/x.paul/__people" => FakeResponse::bytes(308, "text/plain", vec![]).with_header("location", "/at-origin/__people?id=id:a"),
            "/at-origin/__people" => FakeResponse::json(200, &json!({ "profiles": {} })),
            "/api/models/v1/chat/completions" => {
                // two data lines, the second a while after the first: the
                // first reaches the guest before the answer ends
                let write = move |mut s: std::net::TcpStream| {
                    let chunk = |t: &str| format!("{:x}\r\n{t}\r\n", t.len());
                    let _ = s.write_all(chunk("data: {\"n\":1}\n\n").as_bytes());
                    let _ = s.flush();
                    std::thread::sleep(Duration::from_millis(600));
                    let _ = s.write_all(chunk("data: [DONE]\n\n").as_bytes());
                    let _ = s.write_all(b"0\r\n\r\n");
                };
                FakeResponse { status: 200, headers: vec![("content-type".into(), "text/event-stream".into()), ("transfer-encoding".into(), "chunked".into())], body: vec![], unanswered: false, upgrade: Some(Box::new(write)) }
            }
            "/f/x.paul/__live" => {
                let key = req.header("sec-websocket-key").unwrap_or("").to_string();
                let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
                let echo = move |s: std::net::TcpStream| {
                    let mut ws = tungstenite::WebSocket::from_raw_socket(s, tungstenite::protocol::Role::Server, None);
                    if let Ok(tungstenite::Message::Text(t)) = ws.read() {
                        let _ = ws.send(tungstenite::Message::text(format!("echo: {t}")));
                    }
                    let _ = ws.close(None);
                    let _ = ws.flush();
                };
                let headers = vec![("upgrade".into(), "websocket".into()), ("connection".into(), "Upgrade".into()), ("sec-websocket-accept".into(), accept)];
                FakeResponse { status: 101, headers, body: vec![], unanswered: false, upgrade: Some(Box::new(echo)) }
            }
            _ => FakeResponse::json(404, &json!({ "error": "not_found", "message": "no route" })),
        }
    });
    let server = Server::start(0, handler).unwrap();
    *base.lock().unwrap() = server.url.clone();
    (server, seen)
}

fn hands(host: &str) -> Hands {
    Hands { host: host.into(), key: Identity::generate(), agent: "hands-box.paul".into(), identity: "id:agent".into(), owner: "id:paul".into(), machine: "box".into() }
}

async fn start(host: &str) -> (Proxy, Hands, mpsc::Receiver<Heard>) {
    let h = hands(host);
    let (tx, rx) = mpsc::channel(4);
    (Proxy::start(h.clone(), tx).await.unwrap(), h, rx)
}

async fn json_of(r: reqwest::Response) -> Value {
    serde_json::from_slice(&r.bytes().await.unwrap()).unwrap()
}

fn agent(rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    rb.header(AGENT_HEADER, "hands-box.paul")
}

/// Goal: the agent's requests reach the platform signed by the machine's
/// key, for the URL they go to, the agent's header and the guest's own
/// auth dropped; a body's hash is bound. Method: a GET and a POST with a
/// body, through the proxy, as the fake platform verifies them.
#[tokio::test]
async fn the_agents_requests_go_signed_by_the_machine() {
    let (server, seen) = platform();
    let (proxy, h, _rx) = start(&server.url).await;
    let http = reqwest::Client::new();
    let r = agent(http.get(format!("{}/api/fragments", proxy.url())).header("authorization", "Bearer guest")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let r = agent(http.post(format!("{}/api/f/x.paul/channels/chat", proxy.url())).header("content-type", "application/json").body("{\"id\":\"1\",\"body\":\"hi\"}")).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), "{\"id\":\"1\",\"body\":\"hi\"}");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    for s in &seen {
        assert_eq!(s.signer.as_deref(), Some(h.key.pubkey_hex()), "signed by the machine's key, for its URL: {s:?}");
        assert!(!s.agent_header, "the agent's name stays here");
    }
    assert_eq!((seen[0].method.as_str(), seen[1].method.as_str()), ("GET", "POST"));
    assert_eq!(seen[1].body, b"{\"id\":\"1\",\"body\":\"hi\"}");
}

/// Goal: only the paired agent is acted as. Method: no agent named (401),
/// another agent (403), a path that is no route (404): none reaches the
/// platform.
#[tokio::test]
async fn only_the_paired_agent_is_acted_as() {
    let (server, seen) = platform();
    let (proxy, _h, _rx) = start(&server.url).await;
    let http = reqwest::Client::new();
    let r = http.get(format!("{}/api/fragments", proxy.url())).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(json_of(r).await["error"], "unauthenticated");
    let r = http.get(format!("{}/api/fragments", proxy.url())).header(AGENT_HEADER, "other.paul").send().await.unwrap();
    assert_eq!(r.status(), 403);
    let r = agent(http.get(format!("{}/etc/passwd", proxy.url()))).send().await.unwrap();
    assert_eq!(r.status(), 404);
    let r = http.post(format!("{}/v1/chat/completions", proxy.url())).body("{}").send().await.unwrap();
    assert_eq!(r.status(), 401, "a model call names whom it bills");
    assert!(seen.lock().unwrap().is_empty());
}

/// Goal: what a computer's DO answers itself is answered here: the
/// computer's view (the paired agent alone, no credentials), a wake
/// subscription (nothing wakes a machine), and the keepalive socket, held.
#[tokio::test]
async fn the_computers_own_routes_are_answered_here() {
    let (server, seen) = platform();
    let (proxy, h, _rx) = start(&server.url).await;
    let http = reqwest::Client::new();
    let v = json_of(http.get(format!("{}/api/computer", proxy.url())).send().await.unwrap()).await;
    assert_eq!(v["computer"], "machine:box");
    assert_eq!(v["agents"], json!([{ "fragment": "hands-box.paul", "identity": "id:agent", "name": "hands-box", "owner": "id:paul", "credentials": [] }]));
    assert_eq!(v, h.computer());
    let r = agent(http.post(format!("{}/api/f/x.paul/subscriptions", proxy.url())).body("{\"channel\":\"chat\",\"wake\":true}")).send().await.unwrap();
    assert_eq!(json_of(r).await, json!({ "id": "machine", "channel": "chat", "wake": true }));
    let url = format!("ws://{}/api/computer/keepalive", proxy.addr);
    let held = tokio::task::spawn_blocking(move || {
        let (mut ws, resp) = tungstenite::connect(url.as_str()).unwrap();
        let status = resp.status().as_u16();
        let _ = ws.close(None);
        status
    });
    assert_eq!(held.await.unwrap(), 101);
    assert!(seen.lock().unwrap().is_empty(), "none of it reaches the platform");
}

/// Goal: a model call is the platform's model route as the agent, and its
/// answer streams through as it is written. Method: an SSE answer whose
/// second line comes 600 ms after its first: the first is read before.
#[tokio::test]
async fn a_model_call_streams_through() {
    let (server, seen) = platform();
    let (proxy, h, _rx) = start(&server.url).await;
    let http = reqwest::Client::new();
    let t = std::time::Instant::now();
    let mut r = agent(http.post(format!("{}/v1/chat/completions", proxy.url())).header("content-type", "application/json").body("{\"model\":\"cheap\",\"stream\":true}")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "text/event-stream");
    let first = r.chunk().await.unwrap().unwrap();
    let at_first = t.elapsed();
    assert!(std::str::from_utf8(&first).unwrap().contains("\"n\":1"));
    let mut rest = Vec::new();
    while let Some(c) = r.chunk().await.unwrap() {
        rest.extend_from_slice(&c);
    }
    assert!(String::from_utf8(rest).unwrap().contains("[DONE]"));
    assert!(at_first < Duration::from_millis(500), "the first line before the answer ends: {at_first:?}");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0].path, "/api/models/v1/chat/completions");
    assert_eq!(seen[0].signer.as_deref(), Some(h.key.pubkey_hex()));
}

/// Goal: a GET to a fragment's route that the platform redirects to its
/// origin is followed once, signed again for where it points. Method:
/// `__people`, 308 to another path, verified there.
#[tokio::test]
async fn a_redirect_is_followed_signed_again() {
    let (server, seen) = platform();
    let (proxy, h, _rx) = start(&server.url).await;
    let r = agent(reqwest::Client::new().get(format!("{}/f/x.paul/__people?id=id:a", proxy.url()))).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(), ["/f/x.paul/__people", "/at-origin/__people"]);
    assert!(seen.iter().all(|s| s.signer.as_deref() == Some(h.key.pubkey_hex())), "{seen:?}");
}

/// Goal: a socket is upgraded at the platform, signed, then relayed both
/// ways. Method: `__live` through the proxy, a frame out and its echo back.
#[tokio::test]
async fn a_socket_is_relayed() {
    let (server, seen) = platform();
    let (proxy, h, _rx) = start(&server.url).await;
    let url = format!("ws://{}/f/x.paul/__live", proxy.addr);
    let echoed = tokio::task::spawn_blocking(move || {
        use tungstenite::client::IntoClientRequest;
        let mut req = url.as_str().into_client_request().unwrap();
        req.headers_mut().insert(AGENT_HEADER, "hands-box.paul".parse().unwrap());
        let (mut ws, _) = tungstenite::connect(req).unwrap();
        ws.send(tungstenite::Message::text("{\"type\":\"subscribe\"}")).unwrap();
        match ws.read().unwrap() {
            tungstenite::Message::Text(t) => t.to_string(),
            other => format!("{other:?}"),
        }
    });
    assert_eq!(echoed.await.unwrap(), "echo: {\"type\":\"subscribe\"}");
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen[0].path, "/f/x.paul/__live");
    assert_eq!(seen[0].signer.as_deref(), Some(h.key.pubkey_hex()));
}

/// Goal: a 401 from the platform is passed back as it came, and told to
/// `hands run` (the key may have been unpaired). Method: a route that
/// answers 401.
#[tokio::test]
async fn a_refusal_is_told() {
    let (server, _seen) = platform();
    let (proxy, _h, mut rx) = start(&server.url).await;
    let r = agent(reqwest::Client::new().get(format!("{}/api/revoked", proxy.url()))).send().await.unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(json_of(r).await["message"], "the key was revoked");
    assert_eq!(tokio::time::timeout(Duration::from_secs(2), rx.recv()).await.unwrap(), Some(Heard::Refused));
}
