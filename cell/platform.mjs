// Platform code that runs inside the app facet, around the author's App
// class (docs/MODEL.md, Operations). A mutation is synchronous: the
// author's method, its ledger row, and the effects it asked for commit in
// one facet-local transactionSync, so the facet's own database decides
// replays and conflicting bodies, and the ledger doubles as an outbox the
// supervisor applies (and re-applies idempotently) after the commit. (A
// root storage transaction cannot enclose a facet image above ~1.6 MB,
// and a capability call inside one deadlocks or is refused:
// spikes/apps/README.md.)
//
// Every method receives (input, call): `call.principal` and `call.role`
// name the caller (an agent acting for someone: them, the agent in
// `call.agent`); in a mutation, `call.publish(channel, body, kind)`
// appends a record to a channel declared in fragment.json once the
// mutation commits, and `call.files.write(path, content)` /
// `call.files.remove(path)` change files on `main` then (one commit).
// `this.files.read / readBytes / list / stat` read files at `main` from
// anything async (queries, fetch, jobs).
//
// A job's method gets (input, job) and runs as a Workflow: each `await
// job.call / job.fetch / job.publish / job.sleep` is a durable step. The
// body re-runs from the top at every step with the results so far, so it
// must reach its steps in the same order each time and change nothing
// except through steps (docs/api.md, Jobs).
//
// Every answer is an envelope, so no value an author returns can be
// mistaken for a platform answer: a query's { result }, a mutation's
// { result, replayed, effects, run }, or either's { error } (the
// supervisor decodes each into its type once: fragment_core::facet); for
// a job, { next } (its next step), { done, output }, or { failed }. A
// query's, a mutation's, and a ledger lookup's answers cross as JSON text
// made here, once: an input arrives as text and is parsed once, and a
// result is spliced into its envelope as the text JSON.stringify made (or
// the ledger stored), which the supervisor passes on without parsing it.
// Answers are plain JSON, so nothing about one can fail after its
// mutation committed.
//
// The checks here run in the author's realm, which can patch what they
// rely on: they exist so an author sees a refusal while the mutation can
// still roll back. The supervisor checks every effect again in Rust
// (fragment_core::effects) before it applies any. Their limits and rules
// come from limits.js, which the cell generates from the Rust definitions
// (fragment_core::facet), so the two sides count the same way.
import { App as AuthorApp } from "./app.js";
import {
  RECORD_BODY_MAX_BYTES,
  EFFECTS_MAX,
  RESULT_MAX_BYTES,
  APP_DB_MAX_BYTES,
  FILE_WRITE_MAX_BYTES,
  FILE_WRITES_MAX,
  PATH_MAX_BYTES,
  POINTER_MAX_BYTES,
  PUSH_WHO_MAX_CHARS,
  PUSH_PAYLOAD_MAX_BYTES,
  RESERVED_OP_NAMES,
  utf8Bytes,
  validKind,
  validRepoPath,
  isBlobPointer,
} from "./limits.js";

const LEDGER = "_fragment_ops";
// A mutation that leaves the app's database over APP_DB_MAX_BYTES rolls
// back. The node's own hard stop (CELLD_FACET_MAX_BYTES) sits above it, so
// the runtime's bookkeeping always has room.
const STORAGE_FULL = Symbol("storage_full");
// Half of a character (a lone surrogate, as from cutting a string inside an
// emoji) in JSON text: JSON.stringify escapes one as \udXXX (lowercase), and
// the supervisor's JSON reader refuses it, so it is refused here while the
// mutation can still roll back. An even run of backslashes before it is
// escaped backslashes, not the escape.
const LONE_SURROGATE = /(?<!\\)(?:\\\\)*\\ud[89a-f]/;
// A step not yet taken never settles: the body stops there, and a try/catch
// or Promise.all in author code cannot swallow the suspension.
const NEVER = new Promise(() => {});
const SLEEP_MAX_MS = 30 * 24 * 3600 * 1000;
const DURATION = /^(\d+)\s*(second|minute|hour|day)s?$/;
const UNIT_MS = { second: 1000, minute: 60_000, hour: 3_600_000, day: 86_400_000 };

