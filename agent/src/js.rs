//! The few platform calls the agent makes outside workers-rs.

use worker::js_sys::{self, Function, Reflect};
use worker::wasm_bindgen::{JsCast, JsValue};

pub fn now_ms() -> u64 {
    worker::Date::now().as_millis()
}

/// Bytes from the platform's CSPRNG (`crypto.getRandomValues`).
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
