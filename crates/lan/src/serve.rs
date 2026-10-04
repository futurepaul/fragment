//! `fragment-lan serve <config>`: the DNS server and the front door, the one
//! process that binds the privileged ports. `cargo xtask dev --lan` writes
//! its config (`ServeConfig`, JSON) and starts it; an operator may run it
//! from a stable path that holds `cap_net_bind_service`.

use std::collections::BTreeMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::{dns, door};

/// The config's shape: a binary refuses a config of another version, so a
/// stale copy at a stable path (installed with its capability) says so
/// instead of misreading it.
pub const CONFIG_VERSION: u32 = 1;
/// Listeners of one kind a config may name.
pub const LISTENERS_MAX: usize = 8;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ServeConfig {
    pub version: u32,
    pub zone: String,
    /// What the zone's names answer: the box's LAN address.
    pub addresses: Vec<Ipv4Addr>,
    /// DNS on UDP and TCP, each address.
    pub dns_listen: Vec<SocketAddr>,
    /// Where every other name is asked: the router.
    pub dns_upstream: SocketAddr,
    pub https_listen: Vec<SocketAddr>,
    /// The https port devices use, for links and moves.
    pub https_port: u16,
    pub http_listen: Vec<SocketAddr>,
    /// The zone's certificate and key (`ca::issue_leaf`).
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
    /// The root's files (`ca::ensure_ca`).
    pub ca_dir: PathBuf,
    /// The cell, and the labels with upstreams of their own (`dex`).
    pub cell: SocketAddr,
    pub routes: BTreeMap<String, SocketAddr>,
}

impl ServeConfig {
    pub fn check(&self) -> Result<()> {
        if self.version != CONFIG_VERSION {
            bail!("this fragment-lan reads config version {CONFIG_VERSION}, not {}: install the one this checkout builds (docs/self-host-lan.md, step 1)", self.version);
        }
        anyhow::ensure!(crate::valid_zone(&self.zone), "{:?} is not a zone", self.zone);
        anyhow::ensure!((1..=dns::ADDRESSES_MAX).contains(&self.addresses.len()), "1 to {} addresses", dns::ADDRESSES_MAX);
        for (what, l) in [("dns_listen", &self.dns_listen), ("https_listen", &self.https_listen), ("http_listen", &self.http_listen)] {
            anyhow::ensure!(l.len() <= LISTENERS_MAX, "{what}: at most {LISTENERS_MAX}");
        }
        anyhow::ensure!(!self.https_listen.is_empty(), "the front door listens somewhere");
        anyhow::ensure!(self.routes.keys().all(|l| crate::valid_label(l)), "routes are labels under the zone");
        Ok(())
    }
}

/// Binds `addr` for TCP, saying what to do when a privileged port is refused.
fn bind_tcp(addr: SocketAddr, what: &str) -> Result<std::net::TcpListener> {
    let l = std::net::TcpListener::bind(addr).map_err(|e| bind_error(e, addr, what))?;
    l.set_nonblocking(true)?;
    Ok(l)
}

fn bind_udp(addr: SocketAddr, what: &str) -> Result<std::net::UdpSocket> {
    let s = std::net::UdpSocket::bind(addr).map_err(|e| bind_error(e, addr, what))?;
    s.set_nonblocking(true)?;
    Ok(s)
}

fn bind_error(e: std::io::Error, addr: SocketAddr, what: &str) -> anyhow::Error {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied if addr.port() < 1024 => anyhow::anyhow!(
            "{what} on {addr}: permission denied. Ports below 1024 need cap_net_bind_service on this binary, or net.ipv4.ip_unprivileged_port_start (docs/self-host-lan.md, step 1), or set its FRAGMENT_LAN_*_PORT above 1023"
        ),
        std::io::ErrorKind::AddrInUse => anyhow::anyhow!("{what} on {addr}: the address is in use (another server holds it)"),
        std::io::ErrorKind::AddrNotAvailable => anyhow::anyhow!("{what} on {addr}: this machine has no such address (FRAGMENT_LAN_ADDR)"),
        _ => anyhow::anyhow!("{what} on {addr}: {e}"),
    }
}

