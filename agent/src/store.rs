//! The agent's SQL: its settings, its conversation (the replay ledger), the
//! steer queue, and what each step did. goose reloads the conversation
//! before every step (a bounded window of it, fragment_core::history) and
//! hands every result back here to persist.

use anyhow::anyhow;
use async_trait::async_trait;
use fragment_core::history;
use goose_agent::inference::InferenceEffect;
use goose_agent::machine::{EffectHandler, MachineSession, SessionLoader};
use goose_agent::operation::{Emitter, MachineEffect};
use goose_provider_types::conversation::message::Message;
use goose_provider_types::conversation::token_usage::ProviderUsage;
use goose_provider_types::conversation::Conversation;
use serde::Deserialize;
use worker::SqlStorage;

pub const SESSION_ID: &str = "main";

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS messages (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS steer (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, consumed INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS tool_runs (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, tool_call_id TEXT NOT NULL, tool TEXT NOT NULL,
  at INTEGER NOT NULL, driver TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS listens (
  token TEXT PRIMARY KEY, fragment TEXT NOT NULL, channel TEXT NOT NULL, reply TEXT NOT NULL,
  sub INTEGER NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS heard (key TEXT PRIMARY KEY, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS steps (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, step TEXT NOT NULL, effects INTEGER NOT NULL,
  ms INTEGER NOT NULL, at INTEGER NOT NULL, driver TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tunnel (
  rid TEXT PRIMARY KEY, method TEXT NOT NULL, path TEXT NOT NULL, body TEXT,
  created_at INTEGER NOT NULL, sent_at INTEGER, status INTEGER, answer TEXT);
CREATE TABLE IF NOT EXISTS shots (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, mime TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
";

fn ah<T>(result: worker::Result<T>) -> anyhow::Result<T> {
    result.map_err(|error| anyhow!("{error}"))
}

pub fn kv_get(sql: &SqlStorage, key: &str) -> anyhow::Result<Option<String>> {
    #[derive(Deserialize)]
    struct Row {
        v: String,
    }
    let rows: Vec<Row> = ah(ah(sql.exec("SELECT v FROM kv WHERE k = ?", vec![key.into()]))?.to_array())?;
    Ok(rows.into_iter().next().map(|row| row.v))
}

pub fn kv_u64(sql: &SqlStorage, key: &str) -> anyhow::Result<u64> {
    match kv_get(sql, key)? {
        None => Ok(0),
        Some(value) => value.parse().map_err(|_| anyhow!("corrupt state: kv {key} = {value:?} is not an integer")),
    }
}

pub fn kv_set(sql: &SqlStorage, key: &str, value: impl ToString) -> anyhow::Result<()> {
    ah(sql.exec(
        "INSERT INTO kv (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v",
        vec![key.into(), value.to_string().into()],
    ))?;
    Ok(())
}

/// A message that starts a turn's span, as goose reads one
/// (`messages_since_kickoff`): from the user, visible to them, and not a
/// tool result. A steer is one too.
pub fn is_kickoff(message: &Message) -> bool {
    message.role == rmcp::model::Role::User && message.is_user_visible() && !message.is_tool_response()
}

/// The newest `limit` messages, oldest first, each with its stored size.
fn recent_rows(sql: &SqlStorage, limit: usize) -> anyhow::Result<Vec<(Message, usize)>> {
    #[derive(Deserialize)]
    struct Row {
        json: String,
    }
    let rows: Vec<Row> = ah(ah(sql.exec("SELECT json FROM messages ORDER BY seq DESC LIMIT ?", vec![(limit as i64).into()]))?.to_array())?;
    assert!(rows.len() <= limit, "the query's limit holds");
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter().rev() {
        let message: Message = serde_json::from_str(&row.json).map_err(|e| anyhow!("corrupt message: {e}"))?;
        out.push((message, row.json.len()));
    }
    Ok(out)
}

/// The newest `limit` messages, oldest first (the owner's view).
pub fn recent_messages(sql: &SqlStorage, limit: usize) -> anyhow::Result<Vec<Message>> {
    Ok(recent_rows(sql, limit)?.into_iter().map(|(message, _)| message).collect())
}

/// The newest message (a turn's answer, once the turn is idle).
pub fn last_message(sql: &SqlStorage) -> anyhow::Result<Option<Message>> {
    Ok(recent_rows(sql, 1)?.pop().map(|(message, _)| message))
}

/// The conversation a step sees: the newest `messages_max` messages, cut
/// to start at a kickoff, with the running turn whole and earlier turns
/// while they fit (fragment_core::history). A running turn longer than
/// the window fails its step; the next message starts a turn that fits.
pub fn load_window(sql: &SqlStorage, messages_max: usize) -> anyhow::Result<Vec<Message>> {
    let mut rows = recent_rows(sql, messages_max)?;
    let shape: Vec<history::Row> = rows.iter().map(|(message, bytes)| history::Row { kickoff: is_kickoff(message), bytes: *bytes }).collect();
    let start = history::window_start(&shape, history::WINDOW_EARLIER_BYTES_MAX)
        .ok_or_else(|| anyhow!("the running turn outgrew the conversation window ({messages_max} messages); send a new message to start a turn"))?;
    let window: Vec<Message> = rows.drain(start..).map(|(message, _)| message).collect();
    assert!(window.first().is_some_and(is_kickoff), "the window starts at a kickoff");
    Ok(window)
}

/// How many messages a step loads: the product's window, or a smaller one
/// a dev fleet's test controls set.
pub fn window_messages(sql: &SqlStorage) -> anyhow::Result<usize> {
    Ok(match kv_u64(sql, "test_window_messages")? {
        0 => history::WINDOW_MESSAGES_MAX,
        n => (n as usize).min(history::WINDOW_MESSAGES_MAX),
    })
}

/// Appends a message once: the same message applied again (a replayed
/// step) is a no-op, but one id naming two different messages is refused,
/// not dropped (a provider reusing ids would otherwise loop the turn).
pub fn append_message(sql: &SqlStorage, message: &Message) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Row {
        json: String,
    }
    let id = message.id.clone().ok_or_else(|| anyhow!("a message reached storage without an id"))?;
    let json = serde_json::to_string(message)?;
    ah(sql.exec("INSERT OR IGNORE INTO messages (id, json) VALUES (?, ?)", vec![id.as_str().into(), json.as_str().into()]))?;
    let stored: Vec<Row> = ah(ah(sql.exec("SELECT json FROM messages WHERE id = ?", vec![id.as_str().into()]))?.to_array())?;
    if stored.first().is_some_and(|r| r.json != json) {
        return Err(anyhow!("message id {id} names two different messages (does the model service reuse response ids?)"));
    }
    Ok(())
}

pub fn steer(sql: &SqlStorage, text: &str) -> anyhow::Result<()> {
    ah(sql.exec("INSERT INTO steer (text) VALUES (?)", vec![text.into()]))?;
    Ok(())
}

pub fn record_step(sql: &SqlStorage, step: &str, effects: usize, ms: u64, at: u64, driver: &str) -> anyhow::Result<()> {
    ah(sql.exec(
        "INSERT INTO steps (step, effects, ms, at, driver) VALUES (?, ?, ?, ?, ?)",
        vec![step.into(), (effects as i64).into(), (ms as i64).into(), (at as i64).into(), driver.into()],
    ))?;
    Ok(())
}

#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub conversation: Conversation,
}

impl MachineSession for Session {
    fn id(&self) -> &str {
        &self.id
    }
    fn conversation(&self) -> Option<&Conversation> {
        Some(&self.conversation)
    }
}

pub enum Effect {
    Message(Message),
    Usage(ProviderUsage),
    ConsumeSteer(Vec<i64>),
}

impl From<Message> for Effect {
    fn from(message: Message) -> Self {
        Effect::Message(message)
    }
}

impl InferenceEffect for Effect {
    fn record_usage(usage: ProviderUsage) -> Self {
        Effect::Usage(usage)
    }
}

impl MachineEffect for Effect {
    fn ensure_message_ids(&mut self) {
        if let Effect::Message(message) = self {
            if message.id.is_none() {
                message.id = Some(format!("msg_{}", uuid::Uuid::new_v4()));
            }
        }
    }
}

pub struct Store {
    pub sql: SqlStorage,
}

#[async_trait]
impl SessionLoader<Session> for Store {
    async fn load(&self, session_id: &str) -> anyhow::Result<Session> {
        let window = load_window(&self.sql, window_messages(&self.sql)?)?;
        Ok(Session { id: session_id.to_string(), conversation: Conversation::new_unvalidated(window) })
    }
}

#[async_trait]
impl EffectHandler<Session, Effect> for Store {
    // No await inside: the writes of one step land together.
    async fn apply_effects(&self, _: &Session, effects: &mut [Effect], _: &Emitter) -> anyhow::Result<()> {
        for effect in effects.iter() {
            match effect {
                Effect::Message(message) => append_message(&self.sql, message)?,
                Effect::Usage(usage) => {
                    let input = usage.usage.input_tokens.unwrap_or(0).max(0) as u64;
                    let output = usage.usage.output_tokens.unwrap_or(0).max(0) as u64;
                    kv_set(&self.sql, "tokens_in", kv_u64(&self.sql, "tokens_in")? + input)?;
                    kv_set(&self.sql, "tokens_out", kv_u64(&self.sql, "tokens_out")? + output)?;
                }
                Effect::ConsumeSteer(seqs) => {
                    for seq in seqs {
                        ah(self.sql.exec("UPDATE steer SET consumed = 1 WHERE seq = ?", vec![(*seq).into()]))?;
                    }
                }
            }
        }
        Ok(())
    }
}
