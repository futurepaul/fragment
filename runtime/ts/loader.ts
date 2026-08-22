// Worker Loader machinery: mint/check run tokens, load workflow/app/
// rooms code, single-flight workflow execution.
import { json, randHex, toAB } from "./util.js";
import { CTX_SHIM_SOURCE } from "./ctx-shim.js";

export const WORKFLOW_MAIN = `
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

export const APP_MAIN = `
import { makeCtx } from "./fragment-ctx.mjs";
import app from "./app.mjs";
export default {
  async fetch(req, env) {
    const ctx = await makeCtx(env);
    return app.fetch(req, ctx);
  }
}
`;

export const ROOMS_MAIN = `
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


// ------ makeToken ------

export function makeToken(cell, scope) {
  const token = randHex(16);
  // token lifetime = the lifetime of the thing it authenticates:
  // workflow runs are short (1h is a safety net for wedged runs), draft
  // previews are rehearsals (1h), a blessed app lives until superseded
  // (null expiry; revoked at rebless, swept on restart re-mint).
  const ttl = scope.kind === "run" || scope.kind === "draft" ? (scope.blessed ? null : 3600_000) : 3600_000;
  cell.sql.exec("DELETE FROM run_tokens WHERE expires < ?", Date.now() - 24 * 3600_000);
  if (scope.kind === "draft") {
    // one live token per (slug, blessed-ness): re-mints sweep their predecessor
    for (const r of cell.sql.exec("SELECT token, scope FROM run_tokens").toArray()) {
      try {
        const s = JSON.parse(r.scope);
        if (s.kind === "draft" && s.slug === scope.slug && !!s.blessed === !!scope.blessed) {
          cell.sql.exec("DELETE FROM run_tokens WHERE token = ?", r.token);
        }
      } catch {}
    }
  }
  cell.sql.exec("INSERT INTO run_tokens (token, scope, expires) VALUES (?, ?, ?)", token, JSON.stringify(scope), ttl === null ? null : Date.now() + ttl);
  return token;
}

// ------ checkToken ------

export function checkToken(cell, request) {
  // header-only, deliberately: run tokens unlock files/secrets/state for
  // one isolate. Query params land in access logs, proxy logs, and browser
  // history — a leaked run token is a fragment-wide capability leak. The
  // ctx shim has always sent x-fragment-token as a header.
  const token = request.headers.get("x-fragment-token") || "";
  const row = cell.sql.exec("SELECT scope, expires FROM run_tokens WHERE token = ?", token).toArray()[0];
  if (!row) return null;
  if (row.expires !== null && row.expires !== undefined && row.expires < Date.now()) return null;
  return JSON.parse(row.scope);
}

// ------ internalBase ------

export function internalBase(cell) {
  const base = cell.env.FRAGMENT_INTERNAL_URL || "http://127.0.0.1:8789";
  return `${base}/__internal/f/${cell.getMeta("name")}`;
}

// ------ loadCode ------

export async function loadCode(cell, id, mainSource, modules, scope) {
  // raw = { "main.mjs": mainSource, "fragment-ctx.mjs": CTX_SHIM_SOURCE, ...modules };
  // Host divergence: celld's loader wants plain-string modules (and accepts
  // .mjs names); CF's loader requires .js/.py names for strings, so on CF
  // we wrap every module as {js: source} to keep our .mjs names legal.
  const wrapped = {};
  for (const [k, v] of Object.entries({ "main.mjs": mainSource, "fragment-ctx.mjs": CTX_SHIM_SOURCE, ...modules })) {
    wrapped[k] = cell.env.FRAGMENT_HOST_KIND === "cf" ? { js: v } : v;
  }
  // the token is minted ONLY when the worker is actually created: the
  // loader caches workers by id, and a cached worker keeps whatever token
  // its env was born with. Minting per-request (as cell used to) orphaned
  // the live worker's token — every ctx call 403'd once the original
  // expired (or was swept).
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
      ...(cell.env.FRAGMENT_HOST_SECRET ? { FRAGMENT_HOST_SECRET: cell.env.FRAGMENT_HOST_SECRET } : {}),
    },
  }));
  return worker.getEntrypoint ? worker.getEntrypoint() : worker;
}

// ------ collectModules ------

export function collectModules(cell, prefix) {
  const rows = cell.sql.exec("SELECT path, content FROM files WHERE path LIKE ? AND deleted = 0", prefix + "%").toArray();
  const modules = {};
  for (const r of rows) modules[r.path] = new TextDecoder().decode(toAB(r.content));
  return modules;
}

// ------ runWorkflow ------

export async function runWorkflow(cell, wf: any, input: any, opts: { auto?: boolean } = {}) {
  // single-flight: auto-triggered runs (cron/inbox/sync) skip while a
  // previous run of the same workflow is still active. Overlapping runs
  // read pre-run state and duplicate each other's work — a loop amplifier
  // (seen live: the news re-filing pileup). Manual `fragment run` always
  // proceeds. The lock is a timestamp lease so a crashed run can't wedge
  // the workflow forever.
  const lockKey = `wf_lock_${wf.name}`;
  const leaseMs = 10 * 60_000;
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

// ------ runWorkflowLocked ------

export async function runWorkflowLocked(cell, wf, input) {
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
      { kind: "run", workflow: wf.name },
    );
    // body IS the workflow input (WORKFLOW_MAIN passes req.json() to run)
    const resp = await ep.fetch("http://loaded/run", { method: "POST", body: JSON.stringify(input ?? null) });
    const out = await resp.json();
    if (out.ok) cell.addEvent("workflow-ok", `${wf.name}`, out.output !== null && out.output !== undefined ? { output: out.output } : undefined);
    else cell.addEvent("workflow-error", `${wf.name}: ${out.error}`);
    return out;
  } catch (e) {
    cell.addEvent("workflow-error", `${wf.name}: ${String(e)}`);
    return { ok: false, error: String(e) };
  }
}
