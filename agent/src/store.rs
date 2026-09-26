//! The agent's SQL: its settings, its conversations (the replay ledger),
//! the steer queue, the turns waiting, and what each step did. goose
//! reloads the conversation before every step (a bounded window of it,
//! fragment_core::history) and hands every result back here to persist.
//!
//! An agent keeps one conversation per chat (a followed channel,
//! `<fragment>/<channel>`) and one with its owner (`DIRECT`): a turn
//! belongs to one of them, reads only it, and answers there. One turn runs
//! at a time; a message for another conversation, or from someone other
//! than the running turn's starter, waits in `pending` for its own turn.

use anyhow::anyhow;
use async_trait::async_trait;
use fragment_core::history;
use goose_agent::inference::InferenceEffect;
use goose_agent::machine::{EffectHandler, MachineSession, SessionLoader};
use goose_agent::operation::{Emitter, MachineEffect};
use goose_provider_types::conversation::message::{Message, MessageContent};
use goose_provider_types::conversation::token_usage::ProviderUsage;
use goose_provider_types::conversation::Conversation;
use rmcp::model::ContentBlock;
use serde::Deserialize;
use serde_json::Value;
use worker::SqlStorage;

/// The owner's own conversation with the agent (`POST /api/a/{name}/turns`).
/// A chat's is `<fragment>/<channel>`, which always holds a `/`.
pub const DIRECT: &str = "direct";

/// A chat's conversation: the whole chat's, or, for a fragment's own agent,
/// one per person in it (`asker`), so strangers never share one.
pub fn conv_of(fragment: &str, channel: &str, asker: Option<&str>) -> String {
    match asker {
        Some(asker) => format!("{fragment}/{channel}/{asker}"),
        None => format!("{fragment}/{channel}"),
    }
}

/// A chat conversation's fragment and channel; `None` for `DIRECT`.
pub fn chat_of(conv: &str) -> Option<(&str, &str)> {
    let (fragment, rest) = conv.split_once('/')?;
    Some((fragment, rest.split_once('/').map_or(rest, |(channel, _)| channel)))
}

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS messages (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL UNIQUE, json TEXT NOT NULL,
  conv TEXT NOT NULL DEFAULT 'direct');
