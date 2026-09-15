// mock-codestorage — a local stand-in for the code.storage HTTP API, used
// by the runtime unit tests and `scripts/dev`. It implements the DOCUMENTED
// API shape (https://code.storage/docs + openapi.json) for the surface the
// runtime and CLI touch, plus a few /__test/* levers that let tests drive
// external-writer and webhook-delivery scenarios. Production code paths
// never reference the levers — they exist only in this file.
//
//   node mock-codestorage.mjs [port] [stateDir]
//
// stateDir is optional; in-memory state when omitted (unit tests).
//
// Repo identity follows the live service (probed 2026-09-11): createRepo
// returns {repoId, url, defaultBranch, createdAt} and every repo-scoped
// call (/api/repos/{repo}/...) must use the URL-FORM value — the human
// name gets 404 "repository not found". The mock mints a UUID url per
// repo and enforces it on the API routes; the /__test levers accept
// either form (they are not the API surface).
import { createServer } from "node:http";
import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { createHash, createHmac, randomUUID } from "node:crypto";
import { importPKCS8, importSPKI, jwtVerify } from "jose";

const PORT = parseInt(process.argv[2] || process.env.MOCK_PORT || "9940", 10);
const STATE_DIR = process.argv[3] || null;

// ---- org key: the mock verifies JWTs against the org public key ----
// MOCK_ORG_PUB (PEM SPKI) must be set; tests/dev generate the pair and
// hand the private half to the runtime, the public half here — the same
// trust split the real service has (dashboard holds only the pubkey).
// MOCK_ORG_PRIV (the private half) is the dev convenience spelling.
const ORG = process.env.MOCK_ORG_NAME || "fragment-dev";
const ORG_PUB_PEM = process.env.MOCK_ORG_PUB || "";
let orgKey = null;

// ---- repo model ----
// repos: url -> {name, url, repoId, defaultBranch, ephemeral: Set,
//                commits: sha -> {parents, tree, message, at, authorName},
//                blobs: blobSha -> Buffer, branches: name -> sha}
// byName: human name -> url (create-idempotence + test levers)
const repos = new Map();
const byName = new Map();
// registered push webhooks (the real service's dashboard registration):
// url -> { repoUrl, secret }; every branch-moving operation delivers a
// signed push to each registered URL for that repo — the documented
// X-Pierre-Signature scheme, so the runtime's fast path (webhook-driven
// pin refresh + files triggers) runs with production fidelity in dev.
const webhooks = new Map();
// race lever (test-only): when armed for a repo, the NEXT commit-pack for
// it applies this external change BEFORE evaluating CAS — a writer landing
// between the client's head read and its commit
const raceArm = new Map(); // url -> {changes, message}

function sha1hex(buf) {
  return createHash("sha1").update(buf).digest("hex");
}
function gitBlobSha(bytes) {
  return sha1hex(Buffer.concat([Buffer.from(`blob ${bytes.length}\0`), bytes]));
}
function sha40(...parts) {
  return createHash("sha1").update(parts.join("|")).digest("hex").padEnd(40, "0").slice(0, 40);
}
function nanoid(len = 20) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
  let out = "";
  for (let i = 0; i < len; i++) out += alphabet[Math.floor(Math.random() * alphabet.length)];
  return out;
}
function newRepo(name, defaultBranch) {
  const r = {
    name, url: randomUUID(), repoId: nanoid(), defaultBranch: defaultBranch || "main",
    ephemeral: new Set(), branches: new Map(), commits: new Map(), blobs: new Map(),
  };
  repos.set(r.url, r);
  byName.set(name, r.url);
  return r;
}
// resolve a repo by url (the API identity) — null when unknown
function repoByUrl(url) {
  return repos.get(url) || null;
}
// resolve by either form — test levers only (they drive state, not identity)
function repoAny(idOrUrl) {
  return repoByUrl(idOrUrl) || (byName.has(idOrUrl) ? repos.get(byName.get(idOrUrl)) : null);
}

