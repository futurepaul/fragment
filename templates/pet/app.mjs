// The pet: the latest frame of its computer's screen, and who drove it
// last. The computer (computer/pet.mjs, an editor here) stores each frame
// through `frame`, and every page follows `screen` live. People drive it
// by posting to `control`, which the computer follows. Nothing here keeps
// history: one row, replaced.
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS screen (
      id INTEGER PRIMARY KEY CHECK (id = 1), jpeg TEXT NOT NULL, width INTEGER NOT NULL, height INTEGER NOT NULL,
      title TEXT NOT NULL, driver TEXT, at INTEGER NOT NULL)`);
  }

  frame({ jpeg, width, height, title = "", driver = null }) {
    // a JPEG's first bytes (FF D8 FF), in base64
    if (!jpeg.startsWith("/9j/")) throw new Error("a frame is a base64 JPEG");
    const at = Date.now();
    this.ctx.storage.sql.exec(
      "INSERT OR REPLACE INTO screen (id, jpeg, width, height, title, driver, at) VALUES (1, ?, ?, ?, ?, ?, ?)",
      jpeg, width, height, title, driver, at,
    );
    return { at };
  }

  // the screen as the computer last sent it, or null before its first frame
  screen() {
    return this.ctx.storage.sql.exec("SELECT jpeg, width, height, title, driver, at FROM screen").toArray()[0] ?? null;
  }
}
