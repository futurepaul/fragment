// Runs: the failure-leg state machine. Every workflow execution is a row in
// `runs`, and every transition — guards, attempts, backoff, held, auto-pause,
// crash sweep — happens in this module, as straight-line code.
//
//   trigger ─▶ [guards] ─▶ running ─(native launch)─▶ success
//                │            └▶ backoff ─(alarm)─▶ running, attempt+1
//                │                 └▶ held (exhausted | terminal)
//                └▶ blocked | skipped
//
// EXECUTION runs on native Workflows (wf-engine.ts): each attempt is a
// Workflow instance whose step.do drives the author body inside the cell
// and reports the outcome back to /__internal/wf/complete, which applies
// finishAttempt. Backoff WAITS stay on the cell's single alarm (the runs
// ledger owns WHEN); the native engine owns HOW an attempt executes and
// survives restarts. A `running` row with a dead instance is detected by
// the sweep below querying instance status — crash detection stays a
// SELECT plus one status read, never a heartbeat.
import { launchNativeRun, nativeStatus, wfInstanceId } from "./wf-engine.js";

export const HOP_LIMIT = 16;
export const BREAKER_N = 5;
export const BREAKER_WINDOW = 10 * 60_000;
export const LEASE_MS = 10 * 60_000;

// Default retryable classes: the transient failure shapes of network egress
// and upstream providers (undici's "fetch failed", the isolate's Rust-backed
// "error sending request"). Everything else (code errors, 4xx, bad parses)
// is terminal — retrying a poison input politely is still retrying a wall.
// 500 is included for cross-fragment delivery: the host can fail the
// response edge after the target's writes already committed, and a re-POST
// of the same payload is idempotent through the target's content-addressed
// ingest. CAS conflicts ("branch moved", precondition_failed) are retryable:
// the commit funnel already healed-or-retried internally, so what surfaces
// here lost a genuine race and a later attempt refetches state anyway.
const RETRYABLE = /timeout|timed out|abort|network|fetch failed|error sending request|econn|socket|connection|overloaded|rate limit|too many requests|\b429\b|\b500\b|\b502\b|\b503\b|\b504\b|branch moved|precondition_failed/i;

export function retryableError(err) {
  return RETRYABLE.test(String(err || ""));
}

export function retryPolicy(wf) {
  const r = wf.retry === true ? {} : (wf.retry || {});
  return {
    attempts: r.attempts ?? 3,
    backoffMs: r.backoffMs ?? 30_000,
    maxBackoffMs: r.maxBackoffMs ?? 300_000,
  };
}

function backoffDelay(policy, attempt) {
  const base = Math.min(policy.backoffMs * Math.pow(2, attempt - 1), policy.maxBackoffMs);
  const jitter = base * (0.8 + Math.random() * 0.4); // ±20%
  return Math.round(Math.min(jitter, policy.maxBackoffMs));
}

function insertRun(cell, wf, via, input, cause, status, extra: any = {}) {
  // last_insert_rowid, not RETURNING: celld's sql wrapper doesn't reliably
  // materialize RETURNING rows for this statement shape (the insert itself
  // lands), and the DO is single-writer so the follow-up read is race-free
  cell.sql.exec(
    "INSERT INTO runs (wf, via, status, input, cause, attempt, max_attempts, started_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    wf.name, via, status, JSON.stringify(input ?? null), JSON.stringify(cause ?? null), extra.attempt ?? 1, extra.maxAttempts ?? 1, Date.now(),
  );
  return cell.sql.exec("SELECT last_insert_rowid() AS id").toArray()[0].id;
}

function updateRun(cell, id, fields) {
  const sets = [], vals = [];
  for (const [k, v] of Object.entries(fields)) { sets.push(`${k} = ?`); vals.push(v); }
  vals.push(id);
  cell.sql.exec(`UPDATE runs SET ${sets.join(", ")} WHERE id = ?`, ...vals);
}

function setPaused(cell, wf, paused, by, why) {
  const m = cell.manifest();
  const target = (m.workflows || []).find((w) => w.name === wf.name);
  if (!target) return;
  target.paused = paused ? true : undefined;
  cell.setMeta("manifest", JSON.stringify(m));
  cell.sql.exec("DELETE FROM meta WHERE k = ?", `wf_breaker_${wf.name}`);
  cell.addEvent(paused ? "workflow.paused" : "workflow.unpaused", `${wf.name}${why ? ": " + why : ""}`, { wf: wf.name, by });
}

