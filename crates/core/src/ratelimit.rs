//! Per-minute counters for calls by callers who hold only the `public`
//! floor (visitors to a public fragment): one budget per caller and one
//! per fragment (docs/MODEL.md, Limits).

use std::collections::HashMap;

pub struct Rate {
    per_caller: u32,
    per_fragment: u32,
    minute: i64,
    total: u32,
    per: HashMap<String, u32>,
}

impl Rate {
    pub fn new(per_caller: u32, per_fragment: u32) -> Rate {
        Rate { per_caller, per_fragment, minute: i64::MIN, total: 0, per: HashMap::new() }
    }

    /// Counts a call and says whether it fits this minute's budgets.
    pub fn allow(&mut self, caller: &str, now_ms: i64) -> bool {
        let minute = now_ms.div_euclid(60_000);
        if minute != self.minute {
            self.minute = minute;
            self.total = 0;
            self.per.clear();
        }
        let mine = self.per.get(caller).copied().unwrap_or(0);
        if self.total >= self.per_fragment || mine >= self.per_caller {
            return false;
        }
        self.total += 1;
        self.per.insert(caller.to_string(), mine + 1);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets() {
        let mut r = Rate::new(3, 5);
        for _ in 0..3 {
            assert!(r.allow("a", 60_000));
        }
        assert!(!r.allow("a", 60_500), "a's budget is spent");
        assert!(r.allow("b", 61_000));
        assert!(r.allow("c", 61_000));
        assert!(!r.allow("d", 61_000), "the fragment's budget is spent");
        assert!(r.allow("a", 120_000), "a new minute");
    }
}
