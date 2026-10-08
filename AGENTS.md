# fragment-next

Fragment is the product: stateful, multiplayer web apps published from
the CLI. Agents are fragments a computer of your own runs (today, Hermes
profiles; docs/computers.md).
Built on fragment (this repo carries fragment's full history; its
TypeScript runtime was cut in phase 2; this repo is
github.com/futurepaul/fragment's `master`, the Cloudflare line since the
cut: fragment.club runs the `celld` branch, tag `celld-final`, until
cutover, decision 35). The cell runs on Cloudflare Workers (workerd under
`wrangler dev` locally) since phase 2 of that plan. The cut (decision 33)
took Hermes, computers, the desktop, the personal agent's chat,
sandcastle, and the fleet's deploy path off master; they live at the tag
`celld-final`. Computers came back in its phase 4, Hermes as their
image's agent runtime.

## Read first

0. `docs/cloudflare-v1.md` — **the plan of record since 2026-10-02**:
   fragment v1 entirely on Cloudflare, its decisions, phases and the
   hard cut. Where it disagrees with anything below, it wins.
   `SPECIAL-CASE-INVENTORY.md` lists the platform surfaces that are not
   fragments, and why; keep it short.
1. The decisions are cloudflare-v1.md's: its own, and the ROADMAP's that
   still hold, as R4 to R18. The truth map every change is checked
   against is MODEL.md's ("Where each fact lives").
2. `docs/MODEL.md` — the core model, in Cloudflare's primitives.
   `docs/api.md` — the wire contract the cell answers.
   `docs/computers.md` — the platform's contract with a computer's image;
   `docs/bridge.md` — the bridge, the one process an image runs between
   its agent runtime and the fragment API;
   `docs/chat-records.md` — a chat's records, which the bridge writes
   and the chat template reads;
   `docs/durable-computers.md` — how a computer's state survives a
   sleep, a crash or a rolled-back disk: the design of record;
   `docs/ledger.md` — the usage ledger: what is metered, at what price,
   and who pays;
   `docs/finite-integration.md` — how fragment will move into
   finite.computer (Finite V3): every Core concern, its stand-in here,
   and the swap; update its row with any change that touches one;
   `docs/hermes-relay.md` — Hermes' Relay contract, read from its code
   (its connector went at the cut).
3. `docs/published-fragments.md` — primitives that must stay expressible.
   `docs/secrets.md` — where secrets live and how code reaches them.
