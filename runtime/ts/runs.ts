// Runs: the failure-leg state machine. Every workflow execution is a row in
// `runs`, and every transition — guards, attempts, backoff, held, auto-pause,
// crash sweep — happens in this module, as straight-line code. Backoff waits
// are scheduled on the cell's alarm, never a blocking sleep.
//
//   trigger ─▶ [guards] ─▶ running ─▶ success
//                │            └▶ backoff ─(alarm)─▶ running, attempt+1
//                │                 └▶ held (exhausted | terminal)
//                └▶ blocked | skipped
//
// Because a Durable Object is single-threaded, a `running` row observed by
// the alarm handler is by definition a crashed run — crash detection is a
// SELECT, not a heartbeat.

export const HOP_LIMIT = 16;
export const BREAKER_N = 5;
export const BREAKER_WINDOW = 10 * 60_000;
export const LEASE_MS = 10 * 60_000;

// Default retryable classes: the transient failure shapes of network egress
// and upstream providers (undici's "fetch failed", the isolate's Rust-backed
// "error sending request"). Everything else (code errors, 4xx, bad parses)
// is terminal — retrying a poison input politely is still retrying a wall.
const RETRYABLE = /timeout|timed out|abort|network|fetch failed|error sending request|econn|socket|connection|overloaded|rate limit|too many requests|\b429\b|\b502\b|\b503\b|\b504\b/i;

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

export async function executeWorkflow(cell, wf, input, opts: any = {}) {
  const trigger = opts.trigger || "manual";
  const auto = !!opts.auto;
  const cause = { origin: null, depth: 0, ...(opts.cause || {}) };

  // guards, in fixed order — each records what it refused and why
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

  const policy = retryPolicy(wf);
  const t0 = Date.now();
  const runId = insertRun(cell, wf, trigger, input, cause, "running", { attempt: 1, maxAttempts: policy.attempts });
  cell.sql.exec("UPDATE runs SET started_at = ? WHERE id = ?", t0, runId);
  return finishAttempt(cell, wf, runId, 1, policy, trigger, t0,
    await cell.runWorkflowLocked(wf, input, cause));
}

// apply an attempt's outcome: success, schedule the retry, or park as held.
// Called by executeWorkflow and by resumeDueRuns (its only two callers).
async function finishAttempt(cell, wf, runId, attempt, policy, trigger, t0, out) {
  if (out.ok) {
    updateRun(cell, runId, { status: "success", finished_at: Date.now(), duration_ms: Date.now() - t0, error: null });
    cell.sql.exec("DELETE FROM meta WHERE k = ?", `wf_breaker_${wf.name}`);
    cell.addEvent("run.succeeded", `${wf.name}`, { wf: wf.name, runId: runId, output: out.output !== null && out.output !== undefined ? out.output : undefined });
    return { ok: true, output: out.output ?? null, runId };
  }
  const canRetry = retryableError(out.error) && attempt < policy.attempts;
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

  // crashed runs: single-threaded DO means a `running` row seen here died
  // with its host (restart, eviction). Crashed is retryable.
  const crashed = cell.sql.exec("SELECT * FROM runs WHERE status = 'running' AND started_at < ?", Date.now() - 2000).toArray();
  for (const r of crashed) {
    const wf = (m.workflows || []).find((w) => w.name === r.wf);
    if (!wf) { updateRun(cell, r.id, { status: "held", finished_at: Date.now(), error: "workflow removed while run in flight" }); continue; }
    const policy = retryPolicy(wf);
    await finishAttempt(cell, wf, r.id, r.attempt, policy, r.via, r.started_at, { ok: false, error: "run interrupted (host restart)" });
  }

  const due = cell.sql.exec("SELECT * FROM runs WHERE status = 'backoff' AND next_attempt_at <= ? ORDER BY id", Date.now()).toArray();
  for (const r of due) {
    const wf = (m.workflows || []).find((w) => w.name === r.wf);
    if (!wf) { updateRun(cell, r.id, { status: "held", finished_at: Date.now(), error: "workflow removed while retry pending" }); continue; }
    if (wf.paused) continue; // the breaker won mid-retry; leave the row for unpause+replay
    const policy = retryPolicy(wf);
    const attempt = r.attempt + 1;
    const t0 = Date.now();
    updateRun(cell, r.id, { status: "running", attempt, started_at: t0, next_attempt_at: null });
    cell.addEvent("run.started", `${wf.name} (retry ${attempt})`, { wf: wf.name, trigger: r.via, runId: r.id, attempt });
    await finishAttempt(cell, wf, r.id, attempt, policy, r.via, t0,
      await cell.runWorkflowLocked(wf, JSON.parse(r.input || "null"), JSON.parse(r.cause || "null")));
  }
}
