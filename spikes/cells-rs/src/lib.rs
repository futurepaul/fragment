//! Spike 1 (Rust platform cells) and spike 2 (the app facet as author SQL).
//!
//! A Rust supervisor Durable Object on celld: its own SQL (an operation
//! ledger and channels), hibernatable WebSockets, an alarm, and an app facet
//! started from author JavaScript through the Worker Loader. workers-rs
//! 0.8.5 has no loader or facet API, so those are reached through
//! `js_sys::Reflect` on the raw `env` and `ctx`.
//!
//! Routes (after `/c/<cell>`):
//!   PUT  /code    {sha, source, operations: {name: {kind}}}
//!   POST /op      {id, name, input, fail_after_app?}; mutations run in
//!                 the facet's transactionSync with a facet-local ledger
//!   POST /ping    a supervisor-only write (no facet)
//!   GET  /noop    no storage at all
//!   GET  /ws      a hibernatable WebSocket; each text frame is appended to
//!                 the `ws` channel and answered with its sequence number
//!   POST /alarm   {in_ms}; GET /alarms lists the firings
//!   GET  /channel?name=…   GET /stats

use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use worker::js_sys::{self, Array, Function, Object, Promise, Reflect};
use worker::wasm_bindgen::{closure::Closure, prelude::*, JsCast, JsValue};
use worker::wasm_bindgen_futures::{future_to_promise, JsFuture};
use worker::*;

