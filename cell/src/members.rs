//! Membership is live cell state (docs/MODEL.md): members, invites,
//! visibility, and the share link's token live in the supervisor, change in
//! one transaction, and take effect on the next request. Only the owner
//! changes them; a member may leave.
//!
//! Members are identities (`id:…`); a request may name one by a key, which
//! the registry resolves to the identity holding it. An agent member's
//! owner is recorded beside it: the owner reads what the agent reads
//! (fragment.rs, `standing`). Kinds and owners never change once
//! registered, so the copy here cannot go stale.
//!
//! Each identity's list of fragments is an index in its `Principal` cell.
//! The fragment is the authority: a change is written here with an outbox
//! row in the same turn, then delivered (and retried from the alarm).

use fragment_core::access;
use fragment_core::npub;
use fragment_proto::{limits, CreateInvite, ErrorCode, IdentityKind, Invite, Join, Member, Role, SetRole, SetVisibility, Visibility};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::js;

/// Retry backoff for index deliveries: 2^attempts seconds, at most ten minutes.
fn backoff_ms(attempts: i64) -> i64 {
    1000 * (1i64 << attempts.clamp(0, 10)).min(600)
}

fn refusal(actor_is_owner: bool, why: &str) -> CellError {
    if actor_is_owner {
        CellError::invalid(why)
    } else {
        CellError::new(ErrorCode::Forbidden, why)
    }
}

const MEMBER_COLUMNS: &str = "principal, role, added_by, added_at, kind, owner";

fn member_json(r: &Value) -> CellResult<Member> {
    let s = |k: &str| r[k].as_str().map(str::to_string).ok_or_else(|| CellError::host(format!("members.{k}")));
    Ok(Member {
        principal: npub::display(&s("principal")?),
        role: Role::parse(&s("role")?).ok_or_else(|| CellError::host("members.role"))?,
        added_by: npub::display(&s("added_by")?),
        added_at: r["added_at"].as_i64().unwrap_or(0),
        kind: r["kind"].as_str().and_then(IdentityKind::parse),
        owner: r["owner"].as_str().map(str::to_string),
    })
}

/// Who a request names, as the registry knows them.
struct Named {
    id: String,
    kind: IdentityKind,
    owner: Option<String>,
}

fn opt(v: Option<&str>) -> SqlStorageValue {
    v.map_or(SqlStorageValue::Null, |s| s.into())
}

fn invite_json(r: &Value) -> Invite {
    Invite {
        id: r["id"].as_str().unwrap_or("").to_string(),
        role: r["role"].as_str().and_then(Role::parse).unwrap_or(Role::Viewer),
        uses_left: r["uses_left"].as_u64().unwrap_or(0) as u32,
        expires_at: r["expires_at"].as_i64().unwrap_or(0),
        created_by: npub::display(r["created_by"].as_str().unwrap_or("")),
        token: None,
    }
}

impl FragmentCell {
    /// Records an index change for `principal` (`None` removes them). Runs
    /// in the caller's turn, beside the membership write it mirrors.
    pub(crate) fn index_change(&self, principal: &str, role: Option<Role>) -> CellResult<()> {
        let version: i64 = self.meta("index_version")?.and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
        self.set_meta("index_version", &version.to_string())?;
        let role = role.map_or(SqlStorageValue::Null, |r| r.as_str().into());
        self.exec(
            "INSERT INTO index_outbox (principal, role, version, attempts, next_at) VALUES (?, ?, ?, 0, ?)
             ON CONFLICT (principal) DO UPDATE SET role = excluded.role, version = excluded.version, attempts = 0, next_at = excluded.next_at",
            vec![principal.into(), role, SqlStorageValue::Integer(version), SqlStorageValue::Integer(js::now_ms())],
        )
    }

