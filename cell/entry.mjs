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
import { NodeContainer, Placements, health, isPaired, nodeEgress, nodeFor, nodeStatus, nodesOf, probe, routeNodeEgress, take } from "./node.mjs";
import { Uplink, routeNodeUplink } from "./uplink.mjs";

// The last `arm` of a computer on a node, its isolate's to repeat.
const NODE_ARM = "node/arm";
// The meta row computer.rs keeps the node a computer is placed on in.
const NODE_META = "node";

export { DirectoryBackupGateway } from "@cloudflare/sandbox";

// A computer on nodes that has not been placed: nothing runs.
const UNPLACED = Object.freeze({ running: false, images: {} });

// The router is Rust's, but for two routes of a sandcastle node's, on the
// platform's own host: its intercepts (node.mjs), which go to their
// computer's object as they came, and its uplink (uplink.mjs), which goes
// to its `Node` object.
function routed(request, env, rust) {
  const url = new URL(request.url);
  const platform = env.FRAGMENT_PLATFORM_URL ? new URL(env.FRAGMENT_PLATFORM_URL).host : url.host;
  if (url.pathname === "/api/nodes/egress" && url.host === platform && nodesOf(env)) return routeNodeEgress(request, env);
  if (url.pathname === "/api/nodes/uplink" && url.host === platform && nodesOf(env)) return routeNodeUplink(request, env);
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
// deployment places computers on sandcastle nodes (FRAGMENT_NODES), the
// node's it is placed on (`NodeContainer`, node.mjs: the same API).
// Placing it is computer.rs's (`place`), through `probe`, `take` and `pin`.
class ContainerHost {
  #ctx;
  #env;
  #report;
  #generation = 0;
  #backups = null;
  // the deployment's nodes (null: the runtime's containers), and this
  // computer's container on the one it is placed on (null: not yet placed),
  // and that node (the deployment's, or a person's own: node.mjs `nodeFor`)
  #nodes;
  #node = null;
  #def = null;

  constructor(ctx, env, report) {
    this.#ctx = ctx;
    this.#env = env;
    this.#report = report;
    this.#nodes = nodesOf(env);
    if (!this.#nodes) this.#backups = this.#backupsOf(ctx.container);
  }

  #backupsOf(container) {
    return new DirectoryBackup(container, this.#ctx.exports.DirectoryBackupGateway, {
      binding: "BLOBS",
      prefix: `computers/${this.#ctx.id}/backups/`,
    });
  }

  // The container: the runtime's, or its node's. Before a computer on
  // nodes is placed, nothing runs and nothing may be started.
  get #c() {
    if (!this.#nodes) return this.#ctx.container;
    return this.#node || UNPLACED;
  }

  // A new isolate's first look, before any request: the node computer.rs
  // placed the computer on (its meta row; the schema is applied before
  // this runs), and what that node says of its container.
  // A person's own node is the registry's to name (node.mjs `pairedNode`):
  // when it does not answer, the computer is pinned at its next call instead.
  async refresh() {
    if (!this.#nodes) return;
    try {
      await this.#repin();
    } catch (e) {
      console.log(JSON.stringify({ computer: this.#ctx.id.toString(), pin: String((e && e.message) || e) }));
    }
    if (this.#node) await this.#node.refresh();
  }

  // Pins this isolate to the node computer.rs placed the computer on, if
  // it has not yet.
  async #repin() {
    if (!this.#nodes || this.#node) return;
    const row = [...this.#ctx.storage.sql.exec("SELECT value FROM meta WHERE key = ?", NODE_META)][0];
    if (row) await this.pin(row.value);
  }

  // Whether computers run on sandcastle nodes here.
  nodes() {
    return this.#nodes !== null;
  }

  // Each node as placing a computer needs it (node.mjs `probe`): the
  // deployment's, and `own`, the person's own node they chose ([{id}]).
  probe(own) {
    return probe(this.#env, this.#nodes, own || []);
  }

  // Whether node `id` takes `computer`, with room for `capacity` (its
  // object counts what it holds).
  take(id, computer, capacity) {
    return take(this.#env, id, computer, capacity);
  }

  // This computer runs on node `id`, which computer.rs recorded, from now
  // on: a node FRAGMENT_NODES no longer lists answers every call NodeDown,
  // and a person's own node they revoked, NodeRevoked.
  async pin(id) {
    if (this.#node) {
      if (this.#node.node !== id) throw new Error(`placed on ${this.#node.node}, never ${id}`);
      return;
    }
    const node = await nodeFor(this.#env, this.#nodes, id);
    if (this.#node) return;
    this.#def = node;
    this.#node = new NodeContainer(node, this.#ctx.id.toString());
    this.#backups = this.#backupsOf(this.#node);
  }

  // An intercepted request from the node (node.mjs). An isolate that
  // started after its container has no bindings: it sets them again from
  // the last `arm`, kept for this.
  // A person's own node they revoked hands nothing on: its object says so.
  async nodeEgress(request) {
    const rearm = async () => {
      const a = await this.#ctx.storage.get(NODE_ARM);
      const adopted = !!a && (await this.adopt(a.generation));
      if (adopted) await this.arm(a.generation, a.computer, a.idleMs, a.swapHosts);
      console.log(JSON.stringify({ nodeEgress: "rearm", generation: a?.generation ?? null, adopted }));
    };
    const node = this.#node ? this.#def : null;
    if (node?.own && (await nodeStatus(this.#env, node.id)).revoked) {
      console.log(JSON.stringify({ nodeEgress: "refused", node: node.id, status: 403, error: "its node was revoked" }));
      return Response.json({ error: "node_revoked", message: `the node ${node.id} was revoked by its owner` }, { status: 403 });
    }
    return nodeEgress(request, node, this.#node, rearm);
  }

  // Whether its container runs: a node's that could not say as this
  // isolate began (it was dialing again) is asked again first.
  async running() {
    await this.#repin();
    if (this.#node) await this.#node.known();
    return this.#c.running;
  }

  // The images a computer may be pinned to: the deployment's (wrangler.jsonc
  // `containers`), or, on nodes, FRAGMENT_NODES' that its node can run
  // (every one, before it is placed).
  images() {
    if (this.#nodes && !this.#node) return this.#nodes.images;
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
  async adopt(generation) {
    if (!(await this.running())) return false;
    this.#generation = generation;
    this.#c.monitor().then(
      () => this.#report("computer/exited", { generation }),
      (e) => this.#report("computer/exited", { generation, why: String((e && e.message) || e) }),
    );
    return true;
  }

  // Starts `image` (or a snapshot of it), then watches it: its exit, for
  // any reason, is reported as `computer/exited` for this generation.
  start(generation, image, snapshot, env, instance) {
    if (this.#nodes && !this.#node) throw new Error("a computer on nodes is placed before it starts (computer.rs `place`)");
    // a node it can no longer reach says why, not that it lacks the image
    if (this.#def?.error) throw this.#def.error();
    this.#generation = generation;
    const opts = { env, enableInternet: true };
    if (instance) opts.instance = instance;
    if (snapshot) opts.containerSnapshot = { id: snapshot };
    else if (this.#c.images && this.#c.images[image]) opts.image = this.#c.images[image];
    else if (this.#node) throw new Error(`its node ${this.#node.node} has no ${image} image for its architecture (FRAGMENT_NODES' images)`);
    this.#c.start(opts);
    this.#c.monitor().then(
      () => this.#report("computer/exited", { generation }),
      // a start that failed is reported as that, by the start (computer.rs)
      (e) => e?.startFailed || this.#report("computer/exited", { generation, why: String((e && e.message) || e) }),
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
  // `computer/tab`.
  async port(port, request) {
    if (this.#nodes && !this.#node) throw new Error("a computer that has never started has no ports");
    const resp = await this.#c.getTcpPort(port).fetch(request);
    const upstream = resp.webSocket;
    if (!upstream) return resp;
    const [client, server] = Object.values(new WebSocketPair());
    upstream.accept();
    server.accept();
    await this.#report("computer/tab", { open: true });
    let closed = false;
    const close = (code, reason) => {
      if (closed) return;
      closed = true;
      for (const ws of [upstream, server]) {
        try {
          ws.close(code || 1000, reason || "");
        } catch {}
      }
      this.#report("computer/tab", { open: false });
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
    // What befell its container (its exit, a tab's socket) comes back in as
    // a request of its own, through the object's namespace: never as a
    // continuation of the call that started the container, long answered by
    // then. A runtime may tie what such a continuation opens to that call:
    // celld closes its WebSockets at once, so the start an exit's report
    // made took no exec (docs/self-host.md, found 21).
    const self = env.COMPUTER.idFromString(ctx.id.toString());
    const report = (path, body) => env.COMPUTER.get(self).fetch(rs.InternalRoute.request(path, JSON.stringify(body)));
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

// A sandcastle node's object (node.mjs, uplink.mjs): one per node id. It
// counts the computers placed on the node, and, when the node dials in,
// holds its uplink, through which `NodeContainer` calls it. It says whether
// the node is up (`/__node/status`, for its owner's settings), and, a
// person's own node, that its owner revoked it (`/__node/revoke`): its
// uplink is cut, and every dial and call after is refused, typed.
const REVOKED = "revoked";
export class Node extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.uplink = new Uplink(ctx, env);
    this.placements = new Placements(ctx);
    this.revoked = false;
    ctx.blockConcurrencyWhile(async () => {
      this.revoked = (await ctx.storage.get(REVOKED)) === true;
    });
  }
  async fetch(request) {
    const path = new URL(request.url).pathname;
    if (path === "/__node/status" || path === "/__node/revoke") {
      const { id } = await request.json().catch(() => ({}));
      // this object is that node's alone
      if (!id || this.ctx.id.toString() !== this.env.NODE.idFromName(id).toString()) return Response.json({ error: "invalid_request", message: "name this node's id" }, { status: 400 });
      if (path === "/__node/revoke") {
        if (!isPaired(id)) return Response.json({ error: "forbidden", message: "only a person's own node is revoked" }, { status: 403 });
        this.revoked = true;
        await this.ctx.storage.put(REVOKED, true);
        this.uplink.revoke(`the node ${id} was revoked by its owner`);
        return Response.json({ revoked: true });
      }
      return Response.json(await this.#status(id));
    }
    if (path.startsWith("/__node/")) return this.placements.fetch(request);
    if (this.revoked) {
      const message = `the node was revoked by its owner: pair the machine again`;
      return Response.json({ error: "node_revoked", message }, { status: path === "/__uplink/dial" ? 403 : 410 });
    }
    return this.uplink.fetch(request);
  }
  // Up or down now: one that dials in is up while its uplink is open; one
  // the platform calls, while its health answers (as placing probes it).
  async #status(id) {
    if (this.revoked) return { revoked: true, up: false };
    const nodes = nodesOf(this.env);
    const listed = nodes?.byId.get(id);
    if (listed && !listed.uplink) {
      const h = await health(listed);
      return { revoked: false, up: !h.down, why: h.down || null };
    }
    const up = this.uplink.connected();
    return { revoked: false, up, why: up ? null : "its uplink is not open" };
  }
  webSocketMessage(ws, message) { return this.uplink.webSocketMessage(ws, message); }
  webSocketClose(ws, code, reason, clean) { return this.uplink.webSocketClose(ws, code, reason, clean); }
  webSocketError(ws, error) { return this.uplink.webSocketError(ws, error); }
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
