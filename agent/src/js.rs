//! The few platform calls the agent makes outside workers-rs.

use worker::js_sys::{self, Function, Reflect};
use worker::wasm_bindgen::{JsCast, JsValue};

pub fn now_ms() -> u64 {
    worker::Date::now().as_millis()
}

/// Bytes from the OS: celld's `crypto.getRandomValues` fills them in a
/// host op (`getrandom`), the source `KEYS` draws from too.
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let crypto = Reflect::get(&js_sys::global(), &JsValue::from_str("crypto")).expect("globalThis.crypto");
    let get: Function = Reflect::get(&crypto, &JsValue::from_str("getRandomValues"))
        .expect("crypto.getRandomValues")
        .dyn_into()
        .expect("getRandomValues is a function");
    let buf = js_sys::Uint8Array::new_with_length(N as u32);
    get.call1(&crypto, &buf).expect("crypto.getRandomValues");
    let mut out = [0u8; N];
    buf.copy_to(&mut out);
    out
}

/// POSTs `body` (JSON) to a service binding: (status, the answer's text).
/// workers-rs 0.8.5 refuses celld's service stubs (its constructor is not
/// named `Fetcher`), so this goes to the binding itself.
pub async fn service_post(env: &JsValue, binding: &str, url: &str, body: &str) -> anyhow::Result<(u16, String)> {
    use worker::js_sys::{Object, Promise};
    use worker::wasm_bindgen_futures::JsFuture;
    let message = |e: JsValue| e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message())).or_else(|| e.as_string()).unwrap_or_else(|| format!("{e:?}"));
    let get = |o: &JsValue, k: &str| Reflect::get(o, &JsValue::from_str(k)).map_err(|e| anyhow::anyhow!("{k}: {}", message(e)));
    let service = get(env, binding)?;
    if service.is_undefined() {
        anyhow::bail!("this node has no {binding} binding (wrangler.jsonc `services`)");
    }
    let headers = Object::new();
    Reflect::set(&headers, &"content-type".into(), &"application/json".into()).expect("set on a plain object");
    let init = Object::new();
    for (k, v) in [("method", JsValue::from_str("POST")), ("headers", headers.into()), ("body", JsValue::from_str(body))] {
        Reflect::set(&init, &k.into(), &v).expect("set on a plain object");
    }
    let fetch: Function = get(&service, "fetch")?.dyn_into().map_err(|_| anyhow::anyhow!("{binding}.fetch is not a function"))?;
    let pending = fetch.call2(&service, &url.into(), &init).map_err(|e| anyhow::anyhow!("{binding}: {}", message(e)))?;
    let resp = JsFuture::from(Promise::resolve(&pending)).await.map_err(|e| anyhow::anyhow!("{binding}: {}", message(e)))?;
    let status = get(&resp, "status")?.as_f64().unwrap_or(0.0) as u16;
    let text: Function = get(&resp, "text")?.dyn_into().map_err(|_| anyhow::anyhow!("a response without text()"))?;
    let text = JsFuture::from(Promise::resolve(&text.call0(&resp).map_err(|e| anyhow::anyhow!("{binding}: {}", message(e)))?))
        .await
        .map_err(|e| anyhow::anyhow!("{binding}: {}", message(e)))?;
    Ok((status, text.as_string().unwrap_or_default()))
}
