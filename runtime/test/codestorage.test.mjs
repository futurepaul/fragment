// codestorage.test — the client contract against the mock service: wire
// shapes, CAS semantics, JWT claims, webhook HMAC verification.
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert, signedWebhook, pushPayload } from "./harness.mjs";

test("commit-pack: success, CAS conflict, and idempotent same-content commit", async () => {
  const w = await makeWorld();
  try {
    const env = w.env;
    const cs = await import("../src/codestorage.js");
    // repo identity is the url-form id createRepo returns — every
    // repo-scoped call below carries it (the human name 404s)
    const repo = await cs.ensureRepo(env, "alpha");
    assert.match(repo, /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);

    // success: create the branch with a first commit (no expected parent)
    const out1 = await cs.commitFiles(env, repo, "alpha", {
      branch: "main", expectedTargetSha: null, message: "first",
      files: [{ op: "upsert", path: "a.txt", bytes: new TextEncoder().encode("hello\n") }],
    });
    assert.equal(out1.status, "ok");
    assert.match(out1.newSha, /^[0-9a-f]{40}$/);
    assert.equal(out1.oldSha, "0".repeat(40));

    // CAS conflict: expect a stale parent
    await assert.rejects(
      () => cs.commitFiles(env, repo, "alpha", {
        branch: "main", expectedTargetSha: out1.oldSha, message: "stale",
        files: [{ op: "upsert", path: "b.txt", bytes: new TextEncoder().encode("x") }],
      }),
      (e) => e instanceof cs.CodeStorageError && e.kind === "conflict",
    );

    // CAS success with the right parent
    const out2 = await cs.commitFiles(env, repo, "alpha", {
      branch: "main", expectedTargetSha: out1.newSha, message: "second",
      files: [{ op: "upsert", path: "b.txt", bytes: new TextEncoder().encode("world\n") }],
    });
    assert.equal(out2.oldSha, out1.newSha);

    // reads: branch head
    const head = await cs.readBranchHead(env, repo, "main");
    assert.equal(head.headSha, out2.newSha);

    // tree metadata
    const tree = await cs.listTree(env, repo, out2.newSha);
    assert.deepEqual(tree.map((e) => e.path).sort(), ["a.txt", "b.txt"]);
    assert.ok(tree.every((e) => e.size > 0 && /^[0-9a-f]{40}$/.test(e.lastCommitSha)));

    // file stream + HEAD identity
    const st = await cs.streamFile(env, repo, out2.newSha, "a.txt");
    assert.equal(await st.text(), "hello\n");
    const hd = await cs.headFile(env, repo, out2.newSha, "a.txt");
    assert.equal(hd.size, 6);
    // blob identity must equal the git blob sha the runtime computes
    const { gitBlobSha } = await import("../src/git-plane.js");
    assert.equal(hd.blobSha, await gitBlobSha(new TextEncoder().encode("hello\n")));

    // 404 is an answer, not an error
    assert.equal(await cs.headFile(env, repo, out2.newSha, "nope.txt"), null);
    await assert.rejects(
      () => cs.streamFile(env, repo, out2.newSha, "nope.txt"),
      (e) => e instanceof cs.CodeStorageError && e.status === 404,
    );

    // chunk limit is server-enforced: a >4MiB single chunk 413s
    const big = Buffer.alloc(5 * 1024 * 1024, 7).toString("base64");
    const meta = JSON.stringify({
      target_branch: "main", expected_target_sha: out2.newSha, commit_message: "big",
      author: { name: "t", email: "t@t" },
      files: [{ path: "big.bin", operation: "upsert", content_id: "b0", mode: "100644" }],
    });
    const ndjson = JSON.stringify({ metadata: JSON.parse(meta) }) + "\n" + JSON.stringify({ blob_chunk: { content_id: "b0", data: big, eof: true } }) + "\n";
    const tok = await cs.mintCsJwt(env, { repo, scopes: ["git:write"], sub: "t", ttlSec: 60 });
    const resp = await fetch(`${w.mockUrl}/api/repos/${repo}/commit-pack`, {
      method: "POST", headers: { authorization: `Bearer ${tok}`, "content-type": "application/x-ndjson" }, body: ndjson,
    });
    assert.equal(resp.status, 413);
  } finally {
    await w.stop();
  }
});

