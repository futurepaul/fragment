//! The JavaScript surfaces workers-rs 0.8.5 does not wrap: the Worker
//! Loader, Durable Object facets, the Workflows binding, the AI binding's
//! options, and synchronous storage transactions. Every `Reflect` call in
//! the cell lives here, behind typed functions.

use fragment_core::facet::{self, Answer, LedgerRow, Mutated, Queried};
use fragment_proto::ErrorCode;
use worker::js_sys::{self, Array, Function, Object, Promise, Reflect};
use worker::wasm_bindgen::{closure::Closure, JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;

use crate::error::{CellError, CellResult};

fn js_message(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| e.as_string())
        .unwrap_or_else(|| format!("{e:?}"))
}

fn get(obj: &JsValue, key: &str) -> CellResult<JsValue> {
    Reflect::get(obj, &JsValue::from_str(key)).map_err(|e| CellError::host(format!("{key}: {}", js_message(&e))))
}

fn set(obj: &Object, key: &str, value: impl Into<JsValue>) {
    let ok = Reflect::set(obj, &JsValue::from_str(key), &value.into()).expect("Reflect.set on a plain object");
    assert!(ok, "Reflect.set refused {key} on a plain object");
}

fn call(obj: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue, JsValue> {
    let f: Function = Reflect::get(obj, &JsValue::from_str(method))?
        .dyn_into()
        .map_err(|_| JsValue::from_str(&format!("{method} is not a function")))?;
    let list = Array::new();
    for a in args {
        list.push(a);
    }
    Reflect::apply(&f, obj, &list)
}

/// Awaits a promise or an RPC thenable (neither is `instanceof Promise`).
async fn settle(v: JsValue) -> Result<JsValue, JsValue> {
    JsFuture::from(Promise::resolve(&v)).await
}

pub(crate) fn to_js(v: &serde_json::Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).expect("serde_json output is valid JSON")
}

pub(crate) fn from_js(v: &JsValue) -> Result<serde_json::Value, String> {
    if v.is_undefined() {
        return Ok(serde_json::Value::Null);
    }
    let text = js_sys::JSON::stringify(v).map_err(|e| format!("not JSON: {}", js_message(&e)))?;
    serde_json::from_str(&String::from(text)).map_err(|e| format!("not JSON: {e}"))
}

/// `obj.method(...args)`, settled (a value, a promise, or an RPC
/// thenable): for the objects entry.mjs hands the cell (a computer's
/// `ContainerHost`). A throw or a rejection is the host's failure, its
/// message kept; but a sandcastle node that does not answer (node.mjs's
/// `NodeDown`) is `ErrorCode::NodeDown`.
pub(crate) async fn invoke(obj: &JsValue, method: &str, args: &[JsValue]) -> CellResult<JsValue> {
    let out = call(obj, method, args).map_err(|e| host_error(method, &e))?;
    settle(out).await.map_err(|e| host_error(method, &e))
}

fn host_error(method: &str, e: &JsValue) -> CellError {
    let message = js_message(e);
    let name = e.is_object().then(|| Reflect::get(e, &JsValue::from_str("name")).ok()).flatten().and_then(|n| n.as_string());
    match name.as_deref() {
        Some("NodeDown") => CellError::new(ErrorCode::NodeDown, message),
        _ => CellError::host(format!("{method}: {message}")),
    }
}

/// `obj[key]`.
pub(crate) fn property(obj: &JsValue, key: &str) -> CellResult<JsValue> {
    get(obj, key)
}

/// What the Worker Loader compiles for an app: the platform wrapper, the
/// limits it checks (`limits.js`, generated: `fragment_core::facet`), the
/// author's `app.mjs` (as `app.js`), and their `applib/` modules, with no
/// ambient network and bounded CPU. Read from the code row only when the
/// loader asks for it (`AppLoader`).
pub struct AppCode {
    /// The fragment its `FILES` capability is bound to.
    pub fragment: String,
    pub platform: &'static str,
    pub limits: &'static str,
    pub source: String,
    /// `applib/…` path → source.
    pub modules: std::collections::BTreeMap<String, String>,
    pub cpu_ms: u32,
    pub subrequests: u32,
}

/// The `app` facet of a Durable Object: author code in its own SQLite.
pub struct Facet {
    stub: JsValue,
}

