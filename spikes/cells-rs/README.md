# Spike 1: Rust platform cells (workers-rs on celld)

Status: done 2026-09-23. Verdict: **the platform cells can be Rust.**
Every primitive the supervisor needs works from workers-rs 0.8.5 on celld
0.5.1. The cost is a ~35-line JavaScript shim, about 9 ms once per
isolate, and a fraction of a millisecond per request.

## What was built

`src/lib.rs` is a supervisor Durable Object in Rust: an operation ledger
and channels in its own SQL, a hibernatable WebSocket, an alarm, and an app
facet started from author JavaScript through the Worker Loader, with typed
errors, limits, and assertions. `../cells-js/index.js` is the same
supervisor in JavaScript, as the baseline. `../driver` (`spike-driver`)
starts each one under `celld dev`, runs 27 checks on each, and measures
them. `../apps/todo.mjs` is the author app; `../apps/platform.mjs` is the
platform code that runs inside the facet (spike 2).

## What works, and how

| Need | workers-rs 0.8.5 | How |
|---|---|---|
| SQL | yes | `state.storage().sql()` |
| Alarms | yes | `set_alarm`, the `alarm` handler |
| Hibernatable WebSockets | yes | `accept_web_socket`, `websocket_message` |
| Worker Loader | no API | `js_sys::Reflect` on the raw `env`: `LOADER.get(id, getCode)` with a `Closure::once_into_js` callback |
| Facets | no API | `Reflect` on the raw `ctx` (`State::_inner()`): `ctx.facets.get/abort`, RPC calls on the stub |
| Capabilities for loaded code | no `WorkerEntrypoint` | a JavaScript `WorkerEntrypoint` class in `entry.mjs`, created with `ctx.exports.Channel({ props })` |
| RPC into the supervisor | refused | celld refuses RPC to a class that does not `extend DurableObject`; `entry.mjs` declares `class Supervisor extends DurableObject` and forwards every handler to the Rust object |

The shim (`entry.mjs`, 1.4 KB) is the only hand-written JavaScript on the
platform side. The facet wrapper (`platform.mjs`) is JavaScript because it
runs inside the author's isolate, not because Rust cannot host it.

For phase 2, the `Reflect` calls become a small typed module (the
equivalent of `worker-sys` for loaders and facets), so the stringly-typed
surface lives in one place.

## Checks (27 per supervisor, all passing on both)

Valid, invalid, replay, and conflicting-body calls; an author throw rolls
back its write and leaves the operation id unused; an async mutation is
refused and rolled back; a supervisor failure after the facet committed
turns the retry into a replay with no second write; the facet cannot see
supervisor tables, cannot `fetch` (`globalOutbound: null`), and cannot set
an alarm; a query's capability call re-enters the supervisor; the alarm
fires; a WebSocket frame is appended and answered; after a restart the
replay still returns the stored result and the facet's rows survive;
`cpuMs` stops a runaway facet call and the cell answers the next call.

## Numbers

One `celld dev` node on an M-series Mac, local object store; p50 unless
noted. From `../results/spike1.json` (a second full run; the first was
within ~10% everywhere except cold starts, which vary by a few ms).

| | Rust | JavaScript |
|---|---|---|
| Bundle | 517 KB wasm (181 KB gzip) + 28 KB glue + 1.4 KB shim | 8.3 KB |
| Warm no-op request | 0.26 ms | 0.21 ms |
| Warm facet query | 0.56 ms | 0.38 ms |
| Warm mutation (durability-bound) | 8.2 ms | 7.9 ms |
| Warm supervisor-only write | 7.2 ms | 6.9 ms |
| First request after a process restart | 10.4 ms | 1.6 ms |
| First facet call after a restart | 11.2 ms | 5.8 ms |
| First request after idle eviction | 9.8 ms | 6.5 ms |
| `cpuMs: 200` runaway facet call stopped after | 212 ms ("Worker exceeded CPU limit of 200 ms") | 210 ms |
| `celld dev` start to ready (includes the dev build) | 4.4 s | 4.4 s |

Reading: Rust adds ~9 ms the first time an isolate compiles and
instantiates the wasm (celld then shares the compiled module across
isolates in the process) and 0.05–0.2 ms per request for crossing into
and out of wasm. Durability dominates every write either way.

## What this changes

- The TypeScript-runtime debt entry can be deleted by the core cut: the
  supervisor, computer, and agent cells are written in Rust.
- Facet cost and the transaction rule are spike 2 (`../apps/README.md`).
