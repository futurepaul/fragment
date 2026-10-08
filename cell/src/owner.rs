//! A fragment's jobs acting as its owner on the owner's other fragments
//! (docs/api.md, Jobs: `job.owner.fragments`, `job.owner.call`). The
//! platform lends this only to a blessed template's release that declares
//! the `owner` capability, and only while no one but the owner reads or
//! drives that fragment (`fragment_core::access::owner_lent`, asked at
//! each step).
//!
//! The asking fragment's step reads its owner's list (their `Principal`
//! cell) and asks each fragment on it behind an internal route (`owner/…`,
//! marked by `OWNER_HEADER`, which the router never passes):
//!
//!   POST /owner/ops   {owner}   → {role, operations}: the described
//!                     operations the owner may call there, by the tools'
//!                     rule (`fragment_core::mcp::described`)
//!   POST /owner/call  {owner, from, key, op, id, input, depth}
//!                     → {result, replayed}: one of them, as the owner
//!
//! Each reaches only a fragment the owner is a member of, with the role
//! they hold there. A call is recorded as an agent's call `for` someone is
//! (decision R17): the app's `call.principal` is the owner, so what it does
//! is theirs, and its ledger, records, runs and `ops` name the asking
//! fragment's key (`call.agent`); `events` names the asking fragment
//! (`fragment.called`), as a connected client's call names the client. Its
//! id is the asking run's and step's (`steps::owner_call_id`), so a step
//! tried again, or its run replayed, applies nothing twice.

use fragment_core::access::{self, Purpose, Standing};
use fragment_core::manifest::Capability;
use fragment_core::steps::{self, OWNER_FRAGMENTS_MAX};
use fragment_core::{mcp, npub};
use fragment_proto::{valid_fragment_name, valid_op_id, ErrorCode, ListedFragment, OpKind, Role, Via, Visibility};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{CellError, CellResult};
use crate::fragment::{decide, FragmentCell, MetaKey};
use crate::jobs::{permanent, RunRow, StepFail};
use crate::ops::{Invocation, JOB_ID_PREFIX};

/// Set on the `owner/…` calls of another fragment's step; the router never sets or passes it.
pub const OWNER_HEADER: &str = "x-fragment-owner";
/// The fragments one `owner.fragments` step asks at once.
const ASKED_AT_ONCE: usize = 6;

/// `POST /owner/ops`
#[derive(Deserialize)]
struct OpsAsk {
    owner: String,
}

/// `POST /owner/call`
#[derive(Deserialize)]
struct CallAsk {
    owner: String,
    /// The asking fragment's name and key.
    from: String,
    key: String,
    op: String,
    id: String,
    input: Value,
    depth: u32,
}

fn retry(e: CellError) -> StepFail {
    StepFail::Retry(e.message)
}

impl FragmentCell {
    /// The owner whose reach this fragment's jobs are lent, or why not
    /// (`access::owner_lent`), read at each step: a share or a deploy since
    /// the last one is seen.
    fn owner_lent(&self) -> Result<String, StepFail> {
        let [blessed, capabilities, owner] = self.metas([MetaKey::Blessed, MetaKey::Capabilities, MetaKey::Owner]).map_err(retry)?;
        let owner = owner.ok_or_else(|| StepFail::Retry("the fragment has no owner".into()))?;
        let declares = capabilities.and_then(|c| serde_json::from_str::<Vec<String>>(&c).ok()).is_some_and(|c| c.iter().any(|c| c == Capability::Owner.as_str()));
        let visibility = self.facts().map_err(retry)?.visibility;
        let others = self
            .count_of("SELECT COUNT(*) AS n FROM members WHERE principal != ? AND (owner IS NULL OR owner != ?)", vec![owner.as_str().into(), owner.as_str().into()])
            .map_err(retry)?;
        access::owner_lent(blessed.is_some(), declares, visibility, others).map_err(|why| permanent(format!("job.owner: {why}")))?;
        Ok(owner)
    }

    /// `job.owner.fragments()`: the owner's other fragments, by name, at
    /// most `OWNER_FRAGMENTS_MAX`, each `{name, title, kind, role, url,
    /// operations}`. One the owner no longer reaches is left out; one that
    /// cannot answer now tries the whole step again.
    pub(crate) async fn step_owner_fragments(&self) -> Result<Value, StepFail> {
        let owner = self.owner_lent()?;
        let me = self.name().map_err(retry)?;
        let listed = crate::listed(&self.env, &owner).await.map_err(retry)?;
        let mut theirs: Vec<ListedFragment> = listed.fragments.into_iter().filter(|f| f.name != me).collect();
        theirs.sort_by(|a, b| a.name.cmp(&b.name));
        theirs.truncate(OWNER_FRAGMENTS_MAX);
        let (env, body) = (&self.env, json!({ "owner": owner }));
        let asked: Vec<(ListedFragment, CellResult<Value>)> = futures_util::stream::iter(theirs)
            .map(|f| {
                let body = &body;
                async move {
                    let answer = crate::fragment::ask(env, &f.name, "owner/ops", body).await;
                    (f, answer)
                }
            })
            .buffered(ASKED_AT_ONCE)
            .collect()
            .await;
        let mut fragments = Vec::with_capacity(asked.len());
        for (f, answer) in asked {
            match answer {
                Ok(a) => fragments.push(json!({
                    "name": f.name,
                    "title": f.title,
                    "kind": f.kind,
                    "role": a["role"],
                    "url": format!("{}/", self.cfg.outside_origin(&f.name)),
                    "operations": a["operations"],
                })),
                Err(e) if matches!(e.code, ErrorCode::HostFailed | ErrorCode::UpstreamFailed) => return Err(StepFail::Retry(format!("{}: {}", f.name, e.message))),
                // gone, or no longer theirs: not one of their fragments now
                Err(_) => {}
            }
        }
        Ok(json!({ "fragments": fragments }))
    }

