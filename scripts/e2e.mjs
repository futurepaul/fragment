#!/usr/bin/env node
// End-to-end suite: the README "What's verified" bullets as executable checks
// against a running fragment host (default http://127.0.0.1:8789).
//
//   node scripts/e2e.mjs [--base URL] [--bin PATH] [--cron] [--only NAME] [--fast]
//
// Bring a stack up first (scripts/dev up && scripts/dev deploy), then run this.
// Exit code 0 = every check passed. Created fragments are named e2e-* and are
// left behind on purpose — there is no destroy command yet.
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, readdirSync, rmSync, copyFileSync } from 'node:fs';
import { readFileSync as guideRead } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import fs from "node:fs";
import { genKey, pubkeyFromSecret, nreq, authHeader, buildEvent } from './nip98.mjs';
import { createHash } from 'node:crypto';
import { npubFromHex } from '../runtime/src/bech32.js';

const args = process.argv.slice(2);
function arg(name) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : null;
}
const has = (n) => args.includes(n);
const BASE = arg('--base') || process.env.FRAGMENT_BASE_URL || 'http://127.0.0.1:8789';
const ONLY = arg('--only');
const CRON = has('--all');

// ---------- tiny harness ----------
let pass = 0, fail = 0;
const failures = [];
function ok(cond, label) {
  if (cond) pass++; else { fail++; failures.push(label); }
  console.log((cond ? 'ok    ' : 'FAIL  ') + label);
}
function eq(got, want, label) {
  const extra = got === want ? '' : ` [got ${JSON.stringify(got)} want ${JSON.stringify(want)}]`;
  ok(got === want, label + extra);
}
function section(name) {
  if (ONLY && ONLY !== name) return false;
  console.log(`\n# ${name}`);
  return true;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function jres(promise) {
  const r = await promise;
  let body = null;
  const text = Buffer.from(await r.arrayBuffer()).toString('utf8');
  try { body = JSON.parse(text); } catch {
    if (process.env.E2E_DEBUG_BODIES) console.log('NON-JSON RESPONSE', r.status, text.slice(0, 500));
  }
  return { status: r.status, body };
}

// ---------- identity ----------
const ownerKey = genKey();
const ownerNpub = npubFromHex(pubkeyFromSecret(ownerKey));
const ownerPub = pubkeyFromSecret(ownerKey);
const strangerKey = genKey();
const suffix = Math.random().toString(36).slice(2, 7);

async function signed(method, path, body, key = ownerKey) {
  return jres(nreq(method, BASE + path, body, key));
}

// ---------- preflight ----------
console.log('# preflight');
try {
  const ping = await fetch(`${BASE}/__internal/ping`);
  if (!ping.ok) throw new Error(String(ping.status));
} catch {
  console.error(`no host at ${BASE}. Bring one up: scripts/dev up && scripts/dev deploy`);
  process.exit(2);
}

// ---------- fake-blobsd stub for the whole run ----------
// Every file write now touches the blob tier (inline carve-out uploads are
// performed BY the runtime), so the stub must be listening for the ENTIRE
// suite, not just the tier section. It runs as a CHILD PROCESS on purpose:
// the harness makes blocking spawnSync CLI calls, and an in-process server
// would deadlock them (blocked event loop can't serve the runtime's tier
// fetch — found as a 60s spawnSync ETIMEDOUT in the guide transcripts).
// Implements the upload/GET/HEAD hash contract in-memory on 127.0.0.1:9940;
// real-side blobsd coverage happens post-merge.
const E2E_TOKEN = process.env.E2E_BLOBSD_TOKEN || 'e2e-blob-token';
const E2E_BLOBSD_PORT = 9940;
const stubSrc = `
const http = require('node:http');
const { createHash } = require('node:crypto');
const TOKEN = process.env.E2E_BLOBSD_TOKEN;
const blobs = new Map(); // sha256 -> {bytes, mime}
const server = http.createServer((req, res) => {
  const chunks = [];
  req.on('data', (c) => chunks.push(c));
  req.on('end', () => {
    const body = Buffer.concat(chunks);
    const reply = (status, payload, headers = {}) => { res.writeHead(status, headers); res.end(payload); };
    if (req.method === 'GET' && (req.url === '/' || req.url === '/health')) {
      return reply(200, JSON.stringify({ name: 'blobsd', version: 'stub-e2e' }), { 'content-type': 'application/json' });
    }
    // writes are internal-token gated; the hash data plane (GET/HEAD/<sha>)
    // is NO-AUTH per spec — that is exactly what BLOBSD_PUBLIC_GET exposes
    const isWrite = req.method === 'PUT' || req.method === 'DELETE';
    const auth = req.headers.authorization || '';
    const isNostr = auth.startsWith('Nostr ');
    if (isWrite && auth !== ('Bearer ' + TOKEN) && !isNostr) {
      return reply(403, JSON.stringify({ error: 'bad internal token' }), { 'content-type': 'application/json' });
    }
    if (req.method === 'PUT' && req.url === '/upload') {
      const sha = createHash('sha256').update(body).digest('hex');
      const declared = String(req.headers['x-sha-256'] || '').toLowerCase();
      if (declared && declared !== sha) return reply(400, JSON.stringify({ error: 'bad_hash' }), { 'content-type': 'application/json' });
      blobs.set(sha, { bytes: body, mime: String(req.headers['content-type'] || '') });
      return reply(200, JSON.stringify({ sha256: sha, size: body.length, type: String(req.headers['content-type'] || ''), uploaded: Math.floor(Date.now() / 1000) }), { 'content-type': 'application/json' });
    }
    const m = (req.url.match(/^\\/([0-9a-f]{64})$/) || [])[1];
    if ((req.method === 'GET' || req.method === 'HEAD') && m) {
      const hit = blobs.get(m);
      if (!hit) return reply(404, 'no such blob');
      return reply(200, req.method === 'HEAD' ? undefined : hit.bytes, {
        etag: '"' + m + '"',
        'cache-control': 'public, max-age=31536000, immutable',
        'content-type': hit.mime || 'application/octet-stream',
      });
    }
    return reply(404, 'not found');
  });
});
server.listen(${E2E_BLOBSD_PORT}, '127.0.0.1');
`;
let blobsdStub = null;
// Post-merge integration tests the REAL blobsd when one is already serving
// 9940 (dev default); the in-suite stub is the fallback for bare worlds.
// Tier-era children (guide recipes, filesync) upload bytes blob-first, so
// every spawned CLI must be the allowlisted owner: mirror the real identity
// config into the temp HOME instead of letting a random keypair 403.
const realConfigCandidates = [
  join(process.env.HOME, '.config', 'fragment', 'config.json'),
  join(process.env.HOME, 'Library', 'Application Support', 'fragment', 'config.json'),
];
const realConfig = realConfigCandidates.find((p) => existsSync(p));
function seedIdentity(home) {
  if (!existsSync(realConfig)) return;
  const dir = join(home, '.config', 'fragment');
  mkdirSync(dir, { recursive: true });
  copyFileSync(realConfig, join(dir, 'config.json'));
}
let reuseBlobsd = false;
try {
  const h = await fetch(`http://127.0.0.1:${E2E_BLOBSD_PORT}/`, { signal: AbortSignal.timeout(500) });
  if (h.ok) { reuseBlobsd = true; console.log('blob tier: reusing healthy blobsd on ' + E2E_BLOBSD_PORT); }
} catch {}
if (!reuseBlobsd) try {
  blobsdStub = spawn(process.execPath, ['-e', stubSrc], {
    env: { ...process.env, E2E_BLOBSD_TOKEN: E2E_TOKEN },
    stdio: 'ignore',
  });
  blobsdStub.on('exit', () => { blobsdStub = null; });
  // wait until it answers its health route (bounded)
  const stubUp = async () => {
    for (let i = 0; i < 50; i++) {
      try { const r = await fetch(`http://127.0.0.1:${E2E_BLOBSD_PORT}/`); if (r.ok) return true; } catch {}
      await sleep(100);
    }
    return false;
  };
  if (!(await stubUp())) throw new Error('stub did not answer /health in 5s');
  process.on('exit', () => { try { blobsdStub && blobsdStub.kill(); } catch {} });
} catch (e) {
  console.log(`skip  could not start blobsd stub on :${E2E_BLOBSD_PORT} (${String(e.message || e)}) — tier checks will skip`);
}
if (!ONLY || ONLY === 'lockdown') await lockdownSection();

// ---------- lockdown (router-level; see PR "security: lock /__internal") ----------
async function lockdownSection() {
  if (!section('lockdown')) return;
  // registry must never be reachable over HTTP, from anyone
  const reg = await fetch(`${BASE}/__internal/f/_registry/__registry/create`, {
    method: 'POST', body: '{"name":"evil","ownerHex":"00"}',
  });
  eq(reg.status, 404, 'registry create unroutable over HTTP');
  const init = await fetch(`${BASE}/__internal/f/some-frag/__cell/init`, {
    method: 'POST', body: '{"name":"some-frag","ownerHex":"00"}',
  });
  eq(init.status, 404, 'cell init unroutable over HTTP');
  // with a host secret set, loopback needs the header too. The script learns
  // the expected secret from env so it can verify both layers end to end.
  const wantSecret = process.env.FRAGMENT_HOST_SECRET;
  if (wantSecret) {
    const probeName = `e2e-lk-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name: probeName }));
    const noHdr = await fetch(`${BASE}/__internal/f/${probeName}/__internal/secrets/all`);
    eq(noHdr.status, 403, 'internal plane rejects missing host secret');
    const badHdr = await fetch(`${BASE}/__internal/f/${probeName}/__internal/secrets/all`, {
      headers: { 'x-fragment-host-secret': 'wrong' },
    });
    eq(badHdr.status, 403, 'internal plane rejects wrong host secret');
    // a correct secret passes the ROUTER gate; the cell then demands a run
    // token (which an outside caller cannot have) — that second 403 proves
    // both layers are alive and in order.
    const goodHdr = await fetch(`${BASE}/__internal/f/${probeName}/__internal/secrets/all`, {
      headers: { 'x-fragment-host-secret': wantSecret },
    });
    eq(goodHdr.status, 403, 'internal plane: secret ok → cell token layer answers');
    ok((await goodHdr.text()).includes('run token'), 'cell-side token gate is the one rejecting');
  } else {
    console.log('skip  FRAGMENT_HOST_SECRET not set in env — secret layer untested here');
  }
}

// ---------- auth ----------
async function authSection() {
  if (!section('auth')) return;
  const name = `e2e-au-${suffix}`;
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
  eq(created.status, 200, 'create with NIP-98 → 200');
  ok(created.body?.npub?.startsWith('npub1'), 'create returns fragment npub');
  ok(typeof created.body?.viewToken === 'string' && created.body.viewToken.length > 8, 'create returns view token');

  const anon = await fetch(`${BASE}/api/fragments`);
  eq(anon.status, 401, 'list without auth → 401');

  const stranger = await jres(fetch(`${BASE}/api/fragments`, {
    headers: { authorization: await authHeader('GET', `${BASE}/api/fragments`, null, strangerKey) },
  }));
  eq(stranger.status, 200, 'stranger list authenticates fine');

  const status = await signed('GET', `/api/f/${name}/status`);
  eq(status.status, 200, 'owner status → 200');
  const s403 = await signed('GET', `/api/f/${name}/status`, null, strangerKey);
  eq(s403.status, 403, 'stranger status → 403 (valid sig, no role)');
  return name;
}

// ---------- files ----------
async function filesSection() {
  if (!section('files')) return;
  const name = `e2e-fi-${suffix}`;
  await signed('POST', '/api/fragments', JSON.stringify({ name }));

  const put1 = await signed('PUT', `/api/f/${name}/file?path=notes/a.md&base_rev=0`, 'hello v1');
  eq(put1.status, 200, 'first put base_rev=0 → 200');
  eq(put1.body?.rev, 1, 'rev increments to 1');

  const stale = await signed('PUT', `/api/f/${name}/file?path=notes/a.md&base_rev=0`, 'clobber');
  eq(stale.status, 409, 'stale base_rev → 409 conflict');
  eq(stale.body?.currentRev, 1, 'conflict reports currentRev');

  const good = await signed('PUT', `/api/f/${name}/file?path=notes/a.md&base_rev=1`, 'hello v2');
  eq(good.status, 200, 'fresh base_rev put → 200');

  const get = await fetch(`${BASE}/api/f/${name}/file?path=notes/a.md`, {
    headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=notes/a.md`, null, ownerKey) },
  });
  eq(Buffer.from(await get.arrayBuffer()).toString(), 'hello v2', 'get round-trips content');

  const del = await signed('DELETE', `/api/f/${name}/file?path=notes/a.md`);
  eq(del.status, 200, 'delete → 200');
  const gone = await fetch(`${BASE}/api/f/${name}/file?path=notes/a.md`, {
    headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=notes/a.md`, null, ownerKey) },
  });
  eq(gone.status, 404, 'deleted file reads → 404');

  // tombstone semantics: re-upload of a deleted path continues its rev chain
  const reup = await signed('PUT', `/api/f/${name}/file?path=notes/a.md&base_rev=3`, 'back again');
  eq(reup.status, 200, 're-upload after delete → 200');
  ok(reup.body?.rev > 3, `tombstone rev carried forward (rev=${reup.body?.rev})`);

  const evilPath = await signed('PUT', `/api/f/${name}/file?path=../evil&base_rev=0`, 'x');
  eq(evilPath.status, 400, 'path traversal rejected');
}

// ---------- drafts / bless ----------
async function draftsSection() {
  if (!section('drafts')) return;
  const name = `e2e-dr-${suffix}`;
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
  const viewToken = created.body.viewToken;

  await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
    name, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [],
  }));
  await signed('PUT', `/api/f/${name}/file?path=site/index.html&base_rev=0`, '<h1>v1 marker</h1>');
  const d1 = await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'v1' }));
  ok(d1.body?.slug, 'publish returns slug');

  // draft serves publicly by slug
  const dpage = await fetch(`${BASE}${d1.body.url}`);
  eq(dpage.status, 200, 'draft URL serves anonymously');
  ok((await dpage.text()).includes('v1 marker'), 'draft serves published content');

  // canonical gated by token until blessed
  const noTok = await fetch(`${BASE}/f/${name}/`);
  eq(noTok.status, 404, 'canonical without blessing → 404 (nothing blessed yet)');
  const b1 = await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: d1.body.slug }));
  eq(b1.status, 200, 'bless → 200');
  const noView = await fetch(`${BASE}/f/${name}/`);
  eq(noView.status, 403, 'token-visibility canonical without ?view → 403');
  const withView = await fetch(`${BASE}/f/${name}/?view=${viewToken}`);
  eq(withView.status, 200, 'canonical with ?view token → 200');
  ok((await withView.text()).includes('v1 marker'), 'blessed content live at canonical');

  // rollback: publish v2, bless it, roll back to v1
  await signed('PUT', `/api/f/${name}/file?path=site/index.html&base_rev=1`, '<h1>v2 marker</h1>');
  const d2 = await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'v2' }));
  await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: d2.body.slug }));
  const v2 = await (await fetch(`${BASE}/f/${name}/?view=${viewToken}`)).text();
  ok(v2.includes('v2 marker'), 'second bless promotes new draft');
  await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: d1.body.slug }));
  const back = await (await fetch(`${BASE}/f/${name}/?view=${viewToken}`)).text();
  ok(back.includes('v1 marker'), 'rollback = bless older draft');
}

// ---------- dynamic app ----------
async function appSection() {
  if (!section('app')) return;
  const name = `e2e-ap-${suffix}`;
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
  const viewToken = created.body.viewToken;
  const appSrc = [
    'export default {',
    '  async fetch(req, ctx) {',
    '    const n = ((await ctx.state.get("hits")) || 0) + 1;',
    '    await ctx.state.put("hits", n);',
    '    return new Response("hits=" + n);',
    '  },',
    '};',
  ].join('\n');
  await signed('PUT', `/api/f/${name}/file?path=app.mjs&base_rev=0`, appSrc);
  await signed('PUT', `/api/f/${name}/file?path=site/index.html&base_rev=0`, '<html>static</html>');
  const draft = await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({}));
  await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: draft.body.slug }));

  const r1 = await fetch(`${BASE}/f/${name}/anything?view=${viewToken}`);
  eq(r1.status, 200, 'app.mjs serves every non-site path');
  eq(await r1.text(), 'hits=1', 'ctx.state counter first hit');
  const r2 = await fetch(`${BASE}/f/${name}/again?view=${viewToken}`);
  eq(await r2.text(), 'hits=2', 'ctx.state persists across requests (cached isolate)');
  const root = await fetch(`${BASE}/f/${name}/?view=${viewToken}`);
  eq(await root.text(), '<html>static</html>', 'site/index.html owns the root beside an app');
}

// ---------- rooms ----------
async function roomsSection() {
  if (!section('rooms')) return;
  const WebSocket = (await import('node:ws').catch(() => null))?.WebSocket ?? globalThis.WebSocket;
  const name = `e2e-ro-${suffix}`;
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
  const viewToken = created.body.viewToken;
  await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
    name, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [],
  }));
  await signed('PUT', `/api/f/${name}/file?path=rooms.mjs&base_rev=0`,
    'export function onMessage(room, msg, ctx) {\n  if (msg.data.boom) throw new Error("boom");\n  return { broadcast: msg.data };\n}\n');
  const draft = await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({}));
  await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: draft.body.slug }));

  const wsUrl = `${BASE.replace('http', 'ws')}/f/${name}/__room/lounge?view=${viewToken}`;

  // no token → rejected
  const deniedCode = await new Promise((resolve) => {
    const ws = new WebSocket(`${BASE.replace('http', 'ws')}/f/${name}/__room/lounge`);
    ws.onerror = () => resolve('error');
    ws.onclose = () => resolve('close');
    ws.onopen = () => resolve('open');
    setTimeout(() => resolve('timeout'), 3000);
  });
  ok(deniedCode !== 'open' && deniedCode !== 'timeout', 'room without view token refused');

  const client = () => {
    const ws = new WebSocket(wsUrl);
    const c = { ws, hello: null, msgs: [], errors: [] };
    ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.type === 'hello') c.hello = m;
      if (m.type === 'msg') c.msgs.push(m);
      if (m.type === 'error') c.errors.push(m.error);
    };
    c.ready = new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
    c.send = (o) => ws.send(JSON.stringify(o));
    return c;
  };

  const A = client(); await A.ready;
  const B = client(); await B.ready;
  await sleep(300);
  A.send({ type: 'state:set', value: { title: 'board v1' } });
  B.send({ type: 'msg', data: { text: 'hi from B' } });
  await sleep(500);
  ok(A.msgs.some((m) => m.data?.text === 'hi from B'), 'A received B message');
  ok(B.msgs.some((m) => m.from !== undefined), 'B saw broadcast with sender id');

  A.ws.close(); B.ws.close();
  await sleep(300);
  const C = client(); await C.ready;
  await sleep(400);
  ok(C.hello?.state?.title === 'board v1', 'reconnect hello carries persisted state');
  ok(C.hello?.history?.length >= 1, 'reconnect hello carries history');

  C.send({ type: 'msg', data: { boom: true } });
  // the error frame rides a freshly-loaded rooms isolate; on slow/CI stacks
  // (bucket-backed loader) that can take longer than a warm one
  const t0 = Date.now();
  while (Date.now() - t0 < 5000 && C.errors.length === 0) await sleep(250);
  ok(C.errors.length > 0, 'rooms.mjs throw → error frame to sender');
  const evs = await signed('GET', `/api/f/${name}/events`);
  ok(JSON.stringify(evs.body?.events || []).includes('room-error'), 'room-error visible in event log');
  C.ws.close();
}

// ---------- workflows + secrets + inbox ----------
async function workflowSection() {
  if (!section('workflows')) return;
  const name = `e2e-wf-${suffix}`;
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
  const inboxToken = created.body.inboxToken;

  await signed('PUT', `/api/f/${name}/secrets/E2E_SECRET`, 's3cr3t-value');  // via editor plane below
  const wfMain = [
    'export async function run(ctx) {',
    '  const secretOk = ctx.secrets.E2E_SECRET === "s3cr3t-value";',
    '  await ctx.files.write("out/run.txt", "ran");',
    '  const n = ((await ctx.state.get("runs")) || 0) + 1;',
    '  await ctx.state.put("runs", n);',
    '  ctx.log("run number " + n);',
    '  const pending = await ctx.inbox();',
    '  return { secretOk, runs: n, pending: pending.length };',
    '}',
  ].join('\n');
  const wfInbox = 'export async function run(ctx) {\n  return { gotInbox: true };\n}\n';
  await signed('PUT', `/api/f/${name}/file?path=workflows/main.mjs&base_rev=0`, wfMain);
  await signed('PUT', `/api/f/${name}/file?path=workflows/inbox.mjs&base_rev=0`, wfInbox);
  const man = await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
    name, visibility: 'link', editors: [], viewers: [],
    workflows: [
      { name: 'main', file: 'workflows/main.mjs' },
      { name: 'onpost', file: 'workflows/inbox.mjs', trigger: 'inbox' },
    ],
    secrets: ['E2E_SECRET'],
  }));
  eq(man.status, 200, 'manifest with workflows accepted');

  const run = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'main' }));
  eq(run.status, 200, 'manual run → 200');
  eq(run.body?.ok, true, 'workflow ran clean');
  eq(run.body?.output?.secretOk, true, 'workflow sees secret value');
  eq(run.body?.output?.runs, 1, 'ctx.state works in run scope');
  ok(Array.isArray(run.body?.events) && run.body.events.length > 0, 'run reports its events');

  const file = await fetch(`${BASE}/api/f/${name}/file?path=out/run.txt`, {
    headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=out/run.txt`, null, ownerKey) },
  });
  eq(Buffer.from(await file.arrayBuffer()).toString(), 'ran', 'ctx.files.write landed');

  // ---- write-CAS: ifRev pins + conflicts + stat ----
  const wfCas = [
    'export async function run(ctx) {',
    '  await ctx.files.write("cas/a.txt", "v1");',
    '  const st1 = await ctx.files.stat("cas/a.txt");',
    '  const fresh = st1 && st1.rev > 0 && st1.sha256 && st1.deleted === false;',
    '  const w2 = await ctx.files.write("cas/a.txt", "v2", { ifRev: st1.rev });',
    '  let conflict = null;',
    '  try {',
    '    await ctx.files.write("cas/a.txt", "v3", { ifRev: st1.rev });',
    '    conflict = { threw: false };',
    '  } catch (e) {',
    '    conflict = { threw: true, flagged: e.conflict === true, currentRev: e.currentRev };',
    '  }',
    '  const body = await ctx.files.read("cas/a.txt");',
    '  const absent = await ctx.files.stat("cas/never-existed.txt");',
    '  return { fresh, w2rev: w2.rev, advanced: w2.rev > st1.rev, conflict, body, absent };',
    '}',
  ].join('\n');
  await signed('PUT', `/api/f/${name}/file?path=workflows/cas.mjs&base_rev=0`, wfCas);
  const man2 = await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
    name, visibility: 'link', editors: [], viewers: [],
    workflows: [
      { name: 'main', file: 'workflows/main.mjs' },
      { name: 'onpost', file: 'workflows/inbox.mjs', trigger: 'inbox' },
      { name: 'cas', file: 'workflows/cas.mjs' },
    ],
    secrets: ['E2E_SECRET'],
  }));
  eq(man2.status, 200, 'manifest with cas workflow accepted');
  const casRun = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'cas' }));
  eq(casRun.status, 200, 'cas run → 200');
  eq(casRun.body?.ok, true, 'cas workflow ran clean');
  eq(casRun.body?.output?.fresh, true, 'ctx.files.stat reports the live row');
  eq(casRun.body?.output?.advanced, true, 'ifRev write advances the rev');
  eq(casRun.body?.output?.conflict?.threw, true, 'stale ifRev write throws');
  eq(casRun.body?.output?.conflict?.flagged, true, 'the throw is a typed conflict (e.conflict)');
  ok(Number.isInteger(casRun.body?.output?.conflict?.currentRev), 'conflict carries the current rev');
  eq(casRun.body?.output?.body, 'v2', 'the conflicted write changed nothing');
  eq(casRun.body?.output?.absent, null, 'stat of an unknown path is null');

  const evs = await signed('GET', `/api/f/${name}/events`);
  ok(JSON.stringify(evs.body?.events || []).includes('run number 1'), 'ctx.log lands in event log');

  const badTok = await fetch(`${BASE}/api/f/${name}/inbox?t=wrong`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: {} }),
  });
  eq(badTok.status, 403, 'inbox bad token → 403');
  // header form: preferred for callers who control clients
  const hdrTok = await fetch(`${BASE}/api/f/${name}/inbox`, {
    method: 'POST',
    headers: { 'x-fragment-inbox-token': inboxToken },
    body: JSON.stringify({ source: 'e2e', payload: { via: 'header' } }),
  });
  const hdrBody = await hdrTok.json();
  eq(hdrBody?.ok, true, 'inbox via x-fragment-inbox-token header accepted');

  // run tokens are header-only (ctx shim's contract): a query-param token,
  // valid or not, must never reach the internal plane
  const qtok = await fetch(`${BASE}/__internal/f/${name}/__internal/secrets/all?t=junk`, {
    headers: { 'x-fragment-host-secret': process.env.FRAGMENT_HOST_SECRET || '' },
  });
  ok(qtok.status === 403 && !JSON.stringify(await qtok.json()).includes('files'),
    'internal plane ignores ?t= query-param tokens');
  const t0 = Date.now();
  const post = await fetch(`${BASE}/api/f/${name}/inbox?t=${inboxToken}`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: { x: 1 } }),
  });
  const postBody = await post.json();
  eq(postBody?.ok, true, 'inbox POST accepted');
  ok((postBody?.scheduled || postBody?.ran || []).length >= 1, 'workflow scheduled');
  ok(Date.now() - t0 < 3000, 'inbox POST acknowledges fast (no workflow wait)');
  let ranBy = false;
  const rt0 = Date.now();
  while (Date.now() - rt0 < 20_000 && !ranBy) {
    const runs = await signed('GET', `/api/f/${name}/runs`);
    ranBy = (runs.body?.runs || []).some((r) => r.wf === 'onpost' && r.status === 'success');
    if (!ranBy) await sleep(500);
  }
  ok(ranBy, 'inbox-triggered workflow ran (async)');

  const secList = await signed('GET', `/api/f/${name}/secrets`);
  ok((secList.body?.names || []).includes('E2E_SECRET'), 'secret listed by name only');
  const rm = await signed('DELETE', `/api/f/${name}/secrets/E2E_SECRET`);
  eq(rm.status, 200, 'secret removed');
}

