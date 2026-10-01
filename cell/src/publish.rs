//! A fragment's files from the platform (docs/phase-6.md, step 2): a
//! template as a new fragment's first commit and first deploy,
//! `POST /api/files` (one commit to `main`, as a CLI sync makes), and
//! `POST /api/deploy` (`live` to `main`'s tip, as `fragment deploy` does).
//! An agent's tools use the same two routes. Also the capabilities a page
//! can ask for: its owner's fragments, listed and made (`__fragments`),
//! and shown inside it, signed in (`__frame`). And, for a fragment's
//! owner, its template's latest files (`__template`).

use std::collections::BTreeMap;

use fragment_core::site;
use fragment_proto::{CreateFragment, ErrorBody, ErrorCode, IdentityKind, Role, Visibility};
use fragment_templates::{Template, BLANK, BUILDER, CALORIES, CHAT, DESKTOP, HERMES, INBOX, PET, TODO};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
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
/// double the cell. `builder` is also what an agent's hand-off makes.
/// `hermes` makes its owner's own Hermes on the fleet's sandcastle node
/// (docs/hermes-chat.md).
pub(crate) const TEMPLATES: [(&str, Template); 9] = [
    ("blank", BLANK),
    ("todo", TODO),
    ("inbox", INBOX),
    ("calories", CALORIES),
    ("pet", PET),
    ("builder", BUILDER),
    ("chat", CHAT),
    ("hermes", HERMES),
    ("desktop", DESKTOP),
];

/// `live` moving under a deploy this many times is an error.
const DEPLOY_ATTEMPTS: usize = 5;
/// What one `POST /api/files` may write in all: an editor's write (a
/// screenshot), larger than an app's (`limits::FILE_WRITE_MAX_BYTES`);
/// bigger files go through the CLI as blobs.
const API_WRITE_MAX_BYTES: usize = 1024 * 1024;
/// Fragments one `__presence` read may name: a desktop's open panes.
const PRESENCE_NAMES_MAX: usize = 8;

