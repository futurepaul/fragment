# fragment wire contract (CLI ↔ runtime)

Two runtimes answer this contract during phase 2: the Rust cell (`cell/`,
this first part) and the TypeScript runtime (`runtime/`, the second part,
deleted in slice G). The CLI talks to both: every command the Rust cell
keeps has the same route and body, and the cell's errors are
`{"error": "<code>", "message": "..."}` (codes in `crates/proto`).

# The Rust cell (phase 2)

Status: slices B and C (2026-09-23). `cargo xtask e2e` proves every
route below.

## Configuration

Worker variables, rendered from the fleet's settings (ROADMAP decision
13; `cell/.dev.vars` in dev):

| Variable | Meaning |
|---|---|
| `FRAGMENT_HOST_SECRET` | seals secrets at rest (at least 32 bytes); required to create |
| `FRAGMENT_HOST_SECRET_PREVIOUS` | the secret before a rotation; values sealed under it still open |
| `CODESTORAGE_ORG`, `CODESTORAGE_PRIVATE_KEY` | the org and its PKCS#8 P-256 key (a one-line PEM may carry literal `\n`) |
| `CODESTORAGE_API_URL` | the API base (default `https://api.<org>.code.storage`) |
| `FRAGMENT_HOST_SUFFIX` | fragments are served from `<name>.<suffix>`; unset, from `/f/<name>/` |
| `FRAGMENT_POLL_INTERVAL_S` | the webhook backstop (default 300) |

## Principals and access

A principal is a key (64 hex inside, an npub in answers; requests may use
either) or an anonymous visitor (`anon:` + 32 hex, the hash of a random
cookie on the fragment's origin). Control routes need NIP-98 (as the
second part describes); site requests may carry it.