    /// Delivers due index changes to the people's `Principal` cells. A
    /// failure stays in the outbox with a backoff; the alarm retries it.
    pub(crate) async fn flush_index(&self) {
        let (Ok(name), Ok(Some(incarnation))) = (self.must("name"), self.meta("created_at")) else { return };
        let due = self
            .rows(
                "SELECT principal, role, version, attempts FROM index_outbox WHERE next_at <= ?",
                vec![SqlStorageValue::Integer(js::now_ms())],
            )
            .unwrap_or_default();
        for row in due {
            let principal = row["principal"].as_str().unwrap_or("").to_string();
            let version = row["version"].as_i64().unwrap_or(0);
            let body = json!({
                "fragment": name,
                "role": row["role"],
                "incarnation": incarnation.parse::<i64>().unwrap_or(0),
                "version": version,
            });
            let delivered = async {
                let headers = Headers::new();
                headers.set("content-type", "application/json")?;
                let mut init = RequestInit::new();
                init.with_method(Method::Post).with_headers(headers).with_body(Some(body.to_string().into()));
                let req = Request::new_with_init("https://principal.internal/index", &init)?;
                let resp = self.env.durable_object("PRINCIPAL")?.get_by_name(&principal)?.fetch_with_request(req).await?;
                Ok::<bool, worker::Error>(resp.status_code() == 200)
            }
            .await;
            if matches!(delivered, Ok(true)) {
                let _ = self.exec(
                    "DELETE FROM index_outbox WHERE principal = ? AND version = ?",
                    vec![principal.as_str().into(), SqlStorageValue::Integer(version)],
                );
            } else {
                let attempts = row["attempts"].as_i64().unwrap_or(0) + 1;
                let _ = self.exec(
                    "UPDATE index_outbox SET attempts = ?, next_at = ? WHERE principal = ? AND version = ?",
                    vec![
                        SqlStorageValue::Integer(attempts),
                        SqlStorageValue::Integer(js::now_ms() + backoff_ms(attempts)),
                        principal.as_str().into(),
                        SqlStorageValue::Integer(version),
                    ],
                );
            }
        }
    }

    fn actor_role(&self, caller: &Caller) -> CellResult<Option<Role>> {
        self.name()?;
        match &caller.principal {
            Some(p) => self.member_role(p),
            None => Err(CellError::new(ErrorCode::Unauthenticated, "sign the request")),
        }
    }

    /// The identity `who` (an `id:`, an npub, or 64 hex) names.
    async fn named(&self, who: &str) -> CellResult<Named> {
        if npub::parse_named(who).is_none() {
            return Err(CellError::invalid(format!("{who:?} is not an identity (id:…), an npub, or a 64-hex key")));
        }
        let v = crate::ask_registry(&self.env, "/lookup", &json!({ "who": who })).await?;
        let (id, kind, owner) = crate::facts_of(&v)?;
        Ok(Named { id, kind, owner })
    }

