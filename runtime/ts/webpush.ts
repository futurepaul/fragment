// Web Push, dependency-free, over the platform's WebCrypto.
//
//   * RFC 8291 — payload encryption for the `aes128gcm` scheme:
//     ECDH P-256 → HKDF-SHA256 → AES-128-GCM, everything the receiver
//     needs (salt, record size, sender public key) inside the body.
//   * RFC 8292 — VAPID authorization: an ES256 JWT (P-256 + SHA-256,
//     DER/DSS signature) proving the sender's identity to the push
//     service via a keypair the cell owns.
//
// No bundler-time dependencies on purpose: this must behave identically
// in the cell host (workerd) and in a plain node script (the self-test
// proof), so it only touches standard WebCrypto + atob/btoa.

const te = new TextEncoder();
const td = new TextDecoder();

// ---- base64url ----

export function b64urlEncode(bytes: Uint8Array): string {
  let bin = "";
  // chunked: String.fromCharCode(...bytes) blows the arg budget on
  // multi-KB ciphertexts in some engines
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

// Tolerant on the way in: browsers hand the p256dh/auth keys over as
// base64url, but hand-rolled pages (and padded variants) show up too.
export function b64urlDecode(s: string): Uint8Array {
  const std = String(s || "").replace(/-/g, "+").replace(/_/g, "/").trim();
  const pad = std.length % 4 === 0 ? "" : "=".repeat(4 - (std.length % 4));
  const bin = atob(std + pad);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function cat(...parts: Uint8Array[]): Uint8Array {
  const n = parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) { out.set(p, o); o += p.length; }
  return out;
}

const utf8 = (s: string) => te.encode(s);

// WebCrypto HKDF is extract+expand in one shot — exactly the HKDF(salt,
// ikm, info, L) composition RFC 8291 writes out.
async function hkdf(salt: Uint8Array, ikm: Uint8Array, info: Uint8Array, bytes: number): Promise<Uint8Array> {
  const key = await crypto.subtle.importKey("raw", ikm as unknown as BufferSource, "HKDF", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits(
    { name: "HKDF", hash: "SHA-256", salt: salt as unknown as BufferSource, info: info as unknown as BufferSource },
    key, bytes * 8,
  );
  return new Uint8Array(bits);
}

const ECDH = { name: "ECDH", namedCurve: "P-256" } as const;
const ECDSA = { name: "ECDSA", namedCurve: "P-256" } as const;

// ---- RFC 8291: aes128gcm payload encryption ----

export interface PushSub {
  endpoint?: string;
  p256dh: string; // base64url, 65-byte uncompressed P-256 point (the UA public key)
  auth: string;   // base64url, 16-byte auth secret
}

// Record size for the single record we emit. 4096 is the scheme default;
// our payloads are capped far below it, so one record always suffices.
const RS = 4096;

// Encrypt `payload` for one subscription. Returns the exact POST body
// (salt ‖ rs ‖ keyid ‖ ciphertext) and the content headers that describe
// it — the caller merges TTL/Urgency/VAPID/auth around them.
export async function encryptPayload(sub: PushSub, payload: string): Promise<{ body: Uint8Array; headers: Record<string, string> }> {
  const uaPub = b64urlDecode(sub.p256dh);
  const authSecret = b64urlDecode(sub.auth);
  if (uaPub.length !== 65) throw new Error(`p256dh must be a 65-byte uncompressed point, got ${uaPub.length}`);
  if (authSecret.length !== 16) throw new Error(`auth secret must be 16 bytes, got ${authSecret.length}`);
  const plaintext = utf8(payload);
  // 16 (tag) + 1 (padding delimiter) of AEAD overhead per record
  if (plaintext.length + 17 > RS) throw new Error(`payload too large for one aes128gcm record (${plaintext.length} + 17 > ${RS})`);

  const asKeys = await crypto.subtle.generateKey(ECDH, true, ["deriveBits"]) as CryptoKeyPair;
  const asPub = new Uint8Array(await crypto.subtle.exportKey("raw", asKeys.publicKey));
  const uaPubKey = await crypto.subtle.importKey("raw", uaPub as unknown as BufferSource, ECDH, false, []);

  // ikm = shared secret; PRK = HKDF(auth_secret, ikm, "WebPush: info" ‖ 0x00 ‖ ua_pub ‖ as_pub)
  const ikm = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: uaPubKey }, asKeys.privateKey, 256));
  const prk = await hkdf(authSecret, ikm, cat(utf8("WebPush: info\0"), uaPub, asPub), 32);

  const salt = crypto.getRandomValues(new Uint8Array(16));
  const cekRaw = await hkdf(salt, prk, utf8("Content-Encoding: aes128gcm\0"), 16);
  const nonce = await hkdf(salt, prk, utf8("Content-Encoding: nonce\0"), 12);
  const cek = await crypto.subtle.importKey("raw", cekRaw as unknown as BufferSource, "AES-GCM", false, ["encrypt"]);
  // plaintext ends with the RFC 8188 padding delimiter; 0x01 = no padding
  const ciphertext = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce as unknown as BufferSource }, cek, cat(plaintext, new Uint8Array([1])) as unknown as BufferSource));

  const rs = new Uint8Array([(RS >>> 24) & 0xff, (RS >>> 16) & 0xff, (RS >>> 8) & 0xff, RS & 0xff]);
  const body = cat(salt, rs, new Uint8Array([asPub.length]), asPub, ciphertext);
  return { body, headers: { "Content-Encoding": "aes128gcm" } };
}