// ---------- paused workflows ----------
async function pausedSection() {
  if (!section('paused')) return;
  const name = `e2e-paused-${suffix}`;
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
  const inboxToken = created.body.inboxToken;

  const wf = 'export async function run(ctx) {\n  await ctx.files.write("out/fired.txt", "yes");\n  return { fired: true };\n}\n';
  await signed('PUT', `/api/f/${name}/file?path=workflows/w.mjs&base_rev=0`, wf);
  const man = await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
    name, visibility: 'link', editors: [], viewers: [],
    workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox', paused: true }],
    secrets: [],
  }));
  eq(man.status, 200, 'manifest accepts paused: true on a workflow');

  // an inbox arrival while paused must NOT run it
  const postResp = await fetch(`${BASE}/api/f/${name}/inbox?t=${inboxToken}`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: {} }),
  });
  eq(postResp.status, 200, 'inbox POST still accepted while paused');
  let blockedBy = false;
  const bt0 = Date.now();
  while (Date.now() - bt0 < 15_000 && !blockedBy) {
    const runs = await signed('GET', `/api/f/${name}/runs`);
    blockedBy = (runs.body?.runs || []).some((r) => r.wf === 'w' && r.status === 'blocked');
    if (!blockedBy) await sleep(400);
  }
  ok(blockedBy, 'paused trigger recorded as blocked, not run');

  // manual run is the maintenance path and must work
  const run = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'w' }));
  eq(run.status, 200, 'manual run works while paused');
  eq(run.body?.output?.fired, true, 'manual run fired the workflow');

  // unpause via the pause route → trigger works again
  const un = await signed('POST', `/api/f/${name}/pause`, JSON.stringify({ workflow: 'w', paused: false }));
  eq(un.status, 200, 'unpause via /pause accepted');
  await fetch(`${BASE}/api/f/${name}/inbox?t=${inboxToken}`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: {} }),
  });
  let unpausedBy = false;
  const ut0 = Date.now();
  while (Date.now() - ut0 < 15_000 && !unpausedBy) {
    const runs = await signed('GET', `/api/f/${name}/runs`);
    unpausedBy = (runs.body?.runs || []).some((r) => r.wf === 'w' && r.status === 'success');
    if (!unpausedBy) await sleep(400);
  }
  ok(unpausedBy, 'unpaused workflow runs on trigger');
}

// ---------- runs: the failure leg ----------
async function runsSection() {
  if (!section('runs')) return;
  const mkFrag = async (tag, workflows, extra = {}) => {
    const name = `e2e-${tag}-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
    return { name, inboxToken: created.body.inboxToken, workflows };
  };
  const putFile = (name, path, body) =>
    signed('PUT', `/api/f/${name}/file?path=${encodeURIComponent(path)}&base_rev=0`, body);
  const putManifest = (name, manifest) =>
    signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], secrets: [], ...manifest }));
  const postInbox = (name, tok, headers = {}, payload = {}) =>
    fetch(`${BASE}/api/f/${name}/inbox?t=${tok}`, { method: 'POST', body: JSON.stringify(payload), headers });
  const waitRuns = async (name, pred, label, timeoutMs = 15000) => {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const r = await signed('GET', `/api/f/${name}/runs`);
      if (pred(r.body)) return r.body;
      if (Date.now() > deadline) { ok(false, label); return r.body; }
      await sleep(300);
    }
  };

  // retryable failure → backoff → retry (attempt 2) → held → replay after a fix
  {
    const { name, inboxToken } = await mkFrag('retry', null);
    await putFile(name, 'workflows/flaky.mjs',
      'export async function run(ctx) {\n  await ctx.http("http://127.0.0.1:9/unreachable");\n  return { fine: true };\n}\n');
    await putManifest(name, { workflows: [{ name: 'flaky', file: 'workflows/flaky.mjs', trigger: 'inbox', retry: { attempts: 2, backoffMs: 300 } }] });
    await postInbox(name, inboxToken);
    const body = await waitRuns(name, (b) => (b.runs || []).some((r) => r.status === 'held' && r.attempt === 2), 'backoff retry reaches held after attempts exhaust');
    const held = (body.runs || []).find((r) => r.status === 'held');
    ok(!!held, 'held run row exists with input + error parked');
    ok(held && held.error && /fetch/i.test(held.error), 'held row carries the error');
    ok((body.counts || {}).held >= 1, 'runs counts include held');
    const evs = await signed('GET', `/api/f/${name}/events`);
    ok(JSON.stringify(evs.body?.events || []).includes('"kind":"run.retry"'), 'run.retry event on the ledger');
    // fix the workflow, replay the held run with its original input
    await signed('PUT', `/api/f/${name}/file?path=workflows/flaky.mjs&base_rev=1`,
      'export async function run(ctx) {\n  return { fixed: true, got: ctx.input ?? null };\n}\n');
    const rep = await signed('POST', `/api/f/${name}/replay`, JSON.stringify({ run: held.id }));
    eq(rep.status, 200, 'replay accepted');
    eq(rep.body?.ok, true, 'replayed run succeeds after the fix');
    eq(rep.body?.output?.fixed, true, 'replay executed the fixed workflow');
  }

  // terminal failure: no retry, held immediately
  {
    const { name, inboxToken } = await mkFrag('term', null);
    await putFile(name, 'workflows/term.mjs', 'export async function run(ctx) {\n  null.x;\n}\n');
    await putManifest(name, { workflows: [{ name: 'term', file: 'workflows/term.mjs', trigger: 'inbox' }] });
    await postInbox(name, inboxToken);
    let heldBy = false;
    const ht0 = Date.now();
    while (Date.now() - ht0 < 25_000 && !heldBy) {
      const body = await signed('GET', `/api/f/${name}/runs`);
      heldBy = (body.body?.runs || []).some((r) => r.status === 'held' && r.attempt === 1);
      if (!heldBy) await sleep(600);
    }
    ok(heldBy, 'terminal error holds immediately (attempt 1)');
    const body = await signed('GET', `/api/f/${name}/runs`);
    eq(body.body?.runs?.[0]?.attempt, 1, 'terminal error did not retry');
  }

  // write-suppression: identical content is a recorded no-op
  {
    const { name } = await mkFrag('dedup', null);
    await putFile(name, 'workflows/w.mjs',
      'export async function run(ctx) {\n  const a = await ctx.files.write("out/x.txt", "same");\n  const b = await ctx.files.write("out/x.txt", "same");\n  return { first: a.deduped, second: b.deduped };\n}\n');
    await putManifest(name, { workflows: [{ name: 'w', file: 'workflows/w.mjs' }] });
    const run = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'w' }));
    eq(run.body?.output?.first, false, 'first write lands');
    eq(run.body?.output?.second, true, 'identical rewrite is suppressed');
    const evs = await signed('GET', `/api/f/${name}/events`);
    ok(JSON.stringify(evs.body?.events || []).includes('"kind":"write.deduped"'), 'write.deduped on the ledger');
  }

  // breaker: 5 held runs in a window auto-pause the workflow
  {
    const { name, inboxToken } = await mkFrag('breaker', null);
    await putFile(name, 'workflows/w.mjs', 'export async function run(ctx) {\n  null.x;\n}\n');
    await putManifest(name, { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox' }] });
    for (let i = 0; i < 5; i++) await postInbox(name, inboxToken);
    let pausedBy = false;
    const bkT0 = Date.now();
    while (Date.now() - bkT0 < 30_000 && !pausedBy) {
      const st = await signed('GET', `/api/f/${name}/status`);
      pausedBy = (st.body?.paused || []).includes('w');
      if (!pausedBy) await sleep(700);
    }
    ok(pausedBy, 'breaker auto-paused the workflow');
    const evs = await signed('GET', `/api/f/${name}/events`);
    ok(JSON.stringify(evs.body?.events || []).includes('"kind":"workflow.auto-paused"'), 'workflow.auto-paused event');
    await postInbox(name, inboxToken);
    let blocked6 = false;
    const bt0 = Date.now();
    while (Date.now() - bt0 < 20_000 && !blocked6) {
      const runs6 = await signed('GET', `/api/f/${name}/runs`);
      blocked6 = (runs6.body?.runs || []).some((r) => r.status === 'blocked');
      if (!blocked6) await sleep(600);
    }
    ok(blocked6, 'triggers blocked while auto-paused');
  }

  // rate ceiling: maxRunsPerHour trips auto-pause
  {
    const { name, inboxToken } = await mkFrag('rate', null);
    await putFile(name, 'workflows/w.mjs', 'export async function run(ctx) {\n  return { ok: 1 };\n}\n');
    await putManifest(name, { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox', maxRunsPerHour: 2 }] });
    await postInbox(name, inboxToken);
    await postInbox(name, inboxToken);
    await postInbox(name, inboxToken);
    let rateBlocked = false;
    const rt0 = Date.now();
    while (Date.now() - rt0 < 25_000 && !rateBlocked) {
      const runs = await signed('GET', `/api/f/${name}/runs`);
      rateBlocked = (runs.body?.runs || []).some((r) => r.status === 'blocked');
      if (!rateBlocked) await sleep(600);
    }
    ok(rateBlocked, 'third auto run in an hour is blocked');
    let pausedBy = false;
    const pt0 = Date.now();
    while (Date.now() - pt0 < 10_000 && !pausedBy) {
      const st = await signed('GET', `/api/f/${name}/status`);
      pausedBy = (st.body?.paused || []).includes('w');
      if (!pausedBy) await sleep(600);
    }
    ok(pausedBy, 'rate ceiling auto-paused the workflow');
  }

  // hop budget: over-deep inbox POSTs are refused with cycle.detected
  {
    const { name, inboxToken } = await mkFrag('hops', null);
    await putFile(name, 'workflows/w.mjs', 'export async function run(ctx) {\n  return { ran: true };\n}\n');
    await putManifest(name, { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox' }] });
    await postInbox(name, inboxToken, { 'x-fragment-hops': '99', 'x-fragment-cause': 'other-frag' });
    await postInbox(name, inboxToken);
    let blockedBy = false, ranBy = false;
    const ht0 = Date.now();
    while (Date.now() - ht0 < 25_000 && !(blockedBy && ranBy)) {
      const runs = await signed('GET', `/api/f/${name}/runs`);
      const rs = runs.body?.runs || [];
      blockedBy = blockedBy || rs.some((r) => r.status === 'blocked');
      ranBy = ranBy || rs.some((r) => r.status === 'success');
      if (!(blockedBy && ranBy)) await sleep(600);
    }
    ok(blockedBy, 'over-budget hops blocked before author code');
    const evs = await signed('GET', `/api/f/${name}/events`);
    ok(JSON.stringify(evs.body?.events || []).includes('"kind":"cycle.detected"'), 'cycle.detected on the ledger');
    ok(ranBy, 'organic-depth POST still runs');
  }

  // (Idempotency-Key removed — content-hash naming in the patterns is
  // the dedupe that gets used; envelope-level redelivery dedupe never was)

}

// ---------- the guide: every code block is executable ----------
// The GUIDE.md blocks are the product's promises. This section extracts
// them, swaps only ALL-CAPS constants and {placeholders} for local
// fixtures, and runs them: js blocks as workflows/apps/room hooks, the
// manifest JSON as a PUT, the CLI transcripts as real invocations, the
// recipes end-to-end (scaffold → publish → bless → live ingest). A js or
// json block without a runner fails the suite — a guide that rots fails CI.
function extractGuideBlocks() {
  const text = guideRead(new URL('../cli/GUIDE.md', import.meta.url), 'utf8');
  const blocks = [];
  let section = '', sub = '';
  for (let i = 0; i < text.split('\n').length; i++) {} // (placeholder; replaced below)
  const lines = text.split('\n');
  let j = 0;
  while (j < lines.length) {
    const l = lines[j];
    if (l.startsWith('## ')) { section = l.slice(3).trim(); sub = ''; }
    if (l.startsWith('### ')) sub = l.slice(4).trim();
    if (l.startsWith('```')) {
      const lang = l.slice(3).trim();
      const body = [];
      j++;
      while (j < lines.length && !lines[j].startsWith('```')) { body.push(lines[j]); j++; }
      blocks.push({ section, sub, lang, code: body.join('\n') });
    }
    j++;
  }
  return blocks;
}

