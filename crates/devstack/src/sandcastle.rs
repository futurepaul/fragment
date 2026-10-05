//! Computers on sandcastle nodes (docs/self-host.md, seam 2), for dev and
//! the e2e: the deployment's node list, read from a file (`NodesFile`), and
//! nodes started here (`SandcastleNode`), each a `sandcastle-node` in front
//! of sandcastle's Docker engine double, which runs every container in
//! Docker: no root and no VMs, Docker's isolation only (sandcastle's
//! docs/node.md, the double). A node either listens on loopback or dials
//! the platform (its uplink).
//!
//! `SANDCASTLE_DIR` names a sandcastle checkout with the three binaries
//! built: `cargo build --release -p sandcastle-node -p
//! sandcastle-docker-engine`, and `cargo build --release -p
//! sandcastle-docker-relay --target <arch>-unknown-linux-musl` (the relay
//! runs inside each container, so it is static).
//!
//! Or a node fronts the machine's real engine (`Engine::Real`: root, its
//! microVMs jailed, already running under its service), and only
//! `sandcastle-node` is needed. The run's images are loaded into it
//! (`load_images`), and the containers the run started there are destroyed
//! when the node stops: the engine itself stays up.

use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use fragment_core::placement::{self, Byoc, Nodes};
use serde_json::{json, Value};

pub const SANDCASTLE_DIR_VAR: &str = "SANDCASTLE_DIR";
/// The dev stack's node list (`NodesFile`).
pub const NODES_FILE_VAR: &str = "FRAGMENT_NODES_FILE";
/// A node's engine and API must be up within this of their start.
pub const NODE_READY_TIMEOUT: Duration = Duration::from_secs(30);
/// A graceful stop must finish within this; past it the process is killed.
/// The engine double removes its containers as it stops.
pub const NODE_STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// A node's secret, made here: 32 random bytes, as hex.
const SECRET_HEX_BYTES: usize = 32;
/// A unix socket's path, at most this many bytes: Linux's `sun_path` holds
/// 108, the last a NUL. A node handed a longer one cannot bind it, and says
/// so only in its own log.
pub const SOCKET_PATH_MAX: usize = 107;

/// `dir/name`, the path of a socket a node binds or dials, refused when it
/// is too long to be one (a deep checkout's scratch).
pub fn socket_path(dir: &Path, name: &str) -> Result<PathBuf> {
    let path = dir.join(name);
    let len = path.as_os_str().len();
    if len > SOCKET_PATH_MAX {
        bail!("{} is {len} bytes, past a unix socket's {SOCKET_PATH_MAX}: name its directory shorter", path.display());
    }
    Ok(path)
}

/// The deployment's nodes as the cell reads them: `FRAGMENT_NODES`, and
/// each node's secret under the name the cell looks for
/// (`placement::secret_name`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodesVars {
    pub nodes: String,
    pub secrets: Vec<(String, String)>,
}

/// The dev stack's node list (`FRAGMENT_NODES_FILE`): `FRAGMENT_NODES`'
/// shape (fragment_core::placement), each node with its secret's file
/// beside it (`"secret_file"`), which is read here and never written into
/// the variable.
pub struct NodesFile;

impl NodesFile {
    pub fn read(path: &Path) -> Result<NodesVars> {
        let text = fs::read_to_string(path).with_context(|| format!("read {} ({NODES_FILE_VAR})", path.display()))?;
        let mut v: Value = serde_json::from_str(&text).with_context(|| format!("{} is FRAGMENT_NODES' JSON, each node with its \"secret_file\"", path.display()))?;
        let mut secrets = vec![];
        for n in v["nodes"].as_array_mut().context("a node list names its \"nodes\"")? {
            let obj = n.as_object_mut().context("a node is an object")?;
            let id = obj.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
            let file = obj.remove("secret_file").and_then(|f| f.as_str().map(PathBuf::from)).with_context(|| format!("the node {id:?} names its \"secret_file\""))?;
            let secret = fs::read_to_string(&file).with_context(|| format!("read the node {id}'s secret_file {}", file.display()))?.trim().to_string();
            if secret.len() < placement::SECRET_BYTES_MIN {
                bail!("the node {id}'s secret ({}) is at least {} bytes", file.display(), placement::SECRET_BYTES_MIN);
            }
            if placement::valid_node_id(&id) {
                secrets.push((placement::secret_name(&id), secret));
            }
        }
        let nodes = v.to_string();
        // checked as the cell checks it, so a bad list stops here (an empty
        // one too, unless the cell is told FRAGMENT_BYOC=on: it says so)
        Nodes::parse(&nodes, Byoc::On).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        Ok(NodesVars { nodes, secrets })
    }
}

