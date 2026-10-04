//! The local stack: one `wrangler dev` process serving the platform Worker
//! (`cell/`) and the agents' Worker (`agent/`) together, with each one's
//! variables and secrets rendered into its `.dev.vars`. `xtask dev` runs it
//! in the foreground; the e2e starts, crashes, and restarts it.

use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

pub mod celld;
pub mod codestore;
pub mod node;
mod node_release;
pub mod summary;

/// A running node, on either runtime.
pub enum AnyNode {
    Wrangler(Node),
    Celld(celld::CelldNode),
}

impl AnyNode {
    pub fn stop(self) -> Result<()> {
        match self {
            AnyNode::Wrangler(n) => n.stop(),
            AnyNode::Celld(n) => n.stop(),
        }
    }

    pub fn crash(self) -> Result<()> {
        match self {
            AnyNode::Wrangler(n) => n.crash(),
            AnyNode::Celld(n) => n.crash(),
        }
    }

    /// This boot's log.
    pub fn log(&self) -> &Path {
        match self {
            AnyNode::Wrangler(n) => &n.log,
            AnyNode::Celld(n) => &n.log,
        }
    }
}

/// A node must announce "ready" within this: wrangler builds the computer
/// images first (a cold build of the stub compiles its bridge in Docker).
pub const READY_TIMEOUT: Duration = Duration::from_secs(900);
/// A graceful stop must finish within this.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(60);

/// The wrangler the repo pins (package.json; the pinned Node's `npm ci`
/// installs it).
pub const WRANGLER_VERSION: &str = "4.145.0";
/// Names another wrangler entry script (a `bin/wrangler.js`), run on the
/// pinned Node all the same.
pub const WRANGLER_BIN_VAR: &str = "WRANGLER_BIN";
/// The pinned Node, unpacked (node.rs), under the repo root.
pub const TOOLS_DIR: &str = "target/tools";
/// The caches every JavaScript process keeps, under the repo root:
/// `XDG_CACHE_HOME` (miniflare's Chrome for Testing, in `.wrangler/chrome`),
/// wrangler's own (`wrangler/`), and npm's (`npm/`).
pub const CACHE_DIR: &str = "target/cache";

/// What a branch deployment's name may be (`cargo xtask deploy --branch`,
/// and the hosted e2e's): a DNS label short enough that
/// `<label>--<username>--<branch>` fits in one (63 bytes).
pub fn valid_branch(b: &str) -> bool {
    (1..=16).contains(&b.len())
        && b.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        && !b.starts_with('-')
        && !b.ends_with('-')
        && !b.contains("--")
}

pub fn repo_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.parent().and_then(Path::parent).expect("crates/devstack sits two levels below the repo root").to_path_buf()
}

pub fn cell_dir() -> PathBuf {
    repo_root().join("cell")
}

/// The agents' Worker project (goose's loop; phase 5).
pub fn agent_dir() -> PathBuf {
    repo_root().join("agent")
}

/// `text` (JSONC: JSON with `//` comments) as JSON.
pub fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut in_string, mut escaped) = (false, false);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
        } else if c == '"' {
            in_string = true;
            out.push(c);
        } else if c == '/' && chars.peek() == Some(&'/') {
            // to the end of the line, which stays
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A project's wrangler config, parsed.
pub fn read_config(project: &Path) -> Result<serde_json::Value> {
    let path = project.join("wrangler.jsonc");
    let text = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&strip_comments(&text)).with_context(|| format!("parse {}", path.display()))
}

