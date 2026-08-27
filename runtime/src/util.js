// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
const SCHEMA_VERSION = 3;
const SCHEMA = `
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT);
CREATE TABLE IF NOT EXISTS files (path TEXT PRIMARY KEY, sha256 TEXT NOT NULL, size INTEGER NOT NULL, mime TEXT NOT NULL DEFAULT '', rev INTEGER NOT NULL, updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS file_revisions (path TEXT NOT NULL, rev INTEGER NOT NULL, blob_hash TEXT, deleted INTEGER DEFAULT 0, at INTEGER, PRIMARY KEY (path, rev));
CREATE TABLE IF NOT EXISTS drafts (slug TEXT PRIMARY KEY, at INTEGER, note TEXT, blessed INTEGER DEFAULT 0);
CREATE TABLE IF NOT EXISTS draft_files (slug TEXT, path TEXT, sha256 TEXT NOT NULL, size INTEGER NOT NULL, mime TEXT NOT NULL DEFAULT '', rev INTEGER NOT NULL, updated_at INTEGER NOT NULL, deleted INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (slug, path));
CREATE TABLE IF NOT EXISTS secrets (name TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS inbox (id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER, source TEXT, payload TEXT, status TEXT DEFAULT 'pending', claimed_at INTEGER, claim_token TEXT);
CREATE TABLE IF NOT EXISTS events (id INTEGER PRIMARY KEY AUTOINCREMENT, at INTEGER, kind TEXT, summary TEXT, data TEXT);
CREATE TABLE IF NOT EXISTS wstate (scope TEXT, k TEXT, v TEXT, PRIMARY KEY (scope, k));
CREATE TABLE IF NOT EXISTS rooms (room TEXT PRIMARY KEY, state TEXT);
CREATE TABLE IF NOT EXISTS room_msgs (id INTEGER PRIMARY KEY AUTOINCREMENT, room TEXT, at INTEGER, sender TEXT, data TEXT);
CREATE TABLE IF NOT EXISTS run_tokens (token TEXT PRIMARY KEY, scope TEXT, expires INTEGER);
CREATE TABLE IF NOT EXISTS runs (id INTEGER PRIMARY KEY AUTOINCREMENT, wf TEXT, via TEXT, status TEXT, input TEXT, cause TEXT, error TEXT, attempt INTEGER DEFAULT 1, max_attempts INTEGER DEFAULT 1, started_at INTEGER, next_attempt_at INTEGER, finished_at INTEGER, duration_ms INTEGER);
CREATE TABLE IF NOT EXISTS notify_outbox (url TEXT PRIMARY KEY, paths TEXT, attempts INTEGER DEFAULT 0, next_at INTEGER);
CREATE TABLE IF NOT EXISTS fragments (name TEXT PRIMARY KEY, owner TEXT, created_at INTEGER);
CREATE TABLE IF NOT EXISTS roles (name TEXT, pubkey TEXT, role TEXT, PRIMARY KEY (name, pubkey));
CREATE TABLE IF NOT EXISTS slugs (slug TEXT PRIMARY KEY, name TEXT);
`;
const MAX_BODY_BYTES = 1e6;
function bodyTooLarge(request) {
  const len = parseInt(request.headers.get("content-length") || "0", 10);
  return len > MAX_BODY_BYTES;
}
const MIME = {
  html: "text/html; charset=utf-8",
  htm: "text/html; charset=utf-8",
  css: "text/css",
  js: "text/javascript",
  mjs: "text/javascript",
  json: "application/json",
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  svg: "image/svg+xml",
  ico: "image/x-icon",
  webp: "image/webp",
  txt: "text/plain; charset=utf-8",
  md: "text/markdown; charset=utf-8",
  wasm: "application/wasm",
  pdf: "application/pdf",
  xml: "application/xml",
  csv: "text/csv; charset=utf-8",
  // fonts + web-app manifest (blob-tier hosting conventions, docs/blob-tier.md):
  // a vault or gallery that drops in woff/ttf still serves the right type
  woff2: "font/woff2",
  woff: "font/woff",
  ttf: "font/ttf",
  otf: "font/otf",
  webmanifest: "application/manifest+json"
};
function hashInName(path) {
  const base = String(path).split("/").pop() || "";
  return /[0-9a-f]{8}\./i.test(base) || /\.[0-9a-f]{7,8}\./i.test(base);
}
function serveCacheControl(blessed, path) {
  if (!blessed) return "no-store";
  return hashInName(path) ? "public, max-age=31536000, immutable" : "public, max-age=300";
}
function mimeForPath(path) {
  const ext = (String(path).match(/\.([a-z0-9]+)$/) || [])[1] || "";
  return MIME[ext] || "";
}
const rankOf = (r) => ({ owner: 3, editor: 2, viewer: 1 })[r] || 0;
const MACHINERY = ["/fragment.json", "/app.mjs", "/rooms.mjs", "/_index.md"];
function isMachinery(path) {
  if (MACHINERY.includes("/" + path)) return true;
  return /^(workflows|applib|lib|node_modules)\/|^\./.test(path);
}
const SLUG_CHARS = "abcdefghijklmnopqrstuvwxyz0123456789";
function randHex(bytes) {
  const b = new Uint8Array(bytes);
  crypto.getRandomValues(b);
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}
function randSlug(n = 8) {
  let s = "";
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  for (const x of b) s += SLUG_CHARS[x % SLUG_CHARS.length];
  return s;
}
function toAB(x) {
  if (x == null) return new ArrayBuffer(0);
  if (x instanceof ArrayBuffer) return x;
  if (x instanceof Uint8Array) return x.buffer.slice(x.byteOffset, x.byteOffset + x.byteLength);
  if (typeof x === "string") return new TextEncoder().encode(x).buffer;
  throw new Error("unexpected storage value type: " + typeof x);
}
function json(data, status = 200, headers = {}) {
  return new Response(JSON.stringify(data), { status, headers: { "content-type": "application/json", ...headers } });
}
export {
  MACHINERY,
  MAX_BODY_BYTES,
  MIME,
  SCHEMA,
  SCHEMA_VERSION,
  bodyTooLarge,
  hashInName,
  isMachinery,
  json,
  mimeForPath,
  randHex,
  randSlug,
  rankOf,
  serveCacheControl,
  toAB
};
