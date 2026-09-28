//! A person's memory (docs/agent-computer.md, slice 3; docs/platform.md):
//! one private fragment of theirs, holding `memory/*.md` (facts) and
//! `skills/<name>/SKILL.md`, that the platform makes on first need and
//! records in the registry (`calls::Memory`). It is found by that record,
//! never by its name: a fragment the person made called `memory` is never
//! taken over (the platform's is then `memory-2`, …). Every computer of
//! theirs is an editor there, made one as it pairs (either way: a
//! fragment's own, or a machine they approve), all of them at once when the
//! memory is made, and a fragment's own computer again at each sync
//! (computer.rs), which covers those paired before memories were.
//! `/api/memory` answers it: GET the recorded one (none: `null`), POST
//! making it first (a person, or an agent for its owner); PUT, its owner
//! alone, names a fragment of theirs as it (`fragment memory use`: one an
//! agent of theirs made before memories were recorded), replacing the one
//! recorded only when asked, and leaving that one as it is.

use fragment_proto::{split_fragment_name, CreateFragment, ErrorCode, Identity, IdentityKind, Visibility};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls;
use crate::routed::{Routed, Signed};
use crate::{ask_registry, bytes_body, create_fragment, forward, Forward};

/// The names tried for a new memory: `memory`, then `memory-2` to this.
const NAMES_MAX: u32 = 9;

fn person(id: &str, username: Option<&str>) -> Signed {
    Signed::new(Identity { id: id.to_string(), kind: IdentityKind::Person, owner: None, username: username.map(str::to_string) }, None)
}

/// A request to the fragment `name`, as `who`.
async fn ask(env: &Env, url: &Url, name: &str, who: Signed, method: Method, inner: &str, body: Option<Value>) -> CellResult<Response> {
    let routed = Routed { name: name.to_string(), url: url.clone(), mode: None, signed: Some(who), credential: None };
    let body = body.and_then(|b| bytes_body(b.to_string().into_bytes()));
    forward(env, &Request::new(url.as_str(), method)?, body, Forward { routed, inner: inner.to_string(), extra: vec![] }).await
}

/// The memory recorded for `owner`, if any.
pub(crate) async fn recorded(env: &Env, owner: &str) -> CellResult<Option<String>> {
    Ok(ask_registry(env, &calls::Memory { owner: owner.to_string(), name: None, replace: false }).await?.name)
}

/// Makes `label.<username>`, members only, as its owner: whether it was
/// made (false: the name is taken).
async fn make(env: &Env, cfg: &Config, url: &Url, owner: &str, username: &str, label: &str) -> CellResult<bool> {
    let create = CreateFragment { name: label.to_string(), visibility: Some(Visibility::Members), template: None, throwaway: false };
    let mut made = create_fragment(env, cfg, url, create, person(owner, Some(username))).await?;
    match made.status_code() {
        200 => Ok(true),
        409 => Ok(false),
        s => Err(CellError::host(format!("making {label}.{username} ({s}): {}", made.text().await.unwrap_or_default()))),
    }
}

/// `owner`'s memory: the one recorded, or one made now (members only,
/// theirs) under the first free name, recorded, and every computer they
/// have made an editor there. Two first needs at once record one; the
/// other's fragment is deleted again.
pub(crate) async fn ensure(env: &Env, cfg: &Config, url: &Url, owner: &str) -> CellResult<String> {
    if let Some(name) = recorded(env, owner).await? {
        return Ok(name);
    }
    let view = ask_registry(env, &calls::View { identity: None, by: calls::By::Identity(owner.to_string()) }).await?;
    let username = view.username.ok_or_else(|| CellError::invalid(format!("choose a username first (sign in at {}/)", cfg.platform(url))))?;
    let mut made = None;
    for n in 1..=NAMES_MAX {
        let label = if n == 1 { "memory".to_string() } else { format!("memory-{n}") };
        if make(env, cfg, url, owner, &username, &label).await? {
            made = Some(fragment_proto::fragment_name(&label, &username));
            break;
        }
    }
    let made = made.ok_or_else(|| CellError::invalid(format!("{username} has fragments named memory to memory-{NAMES_MAX}: none is free for their memory")))?;
    let kept = ask_registry(env, &calls::Memory { owner: owner.to_string(), name: Some(made.clone()), replace: false }).await?.name;
    let kept = kept.ok_or_else(|| CellError::host("the registry recorded no memory"))?;
    if kept != made {
        if let Err(e) = ask(env, url, &made, person(owner, Some(&username)), Method::Delete, "/delete", None).await {
            console_error!("{made}, a second memory made at once, was not deleted: {}", e.message);
        }
        return Ok(kept);
    }
    add_all(env, url, owner, &made, view.computers).await;
    Ok(made)
}

/// Every computer `owner` has, an editor of `memory` (a failure logged: a
/// computer's next sync tries again), and each fragment's own told to sync,
/// so its task client syncs this memory from its next task.
async fn add_all(env: &Env, url: &Url, owner: &str, memory: &str, computers: Vec<fragment_proto::ComputerRef>) {
    for c in computers {
        if let Err(e) = add(env, url, owner, memory, &c.id).await {
            console_error!("{} was not made an editor of {memory}: {}", c.name, e.message);
        }
        if fragment_proto::valid_fragment_name(&c.name) {
            if let Err(e) = crate::computer::ask(env, &c.name, &crate::computer::Ask::Resync).await {
                console_error!("{} was not told to sync: {}", c.name, e.message);
            }
        }
    }
}

