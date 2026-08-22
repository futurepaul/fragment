#!/usr/bin/env node
// End-to-end suite: the README "What's verified" bullets as executable checks
// against a running fragment host (default http://127.0.0.1:8789).
//
//   node scripts/e2e.mjs [--base URL] [--bin PATH] [--cron] [--only NAME] [--fast]
//
// Bring a stack up first (scripts/dev up && scripts/dev deploy), then run this.
// Exit code 0 = every check passed. Created fragments are named e2e-* and are
// left behind on purpose — there is no destroy command yet.
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { execFileSync } from 'node:child_process';
import { genKey, pubkeyFromSecret, nreq, authHeader } from './nip98.mjs';

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
  try { body = JSON.parse(Buffer.from(await r.arrayBuffer()).toString('utf8')); } catch {}
  return { status: r.status, body };
}

// ---------- identity ----------
const ownerKey = genKey();
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
    name, visibility: 'token', editors: [], viewers: [], workflows: [], secrets: [],
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
  eq(r1.status, 200, 'app.mjs serves all paths');
  eq(await r1.text(), 'hits=1', 'ctx.state counter first hit');
  const r2 = await fetch(`${BASE}/f/${name}/?view=${viewToken}`);
  eq(await r2.text(), 'hits=2', 'ctx.state persists across requests (cached isolate)');

  // drafts are immutable: app writing files must 403 — covered implicitly by
  // design; assert static site/ still reachable when app.mjs exists? No: when
  // app.mjs is present it owns ALL paths (documented). Skip.
}

