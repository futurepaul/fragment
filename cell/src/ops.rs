//! Calling an operation (docs/MODEL.md, Operations): the supervisor checks
//! the caller's role against the operation's declared role, bounds the
//! input, and runs it in the `app` facet, where a mutation and its ledger
//! row commit together. Callers who hold only the `public` floor (visitors
//! to a public fragment) are rate limited. A job is not run here: calling
//! one records a run and starts its Workflow (`jobs.rs`).

use std::collections::BTreeMap;

use fragment_core::npub;
use fragment_proto::{canonical_json, limits, valid_op_id, ErrorCode, OpCall, OpDecl, OpKind, OpResult, Role};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::*;

use crate::channels::Effect;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::jobs::NewRun;
use crate::js::{self, AppCode};
use crate::plane::PLATFORM_JS;

/// Operation ids a job's steps use; callers cannot choose them.
pub const JOB_ID_PREFIX: &str = "job:";

/// One admitted call: who runs which operation, and how deep in a chain of
/// triggered runs it is.
pub(crate) struct Invocation<'a> {
    /// Who the ledger records: an identity, an anonymous visitor, or the fragment itself (its key).
    pub principal: &'a str,
    pub role: Role,
    pub op: &'a str,
    pub decl: OpDecl,
    /// The caller's operation id.
    pub id: String,
    pub input: Value,
    pub depth: u32,
    /// How a job started this way is recorded: `call`, or `job` with its parent run.
    pub via: &'a str,
    pub trigger: Option<String>,
}

pub(crate) fn input_sha(op: &str, input: &Value) -> String {
    hex::encode(Sha256::digest(format!("{op}\n{}", canonical_json(input))))
}

impl FragmentCell {
    /// The running app facet for the installed code.
    pub(crate) fn facet(&self) -> CellResult<js::Facet> {
        let rows = self.rows("SELECT loader_id, source, modules, cpu_ms FROM code WHERE id = 1", vec![])?;
        let row = rows.into_iter().next().ok_or_else(|| CellError::new(ErrorCode::NoCode, "the live commit has no app.mjs (deploy one)"))?;
        let cpu_ms = row["cpu_ms"].as_u64().expect("code.cpu_ms is INTEGER");
        assert!(cpu_ms > 0 && cpu_ms <= limits::APP_CPU_MS as u64, "stored cpu_ms is within the limit");
        let modules: BTreeMap<String, String> = serde_json::from_str(row["modules"].as_str().unwrap_or("{}")).expect("stored modules parse");
        // One loaded worker per fragment: its env holds this fragment's
        // capabilities, and its module state is this fragment's alone.
        let id = format!("{}:{}", row["loader_id"].as_str().expect("code.loader_id is TEXT"), self.must("npub")?);
        js::app_facet(
            &self.raw,
            self.env.as_ref(),
            &self.name()?,
            &AppCode {
                id: &id,
                platform: PLATFORM_JS,
                source: row["source"].as_str().expect("code.source is TEXT"),
                modules: &modules,
                cpu_ms: cpu_ms as u32,
                subrequests: limits::APP_SUBREQUESTS,
            },
        )
    }

    /// `POST /api/f/<name>/ops/<op>`: a signed caller.
    pub(crate) async fn api_op(&self, caller: &Caller, op: &str, body: OpCall) -> CellResult<Response> {
        let who = self.caller_id(caller)?.to_string();
        let result = self.call_op(caller, &who, false, op, body).await?;
        json_response(&result)
    }

    /// The declared operation, or why there is none.
    pub(crate) fn declared(&self, op: &str) -> CellResult<OpDecl> {
        self.operations()?
            .ok_or_else(|| CellError::new(ErrorCode::NoCode, "the live commit has no app.mjs (deploy one)"))?
            .remove(op)
            .ok_or_else(|| CellError::new(ErrorCode::UnknownOperation, format!("no operation named {op:?}")))
    }

