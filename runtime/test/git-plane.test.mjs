// git-plane.test — the file plane: tree index, pins, and the commit
// funnel's idempotent-mutation triples (success / replay / conflict).
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert, del } from "./harness.mjs";

async function freshFragment(w, name) {
  const { resp } = await w.makeFragment(name);
  assert.ok(resp.ok, JSON.stringify(resp));
  return resp;
}

test("refreshPin builds the tree index; ensurePins is idempotent", async () => {
  const w = await makeWorld();
  try {
    await freshFragment(w, "tree1");
    const gp = await import("../src/git-plane.js");
    // external writer seeds the repo (what the CLI's direct commits do)
    await w.mock.externalCommit("tree1", [upsert("fragment.json", JSON.stringify({ name: "tree1", visibility: "public" })), upsert("notes/a.md", "# a")]);

    await gp.ensurePins(w.cell("tree1"));
    const pin1 = gp.pinOf(w.cell("tree1"), "main");
    assert.ok(pin1);
    const rows = gp.treeList(w.cell("tree1"), "main");
    assert.deepEqual(rows.map((r) => r.path).sort(), ["fragment.json", "notes/a.md"]);

    // second ensure is a no-op (head unchanged)
    await gp.ensurePins(w.cell("tree1"));
    assert.equal(gp.pinOf(w.cell("tree1"), "main"), pin1);

    // manifest cache was filled from the repo
    assert.equal(w.cell("tree1").manifest().visibility, "public");

    // no live branch yet: pin null, serve read fails clearly
    assert.equal(gp.pinOf(w.cell("tree1"), "live"), null);
  } finally {
    await w.stop();
  }
});

test("commitPaths triple: success, replay (dedup), conflict (bounded retry)", async () => {
  const w = await makeWorld();
  try {
    await freshFragment(w, "cw");
    const gp = await import("../src/git-plane.js");
    await w.mock.externalCommit("cw", [upsert("fragment.json", JSON.stringify({ name: "cw", visibility: "public" }))]);
    await gp.ensurePins(w.cell("cw"));

    const bytes = () => new TextEncoder().encode("payload v1\n");

    // success
    const s1 = await gp.commitPaths(w.cell("cw"), [{ path: "out/x.md", bytes: bytes() }], "test write", "wf:t");
    assert.equal(s1.ok, true);
    assert.equal(s1.deduped, false);
    assert.ok(s1.commitSha);
    assert.equal((await w.mock.commitCount("cw")).count, 2); // seed + ours
    // pin moved to OUR commit and the tree row landed locally
    assert.equal(gp.pinOf(w.cell("cw"), "main"), s1.commitSha);
    const row = w.cell("cw").sql.exec("SELECT size FROM git_tree WHERE ref='main' AND path='out/x.md'").toArray();
    assert.equal(row.length, 1);

    // replay: identical content is a recorded no-op — no commit, no pin move
    const s2 = await gp.commitPaths(w.cell("cw"), [{ path: "out/x.md", bytes: bytes() }], "test write again", "wf:t");
    assert.equal(s2.ok, true);
    assert.equal(s2.deduped, true);
    assert.equal(s2.commitSha, s1.commitSha, "dedup names the head where the content already lives");
    assert.equal((await w.mock.commitCount("cw")).count, 2);
    assert.equal(gp.pinOf(w.cell("cw"), "main"), s1.commitSha);

    // lost-bookkeeping healing: the commit landed but the cell never
    // recorded it (crash between accept and record) — the next identical
    // write must NOT commit again even with a stale pin
    w.cell("cw").sql.exec("DELETE FROM own_commits");
    w.cell("cw").setMeta("pin_main_sha", ""); // simulate pre-write knowledge
    const s3 = await gp.commitPaths(w.cell("cw"), [{ path: "out/x.md", bytes: bytes() }], "heal", "wf:t");
    assert.equal(s3.ok, true);
    assert.equal((await w.mock.commitCount("cw")).count, 2); // still no duplicate
    assert.ok(gp.pinOf(w.cell("cw"), "main")); // pin healed to head

    // conflict: a racing writer lands BETWEEN our head read and our commit
    // (the mock fires the armed change before CAS evaluation) — the engine
    // refetches, re-plans, and lands our change on top, exactly once
    await w.mock.armRace("cw", [upsert("race/z.txt", "racing writer")]);
    const s4 = await gp.commitPaths(w.cell("cw"), [{ path: "out/y.md", bytes: new TextEncoder().encode("y v1\n") }], "after conflict", "wf:t");
    assert.equal(s4.ok, true);
    assert.equal(s4.deduped, false);
    // commits: seed + s1 + race + s4 = 4; s2/s3 added none
    assert.equal((await w.mock.commitCount("cw")).count, 4);
    // repo identity is the url-form id the cell recorded at create
    const head = await (await import("../src/codestorage.js")).readBranchHead(w.env, w.cell("cw").getMeta("cs_repo"), "main");
    assert.equal(head.headSha, s4.commitSha);
    // the racing writer's file and ours are BOTH in the head tree, and the
    // pin/tree followed the whole way
    const paths = gp.treeList(w.cell("cw"), "main").map((r) => r.path).sort();
    assert.ok(paths.includes("race/z.txt") && paths.includes("out/y.md"));
  } finally {
    await w.stop();
  }
});

