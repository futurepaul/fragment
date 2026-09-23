# Phase 1 spikes

Each spike ends in a verdict with numbers (ROADMAP phase 1). Run on
2026-09-23 against celld 0.5.1 (`celld dev`, one node, local store) on an
M-series Mac.

| Spike | Verdict | Where |
|---|---|---|
| 1. Rust platform cells | Adopt: workers-rs covers the supervisor with a ~35-line JS shim; ~9 ms once per isolate, 0.05–0.2 ms per request | `cells-rs/README.md` |
| 2. The app facet as author SQL | Adopt, with synchronous mutations and a facet-local ledger; the root transaction cannot enclose a facet past ~1.6 MB | `apps/README.md` |
| 3. Deterministic agent turns | Adopt: a libfx turn is a Workflow; SIGKILL mid-turn repeats no finished call | `agent-turns/README.md` |
| 4. celld v0.4.0 → v0.5.1 | Adopt; one alarm regression recorded | `celld-0.5.1/README.md` |

## Running them

```sh
cd spikes/cells-rs && worker-build --release && cd ..
(cd agent-turns && npm ci)
(cd driver && cargo build --release)
export CELLD_ESBUILD=$PWD/../.dev/tools/node_modules/.bin/esbuild
./driver/target/release/spike-driver . spike1.json   # spikes 1 and 2: 54 checks
./driver/target/release/turns . spike3.json          # spike 3: 13 checks
```

The drivers start and stop their own `celld dev` nodes on ports 8811,
8812, and 8821. These folders are spike code: phase 2 ports what they
prove into the product and deletes them.
