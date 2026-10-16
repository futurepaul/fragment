//! How long a row of an outbox waits after a failed try: the index outbox
//! (members.rs, a membership change to a person's `Principal` cell), the
//! joined outbox (runs_on.rs, an agent member's notice to its computer),
//! the delivery outbox (deliveries.rs, a delivery to the queue) and the
//! registry's kept invite claims (registry/invites.rs) back off alike.

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

/// The tries an invite's claim gets (registry/invites.rs): a sign-in's,
/// then the registry's alarm's, with `outbox_retry_ms`'s waits (about 37
/// minutes in all). Past them it waits on its person's next sign-in.
pub const CLAIM_TRIES_MAX: i64 = 12;

/// When a claim whose try number `failed` failed is tried again: after
/// `outbox_retry_ms(failed)`, or never (`None`) once `CLAIM_TRIES_MAX`
/// tries failed.
pub fn claim_retry_ms(failed: i64) -> Option<i64> {
    (failed < CLAIM_TRIES_MAX).then(|| outbox_retry_ms(failed))
}

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

    /// A claim is tried `CLAIM_TRIES_MAX` times in all, the outbox's waits
    /// between: the sign-in's try, then the alarm's, then none.
    #[test]
    fn a_claim_is_tried_a_bounded_number_of_times() {
        let waits: Vec<i64> = (1..CLAIM_TRIES_MAX).map(|n| claim_retry_ms(n).expect("tried again")).collect();
        assert_eq!(waits.len(), 11, "the sign-in's try and eleven more");
        assert_eq!(waits[0], 2_000);
        assert_eq!(waits.iter().sum::<i64>(), 2_222_000, "about 37 minutes in all");
        assert_eq!(claim_retry_ms(CLAIM_TRIES_MAX), None);
        assert_eq!(claim_retry_ms(i64::MAX), None);
        assert_eq!(claim_retry_ms(0), Some(1_000), "a count of none waits a second, as the outbox's");
    }
}
