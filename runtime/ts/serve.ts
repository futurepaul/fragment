// Site serving from the LIVE ref: the blessed serve point is code.storage
// branch `live` (wire contract: "serves blessed content from live@SHA").
// Preview/rollback are ref moves made by the CLI (ephemeral refs /
// reset-branch), not cell state — the draft/snapshot machinery is deleted.
// All bytes stream through the git plane (RAM LRU or pass-through); the
// one whole-body decode is OG-tag injection, bounded by ceiling.
import { MIME, rankOf, isMachinery, serveCacheControl, mimeForPath } from "./util.js";
import { safeEqual } from "./auth.js";
import { json } from "./util.js";
import { APP_MAIN } from "./loader.js";
import { pushSubStore, pushUnsubStore, ensurePushTable, pushVapidFor } from "./internal.js";
import { SW_CLIENT_SOURCE } from "./sw-client.js";
import { ensurePins, pinOf, treeList, readFileStream, readFileTextAt, repoOf } from "./git-plane.js";

// Whole-body budget for the one template materializer that must see text
// (OG-tag injection into a site's index.html). Pages are documents; anything
// over 1MiB of HTML renders un-enriched rather than paying an isolate-sized
// decode for it.
const OG_MATERIALIZE_CEILING = 1024 * 1024;


// ------ canonicalUrl ------

const esc = (s) => String(s || "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;");

export function canonicalUrl(cell, origin, name) {
  const sub = cell.env.FRAGMENT_SUBDOMAIN_HOST;
  if (sub) return `https://${encodeURIComponent(name)}.${sub}/`;
  return `${origin}/f/${name}/`;
}

// ------ serveRoute ------

