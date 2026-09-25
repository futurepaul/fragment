//! A platform form's token (phase 7, decision 4): bound to the browser's
//! session and to what the form does, and good only from a short delay
//! after the form was shown until it goes stale.
//!
//! A fragment's page is one site with the platform, so a form or fetch it
//! sends carries the platform's session cookie. The Origin check refuses a
//! browser's; this token refuses whatever did not read the platform's own
//! page first: it is an HMAC of the form's purpose and the time the page
//! was made, keyed by the session's token (the HttpOnly cookie, which no
//! page's script reads, and which a request carries). Another session's
//! token, another purpose's, or a forged time is refused.
//!
//! The delay is the server's half of a page's armed buttons: a click that
//! opened the page (a double-click's second half) lands before it, so a
//! page cannot be confirmed by the gesture that opened it.

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// A form is good this long after its page was made …
pub const DELAY_MS: i64 = 800;
/// … and until this long after.
pub const MAX_AGE_MS: i64 = 12 * 3600 * 1000;
/// `<ms>.<64 hex>`: a time of at most 15 digits (until the year 33658).
const TOKEN_MAX: usize = 15 + 1 + 64;

/// Why a form's token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Not a token at all (none sent, or not `<ms>.<hex>`).
    Malformed,
    /// Made for another session, another purpose, or another time.
    NotThisForm,
    /// Sent sooner than `DELAY_MS` after its page was made.
    TooSoon,
    /// Older than `MAX_AGE_MS`.
    Stale,
}

impl Refused {
    pub fn message(self) -> &'static str {
        match self {
            Refused::Malformed | Refused::NotThisForm => "this form is not from your own page: open it again",
            Refused::TooSoon => "that was too quick: the page's buttons wait a moment before they act",
            Refused::Stale => "this page is out of date: open it again",
        }
    }
}

fn mac(session: &str, purpose: &str, at_ms: i64) -> Hmac<Sha256> {
    let mut h = Hmac::<Sha256>::new_from_slice(session.as_bytes()).expect("HMAC takes any key length");
    // lengths first: no purpose can end where another's time begins
    h.update(format!("fragment-form/1\n{}\n{purpose}\n{at_ms}", purpose.len()).as_bytes());
    h
}

/// The token for a form `purpose` (`share:<name>`, `join:<name>`) on a page
/// made at `now_ms` for the session whose cookie is `session`.
pub fn issue(session: &str, purpose: &str, now_ms: i64) -> String {
    assert!(now_ms >= 0, "a page is made after 1970");
    format!("{now_ms}.{}", hex::encode(mac(session, purpose, now_ms).finalize().into_bytes()))
}

/// Whether `token` is this session's for `purpose`, sent at `now_ms`
/// between `DELAY_MS` and `MAX_AGE_MS` after its page was made.
pub fn check(session: &str, purpose: &str, token: &str, now_ms: i64) -> Result<(), Refused> {
    if token.is_empty() || token.len() > TOKEN_MAX {
        return Err(Refused::Malformed);
    }
    let (at, sig) = token.split_once('.').ok_or(Refused::Malformed)?;
    if at.is_empty() || !at.bytes().all(|b| b.is_ascii_digit()) || sig.len() != 64 {
        return Err(Refused::Malformed);
    }
    let at: i64 = at.parse().map_err(|_| Refused::Malformed)?;
    let sig = hex::decode(sig).map_err(|_| Refused::Malformed)?;
    // compared in constant time
    mac(session, purpose, at).verify_slice(&sig).map_err(|_| Refused::NotThisForm)?;
    let age = now_ms - at;
    if age < DELAY_MS {
        return Err(Refused::TooSoon);
    }
    if age > MAX_AGE_MS {
        return Err(Refused::Stale);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "5f2a3a8e0f4c1d9b7a6e5d4c3b2a19087f6e5d4c3b2a19087f6e5d4c3b2a1908";
    const T: i64 = 1_790_000_000_000;

    #[test]
    fn a_token_is_good_from_the_delay_until_it_is_stale() {
        let token = issue(SESSION, "share:chat.paul", T);
        assert_eq!(check(SESSION, "share:chat.paul", &token, T + DELAY_MS), Ok(()));
        assert_eq!(check(SESSION, "share:chat.paul", &token, T + MAX_AGE_MS), Ok(()));
        assert_eq!(check(SESSION, "share:chat.paul", &token, T + DELAY_MS - 1), Err(Refused::TooSoon));
        assert_eq!(check(SESSION, "share:chat.paul", &token, T), Err(Refused::TooSoon));
        assert_eq!(check(SESSION, "share:chat.paul", &token, T + MAX_AGE_MS + 1), Err(Refused::Stale));
    }

    #[test]
    fn another_session_purpose_or_time_is_refused() {
        let token = issue(SESSION, "share:chat.paul", T);
        let later = T + DELAY_MS;
        let other = SESSION.replace('5', "6");
        assert_eq!(check(&other, "share:chat.paul", &token, later), Err(Refused::NotThisForm));
        assert_eq!(check(SESSION, "join:chat.paul", &token, later), Err(Refused::NotThisForm));
        assert_eq!(check(SESSION, "share:todo.paul", &token, later), Err(Refused::NotThisForm));
        // an earlier time, to skip the delay, breaks the MAC
        let sig = token.split_once('.').unwrap().1;
        assert_eq!(check(SESSION, "share:chat.paul", &format!("{}.{sig}", T - DELAY_MS), T), Err(Refused::NotThisForm));
    }

    #[test]
    fn a_malformed_token_is_refused_never_a_panic() {
        let good = issue(SESSION, "p", T);
        let sig = good.split_once('.').unwrap().1;
        for bad in ["", ".", "x", "123", "123.", ".abc", &format!("-5.{sig}"), &format!("+5.{sig}"), &format!("1e3.{sig}"), &format!("{T}.{}", &sig[..63]), &format!("{T}.{}g", &sig[..63]), &format!("99999999999999999999.{sig}")] {
            assert_eq!(check(SESSION, "p", bad, T + DELAY_MS), Err(Refused::Malformed), "{bad:?}");
        }
    }
}
