// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { json, toAB, randSlug, isMachinery } from "./util.js";
import { sha256Hex, safeEqual } from "./auth.js";
import { nextRun } from "./cron.js";
import { normalizeManifest } from "./manifest.js";
import { recordRevision } from "./history.js";
function appendOnlyHit(m, path) {
  return (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}
async function apiRoute(cell, request, url) {
  const p = url.pathname.slice(4);
  const m = cell.manifest();
  if (!m) return json({ error: "fragment not initialized" }, 404);
  if (p === "/inbox" && request.method === "POST") {
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
      const out = await cell.executeWorkflow(wf, { inbox: { id: cur.id, source: body.source, payload: body.payload } }, { auto: true, trigger: "inbox", cause, schedule: true });
      results.push({ workflow: wf.name, scheduled: true });
    }
    return json({ ok: true, id: cur.id, scheduled: results });
  }
  const authz = (min) => cell.needRole(request, min);
  const deny = (a) => json({ error: a.error }, a.status);
  if (p === "/status" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
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
      } catch (e) {
        crons.push({ name: wf.name, cron: wf.cron, error: e.message });
      }
    }
    return json({
      name: m.name,
      npub: cell.getMeta("fragment_npub"),
      visibility: m.visibility,
      blessed: cell.getMeta("blessed"),
      counts: { files, drafts, events, held },
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
  if (p === "/manifest" && request.method === "PUT") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const nm = await request.json().catch(() => null);
    const res = normalizeManifest(nm);
    if (res.error) return json({ error: res.error }, 400);
    res.manifest.name = m.name;
    cell.setMeta("manifest", JSON.stringify(res.manifest));
    cell.addEvent("manifest", "manifest updated");
    await cell.syncRolesToRegistry();
    await cell.rearmAlarm();
    return json({ ok: true });
  }
  if (p === "/manifest/check" && request.method === "POST") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const want = await request.json().catch(() => null);
    const res = normalizeManifest(want);
    if (res.error) return json({ error: res.error }, 400);
    res.manifest.name = m.name;
    const canon = (v) => {
      if (Array.isArray(v)) return v.map(canon);
      if (v && typeof v === "object") {
        const out = {};
        for (const k of Object.keys(v).sort()) out[k] = canon(v[k]);
        return out;
      }
      return v;
    };
    return json({ differs: JSON.stringify(canon(m)) !== JSON.stringify(canon(res.manifest)), wanted: res.manifest });
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
  if (p === "/files" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const since = parseInt(url.searchParams.get("since_rev") || "0", 10);
    const rows = cell.sql.exec(
      "SELECT path, rev, sha256, deleted, length(content) AS size FROM files WHERE rev > ? ORDER BY rev",
      since
    ).toArray();
    return json({ rev: parseInt(cell.getMeta("rev") || "0", 10), files: rows.map((r) => ({ path: r.path, rev: r.rev, size: r.size, sha256: r.sha256, deleted: !!r.deleted, machinery: isMachinery(r.path) })) });
  }
  if (p === "/file" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const row = cell.sql.exec("SELECT content, rev FROM files WHERE path = ? AND deleted = 0", path).toArray()[0];
    if (!row) return json({ error: "no such file" }, 404);
    return new Response(toAB(row.content), { headers: { "x-fragment-rev": String(row.rev) } });
  }
  if (p === "/file/history" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const rows = cell.sql.exec("SELECT rev, blob_hash, deleted, at FROM file_revisions WHERE path = ? ORDER BY rev DESC", path).toArray();
    return json({ path, revs: rows.map((r) => ({ rev: r.rev, blobHash: r.blob_hash, deleted: !!r.deleted, at: r.at })) });
  }
  if (p === "/file/at" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const rev = parseInt(url.searchParams.get("rev") || "0", 10);
    const row = rev ? cell.sql.exec("SELECT blob_hash, deleted FROM file_revisions WHERE path = ? AND rev = ?", path, rev).toArray()[0] : cell.sql.exec("SELECT blob_hash, deleted FROM file_revisions WHERE path = ? ORDER BY rev DESC LIMIT 1", path).toArray()[0];
    if (!row) return json({ error: "no such revision (pruned or never existed)" }, 410);
    if (row.deleted) return json({ error: "deleted at that revision" }, 410);
    const blob = cell.sql.exec("SELECT content FROM blobs WHERE hash = ?", row.blob_hash).toArray()[0];
    if (!blob) return json({ error: "revision pruned" }, 410);
    return new Response(toAB(blob.content), { headers: { "x-fragment-rev": String(rev || "") } });
  }
  if (p === "/file" && request.method === "PUT") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    const baseRev = parseInt(url.searchParams.get("base_rev") || "0", 10);
    const cur = cell.sql.exec("SELECT rev, sha256, deleted FROM files WHERE path = ?", path).toArray()[0];
    const curRev = cur ? cur.rev : 0;
    const body = await request.arrayBuffer();
    const sha = await sha256Hex(body);
    if (cur && !cur.deleted && appendOnlyHit(m, path) && cur.sha256 !== sha && a.role !== "owner") {
      return json({ error: "append-only", path }, 409);
    }
    if (cur && !cur.deleted && appendOnlyHit(m, path) && cur.sha256 === sha) {
      return json({ path, rev: curRev, noop: true });
    }
    if (baseRev !== curRev) return json({ error: "conflict", currentRev: curRev }, 409);
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    cell.sql.exec(
      "INSERT INTO files (path, content, rev, sha256, updated_at, deleted) VALUES (?, ?, ?, ?, ?, 0) ON CONFLICT(path) DO UPDATE SET content = excluded.content, rev = excluded.rev, sha256 = excluded.sha256, updated_at = excluded.updated_at, deleted = 0",
      path,
      body,
      newRev,
      sha,
      Date.now()
    );
    await recordRevision(cell, path, newRev, sha, body);
    await cell.scheduleSyncTrigger(path);
    return json({ path, rev: newRev });
  }
  if (p === "/file" && request.method === "DELETE") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    if (appendOnlyHit(m, path) && a.role !== "owner") return json({ error: "append-only", path }, 403);
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    cell.sql.exec(
      "INSERT INTO files (path, content, rev, sha256, updated_at, deleted) VALUES (?, X'', ?, ?, ?, 1) ON CONFLICT(path) DO UPDATE SET content = X'', rev = excluded.rev, sha256 = NULL, updated_at = excluded.updated_at, deleted = 1",
      path,
      newRev,
      null,
      Date.now()
    );
    await recordRevision(cell, path, newRev, null, null, true);
    await cell.scheduleSyncTrigger(path);
    return json({ ok: true, rev: newRev });
  }
  if (p === "/drafts" && request.method === "POST") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const body = await request.json().catch(() => ({}));
    const slug = randSlug(8);
    const rows = cell.sql.exec("SELECT path, content, sha256 FROM files WHERE deleted = 0").toArray();
    const servable = rows.some((r) => r.path === "app.mjs" || r.path.startsWith("site/"));
    const nested = rows.length && !servable && rows.every((r) => r.path.startsWith(m.name + "/"));
    if (!servable) {
      const hint = nested ? ` every file is under "${m.name}/" \u2014 you probably synced the PARENT folder; sync the folder that CONTAINS site/ (or app.mjs)` : " no app.mjs and no site/ files \u2014 the canonical URL will 404";
      cell.addEvent("publish.warn", `draft ${slug}:${hint}`);
    }
    cell.sql.exec("INSERT INTO drafts (slug, at, note) VALUES (?, ?, ?)", slug, Date.now(), String(body.note || ""));
    for (const r of rows) {
      cell.sql.exec("INSERT INTO draft_files (slug, path, content, sha256) VALUES (?, ?, ?, ?)", slug, r.path, toAB(r.content), r.sha256);
    }
    await cell.env.FRAGMENT.getByName("_registry").fetch("http://x/__registry/slug-map", {
      method: "POST",
      body: JSON.stringify({ slug, name: m.name })
    });
    cell.addEvent("draft", `draft ${slug} published (${rows.length} files)${body.note ? ": " + body.note : ""}`);
    return json({ slug, url: `/d/${slug}/`, servable, ...servable ? {} : { warning: nested ? "all files under <name>/ \u2014 synced the parent folder?" : "no app.mjs or site/ \u2014 this draft will 404" } });
  }
  if (p === "/drafts" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const rows = cell.sql.exec("SELECT slug, at, note, blessed FROM drafts ORDER BY at DESC").toArray();
    return json({ drafts: rows });
  }
  if (p === "/bless" && request.method === "POST") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const { slug } = await request.json().catch(() => ({}));
    const d = cell.sql.exec("SELECT slug FROM drafts WHERE slug = ?", slug || "").toArray()[0];
    if (!d) return json({ error: "no such draft" }, 404);
    cell.sql.exec("UPDATE drafts SET blessed = 0");
    cell.sql.exec("UPDATE drafts SET blessed = 1 WHERE slug = ?", slug);
    cell.setMeta("blessed", slug);
    for (const r of cell.sql.exec("SELECT token, scope FROM run_tokens").toArray()) {
      try {
        const s = JSON.parse(r.scope);
        if (s.kind === "draft" && s.blessed && s.slug !== slug) {
          cell.sql.exec("DELETE FROM run_tokens WHERE token = ?", r.token);
        }
      } catch {
      }
    }
    cell.addEvent("bless", `blessed ${slug}`);
    return json({ ok: true, url: cell.canonicalUrl(url.origin, m.name) });
  }
  if (p === "/run" && request.method === "POST") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const { workflow, input } = await request.json().catch(() => ({}));
    const wf = (m.workflows || []).find((w) => w.name === workflow);
    if (!wf) return json({ error: `no such workflow in manifest: ${workflow}` }, 404);
    const before = cell.sql.exec("SELECT COALESCE(MAX(id), 0) m FROM events").toArray()[0].m;
    const out = await cell.executeWorkflow(wf, input ?? null, { trigger: "manual" });
    const evs = cell.sql.exec("SELECT id, at, kind, summary FROM events WHERE id > ? ORDER BY id", before).toArray();
    return json({ ok: !!out.ok, output: out.output ?? null, error: out.error ?? null, runId: out.runId ?? null, events: evs });
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
    if (out.ok) cell.sql.exec("UPDATE runs SET status = 'replayed' WHERE id = ? AND status = 'held'", row.id);
    return json({ ok: !!out.ok, output: out.output ?? null, error: out.error ?? null, runId: out.runId ?? null });
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
    const rows = cell.sql.exec("SELECT id, at, kind, summary, data FROM events WHERE id > ? ORDER BY id LIMIT 500", since).toArray();
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
    cell.sql.exec("INSERT INTO secrets (name, value) VALUES (?, ?) ON CONFLICT(name) DO UPDATE SET value = excluded.value", key, value);
    cell.addEvent("secret", `secret ${key} set`);
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
  return new Response("not found", { status: 404 });
}
export {
  apiRoute
};
