# Technical debt ledger

Shortcuts are allowed only through this ledger (engineering style §1).
Each entry names where it was observed, the risk, the first proof that
would show it biting, and the condition that deletes it. An entry
without a delete condition is unfinished design, not debt.

## Author code shares a process with every fragment and the host secrets

- **Observed:** workflow bodies, `app.mjs`, and `rooms.mjs` run in loader
  isolates inside the same celld process that holds
  `FRAGMENT_HOST_SECRET`, the code.storage org key, and every other
  fragment's cells. fragment's README lists hostile multi-tenant
  isolation as a non-goal while celld is alpha.
- **Risk:** an isolate escape or side channel reads other fragments' data
  and the host's secrets.
- **First proof:** any author able to publish code they did not write
  themselves (public signup, agent-written code for strangers).
- **Delete when:** before strangers can publish code, author code runs
  somewhere that holds no platform secrets and no other tenant's state
  (a separate fleet reached only through per-fragment capability
  tokens), proven by a test that an author isolate cannot reach another
  fragment or a host secret.

## The cell runtime is TypeScript, not Rust

- **Observed:** celld runs Workers-style JavaScript; `runtime/ts` is
  ~5.2k lines compiled with esbuild and type-checked with tsc.
- **Risk:** two languages in the product core; runtime errors are
  strings, not typed enums, at the cell boundary.
- **First proof:** a bug class the Rust type system or typed errors
  would have caught, found in the runtime.
- **Delete when:** the platform cells are rewritten in Rust during the
  core cut. The spike that gated this passed on 2026-09-23
  (`spikes/cells-rs/README.md`): workers-rs 0.8.5 covers SQL, alarms,
  hibernatable WebSockets, the Worker Loader, and facets, with a ~35-line
  JavaScript shim, 181 KB of gzipped wasm, and ~9 ms once per isolate.

## Primitives with no check: web push, the inbox cap

- **Observed:** `docs/published-fragments.md` (2026-09-23). Web push
  (`webpush.ts`, 336 lines) has no test anywhere; the 1000-message inbox
  cap has no e2e. (Presence: proven in the Rust e2e, slice C.)
- **Risk:** linecount- and meatproxy-style notifications, presence
  lists, or overload behavior break silently.
- **First proof:** any change to `webpush.ts`, `rooms.ts`, or the inbox
  route.
- **Delete when:** the Rust e2e (phase 1) proves subscribe → `ctx.push`
  → an encrypted delivery accepted by a fake push service, presence
  join/leave frames, and a 1001st pending message answered with 429 and
  a `queue.rejected` event.

## Shell, Node, and Python tooling

- **Observed:** `scripts/dev` and `scripts/build-runtime` (bash),
  `scripts/*.mjs` (e2e, fakes, helpers), `deploy/caddy-ask.py`, systemd
  units for the VPS.
- **Risk:** infrastructure in languages the engineering style excludes;
  untyped glue that fails at runtime.
- **First proof:** already present.
- **Delete when:** phase 2 lands the `xtask` crate and the Rust e2e, and
  phase 3 replaces the VPS deploy with Fly. Slice A landed `xtask` and the
  Rust e2e; slice B landed the Rust code.storage fake (`crates/fakes`),
  which the CLI's tests use. The JS mock and `scripts/` remain only for the
  TypeScript runtime until slice G.

## The e2e generation lane needs a fake host flag

- **Observed:** the `gen` lane only passes when the dev stack starts
  with `E2E_FAL_FAKE=1`; otherwise it fails for lack of a fal key.
- **Risk:** a red suite that is only red because of how it was started.
- **First proof:** 2026-09-23 baseline (10 failures without the flag).
- **Delete when:** phase 1 moves generation to OpenRouter and the Rust
  harness always starts its own OpenRouter fake.

## celld 0.5.1 holds an alarm open while its handler's timers run

- **Observed:** 2026-09-23, `spikes/celld-0.5.1/README.md`. An `alarm()`
  that leaves a timer pending (fragment's `csFetch` uses
  `AbortSignal.timeout(30 s)`) holds the alarm firing until the timer ends
  or the 15 s operation deadline; every re-arm meanwhile waits. The e2e
  check "paused trigger recorded as blocked, not run" failed on 10 of 12
  runs; 312/312 passed on 0.4.0.
