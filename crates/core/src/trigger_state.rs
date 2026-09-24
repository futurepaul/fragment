//! Paused operations and their breakers were once `meta` keys: `paused`, a
//! JSON array of operation names, and `breaker_since:<op>`, where an
//! operation's auto-pause count starts. Fragments made then still hold
//! them, and they move into the `paused_ops` and `op_breakers` tables once,
//! in place, when the cell starts. This decides what those tables get from
//! what `meta` held; the cell's SQL reads the keys, writes the rows, and
//! deletes the keys around it (jobs.rs).

use fragment_proto::TriggerDecl;

/// The legacy breaker keys: this, then the operation's name.
pub const BREAKER_KEY_PREFIX: &str = "breaker_since:";

/// What the tables get.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Migration {
    /// Operations to pause, sorted, each once.
    pub paused: Vec<String>,
    /// `paused` held something that is not a list of names, so every
    /// operation a trigger runs is paused: an operation someone paused is
    /// never unpaused by a value the cell cannot read.
    pub failed_closed: bool,
    /// Each operation's breaker: where its count of held runs starts. A
    /// value that does not parse starts at 0, so every held run in the
    /// window counts (the breaker trips sooner, never later).
    pub breakers: Vec<(String, i64)>,
}

/// `paused`: `meta`'s `paused` value, if the fragment has one. `triggers`:
/// the installed code's `triggers` column (a JSON array of
/// `TriggerDecl`), if code is installed. `breakers`: each `meta` key that
/// begins with `BREAKER_KEY_PREFIX`, with its value.
pub fn migrate(paused: Option<&str>, triggers: Option<&str>, breakers: &[(String, String)]) -> Migration {
    let mut out = Migration::default();
    match paused.map(serde_json::from_str::<Vec<String>>) {
        None => {}
        Some(Ok(ops)) => out.paused = ops,
        Some(Err(_)) => {
            out.failed_closed = true;
            // A triggers column that does not parse runs no trigger (the
            // cell reads it the same way), so it has nothing to pause.
            let declared: Vec<TriggerDecl> = triggers.and_then(|t| serde_json::from_str(t).ok()).unwrap_or_default();
            out.paused = declared.into_iter().map(|t| t.run).collect();
        }
    }
    out.paused.retain(|op| !op.is_empty());
    out.paused.sort();
    out.paused.dedup();
    for (key, value) in breakers {
        // `LIKE` matched the key without regard to case; only the exact prefix names an operation
        let Some(op) = key.strip_prefix(BREAKER_KEY_PREFIX).filter(|op| !op.is_empty()) else { continue };
        if out.breakers.iter().any(|(seen, _)| seen == op) {
            continue;
        }
        out.breakers.push((op.to_string(), value.parse().unwrap_or(0)));
    }
    assert!(out.paused.windows(2).all(|w| w[0] < w[1]), "paused names each operation once, sorted");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRIGGERS: &str = r#"[{"channel":"inbox","run":"ingest"},{"cron":"* * * * *","run":"tick"},{"channel":"loop","run":"ingest"}]"#;

    fn keys(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn a_good_list_pauses_exactly_its_operations() {
        let m = migrate(Some(r#"["tick","boom","tick"]"#), Some(TRIGGERS), &[]);
        assert_eq!(m, Migration { paused: vec!["boom".into(), "tick".into()], failed_closed: false, breakers: vec![] });
        assert_eq!(migrate(Some("[]"), Some(TRIGGERS), &[]), Migration::default(), "an empty list pauses nothing");
    }

    /// A list the cell cannot read pauses every triggered operation, once
    /// each; with no code installed there is none to pause, but it still
    /// failed closed.
    #[test]
    fn a_corrupt_list_fails_closed() {
        for corrupt in ["not json", r#"{"tick":true}"#, "[1, 2]", ""] {
            let m = migrate(Some(corrupt), Some(TRIGGERS), &[]);
            assert_eq!(m.paused, ["ingest", "tick"], "{corrupt:?}");
            assert!(m.failed_closed, "{corrupt:?}");
        }
        let m = migrate(Some("not json"), None, &[]);
        assert!(m.failed_closed && m.paused.is_empty());
        let m = migrate(Some("not json"), Some("also not json"), &[]);
        assert!(m.failed_closed && m.paused.is_empty());
    }

    #[test]
    fn breaker_keys_become_rows() {
        let m = migrate(
            None,
            None,
            &keys(&[
                ("breaker_since:boom", "1700000000000"),
                ("breaker_since:junk", "yesterday"),
                ("BREAKER_SINCE:loud", "5"),
                ("breaker_since:", "5"),
            ]),
        );
        assert_eq!(m.breakers, [("boom".to_string(), 1_700_000_000_000), ("junk".to_string(), 0)]);
        assert!(m.paused.is_empty() && !m.failed_closed);
    }

    #[test]
    fn nothing_stored_moves_nothing() {
        assert_eq!(migrate(None, Some(TRIGGERS), &[]), Migration::default());
        assert_eq!(migrate(None, None, &[]), Migration::default());
    }
}