// auto-pause: 5 held runs in 10 minutes means the workflow is sick, not
// unlucky — the human is the one escalation level.
function breakerBump(cell, wf, lastError) {
  const k = `wf_breaker_${wf.name}`;
  const now = Date.now();
  let b;
  try { b = JSON.parse(cell.getMeta(k) || "{}"); } catch { b = {}; }
  if (!b.first || now - b.first > BREAKER_WINDOW) { b = { first: now, n: 0 }; }
  b.n++;
  if (b.n >= BREAKER_N) {
    setPaused(cell, wf, true, "auto", `circuit breaker: ${BREAKER_N} held runs in ${BREAKER_WINDOW / 60000}min — ${String(lastError || "").slice(0, 200)}`);
    cell.addEvent("workflow.auto-paused", `${wf.name} auto-paused by circuit breaker`, { wf: wf.name, held: b.n, lastError: String(lastError || "").slice(0, 500) });
  } else {
    cell.setMeta(k, JSON.stringify(b));
  }
}

function recentAutoRuns(cell, wf) {
  return cell.sql.exec(
    "SELECT COUNT(*) c FROM runs WHERE wf = ? AND status NOT IN ('skipped', 'blocked') AND started_at > ?",
    wf.name, Date.now() - 3600_000,
  ).toArray()[0].c;
}

// ------ executeWorkflow: the one entry point for every trigger ------

// guards, in fixed order — each records what it refused and why. Shared
// by direct execution and scheduled (alarm-fired) runs so async inbox
// delivery cannot bypass pause/hops/rate/single-flight.
async function runGuards(cell, wf, input, trigger, auto, cause) {
  if (auto && wf.paused) {
    const id = insertRun(cell, wf, trigger, input, cause, "blocked");
    cell.addEvent("run.blocked", `${wf.name}: paused`, { wf: wf.name, trigger, runId: id });
    return { ok: true, blocked: true, runId: id };
  }
  if (cause.depth > HOP_LIMIT && !wf.cycles) {
    const id = insertRun(cell, wf, trigger, input, cause, "blocked");
    cell.addEvent("cycle.detected", `${wf.name}: ${cause.depth} hops from ${cause.origin || "unknown"}`, { wf: wf.name, runId: id, hops: cause.depth, origin: cause.origin });
    return { ok: false, blocked: true, reason: "cycle", runId: id };
  }
  if (auto && recentAutoRuns(cell, wf) >= (wf.maxRunsPerHour ?? 120)) {
    const id = insertRun(cell, wf, trigger, input, cause, "blocked");
    setPaused(cell, wf, true, "auto", `rate ceiling: ${recentAutoRuns(cell, wf)} auto runs in the last hour`);
    cell.addEvent("workflow.auto-paused", `${wf.name} auto-paused at rate ceiling`, { wf: wf.name, runId: id });
    return { ok: false, blocked: true, reason: "rate", runId: id };
  }
  if (auto) {
    // single-flight: one in-flight run per workflow; level-triggered
    // sources (sync/cron) also wait out a pending retry, since the retry
    // re-reads current state anyway. Inbox messages are distinct work and
    // only respect the in-flight check.
    const active = cell.sql.exec(
      "SELECT id, status FROM runs WHERE wf = ? AND (status = 'running' OR (status = 'backoff' AND ? != 'inbox')) AND started_at > ? ORDER BY id DESC LIMIT 1",
      wf.name, trigger, Date.now() - LEASE_MS,
    ).toArray()[0];
    if (active) {
      const id = insertRun(cell, wf, trigger, input, cause, "skipped");
      cell.addEvent("run.skipped", `${wf.name}: ${active.status === "running" ? "previous run still active" : "retry pending"}`, { wf: wf.name, trigger, runId: id });
      return { ok: true, skipped: true, runId: id };
    }
  }
  return null;
}

