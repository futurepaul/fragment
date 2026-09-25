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
//!
//! An agent acts for whoever asked, capped (ROADMAP decision 17): a request
//! an agent signs `for=<identity>` acts with the lower of that identity's
//! role and a cap. The cap is the agent's own role (its membership, or the
//! visibility floor), or, on a fragment its owner belongs to, the owner's
//! role; never more than `editor`. The owner's part is the owner's own role
//! capped at `editor`, not `editor` outright: an agent never reaches further
//! than its owner could, so a key that signs `for` someone else gains
//! nothing its owner does not hold. Owner-only actions never go through an
//! agent, whatever it acts for (`owner_only`).

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
    /// An agent acts for the caller this standing describes (its asker):
    /// what caps it. `None`: the caller acts as themselves.
    pub cap: Option<Cap>,
}

/// What an agent acting for someone brings to a fragment (decision 17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cap {
    /// The agent's own membership.
    pub agent: Option<Role>,
    /// Its owner's membership.
    pub owner: Option<Role>,
}

/// The most an agent ever acts with: owner-only actions are never its.
pub const AGENT_ROLE_MAX: Role = Role::Editor;

/// Whether the caller reads or acts: an owner's view through their agent
/// counts only for reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Read,
    Act,
}

/// The role a caller acts with, or `None` when they cannot see the fragment.
/// An agent acting for them (`standing.cap`) acts with the lower of that
/// and its cap.
pub fn effective_role(visibility: Visibility, standing: Standing, purpose: Purpose) -> Option<Role> {
    let own = own_role(visibility, standing, purpose);
    match standing.cap {
        None => own,
        // `None` is below every role: nothing on either side is nothing
        Some(cap) => own.min(cap_role(visibility, cap, standing.link)),
    }
}

