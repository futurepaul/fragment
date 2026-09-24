// Fills its own database a MiB at a time: the node caps it at 16 MiB.
import { DurableObject } from "cloudflare:workers";

const MIB = "x".repeat(1024 * 1024);

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS hoard (id INTEGER PRIMARY KEY AUTOINCREMENT, data TEXT NOT NULL)");
  }

  #put(mib) {
    for (let i = 0; i < mib; i++) this.ctx.storage.sql.exec("INSERT INTO hoard (data) VALUES (?)", MIB + i);
  }

  add({ mib }) {
    this.#put(mib);
    return this.size();
  }

  // a query that writes anyway: the cap is the node's, not the platform's wrapper
  sneak({ mib }) {
    this.#put(mib);
    return this.size();
  }

  size() {
    const sql = this.ctx.storage.sql;
    return { rows: sql.exec("SELECT COUNT(*) AS n FROM hoard").one().n, bytes: sql.databaseSize };
  }

  empty() {
    this.ctx.storage.sql.exec("DELETE FROM hoard");
    return this.size();
  }
}