// ---------- rooms ----------
async function roomsSection() {
  if (!section('rooms')) return;
  const WebSocket = (await import('node:ws').catch(() => null))?.WebSocket ?? globalThis.WebSocket;
  const name = `e2e-ro-${suffix}`;
  const created = await signed('POST', '/api/fragments', JSON.stringify({ name }));
  const viewToken = created.body.viewToken;
  await signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({
    name, visibility: 'token', editors: [], viewers: [], workflows: [], secrets: [],
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
  await sleep(600);
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
    name, visibility: 'token', editors: [], viewers: [],
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
  const post = await fetch(`${BASE}/api/f/${name}/inbox?t=${inboxToken}`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: { x: 1 } }),
  });
  const postBody = await post.json();
  eq(postBody?.ok, true, 'inbox POST accepted');
  ok((postBody?.ran || []).some((r) => r.workflow === 'onpost' && r.ok), 'inbox-triggered workflow ran');

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
    name, visibility: 'token', editors: [], viewers: [],
    workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox', paused: true }],
    secrets: [],
  }));
  eq(man.status, 200, 'manifest accepts paused: true on a workflow');

  // an inbox arrival while paused must NOT run it
  const postResp = await fetch(`${BASE}/api/f/${name}/inbox?t=${inboxToken}`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: {} }),
  });
  const post = await postResp.json();
  eq(postResp.status, 200, 'inbox POST still accepted while paused');
  ok((post?.ran || []).some((r) => r.workflow === 'w' && r.status === 'blocked'), 'paused trigger recorded as blocked, not run');

  // manual run is the maintenance path and must work
  const run = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'w' }));
  eq(run.status, 200, 'manual run works while paused');
  eq(run.body?.output?.fired, true, 'manual run fired the workflow');

  // unpause via the pause route → trigger works again
  const un = await signed('POST', `/api/f/${name}/pause`, JSON.stringify({ workflow: 'w', paused: false }));
  eq(un.status, 200, 'unpause via /pause accepted');
  const post2 = await (await fetch(`${BASE}/api/f/${name}/inbox?t=${inboxToken}`, {
    method: 'POST', body: JSON.stringify({ source: 'e2e', payload: {} }),
  })).json();
  ok((post2?.ran || []).some((r) => r.workflow === 'w' && r.ok && r.status === 'ran'), 'unpaused workflow runs on trigger');
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
    signed('PUT', `/api/f/${name}/manifest`, JSON.stringify({ name, visibility: 'token', editors: [], viewers: [], secrets: [], ...manifest }));
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
    const post = await (await postInbox(name, inboxToken)).json();
    eq(post?.ran?.[0]?.status, 'retrying', 'retryable failure schedules a retry');
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
    const post = await (await postInbox(name, inboxToken)).json();
    eq(post?.ran?.[0]?.status, 'held', 'terminal error holds immediately (attempt 1)');
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
    const st = await signed('GET', `/api/f/${name}/status`);
    ok((st.body?.paused || []).includes('w'), 'breaker auto-paused the workflow');
    const evs = await signed('GET', `/api/f/${name}/events`);
    ok(JSON.stringify(evs.body?.events || []).includes('"kind":"workflow.auto-paused"'), 'workflow.auto-paused event');
    const post6 = await (await postInbox(name, inboxToken)).json();
    eq(post6?.ran?.[0]?.status, 'blocked', 'triggers blocked while auto-paused');
  }

  // rate ceiling: maxRunsPerHour trips auto-pause
  {
    const { name, inboxToken } = await mkFrag('rate', null);
    await putFile(name, 'workflows/w.mjs', 'export async function run(ctx) {\n  return { ok: 1 };\n}\n');
    await putManifest(name, { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox', maxRunsPerHour: 2 }] });
    await postInbox(name, inboxToken);
    await postInbox(name, inboxToken);
    const third = await (await postInbox(name, inboxToken)).json();
    eq(third?.ran?.[0]?.status, 'blocked', 'third auto run in an hour is blocked');
    const st = await signed('GET', `/api/f/${name}/status`);
    ok((st.body?.paused || []).includes('w'), 'rate ceiling auto-paused the workflow');
  }

  // hop budget: over-deep inbox POSTs are refused with cycle.detected
  {
    const { name, inboxToken } = await mkFrag('hops', null);
    await putFile(name, 'workflows/w.mjs', 'export async function run(ctx) {\n  return { ran: true };\n}\n');
    await putManifest(name, { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox' }] });
    const deep = await (await postInbox(name, inboxToken, { 'x-fragment-hops': '99', 'x-fragment-cause': 'other-frag' })).json();
    eq(deep?.ran?.[0]?.status, 'blocked', 'over-budget hops blocked before author code');
    const evs = await signed('GET', `/api/f/${name}/events`);
    ok(JSON.stringify(evs.body?.events || []).includes('"kind":"cycle.detected"'), 'cycle.detected on the ledger');
    const shallow = await (await postInbox(name, inboxToken)).json();
    eq(shallow?.ran?.[0]?.status, 'ran', 'organic-depth POST still runs');
  }

  // inbox idempotency: a redelivered key collapses
  {
    const { name, inboxToken } = await mkFrag('idem', null);
    await putFile(name, 'workflows/w.mjs', 'export async function run(ctx) {\n  return { ok: 1 };\n}\n');
    await putManifest(name, { workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'inbox' }] });
    const first = await (await postInbox(name, inboxToken, { 'idempotency-key': `e2e-${suffix}` })).json();
    const second = await (await postInbox(name, inboxToken, { 'idempotency-key': `e2e-${suffix}` })).json();
    eq(second?.deduped, true, 'redelivered Idempotency-Key collapses');
    eq(second?.id, first?.id, 'same inbox id returned');
    await waitRuns(name, (b) => (b.counts || {}).success === 1, 'exactly one run for two deliveries');
  }

  // workflow imports: Node-style relative specifiers (./sibling, ../lib/x)
  // and map paths must all instantiate — the loader is root-relative only,
  // the runtime rewrites relatives at load time
  {
    const { name } = await mkFrag('imports', null);
    await putFile(name, 'workflows/helper.mjs', 'export const tag = "helper-ok";\n');
    await putFile(name, 'lib/other.mjs', 'export const tag = "lib-ok";\n');
    await putFile(name, 'workflows/w.mjs',
      'import { tag as a } from "./helper.mjs";\nimport { tag as b } from "../lib/other.mjs";\nimport { tag as c } from "workflows/helper.mjs";\nexport async function run(ctx) { return { a, b, c }; }\n');
    await putManifest(name, { workflows: [{ name: 'w', file: 'workflows/w.mjs' }] });
    const run = await signed('POST', `/api/f/${name}/run`, JSON.stringify({ workflow: 'w' }));
    eq(run.body?.output?.a, 'helper-ok', 'relative sibling import works in workflows');
    eq(run.body?.output?.b, 'lib-ok', '../lib import works in workflows (fragment add recipes)');
    eq(run.body?.output?.c, 'helper-ok', 'map-path import still works');
  }

  // single-flight for level-triggered sources: a pending retry absorbs the
  // next sync trigger (skipped), then the retry lands held
  {
    const { name } = await mkFrag('flight', null);
    await putFile(name, 'workflows/w.mjs',
      'export async function run(ctx) {\n  await ctx.http("http://127.0.0.1:9/unreachable");\n}\n');
    await putManifest(name, { debounceMs: 250, workflows: [{ name: 'w', file: 'workflows/w.mjs', trigger: 'sync', retry: { attempts: 2, backoffMs: 4000 } }] });
    await putFile(name, 'data/a.txt', 'one');       // trigger 1 fires at +250ms → fails → backoff
    await sleep(1000);
    await putFile(name, 'data/b.txt', 'two');       // trigger 2 fires into the pending retry → skipped
    const body = await waitRuns(name, (b) => (b.counts || {}).skipped >= 1, 'sync trigger during backoff is skipped', 8000);
    ok((body.counts || {}).skipped >= 1, 'single-flight skip recorded');
    await waitRuns(name, (b) => (b.counts || {}).held >= 1, 'pending retry eventually held', 20000);
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

  // conflict: both sides changed → remote saved as .remote-* copy, local kept
  const cur = await jres(nreq('GET', `${BASE}/api/f/${name}/files`, null, ownerKey));
  const entry = cur.body.files.find((f) => f.path === 'a.txt');
  writeFileSync(join(dir, 'a.txt'), 'alpha LOCAL edit');
  await signed('PUT', `/api/f/${name}/file?path=a.txt&base_rev=${entry.rev}`, 'alpha REMOTE edit');
  cli(['sync', name, '--dir', dir]);
  eq(readFileSync(join(dir, 'a.txt'), 'utf8'), 'alpha LOCAL edit', 'conflict: local kept');
  const conflicts = readdirSync(dir).filter((f) => f.startsWith('a.txt.remote-'));
  ok(conflicts.length === 1, `conflict: remote copy preserved as ${conflicts[0] || '???'}`);
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
    name, visibility: 'token', editors: [], viewers: [],
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

// ---------- main ----------
try {
  if (!ONLY || ONLY === 'auth') await authSection();
  if (!ONLY || ONLY === 'files') await filesSection();
  if (!ONLY || ONLY === 'drafts') await draftsSection();
  if (!ONLY || ONLY === 'app') await appSection();
  if (!ONLY || ONLY === 'rooms') await roomsSection();
  if (!ONLY || ONLY === 'workflows') await workflowSection();
  if (!ONLY || ONLY === 'paused') await pausedSection();
  if (!ONLY || ONLY === 'runs') await runsSection();
  if (!ONLY || ONLY === 'sync') await syncSection();
  await cronSection();
} catch (e) {
  fail++;
  failures.push('unexpected: ' + String(e && e.stack || e));
  console.log('FAIL  unexpected: ' + String(e && e.stack || e));
}
console.log(`\n${pass} passed, ${fail} failed${fail ? ': ' + failures.join('; ') : ''}`);
process.exit(fail ? 1 : 0);
