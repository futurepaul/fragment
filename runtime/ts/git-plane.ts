// The code.storage file plane (docs/ROADMAP.md "Wire contract" +
// "Truth map"). One module owns every byte of file-plane policy:
//
//   - TRUTH: code.storage git. The cell pins `main@SHA` for working-copy
//     reads and `live@SHA` for blessed serving.
//   - TREE INDEX (cell SQLite, metadata only — path/size/mode/
//     last_commit_sha per pinned SHA): a derived cache.
//       source of truth: the pinned SHA's tree at code.storage
//       invalidation: webhook push, 5-minute poll, our own commits
//       stale-read behavior: serve the pinned tree until a refresh lands
//     Hard rule (ROADMAP): NO file bytes ever persist in DO SQLite —
//     this table is names and sizes only.
//   - CONTENTS: streamed from code.storage on demand through a RAM LRU
//     (cap 64 MiB, explicit). The stream path NEVER buffers a whole
//     body and has NO size ceiling — files over the per-entry cap skip
//     the cache and pass through. streamFile() in codestorage.ts is the
//     single seam where a Range header could be forwarded later; nothing
//     here may assume whole-file responses.
//   - WRITES: server-side commit-pack with expected-parent CAS, bounded
//     retry, content dedup, and own-commit tracking so workflow writes
//     never re-trigger (loop safety is structural, not advisory).
import { json } from "./util.js";
import { normalizeManifest } from "./manifest.js";
import { watchBroadcast } from "./history.js";
import { enqueueNotify } from "./notify.js";
import {
  CodeStorageError, readBranchHead, listTree, streamFile, headFile, commitFiles,
} from "./codestorage.js";

// Whole-body decode ceiling for the TEXT materializers only (manifest,
// workflow/app source, ctx.files.read) — a giant .text() is exactly the
// heap shape this plane exists to avoid. The STREAM path has no ceiling.
export const READ_CEILING = 8 * 1024 * 1024;

// RAM LRU: total cap 64 MiB (ROADMAP), per-entry cap = READ_CEILING.
// Entries larger than the per-entry cap stream through uncached — one
// huge file must not evict the whole working set, and never fails.
export const LRU_TOTAL_BYTES = 64 * 1024 * 1024;
export const LRU_ENTRY_BYTES = READ_CEILING;

// Bound on one commit's file count: workflow writes are note-shaped, not
// corpus-shaped; a run trying to commit a whole tree in one turn is a
// bug we want to hear about, not a slow path we want to fund.
export const COMMIT_MAX_FILES = 64;

// CAS retry bound (ROADMAP: "conflict → refetch head, rebuild, retry
// (bounded, e.g. 3 attempts, explicit error after)"). Each attempt
// refetches head + per-path identity, so progress is guaranteed whenever
// the counterparty isn't writing faster than us; past this it's a real
// conflict for a human.
export const COMMIT_MAX_ATTEMPTS = 3;

// Tree-rebuild insert batch size. workerd's SQLite caps a statement at
// 100 BIND VARIABLES (probed: 100 ok, 125 fails — node:sqlite has no such
// cap, which is why the unit suite never saw it); 5 params per row means
// 20 rows per statement exactly fills the budget.
const TREE_INSERT_BATCH = 20;

// Reserved paths: platform-internal state never enters the repo, and the
// secrets-out-of-the-repo rule (ROADMAP truth map) is enforced here —
// secret VALUES live only in the wrapped secrets table. fragment.json is
// CLI-owned (manifest-set is an ordinary commit from the editor plane),
// so workflow writes refuse it: one authority per file.
const RESERVED_PATHS = new Set(["fragment.json", ".fragment/secrets.json"]);

// Webhook dedupe retention: 7 days by age, hard row cap for storms.
const WEBHOOK_DEDUPE_ROWS = 10_000;

// ---- the LRU itself ----
// Module-level by necessity (one per isolate; DOs share isolates), so
// every key carries fragment identity. Map preserves insertion order —
// delete+set on hit gives LRU; byte accounting evicts from the front.
const lru = new Map<string, Uint8Array>();
let lruBytes = 0;

