// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { json, randHex, bodyTooLarge, MAX_BODY_BYTES, MIME, mimeForPath } from "./util.js";
import { recordRevision } from "./history.js";
import { READ_CEILING, TierError, admitFileWrite, tierStreamByHash } from "./blob-tier.js";
import { encryptPayload, vapidHeaders, generateVapidKeys, webpushSelfTest, b64urlDecode } from "./webpush.js";
function appendOnlyHit(cell, path) {
  const m = cell.manifest();
  return m && (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}
const pushTablesReady = /* @__PURE__ */ new WeakSet();
function ensurePushTable(cell) {
  if (pushTablesReady.has(cell)) return;
  cell.sql.exec("CREATE TABLE IF NOT EXISTS push_subs (who TEXT, endpoint TEXT PRIMARY KEY, p256dh TEXT, auth TEXT, at INTEGER, fails INTEGER DEFAULT 0)");
  pushTablesReady.add(cell);
}
async function pushVapidFor(cell) {
  const privRaw = cell.getMeta("push_vapid_priv");
  const pubRaw = cell.getMeta("push_vapid_pub");
  if (privRaw && pubRaw) {
    try {
      return { privJwk: JSON.parse(privRaw), pubRaw };
    } catch {
    }
  }
  const keys = await generateVapidKeys();
  cell.setMeta("push_vapid_priv", JSON.stringify(keys.privJwk));
  cell.setMeta("push_vapid_pub", keys.pubRaw);
  cell.addEvent("push.vapid", "generated VAPID keypair", { pubRaw: keys.pubRaw.slice(0, 20) + "\u2026" });
  return keys;
}
function failNote(cell, endpoint, why) {
  cell.sql.exec("UPDATE push_subs SET fails = fails + 1 WHERE endpoint = ?", endpoint);
  const row = cell.sql.exec("SELECT fails FROM push_subs WHERE endpoint = ?", endpoint).toArray()[0];
  const fails = row ? row.fails : "?";
  if (row && row.fails >= 5) {
    cell.sql.exec("DELETE FROM push_subs WHERE endpoint = ?", endpoint);
    return { note: `${why} (${fails} fails) \u2192 dropped`, dropped: true };
  }
  return { note: `${why} (fails ${fails})` };
}
function pushSubStore(cell, body) {
  const { who, endpoint, p256dh, auth } = body || {};
  if (typeof who !== "string" || !who.trim()) return json({ error: "body: {who, endpoint, p256dh, auth} \u2014 who required" }, 400);
  if (typeof endpoint !== "string" || !/^https:\/\//.test(endpoint) || endpoint.length > 500)
    return json({ error: "endpoint must be an https:// push-service URL (<= 500 chars)" }, 400);
  if (typeof p256dh !== "string" || typeof auth !== "string") return json({ error: "p256dh and auth (base64url) required" }, 400);
  const key = b64urlDecode(p256dh), secret = b64urlDecode(auth);
  if (key.length !== 65) return json({ error: `p256dh must decode to 65 bytes, got ${key.length}` }, 400);
  if (secret.length !== 16) return json({ error: `auth must decode to 16 bytes, got ${secret.length}` }, 400);
  const w = who.trim().slice(0, 40);
  cell.sql.exec(
    "INSERT INTO push_subs (who, endpoint, p256dh, auth, at, fails) VALUES (?, ?, ?, ?, ?, 0) ON CONFLICT(endpoint) DO UPDATE SET who = excluded.who, p256dh = excluded.p256dh, auth = excluded.auth, at = excluded.at, fails = 0",
    w,
    endpoint,
    p256dh,
    auth,
    Date.now()
  );
  const stale = cell.sql.exec("SELECT rowid FROM push_subs WHERE who = ? ORDER BY at DESC, rowid DESC LIMIT 100 OFFSET 25", w).toArray();
  for (const r of stale) cell.sql.exec("DELETE FROM push_subs WHERE rowid = ?", r.rowid);
  cell.addEvent("push.sub", `${w}: subscription stored`, { who: w, endpoint: endpoint.slice(0, 60) });
  return json({ ok: true });
}
function pushUnsubStore(cell, body) {
  const { who, endpoint } = body || {};
  if (typeof who !== "string" || !who.trim() || typeof endpoint !== "string" || !endpoint)
    return json({ error: "body: {who, endpoint}" }, 400);
  const hit = cell.sql.exec("SELECT rowid FROM push_subs WHERE who = ? AND endpoint = ?", who.trim().slice(0, 40), endpoint).toArray()[0];
  if (hit) {
    cell.sql.exec("DELETE FROM push_subs WHERE rowid = ?", hit.rowid);
    cell.addEvent("push.unsub", `${who}: subscription removed`, { who: who.trim().slice(0, 40), endpoint: endpoint.slice(0, 60) });
    return json({ ok: true, removed: true });
  }
  return json({ ok: true, removed: false });
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
  if (rest === "push/sub" && request.method === "POST") {
    if (!isRun) return json({ error: "push/sub is run-scoped" }, 403);
    ensurePushTable(cell);
    const body = await request.json().catch(() => ({}));
    return pushSubStore(cell, body);
  }
  if (rest === "push/unsub" && request.method === "POST") {
    if (!isRun) return json({ error: "push/unsub is run-scoped" }, 403);
    ensurePushTable(cell);
    const body = await request.json().catch(() => ({}));
    return pushUnsubStore(cell, body);
  }
  if (rest === "push/send" && request.method === "POST") {
    if (!isRun) return json({ error: "push/send is run-scoped" }, 403);
    ensurePushTable(cell);
    const { who, payload } = await request.json().catch(() => ({}));
    if (typeof who !== "string" || !who.trim()) return json({ error: "who required" }, 400);
    const p2 = payload && typeof payload === "object" ? payload : {};
    const title = String(p2.title || "").slice(0, 80);
    if (!title.trim()) return json({ error: "payload.title required (<= 80 chars)" }, 400);
    const message = JSON.stringify({
      title,
      body: String(p2.body || "").slice(0, 200),
      ...p2.url ? { url: String(p2.url).slice(0, 500) } : {},
      ...p2.tag ? { tag: String(p2.tag).slice(0, 100) } : {}
    });
    if (cell.getMeta("push_selftest_done") !== "ok") {
      const t = await webpushSelfTest();
      cell.addEvent("push.selftest", t.ok ? "webpush crypto self-test passed" : "webpush crypto self-test FAILED", t);
      if (t.ok) cell.setMeta("push_selftest_done", "ok");
      if (!t.ok) return json({ error: "webpush crypto self-test failed \u2014 refusing to send", detail: t.detail }, 500);
    }
    const keys = await pushVapidFor(cell);
    const subs = cell.sql.exec(
      "SELECT endpoint, p256dh, auth FROM push_subs WHERE who = ? ORDER BY at ASC LIMIT 20",
      who.trim().slice(0, 40)
    ).toArray();
    const results = await Promise.all(subs.map(async (s) => {
      try {
        const enc = await encryptPayload(s, message);
        const aud = new URL(s.endpoint).origin;
        const vh = await vapidHeaders(keys.privJwk, keys.pubRaw, aud);
        const resp = await fetch(s.endpoint, {
          method: "POST",
          headers: {
            "content-type": "application/octet-stream",
            ttl: "86400",
            urgency: "high",
            ...enc.headers,
            authorization: vh.authorization,
            "crypto-key": vh["crypto-key"]
          },
          body: enc.body,
          signal: AbortSignal.timeout(1e4)
        });
        if (resp.ok) return { sent: true };
        if (resp.status === 404 || resp.status === 410) {
          cell.sql.exec("DELETE FROM push_subs WHERE endpoint = ?", s.endpoint);
          return { note: `${resp.status} \u2192 dropped`, dropped: true };
        }
        const why = (await resp.text().catch(() => "")).slice(0, 120);
        return failNote(cell, s.endpoint, `${resp.status}${why ? ": " + why : ""}`);
      } catch (e) {
        return failNote(cell, s.endpoint, `error: ${String(e && e.message || e).slice(0, 80)}`);
      }
    }));
    const sent = results.filter((r) => r.sent).length;
    const dropped = results.filter((r) => r.dropped).length;
    const notes = results.map((r) => r.note).filter(Boolean);
    const detail = subs.length ? `${sent}/${subs.length} sent` + (dropped ? `, ${dropped} dropped` : "") + (notes.length ? ` \u2014 ${notes.join("; ")}` : "") : `no subscriptions for ${who.trim().slice(0, 40)}`;
    cell.addEvent("push.send", `${who}: ${detail}`, { who: who.trim().slice(0, 40), sent, dropped, subs: subs.length });
    return json({ sent, dropped, detail });
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
  ensurePushTable,
  internalRoute,
  pushSubStore,
  pushUnsubStore,
  pushVapidFor
};