/// Who may open a fragment made from a template, when its create does not
/// say, from what the template declares (fragment.json holds no access): a
/// computer (awake on its owner's budget while a page is open) or any
/// capability (its owner's powers: `fragments`, `frame`; the desktop) make
/// it its owner's alone; anything else, whoever holds its link.
pub(crate) fn first_visibility(name: Option<&str>) -> Visibility {
    let m = name.and_then(template).map(manifest).unwrap_or_default();
    if m["computer"].is_object() || m["capabilities"].as_array().is_some_and(|c| !c.is_empty()) {
        Visibility::Members
    } else {
        Visibility::Link
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

/// A template's files as the fragment `name` holds them: its fragment.json
/// stamped with that name.
fn stamped(t: Template, name: &str) -> Vec<FileWrite> {
    t.iter()
        .map(|(path, bytes)| FileWrite { path: path.to_string(), bytes: Some(if *path == "fragment.json" { stamp(bytes, name) } else { bytes.to_vec() }) })
        .collect()
}

/// A template's bytes, hashed: what an update to them is keyed by.
fn template_hash(t: Template) -> String {
    let mut h = Sha256::new();
    for (path, bytes) in t {
        h.update(path.as_bytes());
        h.update([0]);
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
    }
    hex::encode(&h.finalize()[..8])
}

/// `__template`'s answer: the template, and the paths where live is not it.
fn template_answer(which: &str, changed: Vec<String>) -> Value {
    json!({ "template": which, "upToDate": changed.is_empty(), "changed": changed })
}

impl FragmentCell {
    /// Commits the template a fragment was made from (`template_pending`)
    /// and deploys it. Keyed by the fragment's incarnation, so the alarm
    /// can retry one that failed without committing twice.
    pub(crate) async fn seed(&self) -> CellResult<()> {
        let Some(which) = self.meta(MetaKey::TemplatePending)? else { return Ok(()) };
        let t = template(&which).ok_or_else(|| CellError::host(format!("no template {which}")))?;
        let (name, owner) = (self.name()?, self.must(MetaKey::Owner)?);
        let key = format!("template:{}", self.must(MetaKey::CreatedAt)?);
        if let Wrote::Conflict(why) = self.commit(&key, &stamped(t, &name), &BTreeMap::new(), &format!("start from the {which} template"), &owner, 0).await? {
            return Err(CellError::host(why));
        }
        self.go_live(&owner, &format!("deploy {name}")).await?;
        self.event("template", &format!("{name} starts from the {which} template"), json!({ "template": which }));
        self.set_meta(MetaKey::Template, &which)?;
        self.del_meta(MetaKey::TemplatePending)
    }

    /// The template this fragment was made from, while the platform still
    /// offers it: as `seed` kept it, or, for one made before it kept it, as
    /// its `template` event says (kept from then on).
    fn made_from(&self) -> CellResult<Option<(String, Template)>> {
        let which = match self.meta(MetaKey::Template)? {
            Some(which) => Some(which),
            None => {
                let rows = self.rows("SELECT body FROM records WHERE channel = 'events' AND kind = 'template' ORDER BY seq DESC LIMIT 1", vec![])?;
                let body = rows.first().and_then(|r| serde_json::from_str::<Value>(r["body"].as_str()?).ok()).unwrap_or_default();
                let logged = body["data"]["template"].as_str().map(str::to_string);
                if let Some(which) = &logged {
                    self.set_meta(MetaKey::Template, which)?;
                }
                logged
            }
        };
        Ok(which.and_then(|w| template(&w).map(|t| (w, t))))
    }

    /// The owner, when they are the caller (signed in on this origin, or
    /// with their key): a fragment's template is theirs to update, with no
    /// capability asked for.
    fn template_owner(&self, caller: &Caller) -> CellResult<String> {
        let owner = self.must(MetaKey::Owner)?;
        match caller.principal() {
            Some(p) if p == owner => Ok(owner),
            Some(_) => Err(CellError::new(ErrorCode::Forbidden, "only this fragment's owner updates it to its template")),
            None => Err(CellError::new(ErrorCode::Unauthenticated, "sign in as this fragment's owner to update it to its template")),
        }
    }

    /// The template's files that live does not hold as they are, by path:
    /// one absent, or of another size, without a read; the rest read from
    /// live and compared.
    async fn unlike_template(&self, t: Template, name: &str) -> CellResult<Vec<String>> {
        let mut facts = self.facts()?;
        self.ensure_pins(&mut facts).await?;
        let cs = self.cs()?;
        let (repo, live, cs) = (&facts.repo, facts.pin_live.as_deref(), &cs);
        let compared = stamped(t, name).into_iter().map(|w| async move {
            let want = w.bytes.expect("a template writes each of its files");
            let same = match (live, self.tree_row("live", &w.path)?) {
                (Some(live), Some(row)) if row.size == want.len() as u64 => cs.read(repo, live, &w.path, want.len()).await? == Some(want),
                _ => false,
            };
            Ok::<_, CellError>((!same).then_some(w.path))
        });
        Ok(futures_util::future::try_join_all(compared).await?.into_iter().flatten().collect())
    }

    /// `GET __template`: the template this fragment was made from, and
    /// whether live holds its latest files, to its owner alone (`{template:
    /// null}`: it was not made from one the platform offers).
    pub(crate) async fn template_status(&self, caller: &Caller) -> CellResult<Value> {
        self.template_owner(caller)?;
        let name = self.name()?;
        match self.made_from()? {
            Some((which, t)) => Ok(template_answer(&which, self.unlike_template(t, &name).await?)),
            None => Ok(json!({ "template": null })),
        }
    }

    /// `POST __template`: the template's latest files committed to main in
    /// one commit (fragment.json stamped; files it does not have stay), and
    /// deployed, for its owner; answers the new status. Keyed by the
    /// template and the live commit it replaces, so a retry commits once;
    /// a fragment up to date commits nothing.
    pub(crate) async fn template_update(&self, caller: &Caller) -> CellResult<Value> {
        let owner = self.template_owner(caller)?;
        let name = self.name()?;
        let (which, t) = self.made_from()?.ok_or_else(|| CellError::invalid(format!("{name} was not made from a template")))?;
        let changed = self.unlike_template(t, &name).await?;
        if changed.is_empty() {
            return Ok(template_answer(&which, changed));
        }
        let key = format!("template-update:{}:{}", template_hash(t), self.pin("live")?.unwrap_or_default());
        let message = format!("update to the latest {which} template");
        if let Wrote::Conflict(why) = self.commit(&key, &stamped(t, &name), &BTreeMap::new(), &message, &owner, 0).await? {
            return Err(CellError::host(why));
        }
        let live = self.go_live(&owner, &format!("deploy {name}: {message}")).await?;
        let summary = format!("{name} updates to the latest {which} template ({} of its files)", changed.len());
        self.event("template.update", &summary, json!({ "template": which, "changed": changed, "live": live }));
        Ok(template_answer(&which, self.unlike_template(t, &name).await?))
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
    ///
    /// `__share?name=` (`sheet`) is the same for the share sheet of one of
    /// the owner's own fragments, on the platform's origin: the redemption
    /// is for that sheet alone (`calls::sheet`), and the frame goes on to
    /// its `embed`, whose session opens that sheet, as the owner, in a
    /// frame of this origin only, and nothing else of the platform's.
    pub(crate) async fn frame_redirect(&self, caller: &Caller, name: &str, sheet: bool) -> CellResult<Response> {
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
        // a sheet's is only for a fragment the owner owns
        let theirs = |f: &Value| f["name"] == target.as_str() && (!sheet || f["role"] == "owner");
        if !listed["fragments"].as_array().into_iter().flatten().any(theirs) {
            let what = if sheet { "its owner's own" } else { "one of its owner's fragments" };
            return Err(CellError::new(ErrorCode::Forbidden, format!("{target} is not {what}")));
        }
        let mint = calls::MintFrame {
            token,
            frame,
            from: name.to_string(),
            owner,
            fragment: if sheet { calls::sheet(&target) } else { target.clone() },
            embedder: self.cfg.origin(&caller.url, name),
            return_to: site::return_path(asked("return").as_deref()),
        };
        let redeem = crate::ask_registry(&self.env, &mint).await?.redeem.ok_or_else(|| CellError::host("a frame's mint answered no redemption"))?;
        let to = match sheet {
            true => format!("{}{}/embed?token={redeem}", self.cfg.platform(&caller.url), calls::sheet(&target)),
            false => format!("{}__signin?token={redeem}", self.cfg.canonical(&caller.url, &target)),
        };
        let h = Headers::new();
        h.set("location", &to)?;
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

    /// `GET __presence?name=…`: who has each named fragment open now, for
    /// the desktop's panes. Each is asked as the owner and answers its own
    /// owner alone (`/api/presence`), so one that is not theirs, or cannot
    /// answer, is left out; a read wakes only the fragments it names.
    pub(crate) async fn owner_presence(&self, caller: &Caller) -> CellResult<Value> {
        self.owner_granted(caller)?;
        let who = caller.signed.as_ref().expect("the owner, granted, signed in");
        let mut names: Vec<String> = caller.url.query_pairs().filter(|(k, _)| k == "name").map(|(_, v)| v.into_owned()).collect();
        names.sort();
        names.dedup();
        if names.len() > PRESENCE_NAMES_MAX {
            return Err(CellError::invalid(format!("at most {PRESENCE_NAMES_MAX} fragments at once")));
        }
        if let Some(bad) = names.iter().find(|n| !fragment_proto::valid_fragment_name(n)) {
            return Err(CellError::invalid(format!("{bad:?} is not a fragment's name")));
        }
        let asked = names.iter().map(|name| crate::share::ask(&self.env, &caller.url, name, who, Method::Get, "/api/presence", None));
        let mut presence = serde_json::Map::new();
        for (name, answer) in names.iter().zip(futures_util::future::join_all(asked).await) {
            match answer {
                Ok(here) => {
                    presence.insert(name.clone(), here);
                }
                Err(e) if crate::auth::is_refusal(e.code) || e.code == ErrorCode::NotFound => {}
                Err(e) => console_error!("__presence: {name}: {:?} {}", e.code, e.message),
            }
        }
        Ok(json!({ "presence": presence }))
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
        let create = CreateFragment { name: text("label")?, visibility: None, template: Some(text("template")?), throwaway: false };
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
