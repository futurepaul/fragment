//! `cargo xtask dev --lan`: the dev stack on celld, served to the home
//! network as a company serves an intranet (docs/self-host-lan.md). Beside
//! the stack it starts:
//!
//! - **the CA** (`fragment_lan::ca`): made once in the state directory, its
//!   root constrained to the zone; a fresh certificate for the zone and
//!   `*.<zone>` each start;
//! - **Dex** (`fragment_lan::dex`), pinned, on loopback: the issuer
//!   `https://dex.<zone>`, the cell its client, the people `FRAGMENT_LAN_USERS`
//!   with their passwords in files;
//! - **`fragment-lan serve`**: DNS for the zone (every other name to the
//!   router), the TLS front door before the cell and Dex, and the root's page
//!   over http. It is the one process that binds 53, 80 and 443, so it may
//!   run from a stable path that holds `cap_net_bind_service`
//!   (`FRAGMENT_LAN_BIN`).
//!
//! The cell is told its zone (`FRAGMENT_HOST_SUFFIX`), its origin
//! (`FRAGMENT_PLATFORM_URL`, https) and its issuer, and celld trusts the root
//! for the cell's own fetches (`CELLD_EXTRA_CA_FILE`). Every setting is an
//! environment variable (`Settings::from_env`); unset, each has the
//! intranet's default.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use fragment_devstack as devstack;
use fragment_lan::{ca, dex, serve};

/// The zone: a delegated subzone of the home's special-use domain (RFC 8375).
pub const DEFAULT_ZONE: &str = "fragment.home.arpa";
/// Dex listens on loopback, this far above the cell's port.
const DEX_OFFSET: u16 = 10;
/// How long Dex and the front door have to come up.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// The LAN mode's settings, each from the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// `FRAGMENT_LAN_ZONE` (default `fragment.home.arpa`).
    pub zone: String,
    /// `FRAGMENT_LAN_ADDR`: what the zone's names answer (default: this
    /// machine's address toward its default gateway).
    pub addr: Ipv4Addr,
    /// `FRAGMENT_LAN_BIND`: where the DNS server and the front door listen,
    /// a comma-separated list (default: `addr`).
    pub bind: Vec<IpAddr>,
    /// `FRAGMENT_LAN_DNS_UPSTREAM`: where every other name is asked
    /// (default: the default gateway, on 53).
    pub dns_upstream: SocketAddr,
    /// `FRAGMENT_LAN_HTTPS_PORT` (443), `FRAGMENT_LAN_HTTP_PORT` (80; 0 for
    /// none), `FRAGMENT_LAN_DNS_PORT` (53; 0 for none).
    pub https_port: u16,
    pub http_port: u16,
    pub dns_port: u16,
    /// `FRAGMENT_LAN_STATE`: the CA, Dex's secrets, the front door's config
    /// (default `target/devstack/lan`; name one outside target/ to keep the
    /// root past `cargo clean`).
    pub state: PathBuf,
    /// `FRAGMENT_LAN_USERS`: the people Dex signs in (default `paul`).
    pub users: Vec<String>,
    /// `FRAGMENT_LAN_BIN`: a `fragment-lan` at a stable path (one that holds
    /// `cap_net_bind_service`); unset, the one this checkout builds.
    pub bin: Option<PathBuf>,
    /// `DEX_BIN`: another Dex binary; unset, the pinned one.
    pub dex_bin: Option<PathBuf>,
}

