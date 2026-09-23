// Platform code that runs inside the app facet, around the author's App
// class. A mutation is synchronous: the author's method and its ledger row
// commit in one facet-local transactionSync, so the facet's own database
// decides replays and conflicting bodies. (A root storage transaction
// cannot enclose a facet image above ~1.7 MB, and a capability call inside
// one deadlocks or is refused: spikes/cells-rs/README.md.)
import { App as AuthorApp } from "./app.js";

const LEDGER = "_fragment_ops";

export class App extends AuthorApp {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS ${LEDGER} (
      id TEXT PRIMARY KEY, name TEXT NOT NULL, input_sha TEXT NOT NULL, result TEXT NOT NULL, at INTEGER NOT NULL)`);
  }

  __mutate(id, name, inputSha, input) {
    if (name.startsWith("__") || typeof AuthorApp.prototype[name] !== "function") {
      return { error: "unknown_operation" };
    }
    const sql = this.ctx.storage.sql;
    return this.ctx.storage.transactionSync(() => {
      const prior = sql.exec(`SELECT input_sha, result FROM ${LEDGER} WHERE id = ?`, id).toArray()[0];
      if (prior) {
        if (prior.input_sha !== inputSha) return { error: "conflicting_body" };
        return { replayed: true, result: JSON.parse(prior.result) };
      }
      const out = AuthorApp.prototype[name].call(this, input);
      if (out && typeof out.then === "function") {
        out.catch(() => {});
        throw new Error(`mutation ${name} returned a promise; mutations are synchronous`);
      }
      const result = out ?? null;
      sql.exec(`INSERT INTO ${LEDGER} (id, name, input_sha, result, at) VALUES (?, ?, ?, ?, ?)`,
        id, name, inputSha, JSON.stringify(result), Date.now());
      return { replayed: false, result };
    });
  }

  __query(name, input) {
    if (name.startsWith("__") || typeof AuthorApp.prototype[name] !== "function") {
      return { error: "unknown_operation" };
    }
    return AuthorApp.prototype[name].call(this, input);
  }
}
