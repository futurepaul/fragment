# fragment-next

Fragment publishes stateful, multiplayer web apps from the CLI. Sign up
(invite-only for now), pair the `fragment` CLI, and publish. fragment.club
lists your fragments, and serves them until they move to fragment.boats.
Hosting, permissions, sharing, crons, jobs, and channels are built in.

A fragment is a folder of files in git, an app of named operations over
its own SQLite, channels that pages follow live, and members with roles.
An **agent** is an optional add-on, and a fragment that declares none
carries nothing of it: goose, so a calorie tracker can take "2 eggs and
toast". Fragments run on Cloudflare: each is a Durable Object that
sleeps when idle, its app in a Worker of its own
([docs/cloudflare-v1.md](docs/cloudflare-v1.md)); computers come back
there.

This repo holds:

- **the cell** (`cell/`): Rust (workers-rs) on Cloudflare Workers, with a
  small JavaScript shim. It answers the control API, serves sites, runs each
  fragment's app in its own loaded worker, and runs jobs as Workflows.
- **the `fragment` CLI** (`cli/`): the whole control surface, built for
  agents. `fragment guide` prints the agent guide.
- **the agents** (`agent/`): goose's loop in a Durable Object per agent,
  a Worker beside the cell's; an agent joins fragments as a member.
- **the harness** (`xtask/`, `crates/`): the dev stack, Rust fakes for
  code.storage, Workers AI, WorkOS, and a push service, and the e2e suite
  that drives the real cell, CLI, and a browser.

Read [docs/MODEL.md](docs/MODEL.md) for the model,
[docs/api.md](docs/api.md) for the wire contract, and
[docs/cloudflare-v1.md](docs/cloudflare-v1.md) for where it is going.
This repo is [futurepaul/fragment](https://github.com/futurepaul/fragment);
fragment.club runs its `celld` branch (tag `celld-final`) on celld until
it moves to Cloudflare. MIT licensed; see [LICENSE](LICENSE).

## Use it

fragment.club is invite-only for now. Install the CLI (macOS or Linux,
no sudo; put `~/.local/bin` on your PATH if it is not), then pair it
with you:

```
mkdir -p ~/.local/bin && curl -fsSL https://github.com/futurepaul/fragment/releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin
fragment login
```

`fragment guide` is the manual; `fragment skill` prints a SKILL.md for
your coding agent.

## Try it locally

One-time setup:

```
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.5 --locked
npm ci                     # the pinned wrangler (Node 22 or later)
```

Then:

```
cargo xtask dev            # the cell and its agents on :8790 under wrangler dev, the code.storage and WorkOS fakes
cargo xtask try todo       # in another terminal: todo | inbox | notes
```

`try` creates and deploys a fragment from a template under
`target/devstack/try/`, and prints the link to open and a `fragment`
alias pointed at the dev stack. Fragments are served at
`http://<label>--<username>.fragment.localhost:8790/`.

## Tests

```
cargo xtask check          # host tests, clippy on host and wasm, warnings denied
cargo xtask e2e            # the full suite against a fresh wrangler dev node (--only a,b | --except a,b)
```

The e2e stages its own copy of the cell, so it runs alongside
`cargo xtask dev`. Its browser sections drive headless Chrome
(`CHROME_BIN` to choose one).

## Layout

```
cell/          the cell: router, registry, fragment supervisor, jobs, files, blobs, deliveries, ledger
agent/         the agents' Worker (goose's loop)
cli/           the fragment CLI and GUIDE.md (the agent guide)
crates/proto   wire types and limits
crates/core    the cell's pure logic, host-tested (schemas, cron, globs, the ledger and price book, web push)
crates/nip98   NIP-98 signing and verification
crates/templates  templates/, embedded in the CLI and the cell
crates/fakes   code.storage, Workers AI, WorkOS, and push-service fakes

crates/devstack  runs wrangler dev and the fakes
crates/e2e     the end-to-end suite
templates/     blank, calories, inbox, notes, todo
xtask/         build, dev, try, check, e2e, deploy, teardown
deploy/        example.jsonc: a deployment's config (yours lives outside the repo)
docs/          model, contract, roadmap, phase records, the debt ledger
```
