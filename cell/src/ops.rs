//! Calling an operation (docs/MODEL.md, Operations): the supervisor checks
//! the caller's role against the operation's declared role, bounds the
//! input, and runs it in the `app` facet, where a mutation and its ledger
//! row commit together. Callers who hold only the `public` floor (visitors
//! to a public fragment) are rate limited. A job is not run here: calling
//! one records a run and starts its Workflow (`jobs.rs`).

use std::collections::BTreeMap;

use fragment_core::facet::{self, Answer, Refusal};
use fragment_core::npub;
use fragment_proto::{canonical_json, limits, valid_op_id, ErrorCode, OpCall, OpDecl, OpKind, OpResult, Role};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::*;

use crate::channels::Settled;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::jobs::NewRun;
use crate::js::{self, AppCode};
use crate::plane::PLATFORM_JS;

/// Operation ids a job's steps use; callers cannot choose them.
pub const JOB_ID_PREFIX: &str = "job:";
/// How long the facet's ledger recognizes an id: a call with it within
/// this is a replay; after it, the same id runs again as a new run. The
/// facet takes the window from each call, so this is its one definition.
pub(crate) const LEDGER_KEPT_MS: i64 = 7 * 24 * 3600 * 1000;

/// The platform code this cell carries: `platform.js`, and the
/// `limits.js` it imports (generated from the Rust limits and rules:
/// `fragment_core::facet`), with their content address. Both come with the
/// cell deploy, not with an install, so the address joins the loader id
/// when the app is loaded, not when its code is stored. Computed once per
/// isolate from constants compiled into that isolate: it has nothing to
/// invalidate and cannot go stale.
struct Platform {
    limits: String,
    id: String,
}

fn platform() -> &'static Platform {
    static PLATFORM: std::sync::OnceLock<Platform> = std::sync::OnceLock::new();
    PLATFORM.get_or_init(|| {
        let limits = facet::limits_js();
        let mut hasher = Sha256::new();
        hasher.update(PLATFORM_JS.as_bytes());
        hasher.update(b"\0");
        hasher.update(facet::LIMITS_MODULE.as_bytes());
        hasher.update(b"\0");
        hasher.update(limits.as_bytes());
        Platform { id: hex::encode(hasher.finalize()), limits }
    })
}

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

/// The facet refused a call: nothing of the call committed.
fn refused(why: Refusal, op: &str) -> CellError {
    let message = match why {
        Refusal::StorageFull => format!("the app's database is full ({} MiB): the mutation was rolled back", limits::APP_DB_MAX_BYTES / (1024 * 1024)),
        Refusal::ConflictingBody => "this operation id was already used with a different input".to_string(),
        Refusal::UnknownOperation => format!("the app has no method {op:?}"),
    };
    CellError::new(why.code(), message)
}

/// A result is bounded here too: the facet's own bound runs in the author's realm.
fn bounded(result: OpResult) -> CellResult<OpResult> {
    let text = result.result.to_string();
    if text.len() > limits::RESULT_MAX_BYTES {
        return Err(CellError::too_large("operation result", text.len(), limits::RESULT_MAX_BYTES));
    }
    Ok(result)
}

