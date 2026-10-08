//! MCP, the Model Context Protocol, as the platform serves it (docs/api.md,
//! A fragment's MCP server): JSON-RPC over Streamable HTTP, one request a
//! POST, answered with one JSON object. It speaks both eras of the spec:
//! the modern one (2026-07-28: no session, every request's version and
//! client in its `_meta`, its method and name mirrored in headers, and
//! `server/discover`), and the legacy one (2025-03-26 to 2025-11-25:
//! `initialize` first, no session id minted).
//!
//! A fragment's tools are its described operations, here for both of its
//! MCP servers, so they agree: its own `__mcp` (cell/src/mcp.rs, for a
//! connected client) and `fragment mcp` (cli/src/mcp.rs, over stdio). An
//! operation is a tool when its `fragment.json` entry has a `description`
//! (`served`): a query always, a mutation or a job only for a client its
//! person let change things (a connection allowed changes, `--write`). A
//! tool's arguments are the operation's input, and each call is a call of
//! its own (a fresh operation id).

use std::collections::BTreeMap;

use base64::Engine;
use fragment_proto::{OpDecl, OpKind};

pub mod verbs;
use serde_json::{json, Value};

/// The modern revision.
pub const MODERN: &str = "2026-07-28";
/// The legacy revisions, newest first: an `initialize` asking another is
/// answered with the newest.
pub const LEGACY: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
/// How long a client may keep a tool list (a deploy changes it).
pub const TOOLS_TTL_MS: u64 = 60_000;

/// JSON-RPC's and MCP's error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// The server's own: the fragment refused (a role, a fragment it may not see).
pub const REFUSED: i64 = -32000;
pub const HEADER_MISMATCH: i64 = -32020;
pub const UNSUPPORTED_VERSION: i64 = -32022;

const META_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_SERVER: &str = "io.modelcontextprotocol/serverInfo";

/// Which revision a request speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    Legacy,
    Modern,
}

/// The headers a request mirrors its body in (Streamable HTTP).
#[derive(Debug, Default, Clone, Copy)]
pub struct Headers<'a> {
    pub protocol_version: Option<&'a str>,
    pub method: Option<&'a str>,
    pub name: Option<&'a str>,
}

/// Who answers: an MCP server of the platform's.
#[derive(Debug, Clone)]
pub struct Server {
    pub name: String,
    pub title: String,
    pub version: String,
    pub instructions: String,
}

impl Server {
    fn info(&self) -> Value {
        json!({ "name": self.name, "title": self.title, "version": self.version })
    }
}

/// An answer: its HTTP status and its JSON-RPC message, or a
/// notification taken (202, no body).
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Json(u16, Value),
    Accepted,
}

/// A JSON-RPC request (an `id`) or notification (none).
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: Option<Value>,
    pub method: String,
    pub params: Value,
}

/// What a request asks its server's owner for, past the envelope.
#[derive(Debug, Clone, PartialEq)]
pub enum Asked {
    /// `tools/list`.
    Tools,
    /// `tools/call`: the tool (an operation's name) and its arguments.
    Call { name: String, arguments: Value },
}

/// A request's next step: answered here, or asked of the server's owner.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Done(Answer),
    Ask { era: Era, id: Value, asked: Asked },
}

/// A JSON-RPC error message.
pub fn error(id: &Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut e = json!({ "code": code, "message": message });
    if let Some(data) = data {
        e["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": e })
}

