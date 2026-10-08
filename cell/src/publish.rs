//! A fragment's files from the platform (docs/api.md, Control API): a
//! template as a new fragment's first commit and first deploy,
//! `POST /api/files` (one commit to `main`, as a CLI sync makes), and
//! `POST /api/deploy` (`live` to `main`'s tip, as `fragment deploy` does).
//! The shell uses the same two routes (an agent's settings, a rename).

use std::collections::BTreeMap;

use fragment_proto::{ErrorCode, Role};
use fragment_templates::{blessed, Template, BLANK, CALORIES, INBOX, TODO, BOARD, WALL, WHEN};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::files::{content_of, FileWrite, Wrote};
use crate::fragment::{json_response, Caller, FragmentCell, MetaKey};
use crate::js;

/// The templates a fragment can start from, the simplest first (a create
/// that names none of them lists them in this order). `notes` stays with
/// the CLI (`fragment new --template notes`): at 3 MiB it would double the
/// cell.
pub(crate) const TEMPLATES: [(&str, Template); 7] = [("blank", BLANK), ("todo", TODO), ("inbox", INBOX), ("calories", CALORIES), ("when", WHEN), ("wall", WALL), ("board", BOARD)];

/// `live` moving under a deploy this many times is an error.
const DEPLOY_ATTEMPTS: usize = 5;
/// What one `POST /api/files` may write in all: an editor's write (an
/// image or a document), larger than an app's (`limits::FILE_WRITE_MAX_BYTES`);
/// bigger files go through the CLI as blobs.
const API_WRITE_MAX_BYTES: usize = 1024 * 1024;

pub(crate) fn template(name: &str) -> Option<Template> {
    TEMPLATES.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

/// The template's fragment.json with the fragment's own name in it.
fn stamp(bytes: &[u8], name: &str) -> Vec<u8> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(mut o)) => {
            o.insert("name".into(), name.into());
            serde_json::to_vec_pretty(&Value::Object(o)).expect("a manifest serializes")
        }
        _ => bytes.to_vec(),
    }
}

/// A template's files as the fragment `name` holds them: its fragment.json
/// stamped with that name.
fn stamped(t: Template, name: &str) -> Vec<FileWrite> {
    t.iter().map(|(path, bytes)| FileWrite { path: path.to_string(), bytes: Some(if *path == "fragment.json" { stamp(bytes, name) } else { bytes.to_vec() }) }).collect()
}

impl FragmentCell {
    /// Commits the template a fragment was made from (`template_pending`)
    /// and deploys it. Keyed by the fragment's incarnation, so the alarm
    /// can retry one that failed without committing twice.
    ///
    /// One at a time: the create seeds, and the alarm it arms meanwhile
    /// (its index flush) seeds too. On code.storage, a round trip away, the
    /// two met: the second commit, changing nothing, was refused (412), and
    /// when that was the create's, it answered before its template was
    /// live (a skills fragment listing none of the release). Under the
    /// lock the second finds the template landed, and does nothing.
    pub(crate) async fn seed(&self) -> CellResult<()> {
        let _seeding = self.seeding.lock().await;
        let Some(which) = self.meta(MetaKey::TemplatePending)? else { return Ok(()) };
        let (name, owner) = (self.name()?, self.must(MetaKey::Owner)?);
        let key = format!("template:{}", self.must(MetaKey::CreatedAt)?);
        let files = match (template(&which), blessed::template(&which)) {
            (Some(t), _) => stamped(t, &name),
            // a blessed template is named, not copied: the release serves it
            (None, Some(_)) => {
                let mut manifest = json!({ "template": which });
                if let Some(title) = self.meta(MetaKey::TemplateTitle)? {
                    manifest["meta"] = json!({ "title": title });
                }
                let bytes = serde_json::to_vec_pretty(&manifest).expect("a manifest serializes");
                vec![FileWrite { path: "fragment.json".into(), bytes: Some(bytes) }]
            }
            (None, None) => return Err(CellError::host(format!("no template {which}"))),
        };
        if let Wrote::Conflict(why) = self.commit(&key, &files, &BTreeMap::new(), &format!("start from the {which} template"), &owner, 0).await? {
            return Err(CellError::host(why));
        }
        self.go_live(&owner, &format!("deploy {name}")).await?;
        self.event("template", &format!("{name} starts from the {which} template"), json!({ "template": which }));
        self.del_meta(MetaKey::TemplateTitle)?;
        self.del_meta(MetaKey::TemplatePending)
    }

    /// Moves `live` to `main`'s tip: the first deploy makes the branch,
    /// later ones fast-forward it (after a rollback, restore commits and a
    /// merge: `Cs::promote_live`), each guarded against a `live` that moved
    /// meanwhile. Answers the new tip.
    pub(crate) async fn go_live(&self, principal: &str, message: &str) -> CellResult<String> {
        let repo = self.must(MetaKey::Repo)?;
        let cs = self.cs()?;
        let main = cs.branch_head(&repo, "main").await?.ok_or_else(|| CellError::invalid("nothing to deploy: main has no commits"))?;
        let (author, email) = crate::files::author(principal);
        for _ in 0..DEPLOY_ATTEMPTS {
            let moved = match cs.branch_head(&repo, "live").await? {
                None => cs.create_branch(&repo, &main, "live").await?,
                Some(tip) if tip == main => Some(tip),
                Some(_) => {
                    // Held across the steps: a refresh or a poll between them
                    // waits, then pins the second, so the files live
                    // holds between them are never served.
                    let _held = self.plane.lock().await;
                    cs.promote_live(&repo, message, (&author, &email)).await?
                }
            };
            if let Some(tip) = moved {
                // the pin follows now; failing that, the poll backstop
                if let Err(e) = self.interpret(&["live"]).await {
                    self.event("deploy.refresh-failed", &e.message, json!({ "live": tip }));
                    self.may_lag().await?;
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
        self.writable().await?;
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
        self.writable().await?;

        let who = self.caller_id(caller)?.to_string();
        let name = self.name()?;
        let message = format!("deploy {name}{}", body["note"].as_str().map(|n| format!(": {n}")).unwrap_or_default());
        let live = self.go_live(&who, &message).await?;
        json_response(&json!({ "live": live, "canonical": self.cfg.canonical(&caller.url, &name) }))
    }
}
