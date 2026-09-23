# Phase 2: the core cut

Status: started 2026-09-23 on Paul's go. This is the working plan for
ROADMAP phase 2; the ROADMAP keeps the acceptance, this file keeps the
slices. Update it in the same commit as the work.

## Goal

Replace the TypeScript runtime with a Rust runtime on the model in
`docs/MODEL.md` (operations, channels, live membership, the app facet,
files in git with blob pointers, OpenRouter), and replace the shell, Node,
and Python tooling with Rust (`cargo xtask`, a Rust e2e, Rust fakes).
Every primitive in `docs/published-fragments.md` is re-expressed and
proven.

**Acceptance** (from the ROADMAP): valid, invalid, replay, and
conflicting-body tests for every mutation; restart tests for the
supervisor and facet storage; the published-fragment table green with no
gap rows; no `.sh` or `.py` in the repo; `runtime/`, `scripts/`,
`deploy/`, `notify-relay/`, and the Cloudflare path deleted.

## Target layout

```
Cargo.toml            workspace
cell/                 the celld project: wrangler.jsonc, entry.mjs (the JS
                      shim), platform.mjs (runs in the facet), src/ (Rust)
cli/                  the fragment CLI (existing), gains call/channel/members
crates/proto/         wire types shared by cell, CLI, and e2e: operations,
                      channel records, errors, limits
crates/nip98/         NIP-98 auth: verify in the cell (wasm), sign on hosts
crates/devstack/      starts, stops, and crashes celld dev nodes for xtask
                      and the e2e
crates/e2e/           the Rust e2e: drives the real node, the CLI, a browser
crates/fakes/         code.storage (from cli/src/mockcs.rs), OpenRouter,
                      a web push service
xtask/                cargo xtask build | celld | dev | e2e | check
templates/            todo, vault (reference apps on the new model)
```

`xtask celld` builds celld from `futurepaul/celld` at a pinned commit (the
alarm fix) until a denoland release carries it.

## What each old primitive becomes

| Today (TS runtime) | Phase 2 |
|---|---|
| workflows as files (`workflows/*.mjs`), triggers, runs ledger | `job` operations run as Workflows; triggers in `fragment.json` name an operation; the ledger and job status replace the runs table; held runs, replay, auto-pause, hop budget move onto jobs |
| rooms: messages, persisted document, presence | channels (WebSocket subscribe from a cursor), queries with change signals, presence per socket |
| inbox table, `ctx.inbox` | the `inbox` channel; a webhook appends a record and triggers an operation; 1000 pending, then 429 |
| `ctx.state` | the app's SQL (the facet) |
| editors/viewers in `fragment.json` (git grants) | members and invites in the supervisor; `fragment members` |
| event ledger, `fragment events` | the `events` channel |
| `ctx.files` | a read capability for queries and jobs; writes are mutation effects or job steps; files of 1 MiB or more are pointers to blobs in Tigris |
| web push (`webpush.ts`), notify relay | deliveries through a Queue, with a push-service fake in e2e |
| fal image and video | OpenRouter text, image, video through a platform capability, with an OpenRouter fake in e2e |
| dynamic `app.mjs` routes | the `App` class's optional `fetch` in the facet |
| `scripts/dev`, `build-runtime`, JS e2e, `fal-fake.mjs`, JS code.storage mock | `cargo xtask`, `crates/e2e`, `crates/fakes` |

## Slices

Each slice lands with its tests; the old runtime and `scripts/e2e.mjs`
stay green until slice G deletes them together. **Checkpoints** are where
Paul reviews before the next slice starts.

**A. Workspace and harness.** The Cargo workspace, `crates/proto` (wire
types), `crates/nip98`, `cell/` from the spike (router, supervisor, shim,
`platform.mjs`), `crates/devstack`, `xtask` (`build`, `celld`, `dev`,
`e2e`, `check`), and the first e2e cases: create, install an app, call a
mutation, replay it, conflict it, restart and crash the node.
**Checkpoint A**: the layout and the harness shape.

*Done 2026-09-23.* `cargo xtask check` is clean (host and wasm clippy,
warnings denied); `cargo xtask e2e` passes 35/35 (auth, create, ops,
restart with a graceful stop and a SIGKILL). The cell is 615 KB of wasm
(215 KB gzipped) with NIP-98 verification in `k256`, which agrees with the
CLI's `secp256k1` signer in both directions. Loaded app code is keyed by
the SHA-256 of the platform wrapper plus the author's source, because the
Worker Loader memoizes by id across every fragment in an isolate. `PUT
code` is in the debt ledger until slice B, and the code.storage mock
moves to `crates/fakes` in slice B, where the cell first needs it.
**Checkpoint A approved by Paul (2026-09-23).**

**B. Identity, membership, the file plane.** NIP-98 in the cell;
fragment create/list; members, invites, roles; visibility (`public`,
`link`, `members`) and anonymous principals for `public`-role operations;
secrets wrapped at rest; the code.storage plane (storage tokens,
`main`/`live` pins, the tree index, webhooks and the poll backstop);
deploy, preview, rollback, drafts. e2e: the auth, lockdown, platform,
deploy, and folder-sync lanes against the new cell. The hostname suffix
and the fleet's settings are configuration, never constants (ROADMAP
decision 13).

**C. Operations, the app facet, channels.** Operations declared in
`fragment.json` with input schemas (a bounded JSON Schema subset in Rust);
the facet with `platform.mjs`, the effects outbox and its sweep; channels
(`events`, `inbox`, `ops`, app-declared) with retention; WebSocket
subscriptions from a cursor, presence, change signals for live queries;
the browser library (`fragment.call`, `fragment.subscribe`) that replaces
`__rt.js`; `fragment call` and `fragment channel`; the todo template.
e2e drives a browser. **Checkpoint C**: the author-facing API, reviewed on
the todo app.

**D. Jobs, triggers, schedules.** `job` operations as Workflows whose
steps call back into the supervisor; triggers (cron, inbox webhook, file
change, channel message); schedules multiplexed on the supervisor's
alarm; held runs, replay, auto-pause, the hop budget; secrets for jobs;
the inbox cap.

**E. Files and blobs.** The files capability; CAS; pointers for files of
1 MiB or more with bytes in Tigris (celld's R2 binding); blob deletion by
reachability from branch tips; the CLI resolves pointers when it syncs;
the vault template.

**F. Deliveries and AI.** Web push and outbound webhooks through a Queue;
OpenRouter text, image (`google/gemini-3.1-flash-lite-image`), and video
(`minimax/hailuo-3-max`) with the fake in e2e. The published-fragment
table all green. **Checkpoint F**: before the cut.

**G. The cut.** Delete `runtime/`, `scripts/`, `deploy/`,
`notify-relay/`, `runtime/wrangler.cf.jsonc`, and the old templates;
update `AGENTS.md`, the README, and the docs; the debt ledger drops the
TypeScript-runtime and tooling entries.

## Open inside the phase (decided in the slice that needs them)

- The browser driver for the e2e (slice C): Chrome over CDP from Rust.
- How the CLI keeps working against both runtimes during slices B–F:
  the wire contract stays; commands that only the old model had (`run`,
  `runs`, `rooms`, `grant`) are cut in slice G.