- **Risk:** every schedule, retry, and inbox run shares one alarm per
  fragment, so work armed during or right after a firing waits up to 15 s.
- **First proof:** already present (the red e2e check).
- **Fix in hand:** a one-hunk patch in the fork worktree
  `celld-worktrees/alarm-fix` (branch `fix/alarm-completion`) passes
  `--only paused` 10 of 10 and the full e2e 312 of 312. Paul chose to run the fork and upstream the
  fix.
- **Delete when:** a celld release from denoland carries the fix and
  passes `--only paused` 10 of 10; until then dev and hosting run the fork
  build, and its timeout is not widened to hide a regression.

## Fragments share one origin when no hostname suffix is configured

- **Observed:** phase 2 slice B. Without `FRAGMENT_HOST_SUFFIX` the cell
  serves every fragment from `/f/<name>/` on one origin. Cookies are
  scoped by path, but a page on one fragment can still send same-origin
  requests to another's `__op` and `__file` with the visitor's cookies.
- **Risk:** a fragment acts as its visitors on another fragment they have
  a share link or anonymous identity for.
- **First proof:** a fleet serving strangers' fragments without a suffix.
- **Mitigated:** with a suffix configured, `/f/<name>/…` redirects to the
  fragment's own host and refuses writes (e2e `site`); `xtask dev` and
  the e2e run with a suffix.
- **Delete when:** phase 3 fleets always configure a suffix and path-mode
  serving is removed (keeping `/f/<name>/__watch`, which carries no
  cookies), with the `pathmode` e2e section replaced by a check that the
  cell refuses to start serving without a suffix.

## A deleted fragment can linger in a person's list

- **Observed:** phase 2 slice B. Each person's list of fragments is an
  index in their `Principal` cell, fed from the fragment's outbox with
  retries. Deleting a fragment delivers the removals once and then wipes
  the fragment, outbox included: a delivery that fails then is never
  retried.
- **Risk:** `fragment list` shows a fragment the person no longer has
  (calls to it answer 404; nothing leaks).
- **First proof:** a Principal cell unreachable during a delete.
- **Delete when:** phase 4 grows the person cell; its list checks each
  entry against the fragment (or the platform keeps delete tombstones and
  retries them), with an e2e that fails a delivery during a delete.

## The poll backstop wakes every fragment every five minutes

- **Observed:** phase 2 slice B, as in the TypeScript runtime: each
  fragment's alarm re-reads both branch heads every
  `FRAGMENT_POLL_INTERVAL_S` (300 s) in case a webhook was lost.
- **Risk:** cost and code.storage traffic grow with the number of
  fragments, not with their activity (1000 fragments: ~7 wakes and ~13
  calls a second).
- **First proof:** phase 3 load numbers on Fly.
- **Delete when:** the interval backs off for fragments with no recent
  pushes (or webhooks are proven reliable enough to poll daily), with a
  test that an idle fragment's alarm spacing grows.

## The effects sweep has no fault-injection test

- **Observed:** phase 2 slice C. A mutation's effects apply after the
  facet commits it; if the supervisor dies in between, the next
  activation sweeps the newest 200 ledger rows and applies what has no
  `ops` record. The e2e proves re-application is idempotent (a replay
  appends nothing twice) but cannot stop the node between the two
  commits.
- **Risk:** a mutation whose records never appear (and no `changed`
  signal) after a crash in that window, if the sweep is wrong.
- **First proof:** a crash in production between facet commit and apply.
- **Delete when:** celld (or a test build of the cell) offers a fault
  point after the facet call, and an e2e kills the node there and finds
  the records after restart.

## App channels grow without bound

- **Observed:** phase 2 slice C, as MODEL decided: app channels keep
  their records forever in the supervisor's SQLite (`events` and `ops`
  keep 90 days and 10 000 records).
- **Risk:** a busy chat grows the supervisor's database until celld's
  per-object limits or replication cost bite.
- **First proof:** a channel past ~100 000 records, or phase 3 storage
  numbers.
- **Delete when:** `fragment.json` can declare a channel's retention
  (count or age) with a platform ceiling, enforced and tested.
