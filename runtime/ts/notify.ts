// Notify-on-change: the push half of "bots watching bots". Mutations
// enqueue a notification per manifest notifyUrls entry; the cell's alarm
// drains the outbox with coalescing and bounded retries. Frames carry the
// hop budget like every cross-fragment trigger, so notify loops die at
// the receiving inbox's cycle guard.
const MAX_ATTEMPTS = 3;

export function enqueueNotify(cell, paths) {
  const urls = (cell.manifest()?.notifyUrls || []).slice(0, 3);
  if (!urls.length) return;
  for (const url of urls) {
    // coalesce: one pending row per URL — a newer change supersedes
    cell.sql.exec(
      "INSERT INTO notify_outbox (url, paths, attempts, next_at) VALUES (?, ?, 0, ?) ON CONFLICT(url) DO UPDATE SET paths = excluded.paths, next_at = excluded.next_at",
      url, JSON.stringify(paths.slice(0, 50)), Date.now(),
    );
  }
  // the mutation path doesn't otherwise arm the alarm (sync triggers may
  // not exist) — wake it so the outbox drains immediately
  void cell.rearmAlarm();
}

export async function drainNotify(cell) {
  const name = cell.getMeta("name");
  const due = cell.sql.exec("SELECT * FROM notify_outbox WHERE next_at <= ? ORDER BY next_at", Date.now()).toArray();
  for (const row of due) {
    const frame = {
      type: "changed",
      fragment: name,
      rev: parseInt(cell.getMeta("rev") || "0", 10),
      paths: JSON.parse(row.paths || "[]"),
    };
    let ok = false;
    try {
      const resp = await fetch(row.url, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          // cross-fragment courtesy: carry the hop budget and origin so
          // the receiver's inbox cycle guard applies to notify loops
          "x-fragment-hops": "1",
          "x-fragment-cause": String(name),
        },
        body: JSON.stringify(frame),
        signal: AbortSignal.timeout(10_000),
      });
      ok = resp.ok;
    } catch {
      ok = false;
    }
    if (ok) {
      cell.sql.exec("DELETE FROM notify_outbox WHERE url = ?", row.url);
      cell.addEvent("notify.sent", `${row.url.slice(0, 80)}`);
    } else if (row.attempts + 1 >= MAX_ATTEMPTS) {
      cell.sql.exec("DELETE FROM notify_outbox WHERE url = ?", row.url);
      cell.addEvent("notify.failed", `${row.url.slice(0, 80)} after ${MAX_ATTEMPTS} attempts`);
    } else {
      // exponential backoff on the alarm
      const delay = 15_000 * Math.pow(2, row.attempts);
      cell.sql.exec("UPDATE notify_outbox SET attempts = attempts + 1, next_at = ? WHERE url = ?", Date.now() + delay, row.url);
      return row.attempts + 1; // signal the alarm to rearm
    }
  }
  return 0;
}

export function nextNotifyAt(cell) {
  const row = cell.sql.exec("SELECT MIN(next_at) t FROM notify_outbox").toArray()[0];
  return row && row.t ? row.t : null;
}
