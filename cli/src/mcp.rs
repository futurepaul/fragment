//! `fragment mcp <fragment> [--write]`: a fragment's described operations
//! as an MCP server's tools, over stdio (cli/GUIDE.md, "Your operations as
//! tools"; docs/api.md, "Operations as MCP tools"). JSON-RPC 2.0, one
//! message a line each way. Stdout carries the protocol's messages and
//! nothing else: a failure to start goes to stderr (main.rs).
//!
//! A tool is an operation whose `fragment.json` entry has a `description`,
//! that the caller's role may call, and whose input is an object (a tool's
//! arguments are one): a query always, and a mutation or a job only with
//! `--write`, since they change state (`OpKind`: master's one mark of a
//! read). Its name and description are the operation's, and its input
//! schema the operation's (`{"type": "object"}` when it declares none).
//! A call's arguments are the operation's input, flat: each call is
//! `POST /api/f/<fragment>/ops/<op>` with a fresh id, through the CLI's
//! own client, so this machine's key signs it, or inside a computer the
//! computer's egress signs it as the agent (`FRAGMENT_AS_AGENT`). A call
//! the platform refuses is the tool's error, carrying the platform's own
//! error (its `ErrorBody`: code, status and message).

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};

use anyhow::Result;
use fragment_proto::{ErrorBody, FragmentStatus, OpCall, OpDecl, OpKind, OpResult, Role};
use serde_json::{json, Map, Value};

use crate::api::{Client, Code, CodedError, Resp};

/// The revisions this server answers in kind, newest first: it serves
/// tools alone, whose messages they share. A client offering another is
/// answered with the newest, and chooses whether to go on.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// One message, at most: an operation's input is at most 256 KiB
/// (`limits::INPUT_MAX_BYTES`), and the rest of a call is small. A longer
/// line is read to its end and refused, and the session goes on.
pub const MESSAGE_MAX_BYTES: usize = 1024 * 1024;

// JSON-RPC's error codes (MCP answers an unknown tool with -32602).
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// An error answered as JSON-RPC's `error`, with the platform's (or the
/// CLI's) code in its `data` when there is one.
#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into(), data: None }
    }
}

/// Why an operation the fragment declares is no tool of this server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotTool {
    /// Its manifest entry says nothing of what it does.
    Undescribed,
    /// It changes state, and this server only reads (no `--write`).
    Writes,
    /// It needs a role the caller does not hold there.
    Role(Role),
    /// Its input is not an object, and a tool's arguments are one.
    NotObject,
}

impl NotTool {
    fn why(self, op: &str, role: Role) -> String {
        match self {
            NotTool::Undescribed => format!("{op} is no tool: an operation is one when its fragment.json entry has a description"),
            NotTool::Writes => format!("{op} changes state, and this server only reads: start it with --write (`fragment mcp <fragment> --write`) to call it"),
            NotTool::Role(needs) => format!("{op} needs the {} role, and you are a {} here", needs.as_str(), role.as_str()),
            NotTool::NotObject => format!("{op} is no tool: its input is not an object, and a tool's arguments are one"),
        }
    }
}

/// An operation's input schema as a tool's: the operation's own when it
/// is an object's (one naming no type is made to say so), `{"type":
/// "object"}` when it declares none, and none when it is another type's.
pub fn input_schema(decl: &OpDecl) -> Option<Value> {
    let Some(input) = &decl.input else { return Some(json!({ "type": "object" })) };
    let Value::Object(schema) = input else { return None };
    match schema.get("type") {
        None => {
            let mut schema = schema.clone();
            schema.insert("type".into(), json!("object"));
            Some(Value::Object(schema))
        }
        Some(t) if t == "object" => Some(input.clone()),
        Some(_) => None,
    }
}

