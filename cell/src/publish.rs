//! A fragment's files from the platform (docs/phase-6.md, step 2): a
//! template as a new fragment's first commit and first deploy,
//! `POST /api/files` (one commit to `main`, as a CLI sync makes), and
//! `POST /api/deploy` (`live` to `main`'s tip, as `fragment deploy` does).
//! An agent's tools use the same two routes. Also `__fragments`, the one
//! capability a page can ask for: its owner's fragments, listed and made.

use std::collections::BTreeMap;

use fragment_proto::{CreateFragment, ErrorBody, ErrorCode, IdentityKind, Role};
use fragment_templates::{Template, BLANK, CHAT, DESKTOP, INBOX, TODO};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::files::{content_of, FileWrite, Wrote};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::{js, Signer};

/// The templates a fragment can start from. `notes` stays with the CLI
/// (`fragment new --template notes`): at 3 MiB it would double the cell.
pub(crate) const TEMPLATES: [(&str, Template); 5] = [("desktop", DESKTOP), ("chat", CHAT), ("todo", TODO), ("inbox", INBOX), ("blank", BLANK)];

/// `live` moving under a deploy this many times is an error.
const DEPLOY_ATTEMPTS: usize = 5;

pub(crate) fn template(name: &str) -> Option<Template> {
    TEMPLATES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// A template's title and description, from its fragment.json.
pub(crate) fn describe(t: Template) -> (String, String) {
    let meta = t
        .iter()
        .find(|(p, _)| *p == "fragment.json")
        .and_then(|(_, b)| serde_json::from_slice::<Value>(b).ok())
        .map(|v| v["meta"].clone())
        .unwrap_or_default();
    let text = |k: &str| meta[k].as_str().unwrap_or_default().to_string();
    (text("title"), text("description"))
}

/// The template's fragment.json with the fragment's own name in it.
fn stamp(bytes: &[u8], name: &str) -> Vec<u8> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(mut o)) => {
            o.insert("name".into(), name.into());
            serde_json::to_vec_pretty(&o).expect("a manifest serializes")
        }
        _ => bytes.to_vec(),
    }
}

impl FragmentCell {
    /// Commits the template a fragment was made from (`template_pending`)
    /// and deploys it. Keyed by the fragment's incarnation, so the alarm
    /// can retry one that failed without committing twice.
    pub(crate) async fn seed(&self) -> CellResult<()> {
        let Some(which) = self.meta("template_pending")? else { return Ok(()) };
        let t = template(&which).ok_or_else(|| CellError::host(format!("no template {which}")))?;
        let (name, owner) = (self.name()?, self.must("owner")?);
        let writes: Vec<FileWrite> = t
            .iter()
            .map(|(path, bytes)| FileWrite { path: path.to_string(), bytes: Some(if *path == "fragment.json" { stamp(bytes, &name) } else { bytes.to_vec() }) })
            .collect();
        let key = format!("template:{}", self.must("created_at")?);
        if let Wrote::Conflict(why) = self.commit(&key, &writes, &BTreeMap::new(), &format!("start from the {which} template"), &owner, 0).await? {
            return Err(CellError::host(why));
        }
        self.go_live(&owner, &format!("deploy {name}")).await?;
        self.event("template", &format!("{name} starts from the {which} template"), json!({ "template": which }));
        self.del_meta("template_pending")
    }

    /// Moves `live` to `main`'s tip: the first deploy makes the branch,
    /// later ones fast-forward it (or merge, after a rollback), each
    /// guarded against a `live` that moved meanwhile. Answers the new tip.
    pub(crate) async fn go_live(&self, principal: &str, message: &str) -> CellResult<String> {
        let repo = self.must("repo")?;
        let cs = self.cs()?;
        let main = cs.branch_head(&repo, "main").await?.ok_or_else(|| CellError::invalid("nothing to deploy: main has no commits"))?;
        let (author, email) = crate::files::author(principal);
        for _ in 0..DEPLOY_ATTEMPTS {
            let moved = match cs.branch_head(&repo, "live").await? {
                None => cs.create_branch(&repo, &main, "live").await?,
                Some(tip) if tip == main => Some(tip),
                Some(tip) => cs.promote_live(&repo, &tip, message, (&author, &email)).await?,
            };
            if let Some(tip) = moved {
                // the pin follows now (the webhook and the poll would, later)
                if let Err(e) = self.interpret(&["live"]).await {
                    self.event("deploy.refresh-failed", &e.message, json!({ "live": tip }));
                }
                return Ok(tip);
            }
        }
        Err(CellError::new(ErrorCode::UpstreamFailed, format!("live kept moving under {DEPLOY_ATTEMPTS} deploys; try again")))
    }

