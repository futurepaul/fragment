//! A person's own models (docs/optchat.md, "Your own models"), on the
//! fakes: the model vendors' (crates/fakes `vendors`: Anthropic's Messages
//! API, OpenAI's Responses API and its sign-in) behind
//! `FRAGMENT_MODELS_UPSTREAM`. Each part runs in the section of what it
//! touches:
//!
//! - `ai`: the picker (Fragment's models, the providers offered, a choice
//!   refused until its provider is connected); a Claude key kept by the
//!   computer, never given back, its models listed; a job's tool turn on
//!   Claude for chat (no thinking at low effort, its prefix read from the
//!   cache) and a step on Claude for memory, counted on the computer and
//!   charged nothing; someone else's run in the person's fragment on
//!   Fragment's model, as it names; Sign in with ChatGPT's tokens handed
//!   over, refreshed at their first call, used under the preview's rules,
//!   and revoked.
//! - `mind`: a mind on Claude: its turns drafted, a tool call run, the
//!   view in 4-line blocks with its cache marks, the compactor on Haiku,
//!   and an agent's model route call (goose's) on the person's hands.
//! - `cli`: `fragment connect chatgpt` signs in through the fake (a
//!   loopback, PKCE, dynamic registration; then the client reused) and
//!   `--forget` signs out.
//! - `ui`: settings' Models: the pickers, a choice made in the page kept.

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine;
use fragment_fakes::vendors::{ANTHROPIC_MODELS, CHATGPT_EMAIL};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::app::ship;
use super::jobs::{settle, started};
use super::ledger::entries;
use crate::api::{Api, Socket};
use crate::browser::{Browser, Page};
use crate::{Keys, Suite};

const MEDIA_APP: &[u8] = include_bytes!("../../fixtures/media.mjs");
const MEDIA_JSON: &[u8] = include_bytes!("../../fixtures/media.json");
/// A turn on the fakes, its steps one Workflow step each: well under this.
const TURN: Duration = Duration::from_secs(60);
const SONNET: &str = "claude-sonnet-5-5";
const HAIKU: &str = "claude-haiku-5-5";

fn choice(provider: &str, model: &str) -> Value {
    json!({ "provider": provider, "model": model })
}

fn provider<'a>(models: &'a Value, p: &str) -> &'a Value {
    models["providers"].as_array().and_then(|l| l.iter().find(|x| x["provider"] == p)).unwrap_or(&Value::Null)
}

/// The person's computer's counts of their own models' calls this month.
fn model_uses(api: &Api, keys: &Keys, computer: &str) -> Vec<Value> {
    api.signed(keys, "GET", &format!("/api/computers/{computer}/uses"), None).ok().and_then(|r| r.body["models"].as_array().cloned()).unwrap_or_default()
}

fn uses_of(uses: &[Value], provider: &str, model: &str, role: &str) -> Value {
    uses.iter().find(|u| u["provider"] == provider && u["model"] == model && u["role"] == role).cloned().unwrap_or(Value::Null)
}

/// A person with a computer (its record: no container is started), and its id.
fn person_with_computer(api: &Api) -> Result<(Keys, String, String)> {
    let keys = api.person()?;
    let identity = api.identity(&keys)?;
    let r = api.signed(&keys, "POST", "/api/computers", Some(&json!({})))?;
    anyhow::ensure!(r.status == 200, "making a computer: {r}");
    Ok((keys, identity, r.body["computer"].as_str().unwrap_or("").to_string()))
}

/// The vendors' calls since `from` with this credential, on this path.
fn calls_since(s: &Suite, from: usize, credential: &str, path: &str) -> Vec<fragment_fakes::vendors::VendorCall> {
    s.vendors.calls().into_iter().skip(from).filter(|c| c.path == path && c.credential.as_deref() == Some(credential)).collect()
}

/// The cache marks a Messages request carries.
fn marks(body: &Value) -> usize {
    body.to_string().matches("\"cache_control\"").count()
}

