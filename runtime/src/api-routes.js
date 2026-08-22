// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { json, toAB, randSlug } from "./util.js";
import { sha256Hex } from "./auth.js";
import { nextRun } from "./cron.js";
async function apiRoute(cell, request, url) {
  const p = url.pathname.slice(4);
  const m = cell.manifest();
  if (!m) return json({ error: "fragment not initialized" }, 404);
  if (p === "/inbox" && request.method === "POST") {
    if (url.searchParams.get("t") !== cell.getMeta("inbox_token")) return json({ error: "bad inbox token" }, 403);
    const body = await request.json().catch(() => ({}));
    const cur = cell.sql.exec(
      "INSERT INTO inbox (at, source, payload) VALUES (?, ?, ?) RETURNING id",
      Date.now(),
      String(body.source || "external"),
      JSON.stringify(body.payload ?? null)
    ).toArray()[0];
    cell.addEvent("inbox", `inbox #${cur.id} from ${body.source || "external"}`);
    const results = [];
    for (const wf of m.workflows || []) {
      if (wf.trigger !== "inbox" || wf.paused) continue;
      const out = await cell.runWorkflow(wf, { inbox: { id: cur.id, source: body.source, payload: body.payload } }, { auto: true });
      results.push({ workflow: wf.name, ok: !!out.ok });
      if (out.ok) cell.sql.exec("UPDATE inbox SET status = 'done' WHERE id = ?", cur.id);
    }
    return json({ ok: true, id: cur.id, ran: results });
  }
  const authz = (min) => cell.needRole(request, min);
  const deny = (a) => json({ error: a.error }, a.status);
  if (p === "/status" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const files = cell.sql.exec("SELECT COUNT(*) c FROM files WHERE deleted = 0").toArray()[0].c;
    const drafts = cell.sql.exec("SELECT COUNT(*) c FROM drafts").toArray()[0].c;
    const events = cell.sql.exec("SELECT COUNT(*) c FROM events").toArray()[0].c;
    const crons = [];
    const cronState = JSON.parse(cell.getMeta("cron_state") || "{}");
    for (const wf of m.workflows || []) {
      if (!wf.cron || wf.paused) continue;
      try {
        const next = nextRun(wf.cron, cronState[wf.name] ?? Date.now());
        crons.push({ name: wf.name, cron: wf.cron, nextAt: next ? new Date(next).toISOString() : null });
      } catch (e) {
        crons.push({ name: wf.name, cron: wf.cron, error: e.message });
      }
    }
    return json({
      name: m.name,
      npub: cell.getMeta("fragment_npub"),
      visibility: m.visibility,
      blessed: cell.getMeta("blessed"),
      counts: { files, drafts, events },
      crons,
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
    const err = cell.validateManifest(nm);
    if (err) return json({ error: err }, 400);
    nm.name = m.name;
    cell.setMeta("manifest", JSON.stringify(nm));
    cell.addEvent("manifest", "manifest updated");
    await cell.syncRolesToRegistry();
    await cell.rearmAlarm();
    return json({ ok: true });
  }
  if (p === "/files" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const since = parseInt(url.searchParams.get("since_rev") || "0", 10);
    const rows = cell.sql.exec(
      "SELECT path, rev, sha256, deleted, length(content) AS size FROM files WHERE rev > ? ORDER BY rev",
      since
    ).toArray();
    return json({ rev: parseInt(cell.getMeta("rev") || "0", 10), files: rows.map((r) => ({ path: r.path, rev: r.rev, size: r.size, sha256: r.sha256, deleted: !!r.deleted })) });
  }
  if (p === "/file" && request.method === "GET") {
    const a = authz("viewer");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const row = cell.sql.exec("SELECT content, rev FROM files WHERE path = ? AND deleted = 0", path).toArray()[0];
    if (!row) return json({ error: "no such file" }, 404);
    return new Response(toAB(row.content), { headers: { "x-fragment-rev": String(row.rev) } });
  }
  if (p === "/file" && request.method === "PUT") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
    const baseRev = parseInt(url.searchParams.get("base_rev") || "0", 10);
    const cur = cell.sql.exec("SELECT rev FROM files WHERE path = ?", path).toArray()[0];
    const curRev = cur ? cur.rev : 0;
    if (baseRev !== curRev) return json({ error: "conflict", currentRev: curRev }, 409);
    const body = await request.arrayBuffer();
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    const sha = await sha256Hex(body);
    cell.sql.exec(
      "INSERT INTO files (path, content, rev, sha256, updated_at, deleted) VALUES (?, ?, ?, ?, ?, 0) ON CONFLICT(path) DO UPDATE SET content = excluded.content, rev = excluded.rev, sha256 = excluded.sha256, updated_at = excluded.updated_at, deleted = 0",
      path,
      body,
      newRev,
      sha,
      Date.now()
    );
    await cell.scheduleSyncTrigger(path);
    return json({ path, rev: newRev });
  }
  if (p === "/file" && request.method === "DELETE") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const path = url.searchParams.get("path") || "";
    const newRev = parseInt(cell.getMeta("rev") || "0", 10) + 1;
    cell.setMeta("rev", String(newRev));
    cell.sql.exec(
      "INSERT INTO files (path, content, rev, sha256, updated_at, deleted) VALUES (?, X'', ?, ?, ?, 1) ON CONFLICT(path) DO UPDATE SET content = X'', rev = excluded.rev, sha256 = NULL, updated_at = excluded.updated_at, deleted = 1",
      path,
      newRev,
      null,
      Date.now()
    );
    await cell.scheduleSyncTrigger(path);
    return json({ ok: true, rev: newRev });
  }
  if (p === "/drafts" && request.method === "POST") {
    const a = authz("editor");
    if (!a.ok) return deny(a);
    const body = await request.json().catch(() => ({}));
    const slug = randSlug(8);
    const rows = cell.sql.exec("SELECT path, content, sha256 FROM files WHERE deleted = 0").toArray();
    cell.sql.exec("INSERT INTO drafts (slug, at, note) VALUES (?, ?, ?)", slug, Date.now(), String(body.note || ""));
    for (const r of rows) {
      cell.sql.exec("INSERT INTO draft_files (slug, path, content, sha256) VALUES (?, ?, ?, ?)", slug, r.path, toAB(r.content), r.sha256);
    }
    await cell.env.FRAGMENT.getByName("_registry").fetch("http://x/__registry/slug-map", {
      method: "POST",
      body: JSON.stringify({ slug, name: m.name })
    });
    cell.addEvent("draft", `draft ${slug} published (${rows.length} files)${body.note ? ": " + body.note : ""}`);
    return json({ slug, url: `/d/${slug}/` });
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
    const out = await cell.runWorkflow(wf, input ?? null);
    const evs = cell.sql.exec("SELECT id, at, kind, summary FROM events WHERE id > ? ORDER BY id", before).toArray();
    return json({ ok: !!out.ok, output: out.output ?? null, error: out.error ?? null, events: evs });
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