function lruGet(key: string): Uint8Array | null {
  const hit = lru.get(key) || null;
  if (hit !== null) { lru.delete(key); lru.set(key, hit); } // refresh recency
  return hit;
}

function lruPut(key: string, bytes: Uint8Array): void {
  if (bytes.byteLength > LRU_ENTRY_BYTES) return; // too big to cache: streamed instead
  const prior = lru.get(key);
  if (prior) { lruBytes -= prior.byteLength; lru.delete(key); }
  lru.set(key, bytes);
  lruBytes += bytes.byteLength;
  while (lruBytes > LRU_TOTAL_BYTES && lru.size > 1) {
    const oldest = lru.keys().next().value as string;
    const b = lru.get(oldest);
    lru.delete(oldest);
    lruBytes -= b ? b.byteLength : 0;
  }
}

export function lruStats() {
  return { entries: lru.size, bytes: lruBytes, totalCap: LRU_TOTAL_BYTES };
}

// ---- pins ----

export function pinOf(cell, which: "main" | "live"): string | null {
  const v = cell.getMeta(`pin_${which}_sha`);
  return v && /^[0-9a-f]{40}$/.test(v) ? v : null;
}

export function repoOf(cell): string {
  const r = cell.getMeta("cs_repo");
  if (!r) throw new CodeStorageError("not-configured", `fragment ${cell.getMeta("name")} has no code.storage repo recorded`);
  return r;
}

export type RefreshOutcome = { changed: boolean; from: string | null; to: string | null; own: boolean; paths: string[] };

// Refresh a pin to the branch head AS READ FROM code.storage. The head
// read is the only authority — a webhook's `after` is never trusted for
// the pin, which is what makes a late/out-of-order delivery unable to
// regress it (heads only move forward in time).
export async function refreshPin(cell, which: "main" | "live"): Promise<RefreshOutcome> {
  const name = cell.getMeta("name");
  const repo = repoOf(cell);
  const from = pinOf(cell, which);
  let headSha: string;
  try {
    headSha = (await readBranchHead(cell.env, repo, which)).headSha;
  } catch (e) {
    // a missing branch is an answer, not an error: the ref was never
    // pushed (fresh repo) or was deleted (live after a teardown). The
    // pin clears and the tree empties — never serve a phantom tree.
    if (e instanceof CodeStorageError && e.status === 404) {
      if (!from) return { changed: false, from: null, to: null, own: false, paths: [] };
      cell.sql.exec("DELETE FROM git_tree WHERE ref = ?", which);
      cell.setMeta(`pin_${which}_sha`, "");
      cell.addEvent("git.refresh", `${which}: branch no longer exists — pin cleared`, { ref: which, from });
      return { changed: true, from, to: null, own: false, paths: [] };
    }
    throw e;
  }
  const to = headSha;
  if (to === from) return { changed: false, from, to, own: false, paths: [] };

  const entries = await listTree(cell.env, repo, to);
  // diff BEFORE replacing rows: the changed paths drive triggers/notify
  const prior: Map<string, any> = new Map(cell.sql.exec("SELECT path, size, last_commit_sha FROM git_tree WHERE ref = ?", which).toArray().map((r: any) => [r.path as string, r]));
  const nextPaths = new Set(entries.map((e) => e.path));
  const changedPaths: string[] = [];
  for (const e of entries) {
    const p = prior.get(e.path);
    if (!p || p.size !== e.size || p.last_commit_sha !== e.lastCommitSha) changedPaths.push(e.path);
  }
  for (const p of prior.keys()) if (!nextPaths.has(p)) changedPaths.push(p);

  cell.sql.exec("DELETE FROM git_tree WHERE ref = ?", which);
  for (let i = 0; i < entries.length; i += TREE_INSERT_BATCH) {
    const slice = entries.slice(i, i + TREE_INSERT_BATCH);
    const tuples = slice.map(() => "(?, ?, ?, ?, ?)").join(", ");
    const flat: unknown[] = [];
    for (const e of slice) flat.push(which, e.path, e.size, e.mode, e.lastCommitSha);
    cell.sql.exec(`INSERT INTO git_tree (ref, path, size, mode, last_commit_sha) VALUES ${tuples}`, ...flat);
  }
  cell.setMeta(`pin_${which}_sha`, to);
  cell.setMeta(`pin_${which}_at`, String(Date.now()));

  if (which === "main") await refreshManifestCache(cell);

  const own = !!cell.sql.exec("SELECT 1 AS y FROM own_commits WHERE sha = ?", to).toArray()[0];
  cell.addEvent(
    own ? "git.self-push" : "git.refresh",
    `${which}: ${String(from || "").slice(0, 12)} → ${to.slice(0, 12)} (${entries.length} files${changedPaths.length ? `, ${changedPaths.length} changed` : ""})`,
    { ref: which, from, to, own, changed: changedPaths.slice(0, 50) },
  );
  // Cached contents are keyed by pin SHA, so reads after a pin move can
  // never be served stale bytes — no active invalidation needed.
  return { changed: true, from, to, own, paths: changedPaths };
}