/// The section `ai`'s part (the module's doc).
pub(super) fn ai(s: &mut Suite, api: &Api) -> Result<()> {
    let wait = Duration::from_secs(40);
    let (owner, owner_id, computer) = person_with_computer(api)?;
    let models = |api: &Api| api.signed(&owner, "GET", "/api/models", None).map(|r| r.body).unwrap_or(Value::Null);
    let m = models(api);
    let offered: Vec<&str> = m["providers"].as_array().into_iter().flatten().filter_map(|p| p["provider"].as_str()).collect();
    s.ok(
        "a person's models: Fragment's (GLM-5.3 Flash the default, GLM-5.3), and the providers the deployment offers, none connected, nothing chosen",
        m["fragment"][0]["id"] == "cheap" && m["fragment"][0]["default"] == true && m["fragment"][1]["id"] == "medium"
            && offered == ["anthropic", "openai", "chatgpt"]
            && provider(&m, "anthropic")["state"] == "not_set" && provider(&m, "chatgpt")["state"] == "not_connected"
            && m["roles"] == json!({}) && m["computer"] == true,
        &m,
    );
    let r = api.signed(&owner, "PUT", "/api/models/choices", Some(&json!({ "chat": choice("anthropic", SONNET) })))?;
    s.ok("a provider not connected is no choice: refused, saying to connect it", r.status == 403 && r.message().contains("connect anthropic first"), &r);

    // Claude: a key, kept by the computer and never given back
    let key = "sk-ant-e2e-own-key";
    let from = s.vendors.calls().len();
    let r = api.signed(&owner, "PUT", "/api/connections/anthropic/key", Some(&json!({ "key": key })))?;
    let m = models(api);
    let ids: Vec<&str> = provider(&m, "anthropic")["models"].as_array().into_iter().flatten().filter_map(|x| x["id"].as_str()).collect();
    let asked = calls_since(s, from, key, "/v1/models");
    s.ok(
        "a Claude key, kept by the person's computer, lists Anthropic's models (asked of Anthropic with it), suggesting Sonnet for chat and hands and Haiku for memory; no answer carries it",
        r.status == 200 && provider(&m, "anthropic")["state"] == "set" && ids == ANTHROPIC_MODELS.map(|(id, _)| id) && asked.len() == 1
            && provider(&m, "anthropic")["suggested"] == json!({ "chat": SONNET, "memory": HAIKU, "hands": SONNET })
            && !m.to_string().contains(key) && !r.text.contains(key),
        &m,
    );
    let r = api.signed(&owner, "PUT", "/api/models/choices", Some(&json!({ "chat": choice("anthropic", SONNET), "memory": choice("anthropic", HAIKU) })))?;
    let again = models(api);
    s.ok(
        "the person picks Claude for chat (Sonnet) and memory (Haiku): kept, read back as chosen",
        r.status == 200 && again["roles"] == json!({ "chat": choice("anthropic", SONNET), "memory": choice("anthropic", HAIKU) }),
        &again,
    );
    let bad = api.signed(&owner, "PUT", "/api/models/choices", Some(&json!({ "chat": choice("fragment", "high") })))?;
    let none = api.signed(&owner, "PUT", "/api/models/choices", Some(&json!({})))?;
    s.ok("a choice out of shape, or none named, is refused", bad.status == 400 && none.status == 400, json!([bad.text, none.text]));

    // a job's tool turn on Claude, for chat
    let name = s.named(api, &owner, "own-ai")?;
    let c = s.create(api, &owner, &name)?;
    ship(s, &c, MEDIA_APP, MEDIA_JSON);
    let run = |keys: &Keys, id: &str, op: &str, input: Value| -> Result<Value> {
        let r = api.op(keys, &name, op, id, input)?;
        Ok(settle(api, keys, &name, started(&r), &["succeeded", "held"], wait))
    };
    let from = s.vendors.calls().len();
    let r = run(&owner, "own-1", "tool_turn", json!({ "ask": "what is a shard? [[call lookup {\"word\":\"shard\"}]]", "role": "chat" }))?;
    let (first, second) = (&r["output"]["first"], &r["output"]["second"]);
    s.ok(
        "a job's tool turn naming chat runs on the person's Claude: it calls the tool, and answers from its result, its timing one call's on that model (first data line, whole answer)",
        r["status"] == "succeeded"
            && first["provider"] == "anthropic" && first["model"] == SONNET
            && first["message"]["tool_calls"][0]["function"]["name"] == "lookup"
            && first["finish_reason"] == "tool_calls"
            && second["text"] == "the tool said: shard: a small piece broken off"
            && first["timing"]["calls"] == 1 && first["timing"]["provider"] == "anthropic" && first["timing"]["model"] == SONNET && first["timing"]["first_ms"].is_i64() && first["timing"]["ms"].is_i64(),
        &r,
    );
    let sent = calls_since(s, from, key, "/v1/messages");
    let next = sent.get(1).map(|c| c.body.clone()).unwrap_or(Value::Null);
    s.ok(
        "Anthropic is sent its own shape: the tool as input_schema, the turn's end marked for the cache, and at low effort no thinking (`thinking: disabled`, so none to carry back: a model's default thinking spent a compaction's budget on the preview)",
        sent.len() == 2
            && sent[0].body["tools"][0]["input_schema"]["required"] == json!(["word"])
            && sent[0].body["tool_choice"] == json!({ "type": "auto" })
            && marks(&sent[0].body) == 1
            && sent.iter().all(|c| c.body["thinking"] == json!({ "type": "disabled" }))
            && first["message"].get("thinking_blocks").is_none_or(|b| b.as_array().is_none_or(|a| a.is_empty()))
            && next["messages"][1]["content"][0]["type"] == "tool_use"
            && next["messages"][2]["content"][0]["type"] == "tool_result",
        json!(sent.iter().map(|c| c.body.clone()).collect::<Vec<_>>()),
    );
    let cached = second["usage"]["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
    let counted = s.eventually(wait, || uses_of(&model_uses(api, &owner, &computer), "anthropic", SONNET, "chat")["calls"] == 2);
    let used = uses_of(&model_uses(api, &owner, &computer), "anthropic", SONNET, "chat");
    let charged = entries(api, &owner_id, &format!("step:{name}@"));
    s.ok(
        "the second call reads the first's prefix from the cache; both are counted on the person's computer (calls, tokens, cached tokens) and charged nothing",
        cached > 0 && counted && used["cached"].as_u64().unwrap_or(0) >= cached && used["output"].as_u64().unwrap_or(0) > 0 && charged.is_empty(),
        json!({ "used": used, "cached": cached, "ledger": charged }),
    );
    let r = run(&owner, "own-2", "ask_text", json!({ "model": "cheap", "prompt": "sum it up" }))?;
    s.ok(
        "a step naming no role runs on its tier's: cheap is memory, the person's Haiku",
        r["status"] == "succeeded" && r["output"]["model"] == HAIKU && r["output"]["text"] == "echo: sum it up",
        &r,
    );
    let r = run(&owner, "own-3", "ask_text", json!({ "prompt": "x", "role": "hands" }))?;
    s.ok("a step's role is chat or memory (hands is the agents'), refused for good otherwise", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("chat or memory")), &r);
    // someone else's run in the person's fragment: as it names, on the person's credit
    let editor = api.person()?;
    api.signed(&owner, "PUT", &format!("/api/f/{name}/members/{}", editor.pubkey_hex()), Some(&json!({ "role": "editor" })))?;
    let before = s.vendors.calls().len();
    let r = run(&editor, "own-4", "ask_text", json!({ "prompt": "from someone else", "role": "chat" }))?;
    s.ok(
        "someone else's step in the person's fragment runs on Fragment's model, as it names, never the person's own (charged to the person, under the fragment's cap)",
        r["status"] == "succeeded" && r["output"]["model"] == "@cf/zai-org/glm-5.3-flash" && s.vendors.calls().len() == before && !entries(api, &owner_id, &format!("step:{name}@")).is_empty(),
        &r,
    );

    // ChatGPT: tokens from its sign-in (done here by hand; the cli section runs the CLI's)
    s.vendors.set_expires_in(60);
    let host = models(api)["providers"].as_array().and_then(|l| l.iter().find(|p| p["provider"] == "chatgpt")).map(|p| p["hostId"].as_str().unwrap_or("").to_string()).unwrap_or_default();
    let tokens = sign_in_by_hand(&s.vendors.url, &host)?;
    let r = api.signed(&owner, "PUT", "/api/connections/chatgpt/tokens", Some(&tokens))?;
    let m = models(api);
    let refreshed = s.vendors.refreshes();
    s.ok(
        "Sign in with ChatGPT's tokens, handed to the platform, are kept (connected as the account), and its models listed (refreshed first: the access token had a minute left)",
        r.status == 200 && provider(&m, "chatgpt")["state"] == "connected" && provider(&m, "chatgpt")["account"] == CHATGPT_EMAIL
            && provider(&m, "chatgpt")["models"].as_array().is_some_and(|l| l.len() == 2) && refreshed >= 1 && !m.to_string().contains("chatgpt-at-"),
        &m,
    );
    let r = api.signed(&owner, "PUT", "/api/connections/chatgpt/tokens", Some(&json!({ "client_id": "oaiapp_x", "access_token": "a", "refresh_token": "r", "expires_in": 3600, "scope": "openid" })))?;
    s.ok("tokens that do not grant plan usage are refused", r.status == 400 && r.message().contains("chatgpt.tokens.use.direct"), &r);
    api.signed(&owner, "PUT", "/api/models/choices", Some(&json!({ "chat": choice("chatgpt", "gpt-6.1-sol") })))?;
    let from = s.vendors.calls().len();
    let r = run(&owner, "own-5", "tool_turn", json!({ "ask": "look [[call lookup {\"word\":\"tide\"}]]", "role": "chat" }))?;
    let sent: Vec<Value> = s.vendors.calls().into_iter().skip(from).filter(|c| c.path == "/v1/responses").map(|c| c.body).collect();
    s.ok(
        "a tool turn on ChatGPT runs under the preview's rules (store off, streamed, the tools in a namespace, no output cap) and answers from the tool's result",
        r["status"] == "succeeded" && r["output"]["first"]["provider"] == "chatgpt" && r["output"]["second"]["text"] == "the tool said: tide: a small piece broken off"
            && sent.len() == 2 && sent[0]["store"] == false && sent[0]["tools"][0]["type"] == "namespace" && sent[0].get("max_output_tokens").is_none()
            && sent[1]["input"].as_array().is_some_and(|i| i.iter().any(|x| x["type"] == "function_call_output")),
        json!({ "run": r, "sent": sent }),
    );
    let r = api.signed(&owner, "DELETE", "/api/connections/chatgpt/tokens", None)?;
    let m = models(api);
    s.ok(
        "signing out revokes the refresh token at OpenAI and forgets the tokens: a choice of ChatGPT then fails its step, saying to sign in again",
        r.status == 200 && r.body["revoked"] == true && !s.vendors.revoked().is_empty() && provider(&m, "chatgpt")["state"] == "not_connected",
        json!({ "delete": r.body, "models": m }),
    );
    let r = run(&owner, "own-6", "ask_text", json!({ "prompt": "hello", "role": "chat" }))?;
    s.ok("(that step is held, saying why)", r["status"] == "held" && r["error"].as_str().is_some_and(|e| e.contains("fragment connect chatgpt")), &r);
    Ok(())
}

