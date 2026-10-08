//! The mind (docs/optchat.md; `templates/mind`, a blessed template) on the
//! fakes: the scripted model (a `[[call NAME {json}]]` in a turn's message
//! is that tool call), Clef's fake, and the stub image's scripted agent on a
//! computer under `wrangler dev` with Docker as its hands.
//!
//! A person makes their mind (members only) and says something on `say`:
//! its trigger's turn logs the message and a reply as `msg` records on
//! `log`, the reply streamed first as a draft. A message asking for a zoom
//! makes the turn call the tool, logged as `tool` and `echo`. The
//! compactor builds every message's node, so the memory's parts are all
//! summaries and the view settles over the whole log; search finds a
//! message; a topic added classifies the threads about it, and only them.
//! With its agent (the stub) an editor of the mind, a turn of a persona
//! with hands hands a task to it on `chat`, recorded under the turn the
//! agent's bridge gives that record; the agent's one reply is the task's
//! report, and comes back as a `[<task>] …` user message that runs a turn
//! of its own. Its steps on `work` start nothing (a page follows them).
//!
//! Files: a message's text file (the mind's blob, named on `say`) is read
//! whole into its turn, goes with the hand-off on `chat`, the stub names
//! it, and its reply's file comes back on the report and is read into the
//! next turn.

use std::time::Duration;

use anyhow::Result;
use fragment_core::blob::sha256_hex;
use serde_json::{json, Value};

use super::computers::{agent_replies, phase, told, turn_of, AGENT_JSON};
use super::jobs::records;
use crate::api::{Api, Call, Socket};
use crate::Keys;
use crate::Suite;

/// A turn on the fakes, its steps one Workflow step each: well under this.
const TURN: Duration = Duration::from_secs(60);
/// A start of the stub and its bridge's first follow (as the computers lane's).
const WAKE: Duration = Duration::from_secs(90);

/// Words a message pads itself with, past a free node's 512 bytes, so its
/// node is the compactor's and no view line repeats its tool call. None is
/// a word of Clef's question ("this", "conversation", "about").
fn padded(text: &str) -> String {
    format!("{text} {}", "lorem ipsum dolor sit amet ".repeat(24))
}

