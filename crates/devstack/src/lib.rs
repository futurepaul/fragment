//! The local stack: one `celld dev` node serving `cell/`, with its
//! variables rendered into `cell/.dev.vars`. `xtask dev` runs it in the
//! foreground; the e2e starts, crashes, and restarts it.

use std::fs;
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

/// A node must announce "ready" within this.
pub const READY_TIMEOUT: Duration = Duration::from_secs(120);
/// A graceful stop must finish within this.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(60);

/// The fork of celld (branch `hardening`): v0.5.1 with the alarm fix
/// (spikes/celld-0.5.1/README.md), public-only Worker egress
/// (`CELLD_EGRESS_PUBLIC_ONLY`, docs/phase-3.md slice E), the native-services
/// seam that serves `KEYS` from crates/native, and the settings
/// docs/hardening.md turns on: `CELLD_FACET_MAX_BYTES`,
/// `CELLD_LOADED_WORKERS_MAX`, `CELLD_DYNAMIC_LOCKDOWN`,
/// `CELLD_INTERNAL_PEER_ONLY`, and a hard heap ceiling.
pub const CELLD_FORK_URL: &str = "https://github.com/futurepaul/celld.git";
pub const CELLD_FORK_REV: &str = "f734f8f44054669a42f489edcb054c6fe06b07f1";

pub fn repo_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.parent().and_then(Path::parent).expect("crates/devstack sits two levels below the repo root").to_path_buf()
}

pub fn cell_dir() -> PathBuf {
    repo_root().join("cell")
}

/// The agents' celld project (goose's loop; phase 5).
pub fn agent_dir() -> PathBuf {
    repo_root().join("agent")
}

/// A copy of the built agent project at `dir` (its config and build).
pub fn stage_agent(dir: &Path) -> Result<PathBuf> {
    let agent = agent_dir();
    fs::create_dir_all(dir.join("build"))?;
    fs::copy(agent.join("wrangler.jsonc"), dir.join("wrangler.jsonc")).context("stage the agent's wrangler.jsonc")?;
    for f in ["index.js", "index_bg.wasm"] {
        fs::copy(agent.join("build").join(f), dir.join("build").join(f)).with_context(|| format!("stage agent/build/{f} (run `cargo xtask build`)"))?;
    }
    Ok(dir.to_path_buf())
}

/// A copy of the built cell project at `dir` (its config, shim, and build),
/// so a node run from it keeps its state and variables apart from `cell/`,
/// where `xtask dev` runs.
pub fn stage_project(dir: &Path) -> Result<PathBuf> {
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
    let cell = cell_dir();
    fs::create_dir_all(dir)?;
    for f in ["wrangler.jsonc", "entry.mjs"] {
        fs::copy(cell.join(f), dir.join(f)).with_context(|| format!("stage {f}"))?;
    }
    let _ = fs::remove_dir_all(dir.join("build"));
    copy_dir(&cell.join("build"), &dir.join("build")).context("stage cell/build (run `cargo xtask build`)")?;
    Ok(dir.to_path_buf())
}

/// Where `xtask celld` installs the fork's binary.
pub fn fork_celld_path() -> PathBuf {
    repo_root().join("target/celld/bin/celld")
}

/// The binaries the node needs.
pub struct Tools {
    pub celld: PathBuf,
    pub esbuild: PathBuf,
}

impl Tools {
    /// `CELLD_BIN` or the fork build; `CELLD_ESBUILD` or the esbuild
    /// worker-build downloads into its cache.
    pub fn locate() -> Result<Tools> {
        let celld = match std::env::var_os("CELLD_BIN") {
            Some(p) => PathBuf::from(p),
            None => fork_celld_path(),
        };
        if !celld.is_file() {
            bail!("no celld at {} (run `cargo xtask celld`, or set CELLD_BIN)", celld.display());
        }
        Ok(Tools { celld, esbuild: esbuild()? })
    }
}

/// The esbuild the node bundles with: `CELLD_ESBUILD`, or the one
/// worker-build downloads into its cache.
pub fn esbuild() -> Result<PathBuf> {
    match std::env::var_os("CELLD_ESBUILD") {
        Some(p) => Ok(PathBuf::from(p)),
        None => worker_build_esbuild(),
    }
}

fn worker_build_esbuild() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is unset")?;
    let cache = PathBuf::from(home).join("Library/Caches/worker-build");
    let mut found: Vec<PathBuf> = fs::read_dir(&cache)
        .with_context(|| format!("no worker-build cache at {} (run `cargo xtask build` first)", cache.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("esbuild-")))
        .collect();
    found.sort();
    let dir = found.pop().context("worker-build has not downloaded esbuild yet (run `cargo xtask build`)")?;
    let bin = if dir.is_file() { dir } else { dir.join("bin/esbuild") };
    if !bin.is_file() {
        bail!("no esbuild binary at {}", bin.display());
    }
    Ok(bin)
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

