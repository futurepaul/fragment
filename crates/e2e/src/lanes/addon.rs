//! The agent add-on, in the `addon` section: a fragment declares its own
//! agent (fragment.json's `agent` block), and signed-in people talk to it
//! through a channel. The calories template: its owner makes it; a second
//! person, signed in and holding its link but no member, says what they
//! ate from its page; the agent calls `log_food` for each item for them
//! (the rows are theirs) and answers on the channel. An
//! anonymous post starts nothing; the agent is offered only the operations
//! its block names; its owner's budget pays for it; a job's turn
//! (`job.agent`) runs once across a replay; a redeploy keeps the one agent,
//! and one without the block removes it.

use std::time::Duration;

use anyhow::Result;
use fragment_fakes::openrouter::Reply;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::jobs::{settle, started};
use super::signin::site_cookie;
use super::templates::person;
use crate::api::{Api, Call};
use crate::Suite;

fn records(api: &Api, keys: &Keys, name: &str, channel: &str) -> Vec<Value> {
    let r = api.signed(keys, "GET", &format!("/api/f/{name}/channels/{channel}?after=0"), None);
    r.ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default()
}

/// `today`, as a page with `cookie` asks it.
fn today(api: &Api, cookie: &str, name: &str) -> Value {
    api.browser_op(name, "today", "t", json!({}), Some(cookie)).map(|r| r.body["result"].clone()).unwrap_or_default()
}

/// Whom `name`'s subscriptions deliver to, and on which channel.
fn listening(api: &Api, owner: &Keys, name: &str) -> Vec<(String, String)> {
    let subs = api.signed(owner, "GET", &format!("/api/f/{name}/subscriptions"), None).map(|r| r.body["subscriptions"].clone()).unwrap_or_default();
    subs.as_array().into_iter().flatten().map(|s| (s["principal"].as_str().unwrap_or("").to_string(), s["channel"].as_str().unwrap_or("").to_string())).collect()
}

fn events(api: &Api, owner: &Keys, name: &str, kind: &str) -> usize {
    let r = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=200"), None);
    r.map_or(0, |r| r.body["events"].as_array().into_iter().flatten().filter(|e| e["kind"] == kind).count())
}