/// `op` as a tool for a caller holding `role`, served read-only or with
/// `write`: its definition, or why it is none.
pub fn tool(op: &str, decl: &OpDecl, role: Role, write: bool) -> Result<Value, NotTool> {
    let Some(description) = &decl.description else { return Err(NotTool::Undescribed) };
    if decl.kind != OpKind::Query && !write {
        return Err(NotTool::Writes);
    }
    if decl.role > role {
        return Err(NotTool::Role(decl.role));
    }
    let schema = input_schema(decl).ok_or(NotTool::NotObject)?;
    assert!(schema["type"] == "object", "a tool's arguments are an object");
    // a query reads; a mutation or a job changes state, each call once (a
    // fresh id), and a job may reach the world (`job.fetch`, AI steps)
    let annotations = match decl.kind {
        OpKind::Query => json!({ "readOnlyHint": true, "openWorldHint": false }),
        OpKind::Mutation => json!({ "readOnlyHint": false, "idempotentHint": false, "openWorldHint": false }),
        OpKind::Job => json!({ "readOnlyHint": false, "idempotentHint": false, "openWorldHint": true }),
    };
    Ok(json!({ "name": op, "description": description, "inputSchema": schema, "annotations": annotations }))
}

/// The tools of a fragment's operations, in name order.
pub fn tools(operations: &BTreeMap<String, OpDecl>, role: Role, write: bool) -> Vec<Value> {
    operations.iter().filter_map(|(op, d)| tool(op, d, role, write).ok()).collect()
}

/// An operation's answer as a tool's result: its result as JSON text, and
/// structured too when it is an object (MCP's structured content is one).
fn called(result: &Value) -> Value {
    let text = serde_json::to_string(result).expect("a JSON value serializes");
    let mut answer = json!({ "content": [{ "type": "text", "text": text }], "isError": false });
    if result.is_object() {
        answer["structuredContent"] = result.clone();
    }
    answer
}

/// A call that did not run, or that the fragment refused, as a tool's
/// error the model can act on: its error (`Failed`) and the operation id
/// (`fragment call … --id <id>` replays it, and never runs it twice).
fn refused(f: &Failed, id: &str) -> Value {
    let replay = match f.code.as_str() {
        "outcome_unknown" => format!(" It may have run: `fragment call` it with --id {id} to find out, which never runs it twice."),
        _ => String::new(),
    };
    let text = format!("{}: {} (operation id {id}).{replay}", f.code, f.message);
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": { "error": f.json(), "id": id }, "isError": true })
}

impl Failed {
    /// `{code, status?, message}`.
    fn json(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let Some(status) = self.status {
            error["status"] = json!(status);
        }
        error
    }
}

/// What a request that did not succeed carries: an error code, the answer's
/// status when one came, and why.
struct Failed {
    code: String,
    status: Option<u16>,
    message: String,
}

/// A request's answer, or what it failed with: the platform's own error
/// (its `ErrorBody`) when it refused, else the CLI's code (an answer that is
/// not the platform's names its status's; no answer, `unavailable` or
/// `outcome_unknown`; a bare label that names nothing, `not_found`).
fn answered(answer: Result<Resp>) -> Result<Vec<u8>, Failed> {
    match answer {
        Ok(r) if r.ok() => Ok(r.body),
        Ok(r) => Err(match serde_json::from_slice::<ErrorBody>(&r.body) {
            Ok(body) => {
                let code = serde_json::to_value(body.error).expect("an error code serializes");
                let code = code.as_str().expect("an error code serializes as a string").to_string();
                Failed { code, status: Some(r.status), message: body.message }
            }
            Err(_) => Failed { code: Code::of_status(r.status).as_str().into(), status: Some(r.status), message: "the answer is not the platform's".into() },
        }),
        Err(e) => {
            let code = e.downcast_ref::<CodedError>().map_or(Code::ServerError, |c| c.code);
            Err(Failed { code: code.as_str().into(), status: None, message: format!("{e:#}") })
        }
    }
}

/// The server: the fragment it serves, and what it last read of it.
pub struct Server<'a> {
    client: &'a Client,
    /// As the command named it (a bare label resolves at each request).
    fragment: String,
    write: bool,
    operations: BTreeMap<String, OpDecl>,
    role: Role,
}

