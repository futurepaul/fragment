// Test harness: runs the compiled runtime (runtime/src) against an
// in-process mock code.storage server, with fakes for the celld platform
// pieces:
//
//   - DO storage: node:sqlite wrapped in celld's sql.exec().toArray() shape
//   - DO binding routing: an HTTP loopback server that dispatches
//     /__internal/f/<name>/... to the right cell (so the REAL ctx shim,
//     the real workflow driver, and the real internal routes all speak
//     HTTP exactly as production does)
//   - Worker Loader: a file-backed loader that materializes the worker
//     modules the runtime hands it and imports main.mjs (the real
//     WORKFLOW_MAIN / APP_MAIN entrypoints run unmodified)
//   - native Workflows binding: instances with replay-faithful step
//     semantics (memoized results; crashAfter hook models losing a step
//     result AFTER its side effect landed — the exact window where
//     memoization cannot save a network side effect)
//
// The production code under test is imported from ../src (what
// scripts/build-runtime emits) — no test hooks, no reimplementation.
import { DatabaseSync } from "node:sqlite";
import { generateKeyPairSync } from "node:crypto";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

// ---- DO-style sqlite wrapper over node:sqlite ----
function rowsOf(result) {
  return { toArray: () => result || [] };
}
export function makeSql(db) {
  db = db || new DatabaseSync(":memory:");
  const w = {
    _db: db,
    exec(sql, ...params) {
      const trimmed = sql.trim();
      // multi-statement scripts (the SCHEMA bootstrap) need raw exec;
      // single statements go through prepare so SELECT rows come back
      const statements = trimmed.split(";").filter((x) => x.trim().length > 0);
      if (params.length === 0 && statements.length > 1) {
        db.exec(sql);
        return rowsOf([]);
      }
      const upper = trimmed.toUpperCase();
      const reads = upper.startsWith("SELECT") || upper.startsWith("PRAGMA")
        || upper.startsWith("WITH") || (upper.startsWith("INSERT") && trimmed.includes("RETURNING"));
      if (reads) {
        return rowsOf(params.length ? db.prepare(sql).all(...params) : db.prepare(sql).all());
      }
      if (params.length) {
        db.prepare(sql).run(...params);
        return rowsOf([]);
      }
      try { db.prepare(sql).run(); } catch { db.exec(sql); }
      return rowsOf([]);
    },
  };
  return w;
}

export function makeStorage(db) {
  // celld's DurableObjectState: state.storage.sql on one hand, and
  // state.getWebSockets/acceptWebSocket on the other — one object plays
  // both roles in the fake (storage === state, nested once)
  let alarmAt = null;
  const st = {
    sql: makeSql(db),
    setAlarm: async (t) => { alarmAt = t; },
    deleteAlarm: async () => { alarmAt = null; },
    getAlarm: () => alarmAt,
    getWebSockets: () => [],
    acceptWebSocket: () => {},
  };
  st.storage = st;
  return st;
}

let worldCounter = 0;

