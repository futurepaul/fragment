# fragment-next

Fragment is the product: stateful, multiplayer web apps published from
the CLI. Agents are optional add-ons a fragment declares; computers come
back on Cloudflare (docs/cloudflare-v1.md, phase 4).
Built on fragment (this repo carries fragment's full history; its
TypeScript runtime was cut in phase 2; this repo is
github.com/futurepaul/fragment's `master`, the Cloudflare line since the
cut: fragment.club runs the `celld` branch, tag `celld-final`, until
cutover, decision 35). The cell runs on Cloudflare Workers (workerd under
`wrangler dev` locally) since phase 2 of that plan. The cut (decision 33)
took Hermes, computers, the desktop, the personal agent's chat,
sandcastle, and the fleet's deploy path off master; they live at the tag
`celld-final`.

## Read first

0. `docs/cloudflare-v1.md` — **the plan of record since 2026-10-02**:
   fragment v1 entirely on Cloudflare, its decisions, phases and the
   hard cut. Where it disagrees with anything below, it wins.
   `SPECIAL-CASE-INVENTORY.md` lists the platform surfaces that are not
   fragments, and why; keep it short.
1. `docs/ROADMAP.md` — decisions, truth map, phases (and what is live),
   escalations.
2. `docs/MODEL.md` — the core model on celld primitives and the spikes.
   `docs/api.md` — the wire contract the cell answers.
   `docs/finite-integration.md` — how fragment will move into
   finite.computer (Finite V3): every Core concern, its stand-in here,
   and the swap; update its row with any change that touches one;
   `docs/phase-6.md` — usernames and flat hosts, templates, your agent
   making apps (its desktop and computers went at the cut);
   `docs/phase-4.md` — sign-in, identities, budgets (live);
   `docs/hardening.md` — the hardening pass (H1–H3 live): native keys,
   the isolation findings;
   `docs/phase-5.md` — agents (choices for review);
   `docs/hermes-relay.md` — Hermes' Relay contract, read from its code
   (its connector went at the cut);
   `docs/phase-3.md` — hosting on fragment.club;
   `docs/phase-2.md` — the record of the core cut (slices A–G);
   `docs/operate.md` — the operator runbook (the `celld` branch's).
3. `docs/finite-next-lessons.md` — what to port from finite-next and the
   gotchas (celld, libfx, fx over ACP, Sprites), prices, resources.
4. `docs/published-fragments.md` — primitives that must stay expressible.
   `docs/secrets.md` — where secrets live and how code reaches them.
5. `docs/technical-debt-ledger.md` — the only place shortcuts may live.
6. The engineering style: `/Users/futurepaul/dev/finite/engineering-style/
   engineering-style.md` (hard cuts, limits, assertions on in release,
   typed errors, valid/invalid/replay/restart tests, Rust for tooling).

## Commands

The cell (`cell/`, Rust on Workers), the CLI (`cli/`), and the harness
(`xtask/`, `crates/`). All tooling is Rust; the repo has no shell or
Python tooling. Its JavaScript from npm is wrangler and the Sandbox SDK
the cell's shim bundles, pinned in `package.json` (the notes viewer's
prebuilt bundle is in the debt ledger).

- One-time setup: `cargo install worker-build --version 0.8.5 --locked`,
  `rustup target add wasm32-unknown-unknown`, an LLVM clang that compiles
  for wasm32 (Apple's does not: `brew install llvm`, or Nix, which xtask
  finds; or name one with `CC_wasm32_unknown_unknown` and
  `AR_wasm32_unknown_unknown`), Node 22 or later, and `npm ci` at the repo
  root (the pinned wrangler; `WRANGLER_BIN` names another).
- `cargo xtask build`: the cell and the agents for wasm32.
- `cargo xtask check`: host tests and clippy (host and wasm), warnings
  denied.
