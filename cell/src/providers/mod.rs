//! A person's own models on the model route (docs/optchat.md, "Your own
//! models"; `fragment_core::providers`, whose translations these calls
//! send and read).
//!
//! - **Where they live.** A person's choices, their own keys (Anthropic's,
//!   OpenAI's: the catalog's `own` rows, `PUT /api/connections/{provider}/key`)
//!   and their Sign in with ChatGPT tokens are kept by their computer,
//!   sealed for it (computer/own_models.rs). A call asks it for the
//!   payer's choice for its role, and gets the credential with it: the cell
//!   holds a credential only for the call it makes, and no client is ever
//!   given one.
//! - **Who chooses.** Every call its person pays for that they or their
//!   agents make: a job's text step (ai.rs, `step::chosen_for_step`, its role
//!   the one it names, else its tier's) and an agent's model route call
//!   (models.rs, `hands`). Anyone else's spending in their fragments runs as
//!   it names, under its cap.
//! - **The call.** Translated (`fragment_core::providers::request`), sent
//!   to the vendor streamed, its answer read back as OpenAI's stream. An own
//!   provider's call reserves nothing and is charged nothing: what it used
//!   is counted by the payer's computer (`computer/model-used`), by month,
//!   provider, model and role, as an own key's swapped calls are counted.
//! - **The picker** (`route`): `GET /api/models`, each provider's state and
//!   models (asked of the vendor with the person's credential), Fragment's,
//!   the choices and this month's counts; `PUT /api/models/choices`. And
//!   `PUT`/`DELETE /api/connections/chatgpt/tokens` (`chatgpt`): the tokens
//!   `fragment connect chatgpt` got (Sign in with ChatGPT's loopback
//!   redirect is the CLI's), handed to the person's computer.

pub(crate) mod step;

use std::pin::Pin;

use fragment_core::catalog::Kind;
use fragment_core::models::{self as bounds, Stream};
use fragment_core::providers::{self as own, Choice, Chunks, Counted, Failure, Listed, Role, Target, Translate, Vendor};
use fragment_proto::{ErrorCode, IdentityKind, Tier};
use futures_util::StreamExt;
use serde_json::{json, Value};
use worker::*;

use crate::config::Config;
use crate::error::{CellError, CellResult};
use crate::fragment::json_response;
use crate::models::{Background, ModelCall};

/// An own provider's answer, read whole (an unstreamed client's), or a
/// refusal's body: at most this.
const ANSWER_MAX_BYTES: usize = 8 * 1024 * 1024;
/// A provider's list of models, read whole: at most this.
const LIST_MAX_BYTES: usize = 1024 * 1024;

/// An own provider's call: the vendor, the model, and the credential its
/// person's computer gave for this call alone.
pub(crate) struct Own {
    pub vendor: Vendor,
    pub model: String,
    credential: String,
}

/// The model a call runs on: Fragment's tier, or an own provider's.
pub(crate) enum Chosen {
    Fragment(Tier),
    Own(Own),
}

/// The model a call of `payer`'s for `role`, naming `named`, runs on
/// (`fragment_core::providers::target`): their choice, asked of their
/// computer, which keeps it with their keys and gives an own provider's
/// credential with it. A person with no computer has chosen nothing; a
/// call `own_spend` is not (someone else's) runs as it names.
pub(crate) async fn chosen(env: &Env, payer: &str, role: Role, named: Tier, own_spend: bool) -> CellResult<Chosen> {
    if !own_spend {
        return Ok(Chosen::Fragment(named));
    }
    let computer = fragment_core::computer::default_computer_of(payer);
    let answer = match crate::computer::ask(env, &computer, "computer/model-call", &json!({ "role": role })).await {
        Ok(v) => v,
        Err(e) if e.code == ErrorCode::NotFound => return Ok(Chosen::Fragment(named)),
        Err(e) => return Err(e),
    };
    let choice: Option<Choice> = serde_json::from_value(answer["choice"].clone()).map_err(|e| CellError::host(format!("the computer's choice: {e}")))?;
    match own::target(choice.as_ref(), named, true) {
        Target::Fragment(tier) => Ok(Chosen::Fragment(tier)),
        Target::Own { vendor, model } => {
            let credential = answer["credential"].as_str().filter(|c| !c.is_empty()).ok_or_else(|| CellError::host("the computer gave no credential for its choice"))?;
            Ok(Chosen::Own(Own { vendor, model, credential: credential.to_string() }))
        }
    }
}

