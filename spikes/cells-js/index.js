// The JavaScript baseline for spike 1: the same supervisor as
// spikes/cells-rs/src/lib.rs (routes, ledger, facet, capability), so the
// Rust numbers have something to be compared with.
import { DurableObject, WorkerEntrypoint } from "cloudflare:workers";
import PLATFORM from "./platform.txt";

const INPUT_MAX_BYTES = 256 * 1024;
const RESULT_MAX_BYTES = 1024 * 1024;
const APP_CPU_MS = 30_000;
const APP_SUBREQUESTS = 50;

const SCHEMA = `
CREATE TABLE IF NOT EXISTS code (id INTEGER PRIMARY KEY CHECK (id = 1), sha TEXT NOT NULL, source TEXT NOT NULL, operations TEXT NOT NULL, cpu_ms INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS ops (id TEXT PRIMARY KEY, name TEXT NOT NULL, input_sha TEXT NOT NULL, result TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS channel (seq INTEGER PRIMARY KEY AUTOINCREMENT, channel TEXT NOT NULL, body TEXT NOT NULL, at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS alarms (n INTEGER PRIMARY KEY AUTOINCREMENT, fired_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS pings (n INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER NOT NULL);
`;

class OpError extends Error {
  constructor(status, code, message) { super(message); this.status = status; this.code = code; }
}

const json = (v, status = 200) => Response.json(v, { status });

