//! Who may do what (docs/MODEL.md, Principals and membership).
//!
//! A caller's role in a fragment is their membership role if they have
//! one. Otherwise it comes from the fragment's visibility: a public
//! fragment gives everyone the `public` floor, and a share link (on a
//! `link` or `public` fragment) counts as a viewer. A `members` fragment
//! gives non-members nothing.

use fragment_proto::{Role, Visibility};

/// The role a caller acts with, or `None` when they cannot see the fragment.
pub fn effective_role(visibility: Visibility, member: Option<Role>, link: bool) -> Option<Role> {
    if member.is_some() {
        return member;
    }
    match visibility {
        Visibility::Members => None,
        Visibility::Link => link.then_some(Role::Viewer),
        Visibility::Public => Some(if link { Role::Viewer } else { Role::Public }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow(Role),
    /// Anonymous and short of the role: signing in could help (401).
    Unauthenticated,
    /// Signed and still short of the role (403).
    Forbidden,
}

/// Whether a caller may do something that needs `needs`.
pub fn decide(visibility: Visibility, member: Option<Role>, link: bool, signed: bool, needs: Role) -> Decision {
    match effective_role(visibility, member, link) {
        Some(role) if role >= needs => Decision::Allow(role),
        _ if signed => Decision::Forbidden,
        _ => Decision::Unauthenticated,
    }
}

/// Why a membership change is refused, or `None` when it is allowed. Only
/// the owner manages members; the owner's own row is never changed here
/// (there is no owner transfer yet); `public` is a floor, not a grant.
pub fn refuse_set_role(actor: Option<Role>, target_current: Option<Role>, new_role: Role) -> Option<&'static str> {
    if actor != Some(Role::Owner) {
        return Some("only the owner manages members");
    }
    if target_current == Some(Role::Owner) {
        return Some("the owner's role cannot be changed");
    }
    match new_role {
        Role::Viewer | Role::Editor => None,
        Role::Owner => Some("a fragment has one owner; ownership transfer is not supported"),
        Role::Public => Some("`public` is what everyone gets on a public fragment; it cannot be granted"),
    }
}

/// Why removing `target` is refused, or `None`. The owner removes anyone
/// but themselves; any other member may leave.
pub fn refuse_remove(actor: Option<Role>, actor_is_target: bool, target_current: Option<Role>) -> Option<&'static str> {
    match target_current {
        None => Some("not a member"),
        Some(Role::Owner) => Some("the owner cannot be removed"),
        Some(_) if actor_is_target || actor == Some(Role::Owner) => None,
        Some(_) => Some("only the owner removes other members"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Role::*;
    use Visibility as V;

    #[test]
    fn roles_from_visibility() {
        assert_eq!(effective_role(V::Members, None, true), None);
        assert_eq!(effective_role(V::Members, Some(Viewer), false), Some(Viewer));
        assert_eq!(effective_role(V::Link, None, false), None);
        assert_eq!(effective_role(V::Link, None, true), Some(Viewer));
        assert_eq!(effective_role(V::Public, None, false), Some(Public));
        assert_eq!(effective_role(V::Public, None, true), Some(Viewer));
        assert_eq!(effective_role(V::Public, Some(Editor), false), Some(Editor));
    }

    #[test]
    fn decisions() {
        assert_eq!(decide(V::Public, None, false, false, Public), Decision::Allow(Public));
        assert_eq!(decide(V::Public, None, false, false, Viewer), Decision::Unauthenticated);
        assert_eq!(decide(V::Public, None, false, true, Viewer), Decision::Forbidden);
        assert_eq!(decide(V::Link, None, true, false, Viewer), Decision::Allow(Viewer));
        assert_eq!(decide(V::Link, None, true, false, Editor), Decision::Unauthenticated);
        assert_eq!(decide(V::Members, Some(Editor), false, true, Editor), Decision::Allow(Editor));
        assert_eq!(decide(V::Members, Some(Viewer), false, true, Editor), Decision::Forbidden);
    }

    #[test]
    fn membership_changes() {
        assert!(refuse_set_role(Some(Owner), None, Editor).is_none());
        assert!(refuse_set_role(Some(Owner), Some(Viewer), Editor).is_none());
        assert!(refuse_set_role(Some(Editor), None, Viewer).is_some());
        assert!(refuse_set_role(Some(Owner), Some(Owner), Viewer).is_some());
        assert!(refuse_set_role(Some(Owner), None, Owner).is_some());
        assert!(refuse_set_role(Some(Owner), None, Public).is_some());
        assert!(refuse_remove(Some(Owner), false, Some(Editor)).is_none());
        assert!(refuse_remove(Some(Viewer), true, Some(Viewer)).is_none());
        assert!(refuse_remove(Some(Editor), false, Some(Viewer)).is_some());
        assert!(refuse_remove(Some(Owner), true, Some(Owner)).is_some());
        assert!(refuse_remove(Some(Owner), false, None).is_some());
    }
}
