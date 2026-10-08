//! The mind's latency, as the hosted `mind-live` lane reads it
//! (docs/optchat.md, "Latency"): each turn's timing (its `turn` record on
//! `log`: when its message was logged, when it began, its view, and each
//! model call's first data line, whole answer, tries and hedge, and when
//! its log landed), each hand-off's marks (the ask, the task's record on
//! `chat`, goose's claim, its `turn.timing` and steps on `work`, its reply,
//! the report logged, the follow-up), and percentiles over them. Pure: the
//! lane fetches the records.

use serde_json::Value;

/// The `p`-th percentile (0 to 1) of `v` by nearest rank; `None` when empty.
pub fn pct(v: &[f64], p: f64) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let rank = ((p * s.len() as f64).ceil() as usize).clamp(1, s.len());
    Some(s[rank - 1])
}

/// `n`, p50, p90 and the most of `v` (seconds), as a line.
pub fn stats(v: &[f64]) -> String {
    match (pct(v, 0.5), pct(v, 0.9), v.iter().copied().reduce(f64::max)) {
        (Some(p50), Some(p90), Some(max)) => format!("n={} p50 {p50:.1} s, p90 {p90:.1} s, max {max:.1} s", v.len()),
        _ => "n=0".into(),
    }
}

fn ms(v: &Value) -> Option<i64> {
    v.as_i64()
}

fn secs(ms: i64) -> String {
    format!("{:.1}", ms as f64 / 1000.0)
}

/// How many of the turns' calls answered on a rung of their ladder (their
/// own model busy), of all.
pub fn fell_back(timings: &[&Value]) -> (usize, usize) {
    let calls: Vec<&Value> = timings.iter().flat_map(|t| t["calls"].as_array().into_iter().flatten()).collect();
    (calls.iter().filter(|c| c["passed"].as_array().is_some_and(|p| !p.is_empty())).count(), calls.len())
}

/// A turn's model calls as `(first data line, whole answer)` in seconds,
/// and whether each was hedged, from its timing.
pub fn calls(timing: &Value) -> Vec<(Option<f64>, Option<f64>, bool)> {
    timing["calls"]
        .as_array()
        .map(|c| c.iter().map(|c| (ms(&c["first"]).map(|m| m as f64 / 1000.0), ms(&c["ms"]).map(|m| m as f64 / 1000.0), c["hedged"] == true)).collect())
        .unwrap_or_default()
}

/// Seconds from a turn's message logged to its end.
pub fn turn_total(timing: &Value) -> Option<f64> {
    Some((ms(&timing["end"])? - ms(&timing["asked"])?) as f64 / 1000.0)
}

/// Whether a turn answered in words alone: one model call, no tool.
pub fn in_words(timing: &Value) -> bool {
    timing["calls"].as_array().is_some_and(|c| c.len() == 1 && c[0]["tools"].as_array().is_none_or(Vec::is_empty))
}

