//! The card browser's only way out: a SOCKS5 proxy (RFC 1928, `CONNECT`
//! with no authentication) that every Chrome the renderer starts is given
//! (`--proxy-server`, with `<-loopback>` so loopback and `*.localhost` go
//! through it too). It lets through a connection to a fragment's origin
//! alone, `<one label with "--">.<suffix>:<port>`, and connects that to
//! the one address where fragments are served, whatever the name: it
//! resolves nothing, so a page cannot steer it with DNS. Everything else
//! (loopback services, the LAN, the internet, an IP literal) is refused.
//! Chrome hands SOCKS5 the names, never addresses it resolved itself.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Connections through the gate at once (each a browser's socket).
pub const CONNECTIONS_MAX: usize = 256;
/// How long a client has to say where it goes, and the upstream to answer.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The origins a card's page may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origins {
    /// The hostname suffixes fragments are served under (`fragment.localhost`):
    /// a deployment's own, and in the e2e the second site's too.
    pub suffixes: Vec<String>,
    /// The port they are served on (the outside origin's: 443 for https).
    pub port: u16,
    /// Where every one of them is served: the edge (or the node itself).
    pub upstream: SocketAddr,
}

impl Origins {
    pub fn new(suffixes: &[&str], port: u16, upstream: SocketAddr) -> Origins {
        let suffixes: Vec<String> = suffixes.iter().map(|s| s.trim_start_matches('.').to_ascii_lowercase()).collect();
        assert!(!suffixes.is_empty() && suffixes.iter().all(|s| !s.is_empty() && s.split('.').all(dns_label)), "each suffix is a host name: {suffixes:?}");
        Origins { suffixes, port, upstream }
    }

    /// A deployment's fragments as a visitor reaches them: under `suffix`,
    /// on its platform URL's scheme and port (as the cell's
    /// `outside_origin` builds their origins), served at `upstream`, else
    /// on this box at that port (the node itself, or an edge before it).
    pub fn of_platform(suffix: &str, platform_url: &str, upstream: Option<SocketAddr>) -> Result<Origins, String> {
        let (scheme, rest) = platform_url.split_once("://").ok_or_else(|| format!("{platform_url:?} is no URL"))?;
        let default = match scheme {
            "http" => 80,
            "https" => 443,
            other => return Err(format!("{platform_url:?}: fragments are served on http or https, not {other}")),
        };
        let authority = rest.split('/').next().unwrap_or("");
        let port = match authority.rsplit_once(':') {
            Some((_, p)) if !authority.ends_with(']') => p.parse::<u16>().map_err(|_| format!("{platform_url:?}: its port is no port"))?,
            _ => default,
        };
        let upstream = upstream.unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], port)));
        Ok(Origins::new(&[suffix], port, upstream))
    }

    /// Whether `host:port` is a fragment's (or a computer's) own origin:
    /// exactly one label under a suffix, holding `--` as theirs do, never
    /// the suffix itself (the platform's) or a deeper name.
    pub fn allows(&self, host: &str, port: u16) -> bool {
        if port != self.port || host.len() > 253 {
            return false;
        }
        let host = host.to_ascii_lowercase();
        self.suffixes.iter().any(|s| {
            host.strip_suffix(s.as_str()).and_then(|h| h.strip_suffix('.')).is_some_and(|label| dns_label(label) && label.contains("--"))
        })
    }
}

/// One DNS label as fragments' hosts have them: lower-case letters,
/// digits and hyphens, not at either end, at most 63.
fn dns_label(l: &str) -> bool {
    (1..=63).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-') && !l.ends_with('-')
}

/// The gate, serving until dropped.
pub struct Gate {
    pub port: u16,
    stop: Arc<AtomicBool>,
}