impl FragmentCell {
    /// The running app facet for the installed code.
    pub(crate) fn facet(&self) -> CellResult<js::Facet> {
        let rows = self.rows("SELECT loader_id, source, modules, cpu_ms FROM code WHERE id = 1", vec![])?;
        let row = rows.into_iter().next().ok_or_else(|| CellError::new(ErrorCode::NoCode, "the live commit has no app.mjs (deploy one)"))?;
        let cpu_ms = row["cpu_ms"].as_u64().expect("code.cpu_ms is INTEGER");
        assert!(cpu_ms > 0 && cpu_ms <= limits::APP_CPU_MS as u64, "stored cpu_ms is within the limit");
        let modules: BTreeMap<String, String> = serde_json::from_str(row["modules"].as_str().unwrap_or("{}")).expect("stored modules parse");
        // The loader memoizes a worker by its id, so the id names every
        // byte it runs: the app's modules (the stored loader_id) and the
        // platform code this cell carries (platform.js and limits.js), which
        // a cell deploy changes under code installed before it. And one loaded worker per
        // fragment: its env holds this fragment's capabilities, and its
        // module state is this fragment's alone.
        let loader_id = row["loader_id"].as_str().expect("code.loader_id is TEXT");
        let platform = platform();
        let id = format!("{loader_id}:{}:{}", platform.id, self.must("npub")?);
        js::app_facet(
            &self.raw,
            self.env.as_ref(),
            &self.name()?,
            &AppCode {
                id: &id,
                platform: PLATFORM_JS,
                limits: &platform.limits,
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
        let is_member = caller.principal().map(|p| self.member_role(p)).transpose()?.flatten().is_some();
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
        let meta = json!({
            "principal": npub::display(inv.principal),
            "role": inv.role,
            "channels": self.declared_channels()?.keys().collect::<Vec<_>>(),
        });
        match inv.decl.kind {
            OpKind::Query => match facet.query(inv.op, inv.input.clone(), meta).await? {
                Answer::Ran(q) => bounded(OpResult { result: q.result, replayed: false }),
                Answer::Refused(why) => Err(refused(why, inv.op)),
            },
            OpKind::Mutation => self.mutate(&facet, &inv, &input_sha, meta).await,
            OpKind::Job => unreachable!("a job's call records a run above"),
        }
    }

    /// A mutation: recorded as pending, run in the facet (where it and its
    /// ledger row commit together), then settled (channels.rs).
    async fn mutate(&self, facet: &js::Facet, inv: &Invocation<'_>, input_sha: &str, mut meta: Value) -> CellResult<OpResult> {
        // Ids are the caller's: the ledger keys them by principal, so one
        // caller can neither replay nor block another's id.
        let ledger_id = format!("{}/{}", inv.principal, inv.id);
        // One call settles an id at a time: a retry that arrives while the
        // first is still running waits for it, then replays.
        let _held = self.hold(&ledger_id).await;
        let (mut pending, fresh) = self.begin(&ledger_id, inv.op, inv.principal, inv.depth)?;
        meta["run"] = pending.seq.into();
        meta["ledgerMs"] = self.ledger_kept_ms()?.into();
        let args = [ledger_id.as_str().into(), inv.op.into(), input_sha.into(), inv.input.clone(), meta];
        let answer = match facet.mutate(args).await {
            Ok(answer) => answer,
            Err(e) => {
                // A node that could not load the app ran nothing. Otherwise
                // the author's code usually threw and the facet rolled back;
                // its ledger says whether this run committed anyway (an
                // answer lost after the commit). A row an earlier attempt
                // left is the sweep's.
                match (fresh, e.code) {
                    (false, _) => {}
                    (true, ErrorCode::NodeFull) => self.forget(&pending)?,
                    (true, _) => {
                        self.settle_from_ledger(facet, &pending).await?;
                    }
                }
                return Err(e);
            }
        };
        let ran = match answer {
            Answer::Ran(ran) => ran,
            Answer::Refused(why) => {
                if fresh {
                    self.forget(&pending)?;
                }
                return Err(refused(why, inv.op));
            }
        };
        let (replayed, run) = (ran.replayed, ran.run);
        if !replayed {
            if run != Some(pending.seq) {
                // Fail closed: the sweep asks the facet's ledger which run it holds.
                return Err(CellError::host(format!("the facet committed run {run:?}, not run {}", pending.seq)));
            }
            if !fresh {
                self.restate(&mut pending, inv.op, inv.depth)?;
            }
        }
        // The facet answers the run it holds for this id. That run is this
        // row's when it just committed, or when an earlier try of it has not
        // settled yet; any other run settled long ago (a replay applies
        // nothing again), and a row that is not its run never committed.
        if run == Some(pending.seq) {
            if let Settled::Refused(why) = self.apply(&pending, &ran.effects).await? {
                return Err(CellError::new(ErrorCode::AppFailed, format!("{} committed, but the platform refused its effects: {why}", inv.op)));
            }
        } else {
            self.forget(&pending)?;
        }
        bounded(OpResult { result: ran.result, replayed })
    }

    /// The ledger window this fragment's facet keeps: `LEDGER_KEPT_MS`, or
    /// a shorter one a test set (on fleets with test hooks only).
    fn ledger_kept_ms(&self) -> CellResult<i64> {
        if !self.cfg.test_hooks {
            return Ok(LEDGER_KEPT_MS);
        }
        Ok(self.meta("test_ledger_ms")?.and_then(|v| v.parse().ok()).unwrap_or(LEDGER_KEPT_MS))
    }

    /// `POST /api/test/fragment {fragment, op, …}`, the router's, on fleets
    /// with test hooks only: the levers the e2e pulls on one fragment.
    /// `fail-deliveries {times}` fails its next queue sends; `fail-outbox
    /// {times}` fails its next records' outbox writes after their append; `fail-triggers
    /// {times}` fails its next trigger steps before their last run; `drop-live
    /// {code}` closes its live sockets; `ledger {ms | null}` sets (or
    /// clears) a shorter ledger window; `age {ms}` forgets write keys as
    /// if `ms` had passed; `members {fill}` adds placeholder members until
    /// there are `fill`; `code-before-tables` puts its installed code back
    /// in the shape stored before the code tables (plane.rs).
    pub(crate) fn test_fragment(&self, body: &Value) -> CellResult<Value> {
        assert!(self.cfg.test_hooks, "the route answers only on fleets with test hooks");
        self.name()?;
        let ms = || body["ms"].as_i64().filter(|ms| *ms >= 0).ok_or_else(|| CellError::invalid("ms is a duration"));
        Ok(match body["op"].as_str() {
            Some("fail-deliveries") => {
                let times = body["times"].as_u64().ok_or_else(|| CellError::invalid("fail-deliveries names how many times"))?;
                self.set_meta(crate::deliveries::TEST_FAILURES_KEY, &times.to_string())?;
                json!({ "ok": true })
            }
            Some("fail-outbox") => {
                let times = body["times"].as_u64().ok_or_else(|| CellError::invalid("fail-outbox names how many times"))?;
                self.set_meta(crate::subscriptions::TEST_OUTBOX_FAILURES_KEY, &times.to_string())?;
                json!({ "ok": true })
            }
            Some("fail-triggers") => {
                let times = body["times"].as_u64().ok_or_else(|| CellError::invalid("fail-triggers names how many times"))?;
                self.set_meta(crate::jobs::TEST_TRIGGER_FAILURES_KEY, &times.to_string())?;
                json!({ "ok": true })
            }
            Some("drop-live") => {
                let code = body["code"].as_u64().and_then(|c| u16::try_from(c).ok()).ok_or_else(|| CellError::invalid("drop-live names a close code"))?;
                for ws in self.state.get_websockets_with_tag("live") {
                    let _ = ws.close(Some(code), Some("dropped by a test hook"));
                }
                json!({ "ok": true })
            }
            Some("ledger") => match body["ms"].as_i64() {
                Some(ms) if ms > 0 => {
                    self.set_meta("test_ledger_ms", &ms.to_string())?;
                    json!({ "ledgerMs": ms })
                }
                _ => {
                    self.del_meta("test_ledger_ms")?;
                    json!({ "ledgerMs": LEDGER_KEPT_MS })
                }
            },
            Some("age") => {
                let ms = ms()?;
                self.trim_writes_before(js::now_ms() + ms - crate::files::WRITES_KEPT_MS)?;
                json!({ "aged": ms })
            }
            Some("members") => {
                let fill = body["fill"].as_u64().ok_or_else(|| CellError::invalid("fill is a count"))?;
                json!({ "members": self.fill_members(fill)? })
            }
            Some("code-before-tables") => {
                self.code_before_tables()?;
                json!({ "ok": true })
            }
            _ => return Err(CellError::invalid("op is fail-deliveries, fail-outbox, fail-triggers, drop-live, ledger, age, members, or code-before-tables")),
        })
    }
}
