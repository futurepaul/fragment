// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
const NOTIFY_MAX_URLS = 3;
function enqueueNotify(cell, paths) {
  const urls = (cell.manifest()?.notifyUrls || []).slice(0, NOTIFY_MAX_URLS);
  if (!urls.length) return Promise.resolve();
  const queue = cell.env.NOTIFY;
  if (!queue || typeof queue.send !== "function") {
    cell.addEvent("notify.unavailable", "NOTIFY queue binding missing on this host \u2014 notifications dropped (deploy notify-relay + queues config)", {});
    return Promise.resolve();
  }
  const name = cell.getMeta("name");
  const frame = {
    type: "changed",
    fragment: name,
    sha: cell.getMeta("pin_main_sha") || null,
    paths: paths.slice(0, 50)
  };
  const sends = urls.map(
    (url) => queue.send({ url, source: `notify:${name}`, frame }).then(() => cell.addEvent("notify.queued", String(url).slice(0, 120), { url: url.slice(0, 300), paths: paths.slice(0, 10) })).catch((e) => cell.addEvent("notify.enqueue-failed", `${String(url).slice(0, 80)}: ${String(e && e.message || e).slice(0, 120)}`))
  );
  return Promise.all(sends);
}
export {
  NOTIFY_MAX_URLS,
  enqueueNotify
};
