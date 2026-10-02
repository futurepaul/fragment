//! A person's home (docs/one-home.md, decision 7): their desktop, a
//! fragment of theirs made from the `desktop` template the first time the
//! platform's `/` opens for them with a username, recorded in the registry
//! (`calls::Home`) and found by that record, as their memory is
//! (memory.rs). `/` then opens it; what the platform's page held moves to
//! `/settings` (auth.rs).
//!
//! - A desktop they made before under the label `desktop` is taken as
//!   theirs (its template says it is one, which its owner alone reads); a
//!   fragment of theirs called `desktop` that is not one is never taken
//!   over: theirs is then `desktop-2`, and so on.
//! - One made here shows their fragments inside it from the start (its
//!   `frame` grant), as one made with the platform's New fragment form
//!   does: the username form that leads here says so, and it is the
//!   platform's own template, made on their own first visit.
//! - One recorded but gone (they deleted it) is made again, in its place.
//! - Two first visits at once record one; the other's desktop is deleted.

use fragment_proto::{fragment_name, CreateFragment, ErrorCode, Identity};
use serde_json::json;
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls;
use crate::routed::Signed;
use crate::{ask_registry, create_fragment, share};

/// The labels tried for a new home: `desktop`, then `desktop-2` to this.
const NAMES_MAX: u32 = 9;
/// The template a home is made from.
const TEMPLATE: &str = "desktop";

/// `who`'s home: the one recorded, while it is there, else one found or
/// made now and recorded.
pub(crate) async fn ensure(env: &Env, cfg: &Config, url: &Url, who: &Identity) -> CellResult<String> {
    let username = who.username.as_deref().ok_or_else(|| CellError::invalid("choose a username first"))?;
    let signer = Signed::new(who.clone(), None);
    let recorded = ask_registry(env, &calls::Home { owner: who.id.clone(), name: None, replace: false }).await?.name;
    if let Some(name) = &recorded {
        match share::ask(env, url, name, &signer, Method::Get, "/api/status", None).await {
            Ok(_) => return Ok(name.clone()),
            // deleted since: made again
            Err(e) if e.code == ErrorCode::NotFound => console_log!("{name}, {}'s home, is gone: a new one is made", who.id),
            Err(e) => return Err(e),
        }
    }
    let mut found: Option<(String, bool)> = None;
    for n in 1..=NAMES_MAX {
        let label = if n == 1 { TEMPLATE.to_string() } else { format!("{TEMPLATE}-{n}") };
        let name = fragment_name(&label, username);
        let create = CreateFragment { name: label, visibility: None, template: Some(TEMPLATE.into()), answers: None };
        let mut made = create_fragment(env, cfg, url, create, signer.clone()).await?;
        match made.status_code() {
            200 => {
                let allow = Some(json!({ "granted": true }));
                if let Err(e) = share::ask(env, url, &name, &signer, Method::Put, "/api/grants/frame", allow).await {
                    // it asks, with its share sheet's button
                    console_error!("{name}: a home's frame grant did not land ({:?}): {}", e.code, e.message);
                }
                found = Some((name, true));
                break;
            }
            409 if desktop_of_theirs(env, url, &name, &signer).await? => {
                found = Some((name, false));
                break;
            }
            409 => {}
            s => return Err(CellError::host(format!("making {name} ({s}): {}", made.text().await.unwrap_or_default()))),
        }
    }
    let (name, new) = found.ok_or_else(|| CellError::invalid(format!("{username} has fragments named {TEMPLATE} to {TEMPLATE}-{NAMES_MAX}, none a desktop: none is free for their home")))?;
    let replace = recorded.is_some();
    let kept = ask_registry(env, &calls::Home { owner: who.id.clone(), name: Some(name.clone()), replace }).await?.name;
    let kept = kept.ok_or_else(|| CellError::host("the registry recorded no home"))?;
    if kept != name && new {
        if let Err(e) = share::ask(env, url, &name, &signer, Method::Delete, "/delete", None).await {
            console_error!("{name}, a second home made at once, was not deleted: {}", e.message);
        }
    }
    Ok(kept)
}

/// Whether `name` is a desktop `signer` made (its template, which its
/// owner alone may read: anyone else's is refused, and is not theirs).
async fn desktop_of_theirs(env: &Env, url: &Url, name: &str, signer: &Signed) -> CellResult<bool> {
    match share::ask(env, url, name, signer, Method::Get, "/api/template", None).await {
        Ok(v) => Ok(v["template"] == TEMPLATE),
        Err(e) if matches!(e.code, ErrorCode::Forbidden | ErrorCode::Unauthenticated | ErrorCode::NotFound) => Ok(false),
        Err(e) => Err(e),
    }
}
