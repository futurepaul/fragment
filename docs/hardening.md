# Hardening pass (proposed)

Status: **proposed 2026-09-24, before building more on phase 4's
abstractions.** Paul asked for it once fragment.club was live: keys out
of JavaScript, and the isolation findings acted on. Sources: the native
services spike (`spike/native-services`, cd1d6d8, its README), and the
two isolation spikes (`spike/cell-isolation` 35c51f1; `spike/isolation`
dadbadb, the zcode session `sess_8a882b9c…`), both run 2026-09-23 against
celld 0.5.1 with our fork.

## Where each finding stands today

| Finding | State on fragment.club | Where |
|---|---|---|
| Tenants with identical app source shared one V8 realm | **fixed** (one loaded worker per fragment) | `cell/src/ops.rs` |
| A supervisor fetch could reach celld's internal listener or 6PN (SSRF; `/do/<scope>` answered with forged principal headers, confirmed live) | **mitigated** by the fork's public-only egress (`CELLD_EGRESS_PUBLIC_ONLY`, phase 3 slice E; the hosted e2e checks loopback and 6PN) | fork `cd3a68b` |
| celld's internal listener has **no auth** (`/state`, `/do`, `/evict`, `/shutdown`) | **open**: it listens on the fleet's private network (8081), which every app in the Fly org can reach | fork, Tier 1 |
| Fleet secrets are Worker `vars`: literals in every isolate's generated source, and plaintext in the deployment manifest in the bucket | **open, and grew in phase 4**: now also `WORKOS_API_KEY` and `OPENROUTER_MANAGEMENT_KEY` | debt ledger "The fleet's secrets live in the bucket" |
| Key material handled in wasm/JS heaps: the host secret (sealing), the code.storage org key (JWTs), each agent's nostr key (NIP-98 signing), session and token generation | **open** | `secrets.rs`, `cs.rs`, `agent/`, `registry/signin.rs` |
| A facet's SQLite has no size cap in celld (grew to 42 MB in the probe) | **open**: the 16 MiB cap is decided, not enforced | Tier 0 |
| One node allows 255 loaded workers per (script, generation), never released; one tenant can exhaust it and brick every other app (confirmed live) | **open**: with one loaded worker per fragment, a node holding more than 255 fragments' apps stops loading new ones | Tier 0 and fork |
| Code generation from strings (`eval`, `new Function`, Wasm from bytes), `SharedArrayBuffer`, `Atomics.wait` in facets | **open** | fork, Tier 1 |
| No hard heap cap per isolate (a facet can grow the node to OOM) | **open** | fork, Tier 1 |
| Facet egress (`globalOutbound: null`), CPU limits, SQL authorizer, frozen clocks | fine | — |

On "JS in a secure random path": `crypto.getRandomValues` is workerd's
native generator (BoringSSL) behind a JS name, so the randomness itself
is not computed in JavaScript. What is exposed is where the results and
the long-lived keys *live*: in isolate heaps and in the manifest. The
steps below remove both.

## The plan, in order

**H1. `KEYS`, a native service in our celld fork** (the spike's seam:
12 lines in 4 upstream files plus `native_seam.rs`; our code in
`crates/native`, depending on `fragment-core`).
- Rebase the seam from the spike's base (`b5f57ea`) onto our current
  fork (`cd3a68b`, egress/public-only).
- Fleet secrets move from `vars` to the node's environment (Fly
  secrets): the host secret, the code.storage org key, the WorkOS API
  key, the OpenRouter management key. They leave every isolate and the
  bucket's manifest.
