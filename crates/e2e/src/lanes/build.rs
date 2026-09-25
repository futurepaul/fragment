//! Building an app from a chat, and a turn that never fails silently, in
//! the `chat` section. On fragment.club (2026-09-25) an agent asked to
//! build a countdown page read other fragments for a dozen calls to learn
//! the format, wrote the whole page in one reply that outlasted the node's
//! 120 s fetch, and ended its turn with nothing said. With the OpenRouter
//! fake scripted as the model:
//!
//! - the build flow: every turn's instructions carry the build guide (an
//!   agent made with the old default gets today's); the agent writes
//!   `site/index.html` to a fragment the person already made, deploys it,
//!   and answers; the page is live. Each request bounds its output.
//! - a reply cut off at `max_tokens` inside a file keeps what it wrote and
//!   says where it stops; the rest, appended, completes the file.
//! - a slow model: a call past its deadline is made once more; a second
//!   timeout ends the turn in an error, said in the chat and on `work`.
//! - an empty answer is asked for again; empty twice with nothing done is
//!   an error said in the chat; empty twice after work that worked is
//!   answered with what that work did.

use std::time::Duration;

use anyhow::Result;
use fragment_fakes::openrouter::{Reply, CHARS_PER_TOKEN};
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::agents::{chat_records, say, settle};
use crate::api::Api;
use crate::Suite;

const STARSHIP: &str = include_str!("../../fixtures/starship.html");
/// What Paul asked on 2026-09-25 (the fragment's label is this run's).
const ASKED: &str = "Build my starship-countdown app: a single page counting down to the next SpaceX Starship launch, \
                     big numbers, dark space theme. Put it in the starship-countdown fragment I already made and deploy it.";
/// Lines of the guide the model must be told (agent/src/lib.rs `BUILD_GUIDE`).
const GUIDE_LINES: [&str; 3] = [
    "How to build an app (a fragment):",
    "Do not read other fragments or templates to learn this format",
    "One file per call, each under 150 lines",
];
/// The old default's advice, which the guide replaces.
const OLD_ADVICE: &str = "read the todo template's files for the shape";

fn work_records(api: &Api, keys: &Keys, chat: &str) -> Vec<Value> {
    api.signed(keys, "GET", &format!("/api/f/{chat}/channels/work?after=0"), None).ok().and_then(|r| r.body["records"].as_array().cloned()).unwrap_or_default()
}

/// The agent's answer on `chat` whose text starts with `text`, if it has one.
fn answered(api: &Api, keys: &Keys, chat: &str, agent: &str, text: &str) -> Option<Value> {
    chat_records(api, keys, chat).into_iter().find(|r| r["principal"] == agent && r["body"]["text"].as_str().is_some_and(|t| t.starts_with(text)))
}

/// The work records of `turn`, once its `turn.end` is there.
fn ended(s: &Suite, api: &Api, keys: &Keys, chat: &str, turn: &str) -> Vec<Value> {
    let of_turn = || work_records(api, keys, chat).into_iter().filter(|r| r["body"]["turn"] == turn).collect::<Vec<_>>();
    s.eventually(Duration::from_secs(30), || of_turn().iter().any(|r| r["body"]["kind"] == "turn.end"));
    of_turn()
}

fn kinds(records: &[Value]) -> Vec<String> {
    records.iter().map(|r| r["body"]["kind"].as_str().unwrap_or("").to_string()).collect()
}

/// The text of a model request's system prompt.
fn system(chat: &Value) -> String {
    chat["messages"][0]["content"].as_str().map(str::to_string).unwrap_or_else(|| chat["messages"][0]["content"].to_string())
}

