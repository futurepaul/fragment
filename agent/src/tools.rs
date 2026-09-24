//! An agent's tools are the operations of the fragments it belongs to
//! (MODEL.md, Agents). The catalog is read once per turn driver: the
//! fragments that list the agent's key as a member (`GET /api/fragments`)
//! and each one's operations (`status.code.operations`, whose input schemas
//! are the tool schemas), filtered to those its role there may call. A call
//! is the operation itself, signed by the agent, with an id made from the
//! model's tool-call id: a replayed step replays the operation, and the
//! fragment's ledger answers it without running it again.
//!
//! Beside them, the platform's own verbs (`platform__*`, phase 6 step 4d):
//! make a fragment for the owner, list and read its files, write files,
//! and deploy, each the signed API the CLI uses, so the agent can make an
//! app. A file write's key comes from the tool-call id, so a replayed step
//! commits nothing twice.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use fragment_proto::{FragmentList, FragmentStatus, OpDecl, OpKind, OpResult};
use serde::Deserialize;
use goose_agent::operation::Emitter;
use goose_agent::tool::ToolProvider;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Tool};
use serde_json::{json, Value};
use fragment_core::tools::{op_id, tool_name};
use worker::send::SendFuture;
use worker::{Delay, Method, SqlStorage};

use crate::fleet::{self, Fleet};
use crate::js;
use crate::store::{kv_u64, Session};

/// The fragments, and the tools, one agent's turn considers at most.
pub const FRAGMENTS_MAX: usize = 16;
pub const TOOLS_MAX: usize = 128;
/// The most of an operation's answer the model reads back.
const RESULT_TEXT_MAX: usize = 16 * 1024;

fn describe(fragment: &str, op: &str, decl: &OpDecl) -> String {
    let what = match decl.kind {
        OpKind::Query => "Reads",
        OpKind::Mutation => "Changes",
        OpKind::Job => "Starts a job on",
    };
    format!("{what} the fragment `{fragment}`: its `{op}` operation. Answers the operation's result as JSON.")
}

