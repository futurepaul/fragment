// Control plane (/api/...): NIP-98-gated fragment management.
import { json, randSlug, randHex, isMachinery, bodyTooLarge, MAX_BODY_BYTES, MIME, mimeForPath } from "./util.js";
import { safeEqual } from "./auth.js";
import { parseCron, nextRun } from "./cron.js";
import { normalizeManifest } from "./manifest.js";
import { recordRevision } from "./history.js";
import { TierError, admitFileWrite, tierStreamByHash } from "./blob-tier.js";

// append-only prefixes from the (already-normalized) manifest
function appendOnlyHit(m, path) {
  return (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}


// ------ apiRoute ------

export async function apiRoute(cell, request, url) {
  const p = url.pathname.slice(4); // after /api
  if (bodyTooLarge(request)) {
    return json({ error: `body too large: cells accept at most ${MAX_BODY_BYTES} bytes per request — keep big assets out of the folder` }, 413);
  }
  const m = cell.manifest();
  if (!m) return json({ error: "fragment not initialized" }, 404);

  // inbox: token-gated, no nostr. The token may arrive as a query param
  // (webhook ergonomics: ?t=...) or, preferably, the x-fragment-inbox-token
  // header for callers who control their clients and don't want the secret
  // in access logs.
  if (p === "/inbox" && request.method === "POST") {
    const presented = request.headers.get("x-fragment-inbox-token") || url.searchParams.get("t") || "";
    if (!safeEqual(presented, cell.getMeta("inbox_token") || "")) return json({ error: "bad inbox token" }, 403);
    // bounded queue: overload is a 429, not memory pressure
    const pending = cell.sql.exec("SELECT COUNT(*) c FROM inbox WHERE status = 'pending'").toArray()[0].c;
    if (pending > 1000) {
      cell.addEvent("queue.rejected", `inbox full (${pending} pending)`, { pending });
      return json({ error: "inbox full — drain pending messages", pending }, 429);
    }
    const body = await request.json().catch(() => ({}));
    const cur = cell.sql.exec("INSERT INTO inbox (at, source, payload) VALUES (?, ?, ?) RETURNING id",
      Date.now(), String(body.source || "external"), JSON.stringify(body.payload ?? null)).toArray()[0];
    cell.addEvent("inbox", `inbox #${cur.id} from ${body.source || "external"}`);
    // the cause chain: cross-fragment hop budget (cycle guard layer 2)
    const cause = {
      origin: request.headers.get("x-fragment-cause") || null,
      depth: parseInt(request.headers.get("x-fragment-hops") || "0", 10) || 0,
      inboxId: cur.id,
    };
    // acknowledge WITHOUT waiting for the workflows: the message is durably
    // in the inbox; the runs are scheduled on the alarm and the failure leg
    // owns retries/holds/dedup. Callers used to pay the whole workflow
    // chain (cold start + nested inbox POSTs = 10s+ drops; found tracing
    // r4-dropzone). The message acks when a run actually drains it.
    const results = [];
    for (const wf of m.workflows || []) {
      if (wf.trigger !== "inbox") continue; // paused is a guard inside executeWorkflow
      const out = await cell.executeWorkflow(wf, { inbox: { id: cur.id, source: body.source, payload: body.payload } }, { auto: true, trigger: "inbox", cause, schedule: true });
      results.push({ workflow: wf.name, scheduled: true });
    }
    return json({ ok: true, id: cur.id, scheduled: results });
  }

  // everything below needs a role
  const authz = (min) => cell.needRole(request, min);
  const deny = (a) => json({ error: a.error }, a.status);

  if (p === "/status" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const files = cell.sql.exec("SELECT COUNT(*) c FROM files WHERE deleted = 0").toArray()[0].c;
    const drafts = cell.sql.exec("SELECT COUNT(*) c FROM drafts").toArray()[0].c;
    const events = cell.sql.exec("SELECT COUNT(*) c FROM events").toArray()[0].c;
    const held = cell.sql.exec("SELECT COUNT(*) c FROM runs WHERE status = 'held'").toArray()[0].c;
    const crons = [];
    const cronState = JSON.parse(cell.getMeta("cron_state") || "{}");
    for (const wf of m.workflows || []) {
      if (!wf.cron) continue;
      try {
        const next = nextRun(wf.cron, cronState[wf.name] ?? Date.now());
        crons.push({ name: wf.name, cron: wf.cron, nextAt: next ? new Date(next).toISOString() : null, paused: !!wf.paused });
      } catch (e) { crons.push({ name: wf.name, cron: wf.cron, error: e.message }); }
    }
    return json({
      name: m.name, npub: cell.getMeta("fragment_npub"), visibility: m.visibility,
      blessed: cell.getMeta("blessed"), counts: { files, drafts, events, held }, crons,
      paused: (m.workflows || []).filter((w) => w.paused).map((w) => w.name),
      viewToken: cell.getMeta("view_token"), inboxToken: cell.getMeta("inbox_token"),
      urls: { canonical: cell.canonicalUrl(url.origin, m.name) },
    });
  }

  if (p === "/manifest" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    return json(m);
  }

  if (p === "/manifest" && request.method === "PUT") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const nm = await request.json().catch(() => null);
    const res = normalizeManifest(nm);
    if (res.error) return json({ error: res.error }, 400);
    res.manifest.name = m.name; // name is immutable
    cell.setMeta("manifest", JSON.stringify(res.manifest));
    cell.addEvent("manifest", "manifest updated");
    await cell.syncRolesToRegistry();
    await cell.rearmAlarm();
    return json({ ok: true });
  }

  if (p === "/manifest/check" && request.method === "POST") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const want = await request.json().catch(() => null);
    const res = normalizeManifest(want);
    if (res.error) return json({ error: res.error }, 400);
    res.manifest.name = m.name;
    // drift check with the server's own normalization, so defaults the
    // runtime adds never count as differences; canonical key order so
    // semantically-equal manifests compare equal
    const canon = (v) => {
      if (Array.isArray(v)) return v.map(canon);
      if (v && typeof v === "object") {
        const out = {};
        for (const k of Object.keys(v).sort()) out[k] = canon(v[k]);
        return out;
      }
      return v;
    };
    // creation seeds a normalized default manifest; until an author
    // manifest-sets something real, that default is "unset" — counting it
    // as drift made every first-sync warn (and guide recipes abort)
    const seeded = canon(normalizeManifest({
      name: m.name, visibility: "link", editors: [], viewers: [], workflows: [], secrets: [],
    }).manifest);
    const unchanged = JSON.stringify(canon(m)) === JSON.stringify(seeded);
    return json({ differs: !unchanged && JSON.stringify(canon(m)) !== JSON.stringify(canon(res.manifest)), wanted: res.manifest });
  }

  if (p === "/pause" && request.method === "POST") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const { workflow, paused } = await request.json().catch(() => ({}));
    const wf = (m.workflows || []).find((w) => w.name === workflow);
    if (!wf) return json({ error: `no such workflow in manifest: ${workflow}` }, 404);
    wf.paused = paused ? true : undefined;
    cell.setMeta("manifest", JSON.stringify(m));
    cell.sql.exec("DELETE FROM meta WHERE k = ?", `wf_breaker_${wf.name}`);
    cell.addEvent(paused ? "workflow.paused" : "workflow.unpaused", `${wf.name}`, { wf: wf.name, by: "manual" });
    await cell.rearmAlarm();
    return json({ ok: true, workflow: wf.name, paused: !!wf.paused });
  }

  // rotate: hard-cut token invalidation for the share link (view_token) and
  // the webhook secret (inbox_token). Older links/webhooks die instantly —
  // accepted product decision (pre-users hard-cut culture).
  if (p === "/rotate" && request.method === "POST") {
    const a = authz("owner"); if (!a.ok) return deny(a);
    const body = await request.json().catch(() => ({}));
    if (body.scopes !== undefined && !Array.isArray(body.scopes)) return json({ error: "unknown scope" }, 400);
    const want = Array.isArray(body.scopes) && body.scopes.length ? body.scopes : ["inbox", "view"];
    for (const s of want) {
      if (s !== "inbox" && s !== "view") return json({ error: "unknown scope" }, 400);
    }
    // same generators minted these at creation (registry.initCell), so the
    // format never drifts; one upsert covers both rows so readers mid-request
    // see pre-rotation or post-rotation tokens, never a half-write
    const nextInbox = want.includes("inbox") ? randHex(16) : (cell.getMeta("inbox_token") || "");
    const nextView = want.includes("view") ? randSlug(12) : (cell.getMeta("view_token") || "");
    cell.sql.exec(
      "INSERT INTO meta (k, v) VALUES ('inbox_token', ?), ('view_token', ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
      nextInbox, nextView);
    cell.addEvent("tokens.rotated", want.join("+"), { scopes: want });
    return json({
      ok: true,
      inbox_token: nextInbox,
      view_token: nextView,
      rotated: ["inbox", "view"].filter((s) => want.includes(s)),
    });
  }

  // rooms inspection: read-side of the rooms plane, gated at editor like the
  // rest of the mutation-heavy API. Listing unions both shapes of room —
  // state-only rows (state:set / rooms.mjs state) and msg-only rows (a plain
  // chat room never touches the rooms table) — so nothing visible in the WS
  // plane is invisible here.
  if (p === "/rooms" && request.method === "GET") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const rooms: Record<string, { room: string; count: number; last_at: number | null }> = {};
    for (const r of cell.sql.exec("SELECT room, COUNT(*) c, MAX(at) la FROM room_msgs GROUP BY room").toArray()) {
      rooms[r.room] = { room: r.room, count: r.c, last_at: r.la ?? null };
    }
    for (const r of cell.sql.exec("SELECT room FROM rooms").toArray()) {
      if (!rooms[r.room]) rooms[r.room] = { room: r.room, count: 0, last_at: null };
    }
    return json({ rooms: Object.values(rooms).sort((x, y) => (y.last_at || 0) - (x.last_at || 0)) });
  }

  // newest page by default; ?before=ID pages older; ALWAYS ascending in the
  // response, so consumers diff/append without flipping (same rule as /events)
  if (p.startsWith("/rooms/") && p.endsWith("/messages") && request.method === "GET") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const mid = p.slice("/rooms/".length, p.length - "/messages".length);
    let room;
    try { room = decodeURIComponent(mid); } catch { return json({ error: "bad room encoding" }, 400); }
    let limit = parseInt(url.searchParams.get("limit") || "100", 10) || 100;
    limit = Math.min(Math.max(limit, 1), 200);
    const before = parseInt(url.searchParams.get("before") || "0", 10);
    // data holds the broadcast frame as stored by rooms.ts (JSON.stringify of
    // whatever the client sent): parse it back; unparseable junk passes raw
    const parseFrame = (s) => { try { return JSON.parse(s); } catch { return s; } };
    const rows = (before > 0
      ? cell.sql.exec("SELECT id, at, sender, data FROM room_msgs WHERE room = ? AND id < ? ORDER BY id DESC LIMIT ?", room, before, limit)
      : cell.sql.exec("SELECT id, at, sender, data FROM room_msgs WHERE room = ? ORDER BY id DESC LIMIT ?", room, limit)
    ).toArray();
    rows.reverse();
    return json({ room, messages: rows.map((r) => ({ id: r.id, at: r.at, sender: r.sender, data: parseFrame(r.data) })) });
  }

  if (p === "/files" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const since = parseInt(url.searchParams.get("since_rev") || "0", 10);
    // names + stored sizes only: the whole payload is O(rows), no bodies
    const rows = cell.sql.exec("SELECT path, rev, sha256, size, deleted FROM files WHERE rev > ? ORDER BY rev", since).toArray();
    return json({ rev: parseInt(cell.getMeta("rev") || "0", 10), files: rows.map((r) => ({ path: r.path, rev: r.rev, size: r.size, sha256: r.sha256, deleted: !!r.deleted, machinery: isMachinery(r.path) })) });
  }

  if (p === "/file" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const row = cell.getFileMeta(path);
    if (!row) return json({ error: "no such file" }, 404);
    // proxy-stream loopback w/o heap buffering; Range rides through
    const upstream = await tierStreamByHash(cell, row.sha256);
    return new Response(upstream.body, { status: upstream.status, headers: {
      "x-fragment-rev": String(row.rev),
      "content-type": row.mime || mimeForPath(path) || "application/octet-stream",
    } });
  }

  if (p === "/file/history" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const rows = cell.sql.exec("SELECT rev, blob_hash, deleted, at FROM file_revisions WHERE path = ? ORDER BY rev DESC", path).toArray();
    return json({ path, revs: rows.map((r) => ({ rev: r.rev, blobHash: r.blob_hash, deleted: !!r.deleted, at: r.at })) });
  }

  if (p === "/file/at" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const rev = parseInt(url.searchParams.get("rev") || "0", 10);
    const row = rev
      ? cell.sql.exec("SELECT blob_hash, deleted FROM file_revisions WHERE path = ? AND rev = ?", path, rev).toArray()[0]
      : cell.sql.exec("SELECT blob_hash, deleted FROM file_revisions WHERE path = ? ORDER BY rev DESC LIMIT 1", path).toArray()[0];
    if (!row) return json({ error: "no such revision (pruned or never existed)" }, 410);
    if (row.deleted) return json({ error: "deleted at that revision" }, 410);
    // revisions are pure hash pointers now — bytes live in the tier forever,
    // so a retained rev always resolves; pruned REV ROWS still answer 410 above
    const upstream = await tierStreamByHash(cell, row.blob_hash);
    return new Response(upstream.body, { status: upstream.status, headers: {
      "x-fragment-rev": String(rev || ""),
      "content-type": "application/octet-stream",
    } });
  }

  // Commit contract (docs/blob-tier.md): EITHER raw body with x-fragment-hash
  // (verify-vs-computed at the tier) OR application/json ref form
  // {"ref":{"sha256","size","mime"}} whose bytes are presumed already in the
  // tier. Raw <=65536 without a hash header is the inline carve-out: the
  // runtime performs the tier upload itself inside this write turn. Every
  // form funnels into the SAME single row-commit below.
  if (p === "/file" && request.method === "PUT") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    const baseRev = parseInt(url.searchParams.get("base_rev") || "0", 10);
    let adm;
    try {
      adm = await admitFileWrite(cell, request, mimeForPath(path) || MIME.txt);
    } catch (e) {
      const status = e instanceof TierError ? e.status : 400;
      return json({ error: String(e.message || e), ...(status === 413 ? { hint: "blob-first" } : {}) }, status);
    }
    const cur = cell.sql.exec("SELECT rev, sha256, deleted FROM files WHERE path = ?", path).toArray()[0];
    const curRev = cur ? cur.rev : 0;
    // append-only: an existing path may be re-sent identically (idempotent)
    // but not modified except by the owner; new paths are always fine —
    // these verdicts run on ROW data before/without byte transport
    if (cur && !cur.deleted && appendOnlyHit(m, path) && cur.sha256 !== adm.effSha && a.role !== "owner") {
      return json({ error: "append-only", path }, 409);
    }
    if (cur && !cur.deleted && appendOnlyHit(m, path) && cur.sha256 === adm.effSha) {
      return json({ path, rev: curRev, noop: true });
    }
    if (baseRev !== curRev) return json({ error: "conflict", currentRev: curRev }, 409);
    let desc;
    try {
      desc = await adm.place();
    } catch (e) {
      const status = e instanceof TierError ? e.status : 502;
      return json({ error: String(e.message || e) }, status);
    }
    // paired assertions: identity survives placement; no dangling names
    if (desc.sha256 !== adm.effSha) {
      return json({ error: `hash mismatch: tier received ${desc.sha256}, caller declared ${adm.effSha}` }, 400);
    }
    if (!(desc.sha256 && Number.isSafeInteger(desc.size))) {
      return json({ error: "tier descriptor incomplete — refusing to commit a dangling name" }, 502);
    }
    // invariant: bytes are IN the tier before the name commits — a crashed
    // placement leaves the old row untouched, never a broken pointer
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    cell.sql.exec("INSERT INTO files (path, sha256, size, mime, rev, updated_at, deleted) VALUES (?, ?, ?, ?, ?, ?, 0) ON CONFLICT(path) DO UPDATE SET sha256 = excluded.sha256, size = excluded.size, mime = excluded.mime, rev = excluded.rev, updated_at = excluded.updated_at, deleted = 0",
      path, desc.sha256, desc.size, desc.mime, newRev, Date.now());
    await recordRevision(cell, path, newRev, desc.sha256);
    await cell.scheduleSyncTrigger(path);
    return json({ path, rev: newRev });
  }

  if (p === "/file" && request.method === "DELETE") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    if (appendOnlyHit(m, path) && a.role !== "owner") return json({ error: "append-only", path }, 403);
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    // tombstone row: empty hash marks the deletion (no body column to blank)
    cell.sql.exec("INSERT INTO files (path, sha256, size, mime, rev, updated_at, deleted) VALUES (?, '', 0, '', ?, ?, 1) ON CONFLICT(path) DO UPDATE SET sha256 = '', size = 0, mime = '', rev = excluded.rev, updated_at = excluded.updated_at, deleted = 1",
      path, newRev, Date.now());
    await recordRevision(cell, path, newRev, null, true);
    await cell.scheduleSyncTrigger(path);
    return json({ ok: true, rev: newRev });
  }

  if (p === "/drafts" && request.method === "POST") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const body = await request.json().catch(() => ({}));
    const slug = randSlug(8);
    // paths only — servability verdicts need names, never bodies
    const rows = cell.sql.exec("SELECT path FROM files WHERE deleted = 0").toArray();
    // servability check: a draft with no app.mjs and no site/ files will
    // 404 at every URL. The #1 cause is syncing the PARENT folder, so
    // every path gains a spurious prefix — name the suspect when visible.
    const servable = rows.some((r) => r.path === "app.mjs" || r.path.startsWith("site/"));
    const nested = rows.length && !servable && rows.every((r) => r.path.startsWith(m.name + "/"));
    if (!servable) {
      const hint = nested
        ? ` every file is under "${m.name}/" — you probably synced the PARENT folder; sync the folder that CONTAINS site/ (or app.mjs)`
        : " no app.mjs and no site/ files — the canonical URL will 404";
      cell.addEvent("publish.warn", `draft ${slug}:${hint}`);
    }
    cell.sql.exec("INSERT INTO drafts (slug, at, note) VALUES (?, ?, ?)", slug, Date.now(), String(body.note || ""));
    // Draft snapshot = pure row copies referencing byte hashes — NO byte
    // storage is touched. Invariant that ended the 513MB wedge class: the
    // snapshot cost is O(rows) metadata copied in one SQL statement; bodies
    // already sit in the blob tier addressed by sha256 and are shared
    // between working tree and every snapshot (dedupe by construction).
    // Before the two-tier split this loop copied every file BLOB into
    // draft_files, inflating SQLite/LTX state by the whole corpus size.
    cell.sql.exec("INSERT INTO draft_files (slug, path, sha256, size, mime, rev, updated_at) SELECT ?, path, sha256, size, mime, rev, updated_at FROM files WHERE deleted = 0", slug);
    await cell.env.FRAGMENT.getByName("_registry").fetch("http://x/__registry/slug-map", {
      method: "POST", body: JSON.stringify({ slug, name: m.name }),
    });
    cell.addEvent("draft", `draft ${slug} published (${rows.length} files)${body.note ? ": " + body.note : ""}`);
    return json({ slug, url: `/d/${slug}/`, servable, ...(servable ? {} : { warning: nested ? "all files under <name>/ — synced the parent folder?" : "no app.mjs or site/ — this draft will 404" }) });
  }

  if (p === "/drafts" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const rows = cell.sql.exec("SELECT slug, at, note, blessed FROM drafts ORDER BY at DESC").toArray();
    return json({ drafts: rows });
  }

  if (p === "/bless" && request.method === "POST") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const { slug } = await request.json().catch(() => ({}));
    const d = cell.sql.exec("SELECT slug FROM drafts WHERE slug = ?", slug || "").toArray()[0];
    if (!d) return json({ error: "no such draft" }, 404);
    cell.sql.exec("UPDATE drafts SET blessed = 0");
    cell.sql.exec("UPDATE drafts SET blessed = 1 WHERE slug = ?", slug);
    cell.setMeta("blessed", slug);
    // a new blessing supersedes the old canonical app's token
    for (const r of cell.sql.exec("SELECT token, scope FROM run_tokens").toArray()) {
      try {
        const s = JSON.parse(r.scope);
        if (s.kind === "draft" && s.blessed && s.slug !== slug) {
          cell.sql.exec("DELETE FROM run_tokens WHERE token = ?", r.token);
        }
      } catch {}
    }
    cell.addEvent("bless", `blessed ${slug}`);
    return json({ ok: true, url: cell.canonicalUrl(url.origin, m.name) });
  }

  if (p === "/run" && request.method === "POST") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const { workflow, input } = await request.json().catch(() => ({}));
    const wf = (m.workflows || []).find((w) => w.name === workflow);
    if (!wf) return json({ error: `no such workflow in manifest: ${workflow}` }, 404);
    const before = cell.sql.exec("SELECT COALESCE(MAX(id), 0) m FROM events").toArray()[0].m;
    const out = await cell.executeWorkflow(wf, input ?? null, { trigger: "manual" });
    const evs = cell.sql.exec("SELECT id, at, kind, summary FROM events WHERE id > ? ORDER BY id", before).toArray();
    return json({ ok: !!out.ok, output: out.output ?? null, error: out.error ?? null, runId: out.runId ?? null, events: evs });
  }

  if (p === "/replay" && request.method === "POST") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const { run: runId } = await request.json().catch(() => ({}));
    const row = cell.sql.exec("SELECT * FROM runs WHERE id = ?", Number(runId) || 0).toArray()[0];
    if (!row) return json({ error: `no such run: ${runId}` }, 404);
    const wf = (m.workflows || []).find((w) => w.name === row.wf);
    if (!wf) return json({ error: `workflow ${row.wf} is no longer in the manifest` }, 404);
    let cause = { origin: null, depth: 0 };
    try { cause = { ...cause, ...JSON.parse(row.cause || "{}") }; } catch {}
    const out = await cell.executeWorkflow(wf, JSON.parse(row.input || "null"), { trigger: "replay", cause: { ...cause, depth: 0, replayOf: row.id } });
    if (out.ok) cell.sql.exec("UPDATE runs SET status = 'replayed' WHERE id = ? AND status = 'held'", row.id);
    return json({ ok: !!out.ok, output: out.output ?? null, error: out.error ?? null, runId: out.runId ?? null });
  }

  if (p === "/runs" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const status = url.searchParams.get("status");
    const wfName = url.searchParams.get("wf");
    const limit = Math.min(parseInt(url.searchParams.get("limit") || "50", 10) || 50, 500);
    const conds = [];
    const vals = [];
    if (status) { conds.push("status = ?"); vals.push(status); }
    if (wfName) { conds.push("wf = ?"); vals.push(wfName); }
    const where = conds.length ? "WHERE " + conds.join(" AND ") : "";
    const rows = cell.sql.exec(`SELECT * FROM runs ${where} ORDER BY id DESC LIMIT ?`, ...vals, limit).toArray();
    const counts = {};
    for (const r of cell.sql.exec("SELECT status, COUNT(*) c FROM runs GROUP BY status").toArray()) counts[r.status] = r.c;
    return json({
      runs: rows.map((r) => ({
        id: r.id, wf: r.wf, via: r.via, status: r.status,
        attempt: r.attempt, maxAttempts: r.max_attempts,
        error: r.error || null,
        startedAt: r.started_at, finishedAt: r.finished_at, durationMs: r.duration_ms,
        nextAttemptAt: r.next_attempt_at,
        input: url.searchParams.get("include") === "input" ? JSON.parse(r.input || "null") : undefined,
        cause: r.cause ? JSON.parse(r.cause) : null,
      })),
      counts,
    });
  }

  if (p === "/events" && request.method === "GET") {
    const a = authz("viewer"); if (!a.ok) return deny(a);
    const since = parseInt(url.searchParams.get("since") || "0", 10);
    // newest window, oldest-first delivery: an ascending LIMIT 500 pins the
    // response to the FIRST 500 rows, so every cell with a longer history
    // reads as frozen at [500] (relay-vault, r2-news in prod) while inserts
    // continue. Select the newest 500 after the cursor DESC, then flip to
    // the ascending order the CLI's --tail and since-cursor expect.
    const rows = cell.sql.exec("SELECT id, at, kind, summary, data FROM events WHERE id > ? ORDER BY id DESC LIMIT 500", since).toArray();
    rows.reverse();
    return json({ events: rows });
  }

  if (p === "/events" && request.method === "POST") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const { kind, summary, data } = await request.json().catch(() => ({}));
    cell.addEvent(String(kind || "note"), String(summary || ""), data);
    return json({ ok: true });
  }

  if (p.startsWith("/secrets/") && request.method === "PUT") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const key = decodeURIComponent(p.slice("/secrets/".length));
    if (!/^[A-Z][A-Z0-9_]*$/.test(key)) return json({ error: "secret names: UPPER_SNAKE" }, 400);
    const value = await request.text();
    cell.sql.exec("INSERT INTO secrets (name, value) VALUES (?, ?) ON CONFLICT(name) DO UPDATE SET value = excluded.value", key, value);
    cell.addEvent("secret", `secret ${key} set`);
    return json({ ok: true });
  }
  if (p === "/secrets" && request.method === "GET") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const rows = cell.sql.exec("SELECT name FROM secrets ORDER BY name").toArray();
    return json({ names: rows.map((r) => r.name) });
  }
  if (p.startsWith("/secrets/") && request.method === "DELETE") {
    const a = authz("editor"); if (!a.ok) return deny(a);
    const key = decodeURIComponent(p.slice("/secrets/".length));
    cell.sql.exec("DELETE FROM secrets WHERE name = ?", key);
    return json({ ok: true });
  }

  return new Response("not found", { status: 404 });
}
