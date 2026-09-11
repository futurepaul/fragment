// The single DO alarm, shared by cron schedules, coalesced sync triggers,
// run retries (backoff waits), and the 5-minute code.storage poll
// backstop. Firing order: sync triggers → cron → crash sweep + due
// retries (runs.resumeDueRuns) → poll backstop → rearm.
import { parseCron, nextRun, cronMatches } from "./cron.js";
import { resumeDueRuns } from "./runs.js";
import { nextPollAt, pollBackstop } from "./git-plane.js";


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
  const retry = cell.sql.exec("SELECT MIN(next_attempt_at) t FROM runs WHERE status IN ('backoff', 'pending')").toArray()[0];
  if (retry && retry.t && (next === null || retry.t < next)) next = retry.t;
  // and the poll backstop (webhooks are the fast path; this is the floor)
  const pollAt = nextPollAt(cell);
  if (pollAt && (next === null || pollAt < next)) next = pollAt;
  if (next !== null) await cell.state.storage.setAlarm(next);
  else await cell.state.storage.deleteAlarm();
}

// ------ alarm ------

export async function alarm(cell) {
  // THE DO ALARM RULE: always re-arm, even when the body throws. A throw
  // anywhere below used to leave no future alarm scheduled — cron died
  // silently while status kept computing a healthy nextAt. The finally
  // is the whole fix.
  try {
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
    // claims held by a run that died (visibility timeout): back to pending
    cell.sql.exec("UPDATE inbox SET status = 'pending', claim_token = NULL WHERE status = 'claimed' AND claimed_at < ?", Date.now() - 10 * 60_000);
    await cell.resumeDueRuns();
    // poll backstop: due when poll_last_at + 5min has passed (pollBackstop
    // restamps poll_last_at; failures inside are events, not throws)
    const pollAt = nextPollAt(cell);
    if (pollAt !== null && pollAt <= Date.now()) await pollBackstop(cell);
  } finally {
    await cell.rearmAlarm();
  }
}

// ------ fireSyncTriggers ------
// (Scheduling now lives in git-plane.ts interpretPush — the single funnel
// for external-change signals; this is the firing half.)

export async function fireSyncTriggers(cell, m) {
  const at = parseInt(cell.getMeta("sync_trigger_at") || "0", 10);
  // debounce window still open — the alarm fired for something else (a
  // cron or a retry); the sync trigger keeps its slot at `at`
  if (!at || at > Date.now()) return;
  cell.setMeta("sync_trigger_at", "");
  const paths = JSON.parse(cell.getMeta("sync_dirty_paths") || "[]");
  cell.setMeta("sync_dirty_paths", "[]");
    for (const wf of m.workflows || []) {
    if (wf.trigger !== "files") continue; // paused is a guard inside executeWorkflow
    await cell.executeWorkflow(wf, { sync: { paths, at } }, { auto: true, trigger: "sync" });
  }
}