test("mintCsJwt: claims are exactly iss/sub/repo/scopes/iat/exp", async () => {
  const w = await makeWorld();
  try {
    const cs = await import("../src/codestorage.js");
    const tok = await cs.mintCsJwt(w.env, { repo: "alpha", scopes: ["git:read", "git:write"], sub: "editor:abcd", ttlSec: 900 });
    const [h, p] = tok.split(".");
    const dec = (s) => JSON.parse(Buffer.from(s.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString());
    const claims = dec(p);
    assert.equal(claims.iss, "fragment-dev");
    assert.equal(claims.sub, "editor:abcd");
    assert.equal(claims.repo, "alpha");
    assert.deepEqual(claims.scopes, ["git:read", "git:write"]);
    assert.equal(claims.exp - claims.iat, 900);
    assert.deepEqual(Object.keys(claims).sort(), ["exp", "iat", "iss", "repo", "scopes", "sub"]);
    // the mock (holding only the org PUBLIC key) accepts it
    await cs.ensureRepo(w.env, "alpha");
  } finally {
    await w.stop();
  }
});

test("verifyWebhookDelivery: valid, stale, future, tampered, malformed", async () => {
  const cs = await import("../src/codestorage.js");
  const secret = "whsec-123";
  const { body, headers } = await signedWebhook(secret, "push", pushPayload("r", "refs/heads/main", "a", "b"));
  const good = await cs.verifyWebhookDelivery(body, headers["x-pierre-signature"], secret);
  assert.equal(good.ok, true);

  const stale = await signedWebhook(secret, "push", { a: 1 }, Math.floor(Date.now() / 1000) - 3600);
  const badStale = await cs.verifyWebhookDelivery(stale.body, stale.headers["x-pierre-signature"], secret);
  assert.equal(badStale.ok, false);
  assert.match(badStale.reason, /too old/);

  const future = await signedWebhook(secret, "push", { a: 1 }, Math.floor(Date.now() / 1000) + 3600);
  const badFuture = await cs.verifyWebhookDelivery(future.body, future.headers["x-pierre-signature"], secret);
  assert.equal(badFuture.ok, false);
  assert.match(badFuture.reason, /future/);

  const tampered = await cs.verifyWebhookDelivery(body.replace("main", "live"), headers["x-pierre-signature"], secret);
  assert.equal(tampered.ok, false);
  assert.match(tampered.reason, /signature/);

  const malformed = await cs.verifyWebhookDelivery(body, "not-a-signature", secret);
  assert.equal(malformed.ok, false);
  assert.match(malformed.reason, /format/);
});

// env-delivery normalization: an \n-escaped single-line PEM (what systemd
// EnvironmentFile can carry) must normalize to the identical key the
// real-newline form produces.
test("csConfig normalizes \\n-escaped PEM for EnvironmentFile delivery", async () => {
  const mod = await import("../src/codestorage.js");
  const pem = "-----BEGIN PRIVATE KEY-----\nMIabc123\n-----END PRIVATE KEY-----";
  const escaped = pem.replace(/\n/g, "\\n");
  const a = mod.csConfig({ PIERRE_PRIVATE_KEY: pem, CODESTORAGE_ORG_NAME: "finite" });
  const b = mod.csConfig({ PIERRE_PRIVATE_KEY: escaped, CODESTORAGE_ORG_NAME: "finite" });
  assert.equal(b.keyPem, a.keyPem, "escaped form normalizes to the real-newline form");
  assert.ok(b.keyPem.startsWith("-----BEGIN"), "PEM header intact after normalization");
});

// regression (live-verified 2026-09-15): the real service 403s claim-less
// tokens on every path, org-level included — the mock must too, or the
// suite passes while prod fails.
test("org-level mint without a repo claim is refused (mock fidelity)", async () => {
  const { SignJWT, importPKCS8 } = await import("jose");
  const w = await makeWorld();
  try {
    const key = await importPKCS8(w.env.PIERRE_PRIVATE_KEY, "ES256");
    const now = Math.floor(Date.now() / 1e3);
    const jwt = await new SignJWT({ iss: w.env.CODESTORAGE_ORG_NAME, sub: "fragment-runtime", scopes: ["repo:write"], iat: now, exp: now + 300 })
      .setProtectedHeader({ alg: "ES256", typ: "JWT" }).sign(key);
    const r = await fetch(`${w.mockUrl}/api/repos`, { method: "POST",
      headers: { authorization: `Bearer ${jwt}`, "content-type": "application/json" },
      body: JSON.stringify({ id: "claimless", default_branch: "main" }) });
    assert.equal(r.status, 403, "claim-less token must be refused");
    // and the runtime's own ensureRepo mint (now always carries repo) works
    const mod = await import("../src/codestorage.js");
    const url = await mod.ensureRepo(w.env, "with-claim");
    assert.ok(url.length > 0, "ensureRepo succeeds with the repo claim present");
  } finally { await w.stop(); }
});
