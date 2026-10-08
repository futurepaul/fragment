// Hook: a live board of what your webhooks say (CI runs, deploys,
// payments). Each delivery to the fragment's inbox runs `heard`, a
// mutation the inbox trigger starts: it reads the delivery as an event
// (GitHub's and Stripe's shapes, or a plain {name, status}), keeps it in
// SQL by its own id (a redelivery or a later status of the same run
// updates it), and pushes when something starts failing or recovers.
// Every open page re-runs `board` as each one lands.
import { DurableObject } from "cloudflare:workers";

const EVENTS_KEPT = 2000;
const DAYS = 14;
const DAY_MS = 24 * 3600 * 1000;
const STATES = {
  ok: ["success", "succeeded", "passed", "pass", "ok", "completed", "complete", "deployed", "paid", "up", "green"],
  failed: ["failure", "failed", "fail", "error", "errored", "timed_out", "startup_failure", "down", "red", "payment_failed", "disputed"],
  running: ["queued", "requested", "in_progress", "pending", "running", "started", "waiting", "building"],
};

const text = (v, n) => [...(typeof v === "string" ? v : v == null ? "" : String(v))].slice(0, n).join("").trim();
const web = (v) => (typeof v === "string" && /^https?:\/\//i.test(v) ? text(v, 500) : null);
const stateOf = (status) => Object.keys(STATES).find((k) => STATES[k].includes(text(status, 40).toLowerCase())) ?? "info";

// A delivery as an event: {source, name, status, detail, url, value, key}.
function read(source, p) {
  const repo = p?.repository?.full_name;
  if (p?.workflow_run) {
    const r = p.workflow_run;
    return { source: repo ?? source, name: r.name ?? "workflow", status: r.conclusion ?? r.status, detail: [r.head_branch, text(r.head_commit?.message, 200).split("\n")[0]].filter(Boolean).join(": "), url: r.html_url, key: `github:run:${r.id}:${r.run_attempt ?? 1}` };
  }
  if (p?.deployment_status) {
    const d = p.deployment_status;
    return { source: repo ?? source, name: `deploy to ${p.deployment?.environment ?? d.environment ?? "?"}`, status: d.state, detail: p.deployment?.ref, url: d.target_url || d.environment_url || d.log_url, key: `github:deploy:${d.id}` };
  }
  if (p?.zen && p?.hook_id) return { source: repo ?? "github", name: "webhook", status: "ok", detail: p.zen, key: `github:ping:${p.hook_id}` };
  if (p?.object === "event" && typeof p.type === "string") {
    const o = p.data?.object ?? {};
    const amount = Number.isFinite(o.amount) ? o.amount / 100 : null;
    const failed = /failed|disputed|canceled/.test(p.type);
    return {
      source: "stripe",
      name: p.type.split(".")[0].replaceAll("_", " "),
      status: failed ? "failed" : /succeeded|paid|completed/.test(p.type) ? "succeeded" : p.type.split(".").pop(),
      detail: [amount === null ? "" : `${amount.toFixed(2)} ${text(o.currency, 8).toUpperCase()}`, o.description ?? o.receipt_email ?? ""].filter(Boolean).join(" · "),
      value: amount,
      key: `stripe:${p.id}`,
    };
  }
  if (p && typeof p === "object" && !Array.isArray(p)) {
    return { source, name: p.name ?? p.title ?? p.workflow ?? "event", status: p.status ?? p.state ?? p.conclusion, detail: p.detail ?? p.text ?? p.message, url: p.url, value: Number.isFinite(p.value) ? p.value : null, key: p.id == null ? null : `${source}:${p.id}` };
  }
  return { source, name: "message", status: null, detail: typeof p === "string" ? p : JSON.stringify(p) };
}

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS events (
      id INTEGER PRIMARY KEY AUTOINCREMENT, key TEXT UNIQUE, source TEXT NOT NULL, name TEXT NOT NULL, status TEXT NOT NULL,
      state TEXT NOT NULL, detail TEXT NOT NULL, url TEXT, value REAL, at INTEGER NOT NULL)`);
    ctx.storage.sql.exec("CREATE INDEX IF NOT EXISTS events_tile ON events (source, name, at)");
  }

  // the inbox trigger's: one delivery (`{source, payload}`); never throws
  // on what a sender sent, so no delivery is held for its shape
  heard({ record }, call) {
    const sql = this.ctx.storage.sql;
    const e = read(text(record.body?.source, 100) || "external", record.body?.payload);
    const ev = {
      source: text(e.source, 100) || "external",
      name: text(e.name, 100) || "event",
      status: text(e.status, 40),
      detail: text(e.detail, 300),
      url: web(e.url),
      value: Number.isFinite(e.value) ? e.value : null,
      key: e.key ? text(e.key, 200) : null,
    };
    const state = stateOf(ev.status);
    // a redelivery says nothing new; else, what the tile last settled on
    const again = ev.key !== null && sql.exec("SELECT state FROM events WHERE key = ?", ev.key).toArray()[0]?.state === state;
    const was = sql.exec("SELECT state FROM events WHERE source = ? AND name = ? AND state IN ('ok', 'failed') AND (key IS NOT ? OR key IS NULL) ORDER BY at DESC, id DESC LIMIT 1", ev.source, ev.name, ev.key).toArray()[0]?.state;
    const at = record.at ?? Date.now();
    sql.exec(
      `INSERT INTO events (key, source, name, status, state, detail, url, value, at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
       ON CONFLICT (key) DO UPDATE SET status = excluded.status, state = excluded.state, detail = excluded.detail, url = COALESCE(excluded.url, url), value = excluded.value, at = excluded.at`,
      ev.key, ev.source, ev.name, ev.status, state, ev.detail, ev.url, ev.value, at,
    );
    sql.exec("DELETE FROM events WHERE id <= (SELECT id FROM events ORDER BY id DESC LIMIT 1 OFFSET ?)", EVENTS_KEPT);
    if (again) return { state };
    if (state === "failed" && was !== "failed") {
      call.push("alerts", { title: `${ev.name} failed`, body: text([ev.source, ev.detail].filter(Boolean).join(": "), 300), tag: `${ev.source}/${ev.name}`, url: "./" });
    } else if (state === "ok" && was === "failed") {
      call.push("alerts", { title: `${ev.name} is passing again`, body: ev.source, tag: `${ev.source}/${ev.name}`, url: "./" });
    }
    return { state };
  }

  // the latest of each tile (failing first), the newest events, and the
  // last DAYS days' counts
  board() {
    const sql = this.ctx.storage.sql;
    const tiles = sql
      .exec(`SELECT source, name, status, state, detail, url, at FROM (
        SELECT *, ROW_NUMBER() OVER (PARTITION BY source, name ORDER BY at DESC, id DESC) AS n FROM events) WHERE n = 1
        ORDER BY state = 'failed' DESC, at DESC LIMIT 60`)
      .toArray();
    const recent = sql.exec("SELECT id, source, name, status, state, detail, url, at FROM events ORDER BY at DESC, id DESC LIMIT 40").toArray();
    const since = (Math.floor(Date.now() / DAY_MS) - DAYS + 1) * DAY_MS;
    const days = sql
      .exec("SELECT CAST(at / 86400000 AS INTEGER) AS day, SUM(state = 'ok') AS ok, SUM(state = 'failed') AS failed, COUNT(*) AS n FROM events WHERE at >= ? GROUP BY day ORDER BY day", since)
      .toArray();
    return { tiles, recent, days, since };
  }

  forget({ source, name }) {
    this.ctx.storage.sql.exec("DELETE FROM events WHERE source = ? AND name = ?", source, name);
    return { source, name };
  }
}
