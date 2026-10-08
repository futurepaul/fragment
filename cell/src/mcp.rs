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
//! The platform's verbs are tools at `<platform>/mcp` (docs/api.md, The
//! platform's MCP server): the CLI's daily loop, each the API's route.
//!
//! The token is asked of the registry on every request, for this
//! resource only, so a token of another fragment's, or one ended a moment
//! before, is 401. Cookies count for nothing here, and a page never calls
//! it (an `Origin` is 403). `tools/list` asks the fragment which
//! operations the person may call; `tools/call` is the fragment's
//! `POST /api/ops/<op>`, as `fragment call` is: the role check, the
//! schema check, the ledger, the public budget, all the fragment's.

use fragment_core::mcp::verbs::{self, Verb};
use fragment_core::mcp::{self, Answer, Asked, Era, Step};
use fragment_core::npub;
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

/// A resource's metadata (RFC 9728): the platform is its authorization server.
fn resource_metadata(req: &Request, cfg: &Config, resource: &str, name: &str) -> CellResult<Response> {
    if !matches!(req.method(), Method::Get | Method::Head) {
        return refused(405, "read the resource's metadata with GET");
    }
    let doc = json!({ "resource": resource, "authorization_servers": [cfg.platform()], "bearer_methods_supported": ["header"], "resource_name": name });
    json_answer(200, &doc)
}

/// A request a server will answer: who asks (the connection's person),
/// and what (its era, its JSON-RPC id, and what it asks past the envelope).
struct Opened {
    signed: Signed,
    era: Era,
    id: Value,
    asked: Asked,
}

/// The envelope of a request to the MCP server at `resource`: its method,
/// no page's call, its bearer token (asked of the registry for this
/// resource alone), and its JSON-RPC message, answered here when the
/// envelope's own (`Err`: that answer).
async fn opened(req: &mut Request, env: &Env, resource: &str, metadata: &str, server: &mcp::Server) -> CellResult<Result<Opened, Response>> {
    if req.method() != Method::Post {
        let mut resp = refused(405, "an MCP request is a POST (no stream is offered)")?;
        resp.headers_mut().set("allow", "POST")?;
        return Ok(Err(resp));
    }
    // a page never calls it: a browser names its page on every POST
    if req.headers().get("origin")?.is_some() {
        return Ok(Err(refused(403, "an MCP client is a program, not a page")?));
    }
    let Some(token) = bearer(req)? else { return Ok(Err(unauthorized(metadata, false)?)) };
    let body = crate::read_body(req, limits::BODY_MAX_BYTES).await?;
    let live = match ask_registry(env, &calls::Connected { token, resource: resource.to_string() }).await {
        Ok(live) => live,
        Err(e) if e.code == ErrorCode::Unauthenticated => return Ok(Err(unauthorized(metadata, true)?)),
        Err(e) => return Err(e),
    };
    let msg = match mcp::message(&body) {
        Ok(msg) => msg,
        Err(answer) => return Ok(Err(answered(answer)?)),
    };
    let header = |k: &str| req.headers().get(k);
    let (version, method, named) = (header("mcp-protocol-version")?, header("mcp-method")?, header("mcp-name")?);
    let headers = mcp::Headers { protocol_version: version.as_deref(), method: method.as_deref(), name: named.as_deref() };
    let (era, id, asked) = match mcp::step(&msg, headers, server) {
        Step::Done(answer) => return Ok(Err(answered(answer)?)),
        Step::Ask { era, id, asked } => (era, id, asked),
    };
    let signed = Signed { through: Some(Through { connection: live.connection, client: live.client }), ..Signed::new(live.identity, None) };
    Ok(Ok(Opened { signed, era, id, asked }))
}

/// A request to fragment `name`'s supervisor at `inner`, as the router
/// decided it (`at`: the URL it is taken to have arrived on, its query
/// the route's): its status and its answer.
async fn ask(env: &Env, name: &str, signed: &Signed, at: &Url, method: Method, inner: String, body: Option<Value>) -> CellResult<(u16, Response)> {
    let routed = Routed { name: name.to_string(), url: at.clone(), signed: Some(signed.clone()), credential: None };
    // a fresh request: nothing of the client's but what the router decided
    let bare = Request::new(at.as_str(), method)?;
    let bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
    let resp = forward(env, &bare, bytes_body(bytes), Forward { routed, inner, extra: vec![] }).await?;
    Ok((resp.status_code(), resp))
}