/// A request to a vendor's `url` (`https://<host><path>`), or, in dev and
/// the e2e, to the models fake (`FRAGMENT_MODELS_UPSTREAM`), its host in
/// `x-fragment-upstream-host`. A redirect is never followed.
pub(crate) async fn vendor_fetch(cfg: &Config, method: Method, url: &str, headers: &[(&str, &str)], body: Option<Vec<u8>>) -> CellResult<Response> {
    let parsed = Url::parse(url).map_err(|e| CellError::host(format!("a vendor's URL: {e}")))?;
    let host = parsed.host_str().ok_or_else(|| CellError::host("a vendor's URL names its host"))?.to_string();
    let h = Headers::new();
    for (k, v) in headers {
        h.set(k, v)?;
    }
    let target = match &cfg.models_upstream {
        Some(upstream) => {
            h.set("x-fragment-upstream-host", &host)?;
            format!("{upstream}{}{}", parsed.path(), parsed.query().map(|q| format!("?{q}")).unwrap_or_default())
        }
        None => url.to_string(),
    };
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(h).with_redirect(RequestRedirect::Manual);
    if let Some(b) = body {
        init.with_body(Some(js_sys::Uint8Array::from(b.as_slice()).into()));
    }
    Fetch::Request(Request::new_with_init(&target, &init)?).send().await.map_err(|e| CellError::new(ErrorCode::UpstreamFailed, format!("{host} did not answer: {e}")))
}

/// The headers that carry `credential` to `vendor`.
fn auth(vendor: Vendor, credential: &str) -> Vec<(&'static str, String)> {
    match vendor {
        Vendor::Anthropic => vec![("x-api-key", credential.to_string()), ("anthropic-version", own::anthropic::VERSION.to_string())],
        _ => vec![("authorization", format!("Bearer {credential}"))],
    }
}

/// An answer's body, at most `max` (read no further), and whether it was cut.
async fn read_some(resp: &mut Response, max: usize) -> Vec<u8> {
    crate::cs::read_answer(resp, max).await.map(|(b, _)| b).unwrap_or_default()
}

/// Sends `own`'s call, translated: the vendor's streamed answer, or its
/// refusal (status and failure).
pub(crate) async fn open(env: &Env, own: &Own, input: &Value) -> CellResult<std::result::Result<Response, (u16, Failure)>> {
    let request = own::request(own.vendor, input, &own.model).map_err(|u| CellError::invalid(u.message()))?;
    let host = own.vendor.host().expect("an own vendor has a host");
    let url = format!("https://{host}{}", own::path(own.vendor));
    let creds = auth(own.vendor, &own.credential);
    let mut headers: Vec<(&str, &str)> = creds.iter().map(|(k, v)| (*k, v.as_str())).collect();
    headers.push(("content-type", "application/json"));
    headers.push(("accept", "text/event-stream"));
    let mut resp = vendor_fetch(Config::from_env(env), Method::Post, &url, &headers, Some(request.to_string().into_bytes())).await?;
    let status = resp.status_code();
    if status != 200 {
        let body = read_some(&mut resp, ANSWER_MAX_BYTES).await;
        return Ok(Err((status, own::failure_of(own.vendor, status, &body))));
    }
    Ok(Ok(resp))
}

/// Counts what an own provider's call used, on its payer's computer: never
/// charged. A count that does not land is logged, never retried.
pub(crate) async fn record(env: &Env, payer: &str, own: &Own, role: Role, counted: Option<Counted>) {
    let computer = fragment_core::computer::default_computer_of(payer);
    let body = json!({ "provider": own.vendor, "model": own.model, "role": role, "counted": counted.unwrap_or_default() });
    match crate::computer::ask(env, &computer, "computer/model-used", &body).await {
        Ok(_) => console_log!("{}", json!({ "event": "model.own", "provider": own.vendor, "model": own.model, "role": role, "counted": counted })),
        Err(e) => console_error!("{}", json!({ "event": "model.own-uncounted", "provider": own.vendor, "message": e.message })),
    }
}

/// A vendor's refusal as a route's client reads one: OpenAI's error, under
/// the vendor's status.
fn refusal(status: u16, f: &Failure) -> CellResult<Response> {
    let mut resp = Response::from_json(&json!({ "error": { "message": f.message, "type": f.kind, "code": f.kind } }))?.with_status(status);
    resp.headers_mut().set("content-type", "application/json")?;
    Ok(resp)
}

