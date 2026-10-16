// Notes on a page's paragraphs, as a document's comments are: a mixin any
// fragment's App takes, `class App extends withNotes(DurableObject) {}`.
// It brings one table and four operations (fragment.json declares them;
// README.md has the lines to copy):
//
//   notes       (query)    every note, oldest first
//   annotate    (mutation) a note on a paragraph, or a reply to a thread
//   resolve     (mutation) resolve a thread, or reopen it
//   remove_note (mutation) delete a note; a thread's first note takes its replies
//
// A note hangs on a paragraph by its `data-a` id (`anchor`); a reply hangs
// on its thread's first note, and a thread is flat (a reply to a reply
// joins the same thread). Whoever may call the operations reads the notes
// (their role is the fragment.json's: `public` in the template); people and
// agents who are signed in write them, and an anonymous visitor may not;
// only a note's author or the page's owner resolves or deletes it, and a
// delete is real. Every change also goes as a small record to the `notes`
// channel (kinds `annotated`, `resolved`, `reopened`, `removed`), for a
// page or a follower that wants the stream rather than the list.
//
// The verbs are the platform's to find on the App's prototype
// (cell/platform.mjs), so they are inherited methods here; an App that
// declares one of the same name would shadow it, which the constructor
// refuses (rename the app's own).

// What one page holds. The `notes` answer is the whole list, and an
// operation's answer is at most 1 MiB (limits::RESULT_MAX_BYTES), so the
// notes' text and quotes are held to a budget of bytes as they are
// answered (their JSON), beside the count: with every note at its largest
// frame, the answer still fits (asserted below).
export const NOTES_MAX = 1000;
export const NOTES_TEXT_MAX_BYTES = 640 * 1024;
export const TEXT_MAX_CHARS = 2000;
export const QUOTE_MAX_CHARS = 280;
export const ANCHOR = /^[A-Za-z0-9_-]{1,64}$/;
// A note's JSON without its text and quote: its keys, a 64-character
// anchor, two principals (an npub is 63 characters), its numbers, a comma.
const NOTE_FRAME_MAX_BYTES = 384;
const PRINCIPAL_MAX_CHARS = 64;
const RESULT_MAX_BYTES = 1024 * 1024;
const ANSWER_FRAME_BYTES = 64;
if (NOTES_MAX * NOTE_FRAME_MAX_BYTES + NOTES_TEXT_MAX_BYTES + ANSWER_FRAME_BYTES > RESULT_MAX_BYTES) {
  throw new Error("applib/notes.mjs: the notes' limits outgrow one answer");
}

const VERBS = ["notes", "annotate", "resolve", "remove_note"];
const ROLES = new Set(["public", "viewer", "editor", "owner"]);
const TABLE = "page_notes";
const encoder = new TextEncoder();

