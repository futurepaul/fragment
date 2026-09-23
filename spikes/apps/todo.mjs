// The spike's author app: the `App` class a supervisor starts as its `app`
// facet. One method per operation; its SQLite database is its own.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`
      CREATE TABLE IF NOT EXISTS todos (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS pad (n INTEGER PRIMARY KEY AUTOINCREMENT, b BLOB NOT NULL);
    `);
  }

  // mutations
  add_todo({ text }) {
    if (typeof text !== "string" || text.length === 0 || text.length > 500) throw new Error("text must be 1..500 characters");
    const row = this.ctx.storage.sql.exec("INSERT INTO todos (text) VALUES (?) RETURNING id", text).one();
    return { id: row.id };
  }

  // writes, then throws: the write must roll back
  add_then_throw({ text }) {
    this.add_todo({ text });
    throw new Error("thrown after the write");
  }

  // an async mutation: the platform refuses it and rolls its write back
  async add_todo_emit({ text }) {
    const { id } = this.add_todo({ text });
    const seq = await this.env.CHANNEL.append("todos", { id, text });
    return { id, seq };
  }

  // grows the database toward `mib` MiB, at most `step_mib` (default 8) per call
  fill({ mib, step_mib = 8 }) {
    const target = mib * 1024 * 1024;
    const sql = this.ctx.storage.sql;
    const stop = sql.databaseSize + step_mib * 1024 * 1024;
    while (sql.databaseSize < target && sql.databaseSize < stop) {
      for (let i = 0; i < 32; i++) sql.exec("INSERT INTO pad (b) VALUES (randomblob(16384))");
    }
    return { bytes: sql.databaseSize };
  }

  spin_forever() {
    let x = 0;
    for (;;) x = (x + 1) | 0;
  }

  // queries

  // a query that calls back into the supervisor through a capability
  async emit_query({ text }) {
    return { seq: await this.env.CHANNEL.append("todos", { text }) };
  }
  list() {
    return { todos: this.ctx.storage.sql.exec("SELECT id, text, done FROM todos ORDER BY id").toArray() };
  }

  count() {
    return { n: this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM todos").one().n };
  }

  size() {
    return { bytes: this.ctx.storage.sql.databaseSize };
  }

  // what the facet can see: only its own tables
  tables() {
    return { tables: this.ctx.storage.sql.exec("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name").toArray().map((r) => r.name) };
  }

  async egress() {
    try {
      const r = await fetch("https://example.com/");
      return { reached: true, status: r.status };
    } catch (e) {
      return { reached: false, error: String(e && e.message || e) };
    }
  }

  async alarm_attempt() {
    try {
      await this.ctx.storage.setAlarm(Date.now() + 60_000);
      return { armed: true };
    } catch (e) {
      return { armed: false, error: String(e && e.message || e) };
    }
  }
}