// Interpretation of an external push signal (webhook or poll): validate
// (already done by the caller for webhooks), persist (the dedupe row +
// ledger entry happen in ingestWebhookPush), then interpret — refresh the
// pin from the head and, only for EXTERNAL commits, fire files triggers +
// notify + watchers. Own commits refresh the pin but never trigger: that
// is the workflow-writes-never-retrigger promise, now structural.
export async function interpretPush(cell, which: "main" | "live"): Promise<RefreshOutcome> {
  const out = await refreshPin(cell, which);
  if (!out.changed || out.own) return out;
  if (which === "main" && out.paths.length) {
    // debounce + dirty-paths, exactly like the old editor-plane funnel
    const dirty = new Set(JSON.parse(cell.getMeta("sync_dirty_paths") || "[]"));
    for (const p of out.paths) dirty.add(p);
    cell.setMeta("sync_dirty_paths", JSON.stringify([...dirty].slice(-500)));
    if (!parseInt(cell.getMeta("sync_trigger_at") || "0", 10)) {
      const m = cell.manifest();
      cell.setMeta("sync_trigger_at", String(Date.now() + ((m && m.debounceMs) || 4000)));
    }
    await cell.rearmAlarm();
    watchBroadcast(cell, { type: "changed", ref: "main", sha: out.to, paths: out.paths.slice(0, 200) });
    await enqueueNotify(cell, out.paths);
  }
  return out;
}

// ---- manifest: read from the repo (pinned SHA), cached in cell meta ----

export async function refreshManifestCache(cell): Promise<void> {
  const pin = pinOf(cell, "main");
  const name = cell.getMeta("name");
  if (!pin) { cell.setMeta("manifest", ""); return; }
  const repo = repoOf(cell);
  let raw: string | null = null;
  try {
    raw = await readFileTextAt(cell, repo, "fragment.json", pin, "manifest");
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) raw = null;
    else throw e;
  }
  if (raw === null) {
    // no manifest in the repo yet: keep a seeded default so role checks
    // and workflow listing still answer (create flow bootstraps this)
    const seeded = normalizeManifest({ name, visibility: "link", editors: [], viewers: [], workflows: [], secrets: [] });
    if (seeded.error) throw new CodeStorageError("bad-response", `seed manifest invalid: ${seeded.error}`);
    cell.setMeta("manifest", JSON.stringify(seeded.manifest));
    cell.setMeta("manifest_cache_sha", pin);
    return;
  }
  let parsed: any;
  try { parsed = JSON.parse(raw); } catch { parsed = null; }
  const res = parsed === null ? { error: "fragment.json is not valid JSON" } : normalizeManifest(parsed);
  if (res.error) {
    cell.addEvent("manifest.invalid", `pinned fragment.json rejected: ${res.error} — serving the last good cache`, { pin });
    return; // stale-read behavior: keep the last good manifest until a valid one lands
  }
  res.manifest.name = name; // name is registry identity, immutable by the repo
  cell.setMeta("manifest", JSON.stringify(res.manifest));
  cell.setMeta("manifest_cache_sha", pin);
  await cell.syncRolesToRegistry();
}

// ---- reads ----

function treeRow(cell, ref: "main" | "live", path: string) {
  return cell.sql.exec("SELECT size, mode, last_commit_sha FROM git_tree WHERE ref = ? AND path = ?", ref, path).toArray()[0] || null;
}

