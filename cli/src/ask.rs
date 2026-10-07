//! `fragment ask <agent> <text>`: ask another of your agents something
//! (an agent asks another of its owner's), in a chat its owner sees.
//!
//! The chat is `--chat`, or else one of the two agents and their owner
//! (`<a>-<b>`, their labels in order, so either asking the other finds the
//! same one), made the first time on the blessed `chat` template; a person
//! asking finds their direct chat with the agent (`<agent>-chat`, as the
//! shell names it). Each agent not in the chat is added as an editor, as
//! the shell adds agents (decision 36: an agent shares its owner's
//! fragments for them). The question is a message whose `to` names the
//! asked agent (docs/chat-records.md), so its bridge takes it as a turn;
//! the hop is that bridge's to count, never this post's (engine.rs,
//! `hop_of`). `--wait` follows the chat's `work` for the asked agent's turn
//! of that message, and prints its replies once the turn ends.
//!
//! Who may be asked is the computer's own list: an agent's `GET
//! /api/computer` (its computer's agents, all its owner's), a person's
//! `GET /api/computers`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fragment_proto::{Created, MemberList, Posted, Role};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::api::{Client, Code, CodedError};

/// A DNS label's longest: a fragment's host is one, `<label>--<username>`,
/// so a chat's label made here leaves room for its owner's username.
pub const HOST_LABEL_MAX: usize = 63;
/// `--wait`'s longest wait, in seconds (its default, 300, is main.rs's
/// flag's): an answer is a model's turn or more, and a tool call that waits
/// past its runtime's own limit is cut (Hermes' terminal tool, an agent's
/// idle bound).
pub const WAIT_S_MAX: u64 = 1_800;
/// How often `--wait` reads the chat's `work`.
pub const POLL_MS: u64 = 2_000;
/// Pages of a channel one look reads, at most (1000 records each).
const PAGES_MAX: usize = 10;

/// An agent that may be asked: one its owner's computer runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    /// Its agent fragment (`fred.paul`).
    pub fragment: String,
    /// Its identity (`id:…`), which a message's `to` names.
    pub identity: String,
    /// Its label (`fred`): how it is @mentioned.
    pub name: String,
}

fn label(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

fn username(name: &str) -> Option<&str> {
    name.split_once('.').map(|(_, u)| u)
}

/// The agent `who` names: its fragment's name or label, its name, or its
/// identity (case aside, but for the identity).
pub fn pick<'a>(agents: &'a [Agent], who: &str) -> Option<&'a Agent> {
    let who = who.trim();
    let lower = who.to_ascii_lowercase();
    agents.iter().find(|a| a.identity == who || a.fragment == lower || label(&a.fragment) == lower || a.name.to_ascii_lowercase() == lower)
}

/// `s` cut to at most `max` bytes (a label is ASCII), never ending in `-`.
fn cut_label(s: &str, max: usize) -> String {
    let cut: String = s.chars().take(max).collect();
    cut.trim_end_matches('-').to_string()
}

/// How long a label of `owner`'s may be: its host, `<label>--<owner>`, is
/// one DNS label.
fn label_max(owner: &str) -> usize {
    HOST_LABEL_MAX.saturating_sub(2 + owner.len()).min(fragment_proto::limits::NAME_MAX_BYTES)
}

/// The chat of two agents (fragment names, one owner's) and their owner:
/// their labels in order, joined by `-`, each cut so its host is one DNS
/// label. Either asking the other names the same chat.
pub fn pair_label(a: &str, b: &str) -> String {
    let max = label_max(username(a).unwrap_or(""));
    let (a, b) = (label(a), label(b));
    let (first, second) = if a <= b { (a, b) } else { (b, a) };
    let half = (max - 1) / 2;
    let out = if first.len() + 1 + second.len() <= max { format!("{first}-{second}") } else { format!("{}-{}", cut_label(first, half), cut_label(second, half)) };
    assert!(out.len() <= max && fragment_proto::valid_label(&out), "a pair's label is a label whose host fits: {out}");
    out
}

/// A person's direct chat with an agent (its fragment's name), as the shell
/// names it: `<label>-chat`.
pub fn direct_label(agent: &str) -> String {
    let max = label_max(username(agent).unwrap_or(""));
    let out = format!("{}-chat", cut_label(label(agent), max - "-chat".len()));
    assert!(out.len() <= max && fragment_proto::valid_label(&out), "a direct chat's label is a label whose host fits: {out}");
    out
}

