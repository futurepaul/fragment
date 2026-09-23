//! Calling an operation (docs/MODEL.md, Operations): the supervisor checks
//! the caller's role against the operation's declared role, bounds the
//! input, and runs it in the `app` facet, where a mutation and its ledger
//! row commit together. Callers who hold only the `public` floor (visitors
//! to a public fragment) are rate limited.

use fragment_proto::{canonical_json, limits, valid_op_id, ErrorCode, OpCall, OpKind, OpResult, Role};
use sha2::{Digest, Sha256};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::js::{self, AppCode};
use crate::plane::PLATFORM_JS;

struct Installed {
    loader_id: String,
    source: String,
    cpu_ms: u32,
}

impl FragmentCell {
    fn installed(&self) -> CellResult<Installed> {
        let rows = self.rows("SELECT loader_id, source, cpu_ms FROM code WHERE id = 1", vec![])?;
        let row = rows.into_iter().next().ok_or_else(|| CellError::new(ErrorCode::NoCode, "the live commit has no app.mjs (deploy one)"))?;
        let cpu_ms = row["cpu_ms"].as_u64().expect("code.cpu_ms is INTEGER");
        assert!(cpu_ms > 0 && cpu_ms <= limits::APP_CPU_MS as u64, "stored cpu_ms is within the limit");
        Ok(Installed {
            loader_id: row["loader_id"].as_str().expect("code.loader_id is TEXT").to_string(),
            source: row["source"].as_str().expect("code.source is TEXT").to_string(),
            cpu_ms: cpu_ms as u32,
        })
    }

    /// `POST /api/f/<name>/ops/<op>`: a signed caller.
    pub(crate) async fn api_op(&self, caller: &Caller, op: &str, body: OpCall) -> CellResult<Response> {
        let who = self.caller_hex(caller)?.to_string();
        let result = self.call_op(caller, &who, false, op, body).await?;
        json_response(&result)
    }

    /// Checks and runs one call. `principal` is who the ledger records (a
    /// key, or an anonymous visitor's id); `link` says the caller holds the
    /// share link.
    pub(crate) async fn call_op(&self, caller: &Caller, principal: &str, link: bool, op: &str, body: OpCall) -> CellResult<OpResult> {
        // Whether the caller can see the fragment at all comes before
        // anything about its operations.
        self.require(caller, link, Role::Public)?;
        let decl = self
            .operations()?
            .ok_or_else(|| CellError::new(ErrorCode::NoCode, "the live commit has no app.mjs (deploy one)"))?
            .remove(op)
            .ok_or_else(|| CellError::new(ErrorCode::UnknownOperation, format!("no operation named {op:?}")))?;
        let role = self.require(caller, link, decl.role)?;
        let is_member = caller.principal.as_deref().map(|p| self.member_role(p)).transpose()?.flatten().is_some();
        if role == Role::Public && !is_member && !self.rate.borrow_mut().allow(principal, js::now_ms()) {
            return Err(CellError::new(ErrorCode::RateLimited, "too many public calls this minute; retry shortly"));
        }
        if !valid_op_id(&body.id) {
            return Err(CellError::invalid("operation id must match ^[A-Za-z0-9._:-]{1,128}$"));
        }
        let input_text = canonical_json(&body.input);
        if input_text.len() > limits::INPUT_MAX_BYTES {
            return Err(CellError::too_large("operation input", input_text.len(), limits::INPUT_MAX_BYTES));
        }
        let code = self.installed()?;
        let facet = js::app_facet(
            &self.raw,
            self.env.as_ref(),
            &AppCode { id: &code.loader_id, platform: PLATFORM_JS, source: &code.source, cpu_ms: code.cpu_ms, subrequests: limits::APP_SUBREQUESTS },
        )?;
        let input_sha = hex::encode(Sha256::digest(format!("{op}\n{input_text}")));
        // Ids are the caller's: the ledger keys them by principal, so one
        // caller can neither replay nor block another's id.
        let ledger_id = format!("{principal}/{}", body.id);
        let answer = match decl.kind {
            OpKind::Query => facet.call("__query", &[op.into(), body.input.clone()]).await,
            OpKind::Mutation => facet.call("__mutate", &[ledger_id.as_str().into(), op.into(), input_sha.as_str().into(), body.input.clone()]).await,
        }
        .map_err(|m| CellError::new(ErrorCode::AppFailed, m))?;
        match answer["error"].as_str() {
            Some("conflicting_body") => {
                return Err(CellError::new(ErrorCode::ConflictingBody, "this operation id was already used with a different input"))
            }
            Some("unknown_operation") => return Err(CellError::new(ErrorCode::UnknownOperation, format!("the app has no method {op:?}"))),
            Some(other) => return Err(CellError::host(format!("the facet's platform code answered {other:?}"))),
            None => {}
        }
        let result = OpResult {
            result: answer["result"].clone(),
            replayed: answer["replayed"].as_bool().expect("the platform answers `replayed`"),
        };
        let text = result.result.to_string();
        if text.len() > limits::RESULT_MAX_BYTES {
            return Err(CellError::too_large("operation result", text.len(), limits::RESULT_MAX_BYTES));
        }
        if decl.kind == OpKind::Mutation {
            // The facet's ledger is the authority; this copy is the audit trail.
            self.exec(
                "INSERT OR IGNORE INTO ops (id, name, principal, input_sha, result, at) VALUES (?, ?, ?, ?, ?, ?)",
                vec![ledger_id.into(), op.into(), principal.into(), input_sha.into(), text.into(), SqlStorageValue::Integer(js::now_ms())],
            )?;
        }
        Ok(result)
    }
}
