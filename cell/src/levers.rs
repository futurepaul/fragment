//! The test levers, `/api/test/*` (docs/api.md, `FRAGMENT_TEST_SECRET`):
//! the router sends a request here only once it has checked the fleet's
//! test secret is set and that the request carries it
//! (`fragment_core::levers`); to anyone else they are no route.
//!
//!   POST /api/test/signin   {email, paidCalls?}: an e2e person's platform session
//!   POST /api/test/people   {after?}: the e2e people, a page at a time (the sweep's)
//!   POST /api/test/ledger   {identity, op, …}: a lever on that person's ledger
//!   POST /api/test/keys     {fragment, op, …}: seal or open as that fragment
//!   POST /api/test/fragment {fragment, op, …}: a lever on that fragment
//!   POST /api/test/computer {computer, op, …}: a lever on that computer (kill, saves, fail-saves, always-on)
//!   POST /api/test/registry {…}: a lever on the registry (the whole deployment's)
//!
//! An e2e person signs in by an `@e2e.test` email under the e2e issuer, so
//! no real sign-in ever reaches them, and is made a seat whose paid calls
//! (model calls and AI steps) are capped at what the request asks for, at
//! most `levers::E2E_PAID_CALLS_MAX` and none unless asked: a hosted run
//! on real models spends only the budget it lends out.
//!
//! On a branch deployment (a preview, `Config::levers_scoped`) the levers
//! reach the e2e's own things alone: fragments labelled `e2e-…`, e2e
//! people's ledgers and computers; the registry's (the whole deployment's) are no
//! route; and a day makes at most `levers::E2E_PEOPLE_DAILY_MAX` e2e
//! people and lends them at most `levers::E2E_PAID_CALLS_DAILY_MAX` paid
//! calls. A secret that leaked spends that much, and touches no one real.

use fragment_core::{levers, npub};
use fragment_proto::computer::valid_computer_id;
use fragment_proto::ledger::{Plan, SetPlan};
use fragment_proto::{limits, ErrorCode};
use serde::{Deserialize, Serialize};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls;
use crate::{ask_registry, check_name, json_answer, ledger, read_body, routed};

/// The ledger command that makes an e2e person a seat: one id, so signing
/// the same person in again sets nothing twice.
const E2E_SEAT_ID: &str = "e2e-seat";

/// `POST /api/test/signin`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SignIn {
    email: String,
    #[serde(default)]
    paid_calls: u64,
}

/// Its answer: the platform session's token (the cookie's value), whose
/// person it is, and whether this sign-in made them.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignedIn {
    session: String,
    identity: String,
    created: bool,
    paid_calls: u64,
}

