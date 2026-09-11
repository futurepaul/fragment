// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { json, randSlug, randHex, isMachinery, mimeForPath } from "./util.js";
import { safeEqual } from "./auth.js";
import { nextRun } from "./cron.js";
import { mintCsJwt, csConfig, verifyWebhookDelivery, webhookDedupeKey } from "./codestorage.js";
import { treeList, readFileStream, ensurePins, pinOf, repoOf, ingestWebhookPush, parsePushPayload, statPath } from "./git-plane.js";
import { wrapSecret } from "./secretwrap.js";
import { awaitNativeRun } from "./wf-engine.js";
const STORAGE_TOKEN_TTL_SEC = 900;
const NAME_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
async function apiRoute(cell, request, url) {
  const p = url.pathname.slice(4);
  const m = cell.manifest();
  if (p === "/inbox" && request.method === "POST") {
    if (!m) return json({ error: "fragment not initialized" }, 404);
    const presented = request.headers.get("x-fragment-inbox-token") || url.searchParams.get("t") || "";
    if (!safeEqual(presented, cell.getMeta("inbox_token") || "")) return json({ error: "bad inbox token" }, 403);
    const pending = cell.sql.exec("SELECT COUNT(*) c FROM inbox WHERE status = 'pending'").toArray()[0].c;
    if (pending > 1e3) {
      cell.addEvent("queue.rejected", `inbox full (${pending} pending)`, { pending });
      return json({ error: "inbox full \u2014 drain pending messages", pending }, 429);
    }
    const body = await request.json().catch(() => ({}));
    const cur = cell.sql.exec(
      "INSERT INTO inbox (at, source, payload) VALUES (?, ?, ?) RETURNING id",
      Date.now(),
      String(body.source || "external"),
      JSON.stringify(body.payload ?? null)
    ).toArray()[0];
    cell.addEvent("inbox", `inbox #${cur.id} from ${body.source || "external"}`);
    const cause = {
      origin: request.headers.get("x-fragment-cause") || null,
      depth: parseInt(request.headers.get("x-fragment-hops") || "0", 10) || 0,
      inboxId: cur.id
    };
    const results = [];
    for (const wf of m.workflows || []) {
      if (wf.trigger !== "inbox") continue;
      await cell.executeWorkflow(wf, { inbox: { id: cur.id, source: body.source, payload: body.payload } }, { auto: true, trigger: "inbox", cause, schedule: true });
      results.push({ workflow: wf.name, scheduled: true });
    }
    return json({ ok: true, id: cur.id, scheduled: results });
  }
  if (p === "/webhook" && request.method === "POST") {
    const secret = cell.getMeta("webhook_secret") || "";
    if (!secret) return json({ error: "fragment has no webhook secret" }, 404);
    const raw = await request.text();
    const event = request.headers.get("x-pierre-event") || "";
    const verdict = await verifyWebhookDelivery(raw, request.headers.get("x-pierre-signature") || "", secret);
    if (verdict.ok === false) {
      cell.addEvent("webhook.rejected", `${event || "?"}: ${verdict.reason}`);
      return json({ error: verdict.reason }, 401);
    }
    let payload = null;
    try {
      payload = JSON.parse(raw);
    } catch {
      payload = null;
    }
    if (payload === null) return json({ error: "invalid JSON payload" }, 400);
    const push = parsePushPayload(event, payload);
    if (push === null) {
      cell.addEvent("webhook.ignored", `${event}: not a tracked branch push`);
      return json({ ok: true, ignored: event });
    }
    const out = await ingestWebhookPush(cell, push, webhookDedupeKey(event, payload));
    return json({ ok: true, ...out });
  }
  if (!m) return json({ error: "fragment not initialized" }, 404);
  const authz = (min) => cell.needRole(request, min);
  const deny = (a) => json({ error: a.error }, a.status);
  if (p === "/status" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    await ensurePins(cell);
    const files = cell.sql.exec("SELECT COUNT(*) c FROM git_tree WHERE ref = 'main'").toArray()[0].c;
    const events = cell.sql.exec("SELECT COUNT(*) c FROM events").toArray()[0].c;
    const held = cell.sql.exec("SELECT COUNT(*) c FROM runs WHERE status = 'held'").toArray()[0].c;
    const crons = [];
    const cronState = JSON.parse(cell.getMeta("cron_state") || "{}");
    for (const wf of m.workflows || []) {
      if (!wf.cron) continue;
      try {
        const next = nextRun(wf.cron, cronState[wf.name] ?? Date.now());
        crons.push({ name: wf.name, cron: wf.cron, nextAt: next ? new Date(next).toISOString() : null, paused: !!wf.paused });
      } catch (e) {
        crons.push({ name: wf.name, cron: wf.cron, error: e.message });
      }
    }
    return json({
      name: m.name,
      npub: cell.getMeta("fragment_npub"),
      visibility: m.visibility,
      repo: cell.getMeta("cs_repo"),
      pins: { main: pinOf(cell, "main"), live: pinOf(cell, "live") },
      counts: { files, events, held },
      crons,
      paused: (m.workflows || []).filter((w) => w.paused).map((w) => w.name),
      viewToken: cell.getMeta("view_token"),
      inboxToken: cell.getMeta("inbox_token"),
      urls: { canonical: cell.canonicalUrl(url.origin, m.name) }
    });
  }
  if (p === "/manifest" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    return json(m);
  }
  if (p === "/storage-token" && request.method === "GET") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const name = cell.getMeta("name");
    if (!NAME_RE.test(name) || name.startsWith("_")) {
      return json({ error: `refusing to mint: fragment name '${name}' fails validation` }, 500);
    }
    const repo = repoOf(cell);
    const scopes = ["git:read", "git:write"];
    const ttlSec = STORAGE_TOKEN_TTL_SEC;
    let token;
    try {
      token = await mintCsJwt(cell.env, { repo, scopes: [...scopes], sub: `editor:${a.pubkey}`, ttlSec });
    } catch (e) {
      return json({ error: String(e.message || e) }, 500);
    }
    const [_, payloadB64] = token.split(".");
    const claims = JSON.parse(atob(payloadB64.replace(/-/g, "+").replace(/_/g, "/")));
    if (claims.repo !== repo || !Array.isArray(claims.scopes) || claims.scopes.join(",") !== "git:read,git:write" || typeof claims.exp !== "number" || claims.exp - claims.iat > STORAGE_TOKEN_TTL_SEC + 1) {
      return json({ error: "minted token claims failed egress validation" }, 500);
    }
    cell.addEvent("storage-token.minted", `editor ${a.pubkey?.slice(0, 16)}\u2026 \u2192 repo ${repo}, scopes ${[...scopes].join("+")}, ${ttlSec}s`, {
      actor: a.pubkey,
      repo,
      scopes: [...scopes],
      expiresAt: claims.exp * 1e3
    });
    const cfg = csConfig(cell.env);
    return json({ token, repo, api: cfg.apiUrl });
  }
  if (p === "/pause" && request.method === "POST") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const { workflow, paused } = await request.json().catch(() => ({}));
    const wf = (m.workflows || []).find((w) => w.name === workflow);
    if (!wf) return json({ error: `no such workflow in manifest: ${workflow}` }, 404);
    wf.paused = paused ? true : void 0;
    cell.setMeta("manifest", JSON.stringify(m));
    cell.sql.exec("DELETE FROM meta WHERE k = ?", `wf_breaker_${wf.name}`);
    cell.addEvent(paused ? "workflow.paused" : "workflow.unpaused", `${wf.name}`, { wf: wf.name, by: "manual" });
    await cell.rearmAlarm();
    return json({ ok: true, workflow: wf.name, paused: !!wf.paused });
  }
  if (p === "/rotate" && request.method === "POST") {
    const a = authz("owner");
    if (!a.ok) return deny(a);
    const body = await request.json().catch(() => ({}));
    if (body.scopes !== void 0 && !Array.isArray(body.scopes)) return json({ error: "unknown scope" }, 400);
    const want = Array.isArray(body.scopes) && body.scopes.length ? body.scopes : ["inbox", "view", "webhook"];
    for (const s of want) {
      if (s !== "inbox" && s !== "view" && s !== "webhook") return json({ error: "unknown scope" }, 400);
    }
    const nextInbox = want.includes("inbox") ? randHex(16) : cell.getMeta("inbox_token") || "";
    const nextView = want.includes("view") ? randSlug(12) : cell.getMeta("view_token") || "";
    const nextWebhook = want.includes("webhook") ? randHex(16) : cell.getMeta("webhook_secret") || "";
    cell.sql.exec(
      "INSERT INTO meta (k, v) VALUES ('inbox_token', ?), ('view_token', ?), ('webhook_secret', ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
      nextInbox,
      nextView,
      nextWebhook
    );
    cell.addEvent("tokens.rotated", want.join("+"), { scopes: want });
    return json({
      ok: true,
      inbox_token: nextInbox,
      view_token: nextView,
      webhook_secret: nextWebhook,
      rotated: ["inbox", "view", "webhook"].filter((s) => want.includes(s))
    });
  }
  if (p === "/rooms" && request.method === "GET") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const rooms = {};
    for (const r of cell.sql.exec("SELECT room, COUNT(*) c, MAX(at) la FROM room_msgs GROUP BY room").toArray()) {
      rooms[r.room] = { room: r.room, count: r.c, last_at: r.la ?? null };
    }
    for (const r of cell.sql.exec("SELECT room FROM rooms").toArray()) {
      if (!rooms[r.room]) rooms[r.room] = { room: r.room, count: 0, last_at: null };
    }
    return json({ rooms: Object.values(rooms).sort((x, y) => (y.last_at || 0) - (x.last_at || 0)) });
  }
  if (p.startsWith("/rooms/") && p.endsWith("/messages") && request.method === "GET") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const mid = p.slice("/rooms/".length, p.length - "/messages".length);
    let room;
    try {
      room = decodeURIComponent(mid);
    } catch {
      return json({ error: "bad room encoding" }, 400);
    }
    let limit = parseInt(url.searchParams.get("limit") || "100", 10) || 100;
    limit = Math.min(Math.max(limit, 1), 200);
    const before = parseInt(url.searchParams.get("before") || "0", 10);
    const parseFrame = (s) => {
      try {
        return JSON.parse(s);
      } catch {
        return s;
      }
    };
    const rows = (before > 0 ? cell.sql.exec("SELECT id, at, sender, data FROM room_msgs WHERE room = ? AND id < ? ORDER BY id DESC LIMIT ?", room, before, limit) : cell.sql.exec("SELECT id, at, sender, data FROM room_msgs WHERE room = ? ORDER BY id DESC LIMIT ?", room, limit)).toArray();
    rows.reverse();
    return json({ room, messages: rows.map((r) => ({ id: r.id, at: r.at, sender: r.sender, data: parseFrame(r.data) })) });
  }
  if (p === "/files" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    await ensurePins(cell);
    const rows = treeList(cell, "main");
    return json({
      ref: pinOf(cell, "main"),
      files: rows.map((r) => ({ path: r.path, size: r.size, mode: r.mode, lastCommitSha: r.last_commit_sha, machinery: isMachinery(r.path) }))
    });
  }
  if (p === "/file" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const resp = await readFileStream(cell, path, "main");
    if (resp.status !== 200) return resp;
    return new Response(resp.body, { status: resp.status, headers: {
      "x-fragment-ref": String(pinOf(cell, "main") || ""),
      "content-type": mimeForPath(path) || "application/octet-stream"
    } });
  }
  if (p === "/file/stat" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    const st = await statPath(cell, path);
    return json({ stat: st, ref: pinOf(cell, "main") });
  }
  if (p === "/run" && request.method === "POST") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const { workflow, input } = await request.json().catch(() => ({}));
    const wf = (m.workflows || []).find((w) => w.name === workflow);
    if (!wf) return json({ error: `no such workflow in manifest: ${workflow}` }, 404);
    const before = cell.sql.exec("SELECT COALESCE(MAX(id), 0) m FROM events").toArray()[0].m;
    const out = await cell.executeWorkflow(wf, input ?? null, { trigger: "manual" });
    let waitStatus = "not-launched";
    if (out.launched && out.instanceId) {
      const w = await awaitNativeRun(cell, out.instanceId);
      waitStatus = w.status;
    }
    const row = cell.sql.exec("SELECT * FROM runs WHERE id = ?", out.runId || 0).toArray()[0];
    const evs = cell.sql.exec("SELECT id, at, kind, summary FROM events WHERE id > ? ORDER BY id", before).toArray();
    let output = null;
    if (row && row.status === "success") {
      for (const e of cell.sql.exec(
        "SELECT data FROM events WHERE kind = 'run.succeeded' AND id > ? ORDER BY id DESC LIMIT 10",
        before
      ).toArray()) {
        try {
          const d = JSON.parse(e.data || "null");
          if (d && Number(d.runId) === Number(out.runId)) {
            output = d.output ?? null;
            break;
          }
        } catch {
        }
      }
    }
    return json({
      ok: row ? row.status === "success" : !!out.ok,
      runId: out.runId ?? null,
      status: row ? row.status : out.blocked ? "blocked" : out.skipped ? "skipped" : waitStatus,
      output,
      error: row?.error ?? out.error ?? null,
      events: evs
    });
  }
  if (p === "/replay" && request.method === "POST") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const { run: runId } = await request.json().catch(() => ({}));
    const row = cell.sql.exec("SELECT * FROM runs WHERE id = ?", Number(runId) || 0).toArray()[0];
    if (!row) return json({ error: `no such run: ${runId}` }, 404);
    const wf = (m.workflows || []).find((w) => w.name === row.wf);
    if (!wf) return json({ error: `workflow ${row.wf} is no longer in the manifest` }, 404);
    let cause = { origin: null, depth: 0 };
    try {
      cause = { ...cause, ...JSON.parse(row.cause || "{}") };
    } catch {
    }
    const out = await cell.executeWorkflow(wf, JSON.parse(row.input || "null"), { trigger: "replay", cause: { ...cause, depth: 0, replayOf: row.id } });
    if (out.ok && out.runId) {
      const fresh = cell.sql.exec("SELECT status FROM runs WHERE id = ?", out.runId).toArray()[0];
      if (fresh && fresh.status === "success") cell.sql.exec("UPDATE runs SET status = 'replayed' WHERE id = ? AND status = 'held'", row.id);
    }
    return json({ ok: !!out.ok, launched: !!out.launched, runId: out.runId ?? null, error: out.error ?? null });
  }
  if (p === "/runs" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const status = url.searchParams.get("status");
    const wfName = url.searchParams.get("wf");
    const limit = Math.min(parseInt(url.searchParams.get("limit") || "50", 10) || 50, 500);
    const conds = [];
    const vals = [];
    if (status) {
      conds.push("status = ?");
      vals.push(status);
    }
    if (wfName) {
      conds.push("wf = ?");
      vals.push(wfName);
    }
    const where = conds.length ? "WHERE " + conds.join(" AND ") : "";
    const rows = cell.sql.exec(`SELECT * FROM runs ${where} ORDER BY id DESC LIMIT ?`, ...vals, limit).toArray();
    const counts = {};
    for (const r of cell.sql.exec("SELECT status, COUNT(*) c FROM runs GROUP BY status").toArray()) counts[r.status] = r.c;
    return json({
      runs: rows.map((r) => ({
        id: r.id,
        wf: r.wf,
        via: r.via,
        status: r.status,
        attempt: r.attempt,
        maxAttempts: r.max_attempts,
        error: r.error || null,
        startedAt: r.started_at,
        finishedAt: r.finished_at,
        durationMs: r.duration_ms,
        nextAttemptAt: r.next_attempt_at,
        input: url.searchParams.get("include") === "input" ? JSON.parse(r.input || "null") : void 0,
        cause: r.cause ? JSON.parse(r.cause) : null
      })),
      counts
    });
  }
  if (p === "/events" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const since = parseInt(url.searchParams.get("since") || "0", 10);
    const rows = cell.sql.exec("SELECT id, at, kind, summary, data FROM events WHERE id > ? ORDER BY id DESC LIMIT 500", since).toArray();
    rows.reverse();
    return json({ events: rows });
  }
  if (p === "/events" && request.method === "POST") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const { kind, summary, data } = await request.json().catch(() => ({}));
    cell.addEvent(String(kind || "note"), String(summary || ""), data);
    return json({ ok: true });
  }
  if (p.startsWith("/secrets/") && request.method === "PUT") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const key = decodeURIComponent(p.slice("/secrets/".length));
    if (!/^[A-Z][A-Z0-9_]*$/.test(key)) return json({ error: "secret names: UPPER_SNAKE" }, 400);
    const value = await request.text();
    const hostSecret = String(cell.env.FRAGMENT_HOST_SECRET || "");
    if (!hostSecret) return json({ error: "FRAGMENT_HOST_SECRET is not set on this host \u2014 refusing to store secrets unwrapped (set CELLD_VAR_FRAGMENT_HOST_SECRET)" }, 500);
    const npub = cell.getMeta("fragment_npub") || "";
    const wrapped = await wrapSecret(hostSecret, npub, value);
    cell.sql.exec("INSERT INTO secrets (name, value) VALUES (?, ?) ON CONFLICT(name) DO UPDATE SET value = excluded.value", key, wrapped);
    cell.addEvent("secret", `secret ${key} set (stored wrapped)`);
    return json({ ok: true });
  }
  if (p === "/secrets" && request.method === "GET") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const rows = cell.sql.exec("SELECT name FROM secrets ORDER BY name").toArray();
    return json({ names: rows.map((r) => r.name) });
  }
  if (p.startsWith("/secrets/") && request.method === "DELETE") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const key = decodeURIComponent(p.slice("/secrets/".length));
    cell.sql.exec("DELETE FROM secrets WHERE name = ?", key);
    return json({ ok: true });
  }
  if (p === "/__registry/delete" && request.method === "POST" && m.name === "_registry") {
    const a = authz("owner");
    if (!a.ok) return deny(a);
    const { name } = await request.json().catch(() => ({}));
    if (!name) return json({ error: "name required" }, 400);
    cell.sql.exec("DELETE FROM fragments WHERE name = ?", name);
    cell.sql.exec("DELETE FROM roles WHERE name = ?", name);
    return json({ ok: true, name });
  }
  return new Response("not found", { status: 404 });
}
export {
  STORAGE_TOKEN_TTL_SEC,
  apiRoute
};