export async function executeWorkflow(cell, wf, input, opts: any = {}) {
  // scheduled mode: record the run as pending and arm the alarm — the
  // caller (an inbox POST acknowledging a webhook) returns immediately;
  // the alarm fires the actual execution. Manual runs and retries are
  // unaffected (they pass no `schedule`).
  if (opts.schedule) {
    const cause = { origin: null, depth: 0, ...(opts.cause || {}) };
    const policy = retryPolicy(wf);
    cell.sql.exec(
      "INSERT INTO runs (wf, via, status, input, cause, attempt, max_attempts, started_at, next_attempt_at) VALUES (?, ?, 'pending', ?, ?, 0, ?, ?, ?)",
      wf.name, opts.trigger || "inbox", JSON.stringify(input ?? null), JSON.stringify(cause), policy.attempts, Date.now(), Date.now(),
    );
    await cell.rearmAlarm();
    return { ok: true, scheduled: true };
  }
  const trigger = opts.trigger || "manual";
  const auto = !!opts.auto;
  const cause = { origin: null, depth: 0, ...(opts.cause || {}) };

  const blocked = await runGuards(cell, wf, input, trigger, auto, cause);
  if (blocked) return blocked;

  const policy = retryPolicy(wf);
  const t0 = Date.now();
  const runId = insertRun(cell, wf, trigger, input, cause, "running", { attempt: 1, maxAttempts: policy.attempts });
  cell.sql.exec("UPDATE runs SET started_at = ? WHERE id = ?", t0, runId);
  cell.addEvent("run.started", `${wf.name} (attempt 1)`, { wf: wf.name, trigger, runId, attempt: 1 });
  try {
    const instanceId = await launchNativeRun(cell, runId, 1);
    return { ok: true, launched: true, runId, instanceId };
  } catch (e) {
    // launch failures are the host's problem, not the workflow's: park as
    // held with the launch error — a missing binding or a wedged engine
    // must be loud, not retried into oblivion
    const out = { ok: false, error: `native launch failed: ${String((e as Error).message || e)}` };
    return finishAttempt(cell, wf, runId, 1, policy, trigger, t0, out);
  }
}

// apply an attempt's outcome: success, schedule the retry, or park as held.
// Called by /__internal/wf/complete (the workflow's report step) and by
// the crash sweep — its only two callers.
export async function finishAttempt(cell, wf, runId, attempt, policy, trigger, t0, out) {
  // idempotence guard: a late duplicate report (workflow retried its
  // report step after the sweep already reconciled) must not double-apply
  const row = cell.sql.exec("SELECT status FROM runs WHERE id = ?", runId).toArray()[0];
  if (!row) return { ok: false, error: "no such run", runId };
  if (row.status !== "running") {
    cell.addEvent("run.report-ignored", `run ${runId} already ${row.status} — duplicate outcome report dropped`);
    return { ok: row.status === "success", output: null, runId, already: row.status };
  }
  if (out.ok) {
    updateRun(cell, runId, { status: "success", finished_at: Date.now(), duration_ms: Date.now() - t0, error: null });
    cell.sql.exec("DELETE FROM meta WHERE k = ?", `wf_breaker_${wf.name}`);
    cell.addEvent("run.succeeded", `${wf.name}`, { wf: wf.name, runId: runId, output: out.output !== null && out.output !== undefined ? out.output : undefined });
    return { ok: true, output: out.output ?? null, runId };
  }
  const canRetry = (out.forceRetry || retryableError(out.error)) && attempt < policy.attempts;
  if (canRetry) {
    const inMs = backoffDelay(policy, attempt);
    updateRun(cell, runId, { status: "backoff", next_attempt_at: Date.now() + inMs, error: String(out.error || "").slice(0, 2000) });
    cell.addEvent("run.retry", `${wf.name}: attempt ${attempt} failed, retrying in ${Math.round(inMs / 1000)}s`, { wf: wf.name, runId, attempt, inMs, error: String(out.error || "").slice(0, 500) });
    await cell.rearmAlarm();
    return { ok: false, retrying: true, runId, error: out.error };
  }
  updateRun(cell, runId, { status: "held", finished_at: Date.now(), error: String(out.error || "").slice(0, 2000) });
  cell.addEvent("run.held", `${wf.name}: ${String(out.error || "").slice(0, 200)}`, { wf: wf.name, runId, attempt, error: String(out.error || "").slice(0, 500) });
  breakerBump(cell, wf, out.error);
  return { ok: false, held: true, runId, error: out.error };
}

// ------ the alarm's half: crash sweep + due retries ------

