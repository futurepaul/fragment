//! `fragment mcp` as an MCP client runs it: the binary, speaking MCP over
//! its stdio, signed with this machine's key, and inside a computer as its
//! agent (no key: the computer's egress signs). The protocol's every case
//! is tested in-process (src/mcp.rs); the e2e's cli section runs it against
//! a node.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use fragment_fakes::http::{Request, Response, Server};
use serde_json::{json, Value};

const NAME: &str = "todo--k3x9";

/// Each request as the host saw it: method, path, `for`, its agent header,
/// its signature's scheme, and its body.
type Seen = Arc<Mutex<Vec<(String, String, Option<String>, Option<String>, Option<String>, Value)>>>;

/// A host with one fragment: a query and a mutation, each described, and
/// a query its app refuses.
fn host() -> (Server, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let server = Server::start(
        0,
        Arc::new(move |r: &Request| {
            let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
            let scheme = r.header("authorization").map(|a| a.split(' ').next().unwrap_or("").to_string());
            let entry = (r.method.clone(), r.path.clone(), r.query.get("for").cloned(), r.header("x-fragment-agent").map(str::to_string), scheme, body);
            log.lock().unwrap().push(entry);
            let status = json!({
                "name": NAME, "npub": "n", "owner": "id:paul", "role": "editor", "visibility": "members", "repo": "r",
                "pins": { "main": "a", "live": "a" }, "counts": { "files": 1, "events": 0, "members": 2 },
                "code": { "sha": "a", "id": "app:x", "error": null, "operations": {
                    "list": { "kind": "query", "role": "viewer", "description": "Every todo." },
                    "fail": { "kind": "query", "role": "viewer", "description": "Always refused." },
                    "add": { "kind": "mutation", "role": "editor", "description": "Add a todo." },
                } },
                "viewToken": null, "inboxToken": null, "urls": { "canonical": "https://todo--k3x9.x/", "platform": "https://x" },
            });
            match (r.method.as_str(), r.path.strip_prefix(&format!("/api/f/{NAME}/"))) {
                ("GET", Some("status")) => Response::json(200, &status),
                ("POST", Some("ops/list")) => Response::json(200, &json!({ "result": { "todos": [] }, "replayed": false })),
                ("POST", Some("ops/fail")) => Response::json(422, &json!({ "error": "app_failed", "message": "the app threw: no" })),
                _ => Response::json(404, &json!({ "error": "not_found", "message": "nothing here" })),
            }
        }),
    )
    .expect("start the fake host");
    (server, seen)
}

