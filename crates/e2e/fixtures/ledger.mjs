// The ledger (phase 3): jobs with paid steps, and a write and a read.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS notes (text TEXT NOT NULL)");
  }

  async summarize({ text, tier }, job) {
    return await job.ai.text({ model: tier, prompt: text });
  }

  async decide(input, job) {
    return await job.ai.decide(input);
  }

  async decide_twice(input, job) {
    const one = await job.ai.decide(input);
    const two = await job.ai.decide(input);
    return { one, two };
  }

  // two paid steps: a replay after the second failed pays only for it
  async twice({ a, b }, job) {
    const one = await job.ai.text({ prompt: a });
    const two = await job.ai.text({ prompt: b });
    return { one: one.text, two: two.text };
  }

  async draw({ prompt, path, steps }, job) {
    return await job.ai.image({ prompt, path, steps });
  }

  // anyone who can see the fragment may call it; its owner pays
  async ask({ text }, job) {
    return await job.ai.text({ prompt: text });
  }

  note({ text }) {
    this.ctx.storage.sql.exec("INSERT INTO notes (text) VALUES (?)", text);
    return { ok: true };
  }

  notes() {
    return { notes: this.ctx.storage.sql.exec("SELECT text FROM notes").toArray().map((r) => r.text) };
  }
}
