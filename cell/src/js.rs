//! The JavaScript surfaces workers-rs 0.8.5 does not wrap (the Worker
//! Loader, Durable Object facets, the Workflows binding, the AI binding's
//! options, R2's read of a `Range` header as it came, and synchronous
//! storage transactions), and the blob store's calls, on workers-rs's R2.
//! Every `Reflect` call in the cell lives here, behind typed functions.

use fragment_core::facet::{self, Answer, LedgerRow, Mutated, Queried};
use fragment_proto::ErrorCode;
use worker::crypto::{DigestStream, DigestStreamAlgorithm};
use worker::js_sys::{self, Array, Function, Object, Promise, Reflect};
// the macro's generated code names `wasm_bindgen`: worker's re-export
use worker::wasm_bindgen::{self, closure::Closure, prelude::*, JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;
use worker::web_sys::{ReadableStream, WritableStream};
use worker::{Bucket, Env};

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
/// message kept.
pub(crate) async fn invoke(obj: &JsValue, method: &str, args: &[JsValue]) -> CellResult<JsValue> {
    let out = call(obj, method, args).map_err(|e| CellError::host(format!("{method}: {}", js_message(&e))))?;
    settle(out).await.map_err(|e| CellError::host(format!("{method}: {}", js_message(&e))))
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

/// The app facet's name, before its life's `@<incarnation>`: each life of
/// a fragment's name has its own (`Fragment::app_facet`).
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
fn blobs(env: &Env) -> CellResult<Bucket> {
    Ok(env.bucket("BLOBS")?)
}

async fn await_js(v: Result<JsValue, JsValue>, what: &str) -> CellResult<JsValue> {
    let pending = v.map_err(|e| CellError::host(format!("{what}: {}", js_message(&e))))?;
    settle(pending).await.map_err(|e| CellError::host(format!("{what}: {}", js_message(&e))))
}

/// Streams `body` into the blob store at `key` while hashing it
/// (`crypto.DigestStream`): answers (bytes stored, SHA-256 hex).
pub async fn blob_put(env: &Env, key: &str, body: ReadableStream) -> CellResult<(u64, String)> {
    let (stored, hashed) = tee(&body)?;
    let digest = DigestStream::new(DigestStreamAlgorithm::Sha256);
    let bucket = blobs(env)?;
    let (put, piped) = futures_util::future::join(bucket.put(key, stored).execute(), JsFuture::from(hashed.pipe_to(digest.raw()))).await;
    let object = put.map_err(|e| CellError::host(format!("storing the blob: {e}")))?;
    piped.map_err(|e| CellError::host(format!("hashing the blob: {}", js_message(&e))))?;
    Ok((object.map_or(0, |o| o.size()), hex::encode(digest.digest().await?.to_vec())))
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
    let sink = WritableStream::new().map_err(|e| CellError::host(format!("WritableStream: {}", js_message(&e))))?;
    JsFuture::from(body.pipe_to(&sink)).await.map_err(|e| CellError::host(format!("draining the body: {}", js_message(&e))))?;
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
pub async fn blob_put_bytes(env: &Env, key: &str, bytes: &[u8]) -> CellResult<()> {
    blobs(env)?.put(key, bytes.to_vec()).execute().await?;
    Ok(())
}

/// A blob's size, or `None` when absent.
pub async fn blob_head(env: &Env, key: &str) -> CellResult<Option<u64>> {
    Ok(blobs(env)?.head(key).await?.map(|o| o.size()))
}

/// A blob's bytes as a stream: (body, whole size, the served range as
/// (offset, length) when `range` asked for one), or `None` when absent.
pub struct BlobBody {
    pub body: worker_sys::web_sys::ReadableStream,
    pub size: u64,
    pub range: Option<(u64, u64)>,
}

/// The request's own `Range` header goes to R2 as it came, which reads it
/// (workers-rs's `get` takes only a parsed range).
pub async fn blob_get(env: &Env, key: &str, range: Option<&str>) -> CellResult<Option<BlobBody>> {
    let options = Object::new();
    if let Some(r) = range {
        let headers = worker_sys::web_sys::Headers::new().map_err(|e| CellError::host(js_message(&e)))?;
        headers.set("range", r).map_err(|e| CellError::host(js_message(&e)))?;
        set(&options, "range", JsValue::from(headers));
    }
    let object = await_js(call(blobs(env)?.as_ref(), "get", &[key.into(), options.into()]), "get").await?;
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

pub async fn blob_delete(env: &Env, keys: &[String]) -> CellResult<()> {
    Ok(blobs(env)?.delete_multiple(keys.to_vec()).await?)
}

/// The first page of keys under `prefix`, and whether there are more.
pub async fn blob_list(env: &Env, prefix: &str) -> CellResult<(Vec<String>, bool)> {
    let listed = blobs(env)?.list().prefix(prefix).execute().await?;
    Ok((listed.objects().iter().map(|o| o.key()).collect(), listed.truncated()))
}

/// Deletes what BLOBS holds under `prefix`, at most `pages` pages (R2's
/// list, up to 1000 keys each) a call: how many it deleted, and whether
/// any are left (the caller's to call again). A deleted fragment's blobs
/// (ended.rs) and a wiped computer's saves (computer.rs) go so.
pub async fn blob_delete_under(env: &Env, prefix: &str, pages: usize) -> CellResult<(u64, bool)> {
    assert!(prefix.ends_with('/') && pages > 0, "a prefix of its own, a bounded number of pages");
    let mut deleted = 0u64;
    for _ in 0..pages {
        // each page listed is deleted, so the next list starts afresh
        let (keys, more) = blob_list(env, prefix).await?;
        assert!(keys.iter().all(|k| k.starts_with(prefix)), "a listing under a prefix answers keys under it");
        if !keys.is_empty() {
            blob_delete(env, &keys).await?;
            deleted += keys.len() as u64;
        }
        if !more {
            return Ok((deleted, false));
        }
    }
    Ok((deleted, true))
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

/// Sends one message through Cloudflare Email Sending (`EMAIL`, the
/// `send_email` binding; its input is `fragment_core::mail::message`):
/// its message id, or the service's error code (`E_…`) and message.
pub async fn email_send(env: &JsValue, message: &serde_json::Value) -> Result<String, (String, String)> {
    let email = binding(env, "EMAIL", "send_email").map_err(|e| (String::new(), e.message))?;
    let sent = match call(&email, "send", &[to_js(message)]) {
        Ok(pending) => settle(pending).await,
        Err(e) => Err(e),
    };
    match sent {
        Ok(answer) => get(&answer, "messageId")
            .ok()
            .and_then(|id| id.as_string())
            .ok_or_else(|| (String::new(), "EMAIL.send answered no messageId".to_string())),
        Err(e) => Err((get(&e, "code").ok().and_then(|c| c.as_string()).unwrap_or_default(), js_message(&e))),
    }
}

/// A stream as two that read the same bytes (`ReadableStream.tee`): one
/// may be read while the other is dropped.
pub fn tee(stream: &ReadableStream) -> CellResult<(ReadableStream, ReadableStream)> {
    let pair = stream.tee();
    let branch = |i: u32| pair.get(i).dyn_into::<ReadableStream>().map_err(|_| CellError::host("tee answered no stream"));
    Ok((branch(0)?, branch(1)?))
}

pub fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// Where one piece of work waited, step by step, for its log line. The
/// Workers clock moves only across I/O, so a step that waited on nothing
/// reads 0: a line shows the work's waits, not its computing.
pub struct Laps {
    start: i64,
    last: i64,
    waits: serde_json::Map<String, serde_json::Value>,
}

impl Laps {
    pub fn start() -> Laps {
        let now = now_ms();
        Laps { start: now, last: now, waits: serde_json::Map::new() }
    }

    /// The wait since the last lap (or the start), added to `step`'s.
    pub fn lap(&mut self, step: &'static str) {
        let now = now_ms();
        let waited = now - self.last + self.waits.get(step).and_then(serde_json::Value::as_i64).unwrap_or(0);
        self.waits.insert(step.to_string(), waited.into());
        self.last = now;
    }

    /// One line (lesson 14): `fields`, then `event`, `at` (when the work
    /// began, ms), its steps' `waits`, and `ms`, its whole.
    pub fn log(self, event: &str, mut fields: serde_json::Value) {
        assert!(fields.is_object(), "a line's fields are an object");
        fields["event"] = event.into();
        fields["at"] = self.start.into();
        fields["ms"] = (now_ms() - self.start).into();
        fields["waits"] = serde_json::Value::Object(self.waits);
        worker::console_log!("{fields}");
    }
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = crypto, js_name = getRandomValues)]
    fn get_random_values(buf: &mut [u8]);
}

/// Cryptographically random bytes (the runtime's `crypto.getRandomValues`).
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    get_random_values(&mut out);
    out
}

pub fn random_hex<const N: usize>() -> String {
    hex::encode(random_bytes::<N>())
}
