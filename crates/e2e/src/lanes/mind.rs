//! The mind (docs/optchat.md; `templates/mind`, a blessed template) on the
//! fakes: the scripted model (a `[[call NAME {json}]]` in a turn's message
//! is that tool call), Clef's fake, and the stub image's scripted agent on a
//! computer under `wrangler dev` with Docker as its hands.
//!
//! A person makes their mind (members only) and says something on `say`:
//! its trigger's turn logs the message and a reply as `msg` records on
//! `log`, the reply streamed first as a draft. Every call (UniiChat's
//! design, docs/optchat.md) is the same tools and system prompt, then the
//! view, then the turn's state (the time, the chat, the persona, the hands)
//! and its messages. A message asking for a zoom makes the turn call the
//! tool, logged as `tool` and `echo`. The compactor's pumps build every
//! message's node with the turns' system prompt and tools (none to be
//! called) and UniiChat's task, so the memory's parts are all summaries
//! and the view settles over the whole log; the view is saved, and an app
//! restarted loads it, not rebuilds it; a message past 30 000 characters is
//! several in a row; search finds a message (the page's and MCP's, never a
//! turn's tool); a topic added classifies the threads about it, and only
//! them. With its agent (the stub) an editor of the mind, its computer
//! awake, a turn of a persona with hands hands a task to it on `chat`,
//! recorded under the turn the agent's bridge gives that record; the
//! agent's one reply is the task's report, and comes back as a `work`
//! message `[<task>] …` that runs a turn of its own. A turn's zoom("<task>")
//! gives the whole run: what it was given, the stub's step and end, read
//! from `work` by the task's turn (`job.records`), and its report; the zoom
//! query gives it without the run. Its steps on `work` start nothing (a
//! page follows them).
//!
//! The web: web_fetch reads a page of a local upstream (a redirect
//! followed) as text, its chrome and scripts left out. A search reaches the
//! internet, which a local run does not call: a skip. Files: a message's
//! text file (the mind's blob, named on `say`) is read whole into its turn,
//! goes with the hand-off on `chat`, the stub names it, and its reply's
//! file comes back on the report and is read into the next turn. `export`
//! pages the raw log. The person's apps (`apps`): their todo is listed and
//! used as them, once though its step is tried twice; a shared mind lends
//! none; a fork asking for the capability is refused at deploy. An import
//! is compacted by up to eight pumps at once; while one is not summarized,
//! live turns pass it (one marker line in their view) and wait for nothing
//! of it.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use fragment_core::blob::sha256_hex;
use fragment_fakes::http::{Handler, Response, Server};
use serde_json::{json, Value};

use super::computers::{agent_replies, phase, told, turn_of, AGENT_JSON};
use super::jobs::records;
use anyhow::Context;

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

/// A page for web_fetch, behind a redirect: words, a link to make absolute,
/// and a script, a nav and a footer to leave out.
const COMPOST_HTML: &str = "<!doctype html><html><head><title>Compost, &amp; how</title><script>var words = \"a script's words\";</script></head>\
<body><nav><a href=\"/\">Home</a> menu words</nav><main><h1>Compost</h1><p>Turn the heap every <b>two weeks</b> &mdash; keep it damp.</p>\
<ul><li>Browns: leaves</li><li>Greens: scraps</li></ul><p>See <a href=\"/guide\">the guide</a>.</p></main><footer>footer words</footer></body></html>";

