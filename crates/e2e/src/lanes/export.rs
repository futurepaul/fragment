//! A person's export of everything that is theirs (docs/api.md, Export),
//! the whole round trip on the stub image: a person with an app (files, one
//! of them a blob; its app's rows; a member, an invite, a secret, a share
//! link), a fragment someone else shares with them, and a computer running
//! their agent in a chat on the template (a message with a file attached,
//! the agent's step and reply, its `/data` written, then saved).
//!
//! - **valid:** the archive holds each of them as it is: its README, its
//!   manifest (theirs, and what others share with them, named only), each
//!   fragment's meta (no token, no secret's value anywhere in it), files
//!   byte for byte, the app's schema and rows, every channel's records, a
//!   chat's transcript and the file its record attaches, and the computer's
//!   save as stored. Its person is mailed a link to Settings, never one to
//!   the archive.
//! - **invalid:** signed out is 401, an agent is 403 (and one acting for
//!   its owner 400), and anyone else finds none and downloads nothing
//!   (404), a fragment's page included.
//! - **replay:** two presses while it builds are one export; three in a
//!   day are the most, the fourth refused saying when; one replaced is
//!   deleted, and its usage metered to its person.
//! - **restart:** the node crashes mid-build (the build held part way by
//!   the test lever), and the build goes on from its place and finishes.

use std::collections::BTreeMap;
use std::io::Read;
use std::time::Duration;

use anyhow::{Context, Result};
use fragment_core::blob::{self, sha256_hex};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::app::ship;
use super::computers::{agent_replies, newest_save, AGENT_JSON, QUEUE_DRAIN};
use super::jobs::records;
use super::ledger::entries;
use crate::api::{now_s, Api, Call, Reply};
use crate::{Need, Suite};

const TODO_APP: &[u8] = include_bytes!("../../fixtures/todo.mjs");
const TODO_JSON: &[u8] = include_bytes!("../../fixtures/todo.json");
const SOUL: &[u8] = b"You keep the garden's notes.\n";
const SECRET: &str = "sk-export-never-in-the-archive-5e1c";
const PICTURE: &[u8] = b"\x89PNG\r\n\x1a\nthe-cat-the-export-carries";
/// A stub's start, its restore, and its bridge's first follow.
const WAKE: Duration = Duration::from_secs(90);
/// A build of a small person's export, on a laptop.
const BUILT: Duration = Duration::from_secs(120);

fn bytes_of(n: usize, seed: u8) -> Vec<u8> {
    (0..n).map(|i| (i as u8).wrapping_mul(29).wrapping_add(seed)).collect()
}

fn view(api: &Api, keys: &Keys) -> Value {
    api.signed(keys, "GET", "/api/export", None).map(|r| r.body["export"].clone()).unwrap_or(Value::Null)
}

fn press(api: &Api, keys: &Keys) -> Result<Reply> {
    api.signed(keys, "POST", "/api/export", None)
}

fn pace(api: &Api, identity: &str, steps: Option<u32>) -> Result<Reply> {
    api.unsigned("POST", "/api/test/export", Some(&json!({ "identity": identity, "op": "pace", "steps": steps })))
}

/// Waits for `keys`' export to be ready (or to fail): its view.
fn built(s: &Suite, api: &Api, keys: &Keys) -> Value {
    s.eventually(BUILT, || view(api, keys)["state"] != "building");
    view(api, keys)
}

/// The archive's entries, by path: its bytes.
fn entries_of(tar_bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut a = tar::Archive::new(tar_bytes);
    let mut out = BTreeMap::new();
    for e in a.entries()? {
        let mut e = e?;
        let path = e.path()?.to_string_lossy().to_string();
        let mut b = vec![];
        e.read_to_end(&mut b)?;
        anyhow::ensure!(out.insert(path.clone(), b).is_none(), "{path} is in the archive once");
    }
    Ok(out)
}

fn json_at(files: &BTreeMap<String, Vec<u8>>, path: &str) -> Value {
    files.get(path).and_then(|b| serde_json::from_slice(b).ok()).unwrap_or(Value::Null)
}

