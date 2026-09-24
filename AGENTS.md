# fragment-next

Places for people and agents: apps, chats, and desktops are all
fragments; agents join them as members; computers are Fly Sprites.
Built on fragment (this repo carries fragment's full history; its
TypeScript runtime was cut in phase 2, and fragment.club still runs it
from github.com/futurepaul/fragment) and on celld
(https://celld.dev/docs/).

## Read first

1. `docs/ROADMAP.md` — decisions, truth map, phases, escalations.
2. `docs/MODEL.md` — the core model on celld primitives and the spikes.
   `docs/api.md` — the wire contract the cell answers.
   `docs/finite-integration.md` — how fragment will move into
   finite.computer (Finite V3): every Core concern, its stand-in here,
   and the swap; update its row with any change that touches one;
   `docs/phase-4.md` — sign-in, identities, budgets (slice A built:
   the registry, identities, key proofs);
   `docs/phase-5.md` — agents (built overnight; choices for review);
   `docs/phase-8.md` — computers, first part (the same night);
   `docs/phase-3.md` — hosting on fragment.club;
   `docs/phase-2.md` — the record of the core cut (slices A–G);
   `docs/operate.md` — the operator runbook.
3. `docs/finite-next-lessons.md` — what to port from finite-next and the
   gotchas (celld, libfx, fx over ACP, Sprites), prices, resources.
4. `docs/published-fragments.md` — primitives that must stay expressible.
   `docs/secrets.md` — where secrets live and how code reaches them.
5. `docs/technical-debt-ledger.md` — the only place shortcuts may live.
6. The engineering style: `/Users/futurepaul/dev/finite/engineering-style/
   engineering-style.md` (hard cuts, limits, assertions on in release,
   typed errors, valid/invalid/replay/restart tests, Rust for tooling).

## Commands

The cell (`cell/`, Rust on celld), the CLI (`cli/`), and the harness
(`xtask/`, `crates/`). All tooling is Rust; the repo has no shell,
Python, or Node tooling (the notes viewer's prebuilt bundle is in the
debt ledger).

- One-time setup: `cargo install worker-build --version 0.8.5 --locked`,
  `rustup target add wasm32-unknown-unknown`, then `cargo xtask celld`
  (builds the celld fork with the alarm fix into `target/celld/bin`).
- `cargo xtask check`: host tests and clippy (host and wasm), warnings
  denied.
- `cargo xtask e2e [--only <section>]`: builds `cell/` and the CLI (with
  its `computer` feature), then
  runs `crates/e2e` against a fresh `celld dev` node and the in-process
  code.storage fake (sections: auth, create, lockdown, members, identities, signin, secrets,
  files, deploy, ops, public, site, watch, schemas, channels, live,
  routes, cli, browser, jobs, triggers, appfiles, blobs, notes, push,
  ai, agents, chat, computer, sync, restart, pathmode). The browser and notes sections drive headless
  Chrome (`CHROME_BIN` to choose one); `triggers` waits for a cron
  minute (about a minute). The node runs from a staged copy of the cell
  (`target/e2e/cell`), so the e2e and `cargo xtask dev` can run at once.
- `cargo xtask dev [--clean]`: the dev stack in the foreground: the cell
  on :8790 with fragments at `http://<name>.fragment.localhost:8790/`,
  agents (`agent/`, goose's loop) on :8793 with their model key from the
  file `OPENROUTER_API_KEY_FILE` names, the code.storage fake on :8792 (state in `target/devstack/`; its org
  key and the host secret are made there on first run), and sign-in at
  http://127.0.0.1:8790/ through the WorkOS fake on :8794 (any email), or
  a real WorkOS environment when `WORKOS_CLIENT_ID_FILE` and
  `WORKOS_API_KEY_FILE` name its files. Point the CLI at it with
  `FRAGMENT_HOST=http://127.0.0.1:8790` and run `fragment login` once. Dev fleets let jobs
  fetch local addresses (`FRAGMENT_EGRESS_LOCAL=allow`).
- `cargo xtask try <todo|inbox|notes|chat> [name]` (with `cargo xtask dev` running):
  creates and deploys a fragment from a template under
  `target/devstack/try/` (never in the repo) and prints the link to open,
  a curl for the inbox, and a `fragment` alias for the dev stack. The
  templates on the new model: `todo` (operations, channels, the browser
  library), `inbox` (a trigger, a job, the inbox), and `notes` (files as
  the state, read through `App.fetch`, refreshed by a file trigger).
  `fragment new|init --template` scaffolds the same three.
- Crates: `crates/proto` (wire types), `crates/core` (the cell's pure
  logic, host-tested), `crates/nip98`, `crates/fakes` (code.storage,
  OpenRouter, a push service), `crates/devstack`, `crates/e2e`.
- `.github/workflows/ci.yml` runs `check` and `e2e`; it has not run yet
  (no remote).
- The hosted fleet (`fleets/fragment-club.json`, `docs/operate.md`):
  `cargo xtask deploy fragment-club` ships the cell; `--nodes` ships the
  node image (local Docker); `cargo xtask e2e --fleet fragment-club` runs
  the hosted e2e (live OpenRouter: a few cents); `cargo xtask fleet
  fragment-club <celld command>` runs celld's operator commands
  (`diagnose`, `cell list`, `queue info <q>`) with the bucket's keys.
  Deploys to the hosted fleet and changes to its Fly app, bucket, or DNS
  are Paul's to approve.

## Rules

- Secrets are files read by path (`docs/finite-next-lessons.md`,
  Resources); never print them, pass them on a command line, or commit
  them.
- `cargo xtask dev` runs `celld dev` on `cell/`, which rebuilds when it
  changes; the e2e runs a staged copy (`target/e2e/cell`) with its own
  variables, so the two can run at once.
- No remote of its own yet; `fragment-rs` is a fetch-only pointer to
  github.com/futurepaul/fragment. Ask Paul before adding or pushing to a
  remote, deleting Sprites, or anything else irreversible.