export function treeList(cell, ref: "main" | "live", prefix = "") {
  return cell.sql.exec(
    "SELECT path, size, mode, last_commit_sha FROM git_tree WHERE ref = ? AND path LIKE ? ORDER BY path",
    ref, prefix + "%",
  ).toArray();
}

function requirePin(cell, ref: "main" | "live"): string {
  const pin = pinOf(cell, ref);
  if (!pin) {
    throw new CodeStorageError("not-configured", ref === "main"
      ? "no main pin yet — the fragment repo has no main branch (push something first)"
      : "no live pin yet — deploy first (move the live ref in code.storage)", 404);
  }
  return pin;
}

function cacheKey(cell, repo: string, pin: string, path: string): string {
  return `${cell.getMeta("name")}|${repo}|${pin}|${path}`;
}

// Bounded whole-body text read (manifests, workflow/app source, ctx.files.read).
// This is a TEXT materializer: bounded by design. The stream path
// (readFileStream) is the unbounded one.
export async function readFileTextAt(cell, repo: string, path: string, pin: string, what: string): Promise<string | null> {
  const key = cacheKey(cell, repo, pin, path);
  const cached = lruGet(key);
  if (cached !== null) return new TextDecoder().decode(cached);
  const resp = await streamFile(cell.env, repo, pin, path);
  const len = parseInt(resp.headers.get("content-length") || "0", 10);
  if (len > READ_CEILING) {
    throw new CodeStorageError("payload-too-large", `${what} is ${(len / 1048576).toFixed(1)}MiB — over the ${READ_CEILING / 1048576}MiB decode ceiling; consume it streamed`, 413);
  }
  const bytes = new Uint8Array(await resp.arrayBuffer());
  if (bytes.byteLength > READ_CEILING) {
    throw new CodeStorageError("payload-too-large", `${what} exceeded the decode ceiling in flight`, 413);
  }
  lruPut(key, bytes);
  return new TextDecoder().decode(bytes);
}

export async function readFileText(cell, path: string, what = `file ${path}`): Promise<string | null> {
  const row = treeRow(cell, "main", path);
  if (!row) return null;
  if ((row.size | 0) > READ_CEILING) {
    throw new CodeStorageError("payload-too-large", `${what} is ${(row.size / 1048576).toFixed(1)}MiB — over the ${READ_CEILING / 1048576}MiB decode ceiling; consume it streamed`, 413);
  }
  return await readFileTextAt(cell, repoOf(cell), path, requirePin(cell, "main"), what);
}

// Streamed read for serving and byte consumers: LRU hit serves a copy;
// miss (or entry over the per-entry cap) proxies the code.storage stream
// through untouched — no whole-body buffering, no size ceiling. ref picks
// main (working copy) or live (blessed serve point).
export async function readFileStream(cell, path: string, ref: "main" | "live"): Promise<Response> {
  const row = treeRow(cell, ref, path);
  if (!row) return json({ error: "no such file" }, 404);
  const pin = requirePin(cell, ref);
  const repo = repoOf(cell);
  const key = cacheKey(cell, repo, pin, path);
  const cached = lruGet(key);
  if (cached !== null) {
    return new Response(new Uint8Array(cached), { headers: { "content-type": "application/octet-stream" } });
  }
  const resp = await streamFile(cell.env, repo, pin, path);
  const ct = resp.headers.get("content-type") || "application/octet-stream";
  if ((row.size | 0) > LRU_ENTRY_BYTES) {
    // too big to cache: pass the stream straight through
    return new Response(resp.body, { status: resp.status, headers: { "content-type": ct } });
  }
  // cacheable size: tee; serve one side immediately, fill the cache from
  // the other as it drains. Cache fill is best-effort by design — if the
  // serve side errors mid-stream the cache simply misses next time.
  const [serve, fill] = resp.body.tee();
  void (async () => {
    try {
      const bytes = new Uint8Array(await new Response(fill).arrayBuffer());
      lruPut(key, bytes);
    } catch { /* best-effort cache fill */ }
  })();
  return new Response(serve, { status: resp.status, headers: { "content-type": ct } });
}

export type StatShape = { path: string; size: number; blobSha: string; lastCommitSha: string; present: boolean };

