# Spike 2: the app facet as author SQL

Status: done 2026-09-23. Verdict: **adopt the facet, with a different
transaction rule than `docs/MODEL.md` proposed.** A mutation is a
synchronous function over the app's own SQL. Platform code inside the
facet runs it in the facet's `transactionSync`, together with the
operation's ledger row in the facet's database. The supervisor's root
transaction cannot enclose a facet call.

Files: `todo.mjs` (the author app), `platform.mjs` (platform code the
supervisor loads around it), the supervisors in `../cells-rs` and
`../cells-js`, the checks and numbers from `../driver` (`spike-driver`).

## Findings

**1. A root transaction around a facet call fails for any real app.** At
commit celld writes the facet's whole database image into one row of the
root database. Once the facet database passes ~1.6 MB (1,667,072 bytes
committed; the next 0.1 MiB step failed), the commit fails with
`storage.transaction: write a deferred facet database image`, and from
then on every mutation in a root transaction fails, even a one-row insert
(checked at 13 MB).

**2. Inside a root transaction a facet cannot call out.** A facet that
writes and then calls a capability is refused ("refusing to send: the
facet's write did not reach its database (the root transaction has not
committed the facet image)"). A capability call that re-enters the
supervisor deadlocks until the 30 s transaction limit resets the object.

**3. Without a root transaction, facets of every tested size work, and the
cost is local.** celld copies the image into the root after each changed
turn; a small mutation replicates kilobytes, not the image.

| Facet database | Mutation p50 / p95 (Rust) | Mutation p50 / p95 (JS) | Replicated per mutation |
|---|---|---|---|
| 1.1 MB | 8.7 / 19.6 ms | 8.2 / 20.1 ms | ~77 KB (early snapshots of a young cell) |
| 16.8 MB | 18.2 / 21.6 ms | 18.8 / 24.5 ms | ~6.5 KB |
| 67.6 MB | 55.2 / 91.3 ms | 61.3 / 111.7 ms | ~2.0–2.2 MB: a root snapshot carrying the whole image fell inside the 30-mutation window in three of four runs |

From `../results/spike1.json`; a first run agreed within ~10%. A
supervisor-only write in the same cells: 7–8 ms. The root cell's
snapshots carry the whole facet image, so a large app also makes its
fragment's snapshots, restores, and moves large (paged restore helps).

**4. The facet-local ledger holds.** `platform.mjs` wraps the author's
`App`: `__mutate(id, name, inputSha, input)` runs in
`ctx.storage.transactionSync`, looks up `_fragment_ops`, runs the author's
method, refuses a returned promise, and records the result. Checked on
both supervisors: replay returns the stored result; a conflicting body is
409; an author throw rolls back its write and leaves the id unused; an
async mutation is refused and rolled back; a supervisor failure after the
facet committed turns the retry into a replay with no second write; after
a restart the replay still answers and the rows survive.

**5. Isolation holds.** The facet sees only its own tables (and celld's
`_cf_*`); `globalOutbound: null` makes `fetch` throw; `setAlarm` throws
("Facets currently cannot set alarms.").

**6. `WorkerCode.limits` applies to facet calls.** With `cpuMs: 200` a
spinning facet call was stopped after 212 ms (Rust) / 210 ms (JS) with
"Worker exceeded CPU limit of 200 ms", and the cell answered the next
call. (`getDurableObjectClass()` takes only `props`; the `WorkerCode`
limits still apply.)

## What this changes in the model

- **mutation**: synchronous, the app's own SQL only, no `await`, no
  capabilities, no network. What it wants to happen next (channel records,
  notifications, starting a job, file writes for a vault) it returns as
  effects in its result. The facet's ledger row stores them, so the
  ledger is also an outbox: the supervisor applies effects keyed by
  (operation id, index), marks the row applied, and on activation sweeps
  rows the facet committed but the supervisor never applied.
- **query**: may be async and may call read capabilities (files, channels,
  blobs); not ledgered.
- **job**: a Workflow; each step is a query, a mutation, or an external
  effect (see spike 3).
- **limits**: facet database 16 MiB by default (~18 ms per mutation),
  64 MiB at most (~55–61 ms, and a whole-image root snapshot every few
  dozen mutations); large bytes go to R2.
