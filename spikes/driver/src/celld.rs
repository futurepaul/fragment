//! A `celld dev` node the drivers start and stop.

use std::fs;
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Result};

pub const READY_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Celld {
    pub child: Child,
    pub base: String,
    /// Set once the process has been reaped, so Drop never signals a PID
    /// the system may have reused.
    reaped: bool,
}

impl Celld {
    pub fn start(project: &Path, port: u16, clean: bool, env: &[(&str, String)]) -> Result<(Celld, Duration)> {
        let log = project.join(".celld-driver.log");
        let out = fs::File::create(&log)?;
        let mut cmd = Command::new("celld");
        cmd.arg("dev").arg(project).args(["--port", &port.to_string(), "--no-watch"]);
        if clean {
            cmd.arg("--clean");
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let t0 = Instant::now();
        let child = cmd.stdout(out.try_clone()?).stderr(out).stdin(Stdio::null()).spawn()?;
        loop {
            let text = fs::read_to_string(&log).unwrap_or_default();
            if text.contains("  ready  ") {
                break;
            }
            if text.contains("Error:") {
                bail!("celld dev failed to start: {text}");
            }
            if t0.elapsed() > READY_TIMEOUT {
                bail!("celld dev on :{port} was not ready after {READY_TIMEOUT:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let startup = t0.elapsed();
        assert!(TcpStream::connect(("127.0.0.1", port)).is_ok(), "ready means listening");
        Ok((Celld { child, base: format!("http://127.0.0.1:{port}"), reaped: false }, startup))
    }

    pub fn stop(mut self) -> Result<()> {
        // SIGTERM: celld's graceful shutdown, as a restart would do.
        let status = Command::new("kill").args(["-TERM", &self.child.id().to_string()]).status()?;
        assert!(status.success(), "kill -TERM delivered");
        let t0 = Instant::now();
        while self.child.try_wait()?.is_none() {
            if t0.elapsed() > Duration::from_secs(60) {
                self.child.kill()?;
                bail!("celld dev did not stop within 60 s");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        self.reaped = true;
        Ok(())
    }
}

impl Drop for Celld {
    /// An early error must not leave a node holding the port.
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        kill_children(self.child.id());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Celld {
    /// SIGKILL the node: a crash, with no graceful handoff. `celld dev`
    /// runs the node as its child process, so the node is what dies.
    #[allow(dead_code)] // only the `turns` binary crashes nodes
    pub fn crash(mut self) -> Result<()> {
        kill_children(self.child.id());
        self.child.kill()?;
        self.child.wait()?;
        self.reaped = true;
        Ok(())
    }
}

/// SIGKILLs the node processes `celld dev` spawned (none is fine).
fn kill_children(parent: u32) {
    let _ = Command::new("pkill").args(["-KILL", "-P", &parent.to_string()]).status();
}
