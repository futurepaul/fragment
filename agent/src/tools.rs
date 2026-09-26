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
//! fragments the asker reaches, read one's operations, call one, list,
//! read, and write its files, and deploy it, each the signed API the CLI
//! uses; and, in its owner's turns only, make a fragment for the owner (an
//! app). A fragment's own agent (`Scope`) has neither kind but one: the
//! operations of its fragment that its block names. A file write's key comes from the tool-call id, so a replayed step
//! commits nothing twice.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use fragment_proto::{valid_fragment_name, valid_op_name, FragmentList, FragmentStatus, ListedFragment, OpDecl, OpKind, OpResult};
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
use crate::js;
use crate::model::{APPEND_FILE, CUT_OFF, WRITE_FILE};
use crate::store::{chat_of, kv_u64, Session};

/// The fragments, and the tools, one agent's turn considers at most.
pub const FRAGMENTS_MAX: usize = 16;
pub const TOOLS_MAX: usize = 128;
/// Status reads in flight at once while the catalog is read: 16 fragments
/// are three waves of round trips before the first model token, not 16.
const STATUS_READS_AT_ONCE: usize = 6;
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

/// The platform's verbs: (name, description, input schema). The first is
/// offered in its owner's turns only.
fn platform_tools(owner_turn: bool) -> Vec<(&'static str, &'static str, Value)> {
    let fragment = json!({ "type": "string", "description": "the fragment's full name, <label>.<username>" });
    let create = (
        "platform__create_fragment",
        "Makes a new fragment (an app, a page, a list) for your owner, named <label>.<their username>, from a \
         template: blank (one page), todo (a live list: a working example of an app), inbox, or chat. You become \
         its editor. Answers its name and URL. How to build what goes in it is in your instructions.",
        json!({ "type": "object", "required": ["label"], "additionalProperties": false, "properties": {
            "label": { "type": "string", "description": "lowercase letters, digits, and single dashes" },
            "template": { "type": "string", "enum": ["blank", "todo", "inbox", "chat"] },
        } }),
    );
    let mut tools = if owner_turn { vec![create] } else { Vec::new() };
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
        (
            WRITE_FILE,
            "Writes one file of a fragment (site/index.html, say), replacing what it held. Keep it under 150 lines: \
             add the rest of a longer file with platform__append_file. Nothing changes for its visitors until you deploy.",
            json!({ "type": "object", "required": ["fragment", "path", "text"], "additionalProperties": false, "properties": {
                "fragment": fragment, "path": { "type": "string" }, "text": { "type": "string" },
            } }),
        ),
        (
            APPEND_FILE,
            "Adds text to the end of one file of a fragment (making it if it is not there): a long file is written in \
             parts, each under 150 lines. Nothing changes for its visitors until you deploy.",
            json!({ "type": "object", "required": ["fragment", "path", "text"], "additionalProperties": false, "properties": {
                "fragment": fragment, "path": { "type": "string" }, "text": { "type": "string" },
            } }),
        ),
        (
            "platform__write_files",
            "Writes several files to a fragment in one commit (at most 16 files and 256 KiB). A file whose text is null is \
             removed. For one file, platform__write_file. Nothing changes for its visitors until you deploy.",
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
            (Method::Post, format!("/api/f/{}/files", fragment()?), Some(body))
        }
        WRITE_FILE => {
            let files = [json!({ "path": text("path")?, "text": text("text")? })];
            let body = json!({ "files": files, "message": "written by an agent", "key": op_id(request_id) });
            (Method::Post, format!("/api/f/{}/files", fragment()?), Some(body))
        }
        "platform__deploy" => (Method::Post, format!("/api/f/{}/deploy", fragment()?), Some(json!({ "note": args["note"] }))),
        other => return Err(format!("no tool named {other}")),
    })
}

/// The last characters of a cut-off file the model is shown, to continue from.
const CUT_TAIL_CHARS: usize = 160;

