//! The agents' script (`agent/`), co-hosted in this fleet (docs/phase-6.md,
//! step 4). It has no ingress of its own: this router authenticates each
//! request as it does its own (a signature resolved to an identity by the
//! registry) and hands it on with the caller's identity, which the script
//! trusts. An inbox delivery passes as it came: its token is the
//! capability.
//!
//! An agent's name is `<label>.<username>`, its owner's username, as a
//! fragment's is; a bare label names one of the signer's own. Making an
//! agent also registers it as its owner's, in the same request.
//!
//! A fragment's `agent` block is made so here (`sync_agent`, after a
//! deploy): the fragment's own agent (named as the fragment is), an editor
//! that listens to the declared channel.

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

use fragment_core::ledger::Spend;

use crate::error::{CellError, CellResult};
use crate::fragment::{Caller, FragmentCell, MetaKey};
use crate::ledger::MaySpend;
use crate::jobs::{permanent, RunRow, StepFail};
use crate::registry::calls;
use crate::routed::Signed;
use crate::{ask_registry, js, read_body, signer};

/// The caller's identity, set by this router only (the script has no other
/// way in); one name for both ends (`fragment_proto::routed`).
pub(crate) const PRINCIPAL_HEADER: &str = fragment_proto::routed::AGENT_PRINCIPAL;

