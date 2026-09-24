//! The agents' script (`agent/`), co-hosted in this fleet (docs/phase-6.md,
//! step 4). It has no ingress of its own: this router authenticates each
//! request as it does its own (a signature resolved to an identity by the
//! registry) and hands it on with the caller's identity, which the script
//! trusts. An inbox delivery, and a computer that connects out asking for
//! work, pass as they came: their tokens are the capability.
//!
//! An agent's name is `<label>.<username>`, its owner's username, as a
//! fragment's is; a bare label names one of the signer's own. Making an
//! agent also registers it as its owner's, in the same request.

use serde_json::{json, Value};
use worker::*;

use fragment_core::npub;
use fragment_proto::{split_fragment_name, valid_fragment_name, valid_label, ErrorCode, IdentityKind};

use crate::error::{CellError, CellResult};
use crate::registry::calls;
use crate::routed::Signed;
use crate::{ask_registry, js, read_body, signer};

/// The caller's identity, set by this router only (the script has no other
/// way in); one name for both ends (`fragment_proto::routed`).
pub(crate) const PRINCIPAL_HEADER: &str = fragment_proto::routed::AGENT_PRINCIPAL;
/// Each person's own agent, made on first need (a chat's).
pub(crate) const DEFAULT_LABEL: &str = "agent";

/// `/api/agents` and `/api/a/*`.
pub(crate) async fn route(mut req: Request, env: &Env, url: &Url, segments: &[&str]) -> CellResult<Response> {
    // an inbox delivery (its token) and a computer that connects out (its
    // connect token) carry their own capability
    if let (Method::Post, ["api", "a", _, "inbox", _] | ["api", "a", _, "computer", "poll" | "answer"]) = (req.method(), segments) {
        return js::service_fetch(env.as_ref(), "AGENTS", req).await;
    }
    let body = read_body(&mut req, fragment_proto::limits::BODY_MAX_BYTES).await?;
    let who = signer(env, &req, url, &body).await?;
    match (req.method(), segments) {
        (Method::Post, ["api", "agents"]) => {
            let v: Value = serde_json::from_slice(&body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            let label = v["name"].as_str().unwrap_or_default();
            json_answer(&create(env, &who, label, &v).await?)
        }
        (method, ["api", "a", name, rest @ ..]) => {
            let name = named(name, &who)?;
            let path = if rest.is_empty() { format!("/api/a/{name}") } else { format!("/api/a/{name}/{}", rest.join("/")) };
            ask(env, method, &path, &who.id, body).await
        }
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {}", m.as_ref(), url.path()))),
    }
}

fn json_answer(v: &Value) -> CellResult<Response> {
    Ok(Response::from_json(v)?)
}

/// An agent's full name: `<label>.<username>`, or a bare label under the
/// signer's username (an agent's: its owner's).
fn named(name: &str, who: &Signed) -> CellResult<String> {
    if valid_fragment_name(name) {
        return Ok(name.to_string());
    }
    match (valid_label(name), who.username.as_deref()) {
        (true, Some(username)) => Ok(fragment_proto::fragment_name(name, username)),
        (true, None) => Err(CellError::invalid("choose a username first: agents live under it")),
        (false, _) => Err(CellError::invalid("an agent's name is <label>.<username>")),
    }
}

/// Asks the agents' script, as `principal`.
pub(crate) async fn ask(env: &Env, method: Method, path: &str, principal: &str, body: Vec<u8>) -> CellResult<Response> {
    let headers = Headers::new();
    headers.set(PRINCIPAL_HEADER, principal)?;
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if !body.is_empty() {
        init.with_body(Some(js_sys::Uint8Array::from(body.as_slice()).into()));
    }
    let req = Request::new_with_init(&format!("https://agents.internal{path}"), &init)?;
    js::service_fetch(env.as_ref(), "AGENTS", req).await
}

/// `ask`, for its JSON answer; a refusal comes back as its error.
pub(crate) async fn ask_json(env: &Env, method: Method, path: &str, principal: &str, body: &Value) -> CellResult<Value> {
    let bytes = if body.is_null() { Vec::new() } else { serde_json::to_vec(body).map_err(|e| CellError::host(e.to_string()))? };
    let mut resp = ask(env, method, path, principal, bytes).await?;
    let status = resp.status_code();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if status == 200 {
        return Ok(v);
    }
    let code = serde_json::from_value::<ErrorCode>(v["error"].clone()).unwrap_or(ErrorCode::UpstreamFailed);
    Err(CellError::new(code, v["message"].as_str().unwrap_or("the agents' script refused").to_string()))
}

/// Makes a person's agent `<label>.<username>` (the options are its
/// `model` and `instructions`) and registers its key as theirs: `{name,
/// npub, model, id}`. Asking again for one already theirs answers it
/// (`replayed`), registered.
pub(crate) async fn create(env: &Env, who: &Signed, label: &str, options: &Value) -> CellResult<Value> {
    if who.kind != IdentityKind::Person {
        return Err(CellError::new(ErrorCode::Forbidden, "agents are made by people"));
    }
    let name = named(label, who)?;
    if split_fragment_name(&name).map(|(_, u)| u) != who.username.as_deref() {
        return Err(CellError::new(ErrorCode::Forbidden, "you make agents under your own username"));
    }
    let body = json!({ "name": name, "model": options["model"], "instructions": options["instructions"] });
    let mut made = ask_json(env, Method::Post, "/api/agents", &who.id, &body).await?;
    let hex = made["npub"].as_str().and_then(npub::parse).ok_or_else(|| CellError::host("the agents' script answered no key"))?;
    let registered = ask_registry(env, &calls::RegisterAgent { owner: who.id.clone(), key: hex }).await?;
    made["id"] = json!(registered.id);
    Ok(made)
}

/// The person's own agent, `agent.<username>`, made on first need:
/// `(identity, npub, name)`.
pub(crate) async fn own_agent(env: &Env, owner: &str, username: &str) -> CellResult<(String, String, String)> {
    let identity = fragment_proto::Identity { id: owner.to_string(), kind: IdentityKind::Person, owner: None, username: Some(username.to_string()) };
    let who = Signed { identity, key: None };
    let made = create(env, &who, DEFAULT_LABEL, &Value::Null).await?;
    let text = |k: &str| made[k].as_str().map(str::to_string).ok_or_else(|| CellError::host(format!("the agent answered no {k}")));
    Ok((text("id")?, text("npub")?, text("name")?))
}