    pub(crate) fn members(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let rows = self.rows(&format!("SELECT {MEMBER_COLUMNS} FROM members ORDER BY added_at, principal"), vec![])?;
        let members = rows.iter().map(member_json).collect::<CellResult<Vec<_>>>()?;
        json_response(&json!({ "members": members }))
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
        let actor = self.actor_role(caller)?;
        // only the owner learns whom a key names
        if actor != Some(Role::Owner) {
            let why = access::refuse_set_role(actor, None, body.role).expect("only the owner manages members");
            return Err(refusal(false, why));
        }
        let target = self.named(who).await?;
        let current = self.member_role(&target.id)?;
        if let Some(why) = access::refuse_set_role(actor, current, body.role) {
            return Err(refusal(true, why));
        }
        self.check_room(current)?;
        let by = self.caller_id(caller)?;
        self.exec(
            "INSERT INTO members (principal, role, added_by, added_at, kind, owner) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (principal) DO UPDATE SET role = excluded.role",
            vec![
                target.id.as_str().into(),
                body.role.as_str().into(),
                by.into(),
                SqlStorageValue::Integer(js::now_ms()),
                target.kind.as_str().into(),
                opt(target.owner.as_deref()),
            ],
        )?;
        self.index_change(&target.id, Some(body.role))?;
        // sharing with an agent says so: its owner reads what it reads (FIN-11)
        let summary = match &target.owner {
            Some(owner) => format!("{} (an agent) is now {}; its owner {owner} reads what it reads", target.id, body.role.as_str()),
            None => format!("{} is now {}", target.id, body.role.as_str()),
        };
        self.event("member.set", &summary, json!({ "principal": target.id, "role": body.role, "kind": target.kind, "owner": target.owner }));
        self.flush_index().await;
        let row = self.rows(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE principal = ?"), vec![target.id.as_str().into()])?;
        json_response(&member_json(&row[0])?)
    }

    pub(crate) async fn remove_member(&self, caller: &Caller, who: &str) -> CellResult<Response> {
        let actor = self.actor_role(caller)?;
        // an identity is removed as it is (`me` is the caller); a key names
        // the identity holding it
        let target = match npub::parse_named(who) {
            _ if who == "me" => self.caller_id(caller)?.to_string(),
            Some(npub::Named::Identity(id)) => id,
            Some(npub::Named::Key(_)) => self.named(who).await?.id,
            None => return Err(CellError::invalid(format!("{who:?} is not an identity (id:…), an npub, or a 64-hex key"))),
        };
        let is_self = caller.principal.as_deref() == Some(target.as_str());
        let current = self.member_role(&target)?;
        if let Some(why) = access::refuse_remove(actor, is_self, current) {
            return Err(match current {
                None => CellError::new(ErrorCode::NotFound, why),
                Some(_) => refusal(actor == Some(Role::Owner), why),
            });
        }
        let owner = self.rows("SELECT owner FROM members WHERE principal = ?", vec![target.as_str().into()])?;
        let owner = owner.first().and_then(|r| r["owner"].as_str()).map(str::to_string);
        self.exec("DELETE FROM members WHERE principal = ?", vec![target.as_str().into()])?;
        self.drop_subscriptions(&target)?;
        self.index_change(&target, None)?;
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
        self.event("member.removed", &format!("{} {how}", npub::display(&target)), json!({ "principal": npub::display(&target) }));
        self.flush_index().await;
        json_response(&json!({ "ok": true, "removed": npub::display(&target) }))
    }

    fn require_owner(&self, caller: &Caller) -> CellResult<()> {
        if self.actor_role(caller)? != Some(Role::Owner) {
            return Err(CellError::new(ErrorCode::Forbidden, "only the owner does this"));
        }
        Ok(())
    }

    fn drop_spent_invites(&self) -> CellResult<()> {
        self.exec("DELETE FROM invites WHERE expires_at <= ? OR uses_left <= 0", vec![SqlStorageValue::Integer(js::now_ms())])
    }

    pub(crate) fn create_invite(&self, caller: &Caller, body: CreateInvite) -> CellResult<Response> {
        self.require_owner(caller)?;
        if !matches!(body.role, Role::Viewer | Role::Editor) {
            return Err(CellError::invalid("an invite grants viewer or editor"));
        }
        let uses = body.uses.unwrap_or(1);
        if uses == 0 || uses > limits::INVITE_USES_MAX {
            return Err(CellError::invalid(format!("uses must be 1..={}", limits::INVITE_USES_MAX)));
        }
        let ttl_s = body.ttl_s.unwrap_or(limits::INVITE_TTL_DEFAULT_S);
        if !(60..=limits::INVITE_TTL_MAX_S).contains(&ttl_s) {
            return Err(CellError::invalid(format!("ttlS must be 60..={}", limits::INVITE_TTL_MAX_S)));
        }
        self.drop_spent_invites()?;
        if self.count("SELECT COUNT(*) AS n FROM invites")? >= limits::INVITES_MAX as u64 {
            return Err(CellError::invalid(format!("a fragment has at most {} open invites", limits::INVITES_MAX)));
        }
        let token = js::random_hex::<24>();
        let id = js::random_hex::<8>();
        let now = js::now_ms();
        let expires_at = now + ttl_s * 1000;
        let by = self.caller_id(caller)?;
        self.exec(
            "INSERT INTO invites (id, token_sha, role, uses_left, expires_at, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            vec![
                id.as_str().into(),
                hex::encode(Sha256::digest(token.as_bytes())).into(),
                body.role.as_str().into(),
                SqlStorageValue::Integer(uses.into()),
                SqlStorageValue::Integer(expires_at),
                by.into(),
                SqlStorageValue::Integer(now),
            ],
        )?;
        self.event("invite.created", &format!("invite {id} for {} ({uses} uses)", body.role.as_str()), json!({ "id": id, "role": body.role }));
        json_response(&Invite { id, role: body.role, uses_left: uses, expires_at, created_by: npub::display(by), token: Some(token) })
    }

    pub(crate) fn invites(&self, caller: &Caller) -> CellResult<Response> {
        self.require_owner(caller)?;
        self.drop_spent_invites()?;
        let rows = self.rows("SELECT id, role, uses_left, expires_at, created_by FROM invites ORDER BY created_at", vec![])?;
        json_response(&json!({ "invites": rows.iter().map(invite_json).collect::<Vec<_>>() }))
    }

    pub(crate) fn revoke_invite(&self, caller: &Caller, id: &str) -> CellResult<Response> {
        self.require_owner(caller)?;
        if self.rows("SELECT id FROM invites WHERE id = ?", vec![id.into()])?.is_empty() {
            return Err(CellError::new(ErrorCode::NotFound, "no such invite"));
        }
        self.exec("DELETE FROM invites WHERE id = ?", vec![id.into()])?;
        self.event("invite.revoked", &format!("invite {id} revoked"), json!({ "id": id }));
        json_response(&json!({ "ok": true, "revoked": id }))
    }

