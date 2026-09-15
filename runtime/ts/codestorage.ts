// code.storage client — the ONLY module that talks to code.storage.
// Cells hold names and metadata; this holds the pipes. (Same structural
// role blob-tier.ts played for blobsd; that module is deleted.)
//
// Wire contract (docs/ROADMAP.md + https://code.storage/docs, OpenAPI at
// /docs/openapi.json):
//   - Auth: customer-signed JWTs. WE sign with the org private key
//     (PIERRE_PRIVATE_KEY, PKCS8 PEM) — claims iss (org), sub, repo,
//     scopes, iat, exp. Bearer on every request.
//   - API base: https://api.{org}.code.storage — override with
//     CODESTORAGE_API_URL (dev/mock/self-hosted gateways).
//   - Preferred routes /api/repos/{repo}/...; repo names with "/" are
//     URL-encoded into one path segment. Fragment repos are plain names.
//   - Commits: POST /api/repos/{repo}/commit-pack — NDJSON, first line
//     metadata {target_branch, expected_target_sha, commit_message,
//     author, files[]}, then blob_chunk lines {content_id, data(base64),
//     eof}. Decoded chunks <= 4 MiB, no total size limit. CAS failure is
//     HTTP 409 (or 400) with result.status = "precondition_failed".
//   - Reads: GET /api/repos/{repo}/file?path=&ref= (streams bytes; HEAD
//     gives X-Blob-Sha / X-Last-Commit-Sha / Content-Length),
//     GET /api/repos/{repo}/files/metadata?ref= (recursive tree, cursor
//     pagination), GET /api/repos/{repo}/branch?name= (head sha).
//   - Webhooks: X-Pierre-Event, X-Pierre-Signature: t=<unix>,sha256=<hex>
//     where hex = HMAC-SHA256(secret, `${t}.${rawBody}`), 5-min freshness.
//
// WHY a first-party client and not @pierre/storage: the SDK's wire shape
// disagrees with the current spec on the two load-bearing details (it
// posts legacy /api/v1 aliases without the repo in the path, and its
// builder sends only `expected_head_sha` where the spec documents
// `expected_target_sha`). The runtime must control these bytes exactly,
// so the client is small, typed, and spec-shaped. See the workstream
// report: revisit if the SDK converges with the spec.
import { importPKCS8, SignJWT } from "jose";

// ---- typed errors (no string-matching at call sites) ----
export type CodeStorageErrorKind =
  | "not-configured" // host did not set PIERRE_PRIVATE_KEY / org
  | "bad-key" // key present but not a usable PKCS8 PEM
  | "api" // non-2xx from code.storage (status + message attached)
  | "conflict" // CAS precondition_failed (expected_target_sha mismatch)
  | "payload-too-large" // chunk over the 4 MiB documented decode limit
  | "bad-response"; // 2xx whose body failed shape validation

export class CodeStorageError extends Error {
  kind: CodeStorageErrorKind;
  status: number; // HTTP status for api/conflict; 0 otherwise
  constructor(kind: CodeStorageErrorKind, message: string, status = 0) {
    super(message);
    this.name = "CodeStorageError";
    this.kind = kind;
    this.status = status;
  }
}

export type CsConfig = {
  org: string;
  keyPem: string;
  apiUrl: string; // e.g. https://api.myorg.code.storage (no trailing /api)
};

export const CS_SCOPES = ["org:read", "repo:write", "git:read", "git:write"] as const;
export type CsScope = (typeof CS_SCOPES)[number];

// Host plumbing: CELLD_VAR_PIERRE_PRIVATE_KEY -> env.PIERRE_PRIVATE_KEY,
// CELLD_VAR_CODESTORAGE_ORG_NAME -> env.CODESTORAGE_ORG_NAME,
// CELLD_VAR_CODESTORAGE_API_URL -> env.CODESTORAGE_API_URL (override).
export function csConfig(env): CsConfig {
  // systemd EnvironmentFile is line-based, so the PEM arrives there with
  // literal \n escapes; real newlines (dev spawns, tests) pass untouched.
  const keyPem = String(env.PIERRE_PRIVATE_KEY || "")
    .replace(/\\n/g, "\n")
    .trim();
  const org = String(env.CODESTORAGE_ORG_NAME || "").trim();
  if (!keyPem || !org) {
    throw new CodeStorageError(
      "not-configured",
      "code.storage not configured on this host (set CELLD_VAR_PIERRE_PRIVATE_KEY + CELLD_VAR_CODESTORAGE_ORG_NAME)",
    );
  }
  const apiUrl = (String(env.CODESTORAGE_API_URL || "").replace(/\/+$/, "")) || `https://api.${org}.code.storage`;
  return { org, keyPem, apiUrl };
}

