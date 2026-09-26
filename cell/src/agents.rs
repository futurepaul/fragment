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
//!
//! A fragment's `agent` block is made so here (`sync_agent`, after a
//! deploy): its owner's own agent, or the fragment's own (named as the
//! fragment is), an editor that listens to the declared channel.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use worker::*;

use std::collections::HashMap;

use fragment_core::access::{listed_role, Cap};
use fragment_core::manifest::AgentDecl;
use fragment_core::npub;
use fragment_core::steps::AgentTurn;
use fragment_proto::{limits, split_fragment_name, valid_fragment_name, valid_label, ErrorCode, FragmentList, IdentityKind, ListedFragment, Role};

use crate::error::{CellError, CellResult};
use crate::fragment::{Caller, FragmentCell, MetaKey};
use crate::jobs::{permanent, RunRow, StepFail};
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
            json_answer(&create(env, &who, label, &v, None).await?)
        }
        (method, ["api", "a", name, rest @ ..]) => {
            let name = named(name, &who)?;
            let path = if rest.is_empty() { format!("/api/a/{name}") } else { format!("/api/a/{name}/{}", rest.join("/")) };
            // the query rides along (a state read's wait_ms)
            let path = match url.query() {
                Some(query) => format!("{path}?{query}"),
                None => path,
            };
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
/// `model` and `instructions`; `scope`, the fragment a fragment's own agent
/// serves, only a deploy names) and registers its key as theirs: `{name,
/// npub, model, scope, id}`. Asking again for one already theirs answers
/// it (`replayed`), registered.
pub(crate) async fn create(env: &Env, who: &Signed, label: &str, options: &Value, scope: Option<&str>) -> CellResult<Value> {
    if who.kind != IdentityKind::Person {
        return Err(CellError::new(ErrorCode::Forbidden, "agents are made by people"));
    }
    let name = named(label, who)?;
    if split_fragment_name(&name).map(|(_, u)| u) != who.username.as_deref() {
        return Err(CellError::new(ErrorCode::Forbidden, "you make agents under your own username"));
    }
    let body = json!({ "name": name, "model": options["model"], "instructions": options["instructions"], "scope": scope });
    let mut made = ask_json(env, Method::Post, "/api/agents", &who.id, &body).await?;
    let hex = made["npub"].as_str().and_then(npub::parse).ok_or_else(|| CellError::host("the agents' script answered no key"))?;
    let registered = ask_registry(env, &calls::RegisterAgent { owner: calls::By::Identity(who.id.clone()), key: hex }).await?;
    made["id"] = json!(registered.id);
    Ok(made)
}

/// The person's own agent, `agent.<username>`, made on first need:
/// `(identity, npub, name)`.
pub(crate) async fn own_agent(env: &Env, owner: &str, username: &str) -> CellResult<(String, String, String)> {
    let identity = fragment_proto::Identity { id: owner.to_string(), kind: IdentityKind::Person, owner: None, username: Some(username.to_string()) };
    let who = Signed::new(identity, None);
    let made = create(env, &who, DEFAULT_LABEL, &Value::Null, None).await?;
    let text = |k: &str| made[k].as_str().map(str::to_string).ok_or_else(|| CellError::host(format!("the agent answered no {k}")));
    Ok((text("id")?, text("npub")?, text("name")?))
}

/// An identity's fragments, as its `Principal` cell lists them.
async fn listed(env: &Env, identity: &str) -> CellResult<FragmentList> {
    let list = Request::new("https://principal.internal/list", Method::Get)?;
    Ok(env.durable_object("PRINCIPAL")?.get_by_name(identity)?.fetch_with_request(list).await?.json().await?)
}

/// `GET /api/fragments?for=<asker>`, signed by an agent: the fragments it
/// reaches for whoever asked (ROADMAP decision 17): the asker's, where the
/// agent or its owner is in too, each with the role the agent acts with
/// there (`access::listed_role`). A call decides again, live.
pub(crate) async fn reachable(env: &Env, agent: &Signed, asker: &str) -> CellResult<FragmentList> {
    let owner = agent.owner.as_deref().ok_or_else(|| CellError::host("an agent without an owner"))?;
    let (askers, own, owners) = futures_util::future::join3(listed(env, asker), listed(env, &agent.id), listed(env, owner)).await;
    let roles = |l: FragmentList| -> HashMap<String, Role> { l.fragments.into_iter().map(|f| (f.name, f.role)).collect() };
    let (own, owners) = (roles(own?), roles(owners?));
    let fragments = askers?
        .fragments
        .into_iter()
        .filter_map(|f| {
            let cap = Cap { agent: own.get(&f.name).copied(), owner: owners.get(&f.name).copied() };
            listed_role(Some(f.role), cap).map(|role| ListedFragment { name: f.name, role, sharing: None })
        })
        .collect();
    Ok(FragmentList { fragments })
}

/// What live's `agent` block declares, as the fragment keeps it
/// (`MetaKey::AgentLive`): the block, and its instructions' text at live.
#[derive(Serialize, Deserialize, PartialEq)]
pub(crate) struct AgentLive {
    pub decl: AgentDecl,
    pub instructions: Option<String>,
}

/// The agent the platform last made answer here (`MetaKey::AgentJoined`),
/// so a deploy that declares another, or none, knows whom to remove.
#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct Joined {
    agent: String,
    name: String,
    channel: String,
}

