//! `fragment mcp` as an MCP client runs it: the binary, over its stdio,
//! inside a computer (the CLI's agent mode: no key, the agent named for the
//! computer's egress to sign). The protocol itself is tested in-process
//! (src/mcp.rs).

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use fragment_fakes::http::{Request, Response, Server};
use serde_json::{json, Value};

/// Each request as the host saw it: method, path, `for`, its agent header,
/// whether it carried a signature, and its body.
type Seen = Arc<Mutex<Vec<(String, String, Option<String>, Option<String>, bool, Value)>>>;

fn host() -> (Server, Seen) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let server = Server::start(
        0,
        Arc::new(move |r: &Request| {
            let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
            let entry = (
                r.method.clone(),
                r.path.clone(),
                r.query.get("for").cloned(),
                r.header("x-fragment-agent").map(str::to_string),
                r.header("authorization").is_some(),
                body,
            );
            log.lock().unwrap().push(entry);
            match (r.method.as_str(), r.path.as_str()) {
                ("GET", "/api/f/mind.paul/status") => Response::json(
                    200,
                    &json!({
                        "name": "mind.paul", "npub": "n", "owner": "id:paul", "role": "editor", "visibility": "members", "repo": "r",
                        "pins": { "main": "a", "live": "a" }, "counts": { "files": 1, "events": 0, "members": 2 },
                        "code": { "sha": "a", "id": "app:x", "error": null, "operations": {
                            "view": { "kind": "query", "role": "viewer", "description": "The rendered view." },
                            "note": { "kind": "mutation", "role": "editor", "description": "Append a note." },
                        } },
                        "viewToken": null, "inboxToken": null, "urls": { "canonical": "https://mind--paul.x/", "platform": "https://x" },
                    }),
                ),
                ("POST", "/api/f/mind.paul/ops/view") => Response::json(200, &json!({ "result": { "text": "<chat>\n</chat>" }, "replayed": false })),
                _ => Response::json(404, &json!({ "error": "not_found", "message": "nothing here" })),
            }
        }),
    )
    .expect("start the fake host");
    (server, seen)
}

/// A HOME with nothing in it: no config, so no key to sign with.
fn empty_home(tag: &str) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("fragment-mcp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

fn fragment(home: &std::path::Path) -> Command {
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

/// Goal: an agent on a computer serves a fragment's tools to its runtime
/// with no key: `FRAGMENT_AS_AGENT` (and `FRAGMENT_FOR`) and the computer's
/// `FRAGMENT_API`, as our images set them. Method: the binary over its
/// stdio, a whole session; every stdout line is a JSON-RPC message, and
/// every request the host saw names the agent, acts for its owner, and
/// carries no signature.
#[test]
fn an_agent_serves_a_fragments_tools_over_stdio() {
    let (server, seen) = host();
    let home = empty_home("agent");
    let mut child = fragment(&home)
        .args(["mcp", "mind.paul"])
        .env("FRAGMENT_AS_AGENT", "hands.paul")
        .env("FRAGMENT_FOR", "id:paul")
        .env("FRAGMENT_API", &server.url)
        .spawn()
        .expect("run the CLI");
    let mut stdin = child.stdin.take().unwrap();
    for message in [
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "t", "version": "1" } } }),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }),
        json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "view", "arguments": {} } }),
    ] {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);
    let lines: Vec<String> = BufReader::new(child.stdout.take().unwrap()).lines().map(Result::unwrap).collect();
    let out = child.wait_with_output().unwrap();
    std::fs::remove_dir_all(&home).ok();
    assert!(out.status.success(), "it ends when the client closes its stdin: {}", String::from_utf8_lossy(&out.stderr));
    let answers: Vec<Value> = lines.iter().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("stdout carries only JSON-RPC: {l:?}: {e}"))).collect();
    assert_eq!(answers.iter().map(|a| a["id"].as_i64().unwrap_or(0)).collect::<Vec<_>>(), [1, 2, 3], "{answers:?}");
    assert!(answers.iter().all(|a| a["jsonrpc"] == "2.0"));
    assert_eq!(answers[0]["result"]["protocolVersion"], "2025-06-18");
    let tools: Vec<&str> = answers[1]["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(tools, ["view"], "read-only: the described query alone");
    assert_eq!(answers[2]["result"]["content"][0]["text"], "<chat>\n</chat>");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3, "status at initialize and at tools/list, and the call: {seen:?}");
    for (method, path, acting_for, agent, signed, _) in seen.iter() {
        assert_eq!((agent.as_deref(), *signed), (Some("hands.paul"), false), "{method} {path} names the agent and carries no signature");
        assert_eq!(acting_for.as_deref(), Some("id:paul"), "{method} {path} acts for the owner");
    }
    assert_eq!(seen[2].1, "/api/f/mind.paul/ops/view");
    assert!(seen[2].5["id"].as_str().is_some_and(|id| id.starts_with("mcp-")), "{}", seen[2].5);
}

/// A server that cannot start says why on stderr and leaves stdout, the
/// protocol's, empty: an agent without its computer's API, and a person
/// with no key.
#[test]
fn a_server_that_cannot_start_says_why_on_stderr() {
    let home = empty_home("refused");
    let out = fragment(&home).args(["mcp", "mind.paul"]).env("FRAGMENT_AS_AGENT", "hands.paul").stdin(Stdio::null()).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stderr).contains("FRAGMENT_API"), "{}", String::from_utf8_lossy(&out.stderr));
    let out = fragment(&home).args(["mcp", "mind.paul"]).stdin(Stdio::null()).output().unwrap();
    std::fs::remove_dir_all(&home).ok();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fragment login"), "{}", String::from_utf8_lossy(&out.stderr));
}
