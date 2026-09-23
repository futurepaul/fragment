// The e2e's author app: one method per operation, its own SQLite.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS todos (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL)");
  }

  add_todo({ text }) {
    if (typeof text !== "string" || text.length === 0 || text.length > 500) throw new Error("text must be 1..500 characters");
    return { id: this.ctx.storage.sql.exec("INSERT INTO todos (text) VALUES (?) RETURNING id", text).one().id };
  }

  // writes, then throws: the write must roll back
  add_then_throw({ text }) {
    this.add_todo({ text });
    throw new Error("thrown after the write");
  }

  // an async mutation: refused, and its write rolled back
  async add_async({ text }) {
    const r = this.add_todo({ text });
    await Promise.resolve();
    return r;
  }

  list() {
    return { todos: this.ctx.storage.sql.exec("SELECT id, text FROM todos ORDER BY id").toArray() };
  }

  count() {
    return { n: this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM todos").one().n };
  }
}
