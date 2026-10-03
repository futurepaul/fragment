//! Test levers (docs/api.md, `FRAGMENT_TEST_SECRET`; docs/secrets.md): the
//! `/api/test/*` routes the e2e pulls, and the sign-in of its people,
//! exist only on a fleet given a test secret, and answer only a request
//! that carries it. A local fleet (dev's and the e2e's, under `wrangler
//! dev`) or a branch deployment (a preview) may have one; a deployment of
//! its own (production) never does: `cargo xtask deploy` refuses such a
//! config, and the cell ignores a secret set on one by hand. Without one
//! the routes answer 404, as any unknown route does; with one, so does a
//! request without it or with another, so a scan cannot tell them apart.
//!
//! The secret is compared in constant time: both sides are hashed first,
//! so neither the compare's time nor its length says how much of a guess
//! was right.

use sha2::{Digest, Sha256};

/// The header a lever's request carries the secret in.
pub const SECRET_HEADER: &str = "x-fragment-test-secret";
/// A secret's length in bytes: 32 random bytes as hex at least (as the
/// host secret is made), at most one line.
pub const SECRET_BYTES_MIN: usize = 32;
pub const SECRET_BYTES_MAX: usize = 256;
/// e2e people sign in as `<name>@e2e.test` (RFC 6761: `.test` is never a
/// real domain), under an issuer no real sign-in has, so an e2e person is
/// never anyone a real sign-in reaches, and a sweep finds them by it.
pub const E2E_EMAIL_DOMAIN: &str = "e2e.test";
pub const E2E_ISSUER: &str = "e2e.test";
/// An e2e email's name, before the `@`.
pub const E2E_EMAIL_NAME_BYTES_MAX: usize = 64;
/// The labels of the fragments the hosted e2e makes start with this, so a
/// sweep deletes only its own.
pub const E2E_LABEL_PREFIX: &str = "e2e-";
/// The paid calls (model calls and AI steps, each one reservation on the
/// ledger) an e2e person may make at most; a hosted run lends each person
/// some of its own budget, and none unless asked.
pub const E2E_PAID_CALLS_MAX: u64 = 200;

const _: () = assert!(SECRET_BYTES_MIN >= 32, "a guess of a secret this long is never made");
const _: () = assert!(SECRET_BYTES_MIN < SECRET_BYTES_MAX);

/// Where a fleet runs, as far as its levers go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fleet {
    /// Dev's or the e2e's, under `wrangler dev` (`FRAGMENT_EGRESS_LOCAL=allow`).
    Local,
    /// A branch deployment, a preview (`FRAGMENT_HOST_LABEL_SUFFIX`).
    Branch,
    /// A deployment of its own: production.
    OwnDeployment,
}

/// Where a fleet runs, from its settings: a branch deployment's fragments'
/// hosts carry its mark (`FRAGMENT_HOST_LABEL_SUFFIX`), which wins (a
/// local rehearsal of a preview is one); a local fleet lets jobs reach
/// local addresses (`FRAGMENT_EGRESS_LOCAL`); anything else is a deployment
/// of its own, production, whatever its other settings.
pub fn fleet_of(branch: bool, local: bool) -> Fleet {
    match (branch, local) {
        (true, _) => Fleet::Branch,
        (false, true) => Fleet::Local,
        (false, false) => Fleet::OwnDeployment,
    }
}

/// What the e2e may make on a branch deployment in one day (UTC), in all:
/// people, and paid calls lent to them. A test secret that leaked spends
/// at most this, and nothing of anyone else's (`Fleet::Branch`'s levers
/// reach the e2e's own fragments and people alone).
pub const E2E_PEOPLE_DAILY_MAX: u64 = 1000;
pub const E2E_PAID_CALLS_DAILY_MAX: u64 = 2000;

const _: () = assert!(E2E_PAID_CALLS_MAX <= E2E_PAID_CALLS_DAILY_MAX, "one person's most fits in a day");

/// One day's e2e sign-ins on a branch deployment, so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Day {
    pub people: u64,
    pub paid_calls: u64,
}

/// Why a day's caps refuse a sign-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayFull {
    People,
    PaidCalls,
}

impl DayFull {
    pub fn message(self) -> String {
        match self {
            DayFull::People => format!("this preview made its {E2E_PEOPLE_DAILY_MAX} e2e people today: try again tomorrow (UTC), or sweep"),
            DayFull::PaidCalls => format!("this preview lent its {E2E_PAID_CALLS_DAILY_MAX} e2e paid calls today: try again tomorrow (UTC)"),
        }
    }
}

