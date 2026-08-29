// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { json, randHex, bodyTooLarge, MAX_BODY_BYTES, MIME, mimeForPath } from "./util.js";
import { recordRevision } from "./history.js";
import { READ_CEILING, TierError, admitFileWrite, tierStreamByHash } from "./blob-tier.js";
function appendOnlyHit(cell, path) {
  const m = cell.manifest();
  return m && (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}
async function internalRoute(cell, request, url) {
  if (bodyTooLarge(request)) {
    return json({ error: `body too large: cells accept at most ${MAX_BODY_BYTES} bytes per write \u2014 keep big assets out of workflows` }, 413);
  }
  const p = url.pathname.slice("/__internal/f/".length);
  const slash = p.indexOf("/");
  const rest = p.slice(slash + 1);
  const scope = cell.checkToken(request);
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
    const live = isRun || cell.manifest()?.freeze !== true;
    const row = live ? cell.getFileMeta(path) : cell.sql.exec("SELECT sha256, size, mime FROM draft_files WHERE slug = ? AND path = ?", scope.slug, path).toArray()[0];
    if (!row) return json({ error: live ? "no such file" : "no such file in draft" }, 404);
    if ((row.size | 0) > READ_CEILING) {
      return json({ error: `file is ${(row.size / 1048576).toFixed(1)}MiB \u2014 over the ${READ_CEILING / 1048576}MiB decode ceiling for whole-file reads; consume it via its hash (${String(row.sha256).slice(0, 12)}\u2026) with ranged/streamed access` }, 413);
    }
    const upstream = await tierStreamByHash(cell, row.sha256);
    return new Response(upstream.body, {
      headers: { "content-type": row.mime || mimeForPath(path) || "application/octet-stream" }
    });
  }
  if (rest === "files/stat") {
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    const row = cell.sql.exec("SELECT rev, sha256, size, deleted FROM files WHERE path = ?", path).toArray()[0];
    if (!row) return json({ stat: null });
    return json({ stat: { path, rev: row.rev || 0, sha256: row.sha256 || "", size: row.size || 0, deleted: !!row.deleted } });
  }
  if (rest === "files/write" && request.method === "PUT") {
    if (!isRun) return json({ error: "drafts are immutable" }, 403);
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    let adm;
    try {
      adm = await admitFileWrite(cell, request, mimeForPath(path) || MIME.txt);
    } catch (e) {
      const status = e instanceof TierError ? e.status : 400;
      return json({ error: String(e.message || e), ...status === 413 ? { hint: "blob-first" } : {} }, status);
    }
    const existing = cell.sql.exec("SELECT rev, sha256 FROM files WHERE path = ? AND deleted = 0", path).toArray()[0];
    const ifRevRaw = url.searchParams.get("if_rev");
    if (ifRevRaw !== null) {
      const ifRev = parseInt(ifRevRaw, 10);
      const curRev = existing ? existing.rev : 0;
      if (ifRev !== curRev) {
        return json({ error: "rev conflict", path, currentRev: curRev, ifRev }, 409);
      }
    }
    if (existing && existing.sha256 === adm.effSha) {
      cell.addEvent("write.deduped", path);
      return json({ ok: true, deduped: true, rev: existing.rev });
    }
    if (path.startsWith("site/")) {
      cell.addEvent("write.warn", `${path}: workflows writing into site/ serve from the deploy snapshot \u2014 data files belong outside site/`);
    }
    if (existing && appendOnlyHit(cell, path)) {
      cell.addEvent("write.refused", `${path}: append-only`);
      return json({ error: "append-only", path }, 409);
    }
    let desc;
    try {
      desc = await adm.place();
    } catch (e) {
      const status = e instanceof TierError ? e.status : 502;
      return json({ error: String(e.message || e) }, status);
    }
    if (desc.sha256 !== adm.effSha) {
      return json({ error: `hash mismatch: tier received ${desc.sha256}, caller declared ${adm.effSha}` }, 400);
    }
    if (!(desc.sha256 && Number.isSafeInteger(desc.size))) {
      return json({ error: "tier descriptor incomplete \u2014 refusing to commit a dangling name" }, 502);
    }
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    cell.sql.exec(
      "INSERT INTO files (path, sha256, size, mime, rev, updated_at, deleted) VALUES (?, ?, ?, ?, ?, ?, 0) ON CONFLICT(path) DO UPDATE SET sha256 = excluded.sha256, size = excluded.size, mime = excluded.mime, rev = excluded.rev, updated_at = excluded.updated_at, deleted = 0",
      path,
      desc.sha256,
      desc.size,
      desc.mime,
      newRev,
      Date.now()
    );
    await recordRevision(cell, path, newRev, desc.sha256);
    return json({ ok: true, deduped: false, rev: newRev });
  }
  if (rest === "files/list") {
    const prefix = url.searchParams.get("prefix") || "";
    const live = isRun || cell.manifest()?.freeze !== true;
    const rows = live ? cell.sql.exec("SELECT path, size, updated_at, rev FROM files WHERE path LIKE ? AND deleted = 0 ORDER BY path", prefix + "%").toArray() : cell.sql.exec("SELECT path, size, 0 AS updated_at, 0 AS rev FROM draft_files WHERE slug = ? AND path LIKE ? ORDER BY path", scope.slug, prefix + "%").toArray();
    return json({
      paths: rows.map((r) => r.path),
      files: rows.map((r) => ({ path: r.path, size: r.size, updatedAt: r.updated_at || null, rev: r.rev || 0 }))
    });
  }
  if (rest === "inbox/pending") {
    const token = randHex(16);
    const rows = cell.sql.exec("SELECT id, at, source, payload FROM inbox WHERE status = 'pending' ORDER BY id LIMIT 100").toArray();
    for (const r of rows) {
      cell.sql.exec("UPDATE inbox SET status = 'claimed', claimed_at = ?, claim_token = ? WHERE id = ? AND status = 'pending'", Date.now(), token, r.id);
    }
    const mine = cell.sql.exec("SELECT id, at, source, payload FROM inbox WHERE claim_token = ? AND status = 'claimed' ORDER BY id", token).toArray();
    return json({ messages: mine.map((r) => ({ id: r.id, at: r.at, source: r.source, payload: JSON.parse(r.payload) })) });
  }
  if (rest === "inbox/ack" && request.method === "POST") {
    const { ids } = await request.json().catch(() => ({}));
    if (!Array.isArray(ids)) return json({ error: "body: {ids: [...]}" }, 400);
    for (const id of ids.slice(0, 1e3)) {
      cell.sql.exec("UPDATE inbox SET status = 'done', claim_token = NULL WHERE id = ? AND status IN ('claimed', 'pending')", Number(id) || 0);
    }
    return json({ ok: true, acked: ids.length });
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
        messages: [{ role: "user", content: String(prompt) }]
      }),
      signal: AbortSignal.timeout(9e4)
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
export {
  internalRoute
};