/// The binaries a node is made of.
#[derive(Clone)]
pub struct Tools {
    pub node: PathBuf,
    pub engine: PathBuf,
    /// The static relay the engine double puts in each container.
    pub relay: PathBuf,
}

impl Tools {
    /// From `SANDCASTLE_DIR`'s builds; each must be a file (only the node,
    /// in front of a real engine).
    pub fn locate(real: bool) -> Result<Tools> {
        let dir = std::env::var_os(SANDCASTLE_DIR_VAR)
            .map(PathBuf::from)
            .with_context(|| format!("set {SANDCASTLE_DIR_VAR} to a sandcastle checkout (its branch node-placement), with sandcastle-node, sandcastle-docker-engine and the static sandcastle-docker-relay built"))?;
        let musl = format!("{}-unknown-linux-musl", std::env::consts::ARCH);
        let tools = Tools {
            node: dir.join("target/release/sandcastle-node"),
            engine: dir.join("target/release/sandcastle-docker-engine"),
            relay: dir.join("target").join(&musl).join("release/sandcastle-docker-relay"),
        };
        for (bin, build) in [
            (&tools.node, "cargo build --release -p sandcastle-node".to_string()),
            (&tools.engine, "cargo build --release -p sandcastle-docker-engine".to_string()),
            (&tools.relay, format!("cargo build --release -p sandcastle-docker-relay --target {musl}")),
        ] {
            if !bin.is_file() && (!real || *bin == tools.node) {
                bail!("no {} ({SANDCASTLE_DIR_VAR}: {build} there)", bin.display());
            }
        }
        Ok(tools)
    }
}

/// How the platform reaches a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// It listens on this loopback port.
    Listen(u16),
    /// It dials the platform's uplink.
    Uplink,
}

/// What a node fronts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Engine {
    /// sandcastle's Docker engine double, started here under the node's
    /// directory, its containers in local Docker.
    Double,
    /// The machine's real engine, already running, its sockets in this
    /// directory (its `state_dir`, e.g. /var/lib/sandcastle).
    Real(PathBuf),
}

impl Engine {
    fn dir(&self, node_dir: &Path) -> PathBuf {
        match self {
            Engine::Double => node_dir.join("e"),
            Engine::Real(dir) => dir.clone(),
        }
    }
}

pub struct NodeSpec {
    pub id: String,
    pub reach: Reach,
    pub engine: Engine,
    /// Its state: the engine's sockets, its config and secret.
    pub dir: PathBuf,
    /// The platform's origin: its uplink and `/api/nodes/egress`.
    pub platform: String,
    /// The computers it holds (`FRAGMENT_NODES`' capacity).
    pub capacity: u32,
    /// Where its two logs go (`sandcastle-<id>-engine.log`, `-node.log`).
    pub log_dir: PathBuf,
}

/// The node's config (sandcastle's crates/node/src/config.rs).
fn node_config(spec: &NodeSpec) -> Result<Value> {
    let engine = spec.engine.dir(&spec.dir);
    let mut config = json!({
        "engine": socket_path(&engine, "engine.sock")?,
        "ports": socket_path(&engine, "ports.sock")?,
        "egress": socket_path(&spec.dir, "egress.sock")?,
        "secret_file": spec.dir.join("node.secret"),
        "platform": spec.platform,
    });
    match spec.reach {
        Reach::Listen(port) => config["listen"] = json!(format!("127.0.0.1:{port}")),
        Reach::Uplink => config["uplink"] = json!({ "url": format!("{}/api/nodes/uplink", spec.platform.replacen("http", "ws", 1)), "id": spec.id }),
    }
    Ok(config)
}

/// One node: the engine double and the node in front of it.
pub struct SandcastleNode {
    pub id: String,
    pub reach: Reach,
    pub capacity: u32,
    /// Its secret, which the platform holds too.
    pub secret: String,
    node_bin: PathBuf,
    config: PathBuf,
    log: PathBuf,
    /// The engine double this node started; none in front of a real one.
    engine: Option<Child>,
    /// A real engine's API socket, and when this node started: the
    /// containers started there since are the run's, destroyed at `stop`.
    real: Option<(PathBuf, u64)>,
    /// Its engine's API socket (the double's, or the real one).
    engine_sock: PathBuf,
    node: Option<Child>,
}

