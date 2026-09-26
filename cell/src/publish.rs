//! A fragment's files from the platform (docs/phase-6.md, step 2): a
//! template as a new fragment's first commit and first deploy,
//! `POST /api/files` (one commit to `main`, as a CLI sync makes), and
//! `POST /api/deploy` (`live` to `main`'s tip, as `fragment deploy` does).
//! An agent's tools use the same two routes. Also the capabilities a page
//! can ask for: its owner's fragments, listed and made (`__fragments`),
//! and shown inside it, signed in (`__frame`).

use std::collections::BTreeMap;

use fragment_core::site;
use fragment_proto::{CreateFragment, ErrorBody, ErrorCode, IdentityKind, Role, Visibility};
use fragment_templates::{Template, BLANK, CALORIES, CHAT, DESKTOP, INBOX, PET, TODO};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::files::{content_of, FileWrite, Wrote};
use crate::fragment::{json_response, Caller, FragmentCell, MetaKey};
use crate::js;
use crate::registry::calls;
use crate::routed::{Credential, Signed};

/// The templates a fragment can start from, in the order the home page
/// offers them: the simplest first, the desktop (a demo) last. `notes`
/// stays with the CLI (`fragment new --template notes`): at 3 MiB it would
/// double the cell.
pub(crate) const TEMPLATES: [(&str, Template); 7] =
    [("blank", BLANK), ("todo", TODO), ("inbox", INBOX), ("calories", CALORIES), ("pet", PET), ("chat", CHAT), ("desktop", DESKTOP)];

/// `live` moving under a deploy this many times is an error.
const DEPLOY_ATTEMPTS: usize = 5;
/// What one `POST /api/files` may write in all: an editor's write (a
/// screenshot), larger than an app's (`limits::FILE_WRITE_MAX_BYTES`);
/// bigger files go through the CLI as blobs.
const API_WRITE_MAX_BYTES: usize = 1024 * 1024;

/// Who may open a fragment made from `template`, when its create does not
/// say: a desktop is its owner's alone; anything else, whoever holds its
/// link. (fragment.json holds no access, so a template cannot say.)
pub(crate) fn first_visibility(template: Option<&str>) -> Visibility {
    match template {
        Some("desktop") => Visibility::Members,
        _ => Visibility::Link,
    }
}

