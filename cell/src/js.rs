//! The JavaScript surfaces workers-rs 0.8.5 does not wrap: the Worker
//! Loader, Durable Object facets, and the Workflows binding. Every
//! `Reflect` call in the cell lives here, behind typed functions.

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

fn to_js(v: &serde_json::Value) -> JsValue {
    js_sys::JSON::parse(&v.to_string()).expect("serde_json output is valid JSON")
}

fn from_js(v: &JsValue) -> Result<serde_json::Value, String> {
    if v.is_undefined() {
        return Ok(serde_json::Value::Null);
    }
    let text = js_sys::JSON::stringify(v).map_err(|e| format!("not JSON: {}", js_message(&e)))?;
    serde_json::from_str(&String::from(text)).map_err(|e| format!("not JSON: {e}"))
}

/// What the Worker Loader compiles for an app: the platform wrapper, the
/// author's `app.mjs` (as `app.js`), and their `applib/` modules, with no
/// ambient network and bounded CPU.
pub struct AppCode<'a> {
    /// Content address of every module: the loader memoizes by id per
    /// isolate across every fragment, so the id must name the bytes.
    pub id: &'a str,
    pub platform: &'a str,
    pub source: &'a str,
    /// `applib/…` path → source.
    pub modules: &'a std::collections::BTreeMap<String, String>,
    pub cpu_ms: u32,
    pub subrequests: u32,
}

/// The `app` facet of a Durable Object: author code in its own SQLite.
pub struct Facet {
    stub: JsValue,
}

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

/// Starts the `app` facet from `code` (or reaches the running one).
pub fn app_facet(ctx: &JsValue, env: &JsValue, fragment: &str, code: &AppCode<'_>) -> CellResult<Facet> {
    let modules = Object::new();
    set(&modules, "platform.js", code.platform);
    set(&modules, "app.js", code.source);
    for (path, source) in code.modules {
        set(&modules, path, source.as_str());
    }
    let limits = Object::new();
    set(&limits, "cpuMs", code.cpu_ms);
    set(&limits, "subRequests", code.subrequests);
    let worker_code = Object::new();
    set(&worker_code, "compatibilityDate", "2026-01-01");
    set(&worker_code, "mainModule", "platform.js");
    set(&worker_code, "modules", modules);
    set(&worker_code, "env", app_env(ctx, fragment)?);
    set(&worker_code, "globalOutbound", JsValue::NULL);
    set(&worker_code, "limits", limits);

    let loader = get(env, "LOADER")?;
    let get_code = Closure::once_into_js(move || -> JsValue { worker_code.into() });
    let worker = call(&loader, "get", &[code.id.into(), get_code])
        .map_err(|e| CellError::host(format!("LOADER.get: {}", js_message(&e))))?;
    let class = call(&worker, "getDurableObjectClass", &["App".into()])
        .map_err(|e| CellError::host(format!("getDurableObjectClass: {}", js_message(&e))))?;
    let start = Closure::once_into_js(move || -> JsValue {
        let o = Object::new();
        set(&o, "class", class);
        o.into()
    });
    let facets = get(ctx, "facets")?;
    let stub = call(&facets, "get", &[APP_FACET.into(), start])
        .map_err(|e| CellError::host(format!("facets.get: {}", js_message(&e))))?;
    Ok(Facet { stub })
}

/// Stops the running `app` facet (its database stays), so the next call
/// starts the newly installed class.
pub fn abort_app_facet(ctx: &JsValue, reason: &str) -> CellResult<()> {
    let facets = get(ctx, "facets")?;
    call(&facets, "abort", &[APP_FACET.into(), js_sys::Error::new(reason).into()])
        .map_err(|e| CellError::host(format!("facets.abort: {}", js_message(&e))))?;
    Ok(())
}

