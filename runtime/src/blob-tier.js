// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
import { sha256Hex } from "./auth.js";
const INLINE_LIMIT = 65536;
const READ_CEILING = 8 * 1024 * 1024;
const SHA_HEADER = "x-sha-256";
function tierConfig(env) {
  return {
    url: String(env.BLOBSD_URL || "").replace(/\/$/, ""),
    token: String(env.BLOBSD_INTERNAL_TOKEN || ""),
    publicUrl: String(env.BLOBSD_PUBLIC_URL || "").replace(/\/$/, ""),
    publicGet: String(env.BLOBSD_PUBLIC_GET || "") === "1"
  };
}
function requireTier(cell) {
  const cfg = tierConfig(cell.env);
  if (!cfg.url || !cfg.token) {
    throw new Error("blob tier not configured on this host (set CELLD_VAR_BLOBSD_URL + CELLD_VAR_BLOBSD_INTERNAL_TOKEN)");
  }
  return cfg;
}
function bearer(cfg, extraHeaders = {}) {
  return {
    authorization: `Bearer ${cfg.token}`,
    ...extraHeaders
  };
}
class TierError extends Error {
  status;
  constructor(message, status = 502) {
    super(message);
    this.status = status;
  }
}
async function tierError(prefix, resp) {
  let detail = "";
  try {
    detail = (await resp.text()).slice(0, 300);
  } catch {
  }
  const status = resp.status >= 400 && resp.status < 500 ? resp.status : 502;
  return new TierError(`${prefix}: blobsd ${resp.status}${detail ? " " + detail : ""}`, status);
}
function cleanMime(mime, fallbackPathMime) {
  return String(mime || "").split(";")[0].trim() || fallbackPathMime || "application/octet-stream";
}
async function uploadInlineBytes(cell, bytes, declaredSha, mime, pathMime) {
  const cfg = requireTier(cell);
  const sha = declaredSha || await sha256Hex(bytes);
  const resp = await fetch(`${cfg.url}/upload`, {
    method: "PUT",
    headers: bearer(cfg, {
      [SHA_HEADER]: sha,
      "content-type": cleanMime(mime, pathMime)
    }),
    body: bytes
  });
  if (!resp.ok) throw await tierError("inline upload", resp);
  const desc = await resp.json().catch(() => null);
  if (!desc || desc.sha256 !== sha) throw new TierError("inline upload: descriptor hash mismatch");
  return { sha256: sha, size: bytes.byteLength, mime: cleanMime(mime, pathMime) };
}
const GEN_PLACE_CAP = 64 * 1024 * 1024;
async function tierPlaceFromUrl(cell, url, mimeHint) {
  const cfg = requireTier(cell);
  const timed = (p) => Promise.race([p, new Promise((_, rej) => setTimeout(() => rej(new TierError("fetch output timed out after 120s", 504)), 12e4))]);
  const remote = await timed(fetch(url));
  if (!remote.ok) throw new TierError(`fetch output: ${remote.status} ${String(url).slice(0, 120)}`, 502);
  const mime = cleanMime(remote.headers.get("content-type"), mimeHint);
  const len = parseInt(remote.headers.get("content-length") || "0", 10);
  if (len > GEN_PLACE_CAP) throw new TierError(`generated output is ${(len / 1048576).toFixed(1)}MiB \u2014 over the ${GEN_PLACE_CAP / 1048576}MiB placement cap`, 413);
  const put = (body) => fetch(`${cfg.url}/upload`, {
    method: "PUT",
    headers: bearer(cfg, { "content-type": mime }),
    body,
    // @ts-ignore duplex required for streaming bodies
    duplex: "half"
  });
  let resp;
  if (len > 0) {
    const owned = typeof IdentityTransformStream === "function" ? remote.body.pipeThrough(new IdentityTransformStream()) : remote.body;
    try {
      resp = await timed(put(owned));
    } catch (e) {
      if (!/not owned/i.test(String(e && e.message || e))) throw e;
      const buf = new Uint8Array(await (await timed(fetch(url))).arrayBuffer());
      if (buf.byteLength > GEN_PLACE_CAP) throw new TierError(`generated output is ${(buf.byteLength / 1048576).toFixed(1)}MiB \u2014 over the ${GEN_PLACE_CAP / 1048576}MiB placement cap`, 413);
      resp = await put(buf);
    }
  } else {
    const bytes = new Uint8Array(await remote.arrayBuffer());
    if (bytes.byteLength > GEN_PLACE_CAP) throw new TierError(`generated output is ${(bytes.byteLength / 1048576).toFixed(1)}MiB \u2014 over the ${GEN_PLACE_CAP / 1048576}MiB placement cap`, 413);
    resp = await put(bytes);
  }
  if (!resp.ok) throw await tierError("place generated output", resp);
  const desc = await resp.json().catch(() => null);
  if (!desc || !SHA_RE.test(String(desc.sha256 || ""))) throw new TierError("place generated output: tier returned no content address");
  if (!Number.isSafeInteger(desc.size)) throw new TierError("place generated output: tier returned no size");
  return { sha256: desc.sha256, size: desc.size, mime };
}
async function streamThroughUpload(cell, requestBody, declaredSha, mime, pathMime) {
  const cfg = requireTier(cell);
  const resp = await fetch(`${cfg.url}/upload`, {
    method: "PUT",
    headers: bearer(cfg, {
      [SHA_HEADER]: declaredSha,
      "content-type": cleanMime(mime, pathMime)
    }),
    body: requestBody,
    // @ts-ignore duplex required for streaming bodies
    duplex: "half"
  });
  if (!resp.ok) {
    if (resp.status === 400) {
      throw new TierError(`x-fragment-hash mismatch: declared ${declaredSha}, tier computed a different hash (bad_hash)`, 400);
    }
    throw await tierError("streamed upload", resp);
  }
  const desc = await resp.json().catch(() => null);
  if (!desc || desc.sha256 !== declaredSha) {
    throw new TierError(`x-fragment-hash mismatch: declared ${declaredSha}, tier received ${desc?.sha256 || "unknown"}`, 400);
  }
  return { sha256: desc.sha256, size: Number(desc.size) || 0, mime: cleanMime(mime, pathMime) };
}
async function tierStreamByHash(cell, sha256) {
  const cfg = requireTier(cell);
  const resp = await fetch(`${cfg.url}/${encodeURIComponent(sha256)}`, {
    headers: bearer(cfg)
  });
  if (!resp.ok) throw await tierError(`read ${sha256.slice(0, 12)}\u2026`, resp);
  return resp;
}
async function tierTextBounded(cell, row, what) {
  if ((row.size | 0) > READ_CEILING) {
    throw new TierError(`${what} is ${(row.size / 1048576).toFixed(1)}MiB \u2014 over the ${READ_CEILING / 1048576}MiB decode ceiling; consume it streamed or via the blob tier directly`, 413);
  }
  const resp = await tierStreamByHash(cell, row.sha256);
  const len = parseInt(resp.headers.get("content-length") || "0", 10);
  if (len > READ_CEILING) throw new TierError(`${what} body exceeds decode ceiling at the tier`, 413);
  return new TextDecoder().decode(await resp.arrayBuffer());
}
function publicRedirectTarget(cell, blessedMode) {
  const cfg = tierConfig(cell.env);
  const vis = cell.manifest()?.visibility;
  if (!blessedMode || !cfg.publicGet || !cfg.publicUrl) return null;
  if (vis !== "public" && vis !== "link") return null;
  return cfg.publicUrl;
}
const OVERSIZE_HINT = 'oversize raw body: over the 65536-byte inline limit with no x-fragment-hash \u2014 use the blob-first flow (upload bytes straight to the blob tier, then PUT the {"ref":{"sha256","size","mime"}} form); the fragment CLI does this automatically';
const SHA_RE = /^[0-9a-f]{64}$/;
function validRef(body) {
  const ref = body && typeof body === "object" && body.ref && typeof body.ref === "object" ? body.ref : body;
  if (!ref || typeof ref !== "object" || !ref.sha256) return "missing ref";
  const sha = String(ref.sha256).toLowerCase();
  if (!SHA_RE.test(sha)) return "ref.sha256 must be 64 hex chars";
  const size = Number(ref.size);
  if (!Number.isSafeInteger(size) || size < 0) return "ref.size must be a non-negative integer";
  if (ref.mime !== void 0 && typeof ref.mime !== "string") return "ref.mime must be a string";
  return { ref };
}
async function admitFileWrite(cell, request, pathMimeGuess) {
  const ct = request.headers.get("content-type");
  const ctBase = (ct || "").split(";")[0].trim().toLowerCase();
  const hdrSha = String(request.headers.get("x-fragment-hash") || "").toLowerCase();
  if (ctBase === "application/json") {
    const body = await request.json().catch(() => null);
    const verdict = validRef(body);
    if (typeof verdict === "string") throw new TierError(`bad ref form: ${verdict}`, 400);
    const ref = verdict.ref;
    const sha2 = String(ref.sha256).toLowerCase();
    const size = Number(ref.size);
    const mime2 = cleanMime(ref.mime, pathMimeGuess);
    return { form: "ref", effSha: sha2, place: async () => ({ sha256: sha2, size, mime: mime2 }) };
  }
  if (hdrSha) {
    if (!SHA_RE.test(hdrSha)) throw new TierError("x-fragment-hash must be 64 hex chars", 400);
    return {
      form: "hashpush",
      effSha: hdrSha,
      place: () => streamThroughUpload(cell, request.body, hdrSha, ct, pathMimeGuess)
    };
  }
  const len = parseInt(request.headers.get("content-length") || "0", 10);
  if (len > INLINE_LIMIT) throw new TierError(OVERSIZE_HINT, 413);
  const bytes = await request.arrayBuffer();
  const mime = cleanMime(ct, pathMimeGuess);
  const sha = await sha256Hex(bytes);
  return {
    form: "inline",
    effSha: sha,
    place: () => uploadInlineBytes(cell, bytes, sha, mime, pathMimeGuess)
  };
}
export {
  GEN_PLACE_CAP,
  INLINE_LIMIT,
  OVERSIZE_HINT,
  READ_CEILING,
  TierError,
  admitFileWrite,
  publicRedirectTarget,
  streamThroughUpload,
  tierConfig,
  tierPlaceFromUrl,
  tierStreamByHash,
  tierTextBounded,
  uploadInlineBytes
};
