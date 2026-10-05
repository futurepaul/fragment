//! Bring your own computer (experimental: docs/self-host.md, seam 2;
//! `fragment_core::pairing`), on a run with sandcastle nodes
//! (`FRAGMENT_E2E_NODES=two`), whose people may pair nodes of their own
//! (`FRAGMENT_BYOC=on`):
//!
//! - the device flow over HTTP: a node's unsigned start and its polls (the
//!   slow ones slowed down), the person's page (signed in, from the
//!   platform's own origin), a wrong code counted, the approval had once,
//!   the secret taken once, a replayed poll answered nothing, another
//!   person seeing none of it, wrong codes past their bound refused;
//! - no node dials as another's: a person's node or a deployment's,
//!   without its secret;
//! - `sandcastle-node pair` end to end, in front of sandcastle's Docker
//!   engine double: the code it shows approved, its config and secret
//!   written, the node dialing in; the person chooses it, and their
//!   computer's first start places it there, its screen through the
//!   node's uplink; a choice that cannot take a computer falls back to the
//!   deployment's rule, saying why;
//! - across a restart of the platform, the pairing and the choice hold;
//! - revoked, the node's uplink is cut and its dial refused, and its
//!   computer's wake answers `node_revoked`, typed, before and after a
//!   restart;
//! - with BYOC off, pairing is refused, saying why; and pairings begun
//!   too fast are refused.

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{url_enc, Api, Call, Reply};
use crate::Suite;

/// A node that dials in is back once it dials again (sandcastle's
/// docs/node.md, The uplink: its backoff after a restart).
const NODE_BACK: Duration = Duration::from_secs(180);
/// The stub's bridge has booted and made its state under `/data` by then.
const BOOTED: Duration = Duration::from_secs(2);

/// A person signed in with a browser (the session approval needs) and a key.
fn person(api: &Api) -> Result<(Keys, String)> {
    let keys = Keys::generate();
    let session = api.sign_in(&Api::email_of(&keys))?;
    let me = api.approve(&session, &keys)?;
    anyhow::ensure!(me.status == 200, "a person: {me}");
    Ok((keys, session))
}

fn start(api: &Api, name: &str) -> Result<Reply> {
    api.unsigned("POST", "/api/nodes/pair", Some(&json!({ "name": name, "arch": std::env::consts::ARCH })))
}

fn poll(api: &Api, device: &str) -> Result<Reply> {
    api.unsigned("POST", "/api/nodes/pair/poll", Some(&json!({ "deviceCode": device })))
}

/// The approval page, as a browser asks for it (`session`: signed in).
fn page(api: &Api, code: &str, session: Option<&str>) -> Result<Reply> {
    api.call(Call { method: "GET", url: format!("{}/nodes/pair?code={}", api.base, url_enc(code)), cookie: session.map(|s| format!("fragment_session={s}")), ..Call::default() })
}

/// The approval form, posted from `origin`.
fn approve(api: &Api, code: &str, session: Option<&str>, origin: &str) -> Result<Reply> {
    api.call(Call {
        method: "POST",
        url: format!("{}/nodes/pair", api.base),
        body: Some(format!("code={}", url_enc(code)).into_bytes()),
        content_type: Some("application/x-www-form-urlencoded"),
        cookie: session.map(|s| format!("fragment_session={s}")),
        extra: vec![("origin", origin.to_string())],
        ..Call::default()
    })
}

fn nodes(api: &Api, who: &Keys) -> Value {
    api.signed(who, "GET", "/api/nodes", None).map(|r| r.body).unwrap_or(Value::Null)
}

fn node_in<'a>(list: &'a Value, id: &str) -> Option<&'a Value> {
    list["nodes"].as_array()?.iter().find(|n| n["id"] == id)
}

fn wake(api: &Api, who: &Keys, id: &str) -> Result<Reply> {
    api.signed(who, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))
}

fn sleep(api: &Api, who: &Keys, id: &str) -> Result<Reply> {
    api.signed(who, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))
}

fn view(api: &Api, who: &Keys, id: &str) -> Value {
    api.signed(who, "GET", &format!("/api/computers/{id}"), None).map(|r| r.body).unwrap_or(Value::Null)
}

