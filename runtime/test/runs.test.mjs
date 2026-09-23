// runs.test — the runs state machine: valid and invalid transitions.
// Guards (paused/rate/single-flight), held vs backoff classification,
// duplicate outcome reports, and the poll backstop's failure containment.
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert } from "./harness.mjs";
import { wfInstanceId } from "../src/wf-engine.js";

const WF = "workflows/job.mjs";

async function setup(w, name, workflows, source) {
  await w.makeFragment(name);
  await w.mock.externalCommit(name, [
    upsert("fragment.json", JSON.stringify({ name, visibility: "public", workflows })),
    upsert(WF, source ?? "export async function run(ctx) { return { ok: true }; }\n"),
  ]);
  const gp = await import("../src/git-plane.js");
  await gp.refreshPin(w.cell(name), "main");
}
const rowOf = (cell, id) => cell.sql.exec("SELECT * FROM runs WHERE id = ?", id).toArray()[0];

test("valid transitions: launch → success; terminal error → held; retryable error → backoff → success", async () => {
  const w = await makeWorld();
  try {
    const name = "runs1";
    await setup(w, name, [{ name: "job", file: WF }]);

    // ---- success ----
    let out = await w.cell(name).executeWorkflow({ name: "job", file: WF }, null, { trigger: "manual" });
    let inst = w.env.WORKFLOWS.instances.get(out.instanceId);
    await inst.run();
    assert.equal(rowOf(w.cell(name), out.runId).status, "success");

    // ---- terminal error parks as held (a code error is not retryable) ----
    await setup(w, name + "x", [{ name: "job", file: WF }], "export async function run(ctx) { throw new Error('bad parse: boom'); }\n");
    const cellx = w.cell(name + "x");
    // repo identity is the url-form id createRepo returned, not the name
    const reg = await cellx.getMeta("cs_repo");
    assert.match(reg, /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
    out = await cellx.executeWorkflow({ name: "job", file: WF }, null, { trigger: "manual" });
    inst = w.env.WORKFLOWS.instances.get(out.instanceId);
    await inst.run(); // workflow completes (the attempt RESULT carries the error)
    const held = rowOf(cellx, out.runId);
    assert.equal(held.status, "held");
    assert.match(held.error, /boom/);

    // ---- retryable error: backoff, then a clean attempt succeeds ----
    let failOnce = true;
    await w.mock.externalCommit(name, [upsert(WF, "export async function run(ctx) { return { ok: true }; }\n")]);
    // simulate a retryable attempt outcome via the engine's own complete
    // route: launch, then report a network-shaped error
    out = await w.cell(name).executeWorkflow({ name: "job", file: WF }, null, { trigger: "manual" });
    const token = w.cell(name).makeToken({ kind: "wf-run", runId: out.runId, attempt: 1 });
    const r1 = await w.env.FRAGMENT.getByName(name).fetch(`http://wf/__internal/wf/complete`, {
      method: "POST",
      headers: { "x-fragment-token": token, "content-type": "application/json" },
      body: JSON.stringify({ runId: out.runId, attempt: 1, outcome: { ok: false, error: "fetch failed: socket hang up" } }),
    });
    assert.equal(r1.status, 200);
    let row = rowOf(w.cell(name), out.runId);
    assert.equal(row.status, "backoff");
    assert.equal(row.attempt, 1);
    assert.ok(row.next_attempt_at > Date.now());

    // a DUPLICATE outcome report (report step retried late) is ignored
    const r2 = await w.env.FRAGMENT.getByName(name).fetch(`http://wf/__internal/wf/complete`, {
      method: "POST",
      headers: { "x-fragment-token": token, "content-type": "application/json" },
      body: JSON.stringify({ runId: out.runId, attempt: 1, outcome: { ok: false, error: "fetch failed: socket hang up" } }),
    });
    assert.equal((await r2.json()).already, "backoff");
    assert.equal(rowOf(w.cell(name), out.runId).status, "backoff");

    // due retry: attempt 2 runs clean and succeeds
    w.cell(name).sql.exec("UPDATE runs SET next_attempt_at = ? WHERE id = ?", Date.now() - 1000, out.runId);
    await w.cell(name).resumeDueRuns();
    const inst2 = w.env.WORKFLOWS.instances.get(wfInstanceId(w.cell(name).getMeta("fragment_npub"), out.runId, 2));
    assert.ok(inst2);
    await inst2.run();
    row = rowOf(w.cell(name), out.runId);
    assert.equal(row.status, "success");
    assert.equal(row.attempt, 2);
  } finally {
    await w.stop();
  }
});

test("instance ids are namespaced per fragment: two fragments' first runs launch side by side", async () => {
  // Goal: the native Workflows binding is one namespace for the whole fleet
  // and run ids restart at 1 in every fragment, so instance ids must name
  // the fragment. Method: launch run 1 in two fragments while the first is
  // still live, on a binding that refuses live duplicates as celld does,
  // then finish both and read each outcome back through its own row.
  const w = await makeWorld();
  try {
    await setup(w, "ns-a", [{ name: "job", file: WF }]);
    await setup(w, "ns-b", [{ name: "job", file: WF }]);
    const a = await w.cell("ns-a").executeWorkflow({ name: "job", file: WF }, null, { trigger: "manual" });
    const b = await w.cell("ns-b").executeWorkflow({ name: "job", file: WF }, null, { trigger: "manual" });
    assert.equal(a.launched, true);
    assert.equal(b.launched, true, "the second fragment's first run launches while the first is live");
    assert.equal(a.runId, 1);
    assert.equal(b.runId, 1);
    assert.notEqual(a.instanceId, b.instanceId);
    await w.env.WORKFLOWS.instances.get(b.instanceId).run();
    await w.env.WORKFLOWS.instances.get(a.instanceId).run();
    assert.equal(rowOf(w.cell("ns-a"), 1).status, "success");
    assert.equal(rowOf(w.cell("ns-b"), 1).status, "success");
  } finally {
    await w.stop();
  }
});

test("instance ids: same fragment and run differ per attempt; corrupt inputs are refused", () => {
  // Goal: retries never collide with a live attempt, and an id is never
  // guessed from corrupt state. Method: derive ids directly.
  const npubA = "npub1" + "q".repeat(58);
  const npubB = "npub1" + "p".repeat(58);
  assert.notEqual(wfInstanceId(npubA, 1, 1), wfInstanceId(npubA, 1, 2));
  assert.notEqual(wfInstanceId(npubA, 1, 1), wfInstanceId(npubB, 1, 1));
  assert.match(wfInstanceId(npubA, 12, 3), /^f[a-z0-9]{16}r12a3$/);
  assert.throws(() => wfInstanceId(null, 1, 1), /corrupt state: fragment npub/);
  assert.throws(() => wfInstanceId("npub1short", 1, 1), /corrupt state: fragment npub/);
  assert.throws(() => wfInstanceId(npubA, 0, 1), /corrupt state: run id/);
  assert.throws(() => wfInstanceId(npubA, 1, 1.5), /corrupt state: attempt/);
});

test("invalid transitions: paused blocks auto runs; single-flight skips; hop budget refuses", async () => {
  const w = await makeWorld();
  try {
    const name = "runs2";
    await setup(w, name, [
      { name: "job", file: WF, paused: true },
    ]);

    // paused + auto → blocked (recorded), manual would still run
    const b = await w.cell(name).executeWorkflow({ name: "job", file: WF, paused: true }, null, { auto: true, trigger: "cron" });
    assert.ok(b.blocked);
    assert.equal(rowOf(w.cell(name), b.runId).status, "blocked");

    // unpause; single-flight: an active run makes the next auto run skip
    w.cell(name).setMeta("manifest", JSON.stringify({ ...(w.cell(name).manifest()), workflows: [{ name: "job", file: WF }] }));
    const m2 = w.cell(name).manifest();
    m2.workflows[0].paused = undefined;
    w.cell(name).setMeta("manifest", JSON.stringify(m2));
    const a1 = await w.cell(name).executeWorkflow({ name: "job", file: WF }, null, { auto: true, trigger: "cron" });
    assert.ok(a1.launched);
    const a2 = await w.cell(name).executeWorkflow({ name: "job", file: WF }, null, { auto: true, trigger: "cron" });
    assert.ok(a2.skipped);
    assert.equal(rowOf(w.cell(name), a2.runId).status, "skipped");

    // hop budget: depth over HOP_LIMIT without cycles:true → blocked
    const deep = await w.cell(name).executeWorkflow({ name: "job", file: WF }, null, { auto: true, trigger: "inbox", cause: { origin: "far", depth: 99 } });
    assert.ok(deep.blocked);
    const cyc = w.cell(name).sql.exec("SELECT COUNT(*) c FROM events WHERE kind = 'cycle.detected'").toArray()[0].c;
    assert.equal(cyc, 1);
  } finally {
    await w.stop();
  }
});

test("poll backstop: refreshes pins on the alarm and contains failures as events", async () => {
  const w = await makeWorld();
  try {
    const name = "poll1";
    await w.makeFragment(name);
    const gp = await import("../src/git-plane.js");

    await w.mock.externalCommit(name, [upsert("a.txt", "a")]);
    await gp.pollBackstop(w.cell(name));
    assert.ok(gp.pinOf(w.cell(name), "main"), "poll filled the pin");
    assert.ok(parseInt(w.cell(name).getMeta("poll_last_at") || "0", 10) > 0);
    assert.equal(gp.nextPollAt(w.cell(name)) > Date.now(), true);

    // a dead mock must not throw the poll — it lands as an event
    await w.mock.stop();
    await gp.pollBackstop(w.cell(name));
    const fails = w.cell(name).sql.exec("SELECT COUNT(*) c FROM events WHERE kind = 'git.poll-failed'").toArray()[0].c;
    assert.equal(fails, 2); // main + live each recorded their failure
  } finally {
    // mock already stopped; loopback still needs closing
    w.loopback.close();
  }
});
