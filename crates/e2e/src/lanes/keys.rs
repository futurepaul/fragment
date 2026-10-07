//! The deployment's keys are Secrets Store bindings in the platform
//! Worker's env (cell/src/keys.rs): an app's env, which the platform
//! builds, holds only its capabilities, and what one fragment seals
//! another cannot open (a sealed value names the Durable Object that
//! sealed it).

use anyhow::Result;
use serde_json::json;

use super::app::ship;
use crate::api::Api;
use crate::Suite;

/// An app that says what its env and its global scope hold.
const PEEK_APP: &[u8] = br#"import { DurableObject } from "cloudflare:workers";
export class App extends DurableObject {
  peek() {
    const own = (o) => { try { return Object.getOwnPropertyNames(o); } catch { return []; } };
    const texts = [];
    for (const k of own(globalThis)) { try { const v = globalThis[k]; if (typeof v === "string") texts.push(v); } catch {} }
    for (const k of own(this.env)) { const v = this.env[k]; if (typeof v === "string") texts.push(v); }
    return { env: own(this.env).sort(), texts };
  }
}
"#;
const PEEK_JSON: &[u8] = br#"{ "name": "peek", "operations": { "peek": { "kind": "query", "role": "viewer" } } }"#;

pub fn keys(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("keys", &[crate::Need::Levers]) {
        return Ok(());
    }
    let owner = api.person()?;
    let (a, b) = (s.named(api, &owner, "keys-a")?, s.named(api, &owner, "keys-b")?);
    for name in [&a, &b] {
        let r = api.create(&owner, name)?;
        s.ok("a fragment is made with a key of its own", r.status == 200 && r.body["npub"].as_str().is_some_and(|n| n.starts_with("npub1")), &r);
    }
    let ask = |fragment: &str, body: serde_json::Value| {
        let mut body = body;
        body["fragment"] = json!(fragment);
        api.unsigned("POST", "/api/test/keys", Some(&body))
    };
    let r = ask(&a, json!({ "op": "seal", "plaintext": "sk-e2e-sealed-value" }))?;
    let sealed = r.body["sealed"].as_str().unwrap_or("").to_string();
    s.ok("a fragment seals a value (w2, no plaintext)", r.status == 200 && sealed.starts_with("w2.") && !sealed.contains("sk-e2e"), &r);
    let r = ask(&a, json!({ "op": "open", "sealed": sealed }))?;
    s.ok("it opens its own value", r.status == 200 && r.body["plaintext"] == "sk-e2e-sealed-value", &r);
    let r = ask(&b, json!({ "op": "open", "sealed": sealed }))?;
    s.ok("another fragment cannot open it", r.status == 500 && r.message().contains("cannot open"), &r);

    let peek = s.named(api, &owner, "keys-peek")?;
    let c = s.create(api, &owner, &peek)?;
    ship(s, &c, PEEK_APP, PEEK_JSON);
    let r = api.op(&owner, &peek, "peek", "p", json!({}))?;
    s.ok("an app's env holds only its capabilities", r.status == 200 && r.body["result"]["env"] == json!(["FILES"]), &r);
    let texts: Vec<String> = r.body["result"]["texts"].as_array().map(|t| t.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if s.hosted() {
        s.skip("no string an app can reach holds a deployment secret", "a preview's secrets are its own, never the run's to compare (its env's names are checked above)");
        return Ok(());
    }
    let held: Vec<&str> = s.deployment_secrets().into_iter().filter(|(_, v)| texts.iter().any(|t| t.contains(v.as_str()))).map(|(label, _)| label).collect();
    s.ok("no string an app can reach holds a deployment secret", r.status == 200 && held.is_empty(), format!("{held:?}"));
    Ok(())
}