// Live path identity at the main pin: the read half of the ifSha CAS.
// blobSha is the GIT blob sha (content address), not sha256.
export async function statPath(cell, path: string): Promise<StatShape> {
  const row = treeRow(cell, "main", path);
  const pin = pinOf(cell, "main");
  if (!row || !pin) return { path, size: 0, blobSha: "", lastCommitSha: "", present: false };
  const head = await headFile(cell.env, repoOf(cell), pin, path);
  return {
    path,
    size: head ? head.size : row.size,
    blobSha: head ? head.blobSha : "",
    lastCommitSha: head ? head.lastCommitSha : row.last_commit_sha,
    present: true,
  };
}

// ---- git blob identity (sha1 of "blob <len>\0" + bytes) ----
// The tree listings carry no blob SHAs (spec shape), so per-path content
// identity is computed where needed and cross-checked against the
// server's X-Blob-Sha — same digest git itself uses.
export async function gitBlobSha(bytes: Uint8Array): Promise<string> {
  const header = new TextEncoder().encode(`blob ${bytes.byteLength}\0`);
  const cat = new Uint8Array(header.length + bytes.byteLength);
  cat.set(header, 0);
  cat.set(bytes, header.length);
  const digest = await crypto.subtle.digest("SHA-1", cat);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

// ---- the write path ----

export type WriteOp = { path: string; delete?: boolean; bytes?: Uint8Array };

export type WriteOutcome =
  | { ok: true; deduped: boolean; commitSha: string | null }
  | { ok: false; error: string; status: number; conflict?: boolean };

function validWritePath(path: string): string | null {
  if (!path || typeof path !== "string") return "bad path";
  if (path.includes("..") || path.startsWith("/")) return "bad path";
  if (path.length > 300) return "path too long (max 300 chars)";
  if (RESERVED_PATHS.has(path) || path.startsWith(".fragment/")) {
    return `path reserved by the platform (${path}) — fragment.json is CLI-owned (manifest-set is an ordinary commit), and .fragment/* never enters the repo (secrets live in the cell, wrapped)`;
  }
  return null;
}

// append-only prefixes from the (already-normalized) manifest
function appendOnlyHit(cell, path: string): boolean {
  const m = cell.manifest();
  return !!m && (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}

async function headOrNull(cell, repo: string, ref: string | null, path: string) {
  if (!ref) return null;
  try {
    return await headFile(cell.env, repo, ref, path);
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) return null;
    throw e;
  }
}

// The branch head, or null when the branch does not exist yet. The head
// is the AUTHORITATIVE write parent — never the (possibly stale) pin.
async function headShaOrNull(cell, repo: string): Promise<string | null> {
  try {
    return (await readBranchHead(cell.env, repo, "main")).headSha;
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) return null;
    throw e;
  }
}

