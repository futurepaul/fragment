//! OpenRouter, for the surface the platform calls (phase 2 slice F): chat
//! completions, image generation (`POST /api/v1/images`), and asynchronous
//! video generation (create, poll, download), each checking the bearer key.
//! Answers are deterministic from the request, so tests can assert on them.
//! Levers: the calls made, and failures queued for the next calls.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// A recorded call: (method, path, model, authorization).
/// (method, path, model, authorization, the body's `reasoning` as JSON or "").
pub type Call = (String, String, String, String, String);

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    failures: VecDeque<u16>,
    videos: HashMap<String, usize>,
}

pub struct OpenRouter {
    pub url: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

/// The bytes an image prompt makes: a PNG signature and filler; a prompt
/// containing "large" makes one over 1 MiB.
pub fn image_bytes(prompt: &str) -> Vec<u8> {
    let n = if prompt.contains("large") { 1024 * 1024 + 4096 } else { 2048 };
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    out.extend(prompt.bytes().cycle().take(n));
    out
}

/// The bytes a video of `seconds` makes (always over 1 MiB: a blob).
pub fn video_bytes(seconds: usize) -> Vec<u8> {
    (0..seconds.max(1) * 256 * 1024).map(|i| (i % 251) as u8).collect()
}

fn problem(status: u16, message: &str) -> Response {
    Response::json(status, &json!({ "error": { "code": status, "message": message } }))
}

impl OpenRouter {
    /// Serves on a free port; `key` is the bearer every call must carry.
    pub fn start(key: &str) -> std::io::Result<OpenRouter> {
        let state: Arc<Mutex<State>> = Arc::default();
        let (st, expected) = (Arc::clone(&state), format!("Bearer {key}"));
        let base = Arc::new(Mutex::new(String::new()));
        let base_in = Arc::clone(&base);
        let handler: Handler = Arc::new(move |req: &Request| {
            let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
            let auth = req.header("authorization").unwrap_or("").to_string();
            let mut s = st.lock().expect("openrouter state");
            let reasoning = if body["reasoning"].is_null() { String::new() } else { body["reasoning"].to_string() };
            s.calls.push((req.method.clone(), req.path.clone(), body["model"].as_str().unwrap_or("").to_string(), auth.clone(), reasoning));
            if auth != expected {
                return problem(401, "No auth credentials found");
            }
            if let Some(status) = s.failures.pop_front() {
                return problem(status, "a failure the test asked for");
            }
            let base = base_in.lock().expect("base").clone();
            let path = req.path.as_str();
            match (req.method.as_str(), path) {
                ("POST", "/api/v1/chat/completions") => {
                    let last = body["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].clone()).unwrap_or(Value::Null);
                    Response::json(
                        200,
                        &json!({
                            "id": "chatcmpl-fake", "object": "chat.completion", "created": 0, "model": body["model"],
                            "choices": [{ "index": 0, "finish_reason": "stop", "message": { "role": "assistant", "content": format!("echo: {}", last.as_str().unwrap_or("")) } }],
                            "usage": { "prompt_tokens": 3, "completion_tokens": 3, "total_tokens": 6 },
                        }),
                    )
                }
                ("POST", "/api/v1/images") => {
                    let bytes = image_bytes(body["prompt"].as_str().unwrap_or(""));
                    Response::json(
                        200,
                        &json!({ "created": 0, "data": [{ "b64_json": base64::engine::general_purpose::STANDARD.encode(bytes), "media_type": "image/png" }], "usage": { "cost": 0.0 } }),
                    )
                }
                ("POST", "/api/v1/videos") => {
                    let id = format!("gen-vid-{}", s.videos.len() + 1);
                    s.videos.insert(id.clone(), body["duration"].as_u64().unwrap_or(5) as usize);
                    Response::json(202, &json!({ "id": id, "generation_id": id, "polling_url": format!("/api/v1/videos/{id}"), "status": "pending" }))
                }
                ("GET", p) if p.starts_with("/api/v1/videos/") => {
                    let rest = &p["/api/v1/videos/".len()..];
                    let (id, content) = match rest.strip_suffix("/content") {
                        Some(id) => (id, true),
                        None => (rest, false),
                    };
                    let Some(seconds) = s.videos.get(id).copied() else { return problem(404, "no such video job") };
                    if content {
                        return Response::bytes(200, "video/mp4", video_bytes(seconds));
                    }
                    Response::json(
                        200,
                        &json!({
                            "id": id, "status": "completed",
                            "unsigned_urls": [format!("{base}/api/v1/videos/{id}/content?index=0")],
                            "usage": { "cost": 0.0, "is_byok": false },
                        }),
                    )
                }
                _ => problem(404, "no such route"),
            }
        });
        let server = Server::start(0, handler)?;
        *base.lock().expect("base") = server.url.clone();
        Ok(OpenRouter { url: server.url.clone(), state, _server: server })
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state.lock().expect("openrouter state").calls.clone()
    }

    /// The next calls answer these statuses, in order.
    pub fn fail_next(&self, statuses: &[u16]) {
        self.state.lock().expect("openrouter state").failures.extend(statuses);
    }
}
