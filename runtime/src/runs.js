// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
const HOP_LIMIT = 16;
const BREAKER_N = 5;
const BREAKER_WINDOW = 10 * 6e4;
const LEASE_MS = 10 * 6e4;
const RETRYABLE = /timeout|timed out|abort|network|fetch failed|error sending request|econn|socket|connection|overloaded|rate limit|too many requests|\b429\b|\b502\b|\b503\b|\b504\b/i;
function retryableError(err) {
  return RETRYABLE.test(String(err || ""));
}
function retryPolicy(wf) {
  const r = wf.retry === true ? {} : wf.retry || {};
  return {
    attempts: r.attempts ?? 3,
    backoffMs: r.backoffMs ?? 3e4,
    maxBackoffMs: r.maxBackoffMs ?? 3e5
  };
}
function backoffDelay(policy, attempt) {
  const base = Math.min(policy.backoffMs * Math.pow(2, attempt - 1), policy.maxBackoffMs);
  const jitter = base * (0.8 + Math.random() * 0.4);
  return Math.round(Math.min(jitter, policy.maxBackoffMs));
}
function insertRun(cell, wf, via, input, cause, status, extra = {}) {
  cell.sql.exec(
    "INSERT INTO runs (wf, via, status, input, cause, attempt, max_attempts, started_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    wf.name,
    via,
    status,
    JSON.stringify(input ?? null),
    JSON.stringify(cause ?? null),
    extra.attempt ?? 1,
    extra.maxAttempts ?? 1,
    Date.now()
  );
  return cell.sql.exec("SELECT last_insert_rowid() AS id").toArray()[0].id;
}
function updateRun(cell, id, fields) {
  const sets = [], vals = [];
  for (const [k, v] of Object.entries(fields)) {
    sets.push(`${k} = ?`);
    vals.push(v);
  }
  vals.push(id);
  cell.sql.exec(`UPDATE runs SET ${sets.join(", ")} WHERE id = ?`, ...vals);
}
function setPaused(cell, wf, paused, by, why) {
  const m = cell.manifest();
  const target = (m.workflows || []).find((w) => w.name === wf.name);
  if (!target) return;
  target.paused = paused ? true : void 0;
  cell.setMeta("manifest", JSON.stringify(m));
  cell.sql.exec("DELETE FROM meta WHERE k = ?", `wf_breaker_${wf.name}`);
  cell.addEvent(paused ? "workflow.paused" : "workflow.unpaused", `${wf.name}${why ? ": " + why : ""}`, { wf: wf.name, by });
}
function breakerBump(cell, wf, lastError) {
  const k = `wf_breaker_${wf.name}`;
  const now = Date.now();
  let b;
  try {
    b = JSON.parse(cell.getMeta(k) || "{}");
  } catch {
    b = {};
  }
  if (!b.first || now - b.first > BREAKER_WINDOW) {
    b = { first: now, n: 0 };
  }
  b.n++;
  if (b.n >= BREAKER_N) {
    setPaused(cell, wf, true, "auto", `circuit breaker: ${BREAKER_N} held runs in ${BREAKER_WINDOW / 6e4}min \u2014 ${String(lastError || "").slice(0, 200)}`);
    cell.addEvent("workflow.auto-paused", `${wf.name} auto-paused by circuit breaker`, { wf: wf.name, held: b.n, lastError: String(lastError || "").slice(0, 500) });
  } else {
    cell.setMeta(k, JSON.stringify(b));
  }
}
function recentAutoRuns(cell, wf) {
  return cell.sql.exec(
    "SELECT COUNT(*) c FROM runs WHERE wf = ? AND status NOT IN ('skipped', 'blocked') AND started_at > ?",
    wf.name,
    Date.now() - 36e5
  ).toArray()[0].c;
}
async function executeWorkflow(cell, wf, input, opts = {}) {
  const trigger = opts.trigger || "manual";
  const auto = !!opts.auto;
  const cause = { origin: null, depth: 0, ...opts.cause || {} };
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
    const active = cell.sql.exec(
      "SELECT id, status FROM runs WHERE wf = ? AND (status = 'running' OR (status = 'backoff' AND ? != 'inbox')) AND started_at > ? ORDER BY id DESC LIMIT 1",
      wf.name,
      trigger,
      Date.now() - LEASE_MS
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
  return finishAttempt(
    cell,
    wf,
    runId,
    1,
    policy,
    trigger,
    t0,
    await cell.runWorkflowLocked(wf, input, cause)
  );
}
async function finishAttempt(cell, wf, runId, attempt, policy, trigger, t0, out) {
  if (out.ok) {
    updateRun(cell, runId, { status: "success", finished_at: Date.now(), duration_ms: Date.now() - t0, error: null });
    cell.sql.exec("DELETE FROM meta WHERE k = ?", `wf_breaker_${wf.name}`);
    cell.addEvent("run.succeeded", `${wf.name}`, { wf: wf.name, runId, output: out.output !== null && out.output !== void 0 ? out.output : void 0 });
    return { ok: true, output: out.output ?? null, runId };
  }
  const canRetry = (out.forceRetry || retryableError(out.error)) && attempt < policy.attempts;
  if (canRetry) {
    const inMs = backoffDelay(policy, attempt);
    updateRun(cell, runId, { status: "backoff", next_attempt_at: Date.now() + inMs, error: String(out.error || "").slice(0, 2e3) });
    cell.addEvent("run.retry", `${wf.name}: attempt ${attempt} failed, retrying in ${Math.round(inMs / 1e3)}s`, { wf: wf.name, runId, attempt, inMs, error: String(out.error || "").slice(0, 500) });
    await cell.rearmAlarm();
    return { ok: false, retrying: true, runId, error: out.error };
  }
  updateRun(cell, runId, { status: "held", finished_at: Date.now(), error: String(out.error || "").slice(0, 2e3) });
  cell.addEvent("run.held", `${wf.name}: ${String(out.error || "").slice(0, 200)}`, { wf: wf.name, runId, attempt, error: String(out.error || "").slice(0, 500) });
  breakerBump(cell, wf, out.error);
  return { ok: false, held: true, runId, error: out.error };
}
async function resumeDueRuns(cell) {
  const m = cell.manifest();
  if (!m) return;
  const crashed = cell.sql.exec("SELECT * FROM runs WHERE status = 'running' AND started_at < ?", Date.now() - 6e4).toArray();
  for (const r of crashed) {
    const wf = (m.workflows || []).find((w) => w.name === r.wf);
    if (!wf) {
      updateRun(cell, r.id, { status: "held", finished_at: Date.now(), error: "workflow removed while run in flight" });
      continue;
    }
    const policy = retryPolicy(wf);
    await finishAttempt(cell, wf, r.id, r.attempt, policy, r.via, r.started_at, { ok: false, error: "run interrupted (host restart)", forceRetry: true });
  }
  const due = cell.sql.exec("SELECT * FROM runs WHERE status = 'backoff' AND next_attempt_at <= ? ORDER BY id", Date.now()).toArray();
  for (const r of due) {
    const wf = (m.workflows || []).find((w) => w.name === r.wf);
    if (!wf) {
      updateRun(cell, r.id, { status: "held", finished_at: Date.now(), error: "workflow removed while retry pending" });
      continue;
    }
    if (wf.paused) continue;
    const policy = retryPolicy(wf);
    const attempt = r.attempt + 1;
    const t0 = Date.now();
    updateRun(cell, r.id, { status: "running", attempt, started_at: t0, next_attempt_at: null });
    cell.addEvent("run.started", `${wf.name} (retry ${attempt})`, { wf: wf.name, trigger: r.via, runId: r.id, attempt });
    await finishAttempt(
      cell,
      wf,
      r.id,
      attempt,
      policy,
      r.via,
      t0,
      await cell.runWorkflowLocked(wf, JSON.parse(r.input || "null"), JSON.parse(r.cause || "null"))
    );
  }
}
export {
  BREAKER_N,
  BREAKER_WINDOW,
  HOP_LIMIT,
  LEASE_MS,
  executeWorkflow,
  resumeDueRuns,
  retryPolicy,
  retryableError
};
