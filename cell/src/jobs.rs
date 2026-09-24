//! Jobs, triggers, and the inbox (docs/MODEL.md, Operations; phase 2
//! slice D).
//!
//! A **run** is one execution of an operation outside a request: a job
//! someone called, or a mutation or job a trigger started. Each run is a
//! celld Workflow instance (`Job` in entry.mjs) that drives it one step at
//! a time by calling back into this supervisor:
//!
//!   POST /job/advance {run, attempt, results}   re-run the job's body in the
//!        app facet over the step results so far, up to its next step
//!   POST /job/effect  {run, attempt, index, kind, args}   perform that step:
//!        call an operation, fetch (secrets are added here), or publish
//!   POST /job/finish  {run, attempt, error}     the Workflow gave up
//!
//! The Workflow records every answer, so a crash resumes at the step it
//! was on, and a step that fails for a reason that may pass (an upstream
//! 5xx, a network error) is retried with backoff. A run that fails for
//! good is **held**: kept with its input until someone replays it.
//!
//! **Triggers** start runs as the fragment itself, with an editor's reach:
//! cron schedules (on this object's alarm), records appended to a channel
//! (the inbox is one), and moves of `main` that touch matching files. An
//! operation's triggers pause themselves after repeated held runs or too
//! many runs in an hour, and a chain of triggered runs deeper than the hop
//! budget is blocked as a loop. Calls are never paused or rate-ceilinged.

use std::collections::BTreeMap;
use std::time::Duration;

use fragment_core::secrets::placeholders;
use fragment_core::{cron::Cron, egress, glob, npub, trigger_state};
use fragment_proto::{
    limits, valid_secret_name, ChannelRecord, ErrorCode, OpKind, Replay, Role, Run, RunStatus, SetPaused, TriggerDecl, TriggerOn,
};
use serde_json::{json, Map, Value};
use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::cs::FetchError;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, Caller, FragmentCell};
use crate::ops::{Invocation, JOB_ID_PREFIX};
use crate::{js, keys};

/// Set by the Workflow on its callbacks; the router never sets or passes it.
pub const JOB_HEADER: &str = "x-fragment-job";
/// How far a fetch carries the chain it is part of (another fragment's inbox reads it).
pub const HOPS_HEADER: &str = "x-fragment-hops";
/// Test fleets: this many trigger steps from this fragment fail (a lever
/// the e2e pulls through `/test/fragment`).
pub const TEST_TRIGGER_FAILURES_KEY: &str = "test_fail_triggers";
/// A run whose Workflow could not be started is tried again this soon.
const QUEUED_RETRY_MS: i64 = 10_000;
const LAUNCH_BATCH: usize = 25;
/// Running runs checked against their Workflow per alarm, once they are this old.
const RECONCILE_BATCH: i64 = 25;
const RECONCILE_AFTER_MS: i64 = 60_000;
const FETCH_HEADERS_MAX: usize = 64;
/// Paths a file trigger passes its run (the rest are counted).
const FILES_INPUT_MAX: usize = 200;
const ERROR_MAX_CHARS: usize = 2000;

pub(crate) struct NewRun<'a> {
    pub op: &'a str,
    /// `call`, `job`, or the trigger: `cron`, `channel`, `files`
    pub via: &'a str,
    pub trigger: Option<String>,
    pub principal: &'a str,
    pub role: Role,
    pub depth: u32,
    /// (operation id, input hash) when a caller named the run: calling
    /// again with that id answers the same run.
    pub call: Option<(&'a str, &'a str)>,
    pub input: Value,
}

pub(crate) struct Started {
    pub id: i64,
    pub status: RunStatus,
    pub replayed: bool,
}

/// Why a step did not produce a value: for good (the job sees an error it
/// may catch), or for now (the Workflow retries the step).
pub(crate) enum StepFail {
    Permanent(String),
    Retry(String),
}

pub(crate) fn permanent(m: impl Into<String>) -> StepFail {
    StepFail::Permanent(m.into())
}

fn clip(s: &str) -> String {
    s.chars().take(ERROR_MAX_CHARS).collect()
}

fn run_of(r: &Value, full: bool) -> Run {
    let text = |k: &str| r[k].as_str().map(str::to_string);
    let parsed = |k: &str| if full { r[k].as_str().and_then(|s| serde_json::from_str(s).ok()) } else { None };
    Run {
        id: r["id"].as_i64().unwrap_or(0),
        op: text("op").unwrap_or_default(),
        via: text("via").unwrap_or_default(),
        trigger: text("trigger"),
        principal: r["principal"].as_str().map(npub::display).unwrap_or_default(),
        status: r["status"].as_str().and_then(RunStatus::parse).unwrap_or(RunStatus::Held),
        attempt: r["attempt"].as_u64().unwrap_or(1) as u32,
        depth: r["depth"].as_u64().unwrap_or(0) as u32,
        created_at: r["created_at"].as_i64().unwrap_or(0),
        finished_at: r["finished_at"].as_i64(),
        error: text("error"),
        input: parsed("input"),
        output: parsed("output"),
        cost_micros: r["cost_micros"].as_i64(),
    }
}

/// A run's columns and what its paid steps cost (`spend`, ai.rs).
const RUN_COLUMNS: &str = "*, (SELECT SUM(micros) FROM spend WHERE spend.run = runs.id) AS cost_micros";

fn ids(body: &Value) -> CellResult<(i64, i64)> {
    match (body["run"].as_i64(), body["attempt"].as_i64()) {
        (Some(r), Some(a)) => Ok((r, a)),
        _ => Err(CellError::invalid("a job callback names its run and attempt")),
    }
}

