//! How long a row of an outbox waits after a failed try: the index outbox
//! (members.rs, a membership change to a person's `Principal` cell) and
//! the delivery outbox (deliveries.rs, a delivery to the queue) back off
//! alike.

/// The wait after try number `attempts` failed: 2^attempts seconds, at
/// most ten minutes. `attempts` counts the failed tries, from 1 (a count
/// of 0 or less waits one second), and the exponent stops at 10, so the
/// shift never overflows.
pub fn outbox_retry_ms(attempts: i64) -> i64 {
    let wait_ms = 1000 * (1i64 << attempts.clamp(0, 10)).min(OUTBOX_RETRY_S_MAX);
    assert!((1000..=OUTBOX_RETRY_S_MAX * 1000).contains(&wait_ms), "an outbox wait is a second to ten minutes");
    wait_ms
}

/// The longest wait between tries, in seconds.
const OUTBOX_RETRY_S_MAX: i64 = 600;

#[cfg(test)]
mod tests {
    use super::*;

    /// The waits double from two seconds and stop at ten minutes; the
    /// count's nonsense values stay in bounds.
    #[test]
    fn waits_double_to_ten_minutes() {
        let waits: Vec<i64> = (1..=11).map(outbox_retry_ms).collect();
        assert_eq!(waits, [2_000, 4_000, 8_000, 16_000, 32_000, 64_000, 128_000, 256_000, 512_000, 600_000, 600_000]);
        assert_eq!(outbox_retry_ms(0), 1_000);
        assert_eq!(outbox_retry_ms(-3), 1_000);
        assert_eq!(outbox_retry_ms(i64::MAX), 600_000);
    }
}
