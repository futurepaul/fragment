// The single DO alarm, shared by cron schedules, coalesced sync triggers,
// and run retries (backoff waits). Firing order: sync triggers → cron →
// crash sweep + due retries (runs.resumeDueRuns) → rearm.
import { parseCron, nextRun, cronMatches } from "./cron.js";
import { resumeDueRuns } from "./runs.js";


// ------ rearmAlarm ------

export async function rearmAlarm(cell) {
  const m = cell.manifest();
  if (!m) return;
  const cronState = JSON.parse(cell.getMeta("cron_state") || "{}");
  let next = null;
  for (const wf of m.workflows || []) {
    if (!wf.cron) continue;
    try {
      const t = nextRun(wf.cron, cronState[wf.name] ?? Date.now());
      if (t !== null && (next === null || t < next)) next = t;
    } catch {}
  }
  // the pending sync trigger (if any) competes for the one alarm
  const syncAt = parseInt(cell.getMeta("sync_trigger_at") || "0", 10);
  if (syncAt && (next === null || syncAt < next)) next = syncAt;
  // and so does the soonest pending retry
  const retry = cell.sql.exec("SELECT MIN(next_attempt_at) t FROM runs WHERE status = 'backoff'").toArray()[0];
  if (retry && retry.t && (next === null || retry.t < next)) next = retry.t;
  if (next !== null) await cell.state.storage.setAlarm(next);
  else await cell.state.storage.deleteAlarm();
}

// ------ alarm ------

export async function alarm(cell) {
  const m = cell.manifest();
  if (!m) return;
  await cell.fireSyncTriggers(m);
  const cronState = JSON.parse(cell.getMeta("cron_state") || "{}");
  const now = Date.now();
  for (const wf of m.workflows || []) {
    if (!wf.cron) continue; // paused is a guard inside executeWorkflow, recorded as a blocked run
    let parsed;
    try { parsed = parseCron(wf.cron); } catch { continue; }
    const last = cronState[wf.name];
    let dueAt = null;
    if (last === undefined) {
      if (cronMatches(parsed, new Date(now))) dueAt = Math.floor(now / 60000) * 60000;
    } else {
      const t = nextRun(parsed, last);
      if (t !== null && t <= now) dueAt = t;
    }
    if (dueAt !== null) {
      await cell.executeWorkflow(wf, { cron: wf.cron, scheduledTime: dueAt }, { auto: true, trigger: "cron" });
      cronState[wf.name] = dueAt;
      cell.setMeta("cron_state", JSON.stringify(cronState));
    }
  }
  await cell.resumeDueRuns();
  await cell.rearmAlarm();
}

// ------ scheduleSyncTrigger ------

export async function scheduleSyncTrigger(cell, path) {
  const m = cell.manifest();
  if (!m || !(m.workflows || []).some((wf) => wf.trigger === "sync")) return;
  const dirty = new Set(JSON.parse(cell.getMeta("sync_dirty_paths") || "[]"));
  dirty.add(path);
  cell.setMeta("sync_dirty_paths", JSON.stringify([...dirty].slice(-500)));
  if (!parseInt(cell.getMeta("sync_trigger_at") || "0", 10)) {
    // debounce: bundle bursts of editor-plane writes into one trigger
    cell.setMeta("sync_trigger_at", String(Date.now() + (m.debounceMs ?? 4000)));
    await cell.rearmAlarm();
  }
}

// ------ fireSyncTriggers ------

export async function fireSyncTriggers(cell, m) {
  const at = parseInt(cell.getMeta("sync_trigger_at") || "0", 10);
  // debounce window still open — the alarm fired for something else (a
  // cron or a retry); the sync trigger keeps its slot at `at`
  if (!at || at > Date.now()) return;
  cell.setMeta("sync_trigger_at", "");
  const paths = JSON.parse(cell.getMeta("sync_dirty_paths") || "[]");
  cell.setMeta("sync_dirty_paths", "[]");
    for (const wf of m.workflows || []) {
    if (wf.trigger !== "sync") continue; // paused is a guard inside executeWorkflow
    await cell.executeWorkflow(wf, { sync: { paths, at } }, { auto: true, trigger: "sync" });
  }
}
