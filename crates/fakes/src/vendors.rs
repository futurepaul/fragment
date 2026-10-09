//! The vendors a person's own models call (cell/src/providers,
//! `FRAGMENT_MODELS_UPSTREAM`): Anthropic's Messages API, OpenAI's
//! Responses API, and OpenAI's sign-in (Sign in with ChatGPT's OAuth), told
//! apart by the host a request names (`x-fragment-upstream-host`) or, for
//! the CLI's sign-in, which calls it directly, by its path. A lower rung at
//! the vendor boundary: what the real vendors take is the hosted lane's to
//! prove.
//!
//! - **Anthropic** (`POST /v1/messages`, `GET /v1/models`): a key beginning
//!   `sk-ant-` (but `BAD_KEY`), `anthropic-version: 2023-06-01`, a request
//!   in the API's shape (no field it does not take: a hint left on a block
//!   is refused, as the API refuses extra inputs; at most 4 cache marks; a
//!   conversation that starts with the user). Its answer streams as the API
//!   streams. Its cache is modelled: each mark stores the prefix up to it,
//!   and a mark reads the longest stored prefix ending within 20 blocks
//!   before it (Anthropic's lookback), so `usage` says what was read from
//!   the cache and what was written. A model that thinks (`THINKS`) starts
//!   each answer with a thinking block (empty, signed), and refuses a turn
//!   whose tool call does not carry back the thinking block it signed.
//! - **OpenAI** (`POST /v1/responses`, `GET /v1/models`): an API key
//!   beginning `sk-` (but `BAD_KEY`), or an access token this fake issued
//!   (`chatgpt-at-…`, not expired), under which the preview's rules hold:
//!   `store: false`, `stream: true`, none of the fields it refuses, tools
//!   in a namespace, no system message item. Its answer streams as the
//!   Responses API streams; the models list is the API's shape for a key and
//!   ChatGPT's for a token.
//! - **The sign-in** (`/.well-known/openid-configuration`,
//!   `/api/accounts/authorize`, `/api/accounts/oauth/token`,
//!   `/oauth/revoke`): dynamic registration (`dynamic_agent_client` issues
//!   `oaiapp_fake_<n>`), PKCE (S256), a loopback redirect, the plan's
//!   scopes, the resource, and a host id; a code exchanged once; rotating
//!   refresh tokens (one used twice is refused); revocation.
//!
//! An answer is a text a test set (`say_next`), else a tool call its last
//! message asks for (`[[call NAME {json}]]`, as the Workers AI fake reads
//! one), else after a tool's result `TOOL_SAID` and its first 200
//! characters, else an echo of the last message. Levers: the calls made,
//! failures queued, the access tokens' lifetime, the refreshes and
//! revocations seen.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::http::{Handler, Request, Response, Server};
use crate::workers_ai::{directive, Reply, CHARS_PER_TOKEN, PIECES, TOOL_SAID, TOOL_SAID_CHARS};

/// A key both vendors refuse (401).
pub const BAD_KEY: &str = "sk-ant-refused";
/// Anthropic's models, newest first, as its list names them.
pub const ANTHROPIC_MODELS: [(&str, &str); 4] =
    [("claude-opus-5-5", "Claude Opus 5.5"), ("claude-sonnet-5-5", "Claude Sonnet 5.5"), ("claude-haiku-5-5", "Claude Haiku 5.5"), ("claude-haiku-4-5", "Claude Haiku 4.5")];
/// The models that think by default (adaptive thinking, its text omitted).
pub const THINKS: [&str; 2] = ["claude-opus-5-5", "claude-sonnet-5-5"];
/// ChatGPT's models for a signed-in account (`visibility: list`), and one hidden.
pub const CHATGPT_MODELS: [(&str, &str); 2] = [("gpt-6.1-sol", "GPT-6.1 Sol"), ("gpt-6.1-sol-mini", "GPT-6.1 Sol mini")];
/// The API's models for a key, oldest first by `created`.
pub const OPENAI_MODELS: [&str; 3] = ["text-embedding-3-large", "gpt-5-mini", "gpt-5"];
/// The account a sign-in is for.
pub const CHATGPT_EMAIL: &str = "paul@e2e.test";
/// The resource every grant names, and the plan's scope.
const RESOURCE: &str = "https://api.openai.com/v1";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const SCOPES: [&str; 6] = ["openid", "profile", "email", "offline_access", "resource.invoke", PLAN_SCOPE];
/// Anthropic looks this many blocks back from a mark for a stored prefix.
const LOOKBACK: usize = 20;
/// The fields the preview refuses on a request.
const PREVIEW_REFUSES: [&str; 14] = [
    "background", "conversation", "max_output_tokens", "max_tool_calls", "metadata", "previous_response_id", "prompt", "prompt_cache_retention", "safety_identifier", "temperature", "top_logprobs", "top_p", "truncation", "user",
];

/// One call as the fake took it.
#[derive(Clone, Debug)]
pub struct VendorCall {
    /// `api.anthropic.com`, `api.openai.com` or `auth.openai.com`.
    pub host: String,
    pub method: String,
    pub path: String,
    pub body: Value,
    /// The credential it carried (`x-api-key`, or the bearer token).
    pub credential: Option<String>,
    /// What the answer said it used (`input`, `cached`, `cache_write`,
    /// `output`), when it was a model's.
    pub usage: Option<Value>,
}

