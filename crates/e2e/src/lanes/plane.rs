//! The file plane: storage tokens, pins moved by webhooks, the refresh
//! route, and the poll backstop; then the CLI's deploy, preview, rollback,
//! and drafts against the cell.

use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use crate::api::{now_s, Api, Call};
use crate::Suite;

fn listing(api: &Api, keys: &Keys, name: &str) -> Vec<(String, u64)> {
    api.signed(keys, "GET", &format!("/api/f/{name}/files"), None)
        .ok()
        .and_then(|r| r.body["files"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|f| (f["path"].as_str().unwrap_or("").to_string(), f["size"].as_u64().unwrap_or(0)))
        .collect()
}

fn read(api: &Api, keys: &Keys, name: &str, path: &str) -> Option<String> {
    let r = api.signed(keys, "GET", &format!("/api/f/{name}/file?path={path}"), None).ok()?;
    (r.status == 200).then_some(r.text)
}

fn signed_webhook(api: &Api, name: &str, secret: &str, body: &Value, at: i64, event: &str) -> Result<crate::api::Reply> {
    let bytes = body.to_string().into_bytes();
    api.call(Call {
        method: "POST",
        url: format!("{}/api/f/{name}/webhook", api.base),
        body: Some(bytes.clone()),
        content_type: Some("application/json"),
        extra: vec![("x-pierre-event", event.into()), ("x-pierre-signature", fragment_fakes::codestorage::signature(&bytes, secret, at))],
        ..Call::default()
    })
}

fn files_lane(s: &mut Suite, api: &Api) -> Result<()> {
    let owner = api.person()?;
    let name = s.named(api, &owner, "files")?;
    let c = s.create(api, &owner, &name)?;
    let repo = c["repo"].as_str().unwrap_or("").to_string();

    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/storage-token"), None)?;
    let token = r.body["token"].as_str().unwrap_or("").to_string();
    let claims: Value = token
        .split('.')
        .nth(1)
        .and_then(|p| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    s.ok("a storage token names the fragment's repo", r.status == 200 && r.body["repo"] == repo.as_str() && claims["repo"] == repo.as_str(), &r);
    s.ok("its scopes are git read and write only", claims["scopes"] == json!(["git:read", "git:write"]), &claims);
    s.ok("it lives fifteen minutes at most", claims["exp"].as_i64().unwrap_or(0) - claims["iat"].as_i64().unwrap_or(0) <= 900, &claims);
    s.ok("it names who minted it (their identity)", claims["sub"] == format!("editor:{}", api.identity(&owner)?), &claims);
    let http = reqwest::blocking::Client::new();
    let st = http.get(format!("{}/api/repos/{repo}/branch?name=main", s.fake.url)).bearer_auth(&token).send()?.status().as_u16();
    s.ok("code.storage accepts it (a fresh repo has no main: 404)", st == 404, st);

    s.commit(&c, &[("notes/a.md", Some(b"hello v1\n"))]);
    s.ok("a pushed file is listed after the webhook", listing(api, &owner, &name).contains(&("notes/a.md".into(), 9)), "");
    s.ok("the file reads through the cell", read(api, &owner, &name, "notes/a.md").as_deref() == Some("hello v1\n"), "");
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/file/stat?path=notes/a.md"), None)?;
    s.ok(
        "stat reports the blob identity",
        r.body["stat"]["present"] == true && r.body["stat"]["blobSha"].as_str().is_some_and(|b| b.len() == 40),
        &r,
    );
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/file/stat?path=notes/nope.md"), None)?;
    s.ok("stat of an absent path reports present: false", r.body["stat"]["present"] == false, &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/file?path=../etc/passwd"), None)?;
    s.ok("a path outside the repo is 400", r.status == 400, &r);
    s.commit(&c, &[("notes/a.md", Some(b"hello v2\n"))]);
    s.ok("reads follow the next push", read(api, &owner, &name, "notes/a.md").as_deref() == Some("hello v2\n"), "");
    s.commit(&c, &[("notes/a.md", None)]);
    s.ok("a deleted file reads 404", read(api, &owner, &name, "notes/a.md").is_none(), "");
    s.ok("and leaves the listing", !listing(api, &owner, &name).iter().any(|(p, _)| p == "notes/a.md"), "");
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/manifest"), None)?;
    s.ok("no fragment.json at main is 404", r.status == 404, &r);
    s.commit(&c, &[("fragment.json", Some(br#"{"name":"x","visibility":"public","editors":["npub1x"]}"#))]);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/manifest"), None)?;
    s.ok("manifest answers fragment.json at main", r.status == 200 && r.body["visibility"] == "public", &r);
    let r = api.status(&owner, &name)?;
    s.ok("fragment.json's visibility grants nothing", r.body["visibility"] == "link", &r);
    let r = api.signed(&owner, "GET", &format!("/api/f/{name}/events"), None)?;
    s.ok("the event log says fragment.json's access keys are ignored", r.text.contains("manifest.ignored"), &r);

    // deliveries
    let secret = c["webhookSecret"].as_str().unwrap_or("").to_string();
    let body = json!({ "repository": { "url": repo }, "ref": "refs/heads/main", "before": "0", "after": "1", "pushed_at": "t1" });
    let r = signed_webhook(api, &name, "wrong-secret", &body, now_s(), "push")?;
    s.ok("a webhook with a bad signature is 401", r.status == 401, &r);
    let r = signed_webhook(api, &name, &secret, &body, now_s() - 3600, "push")?;
    s.ok("a stale webhook is 401", r.status == 401, &r);
    let r = signed_webhook(api, &name, &secret, &body, now_s(), "push")?;
    s.ok("a signed webhook is interpreted", r.status == 200 && r.body["interpreted"] == true, &r);
    let r = signed_webhook(api, &name, &secret, &body, now_s(), "push")?;
    s.ok("its redelivery is acknowledged, not interpreted", r.status == 200 && r.body["redelivery"] == true, &r);
    let r = signed_webhook(api, &name, &secret, &json!({}), now_s(), "repo.created")?;
    s.ok("other events are ignored", r.status == 200 && r.body["ignored"] == "repo.created", &r);

    // lost webhooks
    s.fake.silent_commit(&repo, "main", &[("quiet.md", Some(b"no webhook"))], "silent");
    let r = api.signed(&owner, "POST", &format!("/api/f/{name}/refresh"), Some(&json!({})))?;
    s.ok("refresh moves main at once", r.status == 200 && r.body["refs"]["main"]["moved"] == true && r.body["refs"]["live"]["absent"] == true, &r);
    s.ok("refresh made the file visible", read(api, &owner, &name, "quiet.md").as_deref() == Some("no webhook"), "");
    s.fake.silent_commit(&repo, "main", &[("polled.md", Some(b"found by the poll"))], "silent");
    let polled = s.eventually(Duration::from_secs(u64::from(crate::POLL_S) * 5), || read(api, &owner, &name, "polled.md").is_some());
    s.ok("the poll backstop finds a commit no webhook announced", polled, "");
    let stranger = api.person()?;
    let r = api.signed(&stranger, "GET", &format!("/api/f/{name}/files"), None)?;
    s.ok("a stranger cannot list files", r.status == 403, &r);
    Ok(())
}

pub fn files(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("files") {
        return Ok(());
    }
    files_lane(s, api)
}

fn text(out: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

pub fn deploy(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("deploy") {
        return Ok(());
    }
    let home = s.dir("deploy-home");
    let site = s.dir("deploy-site");
    std::fs::create_dir_all(site.join("site"))?;
    s.login(api, &home);
    let created = s.cli_json(api, &home, &["create", &s.name("deploy"), "--json"])?;
    let name = created["name"].as_str().unwrap_or("").to_string();
    s.hook(api, &created);
    let view = created["viewToken"].as_str().unwrap_or("").to_string();
    let cookie = format!("fragview={view}");
    let page = |api: &Api| api.page(&name, "", Some(&cookie)).map(|r| r.text).unwrap_or_default();

    std::fs::write(site.join("site/index.html"), "<h1>v1 marker</h1>")?;
    let out = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().unwrap(), "--note", "first"]);
    s.ok("deploy prints the fragment's own origin", text(&out).contains(&format!("live: {}", api.site_url(&name, "").trim_end_matches('/'))), text(&out));
    let st = s.cli_json(api, &home, &["status", &name, "--json"])?;
    s.ok("after a deploy live is main's tip", st["pins"]["live"].is_string() && st["pins"]["live"] == st["pins"]["main"], &st);
    s.ok("the site serves the deploy", page(api).contains("v1 marker"), page(api));

    std::fs::write(site.join("site/index.html"), "<h1>v2 marker</h1>")?;
    s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().unwrap()]);
    let st2 = s.cli_json(api, &home, &["status", &name, "--json"])?;
    s.ok("a second deploy moves live", st2["pins"]["live"] != st["pins"]["live"], &st2);
    s.ok("the site follows live", page(api).contains("v2 marker"), page(api));
    let repo = created["repo"].as_str().unwrap_or("");
    s.ok("code.storage holds live where the CLI moved it", s.fake.branch(repo, "live").as_deref() == st2["pins"]["live"].as_str(), "");

    let out = s.cli(api, &home, &["drafts", &name]);
    let drafts = text(&out);
    s.ok("drafts lists the deploys, live marked", drafts.contains("[live]") && drafts.lines().filter(|l| l.len() > 8 && l.as_bytes()[8] == b' ').count() >= 2, &drafts);
    let out = s.cli(api, &home, &["rollback", &name]);
    s.ok("rollback reports the move", text(&out).contains("rolled back"), text(&out));
    s.ok("the site serves the rolled-back content", s.eventually(Duration::from_secs(10), || page(api).contains("v1 marker")), page(api));
    let st3 = s.cli_json(api, &home, &["status", &name, "--json"])?;
    s.ok("rollback is a new live commit", st3["pins"]["live"] != st2["pins"]["live"], &st3);

    let out = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().unwrap(), "--preview"]);
    let slug = text(&out).split_whitespace().find(|w| w.starts_with("preview/")).map(str::to_string).unwrap_or_default();
    s.ok("a preview names its ephemeral ref", !slug.is_empty(), text(&out));
    s.ok("the preview ref is ephemeral at main's tip", s.fake.is_ephemeral(repo, &slug) && s.fake.branch(repo, &slug) == s.fake.branch(repo, "main"), &slug);
    let st4 = s.cli_json(api, &home, &["status", &name, "--json"])?;
    s.ok("a preview leaves live alone", st4["pins"]["live"] == st3["pins"]["live"] && page(api).contains("v1 marker"), &st4);

    // code arrives with a deploy
    std::fs::write(site.join("app.mjs"), include_str!("../../fixtures/todo.mjs"))?;
    std::fs::write(site.join("fragment.json"), include_str!("../../fixtures/todo.json"))?;
    s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().unwrap()]);
    let st5 = s.cli_json(api, &home, &["status", &name, "--json"])?;
    s.ok(
        "the app installs from the live commit",
        st5["code"]["sha"] == st5["pins"]["live"] && st5["code"]["operations"]["add_todo"]["kind"] == "mutation",
        &st5["code"],
    );
    if let Some(keys) = s.cli_keys(&home) {
        let r = api.op(&keys, &name, "add_todo", "d1", json!({ "text": "deployed" }))?;
        s.ok("the deployed app answers", r.status == 200 && r.body["result"]["id"] == 1, &r);
    } else {
        s.ok("the CLI's key is readable for the op check", false, home.display());
    }
    Ok(())
}
