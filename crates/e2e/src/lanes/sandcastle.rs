//! A sandcastle node asks for a computer's credentials (docs/sandbox.md,
//! Credentials): signed with its own key, which the fleet lists
//! (`FRAGMENT_SANDCASTLE_NODES`), naming the key that owns the computer.
//! The answer is the computer's own token, for the platform's host only;
//! the computer's model calls carry it (through its node's swap) to
//! `/api/model/chat/completions`, where they are reserved and settled on
//! the owner's month like every other call. Anyone else, an unlisted
//! node, and an owner key that is no one's or was revoked get nothing, and
//! a revoked owner's token stops working at once.

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{now_s, Api, Call, Reply};
use crate::Suite;

const ROUTE: &str = "/api/sandcastle/credentials";
const MODEL: &str = "/api/model/chat/completions";

fn ask(owner: &Keys, computer: &str, id: &str) -> Value {
    json!({ "computer": computer, "id": id, "node": "e2e", "owner": owner.pubkey_hex() })
}

/// A model call with a computer's token as its bearer, as the node's swap sends it.
fn call_model(api: &Api, token: &str, body: Value) -> Result<Reply> {
    api.call(Call {
        method: "POST",
        url: format!("{}{MODEL}", api.base),
        body: Some(body.to_string().into_bytes()),
        content_type: Some("application/json"),
        extra: vec![("authorization", format!("Bearer {token}"))],
        ..Call::default()
    })
}

fn hi(text: &str) -> Value {
    json!({ "model": "z-ai/glm-5.3-flash", "messages": [{ "role": "user", "content": text }] })
}