/// A code issued and not yet exchanged.
#[derive(Clone)]
struct Pending {
    client_id: String,
    challenge: String,
    redirect_uri: String,
    nonce: String,
}

#[derive(Default)]
struct State {
    calls: Vec<VendorCall>,
    /// Anthropic's cache: the prefixes stored at marks, by their hash.
    cache: HashSet<String>,
    /// The thinking blocks' signatures it gave.
    signatures: HashSet<String>,
    clients: HashSet<String>,
    codes: HashMap<String, Pending>,
    /// Access tokens issued, and when each expires (Unix seconds).
    access: HashMap<String, u64>,
    /// Refresh tokens issued: their client, and whether one was used.
    refresh: HashMap<String, (String, bool)>,
    /// Every authorize request's parameters.
    authorized: Vec<HashMap<String, String>>,
    refreshes: u64,
    revoked: Vec<String>,
    n: u64,
    expires_in: Option<u64>,
    said: VecDeque<String>,
    failures: VecDeque<u16>,
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn sha_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn tokens_of(s: &str) -> u64 {
    (s.len() / CHARS_PER_TOKEN).max(1) as u64
}

/// The `[[call NAME {json}]]` a text asks for, when the call offers it.
fn reply_to(text: Option<&str>, result: Option<&str>, offered: &dyn Fn(&str) -> bool) -> Reply {
    if let Some(r) = result {
        return Reply::Text(format!("{TOOL_SAID}{}", r.chars().take(TOOL_SAID_CHARS).collect::<String>()));
    }
    let text = text.unwrap_or("");
    match directive(text).filter(|(name, _)| offered(name)) {
        Some(call) => Reply::Tools(vec![call]),
        None => Reply::Text(format!("echo: {text}")),
    }
}

/// A text in `PIECES` deltas, so a client's draft grows.
fn pieces(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars.chunks(chars.len().div_ceil(PIECES).max(1)).map(|c| c.iter().collect()).collect()
}

fn sse(events: &[Value]) -> Vec<u8> {
    events.iter().map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap_or("message"))).collect::<String>().into_bytes()
}

fn anthropic_error(status: u16, kind: &str, message: &str) -> Response {
    Response::json(status, &json!({ "type": "error", "error": { "type": kind, "message": message } }))
}

fn openai_error(status: u16, code: &str, message: &str) -> Response {
    Response::json(status, &json!({ "error": { "code": code, "type": code, "message": message, "param": null } }))
}

/// Whether a request's blocks carry a field Anthropic does not take.
fn extra_field(v: &Value) -> Option<String> {
    const BLOCK: [&str; 13] = ["type", "text", "cache_control", "source", "id", "name", "input", "tool_use_id", "content", "is_error", "thinking", "signature", "data"];
    for m in v["messages"].as_array().into_iter().flatten() {
        for b in m["content"].as_array().into_iter().flatten() {
            if let Some(k) = b.as_object().and_then(|o| o.keys().find(|k| !BLOCK.contains(&k.as_str()))) {
                return Some(k.clone());
            }
        }
    }
    None
}

impl State {
    fn fresh(&mut self, prefix: &str) -> String {
        self.n += 1;
        format!("{prefix}{}", self.n)
    }

    /// Anthropic's cache over `body` (the module's doc): (input, cache read, cache write).
    fn cached(&mut self, body: &Value) -> (u64, u64, u64) {
        let mut units: Vec<(String, bool)> = vec![(body["tools"].to_string(), false)];
        let strip = |b: &Value| {
            let mut b = b.clone();
            if let Some(o) = b.as_object_mut() {
                o.remove("cache_control");
            }
            b.to_string()
        };
        for b in body["system"].as_array().into_iter().flatten() {
            units.push((strip(b), b.get("cache_control").is_some()));
        }
        for m in body["messages"].as_array().into_iter().flatten() {
            units.push((format!("<{}>", m["role"]), false));
            for b in m["content"].as_array().into_iter().flatten() {
                units.push((strip(b), b.get("cache_control").is_some()));
            }
        }
        let mut hashes = Vec::with_capacity(units.len());
        let mut tokens = Vec::with_capacity(units.len());
        let (mut joined, mut total) = (String::new(), 0u64);
        for (u, _) in &units {
            joined.push_str(u);
            total += tokens_of(u);
            hashes.push(sha_hex(&joined));
            tokens.push(total);
        }
        let marks: Vec<usize> = units.iter().enumerate().filter(|(_, (_, m))| *m).map(|(i, _)| i).collect();
        let mut read = 0u64;
        for &m in &marks {
            for k in (m.saturating_sub(LOOKBACK)..=m).rev() {
                if self.cache.contains(&hashes[k]) {
                    read = read.max(tokens[k]);
                    break;
                }
            }
        }
        let last = marks.last().map_or(0, |&m| tokens[m]);
        let write = last.saturating_sub(read);
        for &m in &marks {
            self.cache.insert(hashes[m].clone());
        }
        (total.saturating_sub(read + write), read, write)
    }

