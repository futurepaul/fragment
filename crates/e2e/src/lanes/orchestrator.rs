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

    fences(s, api, &Fence { paul: &paul, bob: &bob, paul_id: &paul_id, hand: &hand, hand_id: &hand_id, computer: &computer, maple: &maple, maple_id: &maple_id })?;
    cli(s, api)
}

/// What `fences` works with: Paul, Bob, Hand (Paul's agent, by its key),
/// Paul's computer, and Maple on it.
struct Fence<'a> {
    paul: &'a Keys,
    bob: &'a Keys,
    paul_id: &'a str,
    hand: &'a Keys,
    hand_id: &'a str,
    computer: &'a str,
    maple: &'a str,
    maple_id: &'a str,
}

/// Whether the computer's view has `fragment` fenced.
fn fenced_in(view: &Value, fragment: &str) -> Option<bool> {
    view["agents"].as_array().into_iter().flatten().find(|a| a["fragment"] == fragment).map(|a| a["fenced"] == true)
}

/// A teammate on another provider is fenced (decision 63, proposed): it
/// acts as itself alone. Wren, another agent of Paul's by its key: Hand,
/// acting for Paul, fences it (and again: a replay), and then Wren reaches
/// nothing for Paul, while what is its own, and what Paul shares with it,
/// stand. Hand lets none go and fences not itself; Bob fences no one of
/// Paul's; Paul lets Wren go, and Wren acts for him again. Maple, on Paul's
/// computer, is fenced there too, and an agent fenced before it starts
/// starts fenced.
fn fences(s: &mut Suite, api: &Api, f: &Fence) -> Result<()> {
    let (wren, wren_id) = agent_of(api, f.paul)?;
    let for_paul = format!("for={}", f.paul_id);
    let fence = |keys: &Keys, who: &str, query: &str, body: Value| api.signed(keys, "PUT", &format!("/api/identities/{who}/fenced{query}"), Some(&body));
    let q = format!("?{for_paul}");

    // valid: Hand fences Wren, acting for Paul; again, the same answer
    let r = fence(f.hand, &wren_id, &q, json!({ "fenced": true }))?;
    let again = fence(f.hand, &wren_id, &q, json!({ "fenced": true }))?;
    s.ok(
        "Paul's agent fences another of Paul's agents, acting for Paul (its view says fenced); fenced again, the same answer",
        r.status == 200 && r.body["fenced"] == true && again.status == 200 && again.body["fenced"] == true,
        json!({ "fence": r.body, "again": again.status }),
    );

    // fenced, it acts for no one: not its owner, on any route that honors `for`
    let listed = api.signed(&wren, "GET", &format!("/api/fragments?{for_paul}"), None)?;
    let made = api.signed(&wren, "POST", &format!("/api/fragments?{for_paul}"), Some(&json!({ "name": s.named(api, f.paul, "wren-made")?, "template": "blank" })))?;
    let read = api.signed(&wren, "GET", &format!("/api/f/{}/files?{for_paul}", f.maple), None)?;
    let ran = api.signed(&wren, "DELETE", &format!("/api/computers/{}/agents/{}?{for_paul}", f.computer, f.maple), None)?;
    let fenced_off = |r: &Reply| r.status == 403 && r.text.contains("fenced agent acts as itself alone");
    s.ok(
        "a fenced agent acts for no one (403, saying so): it lists, makes, reads and runs nothing of its owner's",
        fenced_off(&listed) && fenced_off(&made) && fenced_off(&read) && fenced_off(&ran),
        json!({ "list": listed.status, "make": made.status, "read": read.status, "run": ran.status, "says": listed.text }),
    );

    // what is its own stands: its list, and a fragment Paul shares with it by its npub
    let shared = s.named(api, f.paul, "wren-shared")?;
    let r = api.create_with(f.paul, json!({ "name": shared, "template": "blank" }))?;
    anyhow::ensure!(r.status == 200, "Paul's fragment for Wren: {r}");
    let added = api.signed(f.paul, "PUT", &format!("/api/f/{shared}/members/{wren_id}"), Some(&json!({ "role": "editor" })))?;
    let own_list = api.signed(&wren, "GET", "/api/fragments", None)?;
    let own_read = api.signed(&wren, "GET", &format!("/api/f/{shared}/files"), None)?;
    let not_shared = api.signed(&wren, "GET", &format!("/api/f/{}/files", f.maple), None)?;
    s.ok(
        "as itself it reaches what is shared with it (a fragment Paul added it to), never the rest of Paul's (403 or 404)",
        added.status == 200 && own_list.status == 200 && own_read.status == 200 && matches!(not_shared.status, 403 | 404),
        json!({ "added": added.status, "list": own_list.status, "shared": own_read.status, "maple": not_shared.status }),
    );

    // invalid: Hand lets none go, fences not itself; Bob fences no one of
    // Paul's; Wren, fenced, fences no one; a body without `fenced`
    let lift = fence(f.hand, &wren_id, &q, json!({ "fenced": false }))?;
    let itself = fence(f.hand, f.hand_id, &q, json!({ "fenced": true }))?;
    let bob = fence(f.bob, &wren_id, "", json!({ "fenced": false }))?;
    let by_fenced = fence(&wren, f.hand_id, &q, json!({ "fenced": true }))?;
    let as_itself = fence(f.hand, &wren_id, "", json!({ "fenced": false }))?;
    let no_field = fence(f.paul, &wren_id, "", json!({}))?;
    s.ok(
        "refused: Paul's agent lets none go and fences not itself, Bob fences no one of Paul's, a fenced agent fences no one, an agent as itself fences no one (403); a body without `fenced` is 400",
        lift.status == 403 && lift.text.contains("only an agent's owner lets it go")
            && itself.status == 403 && itself.text.contains("never fences itself")
            && bob.status == 403
            && fenced_off(&by_fenced)
            && as_itself.status == 403
            && no_field.status == 400,
        json!({ "lift": lift.status, "itself": itself.status, "bob": bob.status, "fenced": by_fenced.status, "asItself": as_itself.status, "noField": no_field.status }),
    );
    let still = api.signed(&wren, "GET", &format!("/api/fragments?{for_paul}"), None)?;
    s.ok("and Wren is still fenced", fenced_off(&still), &still);

    // Paul lets it go: it acts for him again
    let r = fence(f.paul, &wren_id, "", json!({ "fenced": false }))?;
    let back = api.signed(&wren, "GET", &format!("/api/fragments?{for_paul}"), None)?;
    s.ok("its owner lets it go, and it acts for its owner again", r.status == 200 && r.body["fenced"] != true && back.status == 200, json!({ "lift": r.body, "list": back.status }));

    // on Paul's computer: Maple fenced there at once, and let go
    let view = |keys: &Keys| api.signed(keys, "GET", &format!("/api/computers/{}", f.computer), None);
    let r = fence(f.hand, f.maple_id, &q, json!({ "fenced": true }))?;
    let fenced_view = view(f.paul)?;
    let lifted = fence(f.paul, f.maple_id, "", json!({ "fenced": false }))?;
    let lifted_view = view(f.paul)?;
    s.ok(
        "an agent on Paul's computer fenced is fenced on its view at once, and let go, no more",
        r.status == 200 && fenced_in(&fenced_view.body, f.maple) == Some(true) && lifted.status == 200 && fenced_in(&lifted_view.body, f.maple) == Some(false),
        json!({ "fenced": fenced_view.body["agents"], "lifted": lifted_view.body["agents"] }),
    );

    // fenced before it starts, it starts fenced: Fern, made by Hand, fenced, then started
    let fern = s.named(api, f.paul, "fern")?;
    let r = api.signed(f.hand, "POST", &format!("/api/fragments?{for_paul}"), Some(&json!({ "name": fern, "template": "agent", "title": "Fern" })))?;
    anyhow::ensure!(r.status == 200, "Hand makes Fern: {r}");
    let path = format!("/api/computers/{}/agents/{fern}?{for_paul}", f.computer);
    let started = api.signed(f.hand, "PUT", &path, Some(&json!({})))?;
    let fern_id = identity_in(&started.body, &fern);
    let stopped = api.signed(f.hand, "DELETE", &path, None)?;
    anyhow::ensure!(stopped.status == 200, "Fern stopped: {stopped}");
    let fenced = fence(f.hand, &fern_id, &q, json!({ "fenced": true }))?;
    let again = api.signed(f.hand, "PUT", &path, Some(&json!({})))?;
    s.ok(
        "an agent fenced while it runs nowhere starts fenced (its computer reads the fence as it takes it)",
        fenced.status == 200 && again.status == 200 && fenced_in(&again.body, &fern) == Some(true),
        json!({ "fence": fenced.status, "view": again.body["agents"] }),
    );
    let r = api.signed(f.hand, "DELETE", &path, None)?;
    anyhow::ensure!(r.status == 200, "Fern stopped again: {r}");
    Ok(())
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
    let computer_fenced = |agent: &str| -> Result<Option<bool>> {
        let view = api.signed(&me, "GET", "/api/computers", None)?.body["computers"][0].clone();
        Ok(fenced_in(&view, agent))
    };
    s.ok(
        "and fences it (decision 63, proposed): it says so, and so does its computer",
        own["fenced"] == true && computer_fenced(&agent)? == Some(true),
        json!({ "answer": own["fenced"], "computer": computer_fenced(&agent)? }),
    );
    let back = s.cli_json(api, &home, &["agent", &agent, "--no-model", "--json"])?;
    s.ok("`--no-model` puts it on its tier again", back["runsOn"] == json!({ "on": "platform", "tier": "medium" }), &back);
    s.ok("and lifts no fence: its owner does", back["fenced"] == true, &back);
    let lifted = s.cli_json(api, &home, &["agent", &agent, "--no-fence", "--json"])?;
    let again = s.cli_json(api, &home, &["agent", &agent, "--fence", "--json"])?;
    let lifted_again = s.cli_json(api, &home, &["agent", &agent, "--no-fence", "--json"])?;
    s.ok(
        "its owner's `--no-fence` lets it go and `--fence` fences it again",
        lifted["fenced"] == false && again["fenced"] == true && lifted_again["fenced"] == false && computer_fenced(&agent)? == Some(false),
        json!({ "lifted": lifted["fenced"], "fenced": again["fenced"], "liftedAgain": lifted_again["fenced"] }),
    );
    // made on its owner's own provider, it is fenced from the start
    let elm = s.name("elm");
    let made = s.cli_json(api, &home, &["agent", "--new", &elm, "--name", "Elm", "--job", "You draft letters from the outlines you are sent.", "--model", "openrouter:anthropic/claude-sonnet-5.5", "--json"])?;
    let elm_agent = made["agent"].as_str().unwrap_or("").to_string();
    s.owned(&json!({ "name": elm_agent }), &me);
    s.owned(&json!({ "name": made["chat"] }), &me);
    s.ok(
        "`fragment agent --new … --model <provider>:<id>` makes it fenced, before its job, model or chat",
        made["fenced"] == true && computer_fenced(&elm_agent)? == Some(true),
        json!({ "made": made, "computer": computer_fenced(&elm_agent)? }),
    );
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