/// An agent's model route call on its owner's own provider (models.rs
/// `complete`, the module's doc): translated, sent streamed, and answered
/// in OpenAI's shape, streamed or whole as the client asked; what it used
/// is counted on the owner's computer once it ends, whether or not the
/// client read to its end.
pub(crate) async fn complete(env: &Env, call: ModelCall<'_>, own: Own, after: &dyn Background) -> CellResult<Response> {
    let bounded = bounds::bound_hinted(own::OWN, call.body, true).map_err(|why| CellError::invalid(why.message()))?;
    let payer = call.payer.to_string();
    let mut resp = match open(env, &own, &bounded.input).await? {
        Ok(r) => r,
        Err((status, f)) => return refusal(status, &f),
    };
    if !call.stream {
        let (bytes, cut) = crate::cs::read_answer(&mut resp, ANSWER_MAX_BYTES).await?;
        if cut {
            return Err(CellError::new(ErrorCode::UpstreamFailed, format!("the model's answer is over {ANSWER_MAX_BYTES} bytes")));
        }
        let mut t = own::translator(own.vendor, &own.model);
        let mut out = vec![];
        t.push(&bytes, &mut out);
        t.finish(&mut out);
        if let Some(f) = t.failure() {
            return refusal(if f.passing { 503 } else { 502 }, f);
        }
        let mut folded = Stream::answering();
        folded.push(&out, None);
        folded.finish(None);
        let answer = folded.answer().cloned().expect("an answering stream keeps its answer");
        if answer.finish_reason.is_none() {
            return Err(CellError::new(ErrorCode::UpstreamFailed, "the model's stream ended before its answer did"));
        }
        record(env, &payer, &own, Role::Hands, t.counted()).await;
        let chunks = Chunks { id: format!("own-{}", crate::js::random_hex::<8>()), model: own.model.clone() };
        return Ok(Response::from_json(&own::completion(&chunks, &answer, t.thinking_blocks(), t.counted().as_ref()))?);
    }
    let ResponseBody::Stream(source) = resp.body() else {
        return Err(CellError::new(ErrorCode::UpstreamFailed, "the model's stream had no body"));
    };
    // one branch for the client, one read to its end for the count
    let (client, meter) = crate::js::tee(source)?;
    let env2 = env.clone();
    let client_t = own::translator(own.vendor, &own.model);
    after.later(Box::pin(async move {
        let mut t = own::translator(own.vendor, &own.model);
        let mut bytes = ByteStream::from(meter);
        let mut sink = vec![];
        // bounded by the model's answer: at most the cap's tokens
        while let Some(Ok(chunk)) = bytes.next().await {
            t.push(&chunk, &mut sink);
            sink.clear();
        }
        t.finish(&mut sink);
        record(&env2, &payer, &own, Role::Hands, t.counted()).await;
    }));
    let translated = Translated { source: Box::pin(ByteStream::from(client)), t: client_t, done: false };
    let mut resp = Response::from_stream(translated)?;
    resp.headers_mut().set("content-type", "text/event-stream")?;
    resp.headers_mut().set("cache-control", "no-cache")?;
    Ok(resp)
}

/// The client's branch of an own provider's stream, read as OpenAI's.
struct Translated {
    source: Pin<Box<ByteStream>>,
    t: Box<dyn Translate>,
    done: bool,
}

impl futures_util::Stream for Translated {
    type Item = Result<Vec<u8>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        let this = &mut *self;
        // bounded by the source: each pass takes one chunk of it
        loop {
            if this.done {
                return Poll::Ready(None);
            }
            match futures_util::ready!(this.source.as_mut().poll_next(cx)) {
                Some(Ok(chunk)) => {
                    let mut out = vec![];
                    this.t.push(&chunk, &mut out);
                    // the answer is whole at its end: stop there
                    this.done = this.t.done() || this.t.failure().is_some();
                    if !out.is_empty() {
                        return Poll::Ready(Some(Ok(out)));
                    }
                }
                Some(Err(e)) => {
                    this.done = true;
                    return Poll::Ready(Some(Err(e)));
                }
                None => {
                    this.done = true;
                    let mut out = vec![];
                    this.t.finish(&mut out);
                    return Poll::Ready((!out.is_empty()).then_some(Ok(out)));
                }
            }
        }
    }
}

