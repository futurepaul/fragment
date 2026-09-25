// Jobs and triggers (slice D): an inbox that fills a list through a job, a
// job that fetches with the fragment's secret, and levers for held runs,
// replays, sleeps, loops, auto-pause, cron, and file triggers.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec("CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, source TEXT NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS ticks (id INTEGER PRIMARY KEY AUTOINCREMENT, input TEXT NOT NULL)");
  }

  save({ texts, source }, call) {
    for (const text of texts) {
      this.ctx.storage.sql.exec("INSERT INTO items (text, source) VALUES (?, ?)", text, source);
      call.publish("feed", { text, source }, "item");
    }
    return { saved: texts.length };
  }

  items() {
    return this.ctx.storage.sql.exec("SELECT text, source FROM items ORDER BY id").toArray();
  }

  // Fetches a list with the fragment's API_KEY, saves it, and says so.
  async digest({ url }, job) {
    const resp = await job.fetch(url, { headers: { authorization: "Bearer {{API_KEY}}" } });
    const { items } = resp.json();
    const saved = await job.call("save", { texts: items, source: "digest" });
    await job.publish("feed", { digest: items.length, by: job.principal }, "digest");
    return { status: resp.status, saved: saved.saved, run: job.run };
  }

  // The inbox's trigger: a delivery's items (or its text) saved.
  async ingest({ record }, job) {
    const { source, payload } = record.body;
    const texts = Array.isArray(payload?.items) ? payload.items : [typeof payload === "string" ? payload : JSON.stringify(payload)];
    return await job.call("save", { texts, source });
  }

  set_flag({ on }) {
    this.ctx.storage.sql.exec("INSERT INTO kv (k, v) VALUES ('flag', ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v", on ? "1" : "0");
    return { on };
  }

  flag() {
    return { on: this.ctx.storage.sql.exec("SELECT v FROM kv WHERE k = 'flag'").toArray()[0]?.v === "1" };
  }

  // Held until the flag is set; fine when replayed after.
  async fragile(_input, job) {
    const { on } = await job.call("flag");
    if (!on) throw new Error("the flag is not set");
    await job.call("save", { texts: ["fragile made it"], source: "fragile" });
    return { ok: true };
  }

  // `n` steps, one after another: a publish each, and every tenth a
  // sleep. At each step the body reads every earlier answer back.
  async count_up({ n }, job) {
    const seqs = [];
    for (let i = 0; i < n; i++) {
      if (i % 10 === 9) await job.sleep(1);
      else seqs.push((await job.publish("steps", { i }, "step")).seq);
    }
    return { seqs };
  }

  async nap({ ms }, job) {
    await job.sleep(ms);
    await job.publish("feed", { woke: true }, "nap");
    return { slept: ms };
  }

  // A step that fails for good, caught by the job.
  async careful({ url }, job) {
    try {
      await job.fetch(url);
      return { caught: false };
    } catch (e) {
      return { caught: true, name: e.name, message: e.message };
    }
  }

  // What a fetch answered, as the job sees it.
  async probe({ url }, job) {
    const r = await job.fetch(url);
    return { status: r.status, location: r.headers.get("location") };
  }

  // Starts another job and answers its run.
  async parent(_input, job) {
    return await job.call("nap", { ms: 10 });
  }

  async wrong(_input, job) {
    return await job.call("no_such_op", {});
  }

  // A step whose args do not fit (a text step names no model): the
  // platform says why, and the job may catch it.
  async misfit(_input, job) {
    try {
      await job.ai.text({ prompt: "which model?" });
      return { caught: false };
    } catch (e) {
      return { caught: true, name: e.name, message: e.message };
    }
  }

  ping(_input, call) {
    call.publish("loop", { ping: true }, "ping");
    return { ok: true };
  }

  raise(_input, call) {
    call.publish("alarms", { raised: true }, "alarm");
    return { ok: true };
  }

  async boom() {
    throw new Error("boom");
  }

  tick(input) {
    this.ctx.storage.sql.exec("INSERT INTO ticks (input) VALUES (?)", JSON.stringify(input));
    return { ok: true };
  }

  ticks() {
    return this.ctx.storage.sql.exec("SELECT input FROM ticks ORDER BY id").toArray().map((r) => JSON.parse(r.input));
  }
}