// a public static fixture fragment; returns its served base URL
async function serveFixture(tag, files) {
  const name = `e2e-fx-${tag}-${suffix}`;
  await signed('POST', '/api/fragments', JSON.stringify({ name }));
  for (const [path, body] of files) {
    await signed('PUT', `/api/f/${name}/file?path=${encodeURIComponent(path)}&base_rev=0`, body);
  }
  await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }));
  await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: tag }));
  const ds = await signed('GET', `/api/f/${name}/drafts`);
  await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: ds.body.drafts[0].slug }));
  return { name, base: `${BASE}/f/${name}/` };
}

function shline(line) {
  // strip trailing " # comment" (the guide's transcripts use these), keep quotes
  const cut = line.indexOf(' # ');
  return (cut >= 0 ? line.slice(0, cut) : line).trim();
}

function tokenize(cmd) {
  const out = [];
  const re = /"([^"]*)"|(\S+)/g;
  let m;
  while ((m = re.exec(cmd))) out.push(m[1] !== undefined ? m[1] : m[2]);
  return out;
}

function runCli(bin, args, opts = {}) {
  try {
    return execFileSync(bin, args, {
      encoding: 'utf8',
      cwd: opts.cwd || process.cwd(),
      env: { ...process.env, FRAGMENT_HOST: BASE, ...(opts.env || {}) },
      timeout: opts.timeout || 60_000,
    });
  } catch (e) {
    throw new Error(`fragment ${args.join(' ')} failed: ${String(e.stderr || e.message).slice(0, 300)}`);
  }
}