fn page_upstream() -> Result<Server> {
    let handler: Handler = Arc::new(|req| match req.path.as_str() {
        "/old" => Response::bytes(301, "text/plain", b"moved".to_vec()).with_header("location", "/compost.html"),
        "/compost.html" => Response::bytes(200, "text/html; charset=utf-8", COMPOST_HTML.as_bytes().to_vec()),
        _ => Response::bytes(404, "text/plain", b"no such page".to_vec()),
    });
    Ok(Server::start(0, handler)?)
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
    s.ok(
        "it starts with four personas, Mind the default, Builder's hands on, and Researcher",
        ids == ["mind", "builder", "coach", "researcher"] && personas["default"] == "mind" && personas["personas"][1]["hands"] == true,
        &personas,
    );

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
    let first = calls.iter().find(|c| c["messages"][1]["content"][2]["text"] == "hello mind, the garden has tomatoes and basil").cloned();
    s.ok(
        "the turn's call is the system prompt, then the view before the message, the turn's state and the message whole, with zoom, date, the web's tools, the apps' and computer (no search)",
        first.as_ref().is_some_and(|c| {
            c["messages"][0]["content"].as_str().is_some_and(|p| p.starts_with("You are Mind, an AI agent that works for one user in a single chat that never\nends.") && !p.contains("Be plain, warm and brief"))
                && c["messages"][1]["content"][0]["text"] == "<chat>\n</chat>"
                && c["tools"].as_array().is_some_and(|t| {
                    t.iter().filter_map(|t| t["function"]["name"].as_str()).collect::<Vec<_>>() == ["zoom", "date", "web_search", "web_fetch", "research", "apps", "app_ops", "app_call", "computer"]
                })
        }),
        format!("{first:?}"),
    );
    let state = first.as_ref().and_then(|c| c["messages"][1]["content"][1]["text"].as_str().map(str::to_string)).unwrap_or_default();
    s.ok(
        "its state, after the view: the time, the chat (its id, title, start, none before), the persona and its instructions, and no hands yet",
        state.starts_with("Now: ")
            && state.contains(&format!("\nChat: {garden} \"hello mind, the garden has tomatoes and basil\", begun "))
            && state.contains("; it begins here.\nYou are Mind 🌿 in this chat. Be plain, warm and brief.")
            && state.ends_with("\nYour hands: none."),
        &state,
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
    let task = format!("Compaction: compress message 2 into one line of at most 512 bytes\n(about 70 words), the length of this ruler:\n{}\n<input>\nuser: please open the first message", "-".repeat(512));
    let compaction = s.ai.chats().into_iter().find(|c| c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.starts_with(&task)));
    s.ok(
        "the long message's node is a compaction: the turns' system prompt and tools, none to be called, the compaction view before it, then UniiChat's task",
        compaction.as_ref().is_some_and(|c| {
            first.as_ref().is_some_and(|f| c["messages"][0] == f["messages"][0] && c["tools"] == f["tools"])
                && c["tool_choice"] == "none"
                && c["messages"][1]["content"][0]["text"].as_str().is_some_and(|v| v.starts_with("<chat>\n0+1|user: hello mind, the garden has tomatoes and basil\n1+1|talk: ") && v.ends_with("</chat>"))
                && c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.ends_with("\n</input>"))
        }),
        format!("{compaction:?}"),
    );
    // an app restarted (as an eviction ends it) loads the saved view
    let before = op(api, &owner, &mind, "status", json!({}));
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": mind, "op": "abort-app" })))?;
    let after = op(api, &owner, &mind, "status", json!({}));
    let again = op(api, &owner, &mind, "view", json!({}));
    s.ok(
        "an app restarted loads its saved view, never rebuilt from the log: a new instance, no fold, the same view",
        r.status == 200 && before["instance"].is_string() && after["instance"].is_string() && after["instance"] != before["instance"] && after["folds"] == 0 && again["text"] == view["text"] && after["view"] == before["view"],
        json!({ "before": before, "after": after, "lever": r.body }),
    );
    let z = op(api, &owner, &mind, "zoom", json!({ "id": 0, "n": 1 }));
    s.ok("zoom answers a message whole", z["text"] == "0+0|user: hello mind, the garden has tomatoes and basil", &z);

    // ---- search (the page's and an MCP client's; a turn has none)
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

    // ---- a long text: several messages in a row, never cut
    let long_thread = "t_0d0d0d0d0d0d0d0d";
    let long = format!("{}\n{}", "the long one ".repeat(2600), "and its tail ".repeat(1000));
    let r = say("m8", json!({ "text": long, "thread": long_thread }))?;
    anyhow::ensure!(r.status == 200, "saying m8: {r}");
    let answered = s.eventually(TURN, || messages(api, &owner, &mind, long_thread).iter().any(|m| m["kind"] == "talk"));
    let said = messages(api, &owner, &mind, long_thread);
    let pieces: Vec<&Value> = said.iter().filter(|m| m["kind"] == "user").collect();
    let read = op(api, &owner, &mind, "thread", json!({ "id": long_thread }));
    let whole: String = read["messages"].as_array().into_iter().flatten().filter(|m| m["kind"] == "user").filter_map(|m| m["text"].as_str()).collect();
    s.ok(
        "a message past 30 000 characters is logged as two in a row, the second marked as going on from the first, and whole together",
        answered
            && pieces.len() == 2
            && pieces[1]["i"].as_i64() == pieces[0]["i"].as_i64().map(|i| i + 1)
            && pieces[0].get("cont").is_none()
            && pieces[1]["cont"] == true
            && read["messages"][1]["cont"] == true
            && whole == long,
        json!({ "records": said, "thread": read }),
    );
    let call = s.ai.chats().into_iter().find(|c| c["messages"][1]["content"][2]["text"] == long.as_str());
    s.ok("and its turn reads it whole", call.is_some(), "");

    // ---- a message said while a turn works reaches it between its tool calls
    let busy = "t_0e0e0e0e0e0e0e0e";
    let quiet = s.eventually(TURN, || op(api, &owner, &mind, "status", json!({}))["pumps"] == 0);
    s.ai.delay_next(&[3000]);
    let r = say("m9", json!({ "text": "what day was it [[call date {\"id\": 0}]]", "thread": busy }))?;
    anyhow::ensure!(r.status == 200, "saying m9: {r}");
    std::thread::sleep(Duration::from_millis(1000));
    let r = say("m10", json!({ "text": "and also, the basil is flowering", "thread": busy }))?;
    anyhow::ensure!(r.status == 200, "saying m10: {r}");
    let heard = s.eventually(TURN, || {
        s.ai.chats().iter().any(|c| {
            let m = c["messages"].as_array().cloned().unwrap_or_default();
            m.len() >= 4 && m[m.len() - 2]["role"] == "tool" && m[m.len() - 1]["role"] == "user" && m[m.len() - 1]["content"] == "and also, the basil is flowering"
        })
    });
    let said = messages(api, &owner, &mind, busy);
    let kinds: Vec<&str> = said.iter().filter_map(|m| m["kind"].as_str()).collect();
    s.ok(
        "a message said while a turn works reaches it between its tool calls, after the tool's result, and starts no turn of its own",
        quiet && heard && kinds == ["user", "user", "tool", "echo", "talk"] && op(api, &owner, &mind, "status", json!({}))["queued"] == 0,
        json!({ "messages": said }),
    );

    // ---- the web: a page read as text, behind a redirect
    let web = page_upstream()?;
    let compost = "t_0c0c0c0c0c0c0c0c";
    let r = say("m5", json!({ "text": padded(&format!("read the page [[call web_fetch {{\"url\": \"{}/old\"}}]]", web.url)), "thread": compost }))?;
    anyhow::ensure!(r.status == 200, "saying m5: {r}");
    let read = s.eventually(TURN, || messages(api, &owner, &mind, compost).iter().any(|m| m["kind"] == "talk" && m["text"].as_str().is_some_and(|t| t.starts_with("the tool said: "))));
    let said = messages(api, &owner, &mind, compost);
    let echo = said.iter().find(|m| m["kind"] == "echo").and_then(|m| m["text"].as_str()).unwrap_or("");
    s.ok(
        "web_fetch follows the redirect and reads the page as text: its title, its address, its words, a link made absolute",
        read && said.iter().any(|m| m["kind"] == "tool" && m["text"] == format!("web_fetch {{\"url\":\"{}/old\"}}", web.url).as_str())
            && echo.starts_with(&format!("# Compost, & how\n{}/compost.html\n\n# Compost\n\nTurn the heap every two weeks — keep it damp.\n\n- Browns: leaves\n- Greens: scraps", web.url))
            && echo.contains(&format!("[the guide]({}/guide)", web.url)),
        echo,
    );
    s.ok("its scripts and its chrome (nav, footer) are left out", !echo.is_empty() && ["a script's words", "menu words", "footer words"].iter().all(|w| !echo.contains(w)), echo);
    s.skip(
        "web_search and research find pages on the internet (a keyed search, else DuckDuckGo, else Wikipedia)",
        "a local run calls nothing on the internet: the no-key search was tried by hand (docs/optchat.md, \"The web\")",
    );

    // ---- the person's apps: a todo of theirs, used as them
    if let Err(e) = apps(s, api, &owner, &mind, &npub) {
        s.fail("the mind uses the person's apps", format!("{e:#}"));
    }

    // ---- importing chats, into a mind of a person the CLI signs in as
    if let Err(e) = imports(s, api) {
        s.fail("an import into a mind", format!("{e:#}"));
    }

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
    // its bridge subscribes before it catches up and opens its live socket
    // (images/bridge driver.rs `follow_once`): a turn sees it here once the
    // socket is open, which the `here` lever says
    let here = following
        && s.eventually(WAKE, || {
            api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": mind, "op": "here", "principal": identity }))).ok().is_some_and(|r| r.body["here"] == true)
        });
    s.ok("its computer wakes and follows the mind's chat, its live socket open there", here, phase(api, &owner, &computer));

    let shed = "t_00112233445566aa";
    // (the stub takes a step for a task that names a tool)
    let r = say("m4", json!({ "text": padded("please hand it over [[call computer {\"task\": \"tidy the shed, tool by tool\"}]]"), "thread": shed, "persona": "builder" }))?;
    anyhow::ensure!(r.status == 200, "saying m4: {r}");
    let opened = s.eventually(TURN, || op(api, &owner, &mind, "tasks", json!({ "thread": shed }))["tasks"].as_array().is_some_and(|t| t.len() == 1));
    let tasks = op(api, &owner, &mind, "tasks", json!({ "thread": shed }));
    let task = tasks["tasks"][0]["id"].as_str().unwrap_or("").to_string();
    let chat = records(api, &owner, &mind, "chat");
    let handed = chat.iter().find(|r| r["principal"] == npub.as_str());
    s.ok(
        "a persona with hands hands the task to the agent on chat, as the mind, and records it",
        opened && handed.is_some_and(|h| h["body"]["to"] == json!([identity]) && h["body"]["text"] == format!("tidy the shed, tool by tool\n\n(task {task}, thread {shed})").as_str()),
        json!({ "tasks": tasks, "chat": chat }),
    );
    let builder = s.ai.chats().into_iter().find(|c| c["messages"][1]["content"][2]["text"].as_str().is_some_and(|t| t.starts_with("please hand it over")));
    s.ok(
        "a turn of another persona keeps the cached prefix: the same system prompt and tools; its state names Builder and its hands, the cloud computer awake",
        builder.as_ref().is_some_and(|b| {
            first.as_ref().is_some_and(|f| b["messages"][0] == f["messages"][0] && b["tools"] == f["tools"])
                && b["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.contains("\nYou are Builder 🛠️ in this chat. You get things done") && t.ends_with("\nYour hands: cloud (cloud computer, awake)."))
        }),
        format!("{builder:?}"),
    );
    let reported = s.eventually(WAKE, || {
        messages(api, &owner, &mind, shed).iter().any(|m| m["kind"] == "work" && m["task"] == task.as_str() && m["text"].as_str().is_some_and(|t| t.starts_with(&format!("[{task}] "))))
    });
    let said = messages(api, &owner, &mind, shed);
    let report = said.iter().find(|m| m["kind"] == "work" && m["task"] == task.as_str()).cloned().unwrap_or(Value::Null);
    // the agent's one reply on chat, under the turn its bridge gave the task's record
    let seq = handed.and_then(|h| h["seq"].as_i64()).unwrap_or(-1);
    let turn = turn_of(&agent_name, &mind, "chat", seq);
    let replies: Vec<Value> = agent_replies(&records(api, &owner, &mind, "chat"), &identity).into_iter().filter(|r| r["body"]["turn"] == turn.as_str()).collect();
    s.ok(
        "the agent replies once, and that reply comes back as the [task] work message in the thread",
        reported
            && replies.len() == 1
            && replies[0]["body"]["text"].as_str().is_some_and(|t| t.contains("tidy the shed") && report["text"] == format!("[{task}] {t}").as_str()),
        json!({ "said": said, "replies": replies }),
    );
    let answered = s.eventually(TURN, || messages(api, &owner, &mind, shed).iter().any(|m| m["kind"] == "talk" && m["i"].as_i64() > report["i"].as_i64()));
    s.ok("which runs a turn of its own", answered, json!(messages(api, &owner, &mind, shed)));
    // the report is the reply, trimmed
    let reply = replies.first().and_then(|r| r["body"]["text"].as_str()).unwrap_or("(no reply)").trim().to_string();
    let zoomed = op(api, &owner, &mind, "zoom", json!({ "id": task }));
    s.ok(
        "the zoom query (the page's, an MCP client's) gives the task: what it was given, and its report; goose's run is a turn's zoom's",
        zoomed["text"].as_str().is_some_and(|t| {
            t.starts_with(&format!("Task {task} (done) on cloud (the cloud computer), from "))
                && t.contains("\n\nGiven:\ntidy the shed, tool by tool\n\nIts run on the computer: read by Mind's own zoom in a turn, not here.\n\nIts report (message ")
                && t.ends_with(&format!("):\n{reply}"))
        }),
        &zoomed,
    );
    // a turn's zoom("<task>"): the whole run, goose's steps read from work by the task's turn
    let ended = s.eventually(WAKE, || records(api, &owner, &mind, "work").iter().any(|r| r["body"]["kind"] == "turn.end" && r["body"]["turn"] == turn.as_str()));
    let r = say("m11", json!({ "text": padded(&format!("what did the computer do [[call zoom {{\"id\": \"{task}\"}}]]")), "thread": shed }))?;
    anyhow::ensure!(r.status == 200, "saying m11: {r}");
    let whole = |m: &Value| m["kind"] == "echo" && m["text"].as_str().is_some_and(|t| t.starts_with(&format!("Task {task} (done)")));
    let echoed = s.eventually(TURN, || messages(api, &owner, &mind, shed).iter().any(whole));
    let echo = messages(api, &owner, &mind, shed).into_iter().find(whole).unwrap_or(Value::Null);
    s.ok(
        "a turn's zoom(\"<task>\") gives its whole run: what it was given, goose's words and steps (tool, args, ok, excerpt) and its end, read from work, then its report",
        ended
            && echoed
            && echo["text"].as_str().is_some_and(|t| {
                t.contains("\n\nGiven:\ntidy the shed, tool by tool\n\nIts run on the computer:\nLet me look.\n[step 1] search {\"q\":\"tidy the shed, tool by tool")
                    && t.contains("→ ok: 3 results\n[ended] idle\n\nIts report (message ")
                    && t.ends_with(&format!("):\n{reply}"))
            }),
        &echo,
    );
    // (its turn ends before the next hand-off's)
    s.eventually(TURN, || messages(api, &owner, &mind, shed).iter().any(|m| m["kind"] == "talk" && m["i"].as_i64() > echo["i"].as_i64()));
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
    let call = s.ai.chats().into_iter().find(|c| c["messages"][1]["content"][2]["text"].as_str().is_some_and(|t| t.starts_with("draw the hooks")));
    s.ok(
        "its turn reads the text file whole, below its name (job.blob)",
        call.as_ref().is_some_and(|c| c["messages"][1]["content"][2]["text"].as_str().is_some_and(|t| t.ends_with(&shown))),
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
    let reported = s.eventually(WAKE, || messages(api, &owner, &mind, hooks).iter().any(|m| m["kind"] == "work" && m["task"] == task.as_str()));
    let report = messages(api, &owner, &mind, hooks).into_iter().find(|m| m["kind"] == "work" && m["task"] == task.as_str()).unwrap_or(Value::Null);
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
        s.ai.chats().iter().any(|c| c["messages"][1]["content"][2]["text"].as_str().is_some_and(|t| t.starts_with(&format!("[{task}] ")) && t.contains("[file: drawing.txt (text/plain, ") && t.contains("\na drawing for ")))
    });
    s.ok("and the report's turn reads goose's text file whole", read_back, "");
    let bare = "t_00112233445566cc";
    let r = say("m7", json!({ "text": "", "thread": bare, "attachments": [file] }))?;
    let titled = r.status == 200 && s.eventually(TURN, || logged(api, &owner, &mind, "thread").iter().any(|t| t["body"]["id"] == bare && t["body"]["title"] == "notes.txt"));
    s.ok("a message may be a file alone: its thread is titled by the file's name", titled, &r);

    // ---- export: the raw log, a page at a time
    let t0 = op(api, &owner, &mind, "status", json!({}))["T"].as_i64().unwrap_or(0);
    let mut entries: Vec<Value> = vec![];
    let mut after = Value::Null;
    let mut pages = 0;
    while pages < 200 {
        let input = if after.is_null() { json!({ "limit": 7 }) } else { json!({ "after": after, "limit": 7 }) };
        let page = op(api, &owner, &mind, "export", input);
        pages += 1;
        entries.extend(page["entries"].as_array().cloned().unwrap_or_default());
        after = page["next"].clone();
        if after.is_null() {
            break;
        }
    }
    let ids: Vec<i64> = entries.iter().filter_map(|e| e["i"].as_i64()).collect();
    s.ok(
        "export pages the whole log in order, seven a page, to its end",
        t0 > 7 && ids.len() as i64 >= t0 && ids.iter().enumerate().all(|(k, i)| *i == k as i64) && pages as i64 == (ids.len() as i64 + 6) / 7,
        json!({ "T": t0, "pages": pages, "ids": ids }),
    );
    let with_notes = entries.iter().find(|e| e["thread"] == hooks && e["kind"] == "user" && e["task"].is_null());
    s.ok(
        "each entry carries its files and the text the mind read of them",
        with_notes.is_some_and(|e| e["attachments"][0]["name"] == "notes.txt" && e["attachments"][0]["text"] == "the spare key hangs on the third hook\n"),
        format!("{with_notes:?}"),
    );
    // ---- a machine of the owner's paired as hands, beside the cloud computer
    if let Err(e) = machine_hands(s, api, &owner, &mind, &identity) {
        s.fail("a machine paired as hands", format!("{e:#}"));
    }

    std::thread::sleep(super::computers::QUEUE_DRAIN);
    api.signed(&owner, "POST", &format!("/api/computers/{computer}/sleep"), Some(&json!({})))?;
    // a mind on the person's own models (Claude, the vendors' fake)
    super::own_models::mind(s, api)
}

