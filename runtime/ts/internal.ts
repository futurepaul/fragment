// The /__internal plane: loopback API for ctx calls from loader isolates.
// Run-token (and optional host-secret) gated.
import { json, toAB } from "./util.js";
import { sha256Hex } from "./auth.js";
import { checkToken } from "./loader.js";


// ------ internalRoute ------

export async function internalRoute(cell, request, url) {
  const p = url.pathname.slice("/__internal/f/".length);
  const slash = p.indexOf("/");
  const rest = p.slice(slash + 1);
  const scope = cell.checkToken(request, url);
  if (!scope) return json({ error: "bad or expired run token" }, 403);
  const isRun = scope.kind === "run";

  if (rest === "ping") return new Response("pong");

  if (rest === "secrets/all") {
    const rows = cell.sql.exec("SELECT name, value FROM secrets").toArray();
    const out = {};
    for (const r of rows) out[r.name] = r.value;
    return json(out);
  }

  if (rest === "files/read") {
    const path = url.searchParams.get("path") || "";
    // run scope, or a served app on a liveFiles fragment, reads the working
    // copy: code stays frozen in the blessed draft, data flows live.
    if (isRun || cell.manifest()?.liveFiles === true) {
      const row = cell.getFileRow(path);
      if (!row) return json({ error: "no such file" }, 404);
      return new Response(toAB(row.content));
    }
    // draft scope: read from the draft snapshot
    const row = cell.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = ?", scope.slug, path).toArray()[0];
    if (!row) return json({ error: "no such file in draft" }, 404);
    return new Response(toAB(row.content));
  }

  if (rest === "files/write" && request.method === "PUT") {
    if (!isRun) return json({ error: "drafts are immutable" }, 403);
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    const body = await request.arrayBuffer();
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    const sha = await sha256Hex(body);
    cell.sql.exec("INSERT INTO files (path, content, rev, sha256, updated_at, deleted) VALUES (?, ?, ?, ?, ?, 0) ON CONFLICT(path) DO UPDATE SET content = excluded.content, rev = excluded.rev, sha256 = excluded.sha256, updated_at = excluded.updated_at, deleted = 0",
      path, body, newRev, sha, Date.now());
    return json({ ok: true, rev: newRev });
  }

  if (rest === "files/list") {
    const prefix = url.searchParams.get("prefix") || "";
    const live = isRun || cell.manifest()?.liveFiles === true;
    const rows = live
      ? cell.sql.exec("SELECT path, length(content) AS size, updated_at, rev FROM files WHERE path LIKE ? AND deleted = 0 ORDER BY path", prefix + "%").toArray()
      : cell.sql.exec("SELECT path, length(content) AS size, 0 AS updated_at, 0 AS rev FROM draft_files WHERE slug = ? AND path LIKE ? ORDER BY path", scope.slug, prefix + "%").toArray();
    return json({
      paths: rows.map((r) => r.path),
      files: rows.map((r) => ({ path: r.path, size: r.size, updatedAt: r.updated_at || null, rev: r.rev || 0 })),
    });
  }

  if (rest === "inbox/pending") {
    const rows = cell.sql.exec("SELECT id, at, source, payload FROM inbox WHERE status = 'pending' ORDER BY id LIMIT 100").toArray();
    return json({ messages: rows.map((r) => ({ id: r.id, at: r.at, source: r.source, payload: JSON.parse(r.payload) })) });
  }

  if (rest === "events" && request.method === "POST") {
    const { kind, summary, data } = await request.json().catch(() => ({}));
    cell.addEvent(String(kind || "run"), String(summary || ""), data);
    return json({ ok: true });
  }

  if (rest === "log" && request.method === "POST") {
    const { msg } = await request.json().catch(() => ({}));
    cell.addEvent("log", `[${scope.workflow || scope.kind}] ${msg}`);
    return json({ ok: true });
  }

  if (rest === "infer" && request.method === "POST") {
    const key = cell.env.OPENROUTER_API_KEY;
    if (!key) return json({ error: "host has no OPENROUTER_API_KEY (set CELLD_VAR_OPENROUTER_API_KEY on the node)" }, 501);
    const { prompt, model } = await request.json().catch(() => ({}));
    const resp = await fetch("https://openrouter.ai/api/v1/chat/completions", {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${key}` },
      body: JSON.stringify({
        model: model || cell.env.FRAGMENT_AI_MODEL || "deepseek/deepseek-v4-flash-0731",
        messages: [{ role: "user", content: String(prompt) }],
      }),
      signal: AbortSignal.timeout(90_000),
    });
    if (!resp.ok) return json({ error: `openrouter ${resp.status}: ${await resp.text()}` }, 502);
    const data = await resp.json();
    return json({ text: data.choices?.[0]?.message?.content ?? "" });
  }

  if (rest === "wstate") {
    const k = url.searchParams.get("k") || "";
    const dim = scope.workflow || scope.kind;
    if (request.method === "GET") {
      const row = cell.sql.exec("SELECT v FROM wstate WHERE scope = ? AND k = ?", dim, k).toArray()[0];
      return json({ v: row ? JSON.parse(row.v) : null });
    }
    if (request.method === "PUT") {
      const { v } = await request.json().catch(() => ({}));
      cell.sql.exec("INSERT INTO wstate (scope, k, v) VALUES (?, ?, ?) ON CONFLICT(scope, k) DO UPDATE SET v = excluded.v", dim, k, JSON.stringify(v ?? null));
      return json({ ok: true });
    }
  }

  if (rest === "rooms/state") {
    const room = url.searchParams.get("room") || "";
    if (request.method === "GET") {
      const row = cell.sql.exec("SELECT state FROM rooms WHERE room = ?", room).toArray()[0];
      return json({ state: row ? JSON.parse(row.state) : null });
    }
    if (request.method === "POST") {
      const { value } = await request.json().catch(() => ({}));
      cell.sql.exec("INSERT INTO rooms (room, state) VALUES (?, ?) ON CONFLICT(room) DO UPDATE SET state = excluded.state", room, JSON.stringify(value ?? null));
      cell.broadcast(room, JSON.stringify({ type: "state", value: value ?? null }));
      return json({ ok: true });
    }
  }

  return new Response("not found", { status: 404 });
}
