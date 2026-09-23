//! The file plane (docs/api.md): every file is in the fragment's
//! code.storage repo. The cell pins `main` (the working copy) and `live`
//! (what is served and whose `app.mjs` runs), keeps a tree index of each
//! pin (metadata only), and moves a pin when a signed push webhook, an
//! editor's `refresh`, or the poll backstop sees its branch move. Editors
//! write to git directly with a storage token; deploy, preview, and
//! rollback are ref moves the CLI makes.

use std::collections::BTreeMap;

use fragment_core::codestorage::TreeEntry;
use fragment_core::{manifest, npub, site, webhook};
use fragment_proto::{limits, valid_repo_path, ErrorCode, OpDecl, Role};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::*;

use crate::cs::Cs;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::js;

/// Platform code that runs in the facet around the author's App class.
pub const PLATFORM_JS: &str = include_str!("../platform.mjs");
/// Webhook deliveries remembered for deduplication.
const DELIVERIES_KEPT: i64 = 10_000;
const DELIVERY_TTL_MS: i64 = 7 * 24 * 3600 * 1000;
/// Tree rows per insert statement.
const TREE_BATCH: usize = 20;
pub const REFS: [&str; 2] = ["main", "live"];

pub struct PinMove {
    pub changed: bool,
    pub to: Option<String>,
    pub paths: Vec<String>,
}