function describe(e) {
  return e instanceof Error ? `${e.name}: ${e.message}` : String(e);
}

// A text's UTF-8 size is at least its length in UTF-16 units, so a text
// longer than the limit is over it before it is encoded to be counted.
function overBytes(text, max) {
  return text.length > max || utf8Bytes(text) > max;
}

// A result as JSON text, within the limit and of whole characters.
function resultText(value) {
  const text = JSON.stringify(value ?? null) ?? "null";
  if (overBytes(text, RESULT_MAX_BYTES)) throw new Error(`a result is at most ${RESULT_MAX_BYTES} bytes`);
  if (LONE_SURROGATE.test(text)) throw new Error("a result holds whole characters: this one holds half of one (a lone surrogate)");
  return text;
}

// A mutation's answer, its effects and result spliced in as the texts they are.
function mutated(replayed, run, effects, result) {
  return `{"replayed":${replayed},"run":${JSON.stringify(run)},"effects":${effects},"result":${result}}`;
}

function base64(data) {
  let s = "";
  for (let i = 0; i < data.length; i += 0x8000) s += String.fromCharCode(...data.subarray(i, i + 0x8000));
  return btoa(s);
}

// 20 seconds apart: a video has about 15 minutes to finish.
const VIDEO_POLLS_MAX = 45;
// 2, 4, 8, 16, then 30 seconds apart: an agent's turn has about 20 minutes
// to end, in at most 81 of a run's 256 steps.
const AGENT_POLLS_MAX = 40;
const AGENT_POLL_MS_MAX = 30_000;

function checkPush(who, payload) {
  if (typeof who !== "string" || [...who].length > PUSH_WHO_MAX_CHARS) {
    throw new Error(`push(who, payload): who is a string of at most ${PUSH_WHO_MAX_CHARS} characters`);
  }
  if (utf8Bytes(JSON.stringify(payload ?? {})) > PUSH_PAYLOAD_MAX_BYTES) throw new Error(`a push payload is at most ${PUSH_PAYLOAD_MAX_BYTES} bytes`);
  return payload ?? {};
}

function checkPath(path) {
  if (!validRepoPath(path)) {
    throw new Error(`${JSON.stringify(path)} is not a file path (relative, no . or .. segments, at most ${PATH_MAX_BYTES} bytes)`);
  }
}

// A file's content as a step or effect carries it: { text } or { base64 }.
// It may not be a large-file pointer.
function content(path, data) {
  const pointer = () => new Error(`${path}: an app does not write blob pointers`);
  if (typeof data === "string") {
    // more UTF-16 units than a pointer's bytes: more bytes, too
    if (data.length <= POINTER_MAX_BYTES && isBlobPointer(new TextEncoder().encode(data))) throw pointer();
    return { text: data };
  }
  const bytes = data instanceof ArrayBuffer ? new Uint8Array(data) : data;
  if (!(bytes instanceof Uint8Array)) throw new TypeError("a file's content is a string, a Uint8Array, or an ArrayBuffer");
  if (isBlobPointer(bytes)) throw pointer();
  return { base64: base64(bytes) };
}

function contentSize(c) {
  return c.text !== undefined ? utf8Bytes(c.text) : Math.floor((c.base64.length * 3) / 4);
}

// A mutation's file changes, applied to `main` as one commit after it commits.
class FileEffects {
  #effects;
  #size = 0;
  #count = 0;

  constructor(effects) {
    this.#effects = effects;
  }

