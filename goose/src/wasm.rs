use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use js_sys::{Function, Promise, Reflect};
use rmcp::model::JsonObject;
use serde_json::{json, Value};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

struct Callbacks(JsValue);
// The GDK alpha.9 uses Send/Sync bounds. Workers' wasm32 has one thread;
// callbacks never cross it (the same adapter as the former agent Worker).
unsafe impl Send for Callbacks {}
unsafe impl Sync for Callbacks {}
struct LocalFuture<F>(F);
unsafe impl<F> Send for LocalFuture<F> {}
impl<F: Future + Unpin> Future for LocalFuture<F> {
    type Output = F::Output;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

impl Callbacks {
    async fn ask(&self, name: &str, input: Value) -> Result<Value> {
        let answer = {
            let f: Function = Reflect::get(&self.0, &JsValue::from_str(name))
                .map_err(error)?
                .dyn_into()
                .map_err(error)?;
            f.call1(&JsValue::NULL, &JsValue::from_str(&input.to_string()))
                .map_err(error)?
        };
        let answer = LocalFuture(JsFuture::from(Promise::resolve(&answer)))
            .await
            .map_err(error)?;
        serde_json::from_str(
            &answer
                .as_string()
                .ok_or_else(|| anyhow!("Goose callback {name} returned no JSON text"))?,
        )
        .map_err(Into::into)
    }
}
fn error(e: JsValue) -> anyhow::Error {
    anyhow!(
        "{}",
        e.as_string()
            .or_else(|| Reflect::get(&e, &JsValue::from_str("message"))
                .ok()
                .and_then(|v| v.as_string()))
            .unwrap_or_else(|| "Goose callback failed".into())
    )
}

#[async_trait]
impl super::Host for Callbacks {
    async fn model(&self, messages: &[Value]) -> Result<Value> {
        self.ask("model", json!(messages)).await
    }
    async fn tool(&self, id: &str, name: &str, args: &JsonObject) -> Result<Value> {
        self.ask("tool", json!({ "id": id, "name": name, "args": args }))
            .await
    }
    async fn commit(&self, event: Value) -> Result<Value> {
        self.ask("commit", event).await
    }
}

#[wasm_bindgen]
pub async fn run_goose(input: String, callbacks: JsValue) -> Result<String, JsValue> {
    let run = async {
        let input: Value = serde_json::from_str(&input)?;
        let messages = serde_json::from_value(input["messages"].clone())?;
        let tools = serde_json::from_value(input["tools"].clone())?;
        super::run(messages, tools, Arc::new(Callbacks(callbacks))).await
    };
    run.await
        .map(|v| v.to_string())
        .map_err(|e: anyhow::Error| js_sys::Error::new(&e.to_string()).into())
}