/// A JSON-RPC result, as `era` has it: a modern one says it is complete
/// and who answered.
pub fn result(era: Era, id: &Value, mut result: Value, server: &Server) -> Value {
    if era == Era::Modern {
        result["resultType"] = json!("complete");
        result["_meta"] = json!({ META_SERVER: server.info() });
    }
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A body, read as one JSON-RPC message (a batch is no longer one).
pub fn message(body: &[u8]) -> Result<Message, Answer> {
    let v: Value = serde_json::from_slice(body).map_err(|e| Answer::Json(400, error(&Value::Null, PARSE_ERROR, &format!("not JSON: {e}"), None)))?;
    let invalid = |why: &str| Answer::Json(400, error(&v["id"], INVALID_REQUEST, why, None));
    let o = v.as_object().ok_or_else(|| invalid("a request is one JSON-RPC object (no batches)"))?;
    if o.get("jsonrpc") != Some(&json!("2.0")) {
        return Err(invalid("jsonrpc is \"2.0\""));
    }
    let method = o.get("method").and_then(Value::as_str).ok_or_else(|| invalid("a request names its method"))?;
    let id = match o.get("id") {
        None => None,
        Some(id @ (Value::String(_) | Value::Number(_))) => Some(id.clone()),
        Some(_) => return Err(invalid("an id is a string or a number")),
    };
    let params = o.get("params").cloned().unwrap_or_else(|| json!({}));
    if !params.is_object() {
        return Err(invalid("params is an object"));
    }
    Ok(Message { id, method: method.to_string(), params })
}

/// A header value as the transport encodes it: plain, or
/// `=?base64?…?=` for one a header cannot carry.
fn decoded(value: &str) -> Option<String> {
    match value.strip_prefix("=?base64?").and_then(|v| v.strip_suffix("?=")) {
        Some(b64) => base64::engine::general_purpose::STANDARD.decode(b64).ok().and_then(|b| String::from_utf8(b).ok()),
        None => Some(value.to_string()),
    }
}

/// The modern era's checks: its version, and its headers matching its body.
fn modern(msg: &Message, id: &Value, version: &str, h: Headers<'_>) -> Result<(), Answer> {
    if version != MODERN {
        let supported: Vec<&str> = std::iter::once(MODERN).chain(LEGACY).collect();
        let data = json!({ "supported": supported, "requested": version });
        return Err(Answer::Json(400, error(id, UNSUPPORTED_VERSION, "Unsupported protocol version", Some(data))));
    }
    let mismatch = |why: String| Answer::Json(400, error(id, HEADER_MISMATCH, &format!("Header mismatch: {why}"), None));
    if h.protocol_version != Some(version) {
        return Err(mismatch(format!("MCP-Protocol-Version is {:?}, the body's {version:?}", h.protocol_version)));
    }
    if h.method != Some(msg.method.as_str()) {
        return Err(mismatch(format!("Mcp-Method is {:?}, the body's {:?}", h.method, msg.method)));
    }
    if msg.method == "tools/call" {
        let named = msg.params["name"].as_str();
        if named.is_none() || h.name.and_then(decoded).as_deref() != named {
            return Err(mismatch(format!("Mcp-Name is {:?}, the body's {named:?}", h.name)));
        }
    }
    Ok(())
}

/// A request's step: the envelope's methods answered (`initialize`,
/// `ping`, `server/discover`, notifications), refused (a version or a
/// header that does not fit, an unknown method), or asked of the owner.
pub fn step(msg: &Message, h: Headers<'_>, server: &Server) -> Step {
    let Some(id) = msg.id.clone() else {
        // a notification (the legacy `notifications/initialized`): taken
        return Step::Done(Answer::Accepted);
    };
    let era = match msg.params["_meta"][META_VERSION].as_str() {
        Some(version) => match modern(msg, &id, version, h) {
            Ok(()) => Era::Modern,
            Err(answer) => return Step::Done(answer),
        },
        None => {
            if h.protocol_version.is_some_and(|v| !LEGACY.contains(&v) && v != MODERN) {
                let data = json!({ "supported": LEGACY, "requested": h.protocol_version });
                return Step::Done(Answer::Json(400, error(&id, UNSUPPORTED_VERSION, "Unsupported protocol version", Some(data))));
            }
            Era::Legacy
        }
    };
    let done = |r: Value| Step::Done(Answer::Json(200, result(era, &id, r, server)));
    let capabilities = json!({ "tools": { "listChanged": false } });
    match (era, msg.method.as_str()) {
        (Era::Legacy, "initialize") => {
            let asked = msg.params["protocolVersion"].as_str().unwrap_or("");
            let version = LEGACY.iter().find(|v| **v == asked).unwrap_or(&LEGACY[0]);
            done(json!({ "protocolVersion": version, "capabilities": capabilities, "serverInfo": server.info(), "instructions": server.instructions }))
        }
        (_, "server/discover") => {
            let supported: Vec<&str> = std::iter::once(MODERN).chain(LEGACY).collect();
            done(json!({ "supportedVersions": supported, "capabilities": capabilities, "instructions": server.instructions, "ttlMs": TOOLS_TTL_MS, "cacheScope": "private" }))
        }
        (Era::Legacy, "ping") => done(json!({})),
        (_, "tools/list") => Step::Ask { era, id, asked: Asked::Tools },
        (_, "tools/call") => match msg.params["name"].as_str() {
            Some(name) => Step::Ask { era, id, asked: Asked::Call { name: name.to_string(), arguments: msg.params.get("arguments").cloned().unwrap_or(Value::Null) } },
            None => Step::Done(Answer::Json(200, error(&id, INVALID_PARAMS, "tools/call names its tool", None))),
        },
        (_, method) => {
            let status = if era == Era::Modern { 404 } else { 200 };
            Step::Done(Answer::Json(status, error(&id, METHOD_NOT_FOUND, &format!("Method not found: {method}"), None)))
        }
    }
}

/// The tool list's result, as `era` has it.
pub fn tools(era: Era, tools: Vec<Value>) -> Value {
    match era {
        Era::Modern => json!({ "tools": tools, "ttlMs": TOOLS_TTL_MS, "cacheScope": "private" }),
        Era::Legacy => json!({ "tools": tools }),
    }
}

/// An operation's input schema as a tool's: an object's (a tool's
/// arguments are one), `{"type": "object"}` when it declares none, and none
/// when it declares another type (such an operation is no tool).
pub fn input_schema(decl: &OpDecl) -> Option<Value> {
    let Some(Value::Object(schema)) = &decl.input else {
        return decl.input.is_none().then(|| json!({ "type": "object" }));
    };
    let mut schema = schema.clone();
    match schema.get("type") {
        None => {
            schema.insert("type".into(), "object".into());
        }
        Some(Value::String(t)) if t == "object" => {}
        Some(_) => return None,
    }
    Some(Value::Object(schema))
}

/// Whether an operation is a tool for a client that may change things
/// (`writes`) or only read: it says what it does (its `description`), it
/// takes an object, and it is a query unless the client may write.
pub fn served(decl: &OpDecl, writes: bool) -> bool {
    decl.description.is_some() && (decl.kind == OpKind::Query || writes) && input_schema(decl).is_some()
}

/// Why `op` is no tool for this client: the refusal of a call to it.
pub fn not_served(op: &str, decl: &OpDecl, writes: bool) -> String {
    assert!(!served(decl, writes), "{op} is a tool");
    if decl.description.is_none() {
        format!("{op} is no tool: an operation is one when its fragment.json entry has a description")
    } else if input_schema(decl).is_none() {
        format!("{op} is no tool: its input is no object, and a tool's arguments are one")
    } else {
        format!("{op} changes things, and this client may only read: its person connects it again and allows changes (fragment mcp: --write)")
    }
}

/// An operation as a tool: its arguments are its input, its schema the
/// operation's own. A query only reads; a mutation or a job changes
/// things, each call once (a fresh operation id: no call replays
/// another), and a job may reach the world (`job.fetch`, AI steps).
pub fn tool(name: &str, decl: &OpDecl) -> Value {
    let description = decl.description.as_deref().expect("a tool is a described operation (served)");
    let schema = input_schema(decl).expect("a tool takes an object (served)");
    let annotations = match decl.kind {
        OpKind::Query => json!({ "readOnlyHint": true, "openWorldHint": false }),
        OpKind::Mutation => json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false }),
        OpKind::Job => json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": true }),
    };
    json!({ "name": name, "description": description, "inputSchema": schema, "annotations": annotations })
}

