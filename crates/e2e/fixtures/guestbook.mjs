// A public website anyone can write to: visitors sign, members moderate.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS entries (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL)");
  }

  sign({ text }) {
    if (typeof text !== "string" || !text || text.length > 200) throw new Error("text must be 1..200 characters");
    return { id: this.ctx.storage.sql.exec("INSERT INTO entries (text) VALUES (?) RETURNING id", text).one().id };
  }

  entries() {
    return { entries: this.ctx.storage.sql.exec("SELECT id, text FROM entries ORDER BY id").toArray() };
  }

  peek() {
    return { n: this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM entries").one().n };
  }

  clear() {
    this.ctx.storage.sql.exec("DELETE FROM entries");
    return { cleared: true };
  }
}
