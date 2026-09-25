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
    let log = std::fs::File::create(s.scratch.join(format!("sync-watch-{name}.log")))?;
    let mut child = Command::new(&s.cli)
        .args(["sync", &name, "--dir", &dir_of(&dir), "--watch"])
        .env("HOME", &home)
        .env("FRAGMENT_HOST", &api.base)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;
    std::thread::sleep(Duration::from_millis(2500));
    s.commit(&c, &[("remote.md", Some(b"from the server"))]);
    let pulled = s.eventually(Duration::from_secs(15), || std::fs::read_to_string(dir.join("remote.md")).is_ok_and(|t| t == "from the server"));
    s.ok("continuous sync pulls a remote write within seconds", pulled, "");
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
    Ok(())
}
