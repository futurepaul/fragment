# fragment wire contract (CLI ↔ runtime)

Base URL: the celld public listener, e.g. `http://127.0.0.1:8789`.

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

## Control API (prefix `/api`)

| method & path | role | body → result |
| --- | --- | --- |
| `POST /api/fragments` | any npub | `{name}` → `{name, npub, viewToken, inboxToken}` |
| `GET /api/fragments` | any npub | → `{fragments: [{name, role}]}` (where requester has a role) |
| `GET /api/f/{name}/status` | viewer+ | → `{name, npub, blessed, drafts, counts, crons:[{name,nextAt}]}` |
| `GET /api/f/{name}/manifest` | viewer+ | → manifest JSON |
| `PUT /api/f/{name}/manifest` | editor+ | manifest JSON → `{ok}` |
| `GET /api/f/{name}/files?since_rev={n}` | viewer+ | → `{rev, files:[{path, rev, size, sha256, deleted}]}` (all when since=0; `deleted:true` for tombstones) |
| `GET /api/f/{name}/file?path={p}` | viewer+ | → raw bytes + header `x-fragment-rev` |
| `PUT /api/f/{name}/file?path={p}&base_rev={n}` | editor+ | raw body → `{path, rev}`; `409 {currentRev}` if base_rev stale |
| `DELETE /api/f/{name}/file?path={p}` | editor+ | → `{ok}` (writes tombstone) |
| `POST /api/f/{name}/drafts` | editor+ | `{note?}` → `{slug, url}` (slug = 8 unguessable chars) |
| `GET /api/f/{name}/drafts` | viewer+ | → `{drafts:[{slug, at, note, blessed}]}` |
| `POST /api/f/{name}/bless` | editor+ | `{slug}` → `{ok, url}` |
| `POST /api/f/{name}/run` | editor+ | `{workflow, input?}` → `{ok, output?, error?, runId, events}` |
| `POST /api/f/{name}/replay` | editor+ | `{run: id}` → re-runs a held run with its original input → `{ok, output?, error?, runId}` |
| `GET /api/f/{name}/runs?status=&wf=&limit=&include=input` | viewer+ | → `{runs:[{id, wf, via, status, attempt, maxAttempts, error?, timings, cause}], counts}`. Statuses: `running \| backoff \| success \| held \| skipped \| blocked` |
| `POST /api/f/{name}/pause` | editor+ | `{workflow, paused}` → pause/unpause a workflow (clears the auto-pause breaker) |

