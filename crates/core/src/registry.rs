//! The identity registry's rules (docs/finite-integration.md, rules 1–7;
//! the cell is `cell/src/registry.rs`). Who may change an identity's keys,
//! and who may look at it.

use fragment_proto::IdentityKind;

/// Who manages an identity's keys (rule 4): a person manages their own; an
/// agent's are managed by its owner, never by itself.
pub fn may_manage_keys(by: &str, identity: &str, kind: IdentityKind, owner: Option<&str>) -> bool {
    match kind {
        IdentityKind::Person => by == identity,
        IdentityKind::Agent => owner == Some(by),
    }
}

/// Who sees an identity's keys and agents: itself and its owner.
pub fn may_view(by: &str, identity: &str, owner: Option<&str>) -> bool {
    by == identity || owner == Some(by)
}

/// Who may ask whether a key is one of `identity`'s: the identity itself,
/// or an agent it owns (an agent's runtime checks that a request comes
/// from its owner, whose keys may change).
pub fn may_check_key(by: &str, by_owner: Option<&str>, identity: &str) -> bool {
    by == identity || by_owner == Some(identity)
}

/// Who pairs a machine's key to an agent, unpairs it, or lists them (a
/// person's own machine as their agent's hands: docs/api.md, "A machine's
/// keys"): the agent's owner, a person, and no agent (not even one acting
/// for them).
pub fn may_pair(by: &str, by_kind: IdentityKind, agent_kind: IdentityKind, agent_owner: Option<&str>) -> bool {
    by_kind == IdentityKind::Person && agent_kind == IdentityKind::Agent && agent_owner == Some(by)
}

/// A machine's name, as its pairing gives it (its host's): 1 to
/// `MACHINE_NAME_MAX_BYTES` of letters, digits, `.`, `_` and `-`, starting
/// with a letter or a digit. Pages and the mind show it as is.
pub fn valid_machine_name(name: &str) -> bool {
    (1..=fragment_proto::limits::MACHINE_NAME_MAX_BYTES).contains(&name.len())
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

/// What the registry holds of a key a pairing names, for the agent it is
/// paired to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// No one holds it.
    Free,
    /// The agent holds it as a machine's: `active` unless unpaired.
    Paired { active: bool },
    /// The agent holds it, not as a machine's (its fragment's own key, or
    /// one its owner added): `active` unless revoked.
    Own { active: bool },
    /// Another identity holds (or held) it.
    Other,
}

/// Why a key is not paired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairRefusal {
    /// Someone else holds it.
    Taken,
    /// It was unpaired or revoked: a key that stopped never signs again.
    Stopped,
    /// It is already the agent's own, as no machine's.
    Own,
    /// The agent holds as many machines' keys as it may.
    Full,
}

impl PairRefusal {
    /// A conflict with what is held (409), or the cap (400).
    pub fn conflict(self) -> bool {
        !matches!(self, PairRefusal::Full)
    }

    pub fn message(self) -> String {
        match self {
            PairRefusal::Taken => "this key already belongs to someone".into(),
            PairRefusal::Stopped => "this key was unpaired: a key that stopped stays stopped; pair the machine again with a new one".into(),
            PairRefusal::Own => "this key is already the agent's own, not a machine's".into(),
            PairRefusal::Full => format!(
                "an agent holds at most {} machines' keys at once: unpair one first",
                fragment_proto::limits::PAIRED_KEYS_PER_AGENT_MAX
            ),
        }
    }
}