/// Sign in with ChatGPT through the fake, as the CLI does it but by hand:
/// the authorize request (its redirect read, not followed), the code
/// exchanged; the tokens as the platform takes them.
fn sign_in_by_hand(issuer: &str, host_id: &str) -> Result<Value> {
    let http = reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::none()).build()?;
    let verifier = "a-verifier-of-forty-three-characters-or-more-0123456789";
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let redirect = "http://127.0.0.1:9/auth/callback";
    let mut url = reqwest::Url::parse(&format!("{issuer}/api/accounts/authorize"))?;
    url.query_pairs_mut()
        .append_pair("client_id", "dynamic_agent_client")
        .append_pair("agent_name_hint", "Fragment")
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", redirect)
        .append_pair("scope", fragment_core::providers::CHATGPT_SCOPES)
        .append_pair("resource", fragment_core::providers::CHATGPT_RESOURCE)
        .append_pair("state", "s1")
        .append_pair("nonce", "n1")
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &challenge)
        .append_pair("ext_agent_host_id", host_id);
    let r = http.get(url).send()?;
    let location = r.headers().get("location").and_then(|l| l.to_str().ok()).context("the authorize answer redirects")?.to_string();
    let back = reqwest::Url::parse(&location)?;
    let get = |k: &str| back.query_pairs().find(|(n, _)| n == k).map(|(_, v)| v.into_owned()).unwrap_or_default();
    let (code, client) = (get("code"), get("client_id"));
    let form = [("grant_type", "authorization_code"), ("client_id", client.as_str()), ("code", code.as_str()), ("code_verifier", verifier), ("redirect_uri", redirect), ("resource", fragment_core::providers::CHATGPT_RESOURCE)];
    let t: Value = http.post(format!("{issuer}/api/accounts/oauth/token")).form(&form).send()?.json()?;
    Ok(json!({ "client_id": client, "access_token": t["access_token"], "refresh_token": t["refresh_token"], "expires_in": t["expires_in"], "scope": t["scope"], "email": CHATGPT_EMAIL }))
}

