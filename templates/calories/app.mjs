// Calories: each person's own food log. The fragment's agent (the `agent`
// block in fragment.json) calls log_food and today for whoever asked it,
// as them, so every row is its caller's: each operation reads and changes
// only `call.principal`'s own.
import { DurableObject } from "cloudflare:workers";

const DAY_MS = 24 * 3600 * 1000;
const ENTRIES_MAX = 200;

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS food (
      id INTEGER PRIMARY KEY AUTOINCREMENT, who TEXT NOT NULL, food TEXT NOT NULL, calories INTEGER NOT NULL, at INTEGER NOT NULL)`);
  }

  log_food({ food, calories }, call) {
    const { id } = this.ctx.storage.sql
      .exec("INSERT INTO food (who, food, calories, at) VALUES (?, ?, ?, ?) RETURNING id", call.principal, food, calories, Date.now())
      .one();
    return { id, food, calories };
  }

  // the caller's entries since `since` (the page sends its own midnight;
  // the agent, which has no clock of the person's, the last day)
  today({ since }, call) {
    const entries = this.ctx.storage.sql
      .exec("SELECT id, food, calories, at FROM food WHERE who = ? AND at >= ? ORDER BY id LIMIT ?", call.principal, since ?? Date.now() - DAY_MS, ENTRIES_MAX)
      .toArray();
    return { entries, total: entries.reduce((sum, e) => sum + e.calories, 0) };
  }

  // a job: one turn of the agent, for whoever called it, whose answer (and
  // steps) land on `ask` like any other; the run's output is its answer
  async summarize(input, job) {
    return await job.agent({ prompt: "Summarize what I ate today, in one sentence.", channel: "ask" });
  }

  forget({ id }, call) {
    const gone = this.ctx.storage.sql.exec("DELETE FROM food WHERE id = ? AND who = ? RETURNING id", id, call.principal).toArray();
    if (gone.length === 0) throw new Error(`no entry ${id} of yours`);
    return { id };
  }
}
