//! The computer's swap (Paul, 2026-10-04; docs/computers.md, "Connections
//! and operator keys"), as the computers lane drives it on the stub: its
//! scripted agent reads what its guest is given (`credentials`) and sends
//! requests as any SDK would (`fetch … with $NAME`), with no header of
//! ours, to the catalog's real hosts, which the swap sends to the upstream
//! fake. Each placement (a header, a query parameter, basic auth), each
//! kind (a connection through the WorkOS fake, the operator's keys, an own
//! key), and each refusal (a forged tag, another computer's, the wrong
//! host, the wrong place), then the holds and the counts, and a key's call
//! its owner's ledger will not hold.

use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use serde_json::{json, Value};

use super::ledger::{end_of, entries};
use crate::api::Api;
use crate::{Suite, SWAP_CONNECTION, SWAP_CONNECTION_ENV, SWAP_CONNECTION_HOST, SWAP_KEYS, SWAP_OWN, SWAP_OWN_ENV, SWAP_OWN_HOST};

/// Who the swap's checks act as, and on what.
pub(super) struct Swapping<'a> {
    pub owner: &'a crate::Keys,
    pub stranger: &'a crate::Keys,
    pub id: &'a str,
    pub agent: &'a str,
    pub identity: &'a str,
}

/// The checks `swap_checks` makes, as a run without the stub or the fakes
/// skips them.
pub(super) const SWAP_CHECKS: &[&str] = &[
    "the guest is given each operator key the deployment offers, each in its environment variable, a placeholder naming the agent",
    "and no connection its owner has not connected: its environment variable holds nothing",
    "a placeholder made for that connection is refused, saying so, and reaches no provider",
    "connected, its owner's read of their connections says so, and its guest is given it at its next read, no token minted to tell",
    "a header: the connection's placeholder from the env, sent as a bearer token, reaches its host as the owner's token from Pipes",
    "and never a header of ours",
    "a token is held until shortly before it expires; a provider's redirect is the guest's to follow, never followed with it",
    "a header: an operator key's placeholder as a bearer token reaches its host as the key",
    "a header of the provider's own (xi-api-key) takes its key",
    "a query parameter: the key in place of the placeholder, the rest of the query as it came",
    "a placeholder in a place its provider does not take its key is refused, saying where it goes",
    "a placeholder sent to a host that is not its provider's is refused, and reaches no provider",
    "a forged tag names no agent: refused, reaching no provider",
    "another computer's agent's tag is refused here",
    "an own key: its owner gives it, sealed by the computer, and it is the guest's at once",
    "basic auth: the own key in its password's place, its user as it came",
    "a key that is no printable token is refused; taken away, the guest is given it no more",
    "each operator key's call is held on the agent's owner's ledger, as the agent, and settled at its list price and the margin",
    "this month's uses: each call by agent, a connection's and an own key's counted, never charged, an operator key's at its charge",
    "no one else reads them, and a month is YYYY-MM",
    "a request with no placeholder goes on as it came",
    "an operator key's call its owner's ledger will not hold is refused, and reaches no provider",
];

/// What one call of each key is charged: its list price (fragment_core::
/// price DEFAULT_KEYS) and the 50% margin.
fn charge_of(key: &str) -> i64 {
    let (micros, per) = fragment_core::price::default_key_price(key).expect("each operator key has a list price");
    // ceil(micros / per × 1.5), as the price book rounds a row
    let (n, d) = (micros * 3, 2 * per as i64);
    (n + d - 1) / d
}

/// The placeholder the platform gives `agent` on computer `id` for
/// `provider`: a local run holds the node's host secret, so it makes one
/// as the platform does (a forgery needs the secret; a guest has none).
pub(super) fn placeholder(s: &Suite, id: &str, agent: &str, provider: &str) -> String {
    let kind = if provider == SWAP_CONNECTION { fragment_core::catalog::Kind::Connection } else { fragment_core::catalog::Kind::Operator };
    let tag = fragment_core::swap::TagKey::derive(&s.host_secret).tag(id, agent, provider);
    fragment_core::swap::Placeholder::new(kind, provider, &tag).text()
}

/// The upstream fake's last request, after `before` of them.
fn last_seen(s: &Suite, before: usize) -> Value {
    s.upstream.seen().get(before..).and_then(|l| l.last().cloned()).unwrap_or(Value::Null)
}

