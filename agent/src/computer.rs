//! An agent's computer (phase 8): a machine running `fragment computer
//! serve`, reached at its URL with its own bearer token. While one is
//! attached, the agent's turns also get goose's developer tools (shell,
//! write, edit, tree), run there in the attached project directory. The
//! loop stays in the agent's cell, so the computer holds no platform
//! credential and no model key (ported from spikes/goose-agent's cell,
//! `RemoteTools`).
//!
//! A call is started or re-attached by an id made from the model's
//! tool-call id, so a driver that dies mid-call and the watchdog's replay
//! reach the same run, and the computer's journal answers "interrupted"
//! for a call its own restart cut off.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use goose_agent::operation::Emitter;
use goose_agent::tool::ToolProvider;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Tool};
use serde_json::{json, Value};
use worker::send::SendFuture;
use worker::{Delay, Fetch, Headers, Method, Request, RequestInit, SqlStorage};

use crate::js;
use crate::store::Session;

/// How long one `/calls` request waits for the call to finish, and how
/// many such waits one call gets (about an hour).
const CALL_WAIT_MS: u64 = 15_000;
const CALL_POLLS_MAX: u32 = 240;
/// Transient failures (the computer restarting, a proxy in front of it)
/// are retried with backoff: 1, 2, 4, 8, 16, 16 s.
const ATTEMPTS: u32 = 6;
const BACKOFF_MS: u64 = 1_000;
/// The largest answer the agent reads from its computer.
const ANSWER_MAX_BYTES: usize = 4 * 1024 * 1024;
/// The most of a tool's output the model reads back.
const RESULT_TEXT_MAX: usize = 64 * 1024;
pub const CWD_MAX: usize = 64;

#[derive(Clone)]
pub struct Computer {
    pub url: String,
    pub token: String,
}

impl Computer {
    async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> anyhow::Result<(u16, Value)> {
        let headers = Headers::new();
        headers.set("authorization", &format!("Bearer {}", self.token)).map_err(|e| anyhow!("{e}"))?;
        let mut init = RequestInit::new();
        if let Some(body) = body {
            headers.set("content-type", "application/json").map_err(|e| anyhow!("{e}"))?;
            init.with_body(Some(serde_json::to_string(body)?.into()));
        }
        init.with_method(method).with_headers(headers);
        let request = Request::new_with_init(&format!("{}{path}", self.url), &init).map_err(|e| anyhow!("{e}"))?;
        let mut response = Fetch::Request(request).send().await.map_err(|e| anyhow!("{path}: {e}"))?;
        let status = response.status_code();
        let bytes = response.bytes().await.map_err(|e| anyhow!("{path}: {e}"))?;
        if bytes.len() > ANSWER_MAX_BYTES {
            return Err(anyhow!("{path}: the computer answered {} bytes; at most {ANSWER_MAX_BYTES}", bytes.len()));
        }
        let value = serde_json::from_slice(&bytes).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
        Ok((status, value))
    }

    /// Every computer route is idempotent (calls by id), so a transient
    /// answer is retried: the Sprites proxy answered 404 "sprite not found"
    /// and stalled ~9 s on 2026-09-23 (the spike's measurements).
    async fn request_retrying(&self, method: Method, path: &str, body: Option<&Value>) -> anyhow::Result<(u16, Value)> {
        let mut last = String::new();
        for attempt in 0..ATTEMPTS {
            match self.request(method.clone(), path, body).await {
                Ok((status, value)) if !transient(status, &value) => return Ok((status, value)),
                Ok((status, value)) => last = format!("{status} {value}"),
                Err(error) => last = error.to_string(),
            }
            Delay::from(Duration::from_millis(BACKOFF_MS << attempt.min(4))).await;
        }
        Err(anyhow!("{path}: the computer stayed unreachable after {ATTEMPTS} attempts: {last}"))
    }

    pub async fn get(&self, path: &str) -> anyhow::Result<Value> {
        let (status, value) = self.request_retrying(Method::Get, path, None).await?;
        anyhow::ensure!(status == 200, "GET {path}: {status} {value}");
        Ok(value)
    }

    pub async fn post(&self, path: &str, body: &Value) -> anyhow::Result<Value> {
        let (status, value) = self.request_retrying(Method::Post, path, Some(body)).await?;
        anyhow::ensure!(status == 200, "POST {path}: {status} {value}");
        Ok(value)
    }