export async function resumeDueRuns(cell) {
  const m = cell.manifest();
  if (!m) return;

  // crashed runs: a `running` row whose native instance is terminal (and
  // whose report never landed) or unresolvable. The grace is generous
  // because a false sweep duplicates a live run; the authoritative check
  // is the instance's own status, which is durable state, not a guess.
  const crashed = cell.sql.exec("SELECT * FROM runs WHERE status = 'running' AND started_at < ?", Date.now() - 60_000).toArray();
  for (const r of crashed) {
    const wf = (m.workflows || []).find((w) => w.name === r.wf);
    if (!wf) { updateRun(cell, r.id, { status: "held", finished_at: Date.now(), error: "workflow removed while run in flight" }); continue; }
    const st = await nativeStatus(cell, wfInstanceId(cell.getMeta("fragment_npub"), r.id, r.attempt));
    if (st === null) continue; // engine unreachable: try again next alarm; never guess
    if (st.status === "running" || st.status === "queued" || st.status === "paused" || st.status === "waiting") continue;
    const policy = retryPolicy(wf);
    if (st.status === "complete") {
      // finished but the report step never landed: reconcile from the
      // instance's own return value (the outcome object)
      const out = (st.output && typeof st.output === "object" && "ok" in (st.output as object))
        ? st.output : { ok: false, error: "run finished without a decodable outcome" };
      await finishAttempt(cell, wf, r.id, r.attempt, policy, r.via, r.started_at, out as any);
      continue;
    }
    // errored/terminated: crashed is retryable by definition — a host
    // restart says nothing about the input
    await finishAttempt(cell, wf, r.id, r.attempt, policy, r.via, r.started_at, { ok: false, error: "run interrupted (host restart)", forceRetry: true });
  }

  const due = cell.sql.exec("SELECT * FROM runs WHERE (status = 'backoff' OR status = 'pending') AND next_attempt_at <= ? ORDER BY id", Date.now()).toArray();
  for (const r of due) {
    const wf = (m.workflows || []).find((w) => w.name === r.wf);
    if (!wf) { updateRun(cell, r.id, { status: "held", finished_at: Date.now(), error: "workflow removed while retry pending" }); continue; }
    if (wf.paused && r.status !== "pending") continue; // breaker won mid-retry: leave backoff rows for unpause+replay; pending rows fall through to the guard so the block is RECORDED
    let cause: any = { origin: null, depth: 0 };
    try { cause = { ...cause, ...JSON.parse(r.cause || "{}") }; } catch {}
    // inbox runs batch: ctx.inbox() claims ALL pending messages, so an
    // earlier run may have drained this row's message already. The
    // message is processed — resolve the row as success-by-elsewhere
    // instead of racing a duplicate execution into single-flight skips
    // (three quick posts used to yield one success and two skips).
    if (r.via === "inbox") {
      try {
        const inp = JSON.parse(r.input || "null");
        const mid = Number(inp?.inbox?.id) || 0;
        if (mid) {
          const st = cell.sql.exec("SELECT status FROM inbox WHERE id = ?", mid).toArray()[0];
          if (st && st.status === "done") {
            updateRun(cell, r.id, { status: "success", finished_at: Date.now(), duration_ms: 0, error: null });
            cell.addEvent("run.deduped", `${r.wf}: inbox #${mid} was drained by an earlier run`, { wf: r.wf, run: r.id, inboxId: mid });
            continue;
          }
        }
      } catch {}
    }
    if (r.status === "pending") {
      // first execution of a scheduled run: the same guards direct runs
      // pass (pause/hops/rate/single-flight) — async delivery must not
      // be a guard bypass. A single-flight skip RESCHEDULES rather than
      // drops: the messages this run would drain have no other trigger
      // coming for them.
      const blocked = await runGuards(cell, wf, JSON.parse(r.input || "null"), r.via, true, cause);
      if (blocked) {
        if (blocked.skipped) {
          updateRun(cell, r.id, { status: "pending", next_attempt_at: Date.now() + 2000 });
          continue;
        }
        updateRun(cell, r.id, { status: "blocked", finished_at: Date.now(), error: blocked.reason || "paused" });
        continue;
      }
    }
    const policy = retryPolicy(wf);
    const attempt = r.attempt + 1;
    const t0 = Date.now();
    updateRun(cell, r.id, { status: "running", attempt, started_at: t0, next_attempt_at: null });
    cell.addEvent("run.started", `${wf.name} (attempt ${attempt})`, { wf: wf.name, trigger: r.via, runId: r.id, attempt });
    try {
      await launchNativeRun(cell, r.id, attempt);
    } catch (e) {
      await finishAttempt(cell, wf, r.id, attempt, policy, r.via, t0, { ok: false, error: `native launch failed: ${String((e as Error).message || e)}` });
    }
  }
}
