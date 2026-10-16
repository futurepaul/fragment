//! A person's default agent keeps the rest (docs/cloudflare-v1.md,
//! decision 62, proposed): acting for its owner, an agent starts and stops
//! its owner's agents on its owner's computer, and does nothing else there.
//!
//! Hand, a key Paul registered as his agent, signs `for=<Paul>`: it makes
//! Maple, an agent fragment of Paul's, starts it on Paul's computer, stops
//! it, and makes its own chat, which it leaves so Maple leads it. Each is
//! asked again (a replay changes nothing), and refused as itself, for
//! someone else, held, on every other computer route, and on someone
//! else's computer or agent. Then the CLI does it all in one step, `fragment
//! agent --new`, as a person, and keeps it (`--tier`, `--model`, `--stop`,
//! `--start`). An agent's own path through the CLI (on its computer, where
//! its egress signs) and its refusal to stop itself are the real-Hermes
//! lane's (`hermes`): only a computer signs as an agent fragment.

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::delegation::agent_of;
use crate::api::{Api, Reply};
use crate::Suite;

/// The agents a computer's view lists, by fragment.
fn runs(view: &Value) -> Vec<String> {
    view["agents"].as_array().into_iter().flatten().filter_map(|a| a["fragment"].as_str().map(str::to_string)).collect()
}

fn identity_in(view: &Value, fragment: &str) -> String {
    view["agents"].as_array().into_iter().flatten().find(|a| a["fragment"] == fragment).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string()
}

/// The events of `name` of `kind` that name `by`.
fn events_by(api: &Api, owner: &Keys, name: &str, kind: &str, by: &str) -> Result<Vec<Value>> {
    let r = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=50"), None)?;
    Ok(r.body["events"].as_array().into_iter().flatten().filter(|e| e["kind"] == kind && e["data"]["by"] == by).cloned().collect())
}