    fn anthropic(&mut self, body: Value) -> Response {
        if body["stream"] != true {
            return anthropic_error(400, "invalid_request_error", "this fake streams: stream is true");
        }
        let model = body["model"].as_str().unwrap_or("").to_string();
        if !ANTHROPIC_MODELS.iter().any(|(id, _)| *id == model) {
            return anthropic_error(404, "not_found_error", &format!("model: {model}"));
        }
        if body["max_tokens"].as_u64().is_none_or(|n| n == 0) {
            return anthropic_error(400, "invalid_request_error", "max_tokens: Field required");
        }
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        if messages.first().is_none_or(|m| m["role"] != "user") {
            return anthropic_error(400, "invalid_request_error", "messages: the first message must be the user's");
        }
        if let Some(k) = extra_field(&body) {
            return anthropic_error(400, "invalid_request_error", &format!("messages.content.{k}: Extra inputs are not permitted"));
        }
        if body.to_string().matches("\"cache_control\"").count() > 4 {
            return anthropic_error(400, "invalid_request_error", "A maximum of 4 blocks with cache_control may be provided");
        }
        for (i, m) in messages.iter().enumerate() {
            let blocks = m["content"].as_array().cloned().unwrap_or_default();
            // a model thinks unless the call turns thinking off; only then
            // must a tool use carry its thinking back
            let thinks = THINKS.contains(&model.as_str()) && body["thinking"]["type"] != "disabled";
            if m["role"] == "assistant" && thinks && blocks.iter().any(|b| b["type"] == "tool_use") {
                let signed = blocks.first().is_some_and(|b| b["type"] == "thinking" && b["signature"].as_str().is_some_and(|s| self.signatures.contains(s)));
                if !signed {
                    return anthropic_error(400, "invalid_request_error", &format!("messages.{i}.content.0: a turn's tool use carries back its thinking block, unchanged"));
                }
            }
        }
        let offered: HashSet<String> = body["tools"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
        let none = body["tool_choice"]["type"] == "none";
        let last = messages.last().cloned().unwrap_or(Value::Null);
        let blocks = last["content"].as_array().cloned().unwrap_or_default();
        let result = blocks.iter().rev().find(|b| b["type"] == "tool_result").map(|b| match &b["content"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        });
        let text = blocks.iter().rev().find_map(|b| b["text"].as_str().map(str::to_string));
        let reply = match self.said.pop_front() {
            Some(t) => Reply::Text(t),
            None => reply_to(text.as_deref(), result.as_deref(), &|n| !none && offered.contains(n)),
        };
        let (input, read, write) = self.cached(&body);
        let id = self.fresh("msg_fake_");
        let usage = json!({ "input_tokens": input, "cache_read_input_tokens": read, "cache_creation_input_tokens": write, "output_tokens": 1 });
        let mut events = vec![json!({ "type": "message_start", "message": { "id": id, "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": null, "usage": usage } })];
        let mut index = 0;
        if THINKS.contains(&model.as_str()) && body["thinking"]["type"] != "disabled" {
            let sig = self.fresh("fake-sig-");
            self.signatures.insert(sig.clone());
            events.push(json!({ "type": "content_block_start", "index": 0, "content_block": { "type": "thinking", "thinking": "", "signature": "" } }));
            events.push(json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "signature_delta", "signature": sig } }));
            events.push(json!({ "type": "content_block_stop", "index": 0 }));
            index = 1;
        }
        let (stop, written) = match &reply {
            Reply::Text(t) => {
                events.push(json!({ "type": "content_block_start", "index": index, "content_block": { "type": "text", "text": "" } }));
                for p in pieces(t) {
                    events.push(json!({ "type": "content_block_delta", "index": index, "delta": { "type": "text_delta", "text": p } }));
                }
                events.push(json!({ "type": "content_block_stop", "index": index }));
                ("end_turn", t.len())
            }
            Reply::Tools(calls) => {
                let mut n = 0;
                for (k, (name, args)) in calls.iter().enumerate() {
                    let i = index + k;
                    let call = self.fresh("toolu_fake_");
                    events.push(json!({ "type": "content_block_start", "index": i, "content_block": { "type": "tool_use", "id": call, "name": name, "input": {} } }));
                    let a = args.to_string();
                    let (head, tail) = a.split_at(a.len() / 2);
                    for p in [head, tail] {
                        events.push(json!({ "type": "content_block_delta", "index": i, "delta": { "type": "input_json_delta", "partial_json": p } }));
                    }
                    events.push(json!({ "type": "content_block_stop", "index": i }));
                    n += a.len();
                }
                ("tool_use", n)
            }
        };
        let output = tokens_of(&"x".repeat(written.max(1)));
        events.push(json!({ "type": "message_delta", "delta": { "stop_reason": stop, "stop_sequence": null }, "usage": { "output_tokens": output } }));
        events.push(json!({ "type": "message_stop" }));
        if let Some(c) = self.calls.last_mut() {
            c.usage = Some(json!({ "input": input, "cached": read, "cache_write": write, "output": output }));
        }
        Response::bytes(200, "text/event-stream", sse(&events))
    }

    fn responses(&mut self, chatgpt: bool, body: Value) -> Response {
        if chatgpt {
            if body["store"] != false || body["stream"] != true {
                return openai_error(400, "subscription_sharing_unsupported_capability", "set store: false and stream: true");
            }
            if let Some(k) = PREVIEW_REFUSES.iter().find(|k| body.get(**k).is_some()) {
                return openai_error(400, "subscription_sharing_unsupported_capability", &format!("{k} is not supported"));
            }
            if body["tools"].as_array().into_iter().flatten().any(|t| t["type"] == "function") {
                return openai_error(400, "subscription_sharing_unsupported_capability", "tools: group function tools in a namespace");
            }
            if body["input"].as_array().into_iter().flatten().any(|i| i["role"] == "system") {
                return openai_error(400, "subscription_sharing_unsupported_capability", "input: a system message item; use instructions");
            }
        } else if body["stream"] != true {
            return openai_error(400, "invalid_request_error", "this fake streams: stream is true");
        }
        let model = body["model"].as_str().unwrap_or("").to_string();
        let known = if chatgpt { CHATGPT_MODELS.iter().any(|(id, _)| *id == model) } else { OPENAI_MODELS.contains(&model.as_str()) };
        if !known {
            return openai_error(404, "model_not_found", &format!("the model {model} does not exist"));
        }
        let input = body["input"].as_array().cloned().unwrap_or_default();
        let mut offered: HashSet<String> = HashSet::new();
        let mut namespace = None;
        for t in body["tools"].as_array().into_iter().flatten() {
            if t["type"] == "namespace" {
                namespace = t["name"].as_str().map(str::to_string);
                offered.extend(t["tools"].as_array().into_iter().flatten().filter_map(|f| f["name"].as_str().map(str::to_string)));
            } else if let Some(n) = t["name"].as_str() {
                offered.insert(n.to_string());
            }
        }
        let none = body["tool_choice"] == "none";
        let last = input.last().cloned().unwrap_or(Value::Null);
        let result = (last["type"] == "function_call_output").then(|| last["output"].as_str().unwrap_or("").to_string());
        let text = last["content"].as_array().and_then(|parts| parts.iter().rev().find_map(|p| p["text"].as_str().map(str::to_string))).or_else(|| last["content"].as_str().map(str::to_string));
        let reply = match self.said.pop_front() {
            Some(t) => Reply::Text(t),
            None => reply_to(text.as_deref(), result.as_deref(), &|n| !none && offered.contains(n)),
        };
        let id = self.fresh("resp_fake_");
        let mut events = vec![json!({ "type": "response.created", "response": { "id": id, "status": "in_progress" } })];
        let written = match &reply {
            Reply::Text(t) => {
                events.push(json!({ "type": "response.output_item.added", "output_index": 0, "item": { "type": "message", "id": "msg_0", "role": "assistant", "content": [] } }));
                for p in pieces(t) {
                    events.push(json!({ "type": "response.output_text.delta", "output_index": 0, "content_index": 0, "item_id": "msg_0", "delta": p }));
                }
                events.push(json!({ "type": "response.output_item.done", "output_index": 0, "item": { "type": "message", "id": "msg_0", "role": "assistant" } }));
                t.len()
            }
            Reply::Tools(calls) => {
                let mut n = 0;
                for (k, (name, args)) in calls.iter().enumerate() {
                    let call = self.fresh("call_fake_");
                    let mut item = json!({ "type": "function_call", "id": format!("fc_{k}"), "call_id": call, "name": name, "arguments": "" });
                    if let Some(ns) = &namespace {
                        item["namespace"] = json!(ns);
                    }
                    events.push(json!({ "type": "response.output_item.added", "output_index": k, "item": item }));
                    let a = args.to_string();
                    events.push(json!({ "type": "response.function_call_arguments.delta", "output_index": k, "item_id": format!("fc_{k}"), "delta": a }));
                    events.push(json!({ "type": "response.function_call_arguments.done", "output_index": k, "item_id": format!("fc_{k}"), "arguments": a }));
                    n += a.len();
                }
                n
            }
        };
        let input_tokens = tokens_of(&body["input"].to_string()) + tokens_of(body["instructions"].as_str().unwrap_or(""));
        let output = tokens_of(&"x".repeat(written.max(1)));
        events.push(json!({ "type": "response.completed", "response": { "id": id, "status": "completed",
            "usage": { "input_tokens": input_tokens, "input_tokens_details": { "cached_tokens": 0 }, "output_tokens": output, "output_tokens_details": { "reasoning_tokens": 0 } } } }));
        if let Some(c) = self.calls.last_mut() {
            c.usage = Some(json!({ "input": input_tokens, "cached": 0, "cache_write": 0, "output": output }));
        }
        Response::bytes(200, "text/event-stream", sse(&events))
    }

    fn issue(&mut self, client_id: &str) -> (String, String, u64) {
        let access = self.fresh("chatgpt-at-");
        let refresh = self.fresh("chatgpt-rt-");
        let expires_in = self.expires_in.unwrap_or(3600);
        self.access.insert(access.clone(), now_s() + expires_in);
        self.refresh.insert(refresh.clone(), (client_id.to_string(), false));
        (access, refresh, expires_in)
    }

    fn authorize(&mut self, req: &Request) -> Response {
        let q = &req.query;
        let get = |k: &str| q.get(k).map(String::as_str).unwrap_or("");
        self.authorized.push(q.clone());
        let redirect = get("redirect_uri");
        let scopes: HashSet<&str> = get("scope").split(' ').collect();
        let problems = [
            (get("response_type") != "code", "response_type is code"),
            (!redirect.starts_with("http://127.0.0.1:") || !redirect.ends_with("/auth/callback"), "redirect_uri is a 127.0.0.1 loopback's /auth/callback"),
            (!SCOPES.iter().all(|s| scopes.contains(s)), "scope asks for identity, offline_access and plan usage"),
            (get("resource") != RESOURCE, "resource is https://api.openai.com/v1"),
            (get("code_challenge_method") != "S256" || get("code_challenge").len() != 43, "PKCE is S256"),
            (get("state").is_empty() || get("nonce").is_empty(), "state and nonce are named"),
            (!get("ext_agent_host_id").starts_with("urn:uuid:"), "ext_agent_host_id is the host's id"),
        ];
        if let Some((_, why)) = problems.iter().find(|(bad, _)| *bad) {
            return Response::bytes(400, "text/plain", format!("bad authorize request: {why}").into_bytes());
        }
        let client = match get("client_id") {
            "dynamic_agent_client" => {
                if get("agent_name_hint").is_empty() {
                    return Response::bytes(400, "text/plain", b"a registration names its agent (agent_name_hint)".to_vec());
                }
                let c = self.fresh("oaiapp_fake_");
                self.clients.insert(c.clone());
                c
            }
            c if self.clients.contains(c) => c.to_string(),
            c => return Response::bytes(400, "text/plain", format!("no client {c}").into_bytes()),
        };
        let code = self.fresh("code_fake_");
        self.codes.insert(code.clone(), Pending { client_id: client.clone(), challenge: get("code_challenge").to_string(), redirect_uri: redirect.to_string(), nonce: get("nonce").to_string() });
        let enc = |s: &str| s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect::<String>();
        let location = format!("{redirect}?code={code}&scope={}&state={}&client_id={client}", enc(&SCOPES.join(" ")), enc(get("state")));
        Response::bytes(302, "text/plain", Vec::new()).with_header("location", &location)
    }

    fn token(&mut self, form: &HashMap<String, String>, issuer: &str) -> Response {
        let get = |k: &str| form.get(k).map(String::as_str).unwrap_or("");
        if get("resource") != RESOURCE {
            return Response::json(400, &json!({ "error": "invalid_target" }));
        }
        match get("grant_type") {
            "authorization_code" => {
                let Some(p) = self.codes.remove(get("code")) else { return Response::json(400, &json!({ "error": "invalid_grant" })) };
                let verified = b64url(&Sha256::digest(get("code_verifier").as_bytes())) == p.challenge;
                if !verified || p.client_id != get("client_id") || p.redirect_uri != get("redirect_uri") {
                    return Response::json(400, &json!({ "error": "invalid_grant" }));
                }
                let (access, refresh, expires_in) = self.issue(&p.client_id);
                let header = b64url(br#"{"alg":"RS256","typ":"JWT"}"#);
                let claims = json!({ "iss": issuer, "aud": p.client_id, "sub": "user-fake", "email": CHATGPT_EMAIL, "nonce": p.nonce, "iat": now_s(), "exp": now_s() + 3600 });
                let id_token = format!("{header}.{}.fake-signature", b64url(claims.to_string().as_bytes()));
                Response::json(200, &json!({ "access_token": access, "refresh_token": refresh, "id_token": id_token, "token_type": "Bearer", "expires_in": expires_in, "scope": SCOPES.join(" ") }))
            }
            "refresh_token" => {
                let token = get("refresh_token").to_string();
                match self.refresh.get_mut(&token) {
                    Some((client, used)) if !*used && client == get("client_id") => *used = true,
                    Some((_, true)) => return Response::json(400, &json!({ "error": "refresh_token_reused" })),
                    _ => return Response::json(400, &json!({ "error": "invalid_grant" })),
                }
                self.refreshes += 1;
                let client = get("client_id").to_string();
                let (access, refresh, expires_in) = self.issue(&client);
                Response::json(200, &json!({ "access_token": access, "refresh_token": refresh, "token_type": "Bearer", "expires_in": expires_in }))
            }
            _ => Response::json(400, &json!({ "error": "unsupported_grant_type" })),
        }
    }
}

