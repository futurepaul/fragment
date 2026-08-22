# fragment

A fragment is a folder of files, a SQLite database, some URLs, and an inbox —
wrapped around exactly one problem. This repo holds two things:

- **`fragment`** — a Rust CLI (`cli/`). The whole control surface, designed to
  be driven by agents (`fragment guide` prints the agent doc).
- **the runtime** (`runtime/`) — a JavaScript Worker+Durable Object bundle that
  runs on [celld](https://celld.dev), self-hosted Durable Objects that keep
  their state in a bucket you own. Every fragment is one cell with its own
  SQLite; cells sleep when idle and cost nothing asleep.

## Vaults and dropzones (recipes, not modes)

`fragment new --template` scaffolds two compositions from ordinary parts:

- **vault** — any folder of text files becomes a live, URL-bearing,
  Obsidian-like site: file tree, `[[wikilinks]]`, syntax-highlighted code
  (via [@pierre/diffs](https://diffs.com)), a recent rail. The viewer is
  code (frozen in the blessed draft); notes are data (live via
  `"liveFiles": true` — synced edits appear on reload, no republish).
- **dropzone** — the vault plus a `drop/` folder and an `ingest` workflow
  (`trigger: "sync"`): drop a file, watch `output/` appear on the webview
  and sync back into your folder seconds later.

Both are just folders — read the scaffolded code, edit it, re-bless. See
the Recipes section of `fragment guide`.

Read [ARCHITECTURE.md](ARCHITECTURE.md) for the design and
[docs/api.md](docs/api.md) for the wire contract. MIT licensed — see
[LICENSE](LICENSE).

## Install

Grab a prebuilt binary from the [releases page](../../releases)
(macOS arm64, Linux x86_64), put it on your `PATH`, then:

```
fragment host https://fragment.club   # point at a host (once)
fragment login                        # generate your nostr keypair (once)
fragment guide                        # the full manual — start here
```

Or build from source (Rust 1.75+):

```
git clone https://github.com/futurepaul/fragment
cd cli && cargo install --path .
```

## Quickstart (local, no cloud account, no docker)

```
scripts/dev up        # azurite (bucket emulator, npm) + celld on :8789
scripts/dev deploy    # build + deploy the runtime
fragment login        # generate your nostr keypair (once)
fragment create hello
mkdir hello && cd hello
# …add site/index.html, workflows/*.mjs, manifest.json…
fragment sync hello --dir .
fragment publish hello --note v1
fragment bless hello <slug>
fragment open hello   # prints URLs incl. the ?view= token
```

Dev state lives in `.dev/` (delete it to reset the world). For a real
server, [docs/deploy-vps.md](docs/deploy-vps.md) is the production runbook
(celld node + real bucket + Caddy); nothing else changes.

## Deploy to Cloudflare

The same runtime bundle also deploys to Cloudflare Workers (paid plan — the
Worker Loader is the one exotic binding). Same code, different bindings:

```
cd runtime && npx wrangler login        # once, browser OAuth
../scripts/deploy-cf deploy             # deploys + bakes in the worker URL
../scripts/deploy-cf secret             # OPENROUTER_API_KEY from .env, plus
                                        # FRAGMENT_HOST_SECRET (generated) that
                                        # locks /__internal to ctx loopback
fragment --host https://<you>.workers.dev create hello
```

The CF footprint is deliberately tiny: Workers + Durable Object SQLite + the
Worker Loader. No D1, no R2, no KV — one stateful primitive, one consistency
model. Verified live (2026-08-20, wrangler 4.93.1): create/sync/publish/bless,
token-gated serving, rooms over wss (presence + state + history), dynamic
`app.mjs` in a loaded isolate, and workflows via the Worker Loader with the
ctx loopback over the public hostname — including state persistence across
runs.

Two honest caveats:

- **The Worker Loader is beta on CF.** Its module rules differ slightly from
  celld's (string modules must be `.js`/`.py`; we wrap as `{js: …}` objects,
  selected by the `FRAGMENT_HOST_KIND=cf` var). If CF changes the API, that
  var is the single place to adapt.
- **No backup export on CF.** DO storage has no dump API; on celld your
  bucket *is* the backup (`sqlite3`-readable). If exportable state matters to
  an org, that's the argument for the celld host.

## What's verified

These claims are executable: `scripts/e2e.mjs` runs them against a live host
and fails loudly if any regress.

```
scripts/dev up && scripts/dev deploy
node scripts/e2e.mjs        # add --all for the slow cron-fire check
```

Snapshot from the last full manual pass (2026-08-22, celld v0.4.2, macOS,
196 checks incl. the failure leg, sync v2, the machine-read/notify
plane, and the guide's executable patterns):

- NIP-98 auth end-to-end: create/list/status with signed requests; unsigned →
  401; valid signature without role → 403. Rust CLI signatures verify against
  the JS runtime (@noble/curves) and vice versa (cross-implementation pinned
  npub test).
- Files: PUT/GET/DELETE with per-file `base_rev`; stale base → 409.
- Drafts/bless: publish → `/d/<slug>/` serves; bless → `/f/<name>/` serves;
  token-gated canonical (`?view=`); draft rollback = bless an older slug.
- Dynamic apps: a draft with `app.mjs` serves computed responses in a loader
  isolate, with a persistent counter via `ctx.state`.
- Rooms: two websocket clients exchange messages; `state:set` persists; a
  third client reconnecting gets `hello` with state + history; room without
  the view token → 403; `rooms.mjs` errors → `room-error` events, drops →
  `room-drop` events (both visible in `fragment events`).
- Workflows: manual `run` (ctx.files/state/log all work); **cron fires via
  durable alarm — including after the node was SIGKILLed and failed over
  (~20s, per celld's documented failover), and after graceful restarts**;
  inbox POST with token runs the inbox-triggered workflow; bad token → 403.
- Sync: push, pull, remote-delete propagation, wrong-dir guard, and the
  conflict path (both sides changed → if the contents are identical it
  auto-resolves by adopting the remote rev; otherwise the remote copy is
  saved as `<path>.remote-<ts>`, local kept, reported).
- Secrets: set/list/rm via CLI; values never leave the cell except into
  workflow isolates at run time.
- Grants: grant viewer → dev key can read; revoke → 403.
- The loader loopback spike: a Worker-Loader isolate can fetch the host's own
  listener (this is what makes `ctx.*` possible).

## Contract decisions worth knowing

- **Workflows run from the working copy** (the live folder). **Sites serve
  from draft snapshots.** `app.mjs`/`rooms.mjs` come from the served draft.
- `base_rev` is **per file** (new file → 0; re-uploading a deleted file → the
  tombstone's rev, which the files listing reports).
- Cron subset: 5 fields, `*` lists ranges steps, month/day names; no
  `L W # ?`; day-of-week 1=Sunday..7=Saturday (0 refused) — matches celld.
- `manifest.json` in the folder is just a file. The live manifest changes only
  via `fragment manifest-set`. (Sync carries it as data, nothing more.)
- Drafts are immutable; `ctx.files.write` from `app.mjs` (draft scope) → 403.

## celld alpha caveats observed

- `celld deploy` requires a **node restart** to take effect (`scripts/dev
  deploy` handles it).
- Graceful shutdown can stall past its 25s drain deadline when a cell holds
  hibernated websockets. Dev handles it with a force-kill fallback in
  `scripts/dev`; for production rollouts, expect connected rooms clients to
  ride out a failover (the `__rt.js` client auto-reconnects).
- After a hard kill, cells show "owner unreachable" until the lease expires
  (~20s), then restore from the bucket with zero acknowledged writes lost.
- Not safe for hostile multi-tenant use (celld's own security page says so).
  One fleet = one trust domain.
- **The watch dir is paired to the bucket.** celld keeps local cell state in
  `CELLD_WATCH`; if you point a node with an old watch dir at a fresh/empty
  bucket, stale local state can shadow the bucket and resurrect *zombie
  cells* (observed: a cell from a dead bucket world came back owned by its
  old key, and its bucket chain then failed restore — `RestoreFailed` —
  bricking the name). `scripts/dev up` enforces the pairing: a fresh azurite
  data dir wipes the watch dir, and it warns on a foreign azurite. In this
  dev world the name `hello` is currently bricked by exactly this bug (left
  as a monument; every other fragment is healthy).

## Layout

```
ARCHITECTURE.md      the design, in plain language
docs/api.md          CLI ↔ runtime wire contract (NIP-98, endpoints, rooms, ctx)
docs/deploy-vps.md   production runbook: VPS + bucket + Caddy
deploy/              systemd unit, Caddyfile, env template for the above
cli/                 the Rust CLI (fragment) + GUIDE.md (agent doc)
runtime/             the celld deployment (router + FragmentCell)
scripts/dev          up | down | deploy | status | logs
scripts/req.mjs      dev NIP-98 request helper (node)
scripts/rooms-test.mjs
```
