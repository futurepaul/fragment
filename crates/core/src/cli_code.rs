//! The code a person types to add a CLI's key to themselves (docs/api.md,
//! Sign-in; Paul, 2026-10-17: a typed device code, RFC 8628's shape).
//! `fragment login` asks for one with its new key (`POST /cli/begin`,
//! signed by that key), prints it, and waits; the person opens `/cli`, a
//! page that reads nothing from its URL, signed in, and types it. So no
//! link anyone sends approves a key: only what the person typed from their
//! own terminal does.
//!
//! A code is `LEN` letters from RFC 8628's base-20 (consonants only: no
//! vowels, so no words, and no 0/O or 1/I to confuse), shown as two
//! groups of four (`WDJB-MJHT`). The registry keeps its SHA-256 for
//! `TTL_MS`, spends it once, and counts a person's wrong codes: past
//! `MISSES_MAX` in `TTL_MS` they wait, so guessing someone else's pending
//! code (one in `20^8 / pending`) is out of reach.

/// RFC 8628 section 6.1's alphabet for a code a person types.
pub const ALPHABET: &[u8; 20] = b"BCDFGHJKLMNPQRSTVWXZ";
/// Letters in a code: 20^8, about 34.5 bits.
pub const LEN: usize = 8;
/// A code is typed within this, or not at all (the CLI waits as long).
pub const TTL_MS: i64 = 10 * 60_000;
/// Wrong codes a person may type within `TTL_MS` before they must wait.
pub const MISSES_MAX: u32 = 10;
/// Random bytes a code is drawn from: rejection sampling keeps a byte
/// below `ACCEPT_BELOW` (240 = 12 × 20, so each letter is equally likely);
/// 32 bytes hold 8 such bytes but with odds of about 10^-25 against.
pub const RANDOM_BYTES: usize = 32;
const ACCEPT_BELOW: u8 = 240;
/// What a person may type, at most: a code, its hyphen, and some spaces.
pub const TYPED_MAX_BYTES: usize = 32;

const _: () = assert!((ACCEPT_BELOW as usize).is_multiple_of(ALPHABET.len()), "every letter is drawn equally often");
const _: () = assert!(RANDOM_BYTES >= 2 * LEN, "a draw almost never runs out of bytes");
const _: () = assert!(TYPED_MAX_BYTES > LEN + 1, "a code and its hyphen fit what may be typed");

/// A code from `random` (the caller's randomness), as the registry keeps
/// it: `LEN` letters of `ALPHABET`, no hyphen. `None` in the vanishing
/// case that too few bytes were below `ACCEPT_BELOW`: draw again.
pub fn from_random(random: &[u8; RANDOM_BYTES]) -> Option<String> {
    let mut code = String::with_capacity(LEN);
    // bounded: one pass over RANDOM_BYTES bytes
    for &b in random.iter() {
        if code.len() == LEN {
            break;
        }
        if b < ACCEPT_BELOW {
            code.push(char::from(ALPHABET[usize::from(b) % ALPHABET.len()]));
        }
    }
    if code.len() == LEN {
        assert!(valid(&code), "a drawn code is one a person can type back");
        Some(code)
    } else {
        None
    }
}

/// Whether `code` is a code as kept: `LEN` letters of `ALPHABET`.
pub fn valid(code: &str) -> bool {
    code.len() == LEN && code.bytes().all(|b| ALPHABET.contains(&b))
}

/// What a person typed, as kept: upper case, without the hyphen or any
/// spaces. `None` when it is no code (a letter outside the alphabet, the
/// wrong length): a miss like any other wrong code.
pub fn normalize(typed: &str) -> Option<String> {
    if typed.len() > TYPED_MAX_BYTES {
        return None;
    }
    let code: String = typed.chars().filter(|c| !c.is_whitespace() && *c != '-').map(|c| c.to_ascii_uppercase()).collect();
    if valid(&code) {
        Some(code)
    } else {
        None
    }
}

/// A code as shown: two groups of four, `WDJB-MJHT`.
pub fn display(code: &str) -> String {
    assert!(valid(code), "only a code as kept is shown");
    format!("{}-{}", &code[..LEN / 2], &code[LEN / 2..])
}

/// A person's wrong codes: how many, since when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Misses {
    pub count: u32,
    pub since_ms: i64,
}

/// Whether someone with `misses` may type another code at `now_ms`: their
/// misses are forgotten `TTL_MS` after the first of them.
pub fn may_try(misses: Option<Misses>, now_ms: i64) -> bool {
    match misses {
        Some(m) if now_ms - m.since_ms < TTL_MS => m.count < MISSES_MAX,
        _ => true,
    }
}