// The single write funnel for workflow-plane mutations. Guarantees:
//   - exactly one commit per logical change (dedup by blob identity)
//   - expected-parent CAS against the pinned head, bounded retries
//   - crash between "commit accepted" and "recorded" heals as a no-op
//     success on retry (idempotent replay detection), never a duplicate
//   - own commits refresh the pin but never fire sync triggers
export async function commitPaths(cell, ops: WriteOp[], message: string, via: string): Promise<WriteOutcome> {
  if (!ops.length) return { ok: true, deduped: true, commitSha: null };
  if (ops.length > COMMIT_MAX_FILES) {
    return { ok: false, error: `one commit carries at most ${COMMIT_MAX_FILES} files (got ${ops.length}) — split the batch`, status: 400 };
  }
  const repo = repoOf(cell);
  const name = cell.getMeta("name");
  for (const op of ops) {
    const bad = validWritePath(op.path);
    if (bad) return { ok: false, error: `${bad}: ${op.path}`, status: 400 };
  }

  for (let attempt = 1; attempt <= COMMIT_MAX_ATTEMPTS; attempt++) {
    // The write parent is the branch HEAD as read right now — the pin is
    // for reads and may lag; CAS and identity verdicts (dedup, ifSha,
    // append-only) run against what a commit would actually build on.
    const headSha = await headShaOrNull(cell, repo);
    const effective: { op: WriteOp; blobSha: string }[] = [];
    for (const op of ops) {
      const head = await headOrNull(cell, repo, headSha, op.path);
      const sha = op.delete ? "" : await gitBlobSha(op.bytes!);
      if (op.delete && !head) continue; // deleting an absent path: no-op
      if (!op.delete && head && head.blobSha === sha) continue; // write-suppression: identical content is a recorded no-op
      if (head && appendOnlyHit(cell, op.path)) {
        // append-only refuses CHANGES to existing entries (identical
        // rewrites no-op'd above); new paths under the prefix are the
        // append the prefix exists for
        return { ok: false, error: `append-only: ${op.path} already exists and this changes it`, status: 409 };
      }
      effective.push({ op, blobSha: sha });
    }

    if (!effective.length) {
      // everything already landed byte-identically at the head: a recorded
      // no-op. If our pin lagged (lost bookkeeping, or a concurrent writer),
      // heal it from the head so reads catch up too.
      if (headSha && pinOf(cell, "main") !== headSha) await refreshPin(cell, "main");
      cell.addEvent("write.deduped", ops.map((o) => o.path).join(", ").slice(0, 300));
      return { ok: true, deduped: true, commitSha: headSha };
    }

    try {
      const outcome = await commitFiles(cell.env, repo, name, {
        branch: "main",
        expectedTargetSha: headSha, // null on a repo with no main yet: creates it
        message: message || `fragment ${name}: ${via}`,
        files: effective.map(({ op }) =>
          op.delete
            ? { op: "delete" as const, path: op.path }
            : { op: "upsert" as const, path: op.path, bytes: op.bytes! }),
      });
      return await recordOwnCommit(cell, outcome.newSha, via, effective);
    } catch (e) {
      if (!(e instanceof CodeStorageError) || e.kind !== "conflict") throw e;
      if (attempt >= COMMIT_MAX_ATTEMPTS) {
        cell.addEvent("write.conflict", `CAS failed after ${COMMIT_MAX_ATTEMPTS} attempts: ${effective.map((x) => x.op.path).join(", ").slice(0, 200)}`);
        return { ok: false, error: `branch moved under us ${COMMIT_MAX_ATTEMPTS} times — refetch and retry explicitly`, status: 409, conflict: true };
      }
      // CAS failed: either another writer moved main between our head read
      // and the commit, or OUR commit from a crashed turn landed without
      // being recorded. Distinguish by re-resolving identity at the NEW
      // head: identical content everywhere means our commit is already in —
      // heal as a no-op success, never a duplicate.
      const headNow = await headShaOrNull(cell, repo);
      const healed = headNow !== null && await detectIdempotentReplay(cell, repo, headNow, effective);
      if (healed && headNow) {
        return await recordOwnCommit(cell, headNow, `${via} (replay-healed)`, effective);
      }
      // genuine competing write: the loop re-reads the head and re-plans
    }
  }
  return { ok: false, error: "unreachable: commit retry loop exhausted", status: 500 };
}

// Did every pending change already land byte-identically at `sha`?
async function detectIdempotentReplay(cell, repo: string, sha: string, effective: { op: WriteOp; blobSha: string }[]): Promise<boolean> {
  for (const { op, blobSha } of effective) {
    const head = await headOrNull(cell, repo, sha, op.path);
    if (op.delete) { if (head) return false; continue; }
    if (!head || head.blobSha !== blobSha) return false;
  }
  return true;
}

// Post-commit bookkeeping: own_commits + an AUTHORITATIVE tree rebuild.
// The pin moves to the head we CAUSED, and the tree index is refetched
// from it — a local-delta patch would silently skip any competing commits
// between our previous pin and the head we built on, so correctness beats
// the extra listing call (one per committed batch, not per file). Nothing
// in this path schedules sync triggers: workflow writes never retrigger.
async function recordOwnCommit(cell, newSha: string, via: string, effective: { op: WriteOp; blobSha: string }[]): Promise<{ ok: true; deduped: boolean; commitSha: string }> {
  cell.sql.exec("INSERT OR IGNORE INTO own_commits (sha, at, via) VALUES (?, ?, ?)", newSha, Date.now(), via);
  cell.addEvent("git.commit", `${via}: ${effective.map((x) => (x.op.delete ? "-" : "+") + x.op.path).join(" ").slice(0, 300)} @ ${newSha.slice(0, 12)}`, { sha: newSha, via });
  await refreshPin(cell, "main");
  return { ok: true, deduped: false, commitSha: newSha };
}