/// A turn's timing as one line: its message logged → begun, its wait
/// (settling and the view), each call (the gap before it, its first data
/// line, its whole answer, tries and hedge, then its tools and log), and
/// the end.
pub fn turn_line(timing: &Value) -> String {
    let (Some(asked), Some(begun)) = (ms(&timing["asked"]), ms(&timing["begun"])) else { return "no timing".into() };
    let mut out = format!("logged→begun {}", secs(begun - asked));
    let mut last = begun;
    if let Some(view) = ms(&timing["view"]) {
        out += &format!(", wait+view {}", secs(view - begun));
        last = view;
    }
    for (k, c) in timing["calls"].as_array().into_iter().flatten().enumerate() {
        let (first, whole, at) = (ms(&c["first"]), ms(&c["ms"]), ms(&c["at"]));
        let start = at.zip(whole).map(|(a, w)| a - w);
        let mut call = format!("; call {}: ", k + 1);
        if let Some(s) = start {
            call += &format!("gap {} ", secs(s - last));
        }
        call += &format!("first {} whole {}", first.map_or("?".into(), secs), whole.map_or("?".into(), secs));
        // the model that answered, when its own was busy
        let short = |m: &Value| m.as_str().map(|m| m.rsplit('/').next().unwrap_or(m).to_string());
        if let Some(passed) = c["passed"].as_array().filter(|p| !p.is_empty()) {
            let passed: Vec<String> = passed.iter().filter_map(short).collect();
            call += &format!(" on {} ({} busy)", short(&c["model"]).unwrap_or_default(), passed.join(", "));
        }
        if let Some(n) = c["thought"].as_u64().filter(|n| *n > 0) {
            call += &format!(" ({n} chars of reasoning)");
        }
        if let (Some(p), Some(o)) = (c["tokens"][0].as_u64(), c["tokens"][2].as_u64()) {
            call += &format!(" [{p} in, {} cached, {o} out]", c["tokens"][1].as_u64().unwrap_or(0));
        }
        if c["tries"].as_i64().is_some_and(|t| t > 1) {
            call += &format!(" ({} tries over {})", c["tries"], ms(&c["since"]).map_or("?".into(), secs));
        }
        if c["hedged"] == true {
            call += if c["won"] == "second" { " hedged, the second won" } else { " hedged, the first won" };
        }
        let tools: Vec<&str> = c["tools"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        if let (Some(a), Some(l)) = (at, ms(&c["logged"])) {
            call += &format!(", {}+log {}", if tools.is_empty() { "no tools".to_string() } else { tools.join(",") }, secs(l - a));
            last = l;
        }
        out += &call;
    }
    if let Some(end) = ms(&timing["end"]) {
        out += &format!("; end {} (total {})", secs(end - last), secs(end - asked));
    }
    out
}

/// A hand-off's marks, each a time on the platform's clock (ms), where known.
#[derive(Debug, Default, Clone)]
pub struct Handoff {
    /// The person's message logged.
    pub asked: Option<i64>,
    /// The task's record on `chat`.
    pub published: Option<i64>,
    /// goose's claim (`turn.start` on `work`).
    pub claimed: Option<i64>,
    /// goose's first step on `work`.
    pub first_step: Option<i64>,
    /// goose's reply on `chat`.
    pub replied: Option<i64>,
    /// The report logged in the mind (the task's `ended`).
    pub reported: Option<i64>,
    /// The mind's follow-up (`talk` after the report).
    pub followed: Option<i64>,
    /// goose's own timing (`turn.timing`'s fields).
    pub goose: Value,
}

impl Handoff {
    /// Seconds from the ask to the report logged.
    pub fn total(&self) -> Option<f64> {
        Some((self.reported? - self.asked?) as f64 / 1000.0)
    }

    /// Seconds from the report logged to the follow-up.
    pub fn follow_up(&self) -> Option<f64> {
        Some((self.followed? - self.reported?) as f64 / 1000.0)
    }

    /// The marks as one line, each gap from the mark before it.
    pub fn line(&self) -> String {
        let marks = [
            ("ask→task published", self.asked, self.published),
            ("→claimed", self.published, self.claimed),
            ("→first step", self.claimed, self.first_step),
            ("→reply posted", self.first_step.or(self.claimed), self.replied),
            ("→report logged", self.replied, self.reported),
            ("→follow-up", self.reported, self.followed),
        ];
        let mut parts: Vec<String> = marks.iter().map(|(name, a, b)| format!("{name} {}", a.zip(*b).map_or("?".into(), |(a, b)| secs(b - a)))).collect();
        let g = &self.goose;
        if g.is_object() {
            let f = |k: &str| ms(&g[k]).map_or("?".into(), secs);
            let steps: Vec<String> = g["steps"].as_array().into_iter().flatten().map(|s| format!("{}+{}", ms(&s[0]).map_or("?".into(), secs), ms(&s[1]).map_or("?".into(), secs))).collect();
            parts.push(format!(
                "goose: ready {} (view {}, skills {}, goose {}, session+MCP {}, system {}), first word {}, steps (model+tool) [{}], last call {}, its turn {}",
                f("ready_ms"),
                f("view_ms"),
                f("skills_ms"),
                f("goose_ms"),
                f("session_ms"),
                f("system_ms"),
                f("first_ms"),
                steps.join(" "),
                f("last_ms"),
                f("total_ms")
            ));
        }
        parts.push(format!("ask→report {}", self.total().map_or("?".into(), |t| format!("{t:.1}"))));
        parts.join(", ")
    }

    /// goose's model calls' waits (each step's wait before its tool call,
    /// then the last), in seconds.
    pub fn goose_calls(&self) -> Vec<f64> {
        let mut v: Vec<f64> = self.goose["steps"].as_array().into_iter().flatten().filter_map(|s| ms(&s[0])).map(|m| m as f64 / 1000.0).collect();
        v.extend(ms(&self.goose["last_ms"]).map(|m| m as f64 / 1000.0));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn percentiles_by_nearest_rank() {
        assert_eq!(pct(&[], 0.5), None);
        assert_eq!(pct(&[3.0], 0.9), Some(3.0));
        let v = [5.0, 1.0, 4.0, 2.0, 3.0, 10.0, 6.0, 7.0, 8.0, 9.0];
        assert_eq!(pct(&v, 0.5), Some(5.0));
        assert_eq!(pct(&v, 0.9), Some(9.0));
        assert_eq!(stats(&v), "n=10 p50 5.0 s, p90 9.0 s, max 10.0 s");
    }

    /// Goal: a turn's line names each phase from its timing. Method: a turn
    /// of two calls, the first a tool call that was hedged.
    #[test]
    fn a_turns_line() {
        let t = json!({
            "asked": 1000, "begun": 1400, "view": 1700,
            "calls": [
                { "first": 4600, "ms": 5200, "tries": 1, "at": 7100, "hedged": true, "won": "second", "tools": ["zoom"], "logged": 7500 },
                { "first": 900, "ms": 1500, "tries": 1, "at": 9200, "tools": [], "logged": 9400 }
            ],
            "end": 9600
        });
        assert_eq!(
            turn_line(&t),
            "logged→begun 0.4, wait+view 0.3; call 1: gap 0.2 first 4.6 whole 5.2 hedged, the second won, zoom+log 0.4; call 2: gap 0.2 first 0.9 whole 1.5, no tools+log 0.2; end 0.2 (total 8.6)"
        );
        assert_eq!(turn_total(&t), Some(8.6));
        assert!(!in_words(&t));
        assert_eq!(calls(&t), vec![(Some(4.6), Some(5.2), true), (Some(0.9), Some(1.5), false)]);
    }

    #[test]
    fn a_handoffs_line() {
        let h = Handoff {
            asked: Some(0),
            published: Some(3000),
            claimed: Some(3500),
            first_step: Some(9000),
            replied: Some(12_000),
            reported: Some(12_600),
            followed: Some(15_000),
            goose: json!({ "ready_ms": 2500, "view_ms": 300, "skills_ms": 800, "goose_ms": 0, "session_ms": 1500, "system_ms": 10, "first_ms": 2000, "steps": [[2500, 400]], "last_ms": 2600, "total_ms": 8100 }),
        };
        assert_eq!(h.total(), Some(12.6));
        assert_eq!(h.follow_up(), Some(2.4));
        assert_eq!(h.goose_calls(), vec![2.5, 2.6]);
        assert!(h.line().starts_with("ask→task published 3.0, →claimed 0.5, →first step 5.5, →reply posted 3.0, →report logged 0.6, →follow-up 2.4, goose: ready 2.5 (view 0.3"), "{}", h.line());
    }
}