// ---- persistence (optional; scripts/dev uses it so restarts keep repos) ----
function persist() {
  if (!STATE_DIR) return;
  mkdirSync(STATE_DIR, { recursive: true });
  const dump = [];
  for (const r of repos.values()) {
    dump.push({
      name: r.name, url: r.url, repoId: r.repoId, defaultBranch: r.defaultBranch,
      ephemeral: [...r.ephemeral],
      branches: [...r.branches.entries()],
      commits: [...r.commits.entries()].map(([sha, c]) => [sha, { parents: c.parents, message: c.message, at: c.at, authorName: c.authorName, tree: [...c.tree.entries()] }]),
      blobs: [...r.blobs.entries()].map(([sha, b]) => [sha, b.toString("base64")]),
    });
  }
  writeFileSync(join(STATE_DIR, "repos.json"), JSON.stringify(dump));
  writeFileSync(join(STATE_DIR, "webhooks.json"), JSON.stringify([...webhooks.entries()]));
}
function restore() {
  if (!STATE_DIR) return;
  if (existsSync(join(STATE_DIR, "webhooks.json"))) {
    try { for (const [url, wh] of JSON.parse(readFileSync(join(STATE_DIR, "webhooks.json"), "utf8"))) webhooks.set(url, wh); } catch {}
  }
  if (!existsSync(join(STATE_DIR, "repos.json"))) return;
  for (const d of JSON.parse(readFileSync(join(STATE_DIR, "repos.json"), "utf8"))) {
    const r = {
      name: d.name, url: d.url, repoId: d.repoId, defaultBranch: d.defaultBranch || "main",
      ephemeral: new Set(d.ephemeral || []),
      branches: new Map(d.branches),
      commits: new Map((d.commits || []).map(([sha, c]) => [sha, { parents: c.parents, message: c.message, at: c.at, authorName: c.authorName, tree: new Map(c.tree) }])),
      blobs: new Map((d.blobs || []).map(([sha, b]) => [sha, Buffer.from(b, "base64")])),
    };
    repos.set(r.url, r);
    byName.set(r.name, r.url);
  }
}

// resolve a ref (40-hex sha or branch name) to a commit record; null when unknown
function commitAt(r, ref) {
  if (!ref) ref = r.defaultBranch;
  const sha = /^[0-9a-f]{40}$/.test(ref) ? ref : r.branches.get(ref);
  return sha ? r.commits.get(sha) || null : null;
}
// the sha a ref names (branch lookup or the sha itself)
function shaOfRef(r, ref) {
  if (!ref) ref = r.defaultBranch;
  return /^[0-9a-f]{40}$/.test(ref) ? ref : r.branches.get(ref) || null;
}

// first-parent ancestry: is `maybeAncestor` on the first-parent chain of head?
function isAncestor(r, maybeAncestor, headSha) {
  let cur = headSha;
  // bounded by commit count + 1 — a malformed cycle still terminates
  for (let i = 0; cur && i <= r.commits.size; i++) {
    if (cur === maybeAncestor) return true;
    const c = r.commits.get(cur);
    cur = c && c.parents[0];
  }
  return false;
}

