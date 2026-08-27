// NIP-98 client-side signing, shared by scripts/req.mjs and scripts/e2e.mjs.
// Server-side verification lives in runtime/src/auth.js; the two must agree.
import { schnorr } from '@noble/curves/secp256k1.js';
import { createHash, randomBytes } from 'node:crypto';

export const genKey = () => randomBytes(32).toString('hex');
export const hexToBytes = (h) => Uint8Array.from(Buffer.from(h, 'hex'));
export const pubkeyFromSecret = (secret) =>
  Buffer.from(schnorr.getPublicKey(hexToBytes(secret))).toString('hex');

const sha256hex = (buf) => createHash('sha256').update(buf).digest('hex');

export async function authHeader(method, url, body, secret) {
  const tags = [['u', url], ['method', method.toUpperCase()]];
  if (body && body.length > 0) tags.push(['payload', sha256hex(body)]);
  const event = {
    pubkey: pubkeyFromSecret(secret),
    created_at: Math.floor(Date.now() / 1000),
    kind: 27235,
    tags,
    content: '',
  };
  const id = createHash('sha256')
    .update(JSON.stringify([0, event.pubkey, event.created_at, event.kind, event.tags, event.content]))
    .digest();
  event.id = id.toString('hex');
  event.sig = Buffer.from(schnorr.sign(id, hexToBytes(secret))).toString('hex');
  return 'Nostr ' + Buffer.from(JSON.stringify(event)).toString('base64');
}

// Generic Blossom auth-event builder (kind 24242, BUD-02 style) — reusable
// for blobsd upload/delete/list, not just NIP-98 HTTP auth. Wire shape per
// docs/blob-tier.md: `t` = action verb ("upload"|"delete"|"list"), `x` =
// server URL (must equal the server's configured public URL), `payload` =
// sha256 of the body being authorized (present when given), `expiration` =
// unix seconds after which the event is dead. Returns the full
// "Authorization: Nostr <base64>" header value.
export async function buildEvent(opts, secret) {
  const {
    kind = 24242,
    actionTag,
    serverUrl,
    payloadHash,
    expirationSecs = 60,
  } = opts;
  if (!actionTag || !serverUrl) throw new Error('buildEvent needs actionTag and serverUrl');
  const now = Math.floor(Date.now() / 1000);
  const tags = [['t', actionTag], ['x', serverUrl]];
  if (payloadHash) tags.push(['payload', payloadHash]);
  tags.push(['expiration', String(now + Math.max(1, expirationSecs | 0))]);
  const event = {
    pubkey: pubkeyFromSecret(secret),
    created_at: now,
    kind,
    tags,
    content: '',
  };
  const id = createHash('sha256')
    .update(JSON.stringify([0, event.pubkey, event.created_at, event.kind, event.tags, event.content]))
    .digest();
  event.id = id.toString('hex');
  event.sig = Buffer.from(schnorr.sign(id, hexToBytes(secret))).toString('hex');
  return 'Nostr ' + Buffer.from(JSON.stringify(event)).toString('base64');
}

// Signed fetch. Returns the raw Response. `url` must be exactly what the
// server sees (origin+path+search) since the u tag is compared canonically.
export async function nreq(method, url, body, secret) {
  let buf = null;
  if (body != null) buf = typeof body === 'string' ? Buffer.from(body) : Buffer.isBuffer(body) ? body : Buffer.from(body);
  const headers = { authorization: await authHeader(method.toUpperCase(), url, buf, secret) };
  return fetch(url, { method: method.toUpperCase(), headers, body: buf ?? undefined });
}
