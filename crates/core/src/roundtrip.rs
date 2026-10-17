//! A browser's round trip (docs/api.md, Sign-in): a platform page sends
//! the browser away, to WorkOS for a sign-in (`/auth/login`, `/auth/link`,
//! `/auth/switch`) or to an own key's provider (`own_signin`), and the other
//! side sends it back to a callback with the state it was given. Anyone who
//! holds a callback's URL, or who began a round trip of their own and sent
//! its page to someone else, could open it in another browser; so a round
//! trip finishes only in the browser that began it. Its state is kept server
//! side (the registry's `logins`, a computer's `own_signins`) and is bound
//! to that browser by a host-only cookie holding it, set as it begins.
//!
//! Every callback finishes through a `Bound`, and only `bind` makes one:
//! from the state the callback's URL brings and the browser's cookie for
//! that kind of round trip, when they are the same state. What finishes a
//! round trip takes a `Bound` by value (the registry's `Exchange`, an own
//! key's exchange in connections.rs), never a state read from a URL, so a
//! new callback cannot skip the binding.

/// A state: 32 random bytes, as 64 lowercase hex.
pub const STATE_HEX_LEN: usize = 64;
/// A round trip is finished within this, or not at all: its cookie's
/// `Max-Age`, and how long its server-side state is kept (the registry's
/// `logins`, `own_signin::SIGNIN_TTL_MS`).
pub const TTL_S: i64 = 600;

/// Which round trip: each has its own cookie, so one kind's state never
/// finishes another's.
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

/// A round trip coming back to the browser that began it: the only way a
/// callback finishes one. Its fields are private, so only `bind` makes it.
#[derive(Debug, PartialEq, Eq)]
pub struct Bound {
    trip: Trip,
    state: String,
}

impl Bound {
    pub fn trip(&self) -> Trip {
        self.trip
    }

    /// The state the server keeps the round trip under.
    pub fn state(&self) -> &str {
        &self.state
    }
}

/// The round trip of kind `trip` a callback finishes: when the state its
/// URL `brought` is one made here and the browser `held` the same in that
/// kind's cookie. Compared in constant time: a callback learns nothing of
/// a cookie it does not hold.
pub fn bind(trip: Trip, brought: &str, held: Option<&str>) -> Option<Bound> {
    let held = held?;
    if !valid_state(brought) || !valid_state(held) {
        return None;
    }
    let differ = brought.bytes().zip(held.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b));
    if differ != 0 {
        return None;
    }
    let bound = Bound { trip, state: brought.to_string() };
    assert!(valid_state(&bound.state), "a bound state is one made here");
    Some(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(c: char) -> String {
        std::iter::repeat_n(c, STATE_HEX_LEN).collect()
    }

    /// Valid: the state the URL brings is the one the browser's cookie
    /// holds, and the round trip is bound, of its kind.
    #[test]
    fn a_state_the_browser_holds_is_bound() {
        let s = state('a');
        let bound = bind(Trip::OwnKey, &s, Some(&s)).expect("the same state binds");
        assert_eq!((bound.trip(), bound.state()), (Trip::OwnKey, s.as_str()));
        assert_eq!(bind(Trip::SignIn, &s, Some(&s)).map(|b| b.trip()), Some(Trip::SignIn));
    }

    /// Invalid: no cookie (another browser), another state (another round
    /// trip, an attacker's sent to someone else), or anything not a state.
    #[test]
    fn another_browser_or_state_binds_nothing() {
        let (a, b) = (state('a'), state('b'));
        assert_eq!(bind(Trip::SignIn, &a, None), None);
        assert_eq!(bind(Trip::SignIn, &a, Some(&b)), None);
        let mut one_off = a.clone();
        one_off.replace_range(63.., "b");
        assert_eq!(bind(Trip::SignIn, &a, Some(&one_off)), None);
        for bad in ["", "a", &a[..63], &format!("{a}0"), &a.to_uppercase(), &format!("{}g", &a[..63])] {
            assert_eq!(bind(Trip::SignIn, bad, Some(bad)), None, "{bad:?}");
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