pub(crate) fn template(name: &str) -> Option<Template> {
    TEMPLATES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// A template's fragment.json.
fn manifest(t: Template) -> Value {
    t.iter().find(|(p, _)| *p == "fragment.json").and_then(|(_, b)| serde_json::from_slice(b).ok()).unwrap_or_default()
}

/// A template's title and description, from its fragment.json.
pub(crate) fn describe(t: Template) -> (String, String) {
    let meta = manifest(t)["meta"].clone();
    let text = |k: &str| meta[k].as_str().unwrap_or_default().to_string();
    (text("title"), text("description"))
}

/// Whether the template named `name` asks to show its owner's fragments
/// inside it (`frame`): the new-fragment form says so, and making one there
/// is its owner's grant (auth.rs).
pub(crate) fn frames(name: &str) -> bool {
    template(name).is_some_and(|t| manifest(t)["capabilities"].as_array().is_some_and(|c| c.iter().any(|c| c == "frame")))
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
        let Some(which) = self.meta(MetaKey::TemplatePending)? else { return Ok(()) };
        let t = template(&which).ok_or_else(|| CellError::host(format!("no template {which}")))?;
        let (name, owner) = (self.name()?, self.must(MetaKey::Owner)?);
        let writes: Vec<FileWrite> = t
            .iter()
            .map(|(path, bytes)| FileWrite { path: path.to_string(), bytes: Some(if *path == "fragment.json" { stamp(bytes, &name) } else { bytes.to_vec() }) })
            .collect();
        let key = format!("template:{}", self.must(MetaKey::CreatedAt)?);
        if let Wrote::Conflict(why) = self.commit(&key, &writes, &BTreeMap::new(), &format!("start from the {which} template"), &owner, 0).await? {
            return Err(CellError::host(why));
        }
        self.go_live(&owner, &format!("deploy {name}")).await?;
        self.event("template", &format!("{name} starts from the {which} template"), json!({ "template": which }));
        self.del_meta(MetaKey::TemplatePending)
    }

    /// Moves `live` to `main`'s tip: the first deploy makes the branch,
    /// later ones fast-forward it (or merge, after a rollback), each
    /// guarded against a `live` that moved meanwhile. Answers the new tip.
    pub(crate) async fn go_live(&self, principal: &str, message: &str) -> CellResult<String> {
        let repo = self.must(MetaKey::Repo)?;
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
    /// message?, key?}`: one commit to `main`, for editors (at most 16
    /// files and 1 MiB). A `key` makes a retry commit nothing twice.
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
        if writes.len() > fragment_proto::limits::FILE_WRITES_MAX {
            return Err(CellError::invalid(format!("at most {} files per write", fragment_proto::limits::FILE_WRITES_MAX)));
        }
        let total: usize = writes.iter().filter_map(|w| w.bytes.as_ref().map(Vec::len)).sum();
        if total > API_WRITE_MAX_BYTES {
            return Err(CellError::too_large("the files written", total, API_WRITE_MAX_BYTES));
        }
        let message = body["message"].as_str().map_or_else(|| format!("write {} file(s)", writes.len()), str::to_string);
        let key = format!("api:{who}:{}", body["key"].as_str().map_or_else(js::random_hex::<16>, str::to_string));
        match self.commit(&key, &writes, &BTreeMap::new(), &message, &who, 0).await? {
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

    /// Whether live's fragment.json asks for `capability`.
    fn declares(&self, capability: &str) -> CellResult<bool> {
        let caps: Vec<String> = self.meta(MetaKey::CapabilitiesLive)?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Ok(caps.iter().any(|c| c == capability))
    }

    /// `frame`, when live asks for it: whether its owner allows it (`1`; a
    /// stop is `0`).
    pub(crate) fn framing(&self) -> CellResult<Option<bool>> {
        if !self.declares("frame")? {
            return Ok(None);
        }
        match self.meta(MetaKey::FrameGranted)?.as_deref() {
            Some("1") => Ok(Some(true)),
            Some("0") | None => Ok(Some(false)),
            Some(other) => Err(CellError::host(format!("the stored frame grant {other:?}"))),
        }
    }

    /// The owner, when they are the one viewing a page whose fragment.json
    /// (at live) asks for the `fragments` capability. Anyone else is
    /// refused, even an editor.
    fn owner_granted(&self, caller: &Caller) -> CellResult<String> {
        let owner = self.must(MetaKey::Owner)?;
        if caller.principal() != Some(owner.as_str()) {
            return Err(CellError::new(ErrorCode::Forbidden, "only this fragment's owner, signed in here, has its fragments"));
        }
        if !self.declares("fragments")? {
            return Err(CellError::new(ErrorCode::Forbidden, "this fragment's fragment.json does not ask for the fragments capability"));
        }
        Ok(owner)
    }

    /// The fragments `owner` belongs to, as their Principal cell lists them.
    async fn listed(&self, owner: &str) -> CellResult<Value> {
        let list = Request::new("https://principal.internal/list", Method::Get)?;
        Ok(self.env.durable_object("PRINCIPAL")?.get_by_name(owner)?.fetch_with_request(list).await?.json().await?)
    }

    /// `PUT /api/grants/frame {granted}`: the owner lets this fragment show
    /// their fragments inside its page (`__frame`), or stops it.
    pub(crate) fn grant_frame(&self, caller: &Caller, body: Value) -> CellResult<Response> {
        if caller.principal() != Some(self.must(MetaKey::Owner)?.as_str()) {
            return Err(CellError::new(ErrorCode::Forbidden, "only its owner lets a fragment show their fragments inside it"));
        }
        let granted = body["granted"].as_bool().ok_or_else(|| CellError::invalid("granted is true or false"))?;
        self.set_meta(MetaKey::FrameGranted, if granted { "1" } else { "0" })?;
        let summary = if granted { "its owner lets it show their fragments inside it" } else { "its owner stopped it showing their fragments inside it" };
        self.event("grant.frame", summary, json!({ "granted": granted }));
        json_response(&json!({ "frame": granted }))
    }

    /// `__frame?name=&return=`: this page's frame, signed in on one of its
    /// owner's fragments as them (docs/fragment-boats.md, decision 2). Only
    /// for its owner, signed in here, when live asks for `frame` and they
    /// allow it, and only for a fragment in their list. The registry mints
    /// a frame redemption from this origin's own session, for that fragment
    /// in a frame of this origin only, and the frame goes on to that
    /// fragment's `__signin`: this page's code never holds it (the router
    /// takes `__frame` only as a frame of this origin's own page).
    pub(crate) async fn frame_redirect(&self, caller: &Caller, name: &str) -> CellResult<Response> {
        let why = match self.framing()? {
            Some(true) => None,
            Some(false) => Some(format!("its owner has not let it show their fragments inside it: they allow it in its share sheet ({}/share/{name})", self.cfg.platform(&caller.url))),
            None => Some("this fragment's fragment.json does not ask for the frame capability".to_string()),
        };
        if let Some(why) = why {
            return Err(CellError::new(ErrorCode::Forbidden, why));
        }
        let (token, frame) = match &caller.unresolved {
            Some(Credential::Session(t)) => (t.clone(), false),
            Some(Credential::Frame(t)) => (t.clone(), true),
            _ => return Err(CellError::new(ErrorCode::Unauthenticated, "only this fragment's owner, signed in here, shows their fragments inside it")),
        };
        let asked = |k: &str| caller.url.query_pairs().find(|(q, _)| q == k).map(|(_, v)| v.into_owned());
        let target = asked("name").filter(|n| fragment_proto::valid_fragment_name(n)).ok_or_else(|| CellError::invalid("name a fragment"))?;
        let owner = self.must(MetaKey::Owner)?;
        let listed = self.listed(&owner).await?;
        if !listed["fragments"].as_array().into_iter().flatten().any(|f| f["name"] == target.as_str()) {
            return Err(CellError::new(ErrorCode::Forbidden, format!("{target} is not one of its owner's fragments")));
        }
        let mint = calls::MintFrame {
            token,
            frame,
            from: name.to_string(),
            owner,
            fragment: target.clone(),
            embedder: self.cfg.origin(&caller.url, name),
            return_to: site::return_path(asked("return").as_deref()),
        };
        let redeem = crate::ask_registry(&self.env, &mint).await?.redeem.ok_or_else(|| CellError::host("a frame's mint answered no redemption"))?;
        let h = Headers::new();
        h.set("location", &format!("{}__signin?token={redeem}", self.cfg.canonical(&caller.url, &target)))?;
        h.set("cache-control", "no-store")?;
        h.set("referrer-policy", "no-referrer")?;
        Ok(Response::empty()?.with_status(302).with_headers(h))
    }

    /// `GET __fragments`: the fragments the owner belongs to (a read asks
    /// the owner's Principal cell alone, and wakes none of them), and
    /// whether this page may show them inside it (`frame`).
    pub(crate) async fn owner_fragments(&self, caller: &Caller) -> CellResult<Value> {
        let owner = self.owner_granted(caller)?;
        let v = self.listed(&owner).await?;
        let fragments: Vec<Value> = v["fragments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| {
                let name = f["name"].as_str()?;
                Some(json!({ "name": name, "role": f["role"], "url": self.cfg.canonical(&caller.url, name), "sharing": f["sharing"] }))
            })
            .collect();
        // without it, the desktop says so in place of its panes
        Ok(json!({ "fragments": fragments, "frame": self.framing()? }))
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
        let identity = fragment_proto::Identity { id: owner, kind: IdentityKind::Person, owner: None, username: Some(username.to_string()) };
        let signer = Signed::new(identity, None);
        let mut made = crate::create_fragment(&self.env, self.cfg, &caller.url, create, signer).await?;
        let status = made.status_code();
        let v: Value = made.json().await?;
        if status != 200 {
            let e: ErrorBody = serde_json::from_value(v).map_err(|e| CellError::host(format!("the create answered {status}: {e}")))?;
            return Err(CellError::new(e.error, e.message));
        }
        Ok(json!({ "name": v["name"], "url": v["canonical"] }))
    }
}
