//! Identities (phase 4 slice A; docs/finite-integration.md): every signed
//! request resolves to an identity in the registry, live; grants name
//! identities, so a key replaced with a proof keeps every grant; a revoked
//! key is refused from the next request and never comes back; an agent's
//! owner reads what it reads and never acts through it; and a registry that
//! cannot answer is a visible 503, never an allow.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_core::npub;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use crate::api::{now_s, Api};
use crate::Suite;

const CHAT_APP: &[u8] = include_bytes!("../../fixtures/chat.mjs");
const CHAT_JSON: &[u8] = include_bytes!("../../fixtures/chat.json");
const FORMAT_MJS: &[u8] = include_bytes!("../../fixtures/format.mjs");

fn keys_of(v: &Value) -> Vec<(String, bool)> {
    v["keys"].as_array().into_iter().flatten().map(|k| (k["npub"].as_str().unwrap_or("").to_string(), k["revokedAt"].is_null())).collect()
}

fn median_ms(mut f: impl FnMut() -> Result<u16>, n: usize) -> Result<f64> {
    let mut times = vec![];
    for _ in 0..n {
        let t0 = Instant::now();
        let status = f()?;
        anyhow::ensure!(status == 200, "a timed request answered {status}");
        times.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.total_cmp(b));
    Ok(times[n / 2])
}