/// The app facet's name for a fragment made before each life had its own
/// (`Fragment::app_facet`): `app@<incarnation>` since.
pub const APP_FACET: &str = "app";

/// The capabilities an app's env holds: `FILES`, bound to `fragment`
/// (`ctx.exports.Files({ props })`, the class in entry.mjs).
fn app_env(ctx: &JsValue, fragment: &str) -> CellResult<Object> {
    let props = Object::new();
    set(&props, "fragment", fragment);
    let options = Object::new();
    set(&options, "props", props);
    let exports = get(ctx, "exports")?;
    let files = call(&exports, "Files", &[options.into()]).map_err(|e| CellError::host(format!("ctx.exports.Files: {}", js_message(&e))))?;
    let env = Object::new();
    set(&env, "FILES", files);
    Ok(env)
}

/// The worker code object `LOADER.get` asks its callback for.
fn worker_code(ctx: &JsValue, code: &AppCode) -> CellResult<Object> {
    let modules = Object::new();
    set(&modules, "platform.js", code.platform);
    set(&modules, facet::LIMITS_MODULE, code.limits);
    set(&modules, "app.js", code.source.as_str());
    for (path, source) in &code.modules {
        // typed: the Loader infers a module's type only from `.js` and `.py`
        let module = Object::new();
        set(&module, "js", source.as_str());
        set(&modules, path, module);
    }
    let limits = Object::new();
    set(&limits, "cpuMs", code.cpu_ms);
    set(&limits, "subRequests", code.subrequests);
    let worker_code = Object::new();
    set(&worker_code, "compatibilityDate", "2026-01-01");
    set(&worker_code, "mainModule", "platform.js");
    set(&worker_code, "modules", modules);
    set(&worker_code, "env", app_env(ctx, &code.fragment)?);
    set(&worker_code, "globalOutbound", JsValue::NULL);
    set(&worker_code, "limits", limits);
    Ok(worker_code)
}

