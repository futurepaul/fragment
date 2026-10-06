//! The front door: TLS for the zone, in front of the cell and Dex, and the
//! root's download page over plain http.
//!
//! - **https** (443): the zone's certificate (`*.<zone>` and the apex), HTTP/1.1.
//!   `dex.<zone>` (each `routes` label) goes to its upstream, every other name
//!   under the zone to the cell; a name outside it is 421. Each request goes
//!   on with its own `Host`, `x-forwarded-proto: https` (the cell's router
//!   takes the scheme from it, as behind Fly's proxy), `x-forwarded-host`
//!   and `x-forwarded-for` set here, whatever a client sent. Bodies stream
//!   both ways unbuffered (SSE flows as it is written), and a WebSocket
//!   upgrade is spliced through once both ends switch (the shell's live
//!   channels, a computer's ports, a node's uplink).
//! - **http** (80): `/ca` serves the root (the profile, the DER and the
//!   PEM) and its fingerprint, on any host, so a device fetches it by the
//!   box's address before it trusts anything; `/` by address shows the same
//!   page; every other request under the zone moves to https.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderMap, HeaderName, HeaderValue, CONNECTION, HOST, UPGRADE};
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::{connect::HttpConnector, Client};
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::ca;

type Body = BoxBody<Bytes, hyper::Error>;

/// Connections at once, over both listeners; past it a new one is closed.
pub const CONNECTIONS_MAX: usize = 1024;
/// A request's head must arrive within this, and fit in this many bytes.
pub const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
pub const HEADER_BYTES_MAX: usize = 64 * 1024;
/// A TLS handshake must finish within this.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// An upstream (the cell, Dex) must accept a connection within this.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Headers that describe one hop, never passed on (RFC 9110, 7.6.1).
const HOP: [&str; 8] = ["connection", "keep-alive", "proxy-connection", "proxy-authenticate", "proxy-authorization", "te", "trailer", "transfer-encoding"];
/// Headers only this door may set: a client's are dropped.
const FORWARDED: [&str; 5] = ["forwarded", "x-forwarded-for", "x-forwarded-host", "x-forwarded-proto", "x-real-ip"];

/// What the door serves.
#[derive(Clone, Debug)]
pub struct DoorConfig {
    pub zone: String,
    /// The https port clients use, for the links and moves it makes.
    pub https_port: u16,
    /// The cell: every name under the zone without a route of its own.
    pub cell: SocketAddr,
    /// A label under the zone to its upstream (`dex` to Dex).
    pub routes: BTreeMap<String, SocketAddr>,
    /// Where `ca.rs` keeps the root's files.
    pub ca_dir: PathBuf,
}

pub struct Door {
    cfg: DoorConfig,
    client: Client<HttpConnector, Incoming>,
    connections: Arc<Semaphore>,
    /// The root's download page (made once: the root does not change).
    page: String,
}

impl Door {
    pub fn new(cfg: DoorConfig) -> Result<Arc<Door>, ca::CaError> {
        assert!(crate::valid_zone(&cfg.zone), "a valid zone");
        assert!(cfg.routes.keys().all(|l| crate::valid_label(l)), "routes are labels");
        let pem_path = cfg.ca_dir.join(ca::CA_PEM);
        let pem = std::fs::read_to_string(&pem_path).map_err(|error| ca::CaError::Io { path: pem_path.clone(), error })?;
        let der = ca::pem_der(&pem).ok_or_else(|| ca::CaError::Io { path: pem_path, error: std::io::Error::other("no CERTIFICATE in it") })?;
        let mut http = HttpConnector::new();
        http.set_connect_timeout(Some(CONNECT_TIMEOUT));
        http.set_nodelay(true);
        let client = Client::builder(TokioExecutor::new()).build(http);
        let page = ca_page(&cfg.zone, cfg.https_port, &fingerprint(&der));
        Ok(Arc::new(Door { cfg, client, connections: Arc::new(Semaphore::new(CONNECTIONS_MAX)), page }))
    }

