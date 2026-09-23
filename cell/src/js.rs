//! The JavaScript surfaces workers-rs 0.8.5 does not wrap: the Worker
//! Loader and Durable Object facets. Every `Reflect` call in the cell lives
//! here, behind typed functions.

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

/// Starts the `app` facet from `code` (or reaches the running one).
pub fn app_facet(ctx: &JsValue, env: &JsValue, code: &AppCode<'_>) -> CellResult<Facet> {
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
    set(&worker_code, "env", Object::new());
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
        Ok(worker::Response::from(resp))
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
