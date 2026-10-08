//! An MCP server of someone else's (the browser's, the computer's), run
//! behind a gate of ours on stdio, between goose and it:
//!
//! - **Take over holds the agent back.** A tool call while a person holds
//!   the agent's screen (its lease, lease.rs: the screen's Take over) is
//!   answered here, refused (`human_has_control`), and never reaches the
//!   server: the agent waits while its owner drives.
//! - **A call is a use.** Each one marks the desktop used (its activity
//!   file), and starts it first when it is down (`fragment-desktop start`,
//!   a process of its own, so the desktop outlives this one).
//! - **No images reach the model.** goose's model reads no images (GLM): an
//!   image in a result is replaced by a line saying where to look instead.
//! - **Tools of ours** (`extra`) join the server's list and are answered
//!   here.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufReader};

use crate::desk::Desk;
use crate::mcp::{self, Tools};

/// What a refused call says: the model reads it.
pub const HELD: &str = "human_has_control: your owner has taken over your screen (Take over) and is using it now. Do not act on the screen or the browser until they give it back: wait a little and try again, or carry on with work that needs no screen.";
/// What an image left out says.
pub const NO_IMAGE: &str = "(an image was left out here: you read no images. To ask what the screen shows, call screen_look with your question.)";
/// The server is given this long to end once goose is gone.
pub const END_GRACE_MS: u64 = 3_000;

pub struct Proxy {
    /// The server: its program and arguments, and what it is given beyond
    /// this process's environment.
    pub command: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The desktop whose lease gates its calls, used and started by them.
    pub desk: Option<Desk>,
    /// Tools of ours, beside the server's.
    pub extra: Option<Arc<dyn Tools>>,
}

/// A result with every image replaced by `NO_IMAGE` (once, however many).
pub fn without_images(result: &mut Value) -> bool {
    let Some(content) = result.get_mut("content").and_then(Value::as_array_mut) else { return false };
    let before = content.len();
    content.retain(|c| c["type"] != "image");
    let dropped = content.len() != before;
    if dropped {
        content.push(json!({ "type": "text", "text": NO_IMAGE }));
    }
    dropped
}

/// The names `extra` answers.
fn names(extra: &Option<Arc<dyn Tools>>) -> HashSet<String> {
    extra.as_ref().map(|t| t.list().iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Starts the desktop when it is down: `fragment-desktop start <agent>`, so
/// its supervisor is never this process's child.
pub async fn ensure_up(desk: &Desk) -> Result<(), String> {
    let display = crate::desk::allocate(desk)?;
    if crate::desk::up(desk, display) {
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = tokio::process::Command::new(exe).args(["start", &desk.agent]).env("FRAGMENT_DESKTOPS", &desk.root).output().await.map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().lines().last().unwrap_or("the desktop did not start").to_string())
    }
}

impl Proxy {
    /// Runs until goose closes stdin (then the server is ended) or the
    /// server ends.
    pub async fn run(self) -> std::io::Result<()> {
        let (program, args) = self.command.split_first().ok_or_else(|| std::io::Error::other("no server to run"))?;
        let mut child = tokio::process::Command::new(program).args(args).envs(self.env.iter().cloned()).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::inherit()).kill_on_drop(true).spawn()?;
        let mut to_server = child.stdin.take().expect("piped");
        let from_server = child.stdout.take().expect("piped");
        let out = Arc::new(tokio::sync::Mutex::new(tokio::io::stdout()));
        let lists: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
        let ours = names(&self.extra);

        // the server's lines, to goose
        let down = {
            let (out, lists, extra) = (out.clone(), lists.clone(), self.extra.clone());
            tokio::spawn(async move {
                let mut r = BufReader::new(from_server);
                let mut buf = Vec::new();
                // bounded by the server's stdout
                while let Ok(Some(())) = mcp::read_line(&mut r, &mut buf, mcp::LINE_MAX_BYTES).await {
                    let Ok(mut m) = serde_json::from_slice::<Value>(&buf) else { continue };
                    let listed = m.get("id").is_some_and(|id| lists.lock().expect("lists").remove(&id.to_string()));
                    if let Some(result) = m.get_mut("result") {
                        if listed {
                            if let (Some(tools), Some(extra)) = (result.get_mut("tools").and_then(Value::as_array_mut), &extra) {
                                tools.extend(extra.list());
                            }
                        }
                        without_images(result);
                    }
                    if mcp::write(&out, &m).await.is_err() {
                        return;
                    }
                }
            })
        };

        // goose's lines, to the server
        let mut input = BufReader::new(tokio::io::stdin());
        let mut buf = Vec::new();
        let mut calls = tokio::task::JoinSet::new();
        // bounded by stdin: one message per pass
        while let Ok(Some(())) = mcp::read_line(&mut input, &mut buf, mcp::LINE_MAX_BYTES).await {
            let Ok(m) = serde_json::from_slice::<Value>(&buf) else { continue };
            let id = m.get("id").cloned();
            match (m["method"].as_str(), &id) {
                (Some("tools/list"), Some(id)) => {
                    lists.lock().expect("lists").insert(id.to_string());
                }
                (Some("tools/call"), Some(id)) => {
                    let name = m["params"]["name"].as_str().unwrap_or("").to_string();
                    if let (true, Some(extra)) = (ours.contains(&name), self.extra.clone()) {
                        let (out, id, args) = (out.clone(), id.clone(), m["params"]["arguments"].clone());
                        calls.spawn(async move {
                            let result = extra.call(&name, args).await;
                            let _ = mcp::write(&out, &mcp::answer(&id, result)).await;
                        });
                        continue;
                    }
                    if let Some(desk) = &self.desk {
                        if desk.held() {
                            fragment_bridge::ev!("desktop.held_back", { "agent": desk.agent, "tool": name });
                            mcp::write(&out, &mcp::answer(id, mcp::text_result(HELD, true))).await?;
                            continue;
                        }
                        desk.touch();
                        if let Err(e) = ensure_up(desk).await {
                            mcp::write(&out, &mcp::answer(id, mcp::text_result(&format!("your desktop did not start: {e}"), true))).await?;
                            continue;
                        }
                    }
                }
                _ => {}
            }
            let mut line = buf.clone();
            line.push(b'\n');
            if to_server.write_all(&line).await.is_err() || to_server.flush().await.is_err() {
                break;
            }
        }
        drop(to_server);
        while calls.join_next().await.is_some() {}
        if tokio::time::timeout(Duration::from_millis(END_GRACE_MS), child.wait()).await.is_err() {
            let _ = child.kill().await;
        }
        let _ = tokio::time::timeout(Duration::from_millis(END_GRACE_MS), down).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Images go, once, whatever else the result said stays.
    #[test]
    fn images_never_reach_the_model() {
        let mut r = json!({ "content": [{ "type": "text", "text": "took it" }, { "type": "image", "data": "AAAA", "mimeType": "image/png" }, { "type": "image", "data": "BB", "mimeType": "image/png" }] });
        assert!(without_images(&mut r));
        assert_eq!(r["content"], json!([{ "type": "text", "text": "took it" }, { "type": "text", "text": NO_IMAGE }]));
        let mut plain = json!({ "content": [{ "type": "text", "text": "ok" }] });
        assert!(!without_images(&mut plain));
        assert!(!without_images(&mut json!({ "tools": [] })));
    }
}
