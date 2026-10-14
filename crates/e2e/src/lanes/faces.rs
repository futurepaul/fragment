//! An agent's name and picture (docs/api.md, "An agent's name and
//! picture"): its agent fragment's title and `avatar.png` at live, the one
//! source everything shows it by. Renamed and given a picture through the
//! routes Settings and `fragment agent` use (its files, then a deploy), it
//! reaches its owner's list (what the shell draws), a chat's `__people`
//! (what its page draws), the share sheet, the access view and its
//! picture's own address. The same change again commits nothing new and
//! changes no address (replay); a picture too large, one that is no
//! picture (written, or pushed through git), a title too long, and someone
//! else's agent are refused, the last good face kept; taken away, the
//! picture is gone everywhere. A node's restart keeps it (restart.rs).

use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine;
use fragment_nip98::Keys;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::signin::{site_cookie, with_session};
use super::templates::person;
use crate::api::{Api, Call, Reply};
use crate::Suite;

const WAIT: Duration = Duration::from_secs(30);

/// Two pictures, 16×16 PNGs: orange and blue.
const ORANGE: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR42mO46a9NEmIY1TCqYfhqAADneVMQLy52+gAAAABJRU5ErkJggg==";
const BLUE: &str = "iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR42mPQrrpJEmIY1TCqYfhqAABnSH4QybTRQgAAAABJRU5ErkJggg==";

/// A picture's bytes from its base64.
pub(super) fn picture(b64: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD.decode(b64).expect("a fixture PNG")
}

/// The orange picture, for another lane's agent.
pub(super) fn orange() -> Vec<u8> {
    picture(ORANGE)
}

/// Where an identity's picture of `bytes` is served (fragment_core::face).
pub(super) fn picture_path(identity: &str, bytes: &[u8]) -> String {
    fragment_core::face::picture_path(identity, &hex::encode(Sha256::digest(bytes)))
}

/// An agent of `owner`'s: a fragment on the agent template titled `title`,
/// run by their computer (which gives it its identity, and is never woken
/// here). Its fragment and its identity.
pub(super) fn agent_of(s: &Suite, api: &Api, owner: &Keys, base: &str, title: &str) -> Result<(Value, String)> {
    let name = s.named(api, owner, base)?;
    let r = api.create_with(owner, json!({ "name": name, "template": "agent", "title": title }))?;
    anyhow::ensure!(r.status == 200, "the agent fragment: {r}");
    s.owned(&r.body, owner);
    let r = api.signed(owner, "POST", "/api/computers", Some(&json!({})))?;
    let computer = r.body["computer"].as_str().with_context(|| format!("a computer: {r}"))?.to_string();
    let r = api.signed(owner, "PUT", &format!("/api/computers/{computer}/agents/{name}"), Some(&json!({})))?;
    let id = r.body["agents"].as_array().into_iter().flatten().find(|a| a["fragment"] == name.as_str()).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    anyhow::ensure!(fragment_core::npub::is_identity(&id), "its computer took it: {r}");
    let made = api.status(owner, &name)?.body;
    Ok((json!({ "name": name, "repo": made["repo"] }), id))
}

/// An agent's name and picture as Settings and `fragment agent` write them:
/// its fragment.json with `title` (when one), its `avatar.png` (`Some`
/// bytes, or `None` to take it away; untouched when not asked), in one
/// commit (`key`, when one), then a deploy. The files' answer, and the
/// deploy's (none when the files were refused).
pub(super) fn set_face(api: &Api, keys: &Keys, agent: &str, title: Option<&str>, avatar: Option<Option<&[u8]>>, key: Option<&str>) -> Result<(Reply, Option<Reply>)> {
    let mut files = vec![];
    if let Some(title) = title {
        let mut manifest = api.signed(keys, "GET", &format!("/api/f/{agent}/manifest"), None)?.body;
        manifest["meta"]["title"] = json!(title);
        files.push(json!({ "path": "fragment.json", "text": format!("{}\n", serde_json::to_string_pretty(&manifest)?) }));
    }
    match avatar {
        Some(Some(bytes)) => files.push(json!({ "path": "avatar.png", "base64": base64::engine::general_purpose::STANDARD.encode(bytes) })),
        Some(None) => files.push(json!({ "path": "avatar.png", "delete": true })),
        None => {}
    }
    let mut body = json!({ "message": "its name and picture", "files": files });
    if let Some(key) = key {
        body["key"] = json!(key);
    }
    let wrote = api.signed(keys, "POST", &format!("/api/f/{agent}/files"), Some(&body))?;
    if wrote.status != 200 {
        return Ok((wrote, None));
    }
    let deployed = api.signed(keys, "POST", &format!("/api/f/{agent}/deploy"), Some(&json!({})))?;
    Ok((wrote, Some(deployed)))
}

