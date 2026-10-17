# fragment-next

Fragment publishes stateful, multiplayer web apps from the CLI. Sign in,
pair the `fragment` CLI (or hand your coding agent the platform's
`/llms.txt`), and publish. The platform lists your fragments, and serves
each at its own address.
Hosting, permissions, sharing, crons, jobs, and channels are built in.

A fragment is a folder of files in git, an app of named operations over
its own SQLite, channels that pages follow live, and members with roles.
Your **agents** are fragments too, run by a computer of your own (today,
Hermes profiles), and act on other fragments as their members.
Fragments run on Cloudflare: each is a Durable Object that sleeps when
idle, its app in a Worker of its own
([docs/cloudflare-v1.md](docs/cloudflare-v1.md)), and so do computers
([docs/computers.md](docs/computers.md)).

This repo holds:

- **the cell** (`cell/`): Rust (workers-rs) on Cloudflare Workers, with a
  small JavaScript shim. It answers the control API, serves sites, runs each
  fragment's app in its own loaded worker, and runs jobs as Workflows.
- **the `fragment` CLI** (`cli/`): the whole control surface, built for
  agents. `fragment guide` prints the agent guide.
- **the native iOS preview** ([ios/](ios/README.md)): a minimal SwiftUI
  client against a local HTTP/WebSocket fixture; production QR pairing is
  planned in [docs/ios.md](docs/ios.md).
- **the harness** (`xtask/`, `crates/`): the dev stack, Rust fakes for
  code.storage, Workers AI, WorkOS, and a push service, and the e2e suite
  that drives the real cell, CLI, and a browser.

Read [docs/MODEL.md](docs/MODEL.md) for the model,
[docs/api.md](docs/api.md) for the wire contract, and
[docs/cloudflare-v1.md](docs/cloudflare-v1.md) for where it is going.
This repo is [finitecomputer/fragment](https://github.com/finitecomputer/fragment);
fragment.club runs its `celld` branch (tag `celld-final`) on celld until
it moves to Cloudflare. MIT licensed; see [LICENSE](LICENSE).

## Use it

Production will be https://finite.chat (its fragments at
`*.finite.place`; Paul, 2026-10-16); a preview is its own `<branch>`
copy, master's at https://preview.finite.build. To start, tell
your coding agent (Claude Code, Codex), naming the platform you use:

```
Read https://finite.chat/llms.txt and help me build: …
```

Its `/llms.txt` is filled in with that platform's own URLs (docs/api.md,
Agent docs); its settings give the same prompt to copy. Or install the
CLI yourself (macOS or Linux, no sudo; put `~/.local/bin` on your PATH if
it is not), then pair it with you:

```
mkdir -p ~/.local/bin && curl -fsSL https://github.com/finitecomputer/fragment/releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin
fragment login --host https://finite.chat
```

`fragment login --host` saves its host for every later command; with
none ever named, the CLI calls https://finite.chat.

`fragment guide` is the manual; `fragment skill` prints a SKILL.md for
your coding agent.

## Try it locally

One-time setup:

```
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.5 --locked
```

No Node to install: xtask fetches the pinned one into `target/tools`
and runs its npm (below).

Then:

```
cargo xtask dev            # the cell on :8790 under wrangler dev, the code.storage and WorkOS fakes
cargo xtask try todo       # in another terminal: todo | inbox | notes
```

`try` creates and deploys a fragment from a template under
`target/devstack/try/`, and prints the link to open and a `fragment`
alias pointed at the dev stack. Fragments are served at
`http://<name>.fragment.localhost:8790/`.

## Tests

```
cargo xtask check          # node --check on our JavaScript, host tests, clippy on host and wasm, warnings denied
cargo xtask e2e            # the full suite against a fresh wrangler dev node (--only a,b | --except a,b)
cargo xtask e2e --shard 2/4
                           # one of CI's four shards (crates/e2e/src/lanes/mod.rs, SHARDS)
```

The e2e stages its own copy of the cell, so it runs alongside
`cargo xtask dev`. Its browser sections drive headless Chrome
(`CHROME_BIN` to choose one).

## The pinned Node

The JavaScript the platform needs (wrangler, and the Sandbox SDK it
bundles; `package.json`) runs on one Node release, pinned in
`crates/devstack/src/node_release.rs` with the SHA-256 of each
platform's official tarball. `dev`, `e2e`, `deploy` and `teardown`
fetch it into `target/tools/` on first use (refusing a tarball whose
hash differs), run its own npm's `npm ci` when node_modules is missing
or stale, and start every JavaScript process on it, first on PATH, with
its caches under `target/cache/` (Browser Rendering's Chrome included).
`check` fetches it too, and runs `node --check` on it (no `npm ci`).
A `node` on PATH is never used, and there is no other: a machine the
pin has no tarball for runs nothing.

To move the pin:

1. Pick the release from https://nodejs.org/dist/index.json: the newest
   of an LTS line that wrangler's `engines` allows.
2. Fetch `https://nodejs.org/dist/v<version>/SHASUMS256.txt` and its
   `SHASUMS256.txt.sig`, and check the signature against the releaser's
   key from nodejs/node's README (`gpg --verify SHASUMS256.txt.sig
   SHASUMS256.txt`).
3. Set `NODE_VERSION`, and each `TARBALLS` hash to its
   `node-v<version>-<platform>.tar.gz` line (darwin-arm64, darwin-x64,
   linux-arm64, linux-x64).
4. `cargo xtask check`, then `cargo xtask e2e`: the first run fetches
   the new Node and runs `npm ci` with it. CI's cache of `target/tools`
   keys on that file, so it fetches afresh too.

## Layout

```
cell/          the cell: router, registry, fragment supervisor, jobs, files, blobs, deliveries, ledger
cli/           the fragment CLI and GUIDE.md (the agent guide)
crates/proto   wire types and limits
crates/core    the cell's pure logic, host-tested (schemas, cron, globs, the ledger and price book, web push)
crates/nip98   NIP-98 signing and verification
crates/templates  the release's files (templates/, the shell, the agent docs): embedded in the CLI, an index in the cell (their bytes are its Static Assets)
crates/fakes   code.storage, Workers AI (and Finite Private and OpenRouter: its OpenAI-compatible face), WorkOS, and push-service fakes

crates/devstack  runs wrangler dev and the fakes
crates/e2e     the end-to-end suite
templates/     blank, calories, doc, inbox, notes, todo; the blessed agent, brain, chat, skills
xtask/         build, dev, try, check, e2e, deploy, teardown
deploy/        example.jsonc: a deployment's config (yours lives outside the repo)
docs/          the plan (cloudflare-v1.md), model, contract, the debt ledger
```