export function csConfigured(env): boolean {
  return !!(env.PIERRE_PRIVATE_KEY && env.CODESTORAGE_ORG_NAME);
}

// ---- JWT minting (the only thing the org key is ever used for) ----
let cachedKeyPem = "";
let cachedKey: any = null; // jose KeyLike; typed loosely for workerd/casts

export async function mintCsJwt(env, opts: { repo?: string; scopes: CsScope[]; sub: string; ttlSec: number }): Promise<string> {
  const cfg = csConfig(env);
  if (cachedKeyPem !== cfg.keyPem || !cachedKey) {
    try {
      cachedKey = await importPKCS8(cfg.keyPem, "ES256");
      cachedKeyPem = cfg.keyPem;
    } catch (e) {
      throw new CodeStorageError("bad-key", `PIERRE_PRIVATE_KEY is not a usable PKCS8 ES256 key: ${String((e as Error).message || e)}`);
    }
  }
  const now = Math.floor(Date.now() / 1000);
  return await new SignJWT({
    iss: cfg.org,
    sub: opts.sub,
    // the repo claim is REQUIRED by the real service on every call,
    // org-level included (live-verified 2026-09-15: claim-less tokens 403
    // "Invalid or expired token" even with correct key/scopes; the value
    // is not enforced on org-level paths, so default it to the org)
    repo: opts.repo ?? cfg.org,
    scopes: opts.scopes,
    iat: now,
    exp: now + opts.ttlSec,
  })
    .setProtectedHeader({ alg: "ES256", typ: "JWT" })
    .sign(cachedKey);
}

// ---- request core ----
async function csFetch(env, opts: { repo?: string; scopes: CsScope[]; sub: string; init: RequestInit; path: string; initTimeoutMs?: number }): Promise<Response> {
  const cfg = csConfig(env);
  const jwt = await mintCsJwt(env, { repo: opts.repo, scopes: opts.scopes, sub: opts.sub, ttlSec: 300 });
  const resp = await fetch(`${cfg.apiUrl}${opts.path}`, {
    ...opts.init,
    headers: { authorization: `Bearer ${jwt}`, ...(opts.init.headers || {}) },
    signal: AbortSignal.timeout(opts.initTimeoutMs ?? 30_000),
  });
  if (!resp.ok) {
    const body = await resp.text().catch(() => "");
    if (resp.status === 409 || resp.status === 400) {
      // CAS failures: 409 (branch moved) or 400 (validation) with
      // result.status = "precondition_failed" — both are conflict verdicts
      if (body.includes("precondition_failed")) {
        throw new CodeStorageError("conflict", `expected_target_sha mismatch: branch moved (${resp.status})`, resp.status);
      }
    }
    if (resp.status === 413) throw new CodeStorageError("payload-too-large", `code.storage rejected chunk size: ${body.slice(0, 200)}`, 413);
    throw new CodeStorageError("api", `code.storage ${resp.status} ${opts.path}: ${body.slice(0, 300)}`, resp.status);
  }
  return resp;
}

function repoSeg(repo: string): string {
  return repo.includes("/") ? encodeURIComponent(repo) : repo;
}

// ---- reads ----

export type BranchHead = { name: string; headSha: string };

export async function readBranchHead(env, repo: string, branch: string): Promise<BranchHead> {
  const resp = await csFetch(env, {
    repo, scopes: ["git:read"], sub: "fragment-runtime",
    path: `/api/repos/${repoSeg(repo)}/branch?name=${encodeURIComponent(branch)}`,
    init: { method: "GET" },
  });
  const body = await resp.json().catch(() => null);
  const head = body && body.branch && typeof body.branch.head_sha === "string" ? body.branch.head_sha : null;
  if (!head || !/^[0-9a-f]{40}$/.test(head)) {
    throw new CodeStorageError("bad-response", `branch read for ${repo}/${branch} returned no head_sha`);
  }
  return { name: branch, headSha: head };
}

export type TreeEntry = { path: string; mode: string; type: string; size: number; lastCommitSha: string };

// Full recursive metadata listing for a ref, paginated. The listing has NO
// blob SHAs (spec shape) — per-path blob identity comes from headFile.
// Page bound: 500 pages x 1000 files = 500k files; beyond that the tree is
// not a fragment folder anymore and we refuse loudly instead of looping.
const TREE_PAGE_LIMIT = 500;

