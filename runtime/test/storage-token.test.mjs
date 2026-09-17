// storage-token.test — the mint hardening checkpoints: editor+ only,
// server-built repo claim, git-only scopes, short expiry, ledger audit,
// and the pinned response shape {token, repo, api} the CLI consumes.
import { test } from "node:test";
import assert from "node:assert/strict";
import { makeWorld, upsert } from "./harness.mjs";

const OWNER = "aa".repeat(32);
const EDITOR_HEX = "bb".repeat(32);
const VIEWER_HEX = "cc".repeat(32);

function decodeJwtPayload(tok) {
  const p = tok.split(".")[1];
  return JSON.parse(Buffer.from(p.replace(/-/g, "+").replace(/_/g, "/"), "base64").toString());
}

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
}

async function mint(w, name, pubkey) {
  return await w.env.FRAGMENT.getByName(name).fetch(`http://x/api/storage-token`, {
    method: "GET",
    headers: pubkey ? { "x-fragment-pubkey": pubkey } : {},
  });
}

test("viewer is rejected; editor mints exact-claim short-lived token; mint is audited", async () => {
  const w = await makeWorld();
  try {
    const name = "tok1";
    await setup(w, name);

    // anonymous → 401; viewer → 403, no token in the body
    const anon = await mint(w, name, null);
    assert.equal(anon.status, 401);
    const viewer = await mint(w, name, VIEWER_HEX);
    assert.equal(viewer.status, 403);
    assert.equal((await viewer.json()).token, undefined);

    // before the mint: no audit rows
    assert.equal(w.cell(name).sql.exec("SELECT COUNT(*) c FROM events WHERE kind = 'storage-token.minted'").toArray()[0].c, 0);

    const r = await mint(w, name, EDITOR_HEX);
    assert.equal(r.status, 200);
    const body = await r.json();
    // the pinned response shape — the CLI's only counterparty
    assert.deepEqual(Object.keys(body).sort(), ["api", "repo", "token"]);
    // repo identity is the URL-FORM id createRepo returned (live-verified:
    // the human name 404s on repo-scoped calls), recorded in the cell
    assert.equal(body.repo, w.cell(name).getMeta("cs_repo"));
    assert.match(body.repo, /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/, "repo is a url-form uuid, not the fragment name");
    assert.equal(body.api, w.mockUrl);

    const claims = decodeJwtPayload(body.token);
    assert.equal(claims.repo, body.repo, "repo claim is exactly the cell's recorded repo identity");
    assert.deepEqual(claims.scopes, ["git:read", "git:write"], "git-only scopes — no org reach");
    assert.ok(claims.exp - claims.iat <= 900 + 1 && claims.exp - claims.iat > 0, "short expiry (minutes)");
    assert.equal(claims.sub, `editor:${EDITOR_HEX}`);
    assert.equal(claims.iss, "fragment-dev");

    // every mint writes the ledger event with actor + repo + scopes + expiry
    const ev = w.cell(name).sql.exec("SELECT data FROM events WHERE kind = 'storage-token.minted'").toArray();
    assert.equal(ev.length, 1);
    const data = JSON.parse(ev[0].data);
    assert.equal(data.actor, EDITOR_HEX);
    assert.equal(data.repo, body.repo);
    assert.deepEqual(data.scopes, ["git:read", "git:write"]);
    assert.ok(data.expiresAt > Date.now());

    // owner can mint too
    const owner = await mint(w, name, OWNER);
    assert.equal(owner.status, 200);
    assert.equal(w.cell(name).sql.exec("SELECT COUNT(*) c FROM events WHERE kind = 'storage-token.minted'").toArray()[0].c, 2);

    // the minted token actually works against code.storage (mock enforces
    // scope + repo claims against the url-form identity)
    const resp = await fetch(`${w.mockUrl}/api/repos/${body.repo}/branch?name=main`, {
      headers: { authorization: `Bearer ${body.token}` },
    });
    assert.equal(resp.status, 200);
    // identity discipline: the same call under the human name 404s
    const byName = await fetch(`${w.mockUrl}/api/repos/${name}/branch?name=main`, {
      headers: { authorization: `Bearer ${body.token}` },
    });
    assert.equal(byName.status, 404);
    // ...but is refused org-level (no org scope on the token)
    const orgResp = await fetch(`${w.mockUrl}/api/repos`, {
      method: "POST", headers: { authorization: `Bearer ${body.token}`, "content-type": "application/json" },
      body: JSON.stringify({ id: "sneaky" }),
    });
    assert.equal(orgResp.status, 403);
  } finally {
    await w.stop();
  }
});

// regression (live-found 2026-09-17): a fragment name whose minted-token
// payload needs base64 padding used to throw InvalidCharacterError in the
// egress assert (atob on unpadded base64url) — a coin flip per name.
test("storage-token mints for a padding-demanding name", async () => {
  const w = await makeWorld();
  try {
    const name = "tok-pad";
    await setup(w, name);
    const r = await mint(w, name, EDITOR_HEX);
    assert.equal(r.status, 200, "mint must survive padding-length payloads");
    const body = await r.json();
    assert.ok(body.token && body.repo && body.api);
  } finally { await w.stop(); }
});