/// A callback's failure, thrown into JavaScript as an Error.
fn thrown(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

/// A JavaScript callback the cell hands to the runtime, owned by Rust for
/// the life of the activation: the runtime may call it any number of
/// times, or never (a callback made for one call and never called was
/// leaked with everything it held).
type Callback = Closure<dyn FnMut() -> Result<JsValue, JsValue>>;

/// How this fragment's app facet starts, made once per activation. Each
/// call only asks the facet table for the running facet (`facet`); the two
/// callbacks run when there is none. `start` names the worker the installed
/// code needs by its loader id and asks the loader for it; the loader runs
/// `get_code` only when it holds no worker by that id (the runtime keeps
/// `LOADER.get` workers by id and skips the callback on a hit), and
/// only then are the app's modules read and copied for it.
pub struct AppLoader {
    start: Callback,
    /// How many times this activation built worker code for the loader (a
    /// test hook reads it: `/test/fragment code-builds`).
    builds: std::rc::Rc<std::cell::Cell<u32>>,
}

impl AppLoader {
    /// `id` answers the loader id of the code installed now; `code` answers
    /// the code for a loader id, or why it cannot (that id is not installed
    /// any more), read when the loader asks.
    pub fn new(
        ctx: &JsValue,
        env: &JsValue,
        id: impl Fn() -> Result<String, String> + 'static,
        code: impl Fn(&str) -> Result<AppCode, String> + 'static,
    ) -> AppLoader {
        let ctx_for_code = ctx.clone();
        let builds = std::rc::Rc::new(std::cell::Cell::new(0u32));
        let built = builds.clone();
        let get_code: Closure<dyn FnMut(String) -> Result<JsValue, JsValue>> = Closure::new(move |id: String| -> Result<JsValue, JsValue> {
            let app = code(&id).map_err(|why| thrown(&why))?;
            built.set(built.get() + 1);
            Ok(worker_code(&ctx_for_code, &app).map_err(|e| thrown(&e.message))?.into())
        });
        let env = env.clone();
        let start: Callback = Closure::new(move || -> Result<JsValue, JsValue> {
            let id = id().map_err(|why| thrown(&why))?;
            // the loader calls its callback with no arguments: bound, it
            // is told which id it builds (a fresh bound function a call,
            // collected with it; the closure itself lives here)
            let bound = call(get_code.as_ref(), "bind", &[JsValue::NULL, JsValue::from_str(&id)])?;
            let loader = Reflect::get(&env, &JsValue::from_str("LOADER"))?;
            let worker = call(&loader, "get", &[JsValue::from_str(&id), bound])?;
            let class = call(&worker, "getDurableObjectClass", &["App".into()])?;
            let options = Object::new();
            set(&options, "class", class);
            Ok(options.into())
        });
        AppLoader { start, builds }
    }

    pub fn builds(&self) -> u32 {
        self.builds.get()
    }

    /// The running app facet `name`, or a new one started from the
    /// installed code (the caller has checked there is some).
    pub fn facet(&self, ctx: &JsValue, name: &str) -> CellResult<Facet> {
        let facets = get(ctx, "facets")?;
        let stub = call(&facets, "get", &[name.into(), self.start.as_ref().clone()]).map_err(|e| CellError::host(format!("facets.get: {}", js_message(&e))))?;
        Ok(Facet { stub })
    }
}

/// Stops the running app facet `name` (its database stays), so the next
/// call starts the newly installed class.
pub fn abort_app_facet(ctx: &JsValue, name: &str, reason: &str) -> CellResult<()> {
    let facets = get(ctx, "facets")?;
    call(&facets, "abort", &[name.into(), js_sys::Error::new(reason).into()])
        .map_err(|e| CellError::host(format!("facets.abort: {}", js_message(&e))))?;
    Ok(())
}

/// A failure calling into the app facet: the author's (a throw, or
/// `Worker exceeded CPU time limit.`), unless the runtime refused the call
/// for now: each request may have at most 10 dynamic worker invocations in
/// flight (spike S1), so that one is the platform's, and says so. Both
/// carry `overloaded: true`; nothing retries on it.
fn facet_error(message: String) -> CellError {
    if message.contains("Dynamic worker concurrency limit exceeded") {
        CellError::new(ErrorCode::NodeFull, "this fragment's app is answering as many calls at once as it may; try again shortly")
    } else {
        CellError::new(ErrorCode::AppFailed, message)
    }
}

impl Facet {
    /// Forwards a request to the facet's `fetch` (the author's custom routes).
    pub async fn fetch(&self, req: worker::Request) -> CellResult<worker::Response> {
        let raw = JsValue::from(req.inner());
        let pending = call(&self.stub, "fetch", &[raw]).map_err(|e| facet_error(js_message(&e)))?;
        let out = settle(pending).await.map_err(|e| facet_error(js_message(&e)))?;
        let resp: worker_sys::web_sys::Response =
            out.dyn_into().map_err(|_| CellError::new(ErrorCode::AppFailed, "the app's fetch did not return a Response"))?;
        // The app's response has immutable headers; the platform adds its
        // own (cookies), so it answers a copy around the same body.
        let headers = worker_sys::web_sys::Headers::new_with_headers(&resp.headers()).map_err(|e| CellError::host(js_message(&e)))?;
        let init = worker_sys::web_sys::ResponseInit::new();
        init.set_status(resp.status());
        init.set_headers(&headers);
        let copy = worker_sys::web_sys::Response::new_with_opt_readable_stream_and_init(resp.body().as_ref(), &init).map_err(|e| CellError::host(js_message(&e)))?;
        Ok(worker::Response::from(copy))
    }

    /// Calls a platform method on the facet (a job's `__job`). An
    /// exception from author code comes back as `AppFailed`.
    pub async fn call(&self, method: &str, args: &[serde_json::Value]) -> CellResult<serde_json::Value> {
        let args: Vec<JsValue> = args.iter().map(to_js).collect();
        let out = self.invoke(method, &args).await?;
        from_js(&out).map_err(CellError::host)
    }

    async fn invoke(&self, method: &str, args: &[JsValue]) -> CellResult<JsValue> {
        let pending = call(&self.stub, method, args).map_err(|e| facet_error(js_message(&e)))?;
        settle(pending).await.map_err(|e| facet_error(js_message(&e)))
    }

    /// A platform method's answer, the JSON text it made (platform.mjs):
    /// one copy across, nothing stringified or parsed on the way. An answer
    /// that is not text is the app's failure: author code shares the
    /// platform code's realm and can break it.
    async fn answer_text(&self, method: &str, args: &[JsValue]) -> CellResult<String> {
        let out = self.invoke(method, args).await?;
        out.as_string().ok_or_else(|| app_answer(method, "not JSON text".into()))
    }

    /// Runs a query: `__query(op, input, meta)`, the input as its canonical JSON text.
    pub async fn query(&self, op: &str, input: &str, meta: &serde_json::Value) -> CellResult<Answer<Queried>> {
        let text = self.answer_text("__query", &[op.into(), input.into(), to_js(meta)]).await?;
        facet::decode(&text).map_err(|why| app_answer("__query", why))
    }

    /// Runs a mutation: `__mutate(ledger id, op, input sha, input, meta)`.
    pub async fn mutate(&self, m: Mutation<'_>) -> CellResult<Answer<Mutated>> {
        let args = [m.ledger_id.into(), m.op.into(), m.input_sha.into(), m.input.into(), to_js(m.meta)];
        let text = self.answer_text("__mutate", &args).await?;
        facet::decode(&text).map_err(|why| app_answer("__mutate", why))
    }

    /// The facet's ledger row for a ledger id: `__ledger(id)`.
    pub async fn ledger(&self, ledger_id: &str) -> CellResult<Option<LedgerRow>> {
        let text = self.answer_text("__ledger", &[ledger_id.into()]).await?;
        facet::decode_ledger(&text).map_err(|why| app_answer("__ledger", why))
    }
}

/// One mutation's call into the facet.
pub struct Mutation<'a> {
    pub ledger_id: &'a str,
    pub op: &'a str,
    pub input_sha: &'a str,
    /// The input's canonical JSON text, as it was hashed.
    pub input: &'a str,
    pub meta: &'a serde_json::Value,
}

