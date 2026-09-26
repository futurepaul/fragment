//! The identity registry's rules (docs/finite-integration.md, rules 1–7;
//! the cell is `cell/src/registry.rs`). Who may change an identity's keys,
//! and who may look at it.

use fragment_proto::IdentityKind;

/// Who manages an identity's keys (rule 4): a person manages their own; an
/// agent's and a computer's are managed by their owner, never by
/// themselves (a computer that approved keys would be its owner's reach).
pub fn may_manage_keys(by: &str, identity: &str, kind: IdentityKind, owner: Option<&str>) -> bool {
    match kind {
        IdentityKind::Person => by == identity,
        IdentityKind::Agent | IdentityKind::Computer => owner == Some(by),
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
        // nor does a computer: its owner does
        assert!(may_manage_keys("id:p", "id:c", Computer, Some("id:p")));
        assert!(!may_manage_keys("id:c", "id:c", Computer, Some("id:p")));
        assert!(!may_manage_keys("id:c", "id:p", Person, None));
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
}
