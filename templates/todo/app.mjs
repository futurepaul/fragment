// The todo app: one method per operation in fragment.json, over the app's
// own SQLite. Mutations are synchronous; what should happen next (a line in
// the activity feed) they publish, and the platform appends it once the
// mutation commits.
import { DurableObject } from "cloudflare:workers";

const LIST_MAX = 500;

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS todos (
      id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, done INTEGER NOT NULL DEFAULT 0, at INTEGER NOT NULL)`);
  }

  add({ text }, call) {
    const { id } = this.ctx.storage.sql
      .exec("INSERT INTO todos (text, at) VALUES (?, ?) RETURNING id", text, Date.now())
      .one();
    call.publish("activity", { did: "added", id, text, by: call.principal });
    return { id };
  }

  toggle({ id }, call) {
    const todo = this.#get(id);
    const done = todo.done ? 0 : 1;
    this.ctx.storage.sql.exec("UPDATE todos SET done = ? WHERE id = ?", done, id);
    call.publish("activity", { did: done ? "finished" : "reopened", id, text: todo.text, by: call.principal });
    return { id, done: done === 1 };
  }

  remove({ id }, call) {
    const todo = this.#get(id);
    this.ctx.storage.sql.exec("DELETE FROM todos WHERE id = ?", id);
    call.publish("activity", { did: "removed", id, text: todo.text, by: call.principal });
    return { id };
  }

  // the newest LIST_MAX todos, oldest first as the page shows them: a
  // query's answer is bounded like everything else, and what is new is
  // what someone just added
  list() {
    const todos = this.ctx.storage.sql
      .exec("SELECT id, text, done FROM (SELECT id, text, done FROM todos ORDER BY id DESC LIMIT ?) ORDER BY id", LIST_MAX)
      .toArray();
    return { todos: todos.map((t) => ({ ...t, done: t.done === 1 })) };
  }

  #get(id) {
    const todo = this.ctx.storage.sql.exec("SELECT id, text, done FROM todos WHERE id = ?", id).toArray()[0];
    if (!todo) throw new Error(`no todo ${id}`);
    return todo;
  }
}