#[derive(Clone)]
enum Route {
    Op { fragment: String, op: String },
    Platform(&'static str),
}

/// The platform's verbs: (name, description, input schema).
fn platform_tools() -> Vec<(&'static str, &'static str, Value)> {
    let fragment = json!({ "type": "string", "description": "the fragment's full name, <label>.<username>" });
    vec![
        (
            "platform__create_fragment",
            "Makes a new fragment (an app, a page, a list) for your owner, named <label>.<their username>, from a \
             template: blank (one page), todo (a live list: a working example of an app), inbox, or chat. You become \
             its editor. Answers its name and URL. An app is fragment.json (its operations), app.mjs (their code), \
             and site/index.html (its page, which imports ./__fragment.js to call them): read the todo template's \
             files to see the shape before you write your own.",
            json!({ "type": "object", "required": ["label"], "additionalProperties": false, "properties": {
                "label": { "type": "string", "description": "lowercase letters, digits, and single dashes" },
                "template": { "type": "string", "enum": ["blank", "todo", "inbox", "chat"] },
            } }),
        ),
        (
            "platform__list_files",
            "Lists a fragment's files (at main, what the next deploy ships).",
            json!({ "type": "object", "required": ["fragment"], "additionalProperties": false, "properties": { "fragment": fragment } }),
        ),
        (
            "platform__read_file",
            "Reads one of a fragment's files as text.",
            json!({ "type": "object", "required": ["fragment", "path"], "additionalProperties": false, "properties": {
                "fragment": fragment, "path": { "type": "string" },
            } }),
        ),
        (
            "platform__write_files",
            "Writes files to a fragment in one commit (at most 16 files and 256 KiB). A file whose text is null is removed. \
             Nothing changes for its visitors until you deploy.",
            json!({ "type": "object", "required": ["fragment", "files"], "additionalProperties": false, "properties": {
                "fragment": fragment,
                "files": { "type": "array", "items": { "type": "object", "required": ["path", "text"], "properties": {
                    "path": { "type": "string" }, "text": { "type": ["string", "null"] },
                } } },
                "message": { "type": "string" },
            } }),
        ),
        (
            "platform__deploy",
            "Deploys a fragment: what its files are now goes live for everyone who opens it. Answers its URL.",
            json!({ "type": "object", "required": ["fragment"], "additionalProperties": false, "properties": {
                "fragment": fragment, "note": { "type": "string" },
            } }),
        ),
    ]
}

/// A platform verb's request: (method, path, body).
fn platform_request(tool: &str, args: &Value, request_id: &str) -> Result<(Method, String, Option<Value>), String> {
    let text = |k: &str| args[k].as_str().map(str::to_string).ok_or_else(|| format!("{k} is required"));
    let q = |s: &str| {
        let mut u = worker::Url::parse("https://q/").expect("a URL");
        u.query_pairs_mut().append_pair("path", s);
        u.query().unwrap_or_default().to_string()
    };
    Ok(match tool {
        "platform__create_fragment" => {
            (Method::Post, "/api/fragments".into(), Some(json!({ "name": text("label")?, "template": args["template"].as_str().unwrap_or("blank") })))
        }
        "platform__list_files" => (Method::Get, format!("/api/f/{}/files", text("fragment")?), None),
        "platform__read_file" => (Method::Get, format!("/api/f/{}/file?{}", text("fragment")?, q(&text("path")?)), None),
        "platform__write_files" => {
            let files: Vec<Value> = args["files"]
                .as_array()
                .ok_or("files is a list")?
                .iter()
                .map(|f| match f["text"].as_str() {
                    Some(t) => json!({ "path": f["path"], "text": t }),
                    None => json!({ "path": f["path"], "delete": true }),
                })
                .collect();
            let body = json!({ "files": files, "message": args["message"].as_str().unwrap_or("written by an agent"), "key": op_id(request_id) });
            (Method::Post, format!("/api/f/{}/files", text("fragment")?), Some(body))
        }
        "platform__deploy" => (Method::Post, format!("/api/f/{}/deploy", text("fragment")?), Some(json!({ "note": args["note"] }))),
        other => return Err(format!("no tool named {other}")),
    })
}

struct Catalog {
    tools: Vec<Tool>,
    routes: HashMap<String, Route>,
}

pub struct FragmentTools {
    pub fleet: Fleet,
    pub sql: SqlStorage,
    pub driver: String,
    catalog: Mutex<Option<Arc<Catalog>>>,
}

fn internal(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}

impl FragmentTools {
    pub fn new(fleet: Fleet, sql: SqlStorage, driver: String) -> FragmentTools {
        FragmentTools { fleet, sql, driver, catalog: Mutex::new(None) }
    }

    async fn read_catalog(fleet: Fleet) -> anyhow::Result<Catalog> {
        let listed: FragmentList = fleet.get_as("/api/fragments").await?;
        let mut tools = Vec::new();
        let mut routes = HashMap::new();
        for (name, description, schema) in platform_tools() {
            let schema: rmcp::model::JsonObject = serde_json::from_value(schema)?;
            tools.push(Tool::new(name, description, Arc::new(schema)));
            routes.insert(name.to_string(), Route::Platform(name));
        }
        for f in listed.fragments.iter().take(FRAGMENTS_MAX) {
            let name = f.name.as_str();
            let status: FragmentStatus = match fleet.get_as(&format!("/api/f/{name}/status")).await {
                Ok(s) => s,
                // a fragment that will not answer (or not with a status) is left out, not fatal
                Err(_) => continue,
            };
            for (op, decl) in status.code.operations {
                if decl.role > f.role || tools.len() >= TOOLS_MAX {
                    continue;
                }
                let Some(tool) = tool_name(name, &op) else { continue };
                let schema = decl.input.clone().filter(Value::is_object).unwrap_or_else(|| json!({ "type": "object" }));
                let schema: rmcp::model::JsonObject = serde_json::from_value(schema)?;
                tools.push(Tool::new(tool.clone(), describe(name, &op, &decl), Arc::new(schema)));
                routes.insert(tool, Route::Op { fragment: name.to_string(), op });
            }
        }
        Ok(Catalog { tools, routes })
    }

