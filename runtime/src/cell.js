// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { npubFromHex } from "./bech32.js";
import { json } from "./util.js";
import { readFileText } from "./git-plane.js";
import { initCell, registryRoute, syncRolesToRegistry } from "./registry.js";
import { canonicalUrl, serveRoute, checkVisibility } from "./serve.js";
import { apiRoute } from "./api-routes.js";
import { rearmAlarm, alarm, fireSyncTriggers } from "./alarms.js";
import { makeToken, checkToken, internalBase, loadCode, collectModules, runWorkflowLocked } from "./loader.js";
import { executeWorkflow, resumeDueRuns, finishAttempt, retryPolicy } from "./runs.js";
import { normalizeManifest } from "./manifest.js";
import { internalRoute } from "./internal.js";
import { roomRoute, presenceList, broadcast, webSocketMessage, webSocketClose } from "./rooms.js";
import { watchRoute } from "./history.js";
import { SCHEMA, SCHEMA_VERSION, SCHEMA_DROPS, rankOf } from "./util.js";
import { CodeStorageError } from "./codestorage.js";
class FragmentCell {
  state;
  env;
  sql;
  _manifest = null;
  _manifestRaw = null;
  constructor(state, env) {
    this.state = state;
    this.env = env;
    this.sql = state.storage.sql;
    const hasMeta = this.sql.exec("SELECT COUNT(*) c FROM sqlite_master WHERE type = 'table' AND name = 'meta'").toArray()[0].c > 0;
    const metaRows = hasMeta ? this.sql.exec("SELECT COUNT(*) c FROM meta").toArray()[0].c : 0;
    const stored = hasMeta ? this.getMeta("schema") : null;
    if (!hasMeta || metaRows === 0) {
      this.sql.exec(SCHEMA);
      this.setMeta("schema", String(SCHEMA_VERSION));
    } else if (String(stored) !== String(SCHEMA_VERSION)) {
      this.sql.exec(SCHEMA);
      for (const t of SCHEMA_DROPS) this.sql.exec(`DROP TABLE IF EXISTS ${t}`);
      this.setMeta("schema", String(SCHEMA_VERSION));
    }
  }
  getMeta(k) {
    const row = this.sql.exec("SELECT v FROM meta WHERE k = ?", k).toArray()[0];
    return row ? row.v : null;
  }
  setMeta(k, v) {
    this.sql.exec("INSERT INTO meta (k, v) VALUES (?, ?) ON CONFLICT(k) DO UPDATE SET v = excluded.v", k, String(v));
  }
  manifest() {
    const raw = this.getMeta("manifest");
    if (raw !== this._manifestRaw) {
      this._manifest = raw ? JSON.parse(raw) : null;
      this._manifestRaw = raw;
    }
    return this._manifest;
  }
  addEvent(kind, summary, data) {
    this.sql.exec(
      "INSERT INTO events (at, kind, summary, data) VALUES (?, ?, ?, ?)",
      Date.now(),
      kind,
      String(summary).slice(0, 500),
      data === void 0 ? null : JSON.stringify(data).slice(0, 4e3)
    );
    this.sql.exec("DELETE FROM events WHERE id <= (SELECT COALESCE(MAX(id), 0) - 5000 FROM events)");
  }
  roleOf(pubkeyHex) {
    const m = this.manifest();
    if (!m) return null;
    if (pubkeyHex && pubkeyHex === this.getMeta("owner")) return "owner";
    if (pubkeyHex) {
      let npub;
      try {
        npub = npubFromHex(pubkeyHex);
      } catch {
        return null;
      }
      if ((m.editors || []).includes(npub)) return "editor";
      if ((m.viewers || []).includes(npub)) return "viewer";
    }
    return null;
  }
  needRole(req, min) {
    const pk = req.headers.get("x-fragment-pubkey");
    const role = this.roleOf(pk);
    if (rankOf(role) >= rankOf(min)) return { ok: true, role, pubkey: pk };
    return { ok: false, status: pk ? 403 : 401, error: pk ? `requires ${min} role` : "unauthenticated" };
  }
  async fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;
    try {
      if (path.startsWith("/__registry/")) return await this.registryRoute(request, url);
      if (path === "/__cell/init") return await this.initCell(request);
      if (path === "/__cell/wipe") return this.wipeCell();
      if (path === "/__cell/gallery-info") return this.galleryInfo();
      if (path.startsWith("/__internal/")) return await this.internalRoute(request, url);
      if (path.startsWith("/api/")) return await this.apiRoute(request, url);
      if (path.startsWith("/__serve/")) return await this.serveRoute(request, url);
      if (path.startsWith("/__room/")) return await this.roomRoute(request, url);
      if (path.startsWith("/__watch")) return watchRoute(this, request, url);
      return new Response("not found", { status: 404 });
    } catch (e) {
      if (e instanceof CodeStorageError) return json({ error: String(e && e.message || e) }, e.status || 502);
      return json({ error: String(e && e.stack || e) }, 500);
    }
  }
  validateManifest(m) {
    return normalizeManifest(m).error || null;
  }
  galleryInfo() {
    const m = this.manifest();
    if (!m) return json({ entry: null });
    const listed = m.meta?.listed === true;
    const isPublic = m.visibility === "public";
    if (!listed && !isPublic) return json({ entry: null });
    return json({
      entry: {
        name: m.name,
        visibility: m.visibility,
        title: m.meta?.title || null,
        description: m.meta?.description || null,
        image: m.meta?.image || null,
        // sharing the link through the gallery is the opt-in's meaning
        viewToken: listed ? this.getMeta("view_token") : null
      }
    });
  }
  wipeCell() {
    for (const t of [
      "meta",
      "git_tree",
      "own_commits",
      "webhook_events",
      "secrets",
      "inbox",
      "events",
      "wstate",
      "rooms",
      "room_msgs",
      "run_tokens",
      "runs",
      "fragments",
      "roles",
      "push_subs"
    ]) {
      this.sql.exec(`DELETE FROM ${t}`);
    }
    this._manifest = null;
    this._manifestRaw = null;
    return json({ ok: true });
  }
  // ---------- delegating methods (implementation in sibling modules) ----------
  async initCell(request) {
    return initCell(this, request);
  }
  async registryRoute(request, url) {
    return registryRoute(this, request, url);
  }
  async syncRolesToRegistry() {
    return syncRolesToRegistry(this);
  }
  canonicalUrl(origin, name) {
    return canonicalUrl(this, origin, name);
  }
  async serveRoute(request, url) {
    return serveRoute(this, request, url);
  }
  checkVisibility(request, url) {
    return checkVisibility(this, request, url);
  }
  async apiRoute(request, url) {
    return apiRoute(this, request, url);
  }
  async rearmAlarm() {
    return rearmAlarm(this);
  }
  async alarm() {
    return alarm(this);
  }
  async fireSyncTriggers(m) {
    return fireSyncTriggers(this, m);
  }
  makeToken(scope) {
    return makeToken(this, scope);
  }
  checkToken(request) {
    return checkToken(this, request);
  }
  internalBase() {
    return internalBase(this);
  }
  async loadCode(id, mainSource, modules, scope, cause = null) {
    return loadCode(this, id, mainSource, modules, scope, cause);
  }
  async collectModules(prefix) {
    return collectModules(this, prefix);
  }
  async runWorkflowLocked(wf, input, cause = null) {
    return runWorkflowLocked(this, wf, input, cause);
  }
  async executeWorkflow(wf, input, opts = {}) {
    return executeWorkflow(this, wf, input, opts);
  }
  async resumeDueRuns() {
    return resumeDueRuns(this);
  }
  async applyRunOutcome(runId, attempt, outcome) {
    const row = this.sql.exec("SELECT * FROM runs WHERE id = ?", Number(runId) || 0).toArray()[0];
    if (!row) return json({ error: `no such run: ${runId}` }, 404);
    const m = this.manifest();
    const wf = m && (m.workflows || []).find((w) => w.name === row.wf);
    if (!wf) return json({ error: `workflow ${row.wf} is no longer in the manifest` }, 404);
    const res = await finishAttempt(this, wf, row.id, Number(attempt) || row.attempt, retryPolicy(wf), row.via, row.started_at, outcome);
    return json(res);
  }
  async internalRoute(request, url) {
    return internalRoute(this, request, url);
  }
  async roomRoute(request, url) {
    return roomRoute(this, request, url);
  }
  presenceList(room) {
    return presenceList(this, room);
  }
  broadcast(room, text) {
    return broadcast(this, room, text);
  }
  async webSocketMessage(ws, raw) {
    return webSocketMessage(this, ws, raw);
  }
  async webSocketClose(ws) {
    return webSocketClose(this, ws);
  }
  // bounded text read over the git plane (kept as a cell method because
  // loader/rooms/serve all used cell.getFileText)
  async getFileText(path) {
    return readFileText(this, path);
  }
}
export {
  FragmentCell
};