/// `fragment hands run`, stopped as Ctrl-C stops it (SIGTERM: it stops its
/// bridge's process group, then itself) when dropped, its output in a log
/// of the run's scratch.
struct HandsRun {
    child: Option<std::process::Child>,
}

impl HandsRun {
    fn stop(&mut self) -> Option<std::process::ExitStatus> {
        let mut child = self.child.take()?;
        let _ = std::process::Command::new("kill").args(["-TERM", &child.id().to_string()]).status();
        // bounded: its own stop takes at most its bridge's grace and a half second
        for _ in 0..100 {
            if let Ok(Some(status)) = child.try_wait() {
                return Some(status);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        child.wait().ok()
    }
}

impl Drop for HandsRun {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A machine of the owner's paired as hands, beside their cloud computer
/// (docs/optchat.md, "A machine as hands"), the e2e playing the machine:
/// `fragment hands pair` (as the owner) makes the agent `hands-e2e-box`,
/// pairs a key kept in the CLI's config, and adds it to the mind; a
/// request that key signs acts as the agent, and no one but the owner
/// pairs to it. `fragment hands run` runs the bridge's scripted agent here
/// under its loopback proxy: the machine is online (its socket here), a
/// turn lists both hands, a task `on` it reaches it alone and its one
/// reply comes back as the report, the task saying where it ran, and the
/// persona's next task goes there unasked. Stopped, the machine is offline,
/// and a task on it is refused at once. Unpaired, it leaves the mind and
/// its key is 401 from its next request.
fn machine_hands(s: &mut Suite, api: &Api, owner: &Keys, mind: &str, cloud: &str) -> Result<()> {
    let Some(bridge) = crate::bridge_binary() else {
        s.skip("a machine paired as hands", "no fragment-bridge here: `cargo xtask e2e` builds it (images/target/release), or name one in FRAGMENT_BRIDGE_BIN");
        return Ok(());
    };
    let home = s.dir("hands-home");
    // the CLI acts as the mind's owner: their key in its config (each system's place)
    for dir in [".config/fragment", "Library/Application Support/fragment"] {
        std::fs::create_dir_all(home.join(dir))?;
        std::fs::write(home.join(dir).join("config.json"), json!({ "host": api.base, "secret_key": owner.secret_hex() }).to_string())?;
    }
    let paired = s.cli_json(api, &home, &["hands", "pair", "--name", "e2e-box", "--mind", mind, "--json"])?;
    let agent = paired["agent"].as_str().unwrap_or("").to_string();
    let machine = paired["identity"].as_str().unwrap_or("").to_string();
    s.ok(
        "fragment hands pair makes the agent hands-e2e-box, pairs this machine's key to it, and adds it to the mind",
        paired["paired"] == true && agent.starts_with("hands-e2e-box.") && machine.starts_with("id:") && paired["mind"] == mind,
        &paired,
    );
    let keys = api.signed(owner, "GET", &format!("/api/f/{agent}/keys"), None)?;
    s.ok(
        "its machines' keys list it, named for the machine",
        keys.status == 200 && keys.body["agent"] == machine.as_str() && keys.body["keys"].as_array().is_some_and(|k| k.len() == 1 && k[0]["name"] == "e2e-box" && k[0]["npub"] == paired["npub"] && k[0]["revokedAt"].is_null()),
        &keys,
    );
    // the key, kept only in the CLI's config, readable by its owner alone
    let file = [".config/fragment/hands.json", "Library/Application Support/fragment/hands.json"].iter().map(|p| home.join(p)).find(|p| p.is_file()).context("fragment hands pair keeps hands.json")?;
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file)?)?;
    let machine_keys = Keys::from_secret_hex(kept["secretKey"].as_str().unwrap_or("")).context("hands.json holds the machine's key")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file)?.permissions().mode() & 0o777;
        s.ok("the machine's key is in a file of its owner's alone (0600)", mode == 0o600, format!("{mode:o}"));
    }
    let me = api.signed(&machine_keys, "GET", "/api/identities/me", None)?;
    s.ok("a request the machine's key signs acts as the agent, its owner's", me.status == 200 && me.body["id"] == machine.as_str() && me.body["kind"] == "agent", &me);
    let again = s.cli_json(api, &home, &["hands", "pair", "--name", "e2e-box", "--mind", mind, "--json"])?;
    s.ok("pairing again here is the same pairing", again["paired"] == false && again["npub"] == paired["npub"], &again);
    let stranger = api.person()?;
    let key = Keys::generate();
    let route = format!("/api/f/{agent}/keys");
    let proof = key.proof("POST", &format!("{}{route}", api.base), stranger.pubkey_hex(), crate::api::now_s());
    let r = api.signed(&stranger, "POST", &route, Some(&json!({ "proof": proof, "name": "theirs" })))?;
    s.ok("no one but its owner pairs a machine to it", r.status == 403, &r);

    // ---- it runs: the bridge's scripted agent, here, under the proxy
    let dir = s.dir("hands-run");
    let log = std::fs::File::create(s.scratch.join("hands-run.log"))?;
    let mut cmd = s.bare_cli();
    cmd.args(["hands", "run", "--runtime", "script", "--bridge"]).arg(&bridge).arg("--dir").arg(&dir).env("HOME", &home).stdout(log.try_clone()?).stderr(log);
    let mut run = HandsRun { child: Some(cmd.spawn().context("fragment hands run")?) };
    let here = |want: bool| {
        api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": mind, "op": "here", "principal": machine }))).ok().is_some_and(|r| r.body["here"] == want)
    };
    let online = s.eventually(WAKE, || here(true));
    s.ok("fragment hands run brings it online: its bridge follows the mind's chat, its socket open there", online, s.scratch.join("hands-run.log").display());
    let say = |id: &str, body: Value| api.signed(owner, "POST", &format!("/api/f/{mind}/channels/say"), Some(&json!({ "id": id, "body": body })));
    let shop = "t_00112233445566dd";
    let r = say("m20", json!({ "text": padded("hand it to the box [[call computer {\"task\": \"sort the screws, tool by tool\", \"on\": \"e2e-box\"}]]"), "thread": shop, "persona": "builder" }))?;
    anyhow::ensure!(r.status == 200, "saying m20: {r}");
    let opened = s.eventually(TURN, || op(api, owner, mind, "tasks", json!({ "thread": shop }))["tasks"].as_array().is_some_and(|t| t.len() == 1));
    let tasks = op(api, owner, mind, "tasks", json!({ "thread": shop }));
    let task = tasks["tasks"][0]["id"].as_str().unwrap_or("").to_string();
    let handed = records(api, owner, mind, "chat").into_iter().find(|r| r["body"]["text"].as_str().is_some_and(|t| t.ends_with(&format!("(task {task}, thread {shop})"))));
    s.ok(
        "a task on the machine is for its agent alone (to), and records where it went",
        opened && handed.as_ref().is_some_and(|h| h["body"]["to"] == json!([machine])) && tasks["tasks"][0]["hands"] == json!({ "agent": machine, "name": "e2e-box", "kind": "machine" }),
        json!({ "tasks": tasks, "handed": handed }),
    );
    let call = s.ai.chats().into_iter().find(|c| c["messages"][1]["content"][2]["text"].as_str().is_some_and(|t| t.starts_with("hand it to the box")));
    s.ok(
        "its turn lists both hands: the cloud computer, the default, and the paired machine, online",
        call.as_ref().is_some_and(|c| {
            c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.contains("\nYour hands: cloud (cloud computer, ") && t.ends_with("; the default), e2e-box (paired machine, online)."))
        }),
        format!("{call:?}"),
    );
    let reported = s.eventually(WAKE, || messages(api, owner, mind, shop).iter().any(|m| m["kind"] == "work" && m["task"] == task.as_str()));
    let seq = handed.as_ref().and_then(|h| h["seq"].as_i64()).unwrap_or(-1);
    let turn = turn_of(&agent, mind, "chat", seq);
    let chat = records(api, owner, mind, "chat");
    let replies: Vec<Value> = agent_replies(&chat, &machine).into_iter().filter(|r| r["body"]["turn"] == turn.as_str()).collect();
    let work = records(api, owner, mind, "work");
    let claimed: Vec<&Value> = work.iter().filter(|r| r["body"]["kind"] == "turn.start" && r["body"]["cause"]["seq"] == seq).collect();
    s.ok(
        "the machine claims it (and the cloud computer does not), replies once, and the reply comes back as the report",
        reported
            && replies.len() == 1
            && claimed.len() == 1
            && claimed[0]["principal"] == machine.as_str()
            && !chat.iter().any(|r| r["principal"] == cloud && r["body"]["turn"] == turn.as_str())
            && messages(api, owner, mind, shop).iter().any(|m| m["kind"] == "work" && m["task"] == task.as_str() && m["text"].as_str().is_some_and(|t| t.contains("sort the screws"))),
        json!({ "replies": replies, "claimed": claimed }),
    );
    let zoomed = op(api, owner, mind, "zoom", json!({ "id": task }));
    s.ok("the task says where it ran", zoomed["text"].as_str().is_some_and(|t| t.starts_with(&format!("Task {task} (done) on e2e-box (a paired machine), from "))), &zoomed);
    // (its report's turn ends before the next)
    s.eventually(TURN, || messages(api, owner, mind, shop).iter().filter(|m| m["kind"] == "talk").count() >= 2);
    // (the scripted agent's `think`: one model call through the machine's proxy)
    let r = say("m21", json!({ "text": padded("and again [[call computer {\"task\": \"think count the screws\"}]]"), "thread": shop, "persona": "builder" }))?;
    anyhow::ensure!(r.status == 200, "saying m21: {r}");
    let second = s.eventually(TURN, || op(api, owner, mind, "tasks", json!({ "thread": shop }))["tasks"].as_array().is_some_and(|t| t.len() == 2));
    let tasks = op(api, owner, mind, "tasks", json!({ "thread": shop }));
    s.ok("the persona's next task goes to the hands it chose last, unasked", second && tasks["tasks"][0]["hands"]["name"] == "e2e-box", &tasks);
    let thought = |m: &Value| m["kind"] == "work" && m["text"].as_str().is_some_and(|t| t.contains("thought: echo: count the screws"));
    let done = s.eventually(WAKE, || messages(api, owner, mind, shop).iter().any(thought));
    let modelled = s.ai.chats().iter().any(|c| c["messages"].as_array().and_then(|m| m.last()).and_then(|m| m["content"].as_str()).is_some_and(|t| t.starts_with("count the screws")));
    s.ok(
        "its model call goes through the proxy to the platform's model route, as the agent (its owner pays), and its report comes back",
        done && modelled,
        json!(messages(api, owner, mind, shop)),
    );
    s.eventually(TURN, || messages(api, owner, mind, shop).iter().filter(|m| m["kind"] == "talk").count() >= 4);

    // ---- stopped: offline, and a task on it is refused at once
    let stopped = run.stop();
    s.ok("fragment hands run stops cleanly on SIGTERM", stopped.is_some_and(|st| st.success()), format!("{stopped:?}"));
    // goose itself on the machine, when this run is given one
    match std::env::var_os("FRAGMENT_GOOSE_BIN").map(std::path::PathBuf::from).filter(|g| g.is_file()) {
        None => s.skip("goose itself on a paired machine", "this run names no goose (FRAGMENT_GOOSE_BIN: a build of the fork, images/goose/Dockerfile)"),
        Some(goose) => {
            if s.eventually(WAKE, || here(false)) {
                goose_on_machine(s, api, owner, mind, &machine, &home, &bridge, &goose)?;
            } else {
                s.fail("goose itself on a paired machine", "the scripted run's socket outlived it");
            }
        }
    }
    let offline = s.eventually(WAKE, || here(false));
    s.ok("stopped, the machine is offline", offline, "");
    let r = say("m22", json!({ "text": padded("once more [[call computer {\"task\": \"oil the hinges\", \"on\": \"e2e-box\"}]]"), "thread": shop, "persona": "builder" }))?;
    anyhow::ensure!(r.status == 200, "saying m22: {r}");
    let refused = |m: &Value| m["kind"] == "echo" && m["text"].as_str().is_some_and(|t| t.contains("e2e-box is offline"));
    let said = s.eventually(TURN, || messages(api, owner, mind, shop).iter().any(refused));
    s.ok(
        "a task on an offline machine is refused at once, and none is opened",
        said && op(api, owner, mind, "tasks", json!({ "thread": shop }))["tasks"].as_array().is_some_and(|t| t.len() == 2),
        json!(messages(api, owner, mind, shop)),
    );

    // ---- unpaired: out of the mind, its key refused
    let out = s.cli_json(api, &home, &["hands", "unpair", "--json"])?;
    let gone = api.signed(&machine_keys, "GET", "/api/identities/me", None)?;
    let members = api.signed(owner, "GET", &format!("/api/f/{mind}/members"), None)?;
    s.ok(
        "fragment hands unpair revokes its key (401 from its next request) and takes it out of the mind",
        out["unpaired"] == true && out["leftMind"] == true && gone.status == 401 && members.body["members"].as_array().is_some_and(|m| !m.iter().any(|x| x["principal"] == machine.as_str())),
        json!({ "unpaired": out, "me": gone.status, "members": members.body }),
    );
    let status = s.cli_json(api, &home, &["hands", "status", "--json"])?;
    s.ok("and forgets the pairing here", status["paired"] == false, &status);
    Ok(())
}

