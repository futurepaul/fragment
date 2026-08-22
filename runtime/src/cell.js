// FragmentCell — one Durable Object per fragment (plus one named "_registry").
// All fragment state lives in this cell's SQLite. See ARCHITECTURE.md.
import { schnorr } from "@noble/curves/secp256k1.js";
import { npubFromHex, hexFromNpub } from "./bech32.js";
import { sha256Hex } from "./auth.js";
import { parseCron, nextRun, cronMatches } from "./cron.js";
import { CTX_SHIM_SOURCE } from "./ctx-shim.js";

const NAME_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
const SLUG_CHARS = "abcdefghijklmnopqrstuvwxyz0123456789";

const SCHEMA = `
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, content BLOB, rev INTEGER, sha256 TEXT, updated_at INTEGER, deleted INTEGER DEFAULT 0);
CREATE TABLE IF NOT EXISTS drafts (slug TEXT PRIMARY KEY, at INTEGER, note TEXT, blessed INTEGER DEFAULT 0);
CREATE TABLE IF NOT EXISTS draft_files (slug TEXT, path TEXT, content BLOB, sha256 TEXT, PRIMARY KEY (slug, path));
CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS inbox (id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER, source TEXT, payload TEXT, status TEXT DEFAULT 'pending');
CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER, kind TEXT, summary TEXT, data TEXT);
CREATE TABLE IF NOT EXISTS wstate (scope TEXT, k TEXT, v TEXT, PRIMARY KEY (scope, k));
CREATE TABLE IF NOT EXISTS rooms (room TEXT PRIMARY KEY, state TEXT);
CREATE TABLE IF NOT EXISTS room_msgs (id INTEGER PRIMARY KEY AUTOINCREMENT, room TEXT, at INTEGER, sender TEXT, data TEXT);
CREATE TABLE IF NOT EXISTS run_tokens (token TEXT PRIMARY KEY, scope TEXT, expires INTEGER);
CREATE TABLE IF NOT EXISTS fragments (name TEXT PRIMARY KEY, owner TEXT, created_at INTEGER);
CREATE TABLE IF NOT EXISTS roles (name TEXT, pubkey TEXT, role TEXT, PRIMARY KEY (name, pubkey));
CREATE TABLE IF NOT EXISTS slugs (slug TEXT PRIMARY KEY, name TEXT);
`;

const MIME = {
  html: "text/html; charset=utf-8", htm: "text/html; charset=utf-8",
  css: "text/css; charset=utf-8", js: "text/javascript; charset=utf-8",
  mjs: "text/javascript; charset=utf-8", json: "application/json",
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
  svg: "image/svg+xml", ico: "image/x-icon", webp: "image/webp",
  txt: "text/plain; charset=utf-8", md: "text/markdown; charset=utf-8",
  wasm: "application/wasm", pdf: "application/pdf", xml: "application/xml",
  csv: "text/csv; charset=utf-8", woff2: "font/woff2",
};

const WORKFLOW_MAIN = `
import { makeCtx } from "./fragment-ctx.mjs";
import { run } from "./__WF__";
export default {
  async fetch(req, env) {
    let input = null; try { input = await req.json(); } catch {}
    let ctx;
    try { ctx = await makeCtx(env); } catch (e) {
      return Response.json({ ok: false, error: "ctx init: " + String(e) });
    }
    try {
      const output = await run(ctx, input);
      return Response.json({ ok: true, output: output ?? null });
    } catch (e) {
      return Response.json({ ok: false, error: String((e && e.stack) || e) });
    }
  }
}
`;

const APP_MAIN = `
import { makeCtx } from "./fragment-ctx.mjs";
import app from "./app.mjs";
export default {
  async fetch(req, env) {
    const ctx = await makeCtx(env);
    return app.fetch(req, ctx);
  }
}
`;

const ROOMS_MAIN = `
import { makeCtx } from "./fragment-ctx.mjs";
import { onMessage } from "./rooms.mjs";
export default {
  async fetch(req, env) {
    const { room, msg } = await req.json();
    let out = {};
    try {
      const ctx = await makeCtx(env);
      out = (await onMessage(room, msg, ctx)) ?? {};
    } catch (e) {
      return Response.json({ error: String((e && e.stack) || e) });
    }
    return Response.json(out);
  }
}
`;

