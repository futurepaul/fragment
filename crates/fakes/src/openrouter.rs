//! OpenRouter, for the surface the platform calls (phase 2 slice F): chat
//! completions, image generation (`POST /api/v1/images`), and asynchronous
//! video generation (create, poll, download), each checking the bearer key.
//! Answers are deterministic from the request, so tests can assert on them.
//! Chat completions stream (server-sent events, OpenAI's chunk format) when
//! asked to, and answer scripted replies (text, tool calls, nothing, or
//! reasoning alone) in order before falling back to an echo. A streamed
//! answer keeps to the request's `max_tokens` (`CHARS_PER_TOKEN` characters
//! each): one longer is cut there and ends `length`, as the service's do.
//! An answer can be held back first (`delay_next`), as a slow model's is,
//! its script consumed as the request arrives. Every answer reports its cost
//! (`usage.cost`, dollars) unless told to leave it out. A video's prompt
//! decides how it ends: one naming "expire" expires and one naming
//! "cancel" is cancelled (no cost, nothing to save), one naming "vanish"
//! is forgotten (its polls answer 404); any other completes. The management key mints keys with a credit limit
//! (`POST /api/v1/keys`, `PATCH /api/v1/keys/{hash}`), and a call on a
//! minted key past its limit is 402. Levers: the calls made, the chat
//! requests, failures queued for the next calls, the script, the costs,
//! whether costs are reported, and the keys minted.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{json, Value};

use crate::http::{Handler, Request, Response, Server};

/// A recorded call: (method, path, model, authorization).
/// (method, path, model, authorization, the body's `reasoning` as JSON or "").
pub type Call = (String, String, String, String, String);

/// A scripted model reply.
#[derive(Clone, Debug)]
pub enum Reply {
    Text(String),
    /// Tool calls: (name, arguments).
    Tools(Vec<(String, Value)>),
    /// No text and no tool call.
    Empty,
    /// Reasoning, and nothing else.
    Thinking(String),
}

/// The characters one token of a streamed answer stands for, where the
/// fake keeps to a request's `max_tokens`.
pub const CHARS_PER_TOKEN: usize = 4;
/// A tool call's arguments stream in pieces of at most this many characters.
const ARGS_PIECE_CHARS: usize = 1024;

/// A key the management key minted.
#[derive(Clone, Debug)]
pub struct Minted {
    pub hash: String,
    pub key: String,
    pub name: String,
    /// Dollars; `None` is no limit.
    pub limit: Option<f64>,
    pub limit_reset: Option<String>,
    /// Dollars spent on it, ever, and since its limit last reset.
    pub usage: f64,
    pub usage_period: f64,
}

/// What each answer costs, in dollars.
#[derive(Clone, Copy, Debug)]
pub struct Costs {
    pub text: f64,
    pub image: f64,
    pub video_per_s: f64,
}

