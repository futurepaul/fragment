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

## Primitives with no check: web push, room presence, the inbox cap

- **Observed:** `docs/published-fragments.md` (2026-09-23). Web push
  (`webpush.ts`, 336 lines) has no test anywhere; presence and the
  1000-message inbox cap have no e2e.
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
  phase 3 replaces the VPS deploy with Fly. The spike drivers
  (`spikes/driver`) are the first Rust harness pieces; `scripts/dev` now
  also writes `runtime/.dev.vars` for celld 0.5.

## The e2e generation lane needs a fake host flag

- **Observed:** the `gen` lane only passes when the dev stack starts
  with `E2E_FAL_FAKE=1`; otherwise it fails for lack of a fal key.
- **Risk:** a red suite that is only red because of how it was started.
- **First proof:** 2026-09-23 baseline (10 failures without the flag).
- **Delete when:** phase 1 moves generation to OpenRouter and the Rust
  harness always starts its own OpenRouter fake.

## celld 0.5.1 holds an alarm handler's completion until the deadline

- **Observed:** 2026-09-23, `spikes/celld-0.5.1/README.md`. A re-arm to
  now ~13 ms after an alarm handler re-armed itself did not fire until
  `CELLD_OPERATION_DEADLINE_MS` (15 s) after that handler started. The
  e2e check "paused trigger recorded as blocked, not run" fails on 10 of
  12 runs; 312/312 passed on 0.4.0.
- **Risk:** every schedule, retry, and inbox run shares one alarm per
  fragment, so work armed right after a firing can wait 15 s.
- **First proof:** already present (the red e2e check).
- **Delete when:** a celld release passes `node scripts/e2e.mjs --only
  paused` 10 of 10 on a stack that has not just rebuilt, or a minimal
  repro is reported upstream and fixed. The check stays red until then;
  its timeout is not widened.
