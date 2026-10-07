//! An operator's wipe of a person (docs/api.md, Operators): everything that
//! is theirs or their agents' deleted, and their username and sign-in
//! freed, so their next sign-in is a new person (a new identity, so a new
//! computer, ledger and list: each is named by it).
//!
//!   GET  /api/people/{person}/wipe   the dry run: what a wipe deletes, or
//!                                    what is left of a wipe begun; it
//!                                    changes nothing
//!   POST /api/people/{person}/wipe   `{confirm, steps?}`: the wipe, as far
//!                                    as one call goes; called again, it
//!                                    goes on where it stopped
//!
//! `{person}` is a username or an identity. Only the deployment's operators
//! ask (`operator`): a key `FRAGMENT_OPERATORS` lists, whether or not anyone
//! holds it, or the key of an identity it lists. A key no one holds is the
//! one to wipe with: a wipe ends the keys and sessions of whom it wipes, so
//! an operator wiping themselves by their own identity could never run it
//! again (refused: `fragment_core::wipe::Refusal::Themselves`).
//!
//! **Where it runs.** Here, in the router, one bounded call at a time; the
//! Registry records how far it got and locks the person meanwhile
//! (registry/wipe.rs); each object a step touches does its own part behind
//! an internal route (`wipe/…`, marked by `WIPE_HEADER`, which the router
//! never passes): a fragment ends its life (ended.rs `wipe_route`), a
//! computer empties itself (computer.rs `wipe`), a ledger and a list empty
//! themselves (ledger.rs, principal.rs). The steps and their order are
//! `fragment_core::wipe::STEPS`; each is idempotent, and is recorded done
//! only once it is whole, so a wipe cut anywhere (a crash, a timeout, a
//! vendor's error) finishes when it is run again, and a finished one run
//! again finds nothing left.

use fragment_core::wipe::{self as rules, Progress, Step, Whose};
use fragment_nip98::Payload;
use fragment_proto::wipe::{ComputerFound, Found, Ran, WipeAsk, WipeReport, WipeState};
use fragment_proto::{limits, ErrorCode, IdentityKind, Role};
use serde::Deserialize;
use serde_json::json;
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls::{self, WipeBegin, WipeFacts, WipeLook, WipeStep};
use crate::registry::wipe::refused;
use crate::{acting_for, ask_registry, authenticate, js, json_answer, read_body, routed};

/// The marker a wipe's internal calls carry (routed.rs `marker`): the
/// router never passes it, so a request with it came from here.
pub const WIPE_HEADER: &str = "x-fragment-wipe";

/// Pages of a list (200 rows each: principal.rs) a wipe reads at most.
const LIST_PAGES_MAX: usize = 50;
/// Calls to a computer's own wipe one call makes at most (each deletes up
/// to ten pages of its saves' prefix).
const COMPUTER_CALLS_MAX: usize = 10;

/// Who asks a wipe: the operator's key, and their identity when they are
/// an operator by it (not by the key).
struct Operator {
    key: String,
    identity: Option<String>,
}

impl Operator {
    /// Who the wipe's record names as having begun it.
    fn name(&self) -> String {
        self.identity.clone().unwrap_or_else(|| fragment_core::npub::encode(&self.key))
    }
}

/// The operator a request is from: signed (a platform session never is an
/// operator's), by a key the deployment lists, held by anyone or no one
/// (no registry is asked: a wipe of its holder leaves it working), or by
/// the key of an identity it lists. Anyone else is 403.
async fn operator(env: &Env, cfg: &Config, req: &Request, url: &Url, body: &[u8]) -> CellResult<Operator> {
    if acting_for(url)?.is_some() {
        return Err(CellError::invalid("`for` is honored on a fragment's routes (/api/f/…) and the fragment list only"));
    }
    let key = authenticate(req, url, Payload::Read(body))?;
    if cfg.is_operator(Some(&key), "")? {
        return Ok(Operator { key, identity: None });
    }
    let who = ask_registry(env, &calls::Resolve { key: key.clone() }).await?;
    match cfg.is_operator(Some(&key), &who.id)? {
        true => Ok(Operator { key, identity: Some(who.id) }),
        false => Err(CellError::new(ErrorCode::Forbidden, "only the deployment's operators wipe a person")),
    }
}

