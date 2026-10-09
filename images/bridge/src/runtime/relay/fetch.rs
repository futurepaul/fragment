//! A file Hermes sends by URL: Relay's `send_media` naming a URL of its
//! provider's rather than one of `/relay/media`'s (FAL's image generation
//! hands back `https://….fal.media/…`, and Hermes sends a reply's image
//! links so). Its connector contract takes a `source_url` that is a local
//! upload or "an already-public URL (passed through)": the bridge fetches
//! it, so it is the chat's blob as an uploaded file is (docs/bridge.md).
//!
//! Only what a page may show, from where the public may serve it: `https`;
//! a host whose every address is public (never this computer's own, its
//! network's, a link-local metadata service, nor the platform's
//! `*.internal` intercepts); at most `ATTACHMENT_MAX_BYTES`; a media type
//! the fragment serves as itself (`SERVED`); within `MEDIA_FETCH_MS`,
//! following at most `MEDIA_REDIRECTS_MAX` redirects, each checked as the
//! first. The connection goes to the address checked, so a name that
//! resolves again elsewhere changes nothing. `local` (the tests' fake
//! servers, `BRIDGE_MEDIA_LOCAL=allow`) lets plain `http` and local
//! addresses through.

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Empty, LengthLimitError, Limited};
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::rustls::{self, pki_types::ServerName, ClientConfig, RootCertStore};

use crate::limits;

/// The types a fragment serves a blob as (`__blob/<sha>`): passive media
/// alone (crates/core/src/blob.rs, `served_type`; the images are their own
/// workspace, so the list is said again here).
pub const SERVED: [&str; 12] = ["image/jpeg", "image/png", "image/webp", "image/gif", "video/mp4", "video/webm", "audio/mpeg", "audio/wav", "audio/webm", "audio/ogg", "audio/mp4", "application/pdf"];

/// A URL the bridge may fetch: its host, port and path (with its query).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub https: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    fn authority(&self) -> String {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        match (self.https, self.port) {
            (true, 443) | (false, 80) => host,
            (_, port) => format!("{host}:{port}"),
        }
    }

    /// The URL's file name: its path's last part, of letters, digits and
    /// `._-` (at most 100), or none.
    pub fn file_name(&self) -> Option<String> {
        let path = self.path.split(['?', '#']).next().unwrap_or("");
        let last = path.rsplit('/').next().unwrap_or("");
        let name: String = last.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')).take(100).collect();
        (!name.trim_matches('.').is_empty()).then_some(name)
    }
}

/// Why a URL's file is not fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    Url(&'static str),
    Address(String),
    Type(String),
    TooLarge,
    Status(u16),
    Redirects,
    TimedOut,
    Failed(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Url(why) => write!(f, "not a URL the bridge fetches: {why}"),
            Refused::Address(a) => write!(f, "{a} is not a public address"),
            Refused::Type(t) => write!(f, "{t:?} is not a type a chat shows"),
            Refused::TooLarge => write!(f, "over {} bytes", limits::ATTACHMENT_MAX_BYTES),
            Refused::Status(s) => write!(f, "answered {s}"),
            Refused::Redirects => write!(f, "more than {} redirects", limits::MEDIA_REDIRECTS_MAX),
            Refused::TimedOut => write!(f, "not fetched within {} ms", limits::MEDIA_FETCH_MS),
            Refused::Failed(e) => write!(f, "{e}"),
        }
    }
}

/// A file fetched: its bytes and the type it is served as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetched {
    pub bytes: Bytes,
    pub media_type: &'static str,
    pub url: Url,
}

/// `https://host[:port]/path`, no user, and with `local` `http` too.
pub fn parse(url: &str, local: bool) -> Result<Url, Refused> {
    let (https, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) if local => (false, rest),
        _ => return Err(Refused::Url("only https")),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(end);
    if authority.contains('@') {
        return Err(Refused::Url("no user in it"));
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(v6) => {
            let (host, after) = v6.split_once(']').ok_or(Refused::Url("an unclosed IPv6 address"))?;
            (host, after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        },
    };
    let port = match port {
        Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0).ok_or(Refused::Url("a port of 1 to 65535"))?,
        None if https => 443,
        None => 80,
    };
    let host = host.to_ascii_lowercase();
    let host_ok = !host.is_empty() && host.len() <= 253 && (host.parse::<IpAddr>().is_ok() || host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'));
    if !host_ok {
        return Err(Refused::Url("a host name or an address"));
    }
    let path = path.split('#').next().unwrap_or("");
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{path}") };
    if path.bytes().any(|b| b <= b' ' || b == 0x7f) || path.len() > 4096 {
        return Err(Refused::Url("a path of printable characters"));
    }
    Ok(Url { https, host, port, path })
}

/// Whether an address is one the public may serve from: not loopback,
/// private, link-local, shared (CGNAT), unspecified, multicast, broadcast,
/// documentation's, benchmarking's, or reserved; an IPv6 address that
/// carries an IPv4 one (mapped, NAT64) as that one.
pub fn public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, c, _] = v4.octets();
            let reserved = a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..128).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..32).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 198 && (18..20).contains(&b))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224;
            !reserved
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return public(IpAddr::V4(v4));
            }
            let s = v6.segments();
            if s[0] == 0x64 && s[1] == 0xff9b {
                let [.., c, d] = s;
                return public(IpAddr::V4(std::net::Ipv4Addr::new((c >> 8) as u8, c as u8, (d >> 8) as u8, d as u8)));
            }
            let reserved = v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || s[..6] == [0; 6];
            !reserved
        }
    }
}

