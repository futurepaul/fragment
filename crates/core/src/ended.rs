//! A deleted fragment's cleanup (cell/src/ended.rs; docs/api.md, `DELETE
//! /api/f/{name}` and Operators): how one try of a part of it is told
//! apart, and when the part is tried again.
//!
//! An ended life has three kinds of part to clean: each member's list to
//! tell (one row each), its app's database and blobs (one), and, a wipe's,
//! its repo (one). A try of a part ends one of three ways:
//!
//! - **done**: the part is gone, cleaned by this try or before it (a repo
//!   code.storage says is gone, or was never there, is gone: asking again
//!   cannot make it more so);
//! - **failed, for now** (`Failure::Transient`): the dependency may answer
//!   otherwise on a later try (a timeout, a 5xx, a 429). It is tried again,
//!   the wait doubling (`backoff::outbox_retry_ms`), `TRIES_MAX` times;
//! - **refused** (`Failure::Refused`): the dependency answered that the
//!   same call cannot pass (a 4xx that is not a gone repo). Retrying it is
//!   pointless until something changes, so it is held at once.
//!
//! A part that failed `TRIES_MAX` times, or was refused, is **held**: the
//! alarm tries it once a day (`HELD_RETRY_MS`), so a deleted fragment's
//! cleanup still finishes once its dependency mends, and nothing spins
//! meanwhile. Its last error is kept beside it and named: a wipe's report
//! says which fragments are held and why, never an endless "still
//! cleaning". A wipe's own call makes every part of its fragment's ended
//! lives due at once, held ones too: the operator running a wipe again is
//! the retry, one try a call.

use crate::backoff::outbox_retry_ms;

/// Failed tries of one part before it is held.
pub const TRIES_MAX: i64 = 10;
/// How long a held part waits between the alarm's tries.
pub const HELD_RETRY_MS: i64 = 24 * 60 * 60 * 1000;
/// The longest error a part keeps (in characters): enough for a vendor's
/// status and the start of its answer.
pub const ERROR_MAX_CHARS: usize = 300;

/// Why a try of a part failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A later try may pass.
    Transient(String),
    /// The same try cannot pass until something else changes.
    Refused(String),
}

impl Failure {
    pub fn message(&self) -> &str {
        match self {
            Failure::Transient(m) | Failure::Refused(m) => m,
        }
    }
}

/// A part's row after a failed try: its count of failed tries, and when it
/// is next due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub attempts: i64,
    pub next_at: i64,
}

/// The row of a part that failed after `attempts` failed tries before
/// this one, at `now`.
pub fn after_failure(attempts: i64, failure: &Failure, now: i64) -> Retry {
    let attempts = attempts.max(0) + 1;
    let attempts = match failure {
        Failure::Transient(_) => attempts,
        // held at once: no try of the same call passes
        Failure::Refused(_) => attempts.max(TRIES_MAX),
    };
    let wait = if held(attempts) { HELD_RETRY_MS } else { outbox_retry_ms(attempts) };
    let r = Retry { attempts, next_at: now + wait };
    assert!(r.attempts >= 1 && r.next_at > now, "a failed part is tried again, later");
    assert!(!matches!(failure, Failure::Refused(_)) || held(r.attempts), "a refused part is held");
    r
}

/// Whether a part with this many failed tries is held (tried daily, and
/// named as failed).
pub fn held(attempts: i64) -> bool {
    attempts >= TRIES_MAX
}

/// An error as a part keeps it: bounded, on one line.
pub fn kept_error(message: &str) -> String {
    let one_line: String = message.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let kept: String = one_line.chars().take(ERROR_MAX_CHARS).collect();
    assert!(kept.chars().count() <= ERROR_MAX_CHARS, "a kept error is bounded");
    kept
}

/// What an index change's delivery to a member's list answered, told
/// apart: the list's own refusal (a 4xx) cannot pass again; anything else
/// that is not a 200 (its object failing, the network) may.
pub fn list_told(status: Option<u16>, detail: &str) -> Result<(), Failure> {
    match status {
        Some(200) => Ok(()),
        Some(s @ 400..=499) => Err(Failure::Refused(kept_error(&format!("its list refused the change: {s} {detail}")))),
        Some(s) => Err(Failure::Transient(kept_error(&format!("its list answered {s} {detail}")))),
        None => Err(Failure::Transient(kept_error(&format!("its list did not answer: {detail}")))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: a part failing for now is tried again, waiting longer each
    /// time, and is held after `TRIES_MAX` failed tries: tried daily, never
    /// spun on. Method: fail one part `TRIES_MAX + 2` times in a row.
    #[test]
    fn a_failing_part_backs_off_then_is_held() {
        let now = 1_000_000;
        let mut attempts = 0;
        let mut waits = vec![];
        for _ in 0..TRIES_MAX + 2 {
            let r = after_failure(attempts, &Failure::Transient("503".into()), now);
            assert_eq!(r.attempts, attempts + 1, "each failed try counts once");
            waits.push(r.next_at - now);
            attempts = r.attempts;
        }
        assert_eq!(&waits[..3], &[2_000, 4_000, 8_000], "the outboxes' backoff");
        let before_held = (TRIES_MAX - 1) as usize;
        assert!(waits[..before_held].windows(2).all(|w| w[0] <= w[1]), "waits never shorten: {waits:?}");
        assert!(waits[..before_held].iter().all(|w| *w <= 600_000), "at most ten minutes before it is held");
        assert!(waits[before_held..].iter().all(|w| *w == HELD_RETRY_MS), "held: a day between tries");
        assert!(!held(TRIES_MAX - 1) && held(TRIES_MAX) && held(TRIES_MAX + 5));
    }

    /// Goal: a refused part is held at its first failure (no try of the
    /// same call passes); one already held stays so. Method: refuse a new
    /// part, and one with failed tries.
    #[test]
    fn a_refused_part_is_held_at_once() {
        let now = 5;
        let r = after_failure(0, &Failure::Refused("403".into()), now);
        assert_eq!(r, Retry { attempts: TRIES_MAX, next_at: now + HELD_RETRY_MS });
        let r = after_failure(TRIES_MAX + 3, &Failure::Refused("400".into()), now);
        assert_eq!(r.attempts, TRIES_MAX + 4);
        // a stored count no try makes is still counted from zero
        assert_eq!(after_failure(-7, &Failure::Transient("x".into()), now).attempts, 1);
    }

    /// Goal: a list's delivery is done on its 200 only; its own 4xx is a
    /// refusal, anything else (a 5xx, no answer) a failure for now; the
    /// error kept is bounded and on one line. Method: each answer.
    #[test]
    fn a_lists_answer_is_told_apart() {
        assert_eq!(list_told(Some(200), ""), Ok(()));
        assert!(matches!(list_told(Some(400), "body"), Err(Failure::Refused(_))));
        assert!(matches!(list_told(Some(404), ""), Err(Failure::Refused(_))));
        for s in [Some(500), Some(503), Some(302), None] {
            assert!(matches!(list_told(s, "x"), Err(Failure::Transient(_))), "{s:?}");
        }
        let long = list_told(Some(500), &"y\n".repeat(1000)).unwrap_err();
        assert!(long.message().chars().count() <= ERROR_MAX_CHARS && !long.message().contains('\n'));
    }
}