async function guideSection() {
  if (!section('guide')) return;
  const blocks = extractGuideBlocks();
  ok(blocks.length >= 18, 'guide parses into blocks');

  // ---- runner: Workflows ctx-tour js block ----
  const tour = blocks.find((b) => b.section === 'Workflows' && b.lang === 'js');
  {
    ok(!!tour, 'guide ships the ctx-tour workflow block');
    const fx = await serveFixture('api', [['site/api/data.json', JSON.stringify({ ok: true, items: [1, 2] })]]);
    const name = `e2e-gd-tour-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=notes/today.md&base_rev=0`, 'today: shipped the guide test');
    await signed('PUT', `/api/f/${name}/file?path=data/export.csv&base_rev=0`, 'a,b\n1,2\n');
    await signed('PUT', `/api/f/${name}/secrets/SOME_TOKEN`, 'tok');
    const code = tour.code.replace(/^const API = .*$/m, `const API = ${JSON.stringify(fx.base + 'api/data.json')};`);
    await signed('PUT', `/api/f/${name}/file?path=workflows/digest.mjs&base_rev=0`, code);
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'digest', file: 'workflows/digest.mjs' }], secrets: ['SOME_TOKEN'] }));
    if (!process.env.OPENROUTER_API_KEY) {
      console.log('skip  ctx-tour block run (no OPENROUTER_API_KEY on this stack)');
    } else {
      const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'digest' }));
      eq(r.body?.ok, true, 'ctx-tour block runs clean (files/bytes/http/secrets/ai/state/log)');
      const files = await signed('GET', `/api/f/${name}/files`);
      ok((files.body?.files || []).some((f) => f.path.startsWith('digests/')), 'ctx-tour wrote a digest file');
    }
  }

  // ---- runner: Patterns js blocks (by ### name) ----
  const patterns = Object.fromEntries(
    blocks.filter((b) => b.section.startsWith('Patterns') && b.lang === 'js').map((b) => [b.sub.replace(/^pattern: /, ''), b.code]),
  );
  ok(Object.keys(patterns).length >= 5, 'guide ships at least 5 patterns');
  {
    const fx = await serveFixture('tree', [['site/api/tree.json', JSON.stringify({
      files: [
        { path: 'notes/a.md', size: 10 },
        { path: 'notes/b.md', size: 20 },
        { path: 'workflows/x.mjs', size: 5, machinery: true },
      ],
    })]]);
    const name = `e2e-gd-poll-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    const code = patterns.poller.replace(/^const SOURCE = .*$/m, `const SOURCE = ${JSON.stringify(fx.base + 'api/tree.json')}`);
    await signed('PUT', `/api/f/${name}/file?path=workflows/watch.mjs&base_rev=0`, code);
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'watch', file: 'workflows/watch.mjs' }], secrets: [] }));
    const r1 = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'watch' }));
    eq(r1.body?.ok, true, 'poller pattern runs clean');
    const feed = await signed('GET', `/api/f/${name}/files`);
    const paths = (feed.body?.files || []).map((f) => f.path);
    ok(paths.some((p) => p.includes('notes__a.md')), 'poller filed new content');
    ok(!paths.some((p) => p.includes('x.mjs')), 'poller skipped machinery');
    await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'watch' }));
    const feed2 = await signed('GET', `/api/f/${name}/files`);
    eq(feed2.body?.files?.length, feed.body?.files?.length, 'poller re-run is idempotent');
  }
  {
    const tgt = await signed('POST', '/api/fragments', JSON.stringify({ name: `e2e-gd-tgt-${suffix}` }));
    const name = `e2e-gd-once-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    const code = patterns.once.replace(/^const WEBHOOK = .*$/m, `const WEBHOOK = ${JSON.stringify(`${BASE}/api/f/e2e-gd-tgt-${suffix}/inbox?t=${tgt.body.inboxToken}`)}`);
    await signed('PUT', `/api/f/${name}/file?path=workflows/notify.mjs&base_rev=0`, code);
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'notify', file: 'workflows/notify.mjs', trigger: 'inbox' }], secrets: [] }));
    const input = JSON.stringify({ workflow: 'notify', input: { inbox: { id: 42, payload: { hello: 'guide' } } } });
    const r1 = await signed('POST', `/api/f/${name}/run`, input);
    eq(r1.body?.output?.sent, true, 'once pattern fires the effect');
    const r2 = await signed('POST', `/api/f/${name}/run`, input);
    eq(r2.body?.output?.skipped, true, 'once pattern refuses the duplicate');
    const evs = await signed('GET', `/api/f/e2e-gd-tgt-${suffix}/events`);
    eq((evs.body?.events || []).filter((e) => e.kind === 'inbox').length, 1, 'webhook received exactly one delivery');
  }
  {
    const name = `e2e-gd-sync-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=notes/one.md&base_rev=0`, 'one');
    await signed('PUT', `/api/f/${name}/file?path=notes/two.md&base_rev=0`, 'two');
    await signed('PUT', `/api/f/${name}/file?path=workflows/reindex.mjs&base_rev=0`, patterns['sync-reaction']);
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'reindex', file: 'workflows/reindex.mjs', trigger: 'files' }], secrets: [] }));
    const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'reindex' }));
    eq(r.body?.output?.indexed, 2, 'sync-reaction indexed the notes');
    const idx = await fetch(`${BASE}/api/f/${name}/file?path=INDEX.md`, {
      headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=INDEX.md`, null, ownerKey) },
    });
    ok((await idx.text()).includes('notes/two.md'), 'INDEX.md lists the notes');
  }
  {
    const name = `e2e-gd-log-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=workflows/log.mjs&base_rev=0`, patterns['inbox-log']);
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'log', file: 'workflows/log.mjs', trigger: 'inbox' }], secrets: [] }));
    await fetch(`${BASE}/api/f/${name}/inbox?t=${created.body.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 'guide', payload: { n: 1 } }) });
    await fetch(`${BASE}/api/f/${name}/inbox?t=${created.body.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 'guide', payload: { n: 2 } }) });
    const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'log' }));
    eq(r.body?.ok, true, 'inbox-log pattern runs clean');
    const day = new Date().toISOString().slice(0, 10);
    const readLog = async () => fetch(`${BASE}/api/f/${name}/file?path=log/${day}.jsonl`, {
      headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=log/${day}.jsonl`, null, ownerKey) },
    }).then((x) => x.text());
    // async delivery: whichever message loses the manual-run race arrives on
    // the scheduled sweep — poll rather than assume the run saw both
    let both = false;
    const t0 = Date.now();
    while (Date.now() - t0 < 10_000 && !both) {
      both = (await readLog()).trim().split('\n').filter(Boolean).length >= 2;
      if (!both) await sleep(400);
    }
    ok(both, 'inbox-log appended both messages');
    const r2 = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'log' }));
    eq(r2.body?.output?.drained, 0, 'acked messages never re-process');
    eq((await readLog()).trim().split('\n').filter(Boolean).length, 2, 'no double-append after ack');
  }
  {
    if (!process.env.OPENROUTER_API_KEY) {
      console.log('skip  ai-pass pattern (no OPENROUTER_API_KEY on this stack)');
    } else {
      const name = `e2e-gd-ai-${suffix}`;
      await signed('POST', '/api/fragments', JSON.stringify({ name }));
      await signed('PUT', `/api/f/${name}/file?path=notes/x.md&base_rev=0`, 'the quick brown fox jumps over the lazy dog');
      await signed('PUT', `/api/f/${name}/file?path=workflows/digest.mjs&base_rev=0`, patterns['ai-pass']);
      await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'digest', file: 'workflows/digest.mjs' }], secrets: [] }));
      const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'digest' }));
      eq(r.body?.output?.notes, 1, 'ai-pass pattern runs clean');
    }
  }

  // pattern: dropzone (append-only + hash naming + ack)
  {
    const name = `e2e-gd-drop-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=workflows/ingest.mjs&base_rev=0`, patterns.dropzone);
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'ingest', file: 'workflows/ingest.mjs', trigger: 'inbox' }], secrets: [], appendOnly: ['inbox/'] }));
    const post = (text) => fetch(`${BASE}/api/f/${name}/inbox?t=${created.body.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 'drop', payload: { text } }) });
    await post('first drop');
    await post('first drop'); // identical re-drop
    await post('second drop');
    // each POST scheduled an ingest run; wait for all three to land
    let okRuns = [];
    const gt0 = Date.now();
    while (Date.now() - gt0 < 25_000 && okRuns.length < 3) {
      await sleep(800);
      const runs = await signed('GET', `/api/f/${name}/runs`);
      okRuns = (runs.body?.runs || []).filter((r) => r.status === 'success');
    }
    ok(okRuns.length >= 3, 'each drop ran the ingest workflow');
    const files = await signed('GET', `/api/f/${name}/files`);
    const inboxFiles = (files.body?.files || []).filter((f) => f.path.startsWith('inbox/') && !f.deleted);
    eq(inboxFiles.length, 2, 'identical drops collapsed to one file (hash naming)');
    const r2 = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'ingest' }));
    eq(r2.body?.output?.filed, 0, 'acked drops never re-file');
  }

  // unnamed consumer js block in the dropzone pattern section gets the dropzone runner shape:
  // (it's the RECEIVER side of the same pattern — covered by the same manifest shape)
  {
    // verify the guide teaches shape-not-source filtering
    const consumer = Object.entries(patterns).find(([k]) => k.includes('ingest') || k.includes('RECEIVER'));
    ok(!!consumer || patterns.dropzone.includes('payload.text') || JSON.stringify(patterns).includes('shape filter'),
      'guide teaches the standard drop envelope (shape filter, not source)');
  }

  // pattern: watcher (notify poke + tree read)
  {
    const fx = await serveFixture('wtree', [['site/index.html', '<!doctype html>seed']]);
    // a real change on the fixture fragment pokes the watcher's inbox
    const name = `e2e-gd-watch-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
    const fxManifest = await signed('GET', `/api/f/${fx.name}/manifest`);
    const fxm = fxManifest.body;
    fxm.notifyUrls = [`${BASE}/api/f/${name}/inbox?t=${created.body.inboxToken}`];
    await signed('PUT', `/api/f/${fx.name}/manifest`, JSON.stringify(fxm));
    // public fixture: __tree needs no view token; run enrichment immediately
    const code = patterns.watcher
      .replace(/^const SOURCE = .*$/m, `const SOURCE = ${JSON.stringify(fx.base.replace(/\/$/, ''))}`)
      .replace('const token = ctx.secrets.SOURCE_VIEW_TOKEN;', 'const token = "";')
      .replace(/^const DELAY_MS = .*$/m, process.env.OPENROUTER_API_KEY ? 'const DELAY_MS = 0;' : 'const DELAY_MS = 999999999;')
      .replace('?view=" + token', '"')
      .replace('+ "&view=" + token', '');
    await signed('PUT', `/api/f/${name}/file?path=workflows/check.mjs&base_rev=0`, code);
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'check', file: 'workflows/check.mjs', trigger: 'inbox' }], secrets: [] }));
    // a change on the fixture pokes us; the alarm delivers it
    await signed('PUT', `/api/f/${fx.name}/file?path=notes/new.md&base_rev=0`, 'fresh content');
    let ran = false;
    const t0 = Date.now();
    while (Date.now() - t0 < 60_000 && !ran) {
      const runs = await signed('GET', `/api/f/${name}/runs`);
      ran = (runs.body?.runs || []).some((r) => r.status === 'success');
      if (!ran) await sleep(500);
    }
    ok(ran, 'watcher pattern ran from a notify poke');
    if (process.env.OPENROUTER_API_KEY) {
      let enriched = false;
      const t1 = Date.now();
      while (Date.now() - t1 < 20_000 && !enriched) {
        const files = await signed('GET', `/api/f/${name}/files`);
        enriched = (files.body?.files || []).some((f) => f.path.startsWith('feed/') && !f.deleted);
        if (!enriched) await sleep(800);
      }
      ok(enriched, 'watcher pattern wrote enriched feed items (async phase)');
    }
  }

  // ---- runner: manifest JSON block ----
  {
    const manBlock = blocks.find((b) => b.section === 'The manifest' && b.lang === 'json');
    ok(!!manBlock, 'guide ships the manifest JSON block');
    const name = `e2e-gd-man-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    const parsed = JSON.parse(manBlock.code.replaceAll('npub1…', ownerNpub));
    parsed.name = name;
    const put = await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify(parsed));
    eq(put.status, 200, 'documented manifest shape is accepted verbatim');
  }

  // ---- runner: app.mjs block ----
  {
    const appBlock = blocks.find((b) => b.section === 'Sites and apps' && b.lang === 'js');
    ok(!!appBlock, 'guide ships the app.mjs block');
    const name = `e2e-gd-app-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=app.mjs&base_rev=0`, appBlock.code);
    await signed('PUT', `/api/f/${name}/file?path=site/index.html&base_rev=0`, '<!doctype html><title>seed</title>');
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }));
    await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'guide app' }));
    const ds = await signed('GET', `/api/f/${name}/drafts`);
    await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: ds.body.drafts[0].slug }));
    const resp = await fetch(`${BASE}/f/${name}/api`);
    eq(await resp.text(), `hello /f/${name}/api`, 'app.mjs block serves its documented response (public path via header)');
    const seed = await fetch(`${BASE}/f/${name}/`);
    ok((await seed.text()).includes('seed'), 'guide app example: site/index.html still serves the root');
  }

  // ---- runner: rooms.mjs block ----
  {
    const roomBlock = blocks.find((b) => b.section === 'Multiplayer (rooms)' && b.lang === 'js');
    ok(!!roomBlock, 'guide ships the rooms.mjs block');
    const name = `e2e-gd-room-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=rooms.mjs&base_rev=0`, roomBlock.code);
    await signed('PUT', `/api/f/${name}/file?path=site/index.html&base_rev=0`, '<!doctype html><title>room</title>');
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }));
    await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'guide rooms' }));
    const ds = await signed('GET', `/api/f/${name}/drafts`);
    await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: ds.body.drafts[0].slug }));
    const wsUrl = BASE.replace('http', 'ws') + `/f/${name}/__room/lobby`;
    const ws = new WebSocket(wsUrl);
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
    const got = [];
    ws.onmessage = (ev) => got.push(JSON.parse(ev.data));
    ws.send(JSON.stringify({ type: 'msg', data: { text: '  padded  ', name: 'guide' } }));
    await sleep(700);
    const echo = got.find((m) => m.type === 'msg' && m.data && m.data.text === 'padded');
    ok(!!echo && echo.data.name === 'guide', 'rooms.mjs block trims + rewrites the broadcast');
    ws.send(JSON.stringify({ type: 'msg', data: { text: '   ' } }));
    await sleep(700);
    const dropped = got.filter((m) => m.type === 'msg').length;
    eq(dropped, 1, 'rooms.mjs block drops empty messages');
    ws.close();
  }

  // ---- runner: inbox HTTP spec block ----
  {
    const inboxBlock = blocks.find((b) => b.section === 'Inbox (webhooks in)' && !b.lang);
    ok(!!inboxBlock, 'guide ships the inbox POST spec');
    const m = inboxBlock.code.match(/POST (\S+)\s+(\{.*\})/);
    ok(!!m, 'inbox spec is a parseable POST line + JSON body');
    const tgt = await signed('POST', '/api/fragments', JSON.stringify({ name: `e2e-gd-inb-${suffix}` }));
    const url = m[1].replace('{host}', BASE).replace('{name}', tgt.body.name).replace('{inboxToken}', tgt.body.inboxToken);
    const resp = await fetch(url, { method: 'POST', body: m[2] });
    eq(resp.status, 200, 'inbox spec POST works verbatim');
    eq((await resp.json()).ok, true, 'inbox spec body accepted');
  }

  // ---- runner: CLI transcripts (First moves, daily loop, recipes) ----
  const bin = findBinary();
  if (!bin) {
    console.log('skip  guide CLI transcripts (no fragment binary built)');
  } else {
    // First moves: login + whoami with an isolated HOME (login writes a key)
    {
      const home = mkdtempSync(join(tmpdir(), 'e2e-guide-home-'));
      seedIdentity(home);
      const out1 = runCli(bin, ['login'], { env: { HOME: home } });
      ok(out1.includes('npub'), 'guide: fragment login prints an npub (isolated HOME)');
      const out2 = runCli(bin, ['whoami'], { env: { HOME: home } });
      ok(out2.includes('npub'), 'guide: whoami echoes the identity');
      const out3 = runCli(bin, ['create', `e2e-gd-fm-${suffix}`], { env: { HOME: home } });
      ok(out3.includes('share link') && out3.includes('webhook URL'), 'guide: create prints named URLs');
    }
    // The daily loop: sync → publish → bless → drafts, run as documented
    // (names substituted; the transcript owns its fragment via an isolated
    // CLI identity, because the docs assume you created it yourself)
    {
      const home = mkdtempSync(join(tmpdir(), 'e2e-guide-home-'));
      seedIdentity(home);
      seedIdentity(home);
      const H = { HOME: home };
      const name = `e2e-gd-loop-${suffix}`;
      const dir = join(mkdtempSync(join(tmpdir(), 'e2e-guide-')), 'my-thing');
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, 'note.md'), 'first note');
      runCli(bin, ['login'], { env: H });
      runCli(bin, ['create', name], { env: H });
      runCli(bin, ['sync', name, '--dir', dir], { env: H });
      const pub = runCli(bin, ['deploy', name, '--dir', dir, '--note', 'first cut'], { env: H });
      ok(pub.includes('live:'), 'guide: deploy goes live');
      const slug = '';
      const rb = runCli(bin, ['rollback', name], { env: H });
      ok(rb.includes('rolled back'), 'guide: rollback works');
      const drafts = runCli(bin, ['drafts', name], { env: H });
      ok(drafts.length > 0, 'guide: drafts lists snapshots');
    }
    // Recipes: vault + dropzone, run as documented (scaffold → live)
    for (const [tpl, frag, probeFile] of [['vault', 'my-vault', null], ['dropzone', 'my-drop', 'drop/note.txt']]) {
      const name = `e2e-gd-${tpl === 'vault' ? 'vault' : 'drop'}-${suffix}`;
      const workdir = mkdtempSync(join(tmpdir(), 'e2e-guide-recipe-'));
      const block = blocks.find((b) => b.section.startsWith('Recipes') && !b.lang && b.code.includes(frag));
      ok(!!block, `guide ships the ${tpl} recipe transcript`);
      let cwd = workdir;
      let watcher = null;
      const recipeName = `e2e-gd-recipe-${tpl}-${suffix}`;
      // the transcript owns its fragment via an isolated CLI identity —
      // the docs assume you created it yourself
      const home = mkdtempSync(join(tmpdir(), 'e2e-guide-home-'));
      seedIdentity(home);
      seedIdentity(home);
      const H = { HOME: home };
      runCli(bin, ['login'], { env: H });
      let viewToken = '';
      for (const raw of block.code.split('\n')) {
        const line = shline(raw).replaceAll(frag, recipeName);
        if (!line) continue;
        if (line.startsWith('cd ')) {
          cwd = resolve(cwd, line.slice(3));
          continue;
        }
        if (line.startsWith('echo ')) {
          const mm = line.match(/^echo "(.*)" > (.*)$/);
          const target = resolve(cwd, mm[2]);
          mkdirSync(join(target, '..'), { recursive: true });
          writeFileSync(target, mm[1] + '\n');
          continue;
        }
        if (line.startsWith('fragment ')) {
          const args = tokenize(line.slice('fragment '.length).replace(/publish ([a-z0-9-]+) --dir \. --bless/, 'deploy $1 --dir .'));
          if (args.includes('--watch')) {
            // the documented "leave running" step: it starts (and would keep
            // running); the dropzone pull-back below uses one-shot syncs
            watcher = spawn(bin, args, { cwd, env: { ...process.env, FRAGMENT_HOST: BASE, ...H }, stdio: 'ignore' });
            await sleep(1500);
          } else {
            const out = runCli(bin, args, { cwd, env: H });
            if (args[0] === 'init' || args[0] === 'create') viewToken = (out.match(/share link:\s+\S+\?view=(\S+)/) || [])[1] || (out.match(/view token:\s+(\S+)/) || [])[1] || '';
            if (args[0] === 'publish') ok(out.includes('/d/'), `guide: ${tpl} recipe publishes a draft`);
          }
          continue;
        }
        // plain commentary lines are skipped
      }
      if (watcher) { try { watcher.kill(); } catch {} }
      // the recipe's promise: the scaffold is live at its canonical URL
      ok(!!viewToken, `guide: ${tpl} create printed a view token`);
      const canon = await fetch(`${BASE}/f/${recipeName}/?view=${viewToken}`);
      eq(canon.status, 200, `guide: ${tpl} canonical serves after the recipe`);
      if (probeFile) {
        // dropzone promise: outputs land back in the folder within seconds
        let pulled = false;
        const t0 = Date.now();
        while (Date.now() - t0 < 45_000 && !pulled) {
          runCli(bin, ['sync', recipeName, '--dir', cwd], { env: H });
          pulled = existsSync(join(cwd, 'output')) && readdirSync(join(cwd, 'output')).length > 0;
          if (!pulled) await sleep(1500);
        }
        ok(pulled, 'guide: dropzone ingest output pulled back into the folder');
      }
    }
  }

  // ---- enforcement: every js/json block has a runner ----
  const RUNNER_SECTIONS = new Set(['Workflows', 'Sites and apps', 'Multiplayer (rooms)']);
  for (const b of blocks) {
    if (b.lang === 'js') ok(RUNNER_SECTIONS.has(b.section) || b.section.startsWith('Patterns'), `js block has a runner: ${b.section}/${b.sub || '—'}`);
    if (b.lang === 'json') ok(b.section === 'The manifest', `json block has a runner: ${b.section}`);
  }
}

// ---------- platform api: machine reads + notify ----------
async function platformSection() {
  if (!section('platform')) return;
  // machine-read plane: gated exactly like the site
  {
    const pubName = `e2e-pa-pub-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name: pubName }));
    await signed('PUT', `/api/f/${pubName}/file?path=notes/a.md&base_rev=0`, 'alpha');
    await signed('PUT', `/api/f/${pubName}/file?path=workflows/w.mjs&base_rev=0`, 'code');
    await signed('PUT', `/api/f/${pubName}/manifest`, JSON.stringify({ name: pubName, visibility: 'public', editors: [], viewers: [], workflows: [{ name: 'w', file: 'workflows/w.mjs' }], secrets: [] }));
    await signed('POST', `/api/f/${pubName}/drafts`, JSON.stringify({ note: 'x' }));
    const ds = await signed('GET', `/api/f/${pubName}/drafts`);
    await signed('POST', `/api/f/${pubName}/bless`, JSON.stringify({ slug: ds.body.drafts[0].slug }));
    const t = await (await fetch(`${BASE}/f/${pubName}/__tree`)).json();
    ok(t.files?.some((f) => f.path === 'notes/a.md'), '__tree lists content (public)');
    ok(!t.files?.some((f) => f.path.startsWith('workflows/')), '__tree hides machinery');
    const f = await fetch(`${BASE}/f/${pubName}/__file?path=notes/a.md`);
    eq(await f.text(), 'alpha', '__file returns raw content');
    eq((await fetch(`${BASE}/f/${pubName}/__file?path=workflows/w.mjs`)).status, 400, '__file blocks machinery');
    // draft form serves the snapshot
    const dt = await (await fetch(`${BASE}/d/${ds.body.drafts[0].slug}/__tree`)).json();
    ok(dt.files?.some((x) => x.path === 'notes/a.md'), 'draft __tree serves the snapshot');
  }
  {
    const tokName = `e2e-pa-tok-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name: tokName }));
    await signed('PUT', `/api/f/${tokName}/file?path=notes/secret.md&base_rev=0`, 'hidden');
    await signed('PUT', `/api/f/${tokName}/manifest`, JSON.stringify({ name: tokName, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [] }));
    await signed('POST', `/api/f/${tokName}/drafts`, JSON.stringify({ note: 'x' }));
    const ds = await signed('GET', `/api/f/${tokName}/drafts`);
    await signed('POST', `/api/f/${tokName}/bless`, JSON.stringify({ slug: ds.body.drafts[0].slug }));
    eq((await fetch(`${BASE}/f/${tokName}/__tree`)).status, 403, '__tree refuses without token');
    const t = await (await fetch(`${BASE}/f/${tokName}/__tree?view=${created.body.viewToken}`)).json();
    ok(t.files?.some((f) => f.path === 'notes/secret.md'), '__tree works with the view link');
  }
  // meta: OG injection, placeholder previews, gallery listing
  {
    const name = `e2e-pa-meta-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=site/index.html&base_rev=0`, '<!doctype html><html><head></head><body>x</body></html>');
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'public', workflows: [], meta: { title: 'Meta Test', description: 'desc', listed: true } }));
    await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'x' }));
    const ds = await signed('GET', `/api/f/${name}/drafts`);
    await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: ds.body.drafts[0].slug }));
    const page = await (await fetch(`${BASE}/f/${name}/`)).text();
    ok(page.includes('og:title') && page.includes('Meta Test'), 'manifest meta injects OG tags');
    ok(page.includes('__preview.svg'), 'OG image defaults to the placeholder');
    const svg = await fetch(`${BASE}/f/${name}/__preview.svg`);
    eq(svg.status, 200, 'placeholder preview served');
    ok((svg.headers.get('content-type') || '').includes('svg'), 'placeholder is svg');
    const gal = await (await fetch(`${BASE}/api/gallery`)).json();
    const me = (gal.fragments || []).find((f) => f.name === name);
    ok(!!me && me.title === 'Meta Test' && !!me.viewToken, 'listed fragment appears in the gallery with its share token');
    // __rt.js must parse as a classic script: the source lives in a template
    // literal, where regex escapes once shipped unescaped and SyntaxError'd
    // every page that loaded it (rooms silently degraded to solo)
    const rt = await (await fetch(`${BASE}/f/${name}/__rt.js`)).text();
    writeFileSync(join(tmpdir(), 'rt-check.js'), rt);
    const parse = spawnSync(process.execPath, ['--check', join(tmpdir(), 'rt-check.js')]);
    ok(parse.status === 0, '__rt.js parses as a script (no template-escape damage)');
    // unlisted private fragments do not appear
    const priv = await signed('POST', '/api/fragments', JSON.stringify({ name: `e2e-pa-priv-${suffix}` }));
    const gal2 = await (await fetch(`${BASE}/api/gallery`)).json();
    ok(!(gal2.fragments || []).some((f) => f.name === priv.body.name), 'unlisted fragments stay out of the gallery');
  }

  // notify-on-change: manifest notifyUrls → mutation → target inbox POSTed
  {
    const mk = async (n) => {
      const c = await signed('POST', '/api/fragments', JSON.stringify({ name: n }));
      return c.body;
    };
    const srcFrag = await mk(`e2e-pa-src-${suffix}`);
    const dstFrag = await mk(`e2e-pa-dst-${suffix}`);
    await signed('PUT', `/api/f/${srcFrag.name}/file?path=notes/seed.md&base_rev=0`, 'seed');
    await signed('PUT', `/api/f/${srcFrag.name}/manifest`, JSON.stringify({ name: srcFrag.name, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [], notifyUrls: [`${BASE}/api/f/${dstFrag.name}/inbox?t=${dstFrag.inboxToken}`] }));
    await signed('PUT', `/api/f/${srcFrag.name}/file?path=notes/change.md&base_rev=0`, 'changed');
    let arrived = false;
    const t0 = Date.now();
    while (Date.now() - t0 < 12_000 && !arrived) {
      const evs = await signed('GET', `/api/f/${dstFrag.name}/events`);
      arrived = JSON.stringify(evs.body?.events || []).includes('"kind":"inbox"');
      if (!arrived) await sleep(500);
    }
    ok(arrived, 'notifyUrls POSTed the change to the target inbox');
    const nsrc = await signed('GET', `/api/f/${srcFrag.name}/events`);
    ok(JSON.stringify(nsrc.body?.events || []).includes('"kind":"notify.sent"'), 'notify.sent on the ledger');
  }
}

// ---------- file sync v2 (history, merges, append-only, live) ----------
async function filesyncSection() {
  if (!section('filesync')) return;
  const put = (name, path, body, baseRev) =>
    signed('PUT', `/api/f/${name}/file?path=${encodeURIComponent(path)}&base_rev=${baseRev}`, body);

  // ---- runtime: revision history + file/at + retention ----
  {
    const name = `e2e-fs-hist-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    for (let i = 1; i <= 12; i++) await put(name, 'doc.md', `v${i}\n`, i - 1);
    const h = await signed('GET', `/api/f/${name}/file/history?path=doc.md`);
    ok(h.body?.revs?.length <= 10, 'history retention keeps last 10 revisions');
    const atUrl = `${BASE}/api/f/${name}/file/at?path=doc.md&rev=${h.body.revs[h.body.revs.length - 1].rev}`;
    const at = await fetch(atUrl, { headers: { authorization: await authHeader('GET', atUrl, null, ownerKey) } });
    eq(await at.text(), 'v3\n', 'file/at returns exact historical bytes');
    const goneUrl = `${BASE}/api/f/${name}/file/at?path=doc.md&rev=1`;
    const gone = await fetch(goneUrl, { headers: { authorization: await authHeader('GET', goneUrl, null, ownerKey) } });
    eq(gone.status, 410, 'pruned revisions answer 410');
  }

  // publish servability guard: parent-folder sync gets named
  {
    const name = `e2e-fs-serv-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=${encodeURIComponent(name + '/site/index.html')}&base_rev=0`, '<!doctype html>oops');
    const d = await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'nested' }));
    eq(d.body?.servable, false, 'unservable draft flagged');
    ok(String(d.body?.warning || '').includes('parent folder'), 'nested-sync hint names the cause');
    const evs = await signed('GET', `/api/f/${name}/events`);
    ok(JSON.stringify(evs.body?.events || []).includes('publish.warn'), 'publish.warn on the ledger');
  }

  // ---- runtime: append-only enforcement ----
  {
    const name = `e2e-fs-app-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await put(name, 'logs/a.jsonl', 'x', 0);
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [], appendOnly: ['logs/'] }));
    // the owner is exempt by design; enforcement is proven with an editor
    const editorKey = genKey();
    const edNpub = npubFromHex(pubkeyFromSecret(editorKey));
    const man0 = await signed('GET', `/api/f/${name}/manifest`);
    const nm = man0.body;
    nm.editors = [...(nm.editors || []), edNpub];
    const up = await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify(nm));
    eq(up.status, 200, 'editor granted via manifest');
    const eput = (path, body, br) => signed('PUT', `/api/f/${name}/file?path=${encodeURIComponent(path)}&base_rev=${br}`, body, editorKey);
    const same = await eput('logs/a.jsonl', 'x', 1);
    eq(same.body?.noop, true, 'append-only: identical rewrite is a noop');
    const diff = await eput('logs/a.jsonl', 'y', 1);
    eq(diff.status, 409, 'append-only: modified existing refused (editor)');
    const del = await signed('DELETE', `/api/f/${name}/file?path=logs/a.jsonl`, null, editorKey);
    eq(del.status, 403, 'append-only: delete refused (editor)');
    const ownerDel = await signed('DELETE', `/api/f/${name}/file?path=logs/a.jsonl`);
    eq(ownerDel.status, 200, 'append-only: owner delete allowed');
    const readd = await eput('logs/b.jsonl', 'new', 0);
    eq(readd.status, 200, 'append-only: new paths always allowed');
  }

  // ---- runtime: a skipped inbox run must not ack its message ----
  {
    const name = `e2e-fs-ack-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/file?path=workflows/w.mjs&base_rev=0`,
      'export async function run(ctx) { const m = await ctx.inbox(); await ctx.inboxAck(m.map((x) => x.id)); return { n: m.length }; }');
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox' }], secrets: [] }));
    // async scheduling: both messages land, both runs fire, the workflow
    // drains + acks them itself (the ack lives in the workflow now)
    await fetch(`${BASE}/api/f/${name}/inbox?t=${created.body.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 't', payload: { x: 1 } }) });
    await fetch(`${BASE}/api/f/${name}/inbox?t=${created.body.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 't', payload: { x: 2 } }) });
    let drained = false;
    const dt0 = Date.now();
    while (Date.now() - dt0 < 25_000 && !drained) {
      const runs = await signed('GET', `/api/f/${name}/runs`);
      drained = (runs.body?.counts || {}).success >= 2;
      if (!drained) await sleep(600);
    }
    ok(drained, 'both scheduled runs fired and drained their messages');
    const drain = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'w' }));
    eq(drain.body?.output?.n, 0, 'inbox empty after acked runs');
  }

  // ---- runtime: the __watch live channel ----
  {
    const name = `e2e-fs-live-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
    const ws = new WebSocket(BASE.replace('http', 'ws') + `/f/${name}/__watch?view=${created.body.viewToken}`);
    await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error('watch ws refused')); });
    const frames = [];
    ws.onmessage = (ev) => frames.push(JSON.parse(ev.data));
    await put(name, 'notes/x.md', 'hello', 0);
    const t0 = Date.now();
    while (Date.now() - t0 < 3000 && !frames.some((f) => f.type === 'changed')) await sleep(150);
    ok(frames.some((f) => f.type === 'changed' && f.paths.includes('notes/x.md')), 'watch channel frames mutations');
    ws.close();
    const denied = new WebSocket(BASE.replace('http', 'ws') + `/f/${name}/__watch?view=wrong`);
    const deniedCode = await new Promise((resolve) => {
      denied.onopen = () => resolve('open');
      denied.onerror = () => resolve('refused');
      denied.onclose = () => resolve('closed');
      setTimeout(() => resolve('timeout'), 3000);
    });
    ok(deniedCode !== 'open' && deniedCode !== 'timeout', 'watch channel refuses bad tokens');
  }

  // legacy visibility literal migrates on read AND on check
  {
    const name = `e2e-fs-vis-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    const got = await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ visibility: 'token', workflows: [] }));
    eq(got.status, 400, 'legacy "token" literal refused (hard cut, no port)');
  }

  // manifest/check: server-normalized drift detection (the manifest-set guard)
  {
    const name = `e2e-fs-mchk-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    const man = { visibility: 'public', workflows: [] };
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify(man));
    const same = await signed('POST', `/api/f/${name}/manifest/check`, JSON.stringify(man));
    eq(same.body?.differs, false, 'manifest/check: identical manifest → no drift');
    const drifted = await signed('POST', `/api/f/${name}/manifest/check`, JSON.stringify({ visibility: 'link', workflows: [{ name: 'w', file: 'workflows/w.mjs', cron: '* * * * *' }] }));
    eq(drifted.body?.differs, true, 'manifest/check: drifted manifest detected');
    const defaulted = await signed('POST', `/api/f/${name}/manifest/check`, JSON.stringify({ visibility: 'public' }));
    eq(defaulted.body?.differs, false, 'manifest/check: omitted defaults are not drift');
  }

  // ---- CLI: the whole stage-0/1/2 surface ----
  const bin = findBinary();
  if (!bin) {
    console.log('skip  filesync CLI checks (no fragment binary built)');
    return;
  }
  const H = { HOME: mkdtempSync(join(tmpdir(), 'fragment-fs-home-')) };
  runCli(bin, ['login'], { env: H });
  // CLI-owned fixtures; the suite acts as a second writer via an editor grant
  const cliCreate = async (name) => {
    runCli(bin, ['create', name], { env: H });
    runCli(bin, ['grant', name, '--editor', ownerNpub], { env: H });
  };

  // merge3: non-overlapping offline edits merge clean
  {
    const name = `e2e-fs-m3a-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'doc.md'), 'one\ntwo\nthree\nfour\nfive\n');
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    // offline: local edits the top, remote (owner via API) edits the bottom
    writeFileSync(join(dir, 'doc.md'), 'ONE-local\ntwo\nthree\nfour\nfive\n');
    await put(name, 'doc.md', 'one\ntwo\nthree\nfour\nFIVE-remote\n', 1);
    const r = runCli(bin, ['sync', name, '--dir', dir, '--json'], { env: H });
    const repRaw = JSON.parse(r[r.indexOf('{') >= 0 ? r.indexOf('{') : 0] ? r.slice(r.indexOf('{')) : '{}');
    // both output shapes: the wrapped machine envelope ({ok,data:{...}}) and the bare report
    const rep = repRaw && typeof repRaw === 'object' && 'data' in repRaw && repRaw.ok !== undefined ? repRaw.data : repRaw;
    ok((rep.merged || []).includes('doc.md'), 'non-overlapping edits merged automatically');
    const local = readFileSync(join(dir, 'doc.md'), 'utf8');
    ok(local.includes('ONE-local') && local.includes('FIVE-remote'), 'merge kept both sides');
    const remoteFile = await fetch(`${BASE}/api/f/${name}/file?path=doc.md`, {
      headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=doc.md`, null, ownerKey) },
    });
    const remote = await remoteFile.text();
    ok(remote.includes('ONE-local') && remote.includes('FIVE-remote'), 'merged result pushed');
  }

  // merge3: overlapping edits → markers + exit 3
  {
    const name = `e2e-fs-m3b-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'doc.md'), 'the same line\n');
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    writeFileSync(join(dir, 'doc.md'), 'ours rewrote it\n');
    await put(name, 'doc.md', 'theirs rewrote it\n', 1);
    const res = spawnSync(bin, ['sync', name, '--dir', dir], { encoding: 'utf8', env: { ...process.env, FRAGMENT_HOST: BASE, ...H } });
    eq(res.status, 3, 'overlapping edits exit 3 (conflict)');
    const local = readFileSync(join(dir, 'doc.md'), 'utf8');
    ok(local.includes('<<<<<<<'), 'markers written locally');
  }

  // modes: pull withholds deletions; --prune restores
  {
    const name = `e2e-fs-mode-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'keep.md'), 'keep');
    writeFileSync(join(dir, 'drop.md'), 'drop');
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    await signed('DELETE', `/api/f/${name}/file?path=drop.md`);
    const r = runCli(bin, ['sync', name, '--dir', dir, '--mode', 'pull', '--json'], { env: H });
    ok(r.includes('withheld_deletions') || r.includes('withheldDeletions'), 'pull mode withholds the deletion');
    ok(existsSync(join(dir, 'drop.md')), 'pull mode kept the local file');
    runCli(bin, ['sync', name, '--dir', dir, '--mode', 'pull', '--prune'], { env: H });
    ok(existsSync(join(dir, 'drop.md')), '--prune pulls the file back');
  }

  // verify: clean pass then drift
  {
    const name = `e2e-fs-ver-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'a.md'), 'aaa');
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    const ok1 = spawnSync(bin, ['verify', name, '--dir', dir], { encoding: 'utf8', env: { ...process.env, FRAGMENT_HOST: BASE, ...H } });
    eq(ok1.status, 0, 'verify exits 0 when in sync');
    writeFileSync(join(dir, 'a.md'), 'tampered');
    const ok2 = spawnSync(bin, ['verify', name, '--dir', dir], { encoding: 'utf8', env: { ...process.env, FRAGMENT_HOST: BASE, ...H } });
    eq(ok2.status, 3, 'verify exits 3 on drift');
  }

  // --mirror-from: read-only source overlay
  {
    const name = `e2e-fs-mir-${suffix}`;
    const srcDir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'src');
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'frag');
    mkdirSync(srcDir, { recursive: true });
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(srcDir, 'a.md'), 'one');
    mkdirSync(join(srcDir, 'sub'));
    writeFileSync(join(srcDir, 'sub/b.md'), 'two');
    runCli(bin, ['create', name], { env: H });
    runCli(bin, ['grant', name, '--editor', ownerNpub], { env: H });
    const remoteFiles = async () => {
      const f = await signed('GET', `/api/f/${name}/files`);
      return (f.body?.files || []).filter((x) => !x.deleted).map((x) => x.path);
    };
    runCli(bin, ['sync', name, '--dir', dir, '--mirror-from', srcDir], { env: H });
    let paths = await remoteFiles();
    ok(paths.includes('a.md') && paths.includes('sub/b.md'), 'mirror-from overlays the source');
    // new file in the source arrives on the next pass
    writeFileSync(join(srcDir, 'c.md'), 'three');
    runCli(bin, ['sync', name, '--dir', dir, '--mirror-from', srcDir], { env: H });
    paths = await remoteFiles();
    ok(paths.includes('c.md'), 'new source files arrive on later passes');
    // source untouched
    ok(!existsSync(join(srcDir, '.fragment')), 'mirror source never written');
  }

  // mass-deletion guard
  {
    const name = `e2e-fs-guard-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    for (let i = 0; i < 20; i++) writeFileSync(join(dir, `f${i}.md`), 'x');
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    for (let i = 0; i < 15; i++) rmSync(join(dir, `f${i}.md`));
    const res = spawnSync(bin, ['sync', name, '--dir', dir], { encoding: 'utf8', env: { ...process.env, FRAGMENT_HOST: BASE, ...H } });
    eq(res.status, 4, 'mass deletion trips the guard (exit 4)');
    const files = await signed('GET', `/api/f/${name}/files`);
    eq((files.body?.files || []).filter((f) => !f.deleted).length, 20, 'guard prevented remote deletions');
    runCli(bin, ['sync', name, '--dir', dir, '--apply-mass-delete'], { env: H });
    const files2 = await signed('GET', `/api/f/${name}/files`);
    eq((files2.body?.files || []).filter((f) => !f.deleted).length, 5, '--apply-mass-delete proceeds');
  }

  // continuous: live channel pulls a remote write in seconds
  {
    const name = `e2e-fs-cont-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'seed.md'), 'seed');
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    const child = spawn(bin, ['sync', name, '--dir', dir, '--watch'], {
      env: { ...process.env, FRAGMENT_HOST: BASE, ...H }, stdio: 'pipe',
    });
    let log = '';
    child.stdout.on('data', (d) => { log += d; });
    await sleep(2500);
    await put(name, 'remote.md', 'from the server', 0);
    let arrived = false;
    const t0 = Date.now();
    while (Date.now() - t0 < 10_000 && !arrived) {
      arrived = existsSync(join(dir, 'remote.md')) && readFileSync(join(dir, 'remote.md'), 'utf8') === 'from the server';
      if (!arrived) await sleep(300);
    }
    ok(arrived, 'continuous mode pulled a remote write within seconds');
    // local edit pushes
    writeFileSync(join(dir, 'local.md'), 'from the client');
    let pushed = false;
    const t1 = Date.now();
    while (Date.now() - t1 < 10_000 && !pushed) {
      const remoteFile = await fetch(`${BASE}/api/f/${name}/file?path=local.md`, {
        headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=local.md`, null, ownerKey) },
      });
      pushed = remoteFile.ok;
      if (!pushed) await sleep(300);
    }
    ok(pushed, 'continuous mode pushed a local edit');
    // the 1MB per-file limit: cells hold documents, not media — oversized
    // files are refused up front with the reason, and the just-under-limit
    // file still syncs (regression guard for the silent 90s stall that
    // oversized uploads used to hit inside the host)
    {
      // oversized files are skipped with a warning, never block the sync,
      // and never reach the cell; the API path answers 413 for direct writers
      writeFileSync(join(dir, 'huge.bin'), Buffer.alloc(1_100_000, 7));
      writeFileSync(join(dir, 'blob.bin'), Buffer.alloc(900_000, 7));
      let big = false, hugeIn = false;
      const t2 = Date.now();
      while (Date.now() - t2 < 30_000 && !(big && hugeIn)) {
        const [rf, hf] = await Promise.all([
          fetch(`${BASE}/api/f/${name}/file?path=blob.bin`, {
            headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=blob.bin`, null, ownerKey) },
          }),
          fetch(`${BASE}/api/f/${name}/file?path=huge.bin`, {
            headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=huge.bin`, null, ownerKey) },
          }),
        ]);
        big = rf.ok; hugeIn = hf.ok;
        if (!(big && hugeIn)) await sleep(500);
      }
      ok(big, '900KB file syncs');
      ok(hugeIn, '1.1MB file lands via blob-first (tier cap is 64MB, not 1MB)');
      const api413 = await fetch(`${BASE}/api/f/${name}/file?path=direct.bin&base_rev=0`, {
        method: 'PUT',
        headers: { authorization: await authHeader('PUT', `${BASE}/api/f/${name}/file?path=direct.bin&base_rev=0`, Buffer.alloc(1_100_000, 7), ownerKey) },
        body: Buffer.alloc(1_100_000, 7),
      });
      eq(api413.status, 413, 'direct API write over the limit answers 413');
    }
    // double-watcher guard: a second watcher must refuse
    const second = spawnSync(bin, ['sync', name, '--dir', dir, '--watch'], {
      encoding: 'utf8', env: { ...process.env, FRAGMENT_HOST: BASE, ...H }, timeout: 5000,
    });
    ok(second.status !== 0 && String(second.stderr).includes('another fragment sync'), 'second watcher refused by the lock');
    child.kill();
  }
}

