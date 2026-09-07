# Git-backed fragments

Thought experiment, evaluated against fragment 0.7.0 on 2026-09-07 after
asking: if we were "git repo"-oriented from day 1 instead of physical-folder
oriented — with [code.storage](https://code.storage) as the candidate
backend — what changes, what stays, how much home-rolled sync code deletes,
and which features get easier or harder?

The answer is lopsided in an interesting way: almost everything you'd delete
is on the CLI/transport side, and almost everything that stays is the cell.

## The model swap in one sentence

Today the folder is the source of truth — `.fragment/state.json` sits beside
the files, the cell is a mirror, and a home-rolled content sync keeps them
aligned. Git-first inverts it: the **repo is the truth**, the folder is a
working copy, and the cell becomes a **materialized reader** — a webhook
wakes it, it pulls, it serves from SQLite as it does today. Compute and
serving don't move; only transport and identity do.

## What exists today (the deletion inventory)

The home-rolled sync layer, by file:

- **`sync.rs`** (1,095 lines) — sha256+mtime stat cache in
  `.fragment/state.json`, push/pull/mirror modes, conflict markers/copy,
  the mass-deletion guard, the root-identity (`dev`/`ino`) check that
  refuses to sync against a moved/replaced folder.
- **`blob.rs`** (684 lines) — the two-tier transport: ≤64 KiB inline,
  oversized files hard-fail with guidance rather than silently falling back,
  unless a `blobsd` fs tier is configured.
- **`watch.rs`** (248 lines) — OS file events + the live channel + 60s
  reconciliation sweeps, atomic writes via `.fragment-partial`.
- **`blobsd`** — a separately deployed native service, in the runbook.
- **`runtime/src/blob-tier.js`** — the cell's half of the blob tier.

That is ~2,000 of the CLI's ~4,500 lines — call it 40% — plus one deployed
service.

## What the git version deletes

Each home-rolled part maps onto a git primitive for free:

| Today | Git equivalent |
|---|---|
| stat cache (sha256+mtime in state.json) | git index |
| content addressing by sha256 | the object store |
| root identity (`dev`/`ino` check) | the `.git` dir |
| blob two-tier + `blobsd` + runtime blob-tier | LFS — or plain git, which has no 64 KiB cliff |
| conflict markers/copy (no common ancestor) | merge-base, real three-way merges |
| mass-deletion guard | delete detection + a policy on the commit |
| drafts / bless / rollback | ephemeral refs + promote (see below) |

`sync.rs` shrinks to a ~250–400 line commit/push/pull/materialize shim over
git2-rs or the code.storage SDK. `blob.rs` and `blobsd` go almost entirely —
the biggest operational win is deleting a whole native service from the
deploy runbook. `watch.rs` halves: the filesystem-watching half stays (git
doesn't watch folders), and the 60s-sweep reconciliation half can become
webhook-fed instead of polled.

## What stays the same — the important part

The cell is compute and serving, not storage, so nothing users touch moves:
celld, `app.mjs`, workflows, rooms, crons, the manifest, the inbox, the
events log, templates, and the one-fragment-one-sleeping-cell economics.
Nostr identity stays for the discovery/social layer.

One deliberate design choice: **keep the cell materialized** (webhook →
wake → pull → SQLite, serve from SQLite exactly as today). The alternative —
serving reads directly from git objects inside the JS isolate — means
isomorphic-git (or a pack parser) in a Durable Object, which is the riskiest
possible version of this migration. Materializing preserves today's read
latency and keeps the cell serving when the git backend is down.

## What code.storage specifically offers

Managed git infrastructure aimed at agents: a server-side commit builder
(you can commit without a local clone — `addFileFromString` then `send()` →
SHA), which is exactly the shape an agent in a sandbox needs; standard git
smart HTTP for clone/fetch/push; scoped JWT auth where each token encodes
repository access, scopes, expiration, and **ref policies** that can reject
force pushes; **ephemeral refs** for previews/experiments with a promote
operation, plus a parallel-attempts workflow (several branches from one
commit, promote the best); Git LFS; webhooks on push/sync verified with
HMAC; forks; bidirectional GitHub mirroring; git notes; commit signing.
Explicitly excluded: pull requests, issues, code review.

Same people as `@pierre/diffs`, which the vault viewer already embeds. The
open question to resolve before betting on it: self-hostability and cost —
"state in a bucket you own" is a core fragment promise, and code.storage is
managed infrastructure.

## Features that get easier

1. **Drafts / bless / rollback** — almost embarrassing: code.storage's
   ephemeral refs + promote is the snapshot/bless model verbatim.
   `deploy --preview` becomes push-an-ephemeral-ref, go-live becomes
   promote, rollback becomes re-pointing at any old SHA — and full
   per-change file history with authors and messages comes free (the files
   table is current-state-only today).
2. **Conflicts** — today's marker/copy system has no common ancestor; git
   gives merge-base and real three-way merges. (Merge conflicts also render
   as markers, so agent-legibility is not lost.)
3. **Grants** — scoped JWTs with per-ref policies and force-push rejection
   are a cleaner storage-auth primitive than grant rows; nostr stays for
   identity/discovery.
4. **Offline and multi-machine editing** — free, and genuinely missing
   today: the current model needs the host reachable.
5. **The watcher pattern** — external-change polling can become
   webhook-reactive instead of sweep-based.
6. **Big files** — LFS replaces the entire two-tier blob plumbing.
7. **Backend pluggability** — if the wire contract is plain git smart HTTP,
   code.storage becomes one tenant of many: gitea, a bare repo on a VPS,
   GitHub via their mirroring.

## What gets harder

1. **Webhooks are a new distributed-systems surface**: retries, ordering,
   HMAC verification, replay. The current direct client→cell push has none
   of these failure modes, and "the event log is truth" gets interesting
   when two logs (events + webhook deliveries) have to agree.
2. **A dependency hop in the hot path**: today sync is one hop and
   seconds-to-live; commit → webhook → wake → pull tightens the dropzone's
   "output back in your folder seconds later" budget.
3. **Sovereignty**: code.storage is managed Pierre infra; whether it can be
   self-hosted at sane cost is the first question to answer.
4. **Secrets get a footgun**: "secrets by name only" is easy to enforce when
   secrets only ever live in the cell; repos invite committed secrets, so
   we'd want a pre-push scanner or a server-side reject rule.
5. **`--mirror-from` doesn't improve**: non-git sources — e.g. the fbrain
   encrypted working tree, the exact case the `strategy-vault` mirror test
   exercised — still need the walk-and-copy overlay. If a source *is* git,
   mirror-from collapses to a fetch, but that's not the Brain case.

## Verdict

Roughly 40% of the CLI and one deployed service delete cleanly;
snapshots/rollback/grants/conflicts get better, not just cheaper; the cell
and the entire user surface stay. The price is adopting webhooks as a
reliability surface and answering the sovereignty question for code.storage
specifically. Mitigation for both: **git-the-protocol as the contract** —
keep the remote a plain URL so the backend stays swappable — and keep the
cell materialized so celld stays sovereign.
