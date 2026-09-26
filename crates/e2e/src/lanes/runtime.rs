//! The computer runtime, on the Sprites fake (where a command runs as a
//! local process in its Sprite's home): a fragment's jobs run commands on
//! its declared computer (`job.computer.exec`), awake and billed to its
//! owner for them. A nonzero exit is a result; a replayed run reattaches
//! to its command and never runs it twice; a fragment without a computer
//! is told to declare one. A computer keeps its fragment's live files and
//! runs its `start` from them as a service, restarted when it exits and
//! after a deploy.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::jobs::{records, settle, started};
use crate::api::{Api, Socket};
use crate::Suite;

const APP: &[u8] = include_bytes!("../../fixtures/runtime.mjs");
const MANIFEST: &[u8] = include_bytes!("../../fixtures/runtime.json");

fn dir_of(p: &Path) -> String {
    p.to_str().expect("a UTF-8 path").to_string()
}

/// A fragment its owner's CLI makes and deploys from `files` (in its folder, `runtime-<label>`).
fn deployed(s: &mut Suite, api: &Api, home: &Path, label: &str, files: &[(&str, &[u8])]) -> Result<String> {
    let made = s.cli_json(api, home, &["create", label, "--json"])?;
    let name = made["name"].as_str().context("create answers its name")?.to_string();
    s.hook(api, &made);
    let site = s.dir(&format!("runtime-{label}"));
    for (path, bytes) in files {
        std::fs::write(site.join(path), bytes)?;
    }
    let o = s.cli(api, home, &["deploy", &name, "--dir", &dir_of(&site)]);
    s.ok(&format!("{label} deploys"), o.status.success(), String::from_utf8_lossy(&o.stderr));
    Ok(name)
}

/// Its computer's Sprite, once the fragment's events say it is ready.
fn ready(s: &Suite, api: &Api, owner: &Keys, name: &str) -> Option<String> {
    let mut sprite = None;
    s.eventually(Duration::from_secs(30), || {
        let events = api.signed(owner, "GET", &format!("/api/f/{name}/events?tail=50"), None).map(|r| r.body).unwrap_or_default();
        sprite = events["events"].as_array().into_iter().flatten().find(|e| e["kind"] == "computer.ready").and_then(|e| e["data"]["sprite"].as_str().map(str::to_string));
        sprite.is_some()
    });
    sprite
}

/// A run of `op`, to its end.
fn run(api: &Api, owner: &Keys, name: &str, op: &str, id: &str, input: Value) -> Result<Value> {
    let r = api.op(owner, name, op, id, input)?;
    Ok(settle(api, owner, name, started(&r), &["succeeded", "held"], Duration::from_secs(60)))
}

/// Its owner's `computer.awake` usage rows for `name`.
fn awake_rows(api: &Api, owner: &Keys, name: &str) -> usize {
    let usage = api.signed(owner, "GET", "/api/budget/usage", None).map(|r| r.body).unwrap_or_default();
    usage["usage"].as_array().map_or(0, |u| u.iter().filter(|u| u["kind"] == "computer.awake" && u["fragment"] == name).count())
}