/// A copy of a built project at `dir`: its config (without its `build`
/// step: a staged copy has no source to build), its `files`, and its
/// `build/`, so a node run from it keeps its state and variables apart
/// from the source tree, where `xtask dev` runs.
fn stage(from: &Path, dir: &Path, files: &[&str]) -> Result<PathBuf> {
    fn copy_dir(from: &Path, to: &Path) -> Result<()> {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            let target = to.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                copy_dir(&entry.path(), &target)?;
            } else {
                fs::copy(entry.path(), &target)?;
            }
        }
        Ok(())
    }
    fs::create_dir_all(dir)?;
    let mut config = read_config(from)?;
    config.as_object_mut().context("a wrangler config is an object")?.remove("build");
    absolute_images(&mut config, from)?;
    fs::write(dir.join("wrangler.jsonc"), serde_json::to_string_pretty(&config)?)?;
    for f in files {
        fs::copy(from.join(f), dir.join(f)).with_context(|| format!("stage {f}"))?;
    }
    let _ = fs::remove_dir_all(dir.join("build"));
    copy_dir(&from.join("build"), &dir.join("build")).with_context(|| format!("stage {}/build (run `cargo xtask build`)", from.display()))?;
    Ok(dir.to_path_buf())
}

/// A wrangler config's container images with their Dockerfiles and build
/// contexts as absolute paths (they name paths beside `from`, the config's
/// own directory), so the config works from anywhere.
pub fn absolute_images(config: &mut serde_json::Value, from: &Path) -> Result<()> {
    if let Some(containers) = config.get_mut("containers").and_then(|c| c.as_array_mut()) {
        for c in containers {
            let Some(images) = c.get_mut("images").and_then(|i| i.as_object_mut()) else { continue };
            for image in images.values_mut() {
                for key in ["dockerfile", "build_context"] {
                    if let Some(rel) = image[key].as_str().filter(|p| p.starts_with('.')) {
                        let abs = std::path::absolute(from.join(rel)).with_context(|| format!("resolve {rel}"))?;
                        image[key] = serde_json::Value::String(abs.display().to_string());
                    }
                }
            }
        }
    }
    Ok(())
}

/// Bindings `wrangler dev` always runs against Cloudflare itself: it starts
/// a remote session for them as it boots, which takes a login and an
/// account, so a local node runs without them. The model route's AI
/// binding is one; dev and the e2e point the route at the Workers AI fake
/// instead (`FRAGMENT_AI_URL`), and the cell never reads the binding then.
const REMOTE_ONLY_BINDINGS: [&str; 1] = ["ai"];

/// The project's config as a local node runs it: `wrangler.local.jsonc`,
/// beside its own (so its paths resolve the same), less the remote-only
/// bindings.
fn local_config(project: &Path, containers: bool) -> Result<PathBuf> {
    let mut config = read_config(project)?;
    let obj = config.as_object_mut().context("a wrangler config is an object")?;
    for b in REMOTE_ONLY_BINDINGS {
        obj.remove(b);
    }
    // computers on a sandcastle node (docs/self-host.md, seam 2): the
    // runtime's own containers, and Docker for them, go unused
    if !containers {
        obj.remove("containers");
    }
    let path = project.join("wrangler.local.jsonc");
    fs::write(&path, serde_json::to_string_pretty(&config)?)?;
    Ok(path)
}

/// A copy of the built agent project at `dir`.
pub fn stage_agent(dir: &Path) -> Result<PathBuf> {
    stage(&agent_dir(), dir, &[])
}

/// A copy of the built cell project at `dir` (its config, shim, and build).
pub fn stage_project(dir: &Path) -> Result<PathBuf> {
    stage(&cell_dir(), dir, &["entry.mjs", "storage.mjs", "node.mjs", "uplink.mjs"])
}

/// What the node runs on: the pinned Node and the wrangler it runs.
pub struct Tools {
    pub node: node::Node,
    /// wrangler's entry script: `WRANGLER_BIN`'s, or the pinned one npm
    /// installed (`node_modules/wrangler/bin/wrangler.js`).
    pub wrangler: PathBuf,
    /// `CACHE_DIR`, absolute.
    pub cache: PathBuf,
}

