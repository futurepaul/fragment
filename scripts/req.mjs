// Dev request helper: signs NIP-98 and fetches. Usage:
//   node scripts/req.mjs GET  http://127.0.0.1:8789/api/fragments
//   node scripts/req.mjs POST http://127.0.0.1:8789/api/fragments '{"name":"x"}'
//   node scripts/req.mjs PUT  'http://.../api/f/x/file?path=site/index.html&base_rev=0' @localfile
// Key: .dev/devkey (hex), generated on first use.
import { schnorr } from '@noble/curves/secp256k1.js';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { createHash, randomBytes } from 'node:crypto';

const hexToBytes = (h) => Uint8Array.from(Buffer.from(h, 'hex'));

const keyFile = new URL('../.dev/devkey', import.meta.url).pathname;
let secret;
if (existsSync(keyFile)) secret = readFileSync(keyFile, 'utf8').trim();
else {
  secret = randomBytes(32).toString('hex');
  writeFileSync(keyFile, secret, { mode: 0o600 });
  console.error(`[req] generated dev key -> ${keyFile}`);
}
const pubkey = Buffer.from(schnorr.getPublicKey(hexToBytes(secret))).toString('hex');

const [method, url, bodyArg] = process.argv.slice(2);
if (!method || !url) {
  console.error('usage: req.mjs METHOD URL [json|@file|-]');
  process.exit(2);
}
let body = null;
if (bodyArg) {
  if (bodyArg === '-') body = readFileSync(0);
  else if (bodyArg.startsWith('@')) body = readFileSync(bodyArg.slice(1));
  else body = Buffer.from(bodyArg);
}

const tags = [
  ['u', url],
  ['method', method.toUpperCase()],
];
if (body && body.length > 0) tags.push(['payload', createHash('sha256').update(body).digest('hex')]);

const event = {
  pubkey,
  created_at: Math.floor(Date.now() / 1000),
  kind: 27235,
  tags,
  content: '',
};
const id = createHash('sha256').update(JSON.stringify([0, event.pubkey, event.created_at, event.kind, event.tags, event.content])).digest();
event.id = id.toString('hex');
event.sig = Buffer.from(schnorr.sign(id, hexToBytes(secret))).toString('hex');

const res = await fetch(url, {
  method: method.toUpperCase(),
  headers: { authorization: 'Nostr ' + Buffer.from(JSON.stringify(event)).toString('base64') },
  body,
  redirect: 'manual',
});
const ct = res.headers.get('content-type') || '';
const extra = res.headers.get('x-fragment-rev') ? ` x-rev=${res.headers.get('x-fragment-rev')}` : '';
console.error(`[${res.status}]${extra} ${ct}`);
const buf = Buffer.from(await res.arrayBuffer());
try {
  console.log(JSON.stringify(JSON.parse(buf.toString('utf8')), null, 2));
} catch {
  process.stdout.write(buf);
  if (buf.length && buf[buf.length - 1] !== 10) console.log();
}
