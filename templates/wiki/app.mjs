// Wiki: a team's wiki whose pages are the markdown files under `wiki/`.
// The files are the state: an edit on the page is a commit to `main`
// (`save`), and so is a folder synced by a person or an agent
// (`fragment sync --watch`), or `fragment write`. A file trigger runs
// `changed` on every move of `main` that touches `wiki/`, so open pages
// follow, and Recent changes keeps who changed what (a page's editor,
// or "the folder") and in which commit.
import { DurableObject } from "cloudflare:workers";

const PAGES_MAX = 1000;
const CHANGES_KEPT = 500;

// a page's path: a markdown file under wiki/, no dot files, no climbing out
const isPage = (p) => typeof p === "string" && /^wiki\/.+\.md$/.test(p) && !p.split("/").some((s) => s === "" || s.startsWith("."));
function mustBePage(path) {
  if (!isPage(path) || new TextEncoder().encode(path).length > 300) throw new Error("a page is a .md file under wiki/");
}

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS changes (
      id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL, by TEXT, commit_sha TEXT, at INTEGER NOT NULL)`);
  }

  // every page, by path (at main)
  async pages() {
    const files = (await this.files.list("wiki/")).filter((f) => isPage(f.path) && !f.blob);
    return { pages: files.slice(0, PAGES_MAX).map((f) => ({ path: f.path, size: f.size })), more: Math.max(0, files.length - PAGES_MAX) };
  }

  // one page's text, and the git blob it is (an editor compares it before saving)
  async page({ path }) {
    mustBePage(path);
    const text = await this.files.read(path);
    if (text === null) return null;
    const { sha } = await this.files.stat(path);
    return { path, text, sha };
  }

  // an edit on the page: one commit to main, once the mutation commits;
  // its row waits here for the commit the file trigger names
  save({ path, text }, call) {
    mustBePage(path);
    call.files.write(path, text.endsWith("\n") ? text : `${text}\n`);
    this.ctx.storage.sql.exec("INSERT INTO changes (path, by, at) VALUES (?, ?, ?)", path, call.principal, Date.now());
    return { path };
  }

  remove({ path }, call) {
    mustBePage(path);
    call.files.remove(path);
    this.ctx.storage.sql.exec("INSERT INTO changes (path, by, at) VALUES (?, ?, ?)", path, call.principal, Date.now());
    return { path };
  }

  // The file trigger: main moved. A path a page's edit is waiting on is
  // that edit's commit; any other came from the folder (or `fragment
  // write`). Live queries re-run after it, so open pages follow.
  changed({ commit, paths }) {
    const sql = this.ctx.storage.sql;
    const now = Date.now();
    for (const path of (paths ?? []).filter(isPage)) {
      const mine = sql.exec("UPDATE changes SET commit_sha = ? WHERE id = (SELECT id FROM changes WHERE path = ? AND commit_sha IS NULL ORDER BY id DESC LIMIT 1) RETURNING id", commit, path).toArray();
      if (!mine.length) sql.exec("INSERT INTO changes (path, commit_sha, at) VALUES (?, ?, ?)", path, commit, now);
    }
    sql.exec("DELETE FROM changes WHERE id <= (SELECT id FROM changes ORDER BY id DESC LIMIT 1 OFFSET ?)", CHANGES_KEPT);
    return { commit };
  }

  // the newest changes, each saying whether its page is still there
  async recent() {
    const here = new Set((await this.files.list("wiki/")).map((f) => f.path));
    const changes = this.ctx.storage.sql.exec("SELECT path, by, commit_sha, at FROM changes ORDER BY id DESC LIMIT 50").toArray();
    return { changes: changes.map((c) => ({ ...c, gone: !here.has(c.path) })) };
  }
}