Roles, weakest first: `public`, `viewer`, `editor`, `owner`. A member's
role is their membership. Otherwise visibility decides: on a `public`
fragment everyone holds `public`; on a `public` or `link` fragment the
share link (`?view=<token>`, which sets an HttpOnly cookie on the
fragment's origin) counts as `viewer`; a `members` fragment gives
non-members nothing. A refusal is 401 for an unsigned caller and 403 for
a signed one.

Membership is cell state: `fragment.json`'s `visibility`, `editors`, and
`viewers` grant nothing (the cell records a `manifest.ignored` event).
Only the owner manages members, invites, visibility, and tokens; a
member may leave. Each person's list of fragments is kept in their
`Principal` cell, fed from each fragment's outbox.

## Control API

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/fragments` | any signer | `{name, fragmentSecret, visibility?}` → `{name, npub, owner, visibility, viewToken, inboxToken, webhookSecret, repo, canonical}`. `fragmentSecret` is the fragment's own key, made by the client; it is stored sealed. The cell creates (or, for a name deleted before, finds) the code.storage repo. |
| `GET /api/fragments` | any signer | → `{fragments: [{name, role}]}` |
| `DELETE /api/f/{name}` | owner | → `{ok, deleted}`; the app's database goes too; the repo stays |
| `GET /api/f/{name}/status` | viewer | → `{name, npub, owner, role, visibility, repo, pins: {main, live}, counts: {files, events, members}, code: {sha, operations, error}, viewToken, inboxToken (editor), urls: {canonical}}` |
| `GET /api/f/{name}/manifest` | viewer | → `fragment.json` at main (404 when there is none) |
| `GET /api/f/{name}/members` | viewer | → `{members: [{principal, role, addedBy, addedAt}]}` |
| `PUT /api/f/{name}/members/{npub}` | owner | `{role: viewer\|editor}` → the member |
| `DELETE /api/f/{name}/members/{npub}` | owner, or the member | → `{ok, removed}`; closes that member's change feeds |
| `POST /api/f/{name}/invites` | owner | `{role, uses? (1), ttlS? (7 days, at most 30)}` → `{id, role, usesLeft, expiresAt, createdBy, token}`; the token is shown once |
| `GET /api/f/{name}/invites` | owner | → `{invites: [...]}` without tokens |
| `DELETE /api/f/{name}/invites/{id}` | owner | → `{ok, revoked}` |
| `POST /api/f/{name}/join` | any signer | `{token}` → `{name, role, joined}`; a stronger existing role is kept |
| `PUT /api/f/{name}/visibility` | owner | `{visibility}` → `{ok, visibility}` |
| `POST /api/f/{name}/rotate` | owner | `{scopes?: [inbox, view, webhook]}` → `{ok, inbox_token, view_token, webhook_secret, rotated}`; a new view token closes link holders' feeds |
| `PUT /api/f/{name}/secrets/{KEY}` | editor | raw body (at most 64 KiB) → `{ok, name}`; sealed (AES-256-GCM, key HKDF'd from the host secret and the fragment's npub) |
| `GET /api/f/{name}/secrets` | editor | → `{names}`; values never leave |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor | → `{ok, removed}` |
| `GET /api/f/{name}/storage-token` | editor | → `{token, repo, api, expiresAt}`: ES256, this repo, `git:read`+`git:write`, 15 minutes |
| `POST /api/f/{name}/refresh` | editor | → `{ok, refs: {main: {pin, moved} \| {absent}, live: ...}}` |
| `POST /api/f/{name}/webhook` | code.storage | signed with the fragment's webhook secret (`X-Pierre-Signature`, 5 minutes); validate, remember (redeliveries are acknowledged), then move the pin to the branch's head as read now |
| `GET /api/f/{name}/files` | viewer | → `{ref, files: [{path, size, mode, lastCommitSha, machinery}]}` at main |
| `GET /api/f/{name}/file?path=` | viewer | → the bytes at main (`x-fragment-ref`) |
| `GET /api/f/{name}/file/stat?path=` | viewer | → `{stat: {path, size, blobSha, lastCommitSha, present}, ref}` |
| `GET /api/f/{name}/events?since=` | viewer | → `{events: [{id, at, kind, summary, data}]}` (500 a page; 5000 kept) |
| `POST /api/f/{name}/ops/{op}` | the operation's role | `{id, input}` → `{result, replayed}` |

| `GET /api/f/{name}/channels` | viewer | → `{channels: [{name, read, seq}]}`: `events`, `ops`, and the app's |
| `GET /api/f/{name}/channels/{channel}?after=&limit=` | the channel's reader | → `{channel, records: [{channel, seq, at, principal, kind, body}], next}` (1000 a page) |

## Apps

Code comes from git: when `live` moves, the cell reads `fragment.json`
and `app.mjs` (with `applib/**.mjs|js`, at most 64 modules and 4 MiB in
all) from the live commit. A live commit with an invalid `fragment.json`
keeps the last good code and says why in `status.code.error`.

```json
{
  "operations": {
    "add":  { "kind": "mutation", "role": "editor", "input": { "type": "object", "required": ["text"],
              "properties": { "text": { "type": "string", "maxLength": 200 } }, "additionalProperties": false } },
    "list": { "kind": "query" }
  },
  "channels": { "activity": { "read": "viewer" } },
  "meta": { "title": "Todo", "description": "…", "image": "https://…" }
}
```

- `role` defaults to `viewer` for a query and `editor` for a mutation.
  `fetch` and `alarm` are not operation names.
- `input` is a JSON Schema in a bounded subset (`crates/core/src/schema.rs`:
  types, `enum`, `const`, lengths, ranges, `items`, `properties`,
  `required`, `additionalProperties`, counts; annotations allowed; any
  other keyword is refused at deploy). A call whose input does not fit is
  400 naming the JSON pointer (`input /text: is required`).
- `channels` declares the app's channels and their readers (default
  `viewer`); `events` and `ops` are built in (readers: viewers).

`app.mjs` exports `class App extends DurableObject` with one method per
operation, each called `(input, call)`: `call.principal` (an npub or
`anon:…`) and `call.role`. A mutation is synchronous over the app's own
SQLite; `call.publish(channel, body, kind = "message")` appends a record
(body at most 64 KiB, 64 per mutation) once the mutation commits, and an
exception rolls back its writes and its records (422). The ledger keys a
mutation by (principal, id) for seven days: a retry with the same id
returns the stored result; the same id with another input is 409. Every
applied mutation also appends `{op, id}` to `ops`. An optional
`fetch(request)` answers every path that is not a site file (any
method), with `x-fragment-principal` and `x-fragment-role` set.

## Serving

`<name>.<suffix>/<path>` (or `/f/<name>/<path>` without a suffix; with
one, those redirect to the fragment's host, except `__watch`):

| path | |
| --- | --- |
| `/`, `/<page>` | `site/` in the live commit (`index.html` for directories); Open Graph tags from `fragment.json`'s `meta` |
| `__tree` | `{type, ref: "live", sha, count, files}`, content only |
| `__file?path=` | a content file from live, else main |
| `__preview.svg` | the placeholder preview image |
| `POST __op/{op}` | a browser's call: `application/json` `{id, input}`; an unsigned caller gets an anonymous principal cookie; callers holding only `public` get 60 calls a minute each, 600 per fragment |
| `__fragment.js` | the browser library (below) |
| `__live` | WebSocket, anyone who can see the fragment: channel subscriptions from a cursor, presence, change signals (below) |
| `__watch` | WebSocket, viewers and up (the share link, or a signed upgrade): `{type: "hello", ref, sha}`, then `{type: "changed", ref: "main", sha, paths}` per external move of main |
| anything else | the app's `fetch`, when it has one |

`__live` and `__watch` are also served in place at `/f/<name>/…` for the
CLI. The `__live` protocol (JSON frames):

- client → server: `{type: "subscribe", channel, after}`,
  `{type: "unsubscribe", channel}`, `{type: "presence", data}` (at most
  4 KiB; `null` clears), `{type: "ping"}`
- server → client: `{type: "hello", id, principal, role}`,
  `{type: "record", channel, seq, at, principal, kind, body}`,
  `{type: "subscribed", channel, next, more}` (after the backlog),
  `{type: "presence", list: [{id, principal, data}]}`,
  `{type: "changed", op}` (after every applied mutation),
  `{type: "error", message}`

A socket's role is fixed when it connects. Removing a member closes their
sockets; rotating the share link closes link holders'; a fragment that
stops being public closes its anonymous visitors'.

The browser library (`import * as fragment from "./__fragment.js"`):
`call(op, input, {id?})` (retries keep the id), `live(op, input,
onResult, onError?)` (re-runs a query after every change), `subscribe(
channel, onRecord, {after?})`, `presence.set(data)`, `presence.on(fn)`,
`me()`.

CLI: `fragment call <name> <op> --input '{...}' [--id ID]`, `fragment
channel <name> [<channel>] [--after N] [--follow]`.

# The TypeScript runtime (deleted in slice G)

Base URL: the celld public listener, e.g. `http://127.0.0.1:8789`.

Files live in code.storage (one git repo per fragment, repo name =
fragment name, `main` = working files, `live` = blessed serve point).
The runtime talks to code.storage with JWTs signed by the host's org key
(`CELLD_VAR_PIERRE_PRIVATE_KEY`, PKCS8 PEM; org id
`CELLD_VAR_CODESTORAGE_ORG_NAME`; API base override
`CELLD_VAR_CODESTORAGE_API_URL`). The cell pins `main@SHA` for
working-copy reads and `live@SHA` for serving; the tree index (metadata
only) lives in cell SQLite and refreshes on webhook, poll, and the cell's
own commits.

## Auth — NIP-98 HTTP auth

Control endpoints require header:

```
Authorization: Nostr <base64(JSON event)>
```

Event: kind `27235`, `content: ""`, tags:

- `["u", "<absolute request URL, no fragment>"]`
- `["method", "<uppercase HTTP method>"]`
- `["payload", "<hex sha256 of raw request body>"]` — required iff body non-empty

`created_at` within ±60s of server time. `pubkey` is the x-only secp256k1
key (64 hex chars). `id` = sha256 of the JSON serialization of
`[0, pubkey, created_at, kind, tags, content]` (NIP-01). `sig` = BIP-340
schnorr signature over `id`.

Errors: `401` missing/invalid signature, `403` valid signature but the npub
lacks the role.

Roles per fragment: `owner` (creator; can do everything), `editor`
(everything except owner transfer), `viewer` (read-only control + site when
visibility=viewers).

Fragment identity (level-c): the npub secret is generated CLIENT-side and
supplied at create; the cell stores it wrapped (HKDF+AES-GCM under
`FRAGMENT_HOST_SECRET`, salted by the fragment npub). There is no
server-side generation path.

## Control API (prefix `/api`)

| method & path | role | body → result |
| --- | --- | --- |
| `POST /api/fragments` | any npub | `{name, fragmentSecret}` → `{name, npub, viewToken, inboxToken, webhookSecret, repo, canonical}`. `fragmentSecret` = 64-hex secp256k1 secret generated by the CLI. The cell creates the code.storage repo (idempotent). |
| `GET /api/fragments` | any npub | → `{fragments: [{name, role}]}` (where requester has a role) |
| `GET /api/f/{name}/status` | viewer+ | → `{name, npub, repo, pins: {main, live}, counts, crons:[{name,nextAt,paused}]}` |
| `GET /api/f/{name}/manifest` | viewer+ | → manifest JSON — read from the repo (pinned `fragment.json`), cached in the cell; `manifest-set` is an ordinary commit made by the CLI |
| `GET /api/f/{name}/storage-token` | editor+ | → `{token, repo, api}` — a short-lived (15 min) code.storage JWT scoped to exactly this fragment's repo with `["git:read","git:write"]` (no org scope). `api` is the code.storage API base; all endpoint paths append under it (`/api/repos/{repo}/...`). Every mint writes a `storage-token.minted` ledger event (actor npub, repo, scopes, expiry). The repo claim is built server-side from the validated fragment name and asserted on egress. |
| `POST /api/f/{name}/refresh` | editor+ | `→ {ok, refs: {main, live}}` — re-reads both refs from code.storage and moves the pins (the authenticated equivalent of a push webhook's interpret step; external commits schedule `trigger:"files"` workflows + notify exactly like a delivery). The CLI fires it after landing syncs, deploys, and rollbacks so its own commits are visible immediately instead of waiting out the 5-minute poll backstop. A never-pushed ref reports `{absent: true}`. |
| `GET /api/f/{name}/files` | viewer+ | → `{ref: <pinned main sha>, files:[{path, size, mode, lastCommitSha, machinery}]}` — metadata only, no bytes |
| `GET /api/f/{name}/file?path={p}` | viewer+ | → raw bytes streamed from the pinned main ref (+ `x-fragment-ref`) |
| `GET /api/f/{name}/file/stat?path={p}` | viewer+ | → `{stat: {path, size, sha, lastCommitSha, present}, ref}` — the ifSha read half |
| `POST /api/f/{name}/run` | editor+ | `{workflow, input?}` → `{ok, runId, status, events}` — launches the run on the native Workflows engine, waits (bounded 5 min) for the attempt, then reports from the ledger; the output lands in the `run.succeeded` event |
| `POST /api/f/{name}/replay` | editor+ | `{run: id}` → re-runs a held run with its original input → `{ok, launched, runId}` |
| `GET /api/f/{name}/runs?status=&wf=&limit=&include=input` | viewer+ | → `{runs:[{id, wf, via, status, attempt, maxAttempts, error?, timings, cause}], counts}`. Statuses: `running \| backoff \| success \| held \| skipped \| blocked` |
| `POST /api/f/{name}/pause` | editor+ | `{workflow, paused}` → pause/unpause a workflow (clears the auto-pause breaker) |
| `POST /api/f/{name}/rotate` | owner | `{"scopes": ["inbox","view","webhook"]?}` → `{ok, inbox_token?, view_token?, webhook_secret?, rotated}` (default all three; omitted scopes keep their old values). Callers treat all spellings as opaque. |
| `GET /api/f/{name}/rooms` | editor+ | → `{rooms:[{room, count, last_at}]}` |
| `GET /api/f/{name}/rooms/{room}/messages?limit=&before=` | editor+ | → `{room, messages:[{id, at, sender, data}]}` ascending (most recent `limit`, default 100 max 200; `before=<id>` pages further back) |
| `PUT /api/f/{name}/secrets/{KEY}` | editor+ | raw body = value → `{ok}` — stored WRAPPED at rest (HKDF+AES-GCM under `FRAGMENT_HOST_SECRET`); fails loudly if the host secret is unset |
| `GET /api/f/{name}/secrets` | editor+ | → `{names: [...]}` (never values) |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor+ | → `{ok}` |
| `GET /api/f/{name}/events?since={id}` | viewer+ | → `{events:[{id, at, kind, summary, data?}]}` |
| `POST /api/f/{name}/inbox?t={inboxToken}` | token only | `{source?, payload}` → `{ok, id, scheduled}`; enqueues + schedules `trigger:"inbox"` workflows. Prefer the `x-fragment-inbox-token` header — `?t=` lands in access logs. Optional headers: `x-fragment-hops`/`x-fragment-cause` (over-budget hops refuse with a `cycle.detected` event). Pending cap 1000 → `429`. |
| `POST /api/f/{name}/webhook` | HMAC only | code.storage webhook delivery. Verified with the fragment's `webhook_secret` (`X-Pierre-Signature: t=<unix>,sha256=HMAC-SHA256(secret, "<t>.<body>")`, 5-min freshness). Order is validate → persist (dedupe row + ledger event; redeliveries ack without re-interpreting) → interpret (refresh the pin from the branch HEAD — a late delivery can never regress it; external pushes fire `trigger:"files"` workflows + notify, the cell's own commits never do). |

Deleted at the code.storage hard cut (editors write directly with storage
tokens): `PUT/DELETE /api/f/{name}/file`, `PUT /api/f/{name}/manifest`,
`POST /api/f/{name}/manifest/check`, the draft plane (`POST/GET /drafts`,
`POST /bless`), `/file/history`, `/file/at`. File history is git history;
previews are ephemeral refs; deploy = commit + move `live`; rollback =
re-point `live`.

**Notify-on-change** (manifest `notifyUrls: ["https://…"]`, max 3): external
changes enqueue one message per URL onto the celld Queues binding
(`NOTIFY`); the separate notify-relay deployment consumes and POSTs
`{type:"changed", fragment, sha, paths}` frames (at-least-once, hop headers
stamped so notify loops die at the receiving inbox's cycle guard). Delivery
and retries are queue-owned; `notify.sent`/`notify.enqueue-failed` land on
the ledger.

## Serving (no NIP-98)

| path | behavior |
| --- | --- |
| `GET /f/{name}/...` | serves from the LIVE ref (`live@SHA`). If the repo has `app.mjs`, requests go to its `fetch(req, ctx)`; else static files from `site/` (index.html default, 404 otherwise). By default app data reads (`ctx.files`) ride the working copy (`main@SHA`, code frozen at the live pin, data live); `"freeze": true` pins app reads to the live ref instead. Token visibility: a valid `?view=` mints a `fragview_{name}` cookie so subresources pass the gate. No `live` ref yet → a clear 404 telling you to deploy. |
| `GET /f/{name}/__rt.js` | browser client for rooms (see below). |
| `GET /f/{name}/__tree` | machine-readable tree FROM THE LIVE REF: `{ref:"live", sha, files:[{path,size,mode,lastCommitSha}]}` — content paths only (the fragment's own organs are excluded). Gated exactly like the site. |
| `GET /f/{name}/__file?path=P` | raw file content from the live ref under the same gate. The read API for watchers, feeds and other fragments — a view link is all a reader needs. |
| `WS  /f/{name}/__room/{room}` | realtime room. |
| view token | `visibility:"link"`: append `?view={viewToken}` (mints the cookie). `"viewers"`: NIP-98 header on the GET. `public` needs nothing. |

Canonical subdomains (`<name>.<host>`, when `FRAGMENT_SUBDOMAIN_HOST` is
set) serve the fragment's site. The one carve-out: `/api/f/*` — the control
namespace — is passed through un-rewritten, so a served app can reach its
own inbox, storage-token, and refresh routes same-origin from its
subdomain. Control routes carry their own auth (tokens / NIP-98), which is
why widening reachability this way widens nothing else. An app's OWN
`/api/*` routes are unaffected (they stay on the site plane; don't name an
app route `api/f/…`).

Previews (`fragment deploy --preview`) are ephemeral refs in code.storage;
they are not runtime URLs — the CLI serves/mints them with a storage token.

## Rooms protocol (WS, JSON both ways)

Client → server: `{type:"msg", data}`, `{type:"state:set", value}`,
`{type:"presence", data}`.
Server → client: on join `{type:"hello", state, presence:[...], history:[...last 50]}`
then `{type:"msg", from, data, at}`, `{type:"state", value}`,
`{type:"presence", list}`.

If the fragment ships `rooms.mjs` exporting `onMessage(room, msg, ctx)`, the
cell calls it (via loader isolate) per message; it may return
`{broadcast, state}` to shape what happens.

## Workflow execution (runtime-internal, not for the CLI)

Workflow (`workflows/*.mjs`) and app (`app.mjs`, `rooms.mjs`) code runs in
Worker-Loader isolates. Runs execute on the host's NATIVE WORKFLOWS engine:

- `runs` rows, guards (pause/hops/rate/single-flight), retry
  classification, held, and the circuit breaker stay in the cell
  (`runtime/ts/runs.ts`) — the ledger is the product layer.
- Each attempt is a native Workflow instance (`FragmentWorkflow`, binding
  `WORKFLOWS`, instance id `r<runId>a<attempt>`). Its `step.do("attempt")`
  drives the author body inside the cell over the loopback; the completion
  report is its own `step.do` with bounded `step.sleep` backoff between
  tries; a report that ultimately fails throws and the cell's crash sweep
  reconciles from `instance.status()`.
- Exactly-once commits: `ctx.files.write` lands as a commit-pack with
  expected-parent CAS (`expected_target_sha`), blob-identity dedup, and
  idempotent-replay healing — a replayed or crashed-then-retried step
  commits each logical change exactly once (the flagship test in
  `runtime/test/flagship-replay.test.mjs`).

The cell injects a sibling module `fragment-ctx.mjs` plus plain-JSON env:

- `FRAGMENT_INTERNAL_URL` — e.g. `http://127.0.0.1:8789/__internal`
- `FRAGMENT_RUN_TOKEN` — per-run/per-draft random token (cell validates; sent
  as the `x-fragment-token` header — never a query param, so it stays out of
  access logs)
- `FRAGMENT_HOST_SECRET` — only present when the host sets it; ctx forwards
  it as `x-fragment-host-secret`

`fragment-ctx.mjs` implements `ctx` over fetch against `/__internal`:

- `ctx.http(url, init)` → plain fetch (egress)
- `ctx.files.read / readBytes / write({ifSha}) / delete / ingest / stat / list / index` → `/__internal/files/...`
- `ctx.secrets` → `GET /__internal/secrets/all` (unwrapped in-flight; wrapped at rest)
- `ctx.inbox()` → pending inbox messages; `ctx.inboxAck(ids)`
- `ctx.events.append(kind, data)`, `ctx.log(msg)`
- `ANY /__internal/egress/<host>/<path…>` — keyed egress proxy (run token via
  `x-fragment-token` OR Bearer). Allowlist from the node's keys
  (`CELLD_VAR_FAL_API_KEY` → the FRAGMENT_FAL_BASE host; `CELLD_VAR_OPENROUTER_API_KEY`
  → openrouter.ai); unconfigured hosts fail closed with a 403 naming what IS
  configured.
- `ctx.state` → per-workflow kv via `/__internal/wstate`
- `ctx.push(who, payload)` → Web Push (`/__internal/push/send`)

The native-workflow driver routes (token-gated, `wf-run` scope):
`POST /__internal/wf/attempt` (execute the attempt body) and
`POST /__internal/wf/complete` (apply the outcome through the runs state
machine; duplicate reports are ignored and ledgered).

A workflow file exports `async run(ctx)`. Code limit 64 MiB per loaded
worker; env 1 MiB. Workflow writes cap at 32 MiB per file (explicit,
`WRITE_CEILING`) — bigger media belongs to the CLI's direct commit path,
which streams the same commit-pack chunks client-side.
