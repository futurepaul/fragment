// File history (content-addressed revisions) and the __watch change
// channel. Every mutation of the working tree lands in blobs +
// file_revisions, so sync clients can fetch any recent ancestor for
// three-way merges; watchers get a `changed` frame per mutation batch
// over a dedicated, persistence-free websocket.
const RETENTION = 10;

// record a revision after a successful mutation; broadcasts to watchers
export function recordRevision(cell, path, rev, sha, content, deleted = false) {
  if (!deleted && sha && content != null) {
    cell.sql.exec("INSERT OR IGNORE INTO blobs (hash, content) VALUES (?, ?)", sha, content);
  }
  cell.sql.exec("INSERT OR REPLACE INTO file_revisions (path, rev, blob_hash, deleted, at) VALUES (?, ?, ?, ?, ?)",
    path, rev, deleted ? null : sha, deleted ? 1 : 0, Date.now());
  cell.sql.exec(
    "DELETE FROM file_revisions WHERE path = ? AND rev <= (SELECT COALESCE(MAX(rev), 0) FROM file_revisions WHERE path = ?) - ?",
    path, path, RETENTION,
  );
  watchBroadcast(cell, { type: "changed", rev: parseInt(cell.getMeta("rev") || "0", 10), paths: [path] });
}

export function watchBroadcast(cell, frame) {
  for (const ws of cell.state.getWebSockets()) {
    try {
      const a = ws.deserializeAttachment();
      if (a && a.watch) ws.send(JSON.stringify(frame));
    } catch {}
  }
}

// ------ the channel itself: /__watch, read-only, visibility-gated ------

export function watchRoute(cell, request, url) {
  const vis = cell.checkVisibility(request, url);
  if (!vis.ok) return new Response(vis.error + "\n", { status: vis.status });
  if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
    return new Response("websocket upgrade required", { status: 426 });
  }
  const pair = new WebSocketPair();
  cell.state.acceptWebSocket(pair[0]);
  pair[0].serializeAttachment({ watch: true });
  pair[0].send(JSON.stringify({ type: "hello", rev: parseInt(cell.getMeta("rev") || "0", 10) }));
  return new Response(null, { status: 101, webSocket: pair[1] });
}