/// The lever `rest` names (after `/api/test/`), on a fleet whose secret
/// the request carried.
pub async fn route(mut req: Request, env: &Env, cfg: &Config, rest: &[&str]) -> CellResult<Response> {
    assert!(cfg.test_hooks, "the router sends a lever only on a fleet with a test secret");
    let body = read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    match (req.method(), rest) {
        (Method::Post, ["signin"]) => {
            let asked: SignIn = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            json_answer(&sign_in(env, asked).await?)
        }
        (Method::Post, ["people"]) => {
            let hook: calls::E2ePeople = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            json_answer(&ask_registry(env, &calls::TestHook::E2ePeople(hook)).await?)
        }
        (Method::Post, ["ledger"]) => {
            /// `{identity, …}`: a lever on that person's ledger (ledger.rs `TestHook`).
            #[derive(Deserialize)]
            struct TestLedger {
                identity: String,
                #[serde(flatten)]
                hook: ledger::TestHook,
            }
            let t: TestLedger = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            if !npub::is_identity(&t.identity) {
                return Err(CellError::invalid("name an identity"));
            }
            if cfg.levers_scoped && !is_e2e(env, &t.identity).await? {
                return Err(CellError::new(ErrorCode::Forbidden, "on a preview, the ledger levers reach e2e people alone"));
            }
            json_answer(&ledger::ask(env, &t.identity, &t.hook).await?)
        }
        (Method::Post, [hook @ ("keys" | "fragment")]) => {
            /// The fragment a lever's body names (the rest is the fragment's to read).
            #[derive(Deserialize)]
            struct TestTarget {
                fragment: String,
            }
            let target: TestTarget = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            check_name(&target.fragment)?;
            if cfg.levers_scoped && !levers::is_e2e_fragment(&target.fragment) {
                return Err(CellError::new(ErrorCode::Forbidden, format!("on a preview, the fragment levers reach the e2e's own fragments ({}…) alone", levers::E2E_LABEL_PREFIX)));
            }
            let body = String::from_utf8(body).map_err(|_| CellError::invalid("body: not UTF-8"))?;
            let inner = routed::internal_request(&format!("test/{hook}"), &body)?;
            Ok(env.durable_object("FRAGMENT")?.get_by_name(&target.fragment)?.fetch_with_request(inner).await?)
        }
        (Method::Post, ["computer"]) => {
            /// `{computer, op, times?, on?}`: a lever on that computer
            /// (computer.rs, `lever`): `times` is `fail-saves`', `on` is
            /// `always-on`'s.
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct TestComputer {
                computer: String,
                op: String,
                #[serde(default)]
                times: Option<u32>,
                #[serde(default)]
                on: Option<bool>,
            }
            let t: TestComputer = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            if !valid_computer_id(&t.computer) {
                return Err(CellError::invalid("a computer's id is computer:<24 hex>"));
            }
            if cfg.levers_scoped {
                let view = crate::computer::ask(env, &t.computer, "computer/view", &serde_json::json!({})).await?;
                let owner = view["owner"].as_str().ok_or_else(|| CellError::host("the computer named no owner"))?;
                if !is_e2e(env, owner).await? {
                    return Err(CellError::new(ErrorCode::Forbidden, "on a preview, the computer levers reach e2e people's computers alone"));
                }
            }
            json_answer(&crate::computer::ask(env, &t.computer, "computer/test", &serde_json::json!({ "op": t.op, "times": t.times, "on": t.on })).await?)
        }
        // the registry is the whole deployment's: a preview's is everyone's
        (Method::Post, ["registry"]) if cfg.levers_scoped => Err(no_route("/api/test/registry")),
        (Method::Post, ["registry"]) => {
            let hook: calls::TestHook = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            // the e2e people's own levers are routes of their own
            if matches!(hook, calls::TestHook::E2eSignIn(_) | calls::TestHook::E2ePeople(_) | calls::TestHook::E2eIs(_)) {
                return Err(CellError::invalid("sign e2e people in at /api/test/signin, and list them at /api/test/people"));
            }
            json_answer(&ask_registry(env, &hook).await?)
        }
        (m, _) => Err(CellError::new(fragment_proto::ErrorCode::NotFound, format!("no lever {} /api/test/{}", m.as_ref(), rest.join("/")))),
    }
}

/// An e2e person's session: the registry signs them in (making them the
/// first time), then their ledger makes them a seat with `paid_calls` paid
/// calls at most. Again for the same email: the same person, a new
/// session, and the cap set anew.
async fn sign_in(env: &Env, asked: SignIn) -> CellResult<SignedIn> {
    if !levers::valid_e2e_email(&asked.email) {
        return Err(CellError::invalid(format!(
            "an e2e person's email is <name>@{} (a name of 1-{} lower-case letters, digits, `.`, `_` and `-`)",
            levers::E2E_EMAIL_DOMAIN,
            levers::E2E_EMAIL_NAME_BYTES_MAX
        )));
    }
    if asked.paid_calls > levers::E2E_PAID_CALLS_MAX {
        return Err(CellError::invalid(format!("an e2e person makes at most {} paid calls", levers::E2E_PAID_CALLS_MAX)));
    }
    let answer = ask_registry(env, &calls::TestHook::E2eSignIn(calls::E2eSignIn { email: asked.email, paid_calls: asked.paid_calls })).await?;
    let signed: calls::E2eSignedIn = serde_json::from_value(answer).map_err(|e| CellError::host(format!("the registry's e2e sign-in: {e}")))?;
    assert!(npub::is_identity(&signed.identity), "the registry answers an identity");
    ledger::ask(env, &signed.identity, &SetPlan { id: E2E_SEAT_ID.into(), plan: Plan::Seat }).await?;
    let cap = ledger::ask(env, &signed.identity, &ledger::TestHook::PaidCalls { max: asked.paid_calls }).await?;
    let paid_calls = cap["max"].as_u64().ok_or_else(|| CellError::host(format!("the ledger's paid-calls cap: {cap}")))?;
    assert_eq!(paid_calls, asked.paid_calls, "the ledger holds the cap it was given");
    Ok(SignedIn { session: signed.token, identity: signed.identity, created: signed.created, paid_calls })
}

/// Whether the registry knows `identity` as an e2e person.
async fn is_e2e(env: &Env, identity: &str) -> CellResult<bool> {
    let answer = ask_registry(env, &calls::TestHook::E2eIs(identity.to_string())).await?;
    answer["e2e"].as_bool().ok_or_else(|| CellError::host(format!("the registry's answer to who is an e2e person: {answer}")))
}

/// The 404 every lever answers a request without the fleet's secret: the
/// router's own for a route it does not have (lib.rs, its last arm), word
/// for word.
pub fn no_route(path: &str) -> CellError {
    CellError::new(fragment_proto::ErrorCode::NotFound, format!("no route {path}"))
}
