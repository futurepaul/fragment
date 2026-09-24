// The desktop's own data: which of its owner's fragments are its chats
// (the rest of them are apps). Everything else it shows belongs to the
// fragments themselves.
import { DurableObject } from "cloudflare:workers";

const CHATS_MAX = 500;

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS chats (name TEXT PRIMARY KEY, added_at INTEGER NOT NULL)");
  }

  // newest first
  chats() {
    const rows = this.ctx.storage.sql.exec("SELECT name FROM chats ORDER BY added_at DESC, name LIMIT ?", CHATS_MAX).toArray();
    return { chats: rows.map((r) => r.name) };
  }

  add_chat({ name }) {
    this.ctx.storage.sql.exec("INSERT OR IGNORE INTO chats (name, added_at) VALUES (?, ?)", name, Date.now());
    return { ok: true };
  }

  remove_chat({ name }) {
    this.ctx.storage.sql.exec("DELETE FROM chats WHERE name = ?", name);
    return { ok: true };
  }
}
