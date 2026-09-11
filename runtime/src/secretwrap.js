// GENERATED from runtime/ts - run scripts/build-runtime after editing sources.
const INFO = new TextEncoder().encode("fragment/wrapped-secrets v1");
class SecretWrapError extends Error {
  constructor(message) {
    super(message);
    this.name = "SecretWrapError";
  }
}
function b64url(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return btoa(bin);
}
function fromB64url(s) {
  const bin = atob(String(s || "").replace(/-/g, "+").replace(/_/g, "/"));
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
async function hkdfSha256(ikm, salt, info, length) {
  const bs = (u) => u;
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
async function deriveKey(hostSecret, npub) {
  const ikm = new TextEncoder().encode(String(hostSecret || ""));
  if (!ikm.length) throw new SecretWrapError("FRAGMENT_HOST_SECRET is not set on this host \u2014 refusing to store secrets unwrapped (set CELLD_VAR_FRAGMENT_HOST_SECRET)");
  const salt = new TextEncoder().encode(String(npub || ""));
  const bits = await hkdfSha256(ikm, salt, INFO, 32);
  return await crypto.subtle.importKey("raw", bits, { name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
}
async function wrapSecret(hostSecret, npub, plaintext) {
  const key = await deriveKey(hostSecret, npub);
  const nonce = new Uint8Array(12);
  crypto.getRandomValues(nonce);
  const ct = new Uint8Array(await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: nonce },
    key,
    new TextEncoder().encode(plaintext)
  ));
  const blob = new Uint8Array(nonce.length + ct.length);
  blob.set(nonce, 0);
  blob.set(ct, nonce.length);
  return b64url(blob);
}
async function unwrapSecret(hostSecret, npub, wrapped) {
  const key = await deriveKey(hostSecret, npub);
  const blob = fromB64url(wrapped);
  if (blob.length < 12 + 16) throw new SecretWrapError("wrapped secret blob too short \u2014 corrupt or foreign format");
  const nonce = blob.subarray(0, 12);
  const ct = blob.subarray(12);
  try {
    const pt = await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce }, key, ct);
    return new TextDecoder().decode(pt);
  } catch {
    throw new SecretWrapError("secret unwrap failed (wrong FRAGMENT_HOST_SECRET or corrupt blob)");
  }
}
export {
  SecretWrapError,
  unwrapSecret,
  wrapSecret
};
