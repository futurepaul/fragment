//! The model service (OpenRouter) as a turn calls it: each completion
//! bounded so it finishes, retried once when it does not, and never silent.
//!
//! The node ends any fetch after `CELLD_FETCH_TIMEOUT_S` (120 s), counted to
//! the last byte of its body (reqwest's request timeout), so streaming does
//! not save a long answer: a completion must end within it. Each call asks
//! for at most `MAX_TOKENS` and has a deadline of its own (`DEADLINE_MS`,
//! under the node's, so a slow call ends here, named). The whole answer is
//! read before goose sees it (nothing streams to viewers: a step is a whole
//! record), so a call that fails is made again whole:
//!
//! - one past its deadline, one that fails, or one that answers nothing (no
//!   text and no tool call; reasoning alone is nothing) is made once more,
//!   told why when that helps. A second failure is the turn's error: goose
//!   stores it, and turn.rs ends the turn with it. A key or a budget the
//!   service refuses (401, 402, 403), or a conversation too long for it, is
//!   not tried again.
//! - nothing twice, after tool calls that worked in the turn, is answered
//!   with what those calls did instead.
//! - a call cut off at `MAX_TOKENS` (`finish_reason: length`) keeps what it
//!   wrote: a cut `platform__write_file` or `platform__append_file` runs with
//!   the text written so far, marked `cut_off`, and its answer says where
//!   the file stops (tools.rs), so a long file lands in pieces. Any other cut
//!   call is refused, saying why.

use std::collections::{BTreeMap, HashMap};
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
use rmcp::model::{CallToolRequestParams, ErrorData, Tool};
use serde_json::{json, Map, Value};
use worker::send::SendFuture;
use worker::{AbortController, AbortSignal, Delay, Fetch, Headers, Method, Request, RequestInit};

/// The most one completion writes (reasoning included): at the rates the
/// platform's models write, it ends well inside `DEADLINE_MS`. A file of
/// about 12 KB fits in one call; the guide asks for 6 KB (lib.rs).
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
/// A cut call's arguments are closed by dropping at most this many of their
/// last characters (an escape, a key, a comma cut in half).
const REPAIR_DROPS_MAX: usize = 64;
/// The tools a cut call still runs for: what they wrote is text, and what
/// was written so far is worth keeping.
pub const WRITE_FILE: &str = "platform__write_file";
pub const APPEND_FILE: &str = "platform__append_file";
/// The mark a cut call runs with (tools.rs reads it).
pub const CUT_OFF: &str = "cut_off";

const TIMEOUT_NUDGE: &str = "(Your last reply took longer than the time limit and was lost. Reply again with less in \
     it: one file per call, under 150 lines, the rest of a long file in more calls to platform__append_file.)";
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

/// OpenRouter through the cell's own fetch, with goose's OpenAI-format
/// request builder and stream parser.
pub struct OpenRouter {
    /// The OpenRouter base (`OPENROUTER_API_URL`).
    pub base: String,
    pub key: String,
    /// One call's deadline (`DEADLINE_MS`, or a test control's).
    pub deadline_ms: u64,
}

/// The model config a turn uses: goose's, with the output bound.
pub fn config(model: &str) -> ModelConfig {
    ModelConfig::new(model).with_max_tokens(Some(MAX_TOKENS))
}

#[async_trait]
impl Provider for OpenRouter {
    fn get_name(&self) -> &str {
        "openrouter"
    }