  #push(effect, size) {
    if (this.#count >= FILE_WRITES_MAX) throw new Error(`a mutation writes at most ${FILE_WRITES_MAX} files`);
    if (this.#size + size > FILE_WRITE_MAX_BYTES) throw new Error(`a mutation writes at most ${FILE_WRITE_MAX_BYTES} bytes of files`);
    if (this.#effects.length >= EFFECTS_MAX) throw new Error(`a mutation has at most ${EFFECTS_MAX} effects`);
    this.#count++;
    this.#size += size;
    this.#effects.push(effect);
  }

  write(path, data) {
    checkPath(path);
    const c = content(path, data);
    this.#push({ file: path, ...c }, contentSize(c));
  }

  remove(path) {
    checkPath(path);
    this.#push({ file: path }, 0);
  }
}

function authorMethod(name) {
  return !name.startsWith("__") && !RESERVED_OP_NAMES.has(name) && typeof AuthorApp.prototype[name] === "function";
}

// The platform's own read of a call's effects: author code gets no
// reference to the list, only publish, push, and files.
let effectsOf;

class Call {
  #channels;
  #effects;

  static {
    effectsOf = (call) => call.#effects;
  }

  constructor(meta, mutation) {
    this.principal = meta.principal;
    this.agent = meta.agent ?? null;
    this.role = meta.role;
    this.#channels = new Set(meta.channels || []);
    this.#effects = mutation ? [] : null;
  }

  #files = null;

  get files() {
    if (this.#effects === null) throw new Error("call.files is for mutations; read files with this.files");
    this.#files ??= new FileEffects(this.#effects);
    return this.#files;
  }

  // A web push to the subscriptions tagged `who` ("*": all), sent once
  // the mutation commits. `payload` is shown by the service worker:
  // { title, body, tag, url }.
  push(who, payload) {
    if (this.#effects === null) throw new Error("push is for mutations; a query cannot push");
    const text = JSON.stringify(checkPush(who, payload));
    if (this.#effects.length >= EFFECTS_MAX) throw new Error(`a mutation has at most ${EFFECTS_MAX} effects`);
    this.#effects.push({ push: who, payload: JSON.parse(text) });
  }

  publish(channel, body, kind = "message") {
    if (this.#effects === null) throw new Error("publish is for mutations; a query cannot publish");
    if (!this.#channels.has(channel)) throw new Error(`channel ${channel} is not declared in fragment.json`);
    if (!validKind(kind)) throw new Error("kind must match ^[a-z][a-z0-9._-]{0,63}$");
    const text = JSON.stringify(body ?? null);
    if (utf8Bytes(text) > RECORD_BODY_MAX_BYTES) throw new Error(`a record's body is at most ${RECORD_BODY_MAX_BYTES} bytes`);
    if (this.#effects.length >= EFFECTS_MAX) throw new Error(`a mutation publishes at most ${EFFECTS_MAX} records`);
    this.#effects.push({ channel, kind, body: JSON.parse(text) });
  }
}

// A step that failed for good (its retries ran out, or it was refused).
class StepError extends Error {
  constructor(kind, message) {
    super(message);
    this.name = "StepError";
    this.kind = kind;
  }
}

// What job.fetch resolves to: the parts of a Response a step can record.
class JobResponse {
  constructor(v) {
    this.status = v.status;
    this.ok = v.status >= 200 && v.status < 300;
    this.headers = new Headers(v.headers || {});
    this.body = v.body ?? "";
  }
  text() {
    return this.body;
  }
  json() {
    return JSON.parse(this.body);
  }
}

class Job {
  #channels;
  #results;
  #index = 0;
  #onStep;

  constructor(meta, results, onStep) {
    this.principal = meta.principal;
    this.role = meta.role;
    this.run = meta.run;
    this.attempt = meta.attempt;
    this.#channels = new Set(meta.channels || []);
    this.#results = results;
    this.#onStep = onStep;
  }

  #step(kind, args) {
    const index = this.#index++;
    const done = this.#results[index];
    if (!done) {
      this.#onStep({ index, kind, args });
      return NEVER;
    }
    if (done.kind !== kind) {
      return Promise.reject(new Error(`step ${index} was ${done.kind} when this run took it and is ${kind} now: the job's code changed under the run`));
    }
    if ("error" in done) return Promise.reject(new StepError(kind, done.error));
    return Promise.resolve(done.value);
  }

  // An HTTP request from the platform (the app itself has no network).
  // Header values may name the fragment's secrets as {{NAME}}.
  fetch(url, init = {}) {
    if (typeof url !== "string") throw new TypeError("job.fetch(url, init): url is a string");
    const headers = {};
    for (const [k, v] of Object.entries(init.headers || {})) headers[k] = String(v);
    let body = init.body;
    if (body != null && typeof body !== "string") {
      body = JSON.stringify(body);
      if (!Object.keys(headers).some((k) => k.toLowerCase() === "content-type")) headers["content-type"] = "application/json";
    }
    const args = { url, method: init.method || "GET", headers };
    if (body != null) args.body = body;
    return this.#step("fetch", args).then((v) => new JobResponse(v));
  }

  // An operation of this fragment, as the run's principal. Calling a job
  // starts it and resolves to { run, status }.
  call(op, input = {}) {
    if (typeof op !== "string") throw new TypeError("job.call(op, input): op is an operation name");
    return this.#step("call", { op, input: JSON.parse(JSON.stringify(input ?? {})) });
  }

  publish(channel, body, kind = "message") {
    if (!this.#channels.has(channel)) throw new Error(`channel ${channel} is not declared in fragment.json`);
    if (!validKind(kind)) throw new Error("kind must match ^[a-z][a-z0-9._-]{0,63}$");
    const text = JSON.stringify(body ?? null);
    if (utf8Bytes(text) > RECORD_BODY_MAX_BYTES) throw new Error(`a record's body is at most ${RECORD_BODY_MAX_BYTES} bytes`);
    return this.#step("publish", { channel, kind, body: JSON.parse(text) });
  }

  // Files at `main`, as steps: reads are recorded, writes are one commit
  // each; `{ expect }` names the blob sha the file must have (`stat`'s
  // `sha`), or null for "must not exist", and a mismatch fails the step.
  get files() {
    const step = (kind, args) => this.#step(kind, args);
    return {
      read: (path) => (checkPath(path), step("files.read", { path })).then((v) => (v === null ? null : v.text ?? Uint8Array.from(atob(v.base64), (c) => c.charCodeAt(0)))),
      list: (prefix = "") => step("files.list", { prefix: String(prefix) }),
      stat: (path) => (checkPath(path), step("files.stat", { path })),
      write: (path, data, { expect } = {}) => {
        checkPath(path);
        const c = content(path, data);
        if (contentSize(c) > FILE_WRITE_MAX_BYTES) throw new Error(`a job step writes at most ${FILE_WRITE_MAX_BYTES} bytes`);
        return step("files.write", { path, ...c, ...(expect !== undefined ? { expect } : {}) });
      },
      remove: (path, { expect } = {}) => (checkPath(path), step("files.remove", { path, ...(expect !== undefined ? { expect } : {}) })),
    };
  }

  // OpenRouter, with the fragment's OPENROUTER_API_KEY secret:
  //   ai.text({ model, prompt | messages, max_tokens })   → { text, model, usage }
  //   ai.image({ prompt, path, model?, aspect_ratio? })  → { path, size, sha256 }: a file on main
  //   ai.video({ prompt, path, model?, duration?, resolution?, aspect_ratio? })
  // A video takes minutes: the job polls it and sleeps between polls, all as steps.
  get ai() {
    const step = (kind, args) => this.#step(kind, args);
    const clean = (o) => JSON.parse(JSON.stringify(o ?? {}));
    return {
      text: (opts) => step("ai.text", clean(opts)),
      image: (opts = {}) => (checkPath(opts.path), step("ai.image", clean(opts))),
      video: async (opts = {}) => {
        checkPath(opts.path);
        const { path, ...rest } = clean(opts);
        const { id } = await step("ai.video.start", rest);
        for (let i = 0; i < VIDEO_POLLS_MAX; i++) {
          const st = await step("ai.video.poll", { id });
          if (st.status === "completed") return step("ai.video.save", { id, path, url: st.urls?.[0] });
          // which statuses are final is the platform's (ai.rs): the step says
          if (st.ended) throw new Error(`video ${id} ${st.status}${st.error ? `: ${st.error}` : ""}`);
          await this.sleep("20 seconds");
        }
        throw new Error(`video ${id} was not ready after ${VIDEO_POLLS_MAX} polls`);
      },
    };
  }

  // One turn of the fragment's own agent (fragment.json's `agent`), for
  // the run's principal: resolves to { text, turn }. The turn is named by
  // the run and this step, so a retried or replayed run reattaches to it;
  // the job waits for it in polls and sleeps, as for a video. A turn that
  // fails or is stopped throws a StepError.
  async agent({ prompt, conversation, channel } = {}) {
    if (typeof prompt !== "string" || prompt === "") throw new TypeError("job.agent({ prompt }): prompt is a string");
    const { turn } = await this.#step("agent.start", JSON.parse(JSON.stringify({ prompt, conversation, channel })));
    for (let i = 0; i < AGENT_POLLS_MAX; i++) {
      const st = await this.#step("agent.poll", { turn });
      if (st.ended && st.outcome === "idle") return { text: st.text ?? "", turn };
      if (st.ended) throw new StepError("agent.poll", `the agent's turn ended ${st.outcome}${st.error ? `: ${st.error}` : ""}`);
      await this.sleep(Math.min(2000 * 2 ** i, AGENT_POLL_MS_MAX));
    }
    throw new StepError("agent.poll", `the agent's turn ${turn} did not end after ${AGENT_POLLS_MAX} polls`);
  }

  // A web push to the subscriptions tagged `who` ("*": all).
  push(who, payload) {
    return this.#step("push", { who, payload: JSON.parse(JSON.stringify(checkPush(who, payload))) });
  }

  // Milliseconds, or "N seconds|minutes|hours|days"; up to 30 days.
  sleep(duration) {
    let ms = duration;
    if (typeof duration === "string") {
      const m = DURATION.exec(duration.trim());
      ms = m ? Number(m[1]) * UNIT_MS[m[2]] : NaN;
    }
    if (!Number.isFinite(ms) || ms < 0 || ms > SLEEP_MAX_MS) {
      throw new Error('job.sleep takes milliseconds or "N seconds|minutes|hours|days", up to 30 days');
    }
    return this.#step("sleep", { ms: Math.round(ms) });
  }
}

export class App extends AuthorApp {
  constructor(ctx, env) {
    super(ctx, env);
    const sql = ctx.storage.sql;
    sql.exec(`CREATE TABLE IF NOT EXISTS ${LEDGER} (
      id TEXT PRIMARY KEY, name TEXT NOT NULL, input_sha TEXT NOT NULL, result TEXT NOT NULL, at INTEGER NOT NULL,
      effects TEXT NOT NULL DEFAULT '[]', run INTEGER)`);
    // a ledger made before effects (phase 2 slice B), or before the
    // supervisor numbered runs (older rows have none, and are never applied again)
    const cols = sql.exec(`PRAGMA table_info(${LEDGER})`).toArray();
    if (!cols.some((c) => c.name === "effects")) sql.exec(`ALTER TABLE ${LEDGER} ADD COLUMN effects TEXT NOT NULL DEFAULT '[]'`);
    if (!cols.some((c) => c.name === "run")) sql.exec(`ALTER TABLE ${LEDGER} ADD COLUMN run INTEGER`);
  }

  // Reads at `main` through the FILES capability.
  get files() {
    const cap = this.env.FILES;
    return {
      readBytes: (path) => cap.read(path),
      read: async (path) => {
        const data = await cap.read(path);
        return data === null ? null : new TextDecoder().decode(data);
      },
      list: (prefix = "") => cap.list(prefix),
      stat: (path) => cap.stat(path),
    };
  }

  __mutate(id, name, inputSha, inputText, meta) {
    if (!authorMethod(name)) return JSON.stringify({ error: "unknown_operation" });
    try {
      return this.#mutate(id, name, inputSha, JSON.parse(inputText), meta);
    } catch (e) {
      // over the cap (or at the node's hard stop): the transaction rolled back
      if (e === STORAGE_FULL || /database or disk is full|SQLITE_FULL/.test(describe(e))) return JSON.stringify({ error: "storage_full" });
      throw e;
    }
  }

  // meta.run is the supervisor's number for this run, kept with the row;
  // meta.ledgerMs is how long an id is a replay (ops::LEDGER_KEPT_MS).
  #mutate(id, name, inputSha, input, meta) {
    const sql = this.ctx.storage.sql;
    if (!Number.isSafeInteger(meta.run) || !Number.isSafeInteger(meta.ledgerMs) || meta.ledgerMs <= 0) {
      throw new Error("the supervisor names the run and the ledger's window");
    }
    const now = Date.now();
    return this.ctx.storage.transactionSync(() => {
      const prior = sql.exec(`SELECT input_sha, result, effects, run, at FROM ${LEDGER} WHERE id = ?`, id).toArray()[0];
      if (prior && prior.at >= now - meta.ledgerMs) {
        if (prior.input_sha !== inputSha) return JSON.stringify({ error: "conflicting_body" });
        // the stored texts as they are: the supervisor checks them
        return mutated(true, prior.run ?? null, prior.effects, prior.result);
      }
      // Older than the window, the id runs again: a new run, keyed anew.
      if (prior) sql.exec(`DELETE FROM ${LEDGER} WHERE id = ?`, id);
      const call = new Call(meta, true);
      const out = AuthorApp.prototype[name].call(this, input, call);
      if (out && typeof out.then === "function") {
        out.catch(() => {});
        throw new Error(`mutation ${name} returned a promise; mutations are synchronous`);
      }
      const text = resultText(out);
      const effects = JSON.stringify(effectsOf(call));
      if (LONE_SURROGATE.test(effects)) {
        throw new Error("a mutation's effects hold whole characters: this text holds half of one (a lone surrogate)");
      }
      sql.exec(`INSERT INTO ${LEDGER} (id, name, input_sha, result, at, effects, run) VALUES (?, ?, ?, ?, ?, ?, ?)`,
        id, name, inputSha, text, now, effects, meta.run);
      if (sql.exec("SELECT last_insert_rowid() AS r").one().r % 100 === 0) {
        sql.exec(`DELETE FROM ${LEDGER} WHERE at < ?`, now - meta.ledgerMs);
      }
      if (sql.databaseSize > APP_DB_MAX_BYTES) throw STORAGE_FULL;
      return mutated(false, meta.run, effects, text);
    });
  }

  async __query(name, inputText, meta) {
    if (!authorMethod(name)) return JSON.stringify({ error: "unknown_operation" });
    const result = await AuthorApp.prototype[name].call(this, JSON.parse(inputText), new Call(meta, false));
    return `{"result":${resultText(result)}}`;
  }

  // Runs a job's body over the results of the steps it has taken, up to
  // the next step it asks for (the Workflow takes it and asks again).
  async __job(name, input, meta, results) {
    if (!authorMethod(name)) return { error: "unknown_operation" };
    let next = null;
    let wake;
    const asked = new Promise((resolve) => (wake = resolve));
    const job = new Job(meta, results, (step) => {
      if (!next) {
        next = step;
        wake();
      }
    });
    let out;
    try {
      out = await Promise.race([
        (async () => ({ value: await AuthorApp.prototype[name].call(this, input, job) }))(),
        asked,
      ]);
    } catch (e) {
      return next ? { next } : { failed: describe(e) };
    }
    if (next) return { next };
    const text = JSON.stringify(out.value ?? null) ?? "null";
    if (overBytes(text, RESULT_MAX_BYTES)) return { failed: `a job's result is at most ${RESULT_MAX_BYTES} bytes` };
    return { done: true, output: JSON.parse(text) };
  }

  // One ledger row, for the supervisor settling a run it recorded as
  // pending: whether the facet committed that run, and its effects. The
  // supervisor asks only about ids it recorded, and trusts the run number
  // it gave, never an id or principal from this table.
  __ledger(id) {
    const row = this.ctx.storage.sql.exec(`SELECT run, effects FROM ${LEDGER} WHERE id = ?`, String(id)).toArray()[0];
    if (!row) return JSON.stringify({ result: null });
    // A row the supervisor could not read (the app garbled it, or wrote half
    // a character) goes as null, which the supervisor refuses.
    let effects = null;
    try {
      if (!LONE_SURROGATE.test(row.effects)) effects = JSON.parse(row.effects);
    } catch {
      effects = null;
    }
    return JSON.stringify({ result: { run: row.run ?? null, effects } });
  }

  // Custom routes: the author's fetch, when there is one.
  fetch(request) {
    const own = AuthorApp.prototype.fetch;
    if (typeof own !== "function") return new Response("not found\n", { status: 404 });
    return own.call(this, request);
  }
}