/// Operation input is a request, not an upload (MODEL.md limits).
const INPUT_MAX_BYTES: usize = 256 * 1024;
/// Operation results must fit a Workflows step result.
const RESULT_MAX_BYTES: usize = 1024 * 1024;
const OP_ID_MAX_BYTES: usize = 128;
const OP_NAME_MAX_BYTES: usize = 64;
const CHANNEL_BODY_MAX_BYTES: usize = 64 * 1024;
const CHANNEL_PAGE_MAX: i64 = 1000;
const APP_CPU_MS: u32 = 30_000;
const APP_SUBREQUESTS: u32 = 50;
const APP_FACET: &str = "app";
/// Platform code that runs in the facet around the author's App class.
const PLATFORM_JS: &str = include_str!("../../apps/platform.mjs");

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS code (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  sha TEXT NOT NULL,
  source TEXT NOT NULL,
  operations TEXT NOT NULL,
  cpu_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ops (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  input_sha TEXT NOT NULL,
  result TEXT NOT NULL,
  at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS channel (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  channel TEXT NOT NULL,
  body TEXT NOT NULL,
  at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS alarms (
  n INTEGER PRIMARY KEY AUTOINCREMENT,
  fired_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS pings (
  n INTEGER PRIMARY KEY AUTOINCREMENT,
  at INTEGER NOT NULL
);
";

/// Every refusal the supervisor can answer, with its status and code.
#[derive(Debug)]
enum OpError {
    InvalidRequest(String),
    UnknownOp(String),
    NoCode,
    ConflictingBody,
    TooLarge(&'static str, usize, usize),
    App(String),
    Host(String),
}

impl OpError {
    fn status(&self) -> u16 {
        match self {
            OpError::InvalidRequest(_) => 400,
            OpError::UnknownOp(_) | OpError::NoCode => 404,
            OpError::ConflictingBody => 409,
            OpError::TooLarge(..) => 413,
            OpError::App(_) => 422,
            OpError::Host(_) => 500,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            OpError::InvalidRequest(_) => "invalid_request",
            OpError::UnknownOp(_) => "unknown_operation",
            OpError::NoCode => "no_code",
            OpError::ConflictingBody => "conflicting_body",
            OpError::TooLarge(..) => "too_large",
            OpError::App(_) => "app_failed",
            OpError::Host(_) => "host_failed",
        }
    }

    fn message(&self) -> String {
        match self {
            OpError::InvalidRequest(m) | OpError::App(m) | OpError::Host(m) => m.clone(),
            OpError::UnknownOp(n) => format!("no operation named {n:?}"),
            OpError::NoCode => "no app code is installed".into(),
            OpError::ConflictingBody => {
                "this operation id was already used with a different input".into()
            }
            OpError::TooLarge(what, got, max) => format!("{what} is {got} bytes; the limit is {max}"),
        }
    }

    fn response(&self) -> Result<Response> {
        Ok(Response::from_json(&json!({ "error": self.code(), "message": self.message() }))?
            .with_status(self.status()))
    }
}

impl From<worker::Error> for OpError {
    fn from(e: worker::Error) -> Self {
        OpError::Host(e.to_string())
    }
}

fn js_err(context: &str, e: JsValue) -> OpError {
    let text = e
        .dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"));
    OpError::Host(format!("{context}: {text}"))
}

fn get(obj: &JsValue, key: &str) -> std::result::Result<JsValue, OpError> {
    Reflect::get(obj, &JsValue::from_str(key)).map_err(|e| js_err(key, e))
}

fn set(obj: &Object, key: &str, value: impl Into<JsValue>) {
    let ok = Reflect::set(obj, &JsValue::from_str(key), &value.into()).expect("set on a plain object");
    assert!(ok, "Reflect.set refused {key} on a plain object");
}

fn call(obj: &JsValue, method: &str, args: &[JsValue]) -> std::result::Result<JsValue, OpError> {
    let f: Function = get(obj, method)?
        .dyn_into()
        .map_err(|_| OpError::Host(format!("{method} is not a function")))?;
    let list = Array::new();
    for a in args {
        list.push(a);
    }
    Reflect::apply(&f, obj, &list).map_err(|e| js_err(method, e))
}

/// Awaits a promise or an RPC thenable (neither is `instanceof Promise`).
async fn settle(v: JsValue) -> std::result::Result<JsValue, JsValue> {
    JsFuture::from(Promise::resolve(&v)).await
}

fn to_js(v: &Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).expect("serde_json output is valid JSON")
}

fn from_js(v: &JsValue) -> std::result::Result<Value, OpError> {
    if v.is_undefined() {
        return Ok(Value::Null);
    }
    let text = js_sys::JSON::stringify(v).map_err(|e| js_err("result is not JSON", e))?;
    let text = String::from(text);
    serde_json::from_str(&text).map_err(|e| OpError::App(format!("result is not JSON: {e}")))
}

fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

#[derive(Deserialize)]
struct CodeBody {
    sha: String,
    source: String,
    operations: Map<String, Value>,
    /// Lower than APP_CPU_MS for the limit test; never higher.
    #[serde(default)]
    cpu_ms: Option<u32>,
}

#[derive(Deserialize)]
struct OpBody {
    id: String,
    name: String,
    #[serde(default)]
    input: Value,
    /// Spike-only: fail after the facet committed, before the audit row.
    #[serde(default)]
    fail_after_app: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum OpKind {
    Query,
    Mutation,
}

#[durable_object]
pub struct Supervisor {
    state: State,
    raw: JsValue,
    env: Env,
}

impl DurableObject for Supervisor {
    fn new(state: State, env: Env) -> Self {
        let raw: JsValue = state._inner().into();
        let state = State::from(raw.clone().unchecked_into::<worker_sys::DurableObjectState>());
        state.storage().sql().exec(SCHEMA, None).expect("supervisor schema applies");
        Self { state, raw, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(resp) => Ok(resp),
            Err(e) => e.response(),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        self.state
            .storage()
            .sql()
            .exec("INSERT INTO alarms (fired_at) VALUES (?)", vec![SqlStorageValue::Integer(now_ms())])?;
        Response::ok("fired")
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        let text = match message {
            WebSocketIncomingMessage::String(s) => s,
            WebSocketIncomingMessage::Binary(_) => {
                ws.send_with_str(json!({ "error": "text frames only" }).to_string())?;
                return Ok(());
            }
        };
        let seq = self.append("ws", &text).map_err(|e| worker::Error::RustError(e.message()))?;
        ws.send_with_str(json!({ "seq": seq }).to_string())
    }

    async fn websocket_close(&self, _ws: WebSocket, _code: usize, _reason: String, _clean: bool) -> Result<()> {
        Ok(())
    }
}

/// RPC methods for the capability entrypoints in `entry.mjs`.
#[wasm_bindgen]
impl Supervisor {
    #[wasm_bindgen(js_name = capAppend)]
    pub fn cap_append(&self, channel: String, body: String) -> Promise {
        let result = self.append(&channel, &body);
        future_to_promise(async move {
            result
                .map(|seq| JsValue::from_f64(seq as f64))
                .map_err(|e| JsValue::from(js_sys::Error::new(&e.message())))
        })
    }
}

impl Supervisor {
    fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> std::result::Result<Vec<Value>, OpError> {
        Ok(self.sql().exec(q, binds)?.to_array::<Value>()?)
    }

    async fn route(&self, mut req: Request) -> std::result::Result<Response, OpError> {
        let url = req.url()?;
        let path = url.path();
        let (_, rest) = path
            .strip_prefix("/c/")
            .and_then(|p| p.split_once('/'))
            .ok_or_else(|| OpError::InvalidRequest("expected /c/<cell>/<route>".into()))?;
        match (req.method(), rest) {
            (Method::Get, "noop") => Ok(Response::from_json(&json!({ "ok": true }))?),
            (Method::Post, "ping") => {
                let n = self.rows(
                    "INSERT INTO pings (at) VALUES (?) RETURNING n",
                    vec![SqlStorageValue::Integer(now_ms())],
                )?;
                Ok(Response::from_json(&json!({ "n": n[0]["n"] }))?)
            }
            (Method::Put, "code") => {
                let body: CodeBody = req.json().await.map_err(|e| OpError::InvalidRequest(e.to_string()))?;
                self.install_code(body)
            }
            (Method::Post, "op") => {
                let body: OpBody = req.json().await.map_err(|e| OpError::InvalidRequest(e.to_string()))?;
                self.op(body).await
            }
            (Method::Get, "ws") => {
                let pair = WebSocketPair::new()?;
                self.state.accept_web_socket(&pair.server);
                Ok(Response::from_websocket(pair.client)?)
            }
            (Method::Post, "alarm") => {
                let body: Value = req.json().await.map_err(|e| OpError::InvalidRequest(e.to_string()))?;
                let in_ms = body["in_ms"].as_i64().ok_or_else(|| OpError::InvalidRequest("in_ms".into()))?;
                self.state.storage().set_alarm(in_ms).await?;
                Ok(Response::from_json(&json!({ "armed_in_ms": in_ms }))?)
            }
            (Method::Get, "alarms") => {
                let rows = self.rows("SELECT n, fired_at FROM alarms ORDER BY n", vec![])?;
                Ok(Response::from_json(&json!({ "alarms": rows }))?)
            }
            (Method::Get, "channel") => {
                let name = url
                    .query_pairs()
                    .find(|(k, _)| k == "name")
                    .map(|(_, v)| v.into_owned())
                    .ok_or_else(|| OpError::InvalidRequest("name".into()))?;
                let rows = self.rows(
                    "SELECT seq, body, at FROM channel WHERE channel = ? ORDER BY seq LIMIT ?",
                    vec![name.into(), SqlStorageValue::Integer(CHANNEL_PAGE_MAX)],
                )?;
                Ok(Response::from_json(&json!({ "records": rows }))?)
            }
            (Method::Get, "stats") => {
                let ops = self.rows("SELECT COUNT(*) AS n FROM ops", vec![])?;
                let channel = self.rows("SELECT COUNT(*) AS n FROM channel", vec![])?;
                Ok(Response::from_json(&json!({
                    "db_bytes": self.sql().database_size(),
                    "ops": ops[0]["n"],
                    "channel": channel[0]["n"],
                }))?)
            }
            _ => Err(OpError::InvalidRequest(format!("no route {} {rest}", req.method().as_ref()))),
        }
    }

    fn append(&self, channel: &str, body: &str) -> std::result::Result<i64, OpError> {
        if channel.is_empty() || channel.len() > OP_NAME_MAX_BYTES {
            return Err(OpError::InvalidRequest("channel name".into()));
        }
        if body.len() > CHANNEL_BODY_MAX_BYTES {
            return Err(OpError::TooLarge("channel record", body.len(), CHANNEL_BODY_MAX_BYTES));
        }
        let rows = self.rows(
            "INSERT INTO channel (channel, body, at) VALUES (?, ?, ?) RETURNING seq",
            vec![channel.into(), body.into(), SqlStorageValue::Integer(now_ms())],
        )?;
        let seq = rows[0]["seq"].as_i64().expect("RETURNING seq is an integer");
        assert!(seq > 0, "channel sequence numbers start at 1");
        Ok(seq)
    }

    fn install_code(&self, body: CodeBody) -> std::result::Result<Response, OpError> {
        if body.sha.is_empty() || body.sha.len() > 64 || !body.sha.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(OpError::InvalidRequest("sha".into()));
        }
        let cpu_ms = body.cpu_ms.unwrap_or(APP_CPU_MS);
        if cpu_ms == 0 || cpu_ms > APP_CPU_MS {
            return Err(OpError::InvalidRequest(format!("cpu_ms must be 1..={APP_CPU_MS}")));
        }
        for (name, decl) in &body.operations {
            if name.is_empty() || name.len() > OP_NAME_MAX_BYTES {
                return Err(OpError::InvalidRequest(format!("operation name {name:?}")));
            }
            match decl["kind"].as_str() {
                Some("query") | Some("mutation") => {}
                other => return Err(OpError::InvalidRequest(format!("{name}: kind {other:?}"))),
            }
        }
        self.sql().exec(
            "INSERT INTO code (id, sha, source, operations, cpu_ms) VALUES (1, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET sha = excluded.sha, source = excluded.source,
               operations = excluded.operations, cpu_ms = excluded.cpu_ms",
            vec![
                body.sha.clone().into(),
                body.source.into(),
                Value::Object(body.operations).to_string().into(),
                SqlStorageValue::Integer(cpu_ms.into()),
            ],
        )?;
        // The running facet holds the old class; the next call starts the
        // new one. `abort` keeps the facet's database.
        let facets = get(&self.raw, "facets")?;
        call(&facets, "abort", &[APP_FACET.into(), js_sys::Error::new("code replaced").into()])?;
        Ok(Response::from_json(&json!({ "sha": body.sha }))?)
    }

    /// Starts (or reaches) the app facet for the installed code.
    fn app(&self) -> std::result::Result<(JsValue, Map<String, Value>), OpError> {
        let rows = self.rows("SELECT sha, source, operations, cpu_ms FROM code WHERE id = 1", vec![])?;
        let row = rows.into_iter().next().ok_or(OpError::NoCode)?;
        let sha = row["sha"].as_str().expect("code.sha is TEXT").to_string();
        let source = row["source"].as_str().expect("code.source is TEXT").to_string();
        let operations: Map<String, Value> =
            serde_json::from_str(row["operations"].as_str().expect("code.operations is TEXT"))
                .expect("stored operations are a JSON object");
        let cpu_ms = row["cpu_ms"].as_u64().expect("code.cpu_ms is INTEGER");
        assert!(cpu_ms > 0 && cpu_ms <= APP_CPU_MS as u64, "stored cpu_ms is within the limit");

        let props = Object::new();
        set(&props, "cell", self.state.id().to_string());
        let options = Object::new();
        set(&options, "props", props);
        let exports = get(&self.raw, "exports")?;
        let channel = call(&exports, "Channel", &[options.into()])?;

        let env = Object::new();
        set(&env, "CHANNEL", channel);
        let modules = Object::new();
        set(&modules, "platform.js", PLATFORM_JS);
        set(&modules, "app.js", source);
        let limits = Object::new();
        set(&limits, "cpuMs", cpu_ms as u32);
        set(&limits, "subRequests", APP_SUBREQUESTS);
        let code = Object::new();
        set(&code, "compatibilityDate", "2026-01-01");
        set(&code, "mainModule", "platform.js");
        set(&code, "modules", modules);
        set(&code, "env", env);
        set(&code, "globalOutbound", JsValue::NULL);
        set(&code, "limits", limits);

        let loader = get(self.env.as_ref(), "LOADER")?;
        let get_code = Closure::once_into_js(move || -> JsValue { code.into() });
        let worker = call(&loader, "get", &[format!("app@{sha}").into(), get_code])?;
        let class = call(&worker, "getDurableObjectClass", &["App".into()])?;
        let start = Closure::once_into_js(move || -> JsValue {
            let o = Object::new();
            set(&o, "class", class);
            o.into()
        });
        let facets = get(&self.raw, "facets")?;
        let facet = call(&facets, "get", &[APP_FACET.into(), start])?;
        Ok((facet, operations))
    }

    async fn op(&self, body: OpBody) -> std::result::Result<Response, OpError> {
        if body.id.is_empty()
            || body.id.len() > OP_ID_MAX_BYTES
            || !body.id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b))
        {
            return Err(OpError::InvalidRequest("operation id".into()));
        }
        let input_text = body.input.to_string();
        if input_text.len() > INPUT_MAX_BYTES {
            return Err(OpError::TooLarge("operation input", input_text.len(), INPUT_MAX_BYTES));
        }
        let (facet, operations) = self.app()?;
        let kind = match operations.get(&body.name).and_then(|d| d["kind"].as_str()) {
            Some("query") => OpKind::Query,
            Some("mutation") => OpKind::Mutation,
            Some(_) => unreachable!("install_code accepts only query and mutation"),
            None => return Err(OpError::UnknownOp(body.name)),
        };

        if kind == OpKind::Query {
            let out = settle(call(&facet, "__query", &[body.name.clone().into(), to_js(&body.input)])?)
                .await
                .map_err(|e| OpError::App(js_err("query", e).message()))?;
            let result = from_js(&out)?;
            if result["error"] == "unknown_operation" {
                return Err(OpError::UnknownOp(body.name));
            }
            return Ok(Response::from_json(&json!({ "result": result, "replayed": false }))?);
        }

        // The facet's own ledger (platform.mjs) decides replay and conflict:
        // the author's write and its ledger row commit in one facet-local
        // transactionSync. serde_json orders object keys, so equal inputs
        // hash equally.
        let input_sha = hex::encode(Sha256::digest(format!("{}\n{input_text}", body.name)));
        let args = [body.id.clone().into(), body.name.clone().into(), input_sha.clone().into(), to_js(&body.input)];
        let out = settle(call(&facet, "__mutate", &args)?)
            .await
            .map_err(|e| OpError::App(js_err("mutation", e).message()))?;
        let out = from_js(&out)?;
        match out["error"].as_str() {
            Some("conflicting_body") => return Err(OpError::ConflictingBody),
            Some("unknown_operation") => return Err(OpError::UnknownOp(body.name)),
            Some(other) => return Err(OpError::Host(format!("platform answered {other:?}"))),
            None => {}
        }
        let replayed = out["replayed"].as_bool().expect("__mutate answers replayed");
        let text = out["result"].to_string();
        if text.len() > RESULT_MAX_BYTES {
            return Err(OpError::TooLarge("operation result", text.len(), RESULT_MAX_BYTES));
        }
        if body.fail_after_app {
            return Err(OpError::Host("fail_after_app: the supervisor failed after the facet committed".into()));
        }
        // The supervisor's copy is the audit trail, not the authority.
        self.sql().exec(
            "INSERT OR IGNORE INTO ops (id, name, input_sha, result, at) VALUES (?, ?, ?, ?, ?)",
            vec![body.id.into(), body.name.into(), input_sha.into(), text.into(), SqlStorageValue::Integer(now_ms())],
        )?;
        Ok(Response::from_json(&json!({ "result": out["result"], "replayed": replayed }))?)
    }
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let path = req.path();
    let cell = path
        .strip_prefix("/c/")
        .and_then(|p| p.split('/').next())
        .filter(|c| !c.is_empty() && c.len() <= 64);
    match cell {
        Some(cell) => env.durable_object("SUPERVISOR")?.get_by_name(cell)?.fetch_with_request(req).await,
        None => Response::error("expected /c/<cell>/<route>", 404),
    }
}