    /// Serves https on `listener` until the task is dropped.
    pub async fn serve_https(self: Arc<Self>, listener: TcpListener, tls: Arc<rustls::ServerConfig>) {
        let acceptor = tokio_rustls::TlsAcceptor::from(tls);
        // bounded by the door's life: one connection per pass
        loop {
            let Ok((tcp, peer)) = listener.accept().await else { continue };
            let Ok(permit) = self.connections.clone().try_acquire_owned() else { continue };
            let (door, acceptor) = (self.clone(), acceptor.clone());
            tokio::spawn(async move {
                let _permit = permit;
                let _ = tcp.set_nodelay(true);
                let stream = match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                    Ok(Ok(s)) => s,
                    // a device that does not trust the root yet ends here: say so
                    Ok(Err(e)) => return eprintln!("door: TLS from {peer} failed: {e}"),
                    Err(_) => return,
                };
                let service = hyper::service::service_fn(move |req| {
                    let door = door.clone();
                    async move { Ok::<_, Infallible>(door.proxy(req, peer).await) }
                });
                let _ = http1().serve_connection(TokioIo::new(stream), service).with_upgrades().await;
            });
        }
    }

    /// Serves plain http on `listener` until the task is dropped.
    pub async fn serve_http(self: Arc<Self>, listener: TcpListener) {
        // bounded by the door's life: one connection per pass
        loop {
            let Ok((tcp, _)) = listener.accept().await else { continue };
            let Ok(permit) = self.connections.clone().try_acquire_owned() else { continue };
            let door = self.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let service = hyper::service::service_fn(move |req| {
                    let door = door.clone();
                    async move { Ok::<_, Infallible>(door.plain(req)) }
                });
                let _ = http1().serve_connection(TokioIo::new(tcp), service).await;
            });
        }
    }

    /// Where a host's requests go, and what it is called: a route's
    /// upstream (by its label), the cell, or none (a name outside the zone).
    pub fn upstream(&self, host: &str) -> Option<(SocketAddr, String)> {
        let name = crate::host_name(host);
        if !crate::in_zone(&name, &self.cfg.zone) {
            return None;
        }
        let label = name.strip_suffix(&self.cfg.zone).and_then(|l| l.strip_suffix('.'));
        match label.and_then(|l| self.cfg.routes.get_key_value(l)) {
            Some((label, addr)) => Some((*addr, label.clone())),
            None => Some((self.cfg.cell, "the cell".into())),
        }
    }

    async fn proxy(&self, mut req: Request<Incoming>, peer: SocketAddr) -> Response<Body> {
        let Some(host) = req.headers().get(HOST).and_then(|h| h.to_str().ok()).map(str::to_string) else {
            return text(StatusCode::BAD_REQUEST, "a request names its host");
        };
        let Some((upstream, what)) = self.upstream(&host) else {
            return text(StatusCode::MISDIRECTED_REQUEST, &format!("this door serves {} and the names under it", self.cfg.zone));
        };
        let upgrade = wants_upgrade(req.headers());
        let client_side = upgrade.is_some().then(|| hyper::upgrade::on(&mut req));
        let (mut parts, body) = req.into_parts();
        let path = parts.uri.path_and_query().map_or("/", |p| p.as_str()).to_string();
        parts.uri = match format!("http://{upstream}{path}").parse() {
            Ok(u) => u,
            Err(_) => return text(StatusCode::BAD_REQUEST, "a request's target is a path"),
        };
        parts.version = hyper::Version::HTTP_11;
        strip_hop(&mut parts.headers);
        for name in FORWARDED {
            parts.headers.remove(name);
        }
        let set = |h: &mut HeaderMap, name: &'static str, value: &str| {
            if let Ok(v) = HeaderValue::from_str(value) {
                h.insert(HeaderName::from_static(name), v);
            }
        };
        set(&mut parts.headers, "x-forwarded-proto", "https");
        set(&mut parts.headers, "x-forwarded-host", &host);
        set(&mut parts.headers, "x-forwarded-for", &peer.ip().to_string());
        match &upgrade {
            Some(protocol) => {
                parts.headers.insert(CONNECTION, HeaderValue::from_static("upgrade"));
                parts.headers.insert(UPGRADE, protocol.clone());
            }
            // an Upgrade that Connection does not name is no upgrade: it stays here
            None => {
                parts.headers.remove(UPGRADE);
            }
        }
        let method = parts.method.clone();
        let mut resp = match self.client.request(Request::from_parts(parts, body)).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("door: {method} {} {}: {what} ({upstream}) did not answer: {e}", crate::host_name(&host), path.split('?').next().unwrap_or("/"));
                return text(StatusCode::BAD_GATEWAY, &format!("{what} is not answering"));
            }
        };
        if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
            let Some(client_side) = client_side else { return text(StatusCode::BAD_GATEWAY, "the upstream switched protocols unasked") };
            let upstream_side = hyper::upgrade::on(&mut resp);
            tokio::spawn(async move {
                if let (Ok(a), Ok(b)) = tokio::join!(client_side, upstream_side) {
                    let _ = tokio::io::copy_bidirectional(&mut TokioIo::new(a), &mut TokioIo::new(b)).await;
                }
            });
            let (parts, _) = resp.into_parts();
            return Response::from_parts(parts, empty());
        }
        let (mut parts, body) = resp.into_parts();
        strip_hop(&mut parts.headers);
        parts.headers.remove(UPGRADE);
        Response::from_parts(parts, body.boxed())
    }

    /// Plain http: the root's page and files, else a move to https.
    fn plain(&self, req: Request<Incoming>) -> Response<Body> {
        let host = req.headers().get(HOST).and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
        let name = crate::host_name(&host);
        let path = req.uri().path();
        let by_address = name.parse::<std::net::IpAddr>().is_ok() || name.starts_with('[');
        let file = |file: &str, content_type: &'static str| match std::fs::read(self.cfg.ca_dir.join(file)) {
            Ok(bytes) => Response::builder()
                .header("content-type", content_type)
                .header("content-disposition", format!("attachment; filename=\"{file}\""))
                .header("cache-control", "no-store")
                .body(full(bytes))
                .expect("a well-formed answer"),
            Err(_) => text(StatusCode::NOT_FOUND, "the root is not made yet"),
        };
        match path {
            "/ca" | "/ca/" => html(&self.page),
            "/" if by_address => html(&self.page),
            p if p == format!("/ca/{}", ca::PROFILE) => file(ca::PROFILE, "application/x-apple-aspen-config"),
            p if p == format!("/ca/{}", ca::CA_CRT) => file(ca::CA_CRT, "application/x-x509-ca-cert"),
            p if p == format!("/ca/{}", ca::CA_PEM) => file(ca::CA_PEM, "application/x-pem-file"),
            _ if crate::in_zone(&name, &self.cfg.zone) => {
                let target = format!("{}{}", crate::https_origin(&name, self.cfg.https_port), req.uri().path_and_query().map_or("/", |p| p.as_str()));
                Response::builder().status(StatusCode::PERMANENT_REDIRECT).header("location", target).body(empty()).expect("a well-formed move")
            }
            _ => text(StatusCode::NOT_FOUND, "nothing here: the root is at /ca"),
        }
    }
}