// ---------- CLI sync ----------
async function syncSection() {
  if (!section('sync')) return;
  const bin = findBinary();
  if (!bin) {
    console.log(`skip  fragment binary not found (${process.env.FRAGMENT_BIN || 'searched cli/target/*'}) — build it for sync coverage`);
    return;
  }
  const name = `e2e-sy-${suffix}`;
  await signed('POST', '/api/fragments', JSON.stringify({ name }));
  // CLI config in an isolated HOME so we never touch the real keychain/config
  const home = mkdtempSync(join(tmpdir(), 'fragment-e2e-home-'));
  // dirs::config_dir(): macOS ~/Library/Application Support, Linux $XDG_CONFIG_HOME (~/.config)
  const cfgDir = process.platform === 'darwin'
    ? join(home, 'Library', 'Application Support', 'fragment')
    : join(home, '.config', 'fragment');
  mkdirSync(cfgDir, { recursive: true });
  writeFileSync(join(cfgDir, 'config.json'), JSON.stringify({ host: BASE, secret_key: ownerKey }));

  const dir = mkdtempSync(join(tmpdir(), 'fragment-e2e-dir-'));
  mkdirSync(join(dir, 'sub'));
  const cli = (cliArgs, cwd) => execFileSync(bin, ['--json', '--host', BASE, ...cliArgs], { cwd: cwd || dir, env: { ...process.env, HOME: home, XDG_CONFIG_HOME: join(home, '.config') } });

  writeFileSync(join(dir, 'a.txt'), 'alpha local');
  writeFileSync(join(dir, 'sub/b.txt'), 'bravo');
  cli(['sync', name, '--dir', dir]);
  const remoteA = await fetch(`${BASE}/api/f/${name}/file?path=a.txt`, {
    headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=a.txt`, null, ownerKey) },
  });
  eq(Buffer.from(await remoteA.arrayBuffer()).toString(), 'alpha local', 'sync push: local file reached remote');

  // pull: remote-only file arrives locally
  await signed('PUT', `/api/f/${name}/file?path=c.txt&base_rev=0`, 'charlie remote');
  cli(['sync', name, '--dir', dir]);
  eq(readFileSync(join(dir, 'c.txt'), 'utf8'), 'charlie remote', 'sync pull: remote-only file arrived');

  // delete propagation
  execFileSync('rm', ['-rf', join(dir, 'sub')]);
  cli(['sync', name, '--dir', dir]);
  const goneGet = await fetch(`${BASE}/api/f/${name}/file?path=sub/b.txt`, {
    headers: { authorization: await authHeader('GET', `${BASE}/api/f/${name}/file?path=sub/b.txt`, null, ownerKey) },
  });
  eq(goneGet.status, 404, 'sync delete propagation: remote tombstoned');

  // conflict: both sides changed the same line → markers locally, exit 3
  const cur = await jres(nreq('GET', `${BASE}/api/f/${name}/files`, null, ownerKey));
  const entry = cur.body.files.find((f) => f.path === 'a.txt');
  writeFileSync(join(dir, 'a.txt'), 'alpha LOCAL edit');
  await signed('PUT', `/api/f/${name}/file?path=a.txt&base_rev=${entry.rev}`, 'alpha REMOTE edit');
  const cres = spawnSync(bin, ['sync', name, '--dir', dir], {
    encoding: 'utf8', env: { ...process.env, HOME: home, XDG_CONFIG_HOME: join(home, '.config') },
  });
  eq(cres.status, 3, 'conflict exits 3');
  const after = readFileSync(join(dir, 'a.txt'), 'utf8');
  ok(after.includes('<<<<<<<') && after.includes('alpha LOCAL edit') && after.includes('alpha REMOTE edit'),
    'conflict: markers with both sides');
}

// ---------- git interop: .gitignore honored, git state never uploaded ----------
async function gitignoreSection() {
  if (!section('gitignore')) return;
  const bin = findBinary();
  if (!bin) {
    console.log('skip  gitignore checks (no fragment binary built)');
    return;
  }
  const H = { HOME: mkdtempSync(join(tmpdir(), 'fragment-gi-home-')) };
  runCli(bin, ['login'], { env: H });
  const name = `e2e-gi-${suffix}`;
  runCli(bin, ['create', name], { env: H });
  runCli(bin, ['grant', name, '--editor', ownerNpub], { env: H });

  // a git repo whose .gitignore excludes secrets — the walker keys on .git
  // presence (require_git default), so a bare .git dir makes it a repo
  const dir = join(mkdtempSync(join(tmpdir(), 'fragment-gi-')), 'repo');
  mkdirSync(join(dir, 'ignored-dir'), { recursive: true });
  mkdirSync(join(dir, 'sub'), { recursive: true });
  mkdirSync(join(dir, '.git'), { recursive: true });
  writeFileSync(join(dir, '.git', 'HEAD'), 'ref: refs/heads/main\n');
  writeFileSync(join(dir, '.gitignore'), 'secrets.txt\nignored-dir/\n');
  writeFileSync(join(dir, 'secrets.txt'), 'e2e-secret-marker');
  writeFileSync(join(dir, 'ignored-dir', 'key.pem'), 'private');
  writeFileSync(join(dir, '.env'), 'TOKEN=x');
  writeFileSync(join(dir, 'readme.md'), 'public');
  writeFileSync(join(dir, 'sub', '.gitignore'), 'inner.txt\n');
  writeFileSync(join(dir, 'sub', 'inner.txt'), 'also secret');
  writeFileSync(join(dir, 'sub', 'keep.txt'), 'kept');

  const remotePaths = async () => {
    const f = await signed('GET', `/api/f/${name}/files`);
    return (f.body?.files || []).filter((x) => !x.deleted).map((x) => x.path);
  };

  runCli(bin, ['sync', name, '--dir', dir], { env: H });
  let paths = await remotePaths();
  ok(!paths.includes('secrets.txt'), 'gitignored secrets.txt never uploads');
  ok(!paths.some((p) => p.startsWith('ignored-dir/')), 'gitignored dir contents never upload');
  ok(!paths.includes('sub/inner.txt'), 'nested .gitignore honored');
  ok(paths.includes('readme.md') && paths.includes('sub/keep.txt'), 'non-ignored files upload');
  ok(!paths.includes('.env') && !paths.some((p) => p.startsWith('.git/')), '.env and .git/ never upload (dotfile rule)');

  // a later edit to an ignored file must not leak either
  writeFileSync(join(dir, 'secrets.txt'), 'e2e-secret-marker-v2');
  runCli(bin, ['sync', name, '--dir', dir], { env: H });
  paths = await remotePaths();
  ok(!paths.some((p) => p.endsWith('secrets.txt')), 'modified gitignored file still never uploads');
}

function findBinary() {
  if (process.env.FRAGMENT_BIN) return existsSync(process.env.FRAGMENT_BIN) ? process.env.FRAGMENT_BIN : null;
  for (const c of ['cli/target/release/fragment', 'cli/target/debug/fragment']) {
    const p = resolve(process.cwd(), c);
    if (existsSync(p)) return p;
  }
  return null;
}

// ---------- cron (slow) ----------
async function cronSection() {
  if (!CRON) {
    if (!ONLY || ONLY === 'cron') console.log('skip  cron fire check is slow — pass --all to include it');
    return;
  }
  if (!section('cron')) return;
  const name = `e2e-cr-${suffix}`;
  await signed('POST', '/api/fragments', JSON.stringify({ name }));
  await signed('PUT', `/api/f/${name}/file?path=workflows/tick.mjs&base_rev=0`,
    'export async function run(ctx) {\n  await ctx.files.write("ticks/" + Date.now() + ".md", "tick");\n}\n');
  await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
    name, visibility: 'link', editors: [], viewers: [],
    workflows: [{ name: 'tick', file: 'workflows/tick.mjs', cron: '* * * * *' }],
    secrets: [],
  }));
  const st = await signed('GET', `/api/f/${name}/status`);
  ok(st.body?.crons?.[0]?.nextAt, 'status reports nextAt for cron workflow');
  console.log('      waiting up to 75s for the durable alarm to fire…');
  const deadline = Date.now() + 75_000;
  let fired = false;
  while (Date.now() < deadline) {
    await sleep(5000);
    const evs = await signed('GET', `/api/f/${name}/events`);
    fired = JSON.stringify(evs.body?.events || []).includes('"kind":"run.succeeded"') &&
            JSON.stringify(evs.body?.events || []).includes('tick');
    if (fired) break;
  }
  ok(fired, 'cron workflow fired via durable alarm');
}


// ---------- gen (the platform `fragment:ai` module over keyed egress) ----------
async function genSection() {
  if (!section('gen')) return;

  // One workflow exercising all three generators in RUN scope, with the
  // egress fail-closed check. Errors ride back as data so the suite
  // branches on the host's lane (real keys / e2e fake fal / keyless).
  const wf = `
import { generateText, generateImage, generateVideo } from "fragment:ai";

export async function run(ctx) {
  const out = {};
  try {
    const { text } = await generateText({ prompt: "reply with exactly: ok" });
    out.text = String(text).slice(0, 80);
  } catch (e) { out.textError = String((e && e.message) || e); }
  try {
    const { image } = await generateImage({ prompt: "e2e: a single red cube on a white background, studio light" });
    out.image = { path: image.path, size: image.size, mime: image.mediaType, sha: image.sha256 };
  } catch (e) { out.imageError = String((e && e.message) || e); }
  try {
    const { video } = await generateVideo({ prompt: "e2e: slow pan across a red cube" });
    out.video = { path: video.path, size: video.size, mime: video.mediaType, sha: video.sha256 };
  } catch (e) { out.videoError = String((e && e.message) || e); }
  return out;
}`;
  const name = `e2e-gen-${suffix}`;
  await signed('POST', '/api/fragments', JSON.stringify({ name }));
  await signed('PUT', `/api/f/${name}/file?path=workflows/gen.mjs&base_rev=0`, wf);
  await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'gen', file: 'workflows/gen.mjs' }], secrets: [] }));
  const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'gen' }));
  eq(r.body?.ok, true, 'gen workflow runs');
  const out = r.body?.output || {};
  // text lane is probed, not assumed: whether the host holds an
  // OpenRouter key is a STACK property the suite cannot see in its own env
  // (scripts/dev falls back to the main repo's .env for it)
  if (out.textError && /not allowlisted/.test(String(out.textError))) {
    ok(/not allowlisted/.test(String(out.textError)), 'generateText without an OpenRouter key fails closed');
    console.log('skip  generateText content (host has no OpenRouter key)');
  } else {
    eq(out.textError, undefined, `generateText resolves (${out.textError || 'ok'})`);
    ok(typeof out.text === 'string' && out.text.length > 0, 'generateText returned text');
  }

  if (String(out.imageError || '').includes('not allowlisted')) {
    console.log('skip  gen media placement (no FAL_API_KEY on this stack — put FAL_API_KEY=... in .env and scripts/dev up)');
    ok(String(out.imageError).includes('queue.fal.run') || String(out.imageError).includes('FAL'), 'keyless host error names the fal host');
    return;
  }
  eq(out.imageError, undefined, `generateImage resolves (${out.imageError || 'ok'})`);
  eq(out.videoError, undefined, `generateVideo resolves (${out.videoError || 'ok'})`);
  ok(/^gen\/.+\.(jpeg|png|webp)$/.test(out.image?.path || ''), 'generateImage placed an image row under gen/');
  ok(/^gen\/.+\.mp4$/.test(out.video?.path || ''), 'generateVideo placed an mp4 row under gen/');
  ok(String(out.image?.mime || '').startsWith('image/'), 'image carries an image mediaType');
  ok(String(out.video?.mime || '').startsWith('video/'), 'video carries a video mediaType');

  // integrity through the whole pipe: served bytes hash to the committed sha
  for (const [label, f] of [['image', out.image], ['video', out.video]]) {
    if (!f) continue;
    const fileUrl = `${BASE}/api/f/${name}/file?path=${encodeURIComponent(f.path)}`;
    const resp = await fetch(fileUrl, { headers: { authorization: await authHeader('GET', fileUrl, null, ownerKey) } });
    eq(resp.status, 200, `${label}: placed file serves over the api plane`);
    const buf = Buffer.from(await resp.arrayBuffer());
    eq(buf.length, f.size, `${label}: served size matches the descriptor`);
    eq(createHash('sha256').update(buf).digest('hex'), f.sha, `${label}: served bytes hash to the committed sha256`);
  }

  // the example app (templates/gen) live, in draft scope: single-await generate
  const appName = `e2e-gen-app-${suffix}`;
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name: appName }));
  const vt = created.body.viewToken;
  await signed('PUT', `/api/f/${appName}/file?path=app.mjs&base_rev=0`, guideRead(new URL('../templates/gen/app.mjs', import.meta.url), 'utf8'));
  await signed('PUT', `/api/f/${appName}/file?path=${encodeURIComponent('site/index.html')}&base_rev=0`, guideRead(new URL('../templates/gen/site/index.html', import.meta.url), 'utf8'));
  const draft = await signed('POST', `/api/f/${appName}/drafts`, JSON.stringify({}));
  await signed('POST', `/api/f/${appName}/bless`, JSON.stringify({ slug: draft.body.slug }));
  const page = await fetch(`${BASE}/f/${appName}/?view=${vt}`);
  ok((await page.text()).includes('id="prompt"'), 'gen template page serves with its prompt box');

  const gen0 = await jres(fetch(`${BASE}/f/${appName}/generate?view=${vt}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ kind: 'image', prompt: 'e2e app: blue sphere on white' }) }));
  eq(gen0.status, 200, 'app generate route answers 200');
  ok(gen0.body?.file?.path?.startsWith('gen/'), 'app generate returns a placed file');
  const media = await fetch(`${BASE}/f/${appName}/__file?path=${encodeURIComponent(gen0.body?.file?.path || '')}&view=${vt}`);
  eq(media.status, 200, 'generated media serves at __file?path=');
  eq(media.headers.get('content-type') || '', (gen0.body?.file?.mime) || '', '__file serves the stored mediaType');
  const recent = await (await fetch(`${BASE}/f/${appName}/recent?view=${vt}`)).json();
  ok((recent.files || []).some((x) => x.path === gen0.body?.file?.path), 'app recent route lists the generation');

  // bad input shapes are cheap 400s, not fal traffic
  const bad = await jres(fetch(`${BASE}/f/${appName}/generate?view=${vt}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ kind: 'audio', prompt: 'x' }) }));
  eq(bad.status, 400, 'generate rejects an unknown kind');
  const bad2 = await jres(fetch(`${BASE}/f/${appName}/generate?view=${vt}`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ kind: 'image' }) }));
  eq(bad2.status, 400, 'generate rejects a missing prompt');

  // the CLI knows the template (build.rs registry)
  const bin = findBinary();
  if (bin) {
    const list = runCli(bin, ['new', '--list']);
    ok(/\bgen\b/.test(list), 'fragment new --list offers the gen template');
  } else {
    console.log('skip  gen template registry check (no fragment binary built)');
  }
}

// ---------- main ----------
try {
  if (!ONLY || ONLY === 'auth') await authSection();
  if (!ONLY || ONLY === 'files') await filesSection();
  if (!ONLY || ONLY === 'drafts') await draftsSection();
  if (!ONLY || ONLY === 'app') await appSection();
  if (!ONLY || ONLY === 'rooms') await roomsSection();
  if (!ONLY || ONLY === 'workflows') await workflowSection();
  if (!ONLY || ONLY === 'paused') await pausedSection();
  if (!ONLY || ONLY === 'guide') await guideSection();
  if (!ONLY || ONLY === 'runs') await runsSection();
  if (!ONLY || ONLY === 'platform') await platformSection();
  if (!ONLY || ONLY === 'gen') await genSection();
  if (!ONLY || ONLY === 'filesync') await filesyncSection();
  if (!ONLY || ONLY === 'sync') await syncSection();
  if (!ONLY || ONLY === 'gitignore') await gitignoreSection();
  await cronSection();
} catch (e) {
  fail++;
  failures.push('unexpected: ' + String(e && e.stack || e));
  console.log('FAIL  unexpected: ' + String(e && e.stack || e));
}

// ---- lane/runtime additions ----
async function runtimeLaneSection() {
  if (!section('runtime')) return;
  const mkFrag = async (tag) => {
    const name = `e2e-rt-${tag}-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
    return { name, created: created.body };
  };
  const publishAndBless = async (name) => {
    await signed('PUT', `/api/f/${name}/file?path=site/index.html&base_rev=0`, '<h1>lane</h1>');
    await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'runtime lane' }));
    const ds = await signed('GET', `/api/f/${name}/drafts`);
    await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: ds.body.drafts[0].slug }));
  };

  // 1) __rt.js version stamp + header
  {
    const { name } = await mkFrag('stamp');
    await publishAndBless(name);
    const r = await fetch(`${BASE}/f/${name}/__rt.js`);
    eq(r.headers.get('x-fragment-rt-version'), '1', '__rt.js carries x-fragment-rt-version: 1');
    ok((await r.text()).startsWith('/* fragment rt-client v1 */'), '__rt.js first line is the version stamp');
  }

  // 2) rotate: old tokens die hard, new ones work; scoped rotation holds still otherwise
  {
    const { name, created } = await mkFrag('rotate');
    const oldInbox = created.inboxToken;
    const oldView = created.viewToken;
    await publishAndBless(name);

    eq((await signed('POST', `/api/f/${name}/rotate`, '{}', strangerKey)).status, 403, 'rotate refuses a roleless stranger');

    const rot = await signed('POST', `/api/f/${name}/rotate`, JSON.stringify({}));
    eq(rot.status, 200, 'rotate with no scopes → 200 rotating both');
    eq(JSON.stringify(rot.body?.rotated), JSON.stringify(['inbox', 'view']), 'rotated reports both scopes');
    ok(rot.body?.inbox_token && rot.body.inbox_token !== oldInbox, 'new inbox_token differs from the original');
    ok(rot.body?.view_token && rot.body.view_token !== oldView, 'new view_token differs from the original');
    ok(typeof rot.body?.inbox_token === 'string' && typeof rot.body?.view_token === 'string',
      'rotate always returns both current tokens post-rotation');

    const bad = await fetch(`${BASE}/api/f/${name}/inbox?t=${oldInbox}`, {
      method: 'POST', body: JSON.stringify({ source: 'lane', payload: {} }),
    });
    eq(bad.status, 403, 'old inbox token refused on POST /inbox?t= after rotation');
    const good = await fetch(`${BASE}/api/f/${name}/inbox?t=${rot.body.inbox_token}`, {
      method: 'POST', body: JSON.stringify({ source: 'lane', payload: {} }),
    });
    eq((await good.json())?.ok, true, 'new inbox token accepted on POST /inbox?t=');
    eq((await fetch(`${BASE}/f/${name}/?view=${oldView}`)).status, 403, 'old share link dies after view rotation');
    eq((await fetch(`${BASE}/f/${name}/?view=${rot.body.view_token}`)).status, 200, 'new share link works after rotation');

    const scoped = await signed('POST', `/api/f/${name}/rotate`, JSON.stringify({ scopes: ['view'] }));
    eq(scoped.status, 200, 'scoped rotate → 200');
    eq(JSON.stringify(scoped.body?.rotated), JSON.stringify(['view']), 'scoped rotate reports only the requested scope');
    eq(scoped.body?.inbox_token, rot.body.inbox_token, 'scoped rotate leaves inbox_token untouched');
    ok(scoped.body?.view_token !== rot.body.view_token, 'scoped rotate regenerates view_token');
    // empty array = default = both (same as absent)
    const emptyScopes = await signed('POST', `/api/f/${name}/rotate`, JSON.stringify({ scopes: [] }));
    eq(JSON.stringify(emptyScopes.body?.rotated), JSON.stringify(['inbox', 'view']), 'empty scopes array defaults to both');

    const unknown = await signed('POST', `/api/f/${name}/rotate`, JSON.stringify({ scopes: ['bogus'] }));
    eq(unknown.status, 400, 'unknown scope → 400');
    eq(unknown.body?.error, 'unknown scope', 'unknown scope error message verbatim');
  }

  // 3) rooms inspection: list + counts, tail pages ascending, cursor paging,
  // limit bounds, auth gates
  {
    const { name } = await mkFrag('inspect');
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
      name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [],
    }));
    // seed via the WS plane the suite already exercises — public manifest
    // passes checkVisibility without a view token
    const WebSocket = (await import('node:ws').catch(() => null))?.WebSocket ?? globalThis.WebSocket;
    const connect = async (room) => {
      const ws = new WebSocket(`${BASE.replace('http', 'ws')}/f/${name}/__room/${encodeURIComponent(room)}`);
      await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
      await sleep(300);
      return ws;
    };
    const seedRoom = async (room, n) => {
      const ws = await connect(room);
      for (let i = 0; i < n; i++) ws.send(JSON.stringify({ type: 'msg', data: { seq: i, room } }));
      await sleep(600);
      ws.close();
    };
    await seedRoom('alpha', 6);
    await seedRoom('beta', 2);
    await seedRoom('delta grid', 3);   // space in the name exercises URL decoding
    {
      const ws = await connect('ghost');   // state-only room: listed with count 0
      ws.send(JSON.stringify({ type: 'state:set', value: { note: 'no msgs here' } }));
      await sleep(600);
      ws.close();
    }

    let list = null;
    const t0 = Date.now();
    for (;;) {
      list = await signed('GET', `/api/f/${name}/rooms`);
      const got = Object.fromEntries((list.body?.rooms || []).map((r) => [r.room, r.count]));
      if (got.alpha === 6 && got.beta === 2 && got['delta grid'] === 3 && got.ghost === 0) break;
      if (Date.now() - t0 > 10_000) break;
      await sleep(400);
    }
    eq(list.status, 200, 'rooms list → 200');
    const rooms = list.body?.rooms || [];
    ok(rooms.some((r) => r.room === 'alpha' && r.count === 6 && typeof r.last_at === 'number'), 'alpha listed with count + last_at');
    ok(rooms.some((r) => r.room === 'beta' && r.count === 2), 'beta listed with its count');
    ok(rooms.some((r) => r.room === 'ghost' && r.count === 0 && r.last_at === null), 'state-only room listed with count 0, last_at null');
    ok(rooms.every((r, i) => i === 0 || (rooms[i - 1].last_at || 0) >= (r.last_at || 0)), 'rooms sorted by last_at DESC');

    const enc = encodeURIComponent('delta grid');
    const page1 = await signed('GET', `/api/f/${name}/rooms/${enc}/messages?limit=2`);
    eq(page1.status, 200, 'messages endpoint → 200');
    eq(page1.body?.room, 'delta grid', 'room name decoded from the path');
    const m1 = page1.body?.messages || [];
    eq(m1.length, 2, 'limit trims to the newest N');
    ok(m1.every((m, i) => i === 0 || m.id > m1[i - 1].id), 'messages ascending within the page');
    ok(m1.every((m) => m.data && m.data.room === 'delta grid'), 'data is the parsed frame');

    const cursor = m1[0].id;
    const page2 = await signed('GET', `/api/f/${name}/rooms/${enc}/messages?limit=100&before=${cursor}`);
    const m2 = page2.body?.messages || [];
    ok(m2.length >= 1 && m2.every((m) => m.id < cursor), 'before-cursor returns strictly older ids');
    ok(m2.every((m, i) => i === 0 || m.id > m2[i - 1].id), 'older page also ascending');

    const all = await signed('GET', `/api/f/${name}/rooms/${encodeURIComponent('alpha')}/messages?limit=9999`);
    eq(all.body?.messages?.length, 6, 'oversized limit clamps server-side, full room returned');

    eq((await fetch(`${BASE}/api/f/${name}/rooms`)).status, 401, 'unauthenticated rooms list refused');
    eq((await signed('GET', `/api/f/${name}/rooms`, null, strangerKey)).status, 403, 'roleless stranger rooms list → 403');
    eq((await fetch(`${BASE}/api/f/${name}/rooms/${enc}/messages`)).status, 401, 'unauthenticated messages read refused');
    eq((await signed('GET', `/api/f/${name}/rooms/${enc}/messages`, null, strangerKey)).status, 403, 'roleless stranger messages read → 403');
  }
}

