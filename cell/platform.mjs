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
// name the caller; in a mutation, `call.publish(channel, body, kind)`
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
// mistaken for a platform answer: { result, replayed, effects } or { error };
// for a job, { next } (its next step), { done, output }, or { failed }.
import { App as AuthorApp } from "./app.js";

const LEDGER = "_fragment_ops";
// Replays are recognized for a week; older ledger rows are pruned.
const LEDGER_TTL_MS = 7 * 24 * 3600 * 1000;
const RECORD_MAX_BYTES = 64 * 1024;
const EFFECTS_MAX = 64;
const RESULT_MAX_BYTES = 1024 * 1024;
const KIND = /^[a-z][a-z0-9._-]{0,63}$/;
const FILE_WRITE_MAX_BYTES = 256 * 1024;
const FILE_WRITES_MAX = 16;
const PATH = /^(?!\/)(?!.*\/$)(?!.*(^|\/)\.{1,2}(\/|$))(?!.*\/\/)[^\\\x00-\x1f]{1,300}$/;
const RESERVED = new Set(["constructor", "fetch", "alarm", "webSocketMessage", "webSocketClose", "webSocketError"]);
// A step not yet taken never settles: the body stops there, and a try/catch
// or Promise.all in author code cannot swallow the suspension.
const NEVER = new Promise(() => {});
const SLEEP_MAX_MS = 30 * 24 * 3600 * 1000;
const DURATION = /^(\d+)\s*(second|minute|hour|day)s?$/;
const UNIT_MS = { second: 1000, minute: 60_000, hour: 3_600_000, day: 86_400_000 };

function describe(e) {
  return e instanceof Error ? `${e.name}: ${e.message}` : String(e);
}

function bytes(text) {
  return new TextEncoder().encode(text).length;
}

function base64(data) {
  let s = "";
  for (let i = 0; i < data.length; i += 0x8000) s += String.fromCharCode(...data.subarray(i, i + 0x8000));
  return btoa(s);
}

const PUSH_MAX_BYTES = 3800;
// 20 seconds apart: a video has about 15 minutes to finish.
const VIDEO_POLLS_MAX = 45;

function checkPush(who, payload) {
  if (typeof who !== "string" || who.length > 64) throw new Error("push(who, payload): who is a string of at most 64 characters");
  if (bytes(JSON.stringify(payload ?? {})) > PUSH_MAX_BYTES) throw new Error(`a push payload is at most ${PUSH_MAX_BYTES} bytes`);
  return payload ?? {};
}

function checkPath(path) {
  if (typeof path !== "string" || !PATH.test(path)) throw new Error(`${JSON.stringify(path)} is not a file path (relative, no . or .. segments)`);
}

// A file's content as a step or effect carries it: { text } or { base64 }.
function content(data) {
  if (typeof data === "string") return { text: data };
  if (data instanceof Uint8Array) return { base64: base64(data) };
  if (data instanceof ArrayBuffer) return { base64: base64(new Uint8Array(data)) };
  throw new TypeError("a file's content is a string, a Uint8Array, or an ArrayBuffer");
}

function contentSize(c) {
  return c.text !== undefined ? bytes(c.text) : Math.floor((c.base64.length * 3) / 4);
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
    const c = content(data);
    this.#push({ file: path, ...c }, contentSize(c));
  }

  remove(path) {
    checkPath(path);
    this.#push({ file: path }, 0);
  }
}

// Reads at `main` through the FILES capability.
class FilesReader {
  #cap;

  constructor(cap) {
    this.#cap = cap;
  }

  readBytes(path) {
    return this.#cap.read(path);
  }

  async read(path) {
    const data = await this.#cap.read(path);
    return data === null ? null : new TextDecoder().decode(data);
  }

  list(prefix = "") {
    return this.#cap.list(prefix);
  }

  stat(path) {
    return this.#cap.stat(path);
  }
}

function authorMethod(name) {
  return !name.startsWith("__") && !RESERVED.has(name) && typeof AuthorApp.prototype[name] === "function";
}

class Call {
  #channels;
  #effects;

