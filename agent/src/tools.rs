//! An agent's tools are the operations of the fragments it belongs to
//! (MODEL.md, Agents). The catalog is read once per turn driver: the
//! fragments that list the agent's key as a member (`GET /api/fragments`)
//! and each one's operations (`status.code.operations`, whose input schemas
//! are the tool schemas), filtered to those its role there may call. A call
//! is the operation itself, signed by the agent, with an id made from the
//! model's tool-call id: a replayed step replays the operation, and the
//! fragment's ledger answers it without running it again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use fragment_proto::{OpDecl, OpKind, Role};
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
struct Route {
    fragment: String,
    op: String,
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
        let listed = fleet.get("/api/fragments").await?;
        let mut tools = Vec::new();
        let mut routes = HashMap::new();
        for f in listed["fragments"].as_array().into_iter().flatten().take(FRAGMENTS_MAX) {
            let (Some(name), Some(role)) = (f["name"].as_str(), f["role"].as_str()) else { continue };
            let role: Role = serde_json::from_value(json!(role))?;
            let status = match fleet.get(&format!("/api/f/{name}/status")).await {
                Ok(s) => s,
                // a fragment that will not answer is left out, not fatal
                Err(_) => continue,
            };
            let ops: std::collections::BTreeMap<String, OpDecl> = serde_json::from_value(status["code"]["operations"].clone()).unwrap_or_default();
            for (op, decl) in ops {
                if decl.role > role || tools.len() >= TOOLS_MAX {
                    continue;
                }
                let Some(tool) = tool_name(name, &op) else { continue };
                let schema = decl.input.clone().filter(Value::is_object).unwrap_or_else(|| json!({ "type": "object" }));
                let schema: rmcp::model::JsonObject = serde_json::from_value(schema)?;
                tools.push(Tool::new(tool.clone(), describe(name, &op, &decl), Arc::new(schema)));
                routes.insert(tool, Route { fragment: name.to_string(), op });
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
        let body = json!({ "id": op_id(request_id), "input": Value::Object(call.arguments.unwrap_or_default()) });
        let fleet = self.fleet.clone();
        let path = format!("/api/f/{}/ops/{}", route.fragment, route.op);
        let (status, answer) = SendFuture::new(async move { fleet.call(Method::Post, &path, Some(&body)).await }).await.map_err(internal)?;
        // Test hook: hold after the operation ran and before its result is
        // persisted, so a kill lands in the at-least-once window.
        let hold = kv_u64(&self.sql, "test_hold_in_tool_ms").map_err(internal)?;
        if hold > 0 {
            SendFuture::new(Delay::from(Duration::from_millis(hold))).await;
        }
        if status != 200 {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!("{status}: {}", fleet::message(&answer)))]));
        }
        let mut text = answer["result"].to_string();
        if text.len() > RESULT_TEXT_MAX {
            text.truncate(text.floor_char_boundary(RESULT_TEXT_MAX));
            text.push_str(" …(truncated)");
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}
