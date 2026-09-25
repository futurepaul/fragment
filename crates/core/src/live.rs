//! The live socket's bounds (cell/src/live.rs), as pure logic.

use fragment_proto::limits;

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
