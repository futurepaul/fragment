//! Hedged model calls (docs/optchat.md, "Latency"). A streamed call whose
//! first data line has not come `AFTER_MS` after it was made gets one
//! second, identical call; whichever streams first is the answer, and the
//! other is cancelled. A call that fails for now (a 429, a 5xx, a broken
//! connection) before its first data line gets the second call at once,
//! as a retry within its step. Never more than two calls.
//!
//! Pure: the race's decisions (`Race`), what reads as an answer having
//! begun (`began`), and what the cancelled call is charged
//! (`cancelled_usage`). cell/src/models.rs makes the calls, and each one
//! holds its own reservation on its payer's ledger: the answer's settles
//! from its usage, the other's from `cancelled_usage`, or is released when
//! that call used nothing (it failed before it began).

use crate::price::Usage;

/// The wait for a call's first data line before its second call is made.
/// Workers AI's tool-calling models (GLM-5.3, its Flash, and the others
/// tried) stream their first line in 1 to 3 s for most calls and 15 to 60
/// s for a few identical ones (measured 2026-10-08 against its REST API,
/// and on the preview's turns). 4.5 s is past the usual worst, so few calls
/// are hedged, and a slow one is answered about 4.5 s plus a usual call's
/// first line after it was made.
pub const AFTER_MS: u64 = 4_500;
/// An answer's head is read this far for its first data line, at most:
/// past it the call is taken to have begun (a vendor sends no comments
/// this long before its data).
pub const HEAD_MAX_BYTES: usize = 64 * 1024;

/// One of a hedged call's two calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arm {
    First,
    Second,
}

impl Arm {
    pub fn other(self) -> Arm {
        match self {
            Arm::First => Arm::Second,
            Arm::Second => Arm::First,
        }
    }
}

/// How a call opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    /// Its first data line came: it is answering.
    Streaming,
    /// It failed for now before it began (a 429, a 5xx, a connection that
    /// broke): another identical call may answer.
    Passing,
    /// A refusal another identical call would get too (any other status).
    Lasting,
}

/// What the race says to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    /// Wait for the next call to open, or the timer.
    Wait,
    /// Make the second call now (hold its reservation first: `made`, or
    /// `refused` when it was not held).
    Hedge,
    /// `winner` streams: its answer is the call's. `cancel`: the other call,
    /// made and not yet opened, is cancelled.
    Won { winner: Arm, cancel: Option<Arm> },
    /// The call fails as `arm` did (the first's failure when both failed).
    /// `cancel`: the other, made and not yet opened, is cancelled.
    Failed { arm: Arm, cancel: Option<Arm> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not made (the second, before its cue).
    Unmade,
    /// Made, not opened yet.
    Pending,
    Opened(Opening),
    /// Its reservation was refused: it is never made.
    Never,
}

/// A hedged call's race, from its first call made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Race {
    first: State,
    second: State,
}

impl Default for Race {
    fn default() -> Self {
        Race::new()
    }
}

impl Race {
    /// The first call made.
    pub fn new() -> Race {
        Race { first: State::Pending, second: State::Unmade }
    }

    fn state(&mut self, arm: Arm) -> &mut State {
        match arm {
            Arm::First => &mut self.first,
            Arm::Second => &mut self.second,
        }
    }

    /// The other arm when it is made and not yet opened: the one to cancel.
    fn pending(&self, arm: Arm) -> Option<Arm> {
        let s = match arm {
            Arm::First => self.first,
            Arm::Second => self.second,
        };
        (s == State::Pending).then_some(arm)
    }

    /// Whether the second call was made.
    pub fn hedged(&self) -> bool {
        !matches!(self.second, State::Unmade | State::Never)
    }

    /// `AFTER_MS` passed since the first call was made.
    pub fn timer(&mut self) -> Next {
        match (self.first, self.second) {
            (State::Pending, State::Unmade) => Next::Hedge,
            _ => Next::Wait,
        }
    }

