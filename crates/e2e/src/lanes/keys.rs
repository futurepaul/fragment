//! The fleet's keys stay in the node (docs/hardening.md H1): no fleet
//! secret is a Worker variable, and what one fragment seals through `KEYS`
//! another cannot open (the host attests which cell asks).

use anyhow::Result;
use serde_json::json;

use crate::api::Api;
use crate::Suite;

pub fn keys(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("keys") {
        return Ok(());
    }
    let secrets = s.fleet_secrets();
    let r = api.unsigned("GET", "/api/test/env", None)?;
    let vars = r.body.as_object().cloned().unwrap_or_default();
    let named: Vec<&String> =
        vars.keys().filter(|k| ["FRAGMENT_HOST_SECRET", "CODESTORAGE_PRIVATE_KEY", "WORKOS_API_KEY", "OPENROUTER_MANAGEMENT_KEY"].contains(&k.as_str())).collect();
    s.ok("the cell's variables name no fleet secret", r.status == 200 && !vars.is_empty() && named.is_empty(), format!("{named:?} in {r}"));
    let held: Vec<&str> =
        secrets.iter().filter(|(_, v)| vars.values().any(|x| x.as_str().is_some_and(|x| x.contains(v.as_str())))).map(|(label, _)| *label).collect();
    s.ok("no variable holds a fleet secret's value", held.is_empty(), format!("{held:?}"));
    let rendered = std::fs::read_to_string(s.project.join(".dev.vars"))?;
    let rendered_held: Vec<&str> = secrets.iter().filter(|(_, v)| rendered.contains(v.as_str())).map(|(label, _)| *label).collect();
    s.ok("the rendered variables file holds none either", rendered_held.is_empty(), format!("{rendered_held:?}"));

    let owner = api.person()?;
    let (a, b) = (s.name("keys-a"), s.name("keys-b"));
    for name in [&a, &b] {
        let r = api.create(&owner, name)?;
        s.ok("a fragment is made (its key by KEYS)", r.status == 200 && r.body["npub"].as_str().is_some_and(|n| n.starts_with("npub1")), &r);
    }
    let ask = |fragment: &str, body: serde_json::Value| {
        let mut body = body;
        body["fragment"] = json!(fragment);
        api.unsigned("POST", "/api/test/keys", Some(&body))
    };
    let r = ask(&a, json!({ "op": "seal", "plaintext": "sk-e2e-sealed-value" }))?;
    let sealed = r.body["sealed"].as_str().unwrap_or("").to_string();
    s.ok("a fragment seals through KEYS (w2, no plaintext)", r.status == 200 && sealed.starts_with("w2.") && !sealed.contains("sk-e2e"), &r);
    let r = ask(&a, json!({ "op": "open", "sealed": sealed }))?;
    s.ok("it opens its own value", r.status == 200 && r.body["plaintext"] == "sk-e2e-sealed-value", &r);
    let r = ask(&b, json!({ "op": "open", "sealed": sealed }))?;
    s.ok("another fragment cannot open it", r.status == 500 && r.message().contains("cannot open"), &r);
    Ok(())
}
