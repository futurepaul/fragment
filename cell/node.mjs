// A computer's container on a sandcastle node (docs/self-host.md, seam 2).
// `NodeContainer` is Cloudflare's container API, `ctx.container`, as far as
// `ContainerHost` (entry.mjs) and the Sandbox SDK's `DirectoryBackup` call
// it, over the node's API (sandcastle's docs/node.md): so neither, nor
// computer.rs, nor the image, knows where its container runs. Every call is
// signed with the node's secret. The node hands each intercepted request to
// the platform's `/api/nodes/egress`, signed; the router passes it to the
// computer's object (`routeNodeEgress`), which checks it again and gives it
// to the binding its container set (`NodeContainer.binding`). A node the
// platform cannot reach dials it instead (`"uplink": true`): its calls then
// go through the node's `Node` object (uplink.mjs), signed the same.
//
// The deployment's nodes are `FRAGMENT_NODES` (fragment_core::placement,
// which checks it and says the rule a computer is placed by; computer.rs
// places it). A node that does not answer is `NodeDown`, which the cell
// answers as `node_down`, typed, within a bound: a call never hangs on it.

import * as rs from "./build/index.js";

const AUTH = "x-sandcastle-auth";
// a signature's timestamp, at most this far from our clock
const WINDOW_S = 60;
const SECRET_BYTES_MIN = 32;
// an exec frame's payload, at most (the engine's)
const FRAME_PAYLOAD_MAX = 1 << 20;
// an intercepted request's body, at most (the node's)
const EGRESS_BODY_MAX = 32 << 20;
// a node's health answers within this, or it is down: longer than the
// `Node` object waits for an uplink that is dialing again (uplink.mjs)
const HEALTH_MS = 6000;
// any other call but a start, a `wait`, an exec's stream and a port's
const CALL_MS = 30_000;
// a start: the engine boots the container before it answers
const START_MS = 120_000;
// an exec's socket, closed from this end as its process exits, has closed
// within this, or its exit is answered all the same
const CLOSE_WAIT_MS = 2000;
const enc = new TextEncoder();

// A sandcastle node that did not answer, or answered that it is unreachable:
// the cell's `node_down` (js.rs reads the name).
export class NodeDown extends Error {
  constructor(node, why) {
    super(`its node ${node} does not answer: ${why}`);
    this.name = "NodeDown";
    this.node = node;
  }
}

// `promise`, or NodeDown for `node` after `ms`.
function within(node, ms, promise) {
  let timer;
  const late = new Promise((_, reject) => {
    timer = setTimeout(() => reject(new NodeDown(node, `no answer within ${ms / 1000} s`)), ms);
  });
  return Promise.race([promise, late]).finally(() => clearTimeout(timer));
}

const hex = (buf) => [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
const sha256Hex = async (bytes) => hex(await crypto.subtle.digest("SHA-256", bytes));

function unhex(s) {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(2 * i, 2 * i + 2), 16);
  return out;
}

const callString = (method, path, t, body) => `sandcastle-node-v1\n${method}\n${path}\n${t}\n${body}`;
const egressString = (e, t, body) =>
  `sandcastle-egress-v1\n${e.method}\n${e.container}\n${e.intercept}\n${e.scheme}\n${e.host}\n${e.path}\n${t}\n${body}`;

// This isolate's reading of FRAGMENT_NODES, by its text.
let read = { text: undefined, nodes: null };

// The deployment's nodes, or null where computers run in the runtime's own
// containers: `byId`, each node with its secret's key (the Worker secret
// `secret` names) and its transport, and `images`, every image name a
// computer may be pinned to. Rust checks the variable (`rs.NodesConfig`).
export function nodesOf(env) {
  const text = env.FRAGMENT_NODES || "";
  if (read.text === text) return read.nodes;
  const config = rs.NodesConfig.read(env);
  let nodes = null;
  if (config) {
    const byId = new Map();
    for (const n of config.nodes) byId.set(n.id, nodeOf(env, n));
    nodes = { byId, images: config.images };
  }
  read = { text, nodes };
  return nodes;
}

