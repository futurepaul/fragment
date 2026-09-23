// Platform code that runs inside the app facet, around the author's App
// class (docs/MODEL.md, Operations). A mutation is synchronous: the
// author's method, its ledger row, and the effects it asked for commit in
// one facet-local transactionSync, so the facet's own database decides
// replays and conflicting bodies, and the ledger doubles as an outbox the
// supervisor applies (and re-applies idempotently) after the commit. (A
// root storage transaction cannot enclose a facet image above ~1.6 MB,
// and a capability call inside one deadlocks or is refused:
// spikes/apps/README.md.)
//
// Every method receives (input, call): `call.principal` and `call.role`
// name the caller; in a mutation, `call.publish(channel, body, kind)`
// appends a record to a channel declared in fragment.json once the
// mutation commits.
//
// Every answer is an envelope, so no value an author returns can be
// mistaken for a platform answer: { result, replayed, effects } or { error }.
import { App as AuthorApp } from "./app.js";

const LEDGER = "_fragment_ops";
// Replays are recognized for a week; older ledger rows are pruned.
const LEDGER_TTL_MS = 7 * 24 * 3600 * 1000;
const RECORD_MAX_BYTES = 64 * 1024;
const EFFECTS_MAX = 64;
const RESULT_MAX_BYTES = 1024 * 1024;
const KIND = /^[a-z][a-z0-9._-]{0,63}$/;
const RESERVED = new Set(["constructor", "fetch", "alarm", "webSocketMessage", "webSocketClose", "webSocketError"]);

function authorMethod(name) {
  return !name.startsWith("__") && !RESERVED.has(name) && typeof AuthorApp.prototype[name] === "function";
}

class Call {
  #channels;
  #effects;

  constructor(meta, mutation) {
    this.principal = meta.principal;
    this.role = meta.role;
    this.#channels = new Set(meta.channels || []);
    this.#effects = mutation ? [] : null;
  }

  publish(channel, body, kind = "message") {
    if (this.#effects === null) throw new Error("publish is for mutations; a query cannot publish");
    if (!this.#channels.has(channel)) throw new Error(`channel ${channel} is not declared in fragment.json`);
    if (typeof kind !== "string" || !KIND.test(kind)) throw new Error("kind must match ^[a-z][a-z0-9._-]{0,63}$");
    const text = JSON.stringify(body ?? null);
    if (new TextEncoder().encode(text).length > RECORD_MAX_BYTES) throw new Error(`a record's body is at most ${RECORD_MAX_BYTES} bytes`);
    if (this.#effects.length >= EFFECTS_MAX) throw new Error(`a mutation publishes at most ${EFFECTS_MAX} records`);
    this.#effects.push({ channel, kind, body: JSON.parse(text) });
  }

  get effects() {
    return this.#effects;
  }
}

export class App extends AuthorApp {
  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec(`CREATE TABLE IF NOT EXISTS ${LEDGER} (
      id TEXT PRIMARY KEY, name TEXT NOT NULL, input_sha TEXT NOT NULL, result TEXT NOT NULL, at INTEGER NOT NULL,
      effects TEXT NOT NULL DEFAULT '[]')`);
    // a ledger made before effects existed (phase 2 slice B)
    if (!sql.exec(`PRAGMA table_info(${LEDGER})`).toArray().some((c) => c.name === "effects")) {
      sql.exec(`ALTER TABLE ${LEDGER} ADD COLUMN effects TEXT NOT NULL DEFAULT '[]'`);
    }
  }

  __mutate(id, name, inputSha, input, meta) {
    if (!authorMethod(name)) return { error: "unknown_operation" };
    const sql = this.ctx.storage.sql;
    return this.ctx.storage.transactionSync(() => {
      const prior = sql.exec(`SELECT input_sha, result, effects FROM ${LEDGER} WHERE id = ?`, id).toArray()[0];
      if (prior) {
        if (prior.input_sha !== inputSha) return { error: "conflicting_body" };
        return { replayed: true, result: JSON.parse(prior.result), effects: JSON.parse(prior.effects) };
      }
      const call = new Call(meta, true);
      const out = AuthorApp.prototype[name].call(this, input, call);
      if (out && typeof out.then === "function") {
        out.catch(() => {});
        throw new Error(`mutation ${name} returned a promise; mutations are synchronous`);
      }
      const result = out ?? null;
      const text = JSON.stringify(result);
      if (new TextEncoder().encode(text).length > RESULT_MAX_BYTES) throw new Error(`a result is at most ${RESULT_MAX_BYTES} bytes`);
      const now = Date.now();
      sql.exec(`INSERT INTO ${LEDGER} (id, name, input_sha, result, at, effects) VALUES (?, ?, ?, ?, ?, ?)`,
        id, name, inputSha, text, now, JSON.stringify(call.effects));
      if (sql.exec("SELECT last_insert_rowid() AS r").one().r % 100 === 0) {
        sql.exec(`DELETE FROM ${LEDGER} WHERE at < ?`, now - LEDGER_TTL_MS);
      }
      return { replayed: false, result, effects: call.effects };
    });
  }

  async __query(name, input, meta) {
    if (!authorMethod(name)) return { error: "unknown_operation" };
    const result = await AuthorApp.prototype[name].call(this, input, new Call(meta, false));
    return { replayed: false, result: result ?? null };
  }

  // The newest ledger rows, for the supervisor's sweep of effects it may
  // not have applied (it died between the facet's commit and its own).
  __recent(limit) {
    const rows = this.ctx.storage.sql
      .exec(`SELECT id, name, effects FROM ${LEDGER} ORDER BY rowid DESC LIMIT ?`, limit)
      .toArray();
    return { result: rows.map((r) => ({ id: r.id, name: r.name, effects: JSON.parse(r.effects) })) };
  }

  // Custom routes: the author's fetch, when there is one.
  fetch(request) {
    const own = AuthorApp.prototype.fetch;
    if (typeof own !== "function") return new Response("not found\n", { status: 404 });
    return own.call(this, request);
  }
}
