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

/// The fork of celld with the alarm fix (spikes/celld-0.5.1/README.md),
/// until a denoland release carries it.
pub const CELLD_FORK_URL: &str = "https://github.com/futurepaul/celld.git";
pub const CELLD_FORK_REV: &str = "b5f57ea156ad5443dc66ebce08d7732be193cc46";

pub fn repo_root() -> PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.parent().and_then(Path::parent).expect("crates/devstack sits two levels below the repo root").to_path_buf()
}

pub fn cell_dir() -> PathBuf {
    repo_root().join("cell")
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

/// Writes `cell/.dev.vars` (dotenv, one line per value, mode 600).
pub fn write_dev_vars(vars: &[(&str, &str)]) -> Result<()> {
    let path = cell_dir().join(".dev.vars");
    let mut f = fs::OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(&path)?;
    for (k, v) in vars {
        assert!(!k.is_empty() && k.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'), "var name {k}");
        writeln!(f, "{k}={}", v.replace('\n', "\\n"))?;
    }
    Ok(())
}

pub struct NodeOptions {
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
        cmd.arg("dev").arg(cell_dir()).args(["--port", &opts.port.to_string()]);
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