/// The mind's `log` records of one type.
fn logged(api: &Api, owner: &Keys, mind: &str, kind: &str) -> Vec<Value> {
    api.signed(owner, "GET", &format!("/api/f/{mind}/channels/log?after=0&limit=1000"), None)
        .ok()
        .and_then(|r| r.body["records"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r["body"]["type"] == kind)
        .collect()
}

/// The `msg` records of a thread, as the page follows them.
fn messages(api: &Api, owner: &Keys, mind: &str, thread: &str) -> Vec<Value> {
    logged(api, owner, mind, "msg").into_iter().filter(|r| r["body"]["thread"] == thread).map(|r| r["body"].clone()).collect()
}

fn op(api: &Api, owner: &Keys, mind: &str, name: &str, input: Value) -> Value {
    api.op(owner, mind, name, &format!("{name}-{}", crate::api::now_ms()), input).map(|r| r.body["result"].clone()).unwrap_or(Value::Null)
}

pub fn mind(s: &mut Suite, api: &Api) -> Result<()> {
    if !s.section("mind", &[crate::Need::Fakes, crate::Need::Computers]) {
        return Ok(());
    }
    let owner = api.person()?;
    let mind = s.named(api, &owner, "mind")?;
    let r = api.create_with(&owner, json!({ "name": mind, "template": "mind", "visibility": "members", "title": "Mind" }))?;
    anyhow::ensure!(r.status == 200, "making the mind on the template: {r}");
    s.owned(&r.body, &owner);
    let npub = r.body["npub"].as_str().unwrap_or("").to_string();
    let code = |api: &Api| api.status(&owner, &mind).map(|r| r.body["code"].clone()).unwrap_or_default();
    let installed = s.eventually(Duration::from_secs(30), || code(api)["operations"]["heard"]["kind"] == "job");
    let ops = code(api)["operations"].clone();
    s.ok(
        "a mind runs the template's code from the release, its MCP tools described",
        installed && code(api)["error"].is_null() && ["view", "zoom", "date", "search", "note"].iter().all(|o| ops[o]["description"].is_string()) && ops["threads"].get("description").is_none_or(Value::is_null),
        code(api),
    );
    let personas = op(api, &owner, &mind, "personas", json!({}));
    let ids: Vec<&str> = personas["personas"].as_array().into_iter().flatten().filter_map(|p| p["id"].as_str()).collect();
    s.ok("it starts with three personas, Mind the default and Builder's hands on", ids == ["mind", "builder", "coach"] && personas["default"] == "mind" && personas["personas"][1]["hands"] == true, &personas);

    // ---- a first message: logged, answered, the answer drafted first
    let mut page = Socket::open(api, &mind, "__live", Some(&owner), None)?;
    page.until("hello", 5)?;
    page.send(&json!({ "type": "subscribe", "channel": "log", "after": 0 }))?;
    page.until("subscribed", 5)?;
    page.patience(TURN)?;
    let garden = "t_0123456789abcdef";
    let say = |id: &str, body: Value| api.signed(&owner, "POST", &format!("/api/f/{mind}/channels/say"), Some(&json!({ "id": id, "body": body })));
    let r = say("m1", json!({ "text": "hello mind, the garden has tomatoes and basil", "thread": garden }))?;
    s.ok("the owner says something on say", r.status == 200, &r);
    let mut drafts = vec![];
    let mut reply = Value::Null;
    // bounded: each frame within the socket's patience, at most 200
    for _ in 0..200 {
        let Ok(frame) = page.next() else { break };
        if frame["type"] == "draft" && frame["turn"] == format!("turn:{garden}").as_str() {
            drafts.push(frame.clone());
        }
        if frame["type"] == "record" && frame["body"]["type"] == "msg" && frame["body"]["kind"] == "talk" && frame["body"]["thread"] == garden {
            reply = frame["body"].clone();
            break;
        }
    }
    page.close();
    let said = messages(api, &owner, &mind, garden);
    s.ok(
        "its turn logs the message and a reply, each a msg record on log",
        said.len() == 2 && said[0]["kind"] == "user" && said[0]["i"] == 0 && said[0]["text"] == "hello mind, the garden has tomatoes and basil" && said[0]["persona"] == "mind" && said[1]["kind"] == "talk" && said[1]["i"] == 1,
        json!(said),
    );
    s.ok(
        "a page sees the reply drafted on log, as the mind, under the thread's turn, then its record",
        !drafts.is_empty() && reply["kind"] == "talk" && drafts.iter().all(|d| d["channel"] == "log" && d["principal"] == npub.as_str() && d["text"].as_str().is_some_and(|t| reply["text"].as_str().is_some_and(|w| w.starts_with(t)))),
        json!({ "drafts": drafts, "reply": reply }),
    );
    let threads = logged(api, &owner, &mind, "thread");
    let turns = logged(api, &owner, &mind, "turn");
    s.ok(
        "the thread is made, titled from its first line, and the turn says it thought and is done",
        threads.iter().any(|t| t["body"]["id"] == garden && t["body"]["title"] == "hello mind, the garden has tomatoes and basil")
            && turns.iter().any(|t| t["body"]["thread"] == garden && t["body"]["state"] == "thinking")
            && turns.iter().any(|t| t["body"]["thread"] == garden && t["body"]["state"] == "done"),
        json!({ "threads": threads, "turns": turns }),
    );
    let calls = s.ai.chats();
    let first = calls.iter().find(|c| c["messages"][1]["content"][1]["text"] == "hello mind, the garden has tomatoes and basil");
    s.ok(
        "the turn's call is the system prompt, then the view before the message and the message whole, with zoom, date and search",
        first.is_some_and(|c| {
            c["messages"][0]["content"].as_str().is_some_and(|p| p.starts_with("You are Mind, an AI agent"))
                && c["messages"][1]["content"][0]["text"] == "<chat>\n</chat>"
                && c["tools"].as_array().is_some_and(|t| t.iter().filter_map(|t| t["function"]["name"].as_str()).collect::<Vec<_>>() == ["zoom", "date", "search"])
        }),
        format!("{first:?}"),
    );

    // ---- a tool: zoom, logged as tool and echo
    let r = say("m2", json!({ "text": padded("please open the first message [[call zoom {\"id\":0,\"n\":1}]]"), "thread": garden }))?;
    anyhow::ensure!(r.status == 200, "saying m2: {r}");
    let zoomed = s.eventually(TURN, || messages(api, &owner, &mind, garden).iter().any(|m| m["kind"] == "talk" && m["text"].as_str().is_some_and(|t| t.starts_with("the tool said: "))));
    let said = messages(api, &owner, &mind, garden);
    let kinds: Vec<&str> = said.iter().filter_map(|m| m["kind"].as_str()).collect();
    s.ok(
        "a turn's tool call is logged as tool, its result as echo (zoom(0, 1): the first message whole), then the reply",
        zoomed
            && kinds == ["user", "talk", "user", "tool", "echo", "talk"]
            && said[3]["text"] == "zoom {\"id\":0,\"n\":1}"
            && said[4]["text"] == "0+0|user: hello mind, the garden has tomatoes and basil",
        json!(said),
    );

    // ---- the compactor: every node built, the view settled over the log
    let settled = s.eventually(TURN, || {
        let m = op(api, &owner, &mind, "memory", json!({}));
        m["parts"].as_array().is_some_and(|p| !p.is_empty() && p.iter().all(|x| x["built"] == true)) && m["T"] == 6
    });
    let memory = op(api, &owner, &mind, "memory", json!({}));
    let view = op(api, &owner, &mind, "view", json!({}));
    s.ok("the pump builds every message's node: the memory's parts are all summaries", settled, &memory);
    s.ok(
        "and the view settles over the whole log, a short message its own line, word for word",
        view["settled"] == true && view["T"] == 6 && view["text"].as_str().is_some_and(|t| t.starts_with("<chat>\n0+1|user: hello mind, the garden has tomatoes and basil\n") && t.ends_with("</chat>")),
        &view,
    );
    let compacted = s.ai.chats().iter().any(|c| c["messages"][0]["content"].as_str().is_some_and(|p| p.starts_with("You write the memory of Mind")) && c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.contains("Compress this message into one line, in at most 512 bytes:\nuser: please open the first message")));
    s.ok("the long message's node is the compactor's, asked with its context and SCALE", compacted, "");
    let z = op(api, &owner, &mind, "zoom", json!({ "id": 0, "n": 1 }));
    s.ok("zoom answers a message whole", z["text"] == "0+0|user: hello mind, the garden has tomatoes and basil", &z);

    // ---- search
    let found = op(api, &owner, &mind, "search", json!({ "q": "tomatoes" }));
    s.ok(
        "search finds the message, newest first",
        found["results"].as_array().is_some_and(|r| r.iter().any(|h| h["i"] == 0 && h["kind"] == "user" && h["thread"] == garden)),
        &found,
    );

    // ---- topics: a thread about the garden, and one not
    let kitchen = "t_fedcba9876543210";
    let r = say("m3", json!({ "text": "what should I cook tonight", "thread": kitchen }))?;
    anyhow::ensure!(r.status == 200, "saying m3: {r}");
    s.eventually(TURN, || messages(api, &owner, &mind, kitchen).iter().any(|m| m["kind"] == "talk"));
    let added = op(api, &owner, &mind, "topic_add", json!({ "name": "Garden", "description": "plants" }));
    let topic = added["id"].as_str().unwrap_or("").to_string();
    let sorted = s.eventually(TURN, || {
        op(api, &owner, &mind, "threads", json!({ "topic": topic })).get("threads").and_then(Value::as_array).is_some_and(|t| t.len() == 1)
    });
    let listed = op(api, &owner, &mind, "threads", json!({ "topic": topic }));
    s.ok(
        "a topic added classifies the newest threads by Clef: the garden's is in it, the kitchen's is not",
        sorted && listed["threads"][0]["id"] == garden && listed["threads"][0]["topics"][0]["id"] == topic.as_str() && listed["threads"][0]["count"].as_i64().is_some_and(|n| n >= 2),
        &listed,
    );
    let topics = op(api, &owner, &mind, "topics", json!({}));
    s.ok("the topic counts its thread", topics["topics"][0]["name"] == "Garden" && topics["topics"][0]["count"] == 1, &topics);
    s.ok("its threads' topics are published on log", logged(api, &owner, &mind, "topics").iter().any(|t| t["body"]["thread"] == garden), "");

    // ---- hands: the stub agent, an editor of the mind
    let r = api.signed(&owner, "POST", "/api/computers", Some(&json!({})))?;
    let computer = r.body["computer"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200 && computer.starts_with("computer:"), "making the computer: {r}");
    let agent_name = s.named(api, &owner, "goose")?;
    let agent = s.create(api, &owner, &agent_name)?;
    s.commit(&agent, &[("fragment.json", Some(AGENT_JSON))]);
    s.deploy(&agent);
    let r = api.signed(&owner, "PUT", &format!("/api/computers/{computer}/agents/{agent_name}"), Some(&json!({})))?;
    let identity = r.body["agents"][0]["identity"].as_str().unwrap_or("").to_string();
    anyhow::ensure!(r.status == 200 && identity.starts_with("id:"), "assigning the agent: {r}");
    let r = api.signed(&owner, "PUT", &format!("/api/f/{mind}/members/{identity}"), Some(&json!({ "role": "editor" })))?;
    s.ok("the agent joins the mind as an editor", r.status == 200, &r);
    s.eventually(Duration::from_secs(10), || told(api, &owner, &agent_name, &mind).len() == 1);
    let woke = s.eventually(WAKE, || phase(api, &owner, &computer) == "awake");
    let following = woke
        && s.eventually(WAKE, || {
            api.signed(&owner, "GET", &format!("/api/f/{mind}/subscriptions"), None)
                .ok()
                .is_some_and(|r| r.body["subscriptions"].as_array().is_some_and(|l| l.iter().any(|x| x["wake"] == true && x["channel"] == "chat")))
        });
    s.ok("its computer wakes and follows the mind's chat", following, phase(api, &owner, &computer));

    let shed = "t_00112233445566aa";
    let r = say("m4", json!({ "text": padded("please hand it over [[call computer {\"task\": \"tidy the shed\"}]]"), "thread": shed, "persona": "builder" }))?;
    anyhow::ensure!(r.status == 200, "saying m4: {r}");
    let opened = s.eventually(TURN, || op(api, &owner, &mind, "tasks", json!({ "thread": shed }))["tasks"].as_array().is_some_and(|t| t.len() == 1));
    let tasks = op(api, &owner, &mind, "tasks", json!({ "thread": shed }));
    let task = tasks["tasks"][0]["id"].as_str().unwrap_or("").to_string();
    let chat = records(api, &owner, &mind, "chat");
    let handed = chat.iter().find(|r| r["principal"] == npub.as_str());
    s.ok(
        "a persona with hands hands the task to the agent on chat, as the mind, and records it",
        opened && handed.is_some_and(|h| h["body"]["to"] == json!([identity]) && h["body"]["text"] == format!("tidy the shed\n\n(task {task}, thread {shed})").as_str()),
        json!({ "tasks": tasks, "chat": chat }),
    );
    let reported = s.eventually(WAKE, || {
        messages(api, &owner, &mind, shed).iter().any(|m| m["kind"] == "user" && m["task"] == task.as_str() && m["text"].as_str().is_some_and(|t| t.starts_with(&format!("[{task}] "))))
    });
    let said = messages(api, &owner, &mind, shed);
    let report = said.iter().find(|m| m["kind"] == "user" && m["task"] == task.as_str()).cloned().unwrap_or(Value::Null);
    // the agent's one reply on chat, under the turn its bridge gave the task's record
    let seq = handed.and_then(|h| h["seq"].as_i64()).unwrap_or(-1);
    let turn = turn_of(&agent_name, &mind, "chat", seq);
    let replies: Vec<Value> = agent_replies(&records(api, &owner, &mind, "chat"), &identity).into_iter().filter(|r| r["body"]["turn"] == turn.as_str()).collect();
    s.ok(
        "the agent replies once, and that reply comes back as the [task] user message in the thread",
        reported
            && replies.len() == 1
            && replies[0]["body"]["text"].as_str().is_some_and(|t| t.contains("tidy the shed") && report["text"] == format!("[{task}] {t}").as_str()),
        json!({ "said": said, "replies": replies }),
    );
    let answered = s.eventually(TURN, || messages(api, &owner, &mind, shed).iter().any(|m| m["kind"] == "talk" && m["i"].as_i64() > report["i"].as_i64()));
    s.ok("which runs a turn of its own", answered, json!(messages(api, &owner, &mind, shed)));
    let work = records(api, &owner, &mind, "work");
    let done = op(api, &owner, &mind, "tasks", json!({ "thread": shed }));
    s.ok(
        "the task names the agent's turn, claimed on work, and is done with its reply as the report",
        work.iter().any(|r| r["principal"] == identity.as_str() && r["body"]["kind"] == "turn.start" && r["body"]["turn"] == turn.as_str() && r["body"]["cause"]["seq"] == seq)
            && done["tasks"][0]["turn"] == turn.as_str()
            && done["tasks"][0]["state"] == "done"
            && done["tasks"][0]["report"] == replies.first().map_or(Value::Null, |r| r["body"]["text"].clone())
            && logged(api, &owner, &mind, "task").iter().any(|t| t["body"]["id"] == task.as_str() && t["body"]["turn"] == turn.as_str() && t["body"]["state"] == "done"),
        json!({ "work": work, "tasks": done }),
    );
    let runs = api.signed(&owner, "GET", &format!("/api/f/{mind}/triggers"), None)?;
    s.ok(
        "goose's steps on work start no run: only its reply on chat does",
        runs.body["triggers"].as_array().is_some_and(|t| t.iter().all(|t| t["channel"] != "work") && t.iter().any(|t| t["channel"] == "chat" && t["run"] == "hands_said")),
        &runs,
    );

    // ---- files: a text file said with a message, read whole into its
    // turn, handed on with the task; goose's file back on its report
    let notes = b"the spare key hangs on the third hook\n".to_vec();
    let notes_sha = sha256_hex(&notes);
    let r = api.call(Call {
        method: "PUT",
        url: format!("{}/api/f/{mind}/blobs/{notes_sha}", api.base),
        body: Some(notes.clone()),
        content_type: Some("text/plain"),
        keys: Some(&owner),
        ..Call::default()
    })?;
    anyhow::ensure!(r.status == 200, "uploading notes.txt to the mind: {r}");
    let file = json!({ "sha256": notes_sha, "name": "notes.txt", "type": "text/plain", "size": notes.len() });
    let hooks = "t_00112233445566bb";
    let r = say("m6", json!({ "text": padded("draw the hooks [[call computer {\"task\": \"draw the hooks\"}]]"), "thread": hooks, "persona": "builder", "attachments": [file] }))?;
    anyhow::ensure!(r.status == 200, "saying m6: {r}");
    let opened = s.eventually(TURN, || op(api, &owner, &mind, "tasks", json!({ "thread": hooks }))["tasks"].as_array().is_some_and(|t| t.len() == 1));
    let task = op(api, &owner, &mind, "tasks", json!({ "thread": hooks }))["tasks"][0]["id"].as_str().unwrap_or("").to_string();
    let said = messages(api, &owner, &mind, hooks);
    s.ok("a message's file is named on its msg record, without its text", opened && said.first().is_some_and(|m| m["kind"] == "user" && m["attachments"] == json!([file])), json!(said));
    let shown = format!("[file: notes.txt (text/plain, {} B)]\n```\nthe spare key hangs on the third hook\n```", notes.len());
    let call = s.ai.chats().into_iter().find(|c| c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.starts_with("draw the hooks")));
    s.ok(
        "its turn reads the text file whole, below its name (job.blob)",
        call.as_ref().is_some_and(|c| c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.ends_with(&shown))),
        format!("{call:?}"),
    );
    let first = said.first().and_then(|m| m["i"].as_i64()).unwrap_or(-1);
    let z = op(api, &owner, &mind, "zoom", json!({ "id": first, "n": 1 }));
    let listed = op(api, &owner, &mind, "thread", json!({ "id": hooks }));
    s.ok(
        "zoom opens the message with its file; the thread lists it named",
        z["text"].as_str().is_some_and(|t| t.ends_with(&shown)) && listed["messages"][0]["attachments"] == json!([file]),
        json!({ "zoom": z, "thread": listed }),
    );
    let handed = records(api, &owner, &mind, "chat").into_iter().find(|r| r["principal"] == npub.as_str() && r["body"]["text"].as_str().is_some_and(|t| t.ends_with(&format!("(task {task}, thread {hooks})"))));
    s.ok("the hand-off carries the turn's file on chat", handed.as_ref().is_some_and(|h| h["body"]["attachments"] == json!([file])), format!("{handed:?}"));
    let reported = s.eventually(WAKE, || messages(api, &owner, &mind, hooks).iter().any(|m| m["kind"] == "user" && m["task"] == task.as_str()));
    let report = messages(api, &owner, &mind, hooks).into_iter().find(|m| m["kind"] == "user" && m["task"] == task.as_str()).unwrap_or(Value::Null);
    let seq = handed.as_ref().and_then(|h| h["seq"].as_i64()).unwrap_or(-1);
    let turn = turn_of(&agent_name, &mind, "chat", seq);
    let reply = agent_replies(&records(api, &owner, &mind, "chat"), &identity).into_iter().find(|r| r["body"]["turn"] == turn.as_str()).unwrap_or(Value::Null);
    s.ok(
        "goose got the file (the stub names it), and its reply's file comes back on the report",
        reported
            && reply["body"]["text"].as_str().is_some_and(|t| t.ends_with("[got 1: notes.txt]"))
            && reply["body"]["attachments"][0]["name"] == "drawing.txt"
            && report["attachments"][0]["name"] == "drawing.txt"
            && report["attachments"][0]["sha256"] == reply["body"]["attachments"][0]["sha256"],
        json!({ "reply": reply, "report": report }),
    );
    let read_back = s.eventually(TURN, || {
        s.ai.chats().iter().any(|c| c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.starts_with(&format!("[{task}] ")) && t.contains("[file: drawing.txt (text/plain, ") && t.contains("\na drawing for ")))
    });
    s.ok("and the report's turn reads goose's text file whole", read_back, "");
    let bare = "t_00112233445566cc";
    let r = say("m7", json!({ "text": "", "thread": bare, "attachments": [file] }))?;
    let titled = r.status == 200 && s.eventually(TURN, || logged(api, &owner, &mind, "thread").iter().any(|t| t["body"]["id"] == bare && t["body"]["title"] == "notes.txt"));
    s.ok("a message may be a file alone: its thread is titled by the file's name", titled, &r);
    std::thread::sleep(super::computers::QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{computer}/sleep"), Some(&json!({})))?;
    Ok(())
}
