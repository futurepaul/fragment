//! A sandcastle node asks for a computer's credentials (docs/sandbox.md,
//! Credentials): signed with its own key, which the fleet lists
//! (`FRAGMENT_SANDCASTLE_NODES`), naming the key that owns the computer.
//! The answer is that person's own OpenRouter key from their Ledger (an
//! agent's owner's), for openrouter.ai only; anyone else, an unlisted
//! node, and an owner key that is no one's or was revoked get nothing.

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{now_s, Api, Call};
use crate::Suite;

const ROUTE: &str = "/api/sandcastle/credentials";

fn ask(owner: &Keys) -> Value {
    json!({ "computer": "hermes", "id": "0123456789abcdef", "node": "e2e", "owner": owner.pubkey_hex() })
}

pub fn sandcastle(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("sandcastle") {
        return Ok(());
    }
    let node = Keys::from_secret_hex(&s.sandcastle_node.secret_hex()).expect("a key's own secret reads back");
    let session = api.sign_in("computer-owner@e2e.test")?;
    let owner = Keys::generate();
    let owner_id = api.approve(&session, &owner)?.body["id"].as_str().unwrap_or("").to_string();
    let org = format!("org:{}", owner_id.trim_start_matches("id:"));
    let minted_for = |s: &Suite| s.openrouter.minted().into_iter().filter(|k| k.name.contains(&org)).collect::<Vec<_>>();

    let r = api.unsigned("POST", ROUTE, Some(&ask(&owner)))?;
    s.ok("an unsigned ask is refused", r.status == 401, &r);
    let r = api.signed(&Keys::generate(), "POST", ROUTE, Some(&ask(&owner)))?;
    s.ok("a node the fleet does not list is refused", r.status == 403, &r);
    let r = api.signed(&owner, "POST", ROUTE, Some(&ask(&owner)))?;
    s.ok("so is the owner itself: only a node asks", r.status == 403 && minted_for(s).is_empty(), &r);

    let r = api.signed(&node, "POST", ROUTE, Some(&ask(&owner)))?;
    let minted = minted_for(s);
    let cred = &r.body["credentials"][0];
    s.ok(
        "a listed node gets the owner's own OpenRouter key, for openrouter.ai only",
        r.status == 200
            && r.body["credentials"].as_array().map(Vec::len) == Some(1)
            && cred["name"] == "OPENROUTER_API_KEY"
            && cred["hosts"] == json!(["openrouter.ai"])
            && minted.len() == 1
            && cred["value"] == minted[0].key.as_str(),
        &r,
    );
    s.ok("minted with the owner's allowance as its monthly limit", minted.first().is_some_and(|k| k.limit_reset.as_deref() == Some("monthly")), format!("{minted:?}"));
    let again = api.signed(&node, "POST", ROUTE, Some(&ask(&owner)))?;
    s.ok("asking again answers the same key, minted once", again.body["credentials"][0]["value"] == cred["value"] && minted_for(s).len() == 1, &again);

    // an agent that owns a computer: its owner's key
    let agent = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &owner) })))?;
    anyhow::ensure!(r.status == 200, "registering an agent: {r}");
    let r = api.signed(&node, "POST", ROUTE, Some(&ask(&agent)))?;
    s.ok("an agent's computer gets its owner's key", r.status == 200 && r.body["credentials"][0]["value"] == cred["value"], &r);

    // owners that are no one's
    let stranger = Keys::generate();
    let r = api.signed(&node, "POST", ROUTE, Some(&ask(&stranger)))?;
    s.ok("an owner key that is no one's gets nothing", r.status == 403 && r.body["credentials"].is_null(), &r);
    let second = Keys::generate();
    api.approve(&session, &second)?;
    let r = api.signed(&owner, "DELETE", &format!("/api/identities/me/keys/{}", second.pubkey_hex()), None)?;
    anyhow::ensure!(r.status == 200, "revoking a key: {r}");
    let r = api.signed(&node, "POST", ROUTE, Some(&ask(&second)))?;
    s.ok("nor does a revoked one", r.status == 403 && r.body["credentials"].is_null(), &r);

    // the signature covers what the node said
    let url = format!("{}{ROUTE}", api.base);
    let said = ask(&stranger).to_string().into_bytes();
    let r = api.call(Call {
        method: "POST",
        url: url.clone(),
        body: Some(ask(&owner).to_string().into_bytes()),
        content_type: Some("application/json"),
        extra: vec![("authorization", node.header("POST", &url, &said, now_s()))],
        ..Call::default()
    })?;
    s.ok("a body the node did not sign is refused", r.status == 401, &r);
    let mut bad = ask(&owner);
    bad["owner"] = json!(owner_id);
    let r = api.signed(&node, "POST", ROUTE, Some(&bad))?;
    s.ok("the owner is a key, not an identity", r.status == 400, &r);
    let mut extra = ask(&owner);
    extra["hosts"] = json!(["evil.example"]);
    let r = api.signed(&node, "POST", ROUTE, Some(&extra))?;
    s.ok("a node does not choose the hosts", r.status == 400, &r);
    Ok(())
}
