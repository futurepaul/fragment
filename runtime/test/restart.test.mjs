// restart.test — storage invariant: the tree index survives cell wake,
// and rebuilds correctly from git when absent (the ROADMAP restart test).
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert } from "./harness.mjs";

test("tree index and pins survive a wake; absent index rebuilds from git", async () => {
  const w = await makeWorld();
  try {
    const name = "restart1";
    await w.makeFragment(name);
    const gp = await import("../src/git-plane.js");
    await w.mock.externalCommit(name, [
      upsert("fragment.json", JSON.stringify({ name, visibility: "public" })),
      upsert("a.txt", "a"), upsert("b/c.txt", "c"),
    ]);
    await gp.ensurePins(w.cell(name));
    const pinBefore = gp.pinOf(w.cell(name), "main");
    const treeBefore = gp.treeList(w.cell(name), "main").map((r) => r.path).sort();
    assert.deepEqual(treeBefore, ["a.txt", "b/c.txt", "fragment.json"]);

    // ---- wake: a NEW FragmentCell instance over the SAME sqlite db ----
    const cell2 = w.restartCell(name);
    assert.equal(gp.pinOf(cell2, "main"), pinBefore, "pin survives wake");
    assert.deepEqual(gp.treeList(cell2, "main").map((r) => r.path).sort(), treeBefore, "tree index survives wake");
    // reads work immediately from the persisted index (no refetch needed)
    assert.equal(await gp.readFileText(cell2, "a.txt"), "a");

    // ---- index lost (corrupt/emptied): rebuilt from git on demand ----
    cell2.sql.exec("DELETE FROM git_tree");
    cell2.setMeta("pin_main_sha", "");
    const treeAfter = gp.treeList(cell2, "main");
    assert.equal(treeAfter.length, 0, "index is really gone");
    await gp.ensurePins(cell2);
    assert.equal(gp.pinOf(cell2, "main"), pinBefore, "pin rebuilt to the same head");
    assert.deepEqual(gp.treeList(cell2, "main").map((r) => r.path).sort(), treeBefore, "tree rebuilt from git");
    assert.equal(await gp.readFileText(cell2, "b/c.txt"), "c");
  } finally {
    await w.stop();
  }
});
