// The /__internal plane: loopback API for ctx calls from loader isolates.
// Run-token (and optional host-secret) gated.
import { json, isMachinery, randHex, bodyTooLarge, MAX_BODY_BYTES, MIME, mimeForPath } from "./util.js";
import { checkToken } from "./loader.js";
import { recordRevision } from "./history.js";
import { READ_CEILING, TierError, admitFileWrite, tierStreamByHash } from "./blob-tier.js";

function appendOnlyHit(cell, path) {
  const m = cell.manifest();
  return m && (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}


// ------ internalRoute ------

export async function internalRoute(cell, request, url) {
  if (bodyTooLarge(request)) {
    return json({ error: `body too large: cells accept at most ${MAX_BODY_BYTES} bytes per write — keep big assets out of workflows` }, 413);
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
    // run scope, or a served app (live by default; freeze pins the
    // snapshot), reads the working copy: code stays frozen in the deploy
    // snapshot, data flows live.
    const live = isRun || cell.manifest()?.freeze !== true;
    const row = live
      ? cell.getFileMeta(path)
      : cell.sql.exec("SELECT sha256, size, mime FROM draft_files WHERE slug = ? AND path = ?", scope.slug, path).toArray()[0];
    if (!row) return json({ error: live ? "no such file" : "no such file in draft" }, 404);
    // 8MiB decode ceiling: ctx.files.read consumers .text()/.arrayBuffer()
    // whatever lands, so a bigger body would sit whole on the isolate heap —
    // exactly the shape the two-tier split exists to kill. Point big reads
    // at hashed/ranged access instead.
    if ((row.size | 0) > READ_CEILING) {
      return json({ error: `file is ${(row.size / 1048576).toFixed(1)}MiB — over the ${READ_CEILING / 1048576}MiB decode ceiling for whole-file reads; consume it via its hash (${String(row.sha256).slice(0, 12)}…) with ranged/streamed access` }, 413);
    }
    // proxy-stream loopback: bytes flow through untouched, never heap-buffered
    const upstream = await tierStreamByHash(cell, row.sha256);
    return new Response(upstream.body, {
      headers: { "content-type": row.mime || mimeForPath(path) || "application/octet-stream" },
    });
  }

  if (rest === "files/write" && request.method === "PUT") {
    if (!isRun) return json({ error: "drafts are immutable" }, 403);
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    // admission classifies the wire form and resolves the content address
    // BEFORE any row state is touched (cheap gates stay byte-free)
    let adm;
    try {
      adm = await admitFileWrite(cell, request, mimeForPath(path) || MIME.txt);
    } catch (e) {
      const status = e instanceof TierError ? e.status : 400;
      return json({ error: String(e.message || e), ...(status === 413 ? { hint: "blob-first" } : {}) }, status);
    }
    // write-suppression: identical content is a recorded no-op. Re-writing
    // the same bytes must not churn rev/updatedAt — pollers and revcron
    // feeds key on those, and churn is the fuel of copy-loops.
    const existing = cell.sql.exec("SELECT rev, sha256 FROM files WHERE path = ? AND deleted = 0", path).toArray()[0];
    if (existing && existing.sha256 === adm.effSha) {
      cell.addEvent("write.deduped", path);
      return json({ ok: true, deduped: true, rev: existing.rev });
    }
    // static site files serve from the deploy SNAPSHOT — a workflow
    // writing here updates nothing visitors see (r2-news's silent-empty-
    // feed failure mode)
    if (path.startsWith("site/")) {
      cell.addEvent("write.warn", `${path}: workflows writing into site/ serve from the deploy snapshot — data files belong outside site/`);
    }
    // workflows inherit append-only constraints: identical rewrites are
    // no-ops (above), modifications under a prefix are refused
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
    // paired assertions: identity must survive placement, and a name is
    // never committed without a size — dangling names break every reader
    if (desc.sha256 !== adm.effSha) {
      return json({ error: `hash mismatch: tier received ${desc.sha256}, caller declared ${adm.effSha}` }, 400);
    }
    if (!(desc.sha256 && Number.isSafeInteger(desc.size))) {
      return json({ error: "tier descriptor incomplete — refusing to commit a dangling name" }, 502);
    }
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    cell.sql.exec("INSERT INTO files (path, sha256, size, mime, rev, updated_at, deleted) VALUES (?, ?, ?, ?, ?, ?, 0) ON CONFLICT(path) DO UPDATE SET sha256 = excluded.sha256, size = excluded.size, mime = excluded.mime, rev = excluded.rev, updated_at = excluded.updated_at, deleted = 0",
      path, desc.sha256, desc.size, desc.mime, newRev, Date.now());
    await recordRevision(cell, path, newRev, desc.sha256);
    return json({ ok: true, deduped: false, rev: newRev });
  }

  if (rest === "files/list") {
    const prefix = url.searchParams.get("prefix") || "";
    const live = isRun || cell.manifest()?.freeze !== true;
    // sizes are stored columns now — no length(content) scan over bodies
    const rows = live
      ? cell.sql.exec("SELECT path, size, updated_at, rev FROM files WHERE path LIKE ? AND deleted = 0 ORDER BY path", prefix + "%").toArray()
      : cell.sql.exec("SELECT path, size, 0 AS updated_at, 0 AS rev FROM draft_files WHERE slug = ? AND path LIKE ? ORDER BY path", scope.slug, prefix + "%").toArray();
    return json({
      paths: rows.map((r) => r.path),
      files: rows.map((r) => ({ path: r.path, size: r.size, updatedAt: r.updated_at || null, rev: r.rev || 0 })),
    });
  }

  if (rest === "inbox/pending") {
    // claim-on-drain: pending rows flip to claimed under a unique token in
    // the same pass that reads them, so two runs can never see the same
    // message — even when a read trails another run's ack by a write
    // barrier (observed on single-node CI: 2 messages appended twice)
    // crypto-random like every other token the runtime mints
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
    for (const id of ids.slice(0, 1000)) {
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