/// Serves `fragment`'s tools to the client on `input` and `output` until
/// the client closes `input`.
pub fn serve(client: &Client, fragment: &str, write: bool, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    let mut server = Server { client, fragment: fragment.to_string(), write, operations: BTreeMap::new(), role: Role::Public };
    let mut line = Vec::new();
    // one message a line, until the client closes its end
    loop {
        let answer = match read_line(&mut input, &mut line)? {
            Line::End => return Ok(()),
            Line::TooLong => Some(failure(Value::Null, RpcError::new(INVALID_REQUEST, format!("a message is at most {MESSAGE_MAX_BYTES} bytes")))),
            Line::Text if line.iter().all(u8::is_ascii_whitespace) => None,
            Line::Text => match serde_json::from_slice::<Value>(&line) {
                Ok(message) => server.handle(message),
                Err(e) => Some(failure(Value::Null, RpcError::new(PARSE_ERROR, format!("not JSON: {e}")))),
            },
        };
        if let Some(answer) = answer {
            let mut text = serde_json::to_vec(&answer).expect("a JSON value serializes");
            assert!(!text.contains(&b'\n'), "a message is one line");
            text.push(b'\n');
            output.write_all(&text)?;
            output.flush()?;
        }
    }
}

enum Line {
    Text,
    TooLong,
    End,
}

/// The next line into `buf`, at most `MESSAGE_MAX_BYTES` of it: a longer
/// one is read to its end, 64 KiB at a time, and dropped.
fn read_line(input: &mut impl BufRead, buf: &mut Vec<u8>) -> std::io::Result<Line> {
    buf.clear();
    let limit = u64::try_from(MESSAGE_MAX_BYTES).expect("the limit fits") + 1;
    if input.by_ref().take(limit).read_until(b'\n', buf)? == 0 {
        return Ok(Line::End);
    }
    if buf.last() == Some(&b'\n') || buf.len() <= MESSAGE_MAX_BYTES {
        return Ok(Line::Text);
    }
    let mut rest = Vec::new();
    // bounded by the client's line: each pass reads at most 64 KiB of it
    loop {
        rest.clear();
        if input.by_ref().take(64 * 1024).read_until(b'\n', &mut rest)? == 0 || rest.last() == Some(&b'\n') {
            return Ok(Line::TooLong);
        }
    }
}