    /// One try, no retries: attaching checks the URL and the token answer.
    pub async fn check(&self) -> Result<Value, Unanswered> {
        let (status, value) = self.request(Method::Get, "/tools", None).await.map_err(|e| Unanswered::Unreachable(e.to_string()))?;
        match status {
            200 => Ok(value),
            401 => Err(Unanswered::Refused),
            _ => Err(Unanswered::Unreachable(format!("{status} {}", value["error"].as_str().map(str::to_string).unwrap_or_else(|| value.to_string())))),
        }
    }

    /// Cancels calls a stopped turn left running (goose drops a tool's
    /// future when its turn is cancelled, so the call cannot do it itself).
    pub async fn cancel_all(&self, ids: Vec<String>) {
        for id in ids {
            let _ = self.request_retrying(Method::Post, &format!("/calls/{id}/cancel"), Some(&json!({}))).await;
        }
    }
}

/// Why a computer did not answer an attach.
pub enum Unanswered {
    /// It answered 401: not its token.
    Refused,
    Unreachable(String),
}

/// The path in front of the computer failing, not the computer answering:
/// rate limits, 5xx, the Sprites proxy's "sprite not found".
fn transient(status: u16, value: &Value) -> bool {
    status == 429 || status >= 500 || status == 404 && value["error"].as_str().is_some_and(|e| e.contains("sprite not found"))
}

pub fn valid_cwd(cwd: &str) -> bool {
    !cwd.is_empty() && cwd.len() <= CWD_MAX && cwd.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The tools a computer offers, from its `/tools` answer.
pub fn tools_of(manifest: &Value) -> anyhow::Result<Vec<Tool>> {
    serde_json::from_value(manifest["tools"].clone()).map_err(|e| anyhow!("the computer's tools: {e}"))
}

fn internal(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}

/// Keeps a result's text within what the model reads back.
fn bounded(mut result: CallToolResult) -> CallToolResult {
    for block in result.content.iter_mut() {
        if let Some(text) = block.as_text().map(|t| t.text.clone()) {
            if text.len() > RESULT_TEXT_MAX {
                let mut cut = text;
                cut.truncate(cut.floor_char_boundary(RESULT_TEXT_MAX));
                cut.push_str(" …(truncated)");
                *block = ContentBlock::text(cut);
            }
        }
    }
    result
}

pub struct ComputerTools {
    pub computer: Computer,
    pub cwd: String,
    pub tools: Vec<Tool>,
    pub sql: SqlStorage,
    pub driver: String,
    /// Calls started and not yet answered, for a stop to cancel.
    pub in_flight: Arc<Mutex<BTreeSet<String>>>,
}

#[async_trait]
impl ToolProvider<Session> for ComputerTools {
    async fn tools(&self, _: &Session) -> anyhow::Result<Vec<Tool>> {
        Ok(self.tools.clone())
    }

    async fn call(&self, _: &Session, request_id: &str, call: CallToolRequestParams, _: &Emitter) -> Result<CallToolResult, ErrorData> {
        // One row per attempt from this agent; the computer counts runs.
        self.sql
            .exec(
                "INSERT INTO tool_runs (tool_call_id, tool, at, driver) VALUES (?, ?, ?, ?)",
                vec![request_id.into(), call.name.to_string().into(), (js::now_ms() as i64).into(), self.driver.as_str().into()],
            )
            .map_err(|e| internal(anyhow!("{e}")))?;
        let id = fragment_core::tools::call_id(request_id);
        self.in_flight.lock().expect("in-flight lock").insert(id.clone());
        let body = json!({ "id": id, "name": call.name, "arguments": call.arguments, "cwd": self.cwd, "wait_ms": CALL_WAIT_MS });
        for _ in 0..CALL_POLLS_MAX {
            // A stop drops this future (goose's ToolOperation races the
            // call against the turn's cancellation); the driver then
            // cancels what `in_flight` still holds.
            let computer = self.computer.clone();
            let request = body.clone();
            let record = SendFuture::new(async move { computer.post("/calls", &request).await }).await.map_err(internal)?;
            match record["status"].as_str() {
                Some("running") => continue,
                Some("done" | "interrupted") => {
                    self.in_flight.lock().expect("in-flight lock").remove(&id);
                    let result: CallToolResult = serde_json::from_value(record["result"].clone()).map_err(|e| internal(anyhow!("the computer returned a bad result: {e}")))?;
                    return Ok(bounded(result));
                }
                other => return Err(internal(anyhow!("the computer answered status {other:?}"))),
            }
        }
        Err(internal(anyhow!("the call ran longer than {CALL_POLLS_MAX} waits of {CALL_WAIT_MS} ms")))
    }
}
