//! `fragment hands run`'s loopback proxy: the Computer DO's intercepts,
//! played for this machine (docs/computers.md, "The fragment API" and
//! "Models"; docs/optchat.md, "A machine as hands"). The bridge, its goose
//! and goose's own `fragment` reach the platform only through it, as a
//! computer's guest reaches its egress, and it alone holds a credential:
//! the machine's key, paired to one agent (docs/api.md, "A machine's
//! keys").
//!
//! - `GET /api/computer` and its keepalive socket are answered here: the
//!   one agent is the paired one, with no credentials (no swap runs here);
//!   the keepalive is held open and means nothing (a machine is awake
//!   while `fragment hands run` runs).
//! - Every other `/api/…` and `/f/<fragment>/…` request names the paired
//!   agent (`x-fragment-agent`, refused otherwise) and goes to the
//!   platform signed by the machine's key (NIP-98, its body's hash in
//!   `payload`), that header dropped and the guest's auth with it. A GET's
//!   redirect (`/f/<name>/…` to the fragment's origin) is followed once,
//!   signed again for where it points (a signature binds its URL, so it is
//!   good nowhere else). A socket (`__live`) is upgraded at the platform
//!   first, then relayed as bytes. A wake subscription is answered here:
//!   nothing wakes a machine.
//! - `POST /v1/chat/completions` and `POST /v1/decide` are the platform's
//!   model route as the agent (its owner pays; their `hands` choice
//!   applies), streamed back.
//! - Each 401 the platform answers is told to `hands run`, which asks
//!   whether the key was unpaired.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::TryStreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, Limited, StreamBody};
use hyper::body::{Frame, Incoming};
use hyper::header::{HeaderMap, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::auth::Identity;

/// The header a guest names the agent it acts as with (docs/computers.md).
pub const AGENT_HEADER: &str = "x-fragment-agent";
/// A request body the proxy signs and sends on, at most: the computer's
/// egress's own bound (a blob's upload is read whole to be hashed).
pub const BODY_MAX_BYTES: usize = 32 * 1024 * 1024;
/// A model call's body, at most: the model route's.
pub const MODEL_BODY_MAX_BYTES: usize = fragment_core::models::MODEL_BODY_MAX_BYTES;
/// How long a connection to the platform may take to open (an answer may
/// take as long as it takes: a model's stream, a live socket).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Connections served at once (the bridge holds a few sockets, goose a
/// model call or two, its tools' CLI a call each).
pub const CONNECTIONS_MAX: usize = 256;
/// Headers a hop answers for itself, never passed on.
const HOP_HEADERS: [&str; 8] = ["connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te", "trailer", "transfer-encoding", "upgrade"];
/// A request's headers that go on to the platform (besides the signature).
const FORWARDED: [&str; 4] = ["content-type", "accept", "if-none-match", "range"];
/// A socket's upgrade headers, each way.
const SOCKET_ASKED: [&str; 4] = ["sec-websocket-key", "sec-websocket-version", "sec-websocket-protocol", "sec-websocket-extensions"];
const SOCKET_ANSWERED: [&str; 3] = ["sec-websocket-accept", "sec-websocket-protocol", "sec-websocket-extensions"];

/// Whom the proxy acts as: the paired agent, with the machine's key.
#[derive(Clone)]
pub struct Hands {
    /// The platform's base URL (no trailing slash).
    pub host: String,
    pub key: Identity,
    /// The agent fragment (`hands-<machine>.<username>`).
    pub agent: String,
    /// The agent's identity, and its owner's.
    pub identity: String,
    pub owner: String,
    /// The machine's name (its pairing's).
    pub machine: String,
}

impl Hands {
    /// `GET /api/computer`, as a computer's guest reads its own: the one
    /// agent it runs, holding no credentials.
    pub fn computer(&self) -> Value {
        let name = self.agent.split('.').next().unwrap_or(&self.agent);
        json!({
            "computer": format!("machine:{}", self.machine),
            "owner": self.owner,
            "image": "machine",
            "agents": [{ "fragment": self.agent, "identity": self.identity, "name": name, "owner": self.owner, "credentials": [] }],
            "credentialEnv": [],
        })
    }
}

/// What the proxy tells `hands run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Heard {
    /// The platform refused a signed request (401): the key may have been
    /// unpaired.
    Refused,
}

