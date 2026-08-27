// Worker Loader machinery: mint/check run tokens, load workflow/app/
// rooms code, and execute one workflow attempt in a loader isolate.
// Lifecycle (guards, retries, held) lives in runs.ts; this module only
// loads and executes.
import { json, randHex } from "./util.js";
import { CTX_SHIM_SOURCE } from "./ctx-shim.js";
import { tierTextBounded } from "./blob-tier.js";

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
  // workflow runs live in CACHED isolates (stable code-hash ids), which
  // outlive any ttl — a cached worker with an expired token 403s its own
  // ctx calls — so run tokens live until superseded (like blessed apps).
  // Draft previews are rehearsals (1h); a blessed app lives until
  // superseded (revoked at rebless, swept on restart re-mint).
  const ttl = scope.kind === "run" || (scope.kind === "draft" && scope.blessed) ? null : 3600_000;
  cell.sql.exec("DELETE FROM run_tokens WHERE expires < ?", Date.now() - 24 * 3600_000);
  if (scope.kind === "draft") {
    // one live token per (worker kind, slug, blessed-ness): re-mints sweep
    // ONLY their predecessor. Keying on (slug, blessed) alone let an app
    // re-mint delete a live rooms worker's token (and vice versa) — found
    // live: a comments feature 403'd for 15 minutes after room activity.
    for (const r of cell.sql.exec("SELECT token, scope FROM run_tokens").toArray()) {
      try {
        const s = JSON.parse(r.scope);
        if (s.kind === "draft" && s.worker === scope.worker && s.slug === scope.slug && !!s.blessed === !!scope.blessed) {
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
      ...((cell.manifest()?.secrets || []).length && scope.kind === "draft" ? { FRAGMENT_EAGER_SECRETS: "1" } : {}),
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

// Module source lives in the blob tier now (rows are name->hash); a cold
// code load pays one loopback fetch per module. Bounded by READ_CEILING per
// file inside tierTextBounded. Sequential by choice: cold loads are rare
// (worker cache keyed on the code hash), and the count of workflow/lib
// modules is small and human-scaled.
export async function collectModules(cell, prefixes: string | string[]) {
  const list = Array.isArray(prefixes) ? prefixes : [prefixes];
  const modules: Record<string, string> = {};
  for (const prefix of list) {
    const rows = cell.sql.exec("SELECT path, sha256, size FROM files WHERE path LIKE ? AND deleted = 0", prefix + "%").toArray();
    for (const r of rows) modules[r.path] = await tierTextBounded(cell, r, `module ${r.path}`);
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
    const src = await cell.getFileText(wf.file);
    if (src === null) throw new Error(`workflow file not found in folder: ${wf.file}`);
    // workflows/ plus lib/ (the `fragment add` recipes live there)
    const modules = await collectModules(cell, ["workflows/", "lib/"]);
    for (const k of Object.keys(modules)) modules[k] = rewriteRelatives(modules[k], k);
    // stable worker id, keyed on the workflow CODE, not the folder rev: the
    // rev bumps on every file write — including the workflow's own output —
    // so a rev-keyed id loads a fresh isolate per run and write-heavy cron
    // workflows saturate the loader (256 workers; found live: the relay
    // news bot churned held runs against it). The isolate now reloads only
    // when the code actually changes.
    const codeHash = await crypto.subtle.digest("SHA-256",
      new TextEncoder().encode(JSON.stringify([wf.file, modules])));
    const hash = [...new Uint8Array(codeHash)].map((b) => b.toString(16).padStart(2, "0")).join("").slice(0, 16);
    const ep = await cell.loadCode(
      `wf:${name}:${wf.name}:${hash}`,
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
