// Blob-tier client (docs/blob-tier.md): the ONLY module that moves file
// bytes between a cell and blobsd. Cells hold names; this holds the pipes.
//
// Env plumbing: the host passes CELLD_VAR_BLOBSD_URL, CELLD_VAR_BLOBSD_INTERNAL_TOKEN,
// CELLD_VAR_BLOBSD_PUBLIC_GET and CELLD_VAR_BLOBSD_PUBLIC_URL through the
// existing CELLD_VAR_ passthrough (host strips the prefix), so workers see
// them as env.BLOBSD_* — same path OPENROUTER_API_KEY rides in (internal.ts).
// Loaded author workers never receive these vars: every ctx.files.* call
// funnels through the loopback internal plane back into the cell, which owns
// all tier traffic with its own credentials.
import { sha256Hex } from "./auth.js";

// Bodies up to this size may arrive raw WITHOUT a hash header; the runtime
// buffers them (bounded — the whole buffer exists only inside this turn) and
// performs the tier upload itself.
export const INLINE_LIMIT = 65_536;
// Decode ceiling for whole-body reads funneled through the runtime
// (ctx.files.read and code/doc materializers). Bigger payloads must be
// consumed hashed/ranged/direct-from-blobsd — a >8MiB .text() is exactly the
// heap shape the two-tier split exists to kill.
export const READ_CEILING = 8 * 1024 * 1024;

const SHA_HEADER = "x-sha-256";

export function tierConfig(env) {
  return {
    url: String(env.BLOBSD_URL || "").replace(/\/$/, ""),
    token: String(env.BLOBSD_INTERNAL_TOKEN || ""),
    publicUrl: String(env.BLOBSD_PUBLIC_URL || "").replace(/\/$/, ""),
    publicGet: String(env.BLOBSD_PUBLIC_GET || "") === "1",
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
    ...extraHeaders,
  };
}

// Error carrying an HTTP status from tier operations so routes can answer
// upstream codes without string-matching.
export class TierError extends Error {
  status: number;
  constructor(message: string, status = 502) {
    super(message);
    this.status = status;
  }
}

async function tierError(prefix, resp) {
  let detail = "";
  try { detail = (await resp.text()).slice(0, 300); } catch {}
  // upstream 4xx are caller-facing verdicts (bad_hash, missing object);
  // upstream 5xx/1xx-3xx anomalies collapse to a plain gateway 502
  const status = resp.status >= 400 && resp.status < 500 ? resp.status : 502;
  return new TierError(`${prefix}: blobsd ${resp.status}${detail ? " " + detail : ""}`, status);
}

function cleanMime(mime, fallbackPathMime) {
  return String(mime || "").split(";")[0].trim() || fallbackPathMime || "application/octet-stream";
}

// Inline carve-out (spec): <=64KiB bodies ride inside the write turn. The
// bytes are already fully materialized by the caller (bound asserted there);
// this just computes the address and streams the buffer out to blobsd.
export async function uploadInlineBytes(cell, bytes, declaredSha, mime, pathMime) {
  const cfg = requireTier(cell);
  const sha = declaredSha || await sha256Hex(bytes);
  const resp = await fetch(`${cfg.url}/upload`, {
    method: "PUT",
    headers: bearer(cfg, {
      [SHA_HEADER]: sha,
      "content-type": cleanMime(mime, pathMime),
    }),
    body: bytes,
  });
  if (!resp.ok) throw await tierError("inline upload", resp);
  // paired assertion: the tier's descriptor must agree on content identity —
  // a row committed against a disagreeing hash would name bytes that don't exist
  const desc = await resp.json().catch(() => null);
  if (!desc || desc.sha256 !== sha) throw new TierError("inline upload: descriptor hash mismatch");
  return { sha256: sha, size: bytes.byteLength, mime: cleanMime(mime, pathMime) };
}

// Hash-pushed form (x-fragment-hash present): the body streams THROUGH the
// cell into the tier — chunked reader -> chunked writer, no point where the
// whole payload exists in JS memory. Identity is settled by comparing the
// tier's authoritative descriptor hash with the caller's header afterwards;
// misdelivered bytes stay inert because blobsd stores strictly under the
// true content hash.
export async function streamThroughUpload(cell, requestBody, declaredSha, mime, pathMime) {
  const cfg = requireTier(cell);
  const resp = await fetch(`${cfg.url}/upload`, {
    method: "PUT",
    headers: bearer(cfg, {
      [SHA_HEADER]: declaredSha,
      "content-type": cleanMime(mime, pathMime),
    }),
    body: requestBody,
    // @ts-ignore duplex required for streaming bodies
    duplex: "half",
  });
  if (!resp.ok) {
    // a 400 from the tier on a declared-hash push IS the identity verdict —
    // surface it with the caller's declared hash so the error is actionable
    if (resp.status === 400) {
      throw new TierError(`x-fragment-hash mismatch: declared ${declaredSha}, tier computed a different hash (bad_hash)`, 400);
    }
    throw await tierError("streamed upload", resp);
  }
  const desc = await resp.json().catch(() => null);
  if (!desc || desc.sha256 !== declaredSha) {
    // both addresses in the message: callers verify the DECLARED hash, ops
    // debug what the tier actually got
    throw new TierError(`x-fragment-hash mismatch: declared ${declaredSha}, tier received ${desc?.sha256 || "unknown"}`, 400);
  }
  return { sha256: desc.sha256, size: Number(desc.size) || 0, mime: cleanMime(mime, pathMime) };
}

