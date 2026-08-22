// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { randHex, toAB } from "./util.js";
import { CTX_SHIM_SOURCE } from "./ctx-shim.js";
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
function makeToken(cell, scope) {
  const token = randHex(16);
  const ttl = scope.kind === "run" || scope.kind === "draft" ? scope.blessed ? null : 36e5 : 36e5;
  cell.sql.exec("DELETE FROM run_tokens WHERE expires < ?", Date.now() - 24 * 36e5);
  if (scope.kind === "draft") {
    for (const r of cell.sql.exec("SELECT token, scope FROM run_tokens").toArray()) {
      try {
        const s = JSON.parse(r.scope);
        if (s.kind === "draft" && s.slug === scope.slug && !!s.blessed === !!scope.blessed) {
          cell.sql.exec("DELETE FROM run_tokens WHERE token = ?", r.token);
        }
      } catch {
      }
    }
  }
  cell.sql.exec("INSERT INTO run_tokens (token, scope, expires) VALUES (?, ?, ?)", token, JSON.stringify(scope), ttl === null ? null : Date.now() + ttl);
  return token;
}
function checkToken(cell, request) {
  const token = request.headers.get("x-fragment-token") || "";
  const row = cell.sql.exec("SELECT scope, expires FROM run_tokens WHERE token = ?", token).toArray()[0];
  if (!row) return null;
  if (row.expires !== null && row.expires !== void 0 && row.expires < Date.now()) return null;
  return JSON.parse(row.scope);
}
function internalBase(cell) {
  const base = cell.env.FRAGMENT_INTERNAL_URL || "http://127.0.0.1:8789";
  return `${base}/__internal/f/${cell.getMeta("name")}`;
}
async function loadCode(cell, id, mainSource, modules, scope) {
  const wrapped = {};
  for (const [k, v] of Object.entries({ "main.mjs": mainSource, "fragment-ctx.mjs": CTX_SHIM_SOURCE, ...modules })) {
    wrapped[k] = cell.env.FRAGMENT_HOST_KIND === "cf" ? { js: v } : v;
  }
  const worker = await cell.env.LOADER.get(id, async () => ({
    compatibilityDate: "2026-01-01",
    mainModule: "main.mjs",
    modules: wrapped,
    env: {
      FRAGMENT_INTERNAL_URL: cell.internalBase(),
      FRAGMENT_RUN_TOKEN: cell.makeToken(scope),
      FRAGMENT_SCOPE: scope.kind,
      // forwarded by ctx as x-fragment-host-secret; checked by the router
      // on /__internal when the host sets FRAGMENT_HOST_SECRET
      ...cell.env.FRAGMENT_HOST_SECRET ? { FRAGMENT_HOST_SECRET: cell.env.FRAGMENT_HOST_SECRET } : {}
    }
  }));
  return worker.getEntrypoint ? worker.getEntrypoint() : worker;
}
function collectModules(cell, prefix) {
  const rows = cell.sql.exec("SELECT path, content FROM files WHERE path LIKE ? AND deleted = 0", prefix + "%").toArray();
  const modules = {};
  for (const r of rows) modules[r.path] = new TextDecoder().decode(toAB(r.content));
  return modules;
}
async function runWorkflow(cell, wf, input, opts = {}) {
  const lockKey = `wf_lock_${wf.name}`;
  const leaseMs = 10 * 6e4;
  const lock = parseInt(cell.getMeta(lockKey) || "0", 10);
  if (opts.auto && lock && lock > Date.now() - leaseMs) {
    cell.addEvent("workflow-skipped", `${wf.name}: previous run still active`);
    return { ok: true, skipped: true };
  }
  cell.setMeta(lockKey, String(Date.now()));
  try {
    return await cell.runWorkflowLocked(wf, input);
  } finally {
    cell.setMeta(lockKey, "");
  }
}
async function runWorkflowLocked(cell, wf, input) {
  const name = cell.getMeta("name");
  cell.addEvent("workflow-start", `${wf.name}`);
  try {
    const src = cell.getFileText(wf.file);
    if (src === null) throw new Error(`workflow file not found in folder: ${wf.file}`);
    const modules = cell.collectModules("workflows/");
    const rev = cell.getMeta("rev");
    const ep = await cell.loadCode(
      `wf:${name}:${wf.name}:${rev}`,
      WORKFLOW_MAIN.replaceAll("__WF__", wf.file),
      modules,
      { kind: "run", workflow: wf.name }
    );
    const resp = await ep.fetch("http://loaded/run", { method: "POST", body: JSON.stringify(input ?? null) });
    const out = await resp.json();
    if (out.ok) cell.addEvent("workflow-ok", `${wf.name}`, out.output !== null && out.output !== void 0 ? { output: out.output } : void 0);
    else cell.addEvent("workflow-error", `${wf.name}: ${out.error}`);
    return out;
  } catch (e) {
    cell.addEvent("workflow-error", `${wf.name}: ${String(e)}`);
    return { ok: false, error: String(e) };
  }
}
export {
  APP_MAIN,
  ROOMS_MAIN,
  WORKFLOW_MAIN,
  checkToken,
  collectModules,
  internalBase,
  loadCode,
  makeToken,
  runWorkflow,
  runWorkflowLocked
};