export function withNotes(Base) {
  class Notes extends Base {
    constructor(ctx, env) {
      super(ctx, env);
      for (const verb of VERBS) {
        if (this[verb] !== Notes.prototype[verb]) {
          throw new Error(`App.${verb} shadows the notes' own operation (applib/notes.mjs): give the app's another name`);
        }
      }
      // AUTOINCREMENT: a deleted note's id is never another's, so a
      // reply, resolve or delete sent with an old id finds nothing
      ctx.storage.sql.exec(`CREATE TABLE IF NOT EXISTS ${TABLE} (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        anchor TEXT NOT NULL,
        quote TEXT NOT NULL DEFAULT '',
        text TEXT NOT NULL,
        bytes INTEGER NOT NULL,
        author TEXT NOT NULL,
        agent TEXT,
        role TEXT NOT NULL,
        reply_to INTEGER,
        at INTEGER NOT NULL,
        resolved INTEGER NOT NULL DEFAULT 0
      )`);
    }

    notes() {
      const notes = this.ctx.storage.sql
        .exec(`SELECT id, anchor, quote, text, author, agent, role, reply_to, at, resolved FROM ${TABLE} ORDER BY id LIMIT ?`, NOTES_MAX)
        .toArray()
        .map((n) => ({ ...n, resolved: n.resolved === 1 }));
      return { notes };
    }

    annotate({ anchor, text, quote, reply_to }, call) {
      checkCaller(call);
      if (!signedIn(call)) throw new Error("Sign in to leave a note.");
      text = text.trim();
      const chars = [...text].length;
      if (chars === 0 || chars > TEXT_MAX_CHARS) throw new Error(`A note is 1 to ${TEXT_MAX_CHARS} characters.`);
      const sql = this.ctx.storage.sql;
      let root = null;
      if (reply_to === undefined) {
        if (typeof anchor !== "string" || !ANCHOR.test(anchor)) throw new Error("A note names the paragraph it is on: its data-a id.");
        quote = [...(quote ?? "").replace(/\s+/g, " ").trim()].slice(0, QUOTE_MAX_CHARS).join("");
      } else {
        root = threadOf(sql, reply_to);
        if (root === null) throw new Error("That note is gone.");
        if (anchor !== undefined && anchor !== root.anchor) throw new Error("A reply is on its thread's paragraph.");
        if (quote !== undefined) throw new Error("A reply quotes nothing: its thread's first note holds the quote.");
        anchor = root.anchor;
        quote = "";
      }
      const bytes = jsonBytes(text) + jsonBytes(quote);
      const held = sql.exec(`SELECT COUNT(*) AS n, COALESCE(SUM(bytes), 0) AS bytes FROM ${TABLE}`).one();
      if (held.n >= NOTES_MAX || held.bytes + bytes > NOTES_TEXT_MAX_BYTES) throw new Error("This page holds as many notes as it can: delete some first.");
      const at = Date.now();
      const { id } = sql
        .exec(
          `INSERT INTO ${TABLE} (anchor, quote, text, bytes, author, agent, role, reply_to, at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id`,
          anchor, quote, text, bytes, call.principal, call.agent ?? null, call.role, root?.id ?? null, at,
        )
        .one();
      // a reply to a resolved thread reopens it, as a document's does
      const reopened = root !== null && root.resolved === 1;
      if (reopened) sql.exec(`UPDATE ${TABLE} SET resolved = 0 WHERE id = ?`, root.id);
      const thread = root?.id ?? id;
      call.publish("notes", { id, thread, anchor, quote, text, by: call.principal, agent: call.agent ?? null, reopened }, "annotated");
      return { id, thread };
    }

    resolve({ id, resolved }, call) {
      checkCaller(call);
      const sql = this.ctx.storage.sql;
      const root = threadOf(sql, id);
      if (root === null) throw new Error("That note is gone.");
      if (!mayChange(root, call)) throw new Error("Only the thread's author or the page's owner resolves it.");
      const was = root.resolved === 1;
      if (was !== resolved) {
        sql.exec(`UPDATE ${TABLE} SET resolved = ? WHERE id = ?`, resolved ? 1 : 0, root.id);
        call.publish("notes", { id: root.id, anchor: root.anchor, by: call.principal, agent: call.agent ?? null }, resolved ? "resolved" : "reopened");
      }
      return { id: root.id, resolved };
    }

    remove_note({ id }, call) {
      checkCaller(call);
      const sql = this.ctx.storage.sql;
      const note = noteById(sql, id);
      // gone already: deleting it again changes nothing
      if (note === null) return { removed: 0 };
      if (!mayChange(note, call)) throw new Error("Only a note's author or the page's owner deletes it.");
      const removed = sql.exec(`DELETE FROM ${TABLE} WHERE id = ? OR reply_to = ? RETURNING id`, id, id).toArray().length;
      if (removed < 1 || (note.reply_to !== null && removed !== 1)) throw new Error("notes: a delete removed what it did not find");
      call.publish("notes", { id, thread: note.reply_to ?? id, anchor: note.anchor, removed, by: call.principal, agent: call.agent ?? null }, "removed");
      return { removed };
    }
  }
  return Notes;
}

function noteById(sql, id) {
  return sql.exec(`SELECT id, anchor, author, reply_to, resolved FROM ${TABLE} WHERE id = ?`, id).toArray()[0] ?? null;
}

// The thread a note is in: its first note.
function threadOf(sql, id) {
  const note = noteById(sql, id);
  if (note === null || note.reply_to === null) return note;
  const root = noteById(sql, note.reply_to);
  // a thread's first note takes its replies with it when it goes
  if (root === null || root.reply_to !== null) throw new Error(`notes: note ${id} hangs on no thread`);
  return root;
}

// The platform names every caller; what is stored of them fits the frame
// the limits above count on.
function checkCaller(call) {
  if (typeof call.principal !== "string" || call.principal.length === 0 || call.principal.length > PRINCIPAL_MAX_CHARS) {
    throw new Error("notes: a caller is named by a principal");
  }
  if (call.agent != null && (typeof call.agent !== "string" || call.agent.length > PRINCIPAL_MAX_CHARS)) throw new Error("notes: an agent is named by its npub");
  if (!ROLES.has(call.role)) throw new Error(`notes: no role ${call.role}`);
}

// A person or an agent. An anonymous visitor is `anon:…`; the fragment
// itself (a triggered run) is its own npub, and may leave notes too.
function signedIn(call) {
  return call.principal.startsWith("npub1");
}

function mayChange(note, call) {
  return call.role === "owner" || note.author === call.principal;
}

function jsonBytes(text) {
  return encoder.encode(JSON.stringify(text)).length;
}