/// `/api/agents` and `/api/a/*`.
pub(crate) async fn route(mut req: Request, env: &Env, url: &Url, segments: &[&str]) -> CellResult<Response> {
    // an inbox delivery carries its own capability, its token
    if let (Method::Post, ["api", "a", _, "inbox", _]) = (req.method(), segments) {
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
    let registered = ask_registry(env, &calls::RegisterAgent { owner: calls::By::Identity(who.id.clone()), key: hex, fragment: None }).await?;
    made["id"] = json!(registered.id);
    Ok(made)
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
            // a people-only share is the fragment's to know: a call decides again
            let cap = Cap { agent: own.get(&f.name).copied(), owner: owners.get(&f.name).copied(), people_only: false };
            listed_role(Some(f.role), cap).map(|role| ListedFragment { name: f.name, role, kind: f.kind, title: f.title, sharing: None })
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

/// Where the agent of a newly declared channel starts hearing it
/// (`MetaKey::AgentFloor`): the channel's last record as the deploy that
/// declared it went live. What is posted after, before the agent listens,
/// reaches it as it joins (`catch_up`); nothing from before ever does.
#[derive(Serialize, Deserialize)]
struct Floor {
    channel: String,
    seq: i64,
}

/// One join catches up on the newest this many records, at most.
const CATCH_UP_MAX: i64 = 32;

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

    /// The fragment's own agent (named as the fragment), once the platform
    /// has made it answer here.
    pub(crate) fn own_agent(&self) -> CellResult<Option<String>> {
        let joined: Option<Joined> = stored(self.meta(MetaKey::AgentJoined)?, "agent joined")?;
        let name = self.name()?;
        Ok(joined.filter(|j| j.name == name).map(|j| j.agent))
    }

    /// Keeps what a new live declares, and has `sync_agent` make it so. A
    /// fragment that never declared an agent carries nothing of one. A
    /// channel newly declared gets its floor (`Floor`).
    pub(crate) fn set_agent_live(&self, live: Option<&AgentLive>) -> CellResult<()> {
        match live {
            Some(a) => {
                let before: Option<AgentLive> = stored(self.meta(MetaKey::AgentLive)?, "agent block")?;
                if before.is_none_or(|b| b.decl.channel != a.decl.channel) {
                    let last = self.rows("SELECT MAX(seq) AS seq FROM records WHERE channel = ?", vec![a.decl.channel.as_str().into()])?;
                    let floor = Floor { channel: a.decl.channel.clone(), seq: last.first().and_then(|r| r["seq"].as_i64()).unwrap_or(0) };
                    self.set_meta(MetaKey::AgentFloor, &serde_json::to_string(&floor).expect("a floor serializes"))?;
                }
                self.set_meta(MetaKey::AgentLive, &serde_json::to_string(a).expect("an agent block serializes"))?
            }
            None if self.meta(MetaKey::AgentLive)?.is_none() => return Ok(()),
            None => {
                self.del_meta(MetaKey::AgentFloor)?;
                self.del_meta(MetaKey::AgentLive)?
            }
        }
        self.set_meta(MetaKey::AgentPending, &js::random_hex::<8>())
    }

    /// Makes the agent live declares answer here: the fragment's own (made
    /// on first need, and given what the block declares),
    /// an editor that listens to the declared channel, caught up on what
    /// was posted there since its channel was declared (`catch_up`). The
    /// one that answered before leaves when it is not that one, and stops
    /// listening where it no longer answers. Each part is idempotent, so
    /// the alarm retries one that did not finish. One runs at a time, and
    /// a live that declares again meanwhile leaves its own pending.
    pub(crate) async fn sync_agent(&self) -> CellResult<()> {
        let _held = self.joining.lock().await;
        let Some(pending) = self.meta(MetaKey::AgentPending)? else { return Ok(()) };
        let settled = || self.exec("DELETE FROM meta WHERE key = ? AND value = ?", vec![MetaKey::AgentPending.key().into(), pending.as_str().into()]);
        self.test_countdown(MetaKey::TestFailJoin, "the agent's join failed")?;
        let [live, joined] = self.metas([MetaKey::AgentLive, MetaKey::AgentJoined])?;
        let (live, joined): (Option<AgentLive>, Option<Joined>) = (stored(live, "agent block")?, stored(joined, "agent joined")?);
        let (name, owner) = (self.name()?, self.must(MetaKey::Owner)?);
        let (_, username) = split_fragment_name(&name).ok_or_else(|| CellError::host(format!("{name} is not <label>.<username>")))?;
        let identity = fragment_proto::Identity { id: owner.clone(), kind: IdentityKind::Person, owner: None, username: Some(username.to_string()), held: None };
        let signed = Signed::new(identity, None);
        let wanted = match &live {
            None => None,
            Some(live) => {
                let (agent, agent_name) = self.fragment_agent(&signed, &name, live).await?;
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
            return settled();
        };
        if self.member_role(&wanted.agent)?.is_none() {
            self.set_member(&as_owner, &wanted.agent, fragment_proto::SetRole { role: Role::Editor, people_only: false }).await?;
        }
        let listen = json!({ "fragment": name, "channel": wanted.channel });
        let listening = ask_json(&self.env, Method::Post, &format!("/api/a/{}/listen", wanted.name), &owner, &listen).await?;
        self.set_meta(MetaKey::AgentJoined, &serde_json::to_string(&wanted).expect("a join serializes"))?;
        self.event("agent.joined", &format!("{} answers on {}", wanted.name, wanted.channel), json!({ "agent": wanted.agent }));
        let sub = listening["subscription"].as_i64().ok_or_else(|| CellError::host("the agent's listen named no subscription"))?;
        if self.catch_up(&wanted.channel, sub)? {
            self.drain_deliveries().await;
        }
        settled()
    }

    /// The records of `channel` past its floor that its subscription `sub`
    /// never carried (posted before it began), the newest CATCH_UP_MAX,
    /// queued for it once: the floor goes with them, so a later join (a
    /// redeploy) catches up on nothing. The agent never answers a record
    /// twice (it keeps what it heard). Answers whether any were queued.
    fn catch_up(&self, channel: &str, sub: i64) -> CellResult<bool> {
        let floor: Option<Floor> = stored(self.meta(MetaKey::AgentFloor)?, "agent floor")?;
        // another channel's is a newer live's, for its own join
        let Some(floor) = floor.filter(|f| f.channel == channel) else { return Ok(false) };
        let queued = self.rows(
            "INSERT INTO delivery_outbox (kind, sub, channel, seq, next_at) SELECT 'record', ?, channel, seq, ? FROM (
               SELECT r.channel, r.seq FROM records r JOIN subs s ON s.id = ? AND s.channel = r.channel
               WHERE r.channel = ? AND r.seq > ? AND r.at <= s.created_at ORDER BY r.seq DESC LIMIT ?)
             ORDER BY seq RETURNING id",
            vec![
                SqlStorageValue::Integer(sub),
                SqlStorageValue::Integer(js::now_ms()),
                SqlStorageValue::Integer(sub),
                channel.into(),
                SqlStorageValue::Integer(floor.seq),
                SqlStorageValue::Integer(CATCH_UP_MAX),
            ],
        )?;
        self.del_meta(MetaKey::AgentFloor)?;
        Ok(!queued.is_empty())
    }

    /// The fragment's own agent, named as the fragment is: made its owner's
    /// on first need, for this fragment alone (`scope`), then given what
    /// live declares. An agent of that name made otherwise is not taken
    /// over.
    async fn fragment_agent(&self, owner: &Signed, name: &str, live: &AgentLive) -> CellResult<(String, String)> {
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
        // a turn its owner's ledger would refuse never starts (decision 27):
        // stopped agents, no credit, or this fragment's cap for anyone but its
        // owner (the fragment's own agent acting as itself is the owner's)
        let by_owner = asker == owner || asker == joined.agent;
        let may = MaySpend { spend: Spend::AgentTurn, fragment: Some(self.name().map_err(|e| StepFail::Retry(e.message))?), by_owner };
        crate::ledger::ask(&self.env, &owner, &may).await.map_err(|e| match e.refused {
            Some(_) => permanent(e.message),
            None => StepFail::Retry(format!("the owner's ledger: {}", e.message)),
        })?;
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