// Read side: open the tier object as a stream. The Response body flows out
// untouched (heap-flat proxy); callers must not arrayBuffer() it.
export async function tierStreamByHash(cell, sha256) {
  const cfg = requireTier(cell);
  const resp = await fetch(`${cfg.url}/${encodeURIComponent(sha256)}`, {
    headers: bearer(cfg),
  });
  if (!resp.ok) throw await tierError(`read ${sha256.slice(0, 12)}…`, resp);
  return resp;
}

// Materializer read: bounded whole-body fetch + text decode for things that
// MUST be strings in memory (workflow/app/rooms source, OG-injected HTML).
// The ceiling is checked against stored metadata BEFORE any fetch so a
// mislabeled huge asset can't sneak a decode past it.
export async function tierTextBounded(cell, row, what) {
  if ((row.size | 0) > READ_CEILING) {
    throw new TierError(`${what} is ${(row.size / 1048576).toFixed(1)}MiB — over the ${READ_CEILING / 1048576}MiB decode ceiling; consume it streamed or via the blob tier directly`, 413);
  }
  const resp = await tierStreamByHash(cell, row.sha256);
  const len = parseInt(resp.headers.get("content-length") || "0", 10);
  // paired assertion: what arrived should match what was named; grow-by-cheating fails loudly here too
  if (len > READ_CEILING) throw new TierError(`${what} body exceeds decode ceiling at the tier`, 413);
  return new TextDecoder().decode(await resp.arrayBuffer());
}

// 302 mode (spec): instance-level public GET enabled AND the fragment's
// manifest visibility is public-or-link → browsers get redirected straight
// to blobsd and V8 exits the path entirely. Only the blessed/canonical mode
// redirects (its visibility gate actually ran); draft previews always proxy.
export function publicRedirectTarget(cell, blessedMode) {
  const cfg = tierConfig(cell.env);
  const vis = cell.manifest()?.visibility;
  if (!blessedMode || !cfg.publicGet || !cfg.publicUrl) return null;
  if (vis !== "public" && vis !== "link") return null;
  return cfg.publicUrl;
}

// Exact hint text required by the spec for oversize raw bodies (>inline,
// no hash header): point at the blob-first flow. The CLI implements this
// automatically; hand-callers get told what to do instead of why they failed.
export const OVERSIZE_HINT =
  'oversize raw body: over the 65536-byte inline limit with no x-fragment-hash — use the blob-first flow (upload bytes straight to the blob tier, then PUT the {"ref":{"sha256","size","mime"}} form); the fragment CLI does this automatically';

const SHA_RE = /^[0-9a-f]{64}$/;

function validRef(body) {
  // spec wire form: {"ref":{"sha256","size","mime"}}; a flat {...} is tolerated
  // for hand-callers who skip the envelope
  const ref = body && typeof body === "object" && body.ref && typeof body.ref === "object" ? body.ref : body;
  if (!ref || typeof ref !== "object" || !ref.sha256) return "missing ref";
  const sha = String(ref.sha256).toLowerCase();
  if (!SHA_RE.test(sha)) return "ref.sha256 must be 64 hex chars";
  const size = Number(ref.size);
  if (!Number.isSafeInteger(size) || size < 0) return "ref.size must be a non-negative integer";
  if (ref.mime !== undefined && typeof ref.mime !== "string") return "ref.mime must be a string";
  return { ref };
}

// ------ the two wire forms of the commit contract ------
//
// Admission classifies the request, exposes the CONTENT ADDRESS up front
// (`effSha`), and defers byte transport into `.place()` so callers can run
// their cheap ROW gates first (dedup, append-only, base_rev conflict) before
// a single byte moves. Both funnel into the caller's single row-commit turn.

// Admits a file-write request. Throws TierError(413, OVERSIZE_HINT) for
// oversize raw bodies and 400 for malformed hashes/refs. Only the inline
// carve-out ever materializes a whole body, and only up to INLINE_LIMIT.
export async function admitFileWrite(cell, request, pathMimeGuess) {
  const ct = request.headers.get("content-type");
  const ctBase = (ct || "").split(";")[0].trim().toLowerCase();
  const hdrSha = String(request.headers.get("x-fragment-hash") || "").toLowerCase();

  if (ctBase === "application/json") {
    const body = await request.json().catch(() => null);
    const verdict = validRef(body);
    if (typeof verdict === "string") throw new TierError(`bad ref form: ${verdict}`, 400);
    const ref = verdict.ref;
    const sha = String(ref.sha256).toLowerCase();
    const size = Number(ref.size);
    const mime = cleanMime(ref.mime, pathMimeGuess);
    // bytes are presumed already in the tier (commit contract) — O(1),
    // zero byte transport; misdeclared refs surface later as missing-object reads
    return { form: "ref", effSha: sha, place: async () => ({ sha256: sha, size, mime }) };
  }

  if (hdrSha) {
    if (!SHA_RE.test(hdrSha)) throw new TierError("x-fragment-hash must be 64 hex chars", 400);
    // verify-vs-computed: verification is delegated to the tier (which stores
    // strictly under its own computed hash) and checked against this header.
    // The body streams THROUGH into .place() — never whole in JS memory.
    return {
      form: "hashpush",
      effSha: hdrSha,
      place: () => streamThroughUpload(cell, request.body, hdrSha, ct, pathMimeGuess),
    };
  }

  // inline carve-out
  const len = parseInt(request.headers.get("content-length") || "0", 10);
  if (len > INLINE_LIMIT) throw new TierError(OVERSIZE_HINT, 413);
  const bytes = await request.arrayBuffer(); // bounded above
  const mime = cleanMime(ct, pathMimeGuess);
  // hashing here keeps every admitted write's address known before row gates
  const sha = await sha256Hex(bytes);
  return {
    form: "inline",
    effSha: sha,
    place: () => uploadInlineBytes(cell, bytes, sha, mime, pathMimeGuess),
  };
}
