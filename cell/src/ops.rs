//! Calling an operation (docs/MODEL.md, Operations): the supervisor checks
//! the caller's role against the operation's declared role, bounds the
//! input, and runs it in the `app` facet, where a mutation and its ledger
//! row commit together. Callers who hold only the `public` floor (visitors
//! to a public fragment) are rate limited. A job is not run here: calling
//! one records a run and starts its Workflow (`jobs.rs`).

use std::collections::BTreeMap;

use fragment_core::access::Purpose;
use fragment_core::facet::{self, Answer, Answered, Refusal};
use fragment_core::npub;
use fragment_proto::{canonical_json, limits, valid_op_id, ErrorCode, OpCall, OpDecl, OpKind, Role, Via};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::channels::Settled;
use crate::error::{CellError, CellResult};
use crate::fragment::{decide, json_response, Caller, Facts, FragmentCell, MetaKey};
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
    /// How a job started this way is recorded: a call, or a step of its parent run.
    pub via: Via,
    pub trigger: Option<String>,
}

/// The hash a call's id is recorded with: its operation and its input's
/// canonical JSON text (`fragment_proto::canonical_json`).
pub(crate) fn input_sha(op: &str, canonical_input: &str) -> String {
    hex::encode(Sha256::digest(format!("{op}\n{canonical_input}")))
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

/// A result is bounded here too, by its text's length: the facet's own
/// bound runs in the author's realm.
fn bounded(answer: Answered) -> CellResult<Answered> {
    let bytes = answer.result.get().len();
    if bytes > limits::RESULT_MAX_BYTES {
        return Err(CellError::too_large("operation result", bytes, limits::RESULT_MAX_BYTES));
    }
    Ok(answer)
}

/// The loader id of the code installed now, or why there is none: the
/// loader memoizes a worker by its id, so the id names every byte it runs,
/// the app's modules (the stored loader_id) and the platform code this cell
/// carries (platform.js and limits.js), which a cell deploy changes under
/// code installed before it. And one loaded worker per fragment (its key
/// joins the id): its env holds this fragment's capabilities, and its
/// module state is this fragment's alone.
fn installed_id(loader_id: &str, npub: &str) -> String {
    format!("{loader_id}:{}:{npub}", platform().id)
}

#[derive(Deserialize)]
struct CodeRow {
    loader_id: String,
    npub: Option<String>,
}

/// The loader id `start` asks for: one statement.
fn read_installed_id(sql: &SqlStorage) -> Result<String, String> {
    let rows: Vec<CodeRow> = sql
        .exec("SELECT c.loader_id, (SELECT value FROM meta WHERE key = ?) AS npub FROM code c WHERE c.id = 1", vec![MetaKey::Npub.key().into()])
        .and_then(|c| c.to_array())
        .map_err(|e| format!("the code row: {e}"))?;
    let row = rows.into_iter().next().ok_or("the live commit has no app.mjs (deploy one)")?;
    Ok(installed_id(&row.loader_id, row.npub.as_deref().ok_or("a created fragment has no npub")?))
}

#[derive(Deserialize)]
struct SourceRow {
    loader_id: String,
    source: String,
    modules: String,
    cpu_ms: u32,
    npub: Option<String>,
    name: Option<String>,
}

/// The code the loader asks for by `id`, read now: the id is checked against
/// the code row, so a worker is never built under an id that names other
/// bytes (the code changed between `start` and the loader's call: the
/// load fails, and the next call starts from the code installed then).
fn read_installed_code(sql: &SqlStorage, id: &str) -> Result<AppCode, String> {
    let rows: Vec<SourceRow> = sql
        .exec(
            "SELECT c.loader_id, c.source, c.modules, c.cpu_ms, (SELECT value FROM meta WHERE key = ?) AS npub, (SELECT value FROM meta WHERE key = ?) AS name
             FROM code c WHERE c.id = 1",
            vec![MetaKey::Npub.key().into(), MetaKey::Name.key().into()],
        )
        .and_then(|c| c.to_array())
        .map_err(|e| format!("the code row: {e}"))?;
    let row = rows.into_iter().next().ok_or("the live commit has no app.mjs (deploy one)")?;
    let npub = row.npub.ok_or("a created fragment has no npub")?;
    if installed_id(&row.loader_id, &npub) != id {
        return Err(format!("the loader asked for {id}, which is not the installed code any more"));
    }
    assert!(row.cpu_ms > 0 && row.cpu_ms <= limits::APP_CPU_MS, "stored cpu_ms is within the limit");
    let modules: BTreeMap<String, String> = serde_json::from_str(&row.modules).map_err(|e| format!("the stored applib modules: {e}"))?;
    Ok(AppCode {
        fragment: row.name.ok_or("a created fragment has no name")?,
        platform: PLATFORM_JS,
        limits: &platform().limits,
        source: row.source,
        modules,
        cpu_ms: row.cpu_ms,
        subrequests: limits::APP_SUBREQUESTS,
    })
}

/// This fragment's app loader (js.rs `AppLoader`), made once per activation:
/// its callbacks read the code row themselves when they run.
pub(crate) fn app_loader(ctx: &JsValue, env: &JsValue, sql: SqlStorage) -> js::AppLoader {
    let for_id = sql.clone();
    js::AppLoader::new(ctx, env, move || read_installed_id(&for_id), move |id| read_installed_code(&sql, id))
}

impl FragmentCell {
    /// The running app facet for the installed code. A call reads only
    /// whether there is code: the facet table answers the running facet,
    /// and the loader's callbacks read the code when there is none.
    pub(crate) fn facet(&self) -> CellResult<js::Facet> {
        let rows = self.rows("SELECT loader_id FROM code WHERE id = 1", vec![])?;
        if rows.is_empty() {
            return Err(CellError::new(ErrorCode::NoCode, "the live commit has no app.mjs (deploy one)"));
        }
        self.app.facet(&self.raw)
    }

    /// `POST /api/f/<name>/ops/<op>`: a signed caller.
    pub(crate) async fn api_op(&self, caller: &Caller, op: &str, body: OpCall) -> CellResult<Response> {
        let who = self.caller_id(caller)?.to_string();
        let facts = self.facts()?;
        let result = self.call_op(caller, &facts, &who, false, op, body).await?;
        json_response(&result)
    }

    /// Checks and runs one call from outside. `principal` is who the ledger
    /// records (an identity, or an anonymous visitor's id); `link` says the
    /// caller holds the share link. The caller's standing is read once and
    /// decided twice.
    pub(crate) async fn call_op(&self, caller: &Caller, facts: &Facts, principal: &str, link: bool, op: &str, body: OpCall) -> CellResult<Answered> {
        let standing = self.standing(caller, link)?;
        // Whether the caller can see the fragment at all comes before
        // anything about its operations.
        decide(facts.visibility, standing, Purpose::Read, Role::Public)?;
        let decl = self.declared(op)?;
        // a query reads (docs/MODEL.md), so an agent's owner may ask it; a
        // mutation or a job acts, which takes a membership of one's own
        let purpose = if decl.kind == OpKind::Query { Purpose::Read } else { Purpose::Act };
        let role = decide(facts.visibility, standing, purpose, decl.role)?;
        // Members act with their own role, which is never `public`: only
        // callers holding the public floor alone are rate limited.
        if role == Role::Public && !self.rate.borrow_mut().allow(principal, js::now_ms()) {
            return Err(CellError::new(ErrorCode::RateLimited, "too many public calls this minute; retry shortly"));
        }
        if !valid_op_id(&body.id) || body.id.starts_with(JOB_ID_PREFIX) {
            return Err(CellError::invalid("operation id must match ^[A-Za-z0-9._:-]{1,128}$ and not start with job:"));
        }
        let inv = Invocation { principal, role, op, decl, id: body.id, input: body.input, depth: 0, via: Via::Call, trigger: None };
        let result = self.invoke(inv).await?;
        self.launch_queued().await;
        Ok(result)
    }

    /// Runs an admitted call: a query or a mutation in the facet, or a job's
    /// run recorded (the caller launches queued runs afterwards). The input
    /// is written as canonical JSON once: its size, its hash, and the text
    /// the app parses are all that one text.
    pub(crate) async fn invoke(&self, inv: Invocation<'_>) -> CellResult<Answered> {
        let input_text = canonical_json(&inv.input);
        if input_text.len() > limits::INPUT_MAX_BYTES {
            return Err(CellError::too_large("operation input", input_text.len(), limits::INPUT_MAX_BYTES));
        }
        if let Some(schema) = &inv.decl.input {
            fragment_core::schema::validate(schema, &inv.input).map_err(|e| CellError::invalid(format!("input {e}")))?;
        }
        let input_sha = input_sha(inv.op, &input_text);
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
            let result = serde_json::value::to_raw_value(&json!({ "run": started.id, "status": started.status })).expect("a run's id and status serialize");
            return Ok(Answered { result, replayed: started.replayed });
        }
        let facet = self.facet()?;
        self.sweep(&facet).await?;
        match inv.decl.kind {
            OpKind::Query => {
                // a query publishes nothing, so it is told no channels
                let meta = json!({ "principal": npub::display(inv.principal), "role": inv.role });
                match facet.query(inv.op, &input_text, &meta).await? {
                    Answer::Ran(q) => bounded(Answered { result: q.result, replayed: false }),
                    Answer::Refused(why) => Err(refused(why, inv.op)),
                }
            }
            OpKind::Mutation => {
                let meta = json!({
                    "principal": npub::display(inv.principal),
                    "role": inv.role,
                    "channels": self.declared_channels()?.keys().collect::<Vec<_>>(),
                });
                self.mutate(&facet, &inv, &input_sha, &input_text, meta).await
            }
            OpKind::Job => unreachable!("a job's call records a run above"),
        }
    }

    /// A mutation: recorded as pending, run in the facet (where it and its
    /// ledger row commit together), then settled (channels.rs).
    async fn mutate(&self, facet: &js::Facet, inv: &Invocation<'_>, input_sha: &str, input_text: &str, mut meta: Value) -> CellResult<Answered> {
        // Ids are the caller's: the ledger keys them by principal, so one
        // caller can neither replay nor block another's id.
        let ledger_id = format!("{}/{}", inv.principal, inv.id);
        // One call settles an id at a time: a retry that arrives while the
        // first is still running waits for it, then replays.
        let _held = self.hold(&ledger_id).await;
        let (mut pending, fresh) = self.begin(&ledger_id, inv.op, inv.principal, inv.depth)?;
        meta["run"] = pending.seq.into();
        meta["ledgerMs"] = self.ledger_kept_ms()?.into();
        let call = js::Mutation { ledger_id: &ledger_id, op: inv.op, input_sha, input: input_text, meta: &meta };
        let answer = match facet.mutate(call).await {
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
        bounded(Answered { result: ran.result, replayed })
    }

    /// The ledger window this fragment's facet keeps: `LEDGER_KEPT_MS`, or
    /// a shorter one a test set (on fleets with test hooks only).
    fn ledger_kept_ms(&self) -> CellResult<i64> {
        if !self.cfg.test_hooks {
            return Ok(LEDGER_KEPT_MS);
        }
        Ok(self.meta(MetaKey::TestLedgerMs)?.and_then(|v| v.parse().ok()).unwrap_or(LEDGER_KEPT_MS))
    }

    /// Test fleets: while a `fail-*` or `drop-effects` lever (`test_fragment`)
    /// counts above zero, one less, and `why` fails.
    pub(crate) fn test_countdown(&self, key: MetaKey, why: &str) -> CellResult<()> {
        let left: u64 = if self.cfg.test_hooks { self.meta(key)?.and_then(|n| n.parse().ok()).unwrap_or(0) } else { 0 };
        if left == 0 {
            return Ok(());
        }
        self.set_meta(key, &(left - 1).to_string())?;
        Err(CellError::host(format!("{why} (a test hook)")))
    }

    /// `POST /api/test/fragment {fragment, op, …}`, the router's, on fleets
    /// with test hooks only: the levers the e2e pulls on one fragment
    /// (docs/api.md, `FRAGMENT_TEST_HOOKS`).
    pub(crate) fn test_fragment(&self, body: &Value) -> CellResult<Value> {
        assert!(self.cfg.test_hooks, "the route answers only on fleets with test hooks");
        self.name()?;
        let ms = || body["ms"].as_i64().filter(|ms| *ms >= 0).ok_or_else(|| CellError::invalid("ms is a duration"));
        Ok(match body["op"].as_str() {
            Some(lever @ ("fail-deliveries" | "fail-outbox" | "fail-triggers" | "drop-effects")) => {
                let key = match lever {
                    "fail-deliveries" => MetaKey::TestFailDeliveries,
                    "fail-outbox" => MetaKey::TestFailOutbox,
                    "fail-triggers" => MetaKey::TestFailTriggers,
                    _ => MetaKey::TestDropEffects,
                };
                let times = body["times"].as_u64().ok_or_else(|| CellError::invalid(format!("{lever} names how many times")))?;
                self.set_meta(key, &times.to_string())?;
                json!({ "ok": true })
            }
            Some("forget-live") => {
                self.live_forget();
                json!({ "ok": true })
            }
            Some("forget-steps") => {
                // the kept answers of every run in flight go; with `deploy`,
                // now is when answers began to be kept, as the deploy that
                // began keeping them wrote
                self.exec("DELETE FROM steps WHERE run IN (SELECT id FROM runs WHERE status IN ('queued', 'running'))", vec![])?;
                if body["deploy"] == true {
                    self.set_meta(MetaKey::StepsKeptSince, &js::now_ms().to_string())?;
                }
                json!({ "ok": true })
            }
            Some("hold-advances") => {
                // while on, an advance after a run's first step waits
                // (jobs.rs `held_advance`), at most 20 s
                if body["on"] == true {
                    self.del_meta(MetaKey::TestAdvanceHeld)?;
                    self.set_meta(MetaKey::TestHoldAdvances, "1")?;
                } else {
                    self.del_meta(MetaKey::TestHoldAdvances)?;
                }
                json!({ "ok": true })
            }
            Some("advance-held") => json!({ "run": self.meta(MetaKey::TestAdvanceHeld)?.and_then(|r| r.parse::<i64>().ok()) }),
            Some("age-live") => {
                let ms = body["ms"].as_i64().filter(|ms| *ms > 0).ok_or_else(|| CellError::invalid("ms: a positive number"))?;
                json!({ "aged": self.live_age(ms)? })
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
                    self.set_meta(MetaKey::TestLedgerMs, &ms.to_string())?;
                    json!({ "ledgerMs": ms })
                }
                _ => {
                    self.del_meta(MetaKey::TestLedgerMs)?;
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
                let fill = match &body["fill"] {
                    Value::Null => None,
                    v => Some(v.as_u64().ok_or_else(|| CellError::invalid("fill is a count"))?),
                };
                self.code_before_tables(fill)?;
                json!({ "ok": true })
            }
            Some("code-builds") => json!({ "builds": self.app.builds() }),
            _ => return Err(CellError::invalid("op is fail-deliveries, fail-outbox, fail-triggers, drop-effects, forget-steps, hold-advances, advance-held, forget-live, age-live, drop-live, ledger, age, members, code-before-tables, or code-builds")),
        })
    }
}