export async function listTree(env, repo: string, ref: string): Promise<TreeEntry[]> {
  const out: TreeEntry[] = [];
  let cursor = "";
  for (let page = 0; ; page++) {
    if (page >= TREE_PAGE_LIMIT) {
      throw new CodeStorageError("api", `tree listing for ${repo}@${ref} exceeded ${TREE_PAGE_LIMIT} pages — refusing unbounded pagination`);
    }
    const q = new URLSearchParams({ ref, limit: "1000" });
    if (cursor) q.set("cursor", cursor);
    const resp = await csFetch(env, {
      repo, scopes: ["git:read"], sub: "fragment-runtime",
      path: `/api/repos/${repoSeg(repo)}/files/metadata?${q}`,
      init: { method: "GET" },
    });
    const body = await resp.json().catch(() => null);
    const files = body && Array.isArray(body.files) ? body.files : null;
    if (!files) throw new CodeStorageError("bad-response", `tree listing for ${repo}@${ref} returned no files array`);
    for (const f of files) {
      if (!f || typeof f.path !== "string" || typeof f.size !== "number" || typeof f.last_commit_sha !== "string") continue;
      if (f.type && f.type !== "blob") continue; // trees/symlinks/submodules are not file-plane content
      out.push({ path: f.path, mode: String(f.mode || "100644"), type: "blob", size: f.size, lastCommitSha: f.last_commit_sha });
    }
    if (!body.has_more || !body.next_cursor) break;
    cursor = String(body.next_cursor);
  }
  return out;
}

// Stream a file's bytes at a ref. The Response body must be consumed by
// the caller (or tee'd) — never buffered here.
export async function streamFile(env, repo: string, ref: string, path: string): Promise<Response> {
  const q = new URLSearchParams({ path, ref });
  return await csFetch(env, {
    repo, scopes: ["git:read"], sub: "fragment-runtime",
    path: `/api/repos/${repoSeg(repo)}/file?${q}`,
    init: { method: "GET" },
    initTimeoutMs: 120_000, // large files stream; bound time, not bytes
  });
}

export type FileHead = { size: number; blobSha: string; lastCommitSha: string };

// Per-path identity without bytes: X-Blob-Sha is the content address the
// dedup/write-suppression and ifSha CAS keys on. 404 (absent path) maps
// to null — absence is an answer, not an error.
export async function headFile(env, repo: string, ref: string, path: string): Promise<FileHead | null> {
  const q = new URLSearchParams({ path, ref });
  let resp: Response;
  try {
    resp = await csFetch(env, {
      repo, scopes: ["git:read"], sub: "fragment-runtime",
      path: `/api/repos/${repoSeg(repo)}/file?${q}`,
      init: { method: "HEAD" },
    });
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) return null;
    throw e;
  }
  return fileHeadFrom(resp, path);
}

export function fileHeadFrom(resp: Response, path: string): FileHead | null {
  const blobSha = String(resp.headers.get("x-blob-sha") || "");
  const lastCommitSha = String(resp.headers.get("x-last-commit-sha") || "");
  const len = parseInt(resp.headers.get("content-length") || "0", 10);
  if (!/^[0-9a-f]{40,64}$/.test(blobSha)) return null; // absent/invalid identity: treat as unknown
  return { size: Number.isSafeInteger(len) ? len : 0, blobSha, lastCommitSha };
}

// ---- writes: the commit-pack builder ----

export type CommitFile =
  | { op: "upsert"; path: string; bytes: Uint8Array }
  | { op: "delete"; path: string };

export type CommitOutcome = {
  commitSha: string;
  newSha: string;
  oldSha: string;
  status: string;
};

// Chunk carve-out: the documented per-decoded-chunk limit is 4 MiB; we
// carve at 3 MiB so a chunk that lands exactly at a boundary plus base64
// expansion never trips the server-side 413.
export const COMMIT_CHUNK_BYTES = 3 * 1024 * 1024;

// Author identity for runtime-made commits. The npub is the fragment's;
// the email domain makes provenance greppable in code.storage's UI.
function authorFor(fragmentName: string) {
  return { name: `fragment:${fragmentName}`, email: `${fragmentName}@fragment.runtime` };
}