/// An answer the platform code never gives: the app's realm broke it.
fn app_answer(method: &str, why: String) -> CellError {
    CellError::new(ErrorCode::AppFailed, format!("the app facet's {method} answered what its platform code never does: {why}"))
}

/// Stops the app facet `name` and deletes its database (a deleted
/// fragment), waiting until the runtime has.
pub async fn delete_app_facet(ctx: &JsValue, name: &str) -> CellResult<()> {
    let facets = get(ctx, "facets")?;
    let done = call(&facets, "delete", &[name.into()]).map_err(|e| CellError::host(format!("facets.delete: {}", js_message(&e))))?;
    JsFuture::from(Promise::resolve(&done)).await.map_err(|e| CellError::host(format!("facets.delete: {}", js_message(&e))))?;
    Ok(())
}

/// A binding of the node's; `section` is where wrangler.jsonc declares it.
/// Whether the deployment declares the binding `name`.
pub fn has_binding(env: &JsValue, name: &str) -> bool {
    get(env, name).is_ok_and(|b| !b.is_undefined())
}

fn binding(env: &JsValue, name: &str, section: &str) -> CellResult<JsValue> {
    let binding = get(env, name)?;
    if binding.is_undefined() {
        return Err(CellError::host(format!("this node has no {name} binding (wrangler.jsonc `{section}`)")));
    }
    Ok(binding)
}

/// The Workflows binding that runs jobs (`JOBS`, class `Job` in entry.mjs).
fn jobs(env: &JsValue) -> CellResult<JsValue> {
    binding(env, "JOBS", "workflows")
}

/// Starts a job's Workflow instance; an instance that already exists is
/// left as it is, so starting twice is harmless.
pub async fn jobs_create(env: &JsValue, id: &str, params: &serde_json::Value) -> CellResult<()> {
    let item = Object::new();
    set(&item, "id", id);
    set(&item, "params", to_js(params));
    await_js(call(&jobs(env)?, "createBatch", &[Array::of1(&item).into()]), "createBatch").await?;
    Ok(())
}

/// The Workflows binding rejects `get(id)` for an instance it has no
/// record of with an Error whose message ends so (`WORKFLOW_ERROR:
/// instance does not exist`).
const NO_INSTANCE: &str = "instance does not exist";

/// A Workflow instance's `status()`: `{status, error?, output?}`, or
/// `None` when the binding has no such instance.
pub async fn jobs_status(env: &JsValue, id: &str) -> Result<Option<serde_json::Value>, String> {
    let binding = jobs(env).map_err(|e| e.message)?;
    let instance = match settle(call(&binding, "get", &[id.into()]).map_err(|e| js_message(&e))?).await {
        Ok(instance) => instance,
        Err(e) if js_message(&e).ends_with(NO_INSTANCE) => return Ok(None),
        Err(e) => return Err(js_message(&e)),
    };
    let status = settle(call(&instance, "status", &[]).map_err(|e| js_message(&e))?).await.map_err(|e| js_message(&e))?;
    from_js(&status).map(Some)
}