/// The mind's `log` records of one type.
fn logged(api: &Api, owner: &Keys, mind: &str, kind: &str) -> Vec<Value> {
    api.signed(owner, "GET", &format!("/api/f/{mind}/channels/log?after=0&limit=1000"), None)
        .ok()
        .and_then(|r| r.body["records"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r["body"]["type"] == kind)
        .map(|r| r["body"].clone())
        .collect()
}

/// The section `mind`'s part (the module's doc).
pub(super) fn mind(s: &mut Suite, api: &Api) -> Result<()> {
    let (owner, owner_id, computer) = person_with_computer(api)?;
    let key = "sk-ant-e2e-mind-key";
    let r = api.signed(&owner, "PUT", "/api/connections/anthropic/key", Some(&json!({ "key": key })))?;
    anyhow::ensure!(r.status == 200, "a Claude key: {r}");
    let r = api.signed(&owner, "PUT", "/api/models/choices", Some(&json!({ "chat": choice("anthropic", SONNET), "memory": choice("anthropic", HAIKU), "hands": choice("anthropic", SONNET) })))?;
    anyhow::ensure!(r.status == 200, "choosing Claude: {r}");
    let mind = s.named(api, &owner, "mind")?;
    let r = api.create_with(&owner, json!({ "name": mind, "template": "mind", "visibility": "members", "title": "Mind" }))?;
    anyhow::ensure!(r.status == 200, "making the mind: {r}");
    s.owned(&r.body, &owner);
    let installed = s.eventually(Duration::from_secs(30), || api.status(&owner, &mind).is_ok_and(|r| r.body["code"]["operations"]["heard"]["kind"] == "job"));
    anyhow::ensure!(installed, "the mind's code installs");
    let thread = "t_00000000000c1a0d";
    let say = |id: &str, text: String| api.signed(&owner, "POST", &format!("/api/f/{mind}/channels/say"), Some(&json!({ "id": id, "body": { "text": text, "thread": thread } })));
    let talks = || logged(api, &owner, &mind, "msg").into_iter().filter(|m| m["kind"] == "talk").count();
    let from = s.vendors.calls().len();
    // two messages first, the second long enough to be summarized (by the memory's model)
    say("c1", "hello claude, the garden has tomatoes".into())?;
    s.eventually(TURN, || talks() >= 1);
    say("c2", format!("and basil {}", "lorem ipsum dolor sit amet ".repeat(24)))?;
    s.eventually(TURN, || talks() >= 2);
    // the third calls a tool, watched live
    let mut page = Socket::open(api, &mind, "__live", Some(&owner), None)?;
    page.until("hello", 5)?;
    page.send(&json!({ "type": "subscribe", "channel": "log", "after": 0 }))?;
    // the two turns' records come first (from the start of log)
    page.until("subscribed", 500)?;
    page.patience(TURN)?;
    say("c3", "open the first message [[call zoom {\"id\":0,\"n\":1}]]".into())?;
    let mut drafts = vec![];
    let mut reply = Value::Null;
    let t0 = Instant::now();
    // bounded: each frame within the socket's patience, and the turn's time
    while t0.elapsed() < TURN {
        let Ok(frame) = page.next() else { break };
        if frame["type"] == "draft" && frame["turn"] == format!("turn:{thread}").as_str() {
            drafts.push(frame["text"].clone());
        }
        if frame["type"] == "record" && frame["body"]["type"] == "msg" && frame["body"]["kind"] == "talk" && frame["body"]["text"].as_str().is_some_and(|t| t.starts_with("the tool said: ")) {
            reply = frame["body"].clone();
            break;
        }
    }
    page.close();
    let said: Vec<Value> = logged(api, &owner, &mind, "msg").into_iter().filter(|m| m["thread"] == thread).collect();
    let kinds: Vec<&str> = said.iter().filter_map(|m| m["kind"].as_str()).collect();
    s.ok(
        "a mind's turn on Claude: its reply drafted on log under the thread's turn, the tool call logged as tool and echo, then the reply from its result",
        !drafts.is_empty() && reply["kind"] == "talk" && kinds.ends_with(&["user", "tool", "echo", "talk"]) && said.iter().any(|m| m["kind"] == "tool" && m["text"] == "zoom {\"id\":0,\"n\":1}"),
        json!({ "drafts": drafts, "said": said }),
    );
    let sent = calls_since(s, from, key, "/v1/messages");
    let turn = sent.iter().rev().find(|c| c.body["model"] == SONNET && c.body["messages"].as_array().is_some_and(|m| m.len() == 1) && c.body.to_string().contains("[[call zoom")).map(|c| c.body.clone()).unwrap_or(Value::Null);
    let blocks = turn["messages"][0]["content"].as_array().cloned().unwrap_or_default();
    let view: Vec<&Value> = blocks.iter().take_while(|b| !b["text"].as_str().unwrap_or("").starts_with("Now: ")).collect();
    s.ok(
        "its call is Anthropic's shape: the system prompt marked for the cache (the tools render before it), the view in blocks of 4 lines, its last whole block marked, the request's end marked, and no hint left",
        turn["system"].as_array().is_some_and(|s| s.last().is_some_and(|b| b.get("cache_control").is_some()))
            && view.len() >= 2 && view[0]["text"].as_str().is_some_and(|t| t.starts_with("<chat>\n") && t.matches('\n').count() == 4)
            && view.iter().rev().nth(1).is_some_and(|b| b.get("cache_control").is_some())
            && blocks.last().is_some_and(|b| b.get("cache_control").is_some())
            && marks(&turn) == 3 && !turn.to_string().contains("\"cache\":")
            && turn["tools"].as_array().is_some_and(|t| t.iter().any(|x| x["name"] == "zoom" && x["input_schema"].is_object())),
        &turn,
    );
    let compactions: Vec<&Value> = sent.iter().map(|c| &c.body).filter(|b| b["model"] == HAIKU).collect();
    s.ok(
        "the compactor's calls run on the person's model for memory (Haiku), the tools offered with tool_choice none",
        !compactions.is_empty() && compactions.iter().all(|b| b["tool_choice"] == json!({ "type": "none" }) && b["tools"].as_array().is_some_and(|t| !t.is_empty())),
        json!(compactions.len()),
    );
    let counted = s.eventually(TURN, || uses_of(&model_uses(api, &owner, &computer), "anthropic", SONNET, "chat")["calls"].as_u64().unwrap_or(0) >= 4);
    let uses = model_uses(api, &owner, &computer);
    let chat = uses_of(&uses, "anthropic", SONNET, "chat");
    s.ok(
        "the turns' calls are counted on the person's computer, the later ones read from the cache; the mind's steps charged nothing",
        counted && chat["cached"].as_u64().unwrap_or(0) > 0 && uses_of(&uses, "anthropic", HAIKU, "memory")["calls"].as_u64().unwrap_or(0) >= 1 && entries(api, &owner_id, &format!("step:{mind}@")).is_empty(),
        json!({ "uses": uses, "ledger": entries(api, &owner_id, &format!("step:{mind}@")) }),
    );

    // an agent's model route call (goose's, through its computer's intercept): the person's hands
    let hand = Keys::generate();
    let reg = "/api/identities";
    let r = api.signed(&owner, "POST", reg, Some(&json!({ "kind": "agent", "proof": api.proof(&hand, "POST", reg, &owner) })))?;
    anyhow::ensure!(r.status == 200, "an agent of the owner's: {r}");
    let route = "/api/models/v1/chat/completions";
    let from = s.vendors.calls().len();
    let r = api.signed(&hand, "POST", route, Some(&json!({ "model": "cheap", "stream": true, "messages": [{ "role": "user", "content": "hands, say hi" }] })))?;
    let lines: Vec<Value> = r.text.lines().filter_map(|l| l.strip_prefix("data: ")).filter(|d| *d != "[DONE]").filter_map(|d| serde_json::from_str(d).ok()).collect();
    let text: String = lines.iter().filter_map(|l| l["choices"][0]["delta"]["content"].as_str()).collect();
    let sent = calls_since(s, from, key, "/v1/messages");
    s.ok(
        "an agent's streamed call (goose's) runs on its owner's model for hands, answered as OpenAI's chunks with the usage last, then [DONE]",
        r.status == 200 && text == "echo: hands, say hi" && r.text.trim_end().ends_with("data: [DONE]")
            && lines.last().is_some_and(|l| l["choices"] == json!([]) && l["usage"]["prompt_tokens"].as_u64().is_some())
            && sent.iter().filter(|c| c.body.to_string().contains("hands, say hi")).map(|c| c.body["model"].clone()).collect::<Vec<_>>() == [json!(SONNET)],
        &r.text,
    );
    let tools = json!([{ "type": "function", "function": { "name": "shell", "parameters": { "type": "object", "properties": { "cmd": { "type": "string" } } } } }]);
    let r = api.signed(&hand, "POST", route, Some(&json!({ "model": "cheap", "messages": [{ "role": "user", "content": "run it [[call shell {\"cmd\":\"ls\"}]]" }], "tools": tools })))?;
    s.ok(
        "and its unstreamed call answers one chat completion: the tool call, why it stopped, the usage",
        r.status == 200 && r.body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] == "{\"cmd\":\"ls\"}" && r.body["choices"][0]["finish_reason"] == "tool_calls" && r.body["usage"]["completion_tokens"].as_u64().is_some(),
        &r,
    );
    let counted = s.eventually(Duration::from_secs(20), || uses_of(&model_uses(api, &owner, &computer), "anthropic", SONNET, "hands")["calls"] == 2);
    s.ok(
        "both counted as hands on the person's computer, and no model call reserved or charged on their ledger",
        counted && entries(api, &owner_id, "aig:").is_empty(),
        json!({ "uses": model_uses(api, &owner, &computer), "ledger": entries(api, &owner_id, "aig:") }),
    );
    let m = api.signed(&owner, "GET", "/api/models", None)?;
    s.ok(
        "the person's choices persist",
        m.body["roles"] == json!({ "chat": choice("anthropic", SONNET), "memory": choice("anthropic", HAIKU), "hands": choice("anthropic", SONNET) }),
        &m,
    );
    Ok(())
}

