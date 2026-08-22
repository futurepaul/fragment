// Dev request helper: signs NIP-98 and fetches. Usage:
//   node scripts/req.mjs GET  http://127.0.0.1:8789/api/fragments
//   node scripts/req.mjs POST http://127.0.0.1:8789/api/fragments '{"name":"x"}'
//   node scripts/req.mjs PUT  'http://.../api/f/x/file?path=site/index.html&base_rev=0' @localfile
// Key: .dev/devkey (hex), generated on first use.
import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { randomBytes } from 'node:crypto';
import { authHeader } from './nip98.mjs';

const keyFile = new URL('../.dev/devkey', import.meta.url).pathname;
let secret;
if (existsSync(keyFile)) secret = readFileSync(keyFile, 'utf8').trim();
else {
  secret = randomBytes(32).toString('hex');
  writeFileSync(keyFile, secret, { mode: 0o600 });
  console.error(`[req] generated dev key -> ${keyFile}`);
}

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

const res = await fetch(url, {
  method: method.toUpperCase(),
  headers: { authorization: await authHeader(method.toUpperCase(), url, body, secret) },
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
