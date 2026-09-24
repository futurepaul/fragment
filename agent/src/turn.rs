//! A turn is goose's state machine: load the conversation, run one step (a
//! steer, a batch of tool calls, or one model call), apply its effects in
//! SQL, repeat. A watchdog alarm stays armed while a turn runs, so a driver
//! that dies with its node is replaced and resumes from the last applied
//! step (ported from spikes/goose-agent, branch spike/goose-agent).
//!
//! wasm32 is single-threaded, so `AssertSend` and `SendFuture` only satisfy
//! goose's `Send` bounds; nothing crosses a thread.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use futures::{Stream, StreamExt};
use goose_agent::inference::InferenceRunner;
use goose_agent::machine::{SessionLoader, StateMachine, Step};
use goose_agent::operation::{applied, ends_turn, last_effective_role, messages_since_kickoff, not_applicable, Emitter, Operation, OperationResult};
use goose_agent::tool::ToolOperation;
use goose_provider_types::base::{MessageStream, Provider};
use goose_provider_types::conversation::message::Message;
use goose_provider_types::conversation::{Conversation, EffectiveRole};
use goose_provider_types::errors::ProviderError;
use goose_provider_types::formats::openai::{create_request, response_to_streaming_message};
use goose_provider_types::images::ImageFormat;
use goose_provider_types::model::ModelConfig;
use rmcp::model::Tool;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use worker::send::SendFuture;
use worker::{Delay, Fetch, Headers, Method, Request, RequestInit, SqlStorage, Storage};

use crate::fleet::Fleet;
use crate::js;
use crate::store::{self, kv_get, kv_set, kv_u64, Effect, Session, Store, SESSION_ID};
use crate::tools::FragmentTools;

/// Every loop and input is bounded.
pub const STEPS_PER_TURN_MAX: u32 = 64;
pub const WATCHDOG_MS_DEFAULT: u64 = 30_000;
pub const WATCHDOG_MS_MIN: u64 = 2_000;
const SSE_LINE_MAX: usize = 1024 * 1024;
const EVENTS_BUFFER: usize = 1024;

pub struct Model {
    /// The OpenRouter base (`OPENROUTER_API_URL`, default https://openrouter.ai).
    pub base: String,
    pub key: String,
    pub name: String,
}

pub struct Driver {
    pub storage: Storage,
    pub model: Model,
    pub fleet: Fleet,
    pub instructions: String,
    pub cancel: CancellationToken,
    pub id: String,
}

// ---------------------------------------------------------------- operations

struct Instructions(String);

#[async_trait]
impl Operation<Session, Effect> for Instructions {
    fn name(&self) -> &'static str {
        "instructions"
    }

    async fn prompt_parts(&self, _: &Session, _: &Conversation) -> anyhow::Result<Vec<(String, String)>> {
        Ok(vec![("instructions".into(), self.0.clone())])
    }
}

/// goose's steer operation (crates/goose/src/agents/state_machine/ops_steer.rs),
/// reading a durable queue instead of an in-memory one.
struct Steer {
    sql: SqlStorage,
}

#[async_trait]
impl Operation<Session, Effect> for Steer {
    fn name(&self) -> &'static str {
        "steer"
    }

    async fn run(&self, _: &Session, conversation: &Conversation, emit: &Emitter) -> anyhow::Result<OperationResult<Effect>> {
        let messages = messages_since_kickoff(conversation)?;
        let between_turns = ends_turn(messages) || last_effective_role(messages)? == EffectiveRole::Tool;
        if !between_turns {
            return not_applicable();
        }
        #[derive(Deserialize)]
        struct Row {
            seq: i64,
            text: String,
        }
        let pending: Vec<Row> = self
            .sql
            .exec("SELECT seq, text FROM steer WHERE consumed = 0 ORDER BY seq", None)
            .and_then(|c| c.to_array())
            .map_err(|e| anyhow!("{e}"))?;
        if pending.is_empty() {
            return not_applicable();
        }
        let mut effects = Vec::with_capacity(pending.len() + 1);
        let mut seqs = Vec::with_capacity(pending.len());
        for row in pending {
            let message = emit.message(Message::user().with_text(row.text).with_steer()).await;
            effects.push(Effect::Message(message));
            seqs.push(row.seq);
        }
        effects.push(Effect::ConsumeSteer(seqs));
        applied(effects)
    }
}

