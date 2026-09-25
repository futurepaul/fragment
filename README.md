# fragment-next

A fragment is one small place on the web: a folder of files in git, an
app of named operations over its own SQLite, channels that pages follow
live, and members with roles. Fragments run on
[celld](https://celld.dev) (self-hosted Durable Objects that keep their
state in a bucket); each is a cell that sleeps when idle.

This repo holds:

- **the cell** (`cell/`): Rust (workers-rs) on celld, with a small
  JavaScript shim. It answers the control API, serves sites, runs each
  fragment's app in its own loaded worker, and runs jobs as Workflows.
- **the `fragment` CLI** (`cli/`): the whole control surface, built for
  agents. `fragment guide` prints the agent guide.
- **the harness** (`xtask/`, `crates/`): the dev stack, Rust fakes for
  code.storage, OpenRouter, and a push service, and the e2e suite that
  drives the real cell, CLI, and a browser.

Read [docs/MODEL.md](docs/MODEL.md) for the model,
[docs/api.md](docs/api.md) for the wire contract, and
[docs/ROADMAP.md](docs/ROADMAP.md) for where it is going. fragment.club
runs it: this repo is
[futurepaul/fragment](https://github.com/futurepaul/fragment)'s `master`. MIT
licensed; see [LICENSE](LICENSE).

## Try it locally

One-time setup:

```
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.5 --locked
cargo xtask celld          # builds the celld fork into target/celld/bin
```

Then:

```
cargo xtask dev            # the cell on :8790 and the code.storage fake on :8792
cargo xtask try todo       # in another terminal: todo | inbox | notes
```

`try` creates and deploys a fragment from a template under
`target/devstack/try/`, and prints the link to open and a `fragment`
alias pointed at the dev stack. Fragments are served at
`http://<label>--<username>.fragment.localhost:8790/`.

## Tests

```
cargo xtask check          # host tests, clippy on host and wasm, warnings denied
cargo xtask e2e            # the full suite against a fresh celld node (--only <section>)
```

The e2e stages its own copy of the cell, so it runs alongside
`cargo xtask dev`. Its browser sections drive headless Chrome
(`CHROME_BIN` to choose one). `cargo xtask e2e --fleet fragment-club`
runs the hosted sections against the live fleet (see
[docs/operate.md](docs/operate.md)).

## Layout

```
cell/          the cell: router, fragment supervisor, jobs, files, blobs, deliveries
cli/           the fragment CLI and GUIDE.md (the agent guide)
crates/proto   wire types and limits
crates/core    the cell's pure logic, host-tested (schemas, cron, globs, sealing, web push)
crates/nip98   NIP-98 signing and verification
crates/fakes   code.storage, OpenRouter, and push-service fakes
crates/devstack  runs a celld node and the fakes
crates/e2e     the end-to-end suite
templates/     todo, inbox, notes
fleets/        hosted fleets' settings (no secrets) and the node image's Dockerfile
crates/node    the launcher that starts celld on a fleet Machine
xtask/         build, celld, dev, try, check, e2e, deploy, fleet
docs/          model, contract, roadmap, phase records, the debt ledger
spikes/        the phase 1 spikes and their verdicts
```