fn stored<T: DeserializeOwned>(text: Option<String>, what: &str) -> CellResult<Option<T>> {
    text.map(|t| serde_json::from_str(&t)).transpose().map_err(|e| CellError::host(format!("the stored {what}: {e}")))
}

impl FragmentCell {
    /// An `agent` block as live holds it, with the text of the instructions
    /// it names; the inner `Err` says why live's code cannot be installed.
    pub(crate) async fn agent_at_live(&self, repo: &str, sha: &str, decl: &AgentDecl) -> CellResult<Result<AgentLive, String>> {
        let Some(path) = decl.instructions.as_deref() else { return Ok(Ok(AgentLive { decl: decl.clone(), instructions: None })) };
        let max = limits::AGENT_INSTRUCTIONS_MAX_BYTES;
        let text = match self.cs()?.read(repo, sha, path, max).await {
            Ok(Some(bytes)) => String::from_utf8(bytes).map_err(|_| format!("agent.instructions: {path} is not UTF-8")),
            Ok(None) => Err(format!("agent.instructions: {path} is not in live")),
            Err(e) if e.code == ErrorCode::TooLarge => Err(format!("agent.instructions: {path} is over {max} bytes")),
            Err(e) => return Err(e),
        };
        Ok(text.and_then(|t| match t.trim().is_empty() {
            true => Err(format!("agent.instructions: {path} is empty")),
            false => Ok(AgentLive { decl: decl.clone(), instructions: Some(t) }),
        }))
    }

    /// The fragment's own agent (not its owner's personal one: that is
    /// named `agent.<username>`, and a fragment's own is named as the
    /// fragment), once the platform has made it answer here.
    pub(crate) fn own_agent(&self) -> CellResult<Option<String>> {
        let joined: Option<Joined> = stored(self.meta(MetaKey::AgentJoined)?, "agent joined")?;
        let name = self.name()?;
        Ok(joined.filter(|j| j.name == name).map(|j| j.agent))
    }

    /// Keeps what a new live declares, and has `sync_agent` make it so. A
    /// fragment that never declared an agent carries nothing of one.
    pub(crate) fn set_agent_live(&self, live: Option<&AgentLive>) -> CellResult<()> {
        match live {
            Some(a) => self.set_meta(MetaKey::AgentLive, &serde_json::to_string(a).expect("an agent block serializes"))?,
            None if self.meta(MetaKey::AgentLive)?.is_none() => return Ok(()),
            None => self.del_meta(MetaKey::AgentLive)?,
        }
        self.set_meta(MetaKey::AgentPending, "1")
    }