    async fn stream(&self, model_config: &ModelConfig, system: &str, messages: &[Message], tools: &[Tool]) -> Result<MessageStream, ProviderError> {
        let call = Call {
            url: format!("{}/api/v1/chat/completions", self.base),
            key: self.key.clone(),
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

/// One completion, with all it needs to be made again.
struct Call {
    url: String,
    key: String,
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
        let mut abort = Abort(Some(AbortController::default()));
        let signal = abort.0.as_ref().expect("armed").signal();
        let read = read_answer(&self.url, &self.key, body, &signal);
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

/// The answer's lines (server-sent events), read whole.
async fn read_answer(url: &str, key: &str, body: String, signal: &AbortSignal) -> Result<Vec<String>, ProviderError> {
    let failed = |e: worker::Error| ProviderError::RequestFailed(e.to_string());
    let headers = Headers::new();
    headers.set("authorization", &format!("Bearer {key}")).map_err(failed)?;
    headers.set("content-type", "application/json").map_err(failed)?;
    headers.set("x-title", "fragment agent").map_err(failed)?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post).with_headers(headers).with_body(Some(body.into()));
    let request = Request::new_with_init(url, &init).map_err(failed)?;
    let mut response = Fetch::Request(request).send_with_signal(signal).await.map_err(|e| ProviderError::NetworkError(e.to_string()))?;
    let status = response.status_code();
    if status != 200 {
        let text = response.text().await.unwrap_or_default();
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

/// goose's parse of the answer, then what this module adds: nothing is
/// `Empty`, and a cut call is repaired or refused.
async fn parse(lines: Vec<String>) -> Attempt {
    let raw = raw_calls(&lines);
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
    for message in items.iter_mut().filter_map(|(m, _)| m.as_mut()) {
        if !message.metadata.output_token_limit_reached {
            continue;
        }
        for content in message.content.iter_mut() {
            let MessageContent::ToolRequest(request) = content else { continue };
            if request.tool_call.is_ok() {
                continue;
            }
            let Some((name, args)) = raw.get(&request.id) else { continue };
            request.tool_call = cut_call(name, args);
        }
    }
    Attempt::Answered(items)
}

/// Each tool call's name and arguments as they streamed, by its id.
fn raw_calls(lines: &[String]) -> HashMap<String, (String, String)> {
    let mut by_index: BTreeMap<i64, (String, String, String)> = BTreeMap::new();
    for line in lines {
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { continue };
        if data == "[DONE]" {
            break;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else { continue };
        for (position, call) in chunk["choices"][0]["delta"]["tool_calls"].as_array().into_iter().flatten().enumerate() {
            let index = call["index"].as_i64().unwrap_or(position as i64);
            let (id, name, args) = by_index.entry(index).or_default();
            if let (true, Some(new)) = (id.is_empty(), call["id"].as_str()) {
                *id = new.to_string();
            }
            if let Some(new) = call["function"]["name"].as_str().filter(|n| !n.is_empty()) {
                *name = new.to_string();
            }
            if let Some(more) = call["function"]["arguments"].as_str() {
                args.push_str(more);
            }
        }
    }
    by_index.into_values().filter(|(id, _, _)| !id.is_empty()).map(|(id, name, args)| (id, (name, args))).collect()
}

/// A call in an answer cut off at its output limit (goose refuses them
/// all): whole, it runs; a cut write of a file runs with what it wrote so
/// far, marked; any other is refused, saying why.
fn cut_call(name: &str, args: &str) -> Result<CallToolRequestParams, ErrorData> {
    if let Ok(Value::Object(whole)) = serde_json::from_str::<Value>(args) {
        return Ok(CallToolRequestParams::new(name.to_string()).with_arguments(whole));
    }
    if name == WRITE_FILE || name == APPEND_FILE {
        if let Some(mut kept) = close_json(args).filter(|a| ["fragment", "path", "text"].iter().all(|k| a[*k].as_str().is_some_and(|s| !s.is_empty()))) {
            kept.insert(CUT_OFF.into(), json!(true));
            return Ok(CallToolRequestParams::new(name.to_string()).with_arguments(kept));
        }
    }
    Err(ErrorData::invalid_params(
        format!(
            "{name} was cut off: your reply reached its output limit ({MAX_TOKENS} tokens) before this call was whole, so it \
             did not run. Send less per call: one file per call, under 150 lines; write a long file with {WRITE_FILE} and \
             add the rest with {APPEND_FILE}."
        ),
        None,
    ))
}

/// The JSON object `partial` begins, closed where it was cut: its open
/// string and containers closed, after dropping what cannot end there (half
/// an escape, a key without its value, a comma). `None` when no prefix
/// closes into an object.
pub fn close_json(partial: &str) -> Option<Map<String, Value>> {
    let mut end = partial.len();
    for _ in 0..=REPAIR_DROPS_MAX {
        if let Some(object) = close_at(&partial[..end]) {
            return Some(object);
        }
        // one character fewer
        end = partial[..end].char_indices().next_back()?.0;
    }
    None
}

fn close_at(prefix: &str) -> Option<Map<String, Value>> {
    let mut stack = Vec::new();
    let (mut in_string, mut escaped, mut unicode) = (false, false, 0u8);
    for c in prefix.chars() {
        if in_string {
            if unicode > 0 {
                unicode -= 1;
            } else if escaped {
                escaped = false;
                if c == 'u' {
                    unicode = 4;
                }
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop()?;
            }
            _ => {}
        }
    }
    // half an escape cannot end a string
    if escaped || unicode > 0 {
        return None;
    }
    let mut closed = prefix.to_string();
    if in_string {
        closed.push('"');
    }
    closed.extend(stack.iter().rev());
    match serde_json::from_str::<Value>(&closed) {
        Ok(Value::Object(object)) => Some(object),
        _ => None,
    }
}

/// What the turn's tool calls did, said for them when the model, after
/// they worked, twice answered with nothing. `None` when none worked.
fn summary(messages: &[Message]) -> Option<String> {
    let start = messages.iter().rposition(|m| m.role == rmcp::model::Role::User && !m.is_tool_response() && !m.as_concat_text().trim().is_empty())?;
    let turn = &messages[start..];
    let mut calls: HashMap<&str, (&str, Value)> = HashMap::new();
    for request in turn.iter().flat_map(|m| m.content.iter()).filter_map(MessageContent::as_tool_request) {
        if let Ok(call) = &request.tool_call {
            calls.insert(request.id.as_str(), (call.name.as_ref(), Value::Object(call.arguments.clone().unwrap_or_default())));
        }
    }
    let mut did: Vec<String> = Vec::new();
    for content in turn.iter().flat_map(|m| m.content.iter()) {
        let Some(response) = content.as_tool_response() else { continue };
        let worked = response.tool_result.as_ref().is_ok_and(|r| r.is_error != Some(true));
        let Some((name, args)) = calls.get(response.id.as_str()) else { continue };
        if !worked {
            continue;
        }
        let answer: Value = MessageContent::as_tool_response_text(content).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let fragment = args["fragment"].as_str().unwrap_or("");
        let line = match *name {
            WRITE_FILE | APPEND_FILE => format!("wrote {} in {fragment}", args["path"].as_str().unwrap_or("a file")),
            "platform__write_files" => {
                let paths: Vec<&str> = args["files"].as_array().into_iter().flatten().filter_map(|f| f["path"].as_str()).collect();
                format!("wrote {} in {fragment}", paths.join(", "))
            }
            "platform__deploy" => match answer["url"].as_str() {
                Some(url) => format!("deployed {fragment}: {url}"),
                None => format!("deployed {fragment}"),
            },
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

