// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
const te = new TextEncoder();
const td = new TextDecoder();
function b64urlEncode(bytes) {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 32768) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 32768));
  }
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
function b64urlDecode(s) {
  const std = String(s || "").replace(/-/g, "+").replace(/_/g, "/").trim();
  const pad = std.length % 4 === 0 ? "" : "=".repeat(4 - std.length % 4);
  const bin = atob(std + pad);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
function cat(...parts) {
  const n = parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}
const utf8 = (s) => te.encode(s);
async function hkdf(salt, ikm, info, bytes) {
  const key = await crypto.subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits(
    { name: "HKDF", hash: "SHA-256", salt, info },
    key,
    bytes * 8
  );
  return new Uint8Array(bits);
}
const ECDH = { name: "ECDH", namedCurve: "P-256" };
const ECDSA = { name: "ECDSA", namedCurve: "P-256" };
const RS = 4096;
async function encryptPayload(sub, payload) {
  const uaPub = b64urlDecode(sub.p256dh);
  const authSecret = b64urlDecode(sub.auth);
  if (uaPub.length !== 65) throw new Error(`p256dh must be a 65-byte uncompressed point, got ${uaPub.length}`);
  if (authSecret.length !== 16) throw new Error(`auth secret must be 16 bytes, got ${authSecret.length}`);
  const plaintext = utf8(payload);
  if (plaintext.length + 17 > RS) throw new Error(`payload too large for one aes128gcm record (${plaintext.length} + 17 > ${RS})`);
  const asKeys = await crypto.subtle.generateKey(ECDH, true, ["deriveBits"]);
  const asPub = new Uint8Array(await crypto.subtle.exportKey("raw", asKeys.publicKey));
  const uaPubKey = await crypto.subtle.importKey("raw", uaPub, ECDH, false, []);
  const ikm = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: uaPubKey }, asKeys.privateKey, 256));
  const prk = await hkdf(authSecret, ikm, cat(utf8("WebPush: info\0"), uaPub, asPub), 32);
  const salt = crypto.getRandomValues(new Uint8Array(16));
  const cekRaw = await hkdf(salt, prk, utf8("Content-Encoding: aes128gcm\0"), 16);
  const nonce = await hkdf(salt, prk, utf8("Content-Encoding: nonce\0"), 12);
  const cek = await crypto.subtle.importKey("raw", cekRaw, "AES-GCM", false, ["encrypt"]);
  const ciphertext = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv: nonce }, cek, cat(plaintext, new Uint8Array([1]))));
  const rs = new Uint8Array([RS >>> 24 & 255, RS >>> 16 & 255, RS >>> 8 & 255, RS & 255]);
  const body = cat(salt, rs, new Uint8Array([asPub.length]), asPub, ciphertext);
  return { body, headers: { "Content-Encoding": "aes128gcm" } };
}
async function uaDecrypt(body, uaPriv, uaPub, authSecret) {
  const salt = body.subarray(0, 16);
  const rs = body[16] << 24 | body[17] << 16 | body[18] << 8 | body[19];
  const idLen = body[20];
  const asPub = body.subarray(21, 21 + idLen);
  const ciphertext = body.subarray(21 + idLen);
  const asPubKey = await crypto.subtle.importKey("raw", asPub, ECDH, false, []);
  const ikm = new Uint8Array(await crypto.subtle.deriveBits({ name: "ECDH", public: asPubKey }, uaPriv, 256));
  const prk = await hkdf(authSecret, ikm, cat(utf8("WebPush: info\0"), uaPub, asPub), 32);
  const cekRaw = await hkdf(salt, prk, utf8("Content-Encoding: aes128gcm\0"), 16);
  const nonce = await hkdf(salt, prk, utf8("Content-Encoding: nonce\0"), 12);
  const cek = await crypto.subtle.importKey("raw", cekRaw, "AES-GCM", false, ["decrypt"]);
  const padded = new Uint8Array(await crypto.subtle.decrypt({ name: "AES-GCM", iv: nonce }, cek, ciphertext));
  let end = padded.length;
  while (end > 0 && padded[end - 1] === 0) end--;
  const delim = end > 0 ? padded[end - 1] : 0;
  if (delim !== 1 && delim !== 2) throw new Error(`bad padding delimiter 0x${delim.toString(16)}`);
  return td.decode(padded.subarray(0, end - 1));
}
async function generateVapidKeys() {
  const kp = await crypto.subtle.generateKey(ECDSA, true, ["sign", "verify"]);
  const privJwk = await crypto.subtle.exportKey("jwk", kp.privateKey);
  const pubRaw = b64urlEncode(new Uint8Array(await crypto.subtle.exportKey("raw", kp.publicKey)));
  return { privJwk, pubRaw };
}
function derInt(half) {
  let i = 0;
  while (i < half.length - 1 && half[i] === 0) i++;
  const v = half.subarray(i);
  const pad = (v[0] & 128) !== 0 ? 1 : 0;
  const out = new Uint8Array(2 + v.length + pad);
  out[0] = 2;
  out[1] = v.length + pad;
  out.set(v, 2 + pad);
  return out;
}
function rawSigToDer(sig) {
  const r = derInt(sig.subarray(0, 32));
  const s = derInt(sig.subarray(32, 64));
  return cat(new Uint8Array([48, r.length + s.length]), r, s);
}
function derSigToRaw(der) {
  if (der[0] !== 48) throw new Error("not a DER SEQUENCE");
  let o = 2;
  const read = () => {
    if (der[o] !== 2) throw new Error("expected DER INTEGER");
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
async function vapidHeaders(privJwk, pubRaw, audience) {
  const header = b64urlEncode(utf8(JSON.stringify({ typ: "JWT", alg: "ES256" })));
  const claims = b64urlEncode(utf8(JSON.stringify({
    aud: audience,
    exp: Math.floor(Date.now() / 1e3) + 12 * 3600,
    sub: "mailto:dev@fragment.club"
  })));
  const signingInput = header + "." + claims;
  const priv = await crypto.subtle.importKey("jwk", privJwk, ECDSA, false, ["sign"]);
  const rawSig = new Uint8Array(await crypto.subtle.sign({ name: "ECDSA", hash: "SHA-256" }, priv, utf8(signingInput)));
  const jwt = signingInput + "." + b64urlEncode(rawSigToDer(rawSig));
  return {
    authorization: `vapid t=${jwt}, k=${pubRaw}`,
    "crypto-key": `p256ecdsa=${pubRaw}`
  };
}
async function webpushSelfTest() {
  try {
    const ua = await crypto.subtle.generateKey(ECDH, true, ["deriveBits"]);
    const uaPub = new Uint8Array(await crypto.subtle.exportKey("raw", ua.publicKey));
    const authSecret = crypto.getRandomValues(new Uint8Array(16));
    const msg = JSON.stringify({ title: "self-test \u2714", body: "round-trip", url: "/?id=7", tag: "push-selftest" });
    const enc = await encryptPayload({ p256dh: b64urlEncode(uaPub), auth: b64urlEncode(authSecret) }, msg);
    const rs = enc.body[16] << 24 | enc.body[17] << 16 | enc.body[18] << 8 | enc.body[19];
    const back = await uaDecrypt(enc.body, ua.privateKey, uaPub, authSecret);
    if (back !== msg) throw new Error("round-trip mismatch: " + JSON.stringify(back));
    if (enc.headers["Content-Encoding"] !== "aes128gcm") throw new Error("missing Content-Encoding header");
    if (rs !== RS) throw new Error(`bad rs ${rs}`);
    if (enc.body[20] !== 65) throw new Error(`bad keyid length ${enc.body[20]}`);
    const kp = await generateVapidKeys();
    const vh = await vapidHeaders(kp.privJwk, kp.pubRaw, "https://push.example.net");
    const m = vh.authorization.match(/^vapid t=([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]+)\.([A-Za-z0-9_-]+), k=([A-Za-z0-9_-]+)$/);
    if (!m) throw new Error("authorization header not vapid t=\u2026, k=\u2026: " + vh.authorization);
    if (m[4] !== kp.pubRaw) throw new Error("k= does not match pubRaw");
    const h = JSON.parse(td.decode(b64urlDecode(m[1])));
    const c = JSON.parse(td.decode(b64urlDecode(m[2])));
    if (h.typ !== "JWT" || h.alg !== "ES256") throw new Error("bad JWT header " + JSON.stringify(h));
    if (c.aud !== "https://push.example.net") throw new Error("bad aud " + c.aud);
    if (c.sub !== "mailto:dev@fragment.club") throw new Error("bad sub " + c.sub);
    const nowSec = Math.floor(Date.now() / 1e3);
    if (typeof c.exp !== "number" || c.exp <= nowSec || c.exp > nowSec + 12 * 3600 + 60) throw new Error("bad exp " + c.exp);
    const pub = await crypto.subtle.importKey("raw", b64urlDecode(kp.pubRaw), ECDSA, false, ["verify"]);
    const sig = b64urlDecode(m[3]);
    const rawSig = derSigToRaw(sig);
    const okSig = await crypto.subtle.verify({ name: "ECDSA", hash: "SHA-256" }, pub, rawSig, utf8(m[1] + "." + m[2]));
    if (!okSig) throw new Error("VAPID signature does not verify");
    if (!/^p256ecdsa=/.test(vh["crypto-key"])) throw new Error("bad Crypto-Key header");
    return {
      ok: true,
      detail: `rfc8291 round-trip ok (${msg.length}B payload, rs=${rs}, keyid=${enc.body[20]}B, body=${enc.body.length}B); rfc8292 ES256 jwt verifies (aud=${c.aud}, exp=+${c.exp - nowSec}s)`
    };
  } catch (e) {
    return { ok: false, detail: String(e && e.message || e) };
  }
}
export {
  b64urlDecode,
  b64urlEncode,
  encryptPayload,
  generateVapidKeys,
  vapidHeaders,
  webpushSelfTest
};
