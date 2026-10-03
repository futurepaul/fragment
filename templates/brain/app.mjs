// The brain template's code (docs/cloudflare-v1.md, decision 30). A brain is
// a fragment app whose files are the knowledge, laid out as Finite Brain
// lays it out: each top-level folder a wiki (raw/ wiki/ inventory/
// datasets/ output/, index.md, log.md), and the brain's AGENTS.md
// (applib/guide.mjs) on how an agent keeps one. It runs from the platform's
// release (decision 40): a brain's repo holds only its face (fragment.json)
// and its files.
//
// Search is FTS5 over the brain's markdown, a section at a time (applib/
// sections.mjs), ranked by BM25, in this app's own SQLite: an index derived
// from the files and rebuilt from them. Each move of main runs `changed`
// (fragment.json's file trigger), a job: it marks the paths that moved
// (`mark`, a mutation, which open pages follow), then takes them in
// batches (`reindex`, a query that reads the files at main and writes the
// index) until none are left. A page whose bytes did not change keeps its
// sections, so the same files again change nothing. A search catches the
// index up a little first when a sync is still pending.
//
// The viewer (site/) is the notes template's, reading api/tree and
// api/file here, with a search box of the brain's own beside it.
import { DurableObject } from "cloudflare:workers";
import { GUIDE } from "./applib/guide.mjs";
import { QUERY_WORDS_MAX, SECTIONS_MAX, match, sections, stem, words } from "./applib/sections.mjs";

// The index's shape: a brain whose index is another rebuilds it from its files.
const FORMAT = "brain-sections-1";
// Files one reindex reads, and one search reads first when the index is behind.
const BATCH = 32;
const CATCH_UP = 8;
// A run's reindex steps (a job takes 256 steps; mark is one).
const REINDEX_CALLS_MAX = 200;
// The paths one mark takes (what a file trigger passes: past it, `more`).
const PATHS_MAX = 200;
// A page is read whole up to the platform's read limit; a larger one is a blob.
const PAGE_MAX_BYTES = 1024 * 1024;
// Past this, no new page is indexed: mutations keep 4 of the app's 16 MiB.
const INDEX_DB_MAX_BYTES = 12 * 1024 * 1024;
const RESULTS_DEFAULT = 10;
const RESULTS_MAX = 50;
const SNIPPET_MAX_BYTES = 300;
const SKIPPED_LISTED = 20;

// the fragment's own machinery, which is not the brain's knowledge
const hidden = (p) =>
  p === "fragment.json" || p === "app.mjs" || p.startsWith("site/") || p.startsWith("applib/") ||
  p.startsWith(".") || p.includes("/.") || p.includes(".conflict-");
// what search reads: markdown, but neither a wiki's instructions nor a made index
const indexable = (p) => /\.(md|markdown)$/i.test(p) && !hidden(p) && !/(^|\/)(AGENTS|_index)\.md$/i.test(p);
const wikiOf = (p) => (p.includes("/") ? p.slice(0, p.indexOf("/")) : "");

const MIME = {
  md: "text/plain; charset=utf-8", markdown: "text/plain; charset=utf-8", txt: "text/plain; charset=utf-8",
  json: "application/json", csv: "text/csv", svg: "image/svg+xml", png: "image/png", jpg: "image/jpeg",
  jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", pdf: "application/pdf", mp3: "audio/mpeg",
  wav: "audio/wav", mp4: "video/mp4", webm: "video/webm",
};
const mimeOf = (p) => MIME[(p.match(/\.([a-z0-9]+)$/i) || [])[1]?.toLowerCase()] || "application/octet-stream";

async function sha256(bytes) {
  return [...new Uint8Array(await crypto.subtle.digest("SHA-256", bytes))].map((b) => b.toString(16).padStart(2, "0")).join("");
}

// At most `max` bytes of `text`, cut at a whole character.
function clip(text, max) {
  const enc = new TextEncoder();
  if (enc.encode(text).length <= max) return text;
  let out = "";
  for (const ch of text) {
    if (enc.encode(out + ch + "…").length > max) break;
    out += ch;
  }
  return out + "…";
}

