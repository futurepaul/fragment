//! The `Fragment` supervisor: one Durable Object per fragment. It owns the
//! fragment's identity and code record, answers every call into the app,
//! and keeps an audit copy of the operation ledger. The router has already
//! verified the caller; it passes the principal and the fragment name in
//! headers only it can set.
//!
//! Slice A: only the owner may act, and code arrives by `PUT code`
//! (docs/phase-2.md; membership and the git file plane are slice B).

use fragment_proto::{
    canonical_json, limits, valid_op_id, valid_op_name, CreateFragment, ErrorCode, FragmentStatus, InstallCode, OpCall,
    OpKind, OpResult,
};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::*;

use crate::error::{CellError, CellResult};
use crate::js::{self, AppCode};

pub const PRINCIPAL_HEADER: &str = "x-fragment-principal";
pub const NAME_HEADER: &str = "x-fragment-name";

/// Platform code that runs in the facet around the author's App class.
const PLATFORM_JS: &str = include_str!("../platform.mjs");

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS code (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  sha TEXT NOT NULL,
  loader_id TEXT NOT NULL,
  source TEXT NOT NULL,
  operations TEXT NOT NULL,
  cpu_ms INTEGER NOT NULL,
  installed_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS ops (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  principal TEXT NOT NULL,
  input_sha TEXT NOT NULL,
  result TEXT NOT NULL,
  at INTEGER NOT NULL
);
";

#[durable_object]
pub struct FragmentCell {
    state: State,
    raw: JsValue,
    env: Env,
}

impl DurableObject for FragmentCell {
    fn new(state: State, env: Env) -> Self {
        let raw: JsValue = state._inner().into();
        let state = State::from(raw.clone().unchecked_into::<worker_sys::DurableObjectState>());
        state.storage().sql().exec(SCHEMA, None).expect("the Fragment schema applies");
        FragmentCell { state, raw, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        match self.route(req).await {
            Ok(resp) => Ok(resp),
            Err(e) => e.response(),
        }
    }
}

struct Caller {
    principal: String,
    name: String,
}

struct Installed {
    loader_id: String,
    source: String,
    operations: std::collections::BTreeMap<String, fragment_proto::OpDecl>,
    cpu_ms: u32,
}

async fn body_json<T: DeserializeOwned>(req: &mut Request) -> CellResult<T> {
    let bytes = req.bytes().await?;
    serde_json::from_slice(&bytes).map_err(|e| CellError::invalid(format!("body: {e}")))
}

impl FragmentCell {
    fn sql(&self) -> SqlStorage {
        self.state.storage().sql()
    }

    fn rows(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<Vec<serde_json::Value>> {
        Ok(self.sql().exec(q, binds)?.to_array::<serde_json::Value>()?)
    }

    fn meta(&self, key: &str) -> CellResult<Option<String>> {
        let rows = self.rows("SELECT value FROM meta WHERE key = ?", vec![key.into()])?;
        Ok(rows.first().map(|r| r["value"].as_str().expect("meta.value is TEXT").to_string()))
    }

    async fn route(&self, mut req: Request) -> CellResult<Response> {
        // Only the router reaches a Durable Object's fetch; it sets these.
        let principal = req.headers().get(PRINCIPAL_HEADER)?.ok_or_else(|| CellError::host("no principal from the router"))?;
        let name = req.headers().get(NAME_HEADER)?.ok_or_else(|| CellError::host("no fragment name from the router"))?;
        let caller = Caller { principal, name };
        let path = req.path();
        let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
        match (req.method(), segments.as_slice()) {
            (Method::Post, ["create"]) => {
                let body: CreateFragment = body_json(&mut req).await?;
                self.create(&caller, body)
            }
            (Method::Get, ["status"]) => self.status(&caller),
            (Method::Put, ["code"]) => {
                let body: InstallCode = body_json(&mut req).await?;
                self.install(&caller, body)
            }
            (Method::Post, ["ops", op]) => {
                let op = op.to_string();
                let body: OpCall = body_json(&mut req).await?;
                self.op(&caller, &op, body).await
            }
            _ => Err(CellError::new(ErrorCode::NotFound, format!("no route {} {path}", req.method().as_ref()))),
        }
    }

    fn create(&self, caller: &Caller, body: CreateFragment) -> CellResult<Response> {
        if body.name != caller.name {
            return Err(CellError::host("the router addressed a different fragment than the body names"));
        }
        if self.meta("name")?.is_some() {
            return Err(CellError::new(ErrorCode::AlreadyExists, format!("fragment {} already exists", body.name)));
        }
        let now = js::now_ms().to_string();
        for (k, v) in [("name", body.name.as_str()), ("owner", caller.principal.as_str()), ("created_at", now.as_str())] {
            self.sql().exec("INSERT INTO meta (key, value) VALUES (?, ?)", vec![k.into(), v.into()])?;
        }
        Ok(Response::from_json(&self.status_of()?)?)
    }

    fn status_of(&self) -> CellResult<FragmentStatus> {
        let name = self.meta("name")?.expect("status_of is called on a created fragment");
        let owner = self.meta("owner")?.expect("a created fragment has an owner");
        let code = self.rows("SELECT sha FROM code WHERE id = 1", vec![])?;
        let code_sha = code.first().map(|r| r["sha"].as_str().expect("code.sha is TEXT").to_string());
        Ok(FragmentStatus { name, owner, code_sha })
    }

    /// The fragment exists and the caller may act on it.
    fn authorize(&self, caller: &Caller) -> CellResult<()> {
        let owner = self
            .meta("owner")?
            .ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no fragment named {}", caller.name)))?;
        if owner != caller.principal {
            return Err(CellError::new(ErrorCode::Forbidden, "only the fragment's owner may do this"));
        }
        Ok(())
    }

    fn status(&self, caller: &Caller) -> CellResult<Response> {
        self.authorize(caller)?;
        Ok(Response::from_json(&self.status_of()?)?)
    }

    fn install(&self, caller: &Caller, body: InstallCode) -> CellResult<Response> {
        self.authorize(caller)?;
        if body.sha.is_empty() || body.sha.len() > 64 || !body.sha.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(CellError::invalid("sha must be 1..=64 ASCII letters and digits"));
        }
        if body.source.len() > limits::SOURCE_MAX_BYTES {
            return Err(CellError::too_large("app source", body.source.len(), limits::SOURCE_MAX_BYTES));
        }
        if body.operations.len() > limits::OPERATIONS_MAX {
            return Err(CellError::invalid(format!("at most {} operations", limits::OPERATIONS_MAX)));
        }
        if let Some(bad) = body.operations.keys().find(|n| !valid_op_name(n)) {
            return Err(CellError::invalid(format!("operation name {bad:?} must match ^[a-z][a-z0-9_]{{0,63}}$")));
        }
        let cpu_ms = body.cpu_ms.unwrap_or(limits::APP_CPU_MS);
        if cpu_ms == 0 || cpu_ms > limits::APP_CPU_MS {
            return Err(CellError::invalid(format!("cpu_ms must be 1..={}", limits::APP_CPU_MS)));
        }
        let loader_id = format!("app:{}", hex::encode(Sha256::digest(format!("{PLATFORM_JS}\0{}", body.source))));
        let operations = serde_json::to_string(&body.operations).expect("operations serialize");
        self.sql().exec(
            "INSERT INTO code (id, sha, loader_id, source, operations, cpu_ms, installed_at) VALUES (1, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (id) DO UPDATE SET sha = excluded.sha, loader_id = excluded.loader_id, source = excluded.source,
               operations = excluded.operations, cpu_ms = excluded.cpu_ms, installed_at = excluded.installed_at",
            vec![
                body.sha.as_str().into(),
                loader_id.into(),
                body.source.into(),
                operations.into(),
                SqlStorageValue::Integer(cpu_ms.into()),
                SqlStorageValue::Integer(js::now_ms()),
            ],
        )?;
        js::abort_app_facet(&self.raw, "code replaced")?;
        Ok(Response::from_json(&self.status_of()?)?)
    }

    fn installed(&self) -> CellResult<Installed> {
        let rows = self.rows("SELECT loader_id, source, operations, cpu_ms FROM code WHERE id = 1", vec![])?;
        let row = rows.into_iter().next().ok_or_else(|| CellError::new(ErrorCode::NoCode, "no app code is installed"))?;
        let cpu_ms = row["cpu_ms"].as_u64().expect("code.cpu_ms is INTEGER");
        assert!(cpu_ms > 0 && cpu_ms <= limits::APP_CPU_MS as u64, "stored cpu_ms is within the limit");
        Ok(Installed {
            loader_id: row["loader_id"].as_str().expect("code.loader_id is TEXT").to_string(),
            source: row["source"].as_str().expect("code.source is TEXT").to_string(),
            operations: serde_json::from_str(row["operations"].as_str().expect("code.operations is TEXT"))
                .expect("stored operations parse"),
            cpu_ms: cpu_ms as u32,
        })
    }

    async fn op(&self, caller: &Caller, op: &str, body: OpCall) -> CellResult<Response> {
        self.authorize(caller)?;
        if !valid_op_id(&body.id) {
            return Err(CellError::invalid("operation id must match ^[A-Za-z0-9._:-]{1,128}$"));
        }
        let input_text = canonical_json(&body.input);
        if input_text.len() > limits::INPUT_MAX_BYTES {
            return Err(CellError::too_large("operation input", input_text.len(), limits::INPUT_MAX_BYTES));
        }
        let code = self.installed()?;
        let kind = code
            .operations
            .get(op)
            .map(|d| d.kind)
            .ok_or_else(|| CellError::new(ErrorCode::UnknownOperation, format!("no operation named {op:?}")))?;
        let facet = js::app_facet(
            &self.raw,
            self.env.as_ref(),
            &AppCode {
                id: &code.loader_id,
                platform: PLATFORM_JS,
                source: &code.source,
                cpu_ms: code.cpu_ms,
                subrequests: limits::APP_SUBREQUESTS,
            },
        )?;
        let input_sha = hex::encode(Sha256::digest(format!("{op}\n{input_text}")));
        let answer = match kind {
            OpKind::Query => facet.call("__query", &[op.into(), body.input.clone()]).await,
            OpKind::Mutation => {
                facet.call("__mutate", &[body.id.as_str().into(), op.into(), input_sha.as_str().into(), body.input.clone()]).await
            }
        }
        .map_err(|m| CellError::new(ErrorCode::AppFailed, m))?;
        match answer["error"].as_str() {
            Some("conflicting_body") => {
                return Err(CellError::new(ErrorCode::ConflictingBody, "this operation id was already used with a different input"))
            }
            Some("unknown_operation") => {
                return Err(CellError::new(ErrorCode::UnknownOperation, format!("the app has no method {op:?}")))
            }
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
        if kind == OpKind::Mutation {
            // The facet's ledger is the authority; this copy is the audit trail.
            self.sql().exec(
                "INSERT OR IGNORE INTO ops (id, name, principal, input_sha, result, at) VALUES (?, ?, ?, ?, ?, ?)",
                vec![
                    body.id.into(),
                    op.into(),
                    caller.principal.as_str().into(),
                    input_sha.into(),
                    text.into(),
                    SqlStorageValue::Integer(js::now_ms()),
                ],
            )?;
        }
        Ok(Response::from_json(&result)?)
    }
}