export async function makeWorld(opts = {}) {
  const { FragmentCell } = await import("../src/cell.js");

  // org keypair: private to the runtime env, public to the mock — the
  // same trust split the real service has (dashboard holds the pubkey)
  const { publicKey, privateKey } = generateKeyPairSync("ec", { namedCurve: "P-256" });
  const privPem = privateKey.export({ type: "pkcs8", format: "pem" }).toString();
  const pubPem = publicKey.export({ type: "spki", format: "pem" }).toString();

  const cells = new Map(); // name -> facade {cell, fetch}
  const hostSecret = opts.hostSecret ?? "test-host-secret-0123456789abcdef";
  const wid = ++worldCounter;

  // ---- the loopback HTTP plane (the celld public listener stand-in) ----
  let loopbackUrl;
  const loopback = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, "http://loopback");
      const m = url.pathname.match(/^\/__internal\/f\/([^/]+)\/(.*)$/);
      if (!m) { res.writeHead(404); res.end("no route"); return; }
      const facade = cells.get(m[1]);
      if (!facade) { res.writeHead(404); res.end("no such cell"); return; }
      const chunks = [];
      req.on("data", (c) => chunks.push(c));
      req.on("end", async () => {
        const body = Buffer.concat(chunks);
        const init = { method: req.method, headers: req.headers };
        if (body.length) init.body = body;
        const resp = await facade.fetch(new Request(`http://loopback${url.pathname}${url.search}`, init));
        const buf = Buffer.from(await resp.arrayBuffer());
        const headers = {};
        resp.headers.forEach((v, k) => { headers[k] = v; });
        res.writeHead(resp.status, headers);
        res.end(buf);
      });
    } catch (e) {
      res.writeHead(500); res.end(String(e && e.stack || e));
    }
  });
  await new Promise((r) => loopback.listen(0, "127.0.0.1", r));
  loopbackUrl = `http://127.0.0.1:${loopback.address().port}`;

  const env = {
    FRAGMENT_HOST_SECRET: hostSecret,
    FRAGMENT_INTERNAL_URL: loopbackUrl,
    PIERRE_PRIVATE_KEY: privPem,
    CODESTORAGE_ORG_NAME: "fragment-dev",
    CODESTORAGE_API_URL: null, // installed below once the mock is up
    FRAGMENT: {
      getByName: (name) => {
        if (!cells.has(name)) {
          // the fake state doubles as its own .storage (sql + alarms + ws api)
          const cell = new FragmentCell(makeStorage(), env);
          cells.set(name, makeCellFacade(cell));
        }
        return cells.get(name);
      },
    },
    WORKFLOWS: null,
    NOTIFY: {
      sent: [],
      async send(message) { env.NOTIFY.sent.push(message); },
    },
    LOADER: {
      // file-backed loader: materialize the spec the runtime hands us and
      // import main.mjs — the real WORKFLOW_MAIN/APP_MAIN entrypoints run
      // unmodified against Node's fetch/Request/Response
      async get(id, create) {
        const spec = await create();
        const dir = mkdtempSync(join(tmpdir(), `wfload-${wid}-`));
        try {
          mkdirSync(join(dir, "workflows"), { recursive: true });
          mkdirSync(join(dir, "lib"), { recursive: true });
          for (const [name, src] of Object.entries(spec.modules || {})) {
            if (!/^[A-Za-z0-9._/-]+$/.test(name)) continue; // skip platform-injected odd names (fragment:ai)
            const p = join(dir, name);
            mkdirSync(join(p, ".."), { recursive: true });
            writeFileSync(p, typeof src === "string" ? src : src.js);
          }
          // spec.mainModule NAMES the entry (e.g. "main.mjs"); its source
          // lives in spec.modules under that name
          const mainName = spec.mainModule || "main.mjs";
          const mainSrc = spec.modules[mainName];
          writeFileSync(join(dir, mainName), typeof mainSrc === "string" ? mainSrc : mainSrc.js);
          const mod = await import(pathToFileURL(join(dir, mainName)).href);
          return {
            fetch: async (url, init) => mod.default.fetch(new Request(url, init), spec.env),
          };
        } finally {
          // best-effort cleanup; Node keeps the module cached, so files
          // must outlive the load — remove on process exit instead
          // (rmSync below would break cached re-imports)
          setTimeout(() => { try { rmSync(dir, { recursive: true, force: true }); } catch {} }, 60_000).unref?.();
        }
      },
    },
  };

  // ---- the fake native Workflows binding ----
  const wfInstances = new Map();
  const makeFakeStep = (store, hooks = {}) => ({
    async do(name, fn) {
      const hit = store.steps.get(name);
      if (hit && hit.done) return hit.value; // replay: memoized, no re-execution
      hooks.beforeStep?.(name);
      const value = await fn();
      if (hooks.crashAfter === name) {
        // side effect landed; result NOT recorded — the exact window where
        // step memoization cannot save a network side effect
        throw new Error(`simulated crash after step '${name}' side effect`);
      }
      store.steps.set(name, { done: true, value });
      return value;
    },
    async sleep(_name, _dur) { /* test time is virtual */ },
  });

  const { runNativeAttempt } = await import("../src/wf-engine.js");
  env.WORKFLOWS = {
    instances: wfInstances,
    async create({ id, params }) {
      const inst = {
        id, params,
        store: { steps: new Map() },
        hooks: {},
        statusValue: { status: "queued" },
        async run() {
          try {
            inst.statusValue = { status: "running" };
            const out = await runNativeAttempt(params, makeFakeStep(inst.store, inst.hooks), env);
            inst.statusValue = { status: "complete", output: out };
            return out;
          } catch (e) {
            inst.statusValue = { status: "errored", error: String(e) };
            throw e;
          }
        },
        async status() { return inst.statusValue; },
      };
      wfInstances.set(id, inst);
      return inst;
    },
    async get(id) {
      const inst = wfInstances.get(id);
      if (!inst) throw new Error("no such instance");
      return { status: async () => inst.statusValue };
    },
  };

  function makeCellFacade(cell) {
    // the DO binding forwards straight to the DO's own fetch dispatcher —
    // no narrowing, exactly like celld. The facade keeps the cell's own
    // fields/methods (sql, getMeta, executeWorkflow, ...) so tests can
    // drive both planes through one handle.
    const facade = Object.assign(Object.create(Object.getPrototypeOf(cell)), cell);
    facade._facadeCell = cell;
    facade.fetch = async (input, init) => {
      const req = input instanceof Request ? input : new Request(input, init);
      return await cell.fetch(req);
    };
    return facade;
  }

  // ---- the mock code.storage (child process on an ephemeral port) ----
  const mock = await startMock({ orgPub: pubPem });
  env.CODESTORAGE_API_URL = mock.url;

  const world = {
    env, cells, mock, mockUrl: mock.url, loopbackUrl, loopback, hostSecret,
    async makeFragment(name, ownerHex = "aa".repeat(32), secretHex) {
      const facade = env.FRAGMENT.getByName(name);
      const reg = env.FRAGMENT.getByName("_registry");
      await reg.sql.exec("INSERT OR REPLACE INTO fragments (name, owner, created_at) VALUES (?, ?, ?)", name, ownerHex, Date.now());
      await reg.sql.exec("INSERT OR REPLACE INTO roles (name, pubkey, role) VALUES (?, ?, 'owner')", name, ownerHex);
      const resp = await facade.fetch("http://x/__cell/init", {
        method: "POST",
        body: JSON.stringify({ name, ownerHex, fragmentSecret: secretHex || "ab".repeat(32) }),
        headers: { "content-type": "application/json" },
      });
      return { cell: facade, resp: await resp.json(), facade };
    },
    cell(name) { return cells.get(name); },
    // simulate a cell eviction + wake: a NEW FragmentCell over the SAME
    // sqlite database (module caches — LRU — legitimately reset)
    restartCell(name) {
      const old = cells.get(name);
      const db = old ? old.sql._db : undefined;
      const cell = new FragmentCell(makeStorage(db), env);
      const facade = makeCellFacade(cell);
      cells.set(name, facade);
      return facade;
    },
    async stop() {
      await mock.stop();
      loopback.close();
    },
  };
  return world;
}