/// `/api/people/{person}/wipe`.
pub(crate) async fn route(mut req: Request, env: &Env, cfg: &Config, url: &Url, person: &str) -> CellResult<Response> {
    let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let operator = operator(env, cfg, &req, url, &body).await?;
    let facts = ask_registry(env, &WipeLook { person: person.to_string() }).await?;
    match req.method() {
        Method::Get => json_answer(&report(env, &facts, vec![]).await?),
        Method::Post => {
            let ask: WipeAsk = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e} (a wipe names, in `confirm`, the identity its dry run answered)")))?;
            rules::confirmed(&facts.identity, Some(&ask.confirm)).map_err(refused)?;
            rules::may_wipe(operator.identity.as_deref(), &facts.identity).map_err(refused)?;
            let deadline = js::now_ms() + rules::CALL_BUDGET_MS;
            let ran = run(env, &facts.identity, &operator, ask.steps, deadline).await?;
            // read again by identity: a finished wipe frees the username
            let facts = ask_registry(env, &WipeLook { person: facts.identity.clone() }).await?;
            json_answer(&report(env, &facts, ran).await?)
        }
        m => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}

/// Runs the wipe of `person` from its first step not done, until it is
/// done, `steps` steps ran, or the call's time is spent; a step that does
/// not finish (its work is bigger than one call, or something it asked
/// failed) ends the call, said in its `Ran`. A finished wipe runs again
/// only the steps that need no registry (its computer, ledger and lists),
/// each idempotent: a straggler found after it is deleted, and nothing
/// else changes.
async fn run(env: &Env, person: &str, operator: &Operator, steps: Option<u32>, deadline: i64) -> CellResult<Vec<Ran>> {
    // begun (or going on): locked, every session and key of theirs ended
    let facts = ask_registry(env, &WipeBegin { identity: person.to_string(), by: operator.name() }).await?;
    let stored = facts.done.ok_or_else(|| CellError::host(format!("a wipe of {person} began and has no progress")))?;
    let mut progress = Progress::stored(i64::from(stored)).map_err(|c| CellError::host(format!("the registry counts {} steps of a wipe", c.0)))?;
    let limit = steps.map_or(rules::STEPS.len(), |n| (n as usize).min(rules::STEPS.len()));
    let mut ran = Vec::with_capacity(rules::STEPS.len());
    if progress.finished() {
        for step in [Step::Computer, Step::Ledger, Step::Lists].into_iter().take(limit) {
            ran.push(ran_of(step, run_step(env, &facts, step, deadline).await));
        }
        return Ok(ran);
    }
    // bounded: one step a pass, at most STEPS of them; each step's own work
    // is bounded (its units, and the call's deadline)
    while let Some(step) = progress.next() {
        if ran.len() >= limit || js::now_ms() > deadline {
            break;
        }
        let r = ran_of(step, run_step(env, &facts, step, deadline).await);
        let done = r.done;
        ran.push(r);
        if !done {
            break;
        }
        let recorded = ask_registry(env, &WipeStep { identity: person.to_string(), step }).await?;
        let next = Progress::stored(i64::from(recorded.done)).map_err(|c| CellError::host(format!("the registry counts {} steps of a wipe", c.0)))?;
        assert!(next.has_done(step), "a step recorded done is done");
        progress = next;
    }
    assert!(ran.len() <= rules::STEPS.len(), "a call runs each step at most once");
    Ok(ran)
}

/// A step's outcome as its report says it: done, or what it is waiting
/// for, or why it failed this time (the next call tries again).
fn ran_of(step: Step, outcome: CellResult<Did>) -> Ran {
    match outcome {
        Ok(did) => Ran { step: step.name().into(), done: did.done, deleted: did.deleted, note: did.note },
        Err(e) => Ran { step: step.name().into(), done: false, deleted: 0, note: Some(format!("failed this time ({:?}): {}", e.code, e.message)) },
    }
}

/// What one step did in one call.
struct Did {
    done: bool,
    deleted: u64,
    note: Option<String>,
}

impl Did {
    fn finished(deleted: u64) -> Did {
        Did { done: true, deleted, note: None }
    }
}

