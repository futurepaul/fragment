//! An agent's tools (MODEL.md, Agents). A turn acts for whoever started it
//! (ROADMAP decision 17): every call names them (`for`, fleet.rs), and the
//! platform acts with the lower of their role and the agent's cap. So the
//! agent reaches every fragment its asker can, which for a person may be
//! hundreds: a tool per operation of each would be a tool explosion. The
//! catalog, read once per turn, holds two kinds.
//!
//! Per-operation tools, for this turn's chat and the fragments the agent
//! is a member of (the agent's memberships, `GET /api/fragments`, less the
//! other chats it follows, which are conversations, not apps): each
//! fragment's operations (`status.code.operations`, read `for` the asker,
//! so its role is the one this turn acts with, and whose input schemas are
//! the tool schemas), those that role may call, less the reply operation
//! of each channel it follows (the platform posts its answers there:
//! `listens`). A call is the operation itself, signed by the agent, with
//! an id made from the model's tool-call id: a replayed step replays the
//! operation, and the fragment's ledger answers it without running it
//! again.
//!
//! The platform's own verbs (`platform__*`), for everything else: list the
//! fragments the asker reaches, read one's operations, call one, and list
//! and read its files, each the signed API the CLI uses; and, in its
//! owner's turns only, make a fragment from a template for the owner, and
//! hand work to a computer (handoff.rs). The agent builds nothing itself
//! (Paul, 2026-09-27): what it cannot do in a few calls goes to a computer.
//! A fragment's own agent (`Scope`) has neither kind but one: the
//! operations of its fragment that its block names.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use fragment_proto::{valid_fragment_name, valid_op_name, FragmentList, FragmentStatus, ListedFragment, OpDecl, OpKind, OpResult, Run, RunStatus};
use futures::StreamExt;
use serde::Deserialize;
use goose_agent::operation::Emitter;
use goose_agent::tool::ToolProvider;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, Tool};
use serde_json::{json, Value};
use fragment_core::tools::{op_id, tool_name};
use worker::send::SendFuture;
use worker::{Delay, Method, SqlStorage};

use crate::fleet::{self, Fleet};
use crate::store::{chat_of, kv_u64, Session};
use crate::{handoff, js};

/// The fragments, and the tools, one agent's turn considers at most.
pub const FRAGMENTS_MAX: usize = 16;
pub const TOOLS_MAX: usize = 128;
/// Status reads in flight at once while the catalog is read: 16 fragments
/// are three waves of round trips before the first model token, not 16.
const STATUS_READS_AT_ONCE: usize = 6;
/// The most of an operation's answer the model reads back.
const RESULT_TEXT_MAX: usize = 16 * 1024;
/// How long a job's call waits for its run to end, and how often it looks.
const JOB_WAIT_MS: u64 = 90_000;
const JOB_POLL_MS: u64 = 1_000;

/// The run a job's call started: a job's call answers exactly `{run, status}`.
fn started_run(result: &Value) -> Option<i64> {
    let o = result.as_object().filter(|o| o.len() == 2 && o.get("status").is_some_and(Value::is_string))?;
    o.get("run")?.as_i64()
}

fn describe(fragment: &str, op: &str, decl: &OpDecl) -> String {
    let what = match decl.kind {
        OpKind::Query => "Reads",
        OpKind::Mutation => "Changes",
        OpKind::Job => "Runs a job on",
    };
    format!("{what} the fragment `{fragment}`: its `{op}` operation. Answers the operation's result as JSON.")
}