/// The asked agent's turn of the message at `seq` on `chat`, from records
/// of the chat's `work` (docs/chat-records.md: its `turn.start` names the
/// agent and the record that caused it).
pub fn turn_for(work: &[Value], agent: &str, seq: i64) -> Option<String> {
    work.iter().map(|r| &r["body"]).find(|b| b["kind"] == "turn.start" && b["agent"] == agent && b["cause"]["channel"] == "chat" && b["cause"]["seq"].as_i64() == Some(seq)).and_then(|b| b["turn"].as_str().map(str::to_string))
}

/// How `turn` ended, from records of `work`: its outcome, and its error.
pub fn end_of(work: &[Value], turn: &str) -> Option<(String, Option<String>)> {
    work.iter().map(|r| &r["body"]).find(|b| b["kind"] == "turn.end" && b["turn"] == turn).map(|b| (b["outcome"].as_str().unwrap_or("").to_string(), b["error"].as_str().map(str::to_string)))
}

/// One reply of the asked agent's turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reply {
    pub seq: i64,
    pub text: String,
    /// Files it carries (the chat's blobs).
    pub attachments: usize,
}

/// `agent`'s replies in `turn`, from records of `chat`, in order.
pub fn replies_of(chat: &[Value], agent: &str, turn: &str) -> Vec<Reply> {
    chat.iter()
        .filter(|r| r["principal"] == agent && r["body"]["turn"] == turn && r["body"].get("kind").is_none_or(|k| k == "message"))
        .map(|r| Reply {
            seq: r["seq"].as_i64().unwrap_or(0),
            text: r["body"]["text"].as_str().unwrap_or("").to_string(),
            attachments: r["body"]["attachments"].as_array().map_or(0, Vec::len),
        })
        .collect()
}

/// What `--wait` found: the asked agent's turn, how it ended, its replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Answer {
    pub turn: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub replies: Vec<Reply>,
}

/// What `fragment ask` did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Asked {
    /// The chat's full name.
    pub chat: String,
    pub asked: Agent,
    /// The question's record.
    pub record: Value,
    pub replayed: bool,
    /// The chat was made for this question.
    pub created: bool,
    /// The agents added to the chat for it.
    pub added: Vec<String>,
    /// `--wait`: its answer, or none in the time given.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<Answer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub waited_s: Option<u64>,
}

fn coded(code: Code, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError { code, msg: msg.into() })
}

#[derive(Deserialize)]
struct Roster {
    agents: Vec<Agent>,
}

#[derive(Deserialize)]
struct Computers {
    computers: Vec<Roster>,
}

/// The asker (an agent; none for a person) and the agents it may ask.
fn roster(c: &Client) -> Result<(Option<Agent>, Vec<Agent>)> {
    match c.agent() {
        Some(mode) => {
            // the computer's own route: every agent it runs, all its owner's
            let r: Roster = c.call_as(c.get("/api/computer")?).context("reading this computer's agents")?;
            let me = r.agents.iter().find(|a| a.fragment == mode.agent).cloned().ok_or_else(|| coded(Code::Forbidden, format!("{} does not run on this computer", mode.agent)))?;
            Ok((Some(me), r.agents))
        }
        None => {
            let r: Computers = c.call_as(c.get("/api/computers")?).context("reading your computer's agents")?;
            Ok((None, r.computers.into_iter().flat_map(|c| c.agents).collect()))
        }
    }
}

/// The chat's members, or none when there is no such chat.
fn members(c: &Client, chat: &str) -> Result<Option<MemberList>> {
    let r = c.get(&format!("/api/f/{chat}/members"))?;
    if r.status == 404 {
        return Ok(None);
    }
    c.call_as(r).map(Some)
}

/// The fragments' titles, by name: a new chat is titled by its agents'.
fn titles(c: &Client) -> Vec<(String, String)> {
    let Ok(v) = c.get("/api/fragments").and_then(|r| c.call(r)) else { return Vec::new() };
    v["fragments"].as_array().map(|l| l.iter().filter_map(|f| Some((f["name"].as_str()?.to_string(), f["title"].as_str()?.to_string()))).collect()).unwrap_or_default()
}

