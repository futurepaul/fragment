// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { schnorr } from "@noble/curves/secp256k1.js";
import { npubFromHex, hexFromNpub } from "./bech32.js";
import { json, randHex, randSlug } from "./util.js";
import { normalizeManifest } from "./manifest.js";
const NAME_RE = /^[a-z0-9][a-z0-9-]{0,31}$/;
async function initCell(cell, request) {
  if (cell.getMeta("name")) return json({ ok: true, already: true });
  const { name, ownerHex } = await request.json();
  const secretKey = randHex(32);
  const pubHex = [...schnorr.getPublicKey(Uint8Array.from(secretKey.match(/.{2}/g).map((b) => parseInt(b, 16))))].map((b) => b.toString(16).padStart(2, "0")).join("");
  cell.setMeta("name", name);
  cell.setMeta("owner", ownerHex);
  cell.setMeta("fragment_secret", secretKey);
  cell.setMeta("fragment_npub", npubFromHex(pubHex));
  cell.setMeta("view_token", randSlug(12));
  cell.setMeta("inbox_token", randHex(16));
  cell.setMeta("rev", "0");
  cell.setMeta("manifest", JSON.stringify(normalizeManifest({
    name,
    visibility: "link",
    editors: [],
    viewers: [],
    workflows: [],
    secrets: []
  }).manifest));
  cell.addEvent("create", `fragment ${name} created`);
  return json({ ok: true, npub: cell.getMeta("fragment_npub"), viewToken: cell.getMeta("view_token"), inboxToken: cell.getMeta("inbox_token") });
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
    cell.sql.exec("DELETE FROM slugs WHERE name = ?", name);
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
  if (p === "/__registry/slug-map" && request.method === "POST") {
    const { slug, name } = await request.json();
    cell.sql.exec("INSERT INTO slugs (slug, name) VALUES (?, ?) ON CONFLICT(slug) DO NOTHING", slug, name);
    return json({ ok: true });
  }
  if (p === "/__registry/slug") {
    const slug = url.searchParams.get("s");
    const row = cell.sql.exec("SELECT name FROM slugs WHERE slug = ?", slug || "").toArray()[0];
    return row ? json({ name: row.name }) : json({ error: "unknown draft" }, 404);
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