// Build the NDJSON body as a stream: metadata line, then blob_chunk lines.
// Bytes never sit whole in memory — each file is read from its Uint8Array
// only because callers already bounded it (see WRITE_CEILING); chunk lines
// are emitted per COMMIT_CHUNK_BYTES slice.
function commitPackBody(metadata: Record<string, unknown>, files: CommitFile[]): string {
  // buffered, not streamed: contents are already in memory (the 32 MiB
  // write ceiling bounds them) and the REAL service rejects workerd's
  // chunked stream framing with "first payload must be metadata"
  // (live-verified 2026-09-15; the local mock was stream-tolerant). The
  // body is ASCII (JSON + base64), so a plain string carries it exactly.
  const lines: string[] = [JSON.stringify(metadata)];
  files.forEach((f, i) => {
    const contentId = `b${i}`;
    // delete entries still get an empty-eof chunk (documented example shape)
    const chunks: Uint8Array[] = f.op === "delete" ? [new Uint8Array(0)] : carve(f.bytes);
    for (let c = 0; c < chunks.length; c++) {
      lines.push(JSON.stringify({
        blob_chunk: { content_id: contentId, data: base64(chunks[c]), eof: c === chunks.length - 1 },
      }));
    }
  });
  return lines.join("\n") + "\n";
}

function carve(bytes: Uint8Array): Uint8Array[] {
  if (bytes.byteLength === 0) return [new Uint8Array(0)];
  const out: Uint8Array[] = [];
  for (let o = 0; o < bytes.byteLength; o += COMMIT_CHUNK_BYTES) {
    out.push(bytes.subarray(o, Math.min(o + COMMIT_CHUNK_BYTES, bytes.byteLength)));
  }
  return out;
}

function base64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

// Commit with expected-parent CAS. `expectedTargetSha` pins the commit to
// the branch state the caller believed in; a moved branch throws
// CodeStorageError(kind "conflict") for the caller's bounded-retry loop.
// BOTH wire spellings are sent deliberately: the spec documents
// expected_target_sha while @pierre/storage (checked 1.8.0 and 1.16.2)
// still sends expected_head_sha — identical value, so whichever the
// server reads, the CAS verdict is the one we need. Flagged in the
// workstream report; delete the alias if the SDK/spec converge.
export async function commitFiles(
  env, repo: string, fragmentName: string,
  opts: { branch: string; expectedTargetSha: string | null; message: string; files: CommitFile[] },
): Promise<CommitOutcome> {
  if (!opts.files.length) throw new CodeStorageError("api", "commitFiles: empty file set");
  const metadata: Record<string, unknown> = {
    target_branch: opts.branch,
    commit_message: opts.message.slice(0, 500),
    author: authorFor(fragmentName),
    files: opts.files.map((f, i) => ({
      path: f.path,
      operation: f.op,
      content_id: `b${i}`,
      mode: "100644",
    })),
  };
  if (opts.expectedTargetSha) {
    metadata.expected_target_sha = opts.expectedTargetSha;
    metadata.expected_head_sha = opts.expectedTargetSha;
  }
  const packBody = commitPackBody(metadata, opts.files);
  const resp = await csFetch(env, {
    repo, scopes: ["git:write"], sub: "fragment-runtime",
    path: `/api/repos/${repoSeg(repo)}/commit-pack`,
    init: {
      method: "POST",
      headers: { "content-type": "application/x-ndjson", accept: "application/json" },
      body: packBody,
    },
    initTimeoutMs: 120_000,
  });
  const body = await resp.json().catch(() => null);
  const result = body && body.result ? body.result : null;
  const commit = body && body.commit ? body.commit : null;
  if (!result || !commit || typeof commit.commit_sha !== "string" || typeof result.new_sha !== "string") {
    throw new CodeStorageError("bad-response", `commit-pack ack malformed for ${repo}:${JSON.stringify(body).slice(0, 200)}`);
  }
  if (result.success === false || (result.status && result.status !== "ok")) {
    throw new CodeStorageError("conflict", `commit-pack result ${result.status}: ${String(result.message || "")}`, 409);
  }
  return { commitSha: commit.commit_sha, newSha: result.new_sha, oldSha: String(result.old_sha || ""), status: String(result.status || "ok") };
}

// ---- repo lifecycle ----

