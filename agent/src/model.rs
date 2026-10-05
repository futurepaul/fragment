//! The platform's model route as a turn calls it (cell/src/models.rs,
//! `POST /api/models/v1/chat/completions`): each completion bounded so it
//! finishes, retried once when it does not, and never silent.
//!
//! Each call asks for at most `MAX_TOKENS` and has a deadline of its own
//! (`DEADLINE_MS`, counted to the last byte of its body), so a slow call
//! ends here, named, and streaming does not save a long answer. The whole answer is
//! read before goose sees it (nothing streams to viewers: a step is a whole
//! record), so a call that fails is made again whole:
//!
//! - one past its deadline, one that fails, or one that answers nothing (no
//!   text and no tool call; reasoning alone is nothing) is made once more,
//!   told why when that helps. A second failure is the turn's error: goose
//!   stores it, and turn.rs ends the turn with it. A refusal the platform
//!   will give again (401, 402 for the owner's ledger, 403 for a guest), or
//!   a conversation too long for the model, is not tried again.
//! - nothing twice, after tool calls that worked in the turn, is answered
//!   with what those calls did instead.
//! - a call cut off at `MAX_TOKENS` (`finish_reason: length`) is refused,
//!   saying why (goose's parse does).
//!
//! Every call is the platform's to meter: it is signed by the agent, acts
//! for the turn's asker (`for`), names the turn's fragment (for that
//! fragment's cap), and its owner's ledger pays (decision 36). The agent
//! holds no key to any model.

use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use async_trait::async_trait;
use futures::future::Either;
use futures::{Stream, StreamExt};
use goose_provider_types::base::{MessageStream, Provider};
use goose_provider_types::conversation::message::{Message, MessageContent};
use goose_provider_types::conversation::token_usage::ProviderUsage;
use goose_provider_types::errors::ProviderError;
use goose_provider_types::formats::openai::{create_request, response_to_streaming_message};
use goose_provider_types::images::ImageFormat;
use goose_provider_types::model::ModelConfig;
use rmcp::model::Tool;
use serde_json::Value;
use worker::send::SendFuture;
use worker::{AbortController, AbortSignal, Delay, Fetch, Headers, Method, Request, RequestInit};

use crate::fleet::Fleet;

/// The most one completion writes (reasoning included): at the rates the
/// platform's models write, it ends well inside `DEADLINE_MS`.
pub const MAX_TOKENS: i32 = 4096;
/// One completion's deadline: under the node's fetch timeout (120 s), so a
/// slow call ends here and is named as one.
pub const DEADLINE_MS: u64 = 100_000;
/// The shortest deadline a dev fleet's test controls may set.
pub const DEADLINE_MS_MIN: u64 = 200;
/// Each call is made at most this many times.
const ATTEMPTS: usize = 2;
const SSE_LINE_MAX: usize = 1024 * 1024;
/// The most of one answer read (4096 tokens stream as far less).
const ANSWER_BYTES_MAX: usize = 8 * 1024 * 1024;
const TIMEOUT_NUDGE: &str = "(Your last reply took longer than the time limit and was lost. Reply again, shorter.)";
const EMPTY_NUDGE: &str = "(Your last reply was empty. Reply again: call a tool, or answer the person in a sentence or two, \
     saying what you did and where it is.)";

type Item = (Option<Message>, Option<ProviderUsage>);

pub struct AssertSend<T>(pub T);

// SAFETY: wasm32-unknown-unknown has one thread; goose's bounds are nominal here.
unsafe impl<T> Send for AssertSend<T> {}

impl<S: Stream + Unpin> Stream for AssertSend<S> {
    type Item = S::Item;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<S::Item>> {
        Pin::new(&mut self.0).poll_next(cx)
    }
}

/// Aborts its fetch when dropped (a deadline, or a stop), unless disarmed.
struct Abort(Option<AbortController>);

impl Drop for Abort {
    fn drop(&mut self) {
        if let Some(controller) = self.0.take() {
            controller.abort();
        }
    }
}