impl Tools {
    /// The pinned Node (or `FRAGMENT_NODE`'s), fetched on first use;
    /// node_modules from its own `npm ci` when missing or stale; and the
    /// wrangler package.json pins, or `WRANGLER_BIN`'s, checked by version.
    /// Nothing from PATH.
    pub fn locate() -> Result<Tools> {
        let root = repo_root();
        let cache = root.join(CACHE_DIR);
        let node = node::locate(&root.join(TOOLS_DIR))?;
        node::ensure_modules(&node, &root, &root.join(TOOLS_DIR), &cache)?;
        let wrangler = match std::env::var_os(WRANGLER_BIN_VAR) {
            Some(p) => PathBuf::from(p),
            None => root.join("node_modules/wrangler/bin/wrangler.js"),
        };
        if !wrangler.is_file() {
            bail!("no wrangler entry script at {} ({WRANGLER_BIN_VAR} names one, a wrangler package's bin/wrangler.js; unset, it is the one npm ci installs)", wrangler.display());
        }
        let tools = Tools { node, wrangler, cache };
        let out = tools
            .wrangler()?
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("run {} {} --version", tools.node.node.display(), tools.wrangler.display()))?;
        let version = String::from_utf8_lossy(&out.stdout);
        if !version.contains(WRANGLER_VERSION) {
            bail!("wrangler {WRANGLER_VERSION} is required (package.json), found {:?} ({})", version.trim(), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(tools)
    }

    /// `<node> <wrangler.js>` in the environment every JavaScript process
    /// here gets (`node::Node::script`), with wrangler's own cache in
    /// `CACHE_DIR` too (`WRANGLER_CACHE_DIR`) and no metrics sent.
    pub fn wrangler(&self) -> Result<Command> {
        let mut cmd = self.node.script(&self.wrangler, &self.cache)?;
        cmd.env("WRANGLER_CACHE_DIR", self.cache.join("wrangler")).env("WRANGLER_SEND_METRICS", "false");
        Ok(cmd)
    }
}

/// A free local port.
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Writes a project's `.dev.vars` (dotenv, one line per value, mode 600).
pub fn write_dev_vars(project: &Path, vars: &[(&str, &str)]) -> Result<()> {
    let path = project.join(".dev.vars");
    let mut f = fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&path)?;
    for (k, v) in vars {
        assert!(!k.is_empty() && k.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'), "var name {k}");
        writeln!(f, "{k}={}", v.replace('\n', "\\n"))?;
    }
    Ok(())
}

/// What a local deployment is configured with. The cell reads its
/// settings as Worker variables and its keys as Worker secrets
/// (cell/src/keys.rs); under `wrangler dev` both come from `.dev.vars`
/// (mode 600). A dev or test deployment points code.storage at the fake in
/// `crates/fakes`.
pub struct Fleet {
    pub host_secret: String,
    pub codestorage_org: String,
    pub codestorage_key_pem: String,
    pub codestorage_url: String,
    /// Fragments are served from `<label>--<username>.<suffix>` when set.
    pub host_suffix: Option<String>,
    /// Where fragments were served before the suffix moved: a fragment's
    /// host there redirects to its host under the suffix.
    pub legacy_host_suffix: Option<String>,
    /// A branch deployment's mark on its fragments' hosts (`--<branch>`:
    /// `<label>--<username>--<branch>.<suffix>`), which also scopes its
    /// test levers to the e2e's own things (the hosted lane's rehearsal).
    pub host_label_suffix: Option<String>,
    pub poll_interval_s: u32,
    /// Jobs may fetch loopback and private addresses (the local fakes).
    pub egress_local: bool,
    /// The first retry delay of a failed job step (it doubles each time).
    pub job_retry_delay_s: u32,
    /// How long a blob no branch names is kept (`None`: the cell's 7 days).
    pub blob_grace_s: Option<u32>,
    /// Where the model route sends its calls, text and images
    /// (`FRAGMENT_AI_URL`: the Workers AI fake in dev and the e2e; `None`:
    /// the AI binding, through `ai_gateway`).
    pub ai_url: Option<String>,
    pub ai_gateway: Option<String>,
    /// A new person's plan (`FRAGMENT_DEFAULT_PLAN`; `None`: the cell's, guest).
    pub default_plan: Option<String>,
    /// The wait before a delivery is retried, every time (`None`: the
    /// cell's, 10 s growing with the delivery's age to an hour).
    pub delivery_retry_s: Option<u32>,
    /// Sign-in: WorkOS AuthKit (the real one, or the fake in `crates/fakes`).
    pub workos: Option<WorkOsVars>,
    /// Sign-in with an OpenID Connect provider (docs/self-host.md, seam 4):
    /// it signs people in in place of WorkOS, which then serves Pipes alone.
    pub oidc: Option<OidcVars>,
    /// The platform's origin (sign-in, the platform session), when it is
    /// not the hostname suffix itself.
    pub platform_url: Option<String>,
    /// Who may grant credit and set plans (`FRAGMENT_OPERATORS`).
    pub operators: Option<String>,
    /// Pending sign-ins the Registry keeps (`None`: the cell's default,
    /// `limits::SIGNINS_PENDING_MAX_DEFAULT`).
    pub signins_pending_max: Option<u64>,
    /// The test levers' secret (`FRAGMENT_TEST_SECRET`: `/api/test/*`
    /// answers requests that carry it; the registry can be made to fail).
    /// The e2e's, made per run; never dev's.
    pub test_secret: Option<String>,
    /// The image new computers are pinned to (a name in cell/wrangler.jsonc's
    /// `containers` images), and whether they sleep with a snapshot.
    pub computer_image: Option<String>,
    pub computer_snapshots: bool,
    /// What a computer's swap offers: the provider catalog
    /// (`FRAGMENT_PROVIDERS`, `fragment_core::catalog`'s JSON: connections,
    /// operator keys and own keys, each with its hosts, placements,
    /// environment variables and a key's price), and the operator keys'
    /// values. `swap_upstream` (tests only) takes every swapped request in
    /// place of its host.
    pub providers: Option<String>,
    pub operator_key_values: Vec<(String, String)>,
    pub swap_upstream: Option<String>,
    /// A self-hosted model upstream (docs/self-host.md, seam 3), in place
    /// of `ai_url` and the gateway.
    pub model_upstream: Option<ModelUpstreamVars>,
    /// A sandcastle node new computers run on (docs/self-host.md, seam 2).
    pub node: Option<NodeVars>,
}

/// An OpenAI-compatible model server as the cell reads it.
pub struct ModelUpstreamVars {
    /// Its base, with its `/v1`.
    pub url: String,
    /// `FRAGMENT_MODELS`' JSON: catalog id to the server's model.
    pub models: String,
    pub key: Option<String>,
}

/// A sandcastle node as the cell reads it.
pub struct NodeVars {
    pub url: String,
    pub secret: String,
    /// `FRAGMENT_NODE_IMAGES`' JSON: an image's name to its reference.
    pub images: String,
}

/// An OpenID Connect provider as the cell reads it (`FRAGMENT_OIDC_*`).
pub struct OidcVars {
    /// Its issuer, exactly as its id_tokens say it.
    pub issuer: String,
    pub client_id: String,
    /// `None`: a public client (PKCE alone).
    pub client_secret: Option<String>,
    /// `FRAGMENT_OIDC_SCOPES`, `FRAGMENT_OIDC_CLAIMS` (JSON) and
    /// `FRAGMENT_OIDC_AUTH`; `None`: the cell's defaults.
    pub scopes: Option<String>,
    pub claims: Option<String>,
    pub auth: Option<String>,
}

/// A WorkOS environment as the cell reads it.
pub struct WorkOsVars {
    pub client_id: String,
    pub api_key: String,
    /// `None`: WorkOS itself.
    pub api_url: Option<String>,
}

impl Fleet {
    /// Renders the deployment's settings and secrets into the project's
    /// `.dev.vars`.
    pub fn configure(&self, project: &Path) -> Result<()> {
        let poll = self.poll_interval_s.to_string();
        let retry = self.job_retry_delay_s.to_string();
        let mut vars = vec![
            ("FRAGMENT_HOST_SECRET", self.host_secret.as_str()),
            ("CODESTORAGE_PRIVATE_KEY", self.codestorage_key_pem.as_str()),
            ("CODESTORAGE_ORG", self.codestorage_org.as_str()),
            ("CODESTORAGE_API_URL", self.codestorage_url.as_str()),
            ("FRAGMENT_POLL_INTERVAL_S", poll.as_str()),
            ("FRAGMENT_JOB_RETRY_DELAY_S", retry.as_str()),
        ];
        if self.egress_local {
            vars.push(("FRAGMENT_EGRESS_LOCAL", "allow"));
        }
        let grace = self.blob_grace_s.map(|g| g.to_string());
        if let Some(g) = &grace {
            vars.push(("FRAGMENT_BLOB_GRACE_S", g.as_str()));
        }
        if let Some(u) = &self.ai_url {
            vars.push(("FRAGMENT_AI_URL", u.as_str()));
        }
        if let Some(g) = &self.ai_gateway {
            vars.push(("AI_GATEWAY_ID", g.as_str()));
        }
        if let Some(p) = &self.default_plan {
            vars.push(("FRAGMENT_DEFAULT_PLAN", p.as_str()));
        }
        let retry = self.delivery_retry_s.map(|r| r.to_string());
        if let Some(r) = &retry {
            vars.push(("FRAGMENT_DELIVERY_RETRY_S", r.as_str()));
            vars.push(("FRAGMENT_DELIVERY_RETRY_MAX_S", r.as_str()));
        }
        if let Some(s) = &self.host_suffix {
            vars.push(("FRAGMENT_HOST_SUFFIX", s.as_str()));
        }
        if let Some(s) = &self.legacy_host_suffix {
            vars.push(("FRAGMENT_LEGACY_HOST_SUFFIX", s.as_str()));
        }
        if let Some(s) = &self.host_label_suffix {
            vars.push(("FRAGMENT_HOST_LABEL_SUFFIX", s.as_str()));
        }
        if let Some(w) = &self.workos {
            vars.push(("WORKOS_CLIENT_ID", w.client_id.as_str()));
            vars.push(("WORKOS_API_KEY", w.api_key.as_str()));
            if let Some(u) = &w.api_url {
                vars.push(("WORKOS_API_URL", u.as_str()));
            }
        }
        if let Some(o) = &self.oidc {
            vars.push(("FRAGMENT_OIDC_ISSUER", o.issuer.as_str()));
            vars.push(("FRAGMENT_OIDC_CLIENT_ID", o.client_id.as_str()));
            for (k, v) in [("FRAGMENT_OIDC_CLIENT_SECRET", &o.client_secret), ("FRAGMENT_OIDC_SCOPES", &o.scopes), ("FRAGMENT_OIDC_CLAIMS", &o.claims), ("FRAGMENT_OIDC_AUTH", &o.auth)] {
                if let Some(v) = v {
                    vars.push((k, v.as_str()));
                }
            }
        }
        if let Some(p) = &self.platform_url {
            vars.push(("FRAGMENT_PLATFORM_URL", p.as_str()));
        }
        if let Some(o) = &self.operators {
            vars.push(("FRAGMENT_OPERATORS", o.as_str()));
        }
        let signins = self.signins_pending_max.map(|n| n.to_string());
        if let Some(n) = &signins {
            vars.push(("FRAGMENT_SIGNINS_PENDING_MAX", n.as_str()));
        }
        if let Some(secret) = &self.test_secret {
            vars.push(("FRAGMENT_TEST_SECRET", secret.as_str()));
        }
        if let Some(image) = &self.computer_image {
            vars.push(("FRAGMENT_COMPUTER_IMAGE", image.as_str()));
        }
        if !self.computer_snapshots {
            vars.push(("FRAGMENT_COMPUTER_SNAPSHOTS", "off"));
        }
        if let Some(p) = &self.providers {
            vars.push(("FRAGMENT_PROVIDERS", p.as_str()));
        }
        let key_names: Vec<String> = self.operator_key_values.iter().map(|(name, _)| fragment_core::catalog::key_secret_name(name)).collect();
        for ((_, value), name) in self.operator_key_values.iter().zip(&key_names) {
            vars.push((name.as_str(), value.as_str()));
        }
        if let Some(u) = &self.swap_upstream {
            vars.push(("FRAGMENT_SWAP_UPSTREAM", u.as_str()));
        }
        if let Some(m) = &self.model_upstream {
            vars.push(("FRAGMENT_MODEL_URL", m.url.as_str()));
            vars.push(("FRAGMENT_MODELS", m.models.as_str()));
            if let Some(k) = &m.key {
                vars.push(("FRAGMENT_MODEL_KEY", k.as_str()));
            }
        }
        if let Some(n) = &self.node {
            vars.push(("FRAGMENT_NODE_URL", n.url.as_str()));
            vars.push(("FRAGMENT_NODE_SECRET", n.secret.as_str()));
            vars.push(("FRAGMENT_NODE_IMAGES", n.images.as_str()));
        }
        write_dev_vars(project, &vars)
    }
}

/// What an agent fleet is configured with: the platform it acts on (its
/// model calls are the platform's model route's), and its own host secret.
pub struct AgentFleet {
    pub host_secret: String,
    /// The fragment platform's base URL (`FRAGMENT_API`).
    pub fragment_api: String,
    /// The agent fleet's own base URL (`AGENT_URL`): the inboxes it gives
    /// fragments to deliver to.
    pub agent_url: String,
    /// The owner's test controls (holds, the watchdog period): dev and e2e only.
    pub test_hooks: bool,
}

impl AgentFleet {
    /// Renders the fleet into the project's `.dev.vars`.
    pub fn configure(&self, project: &Path) -> Result<()> {
        let mut vars = vec![
            ("FRAGMENT_HOST_SECRET", self.host_secret.as_str()),
            ("FRAGMENT_API", self.fragment_api.as_str()),
            ("AGENT_URL", self.agent_url.as_str()),
        ];
        if self.test_hooks {

            vars.push(("AGENT_TEST_HOOKS", "allow"));
        }
        write_dev_vars(project, &vars)
    }
}

/// Random hex from the OS.
pub fn random_hex(bytes: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; bytes];
    fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).expect("read /dev/urandom");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// A dev-only secret kept in `target/devstack/<name>` (mode 600), made on
