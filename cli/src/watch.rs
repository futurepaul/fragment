// Continuous sync: OS file events (debounced) + a live `__watch` websocket
// from the cell + periodic stat-only sweeps as the correctness floor.
// Falls back to polling when native watching is unavailable.
use crate::api::Client;
use crate::sync::{self, SyncLock, SyncOptions};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
pub struct WatchConfig {
    pub live: bool, // listen on the cell's live change channel
}

impl Default for WatchConfig {
    fn default() -> Self {
        WatchConfig { live: true }
    }
}

// tuned constants: event debounce 300ms (max-latency 3x), poll fallback
// 5s, full sweep every 60s. Nobody ever set the knobs; the defaults are
// the product now.
const DEBOUNCE_MS: u64 = 300;
const POLL_INTERVAL_SECS: u64 = 5;
const RESCAN_SECS: u64 = 60;

/// paths that never count as content changes
fn ignored(path: &str) -> bool {
    let segs: Vec<&str> = path.split('/').collect();
    segs.iter().any(|&s| {
        s == ".git" || s == "node_modules" || s == ".trash" || s == ".fragment"
    }) || segs.iter().any(|&s| {
        s.starts_with(".#") || s.ends_with("~") || s.starts_with("~$") || s.ends_with(".swp")
            || s.starts_with(".obsidian/workspace")
    }) || path.contains(".fragment-partial")
}

/// The debounce window: hold the latest wakeups until things go quiet (or
/// the max-latency cap forces a flush), then run ONE whole-folder sync.
struct Debounced {
    pending: Arc<AtomicBool>,
    debounce: Duration,
    max_latency: Duration,
}

impl Debounced {
    fn trigger(&self) {
        self.pending.store(true, Ordering::Relaxed);
    }
    /// block until quiet, then clear
    fn wait(&self) {
        let mut quiet_for = Duration::ZERO;
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if self.pending.swap(false, Ordering::Relaxed) {
                quiet_for = Duration::ZERO; // still churning
            } else {
                quiet_for += Duration::from_millis(50);
                if quiet_for >= self.debounce || quiet_for >= self.max_latency {
                    return;
                }
            }
        }
    }
}

enum Wakeup {
    Events,
    Live,
    Sweep,
}

pub fn run(client: &Client, name: &str, dir: &Path, opts: &SyncOptions, cfg: &WatchConfig) -> Result<()> {
    let _lock = SyncLock::acquire(dir)?; // one watcher per folder, ever
    let debounce = Duration::from_millis(DEBOUNCE_MS);
    let gate = Arc::new(Debounced { pending: Arc::new(AtomicBool::new(false)), debounce, max_latency: debounce * 3 });

    // channel of wakeups (events win over the periodic sweep tick)
    let (tx, rx) = std::sync::mpsc::channel::<Wakeup>();
    let mut backend_in_use = "native";

    // ---- backend 1: OS events (with poll fallback) ----
    let evt_tx = tx.clone();
    let evt_gate = gate.pending.clone();
    let use_native = true;
    let watcher: Option<Box<dyn Send>> = if use_native {
        match spawn_native(dir, evt_tx, evt_gate) {
            Ok(w) => Some(w),
            Err(e) => {
                eprintln!("warning: native watcher unavailable ({e}); falling back to polling every {}s", POLL_INTERVAL_SECS);
                backend_in_use = "poll";
                None
            }
        }
    } else {
        backend_in_use = "poll";
        None
    };
    if watcher.is_none() {
        let tx2 = tx.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(POLL_INTERVAL_SECS));
            let _ = tx2.send(Wakeup::Events);
        });
    }

    // ---- backend 2: live channel from the cell ----
    let mut live_state = "off";
    if cfg.live {
        let host = client.host.trim_end_matches('/').to_string();
        let token = view_token(client, name);
        let tx3 = tx.clone();
        let url = match token {
            Some(t) => format!("{}/f/{}/__watch?view={}", host.replace("http", "ws"), name, t),
            None => format!("{}/f/{}/__watch", host.replace("http", "ws"), name),
        };
        std::thread::spawn(move || live_listener(&url, tx3));
        live_state = "connecting";
    }

    println!(
        "sync {name} ({}) — watch: {backend_in_use}, live: {live_state}, sweep: every {}s",
        dir.display(),
        RESCAN_SECS
    );

    // ---- main loop: wakeups → one debounced whole-folder pass ----
    let mut last_sweep = std::time::Instant::now();
    loop {
        // block for a wakeup (or the sweep deadline)
        let timeout = Duration::from_secs(RESCAN_SECS).saturating_sub(last_sweep.elapsed());
        let got = match rx.recv_timeout(timeout) {
            Ok(w) => Some(w),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Some(Wakeup::Sweep),
            Err(_) => None,
        };
        let is_sweep = matches!(got, Some(Wakeup::Sweep));
        if !is_sweep {
            gate.wait(); // let a burst settle into one pass
        }
        last_sweep = std::time::Instant::now();
        let report = sync::sync_once(client, name, dir, opts)?;
        if !report.pulled.is_empty() || !report.pushed.is_empty() || !report.merged.is_empty() || !report.conflicts.is_empty() {
            let at = chrono_like();
            println!("{at} pushed {} pulled {} merged {} conflicts {}", report.pushed.len(), report.pulled.len(), report.merged.len(), report.conflicts.len());
        }
    }
}

