//! The stack on celld (docs/self-host.md, seam 1): one `celld dev` process
//! serving the platform Worker and the agents' Worker, as `wrangler dev`
//! does, each with its `.dev.vars`, from a config rendered without what
//! celld refuses. celld is a self-hosted Workers runtime
//! (github.com/futurepaul/celld, branch `selfhost`: upstream celld's
//! hardening, the container engine, and no native seam).

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::{boot_log, read_config, READY_TIMEOUT};

/// Config keys celld refuses at deploy (its docs/cloudflare-compat.md):
/// Workers AI, Browser Rendering, the build command (worker-build has run),
/// routes; and the runtime's containers, which a self-hosted deployment
/// places on a sandcastle node instead (seam 2).
pub const CELLD_REFUSED: [&str; 5] = ["ai", "browser", "build", "routes", "containers"];

/// The project's config as celld runs it: `wrangler.celld.jsonc`, beside
/// its own (so its paths and `.dev.vars` resolve the same), less what
/// celld refuses.
pub fn celld_config(project: &Path) -> Result<PathBuf> {
    let mut config = read_config(project)?;
    let obj = config.as_object_mut().context("a wrangler config is an object")?;
    for k in CELLD_REFUSED {
        obj.remove(k);
    }
    let path = project.join("wrangler.celld.jsonc");
    fs::write(&path, serde_json::to_string_pretty(&config)?)?;
    Ok(path)
}

/// Where `celld dev` keeps a project's local state.
pub fn state_dir(project: &Path) -> PathBuf {
    project.join(".celld/dev")
}

/// The tools a celld stack needs: celld itself (`CELLD_BIN`), and the
/// esbuild it bundles with (`CELLD_ESBUILD`, else the one worker-build
/// keeps in its cache).
pub struct CelldTools {
    pub celld: PathBuf,
    pub esbuild: PathBuf,
}

impl CelldTools {
    pub fn locate() -> Result<CelldTools> {
        let celld = std::env::var_os("CELLD_BIN")
            .map(PathBuf::from)
            .context("set CELLD_BIN to a celld built from github.com/futurepaul/celld, branch selfhost (cargo build --release -p celld)")?;
        if !celld.is_file() {
            bail!("no celld at {}", celld.display());
        }
        let esbuild = match std::env::var_os("CELLD_ESBUILD") {
            Some(e) => PathBuf::from(e),
            None => worker_build_esbuild()?,
        };
        Ok(CelldTools { celld, esbuild })
    }
}

/// The newest esbuild in worker-build's cache (`~/.cache/worker-build`).
fn worker_build_esbuild() -> Result<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache"));
    let dir = cache.join("worker-build");
    let mut found: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("no worker-build cache at {} (build once with cargo xtask build, or set CELLD_ESBUILD)", dir.display()))?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("esbuild-"))
        .map(|e| e.path().join("bin/esbuild"))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found.pop().context("no esbuild in worker-build's cache (set CELLD_ESBUILD)")
}

pub struct CelldOptions {
    /// The platform Worker's project; its state lives in `.celld/dev` there
    /// (`state_dir`; `crate::clear_state` discards it before the fleet is
    /// configured).
    pub project: PathBuf,
    /// Projects co-hosted for its service bindings: the agents' Worker.
    pub with: Vec<PathBuf>,
    pub port: u16,
    pub log_dir: PathBuf,
    /// A private CA (PEM) a Worker's TLS trusts beside the public roots
    /// (`CELLD_EXTRA_CA_FILE`): an intranet's, for the cell's own fetches.
    pub extra_ca_file: Option<PathBuf>,
    /// A process group of its own, so the e2e crashes it whole. `xtask dev`
    /// keeps it in the terminal's, so Ctrl-C reaches it as it reaches xtask:
    /// in a group of its own, it outlived a Ctrl-C, holding its port.
    pub own_group: bool,
}

/// One `celld dev` process, in a process group of its own or the
/// terminal's (`CelldOptions::own_group`).
pub struct CelldNode {
    child: Child,
    own_group: bool,
    pub base: String,
    pub log: PathBuf,
}