// ---- webhook ingestion: validate -> persist -> interpret ----

export type WebhookPush = { ref: string; before: string; after: string; repoUrl: string };

export function parsePushPayload(event: string, payload: any): WebhookPush | null {
  if (event !== "push" || !payload || typeof payload !== "object") return null;
  const ref = String(payload.ref || "");
  if (!ref.startsWith("refs/heads/")) return null;
  return {
    ref: ref.slice("refs/heads/".length),
    before: String(payload.before || ""),
    after: String(payload.after || ""),
    repoUrl: String((payload.repository && payload.repository.url) || ""),
  };
}

// Persist the delivery (dedupe row + ledger event), then interpret. The
// cell is single-writer, so check-then-insert is race-free and the
// existence check IS the dedupe verdict: a redelivery acks 200 and does
// no interpretation work — no irreversible action straight off an
// external event, and no double-trigger for one push.
export async function ingestWebhookPush(cell, push: WebhookPush, dedupeKey: string): Promise<{ interpreted: boolean; redelivery: boolean }> {
  const seen = !!cell.sql.exec("SELECT 1 AS y FROM webhook_events WHERE k = ?", dedupeKey).toArray()[0];
  if (seen) {
    cell.addEvent("webhook.redelivery", `${push.ref}: ${push.after.slice(0, 12)} (deduped)`, { ref: push.ref });
    return { interpreted: false, redelivery: true };
  }
  cell.sql.exec("INSERT INTO webhook_events (k, at) VALUES (?, ?)", dedupeKey, Date.now());
  // retention: age out old rows, and hard-cap storm volume
  cell.sql.exec("DELETE FROM webhook_events WHERE at < ?", Date.now() - 7 * 24 * 3600_000);
  const rows = cell.sql.exec("SELECT COUNT(*) c FROM webhook_events").toArray()[0].c;
  if (rows > WEBHOOK_DEDUPE_ROWS) {
    cell.sql.exec("DELETE FROM webhook_events WHERE k NOT IN (SELECT k FROM webhook_events ORDER BY at DESC LIMIT ?)", WEBHOOK_DEDUPE_ROWS);
  }
  cell.addEvent("webhook.push", `${push.ref}: ${push.before.slice(0, 12)} → ${push.after.slice(0, 12)}`, { ref: push.ref, dedupeKey });
  if (push.ref !== "main" && push.ref !== "live") {
    return { interpreted: false, redelivery: false };
  }
  await interpretPush(cell, push.ref as "main" | "live");
  return { interpreted: true, redelivery: false };
}

// ---- poll backstop ----

export const POLL_INTERVAL_MS = 5 * 60_000; // ROADMAP: 5-minute contained backstop

export async function pollBackstop(cell): Promise<void> {
  // failures are events, not crashes: the poll is a backstop and must not
  // take the alarm down with it (the alarm's finally re-arms regardless)
  for (const which of ["main", "live"] as const) {
    try {
      await interpretPush(cell, which);
    } catch (e) {
      cell.addEvent("git.poll-failed", `${which}: ${String((e as Error).message || e).slice(0, 200)}`, { ref: which });
    }
  }
  cell.setMeta("poll_last_at", String(Date.now()));
}

export function nextPollAt(cell): number | null {
  const last = parseInt(cell.getMeta("poll_last_at") || "0", 10);
  return last ? last + POLL_INTERVAL_MS : Date.now();
}

// ---- first-touch bootstrap ----

// Pins populate lazily on first read; serve paths and the status
// endpoint call this so a fresh cell fills its tree index without
// waiting for a webhook. A missing branch (404) is an answer: that pin
// just stays empty until something pushes.
export async function ensurePins(cell): Promise<void> {
  for (const which of ["main", "live"] as const) {
    if (pinOf(cell, which)) continue;
    try {
      await refreshPin(cell, which);
    } catch (e) {
      if (e instanceof CodeStorageError && e.status === 404) continue;
      throw e;
    }
  }
}