// ---- commit application (commit-pack, merge, restore, __test levers) ----
// `parents` (merge/restore): [currentTip, treeSource] — the NEW commit's
// tree comes from the LAST parent (source_ref for merges, base_ref for
// restores); without `parents` the tree is the current head's.
function applyCommit(r, { branch, expected, message, changes, authorName, parents }) {
  const oldSha = r.branches.get(branch) || "0".repeat(40);
  if (expected !== undefined && expected !== null && expected !== oldSha) {
    return { status: 409, body: casFail(branch, oldSha) };
  }
  const head = oldSha !== "0".repeat(40) ? oldSha : null;
  const treeSource = parents && parents.length ? parents[parents.length - 1] : head;
  const base = treeSource ? r.commits.get(treeSource) : null;
  const tree = new Map(base ? base.tree.entries() : []);
  let blobCount = 0;
  for (const ch of changes) {
    if (ch.op === "delete") {
      tree.delete(ch.path);
      continue;
    }
    // the __test lever ships base64; the commit-pack path ships Buffers
    const bytes = Buffer.isBuffer(ch.bytes) ? ch.bytes : Buffer.from(ch.bytes, "base64");
    const blobSha = gitBlobSha(bytes);
    r.blobs.set(blobSha, bytes);
    tree.set(ch.path, { blobSha, size: bytes.length, mode: "100644", lastCommit: null });
    blobCount++;
  }
  const newSha = sha40(r.url, branch, oldSha, message, JSON.stringify([...tree.entries()].map(([p, v]) => [p, v.blobSha])));
  // per-path last-commit: the sha of the newest commit that touched each
  // path (the spec shape the CLI's change detection keys on)
  for (const entry of tree.values()) {
    if (entry.lastCommit === null) entry.lastCommit = newSha;
  }
  const parentList = head
    ? [head, ...(parents || []).filter((p) => p && p !== head)]
    : (parents || []).filter((p) => p);
  r.commits.set(newSha, { parents: parentList, tree, message, at: Date.now(), authorName: authorName || "mock" });
  r.branches.set(branch, newSha);
  persist();
  deliverPushes(r, branch, oldSha, newSha);
  return {
    status: 201,
    body: {
      commit: { commit_sha: newSha, tree_sha: sha40("tree", newSha), target_branch: branch, pack_bytes: 1024, blob_count: blobCount },
      result: { target_branch: branch, branch, old_sha: oldSha, new_sha: newSha, success: true, status: "ok" },
    },
  };
}

// fire-and-forget signed push delivery to the repo's registered webhooks
function deliverPushes(r, branch, before, after) {
  if (before === after) return;
  for (const [url, wh] of webhooks.entries()) {
    if (wh.repoUrl !== r.url) continue;
    const t = Math.floor(Date.now() / 1000);
    const body = JSON.stringify({
      repository: { id: r.repoId, url: r.url },
      ref: `refs/heads/${branch}`,
      before,
      after,
      pushed_at: new Date().toISOString(),
    });
    const mac = createHmac("sha256", wh.secret).update(`${t}.${body}`).digest("hex");
    fetch(url, {
      method: "POST",
      headers: { "content-type": "application/json", "x-pierre-event": "push", "x-pierre-signature": `t=${t},sha256=${mac}` },
      body,
      signal: AbortSignal.timeout(5000),
    }).catch(() => { /* delivery is best-effort here; the real service retries */ });
  }
}

function casFail(branch, oldSha) {
  return {
    commit: { commit_sha: "0".repeat(40), tree_sha: "0".repeat(40), target_branch: branch, pack_bytes: 0, blob_count: 0 },
    result: { target_branch: branch, branch, old_sha: oldSha, new_sha: oldSha, success: false, status: "precondition_failed", message: "expected branch head did not match current tip" },
  };
}

// ---- auth ----
async function checkAuth(req, repo, needScope) {
  if (!orgKey) return { ok: true }; // no key configured: open (local dev)
  const h = req.headers.authorization || "";
  if (!h.startsWith("Bearer ")) return { ok: false, status: 401, body: { error: "missing bearer" } };
  try {
    const { payload } = await jwtVerify(h.slice(7), orgKey, { algorithms: ["ES256"] });
    if (payload.iss !== ORG) return { ok: false, status: 401, body: { error: "wrong iss" } };
    if (typeof payload.repo !== "string" || !payload.repo) return { ok: false, status: 403, body: { error: "missing repo claim (the real service rejects claim-less tokens on every path)" } };
    const scopes = Array.isArray(payload.scopes) ? payload.scopes : [];
    if (!scopes.includes(needScope)) return { ok: false, status: 403, body: { error: `missing scope ${needScope}` } };
    if (repo !== null && payload.repo !== repo) return { ok: false, status: 403, body: { error: "repo claim mismatch" } };
    return { ok: true };
  } catch (e) {
    return { ok: false, status: 401, body: { error: String(e.message || e) } };
  }
}

// ---- request reading ----
function readBody(req) {
  return new Promise((resolve) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => resolve(Buffer.concat(chunks)));
  });
}

