//! What the bridge's integration tests share: the fake fragment API, the
//! scripted Hermes, and a bridge run in process (stopped and started again
//! over the same state, as a restart does).

#![allow(dead_code)]

pub mod fake;
pub mod hermes;
pub mod model;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use fragment_bridge::driver::{self, BridgeError, Config};
use fragment_bridge::engine::Settings;
use fragment_bridge::runtime::script::{Script, ScriptConfig};
use fragment_bridge::runtime::Runtime;

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
    Box::new(Script { config: ScriptConfig { pace: Duration::from_millis(15), scratch: std::env::temp_dir().join("bridge-test-script") } })
}

/// A bridge running in this process.
pub struct Running {
    stop: watch::Sender<bool>,
    handle: tokio::task::JoinHandle<Result<(), BridgeError>>,
}

pub fn config(api: &str, state: &std::path::Path, settings: Settings) -> Config {
    Config { api: api.to_string(), state_dir: state.join("bridge"), media_dir: state.join("media"), restore_pending: false, restored: state.join("restored"), settings, agents_file: None }
}

pub fn start(cfg: Config, runtime: Box<dyn Runtime>) -> Running {
    fragment_bridge::log::set_quiet(std::env::var("BRIDGE_TEST_LOG").is_err());
    let (stop, rx) = watch::channel(false);
    let handle = tokio::spawn(driver::run(cfg, runtime, rx));
    Running { stop, handle }
}

impl Running {
    /// Stops it, as SIGTERM does: how long it took to be gone.
    pub async fn stop(self) -> Duration {
        let t = Instant::now();
        let _ = self.stop.send(true);
        let r = tokio::time::timeout(Duration::from_secs(5), self.handle).await.expect("the bridge stops within 5 s").expect("it did not panic");
        assert!(r.is_ok(), "it stopped cleanly: {r:?}");
        t.elapsed()
    }
}