    /// `job.owner.call(fragment, op, input)`: `{result, url}`.
    pub(crate) async fn step_owner_call(&self, run: &RunRow, index: u32, fragment: &str, op: &str, input: Value) -> Result<Value, StepFail> {
        let input = steps::owner_call(fragment, op, &input).map_err(permanent)?;
        let owner = self.owner_lent()?;
        let me = self.name().map_err(retry)?;
        if fragment == me {
            return Err(permanent(format!("{fragment} is this fragment: a job calls its own operations with job.call")));
        }
        let id = steps::owner_call_id(&self.run_key(run.id).map_err(retry)?, index);
        let key = self.own_key().map_err(retry)?;
        let body = json!({ "owner": owner, "from": me, "key": key, "op": op, "id": id, "input": input, "depth": run.depth + 1 });
        match crate::fragment::ask(&self.env, fragment, "owner/call", &body).await {
            Ok(a) => Ok(json!({ "result": a["result"], "url": format!("{}/", self.cfg.outside_origin(fragment)) })),
            Err(e) => match e.code {
                ErrorCode::HostFailed | ErrorCode::UpstreamFailed | ErrorCode::NodeFull => Err(StepFail::Retry(format!("{fragment} {op}: {}", e.message))),
                _ => Err(permanent(format!("{fragment} {op}: {}", e.message))),
            },
        }
    }

    /// `POST /owner/<route>`: another fragment's step, as its owner here.
    pub(crate) async fn owner_route(&self, route: &str, body: &[u8]) -> CellResult<Value> {
        fn decode<'a, T: Deserialize<'a>>(body: &'a [u8]) -> CellResult<T> {
            serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("an owner call: {e}")))
        }
        // what runs here is the release this build serves, as a routed request's is
        if let Err(e) = self.blessed_current().await {
            self.event("blessed.install-failed", &e.message, json!({ "code": e.code }));
        }
        match route {
            "ops" => self.owner_ops(&decode::<OpsAsk>(body)?.owner),
            "call" => self.owner_call(decode(body)?).await,
            _ => Err(CellError::new(ErrorCode::NotFound, format!("no route /owner/{route}"))),
        }
    }

    /// The owner's standing here as themselves, which is a membership: an
    /// owner step reaches their own fragments, never one by its visibility.
    fn owners_standing(&self, owner: &str) -> CellResult<(Visibility, Standing)> {
        if !npub::is_identity(owner) {
            return Err(CellError::host("an owner call names an identity"));
        }
        let visibility = self.facts()?.visibility;
        let standing = self.own_standing(owner, false)?;
        if standing.member.is_none() {
            return Err(CellError::new(ErrorCode::Forbidden, "its owner is no member here: an owner step reaches its owner's own fragments"));
        }
        Ok((visibility, standing))
    }

    /// `POST /owner/ops`: the owner's role here, and the described
    /// operations they may call (a query to read; a mutation or a job to act).
    fn owner_ops(&self, owner: &str) -> CellResult<Value> {
        let (visibility, standing) = self.owners_standing(owner)?;
        let role = decide(visibility, standing, Purpose::Read, Role::Public)?;
        let operations = mcp::described(&self.operations()?, |d| {
            let purpose = if d.kind == OpKind::Query { Purpose::Read } else { Purpose::Act };
            decide(visibility, standing, purpose, d.role).is_ok()
        });
        Ok(json!({ "role": role, "operations": operations }))
    }

    /// `POST /owner/call`: one described operation, as the owner, with the
    /// checks a call from outside meets (role, schema, the overdraft).
    async fn owner_call(&self, ask: CallAsk) -> CellResult<Value> {
        if !valid_fragment_name(&ask.from) || !npub::is_hex_key(&ask.key) || !valid_op_id(&ask.id) || !ask.id.starts_with(JOB_ID_PREFIX) {
            return Err(CellError::host("an owner call names its fragment, its key and a job's id"));
        }
        let (visibility, standing) = self.owners_standing(&ask.owner)?;
        let decl = self.declared(&ask.op)?;
        if !mcp::served(&decl, true) {
            return Err(CellError::new(ErrorCode::UnknownOperation, mcp::not_served(&ask.op, &decl, true)));
        }
        let acts = decl.kind != OpKind::Query;
        let role = decide(visibility, standing, if acts { Purpose::Act } else { Purpose::Read }, decl.role)?;
        if acts {
            self.writable().await?;
        }
        let inv = Invocation {
            principal: &ask.key,
            asker: Some(&ask.owner),
            role,
            op: &ask.op,
            decl,
            id: ask.id.clone(),
            input: ask.input,
            depth: ask.depth,
            via: Via::Call,
            trigger: None,
        };
        let answered = self.invoke(inv).await?;
        if acts && !answered.replayed {
            let owner = npub::display(&ask.owner);
            let summary = format!("{} {} by {owner} through {}", ask.op, ask.id, ask.from);
            self.event("fragment.called", &summary, json!({ "op": ask.op, "id": ask.id, "principal": owner, "fragment": ask.from, "key": npub::display(&ask.key) }));
        }
        self.launch_queued().await;
        Ok(json!({ "result": answered.result, "replayed": answered.replayed }))
    }
}
