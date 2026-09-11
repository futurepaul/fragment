// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { json } from "./util.js";
import { normalizeManifest } from "./manifest.js";
import { watchBroadcast } from "./history.js";
import { enqueueNotify } from "./notify.js";
import {
  CodeStorageError,
  readBranchHead,
  listTree,
  streamFile,
  headFile,
  commitFiles
} from "./codestorage.js";
const READ_CEILING = 8 * 1024 * 1024;
const LRU_TOTAL_BYTES = 64 * 1024 * 1024;
const LRU_ENTRY_BYTES = READ_CEILING;
const COMMIT_MAX_FILES = 64;
const COMMIT_MAX_ATTEMPTS = 3;
const TREE_INSERT_BATCH = 20;
const RESERVED_PATHS = /* @__PURE__ */ new Set(["fragment.json", ".fragment/secrets.json"]);
const WEBHOOK_DEDUPE_ROWS = 1e4;
const lru = /* @__PURE__ */ new Map();
let lruBytes = 0;
function lruGet(key) {
  const hit = lru.get(key) || null;
  if (hit !== null) {
    lru.delete(key);
    lru.set(key, hit);
  }
  return hit;
}
function lruPut(key, bytes) {
  if (bytes.byteLength > LRU_ENTRY_BYTES) return;
  const prior = lru.get(key);
  if (prior) {
    lruBytes -= prior.byteLength;
    lru.delete(key);
  }
  lru.set(key, bytes);
  lruBytes += bytes.byteLength;
  while (lruBytes > LRU_TOTAL_BYTES && lru.size > 1) {
    const oldest = lru.keys().next().value;
    const b = lru.get(oldest);
    lru.delete(oldest);
    lruBytes -= b ? b.byteLength : 0;
  }
}
function lruStats() {
  return { entries: lru.size, bytes: lruBytes, totalCap: LRU_TOTAL_BYTES };
}
function pinOf(cell, which) {
  const v = cell.getMeta(`pin_${which}_sha`);
  return v && /^[0-9a-f]{40}$/.test(v) ? v : null;
}
function repoOf(cell) {
  const r = cell.getMeta("cs_repo");
  if (!r) throw new CodeStorageError("not-configured", `fragment ${cell.getMeta("name")} has no code.storage repo recorded`);
  return r;
}
async function refreshPin(cell, which) {
  const name = cell.getMeta("name");
  const repo = repoOf(cell);
  const from = pinOf(cell, which);
  let headSha;
  try {
    headSha = (await readBranchHead(cell.env, repo, which)).headSha;
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) {
      if (!from) return { changed: false, from: null, to: null, own: false, paths: [] };
      cell.sql.exec("DELETE FROM git_tree WHERE ref = ?", which);
      cell.setMeta(`pin_${which}_sha`, "");
      cell.addEvent("git.refresh", `${which}: branch no longer exists \u2014 pin cleared`, { ref: which, from });
      return { changed: true, from, to: null, own: false, paths: [] };
    }
    throw e;
  }
  const to = headSha;
  if (to === from) return { changed: false, from, to, own: false, paths: [] };
  const entries = await listTree(cell.env, repo, to);
  const prior = new Map(cell.sql.exec("SELECT path, size, last_commit_sha FROM git_tree WHERE ref = ?", which).toArray().map((r) => [r.path, r]));
  const nextPaths = new Set(entries.map((e) => e.path));
  const changedPaths = [];
  for (const e of entries) {
    const p = prior.get(e.path);
    if (!p || p.size !== e.size || p.last_commit_sha !== e.lastCommitSha) changedPaths.push(e.path);
  }
  for (const p of prior.keys()) if (!nextPaths.has(p)) changedPaths.push(p);
  cell.sql.exec("DELETE FROM git_tree WHERE ref = ?", which);
  for (let i = 0; i < entries.length; i += TREE_INSERT_BATCH) {
    const slice = entries.slice(i, i + TREE_INSERT_BATCH);
    const tuples = slice.map(() => "(?, ?, ?, ?, ?)").join(", ");
    const flat = [];
    for (const e of slice) flat.push(which, e.path, e.size, e.mode, e.lastCommitSha);
    cell.sql.exec(`INSERT INTO git_tree (ref, path, size, mode, last_commit_sha) VALUES ${tuples}`, ...flat);
  }
  cell.setMeta(`pin_${which}_sha`, to);
  cell.setMeta(`pin_${which}_at`, String(Date.now()));
  if (which === "main") await refreshManifestCache(cell);
  const own = !!cell.sql.exec("SELECT 1 AS y FROM own_commits WHERE sha = ?", to).toArray()[0];
  cell.addEvent(
    own ? "git.self-push" : "git.refresh",
    `${which}: ${String(from || "").slice(0, 12)} \u2192 ${to.slice(0, 12)} (${entries.length} files${changedPaths.length ? `, ${changedPaths.length} changed` : ""})`,
    { ref: which, from, to, own, changed: changedPaths.slice(0, 50) }
  );
  return { changed: true, from, to, own, paths: changedPaths };
}
async function interpretPush(cell, which) {
  const out = await refreshPin(cell, which);
  if (!out.changed || out.own) return out;
  if (which === "main" && out.paths.length) {
    const dirty = new Set(JSON.parse(cell.getMeta("sync_dirty_paths") || "[]"));
    for (const p of out.paths) dirty.add(p);
    cell.setMeta("sync_dirty_paths", JSON.stringify([...dirty].slice(-500)));
    if (!parseInt(cell.getMeta("sync_trigger_at") || "0", 10)) {
      const m = cell.manifest();
      cell.setMeta("sync_trigger_at", String(Date.now() + (m && m.debounceMs || 4e3)));
    }
    await cell.rearmAlarm();
    watchBroadcast(cell, { type: "changed", ref: "main", sha: out.to, paths: out.paths.slice(0, 200) });
    await enqueueNotify(cell, out.paths);
  }
  return out;
}
async function refreshManifestCache(cell) {
  const pin = pinOf(cell, "main");
  const name = cell.getMeta("name");
  if (!pin) {
    cell.setMeta("manifest", "");
    return;
  }
  const repo = repoOf(cell);
  let raw = null;
  try {
    raw = await readFileTextAt(cell, repo, "fragment.json", pin, "manifest");
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) raw = null;
    else throw e;
  }
  if (raw === null) {
    const seeded = normalizeManifest({ name, visibility: "link", editors: [], viewers: [], workflows: [], secrets: [] });
    if (seeded.error) throw new CodeStorageError("bad-response", `seed manifest invalid: ${seeded.error}`);
    cell.setMeta("manifest", JSON.stringify(seeded.manifest));
    cell.setMeta("manifest_cache_sha", pin);
    return;
  }
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    parsed = null;
  }
  const res = parsed === null ? { error: "fragment.json is not valid JSON" } : normalizeManifest(parsed);
  if (res.error) {
    cell.addEvent("manifest.invalid", `pinned fragment.json rejected: ${res.error} \u2014 serving the last good cache`, { pin });
    return;
  }
  res.manifest.name = name;
  cell.setMeta("manifest", JSON.stringify(res.manifest));
  cell.setMeta("manifest_cache_sha", pin);
  await cell.syncRolesToRegistry();
}
function treeRow(cell, ref, path) {
  return cell.sql.exec("SELECT size, mode, last_commit_sha FROM git_tree WHERE ref = ? AND path = ?", ref, path).toArray()[0] || null;
}
function treeList(cell, ref, prefix = "") {
  return cell.sql.exec(
    "SELECT path, size, mode, last_commit_sha FROM git_tree WHERE ref = ? AND path LIKE ? ORDER BY path",
    ref,
    prefix + "%"
  ).toArray();
}
function requirePin(cell, ref) {
  const pin = pinOf(cell, ref);
  if (!pin) {
    throw new CodeStorageError("not-configured", ref === "main" ? "no main pin yet \u2014 the fragment repo has no main branch (push something first)" : "no live pin yet \u2014 deploy first (move the live ref in code.storage)", 404);
  }
  return pin;
}
function cacheKey(cell, repo, pin, path) {
  return `${cell.getMeta("name")}|${repo}|${pin}|${path}`;
}
async function readFileTextAt(cell, repo, path, pin, what) {
  const key = cacheKey(cell, repo, pin, path);
  const cached = lruGet(key);
  if (cached !== null) return new TextDecoder().decode(cached);
  const resp = await streamFile(cell.env, repo, pin, path);
  const len = parseInt(resp.headers.get("content-length") || "0", 10);
  if (len > READ_CEILING) {
    throw new CodeStorageError("payload-too-large", `${what} is ${(len / 1048576).toFixed(1)}MiB \u2014 over the ${READ_CEILING / 1048576}MiB decode ceiling; consume it streamed`, 413);
  }
  const bytes = new Uint8Array(await resp.arrayBuffer());
  if (bytes.byteLength > READ_CEILING) {
    throw new CodeStorageError("payload-too-large", `${what} exceeded the decode ceiling in flight`, 413);
  }
  lruPut(key, bytes);
  return new TextDecoder().decode(bytes);
}
async function readFileText(cell, path, what = `file ${path}`) {
  const row = treeRow(cell, "main", path);
  if (!row) return null;
  if ((row.size | 0) > READ_CEILING) {
    throw new CodeStorageError("payload-too-large", `${what} is ${(row.size / 1048576).toFixed(1)}MiB \u2014 over the ${READ_CEILING / 1048576}MiB decode ceiling; consume it streamed`, 413);
  }
  return await readFileTextAt(cell, repoOf(cell), path, requirePin(cell, "main"), what);
}
async function readFileStream(cell, path, ref) {
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
    return new Response(resp.body, { status: resp.status, headers: { "content-type": ct } });
  }
  const [serve, fill] = resp.body.tee();
  void (async () => {
    try {
      const bytes = new Uint8Array(await new Response(fill).arrayBuffer());
      lruPut(key, bytes);
    } catch {
    }
  })();
  return new Response(serve, { status: resp.status, headers: { "content-type": ct } });
}
async function statPath(cell, path) {
  const row = treeRow(cell, "main", path);
  const pin = pinOf(cell, "main");
  if (!row || !pin) return { path, size: 0, blobSha: "", lastCommitSha: "", present: false };
  const head = await headFile(cell.env, repoOf(cell), pin, path);
  return {
    path,
    size: head ? head.size : row.size,
    blobSha: head ? head.blobSha : "",
    lastCommitSha: head ? head.lastCommitSha : row.last_commit_sha,
    present: true
  };
}
async function gitBlobSha(bytes) {
  const header = new TextEncoder().encode(`blob ${bytes.byteLength}\0`);
  const cat = new Uint8Array(header.length + bytes.byteLength);
  cat.set(header, 0);
  cat.set(bytes, header.length);
  const digest = await crypto.subtle.digest("SHA-1", cat);
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}
function validWritePath(path) {
  if (!path || typeof path !== "string") return "bad path";
  if (path.includes("..") || path.startsWith("/")) return "bad path";
  if (path.length > 300) return "path too long (max 300 chars)";
  if (RESERVED_PATHS.has(path) || path.startsWith(".fragment/")) {
    return `path reserved by the platform (${path}) \u2014 fragment.json is CLI-owned (manifest-set is an ordinary commit), and .fragment/* never enters the repo (secrets live in the cell, wrapped)`;
  }
  return null;
}
function appendOnlyHit(cell, path) {
  const m = cell.manifest();
  return !!m && (m.appendOnly || []).some((p) => path === p.slice(0, -1) || path.startsWith(p));
}
async function headOrNull(cell, repo, ref, path) {
  if (!ref) return null;
  try {
    return await headFile(cell.env, repo, ref, path);
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) return null;
    throw e;
  }
}
async function headShaOrNull(cell, repo) {
  try {
    return (await readBranchHead(cell.env, repo, "main")).headSha;
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) return null;
    throw e;
  }
}
async function commitPaths(cell, ops, message, via) {
  if (!ops.length) return { ok: true, deduped: true, commitSha: null };
  if (ops.length > COMMIT_MAX_FILES) {
    return { ok: false, error: `one commit carries at most ${COMMIT_MAX_FILES} files (got ${ops.length}) \u2014 split the batch`, status: 400 };
  }
  const repo = repoOf(cell);
  const name = cell.getMeta("name");
  for (const op of ops) {
    const bad = validWritePath(op.path);
    if (bad) return { ok: false, error: `${bad}: ${op.path}`, status: 400 };
  }
  for (let attempt = 1; attempt <= COMMIT_MAX_ATTEMPTS; attempt++) {
    const headSha = await headShaOrNull(cell, repo);
    const effective = [];
    for (const op of ops) {
      const head = await headOrNull(cell, repo, headSha, op.path);
      const sha = op.delete ? "" : await gitBlobSha(op.bytes);
      if (op.delete && !head) continue;
      if (!op.delete && head && head.blobSha === sha) continue;
      if (head && appendOnlyHit(cell, op.path)) {
        return { ok: false, error: `append-only: ${op.path} already exists and this changes it`, status: 409 };
      }
      effective.push({ op, blobSha: sha });
    }
    if (!effective.length) {
      if (headSha && pinOf(cell, "main") !== headSha) await refreshPin(cell, "main");
      cell.addEvent("write.deduped", ops.map((o) => o.path).join(", ").slice(0, 300));
      return { ok: true, deduped: true, commitSha: headSha };
    }
    try {
      const outcome = await commitFiles(cell.env, repo, name, {
        branch: "main",
        expectedTargetSha: headSha,
        // null on a repo with no main yet: creates it
        message: message || `fragment ${name}: ${via}`,
        files: effective.map(({ op }) => op.delete ? { op: "delete", path: op.path } : { op: "upsert", path: op.path, bytes: op.bytes })
      });
      return await recordOwnCommit(cell, outcome.newSha, via, effective);
    } catch (e) {
      if (!(e instanceof CodeStorageError) || e.kind !== "conflict") throw e;
      if (attempt >= COMMIT_MAX_ATTEMPTS) {
        cell.addEvent("write.conflict", `CAS failed after ${COMMIT_MAX_ATTEMPTS} attempts: ${effective.map((x) => x.op.path).join(", ").slice(0, 200)}`);
        return { ok: false, error: `branch moved under us ${COMMIT_MAX_ATTEMPTS} times \u2014 refetch and retry explicitly`, status: 409, conflict: true };
      }
      const headNow = await headShaOrNull(cell, repo);
      const healed = headNow !== null && await detectIdempotentReplay(cell, repo, headNow, effective);
      if (healed && headNow) {
        return await recordOwnCommit(cell, headNow, `${via} (replay-healed)`, effective);
      }
    }
  }
  return { ok: false, error: "unreachable: commit retry loop exhausted", status: 500 };
}
async function detectIdempotentReplay(cell, repo, sha, effective) {
  for (const { op, blobSha } of effective) {
    const head = await headOrNull(cell, repo, sha, op.path);
    if (op.delete) {
      if (head) return false;
      continue;
    }
    if (!head || head.blobSha !== blobSha) return false;
  }
  return true;
}
async function recordOwnCommit(cell, newSha, via, effective) {
  cell.sql.exec("INSERT OR IGNORE INTO own_commits (sha, at, via) VALUES (?, ?, ?)", newSha, Date.now(), via);
  cell.addEvent("git.commit", `${via}: ${effective.map((x) => (x.op.delete ? "-" : "+") + x.op.path).join(" ").slice(0, 300)} @ ${newSha.slice(0, 12)}`, { sha: newSha, via });
  await refreshPin(cell, "main");
  return { ok: true, deduped: false, commitSha: newSha };
}
function parsePushPayload(event, payload) {
  if (event !== "push" || !payload || typeof payload !== "object") return null;
  const ref = String(payload.ref || "");
  if (!ref.startsWith("refs/heads/")) return null;
  return {
    ref: ref.slice("refs/heads/".length),
    before: String(payload.before || ""),
    after: String(payload.after || ""),
    repoUrl: String(payload.repository && payload.repository.url || "")
  };
}
async function ingestWebhookPush(cell, push, dedupeKey) {
  const seen = !!cell.sql.exec("SELECT 1 AS y FROM webhook_events WHERE k = ?", dedupeKey).toArray()[0];
  if (seen) {
    cell.addEvent("webhook.redelivery", `${push.ref}: ${push.after.slice(0, 12)} (deduped)`, { ref: push.ref });
    return { interpreted: false, redelivery: true };
  }
  cell.sql.exec("INSERT INTO webhook_events (k, at) VALUES (?, ?)", dedupeKey, Date.now());
  cell.sql.exec("DELETE FROM webhook_events WHERE at < ?", Date.now() - 7 * 24 * 36e5);
  const rows = cell.sql.exec("SELECT COUNT(*) c FROM webhook_events").toArray()[0].c;
  if (rows > WEBHOOK_DEDUPE_ROWS) {
    cell.sql.exec("DELETE FROM webhook_events WHERE k NOT IN (SELECT k FROM webhook_events ORDER BY at DESC LIMIT ?)", WEBHOOK_DEDUPE_ROWS);
  }
  cell.addEvent("webhook.push", `${push.ref}: ${push.before.slice(0, 12)} \u2192 ${push.after.slice(0, 12)}`, { ref: push.ref, dedupeKey });
  if (push.ref !== "main" && push.ref !== "live") {
    return { interpreted: false, redelivery: false };
  }
  await interpretPush(cell, push.ref);
  return { interpreted: true, redelivery: false };
}
const POLL_INTERVAL_MS = 5 * 6e4;
async function pollBackstop(cell) {
  for (const which of ["main", "live"]) {
    try {
      await interpretPush(cell, which);
    } catch (e) {
      cell.addEvent("git.poll-failed", `${which}: ${String(e.message || e).slice(0, 200)}`, { ref: which });
    }
  }
  cell.setMeta("poll_last_at", String(Date.now()));
}
function nextPollAt(cell) {
  const last = parseInt(cell.getMeta("poll_last_at") || "0", 10);
  return last ? last + POLL_INTERVAL_MS : Date.now();
}
async function ensurePins(cell) {
  for (const which of ["main", "live"]) {
    if (pinOf(cell, which)) continue;
    try {
      await refreshPin(cell, which);
    } catch (e) {
      if (e instanceof CodeStorageError && e.status === 404) continue;
      throw e;
    }
  }
}
export {
  COMMIT_MAX_ATTEMPTS,
  COMMIT_MAX_FILES,
  LRU_ENTRY_BYTES,
  LRU_TOTAL_BYTES,
  POLL_INTERVAL_MS,
  READ_CEILING,
  commitPaths,
  ensurePins,
  gitBlobSha,
  ingestWebhookPush,
  interpretPush,
  lruStats,
  nextPollAt,
  parsePushPayload,
  pinOf,
  pollBackstop,
  readFileStream,
  readFileText,
  readFileTextAt,
  refreshManifestCache,
  refreshPin,
  repoOf,
  statPath,
  treeList
};
