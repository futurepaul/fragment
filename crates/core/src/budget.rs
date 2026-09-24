//! Budgets (ROADMAP decision 14; phase 4 slice C): money in micro-dollars,
//! months in UTC, and what a paid step reserves before it runs.
//!
//! A step reserves its worst case, then settles to the cost OpenRouter
//! reports. The worst case here is an estimate per kind of step, not the
//! model's price times its tokens; the person's own OpenRouter key, whose
//! limit is their allowance, is the hard stop behind it.

use serde_json::Value;

/// One US dollar in micro-dollars.
pub const USD: i64 = 1_000_000;
/// A text completion's reservation.
pub const TEXT_RESERVE: i64 = 50_000;
/// An image's.
pub const IMAGE_RESERVE: i64 = 100_000;
/// A video's, per second of it (the models this plan names cost about
/// $0.05 to $0.08 a second).
pub const VIDEO_RESERVE_PER_S: i64 = 100_000;
pub const VIDEO_DEFAULT_S: i64 = 5;
pub const VIDEO_MAX_S: i64 = 60;
/// The share of the allowance at which people are warned.
pub const WARN_PERCENT: i64 = 80;

/// A cost OpenRouter reports (dollars, a float) in micro-dollars, rounded up.
pub fn micros(usd: f64) -> i64 {
    if !usd.is_finite() || usd <= 0.0 {
        return 0;
    }
    (usd * USD as f64).ceil() as i64
}

/// Micro-dollars as dollars for people: `$0.0412`, `$20.00`.
pub fn dollars(m: i64) -> String {
    let sign = if m < 0 { "-" } else { "" };
    let m = m.abs();
    let (whole, frac) = (m / USD, m % USD);
    if frac % 10_000 == 0 {
        format!("{sign}${whole}.{:02}", frac / 10_000)
    } else {
        format!("{sign}${whole}.{:06}", frac).trim_end_matches('0').to_string()
    }
}

/// The UTC month a time falls in, `YYYY-MM` (Howard Hinnant's civil date).
pub fn period_of(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };
    format!("{year:04}-{month:02}")
}

/// What a step reserves, or `None` for a step that costs nothing (a
/// video's polls and download).
pub fn reservation(kind: &str, args: &Value) -> Option<i64> {
    match kind {
        "ai.text" => Some(TEXT_RESERVE),
        "ai.image" => Some(IMAGE_RESERVE),
        "ai.video.start" => {
            let s = args["duration"].as_i64().unwrap_or(VIDEO_DEFAULT_S).clamp(1, VIDEO_MAX_S);
            Some(s * VIDEO_RESERVE_PER_S)
        }
        _ => None,
    }
}

/// A month's standing: the allowance (the budget plus top-ups), what has
/// been spent (settled), and what is reserved by steps still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Month {
    pub allowance: i64,
    pub spent: i64,
    pub reserved: i64,
}

impl Month {
    pub fn remaining(&self) -> i64 {
        self.allowance - self.spent - self.reserved
    }

    /// Whether a reservation of `amount` fits.
    pub fn fits(&self, amount: i64) -> bool {
        amount <= self.remaining()
    }

    /// At or past the warning share of the allowance.
    pub fn warn(&self) -> bool {
        (self.spent + self.reserved) * 100 >= self.allowance * WARN_PERCENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn months_are_utc() {
        assert_eq!(period_of(0), "1970-01");
        // 2026-09-30T23:59:59.999Z and one millisecond later
        assert_eq!(period_of(1_790_812_799_999), "2026-09");
        assert_eq!(period_of(1_790_812_800_000), "2026-10");
        assert_eq!(period_of(1_709_164_800_000), "2024-02"); // 2024-02-29
        assert_eq!(period_of(1_767_225_599_999), "2025-12");
        assert_eq!(period_of(1_767_225_600_000), "2026-01");
    }

    #[test]
    fn money() {
        assert_eq!(micros(0.000_000_1), 1);
        assert_eq!(micros(0.04), 40_000);
        assert_eq!(micros(-1.0), 0);
        assert_eq!(micros(f64::NAN), 0);
        assert_eq!(dollars(20 * USD), "$20.00");
        assert_eq!(dollars(40_000), "$0.04");
        assert_eq!(dollars(41_234), "$0.041234");
        assert_eq!(dollars(-10_000), "-$0.01");
    }

    #[test]
    fn reservations() {
        assert_eq!(reservation("ai.text", &json!({})), Some(TEXT_RESERVE));
        assert_eq!(reservation("ai.image", &json!({})), Some(IMAGE_RESERVE));
        assert_eq!(reservation("ai.video.start", &json!({ "duration": 6 })), Some(600_000));
        assert_eq!(reservation("ai.video.start", &json!({ "duration": 999 })), Some(VIDEO_MAX_S * VIDEO_RESERVE_PER_S));
        assert_eq!(reservation("ai.video.poll", &json!({})), None);
        assert_eq!(reservation("ai.video.save", &json!({})), None);
    }

    #[test]
    fn a_month_holds_its_allowance() {
        let m = Month { allowance: 100_000, spent: 40_000, reserved: 10_000 };
        assert_eq!(m.remaining(), 50_000);
        assert!(m.fits(50_000));
        assert!(!m.fits(50_001));
        assert!(!m.warn());
        assert!(Month { allowance: 100_000, spent: 70_000, reserved: 10_000 }.warn());
    }
}
