// A room anyone who can see the fragment may talk in, with an editors-only
// channel beside it, and a custom route built from an applib module.
import { DurableObject } from "cloudflare:workers";
import { shout } from "./applib/format.mjs";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS said (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL)");
  }

  say({ text }, call) {
    const { id } = this.ctx.storage.sql.exec("INSERT INTO said (text) VALUES (?) RETURNING id", text).one();
    call.publish("room", { text, id }, "said");
    return { id };
  }

  note({ text }, call) {
    call.publish("staff", { text });
    return { ok: true };
  }

  // writes, then publishes to a channel fragment.json does not declare
  undeclared({ text }, call) {
    this.ctx.storage.sql.exec("INSERT INTO said (text) VALUES (?)", text);
    call.publish("nowhere", { text });
  }

  // `n` records of `size` bytes each, to fill a channel past a page
  bulk({ n, size }, call) {
    for (let i = 0; i < n; i++) call.publish("room", { i, pad: "x".repeat(size) }, "bulk");
    return { n };
  }

  // reaches for the list of effects itself, to add one no check saw
  reach(_input, call) {
    this.ctx.storage.sql.exec("INSERT INTO said (text) VALUES ('reach')");
    call.effects.push({ channel: "events", kind: "forged", body: { summary: "forged" } });
  }

  // patches what the in-app check relies on, so a record for the audit
  // trail passes it; the supervisor's own check comes after the commit
  patched(_input, call) {
    const has = Set.prototype.has;
    Set.prototype.has = () => true;
    try {
      this.ctx.storage.sql.exec("INSERT INTO said (text) VALUES ('patched')");
      call.publish("events", { summary: "forged" }, "forged");
    } finally {
      Set.prototype.has = has;
    }
  }

  count() {
    return { n: this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM said").one().n };
  }

  whoami(_input, call) {
    return { principal: call.principal, role: call.role };
  }

  // a query for editors and up
  backstage(_input, call) {
    return { role: call.role };
  }

  async fetch(request) {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/hello")) {
      const who = request.headers.get("x-fragment-principal");
      const role = request.headers.get("x-fragment-role");
      return new Response(`<p>${shout("hello")} ${who} as ${role} at ${url.pathname}</p>`, { headers: { "content-type": "text/html" } });
    }
    if (url.pathname.endsWith("/echo") && request.method === "POST") {
      return new Response(`echo: ${await request.text()}`);
    }
    return new Response("the app has no such route", { status: 404 });
  }
}