4. `docs/technical-debt-ledger.md` — the only place shortcuts may live.
5. The engineering style: `/Users/futurepaul/dev/finite/engineering-style/
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
  `AR_wasm32_unknown_unknown`), and Docker. No Node install and no `npm
  ci`: xtask (and the e2e) bring their own. On first use they fetch the
  pinned Node (v24.21.0, `crates/devstack/src/node_release.rs`) from
  nodejs.org/dist into `target/tools/node-v<version>-<platform>/`,
  checking the tarball's SHA-256 before unpacking it, and run that Node's
  own npm (`npm ci`) whenever node_modules is missing or was installed
  from another package.json, package-lock.json or Node. Every JavaScript
  process (npm, wrangler as `<node> node_modules/wrangler/bin/wrangler.js`,
  and whatever they start) runs on it, with it first on PATH; no `node`
  from PATH is ever used, and no other Node. Their caches are the repo's:
  `XDG_CACHE_HOME=target/cache` (miniflare keeps the Chrome for Testing
  that preview cards' local Browser Rendering downloads, 126, about 145
  MB, in `target/cache/.wrangler/chrome`), `WRANGLER_CACHE_DIR=
  target/cache/wrangler`, and npm's in `target/cache/npm`; nothing goes
  to the system's cache. Moving the pin: README.md, "The pinned Node".
- `cargo xtask build`: the cell for wasm32.
- `cargo xtask check`: every first-party JavaScript file (`.js`, `.mjs`,
  `.cjs` git tracks or would add, less the vendored ones) through `node
  --check` on the pinned Node, each as a module or a classic script as
  it is loaded (`xtask/src/js_syntax.rs`: its `VENDORED` and `CLASSIC`
  lists), naming each file and line that does not parse; then host tests
  and clippy (host and wasm), warnings denied.
- `cargo xtask e2e [--only <section>[,...] | --except <section>[,...]]`:
  builds `cell/` and the CLI, then runs `crates/e2e` against
  a fresh `wrangler dev` node (workerd) and the in-process fakes, which
  stand only at vendor boundaries (sections, in order:
  auth, create, lockdown, keys, members, identities, signin, levers, secrets,
  delegation, files, deploy, templates, share, isolation, frames, ops, public,
  effects, facet-cap, app-lockdown, site, watch, schemas, channels,
  live, routes, cli, browser, jobs, triggers, appfiles, blobs, notes,
  brain, push, ai, ledger, transcribe, shell, computers, chat, shell-ui, wipe,
  hermes, agent-smoke, sync, restart; `crates/e2e/src/lanes/mod.rs`).
  `wipe` wipes a person it made (docs/api.md, Operators) with an
  operator key no person holds: the local node's own, or, hosted, the
  file `--operator-key-file <file>` names (its npub in the config's
  `operators`; a skip without one).
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
  xtask dev` can run at once, as can runs in several worktrees: a node's
  computer images are its project's own (each its Dockerfile plus a
  `dev.fragment.project` label, written under `<project>/.wrangler/images/`),
  so another node's teardown, which removes containers by image, never
  removes its computers; and a run removes the containers its nodes left
  (each computer's and its `-proxy` sidecar, named for the computers in
  its state, never another's) when it ends, fails, panics, or gets Ctrl-C,
  SIGTERM or SIGHUP, after killing its node (`crates/devstack/src/
  containers.rs`; `xtask dev` does so when wrangler exits). That scratch holds each node boot's log
  (`node-<port>-<boot>.log`, wrangler's debug logs on) and is removed when
  every check passes, kept when one fails (`FRAGMENT_E2E_KEEP=1` keeps it
  anyway). Each section declares what it needs (`crates/e2e/src/needs.rs`:
  fakes, the node, the whole deployment, levers, local Docker, Chrome,
  computers, models, two sites), and that alone chooses the hosted set.
  The node's test levers (`/api/test/*`) take a secret made per run.
  A wait that runs out its limit prints `(a wait ran out its …s at
  <file>:<line>)`: one that does so on a passing run costs every run.
  A call that fails says when it was sent (UTC, as the node's logs stamp
  their lines) and how long it waited: a dropped connection fails at
  once, a hung call after its wait. A boot's log can lack its last
  seconds when a failure stops the node at once (the Workers' output
  reaches it late, in bursts): logs that just stop are not a node that
  died. The run's client never reuses a connection idle 4 s: workerd
  closes one idle 5 s, and a request written onto it as it closes is
  lost (`POOL_IDLE`, `crates/e2e/src/api.rs`).
- `cargo xtask e2e --shard <k>/4`: one of the four shards CI runs, each
  on its own runner with its own build and node (`SHARDS` in
  `crates/e2e/src/lanes/mod.rs`: every section in exactly one, a host
  test checks; rebalance it from the time each shard's log prints for
  each section). CI's `e2e` job is green when every shard is. CI splits
  each shard's run in two, `--build-only` then `--no-build`, so the
  cache saves between (the build also builds the computer images ahead
  of the node, beside the Rust: `xtask/src/build.rs`).
- `cargo xtask e2e --hosted --config <deploy config> --branch <b> [--only
  … | --except …] [--dry-run | --sweep [<run>] | --sweep-all]
  [--max-paid-calls <n>] [--operator-key-file <file>]`: the hosted
  lane, the same sections against the branch deployment
  `https://<b>.<zone>` on its real vendors (crates/e2e/src/hosted.rs). Its
  people sign in through the preview's levers as `<name>@e2e.test` (the
  config's `test_secret_file`, read from its file; docs/secrets.md), its
  fragments are `e2e-<run>-…` (the run's id, 6 hex digits, printed as it
  starts), and a section that needs what a preview lacks is
  a skip that says why. Paid calls (models, AI steps) are lent from the
  run's budget (default 60), each person's capped by their ledger, and the
  run ends saying what it spent. `agent-smoke` runs only here, and only
  by name (`--only agent-smoke`): a real agent (our Hermes image on its
  real model, `Need::RealAgent`) and Chrome, through the flows a person
  uses (a first reply, the CLI, an app, the desktop and its screen, an
  approval, a sleep and a wake), for about half an hour and up to 50 paid
  calls (crates/e2e/src/lanes/agent_smoke.rs). `--dry-run` prints the plan (base URL,
  what runs, what is skipped and why) and calls nothing. A preview is
  shared (several sessions run on it at once), so a sweep is one run's
  unless told otherwise: `--sweep` deletes the fragments of the last run
  that finished in this checkout, `--sweep <run>` those of the run it
  names, whatever their age and no other run's, and puts that run's
  people's computers to sleep (not one whose person owns another run's
  fragment too); a hosted run ends naming its id for this. `--sweep-all`,
  for when nothing else runs there, deletes every e2e fragment at least an
  hour old and spares younger ones (a run started meanwhile may be using
  them), and their people's computers. Each says what it kept and why:
  other runs' fragments counted by run, young ones named with their age
  (crates/e2e/src/hosted/sweep.rs). `cargo xtask e2e --rehearse` keeps the
  hosted lane's rules on the local node (shaped as a branch, its fakes
  hidden from the lanes) and ends with its sweeps: its run's, which keeps
  another run's fragment made beside it, then the whole one's, which
  spares it for its age. Running it against a preview spends test cents:
  Paul's or the coordinating session's to run.
