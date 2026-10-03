// A chat from before postable channels: a message is said through `say`,
// which publishes it to `chat`, a channel that takes no posts. An agent
// following it answers through its listen's reply operation, `say`.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS said (id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL)");
  }

  say({ text }, call) {
    const { id } = this.ctx.storage.sql.exec("INSERT INTO said (text) VALUES (?) RETURNING id", text).one();
    call.publish("chat", { text }, "said");
    return { id };
  }

  count() {
    return { n: this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM said").one().n };
  }
}