/// A form body's pairs, decoded.
fn form_of(body: &[u8]) -> HashMap<String, String> {
    let text = String::from_utf8_lossy(body).replace('+', " ");
    text.split('&').filter_map(|p| p.split_once('=')).map(|(k, v)| (crate::http::decode(k), crate::http::decode(v))).collect()
}

fn answer(s: &mut State, req: &Request, base: &str) -> Response {
    let upstream = req.header("x-fragment-upstream-host").map(str::to_string);
    let bearer = req.header("authorization").and_then(|a| a.strip_prefix("Bearer ")).map(str::to_string);
    let key = req.header("x-api-key").map(str::to_string);
    let host = upstream.clone().unwrap_or_else(|| match req.path.as_str() {
        "/v1/messages" => "api.anthropic.com".into(),
        p if p.starts_with("/v1/") => "api.openai.com".into(),
        _ => "auth.openai.com".into(),
    });
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    s.calls.push(VendorCall { host: host.clone(), method: req.method.clone(), path: req.path.clone(), body: body.clone(), credential: key.clone().or(bearer.clone()), usage: None });
    if host != "auth.openai.com" {
        if let Some(status) = s.failures.pop_front() {
            return match host.as_str() {
                "api.anthropic.com" => anthropic_error(status, "overloaded_error", "a failure the test asked for"),
                _ => openai_error(status, "server_error", "a failure the test asked for"),
            };
        }
    }
    // the sign-in's own URLs: OpenAI's when the platform calls through its upstream, this fake's when the CLI calls it
    let issuer = if upstream.is_some() { "https://auth.openai.com".to_string() } else { base.to_string() };
    match (req.method.as_str(), req.path.as_str(), host.as_str()) {
        ("POST", "/v1/messages", "api.anthropic.com") => {
            match &key {
                Some(k) if k.starts_with("sk-ant-") && k != BAD_KEY => {}
                _ => return anthropic_error(401, "authentication_error", "invalid x-api-key"),
            }
            if req.header("anthropic-version") != Some("2023-06-01") {
                return anthropic_error(400, "invalid_request_error", "anthropic-version: the header is required");
            }
            s.anthropic(body)
        }
        ("GET", "/v1/models", "api.anthropic.com") => match &key {
            Some(k) if k.starts_with("sk-ant-") && k != BAD_KEY => {
                let data: Vec<Value> = ANTHROPIC_MODELS.iter().map(|(id, name)| json!({ "type": "model", "id": id, "display_name": name, "created_at": "2026-09-01T00:00:00Z" })).collect();
                Response::json(200, &json!({ "data": data, "has_more": false, "first_id": data[0]["id"], "last_id": data[data.len() - 1]["id"] }))
            }
            _ => anthropic_error(401, "authentication_error", "invalid x-api-key"),
        },
        (method, "/v1/responses" | "/v1/models", "api.openai.com") => {
            let Some(token) = bearer else { return openai_error(401, "invalid_api_key", "no bearer token") };
            let chatgpt = token.starts_with("chatgpt-at-");
            if chatgpt {
                if s.access.get(&token).is_none_or(|exp| *exp <= now_s()) {
                    return Response::json(401, &json!({ "detail": "the access token is expired or unknown" }));
                }
            } else if !token.starts_with("sk-") || token == BAD_KEY {
                return openai_error(401, "invalid_api_key", "Incorrect API key provided");
            }
            match (method, req.path.as_str()) {
                ("POST", "/v1/responses") => s.responses(chatgpt, body),
                ("GET", "/v1/models") if chatgpt => {
                    let mut models: Vec<Value> = CHATGPT_MODELS.iter().map(|(slug, name)| json!({ "slug": slug, "display_name": name, "visibility": "list" })).collect();
                    models.push(json!({ "slug": "gpt-internal", "display_name": "Internal", "visibility": "hide" }));
                    Response::json(200, &json!({ "models": models }))
                }
                ("GET", "/v1/models") => {
                    let data: Vec<Value> = OPENAI_MODELS.iter().enumerate().map(|(i, id)| json!({ "id": id, "object": "model", "created": 1_700_000_000 + i as i64, "owned_by": "openai" })).collect();
                    Response::json(200, &json!({ "object": "list", "data": data }))
                }
                _ => openai_error(405, "method_not_allowed", "no such route"),
            }
        }
        ("GET", "/.well-known/openid-configuration", _) => Response::json(
            200,
            &json!({
                "issuer": issuer, "authorization_endpoint": format!("{issuer}/api/accounts/authorize"), "token_endpoint": format!("{issuer}/api/accounts/oauth/token"),
                "revocation_endpoint": format!("{issuer}/oauth/revoke"), "jwks_uri": format!("{issuer}/.well-known/jwks.json"),
            }),
        ),
        ("GET", "/api/accounts/authorize", _) => s.authorize(req),
        ("POST", "/api/accounts/oauth/token", _) => s.token(&form_of(&req.body), &issuer),
        ("POST", "/oauth/revoke", _) => {
            let form = form_of(&req.body);
            if let Some(t) = form.get("token") {
                s.revoked.push(t.clone());
                if let Some((_, used)) = s.refresh.get_mut(t) {
                    *used = true;
                }
            }
            Response::bytes(200, "text/plain", Vec::new())
        }
        _ => Response::json(404, &json!({ "error": format!("no route {} {} on {host}", req.method, req.path) })),
    }
}