- `cargo xtask dev [--clean]`: the dev stack in the foreground under
  `wrangler dev`: the cell on :8790 with fragments at
  `http://<label>--<username>.fragment.localhost:8790/`, which rebuilds
  when `cell/src` or `crates/` change, the model route and AI steps on
  the Workers AI fake on :8796 (echoes, and draws placeholder JPEGs for
  image steps; dev never calls a real model), which spend their payer's
  ledger (dev people are seats, with the month's included credit), the
  code.storage fake on :8792 (state in `target/devstack/`; its org
  key and the host secret are made there on first run), and sign-in at
  http://127.0.0.1:8790/ through the WorkOS fake on :8794 (any email), or
  a real WorkOS environment when `WORKOS_CLIENT_ID_FILE` and
  `WORKOS_API_KEY_FILE` name its files. Its secrets are seeded into
  wrangler's local Secrets Store in `cell/.wrangler/state` and bound by
  name as a deploy binds them (`--clean` clears them with the state;
  docs/secrets.md). Each boot's log is
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
  `blank`, and `calories`: a channel trigger and a text step); the
  shell's catalog offers `todo`, `inbox` and `blank`.
- `cargo xtask secret set <name> --config <file> [--from-file <path>]`,
  `secret gen <name> --config <file>`, `secret list --config <file>`: the
  deployment's secrets in its account's Cloudflare Secrets Store
  (xtask/src/secret.rs, through wrangler on the pinned Node). `set` takes
  the value from wrangler's hidden prompt, standard input, or a file once,
  and updates a secret already there (never the config's host secret,
  which rotates by name); `gen` makes a new one of 32 random bytes as hex;
  `list` shows names and times and what the config names that the store
  lacks. `set` and `gen` make the account's one store when there is none.
  No `rm`. `--local <state dir>` acts on wrangler's local store instead.
  Setting a remote secret is Paul's (or the coordinating session's).
- `cargo xtask deploy --config <file> [--branch <name>]`: builds and
  deploys to Cloudflare from a deployment's config, kept outside the repo
  (`deploy/example.jsonc`; xtask/src/deploy.rs). It lists the store first
  and refuses, before anything is built, when a secret the config names
  is missing (naming the `secret set` for each); it binds them by name
  and uploads no secret but a branch's test secret. A branch is a complete
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
- `.github/workflows/ci.yml` runs `check`, the e2e's four shards (`e2e
  shard k/4`), and `e2e`, green when they all are, on Linux. Its caches
  restore on every run and save only from master's pushes (each key's
  contents are named in the workflow). `release.yml` builds the CLI for
  macOS and Linux.
- Master deploys to Cloudflare (`xtask deploy`): branch copies on the dev
  zone `finite.place` in Paul's account, and production only at cutover.
  fragment.club (the celld fleet on Fly) deploys only from the `celld`
  branch (tag `celld-final`) until cutover: its fleet file, node image
  and docs/operate.md's commands are that branch's. Deploys to the
  hosted fleet, production deploys, and DNS changes are Paul's to
  approve.

## Rules

- The deployment's secrets live in its account's Cloudflare Secrets
  Store: set with `cargo xtask secret`, named in its config, bound to the
  Worker by name, and read only by `cell/src/keys.rs` (docs/secrets.md).
  Only the DNS token and a preview's test secret are files read by path.
  Never print them, pass them on a command line (no `--value`), or commit
  them; deleting one is Paul's.
- `cargo xtask dev` runs `wrangler dev` on `cell/`, which rebuilds when it
  changes; the e2e runs a staged copy (`target/e2e/<run>/cell`) with its own
  variables, state and dev registry, so the two can run at once.
- The remote is `fragment-rs`, github.com/futurepaul/fragment: work goes
  up as a branch and a pull request against `master` (branch names under
  `ci/` are refused). The coordinating session reviews and merges a PR
  once its CI is green. Deploys, adding a remote, deleting Sprites, DNS,
  and anything else irreversible are Paul's to approve.
