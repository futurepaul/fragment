# Phase 2: the core cut (done 2026-09-23)

The TypeScript runtime was replaced by a Rust cell on the model in
`docs/MODEL.md`, and the shell, Node, and Python tooling by `cargo
xtask`, a Rust e2e, and Rust fakes. Every primitive in
`docs/published-fragments.md` is re-expressed and has a Rust check. The
contract is `docs/api.md`; this file is the record of how it got there
and what was decided. The deleted tree is in git history at `35f5e18`.

**Acceptance (ROADMAP), met:** valid, invalid, replay, and
conflicting-body tests for every mutation (e2e `ops`, `schemas`);
restart tests for the supervisor and facet storage (e2e `restart`: a
graceful stop, a SIGKILL, a job sleeping through a SIGKILL); the
published-fragment table green with no gap rows; no `.sh` or `.py` in the
repo. The one gap: CI is written but has never run (no remote yet).

**At the end:** `cargo xtask e2e` passes 444 of 444 checks in 27
sections; `cargo xtask check` is clean (host tests, clippy on host and
wasm). The cell is 5,227 lines of Rust and 539 KB of gzipped wasm. The
cut deleted 22,086 lines (adding 652), 10.8k of them hand-written
TypeScript and JavaScript (the runtime 5.5k, its tests 2.2k, `scripts/`
2.8k, the old templates' code).

## Slices

| Slice | What landed | e2e |
|---|---|---|
| A. Workspace and harness | the Cargo workspace, `crates/proto`, `crates/nip98`, the cell from the spike, `crates/devstack`, `xtask` | 35 |
| B. Identity, membership, the file plane | `crates/core`, the code.storage fake, members, invites, visibility, sealed secrets, pins and webhooks, deploy and sync against the cell | 246 |
| C. Operations, the facet, channels | schemas, `(input, call)`, the effects outbox, channels over `__live`, `__fragment.js`, `App.fetch`, `fragment call` and `channel`, `templates/todo` | 310 |
| D. Jobs, triggers, schedules | jobs as Workflows, runs, cron, channel and file triggers, the inbox channel, egress checks, `templates/inbox` | 371 |
| E. Files and blobs | `this.files`, file effects and job steps with CAS, git-lfs pointers with bytes in R2, blob GC, `templates/notes`, `xtask try` | 415 |
| F. Deliveries and AI | the delivery queue, web push (RFC 8291/8292 in Rust), `notifyUrls`, OpenRouter text, image, and video | 443 |
| G. The cut | deleted `runtime/`, `scripts/`, `deploy/`, `notify-relay/`, the Node packages, and the old templates; the CLI lost `run`, `rooms`, `grant`, `revoke`; a new `fragment guide`; `rotate` leaves the webhook secret unless asked; CI on `cargo xtask` | 444 |

Checkpoint A was approved by Paul; checkpoint C was approved with slice
B's decisions ("no gold-plating before a usable demo"); checkpoint F
passed on Paul's review of D–F ("nothing obviously wrong"), and he
approved slice G. The live OpenRouter call it asked about moved to
phase 3, with the hosted fleet.

## What each old primitive became

| The TypeScript runtime | Now |
|---|---|
| workflows as files, triggers, the runs ledger | `job` operations run as Workflows; triggers in `fragment.json` name an operation; runs keep held, replay, auto-pause, and the hop budget |
| rooms: messages, a persisted document, presence | channels from a cursor, live queries, presence per socket |
| the inbox table, `ctx.inbox` | the `inbox` channel and its trigger |
| `ctx.state` | the app's own SQLite (the facet) |
| editors and viewers in `fragment.json` | members and invites in the cell |
| the event ledger | the `events` channel |
| `ctx.files` | `this.files` reads; writes as mutation effects or job steps; blobs for 1 MiB and up |
| web push, the notify relay | deliveries through a Queue |
| fal image and video | OpenRouter text, image, and video as job steps |
| dynamic `app.mjs` | `App.fetch` in the facet |
| `scripts/`, the JS e2e and mocks | `cargo xtask`, `crates/e2e`, `crates/fakes` |

## Decisions

Each is in `docs/api.md`.

Access (B):
- Only the owner manages members, invites, visibility, and tokens; any
  member may leave; no ownership transfer yet.
- A share link counts as a viewer on `link` and `public` fragments,
  and for nothing on `members` ones.
- `fragment.json`'s `visibility`, `editors`, and `viewers` grant nothing
  (`manifest.ignored`).
- Anonymous visitors are `anon:` + a cookie's hash; browser calls must be
  JSON; `public`-only callers get 60 calls a minute each, 600 per
  fragment.
- With a hostname suffix, `/f/<name>/…` redirects to the fragment's own
  host; every path there is the fragment's, `/api/…` included (E).
- Invites are single-use by default, last 7 days (30 at most), and are
  kept only as hashes.
- Secrets name the host secret that sealed them, so the fleet rotates
  with `FRAGMENT_HOST_SECRET_PREVIOUS`.
- Webhooks stay per fragment, signed with its own secret.
- The manifest and the code follow their pins, so a read that failed in
  an outage is retried by the next refresh.

Operations and channels (C):
- `(input, call)` with `call.publish`; effects apply after the commit and
  are swept after a restart.
- Operation ids belong to their caller; the replay window is seven days.
- Templates use the `public` role until sign-in exists (phase 4).
- Channel records come only from the platform and from mutations; app
  channels keep everything; `events` and `ops` keep 90 days and 10 000.
- The browser library's names: `call`, `live`, `subscribe`,
  `presence.set/on`, `me`.

Jobs (D):
- Author code never runs in the Workflow: the job's body re-runs in the
  facet by deterministic replay, and the Workflow only loops and calls
  back. A step whose kind changed under a run fails it.
- Secrets are opened only at the egress point (`{{NAME}}` in a fetch
  header, the OpenRouter key), never in the facet, a step result, or a
  run.
- A triggered mutation is a one-step run; triggered runs act as the
  fragment's own key with an editor's role.
- The inbox is a channel; its pending records are its unfinished runs
  (1000, then 429).
- Pausing stops triggers, never calls; auto-pause after 5 held runs in
  10 minutes or 120 triggered runs in an hour.
- The hop budget (16) counts through channels, job calls, file commits,
  and other fragments (`x-fragment-hops`).
- A 429, 5xx, timeout, or platform error is retried 4 times with doubling
  delays; anything else is final and catchable.

Files (E):
- Apps read files at `main`; code comes from `live`.
- One loaded worker per fragment: its env holds only its own
  capabilities, and no module state is shared across fragments.
- Mutations write last-writer-wins; jobs compare-and-swap.
- The cell's own commits carry their writer's depth.
- Blob pointers are git-lfs v1 pointers; blobs are at most 256 MiB; only
  the latest versions' bytes are kept (7 days' grace).

