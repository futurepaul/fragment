// Effects the platform refuses, a commit that keeps failing, and a ledger
// row the app writes itself: none of it may stop the app, before or after
// a restart.
import { DurableObject } from "cloudflare:workers";

const POINTER = `version https://git-lfs.github.com/spec/v1\noid sha256:${"a".repeat(64)}\nsize 5\n`;

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY AUTOINCREMENT, slug TEXT NOT NULL)");
  }

  // a note: a row, a record that says so, and its file on main
  note({ slug, text }, call) {
    this.ctx.storage.sql.exec("INSERT INTO notes (slug) VALUES (?)", slug);
    call.publish("feed", { slug }, "noted");
    call.files.write(`notes/${slug}.md`, text);
    return { slug };
  }

  count() {
    return { n: this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM notes").one().n };
  }

  // a result of `n` characters: n + 2 bytes of JSON
  big({ n }) {
    return "x".repeat(n);
  }

  // a file whose text is a large-file pointer
  pointer(_input, call) {
    this.ctx.storage.sql.exec("INSERT INTO notes (slug) VALUES ('pointer')");
    call.files.write("big.bin", POINTER);
  }

  // 101 characters of three bytes each: within 300 characters, over 300 bytes
  wide(_input, call) {
    this.ctx.storage.sql.exec("INSERT INTO notes (slug) VALUES ('wide')");
    call.files.write("文".repeat(101), "x");
  }

  // a record holding half of an emoji (a string cut inside one)
  half(_input, call) {
    this.ctx.storage.sql.exec("INSERT INTO notes (slug) VALUES ('half')");
    call.publish("feed", { slug: "half", text: "\u{1F600}".slice(0, 1) }, "noted");
  }

  // the same pointer, past an in-app check the author's code broke
  sneaky(_input, call) {
    const decode = TextDecoder.prototype.decode;
    TextDecoder.prototype.decode = () => {
      throw new TypeError("patched");
    };
    try {
      this.ctx.storage.sql.exec("INSERT INTO notes (slug) VALUES ('sneaky')");
      call.files.write("big.bin", POINTER);
    } finally {
      TextDecoder.prototype.decode = decode;
    }
  }

  // a ledger row of the app's own making, naming its caller as the author
  forge(_input, call) {
    const effects = [{ channel: "feed", kind: "forged", body: { by: "the app" } }];
    this.ctx.storage.sql.exec(
      "INSERT INTO _fragment_ops (id, name, input_sha, result, at, effects) VALUES (?, 'note', 'forged', 'null', ?, ?)",
      `${call.principal}/forged-1`,
      Date.now(),
      JSON.stringify(effects),
    );
    return { ok: true };
  }
}