async function startMock({ orgPub }) {
  const { spawn } = await import("node:child_process");
  const stateDir = mkdtempSync(join(tmpdir(), "cs-mock-"));
  const child = spawn(process.execPath, [new URL("./mock-codestorage.mjs", import.meta.url).pathname, "0", stateDir], {
    env: { ...process.env, MOCK_ORG_PUB: orgPub, MOCK_ORG_NAME: "fragment-dev" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let url = null;
  const stderr = [];
  child.stderr.on("data", (d) => stderr.push(String(d)));
  await new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error(`mock did not start: ${stderr.join("")}`)), 10_000);
    child.stdout.on("data", (d) => {
      const m = String(d).match(/listening on (http:\/\/\S+)/);
      if (m) { url = m[1]; clearTimeout(t); resolve(); }
    });
  });
  return {
    url, child,
    async stop() { child.kill("SIGTERM"); },
    async state() { return (await fetch(`${url}/__test/state`)).json(); },
    async commitCount(repo) { return (await fetch(`${url}/__test/commit-count?repo=${encodeURIComponent(repo)}`)).json(); },
    async externalCommit(repo, changes, branch = "main", message = "external") {
      const r = await fetch(`${url}/__test/external-commit`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ repo, branch, message, changes }),
      });
      return await r.json();
    },
    // arm a racing writer: lands on the NEXT commit-pack before CAS
    async armRace(repo, changes, message = "racing writer") {
      await fetch(`${url}/__test/race`, {
        method: "POST", headers: { "content-type": "application/json" },
        body: JSON.stringify({ repo, changes, message }),
      });
    },
  };
}

// helper: encode an upsert for the mock's external-commit lever
export function upsert(path, text) {
  return { op: "upsert", path, bytes: Buffer.from(text, "utf8").toString("base64") };
}
export function del(path) {
  return { op: "delete", path };
}

// helper: build a signed webhook delivery (their documented scheme:
// HMAC-SHA256(secret, `${t}.${body}`) in X-Pierre-Signature)
export async function signedWebhook(secret, event, payload, atSec = Math.floor(Date.now() / 1000)) {
  const { createHmac } = await import("node:crypto");
  const body = JSON.stringify(payload);
  const mac = createHmac("sha256", secret).update(`${atSec}.${body}`).digest("hex");
  return { body, headers: { "content-type": "application/json", "x-pierre-event": event, "x-pierre-signature": `t=${atSec},sha256=${mac}` } };
}

export function pushPayload(repoUrl, ref, before, after, pushedAt = new Date().toISOString()) {
  return { repository: { id: "repo_x", url: repoUrl }, ref, before, after, pushed_at: pushedAt };
}

// deliver a webhook to the cell's /api/webhook route (as code.storage would)
export async function deliverWebhook(world, fragment, secret, event, payload, atSec) {
  const { body, headers } = await signedWebhook(secret, event, payload, atSec);
  const resp = await world.env.FRAGMENT.getByName(fragment).fetch(`http://x/api/webhook`, {
    method: "POST", headers, body,
  });
  return { status: resp.status, body: await resp.json() };
}
