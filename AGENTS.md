# fragment-next

Places for people and agents: apps, chats, and desktops are all
fragments; agents join them as members; computers are Fly Sprites.
Built on fragment (this repo carries fragment's full history) and on
celld (https://celld.dev/docs/).

## Read first

1. `docs/ROADMAP.md` — decisions, truth map, phases, escalations.
2. `docs/MODEL.md` — the core model on celld primitives and the spikes.
   `docs/phase-2.md` — the current phase's slices and checkpoints.
3. `docs/finite-next-lessons.md` — what to port from finite-next and the
   gotchas (celld, libfx, fx over ACP, Sprites), prices, resources.
4. `docs/published-fragments.md` — primitives that must stay expressible.
5. `docs/technical-debt-ledger.md` — the only place shortcuts may live.
6. The engineering style: `/Users/futurepaul/dev/finite/engineering-style/
   engineering-style.md` (hard cuts, limits, assertions on in release,
   typed errors, valid/invalid/replay/restart tests, Rust for tooling).

## Commands

The new runtime (`cell/`, Rust; phase 2, `docs/phase-2.md`):

- One-time setup: `cargo install worker-build --version 0.8.5 --locked`,
  `rustup target add wasm32-unknown-unknown`, then `cargo xtask celld`
  (builds the celld fork with the alarm fix into `target/celld/bin`).
- `cargo xtask check`: host tests and clippy (host and wasm), warnings
  denied.
- `cargo xtask e2e [--only <section>]`: builds `cell/` and runs
  `crates/e2e` against a fresh `celld dev` node.
- `cargo xtask dev [--clean]`: the new stack in the foreground on :8790.

The old runtime (`runtime/`, TypeScript), until slice G deletes it:

- Runtime tests: `npm run test:runtime` (after `scripts/build-runtime`
  when `runtime/ts` changes; `runtime/src` is committed build output).
- Local stack: `E2E_FAL_FAKE=1 scripts/dev up` (celld :8789, code.storage
  mock :9940, fal fake :9942), then `node scripts/e2e.mjs` (needs
  `cargo build -p fragment-cli` first). `scripts/dev down` stops it.
  `CELLD_BIN=target/celld/bin/celld` runs it on the celld fork.

## Rules

- Secrets are files read by path (`docs/finite-next-lessons.md`,
  Resources); never print them, pass them on a command line, or commit
  them.
- Do not edit files under `runtime/` while an e2e run is in flight:
  `celld dev` rebuilds on change.
- No remote of its own yet; `fragment-rs` is a fetch-only pointer to
  github.com/futurepaul/fragment. Ask Paul before adding or pushing to a
  remote, deleting Sprites, or anything else irreversible.