CREATE TABLE IF NOT EXISTS steer (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, consumed INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS pending (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, conv TEXT NOT NULL, asker TEXT NOT NULL, text TEXT NOT NULL,
  at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS convs (
  conv TEXT PRIMARY KEY, outcome TEXT NOT NULL, error TEXT NOT NULL, asker TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS tool_runs (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, tool_call_id TEXT NOT NULL, tool TEXT NOT NULL,
  at INTEGER NOT NULL, driver TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS listens (
  token TEXT PRIMARY KEY, fragment TEXT NOT NULL, channel TEXT NOT NULL, reply TEXT NOT NULL,
  sub INTEGER NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS heard (key TEXT PRIMARY KEY, at INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS heard_at ON heard (at);
CREATE TABLE IF NOT EXISTS ignored (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, fragment TEXT NOT NULL, channel TEXT NOT NULL,
  principal TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS steps (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, step TEXT NOT NULL, effects INTEGER NOT NULL,
  ms INTEGER NOT NULL, at INTEGER NOT NULL, driver TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS tunnel (
  rid TEXT PRIMARY KEY, method TEXT NOT NULL, path TEXT NOT NULL, body TEXT,
  created_at INTEGER NOT NULL, sent_at INTEGER, status INTEGER, answer TEXT);
CREATE TABLE IF NOT EXISTS shots (
  seq INTEGER PRIMARY KEY AUTOINCREMENT, mime TEXT NOT NULL, data TEXT NOT NULL, at INTEGER NOT NULL);
";

/// Brings an agent made before conversations were kept apart up to the
/// schema: its one conversation becomes its owner's (`DIRECT`).
pub fn migrate(sql: &SqlStorage) -> anyhow::Result<()> {
    let cols: Vec<Value> = ah(ah(sql.exec("PRAGMA table_info(messages)", None))?.to_array())?;
    if !cols.iter().any(|c| c["name"] == "conv") {
        ah(sql.exec("ALTER TABLE messages ADD COLUMN conv TEXT NOT NULL DEFAULT 'direct'", None))?;
    }
    ah(sql.exec("CREATE INDEX IF NOT EXISTS messages_conv ON messages (conv, seq)", None))?;
    Ok(())
}

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

/// The newest `limit` messages of `conv`, oldest first, each with its
/// stored size.
fn recent_rows(sql: &SqlStorage, conv: &str, limit: usize) -> anyhow::Result<Vec<(Message, usize)>> {
    #[derive(Deserialize)]
    struct Row {
        json: String,
    }
    let rows: Vec<Row> =
        ah(ah(sql.exec("SELECT json FROM messages WHERE conv = ? ORDER BY seq DESC LIMIT ?", vec![conv.into(), (limit as i64).into()]))?.to_array())?;
    assert!(rows.len() <= limit, "the query's limit holds");
    let mut out = Vec::with_capacity(rows.len());
    for row in rows.iter().rev() {
        let message: Message = serde_json::from_str(&row.json).map_err(|e| anyhow!("corrupt message: {e}"))?;
        out.push((message, row.json.len()));
    }
    Ok(out)
}

/// The newest `limit` messages of every conversation, oldest first, each
/// with its conversation (the owner's view).
pub fn recent_messages(sql: &SqlStorage, limit: usize) -> anyhow::Result<Vec<(Message, String)>> {
    #[derive(Deserialize)]
    struct Row {
        json: String,
        conv: String,
    }
    let rows: Vec<Row> = ah(ah(sql.exec("SELECT json, conv FROM messages ORDER BY seq DESC LIMIT ?", vec![(limit as i64).into()]))?.to_array())?;
    assert!(rows.len() <= limit, "the query's limit holds");
    rows.into_iter()
        .rev()
        .map(|row| Ok((serde_json::from_str(&row.json).map_err(|e| anyhow!("corrupt message: {e}"))?, row.conv)))
        .collect()
}

/// A turn's answer: its conversation's newest message, when that is the
/// model's text. Read alone (reading the whole history for it grew with
/// the agent's age).
pub fn last_answer(sql: &SqlStorage, conv: &str) -> anyhow::Result<Option<Message>> {
    let newest = recent_rows(sql, conv, 1)?.pop().map(|(message, _)| message);
    Ok(newest.filter(|m| m.role == rmcp::model::Role::Assistant && !m.as_concat_text().trim().is_empty()))
}

/// The conversation a step sees: `conv`'s newest `messages_max` messages,
/// cut to start at a kickoff, with the running turn whole and earlier
/// turns while they fit (fragment_core::history). A running turn longer
/// than the window fails its step; the next message starts a turn that
/// fits.
pub fn load_window(sql: &SqlStorage, conv: &str, messages_max: usize) -> anyhow::Result<Vec<Message>> {
    let mut rows = recent_rows(sql, conv, messages_max)?;
    let shape: Vec<history::Row> = rows.iter().map(|(message, bytes)| history::Row { kickoff: is_kickoff(message), bytes: *bytes }).collect();
    let start = history::window_start(&shape, history::WINDOW_EARLIER_BYTES_MAX)
        .ok_or_else(|| anyhow!("the running turn outgrew the conversation window ({messages_max} messages); send a new message to start a turn"))?;
    let mut window: Vec<Message> = rows.drain(start..).map(|(message, _)| message).collect();
    assert!(window.first().is_some_and(is_kickoff), "the window starts at a kickoff");
    cut_earlier_results(&mut window);
    Ok(window)
}

/// The tool results of the turns before the running one (which starts at
/// its last kickoff that is not a steer), cut to a prefix and a note
/// (`history::cut_earlier_result`), their images to a note: old output
/// cannot grow a request past the model call's deadline.
fn cut_earlier_results(window: &mut [Message]) {
    let Some(running) = window.iter().rposition(|m| is_kickoff(m) && !m.metadata.steer) else { return };
    let earlier = window[..running].iter_mut().flat_map(|m| m.content.iter_mut());
    for result in earlier.filter_map(|c| match c {
        MessageContent::ToolResponse(r) => r.tool_result.as_mut().ok(),
        _ => None,
    }) {
        for block in result.content.iter_mut() {
            match block {
                ContentBlock::Text(t) => {
                    if let Some(cut) = history::cut_earlier_result(&t.text) {
                        t.text = cut;
                    }
                }
                ContentBlock::Image(_) => *block = ContentBlock::text("[an image from an earlier turn, left out]"),
                _ => {}
            }
        }
    }
}

/// How many messages a step loads: the product's window, or a smaller one
/// a dev fleet's test controls set.
pub fn window_messages(sql: &SqlStorage) -> anyhow::Result<usize> {
    Ok(match kv_u64(sql, "test_window_messages")? {
        0 => history::WINDOW_MESSAGES_MAX,
        n => (n as usize).min(history::WINDOW_MESSAGES_MAX),
    })
}

/// Appends a message to `conv` once: the same message applied again (a
/// replayed step) is a no-op, but one id naming two different messages is
/// refused, not dropped (a provider reusing ids would otherwise loop the
/// turn).
pub fn append_message(sql: &SqlStorage, conv: &str, message: &Message) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Row {
        json: String,
    }
    let id = message.id.clone().ok_or_else(|| anyhow!("a message reached storage without an id"))?;
    let json = serde_json::to_string(message)?;
    ah(sql.exec("INSERT OR IGNORE INTO messages (id, json, conv) VALUES (?, ?, ?)", vec![id.as_str().into(), json.as_str().into(), conv.into()]))?;
    let stored: Vec<Row> = ah(ah(sql.exec("SELECT json FROM messages WHERE id = ?", vec![id.as_str().into()]))?.to_array())?;
    if stored.first().is_some_and(|r| r.json != json) {
        return Err(anyhow!("message id {id} names two different messages (does the model service reuse response ids?)"));
    }
    Ok(())
}

/// Where a stored message is in the table (a turn's first message marks
/// where its progress is read from: progress.rs).
pub fn message_seq(sql: &SqlStorage, id: &str) -> anyhow::Result<i64> {
    #[derive(Deserialize)]
    struct Row {
        seq: i64,
    }
    let rows: Vec<Row> = ah(ah(sql.exec("SELECT seq FROM messages WHERE id = ?", vec![id.into()]))?.to_array())?;
    rows.first().map(|r| r.seq).ok_or_else(|| anyhow!("message {id} is not stored"))
}

/// How many messages the running turn has stored (from its first,
/// `turn_seq`): the same when a step is made again after a crash.
pub fn turn_length(sql: &SqlStorage) -> anyhow::Result<u64> {
    #[derive(Deserialize)]
    struct Row {
        n: i64,
    }
    let (conv, from) = (kv_get(sql, "turn_conv")?.unwrap_or_default(), kv_u64(sql, "turn_seq")? as i64);
    let rows: Vec<Row> = ah(ah(sql.exec("SELECT COUNT(*) AS n FROM messages WHERE conv = ? AND seq >= ?", vec![conv.into(), from.into()]))?.to_array())?;
    Ok(rows.first().map_or(0, |r| r.n.max(0) as u64))
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

/// A session is one conversation: its id is the conversation's (`DIRECT`,
/// or a chat's `<fragment>/<channel>`).
#[async_trait]
impl SessionLoader<Session> for Store {
    async fn load(&self, conv: &str) -> anyhow::Result<Session> {
        let window = load_window(&self.sql, conv, window_messages(&self.sql)?)?;
        Ok(Session { id: conv.to_string(), conversation: Conversation::new_unvalidated(window) })
    }
}

#[async_trait]
impl EffectHandler<Session, Effect> for Store {
    // No await inside: the writes of one step land together.
    async fn apply_effects(&self, session: &Session, effects: &mut [Effect], _: &Emitter) -> anyhow::Result<()> {
        for effect in effects.iter() {
            match effect {
                Effect::Message(message) => append_message(&self.sql, &session.id, message)?,
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
