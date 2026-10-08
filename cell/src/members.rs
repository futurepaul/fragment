//! Membership is live cell state (docs/MODEL.md): members, invites,
//! visibility, and the share link's token live in the supervisor, change in
//! one transaction, and take effect on the next request. Only the owner
//! changes them, or their own agent sharing for them (Paul, 2026-10-04:
//! `access::agent_shares`, decided here from the owner's share whatever
//! the router let through); a member may leave. Every change names who
//! made it (`Actor`): an agent's names the agent and the owner it acted
//! for, in the events, and the agent in members' `added_by` and invites'
//! `created_by`, so its owner sees what it shared.
//!
//! Members are identities (npubs); a request may name one by a key, which
//! the registry resolves to the identity holding it. An agent member's
//! owner is recorded beside it: the owner reads what the agent reads
//! (fragment.rs, `standing`). Kinds and owners never change once
//! registered, so the copy here cannot go stale. A new agent member's
//! computer is told it joined, through an outbox of its own (runs_on.rs):
//! nothing that adds an agent need post `joined` itself.
//!
//! Each identity's list of fragments is an index in its `Principal` cell.
//! The fragment is the authority: a change is written here with an outbox
//! row in the same turn, then delivered (and retried from the alarm). The
//! owner's row also carries the fragment's sharing (`Sharing`: who may
//! open it, its members and guests), made when the row is sent: a change
//! to members or visibility sends it again (`sharing_changed`), so the
//! platform's page reads the owner's list alone. Every row carries the
//! fragment's face (its kind and title) and its agents, the first added
//! first: an install that changes the face, or an agent joining or
//! leaving, sends every row again (`reindex`). A person's search cursor
//! (search.rs) follows their row: made with a role, gone without one.
//!
//! An invite waits on an email no one signs in as yet (decision 48): the
//! registry keeps it against the email and mails them, and their first
//! sign-in as it makes them a member here (`claim_invite`). A forwarded
//! mail admits no one else: only a sign-in that verified the email meets
//! it. There are no bearer invites: an unguessable link is `link`
//! visibility.

use fragment_core::access;
use fragment_core::ended::Failure;
use fragment_core::npub;
use fragment_proto::{
    limits, CreateInvite, ErrorCode, FragmentKind, Identity, IdentityKind, Invite, InviteList, Member, MemberList, Role, Rotated, SetRole, SetVisibility,
    Shared, Sharing, Visibility,
};
use futures_util::future::join_all;
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell, MetaKey};
use crate::js;
use crate::registry::calls::{ClaimInvite, InviteEmail, Invited};

/// Index changes one flush sends at most, all at once (and a delete, of
/// its ended life's: ended.rs). Sent one at a time, a fragment of
/// `MEMBERS_MAX` members took 300 s to tell their lists it was deleted on
/// the e2e preview (2026-10-06): about 0.3 s a list, each a Principal made
/// on the spot.
pub(crate) const INDEX_FLUSH_MAX: i64 = 32;

/// Marks the registry's `invites/claim`, a sign-in meeting the invite
/// waiting on its email; the router never sets it.
pub(crate) const CLAIM_HEADER: &str = "x-fragment-invite-claim";

fn refusal(actor_is_owner: bool, why: &str) -> CellError {
    if actor_is_owner {
        CellError::invalid(why)
    } else {
        CellError::new(ErrorCode::Forbidden, why)
    }
}

const MEMBER_COLUMNS: &str = "principal, role, added_by, added_at, kind, owner, people_only";

fn member_json(r: &Value) -> CellResult<Member> {
    let s = |k: &str| r[k].as_str().map(str::to_string).ok_or_else(|| CellError::host(format!("members.{k}")));
    Ok(Member {
        principal: npub::display(&s("principal")?),
        role: Role::parse(&s("role")?).ok_or_else(|| CellError::host("members.role"))?,
        added_by: npub::display(&s("added_by")?),
        added_at: r["added_at"].as_i64().unwrap_or(0),
        kind: r["kind"].as_str().and_then(IdentityKind::parse),
        owner: r["owner"].as_str().map(str::to_string),
        people_only: r["people_only"].as_i64() == Some(1),
    })
}

fn opt(v: Option<&str>) -> SqlStorageValue {
    v.map_or(SqlStorageValue::Null, |s| s.into())
}

/// Who changes a fragment's sharing in one request, and with what role.
struct Actor {
    /// A person's own membership (anyone's own, leaving); an agent sharing
    /// for its owner, its owner's role, lent (`access::agent_shares`).
    role: Option<Role>,
    /// Who made the change: members' `added_by`, invites' `created_by`,
    /// the events' `by`.
    by: String,
    /// The owner an agent shared for: the events' `for`.
    for_owner: Option<String>,
}

impl Actor {
    /// The words a change's summary ends with: an agent's names it and the
    /// owner it acted for; a person's, nothing (the owner made it).
    fn said(&self) -> String {
        match &self.for_owner {
            Some(owner) => format!(", by {} (an agent, for {owner})", self.by),
            None => String::new(),
        }
    }