type Body = BoxBody<Bytes, std::io::Error>;
type Answer = Response<Body>;

fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed()
}

fn empty() -> Body {
    Empty::<Bytes>::new().map_err(|never| match never {}).boxed()
}

/// A refusal of the proxy's own, shaped as the platform's (`{error,
/// message}`), so the bridge reads it as it reads the platform's.
struct Refusal {
    status: u16,
    error: &'static str,
    message: String,
}

impl Refusal {
    fn new(status: u16, error: &'static str, message: impl Into<String>) -> Refusal {
        Refusal { status, error, message: message.into() }
    }

    fn answer(self) -> Answer {
        let body = json!({ "error": self.error, "message": self.message }).to_string();
        Response::builder().status(self.status).header("content-type", "application/json").body(full(body)).expect("a refusal builds")
    }
}

fn refused(status: u16, error: &'static str, message: &str) -> Answer {
    Refusal::new(status, error, message).answer()
}

fn answer_json(v: &Value) -> Answer {
    Response::builder().status(200).header("content-type", "application/json").body(full(v.to_string())).expect("an answer builds")
}

/// The proxy, serving on a loopback port of its own until dropped.
pub struct Proxy {
    pub addr: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Proxy {
    /// Serves `hands` on a free loopback port; each 401 the platform
    /// answers is said on `heard` (never waited for: a full channel drops
    /// it, and the next one says it again).
    pub async fn start(hands: Hands, heard: mpsc::Sender<Heard>) -> std::io::Result<Proxy> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        // redirects are the proxy's to follow (signed again), never reqwest's
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("the HTTP client builds (its TLS backend is compiled in)");
        let shared = Arc::new(Shared { hands, http, heard });
        let slots = Arc::new(tokio::sync::Semaphore::new(CONNECTIONS_MAX));
        let task = tokio::spawn(async move {
            // bounded by the process: one connection per pass, at most
            // CONNECTIONS_MAX served at once
            loop {
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(_) => {
                        // out of descriptors, say: wait rather than spin
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let Ok(slot) = slots.clone().acquire_owned().await else { return };
                let shared = shared.clone();
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |req| {
                        let shared = shared.clone();
                        async move { Ok::<_, Infallible>(shared.handle(req).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).with_upgrades().await;
                    drop(slot);
                });
            }
        });
        Ok(Proxy { addr, task })
    }

    /// Its address, as `FRAGMENT_API` and `FRAGMENT_MODEL` name it.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }
}

struct Shared {
    hands: Hands,
    http: reqwest::Client,
    heard: mpsc::Sender<Heard>,
}

fn is_socket(headers: &HeaderMap) -> bool {
    headers.get("upgrade").and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("websocket"))
}

/// A request's body, read whole, at most `max` bytes.
async fn body_of(req: Request<Incoming>, max: usize) -> Result<Bytes, Refusal> {
    match Limited::new(req.into_body(), max).collect().await {
        Ok(b) => Ok(b.to_bytes()),
        Err(e) if e.downcast_ref::<http_body_util::LengthLimitError>().is_some() => Err(Refusal::new(413, "too_large", format!("a request through this machine is at most {max} bytes"))),
        Err(e) => Err(Refusal::new(400, "invalid_request", format!("the request's body did not arrive: {e}"))),
    }
}