pub struct Vendors {
    pub url: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

impl Vendors {
    /// Serves on `port` (0: a free one).
    pub fn start(port: u16) -> std::io::Result<Vendors> {
        let state: Arc<Mutex<State>> = Arc::default();
        let st = Arc::clone(&state);
        let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
        let base = format!("http://127.0.0.1:{}", listener.local_addr()?.port());
        let at = base.clone();
        let handler: Handler = Arc::new(move |req: &Request| answer(&mut st.lock().expect("vendors state"), req, &at));
        let server = Server::serve(listener, handler)?;
        Ok(Vendors { url: base, state, _server: server })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().expect("vendors state")
    }

    /// Every call, oldest first.
    pub fn calls(&self) -> Vec<VendorCall> {
        self.state().calls.clone()
    }

    /// The texts the next model calls answer, in order.
    pub fn say_next(&self, texts: &[&str]) {
        self.state().said.extend(texts.iter().map(|t| t.to_string()));
    }

    /// The next model calls answer these statuses, in order.
    pub fn fail_next(&self, statuses: &[u16]) {
        self.state().failures.extend(statuses);
    }

    /// The access tokens it issues from now on live this long (seconds).
    pub fn set_expires_in(&self, seconds: u64) {
        self.state().expires_in = Some(seconds);
    }