/// The day after a sign-in that makes a person (`new`) and lends them
/// `paid_calls`, or why the day's caps refuse it (nothing counted then).
pub fn admit(day: Day, new: bool, paid_calls: u64) -> Result<Day, DayFull> {
    assert!(paid_calls <= E2E_PAID_CALLS_MAX, "the route bounds one sign-in's paid calls");
    let people = day.people + u64::from(new);
    if people > E2E_PEOPLE_DAILY_MAX {
        return Err(DayFull::People);
    }
    let lent = day.paid_calls + paid_calls;
    if lent > E2E_PAID_CALLS_DAILY_MAX {
        return Err(DayFull::PaidCalls);
    }
    Ok(Day { people, paid_calls: lent })
}

/// Whether `name` (`<label>.<username>`) is a fragment the e2e made: its
/// label starts `e2e-`. On a branch deployment, levers reach no other.
pub fn is_e2e_fragment(name: &str) -> bool {
    name.split_once('.').is_some_and(|(label, _)| label.starts_with(E2E_LABEL_PREFIX))
}

/// Why a test secret is not honoured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// It is shorter than `SECRET_BYTES_MIN`.
    TooShort,
    /// It is longer than `SECRET_BYTES_MAX`.
    TooLong,
    /// It holds a byte that is not printable ASCII (a stray newline, say).
    NotText,
    /// The fleet is a deployment of its own: levers are a preview's.
    NotAPreview,
}

impl Refusal {
    pub fn message(self) -> String {
        match self {
            Refusal::TooShort => format!("FRAGMENT_TEST_SECRET is at least {SECRET_BYTES_MIN} bytes"),
            Refusal::TooLong => format!("FRAGMENT_TEST_SECRET is at most {SECRET_BYTES_MAX} bytes"),
            Refusal::NotText => "FRAGMENT_TEST_SECRET is printable ASCII".into(),
            Refusal::NotAPreview => "FRAGMENT_TEST_SECRET is a preview's or a local fleet's, never a deployment of its own: its levers stay off".into(),
        }
    }
}

/// A fleet's test secret. Its `Debug` never shows it.
#[derive(Clone)]
pub struct TestSecret(String);

impl std::fmt::Debug for TestSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TestSecret(…)")
    }
}

/// The test secret `fleet` honours from its variable (`None`: none is set).
pub fn honoured(var: Option<&str>, fleet: Fleet) -> Result<Option<TestSecret>, Refusal> {
    let Some(secret) = var else { return Ok(None) };
    if fleet == Fleet::OwnDeployment {
        return Err(Refusal::NotAPreview);
    }
    check(secret)?;
    Ok(Some(TestSecret(secret.to_string())))
}

/// Whether `secret` may be a test secret (the deploy checks a file's with it).
pub fn check(secret: &str) -> Result<(), Refusal> {
    if secret.len() < SECRET_BYTES_MIN {
        return Err(Refusal::TooShort);
    }
    if secret.len() > SECRET_BYTES_MAX {
        return Err(Refusal::TooLong);
    }
    if !secret.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Refusal::NotText);
    }
    Ok(())
}

impl TestSecret {
    /// Whether a request's header (`SECRET_HEADER`) carries this secret.
    pub fn admits(&self, given: Option<&str>) -> bool {
        let Some(given) = given else { return false };
        // hashed first: the compare is over two 32-byte digests, whatever was sent
        let (given, held) = (Sha256::digest(given.as_bytes()), Sha256::digest(self.0.as_bytes()));
        constant_time_eq(&given, &held)
    }
}

/// `a == b`, in time that depends only on their lengths (which callers
/// make equal: two digests).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    // the fold is not cut short by an optimizer that sees its result early
    std::hint::black_box(diff) == 0
}

