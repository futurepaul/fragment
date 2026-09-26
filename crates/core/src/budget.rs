//! Budgets (ROADMAP decision 14; phase 4 slice C): money in micro-dollars,
//! months in UTC, and what a paid step reserves before it runs.
//!
//! A step reserves its worst case, then settles to the cost OpenRouter
//! reports. The worst case here is an estimate per kind of step, not the
//! model's price times its tokens; the person's own OpenRouter key, whose
//! limit is their allowance, is the hard stop behind it.

use crate::steps::Step;

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

/// The UTC month a time falls in, `YYYY-MM`.
pub fn period_of(ms: i64) -> String {
    let (year, month, _) = crate::cron::civil(ms.div_euclid(86_400_000));
    format!("{year:04}-{month:02}")
}

/// What a step reserves, or `None` for a step that costs nothing (a
/// video's polls and download, and every step that is not AI).
pub fn reservation(step: &Step) -> Option<i64> {
    match step {
        Step::AiText(_) => Some(TEXT_RESERVE),
        Step::AiImage(_) => Some(IMAGE_RESERVE),
        Step::AiVideoStart(v) => {
            let s = v.duration.unwrap_or(VIDEO_DEFAULT_S).clamp(1, VIDEO_MAX_S);
            Some(s * VIDEO_RESERVE_PER_S)
        }
        _ => None,
    }
}

/// How an OpenRouter video generation ended, once it has. This list is the
/// platform's one definition of a video's final statuses: the job's poll
/// loop reads `ended` from the poll step's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoEnd {
    /// The video is ready to save.
    Completed,
    /// It failed, was cancelled, or expired: there is nothing to save.
    Undelivered,
}

/// A video job's status as OpenRouter reports it; `None` while it is
/// still going (or a status this list does not know, which is polled on).
pub fn video_end(status: &str) -> Option<VideoEnd> {
    match status {
        "completed" => Some(VideoEnd::Completed),
        "failed" | "cancelled" | "expired" => Some(VideoEnd::Undelivered),
        _ => None,
    }
}

/// What a finished paid step is charged, in micro-dollars: the cost
/// OpenRouter reported. An answer that reported none is `None`, which the
/// ledger settles at the step's reservation, so a missing cost fails
/// closed; a video that was never delivered and reported none costs
/// nothing.
pub fn charge(reported_usd: Option<f64>, video: Option<VideoEnd>) -> Option<i64> {
    match (reported_usd, video) {
        (Some(usd), _) => Some(micros(usd)),
        (None, Some(VideoEnd::Undelivered)) => Some(0),
        (None, Some(VideoEnd::Completed) | None) => None,
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

/// A computer's Sprite at list price (docs/computers.md): an awake hour is
/// billed as the idle footprint measured on one (a tenth of a CPU at
/// $0.07 a CPU-hour, 1.5 GB at $0.04375 a GB-hour; Sprites meter what is
/// used, which the platform cannot see), and its disk, measured when it
/// wakes, at $0.000683 a GB-hour while awake and $0.000027 while asleep.
pub const AWAKE_PER_HOUR: i64 = 70_000 / 10 + 43_750 * 3 / 2;
pub const DISK_HOT_PER_GB_HOUR: i64 = 683;
pub const DISK_COLD_PER_GB_HOUR: i64 = 27;
const HOUR_MS: i128 = 3_600_000;
const GB: i128 = 1_000_000_000;

/// What `ms` of a computer costs, awake or asleep, with `disk_bytes` on
/// its disk: rounded up, so a charge is never zero.
pub fn computer(ms: i64, disk_bytes: i64, awake: bool) -> i64 {
    assert!(ms >= 0 && disk_bytes >= 0, "a span and a size are not negative");
    let disk_rate = if awake { DISK_HOT_PER_GB_HOUR } else { DISK_COLD_PER_GB_HOUR };
    // micro-dollars times GB·ms: i128, so a year of a 100 GB disk fits
    let per_hour_gb = i128::from(if awake { AWAKE_PER_HOUR } else { 0 }) * GB + i128::from(disk_rate) * i128::from(disk_bytes);
    let (n, d) = (per_hour_gb * i128::from(ms), HOUR_MS * GB);
    let micros = ((n + d - 1) / d).max(1);
    i64::try_from(micros).expect("a computer's charge fits an i64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn computers_at_list_price() {
        assert_eq!(AWAKE_PER_HOUR, 72_625);
        assert_eq!(computer(3_600_000, 0, true), 72_625);
        assert_eq!(computer(60_000, 0, true), 1_211, "a minute, rounded up");
        assert_eq!(computer(3_600_000, 10_000_000_000, true), 72_625 + 6_830);
        assert_eq!(computer(30 * 24 * 3_600_000, 10_000_000_000, false), 194_400, "a month asleep with 10 GB");
        assert_eq!(computer(1, 0, false), 1, "never zero");
    }

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
        let reserves = |kind: &str, args: serde_json::Value| reservation(&Step::from_parts(kind, args).unwrap());
        assert_eq!(reserves("ai.text", json!({ "model": "m", "prompt": "hi" })), Some(TEXT_RESERVE));
        assert_eq!(reserves("ai.image", json!({ "prompt": "p", "path": "a.png" })), Some(IMAGE_RESERVE));
        assert_eq!(reserves("ai.video.start", json!({ "prompt": "p", "duration": 6 })), Some(600_000));
        assert_eq!(reserves("ai.video.start", json!({ "prompt": "p" })), Some(VIDEO_DEFAULT_S * VIDEO_RESERVE_PER_S));
        assert_eq!(reserves("ai.video.start", json!({ "prompt": "p", "duration": 999 })), Some(VIDEO_MAX_S * VIDEO_RESERVE_PER_S));
        assert_eq!(reserves("ai.video.start", json!({ "prompt": "p", "duration": -3 })), Some(VIDEO_RESERVE_PER_S));
        assert_eq!(reserves("ai.video.poll", json!({ "id": "v" })), None);
        assert_eq!(reserves("ai.video.save", json!({ "id": "v", "path": "v.mp4" })), None);
        assert_eq!(reserves("fetch", json!({ "url": "https://x/", "method": "GET", "headers": {} })), None);
    }

    #[test]
    fn a_videos_final_statuses() {
        assert_eq!(video_end("completed"), Some(VideoEnd::Completed));
        for s in ["failed", "cancelled", "expired"] {
            assert_eq!(video_end(s), Some(VideoEnd::Undelivered), "{s}");
        }
        for s in ["pending", "in_progress", "queued", ""] {
            assert_eq!(video_end(s), None, "{s}");
        }
    }

    #[test]
    fn a_missing_cost_is_charged_the_reservation() {
        assert_eq!(charge(Some(0.04), None), Some(40_000));
        assert_eq!(charge(None, None), None, "a text or image step that reported no cost");
        assert_eq!(charge(None, Some(VideoEnd::Completed)), None, "a delivered video that reported no cost");
        assert_eq!(charge(None, Some(VideoEnd::Undelivered)), Some(0));
        assert_eq!(charge(Some(0.01), Some(VideoEnd::Undelivered)), Some(10_000), "a reported cost is charged, delivered or not");
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