impl Shared {
    async fn handle(&self, req: Request<Incoming>) -> Answer {
        let path = req.uri().path().to_string();
        let query = req.uri().query().map(|q| format!("?{q}")).unwrap_or_default();
        let method = req.method().clone();
        match (&method, path.as_str()) {
            (&Method::GET, "/api/computer") => return answer_json(&self.hands.computer()),
            (&Method::GET, "/api/computer/keepalive") => return keepalive(req),
            (&Method::POST, "/v1/chat/completions") => return self.model(req, "/api/models/v1/chat/completions").await,
            (&Method::POST, "/v1/decide") => return self.model(req, "/api/models/v1/decide").await,
            _ => {}
        }
        if !(path.starts_with("/api/") || path.starts_with("/f/")) {
            return refused(404, "not_found", "the API is /api/… and a fragment's routes /f/<fragment>/…; the model is POST /v1/chat/completions and /v1/decide");
        }
        if let Err(no) = self.agent_named(req.headers()) {
            return no.answer();
        }
        let target = format!("{}{path}{query}", self.hands.host);
        if is_socket(req.headers()) {
            return self.socket(req, target).await;
        }
        let headers = req.headers().clone();
        let body = match method {
            Method::GET | Method::HEAD => Bytes::new(),
            _ => match body_of(req, BODY_MAX_BYTES).await {
                Ok(b) => b,
                Err(no) => return no.answer(),
            },
        };
        // a wake subscription is a computer's egress's to make: nothing
        // wakes a machine, which runs while `fragment hands run` does
        if method == Method::POST && path.ends_with("/subscriptions") {
            if let Ok(v) = serde_json::from_slice::<Value>(&body) {
                if v["wake"] == true {
                    return answer_json(&json!({ "id": "machine", "channel": v["channel"], "wake": true }));
                }
            }
        }
        self.forward(method, target, &headers, body).await
    }

    /// The request names the paired agent: 401 without a name, 403 naming
    /// another.
    fn agent_named(&self, headers: &HeaderMap) -> Result<(), Refusal> {
        match headers.get(AGENT_HEADER).and_then(|v| v.to_str().ok()) {
            None => Err(Refusal::new(401, "unauthenticated", "name the agent this acts as (x-fragment-agent)")),
            Some(a) if a == self.hands.agent => Ok(()),
            Some(a) => Err(Refusal::new(403, "forbidden", format!("this machine runs {} alone, not {a}", self.hands.agent))),
        }
    }

    fn signed(&self, method: &Method, url: &str, body: &[u8]) -> String {
        self.hands.key.nip98_header(method.as_str(), url, body)
    }

    fn told(&self, status: u16) {
        if status == 401 {
            let _ = self.heard.try_send(Heard::Refused);
        }
    }

    /// The request, signed, to the platform; its answer streamed back. A
    /// GET's or HEAD's redirect is followed once, signed again.
    async fn forward(&self, method: Method, target: String, headers: &HeaderMap, body: Bytes) -> Answer {
        let mut url = target;
        let mut hops = 0;
        // bounded: one redirect followed at most
        loop {
            let mut rb = self.http.request(method.clone(), &url).header("authorization", self.signed(&method, &url, &body));
            for k in FORWARDED {
                if let Some(v) = headers.get(k) {
                    rb = rb.header(k, v);
                }
            }
            if !body.is_empty() {
                rb = rb.body(body.clone());
            }
            let resp = match rb.send().await {
                Ok(r) => r,
                Err(e) => return refused(502, "upstream_failed", &format!("the platform did not answer: {e}")),
            };
            self.told(resp.status().as_u16());
            let redirect = resp.status().is_redirection() && matches!(method, Method::GET | Method::HEAD);
            let next = resp.headers().get("location").and_then(|l| l.to_str().ok()).and_then(|l| reqwest::Url::parse(&url).ok()?.join(l).ok());
            match next {
                Some(next) if redirect && hops == 0 && matches!(next.scheme(), "http" | "https") => {
                    url = next.to_string();
                    hops += 1;
                }
                _ => return streamed(resp),
            }
        }
    }

