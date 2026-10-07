//! The real-Hermes lane (docs/cloudflare-v1.md, phase 4's exit): our Hermes
//! image (`images/hermes`: Hermes' desktop image, the bridge as its Relay
//! connector) on the platform's computers, under `wrangler dev` with
//! Docker, its model the platform's model route through the computer's
//! intercept, answered by the Workers AI fake from each call's transcript
//! (`workers_ai::transcript_reply`). The same flows as the computers lane,
//! with nothing but the image changed.
//!
//! It builds the Hermes image (3.8 GB), so it runs only by name
//! (`cargo xtask e2e --only hermes`).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use sha2::{Digest, Sha256};

use super::computers::{agent_replies, newest_save, phase, routine_app, told, turn_of, work_of, AGENT_JSON, CHAT_JSON, QUEUE_DRAIN, ROUTINE_JSON};
use super::jobs::records;
use crate::api::{Api, Call, Socket};
use crate::{Suite, SWAP_CONNECTION, SWAP_CONNECTION_ENV, SWAP_CONNECTION_HOST, SWAP_KEYS};

pub const SECTION: &str = "hermes";

/// A Hermes start: the container, `/data`'s restore, Hermes' gateway and
/// its first follow of each chat.
const WAKE: Duration = Duration::from_secs(300);
/// One Hermes turn against the scripted model.
const TURN: Duration = Duration::from_secs(120);
/// A routine's cron minute, and the wake it starts.
const ROUTINE: Duration = Duration::from_secs(420);
/// An agent assigned to the awake computer, to its first reply: the image
/// reads its agents every 3 s, writes the new one's profile, and has its
/// gateway serve it; its bridge follows the agent within a second of that
/// (the lower rung, `images/bridge/tests/docker.rs`, takes about one).
const NEW_AGENT: Duration = Duration::from_secs(30);
/// A skills fragment made while the computer is awake, to its managed set
/// installed: the image looks for one every minute while its owner has none
/// (images/hermes/boot: `skills::ABSENT_EVERY_MS`), then installs it.
const SKILLS_APPEAR: Duration = Duration::from_secs(120);
/// A 1×1 PNG: an image a person attaches.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15,
    0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0, 0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00,
    0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
/// An install for the session, offline: a package Hermes builds and
/// installs through apt as root, a program it puts in /usr/local/bin as
/// root, a file in its home; then both programs run, on one line (the
/// scripted model quotes a tool's first line).
const INSTALL: &str = r#"d=/tmp/fragment-hello && mkdir -p $d/DEBIAN $d/usr/bin && printf 'Package: fragment-hello\nVersion: 1.0\nArchitecture: all\nMaintainer: e2e <e2e@e2e.test>\nDescription: a package an agent installs\n' > $d/DEBIAN/control && printf '#!/bin/sh\necho hello-from-apt\n' > $d/usr/bin/fragment-hello && chmod 0755 $d $d/DEBIAN $d/usr/bin/fragment-hello && dpkg-deb --build --root-owner-group $d /tmp/fragment-hello.deb > /dev/null && sudo apt-get install -y /tmp/fragment-hello.deb > /dev/null 2>&1 && printf '#!/bin/sh\necho hello-from-usr-local\n' > /tmp/fragment-hi && sudo install -m 0755 /tmp/fragment-hi /usr/local/bin/fragment-hi && echo kept-in-its-home > /data/hermes/fragment-kept.txt && echo "$(fragment-hello) $(fragment-hi)""#;
/// After a sleep and a wake: the file in its home. Nothing is said of the
/// install: a wake from a snapshot keeps it, a start from the image does not.
const HOME_AFTER: &str = "cat /data/hermes/fragment-kept.txt";
/// A fragment someone shares with the agent's owner, whose `notes` its
/// editors post to.
const NOTES_JSON: &[u8] = br#"{ "channels": { "notes": { "read": "viewer", "post": "editor" } } }"#;

/// The last `n` messages of the newest model call whose messages hold
/// `said` (a failure's detail: what the model was given).
fn model_saw(s: &Suite, said: &str, n: usize) -> Value {
    let chats = s.ai.chats();
    let call = chats.iter().rev().find(|c| c["messages"].to_string().contains(said));
    let messages = call.and_then(|c| c["messages"].as_array().cloned()).unwrap_or_default();
    json!(messages[messages.len().saturating_sub(n)..])
}

/// The first number in brackets in `text` (the scripted model's count of
/// what the person said in the conversation it was given).
fn said_count(text: &str) -> Option<u32> {
    let open = text.rfind('[')?;
    text[open + 1..].split(']').next()?.parse().ok()
}

/// The Hermes images beside the stubs in the node's staged cell config:
/// `hermes`, and `hermes-next`, the same image another build (for the
/// upgrade and the rollback).
pub fn stage_images(project: &Path) -> Result<()> {
    let config = project.join("wrangler.jsonc");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&config)?)?;
    // the Hermes image builds from the repo's root: it carries the fragment CLI
    let root = fragment_devstack::repo_root();
    let dockerfile = root.join("images/hermes/Dockerfile");
    let images = v["containers"][0]["images"].as_object_mut().context("the staged config's container has images")?;
    images.insert("hermes".into(), json!({ "dockerfile": dockerfile, "build_context": root }));
    images.insert("hermes-next".into(), json!({ "dockerfile": dockerfile, "build_context": root, "build_vars": { "IMAGE_VERSION": "2" } }));
    std::fs::write(&config, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}

pub fn hermes(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section_by_name(SECTION, &[crate::Need::Fakes, crate::Need::LocalDocker], "it builds and runs the Hermes image") {
        return Ok(());
    }
    s.ai.clear_script();
    s.ai.transcripts(true);
    let answered = run(s, api);
    s.ai.transcripts(false);
    answered
}

