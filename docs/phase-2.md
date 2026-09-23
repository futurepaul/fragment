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

*Done 2026-09-23* (Paul away; its decisions below were approved with
checkpoint C). `cargo xtask e2e` passes 246 of
246 checks in 14 sections (auth, create, lockdown, members, secrets,
files, deploy, ops, public, site, watch, sync, restart, pathmode) against
a real node, the real CLI, and the code.storage fake; `cargo xtask check`
is clean. The old JS e2e still passes against the TypeScript runtime with
the changed CLI. The contract is in `docs/api.md` (first part).

What landed: `crates/core` (the cell's pure logic, host-tested: access,
`fragment.json`, npub, sealed secrets, ES256 storage tokens, webhook
signatures, serving helpers, the public-call rate limit);
`crates/fakes` (a Rust code.storage fake, ported from the JS mock with
the CLI mock's test hooks: url-form repo identity, ES256 JWT checks,
per-commit trees, merges and restore commits, signed push webhooks; the
CLI's unit tests run on it and `cli/src/mockcs.rs` is a 20-line shim);
the cell's members, invites, visibility, secrets, file plane, serving,
and a `Principal` cell per key that indexes "my fragments"; the CLI's
`members`, `invite`, `join`, `visibility`, and `create --visibility`
(and `grant`/`revoke` become member changes against the Rust cell);
`cargo xtask dev` runs the fake beside the node with fragments at
`<name>.fragment.localhost:8790`. `PUT code` is gone (e2e proves 404):
code installs from the live commit. The cell is 1.06 MB of wasm (375 KB
gzipped; P-256, AES-GCM, and `url` added 160 KB gzipped).

Decisions made in B (each in `docs/api.md`):

- **Only the owner manages access**: members, invites, visibility, and
  token rotation. Editors can write files and secrets but cannot widen
  who sees the fragment. Any member may leave; the owner cannot (no
  ownership transfer yet).
- **A share link counts as a viewer on `link` and `public` fragments**
  (MODEL said `link`; on `public` it lifts a visitor from `public` to
  `viewer`). On `members` fragments it counts for nothing.
- **Operation ids belong to their caller**: the facet ledger keys a
  mutation by principal and id, so one caller can neither replay nor
  block another's id (anonymous visitors made this matter).
- **Anonymous visitors** get an HttpOnly, SameSite=Lax cookie on the
  fragment's origin; their principal is `anon:` + a hash of it. Browser
  calls must be `application/json` (no cross-site forms). Callers holding
  only `public` get 60 calls a minute each, 600 per fragment, in memory.
- **With a hostname suffix, `/f/<name>/…` redirects** to the fragment's
  own host (fragments sharing one origin could act as each other's
  visitors); `__watch` stays there for the CLI.
- **Invites** are single-use by default, last 7 days (30 at most), show
  their token once, and keep only its hash.
- **Secrets name their host secret** (`w1.<key id>.…`), so the fleet
  rotates with `FRAGMENT_HOST_SECRET_PREVIOUS`; sealed per cell with
  HKDF(host secret, the cell's npub).
- **`fragment.json`'s `visibility`, `editors`, `viewers` grant nothing**;
  the cell records a `manifest.ignored` event when they appear.
- **Webhooks stay per fragment** (signed with its own secret, the old
  contract). How code.storage registers them in production is open
  (below).
- **The manifest and the code follow their pins**: each records the
  commit it was read from, so a read that failed during a code.storage
  outage is retried by the next refresh, webhook, or poll (e2e `ops`
  drives an outage through the fake).
- **Deferred**: the public gallery (`/api/gallery`) to phase 7 (sharing);
  `notifyUrls` to slice F (deliveries); `App.fetch` routes to slice C.

**C. Operations, the app facet, channels.** Operations declared in
`fragment.json` with input schemas (a bounded JSON Schema subset in Rust);
the facet with `platform.mjs`, the effects outbox and its sweep; channels
(`events`, `inbox`, `ops`, app-declared) with retention; WebSocket
subscriptions from a cursor, presence, change signals for live queries;
the browser library (`fragment.call`, `fragment.subscribe`) that replaces
`__rt.js`; `fragment call` and `fragment channel`; the todo template.
e2e drives a browser. **Checkpoint C**: the author-facing API, reviewed on
the todo app.

*Done 2026-09-23.* **Checkpoint C approved by Paul (2026-09-23)**, with
slice B's decisions: keep `(input, call)` and `call.publish`; templates
use the `public` role until sign-in exists (no solving later phases
early); the browser library's names stand; channels and the replay window
as below. His direction: no gold-plating before a usable demo. `cargo xtask e2e`
passes 310 of 310 checks in 20 sections (slice C added schemas, channels, live,
routes, cli, browser; the browser section drives headless Chrome over
the DevTools protocol, two tabs on the todo template). What landed:

- **Operations**: `fragment.json` declares each operation's kind, role,
  and input schema (a bounded JSON Schema subset, `crates/core/src/
  schema.rs`, refused at deploy when it steps outside); inputs are checked
  before the app runs and a refusal names the JSON pointer.
- **The call context**: every method gets `(input, call)`: who is calling
  (`call.principal`, `call.role`) and, in a mutation, `call.publish(
  channel, body, kind)`. Published records are the mutation's effects,
  kept in its ledger row, applied after the commit keyed by (ledger id,
  index), and swept after a restart. The ledger keeps a mutation seven
  days.
- **Channels**: `events` (the audit trail moved here), `ops` (one record
  per applied mutation), and app channels declared with a reader role.
  Read a page over the API or live over `__live` from a cursor, with
  presence and a change signal after every mutation.
- **The browser library** `./__fragment.js`: `call`, `live`,
  `subscribe`, `presence`, `me`, over one reconnecting socket.
  The old JS e2e still passes 312 of 312 against the TypeScript runtime.
- **Custom routes**: `App.fetch` answers every path that is not a site
  file; `applib/` modules load with `app.mjs`.
- **CLI**: `fragment call` and `fragment channel [--follow]`; `fragment
  init` refreshes the cell so code installs at once.
- **The todo template** (`templates/todo`, `fragment new --template
  todo`).

**Checkpoint C: what to review** (the author-facing API, on
`templates/todo`):

1. The shape of an app: `fragment.json` (operations, roles, schemas,
   channels) plus an `App` class with one method per operation. Is
   `(input, call)` with `call.publish` the right surface, or should
   effects be returned (`return { result, publish: [...] }`)?
2. Roles on the todo template are `public` (anyone who can open it can
   change it) because browsers cannot act as members until sign-in
   (phase 4). Keep that default for templates, or keep `editor` and
   accept that the template is read-only in a browser until phase 4?
3. The browser library's names: `call`, `live`, `subscribe`,
   `presence.set/on`, `me`.
4. Channel records are written only by the platform and by mutations
   (never directly by clients), and app channels keep their records
   forever (MODEL); `events` and `ops` keep 90 days and 10 000 records.
5. A mutation's replay window is seven days (the facet's ledger is pruned
   after that, keeping the 16 MiB facet budget for the app).

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

- How code.storage webhooks are registered on the real service (the docs
  describe deliveries, not registration). If registration is per org,
  phase 3 adds one org route that verifies the org's secret and routes by
  `repository.url`; if per repo, the cell registers its own at create.

- The browser driver for the e2e (slice C): Chrome over CDP from Rust.
- How the CLI keeps working against both runtimes during slices B–F:
  the wire contract stays; commands that only the old model had (`run`,
  `runs`, `rooms`, `grant`) are cut in slice G.