    /// Checks and runs one call from outside. `principal` is who the ledger
    /// records (an identity, or an anonymous visitor's id); `link` says the
    /// caller holds the share link.
    pub(crate) async fn call_op(&self, caller: &Caller, principal: &str, link: bool, op: &str, body: OpCall) -> CellResult<OpResult> {
        // Whether the caller can see the fragment at all comes before
        // anything about its operations.
        self.require(caller, link, Role::Public)?;
        let decl = self.declared(op)?;
        // a query reads (docs/MODEL.md), so an agent's owner may ask it; a
        // mutation or a job acts, which takes a membership of one's own
        let role = match decl.kind {
            OpKind::Query => self.require(caller, link, decl.role)?,
            _ => self.require_to_act(caller, link, decl.role)?,
        };
        let is_member = caller.principal.as_deref().map(|p| self.member_role(p)).transpose()?.flatten().is_some();
        if role == Role::Public && !is_member && !self.rate.borrow_mut().allow(principal, js::now_ms()) {
            return Err(CellError::new(ErrorCode::RateLimited, "too many public calls this minute; retry shortly"));
        }
        if !valid_op_id(&body.id) || body.id.starts_with(JOB_ID_PREFIX) {
            return Err(CellError::invalid("operation id must match ^[A-Za-z0-9._:-]{1,128}$ and not start with job:"));
        }
        let inv = Invocation { principal, role, op, decl, id: body.id, input: body.input, depth: 0, via: "call", trigger: None };
        let result = self.invoke(inv).await?;
        self.launch_queued().await;
        Ok(result)
    }

    /// Runs an admitted call: a query or a mutation in the facet, or a job's
    /// run recorded (the caller launches queued runs afterwards).
    pub(crate) async fn invoke(&self, inv: Invocation<'_>) -> CellResult<OpResult> {
        let input_text = canonical_json(&inv.input);
        if input_text.len() > limits::INPUT_MAX_BYTES {
            return Err(CellError::too_large("operation input", input_text.len(), limits::INPUT_MAX_BYTES));
        }
        if let Some(schema) = &inv.decl.input {
            fragment_core::schema::validate(schema, &inv.input).map_err(|e| CellError::invalid(format!("input {e}")))?;
        }
        let input_sha = input_sha(inv.op, &inv.input);
        if inv.decl.kind == OpKind::Job {
            let started = self.start_run(NewRun {
                op: inv.op,
                via: inv.via,
                trigger: inv.trigger,
                principal: inv.principal,
                role: inv.role,
                depth: inv.depth,
                call: Some((&inv.id, &input_sha)),
                input: inv.input,
            })?;
            return Ok(OpResult { result: json!({ "run": started.id, "status": started.status }), replayed: started.replayed });
        }
        let facet = self.facet()?;
        self.sweep(&facet).await?;
        // Ids are the caller's: the ledger keys them by principal, so one
        // caller can neither replay nor block another's id.
        let ledger_id = format!("{}/{}", inv.principal, inv.id);
        let meta = json!({
            "principal": npub::display(inv.principal),
            "role": inv.role,
            "channels": self.declared_channels()?.keys().collect::<Vec<_>>(),
        });
        let answer = match inv.decl.kind {
            OpKind::Query => facet.call("__query", &[inv.op.into(), inv.input.clone(), meta]).await,
            _ => facet.call("__mutate", &[ledger_id.as_str().into(), inv.op.into(), input_sha.as_str().into(), inv.input.clone(), meta]).await,
        }
        .map_err(|m| CellError::new(ErrorCode::AppFailed, m))?;
        match answer["error"].as_str() {
            Some("conflicting_body") => {
                return Err(CellError::new(ErrorCode::ConflictingBody, "this operation id was already used with a different input"))
            }
            Some("unknown_operation") => return Err(CellError::new(ErrorCode::UnknownOperation, format!("the app has no method {:?}", inv.op))),
            Some(other) => return Err(CellError::host(format!("the facet's platform code answered {other:?}"))),
            None => {}
        }
        let result = OpResult {
            result: answer["result"].clone(),
            replayed: answer["replayed"].as_bool().expect("the platform answers `replayed`"),
        };
        if inv.decl.kind == OpKind::Mutation {
            // The facet committed; its effects apply now (again, harmlessly, on a replay).
            let effects: Vec<Effect> = serde_json::from_value(answer["effects"].clone()).map_err(|e| CellError::host(format!("effects: {e}")))?;
            self.apply(&ledger_id, inv.op, &effects, inv.depth).await?;
        }
        // A mutation's result was bounded inside its transaction; a query's is bounded here.
        let text = result.result.to_string();
        if text.len() > limits::RESULT_MAX_BYTES {
            return Err(CellError::too_large("operation result", text.len(), limits::RESULT_MAX_BYTES));
        }
        Ok(result)
    }
}
