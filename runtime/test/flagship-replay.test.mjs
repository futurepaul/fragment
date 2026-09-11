// flagship-replay.test — THE flagship requirement (ROADMAP "Testing"):
// a replayed step.do that commits must commit exactly once, across the
// triple: success, replay, and conflicting-body. This drives the REAL
// production stack: FragmentWorkflow's runNativeAttempt (via the fake
// platform binding), the wf/attempt + wf/complete internal routes, the
// real ctx shim inside a real loaded worker, the commit funnel
// (git-plane commitPaths), and the mock code.storage.
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert, deliverWebhook, pushPayload } from "./harness.mjs";

const WF_FILE = `workflows/writer.mjs`;
const WF_SOURCE = `
export async function run(ctx) {
  const r = await ctx.files.write("out/result.md", "committed once\\n");
  ctx.log("write returned deduped=" + r.deduped);
  return { deduped: r.deduped, sha: r.sha };
}
`;

async function setup(w, name, manifest) {
  const { resp } = await w.makeFragment(name);
  assert.ok(resp.ok, JSON.stringify(resp));
  await w.mock.externalCommit(name, [
    upsert("fragment.json", JSON.stringify(manifest)),
    upsert(WF_FILE, WF_SOURCE),
  ]);
  const gp = await import("../src/git-plane.js");
  await gp.refreshPin(w.cell(name), "main");
  return resp;
}

async function drive(w, name, wfName, input) {
  // manual-run path: guards → row → native launch; the test IS the engine
  // ticking the instance (celld would do this on its own schedule)
  const out = await w.cell(name).executeWorkflow(
    { name: wfName, file: WF_FILE }, input ?? null, { trigger: "manual" },
  );
  assert.ok(out.launched, JSON.stringify(out));
  const inst = w.env.WORKFLOWS.instances.get(out.instanceId);
  assert.ok(inst, "instance created");
  return { out, inst };
}

const count = async (w, name) => (await w.mock.commitCount(name)).count;

test("FLAGSHIP success: one run, exactly one commit, ledger + run row agree", async () => {
  const w = await makeWorld();
  try {
    const name = "flag1";
    await setup(w, name, { name, visibility: "public", workflows: [{ name: "writer", file: WF_FILE }] });
    const base = await count(w, name); // seed commit only

    const { out, inst } = await drive(w, name, "writer");
    const result = await inst.run();
    assert.equal(result.ok, true);

    assert.equal(await count(w, name), base + 1); // EXACTLY one commit
    const row = w.cell(name).sql.exec("SELECT status FROM runs WHERE id = ?", out.runId).toArray()[0];
    assert.equal(row.status, "success");
    const evs = w.cell(name).sql.exec("SELECT kind FROM events WHERE kind = 'git.commit'").toArray();
    assert.equal(evs.length, 1);

    // the write is visible through the working-copy read path
    const gp = await import("../src/git-plane.js");
    assert.equal(await gp.readFileText(w.cell(name), "out/result.md"), "committed once\n");
  } finally {
    await w.stop();
  }
});

test("FLAGSHIP replay: memoized steps re-execute nothing; commit count unchanged", async () => {
  const w = await makeWorld();
  try {
    const name = "flag2";
    await setup(w, name, { name, visibility: "public", workflows: [{ name: "writer", file: WF_FILE }] });
    const base = await count(w, name);

    const { inst } = await drive(w, name, "writer");
    await inst.run();
    assert.equal(await count(w, name), base + 1);

    // the replay: celld re-invokes run() (engine restart, instance
    // resumed) with the SAME durable step store — completed steps return
    // memoized results and never re-run their callbacks
    const again = await inst.run();
    assert.equal(again.ok, true);
    assert.equal(await count(w, name), base + 1); // still exactly one
    // the completion report was also memoized — no duplicate application
    const rows = w.cell(name).sql.exec("SELECT status FROM runs WHERE status = 'success'").toArray();
    assert.equal(rows.length, 1);
  } finally {
    await w.stop();
  }
});

test("FLAGSHIP crash-mid-attempt: commit landed, step result lost — re-run heals without a duplicate", async () => {
  const w = await makeWorld();
  try {
    const name = "flag3";
    await setup(w, name, { name, visibility: "public", workflows: [{ name: "writer", file: WF_FILE }] });
    const base = await count(w, name);

    const { out, inst } = await drive(w, name, "writer");
    inst.hooks.crashAfter = "attempt"; // side effect lands, memoization doesn't
    await assert.rejects(() => inst.run(), /simulated crash/);
    assert.equal(await count(w, name), base + 1); // the commit DID land

    // the sweep sees a terminal-errored instance and classifies the run
    // as interrupted (retryable): backoff, then attempt 2
    w.cell(name).sql.exec("UPDATE runs SET started_at = started_at - 120000 WHERE id = ?", out.runId);
    await w.cell(name).resumeDueRuns();
    let row = w.cell(name).sql.exec("SELECT status, attempt FROM runs WHERE id = ?", out.runId).toArray()[0];
    assert.equal(row.status, "backoff");
    assert.equal(row.attempt, 1);

    // due retry launches attempt 2; drive it WITHOUT the crash hook
    w.cell(name).sql.exec("UPDATE runs SET next_attempt_at = ? WHERE id = ?", Date.now() - 1000, out.runId);
    await w.cell(name).resumeDueRuns();
    const inst2Id = `r${out.runId}a2`;
    const inst2 = w.env.WORKFLOWS.instances.get(inst2Id);
    assert.ok(inst2, "attempt-2 instance launched");
    const r2 = await inst2.run();
    assert.equal(r2.ok, true);

    // the re-run wrote IDENTICAL content: deduped — no second commit
    assert.equal(await count(w, name), base + 1); // EXACTLY once, still
    row = w.cell(name).sql.exec("SELECT status, attempt FROM runs WHERE id = ?", out.runId).toArray()[0];
    assert.equal(row.status, "success");
    assert.equal(row.attempt, 2);
    assert.equal(r2.output.deduped, true);
  } finally {
    await w.stop();
  }
});

