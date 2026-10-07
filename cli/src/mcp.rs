//! `fragment mcp <fragment> [--write]`: a fragment's described operations
//! as an MCP server's tools, over stdio (docs/optchat.md, "The MCP
//! server"). JSON-RPC 2.0, one message per line each way. Stdout carries
//! the protocol's messages and nothing else: a log line goes to stderr.
//!
//! A tool is an operation whose `fragment.json` entry has a `description`
//! and that the caller's role may call: a query always, a mutation or a job
//! only with `--write`. Its input schema is the operation's. A call is
//! `POST /api/f/<fragment>/ops/<op>` with a fresh id, through the CLI's own
//! client: signed with this machine's key, or, inside a computer, named as
//! its agent for the computer's egress to sign (`FRAGMENT_AS_AGENT`).

use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};

use anyhow::Result;
use fragment_proto::{FragmentStatus, OpCall, OpDecl, OpKind, OpResult};
use serde_json::{json, Map, Value};

use crate::api::Client;

/// The protocol revision this server speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// The revisions a client may offer that this server answers in kind: it
/// serves tools alone, whose messages these share. Any other is answered
/// with ours, and the client chooses whether to go on.
const VERSIONS: [&str; 3] = [PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];
/// One message, at most: an operation's input is at most 256 KiB
/// (`limits::INPUT_MAX_BYTES`), and the rest of a call is small.
pub const MESSAGE_MAX_BYTES: usize = 1024 * 1024;

// JSON-RPC's error codes (the spec's; MCP names an unknown tool -32602).
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

/// A tool as served: its operation's kind and its MCP definition.
struct Tool {
    kind: OpKind,
    def: Value,
}

/// The server: the fragment it serves, and the tools it last read.
pub struct Server<'a> {
    client: &'a Client,
    fragment: String,
    write: bool,
    tools: BTreeMap<String, Tool>,
}

/// An error answered as JSON-RPC's `error`.
struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into() }
    }
}

/// Serves `fragment`'s tools to the client on `input` and `output` until
/// the client closes `input`.
pub fn serve(client: &Client, fragment: &str, write: bool, mut input: impl BufRead, mut output: impl Write) -> Result<()> {
    let mut server = Server { client, fragment: fragment.to_string(), write, tools: BTreeMap::new() };
    let mut line = Vec::new();
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
/// one is read to its end and dropped.
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
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": e.code, "message": e.message } })
}

impl Server<'_> {
    /// The answer to one message: none for a notification, or for a
    /// client's answer (this server asks nothing of a client).
    fn handle(&mut self, message: Value) -> Option<Value> {
        let Value::Object(mut message) = message else {
            return Some(failure(Value::Null, RpcError::new(INVALID_REQUEST, "a message is a JSON object (batches are not part of 2025-06-18)")));
        };
        let id = message.remove("id");
        let Some(method) = message.get("method").and_then(Value::as_str).map(str::to_string) else {
            return match id {
                Some(id) if !message.contains_key("result") && !message.contains_key("error") => Some(failure(id, RpcError::new(INVALID_REQUEST, "a request names its method"))),
                _ => None,
            };
        };
        let params = message.remove("params").unwrap_or(Value::Null);
        let Some(id) = id else {
            // notifications/initialized, notifications/cancelled: nothing to answer
            return None;
        };
        Some(match self.request(&method, &params) {
            Ok(result) => success(id, result),
            Err(e) => failure(id, e),
        })
    }

    fn request(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => {
                let offered = params.get("protocolVersion").and_then(Value::as_str);
                let version = offered.filter(|v| VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSION);
                self.read_tools()?;
                let served = if self.write { "queries, mutations and jobs (--write)" } else { "queries only (read-only)" };
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "fragment", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": format!("These tools are the described operations of the fragment {}: {served}.", self.fragment),
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                self.read_tools()?;
                Ok(json!({ "tools": self.tools.values().map(|t| t.def.clone()).collect::<Vec<_>>() }))
            }
            "tools/call" => self.call(params),
            other => Err(RpcError::new(METHOD_NOT_FOUND, format!("no method {other:?}: this server serves tools (initialize, ping, tools/list, tools/call)"))),
        }
    }

    /// Reads the fragment's operations again (a deploy may have changed them).
    fn read_tools(&mut self) -> Result<(), RpcError> {
        let read = self.client.get(&format!("/api/f/{}/status", self.fragment)).and_then(|r| self.client.call_as::<FragmentStatus>(r));
        let status = read.map_err(|e| RpcError::new(INTERNAL_ERROR, format!("reading {}'s operations: {e:#}", self.fragment)))?;
        self.tools = tools_of(&status, self.write);
        Ok(())
    }

    fn call(&mut self, params: &Value) -> Result<Value, RpcError> {
        let name = params.get("name").and_then(Value::as_str).ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call names its tool (params.name)"))?;
        let input = match params.get("arguments") {
            None | Some(Value::Null) => Value::Object(Map::new()),
            Some(args @ Value::Object(_)) => args.clone(),
            Some(_) => return Err(RpcError::new(INVALID_PARAMS, "a tool's arguments are an object")),
        };
        // a tool the client knows of and this server does not: deployed since
        if !self.tools.contains_key(name) {
            self.read_tools()?;
        }
        let Some(tool) = self.tools.get(name) else {
            return Err(RpcError::new(INVALID_PARAMS, format!("no tool {name:?} (tools/list lists them)")));
        };
        assert!(tool.kind == OpKind::Query || self.write, "a tool that writes is served only with --write");
        // a fresh id: each call is an action of its own (a retry inside the
        // client reuses it, so the call runs once)
        let id = format!("mcp-{:016x}", rand::random::<u64>());
        let call = OpCall { id: id.clone(), input };
        let path = format!("/api/f/{}/ops/{name}", self.fragment);
        let answer = self.client.post_json_by_id(&path, &call).and_then(|r| self.client.call_as::<OpResult>(r));
        Ok(match answer {
            Ok(done) => json!({ "content": [{ "type": "text", "text": text_of(&done.result) }], "isError": false }),
            Err(e) => {
                let code = e.downcast_ref::<crate::api::CodedError>().map_or("server_error", |c| c.code.as_str());
                json!({ "content": [{ "type": "text", "text": format!("{e:#} ({code}; operation id {id})") }], "isError": true })
            }
        })
    }
}

