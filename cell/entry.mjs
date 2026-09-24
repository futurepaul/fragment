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

export class Registry extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    this.rs = new rs.RegistryCell(ctx, env);
  }
  fetch(request) { return this.rs.fetch(request); }
}

// The app's read access to its files (files.rs), handed to its facet as
// `env.FILES` bound to one fragment by `props`: the app cannot name another.
export class Files extends WorkerEntrypoint {
  async #ask(op, body) {
    const { fragment } = this.ctx.props;
    return this.env.FRAGMENT.getByName(fragment).fetch(`https://fragment.internal/cap/files/${op}`, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-fragment-cap": "files",
        "x-fragment-name": fragment,
        "x-fragment-url": "https://fragment.internal/",
      },
      body: JSON.stringify(body),
    });
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
// step through the supervisor; both answers are durable step results, so a
// replay after a crash re-sends nothing that already happened. A step that
// fails for now (the supervisor answers 5xx) is retried with backoff; when
// its retries run out the job sees the error and may catch it.
export class Job extends WorkflowEntrypoint {
  async run(event, step) {
    const { fragment, incarnation, run, attempt } = event.payload;
    const delay = Math.max(1, Number(this.env.FRAGMENT_JOB_RETRY_DELAY_S) || 10);
    const retrying = { retries: { limit: 4, delay: `${delay} seconds`, backoff: "exponential" }, timeout: "5 minutes" };
    const post = async (path, body) => {
      const resp = await this.env.FRAGMENT.getByName(fragment).fetch(`https://fragment.internal/job/${path}`, {
        method: "POST",
        headers: {
          "content-type": "application/json",
          "x-fragment-job": "1",
          "x-fragment-name": fragment,
          "x-fragment-url": "https://fragment.internal/",
        },
        body: JSON.stringify({ incarnation, run, attempt, ...body }),
      });
      const out = await resp.json().catch(() => ({}));
      if (!resp.ok) throw new Error(out.message || `job/${path} answered ${resp.status}`);
      return out;
    };
    const results = [];
    try {
      for (let i = 0; ; i++) {
        const next = await step.do(`advance ${i}`, retrying, () => post("advance", { results }));
        if (!next.step) return next; // done, failed, or stop: the supervisor has recorded it
        const { kind, args } = next.step;
        if (kind === "sleep") {
          await step.sleep(`sleep ${i}`, args.ms);
          results.push({ kind, value: null });
          continue;
        }
        let result;
        try {
          result = await step.do(`${kind} ${i}`, retrying, () => post("effect", { index: i, kind, args }));
        } catch (e) {
          result = { kind, error: String((e && e.message) || e) };
        }
        if (result.stop) return result;
        results.push(result);
      }
    } catch (e) {
      const error = String((e && e.message) || e);
      await step.do("finish", retrying, () => post("finish", { error }));
      return { failed: error };
    }
  }
}
