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
//! nothing its owner does not hold.
//!
//! Decision 36 adds two limits. A share its fragment's owner marked
//! "people only" lends an agent nothing: its owner's membership there does
//! not count in the agent's cap. And an agent its owner holds below them
//! acts with at most its hold, wherever it is and whomever it acts for.
//!
//! Some control routes need more than a role (`reserved`). Sharing
//! (members, invites, visibility, the links) is the owner's, and, since
//! Paul's "yes, your agent can share on your behalf" (2026-10-04), their
//! own agent's acting for them: an agent that acts for its own owner, held
//! at nothing below them, shares a fragment its owner owns as its owner
//! would (`agent_shares`), under the same role rules (it never makes
//! anyone owner, itself included). For anyone else, as itself, held, or on
//! a fragment its owner only edits or views, it shares nothing. Deleting a
//! fragment and its money cap stay the owner's own: never an agent's,
//! whomever it acts for. What an agent shares is recorded as the agent's,
//! for its owner, so its owner sees it did.

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
    /// The caller is an agent held below its owner: the most it acts with.
    pub held: Option<Role>,
}

/// What an agent acting for someone brings to a fragment (decision 17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cap {
    /// The agent's own membership.
    pub agent: Option<Role>,
    /// Its owner's membership.
    pub owner: Option<Role>,
    /// The owner's share is "people only": it lends the agent nothing.
    pub people_only: bool,
}

/// The highest role an agent ever acts with: owning is never its. Sharing
/// for its owner (`agent_shares`) is its owner's authority lent on the
/// sharing routes alone, not a role it holds anywhere else.
pub const AGENT_ROLE_MAX: Role = Role::Editor;
// an agent's role never reaches the role sharing needs: only the lent
// authority shares, and only where `agent_shares` lends it
const _: () = assert!((AGENT_ROLE_MAX as u8) < (Role::Owner as u8));

/// Whether the caller reads or acts: an owner's view through their agent
/// counts only for reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Read,
    Act,
}

/// The role a caller acts with, or `None` when they cannot see the fragment.
/// An agent acting for them (`standing.cap`) acts with the lower of that
/// and its cap, and of its hold: a hold limits what an agent does with its
/// owner's access (decision 36; Paul, 2026-10-05), never its own
/// memberships, so an agent held at viewer still answers in the chats it is
/// a member of, recording its turns there.
pub fn effective_role(visibility: Visibility, standing: Standing, purpose: Purpose) -> Option<Role> {
    let own = own_role(visibility, standing, purpose);
    match standing.cap {
        None => own,
        // `None` is below every role: nothing on either side is nothing
        Some(cap) => {
            let role = own.min(cap_role(visibility, cap, standing.link));
            match standing.held {
                Some(held) => role.map(|r| r.min(held)),
                None => role,
            }
        }
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
    let agent = own_role(visibility, Standing { member: cap.agent, owns_member_agent: false, link, signed: true, cap: None, held: None }, Purpose::Act);
    let owner = if cap.people_only { None } else { cap.owner };
    agent.max(owner).map(|r| r.min(AGENT_ROLE_MAX))
}

/// The role an agent acting for someone holds in a fragment it lists for
/// them, from the memberships alone (the asker's, its own, and its
/// owner's); `None` leaves the fragment out. A call decides again, with
/// the fragment's visibility.
pub fn listed_role(asker: Option<Role>, cap: Cap) -> Option<Role> {
    let standing = Standing { member: asker, owns_member_agent: false, link: false, signed: true, cap: Some(cap), held: None };
    effective_role(Visibility::Members, standing, Purpose::Act)
}

/// What a control request needs beyond a role (`reserved`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reserved {
    /// Sharing: adding, changing, or removing members (but not leaving),
    /// invites, visibility, and rotating the links. The owner's, and their
    /// own agent's acting for them (`agent_shares`; Paul, 2026-10-04).
    Sharing,
    /// The owner's own: deleting the fragment, and its money cap
    /// (docs/ledger.md). Never through any agent, whomever it acts for.
    OwnerOnly,
}

