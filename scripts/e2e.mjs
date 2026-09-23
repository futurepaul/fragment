#!/usr/bin/env node
// End-to-end suite: the README "What's verified" bullets as executable checks
// against a running fragment host (default http://127.0.0.1:8789).
//
//   node scripts/e2e.mjs [--base URL] [--bin PATH] [--only NAME] [--all]
//
// Bring a stack up first (scripts/dev up — celld dev + the mock
// code.storage), build the CLI (cargo build in cli/), then run this.
// Exit code 0 = every check passed. Created fragments are named e2e-* and
// are left behind on purpose — there is no destroy command yet.
//
// The file model under test (docs/ROADMAP.md wire contract):
//   - code.storage git is file truth: `main` = working files, `live` =
//     the blessed serve point, preview = ephemeral ref, rollback =
//     restore-commit. No drafts, no /d/ snapshot URLs, no blob tier.
//   - this suite writes files either through the CLI binary (a full
//     sync/deploy pass) or directly through the code.storage commit-pack
//     API with a storage token minted from GET /api/f/{name}/storage-token
//     — exactly the two writer shapes that exist in production.
//   - external commits reach the runtime's pins via signed push webhooks
//     (HMAC per the code.storage scheme) plus the 5-minute poll backstop;
//     the suite delivers the webhooks the way the real service would.
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, readdirSync, rmSync } from 'node:fs';
import { readFileSync as guideRead } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { createHmac } from 'node:crypto';
import { genKey, pubkeyFromSecret, nreq, authHeader } from './nip98.mjs';
import { npubFromHex } from '../runtime/src/bech32.js';

const args = process.argv.slice(2);
function arg(name) {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : null;
}
const has = (n) => args.includes(n);
const BASE = arg('--base') || process.env.FRAGMENT_BASE_URL || 'http://127.0.0.1:8789';
const CS_MOCK = arg('--cs-mock') || 'http://127.0.0.1:9940';
// the host secret the stack runs with (CI exports it; local dev reads the
// copy scripts/dev persists next to its state)
const HOST_SECRET = process.env.FRAGMENT_HOST_SECRET
  || (() => { try { return readFileSync('.dev/host-secret', 'utf8').trim(); } catch { return ''; } })();
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
async function waitFor(fn, label, timeoutMs = 15000, everyMs = 300) {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() + everyMs > deadline) { ok(false, `${label} (timed out after ${timeoutMs}ms)`); return null; }
    await sleep(everyMs);
  }
}

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
const ownerPub = pubkeyFromSecret(ownerKey);
const ownerNpub = npubFromHex(ownerPub);
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
  console.error(`no host at ${BASE}. Bring one up: scripts/dev up`);
  process.exit(2);
}

// ---------- the code.storage writer plane ----------
// fragment registry: name -> { viewToken, inboxToken, webhookSecret, repo }
const frags = {};

// Level-c create: the npub secret is generated client-side and crosses the
// wire exactly once, inside the creator's authenticated request.
async function createFragment(name, extra = {}) {
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name, fragmentSecret: genKey() }));
  if (created.status !== 200) throw new Error(`create ${name} -> ${created.status}: ${JSON.stringify(created.body)}`);
  frags[name] = created.body;
  await registerMockWebhook(name);
  return created.body;
}

// Register the fragment's push webhook with the mock — the dev-world
// stand-in for the real service's dashboard registration. With it, every
// commit/ref move the mock applies is ANNOUNCED, so the runtime's
// webhook-driven pin refresh and files triggers run with production
// fidelity (without it, CLI-pushed files would never trigger workflows).
async function registerMockWebhook(name) {
  const wh = frags[name]?.webhookSecret;
  if (!wh) return;
  const r = await fetch(`${CS_MOCK}/__test/webhook-register`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ repo: name, url: `${BASE}/api/f/${name}/webhook`, secret: wh }),
  });
  if (!r.ok) throw new Error(`webhook-register ${name} -> ${r.status}`);
}

// Mint a short-lived, repo-scoped code.storage token (editor+ NIP-98).
async function mint(name, key = ownerKey) {
  const r = await signed('GET', `/api/f/${name}/storage-token`, null, key);
  if (r.status !== 200) throw new Error(`storage-token ${name} -> ${r.status}`);
  return r.body; // { token, repo, api }
}

async function csRaw(name, method, path, { body, contentType } = {}, tok) {
  const { token, repo, api } = tok || await mint(name);
  const r = await fetch(`${api}/api/repos/${repo}${path}`, {
    method,
    headers: { authorization: `Bearer ${token}`, ...(body !== undefined ? { 'content-type': contentType || 'application/json' } : {}) },
    body,
  });
  return { status: r.status, text: await r.text(), repo, api, token };
}

async function csHead(name, branch = 'main', tok) {
  const r = await csRaw(name, 'GET', `/branch?name=${encodeURIComponent(branch)}`, {}, tok);
  if (r.status === 404) return null;
  if (r.status !== 200) throw new Error(`branch read -> ${r.status}: ${r.text.slice(0, 200)}`);
  return JSON.parse(r.text).branch.head_sha;
}

// One NDJSON commit-pack with expected-parent CAS, bounded retries — the
// same writer contract the CLI implements (cli/src/codestorage.rs).
async function csCommit(name, changes, message = 'e2e commit', viaWebhook = true) {
  const tok = await mint(name);
  for (let attempt = 1; ; attempt++) {
    const head = await csHead(name, 'main', tok);
    const meta = {
      target_branch: 'main',
      commit_message: message.slice(0, 400),
      author: { name: 'e2e-suite', email: 'e2e@fragment.test' },
      files: changes.map((c, i) => (c.delete
        ? { path: c.path, operation: 'delete', content_id: `b${i}`, mode: '100644' }
        : { path: c.path, operation: 'upsert', content_id: `b${i}`, mode: '100644' })),
      ...(head ? { expected_target_sha: head } : {}),
    };
    // enveloped first line — the real service (and the hardened mock)
    // refuse a bare metadata payload
    const lines = [JSON.stringify({ metadata: meta })];
    changes.forEach((c, i) => {
      if (c.delete) {
        lines.push(JSON.stringify({ blob_chunk: { content_id: `b${i}`, data: '', eof: true } }));
        return;
      }
      const bytes = typeof c.text === 'string' ? Buffer.from(c.text, 'utf8') : c.bytes;
      if (!bytes.length) {
        lines.push(JSON.stringify({ blob_chunk: { content_id: `b${i}`, data: '', eof: true } }));
        return;
      }
      for (let o = 0; o < bytes.length; o += 3 * 1024 * 1024) {
        const piece = bytes.subarray(o, Math.min(o + 3 * 1024 * 1024, bytes.length));
        lines.push(JSON.stringify({ blob_chunk: { content_id: `b${i}`, data: piece.toString('base64'), eof: o + 3 * 1024 * 1024 >= bytes.length } }));
      }
    });
    const r = await csRaw(name, 'POST', '/commit-pack', { body: lines.join('\n') + '\n', contentType: 'application/x-ndjson' }, tok);
    if (r.status === 201 || r.status === 200) {
      const out = JSON.parse(r.text);
      if (viaWebhook && frags[name]) await webhook(name, 'main');
      return out.result.new_sha;
    }
    if ((r.status === 409 || r.status === 400) && r.text.includes('precondition_failed') && attempt < 3) continue;
    throw new Error(`commit-pack -> ${r.status}: ${r.text.slice(0, 300)}`);
  }
}

// Deliver a signed push webhook exactly the way code.storage would
// (X-Pierre-Signature: t=<unix>,sha256=HMAC(secret, `${t}.${body}`)).
async function webhook(name, ref = 'main') {
  const secret = frags[name]?.webhookSecret;
  if (!secret) throw new Error(`no webhook secret for ${name}`);
  const t = Math.floor(Date.now() / 1000);
  const body = JSON.stringify({
    repository: { url: `${CS_MOCK}/${name}` },
    ref: `refs/heads/${ref}`,
    before: '0'.repeat(40),
    after: '1'.repeat(40),
    pushed_at: new Date().toISOString(),
  });
  const mac = createHmac('sha256', secret).update(`${t}.${body}`).digest('hex');
  const r = await jres(fetch(`${BASE}/api/f/${name}/webhook`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-pierre-event': 'push', 'x-pierre-signature': `t=${t},sha256=${mac}` },
    body,
  }));
  if (r.status !== 200) throw new Error(`webhook ${name}/${ref} -> ${r.status}`);
  return r.body;
}

// Move `live` to main's tip through the documented ref routes — the same
// moves `fragment deploy` makes, from the JS lane.
async function csDeploy(name) {
  const tok = await mint(name);
  const mainTip = await csHead(name, 'main', tok);
  if (!mainTip) throw new Error(`csDeploy ${name}: main has no commits`);
  const liveTip = await csHead(name, 'live', tok);
  if (!liveTip) {
    const r = await csRaw(name, 'POST', '/branches/create', {
      body: JSON.stringify({ base_ref: mainTip, target_branch: 'live', target_is_ephemeral: false }),
    }, tok);
    if (r.status !== 201 && r.status !== 200) throw new Error(`branches/create -> ${r.status}: ${r.text.slice(0, 200)}`);
  } else if (liveTip !== mainTip) {
    const r = await csRaw(name, 'POST', '/merge', {
      body: JSON.stringify({
        target_branch: 'live', source_ref: mainTip, strategy: 'ff_prefer',
        expected_target_sha: liveTip, commit_message: `deploy ${name} (e2e)`,
        author: { name: 'e2e-suite', email: 'e2e@fragment.test' },
      }),
    }, tok);
    if (r.status !== 200) throw new Error(`merge -> ${r.status}: ${r.text.slice(0, 200)}`);
  }
  await webhook(name, 'live');
  return csHead(name, 'live', tok);
}

// Read a file straight from the mock at main (no runtime pin involved).
async function mockFileText(name, path) {
  const r = await csRaw(name, 'GET', `/file?path=${encodeURIComponent(path)}&ref=main`);
  if (r.status !== 200) return null;
  return r.text;
}

// The mock's state levers (test-only routes, never production surface).
async function mockState() {
  return (await fetch(`${CS_MOCK}/__test/state`)).json();
}

// A CLI-owned fragment the JS lane can also touch: create --json captures
// the tokens (webhook secret included), then grant the suite's owner key
// editor so this file's commit/mint helpers work on it too. The grant is
// itself a fragment.json commit, so the webhook after it refreshes the
// runtime's manifest cache before anyone mints.
async function cliCreateGranted(bin, H, name) {
  const out = runCli(bin, ['create', name, '--json'], { env: H });
  frags[name] = JSON.parse(out).data;
  await registerMockWebhook(name);
  runCli(bin, ['grant', name, '--editor', ownerNpub], { env: H });
  await webhook(name, 'main');
  return frags[name];
}

// Grant an npub editor from the JS lane (manifest edit + commit).
async function grantEditor(name, npub) {
  const m = await apiJson(name, '/manifest');
  const editors = new Set([...(m?.editors || []), npub]);
  m.editors = [...editors];
  await csCommit(name, [{ path: 'fragment.json', text: JSON.stringify(m) }], `grant ${npub.slice(0, 16)}`);
}

// Canonical URL + view token fetch helper.
async function canon(name, path = '', viewToken) {
  const vt = viewToken ?? frags[name]?.viewToken;
  const r = await fetch(`${BASE}/f/${name}/${path}?view=${vt}`);
  return { status: r.status, text: await r.text() };
}

