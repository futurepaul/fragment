//! Seats and orgs on the platform's API (docs/billing.md; docs/api.md,
//! Seats and orgs): a person's own seat (`/api/seat`), their org to its
//! admins (`/api/org`), and the operators' comps (`/api/admin/seats`) and
//! view of any org (`/api/admin/orgs/<id>`). The registry keeps them
//! (registry/orgs.rs) and pushes each seat's plan to its holder's ledger
//! and computer; this module only checks who asks.

use fragment_proto::org::{CompSeat, Comped, OrgMember, SeatKind, SetSeatKind, SetSleeps};
use fragment_proto::{limits, ErrorCode};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls::By;
use crate::registry::orgs::{CompKind, CompSeatCall, EndComp, Mine, OrgOf, Sleeps, SyncSeat};
use crate::registry::seats::{AddAdmin, AddSeat, RemoveAdmin, RemoveSeat, SeatKindChange};
use crate::registry::admin;
use crate::registry::trials::{TrialChange, TrialGet, TrialList, TrialNew};
use crate::{acting_for, ask_registry, caller, json_answer, read_body, Caller};
use fragment_nip98::Payload;

fn body<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> CellResult<T> {
    serde_json::from_slice(bytes).map_err(|e| CellError::invalid(format!("body: {e}")))
}

/// Who asks, for the registry to resolve in the same turn as what it asks.
fn by(env: &Env, req: &Request, url: &Url, bytes: &[u8]) -> CellResult<By> {
    if acting_for(url)?.is_some() {
        return Err(CellError::invalid("`for` is honored on a fragment's routes (/api/f/…) and the fragment list only"));
    }
    Ok(match caller(env, req, url, Payload::Read(bytes))? {
        Caller::Key(k) => By::Key(k),
        Caller::Session(t) => By::Session(t),
    })
}

/// `/api/seat`: `GET` the asker's seat, org and the seats offered them;
/// `PUT {sleeps}` lets their always-on computer sleep, or not.
pub(crate) async fn seat(mut req: Request, env: &Env, url: &Url) -> CellResult<Response> {
    let bytes = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let by = by(env, &req, url, &bytes)?;
    match req.method() {
        Method::Get => json_answer(&ask_registry(env, &Mine { by }).await?),
        Method::Put => {
            let set: SetSleeps = body(&bytes)?;
            json_answer(&ask_registry(env, &Sleeps { by, sleeps: set.sleeps }).await?)
        }
        m => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/seat", m.as_ref()))),
    }
}

/// `/api/org…`, for the asker's org's admins: `GET` it; add a paid seat
/// by email, change one's kind, remove one; add or remove an admin.
pub(crate) async fn org(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NewSeat {
        email: String,
        kind: SeatKind,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NewAdmin {
        email: String,
    }
    let bytes = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let by = by(env, &req, url, &bytes)?;
    match (req.method(), rest) {
        (Method::Get, []) => json_answer(&ask_registry(env, &OrgOf { by: Some(by), org: None }).await?),
        (Method::Post, ["seats"]) => {
            let b: NewSeat = body(&bytes)?;
            let added = ask_registry(env, &AddSeat { by, email: b.email, kind: b.kind }).await?;
            let mailed = added.created && cfg.mail_from.is_some() && mail_seat(env, cfg, &added.member, &added.org_name).await;
            json_answer(&serde_json::json!({ "seat": added.member, "created": added.created, "mailed": mailed }))
        }
        (Method::Patch, ["seats", seat]) => {
            let set: SetSeatKind = body(&bytes)?;
            json_answer(&ask_registry(env, &SeatKindChange { by, seat: seat.to_string(), kind: set.kind }).await?)
        }
        (Method::Delete, ["seats", seat]) => json_answer(&ask_registry(env, &RemoveSeat { by, seat: seat.to_string() }).await?),
        (Method::Post, ["admins"]) => {
            let b: NewAdmin = body(&bytes)?;
            let added = ask_registry(env, &AddAdmin { by, email: b.email }).await?;
            json_answer(&serde_json::json!({ "admin": added.member, "created": added.created }))
        }
        (Method::Delete, ["admins", member]) => json_answer(&ask_registry(env, &RemoveAdmin { by, member: member.to_string() }).await?),
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/org/{}", m.as_ref(), rest.join("/")))),
    }
}

/// Who an operator's request is from (the admin's routes, operators'
/// ledger commands): a key the deployment lists, held by anyone or no one
/// (no registry is asked, as a wipe's); a signed request or the shell's
/// session (decision 59) of an identity it lists, or of a person who holds
/// a key it lists (the session acts as that key may). Anyone else is 403.
/// Answers the operator's name (their identity, or a key no one holds),
/// for the log and a grant's `by`.
pub(crate) async fn operator(env: &Env, cfg: &Config, req: &Request, url: &Url, bytes: &[u8]) -> CellResult<String> {
    if acting_for(url)?.is_some() {
        return Err(CellError::invalid("`for` is honored on a fragment's routes (/api/f/…) and the fragment list only"));
    }
    let refused = || CellError::new(ErrorCode::Forbidden, "only the deployment's operators do this");
    match caller(env, req, url, Payload::Read(bytes))? {
        Caller::Key(key) => {
            let listed = cfg.is_operator(Some(&key), "")?;
            match ask_registry(env, &crate::registry::calls::Resolve { key: key.clone() }).await {
                // a person's: named by their identity
                Ok(who) => (listed || cfg.is_operator(Some(&key), &who.id)?).then_some(who.id).ok_or_else(refused),
                // a listed key no one holds: named by itself
                Err(e) if listed && e.code == ErrorCode::Unauthenticated => Ok(fragment_core::npub::encode(&key)),
                Err(e) => Err(e),
            }
        }
        Caller::Session(token) => {
            let view = ask_registry(env, &crate::registry::calls::View { identity: None, by: By::Session(token) }).await?;
            if cfg.is_operator(None, &view.id)? {
                return Ok(view.id);
            }
            // bounded: an identity holds at most KEYS_PER_IDENTITY_MAX keys
            for k in view.keys.iter().filter(|k| k.revoked_at.is_none()) {
                if let Some(hex) = fragment_core::npub::parse(&k.npub) {
                    if cfg.is_operator(Some(&hex), "")? {
                        return Ok(view.id);
                    }
                }
            }
            Err(refused())
        }
    }
}

/// A query parameter, once.
fn param(url: &Url, k: &str) -> Option<String> {
    url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned()).filter(|v| !v.is_empty())
}