test("FLAGSHIP conflicting body: a competing writer between crash and re-run — each logical change commits exactly once", async () => {
  const w = await makeWorld();
  try {
    const name = "flag4";
    await setup(w, name, { name, visibility: "public", workflows: [{ name: "writer", file: WF_FILE }] });
    const base = await count(w, name);

    const { out, inst } = await drive(w, name, "writer");
    inst.hooks.crashAfter = "attempt";
    await assert.rejects(() => inst.run(), /simulated crash/);
    assert.equal(await count(w, name), base + 1); // our X landed

    // an external writer lands DIFFERENT content on the same path (the
    // "conflicting body" arm: the replayed write no longer matches)
    await w.mock.externalCommit(name, [upsert("out/result.md", "someone else got here first\n")]);
    assert.equal(await count(w, name), base + 2);

    // reconcile + retry: attempt 2 sees moved head, its write is NEW
    // content now — it commits on top, exactly once for this logical change
    w.cell(name).sql.exec("UPDATE runs SET started_at = started_at - 120000 WHERE id = ?", out.runId);
    await w.cell(name).resumeDueRuns();
    w.cell(name).sql.exec("UPDATE runs SET next_attempt_at = ? WHERE id = ?", Date.now() - 1000, out.runId);
    await w.cell(name).resumeDueRuns();
    const inst2 = w.env.WORKFLOWS.instances.get(`r${out.runId}a2`);
    const r2 = await inst2.run();
    assert.equal(r2.ok, true);
    assert.equal(r2.output.deduped, false);

    assert.equal(await count(w, name), base + 3); // ours(1) + ext(1) + ours(2)
    const row = w.cell(name).sql.exec("SELECT status FROM runs WHERE id = ?", out.runId).toArray()[0];
    assert.equal(row.status, "success");
    // the final head content is OUR write (the last writer won cleanly)
    const gp2 = await import("../src/git-plane.js");
    await gp2.refreshPin(w.cell(name), "main");
    assert.equal(await gp2.readFileText(w.cell(name), "out/result.md"), "committed once\n");
  } finally {
    await w.stop();
  }
});

test("loop safety: our own commit's webhook refreshes the pin but never fires sync triggers", async () => {
  const w = await makeWorld();
  try {
    const name = "loop1";
    await setup(w, name, {
      name, visibility: "public",
      workflows: [
        { name: "writer", file: WF_FILE },
        { name: "reactor", file: WF_FILE, trigger: "files" },
      ],
    });
    const { inst } = await drive(w, name, "writer");
    await inst.run();

    // code.storage fires the push webhook for OUR commit (it's a real push)
    const gp = await import("../src/git-plane.js");
    const pin = gp.pinOf(w.cell(name), "main");
    const r = await deliverWebhook(w, name, w.cell(name).getMeta("webhook_secret"), "push",
      pushPayload(name, "refs/heads/main", "0".repeat(40), pin));
    assert.equal(r.status, 200);
    assert.equal(r.body.interpreted, true);

    // pin stayed at our commit (own_commits match)
    assert.equal(gp.pinOf(w.cell(name), "main"), pin);
    // the reactor workflow was NOT triggered: no sync trigger scheduled,
    // no run rows for it
    assert.equal(w.cell(name).getMeta("sync_trigger_at") || "", "");
    const reactorRuns = w.cell(name).sql.exec("SELECT COUNT(*) c FROM runs WHERE wf = 'reactor'").toArray()[0].c;
    assert.equal(reactorRuns, 0);
    const ev = w.cell(name).sql.exec("SELECT kind FROM events WHERE kind = 'git.self-push'").toArray();
    assert.equal(ev.length, 1);

    // contrast: an EXTERNAL commit's webhook DOES schedule the trigger
    await w.mock.externalCommit(name, [upsert("ext.txt", "from outside")]);
    const r2 = await deliverWebhook(w, name, w.cell(name).getMeta("webhook_secret"), "push",
      pushPayload(name, "refs/heads/main", pin, "f".repeat(40)));
    assert.equal(r2.body.interpreted, true);
    assert.ok(parseInt(w.cell(name).getMeta("sync_trigger_at") || "0", 10) > 0, "sync trigger scheduled for external push");
  } finally {
    await w.stop();
  }
});
