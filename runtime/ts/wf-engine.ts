// Native Workflows engine: the durable driver that executes one workflow
// run attempt. The AUTHORING SURFACE IS UNCHANGED — fragments still ship
// workflows/*.mjs exporting async run(ctx); this engine maps each run to
// a native Workflow instance (WorkflowEntrypoint / step.do / step.sleep).
//
// Division of labor (docs/ROADMAP.md workstream A):
//   - product layers STAY in the cell: runs ledger, triggers, guards
//     (pause/hops/rate/single-flight), retry classification, held,
//     circuit breaker.
//   - the native workflow owns EXECUTION + durable retry semantics:
//     the attempt body runs inside one step.do; the completion report is
//     its own step.do with bounded step.sleep backoff between tries.
//
// WHY the commit lives in the cell and not inside step memoization: a
// network side effect cannot be made exactly-once by memoization alone —
// the crash window between "commit accepted at code.storage" and "step
// result recorded" re-runs the step, and the commit must still happen
// exactly once. The exactly-once proof therefore lives in the commit
// protocol itself (expected-parent CAS + blob-identity dedup +
// idempotent-replay healing in git-plane.ts commitPaths); the step
// boundary bounds HOW MUCH work re-runs. The flagship test exercises
// exactly this: success, replay (memoized steps), and crash-mid-attempt.
//
// Instance IDs are `r<runId>a<attempt>` — unique per attempt, so a
// retried attempt never collides with a live one (celld's create()
// replaces only TERMINAL instances with the same id).

// Workflow params are bounded at 1 MiB by the platform; we only ship ids
// + a token (input/cause stay in the cell's runs table), so this is a
// sanity ceiling, not a functional limit.
export const WF_PARAM_LIMIT = 64 * 1024;

// Report retries: the attempt's outcome is durable in the workflow; the
// report step may fail only while the cell is unreachable. Five tries
// with exponential sleeps (bounded below) before the workflow errors and
// the cell's crash sweep reconciles from instance status instead.
export const WF_REPORT_TRIES = 5;

export function wfInstanceId(runId: number, attempt: number): string {
  return `r${runId}a${attempt}`;
}

// The one body FragmentWorkflow.run delegates to (exported so tests drive
// the same code the platform drives — no test-only hook: production calls
// this from the WorkflowEntrypoint subclass in index.ts).
export async function runNativeAttempt(event: {
  fragment: string;
  runId: number;
  attempt: number;
  token: string;
  hostSecret?: string;
}, step: import("cloudflare:workers").WorkflowStep, env: any): Promise<{ ok: boolean; output?: unknown; error?: string }> {
  const call = async (route: string, body: unknown) => {
    const headers: Record<string, string> = {
      "content-type": "application/json",
      "x-fragment-token": event.token,
    };
    if (event.hostSecret) headers["x-fragment-host-secret"] = event.hostSecret;
    // HTTP loopback through the router, NOT env.FRAGMENT.getByName: in a
    // workflow instance's context celld resolves the DO binding to a
    // FRESH empty cell (name=null, zero rows) instead of the live one —
    // every attempt 403'd on a token that exists in the real cell's
    // storage (found booting the dev stack). The loopback is the same
    // seam the ctx shim speaks, and the router's own binding resolves
    // the real cell on both platforms.
    const base = String(env.FRAGMENT_INTERNAL_URL || "http://127.0.0.1:8789");
    const resp = await fetch(`${base}/__internal/f/${encodeURIComponent(event.fragment)}${route}`, {
      method: "POST", headers, body: JSON.stringify(body),
    });
    if (!resp.ok) throw new Error(`wf ${route} -> ${resp.status}: ${(await resp.text()).slice(0, 300)}`);
    return await resp.json();
  };

  // THE attempt: one step, executed in the cell (loader isolate + ctx).
  // Replays with a memoized result re-execute nothing; a crash mid-step
  // re-runs the body, where the commit protocol absorbs the duplicate.
  const outcome = await step.do(`attempt`, async () => {
    return await call("/wf/attempt", { runId: event.runId, attempt: event.attempt });
  });

  // Report, bounded: each try is its own step (a failed try re-runs only
  // itself); step.sleep spaces them out. Exhausted -> throw; the cell's
  // crash sweep then reconciles this run from instance.status().
  for (let t = 1; t <= WF_REPORT_TRIES; t++) {
    try {
      await step.do(`report:${t}`, async () => {
        await call("/wf/complete", { runId: event.runId, attempt: event.attempt, outcome });
      });
      return outcome;
    } catch (e) {
      if (t === WF_REPORT_TRIES) throw e;
      await step.sleep(`report-backoff:${t}`, `${Math.min(5 * t, 30)} seconds`);
    }
  }
  throw new Error("unreachable: report retry loop exhausted");
}

// Launch one attempt on the native engine. Returns the instance id the
// caller can poll/await. Typed failure when the host lacks the binding —
// a hard cut means no inline-execution fallback.
export async function launchNativeRun(cell, runId: number, attempt: number): Promise<string> {
  const binding = (cell.env as any).WORKFLOWS;
  if (!binding || typeof binding.create !== "function") {
    throw new Error("native Workflows binding (WORKFLOWS) missing on this host — deploy the runtime with its wrangler workflows config");
  }
  const name = cell.getMeta("name");
  const token = cell.makeToken({ kind: "wf-run", runId, attempt });
  const event = {
    fragment: name,
    runId,
    attempt,
    token,
    ...(cell.env.FRAGMENT_HOST_SECRET ? { hostSecret: String(cell.env.FRAGMENT_HOST_SECRET) } : {}),
  };
  const encoded = JSON.stringify(event);
  if (encoded.length > WF_PARAM_LIMIT) throw new Error("workflow params exceed sanity limit");
  const id = wfInstanceId(runId, attempt);
  await binding.create({ id, params: JSON.parse(encoded) });
  return id;
}

// Read an instance's terminal status (crash sweep + manual-run await).
// Returns null when the instance cannot be resolved (never launched,
// evicted, or the platform lost it) — the caller decides what that means.
export async function nativeStatus(cell, instanceId: string): Promise<{ status: string; output?: unknown; error?: unknown } | null> {
  const binding = (cell.env as any).WORKFLOWS;
  if (!binding || typeof binding.get !== "function") return null;
  try {
    const inst = await binding.get(instanceId);
    return await inst.status();
  } catch {
    return null;
  }
}

// Await a manual run: poll instance status until terminal or the bound.
// Manual runs used to execute inline; native execution makes them async,
// so /api/run waits here (bounded) and then reads the ledger for output.
export const MANUAL_AWAIT_MS = 300_000; // 5 min: generous for one attempt incl. retries of its steps
const POLL_TICK_MS = 500;

export async function awaitNativeRun(cell, instanceId: string): Promise<{ status: string }> {
  const deadline = Date.now() + MANUAL_AWAIT_MS;
  for (;;) {
    const st = await nativeStatus(cell, instanceId);
    if (st && (st.status === "complete" || st.status === "errored" || st.status === "terminated")) {
      return { status: st.status === "complete" ? "complete" : "errored" };
    }
    if (Date.now() + POLL_TICK_MS > deadline) return { status: "timeout" };
    await new Promise((r) => setTimeout(r, POLL_TICK_MS));
  }
}