pub fn identities(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("identities") {
        return Ok(());
    }

    // people come from sign-in; a key joins the person who approves it
    let stray = Keys::generate();
    let r = api.signed(&stray, "GET", "/api/fragments", None)?;
    s.ok("a key no one approved is 401, saying how to add it", r.status == 401 && r.message().contains("fragment login"), &r);
    let r = api.signed(&stray, "POST", "/api/identities", Some(&json!({ "kind": "person" })))?;
    s.ok("a key does not make a person: people sign in", r.status == 400 && r.message().contains("sign in"), &r);
    let session = api.sign_in("stray@e2e.test")?;
    let r = api.approve(&session, &stray)?;
    let stray_id = r.body["id"].as_str().unwrap_or("").to_string();
    s.ok("approved in a signed-in browser, the key is that person's", r.status == 200 && npub::is_identity(&stray_id), &r);
    let r = api.signed(&stray, "GET", "/api/identities/me", None)?;
    s.ok(
        "the person is keyed by their sign-in, and the email is shown beside it",
        r.status == 200 && r.body["id"] == stray_id.as_str() && r.body["kind"] == "person" && r.body["subjects"][0]["email"] == "stray@e2e.test",
        &r,
    );
    let r = api.signed(&stray, "POST", "/api/identities", Some(&json!({ "kind": "robot" })))?;
    s.ok("an unknown kind is 400", r.status == 400, &r);

    // a replaced key keeps every grant
    let paul = api.person()?;
    let paul_id = api.identity(&paul)?;
    let friend = api.person()?;
    let mine = s.name("mine");
    s.create(api, &paul, &mine)?;
    let theirs = s.name("theirs");
    s.create(api, &friend, &theirs)?;
    api.signed(&friend, "PUT", &format!("/api/f/{theirs}/members/{}", npub::encode(paul.pubkey_hex())), Some(&json!({ "role": "editor" })))?;
    let before = api.signed(&friend, "GET", &format!("/api/f/{theirs}/members"), None)?;
    s.ok("a key names the identity holding it as a member", before.body["members"].as_array().is_some_and(|m| m.iter().any(|m| m["principal"] == paul_id.as_str())), &before);

    let new = Keys::generate();
    let add = "/api/identities/me/keys";
    let r = api.signed(&paul, "POST", add, Some(&json!({})))?;
    s.ok("adding a key needs a proof by it", r.status == 400, &r);
    let r = api.signed(&paul, "POST", add, Some(&json!({ "proof": api.proof(&new, "POST", add, &friend) })))?;
    s.ok("a proof meant for someone else is refused", r.status == 400 && r.message().contains("proof"), &r);
    let r = api.signed(&paul, "POST", add, Some(&json!({ "proof": api.proof(&new, "POST", "/api/identities/me/other", &paul) })))?;
    s.ok("a proof for another request is refused", r.status == 400, &r);
    let stale = new.proof("POST", &format!("{}{add}", api.base), paul.pubkey_hex(), now_s() - 120);
    let r = api.signed(&paul, "POST", add, Some(&json!({ "proof": stale })))?;
    s.ok("a stale proof is refused", r.status == 400, &r);
    let r = api.signed(&paul, "POST", add, Some(&json!({ "proof": api.proof(&paul, "POST", add, &paul) })))?;
    s.ok("a key cannot vouch for itself", r.status == 400, &r);
    let theirs_keys = format!("/api/identities/{paul_id}/keys");
    let r = api.signed(&friend, "POST", &theirs_keys, Some(&json!({ "proof": api.proof(&new, "POST", &theirs_keys, &friend) })))?;
    s.ok("nobody else adds a key to a person", r.status == 403, &r);
    let r = api.signed(&paul, "POST", add, Some(&json!({ "proof": api.proof(&friend, "POST", add, &paul) })))?;
    s.ok("a key someone else holds cannot be added", r.status == 409, &r);
    let r = api.signed(&paul, "POST", add, Some(&json!({ "proof": api.proof(&new, "POST", add, &paul) })))?;
    s.ok("with a proof by the new key, it is added", r.status == 200 && r.body["created"] == true && keys_of(&r.body).iter().filter(|k| k.1).count() == 2, &r);
    let r = api.signed(&paul, "POST", add, Some(&json!({ "proof": api.proof(&new, "POST", add, &paul) })))?;
    s.ok("adding it again changes nothing", r.status == 200 && r.body["created"] == false, &r);

    let r = api.signed(&new, "GET", "/api/fragments", None)?;
    let listed = |name: &str| r.body["fragments"].as_array().and_then(|a| a.iter().find(|f| f["name"] == name)).map(|f| f["role"].clone());
    s.ok("the new key lists the same fragments, with the same roles", listed(&mine) == Some(json!("owner")) && listed(&theirs) == Some(json!("editor")), &r);
    let r = api.signed(&new, "POST", &format!("/api/f/{mine}/rotate"), Some(&json!({ "scopes": ["view"] })))?;
    s.ok("the new key acts as the owner", r.status == 200, &r);
    let old_hex = paul.pubkey_hex().to_string();
    let r = api.signed(&new, "DELETE", &format!("/api/identities/me/keys/{}", npub::encode(&old_hex)), None)?;
    s.ok("the new key revokes the old one", r.status == 200 && keys_of(&r.body).iter().filter(|k| k.1).count() == 1, &r);
    let after = api.signed(&friend, "GET", &format!("/api/f/{theirs}/members"), None)?;
    s.ok("no grant was rewritten: the members are the same identities", after.body["members"] == before.body["members"], &after);
    let r = api.signed(&paul, "GET", "/api/fragments", None)?;
    s.ok("the revoked key is 401 from the next request", r.status == 401 && r.message().contains("revoked"), &r);
    let session = api.sign_in("paul-revoked@e2e.test")?;
    let r = api.approve_link(&session, &api.approval_link(&paul, 0))?;
    s.ok("a revoked key cannot be approved again, by anyone", r.status == 409, &r);
    let r = api.signed(&new, "POST", add, Some(&json!({ "proof": api.proof(&paul, "POST", add, &new) })))?;
    s.ok("a revoked key stays revoked", r.status == 409, &r);
    let r = api.signed(&new, "DELETE", &format!("/api/identities/me/keys/{old_hex}"), None)?;
    s.ok("revoking again changes nothing", r.status == 200 && r.body["created"] == false, &r);
    let r = api.signed(&friend, "DELETE", &format!("/api/identities/{paul_id}/keys/{}", new.pubkey_hex()), None)?;
    s.ok("nobody else revokes a person's key", r.status == 403, &r);
    let r = api.signed(&friend, "GET", &format!("/api/identities/{paul_id}"), None)?;
    s.ok("nobody else sees a person's keys", r.status == 404, &r);

    // agents: registered by their owner, who reads what they read
    let owner = api.person()?;
    let owner_id = api.identity(&owner)?;
    let agent = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent" })))?;
    s.ok("an agent registers with a proof by its key", r.status == 400, &r);
    let r = api.signed(&Keys::generate(), "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &owner) })))?;
    s.ok("a key no one registered cannot vouch for an agent", r.status == 401, &r);
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &friend) })))?;
    s.ok("a proof meant for another owner is refused", r.status == 400, &r);
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &owner) })))?;
    let agent_id = r.body["id"].as_str().unwrap_or("").to_string();
    s.ok("the owner registers an agent they own", r.status == 200 && r.body["kind"] == "agent" && r.body["owner"] == owner_id.as_str(), &r);
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &owner) })))?;
    s.ok("registering it again answers the same agent", r.status == 200 && r.body["id"] == agent_id.as_str() && r.body["created"] == false, &r);
    let r = api.signed(&friend, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&agent, "POST", reg, &friend) })))?;
    s.ok("nobody else can claim it", r.status == 409, &r);
    let r = api.signed(&agent, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&Keys::generate(), "POST", reg, &agent) })))?;
    s.ok("an agent owns no agents", r.status == 403, &r);
    let agent_keys = format!("/api/identities/{agent_id}/keys");
    let r = api.signed(&agent, "POST", &agent_keys, Some(&json!({ "proof": api.proof(&Keys::generate(), "POST", &agent_keys, &agent) })))?;
    s.ok("an agent does not add keys to itself", r.status == 403, &r);
    let r = api.signed(&owner, "GET", "/api/identities/me", None)?;
    s.ok("the owner's identity lists the agent", r.body["agents"] == json!([agent_id]), &r);
    let r = api.signed(&owner, "DELETE", &format!("/api/identities/{agent_id}/keys/{}", agent.pubkey_hex()), None)?;
    s.ok("an agent's last key cannot be revoked", r.status == 400, &r);

    let room = s.name("room");
    let c = s.create(api, &friend, &room)?;
    s.commit(&c, &[("applib/format.mjs", Some(FORMAT_MJS))]);
    ship(s, &c, CHAT_APP, CHAT_JSON);
    api.signed(&friend, "PUT", &format!("/api/f/{room}/visibility"), Some(&json!({ "visibility": "members" })))?;
    let r = api.signed(&friend, "PUT", &format!("/api/f/{room}/members/{}", npub::encode(agent.pubkey_hex())), Some(&json!({ "role": "editor" })))?;
    s.ok("adding an agent says whose it is", r.status == 200 && r.body["kind"] == "agent" && r.body["owner"] == owner_id.as_str(), &r);
    let r = api.op(&agent, &room, "say", "a1", json!({ "text": "hello from the agent" }))?;
    s.ok("the agent acts as its membership lets it", r.status == 200, &r);
    let r = api.status(&owner, &room)?;
    s.ok("its owner reads the fragment, as a viewer", r.status == 200 && r.body["role"] == "viewer" && r.body["inboxToken"].is_null(), &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{room}/channels/room"), None)?;
    s.ok("and its channels", r.status == 200 && r.body["records"].as_array().is_some_and(|a| a.iter().any(|x| x["principal"] == agent_id.as_str())), &r);
    let r = api.op(&owner, &room, "count", "q1", json!({}))?;
    s.ok("and asks its queries", r.status == 200, &r);
    let r = api.op(&owner, &room, "say", "o1", json!({ "text": "through my agent" }))?;
    s.ok("but never acts through it: a mutation is 403, saying why", r.status == 403 && r.message().contains("your own"), &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{room}/channels/staff"), None)?;
    s.ok("it reads as a viewer, not with the agent's role", r.status == 403, &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{room}/storage-token"), None)?;
    s.ok("and gets no editor capability", r.status == 403, &r);
    let r = api.signed(&owner, "PUT", &format!("/api/f/{room}/members/{}", npub::encode(owner.pubkey_hex())), Some(&json!({ "role": "viewer" })))?;
    s.ok("nor shares it", r.status == 403, &r);
    let r = api.status(&stray, &room)?;
    s.ok("someone who owns no member agent reads nothing", r.status == 403, &r);
    api.signed(&friend, "DELETE", &format!("/api/f/{room}/members/{agent_id}"), None)?;
    let r = api.status(&owner, &room)?;
    s.ok("when the agent leaves, so does its owner's view", r.status == 403, &r);

    // the CLI: login registers, whoami says who, rotate keeps every grant
    let home = s.dir("identities-home");
    let out = s.login(api, &home);
    s.ok("fragment login, approved in a browser, adds the key to the person", out.status.success() && String::from_utf8_lossy(&out.stdout).contains("id:"), String::from_utf8_lossy(&out.stderr));
    let me = s.cli_json(api, &home, &["whoami", "--json"])?;
    let cli_id = me["identity"]["id"].as_str().unwrap_or("").to_string();
    let first = s.cli_keys(&home).expect("the CLI logged in");
    let made = s.name("rotating");
    s.cli_json(api, &home, &["create", &made, "--json"])?;
    let rotated = s.cli_json(api, &home, &["keys", "rotate", "--json"]);
    let second = s.cli_keys(&home).expect("the CLI has a key");
    s.ok("fragment keys rotate switches the machine to a new key", rotated.is_ok() && second.pubkey_hex() != first.pubkey_hex(), format!("{rotated:?}"));
    let r = api.status(&second, &made)?;
    s.ok("which still owns what the old one made", r.status == 200 && r.body["role"] == "owner" && api.identity(&second)? == cli_id, &r);
    let r = api.status(&first, &made)?;
    s.ok("and the old key is revoked", r.status == 401, &r);

    // what the registry costs: one cell hop per signed request
    let signed = median_ms(|| Ok(api.signed(&new, "GET", "/api/identities/me", None)?.status), 21)?;
    let unsigned = median_ms(|| Ok(api.unsigned("GET", "/healthz", None)?.status), 21)?;
    println!("      a signed request resolved by the registry: {signed:.1} ms median; an unsigned one: {unsigned:.1} ms");

    // the registry down: a visible 503, never an allow
    let r = api.unsigned("POST", "/api/test/registry", Some(&json!({ "down": true })))?;
    s.ok("(the test hook takes the registry down)", r.status == 200, &r);
    let r = api.status(&new, &mine)?;
    s.ok("a signed request is refused with 503 while the registry cannot answer", r.status == 503 && r.error() == "registry_unavailable", &r);
    let r = api.signed(&new, "GET", "/api/fragments", None)?;
    s.ok("and so is every other", r.status == 503, &r);
    api.unsigned("POST", "/api/test/registry", Some(&json!({ "down": false })))?;
    let ok = s.eventually(Duration::from_secs(5), || api.status(&new, &mine).is_ok_and(|r| r.status == 200));
    s.ok("back up, it answers again", ok, "");
    Ok(())
}