Deliveries and AI (F):
- AI spends the fragment's own `OPENROUTER_API_KEY` until people
  connect their own (phase 4); no platform key.
- Images use OpenRouter's `/images` API; a video is a job's steps
  (start, poll every 20 seconds, save).
- Push subscriptions are tagged by `who`; unsubscribing names the
  browser's own endpoint.
- `notifyUrls` stay unsigned, as before.

The cut (G):
- `runs` stays (it lists the new runs); `run` is a call to a job;
  `grant`/`revoke` are `members add`/`rm`; `rooms` are channels.
- The pause body is `{op, paused}` (the `workflow` alias is gone).
- `fragment rotate` renews the inbox token and the share link; the
  webhook secret is code.storage's to know and rotates only when named
  (`scopes: ["webhook"]`), since the cell's default is all three.
- New fragments start from `todo`; the templates are `todo`, `inbox`,
  and `notes`.
- The notes viewer ships as a committed bundle with its source beside it
  (debt ledger).
- fragment.club keeps running the old runtime from
  github.com/futurepaul/fragment, whose `docs/deploy-vps.md` is its
  runbook, until phase 3's cutover.

## Deferred, with owners

- **Phase 3:** how code.storage registers webhooks on the real service
  (per org: one org route that routes by `repository.url`; per repo: the
  cell registers its own at create); an outbound firewall for job egress;
  a live OpenRouter call and a real phone's push; CI's first run.
- **Debt ledger:** spend limits, per-secret host allowlists, resumable
  blob uploads, channel retention, the effects-sweep fault test, the
  browser half of web push, the viewer bundle.
- **When a phase needs them:** cancelling a run, `waitForEvent`
  (approvals, with agents), per-operation retry policy, binary fetch
  bodies, file reads inside mutations, per-file history, the public
  gallery (phase 7).