    /// Makes the agent live declares answer here: its owner's own, or the
    /// fragment's (made on first need, and given what the block declares),
    /// an editor that listens to the declared channel. The one that
    /// answered before leaves when it is not that one, and stops listening
    /// where it no longer answers. Each part is idempotent, so the alarm
    /// retries one that did not finish.
    pub(crate) async fn sync_agent(&self) -> CellResult<()> {
        if self.meta(MetaKey::AgentPending)?.is_none() {
            return Ok(());
        }
        let [live, joined] = self.metas([MetaKey::AgentLive, MetaKey::AgentJoined])?;
        let (live, joined): (Option<AgentLive>, Option<Joined>) = (stored(live, "agent block")?, stored(joined, "agent joined")?);
        let (name, owner) = (self.name()?, self.must(MetaKey::Owner)?);
        let (label, username) = split_fragment_name(&name).ok_or_else(|| CellError::host(format!("{name} is not <label>.<username>")))?;
        let identity = fragment_proto::Identity { id: owner.clone(), kind: IdentityKind::Person, owner: None, username: Some(username.to_string()) };
        let signed = Signed::new(identity, None);
        let wanted = match &live {
            None => None,
            Some(live) => {
                let (agent, agent_name) = match live.decl.personal {
                    true => own_agent(&self.env, &owner, username).await.map(|(id, _, name)| (id, name))?,
                    false => self.fragment_agent(&signed, label, &name, live).await?,
                };
                Some(Joined { agent, name: agent_name, channel: live.decl.channel.clone() })
            }
        };
        let as_owner = Caller { signed: Some(signed), unresolved: None, url: url::Url::parse("https://fragment.internal/").expect("a URL"), mode: None };
        if let Some(before) = joined.filter(|j| Some(j) != wanted.as_ref()) {
            if wanted.as_ref().is_some_and(|w| w.agent == before.agent) {
                self.exec("DELETE FROM subs WHERE principal = ? AND channel = ?", vec![before.agent.as_str().into(), before.channel.as_str().into()])?;
            } else if self.member_role(&before.agent)?.is_some() {
                self.remove_member(&as_owner, &before.agent).await?;
            }
            self.event("agent.left", &format!("{} no longer answers on {}", before.name, before.channel), json!({ "agent": before.agent }));
        }
        let Some(wanted) = wanted else {
            self.del_meta(MetaKey::AgentJoined)?;
            return self.del_meta(MetaKey::AgentPending);
        };
        if self.member_role(&wanted.agent)?.is_none() {
            self.set_member(&as_owner, &wanted.agent, fragment_proto::SetRole { role: Role::Editor }).await?;
        }
        let listen = json!({ "fragment": name, "channel": wanted.channel });
        ask_json(&self.env, Method::Post, &format!("/api/a/{}/listen", wanted.name), &owner, &listen).await?;
        self.set_meta(MetaKey::AgentJoined, &serde_json::to_string(&wanted).expect("a join serializes"))?;
        self.event("agent.joined", &format!("{} answers on {}", wanted.name, wanted.channel), json!({ "agent": wanted.agent }));
        self.del_meta(MetaKey::AgentPending)
    }

    /// The fragment's own agent, named as the fragment is: made its owner's
    /// on first need, for this fragment alone (`scope`), then given what
    /// live declares. An agent of that name made otherwise is not taken
    /// over, nor is the owner's own (`agent.<username>`).
    async fn fragment_agent(&self, owner: &Signed, label: &str, name: &str, live: &AgentLive) -> CellResult<(String, String)> {
        if label == DEFAULT_LABEL {
            return Err(CellError::invalid(format!("{name} cannot have an agent of its own: {name} is its owner's own agent")));
        }
        let options = json!({ "model": live.decl.model, "instructions": live.instructions });
        let made = create(&self.env, owner, name, &options, Some(name)).await?;
        if made["scope"] != name {
            return Err(CellError::new(ErrorCode::AlreadyExists, format!("{name} is an agent of its owner's already, not this fragment's")));
        }
        let scope = json!({ "fragment": name, "tools": live.decl.tools, "instructions": live.instructions, "model": live.decl.model });
        ask_json(&self.env, Method::Put, &format!("/api/a/{name}/scope"), &owner.id, &scope).await?;
        let id = made["id"].as_str().ok_or_else(|| CellError::host("the agents' script answered no identity"))?;
        Ok((id.to_string(), name.to_string()))
    }