/// first use: restarts of `xtask dev` keep cells and repos readable.
pub fn dev_secret(name: &str, make: impl FnOnce() -> String) -> Result<String> {
    let dir = repo_root().join("target/devstack");
    fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    if let Ok(s) = fs::read_to_string(&path) {
        return Ok(s);
    }
    let value = make();
    let mut f = fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&path)?;
    f.write_all(value.as_bytes())?;
    Ok(value)
}

pub struct NodeOptions {
    /// The platform Worker's project: `cell/` for `xtask dev`, a staged
    /// copy for the e2e. Its state (`.wrangler/state`) lives there.
    pub project: PathBuf,
    pub port: u16,
    /// Discard the local state first.
    pub clean: bool,
    /// Projects run beside it for its service bindings (`wrangler dev -c`
    /// again): the agents' Worker.
    pub with: Vec<PathBuf>,
    /// Where each boot's log goes (`node-<port>-<boot>.log`).
    pub log_dir: PathBuf,
    /// wrangler's own debug logs in the log, beside the Workers' output.
    pub node_logs: bool,
    /// A process group of its own, so the e2e crashes wrangler and workerd
    /// as one. `xtask dev` keeps it in the terminal's, so Ctrl-C reaches it
    /// as it reaches xtask: in a group of its own, it outlived a Ctrl-C,
    /// holding its port.
    pub own_group: bool,
    /// The runtime's containers (Docker under `wrangler dev`); `false` when
    /// computers run on a sandcastle node instead.
    pub containers: bool,
}

