// Level-c secrets wrapping (docs/encryption-research.md stage 1, ROADMAP
// decision 3): everything secret-shaped that a cell persists is wrapped
// under a key derived from FRAGMENT_HOST_SECRET before it touches SQLite.
//
//   key = HKDF-SHA256(ikm = FRAGMENT_HOST_SECRET,
//                     salt = fragment npub (utf8),
//                     info = "fragment/wrapped-secrets v1", L = 32)
//   blob = b64url( 12-byte random nonce || AES-256-GCM(key, nonce, plaintext) )
//
// Domain separation is deliberate: the host secret never double-bills as
// the fragment signing key, and each fragment's wrapped blobs key
// differently (salt = its npub), so a leaked cell database plus a second
// fragment's wrap key does not open this one. If FRAGMENT_HOST_SECRET is
// unset the first wrap/unwrap FAILS LOUDLY — silent plaintext-at-rest is
// exactly the level-(c) bug this module exists to close.
//
// Import of @noble/hashes is avoided on purpose: HKDF is a few lines over
// WebCrypto and the runtime already trusts crypto.subtle everywhere else.
// (Spelled as RFC 5869 over HMAC-SHA256 rather than subtle's own "HKDF":
// workerd — celld — rejects importKey(..., "HKDF", ..., ["deriveKey"])
// with NotSupportedError, found booting the dev stack; HMAC + SHA-256 are
// supported everywhere fragment runs, and the output is identical.)
const INFO = new TextEncoder().encode("fragment/wrapped-secrets v1");

export class SecretWrapError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SecretWrapError";
  }
}

function b64url(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}

function fromB64url(s: string): Uint8Array {
  const bin = atob(String(s || "").replace(/-/g, "+").replace(/_/g, "/"));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// RFC 5869 HKDF-SHA256: extract with the salt as the HMAC key, expand
// blocks of T(i) = HMAC(PRK, T(i-1) || info || i), bounded by 255 blocks.
async function hkdfSha256(ikm: Uint8Array, salt: Uint8Array, info: Uint8Array, length: number): Promise<Uint8Array> {
  const bs = (u: Uint8Array) => u as unknown as BufferSource;
  const extractKey = await crypto.subtle.importKey("raw", bs(salt), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const prk = new Uint8Array(await crypto.subtle.sign("HMAC", extractKey, bs(ikm)));
  const prkKey = await crypto.subtle.importKey("raw", bs(prk), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const out = new Uint8Array(length);
  let prev = new Uint8Array(0);
  let pos = 0;
  for (let i = 1; pos < length && i <= 255; i++) {
    const block = new Uint8Array(prev.length + info.length + 1);
    block.set(prev, 0);
    block.set(info, prev.length);
    block[prev.length + info.length] = i;
    prev = new Uint8Array(await crypto.subtle.sign("HMAC", prkKey, bs(block)));
    const take = Math.min(prev.length, length - pos);
    out.set(prev.subarray(0, take), pos);
    pos += take;
  }
  return out;
}

async function deriveKey(hostSecret: string, npub: string): Promise<CryptoKey> {
  const ikm = new TextEncoder().encode(String(hostSecret || ""));
  if (!ikm.length) throw new SecretWrapError("FRAGMENT_HOST_SECRET is not set on this host — refusing to store secrets unwrapped (set CELLD_VAR_FRAGMENT_HOST_SECRET)");
  const salt = new TextEncoder().encode(String(npub || ""));
  const bits = await hkdfSha256(ikm, salt, INFO, 32);
  return await crypto.subtle.importKey("raw", bits as unknown as BufferSource, { name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}

export async function wrapSecret(hostSecret: string, npub: string, plaintext: string): Promise<string> {
  const key = await deriveKey(hostSecret, npub);
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const ct = new Uint8Array(await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: nonce as unknown as BufferSource }, key, new TextEncoder().encode(plaintext) as unknown as BufferSource,
  ));
  const blob = new Uint8Array(nonce.length + ct.length);
  blob.set(nonce, 0);
  blob.set(ct, nonce.length);
  return b64url(blob);
}

export async function unwrapSecret(hostSecret: string, npub: string, wrapped: string): Promise<string> {
  const key = await deriveKey(hostSecret, npub);
  const blob = fromB64url(wrapped);
  if (blob.length < 12 + 16) throw new SecretWrapError("wrapped secret blob too short — corrupt or foreign format");
  const nonce = blob.subarray(0, 12);
  const ct = blob.subarray(12);
  try {
    const pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce as unknown as BufferSource }, key, ct as unknown as BufferSource);
    return new TextDecoder().decode(pt);
  } catch {
    throw new SecretWrapError("secret unwrap failed (wrong FRAGMENT_HOST_SECRET or corrupt blob)");
  }
}
