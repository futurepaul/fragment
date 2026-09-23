// A webhook inbox: every delivery to the fragment's inbox starts `ingest`,
// a job that turns it into a line on the page. A delivery with a `url`
// makes the job fetch that page for its title; if the site is down, the
// fetch is retried with backoff, and a delivery that still fails is held
// (`fragment runs`) until someone replays it.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS items (
      id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, source TEXT NOT NULL, at INTEGER NOT NULL)`);
  }

  // A job: each `await job.…` is a durable step, so a crash resumes here
  // without fetching or adding twice.
  async ingest({ record }, job) {
    const { source, payload } = record.body;
    let text = typeof payload === "string" ? payload : payload?.text ?? JSON.stringify(payload);
    if (typeof payload?.url === "string") {
      const page = await job.fetch(payload.url);
      const title = /<title[^>]*>([^<]*)<\/title>/i.exec(page.text())?.[1]?.trim();
      text = `${title || payload.url} (${page.status})`;
    }
    return await job.call("add", { text: text.slice(0, 2000), source: String(source).slice(0, 200) });
  }

  add({ text, source }, call) {
    const at = Date.now();
    const { id } = this.ctx.storage.sql.exec("INSERT INTO items (text, source, at) VALUES (?, ?, ?) RETURNING id", text, source, at).one();
    call.publish("feed", { id, text, source }, "item");
    return { id };
  }

  list() {
    return { items: this.ctx.storage.sql.exec("SELECT id, text, source, at FROM items ORDER BY id DESC LIMIT 100").toArray() };
  }
}