pub fn computer_runtime(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("computer-runtime") {
        return Ok(());
    }
    let home = s.dir("runtime-owner");
    s.login(api, &home);
    let owner = s.cli_keys(&home).context("the owner's CLI logged in")?;
    let name = deployed(s, api, &home, "runner", &[("app.mjs", APP), ("fragment.json", MANIFEST)])?;
    let sprite = ready(s, api, &owner, &name);
    s.ok("its computer is made and paired", sprite.is_some(), &name);
    let its_home: PathBuf = s.scratch.join("sprites/sprites").join(sprite.unwrap_or_default());

    // a command, as the computer, in ~/fragment, billed to the owner
    let billed = awake_rows(api, &owner, &name);
    let r = run(api, &owner, &name, "shell", "hello", json!({ "command": "echo hello; pwd; fragment whoami --json" }))?;
    let out = &r["output"];
    let stdout = out["stdout"].as_str().unwrap_or("");
    s.ok(
        "a job runs a command on its computer and reads the answer",
        r["status"] == "succeeded" && out["code"] == 0 && stdout.starts_with("hello\n") && out["stderr"] == "" && out["truncated"] == false,
        &r,
    );
    s.ok(
        "in ~/fragment, with the fragment CLI on its PATH, signed in as the computer",
        stdout.contains(&format!("{}/fragment\n", its_home.display())) && stdout.contains(r#""kind":"computer""#),
        stdout,
    );
    let r = run(api, &owner, &name, "shell", "exit-3", json!({ "command": "echo oops >&2; exit 3", "opts": { "env": { "N": "1" } } }))?;
    s.ok(
        "a nonzero exit is a result, not an error",
        r["status"] == "succeeded" && r["output"]["code"] == 3 && r["output"]["stderr"] == "oops\n",
        &r,
    );
    s.ok("its awake time is billed to the owner", s.eventually(Duration::from_secs(5), || awake_rows(api, &owner, &name) > billed), &name);

    // a replay after the answer was kept reattaches: the command runs once
    let r = api.op(&owner, &name, "count", "count", json!({}))?;
    let id = started(&r);
    let held = settle(api, &owner, &name, id, &["succeeded", "held"], Duration::from_secs(60));
    s.ok("a run that fails after its command is held", held["status"] == "held", &held);
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/replay"), Some(&json!({ "run": id })))?;
    let replayed = settle(api, &owner, &name, id, &["succeeded"], Duration::from_secs(60));
    let count = std::fs::read_to_string(its_home.join("count")).unwrap_or_default();
    s.ok(
        "its replay answers the command's result without running it again",
        r.status == 200 && replayed["attempt"] == 2 && replayed["output"]["stdout"] == "1\n" && count == "1\n",
        format!("{replayed}; the counter file says {count:?}"),
    );

    // no computer declared: the job is told to declare one
    let bare = br#"{"operations": {"shell": {"kind": "job"}}}"#;
    let other = deployed(s, api, &home, "no-computer", &[("app.mjs", APP), ("fragment.json", bare)])?;
    let r = run(api, &owner, &other, "shell", "none", json!({ "command": "echo hi" }))?;
    s.ok(
        "a fragment that declares no computer gets a StepError saying to declare one",
        r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("StepError") && e.contains(r#""computer": {}"#)),
        &r,
    );

    // `start`: run from the fragment's live files as a service
    let manifest = br#"{"channels": {"said": {"post": "editor"}}, "computer": {"start": "bash said.sh"}}"#;
    let say = |v: u32, then: &str| format!("fragment post \"$FRAGMENT_NAME\" said --body '{{\"v\": {v}}}'\n{then}\n");
    let pet = deployed(s, api, &home, "said", &[("fragment.json", manifest), ("said.sh", say(1, "exit 0").as_bytes())])?;
    let pet_home = s.scratch.join("sprites/sprites").join(ready(s, api, &owner, &pet).unwrap_or_default());
    let said = |v: u32| records(api, &owner, &pet, "said").iter().filter(|r| r["body"]["v"] == v).count();
    s.ok("its computer syncs the live files at boot and runs start: its record appears", s.eventually(Duration::from_secs(20), || said(1) >= 1), &pet);
    s.ok("start that exits is run again (with backoff)", s.eventually(Duration::from_secs(10), || said(1) >= 2), said(1));
    let log = std::fs::read_to_string(pet_home.join("fragment.log")).unwrap_or_default();
    s.ok("from ~/fragment, its output in ~/fragment.log", pet_home.join("fragment/said.sh").exists() && log.contains("[fragment] exited 0"), &log);
    // awake (a page open), a redeploy that changes the script restarts it
    let page = Socket::open(api, &pet, "__live", Some(&owner), None)?;
    let site = s.scratch.join("runtime-said");
    std::fs::write(site.join("said.sh"), say(2, "exec sleep 600"))?;
    let o = s.cli(api, &home, &["deploy", &pet, "--dir", &dir_of(&site)]);
    let restarted = s.eventually(Duration::from_secs(20), || said(2) == 1);
    let ones = said(1);
    std::thread::sleep(Duration::from_secs(3));
    s.ok(
        "a redeploy that changes the script restarts it: the new record appears, and the old script runs no more",
        o.status.success() && restarted && said(1) == ones && said(2) == 1,
        format!("v1 {ones} then {}, v2 {}", said(1), said(2)),
    );
    page.close();
    Ok(())
}