// Signed API file read at the pinned main ref.
async function apiFile(name, path, key = ownerKey) {
  const url = `${BASE}/api/f/${name}/file?path=${encodeURIComponent(path)}`;
  const r = await fetch(url, { headers: { authorization: await authHeader('GET', url, null, key) } });
  return { status: r.status, text: r.status === 200 ? await r.text() : null };
}

async function apiJson(name, path, key = ownerKey) {
  const r = await signed('GET', `/api/f/${name}${path}`, null, key);
  return r.body;
}

// ---------- lockdown (router-level) ----------
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
  // with a host secret set, loopback needs the header too. scripts/dev
  // exports FRAGMENT_HOST_SECRET so this suite can verify both layers.
  const wantSecret = HOST_SECRET;
  if (wantSecret) {
    const probeName = `e2e-lk-${suffix}`;
    await createFragment(probeName);
    const noHdr = await fetch(`${BASE}/__internal/f/${probeName}/__internal/ping`);
    eq(noHdr.status, 403, 'internal plane rejects missing host secret');
    const badHdr = await fetch(`${BASE}/__internal/f/${probeName}/__internal/ping`, {
      headers: { 'x-fragment-host-secret': 'wrong' },
    });
    eq(badHdr.status, 403, 'internal plane rejects wrong host secret');
    // a correct secret passes the ROUTER gate; the cell then demands a run
    // token (which an outside caller cannot have) — that second 403 proves
    // both layers are alive and in order.
    const goodHdr = await fetch(`${BASE}/__internal/f/${probeName}/__internal/ping`, {
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
  const created = await createFragment(name);
  eq(created?.npub?.startsWith('npub1'), true, 'create returns fragment npub');
  ok(typeof created?.viewToken === 'string' && created.viewToken.length > 8, 'create returns view token');
  ok(typeof created?.webhookSecret === 'string' && created.webhookSecret.length >= 16, 'create returns a webhook secret');
  ok(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(created?.repo || ''), 'create returns the url-form repo identity');

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
  const st403 = await signed('GET', `/api/f/${name}/storage-token`, null, strangerKey);
  eq(st403.status, 403, 'stranger storage-token → 403');
  // create without the client-side secret is refused (level-c)
  const noSecret = await signed('POST', '/api/fragments', JSON.stringify({ name: `e2e-au2-${suffix}` }));
  ok(noSecret.status >= 400 && /fragmentSecret/i.test(noSecret.body?.error || ''), 'create without fragmentSecret is refused naming the field');
}

// ---------- files: the code.storage plane through the storage token ----------
async function filesSection() {
  if (!section('files')) return;
  const name = `e2e-fi-${suffix}`;
  await createFragment(name);

  // storage-token: repo identity is the url-form id, claims are git-only
  const tokBody = await mint(name);
  eq(tokBody.repo, frags[name].repo, 'storage-token repo matches the create-time identity');
  const claims = JSON.parse(Buffer.from(tokBody.token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/'), 'base64').toString());
  eq(claims.repo, tokBody.repo, 'token repo claim is the url-form identity');
  eq(JSON.stringify(claims.scopes), JSON.stringify(['git:read', 'git:write']), 'token scopes are git-only');
  ok(claims.exp - claims.iat <= 901, 'token expiry is minutes-scale');

  // identity discipline at the mock: the human name 404s, the url works
  const byName = await fetch(`${CS_MOCK}/api/repos/${name}/branch?name=main`, {
    headers: { authorization: `Bearer ${tokBody.token}` },
  });
  eq(byName.status, 404, 'repo-scoped call under the human name → 404');
  const byUrl = await fetch(`${CS_MOCK}/api/repos/${tokBody.repo}/branch?name=main`, {
    headers: { authorization: `Bearer ${tokBody.token}` },
  });
  eq(byUrl.status, 404, 'fresh repo has no main branch yet (404 is an answer)');

  // first commit creates main (no expected parent)
  const sha1 = await csCommit(name, [{ path: 'notes/a.md', text: 'hello v1\n' }], 'first');
  ok(/^[0-9a-f]{40}$/.test(sha1), 'first commit-pack lands and returns a sha');

  // the webhook refreshed the pin; the tree/read planes serve it
  const listed = await waitFor(async () => {
    const files = await apiJson(name, '/files');
    return (files?.files || []).some((f) => f.path === 'notes/a.md' && f.size === 9) ? files : null;
  }, 'tree lists the committed file after the webhook');
  ok(!!listed, 'tree lists the committed file with size');
  const got = await apiFile(name, 'notes/a.md');
  eq(got.text, 'hello v1\n', 'api file read round-trips content');

  // stat: git blob identity + last commit
  const stat = await apiJson(name, `/file/stat?path=${encodeURIComponent('notes/a.md')}`);
  ok(stat?.stat?.present === true && /^[0-9a-f]{40}$/.test(stat.stat.blobSha || ''), 'file/stat reports blob identity');
  const absent = await apiJson(name, `/file/stat?path=${encodeURIComponent('notes/nope.md')}`);
  eq(absent?.stat?.present, false, 'stat of an absent path reports present:false');

  // CAS: a stale expected parent is rejected with precondition_failed
  const tok = await mint(name);
  const stalePack = JSON.stringify({ metadata: {
    target_branch: 'main', commit_message: 'stale',
    author: { name: 'e2e-suite', email: 'e2e@fragment.test' },
    files: [{ path: 'notes/b.md', operation: 'upsert', content_id: 'b0', mode: '100644' }],
    expected_target_sha: '0'.repeat(40),
  } }) + '\n' + JSON.stringify({ blob_chunk: { content_id: 'b0', data: Buffer.from('x').toString('base64'), eof: true } }) + '\n';
  const stale = await csRaw(name, 'POST', '/commit-pack', { body: stalePack, contentType: 'application/x-ndjson' }, tok);
  ok(stale.status === 409 && stale.text.includes('precondition_failed'), 'stale expected-parent CAS → 409 precondition_failed');

  // second commit; reads move
  await csCommit(name, [{ path: 'notes/a.md', text: 'hello v2\n' }], 'second');
  await waitFor(async () => (await apiFile(name, 'notes/a.md')).text === 'hello v2\n', 'reads move to the new pin');

  // delete; tombstone-free: the path is simply gone from the tree
  await csCommit(name, [{ path: 'notes/a.md', delete: true }], 'remove');
  await waitFor(async () => (await apiFile(name, 'notes/a.md')).status === 404, 'deleted path reads 404');
  const files2 = await apiJson(name, '/files');
  ok(!(files2?.files || []).some((f) => f.path === 'notes/a.md'), 'tree no longer lists the deleted path');

  // webhook verification: bad signature and stale deliveries are refused
  const t = Math.floor(Date.now() / 1000);
  const badSig = await fetch(`${BASE}/api/f/${name}/webhook`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-pierre-event': 'push', 'x-pierre-signature': `t=${t},sha256=${'0'.repeat(64)}` },
    body: JSON.stringify({ ref: 'refs/heads/main' }),
  });
  eq(badSig.status, 401, 'webhook with a bad signature → 401');
  const oldT = t - 3600;
  const staleBody = JSON.stringify({ ref: 'refs/heads/main' });
  const staleMac = createHmac('sha256', frags[name].webhookSecret).update(`${oldT}.${staleBody}`).digest('hex');
  const oldSig = await fetch(`${BASE}/api/f/${name}/webhook`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', 'x-pierre-event': 'push', 'x-pierre-signature': `t=${oldT},sha256=${staleMac}` },
    body: staleBody,
  });
  eq(oldSig.status, 401, 'stale webhook timestamp → 401');
}

// ---------- deploy: live ref, preview ephemeral ref, rollback ----------
async function deploySection() {
  if (!section('deploy')) return;
  const bin = findBinary();
  ok(!!bin, 'cli binary found for deploy checks');
  if (!bin) return;
  const H = { HOME: mkdtempSync(join(tmpdir(), 'e2e-deploy-home-')), FRAGMENT_HOST: BASE };
  const name = `e2e-dp-${suffix}`;
  const dir = join(mkdtempSync(join(tmpdir(), 'e2e-deploy-')), 'site');
  mkdirSync(join(dir, 'site'), { recursive: true });
  runCli(bin, ['login'], { env: H });
  const cr = await cliCreateGranted(bin, H, name);
  const viewToken = cr.viewToken;
  writeFileSync(join(dir, 'site', 'index.html'), '<h1>v1 marker</h1>');

  // deploy #1: creates live at main's tip. The CLI moves the ref; the
  // suite delivers the push webhook the way code.storage would, so the
  // runtime's live pin follows each move.
  const d1 = runCli(bin, ['deploy', name, '--dir', dir, '--note', 'first'], { env: H });
  ok(d1.includes('live:'), 'deploy prints the live URL');
  // a deploy moves main (the sync commit) AND live — notify both
  await webhook(name, 'main');
  await webhook(name, 'live');
  const st1 = JSON.parse(runCli(bin, ['status', name, '--json'], { env: H })).data;
  ok(/^[0-9a-f]{40}$/.test(st1.pins.live || ''), 'status reports a live pin');
  eq(st1.pins.live, st1.pins.main, 'first deploy: live == main tip');
  const c1 = await canon(name, '', viewToken);
  eq(c1.status, 200, 'canonical serves after deploy');
  ok(c1.text.includes('v1 marker'), 'canonical serves live content');

  // deploy #2: live moves, canonical follows
  writeFileSync(join(dir, 'site', 'index.html'), '<h1>v2 marker</h1>');
  runCli(bin, ['deploy', name, '--dir', dir], { env: H });
  await webhook(name, 'main');
  await webhook(name, 'live');
  const st2 = JSON.parse(runCli(bin, ['status', name, '--json'], { env: H })).data;
  ok(st2.pins.live !== st1.pins.live, 'second deploy moved the live ref');
  await waitFor(async () => (await canon(name, '', viewToken)).text.includes('v2 marker'), 'canonical follows the moved live ref');
  const state = await mockState();
  eq(state[name].branches.live, st2.pins.live, 'mock holds live exactly where the CLI moved it');

  // drafts = the live ref's commit history, newest first
  const drafts = runCli(bin, ['drafts', name], { env: H });
  ok(drafts.includes('[live]'), 'drafts marks the live deploy');
  const liveShas = [...drafts.matchAll(/^([0-9a-f]{8})/gm)].map((m) => m[1]);
  ok(liveShas.length >= 2, 'drafts lists deploy history');

  // rollback: restore-commit moves live back
  const rb = runCli(bin, ['rollback', name], { env: H });
  ok(rb.includes('rolled back'), 'rollback prints the target');
  await webhook(name, 'live');
  // rollback appends a restore commit whose TREE matches the target —
  // the sha is new by construction, so prove it by content, not identity
  await waitFor(async () => (await canon(name, '', viewToken)).text.includes('v1 marker'), 'canonical serves the rolled-back content');
  const st3 = JSON.parse(runCli(bin, ['status', name, '--json'], { env: H })).data;
  ok(st3.pins.live !== st2.pins.live, 'rollback moved live (a restore commit, not the old sha)');

  // preview: an ephemeral ref exists at main's tip; live untouched
  const pv = runCli(bin, ['deploy', name, '--dir', dir, '--preview'], { env: H });
  await webhook(name, 'main'); // the preview deploy syncs main first
  const slug = (pv.match(/preview\/([0-9a-f]+)/) || [])[1];
  ok(!!slug, 'deploy --preview names its ephemeral ref');
  const st4 = await mockState();
  ok(st4[name].branches[`preview/${slug}`] === st4[name].branches.main, 'ephemeral ref points at main tip');
  ok((st4[name].ephemeral || []).includes(`preview/${slug}`), 'the mock marks the ref ephemeral');
  eq(st4[name].branches.live, st3.pins.live, 'preview left live alone');
  const c4 = await canon(name, '', viewToken);
  ok(c4.text.includes('v1 marker'), 'canonical still serves live after a preview');
}

// ---------- dynamic app ----------
async function appSection() {
  if (!section('app')) return;
  const name = `e2e-ap-${suffix}`;
  await createFragment(name);
  const appSrc = [
    'export default {',
    '  async fetch(req, ctx) {',
    '    const n = ((await ctx.state.get("hits")) || 0) + 1;',
    '    await ctx.state.put("hits", n);',
    '    return new Response("hits=" + n);',
    '  },',
    '};',
  ].join('\n');
  await csCommit(name, [
    { path: 'app.mjs', text: appSrc },
    { path: 'site/index.html', text: '<html>static</html>' },
    { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }) },
  ]);
  await csDeploy(name);
  const r1 = await canon(name, 'anything');
  eq(r1.status, 200, 'app.mjs serves every non-site path');
  eq(r1.text, 'hits=1', 'ctx.state counter first hit');
  const r2 = await canon(name, 'again');
  eq(r2.text, 'hits=2', 'ctx.state persists across requests (cached isolate)');
  const root = await canon(name, '');
  eq(root.text, '<html>static</html>', 'site/index.html owns the root beside an app');
}