/// The tools of these operations for a client that may change things
/// (`writes`) or only read: each served one its caller may call
/// (`may_call`: the caller's role there), in name order.
pub fn tools_of(operations: &BTreeMap<String, OpDecl>, writes: bool, may_call: impl Fn(&OpDecl) -> bool) -> Vec<Value> {
    operations.iter().filter(|(_, d)| served(d, writes) && may_call(d)).map(|(name, d)| tool(name, d)).collect()
}

/// What a fragment's MCP server tells its client of itself.
pub fn instructions(fragment: &str, writes: bool) -> String {
    let reach = if writes { "its queries read it, and its mutations and jobs change it, each call once" } else { "its queries alone: this client may only read it" };
    format!(
        "The operations of {fragment}, a fragment (a small web app with its own data), that say what they do, as tools: {reach}. A tool's arguments are its operation's input. Each call is made as the one who connected this server, with their role there."
    )
}

/// A tool call's arguments: the operation's input, an object (none is `{}`).
pub fn call_of(arguments: &Value) -> Result<Value, String> {
    match arguments {
        Value::Null => Ok(json!({})),
        Value::Object(_) => Ok(arguments.clone()),
        _ => Err("a tool's arguments are an object: its operation's input".into()),
    }
}

/// An operation's answer (`{result, replayed}`) as a tool's result: its
/// result's `text` as it is when that is a string (a view an operation
/// rendered for a model to read: a mind's view, zoom, date), else the
/// result as JSON; and the whole answer, structured.
pub fn called(answer: &Value) -> Value {
    let text = match &answer["result"]["text"] {
        Value::String(text) => text.clone(),
        _ => answer["result"].to_string(),
    };
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": answer, "isError": false })
}