/// Whether `email` is an e2e person's: `<name>@e2e.test`, its name 1-64 of
/// lower-case letters, digits, `.`, `_` and `-`.
pub fn valid_e2e_email(email: &str) -> bool {
    let Some((name, domain)) = email.split_once('@') else { return false };
    let name_fits = (1..=E2E_EMAIL_NAME_BYTES_MAX).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    name_fits && domain == E2E_EMAIL_DOMAIN
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// Without a secret no fleet has levers; with one, a preview and a local
    /// fleet do, and a deployment of its own never does.
    #[test]
    fn levers_are_a_previews_alone() {
        for fleet in [Fleet::Local, Fleet::Branch, Fleet::OwnDeployment] {
            assert!(matches!(honoured(None, fleet), Ok(None)), "{fleet:?} without a secret");
        }
        assert!(matches!(honoured(Some(SECRET), Fleet::Branch), Ok(Some(_))));
        assert!(matches!(honoured(Some(SECRET), Fleet::Local), Ok(Some(_))));
        assert_eq!(honoured(Some(SECRET), Fleet::OwnDeployment).err(), Some(Refusal::NotAPreview));
    }

    /// A deployment of its own is whatever is neither a branch nor local:
    /// production, which honours no test secret.
    #[test]
    fn a_fleet_is_production_unless_it_says_otherwise() {
        assert_eq!(fleet_of(false, false), Fleet::OwnDeployment);
        assert_eq!(fleet_of(false, true), Fleet::Local);
        assert_eq!(fleet_of(true, false), Fleet::Branch);
        // a rehearsal of a preview on the local node is a branch: its levers are scoped
        assert_eq!(fleet_of(true, true), Fleet::Branch);
        assert_eq!(honoured(Some(SECRET), fleet_of(false, false)).err(), Some(Refusal::NotAPreview));
    }

    #[test]
    fn a_secret_is_long_printable_text() {
        assert_eq!(check(&SECRET[..SECRET_BYTES_MIN - 1]).err(), Some(Refusal::TooShort));
        assert_eq!(check(&SECRET[..SECRET_BYTES_MIN]), Ok(()));
        assert_eq!(check(&"a".repeat(SECRET_BYTES_MAX)), Ok(()));
        assert_eq!(check(&"a".repeat(SECRET_BYTES_MAX + 1)).err(), Some(Refusal::TooLong));
        assert_eq!(check(&format!("{SECRET}\n")).err(), Some(Refusal::NotText));
        assert_eq!(check(&format!("{} {}", &SECRET[..20], &SECRET[..20])).err(), Some(Refusal::NotText));
        assert_eq!(honoured(Some("short"), Fleet::Branch).err(), Some(Refusal::TooShort));
        // the refusal, and the secret's Debug, never say the secret
        let secret = honoured(Some(SECRET), Fleet::Branch).unwrap().unwrap();
        assert!(!format!("{secret:?}").contains(&SECRET[..8]));
        assert!(!Refusal::NotAPreview.message().contains(&SECRET[..8]));
    }

    /// Only the secret itself is admitted: not a prefix, a longer one, one
    /// of another case, an empty header, or none.
    #[test]
    fn only_the_secret_is_admitted() {
        let secret = honoured(Some(SECRET), Fleet::Branch).unwrap().unwrap();
        assert!(secret.admits(Some(SECRET)));
        for wrong in ["", &SECRET[..SECRET.len() - 1], &format!("{SECRET}0"), &SECRET.to_uppercase(), &SECRET.replacen('0', "1", 1)] {
            assert!(!secret.admits(Some(wrong)), "{wrong:?}");
        }
        assert!(!secret.admits(None));
    }

    #[test]
    fn the_compare_is_whole() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"same bytes", b"same bytes"));
        assert!(!constant_time_eq(b"same bytes", b"same bytez"));
        assert!(!constant_time_eq(b"Same bytes", b"same bytes"));
        assert!(!constant_time_eq(b"short", b"shorter"));
        // every position counts, the last as much as the first
        let a = [7u8; 32];
        for i in 0..32 {
            let mut b = a;
            b[i] ^= 1;
            assert!(!constant_time_eq(&a, &b), "byte {i}");
        }
    }

    /// A day's caps hold at their edge: the last person and call fit, the
    /// next is refused and counts nothing; a sign-in of someone known
    /// makes no one new.
    #[test]
    fn a_days_caps_hold_at_their_edge() {
        let day = admit(Day::default(), true, 5).unwrap();
        assert_eq!(day, Day { people: 1, paid_calls: 5 });
        assert_eq!(admit(day, false, 0).unwrap(), day, "the same person again, lent nothing: nothing counted");
        let full = Day { people: E2E_PEOPLE_DAILY_MAX, paid_calls: 0 };
        assert_eq!(admit(full, true, 0), Err(DayFull::People));
        assert_eq!(admit(full, false, 3).unwrap().paid_calls, 3, "someone known still signs in");
        let lent = Day { people: 1, paid_calls: E2E_PAID_CALLS_DAILY_MAX - 2 };
        assert_eq!(admit(lent, false, 2).unwrap().paid_calls, E2E_PAID_CALLS_DAILY_MAX);
        assert_eq!(admit(lent, false, 3), Err(DayFull::PaidCalls));
        assert!(DayFull::People.message().contains(&E2E_PEOPLE_DAILY_MAX.to_string()));
    }

    #[test]
    fn an_e2e_fragment_is_labelled_so() {
        assert!(is_e2e_fragment("e2e-todo-1a2b.p0123456789"));
        for other in ["todo.paul", "my-e2e-todo.paul", "e2e.paul", "e2e-", "e2e-todo"] {
            assert!(!is_e2e_fragment(other), "{other}");
        }
    }

    #[test]
    fn an_e2e_email_is_at_e2e_test() {
        for ok in ["p-0a1b2c3d4e5f@e2e.test", "operator@e2e.test", "cli-x.y_z@e2e.test", &format!("{}@e2e.test", "a".repeat(64))] {
            assert!(valid_e2e_email(ok), "{ok}");
        }
        for bad in ["paul@example.com", "p@e2e.test.example.com", "p@E2E.test", "P@e2e.test", "@e2e.test", "p@", "p", "p@@e2e.test", "p q@e2e.test", "p+x@e2e.test", &format!("{}@e2e.test", "a".repeat(65))] {
            assert!(!valid_e2e_email(bad), "{bad}");
        }
    }
}
