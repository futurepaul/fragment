// Shared cell utilities: response/json helpers, random ids, MIME, schema.

// Blob-tier hard cut (docs/blob-tier.md "Runtime two-tier model"): cells
// order NAMES (path -> sha256) and never store bodies. `files` and
// `draft_files` carry metadata only; bytes live in blobsd addressed by
// content hash. The old body columns are GONE — an AddColumnIfMissing shim
// cannot express a column-type drop, and shimming the old shape is exactly
// what the spec forbids ("nothing shims the old column").
// v4 adds push_subs (Web Push subscriptions, endpoint-keyed — see
// internal.ts push/* routes). Additive only: the table ships in SCHEMA for
// cells born on v4, and the push routes CREATE TABLE IF NOT EXISTS it
// lazily so cells created earlier pick it up on first touch (cell.ts's
// version-mismatch branch does column adds only and can't mint tables).
export const SCHEMA_VERSION = 4;
export const SCHEMA = `
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
CREATE TABLE IF NOT EXISTS push_subs (who TEXT, endpoint TEXT PRIMARY KEY, p256dh TEXT, auth TEXT, at INTEGER, fails INTEGER DEFAULT 0);
`;

// One bound for every body a cell will accept. Cell content lives in
// SQLite and replicates as LTX WAL frames: big blobs amplify replication
// traffic, restore cost, and write-ack latency, so cells hold documents,
// not media. Decimal-1MB sits under the host's 1MiB ingress stream
// threshold with headroom.
export const MAX_BODY_BYTES = 1_000_000;

export function bodyTooLarge(request) {
  const len = parseInt(request.headers.get("content-length") || "0", 10);
  return len > MAX_BODY_BYTES;
}

export const MIME = {
  html: "text/html; charset=utf-8", htm: "text/html; charset=utf-8",
  css: "text/css", js: "text/javascript",
  mjs: "text/javascript", json: "application/json",
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
  svg: "image/svg+xml", ico: "image/x-icon", webp: "image/webp",
  txt: "text/plain; charset=utf-8", md: "text/markdown; charset=utf-8",
  wasm: "application/wasm", pdf: "application/pdf", xml: "application/xml",
  csv: "text/csv; charset=utf-8",
  // fonts + web-app manifest (blob-tier hosting conventions, docs/blob-tier.md):
  // a vault or gallery that drops in woff/ttf still serves the right type
  woff2: "font/woff2", woff: "font/woff", ttf: "font/ttf", otf: "font/otf",
  webmanifest: "application/manifest+json",
};

// True when the basename carries a content-hash segment. The documented
// convention is `name.<8-hex>.ext` ("app.9f2c1e8b.js", "v-1b2c3d44.png");
// the guide's example spelling ("style-a.1b2c3d4.css") uses a 7-hex dot-
// delimited segment, and the dash convention ("-1b2c3d4f.") shows up too —
// all mark the same thing: the name embeds a content-derived hash, so it
// can never collide across deploys and is safe to cache forever.
export function hashInName(path) {
  const base = String(path).split("/").pop() || "";
  return /[0-9a-f]{8}\./i.test(base) || /\.[0-9a-f]{7,8}\./i.test(base);
}

// Cache-control per docs/blob-tier.md web-hosting conventions: blessed
// snapshots default max-age=300; hash-named assets are immutable; drafts are
// never cached (a preview slug is a rehearsal, not a URL anyone should keep).
export function serveCacheControl(blessed, path) {
  if (!blessed) return "no-store";
  return hashInName(path) ? "public, max-age=31536000, immutable" : "public, max-age=300";
}

export function mimeForPath(path) {
  const ext = (String(path).match(/\.([a-z0-9]+)$/) || [])[1] || "";
  return MIME[ext] || "";
}

export const rankOf = (r) => ({ owner: 3, editor: 2, viewer: 1 })[r] || 0;

// A fragment's own organs, not its content: manifests, app code, workflow
// code, vendored helpers. Tree/list responses mark these with
// `machinery: true` so watchers (feeds, pollers, indexes) don't have to
// each re-derive an exclusion list — and each get it subtly wrong.
export const MACHINERY = ["/fragment.json", "/app.mjs", "/rooms.mjs", "/_index.md"];
export function isMachinery(path) {
  if (MACHINERY.includes("/" + path)) return true;
  return /^(workflows|applib|lib|node_modules)\/|^\./.test(path);
}

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
  // API and tree responses are per-request views over mutable cell state;
  // browsers must never heuristically cache them (a cached __tree makes
  // polling clients go blind to rev changes until a hard reload)
  return new Response(JSON.stringify(data), { status, headers: { "content-type": "application/json", "cache-control": "no-store", ...headers } });
}
