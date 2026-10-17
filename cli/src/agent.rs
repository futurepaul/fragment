//! `fragment agent`'s making and keeping (docs/cloudflare-v1.md, decision
//! 62, proposed): an agent makes another of its owner's agents, changes its
//! job and its model, and starts or stops it, as its owner would in
//! Settings, so a person's default agent keeps the rest.
//!
//! - **Making one** (`--new <label>`) is the shell's first run for any
//!   agent: its agent fragment (the blessed `agent` template, titled by its
//!   name), its start on its owner's computer (`PUT /api/computers/{id}/
//!   agents/{fragment}`, which gives it its identity), its job (`SOUL.md`)
//!   and settings (`agent.json`), a deploy, and its own chat with its owner
//!   (`<label>-chat`, as the shell names one: its Bot Chat), with it in it.
//!   An agent that makes the chat is in it as its maker (an editor, the
//!   first agent added: the chat's lead), so it leaves once the new agent
//!   is in, and the new one leads its own chat. Each step finds what an
//!   earlier try made, so a retry finishes it; a label whose agent has
//!   another job already is refused, never rewritten.
//! - **Changing one** writes its `SOUL.md` and `agent.json` (`tier`, and
//!   `model`: a model of a provider its owner connected) in one commit and
//!   deploys them: its computer reads them again within seconds.
//! - **Starting and stopping one** is its computer's route; an agent never
//!   starts or stops itself (the platform refuses).

use anyhow::{Context, Result};
use fragment_proto::{FragmentKind, FragmentList, ListedFragment, MemberList, Role, Tier};
use serde::Serialize;
use serde_json::{json, Value};

use crate::api::{Client, Code, CodedError};

/// A job (`SOUL.md`) is a page, not a manual.
pub const JOB_MAX_BYTES: usize = 8 * 1024;
/// A model's id (`anthropic/claude-sonnet-5.5`), at most.
pub const MODEL_ID_MAX_BYTES: usize = 128;
/// What an agent's own chat's label adds to its label (the shell's).
const OWN_CHAT: &str = "-chat";

fn coded(code: Code, msg: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError { code, msg: msg.into() })
}

/// A model of a provider its owner connected (`agent.json`'s `model`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Model {
    pub provider: String,
    pub id: String,
}

/// `<provider>:<id>` (`openrouter:anthropic/claude-sonnet-5.5`).
pub fn parse_model(s: &str) -> std::result::Result<Model, String> {
    let (provider, id) = s.split_once(':').ok_or_else(|| format!("a model is <provider>:<id> (openrouter:anthropic/claude-sonnet-5.5), not {s:?}"))?;
    if !fragment_core::catalog::valid_name(provider) {
        return Err(format!("{provider:?} is no provider's name"));
    }
    if id.is_empty() || id.len() > MODEL_ID_MAX_BYTES || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("a model's id is 1 to {MODEL_ID_MAX_BYTES} bytes, with no spaces, not {id:?}"));
    }
    Ok(Model { provider: provider.to_string(), id: id.to_string() })
}

/// A tier an agent runs on: `cheap` or `medium` (`high` the platform
/// refuses for now: decision 23).
pub fn parse_tier(s: &str) -> std::result::Result<Tier, String> {
    match Tier::parse(s) {
        Some(t @ (Tier::Cheap | Tier::Medium)) => Ok(t),
        Some(Tier::High) => Err("the platform refuses `high` for now (docs/cloudflare-v1.md, decision 23): cheap or medium".into()),
        None => Err(format!("a tier is cheap or medium, not {s:?}")),
    }
}

/// A job as `SOUL.md` holds it: its text, trimmed, ending in a newline.
pub fn job_text(text: &str) -> std::result::Result<String, String> {
    let t = text.trim();
    if t.is_empty() || t.len() > JOB_MAX_BYTES {
        return Err(format!("a job is 1 to {JOB_MAX_BYTES} bytes"));
    }
    Ok(format!("{t}\n"))
}

/// What a change asks of an agent's settings (`agent.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub tier: Option<Tier>,
    pub model: Option<Model>,
    /// Take its `model` away: its tier again.
    pub no_model: bool,
}

