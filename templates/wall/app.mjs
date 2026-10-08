// Wall: a page anyone who can open it posts to. A post is a record on
// `posts`, a channel people post to (`"post": "public"`): the platform
// appends it, with no app code, holds an anonymous poster to the public
// call budget, and every open page follows the channel live. The app
// keeps what a post cannot: the wall's title and intro (`about`), and the
// posts an editor took down (`hide`), which the pages leave out.
import { DurableObject } from "cloudflare:workers";

// The channel keeps its newest 10 000 posts; a post older than that is gone anyway.
const HIDDEN_MAX = 10000;

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS about (one INTEGER PRIMARY KEY CHECK (one = 1), title TEXT NOT NULL, intro TEXT NOT NULL)");
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS hidden (seq INTEGER PRIMARY KEY)");
  }

  about({ title, intro = "" }) {
    this.ctx.storage.sql.exec("INSERT INTO about (one, title, intro) VALUES (1, ?, ?) ON CONFLICT (one) DO UPDATE SET title = excluded.title, intro = excluded.intro", title, intro);
    return { title, intro };
  }

  // A post off the wall: the pages leave it out. The channel keeps it (its
  // log is append-only), so whoever reads `posts` itself still can.
  hide({ seq }) {
    const sql = this.ctx.storage.sql;
    sql.exec("INSERT OR IGNORE INTO hidden (seq) VALUES (?)", seq);
    sql.exec("DELETE FROM hidden WHERE seq NOT IN (SELECT seq FROM hidden ORDER BY seq DESC LIMIT ?)", HIDDEN_MAX);
    return { seq };
  }

  wall() {
    const about = this.ctx.storage.sql.exec("SELECT title, intro FROM about").toArray()[0] ?? { title: "", intro: "" };
    const hidden = this.ctx.storage.sql.exec("SELECT seq FROM hidden ORDER BY seq").toArray().map((r) => r.seq);
    return { ...about, hidden };
  }
}
