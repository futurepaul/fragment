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

  huge(_input, call) {
    call.publish("room", { blob: "x".repeat(70 * 1024) });
  }

  count() {
    return { n: this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM said").one().n };
  }

  whoami(_input, call) {
    return { principal: call.principal, role: call.role };
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