/// The fleet's blob store (`BLOBS`, an R2 binding over the fleet bucket).
fn blobs(env: &JsValue) -> CellResult<JsValue> {
    binding(env, "BLOBS", "r2_buckets")
}

async fn await_js(v: Result<JsValue, JsValue>, what: &str) -> CellResult<JsValue> {
    let pending = v.map_err(|e| CellError::host(format!("{what}: {}", js_message(&e))))?;
    settle(pending).await.map_err(|e| CellError::host(format!("{what}: {}", js_message(&e))))
}

/// Streams `body` into the blob store at `key` while hashing it
/// (`crypto.DigestStream`): answers (bytes stored, SHA-256 hex).
pub async fn blob_put(env: &JsValue, key: &str, body: JsValue) -> CellResult<(u64, String)> {
    let bucket = blobs(env)?;
    let pair: Array = call(&body, "tee", &[]).map_err(|e| CellError::host(format!("tee: {}", js_message(&e))))?.unchecked_into();
    let crypto = get(&js_sys::global(), "crypto")?;
    let digest_class: Function = get(&crypto, "DigestStream")?.dyn_into().map_err(|_| CellError::host("crypto.DigestStream is missing"))?;
    let args = Array::of1(&JsValue::from_str("SHA-256"));
    let digest = Reflect::construct(&digest_class, &args).map_err(|e| CellError::host(format!("DigestStream: {}", js_message(&e))))?;
    let put = call(&bucket, "put", &[key.into(), pair.get(0)]);
    let pipe = call(&pair.get(1), "pipeTo", std::slice::from_ref(&digest));
    let both = Array::of2(&put.map_err(|e| CellError::host(format!("put: {}", js_message(&e))))?, &pipe.map_err(|e| CellError::host(format!("pipeTo: {}", js_message(&e))))?);
    let done = JsFuture::from(Promise::all(&both)).await.map_err(|e| CellError::host(format!("storing the blob: {}", js_message(&e))))?;
    let object = Array::from(&done).get(0);
    let size = get(&object, "size")?.as_f64().unwrap_or(0.0) as u64;
    let hash = await_js(Ok(get(&digest, "digest")?), "digest").await?;
    let bytes = js_sys::Uint8Array::new(&hash).to_vec();
    Ok((size, hex::encode(bytes)))
}

/// Reads a request's body to its end, keeping nothing, when nothing read
/// it: the router streams a blob upload from its client, and its stream
/// still flowing after the answer is an error there ("can't read from
/// request stream after response has been sent"), so an answer that
/// needed none of the bytes (a refusal, bytes already stored) waits for
/// them first.
pub async fn drain(req: &worker::Request) -> CellResult<()> {
    let inner = req.inner();
    let Some(body) = inner.body() else { return Ok(()) };
    if inner.body_used() || body.locked() {
        return Ok(());
    }
    let sink_class: Function = get(&js_sys::global(), "WritableStream")?.dyn_into().map_err(|_| CellError::host("WritableStream is missing"))?;
    let sink = Reflect::construct(&sink_class, &Array::new()).map_err(|e| CellError::host(format!("WritableStream: {}", js_message(&e))))?;
    await_js(call(&body, "pipeTo", &[sink]), "draining the body").await?;
    Ok(())
}

/// Sends messages to a queue binding (`sendBatch`, at most 100 a call),
/// through the binding itself: its bodies are JSON values already.
pub async fn queue_send(env: &JsValue, binding: &str, bodies: &[serde_json::Value]) -> CellResult<()> {
    let queue = self::binding(env, binding, "queues")?;
    for chunk in bodies.chunks(100) {
        let list = Array::new();
        for b in chunk {
            let m = Object::new();
            set(&m, "body", to_js(b));
            list.push(&m);
        }
        await_js(call(&queue, "sendBatch", &[list.into()]), "sendBatch").await?;
    }
    Ok(())
}