impl Settings {
    pub fn from_env() -> Result<Settings> {
        Settings::from(|k| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()), default_route)
    }

    /// The settings from `var`, with `route` naming the default gateway and
    /// this machine's address toward it when they are not set.
    fn from(var: impl Fn(&str) -> Option<String>, route: impl Fn() -> Result<(Ipv4Addr, Ipv4Addr)>) -> Result<Settings> {
        let zone = var("FRAGMENT_LAN_ZONE").unwrap_or_else(|| DEFAULT_ZONE.into());
        anyhow::ensure!(fragment_lan::valid_zone(&zone), "FRAGMENT_LAN_ZONE is a lower-case DNS name of two labels or more, not {zone:?}");
        let port = |k: &str, default: u16, zero: bool| -> Result<u16> {
            match var(k) {
                None => Ok(default),
                Some(v) => match v.parse::<u16>() {
                    Ok(0) if zero => Ok(0),
                    Ok(p) if p > 0 => Ok(p),
                    _ => bail!("{k} is a port{}, not {v:?}", if zero { " (0 for none)" } else { "" }),
                },
            }
        };
        let (https_port, http_port, dns_port) = (port("FRAGMENT_LAN_HTTPS_PORT", 443, false)?, port("FRAGMENT_LAN_HTTP_PORT", 80, true)?, port("FRAGMENT_LAN_DNS_PORT", 53, true)?);
        let explicit_addr = var("FRAGMENT_LAN_ADDR").map(|a| a.parse::<Ipv4Addr>().with_context(|| format!("FRAGMENT_LAN_ADDR is an IPv4 address, not {a:?}"))).transpose()?;
        let explicit_upstream = var("FRAGMENT_LAN_DNS_UPSTREAM")
            .map(|u| u.parse::<SocketAddr>().or_else(|_| u.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, 53))).with_context(|| format!("FRAGMENT_LAN_DNS_UPSTREAM is an address (and :port), not {u:?}")))
            .transpose()?;
        let (addr, dns_upstream) = match (explicit_addr, explicit_upstream) {
            (Some(a), Some(u)) => (a, u),
            (a, u) => {
                let (gateway, local) = route().context("finding the LAN address and the router (set FRAGMENT_LAN_ADDR and FRAGMENT_LAN_DNS_UPSTREAM)")?;
                (a.unwrap_or(local), u.unwrap_or(SocketAddr::new(IpAddr::V4(gateway), 53)))
            }
        };
        let bind = match var("FRAGMENT_LAN_BIND") {
            None => vec![IpAddr::V4(addr)],
            Some(list) => list.split(',').map(|a| a.trim().parse::<IpAddr>().with_context(|| format!("FRAGMENT_LAN_BIND is a list of addresses, not {a:?}"))).collect::<Result<_>>()?,
        };
        anyhow::ensure!((1..=serve::LISTENERS_MAX).contains(&bind.len()), "FRAGMENT_LAN_BIND names 1 to {} addresses", serve::LISTENERS_MAX);
        let users: Vec<String> = var("FRAGMENT_LAN_USERS").unwrap_or_else(|| "paul".into()).split(',').map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect();
        anyhow::ensure!((1..=dex::USERS_MAX).contains(&users.len()), "FRAGMENT_LAN_USERS names 1 to {} people", dex::USERS_MAX);
        Ok(Settings {
            zone,
            addr,
            bind,
            dns_upstream,
            https_port,
            http_port,
            dns_port,
            state: var("FRAGMENT_LAN_STATE").map(PathBuf::from).unwrap_or_else(|| devstack::repo_root().join("target/devstack/lan")),
            users,
            bin: var("FRAGMENT_LAN_BIN").map(PathBuf::from),
            dex_bin: var("DEX_BIN").map(PathBuf::from),
        })
    }

    /// The platform's origin: `https://<zone>`, with the port when not 443.
    pub fn platform_url(&self) -> String {
        fragment_lan::https_origin(&self.zone, self.https_port)
    }

    /// Where the front door takes https from this box: its first address.
    pub fn door(&self) -> SocketAddr {
        SocketAddr::new(self.bind[0], self.https_port)
    }

    /// Dex's issuer: `https://dex.<zone>`, likewise.
    pub fn issuer(&self) -> String {
        fragment_lan::https_origin(&format!("dex.{}", self.zone), self.https_port)
    }
}

/// The default gateway (`/proc/net/route`) and this machine's address
/// toward it (the source a UDP socket connected there would use; nothing
/// is sent).
fn default_route() -> Result<(Ipv4Addr, Ipv4Addr)> {
    let table = std::fs::read_to_string("/proc/net/route").context("read /proc/net/route")?;
    let gateway = gateway_of(&table).context("no IPv4 default route")?;
    let s = UdpSocket::bind("0.0.0.0:0")?;
    s.connect((gateway, 53))?;
    match s.local_addr()?.ip() {
        IpAddr::V4(local) => Ok((gateway, local)),
        IpAddr::V6(_) => bail!("the route to {gateway} has no IPv4 source"),
    }
}

/// The default route's gateway in a `/proc/net/route` table: the row whose
/// destination and mask are both 0 (fields in hex, little-endian).
fn gateway_of(table: &str) -> Option<Ipv4Addr> {
    table.lines().skip(1).find_map(|line| {
        let f: Vec<&str> = line.split_whitespace().collect();
        let hex = |s: &str| u32::from_str_radix(s, 16).ok();
        match (f.get(1).and_then(|d| hex(d)), f.get(2).and_then(|g| hex(g)), f.get(7).and_then(|m| hex(m))) {
            (Some(0), Some(g), Some(0)) if g != 0 => Some(Ipv4Addr::from(g.to_le_bytes())),
            _ => None,
        }
    })
}

