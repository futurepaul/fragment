//! Placement (docs/self-host.md, seam 2; `fragment_core::placement`): on a
//! run with sandcastle nodes (`FRAGMENT_E2E_NODES=two`), one that listens
//! and one that dials in. A computer is placed at its first start, not
//! before; two placed one after the other go to two nodes (the rule: the
//! fewest computers for a node's capacity, every placement so far balanced
//! on two nodes of one capacity); each runs its image on its node, its
//! screen through its port; asleep and woken, it wakes where it was. Each
//! node in turn goes down: a wake of a computer on it answers 503
//! `node_down`, typed and within seconds, its view says why, and a new
//! computer goes to the node that is up; the node back, the computer wakes
//! on it again. That pinning outlives a restart of the platform is the
//! restart section's.

use std::time::{Duration, Instant};

use anyhow::Result;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{Api, Call};
use crate::Suite;

/// A start of the stub on a node and its restore: well under this.
const WAKE: Duration = Duration::from_secs(90);
/// A wake of a computer on a node that is down answers within this: a
/// node that listens refuses the connection at once; one that dials in is
/// waited for as long as its object waits for a dial (node.mjs's
/// HEALTH_MS, past uplink.mjs's UPLINK_WAIT_MS).
const DOWN_ANSWER: Duration = Duration::from_secs(15);
/// The stub's bridge has booted and made its state under `/data` by then.
const BOOTED: Duration = Duration::from_secs(2);

/// A person's computer, made.
fn computer(api: &Api, owner: &Keys) -> Result<(String, Value)> {
    let r = api.signed(owner, "POST", "/api/computers", Some(&json!({})))?;
    anyhow::ensure!(r.status == 200, "making a computer: {r}");
    Ok((r.body["computer"].as_str().unwrap_or("").to_string(), r.body.clone()))
}

fn wake(api: &Api, owner: &Keys, id: &str) -> Result<crate::api::Reply> {
    api.signed(owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))
}

fn sleep(api: &Api, owner: &Keys, id: &str) -> Result<crate::api::Reply> {
    api.signed(owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))
}

fn view(api: &Api, owner: &Keys, id: &str) -> Value {
    api.signed(owner, "GET", &format!("/api/computers/{id}"), None).map(|r| r.body).unwrap_or(Value::Null)
}

/// The build its screen serves, through its port, as its owner signs.
fn version(api: &Api, owner: &Keys, origin: &str) -> String {
    api.call(Call { method: "GET", url: format!("{origin}/p/6080/version.txt"), keys: Some(owner), ..Call::default() }).map(|r| r.text.trim().to_string()).unwrap_or_default()
}

pub fn placement(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("placement", &[crate::Need::Nodes, crate::Need::Computers, crate::Need::Node]) {
        return Ok(());
    }
    if s.node_ids().len() < 2 {
        s.skip("computers spread across nodes, each pinned to its own", "it needs two nodes (FRAGMENT_E2E_NODES=two); this run's one is the real engine's");
        return Ok(());
    }
    let ids = s.node_ids();
    anyhow::ensure!(ids.len() == 2, "the run's nodes: {ids:?}");
    let (a, b) = (api.person()?, api.person()?);
    let (ca, made) = computer(api, &a)?;
    s.ok("a computer is placed at its first start, not before", made["node"].is_null() && made["phase"] == "asleep", &made);
    let r = wake(api, &a, &ca)?;
    let na = r.body["node"].as_str().unwrap_or("").to_string();
    s.ok("its first start places it on a node, and it wakes there", r.status == 200 && r.body["phase"] == "awake" && ids.contains(&na), &r);
    let (cb, _) = computer(api, &b)?;
    let r = wake(api, &b, &cb)?;
    let nb = r.body["node"].as_str().unwrap_or("").to_string();
    s.ok(
        "the next one goes to the other node: the fewest computers for its capacity",
        r.status == 200 && r.body["phase"] == "awake" && ids.contains(&nb) && nb != na,
        json!({ "first": na, "second": r.body }),
    );
    // each runs its image on its node: one reached directly, one over its uplink
    let (oa, ob) = (made["origin"].as_str().unwrap_or("").to_string(), view(api, &b, &cb)["origin"].as_str().unwrap_or("").to_string());
    let (va, vb) = (version(api, &a, &oa), version(api, &b, &ob));
    s.ok("both computers' screens answer through their ports, one on each node, whether it listens or dials in", va == "1" && vb == "1", format!("{na} ({}): {va:?}; {nb} ({}): {vb:?}", s.node_reach(&na), s.node_reach(&nb)));

    // asleep and woken, it wakes where it was: its /data saved on the way
    // down, restored on the way up (the stub's bridge makes /data as it
    // boots, within a second: a sleep sooner saves nothing to restore)
    std::thread::sleep(BOOTED);
    sleep(api, &a, &ca)?;
    let r = wake(api, &a, &ca)?;
    s.ok("asleep and woken, a computer wakes on the node it was placed on", r.status == 200 && r.body["phase"] == "awake" && r.body["node"] == na.as_str(), &r);
    sleep(api, &a, &ca)?;
    sleep(api, &b, &cb)?;

    // each node in turn goes down
    for (id, owner, pinned, other) in [(nb.clone(), &b, cb.clone(), na.clone()), (na.clone(), &a, ca.clone(), nb.clone())] {
        let reach = s.node_reach(&id);
        s.node_down(&id)?;
        let t0 = Instant::now();
        let r = wake(api, owner, &pinned)?;
        let took = t0.elapsed();
        s.ok(
            &format!("its node down (one that {reach}), a wake answers 503 node_down, naming it, within {DOWN_ANSWER:?}"),
            r.status == 503 && r.body["error"] == "node_down" && r.body["message"].as_str().is_some_and(|m| m.contains(&id)) && took < DOWN_ANSWER,
            format!("{r} in {took:.1?}"),
        );
        let v = view(api, owner, &pinned);
        s.ok("its view says why, and it stays placed on its node", v["node"] == id.as_str() && v["phase"] != "awake" && v["why"].as_str().is_some_and(|w| w.contains(&id)), &v);
        let c = api.person()?;
        let (cc, _) = computer(api, &c)?;
        let r = wake(api, &c, &cc)?;
        s.ok("a new computer goes to the node that is up", r.status == 200 && r.body["phase"] == "awake" && r.body["node"] == other.as_str(), &r);
        sleep(api, &c, &cc)?;
        s.node_up(&id)?;
        // a node that dials in is back once its uplink is: a wake may find it dialing
        let back = s.eventually(WAKE, || wake(api, owner, &pinned).is_ok_and(|r| r.status == 200 && r.body["phase"] == "awake"));
        let v = view(api, owner, &pinned);
        let why = v["why"].as_str().unwrap_or("");
        s.ok("its node back, the computer wakes on it again, and its view no longer says its node is down", back && v["node"] == id.as_str() && !why.contains(&id), &v);
        sleep(api, owner, &pinned)?;
    }
    Ok(())
}