    /// The fragment's own agent a job's turn runs on (`job.agent`), and its
    /// owner, as whom the platform asks it.
    fn job_agent(&self) -> Result<(Joined, String), StepFail> {
        let retry = |e: CellError| StepFail::Retry(e.message);
        let [live, joined, owner] = self.metas([MetaKey::AgentLive, MetaKey::AgentJoined, MetaKey::Owner]).map_err(retry)?;
        let (live, joined): (Option<AgentLive>, Option<Joined>) = (stored(live, "agent block").map_err(retry)?, stored(joined, "agent joined").map_err(retry)?);
        match (live, joined, owner) {
            (Some(live), _, _) if live.decl.personal => Err(permanent("job.agent runs the fragment's own agent; its agent block names its owner's")),
            (Some(_), Some(joined), Some(owner)) => Ok((joined, owner)),
            (Some(_), None, _) => Err(StepFail::Retry("the fragment's agent is still being made".into())),
            _ => Err(permanent("job.agent needs an agent of the fragment's own: declare one in fragment.json (`agent`)")),
        }
    }

    /// `job.agent`'s start: one turn of the fragment's own agent for the
    /// run's principal (a triggered run's is the fragment itself, so the
    /// agent acts as its own member, an editor), named by the run and this
    /// step, not the attempt: a retried or replayed step reattaches.
    pub(crate) async fn step_agent_start(&self, run: &RunRow, index: u32, turn: AgentTurn) -> Result<Value, StepFail> {
        let (joined, owner) = self.job_agent()?;
        if let Some(channel) = turn.channel.as_deref() {
            // the agent is an editor here: a channel only the owner posts to would refuse it
            let postable = self.declared_channel(channel).map_err(|e| StepFail::Retry(e.message))?.and_then(|c| c.post).is_some_and(|p| p <= Role::Editor);
            if !postable {
                return Err(permanent(format!("job.agent's channel {channel:?} is not one this fragment declares for editors to post to")));
            }
        }
        let asker = if npub::is_identity(&run.principal) { run.principal.clone() } else { joined.agent.clone() };
        let id = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(self.step_ref(run, index)?.as_bytes()));
        let body = json!({
            "id": id,
            "asker": asker,
            "conversation": turn.conversation.unwrap_or_else(|| format!("run-{}", run.id)),
            "channel": turn.channel,
            "text": turn.prompt,
        });
        ask_json(&self.env, Method::Post, &format!("/api/a/{}/job", joined.name), &owner, &body).await.map_err(agent_fail)
    }

    /// `job.agent`'s poll: its turn's state (`{ended, outcome, text, error}`).
    pub(crate) async fn step_agent_poll(&self, turn: &str) -> Result<Value, StepFail> {
        if turn.len() != 24 || !turn.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(permanent(format!("{turn:?} is not a turn's id")));
        }
        let (joined, owner) = self.job_agent()?;
        ask_json(&self.env, Method::Get, &format!("/api/a/{}/job?turn={turn}", joined.name), &owner, &Value::Null).await.map_err(agent_fail)
    }
}

/// The agent's refusal as a step's failure: one it will give again is for
/// good (the job sees it); any other may pass (the step is retried).
fn agent_fail(e: CellError) -> StepFail {
    match e.code {
        ErrorCode::InvalidRequest | ErrorCode::NotFound | ErrorCode::Forbidden => permanent(e.message),
        _ => StepFail::Retry(format!("the fragment's agent: {}", e.message)),
    }
}
