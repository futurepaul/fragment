//! Who may do what (docs/MODEL.md, Principals and membership).
//!
//! A caller's role in a fragment is their membership role if they have
//! one. Otherwise it comes from the fragment's visibility: a public
//! fragment gives everyone the `public` floor, and a share link (on a
//! `link` or `public` fragment) counts as a viewer. A `members` fragment
//! gives non-members nothing.
//!
//! An agent's owner reads what the agent reads (FIN-11): a person who owns
//! an agent member reads the fragment as a viewer, and never acts through
//! it (no operations, nothing a membership would let them change).

use fragment_proto::{Role, Visibility};

/// What a caller brings to a fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Standing {
    /// Their own membership.
    pub member: Option<Role>,
    /// They own an agent that is a member.
    pub owns_member_agent: bool,
    /// They hold the share link.
    pub link: bool,
    /// The request is signed (or has a session).
    pub signed: bool,
}

/// Whether the caller reads or acts: an owner's view through their agent
/// counts only for reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Read,
    Act,
}

/// The role a caller acts with, or `None` when they cannot see the fragment.
pub fn effective_role(visibility: Visibility, standing: Standing, purpose: Purpose) -> Option<Role> {
    if standing.member.is_some() {
        return standing.member;
    }
    let floor = match visibility {
        Visibility::Members => None,
        Visibility::Link => standing.link.then_some(Role::Viewer),
        Visibility::Public => Some(if standing.link { Role::Viewer } else { Role::Public }),
    };
    if purpose == Purpose::Read && standing.owns_member_agent {
        return floor.max(Some(Role::Viewer));
    }
    floor
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
pub fn decide(visibility: Visibility, standing: Standing, purpose: Purpose, needs: Role) -> Decision {
    match effective_role(visibility, standing, purpose) {
        Some(role) if role >= needs => Decision::Allow(role),
        _ if standing.signed => Decision::Forbidden,
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

    fn st(member: Option<Role>, link: bool, signed: bool) -> Standing {
        Standing { member, owns_member_agent: false, link, signed }
    }

    fn owner_of_agent() -> Standing {
        Standing { member: None, owns_member_agent: true, link: false, signed: true }
    }

    #[test]
    fn roles_from_visibility() {
        let r = |v, s| effective_role(v, s, Purpose::Read);
        assert_eq!(r(V::Members, st(None, true, false)), None);
        assert_eq!(r(V::Members, st(Some(Viewer), false, false)), Some(Viewer));
        assert_eq!(r(V::Link, st(None, false, false)), None);
        assert_eq!(r(V::Link, st(None, true, false)), Some(Viewer));
        assert_eq!(r(V::Public, st(None, false, false)), Some(Public));
        assert_eq!(r(V::Public, st(None, true, false)), Some(Viewer));
        assert_eq!(r(V::Public, st(Some(Editor), false, false)), Some(Editor));
    }

    #[test]
    fn decisions() {
        let d = |v, s, needs| decide(v, s, Purpose::Act, needs);
        assert_eq!(d(V::Public, st(None, false, false), Public), Decision::Allow(Public));
        assert_eq!(d(V::Public, st(None, false, false), Viewer), Decision::Unauthenticated);
        assert_eq!(d(V::Public, st(None, false, true), Viewer), Decision::Forbidden);
        assert_eq!(d(V::Link, st(None, true, false), Viewer), Decision::Allow(Viewer));
        assert_eq!(d(V::Link, st(None, true, false), Editor), Decision::Unauthenticated);
        assert_eq!(d(V::Members, st(Some(Editor), false, true), Editor), Decision::Allow(Editor));
        assert_eq!(d(V::Members, st(Some(Viewer), false, true), Editor), Decision::Forbidden);
    }

    #[test]
    fn an_agents_owner_reads_and_never_acts() {
        // reads a members-only fragment as a viewer
        assert_eq!(decide(V::Members, owner_of_agent(), Purpose::Read, Viewer), Decision::Allow(Viewer));
        assert_eq!(decide(V::Members, owner_of_agent(), Purpose::Read, Editor), Decision::Forbidden);
        // acts with nothing an agent's membership gave them
        assert_eq!(decide(V::Members, owner_of_agent(), Purpose::Act, Viewer), Decision::Forbidden);
        assert_eq!(decide(V::Members, owner_of_agent(), Purpose::Act, Public), Decision::Forbidden);
        // on a public fragment they act with the floor everyone has
        assert_eq!(decide(V::Public, owner_of_agent(), Purpose::Act, Public), Decision::Allow(Public));
        assert_eq!(decide(V::Public, owner_of_agent(), Purpose::Read, Viewer), Decision::Allow(Viewer));
        // their own membership wins
        let member = Standing { member: Some(Editor), ..owner_of_agent() };
        assert_eq!(decide(V::Members, member, Purpose::Act, Editor), Decision::Allow(Editor));
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
