//! Files as an app's state (slice E): reads at `main` through the app's
//! capability, writes from mutations and jobs as the cell's own commits,
//! compare-and-swap, file triggers in the hop budget, and one loaded
//! worker per fragment.

use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use super::app::ship;
use super::jobs::{settle, started};
use crate::Suite;

const FILES_APP: &[u8] = include_bytes!("../../fixtures/files.mjs");
const FILES_JSON: &[u8] = include_bytes!("../../fixtures/files.json");

pub fn appfiles(s: &mut Suite, api: &crate::api::Api) -> Result<()> {
    if !s.section("appfiles") {
        return Ok(());
    }
    let owner = api.person()?;
    let name = s.named(api, &owner, "appfiles")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, FILES_APP, FILES_JSON);
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let long = Duration::from_secs(40);
    let call = |op: &str, id: &str, input: Value| api.op(&owner, &name, op, id, input);

    // a mutation writes a file to main, once
    let before = s.fake.commit_pack_count();
    let r = call("add_note", "n1", json!({ "slug": "hello", "text": "# Hello\n" }))?;
    s.ok("a mutation's file write answers once it is committed", r.status == 200 && r.body["result"]["path"] == "notes/hello.md", &r);
    s.ok("the file is on main", s.fake.file_at(&repo, "main", "notes/hello.md").as_deref() == Some(&b"# Hello\n"[..]), "");
    let r = call("add_note", "n1", json!({ "slug": "hello", "text": "# Hello\n" }))?;
    s.ok("a replay commits nothing again", r.body["replayed"] == true && s.fake.commit_pack_count() == before + 1, &r);
    let r = call("read", "q1", json!({ "path": "notes/hello.md" }))?;
    s.ok("a query reads it at once (the pin followed the commit)", r.body["result"]["text"] == "# Hello\n", &r);
    let r = call("read", "q2", json!({ "path": "notes/none.md" }))?;
    s.ok("reading an absent file is null", r.status == 200 && r.body["result"]["text"].is_null(), &r);
    let r = call("pair", "p1", json!({ "a": "A" }))?;
    let after_pair = s.fake.commit_pack_count();
    s.ok("two files in one mutation are one commit", r.status == 200 && after_pair == before + 2, format!("{before} → {after_pair}"));
    let r = call("bytes", "q3", json!({ "path": "pair/b.bin" }))?;
    s.ok("binary files round-trip", r.body["result"]["bytes"] == json!([0, 159, 146, 150]), &r);
    let r = call("list", "q4", json!({ "prefix": "pair/" }))?;
    s.ok("list reads a prefix of main's tree", r.body["result"] == json!([{ "path": "pair/a.txt", "size": 1 }, { "path": "pair/b.bin", "size": 4 }]), &r);
    let r = call("stat", "q5", json!({ "path": "pair/a.txt" }))?;
    s.ok("stat names the file's blob", r.body["result"]["size"] == 1 && r.body["result"]["sha"].as_str().is_some_and(|x| x.len() == 40), &r);
    let r = call("bad_path", "b1", json!({}))?;
    s.ok("a path that climbs out is refused in the mutation", r.status == 422 && r.message().contains("not a file path"), &r);
    let r = call("drop_note", "d1", json!({ "slug": "hello" }))?;
    s.ok("a mutation removes a file", r.status == 200 && s.fake.file_at(&repo, "main", "notes/hello.md").is_none(), &r);
    s.commit(&c, &[("big.bin", Some(&vec![7u8; 1024 * 1024 + 1]))]);
    let r = call("read", "q6", json!({ "path": "big.bin" }))?;
    s.ok("a file over 1 MiB is not read into the app", r.status == 422 && r.message().contains("serve larger files from the site"), &r);

    // jobs: reads and writes as steps, compare-and-swapped
    let r = call("append", "a1", json!({ "line": "one" }))?;
    let one = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    let r = call("append", "a2", json!({ "line": "two" }))?;
    let two = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    s.ok(
        "a job reads, then writes what it read with a compare-and-swap",
        one["output"]["lines"] == 1 && two["output"]["lines"] == 2 && s.fake.file_at(&repo, "main", "log.txt").as_deref() == Some(&b"one\ntwo\n"[..]),
        format!("{one} {two}"),
    );
    let r = call("stale", "s1", json!({ "expect": "0000000000000000000000000000000000000000" }))?;
    let stale = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    s.ok(
        "a write that names a stale blob fails its step with a conflict",
        stale["output"]["name"] == "StepError" && stale["output"]["conflict"].as_str().is_some_and(|m| m.contains("conflict: log.txt")),
        &stale,
    );
    let r = call("stale", "s2", json!({ "expect": null }))?;
    let exists = settle(api, &owner, &name, started(&r), &["succeeded", "held"], long);
    s.ok("a create-only write (expect null) of an existing file conflicts", exists["output"]["conflict"].as_str().is_some_and(|m| m.contains("expected absent")), &exists);
    s.ok("the conflicts wrote nothing", s.fake.file_at(&repo, "main", "log.txt").as_deref() == Some(&b"one\ntwo\n"[..]), "");

    // a file trigger that writes what it watches runs into the hop budget
    s.commit(&c, &[("loop/0.txt", Some(b"0"))]);
    let stopped = s.eventually(Duration::from_secs(90), || {
        let runs = |status: &str| {
            api.signed(&owner, "GET", &format!("/api/f/{name}/runs?op=again&status={status}"), None)
                .map(|r| r.body["runs"].as_array().map_or(0, |a| a.len()))
                .unwrap_or(0)
        };
        runs("blocked") > 0 && runs("running") == 0 && runs("queued") == 0
    });
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/runs?op=again&limit=100"), None)?;
    let runs = r.body["runs"].as_array().cloned().unwrap_or_default();
    s.ok(
        "a job writing the files that trigger it stops at the hop budget",
        stopped && runs.iter().filter(|r| r["status"] == "succeeded").count() == 17 && runs[0]["depth"] == 17 && runs[0]["status"] == "blocked",
        json!(runs.iter().map(|r| (r["depth"].clone(), r["status"].clone())).collect::<Vec<_>>()),
    );

    // one loaded worker per fragment: the same code reads its own files
    let other = s.named(api, &owner, "appfiles2")?;
    let c2 = s.create(api, &owner, &other)?;
    ship(s, &c2, FILES_APP, FILES_JSON);
    s.commit(&c, &[("whoami.txt", Some(b"first"))]);
    s.commit(&c2, &[("whoami.txt", Some(b"second"))]);
    let a = api.op(&owner, &name, "whoami", "w1", json!({}))?;
    let b = api.op(&owner, &other, "whoami", "w1", json!({}))?;
    s.ok("two fragments running the same code each read their own files", a.body["result"]["text"] == "first" && b.body["result"]["text"] == "second", format!("{a} {b}"));
    Ok(())
}
