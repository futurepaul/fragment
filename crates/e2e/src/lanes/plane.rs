//! The file plane: storage tokens, pins moved by webhooks, the refresh
//! route, and the poll backstop; then the CLI's deploy, preview, rollback,
//! and drafts against the cell.

use std::time::Duration;

use anyhow::Result;
use base64::Engine;
use fragment_nip98::Keys;
use fragment_proto::{limits, ErrorCode};
use serde_json::{json, Value};

use crate::api::{now_s, second_start, Api, Call};
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
    s.ok("it lives exactly a storage token's lifetime", claims["exp"].as_i64().unwrap_or(0) - claims["iat"].as_i64().unwrap_or(0) == limits::STORAGE_TOKEN_TTL_S, &claims);
    s.ok("it names who minted it (their identity)", claims["sub"] == format!("editor:{}", api.identity(&owner)?), &claims);
    let http = reqwest::blocking::Client::new();
    let r = http.get(format!("{}/api/repos/{repo}/branch?name=main", s.fake.url)).bearer_auth(&token).send()?;
    let (st, why) = (r.status().as_u16(), r.json::<Value>().map(|b| b["detail"].clone()).unwrap_or_default());
    s.ok("code.storage accepts it (a fresh repo has no main: 404, branch not found)", st == 404 && why == "branch not found", format!("{st} {why}"));

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
    // the window's edges, on either side of now: a delivery stamped the
    // window ahead is taken and one stamped a second past it behind is
    // refused, however late the node reads its clock (it shares ours);
    // sent as a second begins, the first is judged at the edge itself
    let window = limits::WEBHOOK_WINDOW_S;
    let edge = json!({ "repository": { "url": repo }, "ref": "refs/heads/main", "before": "0", "after": "1", "pushed_at": "t0" });
    let now = second_start();
    let ahead = signed_webhook(api, &name, &secret, &edge, now + window, "push")?;
    let behind = signed_webhook(api, &name, &secret, &body, now - window - 1, "push")?;
    s.ok(
        "a webhook stamped at the edge of the window is taken, and one a second past it is 401",
        ahead.status == 200 && ahead.body["interpreted"] == true && behind.code() == Some(ErrorCode::Unauthenticated),
        format!("{ahead} {behind}"),
    );
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
    if !s.section("files", &[crate::Need::Fakes]) {
        return Ok(());
    }
    files_lane(s, api)
}