/// The last message of a model request, as text.
fn last_message(chat: &Value) -> String {
    chat["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].to_string()).unwrap_or_default()
}

pub(super) fn build(s: &mut Suite, api: &Api) -> Result<()> {
    let agents = s.agents()?;
    let wait = Duration::from_secs(30);
    let owner = api.person()?;
    let chat = s.named(api, &owner, "build-chat")?;
    let made = api.create_with(&owner, json!({ "name": chat, "template": "chat" }))?;
    anyhow::ensure!(made.status == 200, "a chat from the template: {made}");
    s.hook(api, &made.body);
    // the owner's own agent joins a chat made from the template, and listens
    let agent_of = || -> Option<String> {
        let members = api.signed(&owner, "GET", &format!("/api/f/{chat}/members"), None).ok()?;
        members.body["members"].as_array()?.iter().find(|m| m["kind"] == "agent").and_then(|m| m["principal"].as_str().map(str::to_string))
    };
    let listening = || api.signed(&owner, "GET", &format!("/api/f/{chat}/subscriptions"), None).map_or(0, |r| r.body["subscriptions"].as_array().map_or(0, Vec::len));
    let joined = s.eventually(wait, || agent_of().is_some() && listening() == 1);
    anyhow::ensure!(joined, "the owner's agent joins the chat and listens");
    let agent = agent_of().unwrap_or_default();
    let test = |controls: Value| agents.signed(&owner, "POST", "/api/a/agent/test", Some(&controls));
    let turn_of = |answer: &Value| answer["body"]["turn"].as_str().unwrap_or("").to_string();

    // the build flow: a fragment the person already made (the blank template)
    let app = s.named(api, &owner, "starship-countdown")?;
    let r = api.create_with(&owner, json!({ "name": app, "template": "blank" }))?;
    anyhow::ensure!(r.status == 200, "the starship-countdown fragment: {r}");
    s.hook(api, &r.body);
    let view_token = r.body["viewToken"].as_str().unwrap_or("").to_string();
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__write_file".into(), json!({ "fragment": app, "path": "site/index.html", "text": STARSHIP }))]),
        Reply::Tools(vec![("platform__deploy".into(), json!({ "fragment": app, "note": "the countdown" }))]),
        Reply::Text("Your Starship countdown is live.".into()),
    ]);
    let asked = s.openrouter.chats().len();
    say(api, &owner, &chat, "b1", &ASKED.replace("starship-countdown fragment", &format!("{app} fragment")))?;
    let done = s.eventually(wait, || answered(api, &owner, &chat, &agent, "Your Starship countdown is live.").is_some());
    let answer = answered(api, &owner, &chat, &agent, "Your Starship countdown is live.").unwrap_or_default();
    let requests: Vec<Value> = s.openrouter.chats()[asked..].to_vec();
    let prompt = requests.first().map(system).unwrap_or_default();
    s.ok(
        "the instructions the model is sent include the build guide, not the old advice to read a template",
        GUIDE_LINES.iter().all(|l| prompt.contains(l)) && !prompt.contains(OLD_ADVICE),
        &prompt,
    );
    s.ok(
        "each request bounds the reply (max_tokens), so a reply ends inside the node's fetch",
        !requests.is_empty() && requests.iter().all(|c| c["max_tokens"] == 4096),
        json!(requests.iter().map(|c| c["max_tokens"].clone()).collect::<Vec<_>>()),
    );
    let page = api.page(&app, "", Some(&format!("fragview={view_token}")))?;
    let records = ended(s, api, &owner, &chat, &turn_of(&answer));
    let steps: Vec<(String, bool)> = records.iter().filter(|r| r["body"]["kind"] == "turn.step").map(|r| (r["body"]["tool"].as_str().unwrap_or("").to_string(), r["body"]["ok"] == true)).collect();
    s.ok(
        "asked to build a page in a fragment the person made, the agent writes site/index.html, deploys, and answers in the chat",
        done && steps == [("platform__write_file".to_string(), true), ("platform__deploy".to_string(), true)] && kinds(&records).last().is_some_and(|k| k == "turn.end"),
        json!({ "answer": answer, "work": records }),
    );
    s.ok("the page is live, as written", page.status == 200 && page.text.contains("Next Starship launch") && page.text.contains("setInterval(tick, 1000)"), &page);

    // an agent made with the old default instructions gets today's
    let old = s.name("old-default");
    let old_default = format!(
        "You are {old}, an agent. Most of your tools are operations of the fragments you belong to: shared places such as an \
         app, a list, or a chat. The platform__ tools make new fragments for your owner and change their files: when asked \
         for an app, make one, {OLD_ADVICE}, write yours, deploy it, and say where it is. Do what you are asked, one call at \
         a time, and when the work is done answer in one short sentence."
    );
    let r = agents.signed(&owner, "POST", "/api/agents", Some(&json!({ "name": old, "instructions": old_default })))?;
    anyhow::ensure!(r.status == 200, "an agent with the old default: {r}");
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Text("Hello.".into())]);
    let asked = s.openrouter.chats().len();
    agents.signed(&owner, "POST", &format!("/api/a/{old}/turns"), Some(&json!({ "text": "hello" })))?;
    let v = settle(s, &agents, &owner, &old, wait);
    let prompt = s.openrouter.chats().get(asked).map(system).unwrap_or_default();
    s.ok(
        "an agent made with the old default instructions is told today's, and the guide",
        v["outcome"] == "idle" && !prompt.contains(OLD_ADVICE) && GUIDE_LINES.iter().all(|l| prompt.contains(l)),
        &prompt,
    );

    // a reply cut off at max_tokens inside a file: what it wrote is kept,
    // the model is told where it stops, and the rest, appended, completes it
    let long: String = (0..400).map(|i| format!("<p class=l{i}>line {i} of a page longer than one reply</p>")).collect();
    let args = json!({ "fragment": app, "path": "site/long.html", "text": long });
    let budget = 4096 * CHARS_PER_TOKEN;
    let head = args.to_string().find(&long[..32]).unwrap_or(0);
    anyhow::ensure!(args.to_string().len() > budget && head > 0, "the long page outgrows one reply");
    let kept = budget - head;
    s.openrouter.clear_script();
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__write_file".into(), args.clone())]),
        Reply::Tools(vec![("platform__append_file".into(), json!({ "fragment": app, "path": "site/long.html", "text": &long[kept..] }))]),
        Reply::Tools(vec![("platform__deploy".into(), json!({ "fragment": app }))]),
        Reply::Text("Your long page is up.".into()),
    ]);
    let asked = s.openrouter.chats().len();
    say(api, &owner, &chat, "b2", "write me a long page in the same fragment")?;
    let done = s.eventually(wait, || answered(api, &owner, &chat, &agent, "Your long page is up.").is_some());
    let requests: Vec<Value> = s.openrouter.chats()[asked..].to_vec();
    let told = requests.get(1).map(last_message).unwrap_or_default();
    let file = api.signed(&owner, "GET", &format!("/api/f/{app}/file?path=site%2Flong.html"), None)?;
    s.ok(
        "a write cut off at the reply's limit keeps what it wrote, and the model is told where the file stops",
        told.contains("Cut off") && told.contains(&long[kept - 40..kept]) && told.contains("platform__append_file"),
        &told,
    );
    s.ok(
        "the rest, appended, completes the file, and the turn answers",
        done && file.status == 200 && file.text == long,
        json!({ "done": done, "status": file.status, "bytes": file.text.len(), "want": long.len() }),
    );

    // a slow model: past its deadline (1.5 s here) a call is made once
    // more, told to send less; answered then, the turn goes on
    test(json!({ "model_timeout_ms": 1500 }))?;
    s.openrouter.clear_script();
    s.openrouter.delay_next(&[4000]);
    s.openrouter.script(&[Reply::Text("Too slow to be read.".into()), Reply::Text("Answered on the second try.".into())]);
    let asked = s.openrouter.chats().len();
    say(api, &owner, &chat, "b3", "are you there?")?;
    let done = s.eventually(wait, || answered(api, &owner, &chat, &agent, "Answered on the second try.").is_some());
    let requests: Vec<Value> = s.openrouter.chats()[asked..].to_vec();
    let slow = answered(api, &owner, &chat, &agent, "Too slow").is_some();
    s.ok(
        "a model call past its deadline is made once more, told to send less, and its answer lands",
        done && !slow && requests.len() == 2 && last_message(&requests[1]).contains("took longer than the time limit"),
        json!({ "requests": requests.len(), "last": requests.last().map(last_message) }),
    );
    // twice past it: the turn ends in an error, said where its answer goes
    s.openrouter.delay_next(&[4000, 4000]);
    s.openrouter.script(&[Reply::Text("Too slow, once.".into()), Reply::Text("Too slow, twice.".into())]);
    let asked = s.openrouter.chats().len();
    say(api, &owner, &chat, "b4", "build it again")?;
    let failed = "I couldn't finish: the model timed out";
    let said = s.eventually(wait, || answered(api, &owner, &chat, &agent, failed).is_some());
    let answer = answered(api, &owner, &chat, &agent, failed).unwrap_or_default();
    let records = ended(s, api, &owner, &chat, &turn_of(&answer));
    let end = records.iter().find(|r| r["body"]["kind"] == "turn.end").map(|r| r["body"].clone()).unwrap_or_default();
    let v = settle(s, &agents, &owner, "agent", wait);
    s.ok(
        "timed out twice, the turn ends in an error the chat says (\"I couldn't finish … Ask me to try again.\"), with its turn",
        said && s.openrouter.chats().len() - asked == 2 && answer["body"]["text"].as_str().is_some_and(|t| t.ends_with("Ask me to try again.")) && !turn_of(&answer).is_empty(),
        json!({ "answer": answer, "requests": s.openrouter.chats().len() - asked }),
    );
    s.ok(
        "and its turn.end on work carries the error, as the agent's view does",
        end["outcome"] == "error" && end["error"].as_str().is_some_and(|e| e.contains("timed out")) && v["outcome"] == "error" && v["error"].as_str().is_some_and(|e| e.contains("timed out")),
        json!({ "end": end, "outcome": v["outcome"], "error": v["error"] }),
    );
    test(json!({}))?;

    // an empty answer (reasoning alone is nothing) is asked for again
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Thinking("Let me think about that.".into()), Reply::Text("Here I am.".into())]);
    let asked = s.openrouter.chats().len();
    say(api, &owner, &chat, "b5", "hello?")?;
    let done = s.eventually(wait, || answered(api, &owner, &chat, &agent, "Here I am.").is_some());
    let requests: Vec<Value> = s.openrouter.chats()[asked..].to_vec();
    s.ok(
        "an empty answer is asked for again, and the second lands",
        done && requests.len() == 2 && last_message(&requests[1]).contains("Your last reply was empty"),
        json!({ "requests": requests.len(), "last": requests.last().map(last_message) }),
    );
    // empty twice, nothing done: an error, said in the chat
    s.openrouter.script(&[Reply::Empty, Reply::Thinking("Still thinking.".into())]);
    say(api, &owner, &chat, "b6", "anything?")?;
    let failed = "I couldn't finish: the model answered with nothing";
    let said = s.eventually(wait, || answered(api, &owner, &chat, &agent, failed).is_some());
    let answer = answered(api, &owner, &chat, &agent, failed).unwrap_or_default();
    let records = ended(s, api, &owner, &chat, &turn_of(&answer));
    let end = records.iter().find(|r| r["body"]["kind"] == "turn.end").map(|r| r["body"].clone()).unwrap_or_default();
    s.ok(
        "empty twice with nothing done: the turn ends in an error the chat says, and work carries it",
        said && end["outcome"] == "error" && end["error"].as_str().is_some_and(|e| e.contains("nothing")),
        json!({ "answer": answer, "end": end }),
    );
    // empty twice after work that worked: what that work did, instead
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__write_file".into(), json!({ "fragment": app, "path": "site/note.txt", "text": "a note" }))]),
        Reply::Empty,
        Reply::Thinking("Done, I think.".into()),
    ]);
    say(api, &owner, &chat, "b7", "leave a note in it")?;
    let summary = format!("Done. Here is what I did: wrote site/note.txt in {app}.");
    let said = s.eventually(wait, || answered(api, &owner, &chat, &agent, &summary).is_some());
    let answer = answered(api, &owner, &chat, &agent, &summary).unwrap_or_default();
    let records = ended(s, api, &owner, &chat, &turn_of(&answer));
    s.ok(
        "empty twice after work that worked: the answer says what the work did, and the turn is answered",
        said && records.iter().any(|r| r["body"]["kind"] == "turn.end" && r["body"]["outcome"] == "idle"),
        json!({ "answer": answer, "work": kinds(&records) }),
    );
    s.openrouter.clear_script();
    Ok(())
}
