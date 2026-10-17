//! Seats and orgs on the platform's API (docs/billing.md; docs/api.md,
//! Seats and orgs): a person's own seat (`/api/seat`), their org to its
//! admins (`/api/org`), and the operators' comps (`/api/admin/seats`) and
//! view of any org (`/api/admin/orgs/<id>`). The registry keeps them
//! (registry/orgs.rs) and pushes each seat's plan to its holder's ledger
//! and computer; this module only checks who asks.

use fragment_core::mail::PRODUCT;
use fragment_proto::org::{CompSeat, Comped, OrgMember, SeatKind, SeatStatus, SetSeatKind, SetSleeps};
use fragment_proto::{limits, ErrorCode};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls::By;
use crate::registry::orgs::{CompKind, CompSeatCall, EndComp, Mine, Mined, OrgOf, Sleeps, SyncSeat};
use crate::registry::offers::{AcceptOffer, DeclineOffer, LeaveOrg};
use crate::registry::seats::{AddAdmin, AddSeat, Added, RemoveAdmin, RemoveSeat};
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
        return Err(CellError::invalid(crate::FOR_HONORED));
    }
    Ok(match caller(env, req, url, Payload::Read(bytes))? {
        Caller::Key(k) => By::Key(k),
        Caller::Session(t) => By::Session(t),
    })
}

/// `/api/seat…`: `GET` the asker's seat, org and what is offered them,
/// whether they are a guest, and what is sold; `PUT {sleeps}` lets their
/// always-on computer sleep, or not. Their own answers: accept or decline
/// an offer (`offers/{id}/accept|decline`), and leave their org (`leave`, a
/// seat's holder who is no admin). Nobody joins an org but by these.
pub(crate) async fn seat(mut req: Request, env: &Env, url: &Url, rest: &[&str]) -> CellResult<Response> {
    let bytes = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let by = by(env, &req, url, &bytes)?;
    let empty = || -> CellResult<()> {
        // a body is `{}` or none: what is answered is in the path
        match bytes.is_empty() || serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&bytes).is_ok_and(|m| m.is_empty()) {
            true => Ok(()),
            false => Err(CellError::invalid("this takes no body (or {})")),
        }
    };
    match (req.method(), rest) {
        (Method::Post, ["offers", offer, "accept"]) => {
            empty()?;
            json_answer(&ask_registry(env, &AcceptOffer { by, offer: offer.to_string() }).await?)
        }
        (Method::Post, ["offers", offer, "decline"]) => {
            empty()?;
            json_answer(&ask_registry(env, &DeclineOffer { by, offer: offer.to_string() }).await?)
        }
        (Method::Post, ["leave"]) => {
            empty()?;
            json_answer(&ask_registry(env, &LeaveOrg { by }).await?)
        }
        (_, [_, ..]) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/seat/{}", req.method().as_ref(), rest.join("/")))),
        (m, []) => seat_itself(env, &bytes, by, m).await,
    }
}

/// `GET` or `PUT /api/seat`.
async fn seat_itself(env: &Env, bytes: &[u8], by: By, method: Method) -> CellResult<Response> {
    match method {
        Method::Get => {
            let Mined { person, mine } = ask_registry(env, &Mine { by }).await?;
            // holding none, their ledger says whether a plan of its own gives them one
            let guest = match mine.seat {
                Some(_) => false,
                None => {
                    let ledger = crate::ledger::ask(env, &person, &crate::ledger::Status {}).await?;
                    fragment_core::org::guest(false, ledger.plan, ledger.seat)
                }
            };
            json_answer(&SeatStatus { mine, guest, offer: fragment_core::stripe::offer() })
        }
        Method::Put => {
            let set: SetSleeps = body(bytes)?;
            json_answer(&ask_registry(env, &Sleeps { by, sleeps: set.sleeps }).await?)
        }
        m => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/seat", m.as_ref()))),
    }
}