/// `/api/admin/…`: the deployment's operators' (docs/api.md, Operators):
/// comps, orgs, people, trial codes and their mail, billing's health, and
/// the log of what operators did.
pub(crate) async fn admin(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    let bytes = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let by = operator(env, cfg, &req, url, &bytes).await?;
    match (req.method(), rest) {
        (Method::Post, ["seats"]) => {
            let comp: CompSeat = body(&bytes)?;
            let mut comped = ask_registry(env, &CompSeatCall { by, comp }).await?;
            if comped.created && cfg.mail_from.is_some() {
                comped.mailed = mail_comp(env, cfg, &comped).await;
            }
            json_answer(&comped)
        }
        (Method::Patch, ["seats", seat]) => {
            let set: SetSeatKind = body(&bytes)?;
            json_answer(&ask_registry(env, &CompKind { by, seat: seat.to_string(), kind: set.kind }).await?)
        }
        (Method::Delete, ["seats", seat]) => json_answer(&ask_registry(env, &EndComp { by, seat: seat.to_string() }).await?),
        (Method::Get, ["orgs"]) => json_answer(&ask_registry(env, &admin::Orgs { after: param(url, "after") }).await?),
        (Method::Get, ["orgs", org]) => json_answer(&ask_registry(env, &OrgOf { by: None, org: Some(org.to_string()) }).await?),
        (Method::Get, ["people"]) => json_answer(&ask_registry(env, &admin::People { q: param(url, "q"), after: param(url, "after") }).await?),
        (Method::Get, ["people", person]) => json_answer(&person_page(env, person).await?),
        (Method::Get, ["health"]) => json_answer(&ask_registry(env, &admin::Health {}).await?),
        (Method::Get, ["log"]) => {
            let before = param(url, "before").map(|b| b.parse::<u64>().map_err(|_| CellError::invalid("`before` is an entry's number"))).transpose()?;
            json_answer(&ask_registry(env, &admin::Log { before }).await?)
        }
        (Method::Post, ["trials"]) => json_answer(&ask_registry(env, &TrialNew { by, code: body(&bytes)? }).await?),
        (Method::Get, ["trials"]) => json_answer(&ask_registry(env, &TrialList {}).await?),
        (Method::Get, ["trials", id]) => json_answer(&ask_registry(env, &TrialGet { id: id.to_string() }).await?),
        (Method::Patch, ["trials", id]) => json_answer(&ask_registry(env, &TrialChange { by, id: id.to_string(), change: body(&bytes)? }).await?),
        (Method::Post, ["trials", id, "send"]) => {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Send {
                email: String,
            }
            let send: Send = body(&bytes)?;
            let code = ask_registry(env, &TrialGet { id: id.to_string() }).await?;
            if !code.active || code.expires_at.is_some_and(|at| at <= crate::js::now_ms()) {
                return Err(CellError::invalid("this trial code has ended: turn it on, or make another"));
            }
            let kind = match code.kind {
                SeatKind::SeatAlwaysOn => "a seat with an always-on computer",
                SeatKind::Seat => "a seat",
            };
            let text = format!(
                "You are invited to try fragment: {} days of {kind}, free. A card is taken first, and charged once the trial ends unless you cancel.\n\nSign in with this email address, then start your trial: {}/settings?trial={}\n\nYour code: {}\n",
                code.days,
                cfg.platform(),
                code.code,
                code.code
            );
            let mail = crate::mail::Mail { to: send.email.trim().to_ascii_lowercase(), subject: "Try fragment".into(), text };
            crate::mail::send(env, cfg, &mail).await?;
            ask_registry(env, &admin::Note { by, action: "trial-send".into(), target: code.id.clone(), detail: format!("to {}", mail.to) }).await?;
            json_answer(&serde_json::json!({ "sent": true, "to": mail.to }))
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/admin/{}", m.as_ref(), rest.join("/")))),
    }
}

/// A person's page for operators: as the list shows them, their ledger,
/// and their computer (none: `null`).
async fn person_page(env: &Env, person: &str) -> CellResult<serde_json::Value> {
    if !fragment_core::npub::is_identity(person) {
        return Err(CellError::new(ErrorCode::NotFound, format!("no person {person}")));
    }
    let row = ask_registry(env, &admin::Person { person: person.to_string() }).await?;
    let ledger = crate::ledger::ask(env, person, &crate::ledger::Status {}).await.map_err(|e| CellError::new(e.code, e.message))?;
    let id = fragment_core::computer::default_computer_of(person);
    let computer = match crate::computer::ask(env, &id, "computer/view", &serde_json::json!({})).await {
        Ok(v) => v,
        Err(e) if e.code == ErrorCode::NotFound => serde_json::Value::Null,
        Err(e) => return Err(e),
    };
    Ok(serde_json::json!({ "person": row, "ledger": ledger, "computer": computer }))
}

/// A new seat's mail to its email, from its org: where to sign in.
async fn mail_seat(env: &Env, cfg: &Config, seat: &OrgMember, org: &str) -> bool {
    let kind = match seat.seat {
        Some(SeatKind::SeatAlwaysOn) => "a seat with an always-on computer",
        _ => "a seat",
    };
    let held = match seat.person {
        Some(_) => "It is yours now",
        None => "Sign in with this email address to take it",
    };
    let text = format!("{org} gave you {kind} on fragment.\n\n{held}: {}/\n", cfg.platform());
    let mail = crate::mail::Mail { to: seat.email.clone(), subject: "You have a seat on fragment".into(), text };
    match crate::mail::send(env, cfg, &mail).await {
        Ok(_) => true,
        Err(e) => {
            console_error!("{}", serde_json::json!({ "seat-mail": seat.id, "failed": e.message }));
            false
        }
    }
}

/// A new comp's mail to its email: where to sign in to use it. The seat
/// is made whatever this answers; a mail that fails is logged and said.
async fn mail_comp(env: &Env, cfg: &Config, comped: &Comped) -> bool {
    let kind = match comped.seat.seat {
        Some(SeatKind::SeatAlwaysOn) => "a seat with an always-on computer",
        _ => "a seat",
    };
    let held = match comped.seat.person {
        Some(_) => "It is yours now",
        None => "Sign in with this email address to take it",
    };
    let text = format!("You have {kind} on fragment, in {}.\n\n{held}: {}/\n", comped.org.name, cfg.platform());
    let mail = crate::mail::Mail { to: comped.seat.email.clone(), subject: "You have a seat on fragment".into(), text };
    match crate::mail::send(env, cfg, &mail).await {
        Ok(_) => true,
        Err(e) => {
            console_error!("{}", serde_json::json!({ "comp-mail": comped.seat.id, "failed": e.message }));
            false
        }
    }
}

/// A computer was made: its owner's seat is pushed to it again, so it
/// learns whether it stays awake. The computer is made whatever this
/// answers; a push not queued now is queued at the seat's next change.
pub(crate) async fn computer_made(env: &Env, person: &str) {
    if let Err(e) = ask_registry(env, &SyncSeat { person: person.to_string() }).await {
        console_error!("{}", serde_json::json!({ "seat-sync": person, "failed": e.message }));
    }
}
