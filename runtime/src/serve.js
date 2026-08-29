// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
import { MIME, rankOf, isMachinery, serveCacheControl, mimeForPath } from "./util.js";
import { safeEqual } from "./auth.js";
import { json } from "./util.js";
import { APP_MAIN } from "./loader.js";
import { tierStreamByHash, tierTextBounded, publicRedirectTarget } from "./blob-tier.js";
import { pushSubStore, pushUnsubStore, ensurePushTable, pushVapidFor } from "./internal.js";
import { SW_CLIENT_SOURCE } from "./sw-client.js";
const OG_MATERIALIZE_CEILING = 1024 * 1024;
const esc = (s) => String(s || "").replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/"/g, "&quot;");
function canonicalUrl(cell, origin, name) {
  const sub = cell.env.FRAGMENT_SUBDOMAIN_HOST;
  if (sub) return `https://${encodeURIComponent(name)}.${sub}/`;
  return `${origin}/f/${name}/`;
}
async function serveRoute(cell, request, url) {
  const parts = url.pathname.slice("/__serve/".length).split("/");
  const mode = parts.shift();
  let slug;
  let setCookie = null;
  if (mode === "b") {
    const m = cell.manifest();
    slug = cell.getMeta("blessed");
    if (!slug) return new Response(`fragment "${m?.name}" has no blessed draft yet \u2014 publish and bless one first.
`, { status: 404 });
    const vis = cell.checkVisibility(request, url);
    if (!vis.ok) return new Response(vis.error + "\n", { status: vis.status });
    setCookie = vis.setCookie;
  } else {
    slug = parts.shift();
  }
  const stamp = (r) => {
    if (!setCookie) return r;
    const h = new Headers(r.headers);
    h.append("set-cookie", setCookie);
    return new Response(r.body, { status: r.status, headers: h });
  };
  const rest = parts.join("/");
  const draft = cell.sql.exec("SELECT slug FROM drafts WHERE slug = ?", slug).toArray()[0];
  if (!draft) return new Response("no such draft\n", { status: 404 });
  if (rest === "__preview.svg") {
    let h = 0;
    for (const c of cell.getMeta("name") || "fragment") h = h * 31 + c.charCodeAt(0) >>> 0;
    const hue = h % 360;
    const letter = (cell.getMeta("name") || "f").charAt(0).toUpperCase();
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630"><defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="hsl(${hue},65%,22%)"/><stop offset="1" stop-color="hsl(${(hue + 60) % 360},70%,38%)"/></linearGradient></defs><rect width="1200" height="630" fill="url(#g)"/><circle cx="1050" cy="90" r="220" fill="hsl(${(hue + 120) % 360},60%,45%)" opacity="0.25"/><text x="80" y="330" font-family="Georgia,serif" font-size="260" fill="hsl(${hue},30%,92%)">${letter}</text><text x="84" y="520" font-family="ui-monospace,Menlo,monospace" font-size="44" fill="hsl(${hue},25%,80%)">${cell.getMeta("name") || ""}</text><text x="84" y="572" font-family="ui-monospace,Menlo,monospace" font-size="26" fill="hsl(${hue},20%,65%)">fragment.club</text></svg>`;
    return new Response(svg, { headers: { "content-type": "image/svg+xml", "cache-control": "public, max-age=3600" } });
  }
  if (rest === "__tree") {
    const rows = mode === "b" ? cell.sql.exec("SELECT path, size, updated_at, rev, sha256 FROM files WHERE deleted = 0 ORDER BY path").toArray() : cell.sql.exec("SELECT path, size, updated_at, rev, sha256 FROM draft_files WHERE slug = ? AND deleted = 0 ORDER BY path", slug).toArray();
    const files = rows.filter((r) => !isMachinery(r.path)).map((r) => ({ path: r.path, size: r.size, updatedAt: r.updated_at || null, rev: r.rev || 0, sha256: r.sha256 }));
    return json({ type: "tree", files, count: files.length });
  }
  if (rest.startsWith("__file")) {
    const fPath = new URL(request.url).searchParams.get("path") || "";
    if (!fPath || fPath.includes("..") || fPath.startsWith("/") || isMachinery(fPath)) {
      return json({ error: "bad path" }, 400);
    }
    const row = mode === "b" ? cell.getFileMeta(fPath) : cell.sql.exec("SELECT sha256, size, mime FROM draft_files WHERE slug = ? AND path = ? AND deleted = 0", slug, fPath).toArray()[0];
    if (!row) return json({ error: "no such file" }, 404);
    const mime = row.mime || mimeForPath(fPath) || "application/octet-stream";
    const redirectBase = publicRedirectTarget(cell, mode === "b");
    if (redirectBase) return stamp(new Response(null, { status: 302, headers: {
      location: `${redirectBase}/${row.sha256}`,
      // public-GET bytes are unauthenticated by design; the ACAO header lets
      // a page's fetch() follow the cross-origin redirect (browsers require
      // CORS on every hop of a cors-mode redirect)
      "access-control-allow-origin": "*",
      // the path->hash mapping is mutable; only the blob itself is immutable.
      // A cached 302 would keep serving a stale file after an edit.
      "cache-control": "no-store"
    } }));
    const upstream = await tierStreamByHash(cell, row.sha256);
    return new Response(upstream.body, { status: upstream.status, headers: {
      "content-type": mime,
      "cache-control": "no-store"
    } });
  }
  if (rest === "__sw.js") {
    return new Response(`/* fragment sw-client v1 */
` + SW_CLIENT_SOURCE, {
      headers: {
        "content-type": "text/javascript; charset=utf-8",
        "cache-control": "no-store",
        "x-fragment-sw-version": "1"
      }
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
  const appMeta = cell.sql.exec("SELECT sha256, size, mime FROM draft_files WHERE slug = ? AND path = 'app.mjs'", slug).toArray()[0];
  const stMeta = (p) => cell.sql.exec("SELECT sha256, size, mime FROM draft_files WHERE slug = ? AND path = ? AND deleted = 0", slug, p).toArray()[0];
  const siteOwnsRoot = !!appMeta && !!stMeta("site/index.html");
  if (rest !== "" || !appMeta || siteOwnsRoot) {
    let rel = rest === "" ? "index.html" : rest;
    let meta = stMeta("site/" + rel);
    if (!meta && !rel.endsWith("/")) meta = stMeta("site/" + rel + "/index.html");
    if (meta) {
      const ext = (rel.match(/\.([a-z0-9]+)$/) || [])[1] || "";
      const mime = meta.mime || MIME[ext] || "application/octet-stream";
      const cache = serveCacheControl(mode === "b", rel);
      const m2 = cell.manifest();
      const wantsOg = (mime || "").includes("text/html") && !!m2?.meta && rel === "index.html";
      let ogHtml = null;
      if (wantsOg && (meta.size | 0) <= OG_MATERIALIZE_CEILING) {
        try {
          ogHtml = await tierTextBounded(cell, meta, `page ${rel}`);
        } catch {
          ogHtml = null;
        }
      }
      if (ogHtml !== null && !ogHtml.includes("og:title")) {
        const pubOrigin = new URL(request.headers.get("x-fragment-url") || request.url).origin;
        const img = m2.meta.image || `${pubOrigin}/f/${m2.name}/__preview.svg`;
        const tags = [
          `<meta property="og:title" content="${esc(m2.meta.title || m2.name)}">`,
          `<meta property="og:description" content="${esc(m2.meta.description || "")}">`,
          `<meta property="og:image" content="${esc(img)}">`,
          `<meta name="twitter:card" content="summary_large_image">`,
          `<title>${esc(m2.meta.title || m2.name)}</title>`
        ].join("");
        ogHtml = ogHtml.includes("<head>") ? ogHtml.replace("<head>", "<head>" + tags) : tags + ogHtml;
        return stamp(new Response(new TextEncoder().encode(ogHtml), { status: 200, headers: { "content-type": mime, "cache-control": cache } }));
      }
      if (ogHtml !== null) {
        return stamp(new Response(ogHtml, { status: 200, headers: { "content-type": mime, "cache-control": cache } }));
      }
      const upstream = await tierStreamByHash(cell, meta.sha256);
      return stamp(new Response(upstream.body, { status: upstream.status, headers: { "content-type": mime, "cache-control": cache } }));
    }
  }
  if (appMeta) {
    const modules = {};
    const libRows = cell.sql.exec("SELECT path, sha256, size FROM draft_files WHERE slug = ? AND path LIKE 'applib/%'", slug).toArray();
    for (const r of libRows) modules[r.path] = await tierTextBounded(cell, r, `module ${r.path}`);
    modules["app.mjs"] = await tierTextBounded(cell, appMeta, "module app.mjs");
    const ep = await cell.loadCode(`app:${mode}:${slug}`, APP_MAIN, modules, { kind: "draft", worker: "app", slug, blessed: mode === "b" });
    const appUrl = new URL(request.url);
    return stamp(await ep.fetch(new Request(appUrl.origin + "/" + rest + appUrl.search, request)));
  }
  return new Response("not found", { status: 404 });
}
function checkVisibility(cell, request, url) {
  const m = cell.manifest();
  if (m.visibility === "public") return { ok: true };
  const token = cell.getMeta("view_token");
  const ck = `fragview_${m.name}`;
  const cookies = Object.fromEntries(
    (request.headers.get("cookie") || "").split(";").map((c) => c.split("=").map((s) => s.trim())).filter((p) => p.length === 2)
  );
  const viaUrl = safeEqual(url.searchParams.get("view") || "", token);
  const viaCookie = safeEqual(cookies[ck] || "", token);
  const okToken = viaUrl || viaCookie;
  const setCookie = viaUrl ? `${ck}=${token}; Path=/; Max-Age=604800; HttpOnly; SameSite=Lax` : null;
  if (m.visibility === "link") {
    if (okToken) return { ok: true, setCookie };
    const role2 = cell.roleOf(request.headers.get("x-fragment-pubkey"));
    if (rankOf(role2) >= 1) return { ok: true };
    return { ok: false, status: 403, error: "this fragment is link-only \u2014 open it with its share link (?view=)" };
  }
  const role = cell.roleOf(request.headers.get("x-fragment-pubkey"));
  if (rankOf(role) >= 1) return { ok: true };
  return { ok: false, status: request.headers.get("x-fragment-pubkey") ? 403 : 401, error: "cell fragment is restricted to listed viewers (NIP-98)" };
}
export {
  canonicalUrl,
  checkVisibility,
  serveRoute
};