impl Settings {
    pub fn is_empty(&self) -> bool {
        self.tier.is_none() && self.model.is_none() && !self.no_model
    }
}

/// `agent.json` as `asked` changes it: its other keys (`color`) kept, its
/// `tier` set when asked (the default tier when it has none), its `model`
/// set or taken away.
pub fn settled(now: Option<&Value>, asked: &Settings) -> Value {
    let mut v = match now {
        Some(Value::Object(m)) => Value::Object(m.clone()),
        _ => json!({}),
    };
    match asked.tier {
        Some(t) => v["tier"] = json!(t.as_str()),
        None if v.get("tier").is_none() => v["tier"] = json!(fragment_proto::DEFAULT_TIER.as_str()),
        None => {}
    }
    if let Some(m) = &asked.model {
        v["model"] = json!({ "provider": m.provider, "id": m.id });
    }
    if asked.no_model {
        v.as_object_mut().expect("settled holds an object").remove("model");
    }
    v
}

/// Where an agent's model calls go, as its `agent.json` says: the
/// platform's tier (the deployment's models), or a provider its owner
/// connected (their own account, outside the platform). Its computer runs
/// a `model` only while the agent's credentials hold that provider, else
/// its tier; a teammate is told the `model`, the side that may be out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "on", rename_all = "camelCase")]
pub enum RunsOn {
    /// The platform's tier.
    Platform { tier: String },
    /// Its owner's own provider.
    Own { provider: String, id: String },
}

pub fn runs_on(settings: Option<&Value>) -> RunsOn {
    let s = settings.cloned().unwrap_or(Value::Null);
    match (s["model"]["provider"].as_str(), s["model"]["id"].as_str()) {
        (Some(provider), Some(id)) => RunsOn::Own { provider: provider.to_string(), id: id.to_string() },
        _ => RunsOn::Platform { tier: s["tier"].as_str().unwrap_or(fragment_proto::DEFAULT_TIER.as_str()).to_string() },
    }
}

impl RunsOn {
    pub fn said(&self) -> String {
        match self {
            RunsOn::Platform { tier } => format!("the platform's {tier} tier"),
            RunsOn::Own { provider, id } => format!("{id} on {provider}, its owner's own provider (outside the platform)"),
        }
    }
}

/// An agent its owner's computer runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Running {
    pub fragment: String,
    pub identity: String,
    /// Fenced (decision 63, proposed): it acts as itself alone.
    pub fenced: bool,
}

/// The signer's computer: its id, the agents it runs, and the asker's own
/// identity when the signer is one of them.
pub struct Home {
    pub computer: String,
    pub agents: Vec<Running>,
    pub asker: Option<String>,
}

fn agents_of(v: &Value) -> Vec<Running> {
    v["agents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Some(Running { fragment: a["fragment"].as_str()?.to_string(), identity: a["identity"].as_str()?.to_string(), fenced: a["fenced"] == true }))
        .collect()
}

/// The computer the signer's agents run on: an agent's own (`GET
/// /api/computer`, its egress's), a person's (`GET /api/computers`; with
/// `make`, made when they have none, asleep).
pub fn home(c: &Client, make: bool) -> Result<Home> {
    if let Some(mode) = c.agent() {
        let v = c.call(c.get("/api/computer")?).context("reading this computer")?;
        let agents = agents_of(&v);
        let asker = agents.iter().find(|a| a.fragment == mode.agent).map(|a| a.identity.clone()).ok_or_else(|| coded(Code::Forbidden, format!("{} does not run on this computer", mode.agent)))?;
        let computer = v["computer"].as_str().ok_or_else(|| coded(Code::ServerError, format!("this computer's view names no computer: {v}")))?.to_string();
        return Ok(Home { computer, agents, asker: Some(asker) });
    }
    let v = c.call(c.get("/api/computers")?).context("reading your computer")?;
    let view = match v["computers"].as_array().and_then(|l| l.first()).cloned() {
        Some(view) => view,
        None if make => c.call(c.post_json("/api/computers", &json!({}))?).context("making your computer")?,
        None => return Err(coded(Code::NotFound, "you have no computer yet: the shell makes one with your first agent")),
    };
    let computer = view["computer"].as_str().ok_or_else(|| coded(Code::ServerError, format!("your computer's view names no computer: {view}")))?.to_string();
    Ok(Home { computer, agents: agents_of(&view), asker: None })
}

/// The signer's owner's fragment of `kind` labelled `label`, if one.
fn own_labelled<'a>(c: &Client, list: &'a [ListedFragment], kind: FragmentKind, label: &str) -> Option<&'a ListedFragment> {
    let owns = |f: &ListedFragment| match c.agent() {
        Some(_) => f.owned,
        None => f.role == Role::Owner,
    };
    list.iter().find(|f| f.kind == kind && owns(f) && fragment_proto::split_fragment_name(&f.name).is_some_and(|(l, _)| l == label))
}