// ---- the picker: `GET /api/models`, `PUT /api/models/choices` ----

/// Fragment's own models a role may run on: the tiers, Flash the default
/// (Paul, 2026-10-08).
fn fragment_models() -> Value {
    json!([
        { "id": Tier::Cheap.as_str(), "name": "GLM-5.3 Flash", "default": true },
        { "id": Tier::Medium.as_str(), "name": "GLM-5.3", "default": false },
    ])
}

/// Whether the deployment offers `vendor`: its catalog has the `own` row
/// whose key it takes, or whose host it calls (Sign in with ChatGPT: OpenAI's).
fn offered(cfg: &Config, vendor: Vendor) -> bool {
    vendor.catalog_row().is_some_and(|row| cfg.providers.get(row).is_some_and(|p| p.kind == Kind::Own))
}

/// A vendor's models, asked with the person's credential.
async fn listed(cfg: &Config, vendor: Vendor, credential: &str) -> CellResult<Vec<Listed>> {
    let host = vendor.host().expect("an own vendor has a host");
    let url = match vendor {
        Vendor::Anthropic => format!("https://{host}/v1/models?limit=100"),
        _ => format!("https://{host}/v1/models"),
    };
    let creds = auth(vendor, credential);
    let mut headers: Vec<(&str, &str)> = creds.iter().map(|(k, v)| (*k, v.as_str())).collect();
    headers.push(("accept", "application/json"));
    let mut resp = vendor_fetch(cfg, Method::Get, &url, &headers, None).await?;
    let status = resp.status_code();
    let body = read_some(&mut resp, LIST_MAX_BYTES).await;
    if status != 200 {
        let f = own::failure_of(vendor, status, &body);
        return Err(CellError::new(ErrorCode::UpstreamFailed, format!("{host} answered {status}: {}", f.message)));
    }
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Ok(match vendor {
        Vendor::Anthropic => own::anthropic_models(&v),
        _ => own::openai_models(&v),
    })
}

