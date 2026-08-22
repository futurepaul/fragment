// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { parseCron, nextRun, cronMatches } from "./cron.js";
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
    if (!wf.cron || wf.paused) continue;
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
      await cell.runWorkflow(wf, { cron: wf.cron, scheduledTime: dueAt }, { auto: true });
      cronState[wf.name] = dueAt;
      cell.setMeta("cron_state", JSON.stringify(cronState));
    }
  }
  await cell.rearmAlarm();
}
async function scheduleSyncTrigger(cell, path) {
  const m = cell.manifest();
  if (!m || !(m.workflows || []).some((wf) => wf.trigger === "sync")) return;
  const dirty = new Set(JSON.parse(cell.getMeta("sync_dirty_paths") || "[]"));
  dirty.add(path);
  cell.setMeta("sync_dirty_paths", JSON.stringify([...dirty].slice(-500)));
  if (!parseInt(cell.getMeta("sync_trigger_at") || "0", 10)) {
    cell.setMeta("sync_trigger_at", String(Date.now() + 4e3));
    await cell.rearmAlarm();
  }
}
async function fireSyncTriggers(cell, m) {
  const at = parseInt(cell.getMeta("sync_trigger_at") || "0", 10);
  if (!at) return;
  cell.setMeta("sync_trigger_at", "");
  const paths = JSON.parse(cell.getMeta("sync_dirty_paths") || "[]");
  cell.setMeta("sync_dirty_paths", "[]");
  for (const wf of m.workflows || []) {
    if (wf.trigger !== "sync" || wf.paused) continue;
    await cell.runWorkflow(wf, { sync: { paths, at } }, { auto: true });
  }
}
export {
  alarm,
  fireSyncTriggers,
  rearmAlarm,
  scheduleSyncTrigger
};