/// A file of `name` at main, or none.
fn read_file(c: &Client, name: &str, path: &str) -> Result<Option<String>> {
    let r = c.get(&format!("/api/f/{name}/file?path={path}"))?;
    match r.status {
        404 => Ok(None),
        _ if r.ok() => Ok(Some(String::from_utf8(r.body).with_context(|| format!("{name}'s {path} is not text"))?)),
        _ => Err(anyhow::Error::new(r.refusal()).context(format!("reading {name}'s {path}"))),
    }
}

/// `agent.json` of `agent`, or none (an agent fresh from the template).
pub fn settings_of(c: &Client, agent: &str) -> Result<Option<Value>> {
    match read_file(c, agent, "agent.json")? {
        Some(text) => Ok(Some(serde_json::from_str(&text).with_context(|| format!("{agent}'s agent.json is not JSON"))?)),
        None => Ok(None),
    }
}

/// Writes `files` to `agent` in one commit, keyed by what it writes (the
/// same change sent again commits nothing twice), and puts them live.
fn write_live(c: &Client, agent: &str, files: &[Value], message: &str) -> Result<()> {
    let key = format!("agent-{}", &hex::encode(<sha2::Sha256 as sha2::Digest>::digest(serde_json::to_vec(files)?))[..16]);
    c.call(c.post_json(&format!("/api/f/{agent}/files"), &json!({ "files": files, "message": message, "key": key }))?)?;
    c.call(c.post_json(&format!("/api/f/{agent}/deploy"), &json!({ "note": message }))?)?;
    Ok(())
}

fn created(c: &Client, body: Value) -> Result<String> {
    let made = c.call(c.post_json("/api/fragments", &body)?)?;
    made["name"].as_str().map(str::to_string).ok_or_else(|| coded(Code::ServerError, format!("a create named nothing: {made}")))
}

/// What `--new` asks for.
pub struct New {
    pub label: String,
    pub name: String,
    pub job: String,
    pub settings: Settings,
}

/// What `--new` made, or found an earlier try had.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Made {
    pub agent: String,
    pub identity: String,
    pub name: String,
    pub computer: String,
    pub chat: String,
    pub runs_on: RunsOn,
    /// Fenced (decision 63, proposed): on its owner's own provider, it acts
    /// as itself alone.
    pub fenced: bool,
    /// Made now (not by an earlier try).
    pub made_agent: bool,
    pub made_chat: bool,
    /// The asker left the new agent's chat, which it made.
    pub left_chat: bool,
}

/// Makes `new` one of the signer's (an agent: its owner's) agents,
/// running on their computer, with a chat of its own.
/// Fences the agent `identity` (decision 63, proposed): one on its owner's
/// own provider acts as itself alone, on the platform and on its computer.
/// Its owner's agent fences, and never lets one go: its owner does.
pub fn fence(c: &Client, identity: &str) -> Result<bool> {
    let v = c.call(c.put_json(&format!("/api/identities/{identity}/fenced"), &json!({ "fenced": true }))?).with_context(|| format!("fencing {identity}"))?;
    Ok(v["fenced"] == true)
}