function randHex(bytes) {
  const b = new Uint8Array(bytes);
  crypto.getRandomValues(b);
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

function randSlug(n = 8) {
  let s = "";
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  for (const x of b) s += SLUG_CHARS[x % SLUG_CHARS.length];
  return s;
}

function toAB(x) {
  if (x == null) return new ArrayBuffer(0);
  if (x instanceof ArrayBuffer) return x;
  if (x instanceof Uint8Array) return x.buffer.slice(x.byteOffset, x.byteOffset + x.byteLength);
  if (typeof x === "string") return new TextEncoder().encode(x).buffer;
  throw new Error("unexpected storage value type: " + typeof x);
}

function json(data, status = 200, headers = {}) {
  return new Response(JSON.stringify(data), { status, headers: { "content-type": "application/json", ...headers } });
}

export class FragmentCell {
  constructor(state, env) {
    this.state = state;
    this.env = env;
    this.sql = state.storage.sql;
    this.sql.exec(SCHEMA);
  }

  // ---------- meta helpers ----------
  getMeta(k) {
    const row = this.sql.exec("SELECT v FROM meta WHERE k = ?", k).toArray()[0];
    return row ? row.v : null;
  }
  setMeta(k, v) {
    this.sql.exec("INSERT INTO meta (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v", k, String(v));
  }
  manifest() {
    const raw = this.getMeta("manifest");
    return raw ? JSON.parse(raw) : null;
  }
  addEvent(kind, summary, data) {
    this.sql.exec("INSERT INTO events (at, kind, summary, data) VALUES (?, ?, ?, ?)",
      Date.now(), kind, String(summary).slice(0, 500), data === undefined ? null : JSON.stringify(data).slice(0, 4000));
  }

  // ---------- roles ----------
  roleOf(pubkeyHex) {
    // pubkeyHex may be null (unauthenticated)
    const m = this.manifest();
    if (!m) return null;
    if (pubkeyHex && pubkeyHex === this.getMeta("owner")) return "owner";
    if (pubkeyHex) {
      let npub;
      try { npub = npubFromHex(pubkeyHex); } catch { return null; }
      if ((m.editors || []).includes(npub)) return "editor";
      if ((m.viewers || []).includes(npub)) return "viewer";
    }
    return null;
  }
  static rank(r) { return { owner: 3, editor: 2, viewer: 1 }[r] || 0; }
  needRole(req, min) {
    const pk = req.headers.get("x-fragment-pubkey");
    const role = this.roleOf(pk);
    if (FragmentCell.rank(role) >= FragmentCell.rank(min)) return { ok: true, role, pubkey: pk };
    return { ok: false, status: pk ? 403 : 401, error: pk ? `requires ${min} role` : "unauthenticated" };
  }

  // ---------- main dispatch ----------
  async fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    try {
      if (path.startsWith("/__registry/")) return await this.registryRoute(request, url);
      if (path === "/__cell/init") return await this.initCell(request);
      if (path.startsWith("/__internal/")) return await this.internalRoute(request, url);
      if (path.startsWith("/api/")) return await this.apiRoute(request, url);
      if (path.startsWith("/__serve/")) return await this.serveRoute(request, url);
      if (path.startsWith("/__room/")) return await this.roomRoute(request, url);
      return new Response("not found", { status: 404 });
    } catch (e) {
      return json({ error: String((e && e.stack) || e) }, 500);
    }
  }

  // ---------- init ----------
  async initCell(request) {
    if (this.getMeta("name")) return json({ ok: true, already: true });
    const { name, ownerHex } = await request.json();
    const secretKey = randHex(32);
    const pubHex = [...schnorr.getPublicKey(Uint8Array.from(secretKey.match(/.{2}/g).map((b) => parseInt(b, 16))))].map((b) => b.toString(16).padStart(2, "0")).join("");
    this.setMeta("name", name);
    this.setMeta("owner", ownerHex);
    this.setMeta("fragment_secret", secretKey);
    this.setMeta("fragment_npub", npubFromHex(pubHex));
    this.setMeta("view_token", randSlug(12));
    this.setMeta("inbox_token", randHex(16));
    this.setMeta("rev", "0");
    this.setMeta("manifest", JSON.stringify({
      name, visibility: "token", editors: [], viewers: [], workflows: [], secrets: [],
    }));
    this.addEvent("create", `fragment ${name} created`);
    return json({ ok: true, npub: this.getMeta("fragment_npub"), viewToken: this.getMeta("view_token"), inboxToken: this.getMeta("inbox_token") });
  }

  // ---------- registry (only the _registry cell gets these routes) ----------
  async registryRoute(request, url) {
    const p = url.pathname;
    if (p === "/__registry/create" && request.method === "POST") {
      const { name, ownerHex } = await request.json();
      if (!NAME_RE.test(name) || name.startsWith("_")) return json({ error: "bad name (lowercase, digits, dashes; 2-32 chars; no leading _)" }, 400);
      const exists = this.sql.exec("SELECT name FROM fragments WHERE name = ?", name).toArray()[0];
      if (exists) return json({ error: `name taken: ${name}` }, 409);
      this.sql.exec("INSERT INTO fragments (name, owner, created_at) VALUES (?, ?, ?)", name, ownerHex, Date.now());
      this.sql.exec("INSERT INTO roles (name, pubkey, role) VALUES (?, ?, 'owner')", name, ownerHex);
      return json({ ok: true });
    }
    if (p === "/__registry/list") {
      const pk = url.searchParams.get("pubkey");
      const rows = this.sql.exec(
        "SELECT r.name, r.role, f.created_at FROM roles r JOIN fragments f ON f.name = r.name WHERE r.pubkey = ?", pk || "").toArray();
      return json({ fragments: rows.map((r) => ({ name: r.name, role: r.role })) });
    }
    if (p === "/__registry/slug-map" && request.method === "POST") {
      const { slug, name } = await request.json();
      this.sql.exec("INSERT INTO slugs (slug, name) VALUES (?, ?) ON CONFLICT(slug) DO NOTHING", slug, name);
      return json({ ok: true });
    }
    if (p === "/__registry/slug") {
      const slug = url.searchParams.get("s");
      const row = this.sql.exec("SELECT name FROM slugs WHERE slug = ?", slug || "").toArray()[0];
      return row ? json({ name: row.name }) : json({ error: "unknown draft" }, 404);
    }
    if (p === "/__registry/roles-sync" && request.method === "POST") {
      const { name, owner, editors, viewers } = await request.json();
      this.sql.exec("DELETE FROM roles WHERE name = ?", name);
      const add = (npub, role) => {
        try { this.sql.exec("INSERT INTO roles (name, pubkey, role) VALUES (?, ?, ?) ON CONFLICT DO NOTHING", name, hexFromNpub(npub), role); } catch {}
      };
      try { this.sql.exec("INSERT INTO roles (name, pubkey, role) VALUES (?, ?, 'owner') ON CONFLICT DO NOTHING", name, owner); } catch {}
      (editors || []).forEach((n) => add(n, "editor"));
      (viewers || []).forEach((n) => add(n, "viewer"));
      return json({ ok: true });
    }
    return new Response("not found", { status: 404 });
  }

  async syncRolesToRegistry() {
    const m = this.manifest();
    await this.env.FRAGMENT.getByName("_registry").fetch("http://x/__registry/roles-sync", {
      method: "POST",
      body: JSON.stringify({ name: m.name, owner: this.getMeta("owner"), editors: m.editors, viewers: m.viewers }),
    });
  }

  // canonical URL for this fragment: subdomain form on hosts that declare a
  // wildcard host (FRAGMENT_SUBDOMAIN_HOST), /f/<name>/ path form elsewhere
  canonicalUrl(origin, name) {
    const sub = this.env.FRAGMENT_SUBDOMAIN_HOST;
    if (sub) return `https://${encodeURIComponent(name)}.${sub}/`;
    return `${origin}/f/${name}/`;
  }

  // ---------- control API (/api/...) ----------
  async apiRoute(request, url) {
    const p = url.pathname.slice(4); // after /api
    const m = this.manifest();
    if (!m) return json({ error: "fragment not initialized" }, 404);

    // inbox: token-gated, no nostr
    if (p === "/inbox" && request.method === "POST") {
      if (url.searchParams.get("t") !== this.getMeta("inbox_token")) return json({ error: "bad inbox token" }, 403);
      const body = await request.json().catch(() => ({}));
      const cur = this.sql.exec("INSERT INTO inbox (at, source, payload) VALUES (?, ?, ?) RETURNING id",
        Date.now(), String(body.source || "external"), JSON.stringify(body.payload ?? null)).toArray()[0];
      this.addEvent("inbox", `inbox #${cur.id} from ${body.source || "external"}`);
      const results = [];
      for (const wf of m.workflows || []) {
        if (wf.trigger !== "inbox") continue;
        const out = await this.runWorkflow(wf, { inbox: { id: cur.id, source: body.source, payload: body.payload } });
        results.push({ workflow: wf.name, ok: !!out.ok });
        if (out.ok) this.sql.exec("UPDATE inbox SET status = 'done' WHERE id = ?", cur.id);
      }
      return json({ ok: true, id: cur.id, ran: results });
    }

    // everything below needs a role
    const authz = (min) => this.needRole(request, min);
    const deny = (a) => json({ error: a.error }, a.status);

    if (p === "/status" && request.method === "GET") {
      const a = authz("viewer"); if (!a.ok) return deny(a);
      const files = this.sql.exec("SELECT COUNT(*) c FROM files WHERE deleted = 0").toArray()[0].c;
      const drafts = this.sql.exec("SELECT COUNT(*) c FROM drafts").toArray()[0].c;
      const events = this.sql.exec("SELECT COUNT(*) c FROM events").toArray()[0].c;
      const crons = [];
      const cronState = JSON.parse(this.getMeta("cron_state") || "{}");
      for (const wf of m.workflows || []) {
        if (!wf.cron) continue;
        try {
          const next = nextRun(wf.cron, cronState[wf.name] ?? Date.now());
          crons.push({ name: wf.name, cron: wf.cron, nextAt: next ? new Date(next).toISOString() : null });
        } catch (e) { crons.push({ name: wf.name, cron: wf.cron, error: e.message }); }
      }
      return json({
        name: m.name, npub: this.getMeta("fragment_npub"), visibility: m.visibility,
        blessed: this.getMeta("blessed"), counts: { files, drafts, events }, crons,
        viewToken: this.getMeta("view_token"), inboxToken: this.getMeta("inbox_token"),
        urls: { canonical: this.canonicalUrl(url.origin, m.name) },
      });
    }

    if (p === "/manifest" && request.method === "GET") {
      const a = authz("viewer"); if (!a.ok) return deny(a);
      return json(m);
    }

    if (p === "/manifest" && request.method === "PUT") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const nm = await request.json().catch(() => null);
      const err = this.validateManifest(nm);
      if (err) return json({ error: err }, 400);
      nm.name = m.name; // name is immutable
      this.setMeta("manifest", JSON.stringify(nm));
      this.addEvent("manifest", "manifest updated");
      await this.syncRolesToRegistry();
      await this.rearmAlarm();
      return json({ ok: true });
    }

    if (p === "/files" && request.method === "GET") {
      const a = authz("viewer"); if (!a.ok) return deny(a);
      const since = parseInt(url.searchParams.get("since_rev") || "0", 10);
      const rows = this.sql.exec(
        "SELECT path, rev, sha256, deleted, length(content) AS size FROM files WHERE rev > ? ORDER BY rev", since).toArray();
      return json({ rev: parseInt(this.getMeta("rev") || "0", 10), files: rows.map((r) => ({ path: r.path, rev: r.rev, size: r.size, sha256: r.sha256, deleted: !!r.deleted })) });
    }

    if (p === "/file" && request.method === "GET") {
      const a = authz("viewer"); if (!a.ok) return deny(a);
      const path = url.searchParams.get("path") || "";
      const row = this.sql.exec("SELECT content, rev FROM files WHERE path = ? AND deleted = 0", path).toArray()[0];
      if (!row) return json({ error: "no such file" }, 404);
      return new Response(toAB(row.content), { headers: { "x-fragment-rev": String(row.rev) } });
    }

    if (p === "/file" && request.method === "PUT") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const path = url.searchParams.get("path") || "";
      if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
      const baseRev = parseInt(url.searchParams.get("base_rev") || "0", 10);
      const cur = this.sql.exec("SELECT rev FROM files WHERE path = ?", path).toArray()[0];
      const curRev = cur ? cur.rev : 0;
      if (baseRev !== curRev) return json({ error: "conflict", currentRev: curRev }, 409);
      const body = await request.arrayBuffer();
      const newRev = parseInt(this.getMeta("rev") || "0", 10) + 1;
      this.setMeta("rev", String(newRev));
      const sha = await sha256Hex(body);
      this.sql.exec("INSERT INTO files (path, content, rev, sha256, updated_at, deleted) VALUES (?, ?, ?, ?, ?, 0) ON CONFLICT(path) DO UPDATE SET content = excluded.content, rev = excluded.rev, sha256 = excluded.sha256, updated_at = excluded.updated_at, deleted = 0",
        path, body, newRev, sha, Date.now());
      await this.scheduleSyncTrigger(path);
      return json({ path, rev: newRev });
    }

    if (p === "/file" && request.method === "DELETE") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const path = url.searchParams.get("path") || "";
      const newRev = parseInt(this.getMeta("rev") || "0", 10) + 1;
      this.setMeta("rev", String(newRev));
      this.sql.exec("INSERT INTO files (path, content, rev, sha256, updated_at, deleted) VALUES (?, X'', ?, ?, ?, 1) ON CONFLICT(path) DO UPDATE SET content = X'', rev = excluded.rev, sha256 = NULL, updated_at = excluded.updated_at, deleted = 1",
        path, newRev, null, Date.now());
      await this.scheduleSyncTrigger(path);
      return json({ ok: true, rev: newRev });
    }

    if (p === "/drafts" && request.method === "POST") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const body = await request.json().catch(() => ({}));
      const slug = randSlug(8);
      const rows = this.sql.exec("SELECT path, content, sha256 FROM files WHERE deleted = 0").toArray();
      this.sql.exec("INSERT INTO drafts (slug, at, note) VALUES (?, ?, ?)", slug, Date.now(), String(body.note || ""));
      for (const r of rows) {
        this.sql.exec("INSERT INTO draft_files (slug, path, content, sha256) VALUES (?, ?, ?, ?)", slug, r.path, toAB(r.content), r.sha256);
      }
      await this.env.FRAGMENT.getByName("_registry").fetch("http://x/__registry/slug-map", {
        method: "POST", body: JSON.stringify({ slug, name: m.name }),
      });
      this.addEvent("draft", `draft ${slug} published (${rows.length} files)${body.note ? ": " + body.note : ""}`);
      return json({ slug, url: `/d/${slug}/` });
    }

    if (p === "/drafts" && request.method === "GET") {
      const a = authz("viewer"); if (!a.ok) return deny(a);
      const rows = this.sql.exec("SELECT slug, at, note, blessed FROM drafts ORDER BY at DESC").toArray();
      return json({ drafts: rows });
    }

    if (p === "/bless" && request.method === "POST") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const { slug } = await request.json().catch(() => ({}));
      const d = this.sql.exec("SELECT slug FROM drafts WHERE slug = ?", slug || "").toArray()[0];
      if (!d) return json({ error: "no such draft" }, 404);
      this.sql.exec("UPDATE drafts SET blessed = 0");
      this.sql.exec("UPDATE drafts SET blessed = 1 WHERE slug = ?", slug);
      this.setMeta("blessed", slug);
      // a new blessing supersedes the old canonical app's token
      for (const r of this.sql.exec("SELECT token, scope FROM run_tokens").toArray()) {
        try {
          const s = JSON.parse(r.scope);
          if (s.kind === "draft" && s.blessed && s.slug !== slug) {
            this.sql.exec("DELETE FROM run_tokens WHERE token = ?", r.token);
          }
        } catch {}
      }
      this.addEvent("bless", `blessed ${slug}`);
      return json({ ok: true, url: this.canonicalUrl(url.origin, m.name) });
    }

    if (p === "/run" && request.method === "POST") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const { workflow, input } = await request.json().catch(() => ({}));
      const wf = (m.workflows || []).find((w) => w.name === workflow);
      if (!wf) return json({ error: `no such workflow in manifest: ${workflow}` }, 404);
      const before = this.sql.exec("SELECT COALESCE(MAX(id), 0) m FROM events").toArray()[0].m;
      const out = await this.runWorkflow(wf, input ?? null);
      const evs = this.sql.exec("SELECT id, at, kind, summary FROM events WHERE id > ? ORDER BY id", before).toArray();
      return json({ ok: !!out.ok, output: out.output ?? null, error: out.error ?? null, events: evs });
    }

    if (p === "/events" && request.method === "GET") {
      const a = authz("viewer"); if (!a.ok) return deny(a);
      const since = parseInt(url.searchParams.get("since") || "0", 10);
      const rows = this.sql.exec("SELECT id, at, kind, summary, data FROM events WHERE id > ? ORDER BY id LIMIT 500", since).toArray();
      return json({ events: rows });
    }

    if (p === "/events" && request.method === "POST") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const { kind, summary, data } = await request.json().catch(() => ({}));
      this.addEvent(String(kind || "note"), String(summary || ""), data);
      return json({ ok: true });
    }

    if (p.startsWith("/secrets/") && request.method === "PUT") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const key = decodeURIComponent(p.slice("/secrets/".length));
      if (!/^[A-Z][A-Z0-9_]*$/.test(key)) return json({ error: "secret names: UPPER_SNAKE" }, 400);
      const value = await request.text();
      this.sql.exec("INSERT INTO secrets (name, value) VALUES (?, ?) ON CONFLICT(name) DO UPDATE SET value = excluded.value", key, value);
      this.addEvent("secret", `secret ${key} set`);
      return json({ ok: true });
    }
    if (p === "/secrets" && request.method === "GET") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const rows = this.sql.exec("SELECT name FROM secrets ORDER BY name").toArray();
      return json({ names: rows.map((r) => r.name) });
    }
    if (p.startsWith("/secrets/") && request.method === "DELETE") {
      const a = authz("editor"); if (!a.ok) return deny(a);
      const key = decodeURIComponent(p.slice("/secrets/".length));
      this.sql.exec("DELETE FROM secrets WHERE name = ?", key);
      return json({ ok: true });
    }

    return new Response("not found", { status: 404 });
  }

  validateManifest(m) {
    if (!m || typeof m !== "object") return "manifest must be a JSON object";
    if (!["public", "viewers", "token"].includes(m.visibility)) return "visibility must be public|viewers|token";
    for (const k of ["editors", "viewers"]) {
      if (!Array.isArray(m[k])) return `${k} must be an array of npubs`;
      for (const n of m[k]) { try { hexFromNpub(n); } catch { return `bad npub in ${k}: ${n}`; } }
    }
    if (!Array.isArray(m.workflows)) return "workflows must be an array";
    for (const wf of m.workflows) {
      if (!wf.name || !wf.file) return "each workflow needs name + file";
      // neither cron nor trigger = manual-only workflow (fragment run)
      if (wf.cron) { try { parseCron(wf.cron); } catch (e) { return `workflow ${wf.name}: ${e.message}`; } }
      if (wf.trigger && wf.trigger !== "inbox" && wf.trigger !== "sync") return `workflow ${wf.name}: unknown trigger ${wf.trigger}`;
    }
    if (!Array.isArray(m.secrets)) return "secrets must be an array of names";
    return null;
  }

  // ---------- files helpers ----------
  getFileRow(path) {
    return this.sql.exec("SELECT content, rev, sha256 FROM files WHERE path = ? AND deleted = 0", path).toArray()[0] || null;
  }
  getFileText(path) {
    const row = this.getFileRow(path);
    return row ? new TextDecoder().decode(toAB(row.content)) : null;
  }

  // ---------- alarms / cron ----------
  async rearmAlarm() {
    const m = this.manifest();
    if (!m) return;
    const cronState = JSON.parse(this.getMeta("cron_state") || "{}");
    let next = null;
    for (const wf of m.workflows || []) {
      if (!wf.cron) continue;
      try {
        const t = nextRun(wf.cron, cronState[wf.name] ?? Date.now());
        if (t !== null && (next === null || t < next)) next = t;
      } catch {}
    }
    // the pending sync trigger (if any) competes with cron for the one alarm
    const syncAt = parseInt(this.getMeta("sync_trigger_at") || "0", 10);
    if (syncAt && (next === null || syncAt < next)) next = syncAt;
    if (next !== null) await this.state.storage.setAlarm(next);
    else await this.state.storage.deleteAlarm();
  }

  async alarm() {
    const m = this.manifest();
    if (!m) return;
    await this.fireSyncTriggers(m);
    const cronState = JSON.parse(this.getMeta("cron_state") || "{}");
    const now = Date.now();
    for (const wf of m.workflows || []) {
      if (!wf.cron) continue;
      let parsed;
      try { parsed = parseCron(wf.cron); } catch { continue; }
      const last = cronState[wf.name];
      let dueAt = null;
      if (last === undefined) {
        if (cronMatches(parsed, new Date(now))) dueAt = Math.floor(now / 60000) * 60000;
      } else {
        const t = nextRun(parsed, last);
        if (t !== null && t <= now) dueAt = t;
      }
      if (dueAt !== null) {
        await this.runWorkflow(wf, { cron: wf.cron, scheduledTime: dueAt });
        cronState[wf.name] = dueAt;
        this.setMeta("cron_state", JSON.stringify(cronState));
      }
    }
    await this.rearmAlarm();
  }

  // A file changed on the editor plane (sync push, CLI write). Coalesces a
  // burst of writes into one trigger run a few seconds out, using the same
  // single alarm slot cron uses. Workflow-plane writes (run scope) do NOT
  // schedule triggers — an ingest workflow's output must not re-trigger it.
  async scheduleSyncTrigger(path) {
    const m = this.manifest();
    if (!m || !(m.workflows || []).some((wf) => wf.trigger === "sync")) return;
    const dirty = new Set(JSON.parse(this.getMeta("sync_dirty_paths") || "[]"));
    dirty.add(path);
    this.setMeta("sync_dirty_paths", JSON.stringify([...dirty].slice(-500)));
    if (!parseInt(this.getMeta("sync_trigger_at") || "0", 10)) {
      this.setMeta("sync_trigger_at", String(Date.now() + 4000));
      await this.rearmAlarm();
    }
  }

  async fireSyncTriggers(m) {
    const at = parseInt(this.getMeta("sync_trigger_at") || "0", 10);
    if (!at) return;
    this.setMeta("sync_trigger_at", "");
    const paths = JSON.parse(this.getMeta("sync_dirty_paths") || "[]");
    this.setMeta("sync_dirty_paths", "[]");
    for (const wf of m.workflows || []) {
      if (wf.trigger !== "sync") continue;
      await this.runWorkflow(wf, { sync: { paths, at } });
    }
  }

  // ---------- workflow execution (Worker Loader) ----------
  makeToken(scope) {
    const token = randHex(16);
    // token lifetime = the lifetime of the thing it authenticates:
    // workflow runs are short (1h is a safety net for wedged runs), draft
    // previews are rehearsals (1h), a blessed app lives until superseded
    // (null expiry; revoked at rebless, swept on restart re-mint).
    const ttl = scope.kind === "run" || scope.kind === "draft" ? (scope.blessed ? null : 3600_000) : 3600_000;
    this.sql.exec("DELETE FROM run_tokens WHERE expires < ?", Date.now() - 24 * 3600_000);
    if (scope.kind === "draft") {
      // one live token per (slug, blessed-ness): re-mints sweep their predecessor
      for (const r of this.sql.exec("SELECT token, scope FROM run_tokens").toArray()) {
        try {
          const s = JSON.parse(r.scope);
          if (s.kind === "draft" && s.slug === scope.slug && !!s.blessed === !!scope.blessed) {
            this.sql.exec("DELETE FROM run_tokens WHERE token = ?", r.token);
          }
        } catch {}
      }
    }
    this.sql.exec("INSERT INTO run_tokens (token, scope, expires) VALUES (?, ?, ?)", token, JSON.stringify(scope), ttl === null ? null : Date.now() + ttl);
    return token;
  }

  checkToken(request, url) {
    const token = request.headers.get("x-fragment-token") || url.searchParams.get("t") || "";
    const row = this.sql.exec("SELECT scope, expires FROM run_tokens WHERE token = ?", token).toArray()[0];
    if (!row) return null;
    if (row.expires !== null && row.expires !== undefined && row.expires < Date.now()) return null;
    return JSON.parse(row.scope);
  }

  internalBase() {
    const base = this.env.FRAGMENT_INTERNAL_URL || "http://127.0.0.1:8789";
    return `${base}/__internal/f/${this.getMeta("name")}`;
  }

  async loadCode(id, mainSource, modules, scope) {
    // raw = { "main.mjs": mainSource, "fragment-ctx.mjs": CTX_SHIM_SOURCE, ...modules };
    // Host divergence: celld's loader wants plain-string modules (and accepts
    // .mjs names); CF's loader requires .js/.py names for strings, so on CF
    // we wrap every module as {js: source} to keep our .mjs names legal.
    const wrapped = {};
    for (const [k, v] of Object.entries({ "main.mjs": mainSource, "fragment-ctx.mjs": CTX_SHIM_SOURCE, ...modules })) {
      wrapped[k] = this.env.FRAGMENT_HOST_KIND === "cf" ? { js: v } : v;
    }
    // the token is minted ONLY when the worker is actually created: the
    // loader caches workers by id, and a cached worker keeps whatever token
    // its env was born with. Minting per-request (as this used to) orphaned
    // the live worker's token — every ctx call 403'd once the original
    // expired (or was swept).
    const worker = await this.env.LOADER.get(id, async () => ({
      compatibilityDate: "2026-01-01",
      mainModule: "main.mjs",
      modules: wrapped,
      env: {
        FRAGMENT_INTERNAL_URL: this.internalBase(),
        FRAGMENT_RUN_TOKEN: this.makeToken(scope),
        FRAGMENT_SCOPE: scope.kind,
      },
    }));
    return worker.getEntrypoint ? worker.getEntrypoint() : worker;
  }

  collectModules(prefix) {
    const rows = this.sql.exec("SELECT path, content FROM files WHERE path LIKE ? AND deleted = 0", prefix + "%").toArray();
    const modules = {};
    for (const r of rows) modules[r.path] = new TextDecoder().decode(toAB(r.content));
    return modules;
  }

  async runWorkflow(wf, input) {
    const name = this.getMeta("name");
    this.addEvent("workflow-start", `${wf.name}`);
    try {
      const src = this.getFileText(wf.file);
      if (src === null) throw new Error(`workflow file not found in folder: ${wf.file}`);
      const modules = this.collectModules("workflows/");
      const rev = this.getMeta("rev");
      const ep = await this.loadCode(
        `wf:${name}:${wf.name}:${rev}`,
        WORKFLOW_MAIN.replaceAll("__WF__", wf.file),
        modules,
        { kind: "run", workflow: wf.name },
      );
      // body IS the workflow input (WORKFLOW_MAIN passes req.json() to run)
      const resp = await ep.fetch("http://loaded/run", { method: "POST", body: JSON.stringify(input ?? null) });
      const out = await resp.json();
      if (out.ok) this.addEvent("workflow-ok", `${wf.name}`, out.output !== null && out.output !== undefined ? { output: out.output } : undefined);
      else this.addEvent("workflow-error", `${wf.name}: ${out.error}`);
      return out;
    } catch (e) {
      this.addEvent("workflow-error", `${wf.name}: ${String(e)}`);
      return { ok: false, error: String(e) };
    }
  }

  // ---------- internal loopback API (/__internal/f/<name>/...) ----------
  async internalRoute(request, url) {
    const p = url.pathname.slice("/__internal/f/".length);
    const slash = p.indexOf("/");
    const rest = p.slice(slash + 1);
    const scope = this.checkToken(request, url);
    if (!scope) return json({ error: "bad or expired run token" }, 403);
    const isRun = scope.kind === "run";

    if (rest === "ping") return new Response("pong");

    if (rest === "secrets/all") {
      const rows = this.sql.exec("SELECT name, value FROM secrets").toArray();
      const out = {};
      for (const r of rows) out[r.name] = r.value;
      return json(out);
    }

    if (rest === "files/read") {
      const path = url.searchParams.get("path") || "";
      // run scope, or a served app on a liveFiles fragment, reads the working
      // copy: code stays frozen in the blessed draft, data flows live.
      if (isRun || this.manifest()?.liveFiles === true) {
        const row = this.getFileRow(path);
        if (!row) return json({ error: "no such file" }, 404);
        return new Response(toAB(row.content));
      }
      // draft scope: read from the draft snapshot
      const row = this.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = ?", scope.slug, path).toArray()[0];
      if (!row) return json({ error: "no such file in draft" }, 404);
      return new Response(toAB(row.content));
    }

    if (rest === "files/write" && request.method === "PUT") {
      if (!isRun) return json({ error: "drafts are immutable" }, 403);
      const path = url.searchParams.get("path") || "";
      if (!path || path.includes("..") || path.startsWith("/")) return json({ error: "bad path" }, 400);
      const body = await request.arrayBuffer();
      const newRev = parseInt(this.getMeta("rev") || "0", 10) + 1;
      this.setMeta("rev", String(newRev));
      const sha = await sha256Hex(body);
      this.sql.exec("INSERT INTO files (path, content, rev, sha256, updated_at, deleted) VALUES (?, ?, ?, ?, ?, 0) ON CONFLICT(path) DO UPDATE SET content = excluded.content, rev = excluded.rev, sha256 = excluded.sha256, updated_at = excluded.updated_at, deleted = 0",
        path, body, newRev, sha, Date.now());
      return json({ ok: true, rev: newRev });
    }

    if (rest === "files/list") {
      const prefix = url.searchParams.get("prefix") || "";
      const live = isRun || this.manifest()?.liveFiles === true;
      const rows = live
        ? this.sql.exec("SELECT path, length(content) AS size, updated_at, rev FROM files WHERE path LIKE ? AND deleted = 0 ORDER BY path", prefix + "%").toArray()
        : this.sql.exec("SELECT path, length(content) AS size, 0 AS updated_at, 0 AS rev FROM draft_files WHERE slug = ? AND path LIKE ? ORDER BY path", scope.slug, prefix + "%").toArray();
      return json({
        paths: rows.map((r) => r.path),
        files: rows.map((r) => ({ path: r.path, size: r.size, updatedAt: r.updated_at || null, rev: r.rev || 0 })),
      });
    }

    if (rest === "inbox/pending") {
      const rows = this.sql.exec("SELECT id, at, source, payload FROM inbox WHERE status = 'pending' ORDER BY id LIMIT 100").toArray();
      return json({ messages: rows.map((r) => ({ id: r.id, at: r.at, source: r.source, payload: JSON.parse(r.payload) })) });
    }

    if (rest === "events" && request.method === "POST") {
      const { kind, summary, data } = await request.json().catch(() => ({}));
      this.addEvent(String(kind || "run"), String(summary || ""), data);
      return json({ ok: true });
    }

    if (rest === "log" && request.method === "POST") {
      const { msg } = await request.json().catch(() => ({}));
      this.addEvent("log", `[${scope.workflow || scope.kind}] ${msg}`);
      return json({ ok: true });
    }

    if (rest === "infer" && request.method === "POST") {
      const key = this.env.OPENROUTER_API_KEY;
      if (!key) return json({ error: "host has no OPENROUTER_API_KEY (set CELLD_VAR_OPENROUTER_API_KEY on the node)" }, 501);
      const { prompt, model } = await request.json().catch(() => ({}));
      const resp = await fetch("https://openrouter.ai/api/v1/chat/completions", {
        method: "POST",
        headers: { "content-type": "application/json", authorization: `Bearer ${key}` },
        body: JSON.stringify({
          model: model || this.env.FRAGMENT_AI_MODEL || "deepseek/deepseek-v4-flash-0731",
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
        const row = this.sql.exec("SELECT v FROM wstate WHERE scope = ? AND k = ?", dim, k).toArray()[0];
        return json({ v: row ? JSON.parse(row.v) : null });
      }
      if (request.method === "PUT") {
        const { v } = await request.json().catch(() => ({}));
        this.sql.exec("INSERT INTO wstate (scope, k, v) VALUES (?, ?, ?) ON CONFLICT(scope, k) DO UPDATE SET v = excluded.v", dim, k, JSON.stringify(v ?? null));
        return json({ ok: true });
      }
    }

    if (rest === "rooms/state") {
      const room = url.searchParams.get("room") || "";
      if (request.method === "GET") {
        const row = this.sql.exec("SELECT state FROM rooms WHERE room = ?", room).toArray()[0];
        return json({ state: row ? JSON.parse(row.state) : null });
      }
      if (request.method === "POST") {
        const { value } = await request.json().catch(() => ({}));
        this.sql.exec("INSERT INTO rooms (room, state) VALUES (?, ?) ON CONFLICT(room) DO UPDATE SET state = excluded.state", room, JSON.stringify(value ?? null));
        this.broadcast(room, JSON.stringify({ type: "state", value: value ?? null }));
        return json({ ok: true });
      }
    }

    return new Response("not found", { status: 404 });
  }

  // ---------- site serving (/__serve/...) ----------
  async serveRoute(request, url) {
    // /__serve/b/<rest>  → blessed draft; /__serve/d/<slug>/<rest>
    const parts = url.pathname.slice("/__serve/".length).split("/");
    const mode = parts.shift();
    let slug;
    let setCookie = null;
    if (mode === "b") {
      const m = this.manifest();
      slug = this.getMeta("blessed");
      if (!slug) return new Response(`fragment "${m?.name}" has no blessed draft yet — publish and bless one first.\n`, { status: 404 });
      const vis = this.checkVisibility(request, url);
      if (!vis.ok) return new Response(vis.error + "\n", { status: vis.status });
      setCookie = vis.setCookie;
    } else {
      slug = parts.shift(); // drafts are unguessable-slug public
    }
    const stamp = (r) => {
      if (!setCookie) return r;
      const h = new Headers(r.headers);
      h.append("set-cookie", setCookie);
      return new Response(r.body, { status: r.status, headers: h });
    };
    const rest = parts.join("/");
    const draft = this.sql.exec("SELECT slug FROM drafts WHERE slug = ?", slug).toArray()[0];
    if (!draft) return new Response("no such draft\n", { status: 404 });

    // dynamic app
    const appRow = this.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = 'app.mjs'", slug).toArray()[0];
    if (appRow) {
      const modules = {};
      const libRows = this.sql.exec("SELECT path, content FROM draft_files WHERE slug = ? AND path LIKE 'applib/%'", slug).toArray();
      for (const r of libRows) modules[r.path] = new TextDecoder().decode(toAB(r.content));
      modules["app.mjs"] = new TextDecoder().decode(toAB(appRow.content));
      const ep = await this.loadCode(`app:${slug}`, APP_MAIN, modules, { kind: "draft", slug, blessed: mode === "b" });
      const appUrl = new URL(request.url);
      return stamp(await ep.fetch(new Request(appUrl.origin + "/" + rest + appUrl.search, request)));
    }

    // static from site/
    let rel = rest === "" ? "index.html" : rest;
    let row = this.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = ?", slug, "site/" + rel).toArray()[0];
    if (!row && !rel.endsWith("/")) row = this.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = ?", slug, "site/" + rel + "/index.html").toArray()[0];
    if (!row) return new Response("not found", { status: 404 });
    const ext = (rel.match(/\.([a-z0-9]+)$/) || [])[1] || "";
    const cache = mode === "b" ? "no-store" : "public, max-age=3600, immutable";
    return stamp(new Response(toAB(row.content), { headers: { "content-type": MIME[ext] || "application/octet-stream", "cache-control": cache } }));
  }

  checkVisibility(request, url) {
    const m = this.manifest();
    if (m.visibility === "public") return { ok: true };
    // a valid ?view= upgrade: browsers don't attach query strings to
    // subresource fetches (module imports, css, img), so mint a scoped
    // cookie on the first token hit and accept it thereafter.
    const token = this.getMeta("view_token");
    const ck = `fragview_${m.name}`;
    const cookies = Object.fromEntries(
      (request.headers.get("cookie") || "").split(";").map((c) => c.split("=").map((s) => s.trim())).filter((p) => p.length === 2)
    );
    const viaUrl = url.searchParams.get("view") === token;
    const viaCookie = cookies[ck] === token;
    const okToken = viaUrl || viaCookie;
    const setCookie = viaUrl
      ? `${ck}=${token}; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax`
      : null;
    if (m.visibility === "token") {
      if (okToken) return { ok: true, setCookie };
      const role = this.roleOf(request.headers.get("x-fragment-pubkey"));
      if (FragmentCell.rank(role) >= 1) return { ok: true };
      return { ok: false, status: 403, error: "this fragment needs its ?view= link token" };
    }
    // viewers
    const role = this.roleOf(request.headers.get("x-fragment-pubkey"));
    if (FragmentCell.rank(role) >= 1) return { ok: true };
    return { ok: false, status: request.headers.get("x-fragment-pubkey") ? 403 : 401, error: "this fragment is restricted to listed viewers (NIP-98)" };
  }

  // ---------- rooms (/__room/<room>) ----------
  async roomRoute(request, url) {
    const room = decodeURIComponent(url.pathname.slice("/__room/".length));
    const draftParam = url.searchParams.get("draft") || "blessed";
    if (draftParam === "blessed") {
      const vis = this.checkVisibility(request, url);
      if (!vis.ok) return new Response(vis.error + "\n", { status: vis.status });
    }
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("websocket upgrade required", { status: 426 });
    }
    const pair = new WebSocketPair();
    const server = pair[0];
    this.state.acceptWebSocket(server);
    const clientId = randSlug(6);
    server.serializeAttachment({ room, clientId, label: null, draft: draftParam });
    // hello
    const stateRow = this.sql.exec("SELECT state FROM rooms WHERE room = ?", room).toArray()[0];
    const history = this.sql.exec("SELECT sender, data, at FROM room_msgs WHERE room = ? ORDER BY id DESC LIMIT 50", room).toArray().reverse();
    server.send(JSON.stringify({
      type: "hello",
      state: stateRow ? JSON.parse(stateRow.state) : null,
      presence: this.presenceList(room),
      history: history.map((h) => ({ from: h.sender, data: JSON.parse(h.data), at: h.at })),
    }));
    return new Response(null, { status: 101, webSocket: pair[1] });
  }

  presenceList(room) {
    const out = [];
    for (const ws of this.state.getWebSockets()) {
      try {
        const a = ws.deserializeAttachment();
        if (a && a.room === room) out.push({ clientId: a.clientId, label: a.label });
      } catch {}
    }
    return out;
  }

  broadcast(room, text) {
    for (const ws of this.state.getWebSockets()) {
      try {
        const a = ws.deserializeAttachment();
        if (a && a.room === room) ws.send(text);
      } catch {}
    }
  }

  async webSocketMessage(ws, raw) {
    let a;
    try { a = ws.deserializeAttachment(); } catch { return; }
    if (!a || !a.room) return;
    let msg;
    try { msg = JSON.parse(typeof raw === "string" ? raw : new TextDecoder().decode(raw)); } catch { return; }

    if (msg.type === "presence") {
      ws.serializeAttachment({ ...a, label: msg.data?.label ?? null });
      this.broadcast(a.room, JSON.stringify({ type: "presence", list: this.presenceList(a.room) }));
      return;
    }

    if (msg.type === "state:set") {
      this.sql.exec("INSERT INTO rooms (room, state) VALUES (?, ?) ON CONFLICT(room) DO UPDATE SET state = excluded.state", a.room, JSON.stringify(msg.value ?? null));
      this.broadcast(a.room, JSON.stringify({ type: "state", value: msg.value ?? null }));
      return;
    }

    if (msg.type === "msg") {
      let data = msg.data;
      // server-side hook from the served draft's rooms.mjs
      const slug = a.draft === "blessed" ? this.getMeta("blessed") : a.draft;
      if (slug) {
        const row = this.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = 'rooms.mjs'", slug).toArray()[0];
        if (row) {
          try {
            const ep = await this.loadCode(`rooms:${slug}`, ROOMS_MAIN,
              { "rooms.mjs": new TextDecoder().decode(toAB(row.content)) }, { kind: "draft", slug });
            const resp = await ep.fetch("http://loaded/rooms", {
              method: "POST",
              body: JSON.stringify({ room: a.room, msg: { from: a.clientId, data, at: Date.now() } }),
            });
            const out = await resp.json();
            if (out.error) {
              this.addEvent("room-error", `rooms.mjs in ${a.room}: ${out.error}`);
              ws.send(JSON.stringify({ type: "error", error: out.error }));
              return;
            }
            if ("broadcast" in out) data = out.broadcast;
            if ("state" in out) {
              this.sql.exec("INSERT INTO rooms (room, state) VALUES (?, ?) ON CONFLICT(room) DO UPDATE SET state = excluded.state", a.room, JSON.stringify(out.state));
              this.broadcast(a.room, JSON.stringify({ type: "state", value: out.state }));
            }
            if (out.drop) {
              this.addEvent("room-drop", `rooms.mjs dropped a message in ${a.room}${out.reason ? ": " + out.reason : ""}`);
              return;
            }
          } catch (e) {
            this.addEvent("room-error", `rooms.mjs threw in ${a.room}: ${String(e)}`);
            ws.send(JSON.stringify({ type: "error", error: "rooms.mjs: " + String(e) }));
            return;
          }
        }
      }
      const at = Date.now();
      this.sql.exec("INSERT INTO room_msgs (room, at, sender, data) VALUES (?, ?, ?, ?)", a.room, at, a.clientId, JSON.stringify(data ?? null));
      this.sql.exec("DELETE FROM room_msgs WHERE room = ? AND id < (SELECT COALESCE(MIN(id), 0) FROM (SELECT id FROM room_msgs WHERE room = ? ORDER BY id DESC LIMIT 50))", a.room, a.room);
      this.broadcast(a.room, JSON.stringify({ type: "msg", from: a.clientId, data, at }));
    }
  }

  async webSocketClose(ws) {
    let a;
    try { a = ws.deserializeAttachment(); } catch { return; }
    if (a && a.room) this.broadcast(a.room, JSON.stringify({ type: "presence", list: this.presenceList(a.room) }));
  }
}