// The UA side, implemented for the self-test: proves a receiver holding
// only (ua_private, auth_secret) recovers the payload from our body.
async function uaDecrypt(body: Uint8Array, uaPriv: CryptoKey, uaPub: Uint8Array, authSecret: Uint8Array): Promise<string> {
  const salt = body.subarray(0, 16);
  const rs = (body[16] << 24) | (body[17] << 16) | (body[18] << 8) | body[19];
  const idLen = body[20];
  const asPub = body.subarray(21, 21 + idLen);
  const ciphertext = body.subarray(21 + idLen);
  const asPubKey = await crypto.subtle.importKey("raw", asPub as unknown as BufferSource, ECDH, false, []);
  const ikm = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: asPubKey }, uaPriv, 256));
  const prk = await hkdf(authSecret, ikm, cat(utf8("WebPush: info\0"), uaPub, asPub), 32);
  const cekRaw = await hkdf(salt, prk, utf8("Content-Encoding: aes128gcm\0"), 16);
  const nonce = await hkdf(salt, prk, utf8("Content-Encoding: nonce\0"), 12);
  const cek = await crypto.subtle.importKey("raw", cekRaw as unknown as BufferSource, "AES-GCM", false, ["decrypt"]);
  const padded = new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce as unknown as BufferSource }, cek, ciphertext as unknown as BufferSource));
  let end = padded.length;
  while (end > 0 && padded[end - 1] === 0) end--; // strip zero padding
  const delim = end > 0 ? padded[end - 1] : 0;
  if (delim !== 1 && delim !== 2) throw new Error(`bad padding delimiter 0x${delim.toString(16)}`);
  return td.decode(padded.subarray(0, end - 1));
}

// ---- RFC 8292: VAPID (ES256 JWT) ----

export interface VapidKeys {
  privJwk: JsonWebKey; // storable/rotatable — the cell keeps it in meta
  pubRaw: string;      // base64url of the 65-byte uncompressed point
}

export async function generateVapidKeys(): Promise<VapidKeys> {
  const kp = await crypto.subtle.generateKey(ECDSA, true, ["sign", "verify"]) as CryptoKeyPair;
  const privJwk = await crypto.subtle.exportKey("jwk", kp.privateKey);
  const pubRaw = b64urlEncode(new Uint8Array(await crypto.subtle.exportKey("raw", kp.publicKey)));
  return { privJwk, pubRaw };
}

// DER INTEGER for an ES256 signature half (strip leading zeros, prepend a
// 0x00 when the high bit is set — JWS DSS format).
function derInt(half: Uint8Array): Uint8Array {
  let i = 0;
  while (i < half.length - 1 && half[i] === 0) i++;
  const v = half.subarray(i);
  const pad = (v[0] & 0x80) !== 0 ? 1 : 0;
  const out = new Uint8Array(2 + v.length + pad);
  out[0] = 0x02;
  out[1] = v.length + pad;
  out.set(v, 2 + pad);
  return out;
}

// WebCrypto signs ECDSA in raw r‖s (64 bytes); JWS wants the DER SEQUENCE.
function rawSigToDer(sig: Uint8Array): Uint8Array {
  const r = derInt(sig.subarray(0, 32));
  const s = derInt(sig.subarray(32, 64));
  return cat(new Uint8Array([0x30, r.length + s.length]), r, s);
}

// And back — WebCrypto's ECDSA verify takes the RAW form (the WebCrypto
// spec's signature format; DER is only the JWS wire format), so the
// self-test converts before verifying. Doubles as a DER structural check.
function derSigToRaw(der: Uint8Array): Uint8Array {
  if (der[0] !== 0x30) throw new Error("not a DER SEQUENCE");
  let o = 2; // SEQUENCE length < 128 always (content is 66-68 bytes)
  const read = (): Uint8Array => {
    if (der[o] !== 0x02) throw new Error("expected DER INTEGER");
    const len = der[o + 1];
    o += 2;
    let b = der.subarray(o, o + len);
    o += len;
    if (b[0] === 0) b = b.subarray(1);
    const out = new Uint8Array(32);
    out.set(b, 32 - b.length);
    return out;
  };
  const raw = cat(read(), read());
  if (o !== der.length) throw new Error("trailing bytes after DER signature");
  return raw;
}