fn token_of(r: &Reply) -> String {
    r.body["credentials"][0]["value"].as_str().unwrap_or("").to_string()
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
    let hermes = ask(&owner, "hermes", "0123456789abcdef");

    let r = api.unsigned("POST", ROUTE, Some(&hermes))?;
    s.ok("an unsigned ask is refused", r.status == 401, &r);
    let r = api.signed(&Keys::generate(), "POST", ROUTE, Some(&hermes))?;
    s.ok("a node the fleet does not list is refused", r.status == 403, &r);
    let r = api.signed(&owner, "POST", ROUTE, Some(&hermes))?;
    s.ok("so is the owner itself: only a node asks", r.status == 403, &r);

    let r = api.signed(&node, "POST", ROUTE, Some(&hermes))?;
    let cred = r.body["credentials"][0].clone();
    let token = token_of(&r);
    let platform_host = reqwest::Url::parse(&api.base)?.host_str().unwrap_or("").to_string();
    s.ok(
        "a listed node gets the computer's own token, for the platform's host only, as OPENAI_API_KEY",
        r.status == 200
            && r.body["credentials"].as_array().map(Vec::len) == Some(1)
            && cred["name"] == "OPENAI_API_KEY"
            && cred["hosts"] == json!([platform_host])
            && token.starts_with("fsc1_")
            && cred["placeholder"].as_str().is_some_and(|p| p.len() >= 8 && p != token),
        &r,
    );
    s.ok("never an OpenRouter key: none is minted for it", minted_for(s).is_empty(), format!("{:?}", minted_for(s)));
    let again = api.signed(&node, "POST", ROUTE, Some(&hermes))?;
    s.ok("asking again answers the same token", token_of(&again) == token, &again);
    let other = api.signed(&node, "POST", ROUTE, Some(&ask(&owner, "other", "fedcba9876543210")))?;
    let other_token = token_of(&other);
    s.ok("another computer gets another", other.status == 200 && !other_token.is_empty() && other_token != token, &other);

    // the model, through the platform, on the owner's month
    let month = || api.signed(&owner, "GET", "/api/budget", None).map(|r| r.body).unwrap_or(Value::Null);
    let row_of = |m: &Value, fragment: &str| m["usage"].as_array().and_then(|u| u.iter().find(|u| u["fragment"] == fragment)).cloned().unwrap_or_default();
    let before = month()["spentMicros"].as_i64().unwrap_or(-1);
    let r = call_model(api, &token, hi("from a sandcastle computer"))?;
    let now = month();
    let row = row_of(&now, "sandcastle:e2e/hermes");
    s.ok(
        "its token calls the model, paid from the owner's month and settled, naming the computer",
        r.status == 200
            && r.body["choices"][0]["message"]["content"] == "echo: from a sandcastle computer"
            && now["spentMicros"].as_i64().unwrap_or(0) > before
            && row["kind"] == "computer.text"
            && row["state"] == "settled",
        format!("{r} {now}"),
    );
    let used = s.openrouter.calls().last().map(|c| c.3.clone()).unwrap_or_default();
    let minted = minted_for(s);
    s.ok(
        "on the owner's own OpenRouter key, which the computer never saw",
        minted.len() == 1 && used == format!("Bearer {}", minted[0].key) && minted[0].key != token,
        format!("{used} {minted:?}"),
    );
    let chats = s.openrouter.chats().len();
    let r = call_model(api, &token, json!({ "model": "anthropic/claude-opus-4.1", "messages": [{ "role": "user", "content": "hi" }] }))?;
    s.ok("only the computer models: another is refused before OpenRouter", r.status == 400 && s.openrouter.chats().len() == chats, &r);

    // tokens that are not
    let mut forged = token.clone();
    let last = forged.pop().unwrap_or('0');
    forged.push(if last == '0' { '1' } else { '0' });
    let r = call_model(api, &forged, hi("forged"))?;
    s.ok("a token one digit off is refused", r.status == 401, &r);
    let r = call_model(api, "sk-or-v1-not-a-computer-token", hi("shape"))?;
    s.ok("and one that is not a token's shape", r.status == 401, &r);
    let secret = token.rsplit('_').next().unwrap_or("");
    let elsewhere = format!("fsc1_{}_{secret}", "0".repeat(32));
    let r = call_model(api, &elsewhere, hi("elsewhere"))?;
    s.ok("and a token's secret under another org", r.status == 401, &r);
    let r = api.unsigned("POST", MODEL, Some(&hi("no one")))?;
    s.ok("and a call with no token or signature", r.status == 401, &r);

    // an agent's computer: its own token, its owner pays
    let agent = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &owner) })))?;
    anyhow::ensure!(r.status == 200, "registering an agent: {r}");
    let r = api.signed(&node, "POST", ROUTE, Some(&ask(&agent, "agents", "a0a0a0a0a0a0a0a0")))?;
    let agents = token_of(&r);
    let called = call_model(api, &agents, hi("from an agent's computer"))?;
    let row = row_of(&month(), "sandcastle:e2e/agents");
    s.ok("an agent's computer gets its own token, and its owner pays", r.status == 200 && agents != token && called.status == 200 && row["state"] == "settled", format!("{r} {called}"));

    // owners that are no one's, now or any more
    let stranger = Keys::generate();
    let r = api.signed(&node, "POST", ROUTE, Some(&ask(&stranger, "x", "1111111111111111")))?;
    s.ok("an owner key that is no one's gets nothing", r.status == 403 && r.body["credentials"].is_null(), &r);
    let second = Keys::generate();
    api.approve(&session, &second)?;
    let second_ask = ask(&second, "second", "2222222222222222");
    let seconds = token_of(&api.signed(&node, "POST", ROUTE, Some(&second_ask))?);
    let worked = call_model(api, &seconds, hi("before"))?;
    let r = api.signed(&owner, "DELETE", &format!("/api/identities/me/keys/{}", second.pubkey_hex()), None)?;
    anyhow::ensure!(r.status == 200, "revoking a key: {r}");
    let after = call_model(api, &seconds, hi("after"))?;
    let asked = api.signed(&node, "POST", ROUTE, Some(&second_ask))?;
    s.ok(
        "a revoked owner key's token stops at once, and its node gets nothing more",
        worked.status == 200 && after.status == 403 && asked.status == 403 && asked.body["credentials"].is_null(),
        format!("{worked} {after} {asked}"),
    );

    // the signature covers what the node said
    let url = format!("{}{ROUTE}", api.base);
    let said = ask(&stranger, "x", "1111111111111111").to_string().into_bytes();
    let r = api.call(Call {
        method: "POST",
        url: url.clone(),
        body: Some(hermes.to_string().into_bytes()),
        content_type: Some("application/json"),
        extra: vec![("authorization", node.header("POST", &url, &said, now_s()))],
        ..Call::default()
    })?;
    s.ok("a body the node did not sign is refused", r.status == 401, &r);
    let mut bad = hermes.clone();
    bad["owner"] = json!(owner_id);
    let r = api.signed(&node, "POST", ROUTE, Some(&bad))?;
    s.ok("the owner is a key, not an identity", r.status == 400, &r);
    let mut extra = hermes.clone();
    extra["hosts"] = json!(["evil.example"]);
    let r = api.signed(&node, "POST", ROUTE, Some(&extra))?;
    s.ok("a node does not choose the hosts", r.status == 400, &r);

    // a token its node stops asking for lapses (a deleted computer's); asking again revives it
    let r = api.signed(&owner, "POST", "/api/test/ledger", Some(&json!({ "identity": owner_id, "offsetMs": 25 * 3600 * 1000 })))?;
    anyhow::ensure!(r.status == 200, "moving the ledger's clock: {r}");
    let lapsed = call_model(api, &token, hi("a day later"))?;
    api.signed(&node, "POST", ROUTE, Some(&hermes))?;
    let revived = call_model(api, &token, hi("asked again"))?;
    s.ok("a token its node has not asked for in a day lapses, and its next ask revives it", lapsed.status == 401 && revived.status == 200, format!("{lapsed} {revived}"));
    Ok(())
}