// One node: its id, reach, architecture, capacity and images (name to its
// reference for that architecture), its secret's key, and `fetch(path,
// init)`, its transport: its URL, or the object that holds its uplink.
function nodeOf(env, n) {
  const secret = (env[n.secret] || "").trim();
  if (enc.encode(secret).length < SECRET_BYTES_MIN) throw new Error(`${n.secret}, the node ${n.id}'s secret, is at least ${SECRET_BYTES_MIN} bytes`);
  const key = crypto.subtle.importKey("raw", enc.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
  const fetcher = n.uplink
    ? (path, init) => nodeObject(env, n.id).fetch(new Request(`https://${n.id}.node.internal${path}`, init))
    : (path, init) => fetch(n.url + path, init);
  return { id: n.id, url: n.url, uplink: n.uplink, arch: n.arch, capacity: n.capacity, images: n.images, key, fetch: fetcher };
}

// A node that FRAGMENT_NODES no longer lists, for a computer placed on it:
// every call is NodeDown.
export function goneNode(id) {
  const gone = () => Promise.reject(new NodeDown(id, "it is no longer in FRAGMENT_NODES"));
  return { id, url: null, uplink: false, arch: null, capacity: 0, images: {}, key: null, fetch: gone };
}

// Node `id`'s object (uplink.mjs): it holds the node's uplink, when it
// dials in, and counts the computers placed on it.
export function nodeObject(env, id) {
  return env.NODE.get(env.NODE.idFromName(id));
}

// What placing a computer needs of each node (computer.rs `place`): whether
// it answers its health within HEALTH_MS, the architecture it reports, and
// the computers its object counts. One object each, as
// `fragment_core::placement::Probe` reads it.
export function probe(env, nodes) {
  return Promise.all(
    [...nodes.byId.values()].map(async (node) => {
      const placed = (await (await nodeObject(env, node.id).fetch(`https://${node.id}.node.internal/__node/placed`)).json()).placed;
      try {
        const health = await within(node.id, HEALTH_MS, signedFetch(node, "GET", "/v1/health"));
        if (!health.ok) {
          const e = await health.json().catch(() => ({}));
          return { id: node.id, down: `its health answered ${health.status} ${e.error || ""}`.trim(), placed };
        }
        const h = await health.json().catch(() => ({}));
        return { id: node.id, arch: h?.node?.arch ?? null, placed };
      } catch (e) {
        return { id: node.id, down: String((e && e.message) || e), placed };
      }
    }),
  );
}

// Node `id` takes `computer` if its object has room (or holds it already).
export async function take(env, node, computer) {
  const r = await nodeObject(env, node.id).fetch(`https://${node.id}.node.internal/__node/take`, {
    method: "POST",
    body: JSON.stringify({ computer, capacity: node.capacity }),
  });
  if (!r.ok) throw new Error(`the node ${node.id}'s object: ${r.status} ${await r.text()}`);
  return (await r.json()).taken === true;
}

// The computers placed on one node (computer.rs `place`), counted by the
// node's object (entry.mjs's `Node`): a computer is taken once, and a node
// at its capacity takes no new one. The count never falls: computers are
// neither deleted nor moved (fragment_core::placement).
const PLACED = "placed";
export class Placements {
  #ctx;

  constructor(ctx) {
    this.#ctx = ctx;
  }

  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/__node/placed") return Response.json({ placed: (await this.#ctx.storage.get(PLACED)) ?? 0 });
    if (path !== "/__node/take" || request.method !== "POST") return Response.json({ error: "no such route" }, { status: 404 });
    const { computer, capacity } = await request.json().catch(() => ({}));
    if (!/^computer:[0-9a-f]{24}$/.test(computer || "") || !Number.isInteger(capacity) || capacity < 1) {
      return Response.json({ error: "a take names a computer and the node's capacity" }, { status: 400 });
    }
    // from here only storage is awaited: no other request runs between the
    // count's read and its write (the object's input gate)
    const placed = (await this.#ctx.storage.get(PLACED)) ?? 0;
    if (await this.#ctx.storage.get(`${PLACED}/${computer}`)) return Response.json({ taken: true, placed });
    if (placed >= capacity) return Response.json({ taken: false, placed });
    await this.#ctx.storage.put({ [`${PLACED}/${computer}`]: true, [PLACED]: placed + 1 });
    return Response.json({ taken: true, placed: placed + 1 });
  }
}

// A signed call to `node` with no body, its transport's failure NodeDown.
async function signedFetch(node, method, path) {
  const headers = { [AUTH]: await signed(node, method, path, await sha256Hex(new Uint8Array())) };
  try {
    return await node.fetch(path, { method, headers });
  } catch (e) {
    throw e instanceof NodeDown ? e : new NodeDown(node.id, String((e && e.message) || e));
  }
}

// `canonical`'s signature at `t`, as the header carries it.
export async function authHeader(node, t, canonical) {
  return `t=${t},sig=${hex(await crypto.subtle.sign("HMAC", await node.key, enc.encode(canonical)))}`;
}

function signed(node, method, path, bodyHash) {
  const t = Math.floor(Date.now() / 1000);
  return authHeader(node, t, callString(method, path, t, bodyHash));
}

// The time `header` was signed at, when it is inside the window and
// matches the string `canonicalOf(t)` makes; else null.
export async function signedAt(node, header, canonicalOf) {
  const m = /^t=(\d{1,12}),sig=([0-9a-f]{64})$/.exec(header || "");
  if (!m) return null;
  const t = Number(m[1]);
  if (Math.abs(Math.floor(Date.now() / 1000) - t) > WINDOW_S) return null;
  return (await crypto.subtle.verify("HMAC", await node.key, unhex(m[2]), enc.encode(canonicalOf(t)))) ? t : null;
}

async function verified(node, header, canonicalOf) {
  return (await signedAt(node, header, canonicalOf)) !== null;
}

// One exec frame: the engine's 8-byte header (a stream byte, three zeroes,
// a big-endian length) and its payload.
function frame(stream, payload) {
  const out = new Uint8Array(8 + payload.length);
  out[0] = stream;
  new DataView(out.buffer).setUint32(4, payload.length);
  out.set(payload, 8);
  return out;
}

// A process exec started on the node, as Cloudflare's: stdin a
// WritableStream, stdout and stderr ReadableStreams (when piped), its exit
// code a promise, `kill`, and `output()`.
class NodeProcess {
  stdin = null;
  stdout = null;
  stderr = null;
  pid = null;
  exitCode;
  #ws;
  #gone;

  constructor(ws, spec, opts) {
    this.#ws = ws;
    let out = null;
    let err = null;
    if (spec.stdout === "pipe") this.stdout = new ReadableStream({ start: (c) => { out = c; } });
    if (spec.stderr === "pipe") this.stderr = new ReadableStream({ start: (c) => { err = c; } });
    let settle;
    this.exitCode = new Promise((resolve, reject) => { settle = { resolve, reject }; });
    let done = false;
    // the socket is closed (or closing) from this end: nothing more is sent
    let gone = false;
    this.#gone = () => gone;
    const end = (e) => {
      if (done) return;
      done = true;
      for (const c of [out, err]) {
        try { e ? c?.error(e) : c?.close(); } catch {}
      }
    };
    // The process is over: its socket is closed from this end too, and its
    // exit is answered once the socket has closed (within CLOSE_WAIT_MS). A
    // socket still open, or still closing, as the request that made it
    // ends holds that request on celld (docs/self-host.md, found 13).
    let finished = null;
    const finish = (outcome) => {
      if (finished) return;
      finished = outcome;
      gone = true;
      try { ws.close(1000, "exited"); } catch {}
      setTimeout(answer, CLOSE_WAIT_MS);
    };
    const answer = () => {
      if (!finished || finished.answered) return;
      finished.answered = true;
      if (finished.error) settle.reject(finished.error);
      else settle.resolve(finished.code);
    };
    ws.addEventListener("message", (m) => {
      if (typeof m.data === "string") return;
      const b = new Uint8Array(m.data);
      const len = new DataView(b.buffer, b.byteOffset, b.byteLength).getUint32(4);
      const payload = b.slice(8, 8 + len);
      const json = () => JSON.parse(new TextDecoder().decode(payload));
      switch (b[0]) {
        case 1: out?.enqueue(payload); break;
        case 2: err?.enqueue(payload); break;
        case 4: this.pid = json().pid; break;
        case 3: {
          const x = json();
          end(null);
          finish({ code: x.code ?? 128 + (x.signal ?? 0) });
          break;
        }
        case 5: {
          const e = new Error(`exec: ${json().error}`);
          end(e);
          finish({ error: e });
          break;
        }
      }
    });
    const closed = () => {
      gone = true;
      if (finished) return answer();
      const e = new Error("exec: the node's socket closed before the process exited");
      end(e);
      finished = { error: e };
      answer();
    };
    ws.addEventListener("close", closed);
    ws.addEventListener("error", closed);
    if (spec.stdin) {
      const write = (chunk) => {
        if (gone) throw new Error("exec: the process has exited");
        const bytes = typeof chunk === "string" ? enc.encode(chunk) : new Uint8Array(chunk);
        for (let at = 0; at < bytes.length; at += FRAME_PAYLOAD_MAX) ws.send(frame(0, bytes.subarray(at, at + FRAME_PAYLOAD_MAX)));
      };
      const eof = () => {
        if (gone) return;
        try { ws.send(frame(0, new Uint8Array())); } catch {}
      };
      if (opts.stdin === "pipe") {
        this.stdin = new WritableStream({ write, close: eof, abort: eof });
      } else if (typeof opts.stdin === "string") {
        write(opts.stdin);
        eof();
      } else {
        (async () => {
          for await (const chunk of opts.stdin) write(chunk);
          eof();
        })().catch(eof);
      }
    }
    if (opts.signal) {
      const abort = () => this.kill(9);
      if (opts.signal.aborted) abort();
      else opts.signal.addEventListener("abort", abort, { once: true });
    }
  }

  kill(signal = 15) {
    if (this.#gone()) return;
    try { this.#ws.send(frame(7, enc.encode(JSON.stringify({ signal })))); } catch {}
  }

  async output() {
    const all = async (s) => (s ? new Uint8Array(await new Response(s).arrayBuffer()) : new Uint8Array());
    const [stdout, stderr] = await Promise.all([all(this.stdout), all(this.stderr)]);
    return { exitCode: await this.exitCode, stdout, stderr };
  }
}

export class NodeContainer {
  #node;
  #name;
  #running = false;
  // the start in flight: every later call waits for it
  #starting = null;
  // the intercepts this start set, in order: the node names one by its index
  #intercepts = [];

  constructor(node, name) {
    this.#node = node;
    this.#name = name;
  }

  get running() {
    return this.#running;
  }

  get images() {
    return this.#node.images;
  }

  // The container's name on the node: its object's id.
  get name() {
    return this.#name;
  }

  // What the node says of this container: an isolate's first look, before
  // `running` is read (Computer's constructor waits for it, so it is
  // bounded as a health is).
  // A node that does not answer has nothing running for us, as far as this
  // isolate can tell: the object carries on (a wake then fails, and says why).
  async refresh() {
    try {
      const r = await this.#call("GET", `/v1/containers/${this.#name}`, undefined, [404], HEALTH_MS);
      this.#running = r.status === 404 ? false : (await r.json()).running === true;
    } catch (e) {
      console.log(JSON.stringify({ node: this.#node.id, container: this.#name, refresh: String((e && e.message) || e) }));
      this.#running = false;
    }
  }

  // The node it is on.
  get node() {
    return this.#node.id;
  }

  // The binding the container's intercept `index` names, if this isolate set it.
  binding(index) {
    return this.#intercepts[index]?.fetcher || null;
  }

  // A signed call, answered within `ms` (none: a `wait`, as long as the
  // container lives). A transport that fails, or a gateway's 502, 503 or
  // 504 (the uplink's object, or a proxy in front of the node), is
  // NodeDown; any other refusal is the node's own.
  async #call(method, path, body, allowed = [], ms = CALL_MS) {
    const node = this.#node.id;
    const bytes = body === undefined ? new Uint8Array() : enc.encode(JSON.stringify(body));
    const headers = { [AUTH]: await signed(this.#node, method, path, await sha256Hex(bytes)) };
    if (body !== undefined) headers["content-type"] = "application/json";
    const sent = this.#node.fetch(path, { method, headers, body: body === undefined ? undefined : bytes }).catch((e) => {
      throw e instanceof NodeDown ? e : new NodeDown(node, String((e && e.message) || e));
    });
    const resp = await (ms ? within(node, ms, sent) : sent);
    if (!resp.ok && !allowed.includes(resp.status)) {
      const e = await resp.json().catch(() => ({}));
      if ([502, 503, 504].includes(resp.status)) throw new NodeDown(node, `${method} ${path}: ${resp.status} ${e.error || ""}`.trim());
      throw new Error(`the node ${node}: ${method} ${path}: ${resp.status} ${e.error || ""}`.trim());
    }
    return resp;
  }

  async #json(method, path, body, ms = CALL_MS) {
    const text = await (await this.#call(method, path, body, [], ms)).text();
    return text ? JSON.parse(text) : null;
  }

  async #started() {
    if (this.#starting) await this.#starting;
  }

  // Cloudflare's start returns at once, the container running from then;
  // the node's start is in flight behind it, and `monitor` reports its failure.
  start(opts = {}) {
    const body = { enableInternet: opts.enableInternet === true, env: opts.env || {} };
    for (const k of ["image", "containerSnapshot", "entrypoint", "instance", "labels"]) if (opts[k] !== undefined) body[k] = opts[k];
    this.#running = true;
    this.#intercepts = [];
    // its health first: a node that is down says so within HEALTH_MS, where
    // a start's own bound is the boot's
    const path = `/v1/containers/${this.#name}/start`;
    this.#starting = this.#call("GET", "/v1/health", undefined, [], HEALTH_MS)
      .then(() => this.#json("POST", path, body, START_MS))
      .catch((e) => {
        this.#running = false;
        throw e;
      });
    this.#starting.catch(() => {});
  }

  // Resolves when the container exits cleanly or is destroyed; rejects on a
  // failed start, a nonzero exit, a signal, or a destroy with an error.
  async monitor() {
    try {
      await this.#started();
    } catch (e) {
      // a start that failed is the start's to report (ContainerHost's
      // `arm` throws it), never an exit of a container that ran
      throw Object.assign(new Error(String((e && e.message) || e)), { startFailed: true });
    }
    const exit = await this.#json("GET", `/v1/containers/${this.#name}/wait`, undefined, null);
    this.#running = false;
    if (exit.error) throw new Error(exit.error);
    if (exit.destroyed || exit.code === 0) return;
    throw new Error(exit.signal != null ? `the container was killed by signal ${exit.signal}` : `the container exited with code ${exit.code}`);
  }

  async destroy(error) {
    await this.#started().catch(() => {});
    await this.#json("POST", `/v1/containers/${this.#name}/destroy`, error ? { error: String(error) } : {});
    this.#running = false;
  }

  signal(signal) {
    this.#started()
      .then(() => this.#json("POST", `/v1/containers/${this.#name}/signal`, { signal }))
      .catch(() => {});
  }

  // The Computer object's own sleep stops an idle computer; a node keeps no
  // timer of its own (docs/self-host.md, seam 2).
  async setInactivityTimeout() {}

  async snapshotContainer(opts = {}) {
    await this.#started();
    const s = await this.#json("POST", `/v1/containers/${this.#name}/snapshots`, opts.name ? { name: opts.name } : {});
    return { id: s.id, size: s.size };
  }

  interceptOutboundHttp(host, fetcher) {
    return this.#intercept("http", host, fetcher);
  }

  interceptOutboundHttps(host, fetcher) {
    return this.#intercept("https", host, fetcher);
  }

  // Appended, never reordered while the container runs: the node names an
  // intercept by its index. Setting one again replaces its binding.
  async #intercept(scheme, target, fetcher) {
    await this.#started();
    const i = this.#intercepts.findIndex((x) => x.scheme === scheme && x.target === target);
    if (i >= 0) this.#intercepts[i].fetcher = fetcher;
    else this.#intercepts.push({ scheme, target, fetcher });
    const intercepts = this.#intercepts.map((x) => ({ scheme: x.scheme, target: x.target, action: { kind: "handler" } }));
    await this.#json("PUT", `/v1/containers/${this.#name}/intercepts`, { intercepts });
  }

  async exec(cmd, opts = {}) {
    await this.#started();
    const path = `/v1/containers/${this.#name}/exec`;
    const headers = { [AUTH]: await signed(this.#node, "GET", path, await sha256Hex(new Uint8Array())), upgrade: "websocket" };
    const node = this.#node.id;
    const sent = this.#node.fetch(path, { headers }).catch((e) => {
      throw e instanceof NodeDown ? e : new NodeDown(node, String((e && e.message) || e));
    });
    const resp = await within(node, CALL_MS, sent);
    const ws = resp.webSocket;
    if (!ws) throw new Error(`the node ${node}: exec: ${resp.status} ${await resp.text()}`);
    ws.accept();
    const stdin = opts.stdin !== undefined && opts.stdin !== "ignore";
    const spec = {
      cmd,
      env: opts.env || {},
      stdin,
      stdout: opts.stdout === "ignore" ? "ignore" : "pipe",
      stderr: opts.stderr === "combined" || opts.stderr === "ignore" ? opts.stderr : "pipe",
    };
    if (opts.cwd) spec.cwd = opts.cwd;
    if (opts.user) spec.user = opts.user;
    const process = new NodeProcess(ws, spec, opts);
    ws.send(JSON.stringify(spec));
    return process;
  }

  // A guest port, HTTP and WebSocket alike: its body streams, signed as
  // unsigned (docs/node.md, Auth); the URL asked for rides along so the
  // guest sees its own host.
  getTcpPort(port) {
    return {
      fetch: async (request) => {
        await this.#started();
        const u = new URL(request.url);
        const path = `/v1/containers/${this.#name}/ports/${port}${u.pathname}${u.search}`;
        const headers = new Headers(request.headers);
        headers.set(AUTH, await signed(this.#node, request.method, path, "UNSIGNED-PAYLOAD"));
        headers.set("x-sandcastle-url", request.url);
        return this.#node.fetch(path, { method: request.method, headers, body: request.body, redirect: "manual" });
      },
    };
  }
}

const EGRESS_HEADERS = ["x-sandcastle-container", "x-sandcastle-intercept", "x-sandcastle-host", "x-sandcastle-scheme", "x-sandcastle-path"];

// The router's half of `/api/nodes/egress`: the computer whose container
// the node names. The object checks the signature (`nodeEgress`); a name
// that is no object's id is refused here.
export function routeNodeEgress(request, env) {
  const container = request.headers.get("x-sandcastle-container") || "";
  if (!/^[0-9a-f]{64}$/.test(container)) return Response.json({ error: "no such computer" }, { status: 404 });
  const id = env.COMPUTER.idFromString(container);
  const inner = new Request("https://computer.internal/__node/egress", request);
  return env.COMPUTER.get(id).fetch(inner);
}

// The object's half: the intercepted request, checked against its node's
// signature (the node the computer is placed on), rebuilt as the guest
// sent it, and given to its binding. `rearm` sets the bindings again when
// this isolate has none (it started after the container did).
export async function nodeEgress(request, node, nodeContainer, rearm) {
  const h = request.headers;
  const [container, intercept, host, scheme, path] = EGRESS_HEADERS.map((k) => h.get(k) || "");
  const body = await request.arrayBuffer();
  if (!node || !node.key || !nodeContainer) return Response.json({ error: "this computer is not on a node" }, { status: 404 });
  if (container !== nodeContainer.name) return Response.json({ error: "another computer's intercept" }, { status: 403 });
  if (body.byteLength > EGRESS_BODY_MAX) return Response.json({ error: `a body of at most ${EGRESS_BODY_MAX} bytes` }, { status: 413 });
  const e = { method: request.method, container, intercept, scheme, host, path };
  const bodyHash = await sha256Hex(body);
  if (!(await verified(node, h.get(AUTH), (t) => egressString(e, t, bodyHash)))) return Response.json({ error: "a bad signature" }, { status: 401 });
  if (!/^\d{1,3}$/.test(intercept)) return Response.json({ error: "an intercept's index" }, { status: 400 });
  const index = Number(intercept);
  let fetcher = nodeContainer.binding(index);
  if (!fetcher) {
    await rearm();
    fetcher = nodeContainer.binding(index);
  }
  if (!fetcher) return Response.json({ error: `no binding for intercept ${intercept}` }, { status: 503 });
  const headers = new Headers(h);
  for (const k of [...EGRESS_HEADERS, AUTH, "host"]) headers.delete(k);
  const method = request.method;
  const hasBody = method !== "GET" && method !== "HEAD";
  // a redirect is the guest's to follow, as the runtime's own intercepts hand it back
  return fetcher.fetch(new Request(`${scheme}://${host}${path}`, { method, headers, body: hasBody ? body : undefined, redirect: "manual" }));
}
