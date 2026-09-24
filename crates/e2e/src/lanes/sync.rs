//! Folder sync through the CLI against the cell and code.storage:
//! conflicts, modes, verify, the mirror source, the mass-deletion guard,
//! chunked large files, and continuous sync with the change feed.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

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

    // both sides changed: local keeps, the remote copy lands beside it, exit 3
    let (name, c) = create(s, "sync-conflict")?;
    let dir = s.dir("sync-conflict");
    std::fs::write(dir.join("doc.md"), "base")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    s.commit(&c, &[("doc.md", Some(b"theirs"))]);
    std::fs::write(dir.join("doc.md"), "ours")?;
    let out = s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    s.ok("a both-sides change exits 3", code(&out) == 3, String::from_utf8_lossy(&out.stdout));
    s.ok("the local file keeps ours", std::fs::read_to_string(dir.join("doc.md"))? == "ours", "");
    let copy = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).find(|n| n.contains(".conflict-"));
    s.ok("the remote copy is saved beside it", copy.as_deref().is_some_and(|n| std::fs::read_to_string(dir.join(n)).is_ok_and(|t| t == "theirs")), format!("{copy:?}"));
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    s.ok("the remote is untouched by our conflict", s.fake.file_at(&repo, "main", "doc.md").as_deref() == Some(&b"theirs"[..]), "");

    // a commit that landed but lost its answer: sync never resends it
    // blind (the old client did: a 409, then a conflict copy of its own
    // bytes, exit 3); it rereads the branch and adopts what landed
    let (name, c) = create(s, "sync-lost")?;
    let dir = s.dir("sync-lost");
    std::fs::write(dir.join("doc.md"), "base")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    std::fs::write(dir.join("doc.md"), "ours, changed")?;
    std::fs::write(dir.join("new.md"), "new")?;
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    s.fake.drop_commit_answers(&repo, 1);
    let out = s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    let said = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    s.ok("a commit whose answer is lost is reread, not resent, and the sync exits 0", code(&out) == 0 && said.contains("answer was lost"), &said);
    let copies = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".conflict-")).count();
    s.ok("it leaves no conflict copy", copies == 0, copies);
    s.ok(
        "what it committed is in the repo",
        s.fake.file_at(&repo, "main", "doc.md").as_deref() == Some(&b"ours, changed"[..]) && s.fake.file_at(&repo, "main", "new.md").as_deref() == Some(&b"new"[..]),
        format!("{:?}", s.fake.paths(&repo, "main")),
    );
    let again = s.cli_json(api, &home, &["sync", &name, "--dir", &dir_of(&dir), "--json"])?;
    s.ok("the next pass has nothing to do", again["pushed"] == serde_json::json!([]) && again["conflicts"] == serde_json::json!([]), &again);

    // pull withholds deletions; --prune applies them
    let (name, c) = create(s, "sync-mode")?;
    let dir = s.dir("sync-mode");
    std::fs::write(dir.join("keep.md"), "keep")?;
    std::fs::write(dir.join("drop.md"), "drop")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    s.commit(&c, &[("drop.md", None)]);
    let out = s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir), "--mode", "pull", "--json"]);
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    s.ok("pull mode withholds a remote deletion", text.contains("withheld") && dir.join("drop.md").exists(), &text);
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir), "--mode", "pull", "--prune"]);
    s.ok("--prune applies it", !dir.join("drop.md").exists() && dir.join("keep.md").exists(), "");

    // verify: in sync, then drift
    let (name, _) = create(s, "sync-verify")?;
    let dir = s.dir("sync-verify");
    std::fs::write(dir.join("a.md"), "aaa")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    let out = s.cli(api, &home, &["verify", &name, "--dir", &dir_of(&dir)]);
    s.ok("verify exits 0 in sync", code(&out) == 0, String::from_utf8_lossy(&out.stdout));
    std::fs::write(dir.join("a.md"), "tampered")?;
    let out = s.cli(api, &home, &["verify", &name, "--dir", &dir_of(&dir)]);
    s.ok("verify exits 3 on drift", code(&out) == 3, String::from_utf8_lossy(&out.stdout));

    // --mirror-from overlays a read-only source
    let (name, c) = create(s, "sync-mirror")?;
    let src = s.dir("sync-mirror-src");
    let dir = s.dir("sync-mirror");
    std::fs::write(src.join("a.md"), "one")?;
    std::fs::create_dir_all(src.join("sub"))?;
    std::fs::write(src.join("sub/b.md"), "two")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir), "--mirror-from", &dir_of(&src)]);
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let paths = s.fake.paths(&repo, "main");
    s.ok("the mirror source lands in the repo", paths.contains(&"a.md".into()) && paths.contains(&"sub/b.md".into()), format!("{paths:?}"));
    std::fs::write(src.join("c.md"), "three")?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir), "--mirror-from", &dir_of(&src)]);
    s.ok("new source files arrive on later passes", s.fake.paths(&repo, "main").contains(&"c.md".into()), "");
    s.ok("the source is never written", !src.join(".fragment").exists(), "");

    // the mass-deletion guard
    let (name, c) = create(s, "sync-guard")?;
    let dir = s.dir("sync-guard");
    for i in 0..20 {
        std::fs::write(dir.join(format!("f{i}.md")), "x")?;
    }
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    for i in 0..15 {
        std::fs::remove_file(dir.join(format!("f{i}.md")))?;
    }
    let out = s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    s.ok("a mass deletion trips the guard (exit 4)", code(&out) == 4, String::from_utf8_lossy(&out.stdout));
    s.ok("the guard kept the remote files", s.fake.paths(&repo, "main").len() == 20, s.fake.paths(&repo, "main").len());
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir), "--apply-mass-delete"]);
    s.ok("--apply-mass-delete proceeds", s.fake.paths(&repo, "main").len() == 5, s.fake.paths(&repo, "main").len());

    // a file of 1 MiB or more: its bytes a blob, a pointer in git (section `blobs` has the rest)
    let (name, c) = create(s, "sync-big")?;
    let dir = s.dir("sync-big");
    let big: Vec<u8> = (0..5 * 1024 * 1024 + 1234).map(|i| (i % 251) as u8).collect();
    std::fs::write(dir.join("big.bin"), &big)?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let in_git = s.fake.file_at(&repo, "main", "big.bin").unwrap_or_default();
    let pointer = fragment_core::blob::parse(&in_git);
    s.ok(
        "a 5 MiB file syncs as a pointer to its bytes",
        pointer.is_some_and(|p| p.size == big.len() as u64 && p.sha256 == fragment_core::blob::sha256_hex(&big)),
        String::from_utf8_lossy(&in_git),
    );

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
    Ok(())
}