/// Binds every listener, then serves until SIGINT or SIGTERM, or (with
/// `until_stdin_closes`) until its parent goes and stdin closes.
pub fn run(cfg: ServeConfig, until_stdin_closes: bool) -> Result<()> {
    cfg.check()?;
    // every socket first, so a refused port stops the start with its reason
    let mut udp = vec![];
    let mut tcp_dns = vec![];
    for a in &cfg.dns_listen {
        udp.push(bind_udp(*a, "DNS (udp)")?);
        tcp_dns.push(bind_tcp(*a, "DNS (tcp)")?);
    }
    let https: Vec<_> = cfg.https_listen.iter().map(|a| bind_tcp(*a, "the front door (https)")).collect::<Result<_>>()?;
    let http: Vec<_> = cfg.http_listen.iter().map(|a| bind_tcp(*a, "the root's page (http)")).collect::<Result<_>>()?;
    let tls = door::tls_config(&cfg.cert_file, &cfg.key_file)?;
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().context("a tokio runtime")?;
    rt.block_on(async move {
        let server = dns::Server::new(dns::Zone::new(&cfg.zone, cfg.addresses.clone()), cfg.dns_upstream);
        for s in udp {
            tokio::spawn(server.clone().serve_udp(tokio::net::UdpSocket::from_std(s)?));
        }
        for l in tcp_dns {
            tokio::spawn(server.clone().serve_tcp(tokio::net::TcpListener::from_std(l)?));
        }
        let d = door::Door::new(door::DoorConfig { zone: cfg.zone.clone(), https_port: cfg.https_port, cell: cfg.cell, routes: cfg.routes.clone(), ca_dir: cfg.ca_dir.clone() })?;
        for l in https {
            tokio::spawn(d.clone().serve_https(tokio::net::TcpListener::from_std(l)?, tls.clone()));
        }
        for l in http {
            tokio::spawn(d.clone().serve_http(tokio::net::TcpListener::from_std(l)?));
        }
        let show = |l: &[SocketAddr]| if l.is_empty() { "none".to_string() } else { l.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", ") };
        println!(
            "fragment-lan: ready: {zone} -> {addrs}; dns {dns} (others to {up}); https {https}; http {http}",
            zone = cfg.zone,
            addrs = cfg.addresses.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(", "),
            dns = show(&cfg.dns_listen),
            up = cfg.dns_upstream,
            https = show(&cfg.https_listen),
            http = show(&cfg.http_listen),
        );
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let stdin_closed = async {
            if !until_stdin_closes {
                return std::future::pending::<()>().await;
            }
            let mut buf = [0u8; 64];
            let mut stdin = tokio::io::stdin();
            // bounded by the parent's life: it writes nothing, so the read ends at its exit
            loop {
                match tokio::io::AsyncReadExt::read(&mut stdin, &mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => println!("fragment-lan: interrupted, stopping"),
            _ = term.recv() => println!("fragment-lan: terminated, stopping"),
            _ = stdin_closed => println!("fragment-lan: its parent is gone, stopping"),
        }
        anyhow::Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> ServeConfig {
        ServeConfig {
            version: CONFIG_VERSION,
            zone: "fragment.home.arpa".into(),
            addresses: vec![Ipv4Addr::new(192, 168, 50, 7)],
            dns_listen: vec!["192.168.50.7:53".parse().unwrap()],
            dns_upstream: "192.168.50.1:53".parse().unwrap(),
            https_listen: vec!["192.168.50.7:443".parse().unwrap()],
            https_port: 443,
            http_listen: vec![],
            cert_file: "zone.pem".into(),
            key_file: "zone.key".into(),
            ca_dir: ".".into(),
            cell: "127.0.0.1:8790".parse().unwrap(),
            routes: BTreeMap::from([("dex".into(), "127.0.0.1:8800".parse().unwrap())]),
        }
    }

    // Goal: a config of another version, an unknown field, or a bad zone is
    // refused before anything binds.
    #[test]
    fn configs_are_checked() {
        let ok = config();
        ok.check().unwrap();
        let text = serde_json::to_string(&ok).unwrap();
        assert_eq!(serde_json::from_str::<ServeConfig>(&text).unwrap(), ok);
        let stale = ServeConfig { version: CONFIG_VERSION + 1, ..config() };
        assert!(stale.check().unwrap_err().to_string().contains("install the one this checkout builds"));
        assert!(ServeConfig { zone: "Fragment.home.arpa".into(), ..config() }.check().is_err());
        assert!(ServeConfig { addresses: vec![], ..config() }.check().is_err());
        assert!(ServeConfig { routes: BTreeMap::from([("a.b".into(), "127.0.0.1:1".parse().unwrap())]), ..config() }.check().is_err());
        let extra = text.replacen('{', "{\"listen_all\":true,", 1);
        assert!(serde_json::from_str::<ServeConfig>(&extra).is_err());
    }

    // Goal: a privileged port refused for want of the capability says what
    // to do; a port in use says so.
    #[test]
    fn a_refused_port_says_why() {
        let denied = bind_error(std::io::Error::from(std::io::ErrorKind::PermissionDenied), "192.168.50.7:443".parse().unwrap(), "the front door (https)");
        assert!(denied.to_string().contains("cap_net_bind_service"), "{denied}");
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let err = bind_tcp(held.local_addr().unwrap(), "x").unwrap_err();
        assert!(err.to_string().contains("in use"), "{err}");
    }
}
