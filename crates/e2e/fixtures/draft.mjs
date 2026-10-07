// A draft's app (the drafts section): a write and a read anyone may call,
// and the two steps a draft refuses until it is claimed: a fetch out, and
// an AI step.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS notes (text TEXT NOT NULL)");
  }

  note({ text }) {
    this.ctx.storage.sql.exec("INSERT INTO notes (text) VALUES (?)", text);
    return { ok: true };
  }

  notes() {
    return { notes: this.ctx.storage.sql.exec("SELECT text FROM notes").toArray().map((r) => r.text) };
  }

  async look({ url }, job) {
    const r = await job.fetch(url);
    return { status: r.status };
  }

  async ask({ text }, job) {
    return await job.ai.text({ prompt: text });
  }
}