    /// The refreshes it granted.
    pub fn refreshes(&self) -> u64 {
        self.state().refreshes
    }

    /// The tokens revoked.
    pub fn revoked(&self) -> Vec<String> {
        self.state().revoked.clone()
    }

    /// Every authorize request's parameters.
    pub fn authorized(&self) -> Vec<HashMap<String, String>> {
        self.state().authorized.clone()
    }
}

/// The catalog's rows for the own providers the model route reads (an
/// `own` key each, as `deploy/example.jsonc` names them): dev's.
pub fn catalog_rows() -> Value {
    json!([
        { "name": "anthropic", "kind": "own", "hosts": ["api.anthropic.com"], "placements": [{ "header": "x-api-key" }], "env": ["ANTHROPIC_API_KEY"] },
        { "name": "openai", "kind": "own", "hosts": ["api.openai.com"], "placements": [{ "header": "authorization", "format": "Bearer {}" }], "env": ["OPENAI_API_KEY"] },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(state: &mut State, body: Value) -> (u16, String) {
        let req = Request::for_test("POST", "/v1/messages", &[("x-api-key", "sk-ant-x"), ("anthropic-version", "2023-06-01")], body.to_string().into_bytes());
        let r = answer(state, &req, "http://fake");
        (r.status, String::from_utf8(r.body).unwrap())
    }

    use fragment_core::models::{bound_hinted, Stream};
    use fragment_core::providers::{self as own, Vendor};

    /// A mind's turn as templates/mind sends it: the view hinted, its state
    /// and its words, the tools offered.
    fn turn(lines: usize, said: &str) -> Value {
        let view = format!("<chat>\n{}</chat>", (0..lines).map(|i| format!("{i}+1|{}\n", "remembered words ".repeat(8))).collect::<String>());
        json!({
            "messages": [
                { "role": "system", "content": "You are Mind. ".repeat(40) },
                { "role": "user", "content": [{ "type": "text", "text": view, "cache": "blocks" }, { "type": "text", "text": "Now: 2026-10-08" }, { "type": "text", "text": said }] },
            ],
            "tools": [{ "type": "function", "function": { "name": "zoom", "description": "Open a line", "parameters": { "type": "object", "properties": { "id": { "type": "integer" } } } } }],
            "tool_choice": "auto",
            // high: a thinking model thinks (low turns thinking off)
            "reasoning_effort": "high",
        })
    }

    /// One call of `body` on `vendor`'s `model` through core's translation
    /// and this fake: the answer folded, the thinking blocks, what it used.
    fn call(s: &mut State, vendor: Vendor, model: &str, credential: &str, body: Value) -> (fragment_core::models::Answer, Vec<Value>, Option<own::Counted>) {
        let input = bound_hinted(own::OWN, body, true).unwrap().input;
        let request = own::request(vendor, &input, model).unwrap();
        let (path, headers): (&str, Vec<(&str, &str)>) = match vendor {
            Vendor::Anthropic => ("/v1/messages", vec![("x-api-key", credential), ("anthropic-version", "2023-06-01")]),
            _ => ("/v1/responses", vec![("authorization", credential)]),
        };
        let r = answer(s, &Request::for_test("POST", path, &headers, request.to_string().into_bytes()), "http://fake");
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        let mut t = own::translator(vendor, model);
        let mut out = vec![];
        t.push(&r.body, &mut out);
        t.finish(&mut out);
        let mut folded = Stream::answering();
        folded.push(&out, None);
        (folded.answer().cloned().unwrap(), t.thinking_blocks().to_vec(), t.counted())
    }

    /// Goal: core's translation is a request this fake takes, end to end: a
    /// mind's turn on Claude calls a tool, its thinking carried back with the
    /// result is accepted, and the next call reads the view from the cache;
    /// the same turn on ChatGPT, under the preview's rules. Method: two calls
    /// each, translated by fragment_core::providers, answered by the fake.
    #[test]
    fn a_turn_through_the_translations() {
        let mut s = State::default();
        let first = turn(30, "what grows? [[call zoom {\"id\":3}]]");
        let (a, thinking, used) = call(&mut s, Vendor::Anthropic, "claude-sonnet-5-5", "sk-ant-x", first.clone());
        assert_eq!((a.finish_reason.as_deref(), a.tool_calls.len()), (Some("tool_calls"), 1));
        assert_eq!(thinking.len(), 1, "a thinking model's block, kept");
        assert!(used.unwrap().cache_write > 0 && used.unwrap().cached == 0, "{used:?}");
        let mut next = first.clone();
        let msgs = next["messages"].as_array_mut().unwrap();
        msgs.push(own::message_of(&a, &thinking));
        msgs.push(json!({ "role": "tool", "tool_call_id": a.tool_calls[0].id, "content": "3+0|user: tomatoes" }));
        let (b, _, used) = call(&mut s, Vendor::Anthropic, "claude-sonnet-5-5", "sk-ant-x", next);
        assert_eq!((b.content.as_str(), b.finish_reason.as_deref()), ("the tool said: 3+0|user: tomatoes", Some("stop")));
        let used = used.unwrap();
        assert!(used.cached > 0, "the second call reads the first's prefix: {used:?}");
        // a turn later, its view two lines longer: its view's first blocks are read back
        let (_, _, used) = call(&mut s, Vendor::Anthropic, "claude-sonnet-5-5", "sk-ant-x", turn(32, "and now?"));
        assert!(used.unwrap().cached > 0, "{used:?}");
        // ChatGPT: a token it issued, the preview's rules
        let (access, _, _) = s.issue("oaiapp_fake_1");
        let bearer = format!("Bearer {access}");
        let (c, thinking, used) = call(&mut s, Vendor::Chatgpt, "gpt-6.1-sol", &bearer, turn(3, "look [[call zoom {\"id\":1}]]"));
        assert_eq!((c.finish_reason.as_deref(), c.tool_calls[0].function.name.as_str(), thinking.len()), (Some("tool_calls"), "zoom", 0));
        assert!(used.is_some());
        let mut next = turn(3, "look");
        let msgs = next["messages"].as_array_mut().unwrap();
        msgs.push(own::message_of(&c, &[]));
        msgs.push(json!({ "role": "tool", "tool_call_id": c.tool_calls[0].id, "content": "1+0|user: hi" }));
        let (d, _, _) = call(&mut s, Vendor::Chatgpt, "gpt-6.1-sol", &bearer, next);
        assert_eq!(d.content, "the tool said: 1+0|user: hi");
        let sent = &s.calls.last().unwrap().body;
        assert_eq!(sent["input"].as_array().unwrap().iter().find(|i| i["type"] == "function_call").unwrap()["namespace"], "fragment");
        // an API key: the same call, top-level tools and an output cap
        let (e, _, _) = call(&mut s, Vendor::Openai, "gpt-5", "Bearer sk-proj-x", turn(3, "hello"));
        assert_eq!(e.content, "echo: hello");
        assert!(s.calls.last().unwrap().body["max_output_tokens"].is_u64());
    }

    /// Goal: the cache model reads what an earlier call marked, looking
    /// back up to 20 blocks from a mark, and a thinking model refuses a tool
    /// call without its signed thinking. Method: two calls whose second
    /// grows the first's marked prefix, and a forged turn.
    #[test]
    fn anthropic_caches_and_checks_thinking() {
        let mut s = State::default();
        let block = |t: &str, mark: bool| if mark { json!({ "type": "text", "text": t, "cache_control": { "type": "ephemeral" } }) } else { json!({ "type": "text", "text": t }) };
        let first = json!({ "model": "claude-haiku-5-5", "max_tokens": 9, "stream": true, "system": [block("s".repeat(400).as_str(), true)],
            "messages": [{ "role": "user", "content": [block("a".repeat(400).as_str(), true), block("q", true)] }] });
        let (status, _) = messages(&mut s, first);
        assert_eq!(status, 200);
        let used = s.calls.last().unwrap().usage.clone().unwrap();
        assert_eq!(used["cached"], 0);
        assert!(used["cache_write"].as_u64().unwrap() > 0);
        let second = json!({ "model": "claude-haiku-5-5", "max_tokens": 9, "stream": true, "system": [block("s".repeat(400).as_str(), true)],
            "messages": [{ "role": "user", "content": [block("a".repeat(400).as_str(), false), block("b".repeat(400).as_str(), true), block("q2", true)] }] });
        messages(&mut s, second);
        let used = s.calls.last().unwrap().usage.clone().unwrap();
        assert!(used["cached"].as_u64().unwrap() >= 200, "the first call's view is read back: {used}");
        let forged = json!({ "model": "claude-sonnet-5-5", "max_tokens": 9, "stream": true, "messages": [
            { "role": "user", "content": [block("q", false)] },
            { "role": "assistant", "content": [{ "type": "tool_use", "id": "t", "name": "f", "input": {} }] },
            { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t", "content": "r" }] }] });
        let (status, text) = messages(&mut s, forged);
        assert_eq!(status, 400, "{text}");
        let hinted = json!({ "model": "claude-haiku-5-5", "max_tokens": 9, "stream": true, "messages": [{ "role": "user", "content": [{ "type": "text", "text": "x", "cache": "blocks" }] }] });
        assert_eq!(messages(&mut s, hinted).0, 400, "a hint is no field of Anthropic's");
    }
}