fn title_of(titles: &[(String, String)], a: &Agent) -> String {
    titles.iter().find(|(n, _)| *n == a.fragment).map(|(_, t)| t.clone()).unwrap_or_else(|| {
        let mut l = a.name.clone();
        if let Some(first) = l.get_mut(0..1) {
            first.make_ascii_uppercase();
        }
        l
    })
}

/// Asks `who` `text`, as the CLI's signer: finds or makes the chat, adds
/// whoever of the two is missing, posts, and with `wait` follows the
/// asked agent's turn to its end.
pub fn ask(c: &Client, who: &str, text: &str, chat: Option<&str>, wait: Option<u64>, id: String) -> Result<Asked> {
    let text = text.trim();
    if text.is_empty() {
        return Err(coded(Code::InvalidUsage, "ask something: the text is empty"));
    }
    if let Some(s) = wait {
        if !(1..=WAIT_S_MAX).contains(&s) {
            return Err(coded(Code::InvalidUsage, format!("--wait is 1 to {WAIT_S_MAX} seconds")));
        }
    }
    let (asker, agents) = roster(c)?;
    let asked = pick(&agents, who).cloned().ok_or_else(|| {
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        coded(Code::NotFound, format!("{who:?} is none of the agents that may be asked here ({})", if names.is_empty() { "none".into() } else { names.join(", ") }))
    })?;
    if asker.as_ref().is_some_and(|a| a.identity == asked.identity) {
        return Err(coded(Code::InvalidUsage, "an agent asks another agent, not itself"));
    }
    let owner_name = username(&asked.fragment).ok_or_else(|| coded(Code::ServerError, format!("the computer named the agent fragment {:?}", asked.fragment)))?.to_string();
    let (named, ours) = match chat {
        Some(c) => (c.trim().to_string(), false),
        None => match &asker {
            Some(a) => (pair_label(&a.fragment, &asked.fragment), true),
            None => (direct_label(&asked.fragment), true),
        },
    };
    let full = if named.contains('.') { named.clone() } else { format!("{named}.{owner_name}") };

    // the chat: found, or (ours to name) made
    let mut created = false;
    let mut list = match members(c, &full)? {
        Some(m) => m,
        None if ours => {
            let all = titles(c);
            let title = match &asker {
                Some(a) => format!("{} and {}", title_of(&all, a), title_of(&all, &asked)),
                None => title_of(&all, &asked),
            };
            let made = c.post_json("/api/fragments", &json!({ "name": label(&full), "template": "chat", "title": title }))?;
            if made.status == 409 {
                // made meanwhile (the other agent asking at once): the same chat
            } else {
                let made: Created = c.call_as(made)?;
                assert_eq!(made.name, full, "a chat made under its owner's name");
                created = true;
            }
            members(c, &full)?.ok_or_else(|| coded(Code::ServerError, format!("{full} was made, and has no members")))?
        }
        None => return Err(coded(Code::NotFound, format!("no chat named {full}"))),
    };
    let channels = c.call(c.get(&format!("/api/f/{full}/channels"))?)?;
    let listed = channels["channels"].as_array().cloned().unwrap_or_default();
    let postable_chat = listed.iter().any(|ch| ch["name"] == "chat" && ch["post"].is_string());
    if !postable_chat {
        return Err(coded(Code::InvalidRequest, format!("{full} is no chat: it has no channel `chat` to post on (pass --chat)")));
    }
    let work_seq = listed.iter().find(|ch| ch["name"] == "work").and_then(|ch| ch["seq"].as_i64()).unwrap_or(0);

    // each of the two in it, an editor (an agent's turns are recorded on `work`)
    let mut added = Vec::new();
    for (who, must) in asker.iter().map(|a| (a, false)).chain([(&asked, true)]) {
        match list.members.iter().find(|m| m.principal == who.identity) {
            Some(m) if must && m.role < Role::Editor => {
                return Err(coded(Code::Forbidden, format!("{} is a viewer in {full}, so it cannot answer there (its turns are an editor's): ask it in another chat", who.name)));
            }
            Some(_) => {}
            None => {
                let r = c.put_json(&format!("/api/f/{full}/members/{}", who.identity), &json!({ "role": "editor" }))?;
                if r.ok() {
                    added.push(who.identity.clone());
                } else if must {
                    let e = r.refusal();
                    return Err(coded(e.code, format!("{} is not in {full}, and adding it was refused ({}): only the chat's owner, or their agent, adds agents to it", who.name, e.msg)));
                }
                // the asker not added (a chat its owner only edits): it posts acting for them
            }
        }
    }
    if !added.is_empty() {
        list = members(c, &full)?.unwrap_or(list);
    }
    assert!(list.members.iter().any(|m| m.principal == asked.identity), "the asked agent is in the chat before it is asked");

    let body = json!({ "text": text, "to": [asked.identity] });
    let posted: Posted = c
        .post_json_by_id(&format!("/api/f/{full}/channels/chat"), &fragment_proto::PostRecord { id: id.clone(), body })
        .and_then(|r| c.call_as(r))
        .map_err(|e| e.context(crate::CallId::post(&id)))?;
    let seq = posted.record.seq;
    let record = serde_json::to_value(&posted.record)?;
    let answer = match wait {
        Some(s) => answered(c, &full, &asked.identity, seq, work_seq, Duration::from_secs(s))?,
        None => None,
    };
    Ok(Asked { chat: full, asked, record, replayed: posted.replayed, created, added, answer, waited_s: wait })
}

