// The /__internal plane: loopback API for ctx calls from loader isolates.
// Run-token (and optional host-secret) gated.
import { json, isMachinery, randHex, bodyTooLarge, MAX_BODY_BYTES, MIME, mimeForPath } from "./util.js";
import { checkToken } from "./loader.js";
import { recordRevision } from "./history.js";
import { READ_CEILING, TierError, admitFileWrite, tierStreamByHash } from "./blob-tier.js";
import { encryptPayload, vapidHeaders, generateVapidKeys, webpushSelfTest, b64urlDecode } from "./webpush.js";

function appendOnlyHit(cell, path) {
  const m = cell.manifest();
  return m && (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}

// ---- Web Push storage/provisioning helpers ----
// push_subs is additive (schema v4): cells born on v4 get it from SCHEMA,
// cells born earlier meet it here first — CREATE TABLE IF NOT EXISTS is
// idempotent, and the WeakSet keeps repeat calls to a pragma-free no-op
// per cell instance.
const pushTablesReady = new WeakSet();
export function ensurePushTable(cell) {
  if (pushTablesReady.has(cell)) return;
  cell.sql.exec("CREATE TABLE IF NOT EXISTS push_subs (who TEXT, endpoint TEXT PRIMARY KEY, p256dh TEXT, auth TEXT, at INTEGER, fails INTEGER DEFAULT 0)");
  pushTablesReady.add(cell);
}

// Get-or-generate the cell's VAPID keypair. Stored as cell meta:
//   push_vapid_priv — JSON JWK (signing key; rotate by overwriting)
//   push_vapid_pub  — base64url of the 65-byte raw point (what pages
//                     feed pushManager.subscribe as applicationServerKey)
// Exported so the site plane (serve.ts GET __push-key, sw-client's
// subscribe route) can provision the keypair BEFORE the first workflow
// send — a page cannot subscribe without the public key in hand.
export async function pushVapidFor(cell) {
  const privRaw = cell.getMeta("push_vapid_priv");
  const pubRaw = cell.getMeta("push_vapid_pub");
  if (privRaw && pubRaw) {
    try {
      return { privJwk: JSON.parse(privRaw), pubRaw };
    } catch {
      // fall through: a corrupt meta row regenerates rather than poisons
    }
  }
  const keys = await generateVapidKeys();
  cell.setMeta("push_vapid_priv", JSON.stringify(keys.privJwk));
  cell.setMeta("push_vapid_pub", keys.pubRaw);
  cell.addEvent("push.vapid", "generated VAPID keypair", { pubRaw: keys.pubRaw.slice(0, 20) + "…" });
  return keys;
}

// A non-gone failure (4xx/5xx/network): bump the failure counter in place
// (atomic in SQL, so concurrent send batches can't lose an increment) and
// describe what happened — dropping happens at 5.
function failNote(cell, endpoint, why) {
  cell.sql.exec("UPDATE push_subs SET fails = fails + 1 WHERE endpoint = ?", endpoint);
  const row = cell.sql.exec("SELECT fails FROM push_subs WHERE endpoint = ?", endpoint).toArray()[0];
  const fails = row ? row.fails : "?";
  if (row && row.fails >= 5) {
    cell.sql.exec("DELETE FROM push_subs WHERE endpoint = ?", endpoint);
    return { note: `${why} (${fails} fails) → dropped`, dropped: true };
  }
  return { note: `${why} (fails ${fails})` };
}


// Shared push-subscription storage cores: the run-scoped internal routes
// and the PUBLIC site-plane routes (serve.ts __push-sub/__push-unsub, which
// ride the fragment's normal visibility gate) enforce identical validation
// because they are the same code.
export function pushSubStore(cell, body) {
  const { who, endpoint, p256dh, auth } = body || {};
  if (typeof who !== "string" || !who.trim()) return json({ error: "body: {who, endpoint, p256dh, auth} — who required" }, 400);
  if (typeof endpoint !== "string" || !/^https:\/\//.test(endpoint) || endpoint.length > 500)
    return json({ error: "endpoint must be an https:// push-service URL (<= 500 chars)" }, 400);
  if (typeof p256dh !== "string" || typeof auth !== "string") return json({ error: "p256dh and auth (base64url) required" }, 400);
  const key = b64urlDecode(p256dh), secret = b64urlDecode(auth);
  if (key.length !== 65) return json({ error: `p256dh must decode to 65 bytes, got ${key.length}` }, 400);
  if (secret.length !== 16) return json({ error: `auth must decode to 16 bytes, got ${secret.length}` }, 400);
  const w = who.trim().slice(0, 40);
  // endpoint-keyed: a device re-subscribing refreshes in place (and
  // resets its failure count); a person renames by re-subscribing
  cell.sql.exec(
    "INSERT INTO push_subs (who, endpoint, p256dh, auth, at, fails) VALUES (?, ?, ?, ?, ?, 0) " +
    "ON CONFLICT(endpoint) DO UPDATE SET who = excluded.who, p256dh = excluded.p256dh, auth = excluded.auth, at = excluded.at, fails = 0",
    w, endpoint, p256dh, auth, Date.now());
  // device flood-brake: keep the newest 25 devices per person
  const stale = cell.sql.exec("SELECT rowid FROM push_subs WHERE who = ? ORDER BY at DESC, rowid DESC LIMIT 100 OFFSET 25", w).toArray();
  for (const r of stale) cell.sql.exec("DELETE FROM push_subs WHERE rowid = ?", r.rowid);
  cell.addEvent("push.sub", `${w}: subscription stored`, { who: w, endpoint: endpoint.slice(0, 60) });
  return json({ ok: true });
}

export function pushUnsubStore(cell, body) {
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

  if (rest === "files/stat") {
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    // live row INCLUDING tombstones: read-modify-write loops compare revs
    // against this, so a deleted path must surface (deleted: true) rather
    // than 404 — "absent" and "never existed" carry different rev history
    const row = cell.sql.exec("SELECT rev, sha256, size, deleted FROM files WHERE path = ?", path).toArray()[0];
    if (!row) return json({ stat: null });
    return json({ stat: { path, rev: row.rev || 0, sha256: row.sha256 || "", size: row.size || 0, deleted: !!row.deleted } });
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
    // optimistic concurrency: ifRev pins the write to a known row state, so
    // a stale snapshot (a sweep holding metas across a slow AI call, two
    // workflows editing one file) fails loudly here instead of clobbering.
    // A deleted row counts as absent: its rev belongs to the tombstone.
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

  // ------ Web Push (RFC 8291 payloads, RFC 8292 VAPID) ------
  // Run-scoped like files/write: subscriptions are managed through the
  // site plane (sw-client) and sends through ctx.push from workflows.

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
    const p = (payload && typeof payload === "object") ? payload : {};
    const title = String(p.title || "").slice(0, 80);
    if (!title.trim()) return json({ error: "payload.title required (<= 80 chars)" }, 400);
    // one JSON message, caps enforced here so ctx.push callers can't
    // smuggle oversized payloads to devices
    const message = JSON.stringify({
      title,
      body: String(p.body || "").slice(0, 200),
      ...(p.url ? { url: String(p.url).slice(0, 500) } : {}),
      ...(p.tag ? { tag: String(p.tag).slice(0, 100) } : {}),
    });

    // crypto sanity once per cell, before the first byte leaves: a broken
    // WebCrypto edge would otherwise fail silently as 400s from every
    // push service on earth
    if (!cell.getMeta("push_selftest_done")) {
      const t = await webpushSelfTest();
      cell.addEvent("push.selftest", t.ok ? "webpush crypto self-test passed" : "webpush crypto self-test FAILED", t);
      cell.setMeta("push_selftest_done", t.ok ? "ok" : "failed");
      if (!t.ok) return json({ error: "webpush crypto self-test failed — refusing to send", detail: t.detail }, 500);
    }

    const keys = await pushVapidFor(cell);
    const subs = cell.sql.exec(
      "SELECT endpoint, p256dh, auth FROM push_subs WHERE who = ? ORDER BY at ASC LIMIT 20", who.trim().slice(0, 40)).toArray();
    // sends run concurrently: a batch of dead endpoints must cost one
    // timeout (10s), not 20 × 10s of the calling run's budget
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
            "crypto-key": vh["crypto-key"],
          },
          body: enc.body as unknown as BodyInit,
          signal: AbortSignal.timeout(10_000),
        });
        if (resp.ok) return { sent: true };
        if (resp.status === 404 || resp.status === 410) {
          // the subscription is gone at the service — keep ours or every
          // future send pays for a dead endpoint
          cell.sql.exec("DELETE FROM push_subs WHERE endpoint = ?", s.endpoint);
          return { note: `${resp.status} → dropped`, dropped: true };
        }
        const why = (await resp.text().catch(() => "")).slice(0, 120);
        return failNote(cell, s.endpoint, `${resp.status}${why ? ": " + why : ""}`);
      } catch (e) {
        // network/timeout: count it, don't let one dead endpoint kill the batch
        return failNote(cell, s.endpoint, `error: ${String((e && e.message) || e).slice(0, 80)}`);
      }
    }));
    const sent = results.filter((r) => r.sent).length;
    const dropped = results.filter((r) => r.dropped).length;
    const notes = results.map((r) => r.note).filter(Boolean);
    const detail = subs.length
      ? `${sent}/${subs.length} sent` + (dropped ? `, ${dropped} dropped` : "") + (notes.length ? ` — ${notes.join("; ")}` : "")
      : `no subscriptions for ${who.trim().slice(0, 40)}`;
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