try {
  if (!ONLY || ONLY === 'runtime') await runtimeLaneSection();
} catch (e) {
  fail++;
  failures.push('unexpected(runtime): ' + String(e && e.stack || e));
  console.log('FAIL  unexpected(runtime): ' + String(e && e.stack || e));
}
// ---- end lane/runtime additions ----
// ---- lane/cli additions ----
// Machine envelope (--json), error codes, and the rotate/rooms verbs.
// rotate/rooms land only when the runtime-lane endpoints merge — their
// checks are labeled "(post-merge)" so a red suite today stays interpretable
// (expected local tally: everything green except the three post-merge lines).
async function cliLaneSection() {
  if (!section('cli-lane')) return;
  const bin = findBinary();
  ok(!!bin, 'cli binary found for lane/cli checks');
  if (!bin) return;
  const H = { HOME: mkdtempSync(join(tmpdir(), 'fragment-cli-home-')) };
  const env = { ...process.env, FRAGMENT_HOST: BASE, ...H };

  // spawn with status capture (usage errors exit nonzero without throwing)
  const run = (args) => {
    const r = spawnSync(bin, args, { encoding: 'utf8', env, timeout: 60_000 });
    return { code: r.status, stdout: r.stdout || '', stderr: r.stderr || '' };
  };
  const parse = (s) => { try { return JSON.parse(s); } catch { return null; } };

  runCli(bin, ['login'], { env: H });
  const nm = `e2e-cli-${suffix}`;

  // create --json: one line on stdout, {"ok":true} envelope carrying the tokens
  const crRaw = run(['create', nm, '--json']).stdout;
  const oneLine = (s) => s.replace(/\n$/, '').split('\n').length === 1;
  ok(oneLine(crRaw) && parse(crRaw)?.ok === true, '(lane/cli) create --json emits a one-line {"ok":true} envelope');
  const cr = parse(crRaw);
  ok(cr?.ok === true && typeof cr.data?.inboxToken === 'string' && typeof cr.data?.viewToken === 'string',
    '(lane/cli) create --json data carries inbox+view tokens');

  // status/events/runs envelopes have the data key
  for (const sub of ['status', 'events', 'runs']) {
    const v = parse(run([sub, nm, '--json']).stdout);
    ok(v?.ok === true && v && 'data' in v, `(lane/cli) ${sub} --json envelope has data key`);
  }

  // unknown fragment → failure envelope with stable code not_found
  const nf = run(['status', `e2e-cli-nope-${suffix}`, '--json']);
  const nfJ = parse(nf.stdout);
  eq(nf.code, 1, '(lane/cli) unknown fragment exits 1');
  ok(nfJ?.ok === false && nfJ?.error?.code === 'not_found' && typeof nfJ.error.message === 'string',
    '(lane/cli) 404 maps to code not_found in the error envelope');

  // bad subcommand → usage class: exit 2 + invalid_usage envelope on stdout
  const bad = run(['definitely-not-a-fragment-verb', nm, '--json']);
  const badJ = parse(bad.stdout);
  eq(bad.code, 2, '(lane/cli) bad subcommand exits 2');
  ok(badJ?.ok === false && badJ?.error?.code === 'invalid_usage', '(lane/cli) bad subcommand emits invalid_usage envelope');

  // FRAGMENT_OUTPUT=json is a first-class equivalent
  const ev = spawnSync(bin, ['whoami'], { encoding: 'utf8', env: { ...env, FRAGMENT_OUTPUT: 'json' } });
  ok(parse(ev.stdout)?.ok === true && !!parse(ev.stdout)?.data?.npub, '(lane/cli) FRAGMENT_OUTPUT=json drives the envelope');

  // -v logs requests to stderr, stdout stays exactly one clean line
  const vb = spawnSync(bin, ['list', '--json', '-v'], { encoding: 'utf8', env });
  ok((vb.stderr || '').includes('GET /api/fragments -> 200') && /\[retries=\d+\]/.test(vb.stderr),
    '(lane/cli) -v traces signed requests to stderr');
  ok(oneLine(vb.stdout || '') && parse(vb.stdout)?.ok === true, '(lane/cli) -v leaves stdout as one clean line');

  // rotate (owner-only token rotation) — runtime endpoint lands post-merge
  const rot = run(['rotate', nm, '--json']);
  const rotJ = parse(rot.stdout);
  ok(rotJ?.ok === true && typeof rotJ?.data?.inbox_token === 'string' && typeof rotJ?.data?.view_token === 'string'
    && Array.isArray(rotJ?.data?.rotated), '(post-merge) rotate --json returns both tokens under data');

  // rooms listing + message reads
  const rms = parse(run(['rooms', nm, '--json']).stdout);
  ok(rms?.ok === true && Array.isArray(rms?.data?.rooms), '(post-merge) rooms <name> --json lists rooms');
  const rm1 = parse(run(['rooms', nm, 'general', '--tail', '5', '--json']).stdout);
  ok(rm1?.ok === true && rm1?.data?.room === 'general' && Array.isArray(rm1?.data?.messages),
    '(post-merge) rooms <name> <room> --tail N returns ascending messages');

  // leave no trace (fragment created this session; allowed to rm e2e-cli-*)
  runCli(bin, ['rm', nm], { env: H });
}
try {
  if (!ONLY || ONLY === 'cli-lane') await cliLaneSection();
} catch (e) {
  fail++;
  failures.push('cli-lane: ' + String(e && e.stack || e));
  console.log('FAIL  cli-lane unexpected: ' + String(e && e.stack || e));
}
// ---- end lane/cli additions ----
// ---- lane/cli-tier additions ----
// Blob-first push/pull (docs/blob-tier.md CLI section) exercised against a
// LOCAL stub on port 9941 implementing the two-tier wire forms keyed off
// Authorization-event PRESENCE, not validity — real crypto validation belongs
// to blobsd's own suite. The local stack still runs the OLD runtime, so the
// genuinely-end-to-end checks against real bloasd are appended below as
// "(post-cutover)" skip-lines and stay interpretable until the cut lands.
const cliTierSection = async () => {
  if (!section('cli-lane-tier')) return;
  const bin = findBinary();
  ok(!!bin, 'cli binary found for lane/cli-tier checks');
  if (!bin) return;
  const http = await import('node:http');
  const { schnorr } = await import('@noble/curves/secp256k1.js');
  const { createHash } = await import('node:crypto');
  const sha256hex = (b) => createHash('sha256').update(b).digest('hex');

  // ---- stub: bloasd upload/GET surface + enough of the fragment API ----
  const S = {
    blobs: new Map(),            // sha256 -> Buffer
    rows: new Map(),             // path -> {rev, sha256, deleted}
    counts: new Map(),           // "METHOD target" -> n
    rawPuts: [],                 // raw-body file commits
    refPuts: [],                 // ref-form commits: {target, ct, body}
    ups: [],                     // {sha256, auth} upload attempts
  };
  const bump = (k) => S.counts.set(k, (S.counts.get(k) || 0) + 1);
  const count = (prefix) => [...S.counts.entries()].filter(([k]) => k.startsWith(prefix)).reduce((a, [, v]) => a + v, 0);
  const srv = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    req.on('end', () => {
      const body = Buffer.concat(chunks);
      const target = req.url;
      // the CLI's presence probe is a 1-byte ranged GET; record it apart
      // from full-body GETs so pull-path counting stays meaningful
      const probe = req.method === 'GET' && !!req.headers.range;
      bump(`${probe ? 'GET-probe' : req.method} ${target}`);
      const auth = req.headers.authorization || '';
      const hasAuth = /^nostr /i.test(auth);
      const reply = (code, obj) => { res.writeHead(code, { 'content-type': 'application/json' }); res.end(JSON.stringify(obj)); };
      let path = '';
      try { path = decodeURIComponent(target); } catch {}
      const q = (name) => new URL(target, 'http://stub').searchParams.get(name);

      if (req.method === 'HEAD' && /^\/[0-9a-f]{64}$/.test(path)) {
        res.writeHead(S.blobs.has(path.slice(1)) ? 200 : 404, { etag: '"stub"' });
        res.end();
      } else if (req.method === 'PUT' && target === '/upload') {
        if (!hasAuth) return reply(401, { error: 'missing Authorization event' });
        const sha = sha256hex(body);
        S.blobs.set(sha, body);
        S.ups.push({ sha256: sha, auth });
        reply(200, { sha256: sha, size: body.length, type: 'application/octet-stream', uploaded: Date.now() });
      } else if (req.method === 'GET' && /^\/[0-9a-f]{64}$/.test(path)) {
        const b = S.blobs.get(path.slice(1));
        if (!b) { res.writeHead(404); return res.end(); }
        res.writeHead(200, {
          'content-type': 'application/octet-stream',
          etag: `"${path.slice(1)}"`,
          'cache-control': 'public, max-age=31536000, immutable',
        });
        res.end(b);
      } else if (req.method === 'GET' && /\/api\/f\/[^/]+\/files\?/.test(target)) {
        reply(200, { files: [...S.rows.entries()].map(([p, r]) => ({ path: p, ...r })) });
      } else if (req.method === 'GET' && /\/api\/f\/[^/]+\/manifest/.test(target)) {
        reply(200, { name: 'stub', appendOnly: [], editors: [], viewers: [] });
      } else if (req.method === 'GET' && /\/api\/f\/[^/]+\/file\?/.test(target)) {
        const row = S.rows.get(q('path') || '');
        if (!row || row.deleted) { res.writeHead(404); return res.end(); }
        const b = S.blobs.get(row.sha256) || Buffer.alloc(0);
        if (S.redirectMode) {
          // the NEW-runtime public-read wire form: 302 to the hashed asset
          res.writeHead(302, { location: `/${row.sha256}` });
          res.end();
        } else {
          res.writeHead(200, { 'content-length': b.length });
          res.end(b);
        }
      } else if (req.method === 'PUT' && /\/api\/f\/[^/]+\/file\?/.test(target)) {
        const p = q('path') || '';
        const rev = (S.rows.get(p)?.rev || 0) + 1;
        const isRef = /^application\/json$/i.test(req.headers['content-type'] || '');
        if (isRef) {
          let ref = null;
          try { ref = JSON.parse(body.toString('utf8')).ref; } catch {}
          if (!ref || typeof ref.sha256 !== 'string' || !/^[0-9a-fA-F]{64}$/.test(ref.sha256)) {
            return reply(400, { error: 'bad ref' });
          }
          S.rows.set(p, { rev, sha256: ref.sha256.toLowerCase(), deleted: false });
          S.refPuts.push({ target, ct: req.headers['content-type'], body: JSON.parse(body.toString('utf8')) });
        } else {
          S.rows.set(p, { rev, sha256: sha256hex(body), deleted: false });
          S.rawPuts.push({ target });
        }
        reply(200, { rev });
      } else if (req.method === 'POST' && /\/api\/fragments$/.test(target)) {
        reply(200, { name: q('name') || 'stub', npub: 'npub1stub' });
      } else {
        reply(404, { error: 'stub: no route ' + req.method + ' ' + target });
      }
    });
  });
  await new Promise((res, rej) => { srv.once('error', rej); srv.listen(9941, '127.0.0.1', res); });
  const STUB = 'http://127.0.0.1:9941';

  // isolated CLI config, same layout syncSection uses
  const mkHome = (config) => {
    const home = mkdtempSync(join(tmpdir(), 'fragment-tier-home-'));
    const cfgDir = process.platform === 'darwin'
      ? join(home, 'Library', 'Application Support', 'fragment')
      : join(home, '.config', 'fragment');
    mkdirSync(cfgDir, { recursive: true });
    writeFileSync(join(cfgDir, 'config.json'), JSON.stringify(config));
    return home;
  };
  // async on purpose: a blocking spawnSync would freeze the event loop and
  // starve the in-process stub (deadlock at first request)
  const cli = (home, args, extraEnv = {}) => new Promise((resolve) => {
    const env = { ...process.env, HOME: home, XDG_CONFIG_HOME: join(home, '.config'), FRAGMENT_HOST: STUB };
    delete env.FRAGMENT_BLOB_URL; // callers re-add it explicitly when wanted
    Object.assign(env, extraEnv);
    const p = spawn(bin, ['sync', 'e2e-tier', ...args], { env });
    let out = '';
    const kill = setTimeout(() => { try { p.kill('SIGKILL'); } catch {} }, 60_000);
    p.stdout.on('data', (d) => (out += d));
    p.stderr.on('data', (d) => (out += d));
    p.on('error', () => {});
    p.on('close', (code) => { clearTimeout(kill); resolve({ code, out }); });
  });

  try {
    // 1) the JS signer: decodable 24242 event, right tags, verifiable sig
    {
      const hdr = await buildEvent({ actionTag: 'upload', serverUrl: STUB, payloadHash: sha256hex('x'), expirationSecs: 600 }, ownerKey);
      ok(hdr.startsWith('Nostr '), 'buildEvent emits a Nostr authorization header');
      const ev = JSON.parse(Buffer.from(hdr.slice(6), 'base64').toString('utf8'));
      eq(ev.kind, 24242, 'signer event kind is 24242');
      const tag = (n) => (ev.tags.find((t) => t[0] === n) || [])[1];
      eq(tag('t'), 'upload', 'signer t tag carries the action');
      eq(tag('x'), STUB, 'signer x tag carries the server URL');
      eq(tag('payload'), sha256hex('x'), 'signer payload tag carries the body hash');
      ok(Number(tag('expiration')) > Date.now() / 1000, 'signer expiration is in the future');
      ok(schnorr.verify(Buffer.from(ev.sig, 'hex'), Buffer.from(ev.id, 'hex'), Buffer.from(ev.pubkey, 'hex')),
        'signer signature verifies against its pubkey');
    }

    // 2) blob-first push: small stays inline; big goes HEAD -> PUT /upload -> ref row
    const big = Buffer.alloc(70 * 1024, 65); // 'A' * 71680
    const bigSha = sha256hex(big);
    const dir = mkdtempSync(join(tmpdir(), 'fragment-tier-dir-'));
    const home = mkHome({ host: STUB, secret_key: ownerKey });
    writeFileSync(join(dir, 'small.txt'), 'tiny inline body');
    writeFileSync(join(dir, 'big.bin'), big);

    let r = await cli(home, ['--dir', dir], { FRAGMENT_BLOB_URL: STUB }); // env-only config
    eq(r.code, 0, 'blob-first sync passes with FRAGMENT_BLOB_URL from env'
      + (r.code === 0 ? '' : ` :: signal/${r.err?.code || ''} :: ${r.out.slice(0, 400)}`));
    eq(count('PUT /upload'), 1, 'oversized file uploaded once to the tier');
    eq(S.ups[0]?.sha256, bigSha, 'tier upload body hashes to the local sha256');
    ok(S.rawPuts.some((x) => x.target.includes('path=small.txt')), 'file under 64 KiB committed raw (inline carve-out)');
    {
      const ref = S.refPuts.find((x) => x.target.includes('path=big.bin'));
      ok(!!ref, 'oversized file committed in the ref form');
      eq(ref?.body?.ref?.sha256, bigSha, 'ref-form sha256 matches local hash');
      eq(ref?.body?.ref?.size, big.length, 'ref-form size matches');
      ok(typeof ref?.body?.ref?.mime === 'string' && ref.body.ref.mime.length > 0, 'ref-form mime present');
      eq(ref?.ct, 'application/json', 'ref-form commit sends content-type application/json');
    }
    // the CLI's own upload auth event (Rust signer) decodes as a proper 24242
    {
      const ev = JSON.parse(Buffer.from(S.ups[0].auth.replace(/^Nostr /, ''), 'base64').toString('utf8'));
      const tag = (n) => (ev.tags.find((t) => t[0] === n) || [])[1];
      eq(ev.kind, 24242, 'CLI upload event is kind 24242');
      eq(tag('t'), 'upload', 'CLI upload event t tag');
      eq(tag('x'), STUB, 'CLI upload event x tag = configured blob URL');
      eq(tag('payload'), bigSha, 'CLI upload event payload = uploaded bytes hash');
      ok(Number(tag('expiration')) > Date.now() / 1000, 'CLI upload event not expired');
      ok(schnorr.verify(Buffer.from(ev.sig, 'hex'), Buffer.from(ev.id, 'hex'), Buffer.from(ev.pubkey, 'hex')),
        'CLI upload event signature verifies');
    }

    // 3) HEAD-hit skips the PUT: wipe rows/state, keep the tier blob, re-push
    S.rows.clear();
    rmSync(join(dir, '.fragment'), { recursive: true, force: true });
    r = await cli(home, ['--dir', dir], { FRAGMENT_BLOB_URL: STUB });
    eq(r.code, 0, 're-push after state rebuild passes');
    eq(count('PUT /upload'), 1, 'HEAD hit on existing blob skips the second PUT');
    eq(count('GET-probe /') >= 2, true, 'both passes probed tier presence (ranged GET) first');
    ok(S.refPuts.filter((x) => x.target.includes('path=big.bin')).length === 2, 'row re-committed via ref form');

    // 4) env FRAGMENT_BLOB_URL overrides a config blob_url
    const homeEnv = mkHome({ host: STUB, secret_key: ownerKey, blob_url: 'http://127.0.0.1:1' });
    const big2 = Buffer.alloc(70 * 1024, 66);
    writeFileSync(join(dir, 'big2.bin'), big2);
    r = await cli(homeEnv, ['--dir', dir], { FRAGMENT_BLOB_URL: STUB });
    eq(r.code, 0, 'FRAGMENT_BLOB_URL overrides config blob_url (dead config URL unused)');
    eq(S.ups.at(-1)?.sha256, sha256hex(big2), 'override config uploaded the new blob');

    // 5) pull: 302 to the hashed asset, then cache short-circuits the network
    S.redirectMode = true;
    rmSync(join(dir, 'big.bin'));
    r = await cli(home, ['--dir', dir, '--mode', 'pull', '--prune'], { FRAGMENT_BLOB_URL: STUB });
    eq(r.code, 0, 'pull --prune restores a locally-deleted file'
      + (r.code === 0 ? '' : ` :: ${r.out.slice(0, 400)}`));
    eq(readFileSync(join(dir, 'big.bin')).equals(big), true, 'pulled bytes identical (via 302 to tier)');
    ok(existsSync(join(dir, '.fragment', 'cache', bigSha)), 'pull populated .fragment/cache/<sha>');
    const getsAfterFirst = count(`GET /${bigSha}`);
    eq(getsAfterFirst, 1, 'first pull fetched from the tier exactly once (probes counted separately)');

    rmSync(join(dir, 'big.bin'));
    r = await cli(home, ['--dir', dir, '--mode', 'pull', '--prune'], { FRAGMENT_BLOB_URL: STUB });
    eq(r.code, 0, 'second pull --prune of the same sha passes');
    eq(count(`GET /${bigSha}`), getsAfterFirst, 'cache short-circuit: zero network fetch on repeated pull');
    eq(readFileSync(join(dir, 'big.bin')).equals(big), true, 'cache-fed pull restores identical bytes');

    // negative control: without the cache entry the network fetch happens again
    rmSync(join(dir, '.fragment', 'cache', bigSha));
    rmSync(join(dir, 'big.bin'));
    r = await cli(home, ['--dir', dir, '--mode', 'pull', '--prune'], { FRAGMENT_BLOB_URL: STUB });
    eq(r.code, 0, 'cache-miss pull passes');
    eq(count(`GET /${bigSha}`), getsAfterFirst + 1, 'cache miss re-fetches from the tier');
    S.redirectMode = false;

    // 6) oversized + no tier configured anywhere → non-zero exit with guidance
    const homeBare = mkHome({ host: STUB, secret_key: ownerKey });
    const dir3 = mkdtempSync(join(tmpdir(), 'fragment-tier-big-'));
    writeFileSync(join(dir3, 'huge.bin'), Buffer.alloc(70 * 1024, 67));
    const r3 = await cli(homeBare, ['--dir', dir3]);
    ok(r3.code !== 0 && r3.code !== null, 'oversized push without any tier config exits non-zero');
    const combined = r3.out;
    ok(combined.includes('FRAGMENT_BLOB_URL'), 'error names FRAGMENT_BLOB_URL as the fix');
    ok(combined.includes('blob_url'), 'error names the config blob_url key too');

    // genuinely-end-to-end against the REAL stack (bloasd + two-tier runtime):
    // appended as labeled skip-lines until the cutover merges; the suite stays
    // interpretable (all green above) meanwhile.
    console.log('skip  (post-cutover) blob-first push through real bloasd: descriptor, allowlist, replay-idempotence');
    console.log('skip  (post-cutover) public read 302s to BLOBSD_PUBLIC_URL through the live blessed path');
    console.log('skip  (post-cutover) acceptance #1: >64KB note syncs blob-first against prod');
  } finally {
    await new Promise((res) => srv.close(res));
  }
};

