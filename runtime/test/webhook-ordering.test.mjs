// webhook-ordering.test — ROADMAP storage invariant: "pinned SHA never
// regresses — late webhook must not move the pin backward". Also covers
// redelivery dedupe and rejected deliveries (bad HMAC, stale timestamp).
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert, deliverWebhook, pushPayload } from "./harness.mjs";

test("a late delivery does not regress the pinned SHA; redeliveries dedupe", async () => {
  const w = await makeWorld();
  try {
    const name = "order1";
    const { resp } = await w.makeFragment(name);
    const secret = resp.webhookSecret;
    const gp = await import("../src/git-plane.js");

    // push 1 lands
    const c1 = await w.mock.externalCommit(name, [upsert("a.txt", "one")]);
    let r = await deliverWebhook(w, name, secret, "push", pushPayload(name, "refs/heads/main", "0".repeat(40), c1.result.new_sha));
    assert.equal(r.body.interpreted, true);
    assert.equal(gp.pinOf(w.cell(name), "main"), c1.result.new_sha);
    assert.ok(parseInt(w.cell(name).getMeta("sync_trigger_at") || "0", 10) > 0, "external push scheduled a trigger");

    // push 2 lands; its webhook arrives
    const c2 = await w.mock.externalCommit(name, [upsert("b.txt", "two")]);
    const push2 = pushPayload(name, "refs/heads/main", c1.result.new_sha, c2.result.new_sha);
    r = await deliverWebhook(w, name, secret, "push", push2);
    assert.equal(r.body.interpreted, true);
    assert.equal(gp.pinOf(w.cell(name), "main"), c2.result.new_sha);

    // LATE delivery: push 1's webhook arrives after push 2 — the pin must
    // stay at c2 (interpretation reads the branch head, never `after`)
    r = await deliverWebhook(w, name, secret, "push", pushPayload(name, "refs/heads/main", "0".repeat(40), c1.result.new_sha, "2026-01-01T00:00:00Z"));
    assert.equal(r.status, 200);
    assert.equal(r.body.interpreted, true);
    assert.equal(gp.pinOf(w.cell(name), "main"), c2.result.new_sha, "late webhook must not regress the pin");
    // the tree still contains both files at the (unmoved) pin
    const paths = gp.treeList(w.cell(name), "main").map((x) => x.path).sort();
    assert.deepEqual(paths, ["a.txt", "b.txt"]);

    // redelivery of push 2's EXACT webhook (same payload bytes): deduped
    r = await deliverWebhook(w, name, secret, "push", push2);
    assert.equal(r.body.redelivery, true);
    assert.equal(r.body.interpreted, false);
    assert.equal(gp.pinOf(w.cell(name), "main"), c2.result.new_sha);
    const redeliveries = w.cell(name).sql.exec("SELECT COUNT(*) c FROM events WHERE kind = 'webhook.redelivery'").toArray()[0].c;
    assert.equal(redeliveries, 1);
  } finally {
    await w.stop();
  }
});

test("rejected deliveries: bad signature and stale timestamps never touch state", async () => {
  const w = await makeWorld();
  try {
    const name = "order2";
    const { resp } = await w.makeFragment(name);
    const gp = await import("../src/git-plane.js");

    await w.mock.externalCommit(name, [upsert("x.txt", "x")]);

    // wrong secret → 401, no pin, no events beyond the rejection
    const bad = await deliverWebhook(w, name, "wrong-secret", "push", pushPayload(name, "refs/heads/main", "0".repeat(40), "a".repeat(40)));
    assert.equal(bad.status, 401);
    // stale timestamp (right secret, 1h old) → 401
    const stale = await deliverWebhook(w, name, resp.webhookSecret, "push",
      pushPayload(name, "refs/heads/main", "0".repeat(40), "a".repeat(40)), Math.floor(Date.now() / 1000) - 3600);
    assert.equal(stale.status, 401);

    assert.equal(gp.pinOf(w.cell(name), "main"), null, "no pin from rejected deliveries");
    const rejected = w.cell(name).sql.exec("SELECT COUNT(*) c FROM events WHERE kind = 'webhook.rejected'").toArray()[0].c;
    assert.equal(rejected, 2);
  } finally {
    await w.stop();
  }
});

test("non-push events and untracked refs ack politely without interpretation", async () => {
  const w = await makeWorld();
  try {
    const name = "order3";
    const { resp } = await w.makeFragment(name);
    const gp = await import("../src/git-plane.js");
    await w.mock.externalCommit(name, [upsert("x.txt", "x")]);

    // a sync event (their documented shape) — not a push: ignored
    let r = await deliverWebhook(w, name, resp.webhookSecret, "repo.sync.succeeded", { type: "repo.sync.succeeded", repository: { id: "r", url: name }, run_count: 1, is_first_sync: true, started_at: "2026-01-01T00:00:00Z", completed_at: "2026-01-01T00:00:01Z" });
    assert.equal(r.status, 200);
    assert.equal(r.body.ignored, "repo.sync.succeeded");

    // a push to an untracked branch: acked, not interpreted
    r = await deliverWebhook(w, name, resp.webhookSecret, "push", pushPayload(name, "refs/heads/some-preview", "0".repeat(40), "b".repeat(40)));
    assert.equal(r.status, 200);
    assert.equal(r.body.interpreted, false);

    assert.equal(gp.pinOf(w.cell(name), "main"), null);
  } finally {
    await w.stop();
  }
});
