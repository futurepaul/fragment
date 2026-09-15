// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { importPKCS8, SignJWT } from "jose";
class CodeStorageError extends Error {
  kind;
  status;
  // HTTP status for api/conflict; 0 otherwise
  constructor(kind, message, status = 0) {
    super(message);
    this.name = "CodeStorageError";
    this.kind = kind;
    this.status = status;
  }
}
const CS_SCOPES = ["org:read", "repo:write", "git:read", "git:write"];
function csConfig(env) {
  const keyPem = String(env.PIERRE_PRIVATE_KEY || "").replace(/\\n/g, "\n").trim();
  const org = String(env.CODESTORAGE_ORG_NAME || "").trim();
  if (!keyPem || !org) {
    throw new CodeStorageError(
      "not-configured",
      "code.storage not configured on this host (set CELLD_VAR_PIERRE_PRIVATE_KEY + CELLD_VAR_CODESTORAGE_ORG_NAME)"
    );
  }
  const apiUrl = String(env.CODESTORAGE_API_URL || "").replace(/\/+$/, "") || `https://api.${org}.code.storage`;
  return { org, keyPem, apiUrl };
}
function csConfigured(env) {
  return !!(env.PIERRE_PRIVATE_KEY && env.CODESTORAGE_ORG_NAME);
}
let cachedKeyPem = "";
let cachedKey = null;
async function mintCsJwt(env, opts) {
  const cfg = csConfig(env);
  if (cachedKeyPem !== cfg.keyPem || !cachedKey) {
    try {
      cachedKey = await importPKCS8(cfg.keyPem, "ES256");
      cachedKeyPem = cfg.keyPem;
    } catch (e) {
      throw new CodeStorageError("bad-key", `PIERRE_PRIVATE_KEY is not a usable PKCS8 ES256 key: ${String(e.message || e)}`);
    }
  }
  const now = Math.floor(Date.now() / 1e3);
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
    exp: now + opts.ttlSec
  }).setProtectedHeader({ alg: "ES256", typ: "JWT" }).sign(cachedKey);
}
async function csFetch(env, opts) {
  const cfg = csConfig(env);
  const jwt = await mintCsJwt(env, { repo: opts.repo, scopes: opts.scopes, sub: opts.sub, ttlSec: 300 });
  const resp = await fetch(`${cfg.apiUrl}${opts.path}`, {
    ...opts.init,
    headers: { authorization: `Bearer ${jwt}`, ...opts.init.headers || {} },
    signal: AbortSignal.timeout(opts.initTimeoutMs ?? 3e4)
  });
  if (!resp.ok) {
    const body = await resp.text().catch(() => "");
    if (resp.status === 409 || resp.status === 400) {
      if (body.includes("precondition_failed")) {
        throw new CodeStorageError("conflict", `expected_target_sha mismatch: branch moved (${resp.status})`, resp.status);
      }
    }
    if (resp.status === 413) throw new CodeStorageError("payload-too-large", `code.storage rejected chunk size: ${body.slice(0, 200)}`, 413);
    throw new CodeStorageError("api", `code.storage ${resp.status} ${opts.path}: ${body.slice(0, 300)}`, resp.status);
  }
  return resp;
}
function repoSeg(repo) {
  return repo.includes("/") ? encodeURIComponent(repo) : repo;
}
async function readBranchHead(env, repo, branch) {
  const resp = await csFetch(env, {
    repo,
    scopes: ["git:read"],
    sub: "fragment-runtime",
    path: `/api/repos/${repoSeg(repo)}/branch?name=${encodeURIComponent(branch)}`,
    init: { method: "GET" }
  });
  const body = await resp.json().catch(() => null);
  const head = body && body.branch && typeof body.branch.head_sha === "string" ? body.branch.head_sha : null;
  if (!head || !/^[0-9a-f]{40}$/.test(head)) {
    throw new CodeStorageError("bad-response", `branch read for ${repo}/${branch} returned no head_sha`);
  }
  return { name: branch, headSha: head };
}
const TREE_PAGE_LIMIT = 500;
async function listTree(env, repo, ref) {
  const out = [];
  let cursor = "";
  for (let page = 0; ; page++) {
    if (page >= TREE_PAGE_LIMIT) {
      throw new CodeStorageError("api", `tree listing for ${repo}@${ref} exceeded ${TREE_PAGE_LIMIT} pages \u2014 refusing unbounded pagination`);
    }
    const q = new URLSearchParams({ ref, limit: "1000" });
    if (cursor) q.set("cursor", cursor);
    const resp = await csFetch(env, {
      repo,
      scopes: ["git:read"],
      sub: "fragment-runtime",
      path: `/api/repos/${repoSeg(repo)}/files/metadata?${q}`,
      init: { method: "GET" }
    });
    const body = await resp.json().catch(() => null);
    const files = body && Array.isArray(body.files) ? body.files : null;
    if (!files) throw new CodeStorageError("bad-response", `tree listing for ${repo}@${ref} returned no files array`);
    for (const f of files) {
      if (!f || typeof f.path !== "string" || typeof f.size !== "number" || typeof f.last_commit_sha !== "string") continue;
      if (f.type && f.type !== "blob") continue;
      out.push({ path: f.path, mode: String(f.mode || "100644"), type: "blob", size: f.size, lastCommitSha: f.last_commit_sha });
    }
    if (!body.has_more || !body.next_cursor) break;
    cursor = String(body.next_cursor);
  }
  return out;
}
async function streamFile(env, repo, ref, path) {
  const q = new URLSearchParams({ path, ref });
  return await csFetch(env, {
    repo,
    scopes: ["git:read"],
    sub: "fragment-runtime",
    path: `/api/repos/${repoSeg(repo)}/file?${q}`,
    init: { method: "GET" },
    initTimeoutMs: 12e4
    // large files stream; bound time, not bytes
  });
}
async function headFile(env, repo, ref, path) {
  const q = new URLSearchParams({ path, ref });
  let resp;
  try {
    resp = await csFetch(env, {
      repo,
      scopes: ["git:read"],
      sub: "fragment-runtime",
      path: `/api/repos/${repoSeg(repo)}/file?${q}`,
      init: { method: "HEAD" }
    });
  } catch (e) {
    if (e instanceof CodeStorageError && e.status === 404) return null;
    throw e;
  }
  return fileHeadFrom(resp, path);
}
function fileHeadFrom(resp, path) {
  const blobSha = String(resp.headers.get("x-blob-sha") || "");
  const lastCommitSha = String(resp.headers.get("x-last-commit-sha") || "");
  const len = parseInt(resp.headers.get("content-length") || "0", 10);
  if (!/^[0-9a-f]{40,64}$/.test(blobSha)) return null;
  return { size: Number.isSafeInteger(len) ? len : 0, blobSha, lastCommitSha };
}
const COMMIT_CHUNK_BYTES = 3 * 1024 * 1024;
function authorFor(fragmentName) {
  return { name: `fragment:${fragmentName}`, email: `${fragmentName}@fragment.runtime` };
}
function commitPackBody(metadata, files) {
  const lines = [JSON.stringify(metadata)];
  files.forEach((f, i) => {
    const contentId = `b${i}`;
    const chunks = f.op === "delete" ? [new Uint8Array(0)] : carve(f.bytes);
    for (let c = 0; c < chunks.length; c++) {
      lines.push(JSON.stringify({
        blob_chunk: { content_id: contentId, data: base64(chunks[c]), eof: c === chunks.length - 1 }
      }));
    }
  });
  return lines.join("\n") + "\n";
}
function carve(bytes) {
  if (bytes.byteLength === 0) return [new Uint8Array(0)];
  const out = [];
  for (let o = 0; o < bytes.byteLength; o += COMMIT_CHUNK_BYTES) {
    out.push(bytes.subarray(o, Math.min(o + COMMIT_CHUNK_BYTES, bytes.byteLength)));
  }
  return out;
}
function base64(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}
async function commitFiles(env, repo, fragmentName, opts) {
  if (!opts.files.length) throw new CodeStorageError("api", "commitFiles: empty file set");
  const metadata = {
    target_branch: opts.branch,
    commit_message: opts.message.slice(0, 500),
    author: authorFor(fragmentName),
    files: opts.files.map((f, i) => ({
      path: f.path,
      operation: f.op,
      content_id: `b${i}`,
      mode: "100644"
    }))
  };
  if (opts.expectedTargetSha) {
    metadata.expected_target_sha = opts.expectedTargetSha;
    metadata.expected_head_sha = opts.expectedTargetSha;
  }
  const packBody = commitPackBody(metadata, opts.files);
  const resp = await csFetch(env, {
    repo,
    scopes: ["git:write"],
    sub: "fragment-runtime",
    path: `/api/repos/${repoSeg(repo)}/commit-pack`,
    init: {
      method: "POST",
      headers: { "content-type": "application/x-ndjson", accept: "application/json" },
      body: packBody
    },
    initTimeoutMs: 12e4
  });
  const body = await resp.json().catch(() => null);
  const result = body && body.result ? body.result : null;
  const commit = body && body.commit ? body.commit : null;
  if (!result || !commit || typeof commit.commit_sha !== "string" || typeof result.new_sha !== "string") {
    throw new CodeStorageError("bad-response", `commit-pack ack malformed for ${repo}:${JSON.stringify(body).slice(0, 200)}`);
  }
  if (result.success === false || result.status && result.status !== "ok") {
    throw new CodeStorageError("conflict", `commit-pack result ${result.status}: ${String(result.message || "")}`, 409);
  }
  return { commitSha: commit.commit_sha, newSha: result.new_sha, oldSha: String(result.old_sha || ""), status: String(result.status || "ok") };
}
async function ensureRepo(env, repo) {
  let repoId = "";
  try {
    const resp = await csFetch(env, {
      scopes: ["repo:write"],
      sub: "fragment-runtime",
      repo,
      path: `/api/repos`,
      init: { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ id: repo, default_branch: "main" }) }
    });
    const body = await resp.json().catch(() => null);
    repoId = body && typeof body.repo_id === "string" ? body.repo_id : "";
  } catch (e) {
    if (!(e instanceof CodeStorageError) || e.status !== 409) throw e;
  }
  if (!repoId) {
    const listResp = await csFetch(env, {
      scopes: ["org:read"],
      sub: "fragment-runtime",
      repo,
      path: `/api/repos`,
      init: { method: "GET" }
    });
    const lb = await listResp.json().catch(() => null);
    const hit = lb && Array.isArray(lb.repos) ? lb.repos.find((r) => r.repo_name === repo) : null;
    const listedUrl = hit && typeof hit.url === "string" ? hit.url : "";
    if (!listedUrl) {
      throw new CodeStorageError("bad-response", `repo ${repo} exists but the org listing resolved no url for it: ${JSON.stringify(lb).slice(0, 200)}`);
    }
    return listedUrl;
  }
  const urlResp = await csFetch(env, {
    scopes: ["org:read"],
    sub: "fragment-runtime",
    repo: repoId,
    path: `/api/repo-urls/${encodeURIComponent(repoId)}`,
    init: { method: "GET" }
  });
  const ub = await urlResp.json().catch(() => null);
  const url = ub && typeof ub.url === "string" && ub.url ? ub.url : "";
  if (!url) {
    throw new CodeStorageError("bad-response", `repo-urls for ${repo} (${repoId}) returned no url identity: ${JSON.stringify(ub).slice(0, 200)}`);
  }
  return url;
}
const WEBHOOK_MAX_AGE_SEC = 300;
async function verifyWebhookDelivery(rawBody, signatureHeader, secret, nowMs = Date.now()) {
  const m = /^t=(\d+),sha256=([0-9a-f]{64})$/.exec(String(signatureHeader || "").trim());
  if (!m) return { ok: false, reason: "invalid signature header format" };
  const t = parseInt(m[1], 10);
  const age = Math.floor(nowMs / 1e3) - t;
  if (age > WEBHOOK_MAX_AGE_SEC) return { ok: false, reason: `webhook timestamp too old (${age}s)` };
  if (age < -WEBHOOK_MAX_AGE_SEC) return { ok: false, reason: "webhook timestamp is in the future" };
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"]
  );
  const mac = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(`${t}.${rawBody}`));
  const got = [...new Uint8Array(mac)].map((b) => b.toString(16).padStart(2, "0")).join("");
  if (got !== m[2].toLowerCase()) return { ok: false, reason: "invalid signature" };
  return { ok: true, event: "" };
}
function webhookDedupeKey(event, payload) {
  const p = payload || {};
  const repoUrl = p.repository && typeof p.repository.url === "string" ? p.repository.url : "";
  return `${event}|${repoUrl}|${p.ref || ""}|${p.before || ""}|${p.after || ""}|${p.pushed_at || p.completed_at || p.started_at || ""}`;
}
export {
  COMMIT_CHUNK_BYTES,
  CS_SCOPES,
  CodeStorageError,
  WEBHOOK_MAX_AGE_SEC,
  commitFiles,
  csConfig,
  csConfigured,
  ensureRepo,
  fileHeadFrom,
  headFile,
  listTree,
  mintCsJwt,
  readBranchHead,
  streamFile,
  verifyWebhookDelivery,
  webhookDedupeKey
};
