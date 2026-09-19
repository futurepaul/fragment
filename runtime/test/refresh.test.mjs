// refresh.test — POST /api/f/{"name"}/refresh is the authenticated
// equivalent of a push webhook's interpret step: editor+ NIP-98, re-reads
// both refs, moves the pins, and (for external commits) schedules the
// files triggers exactly like a delivery would. The CLI fires it after
// landing commits so its own pushes are visible immediately instead of
// waiting out the 5-minute poll backstop.
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert } from "./harness.mjs";

const OWNER = "aa".repeat(32);
const EDITOR_HEX = "bb".repeat(32);
const VIEWER_HEX = "cc".repeat(32);

async function setup(w, name) {
  const { npubFromHex } = await import("../src/bech32.js");
  const editorNpub = npubFromHex(EDITOR_HEX);
  const viewerNpub = npubFromHex(VIEWER_HEX);
  await w.makeFragment(name, OWNER);
  await w.mock.externalCommit(name, [upsert("fragment.json", JSON.stringify({
    name, visibility: "public",
    editors: [editorNpub], viewers: [viewerNpub],
  }))]);
  const gp = await import("../src/git-plane.js");
  await gp.refreshPin(w.cell(name), "main");
  await gp.refreshPin(w.cell(name), "live").catch(() => {}); // absent → stays empty
}

const refresh = (w, name, pubkey) =>
  w.env.FRAGMENT.getByName(name).fetch(`http://x/api/refresh`, {
    method: "POST",
    headers: pubkey ? { "x-fragment-pubkey": pubkey } : {},
  });

test("positive: refresh moves a stale main pin and reports an absent live ref", async () => {
  const w = await makeWorld();
  try {
    const name = "rf1";
    await setup(w, name);
    const gp = await import("../src/git-plane.js");

    // an external commit lands while the cell isn't looking (the state
    // the CLI's nudge exists to fix)
    const landed = await w.mock.externalCommit(name, [upsert("late.txt", "landed after the pin")]);
    const tip = landed.result.new_sha;
    const stalePin = gp.pinOf(w.cell(name), "main");
    assert.ok(stalePin && stalePin !== tip, "the pin is genuinely stale");

    const res = await refresh(w, name, EDITOR_HEX);
    assert.equal(res.status, 200);
    const body = await res.json();
    assert.equal(body.ok, true);
    assert.equal(body.refs.main.moved, true);
    assert.equal(body.refs.main.pin, tip, "pin caught up to the branch head");
    assert.equal(body.refs.live.absent, true, "no live ref yet is a report, not an error");
    assert.equal(await gp.readFileText(w.cell(name), "late.txt"), "landed after the pin");

    // external-push interpretation: the files trigger is scheduled (what
    // a webhook delivery would have done)
    assert.ok(parseInt(w.cell(name).getMeta("sync_trigger_at") || "0", 10) > 0, "files trigger scheduled");

    // idempotent re-refresh: nothing moving is a quiet success
    const res2 = await refresh(w, name, EDITOR_HEX);
    assert.equal(res2.status, 200);
    assert.equal((await res2.json()).refs.main.moved, false);
  } finally {
    await w.stop();
  }
});

test("negative: refresh is editor+ — viewer 403, anonymous 401", async () => {
  const w = await makeWorld();
  try {
    const name = "rf2";
    await setup(w, name);
    const viewer = await refresh(w, name, VIEWER_HEX);
    assert.equal(viewer.status, 403);
    const anon = await refresh(w, name, null);
    assert.equal(anon.status, 401);
  } finally {
    await w.stop();
  }
});