fn text(out: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

pub fn deploy(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("deploy", &[]) {
        return Ok(());
    }
    let home = s.dir("deploy-home");
    let site = s.dir("deploy-site");
    std::fs::create_dir_all(site.join("site"))?;
    s.login(api, &home);
    // a fragment's tokens are credentials: shown only on request
    let plain = s.cli_json(api, &home, &["create", &s.name("deploy-plain"), "--json"])?;
    s.ok(
        "create --json leaves out the tokens unless asked",
        plain["name"].is_string() && ["viewToken", "inboxToken", "webhookSecret"].iter().all(|t| plain.get(t).is_none()),
        &plain,
    );
    let out = s.cli(api, &home, &["create", &s.name("deploy-human")]);
    s.ok("and create prints none, pointing at `fragment open`", out.status.success() && !text(&out).contains("?view=") && !text(&out).contains("?t=") && text(&out).contains("fragment open"), text(&out));
    let created = s.cli_json(api, &home, &["create", &s.name("deploy"), "--show-tokens", "--json"])?;
    s.ok("--show-tokens shows them", created["viewToken"].is_string() && created["webhookSecret"].is_string(), &created);
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

    // the deploy's preview card, shot after it, never in its request (decision 31)
    let keys = s.cli_keys(&home).unwrap_or_else(Keys::generate);
    let live1 = st["pins"]["live"].as_str().unwrap_or("").to_string();
    let cards = s.shoots_cards();
    let first = match cards {
        true => {
            let first = super::site::card_showing(s, api, &keys, &name, &live1);
            s.ok("the deploy makes a preview card of the live it deployed", first.as_ref().is_some_and(super::site::is_card), super::site::card_detail(&first));
            let made = super::site::event_kinds(api, &keys, &name).iter().filter(|k| *k == "card.made").count();
            s.ok("one card for one deploy", made == 1, made);
            first
        }
        false => {
            let skipped = s.eventually(Duration::from_secs(20), || super::site::event_kinds(api, &keys, &name).iter().any(|k| k == "card.skipped"));
            s.ok("with no browser, a deploy shoots no card, and its events say so", skipped, super::site::event_kinds(api, &keys, &name).join(","));
            None
        }
    };

    std::fs::write(site.join("site/index.html"), "<h1>v2 marker</h1>")?;
    s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().unwrap()]);
    let st2 = s.cli_json(api, &home, &["status", &name, "--json"])?;
    s.ok("a second deploy moves live", st2["pins"]["live"] != st["pins"]["live"], &st2);
    s.ok("the site follows live", page(api).contains("v2 marker"), page(api));
    let second = match cards {
        true => super::site::card_showing(s, api, &keys, &name, st2["pins"]["live"].as_str().unwrap_or("")),
        false => None,
    };
    let (tag1, tag2) = (first.as_ref().map(|r| r.header("etag")).unwrap_or_default(), second.as_ref().map(|r| r.header("etag")).unwrap_or_default());
    let conditional = |tag: &str| {
        api.call(Call { method: "GET", url: format!("{}/api/f/{name}/card", api.base), keys: Some(&keys), extra: vec![("if-none-match", tag.to_string())], ..Call::default() })
    };
    match cards {
        true => {
            s.ok(
                "a second deploy replaces the card: another image, of the new live",
                second.as_ref().is_some_and(super::site::is_card) && tag2 != tag1 && second.as_ref().map(|r| &r.bytes) != first.as_ref().map(|r| &r.bytes),
                json!({ "first": super::site::card_detail(&first), "second": super::site::card_detail(&second) }),
            );
            let (same, old) = (conditional(&tag2)?, conditional(&tag1)?);
            s.ok("the card revalidates by its tag: 304 for the current one, the new image for the one before", same.status == 304 && same.bytes.is_empty() && old.status == 200 && old.header("etag") == tag2, format!("{} | {}", same.status, old.status));
        }
        false => {
            let why = "this node has no browser (celld: no Browser Rendering), so it shoots no cards";
            s.skip("a second deploy replaces the card: another image, of the new live", why);
            s.skip("the card revalidates by its tag: 304 for the current one, the new image for the one before", why);
        }
    }
    let repo = created["repo"].as_str().unwrap_or("");
    match s.hosted() {
        true => s.skip("code.storage holds live where the CLI moved it", "it reads the code.storage fake's branches (a preview's git is real)"),
        false => s.ok("code.storage holds live where the CLI moved it", s.fake.branch(repo, "live").as_deref() == st2["pins"]["live"].as_str(), ""),
    }

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
    match s.hosted() {
        true => s.skip("the preview ref is ephemeral at main's tip", "it reads the code.storage fake's refs (a preview's git is real)"),
        false => s.ok("the preview ref is ephemeral at main's tip", s.fake.is_ephemeral(repo, &slug) && s.fake.branch(repo, &slug) == s.fake.branch(repo, "main"), &slug),
    }
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

    // a deploy whose code the platform refuses says so and fails
    std::fs::write(site.join("fragment.json"), include_str!("../../fixtures/todo.json").replacen("\"add_todo\"", "\"Add-Todo\"", 1))?;
    let out = s.cli(api, &home, &["deploy", &name, "--dir", site.to_str().unwrap()]);
    s.ok(
        "a deploy whose code is refused exits non-zero, naming why",
        !out.status.success() && text(&out).contains("refused its code") && text(&out).contains("Add-Todo"),
        text(&out),
    );
    let st6 = s.cli_json(api, &home, &["status", &name, "--json"])?;
    s.ok("and the last good code keeps serving", st6["code"]["sha"] == st5["code"]["sha"] && st6["pins"]["live"] != st5["pins"]["live"], &st6["code"]);
    Ok(())
}
