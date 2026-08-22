// Worker Loader machinery: mint/check run tokens, load workflow/app/
// rooms code, and execute one workflow attempt in a loader isolate.
// Lifecycle (guards, retries, held) lives in runs.ts; this module only
// loads and executes.
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

export async function loadCode(cell, id, mainSource, modules, scope, cause = null) {
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
      // the run's cause chain, so ctx.http can stamp hop headers
      ...(cause ? { FRAGMENT_CAUSE: JSON.stringify(cause) } : {}),
      // forwarded by ctx as x-fragment-host-secret; checked by the router
      // on /__internal when the host sets FRAGMENT_HOST_SECRET
      ...(cell.env.FRAGMENT_HOST_SECRET ? { FRAGMENT_HOST_SECRET: cell.env.FRAGMENT_HOST_SECRET } : {}),
    },
  }));
  return worker.getEntrypoint ? worker.getEntrypoint() : worker;
}

// ------ collectModules ------

export function collectModules(cell, prefixes: string | string[]) {
  const list = Array.isArray(prefixes) ? prefixes : [prefixes];
  const modules: Record<string, string> = {};
  for (const prefix of list) {
    const rows = cell.sql.exec("SELECT path, content FROM files WHERE path LIKE ? AND deleted = 0", prefix + "%").toArray();
    for (const r of rows) modules[r.path] = new TextDecoder().decode(toAB(r.content));
  }
  return modules;
}

// The loader resolves import specifiers root-relative ("./x" → "x"), with no
// directory awareness — a `./helper.mjs` inside workflows/foo.mjs dials a
// nonexistent module and dies with "instantiate: <none>". Authors (and
// agents) write Node-style relative imports, so rewrite them against the
// importing module's directory before the code reaches the loader. Map-path
// imports ("workflows/helper.mjs", "lib/poll.mjs") pass through untouched.
export function rewriteRelatives(src: string, fromKey: string): string {
  const resolve = (spec: string) => {
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
  return src
    .replace(/(\bfrom\s*)(["'])(\.[^"']+)\2/g, sub)                    // import x from / export … from
    .replace(/(\bimport\s*)(["'])(\.[^"']+)\2/g, sub)                  // side-effect import "…"
    .replace(/(\bimport\s*\(\s*)(["'])(\.[^"']+)\2/g, sub);            // dynamic import(…)
}

// ------ runWorkflowLocked: one attempt, in a loader isolate ------
// Called by runs.ts (which owns guards and transitions). Events are
// appended there, not here — the ledger narrative has one author.

export async function runWorkflowLocked(cell, wf, input, cause = null) {
  const name = cell.getMeta("name");
  try {
    const src = cell.getFileText(wf.file);
    if (src === null) throw new Error(`workflow file not found in folder: ${wf.file}`);
    // workflows/ plus lib/ (the `fragment add` recipes live there)
    const modules = collectModules(cell, ["workflows/", "lib/"]);
    for (const k of Object.keys(modules)) modules[k] = rewriteRelatives(modules[k], k);
    const rev = cell.getMeta("rev");
    const ep = await cell.loadCode(
      `wf:${name}:${wf.name}:${rev}`,
      WORKFLOW_MAIN.replaceAll("__WF__", wf.file),
      modules,
      { kind: "run", workflow: wf.name },
      cause,
    );
    // body IS the workflow input (WORKFLOW_MAIN passes req.json() to run)
    const resp = await ep.fetch("http://loaded/run", { method: "POST", body: JSON.stringify(input ?? null) });
    return await resp.json();
  } catch (e) {
    return { ok: false, error: String(e) };
  }
}
