//! Seats and orgs on the platform's API (docs/billing.md; docs/api.md,
//! Seats and orgs): a person's own seat (`/api/seat`), their org to its
//! admins (`/api/org`), and the operators' comps (`/api/admin/seats`) and
//! view of any org (`/api/admin/orgs/<id>`). The registry keeps them
//! (registry/orgs.rs) and pushes each seat's plan to its holder's ledger
//! and computer; this module only checks who asks.

use fragment_proto::org::{CompSeat, Comped, SeatKind, SetSeatKind, SetSleeps};
use fragment_proto::{limits, ErrorCode};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls::By;
use crate::registry::orgs::{CompKind, CompSeatCall, EndComp, Mine, OrgOf, Sleeps, SyncSeat};
use crate::{acting_for, ask_registry, caller, json_answer, read_body, signer, Caller};
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

/// `GET /api/org`: the asker's org, if they are one of its admins.
pub(crate) async fn org(mut req: Request, env: &Env, url: &Url) -> CellResult<Response> {
    let bytes = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let by = by(env, &req, url, &bytes)?;
    json_answer(&ask_registry(env, &OrgOf { by: Some(by), org: None }).await?)
}

/// `/api/admin/…`: the deployment's operators' (docs/api.md, Operators),
/// through a signed request or the shell's session (decision 59).
pub(crate) async fn admin(mut req: Request, env: &Env, cfg: &Config, url: &Url, rest: &[&str]) -> CellResult<Response> {
    let bytes = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let who = signer(env, &req, url, &bytes).await?;
    if !cfg.is_operator(who.key.as_deref(), &who.id)? {
        return Err(CellError::new(ErrorCode::Forbidden, "only the deployment's operators reach /api/admin"));
    }
    match (req.method(), rest) {
        (Method::Post, ["seats"]) => {
            let comp: CompSeat = body(&bytes)?;
            let mut comped = ask_registry(env, &CompSeatCall { by: who.id.clone(), comp }).await?;
            if comped.created && cfg.mail_from.is_some() {
                comped.mailed = mail_comp(env, cfg, &comped).await;
            }
            json_answer(&comped)
        }
        (Method::Patch, ["seats", seat]) => {
            let set: SetSeatKind = body(&bytes)?;
            json_answer(&ask_registry(env, &CompKind { seat: seat.to_string(), kind: set.kind }).await?)
        }
        (Method::Delete, ["seats", seat]) => json_answer(&ask_registry(env, &EndComp { seat: seat.to_string() }).await?),
        (Method::Get, ["orgs", org]) => json_answer(&ask_registry(env, &OrgOf { by: None, org: Some(org.to_string()) }).await?),
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/admin/{}", m.as_ref(), rest.join("/")))),
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
