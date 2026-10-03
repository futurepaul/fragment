//! One JSON line per event on stderr (lesson 14: `wrangler tail` drops
//! lines, so a line is a whole event). `{"at": ms, "event": name, ...}`; the
//! fields never hold a message's text, only ids and sizes.

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Map, Value};

static QUIET: AtomicBool = AtomicBool::new(false);

/// Silences the log (tests that count their own events).
pub fn set_quiet(quiet: bool) {
    QUIET.store(quiet, Ordering::Relaxed);
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("the clock is after 1970");
    u64::try_from(since.as_millis()).expect("milliseconds fit in u64 until the year 584 million")
}

/// Writes one event: its name, then the fields of `fields` (an object).
pub fn event(name: &str, fields: Value) {
    if QUIET.load(Ordering::Relaxed) {
        return;
    }
    let mut line = Map::new();
    line.insert("at".into(), Value::from(now_ms()));
    line.insert("event".into(), Value::from(name));
    if let Value::Object(more) = fields {
        for (k, v) in more {
            line.insert(k, v);
        }
    }
    let mut text = Value::Object(line).to_string();
    text.push('\n');
    // One write per line, so lines from tasks never interleave (and through
    // `eprint!`, so a test's harness captures them).
    eprint!("{text}");
}

/// `log::event` with `json!` fields: `ev!("turn.start", {"turn": t})`.
#[macro_export]
macro_rules! ev {
    ($name:expr) => {
        $crate::log::event($name, serde_json::json!({}))
    };
    ($name:expr, $($fields:tt)+) => {
        $crate::log::event($name, serde_json::json!($($fields)+))
    };
}