/// What a control API request (`method`, and its path under
/// `/api/f/<name>`) reserves, or `None` when a role alone decides it. A
/// member leaving (`DELETE members/me`) reserves nothing. The router asks
/// this of its undecoded path; the fragment's handlers decide again on
/// their own, so a path spelled otherwise (`%69nvites`) gains nothing.
pub fn reserved(method: &str, rest: &[&str]) -> Option<Reserved> {
    match (method, rest) {
        ("DELETE", [] | [""]) => Some(Reserved::OwnerOnly),
        // a fragment's cap is its owner's money (docs/ledger.md)
        ("PUT", ["cap"]) => Some(Reserved::OwnerOnly),
        ("PUT", ["members", _]) => Some(Reserved::Sharing),
        ("DELETE", ["members", "me"]) => None,
        ("DELETE", ["members", _]) => Some(Reserved::Sharing),
        (_, ["invites", ..]) => Some(Reserved::Sharing),
        ("PUT", ["visibility"]) => Some(Reserved::Sharing),
        ("POST", ["rotate"]) => Some(Reserved::Sharing),
        _ => None,
    }
}

/// An agent on a reserved route, as who it is says: what the router knows
/// of it from the registry, before any fragment is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sharer {
    /// It acts for its own owner (`for=<its owner>`): not as itself, and
    /// not for anyone else.
    pub for_owner: bool,
    /// Its owner holds it below them (decision 36): the most it acts with.
    pub held: Option<Role>,
}

/// The agent's owner's share of a fragment, as the fragment keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerShare {
    /// The owner's membership.
    pub role: Option<Role>,
    /// That share is people only: it lends the owner's agents nothing.
    pub people_only: bool,
}

/// Why an agent's request on a reserved route carries no owner's
/// authority (403).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareRefusal {
    /// Deleting the fragment, or setting its cap: its owner's own.
    OwnerOnly,
    /// It acts as itself, or for someone other than its owner.
    NotForOwner,
    /// Its owner holds it below them, and sharing is above every hold.
    Held,
    /// Its owner does not own the fragment: their own role there caps it,
    /// and only an owner shares.
    NotOwner,
    /// Its owner's share there is people only: it lends the agent nothing.
    PeopleOnly,
}

impl ShareRefusal {
    pub fn message(self) -> &'static str {
        match self {
            ShareRefusal::OwnerOnly => "an agent never deletes a fragment or sets its cap: its owner does",
            ShareRefusal::NotForOwner => "an agent shares only acting for its own owner (`for=<its owner>`), never as itself or for anyone else",
            ShareRefusal::Held => "an agent its owner holds below them shares nothing",
            ShareRefusal::NotOwner => "an agent shares only what its owner owns",
            ShareRefusal::PeopleOnly => "its owner's share here is people only: it lends their agents nothing",
        }
    }
}

/// The half of `agent_shares` that who the agent is decides (the router
/// asks it): whether its request on a reserved route may go on to the
/// fragment, which decides the rest from its owner's share there.
pub fn agent_may_ask(reserved: Reserved, sharer: Sharer) -> Result<(), ShareRefusal> {
    match reserved {
        Reserved::OwnerOnly => Err(ShareRefusal::OwnerOnly),
        Reserved::Sharing => {
            if !sharer.for_owner {
                return Err(ShareRefusal::NotForOwner);
            }
            match sharer.held {
                // a hold is at most `AGENT_ROLE_MAX`, below the owner's role
                // sharing needs: a held agent shares nothing above its hold
                Some(held) if held < Role::Owner => Err(ShareRefusal::Held),
                Some(_) | None => Ok(()),
            }
        }
    }
}

