//! A draft (docs/api.md, Drafts; its pure rules are
//! `fragment_core::drafts`): a fragment made by a key no one holds. It
//! keeps to its own loop (its files, deploys, operations, channels and
//! runs) and spends nothing, so it bills no one: no secrets, no storage
//! token or blobs, no subscriptions or push, no outbound fetch, no AI
//! steps, no preview card, and no ledger is asked. Its writes, files and
//! database are capped (`limits::DRAFT_*`), its page says it is a draft
//! (serve.rs), and its alarm ends it at its expiry as a delete does.
//!
//! Its claim, on the platform's page (share.rs), makes it the claimer's in
//! one turn: their membership as its owner, its maker's gone, their list's
//! row, and its meter rows waiting since it was made sent to their ledger
//! with the rest. From then it is an ordinary fragment of theirs, under
//! its own name.

use fragment_core::drafts::{self, Draft};
use fragment_core::npub;
use fragment_proto::{limits, CreateFragment, Created, DraftStatus, ErrorCode, IdentityKind, MakeDraft, Role, Visibility};
use serde_json::{json, Value};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::files::FileWrite;
use crate::fragment::{json_response, Caller, FragmentCell, MetaKey};
use crate::js;

impl FragmentCell {
    /// The draft this fragment was made as, claimed or not.
    fn kept_draft(&self) -> CellResult<Option<Draft>> {
        match self.meta(MetaKey::Draft)? {
            Some(text) => Ok(Some(serde_json::from_str(&text).map_err(|e| CellError::host(format!("the kept draft does not decode: {e}")))?)),
            None => Ok(None),
        }
    }

    /// This fragment's draft, while no one has claimed it.
    pub(crate) fn draft(&self) -> CellResult<Option<Draft>> {
        Ok(self.kept_draft()?.filter(|d| d.claimed_by.is_none()))
    }

    /// 403 while the fragment is a draft no one claimed: `what` waits for
    /// its claim.
    pub(crate) fn draft_refuses(&self, what: &str) -> CellResult<()> {
        match self.draft()? {
            Some(_) => Err(CellError::new(ErrorCode::Forbidden, format!("a draft {what} until it is claimed"))),
            None => Ok(()),
        }
    }

    /// A control request's gate (fragment.rs `route`): a draft's maker (a
    /// key no one holds) reaches its own unclaimed draft alone, and an
    /// unclaimed draft takes its own loop alone (`drafts::takes`).
    pub(crate) fn draft_gate(&self, caller: &Caller, method: &Method, route: &[&str]) -> CellResult<()> {
        let draft = self.draft()?;
        if caller.signed.as_ref().is_some_and(|s| drafts::is_maker(&s.id, s.key.as_deref())) {
            self.name()?;
            if !draft.as_ref().is_some_and(|d| Some(d.key.as_str()) == caller.key()) {
                return Err(CellError::new(
                    ErrorCode::Unauthenticated,
                    "this key belongs to no one: it acts on the draft it made alone, until that draft is claimed (then it is its claimer's key)",
                ));
            }
        }
        if draft.is_some() && !drafts::takes(method.as_ref(), route) {
            return Err(CellError::new(
                ErrorCode::Forbidden,
                "a draft keeps to its files, deploys, operations, channels and runs until it is claimed: secrets, storage tokens, blobs, subscriptions and sharing come with its claim",
            ));
        }
        Ok(())
    }

