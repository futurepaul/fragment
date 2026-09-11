// The /__internal plane: loopback API for ctx calls from loader isolates,
// plus the native-workflow driver routes (/wf/attempt, /wf/complete).
// Run-token (and optional host-secret) gated.
import { json, randHex, mimeForPath, bodyTooLarge, writeBodyTooLarge, WRITE_CEILING } from "./util.js";
import { checkToken, checkTokenRaw, runWorkflowLocked } from "./loader.js";
import { encryptPayload, vapidHeaders, generateVapidKeys, webpushSelfTest, b64urlDecode, b64urlEncode } from "./webpush.js";
import { commitPaths, readFileStream, statPath, treeList, gitBlobSha } from "./git-plane.js";
import { CodeStorageError } from "./codestorage.js";
import { unwrapSecret } from "./secretwrap.js";

function appendOnlyHit(cell, path) {
  const m = cell.manifest();
  return m && (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}

// ---- Web Push storage/provisioning helpers ----
// push_subs is additive: cells born on v5 get it from SCHEMA, cells born
// earlier meet it here first — CREATE TABLE IF NOT EXISTS is idempotent,
// and the WeakSet keeps repeat calls to a pragma-free no-op per cell.
const pushTablesReady = new WeakSet();
export function ensurePushTable(cell) {
  if (pushTablesReady.has(cell)) return;
  cell.sql.exec("CREATE TABLE IF NOT EXISTS push_subs (who TEXT, endpoint TEXT PRIMARY KEY, p256dh TEXT, auth TEXT, at INTEGER, fails INTEGER DEFAULT 0)");
  pushTablesReady.add(cell);
}


function pubRawToBytes(s: string): Uint8Array {
  try { return b64urlDecode(s); } catch { return new Uint8Array(0); }
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
      const privJwk = JSON.parse(privRaw);
      // self-heal a pre-normalization key: the very first keypairs were
      // generated before the SPKI-vs-raw fix, so pubRaw holds 91 bytes the
      // browser rejects at subscribe time. The private JWK carries x/y —
      // rebuild the 65-byte point from them and repair the meta row.
      if (pubRawToBytes(pubRaw).length !== 65 && privJwk.x && privJwk.y) {
        const fixed = b64urlEncode(new Uint8Array([0x04, ...b64urlDecode(privJwk.x), ...b64urlDecode(privJwk.y)]));
        cell.setMeta("push_vapid_pub", fixed);
        cell.addEvent("push.vapid", "repaired public key to raw point form", {});
        return { privJwk, pubRaw: fixed };
      }
      return { privJwk, pubRaw };
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

// ---- secrets: stored wrapped, unwrapped at the door ----

function hostSecretOrThrow(cell) {
  const hs = String(cell.env.FRAGMENT_HOST_SECRET || "");
  if (!hs) {
    // fail loudly: silently serving plaintext secrets (or crashing deep in
    // a run) is the level-(c) bug the wrapping exists to close
    throw new Error("secrets are wrapped at rest but FRAGMENT_HOST_SECRET is not set on this host — set CELLD_VAR_FRAGMENT_HOST_SECRET before using secrets");
  }
  return hs;
}

async function secretsUnwrapped(cell): Promise<Record<string, string>> {
  const npub = cell.getMeta("fragment_npub") || "";
  const hs = hostSecretOrThrow(cell);
  const out = {};
  for (const r of cell.sql.exec("SELECT name, value FROM secrets").toArray()) {
    out[r.name] = await unwrapSecret(hs, npub, r.value);
  }
  return out;
}

// ------ internalRoute ------

export async function internalRoute(cell, request, url) {
  // Two legal spellings: routed via the public router
  // (/__internal/f/<name>/<rest>) or called directly on the DO binding by
  // the native workflow driver (/__internal/<rest>). Normalize to <rest>.
  let pathIsh = url.pathname;
  if (pathIsh.startsWith("/__internal/f/")) {
    const after = pathIsh.slice("/__internal/f/".length);
    pathIsh = "/__internal/" + after.slice(after.indexOf("/") + 1);
  }
  const rest = pathIsh.slice("/__internal/".length);
  // the write plane alone carries the big ceiling; every other internal
  // route keeps the small general body bound
  if (rest === "files/write" ? writeBodyTooLarge(request) : bodyTooLarge(request)) {
    return json({ error: rest === "files/write"
      ? `body too large: workflow writes accept at most ${WRITE_CEILING} bytes per file — split the payload or push big assets with the CLI's direct commit path`
      : `body too large: internal routes accept at most 1 MiB` }, 413);
  }
  let scope = cell.checkToken(request);
  if (!scope && rest.startsWith("egress/")) {
    // apiKey-shaped clients put the run token in the Bearer credential
    const auth = request.headers.get("authorization") || "";
    if (auth.startsWith("Bearer ")) scope = checkTokenRaw(cell, auth.slice(7));
  }
  if (!scope) return json({ error: "bad or expired run token" }, 403);
  const isRun = scope.kind === "run";
  const isWf = scope.kind === "wf-run";

  // ---- native workflow driver routes (wf-engine.ts calls these) ----
  if (rest === "wf/attempt" && request.method === "POST") {
    if (!isWf) return json({ error: "wf/attempt requires a wf-run token" }, 403);
    const { runId, attempt } = await request.json().catch(() => ({}));
    if (Number(runId) !== Number(scope.runId) || Number(attempt) !== Number(scope.attempt)) {
      return json({ error: "token scope mismatch" }, 403);
    }
    const row = cell.sql.exec("SELECT * FROM runs WHERE id = ?", Number(runId) || 0).toArray()[0];
    if (!row) return json({ error: `no such run: ${runId}` }, 404);
    const m = cell.manifest();
    const wf = m && (m.workflows || []).find((w) => w.name === row.wf);
    if (!wf) return json({ error: `workflow ${row.wf} is no longer in the manifest` }, 404);
    return json(await cell.runWorkflowLocked(wf, JSON.parse(row.input || "null"), JSON.parse(row.cause || "null")));
  }
  if (rest === "wf/complete" && request.method === "POST") {
    if (!isWf) return json({ error: "wf/complete requires a wf-run token" }, 403);
    const { runId, attempt, outcome } = await request.json().catch(() => ({}));
    if (Number(runId) !== Number(scope.runId) || Number(attempt) !== Number(scope.attempt)) {
      return json({ error: "token scope mismatch" }, 403);
    }
    if (!outcome || typeof outcome !== "object" || !("ok" in outcome)) {
      return json({ error: "body: {runId, attempt, outcome: {ok, output?|error?}}" }, 400);
    }
    return await cell.applyRunOutcome(runId, attempt, outcome);
  }

  if (rest === "ping") return new Response("pong");

  if (rest === "secrets/all") {
    let out: Record<string, string>;
    try {
      out = await secretsUnwrapped(cell);
    } catch (e) {
      return json({ error: String((e as Error).message || e) }, 500);
    }
    return json(out);
  }

  if (rest === "files/read") {
    const path = url.searchParams.get("path") || "";
    // run scope reads the working copy (main pin); served apps read the
    // working copy too unless the fragment freezes them to the live ref
    // (code frozen at deploy, data flowing live — same promise as before,
    // now over refs: freeze maps main→live)
    const ref = isRun || cell.manifest()?.freeze !== true ? "main" : "live";
    const resp = await readFileStream(cell, path, ref);
    if (resp.status !== 200) return resp;
    return new Response(resp.body, {
      status: resp.status,
      headers: { "content-type": mimeForPath(path) || "application/octet-stream" },
    });
  }

  if (rest === "files/stat") {
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    // read-modify-write loops compare against the path's CONTENT identity
    // now (git blob sha), not a numeric rev — deletion and never-existed
    // unify to {present: false} (git has no tombstones at a ref)
    const st = await statPath(cell, path);
    return json({ stat: st });
  }

  if (rest === "files/write" && request.method === "PUT") {
    if (!isRun) return json({ error: "only workflow runs write files (editors commit directly via code.storage)" }, 403);
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    // bytes buffered ONCE inside this turn (bounded by WRITE_CEILING at
    // the top); the commit-pack writer re-chunks them at 3 MiB
    const bytes = new Uint8Array(await request.arrayBuffer());
    // ifSha: pin the write to the path's current blob sha from stat() —
    // a moved path fails loudly instead of clobbering
    const ifSha = url.searchParams.get("if_sha");
    if (ifSha !== null) {
      const st = await statPath(cell, path);
      const cur = st.present ? st.blobSha : "";
      if (ifSha !== cur) {
        return json({ error: "content conflict", path, currentSha: cur, ifSha }, 409);
      }
    }
    let out;
    try {
      out = await commitPaths(cell, [{ path, bytes }], `workflow write: ${scope.workflow || "run"}`, `wf:${scope.workflow || "?"}`);
    } catch (e) {
      if (e instanceof CodeStorageError) return json({ error: String(e.message || e) }, e.status || 502);
      throw e;
    }
    if (!out.ok) return json({ error: out.error, ...(out.conflict ? { conflict: true } : {}) }, out.status);
    // static site files serve from the LIVE ref — a workflow writing into
    // site/ updates nothing visitors see until the next deploy
    if (path.startsWith("site/")) {
      cell.addEvent("write.warn", `${path}: workflows writing into site/ serve from the live ref — data files belong outside site/`);
    }
    const blobSha = await gitBlobSha(bytes);
    return json({ ok: true, deduped: out.deduped, sha: blobSha, commitSha: out.commitSha });
  }

  if (rest === "files/delete" && request.method === "POST") {
    if (!isRun) return json({ error: "only workflow runs write files" }, 403);
    const { path } = await request.json().catch(() => ({}));
    if (!path || typeof path !== "string" || path.includes("..") || path.startsWith("/")) return json({ error: "body: {path}" }, 400);
    if (appendOnlyHit(cell, path)) return json({ error: `append-only: ${path} refuses deletion` }, 409);
    let out;
    try {
      out = await commitPaths(cell, [{ path, delete: true }], `workflow delete: ${scope.workflow || "run"}`, `wf:${scope.workflow || "?"}`);
    } catch (e) {
      if (e instanceof CodeStorageError) return json({ error: String(e.message || e) }, e.status || 502);
      throw e;
    }
    if (!out.ok) return json({ error: out.error }, out.status);
    return json({ ok: true, deduped: out.deduped, commitSha: out.commitSha });
  }

  if (rest === "files/list") {
    const prefix = url.searchParams.get("prefix") || "";
    const ref = isRun || cell.manifest()?.freeze !== true ? "main" : "live";
    const rows = treeList(cell, ref, prefix);
    return json({
      paths: rows.map((r) => r.path),
      files: rows.map((r) => ({ path: r.path, size: r.size, mode: r.mode, lastCommitSha: r.last_commit_sha })),
    });
  }

  if (rest === "inbox/pending") {
    // claim-on-drain: pending rows flip to claimed under a unique token in
    // the same pass that reads them, so two runs can never see the same
    // message — even when a read trails another run's ack by a write
    // barrier. crypto-random like every other token the runtime mints.
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

  // ------ Keyed egress ------
  // The ONE AI-adjacent surface in the cell — and it is vendor-free: author
  // code (the platform "ai" module, or anything OpenAI-compatible) sends a
  // request to /egress/<host>/<path>; the cell forwards it to
  // https://<host>/<path> IF the host is allowlisted by which provider keys
  // the host holds, attaching the matching key.
  {
    const m = url.pathname.match(/\/egress\/([^/]+)\/(.+)$/);
    if (m && rest.startsWith("egress/")) {
      const host = m[1];
      const falKey = String(cell.env.FAL_API_KEY || "");
      const orKey = String(cell.env.OPENROUTER_API_KEY || "");
      const falBaseHost = (() => { try { return new URL(String(cell.env.FRAGMENT_FAL_BASE || "https://queue.fal.run")).host; } catch { return "queue.fal.run"; } })();
      const keys = {
        [falBaseHost]: falKey ? { header: `Key ${falKey}` } : null,
        "openrouter.ai": orKey ? { header: `Bearer ${orKey}` } : null,
      };
      const cred = keys[host];
      if (!cred) {
        return json({ error: `egress host '${host}' is not allowlisted — the host holds keys for: ${Object.keys(keys).filter((k) => keys[k]).join(", ") || "(none)"}` }, 403);
      }
      const loopback = /^(127\.0\.0\.1|localhost|\[::1\])(:|$)/.test(host);
      const upstream = `http${loopback ? "" : "s"}://${host}/${m[2]}${url.search}`;
      const drop = new Request(upstream, {
        method: request.method,
        headers: {
          "content-type": request.headers.get("content-type") || "application/json",
          authorization: cred.header,
        },
        body: ["GET", "HEAD"].includes(request.method) ? undefined : request.body,
        // @ts-ignore duplex required for streaming bodies
        duplex: "half",
      });
      const upstreamResp = await fetch(drop, { signal: AbortSignal.timeout(120_000) });
      return new Response(upstreamResp.body, {
        status: upstreamResp.status,
        headers: { "content-type": upstreamResp.headers.get("content-type") || "application/json" },
      });
    }
  }

  // ------ Ingest: place remote bytes as a file ------
  // files-plane primitive (not an AI feature): fetch a public URL and
  // commit it at a path (dedup + append-only gates as usual). The ai
  // module composes this for generated media; dropzones and pollers can
  // use it for any remote asset.
  if (rest === "files/ingest" && request.method === "POST") {
    // run scope, plus BLESSED apps: the gen template's /generate route is a
    // served app placing media — the ai module's ingest is its placement
    // primitive, not a general app write path (ctx.files.write stays
    // run-only). Draft previews never load app code (no /d/ anymore), so
    // blessed app scope is exactly the served-app plane.
    const canPlace = isRun || (scope.kind === "draft" && scope.blessed === true);
    if (!canPlace) return json({ error: "ingest is run-scoped (or a blessed app's generate)" }, 403);
    const { url: remoteUrl, path } = await request.json().catch(() => ({}));
    if (typeof remoteUrl !== "string" || !/^https?:\/\//.test(remoteUrl) || remoteUrl.length > 500)
      return json({ error: "body: {url: https://…, path}" }, 400);
    if (!path || typeof path !== "string" || path.includes("..") || path.startsWith("/") || path.length > 200)
      return json({ error: "bad path" }, 400);
    // bounded buffer: same ceiling as writes — media-scale placement
    // belongs to the CLI's direct commit path
    const timed = (p) => Promise.race([p, new Promise((_, rej) => setTimeout(() => rej(new Error("fetch output timed out after 120s")), 120_000))]);
    const remote = await timed(fetch(remoteUrl));
    if (!remote.ok) return json({ error: `fetch output: ${remote.status}` }, 502);
    const bytes = new Uint8Array(await timed(remote.arrayBuffer()));
    if (bytes.byteLength > WRITE_CEILING) {
      return json({ error: `generated output is ${(bytes.byteLength / 1048576).toFixed(1)}MiB — over the ${WRITE_CEILING / 1048576}MiB placement cap; use the CLI commit path for bigger media` }, 413);
    }
    let out;
    try {
      out = await commitPaths(cell, [{ path, bytes }], `ingest from ${String(remoteUrl).slice(0, 80)}`, "wf:ingest");
    } catch (e) {
      if (e instanceof CodeStorageError) return json({ error: String(e.message || e) }, e.status || 502);
      throw e;
    }
    if (!out.ok) return json({ error: out.error }, out.status);
    const blobSha = await gitBlobSha(bytes);
    // sha256 + mime complete the documented media-descriptor contract
    // (fal.mjs surfaces both on generateImage/generateVideo results)
    const digest = await crypto.subtle.digest("SHA-256", bytes as unknown as BufferSource);
    const sha256 = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
    cell.addEvent("file.ingested", `${path} (${(bytes.byteLength / 1024).toFixed(0)}KiB from ${String(remoteUrl).slice(0, 80)})`, { path, sha: blobSha });
    return json({ ok: true, file: { path, sha: blobSha, sha256, size: bytes.byteLength, mime: mimeForPath(path) || "application/octet-stream", url: `__file?path=${encodeURIComponent(path)}` } });
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
    // push service on earth. "ok" is the only sticky state — a FAILED
    // self-test retries next send (self-healing).
    if (cell.getMeta("push_selftest_v2_done") !== "ok") {
      const t = await webpushSelfTest();
      cell.addEvent("push.selftest", t.ok ? "webpush crypto self-test v2 passed" : "webpush crypto self-test FAILED", t);
      if (t.ok) cell.setMeta("push_selftest_v2_done", "ok");
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
        const url = new URL(s.endpoint);
        const vh = await vapidHeaders(keys.privJwk, keys.pubRaw, url.origin);
        const sendWith = (authorization: string) => fetch(s.endpoint, {
          method: "POST",
          headers: {
            "content-type": "application/octet-stream",
            ttl: "86400",
            urgency: "high",
            ...enc.headers,
            authorization,
            "crypto-key": vh["crypto-key"],
          },
          body: enc.body as unknown as BodyInit,
          signal: AbortSignal.timeout(10_000),
        });
        // FCM parses only the raw P1363 JWT signature; RFC 7515 DER is for
        // everyone else. One fallback try on 403 so a service flipping
        // parsers can't kill delivery silently.
        const fcmFirst = url.host === "fcm.googleapis.com";
        let form = fcmFirst ? "raw" : "der";
        let resp = await sendWith(fcmFirst ? vh.authorizationRaw : vh.authorization);
        if (resp.status === 403) {
          form = fcmFirst ? "der" : "raw";
          resp = await sendWith(fcmFirst ? vh.authorization : vh.authorizationRaw);
        }
        if (resp.ok) return { sent: true };
        if (resp.status === 404 || resp.status === 410) {
          cell.sql.exec("DELETE FROM push_subs WHERE endpoint = ?", s.endpoint);
          return { note: `${resp.status} → dropped`, dropped: true };
        }
        const why = (await resp.text().catch(() => "")).slice(0, 120);
        return failNote(cell, s.endpoint, `${resp.status} (sig ${vh.sigShape}, form ${form})${why ? ": " + why : ""}`);
      } catch (e) {
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