/// The type a server's `content-type` is served as, if one of `SERVED`.
pub fn served(content_type: &str) -> Option<&'static str> {
    let essence = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    SERVED.into_iter().find(|t| *t == essence)
}

/// The URL's file, as `SERVED` allows, within the bounds above.
pub async fn fetch(url: &str, local: bool) -> Result<Fetched, Refused> {
    let within = tokio::time::timeout(Duration::from_millis(limits::MEDIA_FETCH_MS), follow(url, local));
    within.await.map_err(|_| Refused::TimedOut)?
}

async fn follow(url: &str, local: bool) -> Result<Fetched, Refused> {
    let mut url = parse(url, local)?;
    // bounded: a fetch and at most MEDIA_REDIRECTS_MAX more
    for _ in 0..=limits::MEDIA_REDIRECTS_MAX {
        let addr = resolve(&url, local).await?;
        let res = get(&url, addr).await?;
        let status = res.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let to = res.headers().get("location").and_then(|v| v.to_str().ok()).ok_or(Refused::Status(status))?;
            url = match to.strip_prefix('/').filter(|p| !p.starts_with('/')) {
                Some(path) => Url { path: format!("/{path}"), ..url },
                None => parse(to, local)?,
            };
            continue;
        }
        if status != 200 {
            return Err(Refused::Status(status));
        }
        let declared = res.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let media_type = served(&declared).ok_or_else(|| Refused::Type(declared.clone()))?;
        let max = usize::try_from(limits::ATTACHMENT_MAX_BYTES).expect("fits");
        let length = res.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        if length.is_some_and(|n| n > limits::ATTACHMENT_MAX_BYTES) {
            return Err(Refused::TooLarge);
        }
        let bytes = match Limited::new(res.into_body(), max).collect().await {
            Ok(b) => b.to_bytes(),
            Err(e) if e.downcast_ref::<LengthLimitError>().is_some() => return Err(Refused::TooLarge),
            Err(e) => return Err(Refused::Failed(format!("reading it: {e}"))),
        };
        if bytes.is_empty() {
            return Err(Refused::Failed("an empty file".into()));
        }
        return Ok(Fetched { bytes, media_type, url });
    }
    Err(Refused::Redirects)
}

/// The address to fetch `url` from: its host's, each of them public
/// (unless `local`), the first.
async fn resolve(url: &Url, local: bool) -> Result<SocketAddr, Refused> {
    let internal = url.host == "localhost" || url.host.ends_with(".localhost") || url.host.ends_with(".internal");
    if internal && !local {
        return Err(Refused::Address(url.host.clone()));
    }
    let addrs: Vec<SocketAddr> = match url.host.parse::<IpAddr>() {
        Ok(ip) => vec![SocketAddr::new(ip, url.port)],
        Err(_) => tokio::net::lookup_host((url.host.as_str(), url.port)).await.map_err(|e| Refused::Failed(format!("{}: {e}", url.host)))?.collect(),
    };
    if let Some(private) = addrs.iter().find(|a| !local && !public(a.ip())) {
        return Err(Refused::Address(private.ip().to_string()));
    }
    addrs.first().copied().ok_or_else(|| Refused::Failed(format!("{} has no address", url.host)))
}

/// webpki's roots, on ring, made once.
fn tls() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
            let config = ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .expect("ring speaks TLS 1.2 and 1.3")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