fn success(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn failure(id: Value, e: RpcError) -> Value {
    let mut error = json!({ "code": e.code, "message": e.message });
    if let Some(data) = e.data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

impl Server<'_> {
    /// The answer to one message: none for a notification, or for a
    /// client's answer (this server asks nothing of a client).
    fn handle(&mut self, message: Value) -> Option<Value> {
        let Value::Object(mut message) = message else {
            return Some(failure(Value::Null, RpcError::new(INVALID_REQUEST, "a message is one JSON-RPC object (no batches)")));
        };
        let id = match message.remove("id") {
            None => None,
            Some(id @ (Value::String(_) | Value::Number(_))) => Some(id),
            Some(_) => return Some(failure(Value::Null, RpcError::new(INVALID_REQUEST, "an id is a string or a number"))),
        };
        let Some(method) = message.get("method").and_then(Value::as_str).map(str::to_string) else {
            return match id {
                Some(id) if !message.contains_key("result") && !message.contains_key("error") => Some(failure(id, RpcError::new(INVALID_REQUEST, "a request names its method"))),
                _ => None,
            };
        };
        let params = message.remove("params").unwrap_or(Value::Null);
        // notifications (initialized, cancelled): nothing to answer
        let id = id?;
        Some(match self.request(&method, &params) {
            Ok(result) => success(id, result),
            Err(e) => failure(id, e),
        })
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => {
                let offered = params.get("protocolVersion").and_then(Value::as_str);
                let version = offered.filter(|v| PROTOCOL_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSIONS[0]);
                self.read()?;
                let full = self.client.fragment(&self.fragment).unwrap_or_else(|_| self.fragment.clone());
                let reach = match self.write {
                    true => "its queries read it, and its mutations and jobs change it",
                    false => "its queries alone, which read it (`fragment mcp` with --write serves its mutations and jobs too)",
                };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "fragment", "title": full, "version": env!("CARGO_PKG_VERSION") },
                    "instructions": format!(
                        "The operations of {full}, a fragment (a small web app with its own data), that say what they do, as tools: {reach}. A tool's arguments are its operation's input, and each call is one call, made as you, with your role there ({}).",
                        self.role.as_str()
                    ),
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                self.read()?;
                Ok(json!({ "tools": tools(&self.operations, self.role, self.write) }))
            }
            "tools/call" => self.call(params),
            other => Err(RpcError::new(METHOD_NOT_FOUND, format!("no method {other:?}: this server serves tools (initialize, ping, tools/list, tools/call)"))),
        }
    }

    /// Reads the fragment's operations and the caller's role there again: a
    /// deploy changes them.
    fn read(&mut self) -> Result<(), RpcError> {
        let path = format!("/api/f/{}/status", self.fragment);
        let read = answered(self.client.get(&path)).and_then(|body| {
            serde_json::from_slice::<FragmentStatus>(&body).map_err(|e| Failed { code: Code::ServerError.as_str().into(), status: None, message: format!("the platform's answer is no status: {e}") })
        });
        let status = read.map_err(|f| RpcError { code: INTERNAL_ERROR, message: format!("reading {}'s operations: {}", self.fragment, f.message), data: Some(f.json()) })?;
        self.operations = status.code.operations;
        self.role = status.role;
        Ok(())
    }

    fn call(&mut self, params: &Value) -> Result<Value, RpcError> {
        let name = params.get("name").and_then(Value::as_str).ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call names its tool (params.name)"))?;
        let input = match params.get("arguments") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(args @ Value::Object(_)) => args.clone(),
            Some(_) => return Err(RpcError::new(INVALID_PARAMS, "a tool's arguments are an object: its operation's input")),
        };
        // a tool the client knows and this server does not: deployed since
        if !self.operations.contains_key(name) {
            self.read()?;
        }
        let Some(decl) = self.operations.get(name) else {
            return Err(RpcError::new(INVALID_PARAMS, format!("no tool {name:?}: {} has no such operation (tools/list lists its tools)", self.fragment)));
        };
        if let Err(not) = tool(name, decl, self.role, self.write) {
            return Err(RpcError::new(INVALID_PARAMS, not.why(name, self.role)));
        }
        assert!(decl.kind == OpKind::Query || self.write, "a tool that writes is served only with --write");
        // a fresh id: each call is an action of its own, and the client's
        // retries of one request reuse it, so it runs once
        let id = format!("mcp-{:016x}", rand::random::<u64>());
        assert!(fragment_proto::valid_op_id(&id), "a call's id is one the platform takes");
        let path = format!("/api/f/{}/ops/{name}", self.fragment);
        let answer = answered(self.client.post_json_by_id(&path, &OpCall { id: id.clone(), input })).and_then(|body| {
            serde_json::from_slice::<OpResult>(&body).map_err(|e| Failed { code: Code::ServerError.as_str().into(), status: None, message: format!("the platform's answer is no operation's result: {e}") })
        });
        Ok(match answer {
            Ok(done) => called(&done.result),
            Err(f) => refused(&f, &id),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_fakes::http::{Request, Response, Server as FakeHost};
    use std::sync::{Arc, Mutex};

    const NAME: &str = "todo--k3x9";

    /// A fragment's status as a host answers it, the caller holding `role`.
    fn status(role: &str) -> Value {
        json!({
            "name": NAME, "npub": "n", "owner": "id:p", "role": role, "visibility": "members", "repo": "r",
            "pins": { "main": "a", "live": "a" }, "counts": { "files": 1, "events": 0, "members": 2 },
            "code": { "sha": "a", "id": "app:x", "error": null, "operations": {
                "list":   { "kind": "query", "role": "viewer", "description": "Every todo." },
                "find":   { "kind": "query", "role": "viewer", "description": "The todos with this text.",
                            "input": { "required": ["q"], "properties": { "q": { "type": "string" } } } },
                "count":  { "kind": "query", "role": "viewer" },
                "secret": { "kind": "query", "role": "owner", "description": "The owner's own." },
                "shout":  { "kind": "query", "role": "viewer", "description": "Takes a string.", "input": { "type": "string" } },
                "add":    { "kind": "mutation", "role": "editor", "description": "Add a todo.",
                            "input": { "type": "object", "required": ["text"], "properties": { "text": { "type": "string" } } } },
                "remove": { "kind": "mutation", "role": "editor" },
                "digest": { "kind": "job", "role": "editor", "description": "Sum it up." },
            } },
            "viewToken": null, "inboxToken": null, "urls": { "canonical": "https://todo--k3x9.x/", "platform": "https://x" },
        })
    }

    /// Each request the host saw: method, path, body, and its signature's scheme.
    type Seen = Arc<Mutex<Vec<(String, String, Value, Option<String>)>>>;

    /// A host serving the fragment, the caller holding `role`: its
    /// status, and each operation as its app would answer it.
    fn host(role: &'static str) -> (FakeHost, Seen) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let host = FakeHost::start(
            0,
            Arc::new(move |r: &Request| {
                let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
                let scheme = r.header("authorization").map(|a| a.split(' ').next().unwrap_or("").to_string());
                log.lock().unwrap().push((r.method.clone(), r.path.clone(), body.clone(), scheme));
                let prefix = format!("/api/f/{NAME}/ops/");
                match (r.method.as_str(), r.path.strip_prefix(&prefix)) {
                    ("GET", _) if r.path == format!("/api/f/{NAME}/status") => Response::json(200, &status(role)),
                    ("GET", _) => Response::json(404, &json!({ "error": "not_found", "message": "no fragment named nope--a2b3" })),
                    (_, Some("list")) => Response::json(200, &json!({ "result": { "todos": [{ "id": 1, "text": "milk" }] }, "replayed": false })),
                    (_, Some("find")) if body["input"]["q"] == "boom" => Response::json(422, &json!({ "error": "app_failed", "message": "the app threw: boom" })),
                    (_, Some("find")) => Response::json(200, &json!({ "result": [body["input"]["q"].clone()], "replayed": false })),
                    (_, Some("add")) if body["input"].get("text").is_none() => {
                        Response::json(400, &json!({ "error": "invalid_request", "message": "input /text: is required" }))
                    }
                    (_, Some("add")) => Response::json(200, &json!({ "result": { "id": 7 }, "replayed": false })),
                    (_, Some("digest")) => Response::unanswered(),
                    _ => Response::json(404, &json!({ "error": "unknown_operation", "message": "no operation" })),
                }
            }),
        )
        .expect("start the fake host");
        (host, seen)
    }

    /// The server's answers to `lines`, one JSON value per line it wrote.
    fn session(client: &Client, fragment: &str, write: bool, lines: &[Value]) -> Vec<Value> {
        let input: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let mut out = Vec::new();
        serve(client, fragment, write, std::io::Cursor::new(input), &mut out).expect("the session ends at the input's end");
        let text = String::from_utf8(out).expect("UTF-8");
        text.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("stdout carries only JSON-RPC: {l:?}: {e}"))).collect()
    }

    fn initialize(id: i64, version: &str) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": "initialize",
                "params": { "protocolVersion": version, "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } })
    }

    fn request(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn call(id: i64, name: &str, arguments: Value) -> Value {
        request(id, "tools/call", json!({ "name": name, "arguments": arguments }))
    }

    fn names(list: &Value) -> Vec<&str> {
        list["result"]["tools"].as_array().map(|t| t.iter().filter_map(|t| t["name"].as_str()).collect()).unwrap_or_default()
    }

    fn text(answer: &Value) -> String {
        answer["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    /// Goal: a client sees the fragment's described operations as tools,
    /// read-only unless --write, and calls them; a refusal is the tool's
    /// error with the platform's code. Method: a whole session against a
    /// fake host (initialize, the initialized notification, a ping,
    /// tools/list, calls), read-only and then with --write; each answer
    /// checked, and each request the host saw.
    #[test]
    fn a_session_serves_the_described_operations() {
        let (host, seen) = host("editor");
        let client = Client::new(&host.url, crate::auth::fixed(7));
        let answers = session(
            &client,
            NAME,
            false,
            &[
                initialize(1, "2025-06-18"),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                request(2, "ping", json!({})),
                request(3, "tools/list", json!({})),
                call(4, "list", json!({})),
                call(5, "find", json!({ "q": "milk" })),
                call(6, "add", json!({ "text": "eggs" })),
                call(7, "find", json!({ "q": "boom" })),
                request(8, "tools/call", json!({ "name": "list" })),
            ],
        );
        assert_eq!(answers.len(), 8, "every request answered, the notification not: {answers:?}");
        assert!(answers.iter().all(|a| a["jsonrpc"] == "2.0"));
        let init = &answers[0]["result"];
        assert_eq!(init["protocolVersion"], "2025-06-18", "a revision it knows is answered in kind");
        assert_eq!(init["capabilities"], json!({ "tools": { "listChanged": false } }));
        assert_eq!((init["serverInfo"]["name"].as_str(), init["serverInfo"]["title"].as_str()), (Some("fragment"), Some(NAME)));
        assert!(init["instructions"].as_str().is_some_and(|i| i.contains(NAME) && i.contains("--write") && i.contains("(editor)")), "{init}");
        assert_eq!(answers[1], json!({ "jsonrpc": "2.0", "id": 2, "result": {} }), "ping");
        assert_eq!(names(&answers[2]), ["find", "list"], "read-only: the described queries the caller may call, whose input is an object");
        let tools = answers[2]["result"]["tools"].as_array().unwrap();
        let list = json!({ "name": "list", "description": "Every todo.", "inputSchema": { "type": "object" }, "annotations": { "readOnlyHint": true, "openWorldHint": false } });
        assert_eq!(tools[1], list, "no input declared: any object");
        let find = json!({ "type": "object", "required": ["q"], "properties": { "q": { "type": "string" } } });
        assert_eq!(tools[0]["inputSchema"], find, "its own schema, saying it is an object's");
        assert_eq!((text(&answers[3]), &answers[3]["result"]["isError"]), (r#"{"todos":[{"id":1,"text":"milk"}]}"#.to_string(), &json!(false)), "a result as JSON");
        assert_eq!(answers[3]["result"]["structuredContent"], json!({ "todos": [{ "id": 1, "text": "milk" }] }), "and structured, an object");
        assert_eq!(text(&answers[4]), r#"["milk"]"#);
        assert!(answers[4]["result"].get("structuredContent").is_none(), "a result that is no object is text alone");
        assert_eq!(answers[5]["error"]["code"], INVALID_PARAMS, "a write without --write is refused: {}", answers[5]);
        assert!(answers[5]["error"]["message"].as_str().is_some_and(|m| m.contains("--write")), "{}", answers[5]);
        let refused = &answers[6]["result"];
        assert_eq!(refused["isError"], true, "the fragment's refusal is the tool's error: {refused}");
        assert_eq!(refused["structuredContent"]["error"], json!({ "code": "app_failed", "status": 422, "message": "the app threw: boom" }), "with the platform's code");
        assert!(text(&answers[6]).starts_with("app_failed: the app threw: boom (operation id mcp-"), "{}", text(&answers[6]));
        assert_eq!(text(&answers[7]), text(&answers[3]), "arguments may be left out");

        let seen = seen.lock().unwrap();
        let calls: Vec<_> = seen.iter().filter(|r| r.0 == "POST").collect();
        assert_eq!(calls.len(), 4, "list, find, the refused find, list: the write never left");
        let ids: std::collections::BTreeSet<&str> = calls.iter().filter_map(|c| c.2["id"].as_str()).collect();
        assert_eq!(ids.len(), 4, "a fresh id a call");
        assert!(ids.iter().all(|id| fragment_proto::valid_op_id(id) && id.starts_with("mcp-")));
        assert_eq!(calls[1].1, format!("/api/f/{NAME}/ops/find"));
        assert_eq!(calls[1].2["input"], json!({ "q": "milk" }), "the arguments are the input, flat");
        assert_eq!(calls[3].2["input"], json!({}));
        assert!(seen.iter().all(|r| r.3.as_deref() == Some("Nostr")), "each request signed with the key, as the CLI's are");
        assert_eq!(seen.iter().filter(|r| r.0 == "GET").count(), 2, "the operations read at initialize and at tools/list (add is among them: no read for it)");
        drop(seen);

        // with --write: the described mutations and jobs too
        let answers = session(&client, NAME, true, &[initialize(1, "2025-06-18"), request(2, "tools/list", json!({})), call(3, "add", json!({ "text": "eggs" })), call(4, "add", json!({}))]);
        assert!(answers[0]["result"]["instructions"].as_str().is_some_and(|i| i.contains("change it")), "{}", answers[0]);
        assert_eq!(names(&answers[1]), ["add", "digest", "find", "list"]);
        let add = &answers[1]["result"]["tools"][0];
        assert_eq!(add["annotations"], json!({ "readOnlyHint": false, "idempotentHint": false, "openWorldHint": false }), "{add}");
        assert_eq!(answers[2]["result"]["structuredContent"], json!({ "id": 7 }));
        let schema_refusal = json!({ "code": "invalid_request", "status": 400, "message": "input /text: is required" });
        assert_eq!(answers[3]["result"]["structuredContent"]["error"], schema_refusal, "a schema refusal, with its code");
    }

    /// The caller's role there bounds the tools, and a call to one beyond
    /// it is refused here, saying why.
    #[test]
    fn the_callers_role_bounds_the_tools() {
        let (owner, _) = host("owner");
        let client = Client::new(&owner.url, crate::auth::fixed(7));
        assert_eq!(names(&session(&client, NAME, false, &[request(1, "tools/list", json!({}))])[0]), ["find", "list", "secret"]);
        let (viewer, seen) = host("viewer");
        let client = Client::new(&viewer.url, crate::auth::fixed(7));
        let answers = session(&client, NAME, true, &[request(1, "tools/list", json!({})), call(2, "add", json!({ "text": "x" })), call(3, "secret", json!({}))]);
        assert_eq!(names(&answers[0]), ["find", "list"], "a viewer's --write adds nothing an editor calls");
        assert!(answers[1]["error"]["message"].as_str().is_some_and(|m| m.contains("needs the editor role, and you are a viewer")), "{}", answers[1]);
        assert!(answers[2]["error"]["message"].as_str().is_some_and(|m| m.contains("needs the owner role")), "{}", answers[2]);
        assert!(seen.lock().unwrap().iter().all(|r| r.0 == "GET"), "nothing was called");
    }

    /// Goal: what is not a well-formed request is answered as JSON-RPC
    /// says, and the session goes on. Method: a revision it does not know,
    /// a line that is not JSON, a blank line, a batch, an id that is no
    /// id, a request with no method, an unknown method, a client's own
    /// answer, bad tool params, an operation that is no tool (undescribed,
    /// unknown, taking a string), a line over the limit; then a fragment
    /// it cannot read.
    #[test]
    fn what_is_not_a_request_is_answered_as_json_rpc_says() {
        let (host, seen) = host("editor");
        let client = Client::new(&host.url, crate::auth::fixed(7));
        let mut input = String::new();
        for line in [
            initialize(1, "2099-01-01").to_string(),
            "{not json".into(),
            String::new(),
            json!([request(2, "ping", json!({}))]).to_string(),
            json!({ "jsonrpc": "2.0", "id": {}, "method": "ping" }).to_string(),
            json!({ "jsonrpc": "2.0", "id": 3 }).to_string(),
            request(4, "resources/list", json!({})).to_string(),
            json!({ "jsonrpc": "2.0", "id": 77, "result": {} }).to_string(),
            request(5, "tools/call", json!({ "arguments": {} })).to_string(),
            call(6, "list", json!([1])).to_string(),
            call(7, "count", json!({})).to_string(),
            call(8, "nothing", json!({})).to_string(),
            call(9, "shout", json!({})).to_string(),
            "x".repeat(MESSAGE_MAX_BYTES + 10),
            json!({ "jsonrpc": "2.0", "id": "s", "method": "ping" }).to_string(),
        ] {
            input.push_str(&line);
            input.push_str("\r\n");
        }
        let mut out = Vec::new();
        serve(&client, NAME, false, std::io::Cursor::new(input), &mut out).unwrap();
        let answers: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let code = |i: usize| (answers[i]["error"]["code"].as_i64(), answers[i]["id"].clone());
        let message = |i: usize| answers[i]["error"]["message"].as_str().unwrap_or("").to_string();
        assert_eq!(answers[0]["result"]["protocolVersion"], PROTOCOL_VERSIONS[0], "a revision it does not know, with its newest");
        assert_eq!(code(1), (Some(PARSE_ERROR), Value::Null), "not JSON (and a blank line is no message)");
        assert_eq!(code(2), (Some(INVALID_REQUEST), Value::Null), "a batch");
        assert_eq!(code(3), (Some(INVALID_REQUEST), Value::Null), "an id that is no string or number");
        assert_eq!(code(4), (Some(INVALID_REQUEST), json!(3)), "no method");
        assert_eq!(code(5), (Some(METHOD_NOT_FOUND), json!(4)));
        assert_eq!(code(6), (Some(INVALID_PARAMS), json!(5)), "the client's own answer is not answered; a call names its tool");
        assert_eq!(code(7), (Some(INVALID_PARAMS), json!(6)), "arguments are an object");
        assert!(message(8).contains("description"), "an undescribed operation is no tool: {}", message(8));
        assert!(message(9).contains("no such operation"), "{}", message(9));
        assert!(message(10).contains("not an object"), "{}", message(10));
        assert_eq!(code(11).0, Some(INVALID_REQUEST), "a line over the limit");
        assert_eq!(answers[12], json!({ "jsonrpc": "2.0", "id": "s", "result": {} }), "and the session goes on");
        assert_eq!(answers.len(), 13);
        assert!(seen.lock().unwrap().iter().all(|r| r.0 == "GET"), "nothing was called");

        // a fragment it cannot read: initialize fails, with the platform's error
        let answers = session(&client, "nope--a2b3", false, &[initialize(1, "2025-06-18"), request(2, "tools/list", json!({}))]);
        for a in &answers {
            assert_eq!(a["error"]["code"].as_i64(), Some(INTERNAL_ERROR), "{a}");
            assert_eq!((&a["error"]["data"]["code"], &a["error"]["data"]["status"]), (&json!("not_found"), &json!(404)), "{a}");
            assert!(a["error"]["message"].as_str().is_some_and(|m| m.contains("nope--a2b3") && m.contains("no fragment named")), "{a}");
        }
    }

    /// A call whose answer was lost may have run: the tool's error says
    /// so, with the id that replays it, and the client's retries reused it.
    #[test]
    fn a_call_whose_answer_was_lost_names_its_id() {
        let (host, seen) = host("editor");
        let client = Client::new(&host.url, crate::auth::fixed(7));
        let answers = session(&client, NAME, true, &[call(1, "digest", json!({}))]);
        let error = &answers[0]["result"];
        assert_eq!((&error["isError"], &error["structuredContent"]["error"]["code"]), (&json!(true), &json!("outcome_unknown")), "{error}");
        let id = error["structuredContent"]["id"].as_str().unwrap_or("");
        assert!(text(&answers[0]).contains(&format!("--id {id}")), "{error}");
        let tries: Vec<Value> = seen.lock().unwrap().iter().filter(|r| r.0 == "POST").map(|r| r.2["id"].clone()).collect();
        assert!(tries.len() == 3 && tries.iter().all(|t| t == id), "the client's retries reuse the call's id: {tries:?}");
    }

    #[test]
    fn an_operations_input_schema_is_its_tools() {
        let op = |input: Option<Value>| OpDecl { kind: OpKind::Query, role: Role::Viewer, input, ephemeral: false, description: Some("d".into()) };
        assert_eq!(input_schema(&op(None)), Some(json!({ "type": "object" })));
        assert_eq!(input_schema(&op(Some(json!({ "properties": {} })))), Some(json!({ "type": "object", "properties": {} })));
        let closed = json!({ "type": "object", "additionalProperties": false });
        assert_eq!(input_schema(&op(Some(closed.clone()))), Some(closed));
        assert_eq!(input_schema(&op(Some(json!({ "type": "string" })))), None);
        assert_eq!(input_schema(&op(Some(json!({ "type": ["object", "null"] })))), None, "only an object's");
        let job = OpDecl { kind: OpKind::Job, ..op(None) };
        assert_eq!(tool("j", &job, Role::Editor, false), Err(NotTool::Writes));
        assert_eq!(tool("j", &job, Role::Editor, true).map(|t| t["annotations"]["openWorldHint"].clone()), Ok(json!(true)), "a job may reach the world");
        assert_eq!(tool("q", &OpDecl { description: None, ..op(None) }, Role::Owner, true), Err(NotTool::Undescribed));
    }
}