/// `name`'s row in `keys`' list (`GET /api/fragments`): what the shell draws.
pub(super) fn row(api: &Api, keys: &Keys, name: &str) -> Value {
    let r = api.signed(keys, "GET", "/api/fragments", None).ok();
    r.and_then(|r| r.body["fragments"].as_array().and_then(|l| l.iter().find(|f| f["name"] == name).cloned())).unwrap_or(Value::Null)
}

/// `id`'s profile as `chat`'s page asks it (`__people`, with a site cookie).
pub(super) fn people(api: &Api, chat: &str, cookie: &str, id: &str) -> Value {
    let r = api.page(chat, &format!("__people?id={id}"), Some(&format!("fragment_site={cookie}"))).ok();
    r.map(|r| r.body["profiles"][id].clone()).unwrap_or(Value::Null)
}

/// The picture at `path` (on the platform), as anyone fetches it.
pub(super) fn fetched(api: &Api, path: &str) -> Result<Reply> {
    api.call(Call { method: "GET", url: format!("{}{path}", api.base), ..Call::default() })
}

pub fn faces(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("faces", &[crate::Need::Computers]) {
        return Ok(());
    }
    let (orange, blue) = (picture(ORANGE), picture(BLUE));
    let (paul, session) = person(api)?;
    let (created, agent_id) = agent_of(s, api, &paul, "faces-juniper", "Juniper")?;
    let agent = created["name"].as_str().unwrap_or("").to_string();
    let label = fragment_proto::split_fragment_name(&agent).map(|(l, _)| l.to_string()).unwrap_or_default();
    // a chat of Paul's, where its page asks who is who
    let chat = s.named(api, &paul, "faces-chat")?;
    let r = api.create_with(&paul, json!({ "name": chat, "template": "chat", "title": "Garden" }))?;
    anyhow::ensure!(r.status == 200, "the chat: {r}");
    let cookie = site_cookie(api, &session, &chat)?;
    let path_of = |bytes: &[u8]| picture_path(&agent_id, bytes);

    // made: its title, no picture
    let made = row(api, &paul, &agent);
    let asked = people(api, &chat, &cookie, &agent_id);
    let none = fetched(api, &format!("/api/identities/{agent_id}/picture"))?;
    s.ok(
        "an agent fresh from the agent template shows by its fragment's title, with no picture (404 at its address)",
        made["title"] == "Juniper" && made.get("picture").is_none() && asked["title"] == "Juniper" && asked["name"] == label.as_str() && asked["picture"].is_null() && none.status == 404,
        json!({ "row": made, "people": asked, "picture": none.status }),
    );

    // renamed, and given a picture, as Settings and `fragment agent` do
    let (wrote, deployed) = set_face(api, &paul, &agent, Some("Frank"), Some(Some(&orange)), Some("face-1"))?;
    let deployed = deployed.with_context(|| format!("the files: {wrote}"))?;
    let shown = path_of(&orange);
    let listed = s.eventually(WAIT, || {
        let r = row(api, &paul, &agent);
        r["title"] == "Frank" && r["picture"] == shown.as_str()
    });
    s.ok(
        "renamed and given a picture through its files and a deploy, its owner's list (what the shell draws) names it and shows its picture's address",
        wrote.status == 200 && deployed.status == 200 && listed,
        json!({ "files": wrote.body, "deploy": deployed.body, "row": row(api, &paul, &agent) }),
    );
    let asked = people(api, &chat, &cookie, &agent_id);
    s.ok(
        "a chat's page is told so too (`__people`): its title and its picture, whole; its @name stays its label",
        asked["title"] == "Frank" && asked["picture"].as_str().is_some_and(|p| p.starts_with("http") && p.ends_with(&shown)) && asked["name"] == label.as_str(),
        &asked,
    );
    let got = fetched(api, &shown)?;
    s.ok(
        "its picture is served at its identity's address to anyone, the bytes its install checked, as an image",
        got.status == 200 && got.bytes == orange && got.header("content-type") == "image/png" && got.header("cache-control").contains("public"),
        json!({ "status": got.status, "type": got.header("content-type"), "bytes": got.bytes.len() }),
    );
    let sheet = with_session(api, "GET", &format!("/share/{agent}"), &session)?;
    s.ok(
        "the share sheet names it so, with its picture",
        sheet.status == 200 && sheet.text.contains("Frank") && sheet.text.contains(&format!("src=\"{}\"", shown.replace('&', "&amp;"))),
        &sheet,
    );
    // who has access to what asks the members of what someone else is in:
    // Bob, a viewer of it for a moment
    let (bob, _) = person(api)?;
    let bob_id = api.identity(&bob)?;
    let r = api.signed(&paul, "PUT", &format!("/api/f/{agent}/members/{bob_id}"), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "Bob, a viewer: {r}");
    let access = api.signed(&paul, "GET", "/api/access", None)?;
    let member = access.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == agent.as_str()).and_then(|f| f["members"].as_array()?.iter().find(|m| m["principal"] == agent_id.as_str()).cloned());
    s.ok("who has access to what names it so too (its title, beside its label)", member.as_ref().is_some_and(|m| m["title"] == "Frank" && m["name"] == label.as_str()), &access);
    let r = api.signed(&paul, "DELETE", &format!("/api/f/{agent}/members/{bob_id}"), None)?;
    anyhow::ensure!(r.status == 200, "Bob, out again: {r}");

    // replay: the same change again, by its key or by its files, commits nothing new
    let tip = wrote.body["commit"].clone();
    let (again, redeployed) = set_face(api, &paul, &agent, Some("Frank"), Some(Some(&orange)), Some("face-1"))?;
    let (same, _) = set_face(api, &paul, &agent, Some("Frank"), Some(Some(&orange)), None)?;
    let kept = row(api, &paul, &agent);
    s.ok(
        "the same change again, under its key or as the same files, commits nothing new, and its name and picture's address stay",
        again.body["commit"] == tip && same.body["commit"] == tip && redeployed.is_some_and(|d| d.status == 200) && kept["title"] == "Frank" && kept["picture"] == shown.as_str(),
        json!({ "first": tip, "again": again.body, "same": same.body, "row": kept }),
    );

    // invalid: written through the platform, refused before anything is committed
    let mut big = b"\x89PNG\r\n\x1a\n".to_vec();
    big.resize(fragment_proto::limits::PICTURE_MAX_BYTES + 1, 0);
    let (large, _) = set_face(api, &paul, &agent, None, Some(Some(&big)), None)?;
    let (text, _) = set_face(api, &paul, &agent, None, Some(Some(b"hello, I am no picture")), None)?;
    let unmoved = api.signed(&paul, "GET", &format!("/api/f/{agent}/status"), None)?.body["pins"]["main"].clone();
    s.ok(
        "a picture past 256 KiB is 413 and one that is no picture 400, each naming avatar.png and why, and nothing is committed",
        large.status == 413
            && large.text.contains("avatar.png")
            && large.text.contains("262144")
            && text.status == 400
            && text.text.contains("avatar.png is not a picture")
            && unmoved == tip,
        json!({ "large": large.to_string(), "text": text.to_string(), "main": unmoved, "tip": tip }),
    );
    // a title too long, and (locally) a picture pushed through git that is none: refused at the install, the last good face kept
    let long = "F".repeat(121);
    let (_, refused) = set_face(api, &paul, &agent, Some(&long), None, None)?;
    let said = api.status(&paul, &agent)?.body["code"]["error"].clone();
    let kept = row(api, &paul, &agent);
    s.ok(
        "a title past 120 characters is refused at its deploy, saying why, and the last good name and picture stay",
        refused.is_some_and(|d| d.status == 200) && said.as_str().is_some_and(|e| e.contains("title")) && kept["title"] == "Frank" && kept["picture"] == shown.as_str(),
        json!({ "error": said, "row": kept }),
    );
    if s.hosted() {
        s.skip("a picture pushed through git that is no picture is refused at its install", "a preview's git is written through the platform's files route, which refuses it first (above)");
    } else {
        s.commit(&created, &[("fragment.json", Some(br#"{"template":"agent","meta":{"title":"Frank"}}"#)), ("avatar.png", Some(b"<svg/>"))]);
        s.deploy(&created);
        let said = api.status(&paul, &agent)?.body["code"]["error"].clone();
        let served = fetched(api, &shown)?;
        let kept = row(api, &paul, &agent);
        s.ok(
            "a picture pushed through git that is no picture is refused at its install, saying why (`fragment deploy` prints it), and the last good one is still served",
            said.as_str().is_some_and(|e| e.contains("avatar.png is not a picture")) && served.status == 200 && served.bytes == orange && kept["picture"] == shown.as_str(),
            json!({ "error": said, "served": served.status, "row": kept }),
        );
    }
    // someone else's agent: Bob, no member of it now, changes nothing
    let (theirs, _) = set_face(api, &bob, &agent, None, Some(Some(&blue)), None)?;
    let deploy = api.signed(&bob, "POST", &format!("/api/f/{agent}/deploy"), Some(&json!({})))?;
    let kept = row(api, &paul, &agent);
    s.ok(
        "someone else's agent: another person writes its picture and deploys it, refused (403 or 404), its name and picture unchanged",
        matches!(theirs.status, 403 | 404) && matches!(deploy.status, 403 | 404) && kept["picture"] == shown.as_str(),
        json!({ "files": theirs.to_string(), "deploy": deploy.to_string(), "row": kept }),
    );
    let nobody = fetched(api, "/api/identities/npub1nobody/picture")?;
    s.ok("an address that names no identity serves no picture (404)", nobody.status == 404, &nobody);
    // an app's avatar.png is a file like any other: only an agent's is its picture
    let app = s.named(api, &paul, "faces-app")?;
    s.create(api, &paul, &app)?;
    let r = api.signed(&paul, "POST", &format!("/api/f/{app}/files"), Some(&json!({ "files": [{ "path": "avatar.png", "text": "not a picture, and no one's face" }] })))?;
    s.ok("an app's avatar.png is a file like any other (only an agent fragment's is a picture)", r.status == 200, &r);

    // a new picture is a new address; taken away, it is gone everywhere
    let fixed = set_face(api, &paul, &agent, Some("Frank"), Some(Some(&blue)), None)?;
    let renewed = path_of(&blue);
    let moved = s.eventually(WAIT, || row(api, &paul, &agent)["picture"] == renewed.as_str());
    let got = fetched(api, &renewed)?;
    s.ok(
        "a new picture is a new address, serving it",
        fixed.1.is_some_and(|d| d.status == 200) && moved && renewed != shown && got.status == 200 && got.bytes == blue,
        json!({ "row": row(api, &paul, &agent), "status": got.status }),
    );
    let (gone, _) = set_face(api, &paul, &agent, None, Some(None), None)?;
    let removed = s.eventually(WAIT, || row(api, &paul, &agent).get("picture").is_none());
    let asked = people(api, &chat, &cookie, &agent_id);
    let none = fetched(api, &format!("/api/identities/{agent_id}/picture"))?;
    s.ok(
        "its picture taken away (avatar.png deleted, deployed), no list, page or address shows one, and its name stays",
        gone.status == 200 && removed && asked["picture"].is_null() && asked["title"] == "Frank" && none.status == 404,
        json!({ "row": row(api, &paul, &agent), "people": asked, "picture": none.status }),
    );
    Ok(())
}
