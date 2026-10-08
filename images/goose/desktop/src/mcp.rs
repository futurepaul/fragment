//! Just enough MCP (modelcontextprotocol.io, protocol 2025-06-18) over
//! stdio for goose: JSON-RPC 2.0, one message a line each way. A server of
//! our own (`serve`: `initialize`, `tools/list`, `tools/call`, `ping`), and
//! the pieces the proxy (proxy.rs) reads and writes.

use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// A line (one message) is at most this many bytes either way: a tool's
/// result with a screenshot in it is a few MiB of base64. Past it the
/// peer is broken, and the line is refused.
pub const LINE_MAX_BYTES: usize = 32 * 1024 * 1024;
/// The protocol version answered when the client names none we know.
pub const PROTOCOL: &str = "2025-06-18";

/// Reads one line of at most `max` bytes (its `\n` dropped): `None` at the
/// end, an error past the bound.
pub async fn read_line<R: AsyncBufRead + Unpin>(r: &mut R, buf: &mut Vec<u8>, max: usize) -> std::io::Result<Option<()>> {
    buf.clear();
    // bounded by `max`: each pass takes bytes or ends
    loop {
        let available = r.fill_buf().await?;
        if available.is_empty() {
            return Ok((!buf.is_empty()).then_some(()));
        }
        let (take, done) = match available.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (available.len(), false),
        };
        if buf.len() + take > max {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("a line past {max} bytes")));
        }
        buf.extend_from_slice(&available[..take]);
        r.consume(if done { take + 1 } else { take });
        if done {
            return Ok(Some(()));
        }
    }
}

/// A tool's result of text, an error or not.
pub fn text_result(text: &str, error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": error })
}

pub fn answer(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A tool as `tools/list` lists it.
pub fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({ "name": name, "description": description, "inputSchema": schema })
}

/// Our own tools: their list, and a call's result (an `isError` result for
/// a failure the model should read; never a JSON-RPC error).
pub trait Tools: Send + Sync + 'static {
    fn list(&self) -> Vec<Value>;
    fn call(&self, name: &str, args: Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Value> + Send + '_>>;
}

/// Serves `tools` on stdio until stdin ends: `name` and `instructions` in
/// its `initialize` answer.
pub async fn serve(name: &str, instructions: &str, tools: std::sync::Arc<dyn Tools>) -> std::io::Result<()> {
    let out = std::sync::Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
    let mut input = BufReader::new(tokio::io::stdin());
    let mut buf = Vec::new();
    let mut calls = tokio::task::JoinSet::new();
    // bounded by stdin: one message per pass
    while read_line(&mut input, &mut buf, LINE_MAX_BYTES).await?.is_some() {
        let Ok(m) = serde_json::from_slice::<Value>(&buf) else { continue };
        let Some(id) = m.get("id").cloned() else { continue };
        let reply = match m["method"].as_str() {
            Some("initialize") => {
                let version = m["params"]["protocolVersion"].as_str().filter(|v| v.starts_with("20")).unwrap_or(PROTOCOL);
                answer(&id, json!({ "protocolVersion": version, "capabilities": { "tools": { "listChanged": false } }, "serverInfo": { "name": name, "version": env!("CARGO_PKG_VERSION") }, "instructions": instructions }))
            }
            Some("ping") => answer(&id, json!({})),
            Some("tools/list") => answer(&id, json!({ "tools": tools.list() })),
            Some("tools/call") => {
                let (tools, out) = (tools.clone(), out.clone());
                calls.spawn(async move {
                    let name = m["params"]["name"].as_str().unwrap_or("").to_string();
                    let args = m["params"]["arguments"].clone();
                    let result = tools.call(&name, args).await;
                    let _ = write(&out, &answer(&id, result)).await;
                });
                continue;
            }
            Some(other) => error(&id, -32601, &format!("{other} is not offered")),
            None => continue,
        };
        write(&out, &reply).await?;
    }
    // what was asked before stdin ended is still answered
    while calls.join_next().await.is_some() {}
    Ok(())
}

/// Writes one message as one line.
pub async fn write<W: AsyncWrite + Unpin>(out: &tokio::sync::Mutex<W>, m: &Value) -> std::io::Result<()> {
    let mut line = m.to_string();
    line.push('\n');
    let mut w = out.lock().await;
    w.write_all(line.as_bytes()).await?;
    w.flush().await
}

/// A text cut to at most `max` bytes, on a character boundary.
pub fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lines_are_bounded() {
        let mut r = BufReader::with_capacity(4, &b"{\"a\":1}\n0123456789\nlast"[..]);
        let mut buf = Vec::new();
        assert_eq!(read_line(&mut r, &mut buf, 16).await.unwrap(), Some(()));
        assert_eq!(buf, b"{\"a\":1}");
        assert!(read_line(&mut r, &mut buf, 8).await.is_err(), "past its bound");
    }

    #[test]
    fn cuts_keep_characters_whole() {
        assert_eq!(cut("héllo", 2), "h");
        assert_eq!(cut("héllo", 3), "hé");
        assert_eq!(cut("hi", 10), "hi");
    }
}
