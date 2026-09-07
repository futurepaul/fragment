# Hard-cut migration: the streamlined fragment

Exploration 3, evaluated 2026-09-07. The plan for landing the three
modernizations (R2 blob tier, native Workflows under a stable authoring
surface, Queues for `notify_outbox`) as a **total hard cut — zero
transition code in the fragment codebase** — while carrying the two
production fragments (`linecount`, `meatproxy`) across with their state.

## The principle

The user contract is the migration boundary: a fragment is a folder of
files + `workflows/*.mjs` authored against `ctx` + manifest triggers.
Everything the three swaps change lives behind that contract, and state
persistence comes from **celld's durability, not from migration code**:
keep the `FragmentCell` class name and migration tag, redeploy the
runtime, and every cell wakes with its SQLite intact (files, events,
secrets, manifest). User-level porting (export/import, code mussing in
individual fragments) is tolerated; platform-level compatibility shims
are not.

## What was verified about the production fragments

- **meatproxy** (`~/dev/finite/meatproxy`): two workflows — `handler.mjs`
  (drains the room inbox, single-writer, applies every mutation as a file
  write) and `sweep.mjs` (minute cron: files `incoming/`, backfills AI
  summaries). They use only `ctx.files.*`, `fragment:ai`, and triggers —
  no engine internals, no retry-config dependence, no hidden counters
  (item ids derive from the file listing every run). **Zero files over
  64 KiB** (largest is a 24 KiB site asset), so the one real blob-tier
  casualty — oversized bodies stored in blobsd, orphaned by its removal —
  does not exist in practice today.
- **linecount**: 8 files, no crons, no paused workflows. Nothing
  engine-touching.

Verdict: **both upgrade in place via a plain runtime redeploy.** The
`rm` + recreate-under-the-same-name dance is demoted to an optional
cleanup (only if a fresh events log is wanted); same-name recreate keeps
canonical URLs, plists, and the meatproxy skill's `BASE` valid either
way.

## The cutover (user/ops-level, no platform code)

1. Deploy the new runtime: one `celld deploy`, same class name + tag.
2. Stop `blobsd`; sweep its bucket objects (ops `rm`, not code).
3. Verify both fragments wake with state: meatproxy room serves, items
   list, inbox posts file correctly; linecount responds.
4. Teammates need nothing: the sync protocol is untouched by all three
   swaps. Oversized pushes from old CLI versions hard-fail loudly with
   guidance — by design, never silently. In-flight held runs at the
   cutover instant die; meatproxy's triggers are all external and
   re-fireable, with idempotency keys collapsing redeliveries at the
   inbox door.

## Documentation is part of the deliverable

Workflow authoring is user-facing surface, so the cut ships with its
docs in the same commit — no drift:

- **`cli/GUIDE.md`** — the Workflows section, the authoring contract,
  the patterns, and the command reference. This is what `fragment guide`
  prints to every agent; it is the primary doc.
- **`templates/basic/fragment.d.ts`** — the typed contract. It currently
  *promises*: trigger kinds (`cron`/`inbox`/`files`), single-flight
  (a cron/files fire while a run is active is skipped), retry with
  backoff (default 3 attempts, 30 s base, `retry: {attempts,
  backoffMs}` or `retry: false`), cron day-of-week rules, loop-safety
  (workflow writes never re-trigger). The native-Workflows engine must
  preserve each of these or change them deliberately — and where they
  change, GUIDE + d.ts change in the same commit.
- **`docs/api.md`** — the `run`/`pause` endpoint rows and the
  workflow-loopback internals section.

The docs describe the contract, not the engine: authoring stays
`workflows/*.mjs` exporting `async run(ctx)` with manifest triggers;
what changes underneath is step durability and retry execution.

## Post-cut polish (optional, user-level)

`sweep`'s summary backfill is the one place native Workflows genuinely
improves meatproxy: `step.do` around each AI call gives durable
mid-run checkpointing. Do it after the cut, as an ordinary edit to the
fragment.

## Verdict

The hard cut is smaller than feared: no export/import needed (the
64 KiB casualty is empty today), no fragment rewrites (both use only
the stable contract), no transition code (celld durability + the ctx
boundary do all the work). The one non-negotiable is the docs: the
workflow retry/single-flight semantics are printed promises, and they
move with the engine or the engine keeps them.