export async function serveRoute(cell, request, url) {
  const parts = url.pathname.slice("/__serve/".length).split("/");
  const mode = parts.shift();
  if (mode !== "b") return new Response("not found\n", { status: 404 });
  const m = cell.manifest();
  const rest = parts.join("/");

  // fill the pins on first touch so a fresh cell serves without waiting
  // for a webhook
  await ensurePins(cell);
  const pin = pinOf(cell, "live");
  if (!pin) {
    return new Response(`fragment "${m?.name}" has no live ref yet — deploy first (move the live ref in code.storage).\n`, { status: 404 });
  }

  const vis = cell.checkVisibility(request, url);
  if (!vis.ok) return new Response(vis.error + "\n", { status: vis.status });
  const stamp = (r) => {
    if (!vis.setCookie) return r;
    const h = new Headers(r.headers);
    h.append("set-cookie", vis.setCookie);
    return new Response(r.body, { status: r.status, headers: h });
  };

  // live tree row lookup at the pinned live ref
  const liveRow = (p) => cell.sql.exec("SELECT size, mode, last_commit_sha FROM git_tree WHERE ref = 'live' AND path = ?", p).toArray()[0] || null;

  // ---- machine-read plane: the tree and raw files, gated exactly like
  // the rendered site — so watchers, feeds and other fragments can read
  // content with a link instead of scraping HTML or holding an editor key
  if (rest === "__preview.svg") {
    let h = 0;
    for (const c of (cell.getMeta("name") || "fragment")) h = (h * 31 + c.charCodeAt(0)) >>> 0;
    const hue = h % 360;
    const letter = (cell.getMeta("name") || "f").charAt(0).toUpperCase();
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${hue},65%,22%)"/><stop offset="1" stop-color="hsl(${(hue + 60) % 360},70%,38%)"/></linearGradient></defs><rect width="1200" height="630" fill="url(#g)"/><circle cx="1050" cy="90" r="220" fill="hsl(${(hue + 120) % 360},60%,45%)" opacity="0.25"/><text x="80" y="330" font-family="Georgia,serif" font-size="260" fill="hsl(${hue},30%,92%)">${letter}</text><text x="84" y="520" font-family="ui-monospace,Menlo,monospace" font-size="44" fill="hsl(${hue},25%,80%)">${cell.getMeta("name") || ""}</text><text x="84" y="572" font-family="ui-monospace,Menlo,monospace" font-size="26" fill="hsl(${hue},20%,65%)">fragment.club</text></svg>`;
    return new Response(svg, { headers: { "content-type": "image/svg+xml", "cache-control": "public, max-age=3600" } });
  }
  if (rest === "__tree") {
    const rows = treeList(cell, "live");
    const files = rows
      .filter((r) => !isMachinery(r.path))
      .map((r) => ({ path: r.path, size: r.size, mode: r.mode, lastCommitSha: r.last_commit_sha }));
    return json({ type: "tree", ref: "live", sha: pin, files, count: files.length });
  }
  if (rest.startsWith("__file")) {
    const fPath = new URL(request.url).searchParams.get("path") || "";
    if (!fPath || fPath.includes("..") || fPath.startsWith("/") || isMachinery(fPath)) {
      return json({ error: "bad path" }, 400);
    }
    // blessed content first; the working copy (main) is the fallback — the
    // same "code frozen at live, data flows" semantics a default app's
    // ctx.files reads have. Generated media (gen/) lands on main and serves
    // here without waiting for the next deploy; hash-named site assets hit
    // the live row and never notice.
    const row = liveRow(fPath) || cell.sql.exec("SELECT size, mode, last_commit_sha FROM git_tree WHERE ref = 'main' AND path = ?", fPath).toArray()[0] || null;
    if (!row) return json({ error: "no such file" }, 404);
    const upstream = await readFileStream(cell, fPath, liveRow(fPath) ? "live" : "main");
    return stamp(new Response(upstream.body, { status: upstream.status, headers: {
      "content-type": mimeForPath(fPath) || "application/octet-stream",
      // the path's content can change when live moves (or, for main-fallback
      // rows, on any commit); only hash-named files are immutable
      "cache-control": "no-store",
    } }));
  }

  // ---- platform notify/push machinery: reserved names served exactly
  // like the router's __rt.js (no-store, version-stamped)
  if (rest === "__sw.js") {
    return new Response(`/* fragment sw-client v1 */\n` + SW_CLIENT_SOURCE, {
      headers: {
        "content-type": "text/javascript; charset=utf-8",
        "cache-control": "no-store",
        "x-fragment-sw-version": "1",
      },
    });
  }
  if (rest === "__push-key") {
    const keys = await pushVapidFor(cell);
    return json({ key: keys.pubRaw });
  }
  if (rest === "__push-sub" || rest === "__push-unsub") {
    if (request.method !== "POST") return json({ error: "POST only" }, 405);
    ensurePushTable(cell);
    const body = await request.json().catch(() => null);
    if (!body) return json({ error: "body required" }, 400);
    return rest === "__push-sub" ? pushSubStore(cell, body) : pushUnsubStore(cell, body);
  }

  // static from site/ — the fragment's browser-asset plane, reserved ahead
  // of app dispatch: browsers don't attach ?view= to subresource fetches
  // (module imports, css, img), so clean hash-named paths are how an
  // app-bearing fragment serves those assets. An exact site/index.html
  // serves the root too — the normal static+API hosting shape. An app
  // with no site/index.html keeps the root (single-handler shape).
  const appMeta = liveRow("app.mjs");
  const stMeta = (p) => cell.sql.exec("SELECT size, mode, last_commit_sha FROM git_tree WHERE ref = 'live' AND path = ?", p).toArray()[0] || null;
  const siteOwnsRoot = !!appMeta && !!stMeta("site/index.html");
  if (rest !== "" || !appMeta || siteOwnsRoot) {
    let rel = rest === "" ? "index.html" : rest;
    let meta = stMeta("site/" + rel);
    if (!meta && !rel.endsWith("/")) meta = stMeta("site/" + rel + "/index.html");
    if (meta) {
      const ext = (rel.match(/\.([a-z0-9]+)$/) || [])[1] || "";
      const mime = MIME[ext] || "application/octet-stream";
      const cache = serveCacheControl(true, rel);
      const m2 = cell.manifest();
      const wantsOg = (mime || "").includes("text/html") && !!m2?.meta && rel === "index.html";
      let ogHtml: string | null = null;
      if (wantsOg && (meta.size | 0) <= OG_MATERIALIZE_CEILING) {
        // template materializer: the ONLY static path allowed a whole-body
        // decode, bounded above so a mislabeled huge file can't blow the heap
        try { ogHtml = await readFileTextAtLive(cell, "site/" + rel); } catch { ogHtml = null; }
      }
      if (ogHtml !== null && !ogHtml.includes("og:title")) {
        // social preview injection: og tags from manifest meta unless the page
        // brings its own; image falls back to the generated placeholder
        const pubOrigin = new URL(request.headers.get("x-fragment-url") || request.url).origin;
        const img = m2.meta.image || `${pubOrigin}/f/${m2.name}/__preview.svg`;
        const tags = [
          `<meta property="og:title" content="${esc(m2.meta.title || m2.name)}">`,
          `<meta property="og:description" content="${esc(m2.meta.description || "")}">`,
          `<meta property="og:image" content="${esc(img)}">`,
          `<meta name="twitter:card" content="summary_large_image">`,
          `<title>${esc(m2.meta.title || m2.name)}</title>`,
        ].join("");
        ogHtml = ogHtml.includes("<head>") ? ogHtml.replace("<head>", "<head>" + tags) : tags + ogHtml;
        return stamp(new Response(new TextEncoder().encode(ogHtml), { status: 200, headers: { "content-type": mime, "cache-control": cache } }));
      }
      if (ogHtml !== null) {
        // decoded for injection eligibility but page carries its own OG tags:
        // re-serve the exact bytes we read rather than a re-encoded copy
        return stamp(new Response(ogHtml, { status: 200, headers: { "content-type": mime, "cache-control": cache } }));
      }
      // everything else streams untouched through the git plane
      const upstream = await readFileStream(cell, "site/" + rel, "live");
      return stamp(new Response(upstream.body, { status: upstream.status, headers: { "content-type": mime, "cache-control": cache } }));
    }
  }

  // dynamic app — code materializes from the live pin through bounded
  // text reads; module counts are human-scaled and cold loads are rare
  // (worker cache keyed on the pin)
  if (appMeta) {
    const modules = {};
    for (const r of treeList(cell, "live", "applib/")) {
      modules[r.path] = await readFileTextAtLive(cell, r.path);
    }
    modules["app.mjs"] = await readFileTextAtLive(cell, "app.mjs");
    // the loader id carries the pin: moving live (a new deploy) creates a
    // fresh worker; a stable pin keeps the cached one (and its token)
    const ep = await cell.loadCode(`app:live:${pin}`, APP_MAIN, modules, { kind: "draft", worker: "app", slug: pin, blessed: true });
    const appUrl = new URL(request.url);
    return stamp(await ep.fetch(new Request(appUrl.origin + "/" + rest + appUrl.search, request)));
  }

  return new Response("not found", { status: 404 });
}