pub fn orchestrator(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("orchestrator", &[crate::Need::Computers]) {
        return Ok(());
    }
    let (paul, bob) = (api.person()?, api.person()?);
    let (paul_id, bob_id) = (api.identity(&paul)?, api.identity(&bob)?);
    let (hand, hand_id) = agent_of(api, &paul)?;
    let r = api.signed(&paul, "POST", "/api/computers", Some(&json!({})))?;
    let computer = r.body["computer"].as_str().with_context(|| format!("Paul's computer: {r}"))?.to_string();
    let for_paul = format!("for={paul_id}");

    // Maple: an agent fragment Hand makes for Paul (Paul's, Hand its editor)
    let maple = s.named(api, &paul, "maple")?;
    let r = api.signed(&hand, "POST", &format!("/api/fragments?{for_paul}"), Some(&json!({ "name": maple, "template": "agent", "title": "Maple" })))?;
    anyhow::ensure!(r.status == 200, "Hand makes Maple for Paul: {r}");
    s.owned(&r.body, &paul);
    let agents_path = |name: &str, query: &str| format!("/api/computers/{computer}/agents/{name}{query}");
    let start = |keys: &Keys, query: &str| api.signed(keys, "PUT", &agents_path(&maple, query), Some(&json!({})));
    let stop = |keys: &Keys, query: &str| api.signed(keys, "DELETE", &agents_path(&maple, query), None);
    let q = format!("?{for_paul}");

    // valid: Hand starts Maple on Paul's computer, acting for Paul
    let r = start(&hand, &q)?;
    let maple_id = identity_in(&r.body, &maple);
    s.ok(
        "Paul's agent starts another of Paul's agents on Paul's computer, acting for Paul: its identity made, on the view",
        r.status == 200 && runs(&r.body).contains(&maple) && fragment_core::npub::is_identity(&maple_id) && maple_id != hand_id,
        &r,
    );
    let started = events_by(api, &paul, &maple, "computer.assigned", &hand_id)?;
    s.ok(
        "Maple's events say an agent started it, for its owner",
        started.len() == 1 && started[0]["summary"].as_str().is_some_and(|t| t.contains("an agent, for its owner")),
        json!(started),
    );
    let again = start(&hand, &q)?;
    s.ok(
        "replay: started again, nothing changes (one row, the same identity)",
        again.status == 200 && runs(&again.body).iter().filter(|f| **f == maple).count() == 1 && identity_in(&again.body, &maple) == maple_id,
        &again,
    );

    // invalid: as itself, for anyone else, held
    let r = start(&hand, "")?;
    s.ok("as itself (no `for`), an agent starts nothing (403)", r.status == 403 && r.text.contains("its own owner"), &r);
    let r = start(&hand, &format!("?for={bob_id}"))?;
    s.ok("nor for anyone but its owner (403)", r.status == 403 && r.text.contains("its own owner"), &r);
    let held = |role: Value| api.signed(&paul, "PUT", &format!("/api/identities/{hand_id}/held"), Some(&json!({ "held": role })));
    held(json!("editor"))?;
    let (held_start, held_stop) = (start(&hand, &q)?, stop(&hand, &q)?);
    held(Value::Null)?;
    s.ok(
        "held below Paul (at editor, the highest hold), it starts and stops nothing (403)",
        held_start.status == 403 && held_stop.status == 403 && held_start.text.contains("holds"),
        json!({ "start": held_start.status, "stop": held_stop.status, "why": held_start.body }),
    );
    // a person names no `for`
    let r = start(&paul, &q)?;
    s.ok("a person never acts `for` anyone (403)", r.status == 403, &r);

    // every other computer route stays Paul's own, `for` or not
    let mut others: Vec<(String, Reply)> = vec![];
    for (method, path, body) in [
        ("GET", "/api/computers".to_string(), None),
        ("POST", "/api/computers".to_string(), Some(json!({}))),
        ("GET", format!("/api/computers/{computer}"), None),
        ("POST", format!("/api/computers/{computer}/wake"), Some(json!({}))),
        ("POST", format!("/api/computers/{computer}/restart"), Some(json!({}))),
        ("PUT", format!("/api/computers/{computer}/image"), Some(json!({ "image": "stub" }))),
        ("PUT", format!("/api/computers/{computer}/agents/{maple}/connections"), Some(json!({ "connections": [] }))),
        ("GET", format!("/api/computers/{computer}/uses"), None),
    ] {
        others.push((format!("{method} {path}"), api.signed(&hand, method, &path, body.as_ref())?));
        let named = format!("{path}?{for_paul}");
        others.push((format!("{method} {named}"), api.signed(&hand, method, &named, body.as_ref())?));
    }
    let wrong: Vec<String> = others
        .iter()
        // as itself, 403; naming `for` where it is not honored, 400
        .filter(|(asked, r)| !matches!((asked.contains("for="), r.status), (false, 403) | (true, 400)))
        .map(|(asked, r)| format!("{asked}: {r}"))
        .collect();
    s.ok("every other computer route stays its owner's: an agent is refused (403; 400 naming `for` where it is not honored)", wrong.is_empty(), json!(wrong));

    // someone else's: Bob's computer, and an agent fragment of Bob's
    let r = api.signed(&bob, "POST", "/api/computers", Some(&json!({})))?;
    let bobs_computer = r.body["computer"].as_str().unwrap_or("").to_string();
    let r = api.signed(&hand, "PUT", &format!("/api/computers/{bobs_computer}/agents/{maple}?{for_paul}"), Some(&json!({})))?;
    s.ok("Paul's agent starts nothing on Bob's computer (404: no such computer of Paul's)", r.status == 404, &r);
    let bobs_agent = s.named(api, &bob, "bobs-agent")?;
    let r = api.create_with(&bob, json!({ "name": bobs_agent, "template": "agent", "title": "Bob's" }))?;
    anyhow::ensure!(r.status == 200, "Bob's agent fragment: {r}");
    s.owned(&r.body, &bob);
    let r = api.signed(&hand, "PUT", &agents_path(&bobs_agent, &q), Some(&json!({})))?;
    s.ok("nor Bob's agent on Paul's computer (403: only the agent fragment's owner runs it)", r.status == 403, &r);

    // its own chat, as `fragment agent --new` makes it: Hand makes it for
    // Paul (in it as its maker, so its lead), puts Maple in, and leaves
    let chat = s.named(api, &paul, "maple-chat")?;
    let r = api.signed(&hand, "POST", &format!("/api/fragments?{for_paul}"), Some(&json!({ "name": chat, "template": "chat", "title": "Maple" })))?;
    anyhow::ensure!(r.status == 200, "Maple's chat: {r}");
    s.owned(&r.body, &paul);
    let members = |keys: &Keys| -> Result<Vec<Value>> { Ok(api.signed(keys, "GET", &format!("/api/f/{chat}/members"), None)?.body["members"].as_array().cloned().unwrap_or_default()) };
    let added = api.signed(&hand, "PUT", &format!("/api/f/{chat}/members/{maple_id}?{for_paul}"), Some(&json!({ "role": "editor" })))?;
    let left = api.signed(&hand, "DELETE", &format!("/api/f/{chat}/members/me?{for_paul}"), None)?;
    let now = members(&paul)?;
    let agents: Vec<&Value> = now.iter().filter(|m| m["kind"] == "agent").collect();
    // the list's row, as the chat last told it
    let row_of = || api.signed(&paul, "GET", "/api/fragments", None).ok().and_then(|r| r.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == chat.as_str()).cloned())).unwrap_or_default();
    s.eventually(std::time::Duration::from_secs(10), || row_of()["agents"] == json!([maple_id]));
    let row = row_of();
    s.ok(
        "Maple's own chat: Paul's, Maple in it, and Hand, its maker, left it, so Maple leads it (the first agent in its list)",
        added.status == 200
            && left.status == 200
            && now.iter().any(|m| m["principal"] == paul_id.as_str() && m["role"] == "owner")
            && agents.len() == 1
            && agents[0]["principal"] == maple_id.as_str()
            && row["agents"].get(0) == Some(&json!(maple_id)),
        json!({ "added": added.status, "left": left.body, "members": now, "row": row }),
    );

    // stopped, and stopped again (a replay): the view no longer lists it
    let r = stop(&hand, &q)?;
    let again = stop(&hand, &q)?;
    let stopped = events_by(api, &paul, &maple, "computer.unassigned", &hand_id)?;
    s.ok(
        "Paul's agent stops Maple, acting for Paul: off the view, its events say an agent did; stopped again, nothing changes",
        r.status == 200 && !runs(&r.body).contains(&maple) && again.status == 200 && !runs(&again.body).contains(&maple) && stopped.len() == 1,
        json!({ "stop": r.body, "again": again.status, "events": stopped }),
    );
    let r = start(&hand, &q)?;
    s.ok("and starts it again, the same identity", r.status == 200 && identity_in(&r.body, &maple) == maple_id, &r);

    cli(s, api)
}