impl Facet {
    /// Forwards a request to the facet's `fetch` (the author's custom routes).
    pub async fn fetch(&self, req: worker::Request) -> CellResult<worker::Response> {
        let raw = JsValue::from(req.inner());
        let pending = call(&self.stub, "fetch", &[raw]).map_err(|e| CellError::new(fragment_proto::ErrorCode::AppFailed, js_message(&e)))?;
        let out = settle(pending).await.map_err(|e| CellError::new(fragment_proto::ErrorCode::AppFailed, js_message(&e)))?;
        let resp: worker_sys::web_sys::Response =
            out.dyn_into().map_err(|_| CellError::new(fragment_proto::ErrorCode::AppFailed, "the app's fetch did not return a Response"))?;
        // The app's response has immutable headers; the platform adds its
        // own (cookies), so it answers a copy around the same body.
        let headers = worker_sys::web_sys::Headers::new_with_headers(&resp.headers()).map_err(|e| CellError::host(js_message(&e)))?;
        let init = worker_sys::web_sys::ResponseInit::new();
        init.set_status(resp.status());
        init.set_headers(&headers);
        let copy = worker_sys::web_sys::Response::new_with_opt_readable_stream_and_init(resp.body().as_ref(), &init).map_err(|e| CellError::host(js_message(&e)))?;
        Ok(worker::Response::from(copy))
    }

    /// Calls a platform method on the facet (`__query`, `__mutate`). An
    /// exception from author code comes back as `Err(message)`.
    pub async fn call(&self, method: &str, args: &[serde_json::Value]) -> Result<serde_json::Value, String> {
        let args: Vec<JsValue> = args.iter().map(to_js).collect();
        let pending = call(&self.stub, method, &args).map_err(|e| js_message(&e))?;
        let out = settle(pending).await.map_err(|e| js_message(&e))?;
        from_js(&out)
    }
}

/// Stops the `app` facet and deletes its database (a deleted fragment).
pub fn delete_app_facet(ctx: &JsValue) -> CellResult<()> {
    let facets = get(ctx, "facets")?;
    call(&facets, "delete", &[APP_FACET.into()]).map_err(|e| CellError::host(format!("facets.delete: {}", js_message(&e))))?;
    Ok(())
}

/// The Workflows binding that runs jobs (`JOBS`, class `Job` in entry.mjs).
fn jobs(env: &JsValue) -> CellResult<JsValue> {
    let binding = get(env, "JOBS")?;
    if binding.is_undefined() {
        return Err(CellError::host("this node has no JOBS Workflow binding (wrangler.jsonc `workflows`)"));
    }
    Ok(binding)
}

/// Starts a job's Workflow instance; an instance that already exists is
/// left as it is, so starting twice is harmless.
pub async fn jobs_create(env: &JsValue, id: &str, params: &serde_json::Value) -> CellResult<()> {
    let binding = jobs(env)?;
    let item = Object::new();
    set(&item, "id", id);
    set(&item, "params", to_js(params));
    let pending = call(&binding, "createBatch", &[Array::of1(&item).into()]).map_err(|e| CellError::host(format!("createBatch: {}", js_message(&e))))?;
    settle(pending).await.map_err(|e| CellError::host(format!("createBatch: {}", js_message(&e))))?;
    Ok(())
}

/// A Workflow instance's `status()`: `{status, error?, output?}`.
pub async fn jobs_status(env: &JsValue, id: &str) -> Result<serde_json::Value, String> {
    let binding = jobs(env).map_err(|e| e.message)?;
    let instance = settle(call(&binding, "get", &[id.into()]).map_err(|e| js_message(&e))?).await.map_err(|e| js_message(&e))?;
    let status = settle(call(&instance, "status", &[]).map_err(|e| js_message(&e))?).await.map_err(|e| js_message(&e))?;
    from_js(&status)
}

/// The fleet's blob store (`BLOBS`, an R2 binding over the fleet bucket).
fn blobs(env: &JsValue) -> CellResult<JsValue> {
    let binding = get(env, "BLOBS")?;
    if binding.is_undefined() {
        return Err(CellError::host("this node has no BLOBS bucket binding (wrangler.jsonc `r2_buckets`)"));
    }
    Ok(binding)
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

/// Sends messages to a queue binding (`sendBatch`, at most 100 a call).
/// workers-rs 0.8.5 refuses celld's queue binding (its constructor is not
/// named `WorkerQueue`), so this goes to the binding itself.
pub async fn queue_send(env: &JsValue, binding: &str, bodies: &[serde_json::Value]) -> CellResult<()> {
    let queue = get(env, binding)?;
    if queue.is_undefined() {
        return Err(CellError::host(format!("this node has no {binding} queue binding (wrangler.jsonc `queues`)")));
    }
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

pub fn now_ms() -> i64 {
    js_sys::Date::now() as i64
}

/// Cryptographically random bytes (`crypto.getRandomValues`).
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
