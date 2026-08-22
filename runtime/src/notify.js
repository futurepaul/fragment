// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
const MAX_ATTEMPTS = 3;
function enqueueNotify(cell, paths) {
  const urls = (cell.manifest()?.notifyUrls || []).slice(0, 3);
  if (!urls.length) return;
  for (const url of urls) {
    cell.sql.exec(
      "INSERT INTO notify_outbox (url, paths, attempts, next_at) VALUES (?, ?, 0, ?) ON CONFLICT(url) DO UPDATE SET paths = excluded.paths, next_at = excluded.next_at",
      url,
      JSON.stringify(paths.slice(0, 50)),
      Date.now()
    );
  }
  void cell.rearmAlarm();
}
async function drainNotify(cell) {
  const name = cell.getMeta("name");
  const due = cell.sql.exec("SELECT * FROM notify_outbox WHERE next_at <= ? ORDER BY next_at", Date.now()).toArray();
  for (const row of due) {
    const frame = {
      type: "changed",
      fragment: name,
      rev: parseInt(cell.getMeta("rev") || "0", 10),
      paths: JSON.parse(row.paths || "[]")
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
          "x-fragment-cause": String(name)
        },
        body: JSON.stringify(frame),
        signal: AbortSignal.timeout(1e4)
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
      const delay = 15e3 * Math.pow(2, row.attempts);
      cell.sql.exec("UPDATE notify_outbox SET attempts = attempts + 1, next_at = ? WHERE url = ?", Date.now() + delay, row.url);
      return row.attempts + 1;
    }
  }
  return 0;
}
function nextNotifyAt(cell) {
  const row = cell.sql.exec("SELECT MIN(next_at) t FROM notify_outbox").toArray()[0];
  return row && row.t ? row.t : null;
}
export {
  drainNotify,
  enqueueNotify,
  nextNotifyAt
};