fn http1() -> hyper::server::conn::http1::Builder {
    let mut b = hyper::server::conn::http1::Builder::new();
    b.timer(TokioTimer::new()).header_read_timeout(HEADER_TIMEOUT).max_buf_size(HEADER_BYTES_MAX);
    b
}

/// The protocol a request asks to switch to, when it asks (`Connection:
/// upgrade` with an `Upgrade`).
fn wants_upgrade(h: &HeaderMap) -> Option<HeaderValue> {
    let asks = h.get_all(CONNECTION).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).any(|t| t.trim().eq_ignore_ascii_case("upgrade"));
    asks.then(|| h.get(UPGRADE).cloned()).flatten()
}

/// Drops the hop-by-hop headers, and those `Connection` names.
fn strip_hop(h: &mut HeaderMap) {
    let named: Vec<String> =
        h.get_all(CONNECTION).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(',')).map(|t| t.trim().to_ascii_lowercase()).filter(|t| !t.is_empty()).collect();
    for name in HOP.iter().copied().map(str::to_string).chain(named) {
        h.remove(name.as_str());
    }
}

fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into()).map_err(|never| match never {}).boxed()
}

fn empty() -> Body {
    full(Bytes::new())
}

fn text(status: StatusCode, msg: &str) -> Response<Body> {
    Response::builder().status(status).header("content-type", "text/plain; charset=utf-8").body(full(format!("{msg}\n"))).expect("a well-formed answer")
}

fn html(page: &str) -> Response<Body> {
    Response::builder().header("content-type", "text/html; charset=utf-8").header("cache-control", "no-store").body(full(page.to_string())).expect("a well-formed answer")
}

