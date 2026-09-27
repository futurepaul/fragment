//! The agent's work guide, and a turn that never fails silently, in the
//! `chat` section. On fragment.club (2026-09-25) an agent asked to build a
//! countdown page read other fragments for a dozen calls, wrote the whole
//! page in one reply that outlasted the node's 120 s fetch, and ended its
//! turn with nothing said; since 2026-09-27 it builds nothing itself and
//! hands such work to a computer (the `builder` section's hand-offs). With
//! the OpenRouter fake scripted as the model:
//!
//! - every turn's instructions carry the work guide, not the build guide it
//!   replaced (an agent made with an old default gets today's); each
//!   request bounds its output.
//! - a slow model: a call past its deadline is made once more; a second
//!   timeout ends the turn in an error, said in the chat and on `work`.
//! - an empty answer is asked for again; empty twice with nothing done is
//!   an error said in the chat; empty twice after work that worked is
//!   answered with what that work did.

use std::time::Duration;

use anyhow::Result;
use fragment_fakes::openrouter::Reply;
use fragment_nip98::Keys;
use serde_json::{json, Value};

use super::agents::{chat_records, say, settle};
use crate::api::Api;
use crate::Suite;

/// Lines of the guide the model must be told (agent/src/lib.rs `WORK_GUIDE`).
const GUIDE_LINES: [&str; 3] = ["What you do, and what you hand off:", "Hand off the rest with platform__hand_off", "Only your owner's turns can hand off"];
/// The oldest default's advice, and the build guide's opening: gone.
const OLD_ADVICE: [&str; 2] = ["read the todo template's files for the shape", "How to build an app (a fragment):"];

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

    // every turn's instructions carry the work guide
    s.openrouter.clear_script();
    s.openrouter.script(&[Reply::Text("Hi there.".into())]);
    let asked = s.openrouter.chats().len();
    say(api, &owner, &chat, "b1", "hello")?;
    let done = s.eventually(wait, || answered(api, &owner, &chat, &agent, "Hi there.").is_some());
    let requests: Vec<Value> = s.openrouter.chats()[asked..].to_vec();
    let prompt = requests.first().map(system).unwrap_or_default();
    s.ok(
        "the instructions the model is sent include the work guide, not the build guide it replaced",
        done && GUIDE_LINES.iter().all(|l| prompt.contains(l)) && !OLD_ADVICE.iter().any(|l| prompt.contains(l)),
        &prompt,
    );
    s.ok(
        "each request bounds the reply (max_tokens), so a reply ends inside the node's fetch",
        !requests.is_empty() && requests.iter().all(|c| c["max_tokens"] == 4096),
        json!(requests.iter().map(|c| c["max_tokens"].clone()).collect::<Vec<_>>()),
    );

    // an agent made with the old default instructions gets today's
    let old = s.name("old-default");
    let old_default = format!(
        "You are {old}, an agent. Most of your tools are operations of the fragments you belong to: shared places such as an \
         app, a list, or a chat. The platform__ tools make new fragments for your owner and change their files: when asked \
         for an app, make one, {}, write yours, deploy it, and say where it is. Do what you are asked, one call at a time, \
         and when the work is done answer in one short sentence.",
        OLD_ADVICE[0]
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
        v["outcome"] == "idle" && !OLD_ADVICE.iter().any(|l| prompt.contains(l)) && GUIDE_LINES.iter().all(|l| prompt.contains(l)),
        &prompt,
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
    let label = s.name("noted");
    s.openrouter.script(&[
        Reply::Tools(vec![("platform__create_fragment".into(), json!({ "label": label, "template": "blank" }))]),
        Reply::Empty,
        Reply::Thinking("Done, I think.".into()),
    ]);
    say(api, &owner, &chat, "b7", "make me a blank page")?;
    let summary = format!("Done. Here is what I did: made {}.", api.qualified(&owner, &label)?);
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