/// The identity of `agent` (its fragment's npub, made when its computer
/// first took it).
fn identity_of(c: &Client, agent: &str) -> Result<String> {
    let status = c.call(c.get(&format!("/api/f/{agent}/status"))?)?;
    status["npub"].as_str().map(str::to_string).ok_or_else(|| coded(Code::ServerError, format!("{agent}'s status names no npub")))
}

/// Fences `agent` or lets it go (`fenced`): its owner either way, their
/// agent fencing only. Answers whether it is fenced now.
pub fn set_fence(c: &Client, agent: &str, fenced: bool) -> Result<bool> {
    let identity = identity_of(c, agent)?;
    let v = c.call(c.put_json(&format!("/api/identities/{identity}/fenced"), &json!({ "fenced": fenced }))?).map_err(|e| match e.downcast_ref::<CodedError>() {
        Some(ce) if ce.code == Code::NotFound => coded(Code::NotFound, format!("{agent} has never run, so it has no identity to fence yet: start it first (`fragment agent {agent} --start`)")),
        _ => e,
    })?;
    Ok(v["fenced"] == true)
}

/// Whether settings put an agent on its owner's own provider, outside the
/// platform: the agents-finite skill's convention for an untrusted model,
/// which fences it.
pub fn outside(settings: &Value) -> bool {
    matches!(runs_on(Some(settings)), RunsOn::Own { .. })
}

pub fn make(c: &Client, new: &New) -> Result<Made> {
    let label = new.label.trim();
    let chat_label = format!("{label}{OWN_CHAT}");
    if !fragment_proto::valid_label(label) || !fragment_proto::valid_label(&chat_label) {
        return Err(coded(Code::InvalidUsage, format!("{label:?} is no label for an agent: lowercase letters, digits and single dashes, short enough that {chat_label:?}, its own chat's, is one too")));
    }
    let name = new.name.trim();
    if name.is_empty() || name.chars().count() > fragment_proto::limits::TITLE_MAX_CHARS {
        return Err(coded(Code::InvalidUsage, format!("a name is 1 to {} characters", fragment_proto::limits::TITLE_MAX_CHARS)));
    }
    let job = job_text(&new.job).map_err(|m| coded(Code::InvalidUsage, m))?;
    let home = home(c, true)?;
    let list: FragmentList = c.call_as(c.get("/api/fragments")?)?;

    // its fragment, an earlier try's or new
    let (agent, made_agent) = match own_labelled(c, &list.fragments, FragmentKind::Agent, label) {
        Some(f) => (f.name.clone(), false),
        None => (created(c, json!({ "label": label, "template": "agent", "title": name }))?, true),
    };
    // a job of its own already: someone's agent, never rewritten here
    let soul = read_file(c, &agent, "SOUL.md")?;
    if let Some(had) = soul.as_deref().filter(|had| had.trim() != job.trim()) {
        let first = had.lines().next().unwrap_or("").chars().take(80).collect::<String>();
        return Err(coded(Code::Conflict, format!("{agent} is one of your agents already, with a job of its own ({first:?}): change it with `fragment agent {agent} --job …`, or make another under a new label")));
    }

    // on its owner's computer: its identity is made here
    let view = c.call(c.put_json(&format!("/api/computers/{}/agents/{agent}", home.computer), &json!({}))?)?;
    let identity = agents_of(&view).into_iter().find(|a| a.fragment == agent).map(|a| a.identity).ok_or_else(|| coded(Code::ServerError, format!("its computer did not take {agent}: {view}")))?;

    // its job and settings, then live (its computer reads them within seconds)
    // (keyed by what they say: an earlier try's same files commit nothing twice)
    let settings = settled(settings_of(c, &agent)?.as_ref(), &new.settings);
    // on its owner's own provider: fenced before its job, its model or its
    // chat exist, so no turn of its runs unfenced (its identity is made as
    // its computer takes it, just above)
    let fenced = if outside(&settings) { fence(c, &identity)? } else { false };
    let files = [json!({ "path": "SOUL.md", "text": job }), json!({ "path": "agent.json", "text": format!("{}\n", serde_json::to_string_pretty(&settings)?) })];
    write_live(c, &agent, &files, "a new agent")?;

    // its own chat with its owner, it in it
    let (chat, made_chat) = match own_labelled(c, &list.fragments, FragmentKind::Chat, &chat_label) {
        Some(f) => (f.name.clone(), false),
        None => (created(c, json!({ "label": chat_label, "template": "chat", "title": name }))?, true),
    };
    let members: MemberList = c.call_as(c.get(&format!("/api/f/{chat}/members"))?)?;
    if !members.members.iter().any(|m| m.principal == identity) {
        c.call(c.put_json(&format!("/api/f/{chat}/members/{identity}"), &json!({ "role": "editor" }))?).with_context(|| format!("adding {agent} to {chat}"))?;
    }
    // the asker, in it as its maker, leaves: the new agent leads its own chat
    let mut left_chat = false;
    if let Some(asker) = home.asker.as_deref().filter(|a| members.members.iter().any(|m| m.principal == *a)) {
        assert_ne!(asker, identity, "an agent makes another agent, not itself");
        c.call(c.delete(&format!("/api/f/{chat}/members/me"))?).with_context(|| format!("leaving {chat}, {agent}'s own chat"))?;
        left_chat = true;
    }
    Ok(Made { agent, identity, name: name.to_string(), computer: home.computer, chat, runs_on: runs_on(Some(&settings)), fenced, made_agent, made_chat, left_chat })
}