pub fn addon(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("addon") {
        return Ok(());
    }
    let agents = s.agents()?;
    let wait = Duration::from_secs(30);
    let ((owner, owner_session), (visitor, visitor_session)) = (person(api)?, person(api)?);
    let (owner_id, visitor_id) = (api.identity(&owner)?, api.identity(&visitor)?);
    let name = s.named(api, &owner, "calories")?;
    let made = api.create_with(&owner, json!({ "name": name, "template": "calories" }))?;
    anyhow::ensure!(made.status == 200, "calories from its template: {made}");
    s.hook(api, &made.body);
    // the visitor opens its link, signed in: no member, a viewer by the link
    let link = format!("fragview={}", made.body["viewToken"].as_str().unwrap_or(""));
    let visiting = format!("fragment_site={}; {link}", site_cookie(api, &visitor_session, &name)?);
    let owning = format!("fragment_site={}", site_cookie(api, &owner_session, &name)?);

    // the block makes the fragment's own agent, named as the fragment is
    let view = agents.signed(&owner, "GET", &format!("/api/a/{name}"), None)?;
    let agent = view.body["id"].as_str().unwrap_or("").to_string();
    let members = api.signed(&owner, "GET", &format!("/api/f/{name}/members"), None)?;
    let role_of = |members: &Value, who: &str| members["members"].as_array().into_iter().flatten().find(|m| m["principal"] == who).map(|m| m["role"].clone());
    s.ok(
        "a deploy that declares an agent makes the fragment's own: its owner's, an editor here, listening to its channel",
        view.status == 200
            && view.body["owner"] == owner_id.as_str()
            && role_of(&members.body, &agent) == Some(json!("editor"))
            && listening(api, &owner, &name) == [(agent.clone(), "ask".to_string())],
        json!({ "agent": view.body, "members": members.body }),
    );

    // a signed-in visitor holding the link says what they ate; the agent logs it for them
    let tool = |op: &str| fragment_core::tools::tool_name(&name, op).expect("a tool name");
    let (log, answer) = (tool("log_food"), "Logged 2 eggs and toast: 220 kcal today.");
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![(log.clone(), json!({ "food": "2 eggs", "calories": 140 })), (log.clone(), json!({ "food": "toast", "calories": 80 }))]),
        Reply::Text(answer.into()),
    ]);
    let asked = s.openrouter.chats().len();
    let r = api.browser_op(&name, "channels/ask", "a1", json!({ "text": "2 eggs and toast" }), Some(&visiting))?;
    anyhow::ensure!(r.status == 200 && r.body["result"]["principal"] == visitor_id.as_str(), "the visitor's post from the page: {r}");
    let answered = || records(api, &owner, &name, "ask").into_iter().find(|r| r["principal"] == agent.as_str() && r["body"]["text"] == answer);
    let landed = s.eventually(wait, || answered().is_some());
    let turn = answered().map(|r| r["body"]["turn"].clone()).unwrap_or_default();
    let started = records(api, &owner, &name, "work").into_iter().find(|r| r["body"]["kind"] == "turn.start" && r["body"]["turn"] == turn);
    let (theirs, owners) = (today(api, &visiting, &name), today(api, &owning, &name));
    let members = api.signed(&owner, "GET", &format!("/api/f/{name}/members"), None)?;
    s.ok(
        "a signed-in link holder's message starts a turn: log_food runs once per item for them (the rows are theirs), and the answer lands on the channel",
        landed
            && turn.is_string()
            && started.is_some_and(|r| r["body"]["asker"] == visitor_id.as_str())
            && theirs["total"] == 220
            && theirs["entries"].as_array().map(Vec::len) == Some(2)
            && owners["total"] == 0
            && role_of(&members.body, &visitor_id).is_none(),
        json!({ "theirs": theirs, "owners": owners, "ask": records(api, &owner, &name, "ask") }),
    );
    let offered: Vec<String> = s.openrouter.chats().get(asked).and_then(|c| c["tools"].as_array().cloned()).unwrap_or_default().iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect();
    s.ok(
        "the agent is offered only the operations its block names: not forget, not another fragment's, no platform verb",
        offered.len() == 2 && offered.contains(&log) && offered.contains(&tool("today")),
        json!(offered),
    );
    let budget = api.signed(&owner, "GET", "/api/budget", None)?;
    let paid: Vec<&Value> = budget.body["usage"].as_array().into_iter().flatten().filter(|u| u["kind"] == "agent.text" && u["fragment"] == name.as_str()).collect();
    let visitors = api.signed(&visitor, "GET", "/api/budget", None)?;
    s.ok(
        "its owner pays: each model call reserved and settled on the owner's month, for the visitor; the visitor's month untouched",
        paid.len() == 2
            && paid.iter().all(|u| u["state"] == "settled" && u["quantity"].as_i64().is_some_and(|q| q > 0) && u["principal"] == visitor_id.as_str() && u["agent"] == agent.as_str())
            && budget.body["spentMicros"].as_i64().is_some_and(|m| m > 0)
            && visitors.body["spentMicros"] == 0,
        json!({ "owner": budget.body, "visitor": visitors.body["spentMicros"] }),
    );

    // an anonymous visitor holding the link may post; the agent notes it and starts nothing
    let before = s.openrouter.chats().len();
    let anon = api.call(Call {
        method: "POST",
        url: api.site_url(&name, "__op/channels/ask"),
        body: Some(json!({ "id": "anon-1", "input": { "text": "a free lunch" } }).to_string().into_bytes()),
        content_type: Some("application/json"),
        cookie: Some(link.clone()),
        ..Call::default()
    })?;
    let noted = || agents.signed(&owner, "GET", &format!("/api/a/{name}"), None).map(|v| v.body["ignored"].as_array().is_some_and(|i| i.iter().any(|i| i["fragment"] == name.as_str()))).unwrap_or(false);
    let ignored = s.eventually(wait, noted);
    s.ok("an anonymous post starts nothing", anon.status == 200 && ignored && s.openrouter.chats().len() == before, &anon);

    job_turn(s, api, &owner, &visiting, &name, &agent)?;

    // a redeploy with the same block keeps the one agent; one without it removes it
    let joins = events(api, &owner, &name, "agent.joined");
    s.commit(&made.body, &[("README.md", Some(b"calories, again".as_slice()))]);
    s.deploy(&made.body);
    let again = s.eventually(wait, || events(api, &owner, &name, "agent.joined") > joins);
    s.ok("a redeploy with the block leaves one agent, one subscription", again && listening(api, &owner, &name) == [(agent.clone(), "ask".to_string())], json!(listening(api, &owner, &name)));
    let mut manifest = api.signed(&owner, "GET", &format!("/api/f/{name}/manifest"), None)?.body;
    let block = manifest.as_object_mut().and_then(|m| m.remove("agent"));
    anyhow::ensure!(block.is_some(), "the template declares an agent: {manifest}");
    s.commit(&made.body, &[("fragment.json", Some(manifest.to_string().as_bytes()))]);
    s.deploy(&made.body);
    let gone = s.eventually(wait, || events(api, &owner, &name, "agent.left") > 0);
    let members = api.signed(&owner, "GET", &format!("/api/f/{name}/members"), None)?;
    s.ok(
        "a redeploy without the block removes the agent: no membership, no subscription",
        gone && role_of(&members.body, &agent).is_none() && listening(api, &owner, &name).is_empty(),
        json!({ "members": members.body, "listening": listening(api, &owner, &name) }),
    );
    // a block whose instructions are not in live is refused like the rest of the manifest
    manifest["agent"] = json!({ "instructions": "nowhere.md", "channel": "ask" });
    s.commit(&made.body, &[("fragment.json", Some(manifest.to_string().as_bytes()))]);
    s.deploy(&made.body);
    let refused = s.eventually(wait, || api.status(&owner, &name).is_ok_and(|r| r.body["code"]["error"].as_str().is_some_and(|e| e.contains("nowhere.md is not in live"))));
    s.ok("an agent whose instructions are not in live is refused at deploy", refused && listening(api, &owner, &name).is_empty(), json!(api.status(&owner, &name)?.body["code"]));
    s.openrouter.clear_script();
    Ok(())
}