impl SandcastleNode {
    /// Starts the engine double, then the node, and waits until the engine's
    /// socket answers and (a node that listens) the node's port takes a
    /// connection. A node that dials does so by itself, again and again,
    /// until the platform answers.
    pub fn start(tools: &Tools, spec: &NodeSpec) -> Result<SandcastleNode> {
        assert!(placement::valid_node_id(&spec.id), "a node's id is the platform's form");
        let engine_dir = spec.engine.dir(&spec.dir);
        fs::create_dir_all(&spec.dir).with_context(|| format!("create {}", spec.dir.display()))?;
        fs::create_dir_all(&spec.log_dir)?;
        let secret = crate::random_hex(SECRET_HEX_BYTES);
        let mut f = fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(spec.dir.join("node.secret"))?;
        std::io::Write::write_all(&mut f, secret.as_bytes())?;
        let config = spec.dir.join("node.json");
        fs::write(&config, serde_json::to_string_pretty(&node_config(spec)?)?)?;
        let log_of = |what: &str| spec.log_dir.join(format!("sandcastle-{}-{what}.log", spec.id));
        let engine_log = log_of("engine");
        let sock = engine_dir.join("engine.sock");
        let (engine, real) = match &spec.engine {
            Engine::Double => {
                fs::create_dir_all(&engine_dir).with_context(|| format!("create {}", engine_dir.display()))?;
                let out = fs::OpenOptions::new().create(true).append(true).open(&engine_log)?;
                let child = Command::new(&tools.engine)
                    .arg("--dir")
                    .arg(&engine_dir)
                    .arg("--relay")
                    .arg(&tools.relay)
                    .stdin(Stdio::null())
                    .stdout(out.try_clone()?)
                    .stderr(out)
                    .spawn()
                    .with_context(|| format!("start {}", tools.engine.display()))?;
                (Some(child), None)
            }
            Engine::Real(_) => {
                let (status, body) = engine_call(&sock, "GET", "/v1/health", None).with_context(|| format!("the real engine at {} (is its service running, and may this user reach its socket?)", sock.display()))?;
                if status != 200 {
                    bail!("the real engine's health at {}: {status} {}", sock.display(), String::from_utf8_lossy(&body));
                }
                (None, Some((sock.clone(), now_ms())))
            }
        };
        let mut n = SandcastleNode {
            id: spec.id.clone(),
            reach: spec.reach,
            capacity: spec.capacity,
            secret,
            node_bin: tools.node.clone(),
            config,
            log: log_of("node"),
            engine,
            real,
            engine_sock: sock.clone(),
            node: None,
        };
        n.wait_for("the engine's socket", &engine_log, || std::os::unix::net::UnixStream::connect(&sock).is_ok())?;
        n.up()?;
        Ok(n)
    }

    /// The engine double alone, for a node that becomes a person's own
    /// (docs/self-host.md, seam 2, Bring your own computer): its id, config
    /// and secret come from `sandcastle-node pair` (`pair`, then `paired`).
    pub fn start_to_pair(tools: &Tools, spec: &NodeSpec) -> Result<SandcastleNode> {
        let engine_dir = spec.dir.join("e");
        fs::create_dir_all(&engine_dir).with_context(|| format!("create {}", engine_dir.display()))?;
        fs::create_dir_all(&spec.log_dir)?;
        let log_of = |what: &str| spec.log_dir.join(format!("sandcastle-{}-{what}.log", spec.id));
        let engine_log = log_of("engine");
        let out = fs::OpenOptions::new().create(true).append(true).open(&engine_log)?;
        let engine = Command::new(&tools.engine)
            .arg("--dir")
            .arg(&engine_dir)
            .arg("--relay")
            .arg(&tools.relay)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .with_context(|| format!("start {}", tools.engine.display()))?;
        // its id until it is paired: the label its logs carry
        let mut n = SandcastleNode {
            id: spec.id.clone(),
            reach: Reach::Uplink,
            capacity: placement::OWN_CAPACITY,
            secret: String::new(),
            node_bin: tools.node.clone(),
            config: spec.dir.join("node.json"),
            log: log_of("node"),
            engine: Some(engine),
            real: None,
            engine_sock: engine_dir.join("engine.sock"),
            node: None,
        };
        let sock = engine_dir.join("engine.sock");
        n.wait_for("the engine's socket", &engine_log, || std::os::unix::net::UnixStream::connect(&sock).is_ok())?;
        Ok(n)
    }