/// The platform's model route through the agent's own signed fetch, with
/// goose's OpenAI-format request builder and stream parser.
pub struct ModelRoute {
    /// The agent's, acting for the turn's asker (`for`).
    pub fleet: Fleet,
    /// The turn's fragment (its chat's, or a fragment's own agent's), whose
    /// cap the call is under; none for its owner's own conversation.
    pub fragment: Option<String>,
    /// One call's deadline (`DEADLINE_MS`, or a test control's).
    pub deadline_ms: u64,
}

/// The model config a turn uses: goose's, with the output bound. Its name
/// is the agent's tier, which the route maps to a model.
pub fn config(tier: &str) -> ModelConfig {
    ModelConfig::new(tier).with_max_tokens(Some(MAX_TOKENS))
}

#[async_trait]
impl Provider for ModelRoute {
    fn get_name(&self) -> &str {
        "fragment"
    }

    async fn stream(&self, model_config: &ModelConfig, system: &str, messages: &[Message], tools: &[Tool]) -> Result<MessageStream, ProviderError> {
        let path = match &self.fragment {
            Some(f) => format!("{ROUTE}?fragment={f}"),
            None => ROUTE.to_string(),
        };
        let call = Call {
            fleet: self.fleet.clone(),
            path,
            deadline_ms: self.deadline_ms,
            config: model_config.clone(),
            system: system.to_string(),
            messages: messages.to_vec(),
            tools: tools.to_vec(),
        };
        // the whole answer is read in the stream's first poll, so a stop
        // (goose selects on it against this stream) still cuts it short
        let items = futures::stream::once(SendFuture::new(call.complete())).flat_map(|done| {
            let items: Vec<Result<Item, ProviderError>> = match done {
                Ok(items) => items.into_iter().map(Ok).collect(),
                Err(failed) => failed.spent.map(|u| Ok((None, Some(u)))).into_iter().chain([Err(failed.error)]).collect(),
            };
            futures::stream::iter(items)
        });
        Ok(Box::pin(items))
    }
}

/// The platform's model route.
const ROUTE: &str = "/api/models/v1/chat/completions";

/// One completion, with all it needs to be made again.
struct Call {
    /// The agent's, acting for the turn's asker: each attempt is signed
    /// for its own body.
    fleet: Fleet,
    /// The route and the turn's fragment (`fragment=`).
    path: String,
    deadline_ms: u64,
    config: ModelConfig,
    system: String,
    messages: Vec<Message>,
    tools: Vec<Tool>,
}

/// A call that failed: why, and what its attempts cost.
struct Failure {
    spent: Option<ProviderUsage>,
    error: ProviderError,
}

/// How one attempt went.
enum Attempt {
    /// Text or tool calls (cut ones repaired or refused).
    Answered(Vec<Item>),
    /// No text and no tool call; what it cost.
    Empty(Option<ProviderUsage>),
    TimedOut,
    Failed(ProviderError),
}

/// Usage summed across attempts (each is billed).
fn add(spent: Option<ProviderUsage>, more: Option<ProviderUsage>) -> Option<ProviderUsage> {
    match (spent, more) {
        (Some(mut a), Some(b)) => {
            a.usage += b.usage;
            a.cost = match (a.cost, b.cost) {
                (Some(x), Some(y)) => Some(x + y),
                (x, y) => x.or(y),
            };
            Some(a)
        }
        (a, b) => a.or(b),
    }
}

/// A refusal that another try would get again.
fn retryable(error: &ProviderError) -> bool {
    !matches!(error, ProviderError::Authentication(_) | ProviderError::CreditsExhausted { .. } | ProviderError::ContextLengthExceeded(_))
}

