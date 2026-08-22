// Site serving: blessed drafts, draft previews, visibility (token/cookie/
// role), canonical URLs.
import { toAB, MIME, rankOf, isMachinery } from "./util.js";
import { safeEqual } from "./auth.js";
import { json } from "./util.js";
import { APP_MAIN } from "./loader.js";


// ------ canonicalUrl ------

export function canonicalUrl(cell, origin, name) {
  const sub = cell.env.FRAGMENT_SUBDOMAIN_HOST;
  if (sub) return `https://${encodeURIComponent(name)}.${sub}/`;
  return `${origin}/f/${name}/`;
}

// ------ serveRoute ------

export async function serveRoute(cell, request, url) {
  // /__serve/b/<rest>  → blessed draft; /__serve/d/<slug>/<rest>
  const parts = url.pathname.slice("/__serve/".length).split("/");
  const mode = parts.shift();
  let slug;
  let setCookie = null;
  if (mode === "b") {
    const m = cell.manifest();
    slug = cell.getMeta("blessed");
    if (!slug) return new Response(`fragment "${m?.name}" has no blessed draft yet — publish and bless one first.\n`, { status: 404 });
    const vis = cell.checkVisibility(request, url);
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
  const draft = cell.sql.exec("SELECT slug FROM drafts WHERE slug = ?", slug).toArray()[0];
  if (!draft) return new Response("no such draft\n", { status: 404 });

  // ---- machine-read plane: the tree and raw files, gated exactly like
  // the rendered site — so watchers, feeds and other fragments can read
  // content with a link instead of scraping HTML or holding an editor key
  if (rest === "__tree") {
    const rows = mode === "b"
      ? cell.sql.exec("SELECT path, length(content) AS size, updated_at, rev, sha256 FROM files WHERE deleted = 0 ORDER BY path").toArray()
      : cell.sql.exec("SELECT df.path, length(df.content) AS size, 0 AS updated_at, 0 AS rev, df.sha256 FROM draft_files df WHERE df.slug = ? ORDER BY df.path", slug).toArray();
    const files = rows
      .filter((r) => !isMachinery(r.path))
      .map((r) => ({ path: r.path, size: r.size, updatedAt: r.updated_at || null, rev: r.rev || 0, sha256: r.sha256 }));
    return json({ type: "tree", files, count: files.length });
  }
  if (rest.startsWith("__file")) {
    const fPath = new URL(request.url).searchParams.get("path") || "";
    if (!fPath || fPath.includes("..") || fPath.startsWith("/") || isMachinery(fPath)) {
      return json({ error: "bad path" }, 400);
    }
    const row = mode === "b"
      ? cell.sql.exec("SELECT content FROM files WHERE path = ? AND deleted = 0", fPath).toArray()[0]
      : cell.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = ?", slug, fPath).toArray()[0];
    if (!row) return json({ error: "no such file" }, 404);
    return new Response(toAB(row.content), { headers: { "content-type": "application/octet-stream", "cache-control": "no-store" } });
  }

  // dynamic app
  const appRow = cell.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = 'app.mjs'", slug).toArray()[0];
  if (appRow) {
    const modules = {};
    const libRows = cell.sql.exec("SELECT path, content FROM draft_files WHERE slug = ? AND path LIKE 'applib/%'", slug).toArray();
    for (const r of libRows) modules[r.path] = new TextDecoder().decode(toAB(r.content));
    modules["app.mjs"] = new TextDecoder().decode(toAB(appRow.content));
    // the loader id carries the mode: blessing a slug creates a fresh worker
// (a preview-cached one would keep its short-lived preview token forever)
    const ep = await cell.loadCode(`app:${mode}:${slug}`, APP_MAIN, modules, { kind: "draft", worker: "app", slug, blessed: mode === "b" });
    // the public path the visitor used rides on x-fragment-url (set by the
    // router and forwarded here) — apps that care read it; url.pathname
    // stays the stable internal form so blessed drafts never break
    const appUrl = new URL(request.url);
    return stamp(await ep.fetch(new Request(appUrl.origin + "/" + rest + appUrl.search, request)));
  }

  // static from site/
  let rel = rest === "" ? "index.html" : rest;
  let row = cell.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = ?", slug, "site/" + rel).toArray()[0];
  if (!row && !rel.endsWith("/")) row = cell.sql.exec("SELECT content FROM draft_files WHERE slug = ? AND path = ?", slug, "site/" + rel + "/index.html").toArray()[0];
  if (!row) return new Response("not found", { status: 404 });
  const ext = (rel.match(/\.([a-z0-9]+)$/) || [])[1] || "";
  const cache = mode === "b" ? "no-store" : "public, max-age=3600, immutable";
  return stamp(new Response(toAB(row.content), { headers: { "content-type": MIME[ext] || "application/octet-stream", "cache-control": cache } }));
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
  if (m.visibility === "token") {
    if (okToken) return { ok: true, setCookie };
    const role = cell.roleOf(request.headers.get("x-fragment-pubkey"));
    if (rankOf(role) >= 1) return { ok: true };
    return { ok: false, status: 403, error: "cell fragment needs its ?view= link token" };
  }
  // viewers
  const role = cell.roleOf(request.headers.get("x-fragment-pubkey"));
  if (rankOf(role) >= 1) return { ok: true };
  return { ok: false, status: request.headers.get("x-fragment-pubkey") ? 403 : 401, error: "cell fragment is restricted to listed viewers (NIP-98)" };
}