  constructor(meta, mutation) {
    this.principal = meta.principal;
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
    if (typeof kind !== "string" || !KIND.test(kind)) throw new Error("kind must match ^[a-z][a-z0-9._-]{0,63}$");
    const text = JSON.stringify(body ?? null);
    if (new TextEncoder().encode(text).length > RECORD_MAX_BYTES) throw new Error(`a record's body is at most ${RECORD_MAX_BYTES} bytes`);
    if (this.#effects.length >= EFFECTS_MAX) throw new Error(`a mutation publishes at most ${EFFECTS_MAX} records`);
    this.#effects.push({ channel, kind, body: JSON.parse(text) });
  }

  get effects() {
    return this.#effects;
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
    if (typeof kind !== "string" || !KIND.test(kind)) throw new Error("kind must match ^[a-z][a-z0-9._-]{0,63}$");
    const text = JSON.stringify(body ?? null);
    if (bytes(text) > RECORD_MAX_BYTES) throw new Error(`a record's body is at most ${RECORD_MAX_BYTES} bytes`);
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
        const c = content(data);
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
          if (["failed", "cancelled", "expired"].includes(st.status)) throw new Error(`video ${id} ${st.status}${st.error ? `: ${st.error}` : ""}`);
          await this.sleep("20 seconds");
        }
        throw new Error(`video ${id} was not ready after ${VIDEO_POLLS_MAX} polls`);
      },
    };
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
      effects TEXT NOT NULL DEFAULT '[]')`);
    // a ledger made before effects existed (phase 2 slice B)
    if (!sql.exec(`PRAGMA table_info(${LEDGER})`).toArray().some((c) => c.name === "effects")) {
      sql.exec(`ALTER TABLE ${LEDGER} ADD COLUMN effects TEXT NOT NULL DEFAULT '[]'`);
    }
  }

  get files() {
    return new FilesReader(this.env.FILES);
  }

  __mutate(id, name, inputSha, input, meta) {
    if (!authorMethod(name)) return { error: "unknown_operation" };
    const sql = this.ctx.storage.sql;
    return this.ctx.storage.transactionSync(() => {
      const prior = sql.exec(`SELECT input_sha, result, effects FROM ${LEDGER} WHERE id = ?`, id).toArray()[0];
      if (prior) {
        if (prior.input_sha !== inputSha) return { error: "conflicting_body" };
        return { replayed: true, result: JSON.parse(prior.result), effects: JSON.parse(prior.effects) };
      }
      const call = new Call(meta, true);
      const out = AuthorApp.prototype[name].call(this, input, call);
      if (out && typeof out.then === "function") {
        out.catch(() => {});
        throw new Error(`mutation ${name} returned a promise; mutations are synchronous`);
      }
      const result = out ?? null;
      const text = JSON.stringify(result);
      if (new TextEncoder().encode(text).length > RESULT_MAX_BYTES) throw new Error(`a result is at most ${RESULT_MAX_BYTES} bytes`);
      const now = Date.now();
      sql.exec(`INSERT INTO ${LEDGER} (id, name, input_sha, result, at, effects) VALUES (?, ?, ?, ?, ?, ?)`,
        id, name, inputSha, text, now, JSON.stringify(call.effects));
      if (sql.exec("SELECT last_insert_rowid() AS r").one().r % 100 === 0) {
        sql.exec(`DELETE FROM ${LEDGER} WHERE at < ?`, now - LEDGER_TTL_MS);
      }
      return { replayed: false, result, effects: call.effects };
    });
  }

  async __query(name, input, meta) {
    if (!authorMethod(name)) return { error: "unknown_operation" };
    const result = await AuthorApp.prototype[name].call(this, input, new Call(meta, false));
    return { replayed: false, result: result ?? null };
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
    const text = JSON.stringify(out.value ?? null);
    if (bytes(text) > RESULT_MAX_BYTES) return { failed: `a job's result is at most ${RESULT_MAX_BYTES} bytes` };
    return { done: true, output: JSON.parse(text) };
  }

  // The newest ledger rows, for the supervisor's sweep of effects it may
  // not have applied (it died between the facet's commit and its own).
  __recent(limit) {
    const rows = this.ctx.storage.sql
      .exec(`SELECT id, name, effects FROM ${LEDGER} ORDER BY rowid DESC LIMIT ?`, limit)
      .toArray();
    return { result: rows.map((r) => ({ id: r.id, name: r.name, effects: JSON.parse(r.effects) })) };
  }

  // Custom routes: the author's fetch, when there is one.
  fetch(request) {
    const own = AuthorApp.prototype.fetch;
    if (typeof own !== "function") return new Response("not found\n", { status: 404 });
    return own.call(this, request);
  }
}
