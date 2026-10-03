//! The plumbing every side of the bridge shares: HTTP/1.1 serving over
//! hyper, a WebSocket upgrade from a hyper request, a WebSocket client, and
//! the jittered backoff every reconnect waits (lesson 5). Plain TCP only:
//! the guest reaches the platform's hosts over HTTP, and the computer's
//! intercepts do the rest (docs/computers.md).

use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::{Role, WebSocketConfig};
use tokio_tungstenite::WebSocketStream;

use crate::limits;

pub type Body = Full<Bytes>;
pub type ServerWs = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;
pub type ClientWs = WebSocketStream<TcpStream>;

/// The frame bounds every socket keeps.
pub fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(limits::FRAME_MAX_BYTES)).max_frame_size(Some(limits::FRAME_MAX_BYTES))
}

/// A plain answer.
pub fn respond(status: StatusCode, content_type: &str, body: impl Into<Bytes>) -> Response<Body> {
    Response::builder().status(status).header("content-type", content_type).body(Full::new(body.into())).expect("a well-formed answer")
}

pub fn json_answer(status: StatusCode, value: &serde_json::Value) -> Response<Body> {
    respond(status, "application/json", value.to_string())
}

/// `{error, message}`, as the platform answers a refusal.
pub fn refusal(status: StatusCode, error: &str, message: &str) -> Response<Body> {
    json_answer(status, &serde_json::json!({ "error": error, "message": message }))
}

/// Whether a request asks to become a WebSocket.
pub fn is_upgrade(req: &Request<Incoming>) -> bool {
    let header_has = |name: &str, want: &str| req.headers().get(name).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|p| p.trim().eq_ignore_ascii_case(want)));
    header_has("connection", "upgrade") && header_has("upgrade", "websocket")
}

/// Accepts a WebSocket upgrade: the 101 to answer with, and the socket once
/// hyper hands the connection over. None when the request is no upgrade.
pub fn accept_ws(req: &mut Request<Incoming>) -> Option<(Response<Body>, impl Future<Output = Option<ServerWs>>)> {
    if !is_upgrade(req) {
        return None;
    }
    let key = req.headers().get("sec-websocket-key")?.as_bytes().to_vec();
    let accept = derive_accept_key(&key);
    let on_upgrade = hyper::upgrade::on(req);
    let response = Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("connection", "Upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-accept", accept)
        .body(Full::new(Bytes::new()))
        .expect("a well-formed 101");
    let socket = async move {
        match on_upgrade.await {
            Ok(upgraded) => Some(WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(ws_config())).await),
            Err(e) => {
                crate::ev!("ws.upgrade_failed", { "error": e.to_string() });
                None
            }
        }
    };
    Some((response, socket))
}

/// Serves HTTP/1.1 (with upgrades) on `listener` until `stop` is true.
pub async fn serve<F, Fut>(listener: TcpListener, handler: F, mut stop: tokio::sync::watch::Receiver<bool>)
where
    F: Fn(Request<Incoming>, SocketAddr) -> Fut + Clone + Send + Sync + 'static,
    Fut: Future<Output = Response<Body>> + Send + 'static,
{
    // bounded by the bridge's life: one accept per pass, ended by `stop`
    loop {
        let accepted = tokio::select! {
            a = listener.accept() => a,
            _ = stop.changed() => return,
        };
        let Ok((stream, peer)) = accepted else { continue };
        let handler = handler.clone();
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |req| {
                let handler = handler.clone();
                async move { Ok::<_, Infallible>(handler(req, peer).await) }
            });
            let conn = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), service).with_upgrades();
            let _ = conn.await;
        });
    }
}

/// A base URL's parts: `http://host[:port][/prefix]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base {
    pub host: String,
    pub port: u16,
    pub prefix: String,
}