#[derive(Clone)]
enum Route {
    Op { fragment: String, op: String },
    Platform(&'static str),
}

/// The platform's verbs: (name, description, input schema). The first two
/// are offered in its owner's turns only.
fn platform_tools(owner_turn: bool) -> Vec<(&'static str, &'static str, Value)> {
    let fragment = json!({ "type": "string", "description": "the fragment's full name, <label>.<username>" });
    let create = (
        "platform__create_fragment",
        "Makes a new fragment for your owner from a template, as they could, named <label>.<their username>: blank (one \
         page), todo (a live list), inbox, or chat. You become its editor. Answers its name and URL. To build something \
         new in it, hand the work off.",
        json!({ "type": "object", "required": ["label"], "additionalProperties": false, "properties": {
            "label": { "type": "string", "description": "lowercase letters, digits, and single dashes" },
            "template": { "type": "string", "enum": ["blank", "todo", "inbox", "chat"] },
        } }),
    );
    let hand_off = (
        handoff::TOOL,
        "Hands work to a computer: building or changing an app, writing code, research, anything more than a few calls. \
         It answers at once and the work runs for minutes on its own; its result is said in this conversation when it \
         ends. Without `computer`, a throwaway computer does it (what it builds is a new fragment of your owner's) and \
         is removed after, keeping what it built.",
        json!({ "type": "object", "required": ["task"], "additionalProperties": false, "properties": {
            "task": { "type": "string", "description": "the whole task, as the computer should read it: it sees nothing of this conversation" },
            "computer": { "type": "string", "description": "only when the person names one: a fragment of theirs with a computer and a `do` or `build` job (<label>.<username>)" },
        } }),
    );
    let mut tools = if owner_turn { vec![create, hand_off] } else { Vec::new() };
    tools.extend([
        (
            "platform__list_fragments",
            "Lists the fragments you can reach for the person who asked you (what they may reach, and you or your \
             owner are in too), each with the role you act with there.",
            json!({ "type": "object", "additionalProperties": false, "properties": {} }),
        ),
        (
            "platform__operations",
            "Reads a fragment's operations: each one's kind (query, mutation, job), the weakest role that may call \
             it, and its input's JSON Schema; and the role you act with there. Use it before platform__call on a \
             fragment you have no tools for.",
            json!({ "type": "object", "required": ["fragment"], "additionalProperties": false, "properties": { "fragment": fragment } }),
        ),
        (
            "platform__call",
            "Calls one of a fragment's operations with its input, as the person who asked you may. Answers the \
             operation's result as JSON.",
            json!({ "type": "object", "required": ["fragment", "operation"], "additionalProperties": false, "properties": {
                "fragment": fragment,
                "operation": { "type": "string" },
                "input": { "type": "object", "description": "the operation's input, as its schema says" },
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
    ]);
    tools
}

/// A platform verb's request: (method, path, body).
fn platform_request(tool: &str, args: &Value, request_id: &str) -> Result<(Method, String, Option<Value>), String> {
    let text = |k: &str| args[k].as_str().map(str::to_string).ok_or_else(|| format!("{k} is required"));
    // a fragment's name goes into a path: it must be one
    let fragment = || text("fragment").and_then(|f| if valid_fragment_name(&f) { Ok(f) } else { Err(format!("{f:?} is not a fragment's name (<label>.<username>)")) });
    let q = |s: &str| {
        let mut u = worker::Url::parse("https://q/").expect("a URL");
        u.query_pairs_mut().append_pair("path", s);
        u.query().unwrap_or_default().to_string()
    };
    Ok(match tool {
        "platform__create_fragment" => {
            (Method::Post, "/api/fragments".into(), Some(json!({ "name": text("label")?, "template": args["template"].as_str().unwrap_or("blank") })))
        }
        "platform__list_fragments" => (Method::Get, "/api/fragments".into(), None),
        "platform__operations" => (Method::Get, format!("/api/f/{}/status", fragment()?), None),
        "platform__call" => {
            let op = text("operation")?;
            if !valid_op_name(&op) {
                return Err(format!("{op:?} is not an operation's name"));
            }
            let input = if args["input"].is_null() { json!({}) } else { args["input"].clone() };
            (Method::Post, format!("/api/f/{}/ops/{op}", fragment()?), Some(json!({ "id": op_id(request_id), "input": input })))
        }
        "platform__list_files" => (Method::Get, format!("/api/f/{}/files", fragment()?), None),
        "platform__read_file" => (Method::Get, format!("/api/f/{}/file?{}", fragment()?, q(&text("path")?)), None),
        other => return Err(format!("no tool named {other}")),
    })
}

/// A platform verb's answer, as the model reads it.
fn platform_answer(tool: &str, answer: Value) -> Result<String, String> {
    Ok(match (tool, answer) {
        ("platform__operations", answer) => {
            let status = FragmentStatus::deserialize(&answer).map_err(|e| format!("the fragment's status: {e}"))?;
            json!({ "fragment": status.name, "role": status.role, "operations": status.code.operations }).to_string()
        }
        // a file's bytes come back as text; the rest are JSON
        (_, Value::String(s)) => s,
        (_, v) => v.to_string(),
    })
}

struct Catalog {
    tools: Vec<Tool>,
    routes: HashMap<String, Route>,
}

/// A fragment's own agent (its `agent` block): the fragment it answers in,
/// and the operations of it the block names. Its catalog is those alone,
/// as its asker may call them: no other fragment, and no platform verb.
#[derive(Clone)]
pub struct Scope {
    pub fragment: String,
    pub tools: Vec<String>,
}

/// The fragment agent's scope (set by the fragment's deploy: lib.rs
/// `scope`); `None` for a person's agent.
pub fn scope_of(sql: &SqlStorage) -> anyhow::Result<Option<Scope>> {
    let Some(fragment) = crate::store::kv_get(sql, "scope")?.filter(|f| !f.is_empty()) else { return Ok(None) };
    let tools = serde_json::from_str(&crate::store::kv_get(sql, "tools")?.unwrap_or_else(|| "[]".into()))?;
    Ok(Some(Scope { fragment, tools }))
}

/// One turn's tools: its calls act for its asker.
pub struct FragmentTools {
    /// Acting for the turn's asker.
    pub fleet: Fleet,
    pub sql: SqlStorage,
    pub driver: String,
    /// The turn's conversation (a chat's names its fragment).
    pub conv: String,
    /// Whether its owner started the turn.
    pub owner_turn: bool,
    pub scope: Option<Scope>,
    catalog: Mutex<Option<Arc<Catalog>>>,
}

fn internal(error: anyhow::Error) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}

/// What the catalog is read from: the fleet acting for the asker, the
/// followed channels, and the turn's.
struct Reading {
    fleet: Fleet,
    listens: Vec<(String, String)>,
    chat: Option<String>,
    owner_turn: bool,
    scope: Option<Scope>,
}

impl FragmentTools {
    pub fn new(fleet: Fleet, sql: SqlStorage, driver: String, conv: String, owner_turn: bool, scope: Option<Scope>) -> FragmentTools {
        assert!(fleet.acting_for.is_some(), "a turn's tools act for its asker");
        FragmentTools { fleet, sql, driver, conv, owner_turn, scope, catalog: Mutex::new(None) }
    }

    /// A fragment agent's catalog: the operations its block names, those
    /// its asker's role there may call (read `for` them).
    async fn scoped_catalog(fleet: Fleet, scope: Scope) -> anyhow::Result<Catalog> {
        let (mut tools, mut routes) = (Vec::new(), HashMap::new());
        let status = match fleet.get_as::<FragmentStatus>(&format!("/api/f/{}/status", scope.fragment)).await {
            Ok(status) => status,
            // an asker with no role there gets no tools, and an answer still
            Err(e) => {
                worker::console_warn!("the agent's tools leave out {}: {e:#}", scope.fragment);
                return Ok(Catalog { tools, routes });
            }
        };
        for (op, decl) in status.code.operations.into_iter().filter(|(op, _)| scope.tools.contains(op)) {
            let Some(tool) = tool_name(&scope.fragment, &op).filter(|_| decl.role <= status.role) else { continue };
            let schema = decl.input.clone().filter(Value::is_object).unwrap_or_else(|| json!({ "type": "object" }));
            tools.push(Tool::new(tool.clone(), describe(&scope.fragment, &op, &decl), Arc::new(serde_json::from_value(schema)?)));
            routes.insert(tool, Route::Op { fragment: scope.fragment.clone(), op });
        }
        assert!(tools.len() <= scope.tools.len(), "a fragment agent is offered only the operations its block names");
        Ok(Catalog { tools, routes })
    }

    async fn read_catalog(r: Reading) -> anyhow::Result<Catalog> {
        if let Some(scope) = r.scope {
            return FragmentTools::scoped_catalog(r.fleet, scope).await;
        }
        // the agent's own memberships: what it is in, whoever asks
        let listed: FragmentList = Fleet { acting_for: None, ..r.fleet.clone() }.get_as("/api/fragments").await?;
        let mut tools = Vec::new();
        let mut routes = HashMap::new();
        for (name, description, schema) in platform_tools(r.owner_turn) {
            let schema: rmcp::model::JsonObject = serde_json::from_value(schema)?;
            tools.push(Tool::new(name, description, Arc::new(schema)));
            routes.insert(name.to_string(), Route::Platform(name));
        }
        // the other chats it follows are conversations, not apps: out, and
        // the turn's own chat first
        let chats: HashSet<&str> = r.listens.iter().map(|(fragment, _)| fragment.as_str()).filter(|f| Some(*f) != r.chat.as_deref()).collect();
        let mut fragments: Vec<&ListedFragment> = listed.fragments.iter().filter(|f| !chats.contains(f.name.as_str())).collect();
        fragments.sort_by_key(|f| Some(f.name.as_str()) != r.chat.as_deref());
        fragments.truncate(FRAGMENTS_MAX);
        let answering: HashSet<(&str, &str)> = r.listens.iter().map(|(f, reply)| (f.as_str(), reply.as_str())).collect();
        // in the listing's order (`buffered`, not `buffer_unordered`), so
        // which tools TOOLS_MAX keeps does not depend on who answered first;
        // each read `for` the asker, so its role is this turn's
        let statuses: Vec<(&ListedFragment, anyhow::Result<FragmentStatus>)> = futures::stream::iter(fragments)
            .map(|f| {
                let fleet = r.fleet.clone();
                async move { (f, fleet.get_as::<FragmentStatus>(&format!("/api/f/{}/status", f.name)).await) }
            })
            .buffered(STATUS_READS_AT_ONCE)
            .collect()
            .await;
        for (f, status) in statuses {
            let name = f.name.as_str();
            let status = match status {
                Ok(s) => s,
                // a fragment that will not answer (or not with a status, or
                // not for this asker) is left out, not fatal
                Err(e) => {
                    worker::console_warn!("the agent's tools leave out {name}: {e:#}");
                    continue;
                }
            };
            for (op, decl) in status.code.operations {
                if decl.role > status.role || tools.len() >= TOOLS_MAX || answering.contains(&(name, op.as_str())) {
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

    /// An operation's answer, as the model reads it. A job's call answers
    /// only the run it started (`{run, status}`), so the tool waits for the
    /// run to end (at most `JOB_WAIT_MS`, read `for` the asker) and answers
    /// what the job answered, or why it was held: an agent that runs a
    /// command on a computer reads what it printed. A run still going by
    /// then is said to be.
    async fn op_answer(&self, fragment: &str, op: &str, answer: &Value) -> Result<String, String> {
        let done = OpResult::deserialize(answer).map_err(|e| format!("the fragment's answer is not an operation's result: {e}"))?;
        let Some(run) = started_run(&done.result) else { return Ok(done.result.to_string()) };
        let deadline = js::now_ms() + JOB_WAIT_MS;
        loop {
            let (fleet, path) = (self.fleet.clone(), format!("/api/f/{fragment}/runs/{run}"));
            // a run it cannot read, or one of another operation: the call's own answer
            let read = SendFuture::new(async move { fleet.get_as::<Run>(&path).await }).await.ok().filter(|r| r.op == op);
            let Some(read) = read else { return Ok(done.result.to_string()) };
            let out = match read.status {
                RunStatus::Succeeded => json!({ "run": run, "status": read.status, "output": read.output }),
                RunStatus::Held | RunStatus::Blocked => json!({ "run": run, "status": read.status, "error": read.error }),
                _ if js::now_ms() >= deadline => json!({ "run": run, "status": read.status, "note": format!("still running after {} s", JOB_WAIT_MS / 1000) }),
                _ => {
                    SendFuture::new(Delay::from(Duration::from_millis(JOB_POLL_MS))).await;
                    continue;
                }
            };
            return Ok(out.to_string());
        }
    }

    async fn catalog(&self) -> anyhow::Result<Arc<Catalog>> {
        if let Some(c) = self.catalog.lock().expect("catalog lock").clone() {
            return Ok(c);
        }
        let reading = Reading {
            fleet: self.fleet.clone(),
            listens: listens(&self.sql)?,
            chat: chat_of(&self.conv).map(|(fragment, _)| fragment.to_string()),
            owner_turn: self.owner_turn,
            scope: self.scope.clone(),
        };
        let catalog = Arc::new(SendFuture::new(async move { FragmentTools::read_catalog(reading).await }).await?);
        *self.catalog.lock().expect("catalog lock") = Some(catalog.clone());
        Ok(catalog)
    }
}

/// Each followed channel's fragment and reply operation (`listens`). The
/// platform posts a turn's answer through the reply operation (lib.rs
/// `reply`), so none is the model's tool: with it, the model said its
/// answer through the tool, and the platform said it again.
fn listens(sql: &SqlStorage) -> anyhow::Result<Vec<(String, String)>> {
    #[derive(Deserialize)]
    struct Row {
        fragment: String,
        reply: String,
    }
    let rows: Vec<Row> = sql.exec("SELECT fragment, reply FROM listens", None).and_then(|c| c.to_array()).map_err(|e| anyhow!("{e}"))?;
    Ok(rows.into_iter().map(|r| (r.fragment, r.reply)).collect())
}

/// The tools an agent's owner's turn has now (for the owner's view):
/// `fleet` acts for the owner.
pub async fn list(fleet: Fleet, sql: &SqlStorage) -> anyhow::Result<Vec<String>> {
    let reading = Reading { fleet, listens: listens(sql)?, chat: None, owner_turn: true, scope: scope_of(sql)? };
    let catalog = FragmentTools::read_catalog(reading).await?;
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
        if let Route::Platform(handoff::TOOL) = route {
            let (fleet, sql, conv, id) = (self.fleet.clone(), self.sql.clone(), self.conv.clone(), request_id.to_string());
            return Ok(match SendFuture::new(async move { handoff::start(&fleet, &sql, &conv, &id, &args).await }).await {
                Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
                Err(why) => CallToolResult::error(vec![ContentBlock::text(why)]),
            });
        }
        let request = match &route {
            Route::Op { fragment, op } => Ok((Method::Post, format!("/api/f/{fragment}/ops/{op}"), Some(json!({ "id": op_id(request_id), "input": args })))),
            Route::Platform(tool) => platform_request(tool, &args, request_id),
        };
        let (method, path, body) = match request {
            Ok(r) => r,
            Err(why) => return Ok(CallToolResult::error(vec![ContentBlock::text(why)])),
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
        let text = match &route {
            Route::Op { fragment, op } => self.op_answer(fragment, op, &answer).await,
            Route::Platform("platform__call") => self.op_answer(args["fragment"].as_str().unwrap_or(""), args["operation"].as_str().unwrap_or(""), &answer).await,
            Route::Platform(tool) => platform_answer(tool, answer),
        };
        let mut text = match text {
            Ok(t) => t,
            Err(why) => return Ok(CallToolResult::error(vec![ContentBlock::text(why)])),
        };
        if text.len() > RESULT_TEXT_MAX {
            text.truncate(text.floor_char_boundary(RESULT_TEXT_MAX));
            text.push_str(" …(truncated)");
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}