/// `PUT /api/memory {fragment, replace?}`: the owner names a fragment of
/// their own as their memory (409 when another is recorded and `replace`
/// is not set), and every computer of theirs is made an editor there.
async fn name_it(env: &Env, url: &Url, who: &Signed, body: &[u8]) -> CellResult<String> {
    #[derive(serde::Deserialize)]
    struct Use {
        fragment: String,
        #[serde(default)]
        replace: bool,
    }
    let Use { fragment, replace } = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
    if !fragment_proto::valid_fragment_name(&fragment) {
        return Err(CellError::invalid("a memory is a fragment: <label>.<username>"));
    }
    let mut status = ask(env, url, &fragment, who.clone(), Method::Get, "/api/status", None).await?;
    let owner = match status.status_code() {
        200 => status.json::<Value>().await?["owner"].as_str().map(str::to_string),
        _ => None,
    };
    if owner.as_deref() != Some(who.id.as_str()) {
        return Err(CellError::new(ErrorCode::Forbidden, format!("{fragment} is not yours: your memory is a fragment you own")));
    }
    let was = recorded(env, &who.id).await?;
    if was.as_ref().is_some_and(|w| *w != fragment) && !replace {
        let was = was.unwrap_or_default();
        return Err(CellError::new(ErrorCode::AlreadyExists, format!("your memory is {was}: replace it with `fragment memory use {fragment} --replace` ({was} is left as it is)")));
    }
    ask_registry(env, &calls::Memory { owner: who.id.clone(), name: Some(fragment.clone()), replace: true }).await?;
    let computers = ask_registry(env, &calls::View { identity: None, by: calls::By::Identity(who.id.clone()) }).await?.computers;
    add_all(env, url, &who.id, &fragment, computers).await;
    Ok(fragment)
}

/// Makes the computer `computer` (an identity) an editor of `memory`: 200,
/// or 404 when there is no such fragment.
async fn add(env: &Env, url: &Url, owner: &str, memory: &str, computer: &str) -> CellResult<u16> {
    let mut put = ask(env, url, memory, person(owner, None), Method::Put, &format!("/api/members/{computer}"), Some(json!({ "role": "editor" }))).await?;
    match put.status_code() {
        s @ (200 | 404) => Ok(s),
        s => Err(CellError::host(format!("{computer} was not made an editor of {memory} ({s}): {}", put.text().await.unwrap_or_default()))),
    }
}

/// `owner`'s memory, made if it is not, with their computer `computer` (an
/// identity) an editor there: its name. A memory its owner deleted is made
/// again under its recorded name.
pub(crate) async fn grant(env: &Env, cfg: &Config, url: &Url, owner: &str, computer: &str) -> CellResult<String> {
    let memory = ensure(env, cfg, url, owner).await?;
    if add(env, url, owner, &memory, computer).await? == 404 {
        let (label, username) = split_fragment_name(&memory).ok_or_else(|| CellError::host("a memory is a fragment"))?;
        if !make(env, cfg, url, owner, username, label).await? || add(env, url, owner, &memory, computer).await? != 200 {
            return Err(CellError::host(format!("{memory}, deleted, was not made again")));
        }
    }
    Ok(memory)
}

/// A fragment's own computer, `name` (its fragment's), an editor of its
/// owner's memory: its name.
pub(crate) async fn grant_named(env: &Env, cfg: &Config, owner: &str, name: &str) -> CellResult<String> {
    let url = Url::parse(&cfg.computer_platform()?).map_err(|e| CellError::host(format!("the platform's URL: {e}")))?;
    let computers = ask_registry(env, &calls::View { identity: None, by: calls::By::Identity(owner.to_string()) }).await?.computers;
    let c = computers.into_iter().find(|c| c.name == name).ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("{name} is not paired")))?;
    grant(env, cfg, &url, owner, &c.id).await
}

/// `GET /api/memory` (anyone signed: theirs, or their owner's), `POST` (a
/// person, or an agent for its owner), and `PUT` (a person): `{name}`.
pub(crate) async fn route(env: &Env, cfg: &Config, url: &Url, method: Method, who: Signed, body: &[u8]) -> CellResult<Response> {
    let owner = match who.kind {
        IdentityKind::Person => who.id.clone(),
        _ => who.owner.clone().ok_or_else(|| CellError::host(format!("{} has no owner", who.id)))?,
    };
    if who.acting_for.as_ref().is_some_and(|asker| *asker != owner) {
        return Err(CellError::new(ErrorCode::Forbidden, "an agent reaches its owner's memory in its owner's turns only"));
    }
    let name = match method {
        Method::Get => recorded(env, &owner).await?,
        Method::Put if who.kind == IdentityKind::Person => Some(name_it(env, url, &who, body).await?),
        Method::Put => return Err(CellError::new(ErrorCode::Forbidden, "a person names their memory: never an agent or a computer")),
        _ if who.kind == IdentityKind::Computer => return Err(CellError::new(ErrorCode::Forbidden, "a computer is given its owner's memory as it pairs and syncs")),
        _ => Some(ensure(env, cfg, url, &owner).await?),
    };
    Ok(Response::from_json(&json!({ "name": name }))?)
}