    /// A change's event data, naming who made it (`by`), and the owner an
    /// agent made it for (`for`).
    fn noted(&self, mut data: Value) -> Value {
        assert!(data.is_object(), "an event's data is an object");
        data["by"] = json!(self.by);
        if let Some(owner) = &self.for_owner {
            data["for"] = json!(owner);
        }
        data
    }
}

fn invite_json(r: &Value) -> Invite {
    Invite {
        email: r["email"].as_str().unwrap_or("").to_string(),
        role: r["role"].as_str().and_then(Role::parse).unwrap_or(Role::Viewer),
        expires_at: r["expires_at"].as_i64().unwrap_or(0),
        created_by: npub::display(r["created_by"].as_str().unwrap_or("")),
    }
}

impl FragmentCell {
    /// Records an index change for `principal` (`None` removes them). Runs
    /// in the caller's turn, beside the membership write it mirrors. Their
    /// search cursor follows it (search.rs).
    pub(crate) fn index_change(&self, principal: &str, role: Option<Role>) -> CellResult<()> {
        let version: i64 = self.meta(MetaKey::IndexVersion)?.and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
        self.set_meta(MetaKey::IndexVersion, &version.to_string())?;
        let stored = role.map_or(SqlStorageValue::Null, |r| r.as_str().into());
        self.exec(
            "INSERT INTO index_outbox (principal, role, version, attempts, next_at) VALUES (?, ?, ?, 0, ?)
             ON CONFLICT (principal) DO UPDATE SET role = excluded.role, version = excluded.version, attempts = 0, next_at = excluded.next_at",
            vec![principal.into(), stored, SqlStorageValue::Integer(version), SqlStorageValue::Integer(js::now_ms())],
        )?;
        self.search_follows(principal, role)
    }

    /// What its members' lists show of it, at each install of live: its
    /// kind and title (and its agents, read as each row is sent). A change
    /// is sent to every member's list (at most `MEMBERS_MAX`) and its
    /// owner's, as an agent's joining or leaving is.
    pub(crate) fn face_is(&self, kind: FragmentKind, title: Option<&str>) -> CellResult<()> {
        let face = face(kind, title);
        if self.meta(MetaKey::Face)?.as_deref() == Some(face.as_str()) {
            return Ok(());
        }
        self.set_meta(MetaKey::Face, &face)?;
        self.reindex()
    }

    /// Every list's row is sent again: its owner's and each member's.
    pub(crate) fn reindex(&self) -> CellResult<()> {
        let owner = self.must(MetaKey::Owner)?;
        self.index_change(&owner, Some(Role::Owner))?;
        for r in self.rows("SELECT principal, role FROM members", vec![])? {
            let (Some(p), Some(role)) = (r["principal"].as_str(), r["role"].as_str().and_then(Role::parse)) else { continue };
            if p != owner {
                self.index_change(p, Some(role))?;
            }
        }
        Ok(())
    }

    /// Who is in, or who may open it, changed: the owner's row in their
    /// list is sent again, with the sharing as it is when it goes.
    pub(crate) fn sharing_changed(&self) -> CellResult<()> {
        let owner = self.must(MetaKey::Owner)?;
        self.index_change(&owner, Some(Role::Owner))
    }

    /// Sends one index change to `principal`'s list: whether it took it.
    pub(crate) async fn send_index(&self, principal: &str, body: &Value) -> bool {
        self.tell_index(principal, body).await.is_ok()
    }

    /// Sends one index change to `principal`'s list, and tells apart why
    /// it was not taken (`fragment_core::ended::list_told`): the list's
    /// refusal, or a failure a later try may pass.
    pub(crate) async fn tell_index(&self, principal: &str, body: &Value) -> Result<(), Failure> {
        let sent = async {
            let headers = Headers::new();
            headers.set("content-type", "application/json")?;
            let mut init = RequestInit::new();
            init.with_method(Method::Post).with_headers(headers).with_body(Some(body.to_string().into()));
            let req = Request::new_with_init("https://principal.internal/index", &init)?;
            let mut resp = self.env.durable_object("PRINCIPAL")?.get_by_name(principal)?.fetch_with_request(req).await?;
            let status = resp.status_code();
            // what a list says of a change it did not take (its error's message)
            let detail = if status == 200 { String::new() } else { resp.text().await.unwrap_or_default() };
            Ok::<(u16, String), worker::Error>((status, detail))
        }
        .await;
        match sent {
            Ok((status, detail)) => fragment_core::ended::list_told(Some(status), &detail),
            Err(e) => fragment_core::ended::list_told(None, &e.to_string()),
        }
    }