/// The section `cli`'s part (the module's doc): `home`'s CLI is signed in as `keys`.
pub(super) fn cli(s: &mut Suite, api: &Api, home: &std::path::Path, keys: &Keys) -> Result<()> {
    if s.hosted() {
        s.skip("fragment connect chatgpt signs in and hands its tokens over", "it signs in through the model vendors' fake, a local run's");
        return Ok(());
    }
    let r = api.signed(keys, "POST", "/api/computers", Some(&json!({})))?;
    anyhow::ensure!(r.status == 200, "making a computer: {r}");
    let host = api.signed(keys, "GET", "/api/models", None)?.body["providers"].as_array().and_then(|l| l.iter().find(|p| p["provider"] == "chatgpt")).map(|p| p["hostId"].as_str().unwrap_or("").to_string()).unwrap_or_default();
    let issuer = s.vendors.url.clone();
    let connect = |s: &Suite, args: &[&str]| -> Result<(bool, String, String)> {
        let mut child = s
            .bare_cli()
            .args(args)
            .env("HOME", home)
            .env("FRAGMENT_HOST", &api.base)
            .env("FRAGMENT_CHATGPT_ISSUER", &issuer)
            .env_remove("FRAGMENT_OUTPUT")
            .env_remove("BROWSER")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut err = BufReader::new(child.stderr.take().context("its stderr")?);
        let mut said = String::new();
        let mut line = String::new();
        // bounded: the CLI prints the terms and the link, then waits
        let link = loop {
            line.clear();
            if err.read_line(&mut line)? == 0 {
                break None;
            }
            said.push_str(&line);
            if let Some(url) = line.trim().strip_prefix("http").map(|rest| format!("http{rest}")).filter(|u| u.contains("/api/accounts/authorize")) {
                break Some(url);
            }
        };
        // the browser: the authorize page redirects to the CLI's loopback, which answers it
        let browser = reqwest::blocking::Client::builder().redirect(reqwest::redirect::Policy::limited(3)).timeout(Duration::from_secs(30)).build()?;
        let page = match &link {
            Some(url) => browser.get(url).send().and_then(|r| r.text()).unwrap_or_else(|e| format!("{e:#}")),
            None => String::new(),
        };
        // a page that is not the CLI's answer: the CLI would wait ten minutes for it
        if !page.contains("ChatGPT is connected to fragment") {
            let _ = child.kill();
        }
        let out = child.wait_with_output()?;
        let mut rest = String::new();
        std::io::Read::read_to_string(&mut err, &mut rest)?;
        Ok((out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), format!("{said}{rest}\n(the page said: {page})")))
    };
    let (ok, out, err) = connect(s, &["connect", "chatgpt", "--no-browser", "--port", "0"])?;
    let asked = s.vendors.authorized().last().cloned().unwrap_or_default();
    let m = api.signed(keys, "GET", "/api/models", None)?;
    let row = provider(&m.body, "chatgpt").clone();
    s.ok(
        "fragment connect chatgpt signs in with ChatGPT through a loopback (PKCE S256, the platform's host id, registering fragment the first time) and hands the platform its tokens: connected as the account, the terms said",
        ok && out.contains(&format!("ChatGPT is connected as {CHATGPT_EMAIL}")) && err.contains("a paid or remotely hosted app joins its waitlist")
            && asked.get("client_id").map(String::as_str) == Some("dynamic_agent_client") && asked.get("agent_name_hint").map(String::as_str) == Some("Fragment")
            && asked.get("ext_agent_host_id") == Some(&host) && asked.get("code_challenge_method").map(String::as_str) == Some("S256")
            && asked.get("redirect_uri").is_some_and(|u| u.starts_with("http://127.0.0.1:") && u.ends_with("/auth/callback"))
            && row["state"] == "connected" && row["account"] == CHATGPT_EMAIL && row["clientId"].as_str().is_some_and(|c| c.starts_with("oaiapp_fake_")),
        json!({ "stdout": out, "stderr": err, "asked": asked, "models": m.body }),
    );
    let client = row["clientId"].clone();
    let forgot = s.cli_json(api, home, &["connect", "chatgpt", "--forget", "--json"])?;
    s.ok("fragment connect chatgpt --forget signs out: revoked at OpenAI", forgot["state"] == "not_connected" && forgot["revoked"] == true, &forgot);
    let (ok, _, err) = connect(s, &["connect", "chatgpt", "--no-browser", "--port", "0"])?;
    let asked = s.vendors.authorized().last().cloned().unwrap_or_default();
    s.ok(
        "signing in again reuses the client fragment registered (no new registration), and hints the account",
        ok && asked.get("client_id").map(|c| json!(c)) == Some(client.clone()) && !asked.contains_key("agent_name_hint") && asked.get("login_hint").map(String::as_str) == Some(CHATGPT_EMAIL),
        json!({ "stderr": err, "asked": asked }),
    );
    Ok(())
}