    async fn catalog(&self) -> anyhow::Result<Arc<Catalog>> {
        if let Some(c) = self.catalog.lock().expect("catalog lock").clone() {
            return Ok(c);
        }
        let fleet = self.fleet.clone();
        let catalog = Arc::new(SendFuture::new(async move { FragmentTools::read_catalog(fleet).await }).await?);
        *self.catalog.lock().expect("catalog lock") = Some(catalog.clone());
        Ok(catalog)
    }
}

/// The tools an agent has now (for its owner's view).
pub async fn list(fleet: Fleet) -> anyhow::Result<Vec<String>> {
    let catalog = FragmentTools::read_catalog(fleet).await?;
    Ok(catalog.tools.iter().map(|t| t.name.to_string()).collect())
}

#[async_trait]
impl ToolProvider<Session> for FragmentTools {
    async fn tools(&self, _: &Session) -> anyhow::Result<Vec<Tool>> {
        Ok(self.catalog().await?.tools.clone())
    }

    async fn call(&self, _: &Session, request_id: &str, call: CallToolRequestParams, _: &Emitter) -> Result<CallToolResult, ErrorData> {
        self.sql
            .exec(
                "INSERT INTO tool_runs (tool_call_id, tool, at, driver) VALUES (?, ?, ?, ?)",
                vec![request_id.into(), call.name.to_string().into(), (js::now_ms() as i64).into(), self.driver.as_str().into()],
            )
            .map_err(|e| internal(anyhow!("{e}")))?;
        let catalog = self.catalog().await.map_err(internal)?;
        let Some(route) = catalog.routes.get(call.name.as_ref()).cloned() else {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!("no tool named {}", call.name))]));
        };
        let args = Value::Object(call.arguments.unwrap_or_default());
        let (method, path, body) = match &route {
            Route::Op { fragment, op } => (Method::Post, format!("/api/f/{fragment}/ops/{op}"), Some(json!({ "id": op_id(request_id), "input": args }))),
            Route::Platform(tool) => match platform_request(tool, &args, request_id) {
                Ok(r) => r,
                Err(why) => return Ok(CallToolResult::error(vec![ContentBlock::text(why)])),
            },
        };
        let fleet = self.fleet.clone();
        let (status, answer) = SendFuture::new(async move { fleet.call(method, &path, body.as_ref()).await }).await.map_err(internal)?;
        // Test hook: hold after the operation ran and before its result is
        // persisted, so a kill lands in the at-least-once window.
        let hold = kv_u64(&self.sql, "test_hold_in_tool_ms").map_err(internal)?;
        if hold > 0 {
            SendFuture::new(Delay::from(Duration::from_millis(hold))).await;
        }
        if status != 200 {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!("{status}: {}", fleet::message(&answer)))]));
        }
        let mut text = match (&route, &answer) {
            (Route::Op { .. }, _) => match OpResult::deserialize(&answer) {
                Ok(done) => done.result.to_string(),
                Err(e) => return Ok(CallToolResult::error(vec![ContentBlock::text(format!("the fragment's answer is not an operation's result: {e}"))])),
            },
            // a file's bytes come back as text; the rest are JSON
            (Route::Platform(_), Value::String(s)) => s.clone(),
            (Route::Platform(_), v) => v.to_string(),
        };
        if text.len() > RESULT_TEXT_MAX {
            text.truncate(text.floor_char_boundary(RESULT_TEXT_MAX));
            text.push_str(" …(truncated)");
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}
