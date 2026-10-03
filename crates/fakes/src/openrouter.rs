//! OpenRouter, for the surface the platform still calls until phase 7
//! (cell/src/ai.rs): image generation (`POST /api/v1/images`) and
//! asynchronous video generation (create, poll, download), each checking
//! the deployment's key. Text goes through the platform's model route, so
//! the fake has no chat completions (`workers_ai` stands there). Answers
//! are deterministic from the request, so tests can assert on them. Every
//! answer reports its cost (`usage.cost`, dollars) unless told to leave it
//! out. A video's prompt decides how it ends: one naming "expire" expires
//! and one naming "cancel" is cancelled (no cost, nothing to save), one
//! naming "vanish" is forgotten (its polls answer 404); any other
//! completes. Levers: the calls made, failures queued for the next calls,
//! the costs, and whether costs are reported.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// A recorded call: (method, path, model, authorization).
pub type Call = (String, String, String, String);

/// What each answer costs, in dollars.
#[derive(Clone, Copy, Debug)]
pub struct Costs {
    pub image: f64,
    pub video_per_s: f64,
}

impl Default for Costs {
    fn default() -> Costs {
        Costs { image: 0.002, video_per_s: 0.01 }
    }
}

#[derive(Default)]
struct State {
    calls: Vec<Call>,
    failures: VecDeque<u16>,
    /// video id → (seconds, cost, status)
    videos: HashMap<String, (usize, f64, &'static str)>,
    /// Answers leave out `usage.cost`.
    costless: bool,
    video_ids: usize,
    costs: Costs,
}

/// One request's answer, from the state (locked by the caller).
fn answer(s: &mut State, req: &Request, expected: &str, base: &str) -> Response {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let auth = req.header("authorization").unwrap_or("").to_string();
    s.calls.push((req.method.clone(), req.path.clone(), body["model"].as_str().unwrap_or("").to_string(), auth.clone()));
    if auth != expected {
        return problem(401, "No auth credentials found");
    }
    if let Some(status) = s.failures.pop_front() {
        return problem(status, "a failure the test asked for");
    }
    let (costs, costless) = (s.costs, s.costless);
    let path = req.path.as_str();
    match (req.method.as_str(), path) {
        ("POST", "/api/v1/images") => {
            let bytes = image_bytes(body["prompt"].as_str().unwrap_or(""));
            let usage = with_cost(json!({}), costs.image, costless);
            Response::json(
                200,
                &json!({ "created": 0, "data": [{ "b64_json": base64::engine::general_purpose::STANDARD.encode(bytes), "media_type": "image/png" }], "usage": usage }),
            )
        }
        ("POST", "/api/v1/videos") => {
            let seconds = body["duration"].as_u64().unwrap_or(5) as usize;
            let cost = costs.video_per_s * seconds as f64;
            s.video_ids += 1;
            let id = format!("gen-vid-{}", s.video_ids);
            let prompt = body["prompt"].as_str().unwrap_or("");
            if !prompt.contains("vanish") {
                let status = if prompt.contains("expire") {
                    "expired"
                } else if prompt.contains("cancel") {
                    "cancelled"
                } else {
                    "completed"
                };
                s.videos.insert(id.clone(), (seconds, cost, status));
            }
            Response::json(202, &json!({ "id": id, "generation_id": id, "polling_url": format!("/api/v1/videos/{id}"), "status": "pending" }))
        }
        ("GET", p) if p.starts_with("/api/v1/videos/") => {
            let rest = &p["/api/v1/videos/".len()..];
            let (id, content) = match rest.strip_suffix("/content") {
                Some(id) => (id, true),
                None => (rest, false),
            };
            let Some((seconds, cost, status)) = s.videos.get(id).copied() else { return problem(404, "no such video job") };
            if status != "completed" {
                return Response::json(200, &json!({ "id": id, "status": status, "error": format!("the generation ended {status}") }));
            }
            if content {
                return Response::bytes(200, "video/mp4", video_bytes(seconds));
            }
            let usage = with_cost(json!({ "is_byok": false }), cost, costless);
            Response::json(
                200,
                &json!({
                    "id": id, "status": "completed",
                    "unsigned_urls": [format!("{base}/api/v1/videos/{id}/content?index=0")],
                    "usage": usage,
                }),
            )
        }
        _ => problem(404, "no such route"),
    }
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

/// An answer's `usage`, with its cost unless costs are left out.
fn with_cost(mut usage: Value, cost: f64, costless: bool) -> Value {
    if !costless {
        usage["cost"] = json!(cost);
    }
    usage
}

fn problem(status: u16, message: &str) -> Response {
    Response::json(status, &json!({ "error": { "code": status, "message": message } }))
}

impl OpenRouter {
    /// Serves on a free port: `key` is the deployment's, which calls carry.
    pub fn start(key: &str) -> std::io::Result<OpenRouter> {
        let state: Arc<Mutex<State>> = Arc::default();
        let (st, expected) = (Arc::clone(&state), format!("Bearer {key}"));
        let base = Arc::new(Mutex::new(String::new()));
        let base_in = Arc::clone(&base);
        let handler: Handler = Arc::new(move |req: &Request| {
            let base = base_in.lock().expect("base").clone();
            answer(&mut st.lock().expect("openrouter state"), req, &expected, &base)
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

    /// What the next answers cost.
    pub fn set_costs(&self, costs: Costs) {
        self.state.lock().expect("openrouter state").costs = costs;
    }

    /// Whether answers leave out `usage.cost` (a provider that reports none).
    pub fn omit_costs(&self, omit: bool) {
        self.state.lock().expect("openrouter state").costless = omit;
    }
}