async fn json_of(resp: &mut Response) -> Value {
    resp.json().await.unwrap_or(Value::Null)
}

/// `__mcp` and its metadata on the fragment `name`'s origin.
pub async fn fragment(mut req: Request, env: &Env, cfg: &Config, url: &Url, name: &str, rest: &str) -> CellResult<Response> {
    let origin = cfg.origin(url, name);
    let resource = format!("{origin}/__mcp");
    if rest != "__mcp" {
        return resource_metadata(&req, cfg, &resource, name);
    }
    let server = mcp::Server {
        name: "fragment".into(),
        title: name.to_string(),
        version: cfg.deploy_id.clone(),
        instructions: format!(
            "The operations of {name}, a fragment (a small web app at {origin}/), as tools, called as you: a query reads; a mutation or a job takes an id you choose, and the same id again answers with the first call's result and runs nothing again. What you do here names this client."
        ),
    };
    let Opened { signed, era, id, asked } = match opened(&mut req, env, &resource, &format!("{origin}/{WELL_KNOWN_MCP}"), &server).await? {
        Ok(opened) => opened,
        Err(answered) => return Ok(answered),
    };
    let answer = match asked {
        Asked::Tools => match ask(env, name, &signed, url, Method::Get, "/mcp/tools".into(), None).await? {
            (200, mut resp) => mcp::result(era, &id, mcp::tools(era, json_of(&mut resp).await["tools"].as_array().cloned().unwrap_or_default()), &server),
            (status, mut resp) => {
                let (_, message) = refusal(status, &json_of(&mut resp).await);
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
            let op_id = op_id.unwrap_or_else(fresh_id);
            let (status, mut resp) = ask(env, name, &signed, url, Method::Post, format!("/api/ops/{op}"), Some(json!({ "id": op_id, "input": input }))).await?;
            let v = json_of(&mut resp).await;
            match status {
                200 => mcp::result(era, &id, mcp::called(&v), &server),
                _ => failed(era, &id, status, &v, &server),
            }
        }
    };
    json_answer(200, &answer)
}

/// An operation id for a call that names none.
fn fresh_id() -> String {
    format!("mcp-{}", crate::js::random_hex::<8>())
}

/// `<platform>/mcp` and its metadata: the platform's verbs as tools
/// (`fragment_core::mcp::verbs`), the CLI's daily loop, each one the API's
/// route, asked as the connection's person.
pub async fn platform(mut req: Request, env: &Env, cfg: &Config, url: &Url, segments: &[&str]) -> CellResult<Response> {
    let platform = cfg.platform();
    let resource = format!("{platform}/mcp");
    if segments != ["mcp"] {
        return resource_metadata(&req, cfg, &resource, "fragment");
    }
    let server = mcp::Server {
        name: "fragment".into(),
        title: "fragment".into(),
        version: cfg.deploy_id.clone(),
        instructions: "fragment publishes small stateful web apps with built-in multiplayer, each at its own link, as you. The loop: create one from a template, read its files, write site/index.html (its page), app.mjs and fragment.json (its operations), deploy, then check status (code.error says why code was refused) and events (what happened: believe it over memory), and call its operations. A fragment is <label>.<username>; a bare label is one of yours. share and visibility say who may open it.".into(),
    };
    let metadata = format!("{platform}/.well-known/oauth-protected-resource/mcp");
    let Opened { signed, era, id, asked } = match opened(&mut req, env, &resource, &metadata, &server).await? {
        Ok(opened) => opened,
        Err(answered) => return Ok(answered),
    };
    let answer = match asked {
        Asked::Tools => mcp::result(era, &id, mcp::tools(era, verbs::tools()), &server),
        Asked::Call { name: tool, arguments } => match verbs::verb(&tool, &arguments) {
            Err(why) => mcp::error(&id, mcp::INVALID_PARAMS, &why, None),
            Ok(verb) => {
                let (status, v) = match run(env, cfg, url, &signed, verb).await {
                    Ok(answered) => answered,
                    Err(e) if e.code.status() < 500 => (e.code.status(), json!({ "error": e.code, "message": e.message })),
                    Err(e) => return Err(e),
                };
                match status {
                    200 => mcp::result(era, &id, mcp::called(&v), &server),
                    _ => failed(era, &id, status, &v, &server),
                }
            }
        },
    };
    json_answer(200, &answer)
}

/// A verb, asked of the API as the connection's person: its status and
/// its answer.
async fn run(env: &Env, cfg: &Config, url: &Url, signed: &Signed, verb: Verb) -> CellResult<(u16, Value)> {
    match verb {
        Verb::List => Ok((200, json!(crate::listed(env, &signed.id).await?))),
        Verb::Create { label, template, visibility } => {
            let create = fragment_proto::CreateFragment { name: label, visibility, template, title: None };
            let mut resp = crate::create_fragment(env, cfg, url, create, signed.clone()).await?;
            Ok((resp.status_code(), json_of(&mut resp).await))
        }
        Verb::Share { name, member, role } => {
            let name = crate::named_fragment(&name, Some(signed))?;
            let member = match npub::is_identity(&member) {
                true => member,
                false => ask_registry(env, &calls::FindUsername { username: member }).await?.identity.id,
            };
            let (method, body) = match role {
                Some(role) => (Method::Put, Some(json!({ "role": role }))),
                None => (Method::Delete, None),
            };
            on_fragment(env, cfg, signed, &name, method, &format!("members/{member}"), body, false).await
        }
        Verb::Fragment { name, method, rest, mut body, file } => {
            let name = crate::named_fragment(&name, Some(signed))?;
            if let Some(b) = body.as_mut().filter(|b| rest.starts_with("ops/") && b["id"].is_null()) {
                b["id"] = json!(fresh_id());
            }
            on_fragment(env, cfg, signed, &name, Method::from(method.to_string()), &rest, body, file).await
        }
    }
}

/// A fragment's route as the API has it, `/api/f/{name}/{rest}`: its
/// status and its answer (a file's as `{path, text}`, at most
/// `limits::RESULT_MAX_BYTES` of UTF-8).
#[allow(clippy::too_many_arguments)]
async fn on_fragment(env: &Env, cfg: &Config, signed: &Signed, name: &str, method: Method, rest: &str, body: Option<Value>, file: bool) -> CellResult<(u16, Value)> {
    let at = Url::parse(&format!("{}/api/f/{name}/{rest}", cfg.platform())).map_err(|e| CellError::host(format!("a route's URL: {e}")))?;
    let inner = format!("/api/{}", rest.split('?').next().unwrap_or_default());
    let (status, mut resp) = ask(env, name, signed, &at, method, inner, body).await?;
    if !(file && status == 200) {
        return Ok((status, json_of(&mut resp).await));
    }
    let (bytes, cut) = crate::cs::read_answer(&mut resp, limits::RESULT_MAX_BYTES).await?;
    let path = at.query_pairs().find(|(k, _)| k == "path").map(|(_, v)| v.into_owned()).unwrap_or_default();
    let refused = |why: String| (400, json!({ "error": ErrorCode::InvalidRequest, "message": why }));
    Ok(match (cut, String::from_utf8(bytes)) {
        (true, _) => refused(format!("{path} is over {} bytes: read it with the CLI", limits::RESULT_MAX_BYTES)),
        (false, Ok(text)) => (200, json!({ "path": path, "text": text })),
        (false, Err(_)) => refused(format!("{path} is not text: read it with the CLI")),
    })
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

/// A refusal as the API answers it: its code and why.
fn refusal(status: u16, v: &Value) -> (ErrorCode, String) {
    match serde_json::from_value::<ErrorBody>(v.clone()) {
        Ok(e) => (e.error, e.message),
        Err(_) => (ErrorCode::HostFailed, format!("the fragment answered {status}")),
    }
}
