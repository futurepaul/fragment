// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { parseCron, nextRun, cronMatches } from "./cron.js";
import { drainNotify, nextNotifyAt } from "./notify.js";
async function rearmAlarm(cell) {
  const m = cell.manifest();
  if (!m) return;
  const cronState = JSON.parse(cell.getMeta("cron_state") || "{}");
  let next = null;
  for (const wf of m.workflows || []) {
    if (!wf.cron) continue;
    try {
      const t = nextRun(wf.cron, cronState[wf.name] ?? Date.now());
      if (t !== null && (next === null || t < next)) next = t;
    } catch {
    }
  }
  const syncAt = parseInt(cell.getMeta("sync_trigger_at") || "0", 10);
  if (syncAt && (next === null || syncAt < next)) next = syncAt;
  const retry = cell.sql.exec("SELECT MIN(next_attempt_at) t FROM runs WHERE status = 'backoff'").toArray()[0];
  if (retry && retry.t && (next === null || retry.t < next)) next = retry.t;
  const notifyAt = nextNotifyAt(cell);
  if (notifyAt && (next === null || notifyAt < next)) next = notifyAt;
  if (next !== null) await cell.state.storage.setAlarm(next);
  else await cell.state.storage.deleteAlarm();
}
async function alarm(cell) {
  const m = cell.manifest();
  if (!m) return;
  await cell.fireSyncTriggers(m);
  const cronState = JSON.parse(cell.getMeta("cron_state") || "{}");
  const now = Date.now();
  for (const wf of m.workflows || []) {
    if (!wf.cron) continue;
    let parsed;
    try {
      parsed = parseCron(wf.cron);
    } catch {
      continue;
    }
    const last = cronState[wf.name];
    let dueAt = null;
    if (last === void 0) {
      if (cronMatches(parsed, new Date(now))) dueAt = Math.floor(now / 6e4) * 6e4;
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
  await drainNotify(cell);
  await cell.rearmAlarm();
}
async function scheduleSyncTrigger(cell, path) {
  const m = cell.manifest();
  if (!m || !(m.workflows || []).some((wf) => wf.trigger === "sync")) return;
  const dirty = new Set(JSON.parse(cell.getMeta("sync_dirty_paths") || "[]"));
  dirty.add(path);
  cell.setMeta("sync_dirty_paths", JSON.stringify([...dirty].slice(-500)));
  if (!parseInt(cell.getMeta("sync_trigger_at") || "0", 10)) {
    cell.setMeta("sync_trigger_at", String(Date.now() + (m.debounceMs ?? 4e3)));
    await cell.rearmAlarm();
  }
}
async function fireSyncTriggers(cell, m) {
  const at = parseInt(cell.getMeta("sync_trigger_at") || "0", 10);
  if (!at || at > Date.now()) return;
  cell.setMeta("sync_trigger_at", "");
  const paths = JSON.parse(cell.getMeta("sync_dirty_paths") || "[]");
  cell.setMeta("sync_dirty_paths", "[]");
  for (const wf of m.workflows || []) {
    if (wf.trigger !== "sync") continue;
    await cell.executeWorkflow(wf, { sync: { paths, at } }, { auto: true, trigger: "sync" });
  }
}
export {
  alarm,
  fireSyncTriggers,
  rearmAlarm,
  scheduleSyncTrigger
};
