// The __watch change channel. File HISTORY is git history now (see git-
// plane.ts); what stays here is the persistence-free websocket that
// pushes `changed` frames to watchers the moment an external refresh
// moves the pin. Frames are emitted by git-plane.ts interpretPush — the
// single funnel for cross-cell change signals.
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
  pair[0].send(JSON.stringify({ type: "hello", ref: "main", sha: cell.getMeta("pin_main_sha") || null }));
  return new Response(null, { status: 101, webSocket: pair[1] });
}