/// A HOME of its own: with `key`, a config holding that secret.
fn home(tag: &str, key: Option<&str>) -> PathBuf {
    let home = std::env::temp_dir().join(format!("fragment-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join(".config/fragment")).unwrap();
    if let Some(key) = key {
        std::fs::write(home.join(".config/fragment/config.json"), json!({ "secret_key": key }).to_string()).unwrap();
    }
    home
}

fn fragment(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_fragment"));
    c.env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("FRAGMENT_HOST")
        .env_remove("FRAGMENT_AS_AGENT")
        .env_remove("FRAGMENT_FOR")
        .env_remove("FRAGMENT_API")
        // even asked for --json envelopes, stdout is the protocol's alone
        .env("FRAGMENT_OUTPUT", "json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

/// Runs a session: each message a line on its stdin, then stdin closed;
/// its answers, each stdout line a JSON-RPC message.
fn session(mut command: Command, messages: &[Value]) -> Vec<Value> {
    let mut child = command.spawn().expect("run the CLI");
    let mut stdin = child.stdin.take().unwrap();
    for message in messages {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);
    let lines: Vec<String> = BufReader::new(child.stdout.take().unwrap()).lines().map(Result::unwrap).collect();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "it ends when the client closes its stdin: {}", String::from_utf8_lossy(&out.stderr));
    lines.iter().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("stdout carries only JSON-RPC: {l:?}: {e}"))).collect()
}

fn messages() -> Vec<Value> {
    let call = |id: i64, name: &str| json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": name, "arguments": {} } });
    vec![
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        call(3, "list"),
        call(4, "fail"),
        call(5, "add"),
    ]
}

/// Goal: a person's MCP client runs `fragment mcp <fragment>`, and it
/// serves the fragment's described queries, each call signed with this
/// machine's key. Method: the binary over its stdio, a whole session:
/// initialize, tools/list, a call that answers, one the app refuses (the
/// tool's error, with the platform's code), and a write without --write
/// (refused, never sent).
#[test]
fn a_person_serves_a_fragments_tools_over_stdio() {
    let (server, seen) = host();
    let home = home("key", Some(&"07".repeat(32)));
    let mut command = fragment(&home);
    command.args(["mcp", NAME, "--host", &server.url]);
    let answers = session(command, &messages());
    std::fs::remove_dir_all(&home).ok();
    assert_eq!(answers.iter().map(|a| a["id"].as_i64().unwrap_or(0)).collect::<Vec<_>>(), [1, 2, 3, 4, 5], "{answers:?}");
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-06-18");
    let tools: Vec<&str> = answers[1]["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(tools, ["fail", "list"], "read-only: the described queries");
    assert_eq!(answers[2]["result"]["structuredContent"], json!({ "todos": [] }));
    assert_eq!((&answers[3]["result"]["isError"], &answers[3]["result"]["structuredContent"]["error"]["code"]), (&json!(true), &json!("app_failed")));
    assert!(answers[4]["error"]["message"].as_str().is_some_and(|m| m.contains("--write")), "{}", answers[4]);
    let seen = seen.lock().unwrap();
    assert!(seen.iter().all(|r| r.4.as_deref() == Some("Nostr") && r.3.is_none()), "each request signed with the key: {seen:?}");
    let called: Vec<&str> = seen.iter().filter(|r| r.0 == "POST").map(|r| r.1.as_str()).collect();
    assert_eq!(called, [format!("/api/f/{NAME}/ops/list"), format!("/api/f/{NAME}/ops/fail")], "the write never left");
}

/// Goal: an agent on a computer serves a fragment's tools to its runtime
/// with no key: `FRAGMENT_AS_AGENT` and `FRAGMENT_FOR` and the computer's
/// `FRAGMENT_API`, as our images set them, and --write serves its
/// mutation. Method: the binary over its stdio; every request the host
/// saw names the agent, acts for its owner, and carries no signature.
#[test]
fn an_agent_serves_a_fragments_tools_over_stdio() {
    let (server, seen) = host();
    let home = home("agent", None);
    let mut command = fragment(&home);
    command.args(["mcp", NAME, "--write"]).env("FRAGMENT_AS_AGENT", "hands--a2b3").env("FRAGMENT_FOR", "npub1paul").env("FRAGMENT_API", &server.url);
    let answers = session(command, &messages());
    std::fs::remove_dir_all(&home).ok();
    let tools: Vec<&str> = answers[1]["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(tools, ["add", "fail", "list"], "--write: the described mutation too");
    assert_eq!(answers[4]["result"]["structuredContent"]["error"]["code"], "not_found", "the write was sent, and the host's refusal is the tool's error: {}", answers[4]);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.iter().filter(|r| r.0 == "POST").count(), 3, "list, fail, add: {seen:?}");
    for (method, path, acting_for, agent, signed, _) in seen.iter() {
        assert_eq!((agent.as_deref(), signed.as_deref()), (Some("hands--a2b3"), None), "{method} {path} names the agent and carries no signature");
        assert_eq!(acting_for.as_deref(), Some("npub1paul"), "{method} {path} acts for the owner");
    }
}

/// A server that cannot start says why on stderr and leaves stdout, the
/// protocol's, empty: a person with no key, and an agent without its
/// computer's API.
#[test]
fn a_server_that_cannot_start_says_why_on_stderr() {
    let home = home("refused", None);
    let out = fragment(&home).args(["mcp", NAME]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fragment login"), "{}", String::from_utf8_lossy(&out.stderr));
    let out = fragment(&home).args(["mcp", NAME]).env("FRAGMENT_AS_AGENT", "hands--a2b3").stdin(Stdio::null()).output().unwrap();
    std::fs::remove_dir_all(&home).ok();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stderr).contains("FRAGMENT_API"), "{}", String::from_utf8_lossy(&out.stderr));
}