fn computer(api: &Api, who: &Keys) -> Result<String> {
    let r = api.signed(who, "POST", "/api/computers", Some(&json!({})))?;
    anyhow::ensure!(r.status == 200, "making a computer: {r}");
    Ok(r.body["computer"].as_str().unwrap_or("").to_string())
}

/// A `sandcastle-node serve` that dials as `id` with a secret of its own
/// making (not the node's): what the platform tells it, from its log. Its
/// directory is named `label`, short: a socket's path inside it must fit a
/// unix socket's (a person's node's id, in a deep checkout, did not).
fn impostor(s: &Suite, id: &str, label: &str) -> Result<String> {
    let tools = s.sandcastle.as_ref().context("sandcastle's binaries")?;
    let dir = s.scratch.join("n").join(format!("x-{label}"));
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("node.secret"), fragment_devstack::random_hex(32))?;
    std::fs::set_permissions(dir.join("node.secret"), std::fs::Permissions::from_mode(0o600))?;
    let platform = s.platform();
    let sock = |name: &str| fragment_devstack::sandcastle::socket_path(&dir, name);
    let config = json!({
        "engine": sock("engine.sock")?, "ports": sock("ports.sock")?, "egress": sock("egress.sock")?,
        "secret_file": dir.join("node.secret"), "platform": platform,
        "uplink": { "url": format!("{}/api/nodes/uplink", platform.replacen("http", "ws", 1)), "id": id },
    });
    std::fs::write(dir.join("node.json"), config.to_string())?;
    let log = dir.join("node.log");
    let out = std::fs::File::create(&log)?;
    let mut child = std::process::Command::new(&tools.node).arg("serve").arg("--config").arg(dir.join("node.json")).stdout(out.try_clone()?).stderr(out).spawn()?;
    let refused = s.eventually(Duration::from_secs(20), || std::fs::read_to_string(&log).is_ok_and(|t| t.contains("refused the dial")));
    let _ = std::process::Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
    let _ = child.wait();
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    Ok(if refused { text } else { format!("(no refusal within 20 s) {text}") })
}

/// The shell's settings, as the person sees them (when the run has
/// Chrome): "Computers (experimental)", their node up with their computer
/// on it, chosen for new computers, with Revoke; a screenshot kept.
fn settings_page(s: &mut Suite, api: &Api, session: &str, id: &str) -> Result<()> {
    let Some(mut b) = s.browser()? else {
        s.skip("the shell's settings show the person's node (in Chrome)", "no Chrome (CHROME_BIN)");
        return Ok(());
    };
    b.set_cookie(&format!("{}/", api.base), "fragment_session", session)?;
    let page = b.open(&format!("{}/settings", api.base))?;
    b.viewport(&page, 1280, 900, false)?;
    let row = format!("#settings-nodes [data-node={id:?}]");
    let shown = b.until(
        &page,
        &format!("(() => {{ const r = document.querySelector('{row}'); return !!r && r.dataset.state === 'up' && r.innerText.includes('Your computer runs here') && document.getElementById('settings-nodes-prefer')?.value === '{id}' && !!r.querySelector('button'); }})()"),
        Duration::from_secs(30),
    );
    let text = b.eval(&page, "document.getElementById('settings-nodes')?.innerText ?? ''")?;
    s.ok(
        "settings' Computers (experimental) shows their node up with their computer on it, chosen for new computers, with Revoke",
        shown && text.as_str().is_some_and(|t| t.to_uppercase().contains("COMPUTERS (EXPERIMENTAL)")),
        text.as_str().unwrap_or("").replace('\n', " | "),
    );
    b.eval(&page, "(document.getElementById('settings-nodes')?.scrollIntoView(), true)")?;
    let _ = b.screenshot(&page, &s.dir("pairing").join("settings-computers.png"));
    b.close(page)?;
    Ok(())
}

