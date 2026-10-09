// The mind template's code (docs/optchat.md): one memory and the main
// agent, UniiChat's design (VictorTaelin's gist of 2026-10-08, kept as
// ~/dev/finite/uniichat-spec.md) in a fragment app. Every message a person
// says on `say`, every reply, tool call and result of the agent, and every
// report of its hands (goose, on the person's computer, through `chat` and
// `work`) goes into one log here, in SQLite. A compactor (up to JOBS `pump`
// runs at once, each building one node at a time) summarizes the log into a
// binary tree of one-line nodes, and each turn (job `heard`) starts fresh
// from the view: the whole log tiled by those nodes, the older the coarser
// (applib/optmem.mjs). Topics are Clef's (job `classify`).
//
// The views (the chat's and the compaction view), the nodes ready to build
// and the sawtooths' state are saved in SQLite as they change, and loaded
// at the instance's first call, never rebuilt from the log (§3.2): a mind
// saved before the views were is folded from its log once, at its first
// load. Every mutation that changes the memory bumps `rev` in kv, so an
// instance whose memory missed a change (a rolled-back mutation) loads it
// again.
//
// Jobs re-run from the top at every step (docs/api.md, Jobs): their
// control flow follows only their steps' answers. A compaction's input
// (the compaction view up to its node) is read from this instance when its
// step is built, not carried in a step's answer: what a past step was sent
// does not matter. A turn's view is the one exception, frozen by a step
// (`turn_view`) so every model call of the turn sees the same one.
import { DurableObject } from "cloudflare:workers";
import * as F from "./applib/files.mjs";
import * as M from "./applib/optmem.mjs";
import { CALL_TOOLS, SUGGEST, system, turnState } from "./applib/prompts.mjs";
import * as W from "./applib/web.mjs";

const THREAD = /^t_[0-9a-f]{16}$/;
const LOGGED_KINDS = new Set(["talk", "tool", "echo"]);
// The turn lock lasts this long after its last touch (docs/optchat.md).
const TURN_LOCK_MS = 15 * 60_000;
// Model calls one turn makes, at most; tool calls one answer runs, and
// answers (the rest are dropped from the answer: a mutation publishes at
// most 64 records, one per message it logs).
const CALLS_MAX = 40;
const TOOL_CALLS_MAX = 8;
const TOOL_CALLS_ANSWERED = 24;
// Past this much conversation after the view, the next call is the last
// (no tools): a step's arguments travel in a Workflow step of at most 1 MiB.
const CONVO_MAX_BYTES = 512 * 1024;
// The queued messages one turn takes, at most (the rest wait for the next).
const TEXTS_MAX_BYTES = 192 * 1024;
// A run's steps and their answers (limits::JOB_STEPS_MAX, JOB_RESULTS_MAX_BYTES):
// a turn starts only below TURN_START_STEPS, a settle takes at most
// SETTLE_STEPS, and RESERVE_STEPS are kept for a turn's end.
const STEPS_MAX = 256;
const RESERVE_STEPS = 8;
const TURN_START_STEPS = 64;
const SETTLE_STEPS = 96;
const RESULTS_SOFT_BYTES = 3 * 1024 * 1024;
// A web fetch's answer is kept whole (up to the 1 MiB a step's result
// takes): one is taken only while the run's 4 MiB of answers has room for it.
const FETCH_ROOM_BYTES = 4 * 1024 * 1024 - 1024 * 1024 - 64 * 1024;
// A turn's zoom of a computer task reads at most this many pages of its
// run's records (`job.records`, 200 a page: a goose turn takes at most 200
// steps).
const WORK_PAGES = 2;
// A web tool keeps this many steps for its answer's log, a last call and its log.
const WEB_KEEP_STEPS = 3;
const TURNS_PER_RUN = 4;
// A turn's wait gives up on a message whose node failed this many times.
const SETTLE_FAILS_MAX = 3;
// A node whose build failed waits this long before another run takes it
// again (and is taken again at the next message, §4).
const RETRY_MS = 10_000;
// A node a run took is that run's to build for this long.
const LEASE_MS = 5 * 60_000;
// A pump a step started counts as at work this long before its first step
// (so pumps at work and starting stay at most JOBS).
const SPAWN_MS = 60_000;
// A pump run keeps this many steps for a node (TRIES calls) and the pumps
// it starts; past it, a fresh run takes its place.
const PUMP_KEEP_STEPS = M.TRIES + M.JOBS + 2;
// A pump run passes at most this many nodes that failed (pump_step's skip).
const PUMP_SKIPS_MAX = 32;
const COMPACT_TOKENS = 4096;
// The compactor is started by an import when no pump is at work (a
// `compact` record, whose trigger runs `pump`), at most once in
// KICK_AGAIN_MS.
const KICK_AGAIN_MS = 60_000;
// An import's part: at most this many messages (docs/optchat.md, "Importing
// chats"); `imported` answers for at most this many conversations.
const IMPORT_MESSAGES_MAX = 64;
const IMPORTED_ASK_MAX = 200;
const IMPORT_SOURCE = /^[a-z][a-z0-9-]{0,31}$/;
const IMPORT_ID_MAX = 200;
// An imported message's text, at most (characters): a part's input is at
// most 256 KiB. One past CAP is logged as several messages in a row.
const IMPORT_TEXT_MAX = 128 * 1024;
// A node a stubborn model wrote over twice NODE is cut there.
const NODE_TEXT_MAX = 2 * M.NODE;
// A `msg` record's text (docs/optchat.md, "Records on log"), and the most
// a record's JSON may take of the platform's 64 KiB.
const RECORD_TEXT_MAX = 48 * 1024;
const RECORD_JSON_MAX = 60 * 1024;
const TASK_TEXT_MAX = 30 * 1024;
const TASK_RECORD_TEXT_MAX = 4096;
const TASK_RECORD_REPORT_MAX = 16 * 1024;
// A turn's timing on its `turn` record, at most.
const TIMING_MAX_BYTES = 16 * 1024;
// A hand-off with no reply this long after it opened is `lost`.
const TASK_LOST_MS = 30 * 60_000;
// Replies whose task is not recorded yet (its `task_open` a step behind),
// kept for it, at most this many.
const EARLY_REPLIES_MAX = 32;
// A turn's messages name its thread's last this many messages.
const THREAD_RECENT = 6;
// A mutation publishes at most 64 records: a turn's step logs at most this
// many pieces with a record each (a page reads the rest from `thread`).
const RECORDS_MAX = 60;
// A message heard (or a report) begins its turn in the same step when it is
// at most this many pieces: its records, the thread's and the turn's stay
// within a mutation's 64 (docs/optchat.md, "Latency").
const BEGIN_PIECES_MAX = 8;
// An operation's line in `apps`, at most (its description in full is app_ops').
const OP_LINE_MAX = 160;
const SEARCH_DEFAULT = 20;
const SEARCH_MAX = 50;
const SNIPPET_MAX_BYTES = 300;
const QUERY_WORDS_MAX = 16;
// What a page's query answers, at most (a result is at most 1 MiB).
const RESULT_SOFT_BYTES = 768 * 1024;
const CLEF_STATE_MAX = 48 * 1024;
const CLEF_LINE_MAX = 1024;
const CLEF_QUESTIONS_MAX = 64;
const CLEF_P = 0.6;
const CLASSIFY_THREADS = 100;
const CLASSIFY_SETS = 10;
const SUGGEST_MAX = 8;
const TITLE_MAX = 80;
const SUMMARY_MAX_BYTES = 600;
const PERSONAS_MAX = 32;
const TOPICS_MAX = 256;
const QUEUE_MAX = 1000;
const FAILS_KEPT = 64;
const ABOUT_MAX = 8000;
// `export`'s pages: messages a page, unless named, and at most.
const EXPORT_LIMIT = 500;
const EXPORT_MAX = 2000;

const PERSONAS = [
  {
    id: "mind",
    name: "Mind",
    emoji: "🌿",
    hands: 0,
    instructions:
      "Be plain, warm and brief. Talk like a friend who remembers everything the user ever told you: no headings or lists unless they help, no filler, at most one question.",
  },
  {
    id: "builder",
    name: "Builder",
    emoji: "🛠️",
    hands: 1,
    instructions:
      "You get things done on the user's computer. When a task needs files, a shell, code, or an app (a fragment) made or changed, hand it to the computer with everything it needs, say what you started, and report results plainly when they come back.",
  },
  {
    id: "coach",
    name: "Coach",
    emoji: "🧭",
    hands: 0,
    instructions:
      "Ask one question at a time, and wait for the answer. Help the user think it through rather than handing them answers: say back in one sentence what you heard before the next question.",
  },
  {
    id: "researcher",
    name: "Researcher",
    emoji: "🔎",
    hands: 0,
    instructions:
      "You find things out. Check anything current or uncertain on the web before you answer: research for a question that needs several sources, web_search and web_fetch for a quick look. Give your sources, and say plainly what you could not confirm.",
  },
];

const describe = (e) => (e instanceof Error ? e.message : String(e));
const clamp = (n, lo, hi) => Math.min(hi, Math.max(lo, Math.trunc(Number.isFinite(n) ? n : lo)));
const isInt = (n) => Number.isSafeInteger(n) && n >= 0;
const sizeOf = (v) => M.utf8(JSON.stringify(v ?? null) ?? "null");
const hex = (n) => [...crypto.getRandomValues(new Uint8Array(n))].map((b) => b.toString(16).padStart(2, "0")).join("");

function need(ok, why) {
  if (!ok) throw new Error(why);
}

// A record's text: at most RECORD_TEXT_MAX bytes, and its JSON within
// `room` (the record's limit, less what else it carries) however many
// characters it escapes.
function recordText(text, room = RECORD_JSON_MAX) {
  let t = M.cutBytes(text, RECORD_TEXT_MAX);
  while (M.utf8(JSON.stringify(t)) > room) t = M.cutBytes(t, Math.floor(M.utf8(t) * 0.8));
  return t;
}

// A thread's title: its first message's first line, short.
function titleOf(text) {
  const line = String(text).split("\n").map((l) => l.replace(/\s+/g, " ").trim()).find(Boolean) ?? "";
  const cps = Array.from(line);
  if (!cps.length) return "New chat";
  return cps.length > TITLE_MAX ? `${cps.slice(0, TITLE_MAX - 1).join("")}…` : line;
}

// Search words, and the FTS5 expression asking for all (or any) of them,
// each a word or a word's start.
const words = (q) => String(q).normalize("NFKC").toLowerCase().split(/[^\p{L}\p{N}\p{M}]+/u).filter(Boolean).slice(0, QUERY_WORDS_MAX);
const match = (ws, op) => ws.map((w) => `"${w.replaceAll('"', '""')}"*`).join(op);

// A Clef answer to a `noul` question (`{type: "noul", noul}`), as a
// probability; anything else is 0.
const pOf = (a) => (a && typeof a === "object" && Number.isFinite(a.noul) ? a.noul : 0);

// Topic names from a model's answer: `{"names": [...]}`, or a bare list.
function namesOf(text, have) {
  let list = [];
  try {
    const s = String(text);
    const at = s.search(/[[{]/);
    const v = JSON.parse(s.slice(at, Math.max(s.lastIndexOf("}"), s.lastIndexOf("]")) + 1));
    list = Array.isArray(v) ? v : Array.isArray(v?.names) ? v.names : [];
  } catch {
    list = [];
  }
  const seen = new Set(have.map((n) => n.toLowerCase()));
  const out = [];
  for (const n of list) {
    if (typeof n !== "string") continue;
    const name = n.replace(/\s+/g, " ").trim().slice(0, 48);
    if (!name || seen.has(name.toLowerCase())) continue;
    seen.add(name.toLowerCase());
    out.push(name);
    if (out.length >= SUGGEST_MAX) break;
  }
  return out;
}

// An imported conversation's thread: `t_` and 16 hex of FNV-1a 64 over its
// source and id, the same on every import of it (a mutation cannot await a
// SHA-256).
function importThread(source, id) {
  let h = 0xcbf29ce484222325n;
  for (const b of new TextEncoder().encode(`${source}\n${id}`)) {
    h ^= BigInt(b);
    h = (h * 0x100000001b3n) & 0xffffffffffffffffn;
  }
  return `t_${h.toString(16).padStart(16, "0")}`;
}

// The bridge's turn for a record (images/bridge/src/records.rs `turn_id`,
// docs/chat-records.md): 24 hex of SHA-256 of `<agent>|<fragment>/<channel>/<seq>`.
async function turnOf(agentFragment, fragment, channel, seq) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(`${agentFragment}|${fragment}/${channel}/${seq}`));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("").slice(0, 24);
}