/// Its screen through the platform (docs/computers.md, Ports; Paul,
/// 2026-10-05), before the agent has used its desktop: the first agent's
/// desktop starts for the screen's first viewer, its RFB stream and its
/// control socket pass through the computer's port, each server's first
/// word included, and in a frame of the platform's page (as the shell's
/// "Its computer's screen" opens it) the page connects and takes over; and
/// it follows the desktop: stopped under it, the page opens its stream
/// again on its own, the desktop started for it, Take over kept (p5,
/// 2026-10-05: the agent's browser showed only once the viewer was
/// reopened). That input then reaches the screen, and the browser shows on
/// it, is the lower rung's (`images/bridge/tests/docker.rs`,
/// `the_hermes_desktop`).
fn screen(s: &mut Suite, api: &Api, owner: &fragment_nip98::Keys, id: &str, stop_desktop: &dyn Fn(&Suite) -> Result<String>) -> Result<()> {
    let ticket = || -> Result<String> {
        let r = api.signed(owner, "POST", &format!("/api/computers/{id}/ports/6080/ticket"), Some(&json!({})))?;
        anyhow::ensure!(r.status == 200, "a ticket: {r}");
        Ok(r.body["url"].as_str().unwrap_or("").to_string())
    };
    let first = ticket()?;
    let origin = first.split("/__ticket").next().unwrap_or("").to_string();
    let r = api.call(Call { method: "GET", url: first, ..Call::default() })?;
    let cookie = r.cookies().into_iter().find(|c| c.starts_with("fragment_computer=")).map(|c| c.split(';').next().unwrap_or("").to_string());
    let socket = |path: &str| Socket::connect(api, &format!("{origin}/p/6080/{path}"), None, cookie.as_deref(), Some(&origin)).map(|(socket, _)| socket);
    let t = std::time::Instant::now();
    let greeting = socket("websockify?viewer=e2e").and_then(|mut rfb| {
        rfb.patience(Duration::from_secs(30))?;
        let version = rfb.bytes(12)?;
        rfb.close();
        Ok(String::from_utf8_lossy(&version).into_owned())
    });
    println!("      (the screen's first viewer to its RFB greeting: {:.1?})", t.elapsed());
    s.ok("its screen's RFB stream opens through its port before the agent used its desktop: the desktop starts for its first viewer", greeting.as_deref().is_ok_and(|g| g.starts_with("RFB 003.")), format!("{greeting:?}"));
    let control = socket("control?viewer=e2e").and_then(|mut c| {
        let first = c.next()?;
        c.close();
        Ok(first)
    });
    s.ok("its control socket says who holds the screen, through its port", control.as_ref().is_ok_and(|c| c["type"] == "control" && c["holder"].is_null()), format!("{control:?}"));

    let Some(mut chrome) = super::frames::safari_like(s)? else {
        s.ok("Chrome is installed for the computer's screen (set CHROME_BIN)", false, "no Chrome found");
        return Ok(());
    };
    let session = api.sign_in(&Api::email_of(owner))?;
    chrome.set_cookie(&format!("{}/", api.base), "fragment_session", &session)?;
    let shell = super::frames::platform_page(&mut chrome, api, super::frames::SIGNED_IN)?;
    super::isolation::frame(&mut chrome, &shell, &ticket()?)?;
    let computer = origin.split("//").nth(1).unwrap_or("").to_string();
    let wait = Duration::from_secs(60);
    // shown, and Take over ready: its button is enabled once both sockets are open
    let ready = "document.getElementById('status')?.textContent === \"Watching the agent's screen\" && !document.getElementById('control').disabled";
    let watching = s.eventually(wait, || chrome.eval_in_frame(&shell, &computer, ready).ok() == Some(json!(true)));
    let said = |chrome: &mut crate::browser::Browser| chrome.eval_in_frame(&shell, &computer, "document.getElementById('status')?.textContent ?? ''").unwrap_or_default();
    s.ok("in a frame of the platform's page, the screen's page connects to it (noVNC through the port), Take over ready", watching, said(&mut chrome));
    chrome.eval_in_frame(&shell, &computer, "document.getElementById('control').click(), true")?;
    let taken = super::isolation::frame_says(s, &mut chrome, &shell, &computer, "You have the screen", Duration::from_secs(20));
    s.ok("and Take over gives the person the screen", taken, said(&mut chrome));

    let stopped = stop_desktop(s)?;
    let connects = "Number(document.getElementById('screen')?.dataset.connects ?? 0)";
    let back = format!("{connects} >= 2 && document.getElementById('status')?.textContent === 'You have the screen' && !document.getElementById('control').disabled");
    let followed = s.eventually(wait, || chrome.eval_in_frame(&shell, &computer, &back).ok() == Some(json!(true)));
    let streams = chrome.eval_in_frame(&shell, &computer, connects).unwrap_or_default();
    s.ok(
        "the desktop stopped under the page, the page opens its stream again on its own: the desktop started for it, the person still holding the screen",
        followed,
        json!({ "agent": stopped, "status": said(&mut chrome), "streams": streams }),
    );
    Ok(())
}

