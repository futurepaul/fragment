// fragment vault viewer — serves this folder as a live, browsable site.
//
// This file is code: it is frozen in whatever draft you bless. Your notes
// are data: with "liveFiles": true in the manifest, every read below sees
// the live working copy, so synced folder changes appear on reload without
// republishing. (Want a new viewer version? Edit, publish, bless.)
//
// Routes (all relative to the fragment root — works under /f/<name>/ and
// <name>.fragment.club alike):
//   /            the app shell (client does hash routing: #/notes/foo.md)
//   /assets/*    the bundled viewer (viewer.js + chunks + css)
//   /api/tree    JSON file listing (path, size, updatedAt, rev)
//   /api/file    raw file content (?path=…)

const MIME = {
  js: "text/javascript", mjs: "text/javascript", css: "text/css",
  json: "application/json", txt: "text/plain; charset=utf-8", md: "text/plain; charset=utf-8",
  markdown: "text/plain; charset=utf-8", csv: "text/csv", svg: "image/svg+xml",
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
  webp: "image/webp", ico: "image/x-icon", pdf: "application/pdf",
  woff2: "font/woff2", woff: "font/woff",
};

const junk = (p) =>
  p === "fragment.json" ||
  p.startsWith(".") || p.includes("/.") ||
  p.endsWith(".remote-") || p.includes(".remote-") ||
  p === "app.mjs" || p.startsWith("applib/") || p.startsWith("assets/") ||
  p.includes("DS_Store") || p.endsWith(".keep");

const resp = (body, type, cache) =>
  new Response(body, { headers: { "content-type": type, "cache-control": cache || "no-store" } });

// content-hash ETags: the browser already knows the hash from api/tree, so
// revisits validate with If-None-Match and get empty 304s
const etagOf = async (data) => {
  const buf = typeof data === "string" ? new TextEncoder().encode(data) : new Uint8Array(data);
  const h = await crypto.subtle.digest("SHA-256", buf);
  return '"' + [...new Uint8Array(h)].map((b) => b.toString(16).padStart(2, "0")).join("") + '"';
};
const maybe304 = (req, etag, cache) =>
  req.headers.get("if-none-match") === etag
    ? new Response(null, { status: 304, headers: { etag, "cache-control": cache } })
    : null;

const SHELL = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>vault</title>
<link rel="stylesheet" href="assets/viewer.css">
<script src="__rt.js"></script>
<script type="module" src="assets/viewer.js"></script>
</head>
<body>
<div id="side">
  <div id="brand">△ fragment vault</div>
  <input id="filter" type="search" placeholder="filter…" autocomplete="off">
  <nav id="tree" aria-label="files"></nav>
  <div id="recent-label">recent</div>
  <nav id="recent" aria-label="recently changed"></nav>
</div>
<main id="main"><div id="content"></div></main>
</body>
</html>`;

export default {
  async fetch(req, ctx) {
    const url = new URL(req.url);
    const path = decodeURIComponent(url.pathname).replace(/^\/+|\/+$/g, "");

    if (path === "api/tree") {
      const files = await ctx.files.index();
      // machinery: the fragment's own organs, pre-marked by the platform —
      // watchers (feeds, pollers) filter on this instead of guessing
      const body = JSON.stringify({
        files: files
          .filter((f) => !junk(f.path))
          .map((f) => ({ path: f.path, size: f.size, updatedAt: f.updatedAt, machinery: !!f.machinery })),
      });
      const cache = "private, max-age=15";
      const etag = await etagOf(body);
      return maybe304(req, etag, cache) || new Response(body, { headers: { "content-type": "application/json", "cache-control": cache, etag } });
    }

    if (path === "api/file") {
      const p = url.searchParams.get("path") || "";
      if (!p || p.includes("..") || p.startsWith("/")) return new Response('{"error":"bad path"}', { status: 400 });
      const ext = (p.match(/\.([a-z0-9]+)$/i) || [])[1]?.toLowerCase() || "";
      const cache = "private, max-age=60";
      try {
        if (["png", "jpg", "jpeg", "gif", "webp", "ico", "pdf", "woff2", "woff"].includes(ext)) {
          const b = await ctx.files.readBytes(p);
          const etag = await etagOf(b);
          return maybe304(req, etag, cache) || new Response(b, { headers: { "content-type": MIME[ext] || "application/octet-stream", "cache-control": cache, etag } });
        }
        const t = await ctx.files.read(p);
        const etag = await etagOf(t);
        return maybe304(req, etag, cache) || new Response(t, { headers: { "content-type": MIME[ext] || "text/plain; charset=utf-8", "cache-control": cache, etag } });
      } catch {
        return new Response(JSON.stringify({ error: "no such file" }), { status: 404 });
      }
    }

    if (path.startsWith("assets/")) {
      try {
        const t = await ctx.files.read(path);
        const ext = (path.match(/\.([a-z0-9]+)$/i) || [])[1]?.toLowerCase() || "";
        return resp(t, MIME[ext] || "application/octet-stream", "public, max-age=300");
      } catch {
        return new Response("no such asset", { status: 404 });
      }
    }

    // everything else is the app shell (client-side hash routing)
    return resp(SHELL, "text/html; charset=utf-8");
  },
};