    /// `POST /api/files {files: [{path, text | base64 | delete: true}],
    /// message?, key?}`: one commit to `main`, for editors. A `key` makes a
    /// retry commit nothing twice.
    pub(crate) async fn write_files_api(&self, caller: &Caller, body: Value) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let who = self.caller_id(caller)?.to_string();
        let files = body["files"].as_array().filter(|f| !f.is_empty()).ok_or_else(|| CellError::invalid("files: a list of {path, text | base64 | delete}"))?;
        let mut writes = Vec::with_capacity(files.len());
        for f in files {
            let path = f["path"].as_str().ok_or_else(|| CellError::invalid("each file has a path"))?.to_string();
            let bytes = match f["delete"].as_bool() {
                Some(true) => None,
                _ => Some(content_of(f).map_err(|e| CellError::invalid(format!("{path}: {e}")))?),
            };
            writes.push(FileWrite { path, bytes });
        }
        let message = body["message"].as_str().map_or_else(|| format!("write {} file(s)", writes.len()), str::to_string);
        let key = format!("api:{who}:{}", body["key"].as_str().map_or_else(js::random_hex::<16>, str::to_string));
        match self.commit_files(&key, &writes, &BTreeMap::new(), &message, &who, 0).await? {
            Wrote::Commit(sha) => json_response(&json!({ "commit": sha })),
            // nothing was expected, so nothing can conflict
            Wrote::Conflict(why) => Err(CellError::host(why)),
        }
    }

    /// `POST /api/deploy {note?}`: `live` to `main`'s tip, for editors.
    pub(crate) async fn deploy_api(&self, caller: &Caller, body: Value) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let who = self.caller_id(caller)?.to_string();
        let name = self.name()?;
        let message = format!("deploy {name}{}", body["note"].as_str().map(|n| format!(": {n}")).unwrap_or_default());
        let live = self.go_live(&who, &message).await?;
        json_response(&json!({ "live": live, "canonical": self.cfg.canonical(&caller.url, &name) }))
    }

    /// The owner, when they are the one viewing a page whose fragment.json
    /// (at live) asks for the `fragments` capability. Anyone else is
    /// refused, even an editor.
    fn owner_granted(&self, caller: &Caller) -> CellResult<String> {
        let owner = self.must("owner")?;
        if caller.principal.as_deref() != Some(owner.as_str()) {
            return Err(CellError::new(ErrorCode::Forbidden, "only this fragment's owner, signed in here, has its fragments"));
        }
        let caps: Vec<String> = self.meta("capabilities_live")?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if !caps.iter().any(|c| c == "fragments") {
            return Err(CellError::new(ErrorCode::Forbidden, "this fragment's fragment.json does not ask for the fragments capability"));
        }
        Ok(owner)
    }

    /// `GET __fragments`: the fragments the owner belongs to.
    pub(crate) async fn owner_fragments(&self, caller: &Caller) -> CellResult<Value> {
        let owner = self.owner_granted(caller)?;
        let list = Request::new("https://principal.internal/list", Method::Get)?;
        let v: Value = self.env.durable_object("PRINCIPAL")?.get_by_name(&owner)?.fetch_with_request(list).await?.json().await?;
        let fragments: Vec<Value> = v["fragments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| {
                let name = f["name"].as_str()?;
                Some(json!({ "name": name, "role": f["role"], "url": self.cfg.canonical(&caller.url, name) }))
            })
            .collect();
        Ok(json!({ "fragments": fragments }))
    }

    /// `POST __fragments {label, template}`: makes `<label>.<username>` from
    /// a template for the owner, as `POST /api/fragments` would for them
    /// (this fragment's username is its owner's: people create under their
    /// own).
    pub(crate) async fn owner_create(&self, caller: &Caller, body: Value) -> CellResult<Value> {
        let owner = self.owner_granted(caller)?;
        let name = self.name()?;
        let (_, username) = fragment_proto::split_fragment_name(&name).ok_or_else(|| CellError::host(format!("{name} is not <label>.<username>")))?;
        let text = |k: &str| body[k].as_str().map(str::to_string).ok_or_else(|| CellError::invalid(format!("{k} is a string")));
        let create = CreateFragment { name: text("label")?, visibility: None, template: Some(text("template")?) };
        let signer = Signer { key: None, id: owner, kind: IdentityKind::Person, owner: None, username: Some(username.to_string()) };
        let mut made = crate::create_fragment(&self.env, &self.cfg, &caller.url, create, signer).await?;
        let status = made.status_code();
        let v: Value = made.json().await?;
        if status != 200 {
            let e: ErrorBody = serde_json::from_value(v).map_err(|e| CellError::host(format!("the create answered {status}: {e}")))?;
            return Err(CellError::new(e.error, e.message));
        }
        Ok(json!({ "name": v["name"], "url": v["canonical"] }))
    }
}
