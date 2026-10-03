//! A job's media steps (`job.ai.image`, `job.ai.video`): OpenRouter is
//! their vendor until phase 7 moves them onto Cloudflare (docs/cloudflare-v1.md;
//! the debt ledger's entry). The deployment pays OpenRouter with its own
//! key, and the payer's ledger meters what OpenRouter reports it cost
//! (`price::Usage::Billed`), with the margin on top. A step reserves its
//! worst case first, so a step the payer cannot cover never reaches the
//! vendor; a video's cost comes with the poll that sees it end.

use crate::price::{Usage, USD};
use crate::steps::Step;

/// The vendor's name in the price book (`price::DEFAULT_VENDORS`).
pub const OPENROUTER: &str = "openrouter";
/// An image's worst case: the models this plan names cost a few cents.
pub const IMAGE_WORST_MICROS: u64 = 100_000;
/// A video's worst case per second (they cost about $0.05 to $0.08 a second).
pub const VIDEO_WORST_MICROS_PER_S: u64 = 100_000;
pub const VIDEO_DEFAULT_S: i64 = 5;
/// The longest video a step reserves for: its duration is passed to the
/// vendor as given, and the worst case is clamped here.
pub const VIDEO_MAX_S: i64 = 60;

const _: () = assert!(VIDEO_WORST_MICROS_PER_S * VIDEO_MAX_S as u64 <= 25 * USD as u64, "a video's worst case fits one reservation (ledger::RESERVATION_MAX)");

/// What a media step reserves, as the vendor's bill would read: `None` for
/// a step that costs nothing (a video's polls and download, and every step
/// that is not media).
pub fn worst(step: &Step) -> Option<Usage> {
    let micros = match step {
        Step::AiImage(_) => IMAGE_WORST_MICROS,
        Step::AiVideoStart(v) => {
            let s = v.duration.unwrap_or(VIDEO_DEFAULT_S).clamp(1, VIDEO_MAX_S);
            s as u64 * VIDEO_WORST_MICROS_PER_S
        }
        _ => return None,
    };
    Some(Usage::Billed { vendor: OPENROUTER.into(), micros })
}

/// What OpenRouter reported a call cost (dollars, a float), as the usage
/// the ledger meters: micro-dollars rounded up. A report that is missing,
/// negative or not a number is `None`, which the ledger settles at the
/// step's reservation: a missing cost fails closed, never at zero.
pub fn billed(reported_usd: Option<f64>) -> Option<Usage> {
    let usd = reported_usd.filter(|u| u.is_finite() && *u >= 0.0)?;
    // f64 holds every micro-dollar exactly up to 2^53, far past QUANTITY_MAX
    let micros = (usd * USD as f64).ceil();
    if micros > crate::price::QUANTITY_MAX as f64 {
        return None;
    }
    Some(Usage::Billed { vendor: OPENROUTER.into(), micros: micros as u64 })
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn billed_micros(u: Option<Usage>) -> Option<u64> {
        match u {
            Some(Usage::Billed { vendor, micros }) if vendor == OPENROUTER => Some(micros),
            None => None,
            other => panic!("{other:?}"),
        }
    }

    /// Goal: each media step reserves its own worst case, a video's by its
    /// duration within the clamp; nothing else reserves. Method: one of each.
    #[test]
    fn worst_cases() {
        let worst_of = |kind: &str, args: serde_json::Value| billed_micros(worst(&Step::from_parts(kind, args).unwrap()));
        assert_eq!(worst_of("ai.image", json!({ "prompt": "p", "path": "a.png" })), Some(IMAGE_WORST_MICROS));
        assert_eq!(worst_of("ai.video.start", json!({ "prompt": "p", "duration": 6 })), Some(600_000));
        assert_eq!(worst_of("ai.video.start", json!({ "prompt": "p" })), Some(VIDEO_DEFAULT_S as u64 * VIDEO_WORST_MICROS_PER_S));
        assert_eq!(worst_of("ai.video.start", json!({ "prompt": "p", "duration": 999 })), Some(VIDEO_MAX_S as u64 * VIDEO_WORST_MICROS_PER_S));
        assert_eq!(worst_of("ai.video.start", json!({ "prompt": "p", "duration": -3 })), Some(VIDEO_WORST_MICROS_PER_S));
        assert_eq!(worst_of("ai.video.poll", json!({ "id": "v" })), None);
        assert_eq!(worst_of("ai.video.save", json!({ "id": "v", "path": "v.mp4" })), None);
        assert_eq!(worst_of("ai.text", json!({ "prompt": "hi" })), None, "text goes through the model route, not here");
        assert_eq!(worst_of("fetch", json!({ "url": "https://x/", "method": "GET", "headers": {} })), None);
    }

    /// Goal: a reported cost meters rounded up; a missing or broken one
    /// fails closed (settled at the reservation). Method: each kind.
    #[test]
    fn a_reported_cost_is_metered_rounded_up() {
        assert_eq!(billed_micros(billed(Some(0.04))), Some(40_000));
        assert_eq!(billed_micros(billed(Some(0.000_000_1))), Some(1));
        assert_eq!(billed_micros(billed(Some(0.0))), Some(0));
        assert_eq!(billed_micros(billed(None)), None);
        assert_eq!(billed_micros(billed(Some(-1.0))), None);
        assert_eq!(billed_micros(billed(Some(f64::NAN))), None);
        assert_eq!(billed_micros(billed(Some(f64::INFINITY))), None);
        assert_eq!(billed_micros(billed(Some(1e12))), None, "past QUANTITY_MAX: a report that large is no price");
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
}