/// A result as a tool answers it: a `text` the operation rendered (a
/// mind's view, zoom, date) as it is, anything else as pretty JSON.
fn text_of(result: &Value) -> String {
    match result.get("text") {
        Some(Value::String(text)) => text.clone(),
        _ => serde_json::to_string_pretty(result).expect("a JSON value serializes"),
    }
}

/// The tools a status declares: each described operation the caller's role
/// may call, a query always and the rest with `write`.
fn tools_of(status: &FragmentStatus, write: bool) -> BTreeMap<String, Tool> {
    let mut tools = BTreeMap::new();
    for (name, op) in &status.code.operations {
        let Some(description) = &op.description else { continue };
        if (op.kind != OpKind::Query && !write) || op.role > status.role {
            continue;
        }
        let Some(schema) = input_schema(op) else {
            eprintln!("fragment mcp: {name} takes no object, and a tool's arguments are one: it is not served");
            continue;
        };
        let mut def = json!({ "name": name, "description": description, "inputSchema": schema });
        if op.kind == OpKind::Query {
            def["annotations"] = json!({ "readOnlyHint": true });
        }
        tools.insert(name.clone(), Tool { kind: op.kind, def });
    }
    tools
}

/// An operation's input schema as a tool's: an object's (MCP's arguments
/// are one), `{"type": "object"}` when it declares none, and none when it
/// declares another type.
fn input_schema(op: &OpDecl) -> Option<Value> {
    let Some(Value::Object(schema)) = &op.input else {
        return Some(json!({ "type": "object" }));
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

#[cfg(test)]
mod tests {
    use super::*;
    use fragment_fakes::http::{Request, Response, Server as FakeHost};
    use std::sync::{Arc, Mutex};

    /// A fragment's status as a host answers it, with these operations,
    /// the caller holding `role`.
    fn status(operations: Value, role: &str) -> Value {
        json!({
            "name": "mind.paul", "npub": "n", "owner": "id:p", "role": role, "visibility": "members", "repo": "r",
            "pins": { "main": "a", "live": "a" }, "counts": { "files": 1, "events": 0, "members": 2 },
            "code": { "sha": "a", "id": "app:x", "operations": operations, "error": null },
            "viewToken": null, "inboxToken": null, "urls": { "canonical": "https://mind--paul.x/", "platform": "https://x" },
        })
    }

    fn mind_ops() -> Value {
        json!({
            "view":   { "kind": "query", "role": "viewer", "description": "The rendered view of the whole memory." },
            "zoom":   { "kind": "query", "role": "viewer", "description": "Expand part of the view.",
                        "input": { "type": "object", "required": ["id", "n"], "properties": { "id": { "type": "integer" }, "n": { "type": "integer" } } } },
            "search": { "kind": "query", "role": "viewer", "description": "Search the log.", "input": { "properties": { "q": { "type": "string" } } } },
            "threads": { "kind": "query", "role": "viewer" },
            "secret": { "kind": "query", "role": "owner", "description": "Only the owner's." },
            "shout":  { "kind": "query", "role": "viewer", "description": "Takes a string.", "input": { "type": "string" } },
            "note":   { "kind": "mutation", "role": "editor", "description": "Append a note.",
                        "input": { "type": "object", "required": ["text"], "properties": { "text": { "type": "string" } } } },
            "hear":   { "kind": "mutation", "role": "editor" },
            "suggest": { "kind": "job", "role": "editor", "description": "Suggest topics." },
        })
    }

    type Seen = Arc<Mutex<Vec<(String, String, Value, Option<String>)>>>;

    /// A host with the mind's operations: it answers status, and each
    /// operation as a mind would, keeping each request (method, path,
    /// body, and its signature's scheme).
    fn mind_host(role: &'static str) -> (FakeHost, Seen) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let host = FakeHost::start(
            0,
            Arc::new(move |r: &Request| {
                let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
                let auth = r.header("authorization").map(|a| a.split(' ').next().unwrap_or("").to_string());
                log.lock().unwrap().push((r.method.clone(), r.path.clone(), body.clone(), auth));
                let op = r.path.strip_prefix("/api/f/mind.paul/ops/");
                match (r.method.as_str(), r.path.as_str(), op) {
                    ("GET", "/api/f/mind.paul/status", _) => Response::json(200, &status(mind_ops(), role)),
                    ("GET", _, _) => Response::json(404, &json!({ "error": "not_found", "message": "no fragment named nope.paul" })),
                    (_, _, Some("view")) => Response::json(200, &json!({ "result": { "text": "<chat>\n0|user: hi\n</chat>", "bytes": 30 }, "replayed": false })),
                    (_, _, Some("zoom")) if body["input"]["id"] == 9 => {
                        Response::json(422, &json!({ "error": "app_failed", "message": "the app threw: no message 9" }))
                    }
                    (_, _, Some("zoom")) => Response::json(200, &json!({ "result": { "text": format!("zoomed {}", body["input"]["id"]) }, "replayed": false })),
                    (_, _, Some("search")) => Response::json(200, &json!({ "result": { "results": [{ "i": 1, "snippet": "hi" }] }, "replayed": false })),
                    (_, _, Some("note")) => Response::json(200, &json!({ "result": { "i": 7 }, "replayed": false })),
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

    fn names(list: &Value) -> Vec<&str> {
        list["result"]["tools"].as_array().map(|t| t.iter().filter_map(|t| t["name"].as_str()).collect()).unwrap_or_default()
    }

    /// Goal: a client that connects sees the fragment's described
    /// operations as tools, read-only unless `--write`, and calls them.
    /// Method: a whole session against a fake host (initialize, the
    /// initialized notification, a ping, tools/list, calls), read-only and
    /// then with --write; each answer checked, and each request the host saw.
    #[test]
    fn a_session_serves_the_described_operations() {
        let (host, seen) = mind_host("editor");
        let client = Client::new(&host.url, crate::auth::fixed(7));
        let answers = session(
            &client,
            "mind.paul",
            false,
            &[
                initialize(1, PROTOCOL_VERSION),
                json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
                request(2, "ping", json!({})),
                request(3, "tools/list", json!({})),
                request(4, "tools/call", json!({ "name": "view", "arguments": {} })),
                request(5, "tools/call", json!({ "name": "zoom", "arguments": { "id": 3, "n": 64 } })),
                request(6, "tools/call", json!({ "name": "search", "arguments": { "q": "hi" } })),
                request(7, "tools/call", json!({ "name": "note", "arguments": { "text": "x" } })),
                request(8, "tools/call", json!({ "name": "zoom", "arguments": { "id": 9, "n": 1 } })),
                request(9, "tools/call", json!({ "name": "view" })),
            ],
        );
        assert_eq!(answers.len(), 9, "every request answered, the notification not: {answers:?}");
        let init = &answers[0]["result"];
        assert_eq!((answers[0]["jsonrpc"].as_str(), answers[0]["id"].as_i64()), (Some("2.0"), Some(1)));
        assert_eq!(init["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(init["capabilities"], json!({ "tools": { "listChanged": false } }));
        assert_eq!(init["serverInfo"]["name"], "fragment");
        assert!(init["instructions"].as_str().is_some_and(|i| i.contains("mind.paul") && i.contains("read-only")), "{init}");
        assert_eq!(answers[1], json!({ "jsonrpc": "2.0", "id": 2, "result": {} }), "ping");
        assert_eq!(names(&answers[2]), ["search", "view", "zoom"], "described queries the caller may call, whose input is an object");
        let tools = answers[2]["result"]["tools"].as_array().unwrap();
        let view = &tools[1];
        assert_eq!(view["description"], "The rendered view of the whole memory.");
        assert_eq!(view["inputSchema"], json!({ "type": "object" }), "no input schema: any object");
        assert_eq!(view["annotations"], json!({ "readOnlyHint": true }));
        assert_eq!(tools[2]["inputSchema"]["required"], json!(["id", "n"]), "the operation's own schema");
        assert_eq!(tools[0]["inputSchema"], json!({ "type": "object", "properties": { "q": { "type": "string" } } }), "an object's schema says so");
        let text = |a: &Value| a["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
        assert_eq!((text(&answers[3]), &answers[3]["result"]["isError"]), ("<chat>\n0|user: hi\n</chat>".to_string(), &json!(false)), "a result's text, as it is");
        assert_eq!(answers[3]["result"]["content"][0]["type"], "text");
        assert_eq!(text(&answers[4]), "zoomed 3");
        assert_eq!(serde_json::from_str::<Value>(&text(&answers[5])).unwrap(), json!({ "results": [{ "i": 1, "snippet": "hi" }] }), "anything else as JSON");
        assert!(text(&answers[5]).contains("\n  "), "pretty");
        assert_eq!(answers[6]["error"]["code"], INVALID_PARAMS, "a mutation is no tool without --write: {}", answers[6]);
        assert_eq!(answers[7]["result"]["isError"], true, "a refusal is the tool's error");
        assert!(text(&answers[7]).contains("the app threw: no message 9") && text(&answers[7]).contains("app_failed"), "{}", text(&answers[7]));
        assert_eq!(text(&answers[8]), "<chat>\n0|user: hi\n</chat>", "arguments may be left out");

        let seen = seen.lock().unwrap();
        let calls: Vec<&(String, String, Value, Option<String>)> = seen.iter().filter(|r| r.0 == "POST").collect();
        assert_eq!(calls.len(), 5, "view, zoom, search, the refused zoom, view; the mutation never left");
        let ids: std::collections::BTreeSet<&str> = calls.iter().filter_map(|c| c.2["id"].as_str()).collect();
        assert_eq!(ids.len(), 5, "a fresh id per call");
        assert!(ids.iter().all(|id| fragment_proto::valid_op_id(id)));
        assert_eq!(calls[1].1, "/api/f/mind.paul/ops/zoom");
        assert_eq!(calls[1].2["input"], json!({ "id": 3, "n": 64 }));
        assert_eq!(calls[4].2["input"], json!({}));
        assert!(seen.iter().all(|r| r.3.as_deref() == Some("Nostr")), "each request signed with the key, as the CLI's are");
        assert_eq!(seen.iter().filter(|r| r.0 == "GET").count(), 3, "the tools read at initialize and tools/list, and again for a tool not known");
        drop(seen);

        // with --write: the described mutations and jobs too
        let answers = session(
            &client,
            "mind.paul",
            true,
            &[initialize(1, PROTOCOL_VERSION), request(2, "tools/list", json!({})), request(3, "tools/call", json!({ "name": "note", "arguments": { "text": "x" } }))],
        );
        assert!(answers[0]["result"]["instructions"].as_str().is_some_and(|i| i.contains("--write")));
        assert_eq!(names(&answers[1]), ["note", "search", "suggest", "view", "zoom"]);
        let note = answers[1]["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "note").unwrap().clone();
        assert!(note.get("annotations").is_none(), "a write claims no read-only hint: {note}");
        assert_eq!(answers[2]["result"]["content"][0]["text"], "{\n  \"i\": 7\n}");
    }

    /// The caller's role bounds the tools: an owner sees the owner's.
    #[test]
    fn the_callers_role_bounds_the_tools() {
        let (host, _) = mind_host("owner");
        let client = Client::new(&host.url, crate::auth::fixed(7));
        let answers = session(&client, "mind.paul", false, &[request(1, "tools/list", json!({}))]);
        assert_eq!(names(&answers[0]), ["search", "secret", "view", "zoom"]);
        let (host, _) = mind_host("viewer");
        let client = Client::new(&host.url, crate::auth::fixed(7));
        let answers = session(&client, "mind.paul", true, &[request(1, "tools/list", json!({}))]);
        assert_eq!(names(&answers[0]), ["search", "view", "zoom"], "a viewer's --write adds nothing an editor's role calls");
    }

    /// Goal: what is not a well-formed request is answered as JSON-RPC
    /// says, and the session goes on. Method: an older protocol offered, a
    /// newer one, a line that is not JSON, a batch, a request without a
    /// method, an unknown method, a client's own answer, bad tool params,
    /// a line over the limit, and a fragment that cannot be read.
    #[test]
    fn what_is_not_a_request_is_answered_as_json_rpc_says() {
        let (host, seen) = mind_host("editor");
        let client = Client::new(&host.url, crate::auth::fixed(7));
        let lines = [
            initialize(1, "2024-11-05"),
            initialize(2, "2099-01-01"),
            json!("placeholder: not JSON"),
            json!([request(3, "ping", json!({}))]),
            json!({ "jsonrpc": "2.0", "id": 4 }),
            request(5, "resources/list", json!({})),
            json!({ "jsonrpc": "2.0", "id": 77, "result": {} }),
            request(6, "tools/call", json!({ "arguments": {} })),
            request(7, "tools/call", json!({ "name": "view", "arguments": [1] })),
            request(8, "tools/call", json!({ "name": "threads" })),
            json!("placeholder: too long"),
            json!({ "jsonrpc": "2.0", "id": "s", "method": "ping" }),
        ];
        let mut input = String::new();
        for l in &lines {
            match l.as_str() {
                Some("placeholder: not JSON") => input.push_str("{not json\n\n"),
                Some("placeholder: too long") => input.push_str(&format!("{}\n", "x".repeat(MESSAGE_MAX_BYTES + 10))),
                _ => input.push_str(&format!("{l}\r\n")),
            }
        }
        let mut out = Vec::new();
        serve(&client, "mind.paul", false, std::io::Cursor::new(input), &mut out).unwrap();
        let answers: Vec<Value> = String::from_utf8(out).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        let code = |i: usize| answers[i]["error"]["code"].as_i64();
        assert_eq!(answers[0]["result"]["protocolVersion"], "2024-11-05", "an older revision it knows is answered in kind");
        assert_eq!(answers[1]["result"]["protocolVersion"], PROTOCOL_VERSION, "one it does not know, with ours");
        assert_eq!((code(2), &answers[2]["id"]), (Some(PARSE_ERROR), &Value::Null), "not JSON");
        assert_eq!(code(3), Some(INVALID_REQUEST), "a batch");
        assert_eq!((code(4), &answers[4]["id"]), (Some(INVALID_REQUEST), &json!(4)), "no method");
        assert_eq!((code(5), &answers[5]["id"]), (Some(METHOD_NOT_FOUND), &json!(5)));
        assert_eq!((code(6), &answers[6]["id"]), (Some(INVALID_PARAMS), &json!(6)), "the client's own answer is not answered; a call names its tool");
        assert_eq!(code(7), Some(INVALID_PARAMS), "arguments are an object");
        assert_eq!(code(8), Some(INVALID_PARAMS), "an undescribed operation is no tool");
        assert!(answers[9]["error"]["message"].as_str().is_some_and(|m| m.contains("at most")), "a line over the limit: {}", answers[9]);
        assert_eq!(answers[10], json!({ "jsonrpc": "2.0", "id": "s", "result": {} }), "and the session goes on");
        assert_eq!(answers.len(), 11);
        assert!(seen.lock().unwrap().iter().all(|r| r.0 == "GET"), "nothing was called");

        // a fragment it cannot read: initialize fails, saying why
        let answers = session(&client, "nope.paul", false, &[initialize(1, PROTOCOL_VERSION), request(2, "tools/list", json!({}))]);
        for a in &answers {
            assert_eq!(a["error"]["code"], INTERNAL_ERROR);
            assert!(a["error"]["message"].as_str().is_some_and(|m| m.contains("nope.paul") && m.contains("no fragment named")), "{a}");
        }
    }

    #[test]
    fn a_result_with_text_is_that_text() {
        assert_eq!(text_of(&json!({ "text": "2026-10-07T09:00:00-05:00" })), "2026-10-07T09:00:00-05:00");
        assert_eq!(text_of(&json!({ "text": 7 })), "{\n  \"text\": 7\n}", "a text that is no string is data");
        assert_eq!(text_of(&json!(null)), "null");
        assert_eq!(text_of(&json!([1])), "[\n  1\n]");
    }
}