/// `misses`, after one more at `now_ms`.
pub fn missed(misses: Option<Misses>, now_ms: i64) -> Misses {
    let next = match misses {
        Some(m) if now_ms - m.since_ms < TTL_MS => Misses { count: m.count.saturating_add(1), since_ms: m.since_ms },
        _ => Misses { count: 1, since_ms: now_ms },
    };
    assert!(next.count >= 1 && next.since_ms <= now_ms, "a miss is counted, at or before now");
    next
}

/// Whether a code made at `created_ms` may still be typed at `now_ms`; the
/// registry's sweep deletes those for which it is false.
pub fn live(created_ms: i64, now_ms: i64) -> bool {
    now_ms - created_ms < TTL_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Valid: a draw is LEN letters of the alphabet, each letter reachable,
    /// and reads back as typed in any case, with a hyphen or spaces.
    #[test]
    fn a_code_is_drawn_from_the_alphabet_and_typed_back() {
        let mut bytes = [0u8; RANDOM_BYTES];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i * 13) as u8;
        }
        let code = from_random(&bytes).expect("enough bytes below 240");
        assert!(valid(&code));
        assert_eq!(code, "BRJZQHXP");
        let shown = display(&code);
        assert_eq!(shown, "BRJZ-QHXP");
        for typed in [shown.clone(), shown.to_lowercase(), " brjz qhxp ".to_string(), "BRJZQHXP".to_string()] {
            assert_eq!(normalize(&typed).as_deref(), Some(code.as_str()), "{typed:?}");
        }
        // every letter is drawn: byte n maps to ALPHABET[n % 20]
        let all: [u8; RANDOM_BYTES] = std::array::from_fn(|i| (i % 20) as u8);
        let drawn: Vec<String> = (0..3).map(|k| from_random(&std::array::from_fn(|i| all[(i + k * 8) % RANDOM_BYTES])).unwrap()).collect();
        let seen: std::collections::BTreeSet<char> = drawn.iter().flat_map(|c| c.chars()).collect();
        assert_eq!(seen.len(), ALPHABET.len(), "{drawn:?}");
    }

    /// Bytes at or past 240 are skipped, never folded into a letter (which
    /// would favour the first sixteen); too few accepted is a fresh draw.
    #[test]
    fn a_draw_rejects_the_biased_bytes() {
        let mut bytes = [255u8; RANDOM_BYTES];
        assert_eq!(from_random(&bytes), None);
        for (i, b) in bytes.iter_mut().enumerate().skip(RANDOM_BYTES - LEN) {
            *b = 239 - i as u8;
        }
        let code = from_random(&bytes).expect("eight bytes below 240");
        assert_eq!(code.len(), LEN);
        assert_eq!(&code[..1], &char::from(ALPHABET[(239 - (RANDOM_BYTES - LEN)) % 20]).to_string());
    }

    /// Invalid: anything that is not a code reads as none (a miss).
    #[test]
    fn what_is_no_code_is_refused() {
        for bad in ["", "BQHV", "BQHV-MDS", "BQHV-MDSKB", "AQHV-MDSK", "BQHV-MDS0", "BQHV_MDSK", "ВQHV-MDSK", &"B".repeat(TYPED_MAX_BYTES + 1), &format!("BQHV-MDSK{}", " ".repeat(TYPED_MAX_BYTES))] {
            assert_eq!(normalize(bad), None, "{bad:?}");
        }
        assert!(!valid("bqhvmdsk"));
    }

    /// A person's wrong codes: MISSES_MAX of them within TTL_MS make them
    /// wait; TTL_MS after the first, they are forgotten.
    #[test]
    fn misses_are_bounded_then_forgotten() {
        let t = 1_790_000_000_000;
        assert!(may_try(None, t));
        let mut m = None;
        for n in 1..=MISSES_MAX {
            assert!(may_try(m, t + i64::from(n)), "{n}");
            m = Some(missed(m, t + i64::from(n)));
        }
        assert_eq!(m, Some(Misses { count: MISSES_MAX, since_ms: t + 1 }));
        assert!(!may_try(m, t + 1 + TTL_MS - 1));
        assert!(may_try(m, t + 1 + TTL_MS));
        assert_eq!(missed(m, t + 1 + TTL_MS), Misses { count: 1, since_ms: t + 1 + TTL_MS });
    }

    /// A code lives TTL_MS, and the sweep takes it at the boundary.
    #[test]
    fn a_code_lives_its_ttl() {
        let t = 1_790_000_000_000;
        assert!(live(t, t) && live(t, t + TTL_MS - 1));
        assert!(!live(t, t + TTL_MS) && !live(t, t + TTL_MS + 1));
    }
}
