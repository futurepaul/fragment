// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { schnorr } from "@noble/curves/secp256k1.js";
import { npubFromHex, hexFromNpub } from "./bech32.js";
import { json, randHex, randSlug } from "./util.js";
import { normalizeManifest } from "./manifest.js";
import { ensureRepo, CodeStorageError } from "./codestorage.js";
import { wrapSecret } from "./secretwrap.js";
const NAME_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
async function initCell(cell, request) {
  if (cell.getMeta("name")) return json({ ok: true, already: true });
  const { name, ownerHex, fragmentSecret } = await request.json();
  if (!NAME_RE.test(name)) return json({ error: "bad name" }, 400);
  if (typeof fragmentSecret !== "string" || !/^[0-9a-f]{64}$/.test(fragmentSecret)) {
    return json({ error: "fragmentSecret required: 64-hex secp256k1 secret generated client-side (the CLI does this)" }, 400);
  }
  const hostSecret = String(cell.env.FRAGMENT_HOST_SECRET || "");
  if (!hostSecret) {
    return json({ error: "FRAGMENT_HOST_SECRET is not set on this host \u2014 wrapped secret storage requires it (set CELLD_VAR_FRAGMENT_HOST_SECRET)" }, 500);
  }
  let pubHex;
  try {
    const sk = Uint8Array.from(fragmentSecret.match(/.{2}/g).map((b) => parseInt(b, 16)));
    pubHex = [...schnorr.getPublicKey(sk)].map((b) => b.toString(16).padStart(2, "0")).join("");
  } catch (e) {
    return json({ error: `fragmentSecret is not a usable secp256k1 scalar: ${String(e)}` }, 400);
  }
  const npub = npubFromHex(pubHex);
  let repoUrl;
  try {
    repoUrl = await ensureRepo(cell.env, name);
  } catch (e) {
    if (e instanceof CodeStorageError && e.kind === "not-configured") {
      return json({ error: e.message }, 500);
    }
    return json({ error: `code.storage repo create failed: ${String(e.message || e)}` }, 502);
  }
  cell.setMeta("name", name);
  cell.setMeta("owner", ownerHex);
  cell.setMeta("fragment_secret", await wrapSecret(hostSecret, npub, fragmentSecret));
  cell.setMeta("fragment_npub", npub);
  cell.setMeta("view_token", randSlug(12));
  cell.setMeta("inbox_token", randHex(16));
  cell.setMeta("webhook_secret", randHex(16));
  cell.setMeta("cs_repo", repoUrl);
  cell.setMeta("manifest", JSON.stringify(normalizeManifest({
    name,
    visibility: "link",
    editors: [],
    viewers: [],
    workflows: [],
    secrets: []
  }).manifest));
  cell.addEvent("create", `fragment ${name} created (repo ${repoUrl}, npub secret supplied client-side, stored wrapped)`);
  return json({
    ok: true,
    npub,
    viewToken: cell.getMeta("view_token"),
    inboxToken: cell.getMeta("inbox_token"),
    webhookSecret: cell.getMeta("webhook_secret"),
    repo: repoUrl
  });
}
async function registryRoute(cell, request, url) {
  const p = url.pathname;
  if (p === "/__registry/create" && request.method === "POST") {
    const { name, ownerHex } = await request.json();
    if (!NAME_RE.test(name) || name.startsWith("_")) return json({ error: "bad name (lowercase, digits, dashes; 2-32 chars; no leading _)" }, 400);
    const exists = cell.sql.exec("SELECT name FROM fragments WHERE name = ?", name).toArray()[0];
    if (exists) return json({ error: `name taken: ${name}` }, 409);
    cell.sql.exec("INSERT INTO fragments (name, owner, created_at) VALUES (?, ?, ?)", name, ownerHex, Date.now());
    cell.sql.exec("INSERT INTO roles (name, pubkey, role) VALUES (?, ?, 'owner')", name, ownerHex);
    return json({ ok: true });
  }
  if (p === "/__registry/list-all") {
    const rows = cell.sql.exec("SELECT name, created_at FROM fragments ORDER BY created_at DESC").toArray();
    return json({ fragments: rows.map((r) => ({ name: r.name, createdAt: r.created_at })) });
  }
  if (p === "/__registry/role") {
    const name = url.searchParams.get("name") || "";
    const pubkey = url.searchParams.get("pubkey") || "";
    const row = cell.sql.exec("SELECT role FROM roles WHERE name = ? AND pubkey = ?", name, pubkey).toArray()[0];
    return json({ role: row ? row.role : null });
  }
  if (p === "/__registry/delete" && request.method === "POST") {
    const { name } = await request.json();
    if (!NAME_RE.test(name)) return json({ error: "bad name" }, 400);
    cell.sql.exec("DELETE FROM fragments WHERE name = ?", name);
    cell.sql.exec("DELETE FROM roles WHERE name = ?", name);
    return json({ ok: true });
  }
  if (p === "/__registry/list") {
    const pk = url.searchParams.get("pubkey");
    const rows = cell.sql.exec(
      "SELECT r.name, r.role, f.created_at FROM roles r JOIN fragments f ON f.name = r.name WHERE r.pubkey = ?",
      pk || ""
    ).toArray();
    return json({ fragments: rows.map((r) => ({ name: r.name, role: r.role })) });
  }
  if (p === "/__registry/roles-sync" && request.method === "POST") {
    const { name, owner, editors, viewers } = await request.json();
    cell.sql.exec("DELETE FROM roles WHERE name = ?", name);
    const add = (npub, role) => {
      try {
        cell.sql.exec("INSERT INTO roles (name, pubkey, role) VALUES (?, ?, ?) ON CONFLICT DO NOTHING", name, hexFromNpub(npub), role);
      } catch {
      }
    };
    try {
      cell.sql.exec("INSERT INTO roles (name, pubkey, role) VALUES (?, ?, 'owner') ON CONFLICT DO NOTHING", name, owner);
    } catch {
    }
    (editors || []).forEach((n) => add(n, "editor"));
    (viewers || []).forEach((n) => add(n, "viewer"));
    return json({ ok: true });
  }
  return new Response("not found", { status: 404 });
}
async function syncRolesToRegistry(cell) {
  const m = cell.manifest();
  await cell.env.FRAGMENT.getByName("_registry").fetch("http://x/__registry/roles-sync", {
    method: "POST",
    body: JSON.stringify({ name: m.name, owner: cell.getMeta("owner"), editors: m.editors, viewers: m.viewers })
  });
}
export {
  initCell,
  registryRoute,
  syncRolesToRegistry
};
