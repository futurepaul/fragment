// NIP-98 HTTP auth verification (kind 27235).
import { schnorr } from "@noble/curves/secp256k1.js";

const textEncoder = new TextEncoder();

export async function sha256Hex(data) {
  const buf = typeof data === "string" ? textEncoder.encode(data) : data;
  const hash = await crypto.subtle.digest("SHA-256", buf);
  return [...new Uint8Array(hash)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function b64decode(s) {
  const bin = atob(s);
  const bytes = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
  return bytes;
}

// Constant-time-ish secret comparison: XOR-fold over zero-padded UTF-8
// bytes, length mismatch folded into the accumulator. Not cryptographic
// perfection (JIT), but removes the early-exit timing signal that `===`
// gives away for free.
export function safeEqual(presented, secret) {
  const enc = new TextEncoder();
  const a = enc.encode(String(presented ?? ""));
  const b = enc.encode(String(secret ?? ""));
  const len = Math.max(a.length, b.length);
  const da = new Uint8Array(len); da.set(a);
  const db = new Uint8Array(len); db.set(b);
  let diff = a.length ^ b.length;
  for (let i = 0; i < len; i++) diff |= da[i] ^ db[i];
  return diff === 0;
}

// Returns { ok, pubkey?, error? }. `request` must still have a readable body
// clone; pass the raw body bytes in explicitly.
export async function verifyNip98(request, bodyBytes) {
  const header = request.headers.get("authorization") || "";
  if (!header.startsWith("Nostr ")) return { ok: false, error: "missing Nostr auth header" };
  let event;
  try {
    event = JSON.parse(new TextDecoder().decode(b64decode(header.slice(6).trim())));
  } catch {
    return { ok: false, error: "malformed auth event" };
  }
  const { id, pubkey, created_at, kind, tags, content, sig } = event || {};
  if (kind !== 27235) return { ok: false, error: "wrong event kind" };
  if (typeof pubkey !== "string" || !/^[0-9a-f]{64}$/.test(pubkey)) return { ok: false, error: "bad pubkey" };
  if (typeof created_at !== "number" || Math.abs(Date.now() / 1000 - created_at) > 60)
    return { ok: false, error: "created_at outside 60s window" };
  const tag = (n) => (tags || []).find((t) => t[0] === n)?.[1];
  const u = tag("u");
  const method = tag("method");
  const canon = (s) => { const x = new URL(s); return x.origin + x.pathname + x.search; };
  let expect, got;
  try {
    expect = canon(request.url);
    got = canon(u || "");
  } catch {
    return { ok: false, error: "bad u tag" };
  }
  if (got !== expect) return { ok: false, error: `u tag mismatch: ${u}` };
  if (method !== request.method.toUpperCase()) return { ok: false, error: "method tag mismatch" };
  if (bodyBytes && bodyBytes.byteLength > 0) {
    const payload = tag("payload");
    if (!payload || payload !== (await sha256Hex(bodyBytes))) return { ok: false, error: "payload hash mismatch" };
  }
  const serialized = JSON.stringify([0, pubkey, created_at, kind, tags, content ?? ""]);
  const computed = await sha256Hex(textEncoder.encode(serialized));
  if (computed !== id) return { ok: false, error: "event id mismatch" };
  let valid = false;
  try {
    const hb = (h) => Uint8Array.from(h.match(/.{2}/g).map((b) => parseInt(b, 16)));
    valid = schnorr.verify(hb(sig), hb(id), hb(pubkey));
  } catch {
    valid = false;
  }
  if (!valid) return { ok: false, error: "bad signature" };
  return { ok: true, pubkey };
}