    /// Delivers due index changes to the people's `Principal` cells, at
    /// most `INDEX_FLUSH_MAX` of them, all at once, the newest first: a
    /// request that flushes sends its own change and waits one round,
    /// whatever its members; the alarm sends the rest, a batch a pass. A
    /// failure stays in the outbox with a backoff; the alarm retries it.
    pub(crate) async fn flush_index(&self) {
        let (Ok(name), Ok(Some(incarnation)), Ok(owner)) = (self.must(MetaKey::Name), self.meta(MetaKey::CreatedAt), self.must(MetaKey::Owner)) else { return };
        if let Err(e) = self.search_fence() {
            console_error!("{name}: its search was not fenced to the channels searched now ({:?}): {}", e.code, e.message);
        }
        let searched = match self.searched_channels() {
            Ok(searched) => searched,
            Err(e) => return console_error!("{name}: its searched channels did not read ({:?}): {}", e.code, e.message),
        };
        let agents = match self.listed_agents() {
            Ok(agents) => agents,
            Err(e) => return console_error!("{name}: its agents did not read ({:?}): {}", e.code, e.message),
        };
        let due = self
            .rows(
                "SELECT principal, role, version, attempts FROM index_outbox WHERE next_at <= ? ORDER BY version DESC LIMIT ?",
                vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(INDEX_FLUSH_MAX)],
            )
            .unwrap_or_default();
        assert!(due.len() as i64 <= INDEX_FLUSH_MAX, "a flush sends a bounded batch");
        let mut batch = Vec::with_capacity(due.len());
        for row in &due {
            let principal = row["principal"].as_str().unwrap_or("").to_string();
            let version = row["version"].as_i64().unwrap_or(0);
            let mut body = json!({
                "fragment": name,
                "role": row["role"],
                "incarnation": incarnation.parse::<i64>().unwrap_or(0),
                "version": version,
            });
            // every row a role names: the channels searched, and the
            // fragment's face and its agents, as they are now
            if row["role"].is_string() {
                body["searched"] = json!(searched);
                if let Ok(Some(face)) = self.meta(MetaKey::Face) {
                    body["face"] = serde_json::from_str(&face).unwrap_or(Value::Null);
                    body["face"]["agents"] = json!(agents);
                }
            }
            // the owner's row: the sharing now, which no later change undoes
            // (a later one sends a newer version, made after it)
            if principal == owner && row["role"].is_string() {
                match self.sharing_counts() {
                    Ok(sharing) => body["sharing"] = json!(sharing),
                    Err(e) => console_error!("{name}: its sharing did not read ({:?}): {}", e.code, e.message),
                }
            }
            batch.push((principal, version, row["attempts"].as_i64().unwrap_or(0), body));
        }
        let delivered = join_all(batch.iter().map(|(principal, _, _, body)| self.send_index(principal, body))).await;
        for ((principal, version, attempts, _), delivered) in batch.into_iter().zip(delivered) {
            if delivered {
                let _ = self.exec(
                    "DELETE FROM index_outbox WHERE principal = ? AND version = ?",
                    vec![principal.as_str().into(), SqlStorageValue::Integer(version)],
                );
            } else {
                let attempts = attempts + 1;
                let _ = self.exec(
                    "UPDATE index_outbox SET attempts = ?, next_at = ? WHERE principal = ? AND version = ?",
                    vec![
                        SqlStorageValue::Integer(attempts),
                        SqlStorageValue::Integer(js::now_ms() + fragment_core::backoff::outbox_retry_ms(attempts)),
                        principal.as_str().into(),
                        SqlStorageValue::Integer(version),
                    ],
                );
            }
        }
        // what the batch left, or did not deliver, is the alarm's; and a
        // cursor made with a role is due at once: the alarm sends it
        // (search.rs), after the row it follows is delivered here
        let left = self.rows("SELECT 1 FROM index_outbox LIMIT 1", vec![]).is_ok_and(|r| !r.is_empty());
        if left || self.search_due_at().ok().flatten().is_some_and(|at| at <= js::now_ms()) {
            if let Err(e) = self.schedule().await {
                console_error!("{name}: the alarm was not armed for its index or search outbox ({:?}): {}", e.code, e.message);
            }
        }
    }

    /// Who shares in this request (Paul, 2026-10-04): a person, with their
    /// own membership; an agent, with its owner's role when
    /// `access::agent_shares` lends it, and refused with why (403) when it
    /// does not. An agent's own membership never shares (it is at most
    /// `AGENT_ROLE_MAX`). The router refused what it could tell from who
    /// the agent is; this decides again, so a path it read otherwise
    /// gains nothing.
    fn sharer(&self, caller: &Caller) -> CellResult<Actor> {
        self.name()?;
        let Some(signed) = caller.signed.as_ref() else {
            return Err(CellError::new(ErrorCode::Unauthenticated, "sign the request"));
        };
        match signed.kind {
            IdentityKind::Person => {
                assert!(signed.acting_for.is_none(), "a person acts as themselves (the router refuses `for` from one)");
                Ok(Actor { role: self.member_role(&signed.id)?, by: signed.id.clone(), for_owner: None })
            }
            IdentityKind::Agent => {
                let owner = signed.owner.as_deref();
                let for_owner = owner.is_some() && signed.acting_for.as_deref() == owner;
                let sharer = access::Sharer { for_owner, held: signed.held };
                let share = match owner {
                    Some(owner) => self.owner_share(owner)?,
                    None => access::OwnerShare { role: None, people_only: false },
                };
                match access::agent_shares(sharer, share) {
                    Ok(role) => {
                        assert_eq!(role, Role::Owner, "an agent shares with its owner's ownership");
                        let owner = owner.expect("an agent that shares acts for its owner").to_string();
                        Ok(Actor { role: Some(role), by: signed.id.clone(), for_owner: Some(owner) })
                    }
                    Err(refusal) => Err(CellError::new(ErrorCode::Forbidden, refusal.message())),
                }
            }
        }
    }

    /// `owner`'s share of this fragment: their membership, and whether it
    /// is people only. One statement.
    fn owner_share(&self, owner: &str) -> CellResult<access::OwnerShare> {
        #[derive(serde::Deserialize)]
        struct Row {
            role: String,
            people_only: i64,
        }
        let rows: Vec<Row> = self.typed("SELECT role, people_only FROM members WHERE principal = ?", vec![owner.into()])?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(access::OwnerShare { role: None, people_only: false });
        };
        let role = Role::parse(&row.role).ok_or_else(|| CellError::host(format!("members.role {:?}", row.role)))?;
        Ok(access::OwnerShare { role: Some(role), people_only: row.people_only == 1 })
    }

    /// The owner, or their agent sharing for them: who they are, or 403.
    fn require_owner(&self, caller: &Caller) -> CellResult<Actor> {
        let actor = self.sharer(caller)?;
        if actor.role != Some(Role::Owner) {
            return Err(CellError::new(ErrorCode::Forbidden, "only the owner does this"));
        }
        Ok(actor)
    }

    /// The identity `who` (an npub or 64 hex: an identity, or a key of
    /// one; or an email: the person who signs in as it) names.
    async fn named(&self, who: &str) -> CellResult<Identity> {
        if npub::parse(who).is_none() && !fragment_core::mail::valid_address(who) {
            return Err(CellError::invalid(format!("{who:?} is not an email, an npub, or a 64-hex key")));
        }
        crate::ask_registry(&self.env, &crate::registry::calls::Lookup { who: who.to_string() }).await
    }

    /// Which of `ids` are members here: those whose emails a member may
    /// see (docs/cloudflare-v1.md, decision 48). Bounded by the caller's
    /// list (a profile ask is at most `PROFILES_MAX`).
    pub(crate) fn members_among(&self, ids: &[String]) -> CellResult<Vec<String>> {
        assert!(ids.len() <= crate::registry::PROFILES_MAX * 2, "a profile ask is bounded before it reaches here");
        let mut out = vec![];
        for id in ids {
            if self.member_role(id)?.is_some() {
                out.push(id.clone());
            }
        }
        Ok(out)
    }

    pub(crate) fn members(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        json_response(&self.member_list()?)
    }

    /// Every member, the first added first (a chat's lead is its first
    /// agent): the API's list, and a page's (`__members`, serve.rs).
    pub(crate) fn member_list(&self) -> CellResult<MemberList> {
        let rows = self.rows(&format!("SELECT {MEMBER_COLUMNS} FROM members ORDER BY added_at, principal"), vec![])?;
        Ok(MemberList { members: rows.iter().map(member_json).collect::<CellResult<Vec<_>>>()? })
    }

    /// Its agent members as every list's row names them: the first added
    /// first, at most `LISTED_AGENTS_MAX`.
    fn listed_agents(&self) -> CellResult<Vec<String>> {
        let rows = self.rows(
            "SELECT principal FROM members WHERE kind = 'agent' ORDER BY added_at, principal LIMIT ?",
            vec![SqlStorageValue::Integer(limits::LISTED_AGENTS_MAX as i64)],
        )?;
        Ok(rows.iter().filter_map(|r| r["principal"].as_str().map(str::to_string)).collect())
    }

    /// A new member needs room under `MEMBERS_MAX`; a role change does not.
    fn check_room(&self, current: Option<Role>) -> CellResult<()> {
        if current.is_none() && self.count("SELECT COUNT(*) AS n FROM members")? >= limits::MEMBERS_MAX as u64 {
            return Err(CellError::invalid(format!("a fragment has at most {} members", limits::MEMBERS_MAX)));
        }
        Ok(())
    }

    /// A test hook (`ops::test_fragment`): placeholder members until there are
    /// `fill`, so the e2e reaches the member cap without a thousand sign-ins.
    pub(crate) fn fill_members(&self, fill: u64) -> CellResult<u64> {
        if fill > limits::MEMBERS_MAX as u64 {
            return Err(CellError::invalid(format!("fill is at most {}", limits::MEMBERS_MAX)));
        }
        let have = self.count("SELECT COUNT(*) AS n FROM members")?;
        // Bounded by MEMBERS_MAX, just checked.
        for _ in have..fill {
            self.exec(
                "INSERT INTO members (principal, role, added_by, added_at, kind) VALUES (?, 'viewer', 'test', ?, 'person')",
                vec![format!("id:e2e-filler-{}", js::random_hex::<8>()).into(), SqlStorageValue::Integer(js::now_ms())],
            )?;
        }
        let now = self.count("SELECT COUNT(*) AS n FROM members")?;
        assert!(now >= fill && now <= limits::MEMBERS_MAX as u64, "filled to the count asked, within the cap");
        Ok(now)
    }

    pub(crate) async fn set_member(&self, caller: &Caller, who: &str, body: SetRole) -> CellResult<Response> {
        let actor = self.sharer(caller)?;
        // only the owner (or their agent sharing for them) learns whom a key names
        if actor.role != Some(Role::Owner) {
            let why = access::refuse_set_role(actor.role, None, body.role).expect("only the owner manages members");
            return Err(refusal(false, why));
        }
        let target = self.named(who).await?;
        json_response(&self.put_member(&actor, &target, body).await?)
    }

    /// `target` made a member at `body`'s role, or their role changed, by
    /// `actor`, the owner or their agent sharing for them.
    async fn put_member(&self, actor: &Actor, target: &Identity, body: SetRole) -> CellResult<Member> {
        let current = self.member_role(&target.id)?;
        if let Some(why) = access::refuse_set_role(actor.role, current, body.role) {
            return Err(refusal(true, why));
        }
        self.check_room(current)?;
        let by = actor.by.as_str();
        let now = js::now_ms();
        self.exec(
            "INSERT INTO members (principal, role, added_by, added_at, kind, owner, people_only) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (principal) DO UPDATE SET role = excluded.role, people_only = excluded.people_only",
            vec![
                target.id.as_str().into(),
                body.role.as_str().into(),
                by.into(),
                SqlStorageValue::Integer(now),
                target.kind.as_str().into(),
                opt(target.owner.as_deref()),
                SqlStorageValue::Integer(i64::from(body.people_only)),
            ],
        )?;
        self.index_change(&target.id, Some(body.role))?;
        self.sharing_changed()?;
        // a new agent member's computer hears it joined (runs_on.rs), and
        // every list's row names it; a role change is no join
        if current.is_none() && target.kind == IdentityKind::Agent {
            self.agent_added(&target.id, target.owner.as_deref(), now)?;
            self.reindex()?;
        }
        // sharing with an agent says so: its owner reads what it reads (FIN-11)
        let summary = match &target.owner {
            Some(owner) => format!("{} (an agent) is now {}; its owner {owner} reads what it reads", target.id, body.role.as_str()),
            None => format!("{} is now {}", target.id, body.role.as_str()),
        };
        let data = json!({ "principal": target.id, "role": body.role, "kind": target.kind, "owner": target.owner, "peopleOnly": body.people_only });
        self.event("member.set", &format!("{summary}{}", actor.said()), actor.noted(data));
        // A socket's role is fixed when it opens, and it answers queries at
        // that role: a changed role reopens the member's sockets (and their
        // owner's, who reads through an agent), at the new one.
        if current.is_some_and(|was| was != body.role) {
            self.reopen_sockets(&format!("p:{}", target.id), "your role changed");
            if let Some(owner) = &target.owner {
                self.reopen_sockets(&format!("p:{owner}"), "your agent's role changed");
            }
        }
        // the agent's list holds this fragment before its computer is told
        self.flush_index().await;
        self.flush_joined().await;
        let row = self.rows(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE principal = ?"), vec![target.id.as_str().into()])?;
        member_json(&row[0])
    }

    pub(crate) async fn remove_member(&self, caller: &Caller, who: &str) -> CellResult<Response> {
        self.name()?;
        let me = self.caller_id(caller)?.to_string();
        // a member is removed as it is (`me` is the caller), even one the
        // registry no longer holds; a key names the identity holding it
        let target = match npub::parse(who) {
            _ if who == "me" => me.clone(),
            Some(key) if self.member_role(&npub::identity_of(&key))?.is_some() => npub::identity_of(&key),
            Some(_) => self.named(who).await?.id,
            None if fragment_core::mail::valid_address(who) => self.named(&who.to_ascii_lowercase()).await?.id,
            None => return Err(CellError::invalid(format!("{who:?} is not an email, an npub, or a 64-hex key"))),
        };
        let is_self = me == target;
        // leaving is any member's own; removing anyone else is sharing
        let actor = match is_self {
            true => Actor { role: self.member_role(&me)?, by: me.clone(), for_owner: None },
            false => self.sharer(caller)?,
        };
        let current = self.member_role(&target)?;
        if let Some(why) = access::refuse_remove(actor.role, is_self, current) {
            return Err(match current {
                None => CellError::new(ErrorCode::NotFound, why),
                Some(_) => refusal(actor.role == Some(Role::Owner), why),
            });
        }
        let removed = self.rows("SELECT owner, kind FROM members WHERE principal = ?", vec![target.as_str().into()])?;
        let owner = removed.first().and_then(|r| r["owner"].as_str()).map(str::to_string);
        self.exec("DELETE FROM members WHERE principal = ?", vec![target.as_str().into()])?;
        self.drop_subscriptions(&target)?;
        self.agent_removed(&target)?;
        self.index_change(&target, None)?;
        self.sharing_changed()?;
        // every list's row names the agents left
        if removed.first().is_some_and(|r| r["kind"] == "agent") {
            self.reindex()?;
        }
        self.close_sockets(&format!("p:{target}"), "membership revoked");
        // an agent's owner who read through it, and has no standing of their own now
        if let Some(owner) = owner {
            let still = self.member_role(&owner)?.is_some()
                || !self.rows("SELECT principal FROM members WHERE owner = ? LIMIT 1", vec![owner.as_str().into()])?.is_empty();
            if !still {
                self.close_sockets(&format!("p:{owner}"), "their agent's membership was revoked");
            }
        }
        let how = if is_self { "left" } else { "was removed" };
        self.event("member.removed", &format!("{} {how}{}", npub::display(&target), actor.said()), actor.noted(json!({ "principal": npub::display(&target) })));
        self.flush_index().await;
        json_response(&json!({ "ok": true, "removed": npub::display(&target) }))
    }

    fn drop_spent_invites(&self) -> CellResult<()> {
        self.exec("DELETE FROM invites WHERE expires_at <= ?", vec![SqlStorageValue::Integer(js::now_ms())])
    }

    /// Its title as its members' lists show it, if it has one.
    fn title(&self) -> CellResult<Option<String>> {
        let face: Option<Value> = self.meta(MetaKey::Face)?.and_then(|f| serde_json::from_str(&f).ok());
        Ok(face.and_then(|f| f["title"].as_str().map(str::to_string)).filter(|t| !t.is_empty()))
    }

    /// Shares it with whoever signs in as `body.email` (decision 48). The
    /// person who holds that email is a member at once. When no one does
    /// yet, the invite waits on the email here, and the registry keeps it
    /// against the email, mails them, and makes them a member at their
    /// first sign-in as it (`claim_invite`). It is written before the
    /// registry is asked, so a sign-in meanwhile meets it, and taken back
    /// when the registry refuses (a day's mails spent, a mail not sent).
    /// No event names the email: a link's viewers read the events.
    pub(crate) async fn create_invite(&self, caller: &Caller, body: CreateInvite) -> CellResult<Response> {
        let actor = self.require_owner(caller)?;
        if !matches!(body.role, Role::Viewer | Role::Editor) {
            return Err(CellError::invalid("an invite grants viewer or editor"));
        }
        let email = body.email.trim().to_ascii_lowercase();
        if !fragment_core::mail::valid_address(&email) {
            return Err(CellError::invalid(format!("{email:?} is not an email")));
        }
        let name = self.name()?;
        self.drop_spent_invites()?;
        let before = self.rows("SELECT email, role, expires_at, created_by, created_at FROM invites WHERE email = ?", vec![email.as_str().into()])?;
        if before.is_empty() && self.count("SELECT COUNT(*) AS n FROM invites")? >= limits::INVITES_MAX as u64 {
            return Err(CellError::invalid(format!("a fragment has at most {} invites waiting", limits::INVITES_MAX)));
        }
        let now = js::now_ms();
        let expires_at = now + limits::INVITE_TTL_S * 1000;
        let write = |role: &str, expires_at: i64, by: &str, at: i64| {
            self.exec(
                "INSERT INTO invites (email, role, expires_at, created_by, created_at) VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT (email) DO UPDATE SET role = excluded.role, expires_at = excluded.expires_at,
                   created_by = excluded.created_by, created_at = excluded.created_at",
                vec![email.as_str().into(), role.into(), SqlStorageValue::Integer(expires_at), by.into(), SqlStorageValue::Integer(at)],
            )
        };
        write(body.role.as_str(), expires_at, &actor.by, now)?;
        let asked = InviteEmail {
            email: email.clone(),
            fragment: name.clone(),
            // an agent's mail counts in its owner's day
            sharer: actor.for_owner.clone().unwrap_or_else(|| actor.by.clone()),
            role: body.role,
            title: self.title()?.unwrap_or_else(|| crate::share::label(&name).to_string()),
            expires_at,
        };
        let answer = crate::ask_registry(&self.env, &asked).await;
        // what this call wrote, unless a sign-in met it meanwhile
        let mine = || self.exec("DELETE FROM invites WHERE email = ? AND created_at = ?", vec![email.as_str().into(), SqlStorageValue::Integer(now)]);
        match answer {
            Ok(Invited::Mailed) => {
                self.event(
                    "invite.created",
                    &format!("an invite waits on an email, as {}{}", body.role.as_str(), actor.said()),
                    actor.noted(json!({ "role": body.role, "expiresAt": expires_at })),
                );
                let invite = Invite { email, role: body.role, expires_at, created_by: actor.by.clone() };
                json_response(&Shared::Invited(invite))
            }
            Ok(Invited::Holder(person)) => {
                mine()?;
                let member = self.put_member(&actor, &person, SetRole { role: body.role, people_only: false }).await?;
                json_response(&Shared::Member(member))
            }
            Err(e) => {
                mine()?;
                if let Some(was) = before.first() {
                    let field = |k: &str| was[k].as_str().unwrap_or_default().to_string();
                    let int = |k: &str| was[k].as_i64().unwrap_or_default();
                    write(&field("role"), int("expires_at"), &field("created_by"), int("created_at"))?;
                }
                Err(e)
            }
        }
    }

    pub(crate) fn invites(&self, caller: &Caller) -> CellResult<Response> {
        self.require_owner(caller)?;
        self.drop_spent_invites()?;
        let rows = self.rows("SELECT email, role, expires_at, created_by FROM invites ORDER BY created_at, email", vec![])?;
        json_response(&InviteList { invites: rows.iter().map(invite_json).collect() })
    }

    pub(crate) fn revoke_invite(&self, caller: &Caller, email: &str) -> CellResult<Response> {
        let actor = self.require_owner(caller)?;
        let email = email.trim().to_ascii_lowercase();
        self.drop_spent_invites()?;
        if self.rows("SELECT email FROM invites WHERE email = ?", vec![email.as_str().into()])?.is_empty() {
            return Err(CellError::new(ErrorCode::NotFound, format!("no invite waits on {email}")));
        }
        self.exec("DELETE FROM invites WHERE email = ?", vec![email.as_str().into()])?;
        self.event("invite.revoked", &format!("an invite was revoked{}", actor.said()), actor.noted(json!({})));
        json_response(&json!({ "ok": true, "revoked": email }))
    }

    /// The invite waiting on `email`, met by `identity`, a person who just
    /// signed in as it (the registry's `ClaimInvite`): a member at the role
    /// it said, added by its maker, and a stronger role they hold kept.
    /// 404 when none waits (revoked, expired, or met already), and the
    /// registry forgets it. A full fragment keeps it for their next sign-in.
    pub(crate) async fn claim_invite(&self, claim: &ClaimInvite) -> CellResult<Value> {
        self.name()?;
        assert!(npub::is_identity(&claim.identity), "the registry names a person by their npub");
        self.drop_spent_invites()?;
        let rows = self.rows("SELECT role, created_by FROM invites WHERE email = ?", vec![claim.email.as_str().into()])?;
        let Some(row) = rows.first() else {
            return Err(CellError::new(ErrorCode::NotFound, "no invite waits on that email"));
        };
        let role = row["role"].as_str().and_then(Role::parse).ok_or_else(|| CellError::host("invites.role"))?;
        let by = row["created_by"].as_str().unwrap_or_default().to_string();
        let who = claim.identity.as_str();
        let current = self.member_role(who)?;
        let kept = current.is_some_and(|c| c >= role);
        if !kept {
            self.check_room(current)?;
        }
        self.exec("DELETE FROM invites WHERE email = ?", vec![claim.email.as_str().into()])?;
        if kept {
            return Ok(json!({ "role": current }));
        }
        self.exec(
            "INSERT INTO members (principal, role, added_by, added_at, kind) VALUES (?, ?, ?, ?, 'person')
             ON CONFLICT (principal) DO UPDATE SET role = excluded.role",
            vec![who.into(), role.as_str().into(), by.as_str().into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        self.index_change(who, Some(role))?;
        self.sharing_changed()?;
        self.event("member.joined", &format!("{who} joined as {}, invited by email", role.as_str()), json!({ "principal": who, "role": role, "by": by }));
        if current.is_some() {
            self.reopen_sockets(&format!("p:{who}"), "your role changed");
        }
        self.flush_index().await;
        Ok(json!({ "role": role }))
    }

    /// The fragment's sharing, as its owner's list carries it: `guests`
    /// counts the members who are neither the owner nor an agent of theirs.
    pub(crate) fn sharing_counts(&self) -> CellResult<Sharing> {
        let visibility = self.visibility()?;
        let owner = self.must(MetaKey::Owner)?;
        #[derive(serde::Deserialize)]
        struct Counts {
            members: u64,
            guests: u64,
        }
        let counts: Vec<Counts> = self.typed(
            "SELECT (SELECT COUNT(*) FROM members) AS members,
                    (SELECT COUNT(*) FROM members WHERE principal != ? AND (owner IS NULL OR owner != ?)) AS guests",
            vec![owner.as_str().into(), owner.as_str().into()],
        )?;
        let counts = counts.into_iter().next().expect("a SELECT without FROM answers one row");
        assert!(counts.guests <= counts.members, "guests are members");
        Ok(Sharing { visibility, members: counts.members, guests: counts.guests })
    }

    pub(crate) async fn set_visibility(&self, caller: &Caller, body: SetVisibility) -> CellResult<Response> {
        let actor = self.require_owner(caller)?;
        let before = self.visibility()?;
        self.set_meta(MetaKey::Visibility, body.visibility.as_str())?;
        self.sharing_changed()?;
        if body.visibility != Visibility::Public {
            self.close_sockets("anon", "the fragment is no longer public");
        }
        if body.visibility == Visibility::Members {
            self.close_sockets("view", "the fragment is now members only");
        }
        self.event(
            "visibility",
            &format!("visibility {} → {}{}", before.as_str(), body.visibility.as_str(), actor.said()),
            actor.noted(json!({ "from": before, "to": body.visibility })),
        );
        self.flush_index().await;
        json_response(&json!({ "ok": true, "visibility": body.visibility }))
    }

    /// New share-link token or inbox token (default: both). An agent
    /// shares for its owner: it renews them as its owner does.
    pub(crate) fn rotate(&self, caller: &Caller, body: Value) -> CellResult<Response> {
        let actor = self.require_owner(caller)?;
        let all = ["inbox", "view"];
        let want: Vec<String> = match &body["scopes"] {
            Value::Array(a) if !a.is_empty() => a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect(),
            Value::Null | Value::Array(_) => all.iter().map(|s| s.to_string()).collect(),
            _ => return Err(CellError::invalid("scopes must be an array of inbox, view")),
        };
        if let Some(bad) = want.iter().find(|s| !all.contains(&s.as_str())) {
            return Err(CellError::invalid(format!("unknown scope {bad:?} (inbox, view)")));
        }
        for (scope, key, fresh) in [("inbox", MetaKey::InboxToken, js::random_hex::<16>()), ("view", MetaKey::ViewToken, js::random_hex::<12>())] {
            if want.iter().any(|w| w == scope) {
                self.set_meta(key, &fresh)?;
            }
        }
        if want.iter().any(|w| w == "view") {
            self.close_sockets("view", "the share link changed");
        }
        let rotated: Vec<&str> = all.into_iter().filter(|s| want.iter().any(|w| w == s)).collect();
        self.event("tokens.rotated", &format!("{}{}", rotated.join("+"), actor.said()), actor.noted(json!({ "scopes": rotated })));
        json_response(&Rotated {
            inbox_token: self.must(MetaKey::InboxToken)?,
            view_token: self.must(MetaKey::ViewToken)?,
            rotated: rotated.iter().map(|s| s.to_string()).collect(),
        })
    }

    pub(crate) async fn put_secret(&self, caller: &Caller, key: &str, value: Vec<u8>) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        if !fragment_proto::valid_secret_name(key) {
            return Err(CellError::invalid("a secret's name must match ^[A-Z][A-Z0-9_]{0,63}$"));
        }
        if value.is_empty() {
            return Err(CellError::invalid("a secret's value is the request body; it is empty"));
        }
        if value.len() > limits::SECRET_MAX_BYTES {
            return Err(CellError::too_large("a secret", value.len(), limits::SECRET_MAX_BYTES));
        }
        // sealed first (reading the host secret may yield): the checks and
        // the write below share one turn
        let sealed = crate::keys::seal(&self.env, &self.scope(), &value).await?;
        self.require(caller, false, Role::Editor)?;
        let exists = !self.rows("SELECT name FROM secrets WHERE name = ?", vec![key.into()])?.is_empty();
        if !exists && self.count("SELECT COUNT(*) AS n FROM secrets")? >= limits::SECRETS_MAX as u64 {
            return Err(CellError::invalid(format!("a fragment has at most {} secrets", limits::SECRETS_MAX)));
        }
        let by = self.caller_id(caller)?;
        self.exec(
            "INSERT INTO secrets (name, sealed, set_by, set_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (name) DO UPDATE SET sealed = excluded.sealed, set_by = excluded.set_by, set_at = excluded.set_at",
            vec![key.into(), sealed.into(), by.into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        self.event("secret.set", &format!("secret {key} set by {}", npub::display(by)), json!({ "name": key }));
        json_response(&json!({ "ok": true, "name": key }))
    }

    pub(crate) fn list_secrets(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let names: Vec<Value> = self.rows("SELECT name FROM secrets ORDER BY name", vec![])?.into_iter().map(|r| r["name"].clone()).collect();
        json_response(&json!({ "names": names }))
    }

    pub(crate) fn delete_secret(&self, caller: &Caller, key: &str) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let removed = !self.rows("SELECT name FROM secrets WHERE name = ?", vec![key.into()])?.is_empty();
        self.exec("DELETE FROM secrets WHERE name = ?", vec![key.into()])?;
        if removed {
            self.event("secret.removed", &format!("secret {key} removed"), json!({ "name": key }));
        }
        json_response(&json!({ "ok": true, "removed": removed }))
    }
}

/// A fragment's face as its members' lists keep it (`MetaKey::Face`).
pub(crate) fn face(kind: FragmentKind, title: Option<&str>) -> String {
    json!({ "kind": kind, "title": title }).to_string()
}