// bounded text read pinned at the live ref (module materializer); used by
// serve.ts and rooms.ts, whose served code comes from the live tree
export async function readFileTextAtLive(cell, path: string): Promise<string> {
  const pin = pinOf(cell, "live");
  const row = cell.sql.exec("SELECT size FROM git_tree WHERE ref = 'live' AND path = ?", path).toArray()[0];
  if (!row) throw new Error(`no such live file: ${path}`);
  return (await readFileTextAt(cell, repoOf(cell), path, pin || "", `module ${path}`)) as string;
}

// ------ checkVisibility ------

export function checkVisibility(cell, request, url) {
  const m = cell.manifest();
  if (m.visibility === "public") return { ok: true };
  // a valid ?view= upgrade: browsers don't attach query strings to
  // subresource fetches (module imports, css, img), so mint a scoped
  // cookie on the first token hit and accept it thereafter.
  const token = cell.getMeta("view_token");
  const ck = `fragview_${m.name}`;
  const cookies = Object.fromEntries(
    (request.headers.get("cookie") || "").split(";").map((c) => c.split("=").map((s) => s.trim())).filter((p) => p.length === 2)
  );
  const viaUrl = safeEqual(url.searchParams.get("view") || "", token);
  const viaCookie = safeEqual(cookies[ck] || "", token);
  const okToken = viaUrl || viaCookie;
  const setCookie = viaUrl
    ? `${ck}=${token}; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax`
    : null;
  if (m.visibility === "link") {
    if (okToken) return { ok: true, setCookie };
    const role = cell.roleOf(request.headers.get("x-fragment-pubkey"));
    if (rankOf(role) >= 1) return { ok: true };
    return { ok: false, status: 403, error: "this fragment is link-only — open it with its share link (?view=)" };
  }
  // viewers
  const role = cell.roleOf(request.headers.get("x-fragment-pubkey"));
  if (rankOf(role) >= 1) return { ok: true };
  return { ok: false, status: request.headers.get("x-fragment-pubkey") ? 403 : 401, error: "cell fragment is restricted to listed viewers (NIP-98)" };
}
