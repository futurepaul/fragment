// The platform's hand-written JavaScript (besides platform.mjs, which runs
// inside the app facet, and storage.mjs, a computer's S3 endpoint).
// Everything else is Rust. The runtime gives RPC only to a class that
// extends DurableObject, and workers-rs classes do not, so these classes do
// and forward each handler. workers-rs 0.8.5 has no Workflows and no
// Containers, so the job driver and a computer's container calls are here
// too; they only call and call back: every decision is Rust's (jobs.rs,
// computer.rs).
import { DurableObject, WorkerEntrypoint, WorkflowEntrypoint } from "cloudflare:workers";
import { DirectoryBackup } from "@cloudflare/sandbox";
import * as rs from "./build/index.js";
import { handleS3 } from "./storage.mjs";
import { NodeContainer, nodeEgress, nodeOf, routeNodeEgress } from "./node.mjs";

// The last `arm` of a computer on a node, its isolate's to repeat.
const NODE_ARM = "node/arm";

export { DirectoryBackupGateway } from "@cloudflare/sandbox";

// The router is Rust's, but for one route: a sandcastle node's intercepts
// (node.mjs), on the platform's own host, which go to their computer's
// object as they came.
function routed(request, env, rust) {
  const url = new URL(request.url);
  const platform = env.FRAGMENT_PLATFORM_URL ? new URL(env.FRAGMENT_PLATFORM_URL).host : url.host;
  if (url.pathname === "/api/nodes/egress" && url.host === platform && nodeOf(env)) return routeNodeEgress(request, env);
  return rust();
}

const Rust = rs.default;
export default typeof Rust === "function"
  ? class extends Rust {
      fetch(request) {
        return routed(request, this.env, () => super.fetch(request));
      }
    }
  : { ...Rust, fetch: (request, env, ctx) => routed(request, env, () => Rust.fetch(request, env, ctx)) };

export class Fragment extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.FragmentCell(ctx, env);
  }
  fetch(request) { return this.rs.fetch(request); }
  alarm(info) { return this.rs.alarm(info); }
  webSocketMessage(ws, message) { return this.rs.webSocketMessage(ws, message); }
  webSocketClose(ws, code, reason, clean) { return this.rs.webSocketClose(ws, code, reason, clean); }
  webSocketError(ws, error) { return this.rs.webSocketError(ws, error); }
}

export class Principal extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.PrincipalCell(ctx, env);
  }
  fetch(request) { return this.rs.fetch(request); }
}

export class Ledger extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.LedgerCell(ctx, env);
  }
  fetch(request) { return this.rs.fetch(request); }
  alarm(info) { return this.rs.alarm(info); }
}

export class Registry extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.RegistryCell(ctx, env);
  }
  fetch(request) { return this.rs.fetch(request); }
  alarm(info) { return this.rs.alarm(info); }
}

