//! Folder sync through the real CLI against the cell and code.storage:
//! an exit code as a script reads it, continuous sync with the change
//! feed, and the event log's tail. What one pass does (conflicts, a
//! commit whose answer is lost, modes, verify, the mirror source, what
//! syncs, the mass-deletion guard) is host-tested in cli/src/sync.rs
//! against the same code.storage fake, and large files are the `blobs`
//! section's.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use serde_json::{json, Value};

use crate::api::Api;
use crate::Suite;

fn code(out: &std::process::Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

pub fn folder_sync(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("sync") {
        return Ok(());
    }
    let home = s.dir("sync-home");
    s.login(api, &home);
    let create = |s: &Suite, base: &str| -> Result<(String, Value)> {
        let c = s.cli_json(api, &home, &["create", &s.name(base), "--json"])?;
        s.hook(api, &c);
        Ok((c["name"].as_str().unwrap_or("").to_string(), c))
    };
    let dir_of = |p: &Path| p.to_str().expect("utf-8 path").to_string();

    // a both-sides change exits 3 through the process, as a script reads
    // it (cli/src/sync.rs has the rest of a conflict, and each exit code)
    let (name, c) = create(s, "sync-conflict")?;
    let dir = s.dir("sync-conflict");
    std::fs::write(dir.join("doc.md"), "base")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    s.commit(&c, &[("doc.md", Some(b"theirs"))]);
    std::fs::write(dir.join("doc.md"), "ours")?;
    let out = s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    s.ok("a both-sides change exits 3", code(&out) == 3, String::from_utf8_lossy(&out.stdout));

    // continuous: the change feed pulls a remote write; a local edit pushes
    let (name, c) = create(s, "sync-watch")?;
    let dir = s.dir("sync-watch");
    std::fs::write(dir.join("seed.md"), "seed")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    // a file the watcher's first pass pushes, and logs: after that line a
    // remote write reaches the folder only by the change feed (nothing
    // changes locally, and the next sweep is a minute away)
    std::fs::write(dir.join("first.md"), "first")?;
    let log_path = s.scratch.join(format!("sync-watch-{name}.log"));
    let log = std::fs::File::create(&log_path)?;
    let mut child = Command::new(&s.cli)
        .args(["sync", &name, "--dir", &dir_of(&dir), "--watch"])
        .env("HOME", &home)
        .env("FRAGMENT_HOST", &api.base)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    let first_pass = s.eventually(Duration::from_secs(15), || std::fs::read_to_string(&log_path).is_ok_and(|t| t.contains("pushed 1")));
    s.commit(&c, &[("remote.md", Some(b"from the server"))]);
    let pulled = s.eventually(Duration::from_secs(15), || std::fs::read_to_string(dir.join("remote.md")).is_ok_and(|t| t == "from the server"));
    s.ok("continuous sync pulls a remote write within seconds", first_pass && pulled, std::fs::read_to_string(&log_path).unwrap_or_default());
    std::fs::write(dir.join("local.md"), "from the client")?;
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let pushed = s.eventually(Duration::from_secs(15), || s.fake.file_at(&repo, "main", "local.md").as_deref() == Some(&b"from the client"[..]));
    s.ok("continuous sync pushes a local edit", pushed, "");
    // a save is one pass by the watcher (one head read, one listing, one
    // commit), and the feed's echo of its commit starts none: two saves,
    // each waited for, so the first's echo has come before the second
    s.fake.take_requests("");
    let mut saved = true;
    for (file, text) in [("second.md", "two"), ("third.md", "three")] {
        std::fs::write(dir.join(file), text)?;
        saved &= s.eventually(Duration::from_secs(15), || s.fake.file_at(&repo, "main", file).as_deref() == Some(text.as_bytes()));
    }
    let asked = s.fake.take_requests("editor:");
    let expected: std::collections::BTreeMap<String, u32> = [("GET branch", 2), ("GET files/metadata", 2), ("POST commit-pack", 2)].iter().map(|(r, n)| (r.to_string(), *n)).collect();
    s.ok("two saves are two passes of one head read, one listing, and one commit each", saved && asked == expected, format!("{asked:?}"));
    let err = s.scratch.join(format!("sync-watch-second-{name}.log"));
    let mut second = Command::new(&s.cli)
        .args(["sync", &name, "--dir", &dir_of(&dir), "--watch"])
        .env("HOME", &home)
        .env("FRAGMENT_HOST", &api.base)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&err)?)
        .spawn()?;
    // a watcher that is not refused runs forever: give it ten seconds
    let exited = s.eventually(Duration::from_secs(10), || second.try_wait().ok().flatten().is_some());
    if !exited {
        let _ = second.kill();
    }
    let status = second.wait()?;
    let stderr = std::fs::read_to_string(&err).unwrap_or_default();
    s.ok("a second watcher on the folder is refused", exited && !status.success() && stderr.contains("another fragment sync"), &stderr);
    let _ = child.kill();
    let _ = child.wait();

    // the log a failed sync points to: `fragment events --tail` shows the
    // newest events once the log outgrows a page (it showed the oldest
    // page's last few)
    let (name, _) = create(s, "sync-events")?;
    let keys = s.cli_keys(&home).expect("the CLI logged in");
    let page = fragment_proto::limits::EVENTS_PAGE;
    let rotations = page + 100;
    let mut refused = 0;
    for _ in 0..rotations {
        let r = api.signed(&keys, "POST", &format!("/api/f/{name}/rotate"), Some(&json!({ "scopes": ["inbox"] })))?;
        refused += usize::from(r.status != 200);
    }
    let channels = api.signed(&keys, "GET", &format!("/api/f/{name}/channels"), None)?;
    let newest = channels.body["channels"].as_array().and_then(|a| a.iter().find(|c| c["name"] == "events")).and_then(|c| c["seq"].as_i64()).unwrap_or(0);
    let tail = s.cli_json(api, &home, &["events", &name, "--tail", "5", "--json"])?;
    let ids: Vec<i64> = tail["events"].as_array().into_iter().flatten().filter_map(|e| e["id"].as_i64()).collect();
    s.ok(
        &format!("with {} events, --tail 5 shows the newest five, oldest first", rotations + 1),
        refused == 0 && newest > page as i64 && ids == (newest - 4..=newest).collect::<Vec<_>>(),
        json!({ "refused": refused, "newest": newest, "ids": ids }),
    );
    let bad: Vec<u16> = ["tail=0".to_string(), format!("tail={}", page + 1), "tail=x".into(), "tail=5&since=3".into()]
        .iter()
        .map(|q| api.signed(&keys, "GET", &format!("/api/f/{name}/events?{q}"), None).map(|r| r.status).unwrap_or(0))
        .collect();
    s.ok("a tail outside 1-500, or beside since, is refused", bad == [400, 400, 400, 400], json!(bad));
    quiet_poll(s, api)
}

