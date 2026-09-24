//! An agent's conversation window (agent/src/store.rs): the part of its
//! history each step loads and sends to the model. Sending the whole
//! history grew every step's cost with the agent's age, and once the
//! history passed the model's context every inference failed, for good.
//! goose's compaction (a summary of what falls out) comes later.

/// Messages one step loads at most, the newest. A drive runs at most 64
/// steps of about one message each (agent/src/turn.rs,
/// `STEPS_PER_TURN_MAX`), so a whole turn fits with room for the turns
/// before it.
pub const WINDOW_MESSAGES_MAX: usize = 256;

/// Stored bytes of the earlier turns a window keeps besides the running
/// one (about 64k tokens): the running turn is kept whole, and older turns
/// only while they fit.
pub const WINDOW_EARLIER_BYTES_MAX: usize = 256 * 1024;

/// One loaded message, as the window sees it.
#[derive(Clone, Copy, Debug)]
pub struct Row {
    /// A message that starts a turn's span: from the user, visible to
    /// them, and not a tool result (goose's kickoff, which a steer is too).
    pub kickoff: bool,
    /// Its stored size.
    pub bytes: usize,
}

/// Where the window starts in `rows` (the loaded messages, oldest first).
///
/// It always starts at a kickoff, so a tool call is never split from its
/// result: a tool result answers a call made after the latest kickoff
/// before it. It keeps the running turn (from the last kickoff) whole,
/// and reaches back to earlier kickoffs while the messages between them
/// and the running turn total at most `earlier_bytes_max`.
///
/// `None` when no kickoff was loaded: the running turn alone outgrew the
/// loaded messages, and no cut keeps it answerable.
pub fn window_start(rows: &[Row], earlier_bytes_max: usize) -> Option<usize> {
    let current = rows.iter().rposition(|r| r.kickoff)?;
    let mut start = current;
    let mut earlier: usize = 0;
    // bounded by the loaded rows
    for i in (0..current).rev() {
        earlier = earlier.saturating_add(rows[i].bytes);
        if earlier > earlier_bytes_max {
            break;
        }
        if rows[i].kickoff {
            start = i;
        }
    }
    assert!(rows[start].kickoff, "the window starts at a kickoff");
    assert!(start <= current, "the window keeps the running turn");
    Some(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(bytes: usize) -> Row {
        Row { kickoff: true, bytes }
    }
    fn m(bytes: usize) -> Row {
        Row { kickoff: false, bytes }
    }

    #[test]
    fn no_kickoff_no_window() {
        assert_eq!(window_start(&[], 100), None);
        // the loaded rows are the tail of a turn whose kickoff fell out
        assert_eq!(window_start(&[m(1), m(1), m(1)], 100), None);
    }

    #[test]
    fn the_running_turn_is_kept_whole_whatever_its_size() {
        // kickoff, a call, its (huge) result, the answer
        let rows = [k(10), m(10), m(10_000), m(10)];
        assert_eq!(window_start(&rows, 100), Some(0));
        // a partial earlier turn before it is dropped, never cut into
        let rows = [m(5), m(5), k(10), m(10), m(10_000)];
        assert_eq!(window_start(&rows, 1_000_000), Some(2));
    }

    #[test]
    fn earlier_turns_are_kept_while_they_fit() {
        // turns of 30, 30, 30 bytes, then the running one
        let rows = [k(10), m(10), m(10), k(10), m(10), m(10), k(10), m(10), m(10), k(10), m(10)];
        assert_eq!(window_start(&rows, 0), Some(9));
        assert_eq!(window_start(&rows, 29), Some(9), "one byte short of the turn before");
        assert_eq!(window_start(&rows, 30), Some(6));
        assert_eq!(window_start(&rows, 89), Some(3));
        assert_eq!(window_start(&rows, 90), Some(0));
        assert_eq!(window_start(&rows, usize::MAX), Some(0));
    }

    /// Goal: over many shapes of conversation, the window starts at a
    /// kickoff, keeps the running turn, fits its earlier bytes, and is the
    /// longest window that does. Method: conversations of turns (a
    /// kickoff, some call/result pairs, an answer) with pseudo-random
    /// sizes, checked against a direct reading of the rule.
    #[test]
    fn the_window_is_the_longest_that_fits_at_a_kickoff() {
        let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..500 {
            let mut rows = Vec::new();
            let turns = 1 + next(12);
            for _ in 0..turns {
                rows.push(k(1 + next(200) as usize));
                for _ in 0..next(5) {
                    rows.push(m(1 + next(100) as usize)); // a call
                    rows.push(m(1 + next(4000) as usize)); // its result
                }
                rows.push(m(1 + next(300) as usize)); // the answer
            }
            // a loaded window may begin mid-turn
            let loaded = &rows[next(rows.len() as u64 / 2 + 1) as usize..];
            let budget = next(20_000) as usize;
            let Some(start) = window_start(loaded, budget) else {
                assert!(loaded.iter().all(|r| !r.kickoff));
                continue;
            };
            let current = loaded.iter().rposition(|r| r.kickoff).unwrap();
            let earlier = |from: usize| loaded[from..current].iter().map(|r| r.bytes).sum::<usize>();
            assert!(loaded[start].kickoff);
            assert!(start <= current);
            assert!(earlier(start) <= budget);
            let longer = loaded[..start].iter().rposition(|r| r.kickoff);
            if let Some(before) = longer {
                assert!(earlier(before) > budget, "a longer window would have fit");
            }
        }
    }
}