/// Processes whose working directory is under `dir` (Linux's /proc): what a
/// stopped `hands run` must not leave behind.
fn left_in(dir: &std::path::Path) -> Vec<String> {
    let Ok(procs) = std::fs::read_dir("/proc") else { return vec![] };
    let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    procs
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().bytes().all(|b| b.is_ascii_digit()))
        .filter(|e| std::fs::read_link(e.path().join("cwd")).is_ok_and(|cwd| cwd.starts_with(&dir)))
        .map(|e| format!("{} {}", e.file_name().to_string_lossy(), std::fs::read_to_string(e.path().join("comm")).unwrap_or_default().trim()))
        .collect()
}

/// goose itself on the paired machine (`FRAGMENT_GOOSE_BIN`): the real
/// runtime under `fragment hands run`, on the Workers AI fake through the
/// proxy's model route. A task on the machine runs a turn whose model call
/// is told it works on its owner's own machine, offered goose's shell and
/// the mind's tools (and the web's, with fragment-desktop built) and no
/// screen's or browser's; the fake asks for a shell command, goose runs it
/// in the machine's work folder, and the report quotes it. Stopped, nothing
/// of it is left running in that folder.
#[allow(clippy::too_many_arguments)]
fn goose_on_machine(s: &mut Suite, api: &Api, owner: &Keys, mind: &str, machine: &str, home: &std::path::Path, bridge: &std::path::Path, goose: &std::path::Path) -> Result<()> {
    let dir = s.dir("hands-goose");
    let log = std::fs::File::create(s.scratch.join("hands-goose.log"))?;
    let desktop = bridge.with_file_name("fragment-desktop");
    let mut cmd = s.bare_cli();
    cmd.args(["hands", "run", "--no-browser", "--goose"]).arg(goose).arg("--bridge").arg(bridge).arg("--dir").arg(&dir).env("HOME", home).stdout(log.try_clone()?).stderr(log);
    if desktop.is_file() {
        cmd.arg("--desktop").arg(&desktop);
    }
    let mut run = HandsRun { child: Some(cmd.spawn().context("fragment hands run, goose")?) };
    let online = s.eventually(WAKE, || {
        api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": mind, "op": "here", "principal": machine }))).ok().is_some_and(|r| r.body["here"] == true)
    });
    s.ok("goose's run brings the machine online", online, s.scratch.join("hands-goose.log").display());
    let thread = "t_00112233445566ee";
    let say = |id: &str, body: Value| api.signed(owner, "POST", &format!("/api/f/{mind}/channels/say"), Some(&json!({ "id": id, "body": body })));
    let text = "goose, prove it [[call computer {\"task\": \"make the proof [[call shell {\\\"command\\\": \\\"echo machine-hands > proof.txt && cat proof.txt\\\"}]]\", \"on\": \"e2e-box\"}]]";
    let r = say("m30", json!({ "text": padded(text), "thread": thread, "persona": "builder" }))?;
    anyhow::ensure!(r.status == 200, "saying m30: {r}");
    let reported = |m: &Value| m["kind"] == "work" && m["text"].as_str().is_some_and(|t| t.contains("machine-hands"));
    let done = s.eventually(Duration::from_secs(240), || messages(api, owner, mind, thread).iter().any(reported));
    let proof = std::fs::read_to_string(dir.join("work/proof.txt")).unwrap_or_default();
    let steps: Vec<Value> = records(api, owner, mind, "work").into_iter().filter(|r| r["principal"] == machine && r["body"]["kind"] == "turn.step").collect();
    s.ok(
        "goose takes the task on the machine, runs its shell in the machine's work folder, and the report quotes it",
        done && proof == "machine-hands\n" && steps.iter().any(|r| r["body"]["tool"] == "shell" && r["body"]["ok"] == true),
        json!({ "proof": proof, "steps": steps, "said": messages(api, owner, mind, thread) }),
    );
    let call = s.ai.chats().into_iter().find(|c| {
        c["messages"].as_array().is_some_and(|m| m.iter().any(|m| m["content"].to_string().contains("make the proof")))
            && c["tools"].as_array().is_some_and(|t| t.iter().any(|t| t["function"]["name"] == "shell"))
    });
    let tools: Vec<String> = call.as_ref().and_then(|c| c["tools"].as_array()).map(|t| t.iter().filter_map(|t| t["function"]["name"].as_str().map(str::to_string)).collect()).unwrap_or_default();
    let system = call.as_ref().map(|c| c["messages"][0]["content"].to_string()).unwrap_or_default();
    s.ok(
        "its model call is told it works on its owner's own machine, with goose's shell and the mind's tools, and no screen's or browser's",
        system.contains("your owner's own machine")
            && tools.iter().any(|t| t == "mind__view")
            && (!desktop.is_file() || tools.iter().any(|t| t == "web__web_search"))
            && !tools.iter().any(|t| t.starts_with("computer__") || t.starts_with("browser__")),
        json!({ "tools": tools }),
    );
    let stopped = run.stop();
    std::thread::sleep(Duration::from_millis(500));
    let left = left_in(&dir);
    s.ok("stopped, goose and everything it started stop with it", stopped.is_some_and(|st| st.success()) && left.is_empty(), json!({ "status": format!("{stopped:?}"), "left": left }));
    Ok(())
}