pub fn pairing(s: &mut Suite, _: &Api) -> Result<()> {
    if !s.section("pairing", &[crate::Need::Nodes, crate::Need::Computers, crate::Need::Node]) {
        return Ok(());
    }
    if s.real_engine() {
        s.skip("a node pairs, is placed on by choice, and is revoked", "its nodes to pair run on the engine double (FRAGMENT_E2E_NODES=two); this run's one node is the real engine's");
        return Ok(());
    }
    let api = s.api();
    let platform = s.platform();
    let (ann, ann_session) = person(&api)?;
    let bob = api.person()?;

    // ---- the device flow, over HTTP ----
    let r = start(&api, "raw")?;
    let code = r.body["userCode"].as_str().unwrap_or("").to_string();
    let device = r.body["deviceCode"].as_str().unwrap_or("").to_string();
    let code_ok = code.len() == 9 && code.bytes().enumerate().all(|(i, b)| if i == 4 { b == b'-' } else { b"BCDFGHJKLMNPQRSTVWXZ".contains(&b) });
    s.ok(
        "a node asks to pair, unsigned: a code to show, the link to approve it at, and a device code of its own",
        r.status == 200 && code_ok && device.len() == 64 && r.body["verifyUrl"] == format!("{platform}/nodes/pair?code={code}") && r.body["intervalS"] == 5,
        &r,
    );
    let first = poll(&api, &device)?;
    let soon = poll(&api, &device)?;
    s.ok(
        "its first poll waits; a poll sooner than its interval is told to slow down",
        first.body["state"] == "pending" && soon.body["state"] == "slow_down" && soon.body["intervalS"] == 10,
        format!("{first} | {soon}"),
    );
    let r = page(&api, &code, None)?;
    s.ok("the approval page sends a browser that is not signed in to sign in first", r.status == 302 && r.header("location").contains("/auth/login"), &r);
    let r = page(&api, &code, Some(&ann_session))?;
    s.ok("signed in, it names the node and its code, and says it is experimental", r.status == 200 && r.text.contains("raw") && r.text.contains(&code) && r.text.contains("experimental"), &r);
    let r = approve(&api, "BBBB-BBBB", Some(&ann_session), &platform)?;
    s.ok("a wrong code is no node waiting", r.status == 404 && r.error() == "not_found", &r);
    let r = approve(&api, &code, Some(&ann_session), "http://evil.localhost")?;
    s.ok("an approval from another origin is refused", r.status == 403, &r);
    let r = approve(&api, &code, None, &platform)?;
    s.ok("an approval with no session is refused", r.status == 401, &r);
    let r = approve(&api, &code, Some(&ann_session), &platform)?;
    s.ok("approved in their session, from the platform's own page, it is the person's", r.status == 200 && r.text.contains("Node added"), &r);
    let r = approve(&api, &code, Some(&ann_session), &platform)?;
    s.ok("the same code is not approved twice", r.status == 400 && r.message().contains("approved already"), &r);
    // past the poll's interval: an approval is answered at once all the same
    std::thread::sleep(Duration::from_secs(1));
    let r = poll(&api, &device)?;
    let raw = r.body["node"].as_str().unwrap_or("").to_string();
    s.ok(
        "the node's next poll takes the id the platform gave it and its secret",
        r.status == 200 && r.body["state"] == "approved" && fragment_core::pairing::is_paired_id(&raw) && r.body["secret"].as_str().is_some_and(|x| x.len() == 64),
        format!("{} {}", r.status, r.body["state"]),
    );
    let r = poll(&api, &device)?;
    s.ok("a poll replayed after it answers nothing: the pairing is spent", r.status == 404 && r.body.get("secret").is_none(), &r);
    let mine = nodes(&api, &ann);
    let listed = node_in(&mine, &raw).cloned().unwrap_or(Value::Null);
    s.ok("the person's settings list it as theirs, down until it dials", mine["byoc"] == true && listed["kind"] == "own" && listed["state"] == "down" && listed["name"] == "raw", &mine);
    let theirs = nodes(&api, &bob);
    let prefer = api.signed(&bob, "PUT", "/api/nodes/prefer", Some(&json!({ "node": raw })))?;
    let revoke = api.signed(&bob, "DELETE", &format!("/api/nodes/{raw}"), None)?;
    s.ok(
        "another person sees none of it: not listed, not theirs to choose, not theirs to revoke",
        node_in(&theirs, &raw).is_none() && prefer.status == 404 && revoke.status == 404,
        format!("{theirs} | {prefer} | {revoke}"),
    );
    // no node dials as another's
    let log = impostor(s, &raw, "own")?;
    s.ok("a node that dials as a person's node, without its secret, is refused", log.contains("refused the dial: 401"), log.lines().last().unwrap_or(""));
    let log = impostor(s, "uplink", "uplink")?;
    s.ok("a node that dials as the deployment's own node, without its secret, is refused", log.contains("refused the dial: 401"), log.lines().last().unwrap_or(""));
    let log = impostor(s, "direct", "direct")?;
    s.ok("a node that dials as a deployment node that does not dial in is refused", log.contains("refused the dial: 403"), log.lines().last().unwrap_or(""));
    let r = api.signed(&ann, "DELETE", &format!("/api/nodes/{raw}"), None)?;
    s.ok("its owner revokes it: listed as revoked", r.status == 200 && node_in(&r.body, &raw).is_some_and(|n| n["state"] == "revoked"), &r);
    // a person's wrong codes are bounded
    let (_, cat_session) = person(&api)?;
    let misses: Vec<u16> = (0..fragment_core::pairing::MISSES_MAX).map(|_| approve(&api, "BBBB-BBBB", Some(&cat_session), &platform).map(|r| r.status).unwrap_or(0)).collect();
    let r = approve(&api, "BBBB-BBBB", Some(&cat_session), &platform)?;
    s.ok(
        &format!("{} wrong codes in a row, and the next is refused before any lookup (429)", fragment_core::pairing::MISSES_MAX),
        misses.iter().all(|&m| m == 404) && r.status == 429 && r.error() == "rate_limited",
        format!("{misses:?} then {r}"),
    );

    // ---- sandcastle-node pair, end to end ----
    let mut node = s.node_to_pair("own")?;
    let mut pairing = node.pair(&platform, "e2e-mac")?;
    let (code, link) = pairing.code(Duration::from_secs(30))?;
    s.ok("`sandcastle-node pair` shows the code, and the platform's link to approve it at", link == format!("{platform}/nodes/pair?code={code}"), pairing.said.join(" | "));
    let r = approve(&api, &code, Some(&ann_session), &platform)?;
    anyhow::ensure!(r.status == 200, "approving the node: {r}");
    pairing.finish(Duration::from_secs(60))?;
    let id = node.paired()?;
    let dir = s.scratch.join("n").join("own");
    let config: Value = serde_json::from_slice(&std::fs::read(dir.join("node.json"))?)?;
    let mode = std::fs::metadata(dir.join("node.secret"))?.permissions().mode() & 0o777;
    s.ok(
        "it ends having written its config (the platform, its uplink as the id the platform gave) and its secret, 0600",
        config["uplink"]["id"] == id.as_str() && config["uplink"]["url"] == format!("{}/api/nodes/uplink", platform.replacen("http", "ws", 1)) && config["platform"] == platform.as_str() && mode == 0o600,
        format!("{config} (secret {mode:o})"),
    );
    let up = s.eventually(Duration::from_secs(30), || node_in(&nodes(&api, &ann), &id).is_some_and(|n| n["state"] == "up"));
    s.ok("started, it dials in, and the person's settings show it up", up, nodes(&api, &ann));
    let r = api.signed(&ann, "PUT", "/api/nodes/prefer", Some(&json!({})))?;
    let none = api.signed(&ann, "PUT", "/api/nodes/prefer", Some(&json!({ "node": "nonesuch" })))?;
    s.ok("a choice that names nothing, or no node of theirs, changes nothing", r.status == 400 && none.status == 404, format!("{r} | {none}"));
    let r = api.signed(&ann, "PUT", "/api/nodes/prefer", Some(&json!({ "node": id })))?;
    s.ok("they choose it for their new computers", r.status == 200 && r.body["prefer"] == id.as_str(), &r);
    let c = computer(&api, &ann)?;
    let r = wake(&api, &ann, &c)?;
    s.ok(
        "their computer's first start places it on their node, as they chose, and it wakes there",
        r.status == 200 && r.body["phase"] == "awake" && r.body["node"] == id.as_str() && r.body["placed"] == "as its owner chose",
        &r,
    );
    let origin = r.body["origin"].as_str().unwrap_or("").to_string();
    let version = api.call(Call { method: "GET", url: format!("{origin}/p/6080/version.txt"), keys: Some(&ann), ..Call::default() }).map(|r| r.text.trim().to_string()).unwrap_or_default();
    s.ok("its screen answers through their node's uplink", version == "1", &version);
    let listed = nodes(&api, &ann);
    s.ok("their settings show the computer on their node", node_in(&listed, &id).is_some_and(|n| n["computers"] == json!([c])), &listed);
    settings_page(s, &api, &ann_session, &id)?;
    // the stub's bridge makes /data as it boots, within a second: a sleep
    // sooner saves nothing (lanes/placement.rs)
    std::thread::sleep(BOOTED);
    sleep(&api, &ann, &c)?;

    // a choice that cannot take a computer falls back to the deployment's rule, saying why
    let dee = api.person()?;
    let r = api.signed(&dee, "PUT", "/api/nodes/prefer", Some(&json!({ "node": "direct" })))?;
    anyhow::ensure!(r.status == 200, "choosing a deployment node: {r}");
    s.node_down("direct")?;
    let dc = computer(&api, &dee)?;
    let r = wake(&api, &dee, &dc)?;
    let placed = r.body["placed"].as_str().unwrap_or("");
    s.ok(
        "a person's choice that is down falls back to the deployment's rule, and the computer says why",
        r.status == 200 && r.body["node"] == "uplink" && placed.contains("direct, its owner's choice, is down"),
        &r,
    );
    s.node_up("direct")?;
    sleep(&api, &dee, &dc)?;

    // ---- across a restart of the platform ----
    s.stop()?;
    let api = s.start(false, true)?;
    let mut last = Value::Null;
    let back = s.eventually(NODE_BACK, || {
        let r = wake(&api, &ann, &c);
        let ok = r.as_ref().is_ok_and(|r| r.status == 200 && r.body["phase"] == "awake");
        last = r.map(|r| r.body).unwrap_or(Value::Null);
        ok
    });
    s.ok("after a restart, the node dials again, and the computer wakes on it", back && last["node"] == id.as_str(), &last);
    s.ok("the pairing and the choice held", nodes(&api, &ann)["prefer"] == id.as_str(), nodes(&api, &ann));
    sleep(&api, &ann, &c)?;

    // ---- revoked ----
    let r = api.signed(&ann, "DELETE", &format!("/api/nodes/{id}"), None)?;
    s.ok("its owner revokes it in settings: revoked, and no longer their choice", r.status == 200 && node_in(&r.body, &id).is_some_and(|n| n["state"] == "revoked") && r.body["prefer"].is_null(), &r);
    let cut = s.eventually(Duration::from_secs(30), || node.log_text().contains("node_revoked"));
    s.ok("its uplink is cut: its next dial is refused, saying it was revoked", cut, node.log_text().lines().rev().find(|l| l.contains("uplink")).unwrap_or(""));
    let r = wake(&api, &ann, &c)?;
    s.ok("a wake of its computer answers 410 node_revoked, typed", r.status == 410 && r.error() == "node_revoked", &r);
    let v = view(&api, &ann, &c);
    s.ok("its view says why, and it stays placed on the node", v["node"] == id.as_str() && v["why"].as_str().is_some_and(|w| w.contains("revoked")), &v);
    s.stop()?;
    let api = s.start(false, true)?;
    let r = wake(&api, &ann, &c)?;
    s.ok("after a restart it is still revoked: the wake answers node_revoked", r.status == 410 && r.error() == "node_revoked", &r);
    // its node is told so, whenever it dials
    node.stop()?;

    // ---- BYOC off ----
    s.set_byoc(false);
    s.stop()?;
    let api = s.start(false, true)?;
    let r = start(&api, "off")?;
    let listed = nodes(&api, &ann);
    s.ok(
        "with BYOC off, a node's pairing is refused, saying why, and settings say so",
        r.status == 403 && r.message().contains("FRAGMENT_BYOC is off") && listed["byoc"] == false,
        format!("{r} | byoc {}", listed["byoc"]),
    );
    s.set_byoc(true);
    s.stop()?;
    let api = s.start(false, true)?;

    // pairings begun too fast are refused (platform-wide: a start is unsigned)
    let statuses: Vec<u16> = (0..=fragment_core::pairing::STARTS_PER_MINUTE).map(|i| start(&api, &format!("flood-{i}")).map(|r| r.status).unwrap_or(0)).collect();
    s.ok(
        &format!("at most {} pairings begin in a minute; the next is 429", fragment_core::pairing::STARTS_PER_MINUTE),
        statuses.last() == Some(&429) && statuses.iter().filter(|&&x| x == 200).count() as u64 <= fragment_core::pairing::STARTS_PER_MINUTE,
        format!("{statuses:?}"),
    );
    Ok(())
}
