// secretwrap.test — level-c: secrets are wrapped at rest (HKDF+AES-GCM
// under FRAGMENT_HOST_SECRET, salted by the fragment npub), the npub
// secret arrives from the client, and unwrapping fails loudly on the
// wrong key rather than serving garbage.
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld } from "./harness.mjs";

test("wrap/unwrap roundtrip; wrong host secret refuses; per-fragment salting", async () => {
  const sw = await import("../src/secretwrap.js");
  const npub = "npub1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqesuvyx";
  const secret = "GRAFANA_TOKEN value with unicode ✓ and newlines\n";
  const wrapped = await sw.wrapSecret("host-secret-a", npub, secret);
  assert.notEqual(wrapped, secret);
  assert.ok(!wrapped.includes("GRAFANA"), "ciphertext must not contain plaintext");
  assert.equal(await sw.unwrapSecret("host-secret-a", npub, wrapped), secret);

  // wrong host secret: typed failure, never silent plaintext
  await assert.rejects(() => sw.unwrapSecret("host-secret-b", npub, wrapped), sw.SecretWrapError);
  // wrong salt (another fragment's npub): also refuses — per-fragment keys
  await assert.rejects(() => sw.unwrapSecret("host-secret-a", npub.replace("q", "w"), wrapped), sw.SecretWrapError);
  // empty host secret: refuses to wrap at all
  await assert.rejects(() => sw.wrapSecret("", npub, secret), /FRAGMENT_HOST_SECRET/);
  // deterministic nonce never: two wraps differ
  assert.notEqual(await sw.wrapSecret("host-secret-a", npub, secret), wrapped);
});

test("create stores the CLIENT-supplied npub secret wrapped; secrets API wraps values at rest", async () => {
  const w = await makeWorld();
  try {
    const clientSecret = "1".repeat(64); // client-generated secp256k1 scalar
    const { resp } = await w.makeFragment("sec1", "aa".repeat(32), clientSecret);
    assert.ok(resp.ok, JSON.stringify(resp));
    const cell = w.cell("sec1");

    // stored wrapped: the raw hex never appears; unwrap recovers it
    const stored = cell.getMeta("fragment_secret");
    assert.notEqual(stored, clientSecret);
    assert.ok(!stored.includes("1111"));
    const sw = await import("../src/secretwrap.js");
    assert.equal(await sw.unwrapSecret(w.hostSecret, cell.getMeta("fragment_npub"), stored), clientSecret);

    // a secret set through the API lands wrapped in the table
    const r = await cell.fetch("http://x/api/secrets/MY_KEY", {
      method: "PUT", headers: { "x-fragment-pubkey": "aa".repeat(32) }, body: "super-secret-value",
    });
    assert.equal(r.status, 200);
    const row = cell.sql.exec("SELECT value FROM secrets WHERE name = 'MY_KEY'").toArray()[0];
    assert.notEqual(row.value, "super-secret-value");
    assert.equal(await sw.unwrapSecret(w.hostSecret, cell.getMeta("fragment_npub"), row.value), "super-secret-value");

    // names listing never leaks values
    const names = await (await cell.fetch("http://x/api/secrets", { headers: { "x-fragment-pubkey": "aa".repeat(32) } })).json();
    assert.deepEqual(names, { names: ["MY_KEY"] });

    // create REFUSES a missing/invalid client secret (no server-side
    // generation path — the hard cut)
    const bad = await w.makeFragment("sec2", "aa".repeat(32), "not-hex");
    assert.ok(!bad.resp.ok, JSON.stringify(bad.resp));
    assert.match(String(bad.resp.error), /fragmentSecret/);
  } finally {
    await w.stop();
  }
});