// ---------- rooms ----------
async function roomsSection() {
  if (!section('rooms')) return;
  const WebSocket = (await import('node:ws').catch(() => null))?.WebSocket ?? globalThis.WebSocket;
  const name = `e2e-ro-${suffix}`;
  await createFragment(name);
  const vt = frags[name].viewToken;
  await csCommit(name, [
    { path: 'rooms.mjs', text: 'export function onMessage(room, msg, ctx) {\n  if (msg.data.boom) throw new Error("boom");\n  return { broadcast: msg.data };\n}\n' },
    { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [] }) },
  ]);
  await csDeploy(name);

  const wsUrl = `${BASE.replace('http', 'ws')}/f/${name}/__room/lounge?view=${vt}`;

  const deniedCode = await new Promise((resolve) => {
    const ws = new WebSocket(`${BASE.replace('http', 'ws')}/f/${name}/__room/lounge`);
    ws.onerror = () => resolve('error');
    ws.onclose = () => resolve('close');
    ws.onopen = () => resolve('open');
    setTimeout(() => resolve('timeout'), 3000);
  });
  ok(deniedCode !== 'open' && deniedCode !== 'timeout', 'room without a view token on a link fragment refused');

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
  const gotErr = await waitFor(() => C.errors.length > 0 ? true : null, 'rooms.mjs throw → error frame to sender', 8000, 250);
  ok(!!gotErr, 'rooms.mjs throw → error frame to sender');
  const evs = await apiJson(name, '/events');
  ok(JSON.stringify(evs?.events || []).includes('room-error'), 'room-error visible in event log');
  C.ws.close();
}

// ---------- workflows + secrets + inbox ----------
async function workflowSection() {
  if (!section('workflows')) return;
  const name = `e2e-wf-${suffix}`;
  const created = await createFragment(name);
  const inboxToken = created.inboxToken;

  await signed('PUT', `/api/f/${name}/secrets/E2E_SECRET`, 's3cr3t-value');
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
  await csCommit(name, [
    { path: 'workflows/main.mjs', text: wfMain },
    { path: 'workflows/inbox.mjs', text: wfInbox },
    { path: 'fragment.json', text: JSON.stringify({
      name, visibility: 'link', editors: [], viewers: [],
      workflows: [
        { name: 'main', file: 'workflows/main.mjs' },
        { name: 'onpost', file: 'workflows/inbox.mjs', trigger: 'inbox' },
      ],
      secrets: ['E2E_SECRET'],
    }) },
  ], 'workflows + manifest');

  const run = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'main' }));
  eq(run.status, 200, 'manual run → 200');
  eq(run.body?.ok, true, 'workflow ran clean');
  eq(run.body?.output?.secretOk, true, 'workflow sees secret value');
  eq(run.body?.output?.runs, 1, 'ctx.state works in run scope');
  ok(Array.isArray(run.body?.events) && run.body.events.length > 0, 'run reports its events');

  const file = await waitFor(async () => apiFile(name, 'out/run.txt'), 'workflow write lands in the repo');
  eq(file.text, 'ran', 'ctx.files.write landed (own commit refreshed the pin)');

  // ---- write-CAS: ifSha pins + conflicts + stat ----
  const wfCas = [
    'export async function run(ctx) {',
    '  await ctx.files.write("cas/a.txt", "v1");',
    '  const st1 = await ctx.files.stat("cas/a.txt");',
    '  const fresh = st1 && st1.present === true && /^[0-9a-f]{40}/.test(st1.blobSha);',
    '  const w2 = await ctx.files.write("cas/a.txt", "v2", { ifSha: st1.blobSha });',
    '  let conflict = null;',
    '  try {',
    '    await ctx.files.write("cas/a.txt", "v3", { ifSha: st1.blobSha });',
    '    conflict = { threw: false };',
    '  } catch (e) {',
    '    conflict = { threw: true, flagged: e.conflict === true, currentSha: e.currentSha };',
    '  }',
    '  const body = await ctx.files.read("cas/a.txt");',
    '  const absent = await ctx.files.stat("cas/never-existed.txt");',
    '  return { fresh, advanced: w2.deduped === false, conflict, body, absent };',
    '}',
  ].join('\n');
  await csCommit(name, [
    { path: 'workflows/cas.mjs', text: wfCas },
    { path: 'fragment.json', text: JSON.stringify({
      name, visibility: 'link', editors: [], viewers: [],
      workflows: [
        { name: 'main', file: 'workflows/main.mjs' },
        { name: 'onpost', file: 'workflows/inbox.mjs', trigger: 'inbox' },
        { name: 'cas', file: 'workflows/cas.mjs' },
      ],
      secrets: ['E2E_SECRET'],
    }) },
  ]);
  const casRun = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'cas' }));
  eq(casRun.status, 200, 'cas run → 200');
  eq(casRun.body?.ok, true, 'cas workflow ran clean');
  eq(casRun.body?.output?.fresh, true, 'ctx.files.stat reports blob identity');
  eq(casRun.body?.output?.advanced, true, 'ifSha write lands');
  eq(casRun.body?.output?.conflict?.threw, true, 'stale ifSha write throws');
  eq(casRun.body?.output?.conflict?.flagged, true, 'the throw is a typed conflict (e.conflict)');
  ok(/^[0-9a-f]{40}/.test(casRun.body?.output?.conflict?.currentSha || ''), 'conflict carries the current sha');
  eq(casRun.body?.output?.body, 'v2', 'the conflicted write changed nothing');
  eq(casRun.body?.output?.absent?.present, false, 'stat of an unknown path reports present:false');

  const evs = await apiJson(name, '/events');
  ok(JSON.stringify(evs?.events || []).includes('run number 1'), 'ctx.log lands in event log');

  const badTok = await fetch(`${BASE}/api/f/${name}/inbox?t=wrong`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: {} }),
  });
  eq(badTok.status, 403, 'inbox bad token → 403');
  const hdrTok = await fetch(`${BASE}/api/f/${name}/inbox`, {
    method: 'POST',
    headers: { 'x-fragment-inbox-token': inboxToken },
    body: JSON.stringify({ source: 'e2e', payload: { via: 'header' } }),
  });
  eq((await hdrTok.json())?.ok, true, 'inbox via x-fragment-inbox-token header accepted');

  // run tokens are header-only: a query-param token must never reach the
  // internal plane
  const qtok = await fetch(`${BASE}/__internal/f/${name}/__internal/secrets/all?t=junk`, {
    headers: { 'x-fragment-host-secret': HOST_SECRET },
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
  const ranBy = await waitFor(async () => {
    const runs = await apiJson(name, '/runs');
    return (runs?.runs || []).some((r) => r.wf === 'onpost' && r.status === 'success') || null;
  }, 'inbox-triggered workflow ran (async)', 20000, 500);
  ok(!!ranBy, 'inbox-triggered workflow ran (async)');

  // append-only prefixes: enforced on the workflow write funnel
  const wfApp = 'export async function run(ctx) {\n  try {\n    await ctx.files.write("logs/a.jsonl", "changed");\n    return { refused: false };\n  } catch (e) {\n    return { refused: true, conflict: e.conflict === true };\n  }\n}\n';
  await csCommit(name, [
    { path: 'logs/a.jsonl', text: 'first' },
    { path: 'workflows/apponly.mjs', text: wfApp },
    { path: 'fragment.json', text: JSON.stringify({
      name, visibility: 'link', editors: [], viewers: [],
      workflows: [{ name: 'apponly', file: 'workflows/apponly.mjs' }],
      secrets: [], appendOnly: ['logs/'],
    }) },
  ]);
  const apRun = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'apponly' }));
  eq(apRun.body?.output?.refused, true, 'append-only: modifying an existing path is refused');
  eq(apRun.body?.output?.conflict, true, 'append-only refusal is a typed conflict');

  const secList = await signed('GET', `/api/f/${name}/secrets`);
  ok((secList.body?.names || []).includes('E2E_SECRET'), 'secret listed by name only');
  const rm = await signed('DELETE', `/api/f/${name}/secrets/E2E_SECRET`);
  eq(rm.status, 200, 'secret removed');
}

// ---------- paused workflows ----------
async function pausedSection() {
  if (!section('paused')) return;
  const name = `e2e-paused-${suffix}`;
  const created = await createFragment(name);
  const inboxToken = created.inboxToken;

  const wf = 'export async function run(ctx) {\n  await ctx.files.write("out/fired.txt", "yes");\n  return { fired: true };\n}\n';
  await csCommit(name, [
    { path: 'workflows/w.mjs', text: wf },
    { path: 'fragment.json', text: JSON.stringify({
      name, visibility: 'link', editors: [], viewers: [],
      workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox', paused: true }],
      secrets: [],
    }) },
  ]);

  const postResp = await fetch(`${BASE}/api/f/${name}/inbox?t=${inboxToken}`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: {} }),
  });
  eq(postResp.status, 200, 'inbox POST still accepted while paused');
  const blockedBy = await waitFor(async () => {
    const runs = await apiJson(name, '/runs');
    return (runs?.runs || []).some((r) => r.wf === 'w' && r.status === 'blocked') || null;
  }, 'paused trigger recorded as blocked, not run', 15000, 400);
  ok(!!blockedBy, 'paused trigger recorded as blocked, not run');

  const run = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'w' }));
  eq(run.status, 200, 'manual run works while paused');
  eq(run.body?.output?.fired, true, 'manual run fired the workflow');

  const un = await signed('POST', `/api/f/${name}/pause`, JSON.stringify({ workflow: 'w', paused: false }));
  eq(un.status, 200, 'unpause via /pause accepted');
  await fetch(`${BASE}/api/f/${name}/inbox?t=${inboxToken}`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: {} }),
  });
  const unpausedBy = await waitFor(async () => {
    const runs = await apiJson(name, '/runs');
    return (runs?.runs || []).some((r) => r.wf === 'w' && r.status === 'success') || null;
  }, 'unpaused workflow runs on trigger', 15000, 400);
  ok(!!unpausedBy, 'unpaused workflow runs on trigger');
}

