// The mind template's code (docs/optchat.md): one memory and the main
// agent, OptChat's design (its spec, ~/dev/finite/optchat-spec.md) in a
// fragment app. Every message a person says on `say`, every reply, tool
// call and result of the agent, and every report of its hands (goose, on
// the person's computer, through `chat` and `work`) goes into one log
// here, in SQLite. A compactor (job `pump`) summarizes the log into a
// binary tree of one-line nodes, and each turn (job `heard`) starts fresh
// from the view: a fixed-size tiling of the whole log by those nodes, the
// older the coarser (applib/optmem.mjs). Topics are Clef's (job
// `classify`).
//
// The view is never stored: it is folded from the log and the tree at the
// app's first call and kept on this instance, which the facet may evict
// at any time. Every mutation that changes the log or the tree bumps
// `rev` in kv, so an instance whose memory missed a change (a rolled-back
// mutation) folds it again.
//
// Jobs re-run from the top at every step (docs/api.md, Jobs): their
// control flow follows only their steps' answers. The compactor's input
// (the view up to a node) is read from this instance when its step is
// built, not carried in a step's answer: it would be 128 KB per node in a
// run's 4 MiB of answers, and what a past step was sent does not matter.
// A turn's view is the one exception, frozen by a step (`view`) so every
// model call of the turn sees the same one.
import { DurableObject } from "cloudflare:workers";
import * as M from "./applib/optmem.mjs";
import { COMPACT, SUGGEST, TOOLS, system } from "./applib/prompts.mjs";

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
const TURNS_PER_RUN = 4;
// A turn's settle tries a failing node this many times, RETRY_MS apart.
const SETTLE_FAILS_MAX = 3;
const RETRY_MS = 10_000;
// A node a pump took is another run's to build for this long.
const LEASE_MS = 5 * 60_000;
// One pump run's rounds; a node's steps at most (TRIES calls and its write),
// and a batch's (the same: its lines are retried together).
const PUMP_ROUNDS_MAX = 30;
const NODE_STEPS = M.TRIES + 1;
const COMPACT_TOKENS = 4096;
const BATCH_TOKENS = 8192;
// The compactor is started again by an import when no pump planned this
// long ago (a `compact` record, whose trigger runs `pump`).
const PUMP_KICK_MS = 3 * 60_000;
// An import's part: at most this many messages (docs/optchat.md, "Importing
// chats"); `imported` answers for at most this many conversations.
const IMPORT_MESSAGES_MAX = 64;
const IMPORTED_ASK_MAX = 200;
const IMPORT_SOURCE = /^[a-z][a-z0-9-]{0,31}$/;
const IMPORT_ID_MAX = 200;
// A node a stubborn model wrote over twice NODE is cut there.
const NODE_TEXT_MAX = 2 * M.NODE;
// A `msg` record's text (docs/optchat.md, "Records on log"), and the most
// a record's JSON may take of the platform's 64 KiB.
const RECORD_TEXT_MAX = 48 * 1024;
const RECORD_JSON_MAX = 60 * 1024;
const TASK_TEXT_MAX = 30 * 1024;
const TASK_RECORD_TEXT_MAX = 4096;
const TASK_RECORD_REPORT_MAX = 16 * 1024;
// A hand-off with no reply this long after it opened is `lost`.
const TASK_LOST_MS = 30 * 60_000;
// Replies whose task is not recorded yet (its `task_open` a step behind),
// kept for it, at most this many.
const EARLY_REPLIES_MAX = 32;
const SEARCH_TOOL_MAX = 20;
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
      "You get things done on the user's computer. When a task needs files, a shell, a browser or code, hand it to the computer with everything it needs, say what you started, and report results plainly when they come back.",
  },
  {
    id: "coach",
    name: "Coach",
    emoji: "🧭",
    hands: 0,
    instructions:
      "Ask one question at a time, and wait for the answer. Help the user think it through rather than handing them answers: say back in one sentence what you heard before the next question.",
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

// A record's text: at most RECORD_TEXT_MAX bytes, and its JSON within the
// record's limit however many characters it escapes.
function recordText(text) {
  let t = M.cutBytes(text, RECORD_TEXT_MAX);
  while (M.utf8(JSON.stringify(t)) > RECORD_JSON_MAX) t = M.cutBytes(t, Math.floor(M.utf8(t) * 0.8));
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

  sleep(ms) {
    return this.#took(this.job.sleep(ms));
  }

  members() {
    return this.#took(this.job.members());
  }

  people(ids) {
    return this.#took(this.job.people(ids));
  }

  left() {
    return STEPS_MAX - RESERVE_STEPS - this.n;
  }
}

export class App extends DurableObject {
  // The memory folded from the log and the tree, and the `rev` it is of
  // (null: unknown, folded again at the next read).
  #mem = null;
  #rev = null;

  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec(`CREATE TABLE IF NOT EXISTS log (
      i INTEGER PRIMARY KEY, kind TEXT NOT NULL, text TEXT NOT NULL, at INTEGER NOT NULL, thread TEXT, persona TEXT, task TEXT)`);
    sql.exec("CREATE INDEX IF NOT EXISTS log_thread ON log (thread, i)");
    sql.exec("CREATE TABLE IF NOT EXISTS node (l INTEGER NOT NULL, i INTEGER NOT NULL, text TEXT NOT NULL, PRIMARY KEY (l, i))");
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
    if (sql.exec("SELECT COUNT(*) AS n FROM persona").one().n === 0) {
      ctx.storage.transactionSync(() => {
        const now = Date.now();
        // listed in this order (by `made`)
        for (const [k, p] of PERSONAS.entries()) {
          sql.exec("INSERT INTO persona (id, name, emoji, instructions, hands, made) VALUES (?, ?, ?, ?, ?, ?)", p.id, p.name, p.emoji, p.instructions, p.hands, now + k);
        }
        this.#set("default", PERSONAS[0].id);
      });
    }
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

  #memory() {
    const rev = this.#get("rev") ?? "0";
    if (this.#mem !== null && this.#rev === rev) return this.#mem;
    const sql = this.ctx.storage.sql;
    const { n, top } = sql.exec("SELECT COUNT(*) AS n, MAX(i) AS top FROM log").one();
    need(n === 0 || top === n - 1, `the log's ids are not 0 to ${n - 1}`);
    this.#mem = M.fold(n, sql.exec("SELECT l, i, text FROM node"));
    this.#rev = rev;
    return this.#mem;
  }

  // A mutation that changes the log or the tree: the memory is unknown
  // while it runs (an exception leaves it so, to be folded again), and of
  // the new `rev` once it is done (a rollback after, of the old one).
  #changing(fn) {
    const m = this.#memory();
    this.#rev = null;
    const out = fn(m);
    const rev = String(Number(this.#get("rev") ?? 0) + 1);
    this.#set("rev", rev);
    this.#rev = rev;
    return out;
  }

  #message(i) {
    return this.ctx.storage.sql.exec("SELECT i, kind, text, at, thread, persona, task FROM log WHERE i = ?", i).toArray()[0] ?? null;
  }

  #line(i) {
    const r = this.ctx.storage.sql.exec("SELECT kind, text FROM log WHERE i = ?", i).one();
    return M.line0(r.kind, r.text);
  }

  // The free nodes ready now, built and stored (no model: spec 3).
  #free(m) {
    for (const n of M.buildFree(m, (i) => this.#line(i))) {
      this.ctx.storage.sql.exec("INSERT INTO node (l, i, text) VALUES (?, ?, ?)", n.l, n.i, n.text);
    }
  }

  // One message appended to the log (capped: the 16 MiB debt), its thread
  // touched, its free nodes built, and its record published on `log`. An
  // import's message keeps its own time and publishes no record (a page
  // reads an imported thread with `thread`).
  #log(call, m, kind, text, { thread = null, persona = null, task = null, at = null, publish = true } = {}) {
    const sql = this.ctx.storage.sql;
    const i = m.T;
    const t = M.capText(text);
    at ??= Date.now();
    sql.exec("INSERT INTO log (i, kind, text, at, thread, persona, task) VALUES (?, ?, ?, ?, ?, ?, ?)", i, kind, t, at, thread, persona, task);
    sql.exec("INSERT INTO log_fts (rowid, text) VALUES (?, ?)", i, t);
    if (thread !== null) sql.exec("UPDATE thread SET last = ?, last_i = ? WHERE id = ?", at, i, thread);
    M.append(m);
    this.#free(m);
    if (publish) call.publish("log", { type: "msg", i, kind, text: recordText(t), thread, at, persona, task });
    return { i, at };
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

  // A `user` message: logged in its thread (made on its first message,
  // titled from its first line) and queued for a turn. Answers whether a
  // turn is running, which takes it next.
  #enqueue(call, m, { text, thread, persona = null, task = null }) {
    const sql = this.ctx.storage.sql;
    const now = Date.now();
    const t = this.#thread(thread);
    const p = this.#persona(persona ?? t?.persona ?? null).id;
    if (!t) {
      const title = titleOf(text);
      sql.exec("INSERT INTO thread (id, title, persona, started, last, first_i, last_i) VALUES (?, ?, ?, ?, ?, ?, ?)", thread, title, p, now, now, m.T, m.T);
      call.publish("log", { type: "thread", id: thread, title });
    } else if (persona !== null && t.persona !== p) {
      sql.exec("UPDATE thread SET persona = ? WHERE id = ?", p, thread);
    }
    const queue = this.#json("queue", []);
    need(queue.length < QUEUE_MAX, `${QUEUE_MAX} messages already wait for a turn`);
    const { i } = this.#log(call, m, "user", text, { thread, persona: p, task });
    queue.push({ i, thread });
    this.#setJson("queue", queue);
    return { i, running: this.#lock(now) !== null };
  }

  // ---- internal operations: the jobs' halves (docs/optchat.md) ----

  // A person's message from `say`.
  hear({ text, thread, persona = null }, call) {
    need(typeof text === "string" && text.trim().length > 0, "hear: text is words");
    need(THREAD.test(thread), "hear: thread is t_ and 16 hex");
    need(persona === null || typeof persona === "string", "hear: persona is a persona's id");
    return this.#changing((m) => this.#enqueue(call, m, { text, thread, persona }));
  }

  // Takes the turn lock (free, run out, or this run's) and the queued
  // messages of the oldest thread waiting. `tail` is where the turn's view
  // stops: before the newest run of messages still waiting for an answer,
  // which the turn is given whole (spec 7: the view is rendered before the
  // new messages are logged).
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
    const taken = [];
    const texts = [];
    let bytes = 0;
    for (const q of queue) {
      if (q.thread !== thread) continue;
      const text = sql.exec("SELECT text FROM log WHERE i = ?", q.i).one().text;
      if (taken.length && bytes + M.utf8(text) > TEXTS_MAX_BYTES) break;
      bytes += M.utf8(text);
      taken.push(q.i);
      texts.push(text);
    }
    const took = new Set(taken);
    this.#setJson("queue", queue.filter((q) => !took.has(q.i)));
    const m = this.#memory();
    const waiting = new Set(queue.map((q) => q.i));
    let tail = m.T;
    while (tail > 0 && waiting.has(tail - 1)) tail--;
    const t = this.#thread(thread);
    const persona = this.#persona(t?.persona);
    this.#setJson("turn", { run, thread, since: now, touched: now, stop: false });
    const settled = M.first(m) >= tail;
    call.publish("log", { type: "turn", thread, state: settled ? "thinking" : "settling" });
    // hand-offs with no reply in TASK_LOST_MS are lost (a reply later still reports)
    for (const { id } of sql.exec("SELECT id FROM task WHERE state = 'running' AND started < ? LIMIT 32", now - TASK_LOST_MS).toArray()) {
      sql.exec("UPDATE task SET state = 'lost' WHERE id = ?", id);
      this.#publishTask(call, this.#task(id));
    }
    return { took: true, thread, persona, about: this.#get("about") ?? "", texts, taken, tail, settled };
  }

  // The lock kept by its run; answers whether Stop was asked.
  turn_touch({ run }) {
    const l = this.#json("turn", null);
    if (!l || l.run !== run) return { held: false, stopped: false };
    l.touched = Date.now();
    this.#setJson("turn", l);
    return { held: true, stopped: l.stop === true };
  }

  // One model call's messages, in order: its reply (talk), each tool call
  // (tool) and its result (echo). Touches the lock; answers whether Stop
  // was asked.
  logged({ run, thread, persona = null, entries }, call) {
    need(Array.isArray(entries) && entries.length <= 1 + 2 * TOOL_CALLS_ANSWERED, "logged: entries is a list");
    for (const e of entries) need(e && LOGGED_KINDS.has(e.kind) && typeof e.text === "string", "logged: each entry is {kind: talk|tool|echo, text}");
    const sql = this.ctx.storage.sql;
    return this.#changing((m) => {
      const ids = [];
      for (const e of entries) {
        const task = typeof e.task === "string" ? e.task : null;
        const { i } = this.#log(call, m, e.kind, e.text, { thread, persona, task });
        if (e.kind === "tool" && task !== null) sql.exec("UPDATE task SET i = ? WHERE id = ? AND i IS NULL", i, task);
        ids.push(i);
      }
      return { ids, ...this.turn_touch({ run }) };
    });
  }

  // The lock let go, and what ended published. `requeue` puts a turn's
  // messages back first in line (a turn handed on to a fresh run).
  turn_end({ run, thread, state, error = null, requeue = [] }, call) {
    need(["done", "stopped", "error", "settling"].includes(state), "turn_end: state is done, stopped, error or settling");
    let queue = this.#json("queue", []);
    if (Array.isArray(requeue) && requeue.length) {
      const back = new Set(requeue);
      queue = [...requeue.filter(isInt).map((i) => ({ i, thread })), ...queue.filter((q) => !back.has(q.i))];
      this.#setJson("queue", queue);
    }
    const l = this.#json("turn", null);
    if (l && l.run === run) this.#del("turn");
    call.publish("log", { type: "turn", thread, state, ...(error ? { error: String(error).slice(0, 2000) } : {}) });
    const topics = this.ctx.storage.sql.exec("SELECT COUNT(*) AS n FROM topic").one().n;
    return { queued: queue.length, topics };
  }

  // Spec 4.1, for a job, batched (docs/optchat.md, "Importing chats"): the
  // level-0 nodes one call builds (`nodes`: `first`'s and the long messages
  // after it, M.batch) and up to `max` ready merges (`merges`), each leased
  // to this run (another's lease is skipped until it runs out), and whether
  // the view is settled up to `upto` (all of it by default). A turn's
  // settle (`upto` named) takes merges only while the view is over VIEW:
  // they only coarsen it, but a long backlog (an import) would otherwise
  // grow it, and every call's context with it, without bound. A query that
  // writes its leases (as the brain's reindex writes its index), and when a
  // pump planned last (`status`, and an import's start of the compactor).
  pump_plan({ run = null, upto = null, skip = [], max = M.JOBS } = {}) {
    const now = Date.now();
    const m = this.#memory();
    const leases = this.#json("busy", {});
    for (const [k, v] of Object.entries(leases)) {
      const [l, i] = k.split(":").map(Number);
      if (v.until < now || M.isBuilt(m, l, i)) delete leases[k];
    }
    const busy = new Set(Array.isArray(skip) ? skip.map(String) : []);
    for (const [k, v] of Object.entries(leases)) if (v.run !== run) busy.add(k);
    const f = M.first(m);
    const settled = f >= (upto ?? m.T);
    const n = clamp(max, 1, M.JOBS);
    const nodes = settled ? [] : M.batch(m, (i) => this.#line(i), { busy, upto: upto ?? m.T, max: n });
    const merges = upto === null || m.bytes > M.VIEW ? M.ready(m, { busy, max: n + 1 }).filter((x) => x.l > 0).slice(0, n) : [];
    for (const x of [...nodes, ...merges]) leases[M.key(x.l, x.i)] = { run, until: now + LEASE_MS };
    this.#setJson("busy", leases);
    if (upto === null && run !== null) this.#set("pump_at", now);
    const lock = this.#json("turn", null);
    return { nodes, merges, settled, first: f, T: m.T, stopped: !!(run !== null && lock && lock.run === run && lock.stop) };
  }

  // A node the compactor built: the first write wins, and the view is
  // refitted. No text: the build failed, its lease let go, the failure
  // kept for `status` (the next pump tries again).
  node_built({ run = null, l, i, text = null, error = null }) {
    need(isInt(l) && isInt(i), "node_built: l and i are counts");
    if (text === null) return this.#nodeFailed(run, l, i, error);
    need(typeof text === "string" && text.trim().length > 0, "node_built: text is the line");
    return this.#changing((m) => this.#nodeBuilt(m, l, i, text));
  }

  // A batch's nodes (docs/optchat.md, "Importing chats"), each as
  // node_built takes one, in order: a level-0 batch's messages, or a
  // batch's merges (never a parent and its child).
  nodes_built({ run = null, nodes }) {
    need(Array.isArray(nodes) && nodes.length >= 1 && nodes.length <= 2 * M.BATCH, `nodes_built: 1 to ${2 * M.BATCH} nodes`);
    for (const x of nodes) {
      need(x && isInt(x.l) && isInt(x.i), "nodes_built: each node's l and i are counts");
      need(x.text === null || x.text === undefined || (typeof x.text === "string" && x.text.trim().length > 0), "nodes_built: a node's text is its line, or null");
    }
    const out = [];
    for (const x of nodes) if (typeof x.text !== "string") out.push(this.#nodeFailed(run, x.l, x.i, x.error ?? null));
    const done = nodes.filter((x) => typeof x.text === "string");
    if (done.length) out.push(...this.#changing((m) => done.map((x) => this.#nodeBuilt(m, x.l, x.i, x.text))));
    return { built: out.filter((r) => r.built).length, first: M.first(this.#memory()) };
  }

  // A node's build failed: its lease let go, the failure kept for `status`
  // (the next pump tries again).
  #nodeFailed(run, l, i, error) {
    const k = M.key(l, i);
    const leases = this.#json("busy", {});
    if (k in leases) {
      delete leases[k];
      this.#setJson("busy", leases);
    }
    const fails = this.#json("fails", {});
    const first = !(k in fails);
    fails[k] = { id: i * 2 ** l, n: 2 ** l, error: String(error ?? "failed").slice(0, 500), at: Date.now(), tries: (fails[k]?.tries ?? 0) + 1, run };
    const keys = Object.keys(fails);
    if (keys.length > FAILS_KEPT) for (const old of keys.slice(0, keys.length - FAILS_KEPT)) delete fails[old];
    this.#setJson("fails", fails);
    return { built: false, first };
  }

  // A node built, in a #changing: the first write wins, its lease let go,
  // its failures forgotten, the view refitted and the free nodes it readied
  // built.
  #nodeBuilt(m, l, i, text) {
    const k = M.key(l, i);
    const leases = this.#json("busy", {});
    if (k in leases) {
      delete leases[k];
      this.#setJson("busy", leases);
    }
    if (M.isBuilt(m, l, i)) return { built: false, why: "built already" };
    need((i + 1) * 2 ** l <= m.T, `node_built: ${i * 2 ** l}+${2 ** l} covers messages past the log`);
    need(l === 0 || (M.isBuilt(m, l - 1, 2 * i) && M.isBuilt(m, l - 1, 2 * i + 1)), "node_built: a parent comes after its children");
    const t = M.cutBytes(text, NODE_TEXT_MAX);
    this.ctx.storage.sql.exec("INSERT INTO node (l, i, text) VALUES (?, ?, ?)", l, i, t);
    M.setNode(m, l, i, t);
    this.#free(m);
    const fails = this.#json("fails", {});
    if (k in fails) {
      delete fails[k];
      this.#setJson("fails", fails);
    }
    return { built: true, first: M.first(m) };
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
        id, thread, M.capText(text), seq, turn, Date.now(),
      );
      const early = this.#json("early", {});
      if (turn in early) {
        const said = early[turn].text;
        delete early[turn];
        this.#setJson("early", early);
        return { id, turn, opened: true, ...this.#endTask(call, m, this.#task(id), said) };
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

  // A hand-off's end: its one reply is its report, queued as a `user`
  // message `[<task>] …` in its thread, which starts a turn. A reply to a
  // task that ended already (a later part) changes nothing.
  #endTask(call, m, t, said) {
    if (t.report !== null) return { task: t.id, ended: false };
    const report = M.capText(said.trim() || "(ended: idle: no words)");
    const state = endedBy(report);
    this.ctx.storage.sql.exec("UPDATE task SET state = ?, report = ?, ended = ? WHERE id = ?", state, report, Date.now(), t.id);
    this.#publishTask(call, { ...t, state, report });
    const q = this.#enqueue(call, m, { text: `[${t.id}] ${report}`, thread: t.thread, task: t.id });
    return { task: t.id, ended: true, running: q.running, i: q.i };
  }

  // A reply of goose's on `chat`: the report of the task its turn names.
  // One whose task is not recorded yet is kept for `task_open`.
  hands_reply({ turn, text }, call) {
    need(typeof turn === "string" && typeof text === "string", "hands_reply: {turn, text}");
    const id = this.ctx.storage.sql.exec("SELECT id FROM task WHERE turn = ?", turn).toArray()[0]?.id ?? null;
    if (id === null) {
      const early = this.#json("early", {});
      if (!(turn in early)) early[turn] = { text: M.capText(text), at: Date.now() };
      const keys = Object.keys(early);
      if (keys.length > EARLY_REPLIES_MAX) for (const old of keys.slice(0, keys.length - EARLY_REPLIES_MAX)) delete early[old];
      this.#setJson("early", early);
      return { task: null, ended: false };
    }
    return this.#changing((m) => this.#endTask(call, m, this.#task(id), text));
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
  // `upto` (all by default), up to the first not summarized yet.
  view({ upto = null } = {}) {
    const m = this.#memory();
    const r = M.render(m, upto ?? m.T);
    return { text: r.text, bytes: r.bytes, parts: r.parts, T: m.T, settled: r.settled };
  }

  zoom({ id, n }) {
    return { text: M.zoom(this.#memory(), id, n, (i) => this.#message(i)) };
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

  search({ q, limit = SEARCH_TOOL_MAX, thread = null }) {
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
  // `user` and `talk`, in the conversation's thread (made on its first
  // part, titled from it). No turn is started: an import plays history, it
  // asks the agent nothing. A part already landed changes nothing, so a
  // retry or a rerun resumes; one past what landed is refused (parts go in
  // order). The compactor is started when none ran lately.
  import({ source, conversation, from = 0, total = null, messages }, call) {
    need(typeof source === "string" && IMPORT_SOURCE.test(source), "import: source is a word (claude-code, codex, …)");
    const id = conversation?.id;
    need(typeof id === "string" && id.length >= 1 && id.length <= IMPORT_ID_MAX, `import: conversation.id is 1 to ${IMPORT_ID_MAX} characters`);
    need(isInt(from), "import: from is the index of the part's first message");
    need(Array.isArray(messages) && messages.length >= 1 && messages.length <= IMPORT_MESSAGES_MAX, `import: 1 to ${IMPORT_MESSAGES_MAX} messages`);
    for (const msg of messages) {
      need(msg && (msg.role === "user" || msg.role === "assistant") && typeof msg.text === "string" && msg.text.trim().length > 0 && isInt(msg.at), "import: each message is {role: user|assistant, text, at}");
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
      for (const msg of fresh) this.#log(call, m, msg.role === "user" ? "user" : "talk", msg.text, { thread, at: msg.at, publish: false });
      const n = landed + fresh.length;
      sql.exec(
        "INSERT INTO import (source, conv, thread, n, at) VALUES (?, ?, ?, ?, ?) ON CONFLICT (source, conv) DO UPDATE SET n = excluded.n, at = excluded.at",
        source, id, thread, n, now,
      );
      call.publish("log", { type: "import", source, conversation: id, thread, n, total: isInt(total) ? total : null, T: m.T });
      // a pump planned lately is running, or about to: else one is started
      const last = Math.max(Number(this.#get("pump_at") ?? 0), Number(this.#get("kicked") ?? 0));
      if (now - last > PUMP_KICK_MS) {
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
        count: sql.exec("SELECT COUNT(*) AS n FROM log WHERE thread = ? AND kind IN ('user', 'talk')", t.id).one().n,
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
        `SELECT i, kind, text, at, persona, task FROM log WHERE thread = ?${before !== null ? " AND i < ?" : ""} ORDER BY i DESC LIMIT ?`,
        ...(before !== null ? [id, before, n + 1] : [id, n + 1]),
      )
      .toArray();
    const { messages, cut } = this.#page(rows.slice(0, n));
    return { thread: { id: t.id, title: t.title, persona: t.persona, started: t.started, last: t.last }, messages, more: rows.length > n || cut };
  }

  context({ i, before = 5, after = 5 }) {
    need(isInt(i), "context: i is a message's id");
    const lo = Math.max(0, i - clamp(before, 0, 50));
    const hi = i + clamp(after, 0, 50);
    const rows = this.ctx.storage.sql.exec("SELECT i, kind, text, at, thread, persona, task FROM log WHERE i BETWEEN ? AND ? ORDER BY i DESC", lo, hi).toArray();
    return { messages: this.#page(rows).messages };
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
    return { parts, bytes: m.bytes, T: m.T, ...(cut ? { cut } : {}) };
  }

  node({ id, n }) {
    const m = this.#memory();
    need(M.isLine(m, id, n), `no line ${id}+${n}`);
    if (n === 1) return { message: this.#message(id) };
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
    return {
      turn: l ? { running: true, thread: l.thread, since: l.since } : null,
      queued: this.#json("queue", []).length,
      unbuilt: m.T - M.first(m),
      T: m.T,
      hands: this.#get("agent") !== null,
      failing: Object.values(this.#json("fails", {})).map((f) => ({ id: f.id, n: f.n, error: f.error, tries: f.tries })),
      // the compactor: whether it has a node to build now, when a pump last
      // planned, and the view's size (over VIEW while merges lag)
      ready: M.ready(m, { max: 1 }).length > 0,
      pump: pumped ? { at: pumped } : null,
      view: m.bytes,
      import: imp.conversations ? { conversations: imp.conversations, messages: imp.messages, last: imp.last } : null,
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
      if (typeof b.text !== "string" || !b.text.trim() || typeof b.thread !== "string" || !THREAD.test(b.thread)) {
        return { why: "a say record is {text, thread, persona?}, thread t_ and 16 hex" };
      }
      const h = await s.call("hear", { text: b.text, thread: b.thread, persona: typeof b.persona === "string" ? b.persona : null });
      if (h.running) return { i: h.i, queued: true };
    }
    return this.#turns(job, s);
  }

  // Turns while messages are queued (spec 7, docs/optchat.md "Turns"), each
  // ended and its thread classified, then the compactor behind them. Past
  // a run's budget, the rest go to a fresh run.
  async #turns(job, s) {
    const members = await s.members();
    const agent = members.filter((m) => m.kind === "agent").sort((a, b) => (a.addedAt ?? 0) - (b.addedAt ?? 0))[0]?.principal ?? null;
    let turns = 0;
    for (;;) {
      if (turns > 0 && (turns >= TURNS_PER_RUN || s.n > TURN_START_STEPS || s.bytes > RESULTS_SOFT_BYTES)) {
        await s.call("heard", { resume: true });
        return { turns, continued: true };
      }
      const b = await s.call("turn_begin", { run: job.run, agent });
      if (!b.took) {
        if (turns > 0) await s.call("pump", {});
        return { turns, why: b.why };
      }
      turns++;
      const end = await this.#turn(job, s, b, agent);
      const e = await s.call("turn_end", { run: job.run, thread: b.thread, state: end.state, error: end.error ?? null, requeue: end.requeue ?? [] });
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

  // One turn: settle, render the view once, then model calls with tools
  // until one answers without a tool call (at most CALLS_MAX).
  async #turn(job, s, b, agent) {
    const thread = b.thread;
    try {
      if (!b.settled) {
        const settled = await this.#settle(job, s, b);
        if (settled !== true) return settled;
        await s.publish("log", { type: "turn", thread, state: "thinking" });
      }
      const v = await s.call("view", { upto: b.tail });
      if (!v.settled) return { state: "error", error: "the memory is not summarized up to this message" };
      const hands = b.persona.hands === true && agent !== null;
      const tools = [TOOLS.zoom, TOOLS.date, TOOLS.search, ...(hands ? [TOOLS.computer] : [])];
      const messages = [
        { role: "system", content: system(b.persona, b.about) },
        { role: "user", content: [{ type: "text", text: v.text }, { type: "text", text: b.texts.join("\n\n") }] },
      ];
      let convo = 0;
      let last = false;
      for (let calls = 0; calls < CALLS_MAX; calls++) {
        last = last || calls === CALLS_MAX - 1 || s.left() < 3 || s.bytes > RESULTS_SOFT_BYTES || convo > CONVO_MAX_BYTES;
        const a = await s.text({
          model: "medium",
          messages,
          tools,
          tool_choice: last ? "none" : "auto",
          draft: { channel: "log", turn: `turn:${thread}` },
        });
        // `message` is the platform's answer with tools; without it, the text
        const msg = a && typeof a.message === "object" && a.message !== null ? a.message : { role: "assistant", content: a?.text ?? "" };
        const content = typeof msg.content === "string" ? msg.content : typeof a?.text === "string" ? a.text : "";
        const asked = !last && Array.isArray(msg.tool_calls) ? msg.tool_calls.slice(0, TOOL_CALLS_ANSWERED) : [];
        const entries = [];
        if (content.trim()) entries.push({ kind: "talk", text: content });
        const results = [];
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
          // a tool's steps (3 at most), this answer's log, and a last call and its log
          else if (s.left() < 6) {
            out = { text: "Error: this turn is out of steps; answer with what you have." };
            last = true;
          } else out = await this.#tool(job, s, name, args, { thread, hands, agent });
          const echo = M.capText(out.text);
          entries.push({ kind: "tool", text: `${name} ${args === null ? String(raw) : JSON.stringify(args)}`, task: out.task ?? null });
          entries.push({ kind: "echo", text: echo, task: out.task ?? null });
          results.push({ role: "tool", tool_call_id: String(tc?.id ?? `call_${calls}_${k}`), content: echo });
        }
        const lg = await s.call("logged", { run: job.run, thread, persona: b.persona.id, entries });
        if (!asked.length) return { state: "done" };
        if (lg.stopped) return { state: "stopped" };
        const said = { role: "assistant", content: msg.content ?? null, tool_calls: asked };
        messages.push(said, ...results);
        convo += sizeOf(said) + results.reduce((n, r) => n + sizeOf(r), 0);
      }
      return { state: "done" };
    } catch (e) {
      return { state: "error", error: describe(e) };
    }
  }

  // Spec 6: no turn sees a placeholder. Builds the view's unsummarized
  // lines before the turn's messages, level 0 in order a batch at a time
  // (sharing the work with a pump through leases), and merges while the
  // view is over its budget (a long backlog, as an import leaves); or waits
  // for another run building them. Answers true, or how the turn ends.
  async #settle(job, s, b) {
    const began = s.n;
    const fails = new Map();
    const skip = [];
    let waits = 0;
    for (;;) {
      const plan = await s.call("pump_plan", { run: job.run, upto: b.tail, skip });
      if (plan.settled) return true;
      if (plan.stopped) return { state: "stopped" };
      if (s.n - began > SETTLE_STEPS || s.left() < 2 * NODE_STEPS + 2) return { state: "settling", requeue: b.taken, handOn: true };
      if (plan.nodes.length || plan.merges.length) {
        waits = 0;
        const failed = [...(await this.#build(job, s, plan.nodes)), ...(await this.#build(job, s, plan.merges))];
        for (const f of failed) {
          // a merge that failed waits for the pump: the turn needs level 0 alone
          if (f.l > 0) {
            skip.push(M.key(f.l, f.i));
            continue;
          }
          const k = M.key(f.l, f.i);
          fails.set(k, (fails.get(k) ?? 0) + 1);
          if (fails.get(k) >= SETTLE_FAILS_MAX) {
            return { state: "error", error: `the memory could not summarize message ${f.i * 2 ** f.l}: ${f.error}` };
          }
        }
        if (failed.some((f) => f.l === 0)) await s.sleep(RETRY_MS);
      } else {
        // another run is building what this turn waits for
        waits++;
        await s.sleep(Math.min(1000 * waits, 5000));
        if ((await s.call("turn_touch", { run: job.run })).stopped) return { state: "stopped" };
      }
    }
  }

  // The compactor (spec 4.2, 4.3): a node alone in its own conversation,
  // asked again with the line cut at the limit until it fits or TRIES,
  // then written (`node_built`, first write wins). Two or more go in one
  // call (#buildBatch): a job's steps run one at a time (cell/platform.mjs),
  // so the spec's parallel JOBS would be one call after another, each with
  // the whole view as its context. Answers the nodes that failed.
  async #build(job, s, nodes) {
    if (nodes.length >= 2) return this.#buildBatch(job, s, nodes);
    const failed = [];
    for (const { l, i } of nodes) {
      // read from this instance as the step is built (see the top)
      let messages = M.compactMessages(COMPACT, this.#compactInput(l, i));
      let tries = [];
      let text = null;
      let error = null;
      for (;;) {
        let a;
        try {
          a = await s.text({ model: "cheap", messages, max_tokens: COMPACT_TOKENS });
        } catch (e) {
          error = describe(e);
          break;
        }
        const r = M.compactTry(tries, a?.text);
        if (r.fail) {
          error = r.fail;
          break;
        }
        tries = r.tries;
        if (r.text !== undefined) {
          text = r.text;
          break;
        }
        messages = [...messages, { role: "assistant", content: a.text }, { role: "user", content: r.retry }];
      }
      await s.call("node_built", { run: job.run, l, i, text, error });
      if (text === null) failed.push({ l, i, error });
    }
    return failed;
  }

  #compactInput(l, i) {
    return M.compactInput(this.#memory(), l, i, (k) => this.#line(k));
  }

  // A batch in one call (docs/optchat.md, "Importing chats"): level-0
  // nodes in a row, or merges, a numbered line each, with the spec's SCALE
  // and its cut-at-limit retry line by line, all of them in the same
  // conversation (M.batchTry). Its lines are written at once
  // (`nodes_built`); a line that never came, or a call that failed, fails
  // its node alone.
  async #buildBatch(job, s, nodes) {
    // read from this instance as the step is built (see the top)
    const m = this.#memory();
    const input = nodes[0].l === 0 ? M.compactBatchInput(m, nodes, (k) => this.#line(k)) : M.compactMergeBatchInput(m, nodes);
    let messages = M.compactMessages(COMPACT, input);
    let state = M.batchStart(nodes.length);
    let lines = null;
    let error = null;
    for (;;) {
      let a;
      try {
        a = await s.text({ model: "cheap", messages, max_tokens: BATCH_TOKENS });
      } catch (e) {
        error = describe(e);
        lines = M.batchDone(state);
        break;
      }
      const r = M.batchTry(state, a?.text);
      state = r.state;
      if (r.lines) {
        lines = r.lines;
        break;
      }
      messages = [...messages, { role: "assistant", content: a?.text ?? "" }, { role: "user", content: r.retry }];
    }
    const results = nodes.map(({ l, i }, k) => ({ l, i, text: lines[k], error: lines[k] === null ? (error ?? "the compactor wrote no line for it") : null }));
    await s.call("nodes_built", { run: job.run, nodes: results });
    return results.filter((x) => x.text === null).map(({ l, i, error }) => ({ l, i, error }));
  }

  // The compactor's job (docs/optchat.md, "The compactor"): rounds of a
  // level-0 batch and up to JOBS ready merges until none is ready; a node
  // that failed waits for the next pump. Past PUMP_ROUNDS_MAX or the run's
  // budget with work left, a fresh pump takes the rest (a chain the hop
  // limit ends at 16; an import's next part, or `fragment mind import`
  // following it, starts another).
  async pump(input, job) {
    const s = new Steps(job);
    const skip = [];
    let built = 0;
    for (let round = 0; round < PUMP_ROUNDS_MAX; round++) {
      if (s.left() < 2 * NODE_STEPS + 2 || s.bytes > RESULTS_SOFT_BYTES) {
        await s.call("pump", {});
        return { built, continued: true };
      }
      const plan = await s.call("pump_plan", { run: job.run, skip });
      if (!plan.nodes.length && !plan.merges.length) return { built, unbuilt: plan.T - plan.first };
      // level 0 first (a turn waits on it, spec 6), then the merges behind
      // it, which keep the view near its budget while level 0 catches up
      const failed = [...(await this.#build(job, s, plan.nodes)), ...(await this.#build(job, s, plan.merges))];
      built += plan.nodes.length + plan.merges.length - failed.length;
      for (const f of failed) skip.push(M.key(f.l, f.i));
    }
    // its rounds spent with work left: a fresh run takes the rest
    await s.call("pump", {});
    return { built, rounds: PUMP_ROUNDS_MAX, continued: true };
  }

  // One tool call of a turn: zoom, date and search are queries; computer
  // hands the task to goose on `chat` (docs/optchat.md, "Hand-offs").
  async #tool(job, s, name, args, ctx) {
    switch (name) {
      case "zoom": {
        const id = Number(args.id);
        const n = Number(args.n);
        if (!isInt(id) || !Number.isSafeInteger(n) || n < 1) return { text: `No line ${args.id}+${args.n}.` };
        return { text: (await s.call("zoom", { id, n })).text };
      }
      case "date": {
        const id = Number(args.id);
        if (!isInt(id)) return { text: `No message ${args.id}.` };
        return { text: (await s.call("date", { id })).text };
      }
      case "search": {
        const q = String(args.q ?? "").slice(0, 256);
        if (!words(q).length) return { text: "Error: search needs words (q)." };
        const limit = clamp(Number(args.limit ?? SEARCH_TOOL_MAX), 1, SEARCH_TOOL_MAX);
        const { results } = await s.call("search", { q, limit });
        return { text: results.length ? results.map((r) => `${r.i}+1|${r.kind}: ${r.snippet}`).join("\n") : "No match." };
      }
      case "computer": {
        if (!ctx.hands) return { text: "Error: no computer is at hand for this persona; answer yourself." };
        const task = String(args.task ?? "").trim();
        if (!task) return { text: "Error: computer needs the task, in words." };
        // the agent's fragment names the turn its bridge gives the task
        const agentFragment = (await s.people([ctx.agent]))?.[ctx.agent]?.fragment;
        if (typeof agentFragment !== "string" || !agentFragment) return { text: "Error: the computer's agent has no fragment to hand work to." };
        // the run and this step: the same id on every re-run
        const id = `w${job.run}-${s.n}`;
        const posted = await s.publish("chat", { text: `${M.cutBytes(task, TASK_TEXT_MAX)}\n\n(task ${id}, thread ${ctx.thread})`, to: [ctx.agent] });
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
    const h = await s.call("hands_reply", { turn: b.turn, text: typeof b.text === "string" ? b.text : "" });
    if (!h.ended || h.running) return h;
    return this.#turns(job, s);
  }
}
