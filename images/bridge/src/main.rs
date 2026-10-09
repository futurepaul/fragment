//! `fragment-bridge`: the process a computer image runs to connect its agent
//! runtime to the fragment API (docs/computers.md).
//!
//! ```text
//! fragment-bridge run      the bridge (BRIDGE_RUNTIME=goose|script), and
//!                          its screens on BRIDGE_SCREEN_LISTEN
//! fragment-bridge version
//! ```
//!
//! Its settings are environment variables (docs/bridge.md lists them);
//! secrets are files named by path, never values.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use fragment_bridge::driver::{self, Config};
use fragment_bridge::engine::Settings;
use fragment_bridge::runtime::goose::{Goose, GooseConfig, Place};
use fragment_bridge::runtime::script::{Script, ScriptConfig};
use fragment_bridge::runtime::Runtime;
use fragment_bridge::{ev, limits, screen, screens};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_or(name: &str, default: &str) -> String {
    env(name).unwrap_or_else(|| default.to_string())
}

fn fail(why: &str) -> ! {
    ev!("bridge.misconfigured", { "why": why });
    std::process::exit(2);
}

fn parse_ms(name: &str, default: u64) -> u64 {
    match env(name) {
        Some(v) => v.parse().unwrap_or_else(|_| fail(&format!("{name} is not a number of ms"))),
        None => default,
    }
}

/// `BRIDGE_GOOSE_PLACE` (`computer`, else `machine`: a paired machine's
/// hands) and `BRIDGE_GOOSE_TOOLS` (the desktop's MCP servers every session
/// gets, commas between; the place's own by default), each one the place
/// offers.
fn place_and_tools() -> (Place, Vec<String>) {
    let place = Place::parse(&env_or("BRIDGE_GOOSE_PLACE", "computer")).unwrap_or_else(|| fail("BRIDGE_GOOSE_PLACE is computer or machine"));
    let tools = match env("BRIDGE_GOOSE_TOOLS") {
        None => place.default_tools(),
        Some(t) if t.trim() == "none" => vec![],
        Some(t) => t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
    };
    if let Some(bad) = tools.iter().find(|t| !place.offers(t)) {
        fail(&format!("BRIDGE_GOOSE_TOOLS names {bad:?}, which a {} does not offer (browser, web{})", if place == Place::Machine { "machine" } else { "computer" }, if place == Place::Machine { "" } else { ", computer" }));
    }
    (place, tools)
}

fn runtime() -> Box<dyn Runtime> {
    match env_or("BRIDGE_RUNTIME", "goose").as_str() {
        "goose" => {
            let (place, tools) = place_and_tools();
            Box::new(Goose::new(GooseConfig {
                command: PathBuf::from(env_or("BRIDGE_GOOSE_BIN", "/usr/local/bin/goose")),
                args: vec!["acp".into(), "--with-builtin".into(), env_or("BRIDGE_GOOSE_BUILTINS", "developer,skills")],
                work: PathBuf::from(env_or("BRIDGE_GOOSE_WORK", "/data/work")),
                home: PathBuf::from(env_or("BRIDGE_GOOSE_HOME", "/data/work/home")),
                root: PathBuf::from(env_or("BRIDGE_GOOSE_ROOT", "/tmp/goose")),
                api: env("FRAGMENT_API").unwrap_or_else(|| fail("FRAGMENT_API is the fragment API's address")),
                model: env("FRAGMENT_MODEL").unwrap_or_else(|| fail("FRAGMENT_MODEL is the model intercept's address")),
                tier: env_or("BRIDGE_GOOSE_TIER", "cheap"),
                cli: env("BRIDGE_GOOSE_CLI").map(PathBuf::from),
                ca: env("BRIDGE_TRUST_CA").map(|ca| (PathBuf::from(ca), PathBuf::from("/etc/ssl/certs/ca-certificates.crt"))),
                desktop: env("BRIDGE_GOOSE_DESKTOP").map(PathBuf::from),
                tools,
                place,
                skills: env("BRIDGE_GOOSE_SKILLS").is_some_and(|v| v == "1"),
            }))
        }
        "script" => Box::new(Script { config: ScriptConfig { pace: Duration::from_millis(parse_ms("BRIDGE_SCRIPT_PACE_MS", 40)), scratch: PathBuf::from(env_or("BRIDGE_SCRIPT_SCRATCH", "/tmp/bridge-script")), data: PathBuf::from(env_or("BRIDGE_SCRIPT_DATA", "/data")) } }),
        other => fail(&format!("BRIDGE_RUNTIME {other:?} is neither goose nor script")),
    }
}