    /// `POST /draft` (the router's `POST /api/drafts`): the draft its maker's
    /// key names. The same key again answers the same draft; another
    /// template is a conflict.
    pub(crate) async fn make_draft(&self, caller: &Caller, name: &str, body: MakeDraft) -> CellResult<Response> {
        let key = caller.key().filter(|_| caller.signed.as_ref().is_some_and(|s| drafts::is_maker(&s.id, s.key.as_deref())));
        let key = key.ok_or_else(|| CellError::host("a draft is made by its maker's key"))?.to_string();
        if drafts::name(&key) != name || body.template.as_deref().is_some_and(|t| crate::publish::template(t).is_none()) {
            return Err(CellError::host("the router addressed another draft than its key names, or a template no draft starts from"));
        }
        if let (Some(_), Some(kept)) = (self.meta(MetaKey::CreatedAt)?, self.kept_draft()?) {
            if kept.claimed_by.is_some() {
                return Err(CellError::new(ErrorCode::AlreadyExists, format!("{name} was claimed: it is its claimer's")));
            }
            if kept.template != body.template {
                return Err(CellError::new(
                    ErrorCode::ConflictingBody,
                    format!("this key's draft is {name}, from {}: one key makes one draft (delete it to start another)", kept.template.as_deref().unwrap_or("no template")),
                ));
            }
            return json_response(&self.created(caller, Some(&kept))?);
        }
        let draft = Draft { key, until: js::now_ms() + limits::DRAFT_TTL_MS, code: drafts::code(js::random_bytes::<5>()), template: body.template.clone(), claimed_by: None };
        let create = CreateFragment { name: name.to_string(), visibility: Some(Visibility::Link), template: body.template, title: None };
        self.create(caller, create, Some(draft)).await
    }

    /// A created fragment's answer to its create, as it stands.
    pub(crate) fn created(&self, caller: &Caller, draft: Option<&Draft>) -> CellResult<Created> {
        let [npub, owner, view_token, inbox_token, repo] = self.metas([MetaKey::Npub, MetaKey::Owner, MetaKey::ViewToken, MetaKey::InboxToken, MetaKey::Repo])?;
        let missing = crate::fragment::missing;
        let name = self.name()?;
        Ok(Created {
            npub: npub.ok_or_else(|| missing(MetaKey::Npub))?,
            owner: owner.ok_or_else(|| missing(MetaKey::Owner))?,
            visibility: self.visibility()?,
            view_token: view_token.ok_or_else(|| missing(MetaKey::ViewToken))?,
            inbox_token: inbox_token.ok_or_else(|| missing(MetaKey::InboxToken))?,
            repo: repo.ok_or_else(|| missing(MetaKey::Repo))?,
            canonical: self.cfg.canonical(&caller.url, &name),
            draft: draft.map(|d| self.draft_status(&name, d, true)),
            name,
        })
    }

    /// What a status says of a draft: its end, and its claim link (with
    /// its code for its maker, `with_code`; the page asks anyone else).
    pub(crate) fn draft_status(&self, name: &str, draft: &Draft, with_code: bool) -> DraftStatus {
        let page = format!("{}/claim/{name}", self.cfg.platform());
        let claim = match with_code {
            true => format!("{page}?code={}", drafts::shown(&draft.code)),
            false => page,
        };
        DraftStatus { expires_at: draft.until, claim }
    }