/// The poll backstop (docs/api.md, `FRAGMENT_POLL_INTERVAL_S`): a fragment
/// something outside the platform may write (a storage token was minted
/// for it, or a webhook came, in the last day) is polled every interval,
/// so an editor's push through code.storage syncs within it; one nothing
/// touches is polled once a day.
fn quiet_poll(s: &mut Suite, api: &Api) -> Result<()> {
    let owner = api.person()?;
    let name = s.named(api, &owner, "quiet")?;
    let c = s.create(api, &owner, &name)?;
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let interval = Duration::from_secs(u64::from(crate::POLL_S));
    let interval_ms = i64::from(crate::POLL_S) * 1000;
    let day_ms: i64 = 24 * 3600 * 1000;
    let hook = |op: &str, ms: Option<i64>| -> Value {
        let body = json!({ "fragment": name, "op": op, "ms": ms });
        api.unsigned("POST", "/api/test/fragment", Some(&body)).map(|r| r.body).unwrap_or_default()
    };
    // how far off the next pass (`pollAt`) or the alarm (`alarmAt`) is
    let ahead = |alarm: &Value, key: &str| alarm[key].as_i64().zip(alarm["now"].as_i64()).map_or(i64::MIN, |(at, now)| at - now);
    let read = |path: &str| api.signed(&owner, "GET", &format!("/api/f/{name}/file?path={path}"), None).is_ok_and(|r| r.status == 200);
    // the first request reads the branches once: made now, nothing after it
    // reads them but the poll
    api.signed(&owner, "GET", &format!("/api/f/{name}/files"), None)?;

    let quiet = s.eventually(interval * 5, || ahead(&hook("alarm", None), "pollAt") > day_ms - 60_000);
    let alarm = hook("alarm", None);
    s.ok(
        "a fragment nothing outside the platform writes is polled once a day: its next pass and its alarm are a day away",
        quiet && ahead(&alarm, "alarmAt") > day_ms - 60_000,
        &alarm,
    );
    let reads = s.fake.requests(&repo, "GET branch");
    s.fake.silent_commit(&repo, "main", &[("quiet.md", Some(b"no webhook"))], "silent");
    std::thread::sleep(interval * 4);
    let asked = s.fake.requests(&repo, "GET branch") - reads;
    s.ok(
        "and asks code.storage nothing for four poll intervals: a commit no webhook announced waits",
        asked == 0 && !read("quiet.md"),
        format!("{asked} branch reads"),
    );

    // an editor who may push through code.storage: a storage token
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/storage-token"), None)?;
    let found = s.eventually(interval * 5, || read("quiet.md"));
    let alarm = hook("alarm", None);
    s.ok(
        "a storage token minted brings the poll back to its interval, which finds a commit no webhook announced",
        r.status == 200 && found && ahead(&alarm, "pollAt") <= interval_ms,
        &alarm,
    );
    let reads = s.fake.requests(&repo, "GET branch");
    std::thread::sleep(interval * 4);
    println!("      busy, {} branch reads in four poll intervals; quiet, none", s.fake.requests(&repo, "GET branch") - reads);
    hook("age-outside", Some(day_ms));
    let quiet = s.eventually(interval * 5, || ahead(&hook("alarm", None), "pollAt") > day_ms - 60_000);
    s.ok("a day after the token, it is polled once a day again", quiet, hook("alarm", None));
    s.commit(&c, &[("announced.md", Some(b"by webhook"))]);
    let alarm = hook("alarm", None);
    s.ok("a webhook brings the poll back to its interval too", read("announced.md") && ahead(&alarm, "pollAt") <= interval_ms, &alarm);
    Ok(())
}