async fn get(url: &Url, addr: SocketAddr) -> Result<hyper::Response<hyper::body::Incoming>, Refused> {
    let tcp = tokio::net::TcpStream::connect(addr).await.map_err(|e| Refused::Failed(format!("{addr}: {e}")))?;
    if !url.https {
        return request(tcp, url).await;
    }
    let name = ServerName::try_from(url.host.clone()).map_err(|_| Refused::Url("a host name TLS can verify"))?;
    let tls = tokio_rustls::TlsConnector::from(tls()).connect(name, tcp).await.map_err(|e| Refused::Failed(format!("TLS with {}: {e}", url.host)))?;
    request(tls, url).await
}

async fn request<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(io: S, url: &Url) -> Result<hyper::Response<hyper::body::Incoming>, Refused> {
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(|e| Refused::Failed(e.to_string()))?;
    // ends with the answer's body, or when it is dropped
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let req = Request::get(url.path.as_str())
        .header("host", url.authority())
        .header("accept", "image/*, audio/*, video/*, application/pdf")
        .header("user-agent", "fragment-bridge")
        .body(Empty::<Bytes>::new())
        .map_err(|e| Refused::Failed(e.to_string()))?;
    sender.send_request(req).await.map_err(|e| Refused::Failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid: `https`, a host or an address, a port, a path with its query
    /// (the fragment dropped). Invalid: any other scheme, `http` unless
    /// local, a user, a bad port, a host of other characters, a control
    /// character in the path.
    #[test]
    fn urls_it_fetches() {
        let u = parse("https://v3.fal.media/files/cat.png?x=1#top", false).unwrap();
        assert_eq!(u, Url { https: true, host: "v3.fal.media".into(), port: 443, path: "/files/cat.png?x=1".into() });
        assert_eq!((u.authority(), u.file_name().as_deref()), ("v3.fal.media".to_string(), Some("cat.png")));
        assert_eq!(parse("https://EXAMPLE.com:8443", false).unwrap(), Url { https: true, host: "example.com".into(), port: 8443, path: "/".into() });
        assert_eq!(parse("https://[2606:4700::1]/a", false).unwrap().authority(), "[2606:4700::1]");
        assert_eq!(parse("http://127.0.0.1:9/x.png", true).unwrap(), Url { https: false, host: "127.0.0.1".into(), port: 9, path: "/x.png".into() });
        assert_eq!(parse("https://a.test/", false).unwrap().file_name(), None);
        for bad in ["http://example.com/x.png", "ftp://example.com/x", "file:///etc/passwd", "//example.com/x", "https://user:pw@example.com/x", "https://example.com:0/x", "https://example.com:99999/x", "https://exa_mple.com/x", "https:///x", "https://[::1/x", "https://example.com/a b"] {
            assert!(parse(bad, false).is_err(), "{bad}");
        }
    }

    /// Public addresses only: every private, local, shared, reserved or
    /// special range is not one, an IPv4 address carried in IPv6 as itself.
    #[test]
    fn public_addresses_only() {
        for ip in ["1.1.1.1", "8.8.8.8", "151.101.1.69", "2606:4700::6810:84e5", "::ffff:8.8.8.8", "64:ff9b::808:808"] {
            assert!(public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "0.0.0.0", "10.1.2.3", "127.0.0.1", "100.64.0.1", "169.254.169.254", "172.16.0.1", "172.31.255.255", "192.168.1.1", "192.0.0.8", "192.0.2.1", "198.18.0.1", "198.51.100.1", "203.0.113.1",
            "224.0.0.1", "240.0.0.1", "255.255.255.255", "::", "::1", "fc00::1", "fd12::1", "fe80::1", "ff02::1", "2001:db8::1", "::ffff:127.0.0.1", "::ffff:10.0.0.1", "64:ff9b::a00:1", "::127.0.0.1",
        ] {
            assert!(!public(ip.parse().unwrap()), "{ip}");
        }
    }

    /// The types a chat shows, by their essence; any other is refused.
    #[test]
    fn served_types_only() {
        assert_eq!(served("image/png"), Some("image/png"));
        assert_eq!(served(" Image/JPEG ; charset=binary"), Some("image/jpeg"));
        assert_eq!(served("application/pdf"), Some("application/pdf"));
        for t in ["", "text/html", "image/svg+xml", "application/octet-stream", "application/javascript", "image/png+evil"] {
            assert_eq!(served(t), None, "{t}");
        }
    }

    /// Invalid, before any request: an internal host, a private address, or
    /// a name that resolves to one (`localhost`).
    #[tokio::test]
    async fn local_hosts_are_refused() {
        for url in ["https://api.fragment.internal/x.png", "https://127.0.0.1/x.png", "https://[::1]/x.png", "https://10.0.0.1/x.png", "https://169.254.169.254/latest/meta-data", "https://localhost/x.png"] {
            assert!(matches!(fetch(url, false).await, Err(Refused::Address(_))), "{url}");
        }
    }
}