/// What a change of `agent`'s job and settings wrote (none: it already
/// said so).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Changed {
    pub agent: String,
    pub runs_on: RunsOn,
    pub changed: bool,
    /// Fenced now, put on its owner's own provider (decision 63, proposed).
    pub fenced: bool,
}

/// Writes `agent`'s job and settings as asked, and puts them live.
pub fn change(c: &Client, agent: &str, job: Option<&str>, asked: &Settings) -> Result<Changed> {
    let now = settings_of(c, agent)?;
    let mut files = Vec::new();
    if let Some(job) = job {
        let job = job_text(job).map_err(|m| coded(Code::InvalidUsage, m))?;
        if read_file(c, agent, "SOUL.md")?.as_deref() != Some(job.as_str()) {
            files.push(json!({ "path": "SOUL.md", "text": job }));
        }
    }
    let settings = settled(now.as_ref(), asked);
    // put on its owner's own provider: fenced first (its identity is its
    // fragment's npub, made when its computer first took it)
    let fenced = asked.model.is_some() && outside(&settings) && set_fence(c, agent, true)?;
    if !asked.is_empty() && now.as_ref() != Some(&settings) {
        files.push(json!({ "path": "agent.json", "text": format!("{}\n", serde_json::to_string_pretty(&settings)?) }));
    }
    if !files.is_empty() {
        write_live(c, agent, &files, "its job and model")?;
    }
    Ok(Changed { agent: agent.to_string(), runs_on: runs_on(Some(&settings)), changed: !files.is_empty(), fenced })
}

/// Starts (`start`) or stops `agent` on the signer's computer: the view
/// after, and whether it runs.
pub fn run(c: &Client, agent: &str, start: bool) -> Result<bool> {
    let home = home(c, false)?;
    let path = format!("/api/computers/{}/agents/{agent}", home.computer);
    let view = match start {
        true => c.call(c.put_json(&path, &json!({}))?)?,
        false => c.call(c.delete(&path)?)?,
    };
    Ok(agents_of(&view).iter().any(|a| a.fragment == agent))
}