/// Paused operations were a JSON array in meta (`paused`), and each
/// operation's breaker a meta key (`breaker_since:<op>`); both move into
/// their tables once, in place (`fragment_core::trigger_state` decides
/// what they get). Answers the operations paused because `paused` did not
/// parse (it fails closed), for the caller's event.
pub(crate) fn migrate_trigger_state(sql: &SqlStorage, now: i64) -> Vec<String> {
    let rows = |q: &str| -> Vec<Value> { sql.exec(q, None).and_then(|c| c.to_array()).expect("the trigger state migration reads") };
    let exec = |q: &str, binds: Vec<SqlStorageValue>| {
        sql.exec(q, binds).expect("the trigger state migration writes");
    };
    let text = |r: &Value, k: &str| r[k].as_str().map(str::to_string);
    let paused = rows("SELECT value FROM meta WHERE key = 'paused'").first().and_then(|r| text(r, "value"));
    let breakers: Vec<(String, String)> = rows("SELECT key, value FROM meta WHERE key LIKE 'breaker_since:%'")
        .iter()
        .map(|r| (text(r, "key").expect("meta.key is TEXT"), text(r, "value").expect("meta.value is TEXT")))
        .collect();
    if paused.is_none() && breakers.is_empty() {
        return vec![];
    }
    // the installed code's tables, which migrate_code filled first (plane.rs)
    let triggered: Vec<String> = rows("SELECT run FROM code_triggers ORDER BY idx").iter().map(|r| text(r, "run").expect("code_triggers.run is TEXT")).collect();
    let moved = trigger_state::migrate(paused.as_deref(), &triggered, &breakers);
    for op in &moved.paused {
        exec("INSERT INTO paused_ops (op, by, at) VALUES (?, 'migrated', ?) ON CONFLICT (op) DO NOTHING", vec![op.as_str().into(), SqlStorageValue::Integer(now)]);
    }
    for (op, reset_at) in &moved.breakers {
        exec("INSERT INTO op_breakers (op, reset_at) VALUES (?, ?) ON CONFLICT (op) DO NOTHING", vec![op.as_str().into(), SqlStorageValue::Integer(*reset_at)]);
    }
    exec("DELETE FROM meta WHERE key = 'paused'", vec![]);
    for (key, _) in &breakers {
        exec("DELETE FROM meta WHERE key = ?", vec![key.as_str().into()]);
    }
    if moved.failed_closed {
        moved.paused
    } else {
        vec![]
    }
}

impl FragmentCell {
    /// The fragment's own key: who triggered runs act as.
    fn own_key(&self) -> CellResult<String> {
        npub::parse(&self.must("npub")?).ok_or_else(|| CellError::host("the stored npub does not parse"))
    }

    /// The operations whose triggers are paused, in the order they were.
    pub(crate) fn paused_ops(&self) -> CellResult<Vec<String>> {
        let rows = self.rows("SELECT op FROM paused_ops ORDER BY at, op", vec![])?;
        Ok(rows.iter().map(|r| r["op"].as_str().expect("paused_ops.op is TEXT").to_string()).collect())
    }

    fn is_paused(&self, op: &str) -> CellResult<bool> {
        Ok(!self.rows("SELECT op FROM paused_ops WHERE op = ?", vec![op.into()])?.is_empty())
    }

    /// Where an operation's auto-pause count starts: held runs finished
    /// before its last unpause do not count.
    fn breaker_reset_at(&self, op: &str) -> CellResult<i64> {
        let rows = self.rows("SELECT reset_at FROM op_breakers WHERE op = ?", vec![op.into()])?;
        Ok(rows.first().map_or(0, |r| r["reset_at"].as_i64().expect("op_breakers.reset_at is INTEGER")))
    }

    /// Pauses or unpauses an operation's triggers; `by` is an npub or `auto`.
    /// Answers whether that changed anything.
    fn set_paused(&self, op: &str, paused: bool, by: &str, why: &str) -> CellResult<bool> {
        let now = SqlStorageValue::Integer(js::now_ms());
        let changed = if paused {
            self.rows("INSERT INTO paused_ops (op, by, at) VALUES (?, ?, ?) ON CONFLICT (op) DO NOTHING RETURNING op", vec![op.into(), by.into(), now])?
        } else {
            let gone = self.rows("DELETE FROM paused_ops WHERE op = ? RETURNING op", vec![op.into()])?;
            if !gone.is_empty() {
                // held runs from before the unpause no longer count toward the next auto-pause
                self.exec(
                    "INSERT INTO op_breakers (op, reset_at) VALUES (?, ?) ON CONFLICT (op) DO UPDATE SET reset_at = excluded.reset_at",
                    vec![op.into(), now],
                )?;
            }
            gone
        };
        if changed.is_empty() {
            return Ok(false);
        }
        assert_eq!(self.is_paused(op)?, paused, "the pause of {op} was written");
        let kind = match (paused, by) {
            (true, "auto") => "op.auto-paused",
            (true, _) => "op.paused",
            (false, _) => "op.unpaused",
        };
        let summary = if why.is_empty() { format!("{op} by {by}") } else { format!("{op} by {by}: {why}") };
        self.event(kind, &summary, json!({ "op": op, "by": by }));
        Ok(true)
    }

    /// Records a run: queued to start, or blocked. The caller launches
    /// queued runs (`launch_queued`) once it is done writing.
    pub(crate) fn start_run(&self, r: NewRun<'_>) -> CellResult<Started> {
        if let Some((id, sha)) = r.call {
            let prior = self.rows("SELECT id, status, input_sha FROM runs WHERE principal = ? AND call_id = ?", vec![r.principal.into(), id.into()])?;
            if let Some(p) = prior.first() {
                if p["input_sha"].as_str() != Some(sha) {
                    return Err(CellError::new(ErrorCode::ConflictingBody, "this operation id was already used with a different input"));
                }
                let status = p["status"].as_str().and_then(RunStatus::parse).unwrap_or(RunStatus::Held);
                return Ok(Started { id: p["id"].as_i64().unwrap_or(0), status, replayed: true });
            }
        }
        let triggered = !matches!(r.via, "call" | "job");
        let now = js::now_ms();
        let blocked = if r.depth > limits::HOP_DEPTH_MAX {
            let why = format!("{} hops deep: triggered runs chained past the budget of {} (a loop?)", r.depth, limits::HOP_DEPTH_MAX);
            self.event("cycle.detected", &format!("{}: {why}", r.op), json!({ "op": r.op, "depth": r.depth }));
            Some(why)
        } else if triggered && self.is_paused(r.op)? {
            Some(format!("{} is paused (`fragment unpause`)", r.op))
        } else if triggered
            && self.count_of(
                "SELECT COUNT(*) AS n FROM runs WHERE op = ? AND via NOT IN ('call', 'job') AND status != 'blocked' AND created_at > ?",
                vec![r.op.into(), SqlStorageValue::Integer(now - 3_600_000)],
            )? >= limits::TRIGGERED_RUNS_PER_HOUR
        {
            let why = format!("{} triggered runs in the last hour", limits::TRIGGERED_RUNS_PER_HOUR);
            self.set_paused(r.op, true, "auto", &why)?;
            Some(format!("{} was paused: {why}", r.op))
        } else {
            None
        };
        let status = if blocked.is_some() { RunStatus::Blocked } else { RunStatus::Queued };
        let (call_id, sha) = match r.call {
            Some((id, sha)) => (SqlStorageValue::from(id), SqlStorageValue::from(sha)),
            None => (SqlStorageValue::Null, SqlStorageValue::Null),
        };
        let opt = |s: Option<&str>| s.map_or(SqlStorageValue::Null, SqlStorageValue::from);
        let finished = if blocked.is_some() { SqlStorageValue::Integer(now) } else { SqlStorageValue::Null };
        let rows = self.rows(
            "INSERT INTO runs (op, via, trigger, principal, role, depth, call_id, input_sha, input, status, attempt, error, created_at, finished_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?) RETURNING id",
            vec![
                r.op.into(),
                r.via.into(),
                opt(r.trigger.as_deref()),
                r.principal.into(),
                r.role.as_str().into(),
                SqlStorageValue::Integer(r.depth.into()),
                call_id,
                sha,
                r.input.to_string().into(),
                status.as_str().into(),
                opt(blocked.as_deref()),
                SqlStorageValue::Integer(now),
                finished,
            ],
        )?;
        // A blocked run is its own record (no event each: a paused inbox would flood the log).
        let id = rows.first().and_then(|r| r["id"].as_i64()).ok_or_else(|| CellError::host("a run insert returned no id"))?;
        Ok(Started { id, status, replayed: false })
    }