fn view_token(client: &Client, name: &str) -> Option<String> {
    client
        .call(client.get(&format!("/api/f/{name}/status")).ok()?)
        .ok()
        .and_then(|v| v["viewToken"].as_str().map(|s| s.to_string()))
}

/// native OS watcher; the boxed return keeps the watcher alive (dropping it
/// stops events)
fn spawn_native(dir: &Path, tx: std::sync::mpsc::Sender<Wakeup>, pending: Arc<AtomicBool>) -> Result<Box<dyn Send>> {
    use notify_debouncer_full::{new_debouncer, DebounceEventResult};
    let dir_owned: PathBuf = dir.to_path_buf();
    let mut debouncer = new_debouncer(
        Duration::from_millis(200),
        None,
        move |res: DebounceEventResult| {
            if let Ok(events) = res {
                let relevant = events.iter().any(|e| {
                    let p = e.paths.first().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
                    !ignored(&p)
                });
                if relevant {
                    pending.store(true, Ordering::Relaxed);
                    let _ = tx.send(Wakeup::Events);
                }
            }
        },
    )
    .context("start watcher")?;
    debouncer.watch(dir_owned, notify::RecursiveMode::Recursive)
        .context("watch folder")?;
    Ok(Box::new(debouncer))
}

/// the cell's change channel: frames arrive per remote mutation; reconnect
/// with backoff forever (degradation is reported by the sweep still working)
fn live_listener(url: &str, tx: std::sync::mpsc::Sender<Wakeup>) {
    use tungstenite::client::IntoClientRequest;
    let mut backoff = 1u64;
    loop {
        // build from the URL so tungstenite generates the handshake headers;
        // a hand-built Request skips them and the server rejects the upgrade
        let req = match url.into_client_request() {
            Ok(r) => r,
            Err(_) => return,
        };
        match tungstenite::connect(req) {
            Ok((mut socket, _)) => {
                backoff = 1;
                loop {
                    match socket.read() {
                        Ok(tungstenite::Message::Text(_)) => {
                            let _ = tx.send(Wakeup::Live);
                        }
                        Ok(tungstenite::Message::Close(_)) => break,
                        Err(_) => break,
                        Ok(_) => {}
                    }
                }
            }
            Err(_) => {}
        }
        std::thread::sleep(Duration::from_secs(backoff.min(30)));
        backoff *= 2;
    }
}

fn chrono_like() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = secs / 86400;
    let (h, mi, s) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    // 2026-01-01 = 20454 days after epoch (close enough for log lines)
    let (y, mut doy) = (1970u64, days);
    let mut year = y;
    loop {
        let dy = if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) { 366 } else { 365 };
        if doy < dy {
            break;
        }
        doy -= dy;
        year += 1;
    }
    let _ = &mut doy;
    format!("{:02}:{:02}:{:02}", h, mi, s)
}