test("commitPaths refuses: reserved paths, append-only modifications, oversized batches", async () => {
  const w = await makeWorld();
  try {
    await freshFragment(w, "guard");
    const gp = await import("../src/git-plane.js");
    await w.mock.externalCommit("guard", [
      upsert("fragment.json", JSON.stringify({ name: "guard", visibility: "public", appendOnly: ["log/"] })),
      upsert("log/first.md", "one"),
    ]);
    await gp.ensurePins(w.cell("guard"));

    // fragment.json is CLI-owned; .fragment/* never enters the repo
    const r1 = await gp.commitPaths(w.cell("guard"), [{ path: "fragment.json", bytes: new TextEncoder().encode("{}") }], "m", "wf:t");
    assert.equal(r1.ok, false);
    assert.match(r1.error, /reserved/);
    const r2 = await gp.commitPaths(w.cell("guard"), [{ path: ".fragment/secrets.json", bytes: new TextEncoder().encode("x") }], "m", "wf:t");
    assert.equal(r2.ok, false);
    assert.match(r2.error, /reserved/);

    // append-only: identical rewrite is a no-op, modification is refused
    const r3 = await gp.commitPaths(w.cell("guard"), [{ path: "log/first.md", bytes: new TextEncoder().encode("one") }], "m", "wf:t");
    assert.equal(r3.ok, true);
    assert.equal(r3.deduped, true);
    const r4 = await gp.commitPaths(w.cell("guard"), [{ path: "log/first.md", bytes: new TextEncoder().encode("CHANGED") }], "m", "wf:t");
    assert.equal(r4.ok, false);
    assert.equal(r4.status, 409);
    // a NEW path under the prefix is fine
    const r5 = await gp.commitPaths(w.cell("guard"), [{ path: "log/second.md", bytes: new TextEncoder().encode("two") }], "m", "wf:t");
    assert.equal(r5.ok, true);

    // batch bound: COMMIT_MAX_FILES is explicit and enforced
    const tooMany = Array.from({ length: gp.COMMIT_MAX_FILES + 1 }, (_, i) => ({ path: `b/${i}.txt`, bytes: new TextEncoder().encode("x") }));
    const r6 = await gp.commitPaths(w.cell("guard"), tooMany, "m", "wf:t");
    assert.equal(r6.ok, false);
    assert.match(r6.error, /split the batch/);
  } finally {
    await w.stop();
  }
});

test("deletes: absent-path delete is a no-op; real delete commits", async () => {
  const w = await makeWorld();
  try {
    await freshFragment(w, "del");
    const gp = await import("../src/git-plane.js");
    await w.mock.externalCommit("del", [upsert("fragment.json", JSON.stringify({ name: "del", visibility: "public" })), upsert("gone.md", "bye")]);
    await gp.ensurePins(w.cell("del"));

    const r1 = await gp.commitPaths(w.cell("del"), [{ path: "never-existed.md", delete: true }], "m", "wf:t");
    assert.equal(r1.ok, true);
    assert.equal(r1.deduped, true);

    const r2 = await gp.commitPaths(w.cell("del"), [{ path: "gone.md", delete: true }], "m", "wf:t");
    assert.equal(r2.ok, true);
    assert.equal(r2.deduped, false);
    assert.equal(gp.treeList(w.cell("del"), "main").map((r) => r.path).includes("gone.md"), false);
    // double delete: no-op
    const r3 = await gp.commitPaths(w.cell("del"), [{ path: "gone.md", delete: true }], "m", "wf:t");
    assert.equal(r3.deduped, true);
  } finally {
    await w.stop();
  }
});

test("LRU: text reads cache; oversized entries reject on text, stream fine", async () => {
  const w = await makeWorld();
  try {
    await freshFragment(w, "lru");
    const gp = await import("../src/git-plane.js");
    const cs = await import("../src/codestorage.js");
    const small = "x".repeat(1024);
    await w.mock.externalCommit("lru", [
      upsert("fragment.json", JSON.stringify({ name: "lru", visibility: "public" })),
      upsert("small.txt", small),
    ]);
    await gp.ensurePins(w.cell("lru"));

    const t1 = await gp.readFileText(w.cell("lru"), "small.txt");
    assert.equal(t1, small);
    const stats = gp.lruStats();
    assert.ok(stats.entries >= 1 && stats.bytes > 0 && stats.bytes <= gp.LRU_TOTAL_BYTES);

    // an entry over the per-entry cap: the TEXT materializer refuses
    // (bounded decode), the STREAM path serves it — no ceiling there
    const bigLen = gp.LRU_ENTRY_BYTES + 1024;
    const big = "y".repeat(bigLen);
    await w.mock.externalCommit("lru", [upsert("big.txt", big)]);
    await gp.refreshPin(w.cell("lru"), "main");
    await assert.rejects(
      () => gp.readFileText(w.cell("lru"), "big.txt"),
      (e) => e instanceof cs.CodeStorageError && e.kind === "payload-too-large",
    );
    const resp = await gp.readFileStream(w.cell("lru"), "big.txt", "main");
    assert.equal(resp.status, 200);
    assert.equal((await resp.arrayBuffer()).byteLength, bigLen);
    // streaming it did not evict the small entry
    assert.ok(gp.lruStats().entries >= 1);
  } finally {
    await w.stop();
  }
});
