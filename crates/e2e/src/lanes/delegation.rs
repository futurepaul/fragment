//! Agents acting for people (docs/cloudflare-v1.md, decision 36): sharing
//! with a person shares with their agents, never above that person, unless
//! the share is people only or the agent is held below its owner.
//!
//! Paul shares a fragment with Skyler as an editor; Skyler's agent edits it
//! acting for Skyler, and alone (as itself, no member) reaches nothing.
//! Marked people only, the share lends the agent nothing. Held at viewer
//! by Skyler, it reads and never edits. No one else holds Skyler's agent.

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::json;

use super::app::ship;
use crate::api::Api;
use crate::Suite;

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");

pub fn delegation(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("delegation", &[]) {
        return Ok(());
    }
    let (paul, skyler) = (api.person()?, api.person()?);
    let skyler_id = api.identity(&skyler)?;
    // Skyler's agent, registered by Skyler
    let juniper = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&skyler, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&juniper, "POST", reg, &skyler) })))?;
    let juniper_id = r.body["id"].as_str().unwrap_or("").to_string();
    s.ok("Skyler registers an agent", r.status == 200 && r.body["owner"] == skyler_id.as_str(), &r);

    let name = s.named(api, &paul, "shared")?;
    let c = s.create(api, &paul, &name)?;
    ship(s, &c, TODO_APP, TODO_JSON);
    api.signed(&paul, "PUT", &format!("/api/f/{name}/visibility"), Some(&json!({ "visibility": "members" })))?;
    let share = |people_only: bool| api.signed(&paul, "PUT", &format!("/api/f/{name}/members/{skyler_id}"), Some(&json!({ "role": "editor", "peopleOnly": people_only })));
    let r = share(false)?;
    s.ok("Paul shares it with Skyler as an editor", r.status == 200 && r.body["role"] == "editor", &r);
    let mut n = 0;
    let mut add = |for_skyler: bool| {
        n += 1;
        let path = match for_skyler {
            true => format!("/api/f/{name}/ops/add_todo?for={skyler_id}"),
            false => format!("/api/f/{name}/ops/add_todo"),
        };
        api.signed(&juniper, "POST", &path, Some(&json!({ "id": format!("j{n}"), "input": { "text": "from Skyler's agent" } })))
    };
    let r = add(true)?;
    s.ok("Skyler's agent edits it, acting for Skyler", r.status == 200, &r);
    let r = add(false)?;
    s.ok("but alone, as itself, it reaches nothing (it is no member)", r.status == 403, &r);

    let r = share(true)?;
    s.ok("Paul marks the share people only", r.status == 200 && r.body["peopleOnly"] == true, &r);
    let r = add(true)?;
    s.ok("a people-only share lends Skyler's agent nothing", r.status == 403, &r);
    let r = api.op(&skyler, &name, "add_todo", "s1", json!({ "text": "from Skyler" }))?;
    s.ok("while Skyler still edits", r.status == 200, &r);
    share(false)?;

    let held = |by: &Keys, held: serde_json::Value| api.signed(by, "PUT", &format!("/api/identities/{juniper_id}/held"), Some(&json!({ "held": held })));
    let r = held(&paul, json!("viewer"))?;
    s.ok("no one but its owner holds an agent", r.status == 403, &r);
    let r = held(&skyler, json!("owner"))?;
    s.ok("an agent is held at viewer or editor, never owner", r.status == 400, &r);
    let r = held(&skyler, json!("viewer"))?;
    s.ok("Skyler holds the agent at viewer", r.status == 200, &r);
    let r = add(true)?;
    s.ok("held below Skyler, it no longer edits", r.status == 403, &r);
    let q = api.signed(&juniper, "POST", &format!("/api/f/{name}/ops/count?for={skyler_id}"), Some(&json!({ "id": "q1", "input": {} })))?;
    s.ok("and still reads", q.status == 200, &q);
    held(&skyler, serde_json::Value::Null)?;
    let r = add(true)?;
    s.ok("let go, it edits again", r.status == 200, &r);
    Ok(())
}