    fn count_of(&self, q: &str, binds: Vec<SqlStorageValue>) -> CellResult<u64> {
        Ok(self.rows(q, binds)?.first().and_then(|r| r["n"].as_u64()).unwrap_or(0))
    }

    /// A Workflow instance id: unique across the fleet (the binding is shared
    /// by every fragment) and across a deleted fragment's reincarnations.
    fn instance_id(&self, run: i64, attempt: i64) -> CellResult<String> {
        let npub = self.must("npub")?;
        Ok(format!("{}-{}-r{run}-a{attempt}", &npub[5..25], self.must("created_at")?))
    }

    /// Starts the Workflows of queued runs. A failure leaves them queued for
    /// the alarm to try again.
    pub(crate) async fn launch_queued(&self) {
        let Ok(rows) = self.rows("SELECT id, op, via, attempt FROM runs WHERE status = 'queued' ORDER BY id LIMIT ?", vec![SqlStorageValue::Integer(LAUNCH_BATCH as i64)])
        else {
            return;
        };
        let (Ok(name), Ok(incarnation)) = (self.name(), self.must("created_at")) else { return };
        for r in rows {
            let (id, attempt) = (r["id"].as_i64().unwrap_or(0), r["attempt"].as_i64().unwrap_or(1));
            let Ok(instance) = self.instance_id(id, attempt) else { return };
            let params = json!({ "fragment": name, "incarnation": incarnation, "run": id, "attempt": attempt });
            match js::jobs_create(self.env.as_ref(), &instance, &params).await {
                Ok(()) => {
                    let _ = self.exec(
                        "UPDATE runs SET status = 'running', launched_at = ? WHERE id = ? AND attempt = ? AND status = 'queued'",
                        vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(id), SqlStorageValue::Integer(attempt)],
                    );
                    self.event(
                        "run.started",
                        &format!("{} run #{id} (attempt {attempt}, via {})", r["op"].as_str().unwrap_or(""), r["via"].as_str().unwrap_or("")),
                        json!({ "run": id, "attempt": attempt }),
                    );
                }
                Err(e) => {
                    self.event("run.launch-failed", &format!("run #{id}: its Workflow did not start: {}", e.message), json!({ "run": id }));
                    let _ = self.schedule().await;
                    return;
                }
            }
        }
    }

    /// The run a Workflow callback is for, if it is still that attempt and
    /// unfinished (a replay or a finish makes older instances stop).
    fn current_run(&self, run: i64, attempt: i64) -> CellResult<Option<Value>> {
        let rows = self.rows(
            "SELECT * FROM runs WHERE id = ? AND attempt = ? AND status IN ('queued', 'running')",
            vec![SqlStorageValue::Integer(run), SqlStorageValue::Integer(attempt)],
        )?;
        if rows.first().is_some_and(|r| r["status"] == "queued") {
            // its Workflow called before the launch that created it returned
            self.exec(
                "UPDATE runs SET status = 'running', launched_at = ? WHERE id = ? AND status = 'queued'",
                vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(run)],
            )?;
        }
        Ok(rows.into_iter().next())
    }

    /// Records a run's outcome (once: a second report is ignored).
    fn finish_run(&self, run: &Value, outcome: Result<Value, String>) -> CellResult<()> {
        let (id, attempt, op) = (run["id"].as_i64().unwrap_or(0), run["attempt"].as_i64().unwrap_or(1), run["op"].as_str().unwrap_or(""));
        let (status, output, error) = match &outcome {
            Ok(v) => (RunStatus::Succeeded, SqlStorageValue::from(v.to_string()), SqlStorageValue::Null),
            Err(e) => (RunStatus::Held, SqlStorageValue::Null, SqlStorageValue::from(clip(e))),
        };
        let now = js::now_ms();
        let changed = self.rows(
            "UPDATE runs SET status = ?, output = ?, error = ?, finished_at = ? WHERE id = ? AND attempt = ? AND status IN ('queued', 'running') RETURNING id",
            vec![status.as_str().into(), output, error, SqlStorageValue::Integer(now), SqlStorageValue::Integer(id), SqlStorageValue::Integer(attempt)],
        )?;
        if changed.is_empty() {
            return Ok(());
        }
        match outcome {
            Ok(_) => self.event("run.succeeded", &format!("{op} run #{id}"), json!({ "op": op, "run": id })),
            Err(e) => {
                self.event("run.held", &format!("{op} run #{id}: {}", clip(&e)), json!({ "op": op, "run": id, "attempt": attempt }));
                let since = self.breaker_reset_at(op)?;
                let held = self.count_of(
                    "SELECT COUNT(*) AS n FROM runs WHERE op = ? AND status = 'held' AND finished_at > ?",
                    vec![op.into(), SqlStorageValue::Integer((now - limits::AUTO_PAUSE_WINDOW_MS).max(since))],
                )?;
                if held >= limits::AUTO_PAUSE_HELD {
                    let why = format!("{held} held runs in {} minutes; last: {}", limits::AUTO_PAUSE_WINDOW_MS / 60_000, clip(&e));
                    self.set_paused(op, true, "auto", &why)?;
                }
            }
        }
        Ok(())
    }

    /// `POST /job/advance`
    pub(crate) async fn job_advance(&self, body: Value, size: usize) -> CellResult<Value> {
        let answer = self.advance(body, size).await?;
        if answer.get("failed").is_some() {
            // the held run polls its videos no more: their reservations go back
            self.release_held_videos().await;
        }
        Ok(answer)
    }

    async fn advance(&self, body: Value, size: usize) -> CellResult<Value> {
        let (run_id, attempt) = ids(&body)?;
        let Some(run) = self.current_run(run_id, attempt)? else { return Ok(json!({ "stop": true })) };
        let fail = |why: String| -> CellResult<Value> {
            self.finish_run(&run, Err(why.clone()))?;
            Ok(json!({ "failed": why }))
        };
        if size > limits::JOB_RESULTS_MAX_BYTES {
            return fail(format!("its step results are over {} bytes together", limits::JOB_RESULTS_MAX_BYTES));
        }
        let results = body["results"].as_array().cloned().unwrap_or_default();
        let op = run["op"].as_str().unwrap_or("").to_string();
        let input: Value = run["input"].as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
        let decl = match self.declared(&op) {
            Ok(d) => d,
            Err(e) => return fail(format!("{op}: {}", e.message)),
        };
        match decl.kind {
            // A triggered mutation is a run of one step: the call.
            OpKind::Mutation => match results.first() {
                None => Ok(json!({ "step": { "index": 0, "kind": "call", "args": { "op": op, "input": input } } })),
                Some(r) => match r.get("error") {
                    Some(e) => fail(e.as_str().unwrap_or("the call failed").to_string()),
                    None => {
                        self.finish_run(&run, Ok(r["value"].clone()))?;
                        Ok(json!({ "done": true }))
                    }
                },
            },
            OpKind::Job => {
                let facet = self.facet()?;
                self.sweep(&facet).await?;
                let principal = run["principal"].as_str().unwrap_or("");
                let meta = json!({
                    "principal": npub::display(principal),
                    "role": run["role"],
                    "run": run_id,
                    "attempt": attempt,
                    "channels": self.declared_channels()?.keys().collect::<Vec<_>>(),
                });
                let answer = facet.call("__job", &[op.as_str().into(), input, meta, Value::Array(results)]).await.map_err(|e| match e.code {
                    ErrorCode::NodeFull => e,
                    _ => CellError::host(format!("the app facet: {}", e.message)),
                })?;
                if let Some(next) = answer.get("next") {
                    if next["index"].as_u64().unwrap_or(0) as usize >= limits::JOB_STEPS_MAX {
                        return fail(format!("a job takes at most {} steps", limits::JOB_STEPS_MAX));
                    }
                    return Ok(json!({ "step": next }));
                }
                if answer["done"] == true {
                    self.finish_run(&run, Ok(answer["output"].clone()))?;
                    return Ok(json!({ "done": true }));
                }
                if let Some(why) = answer["failed"].as_str() {
                    return fail(why.to_string());
                }
                if answer["error"] == "unknown_operation" {
                    return fail(format!("the app has no method {op:?}"));
                }
                Err(CellError::host(format!("the facet's platform code answered {answer}")))
            }
            OpKind::Query => fail(format!("{op} is a query; queries do not run as jobs")),
        }
    }

    /// `POST /job/effect`: one step. A value or a lasting failure is the
    /// step's result; a passing failure is a 502, which the Workflow retries.
    pub(crate) async fn job_effect(&self, body: Value) -> CellResult<Value> {
        let (run_id, attempt) = ids(&body)?;
        let Some(run) = self.current_run(run_id, attempt)? else { return Ok(json!({ "stop": true })) };
        let index = body["index"].as_i64().ok_or_else(|| CellError::invalid("a step has an index"))?;
        let kind = body["kind"].as_str().unwrap_or("").to_string();
        let args = &body["args"];
        let out = match kind.as_str() {
            "call" => self.step_call(&run, index, args).await,
            "fetch" => self.step_fetch(&run, args).await,
            "publish" => self.step_publish(&run, index, args).await,
            "push" => {
                let key = format!("{JOB_ID_PREFIX}{run_id}:{index}");
                match self.send_push(&key, args["who"].as_str().unwrap_or(""), &args["payload"]).await {
                    Ok(n) => Ok(json!({ "queued": n })),
                    Err(e) if e.code == ErrorCode::HostFailed => Err(StepFail::Retry(e.message)),
                    Err(e) => Err(permanent(e.message)),
                }
            }
            "files.read" | "files.list" | "files.stat" | "files.write" | "files.remove" => self.step_files(&run, index, &kind, args).await,
            k if k.starts_with("ai.") => self.step_ai(&run, index, k, args).await,
            other => Err(permanent(format!("unknown step kind {other:?}"))),
        };
        let answer = match out {
            Ok(v) => json!({ "kind": kind, "value": v }),
            Err(StepFail::Permanent(m)) => json!({ "kind": kind, "error": clip(&m) }),
            Err(StepFail::Retry(m)) => return Err(CellError::new(ErrorCode::UpstreamFailed, m)),
        };
        // The Workflow stores each step result, up to 1 MiB.
        let answer = if answer.to_string().len() > limits::RESULT_MAX_BYTES - 4096 {
            json!({ "kind": kind, "error": format!("the step's result is over {} bytes", limits::RESULT_MAX_BYTES - 4096) })
        } else {
            answer
        };
        self.launch_queued().await;
        Ok(answer)
    }

    /// `POST /job/finish`: the Workflow ran out of retries outside a step.
    pub(crate) fn job_finish(&self, body: Value) -> CellResult<Value> {
        let (run_id, attempt) = ids(&body)?;
        if let Some(run) = self.current_run(run_id, attempt)? {
            self.finish_run(&run, Err(body["error"].as_str().unwrap_or("the Workflow failed").to_string()))?;
        }
        Ok(json!({ "ok": true }))
    }

    /// `job.call(op, input)`: as the run's principal, with the step as its
    /// operation id, so a retried or replayed step is a replay.
    async fn step_call(&self, run: &Value, index: i64, args: &Value) -> Result<Value, StepFail> {
        let op = args["op"].as_str().ok_or_else(|| permanent("call names an operation"))?;
        let decl = self.declared(op).map_err(|e| permanent(e.message))?;
        let role = run["role"].as_str().and_then(Role::parse).unwrap_or(Role::Public);
        if role < decl.role {
            return Err(permanent(format!("{op} needs the {} role; this run acts as {}", decl.role.as_str(), role.as_str())));
        }
        let run_id = run["id"].as_i64().unwrap_or(0);
        let depth = run["depth"].as_u64().unwrap_or(0) as u32;
        let child = decl.kind == OpKind::Job;
        let inv = Invocation {
            principal: run["principal"].as_str().unwrap_or(""),
            role,
            op,
            decl,
            id: format!("{JOB_ID_PREFIX}{run_id}:{index}"),
            input: args["input"].clone(),
            depth: if child { depth + 1 } else { depth },
            via: "job",
            trigger: Some(format!("run {run_id}")),
        };
        match self.invoke(inv).await {
            Ok(r) => Ok(r.result),
            Err(e) => match e.code {
                ErrorCode::HostFailed | ErrorCode::UpstreamFailed => Err(StepFail::Retry(e.message)),
                _ => Err(permanent(format!("{op}: {}", e.message))),
            },
        }
    }

    /// `job.fetch(url, init)`: the fragment's one way out. Header values
    /// may name secrets as `{{NAME}}`; they are opened here, at the egress
    /// point, and never reach the app.
    async fn step_fetch(&self, run: &Value, args: &Value) -> Result<Value, StepFail> {
        let url = egress::check(args["url"].as_str().unwrap_or(""), self.cfg.egress_local).map_err(permanent)?;
        let method = match args["method"].as_str().unwrap_or("GET").to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "HEAD" => Method::Head,
            other => return Err(permanent(format!("method {other} is not supported"))),
        };
        let headers = Headers::new();
        let given = args["headers"].as_object().cloned().unwrap_or_default();
        if given.len() > FETCH_HEADERS_MAX {
            return Err(permanent(format!("at most {FETCH_HEADERS_MAX} headers")));
        }
        for (k, v) in &given {
            let mut value = v.as_str().ok_or_else(|| permanent(format!("header {k} must be a string")))?.to_string();
            for name in placeholders(&value.clone()) {
                if !valid_secret_name(name) {
                    return Err(permanent(format!("{{{{{name}}}}} is not a secret name (^[A-Z][A-Z0-9_]*$)")));
                }
                let secret = self
                    .open_secret(name)
                    .await
                    .map_err(|e| StepFail::Retry(e.message))?
                    .ok_or_else(|| permanent(format!("no secret named {name} (set it with `fragment secret set`)")))?;
                let secret = String::from_utf8(secret).map_err(|_| permanent(format!("secret {name} is not UTF-8 text")))?;
                value = value.replace(&format!("{{{{{name}}}}}"), &secret);
            }
            headers.set(k, &value).map_err(|e| permanent(format!("header {k}: {e}")))?;
        }
        let depth = run["depth"].as_u64().unwrap_or(0);
        headers.set(HOPS_HEADER, &(depth + 1).to_string()).map_err(|e| permanent(e.to_string()))?;
        let mut init = RequestInit::new();
        // A redirect comes back to the job as its 3xx: following it here
        // would skip the egress check for where it points.
        init.with_method(method).with_headers(headers).with_redirect(RequestRedirect::Manual);
        if let Some(body) = args["body"].as_str() {
            if body.len() > limits::FETCH_BODY_MAX_BYTES {
                return Err(permanent(format!("a fetch body is at most {} bytes", limits::FETCH_BODY_MAX_BYTES)));
            }
            init.with_body(Some(JsValue::from_str(body)));
        }
        let req = Request::new_with_init(url.as_str(), &init).map_err(|e| permanent(e.to_string()))?;
        let host = url.host_str().unwrap_or("").to_string();
        let mut resp = crate::cs::fetch(req, Duration::from_millis(limits::FETCH_TIMEOUT_MS)).await.map_err(|e| match e {
            // the node refused the address (CELLD_EGRESS_PUBLIC_ONLY): no retry passes
            FetchError::Refused(why) => permanent(format!("{host}: {why}")),
            FetchError::Failed(why) => StepFail::Retry(format!("{host}: {why}")),
        })?;
        let status = resp.status_code();
        if status == 429 || status >= 500 {
            return Err(StepFail::Retry(format!("{host} answered {status}")));
        }
        let declared: usize = resp.headers().get("content-length").ok().flatten().and_then(|l| l.parse().ok()).unwrap_or(0);
        if declared > limits::FETCH_RESPONSE_MAX_BYTES {
            return Err(permanent(format!("{host} answered {declared} bytes; a fetch reads at most {}", limits::FETCH_RESPONSE_MAX_BYTES)));
        }
        let bytes = resp.bytes().await.map_err(|e| StepFail::Retry(format!("{host}: {e}")))?;
        if bytes.len() > limits::FETCH_RESPONSE_MAX_BYTES {
            return Err(permanent(format!("{host} answered {} bytes; a fetch reads at most {}", bytes.len(), limits::FETCH_RESPONSE_MAX_BYTES)));
        }
        let out_headers: Map<String, Value> = resp.headers().entries().map(|(k, v)| (k.to_ascii_lowercase(), Value::String(v))).collect();
        Ok(json!({ "status": status, "headers": out_headers, "body": String::from_utf8_lossy(&bytes) }))
    }

    /// `job.publish(channel, body, kind)`: keyed by (run, step), so a
    /// retried step appends nothing twice, and starts what its record's
    /// triggers did not start before.
    async fn step_publish(&self, run: &Value, index: i64, args: &Value) -> Result<Value, StepFail> {
        let retry = |e: CellError| StepFail::Retry(e.message);
        let channel = args["channel"].as_str().unwrap_or("");
        let kind = args["kind"].as_str().unwrap_or("message");
        let body = args.get("body").cloned().unwrap_or(Value::Null);
        // the one check a mutation's records meet too
        fragment_core::effects::check_record(channel, kind, &body, &self.declared_channels().map_err(retry)?).map_err(permanent)?;
        let key = format!("{JOB_ID_PREFIX}{}", run["id"].as_i64().unwrap_or(0));
        let principal = run["principal"].as_str().unwrap_or("");
        let (record, appended) = self.append_once(channel, principal, kind, &body, &key, index).map_err(retry)?;
        let depth = run["depth"].as_u64().unwrap_or(0) as u32;
        self.published(&record, appended, depth + 1).await.map_err(retry)?;
        Ok(json!({ "seq": record.seq }))
    }

    /// `job.files.*`: reads at `main`'s pin, recorded as the step's result;
    /// writes as one commit per step, compare-and-swapped when the step
    /// names what it expects (`expect`: a blob sha, or `null` for absent).
    async fn step_files(&self, run: &Value, index: i64, kind: &str, args: &Value) -> Result<Value, StepFail> {
        let settle = |e: CellError| match e.code {
            ErrorCode::HostFailed | ErrorCode::UpstreamFailed => StepFail::Retry(e.message),
            _ => permanent(e.message),
        };
        let path = args["path"].as_str().unwrap_or("");
        match kind {
            "files.read" => Ok(self.read_main(path).await.map_err(settle)?.map_or(Value::Null, crate::files::content_json)),
            "files.list" => Ok(Value::Array(self.list_main(args["prefix"].as_str().unwrap_or("")).map_err(settle)?)),
            "files.stat" => Ok(self.stat_main(path).await.map_err(settle)?.unwrap_or(Value::Null)),
            _ => {
                let bytes = match kind {
                    "files.write" => Some(crate::files::content_of(args).map_err(permanent)?),
                    _ => None,
                };
                if bytes.as_deref().is_some_and(|b| fragment_core::blob::parse(b).is_some()) {
                    return Err(permanent(format!("{path}: an app does not write blob pointers")));
                }
                let mut expect = BTreeMap::new();
                if let Some(e) = args.get("expect") {
                    expect.insert(path.to_string(), e.as_str().map(str::to_string));
                }
                let run_id = run["id"].as_i64().unwrap_or(0);
                let key = format!("{JOB_ID_PREFIX}{run_id}:{index}");
                let message = format!("{} run {run_id}: {} {path}", run["op"].as_str().unwrap_or(""), if bytes.is_some() { "write" } else { "remove" });
                let writes = [crate::files::FileWrite { path: path.to_string(), bytes }];
                let depth = run["depth"].as_u64().unwrap_or(0) as u32;
                match self.commit_files(&key, &writes, &expect, &message, run["principal"].as_str().unwrap_or(""), depth).await.map_err(settle)? {
                    crate::files::Wrote::Commit(sha) => Ok(json!({ "commit": sha })),
                    crate::files::Wrote::Conflict(why) => Err(permanent(format!("conflict: {why}"))),
                }
            }
        }
    }

    /// Starts the runs a record on a channel triggers, one for each
    /// operation its triggers run. Each is the fragment's own call, its id
    /// the record's (`record:<channel>:<seq>:<op>`), so this again for the
    /// same record answers the runs it started: a try after a failure part
    /// way starts only the rest, never one twice.
    pub(crate) fn fire_channel(&self, record: &ChannelRecord, depth: u32) -> CellResult<Vec<i64>> {
        let ops = self.channel_triggers(&record.channel)?;
        let own = self.own_key()?;
        let input = json!({ "channel": record.channel, "record": record });
        let mut started = vec![];
        for (i, op) in ops.iter().enumerate() {
            if i + 1 == ops.len() {
                self.test_trigger_failure()?;
            }
            let call_id = format!("record:{}:{}:{op}", record.channel, record.seq);
            let sha = crate::ops::input_sha(op, &input);
            let s = self.start_run(NewRun {
                op,
                via: "channel",
                trigger: Some(record.channel.clone()),
                principal: &own,
                role: Role::Editor,
                depth,
                call: Some((&call_id, &sha)),
                input: input.clone(),
            })?;
            started.push(s.id);
        }
        Ok(started)
    }

    /// Test fleets: the next `times` trigger steps fail just before their
    /// last run starts (`/api/test/fragment` `fail-triggers`), after the
    /// record and its deliveries, and any runs before it, are written.
    fn test_trigger_failure(&self) -> CellResult<()> {
        if !self.cfg.test_hooks {
            return Ok(());
        }
        let left: u64 = self.meta(TEST_TRIGGER_FAILURES_KEY)?.and_then(|n| n.parse().ok()).unwrap_or(0);
        if left == 0 {
            return Ok(());
        }
        self.set_meta(TEST_TRIGGER_FAILURES_KEY, &(left - 1).to_string())?;
        Err(CellError::host("the trigger step failed before its last run started (a test hook)"))
    }

    /// Starts the runs a move of `main` triggers.
    pub(crate) fn fire_files(&self, commit: Option<&str>, paths: &[String], depth: u32) -> CellResult<()> {
        for t in self.triggers()? {
            let TriggerOn::Files(pattern) = &t.on else { continue };
            let matched: Vec<&String> = paths.iter().filter(|p| glob::matches_path(pattern, p)).collect();
            if matched.is_empty() {
                continue;
            }
            self.start_run(NewRun {
                op: &t.run,
                via: "files",
                trigger: Some(pattern.clone()),
                principal: &self.own_key()?,
                role: Role::Editor,
                depth,
                call: None,
                input: json!({
                    "ref": "main",
                    "commit": commit,
                    "paths": matched.iter().take(FILES_INPUT_MAX).collect::<Vec<_>>(),
                    "more": matched.len().saturating_sub(FILES_INPUT_MAX),
                }),
            })?;
        }
        Ok(())
    }

    /// After code is installed: an operation it does not have loses its
    /// pause and its breaker (one of that name later starts clean). Only
    /// installed code says an operation is gone; a live commit with no app
    /// says nothing about the next one, so it keeps them (plane.rs).
    pub(crate) fn forget_undeclared_pauses(&self) -> CellResult<()> {
        assert!(!self.rows("SELECT id FROM code WHERE id = 1", vec![])?.is_empty(), "pauses are forgotten only against installed code");
        let declared = self.operations()?;
        for table in ["paused_ops", "op_breakers"] {
            for row in self.rows(&format!("SELECT op FROM {table}"), vec![])? {
                let op = row["op"].as_str().expect("op is TEXT");
                if !declared.contains_key(op) {
                    self.exec(&format!("DELETE FROM {table} WHERE op = ?"), vec![op.into()])?;
                }
            }
        }
        Ok(())
    }

    /// Rebuilds the cron schedules from newly installed triggers (none,
    /// when the live commit has no app). A schedule that did not change
    /// keeps its next time, so a redeploy neither skips nor repeats a tick.
    pub(crate) fn sync_schedules(&self, triggers: &[TriggerDecl]) -> CellResult<()> {
        let prior: BTreeMap<(String, String), i64> = self
            .rows("SELECT op, cron, next_at FROM schedules", vec![])?
            .iter()
            .map(|r| ((r["op"].as_str().unwrap_or("").to_string(), r["cron"].as_str().unwrap_or("").to_string()), r["next_at"].as_i64().unwrap_or(0)))
            .collect();
        self.exec("DELETE FROM schedules", vec![])?;
        let now = js::now_ms();
        for (i, t) in triggers.iter().enumerate() {
            let TriggerOn::Cron(expr) = &t.on else { continue };
            let next = prior.get(&(t.run.clone(), expr.clone())).copied().or_else(|| Cron::parse(expr).ok()?.next_after(now));
            if let Some(at) = next {
                self.exec(
                    "INSERT INTO schedules (idx, op, cron, next_at) VALUES (?, ?, ?, ?)",
                    vec![SqlStorageValue::Integer(i as i64), t.run.as_str().into(), expr.as_str().into(), SqlStorageValue::Integer(at)],
                )?;
            }
        }
        Ok(())
    }

    /// From the alarm: starts the runs whose schedules are due. A schedule
    /// whose previous run is still going skips this tick.
    pub(crate) fn fire_cron(&self) -> CellResult<()> {
        let now = js::now_ms();
        for row in self.rows("SELECT idx, op, cron, next_at FROM schedules WHERE next_at <= ?", vec![SqlStorageValue::Integer(now)])? {
            let (op, expr, at) = (row["op"].as_str().unwrap_or(""), row["cron"].as_str().unwrap_or(""), row["next_at"].as_i64().unwrap_or(now));
            let busy = self.count_of(
                "SELECT COUNT(*) AS n FROM runs WHERE op = ? AND via = 'cron' AND status IN ('queued', 'running')",
                vec![op.into()],
            )? > 0;
            if busy {
                self.event("cron.skipped", &format!("{op} ({expr}): the previous run is still going"), json!({ "op": op }));
            } else if !self.is_paused(op)? {
                self.start_run(NewRun {
                    op,
                    via: "cron",
                    trigger: Some(expr.to_string()),
                    principal: &self.own_key()?,
                    role: Role::Editor,
                    depth: 0,
                    call: None,
                    input: json!({ "cron": expr, "at": at }),
                })?;
            }
            match Cron::parse(expr).ok().and_then(|c| c.next_after(now.max(at))) {
                Some(next) => self.exec("UPDATE schedules SET next_at = ? WHERE idx = ?", vec![SqlStorageValue::Integer(next), row["idx"].as_i64().unwrap_or(0).into()])?,
                None => self.exec("DELETE FROM schedules WHERE idx = ?", vec![row["idx"].as_i64().unwrap_or(0).into()])?,
            }
        }
        Ok(())
    }

    /// When the alarm must next wake for runs: a due schedule, or queued
    /// runs whose Workflows did not start.
    pub(crate) fn runs_due_at(&self) -> CellResult<Option<i64>> {
        let cron = self.rows("SELECT MIN(next_at) AS at FROM schedules", vec![])?.first().and_then(|r| r["at"].as_i64());
        let queued = self.count_of("SELECT COUNT(*) AS n FROM runs WHERE status = 'queued'", vec![])? > 0;
        let retry = queued.then(|| js::now_ms() + QUEUED_RETRY_MS);
        Ok([cron, retry].into_iter().flatten().min())
    }

    /// From the alarm: a running run whose Workflow ended without reporting
    /// (it errored outside a step, or it is gone) is held.
    pub(crate) async fn reconcile_runs(&self) {
        let Ok(rows) = self.rows(
            "SELECT * FROM runs WHERE status = 'running' AND launched_at < ? ORDER BY launched_at LIMIT ?",
            vec![SqlStorageValue::Integer(js::now_ms() - RECONCILE_AFTER_MS), SqlStorageValue::Integer(RECONCILE_BATCH)],
        ) else {
            return;
        };
        for run in rows {
            let Ok(instance) = self.instance_id(run["id"].as_i64().unwrap_or(0), run["attempt"].as_i64().unwrap_or(1)) else { return };
            let why = match js::jobs_status(self.env.as_ref(), &instance).await {
                Ok(Some(st)) => match st["status"].as_str() {
                    Some(s @ ("errored" | "terminated" | "complete")) => format!("its Workflow ended ({s}) without reporting: {}", st["error"]),
                    _ => continue,
                },
                Ok(None) => "its Workflow is gone".to_string(),
                // the binding did not answer: ask again on the next pass
                Err(_) => continue,
            };
            let _ = self.finish_run(&run, Err(why));
        }
    }

    /// From the alarm: finished runs past their retention go.
    pub(crate) fn trim_runs(&self) -> CellResult<()> {
        self.exec(
            "DELETE FROM runs WHERE status NOT IN ('queued', 'running') AND (finished_at < ? OR id <= (SELECT MAX(id) FROM runs) - ?)",
            vec![SqlStorageValue::Integer(js::now_ms() - limits::RUN_RETENTION_MS), SqlStorageValue::Integer(limits::RUNS_KEPT)],
        )?;
        // a run's costs go with it (the ledger keeps the month's usage)
        self.exec("DELETE FROM spend WHERE run NOT IN (SELECT id FROM runs)", vec![])
    }

    /// `GET /api/f/<name>/runs?status=&op=&limit=`
    pub(crate) fn runs_api(&self, caller: &Caller, status: Option<String>, op: Option<String>, limit: usize) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let mut q = format!("SELECT {RUN_COLUMNS} FROM runs WHERE 1 = 1");
        let mut binds: Vec<SqlStorageValue> = vec![];
        if let Some(s) = status {
            let s = RunStatus::parse(&s).ok_or_else(|| CellError::invalid("status is queued, running, succeeded, held, or blocked"))?;
            q.push_str(" AND status = ?");
            binds.push(s.as_str().into());
        }
        if let Some(op) = op {
            q.push_str(" AND op = ?");
            binds.push(op.into());
        }
        q.push_str(" ORDER BY id DESC LIMIT ?");
        binds.push(SqlStorageValue::Integer(limit.clamp(1, 200) as i64));
        let runs: Vec<Run> = self.rows(&q, binds)?.iter().map(|r| run_of(r, false)).collect();
        let mut counts = Map::new();
        for r in self.rows("SELECT status, COUNT(*) AS n FROM runs GROUP BY status", vec![])? {
            counts.insert(r["status"].as_str().unwrap_or("").to_string(), r["n"].clone());
        }
        json_response(&json!({ "runs": runs, "counts": counts, "paused": self.paused_ops()? }))
    }

    /// `GET /api/f/<name>/runs/<id>`
    pub(crate) fn run_api(&self, caller: &Caller, id: &str) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let id: i64 = id.parse().map_err(|_| CellError::invalid("a run id is a number"))?;
        let rows = self.rows(&format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ?"), vec![SqlStorageValue::Integer(id)])?;
        let run = rows.first().ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no run #{id}")))?;
        json_response(&run_of(run, true))
    }

    /// `POST /api/f/<name>/replay` (editor): a held or blocked run again, as
    /// a new attempt with its original input. Steps that already applied a
    /// mutation replay it rather than apply it twice.
    pub(crate) async fn replay(&self, caller: &Caller, body: Replay) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        let rows = self.rows(
            "UPDATE runs SET status = 'queued', attempt = attempt + 1, error = NULL, output = NULL, finished_at = NULL, launched_at = NULL
             WHERE id = ? AND status IN ('held', 'blocked') RETURNING op, attempt",
            vec![SqlStorageValue::Integer(body.run)],
        )?;
        let Some(row) = rows.first() else {
            let exists = !self.rows("SELECT id FROM runs WHERE id = ?", vec![SqlStorageValue::Integer(body.run)])?.is_empty();
            return Err(match exists {
                true => CellError::invalid(format!("run #{} is not held or blocked", body.run)),
                false => CellError::new(ErrorCode::NotFound, format!("no run #{}", body.run)),
            });
        };
        let by = npub::display(self.caller_id(caller)?);
        self.event("run.replayed", &format!("{} run #{} by {by}", row["op"].as_str().unwrap_or(""), body.run), json!({ "run": body.run }));
        self.launch_queued().await;
        json_response(&json!({ "ok": true, "run": body.run, "attempt": row["attempt"] }))
    }

    /// `POST /api/f/<name>/pause` (editor)
    pub(crate) fn pause(&self, caller: &Caller, body: SetPaused) -> CellResult<Response> {
        self.require(caller, false, Role::Editor)?;
        self.declared(&body.op)?;
        let by = npub::display(self.caller_id(caller)?);
        self.set_paused(&body.op, body.paused, &by, "")?;
        json_response(&json!({ "ok": true, "op": body.op, "paused": body.paused }))
    }

    /// `GET /api/f/<name>/triggers`
    pub(crate) fn triggers_api(&self, caller: &Caller) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let next: BTreeMap<i64, i64> =
            self.rows("SELECT idx, next_at FROM schedules", vec![])?.iter().filter_map(|r| Some((r["idx"].as_i64()?, r["next_at"].as_i64()?))).collect();
        let paused = self.paused_ops()?;
        let triggers: Vec<Value> = self
            .triggers()?
            .into_iter()
            .enumerate()
            .map(|(i, t)| {
                let mut v = serde_json::to_value(&t).expect("a trigger serializes");
                v["paused"] = json!(paused.contains(&t.run));
                if let Some(at) = next.get(&(i as i64)) {
                    v["nextAt"] = json!(at);
                }
                v
            })
            .collect();
        json_response(&json!({ "triggers": triggers, "paused": paused }))
    }

    /// `POST /api/f/<name>/inbox` (the inbox token): a webhook's delivery,
    /// appended to the `inbox` channel; the operations it triggers run with
    /// the record. Records whose runs have not succeeded are pending; past
    /// the cap a post is refused, so overload is a 429, not memory.
    pub(crate) async fn inbox(&self, token: &str, hops: u32, body: &[u8]) -> CellResult<Response> {
        self.name()?;
        if !crate::serve::eq_ct(token, &self.must("inbox_token")?) {
            return Err(CellError::new(ErrorCode::Forbidden, "bad inbox token (x-fragment-inbox-token, or ?t=)"));
        }
        let pending = self.count_of(
            "SELECT COUNT(*) AS n FROM runs WHERE via = 'channel' AND trigger = 'inbox' AND status IN ('queued', 'running', 'held', 'blocked')",
            vec![],
        )?;
        if pending >= limits::INBOX_PENDING_MAX {
            self.event("inbox.rejected", &format!("the inbox is full ({pending} pending)"), json!({ "pending": pending }));
            return Err(CellError::new(
                ErrorCode::RateLimited,
                format!("the inbox has {pending} records waiting on their runs; replay or fix the held ones, then retry"),
            ));
        }
        if body.len() > limits::RECORD_BODY_MAX_BYTES {
            return Err(CellError::too_large("an inbox delivery", body.len(), limits::RECORD_BODY_MAX_BYTES));
        }
        let parsed: Value = serde_json::from_slice(body).unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).into_owned()));
        let record_body = match parsed.get("payload") {
            Some(payload) => json!({ "source": parsed["source"].as_str().unwrap_or("external"), "payload": payload }),
            None => json!({ "source": "external", "payload": parsed }),
        };
        // unkeyed: a sender's retry is a new record, so a failure of its
        // triggers is the sender's error to see, never a silent 200
        let record = self.append("inbox", "inbox", "message", &record_body, None)?.ok_or_else(|| CellError::host("an inbox append returned nothing"))?;
        let runs = self.published(&record, true, hops).await?;
        self.launch_queued().await;
        json_response(&json!({ "ok": true, "seq": record.seq, "runs": runs }))
    }

    /// A secret's value, for the egress point only (`KEYS` opens it for
    /// this cell alone). A value sealed under a previous host secret, or by
    /// the cell itself before `KEYS`, comes back resealed and is stored so.
    pub(crate) async fn open_secret(&self, name: &str) -> CellResult<Option<Vec<u8>>> {
        let rows = self.rows("SELECT sealed FROM secrets WHERE name = ?", vec![name.into()])?;
        let Some(sealed) = rows.first().and_then(|r| r["sealed"].as_str()).map(str::to_string) else { return Ok(None) };
        let opened = keys::open(&self.env, &sealed, &self.must("npub")?).await.map_err(|e| CellError::host(format!("secret {name}: {}", e.message)))?;
        if let Some(fresh) = opened.resealed {
            self.exec("UPDATE secrets SET sealed = ? WHERE name = ? AND sealed = ?", vec![fresh.into(), name.into(), sealed.into()])?;
            self.event("secret.resealed", &format!("secret {name} resealed under the current host secret"), json!({ "name": name }));
        }
        Ok(Some(opened.plaintext))
    }
}