/// A fragment with its own code that asks for the `owner` capability.
const FORK_JSON: &[u8] = br#"{"capabilities":["owner"],"operations":{"look":{"kind":"job"}}}"#;
const FORK_APP: &[u8] = b"import { DurableObject } from \"cloudflare:workers\";\n\
export class App extends DurableObject {\n  async look(input, job) {\n    return await job.owner.fragments();\n  }\n}\n";

/// The person's apps (docs/optchat.md, "The user's apps"): their todo,
/// listed with its described operations (`apps`, `app_ops`) and used as
/// them (`app_call`, `job.owner.call`), once though its step is tried
/// twice (the todo's outbox write fails the first try: the call's id is
/// the step's, so the todo's ledger replays it). Shared with someone else,
/// the mind lends none; a fragment with its own code that asks for the
/// capability is refused at deploy.
fn apps(s: &mut Suite, api: &Api, owner: &Keys, mind: &str, npub: &str) -> Result<()> {
    let todo = s.named(api, owner, "mtodo")?;
    let r = api.create_with(owner, json!({ "name": todo, "template": "todo" }))?;
    anyhow::ensure!(r.status == 200, "making the todo: {r}");
    s.owned(&r.body, owner);
    let owner_id = api.identity(owner)?;
    // its code installed, and the person's list holds it (fed from the todo's outbox)
    let ready = s.eventually(Duration::from_secs(30), || {
        api.status(owner, &todo).is_ok_and(|r| r.body["code"]["operations"]["add"]["kind"] == "mutation")
            && api.signed(owner, "GET", "/api/fragments", None).is_ok_and(|r| r.body["fragments"].as_array().is_some_and(|l| l.iter().any(|f| f["name"] == todo.as_str())))
    });
    anyhow::ensure!(ready, "the todo is not installed and listed");
    let say = |id: &str, thread: &str, text: String| api.signed(owner, "POST", &format!("/api/f/{mind}/channels/say"), Some(&json!({ "id": id, "body": { "text": padded(&text), "thread": thread } })));
    // the turn's tool answered, and the model said so
    let answered = |thread: &str| messages(api, owner, mind, thread).iter().any(|m| m["kind"] == "talk" && m["text"].as_str().is_some_and(|t| t.starts_with("the tool said: ")));
    let echo = |thread: &str| messages(api, owner, mind, thread).iter().find(|m| m["kind"] == "echo").and_then(|m| m["text"].as_str().map(str::to_string)).unwrap_or_default();

    // apps: the todo, its described operations a line each, not the mind itself
    let listing = "t_a0a0a0a0a0a0a0a0";
    anyhow::ensure!(say("ap1", listing, "what apps do I have [[call apps {}]]".into())?.status == 200, "saying ap1");
    let listed = s.eventually(TURN, || answered(listing));
    let text = echo(listing);
    let flat = todo.replace('.', "--");
    s.ok(
        "apps lists the person's todo (as its owner, at its address) with its described operations, a line each, and not the mind",
        listed
            && text.lines().any(|l| l.starts_with(&format!("- {todo}:")) && l.contains("(app, owner) http") && l.contains(&format!("://{flat}")))
            && text.contains("\n  add (mutation): Add a todo to the list. Answers its id.")
            && text.contains("\n  list (query): The list:")
            && !text.contains(mind),
        &text,
    );

    // app_ops: an operation's input in full
    let inputs = "t_a1a1a1a1a1a1a1a1";
    anyhow::ensure!(say("ap2", inputs, format!("what does it take [[call app_ops {{\"fragment\": \"{todo}\"}}]]"))?.status == 200, "saying ap2");
    let read = s.eventually(TURN, || answered(inputs));
    let text = echo(inputs);
    s.ok(
        "app_ops shows each operation's input schema",
        read && text.contains("add (mutation): Add a todo to the list. Answers its id.\n  input: {") && text.contains("\"required\":[\"text\"]"),
        &text,
    );

    // app_call: add milk, as the person, its step tried twice
    let r = api.unsigned("POST", "/api/test/fragment", Some(&json!({ "fragment": todo, "op": "fail-outbox", "times": 1 })))?;
    s.ok("(the test fleet fails the todo's next outbox write: the call answers 500, and its step is tried again)", r.status == 200, &r);
    let shopping = "t_a2a2a2a2a2a2a2a2";
    let call = format!("add milk to my todo [[call app_call {{\"fragment\": \"{todo}\", \"op\": \"add\", \"input\": {{\"text\": \"milk\"}}}}]]");
    anyhow::ensure!(say("ap3", shopping, call)?.status == 200, "saying ap3");
    let added = s.eventually(TURN, || answered(shopping));
    let text = echo(shopping);
    let list = api.op(owner, &todo, "list", "mind-list", json!({}))?;
    let milk = list.body["result"]["todos"].as_array().map_or(0, |l| l.iter().filter(|t| t["text"] == "milk").count());
    s.ok(
        "app_call adds milk to the todo, once, its echo naming the todo and where it is, then its result",
        added && milk == 1 && text.starts_with(&format!("{todo} add at http")) && text.lines().nth(1) == Some("{\"id\":1}"),
        json!({ "echo": text, "list": list.body }),
    );
    let events = api.signed(owner, "GET", &format!("/api/f/{todo}/events?tail=50"), None)?;
    let events = events.body["events"].as_array().cloned().unwrap_or_default();
    let called: Vec<&Value> = events.iter().filter(|e| e["kind"] == "fragment.called").collect();
    s.ok(
        "its first try failed after the commit, and its retry replayed it: the todo's events name the mind, as the person",
        events.iter().any(|e| e["kind"] == "effects.delayed" && e["data"]["op"] == "add")
            && called.len() == 1
            && called[0]["data"]["replayed"] == true
            && called[0]["data"]["op"] == "add"
            && called[0]["data"]["fragment"] == mind
            && called[0]["data"]["principal"] == owner_id.as_str()
            && called[0]["data"]["key"] == npub,
        json!(events),
    );
    let ops = records(api, owner, &todo, "ops");
    let activity = records(api, owner, &todo, "activity");
    s.ok(
        "its ops record names the mind's key, and its record the person (call.principal)",
        ops.iter().filter(|r| r["principal"] == npub).count() == 1
            && activity.len() == 1
            && activity[0]["body"]["by"] == owner_id.as_str()
            && activity[0]["body"]["text"] == "milk",
        json!({ "ops": ops, "activity": activity }),
    );

    // shared with someone else, the mind lends none
    let other = api.person()?;
    let other_id = api.identity(&other)?;
    let r = api.signed(owner, "PUT", &format!("/api/f/{mind}/members/{other_id}"), Some(&json!({ "role": "viewer" })))?;
    anyhow::ensure!(r.status == 200, "sharing the mind: {r}");
    let shared = "t_a3a3a3a3a3a3a3a3";
    anyhow::ensure!(say("ap4", shared, "what apps do I have now [[call apps {}]]".into())?.status == 200, "saying ap4");
    let refused = s.eventually(TURN, || answered(shared));
    let text = echo(shared);
    s.ok(
        "a mind shared with someone else acts as its owner nowhere: apps says why",
        refused && text.starts_with("Error: ") && text.contains("no one else can read or drive it") && !text.contains(&todo),
        &text,
    );
    let r = api.signed(owner, "DELETE", &format!("/api/f/{mind}/members/{other_id}"), None)?;
    anyhow::ensure!(r.status == 200, "unsharing the mind: {r}");

    // a fragment with its own code may not ask for the capability
    let fork = s.named(api, owner, "mfork")?;
    let c = s.create(api, owner, &fork)?;
    s.commit(&c, &[("fragment.json", Some(FORK_JSON)), ("app.mjs", Some(FORK_APP))]);
    let refused = s.deploy(&c);
    let st = api.status(owner, &fork)?;
    s.ok(
        "a fragment with its own code that asks for the owner capability is refused at deploy, saying why",
        st.body["code"]["error"].as_str().is_some_and(|e| e.contains(&refused[..12]) && e.contains("capabilities (owner)") && e.contains("blessed template")),
        &st.body["code"],
    );
    Ok(())
}