    /// Redeems an invite. The token is the capability: no visibility check.
    pub(crate) async fn join(&self, caller: &Caller, body: Join) -> CellResult<Response> {
        let name = self.name()?;
        let who = self.caller_id(caller)?.to_string();
        let rows = self.rows(
            "SELECT id, role FROM invites WHERE token_sha = ? AND expires_at > ? AND uses_left > 0",
            vec![hex::encode(Sha256::digest(body.token.as_bytes())).into(), SqlStorageValue::Integer(js::now_ms())],
        )?;
        let row = rows.first().ok_or_else(|| CellError::new(ErrorCode::NotFound, "no such invite (it may have expired or been used)"))?;
        let id = row["id"].as_str().unwrap_or("").to_string();
        let role = row["role"].as_str().and_then(Role::parse).ok_or_else(|| CellError::host("invites.role"))?;
        let current = self.member_role(&who)?;
        if let Some(current) = current {
            if current >= role {
                return json_response(&json!({ "name": name, "role": current, "joined": false }));
            }
        }
        // A full fragment refuses before the invite spends a use.
        self.check_room(current)?;
        self.exec(
            "INSERT INTO members (principal, role, added_by, added_at, kind, owner) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT (principal) DO UPDATE SET role = excluded.role",
            vec![
                who.as_str().into(),
                role.as_str().into(),
                format!("invite:{id}").into(),
                SqlStorageValue::Integer(js::now_ms()),
                opt(caller.kind.map(IdentityKind::as_str)),
                opt(caller.owner.as_deref()),
            ],
        )?;
        self.exec("UPDATE invites SET uses_left = uses_left - 1 WHERE id = ?", vec![id.as_str().into()])?;
        self.index_change(&who, Some(role))?;
        self.event("member.joined", &format!("{} joined as {} (invite {id})", npub::display(&who), role.as_str()), json!({ "principal": npub::display(&who), "role": role, "invite": id }));
        self.flush_index().await;
        json_response(&json!({ "name": name, "role": role, "joined": true }))
    }

    pub(crate) fn set_visibility(&self, caller: &Caller, body: SetVisibility) -> CellResult<Response> {
        self.require_owner(caller)?;
        let before = self.visibility()?;
        self.set_meta("visibility", body.visibility.as_str())?;
        if body.visibility != Visibility::Public {
            self.close_sockets("anon", "the fragment is no longer public");
        }
        if body.visibility == Visibility::Members {
            self.close_sockets("view", "the fragment is now members only");
        }
        self.event(
            "visibility",
            &format!("visibility {} → {}", before.as_str(), body.visibility.as_str()),
            json!({ "from": before, "to": body.visibility }),
        );
        json_response(&json!({ "ok": true, "visibility": body.visibility }))
    }

    /// New share-link token, inbox token, or webhook secret (default: all three).
    pub(crate) fn rotate(&self, caller: &Caller, body: Value) -> CellResult<Response> {
        self.require_owner(caller)?;
        let all = ["inbox", "view", "webhook"];
        let want: Vec<String> = match &body["scopes"] {
            Value::Null => all.iter().map(|s| s.to_string()).collect(),
            Value::Array(a) if !a.is_empty() => a.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect(),
            Value::Array(_) => all.iter().map(|s| s.to_string()).collect(),
            _ => return Err(CellError::invalid("scopes must be an array of inbox, view, webhook")),
        };
        if let Some(bad) = want.iter().find(|s| !all.contains(&s.as_str())) {
            return Err(CellError::invalid(format!("unknown scope {bad:?} (inbox, view, webhook)")));
        }
        for (scope, key, fresh) in [("inbox", "inbox_token", js::random_hex::<16>()), ("view", "view_token", js::random_hex::<12>()), ("webhook", "webhook_secret", js::random_hex::<16>())] {
            if want.iter().any(|w| w == scope) {
                self.set_meta(key, &fresh)?;
            }
        }
        if want.iter().any(|w| w == "view") {
            self.close_sockets("view", "the share link changed");
        }
        let rotated: Vec<&str> = all.into_iter().filter(|s| want.iter().any(|w| w == s)).collect();
        self.event("tokens.rotated", &rotated.join("+"), json!({ "scopes": rotated }));
        json_response(&json!({
            "ok": true,
            "inbox_token": self.must("inbox_token")?,
            "view_token": self.must("view_token")?,
            "webhook_secret": self.must("webhook_secret")?,
            "rotated": rotated,
        }))
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
        // sealed first: the checks and the write below share one turn
        let sealed = crate::keys::seal(&self.env, &value).await?;
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