impl Gate {
    /// On a free loopback port.
    pub fn start(origins: Origins) -> io::Result<Gate> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let origins = Arc::new(origins);
        let open = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if flag.load(Ordering::Relaxed) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                if open.load(Ordering::Relaxed) >= CONNECTIONS_MAX {
                    // dropped unanswered: Chrome reports the proxy failed
                    continue;
                }
                open.fetch_add(1, Ordering::Relaxed);
                let (origins, open) = (Arc::clone(&origins), Arc::clone(&open));
                std::thread::spawn(move || {
                    let _ = serve(stream, &origins);
                    open.fetch_sub(1, Ordering::Relaxed);
                });
            }
        });
        Ok(Gate { port, stop })
    }

    /// What Chrome's `--proxy-server` names.
    pub fn proxy(&self) -> String {
        format!("socks5://127.0.0.1:{}", self.port)
    }
}

impl Drop for Gate {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port)); // wake the accept loop
    }
}

/// Where a client asked to go (RFC 1928, 4): a name, or an address.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    Name(String, u16),
    /// An IPv4 or IPv6 address: always refused (a fragment has a name).
    Address,
}

/// RFC 1928's replies.
const SUCCEEDED: u8 = 0x00;
const NOT_ALLOWED: u8 = 0x02;
const REFUSED: u8 = 0x05;
const COMMAND_UNSUPPORTED: u8 = 0x07;

/// One client: its greeting (no authentication), its `CONNECT`, the rule,
/// then the bytes both ways until either side closes.
fn serve(mut client: TcpStream, origins: &Origins) -> io::Result<()> {
    client.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let target = match handshake(&mut client) {
        Ok(t) => t,
        Err(reply) => {
            if let Some(code) = reply {
                let _ = reply_with(&mut client, code);
            }
            return Ok(());
        }
    };
    let Target::Name(host, port) = target else { return reply_with(&mut client, NOT_ALLOWED) };
    if !origins.allows(&host, port) {
        return reply_with(&mut client, NOT_ALLOWED);
    }
    let upstream = match TcpStream::connect_timeout(&origins.upstream, HANDSHAKE_TIMEOUT) {
        Ok(u) => u,
        Err(_) => return reply_with(&mut client, REFUSED),
    };
    reply_with(&mut client, SUCCEEDED)?;
    client.set_read_timeout(None)?;
    relay(client, upstream)
}

/// The greeting and the request; `Err` with the reply to send (`None`:
/// the client is not speaking SOCKS5 at all, or went away).
fn handshake(c: &mut TcpStream) -> Result<Target, Option<u8>> {
    let mut b = [0u8; 2];
    c.read_exact(&mut b).map_err(|_| None)?;
    if b[0] != 5 || b[1] == 0 {
        return Err(None);
    }
    let mut methods = vec![0u8; usize::from(b[1])];
    c.read_exact(&mut methods).map_err(|_| None)?;
    if !methods.contains(&0) {
        // no acceptable method
        let _ = c.write_all(&[5, 0xFF]);
        return Err(None);
    }
    c.write_all(&[5, 0]).map_err(|_| None)?;
    let mut req = [0u8; 4];
    c.read_exact(&mut req).map_err(|_| None)?;
    if req[0] != 5 {
        return Err(None);
    }
    if req[1] != 1 {
        return Err(Some(COMMAND_UNSUPPORTED));
    }
    let target = match req[3] {
        1 | 4 => {
            let mut addr = vec![0u8; if req[3] == 1 { 4 } else { 16 }];
            c.read_exact(&mut addr).map_err(|_| None)?;
            Target::Address
        }
        3 => {
            let mut len = [0u8; 1];
            c.read_exact(&mut len).map_err(|_| None)?;
            let mut name = vec![0u8; usize::from(len[0])];
            c.read_exact(&mut name).map_err(|_| None)?;
            match String::from_utf8(name) {
                Ok(n) => Target::Name(n, 0),
                Err(_) => Target::Address,
            }
        }
        _ => return Err(Some(NOT_ALLOWED)),
    };
    let mut port = [0u8; 2];
    c.read_exact(&mut port).map_err(|_| None)?;
    Ok(match target {
        Target::Name(n, _) => Target::Name(n, u16::from_be_bytes(port)),
        a => a,
    })
}