fn lines_at(files: &BTreeMap<String, Vec<u8>>, path: &str) -> Vec<Value> {
    files.get(path).map(|b| String::from_utf8_lossy(b).lines().filter_map(|l| serde_json::from_str(l).ok()).collect()).unwrap_or_default()
}

fn text_at(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    files.get(path).map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default()
}

/// A download as a browser asks it: the platform session's cookie, and the
/// Fetch Metadata of the page the click was on.
fn browser_download(api: &Api, session: &str, id: &str, site: &str, range: Option<&str>) -> Result<Reply> {
    let mut extra = vec![("sec-fetch-site", site.to_string()), ("sec-fetch-mode", "navigate".to_string()), ("sec-fetch-dest", "document".to_string())];
    if let Some(r) = range {
        extra.push(("range", r.to_string()));
    }
    api.call(Call { method: "GET", url: format!("{}/api/export/{id}/download", api.base), cookie: Some(format!("fragment_session={session}")), extra, ..Call::default() })
}

pub fn export(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("export", &[Need::Fakes, Need::LocalDocker, Need::Node, Need::Levers, Need::Chrome]) {
        return Ok(());
    }
    // ---- a person, and someone else
    let paul = api.person()?;
    let paul_id = api.identity(&paul)?;
    let email = Api::email_of(&paul);
    let bob = api.person()?;
    let bob_id = api.identity(&bob)?;

    // an app: its files (one of them a blob), its rows, a member, an invite, a secret
    let app_name = s.named(api, &paul, "export-app")?;
    let app = s.create(api, &paul, &app_name)?;
    // past a part of the archive (8 MiB), so it is copied in over slices
    let big = bytes_of(12 * 1024 * 1024 + 77, 3);
    let big_sha = sha256_hex(&big);
    let put = api.call(Call { method: "PUT", url: format!("{}/api/f/{app_name}/blobs/{big_sha}", api.base), body: Some(big.clone()), keys: Some(&paul), ..Call::default() })?;
    let pointer = blob::pointer(&big_sha, big.len() as u64);
    s.commit(&app, &[("site/index.html", Some(b"<p>mine</p>\n")), ("media/big.bin", Some(pointer.as_bytes()))]);
    ship(s, &app, TODO_APP, TODO_JSON);
    let installed = s.eventually(Duration::from_secs(30), || api.op(&paul, &app_name, "count", "q", json!({})).is_ok_and(|r| r.status == 200));
    let row = api.op(&paul, &app_name, "add_todo", "x1", json!({ "text": "export me, a row" }))?;
    let member = api.signed(&paul, "PUT", &format!("/api/f/{app_name}/members/{bob_id}"), Some(&json!({ "role": "viewer" })))?;
    let invitee = format!("export-invitee-{}@example.com", &Keys::generate().pubkey_hex()[..8]);
    let invited = api.signed(&paul, "POST", &format!("/api/f/{app_name}/invites"), Some(&json!({ "email": invitee, "role": "editor" })))?;
    let secret = api.call(Call { method: "PUT", url: format!("{}/api/f/{app_name}/secrets/API_KEY", api.base), body: Some(SECRET.as_bytes().to_vec()), keys: Some(&paul), ..Call::default() })?;
    let status = api.status(&paul, &app_name)?;
    let tokens = [status.body["viewToken"].as_str().unwrap_or("").to_string(), status.body["inboxToken"].as_str().unwrap_or("").to_string()];
    s.ok(
        "a person's app: a blob, its rows, a member, an invite, a secret, its tokens",
        put.status == 200 && installed && row.status == 200 && member.status == 200 && invited.status == 200 && secret.status == 200 && tokens.iter().all(|t| t.len() >= 16),
        format!("{put} | {row} | {member} | {invited} | {secret} | {status}"),
    );
    // someone else's fragment, shared with them
    let bobs = s.named(api, &bob, "export-bobs")?;
    s.create(api, &bob, &bobs)?;
    let shared = api.signed(&bob, "PUT", &format!("/api/f/{bobs}/members/{paul_id}"), Some(&json!({ "role": "editor" })))?;
    anyhow::ensure!(shared.status == 200, "sharing bob's fragment: {shared}");

    // a computer running their agent, in a chat on the template
    let r = api.signed(&paul, "POST", "/api/computers", Some(&json!({})))?;
    let computer = r.body["computer"].as_str().unwrap_or("").to_string();
    let agent_name = s.named(api, &paul, "export-agent")?;
    let agent = s.create(api, &paul, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON)), ("SOUL.md", Some(SOUL))]);
    s.deploy(&agent);
    let r = api.signed(&paul, "PUT", &format!("/api/computers/{computer}/agents/{agent_name}"), Some(&json!({})))?;
    let agent_id = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    let chat_name = s.named(api, &paul, "export-chat")?;
    let r = api.create_with(&paul, json!({ "name": chat_name, "template": "chat", "title": "Garden" }))?;
    anyhow::ensure!(r.status == 200, "making the chat: {r}");
    s.owned(&r.body, &paul);
    api.signed(&paul, "PUT", &format!("/api/f/{chat_name}/members/{agent_id}"), Some(&json!({ "role": "editor" })))?;
    let pic_sha = sha256_hex(PICTURE);
    let up = api.call(Call {
        method: "PUT",
        url: format!("{}/api/f/{chat_name}/blobs/{pic_sha}", api.base),
        body: Some(PICTURE.to_vec()),
        content_type: Some("image/png"),
        keys: Some(&paul),
        ..Call::default()
    })?;
    let attachment = json!({ "sha256": pic_sha, "size": PICTURE.len(), "type": "image/png", "name": "cat.png" });
    let said = api.signed(&paul, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "e1", "body": { "text": "tool kale for the export", "attachments": [attachment] } })))?;
    let replied = |api: &Api, text: &str| agent_replies(&records(api, &paul, &chat_name, "chat"), &agent_id).iter().any(|r| r["body"]["text"].as_str().is_some_and(|t| t.contains(text)));
    let answered = up.status == 200 && said.status == 200 && s.eventually(WAKE, || replied(api, "tool kale for the export"));
    let wrote = api.signed(&paul, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "e2", "body": { "text": "write notes/export.txt kept for the export" } })))?;
    let kept = wrote.status == 200 && s.eventually(WAKE, || replied(api, "wrote notes/export.txt"));
    std::thread::sleep(QUEUE_DRAIN);
    let slept = api.signed(&paul, "POST", &format!("/api/computers/{computer}/sleep"), Some(&json!({})))?;
    let save = newest_save(api, &computer);
    let records_of_save = save["records"].as_array().cloned().unwrap_or_default();
    s.ok(
        "their agent answers in the chat (a file attached, a step), writes its /data, and the computer saves it asleep",
        answered && kept && slept.body["phase"] == "asleep" && !records_of_save.is_empty(),
        format!("{said} | {wrote} | {slept} | {save}"),
    );

    // ---- invalid: signed out, an agent, an agent for its owner, a body
    let r1 = api.unsigned("POST", "/api/export", None)?;
    let r2 = api.unsigned("GET", "/api/export", None)?;
    let cli_agent = Keys::generate();
    let proof = cli_agent.proof("POST", &format!("{}/api/identities", api.base), bob.pubkey_hex(), now_s());
    let r = api.signed(&bob, "POST", "/api/identities", Some(&json!({ "kind": "agent", "proof": proof })))?;
    anyhow::ensure!(r.status == 200, "an agent of bob's: {r}");
    let as_agent = press(api, &cli_agent)?;
    let for_owner = api.signed(&cli_agent, "POST", &format!("/api/export?for={bob_id}"), None)?;
    let agent_reads = api.signed(&cli_agent, "GET", "/api/export", None)?;
    let with_body = api.signed(&paul, "POST", "/api/export", Some(&json!({ "person": bob_id })))?;
    s.ok("signed out, starting or reading an export is 401", r1.status == 401 && r2.status == 401, format!("{r1} | {r2}"));
    s.ok(
        "an agent starts and reads none (403), nor one acting for its owner (400); a press carries no body (400)",
        as_agent.status == 403 && agent_reads.status == 403 && for_owner.status == 400 && with_body.status == 400,
        format!("{as_agent} | {agent_reads} | {for_owner} | {with_body}"),
    );
    s.ok("before a press, a person has no export", view(api, &paul).is_null(), view(api, &paul));

    // ---- replay and restart: two presses are one export; held part way
    // (a part of its archive uploaded, the blob copied in part), the node
    // crashes, and the build goes on from its place. Its entries of unknown
    // size are put aside in pieces of 2 KiB, as a large export's are.
    pace(api, &paul_id, Some(0))?;
    let piece = api.unsigned("POST", "/api/test/export", Some(&json!({ "identity": paul_id, "op": "piece", "steps": 2048 })))?;
    anyhow::ensure!(piece.status == 200, "the piece lever: {piece}");
    let first = press(api, &paul)?;
    let again = press(api, &paul)?;
    let id = first.body["id"].as_str().unwrap_or("").to_string();
    s.ok(
        "a press starts an export, building; a second while it builds is the same one",
        first.status == 200 && first.body["state"] == "building" && again.status == 200 && again.body["id"] == id.as_str() && fragment_proto::export::valid_export_id(&id),
        format!("{first} | {again}"),
    );
    const PART: u64 = fragment_core::export::limits::PART_BYTES as u64;
    let held = s.eventually(Duration::from_secs(90), || {
        if view(api, &paul)["progress"]["bytes"].as_u64().unwrap_or(0) > PART {
            return true;
        }
        let _ = pace(api, &paul_id, Some(1));
        std::thread::sleep(Duration::from_millis(400));
        false
    });
    pace(api, &paul_id, Some(0))?;
    std::thread::sleep(Duration::from_secs(2));
    let mid = view(api, &paul);
    let mid_bytes = mid["progress"]["bytes"].as_u64().unwrap_or(0);
    s.ok(
        "held part way: building, a part of its archive uploaded and the rest not",
        held && mid["state"] == "building" && mid_bytes > PART && mid_bytes < big.len() as u64 && mid["progress"]["done"].as_u64().unwrap_or(0) < mid["progress"]["total"].as_u64().unwrap_or(0),
        &mid,
    );
    s.crash()?;
    let restarted = s.start(false)?;
    let api = &restarted;
    let after = view(api, &paul);
    s.ok(
        "after a crash of the node, the export is the same one, where it was",
        after["id"] == id.as_str() && after["state"] == "building" && after["progress"]["bytes"].as_u64() >= mid["progress"]["bytes"].as_u64(),
        format!("{after} after {mid}"),
    );
    pace(api, &paul_id, None)?;
    let done = built(s, api, &paul);
    api.unsigned("POST", "/api/test/export", Some(&json!({ "identity": paul_id, "op": "piece", "steps": null })))?;
    let ready_at = done["readyAt"].as_i64().unwrap_or(0);
    s.ok(
        "let go, it goes on from its place and is ready: its size, and its seven days",
        done["id"] == id.as_str() && done["state"] == "ready" && done["size"].as_u64().unwrap_or(0) > big.len() as u64 && done["expiresAt"].as_i64() == Some(ready_at + 7 * 24 * 3600 * 1000) && done["download"] == format!("/api/export/{id}/download").as_str(),
        &done,
    );

    // ---- the archive
    let r = api.signed(&paul, "GET", &format!("/api/export/{id}/download"), None)?;
    s.ok(
        "it downloads to its person: one tar file, as an attachment",
        r.status == 200 && r.header("content-type") == "application/x-tar" && r.header("content-disposition").starts_with("attachment; filename=\"fragment-export-") && r.bytes.len() as u64 == done["size"].as_u64().unwrap_or(0),
        format!("{} {:?} {} bytes", r.status, r.headers, r.bytes.len()),
    );
    let archive = r.bytes.clone();
    // kept in the run's scratch, which a failing run keeps (FRAGMENT_E2E_KEEP=1 keeps it anyway)
    std::fs::write(s.dir("export").join("export.tar"), &archive)?;
    let files = entries_of(&archive).context("the archive is a tar")?;
    let root = fragment_core::export::ROOT;
    let frag = |name: &str, path: &str| format!("{root}/fragments/{name}/{path}");
    s.ok("every entry is in its one folder, and its README is the platform's", files.keys().all(|k| k.starts_with(&format!("{root}/"))) && text_at(&files, &format!("{root}/README.md")) == fragment_core::export::README, files.keys().cloned().collect::<Vec<_>>().join(" "));
    let manifest = json_at(&files, &format!("{root}/manifest.json"));
    let names: Vec<&str> = manifest["fragments"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str()).collect();
    let shared_row = manifest["sharedWithMe"].as_array().into_iter().flatten().find(|f| f["name"] == bobs.as_str()).cloned().unwrap_or(Value::Null);
    s.ok(
        "its manifest: whose it is, their fragments and computer, and what others share with them, named",
        manifest["format"] == "fragment-export"
            && manifest["version"] == 1
            && manifest["export"] == id.as_str()
            && manifest["person"]["npub"] == paul_id.as_str()
            && manifest["person"]["email"] == email.as_str()
            && [app_name.as_str(), chat_name.as_str(), agent_name.as_str()].iter().all(|n| names.contains(n))
            && !names.contains(&bobs.as_str())
            && manifest["computers"][0]["id"] == computer.as_str()
            && shared_row["role"] == "editor"
            && shared_row["owner"] == bob_id.as_str()
            && shared_row["member"] == paul_id.as_str(),
        &manifest,
    );
    s.ok("a fragment shared with them is named, never copied", !files.keys().any(|k| k.contains(&bobs)), "");
    let meta = json_at(&files, &frag(&app_name, "meta.json"));
    let bob_row = meta["members"].as_array().into_iter().flatten().find(|m| m["principal"] == bob_id.as_str()).cloned().unwrap_or(Value::Null);
    s.ok(
        "an app's meta: its owner, its member (their email), the invite waiting, its secret's name, whether its links are",
        meta["owner"] == paul_id.as_str()
            && bob_row["role"] == "viewer"
            && bob_row["email"] == Api::email_of(&bob).as_str()
            && meta["invites"][0]["email"] == invitee.as_str()
            && meta["secrets"] == json!(["API_KEY"])
            && meta["shareLink"] == true
            && meta["inboxToken"] == true
            && meta["visibility"] == "link",
        &meta,
    );
    let leaked: Vec<&str> = tokens.iter().map(String::as_str).chain([SECRET]).filter(|t| archive.windows(t.len()).any(|w| w == t.as_bytes())).collect();
    s.ok("no token, nor a secret's value, is anywhere in the archive", leaked.is_empty(), format!("{leaked:?}"));
    s.ok(
        "its files, byte for byte, a blob's whole",
        files.get(&frag(&app_name, "files/site/index.html")).map(Vec::as_slice) == Some(&b"<p>mine</p>\n"[..])
            && files.get(&frag(&app_name, "files/media/big.bin")) == Some(&big)
            && files.get(&frag(&app_name, "files/app.mjs")).map(Vec::as_slice) == Some(TODO_APP)
            && meta["files"]["count"].as_u64() >= Some(4),
        &meta["files"],
    );
    let schema = text_at(&files, &frag(&app_name, "database/schema.sql"));
    let todos = lines_at(&files, &frag(&app_name, "database/todos.jsonl"));
    s.ok(
        "its app's database: its schema, and its rows, the platform's ledger left out",
        schema.contains("CREATE TABLE todos") && !schema.contains("_fragment_ops") && todos.iter().any(|r| r["text"] == "export me, a row") && !files.keys().any(|k| k.contains("_fragment_ops")),
        format!("{schema} {todos:?}"),
    );
    let ops = lines_at(&files, &frag(&app_name, "channels/ops.jsonl"));
    let events = lines_at(&files, &frag(&app_name, "channels/events.jsonl"));
    let events_bytes = files.get(&frag(&app_name, "channels/events.jsonl")).map_or(0, Vec::len);
    s.ok(
        "every channel's records, in order, whole when put aside in pieces: its audit trail and its applied operations",
        events_bytes > 2048
            && events.len() == String::from_utf8_lossy(&files[&frag(&app_name, "channels/events.jsonl")]).lines().count()
            && events.windows(2).all(|w| w[0]["seq"].as_i64() < w[1]["seq"].as_i64())
            && ops.iter().any(|r| r["body"]["op"] == "add_todo"),
        format!("{} events in {events_bytes} bytes, {ops:?}", events.len()),
    );
    let chat = lines_at(&files, &frag(&chat_name, "channels/chat.jsonl"));
    let work = lines_at(&files, &frag(&chat_name, "channels/work.jsonl"));
    s.ok(
        "a chat's records: the message with its file, the agent's reply, its turn's step",
        chat.iter().any(|r| r["body"]["text"] == "tool kale for the export" && r["body"]["attachments"][0]["sha256"] == pic_sha.as_str() && r["principal"] == paul_id.as_str())
            && chat.iter().any(|r| r["principal"] == agent_id.as_str() && r["body"]["text"].as_str().is_some_and(|t| t.contains("tool kale")))
            && work.iter().any(|r| r["body"]["kind"] == "turn.step"),
        format!("{chat:?}"),
    );
    let transcript = text_at(&files, &frag(&chat_name, "transcript.md"));
    s.ok(
        "a chat's transcript reads as the conversation: who said what, the file, the step, the reply",
        transcript.starts_with("# Garden")
            && transcript.contains(&format!("**{email}**"))
            && transcript.contains("tool kale for the export")
            && transcript.contains(&format!("Attached: [cat.png](blobs/{pic_sha})"))
            && transcript.contains("wrote notes/export.txt")
            && transcript.contains("> "),
        &transcript,
    );
    s.ok("the file a record attaches, by its hash", files.get(&frag(&chat_name, &format!("blobs/{pic_sha}"))).map(Vec::as_slice) == Some(PICTURE), "");
    s.ok("an agent's soul is its fragment's file", files.get(&frag(&agent_name, "files/SOUL.md")).map(Vec::as_slice) == Some(SOUL), "");
    let hex = computer.strip_prefix("computer:").unwrap_or("");
    let cmeta = json_at(&files, &format!("{root}/computers/{hex}/meta.json"));
    let stored: Vec<(String, bool)> = records_of_save
        .iter()
        .map(|rec| {
            let file = fragment_core::export::save_path(&computer, rec["dir"].as_str().unwrap_or("/data"));
            (file.clone(), files.get(&file).is_some_and(|b| sha256_hex(b) == rec["sha256"].as_str().unwrap_or("-")))
        })
        .collect();
    s.ok(
        "its computer's newest good save, each record as stored (its SHA-256 the save's)",
        cmeta["save"]["id"] == save["id"] && cmeta["view"]["computer"] == computer.as_str() && !stored.is_empty() && stored.iter().all(|(_, same)| *same) && files.contains_key(&format!("{root}/computers/{hex}/data.tar.zst")),
        format!("{stored:?} {cmeta}"),
    );

    // ---- the mail: a link to Settings, never one to the archive
    let mail = s.mail.sent_to(&email);
    let told = mail.iter().find(|m| m.subject.contains("export is ready"));
    s.ok(
        "its person is mailed once it is ready: a link to Settings, none to the archive",
        told.is_some_and(|m| m.text.contains("/settings") && !m.text.contains("/download") && !m.text.contains(&id)),
        format!("{mail:?}"),
    );

    // ---- only its person
    let bobs_view = api.signed(&bob, "GET", "/api/export", None)?;
    let bob_down = api.signed(&bob, "GET", &format!("/api/export/{id}/download"), None)?;
    let no_one = api.unsigned("GET", &format!("/api/export/{id}/download"), None)?;
    s.ok(
        "anyone else finds none of theirs, and downloads nothing (404); signed out, 401",
        bobs_view.status == 200 && bobs_view.body["export"].is_null() && bob_down.status == 404 && no_one.status == 401,
        format!("{bobs_view} | {bob_down} | {no_one}"),
    );
    let paul_session = api.sign_in(&email)?;
    let bob_session = api.sign_in(&Api::email_of(&bob))?;
    let clicked = browser_download(api, &paul_session, &id, "same-origin", None)?;
    let ranged = browser_download(api, &paul_session, &id, "same-origin", Some("bytes=0-99"))?;
    let other_page = browser_download(api, &paul_session, &id, "same-site", None)?;
    let elsewhere = browser_download(api, &paul_session, &id, "cross-site", None)?;
    let bobs_click = browser_download(api, &bob_session, &id, "same-origin", None)?;
    s.ok(
        "a click on Settings downloads it with the platform session (a range too)",
        clicked.status == 200 && clicked.bytes == archive && ranged.status == 206 && ranged.bytes == archive[..100],
        format!("{} {} bytes | {}", clicked.status, clicked.bytes.len(), ranged.status),
    );
    s.ok(
        "no other page downloads it with the person's cookie (a fragment's, another site's: 401), nor anyone else's session (404)",
        other_page.status == 401 && elsewhere.status == 401 && bobs_click.status == 404,
        format!("{other_page} | {elsewhere} | {bobs_click}"),
    );

    // ---- the shell: Settings' Your data shows it, downloads it on a click
    // (Chrome's own navigation, its Fetch Metadata as a person's click sends
    // it), and a press there makes a new one, which it follows to ready
    match s.browser()? {
        None => s.ok("Chrome is installed for the export section (set CHROME_BIN)", false, "no Chrome found"),
        Some(mut chrome) => {
            let downloads = s.dir("export-downloads");
            chrome.allow_downloads(&downloads)?;
            chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &paul_session)?;
            let page = chrome.open(&format!("{}/settings", api.base))?;
            let link = |id: &str| format!("document.querySelector('#settings-data [data-action=export-download]')?.getAttribute('href') === '/api/export/{id}/download'");
            let shown = chrome.until(&page, &link(&id), Duration::from_secs(30));
            let text = chrome.eval(&page, "document.getElementById('settings-data')?.innerText ?? ''")?;
            let text = text.as_str().unwrap_or("").to_string();
            s.ok("Settings shows Your data: the export ready, its size, until when it is kept, and its download", shown && text.contains("Ready to download") && text.contains("Kept until"), &text);
            let _ = chrome.viewport(&page, 1100, 2600, false);
            let _ = chrome.eval(&page, "document.getElementById('settings-data')?.scrollIntoView({ block: 'center' })");
            let _ = chrome.screenshot(&page, &s.dir("export-shots").join("settings-your-data.png"));
            chrome.click(&page, "#settings-data [data-action=export-download]")?;
            let saved = || std::fs::read_dir(&downloads).ok().into_iter().flatten().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "tar"));
            let got = s.eventually(Duration::from_secs(30), || saved().and_then(|p| std::fs::metadata(p).ok()).is_some_and(|m| m.len() == archive.len() as u64));
            let same = saved().and_then(|p| std::fs::read(p).ok()).is_some_and(|b| b == archive);
            s.ok("a click on its download saves the archive, byte for byte", got && same, format!("{:?}", saved()));
            chrome.click(&page, "#settings-data [data-action=export]")?;
            let again = s.eventually(BUILT, || view(api, &paul)["id"] != id.as_str() && view(api, &paul)["state"] == "ready");
            let new_id = view(api, &paul)["id"].as_str().unwrap_or("").to_string();
            let followed = again && chrome.until(&page, &link(&new_id), Duration::from_secs(30));
            s.ok("its Export again starts a new one, which the page follows until it is ready", followed && fragment_proto::export::valid_export_id(&new_id), view(api, &paul));
        }
    }

    // ---- a day's presses, and what a replaced export leaves
    let carol = api.person()?;
    let carol_id = api.identity(&carol)?;
    let mut ids = vec![];
    for _ in 0..3 {
        let r = press(api, &carol)?;
        ids.push(r.body["id"].as_str().unwrap_or("").to_string());
        built(s, api, &carol);
    }
    let fourth = press(api, &carol)?;
    let newest = view(api, &carol);
    s.ok(
        "three exports a day are the most: the fourth is refused (429), saying when the next may start",
        ids.iter().all(|i| fragment_proto::export::valid_export_id(i)) && ids[0] != ids[1] && fourth.status == 429 && fourth.message().contains("the next may start at") && newest["id"] == ids[2].as_str() && newest["state"] == "ready",
        format!("{ids:?} | {fourth} | {newest}"),
    );
    let old = api.signed(&carol, "GET", &format!("/api/export/{}/download", ids[0]), None)?;
    let metered = s.eventually(Duration::from_secs(30), || entries(api, &carol_id, &format!("export:{}:", ids[0])).iter().any(|e| e["ref"] == format!("export:{}:requests", ids[0]).as_str()));
    s.ok("one replaced is gone (404), and its usage is metered to its person", old.status == 404 && metered, format!("{old} {:?}", entries(api, &carol_id, "export:")));

    // ---- a limit: an export that would pass the archive's fails as it
    // builds, its view saying why, and downloads nothing; the next one,
    // within it, is made (the lever lowers the limit for this person alone)
    let erin = api.person()?;
    let erin_id = api.identity(&erin)?;
    let fat = s.named(api, &erin, "export-fat")?;
    let fc = s.create(api, &erin, &fat)?;
    s.commit(&fc, &[("notes.txt", Some(&bytes_of(200 * 1024, 7)))]);
    api.unsigned("POST", "/api/test/export", Some(&json!({ "identity": erin_id, "op": "cap", "steps": 64 * 1024 })))?;
    let r = press(api, &erin)?;
    let failed = built(s, api, &erin);
    let gone = api.signed(&erin, "GET", &format!("/api/export/{}/download", r.body["id"].as_str().unwrap_or("")), None)?;
    s.ok(
        "past the archive's limit it fails as it builds, its view saying why (too_large), and downloads nothing (404)",
        failed["state"] == "failed" && failed["error"]["code"] == "too_large" && failed["error"]["message"].as_str().is_some_and(|m| m.contains("the archive would pass")) && failed["download"].is_null() && gone.status == 404,
        format!("{failed} | {gone}"),
    );
    api.unsigned("POST", "/api/test/export", Some(&json!({ "identity": erin_id, "op": "cap", "steps": null })))?;
    let again = press(api, &erin)?;
    let made = built(s, api, &erin);
    s.ok("within its limit, the next is made", again.body["id"] != r.body["id"] && made["state"] == "ready", &made);

    // ---- the CLI: `fragment export` starts one, waits, and downloads it
    let home = s.dir("export-cli");
    s.login(api, &home);
    let out = home.join("mine.tar");
    let path = out.to_string_lossy().to_string();
    let r = s.cli_json(api, &home, &["export", "--out", &path, "--json"]);
    let saved = std::fs::read(&out).ok().and_then(|b| entries_of(&b).ok()).unwrap_or_default();
    s.ok(
        "fragment export starts one, waits for it, and downloads it: one tar, its README first",
        r.as_ref().is_ok_and(|v| v["export"]["state"] == "ready" && v["out"] == path.as_str()) && saved.get(&format!("{root}/README.md")).is_some_and(|b| b == fragment_core::export::README.as_bytes()),
        format!("{r:?}"),
    );
    Ok(())
}