/// A file write's answer: its size now, and, for a write cut off at the
/// model's output limit (model.rs), where the file stops and what to do.
fn file_written(args: &Value, written: &str, commit: &str) -> String {
    let path = args["path"].as_str().unwrap_or("");
    if args[CUT_OFF] != true {
        return json!({ "path": path, "bytes": written.len(), "commit": serde_json::from_str::<Value>(commit).unwrap_or_default()["commit"] }).to_string();
    }
    let tail: String = {
        let chars: Vec<char> = written.chars().collect();
        chars[chars.len().saturating_sub(CUT_TAIL_CHARS)..].iter().collect()
    };
    format!(
        "Cut off: your reply reached its output limit inside {path}, so it holds what you wrote so far ({} bytes), ending \
         with:\n{tail}\nContinue it now: call {APPEND_FILE} with fragment {}, path {path}, and the rest of the file, starting \
         right after that. Nothing is deployed yet.",
        written.len(),
        args["fragment"].as_str().unwrap_or(""),
    )
}

/// A platform verb's answer, as the model reads it.
fn platform_answer(tool: &str, answer: Value) -> Result<String, String> {
    Ok(match (tool, answer) {
        ("platform__operations", answer) => {
            let status = FragmentStatus::deserialize(&answer).map_err(|e| format!("the fragment's status: {e}"))?;
            json!({ "fragment": status.name, "role": status.role, "operations": status.code.operations }).to_string()
        }
        ("platform__call", answer) => OpResult::deserialize(&answer).map_err(|e| format!("the fragment's answer is not an operation's result: {e}"))?.result.to_string(),
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

    /// An append's write: the file as it is now (none yet is empty), with
    /// the text added, in one commit keyed by the call (a replayed call
    /// reads the appended file, and the key answers the first commit).
    async fn appended(&self, args: &Value, request_id: &str) -> Result<(Method, String, Option<Value>), String> {
        let text = |k: &str| args[k].as_str().map(str::to_string).ok_or_else(|| format!("{k} is required"));
        let (fragment, path, more) = (text("fragment")?, text("path")?, text("text")?);
        if !valid_fragment_name(&fragment) {
            return Err(format!("{fragment:?} is not a fragment's name (<label>.<username>)"));
        }
        let mut u = worker::Url::parse("https://q/").expect("a URL");
        u.query_pairs_mut().append_pair("path", &path);
        let at = format!("/api/f/{fragment}/file?{}", u.query().unwrap_or_default());
        let fleet = self.fleet.clone();
        let (status, bytes) = SendFuture::new(async move { fleet.call_raw(Method::Get, &at, None).await }).await.map_err(|e| e.to_string())?;
        let now = match status {
            200 => String::from_utf8(bytes).map_err(|_| format!("{path} is not text: write it whole with {WRITE_FILE}"))?,
            404 => String::new(),
            _ => return Err(format!("reading {path}: {status}: {}", fleet::message(&serde_json::from_slice(&bytes).unwrap_or_default()))),
        };
        let files = [json!({ "path": path, "text": now + &more })];
        let body = json!({ "files": files, "message": "written by an agent", "key": op_id(request_id) });
        Ok((Method::Post, format!("/api/f/{fragment}/files"), Some(body)))
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
        let request = match &route {
            Route::Op { fragment, op } => Ok((Method::Post, format!("/api/f/{fragment}/ops/{op}"), Some(json!({ "id": op_id(request_id), "input": args })))),
            Route::Platform(APPEND_FILE) => self.appended(&args, request_id).await,
            Route::Platform(tool) => platform_request(tool, &args, request_id),
        };
        let (method, path, body) = match request {
            Ok(r) => r,
            Err(why) => return Ok(CallToolResult::error(vec![ContentBlock::text(why)])),
        };
        // a file write's whole new text (an append's too), for its answer
        let written = match &route {
            Route::Platform(WRITE_FILE | APPEND_FILE) => body.as_ref().and_then(|b| b["files"][0]["text"].as_str()).map(str::to_string),
            _ => None,
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
            Route::Op { .. } => OpResult::deserialize(&answer).map(|done| done.result.to_string()).map_err(|e| format!("the fragment's answer is not an operation's result: {e}")),
            Route::Platform(tool) => platform_answer(tool, answer),
        };
        let mut text = match text {
            Ok(t) => t,
            Err(why) => return Ok(CallToolResult::error(vec![ContentBlock::text(why)])),
        };
        if let Some(written) = written {
            text = file_written(&args, &written, &text);
        }
        if text.len() > RESULT_TEXT_MAX {
            text.truncate(text.floor_char_boundary(RESULT_TEXT_MAX));
            text.push_str(" …(truncated)");
        }
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }
}