// Repo identity is the URL-FORM value, not the human name (live-verified
// 2026-09-11): createRepo answers {repoId, url, defaultBranch, createdAt}
// and repo-scoped calls succeed only with the `url` in the path AND the
// JWT repo claim. The caller must persist what this returns as THE repo
// identifier; using the requested name gets 404 "repository not found".
export async function ensureRepo(env, repo: string): Promise<string> {
  // create-or-exists, then resolve the authoritative url identity. The
  // real service (spec + live-verified 2026-09-15): POST /api/repos replies
  // {repo_id, repo_name, http_url, message} (201) or 409 when the name
  // exists; the JWT repo claim/path identity comes from
  // GET /api/repo-urls/{repo_id} -> {repo_id, repo_name, url}. The url is
  // what every repo-scoped call and minted token must carry (live-verified:
  // other spellings 404/403).
  let repoId = "";
  try {
    const resp = await csFetch(env, {
      scopes: ["repo:write"], sub: "fragment-runtime", repo,
      path: `/api/repos`,
      init: { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ id: repo, default_branch: "main" }) },
    });
    const body = await resp.json().catch(() => null);
    repoId = body && typeof body.repo_id === "string" ? body.repo_id : "";
  } catch (e) {
    if (!(e instanceof CodeStorageError) || e.status !== 409) throw e;
    // name already taken in our org: resolve the existing repo via the
    // org-level listing (repo_name + url ride every row — spec-verified)
  }
  if (!repoId) {
    const listResp = await csFetch(env, {
      scopes: ["org:read"], sub: "fragment-runtime", repo,
      path: `/api/repos`,
      init: { method: "GET" },
    });
    const lb = await listResp.json().catch(() => null);
    const hit = lb && Array.isArray(lb.repos) ? lb.repos.find((r: any) => r.repo_name === repo) : null;
    const listedUrl = hit && typeof hit.url === "string" ? hit.url : "";
    if (!listedUrl) {
      throw new CodeStorageError("bad-response", `repo ${repo} exists but the org listing resolved no url for it: ${JSON.stringify(lb).slice(0, 200)}`);
    }
    return listedUrl;
  }
  const urlResp = await csFetch(env, {
    scopes: ["org:read"], sub: "fragment-runtime", repo: repoId,
    path: `/api/repo-urls/${encodeURIComponent(repoId)}`,
    init: { method: "GET" },
  });
  const ub = await urlResp.json().catch(() => null);
  const url = ub && typeof ub.url === "string" && ub.url ? ub.url : "";
  if (!url) {
    throw new CodeStorageError("bad-response", `repo-urls for ${repo} (${repoId}) returned no url identity: ${JSON.stringify(ub).slice(0, 200)}`);
  }
  return url;
}

// ---- webhook verification (their documented scheme, WebCrypto) ----

export type WebhookVerdict = { ok: true; event: string } | { ok: false; reason: string };

export const WEBHOOK_MAX_AGE_SEC = 300; // their SDK default freshness window

export async function verifyWebhookDelivery(rawBody: string, signatureHeader: string, secret: string, nowMs = Date.now()): Promise<WebhookVerdict> {
  const m = /^t=(\d+),sha256=([0-9a-f]{64})$/.exec(String(signatureHeader || "").trim());
  if (!m) return { ok: false, reason: "invalid signature header format" };
  const t = parseInt(m[1], 10);
  const age = Math.floor(nowMs / 1000) - t;
  if (age > WEBHOOK_MAX_AGE_SEC) return { ok: false, reason: `webhook timestamp too old (${age}s)` };
  if (age < -WEBHOOK_MAX_AGE_SEC) return { ok: false, reason: "webhook timestamp is in the future" };
  const key = await crypto.subtle.importKey(
    "raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"],
  );
  const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(`${t}.${rawBody}`));
  const got = [...new Uint8Array(mac)].map((b) => b.toString(16).padStart(2, "0")).join("");
  if (got !== m[2].toLowerCase()) return { ok: false, reason: "invalid signature" };
  return { ok: true, event: "" }; // event filled by caller from X-Pierre-Event
}

// Deterministic delivery-dedup key. FLAGGED CONTRADICTION: code.storage
// documents no delivery-ID header (only Content-Type/User-Agent/
// X-Pierre-Event/X-Pierre-Signature), so the ROADMAP's "dedup by delivery
// ID" is implemented as a content-derived key over the identity of the
// event itself (event type + repo + ref + before + after + timestamp):
// identical redeliveries collapse; distinct pushes never collide.
export function webhookDedupeKey(event: string, payload: any): string {
  const p = payload || {};
  const repoUrl = p.repository && typeof p.repository.url === "string" ? p.repository.url : "";
  return `${event}|${repoUrl}|${p.ref || ""}|${p.before || ""}|${p.after || ""}|${p.pushed_at || p.completed_at || p.started_at || ""}`;
}