impl Call {
    /// The answer, or why it failed (and what the attempts cost).
    async fn complete(self) -> Result<Vec<Item>, Box<Failure>> {
        let mut nudge = None;
        let mut spent = None;
        let mut why = String::new();
        let mut empty = false;
        for _ in 0..ATTEMPTS {
            empty = false;
            match self.attempt(nudge).await {
                Attempt::Answered(mut items) => {
                    if spent.is_some() {
                        // this attempt's usage, and the ones before it, as one
                        let last = items.iter().rposition(|(_, u)| u.is_some());
                        match last {
                            Some(i) => items[i].1 = add(spent.take(), items[i].1.take()),
                            None => items.push((None, spent.take())),
                        }
                    }
                    return Ok(items);
                }
                Attempt::Empty(usage) => {
                    spent = add(spent, usage);
                    why = "the model answered with nothing".into();
                    nudge = Some(EMPTY_NUDGE);
                    empty = true;
                }
                Attempt::TimedOut => {
                    why = format!("the model timed out (no whole answer in {} s)", self.deadline_ms / 1000);
                    nudge = Some(TIMEOUT_NUDGE);
                }
                Attempt::Failed(error) if !retryable(&error) => return Err(Box::new(Failure { spent, error })),
                Attempt::Failed(error) => {
                    why = format!("the model service failed ({error})");
                    nudge = None;
                }
            }
        }
        if empty {
            if let Some(summary) = summary(&self.messages) {
                return Ok(vec![(Some(Message::assistant().with_text(summary)), spent)]);
            }
        }
        Err(Box::new(Failure { spent, error: ProviderError::RequestFailed(format!("{why}, twice")) }))
    }

    async fn attempt(&self, nudge: Option<&str>) -> Attempt {
        let mut messages = self.messages.clone();
        if let Some(nudge) = nudge {
            messages.push(Message::user().with_text(nudge));
        }
        let body = match create_request(&self.config, &self.system, &messages, &self.tools, &ImageFormat::OpenAi, true)
            .map_err(|e| e.to_string())
            .and_then(|payload| serde_json::to_string(&payload).map_err(|e| e.to_string()))
        {
            Ok(body) => body,
            Err(e) => return Attempt::Failed(ProviderError::RequestFailed(format!("request: {e}"))),
        };
        let (url, auth) = match self.fleet.signed("POST", &self.path, body.as_bytes()).await {
            Ok(signed) => signed,
            Err(e) => return Attempt::Failed(ProviderError::RequestFailed(format!("signing the call: {e}"))),
        };
        let mut abort = Abort(Some(AbortController::default()));
        let signal = abort.0.as_ref().expect("armed").signal();
        let read = read_answer(&url, &auth, body, &signal);
        let deadline = Delay::from(Duration::from_millis(self.deadline_ms));
        let lines = match futures::future::select(Box::pin(read), Box::pin(deadline)).await {
            Either::Left((Ok(lines), _)) => lines,
            Either::Left((Err(error), _)) => return Attempt::Failed(error),
            // the fetch is aborted as `abort` drops
            Either::Right(_) => return Attempt::TimedOut,
        };
        // read whole: nothing to abort
        abort.0 = None;
        parse(lines).await
    }
}

/// A refusal's text: the platform's message when it answered an
/// `ErrorBody`, else the answer as it came.
fn refusal_text(text: String) -> String {
    match serde_json::from_str::<fragment_proto::ErrorBody>(&text) {
        Ok(refusal) => refusal.message,
        Err(_) => text,
    }
}

/// The answer's lines (server-sent events), read whole.
async fn read_answer(url: &str, auth: &str, body: String, signal: &AbortSignal) -> Result<Vec<String>, ProviderError> {
    let failed = |e: worker::Error| ProviderError::RequestFailed(e.to_string());
    let headers = Headers::new();
    headers.set("authorization", auth).map_err(failed)?;
    headers.set("content-type", "application/json").map_err(failed)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
    let request = Request::new_with_init(url, &init).map_err(failed)?;
    let mut response = Fetch::Request(request).send_with_signal(signal).await.map_err(|e| ProviderError::NetworkError(e.to_string()))?;
    let status = response.status_code();
    if status != 200 {
        let text = refusal_text(response.text().await.unwrap_or_default());
        return Err(match status {

            401 | 403 => ProviderError::Authentication(text),
            402 => ProviderError::CreditsExhausted { details: text, top_up_url: None },
            429 => ProviderError::RateLimitExceeded { details: text, retry_delay: None },
            _ => ProviderError::RequestFailed(format!("{status}: {text}")),
        });
    }
    let mut bytes = AssertSend(response.stream().map_err(failed)?);
    let mut lines = Vec::new();
    let mut buffer: Vec<u8> = Vec::new();
    let mut total = 0usize;
    // bounded: ANSWER_BYTES_MAX bytes, then the read stops
    loop {
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            lines.push(String::from_utf8_lossy(&line[..end]).trim_end().to_string());
        }
        if buffer.len() > SSE_LINE_MAX {
            return Err(ProviderError::RequestFailed(format!("a stream line is over {SSE_LINE_MAX} bytes")));
        }
        match bytes.next().await {
            Some(Ok(chunk)) => {
                total += chunk.len();
                if total > ANSWER_BYTES_MAX {
                    return Err(ProviderError::RequestFailed(format!("the answer is over {ANSWER_BYTES_MAX} bytes")));
                }
                buffer.extend_from_slice(&chunk);
            }
            Some(Err(error)) => return Err(ProviderError::NetworkError(format!("stream read: {error}"))),
            None => break,
        }
    }
    if !buffer.is_empty() {
        lines.push(String::from_utf8_lossy(&buffer).trim_end().to_string());
    }
    Ok(lines)
}

