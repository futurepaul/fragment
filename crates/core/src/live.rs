//! The live socket's bounds (cell/src/live.rs), as pure logic: the
//! queries one socket may run, and how often it may change its presence.

use std::collections::BTreeSet;

use fragment_proto::limits;

/// Why a socket's query was not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryRefused {
    /// A query with this id is running on the socket: one at a time each.
    InFlight,
    /// The socket ran `LIVE_QUERIES_MAX` queries since the fragment last
    /// changed, or has that many running.
    Spent,
}

/// One socket's queries. A page re-runs its live views after changes, so
/// a socket may run `limits::LIVE_QUERIES_MAX` queries between two
/// changes to the fragment (a page with that many views never runs out;
/// one asking again and again between changes does), no more than that
/// many at once, and one at a time for each id. Queries over the socket
/// are outside the public call budget: this is their bound.
#[derive(Debug, Default)]
pub struct QueryBudget {
    /// The fragment's change count when the budget was last refilled.
    change: u64,
    used: u32,
    running: BTreeSet<String>,
}

impl QueryBudget {
    /// Admits a run of query `id` when the fragment has had `change` changes.
    pub fn admit(&mut self, id: &str, change: u64) -> Result<(), QueryRefused> {
        assert!(change >= self.change, "a fragment's change count only grows");
        if change > self.change {
            self.change = change;
            self.used = 0;
        }
        if self.running.contains(id) {
            return Err(QueryRefused::InFlight);
        }
        if self.used >= limits::LIVE_QUERIES_MAX || self.running.len() >= limits::LIVE_QUERIES_MAX as usize {
            return Err(QueryRefused::Spent);
        }
        self.used += 1;
        self.running.insert(id.to_string());
        assert!(self.running.len() <= limits::LIVE_QUERIES_MAX as usize, "a socket runs at most LIVE_QUERIES_MAX queries at once");
        Ok(())
    }

    /// A run of query `id` finished; answers whether it was running.
    pub fn done(&mut self, id: &str) -> bool {
        self.running.remove(id)
    }

    /// Whether no query is running on the socket.
    pub fn idle(&self) -> bool {
        self.running.is_empty()
    }
}

/// The steady gap between one socket's presence changes.
pub const PRESENCE_EVERY_MS: i64 = 1000 / limits::PRESENCE_PER_S;
const _: () = assert!(1000 % limits::PRESENCE_PER_S == 0, "a whole number of milliseconds between presence changes");
const _: () = assert!(limits::PRESENCE_BURST >= 1, "a socket may always make one change");

/// Whether a socket's presence change now is let through: at most
/// `PRESENCE_PER_S` a second, after a burst of `PRESENCE_BURST` (frames
/// bunch up on their way, and a page's library already keeps under the
/// pace). A generic cell rate algorithm: `due_ms` is when the socket's
/// next change is due at the steady pace (0 before its first). Answers
/// the next `due_ms` when the change goes through, `None` when it is
/// dropped (and then `due_ms` stands).
pub fn presence_admit(due_ms: i64, now_ms: i64) -> Option<i64> {
    let due = due_ms.max(now_ms);
    if due - now_ms > (limits::PRESENCE_BURST - 1) * PRESENCE_EVERY_MS {
        return None;
    }
    let next = due + PRESENCE_EVERY_MS;
    assert!(next > now_ms, "the next change is due after this one");
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: u32 = limits::LIVE_QUERIES_MAX;

    #[test]
    fn a_socket_runs_its_budget_between_changes() {
        let mut b = QueryBudget::default();
        for i in 0..MAX {
            assert_eq!(b.admit(&format!("q{i}"), 0), Ok(()));
            assert!(b.done(&format!("q{i}")));
        }
        assert_eq!(b.admit("q0", 0), Err(QueryRefused::Spent), "the budget is spent until the fragment changes");
        assert_eq!(b.admit("q0", 1), Ok(()), "a change refills it");
        assert!(b.done("q0"));
        for _ in 1..MAX {
            assert_eq!(b.admit("q0", 1), Ok(()));
            assert!(b.done("q0"));
        }
        assert_eq!(b.admit("q0", 1), Err(QueryRefused::Spent));
        assert_eq!(b.admit("q0", 5), Ok(()), "however many changes came, one refill");
    }

    #[test]
    fn one_run_at_a_time_for_each_id_and_a_bound_on_all() {
        let mut b = QueryBudget::default();
        assert_eq!(b.admit("list", 0), Ok(()));
        assert_eq!(b.admit("list", 0), Err(QueryRefused::InFlight));
        assert_eq!(b.admit("list", 3), Err(QueryRefused::InFlight), "a change does not let a second run of the same id start");
        assert!(b.done("list"));
        assert!(!b.done("list"), "a run finishes once");
        assert!(b.idle());
        // runs that never finish, across changes: at most MAX at once
        let mut b = QueryBudget::default();
        for i in 0..MAX {
            assert_eq!(b.admit(&format!("q{i}"), u64::from(i)), Ok(()));
        }
        assert_eq!(b.admit("one-more", u64::from(MAX)), Err(QueryRefused::Spent), "a fresh budget, but MAX running");
        assert!(b.done("q3"));
        assert_eq!(b.admit("one-more", u64::from(MAX)), Ok(()));
        assert!(!b.idle());
    }

    #[test]
    #[should_panic(expected = "only grows")]
    fn a_change_count_never_goes_back() {
        let mut b = QueryBudget::default();
        b.admit("q", 2).unwrap();
        let _ = b.admit("r", 1);
    }

    /// Changes at `at` (ms), from a socket that has made none: which go through.
    fn admitted(at: &[i64]) -> Vec<bool> {
        let mut due = 0;
        at.iter()
            .map(|now| match presence_admit(due, *now) {
                Some(next) => {
                    due = next;
                    true
                }
                None => false,
            })
            .collect()
    }

    #[test]
    fn a_burst_goes_through_then_the_steady_pace() {
        let t = 1_790_000_000_000;
        let burst = vec![t; limits::PRESENCE_BURST as usize + 1];
        let mut want = vec![true; limits::PRESENCE_BURST as usize];
        want.push(false);
        assert_eq!(admitted(&burst), want, "a burst of PRESENCE_BURST, and the next one dropped");
        let mut then = burst.clone();
        then.push(t + PRESENCE_EVERY_MS - 1);
        then.push(t + PRESENCE_EVERY_MS);
        let got = admitted(&then);
        assert_eq!(&got[burst.len()..], &[false, true], "one more once a gap has passed, and not before");
    }

    #[test]
    fn a_page_at_the_pace_is_never_dropped() {
        let t = 1_790_000_000_000;
        let steady: Vec<i64> = (0..1000).map(|i| t + i * PRESENCE_EVERY_MS).collect();
        assert!(admitted(&steady).iter().all(|a| *a), "a change every 100 ms, a thousand times");
        // the library sends at most every 150 ms; frames may arrive bunched
        let bunched: Vec<i64> = (0..1000).map(|i| t + i * 150 - if i % 7 == 0 { 140 } else { 0 }).collect();
        assert!(admitted(&bunched).iter().all(|a| *a), "a change every 150 ms, some arriving early");
    }

    #[test]
    fn a_flood_gets_the_pace_and_no_more() {
        let t = 1_790_000_000_000;
        // a change every millisecond for ten seconds
        let flood: Vec<i64> = (0..10_000).map(|i| t + i).collect();
        let through = admitted(&flood).iter().filter(|a| **a).count() as i64;
        assert_eq!(through, limits::PRESENCE_BURST + 10 * limits::PRESENCE_PER_S - 1, "a burst, then ten a second");
    }
}