**Notify-on-change** (manifest `notifyUrls: ["https://…"]`, max 3): every
mutation POSTs `{type:"changed", fragment, rev, paths}` to each URL,
coalesced per URL with 3 retries — push for fragments that can't hold a
socket. Frames carry the hop budget (`x-fragment-hops: 1`), so notify
loops die at the receiving inbox's cycle guard. `notify.sent` /
`notify.failed` land on the event ledger.
| `PUT /api/f/{name}/secrets/{KEY}` | editor+ | raw body = value → `{ok}` |
| `GET /api/f/{name}/secrets` | editor+ | → `{names: [...]}` (never values) |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor+ | → `{ok}` |
| `GET /api/f/{name}/events?since={id}` | viewer+ | → `{events:[{id, at, kind, summary, data?}]}` |
| `POST /api/f/{name}/inbox?t={inboxToken}` | token only | `{source?, payload}` → `{ok, id, ran}`; enqueues + runs `trigger:"inbox"` workflows. Prefer the `x-fragment-inbox-token` header when you control the client — `?t=` lands in access logs. Optional headers: `Idempotency-Key` (redelivery collapses for 24h), `x-fragment-hops`/`x-fragment-cause` (stamped by `ctx.http` — over-budget hops are refused with a `cycle.detected` event). Pending cap 1000 → `429`. |
| `PUT /api/f/{name}/file?path=&base_rev=` | editor+ | raw body; stale `base_rev` → 409. Successful writes schedule `trigger:"sync"` workflows (coalesced; workflow-plane writes don't) |

## Serving (no NIP-98)

| path | behavior |
| --- | --- |
| `GET /f/{name}/...` | blessed draft of `{name}`. If draft contains `app.mjs`, requests go to its `fetch(req, ctx)`; else static files from `site/` (index.html default, 404 otherwise). Manifest `"liveFiles": true` switches the app's `ctx.files` reads from the draft snapshot to the live working copy (code frozen, data live). Token visibility: a valid `?view=` mints a `fragview_{name}` cookie so subresources (module imports, css, images) pass the gate. |
| `GET /d/{slug}/...` | same, for one draft snapshot. |
| `GET /f/{name}/__rt.js` | browser client for rooms (see below). |
| `GET /f/{name}/__tree` | machine-readable tree: `{files:[{path,size,updatedAt,rev,sha256}]}`, machinery excluded. Gated exactly like the site (public / `?view=` token / NIP-98). Same form at `/d/{slug}/__tree` for the snapshot. |
| `GET /f/{name}/__file?path=P` | raw file content under the same gate; machinery paths refused. The read API for watchers, feeds and other fragments — a view link is all a reader needs. |
| `WS  /f/{name}/__room/{room}` (also `/d/{slug}/__room/{room}`) | realtime room. |
| view token | when manifest `visibility:"token"`, append `?view={viewToken}`. When `"viewers"`, NIP-98 header on the GET. `public` needs nothing. Drafts are unguessable-slug public by default, plus the same visibility rule if a `view` token / NIP-98 is present it is ignored. |

## Rooms protocol (WS, JSON both ways)

Client → server: `{type:"msg", data}`, `{type:"state:set", value}`,
`{type:"presence", data}`.
Server → client: on join `{type:"hello", state, presence:[...], history:[...last 50]}`
then `{type:"msg", from, data, at}`, `{type:"state", value}`,
`{type:"presence", list}`.

If the fragment ships `rooms.mjs` exporting `onMessage(room, msg, ctx)`, the
cell calls it (via loader isolate) per message; it may return
`{broadcast, state}` to shape what happens.

## Workflow loopback (runtime-internal, not for the CLI)

Workflow (`workflows/*.mjs`) and app (`app.mjs`, `rooms.mjs`) code runs in
Worker-Loader isolates. The cell injects a sibling module `fragment-ctx.mjs`
plus plain-JSON env:

- `FRAGMENT_INTERNAL_URL` — e.g. `http://127.0.0.1:8789/__internal`
- `FRAGMENT_RUN_TOKEN` — per-run/per-draft random token (cell validates; sent
  as the `x-fragment-token` header — never a query param, so it stays out of
  access logs)
- `FRAGMENT_HOST_SECRET` — only present when the host sets it; ctx forwards
  it as `x-fragment-host-secret`

`fragment-ctx.mjs` implements `ctx` over fetch against `/__internal`:

- `ctx.http(url, init)` → plain fetch (egress)
- `ctx.files.read(path) / write(path, bytes) / list(prefix)` → `/__internal/files/...`
- `ctx.secrets` → lazy proxy, `GET /__internal/secrets/{KEY}`
- `ctx.inbox()` → pending inbox messages
- `ctx.events.append(kind, data)`, `ctx.log(msg)`
- `ctx.ai(prompt, {model?})` → `POST /__internal/infer` (host adds the platform key from `CELLD_VAR_OPENROUTER_API_KEY`; default model `deepseek/deepseek-v4-flash-0731`... configurable via `CELLD_VAR_FRAGMENT_AI_MODEL`)
- `ctx.state` → per-workflow kv via `/__internal/wstate`

A workflow file exports `async run(ctx)`. The cell invokes it via
`loader.get(fragmentName + ":" + workflow + ":" + rev, getCode)` and a single
RPC call. `app.mjs` default-exports `{fetch}`; `rooms.mjs` exports
`onMessage`. Code limit 64 MiB per loaded worker; env 1 MiB.

**First spike to verify before building everything else**: a loaded worker can
fetch `http://127.0.0.1:8789/...` (loopback egress). If that fails, stop and
report — the whole ctx design hinges on it.