impl Default for Costs {
    fn default() -> Costs {
        Costs { text: 0.001, image: 0.002, video_per_s: 0.01 }
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
    script: VecDeque<Reply>,
    chats: Vec<Value>,
    tool_calls: u64,
    video_ids: usize,
    costs: Costs,
    minted: Vec<Minted>,
    /// (hash, body) of each PATCH.
    patches: Vec<(String, Value)>,
    /// How long the next chat completions wait before answering, in order.
    delays: VecDeque<u64>,
    /// The wait the answer being made takes (set by a chat completion).
    sleep_ms: u64,
}

impl State {
    /// Charges `cost` to the key the call carried: past a minted key's
    /// limit it is refused (402).
    fn charge(&mut self, auth: &str, cost: f64) -> Result<(), Response> {
        let Some(m) = self.minted.iter_mut().find(|m| auth == format!("Bearer {}", m.key)) else { return Ok(()) };
        // a limit with a reset is the period's; without one, forever
        let counted = if m.limit_reset.is_some() { m.usage_period } else { m.usage };
        if let Some(limit) = m.limit {
            if counted + cost > limit + 1e-9 {
                return Err(problem(402, "Key limit exceeded"));
            }
        }
        m.usage += cost;
        m.usage_period += cost;
        Ok(())
    }
}

fn chunk(id: &str, model: &Value, delta: Value, finish: Option<&str>, usage: Option<f64>) -> String {
    let mut c = json!({
        "id": id, "object": "chat.completion.chunk", "created": 0, "model": model,
        "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
    });
    if let Some(cost) = usage {
        c["usage"] = json!({ "prompt_tokens": 3, "completion_tokens": 3, "total_tokens": 6, "cost": cost });
    }
    format!("data: {c}\n\n")
}

/// At most `budget` characters of `text` (all of it with no budget), and
/// whether it was cut.
fn within(text: &str, budget: &mut Option<usize>) -> (String, bool) {
    let Some(left) = budget.as_mut() else { return (text.to_string(), false) };
    let kept: String = text.chars().take(*left).collect();
    let n = kept.chars().count();
    *left -= n;
    (kept, n < text.chars().count())
}

/// A reply as server-sent events (OpenAI's streaming format); each reply
/// has its own id, as the service's generations do. `budget`: the
/// characters the request's `max_tokens` allows; a reply longer is cut
/// there and ends `length`.
fn stream(model: &Value, reply: &Reply, ids: &mut u64, cost: Option<f64>, mut budget: Option<usize>) -> String {
    let mut out = String::new();
    *ids += 1;
    let id = format!("chatcmpl-fake-{ids}");
    let finish = |cut: bool, whole: &'static str| if cut { "length" } else { whole };
    match reply {
        Reply::Text(text) => {
            let (text, cut) = within(text, &mut budget);
            out += &chunk(&id, model, json!({ "role": "assistant", "content": text }), None, None);
            out += &chunk(&id, model, json!({}), Some(finish(cut, "stop")), cost);
        }
        Reply::Empty => {
            out += &chunk(&id, model, json!({ "role": "assistant", "content": "" }), None, None);
            out += &chunk(&id, model, json!({}), Some("stop"), cost);
        }
        Reply::Thinking(text) => {
            out += &chunk(&id, model, json!({ "role": "assistant", "reasoning": text }), None, None);
            out += &chunk(&id, model, json!({}), Some("stop"), cost);
        }
        Reply::Tools(calls) => {
            let mut cut = false;
            for (i, (name, args)) in calls.iter().enumerate() {
                if cut {
                    break;
                }
                *ids += 1;
                let (args, was_cut) = within(&args.to_string(), &mut budget);
                cut = was_cut;
                // the call's id and name, then its arguments in pieces
                let chars: Vec<char> = args.chars().collect();
                let mut pieces: Vec<String> = chars.chunks(ARGS_PIECE_CHARS).map(|c| c.iter().collect()).collect();
                if pieces.is_empty() {
                    pieces.push(String::new());
                }
                for (n, piece) in pieces.iter().enumerate() {
                    let call = match n {
                        0 => json!({ "index": i, "id": format!("call_{ids}"), "type": "function", "function": { "name": name, "arguments": piece } }),
                        _ => json!({ "index": i, "function": { "arguments": piece } }),
                    };
                    let delta = match (i, n) {
                        (0, 0) => json!({ "role": "assistant", "tool_calls": [call] }),
                        _ => json!({ "tool_calls": [call] }),
                    };
                    out += &chunk(&id, model, delta, None, None);
                }
            }
            out += &chunk(&id, model, json!({}), Some(finish(cut, "tool_calls")), cost);
        }
    }
    out + "data: [DONE]\n\n"
}

/// One request's answer, from the state (locked by the caller).
fn answer(s: &mut State, req: &Request, expected: &str, manager: &str, base_in: &Mutex<String>) -> Response {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let auth = req.header("authorization").unwrap_or("").to_string();
    let reasoning = if body["reasoning"].is_null() { String::new() } else { body["reasoning"].to_string() };
    s.calls.push((req.method.clone(), req.path.clone(), body["model"].as_str().unwrap_or("").to_string(), auth.clone(), reasoning));
    let path = req.path.as_str();
    if path == "/api/v1/keys" || path.starts_with("/api/v1/keys/") {
        if auth != manager {
            return problem(401, "a management key is required");
        }
        return match (req.method.as_str(), path.strip_prefix("/api/v1/keys/")) {
            ("POST", None) => {
                let n = s.minted.len() + 1;
                let m = Minted {
                    hash: format!("{:064x}", n),
                    key: format!("sk-or-v1-minted-{n:04}-{}", "f".repeat(40)),
                    name: body["name"].as_str().unwrap_or("").to_string(),
                    limit: body["limit"].as_f64(),
                    limit_reset: body["limit_reset"].as_str().map(str::to_string),
                    usage: 0.0,
                    usage_period: 0.0,
                };
                s.minted.push(m.clone());
                Response::json(
                    201,
                    &json!({ "data": { "hash": m.hash, "name": m.name, "label": format!("sk-or-v1-min...{n:04}"), "limit": m.limit, "limit_reset": m.limit_reset, "disabled": false, "usage": 0 }, "key": m.key }),
                )
            }
            ("PATCH", Some(hash)) => {
                let hash = hash.to_string();
                let Some(m) = s.minted.iter_mut().find(|m| m.hash == hash) else { return problem(404, "no such key") };
                if let Some(l) = body.get("limit") {
                    m.limit = l.as_f64();
                }
                let data = json!({ "hash": m.hash, "limit": m.limit, "limit_reset": m.limit_reset, "usage": m.usage });
                s.patches.push((hash, body.clone()));
                Response::json(200, &json!({ "data": data }))
            }
            _ => problem(404, "no such route"),
        };
    }
    let minted = s.minted.iter().any(|m| auth == format!("Bearer {}", m.key));
    if auth != expected && !minted {
        return problem(401, "No auth credentials found");
    }
    if let Some(status) = s.failures.pop_front() {
        return problem(status, "a failure the test asked for");
    }
    let base = base_in.lock().expect("base").clone();
    let costs = s.costs;
    let costless = s.costless;
    match (req.method.as_str(), path) {
        ("POST", "/api/v1/chat/completions") => {
            if let Err(r) = s.charge(&auth, costs.text) {
                return r;
            }
            let last = body["messages"].as_array().and_then(|m| m.last()).map(|m| m["content"].clone()).unwrap_or(Value::Null);
            s.chats.push(body.clone());
            let scripted = s.script.pop_front();
            s.sleep_ms = s.delays.pop_front().unwrap_or(0);
            if body["stream"] == true {
                let reply = scripted.unwrap_or_else(|| Reply::Text(format!("echo: {}", last.as_str().unwrap_or(""))));
                let cost = (!costless).then_some(costs.text);
                let budget = body["max_tokens"].as_u64().or(body["max_completion_tokens"].as_u64()).map(|t| t as usize * CHARS_PER_TOKEN);
                let events = stream(&body["model"], &reply, &mut s.tool_calls, cost, budget);
                return Response::bytes(200, "text/event-stream", events.into_bytes());
            }
            let usage = with_cost(json!({ "prompt_tokens": 3, "completion_tokens": 3, "total_tokens": 6 }), costs.text, costless);
            Response::json(
                200,
                &json!({
                    "id": "chatcmpl-fake", "object": "chat.completion", "created": 0, "model": body["model"],
                    "choices": [{ "index": 0, "finish_reason": "stop", "message": { "role": "assistant", "content": format!("echo: {}", last.as_str().unwrap_or("")) } }],
                    "usage": usage,
                }),
            )
        }
        ("POST", "/api/v1/images") => {
            if let Err(r) = s.charge(&auth, costs.image) {
                return r;
            }
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
            if let Err(r) = s.charge(&auth, cost) {
                return r;
            }
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
    /// Serves on a free port: `key` is a key calls may carry (a fragment's
    /// own), `management` the key that mints more.
    pub fn start(key: &str, management: &str) -> std::io::Result<OpenRouter> {
        let state: Arc<Mutex<State>> = Arc::default();
        let (st, expected, manager) = (Arc::clone(&state), format!("Bearer {key}"), format!("Bearer {management}"));
        let base = Arc::new(Mutex::new(String::new()));
        let base_in = Arc::clone(&base);
        let handler: Handler = Arc::new(move |req: &Request| {
            // an answer held back (`delay_next`) waits here, with the state
            // unlocked, after its script was consumed
            let (response, sleep_ms) = {
                let mut s = st.lock().expect("openrouter state");
                let response = answer(&mut s, req, &expected, &manager, &base_in);
                (response, std::mem::take(&mut s.sleep_ms))
            };
            if sleep_ms > 0 {
                std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
            }
            response
        });
        let server = Server::start(0, handler)?;
        *base.lock().expect("base") = server.url.clone();
        Ok(OpenRouter { url: server.url.clone(), state, _server: server })
    }

    /// Replies the next streamed chat completions answer, in order.
    pub fn script(&self, replies: &[Reply]) {
        self.state.lock().expect("openrouter state").script.extend(replies.iter().cloned());
    }

    /// Drops any scripted replies not yet answered, and any delays.
    pub fn clear_script(&self) {
        let mut s = self.state.lock().expect("openrouter state");
        s.script.clear();
        s.delays.clear();
    }

    /// Holds the next chat completions' answers back this long (ms), in
    /// order, as a slow model's are: a client that gives up first sees none.
    pub fn delay_next(&self, ms: &[u64]) {
        self.state.lock().expect("openrouter state").delays.extend(ms);
    }

    /// The chat completion request bodies, in order.
    pub fn chats(&self) -> Vec<Value> {
        self.state.lock().expect("openrouter state").chats.clone()
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

    /// The keys the management key minted, with what each has spent.
    pub fn minted(&self) -> Vec<Minted> {
        self.state.lock().expect("openrouter state").minted.clone()
    }

    /// A new period begins for keys whose limits reset (the test moved the
    /// platform's clock past a month).
    pub fn reset_period(&self) {
        for m in self.state.lock().expect("openrouter state").minted.iter_mut().filter(|m| m.limit_reset.is_some()) {
            m.usage_period = 0.0;
        }
    }

    /// Each PATCH of a minted key: its hash and body.
    pub fn patches(&self) -> Vec<(String, Value)> {
        self.state.lock().expect("openrouter state").patches.clone()
    }
}
