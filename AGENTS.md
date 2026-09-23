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
   `docs/secrets.md` — where secrets live and how code reaches them.
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
- `cargo xtask e2e [--only <section>]`: builds `cell/` and the CLI, then
  runs `crates/e2e` against a fresh `celld dev` node and the in-process
  code.storage fake (sections: auth, create, lockdown, members, secrets,
  files, deploy, ops, public, site, watch, schemas, channels, live,
  routes, cli, browser, sync, restart, pathmode). The browser section
  drives headless Chrome (`CHROME_BIN` to choose one).
- `cargo xtask dev [--clean]`: the new stack in the foreground: the cell
  on :8790 with fragments at `http://<name>.fragment.localhost:8790/`, and
  the code.storage fake on :8792 (state in `target/devstack/`; its org
  key and the host secret are made there on first run). Point the CLI at
  it with `FRAGMENT_HOST=http://127.0.0.1:8790`.
- Crates: `crates/proto` (wire types), `crates/core` (the cell's pure
  logic, host-tested), `crates/nip98`, `crates/fakes` (code.storage),
  `crates/devstack`, `crates/e2e`.

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
  `celld dev` rebuilds on change. The Rust e2e and `xtask dev` both write
  `cell/.dev.vars`: run one at a time.
- No remote of its own yet; `fragment-rs` is a fetch-only pointer to
  github.com/futurepaul/fragment. Ask Paul before adding or pushing to a
  remote, deleting Sprites, or anything else irreversible.