/// What a fleet is configured with (ROADMAP decision 13). The cell reads
/// the plain settings as Worker variables; the fleet's secrets go to the
/// node's environment, where only `KEYS` reads them (crates/native,
/// docs/hardening.md H1). A dev or test fleet points code.storage at the
/// fake in `crates/fakes`.
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
    pub poll_interval_s: u32,
    /// Jobs may fetch loopback and private addresses (the local fakes).
    pub egress_local: bool,
    /// The first retry delay of a failed job step (it doubles each time).
    pub job_retry_delay_s: u32,
    /// How long a blob no branch names is kept (`None`: the cell's 7 days).
    pub blob_grace_s: Option<u32>,
    /// Where AI calls go (`None`: OpenRouter itself).
    pub openrouter_url: Option<String>,
    /// The wait before a delivery is retried, every time (`None`: the
    /// cell's, 10 s growing with the delivery's age to an hour).
    pub delivery_retry_s: Option<u32>,
    /// Sign-in: WorkOS AuthKit (the real one, or the fake in `crates/fakes`).
    pub workos: Option<WorkOsVars>,
    /// The platform's origin (sign-in, the platform session), when it is
    /// not the hostname suffix itself.
    pub platform_url: Option<String>,
    /// Budgets: the OpenRouter management key that mints each person's key
    /// (`None`: only a fragment's own key pays), the monthly budget in
    /// dollars (`None`: the cell's 20), and who may top one up.
    pub openrouter_management: Option<String>,
    pub budget_usd: Option<String>,
    pub operators: Option<String>,
    /// Pending sign-ins the Registry keeps (`None`: the cell's default,
    /// `limits::SIGNINS_PENDING_MAX_DEFAULT`).
    pub signins_pending_max: Option<u64>,
    /// Test controls (`FRAGMENT_TEST_HOOKS=allow`: the registry can be made
    /// to fail). Never on a shared fleet.
    pub test_hooks: bool,
}

/// A WorkOS environment as the cell reads it.
pub struct WorkOsVars {
    pub client_id: String,
    pub api_key: String,
    /// `None`: WorkOS itself.
    pub api_url: Option<String>,
}

/// An app's database: the platform refuses a mutation past
/// `fragment_proto::limits::APP_DB_MAX_BYTES` (16 MiB); our celld fork stops
/// every write at this, 4 MiB above, so the runtime's own bookkeeping in
/// that database always has room.
pub const FACET_MAX_BYTES: u64 = 20 * 1024 * 1024;

/// The node's settings, as a fleet's (docs/hardening.md): `KEYS`'s secret,
/// the app database cap, loaded workers without `eval` or `Atomics.wait`,
/// and an internal listener that serves only fleet-signed peer routes.
fn node_env(host_secret: &str) -> Vec<(String, String)> {
    vec![
        ("FRAGMENT_KEYS_HOST_SECRET".into(), host_secret.into()),
        ("CELLD_FACET_MAX_BYTES".into(), FACET_MAX_BYTES.to_string()),
        ("CELLD_DYNAMIC_LOCKDOWN".into(), "1".into()),
        ("CELLD_INTERNAL_PEER_ONLY".into(), "1".into()),
    ]
}

