// A room people post to: the platform appends each record, so the app has
// no `say` of its own. It only hears each record posted to `chat` (a
// channel trigger), and answers what it heard.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS heard (seq INTEGER PRIMARY KEY, principal TEXT NOT NULL, body TEXT NOT NULL)");
  }

  // each record posted to chat, once, whoever posted it
  heard({ record }) {
    this.ctx.storage.sql.exec("INSERT OR IGNORE INTO heard (seq, principal, body) VALUES (?, ?, ?)", record.seq, record.principal, JSON.stringify(record.body));
    return { seq: record.seq };
  }

  log() {
    const rows = this.ctx.storage.sql.exec("SELECT seq, principal, body FROM heard ORDER BY seq").toArray();
    return { heard: rows.map((r) => ({ seq: r.seq, principal: r.principal, body: JSON.parse(r.body) })) };
  }
}