/// The checks; answers how many turns of the agent's they took (each a
/// reply in the chat).
pub(super) fn swap_checks(s: &mut Suite, api: &Api, w: &Swapping, fetched: &dyn Fn(&Suite, u32, &str) -> Result<String>) -> Result<usize> {
    let mut turns = 0;
    let mut say = |s: &Suite, n: u32, text: &str| -> Result<String> {
        turns += 1;
        fetched(s, n, text)
    };
    let owner_id = api.identity(w.owner)?;
    let key_value = |name: &str| SWAP_KEYS.iter().find(|(n, _, _)| *n == name).map(|(_, v, _)| v.to_string()).unwrap_or_default();

    // what the guest is given, before its owner connects anything
    let given = say(s, 101, "credentials")?;
    let every_key = SWAP_KEYS.iter().all(|(name, _, env)| given.contains(&format!("{name} {env} {}", placeholder(s, w.id, w.agent, name))));
    s.ok(SWAP_CHECKS[0], given.starts_with("credentials: ") && every_key, &given);
    s.ok(SWAP_CHECKS[1], !given.contains(SWAP_CONNECTION_ENV) && !given.contains(SWAP_OWN_ENV), &given);
    let seen_before = s.upstream.seen().len();
    let said = say(s, 102, &format!("fetch http://{SWAP_CONNECTION_HOST}/drive/v3/files with ${SWAP_CONNECTION_ENV}"))?;
    let made = say(s, 103, &format!("fetch http://{SWAP_CONNECTION_HOST}/drive/v3/files with {}", placeholder(s, w.id, w.agent, SWAP_CONNECTION)))?;
    s.ok(
        SWAP_CHECKS[2],
        said.contains("fetch failed: no credential") && made.starts_with("fetched 403") && made.contains("not_connected") && made.contains("connect google first") && s.upstream.seen().len() == seen_before,
        json!({ "env": said, "made": made }),
    );

    // connected through Pipes: the owner's read tells the computer at once
    let email = Api::email_of(w.owner);
    s.workos.connect(&email, SWAP_CONNECTION, true);
    let minted_before = s.workos.tokens(SWAP_CONNECTION).len();
    let read_before = s.workos.states_read();
    let r = api.signed(w.owner, "GET", "/api/connections", None)?;
    let state_of = |r: &crate::api::Reply, p: &str| r.body["providers"].as_array().and_then(|l| l.iter().find(|x| x["provider"] == p)).map(|x| x["state"].clone()).unwrap_or(Value::Null);
    let given = say(s, 104, "credentials")?;
    s.ok(
        SWAP_CHECKS[3],
        state_of(&r, SWAP_CONNECTION) == "connected"
            && given.contains(&format!("{SWAP_CONNECTION} {SWAP_CONNECTION_ENV} {}", placeholder(s, w.id, w.agent, SWAP_CONNECTION)))
            && s.workos.tokens(SWAP_CONNECTION).len() == minted_before
            && s.workos.states_read() > read_before,
        json!({ "connections": r.body, "given": given }),
    );

    // a header: the connection's bearer token
    let said = say(s, 105, &format!("fetch http://{SWAP_CONNECTION_HOST}/drive/v3/files with ${SWAP_CONNECTION_ENV}"))?;
    let seen = s.upstream.seen().last().cloned().unwrap_or_default();
    let tokens = s.workos.tokens(SWAP_CONNECTION).split_off(minted_before);
    s.ok(
        SWAP_CHECKS[4],
        said.starts_with("fetched 200") && tokens.len() == 1 && seen["host"] == SWAP_CONNECTION_HOST && seen["auth"]["authorization"] == format!("Bearer {}", tokens[0]),
        json!({ "said": said, "seen": seen, "tokens": tokens }),
    );
    s.ok(SWAP_CHECKS[5], seen["agent"].is_null(), &seen);
    let said = say(s, 106, &format!("fetch http://{SWAP_CONNECTION_HOST}/redirect with ${SWAP_CONNECTION_ENV}"))?;
    s.ok(SWAP_CHECKS[6], said.starts_with("fetched 302") && s.workos.tokens(SWAP_CONNECTION).len() == minted_before + 1, &said);

    // the operator's keys: a bearer token, a header of the provider's own, a query parameter
    let said = say(s, 107, "fetch http://api.perplexity.ai/search with $PERPLEXITY_API_KEY")?;
    let seen = s.upstream.seen().last().cloned().unwrap_or_default();
    s.ok(SWAP_CHECKS[7], said.starts_with("fetched 200") && seen["host"] == "api.perplexity.ai" && seen["auth"]["authorization"] == format!("Bearer {}", key_value("perplexity")), json!({ "said": said, "seen": seen }));
    let said = say(s, 108, "fetch http://api.elevenlabs.io/v1/music with $ELEVENLABS_API_KEY in xi-api-key")?;
    let seen = s.upstream.seen().last().cloned().unwrap_or_default();
    s.ok(SWAP_CHECKS[8], said.starts_with("fetched 200") && seen["host"] == "api.elevenlabs.io" && seen["auth"]["xi-api-key"] == key_value("elevenlabs"), json!({ "said": said, "seen": seen }));
    let said = say(s, 109, "fetch http://places.googleapis.com/v1/places:searchText?fields=name with $GOOGLE_PLACES_API_KEY in query key")?;
    let seen = s.upstream.seen().last().cloned().unwrap_or_default();
    s.ok(
        SWAP_CHECKS[9],
        said.starts_with("fetched 200") && seen["host"] == "places.googleapis.com" && seen["query"] == json!([["fields", "name"], ["key", key_value("google-places")]]),
        json!({ "said": said, "seen": seen }),
    );

    // refusals: the wrong place, the wrong host, a forged tag, another computer's
    let seen_before = s.upstream.seen().len();
    let place = say(s, 110, "fetch http://api.perplexity.ai/search with $PERPLEXITY_API_KEY in x-api-key")?;
    s.ok(SWAP_CHECKS[10], place.starts_with("fetched 400") && place.contains("goes in authorization: Bearer {}") && s.upstream.seen().len() == seen_before, &place);
    let host = say(s, 111, &format!("fetch http://{SWAP_CONNECTION_HOST}/x with $PERPLEXITY_API_KEY"))?;
    s.ok(SWAP_CHECKS[11], host.starts_with("fetched 403") && host.contains("is for api.perplexity.ai, not www.googleapis.com") && s.upstream.seen().len() == seen_before, &host);
    let forged = say(s, 112, &format!("fetch http://api.perplexity.ai/search with fck_perplexity_{}", "0".repeat(fragment_core::swap::TAG_HEX)))?;
    s.ok(SWAP_CHECKS[12], forged.starts_with("fetched 403") && forged.contains("names no agent on this computer") && s.upstream.seen().len() == seen_before, &forged);
    let elsewhere = fragment_core::computer::default_computer_of(&api.identity(w.stranger)?);
    let other = say(s, 113, &format!("fetch http://api.perplexity.ai/search with {}", placeholder(s, &elsewhere, w.agent, "perplexity")))?;
    s.ok(SWAP_CHECKS[13], other.starts_with("fetched 403") && other.contains("names no agent on this computer") && s.upstream.seen().len() == seen_before, &other);

    // an own key, behind basic auth
    let own = "mail-own-e2e-55aa";
    let r = api.signed(w.owner, "PUT", &format!("/api/connections/{SWAP_OWN}/key"), Some(&json!({ "key": own })))?;
    let listed = api.signed(w.owner, "GET", "/api/connections", None)?;
    let given = say(s, 114, "credentials")?;
    s.ok(SWAP_CHECKS[14], r.status == 200 && state_of(&listed, SWAP_OWN) == "set" && given.contains(&format!("{SWAP_OWN} {SWAP_OWN_ENV} fck_{SWAP_OWN}_")), json!({ "put": r.body, "given": given }));
    let said = say(s, 115, &format!("fetch http://{SWAP_OWN_HOST}/v3/send with ${SWAP_OWN_ENV} as basic api"))?;
    let seen = last_seen(s, seen_before);
    let basic = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("api:{own}")));
    s.ok(SWAP_CHECKS[15], said.starts_with("fetched 200") && seen["host"] == SWAP_OWN_HOST && seen["auth"]["authorization"] == basic, json!({ "said": said, "seen": seen }));
    let bad = api.signed(w.owner, "PUT", &format!("/api/connections/{SWAP_OWN}/key"), Some(&json!({ "key": "two words" })))?;
    let not_own = api.signed(w.owner, "PUT", "/api/connections/perplexity/key", Some(&json!({ "key": "pplx-mine" })))?;
    let gone = api.signed(w.owner, "DELETE", &format!("/api/connections/{SWAP_OWN}/key"), None)?;
    let listed = api.signed(w.owner, "GET", "/api/connections", None)?;
    let given = say(s, 116, "credentials")?;
    s.ok(
        SWAP_CHECKS[16],
        bad.status == 400 && not_own.status == 400 && gone.status == 200 && state_of(&listed, SWAP_OWN) == "not_set" && !given.contains(SWAP_OWN_ENV),
        json!({ "bad": bad.status, "notOwn": not_own.status, "gone": gone.status, "given": given }),
    );

    // the holds: each operator key's call, settled at its price; and the month's counts
    let keyed = ["perplexity", "elevenlabs", "google-places"];
    let held = || entries(api, &owner_id, &format!("key:{}:", w.id));
    let metered = s.eventually(Duration::from_secs(20), || held().iter().filter(|e| end_of(e) == "settled").count() == keyed.len());
    let rows = held();
    let priced = rows.len() == keyed.len()
        && keyed.iter().all(|k| {
            rows.iter().any(|r| r["entry"]["reserve"]["worst"] == json!({ "kind": "key", "key": k, "units": 1 }) && r["entry"]["reserve"]["agent"] == w.identity && r["entry"]["end"]["charge"] == charge_of(k))
        });
    s.ok(SWAP_CHECKS[17], metered && priced, json!(rows));
    let expect = [(SWAP_CONNECTION, 2, 0), ("perplexity", 1, charge_of("perplexity")), ("elevenlabs", 1, charge_of("elevenlabs")), ("google-places", 1, charge_of("google-places")), (SWAP_OWN, 1, 0)];
    let uses = || api.signed(w.owner, "GET", &format!("/api/computers/{}/uses", w.id), None).map(|r| r.body).unwrap_or(Value::Null);
    let counted = |u: &Value| expect.iter().all(|(p, calls, micros)| u["uses"].as_array().is_some_and(|l| l.iter().any(|x| x["provider"] == *p && x["agent"] == w.agent && x["calls"] == *calls && x["micros"] == *micros)));
    let all = s.eventually(Duration::from_secs(20), || counted(&uses()));
    let u = uses();
    let month = fragment_core::ledger::Month::of(crate::api::now_s() * 1000).label();
    s.ok(SWAP_CHECKS[18], all && u["month"] == month.as_str() && u["computer"] == w.id, &u);
    let theirs = api.signed(w.stranger, "GET", &format!("/api/computers/{}/uses", w.id), None)?;
    let bad_month = api.signed(w.owner, "GET", &format!("/api/computers/{}/uses/2026-13", w.id), None)?;
    let this = api.signed(w.owner, "GET", &format!("/api/computers/{}/uses/{month}", w.id), None)?;
    s.ok(SWAP_CHECKS[19], theirs.status == 404 && bad_month.status == 400 && this.status == 200 && this.body == u, json!({ "stranger": theirs.status, "badMonth": bad_month.status, "month": this.body }));

    let said = say(s, 117, "fetch http://api.perplexity.ai/plain with nothing-swapped")?;
    let seen = s.upstream.seen().last().cloned().unwrap_or_default();
    s.ok(SWAP_CHECKS[20], said.starts_with("fetched 200") && seen["auth"]["authorization"] == "Bearer nothing-swapped", &seen);

    // a key is the operator's money: with the owner's paid calls capped at
    // none (a refusal that is no typed one, as a ledger that does not answer
    // gives), the call is not made
    let cap = |max: u64| api.unsigned("POST", "/api/test/ledger", Some(&json!({ "identity": owner_id, "op": "paid-calls", "max": max })));
    let capped = cap(0)?;
    let seen_before = s.upstream.seen().len();
    let refused = say(s, 118, "fetch http://api.perplexity.ai/search with $PERPLEXITY_API_KEY")?;
    let uncapped = cap(u64::from(u32::MAX))?;
    s.ok(
        SWAP_CHECKS[21],
        capped.status == 200 && uncapped.status == 200 && refused.starts_with("fetched 402") && refused.contains("perplexity's call is not made") && s.upstream.seen().len() == seen_before && held().len() == keyed.len(),
        json!({ "refused": refused, "capped": capped.body }),
    );
    Ok(turns)
}