/// When the imported session began: 2025-03-01T12:00:00Z.
const IMPORTED_T0: i64 = 1_740_830_400_000;

/// A Claude Code session as its file holds it (synthetic): a person's words,
/// a reply, a long message of theirs, a turn with a tool call before its
/// final reply, and a thanks. Three long messages: a compaction each.
fn claude_code_session() -> String {
    let s = "e2e-import-session";
    let at = |sec: i64| format!("2025-03-01T12:{:02}:{:02}.000Z", sec / 60, sec % 60);
    let user = |sec: i64, text: &str| json!({ "type": "user", "sessionId": s, "uuid": format!("u{sec}"), "timestamp": at(sec), "message": { "role": "user", "content": text } });
    let said = |sec: i64, id: &str, content: Value| json!({ "type": "assistant", "sessionId": s, "uuid": format!("a{sec}"), "timestamp": at(sec), "message": { "id": id, "role": "assistant", "model": "claude", "content": content } });
    let lines = [
        json!({ "type": "custom-title", "sessionId": s, "customTitle": "Seed swap" }),
        user(0, "we swap seeds with the neighbors in spring"),
        said(60, "m1", json!([{ "type": "text", "text": padded("Seed swap notes: bring labels and envelopes.") }])),
        user(120, &padded("the list: tomatoes, beans, squash, marigolds")),
        said(180, "m2", json!([{ "type": "text", "text": "Saving it." }, { "type": "tool_use", "id": "t1", "name": "Write", "input": {} }])),
        json!({ "type": "user", "sessionId": s, "uuid": "r1", "timestamp": at(200), "toolUseResult": {}, "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "ok" }] } }),
        said(240, "m3", json!([{ "type": "text", "text": padded("Saved the swap list: tomatoes, beans, squash, marigolds.") }])),
        user(300, "thanks"),
    ];
    lines.iter().map(|l| format!("{l}\n")).collect()
}

/// `fragment mind import` and the mind's `import`: a person signed in with
/// the CLI plays a Claude Code session into their mind. A dry run counts
/// it; the import lands its words and final replies with their own times
/// in a thread of its own, and starts the compactor, which summarizes each
/// long message in a compaction of its own; a rerun sends nothing; a part
/// ahead of what landed is refused, and one landed again changes nothing.
/// Then a longer import, its model calls held a moment each: up to eight
/// pumps compact it at once. Then one held until released, passed by live
/// turns meanwhile (`passing_an_import`).
fn imports(s: &mut Suite, api: &Api) -> Result<()> {
    let home = s.dir("mind-import");
    s.login(api, &home);
    let keys = s.cli_keys(&home).ok_or_else(|| anyhow::anyhow!("the CLI logged in"))?;
    let mind = s.named(api, &keys, "mind")?;
    let r = api.create_with(&keys, json!({ "name": mind, "template": "mind", "visibility": "members" }))?;
    anyhow::ensure!(r.status == 200, "making the CLI person's mind: {r}");
    s.owned(&r.body, &keys);
    s.eventually(Duration::from_secs(30), || api.status(&keys, &mind).is_ok_and(|r| r.body["code"]["operations"]["import"]["kind"] == "mutation"));
    let file = s.dir("mind-import-files").join("e2e-import-session.jsonl");
    std::fs::write(&file, claude_code_session())?;
    let path = file.to_string_lossy().to_string();

    let dry = s.cli_json(api, &home, &["mind", "import", &path, "--dry-run", "--json"])?;
    s.ok(
        "fragment mind import --dry-run counts a session's words and final replies, and the compactor's calls",
        dry["conversations"] == 1
            && dry["sources"][0]["source"] == "claude-code"
            && dry["sources"][0]["user"] == 3
            && dry["sources"][0]["assistant"] == 2
            && dry["estimate"]["level0_calls"] == 3
            && dry["estimate"]["logged"] == 5
            && dry["estimate"]["calls"].as_u64().is_some_and(|n| n >= 3),
        &dry,
    );
    let sent = s.cli_json(api, &home, &["mind", "import", &path, "--mind", &mind, "--no-wait", "--json"])?;
    s.ok("fragment mind import sends it", sent["conversations"] == 1 && sent["messages"] == 5, &sent);
    let again = s.cli_json(api, &home, &["mind", "import", &path, "--mind", &mind, "--no-wait", "--json"])?;
    s.ok("and a rerun sends nothing again", again["conversations"] == 0 && again["messages"] == 0, &again);

    let threads = op(api, &keys, &mind, "threads", json!({}));
    let thread = threads["threads"].as_array().and_then(|t| t.iter().find(|t| t["title"] == "Seed swap")).cloned().unwrap_or(Value::Null);
    let id = thread["id"].as_str().unwrap_or("").to_string();
    let read = op(api, &keys, &mind, "thread", json!({ "id": id }));
    let msgs = read["messages"].as_array().cloned().unwrap_or_default();
    let kinds: Vec<&str> = msgs.iter().filter_map(|m| m["kind"].as_str()).collect();
    let ats: Vec<i64> = msgs.iter().filter_map(|m| m["at"].as_i64()).collect();
    s.ok(
        "the session is a thread of its own, titled as it was, its messages user and talk with their own times",
        id.len() == 18
            && id.starts_with("t_")
            && thread["started"] == IMPORTED_T0
            && thread["last"] == IMPORTED_T0 + 300_000
            && kinds == ["user", "talk", "user", "talk", "user"]
            && ats == [0, 60, 120, 240, 300].map(|sec| IMPORTED_T0 + sec * 1000),
        json!({ "threads": threads, "thread": read }),
    );
    s.ok(
        "the turn with a tool call keeps its final reply alone",
        msgs.get(3).and_then(|m| m["text"].as_str()).is_some_and(|t| t.starts_with("Saved the swap list")) && !msgs.iter().any(|m| m["text"] == "Saving it."),
        json!(msgs),
    );
    let published = logged(api, &keys, &mind, "import");
    s.ok(
        "an import publishes its progress on log, and no msg record for each message",
        published.iter().any(|r| r["body"]["source"] == "claude-code" && r["body"]["n"] == 5 && r["body"]["thread"] == id.as_str()) && messages(api, &keys, &mind, &id).is_empty(),
        json!(published),
    );

    let done = s.eventually(TURN, || {
        let st = op(api, &keys, &mind, "status", json!({}));
        st["T"] == 5 && st["unbuilt"] == 0 && st["ready"] == false
    });
    let status = op(api, &keys, &mind, "status", json!({}));
    s.ok("the import starts the compactor, which summarizes every message", done && status["import"]["conversations"] == 1 && status["import"]["messages"] == 5, &status);
    let compactions: Vec<String> = s
        .ai
        .chats()
        .into_iter()
        .filter(|c| c["tool_choice"] == "none")
        .filter_map(|c| c["messages"][1]["content"][1]["text"].as_str().map(str::to_string))
        .filter(|t| t.contains("\n<input>\ntalk: Seed swap notes") || t.contains("\n<input>\nuser: the list: tomatoes") || t.contains("\n<input>\ntalk: Saved the swap list"))
        .collect();
    let memory = op(api, &keys, &mind, "memory", json!({}));
    s.ok(
        "each long message is a compaction of its own, UniiChat's task, and every line is built",
        compactions.len() == 3 && compactions.iter().all(|t| t.starts_with("Compaction: compress message ")) && memory["parts"].as_array().is_some_and(|p| p.iter().all(|x| x["built"] == true)),
        json!({ "memory": memory, "compactions": compactions }),
    );

    let part = |from: i64| {
        json!({ "source": "claude-code", "conversation": { "id": "e2e-import-session", "title": "Seed swap" }, "from": from,
                "messages": [{ "role": "user", "text": "one more", "at": IMPORTED_T0 + 400_000 }] })
    };
    let ahead = api.op(&keys, &mind, "import", &format!("import-ahead-{}", crate::api::now_ms()), part(9))?;
    s.ok("a part ahead of what landed is refused", ahead.status != 200 && ahead.to_string().contains("ahead"), &ahead);
    let landed = op(api, &keys, &mind, "import", part(4));
    let asked = op(api, &keys, &mind, "imported", json!({ "conversations": [{ "source": "claude-code", "id": "e2e-import-session" }, { "source": "codex", "id": "never" }] }));
    s.ok(
        "a part that landed already changes nothing, and imported says how much of each is in",
        landed["appended"] == 0 && landed["landed"] == 5 && landed["thread"] == id.as_str() && asked["landed"] == json!([5, 0]),
        json!({ "landed": landed, "imported": asked }),
    );

    // up to eight pumps at once: a longer import, each model call held a moment
    let held = Duration::from_millis(PUMP_CALL_MS);
    s.ai.reset_at_once();
    s.ai.delay_next(&[PUMP_CALL_MS; 64]);
    let began = std::time::Instant::now();
    let messages: Vec<Value> = (0..PUMP_MESSAGES)
        .map(|k| json!({ "role": if k % 2 == 0 { "user" } else { "assistant" }, "text": padded(&format!("pumped message {k}")), "at": IMPORTED_T0 + 600_000 + k * 1000 }))
        .collect();
    let r = api.op(&keys, &mind, "import", &format!("import-pumps-{}", crate::api::now_ms()), json!({ "source": "claude-code", "conversation": { "id": "e2e-pumps", "title": "Pumps" }, "from": 0, "messages": messages }))?;
    anyhow::ensure!(r.status == 200, "importing the pumps' conversation: {r}");
    // an import starts the compactor at most once a minute (the triggered
    // runs' hourly breaker): started as the CLI's follower starts it
    let r = api.op(&keys, &mind, "pump", &format!("pump-pumps-{}", crate::api::now_ms()), json!({}))?;
    anyhow::ensure!(r.status == 200, "starting a pump: {r}");
    let done = s.eventually(TURN, || {
        let st = op(api, &keys, &mind, "status", json!({}));
        st["T"] == 5 + PUMP_MESSAGES && st["unbuilt"] == 0 && st["ready"] == false
    });
    let took = began.elapsed();
    let most = s.ai.most_at_once();
    let calls = s.ai.chats().into_iter().filter(|c| c["tool_choice"] == "none" && c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.contains("pumped message"))).count();
    s.ok(
        &format!("an import is compacted by pumps at once: {most} calls at most at once, {calls} level-0 calls of {} ms each in {:.1} s", held.as_millis(), took.as_secs_f64()),
        done && (3..=8).contains(&most) && took < held * calls as u32,
        json!({ "most": most, "calls": calls, "seconds": took.as_secs_f64(), "status": op(api, &keys, &mind, "status", json!({})) }),
    );
    s.ai.clear_script();
    // measured, not checked: the same at the fake's own latency
    let began = std::time::Instant::now();
    let messages: Vec<Value> = (0..64)
        .map(|k| json!({ "role": if k % 2 == 0 { "user" } else { "assistant" }, "text": padded(&format!("quick message {k}")), "at": IMPORTED_T0 + 900_000 + k * 1000 }))
        .collect();
    let r = api.op(&keys, &mind, "import", &format!("import-quick-{}", crate::api::now_ms()), json!({ "source": "claude-code", "conversation": { "id": "e2e-quick", "title": "Quick" }, "from": 0, "messages": messages }))?;
    anyhow::ensure!(r.status == 200, "importing the quick conversation: {r}");
    let r = api.op(&keys, &mind, "pump", &format!("pump-quick-{}", crate::api::now_ms()), json!({}))?;
    anyhow::ensure!(r.status == 200, "starting a pump: {r}");
    let quick = s.eventually(TURN, || {
        let st = op(api, &keys, &mind, "status", json!({}));
        st["T"] == 5 + PUMP_MESSAGES + 64 && st["unbuilt"] == 0 && st["ready"] == false
    });
    let took = began.elapsed().as_secs_f64();
    println!("      (64 imported messages compacted at the fake's latency in {took:.1} s, {:.1} calls a second{})", 64.0 / took, if quick { "" } else { ": not all" });

    // an import not summarized yet, its compactions held: live chat passes it
    s.eventually(TURN, || {
        let st = op(api, &keys, &mind, "status", json!({}));
        st["unbuilt"] == 0 && st["ready"] == false
    });
    s.ai.hold(HELD);
    let passed = passing_an_import(s, api, &keys, &mind);
    s.ai.release();
    passed
}

/// Words only the held import's messages hold (`WorkersAi::hold`).
const HELD: &str = "unsummarizedimport";
/// The held import's messages: fewer than the compactor's eight at once,
/// so its held calls leave room for the live chat's.
const HELD_MESSAGES: i64 = 4;

/// docs/optchat.md, "Where we differ from the gist", 16: while an import's
/// messages are not summarized (its compactions held), a live message's
/// turn does not wait for them, and its view shows them as one marker line;
/// a second turn's wait is for the first turn's messages only, which the
/// compactor summarizes ahead of the import; released, the import is
/// summarized and the marker goes.
fn passing_an_import(s: &mut Suite, api: &Api, keys: &Keys, mind: &str) -> Result<()> {
    let first = op(api, keys, mind, "status", json!({}))["T"].as_i64().unwrap_or(0);
    let last = first + HELD_MESSAGES - 1;
    let marker = format!("(messages {first}–{last}: imported chats, not summarized yet; zoom(id, 1) gives one whole)");
    let part: Vec<Value> = (0..HELD_MESSAGES)
        .map(|k| json!({ "role": if k % 2 == 0 { "user" } else { "assistant" }, "text": padded(&format!("{HELD} message {k}")), "at": IMPORTED_T0 + 1_200_000 + k * 1000 }))
        .collect();
    let r = api.op(keys, mind, "import", &format!("import-held-{}", crate::api::now_ms()), json!({ "source": "claude-code", "conversation": { "id": "e2e-held", "title": "Held" }, "from": 0, "messages": part }))?;
    anyhow::ensure!(r.status == 200, "importing the held conversation: {r}");
    let r = api.op(keys, mind, "pump", &format!("pump-held-{}", crate::api::now_ms()), json!({}))?;
    anyhow::ensure!(r.status == 200, "starting a pump: {r}");
    let holding = s.eventually(Duration::from_secs(30), || s.ai.held() == HELD_MESSAGES as usize);
    s.ok("the held import's compactions are all at work, held", holding, json!({ "held": s.ai.held(), "status": op(api, keys, mind, "status", json!({})) }));

    let thread = "t_0f0f0f0f0f0f0f0f";
    let say = |id: &str, text: &str| api.signed(keys, "POST", &format!("/api/f/{mind}/channels/say"), Some(&json!({ "id": id, "body": { "text": text, "thread": thread } })));
    let asked = padded("what did I bring in today");
    let r = say("held-1", &asked)?;
    anyhow::ensure!(r.status == 200, "saying held-1: {r}");
    let answered = s.eventually(TURN, || messages(api, keys, mind, thread).iter().any(|m| m["kind"] == "talk"));
    let held = s.ai.held();
    let said = messages(api, keys, mind, thread);
    let turns: Vec<Value> = logged(api, keys, mind, "turn").into_iter().filter(|t| t["body"]["thread"] == thread).map(|t| t["body"]["state"].clone()).collect();
    let view = s.ai.chats().into_iter().find(|c| c["messages"][1]["content"][2]["text"] == asked.as_str()).and_then(|c| c["messages"][1]["content"][0]["text"].as_str().map(str::to_string)).unwrap_or_default();
    s.ok(
        "a live message's turn answers while the import is not summarized, never settling: its view has the import as one marker line",
        answered && held == HELD_MESSAGES as usize && !turns.contains(&json!("settling")) && view.contains(&format!("\n{marker}\n")) && !view.contains(HELD),
        json!({ "held": held, "turns": turns, "view": view, "messages": said }),
    );

    // the first turn's own messages (long ones: compactions) come before the import's
    let ids: Vec<i64> = said.iter().filter_map(|m| m["i"].as_i64()).collect();
    let r = say("held-2", "and the second thing")?;
    anyhow::ensure!(r.status == 200, "saying held-2: {r}");
    let again = s.eventually(TURN, || messages(api, keys, mind, thread).iter().filter(|m| m["kind"] == "talk").count() == 2);
    let held = s.ai.held();
    let view = s.ai.chats().into_iter().find(|c| c["messages"][1]["content"][2]["text"] == "and the second thing").and_then(|c| c["messages"][1]["content"][0]["text"].as_str().map(str::to_string)).unwrap_or_default();
    let after = view.split_once(&marker).map(|(_, rest)| rest.to_string()).unwrap_or_default();
    let compaction = s
        .ai
        .chats()
        .into_iter()
        .find(|c| c["tool_choice"] == "none" && c["messages"][1]["content"][1]["text"].as_str().is_some_and(|t| t.contains("<input>\nuser: what did I bring in today")))
        .and_then(|c| c["messages"][1]["content"][0]["text"].as_str().map(str::to_string))
        .unwrap_or_default();
    s.ok(
        "a second turn waits only for the first turn's messages, summarized ahead of the import (their compaction views pass it too)",
        again && held == HELD_MESSAGES as usize && ids.len() == 2 && ids.iter().all(|i| after.contains(&format!("\n{i}+1|"))) && compaction.contains(&format!("\n{marker}\n")),
        json!({ "held": held, "ids": ids, "view": view, "compaction": compaction }),
    );

    // released: the import is summarized and the marker goes
    s.ai.release();
    let done = s.eventually(TURN, || {
        let st = op(api, keys, mind, "status", json!({}));
        st["unbuilt"] == 0 && st["ready"] == false
    });
    let view = op(api, keys, mind, "view", json!({}));
    let memory = op(api, keys, mind, "memory", json!({}));
    s.ok(
        "released, the import is summarized: every line built, and the view has no marker",
        done && view["settled"] == true && view["text"].as_str().is_some_and(|t| !t.contains("not summarized yet")) && memory["parts"].as_array().is_some_and(|p| p.iter().all(|x| x["built"] == true)),
        json!({ "view": view, "memory": memory }),
    );
    Ok(())
}

/// The longer import: this many long messages, each call held this long.
const PUMP_MESSAGES: i64 = 24;
const PUMP_CALL_MS: u64 = 1500;