/// One step of the wipe of `facts.identity`, as far as the call's time goes.
async fn run_step(env: &Env, facts: &WipeFacts, step: Step, deadline: i64) -> CellResult<Did> {
    let person = facts.identity.as_str();
    match step {
        Step::Computer => {
            let computer = fragment_core::computer::default_computer_of(person);
            let mut deleted = 0u64;
            // bounded: COMPUTER_CALLS_MAX calls, each its own pages
            for _ in 0..COMPUTER_CALLS_MAX {
                let w = crate::computer::ask(env, &computer, "computer/wipe", &json!({})).await?;
                deleted += w["records"].as_u64().unwrap_or(0) + w["objects"].as_u64().unwrap_or(0);
                if w["more"] != true {
                    return Ok(Did::finished(deleted));
                }
                if js::now_ms() > deadline {
                    break;
                }
            }
            Ok(Did { done: false, deleted, note: Some("its saves' prefix holds more: the next call deletes on".into()) })
        }
        Step::Fragments => {
            let names = theirs(env, facts, true).await?;
            let (mut ended, mut skipped) = (0u64, vec![]);
            for (i, name) in names.iter().enumerate() {
                if i >= rules::FRAGMENTS_PER_CALL || js::now_ms() > deadline {
                    return Ok(Did { done: false, deleted: ended, note: Some(format!("{} fragments left for the next call", names.len() - i)) });
                }
                match crate::fragment::ask(env, name, "wipe/end", &json!({ "owner": person })).await {
                    Ok(v) => ended += u64::from(v["ended"] == true),
                    // someone else's: never touched (a wipe ends its person's only)
                    Err(e) if e.code == ErrorCode::Forbidden => skipped.push(name.clone()),
                    Err(e) => return Err(e),
                }
            }
            let note = (!skipped.is_empty()).then(|| format!("skipped, someone else's: {}", skipped.join(", ")));
            Ok(Did { done: true, deleted: ended, note })
        }
        Step::Memberships => {
            let mut left = 0u64;
            let mut asked = 0usize;
            for (principal, kind) in whom(facts) {
                for (fragment, role) in list(env, &principal).await? {
                    if role.is_none() || rules::whose(&fragment, facts.username.as_deref()) == Whose::Theirs {
                        continue;
                    }
                    if asked >= rules::FRAGMENTS_PER_CALL || js::now_ms() > deadline {
                        return Ok(Did { done: false, deleted: left, note: Some("memberships left for the next call".into()) });
                    }
                    asked += 1;
                    let v = crate::fragment::ask(env, &fragment, "wipe/leave", &json!({ "principal": principal, "kind": kind })).await?;
                    left += u64::from(v["left"] == true);
                }
            }
            Ok(Did::finished(left))
        }
        Step::Cleanup => {
            let names = theirs(env, facts, false).await?;
            let mut busy = vec![];
            for (i, name) in names.iter().enumerate() {
                if i >= rules::FRAGMENTS_PER_CALL * 4 || js::now_ms() > deadline {
                    return Ok(Did { done: false, deleted: 0, note: Some(format!("{} fragments not looked at yet", names.len() - i)) });
                }
                let v = crate::fragment::ask(env, name, "wipe/end", &json!({ "owner": person })).await?;
                if v["left"].as_u64() != Some(0) {
                    busy.push(name.clone());
                }
            }
            match busy.is_empty() {
                true => Ok(Did::finished(names.len() as u64)),
                false => Ok(Did { done: false, deleted: 0, note: Some(format!("still cleaning (members' lists, app databases, blobs, repos): {}", busy.join(", "))) }),
            }
        }
        Step::Ledger => {
            let v = routed::ask_object(env, "LEDGER", person, "wipe/end", &json!({})).await?;
            Ok(Did::finished(u64::from(v["held"] == true)))
        }
        Step::Lists => {
            let mut rows = 0u64;
            for (principal, _) in whom(facts) {
                let v = routed::ask_object(env, "PRINCIPAL", &principal, "wipe/end", &json!({})).await?;
                rows += v["rows"].as_u64().unwrap_or(0);
            }
            Ok(Did::finished(rows))
        }
        Step::Pictures => {
            // bytes another identity also set stay theirs
            let keys: Vec<String> = facts.pictures.iter().filter(|p| !p.shared).map(|p| format!("pictures/{}", p.sha)).collect();
            if !keys.is_empty() {
                js::blob_delete(env, &keys).await?;
            }
            Ok(Did::finished(keys.len() as u64))
        }
        // its rows go as the registry records it done (`WipeStep`)
        Step::Registry => Ok(Did::finished(0)),
    }
}

/// The person and each agent of theirs, with their kind.
fn whom(facts: &WipeFacts) -> Vec<(String, IdentityKind)> {
    let mut out = vec![(facts.identity.clone(), IdentityKind::Person)];
    out.extend(facts.agents.iter().map(|a| (a.clone(), IdentityKind::Agent)));
    out
}