// canonical JSON (sorted keys), matching serde_json's default map order
function canon(v) {
  if (Array.isArray(v)) return `[${v.map(canon).join(",")}]`;
  if (v && typeof v === "object") return `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canon(v[k])}`).join(",")}}`;
  return JSON.stringify(v ?? null);
}

async function sha256hex(text) {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export class Channel extends WorkerEntrypoint {
  append(channel, body) {
    const ns = this.env.SUPERVISOR;
    return ns.get(ns.idFromString(this.ctx.props.cell)).capAppend(String(channel), JSON.stringify(body ?? null));
  }
}

export class Supervisor extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(SCHEMA);
  }

  async fetch(req) {
    try {
      return await this.route(req);
    } catch (e) {
      if (e instanceof OpError) return json({ error: e.code, message: e.message }, e.status);
      return json({ error: "host_failed", message: String(e && e.message || e) }, 500);
    }
  }

  alarm() {
    this.ctx.storage.sql.exec("INSERT INTO alarms (fired_at) VALUES (?)", Date.now());
  }

  webSocketMessage(ws, message) {
    if (typeof message !== "string") { ws.send(JSON.stringify({ error: "text frames only" })); return; }
    ws.send(JSON.stringify({ seq: this.append("ws", message) }));
  }

  webSocketClose() {}

  capAppend(channel, body) { return this.append(channel, body); }

  append(channel, body) {
    if (!channel || channel.length > 64) throw new OpError(400, "invalid_request", "channel name");
    if (body.length > 64 * 1024) throw new OpError(413, "too_large", "channel record");
    return this.ctx.storage.sql.exec("INSERT INTO channel (channel, body, at) VALUES (?, ?, ?) RETURNING seq", channel, body, Date.now()).one().seq;
  }

  async route(req) {
    const url = new URL(req.url);
    const rest = url.pathname.split("/").slice(3).join("/");
    const sql = this.ctx.storage.sql;
    const key = `${req.method} ${rest}`;
    if (key === "GET noop") return json({ ok: true });
    if (key === "POST ping") return json({ n: sql.exec("INSERT INTO pings (at) VALUES (?) RETURNING n", Date.now()).one().n });
    if (key === "PUT code") return this.installCode(await req.json());
    if (key === "POST op") return this.op(await req.json());
    if (key === "GET ws") {
      const [client, server] = Object.values(new WebSocketPair());
      this.ctx.acceptWebSocket(server);
      return new Response(null, { status: 101, webSocket: client });
    }
    if (key === "POST alarm") { const { in_ms } = await req.json(); await this.ctx.storage.setAlarm(in_ms); return json({ armed_in_ms: in_ms }); }
    if (key === "GET alarms") return json({ alarms: sql.exec("SELECT n, fired_at FROM alarms ORDER BY n").toArray() });
    if (key === "GET channel") return json({ records: sql.exec("SELECT seq, body, at FROM channel WHERE channel = ? ORDER BY seq LIMIT 1000", url.searchParams.get("name")).toArray() });
    if (key === "GET stats") return json({ db_bytes: sql.databaseSize, ops: sql.exec("SELECT COUNT(*) n FROM ops").one().n, channel: sql.exec("SELECT COUNT(*) n FROM channel").one().n });
    throw new OpError(400, "invalid_request", `no route ${key}`);
  }

  installCode({ sha, source, operations, cpu_ms }) {
    const cpu = cpu_ms ?? APP_CPU_MS;
    if (!(cpu > 0 && cpu <= APP_CPU_MS)) throw new OpError(400, "invalid_request", "cpu_ms");
    this.ctx.storage.sql.exec(
      "INSERT INTO code (id, sha, source, operations, cpu_ms) VALUES (1, ?, ?, ?, ?) ON CONFLICT (id) DO UPDATE SET sha = excluded.sha, source = excluded.source, operations = excluded.operations, cpu_ms = excluded.cpu_ms",
      sha, source, JSON.stringify(operations), cpu,
    );
    this.ctx.facets.abort("app", new Error("code replaced"));
    return json({ sha });
  }

  app() {
    const row = this.ctx.storage.sql.exec("SELECT sha, source, operations, cpu_ms FROM code WHERE id = 1").toArray()[0];
    if (!row) throw new OpError(404, "no_code", "no app code is installed");
    const channel = this.ctx.exports.Channel({ props: { cell: this.ctx.id.toString() } });
    const worker = this.env.LOADER.get(`app@${row.sha}`, () => ({
      compatibilityDate: "2026-01-01",
      mainModule: "platform.js",
      modules: { "platform.js": PLATFORM, "app.js": row.source },
      env: { CHANNEL: channel },
      globalOutbound: null,
      limits: { cpuMs: row.cpu_ms, subRequests: APP_SUBREQUESTS },
    }));
    const facet = this.ctx.facets.get("app", () => ({ class: worker.getDurableObjectClass("App") }));
    return { facet, operations: JSON.parse(row.operations) };
  }

  async op({ id, name, input = null, fail_after_app = false }) {
    if (typeof id !== "string" || !/^[A-Za-z0-9\-_:.]{1,128}$/.test(id)) throw new OpError(400, "invalid_request", "operation id");
    const inputText = canon(input);
    if (inputText.length > INPUT_MAX_BYTES) throw new OpError(413, "too_large", "operation input");
    const { facet, operations } = this.app();
    const kind = operations[name]?.kind;
    if (!kind) throw new OpError(404, "unknown_operation", `no operation named ${JSON.stringify(name)}`);
    if (kind === "query") {
      let out;
      try {
        out = await facet.__query(name, input);
      } catch (e) {
        throw new OpError(422, "app_failed", `query: ${String(e && e.message || e)}`);
      }
      if (out && out.error === "unknown_operation") throw new OpError(404, "unknown_operation", name);
      return json({ result: out ?? null, replayed: false });
    }
    const inputSha = await sha256hex(`${name}\n${inputText}`);
    let out;
    try {
      out = await facet.__mutate(id, name, inputSha, input);
    } catch (e) {
      throw new OpError(422, "app_failed", `mutation: ${String(e && e.message || e)}`);
    }
    if (out.error === "conflicting_body") throw new OpError(409, "conflicting_body", "this operation id was already used with a different input");
    if (out.error === "unknown_operation") throw new OpError(404, "unknown_operation", name);
    const text = JSON.stringify(out.result);
    if (text.length > RESULT_MAX_BYTES) throw new OpError(413, "too_large", "operation result");
    // spike-only: the facet committed; the supervisor dies before its audit row
    if (fail_after_app) throw new OpError(500, "host_failed", "fail_after_app: the supervisor failed after the facet committed");
    this.ctx.storage.sql.exec("INSERT OR IGNORE INTO ops (id, name, input_sha, result, at) VALUES (?, ?, ?, ?, ?)", id, name, inputSha, text, Date.now());
    return json({ result: out.result, replayed: out.replayed });
  }
}

export default {
  fetch(req, env) {
    const cell = new URL(req.url).pathname.split("/")[2];
    if (!cell || cell.length > 64) return new Response("expected /c/<cell>/<route>", { status: 404 });
    return env.SUPERVISOR.getByName(cell).fetch(req);
  },
};
