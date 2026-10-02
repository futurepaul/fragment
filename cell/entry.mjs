// The only hand-written JavaScript on the platform side (besides
// platform.mjs, which runs inside the app facet). Everything else is Rust.
// celld refuses RPC to a class that does not extend DurableObject, and
// workers-rs classes do not, so these classes do and forward each handler.
// workers-rs 0.8.5 has no Workflows, so the job driver is here too; it
// only loops and calls back: every decision is the supervisor's (jobs.rs).
import { DurableObject, WorkerEntrypoint, WorkflowEntrypoint } from "cloudflare:workers";
import * as rs from "./build/index.js";

export default rs.default;

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
}

export class Computer extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.ComputerCell(ctx, env);
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