    /// `sandcastle-node pair <platform> --name <name>`, writing this node's
    /// config and secret: started, its output read as it comes. A person
    /// approves the code it shows (`Pairing::code`), then it ends.
    pub fn pair(&self, platform: &str, name: &str) -> Result<Pairing> {
        let dir = self.config.parent().context("a node's config has a directory")?;
        let engine = dir.join("e");
        let err = fs::OpenOptions::new().create(true).append(true).open(&self.log)?;
        let mut child = Command::new(&self.node_bin)
            .arg("pair")
            .arg(platform)
            .arg("--config")
            .arg(&self.config)
            .args(["--name", name])
            .arg("--engine")
            .arg(socket_path(&engine, "engine.sock")?)
            .arg("--ports")
            .arg(socket_path(&engine, "ports.sock")?)
            .arg("--egress")
            .arg(socket_path(dir, "egress.sock")?)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(err)
            .spawn()
            .with_context(|| format!("start {} pair", self.node_bin.display()))?;
        let out = child.stdout.take().context("pair's output")?;
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            use std::io::BufRead;
            for line in std::io::BufReader::new(out).lines().map_while(std::result::Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Pairing { child: Some(child), lines, said: vec![], log: self.log.clone() })
    }

    /// After its pairing ended well: the id the platform named it by, read
    /// from the config the pairing wrote, and the node started on it.
    pub fn paired(&mut self) -> Result<String> {
        let config: Value = serde_json::from_slice(&fs::read(&self.config).with_context(|| format!("read {}", self.config.display()))?)?;
        let id = config["uplink"]["id"].as_str().context("a paired node's config names its uplink's id")?.to_string();
        anyhow::ensure!(fragment_core::pairing::is_paired_id(&id), "the pairing named the node {id:?}, which is no person's node's id");
        self.id = id.clone();
        self.up()?;
        Ok(id)
    }

