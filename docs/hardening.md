# Hardening pass

Status: **H1–H3 deployed to fragment.club on 2026-09-24** (Paul: "proceed
with h1-h4", then each deploy step). The bucket clean-up and the secret
rotations wait on Paul (below). The celld fork is branch `hardening` at
`2779418` on futurepaul/celld (`celld-worktrees/hardening`; `43c3238` after it adds only a lockfile line for phase 6). Sources: the native services spike
(`spike/native-services`, cd1d6d8), and the two isolation spikes
(`spike/cell-isolation` 35c51f1; `spike/isolation` dadbadb).

## Where each finding stands

| Finding | State (in code) | Where |
|---|---|---|
| Tenants with identical app source shared one V8 realm | fixed (phase 4: one loaded worker per fragment) | `cell/src/ops.rs` |
| A supervisor fetch could reach celld's internal listener or 6PN (SSRF) | mitigated since phase 3 (`CELLD_EGRESS_PUBLIC_ONLY`); **now also closed at the listener** | fork `cd3a68b`, `2779418` |
| celld's internal listener has no auth (`/state`, `/do`, `/evict`, `/shutdown`) | **fixed**: `CELLD_INTERNAL_PEER_ONLY=1` serves only the fleet-signed routes (`/peer/*`, `/runtime/`) | fork `2779418` |
| Fleet secrets are Worker `vars` (in every isolate, and in the manifest in the bucket) | **fixed**: they are the node's environment (Fly secrets), read only by `KEYS`; a cell deploy refuses a var that holds one. The old deployments' manifests still hold them (debt ledger) | `crates/native`, `xtask/src/deploy.rs` |
| Key material in wasm/JS heaps (sealing, the code.storage key, agents' nostr keys) | **fixed** for the fleet's keys and the agents' and fragments' own keys; tenant secrets (a fragment's `{{NAME}}`, an org's OpenRouter key, a VAPID key, a computer's token) still open in the cell at the egress point, now only for the cell that sealed them | `cell/src/keys.rs`, `agent/src/keys.rs` |
| A facet's SQLite has no size cap | **fixed**: 16 MiB per app (a mutation past it rolls back, 507), and the node's hard stop 4 MiB above | `cell/platform.mjs`, fork `cbea45e` |
| 255 loaded workers per node, never released; one tenant can fill a node | **visible**: the next app answers 503 `node_full` and the rest serve on. Release is not built (debt ledger) | `cell/src/js.rs`, fork `c4d64a9` |
| Code generation from strings, `Atomics.wait` in facets | **fixed**: `CELLD_DYNAMIC_LOCKDOWN=1` | fork `c4d64a9` |
| No hard heap cap per isolate | **fixed** for the V8 heap: past twice its limit (128 MiB) an isolate's execution ends; ArrayBuffer memory is still uncounted (debt ledger) | fork `c4d64a9` |
| Facet egress, CPU limits, SQL authorizer, frozen clocks | fine | — |

## What was built

### H1. `KEYS`, a native service in our celld fork

- **The seam** (fork `ddd220d`, `6a194d5`): a service binding whose
  target is `native:<name>` is answered by `crates/native` instead of a
  script, with the calling cell's scope as the host attests it (JS cannot
  claim another cell's). celld builds our crate from fragment-next checked
  out beside the fork as `fragment/` (`cargo xtask celld` links it; the
  node image copies it there).
- **`KEYS`** (`crates/native/src/keys.rs`, 12 host tests): only a cell may
  call. It seals and opens values for the caller alone (`w2`, salted with
  the attested scope; `w1` values sealed before `KEYS` open when the
  caller names their old salt, and come back resealed), makes nostr keys
  and signs NIP-98 headers and key proofs with them, signs code.storage
  JWTs (for `Fragment` cells), makes WorkOS's code exchange (for the
  `Registry`; the answer's refresh token is dropped), and calls
  OpenRouter's key API (for `Ledger` cells). Its keys come from
  `FRAGMENT_KEYS_*` in the node's environment.
- **The cell** asks `KEYS` for all of that (`cell/src/keys.rs`, through
  `js::service_post`: workers-rs refuses celld's service stubs). The
  WorkOS exchange moved from the router into the registry
  (`/login/exchange`). A fragment's own key is made by `KEYS` (the create
  body lost `fragmentSecret`). `fragment_core::secrets` keeps only the
  `{{NAME}}` parser.
- **The agent** (`agent/src/keys.rs`): its nostr key is made by `KEYS`
  and signs there (`Signer` in `agent/src/fleet.rs`); its computer's
  token is sealed for it.
- **Fleet config:** the fleet file's `secret_vars` became `node_secrets`
  (Fly secrets, `FRAGMENT_KEYS_*`, staged by `deploy --nodes` from stdin,
  values never printed) and `var_files` (WorkOS's client id, not secret).
  `cargo xtask deploy` refuses a Worker variable that is a fleet secret by
  name or value (a unit test). dev and the e2e pass the same settings to
  the node's environment (`devstack`).
- **Randomness stays with `crypto.getRandomValues`.** Paul asked for no JS
  in a secure random path. celld's `getRandomValues` is a host op,
  `op_webcrypto_random` → `getrandom::fill`: the same OS source `KEYS`
  would use, reached through the same kind of JS binding a `KEYS` call
  is. Routing tokens through `KEYS` would add a hop without changing where
  the bytes come from, so it was dropped from the plan; the doc comments
  on `js::random_bytes` say so. Keys that must never be in a heap
  (fragments' and agents' nostr keys) are made in `KEYS`.
- **Deviation from the plan:** a code.storage token is not tied to the
  fragment's own repo. Repo ids are opaque, and supervisors share a V8
  context (up to 32 cells), so binding it would not stop a compromised
  context. It is in the debt ledger with its delete condition.

### H2. Platform rules

- **The app database cap** is the node's, not only the platform's: the
  plan's JS check could be bypassed by author code writing from a query
  or `fetch`. The fork caps every facet database
  (`CELLD_FACET_MAX_BYTES`, SQLite's `max_page_count`, which JS cannot
  raise); the platform refuses a mutation that leaves the database over
  16 MiB (it rolls back: 507 `storage_full`). The hard stop sits 4 MiB
  higher because celld writes its own bookkeeping into the facet's
  database on every call: with no headroom, an app at the cap refused
  even the call that would empty it (found by the e2e).
- **A full node says so:** celld's "too many loaded workers" becomes 503
  `node_full` naming the node, not an app failure, and the fragment's
  other routes keep working. The fork's `CELLD_LOADED_WORKERS_MAX` lets
  the e2e fill a node with three apps.
- **Identity headers:** audited; nothing to fix. The router builds each
  hop's headers fresh from an allowlist and sets identity itself; the
  agent's router does the same; the internal headers (the delivery
  report, the files capability, jobs, the ledger's org) are set only by
  our own code on paths the router never forwards. The one other way in
  was celld's internal `/do/`, closed in H3.

### H3. The fork

- **The internal listener** (`2779418`): `CELLD_INTERNAL_PEER_ONLY=1`.
  Checked by hand on a local node: with it, `GET /state` and `GET
  /do/<scope>` answer 403; without it, 200 and reachable; unsigned
  `/peer/probe` is 401 either way. (celld dev hides its internal port, so
  the e2e cannot reach it.) A node then stops on its signal: Fly sends
  SIGTERM, and `celld dev` falls back to it.
- **Loaded workers** (`c4d64a9`): `CELLD_DYNAMIC_LOCKDOWN=1` turns off
  code generation from strings (before the module's first line runs) and
  `Atomics.wait`. A heap past twice its limit ends the isolate's
  execution ("Worker exceeded its memory limit of 128 MiB"); the app
  answers again on its next call, and its neighbors never notice.
- **Not built:** releasing loaded workers. celld memoizes a named loaded
  worker in each host isolate's `byName` and in a process registry, and a
  facet holds its class; releasing one safely means evicting all three
  together. It is the one H3 item left, in the debt ledger, and the H2
  503 covers it until a node nears 255 apps.
- **Upstreamable:** each setting defaults to upstream's behavior and
  lives in its own commit (`cbea45e`, `c4d64a9`, `2779418`).

## Evidence

- `cargo test -p fragment-native` (12), `-p fragment-core`, `-p xtask`
  (the fleet file parses; no fleet secret can be a var; the rendered
  fly.toml carries the settings).
- Local e2e on the pinned fork (`cargo xtask e2e`, the release build): 653
  passed, 0 failed. The new lanes are `keys` (no
  fleet secret in the cell's variables or the rendered `.dev.vars`; a
  fragment is made with a `KEYS` key; one fragment cannot open another's
  sealed value), `facet-cap` (the cap, rollback, emptying at the cap,
  the hard stop from a query, neighbors unaffected), `app-lockdown`
  (`eval`, `new Function`, `Atomics.wait` refused; a heap bomb ends only
  its own turn), and `node-full` (503 for the third app on a node with
  room for two; the others serve on). Every earlier lane passes through
  `KEYS`: sign-in (WorkOS's exchange), budgets (OpenRouter's key API),
  every create (a code.storage token), agents (their keys and proofs).

## Deploying it (Paul approved each step, 2026-09-24)

1. **Done:** the fork branch `hardening` is on futurepaul/celld.
2. **Done:** nodes first (`deploy --nodes`, the four Fly secrets staged
   and rolled with image `node-2779418-e55f665`), then the cell. The
   deployment's Worker variables hold no secret.
3. **Done:** the hosted e2e, 29/29, live AI included. (Its first run
   found a phase 4 bug: a fragment from before phase 4 panicked in its
   constructor, and each panic reset the wasm app shared by its isolate,
   so other fragments' AI steps never finished; fixed in the next commit
   and redeployed.) From inside both Machines, the internal listener
   answers 403 to `GET /state` and `GET /do/<scope>`.
4. **Done by Paul:** the nine earlier deployments deleted from the bucket
   (27 objects), and the rotated host secret set (`--secrets`). The
   hosted e2e passes after it, 29/29.
5. **Open:** new WorkOS, OpenRouter management, and code.storage keys,
   and the bucket-scoped Tigris key (debt ledger: "Three fleet secrets
   that sat in the bucket are not rotated yet").

## H4. Deployment (decided with Paul, 2026-09-24)

- **Tigris keys scoped to the fleet's bucket: now.** Tigris makes a key
  scoped to one bucket in its console (Access Keys, bucket
  `fragment-club-ord`, Editor); its IAM API can too, but its "no default
  access" option is not documented well enough to trust on Paul's org.
  The key replaces the app's `AWS_ACCESS_KEY_ID`/`AWS_SECRET_ACCESS_KEY`
  secrets and the operator's `fragment-club-bucket.env`; then the old
  key is revoked.
- **The fleet's own private network: later**, with Tier 3's cordons
  (debt ledger: "The fleet shares Fly's default private network").
- **Non-root in the image: later**, in a node deploy that touches the
  image anyway (debt ledger: "celld runs as root in the node image").

**Not in this pass: Tier 3, cordons** (separate fleets per trust tier,
so strangers are never co-resident with friends). Needed before public
sign-up or strangers' code, not for an invite-only alpha.