// ---------------------------------------------------------------- the provider

struct AssertSend<T>(T);

// SAFETY: wasm32-unknown-unknown has one thread; goose's bounds are nominal here.
unsafe impl<T> Send for AssertSend<T> {}

impl<S: Stream + Unpin> Stream for AssertSend<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<S::Item>> {
        Pin::new(&mut self.0).poll_next(cx)
    }
}

type LineStream = Pin<Box<dyn Stream<Item = anyhow::Result<String>> + Send>>;

/// Splits a byte stream into lines without breaking UTF-8 across chunks.
fn lines(bytes: AssertSend<worker::ByteStream>) -> LineStream {
    Box::pin(futures::stream::unfold((bytes, Vec::<u8>::new(), false), |(mut bytes, mut buffer, mut done)| async move {
        loop {
            if let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=end).collect();
                let line = String::from_utf8_lossy(&line[..end]).trim_end().to_string();
                return Some((Ok(line), (bytes, buffer, done)));
            }
            if done {
                if buffer.is_empty() {
                    return None;
                }
                let line = String::from_utf8_lossy(&buffer).to_string();
                buffer.clear();
                return Some((Ok(line), (bytes, buffer, done)));
            }
            if buffer.len() > SSE_LINE_MAX {
                return Some((Err(anyhow!("a stream line is over {SSE_LINE_MAX} bytes")), (bytes, Vec::new(), true)));
            }
            match bytes.next().await {
                Some(Ok(chunk)) => buffer.extend_from_slice(&chunk),
                Some(Err(error)) => return Some((Err(anyhow!("stream read: {error}")), (bytes, Vec::new(), true))),
                None => done = true,
            }
        }
    }))
}

async fn open_stream(url: String, key: String, body: String) -> Result<LineStream, ProviderError> {
    let failed = |e: worker::Error| ProviderError::RequestFailed(e.to_string());
    let headers = Headers::new();
    headers.set("authorization", &format!("Bearer {key}")).map_err(failed)?;
    headers.set("content-type", "application/json").map_err(failed)?;
    headers.set("x-title", "fragment agent").map_err(failed)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
    let request = Request::new_with_init(&url, &init).map_err(failed)?;
    let mut response = Fetch::Request(request).send().await.map_err(failed)?;
    let status = response.status_code();
    if status != 200 {
        let text = response.text().await.unwrap_or_default();
        return Err(match status {
            401 | 403 => ProviderError::Authentication(text),
            429 => ProviderError::RateLimitExceeded { details: text, retry_delay: None },
            _ => ProviderError::RequestFailed(format!("{status}: {text}")),
        });
    }
    Ok(lines(AssertSend(response.stream().map_err(failed)?)))
}

/// OpenRouter through the cell's own fetch, with goose's OpenAI-format
/// request builder and stream parser.
struct OpenRouter {
    base: String,
    key: String,
}

#[async_trait]
impl Provider for OpenRouter {
    fn get_name(&self) -> &str {
        "openrouter"
    }

    async fn stream(&self, model_config: &ModelConfig, system: &str, messages: &[Message], tools: &[Tool]) -> Result<MessageStream, ProviderError> {
        let payload = create_request(model_config, system, messages, tools, &ImageFormat::OpenAi, true)
            .map_err(|e| ProviderError::RequestFailed(format!("request: {e}")))?;
        let body = serde_json::to_string(&payload).map_err(|e| ProviderError::RequestFailed(format!("request: {e}")))?;
        let url = format!("{}/api/v1/chat/completions", self.base);
        let lines = SendFuture::new(open_stream(url, self.key.clone(), body)).await?;
        Ok(Box::pin(response_to_streaming_message(lines).map(|item| item.map_err(|e| ProviderError::RequestFailed(e.to_string())))))
    }
}