/// Hands a request, as it came (method, URL, headers, body), to a service
/// binding: the agents' script, co-hosted in this fleet.
pub async fn service_fetch(env: &JsValue, binding: &str, req: worker::Request) -> CellResult<worker::Response> {
    let service = self::binding(env, binding, "services")?;
    let out = await_js(call(&service, "fetch", &[JsValue::from(req.inner())]), binding).await?;
    let resp: worker_sys::web_sys::Response = out.dyn_into().map_err(|_| CellError::host(format!("{binding} answered no Response")))?;
    Ok(worker::Response::from(resp))
}

/// A request to the Browser Rendering binding (`BROWSER`, wrangler.jsonc
/// `browser`), through its `fetch`: the routes `@cloudflare/puppeteer`
/// speaks to it, a WebSocket upgrade among them (card.rs).
pub async fn browser_fetch(env: &JsValue, req: worker::Request) -> CellResult<worker::Response> {
    let browser = self::binding(env, "BROWSER", "browser")?;
    let out = await_js(call(&browser, "fetch", &[JsValue::from(req.inner())]), "BROWSER").await?;
    let resp: worker_sys::web_sys::Response = out.dyn_into().map_err(|_| CellError::host("BROWSER answered no Response"))?;
    Ok(worker::Response::from(resp))
}

/// Stores bytes at `key`.
pub async fn blob_put_bytes(env: &JsValue, key: &str, bytes: &[u8]) -> CellResult<()> {
    let data = js_sys::Uint8Array::from(bytes);
    await_js(call(&blobs(env)?, "put", &[key.into(), data.into()]), "put").await?;
    Ok(())
}

/// A blob's size, or `None` when absent.
pub async fn blob_head(env: &JsValue, key: &str) -> CellResult<Option<u64>> {
    let object = await_js(call(&blobs(env)?, "head", &[key.into()]), "head").await?;
    if object.is_null() || object.is_undefined() {
        return Ok(None);
    }
    Ok(Some(get(&object, "size")?.as_f64().unwrap_or(0.0) as u64))
}

/// A blob's bytes as a stream: (body, whole size, the served range as
/// (offset, length) when `range` asked for one), or `None` when absent.
pub struct BlobBody {
    pub body: worker_sys::web_sys::ReadableStream,
    pub size: u64,
    pub range: Option<(u64, u64)>,
}

pub async fn blob_get(env: &JsValue, key: &str, range: Option<&str>) -> CellResult<Option<BlobBody>> {
    let options = Object::new();
    if let Some(r) = range {
        let headers = worker_sys::web_sys::Headers::new().map_err(|e| CellError::host(js_message(&e)))?;
        headers.set("range", r).map_err(|e| CellError::host(js_message(&e)))?;
        set(&options, "range", JsValue::from(headers));
    }
    let object = await_js(call(&blobs(env)?, "get", &[key.into(), options.into()]), "get").await?;
    if object.is_null() || object.is_undefined() {
        return Ok(None);
    }
    let size = get(&object, "size")?.as_f64().unwrap_or(0.0) as u64;
    let served = get(&object, "range")?;
    let range = if range.is_some() && !served.is_undefined() && !served.is_null() {
        let offset = get(&served, "offset")?.as_f64().unwrap_or(0.0) as u64;
        let length = get(&served, "length")?.as_f64().map_or(size.saturating_sub(offset), |l| l as u64);
        Some((offset, length))
    } else {
        None
    };
    let body = get(&object, "body")?.dyn_into().map_err(|_| CellError::host("a blob without a body stream"))?;
    Ok(Some(BlobBody { body, size, range }))
}

pub async fn blob_delete(env: &JsValue, keys: &[String]) -> CellResult<()> {
    let list = Array::new();
    for k in keys {
        list.push(&JsValue::from_str(k));
    }
    await_js(call(&blobs(env)?, "delete", &[list.into()]), "delete").await?;
    Ok(())
}

/// Keys under `prefix`, a page at a time: (keys, the cursor for the next page).
pub async fn blob_list(env: &JsValue, prefix: &str, cursor: Option<&str>) -> CellResult<(Vec<String>, Option<String>)> {
    let options = Object::new();
    set(&options, "prefix", prefix);
    if let Some(c) = cursor {
        set(&options, "cursor", c);
    }
    let listed = await_js(call(&blobs(env)?, "list", &[options.into()]), "list").await?;
    let objects = Array::from(&get(&listed, "objects")?);
    let keys = objects.iter().filter_map(|o| get(&o, "key").ok().and_then(|k| k.as_string())).collect();
    let next = if get(&listed, "truncated")?.as_bool() == Some(true) { get(&listed, "cursor")?.as_string() } else { None };
    Ok((keys, next))
}