- `cargo xtask e2e [--only <section>[,...] | --except <section>[,...]]`:
  builds `cell/`, `agent/`, and the CLI, then runs `crates/e2e` against
  a fresh `wrangler dev` node (workerd) and the in-process fakes, which
  stand only at vendor boundaries (sections, in order:
  auth, create, lockdown, keys, members, identities, signin, secrets,
  delegation, files, deploy, templates, share, isolation, frames, ops, public,
  effects, facet-cap, app-lockdown, site, watch, schemas, channels,
  live, routes, cli, browser, jobs, triggers, appfiles, blobs, notes,
  push, ai, ledger, agents, addon, shell, computers, chat, shell-ui,
  hermes, sync, restart, pathmode; `crates/e2e/src/lanes/mod.rs`).
  `computers`, `chat` and `shell-ui` run the stub image (`images/stub`)
  in Docker, and `chat`, `frames` and `shell-ui` drive Chrome; `hermes`, the real-Hermes lane, builds
  and runs our Hermes image (3.8 GB), so it runs only by name
  (`--only hermes`) and is a skip otherwise. A check local workerd cannot make (its
  CPU and memory limits, a Workflow that sleeps through a crash) is a
  `skip`, printed and counted: the hosted lane's. The share, isolation, browser, and
  notes sections drive headless Chrome (`CHROME_BIN` to choose one; one
  Chrome serves the whole run, a fresh browser context per section;
  frames, and computers' frame checks, start one of their own that
  blocks third-party cookies, as Safari does);
  `--only triggers` waits for a cron
  minute (up to a minute; a full run deploys its cron fragment sections
  earlier). A section that errors or panics is one FAIL and the sections
  after it still run. The node runs from a staged copy of the cell in the
  run's own scratch (`target/e2e/<run>/cell`), so the e2e and `cargo
  xtask dev` can run at once. That scratch holds each node boot's log
  (`node-<port>-<boot>.log`, wrangler's debug logs on) and is removed when
  every check passes, kept when one fails (`FRAGMENT_E2E_KEEP=1` keeps it
  anyway).
- `cargo xtask dev [--clean]`: the dev stack in the foreground under
  `wrangler dev`: the cell on :8790 with fragments at
  `http://<label>--<username>.fragment.localhost:8790/`, which rebuilds
  when `cell/src` or `crates/` change,
  agents (`agent/`, goose's loop) beside it in the same process (the router
  hands them `/api/agents` and `/api/a/*`), whose model calls go through
  the model route to the Workers AI fake on :8796 (echoes, and draws
  placeholder JPEGs for image steps; dev never calls a real model) and
  spend their owner's ledger (dev people are seats, with the month's
  included credit), the code.storage fake on :8792 (state in `target/devstack/`; its org
  key and the host secret are made there on first run), and sign-in at
  http://127.0.0.1:8790/ through the WorkOS fake on :8794 (any email), or
  a real WorkOS environment when `WORKOS_CLIENT_ID_FILE` and
  `WORKOS_API_KEY_FILE` name its files. Each boot's log is
  `target/devstack/node-8790-<boot>.log` (printed at start;
  `FRAGMENT_NODE_LOGS=1` adds wrangler's debug logs). Point the CLI at it with
  `FRAGMENT_HOST=http://127.0.0.1:8790` and run `fragment login` once. Dev fleets let jobs
  fetch local addresses (`FRAGMENT_EGRESS_LOCAL=allow`).
- `cargo xtask try <todo|inbox|notes> [name]` (with `cargo xtask dev` running):
  creates and deploys a fragment from a template under
  `target/devstack/try/` (never in the repo) and prints the link to open,
  a curl for the inbox, and a `fragment` alias for the dev stack. The
  reference templates: `todo` (operations, channels, the browser
  library), `inbox` (a trigger, a job, the inbox), and `notes` (files as
  the state, read through `App.fetch`, refreshed by a file trigger).
  `fragment new|init --template` scaffolds any of `templates/` (also
  `blank` and `calories`, which has a goose agent of its own); the
  platform's "new" page offers all but `notes`.
- `cargo xtask deploy --config <file> [--branch <name>]`: builds and
  deploys to Cloudflare from a deployment's config, kept outside the repo
  (`deploy/example.jsonc`; xtask/src/deploy.rs). A branch is a complete
  copy at `<branch>.<zone>`, its fragments at
  `<label>--<username>--<branch>.<zone>`. `cargo xtask teardown --config
  <file> --branch <name>` removes one (irreversible: ask Paul).
- Crates: `crates/proto` (wire types), `crates/core` (the cell's pure
  logic, host-tested; sealing at rest is `seal.rs`), `crates/nip98`,
  `crates/templates` (`templates/`, embedded),
  `crates/fakes` (code.storage, Workers AI, WorkOS, a push service),

  `crates/devstack`, `crates/e2e`.
- `images/` (the computer images: the bridge, the stub, our Hermes image;
  docs/bridge.md) is its own workspace: `cargo test --workspace` and
  `cargo clippy --workspace --all-targets -- -D warnings` there (CI:
  `.github/workflows/images.yml`); `cargo test -p fragment-bridge --test
  docker -- --ignored` builds both images and runs them in Docker
  (linux/amd64) against a fake API and a scripted model, real Hermes
  included. The e2e's computer sections run the stub image under
  `wrangler dev`, which needs Docker.
- `.github/workflows/ci.yml` runs `check` and `e2e` on Linux.
  `release.yml` builds the CLI for macOS and Linux.
- Master deploys to Cloudflare (`xtask deploy`): branch copies on the dev
  zone `finite.place` in Paul's account, and production only at cutover.
  fragment.club (the celld fleet on Fly) deploys only from the `celld`
  branch (tag `celld-final`) until cutover: its fleet file, node image
  and docs/operate.md's commands are that branch's. Deploys to the
  hosted fleet, production deploys, and DNS changes are Paul's to
  approve.

## Rules

- Secrets are files read by path (`docs/finite-next-lessons.md`,
  Resources); never print them, pass them on a command line, or commit
  them.
- `cargo xtask dev` runs `wrangler dev` on `cell/`, which rebuilds when it
  changes; the e2e runs a staged copy (`target/e2e/<run>/cell`) with its own
  variables, state and dev registry, so the two can run at once.
- The remote is `fragment-rs`, github.com/futurepaul/fragment: work goes
  up as a branch and a pull request against `master` (branch names under
  `ci/` are refused). The coordinating session reviews and merges a PR
  once its CI is green. Deploys, adding a remote, deleting Sprites, DNS,
  and anything else irreversible are Paul's to approve.