impl FragmentCell {
    pub(crate) fn cs(&self) -> CellResult<Cs<'_>> {
        Ok(Cs::new(self.cfg.codestorage()?))
    }

    pub(crate) fn pin(&self, which: &str) -> CellResult<Option<String>> {
        self.meta(&format!("pin_{which}"))
    }

    pub(crate) fn tree_row(&self, which: &str, path: &str) -> CellResult<Option<Value>> {
        Ok(self.rows("SELECT path, size, mode, last_commit FROM tree WHERE ref = ? AND path = ?", vec![which.into(), path.into()])?.into_iter().next())
    }

    pub(crate) fn tree_rows(&self, which: &str) -> CellResult<Vec<Value>> {
        self.rows("SELECT path, size, mode, last_commit FROM tree WHERE ref = ? ORDER BY path", vec![which.into()])
    }

    /// Reads the branch head and, when it moved, the new tree; the manifest
    /// (main) or the code (live) follow. Callers hold `self.plane`.
    async fn refresh_pin(&self, which: &str) -> CellResult<PinMove> {
        let repo = self.must("repo")?;
        let cs = self.cs()?;
        let from = self.pin(which)?;
        let Some(to) = cs.branch_head(&repo, which).await? else {
            if from.is_none() {
                return Ok(PinMove { changed: false, to: None, paths: vec![] });
            }
            self.exec("DELETE FROM tree WHERE ref = ?", vec![which.into()])?;
            self.del_meta(&format!("pin_{which}"))?;
            self.event("git.refresh", &format!("{which}: the branch is gone; pin cleared"), json!({ "ref": which, "from": from }));
            return Ok(PinMove { changed: true, to: None, paths: vec![] });
        };
        if from.as_deref() == Some(to.as_str()) {
            return Ok(PinMove { changed: false, to: Some(to), paths: vec![] });
        }
        let entries = cs.tree(&repo, &to).await?;
        let prior: BTreeMap<String, (u64, String)> = self
            .tree_rows(which)?
            .into_iter()
            .map(|r| (r["path"].as_str().unwrap_or("").to_string(), (r["size"].as_u64().unwrap_or(0), r["last_commit"].as_str().unwrap_or("").to_string())))
            .collect();
        let mut paths: Vec<String> = entries
            .iter()
            .filter(|e| prior.get(&e.path).is_none_or(|(size, last)| *size != e.size || *last != e.last_commit_sha))
            .map(|e| e.path.clone())
            .collect();
        let now: std::collections::BTreeSet<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        paths.extend(prior.keys().filter(|p| !now.contains(p.as_str())).cloned());
        self.write_tree(which, &entries)?;
        self.set_meta(&format!("pin_{which}"), &to)?;
        self.event(
            "git.refresh",
            &format!("{which}: {} → {} ({} files, {} changed)", short(from.as_deref()), short(Some(&to)), entries.len(), paths.len()),
            json!({ "ref": which, "from": from, "to": to, "changed": paths.iter().take(50).collect::<Vec<_>>() }),
        );
        Ok(PinMove { changed: true, to: Some(to), paths })
    }

    fn write_tree(&self, which: &str, entries: &[TreeEntry]) -> CellResult<()> {
        self.exec("DELETE FROM tree WHERE ref = ?", vec![which.into()])?;
        for batch in entries.chunks(TREE_BATCH) {
            let tuples = vec!["(?, ?, ?, ?, ?)"; batch.len()].join(", ");
            let mut binds: Vec<SqlStorageValue> = Vec::with_capacity(batch.len() * 5);
            for e in batch {
                binds.extend([which.into(), e.path.as_str().into(), SqlStorageValue::Integer(e.size as i64), e.mode.as_str().into(), e.last_commit_sha.as_str().into()]);
            }
            self.exec(&format!("INSERT INTO tree (ref, path, size, mode, last_commit) VALUES {tuples}"), binds)?;
        }
        Ok(())
    }

    /// What follows a pin: the manifest from main, the code from live.
    /// Each records the commit it was read from, so a read that failed
    /// (code.storage down) is retried by the next refresh, webhook, or poll
    /// even though the pin itself already moved.
    async fn follow(&self, which: &str) -> CellResult<()> {
        let pin = self.pin(which)?;
        let done_key = format!("{which}_read_at");
        if self.meta(&done_key)? == pin {
            return Ok(());
        }
        match (which, pin.as_deref()) {
            ("main", Some(sha)) => self.read_manifest(&self.must("repo")?, sha).await?,
            ("main", None) => self.del_meta("manifest_main")?,
            (_, live) => self.install_code(live).await?,
        }
        match pin {
            Some(sha) => self.set_meta(&done_key, &sha),
            None => self.del_meta(&done_key),
        }
    }

    /// Keeps `fragment.json` at main for `GET manifest`, and says when it
    /// asks for access the platform no longer takes from git.
    async fn read_manifest(&self, repo: &str, sha: &str) -> CellResult<()> {
        if self.tree_row("main", "fragment.json")?.is_none() {
            return self.del_meta("manifest_main");
        }
        let Some(bytes) = self.cs()?.read(repo, sha, "fragment.json", limits::MANIFEST_MAX_BYTES).await? else { return self.del_meta("manifest_main") };
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => self.set_meta("manifest_main", &v.to_string())?,
            Err(e) => {
                self.del_meta("manifest_main")?;
                self.event("manifest.invalid", &format!("fragment.json at main is not JSON: {e}"), json!({ "sha": sha }));
                return Ok(());
            }
        }
        if let Ok(m) = manifest::parse(&bytes) {
            if !m.ignored.is_empty() {
                self.event(
                    "manifest.ignored",
                    &format!("fragment.json's {} no longer grant access: use `fragment members` and `fragment visibility`", m.ignored.join(", ")),
                    json!({ "keys": m.ignored }),
                );
            }
        }
        Ok(())
    }

    /// Installs the app from the live commit: `fragment.json` declares the
    /// operations and `app.mjs` is the code. A live commit without
    /// `app.mjs` has no app; one with an invalid manifest keeps the last
    /// good code and records why.
    async fn install_code(&self, live: Option<&str>) -> CellResult<()> {
        let repo = self.must("repo")?;
        let Some(sha) = live else {
            self.exec("DELETE FROM code", vec![])?;
            self.del_meta("meta_live")?;
            self.del_meta("code_error")?;
            js::abort_app_facet(&self.raw, "live is gone")?;
            return Ok(());
        };
        let cs = self.cs()?;
        let manifest = match self.tree_row("live", "fragment.json")? {
            None => manifest::Manifest::default(),
            Some(_) => {
                let bytes = cs.read(&repo, sha, "fragment.json", limits::MANIFEST_MAX_BYTES).await?.unwrap_or_default();
                match manifest::parse(&bytes) {
                    Ok(m) => m,
                    Err(e) => return self.code_refused(sha, &format!("fragment.json: {e}")),
                }
            }
        };
        match &manifest.meta {
            Some(meta) => self.set_meta("meta_live", &serde_json::to_string(meta).expect("meta serializes"))?,
            None => self.del_meta("meta_live")?,
        }
        if self.tree_row("live", "app.mjs")?.is_none() {
            self.exec("DELETE FROM code", vec![])?;
            self.del_meta("code_error")?;
            js::abort_app_facet(&self.raw, "live has no app.mjs")?;
            self.event("code.none", &format!("live {} has no app.mjs", short(Some(sha))), json!({ "sha": sha }));
            return Ok(());
        }
        let source = match cs.read(&repo, sha, "app.mjs", limits::SOURCE_MAX_BYTES).await {
            Ok(Some(b)) => match String::from_utf8(b) {
                Ok(s) => s,
                Err(_) => return self.code_refused(sha, "app.mjs is not UTF-8"),
            },
            Ok(None) => return self.code_refused(sha, "app.mjs vanished between the listing and the read"),
            Err(e) if e.code == ErrorCode::TooLarge => return self.code_refused(sha, &e.message),
            Err(e) => return Err(e),
        };
        let loader_id = format!("app:{}", hex::encode(Sha256::digest(format!("{PLATFORM_JS}\0{source}"))));
        let operations = serde_json::to_string(&manifest.operations).expect("operations serialize");
        self.exec(
            "INSERT INTO code (id, sha, loader_id, source, operations, cpu_ms, installed_at) VALUES (1, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET sha = excluded.sha, loader_id = excluded.loader_id, source = excluded.source,
               operations = excluded.operations, cpu_ms = excluded.cpu_ms, installed_at = excluded.installed_at",
            vec![
                sha.into(),
                loader_id.into(),
                source.into(),
                operations.into(),
                SqlStorageValue::Integer(limits::APP_CPU_MS.into()),
                SqlStorageValue::Integer(js::now_ms()),
            ],
        )?;
        self.del_meta("code_error")?;
        js::abort_app_facet(&self.raw, "new code from live")?;
        self.event(
            "code.installed",
            &format!("app.mjs from live {} ({} operations)", short(Some(sha)), manifest.operations.len()),
            json!({ "sha": sha, "operations": manifest.operations.keys().collect::<Vec<_>>() }),
        );
        Ok(())
    }

    fn code_refused(&self, sha: &str, why: &str) -> CellResult<()> {
        self.set_meta("code_error", &format!("live {}: {why}", short(Some(sha))))?;
        self.event("code.refused", &format!("live {} not installed: {why}", short(Some(sha))), json!({ "sha": sha }));
        Ok(())
    }

    /// Refreshes both pins; an external move of main notifies the change feed.
    pub(crate) async fn interpret(&self, refs: &[&str]) -> CellResult<Vec<(String, PinMove)>> {
        let _held = self.plane.lock().await;
        let mut out = vec![];
        for which in refs {
            let moved = self.refresh_pin(which).await?;
            if *which == "main" && moved.changed {
                self.broadcast_change(moved.to.as_deref(), &moved.paths);
            }
            self.follow(which).await?;
            out.push((which.to_string(), moved));
        }
        Ok(out)
    }

    /// Pins read before either branch has been announced (a fragment
    /// whose first push predates its webhook) are fetched on first use.
    pub(crate) async fn ensure_pins(&self) -> CellResult<()> {
        let missing: Vec<&str> = REFS.into_iter().filter(|w| self.pin(w).ok().flatten().is_none()).collect();
        if !missing.is_empty() {
            self.interpret(&missing).await?;
        }
        Ok(())
    }

    fn broadcast_change(&self, sha: Option<&str>, paths: &[String]) {
        let frame = json!({ "type": "changed", "ref": "main", "sha": sha, "paths": paths.iter().take(200).collect::<Vec<_>>() }).to_string();
        for ws in self.state.get_websockets_with_tag("watch") {
            let _ = ws.send_with_str(&frame);
        }
    }

    /// The backstop for lost webhooks, from the alarm.
    pub(crate) async fn poll(&self) {
        if let Err(e) = self.interpret(&REFS).await {
            self.event("git.poll-failed", &e.message, json!({ "code": e.code }));
        }
    }

    pub(crate) async fn refresh(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let moved = self.interpret(&REFS).await?;
        let refs: serde_json::Map<String, Value> = moved
            .into_iter()
            .map(|(which, m)| {
                let v = match m.to {
                    Some(pin) => json!({ "pin": pin, "moved": m.changed }),
                    None => json!({ "absent": true }),
                };
                (which, v)
            })
            .collect();
        json_response(&json!({ "ok": true, "refs": refs }))
    }

    /// A code.storage push delivery: validate, then remember (redeliveries
    /// are acknowledged, not interpreted twice), then interpret. The pin
    /// moves to the branch's head as read now, so a late or out-of-order
    /// delivery never moves it backwards.
    pub(crate) async fn webhook(&self, event: &str, signature: &str, body: &[u8]) -> CellResult<Response> {
        self.name()?;
        let secret = self.must("webhook_secret")?;
        if let Err(why) = webhook::verify(body, signature, &secret, js::now_ms() / 1000, limits::WEBHOOK_WINDOW_S) {
            self.event("webhook.rejected", &format!("{event}: {why}"), Value::Null);
            return Err(CellError::new(ErrorCode::Unauthenticated, why));
        }
        let payload: Value = serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("webhook body: {e}")))?;
        let Some(push) = webhook::parse_push(event, &payload) else {
            return json_response(&json!({ "ok": true, "ignored": event }));
        };
        let key = webhook::dedupe_key(event, &payload);
        if !self.rows("SELECT key FROM deliveries WHERE key = ?", vec![key.as_str().into()])?.is_empty() {
            self.event("webhook.redelivery", &format!("{}: {} (already seen)", push.branch, short(Some(&push.after))), Value::Null);
            return json_response(&json!({ "ok": true, "interpreted": false, "redelivery": true }));
        }
        let now = js::now_ms();
        self.exec("INSERT INTO deliveries (key, at) VALUES (?, ?)", vec![key.into(), SqlStorageValue::Integer(now)])?;
        self.exec("DELETE FROM deliveries WHERE at < ?", vec![SqlStorageValue::Integer(now - DELIVERY_TTL_MS)])?;
        self.exec(
            "DELETE FROM deliveries WHERE key NOT IN (SELECT key FROM deliveries ORDER BY at DESC LIMIT ?)",
            vec![SqlStorageValue::Integer(DELIVERIES_KEPT)],
        )?;
        self.event(
            "webhook.push",
            &format!("{}: {} → {}", push.branch, short(Some(&push.before)), short(Some(&push.after))),
            json!({ "ref": push.branch }),
        );
        if !REFS.contains(&push.branch.as_str()) {
            return json_response(&json!({ "ok": true, "interpreted": false, "redelivery": false }));
        }
        self.interpret(&[push.branch.as_str()]).await?;
        json_response(&json!({ "ok": true, "interpreted": true, "redelivery": false }))
    }

    pub(crate) fn storage_token(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let who = self.caller_hex(caller)?;
        let repo = self.must("repo")?;
        let token = self.cs()?.storage_token(&repo, who)?;
        self.event(
            "storage-token.minted",
            &format!("{} → repo {repo}, git:read+git:write, {}s", npub::encode(who), limits::STORAGE_TOKEN_TTL_S),
            json!({ "actor": npub::encode(who), "repo": repo, "expiresAt": token.expires_at }),
        );
        json_response(&token)
    }

    pub(crate) fn manifest(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        match self.meta("manifest_main")? {
            Some(text) => json_response(&serde_json::from_str::<Value>(&text).map_err(|e| CellError::host(e.to_string()))?),
            None => Err(CellError::new(ErrorCode::NotFound, "main has no fragment.json")),
        }
    }

    pub(crate) async fn files(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        self.ensure_pins().await?;
        let files: Vec<Value> = self
            .tree_rows("main")?
            .into_iter()
            .map(|r| {
                let path = r["path"].as_str().unwrap_or("").to_string();
                json!({ "path": path, "size": r["size"], "mode": r["mode"], "lastCommitSha": r["last_commit"], "machinery": site::is_machinery(&path) })
            })
            .collect();
        json_response(&json!({ "ref": self.pin("main")?, "files": files }))
    }

    /// Streams a file from a pin (`main` for the API, `live` for the site).
    pub(crate) async fn stream_file(&self, which: &str, path: &str) -> CellResult<Response> {
        let pin = self.pin(which)?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no {which} pin yet")))?;
        let upstream = self.cs()?.stream(&self.must("repo")?, &pin, path).await?;
        let headers = Headers::new();
        headers.set("content-type", site::mime_for_path(path))?;
        headers.set("x-fragment-ref", &pin)?;
        if let Ok(Some(len)) = upstream.headers().get("content-length") {
            headers.set("content-length", &len)?;
        }
        let (_, body) = upstream.into_parts();
        Ok(Response::from_body(body)?.with_headers(headers))
    }

    pub(crate) async fn file(&self, caller: &Caller, path: &str) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        if !valid_repo_path(path) {
            return Err(CellError::invalid("path must be a relative repo path"));
        }
        self.ensure_pins().await?;
        if self.tree_row("main", path)?.is_none() {
            return Err(CellError::new(ErrorCode::NotFound, format!("no file {path} at main")));
        }
        let mut resp = self.stream_file("main", path).await?;
        resp.headers_mut().set("cache-control", "no-store")?;
        Ok(resp)
    }

    pub(crate) async fn stat(&self, caller: &Caller, path: &str) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        if !valid_repo_path(path) {
            return Err(CellError::invalid("path must be a relative repo path"));
        }
        self.ensure_pins().await?;
        let pin = self.pin("main")?;
        let absent = json!({ "path": path, "size": 0, "blobSha": "", "lastCommitSha": "", "present": false });
        let stat = match (&pin, self.tree_row("main", path)?) {
            (Some(pin), Some(row)) => match self.cs()?.head(&self.must("repo")?, pin, path).await? {
                Some(h) => json!({ "path": path, "size": h.size, "blobSha": h.blob_sha, "lastCommitSha": h.last_commit_sha, "present": true }),
                None => json!({ "path": path, "size": row["size"], "blobSha": "", "lastCommitSha": row["last_commit"], "present": true }),
            },
            _ => absent,
        };
        json_response(&json!({ "stat": stat, "ref": pin }))
    }

    /// The installed operations (from the live commit).
    pub(crate) fn operations(&self) -> CellResult<Option<BTreeMap<String, OpDecl>>> {
        let rows = self.rows("SELECT operations FROM code WHERE id = 1", vec![])?;
        Ok(rows.first().map(|r| serde_json::from_str(r["operations"].as_str().unwrap_or("{}")).expect("stored operations parse")))
    }
}

fn short(sha: Option<&str>) -> String {
    sha.map(|s| s.chars().take(12).collect()).unwrap_or_else(|| "(none)".into())
}