/// The section `shell-ui`'s part (the module's doc): settings' Models, in
/// the page `page` signed in by `session`.
pub(super) fn ui(s: &mut Suite, api: &Api, b: &mut Browser, page: &Page, session: &str) -> Result<()> {
    let wait = Duration::from_secs(30);
    let shell = |method: &'static str, path: &str, body: Option<&Value>| super::shell::shell(api, session, method, path, body, &[]);
    shell("PUT", "/api/connections/anthropic/key", Some(&json!({ "key": "sk-ant-e2e-ui-key" })))?;
    b.reload(page)?;
    let drawn = b.until(page, "document.querySelectorAll('#settings-models select.model-picker').length === 3 && !!document.querySelector('#settings-models [data-model-provider=anthropic][data-state=set]')", wait);
    let shown = b.eval(
        page,
        "(() => { const m = document.getElementById('settings-models'); const pick = (r) => m.querySelector(`select[data-role=${r}]`); \
          return { roles: ['chat', 'memory', 'hands'].map((r) => pick(r).value), claude: [...pick('chat').options].filter((o) => o.value.startsWith('anthropic:')).map((o) => o.textContent), \
          providers: [...m.querySelectorAll('[data-model-provider]')].map((p) => [p.dataset.modelProvider, p.dataset.state]), \
          terms: !!m.querySelector('[data-terms=chatgpt]'), command: [...m.querySelectorAll('pre.command')].map((p) => p.textContent), \
          credits: !!m.querySelector('a[href=\"https://support.claude.com/en/articles/15036540\"]') }; })()",
    )?;
    s.ok(
        "settings' Models: three pickers (Chat, Memory, Hands) on Fragment's GLM-5.3 Flash by default, Claude's models listed once a key is set (its suggestion first), ChatGPT's command and OpenAI's terms, and Max and Team plans' credits linked",
        drawn
            && shown["roles"] == json!(["fragment:cheap", "fragment:cheap", "fragment:cheap"])
            && shown["claude"][0] == "Claude Sonnet 5.5 (suggested)"
            && shown["providers"] == json!([["anthropic", "set"], ["openai", "not_set"], ["chatgpt", "not_connected"]])
            && shown["terms"] == true && shown["command"] == json!(["fragment connect chatgpt"]) && shown["credits"] == true,
        &shown,
    );
    b.eval(page, &format!("(() => {{ const p = document.querySelector('#settings-models select[data-role=chat]'); p.value = 'anthropic:{SONNET}'; p.dispatchEvent(new Event('change')); return true; }})()"))?;
    let saved = b.until(page, "document.querySelector('#settings-models select[data-role=chat]').dataset.saved === 'anthropic:claude-sonnet-5-5'", wait);
    let kept = shell("GET", "/api/models", None)?;
    b.reload(page)?;
    let again = b.until(page, &format!("document.querySelector('#settings-models select[data-role=chat]')?.value === 'anthropic:{SONNET}'"), wait);
    s.ok(
        "a model picked for chat in the page is kept, and shown again after a reload",
        saved && again && kept.body["roles"]["chat"] == choice("anthropic", SONNET),
        &kept,
    );
    b.eval(page, "(document.getElementById('settings-models').scrollIntoView(), true)")?;
    let _ = b.screenshot(page, &s.scratch.join("shell-ui").join("desktop-models.png"));
    // back to the default, so the lane's later checks see Fragment's model
    shell("PUT", "/api/models/choices", Some(&json!({ "chat": null })))?;
    Ok(())
}