/// What the person's computer keeps of their models (`computer/models`),
/// or nothing when they have no computer yet.
async fn kept(env: &Env, who: &str) -> CellResult<Option<Value>> {
    let computer = fragment_core::computer::default_computer_of(who);
    match crate::computer::ask(env, &computer, "computer/models", &json!({})).await {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.code == ErrorCode::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// A provider's state for the person: an own key's `set` or `not_set`,
/// ChatGPT's `connected`, `needs_reauthorization` or `not_connected`.
fn state_of(kept: Option<&Value>, vendor: Vendor) -> String {
    let Some(k) = kept else {
        return if vendor == Vendor::Chatgpt { "not_connected".into() } else { "not_set".into() };
    };
    match vendor {
        Vendor::Chatgpt => k["chatgpt"]["state"].as_str().unwrap_or("not_connected").to_string(),
        v => {
            let row = v.catalog_row().unwrap_or_default();
            if k["own"].as_array().is_some_and(|o| o.iter().any(|p| p == row)) {
                "set".into()
            } else {
                "not_set".into()
            }
        }
    }
}

fn usable(state: &str) -> bool {
    matches!(state, "set" | "connected")
}

/// `GET /api/models`: the picker (the module's doc).
async fn view(env: &Env, who: &str) -> CellResult<Value> {
    let cfg = Config::from_env(env);
    let kept = kept(env, who).await?;
    let computer = fragment_core::computer::default_computer_of(who);
    let mut providers = vec![];
    for vendor in Vendor::OWN {
        if !offered(cfg, vendor) {
            continue;
        }
        let state = state_of(kept.as_ref(), vendor);
        let mut p = json!({ "provider": vendor, "state": state, "models": [], "suggested": {} });
        if vendor == Vendor::Chatgpt {
            let c = kept.as_ref().map(|k| k["chatgpt"].clone()).unwrap_or(Value::Null);
            for (from, to) in [("account", "account"), ("clientId", "clientId"), ("hostId", "hostId"), ("expiresAt", "expiresAt")] {
                p[to] = c[from].clone();
            }
        }
        if usable(&state) {
            let credential = crate::computer::ask(env, &computer, "computer/model-credential", &json!({ "provider": vendor })).await;
            match credential {
                Ok(c) => match listed(cfg, vendor, c["credential"].as_str().unwrap_or_default()).await {
                    Ok(models) => {
                        p["suggested"] = json!(own::suggested(vendor, &models));
                        p["models"] = json!(models);
                    }
                    Err(e) => p["error"] = json!(e.message),
                },
                Err(e) => {
                    p["error"] = json!(e.message);
                    if e.code == ErrorCode::NotConnected && vendor == Vendor::Chatgpt {
                        p["state"] = json!("needs_reauthorization");
                    }
                }
            }
        }
        providers.push(p);
    }
    Ok(json!({
        "roles": kept.as_ref().map(|k| k["choices"].clone()).unwrap_or_else(|| json!({})),
        "fragment": fragment_models(),
        "providers": providers,
        "uses": kept.as_ref().map(|k| k["uses"].clone()).unwrap_or(Value::Null),
        "computer": kept.is_some(),
    }))
}

/// `PUT /api/models/choices`: each role named, checked (a model in shape, a
/// provider the deployment offers and the person connected), kept by
/// their computer. Answers the picker.
async fn choose(env: &Env, who: &str, body: &[u8]) -> CellResult<Value> {
    let cfg = Config::from_env(env);
    let b: own::RoleChoices = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e} (chat, memory or hands, each {{provider, model}} or null)")))?;
    let named = b.named();
    if named.is_empty() {
        return Err(CellError::invalid("name a role's choice: chat, memory or hands"));
    }
    let kept = kept(env, who).await?.ok_or_else(|| CellError::new(ErrorCode::NotFound, "your models are kept by your computer: make it first (POST /api/computers)"))?;
    for (_, choice) in &named {
        let Some(c) = choice else { continue };
        c.check().map_err(CellError::invalid)?;
        if c.provider != Vendor::Fragment {
            if !offered(cfg, c.provider) {
                return Err(CellError::invalid(format!("this deployment does not offer {}", c.provider.as_str())));
            }
            if !usable(&state_of(Some(&kept), c.provider)) {
                return Err(CellError::new(ErrorCode::NotConnected, format!("connect {} first (Settings, Models)", c.provider.as_str())));
            }
        }
    }
    let computer = fragment_core::computer::default_computer_of(who);
    for (role, choice) in named {
        let choice = choice.filter(|c| !c.is_default());
        crate::computer::ask(env, &computer, "computer/model-choice", &json!({ "role": role, "choice": choice })).await?;
    }
    view(env, who).await
}

pub(crate) async fn route(env: &Env, who: &str, kind: IdentityKind, method: Method, rest: &[&str], body: &[u8]) -> CellResult<Response> {
    if kind != IdentityKind::Person {
        return Err(CellError::new(ErrorCode::Forbidden, "a person chooses their models: their agents use them"));
    }
    match (method, rest) {
        (Method::Get, []) => json_response(&view(env, who).await?),
        (Method::Put, ["choices"]) => json_response(&choose(env, who, body).await?),
        (m, _) => Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/models/{}", m.as_ref(), rest.join("/")))),
    }
}

// ---- Sign in with ChatGPT's tokens: `PUT`/`DELETE /api/connections/chatgpt/tokens` ----

/// `PUT /api/connections/chatgpt/tokens` (its tokens, kept sealed by the
/// person's computer) and `DELETE` (revoked at OpenAI, and forgotten).
pub(crate) async fn chatgpt(env: &Env, who: &str, method: Method, body: &[u8]) -> CellResult<Response> {
    if !offered(Config::from_env(env), Vendor::Chatgpt) {
        return Err(CellError::new(ErrorCode::NotFound, "this deployment does not offer OpenAI (its catalog has no openai row)"));
    }
    let tokens = match method {
        Method::Put => {
            let t: own::ChatgptTokens = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("body: {e}")))?;
            own::tokens_check(&t).map_err(CellError::invalid)?;
            Some(t)
        }
        Method::Delete => None,
        m => return Err(CellError::new(ErrorCode::NotFound, format!("no route {} /api/connections/chatgpt/tokens", m.as_ref()))),
    };
    let computer = fragment_core::computer::default_computer_of(who);
    match crate::computer::ask(env, &computer, "computer/chatgpt", &json!({ "tokens": tokens })).await {
        Ok(v) => json_response(&v),
        Err(e) if e.code == ErrorCode::NotFound => Err(CellError::new(ErrorCode::NotFound, "your sign-ins are kept by your computer: make it first (POST /api/computers)")),
        Err(e) => Err(e),
    }
}