/// `/api/org…`, for the asker's org's admins: `GET` it; offer a paid seat
/// by email (the one sold, `seat_always_on`), remove one; offer or remove
/// an admin's place. An offer is mailed; it is no one's until they accept
/// it. One of the org's members is given it at once.
pub(crate) async fn org(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct NewSeat {
        email: String,
        #[serde(default = "SeatKind::sold")]
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
            let mailed = added.created && cfg.mail_from.is_some() && mail_offer(env, cfg, &added).await;
            json_answer(&serde_json::json!({ "seat": added.member, "created": added.created, "mailed": mailed }))
        }
        (Method::Delete, ["seats", seat]) => json_answer(&ask_registry(env, &RemoveSeat { by, seat: seat.to_string() }).await?),
        (Method::Post, ["admins"]) => {
            let b: NewAdmin = body(&bytes)?;
            let added = ask_registry(env, &AddAdmin { by, email: b.email }).await?;
            let mailed = added.created && cfg.mail_from.is_some() && mail_offer(env, cfg, &added).await;
            json_answer(&serde_json::json!({ "admin": added.member, "created": added.created, "mailed": mailed }))
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
        return Err(CellError::invalid(crate::FOR_HONORED));
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
        Caller::Session(token) => session_operator(env, cfg, token).await?.ok_or_else(refused),
    }
}

/// The operator a platform session is (decision 59): an identity the
/// deployment lists, or a person who holds a key it lists. Answers their
/// identity, or `None` for anyone else; a session that is not live is the
/// registry's 401. `operator` holds the shell's session to it, and
/// `GET /admin` the page's (shell.rs `admin_page`).
pub(crate) async fn session_operator(env: &Env, cfg: &Config, token: String) -> CellResult<Option<String>> {
    let view = ask_registry(env, &crate::registry::calls::View { identity: None, by: By::Session(token) }).await?;
    if cfg.is_operator(None, &view.id)? {
        return Ok(Some(view.id));
    }
    // bounded: an identity holds at most KEYS_PER_IDENTITY_MAX keys
    for k in view.keys.iter().filter(|k| k.revoked_at.is_none()) {
        if let Some(hex) = fragment_core::npub::parse(&k.npub) {
            if cfg.is_operator(Some(&hex), "")? {
                return Ok(Some(view.id));
            }
        }
    }
    Ok(None)
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
        (Method::Put, ["people", person, "computer", "image"]) => json_answer(&pin(env, &by, person, &bytes).await?),
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
            // a code of the $100 seat, made before it stopped being sold, is refused
            fragment_core::stripe::for_sale(code.kind)?;
            let text = format!(
                "You are invited to try {PRODUCT}: {} days of a seat with an always-on computer, free. A card is taken first, and charged once the trial ends unless you cancel.\n\nSign in with this email address, then start your trial: {}/welcome?trial={}\n\nYour code: {}\n",
                code.days,
                cfg.platform(),
                code.code,
                code.code
            );
            let mail = crate::mail::Mail { to: send.email.trim().to_ascii_lowercase(), subject: format!("Try {PRODUCT}"), text };
            crate::mail::send(env, cfg, &mail).await?;
            ask_registry(env, &admin::Note { by, action: "trial-send".into(), target: code.id.clone(), detail: format!("to {}", mail.to) }).await?;
            json_answer(&serde_json::json!({ "sent": true, "to": mail.to }))
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/admin/{}", m.as_ref(), rest.join("/")))),
    }
}

/// A person's page for operators: as the list shows them, their ledger,
/// their computer, and the whole references of the image it runs and of
/// the one its next start runs (both `null` without a computer).
async fn person_page(env: &Env, person: &str) -> CellResult<serde_json::Value> {
    if !fragment_core::npub::is_identity(person) {
        return Err(CellError::new(ErrorCode::NotFound, format!("no person {person}")));
    }
    let row = ask_registry(env, &admin::Person { person: person.to_string() }).await?;
    let ledger = crate::ledger::ask(env, person, &crate::ledger::Status {}).await.map_err(|e| CellError::new(e.code, e.message))?;
    let id = fragment_core::computer::default_computer_of(person);
    let (computer, references) = match crate::computer::ask(env, &id, "computer/view", &serde_json::json!({})).await {
        Ok(v) => (v, crate::computer::ask(env, &id, "computer/references", &serde_json::json!({})).await?),
        Err(e) if e.code == ErrorCode::NotFound => (serde_json::Value::Null, serde_json::Value::Null),
        Err(e) => return Err(e),
    };
    Ok(serde_json::json!({ "person": row, "ledger": ledger, "computer": computer, "references": references }))
}

