//! A browser's round trip (docs/api.md, Sign-in): a platform page sends
//! the browser away, to WorkOS for a sign-in (`/auth/login`, `/auth/link`,
//! `/auth/switch`) or to an own key's provider (`own_signin`), and the other
//! side sends it back to a callback with the state it was given. Anyone who
//! holds a callback's URL, or who began a round trip of their own and sent
//! its page to someone else, could open it in another browser; so a round
//! trip finishes only in the browser that began it. Its state is kept server
//! side (the registry's `logins`, a computer's `own_signins`) and is bound
//! to that browser by a host-only cookie of its kind holding it, set as it
//! begins.
//!
//! The pure half lives here: the kinds, their cookies, a state's shape, and
//! the comparison. The binding itself is the cell's `auth::Bound`, which
//! only `auth::trip_bound` makes, from the request's own cookie: what
//! finishes a round trip takes one, never a state read from a URL.

/// A state: 32 random bytes, as 64 lowercase hex.
pub const STATE_HEX_LEN: usize = 64;
/// A round trip is finished within this, or not at all: its cookie's
/// `Max-Age`, and how long its server-side state is kept (the registry's
/// `logins`, `own_signin::SIGNIN_TTL_MS`).
pub const TTL_S: i64 = 600;

/// Which round trip: each has its own cookie, so one kind's state never
/// finishes another's. A browser holds one of each at a time: a second
/// round trip of a kind begun in it replaces the first's binding, and the
/// first's callback is refused (connect, or sign in, again).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trip {
    /// A sign-in at WorkOS (`/auth/login`), or one adding a second sign-in
    /// to the signed-in person (`/auth/link`, whose person the registry
    /// keeps with the state).
    SignIn,
    /// "Use another account" (`/auth/switch`): a sign-in, its state bound
    /// apart from one's, as WorkOS may answer it from the session it held.
    Switch,
    /// An own key's sign-in at its provider (`own_signin`), begun by the
    /// signed-in person from settings.
    OwnKey,
}

impl Trip {
    pub const ALL: [Trip; 3] = [Trip::SignIn, Trip::Switch, Trip::OwnKey];

    /// The cookie that binds its state (`__Host-` first over https).
    pub const fn cookie(self) -> &'static str {
        match self {
            Trip::SignIn => "fragment_login",
            Trip::Switch => "fragment_switch",
            Trip::OwnKey => "fragment_own_key",
        }
    }
}

/// Whether `s` is a state as made here: 64 lowercase hex.
pub fn valid_state(s: &str) -> bool {
    s.len() == STATE_HEX_LEN && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Whether the state a callback's URL `brought` is the one the browser's
/// cookie `held`: both states as made here, and equal. Compared in
/// constant time, so a callback learns nothing of a cookie it does not
/// hold.
pub fn same_state(brought: &str, held: &str) -> bool {
    if !valid_state(brought) || !valid_state(held) {
        return false;
    }
    let differ = brought.bytes().zip(held.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b));
    differ == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(c: char) -> String {
        std::iter::repeat_n(c, STATE_HEX_LEN).collect()
    }

    /// Valid: the state the URL brings is the one the cookie holds.
    #[test]
    fn a_state_the_browser_holds_is_the_same() {
        let s = state('a');
        assert!(same_state(&s, &s.clone()));
    }

    /// Invalid: another state (another round trip, an attacker's sent to
    /// someone else), one that differs in its last place, or anything that
    /// is not a state, even when both sides are alike.
    #[test]
    fn another_state_or_none_is_never_the_same() {
        let (a, b) = (state('a'), state('b'));
        assert!(!same_state(&a, &b));
        let mut one_off = a.clone();
        one_off.replace_range(63.., "b");
        assert!(!same_state(&a, &one_off));
        for bad in ["", "a", &a[..63], &format!("{a}0"), &a.to_uppercase(), &format!("{}g", &a[..63])] {
            assert!(!same_state(bad, bad), "{bad:?}");
            assert!(!valid_state(bad), "{bad:?}");
        }
    }

    /// Each kind of round trip has a cookie of its own.
    #[test]
    fn each_trip_has_its_own_cookie() {
        let names: std::collections::BTreeSet<&str> = Trip::ALL.iter().map(|t| t.cookie()).collect();
        assert_eq!(names.len(), Trip::ALL.len());
        assert!(names.iter().all(|n| n.starts_with("fragment_")));
    }
}
