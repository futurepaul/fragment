//! MCP servers at the router (docs/api.md, A fragment's MCP server; the
//! protocol's envelope is `fragment_core::mcp`). A fragment's operations
//! are tools at its own origin's `/__mcp`, beside `__op`, for a connected
//! client (Claude, ChatGPT, any other: docs/api.md, Connected clients)
//! acting as the person who connected it:
//!
//!   POST /__mcp                                   JSON-RPC, a bearer token bound to this fragment
//!   GET  /.well-known/oauth-protected-resource[/__mcp]   its metadata (RFC 9728): the platform
//!                                                 is its authorization server
//!
//! The token is asked of the registry on every request, for this
//! resource only, so a token of another fragment's, or one ended a moment
//! before, is 401. Cookies count for nothing here, and a page never calls
//! it (an `Origin` is 403). `tools/list` asks the fragment which
//! operations the person may call; `tools/call` is the fragment's
//! `POST /api/ops/<op>`, as `fragment call` is: the role check, the
//! schema check, the ledger, the public budget, all the fragment's.

use fragment_core::mcp::{self, Answer, Asked, Era, Step};
use fragment_proto::{limits, ErrorBody, ErrorCode};
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::registry::calls;
use crate::routed::{Routed, Signed, Through};
use crate::{ask_registry, bytes_body, forward, Forward};

const WELL_KNOWN: &str = ".well-known/oauth-protected-resource";
const WELL_KNOWN_MCP: &str = ".well-known/oauth-protected-resource/__mcp";

/// Whether a path on a fragment's origin is its MCP server's.
pub fn is_fragment_route(rest: &str) -> bool {
    matches!(rest, "__mcp" | WELL_KNOWN | WELL_KNOWN_MCP)
}

/// A JSON answer that no cache keeps.
fn json_answer(status: u16, v: &Value) -> CellResult<Response> {
    let mut resp = Response::from_json(v)?.with_status(status);
    resp.headers_mut().set("cache-control", "no-store")?;
    Ok(resp)
}

/// 401, pointing the client at the resource's metadata (RFC 9728 5.1): a
/// client starts its authorization from it.
fn unauthorized(metadata: &str, invalid: bool) -> CellResult<Response> {
    let why = if invalid { "this token is not live, or is another resource's" } else { "connect a client: a bearer token is required" };
    let mut resp = CellError::new(ErrorCode::Unauthenticated, why).response()?;
    let error = if invalid { ", error=\"invalid_token\"" } else { "" };
    resp.headers_mut().set("www-authenticate", &format!("Bearer resource_metadata=\"{metadata}\"{error}"))?;
    Ok(resp)
}

/// The bearer token a request carries, if it carries one.
fn bearer(req: &Request) -> CellResult<Option<String>> {
    let header = req.headers().get("authorization")?.unwrap_or_default();
    Ok(header.strip_prefix("Bearer ").map(str::trim).filter(|t| !t.is_empty()).map(str::to_string))
}

/// What the router answers a request it could not take (the method, a page's
/// call).
fn refused(status: u16, message: &str) -> CellResult<Response> {
    json_answer(status, &mcp::error(&Value::Null, mcp::INVALID_REQUEST, message, None))
}

/// The answer to a JSON-RPC message.
fn answered(answer: Answer) -> CellResult<Response> {
    match answer {
        Answer::Json(status, v) => json_answer(status, &v),
        Answer::Accepted => Ok(Response::empty()?.with_status(202)),
    }
}

