// Rooms: websockets with presence, persisted state, optional rooms.mjs hook.
import { randSlug } from "./util.js";
import { ROOMS_MAIN } from "./loader.js";
import { tierTextBounded } from "./blob-tier.js";


// ------ roomRoute ------

export async function roomRoute(cell, request, url) {
  const room = decodeURIComponent(url.pathname.slice("/__room/".length));
  const draftParam = url.searchParams.get("draft") || "blessed";
  if (draftParam === "blessed") {
    const vis = cell.checkVisibility(request, url);
    if (!vis.ok) return new Response(vis.error + "\n", { status: vis.status });
  }
  if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
    return new Response("websocket upgrade required", { status: 426 });
  }
  const pair = new WebSocketPair();
  const server = pair[0];
  cell.state.acceptWebSocket(server);
  const clientId = randSlug(6);
  server.serializeAttachment({ room, clientId, label: null, draft: draftParam });
  // hello
  const stateRow = cell.sql.exec("SELECT state FROM rooms WHERE room = ?", room).toArray()[0];
  const history = cell.sql.exec("SELECT sender, data, at FROM room_msgs WHERE room = ? ORDER BY id DESC LIMIT 50", room).toArray().reverse();
  server.send(JSON.stringify({
    type: "hello",
    state: stateRow ? JSON.parse(stateRow.state) : null,
    presence: cell.presenceList(room),
    history: history.map((h) => ({ from: h.sender, data: JSON.parse(h.data), at: h.at })),
  }));
  return new Response(null, { status: 101, webSocket: pair[1] });
}

// ------ presenceList ------

export function presenceList(cell, room) {
  const out = [];
  for (const ws of cell.state.getWebSockets()) {
    try {
      const a = ws.deserializeAttachment();
      if (a && a.room === room) out.push({ clientId: a.clientId, label: a.label });
    } catch {}
  }
  return out;
}

// ------ broadcast ------

export function broadcast(cell, room, text) {
  for (const ws of cell.state.getWebSockets()) {
    try {
      const a = ws.deserializeAttachment();
      if (a && a.room === room) ws.send(text);
    } catch {}
  }
}

// ------ webSocketMessage ------

export async function webSocketMessage(cell, ws, raw) {
  let a;
  try { a = ws.deserializeAttachment(); } catch { return; }
  if (!a || !a.room) return;
  let msg;
  try { msg = JSON.parse(typeof raw === "string" ? raw : new TextDecoder().decode(raw)); } catch { return; }

  if (msg.type === "presence") {
    ws.serializeAttachment({ ...a, label: msg.data?.label ?? null });
    cell.broadcast(a.room, JSON.stringify({ type: "presence", list: cell.presenceList(a.room) }));
    return;
  }

  if (msg.type === "state:set") {
    cell.sql.exec("INSERT INTO rooms (room, state) VALUES (?, ?) ON CONFLICT(room) DO UPDATE SET state = excluded.state", a.room, JSON.stringify(msg.value ?? null));
    cell.broadcast(a.room, JSON.stringify({ type: "state", value: msg.value ?? null }));
    return;
  }

  if (msg.type === "msg") {
    let data = msg.data;
    // server-side hook from the served draft's rooms.mjs
    const slug = a.draft === "blessed" ? cell.getMeta("blessed") : a.draft;
    if (slug) {
      const row = cell.sql.exec("SELECT sha256, size, mime FROM draft_files WHERE slug = ? AND path = 'rooms.mjs'", slug).toArray()[0];
      if (row) {
        try {
          const isBlessed = cell.getMeta("blessed") === slug;
          // blessed apps' rooms code is permanent like the app itself —
          // a 1h preview token here dies mid-conversation; and the loader
          // id carries blessed-ness so a preview worker is never reused
          // for canonical traffic
          const src = await tierTextBounded(cell, row, "module rooms.mjs");
          const ep = await cell.loadCode(`rooms:${isBlessed ? "b" : "d"}:${slug}`, ROOMS_MAIN,
            { "rooms.mjs": src }, { kind: "draft", worker: "rooms", slug, blessed: isBlessed });
          const resp = await ep.fetch("http://loaded/rooms", {
            method: "POST",
            body: JSON.stringify({ room: a.room, msg: { from: a.clientId, data, at: Date.now() } }),
          });
          const out = await resp.json();
          if (out.error) {
            cell.addEvent("room-error", `rooms.mjs in ${a.room}: ${out.error}`);
            ws.send(JSON.stringify({ type: "error", error: out.error }));
            return;
          }
          if ("broadcast" in out) data = out.broadcast;
          if ("state" in out) {
            cell.sql.exec("INSERT INTO rooms (room, state) VALUES (?, ?) ON CONFLICT(room) DO UPDATE SET state = excluded.state", a.room, JSON.stringify(out.state));
            cell.broadcast(a.room, JSON.stringify({ type: "state", value: out.state }));
          }
          if (out.drop) {
            cell.addEvent("room-drop", `rooms.mjs dropped a message in ${a.room}${out.reason ? ": " + out.reason : ""}`);
            return;
          }
        } catch (e) {
          cell.addEvent("room-error", `rooms.mjs threw in ${a.room}: ${String(e)}`);
          ws.send(JSON.stringify({ type: "error", error: "rooms.mjs: " + String(e) }));
          return;
        }
      }
    }
    const at = Date.now();
    cell.sql.exec("INSERT INTO room_msgs (room, at, sender, data) VALUES (?, ?, ?, ?)", a.room, at, a.clientId, JSON.stringify(data ?? null));
    cell.sql.exec("DELETE FROM room_msgs WHERE room = ? AND id < (SELECT COALESCE(MIN(id), 0) FROM (SELECT id FROM room_msgs WHERE room = ? ORDER BY id DESC LIMIT 50))", a.room, a.room);
    cell.broadcast(a.room, JSON.stringify({ type: "msg", from: a.clientId, data, at }));
  }
}

// ------ webSocketClose ------

export async function webSocketClose(cell, ws) {
  let a;
  try { a = ws.deserializeAttachment(); } catch { return; }
  if (a && a.room) cell.broadcast(a.room, JSON.stringify({ type: "presence", list: cell.presenceList(a.room) }));
}
