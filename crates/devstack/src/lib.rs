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

/// The fork of celld: v0.5.1 with the alarm fix (spikes/celld-0.5.1/README.md)
/// and public-only Worker egress (`CELLD_EGRESS_PUBLIC_ONLY`, branch
/// `egress/public-only`, docs/phase-3.md slice E), until denoland carries them.
pub const CELLD_FORK_URL: &str = "https://github.com/futurepaul/celld.git";
pub const CELLD_FORK_REV: &str = "cd3a68be5826a5a86594ab65e7dbe018952490fc";

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
        let esbuild = match std::env::var_os("CELLD_ESBUILD") {
            Some(p) => PathBuf::from(p),
            None => worker_build_esbuild()?,
        };
        Ok(Tools { celld, esbuild })
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

/// What a fleet is configured with (ROADMAP decision 13): the cell reads
/// these as Worker variables. A dev or test fleet points code.storage at the
/// fake in `crates/fakes`.
pub struct Fleet {
    pub host_secret: String,
    pub codestorage_org: String,
    pub codestorage_key_pem: String,
    pub codestorage_url: String,
    /// Fragments are served from `<name>.<suffix>` when set.
    pub host_suffix: Option<String>,
    pub poll_interval_s: u32,
    /// Jobs may fetch loopback and private addresses (the local fakes).
    pub egress_local: bool,
    /// The first retry delay of a failed job step (it doubles each time).
    pub job_retry_delay_s: u32,
    /// How long a blob no branch names is kept (`None`: the cell's 7 days).
    pub blob_grace_s: Option<u32>,
    /// Where AI calls go (`None`: OpenRouter itself).
    pub openrouter_url: Option<String>,
    /// The shortest wait before a delivery is retried (`None`: the cell's 10 s).
    pub delivery_retry_s: Option<u32>,
    /// Sign-in: WorkOS AuthKit (the real one, or the fake in `crates/fakes`).
    pub workos: Option<WorkOsVars>,
    /// The platform's origin (sign-in, the platform session), when it is
    /// not the hostname suffix itself.
    pub platform_url: Option<String>,
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

impl Fleet {
    /// Renders the fleet into the project's `.dev.vars`.
    pub fn write_vars(&self, project: &Path) -> Result<()> {
        let poll = self.poll_interval_s.to_string();
        let retry = self.job_retry_delay_s.to_string();
        let mut vars = vec![
            ("FRAGMENT_HOST_SECRET", self.host_secret.as_str()),
            ("CODESTORAGE_ORG", self.codestorage_org.as_str()),
            ("CODESTORAGE_PRIVATE_KEY", self.codestorage_key_pem.as_str()),
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
        }
        if let Some(s) = &self.host_suffix {
            vars.push(("FRAGMENT_HOST_SUFFIX", s.as_str()));
        }
        if let Some(w) = &self.workos {
            vars.push(("WORKOS_CLIENT_ID", w.client_id.as_str()));
            vars.push(("WORKOS_API_KEY", w.api_key.as_str()));
            if let Some(u) = &w.api_url {
                vars.push(("WORKOS_API_URL", u.as_str()));
            }
        }
        if let Some(p) = &self.platform_url {
            vars.push(("FRAGMENT_PLATFORM_URL", p.as_str()));
        }
        if self.test_hooks {
            vars.push(("FRAGMENT_TEST_HOOKS", "allow"));
        }
        write_dev_vars(project, &vars)
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
    /// Where model calls go (`None`: OpenRouter itself).
    pub openrouter_url: Option<String>,
    pub openrouter_key: String,
    /// The owner's test controls (holds, the watchdog period): dev and e2e only.
    pub test_hooks: bool,
    /// `FRAGMENT_EGRESS_LOCAL=allow`: computers on loopback or a private
    /// network may be attached (dev and e2e only).
    pub egress_local: bool,
}

impl AgentFleet {
    /// Renders the fleet into the project's `.dev.vars`.
    pub fn write_vars(&self, project: &Path) -> Result<()> {
        let mut vars = vec![
            ("FRAGMENT_HOST_SECRET", self.host_secret.as_str()),
            ("FRAGMENT_API", self.fragment_api.as_str()),
            ("AGENT_URL", self.agent_url.as_str()),
            ("OPENROUTER_API_KEY", self.openrouter_key.as_str()),
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
}

/// One `celld dev` node. `celld dev` runs the node as a child process, so
/// a crash must kill that child, not only `celld dev`.
pub struct Node {
    child: Child,
    pub base: String,
    pub port: u16,
    reaped: bool,
}

impl Node {
    pub fn start(tools: &Tools, opts: &NodeOptions) -> Result<(Node, Duration)> {
        let logs = repo_root().join("target/devstack");
        fs::create_dir_all(&logs)?;
        let log = logs.join(format!("celld-{}.log", opts.port));
        let out = fs::File::create(&log)?;
        let mut cmd = Command::new(&tools.celld);
        cmd.arg("dev").arg(&opts.project).args(["--port", &opts.port.to_string()]);
        if opts.clean {
            cmd.arg("--clean");
        }
        if !opts.watch {
            cmd.arg("--no-watch");
        }
        cmd.env("CELLD_ESBUILD", &tools.esbuild);
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let t0 = Instant::now();
        let child = cmd.stdout(out.try_clone()?).stderr(out).stdin(Stdio::null()).spawn()?;
        let mut node = Node { child, base: format!("http://127.0.0.1:{}", opts.port), port: opts.port, reaped: false };
        loop {
            let text = fs::read_to_string(&log).unwrap_or_default();
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
