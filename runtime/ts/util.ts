// Shared cell utilities: response/json helpers, random ids, MIME, schema.

export const SCHEMA = `
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, content BLOB, rev INTEGER, sha256 TEXT, updated_at INTEGER, deleted INTEGER DEFAULT 0);
CREATE TABLE IF NOT EXISTS drafts (slug TEXT PRIMARY KEY, at INTEGER, note TEXT, blessed INTEGER DEFAULT 0);
CREATE TABLE IF NOT EXISTS draft_files (slug TEXT, path TEXT, content BLOB, sha256 TEXT, PRIMARY KEY (slug, path));
CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS inbox (id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER, source TEXT, payload TEXT, status TEXT DEFAULT 'pending');
CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER, kind TEXT, summary TEXT, data TEXT);
CREATE TABLE IF NOT EXISTS wstate (scope TEXT, k TEXT, v TEXT, PRIMARY KEY (scope, k));
CREATE TABLE IF NOT EXISTS rooms (room TEXT PRIMARY KEY, state TEXT);
CREATE TABLE IF NOT EXISTS room_msgs (id INTEGER PRIMARY KEY AUTOINCREMENT, room TEXT, at INTEGER, sender TEXT, data TEXT);
CREATE TABLE IF NOT EXISTS run_tokens (token TEXT PRIMARY KEY, scope TEXT, expires INTEGER);
CREATE TABLE IF NOT EXISTS runs (id INTEGER PRIMARY KEY AUTOINCREMENT, wf TEXT, via TEXT, status TEXT, input TEXT, cause TEXT, error TEXT, attempt INTEGER DEFAULT 1, max_attempts INTEGER DEFAULT 1, started_at INTEGER, next_attempt_at INTEGER, finished_at INTEGER, duration_ms INTEGER);
CREATE TABLE IF NOT EXISTS idem (key TEXT PRIMARY KEY, inbox_id INTEGER, at INTEGER);
CREATE TABLE IF NOT EXISTS fragments (name TEXT PRIMARY KEY, owner TEXT, created_at INTEGER);
CREATE TABLE IF NOT EXISTS roles (name TEXT, pubkey TEXT, role TEXT, PRIMARY KEY (name, pubkey));
CREATE TABLE IF NOT EXISTS slugs (slug TEXT PRIMARY KEY, name TEXT);
`;

export const MIME = {
  html: "text/html; charset=utf-8", htm: "text/html; charset=utf-8",
  css: "text/css", js: "text/javascript",
  mjs: "text/javascript", json: "application/json",
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
  svg: "image/svg+xml", ico: "image/x-icon", webp: "image/webp",
  txt: "text/plain; charset=utf-8", md: "text/markdown; charset=utf-8",
  wasm: "application/wasm", pdf: "application/pdf", xml: "application/xml",
  csv: "text/csv; charset=utf-8", woff2: "font/woff2",
};

export const rankOf = (r) => ({ owner: 3, editor: 2, viewer: 1 })[r] || 0;

const SLUG_CHARS = "abcdefghijklmnopqrstuvwxyz0123456789";

export function randHex(bytes) {
  const b = new Uint8Array(bytes);
  crypto.getRandomValues(b);
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

export function randSlug(n = 8) {
  let s = "";
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  for (const x of b) s += SLUG_CHARS[x % SLUG_CHARS.length];
  return s;
}

export function toAB(x: unknown): ArrayBuffer {
  if (x == null) return new ArrayBuffer(0);
  if (x instanceof ArrayBuffer) return x;
  if (x instanceof Uint8Array) return x.buffer.slice(x.byteOffset, x.byteOffset + x.byteLength) as ArrayBuffer;
  if (typeof x === "string") return new TextEncoder().encode(x).buffer as ArrayBuffer;
  throw new Error("unexpected storage value type: " + typeof x);
}

export function json(data, status = 200, headers = {}) {
  return new Response(JSON.stringify(data), { status, headers: { "content-type": "application/json", ...headers } });
}