// A hand-off's state from its one reply: goose's `(ended: <outcome>: <why>)`
// when it had no final words (idle: done), stopped, or failed; else done.
function endedBy(text) {
  const m = /^\(ended: ([a-z]+)/.exec(text.trim());
  return m === null || m[1] === "idle" ? "done" : m[1] === "stopped" ? "stopped" : "error";
}

// A computer task whole, as zoom("<task id>") answers it (§6's
// zoom("Name"), an agent's whole chat): what it was given, its state and
// times, goose's run, and its report whole. `run` is what a turn's zoom
// read of the run (`#zoomTask`: `{records, more, error}`), or null where
// none was read: the `zoom` query (the page's, an MCP client's) takes no
// steps, and only a job reads a channel.
function taskText(t, run) {
  const at = (ms) => new Date(ms).toISOString();
  const head = `Task ${t.id} (${t.state}) on the user's computer, from ${at(t.started)}${t.ended ? ` to ${at(t.ended)}` : ""}${isInt(t.i) ? `, handed off at message ${t.i}` : ""}.`;
  const lines = [head, "", "Given:", t.text, ""];
  if (run === null) lines.push("Its run on the computer: read by Mind's own zoom in a turn, not here.", "");
  else lines.push(...runLines(run), "");
  if (t.report === null) lines.push("No report yet.");
  else lines.push(`Its report${isInt(t.reported) ? ` (message ${t.reported})` : ""}:`, t.report);
  return lines.join("\n");
}

// goose's run from its records on `work` (docs/chat-records.md), in order:
// the words before each step, then the step (its tool and args, ok or
// failed, and its result's excerpt), each card it showed and how it
// closed, and how the turn ended.
function runLines({ records, more, error }) {
  const lines = ["Its run on the computer:"];
  const closed = new Map(records.filter((r) => r.body?.kind === "turn.prompt.closed").map((r) => [r.body.prompt, r.body]));
  for (const { body: b } of records) {
    if (b?.kind === "turn.step") {
      if (b.text) lines.push(String(b.text));
      const excerpt = b.excerpt ? `: ${b.excerpt}` : "";
      lines.push(`[step ${b.step}] ${b.tool}${b.args ? ` ${b.args}` : ""} → ${b.ok === false ? "failed" : "ok"}${excerpt}`);
    } else if (b?.kind === "turn.prompt") {
      const c = closed.get(b.prompt);
      const how = !c ? "open" : c.outcome === "answered" ? `answered: ${c.option}` : c.outcome;
      const options = Array.isArray(b.options) ? ` (${b.options.map((o) => o?.label ?? o?.id).join(" / ")})` : "";
      lines.push(`[asked] ${b.text}${options} → ${how}`);
    } else if (b?.kind === "turn.end") {
      lines.push(`[ended] ${b.outcome}${b.error ? `: ${b.error}` : ""}`);
    }
  }
  if (lines.length === 1 && !error) lines.push("(none yet: the computer has not taken it, or no longer keeps its records)");
  if (more) lines.push(`(more of it is on the computer's records than a zoom reads: its first ${records.length})`);
  if (error) lines.push(`(${records.length ? "the rest of its run was not read" : "its run could not be read"}: ${error})`);
  return lines;
}

// A task's text in zoom's pages (M.ZOOM_PAGE characters, as a long
// message's), each naming the next.
function taskPage(id, text, page) {
  const pages = M.splitText(text, M.ZOOM_PAGE);
  if (!isInt(page) || page < 1 || page > pages.length) return `Task ${id} has ${pages.length} page${pages.length > 1 ? "s" : ""}.`;
  const more = pages.length > 1 ? `\n[page ${page} of ${pages.length}${page < pages.length ? `: zoom("${id}", 1, ${page + 1}) gives the next` : ""}]` : "";
  return `${pages[page - 1]}${more}`;
}

// The user's apps for the model (the `apps` tool): each by name, with its
// title, kind, the user's role and its address, then its operations, a
// line each.
function appsText(fragments) {
  if (!fragments.length) return "The user has no apps besides this mind. The computer makes one.";
  const lines = ["The user's apps (app_ops shows one's inputs; app_call uses it):"];
  for (const f of fragments) {
    const title = f.title ? ` "${f.title}"` : "";
    lines.push(`- ${f.name}:${title} (${f.kind}, ${f.role}) ${f.url}`);
    const ops = Array.isArray(f.operations) ? f.operations : [];
    if (!ops.length) lines.push("  (no operations to use: the computer changes it)");
    for (const o of ops) lines.push(`  ${o.name} (${o.kind}): ${opLine(o.description)}`);
  }
  return lines.join("\n");
}

// An operation's description as one line, short.
function opLine(text) {
  const line = String(text ?? "").replace(/\s+/g, " ").trim();
  return line.length > OP_LINE_MAX ? `${line.slice(0, OP_LINE_MAX - 1).trimEnd()}…` : line;
}

// One app's operations in full (the `app_ops` tool), each with its input's schema.
function opsText(app) {
  const title = app.title ? ` "${app.title}"` : "";
  const lines = [`${app.name}:${title} (${app.kind}, ${app.role}) ${app.url}`];
  const ops = Array.isArray(app.operations) ? app.operations : [];
  if (!ops.length) lines.push("(no operations to use: the computer changes it)");
  for (const o of ops) lines.push(`${o.name} (${o.kind}): ${String(o.description ?? "").trim()}`, `  input: ${JSON.stringify(o.input ?? { type: "object" })}`);
  return lines.join("\n");
}

// An operation's result for the model: a `text` it rendered for one as it
// is, else its JSON (the MCP servers' rule, fragment_core::mcp `called`).
const resultText = (r) => (typeof r?.text === "string" ? r.text : (JSON.stringify(r ?? null) ?? "null"));

// A run's steps, counted with the size of their answers, so a run stays
// within the platform's 256 steps and 4 MiB of answers.
class Steps {
  constructor(job) {
    this.job = job;
    this.n = 0;
    this.bytes = 0;
  }

  #took(p) {
    this.n++;
    return p.then((v) => {
      this.bytes += sizeOf(v);
      return v;
    });
  }

  call(op, input = {}) {
    return this.#took(this.job.call(op, input));
  }

  text(opts) {
    return this.#took(this.job.ai.text(opts));
  }

  decide(opts) {
    return this.#took(this.job.ai.decide(opts));
  }

  publish(channel, body) {
    return this.#took(this.job.publish(channel, body));
  }

  fetch(url, init) {
    return this.#took(this.job.fetch(url, init));
  }

  blob(sha256) {
    return this.#took(this.job.blob(sha256));
  }

  records(channel, opts) {
    return this.#took(this.job.records(channel, opts));
  }

  sleep(ms) {
    return this.#took(this.job.sleep(ms));
  }

  members() {
    return this.#took(this.job.members());
  }

  people(ids) {
    return this.#took(this.job.people(ids));
  }

  ownerFragments() {
    return this.#took(this.job.owner.fragments());
  }

  ownerCall(fragment, op, input) {
    return this.#took(this.job.owner.call(fragment, op, input));
  }

  left() {
    return STEPS_MAX - RESERVE_STEPS - this.n;
  }
}

export class App extends DurableObject {
  // The memory loaded from what was saved, and the `rev` it is of (null:
  // unknown, loaded again at the next read).
  #mem = null;
  #rev = null;
  // This instance, for `status`: a restart is a new one.
  #instance = hex(4);
  // Records the current mutation published for messages (RECORDS_MAX).
  #records = 0;

  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec(`CREATE TABLE IF NOT EXISTS log (
      i INTEGER PRIMARY KEY, kind TEXT NOT NULL, text TEXT NOT NULL, at INTEGER NOT NULL, thread TEXT, persona TEXT, task TEXT)`);
    sql.exec("CREATE INDEX IF NOT EXISTS log_thread ON log (thread, i)");
    sql.exec("CREATE TABLE IF NOT EXISTS node (l INTEGER NOT NULL, i INTEGER NOT NULL, text TEXT NOT NULL, PRIMARY KEY (l, i))");
    // The views (§3.2: saved, never rebuilt from the log), a line a row by
    // its first message: the chat's (`vline`) and the compaction view
    // (`cline`). The nodes ready to build (§4, "The order"): `e` is the last
    // message one covers; `run` and `until`, a run's lease on it, or (`run`
    // null) when one that failed may be taken again.
    sql.exec("CREATE TABLE IF NOT EXISTS vline (s INTEGER PRIMARY KEY, l INTEGER NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS cline (s INTEGER PRIMARY KEY, l INTEGER NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS ready (l INTEGER NOT NULL, i INTEGER NOT NULL, e INTEGER NOT NULL, run INTEGER, until INTEGER, PRIMARY KEY (l, i))");
    sql.exec("CREATE INDEX IF NOT EXISTS ready_e ON ready (e, l)");
    sql.exec(`CREATE TABLE IF NOT EXISTS thread (
      id TEXT PRIMARY KEY, title TEXT NOT NULL, persona TEXT, started INTEGER NOT NULL, last INTEGER NOT NULL,
      first_i INTEGER NOT NULL, last_i INTEGER NOT NULL)`);
    sql.exec("CREATE INDEX IF NOT EXISTS thread_last ON thread (last)");
    sql.exec("CREATE TABLE IF NOT EXISTS topic (id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL, made INTEGER NOT NULL)");
    sql.exec("CREATE TABLE IF NOT EXISTS thread_topic (thread TEXT NOT NULL, topic TEXT NOT NULL, p REAL NOT NULL, PRIMARY KEY (thread, topic))");
    sql.exec("CREATE INDEX IF NOT EXISTS thread_topic_topic ON thread_topic (topic)");
    sql.exec(`CREATE TABLE IF NOT EXISTS persona (
      id TEXT PRIMARY KEY, name TEXT NOT NULL, emoji TEXT NOT NULL, instructions TEXT NOT NULL, hands INTEGER NOT NULL, made INTEGER NOT NULL)`);
    sql.exec(`CREATE TABLE IF NOT EXISTS task (
      id TEXT PRIMARY KEY, thread TEXT NOT NULL, i INTEGER, text TEXT NOT NULL, seq INTEGER, turn TEXT, state TEXT NOT NULL,
      report TEXT, steps TEXT NOT NULL, started INTEGER NOT NULL, ended INTEGER)`);
    sql.exec("CREATE INDEX IF NOT EXISTS task_seq ON task (seq)");
    sql.exec("CREATE INDEX IF NOT EXISTS task_turn ON task (turn)");
    sql.exec("CREATE INDEX IF NOT EXISTS task_thread ON task (thread, started)");
    sql.exec("CREATE TABLE IF NOT EXISTS kv (k TEXT PRIMARY KEY, v TEXT NOT NULL)");
    // what an import landed of each conversation: its first n messages
    sql.exec(`CREATE TABLE IF NOT EXISTS import (
      source TEXT NOT NULL, conv TEXT NOT NULL, thread TEXT NOT NULL, n INTEGER NOT NULL, at INTEGER NOT NULL, PRIMARY KEY (source, conv))`);
    sql.exec("CREATE VIRTUAL TABLE IF NOT EXISTS log_fts USING fts5(text, content='log', content_rowid='i')");
    // A message's files (applib/files.mjs), JSON, null with none; and
    // whether it goes on from the message before it (a long text is several
    // in a row, §1), 1 or null. A log made before them gains the columns.
    const columns = sql.exec("SELECT * FROM log LIMIT 0").columnNames;
    if (!columns.includes("attachments")) sql.exec("ALTER TABLE log ADD COLUMN attachments TEXT");
    if (!columns.includes("cont")) sql.exec("ALTER TABLE log ADD COLUMN cont INTEGER");
    // Whether a message is an import's (1, else null), and a node ready the
    // import's work (`imp`: its last message is an import's, 1, else 0):
    // docs/optchat.md, "Where we differ from the gist", 16. A mind made
    // before them gains them, its imports' messages marked: its imported
    // threads' `user` and `talk` messages with no persona (a live one always
    // has its persona).
    if (!columns.includes("imported")) {
      ctx.storage.transactionSync(() => {
        sql.exec("ALTER TABLE log ADD COLUMN imported INTEGER");
        sql.exec("UPDATE log SET imported = 1 WHERE persona IS NULL AND kind IN ('user', 'talk') AND thread IN (SELECT thread FROM import)");
      });
    }
    if (!sql.exec("SELECT * FROM ready LIMIT 0").columnNames.includes("imp")) {
      ctx.storage.transactionSync(() => {
        sql.exec("ALTER TABLE ready ADD COLUMN imp INTEGER NOT NULL DEFAULT 0");
        sql.exec("UPDATE ready SET imp = 1 WHERE e IN (SELECT i FROM log WHERE imported = 1)");
      });
    }
    sql.exec("CREATE INDEX IF NOT EXISTS log_imported ON log (i) WHERE imported = 1");
    sql.exec("CREATE INDEX IF NOT EXISTS ready_imp ON ready (imp, l, i)");
    sql.exec("CREATE INDEX IF NOT EXISTS ready_imp_e ON ready (imp, e, l)");
    // The seeded personas, each once: a mind made before one was seeded
    // (`seeded` counts them; three before it was kept) gains it, and one
    // its person removed stays removed.
    const fresh = sql.exec("SELECT COUNT(*) AS n FROM persona").one().n === 0;
    const seeded = fresh ? 0 : Number(this.#get("seeded") ?? 3);
    if (seeded < PERSONAS.length) {
      ctx.storage.transactionSync(() => {
        const now = Date.now();
        // listed in this order (by `made`)
        for (const [k, p] of PERSONAS.entries()) {
          if (k < seeded || sql.exec("SELECT id FROM persona WHERE id = ?", p.id).toArray().length) continue;
          sql.exec("INSERT INTO persona (id, name, emoji, instructions, hands, made) VALUES (?, ?, ?, ?, ?, ?)", p.id, p.name, p.emoji, p.instructions, p.hands, now + k);
        }
        if (fresh) this.#set("default", PERSONAS[0].id);
        this.#set("seeded", PERSONAS.length);
      });
    }
    // A mind made before its views were saved (the spec's first version
    // folded them again at every load): folded from its log once, here,
    // and saved.
    if (this.#get("saved") === null) ctx.storage.transactionSync(() => this.#migrate());
  }

  // ---- kv ----

  #get(k) {
    return this.ctx.storage.sql.exec("SELECT v FROM kv WHERE k = ?", k).toArray()[0]?.v ?? null;
  }