- The cell asks `KEYS`, which knows the calling cell (the host attests
  it; JS cannot forge it), for: sealing and opening (salted with the
  caller), code.storage JWTs (checked against the fragment's own repo),
  random tokens and ids (sessions, redemptions, agent keys), and a small
  set of credentialed calls with fixed hosts (WorkOS's code exchange,
  OpenRouter's key API), so no JS holds those keys even briefly.
- Agents' keys: generated and held by `KEYS` (sealed to the agent's
  cell); the agent cell asks `KEYS` to sign each NIP-98 header.
- workers-rs refuses celld's service stubs (`Fetcher` name check): a
  few lines of JS glue in `entry.mjs`, or the one-line upstream fix.
- Numbers from the spike: a `KEYS` call ~55–70 µs from a cell; an ES256
  JWT 184 µs natively vs 320 µs in the isolate.
- Needs Paul: a node image rebuild and rolling deploy, and the Fly
  secrets set (the fleet file names which files hold them).

**H2. Tier 0, platform rules (no fork).**
- Enforce the 16 MiB app-facet cap: refuse a mutation that would pass
  it, with a test.
- The loaded-worker budget: count loaded fragments per node generation;
  until the fork can release workers (H3), cap it and fail visibly (a
  clear 503 "this node is full") instead of bricking other fragments.
- Audit that every hop overwrites identity headers (the router already
  passes an allowlist; recheck the agent service and deliveries).

**H3. Tier 1, the fork (mechanical, upstreamable where possible).**
- Authenticate the internal listener (a shared HMAC from the node's
  environment, or a unix socket for local operator commands).
- Loaded workers: no code generation from strings, no
  `SharedArrayBuffer`, no `Atomics.wait` (V8 flags for loaded workers).
- A hard heap cap that ends the isolate, not the node.
- Release memoized loaded workers (per-tenant accounting), which lifts
  H2's cap.

**H4. Tier 2, deployment.**
- The fleet on its own Fly private network (not the org's default
  6PN), reached only through its public services.
- Tigris keys scoped to the fleet's prefixes; non-root in the image.
- Needs Paul: Fly network and Tigris changes.

**Not in this pass: Tier 3, cordons** (separate fleets per trust tier,
so strangers are never co-resident with friends). Needed before public
sign-up or strangers' code, not for an invite-only alpha.

## Evaluation

- H1: an e2e check that no fleet secret appears in the rendered
  manifest or in a cell's `env` (a probe reads `env` and the deployment
  object); sealing across cells refused; a JWT for another fragment's
  repo refused; the existing 622 local checks and the hosted e2e pass.
- H2: a facet write past 16 MiB refused; the loaded-worker cap answers
  503 and leaves other fragments serving.
- H3: the isolation spikes' probes rerun against the fork: `/state` and
  `/do` refused without the HMAC; `eval` and `SharedArrayBuffer` gone;
  a heap bomb ends its isolate only.
- H4: from another app in the org, the fleet's private address does not
  answer.

## Order and size

H1 is the largest (the fork seam, a crate, the cell's key paths, a node
deploy). H2 is small and needs nothing from Paul. H3 is fork work in the
same rebuild as H1. H4 is configuration. Suggested: H2 and H1 together
(one node deploy), then H3 (a second), then H4.

## Starting points (for whoever picks this up)

- **Paul's go:** asked 2026-09-24 ("are you ready to do the h1-h4?")
  after fragment.club went live on phase 4 and the live-socket fix
  (3b1fbc3). Order: H2 with H1 (one node deploy), then H3, then H4. Each
  node image deploy, the Fly secrets, and the Fly network and Tigris
  changes are still his to approve at the moment they happen.
- **The fork today:** `futurepaul/celld` at `cd3a68b` (branch
  `egress/public-only`: the alarm fix plus public-only egress), pinned in
  `crates/devstack/src/lib.rs` (`CELLD_FORK_REV`) and built by
  `cargo xtask celld`; the node image by `cargo xtask deploy fragment-club
  --nodes` (local OrbStack build, rolling update).
- **The seam:** `celld-worktrees/native-services` at `f373feb` ("fork
  seam: service bindings to native:<name> reach fragment-native"), based
  on `b5f57ea`; rebase it onto `cd3a68b`.
- **The crate:** `fragment-next-worktrees/native-services` at `cd1d6d8`,
  `crates/native` (`lib.rs`, `keys.rs`, `scope.rs`, `echo.rs`) and the
  probe in `spikes/native-services/`; bring it onto `main` (it predates
  phase 2's later slices and phase 4).
- **The isolation probes** to rerun for H3: `spike/isolation` (dadbadb)
  and `spike/cell-isolation` (35c51f1), `spikes/isolation/probe/`.
- **Keys in the cell now:** `crates/core/src/secrets.rs` (sealing),
  `cell/src/cs.rs` (the code.storage JWT), `cell/src/js.rs`
  (`random_bytes`), `cell/src/registry/signin.rs` (session and
  redemption tokens), `cell/src/ledger.rs` (the org's OpenRouter key,
  sealed), `cell/src/auth.rs` (the WorkOS exchange), `agent/src/lib.rs`
  (agent keys, sealed; NIP-98 signing in `agent/src/fleet.rs`).
- **Fleet secrets now:** `fleets/fragment-club.json` `secret_vars`
  (host secret, code.storage key, WorkOS client ID and key, OpenRouter
  management key): rendered into `vars` by `xtask/src/deploy.rs`, which
  H1 changes to Fly secrets on the node.