const server = createServer(async (req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  const path = decodeURIComponent(url.pathname);
  const send = (status, body, headers = {}) => {
    res.writeHead(status, { "content-type": headers["content-type"] || "application/json", ...headers });
    res.end(headers.raw ? body : JSON.stringify(body));
  };

  try {
    if (path === "/healthz") return send(200, { ok: true });

    // ---- org-level: create repo ----
    // raw HTTP shapes per the org OpenAPI spec (live-verified 2026-09-15):
    // create replies {repo_id, repo_name, http_url, message}; the url
    // identity comes from GET /api/repo-urls/{repo_id}; the org listing
    // rows carry repo_name + url.
    if (path === "/api/repos" && req.method === "POST") {
      const a = await checkAuth(req, null, "repo:write");
      if (!a.ok) return send(a.status, a.body);
      const body = JSON.parse((await readBody(req)).toString() || "{}");
      if (!body.id) return send(400, { error: "id required" });
      if (byName.has(body.id)) {
        // dev mode (persisted state) is idempotent — a wiped cell re-init
        // must find the same repo; unit mode treats re-create as 409
        if (!STATE_DIR) return send(409, { error: "repository already exists" });
        const r = repos.get(byName.get(body.id));
        return send(200, { repo_id: r.repoId, repo_name: body.id, http_url: r.url, message: "repository exists" });
      }
      const r = newRepo(body.id, body.default_branch);
      persist();
      return send(201, { repo_id: r.repoId, repo_name: body.id, http_url: r.url, message: "repository created" });
    }
    if (path === "/api/repos" && req.method === "GET") {
      const a = await checkAuth(req, null, "org:read");
      if (!a.ok) return send(a.status, a.body);
      return send(200, { repos: [...repos.values()].map((r) => ({ repo_id: r.repoId, repo_name: r.name, url: r.url, default_branch: r.defaultBranch, created_at: new Date(r.createdAt || 0).toISOString() })), has_more: false });
    }
    if (path.startsWith("/api/repo-urls/") && req.method === "GET") {
      const a = await checkAuth(req, null, "org:read");
      if (!a.ok) return send(a.status, a.body);
      const repoId = decodeURIComponent(path.slice("/api/repo-urls/".length));
      const r = [...repos.values()].find((x) => x.repoId === repoId);
      if (!r) return send(404, { error: "repository not found" });
      return send(200, { repo_id: r.repoId, repo_name: r.name, url: r.url });
    }

    // ---- repo-scoped routes: /api/repos/{repo}/... ({repo} = url-form id) ----
    const m = path.match(/^\/api\/repos\/([^/]+)\/(.+)$/);
    if (m) {
      const repoSeg = m[1];
      const sub = m[2];
      const r = repoByUrl(repoSeg);
      if (!r) return send(404, { error: "repository not found" });

      if (sub === "branch" && req.method === "GET") {
        const a = await checkAuth(req, repoSeg, "git:read");
        if (!a.ok) return send(a.status, a.body);
        const name = url.searchParams.get("name") || "main";
        const sha = r.branches.get(name);
        if (!sha) return send(404, { error: "branch not found" });
        return send(200, { branch: { name, head_sha: sha, created_at: new Date(0).toISOString() } });
      }

      if (sub === "files/metadata" && req.method === "GET") {
        const a = await checkAuth(req, repoSeg, "git:read");
        if (!a.ok) return send(a.status, a.body);
        const ref = url.searchParams.get("ref") || r.defaultBranch;
        const commit = commitAt(r, ref);
        if (!commit) return send(404, { error: "ref not found" });
        const files = [...commit.tree.entries()].map(([p, v]) => ({
          path: p, mode: v.mode, type: "blob", size: v.size, last_commit_sha: v.lastCommit || v.blobSha.slice(0, 40),
        }));
        return send(200, { files, commits: {}, ref, has_more: false, next_cursor: "" });
      }

      if (sub === "file" && (req.method === "GET" || req.method === "HEAD")) {
        const a = await checkAuth(req, repoSeg, "git:read");
        if (!a.ok) return send(a.status, a.body);
        const filePath = url.searchParams.get("path") || "";
        const ref = url.searchParams.get("ref") || r.defaultBranch;
        const refSha = shaOfRef(r, ref);
        const commit = refSha ? r.commits.get(refSha) : null;
        const entry = commit ? commit.tree.get(filePath) : null;
        if (!entry) return send(404, { error: "file not found" });
        const bytes = r.blobs.get(entry.blobSha) || Buffer.alloc(0);
        const headers = {
          "content-length": String(bytes.length),
          "content-type": "application/octet-stream",
          etag: `"${entry.blobSha}"`,
          "x-blob-sha": entry.blobSha,
          "x-last-commit-sha": refSha,
          "accept-ranges": "bytes",
        };
        if (req.method === "HEAD") { res.writeHead(200, headers); return res.end(); }
        res.writeHead(200, headers);
        return res.end(bytes);
      }

      if (sub === "commit-pack" && req.method === "POST") {
        const a = await checkAuth(req, repoSeg, "git:write");
        if (!a.ok) return send(a.status, a.body);
        // armed race fires before CAS evaluation, once
        if (raceArm.has(repoSeg)) {
          const arm = raceArm.get(repoSeg);
          raceArm.delete(repoSeg);
          applyCommit(r, { branch: "main", expected: null, message: arm.message || "racing writer", changes: arm.changes });
        }
        const raw = (await readBody(req)).toString();
        const lines = raw.split("\n").filter((l) => l.trim());
        if (!lines.length) return send(400, { error: "empty pack" });
        let meta;
        try {
          meta = JSON.parse(lines[0]);
        } catch { return send(400, { error: "bad metadata line" }); }
        // tolerate both {metadata:{...}} and a bare metadata object
        if (meta && typeof meta === "object" && meta.metadata === undefined) {
          return send(400, { error: "first payload must be metadata", result: { status: "invalid", message: "first payload must be metadata" } });
        }
        if (meta && typeof meta === "object" && meta.metadata) meta = meta.metadata;
        if (!meta || typeof meta !== "object" || !meta.target_branch) {
          return send(400, { error: "metadata line needs target_branch" });
        }
        const chunksById = new Map(); // content_id -> Buffer[] fragments
        for (let i = 1; i < lines.length; i++) {
          let line;
          try { line = JSON.parse(lines[i]); } catch { return send(400, { error: `bad line ${i}` }); }
          const bc = line.blob_chunk;
          if (!bc || typeof bc.content_id !== "string") return send(400, { error: "bad blob_chunk" });
          const data = Buffer.from(bc.data || "", "base64");
          // documented decoded-chunk limit: 4 MiB
          if (data.length > 4 * 1024 * 1024) {
            return send(413, { result: { status: "payload_too_large", success: false, message: `blob chunk size ${data.length} exceeds maximum 4194304` } });
          }
          if (!chunksById.has(bc.content_id)) chunksById.set(bc.content_id, []);
          chunksById.get(bc.content_id).push({ data, eof: !!bc.eof });
        }
        const expected = meta.expected_target_sha || meta.expected_head_sha || null;
        const changes = [];
        for (const f of meta.files || []) {
          if (f.operation === "delete") {
            changes.push({ op: "delete", path: f.path });
            continue;
          }
          const parts = chunksById.get(f.content_id) || [];
          // the real service requires each content stream to end with an
          // eof:true chunk (live-verified: eof:false finals → 400
          // "incomplete content stream") — enforce it here or the suite
          // passes while prod fails
          if (!parts.length || !parts[parts.length - 1].eof) {
            return send(400, { error: `incomplete content stream for ${f.path}` });
          }
          const bytes = Buffer.concat(parts.map((p) => p.data));
          changes.push({ op: "upsert", path: f.path, bytes });
        }
        const out = applyCommit(r, {
          branch: meta.target_branch,
          expected: expected === null ? null : (expected === "" ? "0".repeat(40) : expected),
          message: meta.commit_message || "mock commit",
          authorName: meta.author && meta.author.name,
          changes,
        });
        return send(out.status, out.body);
      }

      // ---- branch create (first deploy / preview ephemeral refs) ----
      if (sub === "branches/create" && req.method === "POST") {
        const a = await checkAuth(req, repoSeg, "git:write");
        if (!a.ok) return send(a.status, a.body);
        const body = JSON.parse((await readBody(req)).toString() || "{}");
        const baseSha = shaOfRef(r, body.base_ref);
        if (!baseSha || !r.commits.has(baseSha)) return send(404, { error: `base_ref not found: ${body.base_ref}` });
        if (!body.target_branch) return send(400, { error: "target_branch required" });
        if (body.target_branch === r.defaultBranch) {
          return send(409, { error: "cannot replace the default branch" });
        }
        if (r.branches.has(body.target_branch)) {
          return send(409, { error: `branch already exists: ${body.target_branch}` });
        }
        r.branches.set(body.target_branch, baseSha);
        if (body.target_is_ephemeral) r.ephemeral.add(body.target_branch);
        persist();
        deliverPushes(r, body.target_branch, "0".repeat(40), baseSha);
        return send(201, { commit_sha: baseSha, branch: body.target_branch, ephemeral: !!body.target_is_ephemeral });
      }

      // ---- merge (deploy: move live to main's tip, ff-prefer) ----
      if (sub === "merge" && req.method === "POST") {
        const a = await checkAuth(req, repoSeg, "git:write");
        if (!a.ok) return send(a.status, a.body);
        const body = JSON.parse((await readBody(req)).toString() || "{}");
        const target = body.target_branch;
        const sourceSha = shaOfRef(r, body.source_ref);
        if (!target || !sourceSha || !r.commits.has(sourceSha)) return send(404, { error: "target_branch/source_ref not found" });
        const oldSha = r.branches.get(target);
        if (!oldSha) return send(404, { error: `branch not found: ${target}` });
        if (body.expected_target_sha && body.expected_target_sha !== oldSha) {
          return send(409, casFail(target, oldSha));
        }
        if (sourceSha === oldSha || isAncestor(r, oldSha, sourceSha)) {
          // fast-forward (or no-op): live just points at the source tip
          r.branches.set(target, sourceSha);
          persist();
          deliverPushes(r, target, oldSha, sourceSha);
          return send(200, {
            target: { branch: target, old_sha: oldSha, new_sha: sourceSha, strategy: "ff" },
            result: { success: true, status: "ok" },
          });
        }
        // merge commit: tree of the source, both parents recorded
        const out = applyCommit(r, {
          branch: target,
          expected: body.expected_target_sha || oldSha,
          message: body.commit_message || "merge",
          authorName: body.author && body.author.name,
          parents: [oldSha, sourceSha],
          changes: [],
        });
        if (out.status !== 201) return send(out.status, out.body);
        return send(200, {
          target: { branch: target, old_sha: oldSha, new_sha: out.body.result.new_sha, strategy: "merge_commit" },
          result: { success: true, status: "ok" },
        });
      }

      // ---- restore-commit (rollback: append a commit whose tree matches base) ----
      if (sub === "restore-commit" && req.method === "POST") {
        const a = await checkAuth(req, repoSeg, "git:write");
        if (!a.ok) return send(a.status, a.body);
        const raw = (await readBody(req)).toString();
        const lines = raw.split("\n").filter((l) => l.trim());
        if (!lines.length) return send(400, { error: "empty body" });
        let meta;
        try { meta = JSON.parse(lines[0]); } catch { return send(400, { error: "bad metadata line" }); }
        if (meta && typeof meta === "object" && meta.metadata) meta = meta.metadata;
        if (!meta || typeof meta !== "object" || !meta.target_branch) {
          return send(400, { error: "metadata line needs target_branch" });
        }
        const target = meta.target_branch;
        const baseSha = shaOfRef(r, meta.base_ref);
        if (!target || !baseSha || !r.commits.has(baseSha)) return send(404, { error: "target_branch/base_ref not found" });
        const oldSha = r.branches.get(target);
        if (!oldSha) return send(404, { error: `branch not found: ${target}` });
        if (!isAncestor(r, baseSha, oldSha)) {
          return send(412, { error: `base_ref ${meta.base_ref} is not an ancestor of ${target}'s tip` });
        }
        if (meta.expected_target_sha && meta.expected_target_sha !== oldSha) {
          return send(409, casFail(target, oldSha));
        }
        const out = applyCommit(r, {
          branch: target,
          expected: meta.expected_target_sha || oldSha,
          message: meta.commit_message || "restore",
          authorName: meta.author && meta.author.name,
          parents: [oldSha, baseSha],
          changes: [],
        });
        return send(out.status, out.body);
      }

      // ---- commit history (deploy history on live; `fragment drafts`) ----
      if (sub === "commits" && req.method === "GET") {
        const a = await checkAuth(req, repoSeg, "git:read");
        if (!a.ok) return send(a.status, a.body);
        const ref = url.searchParams.get("ref") || r.defaultBranch;
        const limit = Math.min(parseInt(url.searchParams.get("limit") || "30", 10) || 30, 100);
        let cur = shaOfRef(r, ref);
        if (!cur || !r.commits.has(cur)) return send(404, { error: "ref not found" });
        const out = [];
        while (cur && out.length < limit) {
          const c = r.commits.get(cur);
          if (!c) break;
          out.push({ sha: cur, message: c.message, author_name: c.authorName || "mock", date: new Date(c.at).toISOString() });
          cur = c.parents[0];
        }
        return send(200, { commits: out });
      }
    }

    // ---- test levers (NOT part of the documented API; test/dev only).
    // They accept the repo by name OR url — they drive state, not identity. ----
    if (path === "/__test/state" && req.method === "GET") {
      const out = {};
      for (const r of repos.values()) {
        out[r.name] = {
          url: r.url,
          branches: Object.fromEntries(r.branches),
          ephemeral: [...r.ephemeral],
          commitCount: r.commits.size,
          // path -> blobSha per branch head (what "the repo contains")
          heads: Object.fromEntries([...r.branches.entries()].map(([b, sha]) => [b, [...(r.commits.get(sha)?.tree.entries() || []).map(([p, v]) => [p, v.blobSha])]])),
        };
      }
      return send(200, out);
    }
    if (path === "/__test/commit-count" && req.method === "GET") {
      const r = repoAny(url.searchParams.get("repo") || "");
      return send(200, { count: r ? r.commits.size : 0 });
    }
    if (path === "/__test/webhook-register" && req.method === "POST") {
      const body = JSON.parse((await readBody(req)).toString() || "{}");
      const r = repoAny(body.repo);
      if (!r) return send(404, { error: "no such repo" });
      if (!body.url || !body.secret) return send(400, { error: "url and secret required" });
      webhooks.set(body.url, { repoUrl: r.url, secret: String(body.secret) });
      persist();
      return send(200, { ok: true, repo: r.url, url: body.url });
    }
    if (path === "/__test/race" && req.method === "POST") {
      const body = JSON.parse((await readBody(req)).toString() || "{}");
      const r = repoAny(body.repo);
      if (!r) return send(404, { error: "no such repo" });
      raceArm.set(r.url, { changes: body.changes || [], message: body.message });
      return send(200, { ok: true, armed: r.url });
    }
    if (path === "/__test/external-commit" && req.method === "POST") {
      // simulate a CLI/external writer: commits WITHOUT CAS expectations,
      // exactly like a storage-token-holding client would
      const body = JSON.parse((await readBody(req)).toString() || "{}");
      const r = repoAny(body.repo);
      if (!r) return send(404, { error: "no such repo" });
      const out = applyCommit(r, { branch: body.branch || "main", expected: null, message: body.message || "external", changes: body.changes });
      return send(out.status, out.body);
    }

    return send(404, { error: `no such route: ${path}` });
  } catch (e) {
    return send(500, { error: String(e && e.stack || e) });
  }
});

async function start() {
  if (ORG_PUB_PEM) {
    orgKey = await importSPKI(ORG_PUB_PEM, "ES256");
  } else if (process.env.MOCK_ORG_PRIV) {
    // convenience for tests: derive the public side from the private key
    const pk = await importPKCS8(process.env.MOCK_ORG_PRIV, "ES256");
    orgKey = pk;
  }
  restore();
  await new Promise((r) => server.listen(PORT, "127.0.0.1", r));
  const addr = server.address();
  // stdout contract: tests/scripts wait for this line (real port — 0 means ephemeral)
  console.log(`mock-codestorage listening on http://127.0.0.1:${addr.port} org=${ORG} auth=${!!orgKey}`);
}

// programmatic use from tests: import the factory instead of the singleton
export function mockExports() {
  return { repos, applyCommit, server };
}

const isMain = process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href;
if (isMain) start();