/// goose's parse of the answer (which refuses a call cut off at the output
/// limit, saying so), then what this module adds: nothing is `Empty`.
async fn parse(lines: Vec<String>) -> Attempt {
    let parsed: Vec<anyhow::Result<Item>> = response_to_streaming_message(futures::stream::iter(lines.into_iter().map(Ok))).collect().await;
    let mut items = Vec::with_capacity(parsed.len());
    for item in parsed {
        match item {
            Ok(item) => items.push(item),
            Err(error) => return Attempt::Failed(ProviderError::RequestFailed(format!("the answer: {error}"))),
        }
    }
    let text: String = items.iter().filter_map(|(m, _)| m.as_ref()).map(Message::as_concat_text).collect();
    let calls = items.iter().filter_map(|(m, _)| m.as_ref()).any(Message::is_tool_call);
    if text.trim().is_empty() && !calls {
        let usage = items.into_iter().filter_map(|(_, u)| u).next_back();
        return Attempt::Empty(usage);
    }
    Attempt::Answered(items)
}

/// What the turn's tool calls did, said for them when the model, after
/// they worked, twice answered with nothing. `None` when none worked.
fn summary(messages: &[Message]) -> Option<String> {
    let start = messages.iter().rposition(|m| m.role == rmcp::model::Role::User && !m.is_tool_response() && !m.as_concat_text().trim().is_empty())?;
    let turn = &messages[start..];
    let mut calls: HashMap<&str, &str> = HashMap::new();
    for request in turn.iter().flat_map(|m| m.content.iter()).filter_map(MessageContent::as_tool_request) {
        if let Ok(call) = &request.tool_call {
            calls.insert(request.id.as_str(), call.name.as_ref());
        }
    }
    let mut did: Vec<String> = Vec::new();
    for content in turn.iter().flat_map(|m| m.content.iter()) {
        let Some(response) = content.as_tool_response() else { continue };
        let worked = response.tool_result.as_ref().is_ok_and(|r| r.is_error != Some(true));
        let Some(name) = calls.get(response.id.as_str()) else { continue };
        if !worked {
            continue;
        }
        let answer: Value = MessageContent::as_tool_response_text(content).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let line = match *name {
            "platform__create_fragment" => format!("made {}", answer["name"].as_str().unwrap_or("a fragment")),
            other => format!("ran {other}"),
        };
        if !did.contains(&line) {
            did.push(line);
        }
    }
    if did.is_empty() {
        return None;
    }
    Some(format!("Done. Here is what I did: {}.", did.join("; ")))
}

/// A failed turn's reason, as the person reads it: the text of goose's
/// error message without goose's wrapping ("Ran into this error: Request
/// failed: …. Please retry …").
pub fn reason(error_text: &str) -> String {
    let text = error_text.split("\n\n").next().unwrap_or(error_text);
    let text = text.strip_prefix("Ran into this error: ").unwrap_or(text);
    let text = text.strip_prefix("Request failed: ").unwrap_or(text);
    text.strip_suffix('.').unwrap_or(text).trim().to_string()
}