impl Base {
    pub fn parse(url: &str) -> Result<Base, String> {
        let rest = url.strip_prefix("http://").ok_or_else(|| format!("{url}: only http:// is spoken here (the computer's intercepts add TLS)"))?;
        let (authority, prefix) = match rest.find('/') {
            Some(i) => (&rest[..i], rest[i..].trim_end_matches('/')),
            None => (rest, ""),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("{url}: a bad port"))?),
            None => (authority, 80),
        };
        if host.is_empty() {
            return Err(format!("{url}: no host"));
        }
        Ok(Base { host: host.to_string(), port, prefix: prefix.to_string() })
    }

    /// The `Host` header for it.
    pub fn authority(&self) -> String {
        if self.port == 80 {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn url(&self, path: &str) -> String {
        assert!(path.starts_with('/'), "paths are absolute: {path}");
        format!("http://{}{}{}", self.authority(), self.prefix, path)
    }
}

/// Opens a WebSocket client to `base` + `path`, with `headers`.
pub async fn connect_ws(base: &Base, path: &str, headers: &[(&str, String)]) -> Result<ClientWs, String> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let url = base.url(path).replacen("http://", "ws://", 1);
    let mut req = url.as_str().into_client_request().map_err(|e| format!("{url}: {e}"))?;
    for (name, value) in headers {
        let name = hyper::header::HeaderName::from_bytes(name.as_bytes()).map_err(|e| e.to_string())?;
        req.headers_mut().insert(name, value.parse().map_err(|_| "a header value that is not text".to_string())?);
    }
    let open = async {
        let tcp = TcpStream::connect((base.host.as_str(), base.port)).await.map_err(|e| format!("{url}: {e}"))?;
        let _ = tcp.set_nodelay(true);
        let (ws, _) = tokio_tungstenite::client_async_with_config(req, tcp, Some(ws_config())).await.map_err(|e| format!("{url}: {e}"))?;
        Ok::<_, String>(ws)
    };
    match tokio::time::timeout(Duration::from_millis(limits::WS_OPEN_TIMEOUT_MS), open).await {
        Ok(r) => r,
        Err(_) => Err(format!("{url}: no answer in {} ms", limits::WS_OPEN_TIMEOUT_MS)),
    }
}

/// Resolves once `stop` is true (or its sender is gone). The guard
/// `wait_for` hands back is dropped here, so a `select!` branch on this
/// holds nothing across its awaits.
pub async fn stopped(stop: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// A number from the clock and a counter, for jitter (not for secrets).
pub fn jitter_seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);
    let n = COUNTER.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0);
    let mut x = n ^ t ^ u64::from(std::process::id()).rotate_left(32);
    // xorshift64*
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

/// The waits between reconnects: doubling from `RECONNECT_MS_MIN` to
/// `RECONNECT_MS_MAX`, each a uniform 0.5–1.5 of the step (lesson 5).
#[derive(Debug, Clone)]
pub struct Backoff {
    step_ms: u64,
}

impl Default for Backoff {
    fn default() -> Backoff {
        Backoff { step_ms: limits::RECONNECT_MS_MIN }
    }
}

impl Backoff {
    /// The next wait, in ms.
    pub fn next_ms(&mut self) -> u64 {
        let step = self.step_ms;
        self.step_ms = (self.step_ms * 2).min(limits::RECONNECT_MS_MAX);
        let wait = step / 2 + jitter_seed() % (step + 1);
        assert!(wait >= step / 2 && wait <= step + step / 2, "a wait stays within its jitter");
        wait
    }

    /// A connection held: the next failure waits the least again.
    pub fn reset(&mut self) {
        self.step_ms = limits::RECONNECT_MS_MIN;
    }

    pub async fn wait(&mut self, mut stop: tokio::sync::watch::Receiver<bool>) -> bool {
        let ms = self.next_ms();
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => true,
            _ = crate::net::stopped(&mut stop) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bases_parse() {
        assert_eq!(Base::parse("http://api.fragment.internal").unwrap(), Base { host: "api.fragment.internal".into(), port: 80, prefix: String::new() });
        let b = Base::parse("http://127.0.0.1:8790/x/").unwrap();
        assert_eq!((b.port, b.prefix.as_str()), (8790, "/x"));
        assert_eq!(b.url("/api/computer"), "http://127.0.0.1:8790/x/api/computer");
        assert_eq!(Base::parse("http://h").unwrap().url("/a"), "http://h/a");
        for bad in ["https://h", "h:1", "http://", "http://h:port"] {
            assert!(Base::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn backoff_doubles_with_jitter() {
        let mut b = Backoff::default();
        let mut steps = Vec::new();
        for _ in 0..8 {
            steps.push(b.next_ms());
        }
        assert!(steps[0] >= 500 && steps[0] <= 1500, "{steps:?}");
        assert!(steps[7] >= limits::RECONNECT_MS_MAX / 2 && steps[7] <= limits::RECONNECT_MS_MAX * 3 / 2, "{steps:?}");
        b.reset();
        assert!(b.next_ms() <= 1500);
    }
}
