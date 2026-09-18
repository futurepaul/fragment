// rooms-state.test — ctx.rooms.setState must address the NAMED room. The
// shim used to POST /rooms/state with the room argument dropped, so every
// workflow setState (the vault notify pattern) landed on the unnamed room
// "" and viewers listening on the real room never got the bump. Drives the
// real shim inside a real loaded worker through the loopback, then checks
// the cell's own rooms table.
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert } from "./harness.mjs";

const WF_FILE = "workflows/bumper.mjs";
const WF_SOURCE = `
export async function run(ctx) {
  await ctx.rooms.setState("anno", { n: 1 });
  return { back: await ctx.rooms.getState("anno") };
}
`;

test("positive: setState addresses the named room and getState reads it back", async () => {
  const w = await makeWorld();
  try {
    const name = "rs1";
    const { resp } = await w.makeFragment(name);
    assert.ok(resp.ok, JSON.stringify(resp));
    await w.mock.externalCommit(name, [
      upsert("fragment.json", JSON.stringify({ name, visibility: "public", workflows: [{ name: "bumper", file: WF_FILE }] })),
      upsert(WF_FILE, WF_SOURCE),
    ]);
    const gp = await import("../src/git-plane.js");
    await gp.refreshPin(w.cell(name), "main");

    const out = await w.cell(name).executeWorkflow(
      { name: "bumper", file: WF_FILE }, null, { trigger: "manual" },
    );
    assert.ok(out.launched, JSON.stringify(out));
    const inst = w.env.WORKFLOWS.instances.get(out.instanceId);
    const result = await inst.run();
    assert.equal(result.ok, true);
    assert.deepEqual(result.output.back, { n: 1 }); // getState roundtrip

    const rows = w.cell(name).sql.exec("SELECT room, state FROM rooms").toArray();
    assert.equal(rows.length, 1);
    assert.equal(rows[0].room, "anno");
    assert.deepEqual(JSON.parse(rows[0].state), { n: 1 });
  } finally {
    await w.stop();
  }
});

test("negative: no state leaks onto the unnamed room", async () => {
  const w = await makeWorld();
  try {
    const name = "rs2";
    const { resp } = await w.makeFragment(name);
    assert.ok(resp.ok, JSON.stringify(resp));
    await w.mock.externalCommit(name, [
      upsert("fragment.json", JSON.stringify({ name, visibility: "public", workflows: [{ name: "bumper", file: WF_FILE }] })),
      upsert(WF_FILE, WF_SOURCE),
    ]);
    const gp = await import("../src/git-plane.js");
    await gp.refreshPin(w.cell(name), "main");

    const out = await w.cell(name).executeWorkflow(
      { name: "bumper", file: WF_FILE }, null, { trigger: "manual" },
    );
    const inst = w.env.WORKFLOWS.instances.get(out.instanceId);
    await inst.run();

    // the bug's exact shape: state silently addressed to room ""
    const leak = w.cell(name).sql.exec("SELECT state FROM rooms WHERE room = ''").toArray();
    assert.equal(leak.length, 0);
  } finally {
    await w.stop();
  }
});