// Headers proving sender identity to the push service. `audience` is the
// ORIGIN of the subscription endpoint; exp gives the JWT a 12h life.
//
// NOTE: RFC 8292 §3 spells the scheme `Authorization: vapid t=<jwt>,
// k=<pub>` (the `WebPush` scheme is the retired draft-01 form, and the
// `vapid=` Crypto-Key label was never standardized — `p256ecdsa` was).
// We send the RFC form with the public key in BOTH blessed locations
// (`k=` in the auth params, `p256ecdsa=` in Crypto-Key) so both the
// current services and older autopush deployments can read it.
export async function vapidHeaders(privJwk: JsonWebKey, pubRaw: string, audience: string): Promise<{ authorization: string; "crypto-key": string }> {
  const header = b64urlEncode(utf8(JSON.stringify({ typ: "JWT", alg: "ES256" })));
  const claims = b64urlEncode(utf8(JSON.stringify({
    aud: audience,
    exp: Math.floor(Date.now() / 1000) + 12 * 3600,
    sub: "mailto:dev@fragment.club",
  })));
  const signingInput = header + "." + claims;
  const priv = await crypto.subtle.importKey("jwk", privJwk, ECDSA, false, ["sign"]);
  const rawSig = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, priv, utf8(signingInput)));
  const jwt = signingInput + "." + b64urlEncode(rawSigToDer(rawSig));
  return {
    authorization: `vapid t=${jwt}, k=${pubRaw}`,
    "crypto-key": `p256ecdsa=${pubRaw}`,
  };
}

// ---- self-test ----

// Proves the implementation against itself with independent keys: a
// round-trip through the REAL receiver math (a UA keypair the encryptor
// never sees, decrypt via ua_private + the keyid embedded in the body)
// plus a VAPID JWT that verifies against the signer's public key and
// decodes to the exact header/claims we wrote. Pure — no cell, no I/O.
export async function webpushSelfTest(): Promise<{ ok: boolean; detail: string }> {
  try {
    // 1) RFC 8291 round-trip
    const ua = await crypto.subtle.generateKey(ECDH, true, ["deriveBits"]) as CryptoKeyPair;
    const uaPub = new Uint8Array(await crypto.subtle.exportKey("raw", ua.publicKey));
    const authSecret = crypto.getRandomValues(new Uint8Array(16));
    const msg = JSON.stringify({ title: "self-test ✔", body: "round-trip", url: "/?id=7", tag: "push-selftest" });
    const enc = await encryptPayload({ p256dh: b64urlEncode(uaPub), auth: b64urlEncode(authSecret) }, msg);
    const rs = (enc.body[16] << 24) | (enc.body[17] << 16) | (enc.body[18] << 8) | enc.body[19];
    const back = await uaDecrypt(enc.body, ua.privateKey, uaPub, authSecret);
    if (back !== msg) throw new Error("round-trip mismatch: " + JSON.stringify(back));
    if (enc.headers["Content-Encoding"] !== "aes128gcm") throw new Error("missing Content-Encoding header");
    if (rs !== RS) throw new Error(`bad rs ${rs}`);
    if (enc.body[20] !== 65) throw new Error(`bad keyid length ${enc.body[20]}`);

    // 2) RFC 8292 sign → verify
    const kp = await generateVapidKeys();
    const vh = await vapidHeaders(kp.privJwk, kp.pubRaw, "https://push.example.net");
    const m = vh.authorization.match(/^vapid t=([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]+), k=([A-Za-z0-9_-]+)$/);
    if (!m) throw new Error("authorization header not vapid t=…, k=…: " + vh.authorization);
    if (m[4] !== kp.pubRaw) throw new Error("k= does not match pubRaw");
    const h = JSON.parse(td.decode(b64urlDecode(m[1])));
    const c = JSON.parse(td.decode(b64urlDecode(m[2])));
    if (h.typ !== "JWT" || h.alg !== "ES256") throw new Error("bad JWT header " + JSON.stringify(h));
    if (c.aud !== "https://push.example.net") throw new Error("bad aud " + c.aud);
    if (c.sub !== "mailto:dev@fragment.club") throw new Error("bad sub " + c.sub);
    const nowSec = Math.floor(Date.now() / 1000);
    if (typeof c.exp !== "number" || c.exp <= nowSec || c.exp > nowSec + 12 * 3600 + 60) throw new Error("bad exp " + c.exp);
    // rebuild the public key from the raw point and verify — the JWT
    // carries the DER (JWS) form, WebCrypto verifies the raw form, so
    // derSigToRaw doubles as the structural check of our own encoding
    const pub = await crypto.subtle.importKey("raw", b64urlDecode(kp.pubRaw) as unknown as BufferSource, ECDSA, false, ["verify"]);
    const sig = b64urlDecode(m[3]);
    const rawSig = derSigToRaw(sig);
    const okSig = await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, pub, rawSig as unknown as BufferSource, utf8(m[1] + "." + m[2]));
    if (!okSig) throw new Error("VAPID signature does not verify");
    if (!/^p256ecdsa=/.test(vh["crypto-key"])) throw new Error("bad Crypto-Key header");

    return {
      ok: true,
      detail: `rfc8291 round-trip ok (${msg.length}B payload, rs=${rs}, keyid=${enc.body[20]}B, body=${enc.body.length}B); rfc8292 ES256 jwt verifies (aud=${c.aud}, exp=+${c.exp - nowSec}s)`,
    };
  } catch (e) {
    return { ok: false, detail: String((e && e.message) || e) };
  }
}