// The app's read access to its files (files.rs), handed to its facet as
// `env.FILES` bound to one fragment by `props`: the app cannot name another.
// Both it and the job driver call the supervisor's internal routes through
// `rs.InternalRoute.request` (routed.rs), which marks the request as that route
// expects: the JavaScript names no header.
export class Files extends WorkerEntrypoint {
  async #ask(op, body) {
    const { fragment } = this.ctx.props;
    return this.env.FRAGMENT.getByName(fragment).fetch(rs.InternalRoute.request(`cap/files/${op}`, JSON.stringify(body)));
  }

  async #answer(op, body) {
    const resp = await this.#ask(op, body);
    if (resp.status === 404 && op === "read") return null;
    if (!resp.ok) {
      const e = await resp.json().catch(() => ({}));
      throw new Error(e.message || `files.${op} answered ${resp.status}`);
    }
    return op === "read" ? new Uint8Array(await resp.arrayBuffer()) : resp.json();
  }

  read(path) { return this.#answer("read", { path: String(path) }); }
  list(prefix = "") { return this.#answer("list", { prefix: String(prefix) }); }
  stat(path) { return this.#answer("stat", { path: String(path) }); }
}

// One run of an operation (docs/MODEL.md, Operations: job). Each round asks
// the supervisor to advance the job to its next step, then performs that
// step through the supervisor, which keeps the step's answer before it
// replies (jobs.rs, `steps`): an advance names only how many steps are
// done, and a step tried again because its reply was lost is answered from
// what was kept, not performed again. Both calls are durable steps here, so
// a replay after a crash re-sends nothing that already happened. A step
// that fails for now (the supervisor answers 5xx) is retried with backoff;
// when its retries run out, the next advance carries its error, and the job
// sees it and may catch it.
export class Job extends WorkflowEntrypoint {
  async run(event, step) {
    const { fragment, incarnation, run, attempt } = event.payload;
    const delay = Math.max(1, Number(this.env.FRAGMENT_JOB_RETRY_DELAY_S) || 10);
    const retrying = { retries: { limit: 4, delay: `${delay} seconds`, backoff: "exponential" }, timeout: "5 minutes" };
    const post = async (path, body) => {
      const call = rs.InternalRoute.request(`job/${path}`, JSON.stringify({ incarnation, run, attempt, ...body }));
      const resp = await this.env.FRAGMENT.getByName(fragment).fetch(call);
      const out = await resp.json().catch(() => ({}));
      if (!resp.ok) throw new Error(out.message || `job/${path} answered ${resp.status}`);
      return out;
    };
    // the step before this round, when it ran out of retries
    let failed = null;
    try {
      for (let i = 0; ; i++) {
        const next = await step.do(`advance ${i}`, retrying, () => post("advance", { count: i, failed }));
        if (!next.step) return next; // done, failed, or stop: the supervisor has recorded it
        failed = null;
        const { kind, args } = next.step;
        if (kind === "sleep") {
          // the supervisor kept the sleep's answer as it handed the step out
          await step.sleep(`sleep ${i}`, args.ms);
          continue;
        }
        try {
          const done = await step.do(`${kind} ${i}`, retrying, () => post("effect", { index: i, kind, args }));
          if (done.stop) return done;
        } catch (e) {
          failed = { index: i, kind, error: String((e && e.message) || e) };
        }
      }
    } catch (e) {
      const error = String((e && e.message) || e);
      await step.do("finish", retrying, () => post("finish", { error }));
      return { failed: error };
    }
  }
}

// A computer's container (docs/computers.md), for the Rust `ComputerCell`
// (computer.rs), which reaches it as `ctx.computerHost`. Each method is one
// runtime call and its plumbing. Every call that touches the container
// names the start it is for (its generation): a late call for an earlier
// start (a sleep that finishes after a wake started another) touches
// nothing. The container is the runtime's (`ctx.container`), or, when the
// deployment places computers on a sandcastle node, the node's
// (`NodeContainer`, node.mjs: the same API).
class ContainerHost {
  #ctx;
  #env;
  #report;
  #generation = 0;
  #backups;
  #node;

  constructor(ctx, env, report) {
    this.#ctx = ctx;
    this.#env = env;
    this.#report = report;
    const node = nodeOf(env);
    this.#node = node ? new NodeContainer(node, ctx.id.toString()) : null;
    this.#backups = new DirectoryBackup(this.#c, ctx.exports.DirectoryBackupGateway, {
      binding: "BLOBS",
      prefix: `computers/${ctx.id}/backups/`,
    });
  }

  get #c() {
    return this.#node || this.#ctx.container;
  }

  // A new isolate's first look at a node's container (`running` is the
  // node's to say), before any request.
  refresh() {
    return this.#node ? this.#node.refresh() : Promise.resolve();
  }

  // An intercepted request from the node (node.mjs). An isolate that
  // started after its container has no bindings: it sets them again from
  // the last `arm`, kept for this.
  nodeEgress(request) {
    const rearm = async () => {
      const a = await this.#ctx.storage.get(NODE_ARM);
      if (a && this.adopt(a.generation)) await this.arm(a.generation, a.computer, a.idleMs, a.swapHosts);
    };
    return nodeEgress(request, this.#env, this.#node, rearm);
  }

  running() {
    return this.#c.running;
  }

  // The images the deployment declares (wrangler.jsonc `containers`).
  images() {
    return Object.keys(this.#c.images || {});
  }

  // What the name `image` stands for in this deployment (its reference: a
  // redeploy that changes the image keeps the name and changes this).
  imageRef(image) {
    const ref = this.#c.images && this.#c.images[image];
    return ref === undefined ? null : typeof ref === "string" ? ref : JSON.stringify(ref);
  }

  // A new isolate finds the container of start `generation` (lesson 6):
  // when it runs, this isolate takes it, watching its exit again. Answers
  // whether it runs.
  adopt(generation) {
    if (!this.#c.running) return false;
    this.#generation = generation;
    this.#c.monitor().then(
      () => this.#report("container/exited", { generation }),
      (e) => this.#report("container/exited", { generation, why: String((e && e.message) || e) }),
    );
    return true;
  }

  // Starts `image` (or a snapshot of it), then watches it: its exit, for
  // any reason, is reported as `container/exited` for this generation.
  start(generation, image, snapshot, env, instance) {
    this.#generation = generation;
    const opts = { env, enableInternet: true };
    if (instance) opts.instance = instance;
    if (snapshot) opts.containerSnapshot = { id: snapshot };
    else if (this.#c.images && this.#c.images[image]) opts.image = this.#c.images[image];
    this.#c.start(opts);
    this.#c.monitor().then(
      () => this.#report("container/exited", { generation }),
      (e) => this.#report("container/exited", { generation, why: String((e && e.message) || e) }),
    );
  }

  // A start from a snapshot answers "temporarily unavailable" for a while
  // (spike S3b): the calls right after it are tried again for up to two
  // minutes.
  async #settled(f) {
    const t0 = Date.now();
    for (;;) {
      try {
        return await f();
      } catch (e) {
        if (!/temporarily unavailable/.test(String(e)) || Date.now() - t0 > 120_000) throw e;
        await new Promise((r) => setTimeout(r, 100));
      }
    }
  }

  // The egress intercepts (one entry per host: `ComputerEgress` with the
  // route as props) and the runtime's idle stop, the safety net under the
  // Computer DO's own sleep. Neither survives a stop, so each start sets
  // them again. `swapHosts` are the connections' and operator keys' hosts,
  // caught on HTTPS and on plain HTTP alike: the swap always sends on over
  // HTTPS, so a credential never crosses the internet in the clear.
  async arm(generation, computer, idleMs, swapHosts) {
    if (generation !== this.#generation) return false;
    const c = this.#c;
    const egress = (route) => this.#ctx.exports.ComputerEgress({ props: { computer, route } });
    await this.#settled(() => c.interceptOutboundHttp("api.fragment.internal", egress("api")));
    await this.#settled(() => c.interceptOutboundHttp("model.fragment.internal", egress("model")));
    await this.#settled(() => c.interceptOutboundHttp("storage.fragment.internal", egress("storage")));
    for (const host of swapHosts) {
      await this.#settled(() => c.interceptOutboundHttp(host, egress("swap")));
      await this.#settled(() => c.interceptOutboundHttps(host, egress("swap")));
    }
    await this.#settled(() => this.#backups.intercept());
    await this.#settled(() => c.setInactivityTimeout(idleMs));
    if (this.#node) await this.#ctx.storage.put(NODE_ARM, { generation, computer, idleMs, swapHosts });
    return true;
  }

  // Waits until the container takes an exec (it is up), for at most `ms`.
  async execReady(generation, ms) {
    const t0 = Date.now();
    while (generation === this.#generation && Date.now() - t0 < ms) {
      try {
        const out = await (await this.#c.exec(["true"])).output();
        if (out.exitCode === 0) return true;
      } catch {}
      await new Promise((r) => setTimeout(r, 100));
    }
    return false;
  }

  async exec(generation, argv) {
    if (generation !== this.#generation) throw new Error(`start ${generation} is not the running one`);
    const out = await (await this.#c.exec(argv, { stderr: "combined" })).output();
    return { exitCode: out.exitCode, output: new TextDecoder().decode(out.stdout).slice(-4096) };
  }

  async backup(generation) {
    if (generation !== this.#generation) throw new Error(`start ${generation} is not the running one`);
    return this.#backups.backup({ dir: "/data", name: `generation ${generation}` });
  }

  async restore(generation, record) {
    if (generation !== this.#generation) throw new Error(`start ${generation} is not the running one`);
    await this.#backups.restore(record);
  }

  async forget(record) {
    await this.#backups.delete(record);
  }

  async snapshot(generation, name) {
    if (generation !== this.#generation) throw new Error(`start ${generation} is not the running one`);
    const s = await this.#c.snapshotContainer({ name });
    return s.id;
  }

  signal(generation, n) {
    if (generation === this.#generation && this.#c.running) this.#c.signal(n);
  }

  // Destroys the container if it is still this generation's, and waits for
  // it to be gone (lesson 6: never start before `running` is false).
  async destroy(generation, reason) {
    if (generation !== this.#generation) return false;
    if (this.#c.running) await this.#c.destroy(reason).catch(() => {});
    for (let i = 0; i < 100 && this.#c.running; i++) await new Promise((r) => setTimeout(r, 100));
    return !this.#c.running;
  }

  // Waits up to `ms` for the container to exit on its own (after a signal).
  async exited(ms) {
    const t0 = Date.now();
    while (this.#c.running && Date.now() - t0 < ms) await new Promise((r) => setTimeout(r, 100));
    return !this.#c.running;
  }

  // A request to one of the container's ports. A WebSocket is bridged
  // through this Durable Object (both ends accepted here, so an open tab
  // keeps the computer awake), its opening and closing reported as
  // `container/tab`.
  async port(port, request) {
    const resp = await this.#c.getTcpPort(port).fetch(request);
    const upstream = resp.webSocket;
    if (!upstream) return resp;
    const [client, server] = Object.values(new WebSocketPair());
    upstream.accept();
    server.accept();
    await this.#report("container/tab", { open: true });
    let closed = false;
    const close = (code, reason) => {
      if (closed) return;
      closed = true;
      for (const ws of [upstream, server]) {
        try {
          ws.close(code || 1000, reason || "");
        } catch {}
      }
      this.#report("container/tab", { open: false });
    };
    upstream.addEventListener("message", (e) => server.send(e.data));
    server.addEventListener("message", (e) => upstream.send(e.data));
    for (const ws of [upstream, server]) {
      ws.addEventListener("close", (e) => close(e.code, e.reason));
      ws.addEventListener("error", () => close(1011, "the other end failed"));
    }
    return new Response(null, { status: 101, webSocket: client });
  }
}

export class Computer extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    const report = (path, body) => this.rs.fetch(rs.InternalRoute.request(path, JSON.stringify(body)));
    const host = new ContainerHost(ctx, env, report);
    Object.defineProperty(ctx, "computerHost", { value: host });
    this.rs = new rs.ComputerCell(ctx, env);
    ctx.blockConcurrencyWhile(() => host.refresh());
  }
  fetch(request) {
    if (new URL(request.url).pathname === "/__node/egress") return this.ctx.computerHost.nodeEgress(request);
    return this.rs.fetch(request);
  }
  alarm(info) { return this.rs.alarm(info); }
  webSocketMessage(ws, message) { return this.rs.webSocketMessage(ws, message); }
  webSocketClose(ws, code, reason, clean) { return this.rs.webSocketClose(ws, code, reason, clean); }
  webSocketError(ws, error) { return this.rs.webSocketError(ws, error); }
}

// Every intercepted request a computer's guest makes (docs/computers.md):
// `props.route` is the host it asked for, and `props.computer` the
// computer, both set by the Computer DO, never by the guest. Storage is
// answered here; the rest is Rust's (`ComputerEgress.handle`).
export class ComputerEgress extends WorkerEntrypoint {
  fetch(request) {
    const { computer, route } = this.ctx.props;
    if (route === "storage") {
      return handleS3(request, this.env.BLOBS, `computers/${computer}/storage/`).then(([resp]) => resp);
    }
    return rs.ComputerEgress.handle(request, this.env, this.ctx, computer, route);
  }
}
