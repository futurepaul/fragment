//! What the bridge's integration tests share: the fake fragment API, the
//! scripted Hermes, and a bridge run in process (stopped and started again
//! over the same state, as a restart does; or killed, as a crash does).
//!
//! The tools of the failure cases (docs/explorations/pi-durable.md,
//! "Testing the failure cases"):
//!
//! - `Running::kill`: an abrupt stop. A bridge from `start_killable` runs on
//!   a tokio runtime of its own, and a kill shuts that runtime down: every
//!   task it spawned ends at its next await, with no SIGTERM path, and the
//!   state file is as the last step wrote it, which is what a crash leaves;
//! - `save_state`, `restore_state`, `lose_state`: a save of the bridge's
//!   state directory, put back (a rollback), or removed (a lost `/data`);
//! - `counting`: a runtime that records each turn it is given
//!   (`Command::Start`). A second run of a turn leaves the records as they
//!   were (its posts are replays of the first's, or 409s), so a test counts
//!   runs, not records.

#![allow(dead_code)]

pub mod fake;
pub mod hermes;
pub mod model;
pub mod rfb;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};

use fragment_bridge::driver::{self, BridgeError, Config};
use fragment_bridge::engine::Settings;
use fragment_bridge::runtime::script::{Script, ScriptConfig};
use fragment_bridge::runtime::{Command, Runtime, RuntimeFuture, RuntimeIo};

/// A fresh directory under the target dir, for one test's state.
pub fn dir(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("a test dir");
    d
}

pub fn settings() -> Settings {
    Settings { prompt_ttl_ms: 60_000, turn_idle_ms: 60_000 }
}

pub fn script() -> Box<dyn Runtime> {
    script_paced(Duration::from_millis(15))
}

/// The scripted agent with drafts `pace` apart (`slow` is twenty of them).
pub fn script_paced(pace: Duration) -> Box<dyn Runtime> {
    Box::new(Script { config: ScriptConfig { pace, scratch: std::env::temp_dir().join("bridge-test-script") } })
}

/// A bridge running in this process: on the test's own runtime (`start`),
/// or on a runtime of its own, so a kill can end it (`start_killable`).
pub struct Running {
    stop: watch::Sender<bool>,
    handle: Option<tokio::task::JoinHandle<Result<(), BridgeError>>>,
    runtime: Option<tokio::runtime::Runtime>,
}

pub fn config(api: &str, state: &Path, settings: Settings) -> Config {
    Config { api: api.to_string(), state_dir: state.join("bridge"), media_dir: state.join("media"), restore_pending: false, restored: state.join("restored"), hold: hold_path(state), held: held_path(state), settings, agents_file: None }
}

/// A bridge on the test's runtime, beside the fakes, as the tests have run
/// it from the start (its posts and its runtime's frames interleave with
/// the fakes' work on one thread); stopped with `stop`.
pub fn start(cfg: Config, runtime: Box<dyn Runtime>) -> Running {
    fragment_bridge::log::set_quiet(std::env::var("BRIDGE_TEST_LOG").is_err());
    let (stop, rx) = watch::channel(false);
    let handle = tokio::spawn(driver::run(cfg, runtime, rx));
    Running { stop, handle: Some(handle), runtime: None }
}

/// A bridge on a runtime of its own, which `kill` shuts down whole.
pub fn start_killable(cfg: Config, runtime: Box<dyn Runtime>) -> Running {
    fragment_bridge::log::set_quiet(std::env::var("BRIDGE_TEST_LOG").is_err());
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("bridge").enable_all().build().expect("a runtime for the bridge");
    let (stop, rx) = watch::channel(false);
    let handle = rt.spawn(driver::run(cfg, runtime, rx));
    Running { stop, handle: Some(handle), runtime: Some(rt) }
}

impl Running {
    /// Stops it, as SIGTERM does: how long it took to be gone.
    pub async fn stop(mut self) -> Duration {
        let t = Instant::now();
        let _ = self.stop.send(true);
        let handle = self.handle.take().expect("a bridge stops once");
        let r = tokio::time::timeout(Duration::from_secs(5), handle).await.expect("the bridge stops within 5 s").expect("it did not panic");
        assert!(r.is_ok(), "it stopped cleanly: {r:?}");
        let took = t.elapsed();
        self.shut_down().await;
        took
    }

    /// Kills it, as a crash does: every task ends where it is (its sockets
    /// close, its listener's port is free), nothing is flushed, and the
    /// state file is as its last step wrote it (written whole and renamed,
    /// so a crash leaves the old state or the new). Only a bridge started
    /// with `start_killable`: one on the test's runtime leaves its tasks
    /// running when its own is aborted.
    pub async fn kill(mut self) {
        assert!(self.runtime.is_some(), "a killed bridge was started with start_killable");
        self.handle.take();
        self.shut_down().await;
    }