  #set(k, v) {
    this.ctx.storage.sql.exec("INSERT INTO kv (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v", k, String(v));
  }

  #del(k) {
    this.ctx.storage.sql.exec("DELETE FROM kv WHERE k = ?", k);
  }

  #json(k, otherwise) {
    const v = this.#get(k);
    if (v === null) return otherwise;
    try {
      return JSON.parse(v);
    } catch {
      return otherwise;
    }
  }

  #setJson(k, v) {
    this.#set(k, JSON.stringify(v));
  }

  // ---- the memory ----

  // T: the log's messages, its ids 0 to T - 1.
  #count() {
    const { n, top } = this.ctx.storage.sql.exec("SELECT COUNT(*) AS n, MAX(i) AS top FROM log").one();
    need(n === 0 || top === n - 1, `the log's ids are not 0 to ${n - 1}`);
    return n;
  }

  // The views and the nodes ready, built once from the log as it stands
  // (M.fold) and saved whole, for a mind made before they were saved; its
  // old leases go. `folds` counts these: one at most, a mind's life long.
  #migrate() {
    const sql = this.ctx.storage.sql;
    const T = this.#count();
    const m = M.fold(T, sql.exec("SELECT l, i, text FROM node"), (i) => this.#line(i), this.#importedIds());
    sql.exec("DELETE FROM vline");
    sql.exec("DELETE FROM cline");
    sql.exec("DELETE FROM ready");
    for (const p of m.v.lines) sql.exec("INSERT INTO vline (s, l) VALUES (?, ?)", M.startOf(p), p.l);
    for (const p of m.c.lines) sql.exec("INSERT INTO cline (s, l) VALUES (?, ?)", M.startOf(p), p.l);
    this.#set("vshrink", m.v.shrink ? 1 : 0);
    this.#set("cshrink", m.c.shrink ? 1 : 0);
    this.#flush(m);
    this.#del("busy");
    if (T > 0) this.#set("folds", Number(this.#get("folds") ?? 0) + 1);
    this.#set("saved", 1);
    this.#mem = m;
    this.#rev = this.#get("rev") ?? "0";
  }

  #memory() {
    const rev = this.#get("rev") ?? "0";
    if (this.#mem !== null && this.#rev === rev) return this.#mem;
    const sql = this.ctx.storage.sql;
    this.#mem = M.load({
      T: this.#count(),
      nodes: sql.exec("SELECT l, i, text FROM node"),
      v: sql.exec("SELECT s, l FROM vline ORDER BY s").toArray(),
      c: sql.exec("SELECT s, l FROM cline ORDER BY s").toArray(),
      vshrink: this.#get("vshrink") === "1",
      cshrink: this.#get("cshrink") === "1",
      imported: this.#importedIds(),
    });
    this.#rev = rev;
    return this.#mem;
  }

  // The imported messages' ids, ascending (by the partial index).
  #importedIds() {
    return this.ctx.storage.sql.exec("SELECT i FROM log WHERE imported = 1 ORDER BY i");
  }

  // What the memory did (its journal), written: the nodes built (gone from
  // those ready), the nodes ready, each view's lines, and each sawtooth's
  // state.
  #flush(m) {
    const sql = this.ctx.storage.sql;
    const table = (tag) => (tag === "v" ? "vline" : "cline");
    for (const e of m.journal) {
      switch (e[0]) {
        case "node":
          sql.exec("INSERT INTO node (l, i, text) VALUES (?, ?, ?)", e[1], e[2], e[3]);
          sql.exec("DELETE FROM ready WHERE l = ? AND i = ?", e[1], e[2]);
          break;
        case "ready":
          sql.exec(
            "INSERT INTO ready (l, i, e, imp) VALUES (?, ?, ?, ?) ON CONFLICT (l, i) DO NOTHING",
            e[1], e[2], M.endOf(e[1], e[2]), M.isImported(m, M.endOf(e[1], e[2])) ? 1 : 0,
          );
          break;
        case "line":
          sql.exec(`INSERT INTO ${table(e[1])} (s, l) VALUES (?, ?) ON CONFLICT (s) DO UPDATE SET l = excluded.l`, e[2], e[3]);
          break;
        case "drop":
          sql.exec(`DELETE FROM ${table(e[1])} WHERE s = ?`, e[2]);
          break;
        case "clear":
          sql.exec(`DELETE FROM ${table(e[1])}`);
          break;
        case "shrink":
          this.#set(e[1] === "v" ? "vshrink" : "cshrink", e[2] ? 1 : 0);
          break;
        default:
          throw new Error(`the memory's journal has no entry ${e[0]}`);
      }
    }
    m.journal.length = 0;
  }

  // A mutation that changes the memory: the memory is unknown while it
  // runs (an exception leaves it so, to be loaded again: the platform rolls
  // the mutation back), what it did is written, and it is of the new `rev`
  // once it is done (a rollback after, of the old one).
  #changing(fn) {
    const m = this.#memory();
    this.#rev = null;
    this.#records = 0;
    const out = fn(m);
    this.#flush(m);
    const rev = String(Number(this.#get("rev") ?? 0) + 1);
    this.#set("rev", rev);
    this.#rev = rev;
    return out;
  }

  // A log row as the page reads it: its files named, without their text,
  // and whether it goes on from the message before it.
  #shown(r) {
    const { cont, ...rest } = r;
    return { ...rest, attachments: F.described(F.filesOf(r.attachments)), cont: cont === 1 };
  }

  #message(i) {
    return this.ctx.storage.sql.exec("SELECT i, kind, text, at, thread, persona, task, attachments, cont FROM log WHERE i = ?", i).toArray()[0] ?? null;
  }

  // Message i as the memory reads it: its words and its files
  // (applib/files.mjs), and whether its text goes on from the message
  // before it, or in the one after.
  #full(i) {
    const sql = this.ctx.storage.sql;
    const r = sql.exec("SELECT kind, text, attachments, cont FROM log WHERE i = ?", i).one();
    const more = sql.exec("SELECT cont FROM log WHERE i = ?", i + 1).toArray()[0]?.cont === 1;
    return { kind: r.kind, text: F.rendered(r.text, F.filesOf(r.attachments)), cont: r.cont === 1, more };
  }

  #line(i) {
    const r = this.#full(i);
    return M.line0(r.kind, r.text);
  }

  // A text logged (§1): a message, or several in a row when it is past CAP
  // characters (each piece after the first `cont`), the first with its
  // files (`attachments`, applib/files.mjs: their text, when read, kept with
  // them); its thread touched, each piece's line appended to the memory,
  // and each published on `log` as a `msg` record (the files named, not
  // their text), at most RECORDS_MAX a mutation. An import's message
  // (`imported`) is marked so, keeps its own time and publishes no record
  // (a page reads an imported thread with `thread`). A tool's output is
  // clipped before it comes here; no other text is cut. Answers its first
  // id and how many it took.
  #log(call, m, kind, text, { thread = null, persona = null, task = null, attachments = [], at = null, imported = false } = {}) {
    const sql = this.ctx.storage.sql;
    at ??= Date.now();
    const pieces = M.splitText(text);
    const first = m.T;
    for (const [k, piece] of pieces.entries()) {
      const i = m.T;
      const files = k === 0 ? attachments : [];
      sql.exec(
        "INSERT INTO log (i, kind, text, at, thread, persona, task, attachments, cont, imported) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        i, kind, piece, at, thread, persona, task, files.length ? JSON.stringify(files) : null, k > 0 ? 1 : null, imported ? 1 : null,
      );
      sql.exec("INSERT INTO log_fts (rowid, text) VALUES (?, ?)", i, piece);
      if (thread !== null) sql.exec("UPDATE thread SET last = ?, last_i = ? WHERE id = ?", at, i, thread);
      M.append(m, M.line0(kind, F.rendered(piece, files)), imported);
      if (!imported && this.#records < RECORDS_MAX) {
        this.#records++;
        const named = F.described(files);
        const body = { type: "msg", i, kind, text: recordText(piece, RECORD_JSON_MAX - sizeOf(named)), thread, at, persona, task };
        if (named.length) body.attachments = named;
        if (k > 0) body.cont = true;
        call.publish("log", body);
      }
    }
    return { i: first, n: pieces.length, at };
  }

  // A new message from the person or the hands (not an import's): the
  // nodes whose build failed are tried again (§4: "A failed call is tried
  // again at the next message").
  #retryFailed() {
    const sql = this.ctx.storage.sql;
    for (const k of Object.keys(this.#json("fails", {}))) {
      const [l, i] = k.split(":").map(Number);
      sql.exec("UPDATE ready SET until = NULL WHERE l = ? AND i = ? AND run IS NULL", l, i);
    }
  }

  // ---- threads, personas, the queue, the lock ----

  #thread(id) {
    return this.ctx.storage.sql.exec("SELECT * FROM thread WHERE id = ?", id).toArray()[0] ?? null;
  }

  #persona(id) {
    const sql = this.ctx.storage.sql;
    const row =
      (id ? sql.exec("SELECT * FROM persona WHERE id = ?", id).toArray()[0] : null) ??
      sql.exec("SELECT * FROM persona WHERE id = ?", this.#get("default") ?? "").toArray()[0] ??
      sql.exec("SELECT * FROM persona ORDER BY made, id LIMIT 1").toArray()[0];
    need(row, "the mind has no persona");
    return { id: row.id, name: row.name, emoji: row.emoji, instructions: row.instructions, hands: row.hands === 1 };
  }

  // The turn lock, when one holds: a run's, touched within TURN_LOCK_MS.
  #lock(now) {
    const l = this.#json("turn", null);
    return l && now - l.touched <= TURN_LOCK_MS ? l : null;
  }

  // A message for a turn, `user` (the person's) or `work` (a hand-off's
  // report): logged in its thread (made on its first message, titled from
  // its first line) and queued. Answers whether a turn is running, which
  // takes it next (between its tool calls, or as its next turn).
  #enqueue(call, m, { kind = "user", text, thread, persona = null, task = null, attachments = [] }) {
    const sql = this.ctx.storage.sql;
    const now = Date.now();
    const t = this.#thread(thread);
    const p = this.#persona(persona ?? t?.persona ?? null).id;
    if (!t) {
      const title = titleOf(text.trim() ? text : (attachments[0]?.name ?? ""));
      sql.exec("INSERT INTO thread (id, title, persona, started, last, first_i, last_i) VALUES (?, ?, ?, ?, ?, ?, ?)", thread, title, p, now, now, m.T, m.T);
      call.publish("log", { type: "thread", id: thread, title });
    } else if (persona !== null && t.persona !== p) {
      sql.exec("UPDATE thread SET persona = ? WHERE id = ?", p, thread);
    }
    const queue = this.#json("queue", []);
    need(queue.length < QUEUE_MAX, `${QUEUE_MAX} messages already wait for a turn`);
    this.#retryFailed();
    const { i, n } = this.#log(call, m, kind, text, { thread, persona: p, task, attachments });
    queue.push({ i, n, thread });
    this.#setJson("queue", queue);
    return { i, n, running: this.#lock(now) !== null };
  }

  // A queued message as a turn reads it: its pieces joined, then its files
  // (applib/files.mjs); and its files named.
  #queued(q) {
    const rows = this.ctx.storage.sql.exec("SELECT text, attachments FROM log WHERE i >= ? AND i < ? ORDER BY i", q.i, q.i + (q.n ?? 1)).toArray();
    const files = F.filesOf(rows[0]?.attachments);
    return { text: F.rendered(rows.map((r) => r.text).join(""), files), files: F.described(files) };
  }

  // ---- internal operations: the jobs' halves (docs/optchat.md) ----

  // A person's message from `say`: words, files (their text, when its job
  // read it), or both. With `begin` ({run, agent}) and no turn running, the
  // turn begins in the same step (`begun`: turn_begin's answer).
  hear({ text, thread, persona = null, attachments = [], begin = null }, call) {
    const a = F.attachmentsOf(attachments, { texts: true });
    need(!a.error, `hear: ${a.error}`);
    need(typeof text === "string" && (text.trim().length > 0 || a.files.length > 0), "hear: text is words, or the message carries files");
    need(THREAD.test(thread), "hear: thread is t_ and 16 hex");
    need(persona === null || typeof persona === "string", "hear: persona is a persona's id");
    const q = this.#changing((m) => this.#enqueue(call, m, { text, thread, persona, attachments: a.files }));
    return this.#begins(q, begin, call);
  }

  // A message just queued (`q`, #enqueue's), and the turn begun with it when
  // none runs and `begin` asks (a step saved), as long as the records it
  // and the turn's start publish stay within a mutation's (the message's
  // pieces few, no hand-off found lost: else turn_begin is a step of its own).
  #begins(q, begin, call) {
    if (!begin || q.running || (q.n ?? 1) > BEGIN_PIECES_MAX) return q;
    const lost = this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM task WHERE state = 'running' AND started < ?", Date.now() - TASK_LOST_MS).one().n;
    if (lost > 0) return q;
    return { ...q, begun: this.turn_begin(begin, call) };
  }

  // The queued messages of `thread` (all of them, or up to TEXTS_MAX_BYTES
  // past the first), taken off the queue: their ids ({i, n}: a message's
  // pieces), their texts as a turn reads them, and their files (at most
  // FILES_MAX, which a hand-off of the turn carries).
  #take(thread) {
    const queue = this.#json("queue", []);
    const taken = [];
    const texts = [];
    const attachments = [];
    let bytes = 0;
    for (const q of queue) {
      if (q.thread !== thread) continue;
      const { text, files } = this.#queued(q);
      if (taken.length && bytes + M.utf8(text) > TEXTS_MAX_BYTES) break;
      bytes += M.utf8(text);
      taken.push({ i: q.i, n: q.n ?? 1 });
      texts.push(text);
      for (const f of files) if (attachments.length < F.FILES_MAX && !attachments.some((a) => a.sha256 === f.sha256)) attachments.push(f);
    }
    const took = new Set(taken.map((q) => q.i));
    this.#setJson("queue", queue.filter((q) => !took.has(q.i)));
    return { taken, texts, attachments };
  }

  // Takes the turn lock (free, run out, or this run's) and the queued
  // messages of the oldest thread waiting. `tail` is where the turn's view
  // stops: before the newest run of messages still waiting for an answer,
  // which the turn is given whole (§6: the view is rendered before the new
  // message is logged). `now` is the turn's time, which its messages say
  // (§6: per-turn state starts each message).
  turn_begin({ run, agent = null }, call) {
    const sql = this.ctx.storage.sql;
    const now = Date.now();
    if (agent === null) this.#del("agent");
    else this.#set("agent", agent);
    const held = this.#lock(now);
    if (held && held.run !== run) return { took: false, why: "a turn is running" };
    const queue = this.#json("queue", []);
    if (!queue.length) {
      if (held) this.#del("turn");
      return { took: false, why: "nothing is queued" };
    }
    const thread = queue[0].thread;
    const waiting = new Set(queue.flatMap((q) => Array.from({ length: q.n ?? 1 }, (_, k) => q.i + k)));
    const { taken, texts, attachments } = this.#take(thread);
    const m = this.#memory();
    let tail = m.T;
    while (tail > 0 && waiting.has(tail - 1)) tail--;
    const t = this.#thread(thread);
    const persona = this.#persona(t?.persona);
    // the thread, as the turn's messages name it: its title, when it began,
    // and its last messages before these (where to zoom in it)
    const recent = sql
      .exec("SELECT i FROM log WHERE thread = ? AND i < ? AND cont IS NULL ORDER BY i DESC LIMIT ?", thread, taken[0]?.i ?? tail, THREAD_RECENT)
      .toArray()
      .map((r) => r.i)
      .reverse();
    const chat = { id: thread, title: t?.title ?? "", started: t?.started ?? now, recent };
    // when its first message was logged: the turn's timing counts from there
    const asked = taken.length ? (sql.exec("SELECT at FROM log WHERE i = ?", taken[0].i).toArray()[0]?.at ?? null) : null;
    this.#setJson("turn", { run, thread, since: now, touched: now, stop: false });
    // an import not summarized yet is not waited for (its view passes it)
    const settled = this.#firstUnbuilt(m, true) >= tail;
    call.publish("log", { type: "turn", thread, state: settled ? "thinking" : "settling" });
    // hand-offs with no reply in TASK_LOST_MS are lost (a reply later still reports)
    for (const { id } of sql.exec("SELECT id FROM task WHERE state = 'running' AND started < ? LIMIT 32", now - TASK_LOST_MS).toArray()) {
      sql.exec("UPDATE task SET state = 'lost' WHERE id = ?", id);
      this.#publishTask(call, this.#task(id));
    }
    // the hands' profile as a turn last read it (`people`, a step: read once)
    const kept = this.#json("agent_profile", null);
    const profile = kept && kept.agent === agent ? kept : null;
    const begun = { took: true, thread, chat, persona, about: this.#get("about") ?? "", texts, taken, tail, settled, attachments, now, asked, profile };
    // nothing to wait for: the view is rendered now, as turn_view would (a step saved)
    if (settled) begun.view = this.turn_view({ upto: tail });
    return begun;
  }

  // The lock kept by its run; answers whether Stop was asked.
  turn_touch({ run }) {
    const l = this.#json("turn", null);
    if (!l || l.run !== run) return { held: false, stopped: false };
    l.touched = Date.now();
    this.#setJson("turn", l);
    return { held: true, stopped: l.stop === true };
  }

  // A turn's view, after its wait: the views fitted (§3.2: as at a new
  // message, so a backlog built since the last one, an import's, merges
  // before the turn sees it), then the chat's rendered up to the turn's
  // messages.
  turn_view({ upto }) {
    need(isInt(upto), "turn_view: upto is where the turn's messages start");
    return this.#changing((m) => {
      M.fit(m);
      const r = M.render(m, Math.min(upto, m.T));
      return { text: r.text, bytes: r.bytes, parts: r.parts, T: m.T, settled: r.settled, at: Date.now() };
    });
  }

  // One model call's messages, in order: its reply (talk), each tool call
  // (tool) and its result (echo). Touches the lock and answers whether Stop
  // was asked. With `take` (the call asked for tools: the turn goes on), the
  // messages of its thread queued since are taken: they reach it between
  // its tool calls (§6). `pump`: start one, none being at work while a node
  // is ready (the compactor keeps up with a long turn).
  logged({ run, thread, persona = null, entries, take = false, end = null }, call) {
    need(Array.isArray(entries) && entries.length <= 1 + 2 * TOOL_CALLS_ANSWERED, "logged: entries is a list");
    for (const e of entries) need(e && LOGGED_KINDS.has(e.kind) && typeof e.text === "string", "logged: each entry is {kind: talk|tool|echo, text}");
    need(end === null || (!take && typeof end === "object"), "logged: end is {state, timing?, profile?}, for a call that asked no tools");
    const sql = this.ctx.storage.sql;
    const out = this.#changing((m) => {
      const ids = [];
      for (const e of entries) {
        const task = typeof e.task === "string" ? e.task : null;
        const { i } = this.#log(call, m, e.kind, e.text, { thread, persona, task });
        if (e.kind === "tool" && task !== null) sql.exec("UPDATE task SET i = ? WHERE id = ? AND i IS NULL", i, task);
        ids.push(i);
      }
      const touched = this.turn_touch({ run });
      const heard = take && touched.held && !touched.stopped ? this.#take(thread) : { taken: [], texts: [], attachments: [] };
      // the nodes these messages readied, written before asking whether any is
      this.#flush(m);
      return { ids, ...touched, heard, pump: this.#kick(Date.now()), at: Date.now() };
    });
    if (end === null) return out;
    // the turn's last call: its end in the same step (turn_end's, a step saved)
    const timing = end.timing && typeof end.timing === "object" ? end.timing : null;
    const calls = Array.isArray(timing?.calls) ? timing.calls : [];
    if (calls.length && calls[calls.length - 1] && typeof calls[calls.length - 1] === "object") calls[calls.length - 1].logged ??= out.at;
    return { ...out, ended: this.turn_end({ run, thread, state: end.state, timing, profile: end.profile ?? null }, call) };
  }

  // The lock let go, and what ended published. `requeue` puts a turn's
  // messages ({i, n}) back first in line (a turn handed on to a fresh run).
  turn_end({ run, thread, state, error = null, requeue = [], timing = null, profile = null }, call) {
    need(["done", "stopped", "error", "settling"].includes(state), "turn_end: state is done, stopped, error or settling");
    // the hands' profile a turn read (`people`): kept for the next turns
    if (profile && typeof profile === "object" && typeof profile.agent === "string") {
      this.#setJson("agent_profile", { agent: profile.agent, fragment: typeof profile.fragment === "string" ? profile.fragment : null, username: typeof profile.username === "string" ? profile.username : null });
    }
    let queue = this.#json("queue", []);
    if (Array.isArray(requeue) && requeue.length) {
      const back = requeue
        .map((q) => (isInt(q) ? { i: q, n: 1 } : q))
        .filter((q) => q && isInt(q.i))
        .map((q) => ({ i: q.i, n: isInt(q.n) && q.n > 0 ? q.n : 1, thread }));
      const ids = new Set(back.map((q) => q.i));
      queue = [...back, ...queue.filter((q) => !ids.has(q.i))];
      this.#setJson("queue", queue);
    }
    const l = this.#json("turn", null);
    if (l && l.run === run) this.#del("turn");
    // how long the turn took (docs/optchat.md, "Latency"), its end now
    const timed = timing && typeof timing === "object" && sizeOf(timing) <= TIMING_MAX_BYTES ? { timing: { ...timing, end: Date.now() } } : {};
    call.publish("log", { type: "turn", thread, state, ...(error ? { error: String(error).slice(0, 2000) } : {}), ...timed });
    const topics = this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM topic").one().n;
    return { queued: queue.length, topics };
  }

  // ---- the compactor's queue (§4, "The order"; never a scan of the tree) ----
  //
  // An import's work (`imp`) comes after the live chat's, and does not hold
  // it back (docs/optchat.md, "Where we differ from the gist", 16): a live
  // message's node starts once fewer than JOBS live messages before it are
  // unbuilt, and a run takes live work first.

  // The first message not summarized yet, or T; with `live`, the first not
  // an import's (what a turn waits for).
  #firstUnbuilt(m, live = false) {
    return this.ctx.storage.sql.exec(`SELECT MIN(i) AS f FROM ready WHERE ${live ? "imp = 0 AND " : ""}l = 0`).one().f ?? m.T;
  }

  // The last message whose node may start: one starts once fewer than
  // JOBS messages before it are unbuilt, imported ones not counted for a
  // live one (`imp` 0); for an import's (1), all of them.
  #window(imp) {
    const live = imp === 0 ? "imp = 0 AND " : "";
    return this.ctx.storage.sql.exec(`SELECT i FROM ready WHERE ${live}l = 0 ORDER BY i LIMIT 1 OFFSET ?`, M.JOBS - 1).toArray()[0]?.i ?? Number.MAX_SAFE_INTEGER;
  }

  // Nodes leased now: compactions at work.
  #leased(now) {
    return this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM ready WHERE run IS NOT NULL AND until > ?", now).one().n;
  }

  // Nodes a run may take now, at most JOBS counted.
  #takeable(now) {
    const sql = this.ctx.storage.sql;
    const free = "(until IS NULL OR until <= ?)";
    let n = 0;
    for (const imp of [0, 1]) {
      n += sql.exec(`SELECT COUNT(*) AS n FROM (SELECT 1 FROM ready WHERE imp = ? AND l > 0 AND ${free} LIMIT ?)`, imp, now, M.JOBS).one().n;
      n += sql.exec(`SELECT COUNT(*) AS n FROM (SELECT 1 FROM ready WHERE imp = ? AND l = 0 AND i <= ? AND ${free} LIMIT ?)`, imp, this.#window(imp), now, M.JOBS).one().n;
    }
    return Math.min(M.JOBS, n);
  }

  // The next node a run may build: of the merges whose halves are built
  // and the messages that may start, the one whose last message is the
  // oldest (a merge before a message it ends with), the live chat's before
  // an import's; with `upto` (a turn's wait), a live message before it
  // only. One this run failed (`skip`) is passed.
  #next(now, upto, skip) {
    const sql = this.ctx.storage.sql;
    const free = "(until IS NULL OR until <= ?)";
    const want = skip.length + 1;
    const skipped = new Set(skip.map(String));
    const pick = (rows) => rows.find((r) => !skipped.has(M.key(r.l, r.i))) ?? null;
    if (upto !== null) return pick(sql.exec(`SELECT l, i, e FROM ready WHERE imp = 0 AND l = 0 AND i <= ? AND i < ? AND ${free} ORDER BY i LIMIT ?`, this.#window(0), upto, now, want).toArray());
    const of = (imp) => {
      const message = pick(sql.exec(`SELECT l, i, e FROM ready WHERE imp = ? AND l = 0 AND i <= ? AND ${free} ORDER BY i LIMIT ?`, imp, this.#window(imp), now, want).toArray());
      const merge = pick(sql.exec(`SELECT l, i, e FROM ready WHERE imp = ? AND l > 0 AND ${free} ORDER BY e, l LIMIT ?`, imp, now, want).toArray());
      if (!message || !merge) return message ?? merge;
      return merge.e <= message.e ? merge : message;
    };
    return of(0) ?? of(1);
  }

  // Pumps a step started and not at work yet (each counted SPAWN_MS).
  #spawns(now) {
    return this.#json("spawns", []).filter((t) => Number.isFinite(t) && t > now);
  }

  // Whether to start a pump: none is at work or starting while a node is
  // ready; if so, it is counted as starting.
  #kick(now) {
    const spawns = this.#spawns(now);
    if (spawns.length || this.#leased(now) || !this.#takeable(now)) return false;
    this.#setJson("spawns", [now + SPAWN_MS]);
    return true;
  }

  // The compactor's step (§4), a pump's or a turn's wait: the node this run
  // built written (`built`: {l, i, text}; or {l, i, error}, its failure
  // kept, the node tried again after RETRY_MS or at the next message), then
  // the next node it may build (`#next`) taken under a lease, while fewer
  // than JOBS are leased. Answers it (`node`, or null) and `spawn`, how many
  // pumps to start for what else is ready, each counted as at work until
  // its first step (`first`) or SPAWN_MS. `end`: the run takes nothing more,
  // `next` with a fresh run after it. With `upto` (a turn's wait, which holds
  // the turn lock): whether every message before it is summarized, but an
  // import's (`settled`), Stop was asked (`stopped`), or the first one that
  // is not failed SETTLE_FAILS_MAX times (`failing`), else a message before
  // it.
  pump_step({ run, built = null, skip = [], upto = null, first = false, end = false, next = false }) {
    need(isInt(run), "pump_step: run is the run's number");
    need(Array.isArray(skip) && skip.length <= 64, "pump_step: skip is at most 64 nodes");
    need(upto === null || isInt(upto), "pump_step: upto is a message's id");
    const now = Date.now();
    const sql = this.ctx.storage.sql;
    if (built !== null) {
      need(typeof built === "object" && isInt(built.l) && isInt(built.i), "pump_step: built is {l, i, text} or {l, i, error}");
      if (typeof built.text === "string" && built.text.trim()) this.#changing((m) => this.#nodeBuilt(m, built.l, built.i, built.text));
      else this.#nodeFailed(run, built.l, built.i, built.error ?? "the compactor wrote no line");
    }
    const spawns = this.#spawns(now);
    if (first && spawns.length) spawns.shift();
    if (next) spawns.push(now + SPAWN_MS);
    const out = { node: null, spawn: 0 };
    let taking = !end;
    if (upto !== null) {
      const f = this.#firstUnbuilt(this.#memory(), true);
      out.settled = f >= upto;
      const lock = this.#json("turn", null);
      if (lock && lock.run === run) {
        lock.touched = now;
        this.#setJson("turn", lock);
        out.stopped = lock.stop === true;
      }
      const fail = out.settled ? null : this.#json("fails", {})[M.key(0, f)];
      if (fail && fail.tries >= SETTLE_FAILS_MAX) out.failing = { id: f, error: fail.error };
      taking = taking && !out.settled && !out.stopped && !out.failing;
    }
    if (taking) {
      const leased = this.#leased(now);
      if (leased < M.JOBS) {
        const node = this.#next(now, upto, skip);
        if (node) {
          sql.exec("UPDATE ready SET run = ?, until = ? WHERE l = ? AND i = ?", run, now + LEASE_MS, node.l, node.i);
          out.node = { l: node.l, i: node.i };
          this.#set("pump_at", now);
        }
      }
      // more pumps for what else is ready, up to JOBS at work at once
      const more = Math.min(M.JOBS - this.#leased(now) - spawns.length, this.#takeable(now));
      for (let k = 0; k < more; k++) spawns.push(now + SPAWN_MS);
      out.spawn = Math.max(0, more);
    }
    this.#setJson("spawns", spawns);
    return out;
  }

  // A node's build failed: its lease let go, the node tried again after
  // RETRY_MS (or at the next message), the failure kept for `status`.
  #nodeFailed(run, l, i, error) {
    const k = M.key(l, i);
    this.ctx.storage.sql.exec("UPDATE ready SET run = NULL, until = ? WHERE l = ? AND i = ?", Date.now() + RETRY_MS, l, i);
    const fails = this.#json("fails", {});
    fails[k] = { id: i * 2 ** l, n: 2 ** l, error: String(error ?? "failed").slice(0, 500), at: Date.now(), tries: (fails[k]?.tries ?? 0) + 1, run };
    const keys = Object.keys(fails);
    if (keys.length > FAILS_KEPT) for (const old of keys.slice(0, keys.length - FAILS_KEPT)) delete fails[old];
    this.#setJson("fails", fails);
    return { built: false };
  }

  // A node built, in a #changing: the first write wins (a lease that ran
  // out may have let another run build it too), its failures forgotten, and
  // the memory told (its parent readied, the compaction view fitted).
  #nodeBuilt(m, l, i, text) {
    const k = M.key(l, i);
    if (M.isBuilt(m, l, i)) return { built: false, why: "built already" };
    need((i + 1) * 2 ** l <= m.T, `pump_step: ${M.nameOf(l, i)} covers messages past the log`);
    need(l === 0 || (M.isBuilt(m, l - 1, 2 * i) && M.isBuilt(m, l - 1, 2 * i + 1)), "pump_step: a parent comes after its children");
    M.setNode(m, l, i, M.cutBytes(text, NODE_TEXT_MAX));
    const fails = this.#json("fails", {});
    if (k in fails) {
      delete fails[k];
      this.#setJson("fails", fails);
    }
    return { built: true };
  }

  // A hand-off opened: its task, published on `chat` at `seq`, recorded
  // with the turn the agent's bridge gives that record. A reply that came
  // first (this a step behind it) is its report at once.
  task_open({ id, thread, text, seq, turn }, call) {
    need(typeof id === "string" && /^w\d+-\d+$/.test(id), "task_open: id is a task's");
    need(typeof thread === "string" && typeof text === "string" && isInt(seq), "task_open: {id, thread, text, seq, turn}");
    need(typeof turn === "string" && /^[0-9a-f]{24}$/.test(turn), "task_open: turn is the bridge's, 24 hex");
    const sql = this.ctx.storage.sql;
    return this.#changing((m) => {
      if (sql.exec("SELECT id FROM task WHERE id = ?", id).toArray().length) return { id, turn, opened: false };
      sql.exec(
        "INSERT INTO task (id, thread, i, text, seq, turn, state, report, steps, started, ended) VALUES (?, ?, NULL, ?, ?, ?, 'running', NULL, '[]', ?, NULL)",
        id, thread, text, seq, turn, Date.now(),
      );
      const early = this.#json("early", {});
      if (turn in early) {
        const { text: said, attachments = [] } = early[turn];
        delete early[turn];
        this.#setJson("early", early);
        return { id, turn, opened: true, ...this.#endTask(call, m, this.#task(id), said, attachments) };
      }
      this.#publishTask(call, this.#task(id));
      return { id, turn, opened: true };
    });
  }

  // A task as the page reads it: `lost` once it has waited TASK_LOST_MS
  // for its reply.
  #task(id) {
    const t = this.ctx.storage.sql.exec("SELECT * FROM task WHERE id = ?", id).toArray()[0] ?? null;
    return t && { ...t, state: t.state === "running" && Date.now() - t.started > TASK_LOST_MS ? "lost" : t.state };
  }

  #publishTask(call, t) {
    const body = { type: "task", id: t.id, thread: t.thread, state: t.state, text: M.cutBytes(t.text, TASK_RECORD_TEXT_MAX), turn: t.turn };
    if (t.report !== null && t.report !== undefined) body.report = M.cutBytes(t.report, TASK_RECORD_REPORT_MAX);
    call.publish("log", body);
  }

  // A hand-off's end: its one reply is its report, logged whole as a `work`
  // message `[<task>] …` in its thread (§1), with the reply's files, and
  // queued, which starts a turn (or reaches the running one between its
  // tool calls). A reply to a task that ended already (a later part)
  // changes nothing.
  #endTask(call, m, t, said, files = []) {
    if (t.report !== null) return { task: t.id, ended: false };
    const report = said.trim() || (files.length ? "(files)" : "(ended: idle: no words)");
    const state = endedBy(report);
    this.ctx.storage.sql.exec("UPDATE task SET state = ?, report = ?, ended = ? WHERE id = ?", state, report, Date.now(), t.id);
    this.#publishTask(call, { ...t, state, report });
    const q = this.#enqueue(call, m, { kind: "work", text: `[${t.id}] ${report}`, thread: t.thread, task: t.id, attachments: files });
    return { task: t.id, ended: true, running: q.running, i: q.i, n: q.n };
  }

  // A reply of goose's on `chat`, with its files (their text, when its job
  // read it): the report of the task its turn names. One whose task is not
  // recorded yet is kept for `task_open`.
  hands_reply({ turn, text, attachments = [], begin = null }, call) {
    need(typeof turn === "string" && typeof text === "string", "hands_reply: {turn, text}");
    const h = this.#handsReply({ turn, text, attachments }, call);
    // the report queued, its turn begun in the same step (`#begins`)
    return h.ended ? this.#begins(h, begin, call) : h;
  }

  #handsReply({ turn, text, attachments }, call) {
    const a = F.attachmentsOf(attachments, { texts: true });
    need(!a.error, `hands_reply: ${a.error}`);
    const id = this.ctx.storage.sql.exec("SELECT id FROM task WHERE turn = ?", turn).toArray()[0]?.id ?? null;
    if (id === null) {
      const early = this.#json("early", {});
      if (!(turn in early)) early[turn] = { text, attachments: a.files, at: Date.now() };
      const keys = Object.keys(early);
      if (keys.length > EARLY_REPLIES_MAX) for (const old of keys.slice(0, keys.length - EARLY_REPLIES_MAX)) delete early[old];
      this.#setJson("early", early);
      return { task: null, ended: false };
    }
    return this.#changing((m) => this.#endTask(call, m, this.#task(id), text, a.files));
  }

  // Clef's answers: for each thread, its topics among `scope` replaced by
  // those at p ≥ CLEF_P, and the thread's topics published.
  topics_set({ sets, scope }, call) {
    need(Array.isArray(sets) && sets.length <= CLASSIFY_SETS && Array.isArray(scope), "topics_set: {sets, scope}");
    const sql = this.ctx.storage.sql;
    const known = new Set(sql.exec("SELECT id FROM topic").toArray().map((r) => r.id));
    const asked = scope.filter((id) => typeof id === "string");
    for (const { thread, topics } of sets) {
      if (!this.#thread(thread)) continue;
      for (const id of asked) sql.exec("DELETE FROM thread_topic WHERE thread = ? AND topic = ?", thread, id);
      for (const { id, p } of Array.isArray(topics) ? topics : []) {
        if (known.has(id) && asked.includes(id) && Number.isFinite(p) && p >= CLEF_P) {
          sql.exec("INSERT INTO thread_topic (thread, topic, p) VALUES (?, ?, ?) ON CONFLICT (thread, topic) DO UPDATE SET p = excluded.p", thread, id, p);
        }
      }
      call.publish("log", { type: "topics", thread, topics: this.#threadTopics(thread) });
    }
    return { sets: sets.length };
  }

  // ---- what the page, the MCP server and goose ask ----

  // The rendered view, `<chat>…</chat>`: the parts that start before
  // `upto` (all by default), up to the first not summarized yet (an
  // import's passed, a marker line in its place).
  view({ upto = null } = {}) {
    const m = this.#memory();
    const r = M.render(m, upto ?? m.T);
    return { text: r.text, bytes: r.bytes, parts: r.parts, T: m.T, settled: r.settled };
  }

  // §6: zoom(id, n) opens a line; zoom(id, 1) gives a message whole, in
  // pages; zoom("<task id>") gives a computer task (`taskText`), in pages:
  // here (the page's, an MCP client's) without goose's run, which a turn's
  // zoom reads (`#zoomTask`).
  zoom({ id, n = 1, page = 1 }) {
    if (typeof id === "string" && !/^\d+$/.test(id)) {
      const { task } = this.task({ id: id.trim() });
      return { text: task ? taskPage(id, taskText(task, null), page) : `No task ${id}.` };
    }
    return { text: M.zoom(this.#memory(), Number(id), n, (i) => this.#full(i), page) };
  }

  // One computer task whole (a turn's zoom renders it with goose's run):
  // its state as `tasks` says it, and `reported`, its report's message.
  task({ id }) {
    const t = this.#task(id);
    if (!t) return { task: null };
    const reported = this.ctx.storage.sql.exec("SELECT i FROM log WHERE task = ? AND kind IN ('work', 'user') AND cont IS NULL ORDER BY i LIMIT 1", t.id).toArray()[0]?.i ?? null;
    return { task: { id: t.id, thread: t.thread, i: t.i, turn: t.turn, text: t.text, state: t.state, report: t.report, started: t.started, ended: t.ended, reported } };
  }

  date({ id }) {
    const m = isInt(id) ? this.#message(id) : null;
    return { text: m ? new Date(m.at).toISOString() : `No message ${id}.` };
  }

  #search(q, limit, thread) {
    const ws = words(q);
    if (!ws.length) return [];
    const sql = this.ctx.storage.sql;
    const one = thread ? " AND l.thread = ?" : "";
    const run = (expr) =>
      sql
        .exec(
          `SELECT l.i AS i, l.kind AS kind, l.thread AS thread, l.at AS at, snippet(log_fts, 0, '', '', '…', 16) AS snippet
           FROM log_fts JOIN log l ON l.i = log_fts.rowid WHERE log_fts MATCH ?${one} ORDER BY l.i DESC LIMIT ?`,
          ...(thread ? [expr, thread, limit] : [expr, limit]),
        )
        .toArray();
    let rows = run(match(ws, " "));
    if (!rows.length && ws.length > 1) rows = run(match(ws, " OR "));
    return rows.map((r) => ({ i: r.i, kind: r.kind, thread: r.thread, at: r.at, snippet: M.cutBytes(M.flat(r.snippet).trim(), SNIPPET_MAX_BYTES) }));
  }

  // The page's Search and an MCP client's; never a turn's tool (§5: zoom is
  // the only way a turn navigates the memory).
  search({ q, limit = SEARCH_DEFAULT, thread = null }) {
    return { results: this.#search(q, clamp(limit, 1, SEARCH_MAX), thread) };
  }

  // A note: something to remember, from an MCP client (or an import).
  note({ text }, call) {
    need(typeof text === "string" && text.trim().length > 0, "note: text is words");
    return this.#changing((m) => ({ i: this.#log(call, m, "note", text).i }));
  }

  // ---- importing chats (docs/optchat.md, "Importing chats") ----

  // Another agent's chat played into the log: a part of one conversation,
  // its messages `from` on, appended in order with their own times as
  // `user` and `talk` (a text past CAP as several messages in a row, §1),
  // in the conversation's thread (made on its first part, titled from it).
  // No turn is started: an import plays history, it asks the agent nothing.
  // A part already landed changes nothing, so a retry or a rerun resumes;
  // one past what landed is refused (parts go in order); `landed` counts
  // the conversation's messages, not their pieces. The compactor is started
  // when none is at work.
  import({ source, conversation, from = 0, total = null, messages }, call) {
    need(typeof source === "string" && IMPORT_SOURCE.test(source), "import: source is a word (claude-code, codex, …)");
    const id = conversation?.id;
    need(typeof id === "string" && id.length >= 1 && id.length <= IMPORT_ID_MAX, `import: conversation.id is 1 to ${IMPORT_ID_MAX} characters`);
    need(isInt(from), "import: from is the index of the part's first message");
    need(Array.isArray(messages) && messages.length >= 1 && messages.length <= IMPORT_MESSAGES_MAX, `import: 1 to ${IMPORT_MESSAGES_MAX} messages`);
    for (const msg of messages) {
      need(msg && (msg.role === "user" || msg.role === "assistant") && typeof msg.text === "string" && msg.text.trim().length > 0 && isInt(msg.at), "import: each message is {role: user|assistant, text, at}");
      need(msg.text.length <= IMPORT_TEXT_MAX, `import: a message's text is at most ${IMPORT_TEXT_MAX} characters`);
    }
    const sql = this.ctx.storage.sql;
    const thread = importThread(source, id);
    const landed = sql.exec("SELECT n FROM import WHERE source = ? AND conv = ?", source, id).toArray()[0]?.n ?? 0;
    need(from <= landed, `import: ${source} ${id} has ${landed} messages in; a part from ${from} is ahead of them`);
    const fresh = messages.slice(landed - from);
    if (!fresh.length) return { thread, landed, appended: 0, T: this.#memory().T };
    const now = Date.now();
    return this.#changing((m) => {
      if (!this.#thread(thread)) {
        const named = typeof conversation.title === "string" ? titleOf(conversation.title) : "";
        const title = named && named !== "New chat" ? named : titleOf((fresh.find((x) => x.role === "user") ?? fresh[0]).text);
        const started = isInt(conversation.started) ? Math.min(conversation.started, fresh[0].at) : fresh[0].at;
        sql.exec(
          "INSERT INTO thread (id, title, persona, started, last, first_i, last_i) VALUES (?, ?, ?, ?, ?, ?, ?)",
          thread, title, this.#persona(null).id, started, fresh[0].at, m.T, m.T,
        );
        call.publish("log", { type: "thread", id: thread, title });
      }
      for (const msg of fresh) this.#log(call, m, msg.role === "user" ? "user" : "talk", msg.text, { thread, at: msg.at, imported: true });
      const n = landed + fresh.length;
      sql.exec(
        "INSERT INTO import (source, conv, thread, n, at) VALUES (?, ?, ?, ?, ?) ON CONFLICT (source, conv) DO UPDATE SET n = excluded.n, at = excluded.at",
        source, id, thread, n, now,
      );
      call.publish("log", { type: "import", source, conversation: id, thread, n, total: isInt(total) ? total : null, T: m.T });
      // a pump at work (or starting) goes on; else one is started, at most
      // one a minute (each is a triggered run, under the hourly breaker),
      // and it starts the others (pump_step's `spawn`)
      this.#flush(m);
      if (now - Number(this.#get("kicked") ?? 0) > KICK_AGAIN_MS && this.#kick(now)) {
        this.#set("kicked", now);
        call.publish("compact", { at: now });
      }
      return { thread, landed: n, appended: fresh.length, T: m.T };
    });
  }

  // How many messages of each conversation an import landed (0: none).
  imported({ conversations }) {
    need(Array.isArray(conversations) && conversations.length <= IMPORTED_ASK_MAX, `imported: at most ${IMPORTED_ASK_MAX} conversations`);
    const sql = this.ctx.storage.sql;
    return {
      landed: conversations.map((c) => sql.exec("SELECT n FROM import WHERE source = ? AND conv = ?", String(c?.source ?? ""), String(c?.id ?? "")).toArray()[0]?.n ?? 0),
    };
  }

  #threadTopics(thread) {
    return this.ctx.storage.sql.exec("SELECT topic AS id, p FROM thread_topic WHERE thread = ? ORDER BY p DESC", thread).toArray();
  }

  // A thread in one line: the smallest built node over its messages, or
  // its first message's first line.
  #summary(m, t) {
    const c = M.covering(m, t.first_i, t.last_i);
    if (c) return M.cutBytes(M.flat(c.text), SUMMARY_MAX_BYTES);
    const first = this.ctx.storage.sql.exec("SELECT text FROM log WHERE thread = ? AND kind = 'user' ORDER BY i LIMIT 1", t.id).toArray()[0];
    return first ? titleOf(first.text) : t.title;
  }

  threads({ topic = null, before = null, limit = 30 } = {}) {
    const sql = this.ctx.storage.sql;
    const where = [];
    const args = [];
    if (topic !== null) {
      where.push("id IN (SELECT thread FROM thread_topic WHERE topic = ?)");
      args.push(topic);
    }
    if (before !== null) {
      where.push("last < ?");
      args.push(before);
    }
    const rows = sql.exec(`SELECT * FROM thread${where.length ? ` WHERE ${where.join(" AND ")}` : ""} ORDER BY last DESC LIMIT ?`, ...args, clamp(limit, 1, 100)).toArray();
    const m = this.#memory();
    return {
      threads: rows.map((t) => ({
        id: t.id,
        title: t.title,
        persona: t.persona,
        started: t.started,
        last: t.last,
        summary: this.#summary(m, t),
        topics: this.#threadTopics(t.id),
        count: sql.exec("SELECT COUNT(*) AS n FROM log WHERE thread = ? AND kind IN ('user', 'talk') AND cont IS NULL", t.id).one().n,
      })),
    };
  }

  // Messages newest first until the result's budget, answered oldest first.
  #page(rows) {
    const out = [];
    let bytes = 0;
    for (const r of rows) {
      bytes += sizeOf(r);
      if (out.length && bytes > RESULT_SOFT_BYTES) return { messages: out.reverse(), cut: true };
      out.push(r);
    }
    return { messages: out.reverse(), cut: false };
  }

  thread({ id, before = null, limit = 100 }) {
    const t = this.#thread(id);
    need(t, `no thread ${id}`);
    const n = clamp(limit, 1, 200);
    const rows = this.ctx.storage.sql
      .exec(
        `SELECT i, kind, text, at, persona, task, attachments, cont FROM log WHERE thread = ?${before !== null ? " AND i < ?" : ""} ORDER BY i DESC LIMIT ?`,
        ...(before !== null ? [id, before, n + 1] : [id, n + 1]),
      )
      .toArray();
    const { messages, cut } = this.#page(rows.slice(0, n).map((r) => this.#shown(r)));
    return { thread: { id: t.id, title: t.title, persona: t.persona, started: t.started, last: t.last }, messages, more: rows.length > n || cut };
  }

  context({ i, before = 5, after = 5 }) {
    need(isInt(i), "context: i is a message's id");
    const lo = Math.max(0, i - clamp(before, 0, 50));
    const hi = i + clamp(after, 0, 50);
    const rows = this.ctx.storage.sql.exec("SELECT i, kind, text, at, thread, persona, task, attachments, cont FROM log WHERE i BETWEEN ? AND ? ORDER BY i DESC", lo, hi).toArray();
    return { messages: this.#page(rows.map((r) => this.#shown(r))).messages };
  }

  // The raw log, a page at a time (the page's "Export memory"): every
  // message after `after` (all from the first by default), oldest first,
  // at most `limit` and RESULT_SOFT_BYTES of them, each with its files and
  // the text the mind read of them. `next` is the `after` of the next page;
  // null, the log's end.
  export({ after = null, limit = EXPORT_LIMIT } = {}) {
    need(after === null || isInt(after), "export: after is a message's id");
    const n = clamp(limit, 1, EXPORT_MAX);
    const rows = this.ctx.storage.sql
      .exec("SELECT i, kind, text, at, thread, persona, task, attachments, cont FROM log WHERE i > ? ORDER BY i LIMIT ?", after ?? -1, n + 1)
      .toArray();
    const entries = [];
    let bytes = 0;
    for (const r of rows.slice(0, n)) {
      const e = { ...r, attachments: F.filesOf(r.attachments), cont: r.cont === 1 };
      bytes += sizeOf(e);
      if (entries.length && bytes > RESULT_SOFT_BYTES) break;
      entries.push(e);
    }
    return { entries, next: entries.length < rows.length ? entries[entries.length - 1].i : null };
  }

  memory() {
    const m = this.#memory();
    let parts = M.parts(m);
    let cut = 0;
    let bytes = 0;
    for (let k = 0; k < parts.length; k++) {
      bytes += M.utf8(parts[k].text) + 48;
      if (bytes > RESULT_SOFT_BYTES) {
        cut = parts.length - k;
        parts = parts.slice(0, k);
        break;
      }
    }
    return { parts, bytes: m.v.bytes, T: m.T, ...(cut ? { cut } : {}) };
  }

  node({ id, n }) {
    const m = this.#memory();
    need(M.isLine(m, id, n), `no line ${id}+${n}`);
    if (n === 1) return { message: this.#shown(this.#message(id)) };
    const l = Math.round(Math.log2(n)) - 1;
    const h = n / 2;
    return {
      children: [2 * (id / n), 2 * (id / n) + 1].map((c) => ({ id: c * h, n: h, text: M.built(m, l, c), built: M.isBuilt(m, l, c) })),
    };
  }

  topics() {
    const rows = this.ctx.storage.sql
      .exec("SELECT t.id, t.name, t.description, (SELECT COUNT(*) FROM thread_topic tt WHERE tt.topic = t.id) AS count FROM topic t ORDER BY t.made, t.id")
      .toArray();
    return { topics: rows };
  }

  personas() {
    const rows = this.ctx.storage.sql.exec("SELECT id, name, emoji, instructions, hands FROM persona ORDER BY made, id").toArray();
    return { personas: rows.map((p) => ({ ...p, hands: p.hands === 1 })), default: this.#persona(null).id };
  }

  tasks({ thread = null } = {}) {
    const rows = this.ctx.storage.sql
      .exec(`SELECT * FROM task${thread !== null ? " WHERE thread = ?" : ""} ORDER BY started DESC LIMIT 50`, ...(thread !== null ? [thread] : []))
      .toArray();
    // newest first until the result's budget; a report whole is its message's
    const tasks = [];
    let bytes = 0;
    const now = Date.now();
    for (const t of rows) {
      const one = {
        id: t.id,
        thread: t.thread,
        i: t.i,
        turn: t.turn,
        // its record on `chat` (where goose's reply and claim name it)
        seq: t.seq,
        text: M.cutBytes(t.text, TASK_RECORD_TEXT_MAX),
        state: t.state === "running" && now - t.started > TASK_LOST_MS ? "lost" : t.state,
        report: t.report === null ? null : M.cutBytes(t.report, TASK_RECORD_REPORT_MAX),
        started: t.started,
        ended: t.ended,
      };
      bytes += sizeOf(one);
      if (tasks.length && bytes > RESULT_SOFT_BYTES) break;
      tasks.push(one);
    }
    return { tasks };
  }

  status() {
    const m = this.#memory();
    const now = Date.now();
    const l = this.#lock(now);
    const imp = this.ctx.storage.sql.exec("SELECT COUNT(*) AS conversations, COALESCE(SUM(n), 0) AS messages, MAX(at) AS last FROM import").one();
    const pumped = Number(this.#get("pump_at") ?? 0);
    const sql = this.ctx.storage.sql;
    const left = sql.exec("SELECT COUNT(*) AS n FROM ready").one().n;
    return {
      turn: l ? { running: true, thread: l.thread, since: l.since } : null,
      queued: this.#json("queue", []).length,
      unbuilt: m.T - this.#firstUnbuilt(m),
      T: m.T,
      hands: this.#get("agent") !== null,
      failing: Object.values(this.#json("fails", {})).map((f) => ({ id: f.id, n: f.n, error: f.error, tries: f.tries })),
      // the compactor: whether nodes are left to build (`ready`, how many),
      // the compactions at work now (`pumps`) and when one was last taken,
      // and the views' sizes (the chat's, and the compaction view's)
      ready: left > 0,
      left,
      pumps: this.#leased(now),
      pump: pumped ? { at: pumped } : null,
      view: m.v.bytes,
      cview: m.c.bytes,
      nodes: sql.exec("SELECT COUNT(*) AS n FROM node").one().n,
      import: imp.conversations ? { conversations: imp.conversations, messages: imp.messages, last: imp.last } : null,
      // this instance (a restart is another), and how many times the views
      // were built from the log (a mind made before they were saved: once)
      instance: this.#instance,
      folds: Number(this.#get("folds") ?? 0),
      now,
    };
  }

  settings() {
    return { about: this.#get("about") ?? "" };
  }

  // ---- the page's mutations ----

  topic_add({ name, description = "" }, call) {
    const sql = this.ctx.storage.sql;
    const nm = String(name).replace(/\s+/g, " ").trim();
    need(nm.length > 0, "topic_add: a topic has a name");
    const have = sql.exec("SELECT id FROM topic WHERE lower(name) = lower(?)", nm).toArray()[0];
    if (have) return { id: have.id, made: false };
    need(sql.exec("SELECT COUNT(*) AS n FROM topic").one().n < TOPICS_MAX, `a mind has at most ${TOPICS_MAX} topics`);
    const id = `c_${hex(6)}`;
    sql.exec("INSERT INTO topic (id, name, description, made) VALUES (?, ?, ?, ?)", id, nm, String(description).trim(), Date.now());
    // its trigger starts `classify` over the newest threads
    call.publish("sort", { topic: id });
    return { id, made: true };
  }

  topic_remove({ id }) {
    const sql = this.ctx.storage.sql;
    sql.exec("DELETE FROM thread_topic WHERE topic = ?", id);
    const gone = sql.exec("DELETE FROM topic WHERE id = ? RETURNING id", id).toArray().length > 0;
    return { id, removed: gone };
  }

  persona_set({ id = null, name, emoji, instructions, hands }) {
    const sql = this.ctx.storage.sql;
    if (id !== null) {
      need(sql.exec("SELECT id FROM persona WHERE id = ?", id).toArray().length, `no persona ${id}`);
      sql.exec("UPDATE persona SET name = ?, emoji = ?, instructions = ?, hands = ? WHERE id = ?", name.trim(), emoji, instructions, hands ? 1 : 0, id);
      return { id };
    }
    need(sql.exec("SELECT COUNT(*) AS n FROM persona").one().n < PERSONAS_MAX, `a mind has at most ${PERSONAS_MAX} personas`);
    const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "").slice(0, 24) || "persona";
    let made = slug;
    while (sql.exec("SELECT id FROM persona WHERE id = ?", made).toArray().length) made = `${slug}-${hex(2)}`;
    sql.exec("INSERT INTO persona (id, name, emoji, instructions, hands, made) VALUES (?, ?, ?, ?, ?, ?)", made, name.trim(), emoji, instructions, hands ? 1 : 0, Date.now());
    return { id: made };
  }

  persona_remove({ id }) {
    const sql = this.ctx.storage.sql;
    need(this.#persona(null).id !== id, "the default persona stays: make another the default first");
    sql.exec("DELETE FROM persona WHERE id = ?", id);
    return { id };
  }

  persona_default({ id }) {
    need(this.ctx.storage.sql.exec("SELECT id FROM persona WHERE id = ?", id).toArray().length, `no persona ${id}`);
    this.#set("default", id);
    return { id };
  }

  settings_set({ about }) {
    need(typeof about === "string" && about.length <= ABOUT_MAX, `about is at most ${ABOUT_MAX} characters`);
    this.#set("about", about);
    return { about };
  }

  // Stop: the running turn of `thread` stops at its next step.
  stop({ thread }) {
    const l = this.#lock(Date.now());
    if (!l || l.thread !== thread) return { stopped: false };
    l.stop = true;
    this.#setJson("turn", l);
    return { stopped: true };
  }

  // ---- jobs ----

  // `say`'s trigger: a person's message heard, then turns until none is
  // queued. Also a fresh run of the turns another run handed on (`resume`).
  async heard(input = {}, job) {
    const s = new Steps(job);
    const record = input?.record;
    if (record) {
      if (job.via !== "channel") return { why: "only the say channel's trigger hears a record" };
      const b = record.body ?? {};
      const a = F.attachmentsOf(b.attachments);
      if (a.error) return { why: `a say record: ${a.error}` };
      const text = typeof b.text === "string" ? b.text : "";
      if ((!text.trim() && !a.files.length) || typeof b.thread !== "string" || !THREAD.test(b.thread)) {
        return { why: "a say record is {text, thread, persona?, attachments?}, words or files, thread t_ and 16 hex" };
      }
      const attachments = await this.#readFiles(job, s, a.files);
      // the hands first: the turn begins with the message, in its step
      const lead = await this.#lead(s);
      const h = await s.call("hear", { text, thread: b.thread, persona: typeof b.persona === "string" ? b.persona : null, attachments, begin: { run: job.run, agent: lead.agent } });
      if (h.running) return { i: h.i, queued: true };
      return this.#turns(job, s, lead, h.begun ?? null);
    }
    return this.#turns(job, s);
  }

  // A message's files, those the memory reads whole (small text:
  // applib/files.mjs) with their text, read by `job.blob` (a step each, up
  // to READ_TOTAL_MAX in all). A blob that is gone, or no text, stays named
  // alone; so does every file on a platform without `job.blob`.
  async #readFiles(job, s, files) {
    if (typeof job.blob !== "function") return files;
    const out = [];
    let read = 0;
    for (const f of files) {
      if (F.readable(f) && read + f.size <= F.READ_TOTAL_MAX) {
        let r = null;
        try {
          r = await s.blob(f.sha256);
        } catch {
          r = null;
        }
        if (typeof r?.text === "string") {
          read += f.size;
          out.push({ ...f, text: r.cut ? `${r.text}\n[… the file goes on past its first ${F.bytesText(F.READ_MAX)} …]` : r.text });
          continue;
        }
      }
      out.push(f);
    }
    return out;
  }

  // Turns while messages are queued (§6, docs/optchat.md "Turns"), each
  // ended and its thread classified, then the compactor behind them. Past
  // a run's budget, the rest go to a fresh run.
  // The mind's lead agent (its first agent member: the hands), and whether
  // its computer is awake (its bridge holds a live socket here): one step.
  async #lead(s) {
    const members = await s.members();
    const lead = members.filter((m) => m.kind === "agent").sort((a, b) => (a.addedAt ?? 0) - (b.addedAt ?? 0))[0] ?? null;
    return { agent: lead?.principal ?? null, here: lead?.here === true };
  }

  // The hands, as each turn's messages name them: the lead agent by its
  // fragment's label, and whether its computer is awake. Its profile is the
  // one a turn kept (`kept`, turn_begin's), else read (`people`, a step) and
  // `fresh`, for turn_end to keep.
  async #hands(s, lead, kept) {
    const agent = lead.agent;
    if (agent === null) return { agent, profiles: {}, hands: [], fresh: null };
    let profile = kept && kept.agent === agent ? kept : null;
    let fresh = null;
    if (!profile) {
      let read = {};
      try {
        read = (await s.people([agent])) ?? {};
      } catch {
        read = {};
      }
      if (read[agent]) profile = fresh = { agent, fragment: read[agent].fragment ?? null, username: read[agent].username ?? null };
    }
    const named = String(profile?.fragment ?? "").split(".")[0] || profile?.username || "your agent";
    const profiles = profile ? { [agent]: { fragment: profile.fragment, username: profile.username } } : {};
    return { agent, profiles, hands: [{ name: named, awake: lead.here }], fresh };
  }

  async #turns(job, s, lead = null, first = null) {
    lead ??= await this.#lead(s);
    const agent = lead.agent;
    let h = null;
    let turns = 0;
    for (;;) {
      if (turns > 0 && (turns >= TURNS_PER_RUN || s.n > TURN_START_STEPS || s.bytes > RESULTS_SOFT_BYTES)) {
        await s.call("heard", { resume: true });
        return { turns, continued: true };
      }
      // the turn its message's step began, else one begun now
      const b = first ?? (await s.call("turn_begin", { run: job.run, agent }));
      first = null;
      if (!b.took) {
        if (turns > 0) await s.call("pump", {});
        return { turns, why: b.why };
      }
      turns++;
      h ??= await this.#hands(s, lead, b.profile);
      // what the turn's steps took (docs/optchat.md, "Latency"), filled by #turn
      const timing = { asked: b.asked ?? null, begun: b.now, view: null, calls: [] };
      const end = await this.#turn(job, s, b, h, timing);
      // its last call's step ended it (`logged`'s end), else a step of its own
      const e = end.ended ?? (await s.call("turn_end", { run: job.run, thread: b.thread, state: end.state, error: end.error ?? null, requeue: end.requeue ?? [], timing, profile: h.fresh }));
      if (end.handOn) {
        await s.call("heard", { resume: true });
        return { turns, continued: true };
      }
      if (e.topics > 0 && end.state !== "settling") await s.call("classify", { thread: b.thread });
      if (!e.queued) {
        await s.call("pump", {});
        return { turns };
      }
    }
  }

  // One turn (§6): wait until every message before its own is summarized,
  // render the view once, then model calls until one answers without a tool
  // call (at most CALLS_MAX). Every call is [tools] [system prompt] [view]
  // [the turn's state, then its messages], the tools and the system prompt
  // the same for every persona and thread (and every compaction), so the
  // cached prefix runs through the view.
  async #turn(job, s, b, h, timing) {
    const thread = b.thread;
    try {
      if (!b.settled) {
        const settled = await this.#settle(job, s, b);
        if (settled !== true) return settled;
        await s.publish("log", { type: "turn", thread, state: "thinking" });
      }
      // rendered as the turn began when it had nothing to wait for
      const v = (b.settled && b.view) || (await s.call("turn_view", { upto: b.tail }));
      timing.view = typeof v.at === "number" ? v.at : null;
      if (!v.settled) return { state: "error", error: "the memory is not summarized up to this message" };
      // the web's steps (applib/web.mjs), and the providers it passes over
      // this turn (a secret found missing, DuckDuckGo found refusing)
      const web = {
        fetch: (url, init) => {
          if (s.bytes > FETCH_ROOM_BYTES) throw new Error("this turn has read all the web its run can keep; answer with what you have");
          return s.fetch(url, init);
        },
        text: (opts) => s.text(opts),
        room: (n) => s.left() - WEB_KEEP_STEPS >= n,
        passed: new Set(),
      };
      // `apps`: the owner's apps, read once a turn (`#apps`)
      const ctx = {
        thread,
        persona: b.persona,
        hands: b.persona.hands === true && h.agent !== null,
        agent: h.agent,
        profiles: h.profiles,
        web,
        attachments: Array.isArray(b.attachments) ? [...b.attachments] : [],
        apps: null,
      };
      const state = turnState({ now: b.now, chat: b.chat, persona: b.persona, hands: h.hands });
      const messages = [
        { role: "system", content: system(b.about) },
        { role: "user", content: [{ type: "text", text: v.text, cache: "blocks" }, { type: "text", text: state }, { type: "text", text: b.texts.join("\n\n") }] },
      ];
      let convo = 0;
      let last = false;
      for (let calls = 0; calls < CALLS_MAX; calls++) {
        last = last || calls === CALLS_MAX - 1 || s.left() < 4 || s.bytes > RESULTS_SOFT_BYTES || convo > CONVO_MAX_BYTES;
        const a = await s.text({
          model: "cheap",
          messages,
          tools: CALL_TOOLS,
          tool_choice: last ? "none" : "auto",
          draft: { channel: "log", turn: `turn:${thread}` },
          // the person's model for chat (docs/optchat.md, "Your own models")
          role: "chat",
        });
        // `message` is the platform's answer with tools; without it, the text
        const msg = a && typeof a.message === "object" && a.message !== null ? a.message : { role: "assistant", content: a?.text ?? "" };
        const content = typeof msg.content === "string" ? msg.content : typeof a?.text === "string" ? a.text : "";
        const asked = !last && Array.isArray(msg.tool_calls) ? msg.tool_calls.slice(0, TOOL_CALLS_ANSWERED) : [];
        const entries = [];
        if (content.trim()) entries.push({ kind: "talk", text: content });
        const results = [];
        // what this answer's results add to the conversation so far
        let adding = 0;
        for (const [k, tc] of asked.entries()) {
          const name = String(tc?.function?.name ?? "");
          const raw = tc?.function?.arguments;
          let args = null;
          try {
            args = typeof raw === "string" ? JSON.parse(raw || "{}") : raw && typeof raw === "object" ? raw : {};
          } catch {
            args = null;
          }
          let out;
          if (k >= TOOL_CALLS_MAX) out = { text: `Error: at most ${TOOL_CALLS_MAX} tool calls run in one answer; call this one again.` };
          else if (args === null || typeof args !== "object") out = { text: "Error: the arguments are not a JSON object." };
          // a tool's steps (3 at most), this answer's log, a pump, and a last call and its log
          else if (s.left() < 7) {
            out = { text: "Error: this turn is out of steps; answer with what you have." };
            last = true;
          } else if (convo + adding > CONVO_MAX_BYTES) {
            // a page or a message whole is up to CAP: a few fill a call's arguments
            out = { text: "Error: this turn has read all it can hold; answer with what you have." };
            last = true;
          } else out = await this.#tool(job, s, name, args, ctx);
          // §1: a tool's output is clipped to its head and tail
          const echo = M.capText(out.text);
          adding += M.utf8(echo);
          entries.push({ kind: "tool", text: `${name} ${args === null ? String(raw) : JSON.stringify(args)}`, task: out.task ?? null });
          entries.push({ kind: "echo", text: echo, task: out.task ?? null });
          results.push({ role: "tool", tool_call_id: String(tc?.id ?? `call_${calls}_${k}`), content: echo });
        }
        // the call's own timing (the platform's), its tools, and when its log landed
        const t = a?.timing && typeof a.timing === "object" ? a.timing : {};
        const u = a?.usage ?? {};
        const timed = { first: t.first_ms ?? null, ms: t.ms ?? null, model: a?.model ?? null, calls: t.calls ?? null, thought: t.thought ?? null, tokens: [u.prompt_tokens ?? null, u.prompt_tokens_details?.cached_tokens ?? null, u.completion_tokens ?? null], tries: t.tries ?? null, since: t.since_ms ?? null, at: t.at ?? null, tools: asked.map((tc) => String(tc?.function?.name ?? "").slice(0, 32)), logged: null };
        if (timing.calls.length < CALLS_MAX) timing.calls.push(timed);
        // the last call (no tools): its log ends the turn in the same step
        const end = asked.length ? null : { state: "done", timing, profile: h.fresh };
        const lg = await s.call("logged", { run: job.run, thread, persona: b.persona.id, entries, take: asked.length > 0, end });
        timed.logged = lg.at ?? null;
        if (lg.pump) await s.call("pump", {});
        if (!asked.length) return { state: "done", ended: lg.ended ?? null };
        if (lg.stopped) return { state: "stopped" };
        // an answer's thinking blocks (Anthropic's, opaque) go back with its calls
        const said = { role: "assistant", content: msg.content ?? null, tool_calls: asked, ...(Array.isArray(msg.thinking_blocks) ? { thinking_blocks: msg.thinking_blocks } : {}) };
        messages.push(said, ...results);
        convo += sizeOf(said) + results.reduce((n, r) => n + sizeOf(r), 0);
        // §6: what the person (or a hand-off's report) said meanwhile, between tool calls
        const heard = lg.heard?.texts ?? [];
        if (heard.length) {
          const said = { role: "user", content: heard.join("\n\n") };
          messages.push(said);
          convo += sizeOf(said);
          for (const f of lg.heard.attachments ?? []) if (ctx.attachments.length < F.FILES_MAX && !ctx.attachments.some((x) => x.sha256 === f.sha256)) ctx.attachments.push(f);
        }
      }
      return { state: "done" };
    } catch (e) {
      return { state: "error", error: describe(e) };
    }
  }

  // §6: no turn sees a placeholder. The turn waits until every message
  // before its own is summarized, but an import's (its view passes those:
  // docs/optchat.md, "Where we differ from the gist", 16): it builds those
  // it may itself, one at a
  // time under a lease as a pump does, and starts pumps for the rest (a
  // message whose node failed is tried again RETRY_MS later; one that
  // failed SETTLE_FAILS_MAX times ends the turn with an error). Past
  // SETTLE_STEPS the turn hands its messages to a fresh run. Answers true,
  // or how the turn ends.
  async #settle(job, s, b) {
    const began = s.n;
    let built = null;
    let waits = 0;
    for (;;) {
      if (s.n - began > SETTLE_STEPS || s.left() < PUMP_KEEP_STEPS + 4) {
        if (built !== null) await s.call("pump_step", { run: job.run, built, end: true });
        return { state: "settling", requeue: b.taken, handOn: true };
      }
      const r = await s.call("pump_step", { run: job.run, built, upto: b.tail });
      built = null;
      for (let k = 0; k < r.spawn; k++) await s.call("pump", {});
      if (r.settled) return true;
      if (r.stopped) return { state: "stopped" };
      if (r.failing) return { state: "error", error: `the memory could not summarize message ${r.failing.id}: ${r.failing.error}` };
      if (r.node) {
        waits = 0;
        built = await this.#compact(s, r.node);
        continue;
      }
      // pumps are building what this turn waits for
      waits++;
      await s.sleep(Math.min(1000 * waits, 5000));
    }
  }

  // One compaction (§4): the node's call, with the system prompt and the
  // tools every turn has (none to be called), then the compaction view up
  // to the node and its task; a line over NODE asked again in the same
  // conversation, cut where the limit falls, until it fits or TRIES (the
  // shortest kept). Answers {l, i, text}, or {l, i, error}.
  async #compact(s, { l, i }) {
    // read from this instance as the step is built (see the top)
    let messages = this.#compaction(l, i);
    let tries = [];
    for (;;) {
      let a;
      try {
        a = await s.text({ model: "cheap", role: "memory", messages, tools: CALL_TOOLS, tool_choice: "none", max_tokens: COMPACT_TOKENS });
      } catch (e) {
        return { l, i, error: describe(e) };
      }
      const r = M.compactTry(tries, a?.text);
      if (r.fail) return { l, i, error: r.fail };
      tries = r.tries;
      if (r.text !== undefined) return { l, i, text: r.text };
      messages = [...messages, { role: "assistant", content: a.text }, { role: "user", content: r.retry }];
    }
  }

  // A compaction's first messages: the system prompt of every call, then
  // the compaction view up to the node and its task (M.compaction). A node
  // another run built meanwhile (its lease ran out) is asked all the same:
  // a past step's arguments do not matter, and the first write wins.
  #compaction(l, i) {
    const m = this.#memory();
    const sys = { role: "system", content: system(this.#get("about") ?? "") };
    const open = (i + 1) * 2 ** l <= m.T && !M.isBuilt(m, l, i) && (l === 0 || (M.isBuilt(m, l - 1, 2 * i) && M.isBuilt(m, l - 1, 2 * i + 1)));
    if (!open) return [sys, { role: "user", content: `Compaction: ${M.nameOf(l, i)} is built already: answer "built".` }];
    const c = M.compaction(m, l, i, (k) => this.#line(k));
    return [sys, { role: "user", content: [{ type: "text", text: c.view, cache: "blocks" }, { type: "text", text: c.task }] }];
  }

  // A compactor (§4, "The order"): one node at a time, each taken under a
  // lease and written by `pump_step`, until none is left it may take; it
  // starts pumps for what else is ready, up to JOBS at work at once (a
  // run's steps go one at a time, so JOBS calls at once are JOBS runs). A
  // node that failed is passed for the rest of this run. Past its budget a
  // fresh pump takes its place (a chain the hop limit ends at 16; the next
  // message, an import's next part, or `fragment mind import` following it
  // starts another).
  async pump(input, job) {
    const s = new Steps(job);
    const skip = [];
    let built = null;
    let made = 0;
    let first = true;
    for (;;) {
      if (s.left() < PUMP_KEEP_STEPS || s.bytes > RESULTS_SOFT_BYTES || skip.length >= PUMP_SKIPS_MAX) {
        await s.call("pump_step", { run: job.run, built, first, end: true, next: true });
        await s.call("pump", {});
        return { built: made, continued: true };
      }
      const r = await s.call("pump_step", { run: job.run, built, skip, first });
      first = false;
      if (typeof built?.text === "string") made++;
      built = null;
      for (let k = 0; k < r.spawn; k++) await s.call("pump", {});
      if (!r.node) return { built: made };
      built = await this.#compact(s, r.node);
      if (typeof built.text !== "string") skip.push(M.key(built.l, built.i));
    }
  }

  // zoom("<task id>") in a turn: the task (`task`), then goose's run, its
  // records on `work` under the task's turn (`job.records`), read now, at
  // most WORK_PAGES pages while the run's answers have room for one; the
  // whole in pages as a long message's. A tool's steps: 3 at most.
  async #zoomTask(s, id, page) {
    const { task } = await s.call("task", { id });
    if (!task) return `No task ${id}.`;
    const run = { records: [], more: false, error: null };
    try {
      for (let after = 0, k = 0; typeof task.turn === "string"; k++) {
        if (k === WORK_PAGES) {
          run.more = true;
          break;
        }
        // a page is at most 512 KiB: one is read where a fetch's answer would fit
        if (s.bytes > FETCH_ROOM_BYTES) {
          run.error = "this turn has read all its run can keep";
          break;
        }
        const p = await s.records("work", { after, turn: task.turn });
        run.records.push(...p.records);
        if (p.next === null) break;
        after = p.next;
      }
    } catch (e) {
      run.error = describe(e);
    }
    return taskPage(id, taskText(task, run), page);
  }

  // The owner's apps (`job.owner.fragments`), read once a turn: an answer's
  // `apps` and a later `app_ops` share one step. `{fragments}`, or the
  // tool's error (a platform that lends none, a mind shared with someone).
  async #apps(s, ctx) {
    if (ctx.apps) return ctx.apps;
    if (typeof s.job.owner?.fragments !== "function") return (ctx.apps = { error: "Error: this platform lends the mind no apps." });
    try {
      ctx.apps = { fragments: await s.ownerFragments() };
    } catch (e) {
      ctx.apps = { error: `Error: ${describe(e)}` };
    }
    return ctx.apps;
  }

  // One tool call of a turn: zoom and date are queries (§6; a turn has no
  // search, §5: zoom is its one way through the memory); the web's are
  // fetches (applib/web.mjs); the apps' are the owner's (`job.owner`,
  // docs/optchat.md "The user's apps"); computer hands the task to goose on
  // `chat` (docs/optchat.md, "Hand-offs").
  async #tool(job, s, name, args, ctx) {
    switch (name) {
      case "zoom": {
        const page = args.page === undefined ? 1 : Number(args.page);
        if (!isInt(page) || page < 1) return { text: `No page ${args.page}.` };
        // zoom("<task id>"): a computer task whole, with goose's run
        if (typeof args.id === "string" && !/^\s*\d+\s*$/.test(args.id)) {
          const id = args.id.trim().replace(/^\[|\]$/g, "").slice(0, 64);
          return { text: id ? await this.#zoomTask(s, id, page) : `No task ${JSON.stringify(args.id)}.` };
        }
        const id = Number(args.id);
        const n = args.n === undefined ? 1 : Number(args.n);
        if (!isInt(id) || !Number.isSafeInteger(n) || n < 1) return { text: `No line ${args.id}+${args.n}.` };
        return { text: (await s.call("zoom", { id, n, page })).text };
      }
      case "date": {
        const id = Number(args.id);
        if (!isInt(id)) return { text: `No message ${args.id}.` };
        return { text: (await s.call("date", { id })).text };
      }
      case "web_search": {
        const q = String(args.q ?? "").trim().slice(0, 400);
        if (!q) return { text: "Error: web_search needs what to search for (q)." };
        const n = clamp(Number(args.limit ?? W.SEARCH_DEFAULT), 1, W.SEARCH_MAX);
        return { text: W.searchText(q, await W.search(ctx.web, q, n)) };
      }
      case "web_fetch":
        return { text: W.pageText(await W.read(ctx.web, args.url)) };
      case "research": {
        const question = String(args.question ?? "").trim().slice(0, 2000);
        if (!question) return { text: "Error: research needs the question." };
        return { text: await W.research(ctx.web, question) };
      }
      case "apps": {
        const apps = await this.#apps(s, ctx);
        return { text: apps.error ?? appsText(apps.fragments) };
      }
      case "app_ops": {
        const fragment = String(args.fragment ?? "").trim();
        if (!fragment) return { text: "Error: app_ops needs the app's name (fragment), as apps lists it." };
        const apps = await this.#apps(s, ctx);
        if (apps.error) return { text: apps.error };
        const app = apps.fragments.find((f) => f.name === fragment);
        return { text: app ? opsText(app) : `Error: the user has no app ${JSON.stringify(fragment)}: apps lists theirs.` };
      }
      case "app_call": {
        const fragment = String(args.fragment ?? "").trim();
        const op = String(args.op ?? "").trim();
        if (!fragment || !op) return { text: "Error: app_call needs the app (fragment) and its operation (op)." };
        const input = args.input ?? {};
        if (typeof input !== "object" || Array.isArray(input)) return { text: "Error: an operation's input is an object." };
        if (typeof job.owner?.call !== "function") return { text: "Error: this platform lends the mind no apps." };
        try {
          const r = await s.ownerCall(fragment, op, input);
          // its first line names the app and where it is (the page links it)
          return { text: `${fragment} ${op} at ${r.url}\n${resultText(r.result)}` };
        } catch (e) {
          return { text: `Error: ${describe(e)}` };
        }
      }
      case "computer": {
        // offered to every persona (the tools are every call's), used by those with hands
        if (ctx.agent === null) return { text: "Error: the user has no agent on a computer to hand work to; answer yourself, and tell them." };
        if (!ctx.hands) return { text: `Error: as ${ctx.persona.name} you hand nothing to the computer in this chat; answer yourself, or tell the user a persona with hands can.` };
        const task = String(args.task ?? "").trim();
        if (!task) return { text: "Error: computer needs the task, in words." };
        // the agent's fragment names the turn its bridge gives the task
        const agentFragment = ctx.profiles?.[ctx.agent]?.fragment ?? (await s.people([ctx.agent]))?.[ctx.agent]?.fragment;
        if (typeof agentFragment !== "string" || !agentFragment) return { text: "Error: the computer's agent has no fragment to hand work to." };
        // the run and this step: the same id on every re-run
        const id = `w${job.run}-${s.n}`;
        // the turn's files go with it: goose's bridge downloads them (docs/chat-records.md)
        const handed = { text: `${M.cutBytes(task, TASK_TEXT_MAX)}\n\n(task ${id}, thread ${ctx.thread})`, to: [ctx.agent] };
        if (ctx.attachments.length) handed.attachments = ctx.attachments.slice(0, F.FILES_MAX);
        const posted = await s.publish("chat", handed);
        const turn = await turnOf(agentFragment, job.fragment, "chat", posted.seq);
        await s.call("task_open", { id, thread: ctx.thread, text: task, seq: posted.seq, turn });
        return { text: `[${id}] started`, task: id };
      }
      default:
        return { text: `Error: there is no tool ${JSON.stringify(name)}.` };
    }
  }

  // Topics (docs/optchat.md, "Topics"): a turn's thread against every
  // topic, or a new topic (its `sort` record) against the newest threads,
  // by Clef. The questions are steps' answers; each thread's state is read
  // from this instance as its step is built.
  async classify(input = {}, job) {
    if (typeof job.ai?.decide !== "function") return { why: "this platform has no ai.decide" };
    const s = new Steps(job);
    const topic = typeof input?.topic === "string" ? input.topic : typeof input?.record?.body?.topic === "string" ? input.record.body.topic : null;
    const { topics } = await s.call("topics", {});
    let asked;
    let threads;
    if (typeof input?.thread === "string") {
      asked = topics;
      threads = [input.thread];
    } else if (topic !== null) {
      asked = topics.filter((t) => t.id === topic);
      threads = asked.length ? (await s.call("threads", { limit: CLASSIFY_THREADS })).threads.map((t) => t.id) : [];
    } else return { why: "classify takes a thread or a topic" };
    if (!asked.length || !threads.length) return { sorted: 0 };
    const scope = asked.map((t) => t.id);
    let sets = [];
    let sorted = 0;
    for (const thread of threads) {
      const per = Math.ceil(asked.length / CLEF_QUESTIONS_MAX);
      if (s.left() < per + 2) break;
      const ps = [];
      for (let k = 0; k < asked.length; k += CLEF_QUESTIONS_MAX) {
        const batch = asked.slice(k, k + CLEF_QUESTIONS_MAX);
        // one question per topic, by the topic's id
        const questions = Object.fromEntries(
          batch.map((t) => [t.id, { type: "noul", instructions: `Is this conversation about ${t.name}? ${t.description ?? ""}`.trim() }]),
        );
        let r = null;
        try {
          r = await s.decide({ model: "clef-flash", state: this.#clefState(thread), questions });
        } catch {
          r = null;
        }
        for (const t of batch) ps.push({ id: t.id, p: pOf(r?.answers?.[t.id]) });
      }
      sets.push({ thread, topics: ps });
      sorted++;
      if (sets.length >= CLASSIFY_SETS) {
        await s.call("topics_set", { sets, scope });
        sets = [];
      }
    }
    if (sets.length) await s.call("topics_set", { sets, scope });
    return { sorted };
  }

  // A thread for Clef: its title, then its messages as `kind: text` lines,
  // newest kept, at most CLEF_STATE_MAX bytes.
  #clefState(thread) {
    const t = this.#thread(thread);
    const head = t ? t.title : "";
    let room = CLEF_STATE_MAX - M.utf8(head) - 1;
    const lines = [];
    for (const r of this.ctx.storage.sql.exec("SELECT kind, text FROM log WHERE thread = ? ORDER BY i DESC LIMIT 2000", thread)) {
      const line = M.cutBytes(`${r.kind}: ${M.flat(r.text)}`, CLEF_LINE_MAX);
      room -= M.utf8(line) + 1;
      if (room < 0) break;
      lines.push(line);
    }
    return `${head}\n${lines.reverse().join("\n")}`;
  }

  // Topic names to offer (docs/optchat.md, "Topics"): one cheap call over
  // the view, published on `log` as `{type: "suggest", names}`.
  async topic_suggest(input, job) {
    const s = new Steps(job);
    const have = this.ctx.storage.sql.exec("SELECT name FROM topic ORDER BY made").toArray().map((r) => r.name);
    const view = M.render(this.#memory()).text;
    const a = await s.text({
      model: "cheap",
      role: "memory",
      messages: [
        { role: "system", content: SUGGEST },
        { role: "user", content: `${view}\n\nTopics they have: ${have.length ? have.join(", ") : "none yet"}.` },
      ],
      max_tokens: 2000,
    });
    const names = namesOf(a?.text ?? "", have);
    await s.publish("log", { type: "suggest", names });
    return { names };
  }

  // `chat`'s trigger, for goose's records there: a hand-off's one reply,
  // its report, queued and a turn run on it. goose's steps on `work` are
  // the page's to follow (by `turn`); the mind runs nothing for them.
  async hands_said(input = {}, job) {
    if (job.via !== "channel") return { why: "only the chat channel's trigger takes goose's replies" };
    const b = input?.record?.body;
    if (!b || typeof b !== "object" || typeof b.turn !== "string" || (b.kind !== undefined && b.kind !== "message")) return { why: "not a reply" };
    const s = new Steps(job);
    // its files (a reply naming them badly reports without them)
    const attachments = await this.#readFiles(job, s, F.attachmentsOf(b.attachments).files ?? []);
    // the hands first: the report's turn begins with it, in its step
    const lead = await this.#lead(s);
    const h = await s.call("hands_reply", { turn: b.turn, text: typeof b.text === "string" ? b.text : "", attachments, begin: { run: job.run, agent: lead.agent } });
    if (!h.ended || h.running) return h;
    return this.#turns(job, s, lead, h.begun ?? null);
  }
}