    /// The second call was made, its reservation held.
    pub fn made(&mut self) {
        assert_eq!(self.second, State::Unmade, "the second call is made once, at its cue");
        self.second = State::Pending;
    }

    /// The second call's reservation was refused: it is never made.
    pub fn refused(&mut self) -> Next {
        assert_eq!(self.second, State::Unmade, "only a second call not made is refused");
        self.second = State::Never;
        match self.first {
            State::Opened(Opening::Passing) => Next::Failed { arm: Arm::First, cancel: None },
            _ => Next::Wait,
        }
    }

    /// `arm` opened as `how`.
    pub fn opened(&mut self, arm: Arm, how: Opening) -> Next {
        let s = self.state(arm);
        assert_eq!(*s, State::Pending, "a call opens once, after it was made");
        *s = State::Opened(how);
        let other = arm.other();
        match how {
            Opening::Streaming => Next::Won { winner: arm, cancel: self.pending(other) },
            Opening::Lasting => Next::Failed { arm, cancel: self.pending(other) },
            Opening::Passing => match (arm, self.first, self.second) {
                // the second call's cue: a retry within the step
                (Arm::First, _, State::Unmade) => Next::Hedge,
                (Arm::First, _, State::Pending) | (Arm::Second, State::Pending, _) => Next::Wait,
                _ => Next::Failed { arm: Arm::First, cancel: None },
            },
        }
    }
}

/// Whether an answer's head (its first bytes) shows it began: a whole
/// `data:` line, or `HEAD_MAX_BYTES` read.
pub fn began(head: &[u8]) -> bool {
    if head.len() >= HEAD_MAX_BYTES {
        return true;
    }
    let mut lines = head.split(|b| *b == b'\n');
    // the last piece has no newline yet: not whole
    let _partial = lines.next_back();
    lines.any(|line| line.starts_with(b"data:"))
}

