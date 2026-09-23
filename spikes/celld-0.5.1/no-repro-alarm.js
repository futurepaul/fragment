import { DurableObject } from "cloudflare:workers";
// Repro: a request re-arms the alarm to "now" while the alarm handler is
// finishing (after the handler's own re-arm to +4 s). The new alarm should
// fire right away.
export class C extends DurableObject {
  constructor(ctx, env) { super(ctx, env); ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS fired (n INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER)"); }
  async fetch(req) {
    const u = new URL(req.url);
    if (u.pathname === "/arm") {
      await this.ctx.storage.setAlarm(Date.now());
      return Response.json({ armedAt: Date.now() });
    }
    const rows = this.ctx.storage.sql.exec("SELECT * FROM fired").toArray();
    return Response.json({ rows, alarm: await this.ctx.storage.getAlarm(), now: Date.now() });
  }
  async alarm() {
    const n = this.ctx.storage.sql.exec("INSERT INTO fired (at) VALUES (?) RETURNING n", Date.now()).one().n;
    // like fragment's alarm(): some writes, then the handler's own re-arm last
    if (n === 1) {
      this.ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS pad (b BLOB)");
      for (let i = 0; i < 1000; i++) this.ctx.storage.sql.exec("INSERT INTO pad VALUES (randomblob(4096))");
      await this.ctx.storage.setAlarm(Date.now() + 4000);
    }
  }
}
export default { fetch(req, env) { const u = new URL(req.url); return env.C.getByName(u.searchParams.get("c") || "a").fetch(req); } };