/// An operator pins a person's computer to another of the deployment's
/// images (docs/computers.md, "Stable and canary"): onto the canary, or
/// back. As its owner's pin is, its next start runs it, and awake on
/// another, its owner is told an update is ready. Logged as `pin`.
async fn pin(env: &Env, by: &str, person: &str, bytes: &[u8]) -> CellResult<serde_json::Value> {
    if !fragment_core::npub::is_identity(person) {
        return Err(CellError::new(ErrorCode::NotFound, format!("no person {person}")));
    }
    let image = crate::computer::image_named(bytes)?;
    let id = fragment_core::computer::default_computer_of(person);
    let view = crate::computer::ask(env, &id, "computer/pin", &serde_json::json!({ "image": image })).await.map_err(|e| match e.code {
        ErrorCode::NotFound => CellError::new(ErrorCode::NotFound, format!("{person} has no computer yet")),
        _ => e,
    })?;
    ask_registry(env, &admin::Note { by: by.to_string(), action: "pin".into(), target: person.to_string(), detail: image }).await?;
    Ok(view)
}

/// What an org's row gives, as a mail says it.
fn what(m: &OrgMember) -> &'static str {
    match (m.seat, m.admin) {
        (Some(SeatKind::SeatAlwaysOn), true) => "a seat with an always-on computer, and a place as its admin",
        (Some(_), true) => "a seat, and a place as its admin",
        (Some(SeatKind::SeatAlwaysOn), false) => "a seat with an always-on computer",
        (Some(_), false) => "a seat",
        (None, _) => "a place as its admin (managing its seats and billing)",
    }
}

/// An offer's mail (or, to one of the org's members, what they were
/// given): from whom, and where to accept or decline it. The offer is
/// made whatever this answers; a mail that fails is logged and said.
async fn mail_offer(env: &Env, cfg: &Config, added: &Added) -> bool {
    let m = &added.member;
    let from = added.from.as_deref().map(|f| format!(" ({f})")).unwrap_or_default();
    let text = match m.person {
        Some(_) => format!("{}{from} gave you {} on {PRODUCT}.\n\nIt is yours now: {}/settings\n", added.org_name, what(m), cfg.platform()),
        None => format!(
            "{}{from} offers you {} on {PRODUCT}.\n\nNothing is yours until you accept it. Sign in with this email address to accept or decline it: {}/settings\n",
            added.org_name,
            what(m),
            cfg.platform()
        ),
    };
    let subject = match m.person {
        Some(_) => format!("{} gave you a place on {PRODUCT}", added.org_name),
        None => format!("{} offers you a place on {PRODUCT}", added.org_name),
    };
    let mail = crate::mail::Mail { to: m.email.clone(), subject, text };
    match crate::mail::send(env, cfg, &mail).await {
        Ok(_) => true,
        Err(e) => {
            console_error!("{}", serde_json::json!({ "offer-mail": m.id, "failed": e.message }));
            false
        }
    }
}

/// A new comp's mail to its email: where to sign in to use it, or to
/// accept it (an email no one signs in with yet is offered it). The seat
/// is made whatever this answers; a mail that fails is logged and said.
async fn mail_comp(env: &Env, cfg: &Config, comped: &Comped) -> bool {
    let kind = match comped.seat.seat {
        Some(SeatKind::SeatAlwaysOn) => "a seat with an always-on computer",
        _ => "a seat",
    };
    let held = match comped.seat.person {
        Some(_) => "It is yours now",
        None => "Sign in with this email address to accept it",
    };
    let text = format!("You have {kind} on {PRODUCT}, in {}.\n\n{held}: {}/\n", comped.org.name, cfg.platform());
    let mail = crate::mail::Mail { to: comped.seat.email.clone(), subject: format!("You have a seat on {PRODUCT}"), text };
    match crate::mail::send(env, cfg, &mail).await {
        Ok(_) => true,
        Err(e) => {
            console_error!("{}", serde_json::json!({ "comp-mail": comped.seat.id, "failed": e.message }));
            false
        }
    }
}

/// A computer was made: its owner's seat is pushed to it again, so it
/// learns whether it stays awake (a push before it was made found none, and
/// counted as done). The computer is made whatever this answers; a push
/// not queued fails the making, so its caller makes it again (the same
/// one: `computer/init` is idempotent), which asks again.
pub(crate) async fn computer_made(env: &Env, person: &str) -> CellResult<()> {
    ask_registry(env, &SyncSeat { person: person.to_string() }).await.inspect_err(|e| {
        console_error!("{}", serde_json::json!({ "seat-sync": person, "failed": e.message }));
    })
}