// ---------------------------------------------------------------- the driver

pub async fn arm_watchdog(storage: &Storage, sql: &SqlStorage) -> anyhow::Result<()> {
    let watchdog = match kv_u64(sql, "watchdog_ms")? {
        0 => WATCHDOG_MS_DEFAULT,
        ms => ms,
    };
    storage.set_alarm(Duration::from_millis(watchdog)).await.map_err(|e| anyhow!("{e}"))
}

async fn run_steps(driver: &Driver, machine: &StateMachine<'_, Session, Effect>, store: &Store, emit: Emitter) -> anyhow::Result<&'static str> {
    let sql = &store.sql;
    // Arm the watchdog before any work, so a node that dies during the
    // first step still leaves a wake behind.
    arm_watchdog(&driver.storage, sql).await?;
    for _ in 0..STEPS_PER_TURN_MAX {
        if kv_u64(sql, "cancel")? == 1 {
            driver.cancel.cancel();
        }
        let t0 = js::now_ms();
        let session = store.load(SESSION_ID).await?;
        let Some(mut result) = machine.step(&session, &emit).await? else {
            return Ok("idle");
        };
        machine.apply(store, &session, &mut result, &emit).await?;
        let name = result.applied_step.unwrap_or("?");
        let t1 = js::now_ms();
        store::record_step(sql, name, result.effects.len(), t1 - t0, t1, &driver.id)?;
        arm_watchdog(&driver.storage, sql).await?;
        if name == "tools" {
            // Test hook: hold after a tool result is persisted, so a kill
            // lands between steps.
            let hold = kv_u64(sql, "test_hold_after_tool_ms")?;
            if hold > 0 && !driver.cancel.is_cancelled() {
                futures::future::select(Box::pin(Delay::from(Duration::from_millis(hold))), Box::pin(driver.cancel.cancelled())).await;
            }
        }
        if result.yield_to_client {
            return Ok("yielded");
        }
    }
    Err(anyhow!("the turn exceeded {STEPS_PER_TURN_MAX} steps"))
}

/// Runs the turn to its end (or its cancellation): the outcome is "idle"
/// (the model answered), "yielded", or an error.
pub async fn drive(driver: Driver) -> anyhow::Result<&'static str> {
    let sql = driver.storage.sql();
    let store = Store { sql: driver.storage.sql() };
    let provider: Arc<dyn Provider> = Arc::new(OpenRouter { base: driver.model.base.clone(), key: driver.model.key.clone() });
    let tools = FragmentTools::new(driver.fleet.clone(), driver.storage.sql(), driver.id.clone());
    let machine: StateMachine<Session, Effect> = StateMachine::new(
        vec![
            Step::Operation(Arc::new(ToolOperation::new().with_provider(Arc::new(tools)))),
            Step::Operation(Arc::new(Steer { sql: driver.storage.sql() })),
            Step::Operation(Arc::new(Instructions(driver.instructions.clone()))),
            Step::Inference(Arc::new(InferenceRunner::new(provider, ModelConfig::new(&driver.model.name)))),
        ],
        driver.cancel.clone(),
    );
    let (tx, mut rx) = tokio::sync::mpsc::channel(EVENTS_BUFFER);
    let emit = Emitter::new(tx, driver.cancel.clone());
    let drain = async move {
        let mut events = 0u64;
        while rx.recv().await.is_some() {
            events += 1;
        }
        events
    };
    let (outcome, events) = futures::join!(run_steps(&driver, &machine, &store, emit), drain);
    kv_set(&sql, "events", kv_u64(&sql, "events")? + events)?;
    if kv_get(&sql, "cancel")?.as_deref() == Some("1") && outcome.is_ok() {
        return Ok("stopped");
    }
    outcome
}
