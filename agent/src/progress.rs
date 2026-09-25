//! A chat turn's progress on the chat's `work` channel (docs/api.md, the
//! chat template): `turn.start`, one record per tool call once its result
//! is stored, and `turn.end`, each built by `fragment_core::work`. Posted
//! as the agent, best-effort: a post that fails is logged and tried again
//! after the next step, and never fails the turn.
//!
//! Each record's id comes from the turn and the call's number in it, so the
//! same record is the same post. A cursor in kv (`work_posted`, the calls
//! posted so far) makes a driver that replaces a dead one post what the
//! dead one had not, and a replayed step (its tool call's number, its
//! stored result) posts nothing new.

use std::collections::HashMap;

use anyhow::anyhow;
use fragment_core::work::{self, Call, Outcome};
use fragment_proto::TurnOutcome;
use goose_provider_types::conversation::message::{Message, MessageContent};
use serde::Deserialize;
use serde_json::{json, Value};
use worker::{Method, SqlStorage};

use crate::fleet::{self, Fleet};
use crate::store::{kv_get, kv_set, kv_u64};

/// The turn's messages one read takes at most (a turn longer than the
/// conversation window ends in an error before it gets here).
const TURN_MESSAGES_MAX: i64 = 2 * fragment_core::history::WINDOW_MESSAGES_MAX as i64;

/// What a chat declares, as its agent reads it when a turn starts there.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    /// Its channel takes posts: the answer is posted there (else it goes
    /// through the listen's reply operation, as chats made before did).
    pub posts: bool,
    /// It declares a `work` channel that takes posts.
    pub work: bool,
}

/// Reads what `fragment` declares about `channel` and `work`.
pub async fn shape(fleet: &Fleet, fragment: &str, channel: &str) -> anyhow::Result<Shape> {
    #[derive(Deserialize)]
    struct Channel {
        name: String,
        post: Option<String>,
    }
    #[derive(Deserialize)]
    struct Channels {
        channels: Vec<Channel>,
    }
    let listed: Channels = fleet.get_as(&format!("/api/f/{fragment}/channels")).await?;
    let postable = |name: &str| listed.channels.iter().any(|c| c.name == name && c.post.is_some());
    Ok(Shape { posts: postable(channel), work: postable(work::WORK_CHANNEL) })
}

/// One chat turn's progress: its fragment, its turn, and the agent's own
/// fleet (the records are the agent's, whoever asked).
pub struct Progress {
    pub fleet: Fleet,
    pub fragment: String,
    pub turn: String,
    pub sql: SqlStorage,
}

impl Progress {
    /// Posts one record; answers whether it is there now.
    async fn post(&self, part: &str, body: &Value) -> bool {
        let path = format!("/api/f/{}/channels/{}", self.fragment, work::WORK_CHANNEL);
        let id = work::record_id(&self.turn, part);
        match self.fleet.call(Method::Post, &path, Some(&json!({ "id": id, "body": body }))).await {
            Ok((200, _)) => true,
            // the same id with another body: a record of this part is there
            // already (a turn's end said again, differently, after a crash)
            Ok((409, _)) => true,
            Ok((status, answer)) => {
                worker::console_warn!("progress {id} on {}: {status} {}", self.fragment, fleet::message(&answer));
                false
            }
            Err(e) => {
                worker::console_warn!("progress {id} on {}: {e:#}", self.fragment);
                false
            }
        }
    }

    pub async fn start(&self, asker: &str) {
        self.post("start", &work::start(&self.turn, asker)).await;
    }

    /// Posts, in order, the calls whose results are stored and that no
    /// driver posted yet; stops at the first that fails (the next step
    /// tries it again).
    pub async fn steps(&self) {
        let calls = match turn_calls(&self.sql) {
            Ok(c) => c,
            Err(e) => {
                worker::console_warn!("progress on {}: reading the turn: {e:#}", self.fragment);
                return;
            }
        };
        let posted = kv_u64(&self.sql, "work_posted").unwrap_or(0) as usize;
        for (i, call) in calls.iter().enumerate().skip(posted) {
            let Some(result) = &call.result else { break };
            let n = i + 1;
            if !self.post(&n.to_string(), &work::step(&self.turn, n, call, result)).await {
                break;
            }
            if let Err(e) = kv_set(&self.sql, "work_posted", n) {
                worker::console_warn!("progress on {}: {e:#}", self.fragment);
                break;
            }
        }
    }

    /// The calls not posted yet, then how the turn ended.
    pub async fn end(&self, outcome: &anyhow::Result<TurnOutcome>) {
        self.steps().await;
        let (how, error) = match outcome {
            Ok(how) => (how.as_str(), None),
            Err(e) => (TurnOutcome::Error.as_str(), Some(e.to_string())),
        };
        self.post("end", &work::end(&self.turn, how, error.as_deref())).await;
    }
}

/// The running turn's tool calls, in the order the model asked for them,
/// each with its stored result: its messages from its first (`turn_seq`)
/// in its conversation.
fn turn_calls(sql: &SqlStorage) -> anyhow::Result<Vec<Call>> {
    let conv = kv_get(sql, "turn_conv")?.unwrap_or_default();
    let from = kv_u64(sql, "turn_seq")? as i64;
    #[derive(Deserialize)]
    struct Row {
        json: String,
    }
    let rows: Vec<Row> = sql
        .exec("SELECT json FROM messages WHERE conv = ? AND seq >= ? ORDER BY seq LIMIT ?", vec![conv.into(), from.into(), TURN_MESSAGES_MAX.into()])
        .and_then(|c| c.to_array())
        .map_err(|e| anyhow!("{e}"))?;
    let mut calls: Vec<(String, Call)> = Vec::new();
    let mut results: HashMap<String, Outcome> = HashMap::new();
    for row in rows {
        let message: Message = serde_json::from_str(&row.json).map_err(|e| anyhow!("corrupt message: {e}"))?;
        let mut text = message.as_concat_text();
        for content in &message.content {
            if let Some(request) = content.as_tool_request() {
                let (name, args) = match &request.tool_call {
                    Ok(c) => (c.name.to_string(), work::short_args(c.arguments.as_ref())),
                    Err(e) => ("(a call the model got wrong)".to_string(), work::cut(&e.message, work::ARGS_MAX_CHARS)),
                };
                calls.push((request.id.clone(), Call { name, args, text: std::mem::take(&mut text), result: None }));
            } else if let Some(response) = content.as_tool_response() {
                let result = match &response.tool_result {
                    Ok(r) => Outcome { ok: r.is_error != Some(true), text: MessageContent::as_tool_response_text(content).unwrap_or_default() },
                    Err(e) => Outcome { ok: false, text: e.message.to_string() },
                };
                results.insert(response.id.clone(), result);
            }
        }
    }
    Ok(calls
        .into_iter()
        .map(|(id, mut call)| {
            call.result = results.remove(&id);
            call
        })
        .collect())
}