/// A list's rows (`Principal`'s `wipe/view`, a page at a time): each
/// fragment, and the role it holds there (`None`: one it left, or ended).
async fn list(env: &Env, principal: &str) -> CellResult<Vec<(String, Option<Role>)>> {
    #[derive(Deserialize)]
    struct Row {
        fragment: String,
        role: Option<Role>,
    }
    let mut rows = vec![];
    let mut after: Option<String> = None;
    // bounded: LIST_PAGES_MAX pages
    for _ in 0..LIST_PAGES_MAX {
        let v = routed::ask_object(env, "PRINCIPAL", principal, "wipe/view", &json!({ "after": after })).await?;
        let page: Vec<Row> = serde_json::from_value(v["rows"].clone()).map_err(|e| CellError::host(format!("a list's wipe view: {e}")))?;
        after = page.last().map(|r| r.fragment.clone());
        rows.extend(page.into_iter().map(|r| (r.fragment, r.role)));
        if v["more"] != true {
            return Ok(rows);
        }
    }
    Err(CellError::host(format!("{principal}'s list holds more than {LIST_PAGES_MAX} pages")))
}

/// The fragments that are the person's (under their username) on their and
/// their agents' lists (`live`: those a role still names; else every one,
/// ended ones with their rows left), and the agent fragments the registry
/// names: each once, sorted.
async fn theirs(env: &Env, facts: &WipeFacts, live: bool) -> CellResult<Vec<String>> {
    let mut names = facts.agent_fragments.clone();
    for (principal, _) in whom(facts) {
        for (fragment, role) in list(env, &principal).await? {
            if (role.is_some() || !live) && rules::whose(&fragment, facts.username.as_deref()) == Whose::Theirs {
                names.push(fragment);
            }
        }
    }
    names.retain(|n| fragment_proto::valid_fragment_name(n));
    names.sort();
    names.dedup();
    Ok(names)
}

/// What a wipe finds of the person (a dry run's: what it deletes; once
/// wiped: what is left), and where their wipe is.
async fn report(env: &Env, facts: &WipeFacts, ran: Vec<Ran>) -> CellResult<WipeReport> {
    let progress = facts.done.map(|d| Progress::stored(i64::from(d))).transpose().map_err(|c| CellError::host(format!("the registry counts {} steps of a wipe", c.0)))?;
    let state = match progress {
        None => WipeState::Live,
        Some(p) if p.finished() => WipeState::Wiped,
        Some(_) => WipeState::Wiping,
    };
    let found = survey(env, facts, state).await?;
    Ok(WipeReport {
        identity: facts.identity.clone(),
        username: facts.username.clone(),
        state,
        next: progress.and_then(Progress::next).map(|s| s.name().to_string()),
        done: state == WipeState::Wiped && found.is_empty(),
        found,
        ran,
    })
}

/// What the person and their agents hold now, from each object a wipe
/// empties: the registry's rows (`facts`), their lists, computer and ledger.
async fn survey(env: &Env, facts: &WipeFacts, state: WipeState) -> CellResult<Found> {
    let (mut fragments, mut memberships, mut lists) = (vec![], vec![], 0u64);
    for (principal, _) in whom(facts) {
        for (fragment, role) in list(env, &principal).await? {
            lists += 1;
            let Some(role) = role else { continue };
            match rules::whose(&fragment, facts.username.as_deref()) {
                Whose::Theirs => fragments.push(fragment),
                Whose::Elsewhere => memberships.push(format!("{fragment} ({}, {principal})", role.as_str())),
            }
        }
    }
    let computer = fragment_core::computer::default_computer_of(&facts.identity);
    let c = crate::computer::ask(env, &computer, "computer/wipe-view", &json!({})).await?;
    let ledger = routed::ask_object(env, "LEDGER", &facts.identity, "wipe/view", &json!({})).await?;
    Ok(Found {
        sign_ins: facts.sign_ins,
        keys: facts.keys,
        sessions: facts.sessions,
        pictures: facts.pictures.len() as u64,
        // a wiped person's agents are gone; their lists were read above
        agents: if state == WipeState::Wiped { vec![] } else { facts.agents.clone() },
        fragments: rules::listed(fragments),
        memberships: rules::listed(memberships),
        computer: Some(ComputerFound {
            computer,
            phase: c["phase"].as_str().map(str::to_string),
            saves: c["saves"].as_u64().unwrap_or(0),
            backups: c["backups"].as_u64().unwrap_or(0),
            more: c["more"] == true,
            snapshot: c["snapshot"] == true,
        }),
        ledger: ledger["held"] == true,
        lists,
    })
}