    /// `GET /claim` (share.rs, the claim page): what claiming it would take.
    pub(crate) fn claim_view(&self) -> CellResult<Value> {
        let name = self.name()?;
        let draft = self.kept_draft()?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("{name} is no draft")))?;
        if draft.claimed_by.is_none() && draft.until <= js::now_ms() {
            return Err(CellError::new(ErrorCode::NotFound, format!("{name} ended: no one claimed it in time")));
        }
        Ok(json!({ "name": name, "key": npub::encode(&draft.key), "expiresAt": draft.until, "claimedBy": draft.claimed_by }))
    }

    /// `POST /claim {code}` (share.rs, as the signed-in person): the draft
    /// becomes theirs, in one turn. Theirs already, it answers the same; a
    /// draft someone else claimed, or past its end, is refused.
    pub(crate) async fn claim(&self, caller: &Caller, code: &str) -> CellResult<Value> {
        let name = self.name()?;
        let person = caller.signed.as_ref().filter(|s| s.kind == IdentityKind::Person && !drafts::is_maker(&s.id, s.key.as_deref()));
        let person = person.ok_or_else(|| CellError::new(ErrorCode::Forbidden, "a person claims a draft"))?.id.clone();
        let mut draft = self.kept_draft()?.ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("{name} is no draft")))?;
        let key = draft.key.clone();
        let answer = |claimed: bool| json!({ "name": name, "key": key, "claimed": claimed });
        match &draft.claimed_by {
            Some(by) if *by == person => return Ok(answer(false)),
            Some(_) => return Err(CellError::new(ErrorCode::AlreadyExists, format!("{name} was claimed by someone else"))),
            None => {}
        }
        let now = js::now_ms();
        if draft.until <= now {
            return Err(CellError::new(ErrorCode::NotFound, format!("{name} ended: no one claimed it in time")));
        }
        if !drafts::code_matches(&draft.code, code) {
            return Err(CellError::new(ErrorCode::Forbidden, "that is not this draft's claim code: it is in the link its maker gave"));
        }
        let maker = drafts::maker(&draft.key);
        // one turn, no await: the claimer owns it, its maker is no member
        self.exec("DELETE FROM members WHERE principal = ?", vec![maker.as_str().into()])?;
        self.exec(
            "INSERT INTO members (principal, role, added_by, added_at, kind) VALUES (?, 'owner', ?, ?, 'person')",
            vec![person.as_str().into(), person.as_str().into(), SqlStorageValue::Integer(now)],
        )?;
        self.set_meta(MetaKey::Owner, &person)?;
        draft.claimed_by = Some(person.clone());
        self.set_meta(MetaKey::Draft, &serde_json::to_string(&draft).expect("a draft serializes"))?;
        self.index_change(&person, Some(Role::Owner))?;
        self.event("draft.claimed", &format!("{name} is {person}'s, who claimed it"), json!({ "by": person, "maker": maker }));
        self.flush_index().await;
        self.schedule().await?;
        Ok(answer(true))
    }

    /// `writable`'s part on an unclaimed draft: its database's cap, and its
    /// writes a minute (counted in this activation, as the public budget is).
    pub(crate) fn draft_writes(&self) -> CellResult<()> {
        let size = self.sql().database_size() as u64;
        if size >= limits::DRAFT_STORAGE_MAX_BYTES {
            return Err(CellError::new(
                ErrorCode::StorageFull,
                format!("a draft keeps at most {} MiB of records, runs and events until it is claimed", limits::DRAFT_STORAGE_MAX_BYTES >> 20),
            ));
        }
        if !self.draft_rate.borrow_mut().allow("draft", js::now_ms()) {
            return Err(CellError::new(ErrorCode::RateLimited, format!("a draft takes at most {} writes a minute until it is claimed", limits::DRAFT_WRITES_PER_MIN)));
        }
        Ok(())
    }

    /// A commit's part on an unclaimed draft (files.rs `commit`): main's
    /// files, as they would be after `writes`, fit its cap.
    pub(crate) fn draft_files_fit(&self, writes: &[FileWrite]) -> CellResult<()> {
        if self.draft()?.is_none() {
            return Ok(());
        }
        // bounded: a commit names at most a template's files, or FILE_WRITES_MAX
        let marks = vec!["?"; writes.len()].join(", ");
        let paths = writes.iter().map(|w| w.path.as_str().into()).collect();
        let kept = self.count_of(&format!("SELECT COALESCE(SUM(size), 0) AS n FROM tree WHERE ref = 'main' AND path NOT IN ({marks})"), paths)?;
        let written: u64 = writes.iter().filter_map(|w| w.bytes.as_ref()).map(|b| b.len() as u64).sum();
        if kept + written > limits::DRAFT_FILES_MAX_BYTES {
            return Err(CellError::too_large("a draft's files", (kept + written) as usize, limits::DRAFT_FILES_MAX_BYTES as usize));
        }
        Ok(())
    }

    /// When an unclaimed draft ends (the alarm's, fragment.rs `arm`).
    pub(crate) fn draft_due_at(&self) -> CellResult<Option<i64>> {
        Ok(self.draft()?.map(|d| d.until))
    }

    /// From the alarm: an unclaimed draft past its end ends as a delete
    /// ends it (ended.rs), its repo with it (its life's alone). Whether it did.
    pub(crate) async fn end_expired_draft(&self) -> CellResult<bool> {
        let Some(draft) = self.draft()?.filter(|d| d.until <= js::now_ms()) else { return Ok(false) };
        let name = self.name()?;
        let ended = self.end_life_wiped()?;
        console_log!("{}", json!({ "event": "draft.expired", "fragment": name, "until": draft.until }));
        self.tell_ended(Some(&ended.owner)).await;
        Ok(true)
    }
}