/// A child of this process, killed (by its own PID) and reaped when this
/// is dropped: an early error never leaves one holding a port.
struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The LAN half of a running stack: Dex and `fragment-lan serve`, each this
/// process's child, stopped when this is dropped.
pub struct Lan {
    pub settings: Settings,
    pub oidc: devstack::OidcVars,
    /// The root, for celld's own fetches (`CELLD_EXTRA_CA_FILE`).
    pub ca_file: PathBuf,
    pub fingerprint: String,
    pub users: Vec<dex::User>,
    pub door_log: PathBuf,
    pub dex_log: PathBuf,
    /// The front door exits when this closes (`--until-stdin-closes`): it
    /// never outlives this process, however this process ends.
    _door_stdin: ChildStdin,
    _door: Owned,
    _dex: Owned,
}

/// The CA, Dex and the front door, ready, for a cell on `cell_port`.
pub fn start(cell_port: u16) -> Result<Lan> {
    let s = Settings::from_env()?;
    let log_dir = devstack::repo_root().join("target/devstack");
    std::fs::create_dir_all(&log_dir)?;
    // the root, made once; a fresh certificate for the zone
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname").map(|h| h.trim().to_string()).unwrap_or_default();
    let name = if host.is_empty() { "fragment LAN CA".to_string() } else { format!("fragment LAN CA ({host})") };
    let root = ca::ensure_ca(&s.state.join("ca"), &s.zone, &name)?;
    let leaf = ca::issue_leaf(&root)?;
    // the front door's binary first: a stale one stops the start before anything runs
    let bin = door_bin(&s)?;
    // Dex, on loopback behind the door
    let dex_bin = match &s.dex_bin {
        Some(b) => b.clone(),
        None => dex::locate(&devstack::repo_root().join(devstack::TOOLS_DIR))?,
    };
    let dex_listen = SocketAddr::from((Ipv4Addr::LOCALHOST, cell_port.checked_add(DEX_OFFSET).context("the cell's port leaves no room for Dex")?));
    let setup = dex::configure(&s.state.join("dex"), &s.zone, &s.issuer(), dex_listen, &format!("{}/auth/callback", s.platform_url()), &s.users)?;
    let dex_log = log_dir.join("lan-dex.log");
    let (out, err) = log_file(&dex_log)?;
    let mut dex = Owned(Command::new(&dex_bin).arg("serve").arg(&setup.config_file).stdin(Stdio::null()).stdout(out).stderr(err).spawn().with_context(|| format!("start {}", dex_bin.display()))?);
    // the front door and DNS
    let cfg = serve::ServeConfig {
        version: serve::CONFIG_VERSION,
        zone: s.zone.clone(),
        addresses: vec![s.addr],
        dns_listen: if s.dns_port == 0 { vec![] } else { s.bind.iter().map(|ip| SocketAddr::new(*ip, s.dns_port)).collect() },
        dns_upstream: s.dns_upstream,
        https_listen: s.bind.iter().map(|ip| SocketAddr::new(*ip, s.https_port)).collect(),
        https_port: s.https_port,
        http_listen: if s.http_port == 0 { vec![] } else { s.bind.iter().map(|ip| SocketAddr::new(*ip, s.http_port)).collect() },
        cert_file: leaf.cert_file.clone(),
        key_file: leaf.key_file.clone(),
        ca_dir: root.dir.clone(),
        cell: SocketAddr::from((Ipv4Addr::LOCALHOST, cell_port)),
        routes: [("dex".to_string(), dex_listen)].into(),
    };
    cfg.check()?;
    let cfg_file = s.state.join("serve.json");
    std::fs::write(&cfg_file, serde_json::to_string_pretty(&cfg)?)?;
    let door_log = log_dir.join("lan-door.log");
    let (out, err) = log_file(&door_log)?;
    let mut door = Owned(
        Command::new(&bin).arg("serve").arg(&cfg_file).arg("--until-stdin-closes").stdin(Stdio::piped()).stdout(out).stderr(err).spawn().with_context(|| format!("start {}", bin.display()))?,
    );
    let door_stdin = door.0.stdin.take().expect("a piped stdin");
    wait_ready(&mut door.0, &door_log, "fragment-lan serve", |text| text.contains("fragment-lan: ready"))?;
    let discovery = format!("http://{dex_listen}/.well-known/openid-configuration");
    let issuer = s.issuer();
    wait_ready(&mut dex.0, &dex_log, "dex", |_| {
        reqwest::blocking::Client::new().get(&discovery).timeout(Duration::from_secs(2)).send().ok().and_then(|r| r.json::<serde_json::Value>().ok()).is_some_and(|d| d["issuer"] == issuer.as_str())
    })?;
    let client_secret = std::fs::read_to_string(&setup.client_secret_file)?.trim().to_string();
    Ok(Lan {
        oidc: devstack::OidcVars { issuer: s.issuer(), client_id: dex::CLIENT_ID.into(), client_secret: Some(client_secret), scopes: None, claims: None, auth: None, keyed_as: None },
        ca_file: root.pem_file(),
        fingerprint: root.fingerprint(),
        users: setup.users.clone(),
        door_log: door_log.clone(),
        dex_log: dex_log.clone(),
        settings: s,
        _door_stdin: door_stdin,
        _door: door,
        _dex: dex,
    })
}

