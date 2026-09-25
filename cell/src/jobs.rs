//! Jobs, triggers, and the inbox (docs/MODEL.md, Operations; phase 2
//! slice D).
//!
//! A **run** is one execution of an operation outside a request: a job
//! someone called, or a mutation or job a trigger started. Each run is a
//! celld Workflow instance (`Job` in entry.mjs) that drives it one step at
//! a time by calling back into this supervisor:
//!
//!   POST /job/advance {run, attempt, count, failed?}   re-run the job's body
//!        in the app facet over the answers of its first `count` steps, up
//!        to its next step
//!   POST /job/effect  {run, attempt, index, kind, args}   perform that step
//!        (call an operation, fetch with secrets added here, publish, …)
//!        and keep its answer
//!   POST /job/finish  {run, attempt, error}     the Workflow gave up
//!
//! Every callback names the fragment's incarnation, checked before the
//! callback decodes into its struct below; a step's kind and args decode
//! into `fragment_core::steps::Step`, and a run's row into `RunRow`.
//!
//! Each step's answer is kept here (`steps`, by run, attempt, and index)
//! before the Workflow hears that the step is done, and the job's body
//! reads the answers back from there, so an advance carries a count, not
//! every answer so far. A step the Workflow tries again because its reply
//! was lost (a timeout, a crash) is answered from what was kept, not
//! performed again: a fetch reaches its upstream once. Only an answer the
//! cell never gave comes from the Workflow: a step that ran out of its
//! retries, whose error the next advance carries (`failed`). The Workflow
//! records each callback, so a crash resumes at the step it was on, and a
//! step that fails for a reason that may pass (an upstream 5xx, a network
//! error) is retried with backoff. A run that fails for good is **held**:
//! kept with its input until someone replays it; its kept answers go when
//! it finishes, and a replay takes every step afresh.
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
use fragment_core::steps::{Fetch, NextStep, Step, StepOutcome, StepResult};
use fragment_core::{cron::Cron, egress, glob, npub};
use fragment_proto::{
    limits, valid_secret_name, ChannelRecord, ErrorCode, OpKind, Replay, Role, Run, RunList, RunStatus, SetPaused, TriggerDecl, TriggerOn, Via,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use worker::wasm_bindgen::JsValue;
use worker::*;

use crate::cs::FetchError;
use crate::error::{CellError, CellResult};
use crate::fragment::{json_response, missing, Caller, FragmentCell, MetaKey};
use crate::ops::{Invocation, JOB_ID_PREFIX};
use crate::{js, keys};

/// Set by the Workflow on its callbacks; the router never sets or passes it.
pub const JOB_HEADER: &str = "x-fragment-job";
/// How far a fetch carries the chain it is part of (another fragment's inbox reads it).
pub const HOPS_HEADER: &str = "x-fragment-hops";
/// A run whose Workflow could not be started is tried again this soon.
const QUEUED_RETRY_MS: i64 = 10_000;

/// Test fleets: the longest an advance is held (`hold-advances`), and how
/// often a held one looks again.
const TEST_HOLD_MAX_MS: u64 = 20_000;
const TEST_HOLD_POLL_MS: u64 = 50;
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
    pub via: Via,
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

/// A file step's failure: `main` or code.storage not answering passes;
/// anything else (a bad path, a file too large) is for good.
fn settle_files(e: CellError) -> StepFail {
    match e.code {
        ErrorCode::HostFailed | ErrorCode::UpstreamFailed => StepFail::Retry(e.message),
        _ => permanent(e.message),
    }
}

fn clip(s: &str) -> String {
    s.chars().take(ERROR_MAX_CHARS).collect()
}

/// What `kept_answers` found for a run's first steps.
enum Kept {
    All(Vec<StepResult>),
    /// The first step with no kept answer.
    Missing(usize),
    /// Together they are over `JOB_RESULTS_MAX_BYTES`.
    TooLarge,
}

/// A `runs` row as the cell wrote it (`RUN_COLUMNS`), decoded once. Its
/// input and output are read where they are needed.
pub(crate) struct RunRow {
    pub id: i64,
    pub op: String,
    pub via: Via,
    /// The cron schedule, channel, file pattern, or parent run.
    pub trigger: Option<String>,
    /// Who it acts as: an identity, an anonymous visitor, or the fragment's own key.
    pub principal: String,
    pub role: Role,
    pub depth: u32,
    pub status: RunStatus,
    pub attempt: u32,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
}

const RUN_COLUMNS: &str = "id, op, via, trigger, principal, role, depth, status, attempt, created_at, finished_at, error";
/// What a run's paid steps cost (`spend`, ai.rs).
const RUN_COST: &str = "(SELECT SUM(micros) FROM spend WHERE spend.run = runs.id) AS cost_micros";

/// Only the cell writes `runs`: a NOT NULL column that is missing, or a
/// stored state that does not parse, is corruption, never a default.
fn run_row(r: &Value) -> RunRow {
    let text = |k: &str| r[k].as_str().unwrap_or_else(|| panic!("runs.{k} is TEXT NOT NULL: {r}"));
    let int = |k: &str| r[k].as_i64().unwrap_or_else(|| panic!("runs.{k} is INTEGER NOT NULL: {r}"));
    let count = |k: &str| u32::try_from(int(k)).unwrap_or_else(|_| panic!("runs.{k} is a small count: {r}"));
    let row = RunRow {
        id: int("id"),
        op: text("op").to_string(),
        via: Via::parse(text("via")).unwrap_or_else(|| panic!("runs.via is one the cell writes: {r}")),
        trigger: r["trigger"].as_str().map(str::to_string),
        principal: text("principal").to_string(),
        role: Role::parse(text("role")).unwrap_or_else(|| panic!("runs.role is a role: {r}")),
        depth: count("depth"),
        status: RunStatus::parse(text("status")).unwrap_or_else(|| panic!("runs.status is a run status: {r}")),
        attempt: count("attempt"),
        created_at: int("created_at"),
        finished_at: r["finished_at"].as_i64(),
        error: r["error"].as_str().map(str::to_string),
    };
    assert!(row.attempt >= 1, "a run's attempts count from 1: {r}");
    row
}

/// A JSON column the cell wrote from a value.
fn stored_json(r: &Value, column: &str) -> Value {
    let text = r[column].as_str().unwrap_or_else(|| panic!("runs.{column} is TEXT: {r}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("runs.{column} is the JSON the cell wrote: {e}"))
}

/// A run as `GET runs` shows it; `detail` is its input and output, when one run is read.
fn run_view(row: RunRow, cost_micros: Option<i64>, detail: Option<(Value, Option<Value>)>) -> Run {
    let (input, output) = detail.map_or((None, None), |(input, output)| (Some(input), output));
    Run {
        id: row.id,
        op: row.op,
        via: row.via,
        trigger: row.trigger,
        principal: npub::display(&row.principal),
        status: row.status,
        attempt: row.attempt,
        depth: row.depth,
        created_at: row.created_at,
        finished_at: row.finished_at,
        error: row.error,
        input,
        output,
        cost_micros,
    }
}

/// `POST /job/advance`
#[derive(Deserialize)]
struct AdvanceCall {
    run: i64,
    attempt: u32,
    /// Steps taken so far: the cell kept each one's answer.
    count: u32,
    /// The last step, when it ran out of retries: its answer is the error
    /// only the Workflow saw.
    #[serde(default)]
    failed: Option<FailedStep>,
}

#[derive(Deserialize)]
struct FailedStep {
    index: u32,
    kind: String,
    error: String,
}

/// A `steps` row: one step's answer as the cell kept it.
#[derive(Deserialize)]
struct StepRow {
    idx: i64,
    kind: String,
    /// The value's JSON text, or none when the step failed.
    value: Option<String>,
    error: Option<String>,
}

impl StepRow {
    fn size(&self) -> usize {
        self.kind.len() + self.value.as_ref().map_or(0, String::len) + self.error.as_ref().map_or(0, String::len)
    }

    /// Only the cell writes `steps`, and its CHECK keeps exactly one of
    /// value and error: anything else is corruption.
    fn answer(self) -> StepResult {
        let outcome = match (self.value, self.error) {
            (Some(v), None) => StepOutcome::Value(serde_json::from_str(&v).unwrap_or_else(|e| panic!("steps.value is the JSON the cell wrote: {e}"))),
            (None, Some(e)) => StepOutcome::Error(e),
            _ => panic!("a steps row holds a value or an error, not both or neither"),
        };
        StepResult { kind: self.kind, outcome }
    }
}

/// `POST /job/effect`: the step as the Workflow carries it, whose kind and
/// args decode into a `Step` (a step that does not is the job's failure).
#[derive(Deserialize)]
struct EffectCall {
    run: i64,
    attempt: u32,
    index: u32,
    kind: String,
    args: Value,
}

/// `POST /job/finish`
#[derive(Deserialize)]
struct FinishCall {
    run: i64,
    attempt: u32,
    error: String,
}

impl FragmentCell {
    /// The fragment's own key: who triggered runs act as.
    fn own_key(&self) -> CellResult<String> {
        npub::parse(&self.must(MetaKey::Npub)?).ok_or_else(|| CellError::host("the stored npub does not parse"))
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
                let status = p["status"].as_str().and_then(RunStatus::parse).expect("runs.status is a run status");
                return Ok(Started { id: p["id"].as_i64().expect("runs.id is INTEGER"), status, replayed: true });
            }
        }
        let triggered = r.via.triggered();
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
                r.via.as_str().into(),
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
        Ok(self.rows(q, binds)?.first().and_then(|r| r["n"].as_u64()).expect("COUNT(*) answers one integer"))
    }

    /// A Workflow instance id: unique across the fleet (the binding is shared
    /// by every fragment) and across a deleted fragment's reincarnations.
    fn instance_id(&self, run: i64, attempt: u32) -> CellResult<String> {
        let [npub, created_at] = self.metas([MetaKey::Npub, MetaKey::CreatedAt])?;
        let (npub, created_at) = (npub.ok_or_else(|| missing(MetaKey::Npub))?, created_at.ok_or_else(|| missing(MetaKey::CreatedAt))?);
        Ok(format!("{}-{}-r{run}-a{attempt}", &npub[5..25], created_at))
    }

    /// Starts the Workflows of queued runs. A failure leaves them queued for
    /// the alarm to try again.
    pub(crate) async fn launch_queued(&self) {
        let Ok(rows) =
            self.rows(&format!("SELECT {RUN_COLUMNS} FROM runs WHERE status = 'queued' ORDER BY id LIMIT ?"), vec![SqlStorageValue::Integer(LAUNCH_BATCH as i64)])
        else {
            return;
        };
        // most calls queue nothing: they read no more than this
        if rows.is_empty() {
            return;
        }
        let (Ok(name), Ok(incarnation)) = (self.name(), self.must(MetaKey::CreatedAt)) else { return };
        for run in rows.iter().map(run_row) {
            let (id, attempt) = (run.id, run.attempt);
            let Ok(instance) = self.instance_id(id, attempt) else { return };
            let params = json!({ "fragment": name, "incarnation": incarnation, "run": id, "attempt": attempt });
            match js::jobs_create(self.env.as_ref(), &instance, &params).await {
                Ok(()) => {
                    let _ = self.exec(
                        "UPDATE runs SET status = 'running', launched_at = ? WHERE id = ? AND attempt = ? AND status = 'queued'",
                        vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(id), SqlStorageValue::Integer(attempt.into())],
                    );
                    self.event("run.started", &format!("{} run #{id} (attempt {attempt}, via {})", run.op, run.via.as_str()), json!({ "run": id, "attempt": attempt }));
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
    fn current_run(&self, run: i64, attempt: u32) -> CellResult<Option<RunRow>> {
        let rows = self.rows(
            &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id = ? AND attempt = ? AND status IN ('queued', 'running')"),
            vec![SqlStorageValue::Integer(run), SqlStorageValue::Integer(attempt.into())],
        )?;
        let Some(mut current) = rows.first().map(run_row) else { return Ok(None) };
        if current.status == RunStatus::Queued {
            // its Workflow called before the launch that created it returned
            self.exec(
                "UPDATE runs SET status = 'running', launched_at = ? WHERE id = ? AND status = 'queued'",
                vec![SqlStorageValue::Integer(js::now_ms()), SqlStorageValue::Integer(run)],
            )?;
            current.status = RunStatus::Running;
        }
        Ok(Some(current))
    }

    /// Whether a callback is from this fragment's life: a Workflow from a
    /// deleted fragment's earlier life stops.
    fn this_life(&self, incarnation: &str) -> CellResult<bool> {
        Ok(self.meta(MetaKey::CreatedAt)?.as_deref() == Some(incarnation))
    }

    /// Records a run's outcome (once: a second report is ignored).
    fn finish_run(&self, run: &RunRow, outcome: Result<Value, String>) -> CellResult<()> {
        let (id, attempt, op) = (run.id, run.attempt, run.op.as_str());
        let (status, output, error) = match &outcome {
            Ok(v) => (RunStatus::Succeeded, SqlStorageValue::from(v.to_string()), SqlStorageValue::Null),
            Err(e) => (RunStatus::Held, SqlStorageValue::Null, SqlStorageValue::from(clip(e))),
        };
        let now = js::now_ms();
        let changed = self.rows(
            "UPDATE runs SET status = ?, output = ?, error = ?, finished_at = ? WHERE id = ? AND attempt = ? AND status IN ('queued', 'running') RETURNING id",
            vec![status.as_str().into(), output, error, SqlStorageValue::Integer(now), SqlStorageValue::Integer(id), SqlStorageValue::Integer(attempt.into())],
        )?;
        if changed.is_empty() {
            return Ok(());
        }
        // No callback of this attempt is answered from here on, and a
        // replay takes its steps afresh: the kept answers go.
        self.exec("DELETE FROM steps WHERE run = ?", vec![SqlStorageValue::Integer(id)])?;
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

    /// A callback from a run's Workflow (`/job/<route>`); one from an
    /// earlier life of the fragment is told to stop.
    pub(crate) async fn job_callback(&self, route: &str, body: &[u8]) -> CellResult<Value> {
        fn decode<'a, T: Deserialize<'a>>(body: &'a [u8]) -> CellResult<T> {
            serde_json::from_slice(body).map_err(|e| CellError::invalid(format!("a job callback: {e}")))
        }
        /// Every callback names the fragment's life it is from.
        #[derive(Deserialize)]
        struct Life {
            incarnation: String,
        }
        if !self.this_life(&decode::<Life>(body)?.incarnation)? {
            return Ok(json!({ "stop": true }));
        }
        match route {
            "advance" => {
                let answer = self.advance(decode(body)?).await?;
                if answer.get("failed").is_some() {
                    // the held run polls its videos no more: their reservations go back
                    self.release_held_videos().await;
                }
                Ok(answer)
            }
            "effect" => self.job_effect(decode(body)?).await,
            "finish" => {
                let call: FinishCall = decode(body)?;
                if let Some(run) = self.current_run(call.run, call.attempt)? {
                    self.finish_run(&run, Err(call.error))?;
                }
                Ok(json!({ "ok": true }))
            }
            _ => Err(CellError::new(ErrorCode::NotFound, format!("no route /job/{route}"))),
        }
    }

    /// `POST /job/advance`
    async fn advance(&self, call: AdvanceCall) -> CellResult<Value> {
        let (run_id, attempt, count) = (call.run, call.attempt, call.count);
        let Some(run) = self.current_run(run_id, attempt)? else { return Ok(json!({ "stop": true })) };
        let fail = |why: String| -> CellResult<Value> {
            self.finish_run(&run, Err(why.clone()))?;
            Ok(json!({ "failed": why }))
        };
        if count as usize > limits::JOB_STEPS_MAX {
            return Err(CellError::invalid(format!("an advance after {count} steps; a job takes at most {}", limits::JOB_STEPS_MAX)));
        }
        if let Some(f) = call.failed {
            if f.index.checked_add(1) != Some(count) {
                return Err(CellError::invalid(format!("step {} ran out of retries, but the advance is after {count} steps", f.index)));
            }
            self.keep_step(run_id, attempt, f.index, &StepResult { kind: f.kind, outcome: StepOutcome::Error(clip(&f.error)) })?;
        }
        // Test fleets: an advance after the first step waits while the
        // `hold-advances` lever is on, so a test acts on a run it knows is
        // between steps (a latch, not a clock)
        if self.cfg.test_hooks && count > 0 {
            self.held_advance(run_id).await?;
        }
        let results = match self.kept_answers(run_id, attempt, count)? {
            Kept::All(results) => results,
            Kept::Missing(index) => return fail(format!("step {index} has no kept answer: replay it")),
            Kept::TooLarge => return fail(format!("its step results are over {} bytes together", limits::JOB_RESULTS_MAX_BYTES)),
        };
        let op = run.op.clone();
        let input = self.run_input(run_id)?;
        let decl = match self.declared(&op) {
            Ok(d) => d,
            Err(e) => return fail(format!("{op}: {}", e.message)),
        };
        match decl.kind {
            // A triggered mutation is a run of one step: the call.
            OpKind::Mutation => match results.into_iter().next() {
                None => Ok(json!({ "step": NextStep { index: 0, step: Step::Call { op, input } } })),
                Some(result) => {
                    match result.outcome {
                        StepOutcome::Error(e) => fail(e),
                        StepOutcome::Value(v) => {
                            self.finish_run(&run, Ok(v))?;
                            Ok(json!({ "done": true }))
                        }
                    }
                }
            },
            OpKind::Job => {
                let facet = self.facet()?;
                self.sweep(&facet).await?;
                let meta = json!({
                    "principal": npub::display(&run.principal),
                    "role": run.role,
                    "run": run_id,
                    "attempt": attempt,
                    "channels": self.declared_channels()?.keys().collect::<Vec<_>>(),
                });
                let results: Vec<Value> = results.iter().map(|r| serde_json::to_value(r).expect("a step result serializes")).collect();
                let answer = facet.call("__job", &[op.as_str().into(), input, meta, Value::Array(results)]).await.map_err(|e| match e.code {
                    ErrorCode::NodeFull => e,
                    _ => CellError::host(format!("the app facet: {}", e.message)),
                })?;
                if let Some(next) = answer.get("next") {
                    let index = next["index"].as_u64().unwrap_or(u64::MAX);
                    if index >= limits::JOB_STEPS_MAX as u64 {
                        return fail(format!("a job takes at most {} steps", limits::JOB_STEPS_MAX));
                    }
                    // the platform code asks for the first step it has no answer for
                    if index != u64::from(count) {
                        return fail(format!("the job asked for step {index} after {count} steps: the app broke its platform code"));
                    }
                    // A sleep is the Workflow's own step: its answer (nothing)
                    // is kept as it is handed out, and read once it is over.
                    if next["kind"] == "sleep" {
                        self.keep_step(run_id, attempt, count, &StepResult { kind: "sleep".into(), outcome: StepOutcome::Value(Value::Null) })?;
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

    /// Test fleets: waits while `hold-advances` is on, at most
    /// `TEST_HOLD_MAX_MS`, naming the held run (`advance-held`).
    async fn held_advance(&self, run: i64) -> CellResult<()> {
        assert!(self.cfg.test_hooks, "only a test fleet holds an advance");
        for _ in 0..TEST_HOLD_MAX_MS / TEST_HOLD_POLL_MS {
            if self.meta(MetaKey::TestHoldAdvances)?.is_none() {
                return Ok(());
            }
            self.set_meta(MetaKey::TestAdvanceHeld, &run.to_string())?;
            Delay::from(Duration::from_millis(TEST_HOLD_POLL_MS)).await;
        }
        Ok(())
    }

    /// The kept answers of a run's first `count` steps, in order, or why the
    /// run cannot go on: the first step without one, or answers that are
    /// over their limit together.
    fn kept_answers(&self, run: i64, attempt: u32, count: u32) -> CellResult<Kept> {
        let rows: Vec<StepRow> = self.typed(
            "SELECT idx, kind, value, error FROM steps WHERE run = ? AND attempt = ? AND idx < ? ORDER BY idx",
            vec![SqlStorageValue::Integer(run), SqlStorageValue::Integer(attempt.into()), SqlStorageValue::Integer(count.into())],
        )?;
        let mut answers = Vec::with_capacity(rows.len());
        let mut bytes = 0;
        for row in rows {
            if row.idx != answers.len() as i64 {
                break;
            }
            bytes += row.size();
            if bytes > limits::JOB_RESULTS_MAX_BYTES {
                return Ok(Kept::TooLarge);
            }
            answers.push(row.answer());
        }
        if answers.len() != count as usize {
            return Ok(Kept::Missing(answers.len()));
        }
        Ok(Kept::All(answers))
    }

    /// Keeps a step's answer, once: a second answer for the same step (a
    /// retry that raced the first) leaves the first.
    fn keep_step(&self, run: i64, attempt: u32, index: u32, answer: &StepResult) -> CellResult<()> {
        let (value, error) = match &answer.outcome {
            StepOutcome::Value(v) => (SqlStorageValue::from(v.to_string()), SqlStorageValue::Null),
            StepOutcome::Error(e) => (SqlStorageValue::Null, SqlStorageValue::from(e.as_str())),
        };
        self.exec(
            "INSERT INTO steps (run, attempt, idx, kind, value, error) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
            vec![
                SqlStorageValue::Integer(run),
                SqlStorageValue::Integer(attempt.into()),
                SqlStorageValue::Integer(index.into()),
                answer.kind.as_str().into(),
                value,
                error,
            ],
        )
    }

    /// The kind of a step's kept answer, when it has one.
    fn kept_kind(&self, run: i64, attempt: u32, index: u32) -> CellResult<Option<String>> {
        let rows = self.rows(
            "SELECT kind FROM steps WHERE run = ? AND attempt = ? AND idx = ?",
            vec![SqlStorageValue::Integer(run), SqlStorageValue::Integer(attempt.into()), SqlStorageValue::Integer(index.into())],
        )?;
        Ok(rows.first().map(|r| r["kind"].as_str().expect("steps.kind is TEXT NOT NULL").to_string()))
    }

    /// A run's input, as the call or trigger that started it gave it.
    fn run_input(&self, run: i64) -> CellResult<Value> {
        let rows = self.rows("SELECT input FROM runs WHERE id = ?", vec![SqlStorageValue::Integer(run)])?;
        let row = rows.first().ok_or_else(|| CellError::host(format!("run #{run} went while it ran")))?;
        Ok(stored_json(row, "input"))
    }

    /// `POST /job/effect`: one step, performed and its answer kept (a
    /// value, or a lasting failure the job sees) before the reply; a
    /// passing failure is a 502, which the Workflow retries. A step that
    /// already has a kept answer (its reply was lost) is not performed again.
    async fn job_effect(&self, call: EffectCall) -> CellResult<Value> {
        let Some(run) = self.current_run(call.run, call.attempt)? else { return Ok(json!({ "stop": true })) };
        let index = call.index;
        if index as usize >= limits::JOB_STEPS_MAX {
            return Err(CellError::invalid(format!("step {index}: a job takes at most {} steps", limits::JOB_STEPS_MAX)));
        }
        let kept = match self.kept_kind(run.id, run.attempt, index)? {
            Some(kept) => kept,
            None => {
                let out = match Step::from_parts(&call.kind, call.args) {
                    Ok(step) => self.perform(&run, index, step).await,
                    // the args come from the app's realm: the job sees why, and may catch it
                    Err(why) => Err(permanent(format!("step {index} ({}): {why}", call.kind))),
                };
                let outcome = match out {
                    Ok(v) => StepOutcome::Value(v),
                    Err(StepFail::Permanent(m)) => StepOutcome::Error(clip(&m)),
                    Err(StepFail::Retry(m)) => return Err(CellError::new(ErrorCode::UpstreamFailed, m)),
                };
                let mut answer = StepResult { kind: call.kind.clone(), outcome };
                // the job's body reads every answer back at each later step
                let size = serde_json::to_string(&answer).expect("a step result serializes").len();
                if size > limits::RESULT_MAX_BYTES {
                    answer.outcome = StepOutcome::Error(format!("the step's result is over {} bytes", limits::RESULT_MAX_BYTES));
                }
                self.keep_step(run.id, run.attempt, index, &answer)?;
                let kept = self.kept_kind(run.id, run.attempt, index)?.ok_or_else(|| CellError::host(format!("step {index}'s answer was not kept")))?;
                // lost after the step ran and its answer was kept: the Workflow tries it again
                self.test_countdown(MetaKey::TestDropEffects, "the step's answer was lost on its way back")?;
                kept
            }
        };
        // one step, one kind: a retry names the kind its first try did
        if kept != call.kind {
            return Err(CellError::host(format!("step {index} of run #{} was kept as {kept}, and is {} now", run.id, call.kind)));
        }
        self.launch_queued().await;
        Ok(json!({ "kept": index }))
    }

    /// Performs one step of `run`.
    async fn perform(&self, run: &RunRow, index: u32, step: Step) -> Result<Value, StepFail> {
        match step {
            Step::Call { op, input } => self.step_call(run, index, &op, input).await,
            Step::Fetch(f) => self.step_fetch(run, f).await,
            Step::Publish { channel, kind, body } => self.step_publish(run, index, &channel, &kind, body).await,
            Step::Push { who, payload } => {
                let key = format!("{JOB_ID_PREFIX}{}:{index}", run.id);
                match self.send_push(&key, &who, &payload).await {
                    Ok(n) => Ok(json!({ "queued": n })),
                    Err(e) if e.code == ErrorCode::HostFailed => Err(StepFail::Retry(e.message)),
                    Err(e) => Err(permanent(e.message)),
                }
            }
            Step::Sleep { .. } => Err(permanent("a sleep is the Workflow's own step; the platform performs none")),
            Step::FilesRead { path } => self.read_main(&path).await.map_err(settle_files).map(|f| f.map_or(Value::Null, crate::files::content_json)),
            Step::FilesList { prefix } => self.list_main(&prefix).map(Value::Array).map_err(settle_files),
            Step::FilesStat { path } => Ok(self.stat_main(&path).await.map_err(settle_files)?.unwrap_or(Value::Null)),
            Step::FilesWrite(w) => {
                let bytes = w.content.into_bytes().map_err(permanent)?;
                if fragment_core::blob::parse(&bytes).is_some() {
                    return Err(permanent(format!("{}: an app does not write blob pointers", w.path)));
                }
                self.step_write(run, index, &w.path, Some(bytes), w.expect).await
            }
            Step::FilesRemove { path, expect } => self.step_write(run, index, &path, None, expect).await,
            ai @ (Step::AiText(_) | Step::AiImage(_) | Step::AiVideoStart(_) | Step::AiVideoPoll { .. } | Step::AiVideoSave { .. }) => {
                self.step_ai(run, index, &ai).await
            }
        }
    }

    /// `job.call(op, input)`: as the run's principal, with the step as its
    /// operation id, so a retried or replayed step is a replay.
    async fn step_call(&self, run: &RunRow, index: u32, op: &str, input: Value) -> Result<Value, StepFail> {
        let decl = self.declared(op).map_err(|e| permanent(e.message))?;
        if run.role < decl.role {
            return Err(permanent(format!("{op} needs the {} role; this run acts as {}", decl.role.as_str(), run.role.as_str())));
        }
        let child = decl.kind == OpKind::Job;
        let inv = Invocation {
            principal: &run.principal,
            role: run.role,
            op,
            decl,
            id: format!("{JOB_ID_PREFIX}{}:{index}", run.id),
            input,
            depth: if child { run.depth + 1 } else { run.depth },
            via: Via::Job,
            trigger: Some(format!("run {}", run.id)),
        };
        match self.invoke(inv).await {
            // a step's value is a tree (its answers are stored as JSON with the run's)
            Ok(r) => serde_json::from_str(r.result.get()).map_err(|e| permanent(format!("{op}: its result does not read as JSON: {e}"))),
            Err(e) => match e.code {
                ErrorCode::HostFailed | ErrorCode::UpstreamFailed => Err(StepFail::Retry(e.message)),
                _ => Err(permanent(format!("{op}: {}", e.message))),
            },
        }
    }

    /// `job.fetch(url, init)`: the fragment's one way out. Header values
    /// may name secrets as `{{NAME}}`; they are opened here, at the egress
    /// point, and never reach the app.
    async fn step_fetch(&self, run: &RunRow, f: Fetch) -> Result<Value, StepFail> {
        let url = egress::check(&f.url, self.cfg.egress_local).map_err(permanent)?;
        let method = match f.method.to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "PATCH" => Method::Patch,
            "DELETE" => Method::Delete,
            "HEAD" => Method::Head,
            other => return Err(permanent(format!("method {other} is not supported"))),
        };
        if f.headers.len() > FETCH_HEADERS_MAX {
            return Err(permanent(format!("at most {FETCH_HEADERS_MAX} headers")));
        }
        let headers = Headers::new();
        for (k, given) in &f.headers {
            let mut value = given.clone();
            for name in placeholders(given) {
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
        headers.set(HOPS_HEADER, &(run.depth + 1).to_string()).map_err(|e| permanent(e.to_string()))?;
        let mut init = RequestInit::new();
        // A redirect comes back to the job as its 3xx: following it here
        // would skip the egress check for where it points.
        init.with_method(method).with_headers(headers).with_redirect(RequestRedirect::Manual);
        if let Some(body) = f.body {
            if body.len() > limits::FETCH_BODY_MAX_BYTES {
                return Err(permanent(format!("a fetch body is at most {} bytes", limits::FETCH_BODY_MAX_BYTES)));
            }
            init.with_body(Some(JsValue::from_str(&body)));
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
    async fn step_publish(&self, run: &RunRow, index: u32, channel: &str, kind: &str, body: Value) -> Result<Value, StepFail> {
        let retry = |e: CellError| StepFail::Retry(e.message);
        // the one check a mutation's records meet too
        fragment_core::effects::check_record(channel, kind, &body, &self.declared_channels().map_err(retry)?).map_err(permanent)?;
        let key = format!("{JOB_ID_PREFIX}{}", run.id);
        let (record, appended) = self.append_once(channel, &run.principal, kind, &body, &key, i64::from(index)).map_err(retry)?;
        self.published(&record, appended, run.depth + 1).await.map_err(retry)?;
        Ok(json!({ "seq": record.seq }))
    }

    /// `job.files.write` and `remove` (`bytes: None`): one commit per step,
    /// compare-and-swapped when the step names what it expects.
    async fn step_write(&self, run: &RunRow, index: u32, path: &str, bytes: Option<Vec<u8>>, expect: Option<Option<String>>) -> Result<Value, StepFail> {
        let mut expected = BTreeMap::new();
        if let Some(e) = expect {
            expected.insert(path.to_string(), e);
        }
        let key = format!("{JOB_ID_PREFIX}{}:{index}", run.id);
        let message = format!("{} run {}: {} {path}", run.op, run.id, if bytes.is_some() { "write" } else { "remove" });
        let writes = [crate::files::FileWrite { path: path.to_string(), bytes }];
        match self.commit_files(&key, &writes, &expected, &message, &run.principal, run.depth).await.map_err(settle_files)? {
            crate::files::Wrote::Commit(sha) => Ok(json!({ "commit": sha })),
            crate::files::Wrote::Conflict(why) => Err(permanent(format!("conflict: {why}"))),
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
                // after the record, its deliveries, and the runs before this one are written
                self.test_countdown(MetaKey::TestFailTriggers, "the trigger step failed before its last run started")?;
            }
            let call_id = format!("record:{}:{}:{op}", record.channel, record.seq);
            let sha = crate::ops::input_sha(op, &fragment_proto::canonical_json(&input));
            let s = self.start_run(NewRun {
                op,
                via: Via::Channel,
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
                via: Via::Files,
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
            let (op, expr) = (row["op"].as_str().expect("schedules.op is TEXT NOT NULL"), row["cron"].as_str().expect("schedules.cron is TEXT NOT NULL"));
            let (idx, at) = (row["idx"].as_i64().expect("schedules.idx is INTEGER"), row["next_at"].as_i64().expect("schedules.next_at is INTEGER NOT NULL"));
            let busy = self.count_of(
                "SELECT COUNT(*) AS n FROM runs WHERE op = ? AND via = 'cron' AND status IN ('queued', 'running')",
                vec![op.into()],
            )? > 0;
            if busy {
                self.event("cron.skipped", &format!("{op} ({expr}): the previous run is still going"), json!({ "op": op }));
            } else if !self.is_paused(op)? {
                self.start_run(NewRun {
                    op,
                    via: Via::Cron,
                    trigger: Some(expr.to_string()),
                    principal: &self.own_key()?,
                    role: Role::Editor,
                    depth: 0,
                    call: None,
                    input: json!({ "cron": expr, "at": at }),
                })?;
            }
            match Cron::parse(expr).ok().and_then(|c| c.next_after(now.max(at))) {
                Some(next) => self.exec("UPDATE schedules SET next_at = ? WHERE idx = ?", vec![SqlStorageValue::Integer(next), SqlStorageValue::Integer(idx)])?,
                None => self.exec("DELETE FROM schedules WHERE idx = ?", vec![SqlStorageValue::Integer(idx)])?,
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
            &format!("SELECT {RUN_COLUMNS} FROM runs WHERE status = 'running' AND launched_at < ? ORDER BY launched_at LIMIT ?"),
            vec![SqlStorageValue::Integer(js::now_ms() - RECONCILE_AFTER_MS), SqlStorageValue::Integer(RECONCILE_BATCH)],
        ) else {
            return;
        };
        for run in rows.iter().map(run_row) {
            let Ok(instance) = self.instance_id(run.id, run.attempt) else { return };
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
        let mut q = format!("SELECT {RUN_COLUMNS}, {RUN_COST} FROM runs WHERE 1 = 1");
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
        let runs: Vec<Run> = self.rows(&q, binds)?.iter().map(|r| run_view(run_row(r), r["cost_micros"].as_i64(), None)).collect();
        let mut counts = BTreeMap::new();
        for r in self.rows("SELECT status, COUNT(*) AS n FROM runs GROUP BY status", vec![])? {
            let status = r["status"].as_str().expect("runs.status is TEXT NOT NULL").to_string();
            counts.insert(status, r["n"].as_u64().expect("COUNT answers a count"));
        }
        json_response(&RunList { runs, counts, paused: self.paused_ops()? })
    }

    /// `GET /api/f/<name>/runs/<id>`
    pub(crate) fn run_api(&self, caller: &Caller, id: &str) -> CellResult<Response> {
        self.require(caller, false, Role::Viewer)?;
        let id: i64 = id.parse().map_err(|_| CellError::invalid("a run id is a number"))?;
        let rows = self.rows(&format!("SELECT {RUN_COLUMNS}, {RUN_COST}, input, output FROM runs WHERE id = ?"), vec![SqlStorageValue::Integer(id)])?;
        let r = rows.first().ok_or_else(|| CellError::new(ErrorCode::NotFound, format!("no run #{id}")))?;
        // a run has its output once it succeeded
        let output = r["output"].is_string().then(|| stored_json(r, "output"));
        json_response(&run_view(run_row(r), r["cost_micros"].as_i64(), Some((stored_json(r, "input"), output))))
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
        let op = row["op"].as_str().expect("runs.op is TEXT NOT NULL");
        self.event("run.replayed", &format!("{op} run #{} by {by}", body.run), json!({ "run": body.run }));
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
        if !crate::serve::eq_ct(token, &self.must(MetaKey::InboxToken)?) {
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
        let opened = keys::open(&self.env, &sealed, &self.must(MetaKey::Npub)?).await.map_err(|e| CellError::host(format!("secret {name}: {}", e.message)))?;
        if let Some(fresh) = opened.resealed {
            self.exec("UPDATE secrets SET sealed = ? WHERE name = ? AND sealed = ?", vec![fresh.into(), name.into(), sealed.into()])?;
            self.event("secret.resealed", &format!("secret {name} resealed under the current host secret"), json!({ "name": name }));
        }
        Ok(Some(opened.plaintext))
    }
}
