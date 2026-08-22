// FragmentCell — one Durable Object per fragment (plus one named "_registry").
// All fragment state lives in this cell's SQLite. See ARCHITECTURE.md.
// The class is the state + dispatch core; route planes and machinery live in
// sibling modules and are invoked through the delegating methods below.
import { npubFromHex, hexFromNpub } from "./bech32.js";
import { json, toAB } from "./util.js";
import { parseCron } from "./cron.js";
import { initCell, registryRoute, syncRolesToRegistry } from './registry.js';
import { canonicalUrl, serveRoute, checkVisibility } from './serve.js';
import { apiRoute } from './api-routes.js';
import { rearmAlarm, alarm, scheduleSyncTrigger, fireSyncTriggers } from './alarms.js';
import { makeToken, checkToken, internalBase, loadCode, collectModules, runWorkflow, runWorkflowLocked } from './loader.js';
import { internalRoute } from './internal.js';
import { roomRoute, presenceList, broadcast, webSocketMessage, webSocketClose } from './rooms.js';
import { SCHEMA, rankOf } from './util.js';

export class FragmentCell {
  state: any;
  env: any;
  sql: any;
  constructor(state: any, env: any) {
    this.state = state;
    this.env = env;
    this.sql = state.storage.sql;
    this.sql.exec(SCHEMA);
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
    return raw ? JSON.parse(raw) : null;
  }
  addEvent(kind, summary, data) {
    this.sql.exec("INSERT INTO events (at, kind, summary, data) VALUES (?, ?, ?, ?)",
      Date.now(), kind, String(summary).slice(0, 500), data === undefined ? null : JSON.stringify(data).slice(0, 4000));
  }
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
      if (path.startsWith("/__internal/")) return await this.internalRoute(request, url);
      if (path.startsWith("/api/")) return await this.apiRoute(request, url);
      if (path.startsWith("/__serve/")) return await this.serveRoute(request, url);
      if (path.startsWith("/__room/")) return await this.roomRoute(request, url);
      return new Response("not found", { status: 404 });
    } catch (e) {
      return json({ error: String((e && e.stack) || e) }, 500);
    }
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
  getFileRow(path) {
    return this.sql.exec("SELECT content, rev, sha256 FROM files WHERE path = ? AND deleted = 0", path).toArray()[0] || null;
  }
  getFileText(path) {
    const row = this.getFileRow(path);
    return row ? new TextDecoder().decode(toAB(row.content)) : null;
  }

  // ---------- delegating methods (implementation in sibling modules) ----------
  async initCell(request) { return initCell(this, request); }
  async registryRoute(request, url) { return registryRoute(this, request, url); }
  async syncRolesToRegistry() { return syncRolesToRegistry(this); }
  canonicalUrl(origin, name) { return canonicalUrl(this, origin, name); }
  async serveRoute(request, url) { return serveRoute(this, request, url); }
  checkVisibility(request, url) { return checkVisibility(this, request, url); }
  async apiRoute(request, url) { return apiRoute(this, request, url); }
  async rearmAlarm() { return rearmAlarm(this); }
  async alarm() { return alarm(this); }
  async scheduleSyncTrigger(path) { return scheduleSyncTrigger(this, path); }
  async fireSyncTriggers(m) { return fireSyncTriggers(this, m); }
  makeToken(scope) { return makeToken(this, scope); }
  checkToken(request) { return checkToken(this, request); }
  internalBase() { return internalBase(this); }
  async loadCode(id, mainSource, modules, scope) { return loadCode(this, id, mainSource, modules, scope); }
  collectModules(prefix) { return collectModules(this, prefix); }
  async runWorkflow(wf, input, opts = {}) { return runWorkflow(this, wf, input, opts); }
  async runWorkflowLocked(wf, input) { return runWorkflowLocked(this, wf, input); }
  async internalRoute(request, url) { return internalRoute(this, request, url); }
  async roomRoute(request, url) { return roomRoute(this, request, url); }
  presenceList(room) { return presenceList(this, room); }
  broadcast(room, text) { return broadcast(this, room, text); }
  async webSocketMessage(ws, raw) { return webSocketMessage(this, ws, raw); }
  async webSocketClose(ws) { return webSocketClose(this, ws); }
}