impl Lan {
    /// What the card renderer reaches fragments through on the LAN: the
    /// front door (its first address, on the https port: the door may
    /// listen on the LAN's address alone, never loopback), and the root
    /// the door's certificate is under (docs/self-host.md, seam 7).
    pub fn renderer_defaults(&self) -> (SocketAddr, PathBuf) {
        (self.settings.door(), self.ca_file.clone())
    }
}

/// A log file, truncated at each start, for a child's stdout and stderr.
fn log_file(path: &Path) -> Result<(std::fs::File, std::fs::File)> {
    let f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(path).with_context(|| format!("open {}", path.display()))?;
    Ok((f.try_clone()?, f))
}

/// Waits until `ready` holds of the log's text (or of the world), the
/// child still running.
fn wait_ready(child: &mut Child, log: &Path, what: &str, mut ready: impl FnMut(&str) -> bool) -> Result<()> {
    let t0 = Instant::now();
    // bounded by READY_TIMEOUT
    loop {
        let text = std::fs::read(log).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        if ready(&text) {
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            bail!("{what} exited ({status}) before it was ready:\n{}", tail(&text));
        }
        if t0.elapsed() > READY_TIMEOUT {
            bail!("{what} was not ready after {READY_TIMEOUT:?}:\n{}", tail(&text));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The last lines of a log, for an error.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(30)..].join("\n")
}

/// The `fragment-lan` to run: `FRAGMENT_LAN_BIN`'s, checked to read this
/// checkout's config; else the one this checkout builds.
fn door_bin(s: &Settings) -> Result<PathBuf> {
    let bin = match &s.bin {
        Some(b) => b.clone(),
        None => {
            let root = devstack::repo_root();
            let status = Command::new("cargo").args(["build", "--quiet", "--release", "-p", "fragment-lan", "--manifest-path"]).arg(root.join("Cargo.toml")).status().context("cargo build -p fragment-lan")?;
            anyhow::ensure!(status.success(), "building fragment-lan failed");
            root.join("target/release/fragment-lan")
        }
    };
    let out = Command::new(&bin).arg("version").output().with_context(|| format!("run {} version", bin.display()))?;
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let want = format!("fragment-lan {}", serve::CONFIG_VERSION);
    anyhow::ensure!(
        said.trim() == want,
        "{} says {:?}, not {want:?}: install the one this checkout builds again (docs/self-host-lan.md, step 1)",
        bin.display(),
        said.trim()
    );
    Ok(bin)
}

/// The lines the dev banner adds.
pub fn banner(lan: &Lan) -> Vec<String> {
    let s = &lan.settings;
    let show = |ips: &[IpAddr], port: u16| ips.iter().map(|ip| SocketAddr::new(*ip, port).to_string()).collect::<Vec<_>>().join(", ");
    let mut lines = vec![
        format!("  LAN:          {}/ (the front door on {}; log {})", s.platform_url(), show(&s.bind, s.https_port), lan.door_log.display()),
        format!("  fragments:    {}", fragment_lan::https_origin(&format!("<label>--<username>.{}", s.zone), s.https_port)),
        format!("  DNS:          {} and *.{} -> {}, on {} (every other name to {})", s.zone, s.zone, s.addr, if s.dns_port == 0 { "no port".into() } else { show(&s.bind, s.dns_port) }, s.dns_upstream),
        format!("  the root:     http://{}{}/ca  (SHA-256 {})", s.addr, if s.http_port == 80 { String::new() } else { format!(":{}", s.http_port) }, lan.fingerprint),
        format!("                {}", lan.ca_file.display()),
        format!("  Dex:          {} (log {})", s.issuer(), lan.dex_log.display()),
    ];
    for u in &lan.users {
        lines.push(format!("  person:       {} ({}), the password in {}", u.name, u.email, u.password_file.display()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn settings(vars: &[(&str, &str)]) -> Result<Settings> {
        let map: HashMap<String, String> = vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Settings::from(|k| map.get(k).cloned(), || Ok((Ipv4Addr::new(192, 168, 50, 1), Ipv4Addr::new(192, 168, 50, 7))))
    }

    // Goal: unset, the LAN is the intranet's: the zone, the box's address
    // toward its router, the router for every other name, 53, 80 and 443.
    #[test]
    fn the_defaults_are_the_intranet() {
        let s = settings(&[]).unwrap();
        assert_eq!(s.zone, "fragment.home.arpa");
        assert_eq!(s.addr, Ipv4Addr::new(192, 168, 50, 7));
        assert_eq!(s.bind, [IpAddr::V4(Ipv4Addr::new(192, 168, 50, 7))]);
        assert_eq!(s.dns_upstream, "192.168.50.1:53".parse().unwrap());
        assert_eq!((s.https_port, s.http_port, s.dns_port), (443, 80, 53));
        assert_eq!(s.users, ["paul"]);
        assert_eq!(s.platform_url(), "https://fragment.home.arpa");
        assert_eq!(s.issuer(), "https://dex.fragment.home.arpa");
        assert_eq!(s.door(), "192.168.50.7:443".parse().unwrap(), "the door is on the LAN's address, not loopback");
    }

    // Goal: each setting moves what it names; high ports show in the origins.
    #[test]
    fn each_setting_moves_its_part() {
        let s = settings(&[
            ("FRAGMENT_LAN_ZONE", "corp.example"),
            ("FRAGMENT_LAN_ADDR", "10.0.0.5"),
            ("FRAGMENT_LAN_BIND", "127.0.0.1, 10.0.0.5"),
            ("FRAGMENT_LAN_DNS_UPSTREAM", "10.0.0.1:5353"),
            ("FRAGMENT_LAN_HTTPS_PORT", "8443"),
            ("FRAGMENT_LAN_HTTP_PORT", "0"),
            ("FRAGMENT_LAN_USERS", "paul, ana"),
        ])
        .unwrap();
        assert_eq!(s.addr, Ipv4Addr::new(10, 0, 0, 5));
        assert_eq!(s.bind.len(), 2);
        assert_eq!(s.dns_upstream, "10.0.0.1:5353".parse().unwrap());
        assert_eq!(s.http_port, 0);
        assert_eq!(s.users, ["paul", "ana"]);
        assert_eq!(s.platform_url(), "https://corp.example:8443");
        assert_eq!(s.issuer(), "https://dex.corp.example:8443");
        assert_eq!(s.door(), "127.0.0.1:8443".parse().unwrap(), "the first address bound");
        assert_eq!(settings(&[("FRAGMENT_LAN_DNS_UPSTREAM", "10.0.0.1")]).unwrap().dns_upstream, "10.0.0.1:53".parse().unwrap());
    }

    // Goal: a bad value stops the start, saying which.
    #[test]
    fn bad_settings_are_refused() {
        for (k, v) in [("FRAGMENT_LAN_ZONE", "Fragment.Home.Arpa"), ("FRAGMENT_LAN_ADDR", "fragment"), ("FRAGMENT_LAN_HTTPS_PORT", "0"), ("FRAGMENT_LAN_DNS_PORT", "99999"), ("FRAGMENT_LAN_BIND", "x")] {
            let e = settings(&[(k, v)]).unwrap_err();
            assert!(format!("{e:#}").contains(k), "{k}: {e:#}");
        }
    }

    // Goal: the default route's gateway is read from the kernel's table.
    #[test]
    fn the_gateway_is_the_default_routes() {
        let table = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0\n\
                     enp11s0\t00000000\t0132A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
        assert_eq!(gateway_of(table), Some(Ipv4Addr::new(192, 168, 50, 1)));
        assert_eq!(gateway_of("Iface\tDestination\n"), None);
    }
}