    /// A model call, as the agent: its owner pays (docs/computers.md,
    /// Models). A call naming no agent bills no one: 401.
    async fn model(&self, req: Request<Incoming>, path: &str) -> Answer {
        if req.headers().get(AGENT_HEADER).is_none() {
            return refused(401, "unauthenticated", "name the agent this call is for (x-fragment-agent): its owner pays for it");
        }
        if let Err(no) = self.agent_named(req.headers()) {
            return no.answer();
        }
        let headers = req.headers().clone();
        let body = match body_of(req, MODEL_BODY_MAX_BYTES).await {
            Ok(b) => b,
            Err(no) => return no.answer(),
        };
        let target = format!("{}{path}", self.hands.host);
        self.forward(Method::POST, target, &headers, body).await
    }

    /// A socket (`__live`): upgraded at the platform, signed, then the
    /// guest's upgraded too, and the two relayed as bytes until either
    /// closes. A platform that refuses the upgrade is answered as it
    /// answered.
    async fn socket(&self, mut req: Request<Incoming>, target: String) -> Answer {
        let mut rb = self.http.get(&target).header("authorization", self.signed(&Method::GET, &target, &[])).header("connection", "Upgrade").header("upgrade", "websocket");
        for k in SOCKET_ASKED {
            if let Some(v) = req.headers().get(k) {
                rb = rb.header(k, v);
            }
        }
        let resp = match rb.send().await {
            Ok(r) => r,
            Err(e) => return refused(502, "upstream_failed", &format!("the platform did not answer: {e}")),
        };
        if resp.status() != StatusCode::SWITCHING_PROTOCOLS {
            self.told(resp.status().as_u16());
            return streamed(resp);
        }
        let mut out = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS).header("connection", "Upgrade").header("upgrade", "websocket");
        for k in SOCKET_ANSWERED {
            if let Some(v) = resp.headers().get(k) {
                out = out.header(k, v);
            }
        }
        let guest = hyper::upgrade::on(&mut req);
        tokio::spawn(async move {
            let (Ok(mut platform), Ok(guest)) = (resp.upgrade().await, guest.await) else { return };
            let mut guest = TokioIo::new(guest);
            // bounded by the two sockets: either end closing ends it
            let _ = tokio::io::copy_bidirectional(&mut guest, &mut platform).await;
        });
        out.body(empty()).expect("a socket's answer builds")
    }
}

/// The keepalive socket, answered here: held open while the bridge holds
/// it, what it says read and let go.
fn keepalive(mut req: Request<Incoming>) -> Answer {
    if !is_socket(req.headers()) {
        return refused(400, "invalid_request", "the keepalive is a WebSocket");
    }
    let Some(key) = req.headers().get("sec-websocket-key").map(HeaderValue::as_bytes).map(<[u8]>::to_vec) else {
        return refused(400, "invalid_request", "a WebSocket's upgrade names its key");
    };
    let accept = tungstenite::handshake::derive_accept_key(&key);
    let on = hyper::upgrade::on(&mut req);
    tokio::spawn(async move {
        let Ok(up) = on.await else { return };
        let mut io = TokioIo::new(up);
        let mut buf = [0u8; 1024];
        // bounded by the socket: the bridge letting go closes it
        while let Ok(n) = io.read(&mut buf).await {
            if n == 0 {
                return;
            }
        }
    });
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-accept", accept)
        .body(empty())
        .expect("the keepalive's answer builds")
}

/// The platform's answer, as it came: its status, its headers less a
/// hop's, its body streamed (a model's answer as it is written).
fn streamed(resp: reqwest::Response) -> Answer {
    let mut out = Response::builder().status(resp.status());
    for (k, v) in resp.headers() {
        if !HOP_HEADERS.contains(&k.as_str()) {
            out = out.header(k, v);
        }
    }
    let body = StreamBody::new(resp.bytes_stream().map_ok(Frame::data).map_err(std::io::Error::other));
    out.body(BodyExt::boxed(body)).expect("an answer passed on builds")
}

#[cfg(test)]
mod tests;
