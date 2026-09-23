//! Large files (slice E): a file of 1 MiB or more is a pointer in git and
//! its bytes a blob. The CLI uploads before it commits and downloads when
//! it pulls; the site serves a pointer's bytes (ranges too); the cell
//! refuses bytes that are not what they claim, and deletes blobs no branch
//! names once their grace period is over.

use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use fragment_core::blob::{self, sha256_hex};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{Api, Call};
use crate::Suite;

fn bytes_of(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect()
}

pub fn blobs(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("blobs") {
        return Ok(());
    }
    let home = s.dir("blobs-home");
    s.cli(api, &home, &["login"]);
    let keys = s.cli_keys(&home).expect("the CLI logged in");
    let name = s.name("blobs");
    let c = s.cli_json(api, &home, &["create", &name, "--json"])?;
    s.hook(api, &c);
    let repo = c["repo"].as_str().unwrap_or("").to_string();
    let dir = s.dir("blobs");
    let dir_of = |p: &Path| p.to_str().expect("utf-8 path").to_string();
    let blob_path = |sha: &str| format!("/api/f/{name}/blobs/{sha}");

    // sync: the bytes go to the blob store, the pointer to git
    let video = bytes_of(3 * 1024 * 1024 + 17, 1);
    let sha = sha256_hex(&video);
    std::fs::create_dir_all(dir.join("site"))?;
    std::fs::write(dir.join("site/clip.bin"), &video)?;
    std::fs::write(dir.join("site/index.html"), "<p>clips</p>")?;
    std::fs::write(dir.join("small.txt"), "small")?;
    let out = s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&dir)]);
    let in_git = s.fake.file_at(&repo, "main", "site/clip.bin").unwrap_or_default();
    s.ok(
        "a large file is a pointer in git",
        out.status.success() && blob::parse(&in_git) == Some(blob::Pointer { sha256: sha.clone(), size: video.len() as u64 }),
        String::from_utf8_lossy(&in_git),
    );
    s.ok("a small file stays in git", s.fake.file_at(&repo, "main", "small.txt").as_deref() == Some(&b"small"[..]), "");
    let r = api.signed(&keys, "GET", &blob_path(&sha), None)?;
    s.ok("the blob store holds the bytes", r.status == 200 && r.bytes == video, format!("{} ({} bytes)", r.status, r.bytes.len()));
    let r = api.signed(&keys, "GET", &format!("/api/f/{name}/files"), None)?;
    let listed = r.body["files"].as_array().and_then(|a| a.iter().find(|f| f["path"] == "site/clip.bin").cloned()).unwrap_or(Value::Null);
    s.ok("the file list shows its real size", listed["size"] == video.len() && listed["blob"] == true, &listed);

    // another folder pulls the real bytes; a folder that has them adopts them
    let other = s.dir("blobs-pull");
    let before = s.fake.commit_pack_count();
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&other)]);
    s.ok("a pull writes the pointer's bytes", std::fs::read(other.join("site/clip.bin")).is_ok_and(|b| b == video), "");
    let out = s.cli(api, &home, &["verify", &name, "--dir", &dir_of(&other), "--json"]);
    let verify: Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    s.ok("verify sees no drift", verify["data"]["conflicts"] == json!([]), String::from_utf8_lossy(&out.stdout));
    let fresh = s.dir("blobs-adopt");
    std::fs::create_dir_all(fresh.join("site"))?;
    std::fs::write(fresh.join("site/clip.bin"), &video)?;
    s.cli(api, &home, &["sync", &name, "--dir", &dir_of(&fresh)]);
    s.ok("a folder holding the same bytes adopts them (nothing committed)", s.fake.commit_pack_count() == before, s.fake.commit_pack_count() - before);

    // the site serves a pointer's bytes, and ranges of them
    let dir_s = dir_of(&dir);
    s.cli(api, &home, &["deploy", &name, "--dir", &dir_s]);
    let site = |range: Option<&str>| {
        api.call(Call {
            method: "GET",
            url: format!("{}?view={}", api.site_url(&name, "clip.bin"), c["viewToken"].as_str().unwrap_or("")),
            extra: range.map(|r| vec![("range", r.to_string())]).unwrap_or_default(),
            ..Call::default()
        })
    };
    let r = site(None)?;
    s.ok("the site serves a pointer's bytes", r.status == 200 && r.bytes == video, format!("{} ({} bytes)", r.status, r.bytes.len()));
    let r = site(Some("bytes=10-19"))?;
    s.ok(
        "and a range of them (206)",
        r.status == 206 && r.bytes == video[10..20] && r.header("content-range") == format!("bytes 10-19/{}", video.len()),
        format!("{} {}", r.status, r.header("content-range")),
    );

    // uploads are checked
    let wrong = bytes_of(1024, 9);
    let claimed = sha256_hex(b"something else");
    let put = |keys: Option<&Keys>, sha: &str, body: Vec<u8>| {
        api.call(Call { method: "PUT", url: format!("{}{}", api.base, blob_path(sha)), body: Some(body), keys, ..Call::default() })
    };
    let r = put(Some(&keys), &claimed, wrong.clone())?;
    s.ok("bytes that are not what they claim are refused", r.status == 400 && r.message().contains("hash to"), &r);
    let r = api.signed(&keys, "HEAD", &blob_path(&claimed), None)?;
    s.ok("and not kept", r.status == 404, &r);
    let r = put(None, &sha256_hex(&wrong), wrong.clone())?;
    s.ok("an unsigned upload is 401", r.status == 401, &r);
    let viewer = Keys::generate();
    api.signed(&keys, "PUT", &format!("/api/f/{name}/members/{}", viewer.pubkey_hex()), Some(&json!({ "role": "viewer" })))?;
    let r = put(Some(&viewer), &sha256_hex(&wrong), wrong.clone())?;
    s.ok("a viewer cannot upload", r.status == 403, &r);
    let r = api.signed(&Keys::generate(), "GET", &blob_path(&sha), None)?;
    s.ok("a stranger cannot read a blob", r.status == 403, &r);

    // a new version: the old bytes go once no branch names them
    let v2 = bytes_of(2 * 1024 * 1024, 2);
    std::fs::write(dir.join("site/clip.bin"), &v2)?;
    s.cli(api, &home, &["deploy", &name, "--dir", &dir_s]);
    let gone = s.eventually(Duration::from_secs(30), || api.signed(&keys, "HEAD", &blob_path(&sha), None).is_ok_and(|r| r.status == 404));
    s.ok("a blob no branch names is deleted after its grace period", gone, "");
    let r = api.signed(&keys, "HEAD", &blob_path(&sha256_hex(&v2)), None)?;
    s.ok("the current version's bytes stay", r.status == 200 && r.header("content-length") == v2.len().to_string(), &r);
    s.ok("the event log says so", api.signed(&keys, "GET", &format!("/api/f/{name}/events"), None)?.text.contains("blobs.collected"), "");
    Ok(())
}