/// Whether `agent` is fenced, as the signer's computer has it (none: no
/// computer, or it runs nowhere).
pub fn fenced(c: &Client, agent: &str) -> Result<Option<bool>> {
    match home(c, false) {
        Ok(h) => Ok(h.agents.iter().find(|a| a.fragment == agent).map(|a| a.fenced)),
        Err(e) if e.downcast_ref::<CodedError>().is_some_and(|e| e.code == Code::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Whether the signer's computer runs `agent` (none: no computer).
pub fn running(c: &Client, agent: &str) -> Result<Option<bool>> {
    match home(c, false) {
        Ok(h) => Ok(Some(h.agents.iter().any(|a| a.fragment == agent))),
        Err(e) if e.downcast_ref::<CodedError>().is_some_and(|e| e.code == Code::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_is_a_provider_and_an_id() {
        assert_eq!(parse_model("openrouter:anthropic/claude-sonnet-5.5"), Ok(Model { provider: "openrouter".into(), id: "anthropic/claude-sonnet-5.5".into() }));
        // an id may hold a colon of its own (`:free`)
        assert_eq!(parse_model("openrouter:z-ai/glm-5.3:free").map(|m| m.id), Ok("z-ai/glm-5.3:free".into()));
        for bad in ["anthropic/claude", "openrouter:", ":id", "Open Router:x", "openrouter:a b", &format!("openrouter:{}", "x".repeat(MODEL_ID_MAX_BYTES + 1))] {
            assert!(parse_model(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_tier_is_cheap_or_medium() {
        assert_eq!(parse_tier("cheap"), Ok(Tier::Cheap));
        assert_eq!(parse_tier("medium"), Ok(Tier::Medium));
        assert!(parse_tier("high").unwrap_err().contains("refuses"));
        assert!(parse_tier("fast").is_err());
    }

    #[test]
    fn a_job_is_a_page() {
        assert_eq!(job_text("  Keep the garden notes.  \n"), Ok("Keep the garden notes.\n".into()));
        assert!(job_text(" \n ").is_err());
        assert!(job_text(&"x".repeat(JOB_MAX_BYTES + 1)).is_err());
    }

    /// Goal: a change keeps what it does not name. Method: each change
    /// over agent.json as Settings and the shell write it.
    #[test]
    fn settings_change_only_what_is_asked() {
        let shell = json!({ "tier": "cheap", "color": "#c2410c" });
        let sonnet = Model { provider: "openrouter".into(), id: "anthropic/claude-sonnet-5.5".into() };
        // nothing asked, nothing there: the default tier
        assert_eq!(settled(None, &Settings::default()), json!({ "tier": "cheap" }));
        assert_eq!(settled(Some(&shell), &Settings::default()), shell);
        let medium = settled(Some(&shell), &Settings { tier: Some(Tier::Medium), ..Settings::default() });
        assert_eq!(medium, json!({ "tier": "medium", "color": "#c2410c" }));
        let own = settled(Some(&shell), &Settings { model: Some(sonnet.clone()), ..Settings::default() });
        assert_eq!(own["model"], json!({ "provider": "openrouter", "id": "anthropic/claude-sonnet-5.5" }));
        assert_eq!(own["color"], "#c2410c");
        let back = settled(Some(&own), &Settings { no_model: true, ..Settings::default() });
        assert_eq!(back, shell, "taking the model away leaves its tier");
        // replay: the same change twice is the same file
        assert_eq!(settled(Some(&own), &Settings { model: Some(sonnet), ..Settings::default() }), own);
        // not an object (a hand-written mistake): made one
        assert_eq!(settled(Some(&json!("cheap")), &Settings::default()), json!({ "tier": "cheap" }));
    }

    #[test]
    fn where_an_agents_model_calls_go() {
        assert_eq!(runs_on(None), RunsOn::Platform { tier: "cheap".into() });
        assert_eq!(runs_on(Some(&json!({ "tier": "medium" }))), RunsOn::Platform { tier: "medium".into() });
        let own = runs_on(Some(&json!({ "tier": "cheap", "model": { "provider": "openrouter", "id": "openai/gpt-6.1" } })));
        assert_eq!(own, RunsOn::Own { provider: "openrouter".into(), id: "openai/gpt-6.1".into() });
        assert!(own.said().contains("outside the platform"));
        assert_eq!(serde_json::to_value(&own).unwrap(), json!({ "on": "own", "provider": "openrouter", "id": "openai/gpt-6.1" }));
        // a model half named is no model: its tier
        assert_eq!(runs_on(Some(&json!({ "tier": "medium", "model": { "provider": "openrouter" } }))), RunsOn::Platform { tier: "medium".into() });
    }
}