impl Fleet {
    /// Renders the fleet's plain settings into the project's `.dev.vars`,
    /// and answers the node's environment: the fleet's secrets, for `KEYS`.
    pub fn configure(&self, project: &Path) -> Result<Vec<(String, String)>> {
        let mut env = node_env(&self.host_secret);
        env.push(("FRAGMENT_KEYS_CODESTORAGE_ORG".into(), self.codestorage_org.clone()));
        env.push(("FRAGMENT_KEYS_CODESTORAGE_PRIVATE_KEY".into(), self.codestorage_key_pem.clone()));
        if let Some(w) = &self.workos {
            env.push(("FRAGMENT_KEYS_WORKOS_API_KEY".into(), w.api_key.clone()));
            if let Some(u) = &w.api_url {
                env.push(("FRAGMENT_KEYS_WORKOS_URL".into(), u.clone()));
            }
        }
        if let Some(k) = &self.openrouter_management {
            env.push(("FRAGMENT_KEYS_OPENROUTER_MANAGEMENT_KEY".into(), k.clone()));
            if let Some(u) = &self.openrouter_url {
                env.push(("FRAGMENT_KEYS_OPENROUTER_URL".into(), u.clone()));
            }
        }
        let poll = self.poll_interval_s.to_string();
        let retry = self.job_retry_delay_s.to_string();
        let mut vars = vec![
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
        if let Some(u) = &self.openrouter_url {
            vars.push(("OPENROUTER_API_URL", u.as_str()));
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
        if let Some(w) = &self.workos {
            vars.push(("WORKOS_CLIENT_ID", w.client_id.as_str()));
            if let Some(u) = &w.api_url {
                vars.push(("WORKOS_API_URL", u.as_str()));
            }
        }
        if let Some(p) = &self.platform_url {
            vars.push(("FRAGMENT_PLATFORM_URL", p.as_str()));
        }
        if let Some(b) = &self.budget_usd {
            vars.push(("FRAGMENT_BUDGET_USD", b.as_str()));
        }
        if let Some(o) = &self.operators {
            vars.push(("FRAGMENT_OPERATORS", o.as_str()));
        }
        let signins = self.signins_pending_max.map(|n| n.to_string());
        if let Some(n) = &signins {
            vars.push(("FRAGMENT_SIGNINS_PENDING_MAX", n.as_str()));
        }
        if self.test_hooks {
            vars.push(("FRAGMENT_TEST_HOOKS", "allow"));
        }
        write_dev_vars(project, &vars)?;
        Ok(env)
    }
}

/// What an agent fleet is configured with: the platform it acts on, the
/// model service, and its own host secret.
pub struct AgentFleet {
    pub host_secret: String,
    /// The fragment platform's base URL (`FRAGMENT_API`).
    pub fragment_api: String,
    /// The agent fleet's own base URL (`AGENT_URL`): the inboxes it gives
    /// fragments to deliver to.
    pub agent_url: String,
    /// Where model calls go (`None`: OpenRouter itself). The key is each
    /// owner's, which the platform's `Ledger` mints (`POST /api/budget/key`).
    pub openrouter_url: Option<String>,
    /// The owner's test controls (holds, the watchdog period): dev and e2e only.
    pub test_hooks: bool,
    /// `FRAGMENT_EGRESS_LOCAL=allow`: computers on loopback or a private
    /// network may be attached (dev and e2e only).
    pub egress_local: bool,
}

impl AgentFleet {
    /// Renders the fleet into the project's `.dev.vars`, and answers the
    /// node's environment (the host secret, for `KEYS`).
    pub fn configure(&self, project: &Path) -> Result<Vec<(String, String)>> {
        let mut vars = vec![
            ("FRAGMENT_API", self.fragment_api.as_str()),
            ("AGENT_URL", self.agent_url.as_str()),
        ];
        if let Some(u) = &self.openrouter_url {
            vars.push(("OPENROUTER_API_URL", u.as_str()));
        }
        if self.test_hooks {
            vars.push(("AGENT_TEST_HOOKS", "allow"));
        }
        if self.egress_local {
            vars.push(("FRAGMENT_EGRESS_LOCAL", "allow"));
        }
        write_dev_vars(project, &vars)?;
        Ok(node_env(&self.host_secret))
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
    /// The celld project the node runs: `cell/` for `xtask dev`, a staged
    /// copy for the e2e. Its state (`.celld/dev`) and variables live there.
    pub project: PathBuf,
    pub port: u16,
    /// Discard the local state first.
    pub clean: bool,
    /// Rebuild on file changes (for `xtask dev`; the e2e never watches).
    pub watch: bool,
    /// Extra environment for the node (celld tuning variables).
    pub env: Vec<(String, String)>,
    /// Projects co-hosted beside it for its service bindings (`celld dev
    /// --with`): the agents' script, as the fleet runs it.
    pub with: Vec<PathBuf>,
    /// Where each boot's log goes (`celld-<port>-<boot>.log`).
    pub log_dir: PathBuf,
    /// `celld dev --logs`: the node's own warnings and information in the
    /// log, beside the workers' output (a panic's message among them).
    pub node_logs: bool,
}

/// Boots of one port whose logs one directory keeps; a start past this
/// asks for the directory to be cleared rather than scanning without end.
pub const BOOT_LOGS_MAX: u32 = 10_000;

/// A new log for a boot on `port`: `celld-<port>-<boot>.log`, the first
/// boot number `dir` has no log for. A node started again on its port (the
/// e2e's restarts) never truncates the log of the one before, which a
/// crash leaves there, and its own `ready` is the only one in its file.
fn boot_log(dir: &Path, port: u16) -> Result<(PathBuf, fs::File)> {
    fs::create_dir_all(dir)?;
    for boot in 1..=BOOT_LOGS_MAX {
        let path = dir.join(format!("celld-{port}-{boot}.log"));
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
        }
    }
    bail!("{} holds {BOOT_LOGS_MAX} logs of nodes on :{port} already; clear it", dir.display())
}

/// One `celld dev` node. `celld dev` runs the node as a child process, so
/// a crash must kill that child, not only `celld dev`.
pub struct Node {
    child: Child,
    pub base: String,
    pub port: u16,
    /// This boot's log.
    pub log: PathBuf,
    reaped: bool,
}

/// A dev node's shutdown budget (`CELLD_SHUTDOWN_TOTAL_MS`): room for its
/// handoff on a slow runner, not a peer's.
const DEV_SHUTDOWN_TOTAL_MS: &str = "5000";

impl Node {
    pub fn start(tools: &Tools, opts: &NodeOptions) -> Result<(Node, Duration)> {
        let (log, out) = boot_log(&opts.log_dir, opts.port)?;
        let mut cmd = Command::new(&tools.celld);
        cmd.arg("dev").arg(&opts.project).args(["--port", &opts.port.to_string()]);
        if opts.node_logs {
            cmd.arg("--logs");
        }
        for other in &opts.with {
            cmd.arg("--with").arg(other);
        }
        if opts.clean {
            cmd.arg("--clean");
        }
        if !opts.watch {
            cmd.arg("--no-watch");
        }
        cmd.env("CELLD_ESBUILD", &tools.esbuild);
        // A dev node is the fleet's only one: once its cells are handed off
        // (durable, well inside a second) no peer takes them, and celld
        // waits out its whole no-progress window, 25 s of its default 40.
        if std::env::var_os("CELLD_SHUTDOWN_TOTAL_MS").is_none() {
            cmd.env("CELLD_SHUTDOWN_TOTAL_MS", DEV_SHUTDOWN_TOTAL_MS);
        }
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let t0 = Instant::now();
        let child = cmd.stdout(out.try_clone()?).stderr(out).stdin(Stdio::null()).spawn()?;
        let mut node = Node { child, base: format!("http://127.0.0.1:{}", opts.port), port: opts.port, log: log.clone(), reaped: false };
        loop {
            let text = fs::read(&log).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            if text.contains("  ready  ") {
                break;
            }
            if let Some(status) = node.child.try_wait()? {
                node.reaped = true;
                bail!("celld dev exited ({status}) before it was ready:\n{text}");
            }
            if t0.elapsed() > READY_TIMEOUT {
                bail!("celld dev on :{} was not ready after {READY_TIMEOUT:?}:\n{text}", opts.port);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(TcpStream::connect(("127.0.0.1", opts.port)).is_ok(), "a ready node listens");
        Ok((node, t0.elapsed()))
    }

    /// SIGTERM: celld's graceful shutdown.
    pub fn stop(mut self) -> Result<()> {
        let status = Command::new("kill").args(["-TERM", &self.child.id().to_string()]).status()?;
        assert!(status.success(), "kill -TERM delivered");
        let t0 = Instant::now();
        while self.child.try_wait()?.is_none() {
            if t0.elapsed() > STOP_TIMEOUT {
                self.kill();
                bail!("celld dev did not stop within {STOP_TIMEOUT:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.reaped = true;
        Ok(())
    }

    /// SIGKILL the node: a crash, with no graceful handoff.
    pub fn crash(mut self) -> Result<()> {
        self.kill();
        Ok(())
    }

    /// Waits for `celld dev` to exit (the foreground `xtask dev`).
    pub fn wait(mut self) -> Result<std::process::ExitStatus> {
        let status = self.child.wait()?;
        self.reaped = true;
        Ok(status)
    }

    fn kill(&mut self) {
        let _ = Command::new("pkill").args(["-KILL", "-P", &self.child.id().to_string()]).status();
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.reaped = true;
    }
}

impl Drop for Node {
    /// An early error must not leave a node holding the port. Never signal
    /// a reaped PID: the system may have reused it.
    fn drop(&mut self) {
        if !self.reaped {
            self.kill();
        }
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
        assert_eq!(first, dir.join("celld-4321-1.log"));
        assert_eq!(second, dir.join("celld-4321-2.log"));
        assert_eq!(other, dir.join("celld-4322-1.log"));
        assert_eq!(fs::read_to_string(&first).expect("read the first log"), "the first boot's last words\n");
        fs::remove_dir_all(&dir).expect("remove the test's directory");
    }
}