impl CelldNode {
    pub fn start(tools: &CelldTools, opts: &CelldOptions) -> Result<(CelldNode, Duration)> {
        let config = celld_config(&opts.project)?;
        let (log, out) = boot_log(&opts.log_dir, opts.port)?;
        let esbuild_dir = tools.esbuild.parent().context("esbuild's directory")?;
        let path = std::env::join_paths(std::iter::once(esbuild_dir.to_path_buf()).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())))?;
        let mut cmd = Command::new(&tools.celld);
        cmd.arg("dev").arg(&config).args(["--host", "127.0.0.1", "--port", &opts.port.to_string(), "--logs", "--no-watch"]);
        for w in &opts.with {
            cmd.arg("--with").arg(w);
        }
        // the fork's hardening (docs/hardening.md): a facet's database
        // capped past the cell's own 16 MiB, and loaded code locked down
        cmd.env("PATH", path).env("CELLD_FACET_MAX_BYTES", (20u64 << 20).to_string()).env("CELLD_DYNAMIC_LOCKDOWN", "1").env("NO_COLOR", "1");
        match &opts.extra_ca_file {
            Some(ca) => cmd.env("CELLD_EXTRA_CA_FILE", ca),
            None => cmd.env_remove("CELLD_EXTRA_CA_FILE"),
        };
        cmd.current_dir(&opts.project);
        if opts.own_group {
            cmd.process_group(0);
        }
        let t0 = Instant::now();
        let mut child = cmd.stdout(out.try_clone()?).stderr(out).stdin(Stdio::null()).spawn().with_context(|| format!("start {}", tools.celld.display()))?;
        let ready = format!("ready  http://127.0.0.1:{}", opts.port);
        // Bounded by READY_TIMEOUT.
        loop {
            let text = fs::read(&log).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
            if text.contains(&ready) {
                break;
            }
            if let Some(status) = child.try_wait()? {
                bail!("celld dev exited ({status}) before it was ready:\n{text}");
            }
            if t0.elapsed() > READY_TIMEOUT {
                let _ = child.kill();
                bail!("celld dev on :{} was not ready after {READY_TIMEOUT:?}:\n{text}", opts.port);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok((CelldNode { child, own_group: opts.own_group, base: format!("http://127.0.0.1:{}", opts.port), log }, t0.elapsed()))
    }

    pub fn wait(mut self) -> Result<std::process::ExitStatus> {
        Ok(self.child.wait()?)
    }

    /// Signals its group when it has one of its own, else celld alone
    /// (which stops the node it supervises).
    fn signal_group(&self, signal: &str) {
        let target = if self.own_group { format!("-{}", self.child.id()) } else { self.child.id().to_string() };
        let _ = Command::new("kill").args([signal, "--", &target]).stderr(Stdio::null()).status();
    }

    /// A graceful stop, as Ctrl-C stops it (celld dev answers SIGINT): its
    /// state kept for the next start.
    pub fn stop(mut self) -> Result<()> {
        self.signal_group("-INT");
        let t0 = Instant::now();
        // Bounded by STOP_TIMEOUT.
        while self.child.try_wait()?.is_none() {
            if t0.elapsed() > crate::STOP_TIMEOUT {
                self.signal_group("-KILL");
                bail!("celld dev did not stop within {:?}", crate::STOP_TIMEOUT);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// A crash: the whole group killed at once, nothing flushed.
    pub fn crash(mut self) -> Result<()> {
        self.signal_group("-KILL");
        self.child.wait()?;
        Ok(())
    }
}

impl Drop for CelldNode {
    /// An early error must not leave celld holding the port: its group
    /// (the supervisor and its node) goes with it.
    fn drop(&mut self) {
        self.signal_group("-INT");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Goal: the rendered config keeps every binding celld runs and drops
    // each key it refuses, whatever the source config's comments.
    #[test]
    fn renders_what_celld_runs() {
        let dir = std::env::temp_dir().join(format!("celld-config-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("wrangler.jsonc"),
            r#"{
  // a comment
  "name": "fragment", "main": "entry.mjs", "build": { "command": "x" },
  "ai": { "binding": "AI" }, "browser": { "binding": "BROWSER" },
  "containers": [{ "class_name": "Computer" }], "routes": [],
  "r2_buckets": [{ "binding": "BLOBS", "bucket_name": "b" }], "worker_loaders": [{ "binding": "LOADER" }]
}"#,
        )
        .unwrap();
        let out = celld_config(&dir).unwrap();
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(out).unwrap()).unwrap();
        for k in CELLD_REFUSED {
            assert!(v.get(k).is_none(), "{k} dropped");
        }
        for k in ["name", "main", "r2_buckets", "worker_loaders"] {
            assert!(v.get(k).is_some(), "{k} kept");
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