fn own_role(visibility: Visibility, standing: Standing, purpose: Purpose) -> Option<Role> {
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

/// The role a cap allows: the agent's own (its membership, or the floor
/// anyone has) or its owner's membership, whichever is higher, and never
/// above `AGENT_ROLE_MAX`.
fn cap_role(visibility: Visibility, cap: Cap, link: bool) -> Option<Role> {
    let agent = own_role(visibility, Standing { member: cap.agent, owns_member_agent: false, link, signed: true, cap: None }, Purpose::Act);
    agent.max(cap.owner).map(|r| r.min(AGENT_ROLE_MAX))
}

/// The role an agent acting for someone holds in a fragment it lists for
/// them, from the memberships alone (the asker's, its own, and its
/// owner's); `None` leaves the fragment out. A call decides again, with
/// the fragment's visibility.
pub fn listed_role(asker: Option<Role>, cap: Cap) -> Option<Role> {
    let standing = Standing { member: asker, owns_member_agent: false, link: false, signed: true, cap: Some(cap) };
    effective_role(Visibility::Members, standing, Purpose::Act)
}

/// Whether a control API request (`method`, and its path under
/// `/api/f/<name>`) is one only the owner makes: members, invites,
/// visibility, link rotation, a capability's grant, deletion. An agent never makes one, whatever
/// it acts for; a member leaving (`DELETE members/me`) is not one.
pub fn owner_only(method: &str, rest: &[&str]) -> bool {
    match (method, rest) {
        ("DELETE", [] | [""]) => true,
        ("PUT", ["members", _]) => true,
        ("DELETE", ["members", who]) => *who != "me",
        (_, ["invites", ..]) => true,
        ("PUT", ["visibility"]) => true,
        ("POST", ["rotate"]) => true,
        ("PUT", ["grants", _]) => true,
        _ => false,
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
        Standing { member, owns_member_agent: false, link, signed, cap: None }
    }

    fn owner_of_agent() -> Standing {
        Standing { member: None, owns_member_agent: true, link: false, signed: true, cap: None }
    }

    /// An agent acting for someone whose membership is `asker`, beside its
    /// own membership and its owner's.
    fn acting(asker: Option<Role>, agent: Option<Role>, owner: Option<Role>) -> Standing {
        Standing { member: asker, owns_member_agent: false, link: false, signed: true, cap: Some(Cap { agent, owner }) }
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
    fn an_agent_acts_for_its_asker_capped() {
        let d = |v, s, needs| decide(v, s, Purpose::Act, needs);
        // the owner's turn on an app the owner made, which the agent is not in
        assert_eq!(d(V::Members, acting(Some(Owner), None, Some(Owner)), Editor), Decision::Allow(Editor));
        // a guest who is in the owner's chat only: the owner's app is out of reach
        assert_eq!(d(V::Members, acting(None, None, Some(Owner)), Viewer), Decision::Forbidden);
        assert_eq!(d(V::Link, acting(None, Some(Editor), Some(Owner)), Public), Decision::Forbidden);
        // what the owner shared with the guest: the guest's own role
        assert_eq!(d(V::Members, acting(Some(Editor), None, Some(Owner)), Editor), Decision::Allow(Editor));
        assert_eq!(d(V::Members, acting(Some(Viewer), None, Some(Owner)), Editor), Decision::Forbidden);
        // what neither the agent nor its owner is in: nothing, whoever asks
        assert_eq!(d(V::Members, acting(Some(Owner), None, None), Viewer), Decision::Forbidden);
        // the agent's own membership caps it
        assert_eq!(d(V::Members, acting(Some(Editor), Some(Viewer), None), Editor), Decision::Forbidden);
        assert_eq!(d(V::Members, acting(Some(Editor), Some(Viewer), None), Viewer), Decision::Allow(Viewer));
        // never above editor: owner-only actions are never an agent's
        assert_eq!(d(V::Members, acting(Some(Owner), Some(Owner), Some(Owner)), Owner), Decision::Forbidden);
        assert_eq!(effective_role(V::Members, acting(Some(Owner), Some(Owner), Some(Owner)), Purpose::Act), Some(Editor));
        // never further than its owner: an owner who only views caps a guest who edits
        assert_eq!(effective_role(V::Members, acting(Some(Editor), None, Some(Viewer)), Purpose::Act), Some(Viewer));
        // the floor anyone has: a public fragment, for anyone
        assert_eq!(d(V::Public, acting(None, None, None), Public), Decision::Allow(Public));
        assert_eq!(d(V::Public, acting(None, None, None), Viewer), Decision::Forbidden);
        // the link is not the agent's to hold for anyone
        assert_eq!(effective_role(V::Link, acting(None, None, None), Purpose::Read), None);
        // an asker who reads through their own agent's membership reads, and never acts
        let owner_reads = Standing { owns_member_agent: true, ..acting(None, Some(Editor), None) };
        assert_eq!(decide(V::Members, owner_reads, Purpose::Read, Viewer), Decision::Allow(Viewer));
        assert_eq!(decide(V::Members, owner_reads, Purpose::Act, Viewer), Decision::Forbidden);
    }

    #[test]
    fn listing_for_an_asker() {
        let cap = |agent, owner| Cap { agent, owner };
        assert_eq!(listed_role(Some(Owner), cap(None, Some(Owner))), Some(Editor));
        assert_eq!(listed_role(Some(Viewer), cap(Some(Editor), None)), Some(Viewer));
        assert_eq!(listed_role(Some(Editor), cap(None, None)), None);
        assert_eq!(listed_role(None, cap(Some(Editor), Some(Owner))), None);
    }

    #[test]
    fn owner_only_routes() {
        for (method, rest) in [
            ("DELETE", &[][..]),
            ("PUT", &["members", "id:00"][..]),
            ("DELETE", &["members", "npub1x"][..]),
            ("POST", &["invites"][..]),
            ("GET", &["invites"][..]),
            ("DELETE", &["invites", "ab12"][..]),
            ("PUT", &["visibility"][..]),
            ("POST", &["rotate"][..]),
            ("PUT", &["grants", "frame"][..]),
        ] {
            assert!(owner_only(method, rest), "{method} {rest:?}");
        }
        for (method, rest) in [
            ("DELETE", &["members", "me"][..]),
            ("GET", &["members"][..]),
            ("GET", &["status"][..]),
            ("POST", &["ops", "add"][..]),
            ("POST", &["files"][..]),
            ("POST", &["deploy"][..]),
            ("PUT", &["secrets", "K"][..]),
        ] {
            assert!(!owner_only(method, rest), "{method} {rest:?}");
        }
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