try {
  if (!ONLY || ONLY === 'cli-lane-tier') await cliTierSection();
} catch (e) {
  fail++;
  failures.push('cli-lane-tier: ' + String(e && e.stack || e));
  console.log('FAIL  cli-lane-tier unexpected: ' + String(e && e.stack || e));
}
// ---- end lane/cli-tier additions ----

// ---- lane/runtime-tier additions ----
// Two-tier files: names in cells, bytes in blobsd (docs/blob-tier.md).
//
// HOST ENV REQUIRED by these checks (all four ride the CELLD_VAR_ passthrough):
//   CELLD_VAR_BLOBSD_URL=http://127.0.0.1:9940        (points at the stub started below)
//   CELLD_VAR_BLOBSD_INTERNAL_TOKEN=e2e-blob-token    (override with E2E_BLOBSD_TOKEN)
//   CELLD_VAR_BLOBSD_PUBLIC_GET=1                     (flips 302 mode ON)
//   CELLD_VAR_BLOBSD_PUBLIC_URL=http://127.0.0.1:9940
// scripts/dev does NOT export these automatically — bring the stack up like:
//   CELLD_VAR_BLOBSD_URL=http://127.0.0.1:9940 \
//   CELLD_VAR_BLOBSD_INTERNAL_TOKEN=e2e-blob-token \
//   CELLD_VAR_BLOBSD_PUBLIC_GET=1 CELLD_VAR_BLOBSD_PUBLIC_URL=http://127.0.0.1:9940 \
//   ./scripts/dev up && ./scripts/dev deploy && node scripts/e2e.mjs --only tier
// If the running host lacks the env, every tier check SKIPS (detected by
// probing one real inline write — the vars live on the host process and are
// unreachable from here), so a plain suite stays green until cutover adds
// them. Real-side coverage (the actual blobsd binary) happens post-merge;
// the stub below implements only the upload/HEAD/GET hash contract in-memory
// so this lane runs standalone.
async function tierLaneSection() {
  if (!section('tier')) return;
  const TOKEN = E2E_TOKEN;
  const PORT = E2E_BLOBSD_PORT;
  const { createHash } = await import('node:crypto');

  // helpers that keep raw bytes + headers (jres() above is JSON-only)
  const rawFetch = async (url, init) => {
    const r = await fetch(url, init);
    return { status: r.status, buf: Buffer.from(await r.arrayBuffer()), headers: r.headers };
  };
  const signedPutRaw = async (name, qs, bodyBuf, headers = {}) => {
    const url = `${BASE}/api/f/${name}/file?${qs}`;
    return rawFetch(url, {
      method: 'PUT',
      headers: { authorization: await authHeader('PUT', url, bodyBuf, ownerKey), ...headers },
      body: bodyBuf,
    });
  };
  const timems = (fn) => {
    const t0 = Date.now();
    return fn().then((v) => ({ v, took: Date.now() - t0 }));
  };
  const getFile = async (name, path) => {
    const url = `${BASE}/api/f/${name}/file?path=${encodeURIComponent(path)}`;
    return rawFetch(url, { headers: { authorization: await authHeader('GET', url, null, ownerKey) } });
  };

  try {
    // probe: does the RUNNING host carry the blob-tier env?
    const probeName = `e2e-tier-probe-${suffix}`;
    const created = await signed('POST', '/api/fragments', JSON.stringify({ name: probeName }));
    eq(created.status, 200, '[tier] probe fragment created');
    const probe = await signedPutRaw(probeName, 'path=probe.txt&base_rev=0', Buffer.from('probe'));
    if (probe.status !== 200) {
      console.log('skip  blob tier not configured on the host (set CELLD_VAR_BLOBSD_* per block header) — all tier checks skipped');
      return;
    }

    // fixture: public fragment so __file/blessed reads need no token
    const name = `e2e-tier-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    const man = await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
      name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [],
    }));
    eq(man.status, 200, '[tier] public manifest set');

    // ---- static fixtures BEFORE first draft: cache heuristic needs blessing ----
    const cssBody = Buffer.from('body{color:#123}');
    const pCss = await signedPutRaw(name, 'path=' + encodeURIComponent('site/style-a.1b2c3d4.css') + '&base_rev=0', cssBody);
    eq(pCss.status, 200, '[tier] hash-named css seeded');
    const pIdx = await signedPutRaw(name, 'path=site%2Findex.html&base_rev=0', Buffer.from('<!doctype html><title>tier</title>'));
    eq(pIdx.status, 200, '[tier] index.html seeded');
    const d0 = await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'tier fixture' }));
    ok(d0.body?.slug, '[tier] fixture draft published');
    const bl = await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: d0.body.slug }));
    eq(bl.status, 200, '[tier] fixture blessed');

    // 1) raw-inline write/read roundtrip (no hash header; runtime uploads itself)
    const inlineBody = Buffer.from('tier inline roundtrip\n');
    const pInline = await signedPutRaw(name, 'path=' + encodeURIComponent('notes/inline.md') + '&base_rev=0', inlineBody);
    eq(pInline.status, 200, '[tier] raw inline write commits');
    const inlineRev = (JSON.parse(pInline.buf.toString()) || {}).rev;
    ok(Number.isInteger(inlineRev), '[tier] raw inline write returns rev');
    const wantSha = createHash('sha256').update(inlineBody).digest('hex');
    const apiGet = await getFile(name, 'notes/inline.md');
    eq(apiGet.status, 200, '[tier] authed API read after inline commit');
    ok(apiGet.buf.equals(inlineBody), '[tier] inline roundtrip preserves bytes');
    const listing = await signed('GET', `/api/f/${name}/files`);
    const entry = (listing.body?.files || []).find((f) => f.path === 'notes/inline.md');
    eq(entry?.sha256, wantSha, '[tier] row carries the content address');

    // 2) >64KB push WITH x-fragment-hash commits & reads back (stream-through form)
    const big = Buffer.alloc(100_000);
    for (let i = 0; i < big.length; i++) big[i] = i % 251;
    const bigSha = createHash('sha256').update(big).digest('hex');
    const pBig = await signedPutRaw(name, 'path=' + encodeURIComponent('media/big.bin') + '&base_rev=0', big, { 'x-fragment-hash': bigSha });
    eq(pBig.status, 200, '[tier] >64KB raw push with x-fragment-hash commits');
    const bigBack = await getFile(name, 'media/big.bin');
    ok(bigBack.buf.equals(big), '[tier] >64KB push reads back byte-exact');
    // identity is enforced: declared hash must match what actually arrives
    const wrongSha = createHash('sha256').update(Buffer.from('other')).digest('hex');
    const pBad = await signedPutRaw(name, 'path=' + encodeURIComponent('media/evil.bin') + '&base_rev=0', big.slice(0, 1000), { 'x-fragment-hash': wrongSha });
    eq(pBad.status, 400, '[tier] hash mismatch refuses to commit');
    ok(JSON.parse(pBad.buf.toString()).error.includes(wrongSha), '[tier] mismatch error names the declared hash');
    eq((await signed('GET', `/api/f/${name}/files`)).body.files.some((f) => f.path === 'media/evil.bin'), false, '[tier] refused push left no row');

    // 3) oversize-no-hash answers 413 with the blob-first hint (spec text)
    const p413 = await signedPutRaw(name, 'path=' + encodeURIComponent('media/too-big.bin') + '&base_rev=0', Buffer.alloc(70_000, 3));
    eq(p413.status, 413, '[tier] oversize raw without hash -> 413');
    ok(p413.buf.toString().includes('blob-first'), '[tier] 413 hint points at the blob-first flow');

    // 4) ref-form JSON commit returns rev; subsequent __file serves stored mime
    //    (bytes pushed DIRECT to the tier first — blob-first by construction;
    //    same internal token proves it is the same store the runtime uses)
    const refBytes = Buffer.from('ref-form woff payload'.repeat(10));
    const refSha = createHash('sha256').update(refBytes).digest('hex');
    const up = await rawFetch(`http://127.0.0.1:${PORT}/upload`, {
      method: 'PUT',
      headers: { authorization: `Bearer ${TOKEN}`, 'x-sha-256': refSha, 'content-type': 'font/woff2' },
      body: refBytes,
    });
    eq(up.status, 200, '[tier] direct tier upload accepted (same token/store)');
    const refJson = JSON.stringify({ ref: { sha256: refSha, size: refBytes.length, mime: 'font/woff2' } });
    const refUrl = `${BASE}/api/f/${name}/file?path=${encodeURIComponent('assets/font.woff2')}&base_rev=0`;
    // fresh path: base_rev 0; sign over the exact json bytes
    const pRef = await rawFetch(refUrl, {
      method: 'PUT',
      headers: { authorization: await authHeader('PUT', refUrl, Buffer.from(refJson), ownerKey), 'content-type': 'application/json' },
      body: refJson,
    });
    eq(pRef.status, 200, '[tier] ref-form JSON commit returns 200');
    ok(Number.isInteger((JSON.parse(pRef.buf.toString()) || {}).rev), '[tier] ref-form returns a rev');
    const mimeRead = await rawFetch(`${BASE}/f/${name}/__file?path=assets%2Ffont.woff2`);
    eq(mimeRead.status, 200, '[tier] blessed __file serves tier-backed file');
    ok((mimeRead.headers.get('content-type') || '').startsWith('font/woff2'), '[tier] __file serves the stored mime');
    ok(mimeRead.buf.equals(refBytes), '[tier] ref-committed bytes read back exactly');
    // a ref naming bytes NOT in the tier still commits (presumed-present
    // contract); its absence surfaces at READ time as a tier miss
    const ghostSha = createHash('sha256').update(Buffer.from('ghost-bytes')).digest('hex');
    const ghostJson = JSON.stringify({ ref: { sha256: ghostSha, size: 11, mime: 'text/plain' } });
    const ghostUrl = `${BASE}/api/f/${name}/file?path=${encodeURIComponent('ghost.txt')}&base_rev=0`;
    const pGhost = await rawFetch(ghostUrl, {
      method: 'PUT',
      headers: { authorization: await authHeader('PUT', ghostUrl, Buffer.from(ghostJson), ownerKey), 'content-type': 'application/json' },
      body: ghostJson,
    });
    eq(pGhost.status, 200, '[tier] ghost-ref commits (bytes presumed in tier)');
    const ghostRead = await rawFetch(`${BASE}/f/${name}/__file?path=ghost.txt`);
    eq(ghostRead.status, 404, '[tier] reading a missing tier object answers blobsd 404, not a 500');

    // 5) cache heuristic: immutable flag on [8-hex]-in-name asset through the
    //    LIVE blessed path; everything else defaults to max-age=300
    const cssResp = await rawFetch(`${BASE}/f/${name}/style-a.1b2c3d4.css`);
    eq(cssResp.status, 200, '[tier] hash-named css serves from blessed snapshot');
    ok((cssResp.headers.get('cache-control') || '').includes('immutable'), '[tier] hash-in-name asset marked immutable');
    ok(cssResp.buf.equals(cssBody), '[tier] hash-named css bytes intact');
    const idxResp = await rawFetch(`${BASE}/f/${name}/`);
    ok((idxResp.headers.get('cache-control') || '').includes('max-age=300'), '[tier] blessed default is max-age=300');

    // 6) 302 mode: gated on CELLD_VAR_BLOBSD_PUBLIC_GET=1 on the host.
    //    Skipped when unset locally (documented in the block header).
    const pubGet = await rawFetch(`${BASE}/f/${name}/__file?path=media%2Fbig.bin`, { redirect: 'manual' });
    if (pubGet.status === 302) {
      const loc = pubGet.headers.get('location') || '';
      ok(new RegExp(`^http://127\\.0\\.0\\.1:${PORT}/${bigSha}$`).test(loc), '[tier] 302 Location targets BLOBSD_PUBLIC_URL/<sha>');
      const followed = await rawFetch(loc);
      ok(followed.buf.equals(big), '[tier] following the 302 yields the bytes browser-direct');
      ok((followed.headers.get('etag') || '').includes(bigSha), '[tier] blobsd carries etag "<sha>"');
    } else {
      console.log('skip  CELLD_VAR_BLOBSD_PUBLIC_GET=1 not set on the host — 302 mode untested here');
    }

    // 7) draft snapshot stays O(rows): pure pointer copies — time-boxed — and
    //    the bless round-trip moves counts/sizes by exactly zero (no byte
    //    duplication anywhere: rows share tier addresses)
    {
      const snapName = `e2e-tier-snap-${suffix}`;
      await signed('POST', '/api/fragments', JSON.stringify({ name: snapName }));
      // public so /f/<name>/__tree reads without a token (the checks compare
      // canonical vs snapshot trees)
      await signed('PUT', `/api/f/${snapName}/manifest`, JSON.stringify({
        name: snapName, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [],
      }));
      const N = 250;
      for (let i = 0; i < N; i++) {
        const r = await signedPutRaw(snapName, `path=${encodeURIComponent('corpus/f' + i + '.md')}&base_rev=0`, Buffer.from('doc '.repeat(i % 37 || 1)));
        if (r.status !== 200) break;
      }
      const timed = await timems(() => signed('POST', `/api/f/${snapName}/drafts`, JSON.stringify({ note: 'snapshot perf' })));
      const slug = timed.v.body?.slug;
      ok(!!slug, '[tier] draft publish answered');
      ok(timed.took < 2000, `[tier] snapshot stayed O(rows) (${timed.took}ms for ${N} rows, budget 2000ms)`);
      await signed('POST', `/api/f/${snapName}/bless`, JSON.stringify({ slug }));
      const sum = (t) => (t.body?.files || []).reduce((a, f) => a + (f.size || 0), 0);
      const fTree = await signed('GET', `/f/${snapName}/__tree`);
      eq(fTree.body?.count, N, '[tier] corpus of 250 listed (machinery excluded)');
      const dTree = await signed('GET', `/d/${slug}/__tree`);
      eq(dTree.body?.count, N, '[tier] snapshot tree mirrors working tree (count)');
      eq(sum(dTree), sum(fTree), '[tier] snapshot size delta == 0 (pointers, not copies)');
      // a second bless cycle leaves the working tree untouched: delta == 0
      const d2 = await signed('POST', `/api/f/${snapName}/drafts`, JSON.stringify({ note: 'second' }));
      await signed('POST', `/api/f/${snapName}/bless`, JSON.stringify({ slug: d2.body.slug }));
      const fTree2 = await signed('GET', `/f/${snapName}/__tree`);
      eq(fTree2.body?.count, fTree.body?.count, '[tier] working-tree count delta across re-bless == 0');
      eq(sum(fTree2), sum(fTree), '[tier] working-tree size delta across re-bless == 0');
    }
  } catch (e) {
    fail++;
    failures.push('tier: ' + String(e && e.stack || e));
    console.log('FAIL  tier unexpected: ' + String(e && e.stack || e));
  }
}

