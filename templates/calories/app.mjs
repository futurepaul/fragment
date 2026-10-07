// Calories: each person's own food log. Someone signed in says what they
// ate on `ask` ("2 eggs and toast"); `heard` (that channel's trigger)
// asks a model for the items, logs each as theirs, and answers them on
// `replies`. Every row is its person's: `today` and `forget` read and
// change only `call.principal`'s own.
import { DurableObject } from "cloudflare:workers";

const DAY_MS = 24 * 3600 * 1000;
const ENTRIES_MAX = 200;
const ITEMS_MAX = 10;
const READ = `You keep a food log. Answer with JSON only: {"items": [{"food": "2 eggs", "calories": 140}]}, one item per food the message names, a short name and your best estimate of its calories as a whole number. A message that names no food is {"items": []}.`;

// The items a model's answer names, at most ITEMS_MAX: none when it is not
// the JSON asked for.
function items(text) {
  try {
    const { items } = JSON.parse(text.slice(text.indexOf("{"), text.lastIndexOf("}") + 1));
    return items
      .filter((i) => typeof i.food === "string" && i.food.trim() && Number.isFinite(i.calories))
      .slice(0, ITEMS_MAX)
      .map((i) => ({ food: i.food.trim().slice(0, 200), calories: Math.min(10000, Math.max(0, Math.round(i.calories))) }));
  } catch {
    return [];
  }
}

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS food (
      id INTEGER PRIMARY KEY AUTOINCREMENT, who TEXT NOT NULL, food TEXT NOT NULL, calories INTEGER NOT NULL, at INTEGER NOT NULL)`);
  }

  // an editor's (`heard` runs as one): an entry of `who`'s
  log_food({ who, food, calories }) {
    const { id } = this.ctx.storage.sql.exec("INSERT INTO food (who, food, calories, at) VALUES (?, ?, ?, ?) RETURNING id", who, food, calories, Date.now()).one();
    return { id, food, calories };
  }

  // the caller's entries since `since` (the page sends its own midnight;
  // a job, which has no clock of the person's, the last day)
  today({ since }, call) {
    const entries = this.ctx.storage.sql
      .exec("SELECT id, food, calories, at FROM food WHERE who = ? AND at >= ? ORDER BY id LIMIT ?", call.principal, since ?? Date.now() - DAY_MS, ENTRIES_MAX)
      .toArray();
    return { entries, total: entries.reduce((sum, e) => sum + e.calories, 0) };
  }

  forget({ id }, call) {
    const gone = this.ctx.storage.sql.exec("DELETE FROM food WHERE id = ? AND who = ? RETURNING id", id, call.principal).toArray();
    if (gone.length === 0) throw new Error(`no entry ${id} of yours`);
    return { id };
  }

  // a message on `ask`: its items, logged for whoever posted it, and an answer for them
  async heard({ record }, job) {
    const who = record.principal;
    const said = typeof record.body?.text === "string" ? record.body.text.slice(0, 500) : "";
    const { text } = await job.ai.text({ messages: [{ role: "system", content: READ }, { role: "user", content: said }], max_tokens: 1000 });
    const logged = items(text);
    for (const item of logged) await job.call("log_food", { who, ...item });
    const answer = logged.length
      ? `Logged ${logged.map((i) => `${i.food} (${i.calories} kcal)`).join(", ")}.`
      : `I couldn't tell what you ate. Try "2 eggs and toast".`;
    await job.publish("replies", { for: who, text: answer });
    return { logged };
  }

  // the caller's day in one sentence, answered on `replies` too
  async summarize(input, job) {
    const { entries, total } = await job.call("today", {});
    const eaten = entries.map((e) => `${e.food} (${e.calories} kcal)`).join(", ") || "nothing yet";
    const { text } = await job.ai.text({ prompt: `In one short sentence, sum up a day of eating: ${eaten}; ${total} kcal in all.`, max_tokens: 1000 });
    await job.publish("replies", { for: job.principal, text });
    return { text };
  }
}