    /// The node's own log (`sandcastle-node`'s, a pairing's included).
    pub fn log_text(&self) -> String {
        fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// The node's API, when it listens.
    pub fn url(&self) -> Option<String> {
        match self.reach {
            Reach::Listen(port) => Some(format!("http://127.0.0.1:{port}")),
            Reach::Uplink => None,
        }
    }

    /// Its row in `FRAGMENT_NODES`.
    pub fn listing(&self) -> Value {
        let mut row = json!({ "id": self.id, "arch": std::env::consts::ARCH, "capacity": self.capacity });
        match self.url() {
            Some(url) => row["url"] = json!(url),
            None => row["uplink"] = json!(true),
        }
        row
    }

    /// Whether it fronts the machine's real engine (`Engine::Real`).
    pub fn is_real(&self) -> bool {
        self.real.is_some()
    }

    /// Whether the node process runs (not `down`).
    pub fn is_up(&self) -> bool {
        self.node.is_some()
    }

    /// Starts the node process (again, after `down`).
    pub fn up(&mut self) -> Result<()> {
        assert!(self.node.is_none(), "one node process at a time");
        let out = fs::OpenOptions::new().create(true).append(true).open(&self.log)?;
        let child = Command::new(&self.node_bin)
            .arg("serve")
            .arg("--config")
            .arg(&self.config)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .spawn()
            .with_context(|| format!("start {}", self.node_bin.display()))?;
        self.node = Some(child);
        if let Reach::Listen(port) = self.reach {
            let log = self.log.clone();
            self.wait_for("the node's port", &log, || std::net::TcpStream::connect(("127.0.0.1", port)).is_ok())?;
        }
        Ok(())
    }

    /// The node process stops (the node going down, to the platform); the
    /// engine and its containers stay, as a node's do when its API process
    /// restarts.
    pub fn down(&mut self) -> Result<()> {
        let child = self.node.take().context("the node is already down")?;
        stop(child, "sandcastle-node")
    }

    /// The containers running on its engine that started at or after
    /// `since_ms` (on a real engine, which other runs share, only this
    /// run's), by name.
    pub fn running_since(&self, since_ms: u64) -> Result<Vec<String>> {
        Ok(containers(&self.engine_sock)?.into_iter().filter(|c| c.running && c.started_at_ms >= since_ms).map(|c| c.name).collect())
    }

    /// Kills container `name` on its engine (SIGKILL), behind the platform's
    /// back: to its computer, the container died under it.
    pub fn kill(&self, name: &str) -> Result<()> {
        let (status, body) = engine_call(&self.engine_sock, "POST", &format!("/v1/containers/{name}/signal"), Some(EngineBody::Bytes(br#"{"signal":9}"#.to_vec())))?;
        anyhow::ensure!(status == 200 || status == 204, "the engine's signal to {name}: {status} {}", String::from_utf8_lossy(&body));
        Ok(())
    }

    /// Both processes stop: the node, then the engine double, which removes
    /// its containers.
    pub fn stop(mut self) -> Result<()> {
        let node = self.node.take().map(|c| stop(c, "sandcastle-node"));
        let engine = self.engine.take().map(|c| stop(c, "the engine double"));
        let real = self.real.take().map(|(sock, since)| destroy_since(&sock, since));
        node.transpose()?;
        engine.transpose()?;
        real.transpose()?;
        Ok(())
    }

    /// Polls `ready` until it holds, the engine or the node exits, or
    /// NODE_READY_TIMEOUT passes.
    fn wait_for(&mut self, what: &str, log: &Path, mut ready: impl FnMut() -> bool) -> Result<()> {
        let t0 = Instant::now();
        // Bounded by NODE_READY_TIMEOUT.
        loop {
            if ready() {
                return Ok(());
            }
            for (name, child) in [("the engine double", &mut self.engine), ("sandcastle-node", &mut self.node)] {
                if let Some(status) = child.as_mut().map(Child::try_wait).transpose()?.flatten() {
                    *child = None;
                    bail!("{name} of the node {} exited ({status}) before {what} answered:\n{}", self.id, fs::read_to_string(log).unwrap_or_default());
                }
            }
            if t0.elapsed() > NODE_READY_TIMEOUT {
                bail!("{what} of the node {} did not answer within {NODE_READY_TIMEOUT:?}:\n{}", self.id, fs::read_to_string(log).unwrap_or_default());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A `sandcastle-node pair` under way (`SandcastleNode::pair`).
pub struct Pairing {
    child: Option<Child>,
    lines: std::sync::mpsc::Receiver<String>,
    /// What it printed so far.
    pub said: Vec<String>,
    log: PathBuf,
}

impl Pairing {
    /// The code it shows and the link it says to approve it at, within `wait`.
    pub fn code(&mut self, wait: Duration) -> Result<(String, String)> {
        let t0 = Instant::now();
        // Bounded by `wait`.
        while t0.elapsed() < wait {
            if let Ok(line) = self.lines.recv_timeout(Duration::from_millis(100)) {
                self.said.push(line);
            }
            let link = self.said.iter().find_map(|l| l.trim().strip_prefix("open ")).map(str::to_string);
            if let Some(link) = link {
                let code = link.split("code=").nth(1).context("the link names its code")?.to_string();
                return Ok((code, link));
            }
            if let Some(status) = self.child.as_mut().map(Child::try_wait).transpose()?.flatten() {
                bail!("sandcastle-node pair exited ({status}) before it showed a code:\n{}\n{}", self.said.join("\n"), fs::read_to_string(&self.log).unwrap_or_default());
            }
        }
        bail!("sandcastle-node pair showed no code within {wait:?}: {}", self.said.join("\n"))
    }

    /// Its exit within `wait`: success, or what it said.
    pub fn finish(mut self, wait: Duration) -> Result<()> {
        let mut child = self.child.take().expect("a pairing's process, once");
        let t0 = Instant::now();
        // Bounded by `wait`.
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if t0.elapsed() > wait {
                let _ = child.kill();
                let _ = child.wait();
                bail!("sandcastle-node pair did not finish within {wait:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        while let Ok(line) = self.lines.recv_timeout(Duration::from_millis(100)) {
            self.said.push(line);
        }
        anyhow::ensure!(status.success(), "sandcastle-node pair failed ({status}):\n{}\n{}", self.said.join("\n"), fs::read_to_string(&self.log).unwrap_or_default());
        Ok(())
    }
}

impl Drop for Pairing {
    fn drop(&mut self) {
        if let Some(c) = self.child.take() {
            let _ = stop(c, "sandcastle-node pair");
        }
    }
}

/// SIGTERM to a child this process started, and its exit awaited; killed
/// past NODE_STOP_TIMEOUT.
fn stop(mut child: Child, what: &str) -> Result<()> {
    let _ = Command::new("kill").args(["-TERM", &child.id().to_string()]).stderr(Stdio::null()).status();
    let t0 = Instant::now();
    // Bounded by NODE_STOP_TIMEOUT.
    while child.try_wait()?.is_none() {
        if t0.elapsed() > NODE_STOP_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{what} did not stop within {NODE_STOP_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

impl Drop for SandcastleNode {
    /// An early error must not leave its processes running: the engine is
    /// told to stop (it removes its containers), never killed outright; a
    /// real engine's containers the run started are destroyed.
    fn drop(&mut self) {
        for (child, what) in [(self.node.take(), "sandcastle-node"), (self.engine.take(), "the engine double")] {
            if let Some(c) = child {
                let _ = stop(c, what);
            }
        }
        if let Some((sock, since)) = self.real.take() {
            let _ = destroy_since(&sock, since);
        }
    }
}

/// The time now, as an engine's `startedAtMs` counts it.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// The most one answer from an engine's API may be.
const ENGINE_ANSWER_BYTES_MAX: u64 = 16 << 20;
/// An engine call's limit: an image's load builds its disk in a VM.
const ENGINE_CALL_TIMEOUT: Duration = Duration::from_secs(600);

/// One HTTP/1.1 call to an engine's API on its unix socket: its status and
/// body. A body is a file, streamed (an image's tar), or bytes.
fn engine_call(sock: &Path, method: &str, path: &str, body: Option<EngineBody>) -> Result<(u16, Vec<u8>)> {
    use std::io::{Read, Write};
    let mut c = std::os::unix::net::UnixStream::connect(sock).with_context(|| format!("connect {}", sock.display()))?;
    c.set_read_timeout(Some(ENGINE_CALL_TIMEOUT))?;
    c.set_write_timeout(Some(ENGINE_CALL_TIMEOUT))?;
    let len = match &body {
        None => 0,
        Some(EngineBody::Bytes(b)) => b.len() as u64,
        Some(EngineBody::File(p)) => fs::metadata(p)?.len(),
    };
    write!(c, "{method} {path} HTTP/1.1\r\nhost: engine\r\nconnection: close\r\ncontent-type: application/json\r\ncontent-length: {len}\r\n\r\n")?;
    match body {
        None => {}
        Some(EngineBody::Bytes(b)) => c.write_all(&b)?,
        Some(EngineBody::File(p)) => {
            std::io::copy(&mut fs::File::open(&p)?, &mut c)?;
        }
    }
    let mut answer = vec![];
    c.take(ENGINE_ANSWER_BYTES_MAX).read_to_end(&mut answer)?;
    let head_end = answer.windows(4).position(|w| w == b"\r\n\r\n").context("an engine's answer has a head")?;
    let head = String::from_utf8_lossy(&answer[..head_end]).into_owned();
    let status = head.split(' ').nth(1).and_then(|s| s.parse().ok()).context("an engine's answer has a status")?;
    let mut rest = answer[head_end + 4..].to_vec();
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        rest = unchunk(&rest)?;
    }
    Ok((status, rest))
}

enum EngineBody {
    Bytes(Vec<u8>),
    File(PathBuf),
}

/// A chunked body, joined.
fn unchunk(mut b: &[u8]) -> Result<Vec<u8>> {
    let mut out = vec![];
    // Bounded by the body: each pass consumes one chunk.
    loop {
        let line_end = b.windows(2).position(|w| w == b"\r\n").context("a chunk's size line")?;
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..line_end]).split(';').next().unwrap_or("").trim(), 16).context("a chunk's size")?;
        b = &b[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        anyhow::ensure!(b.len() >= size + 2, "a chunk shorter than its size");
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

/// The run's images, built into local Docker (`build_cell_images`), loaded
/// into a real engine as `docker.io/library/<tag>`: each saved from Docker
/// to a file under `scratch`, then handed to the engine, which builds its
/// disk (once per image: a reference to one it holds is named at once).
/// Each load is printed with its size and what it took.
pub fn load_images(engine_dir: &Path, tags: &[String], scratch: &Path) -> Result<()> {
    let sock = engine_dir.join("engine.sock");
    fs::create_dir_all(scratch)?;
    // bounded: the run's images
    for tag in tags {
        let t0 = Instant::now();
        let tar = scratch.join(format!("{}.tar", tag.replace([':', '/'], "-")));
        let out = Command::new("docker").args(["save", "-o"]).arg(&tar).arg(tag).stdin(Stdio::null()).output().context("run docker save")?;
        if !out.status.success() {
            bail!("docker save {tag}: {}", String::from_utf8_lossy(&out.stderr));
        }
        let saved = t0.elapsed();
        let bytes = fs::metadata(&tar)?.len();
        let reference = format!("docker.io/library/{tag}");
        let path = format!("/v1/images/load?reference={}", reference.replace('/', "%2F").replace(':', "%3A"));
        let answer = engine_call(&sock, "POST", &path, Some(EngineBody::File(tar.clone())));
        let _ = fs::remove_file(&tar);
        let (status, body) = answer?;
        if status != 200 {
            bail!("the real engine's load of {reference}: {status} {}", String::from_utf8_lossy(&body));
        }
        // the disk's build time, as the engine kept it when it first built
        // the image: a load that took less found it built
        let built = Duration::from_millis(serde_json::from_slice::<Value>(&body).ok().and_then(|v| v["buildMs"].as_u64()).unwrap_or(0));
        let loaded = t0.elapsed() - saved;
        let disk = if loaded < built { "its disk built before".to_string() } else { format!("its disk built in {built:.1?}") };
        println!("      (the engine holds {reference}: {} MB saved in {saved:.1?}, loaded in {loaded:.1?}; {disk})", bytes >> 20);
    }
    Ok(())
}

/// The VMs a real engine runs at most (its health's `vms_max`): a node's
/// capacity in front of it.
pub fn real_engine_vms(engine_dir: &Path) -> Result<u32> {
    let (status, body) = engine_call(&engine_dir.join("engine.sock"), "GET", "/v1/health", None)?;
    anyhow::ensure!(status == 200, "the real engine's health: {status}");
    let v: Value = serde_json::from_slice(&body)?;
    v["vms_max"].as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0).context("the real engine's health names its vms_max")
}

/// One container as an engine lists it (sandcastle's `api::Info`).
struct Listed {
    name: String,
    running: bool,
    started_at_ms: u64,
}

/// The containers an engine holds (at most its VMs).
fn containers(sock: &Path) -> Result<Vec<Listed>> {
    let (status, body) = engine_call(sock, "GET", "/v1/containers", None)?;
    anyhow::ensure!(status == 200, "the engine's containers: {status}");
    let list: Value = serde_json::from_slice(&body)?;
    let all = list.as_array().cloned().or_else(|| list["containers"].as_array().cloned()).unwrap_or_default();
    Ok(all
        .iter()
        .filter_map(|c| Some(Listed { name: c["name"].as_str()?.to_string(), running: c["running"].as_bool().unwrap_or(false), started_at_ms: c["startedAtMs"].as_u64()? }))
        .collect())
}

/// Every container on a real engine started at or after `since_ms` (the
/// run's), destroyed.
fn destroy_since(sock: &Path, since_ms: u64) -> Result<()> {
    // bounded: the engine's containers (at most its VMs)
    for c in containers(sock)? {
        if c.started_at_ms >= since_ms {
            let _ = engine_call(sock, "POST", &format!("/v1/containers/{}/destroy", c.name), Some(EngineBody::Bytes(b"{}".to_vec())));
        }
    }
    Ok(())
}

/// The computer images of the cell project at `project` (its
/// `wrangler.jsonc`'s `containers` images, which wrangler builds for the
/// runtime's own containers: the e2e's staged copy, with the Hermes images
/// when its lane runs), built for this machine's architecture into local
/// Docker for its nodes to run, each tagged `fragment-<name>:e2e-<run>`:
/// (name, tag). Every layer a build before made is a cache hit.
pub fn build_cell_images(project: &Path, run: &str) -> Result<Vec<(String, String)>> {
    let mut config = crate::read_config(project)?;
    crate::absolute_images(&mut config, project)?;
    let platform = match std::env::consts::ARCH {
        "x86_64" => "linux/amd64",
        "aarch64" => "linux/arm64",
        other => bail!("no computer images for {other}"),
    };
    let mut built = vec![];
    // bounded: the images one config names
    for container in config["containers"].as_array().into_iter().flatten() {
        for (name, image) in container["images"].as_object().into_iter().flatten() {
            let tag = format!("fragment-{name}:e2e-{run}");
            let dockerfile = image["dockerfile"].as_str().context("an image names its dockerfile")?;
            let context = image["build_context"].as_str().context("an image names its build context")?;
            let mut cmd = Command::new("docker");
            cmd.args(["build", "--load", "--platform", platform, "--provenance=false", "-t", &tag, "-f", dockerfile]);
            for (k, v) in image["build_vars"].as_object().into_iter().flatten() {
                cmd.arg("--build-arg").arg(format!("{k}={}", v.as_str().with_context(|| format!("build var {k} is a string"))?));
            }
            let out = cmd.arg(context).stdin(Stdio::null()).output().context("run docker build (is Docker running, and may this user reach it?)")?;
            if !out.status.success() {
                bail!("docker build of {name} failed: {}\n{}{}", out.status, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            }
            built.push((name.clone(), tag));
        }
    }
    Ok(built)
}

/// The nodes as `FRAGMENT_NODES` lists them, with `images` (name to its
/// reference, every architecture), and their secrets.
pub fn vars(nodes: &[&SandcastleNode], images: &Value) -> Result<NodesVars> {
    let list = json!({ "nodes": nodes.iter().map(|n| n.listing()).collect::<Vec<_>>(), "images": images });
    let text = list.to_string();
    Nodes::parse(&text, Byoc::Off).map_err(|e| anyhow::anyhow!("the nodes started here: {e}"))?;
    Ok(NodesVars { nodes: text, secrets: nodes.iter().map(|n| (placement::secret_name(&n.id), n.secret.clone())).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(reach: Reach) -> NodeSpec {
        NodeSpec { id: "uplink".into(), reach, engine: Engine::Double, dir: PathBuf::from("/tmp/n"), platform: "http://127.0.0.1:9000".into(), capacity: 8, log_dir: PathBuf::from("/tmp") }
    }

    /// Goal: a node that listens is given its port, one that dials the
    /// platform's uplink as a WebSocket URL and its id; both the engine's
    /// sockets, their secret's file and the platform.
    #[test]
    fn a_node_is_configured_to_listen_or_dial() {
        let listen = node_config(&spec(Reach::Listen(9401))).unwrap();
        assert_eq!(listen["listen"], "127.0.0.1:9401");
        assert!(listen.get("uplink").is_none());
        assert_eq!(listen["engine"], "/tmp/n/e/engine.sock");
        assert_eq!(listen["secret_file"], "/tmp/n/node.secret");
        let dial = node_config(&spec(Reach::Uplink)).unwrap();
        assert!(dial.get("listen").is_none());
        assert_eq!(dial["uplink"], json!({ "url": "ws://127.0.0.1:9000/api/nodes/uplink", "id": "uplink" }));
        assert_eq!(dial["platform"], "http://127.0.0.1:9000");
        // in front of a real engine, its own sockets
        let real = node_config(&NodeSpec { engine: Engine::Real(PathBuf::from("/var/lib/sandcastle")), ..spec(Reach::Listen(9401)) }).unwrap();
        assert_eq!(real["engine"], "/var/lib/sandcastle/engine.sock");
        assert_eq!(real["ports"], "/var/lib/sandcastle/ports.sock");
        assert_eq!(real["secret_file"], "/tmp/n/node.secret");
    }

    /// Goal: a socket's path that fits is given back; one past `sun_path`
    /// (a deep checkout's scratch, as the e2e's impostor's was) is refused,
    /// saying which and why, before a node is handed it.
    #[test]
    fn a_socket_path_fits_sun_path_or_is_refused() {
        let fits = PathBuf::from(format!("/{}", "d".repeat(SOCKET_PATH_MAX - "/egress.sock".len() - 1)));
        assert_eq!(socket_path(&fits, "egress.sock").unwrap().as_os_str().len(), SOCKET_PATH_MAX);
        let long = PathBuf::from(format!("/{}", "d".repeat(SOCKET_PATH_MAX - "/egress.sock".len())));
        let e = socket_path(&long, "egress.sock").unwrap_err().to_string();
        assert!(e.contains("108 bytes") && e.contains("egress.sock"), "{e}");
        let deep = NodeSpec { dir: long, ..spec(Reach::Uplink) };
        assert!(node_config(&deep).is_err());
    }

    /// Goal: a chunked answer is joined; one cut short is refused.
    #[test]
    fn chunked_answers() {
        assert_eq!(unchunk(b"4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n").unwrap(), b"Wikipedia");
        assert!(unchunk(b"9\r\nWiki\r\n").is_err());
        assert!(unchunk(b"zz\r\n").is_err());
    }

    /// Goal: a node list from a file keeps the secrets out of the variable,
    /// names each as the cell looks for it, and is refused as the cell
    /// would refuse it. Method: files written here, good and bad.
    #[test]
    fn a_node_list_from_a_file() {
        let dir = std::env::temp_dir().join(format!("fragment-nodes-file-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let secret = dir.join("box.secret");
        fs::write(&secret, format!("{}\n", "s".repeat(40))).unwrap();
        let short = dir.join("short.secret");
        fs::write(&short, "short").unwrap();
        let list = |secret_file: &Path, arch: &str| {
            json!({
                "nodes": [{ "id": "box", "url": "http://192.168.50.7:9400", "arch": arch, "capacity": 4, "secret_file": secret_file }],
                "images": { "stub": "docker.io/library/fragment-stub:3" }
            })
        };
        let file = dir.join("nodes.json");
        fs::write(&file, list(&secret, "x86_64").to_string()).unwrap();
        let vars = NodesFile::read(&file).unwrap();
        assert_eq!(vars.secrets, vec![("FRAGMENT_NODE_SECRET_BOX".to_string(), "s".repeat(40))]);
        assert!(!vars.nodes.contains("secret"), "{}", vars.nodes);
        Nodes::parse(&vars.nodes, Byoc::Off).unwrap();
        fs::write(&file, list(&short, "x86_64").to_string()).unwrap();
        assert!(NodesFile::read(&file).unwrap_err().to_string().contains("at least 32 bytes"));
        fs::write(&file, list(&secret, "sparc").to_string()).unwrap();
        assert!(NodesFile::read(&file).is_err());
        fs::write(&file, list(&dir.join("missing"), "x86_64").to_string()).unwrap();
        assert!(NodesFile::read(&file).unwrap_err().to_string().contains("secret_file"));
        let _ = fs::remove_dir_all(&dir);
    }
}