fn reply_with(c: &mut TcpStream, code: u8) -> io::Result<()> {
    // the bound address is never used: 0.0.0.0:0
    c.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0])
}

/// Bytes both ways; each direction's end half-closes the other side.
fn relay(client: TcpStream, upstream: TcpStream) -> io::Result<()> {
    let (mut c_in, mut u_out) = (client.try_clone()?, upstream.try_clone()?);
    let up = std::thread::spawn(move || {
        let _ = io::copy(&mut c_in, &mut u_out);
        let _ = u_out.shutdown(Shutdown::Write);
    });
    let (mut u_in, mut c_out) = (upstream, client);
    let _ = io::copy(&mut u_in, &mut c_out);
    let _ = c_out.shutdown(Shutdown::Write);
    let _ = up.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origins(upstream: SocketAddr) -> Origins {
        Origins::new(&["fragment.localhost", "boats.localhost"], 8790, upstream)
    }

    /// Fragments' and computers' origins pass; the platform's own host, a
    /// deeper name, another suffix, an IP literal, `localhost`, another
    /// port, and a name that only ends like a suffix do not.
    #[test]
    fn only_a_fragments_origin_passes() {
        let o = origins("127.0.0.1:1".parse().unwrap());
        for host in ["todo--paul.fragment.localhost", "TODO--Paul.Fragment.Localhost", "todo--paul--rh.fragment.localhost", "0123456789abcdef01234567--computer.fragment.localhost", "a--b.boats.localhost"] {
            assert!(o.allows(host, 8790), "{host}");
        }
        for (host, port) in [
            ("todo--paul.fragment.localhost", 8791),
            ("todo--paul.fragment.localhost", 443),
            ("fragment.localhost", 8790),
            ("todo.fragment.localhost", 8790),
            ("x.todo--paul.fragment.localhost", 8790),
            ("todo--paul.fragment.localhost.", 8790),
            ("todo--paulfragment.localhost", 8790),
            ("a--b.evil-fragment.localhost", 8790),
            ("-a--b.fragment.localhost", 8790),
            ("a_--b.fragment.localhost", 8790),
            ("127.0.0.1", 8790),
            ("[::1]", 8790),
            ("::1", 8790),
            ("localhost", 8790),
            ("other.localhost", 8790),
            ("example.com", 80),
            ("", 8790),
        ] {
            assert!(!o.allows(host, port), "{host}:{port}");
        }
        assert!(!o.allows(&format!("{}--b.fragment.localhost", "a".repeat(62)), 8790), "a label past 63");
    }

    /// The origins of a deployment's platform URL: its port, or its
    /// scheme's; the upstream on loopback there unless one is named.
    #[test]
    fn a_platforms_origins_take_its_port() {
        let up: SocketAddr = "192.168.50.7:443".parse().unwrap();
        for (url, upstream, port, at) in [
            ("http://127.0.0.1:8790", None, 8790, "127.0.0.1:8790"),
            ("https://fragment.home.arpa", None, 443, "127.0.0.1:443"),
            ("https://fragment.home.arpa/", Some(up), 443, "192.168.50.7:443"),
            ("http://fragment.localhost", None, 80, "127.0.0.1:80"),
            ("https://[::1]:8443", None, 8443, "127.0.0.1:8443"),
        ] {
            let o = Origins::of_platform("fragment.home.arpa", url, upstream).unwrap();
            assert_eq!((o.port, o.upstream.to_string(), o.suffixes.clone()), (port, at.to_string(), vec!["fragment.home.arpa".to_string()]), "{url}");
        }
        for bad in ["127.0.0.1:8790", "ftp://x", "http://x:http", "http://x:99999"] {
            assert!(Origins::of_platform("fragment.home.arpa", bad, None).is_err(), "{bad}");
        }
    }

    /// A SOCKS5 client's request to the gate: the reply code, and the
    /// connection when it was let through.
    fn ask(gate: &Gate, atyp: u8, addr: &[u8], port: u16) -> (u8, TcpStream) {
        let mut c = TcpStream::connect(("127.0.0.1", gate.port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&[5, 1, 0]).unwrap();
        let mut b = [0u8; 2];
        c.read_exact(&mut b).unwrap();
        assert_eq!(b, [5, 0], "no authentication is chosen");
        let mut req = vec![5, 1, 0, atyp];
        if atyp == 3 {
            req.push(addr.len() as u8);
        }
        req.extend_from_slice(addr);
        req.extend_from_slice(&port.to_be_bytes());
        c.write_all(&req).unwrap();
        let mut reply = [0u8; 10];
        c.read_exact(&mut reply).unwrap();
        (reply[1], c)
    }

    /// Valid: a fragment's origin reaches the upstream, whatever the name
    /// resolves to (it is never resolved), both ways. Invalid: anything
    /// else is refused before any connection is made; an upstream that is
    /// down is reported so. Replay: a second client after the first gets
    /// its own connection.
    #[test]
    fn the_gate_connects_fragments_to_the_upstream_alone() {
        let upstream = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = upstream.local_addr().unwrap();
        let served = std::thread::spawn(move || {
            let mut seen = vec![];
            for _ in 0..2 {
                let (mut s, _) = upstream.accept().unwrap();
                let mut b = [0u8; 4];
                s.read_exact(&mut b).unwrap();
                seen.push(b);
                s.write_all(b"pong").unwrap();
            }
            seen
        });
        let gate = Gate::start(origins(addr)).unwrap();
        for _ in 0..2 {
            let (code, mut c) = ask(&gate, 3, b"todo--paul.fragment.localhost", 8790);
            assert_eq!(code, SUCCEEDED);
            c.write_all(b"ping").unwrap();
            let mut b = [0u8; 4];
            c.read_exact(&mut b).unwrap();
            assert_eq!(&b, b"pong");
        }
        assert_eq!(served.join().unwrap(), vec![*b"ping", *b"ping"], "only the two fragments' connections reached it");
        for (atyp, addr, port) in [
            (3u8, b"127.0.0.1".to_vec(), 8790u16),
            (3, b"localhost".to_vec(), 8790),
            (3, b"todo--paul.fragment.localhost".to_vec(), 22),
            (3, b"fragment.localhost".to_vec(), 8790),
            (1, vec![127, 0, 0, 1], 8790),
            (4, [0u8; 15].iter().chain([1u8].iter()).copied().collect(), 8790),
        ] {
            let (code, _) = ask(&gate, atyp, &addr, port);
            assert_eq!(code, NOT_ALLOWED, "{:?}:{port}", String::from_utf8_lossy(&addr));
        }
        // an upstream that is not there
        let dead = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let gate = Gate::start(origins(dead)).unwrap();
        assert_eq!(ask(&gate, 3, b"todo--paul.fragment.localhost", 8790).0, REFUSED);
    }

    /// A client that asks for a method other than none, or a command other
    /// than CONNECT, is refused.
    #[test]
    fn only_connect_without_authentication_is_spoken() {
        let gate = Gate::start(origins("127.0.0.1:1".parse().unwrap())).unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", gate.port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&[5, 1, 2]).unwrap();
        let mut b = [0u8; 2];
        c.read_exact(&mut b).unwrap();
        assert_eq!(b, [5, 0xFF], "username and password alone: no acceptable method");
        let mut c = TcpStream::connect(("127.0.0.1", gate.port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&[5, 1, 0]).unwrap();
        c.read_exact(&mut b).unwrap();
        let name = b"todo--paul.fragment.localhost";
        let mut req = vec![5, 2, 0, 3, name.len() as u8];
        req.extend_from_slice(name);
        req.extend_from_slice(&8790u16.to_be_bytes());
        c.write_all(&req).unwrap();
        let mut reply = [0u8; 10];
        c.read_exact(&mut reply).unwrap();
        assert_eq!(reply[1], COMMAND_UNSUPPORTED, "BIND");
    }
}