/// What the cancelled call of a hedged pair is charged: both sent the same
/// request, so the same prompt; it is charged the prompt the answer
/// reported, all of it as uncached input (it may have been routed where
/// nothing of it was cached: the dearest reading of what its prefill cost),
/// or, when the answer reported none, the reservation's own bound on it
/// (the request's bytes as tokens). Never any output: it is cancelled
/// before its first data line, so nothing it wrote was read. Workers AI
/// says nothing of a cancelled call's cost; this is the most it can be.
pub fn cancelled_usage(model: &str, answer: Option<&Usage>, body_bytes: usize) -> Usage {
    let input = match answer {
        Some(Usage::Tokens { input, cached_input, cache_write, .. }) => input + cached_input + cache_write,
        _ => body_bytes as u64,
    };
    Usage::Tokens { model: model.to_string(), input, cached_input: 0, cache_write: 0, output: 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Goal: a call that streams before the timer is the answer, with no
    /// second call. Method: the first opens streaming, then the timer.
    #[test]
    fn a_quick_call_is_never_hedged() {
        let mut r = Race::new();
        assert_eq!(r.opened(Arm::First, Opening::Streaming), Next::Won { winner: Arm::First, cancel: None });
        assert_eq!(r.timer(), Next::Wait, "a timer after the answer began asks for nothing");
        assert!(!r.hedged());
    }

    /// Goal: past the timer a second call is made, and whichever streams
    /// first wins, the other cancelled. Method: both orders.
    #[test]
    fn past_the_timer_the_first_to_stream_wins() {
        let mut r = Race::new();
        assert_eq!(r.timer(), Next::Hedge);
        r.made();
        assert_eq!(r.timer(), Next::Wait, "one second call, never a third");
        assert_eq!(r.opened(Arm::Second, Opening::Streaming), Next::Won { winner: Arm::Second, cancel: Some(Arm::First) });
        assert!(r.hedged());

        let mut r = Race::new();
        assert_eq!(r.timer(), Next::Hedge);
        r.made();
        assert_eq!(r.opened(Arm::First, Opening::Streaming), Next::Won { winner: Arm::First, cancel: Some(Arm::Second) });
    }

    /// Goal: a call that fails for now before it begins gets its second
    /// call at once (a retry within the step), and one that fails after the
    /// second was made waits for it. Method: a 5xx first, then each order.
    #[test]
    fn a_passing_failure_is_the_second_calls_cue() {
        let mut r = Race::new();
        assert_eq!(r.opened(Arm::First, Opening::Passing), Next::Hedge);
        r.made();
        assert_eq!(r.opened(Arm::Second, Opening::Streaming), Next::Won { winner: Arm::Second, cancel: None });

        let mut r = Race::new();
        assert_eq!(r.timer(), Next::Hedge);
        r.made();
        assert_eq!(r.opened(Arm::First, Opening::Passing), Next::Wait, "the second may still answer");
        assert_eq!(r.opened(Arm::Second, Opening::Passing), Next::Failed { arm: Arm::First, cancel: None }, "both failed: the first's failure is the call's");

        let mut r = Race::new();
        assert_eq!(r.timer(), Next::Hedge);
        r.made();
        assert_eq!(r.opened(Arm::Second, Opening::Passing), Next::Wait, "the first may still answer");
        assert_eq!(r.opened(Arm::First, Opening::Streaming), Next::Won { winner: Arm::First, cancel: None });
    }

    /// Goal: a lasting refusal ends the call at once (another identical
    /// call would be refused too), cancelling a second call still waiting.
    #[test]
    fn a_lasting_refusal_ends_the_race() {
        let mut r = Race::new();
        assert_eq!(r.opened(Arm::First, Opening::Lasting), Next::Failed { arm: Arm::First, cancel: None });
        let mut r = Race::new();
        assert_eq!(r.timer(), Next::Hedge);
        r.made();
        assert_eq!(r.opened(Arm::First, Opening::Lasting), Next::Failed { arm: Arm::First, cancel: Some(Arm::Second) });
    }

    /// Goal: a second call whose reservation is refused is never made: the
    /// first is waited for, or its failure is the call's.
    #[test]
    fn a_refused_reservation_makes_no_second_call() {
        let mut r = Race::new();
        assert_eq!(r.timer(), Next::Hedge);
        assert_eq!(r.refused(), Next::Wait);
        assert_eq!(r.timer(), Next::Wait);
        assert_eq!(r.opened(Arm::First, Opening::Streaming), Next::Won { winner: Arm::First, cancel: None });
        assert!(!r.hedged());

        let mut r = Race::new();
        assert_eq!(r.opened(Arm::First, Opening::Passing), Next::Hedge);
        assert_eq!(r.refused(), Next::Failed { arm: Arm::First, cancel: None });
    }

    /// Goal: an answer began at its first whole data line, wherever its
    /// chunks split, and not at a comment, an event name, or half a line.
    #[test]
    fn an_answer_begins_at_its_first_whole_data_line() {
        assert!(!began(b""));
        assert!(!began(b": keepalive\n\nevent: message\n"));
        assert!(!began(b"data: {\"choices\":[{\"delta\":{\"role\""), "half a line");
        assert!(began(b"data: {\"choices\":[]}\n"));
        assert!(began(b": hi\n\ndata: {}\n\n"));
        assert!(began(&vec![b':'; HEAD_MAX_BYTES]), "past the head's bound it is taken to have begun");
    }

    /// Goal: the cancelled call is charged the answer's prompt, all of it
    /// uncached, and never any output; with no usage, the request's bytes.
    #[test]
    fn the_cancelled_call_pays_its_prompt_and_nothing_more() {
        let answer = Usage::Tokens { model: "m".into(), input: 54, cached_input: 2560, cache_write: 0, output: 300 };
        assert_eq!(cancelled_usage("m", Some(&answer), 99_999), Usage::Tokens { model: "m".into(), input: 2614, cached_input: 0, cache_write: 0, output: 0 });
        assert_eq!(cancelled_usage("m", None, 12_000), Usage::Tokens { model: "m".into(), input: 12_000, cached_input: 0, cache_write: 0, output: 0 });
    }
}
