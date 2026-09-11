// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { parseCron, nextRun, cronMatches } from "./cron.js";
import { nextPollAt, pollBackstop } from "./git-plane.js";
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
  const retry = cell.sql.exec("SELECT MIN(next_attempt_at) t FROM runs WHERE status IN ('backoff', 'pending')").toArray()[0];
  if (retry && retry.t && (next === null || retry.t < next)) next = retry.t;
  const pollAt = nextPollAt(cell);
  if (pollAt && (next === null || pollAt < next)) next = pollAt;
  if (next !== null) await cell.state.storage.setAlarm(next);
  else await cell.state.storage.deleteAlarm();
}
async function alarm(cell) {
  try {
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
    cell.sql.exec("UPDATE inbox SET status = 'pending', claim_token = NULL WHERE status = 'claimed' AND claimed_at < ?", Date.now() - 10 * 6e4);
    await cell.resumeDueRuns();
    const pollAt = nextPollAt(cell);
    if (pollAt !== null && pollAt <= Date.now()) await pollBackstop(cell);
  } finally {
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
    if (wf.trigger !== "files") continue;
    await cell.executeWorkflow(wf, { sync: { paths, at } }, { auto: true, trigger: "sync" });
  }
}
export {
  alarm,
  fireSyncTriggers,
  rearmAlarm
};