// Markdown text that reads as itself: a name from the brain's files, written into a made page.
const literal = (s) => String(s).replace(/[\\`*_[\]<>#|]/g, (c) => `\\${c}`);

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec("CREATE TABLE IF NOT EXISTS brain_state (k TEXT PRIMARY KEY, v TEXT NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS changes (id INTEGER PRIMARY KEY CHECK (id = 1), at INTEGER NOT NULL, paths TEXT NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS dirty (path TEXT PRIMARY KEY, gen INTEGER NOT NULL)");
    if (this.#get("format") !== FORMAT) {
      // a new shape (or none yet): drop what was, and rebuild it all from the files
      ctx.storage.transactionSync(() => {
        sql.exec("DROP TABLE IF EXISTS sections");
        sql.exec("DROP TABLE IF EXISTS pages");
        this.#set("format", FORMAT);
        this.#set("full", "1");
      });
    }
    sql.exec(`CREATE TABLE IF NOT EXISTS pages (
      path TEXT PRIMARY KEY, wiki TEXT NOT NULL, title TEXT NOT NULL, hash TEXT NOT NULL,
      bytes INTEGER NOT NULL, sections INTEGER NOT NULL, skipped TEXT, at INTEGER NOT NULL)`);
    sql.exec(`CREATE VIRTUAL TABLE IF NOT EXISTS sections USING fts5(
      path UNINDEXED, wiki UNINDEXED, location, title, ancestry, heading, body,
      tokenize = 'unicode61 remove_diacritics 2')`);
  }

  #get(k) {
    return this.ctx.storage.sql.exec("SELECT v FROM brain_state WHERE k = ?", k).toArray()[0]?.v ?? null;
  }

  #set(k, v) {
    this.ctx.storage.sql.exec("INSERT INTO brain_state (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v", k, String(v));
  }

  // A counter of the index's own: generations of marks, and pages written.
  #bump(k) {
    const n = Number(this.#get(k) ?? 0) + 1;
    this.#set(k, n);
    return n;
  }

  #pending() {
    return this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM dirty").one().n + (this.#get("full") === "1" ? 1 : 0);
  }

  // ---- the index, kept by the file trigger ----

  // The file trigger's run: mark what moved, then index it a batch at a time.
  async changed(input = {}, job) {
    const paths = (Array.isArray(input.paths) ? input.paths : []).filter((p) => typeof p === "string").slice(0, PATHS_MAX);
    const full = input.full === true || (Number.isSafeInteger(input.more) && input.more > 0);
    const marked = await job.call("mark", { paths, full });
    let last = { pending: 1 };
    let calls = 0;
    while (last.pending > 0 && calls < REINDEX_CALLS_MAX) {
      last = await job.call("reindex", {});
      calls++;
    }
    return { marked: marked.marked, full, calls, pending: last.pending };
  }

  // The paths that moved, each at a new generation (a reindex already reading
  // one writes nothing for it); `full` asks for every file to be read again.
  // Open pages re-run last_change after it, and refresh their tree.
  mark({ paths, full }) {
    const sql = this.ctx.storage.sql;
    const gen = this.#bump("gen");
    const mine = paths.filter(indexable);
    for (const p of mine) {
      sql.exec("INSERT INTO dirty (path, gen) VALUES (?, ?) ON CONFLICT (path) DO UPDATE SET gen = excluded.gen", p, gen);
    }
    if (full) this.#set("full", "1");
    sql.exec(
      "INSERT INTO changes (id, at, paths) VALUES (1, ?, ?) ON CONFLICT (id) DO UPDATE SET at = excluded.at, paths = excluded.paths",
      Date.now(),
      JSON.stringify(paths),
    );
    return { marked: mine.length, full, gen };
  }

  // One batch: the files marked, read at main, their sections written.
  async reindex({ full = false } = {}) {
    return this.#catchUp(BATCH, full === true);
  }

  async #catchUp(limit, full) {
    const sql = this.ctx.storage.sql;
    if (full || this.#get("full") === "1") await this.#markAll();
    const rows = sql.exec("SELECT path, gen FROM dirty ORDER BY path LIMIT ?", limit).toArray();
    const done = { indexed: 0, unchanged: 0, removed: 0, skipped: 0, deferred: 0 };
    for (const { path, gen } of rows) done[await this.#index(path, gen)]++;
    return { ...done, pending: this.#pending() };
  }

  // A full pass: every markdown file at main, and every page that left it.
  async #markAll() {
    const listed = (await this.files.list("")).map((f) => f.path).filter(indexable);
    const sql = this.ctx.storage.sql;
    this.ctx.storage.transactionSync(() => {
      const gen = this.#bump("gen");
      const have = new Set(listed);
      const gone = sql.exec("SELECT path FROM pages").toArray().map((r) => r.path).filter((p) => !have.has(p));
      for (const p of [...listed, ...gone]) {
        sql.exec("INSERT INTO dirty (path, gen) VALUES (?, ?) ON CONFLICT (path) DO UPDATE SET gen = excluded.gen", p, gen);
      }
      sql.exec("DELETE FROM brain_state WHERE k = 'full'");
    });
  }

  // One path, as main holds it now: its sections written when its bytes
  // changed, dropped when it is gone. Answers what happened to it.
  async #index(path, gen) {
    const sql = this.ctx.storage.sql;
    const file = (await this.files.list(path)).find((f) => f.path === path) ?? null;
    const large = file !== null && (file.blob === true || file.size > PAGE_MAX_BYTES);
    const bytes = file && !large ? await this.files.readBytes(path) : null;
    // what the page is now: its bytes' hash, or its size when it is too large to read
    const hash = file === null ? null : large ? `large:${file.size}` : await sha256(bytes);
    return this.ctx.storage.transactionSync(() => {
      // marked again while this read: the next pass reads it again
      if (sql.exec("SELECT gen FROM dirty WHERE path = ?", path).toArray()[0]?.gen !== gen) return "deferred";
      sql.exec("DELETE FROM dirty WHERE path = ?", path);
      const prior = sql.exec("SELECT hash, skipped FROM pages WHERE path = ?", path).toArray()[0] ?? null;
      if (prior?.hash === hash) return "unchanged";
      if (file === null) {
        if (prior === null) return "unchanged";
        sql.exec("DELETE FROM sections WHERE path = ?", path);
        sql.exec("DELETE FROM pages WHERE path = ?", path);
        this.#bump("writes");
        return "removed";
      }
      // a page changed is indexed again; past the index's size, no new one is
      const fresh = prior === null || prior.skipped !== null;
      const full = !large && fresh && sql.databaseSize > INDEX_DB_MAX_BYTES;
      sql.exec("DELETE FROM sections WHERE path = ?", path);
      const page = large || full ? { title: stem(path), sections: [], truncated: false } : sections(path, new TextDecoder().decode(bytes));
      const wiki = wikiOf(path);
      for (const s of page.sections) {
        sql.exec(
          "INSERT INTO sections (path, wiki, location, title, ancestry, heading, body) VALUES (?, ?, ?, ?, ?, ?, ?)",
          path, wiki, path, page.title, JSON.stringify(s.ancestry), s.heading ?? "", s.body,
        );
      }
      const why = large ? "1 MiB or more: a blob, not read" : full ? "the index is full" : page.truncated ? `only its first ${SECTIONS_MAX} sections` : null;
      sql.exec(
        `INSERT INTO pages (path, wiki, title, hash, bytes, sections, skipped, at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (path) DO UPDATE SET wiki = excluded.wiki, title = excluded.title, hash = excluded.hash,
           bytes = excluded.bytes, sections = excluded.sections, skipped = excluded.skipped, at = excluded.at`,
        // a page left out for room is tried again at its next pass
        path, wiki, page.title, full ? `full:${hash}` : hash, file.size, page.sections.length, why, Date.now(),
      );
      this.#bump("writes");
      return large || full ? "skipped" : "indexed";
    });
  }

  // ---- what agents and pages ask ----

  // Ranked sections for plain words: `{q, words, results: [{rank, path,
  // wiki, title, heading, ancestry, snippet}], pending}`.
  async search({ q, limit = RESULTS_DEFAULT, wiki = null }) {
    const ws = words(q ?? "");
    if (ws.length > QUERY_WORDS_MAX) throw new Error(`a search is at most ${QUERY_WORDS_MAX} words (this one is ${ws.length})`);
    let pending = this.#pending();
    if (pending > 0) {
      // the index is behind a sync: take a little of it first, and answer
      // from what is indexed if the files cannot be read now
      try {
        pending = (await this.#catchUp(CATCH_UP, false)).pending;
      } catch {
        pending = this.#pending();
      }
    }
    if (!ws.length) return { q, words: [], results: [], pending };
    const n = Math.min(Math.max(1, Math.trunc(limit)), RESULTS_MAX);
    // columns: path, wiki (not searched), location (the path), title,
    // ancestry, heading, body; the snippet is the body's, around the words
    const one = wiki === null ? "" : " AND wiki = ?";
    const args = wiki === null ? [match(ws), n] : [match(ws), wiki, n];
    const rows = this.ctx.storage.sql.exec(
      `SELECT path, wiki, title, ancestry, heading,
         snippet(sections, 6, '', '', '…', 24) AS snippet,
         bm25(sections, 0.0, 0.0, 2.0, 4.0, 2.0, 3.0, 1.0) AS score
       FROM sections WHERE sections MATCH ?${one}
       ORDER BY score LIMIT ?`,
      ...args,
    ).toArray();
    const results = rows.map((r, i) => ({
      rank: i + 1,
      path: r.path,
      wiki: r.wiki,
      title: r.title,
      heading: r.heading || null,
      ancestry: JSON.parse(r.ancestry),
      snippet: clip(String(r.snippet).replace(/\s+/g, " ").trim(), SNIPPET_MAX_BYTES),
    }));
    return { q, words: ws, results, pending };
  }

  // What the index holds: its pages and sections, what is pending, how
  // many pages it has written, and the pages it skipped.
  index_status() {
    const sql = this.ctx.storage.sql;
    const count = (q) => sql.exec(q).one().n;
    const skipped = sql.exec("SELECT path, skipped FROM pages WHERE skipped IS NOT NULL ORDER BY path LIMIT ?", SKIPPED_LISTED).toArray();
    return {
      format: FORMAT,
      pages: count("SELECT COUNT(*) AS n FROM pages"),
      sections: count("SELECT COUNT(*) AS n FROM sections"),
      pending: this.#pending(),
      writes: Number(this.#get("writes") ?? 0),
      skipped: skipped.map((r) => ({ path: r.path, why: r.skipped })),
    };
  }

  // The brain's AGENTS.md, for an agent with the CLI.
  guide() {
    return { text: GUIDE };
  }

  last_change() {
    const row = this.ctx.storage.sql.exec("SELECT at, paths FROM changes WHERE id = 1").toArray()[0];
    return row ? { at: row.at, paths: JSON.parse(row.paths) } : null;
  }

  // ---- the viewer's routes ----

  // The brain's title, from its face.
  async #title() {
    try {
      const m = JSON.parse((await this.files.read("fragment.json")) ?? "{}");
      return typeof m.meta?.title === "string" && m.meta.title ? m.meta.title : "Brain";
    } catch {
      return "Brain";
    }
  }

  // _index.md, made: the brain's wikis, each with its pages, and the guide.
  async #landing(files) {
    const wikis = new Map();
    for (const f of files) {
      const w = wikiOf(f.path);
      if (!w) continue;
      const e = wikis.get(w) ?? { pages: 0, sources: 0, index: false };
      if (/\.(md|markdown)$/i.test(f.path)) e.pages++;
      if (f.path.startsWith(`${w}/raw/`)) e.sources++;
      if (f.path === `${w}/index.md`) e.index = true;
      wikis.set(w, e);
    }
    const lines = [`# ${literal(await this.#title())}`, "", "A brain: each folder is a wiki, kept as [[AGENTS.md]] says. Search it with the box above.", ""];
    if (!wikis.size) lines.push("No wikis yet. An agent starts one by writing its index.md and log.md (see [[AGENTS.md]]).");
    else lines.push("## Wikis", "");
    for (const [w, e] of [...wikis].sort(([a], [b]) => a.localeCompare(b))) {
      const name = e.index ? `[[${w}/index.md|${literal(w)}]]` : literal(w);
      lines.push(`- ${name}: ${e.pages} page${e.pages === 1 ? "" : "s"}, ${e.sources} in raw/`);
    }
    return lines.join("\n") + "\n";
  }

  // api/tree: the brain's files at main, and the two it makes (its
  // _index.md, and the guide as AGENTS.md, each unless the brain holds one
  // of its own); api/file?path=: one of them (a blob's bytes come from the
  // platform's __file, beside this route).
  async fetch(request) {
    const url = new URL(request.url);
    const text = (body) => new Response(body, { headers: { "content-type": "text/plain; charset=utf-8", "cache-control": "no-store" } });
    if (url.pathname.endsWith("/api/tree")) {
      const files = (await this.files.list("")).filter((f) => !hidden(f.path));
      const made = [];
      if (!files.some((f) => f.path === "_index.md")) made.push({ path: "_index.md", size: 0, made: true });
      if (!files.some((f) => f.path === "AGENTS.md")) made.push({ path: "AGENTS.md", size: GUIDE.length, made: true });
      return Response.json({ files: [...made, ...files] }, { headers: { "cache-control": "no-store" } });
    }
    if (!url.pathname.endsWith("/api/file")) return new Response(`no route ${url.pathname}`, { status: 404 });
    const path = url.searchParams.get("path") || "";
    if (hidden(path)) return new Response("not a file of the brain", { status: 404 });
    const stat = (await this.files.list(path)).find((f) => f.path === path);
    if (!stat && path === "AGENTS.md") return text(GUIDE);
    if (!stat && path === "_index.md") return text(await this.#landing((await this.files.list("")).filter((f) => !hidden(f.path))));
    if (!stat) return new Response("no such file", { status: 404 });
    if (stat.blob) {
      const to = new URL("../__file", url);
      to.searchParams.set("path", path);
      const view = url.searchParams.get("view");
      if (view) to.searchParams.set("view", view);
      return Response.redirect(to.href, 302);
    }
    return new Response(await this.files.readBytes(path), {
      headers: { "content-type": mimeOf(path), "cache-control": "no-store", "x-content-type-options": "nosniff" },
    });
  }
}
