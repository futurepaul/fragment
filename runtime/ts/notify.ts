// Notify-on-change: the push half of "bots watching bots". Mutations
// enqueue a notification per manifest notifyUrls entry onto the celld
// Queues binding (NOTIFY); the notify-relay consumer script delivers.
// The hand-rolled retry loop (notify_outbox table + alarm drain) is
// DELETED — delivery, ordering, and retries are queue-owned now
// (at-least-once, 4-day retention; same promise class as before).
//
// One writer (this runtime) and one consumer (notify-relay) fits celld
// Queues' constraints exactly (docs/explorations/queues-eval.md).
// Frames carry the hop budget like every cross-fragment trigger, so
// notify loops die at the receiving inbox's cycle guard.
export const NOTIFY_MAX_URLS = 3; // manifest cap, unchanged

export function enqueueNotify(cell, paths: string[]) {
  const urls = (cell.manifest()?.notifyUrls || []).slice(0, NOTIFY_MAX_URLS);
  if (!urls.length) return Promise.resolve();
  const queue = (cell.env as any).NOTIFY;
  if (!queue || typeof queue.send !== "function") {
    // loud, not fatal: file-plane mutations must not fail because the
    // relay deployment is missing — but the ledger records every drop
    cell.addEvent("notify.unavailable", "NOTIFY queue binding missing on this host — notifications dropped (deploy notify-relay + queues config)", {});
    return Promise.resolve();
  }
  const name = cell.getMeta("name");
  const frame = {
    type: "changed",
    fragment: name,
    sha: cell.getMeta("pin_main_sha") || null,
    paths: paths.slice(0, 50),
  };
  // one message per URL: the relay POSTs each to its destination; queue
  // retries are per-message, so one dead URL never blocks the others.
  // Every enqueue lands on the ledger (audit-trail parity with webhook
  // deliveries); the relay's delivery is at-least-once queue semantics.
  const sends = urls.map((url) =>
    queue.send({ url, source: `notify:${name}`, frame })
      .then(() => cell.addEvent("notify.queued", String(url).slice(0, 120), { url: url.slice(0, 300), paths: paths.slice(0, 10) }))
      .catch((e) => cell.addEvent("notify.enqueue-failed", `${String(url).slice(0, 80)}: ${String((e && e.message) || e).slice(0, 120)}`)),
  );
  return Promise.all(sends);
}