/// Runs `f` as one `storage.transactionSync` of the Durable Object whose
/// state is `state` (workers-rs 0.8.5 has no synchronous transaction): it
/// commits when `f` answers `Ok`, and rolls back when `f` answers `Err`,
/// which is thrown into the runtime so that it rolls back, then answered
/// here as it was. `f` is synchronous, as SQL in a Durable Object is: no
/// other event runs inside it.
pub fn transaction_sync<T: 'static>(state: &JsValue, f: impl FnOnce() -> CellResult<T> + 'static) -> CellResult<T> {
    let storage = get(state, "storage")?;
    let slot: std::rc::Rc<std::cell::RefCell<Option<CellResult<T>>>> = std::rc::Rc::default();
    let out = slot.clone();
    let body = Closure::once(move || -> Result<JsValue, JsValue> {
        let answer = f();
        let failed = answer.is_err();
        *out.borrow_mut() = Some(answer);
        if failed {
            return Err(thrown("rolled back"));
        }
        Ok(JsValue::UNDEFINED)
    });
    let ran = call(&storage, "transactionSync", &[body.as_ref().clone()]);
    let answered = slot.borrow_mut().take();
    match (ran, answered) {
        (Ok(_), Some(Ok(v))) => Ok(v),
        // f's own refusal: the runtime rolled back and threw it back here
        (Err(_), Some(Err(e))) => Err(e),
        // f answered, and the commit failed; or the runtime never ran f
        (Err(e), _) => Err(CellError::host(format!("transactionSync: {}", js_message(&e)))),
        (Ok(_), Some(Err(_))) => unreachable!("transactionSync returned past a callback that threw"),
        (Ok(_), None) => unreachable!("transactionSync returned without running its callback"),
    }
}

/// `env.AI.run(model, input, {gateway, returnRawResponse: true})`: the
/// model's answer as the vendor sent it, through the named AI Gateway
/// (spike S4: the binding is pre-authenticated, so the Worker holds no
/// token). `options` is `{gateway: {id, metadata, collectLog}, extraHeaders}`.
pub async fn ai_run(env: &JsValue, model: &str, input: &serde_json::Value, options: &serde_json::Value) -> CellResult<worker::Response> {
    let ai = binding(env, "AI", "ai")?;
    let opts = to_js(options);
    set(opts.unchecked_ref::<Object>(), "returnRawResponse", true);
    let out = await_js(call(&ai, "run", &[model.into(), to_js(input), opts]), "AI.run").await?;
    let resp: worker_sys::web_sys::Response = out.dyn_into().map_err(|_| CellError::host("AI.run answered no Response"))?;
    Ok(worker::Response::from(resp))
}

/// A stream as two that read the same bytes (`ReadableStream.tee`): one
/// may be read while the other is dropped.
pub fn tee(stream: &worker_sys::web_sys::ReadableStream) -> CellResult<(worker_sys::web_sys::ReadableStream, worker_sys::web_sys::ReadableStream)> {
    let pair: Array = call(stream.as_ref(), "tee", &[]).map_err(|e| CellError::host(format!("tee: {}", js_message(&e))))?.unchecked_into();
    let branch = |i: u32| pair.get(i).dyn_into::<worker_sys::web_sys::ReadableStream>().map_err(|_| CellError::host("tee answered no stream"));
    Ok((branch(0)?, branch(1)?))
}

pub fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// Cryptographically random bytes (the runtime's `crypto.getRandomValues`).
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let crypto = Reflect::get(&js_sys::global(), &JsValue::from_str("crypto")).expect("globalThis.crypto");
    let buf = js_sys::Uint8Array::new_with_length(N as u32);
    call(&crypto, "getRandomValues", &[buf.clone().into()]).expect("crypto.getRandomValues");
    let mut out = [0u8; N];
    buf.copy_to(&mut out);
    out
}

pub fn random_hex<const N: usize>() -> String {
    hex::encode(random_bytes::<N>())
}