/// Records of `channel` after `after`, at most `PAGES_MAX` pages, and the
/// cursor after them.
fn records_after(c: &Client, chat: &str, channel: &str, after: i64) -> Result<(Vec<Value>, i64)> {
    let (mut out, mut cursor) = (Vec::new(), after);
    // bounded: PAGES_MAX pages
    for _ in 0..PAGES_MAX {
        let page = c.call(c.get(&format!("/api/f/{chat}/channels/{channel}?after={cursor}"))?)?;
        let records = page["records"].as_array().cloned().unwrap_or_default();
        let next = page["next"].as_i64().unwrap_or(cursor);
        let full = records.len() >= 1000;
        out.extend(records);
        if next <= cursor || !full {
            cursor = next.max(cursor);
            break;
        }
        cursor = next;
    }
    Ok((out, cursor))
}

/// Follows the chat's `work` from `work_seq` until the asked agent's turn
/// of the record at `seq` ends, or `limit` passes: its replies, or none.
fn answered(c: &Client, chat: &str, agent: &str, seq: i64, work_seq: i64, limit: Duration) -> Result<Option<Answer>> {
    let deadline = Instant::now() + limit;
    let (mut seen, mut cursor, mut turn) = (Vec::new(), work_seq, None::<String>);
    // bounded by the deadline: one look every POLL_MS
    loop {
        let (more, next) = records_after(c, chat, "work", cursor)?;
        cursor = next;
        seen.extend(more);
        if turn.is_none() {
            turn = turn_for(&seen, agent, seq);
        }
        if let Some(t) = &turn {
            if let Some((outcome, error)) = end_of(&seen, t) {
                let (said, _) = records_after(c, chat, "chat", seq)?;
                return Ok(Some(Answer { turn: t.clone(), outcome, error, replies: replies_of(&said, agent, t) }));
            }
            // only its own records matter from here
            seen.retain(|r| r["body"]["turn"] == t.as_str());
        } else {
            // the start may be in a later page; a record of no turn of its is none of ours
            seen.retain(|r| r["body"]["agent"] == agent);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        std::thread::sleep(left.min(Duration::from_millis(POLL_MS)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(label: &str) -> Agent {
        Agent { fragment: format!("{label}.paul"), identity: format!("id:{label}"), name: label.into() }
    }

    /// Goal: an agent is named as a person would name it. Invalid: a name
    /// that is none of them, or an identity in other case.
    #[test]
    fn an_agent_is_picked_by_any_of_its_names() {
        let agents = [agent("juniper"), agent("fred")];
        for who in ["fred", "Fred", "fred.paul", "FRED.paul", " fred ", "id:fred"] {
            assert_eq!(pick(&agents, who).map(|a| a.name.as_str()), Some("fred"), "{who}");
        }
        for who in ["fre", "fred.skyler", "id:FRED", "", "juniper-chat"] {
            assert_eq!(pick(&agents, who), None, "{who}");
        }
    }

    /// Goal: two agents name one chat, whichever asks, and it is a label
    /// whose host (`<label>--<username>`) is one DNS label, however long
    /// their names and the owner's. Invalid: a cut that would end in `-` or
    /// make a `--` is trimmed.
    #[test]
    fn two_agents_name_one_chat() {
        assert_eq!(pair_label("juniper.paul", "fred.paul"), "fred-juniper");
        assert_eq!(pair_label("fred.paul", "juniper.paul"), "fred-juniper", "either way round");
        let host_fits = |l: &str, owner: &str| fragment_proto::valid_label(l) && l.len() + 2 + owner.len() <= HOST_LABEL_MAX;
        let owner = "a-rather-long-username-of-thirty";
        for (a, b) in [
            ("a".repeat(32), format!("{}-b", "b".repeat(29))),
            (format!("{}-x", "c".repeat(12)), "d".repeat(32)),
            (format!("{}-z", "e".repeat(13)), format!("{}-y", "f".repeat(13))),
        ] {
            let l = pair_label(&format!("{a}.{owner}"), &format!("{b}.{owner}"));
            assert!(host_fits(&l, owner), "{l}");
            assert_eq!(l, pair_label(&format!("{b}.{owner}"), &format!("{a}.{owner}")), "either way round");
        }
        assert_eq!(direct_label("fred.paul"), "fred-chat");
        let d = direct_label(&format!("{}.{owner}", "z".repeat(63)));
        assert!(host_fits(&d, owner) && d.ends_with("-chat"), "{d}");
    }

    fn rec(seq: i64, principal: &str, body: Value) -> Value {
        json!({ "channel": "work", "seq": seq, "at": seq, "principal": principal, "kind": "message", "body": body })
    }

    /// Goal: the asked agent's turn of the question, how it ended, and its
    /// replies, read from the chat's records. Invalid: another agent's turn
    /// of the same record, its turn of another record, a routine's, and
    /// another turn's replies are none of it.
    #[test]
    fn the_answer_is_read_from_the_journal() {
        let start = |turn: &str, agent: &str, channel: &str, seq: i64| json!({ "kind": "turn.start", "turn": turn, "asker": "id:juniper", "agent": agent, "cause": { "fragment": "fred-juniper.paul", "channel": channel, "seq": seq }, "life": "0" });
        let work = vec![
            rec(1, "id:rowan", start("t-rowan", "id:rowan", "chat", 7)),
            rec(2, "id:fred", start("t-other", "id:fred", "chat", 6)),
            rec(3, "id:fred", start("t-routine", "id:fred", "tasks", 7)),
            rec(4, "id:fred", start("t-fred", "id:fred", "chat", 7)),
            rec(5, "id:fred", json!({ "kind": "turn.step", "turn": "t-fred", "step": 1 })),
        ];
        assert_eq!(turn_for(&work, "id:fred", 7).as_deref(), Some("t-fred"));
        assert_eq!(turn_for(&work, "id:fred", 8), None);
        assert_eq!(end_of(&work, "t-fred"), None, "running");
        let mut ended = work.clone();
        ended.push(rec(6, "id:fred", json!({ "kind": "turn.end", "turn": "t-other", "outcome": "idle" })));
        assert_eq!(end_of(&ended, "t-fred"), None, "another turn's end");
        ended.push(rec(7, "id:fred", json!({ "kind": "turn.end", "turn": "t-fred", "outcome": "error", "error": "agents in this chat started 20 turns" })));
        assert_eq!(end_of(&ended, "t-fred"), Some(("error".into(), Some("agents in this chat started 20 turns".into()))));
        let chat = vec![
            json!({ "seq": 8, "principal": "id:fred", "body": { "text": "sunny", "turn": "t-fred", "attachments": [{}] } }),
            json!({ "seq": 9, "principal": "id:fred", "body": { "text": "other", "turn": "t-other" } }),
            json!({ "seq": 10, "principal": "id:juniper", "body": { "text": "forged", "turn": "t-fred" } }),
            json!({ "seq": 11, "principal": "id:fred", "body": { "kind": "stop", "turn": "t-fred" } }),
            json!({ "seq": 12, "principal": "id:fred", "body": { "text": "and warm", "turn": "t-fred" } }),
        ];
        assert_eq!(replies_of(&chat, "id:fred", "t-fred"), vec![Reply { seq: 8, text: "sunny".into(), attachments: 1 }, Reply { seq: 12, text: "and warm".into(), attachments: 0 }]);
    }
}