    async fn shut_down(&mut self) {
        let Some(rt) = self.runtime.take() else { return };
        // Shutting a runtime down blocks until its workers are gone, which
        // an async context may not do; a blocking thread may.
        tokio::task::spawn_blocking(move || rt.shutdown_timeout(Duration::from_secs(2))).await.expect("the bridge's runtime shut down");
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // A test that failed before it stopped its bridge: let it go without
        // blocking the test's own runtime.
        if let Some(rt) = self.runtime.take() {
            rt.shutdown_background();
        }
    }
}

// ---- the state directory: a save, a restore, a loss ----

/// A copy of the bridge's state directory (`<dir>/bridge`), as a save of
/// `/data` holds it (none, when the bridge had written none yet).
pub struct Saved {
    copy: PathBuf,
}

fn state_dir(dir: &Path) -> PathBuf {
    dir.join("bridge")
}

/// Copies the state file of the directory `from` into `to` (made fresh),
/// if there is one. Its temporary twin is left behind: the bridge renames
/// it into place whole and never reads it, so a save that caught it would
/// hold nothing a restore uses.
fn copy_state(from: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    std::fs::create_dir_all(to).expect("a copy's directory");
    match std::fs::copy(from.join("state.json"), to.join("state.json")) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("{}: {e}", from.display()),
    }
}

/// The save: the state directory as it is now.
pub fn save_state(dir: &Path) -> Saved {
    static N: AtomicU64 = AtomicU64::new(0);
    let copy = dir.join(format!("saved-{}", N.fetch_add(1, Ordering::Relaxed)));
    copy_state(&state_dir(dir), &copy);
    Saved { copy }
}

/// The restore: the state directory goes back to the save (a rollback).
pub fn restore_state(dir: &Path, saved: &Saved) {
    copy_state(&saved.copy, &state_dir(dir));
}

/// The loss: no state directory at all (a `/data` gone).
pub fn lose_state(dir: &Path) {
    let _ = std::fs::remove_dir_all(state_dir(dir));
}

// ---- the hold (docs/computers.md: a sleep's mark before its save) ----

/// Where the tests' bridges look for the hold.
pub fn hold_path(dir: &Path) -> PathBuf {
    dir.join("hold")
}

/// Waits until `done` (looked at every 50 ms), for at most `ms`.
pub async fn until(ms: u64, what: &str, done: impl Fn() -> bool) {
    let started = Instant::now();
    // bounded by `ms`
    while !done() {
        assert!(started.elapsed() < Duration::from_millis(ms), "waited {ms} ms for {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Where the tests' bridges answer the hold.
pub fn held_path(dir: &Path) -> PathBuf {
    dir.join("held")
}

/// The platform holds the computer, as a save does (a sleep's included),
/// clearing any answer to an earlier hold first.
pub fn hold(dir: &Path) {
    let _ = std::fs::remove_file(held_path(dir));
    std::fs::write(hold_path(dir), b"").expect("the hold touched");
}

/// Whether the bridge answered the hold (`held`).
pub fn held(dir: &Path) -> bool {
    held_path(dir).exists()
}

/// The hold is gone (the save done, the sleep called off, or a new
/// container), and its answer with it.
pub fn unhold(dir: &Path) {
    let _ = std::fs::remove_file(hold_path(dir));
    let _ = std::fs::remove_file(held_path(dir));
}

// ---- runs ----

/// Each turn a runtime was given (`Command::Start`), in order, across every
/// life of a test.
#[derive(Clone, Default)]
pub struct Runs(Arc<Mutex<Vec<String>>>);

impl Runs {
    pub fn all(&self) -> Vec<String> {
        self.0.lock().expect("runs").clone()
    }

    /// How many times `turn` was run.
    pub fn of(&self, turn: &str) -> usize {
        self.0.lock().expect("runs").iter().filter(|t| *t == turn).count()
    }
}

/// `inner`, recording each turn it is given in `runs`.
pub fn counting(inner: Box<dyn Runtime>, runs: &Runs) -> Box<dyn Runtime> {
    Box::new(Counting { inner, runs: runs.clone() })
}

struct Counting {
    inner: Box<dyn Runtime>,
    runs: Runs,
}

impl Runtime for Counting {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn run(self: Box<Self>, io: RuntimeIo) -> RuntimeFuture {
        let RuntimeIo { mut commands, events, shutdown } = io;
        let (to_inner, inner_commands) = mpsc::channel::<Command>(256);
        let runs = self.runs;
        let inner = self.inner.run(RuntimeIo { commands: inner_commands, events, shutdown });
        Box::pin(async move {
            // On the bridge's runtime, so a kill ends it with the bridge.
            tokio::spawn(async move {
                // bounded by the bridge: ends when it drops its sender
                while let Some(c) = commands.recv().await {
                    if let Command::Start(ts) = &c {
                        runs.0.lock().expect("runs").push(ts.turn.clone());
                    }
                    if to_inner.send(c).await.is_err() {
                        return;
                    }
                }
            });
            inner.await
        })
    }
}