/// `job.agent` (the template's `summarize`): the run is caught after its
/// first step (the turn's start) and its kept answers are forgotten, so it
/// is held; its replay starts the step again, which reattaches to the same
/// turn: the model is asked once, the turn's answer is the run's output,
/// and the owner pays.
fn job_turn(s: &mut Suite, api: &Api, owner: &Keys, visiting: &str, name: &str, agent: &str) -> Result<()> {
    let (wait, summary) = (Duration::from_secs(30), "You ate 2 eggs and toast today: 220 kcal.");
    let lever = |op: &str, on: Option<bool>| api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": name, "op": op, "on": on })));
    let paid = || api.signed(owner, "GET", "/api/budget", None).map_or(0, |r| r.body["usage"].as_array().into_iter().flatten().filter(|u| u["kind"] == "agent.text").count());
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Text(summary.into())]);
    let (asked, paid_before) = (s.openrouter.chats().len(), paid());
    lever("hold-advances", Some(true))?;
    let run = started(&api.browser_op(name, "summarize", "sum-1", json!({}), Some(visiting))?);
    let caught = s.eventually(wait, || lever("advance-held", None).is_ok_and(|r| r.body["run"] == run));
    lever("forget-steps", None)?;
    lever("hold-advances", Some(false))?;
    let held = settle(api, owner, name, run, &["succeeded", "held"], wait);
    let answered = s.eventually(wait, || records(api, owner, name, "ask").iter().any(|r| r["principal"] == agent && r["body"]["text"] == summary));
    let r = api.signed(owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": run })))?;
    let done = settle(api, owner, name, run, &["succeeded"], wait);
    let turn = records(api, owner, name, "ask").into_iter().find(|r| r["body"]["text"] == summary).map(|r| r["body"]["turn"].clone()).unwrap_or_default();
    s.ok(
        "a job's turn (job.agent) runs once across a replay: its answer is the step's result, on the channel it named",
        caught && held["status"] == "held" && answered && r.status == 200 && done["status"] == "succeeded" && done["attempt"] == 2
            && done["output"] == json!({ "text": summary, "turn": turn })
            && s.openrouter.chats().len() == asked + 1,
        json!({ "held": held, "run": done, "model requests": s.openrouter.chats().len() - asked }),
    );
    s.ok("and its owner pays for it", paid() == paid_before + 1, json!({ "before": paid_before, "after": paid() }));
    Ok(())
}