/// `BRIDGE_HELD_LEAVE_OUT`: what the save may leave out (whitespace
/// between), each a pattern the platform takes.
fn left_out() -> Vec<String> {
    let patterns: Vec<String> = env("BRIDGE_HELD_LEAVE_OUT").map(|v| v.split_whitespace().map(str::to_string).collect()).unwrap_or_default();
    if patterns.len() > limits::HELD_PATTERNS_MAX || !patterns.iter().all(|p| driver::left_out_ok(p)) {
        fail(&format!("BRIDGE_HELD_LEAVE_OUT is at most {} patterns of letters, digits and ._-/*?[]", limits::HELD_PATTERNS_MAX));
    }
    patterns
}

fn screen_config() -> Option<screen::ScreenConfig> {
    let listen: SocketAddr = env("BRIDGE_SCREEN_LISTEN")?.parse().unwrap_or_else(|_| fail("BRIDGE_SCREEN_LISTEN is not host:port"));
    // one display for every agent was the cut model: each agent's is its own now
    if env("BRIDGE_SCREEN_RFB").is_some() {
        fail("BRIDGE_SCREEN_RFB is gone: name each agent's display in BRIDGE_SCREENS_FILE");
    }
    let screens = match (env("BRIDGE_SCREENS_FILE"), env("BRIDGE_SCREENS_DIR")) {
        (None, None) => None,
        (Some(file), None) => Some(screens::Source::File(PathBuf::from(file))),
        (None, Some(dir)) => {
            let dir = PathBuf::from(dir);
            if !screens::dir_ok(&dir) {
                fail(&format!("BRIDGE_SCREENS_DIR is an absolute path of at most {} bytes (its agents' sockets are under it)", screens::DIR_PATH_MAX_BYTES));
            }
            Some(screens::Source::Dir(dir))
        }
        (Some(_), Some(_)) => fail("BRIDGE_SCREENS_FILE or BRIDGE_SCREENS_DIR names the screens, not both"),
    };
    let start = env("BRIDGE_SCREEN_START").map(|s| s.split_whitespace().map(str::to_string).collect());
    Some(screen::ScreenConfig { listen, dir: PathBuf::from(env_or("BRIDGE_SCREEN_DIR", "/opt/fragment/screen")), screens, start })
}

/// SIGTERM (or SIGINT) turns `stop` true; the process is gone within
/// `SHUTDOWN_MS_MAX` of it, whatever is left (docs/computers.md).
fn on_signal(stop: tokio::sync::watch::Sender<bool>) {
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("a SIGTERM handler");
        let mut int = signal(SignalKind::interrupt()).expect("a SIGINT handler");
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        ev!("bridge.signal", { "graceMs": limits::SHUTDOWN_MS_MAX });
        let _ = stop.send(true);
        tokio::time::sleep(Duration::from_millis(limits::SHUTDOWN_MS_MAX)).await;
        ev!("bridge.forced_exit");
        std::process::exit(0);
    });
}

#[tokio::main]
async fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "run".into());
    let (stop_tx, stop) = tokio::sync::watch::channel(false);
    match cmd.as_str() {
        "version" => {
            println!("fragment-bridge {}", env!("CARGO_PKG_VERSION"));
        }
        "run" => {
            on_signal(stop_tx);
            let api = env("FRAGMENT_API").unwrap_or_else(|| fail("FRAGMENT_API is the fragment API's address"));
            let cfg = Config {
                api,
                state_dir: PathBuf::from(env_or("BRIDGE_STATE_DIR", "/data/bridge")),
                media_dir: PathBuf::from(env_or("BRIDGE_MEDIA_DIR", "/tmp/bridge-media")),
                restore_pending: env("RESTORE_PENDING").is_some_and(|v| v == "1"),
                restored: PathBuf::from(env_or("BRIDGE_RESTORED", "/run/computer/restored")),
                hold: PathBuf::from(env_or("BRIDGE_HOLD", "/run/computer/hold")),
                held: PathBuf::from(env_or("BRIDGE_HELD", "/run/computer/held")),
                left_out: left_out(),
                settings: Settings { prompt_ttl_ms: parse_ms("BRIDGE_PROMPT_TTL_MS", limits::PROMPT_TTL_MS_DEFAULT), turn_idle_ms: parse_ms("BRIDGE_TURN_IDLE_MS", limits::TURN_IDLE_MS_MAX) },
                screen: screen_config(),
            };
            ev!("bridge.boot", { "computer": env("FRAGMENT_COMPUTER"), "image": env("FRAGMENT_IMAGE"), "restorePending": cfg.restore_pending });
            match driver::run(cfg, runtime(), stop).await {
                Ok(()) => std::process::exit(0),
                Err(e) => {
                    ev!("bridge.failed", { "error": e.to_string() });
                    std::process::exit(1);
                }
            }
        }
        other => fail(&format!("{other:?}: run or version")),
    }
}