/// `__mcp` and its metadata on the fragment `name`'s origin.
pub async fn fragment(mut req: Request, env: &Env, cfg: &Config, url: &Url, name: &str, rest: &str) -> CellResult<Response> {
    let origin = cfg.origin(url, name);
    let resource = format!("{origin}/__mcp");
    let metadata = format!("{origin}/{WELL_KNOWN_MCP}");
    if rest != "__mcp" {
        if !matches!(req.method(), Method::Get | Method::Head) {
            return refused(405, "read the resource's metadata with GET");
        }
        let doc = json!({ "resource": resource, "authorization_servers": [cfg.platform()], "bearer_methods_supported": ["header"], "resource_name": name });
        return json_answer(200, &doc);
    }
    if req.method() != Method::Post {
        let mut resp = refused(405, "an MCP request is a POST (no stream is offered)")?;
        resp.headers_mut().set("allow", "POST")?;
        return Ok(resp);
    }
    // a page never calls it: a browser names its page on every POST
    if req.headers().get("origin")?.is_some() {
        return refused(403, "an MCP client is a program, not a page");
    }
    let Some(token) = bearer(&req)? else { return unauthorized(&metadata, false) };
    let body = crate::read_body(&mut req, limits::BODY_MAX_BYTES).await?;
    let live = match ask_registry(env, &calls::Connected { token, resource }).await {
        Ok(live) => live,
        Err(e) if e.code == ErrorCode::Unauthenticated => return unauthorized(&metadata, true),
        Err(e) => return Err(e),
    };
    let msg = match mcp::message(&body) {
        Ok(msg) => msg,
        Err(answer) => return answered(answer),
    };
    let header = |k: &str| req.headers().get(k);
    let (version, method, named) = (header("mcp-protocol-version")?, header("mcp-method")?, header("mcp-name")?);
    let headers = mcp::Headers { protocol_version: version.as_deref(), method: method.as_deref(), name: named.as_deref() };
    let server = mcp::Server {
        name: "fragment".into(),
        title: name.to_string(),
        version: cfg.deploy_id.clone(),
        instructions: format!(
            "The operations of {name}, a fragment (a small web app at {origin}/), as tools, called as you: a query reads; a mutation or a job takes an id you choose, and the same id again answers with the first call's result and runs nothing again. What you do here names this client."
        ),
    };
    let (era, id, asked) = match mcp::step(&msg, headers, &server) {
        Step::Done(answer) => return answered(answer),
        Step::Ask { era, id, asked } => (era, id, asked),
    };
    let signed = Signed { through: Some(Through { connection: live.connection, client: live.client }), ..Signed::new(live.identity, None) };
    let ask = |method: Method, inner: String, body: Option<Value>| {
        let routed = Routed { name: name.to_string(), url: url.clone(), signed: Some(signed.clone()), credential: None };
        async move {
            // a fresh request: nothing of the client's but what the router decided
            let bare = Request::new(url.as_str(), method)?;
            let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
            let mut resp = forward(env, &bare, bytes_body(bytes), Forward { routed, inner, extra: vec![] }).await?;
            let status = resp.status_code();
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            Ok::<_, CellError>((status, v))
        }
    };
    let answer = match asked {
        Asked::Tools => match ask(Method::Get, "/mcp/tools".into(), None).await? {
            (200, v) => mcp::result(era, &id, mcp::tools(era, v["tools"].as_array().cloned().unwrap_or_default()), &server),
            (status, v) => {
                let (_, message) = refusal(status, &v);
                let code = if status >= 500 { mcp::INTERNAL_ERROR } else { mcp::REFUSED };
                mcp::error(&id, code, &message, None)
            }
        },
        Asked::Call { name: op, arguments } => {
            let (op_id, input) = match mcp::call_of(&arguments) {
                Ok(call) => call,
                Err(why) => return json_answer(200, &mcp::error(&id, mcp::INVALID_PARAMS, &why, None)),
            };
            if !fragment_proto::valid_op_name(&op) {
                return json_answer(200, &mcp::error(&id, mcp::INVALID_PARAMS, &format!("Unknown tool: {op}"), None));
            }
            // a query keeps no id, and a caller that names none gets a fresh one
            let op_id = op_id.unwrap_or_else(|| format!("mcp-{}", crate::js::random_hex::<8>()));
            match ask(Method::Post, format!("/api/ops/{op}"), Some(json!({ "id": op_id, "input": input }))).await? {
                (200, v) => mcp::result(era, &id, mcp::called(&v), &server),
                (status, v) => failed(era, &id, status, &v, &server),
            }
        }
    };
    json_answer(200, &answer)
}

/// The fragment's refusal, as MCP has it: a tool it has none of is
/// unknown; one the platform failed at is the server's error; any other
/// (a role, a schema, a conflict, a budget) is the call's, which the model
/// may act on.
fn failed(era: Era, id: &Value, status: u16, v: &Value, server: &mcp::Server) -> Value {
    let (code, message) = refusal(status, v);
    match code {
        ErrorCode::UnknownOperation | ErrorCode::NoCode => mcp::error(id, mcp::INVALID_PARAMS, &format!("Unknown tool: {message}"), None),
        _ if status >= 500 => mcp::error(id, mcp::INTERNAL_ERROR, &message, None),
        _ => {
            let named = serde_json::to_value(code).ok().and_then(|c| c.as_str().map(str::to_string)).unwrap_or_default();
            mcp::result(era, id, mcp::refused(&named, &message), server)
        }
    }
}

/// The fragment's refusal: its code and why.
fn refusal(status: u16, v: &Value) -> (ErrorCode, String) {
    match serde_json::from_value::<ErrorBody>(v.clone()) {
        Ok(e) => (e.error, e.message),
        Err(_) => (ErrorCode::HostFailed, format!("the fragment answered {status}")),
    }
}
