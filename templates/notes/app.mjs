// A folder of notes as a live site. The files are the state: they live in
// git on `main`, edited by syncing a folder (`fragment sync --watch`) or by
// anything else that commits. The viewer in site/ reads them through the
// routes below, and a file trigger tells open pages when main moves.
import { DurableObject } from "cloudflare:workers";

// the fragment's own machinery, hidden from the notes tree
const hidden = (p) =>
  p === "fragment.json" || p === "app.mjs" || p.startsWith("site/") || p.startsWith("applib/") ||
  p.startsWith(".") || p.includes("/.") || p.includes(".conflict-");

const MIME = {
  md: "text/plain; charset=utf-8", markdown: "text/plain; charset=utf-8", txt: "text/plain; charset=utf-8",
  json: "application/json", csv: "text/csv", svg: "image/svg+xml", png: "image/png", jpg: "image/jpeg",
  jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", pdf: "application/pdf",
};

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS changes (id INTEGER PRIMARY KEY CHECK (id = 1), at INTEGER NOT NULL, paths TEXT NOT NULL)");
  }

  // The file trigger: main moved. Live queries re-run after every mutation,
  // so open pages see this at once.
  changed({ paths }) {
    this.ctx.storage.sql.exec(
      "INSERT INTO changes (id, at, paths) VALUES (1, ?, ?) ON CONFLICT (id) DO UPDATE SET at = excluded.at, paths = excluded.paths",
      Date.now(),
      JSON.stringify(paths ?? []),
    );
    return { ok: true };
  }

  last_change() {
    const row = this.ctx.storage.sql.exec("SELECT at, paths FROM changes WHERE id = 1").toArray()[0];
    return row ? { at: row.at, paths: JSON.parse(row.paths) } : null;
  }

  // api/tree: the notes; api/file?path=: one note's bytes (at main)
  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/api/tree")) {
      const files = (await this.files.list()).filter((f) => !hidden(f.path));
      return Response.json({ files }, { headers: { "cache-control": "no-store" } });
    }
    if (url.pathname.endsWith("/api/file")) {
      const path = url.searchParams.get("path") || "";
      if (hidden(path)) return new Response("not a note", { status: 404 });
      const stat = (await this.files.list(path)).find((f) => f.path === path);
      if (!stat) return new Response("no such note", { status: 404 });
      // a large file (a blob) is served by the platform itself
      if (stat.blob) return Response.redirect(new URL(`/__file?path=${encodeURIComponent(path)}`, url), 302);
      const ext = (path.match(/\.([a-z0-9]+)$/i) || [])[1]?.toLowerCase() || "";
      return new Response(await this.files.readBytes(path), {
        headers: { "content-type": MIME[ext] || "application/octet-stream", "cache-control": "no-store" },
      });
    }
    return new Response("not found", { status: 404 });
  }
}