/// A route's answer as a tool's result (the platform's verbs): as JSON,
/// and structured.
pub fn answered(answer: &Value) -> Value {
    json!({ "content": [{ "type": "text", "text": answer.to_string() }], "structuredContent": answer, "isError": false })
}

/// A call the fragment refused, as a tool's result the model may act on
/// (the platform's error code and why).
pub fn refused(code: &str, message: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": format!("{code}: {message}") }], "isError": true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_proto::Role;

    fn server() -> Server {
        Server { name: "fragment".into(), title: "todo.paul".into(), version: "dev".into(), instructions: "its operations".into() }
    }

    fn msg(v: Value) -> Message {
        message(v.to_string().as_bytes()).unwrap()
    }

    fn modern_meta() -> Value {
        json!({ META_VERSION: MODERN, "io.modelcontextprotocol/clientInfo": { "name": "c", "version": "1" }, "io.modelcontextprotocol/clientCapabilities": {} })
    }

    #[test]
    fn a_message_is_one_jsonrpc_object() {
        assert!(matches!(message(b"{"), Err(Answer::Json(400, e)) if e["error"]["code"] == PARSE_ERROR));
        for bad in [json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }]), json!({ "id": 1, "method": "ping" }), json!({ "jsonrpc": "2.0", "id": {}, "method": "ping" }), json!({ "jsonrpc": "2.0", "id": 1, "method": "ping", "params": [] })] {
            assert!(matches!(message(bad.to_string().as_bytes()), Err(Answer::Json(400, e)) if e["error"]["code"] == INVALID_REQUEST), "{bad}");
        }
        assert_eq!(msg(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).id, None);
    }

    #[test]
    fn a_legacy_client_initializes_and_lists() {
        let s = server();
        let init = msg(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "c", "version": "1" } } }));
        let Step::Done(Answer::Json(200, r)) = step(&init, Headers::default(), &s) else { panic!("initialize is answered") };
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["title"], "todo.paul");
        assert!(r["result"]["capabilities"]["tools"].is_object() && r["result"].get("resultType").is_none());
        let newer = msg(json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2099-01-01" } }));
        let Step::Done(Answer::Json(200, r)) = step(&newer, Headers::default(), &s) else { panic!() };
        assert_eq!(r["result"]["protocolVersion"], LEGACY[0], "an unknown version is answered with the newest legacy one");
        assert_eq!(step(&msg(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })), Headers::default(), &s), Step::Done(Answer::Accepted));
        let list = msg(json!({ "jsonrpc": "2.0", "id": "a", "method": "tools/list" }));
        let h = Headers { protocol_version: Some("2025-06-18"), ..Headers::default() };
        assert_eq!(step(&list, h, &s), Step::Ask { era: Era::Legacy, id: json!("a"), asked: Asked::Tools });
        let h = Headers { protocol_version: Some("1999-01-01"), ..Headers::default() };
        assert!(matches!(step(&list, h, &s), Step::Done(Answer::Json(400, e)) if e["error"]["code"] == UNSUPPORTED_VERSION));
        let Step::Done(Answer::Json(200, r)) = step(&msg(json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" })), Headers::default(), &s) else { panic!() };
        assert_eq!(r["result"], json!({}));
        let Step::Done(Answer::Json(200, r)) = step(&msg(json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" })), Headers::default(), &s) else { panic!() };
        assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
    }

    #[test]
    fn a_modern_request_carries_its_version_and_mirrors_its_headers() {
        let s = server();
        let call = msg(json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "add", "arguments": { "id": "a1", "input": {} }, "_meta": modern_meta() } }));
        let ok = Headers { protocol_version: Some(MODERN), method: Some("tools/call"), name: Some("add") };
        assert_eq!(step(&call, ok, &s), Step::Ask { era: Era::Modern, id: json!(7), asked: Asked::Call { name: "add".into(), arguments: json!({ "id": "a1", "input": {} }) } });
        let encoded = Headers { name: Some("=?base64?YWRk?="), ..ok };
        assert!(matches!(step(&call, encoded, &s), Step::Ask { .. }), "a base64 name is decoded before it is compared");
        for bad in [Headers { method: None, ..ok }, Headers { name: Some("remove"), ..ok }, Headers { protocol_version: Some("2025-11-25"), ..ok }, Headers { method: Some("tools/list"), ..ok }] {
            assert!(matches!(step(&call, bad, &s), Step::Done(Answer::Json(400, e)) if e["error"]["code"] == HEADER_MISMATCH), "{bad:?}");
        }
        let old = msg(json!({ "jsonrpc": "2.0", "id": 8, "method": "tools/list", "params": { "_meta": { META_VERSION: "2027-01-01" } } }));
        let Step::Done(Answer::Json(400, e)) = step(&old, ok, &s) else { panic!("an unknown modern version is refused") };
        assert_eq!(e["error"]["code"], UNSUPPORTED_VERSION);
        assert_eq!(e["error"]["data"]["supported"][0], MODERN);
        let discover = msg(json!({ "jsonrpc": "2.0", "id": 9, "method": "server/discover", "params": { "_meta": modern_meta() } }));
        let Step::Done(Answer::Json(200, r)) = step(&discover, Headers { method: Some("server/discover"), ..ok }, &s) else { panic!() };
        assert_eq!(r["result"]["resultType"], "complete");
        assert_eq!(r["result"]["_meta"][META_SERVER]["name"], "fragment");
        assert!(r["result"]["supportedVersions"].as_array().is_some_and(|v| v.contains(&json!(MODERN)) && v.contains(&json!(LEGACY[0]))));
        let unknown = msg(json!({ "jsonrpc": "2.0", "id": 10, "method": "resources/list", "params": { "_meta": modern_meta() } }));
        assert!(matches!(step(&unknown, Headers { method: Some("resources/list"), ..ok }, &s), Step::Done(Answer::Json(404, e)) if e["error"]["code"] == METHOD_NOT_FOUND));
        let ping = msg(json!({ "jsonrpc": "2.0", "id": 11, "method": "ping", "params": { "_meta": modern_meta() } }));
        assert!(matches!(step(&ping, Headers { method: Some("ping"), ..ok }, &s), Step::Done(Answer::Json(404, _))), "ping went in 2026-07-28");
    }

    fn op(kind: OpKind, role: Role, input: Option<Value>, description: Option<&str>) -> OpDecl {
        OpDecl { kind, role, input, ephemeral: false, description: description.map(str::to_string) }
    }

    /// Goal: one rule says which operations are tools, for both servers
    /// (`__mcp` and `fragment mcp`). Method: described or not, each kind,
    /// read-only and allowed changes, an input that is no object, and the
    /// caller's role; each refusal says why.
    #[test]
    fn a_described_operation_is_a_tool_and_one_that_writes_needs_changes_allowed() {
        let object = json!({ "type": "object", "required": ["text"], "properties": { "text": { "type": "string" } } });
        let ops: BTreeMap<String, OpDecl> = [
            ("list", op(OpKind::Query, Role::Viewer, None, Some("The list."))),
            ("count", op(OpKind::Query, Role::Viewer, None, None)),
            ("add", op(OpKind::Mutation, Role::Editor, Some(object.clone()), Some("Adds one."))),
            ("digest", op(OpKind::Job, Role::Editor, None, Some("Sums it up."))),
            ("shout", op(OpKind::Query, Role::Viewer, Some(json!({ "type": "string" })), Some("Takes a string."))),
            ("secret", op(OpKind::Query, Role::Owner, None, Some("The owner's."))),
        ]
        .into_iter()
        .map(|(n, d)| (n.to_string(), d))
        .collect();
        let names = |writes: bool, role: Role| -> Vec<String> { tools_of(&ops, writes, |d| d.role <= role).iter().map(|t| t["name"].as_str().unwrap().to_string()).collect() };
        assert_eq!(names(false, Role::Editor), ["list"], "read-only: the described queries that take an object");
        assert_eq!(names(true, Role::Editor), ["add", "digest", "list"], "allowed changes: its described mutations and jobs too");
        assert_eq!(names(true, Role::Viewer), ["list"], "the caller's role bounds them");
        assert_eq!(names(false, Role::Owner), ["list", "secret"]);
        assert!(not_served("count", &ops["count"], true).contains("description"));
        assert!(not_served("add", &ops["add"], false).contains("may only read"));
        assert!(not_served("shout", &ops["shout"], true).contains("no object"));
        assert!(instructions("todo.paul", false).contains("may only read") && instructions("todo.paul", true).contains("change it"));
    }

    #[test]
    fn an_operation_is_a_tool_whose_arguments_are_its_input() {
        let input = json!({ "type": "object", "required": ["text"], "properties": { "text": { "type": "string" } } });
        let q = tool("list", &op(OpKind::Query, Role::Viewer, None, Some("The list.")));
        assert_eq!(q["annotations"]["readOnlyHint"], true);
        assert_eq!(q["inputSchema"], json!({ "type": "object" }), "no input declared: any object");
        let m = tool("add", &op(OpKind::Mutation, Role::Editor, Some(input.clone()), Some("Adds one.")));
        assert_eq!(m["description"], "Adds one.");
        assert_eq!((m["annotations"]["readOnlyHint"].clone(), m["annotations"]["idempotentHint"].clone()), (json!(false), json!(false)), "each call runs once");
        assert_eq!(m["inputSchema"], input, "the operation's schema, as it is");
        let typeless = tool("zoom", &op(OpKind::Query, Role::Viewer, Some(json!({ "properties": { "n": { "type": "integer" } } })), Some("z")));
        assert_eq!(typeless["inputSchema"]["type"], "object", "a schema that names no type says it is an object's");
        let j = tool("digest", &op(OpKind::Job, Role::Editor, None, Some("d")));
        assert_eq!((j["annotations"]["readOnlyHint"].clone(), j["annotations"]["openWorldHint"].clone()), (json!(false), json!(true)));
    }

    #[test]
    fn a_calls_arguments_are_its_input_and_its_result_reads_as_text() {
        assert_eq!(call_of(&json!({ "text": "x" })), Ok(json!({ "text": "x" })));
        assert_eq!(call_of(&Value::Null), Ok(json!({})), "none is the empty object");
        assert!(call_of(&json!([1])).is_err());
        assert!(call_of(&json!(3)).is_err());
        let r = called(&json!({ "result": { "n": 1 }, "replayed": false }));
        assert_eq!((r["isError"].clone(), r["structuredContent"]["result"]["n"].clone()), (json!(false), json!(1)));
        assert_eq!(r["content"][0]["text"], r#"{"n":1}"#, "the result as JSON");
        let view = called(&json!({ "result": { "text": "<chat>\n0+1|user: hi\n</chat>", "bytes": 30 }, "replayed": false }));
        assert_eq!(view["content"][0]["text"], "<chat>\n0+1|user: hi\n</chat>", "a rendered text as it is");
        assert_eq!(view["structuredContent"]["result"]["bytes"], 30);
        assert_eq!(called(&json!({ "result": { "text": 7 }, "replayed": false }))["content"][0]["text"], r#"{"text":7}"#, "a text that is no string is data");
        assert_eq!(answered(&json!({ "fragments": [] }))["content"][0]["text"], r#"{"fragments":[]}"#);
        assert_eq!(refused("forbidden", "this needs the editor role")["content"][0]["text"], "forbidden: this needs the editor role");
    }
}
