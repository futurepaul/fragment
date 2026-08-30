// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { randHex } from "./util.js";
import { CTX_SHIM_SOURCE } from "./ctx-shim.js";
import { AI_MODULE_SOURCE } from "./ai-module.js";
import { tierTextBounded } from "./blob-tier.js";
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
    //__FRAGMENT_AI_INIT__
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
    //__FRAGMENT_AI_INIT__
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
      //__FRAGMENT_AI_INIT__
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
  const ttl = scope.kind === "run" || scope.kind === "draft" && scope.blessed ? null : 36e5;
  cell.sql.exec("DELETE FROM run_tokens WHERE expires < ?", Date.now() - 24 * 36e5);
  if (scope.kind === "draft") {
    for (const r of cell.sql.exec("SELECT token, scope FROM run_tokens").toArray()) {
      try {
        const s = JSON.parse(r.scope);
        if (s.kind === "draft" && s.worker === scope.worker && s.slug === scope.slug && !!s.blessed === !!scope.blessed) {
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
  return checkTokenRaw(cell, token);
}
function checkTokenRaw(cell, token) {
  if (!token) return null;
  const row = cell.sql.exec("SELECT scope, expires FROM run_tokens WHERE token = ?", token).toArray()[0];
  if (!row) return null;
  if (row.expires !== null && row.expires !== void 0 && row.expires < Date.now()) return null;
  return JSON.parse(row.scope);
}
function internalBase(cell) {
  const base = cell.env.FRAGMENT_INTERNAL_URL || "http://127.0.0.1:8789";
  return `${base}/__internal/f/${cell.getMeta("name")}`;
}
async function loadCode(cell, id, mainSource, modules, scope, cause = null) {
  const wantsAi = Object.values(modules).some((src) => /\b(?:from|import)\s*["']fragment:ai["']/.test(String(src)));
  const mainFinal = wantsAi ? 'import { init as __fragmentAiInit } from "fragment:ai";\n' + mainSource.replace(
    "//__FRAGMENT_AI_INIT__",
    'try { __fragmentAiInit(env); } catch (e) { return Response.json({ ok: false, error: "fragment:ai init: " + String(e) }); }'
  ) : mainSource.replace("//__FRAGMENT_AI_INIT__", "");
  const wrapped = {};
  for (const [k, v] of Object.entries({ "main.mjs": mainFinal, "fragment-ctx.mjs": CTX_SHIM_SOURCE, ...wantsAi ? { "fragment:ai": AI_MODULE_SOURCE } : {}, ...modules })) {
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
      // Blob-tier vars (docs/blob-tier.md) deliberately stay OUT of author
      // workers: ctx.files.* funnels through the loopback internal plane and
      // the CELL performs all tier traffic with host credentials. They ride
      // the same CELLD_VAR_ passthrough as everything else on cell.env:
      //   CELLD_VAR_BLOBSD_URL            -> env.BLOBSD_URL            (tier write/read base)
      //   CELLD_VAR_BLOBSD_INTERNAL_TOKEN -> env.BLOBSD_INTERNAL_TOKEN (Bearer for runtime tier calls)
      //   CELLD_VAR_BLOBSD_PUBLIC_GET=1   -> env.BLOBSD_PUBLIC_GET     (302 mode for public/link cells)
      //   CELLD_VAR_BLOBSD_PUBLIC_URL     -> env.BLOBSD_PUBLIC_URL     (302 Location base)
      // apps that declare secrets get them eagerly — one loopback, paid
      // only when the manifest asks for it (lazy fill broke first-render
      // reads; found live: the tray 500'd "missing secrets")
      ...(cell.manifest()?.secrets || []).length && scope.kind === "draft" ? { FRAGMENT_EAGER_SECRETS: "1" } : {},
      // the run's cause chain, so ctx.http can stamp hop headers
      ...cause ? { FRAGMENT_CAUSE: JSON.stringify(cause) } : {},
      // forwarded by ctx as x-fragment-host-secret; checked by the router
      // on /__internal when the host sets FRAGMENT_HOST_SECRET
      ...cell.env.FRAGMENT_HOST_SECRET ? { FRAGMENT_HOST_SECRET: cell.env.FRAGMENT_HOST_SECRET } : {},
      // host model defaults + fal base for the platform "ai" module —
      // configuration, never credentials (keys stay cell-side; the egress
      // route attaches them)
      ...cell.env.FRAGMENT_AI_MODEL ? { FRAGMENT_AI_MODEL: cell.env.FRAGMENT_AI_MODEL } : {},
      ...cell.env.FRAGMENT_IMAGE_MODEL ? { FRAGMENT_IMAGE_MODEL: cell.env.FRAGMENT_IMAGE_MODEL } : {},
      ...cell.env.FRAGMENT_VIDEO_MODEL ? { FRAGMENT_VIDEO_MODEL: cell.env.FRAGMENT_VIDEO_MODEL } : {},
      ...cell.env.FRAGMENT_FAL_BASE ? { FRAGMENT_FAL_BASE: cell.env.FRAGMENT_FAL_BASE } : {}
    }
  }));
  return worker.getEntrypoint ? worker.getEntrypoint() : worker;
}
async function collectModules(cell, prefixes) {
  const list = Array.isArray(prefixes) ? prefixes : [prefixes];
  const modules = {};
  for (const prefix of list) {
    const rows = cell.sql.exec("SELECT path, sha256, size FROM files WHERE path LIKE ? AND deleted = 0", prefix + "%").toArray();
    for (const r of rows) modules[r.path] = await tierTextBounded(cell, r, `module ${r.path}`);
  }
  return modules;
}
function rewriteRelatives(src, fromKey) {
  const resolve = (spec) => {
    const parts = spec.split("/");
    if (parts[0] !== "." && parts[0] !== "..") return null;
    const stack = fromKey.split("/").slice(0, -1);
    for (const p of parts) {
      if (p === ".") continue;
      else if (p === "..") stack.pop();
      else stack.push(p);
    }
    return stack.join("/");
  };
  const sub = (_m, pre, q, spec) => {
    const r = resolve(spec);
    return r === null ? _m : pre + q + r + q;
  };
  return src.replace(/(\bfrom\s*)(["'])(\.[^"']+)\2/g, sub).replace(/(\bimport\s*)(["'])(\.[^"']+)\2/g, sub).replace(/(\bimport\s*\(\s*)(["'])(\.[^"']+)\2/g, sub);
}
async function runWorkflowLocked(cell, wf, input, cause = null) {
  const name = cell.getMeta("name");
  try {
    const src = await cell.getFileText(wf.file);
    if (src === null) throw new Error(`workflow file not found in folder: ${wf.file}`);
    const modules = await collectModules(cell, ["workflows/", "lib/"]);
    for (const k of Object.keys(modules)) modules[k] = rewriteRelatives(modules[k], k);
    const codeHash = await crypto.subtle.digest(
      "SHA-256",
      new TextEncoder().encode(JSON.stringify([wf.file, modules]))
    );
    const hash = [...new Uint8Array(codeHash)].map((b) => b.toString(16).padStart(2, "0")).join("").slice(0, 16);
    const ep = await cell.loadCode(
      `wf:${name}:${wf.name}:${hash}`,
      WORKFLOW_MAIN.replaceAll("__WF__", wf.file),
      modules,
      { kind: "run", workflow: wf.name },
      cause
    );
    const resp = await ep.fetch("http://loaded/run", { method: "POST", body: JSON.stringify(input ?? null) });
    return await resp.json();
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}
export {
  APP_MAIN,
  ROOMS_MAIN,
  WORKFLOW_MAIN,
  checkToken,
  checkTokenRaw,
  collectModules,
  internalBase,
  loadCode,
  makeToken,
  rewriteRelatives,
  runWorkflowLocked
};