/// Whether pairing a key the registry holds as `held` adds it (`true`), is
/// a replay of its own pairing (`false`), or is refused, the agent holding
/// `active` machines' keys now.
pub fn pairing(held: Held, active: u64) -> Result<bool, PairRefusal> {
    match held {
        Held::Paired { active: true } => Ok(false),
        Held::Paired { active: false } | Held::Own { active: false } => Err(PairRefusal::Stopped),
        Held::Own { active: true } => Err(PairRefusal::Own),
        Held::Other => Err(PairRefusal::Taken),
        Held::Free if active >= fragment_proto::limits::PAIRED_KEYS_PER_AGENT_MAX => Err(PairRefusal::Full),
        Held::Free => Ok(true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use IdentityKind::*;

    #[test]
    fn keys_are_managed_by_the_human() {
        assert!(may_manage_keys("id:p", "id:p", Person, None));
        assert!(!may_manage_keys("id:q", "id:p", Person, None));
        assert!(may_manage_keys("id:p", "id:a", Agent, Some("id:p")));
        // an agent never manages its own keys, and nobody else's
        assert!(!may_manage_keys("id:a", "id:a", Agent, Some("id:p")));
        assert!(!may_manage_keys("id:q", "id:a", Agent, Some("id:p")));
        assert!(!may_manage_keys("id:a", "id:p", Person, None));
    }

    #[test]
    fn who_sees_what() {
        assert!(may_view("id:p", "id:p", None));
        assert!(may_view("id:p", "id:a", Some("id:p")));
        assert!(!may_view("id:q", "id:a", Some("id:p")));
        assert!(may_check_key("id:a", Some("id:p"), "id:p"));
        assert!(may_check_key("id:p", None, "id:p"));
        assert!(!may_check_key("id:b", Some("id:q"), "id:p"));
    }

    /// Goal: only an agent's owner, a person, pairs a machine to it.
    /// Method: the owner, another person, the agent itself, another agent
    /// (as one acting for the owner signs), and a person named as the
    /// agent.
    #[test]
    fn only_the_owner_pairs_a_machine() {
        assert!(may_pair("id:p", Person, Agent, Some("id:p")));
        assert!(!may_pair("id:q", Person, Agent, Some("id:p")), "another person");
        assert!(!may_pair("id:a", Agent, Agent, Some("id:p")), "the agent itself");
        assert!(!may_pair("id:p", Agent, Agent, Some("id:p")), "an agent, whoever it names");
        assert!(!may_pair("id:p", Person, Person, None), "a person has no machines' keys");
    }

    /// Goal: a machine's name is a host's, shown as is. Method: names a
    /// host gives, and ones with spaces, slashes, control characters, a
    /// leading dash, or past the bound.
    #[test]
    fn a_machine_is_named_as_its_host() {
        for ok in ["paulbox", "Pauls-MacBook-Pro.local", "a", "box_2", &"x".repeat(63)] {
            assert!(valid_machine_name(ok), "{ok:?}");
        }
        for bad in ["", "-box", ".box", "my box", "a/b", "tab\tbox", "é", &"x".repeat(64)] {
            assert!(!valid_machine_name(bad), "{bad:?}");
        }
    }

    /// Goal: a pairing adds a free key once, replays as itself, and never
    /// brings back a stopped key, takes another's, or passes the cap.
    /// Method: every state a key can be in, at the cap and below it.
    #[test]
    fn a_pairing_adds_a_free_key_once() {
        use PairRefusal::*;
        let max = fragment_proto::limits::PAIRED_KEYS_PER_AGENT_MAX;
        assert_eq!(pairing(Held::Free, 0), Ok(true));
        assert_eq!(pairing(Held::Free, max - 1), Ok(true));
        assert_eq!(pairing(Held::Free, max), Err(Full), "the cap");
        assert_eq!(pairing(Held::Paired { active: true }, max), Ok(false), "a replay, even at the cap");
        assert_eq!(pairing(Held::Paired { active: false }, 0), Err(Stopped), "unpaired stays unpaired");
        assert_eq!(pairing(Held::Own { active: false }, 0), Err(Stopped));
        assert_eq!(pairing(Held::Own { active: true }, 0), Err(Own));
        assert_eq!(pairing(Held::Other, 0), Err(Taken));
        assert!(Taken.conflict() && Stopped.conflict() && Own.conflict() && !Full.conflict());
        assert!(Full.message().contains(&max.to_string()));
    }
}
