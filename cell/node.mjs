// A computer's container on a sandcastle node (docs/self-host.md, seam 2).
// `NodeContainer` is Cloudflare's container API, `ctx.container`, as far as
// `ContainerHost` (entry.mjs) and the Sandbox SDK's `DirectoryBackup` call
// it, over the node's API (sandcastle's docs/node.md): so neither, nor
// computer.rs, nor the image, knows where its container runs. Every call is
// signed with the node's secret. The node hands each intercepted request to
// the platform's `/api/nodes/egress`, signed; the router passes it to the
// computer's object (`routeNodeEgress`), which checks it again and gives it
// to the binding its container set (`NodeContainer.binding`). A node the
// platform cannot reach dials it instead (`FRAGMENT_NODE_URL=uplink:<id>`):
// its calls then go through the node's `Node` object (uplink.mjs), signed
// the same.

const AUTH = "x-sandcastle-auth";
// a signature's timestamp, at most this far from our clock
const WINDOW_S = 60;
const SECRET_BYTES_MIN = 32;
// an exec frame's payload, at most (the engine's)
const FRAME_PAYLOAD_MAX = 1 << 20;
// an intercepted request's body, at most (the node's)
const EGRESS_BODY_MAX = 32 << 20;
const enc = new TextEncoder();

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

// The node this deployment places its computers on, or null: its URL, its
// secret (a Worker secret), and the images it holds, by the names
// computers are pinned to. `fetch(path, init)` is the transport: the
// node's URL, or, for `uplink:<id>`, the object that holds its uplink.
export function nodeOf(env) {
  const url = (env.FRAGMENT_NODE_URL || "").trim().replace(/\/+$/, "");
  if (!url) return null;
  const secret = (env.FRAGMENT_NODE_SECRET || "").trim();
  if (enc.encode(secret).length < SECRET_BYTES_MIN) throw new Error(`FRAGMENT_NODE_SECRET is at least ${SECRET_BYTES_MIN} bytes`);
  const images = JSON.parse(env.FRAGMENT_NODE_IMAGES || "{}");
  const key = crypto.subtle.importKey("raw", enc.encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign", "verify"]);
  const uplink = /^uplink:([a-z0-9-]{1,64})$/.exec(url)?.[1] || null;
  if (url.startsWith("uplink:") && !uplink) throw new Error("FRAGMENT_NODE_URL is uplink:<id>, the id 1 to 64 of a-z, 0-9 and -");
  const fetcher = uplink
    ? (path, init) => env.NODE.get(env.NODE.idFromName(uplink)).fetch(new Request(`https://node.internal${path}`, init))
    : (path, init) => fetch(url + path, init);
  return { url, images, key, uplink, fetch: fetcher };
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

  constructor(ws, spec, opts) {
    this.#ws = ws;
    let out = null;
    let err = null;
    if (spec.stdout === "pipe") this.stdout = new ReadableStream({ start: (c) => { out = c; } });
    if (spec.stderr === "pipe") this.stderr = new ReadableStream({ start: (c) => { err = c; } });
    let settle;
    this.exitCode = new Promise((resolve, reject) => { settle = { resolve, reject }; });
    let done = false;
    const end = (e) => {
      if (done) return;
      done = true;
      for (const c of [out, err]) {
        try { e ? c?.error(e) : c?.close(); } catch {}
      }
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
          settle.resolve(x.code ?? 128 + (x.signal ?? 0));
          break;
        }
        case 5: {
          const e = new Error(`exec: ${json().error}`);
          end(e);
          settle.reject(e);
          break;
        }
      }
    });
    const closed = () => {
      if (done) return;
      const e = new Error("exec: the node's socket closed before the process exited");
      end(e);
      settle.reject(e);
    };
    ws.addEventListener("close", closed);
    ws.addEventListener("error", closed);
    if (spec.stdin) {
      const write = (chunk) => {
        const bytes = typeof chunk === "string" ? enc.encode(chunk) : new Uint8Array(chunk);
        for (let at = 0; at < bytes.length; at += FRAME_PAYLOAD_MAX) ws.send(frame(0, bytes.subarray(at, at + FRAME_PAYLOAD_MAX)));
      };
      const eof = () => { try { ws.send(frame(0, new Uint8Array())); } catch {} };
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
  // `running` is read (Computer's constructor waits for it).
  // A node that does not answer has nothing running for us, as far as this
  // isolate can tell: the object carries on (a wake then fails, and says why).
  async refresh() {
    try {
      const r = await this.#call("GET", `/v1/containers/${this.#name}`, undefined, [404]);
      this.#running = r.status === 404 ? false : (await r.json()).running === true;
    } catch (e) {
      console.log(JSON.stringify({ node: this.#node.url, container: this.#name, refresh: String((e && e.message) || e) }));
      this.#running = false;
    }
  }

  // The binding the container's intercept `index` names, if this isolate set it.
  binding(index) {
    return this.#intercepts[index]?.fetcher || null;
  }

  async #call(method, path, body, allowed = []) {
    const bytes = body === undefined ? new Uint8Array() : enc.encode(JSON.stringify(body));
    const headers = { [AUTH]: await signed(this.#node, method, path, await sha256Hex(bytes)) };
    if (body !== undefined) headers["content-type"] = "application/json";
    const resp = await this.#node.fetch(path, { method, headers, body: body === undefined ? undefined : bytes });
    if (!resp.ok && !allowed.includes(resp.status)) {
      const e = await resp.json().catch(() => ({}));
      throw new Error(`the node: ${method} ${path}: ${resp.status} ${e.error || ""}`.trim());
    }
    return resp;
  }

  async #json(method, path, body) {
    const text = await (await this.#call(method, path, body)).text();
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
    this.#starting = this.#json("POST", `/v1/containers/${this.#name}/start`, body).catch((e) => {
      this.#running = false;
      throw e;
    });
    this.#starting.catch(() => {});
  }

  // Resolves when the container exits cleanly or is destroyed; rejects on a
  // failed start, a nonzero exit, a signal, or a destroy with an error.
  async monitor() {
    await this.#started();
    const exit = await this.#json("GET", `/v1/containers/${this.#name}/wait`);
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
    const resp = await this.#node.fetch(path, { headers });
    const ws = resp.webSocket;
    if (!ws) throw new Error(`the node: exec: ${resp.status} ${await resp.text()}`);
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

// The object's half: the intercepted request, checked against the node's
// signature, rebuilt as the guest sent it, and given to its binding.
// `rearm` sets the bindings again when this isolate has none (it started
// after the container did).
export async function nodeEgress(request, env, nodeContainer, rearm) {
  const node = nodeOf(env);
  const h = request.headers;
  const [container, intercept, host, scheme, path] = EGRESS_HEADERS.map((k) => h.get(k) || "");
  const body = await request.arrayBuffer();
  if (!node || !nodeContainer) return Response.json({ error: "this computer is not on a node" }, { status: 404 });
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
  return fetcher.fetch(new Request(`${scheme}://${host}${path}`, { method, headers, body: hasBody ? body : undefined }));
}