/// Whether an agent shares a fragment with its owner's authority (Paul,
/// 2026-10-04: "your agent can share on your behalf"), and the role it
/// shares with: `owner`, when it acts for its own owner, its owner holds it
/// at nothing below them, and its owner owns the fragment through a share
/// that lends it. The role rules still apply to what it does
/// (`refuse_set_role`, `refuse_remove`): it never makes anyone owner.
/// Anywhere else its owner's own role caps it, as `effective_role` has it.
pub fn agent_shares(sharer: Sharer, owner: OwnerShare) -> Result<Role, ShareRefusal> {
    agent_may_ask(Reserved::Sharing, sharer)?;
    if owner.role != Some(Role::Owner) {
        return Err(ShareRefusal::NotOwner);
    }
    // as in `cap_role`: a people-only share lends the agent nothing (an
    // owner's own share is never one, so this is a tripwire that refuses)
    if owner.people_only {
        return Err(ShareRefusal::PeopleOnly);
    }
    Ok(Role::Owner)
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
/// the owner manages members (their own agent sharing for them acts with
/// their role: `agent_shares`); the owner's own row is never changed here
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
        Standing { member, owns_member_agent: false, link, signed, cap: None, held: None }
    }

    fn owner_of_agent() -> Standing {
        Standing { member: None, owns_member_agent: true, link: false, signed: true, cap: None, held: None }
    }

    /// An agent acting for someone whose membership is `asker`, beside its
    /// own membership and its owner's.
    fn acting(asker: Option<Role>, agent: Option<Role>, owner: Option<Role>) -> Standing {
        Standing { member: asker, owns_member_agent: false, link: false, signed: true, cap: Some(Cap { agent, owner, people_only: false }), held: None }
    }

    /// Goal: decision 36's example and its two limits. Skyler's agent edits
    /// what Paul shared with Skyler as an editor, acting for Skyler; a
    /// people-only share lends it nothing; held at viewer it only reads
    /// with whatever it reaches for whomever it acts, and keeps its own
    /// memberships.
    #[test]
    fn delegation_and_its_limits() {
        let d = |s, needs| decide(V::Members, s, Purpose::Act, needs);
        let skylers_agent = acting(Some(Editor), None, Some(Editor));
        assert_eq!(d(skylers_agent, Editor), Decision::Allow(Editor));
        let people_only = Standing { cap: Some(Cap { agent: None, owner: Some(Editor), people_only: true }), ..skylers_agent };
        assert_eq!(d(people_only, Viewer), Decision::Forbidden);
        // its own membership still counts on a people-only share
        let member_itself = Standing { cap: Some(Cap { agent: Some(Editor), owner: Some(Editor), people_only: true }), ..skylers_agent };
        assert_eq!(d(member_itself, Editor), Decision::Allow(Editor));
        let held = Standing { held: Some(Viewer), ..skylers_agent };
        assert_eq!(d(held, Editor), Decision::Forbidden);
        assert_eq!(d(held, Viewer), Decision::Allow(Viewer));
        // a hold never limits what it is a member of itself (its chats)
        let as_itself = Standing { member: Some(Editor), held: Some(Viewer), ..st(None, false, true) };
        assert_eq!(d(as_itself, Editor), Decision::Allow(Editor));
        // a hold never raises anything
        let raised = Standing { held: Some(Editor), ..acting(Some(Viewer), None, Some(Editor)) };
        assert_eq!(d(raised, Editor), Decision::Forbidden);
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
        // never above editor: owning is never an agent's (sharing for its owner is `agent_shares`)
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
        let cap = |agent, owner| Cap { agent, owner, people_only: false };
        assert_eq!(listed_role(Some(Owner), cap(None, Some(Owner))), Some(Editor));
        assert_eq!(listed_role(Some(Viewer), cap(Some(Editor), None)), Some(Viewer));
        assert_eq!(listed_role(Some(Editor), cap(None, None)), None);
        assert_eq!(listed_role(None, cap(Some(Editor), Some(Owner))), None);
    }

    #[test]
    fn reserved_routes() {
        for (method, rest) in [("DELETE", &[][..]), ("DELETE", &[""][..]), ("PUT", &["cap"][..])] {
            assert_eq!(reserved(method, rest), Some(Reserved::OwnerOnly), "{method} {rest:?}");
        }
        for (method, rest) in [
            ("PUT", &["members", "id:00"][..]),
            ("DELETE", &["members", "npub1x"][..]),
            ("POST", &["invites"][..]),
            ("GET", &["invites"][..]),
            ("DELETE", &["invites", "ab12"][..]),
            ("PUT", &["visibility"][..]),
            ("POST", &["rotate"][..]),
        ] {
            assert_eq!(reserved(method, rest), Some(Reserved::Sharing), "{method} {rest:?}");
        }
        for (method, rest) in [
            ("DELETE", &["members", "me"][..]),
            ("GET", &["members"][..]),
            ("GET", &["status"][..]),
            ("POST", &["ops", "add"][..]),
            ("POST", &["files"][..]),
            ("POST", &["deploy"][..]),
            ("PUT", &["secrets", "K"][..]),
            ("GET", &["cap"][..]),
            ("GET", &["visibility"][..]),
        ] {
            assert_eq!(reserved(method, rest), None, "{method} {rest:?}");
        }
    }

    /// Goal: Paul's decision of 2026-10-04 ("your agent can share on your
    /// behalf") and its limits. Method: the rule's two halves over each
    /// case: the router's (`agent_may_ask`, from who the agent is) and the
    /// fragment's (`agent_shares`, from its owner's share there), then the
    /// role rules over what the lent authority does.
    #[test]
    fn an_agent_shares_for_its_owner() {
        let for_owner = Sharer { for_owner: true, held: None };
        let owns = OwnerShare { role: Some(Owner), people_only: false };
        // valid: the owner's own agent shares the owner's fragment, as its owner
        assert_eq!(agent_may_ask(Reserved::Sharing, for_owner), Ok(()));
        assert_eq!(agent_shares(for_owner, owns), Ok(Owner));
        // ...under the role rules: it adds, changes, and removes members
        assert_eq!(refuse_set_role(Some(Owner), None, Viewer), None);
        assert_eq!(refuse_set_role(Some(Owner), Some(Viewer), Editor), None);
        assert_eq!(refuse_remove(Some(Owner), false, Some(Editor)), None);
        // ...and never makes anyone owner, itself included, nor changes or removes the owner
        assert!(refuse_set_role(Some(Owner), Some(Editor), Owner).is_some());
        assert!(refuse_set_role(Some(Owner), None, Public).is_some());
        assert!(refuse_set_role(Some(Owner), Some(Owner), Viewer).is_some());
        assert!(refuse_remove(Some(Owner), false, Some(Owner)).is_some());

        // invalid: as itself, or for anyone but its owner (another
        // person's agent naming you, or your agent naming someone else)
        let not_for_owner = Sharer { for_owner: false, held: None };
        assert_eq!(agent_may_ask(Reserved::Sharing, not_for_owner), Err(ShareRefusal::NotForOwner));
        assert_eq!(agent_shares(not_for_owner, owns), Err(ShareRefusal::NotForOwner));
        // another person's agent for its own owner, on what that person only edits
        let editor = OwnerShare { role: Some(Editor), people_only: false };
        assert_eq!(agent_shares(for_owner, editor), Err(ShareRefusal::NotOwner));
        // a fragment its owner merely edits, views, or is not in
        for role in [Some(Editor), Some(Viewer), Some(Public), None] {
            assert_eq!(agent_shares(for_owner, OwnerShare { role, people_only: false }), Err(ShareRefusal::NotOwner), "{role:?}");
        }
        // held below its owner, at any hold: it shares nothing above its hold
        for held in [Viewer, Editor] {
            let held = Sharer { for_owner: true, held: Some(held) };
            assert_eq!(agent_may_ask(Reserved::Sharing, held), Err(ShareRefusal::Held));
            assert_eq!(agent_shares(held, owns), Err(ShareRefusal::Held));
        }
        // a people-only share lends it nothing, its owner's included
        assert_eq!(agent_shares(for_owner, OwnerShare { role: Some(Owner), people_only: true }), Err(ShareRefusal::PeopleOnly));
        // deleting and the cap are the owner's own, for whomever it acts
        for sharer in [for_owner, not_for_owner, Sharer { for_owner: true, held: Some(Viewer) }] {
            for (method, rest) in [("DELETE", &[][..]), ("PUT", &["cap"][..])] {
                let route = reserved(method, rest).expect("a reserved route");
                assert_eq!(agent_may_ask(route, sharer), Err(ShareRefusal::OwnerOnly), "{method} {rest:?}");
            }
        }
        // and the lent authority is no role: acting for its owner it is
        // still an editor, so what needs owner (deleting) is refused there too
        let acting_for_owner = acting(Some(Owner), None, Some(Owner));
        assert_eq!(decide(V::Members, acting_for_owner, Purpose::Act, Owner), Decision::Forbidden);

        // replay: the same member PUT twice decides the same, the second
        // finding the role the first granted
        assert_eq!(agent_shares(for_owner, owns), agent_shares(for_owner, owns));
        let first = refuse_set_role(Some(Owner), None, Viewer);
        let again = refuse_set_role(Some(Owner), Some(Viewer), Viewer);
        assert_eq!((first, again), (None, None));
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