/// `fragment agent --new` and the flags that keep an agent, through the
/// CLI as a person (on their own computer: an agent's are the hermes
/// lane's).
fn cli(s: &mut Suite, api: &Api) -> Result<()> {
    let home = s.dir("orchestrator-home");
    s.login(api, &home);
    let me = s.cli_keys(&home).context("the CLI's key")?;
    let label = s.name("oak");
    let job = "You keep the oak grove's notes: what grows where, and when to prune.";
    let new = ["agent", "--new", &label, "--name", "Oak", "--job", job, "--tier", "medium", "--json"];
    let made = s.cli_json(api, &home, &new)?;
    let agent = made["agent"].as_str().unwrap_or("").to_string();
    let chat = made["chat"].as_str().unwrap_or("").to_string();
    s.owned(&json!({ "name": agent }), &me);
    s.owned(&json!({ "name": chat }), &me);
    let view = api.signed(&me, "GET", "/api/computers", None)?.body["computers"][0].clone();
    let soul = api.signed(&me, "GET", &format!("/api/f/{agent}/file?path=SOUL.md"), None)?;
    let settings = api.signed(&me, "GET", &format!("/api/f/{agent}/file?path=agent.json"), None)?;
    let status = api.signed(&me, "GET", &format!("/api/f/{agent}/status"), None)?;
    s.ok(
        "`fragment agent --new` makes an agent in one step: its fragment (titled by --name), running on its owner's computer (made for it), its job and tier live",
        made["madeAgent"] == true
            && agent.starts_with(&format!("{label}--"))
            && runs(&view).contains(&agent)
            && identity_in(&view, &agent) == made["identity"].as_str().unwrap_or("-")
            && soul.text == format!("{job}\n")
            && serde_json::from_str::<Value>(&settings.text).ok().is_some_and(|v| v["tier"] == "medium")
            && made["runsOn"] == json!({ "on": "platform", "tier": "medium" })
            && status.body["pins"]["live"].is_string(),
        json!({ "made": made, "view": view, "soul": soul.text, "agentJson": settings.text }),
    );
    let members = api.signed(&me, "GET", &format!("/api/f/{chat}/members"), None)?.body["members"].clone();
    let agents: Vec<&Value> = members.as_array().into_iter().flatten().filter(|m| m["kind"] == "agent").collect();
    s.ok(
        "and its own chat with its owner (`<label>-chat`, its Bot Chat), it alone there of the agents: its lead",
        made["madeChat"] == true && chat.starts_with(&format!("{label}-chat--")) && agents.len() == 1 && agents[0]["principal"] == made["identity"] && agents[0]["role"] == "editor",
        json!({ "chat": chat, "members": members }),
    );
    let again = s.cli_json(api, &home, &new)?;
    s.ok(
        "replay: asked again, it finds what it made (no second agent or chat)",
        again["agent"] == agent.as_str() && again["chat"] == chat.as_str() && again["madeAgent"] == false && again["madeChat"] == false,
        &again,
    );
    let other = ["agent", "--new", &label, "--name", "Oak", "--job", "Something else entirely.", "--json"];
    let out = s.cli(api, &home, &other);
    let said = String::from_utf8_lossy(&out.stdout).to_string();
    s.ok(
        "a label whose agent has another job already is refused (conflict), its job never rewritten",
        !out.status.success() && said.contains("job of its own") && api.signed(&me, "GET", &format!("/api/f/{agent}/file?path=SOUL.md"), None)?.text == format!("{job}\n"),
        &said,
    );
    let bad = s.cli(api, &home, &["agent", "--new", "Not A Label", "--name", "X", "--job", "y"]);
    let high = s.cli(api, &home, &["agent", &agent, "--tier", "high"]);
    let model = s.cli(api, &home, &["agent", &agent, "--model", "claude"]);
    s.ok(
        "a bad label, the refused tier and a model without its provider are usage mistakes (exit 2), nothing written",
        [&bad, &high, &model].iter().all(|o| o.status.code() == Some(2)),
        json!([bad.status.code(), high.status.code(), model.status.code()]),
    );

    // keeping it: its model, its tier again, stopped and started
    let own = s.cli_json(api, &home, &["agent", &agent, "--model", "openrouter:anthropic/claude-sonnet-5.5", "--json"])?;
    let written = api.signed(&me, "GET", &format!("/api/f/{agent}/file?path=agent.json"), None)?;
    s.ok(
        "`--model <provider>:<id>` writes agent.json's model beside its tier, and says its calls go outside the platform",
        own["changed"] == true
            && own["runsOn"] == json!({ "on": "own", "provider": "openrouter", "id": "anthropic/claude-sonnet-5.5" })
            && serde_json::from_str::<Value>(&written.text).ok().is_some_and(|v| v["model"]["id"] == "anthropic/claude-sonnet-5.5" && v["tier"] == "medium"),
        json!({ "answer": own, "agentJson": written.text }),
    );
    let back = s.cli_json(api, &home, &["agent", &agent, "--no-model", "--json"])?;
    s.ok("`--no-model` puts it on its tier again", back["runsOn"] == json!({ "on": "platform", "tier": "medium" }), &back);
    let stopped = s.cli_json(api, &home, &["agent", &agent, "--stop", "--json"])?;
    let shown = s.cli_json(api, &home, &["agent", &agent, "--json"])?;
    let started = s.cli_json(api, &home, &["agent", &agent, "--start", "--json"])?;
    s.ok(
        "`--stop` stops it and `--start` starts it again; shown, it says whether it runs",
        stopped["running"] == false && shown["running"] == false && started["running"] == true,
        json!({ "stopped": stopped, "shown": shown, "started": started }),
    );
    Ok(())
}