fn fingerprint(der: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// The page a device opens first, by the box's address.
fn ca_page(zone: &str, https_port: u16, fingerprint: &str) -> String {
    let origin = crate::https_origin(zone, https_port);
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>fragment on {zone}</title>
<style>body{{font:16px/1.5 system-ui,sans-serif;max-width:36rem;margin:2rem auto;padding:0 1rem}}code{{word-break:break-all;font-size:.8rem}}a.b{{display:block;margin:.5rem 0;padding:.75rem 1rem;border:1px solid #888;border-radius:.5rem;text-decoration:none}}</style>
</head><body>
<h1>fragment on {zone}</h1>
<p>This network's intranet signs its own certificates. Trust its root once on each device, then open <a href="{origin}/">{origin}</a>.</p>
<a class="b" href="/ca/{profile}">iPhone, iPad or Mac: the profile</a>
<a class="b" href="/ca/{crt}">The certificate (.crt)</a>
<a class="b" href="/ca/{pem}">The certificate (PEM)</a>
<p>The root is valid for {zone} and the names under it only. Its SHA-256 fingerprint, to compare with the one the device shows:</p>
<p><code>{fingerprint}</code></p>
<p>On an iPhone: open the profile link and allow the download; then Settings, General, VPN &amp; Device Management, install it; then Settings, General, About, Certificate Trust Settings, and turn it on.</p>
</body></html>
"#,
        profile = ca::PROFILE,
        crt = ca::CA_CRT,
        pem = ca::CA_PEM,
    )
}

/// The rustls config for the zone's certificate (`ca::issue_leaf`'s
/// files): TLS 1.2 and 1.3, HTTP/1.1 by ALPN.
pub fn tls_config(cert_file: &Path, key_file: &Path) -> anyhow::Result<Arc<rustls::ServerConfig>> {
    use anyhow::Context as _;
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_file_iter(cert_file).with_context(|| format!("read {}", cert_file.display()))?.collect::<Result<_, _>>().with_context(|| format!("parse {}", cert_file.display()))?;
    anyhow::ensure!(!chain.is_empty(), "{} holds no certificate", cert_file.display());
    let key = PrivateKeyDer::from_pem_slice(ca::read_key(key_file)?.as_bytes()).with_context(|| format!("parse {}", key_file.display()))?;
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .context("the zone's certificate and key")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const ZONE: &str = "fragment.home.arpa";

    /// A stand-in upstream: `/echo` answers the request's headers as JSON
    /// lines; `/sse` writes one event, then waits for `/release` before the
    /// second; `/ws` echoes WebSocket messages; it tags each answer with
    /// `tag`.
    async fn upstream(tag: &'static str) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let gate = Arc::new(tokio::sync::Notify::new());
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let gate = gate.clone();
                tokio::spawn(async move {
                    let svc = hyper::service::service_fn(move |mut req: Request<Incoming>| {
                        let gate = gate.clone();
                        async move {
                            let resp: Response<Body> = match req.uri().path() {
                                "/echo" => {
                                    let mut lines: Vec<String> = req.headers().iter().map(|(k, v)| format!("{k}: {}", v.to_str().unwrap())).collect();
                                    lines.sort();
                                    lines.insert(0, format!("upstream: {tag}"));
                                    lines.insert(1, format!("target: {}", req.uri()));
                                    let body = req.into_body().collect().await.unwrap().to_bytes();
                                    lines.push(format!("body: {}", String::from_utf8_lossy(&body)));
                                    Response::new(full(lines.join("\n")))
                                }
                                "/release" => {
                                    gate.notify_waiters();
                                    Response::new(full("released"))
                                }
                                "/sse" => {
                                    let (tx, rx) = tokio::sync::mpsc::channel::<Result<hyper::body::Frame<Bytes>, hyper::Error>>(4);
                                    tokio::spawn(async move {
                                        let _ = tx.send(Ok(hyper::body::Frame::data(Bytes::from("data: one\n\n")))).await;
                                        gate.notified().await;
                                        let _ = tx.send(Ok(hyper::body::Frame::data(Bytes::from("data: two\n\n")))).await;
                                    });
                                    let body = http_body_util::StreamBody::new(tokio_stream(rx));
                                    Response::builder().header("content-type", "text/event-stream").body(BodyExt::boxed(body)).unwrap()
                                }
                                "/ws" => {
                                    let key = req.headers().get("sec-websocket-key").unwrap().as_bytes().to_vec();
                                    let on = hyper::upgrade::on(&mut req);
                                    tokio::spawn(async move {
                                        let io = TokioIo::new(on.await.unwrap());
                                        let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(io, tokio_tungstenite::tungstenite::protocol::Role::Server, None).await;
                                        while let Some(Ok(m)) = ws.next().await {
                                            if m.is_text() {
                                                ws.send(m).await.unwrap();
                                            }
                                        }
                                    });
                                    Response::builder()
                                        .status(StatusCode::SWITCHING_PROTOCOLS)
                                        .header("connection", "Upgrade")
                                        .header("upgrade", "websocket")
                                        .header("sec-websocket-accept", tokio_tungstenite::tungstenite::handshake::derive_accept_key(&key))
                                        .body(empty())
                                        .unwrap()
                                }
                                _ => text(StatusCode::NOT_FOUND, "no"),
                            };
                            Ok::<_, Infallible>(resp)
                        }
                    });
                    let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tcp), svc).with_upgrades().await;
                });
            }
        });
        addr
    }

    fn tokio_stream<T: Send + 'static>(mut rx: tokio::sync::mpsc::Receiver<T>) -> impl futures_util::Stream<Item = T> {
        futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx))
    }

    struct Fixture {
        https: SocketAddr,
        http: SocketAddr,
        ca: Vec<u8>,
        dir: PathBuf,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn door(cell: SocketAddr, dex: SocketAddr) -> Fixture {
        let mut b = [0u8; 6];
        ca::getrandom(&mut b);
        let dir = std::env::temp_dir().join(format!("fragment-lan-door-{}", hex::encode(b)));
        let root = ca::ensure_ca(&dir, ZONE, "fragment LAN CA (test)").unwrap();
        let leaf = ca::issue_leaf(&root).unwrap();
        let https = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (https_addr, http_addr) = (https.local_addr().unwrap(), http.local_addr().unwrap());
        let cfg = DoorConfig { zone: ZONE.into(), https_port: https_addr.port(), cell, routes: BTreeMap::from([("dex".to_string(), dex)]), ca_dir: dir.clone() };
        let d = Door::new(cfg).unwrap();
        tokio::spawn(d.clone().serve_https(https, tls_config(&leaf.cert_file, &leaf.key_file).unwrap()));
        tokio::spawn(d.serve_http(http));
        Fixture { https: https_addr, http: http_addr, ca: root.der.clone(), dir }
    }

    fn client_tls(ca: &[u8]) -> tokio_rustls::TlsConnector {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from(ca.to_vec())).unwrap();
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        tokio_rustls::TlsConnector::from(Arc::new(cfg))
    }

    /// One request over TLS to `name` (SNI and Host), as a device that
    /// trusts the root sends it; the answer's status and body.
    async fn get(f: &Fixture, name: &str, path: &str, extra: &[(&str, &str)]) -> (u16, String) {
        let tcp = tokio::net::TcpStream::connect(f.https).await.unwrap();
        let tls = client_tls(&f.ca).connect(rustls::pki_types::ServerName::try_from(name.to_string()).unwrap(), tcp).await.unwrap();
        let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await.unwrap();
        tokio::spawn(conn);
        let mut req = Request::get(path).header("host", format!("{name}:{}", f.https.port()));
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let resp = send.send_request(req.body(full("")).unwrap()).await.unwrap();
        let status = resp.status().as_u16();
        (status, String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned())
    }

    // Goal: a request reaches the cell with its own Host and the forwarded
    // headers set here (a client's own are dropped), dex.<zone> reaches Dex,
    // and a name outside the zone is refused.
    #[tokio::test]
    async fn requests_reach_their_upstream_marked_https() {
        let (cell, dex) = (upstream("cell").await, upstream("dex").await);
        let f = door(cell, dex).await;
        let (status, body) = get(&f, "todo--paul.fragment.home.arpa", "/echo?x=1", &[("x-forwarded-proto", "http"), ("x-forwarded-host", "evil.example"), ("forwarded", "for=1.2.3.4")]).await;
        assert_eq!(status, 200, "{body}");
        let port = f.https.port();
        assert!(body.contains("upstream: cell"), "{body}");
        assert!(body.contains("target: /echo?x=1"), "{body}");
        assert!(body.contains(&format!("host: todo--paul.fragment.home.arpa:{port}")), "{body}");
        assert!(body.contains("x-forwarded-proto: https"), "{body}");
        assert!(body.contains(&format!("x-forwarded-host: todo--paul.fragment.home.arpa:{port}")), "{body}");
        assert!(body.contains("x-forwarded-for: 127.0.0.1"), "{body}");
        assert!(!body.contains("evil.example") && !body.contains("forwarded: for="), "a client's forwarded headers are dropped: {body}");
        let (_, body) = get(&f, "dex.fragment.home.arpa", "/echo", &[]).await;
        assert!(body.contains("upstream: dex"), "{body}");
        let (_, body) = get(&f, "fragment.home.arpa", "/echo", &[]).await;
        assert!(body.contains("upstream: cell"), "{body}");
        // SNI for the zone, Host for another name: refused, not proxied
        let tcp = tokio::net::TcpStream::connect(f.https).await.unwrap();
        let tls = client_tls(&f.ca).connect(rustls::pki_types::ServerName::try_from("fragment.home.arpa").unwrap(), tcp).await.unwrap();
        let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await.unwrap();
        tokio::spawn(conn);
        let resp = send.send_request(Request::get("/echo").header("host", "bank.example").body(full("")).unwrap()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::MISDIRECTED_REQUEST);
    }

    // Goal: an event stream reaches the device as it is written: the first
    // event arrives while the upstream still holds the second.
    #[tokio::test]
    async fn event_streams_are_not_buffered() {
        let cell = upstream("cell").await;
        let f = door(cell, cell).await;
        let tcp = tokio::net::TcpStream::connect(f.https).await.unwrap();
        let tls = client_tls(&f.ca).connect(rustls::pki_types::ServerName::try_from("fragment.home.arpa").unwrap(), tcp).await.unwrap();
        let (mut send, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await.unwrap();
        tokio::spawn(conn);
        let resp = send.send_request(Request::get("/sse").header("host", "fragment.home.arpa").body(full("")).unwrap()).await.unwrap();
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        let mut body = resp.into_body();
        let first = tokio::time::timeout(Duration::from_secs(5), body.frame()).await.expect("the first event before the second is written").unwrap().unwrap();
        assert_eq!(first.into_data().unwrap(), "data: one\n\n");
        let (status, _) = get(&f, "fragment.home.arpa", "/release", &[]).await;
        assert_eq!(status, 200);
        let second = tokio::time::timeout(Duration::from_secs(5), body.frame()).await.unwrap().unwrap().unwrap();
        assert_eq!(second.into_data().unwrap(), "data: two\n\n");
    }

    // Goal: a WebSocket through the door (wss to the zone) is the
    // upstream's: messages go both ways.
    #[tokio::test]
    async fn websockets_pass_through() {
        let cell = upstream("cell").await;
        let f = door(cell, cell).await;
        let tcp = tokio::net::TcpStream::connect(f.https).await.unwrap();
        let tls = client_tls(&f.ca).connect(rustls::pki_types::ServerName::try_from("fragment.home.arpa").unwrap(), tcp).await.unwrap();
        let url = format!("wss://fragment.home.arpa:{}/ws", f.https.port());
        let (mut ws, resp) = tokio_tungstenite::client_async(url, tls).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SWITCHING_PROTOCOLS);
        for m in ["hello", "again"] {
            ws.send(tokio_tungstenite::tungstenite::Message::text(m)).await.unwrap();
            let back = tokio::time::timeout(Duration::from_secs(5), ws.next()).await.unwrap().unwrap().unwrap();
            assert_eq!(back.into_text().unwrap(), m);
        }
    }

    // Goal: a device that has not trusted the root fails the handshake, and
    // the door goes on serving the devices that have.
    #[tokio::test]
    async fn an_untrusting_device_fails_the_handshake() {
        let cell = upstream("cell").await;
        let f = door(cell, cell).await;
        let other = Fixture { https: f.https, http: f.http, ca: rcgen::generate_simple_self_signed(vec!["x.example".to_string()]).unwrap().cert.der().to_vec(), dir: std::env::temp_dir().join("fragment-lan-door-none") };
        let tcp = tokio::net::TcpStream::connect(f.https).await.unwrap();
        let refused = client_tls(&other.ca).connect(rustls::pki_types::ServerName::try_from("fragment.home.arpa").unwrap(), tcp).await;
        assert!(refused.is_err());
        assert_eq!(get(&f, "fragment.home.arpa", "/echo", &[]).await.0, 200);
    }

    // Goal: an upstream that is down is a 502 that says which, at once.
    #[tokio::test]
    async fn a_dead_upstream_is_502() {
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap();
        let f = door(dead, dead).await;
        let (status, body) = get(&f, "fragment.home.arpa", "/", &[]).await;
        assert_eq!((status, body.as_str()), (502, "the cell is not answering\n"));
        let (status, body) = get(&f, "dex.fragment.home.arpa", "/", &[]).await;
        assert_eq!((status, body.as_str()), (502, "dex is not answering\n"));
    }

    /// One plain-http request; the status, headers and body.
    async fn plain(f: &Fixture, host: &str, path: &str) -> (u16, HeaderMap, Vec<u8>) {
        let mut tcp = tokio::net::TcpStream::connect(f.http).await.unwrap();
        tcp.write_all(format!("GET {path} HTTP/1.1\r\nhost: {host}\r\nconnection: close\r\n\r\n").as_bytes()).await.unwrap();
        let mut raw = vec![];
        tcp.read_to_end(&mut raw).await.unwrap();
        let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&raw[..split]).into_owned();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        let mut headers = HeaderMap::new();
        for line in head.lines().skip(1) {
            let (k, v) = line.split_once(": ").unwrap();
            headers.append(HeaderName::from_bytes(k.as_bytes()).unwrap(), HeaderValue::from_str(v).unwrap());
        }
        (status, headers, raw[split + 4..].to_vec())
    }

    // Goal: over plain http, the box's address shows the root's page and
    // serves the profile as iOS takes it; the zone moves to https.
    #[tokio::test]
    async fn plain_http_serves_the_root_and_moves_the_rest() {
        let cell = upstream("cell").await;
        let f = door(cell, cell).await;
        let (status, _, page) = plain(&f, "192.168.50.7", "/").await;
        assert_eq!(status, 200);
        let page = String::from_utf8(page).unwrap();
        let fp = fingerprint(&f.ca);
        assert!(page.contains(&fp), "the page shows the root's fingerprint");
        let (status, headers, body) = plain(&f, "192.168.50.7", "/ca/fragment-ca.mobileconfig").await;
        assert_eq!(status, 200);
        assert_eq!(headers["content-type"], "application/x-apple-aspen-config");
        assert!(String::from_utf8(body).unwrap().contains("com.apple.security.root"));
        let (_, headers, der) = plain(&f, "fragment.home.arpa", "/ca/fragment-ca.crt").await;
        assert_eq!(headers["content-type"], "application/x-x509-ca-cert");
        assert_eq!(der, f.ca);
        let (status, headers, _) = plain(&f, "todo--paul.fragment.home.arpa", "/a/b?c=d").await;
        assert_eq!(status, 308);
        assert_eq!(headers["location"], format!("https://todo--paul.fragment.home.arpa:{}/a/b?c=d", f.https.port()));
        assert_eq!(plain(&f, "bank.example", "/").await.0, 404);
    }

    // Goal: hop-by-hop headers, and those a Connection header names, stay
    // on their hop; an upgrade is asked for only with Connection: upgrade.
    #[test]
    fn hop_headers_stay_on_their_hop() {
        let mut h = HeaderMap::new();
        for (k, v) in [("connection", "keep-alive, x-secret"), ("keep-alive", "timeout=5"), ("x-secret", "1"), ("te", "trailers"), ("cookie", "a=b"), ("upgrade", "websocket")] {
            h.insert(HeaderName::from_static(k), HeaderValue::from_static(v));
        }
        assert_eq!(wants_upgrade(&h), None, "Connection does not say upgrade");
        strip_hop(&mut h);
        let mut left: Vec<&str> = h.keys().map(|k| k.as_str()).collect();
        left.sort();
        assert_eq!(left, ["cookie", "upgrade"]);
        h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        assert_eq!(wants_upgrade(&h), Some(HeaderValue::from_static("websocket")));
    }
}