// ---------- runs: the failure leg ----------
async function runsSection() {
  if (!section('runs')) return;
  const mkFrag = async (tag, manifest) => {
    const name = `e2e-${tag}-${suffix}`;
    const created = await createFragment(name);
    return { name, inboxToken: created.inboxToken, manifest };
  };
  const putWf = (name, files, manifest) =>
    csCommit(name, [...files, { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], secrets: [], ...manifest }) }], 'workflow fixture');
  const postInbox = (name, tok, headers = {}, payload = {}) =>
    fetch(`${BASE}/api/f/${name}/inbox?t=${tok}`, { method: 'POST', body: JSON.stringify(payload), headers });

  // retryable failure → backoff → retry (attempt 2) → held → replay after a fix
  {
    const { name, inboxToken } = await mkFrag('retry');
    await putWf(name, [{ path: 'workflows/flaky.mjs', text: 'export async function run(ctx) {\n  await ctx.http("http://127.0.0.1:9/unreachable");\n  return { fine: true };\n}\n' }],
      { workflows: [{ name: 'flaky', file: 'workflows/flaky.mjs', trigger: 'inbox', retry: { attempts: 2, backoffMs: 300 } }] });
    await postInbox(name, inboxToken);
    const body = await waitFor(async () => {
      const r = await apiJson(name, '/runs');
      return (r?.runs || []).some((x) => x.status === 'held' && x.attempt === 2) ? r : null;
    }, 'backoff retry reaches held after attempts exhaust', 20000, 300);
    const held = (body?.runs || []).find((r) => r.status === 'held');
    ok(!!held, 'held run row exists with input + error parked');
    ok(held && held.error && /fetch/i.test(held.error), 'held row carries the error');
    ok((body?.counts || {}).held >= 1, 'runs counts include held');
    const evs = await apiJson(name, '/events');
    ok(JSON.stringify(evs?.events || []).includes('"kind":"run.retry"'), 'run.retry event on the ledger');
    // fix the workflow, replay the held run with its original input
    await csCommit(name, [{ path: 'workflows/flaky.mjs', text: 'export async function run(ctx) {\n  return { fixed: true, got: ctx.input ?? null };\n}\n' }]);
    const rep = await signed('POST', `/api/f/${name}/replay`, JSON.stringify({ run: held.id }));
    eq(rep.status, 200, 'replay accepted');
    eq(rep.body?.ok, true, 'replayed run succeeds after the fix');
  }

  // terminal failure: no retry, held immediately
  {
    const { name, inboxToken } = await mkFrag('term');
    await putWf(name, [{ path: 'workflows/term.mjs', text: 'export async function run(ctx) {\n  null.x;\n}\n' }],
      { workflows: [{ name: 'term', file: 'workflows/term.mjs', trigger: 'inbox' }] });
    await postInbox(name, inboxToken);
    const heldBy = await waitFor(async () => {
      const r = await apiJson(name, '/runs');
      return (r?.runs || []).some((x) => x.status === 'held' && x.attempt === 1) ? r : null;
    }, 'terminal error holds immediately (attempt 1)', 25000, 600);
    ok(!!heldBy, 'terminal error holds immediately (attempt 1)');
  }

  // write-suppression: identical content is a recorded no-op
  {
    const { name } = await mkFrag('dedup');
    await putWf(name, [{ path: 'workflows/w.mjs', text: 'export async function run(ctx) {\n  const a = await ctx.files.write("out/x.txt", "same");\n  const b = await ctx.files.write("out/x.txt", "same");\n  return { first: a.deduped, second: b.deduped };\n}\n' }],
      { workflows: [{ name: 'w', file: 'workflows/w.mjs' }] });
    const run = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'w' }));
    eq(run.body?.output?.first, false, 'first write lands');
    eq(run.body?.output?.second, true, 'identical rewrite is suppressed');
    const evs = await apiJson(name, '/events');
    ok(JSON.stringify(evs?.events || []).includes('"kind":"write.deduped"'), 'write.deduped on the ledger');
  }

  // breaker: 5 held runs in a window auto-pause the workflow
  {
    const { name, inboxToken } = await mkFrag('breaker');
    await putWf(name, [{ path: 'workflows/w.mjs', text: 'export async function run(ctx) {\n  null.x;\n}\n' }],
      { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox' }] });
    for (let i = 0; i < 5; i++) await postInbox(name, inboxToken);
    const pausedBy = await waitFor(async () => {
      const st = await apiJson(name, '/status');
      return (st?.paused || []).includes('w') ? st : null;
    }, 'breaker auto-paused the workflow', 30000, 700);
    ok(!!pausedBy, 'breaker auto-paused the workflow');
    const evs = await apiJson(name, '/events');
    ok(JSON.stringify(evs?.events || []).includes('"kind":"workflow.auto-paused"'), 'workflow.auto-paused event');
    await postInbox(name, inboxToken);
    const blocked6 = await waitFor(async () => {
      const runs6 = await apiJson(name, '/runs');
      return (runs6?.runs || []).some((r) => r.status === 'blocked') ? runs6 : null;
    }, 'triggers blocked while auto-paused', 20000, 600);
    ok(!!blocked6, 'triggers blocked while auto-paused');
  }

  // rate ceiling: maxRunsPerHour trips auto-pause
  {
    const { name, inboxToken } = await mkFrag('rate');
    await putWf(name, [{ path: 'workflows/w.mjs', text: 'export async function run(ctx) {\n  return { ok: 1 };\n}\n' }],
      { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox', maxRunsPerHour: 2 }] });
    await postInbox(name, inboxToken);
    await postInbox(name, inboxToken);
    await postInbox(name, inboxToken);
    const rateBlocked = await waitFor(async () => {
      const runs = await apiJson(name, '/runs');
      return (runs?.runs || []).some((r) => r.status === 'blocked') ? runs : null;
    }, 'third auto run in an hour is blocked', 25000, 600);
    ok(!!rateBlocked, 'third auto run in an hour is blocked');
    const pausedBy = await waitFor(async () => {
      const st = await apiJson(name, '/status');
      return (st?.paused || []).includes('w') ? st : null;
    }, 'rate ceiling auto-paused the workflow', 12000, 600);
    ok(!!pausedBy, 'rate ceiling auto-paused the workflow');
  }

  // hop budget: over-deep inbox POSTs are refused with cycle.detected
  {
    const { name, inboxToken } = await mkFrag('hops');
    await putWf(name, [{ path: 'workflows/w.mjs', text: 'export async function run(ctx) {\n  return { ran: true };\n}\n' }],
      { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox' }] });
    await postInbox(name, inboxToken, { 'x-fragment-hops': '99', 'x-fragment-cause': 'other-frag' });
    await postInbox(name, inboxToken);
    const both = await waitFor(async () => {
      const runs = await apiJson(name, '/runs');
      const rs = runs?.runs || [];
      return rs.some((r) => r.status === 'blocked') && rs.some((r) => r.status === 'success') ? runs : null;
    }, 'hop budget blocks over-deep, runs organic', 25000, 600);
    ok(!!both, 'over-budget hops blocked before author code');
    const evs = await apiJson(name, '/events');
    ok(JSON.stringify(evs?.events || []).includes('"kind":"cycle.detected"'), 'cycle.detected on the ledger');
  }
}

// ---------- the guide: every code block is executable ----------
// The GUIDE.md blocks are the product's promises. This section extracts
// them, swaps only ALL-CAPS constants and {placeholders} for local
// fixtures, and runs them: js blocks as workflows/apps/room hooks, the
// manifest JSON as a fragment.json commit, the CLI transcripts as real
// invocations, the recipes end-to-end (scaffold → deploy → live). A js or
// json block without a runner fails the suite — a guide that rots fails CI.
function extractGuideBlocks() {
  const text = guideRead(new URL('../cli/GUIDE.md', import.meta.url), 'utf8');
  const blocks = [];
  const lines = text.split('\n');
  let section = '', sub = '';
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

function runCli(bin, argsx, opts = {}) {
  try {
    return execFileSync(bin, argsx, {
      encoding: 'utf8',
      cwd: opts.cwd || process.cwd(),
      env: { ...process.env, FRAGMENT_HOST: BASE, ...(opts.env || {}) },
      timeout: opts.timeout || 90_000,
    });
  } catch (e) {
    throw new Error(`fragment ${argsx.join(' ')} failed: ${String(e.stderr || e.message).slice(0, 400)}`);
  }
}

// a public deployed fixture fragment (live ref); returns its served base URL
async function serveFixture(tag, files, manifestExtra = {}) {
  const name = `e2e-fx-${tag}-${suffix}`;
  await createFragment(name);
  await csCommit(name, [
    ...files.map(([path, text]) => ({ path, text })),
    { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [], ...manifestExtra }) },
  ]);
  await csDeploy(name);
  return { name, base: `${BASE}/f/${name}/` };
}

async function guideSection() {
  if (!section('guide')) return;
  const blocks = extractGuideBlocks();
  ok(blocks.length >= 15, 'guide parses into blocks');

  // ---- runner: Workflows ctx-tour js block ----
  const tour = blocks.find((b) => b.section === 'Workflows' && b.lang === 'js');
  {
    ok(!!tour, 'guide ships the ctx-tour workflow block');
    const fx = await serveFixture('api', [['site/api/data.json', JSON.stringify({ ok: true, items: [1, 2] })]]);
    const name = `e2e-gd-tour-${suffix}`;
    await createFragment(name);
    await signed('PUT', `/api/f/${name}/secrets/SOME_TOKEN`, 'tok');
    const code = tour.code.replace(/^const API = .*$/m, `const API = ${JSON.stringify(fx.base + 'api/data.json')};`);
    await csCommit(name, [
      { path: 'notes/today.md', text: 'today: shipped the guide test' },
      { path: 'data/export.csv', text: 'a,b\n1,2\n' },
      { path: 'workflows/digest.mjs', text: code },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'digest', file: 'workflows/digest.mjs' }], secrets: ['SOME_TOKEN'] }) },
    ]);
    if (!process.env.OPENROUTER_API_KEY) {
      console.log('skip  ctx-tour block run (no OPENROUTER_API_KEY on this stack)');
    } else {
      const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'digest' }));
      eq(r.body?.ok, true, 'ctx-tour block runs clean (files/bytes/http/secrets/ai/state/log)');
      const files = await apiJson(name, '/files');
      ok((files?.files || []).some((f) => f.path.startsWith('digests/')), 'ctx-tour wrote a digest file');
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
    await createFragment(name);
    const code = patterns.poller.replace(/^const SOURCE = .*$/m, `const SOURCE = ${JSON.stringify(fx.base + 'api/tree.json')}`);
    await csCommit(name, [
      { path: 'workflows/watch.mjs', text: code },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'watch', file: 'workflows/watch.mjs' }], secrets: [] }) },
    ]);
    const r1 = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'watch' }));
    eq(r1.body?.ok, true, 'poller pattern runs clean');
    const feed = await apiJson(name, '/files');
    const paths = (feed?.files || []).map((f) => f.path);
    ok(paths.some((p) => p.includes('notes__a.md')), 'poller filed new content');
    ok(!paths.some((p) => p.includes('x.mjs')), 'poller skipped machinery');
    await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'watch' }));
    const feed2 = await apiJson(name, '/files');
    eq(feed2?.files?.length, feed?.files?.length, 'poller re-run is idempotent');
  }
  {
    const tgt = await createFragment(`e2e-gd-tgt-${suffix}`);
    const name = `e2e-gd-once-${suffix}`;
    await createFragment(name);
    const code = patterns.once.replace(/^const WEBHOOK = .*$/m, `const WEBHOOK = ${JSON.stringify(`${BASE}/api/f/e2e-gd-tgt-${suffix}/inbox?t=${tgt.inboxToken}`)}`);
    await csCommit(name, [
      { path: 'workflows/notify.mjs', text: code },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'notify', file: 'workflows/notify.mjs', trigger: 'inbox' }], secrets: [] }) },
    ]);
    const input = JSON.stringify({ workflow: 'notify', input: { inbox: { id: 42, payload: { hello: 'guide' } } } });
    const r1 = await signed('POST', `/api/f/${name}/run`, input);
    eq(r1.body?.output?.sent, true, 'once pattern fires the effect');
    const r2 = await signed('POST', `/api/f/${name}/run`, input);
    eq(r2.body?.output?.skipped, true, 'once pattern refuses the duplicate');
    const evs = await apiJson(`e2e-gd-tgt-${suffix}`, '/events');
    eq((evs?.events || []).filter((e) => e.kind === 'inbox').length, 1, 'webhook received exactly one delivery');
  }
  {
    const name = `e2e-gd-sync-${suffix}`;
    await createFragment(name);
    await csCommit(name, [
      { path: 'notes/one.md', text: 'one' },
      { path: 'notes/two.md', text: 'two' },
      { path: 'workflows/reindex.mjs', text: patterns['sync-reaction'] },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'reindex', file: 'workflows/reindex.mjs', trigger: 'files' }], secrets: [] }) },
    ]);
    const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'reindex' }));
    eq(r.body?.output?.indexed, 2, 'sync-reaction indexed the notes');
    const idx = await waitFor(async () => apiFile(name, 'INDEX.md'), 'INDEX.md lands');
    ok((idx?.text || '').includes('notes/two.md'), 'INDEX.md lists the notes');
  }
  {
    const name = `e2e-gd-log-${suffix}`;
    const created = await createFragment(name);
    await csCommit(name, [
      { path: 'workflows/log.mjs', text: patterns['inbox-log'] },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'log', file: 'workflows/log.mjs', trigger: 'inbox' }], secrets: [] }) },
    ]);
    await fetch(`${BASE}/api/f/${name}/inbox?t=${created.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 'guide', payload: { n: 1 } }) });
    await fetch(`${BASE}/api/f/${name}/inbox?t=${created.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 'guide', payload: { n: 2 } }) });
    const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'log' }));
    eq(r.body?.ok, true, 'inbox-log pattern runs clean');
    const day = new Date().toISOString().slice(0, 10);
    const both = await waitFor(async () => {
      const f = await apiFile(name, `log/${day}.jsonl`);
      return (f.text || '').trim().split('\n').filter(Boolean).length >= 2 ? f : null;
    }, 'inbox-log appended both messages', 12000, 400);
    ok(!!both, 'inbox-log appended both messages');
    const r2 = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'log' }));
    eq(r2.body?.output?.drained, 0, 'acked messages never re-process');
  }
  {
    if (!process.env.OPENROUTER_API_KEY) {
      console.log('skip  ai-pass pattern (no OPENROUTER_API_KEY on this stack)');
    } else {
      const name = `e2e-gd-ai-${suffix}`;
      await createFragment(name);
      await csCommit(name, [
        { path: 'notes/x.md', text: 'the quick brown fox jumps over the lazy dog' },
        { path: 'workflows/digest.mjs', text: patterns['ai-pass'] },
        { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'digest', file: 'workflows/digest.mjs' }], secrets: [] }) },
      ]);
      const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'digest' }));
      eq(r.body?.output?.notes, 1, 'ai-pass pattern runs clean');
    }
  }

  // pattern: dropzone (append-only + hash naming + ack)
  {
    const name = `e2e-gd-drop-${suffix}`;
    const created = await createFragment(name);
    await csCommit(name, [
      { path: 'workflows/ingest.mjs', text: patterns.dropzone },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'ingest', file: 'workflows/ingest.mjs', trigger: 'inbox' }], secrets: [], appendOnly: ['inbox/'] }) },
    ]);
    const post = (text) => fetch(`${BASE}/api/f/${name}/inbox?t=${created.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 'drop', payload: { text } }) });
    await post('first drop');
    await post('first drop'); // identical re-drop
    await post('second drop');
    const okRuns = await waitFor(async () => {
      const runs = await apiJson(name, '/runs');
      const n = (runs?.runs || []).filter((r) => r.status === 'success').length;
      return n >= 3 ? n : null;
    }, 'each drop ran the ingest workflow', 25000, 800);
    ok(!!okRuns, 'each drop ran the ingest workflow');
    const files = await apiJson(name, '/files');
    const inboxFiles = (files?.files || []).filter((f) => f.path.startsWith('inbox/'));
    eq(inboxFiles.length, 2, 'identical drops collapsed to one file (hash naming)');
    const r2 = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'ingest' }));
    eq(r2.body?.output?.filed, 0, 'acked drops never re-file');
  }

  // pattern: watcher — its documented inbox trigger drives the run; the
  // notify-poke leg (manifest notifyUrls → relay delivery) is asserted in
  // the platform section (the relay is a separate deployment).
  {
    const fx = await serveFixture('wtree', [['site/index.html', '<!doctype html>seed']]);
    const name = `e2e-gd-watch-${suffix}`;
    const created = await createFragment(name);
    const code = patterns.watcher
      .replace(/^const SOURCE = .*$/m, `const SOURCE = ${JSON.stringify(fx.base.replace(/\/$/, ''))}`)
      .replace('const token = ctx.secrets.SOURCE_VIEW_TOKEN;', 'const token = "";')
      .replace(/^const DELAY_MS = .*$/m, process.env.OPENROUTER_API_KEY ? 'const DELAY_MS = 0;' : 'const DELAY_MS = 999999999;')
      .replace('?view=" + token', '"')
      .replace('+ "&view=" + token', '');
    await csCommit(name, [
      { path: 'workflows/check.mjs', text: code },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'check', file: 'workflows/check.mjs', trigger: 'inbox' }], secrets: [] }) },
    ]);
    await fetch(`${BASE}/api/f/${name}/inbox?t=${created.inboxToken}`, { method: 'POST', body: JSON.stringify({ source: 'poke', payload: {} }) });
    const ran = await waitFor(async () => {
      const runs = await apiJson(name, '/runs');
      return (runs?.runs || []).some((r) => r.status === 'success') ? true : null;
    }, 'watcher pattern ran from its inbox trigger', 60000, 500);
    ok(ran, 'watcher pattern ran (inbox trigger; notify leg covered in platform)');
    if (process.env.OPENROUTER_API_KEY) {
      const enriched = await waitFor(async () => {
        const files = await apiJson(name, '/files');
        return (files?.files || []).some((f) => f.path.startsWith('feed/')) ? true : null;
      }, 'watcher pattern wrote enriched feed items (async phase)', 20000, 800);
      ok(enriched, 'watcher pattern wrote enriched feed items');
    }
  }

  // ---- runner: manifest JSON block (edit-and-commit, manifest-set shape) ----
  {
    const manBlock = blocks.find((b) => b.section === 'The manifest' && b.lang === 'json');
    ok(!!manBlock, 'guide ships the manifest JSON block');
    const name = `e2e-gd-man-${suffix}`;
    await createFragment(name);
    const parsed = JSON.parse(manBlock.code.replaceAll('npub1…', ownerNpubStub()));
    parsed.name = name;
    await csCommit(name, [{ path: 'fragment.json', text: JSON.stringify(parsed) }]);
    const got = await apiJson(name, '/manifest');
    eq(got?.name, name, 'documented manifest shape commits and serves (name from registry)');
    eq(Array.isArray(got?.workflows), true, 'normalized manifest carries workflows');
  }

  // ---- runner: app.mjs block ----
  {
    const appBlock = blocks.find((b) => b.section === 'Sites and apps' && b.lang === 'js');
    ok(!!appBlock, 'guide ships the app.mjs block');
    const name = `e2e-gd-app-${suffix}`;
    await createFragment(name);
    await csCommit(name, [
      { path: 'app.mjs', text: appBlock.code },
      { path: 'site/index.html', text: '<!doctype html><title>seed</title>' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }) },
    ]);
    await csDeploy(name);
    const resp = await fetch(`${BASE}/f/${name}/api`);
    eq(await resp.text(), `hello /f/${name}/api`, 'app.mjs block serves its documented response');
    const seed = await fetch(`${BASE}/f/${name}/`);
    ok((await seed.text()).includes('seed'), 'guide app example: site/index.html still serves the root');
  }

  // ---- runner: rooms.mjs block ----
  {
    const WebSocket = (await import('node:ws').catch(() => null))?.WebSocket ?? globalThis.WebSocket;
    const roomBlock = blocks.find((b) => b.section === 'Multiplayer (rooms)' && b.lang === 'js');
    ok(!!roomBlock, 'guide ships the rooms.mjs block');
    const name = `e2e-gd-room-${suffix}`;
    await createFragment(name);
    await csCommit(name, [
      { path: 'rooms.mjs', text: roomBlock.code },
      { path: 'site/index.html', text: '<!doctype html><title>room</title>' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }) },
    ]);
    await csDeploy(name);
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
    const tgt = await createFragment(`e2e-gd-inb-${suffix}`);
    const url = m[1].replace('{host}', BASE).replace('{name}', tgt.name).replace('{inboxToken}', tgt.inboxToken);
    const resp = await fetch(url, { method: 'POST', body: m[2] });
    eq(resp.status, 200, 'inbox spec POST works verbatim');
    eq((await resp.json()).ok, true, 'inbox spec body accepted');
  }

  // ---- runner: CLI transcripts (First moves, daily loop, recipes) ----
  const bin = findBinary();
  if (!bin) {
    console.log('skip  guide CLI transcripts (no fragment binary built)');
  } else {
    // First moves: login + whoami + init with an isolated HOME
    {
      const home = mkdtempSync(join(tmpdir(), 'e2e-guide-home-'));
      const H = { HOME: home };
      const out1 = runCli(bin, ['login'], { env: H });
      ok(out1.includes('npub'), 'guide: fragment login prints an npub (isolated HOME)');
      const out2 = runCli(bin, ['whoami'], { env: H });
      ok(out2.includes('npub'), 'guide: whoami echoes the identity');
      const out3 = runCli(bin, ['init', `e2e-gd-fm-${suffix}`], { cwd: home, env: H });
      ok(out3.includes('live:') && out3.includes('webhook URL'), 'guide: init prints the live + webhook URLs');
    }
    // The daily loop: sync → deploy → preview → drafts → rollback, run as
    // documented (names substituted; the transcript owns its fragment via
    // an isolated CLI identity)
    {
      const home = mkdtempSync(join(tmpdir(), 'e2e-guide-home-'));
      const H = { HOME: home };
      const name = `e2e-gd-loop-${suffix}`;
      const dir = join(mkdtempSync(join(tmpdir(), 'e2e-guide-')), 'my-thing');
      mkdirSync(join(dir, 'site'), { recursive: true });
      writeFileSync(join(dir, 'site', 'note.html'), '<h1>first</h1>');
      runCli(bin, ['login'], { env: H });
      frags[name] = JSON.parse(runCli(bin, ['create', name, '--json'], { env: H })).data;
      await registerMockWebhook(name);
      runCli(bin, ['sync', name, '--dir', dir], { env: H, cwd: dir });
      const pub = runCli(bin, ['deploy', name, '--dir', dir, '--note', 'first cut'], { env: H });
      ok(pub.includes('live:'), 'guide: deploy goes live');
      const pv = runCli(bin, ['deploy', name, '--dir', dir, '--preview'], { env: H });
  await webhook(name, 'main'); // the preview deploy syncs main first
      ok(/preview: preview\/[0-9a-f]+ \(ephemeral ref/.test(pv), 'guide: deploy --preview prints its ephemeral ref (no served URL)');
      const drafts = runCli(bin, ['drafts', name], { env: H });
      ok(drafts.includes('[live]'), 'guide: drafts lists the deploy history');
      writeFileSync(join(dir, 'site', 'note.html'), '<h1>second</h1>');
      runCli(bin, ['deploy', name, '--dir', dir], { env: H });
      const rb = runCli(bin, ['rollback', name], { env: H });
      ok(rb.includes('rolled back'), 'guide: rollback works');
    }
    // Recipes: vault + dropzone, run as documented (scaffold → deploy → live)
    for (const [tpl, frag] of [['vault', 'my-vault'], ['dropzone', 'my-drop']]) {
      const block = blocks.find((b) => b.section.startsWith('Recipes') && !b.lang && b.code.includes(frag));
      ok(!!block, `guide ships the ${tpl} recipe transcript`);
      const workdir = mkdtempSync(join(tmpdir(), 'e2e-guide-recipe-'));
      const recipeName = `e2e-gd-recipe-${tpl}-${suffix}`;
      const home = mkdtempSync(join(tmpdir(), 'e2e-guide-home-'));
      const H = { HOME: home };
      runCli(bin, ['login'], { env: H });
      // the vault recipe references ../my-notes — the folder to overlay
      if (tpl === 'vault') {
        mkdirSync(join(workdir, 'my-notes'), { recursive: true });
        writeFileSync(join(workdir, 'my-notes', 'welcome.md'), '# welcome\nfirst note\n');
      }
      let cwd = workdir;
      let watcher = null;
      let viewToken = '';
      for (const raw of block.code.split('\n')) {
        const line = shline(raw).replaceAll(frag, recipeName);
        if (!line) continue;
        if (line.startsWith('cd ')) { cwd = resolve(cwd, line.slice(3)); continue; }
        if (line.startsWith('echo ')) {
          const mm = line.match(/^echo "(.*)" > (.*)$/);
          if (!mm) continue;
          const target = resolve(cwd, mm[2]);
          mkdirSync(join(target, '..'), { recursive: true });
          writeFileSync(target, mm[1] + '\n');
          continue;
        }
        if (line.startsWith('fragment ')) {
          const argv = tokenize(line.slice('fragment '.length));
          if (argv.includes('--watch')) {
            // the documented "leave running" step: start it, let it work,
            // kill it before moving on
            watcher = spawn(bin, argv, { cwd, env: { ...process.env, FRAGMENT_HOST: BASE, ...H }, stdio: 'ignore' });
            await sleep(2000);
          } else {
            const out = runCli(bin, argv, { cwd, env: H });
            if (argv[0] === 'init' || argv[0] === 'create') {
              viewToken = (out.match(/share link:\s+\S+\?view=(\S+)/) || [])[1] || '';
              // register the recipe fragment's push webhook with the mock
              // (the real service would have it from its dashboard). The
              // code.storage webhook secret only ever leaves via create's
              // JSON or a rotate; rotate is the documented owner verb that
              // mints a fresh one here.
              const rot = JSON.parse(runCli(bin, ['rotate', argv[1], '--json'], { env: H }));
              frags[recipeName] = { ...(frags[recipeName] || {}), webhookSecret: rot.data.webhook_secret };
              viewToken = rot.data.view_token || viewToken;
              await registerMockWebhook(recipeName);
            }
          }
          continue;
        }
        // plain commentary lines are skipped
      }
      if (watcher) { try { watcher.kill(); } catch {} }
      // the recipe's promise: the scaffold is live at its canonical URL
      ok(!!viewToken, `guide: ${tpl} recipe printed a view token`);
      const canonResp = await waitFor(async () => {
        const r = await fetch(`${BASE}/f/${recipeName}/?view=${viewToken}`);
        return r.status === 200 ? r : null;
      }, `guide: ${tpl} canonical serves after the recipe`, 10000);
      eq(canonResp ? 200 : 0, 200, `guide: ${tpl} canonical serves after the recipe`);
      if (tpl === 'dropzone') {
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
    if (b.lang === 'json') ok(b.section === 'The manifest', `json block has a runner: ${b.section}/${b.sub || '—'}`);
  }
}

// ---------- platform api: machine reads + notify ----------
async function platformSection() {
  if (!section('platform')) return;
  // machine-read plane: gated exactly like the site, served from live
  {
    const name = `e2e-pa-pub-${suffix}`;
    await createFragment(name);
    await csCommit(name, [
      { path: 'notes/a.md', text: 'alpha' },
      { path: 'workflows/w.mjs', text: 'code' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [{ name: 'w', file: 'workflows/w.mjs' }], secrets: [] }) },
    ]);
    await csDeploy(name);
    const t = await (await fetch(`${BASE}/f/${name}/__tree`)).json();
    ok(t.files?.some((f) => f.path === 'notes/a.md'), '__tree lists content (public, from live)');
    ok(!t.files?.some((f) => f.path.startsWith('workflows/')), '__tree hides machinery');
    const f = await fetch(`${BASE}/f/${name}/__file?path=notes/a.md`);
    eq(await f.text(), 'alpha', '__file returns raw content');
    eq((await fetch(`${BASE}/f/${name}/__file?path=workflows/w.mjs`)).status, 400, '__file blocks machinery');
  }
  {
    const name = `e2e-pa-tok-${suffix}`;
    await createFragment(name);
    await csCommit(name, [
      { path: 'notes/secret.md', text: 'hidden' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [] }) },
    ]);
    await csDeploy(name);
    eq((await fetch(`${BASE}/f/${name}/__tree`)).status, 403, '__tree refuses without token');
    const t = await (await fetch(`${BASE}/f/${name}/__tree?view=${frags[name].viewToken}`)).json();
    ok(t.files?.some((x) => x.path === 'notes/secret.md'), '__tree works with the view link');
  }
  // meta: OG injection, placeholder previews, gallery listing
  {
    const name = `e2e-pa-meta-${suffix}`;
    await createFragment(name);
    await csCommit(name, [
      { path: 'site/index.html', text: '<!doctype html><html><head></head><body>x</body></html>' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], meta: { title: 'Meta Test', description: 'desc', listed: true } }) },
    ]);
    await csDeploy(name);
    const page = await (await fetch(`${BASE}/f/${name}/`)).text();
    ok(page.includes('og:title') && page.includes('Meta Test'), 'manifest meta injects OG tags');
    ok(page.includes('__preview.svg'), 'OG image defaults to the placeholder');
    const svg = await fetch(`${BASE}/f/${name}/__preview.svg`);
    eq(svg.status, 200, 'placeholder preview served');
    ok((svg.headers.get('content-type') || '').includes('svg'), 'placeholder is svg');
    const gal = await (await fetch(`${BASE}/api/gallery`)).json();
    const me = (gal.fragments || []).find((x) => x.name === name);
    ok(!!me && me.title === 'Meta Test' && !!me.viewToken, 'listed fragment appears in the gallery with its share token');
    // __rt.js must parse as a classic script (template-escape regression)
    const rt = await (await fetch(`${BASE}/f/${name}/__rt.js`)).text();
    const { writeFileSync: wfs } = await import('node:fs');
    const { tmpdir: td } = await import('node:os');
    const { join: jn } = await import('node:path');
    wfs(jn(td(), 'rt-check.js'), rt);
    const parse = spawnSync(process.execPath, ['--check', jn(td(), 'rt-check.js')]);
    ok(parse.status === 0, '__rt.js parses as a script (no template-escape damage)');
    // unlisted private fragments do not appear
    await createFragment(`e2e-pa-priv-${suffix}`);
    const gal2 = await (await fetch(`${BASE}/api/gallery`)).json();
    ok(!(gal2.fragments || []).some((x) => x.name === `e2e-pa-priv-${suffix}`), 'unlisted fragments stay out of the gallery');
  }

  // notify-on-change: manifest notifyUrls → external mutation → queue
  // enqueue lands on the ledger (notify.queued). The relay DELIVERY (the
  // notify-relay deployment consuming the queue) is deployment-level and
  // cannot run inside single-app `celld dev` — asserted here at the
  // enqueue boundary, which is the runtime's half of the contract.
  {
    const dst = await createFragment(`e2e-pa-dst-${suffix}`);
    const src = await createFragment(`e2e-pa-src-${suffix}`);
    await csCommit(src.name, [
      { path: 'notes/seed.md', text: 'seed' },
      { path: 'fragment.json', text: JSON.stringify({ name: src.name, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [], notifyUrls: [`${BASE}/api/f/${dst.name}/inbox?t=${dst.inboxToken}`] }) },
    ]);
    await csCommit(src.name, [{ path: 'notes/change.md', text: 'changed' }], 'notify trigger');
    const queued = await waitFor(async () => {
      const evs = await apiJson(src.name, '/events');
      return JSON.stringify(evs?.events || []).includes('"kind":"notify.queued"') ? true : null;
    }, 'notifyUrls change enqueued a notification (notify.queued)', 12000, 400);
    ok(queued, 'external change enqueued a notify message (relay delivery is deployment-level)');
    console.log('skip  notify-relay delivery (needs the relay deployment; celld dev runs one app)');
  }
}

// ---------- file sync through the CLI (the code.storage sync engine) ----------
async function filesyncSection() {
  if (!section('filesync')) return;
  const bin = findBinary();
  if (!bin) {
    console.log('skip  filesync CLI checks (no fragment binary built)');
    return;
  }
  const H = { HOME: mkdtempSync(join(tmpdir(), 'fragment-fs-home-')), FRAGMENT_HOST: BASE };
  runCli(bin, ['login'], { env: H });
  const cliCreate = (name) => cliCreateGranted(bin, H, name);

  // conflict: both sides changed → local keeps, remote copy saved, exit 3
  {
    const name = `e2e-fs-conf-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'doc.md'), 'base');
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    // remote side (the commit API) rewrites it; local edits too
    await csCommit(name, [{ path: 'doc.md', text: 'theirs' }], 'remote edit');
    writeFileSync(join(dir, 'doc.md'), 'ours');
    const res = spawnSync(bin, ['sync', name, '--dir', dir], { encoding: 'utf8', env: { ...process.env, FRAGMENT_HOST: BASE, ...H } });
    eq(res.status, 3, 'both-changed sync exits 3 (conflict)');
    eq(readFileSync(join(dir, 'doc.md'), 'utf8'), 'ours', 'conflict: local keeps ours');
    const copy = readdirSync(dir).find((n) => n.startsWith('doc.md.conflict-') || n.startsWith('doc.conflict-'));
    ok(!!copy, 'conflict: remote copy saved beside it (.conflict-…)');
    if (copy) eq(readFileSync(join(dir, copy), 'utf8'), 'theirs', 'conflict copy holds the remote content');
    eq(await mockFileText(name, 'doc.md'), 'theirs', 'remote untouched by our conflict');
  }

  // modes: pull withholds deletions; --prune applies them
  {
    const name = `e2e-fs-mode-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'keep.md'), 'keep');
    writeFileSync(join(dir, 'drop.md'), 'drop');
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    await csCommit(name, [{ path: 'drop.md', delete: true }], 'remote delete');
    const r = runCli(bin, ['sync', name, '--dir', dir, '--mode', 'pull', '--json'], { env: H });
    ok(r.includes('withheld_deletions') || r.includes('withheldDeletions'), 'pull mode withholds the deletion');
    ok(existsSync(join(dir, 'drop.md')), 'pull mode kept the local file');
    runCli(bin, ['sync', name, '--dir', dir, '--mode', 'pull', '--prune'], { env: H });
    ok(!existsSync(join(dir, 'drop.md')), '--prune applies the remote deletion');
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
    const base = mkdtempSync(join(tmpdir(), 'fragment-fs-'));
    const srcDir = join(base, 'src');
    const dir = join(base, 'frag');
    mkdirSync(srcDir, { recursive: true });
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(srcDir, 'a.md'), 'one');
    mkdirSync(join(srcDir, 'sub'));
    writeFileSync(join(srcDir, 'sub', 'b.md'), 'two');
    await cliCreate(name);
    runCli(bin, ['sync', name, '--dir', dir, '--mirror-from', srcDir], { env: H });
    const paths = await mockPaths(name);
    ok(paths.includes('a.md') && paths.includes('sub/b.md'), 'mirror-from overlays the source');
    writeFileSync(join(srcDir, 'c.md'), 'three');
    runCli(bin, ['sync', name, '--dir', dir, '--mirror-from', srcDir], { env: H });
    const paths2 = await mockPaths(name);
    ok(paths2.includes('c.md'), 'new source files arrive on later passes');
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
    const content = (ps) => ps.filter((x) => x !== 'fragment.json');
    const paths = content(await mockPaths(name));
    eq(paths.length, 20, 'guard prevented remote deletions');
    runCli(bin, ['sync', name, '--dir', dir, '--apply-mass-delete'], { env: H });
    const paths2 = content(await mockPaths(name));
    eq(paths2.length, 5, '--apply-mass-delete proceeds');
  }

  // chunked big file: >4MiB rides multiple commit-pack chunks
  {
    const name = `e2e-fs-big-${suffix}`;
    await cliCreate(name);
    const dir = join(mkdtempSync(join(tmpdir(), 'fragment-fs-')), 'm');
    mkdirSync(dir, { recursive: true });
    const big = Buffer.alloc(5 * 1024 * 1024 + 1234);
    for (let i = 0; i < big.length; i++) big[i] = i % 251;
    writeFileSync(join(dir, 'big.bin'), big);
    runCli(bin, ['sync', name, '--dir', dir], { env: H });
    const tok = await mint(name);
    const back = await fetch(`${tok.api}/api/repos/${tok.repo}/file?path=big.bin&ref=main`, {
      headers: { authorization: `Bearer ${tok.token}` },
    });
    eq(back.status, 200, '5MB file landed via the chunked commit-pack');
    const bytes = Buffer.from(await back.arrayBuffer());
    ok(bytes.length === big.length && bytes.equals(big), '5MB file round-trips byte-exact');
  }

  // continuous: live channel pulls a remote write in seconds; local pushes
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
    child.stderr.on('data', (d) => { log += d; });
    await sleep(2500);
    // remote write through the commit API + webhook → runtime broadcasts
    // → the watcher's live channel pulls it
    await csCommit(name, [{ path: 'remote.md', text: 'from the server' }], 'remote write');
    const arrived = await waitFor(async () =>
      existsSync(join(dir, 'remote.md')) && readFileSync(join(dir, 'remote.md'), 'utf8') === 'from the server' ? true : null,
    'continuous mode pulled a remote write within seconds', 15000, 300);
    ok(arrived, 'continuous mode pulled a remote write within seconds');
    // local edit pushes
    writeFileSync(join(dir, 'local.md'), 'from the client');
    const pushed = await waitFor(async () => (await mockFileText(name, 'local.md')) === 'from the client' ? true : null,
      'continuous mode pushed a local edit', 15000, 300);
    ok(pushed, 'continuous mode pushed a local edit');
    // double-watcher guard: a second watcher must refuse
    const second = spawnSync(bin, ['sync', name, '--dir', dir, '--watch'], {
      encoding: 'utf8', env: { ...process.env, FRAGMENT_HOST: BASE, ...H }, timeout: 5000,
    });
    ok(second.status !== 0 && String(second.stderr).includes('another fragment sync'), 'second watcher refused by the lock');
    child.kill();
  }
}

async function mockPaths(name) {
  const tok = await mint(name);
  const r = await csRaw(name, 'GET', '/files/metadata?ref=main', {}, tok);
  if (r.status === 404) return [];
  return JSON.parse(r.text).files.map((f) => f.path);
}

// ---------- git interop: .gitignore honored, git state never uploaded ----------
async function gitignoreSection() {
  if (!section('gitignore')) return;
  const bin = findBinary();
  if (!bin) {
    console.log('skip  gitignore checks (no fragment binary built)');
    return;
  }
  const H = { HOME: mkdtempSync(join(tmpdir(), 'fragment-gi-home-')), FRAGMENT_HOST: BASE };
  runCli(bin, ['login'], { env: H });
  const name = `e2e-gi-${suffix}`;
  await cliCreateGranted(bin, H, name);

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

  runCli(bin, ['sync', name, '--dir', dir], { env: H });
  let paths = await mockPaths(name);
  ok(!paths.includes('secrets.txt'), 'gitignored secrets.txt never uploads');
  ok(!paths.some((p) => p.startsWith('ignored-dir/')), 'gitignored dir contents never upload');
  ok(!paths.includes('sub/inner.txt'), 'nested .gitignore honored');
  ok(paths.includes('readme.md') && paths.includes('sub/keep.txt'), 'non-ignored files upload');
  ok(!paths.includes('.env') && !paths.some((p) => p.startsWith('.git/')), '.env and .git/ never upload (dotfile rule)');

  writeFileSync(join(dir, 'secrets.txt'), 'e2e-secret-marker-v2');
  runCli(bin, ['sync', name, '--dir', dir], { env: H });
  paths = await mockPaths(name);
  ok(!paths.some((p) => p.endsWith('secrets.txt')), 'modified gitignored file still never uploads');
}

function findBinary() {
  if (process.env.FRAGMENT_BIN) return existsSync(process.env.FRAGMENT_BIN) ? process.env.FRAGMENT_BIN : null;
  // debug first: `cargo build` (what CI and this suite document) refreshes
  // it, while a stale local release build silently tests old code
  for (const c of ['target/debug/fragment', 'target/release/fragment']) {
    const p = resolve(process.cwd(), c);
    if (existsSync(p)) return p;
  }
  return null;
}

function ownerNpubStub() {
  // the guide's manifest block uses npub1… — any valid-shaped npub works
  // for the commit; the runtime forces name from the registry anyway
  return 'npub1qql65jg4383y7glnpktk3ppxgkppqqqqqqqqqqqqqqqqqqqqqqqs0lwzn';
}

// ---------- cron (slow) ----------
async function cronSection() {
  if (!CRON) {
    if (!ONLY || ONLY === 'cron') console.log('skip  cron fire check is slow — pass --all to include it');
    return;
  }
  if (!section('cron')) return;
  const name = `e2e-cr-${suffix}`;
  await createFragment(name);
  await csCommit(name, [
    { path: 'workflows/tick.mjs', text: 'export async function run(ctx) {\n  await ctx.files.write("ticks/" + Date.now() + ".md", "tick");\n}\n' },
    { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'tick', file: 'workflows/tick.mjs', cron: '* * * * *' }], secrets: [] }) },
  ]);
  const st = await apiJson(name, '/status');
  ok(st?.crons?.[0]?.nextAt, 'status reports nextAt for cron workflow');
  console.log('      waiting up to 75s for the durable alarm to fire…');
  const fired = await waitFor(async () => {
    const evs = await apiJson(name, '/events');
    const s = JSON.stringify(evs?.events || []);
    return s.includes('"kind":"run.succeeded"') && s.includes('tick') ? true : null;
  }, 'cron workflow fired via durable alarm', 75000, 5000);
  ok(fired, 'cron workflow fired via durable alarm');
}

// ---------- gen (the platform `fragment:ai` module over keyed egress) ----------
async function genSection() {
  if (!section('gen')) return;
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
  await createFragment(name);
  await csCommit(name, [
    { path: 'workflows/gen.mjs', text: wf },
    { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [{ name: 'gen', file: 'workflows/gen.mjs' }], secrets: [] }) },
  ]);
  const r = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'gen' }));
  eq(r.body?.ok, true, 'gen workflow runs');
  const out = r.body?.output || {};
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
  const { createHash } = await import('node:crypto');
  for (const [label, f] of [['image', out.image], ['video', out.video]]) {
    if (!f) continue;
    const url = `${BASE}/api/f/${name}/file?path=${encodeURIComponent(f.path)}`;
    const raw = Buffer.from(await waitFor(async () => {
      const r = await fetch(url, { headers: { authorization: await authHeader('GET', url, null, ownerKey) } });
      return r.status === 200 ? r : null;
    }, `${label}: placed file serves over the api plane`, 10000).then((r) => r.arrayBuffer()));
    eq(raw.length, f.size, `${label}: served size matches the descriptor`);
    eq(createHash('sha256').update(raw).digest('hex'), f.sha, `${label}: served bytes hash to the committed sha256`);
  }

  // the example app (templates/gen) live: single-await generate
  const appName = `e2e-gen-app-${suffix}`;
  await createFragment(appName);
  const tplApp = guideRead(new URL('../templates/gen/app.mjs', import.meta.url), 'utf8');
  const tplIdx = guideRead(new URL('../templates/gen/site/index.html', import.meta.url), 'utf8');
  await csCommit(appName, [
    { path: 'app.mjs', text: tplApp },
    { path: 'site/index.html', text: tplIdx },
    { path: 'fragment.json', text: JSON.stringify({ name: appName, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [] }) },
  ]);
  await csDeploy(appName);
  const vt = frags[appName].viewToken;
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

// ---------- runtime lane: stamps, rotation, rooms inspection ----------
async function runtimeLaneSection() {
  if (!section('runtime')) return;
  const mkFrag = async (tag) => {
    const name = `e2e-rt-${tag}-${suffix}`;
    const created = await createFragment(name);
    return { name, created };
  };

  // 1) __rt.js version stamp + header
  {
    const { name } = await mkFrag('stamp');
    await csCommit(name, [
      { path: 'site/index.html', text: '<h1>lane</h1>' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }) },
    ]);
    await csDeploy(name);
    const r = await fetch(`${BASE}/f/${name}/__rt.js`);
    eq(r.headers.get('x-fragment-rt-version'), '1', '__rt.js carries x-fragment-rt-version: 1');
    ok((await r.text()).startsWith('/* fragment rt-client v1 */'), '__rt.js first line is the version stamp');
  }

  // 2) rotate: old tokens die hard, new ones work; scoped rotation holds
  {
    const { name, created } = await mkFrag('rotate');
    const oldInbox = created.inboxToken;
    const oldView = created.viewToken;
    const oldWebhook = created.webhookSecret; // frags[name] aliases this object
    await csCommit(name, [
      { path: 'site/index.html', text: '<h1>lane</h1>' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'link', editors: [], viewers: [], workflows: [], secrets: [] }) },
    ]);
    await csDeploy(name);

    eq((await signed('POST', `/api/f/${name}/rotate`, '{}', strangerKey)).status, 403, 'rotate refuses a roleless stranger');

    const rot = await signed('POST', `/api/f/${name}/rotate`, JSON.stringify({}));
    eq(rot.status, 200, 'rotate with no scopes → 200 rotating all three');
    eq(JSON.stringify(rot.body?.rotated), JSON.stringify(['inbox', 'view', 'webhook']), 'rotated reports inbox+view+webhook');
    frags[name].webhookSecret = rot.body.webhook_secret; // stay current for later deliveries
    ok(rot.body?.inbox_token && rot.body.inbox_token !== oldInbox, 'new inbox_token differs from the original');
    ok(rot.body?.view_token && rot.body.view_token !== oldView, 'new view_token differs from the original');
    ok(rot.body?.webhook_secret && rot.body.webhook_secret !== oldWebhook, 'new webhook HMAC secret differs from the original');

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
    eq(scoped.body?.webhook_secret, rot.body.webhook_secret, 'scoped rotate leaves the webhook secret untouched');
    ok(scoped.body?.view_token !== rot.body.view_token, 'scoped rotate regenerates view_token');
    const emptyScopes = await signed('POST', `/api/f/${name}/rotate`, JSON.stringify({ scopes: [] }));
    eq(JSON.stringify(emptyScopes.body?.rotated), JSON.stringify(['inbox', 'view', 'webhook']), 'empty scopes array defaults to all three');
    const unknown = await signed('POST', `/api/f/${name}/rotate`, JSON.stringify({ scopes: ['bogus'] }));
    eq(unknown.status, 400, 'unknown scope → 400');
    eq(unknown.body?.error, 'unknown scope', 'unknown scope error message verbatim');
  }

  // 3) rooms inspection: list + counts, tail pages ascending, auth gates
  {
    const { name } = await mkFrag('inspect');
    await csCommit(name, [
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }) },
    ]);
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
    await seedRoom('delta grid', 3);
    {
      const ws = await connect('ghost');
      ws.send(JSON.stringify({ type: 'state:set', value: { note: 'no msgs here' } }));
      await sleep(600);
      ws.close();
    }

    const list = await waitFor(async () => {
      const l = await apiJson(name, '/rooms');
      const got = Object.fromEntries((l?.rooms || []).map((r) => [r.room, r.count]));
      return got.alpha === 6 && got.beta === 2 && got['delta grid'] === 3 && got.ghost === 0 ? l : null;
    }, 'rooms listed with counts', 10000, 400);
    ok(!!list, 'rooms list → 200');
    const rooms = list?.rooms || [];
    ok(rooms.some((r) => r.room === 'alpha' && r.count === 6 && typeof r.last_at === 'number'), 'alpha listed with count + last_at');
    ok(rooms.some((r) => r.room === 'ghost' && r.count === 0 && r.last_at === null), 'state-only room listed with count 0, last_at null');
    ok(rooms.every((r, i) => i === 0 || (rooms[i - 1].last_at || 0) >= (r.last_at || 0)), 'rooms sorted by last_at DESC');

    const enc = encodeURIComponent('delta grid');
    const page1 = await apiJson(name, `/rooms/${enc}/messages?limit=2`);
    eq(page1?.room, 'delta grid', 'room name decoded from the path');
    const m1 = page1?.messages || [];
    eq(m1.length, 2, 'limit trims to the newest N');
    ok(m1.every((m, i) => i === 0 || m.id > m1[i - 1].id), 'messages ascending within the page');
    ok(m1.every((m) => m.data && m.data.room === 'delta grid'), 'data is the parsed frame');

    const cursor = m1[0].id;
    const page2 = await apiJson(name, `/rooms/${enc}/messages?limit=100&before=${cursor}`);
    const m2 = page2?.messages || [];
    ok(m2.length >= 1 && m2.every((m) => m.id < cursor), 'before-cursor returns strictly older ids');
    ok(m2.every((m, i) => i === 0 || m.id > m2[i - 1].id), 'older page also ascending');

    const all = await apiJson(name, `/rooms/${encodeURIComponent('alpha')}/messages?limit=9999`);
    eq(all?.messages?.length, 6, 'oversized limit clamps server-side, full room returned');

    eq((await fetch(`${BASE}/api/f/${name}/rooms`)).status, 401, 'unauthenticated rooms list refused');
    eq((await signed('GET', `/api/f/${name}/rooms`, null, strangerKey)).status, 403, 'roleless stranger rooms list → 403');
    eq((await fetch(`${BASE}/api/f/${name}/rooms/${enc}/messages`)).status, 401, 'unauthenticated messages read refused');
    eq((await signed('GET', `/api/f/${name}/rooms/${enc}/messages`, null, strangerKey)).status, 403, 'roleless stranger messages read → 403');
  }
}

// ---------- cli lane: machine envelopes ----------
async function cliLaneSection() {
  if (!section('cli-lane')) return;
  const bin = findBinary();
  ok(!!bin, 'cli binary found for lane/cli checks');
  if (!bin) return;
  const H = { HOME: mkdtempSync(join(tmpdir(), 'fragment-cli-home-')) };
  const env = { ...process.env, FRAGMENT_HOST: BASE, ...H };

  const run = (argv) => {
    const r = spawnSync(bin, argv, { encoding: 'utf8', env, timeout: 90_000 });
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
  ok(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(cr?.data?.repo || ''),
    '(lane/cli) create --json carries the url-form repo identity');

  // status/events/runs envelopes have the data key
  for (const sub of ['status', 'events', 'runs']) {
    const v = parse(run([sub, nm, '--json']).stdout);
    ok(v?.ok === true && v && 'data' in v, `(lane/cli) ${sub} --json envelope has data key`);
  }

  // grant (edit-and-commit of fragment.json) round-trips
  const npub = run(['whoami', '--json']).stdout;
  const meNpub = parse(npub)?.data?.npub;
  const gr = run(['grant', nm, '--editor', meNpub]);
  eq(gr.code, 0, '(lane/cli) grant --editor exits 0');
  ok(String(gr.stdout).includes('roles updated'), '(lane/cli) grant reports the commit');

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

  // rotate (owner-only token rotation)
  const rot = run(['rotate', nm, '--json']);
  const rotJ = parse(rot.stdout);
  ok(rotJ?.ok === true && typeof rotJ?.data?.inbox_token === 'string' && typeof rotJ?.data?.view_token === 'string'
    && Array.isArray(rotJ?.data?.rotated), 'rotate --json returns both tokens under data');

  // rooms listing + message reads
  const rms = parse(run(['rooms', nm, '--json']).stdout);
  ok(rms?.ok === true && Array.isArray(rms?.data?.rooms), 'rooms <name> --json lists rooms');
  const rm1 = parse(run(['rooms', nm, 'general', '--tail', '5', '--json']).stdout);
  ok(rm1?.ok === true && rm1?.data?.room === 'general' && Array.isArray(rm1?.data?.messages),
    'rooms <name> <room> --tail N returns ascending messages');

  // leave no trace (fragment created this session; allowed to rm e2e-cli-*)
  runCli(bin, ['rm', nm], { env: H });
}

// ---------- lane/converge: multi-mirror deletion propagation ----------
// The resurrection bug this pins: a mirror still holding a file the repo
// deleted must not PUSH it back (remote-absence read as "must upload").
async function convergeSection() {
  if (!section('converge')) return;
  const bin = findBinary();
  ok(!!bin, 'cli binary found for lane/converge');
  if (!bin) return;

  const name = `e2e-cv-${suffix}`;
  await createFragment(name);
  const home = mkdtempSync(join(tmpdir(), 'fragment-cv-home-'));
  const env = { ...process.env, HOME: home, XDG_CONFIG_HOME: join(home, '.config'), FRAGMENT_HOST: BASE };
  runCli(bin, ['login'], { env: { HOME: home } });
  const cliNpub = JSON.parse(runCli(bin, ['whoami', '--json'], { env: { HOME: home } })).data.npub;
  await grantEditor(name, cliNpub);

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
  ok(/all \d+ files match the repo/.test(outB3), '[converge] repeat sync is a no-op (no ping-pong)');
  const paths = await mockPaths(name);
  eq(paths.filter((p) => !p.includes('fragment.json')).length, 2, '[converge] repo holds exactly the two live files');

  // modification beats deletion: B edits a file A deletes
  writeFileSync(join(dirB, 'winner.md'), 'modified content wins');
  rmSync(join(dirA, 'winner.md'));
  sync(dirA);
  sync(dirB);
  eq(await mockFileText(name, 'winner.md'), 'modified content wins', '[converge] a modified local copy beats a remote deletion');
}

// ---------- lane/static-root: site/index.html owns the root when present ----------
async function staticRootSection() {
  if (!section('static-root')) return;
  {
    const name = `e2e-sr-${suffix}`;
    await createFragment(name);
    await csCommit(name, [
      { path: 'site/index.html', text: '<!doctype html><html><head><script type="module" src="/app.js"></script></head><body><h1>static root</h1></body></html>' },
      { path: 'site/app.js', text: 'export const n = 41 + 1;\n' },
      { path: 'app.mjs', text: 'export default { async fetch(req) { return new Response(req.method === "POST" ? "app-post" : "app-get", { status: 200 }); } }\n' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }) },
    ]);
    await csDeploy(name);

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
    // no site/index.html: the app keeps the root (single-handler shape)
    const name = `e2e-sr-apponly-${suffix}`;
    await createFragment(name);
    await csCommit(name, [
      { path: 'app.mjs', text: 'export default { async fetch() { return new Response("app-owns-root", { status: 200 }); } }\n' },
      { path: 'fragment.json', text: JSON.stringify({ name, visibility: 'public', editors: [], viewers: [], workflows: [], secrets: [] }) },
    ]);
    await csDeploy(name);
    const root = await fetch(`${BASE}/f/${name}/`);
    eq(await root.text(), 'app-owns-root', '[static-root] app without site/index.html keeps the root');
  }
}

// ---------- lane/build: fragment build (TS strip, hashing, parse gate) ----------
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

  // isolated HOME + login: `fragment build` signs with the local keypair
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

// ---------- main ----------
try {
  if (!ONLY || ONLY === 'lockdown') await lockdownSection();
  if (!ONLY || ONLY === 'auth') await authSection();
  if (!ONLY || ONLY === 'files') await filesSection();
  if (!ONLY || ONLY === 'deploy') await deploySection();
  if (!ONLY || ONLY === 'app') await appSection();
  if (!ONLY || ONLY === 'rooms') await roomsSection();
  if (!ONLY || ONLY === 'workflows') await workflowSection();
  if (!ONLY || ONLY === 'paused') await pausedSection();
  if (!ONLY || ONLY === 'runs') await runsSection();
  if (!ONLY || ONLY === 'guide') await guideSection();
  if (!ONLY || ONLY === 'platform') await platformSection();
  if (!ONLY || ONLY === 'gen') await genSection();
  if (!ONLY || ONLY === 'filesync') await filesyncSection();
  if (!ONLY || ONLY === 'gitignore') await gitignoreSection();
  await cronSection();
  if (!ONLY || ONLY === 'runtime') await runtimeLaneSection();
  if (!ONLY || ONLY === 'cli-lane') await cliLaneSection();
  if (!ONLY || ONLY === 'converge') await convergeSection();
  if (!ONLY || ONLY === 'static-root') await staticRootSection();
  if (!ONLY || ONLY === 'build') await buildLaneSection();
} catch (e) {
  fail++;
  failures.push('unexpected: ' + String(e && e.stack || e));
  console.log('FAIL  unexpected: ' + String(e && e.stack || e));
}

console.log(`\n${pass} passed, ${fail} failed${fail ? ': ' + failures.join('; ') : ''}`);
process.exit(fail ? 1 : 0);