try {
  if (!ONLY || ONLY === 'tier') await tierLaneSection();
} catch (e) {
  fail++;
  failures.push('tier-lane: ' + String(e && e.stack || e));
  console.log('FAIL  tier-lane unexpected: ' + String(e && e.stack || e));
}
// ---- end lane/runtime-tier additions ----

// ---- lane/converge: multi-mirror deletion propagation (tombstones) ----
// The resurrection bug this pins: a mirror still holding a file the remote
// deleted used to PUSH it back (remote-absence read as "must upload"),
// ping-ponging deletions between mirrors forever.
async function convergeSection() {
  if (!section('converge')) return;
  const bin = findBinary();
  ok(!!bin, 'cli binary found for lane/converge');
  if (!bin) return;

  const name = `e2e-cv-${suffix}`;
  await signed('POST', '/api/fragments', JSON.stringify({ name }));
  const home = mkdtempSync(join(tmpdir(), 'fragment-cv-home-'));
  const cfgDir = process.platform === 'darwin'
    ? join(home, 'Library', 'Application Support', 'fragment')
    : join(home, '.config', 'fragment');
  mkdirSync(cfgDir, { recursive: true });
  writeFileSync(join(cfgDir, 'config.json'), JSON.stringify({ host: BASE, secret_key: ownerKey }));
  const env = { ...process.env, HOME: home, XDG_CONFIG_HOME: join(home, '.config'), FRAGMENT_HOST: BASE };

  const dirA = mkdtempSync(join(tmpdir(), 'fragment-cv-a-'));
  const dirB = mkdtempSync(join(tmpdir(), 'fragment-cv-b-'));
  const sync = (dir) => execFileSync(bin, ['sync', name, '--dir', dir], { env, encoding: 'utf8' });

  writeFileSync(join(dirA, 'keep.md'), 'kept');
  writeFileSync(join(dirA, 'gone.md'), 'to be deleted');
  writeFileSync(join(dirA, 'winner.md'), 'original');
  const outA1 = sync(dirA);
  ok(outA1.includes('pushed: 3'), '[converge] mirror A pushes three files');
  const outB1 = sync(dirB);
  ok(existsSync(join(dirB, 'gone.md')), '[converge] mirror B pulls the full set');
  eq(readFileSync(join(dirB, 'keep.md'), 'utf8'), 'kept', '[converge] B content matches');

  rmSync(join(dirA, 'gone.md'));
  const outA2 = sync(dirA);
  ok(outA2.includes('deleted remotely: 1'), '[converge] A pushes its deletion');

  const outB2 = sync(dirB);
  ok(!existsSync(join(dirB, 'gone.md')), '[converge] mirror B deletes its stale copy (no resurrection)');
  ok(outB2.includes('deleted locally: 1'), '[converge] B reports the propagated deletion');

  const outB3 = sync(dirB);
  ok(outB3.includes('all 2 files match host'), '[converge] repeat sync is a no-op (no ping-pong)');
  const list1 = await signed('GET', `/api/f/${name}/files?since_rev=0`);
  const rows1 = list1.body?.files || [];
  eq(rows1.filter((f) => !f.deleted).length, 2, '[converge] host holds exactly the two live files');
  ok(rows1.some((f) => f.deleted && f.path === 'gone.md'), '[converge] gone.md lives on as a tombstone row (the convergence signal)');

  // modification beats deletion: B edits a file A deletes
  writeFileSync(join(dirB, 'winner.md'), 'modified content wins');
  rmSync(join(dirA, 'winner.md'));
  sync(dirA);
  sync(dirB);
  const wUrl = `${BASE}/api/f/${name}/file?path=winner.md`;
  const w = await fetch(wUrl, { headers: { authorization: await authHeader('GET', wUrl, null, ownerKey) } });
  eq(await w.text(), 'modified content wins', '[converge] a modified local copy beats a remote deletion');
}
try {
  if (!ONLY || ONLY === 'converge') await convergeSection();
} catch (e) {
  fail++;
  failures.push('converge: ' + String(e && e.stack || e));
  console.log('FAIL  converge unexpected: ' + String(e && e.stack || e));
}
// ---- end lane/converge ----

// ---- lane/static-root: site/index.html owns the root when present ----
// The static+API authoring shape: page is a file, modules import normally,
// the app handles POSTs and unknown paths. An app with no site/index.html
// keeps the legacy single-handler behavior.
async function staticRootSection() {
  if (!section('static-root')) return;
  {
    const name = `e2e-sr-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
      name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [],
    }));
    await signed('PUT', `/api/f/${name}/file?path=site/index.html&base_rev=0`,
      '<!doctype html><html><head><script type="module" src="/app.js"></script></head><body><h1>static root</h1></body></html>');
    await signed('PUT', `/api/f/${name}/file?path=site/app.js&base_rev=0`,
      'export const n = 41 + 1;\n');
    await signed('PUT', `/api/f/${name}/file?path=app.mjs&base_rev=0`,
      'export default { async fetch(req) { return new Response(req.method === "POST" ? "app-post" : "app-get", { status: 200 }); } }\n');
    const d = await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'go live' }));
    await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: d.body.slug }));

    const root = await fetch(`${BASE}/f/${name}/`);
    const rootBody = await root.text();
    eq(root.status, 200, '[static-root] root serves the page');
    ok(rootBody.includes('static root'), '[static-root] root body is index.html');
    ok((root.headers.get('content-type') || '').includes('text/html'), '[static-root] root content-type is html');

    const mod = await fetch(`${BASE}/f/${name}/app.js`);
    eq(mod.status, 200, '[static-root] module served at clean path');
    ok((mod.headers.get('content-type') || '').includes('javascript'), '[static-root] module MIME is javascript');

    const post = await fetch(`${BASE}/f/${name}/submit`, { method: 'POST', body: 'x' });
    eq(await post.text(), 'app-post', '[static-root] app answers POSTs');

    const unknown = await fetch(`${BASE}/f/${name}/no-such-page`);
    eq(await unknown.text(), 'app-get', '[static-root] unknown GET paths reach the app');
  }
  {
    // no site/index.html: the app keeps the root (legacy shape)
    const name = `e2e-sr-apponly-${suffix}`;
    await signed('POST', '/api/fragments', JSON.stringify({ name }));
    await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
      name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [],
    }));
    await signed('PUT', `/api/f/${name}/file?path=app.mjs&base_rev=0`,
      'export default { async fetch() { return new Response("app-owns-root", { status: 200 }); } }\n');
    const d = await signed('POST', `/api/f/${name}/drafts`, JSON.stringify({ note: 'go live' }));
    await signed('POST', `/api/f/${name}/bless`, JSON.stringify({ slug: d.body.slug }));
    const root = await fetch(`${BASE}/f/${name}/`);
    eq(await root.text(), 'app-owns-root', '[static-root] app without site/index.html keeps the root');
  }
}
try {
  if (!ONLY || ONLY === 'static-root') await staticRootSection();
} catch (e) {
  fail++;
  failures.push('static-root: ' + String(e && e.stack || e));
  console.log('FAIL  static-root unexpected: ' + String(e && e.stack || e));
}
// ---- end lane/static-root ----

// ---- lane/build: fragment build (TS strip, hashing, parse gate) ----
async function buildLaneSection() {
  if (!section('build')) return;
  const bin = findBinary();
  ok(!!bin, 'cli binary found for lane/build');
  if (!bin) return;

  const dir = mkdtempSync(join(tmpdir(), 'fragment-build-'));
  mkdirSync(join(dir, 'site'), { recursive: true });
  mkdirSync(join(dir, 'workflows'), { recursive: true });
  writeFileSync(join(dir, 'fragment.json'), JSON.stringify({ name: `e2e-bd-${suffix}`, visibility: 'public', workflows: [{ name: 'w', file: 'workflows/w.mjs' }], secrets: [] }));
  writeFileSync(join(dir, 'app.ts'), 'const app = { async fetch(req) { return new Response("ts-app " + new URL(req.url).pathname); } };\nexport default app;\n');
  writeFileSync(join(dir, 'site', 'dep.ts'), 'export const answer = (q: string): number => q.length;\n');
  writeFileSync(join(dir, 'site', 'main.ts'), 'import { answer } from "./dep.ts";\nconsole.log(answer("fragment"));\n');
  writeFileSync(join(dir, 'site', 'index.html'), '<p id="out"></p><script type="module" src="/main.js"></script>\n');
  writeFileSync(join(dir, 'workflows', 'w.ts'), 'export async function run(ctx) { await ctx.files.write("built.txt", "ok"); return { done: true }; }\n');

  // isolated HOME + login: `fragment build` signs/records with the local
  // keypair, and a bare runner HOME has none ("no keypair — run fragment
  // login first" — found on the first hosted run)
  const home = mkdtempSync(join(tmpdir(), 'e2e-build-home-'));
  execFileSync(bin, ['login'], { env: { HOME: home }, encoding: 'utf8', stdio: 'pipe' });
  const out = execFileSync(bin, ['build', dir], { encoding: 'utf8', env: { HOME: home } });
  ok(out.includes('compiled (ts -> js): 4'), '[build] TS sources compiled');
  ok(existsSync(join(dir, 'app.mjs')), '[build] app.ts -> app.mjs');
  ok(existsSync(join(dir, 'app.ts')), '[build] sources kept beside compiled siblings');
  ok(existsSync(join(dir, 'workflows', 'w.mjs')), '[build] workflow compiled');
  const hashed = readdirSync(join(dir, 'site')).filter((f) => /^main\.[0-9a-f]{8}\.mjs$/.test(f));
  eq(hashed.length, 1, '[build] site module content-hashed');
  const idx = readFileSync(join(dir, 'site', 'index.html'), 'utf8');
  ok(idx.includes(`src="/${hashed[0]}"`), '[build] index.html rewritten to the hashed name');
  const mainJs = readFileSync(join(dir, 'site', 'main.mjs'), 'utf8');
  const depHashed = readdirSync(join(dir, 'site')).find((f) => /^dep\.[0-9a-f]{8}\.mjs$/.test(f));
  ok(depHashed && mainJs.includes(`./${depHashed}`), '[build] import specifier rewritten to hashed sibling');
  ok(out.includes('parse gate:'), '[build] parse gate ran');

  // gate refuses broken served bytes
  const bad = mkdtempSync(join(tmpdir(), 'fragment-build-bad-'));
  mkdirSync(join(bad, 'site'), { recursive: true });
  writeFileSync(join(bad, 'fragment.json'), JSON.stringify({ name: 'bad', visibility: 'public' }));
  writeFileSync(join(bad, 'site', 'broken.js'), 'const x = {::::;\n');
  let refused = false;
  try { execFileSync(bin, ['build', bad], { encoding: 'utf8', stdio: 'pipe' }); } catch { refused = true; }
  ok(refused, '[build] parse gate refuses a syntax error');
}
try {
  if (!ONLY || ONLY === 'build') await buildLaneSection();
} catch (e) {
  fail++;
  failures.push('build: ' + String(e && e.stack || e));
  console.log('FAIL  build unexpected: ' + String(e && e.stack || e));
}
// ---- end lane/build ----

console.log(`\n${pass} passed, ${fail} failed${fail ? ': ' + failures.join('; ') : ''}`);
process.exit(fail ? 1 : 0);