/// Boots of one port whose logs one directory keeps; a start past this
/// asks for the directory to be cleared rather than scanning without end.
pub const BOOT_LOGS_MAX: u32 = 10_000;

/// A new log for a boot on `port`: `node-<port>-<boot>.log`, the first
/// boot number `dir` has no log for. A node started again on its port (the
/// e2e's restarts) never truncates the log of the one before, which a
/// crash leaves there, and its own `Ready on` is the only one in its file.
pub(crate) fn boot_log(dir: &Path, port: u16) -> Result<(PathBuf, fs::File)> {
    fs::create_dir_all(dir)?;
    for boot in 1..=BOOT_LOGS_MAX {
        let path = dir.join(format!("node-{port}-{boot}.log"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
        }
    }
    bail!("{} holds {BOOT_LOGS_MAX} logs of nodes on :{port} already; clear it", dir.display())
}

/// Where a project's local state lives: its Durable Objects' SQLite, R2,
/// Workflows and queues, as workerd persists them.
pub fn state_dir(project: &Path) -> PathBuf {
    project.join(".wrangler/state")
}

/// One `wrangler dev` process, which runs workerd as its child: in a
/// process group of its own (`NodeOptions::own_group`), so a crash kills
/// both, or in the terminal's.
pub struct Node {
    child: Child,
    own_group: bool,
    pub base: String,
    pub port: u16,
    /// This boot's log.
    pub log: PathBuf,
    reaped: bool,
}

impl Node {
    pub fn start(tools: &Tools, opts: &NodeOptions) -> Result<(Node, Duration)> {
        if opts.clean {
            let state = state_dir(&opts.project);
            if state.exists() {
                fs::remove_dir_all(&state).with_context(|| format!("clear {}", state.display()))?;
            }
        }
        let (log, out) = boot_log(&opts.log_dir, opts.port)?;
        let mut cmd = tools.wrangler()?;
        cmd.arg("dev").arg("-c").arg(local_config(&opts.project, opts.containers)?);
        for other in &opts.with {
            cmd.arg("-c").arg(other.join("wrangler.jsonc"));
        }
        cmd.args(["--ip", "127.0.0.1", "--port", &opts.port.to_string()]);
        cmd.args(["--inspector-port", &free_port()?.to_string()]);
        cmd.arg("--persist-to").arg(state_dir(&opts.project));
        cmd.args(["--show-interactive-dev-session=false", "--log-level", if opts.node_logs { "debug" } else { "log" }]);
        // its own dev registry: a crashed node's entries (or another
        // stack's Workers of the same names) never stand in for its own
        cmd.current_dir(&opts.project)
            .env("WRANGLER_LOG_PATH", &opts.log_dir)
            .env("WRANGLER_REGISTRY_PATH", opts.project.join(".wrangler/registry"));
        if opts.own_group {
            cmd.process_group(0);
        }
        let t0 = Instant::now();
        let child = cmd.stdout(out.try_clone()?).stderr(out).stdin(Stdio::null()).spawn()?;
        let mut node = Node { child, own_group: opts.own_group, base: format!("http://127.0.0.1:{}", opts.port), port: opts.port, log: log.clone(), reaped: false };
        loop {
            let text = fs::read(&log).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            if text.contains(&format!("Ready on http://127.0.0.1:{}", opts.port)) {
                break;
            }
            if let Some(status) = node.child.try_wait()? {
                node.reaped = true;
                bail!("wrangler dev exited ({status}) before it was ready:\n{text}");
            }
            if t0.elapsed() > READY_TIMEOUT {
                bail!("wrangler dev on :{} was not ready after {READY_TIMEOUT:?}:\n{text}", opts.port);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(TcpStream::connect(("127.0.0.1", opts.port)).is_ok(), "a ready node listens");
        Ok((node, t0.elapsed()))
    }

    /// Signals its group when it has one of its own, else wrangler alone
    /// (which stops workerd).
    fn signal_group(&self, signal: &str) -> Result<()> {
        let target = if self.own_group { format!("-{}", self.child.id()) } else { self.child.id().to_string() };
        let status = Command::new("kill").args([signal, "--", &target]).status()?;
        anyhow::ensure!(status.success(), "kill {signal} delivered to the node");
        Ok(())
    }

    /// SIGINT, as Ctrl-C stops it: wrangler shuts workerd down, and every
    /// write already answered is on disk.
    pub fn stop(mut self) -> Result<()> {
        self.signal_group("-INT")?;
        let t0 = Instant::now();
        while self.child.try_wait()?.is_none() {
            if t0.elapsed() > STOP_TIMEOUT {
                self.kill();
                bail!("wrangler dev did not stop within {STOP_TIMEOUT:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.reaped = true;
        // workerd may outlive wrangler by a moment; the port must be free for the next node
        self.kill_group();
        Ok(())
    }

    /// SIGKILL the node and workerd: a crash, with no graceful shutdown.
    pub fn crash(mut self) -> Result<()> {
        self.kill();
        Ok(())
    }

    /// Waits for `wrangler dev` to exit (the foreground `xtask dev`).
    pub fn wait(mut self) -> Result<std::process::ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        self.kill_group();
        Ok(status)
    }

    /// SIGKILL to its group, when it has one of its own (workerd may outlive
    /// wrangler in it). Without one there is no group to reach, and a
    /// reaped process's id is never signalled.
    fn kill_group(&self) {
        if self.own_group {
            let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", self.child.id())]).stderr(Stdio::null()).status();
        }
    }

    fn kill(&mut self) {
        if !self.reaped {
            self.kill_group();
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.reaped = true;
        }
    }
}

impl Drop for Node {
    /// An early error must not leave a node holding the port. A reaped
    /// leader's group id is never signalled but by `stop` and `wait`, right
    /// after it exits, while workerd may still hold the group.
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node started again on its port gets a log of its own, numbered
    /// after the last, and the earlier boot's log keeps what it holds.
    #[test]
    fn each_boot_on_a_port_gets_its_own_log() {
        let dir = std::env::temp_dir().join(format!("devstack-boot-log-{}", random_hex(6)));
        let (first, mut file) = boot_log(&dir, 4321).expect("a first log");
        writeln!(file, "the first boot's last words").expect("write the first log");
        let (second, _) = boot_log(&dir, 4321).expect("a second log");
        let (other, _) = boot_log(&dir, 4322).expect("another port's log");
        assert_eq!(first, dir.join("node-4321-1.log"));
        assert_eq!(second, dir.join("node-4321-2.log"));
        assert_eq!(other, dir.join("node-4322-1.log"));
        assert_eq!(fs::read_to_string(&first).expect("read the first log"), "the first boot's last words\n");
        fs::remove_dir_all(&dir).expect("remove the test's directory");
    }

    /// wrangler runs as `<node> <wrangler.js>`, never through its
    /// `#!/usr/bin/env node`, with the pinned Node first on its PATH and
    /// every cache it and miniflare keep under the repo's target/cache.
    #[test]
    fn wrangler_runs_on_the_pinned_node_with_repo_caches() {
        let root = repo_root();
        let bin = root.join(TOOLS_DIR).join("node-test/bin");
        let node = node::Node { node: bin.join("node"), bin: bin.clone(), npm_cli: root.join("npm-cli.js"), release: node::pinned_release() };
        let tools = Tools { node, wrangler: root.join("node_modules/wrangler/bin/wrangler.js"), cache: root.join(CACHE_DIR) };
        let cmd = tools.wrangler().expect("a wrangler command");
        assert_eq!(cmd.get_program(), bin.join("node").as_os_str());
        assert_eq!(cmd.get_args().collect::<Vec<_>>(), [root.join("node_modules/wrangler/bin/wrangler.js").as_os_str()]);
        let env = |key: &str| cmd.get_envs().find(|(k, _)| *k == key).and_then(|(_, v)| v).map(PathBuf::from);
        assert_eq!(env("PATH").map(|p| std::env::split_paths(&p).next()), Some(Some(bin)));
        assert_eq!(env("XDG_CACHE_HOME"), Some(root.join("target/cache")));
        assert_eq!(env("WRANGLER_CACHE_DIR"), Some(root.join("target/cache/wrangler")));
        assert_eq!(env("WRANGLER_SEND_METRICS"), Some(PathBuf::from("false")));
    }

    /// Comments go, strings keep what looks like one, and the result is JSON.
    #[test]
    fn jsonc_comments_are_stripped_outside_strings() {
        let text = "{\n  // a comment\n  \"url\": \"http://a//b\", // trailing\n  \"q\": \"say \\\"//\\\"\"\n}";
        let v: serde_json::Value = serde_json::from_str(&strip_comments(text)).expect("JSON");
        assert_eq!(v["url"], "http://a//b");
        assert_eq!(v["q"], "say \"//\"");
        assert!(read_config(&cell_dir()).expect("cell/wrangler.jsonc parses")["durable_objects"].is_object());
    }
}