fn run(s: &mut Suite, api: &Api) -> Result<()> {
    let owner = api.person()?;
    let owner_id = api.identity(&owner)?;
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let id = r.body["computer"].as_str().unwrap_or("").to_string();
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "hermes" })))?;
    s.ok("a person's computer, pinned to the Hermes image", r.status == 200 && r.body["image"] == "hermes", &r);

    // an agent fragment with a soul, a tier and skills of its own (one a
    // managed skill also names), and a chat it is in
    let agent_name = s.named(api, &owner, "juniper")?;
    let agent = s.create(api, &owner, &agent_name)?;
    s.commit(
        &agent,
        &[
            ("fragment.json", Some(AGENT_JSON)),
            ("SOUL.md", Some(b"You are Juniper, a careful gardener.\n")),
            ("agent.json", Some(br#"{"tier":"cheap"}"#)),
            ("skills/garden-notes/SKILL.md", Some(b"---\nname: garden-notes\ndescription: Juniper's notes on the garden.\n---\n")),
            ("skills/grill-me/SKILL.md", Some(b"---\nname: grill-me\ndescription: Juniper's own grill, which wins.\n---\n")),
        ],
    );
    s.deploy(&agent);
    // its owner has no skills fragment yet (one is made below, as settings'
    // "Add the managed skills" makes it): the agent still knows its platform
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    s.ok("its owner assigns the agent to it", r.status == 200 && identity.starts_with("id:"), &r);
    let chat_name = s.named(api, &owner, "chat")?;
    let chat = s.create(api, &owner, &chat_name)?;
    s.commit(&chat, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&chat);
    // the platform tells the agent's computer it joined, and wakes it
    let t0 = std::time::Instant::now();
    api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    let woke = s.eventually(WAKE, || phase(api, &owner, &id) == "awake");
    s.ok("adding its agent to the chat wakes it, joined posted on its tasks", woke && told(api, &owner, &agent_name, &chat_name).len() == 1, phase(api, &owner, &id));
    let subscribed = s.eventually(WAKE, || {
        api.signed(&owner, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
            .ok()
            .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
    });
    println!("      (Hermes following its chat {:.1?} after the wake)", t0.elapsed());
    s.ok("Hermes' bridge follows the chat as the agent", subscribed, "");

    let say = |n: u32, text: &str| api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": format!("h{n}"), "body": { "text": text } })));
    let turn_for = |r: &crate::api::Reply| turn_of(&agent_name, &chat_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
    let reply_of = |turn: &str| agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).into_iter().find(|r| r["body"]["turn"] == turn).and_then(|r| r["body"]["text"].as_str().map(str::to_string));
    let ended = |turn: &str| work_of(&records(api, &owner, &chat_name, "work"), turn).into_iter().find(|r| r["body"]["kind"] == "turn.end").map(|r| r["body"]["outcome"].clone());
    // the agent stops its desktop from its terminal, as Hermes' own command does
    let stop_desktop = |s: &Suite| -> Result<String> {
        let r = say(120, "run: /opt/hermes/.venv/bin/hermes computer-use screen stop")?;
        let turn = turn_for(&r);
        s.eventually(TURN, || ended(&turn).is_some());
        Ok(reply_of(&turn).unwrap_or_default())
    };

    // a reply, streamed: a page sees its draft live, then the reply
    let mut page = Socket::open(api, &chat_name, "__live", Some(&owner), None)?;
    page.until("hello", 5)?;
    let seq = records(api, &owner, &chat_name, "chat").last().and_then(|r| r["seq"].as_i64()).unwrap_or(0);
    page.send(&json!({ "type": "subscribe", "channel": "chat", "after": seq }))?;
    page.until("subscribed", 20)?;
    // the model fake keeps every section's calls: this check reads its own
    let calls_at_start = s.ai.calls().len();
    let t1 = std::time::Instant::now();
    let r = say(1, "hello hermes")?;
    let first = turn_for(&r);
    // Hermes' first reply after a wake takes 5 to 7 s on a laptop: past a
    // frame's usual 5 s
    page.patience(Duration::from_secs(30))?;
    let draft = page.until("draft", 600);
    let record = page.until("record", 600);
    page.close();
    println!("      (first message to its reply: {:.1?})", t1.elapsed());
    s.ok(
        "a page sees Hermes' reply drafted live, as the agent",
        draft.as_ref().is_ok_and(|d| d["principal"] == identity.as_str() && d["turn"] == first.as_str() && d["text"].as_str().is_some_and(|t| !t.is_empty())),
        format!("{draft:?}"),
    );
    s.ok(
        "then the reply, the model's answer, naming the draft's turn",
        s.eventually(TURN, || reply_of(&first).is_some_and(|t| t.contains("hello hermes"))) && record.as_ref().is_ok_and(|r| r["body"]["turn"] == first.as_str() || r["principal"] != identity.as_str()),
        json!({ "reply": reply_of(&first), "record": format!("{record:?}") }),
    );
    s.ok("its turn ends idle", s.eventually(TURN, || ended(&first) == Some(json!("idle"))), json!(work_of(&records(api, &owner, &chat_name, "work"), &first)));
    // with no skills fragment, the platform skill (images/hermes: the CLI's
    // own, after the computer's page) is the profile's: Hermes lists it to its
    // model in the turn's skills index, and no managed skill beside it
    let indexed = s.ai.chats().iter().find_map(|c| {
        let system = c["messages"].as_array()?.iter().find(|m| m["role"] == "system")?["content"].to_string();
        (c["messages"].to_string().contains("hello hermes") && system.contains("<available_skills>")).then_some(system)
    });
    s.ok(
        "with no skills fragment, Hermes lists the platform skill to its model: `fragment`, an agent on a Fragment computer",
        indexed.as_deref().is_some_and(|t| t.contains("- fragment: You are an agent on a Fragment computer") && t.contains("- garden-notes: ") && !t.contains("- apps-finite: ")),
        json!(indexed.as_deref().and_then(|t| t.find("<available_skills>").map(|at| t[at..].chars().take(1200).collect::<String>()))),
    );
    let calls = s.ai.calls().split_off(calls_at_start);
    s.ok(
        "every model call went through the platform's route, on the agent's tier, as the agent",
        !calls.is_empty() && calls.iter().all(|c| c.model.contains("flash") && c.metadata["agent_id"].is_string()),
        json!(calls.iter().map(|c| json!({ "model": c.model, "metadata": c.metadata })).collect::<Vec<_>>()),
    );
    let aig = super::ledger::entries(api, &owner_id, "aig:");
    s.ok("and each is settled on its owner's ledger", !aig.is_empty() && aig.iter().all(|e| super::ledger::end_of(e) == "settled"), json!(aig));

    // a tool step, then the answer that names it
    let r = say(2, "run: echo tool-ran")?;
    let tooled = turn_for(&r);
    s.eventually(TURN, || ended(&tooled).is_some());
    let steps: Vec<Value> = work_of(&records(api, &owner, &chat_name, "work"), &tooled).into_iter().filter(|r| r["body"]["kind"] == "turn.step").collect();
    // Hermes shows the command as a fenced block under its header: the
    // block is the step's arguments, never a step of its own
    s.ok(
        "Hermes' terminal call is one step on work, its command the step's arguments",
        steps.len() == 1 && steps[0]["body"]["tool"] == "terminal" && steps[0]["body"]["args"].as_str().is_some_and(|a| a.contains("echo tool-ran")),
        json!(steps),
    );
    s.ok("and its answer names the tool's result", reply_of(&tooled).is_some_and(|t| t.contains("the tool ran: ") && t.contains("tool-ran")), json!(reply_of(&tooled)));

    // a model's text beside its tool call (Paul on p5, 2026-10-05: one
    // message got two replies, the model's narration of its call, then the
    // answer): the turn's one reply is its answer, the call a step on work,
    // and the narration never a message of its own
    let r = say(20, "narrate: echo narrated-ran")?;
    let narrated = turn_for(&r);
    s.eventually(TURN, || ended(&narrated).is_some());
    let replies: Vec<String> = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).into_iter().filter(|r| r["body"]["turn"] == narrated.as_str()).filter_map(|r| r["body"]["text"].as_str().map(str::to_string)).collect();
    let steps: Vec<Value> = work_of(&records(api, &owner, &chat_name, "work"), &narrated).into_iter().filter(|r| r["body"]["kind"] == "turn.step").collect();
    s.ok(
        "a model's narration beside its tool call is no reply of its own: the turn's one reply is its answer, the call one step on work",
        replies.len() == 1
            && replies[0].contains("the tool ran: ")
            && replies[0].contains("narrated-ran")
            && !replies.iter().any(|t| t.contains(fragment_fakes::workers_ai::NARRATION))
            && steps.len() == 1
            && steps[0]["body"]["tool"] == "terminal"
            && steps[0]["body"]["args"].as_str().is_some_and(|a| a.contains("echo narrated-ran"))
            && ended(&narrated) == Some(json!("idle")),
        json!({ "replies": replies, "steps": steps, "ended": ended(&narrated) }),
    );

    // its screen, the agent's desktop not used yet; after the steps above:
    // with the screen's terminal call before the first, its step went missing in
    // 3 runs of 4 (Hermes sends progress on a tick, and a turn this model
    // ends at once can end before it)
    screen(s, api, &owner, &id, &stop_desktop)?;

    // an approval: Hermes flags `rm -rf`, its guardian escalates, the owner answers
    let r = say(3, "run: rm -rf /tmp/fragment-risky && echo tool-ran")?;
    let risky = turn_for(&r);
    let carded = s.eventually(TURN, || work_of(&records(api, &owner, &chat_name, "work"), &risky).iter().any(|r| r["body"]["kind"] == "turn.prompt"));
    let card = work_of(&records(api, &owner, &chat_name, "work"), &risky).into_iter().find(|r| r["body"]["kind"] == "turn.prompt").unwrap_or_default();
    s.ok("Hermes asks before a risky command: a card asking the agent's owner", carded && card["body"]["asks"] == owner_id.as_str(), &card);
    let prompt = card["body"]["prompt"].as_str().unwrap_or("").to_string();
    let once = card["body"]["options"].as_array().and_then(|o| o.iter().find(|o| o["id"] == "once").or(o.first())).and_then(|o| o["id"].as_str()).unwrap_or("once").to_string();
    let answer = json!({ "id": format!("pr:{prompt}"), "body": { "kind": "prompt_response", "prompt": prompt, "option": once } });
    api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&answer))?;
    let ran = s.eventually(TURN, || reply_of(&risky).is_some_and(|t| t.contains("the tool ran")));
    let closed = work_of(&records(api, &owner, &chat_name, "work"), &risky).into_iter().find(|r| r["body"]["kind"] == "turn.prompt.closed").unwrap_or_default();
    s.ok("its owner's answer runs it, the card closed as answered", ran && closed["body"]["outcome"] == "answered", json!({ "reply": reply_of(&risky), "closed": closed }));

    // Stop: its asker stops a turn waiting on its card
    let r = say(4, "run: rm -rf /tmp/fragment-stop && echo tool-ran")?;
    let stopping = turn_for(&r);
    s.eventually(TURN, || work_of(&records(api, &owner, &chat_name, "work"), &stopping).iter().any(|r| r["body"]["kind"] == "turn.prompt"));
    let stop = json!({ "id": "stop-1", "body": { "kind": "stop", "turn": stopping } });
    api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&stop))?;
    let stopped = s.eventually(TURN, || ended(&stopping) == Some(json!("stopped")));
    let closed = work_of(&records(api, &owner, &chat_name, "work"), &stopping).into_iter().find(|r| r["body"]["kind"] == "turn.prompt.closed").unwrap_or_default();
    s.ok(
        "its asker's Stop ends the turn stopped, its card closed, the command never run",
        stopped && closed["body"]["outcome"] == "stopped" && reply_of(&stopping).is_none_or(|t| !t.contains("the tool ran")),
        json!(work_of(&records(api, &owner, &chat_name, "work"), &stopping)),
    );

    // a person's attachment reaches the model
    let sha = hex::encode(Sha256::digest(PNG));
    let up = api.call(Call { method: "PUT", url: format!("{}/api/f/{chat_name}/blobs/{sha}", api.base), body: Some(PNG.to_vec()), content_type: Some("image/png"), keys: Some(&owner), ..Call::default() })?;
    let calls_before = s.ai.calls().len();
    let attachment = json!({ "sha256": sha, "size": PNG.len(), "type": "image/png", "name": "leaf.png" });
    let r = api.signed(&owner, "POST", &format!("/api/f/{chat_name}/channels/chat"), Some(&json!({ "id": "h5", "body": { "text": "what is on this leaf?", "attachments": [attachment] } })))?;
    let looked = turn_for(&r);
    s.eventually(TURN, || ended(&looked).is_some());
    let seen = s.ai.chats()[calls_before.min(s.ai.chats().len())..].iter().any(|c| {
        let t = c.to_string();
        t.contains("image_url") || t.contains("leaf.png") || t.contains("/relay/media/")
    });
    s.ok(
        "a person's image, uploaded as the chat's blob, reaches Hermes and its model",
        up.status == 200 && seen && reply_of(&looked).is_some(),
        json!({ "upload": up.status, "reply": reply_of(&looked), "ended": ended(&looked) }),
    );

    // its credentials in its terminal's environment (Paul, 2026-10-04): a
    // connection its owner makes (an agent may use every one its owner has:
    // decision 44), and the operator's keys, each a placeholder; and a stock
    // curl over HTTPS, with no header of ours, gets the swap
    s.workos.connect(&Api::email_of(&owner), SWAP_CONNECTION, true);
    // the owner's read of their connections tells the computer at once; the
    // image reads its credentials every 3 s and writes them into the profile
    api.signed(&owner, "GET", "/api/connections", None)?;
    let found = format!("printenv {SWAP_CONNECTION_ENV} PERPLEXITY_API_KEY GOOGLE_PLACES_API_KEY | cut -c1-15 | tr '\\n' ' '");
    let mut env_seen = None;
    for n in 0..6u32 {
        let r = say(70 + n, &format!("run: {found}"))?;
        let asked = turn_for(&r);
        s.eventually(TURN, || ended(&asked).is_some());
        env_seen = reply_of(&asked);
        if env_seen.as_deref().is_some_and(|t| t.contains("fcx_google_")) {
            break;
        }
        std::thread::sleep(Duration::from_secs(3));
    }
    s.ok(
        "Hermes' terminal finds each credential in its standard environment variable: a placeholder naming the agent, the connection's once its owner connects it",
        env_seen.as_deref().is_some_and(|t| t.contains("fcx_google_") && t.contains("fck_perplexity_") && t.contains("fck_google-plac")),
        json!(env_seen),
    );
    let seen_before = s.upstream.seen().len();
    let curl = format!("curl -s https://{SWAP_CONNECTION_HOST}/drive/v3/files -H \"Authorization: Bearer ${SWAP_CONNECTION_ENV}\"");
    let r = say(6, &format!("run: {curl}"))?;
    let swapped = turn_for(&r);
    s.eventually(TURN, || ended(&swapped).is_some());
    let seen = s.upstream.seen().get(seen_before).cloned().unwrap_or_default();
    let tokens = s.workos.tokens(SWAP_CONNECTION);
    s.ok(
        "a stock curl over HTTPS, with the connection's variable and no header of ours, reaches its host with the owner's token from Pipes in the placeholder's place",
        seen["host"] == SWAP_CONNECTION_HOST && tokens.last().is_some_and(|t| seen["auth"]["authorization"] == format!("Bearer {t}")) && seen["agent"].is_null(),
        json!({ "reply": reply_of(&swapped), "seen": seen }),
    );
    // Hermes keeps PERPLEXITY_API_KEY from its terminal's passthrough (its
    // own providers' names): the image's credentials file sets it
    let seen_before = s.upstream.seen().len();
    let r = say(77, "run: curl -s https://api.perplexity.ai/search -H \"Authorization: Bearer $PERPLEXITY_API_KEY\" -d '{\"query\":\"x\"}'")?;
    let keyed = turn_for(&r);
    s.eventually(TURN, || ended(&keyed).is_some());
    let seen = s.upstream.seen().get(seen_before).cloned().unwrap_or_default();
    let key = SWAP_KEYS.iter().find(|(k, _, _)| *k == "perplexity").map(|(_, v, _)| *v).unwrap_or("");
    s.ok(
        "an operator key's variable, one Hermes keeps from passthrough, is its terminal's too: a stock curl gets the key swapped in, metered",
        seen["host"] == "api.perplexity.ai" && seen["auth"]["authorization"] == format!("Bearer {key}"),
        json!({ "reply": reply_of(&keyed), "seen": seen }),
    );

    // The platform skill where its config says, and the managed skills and
    // its own, where its profile looks: its own `skills/` (its fragment's),
    // then the managed set and the platform skill's view its config names
    // (the images' Docker lane checks Hermes' own view of the three: its own
    // wins a name, and a managed one the platform skill's); and its terminal
    // runs the fragment CLI as itself, acting for its owner, with no key: it
    // lists its owner's fragments (their skills fragment, which it is no
    // member of, among them).
    let view = "/var/lib/fragment-run/platform-skills";
    let r = say(39, &format!("run: grep -q {view} \"$HERMES_HOME/config.yaml\" && sed -n 2p {view}/platform/fragment/SKILL.md | sed 's/^/platform-/'"))?;
    let asked = turn_for(&r);
    s.eventually(TURN, || ended(&asked).is_some());
    s.ok("its config names the platform skill, which is in the image, named `fragment`", reply_of(&asked).is_some_and(|t| t.contains("platform-name: fragment")), json!(reply_of(&asked)));
    // the owner's skills fragment appears (decision 17; a person who lacks
    // one adds it from settings): its computer, awake, installs its managed set
    // within a minute, as it looks for one that often while there is none
    let skills_label = s.name("skills");
    let r = api.create_with(&owner, json!({ "name": skills_label, "template": "skills" }))?;
    let skills_name = r.body["name"].as_str().unwrap_or("").to_string();
    s.ok("its owner then has a skills fragment on the blessed template", r.status == 200 && !skills_name.is_empty(), &r);
    let appeared = std::time::Instant::now();
    let managed = "/data/hermes/managed-skills/software-development/apps-finite/SKILL.md";
    let found = format!("cd \"$HERMES_HOME\" && grep -c managed-skills config.yaml && ls skills/garden-notes/SKILL.md {managed}");
    let has_both = |t: &str| t.contains("skills/garden-notes/SKILL.md") && t.contains(managed) && !t.contains("No such file");
    let mut skills_seen = None;
    // the managed set is installed off the boot's path: asked again until it
    // is, for the minute it may wait and the install
    for n in 0..20u32 {
        let r = say(80 + n, &format!("run: {found}"))?;
        let asked = turn_for(&r);
        s.eventually(TURN, || ended(&asked).is_some());
        skills_seen = reply_of(&asked);
        if skills_seen.as_deref().is_some_and(has_both) || appeared.elapsed() > SKILLS_APPEAR {
            break;
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    println!("      (the skills fragment made, to its managed set installed: {:.1?})", appeared.elapsed());
    s.ok(
        &format!("its profile has the managed skills within {}s of the skills fragment's making (its config naming them), and its own", SKILLS_APPEAR.as_secs()),
        skills_seen.as_deref().is_some_and(has_both),
        json!(skills_seen),
    );
    let r = say(50, &format!("run: fragment list | grep -c '^{skills_name} ' | sed 's/^/listed-/'"))?;
    let listed = turn_for(&r);
    s.eventually(TURN, || ended(&listed).is_some());
    let r = say(51, &format!("run: fragment whoami | grep -c '^agent: {agent_name} ' | sed 's/^/whoami-/'"))?;
    let whoami = turn_for(&r);
    s.eventually(TURN, || ended(&whoami).is_some());
    s.ok(
        "its terminal runs `fragment` as itself, with no key, acting for its owner: it lists its owner's fragments",
        reply_of(&listed).is_some_and(|t| t.contains("listed-1")) && reply_of(&whoami).is_some_and(|t| t.contains("whoami-1")),
        json!({ "list": reply_of(&listed), "whoami": reply_of(&whoami) }),
    );

    // phase 6's exit: from its chat, the agent builds an app, publishes it and
    // shares it (its link, then public, then with a person), then ingests a
    // source into a brain and searches it, all with the `fragment` CLI in its
    // terminal, acting for its owner
    let run = |s: &Suite, n: u32, cmd: &str| -> Result<Option<String>> {
        let r = say(n, &format!("run: {cmd}"))?;
        // a post the chat refused (its id another check's, say) runs
        // nothing: said at once, not after a whole turn's wait
        if r.status != 200 {
            return Ok(Some(format!("the post h{n} was refused: {r}")));
        }
        let turn = turn_for(&r);
        s.eventually(TURN, || ended(&turn).is_some());
        Ok(reply_of(&turn))
    };
    let said = |reply: &Option<String>, mark: &str| reply.as_deref().is_some_and(|t| t.contains(mark));
    let username = agent_name.split_once('.').map(|(_, u)| u.to_string()).unwrap_or_default();
    let owner_name = |label: &str| format!("{label}.{username}");
    let made = run(s, 60, "fragment create groceries --template todo --json | grep -c '\"ok\":true' | sed 's/^/made-/'")?;
    let wrote = run(s, 61, "fragment write groceries site/hello.html --text '<h1>Picked by the agent</h1>' --json | grep -c '\"ok\":true' | sed 's/^/wrote-/'")?;
    let live = run(s, 62, "fragment deploy groceries | grep -o 'view=[0-9a-f]*' | head -1 | sed 's/^/link-/'")?;
    let app = owner_name("groceries");
    let status = api.signed(&owner, "GET", &format!("/api/f/{app}/status"), None)?;
    let owned = status.body["owner"] == api.identity(&owner)?.as_str();
    let token = status.body["viewToken"].as_str().unwrap_or("").to_string();
    let page = api.call(Call { method: "GET", url: format!("{}?view={token}", api.site_url(&app, "hello.html")), ..Call::default() })?;
    s.ok(
        "from its chat, the agent makes an app (its owner's), writes a page, and publishes it",
        said(&made, "made-1") && said(&wrote, "wrote-1") && owned && page.text.contains("Picked by the agent"),
        json!({ "made": made, "wrote": wrote, "owner": status.body["owner"], "page": page.status }),
    );
    s.ok(
        "and shares it: its share link, which an anonymous visitor opens",
        said(&live, &format!("link-view={token}")) && !token.is_empty(),
        json!({ "reply": live, "token": !token.is_empty() }),
    );
    // it shares as its owner would (Paul, 2026-10-04): it makes the app
    // public, and adds another person to it as a viewer
    let guest = api.person()?;
    let guest_id = api.identity(&guest)?;
    let public = run(s, 66, "fragment visibility groceries public --json | grep -c '\"ok\":true' | sed 's/^/public-/'")?;
    let added = run(s, 67, &format!("fragment members add groceries {guest_id} --role viewer --json | grep -c '\"ok\":true' | sed 's/^/added-/'"))?;
    let anonymous = api.call(Call { method: "GET", url: api.site_url(&app, "hello.html"), ..Call::default() })?;
    let lists = api.signed(&guest, "GET", "/api/fragments", None)?;
    let guest_role = lists.body["fragments"].as_array().into_iter().flatten().find(|f| f["name"] == app.as_str()).map(|f| f["role"].clone());
    s.ok(
        "it makes the app public, which an anonymous visitor opens with no link",
        said(&public, "public-1") && anonymous.status == 200 && anonymous.text.contains("Picked by the agent"),
        json!({ "reply": public, "page": anonymous.status }),
    );
    s.ok(
        "and adds another person as a viewer, who sees it in their list as one",
        said(&added, "added-1") && guest_role == Some(json!("viewer")),
        json!({ "reply": added, "role": guest_role }),
    );
    let brain = run(s, 63, "fragment create garden --template brain --title 'Garden notes' --json | grep -c '\"ok\":true' | sed 's/^/brain-/'")?;
    let ingested = run(s, 64, "printf '# Tomatoes\\n\\nWater the tomatoes at dawn, before the heat.\\n' | fragment write garden garden/raw/tomatoes.md --from - --json | grep -c '\"ok\":true' | sed 's/^/ingested-/'")?;
    let found = run(s, 65, "fragment call garden search --input '{\"q\":\"tomatoes dawn\"}' --json | grep -o 'garden/raw/tomatoes.md' | head -1 | sed 's/^/found-/'")?;
    let brain_name = owner_name("garden");
    let searched = api.signed(&owner, "POST", &format!("/api/f/{brain_name}/ops/search"), Some(&json!({ "id": "e2e-brain-search", "input": { "q": "tomatoes" } })))?;
    s.ok(
        "then makes a brain, ingests a source into it, and finds it by searching",
        said(&brain, "brain-1") && said(&ingested, "ingested-1") && said(&found, "found-garden/raw/tomatoes.md"),
        json!({ "brain": brain, "ingested": ingested, "found": found }),
    );
    s.ok(
        "the brain is its owner's, and its search finds the source for its owner too",
        searched.status == 200 && searched.text.contains("garden/raw/tomatoes.md"),
        &searched,
    );

    // delegation (decision 36): Skyler shares a fragment with the owner as an
    // editor, and the owner's agent edits it acting for them (`?for=`); a
    // people-only share, or the agent held below its owner, refuses it
    let skyler = api.person()?;
    let notes_name = s.named(api, &skyler, "garden")?;
    let notes = s.create(api, &skyler, &notes_name)?;
    s.commit(&notes, &[("fragment.json", Some(NOTES_JSON))]);
    s.deploy(&notes);
    let share = |people_only: bool| api.signed(&skyler, "PUT", &format!("/api/f/{notes_name}/members/{owner_id}"), Some(&json!({ "role": "editor", "peopleOnly": people_only })));
    share(false)?;
    let post_note = |n: u32| {
        let body = json!({ "id": format!("d{n}"), "body": { "text": "from juniper" } }).to_string();
        format!("run: curl -s -o /dev/null -w 'status %{{http_code}}' -X POST 'http://api.fragment.internal/api/f/{notes_name}/channels/notes?for={owner_id}' -H 'x-fragment-agent: {agent_name}' -H 'content-type: application/json' -d '{body}'")
    };
    let status_of = |turn: &str| reply_of(turn).and_then(|t| t.split("status ").nth(1).map(|c| c.chars().take(3).collect::<String>()));
    let r = say(7, &post_note(1))?;
    let delegated = turn_for(&r);
    s.eventually(TURN, || ended(&delegated).is_some());
    let wrote = records(api, &skyler, &notes_name, "notes").into_iter().any(|r| r["principal"] == identity.as_str());
    s.ok(
        "the owner's agent edits a fragment shared with its owner as an editor, acting for its owner",
        status_of(&delegated).as_deref() == Some("200") && wrote,
        json!({ "reply": reply_of(&delegated), "notes": records(api, &skyler, &notes_name, "notes") }),
    );
    share(true)?;
    let r = say(8, &post_note(2))?;
    let refused = turn_for(&r);
    s.eventually(TURN, || ended(&refused).is_some());
    s.ok("a people-only share lends its agents nothing", status_of(&refused).as_deref() == Some("403"), json!(reply_of(&refused)));
    share(false)?;
    api.signed(&owner, "PUT", &format!("/api/identities/{identity}/held"), Some(&json!({ "held": "viewer" })))?;
    let r = say(9, &post_note(3))?;
    let held = turn_for(&r);
    s.eventually(TURN, || ended(&held).is_some());
    s.ok("an agent its owner holds at viewer edits nothing", status_of(&held).as_deref() == Some("403"), json!(reply_of(&held)));
    api.signed(&owner, "PUT", &format!("/api/identities/{identity}/held"), Some(&json!({ "held": null })))?;

    // an install for the session (Paul, 2026-10-05; docs/computers.md, "Root
    // in our Hermes image"): Hermes' user runs anything as root with sudo,
    // here offline (a package it builds, through apt; a program into
    // /usr/local/bin). Its ids are past the managed skills' asks (h80 to
    // h99), which a fresh image's slower install reaches, and the screen's
    // stop (h120).
    let installed = run(s, 130, INSTALL)?;
    s.ok(
        "Hermes installs software as root with passwordless sudo, a package through apt and a program into /usr/local/bin, and runs both",
        said(&installed, "hello-from-apt hello-from-usr-local"),
        json!({ "reply": installed }),
    );

    // a restart mid-turn: a turn waiting on its card when the computer sleeps
    let r = say(10, "run: rm -rf /tmp/fragment-restart && echo tool-ran")?;
    let lost = turn_for(&r);
    s.eventually(TURN, || work_of(&records(api, &owner, &chat_name, "work"), &lost).iter().any(|r| r["body"]["kind"] == "turn.prompt"));
    std::thread::sleep(QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    // its desktop has drawn (its screen, above), and the hold is answered
    // still (the hosted hold, 2026-10-06: the desktop's Mesa shader cache,
    // named `*.db`, failed the image's copy, so every hold after it went
    // unanswered)
    let newest = newest_save(api, &id);
    s.ok("its sleep's save, its desktop used, is held: the image answered the hold", newest["held"] == true, &newest);
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    s.ok("woken again", r.body["phase"] == "awake", &r);
    let closed = s.eventually(WAKE, || {
        let w = work_of(&records(api, &owner, &chat_name, "work"), &lost);
        w.iter().any(|r| r["body"]["kind"] == "turn.prompt.closed" && r["body"]["outcome"] == "expired") && w.iter().any(|r| r["body"]["kind"] == "turn.end" && r["body"]["outcome"] == "error")
    });
    s.ok(
        "a turn the restart cut short ends in an error, its card expired, its command never run",
        closed && reply_of(&lost).is_none_or(|t| !t.contains("the tool ran")),
        json!(work_of(&records(api, &owner, &chat_name, "work"), &lost)),
    );

    // a wake with restore: Hermes' conversation came back with /data
    let r = say(11, "do you remember")?;
    let remembered = turn_for(&r);
    s.eventually(TURN, || reply_of(&remembered).is_some());
    s.ok(
        "after a sleep and a wake, Hermes answers with the conversation it had before (its /data restored)",
        reply_of(&remembered).is_some_and(|t| t.contains("do you remember") && said_count(&t).is_some_and(|n| n > 1)),
        json!({ "reply": reply_of(&remembered), "model_saw": model_saw(s, "do you remember", 4) }),
    );
    // what it wrote in its home came back with /data; the install may or
    // may not have (docs/computers.md, "Root in our Hermes image")
    let after = run(s, 131, HOME_AFTER)?;
    s.ok(
        "after the sleep and the wake, what it wrote in its home beside the install is kept",
        said(&after, "kept-in-its-home"),
        json!({ "reply": after }),
    );

    // an upgrade, then a rollback, by pin
    let ticket = api.signed(&owner, "POST", &format!("/api/computers/{id}/ports/6080/ticket"), Some(&json!({})))?;
    let origin = ticket.body["url"].as_str().and_then(|u| u.split("/__ticket").next()).unwrap_or("").to_string();
    let version = || api.call(Call { method: "GET", url: format!("{origin}/p/6080/version.txt"), keys: Some(&owner), ..Call::default() }).map(|r| r.text.trim().to_string()).unwrap_or_default();
    s.ok("it runs the Hermes image's first build", version() == "1", version());
    api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "hermes-next" })))?;
    std::thread::sleep(QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    let r = say(12, "after the upgrade")?;
    let upgraded = turn_for(&r);
    s.eventually(WAKE, || reply_of(&upgraded).is_some());
    s.ok(
        "a message wakes it on the next build (an upgrade), its conversation kept",
        version() == "2" && reply_of(&upgraded).is_some_and(|t| said_count(&t).is_some_and(|n| n > 2)),
        json!({ "version": version(), "reply": reply_of(&upgraded) }),
    );
    api.signed(&owner, "PUT", &format!("/api/computers/{id}/image"), Some(&json!({ "image": "hermes" })))?;
    std::thread::sleep(QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    let r = say(13, "after the rollback")?;
    let rolled = turn_for(&r);
    s.eventually(WAKE, || reply_of(&rolled).is_some());
    s.ok(
        "and on the first build again (a rollback), its conversation kept",
        version() == "1" && reply_of(&rolled).is_some_and(|t| said_count(&t).is_some_and(|n| n > 3)),
        json!({ "version": version(), "reply": reply_of(&rolled) }),
    );

    // two profiles: a second agent assigned to the awake computer while the
    // lead's turn runs (docs/computers.md: a computer's agents may change
    // while it runs). The image writes its profile and its gateway serves it,
    // nothing restarted: it answers in its own chat as itself within
    // seconds, and the lead's turn runs on, whole
    let maple_name = s.named(api, &owner, "maple")?;
    let maple = s.create(api, &owner, &maple_name)?;
    s.commit(&maple, &[("fragment.json", Some(AGENT_JSON)), ("SOUL.md", Some(b"You are Maple, who tends the trees.\n"))]);
    s.deploy(&maple);
    let grove_name = s.named(api, &owner, "grove")?;
    let grove = s.create(api, &owner, &grove_name)?;
    s.commit(&grove, &[("fragment.json", Some(CHAT_JSON))]);
    s.deploy(&grove);
    api.signed(&owner, "POST", &format!("/api/computers/{id}/wake"), Some(&json!({})))?;
    let r = say(14, "run: sleep 20 && echo slow-ran")?;
    let slow = turn_for(&r);
    let running = s.eventually(TURN, || work_of(&records(api, &owner, &chat_name, "work"), &slow).iter().any(|r| r["body"]["kind"] == "turn.start"));
    let t0 = std::time::Instant::now();
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{id}/agents/{maple_name}"), Some(&json!({})))?;
    let maple_id = r.body["agents"].as_array().and_then(|a| a.iter().find(|x| x["fragment"] == maple_name.as_str())).and_then(|a| a["identity"].as_str()).unwrap_or("").to_string();
    s.ok(
        "a second agent is assigned to the awake computer while the lead's turn runs",
        running && r.status == 200 && maple_id.starts_with("id:") && maple_id != identity && r.body["phase"] == "awake" && ended(&slow).is_none(),
        json!({ "assigned": r.body, "slow": work_of(&records(api, &owner, &chat_name, "work"), &slow) }),
    );
    api.signed(&owner, "PUT", &format!("/api/f/{grove_name}/members/{maple_id}"), Some(&json!({ "role": "editor" })))?;
    let r = api.signed(&owner, "POST", &format!("/api/f/{grove_name}/channels/chat"), Some(&json!({ "id": "g1", "body": { "text": "hello maple" } })))?;
    let greeted = turn_of(&maple_name, &grove_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
    let maple_reply = |turn: &str| agent_replies(&records(api, &owner, &grove_name, "chat"), &maple_id).into_iter().find(|r| r["body"]["turn"] == turn).and_then(|r| r["body"]["text"].as_str().map(str::to_string));
    let answered = s.eventually(NEW_AGENT, || maple_reply(&greeted).is_some());
    let took = t0.elapsed();
    println!("      (a second agent, assigned while awake, to its first reply in its own chat: {took:.1?})");
    let maple_ended = || work_of(&records(api, &owner, &grove_name, "work"), &greeted).into_iter().find(|r| r["body"]["kind"] == "turn.end").map(|r| r["body"]["outcome"].clone());
    s.ok(
        &format!("it answers in its own chat within {}s, as itself: its own profile's model answer (no 401), its turn ended idle", NEW_AGENT.as_secs()),
        answered && maple_reply(&greeted).is_some_and(|t| t.contains("hello maple")) && s.eventually(TURN, || maple_ended() == Some(json!("idle"))),
        json!({ "tookMs": took.as_millis() as u64, "reply": maple_reply(&greeted), "work": work_of(&records(api, &owner, &grove_name, "work"), &greeted) }),
    );
    s.ok(
        "nothing restarted: the lead's turn ran on, and ends idle with its whole answer",
        s.eventually(TURN, || ended(&slow) == Some(json!("idle"))) && reply_of(&slow).is_some_and(|t| t.contains("slow-ran")) && phase(api, &owner, &id) == "awake",
        json!({ "reply": reply_of(&slow), "work": work_of(&records(api, &owner, &chat_name, "work"), &slow) }),
    );

    // its eyes (Paul, 2026-10-05): Maple is on the medium tier, whose
    // GLM-5.3 reads no images; its computer_use screenshot goes to the
    // route's `vision` (the deployment's vision model, GLM-5.3 Flash), as
    // Maple, metered to its owner, and its answer is what that model saw
    let calls_before = s.ai.calls().len();
    let r = api.signed(&owner, "POST", &format!("/api/f/{grove_name}/channels/chat"), Some(&json!({ "id": "g2", "body": { "text": "look at your screen" } })))?;
    let looked = turn_of(&maple_name, &grove_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
    let grove_ended = |turn: &str| work_of(&records(api, &owner, &grove_name, "work"), turn).into_iter().find(|r| r["body"]["kind"] == "turn.end").map(|r| r["body"]["outcome"].clone());
    s.eventually(TURN, || grove_ended(&looked).is_some());
    let calls: Vec<fragment_fakes::workers_ai::AiCall> = s.ai.calls().into_iter().skip(calls_before).collect();
    let maple_opaque = hex::encode(&Sha256::digest(maple_id.as_bytes())[..8]);
    let shown = |c: &fragment_fakes::workers_ai::AiCall| c.body["messages"].to_string().contains("\"image_url\"");
    let seen: Vec<Value> = calls.iter().map(|c| json!({ "model": c.model, "agent": c.metadata["agent_id"], "image": shown(c) })).collect();
    let looks: Vec<&fragment_fakes::workers_ai::AiCall> = calls.iter().filter(|c| shown(c)).collect();
    s.ok(
        "a medium-tier agent's computer_use screenshot goes to the route's vision model (GLM-5.3 Flash), as that agent, its main calls on GLM-5.3 and none of them shown an image",
        !looks.is_empty()
            && looks.iter().all(|c| c.model == fragment_core::models::VISION_MODEL_DEFAULT && c.metadata["agent_id"] == maple_opaque.as_str())
            && calls.iter().any(|c| c.model == fragment_core::models::MEDIUM_MODEL && c.metadata["agent_id"] == maple_opaque.as_str())
            && calls.iter().filter(|c| c.model == fragment_core::models::MEDIUM_MODEL).all(|c| !shown(c)),
        json!(seen),
    );
    s.ok(
        "and its answer is what the vision model saw of its screen",
        maple_reply(&looked).is_some_and(|t| t.contains("the screen: I see an image, a ")),
        json!({ "reply": maple_reply(&looked), "work": work_of(&records(api, &owner, &grove_name, "work"), &looked) }),
    );
    let vision_entries: Vec<Value> = super::ledger::entries(api, &owner_id, "aig:")
        .into_iter()
        .filter(|e| e["entry"]["reserve"]["agent"] == maple_id.as_str() && e["entry"]["reserve"]["worst"]["model"] == fragment_core::models::VISION_MODEL_DEFAULT)
        .collect();
    s.ok(
        "each vision call is metered to the agent's owner, as the agent, settled at the vision model's price",
        vision_entries.len() >= looks.len() && !looks.is_empty() && vision_entries.iter().all(|e| super::ledger::end_of(e) == "settled" && e["entry"]["end"]["usage"]["model"] == fragment_core::models::VISION_MODEL_DEFAULT),
        json!(vision_entries),
    );

    // added to the lead's chat while awake: @mentioned it answers there, the
    // lead does not (its view of the chat is read again on the join)
    api.signed(&owner, "PUT", &format!("/api/f/{chat_name}/members/{maple_id}"), Some(&json!({ "role": "editor" })))?;
    let wakes = || {
        api.signed(&owner, "GET", &format!("/api/f/{chat_name}/subscriptions"), None)
            .ok()
            .map_or(0, |r| r.body["subscriptions"].as_array().map_or(0, |l| l.iter().filter(|x| x["wake"] == true && x["channel"] == "chat").count()))
    };
    let followed = s.eventually(NEW_AGENT, || wakes() >= 2);
    let maple_label = maple_name.split('.').next().unwrap_or("").to_string();
    let lead_before = agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len();
    let r = say(15, &format!("@{maple_label} what do you think"))?;
    let mentioned = turn_of(&maple_name, &chat_name, "chat", r.body["record"]["seq"].as_i64().unwrap_or(0));
    let heard = s.eventually(TURN, || agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id).iter().any(|r| r["body"]["turn"] == mentioned.as_str()));
    s.ok("a second Hermes profile, joined to the lead's chat while awake, answers there when @mentioned", followed && heard, json!(agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id)));
    std::thread::sleep(Duration::from_secs(5));
    s.ok("and the lead does not", agent_replies(&records(api, &owner, &chat_name, "chat"), &identity).len() == lead_before, "");

    // a routine: the second agent's cron wakes the sleeping computer on time.
    // Its cron runs each minute from its deploy: one answered awake first,
    // so the sleep comes a minute before the next
    s.commit(&maple, &[("fragment.json", Some(ROUTINE_JSON)), ("app.mjs", Some(routine_app(&chat_name).as_bytes()))]);
    s.deploy(&maple);
    let routines =|| agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id).into_iter().filter(|r| r["body"]["text"].as_str().is_some_and(|t| t.contains("water the plants"))).count();
    let awake_routines = routines();
    s.eventually(ROUTINE, || routines() > awake_routines);
    std::thread::sleep(QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    let r = api.signed(&owner, "GET", &format!("/api/computers/{id}"), None)?;
    s.ok("asleep, it waits for the routine", r.body["phase"] == "asleep", &r);
    let routine_before = routines();
    let ran = s.eventually(ROUTINE, || routines() > routine_before);
    s.ok("its cron's routine wakes it, and Hermes does it in the chat", ran, json!(agent_replies(&records(api, &owner, &chat_name, "chat"), &maple_id)));

    std::thread::sleep(QUEUE_DRAIN);
    let r = api.signed(&owner, "POST", &format!("/api/computers/{id}/sleep"), Some(&json!({})))?;
    s.ok("it sleeps at the end", r.body["phase"] == "asleep", &r);
    Ok(())
}
