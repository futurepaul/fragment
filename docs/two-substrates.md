# fragment on Cloudflare, or entirely self-hosted

*Proposed 2026-09-30 (Paul: "very compelling if I could ship a version of
fragment that could run entirely on Cloudflare Workers AND run entirely
self-hosted").*

One fragment, shipped two ways from the same code:

- **On Cloudflare:** Workers and Durable Objects for the platform and
  every cell, Dynamic Workers and Durable Object Facets for each
  fragment's app, R2 for blobs, and Cloudflare Containers for computers.
- **Self-hosted:** celld for the platform and cells (it runs the Workers
  and Durable Object model on your own machines, its state in a bucket),
  and sandcastle for computers on your own boxes.

The operator chooses the substrate. A developer, and a person using a
fragment, cannot tell which one they are on. This doc is the shape that
makes that true, what each substrate supplies, what the code must change,
and the phases.

The two are not the same promise:

- **Cloudflare is fragment for everyone.** Strangers publish code and
  run computers, isolated by Cloudflare's own sandboxing (Dynamic
  Workers, Containers), with the platform's secrets in a Worker of their
  own. celld calls itself not safe for hostile multi-tenant use while it
  is alpha (docs/technical-debt-ledger.md, the first entry), so this is
  also the way out of that entry for the hosted product.
- **Self-hosted is fragment for your own people:** a family, a team, an
  org, on machines they run, with the privacy that gives.

## Why it is plausible now

- **The platform is already Workers-shaped.** fragment's cell is Rust
  compiled to wasm against the Workers API, on celld, which implements that
  model on our own nodes. The cell does not know it is not on Cloudflare,
  except where celld's fork adds things (below).
- **Cloudflare now runs fragment's app model.** A fragment's `app.mjs` runs
  as a facet of its fragment's cell: loaded at runtime, with its own SQLite,
  supervised by the cell. Cloudflare shipped exactly that: Dynamic Workers
  (the Worker Loader, `env.LOADER.get`) and Durable Object Facets
  (`ctx.facets.get`, `abort`, `delete`), in open beta on the paid plan.
- **Cloudflare now runs sandcastle's computer model.** A Cloudflare
  Container is controlled by its own Durable Object (`ctx.container`),
  started on a request, asleep when idle, and its outbound traffic can be
  intercepted by Worker code outside it, TLS included. That is sandcastle's
  router, tiers, and credential swap, as platform APIs.

## Decisions this rests on (Paul, 2026-09-30)

- **One computer wherever it runs** (docs/runtime-seam.md): presets
  (`hermes`, `goose`), placement outside the manifest, one trust floor.
- **The platform may be in the data path.** Keeping a person's data from
  the platform is not a design driver:
  - on Cloudflare, fragment manages everything and holds it, as it holds
    every fragment's data today;
  - self-hosted, the operator's machines are their own;
  - a person who connects their own computer to a hosted fragment knows
    the platform could see what passes through, as with the fragments they
    publish there and the agents it runs.

  Privacy is what self-hosting gives. This refines runtime-seam.md's
  decisions 4 and 5: a computer is reached **through the platform**, and
  iroh becomes a later optimization (a native app on the same network),
  not the transport.

## The layers

| Layer | On Cloudflare | Self-hosted |
|---|---|---|
| Edge, routing, TLS | Workers routes and custom domains | a celld node (Fly today, any box) |
| Cells: the platform, each fragment, each identity | Durable Objects with SQLite | celld cells, replicated to its bucket |
| A fragment's app code | Worker Loader and Durable Object Facets | celld's same APIs |
| Blobs and files | R2 | an S3-compatible bucket |
| Fleet secrets and signing (`KEYS`) | a `keys` Worker behind a service binding, its secrets in its own Worker secrets | `KEYS`, native in celld's fork |
| Computers | Containers under their own Durable Object | sandcastle nodes (microVMs, ZFS) |
| A computer's credentials | the outbound handler, in the computer's own object | the node's swap, from the platform's credential source |
| Sleep and wake | the object starts its container on a request; sleep after idle | tiers: warm (paused), cold (stopped) |
| Durable `/data` | an archive in R2, saved by the object (below) | a ZFS volume, snapshotted and shipped |
| A person's own computer | the same: it dials out to the platform | the same |

## Computers: one seam, two hosts

*Superseded in part, 2026-09-30:* upstream celld already implements
Cloudflare's `ctx.container` (on Docker or Podman), so the seam is that
API itself. fragment writes one host over `ctx.container`, and
`SandcastleHost` becomes a sandcastle engine inside celld.
docs/containers-on-celld.md is the audit: what celld and sandcastle add,
method by method, and the order. What follows is the design as first
written.

A computer's cell (the `Computer` cell, reshaped by runtime-seam.md's
decisions) talks to its host through one Rust trait in the cell, a
**computer host**:

- converge to a spec (an image or preset, size, service, the credentials
  it needs);
- look, without waking it;
- wake, sleep, hold awake while a turn runs;
- serve an HTTP request or a WebSocket to its service;
- exec and files (the Sprites-shaped `Computer` cell needs them: the CLI's
  install, file sync, jobs);
- save and restore `/data`; update to a new image, keeping `/data`, with
  rollback;
- destroy.

Two hosts implement it:

- **`ContainersHost`**, on Cloudflare: the computer's own Durable Object
  holds `ctx.container`. No API between them, no grants, no signing: the
  object is the controller.
- **`SandcastleHost`**, self-hosted: the cell signs calls to a sandcastle
  node's API (NIP-98, grants, owner keys: sandcastle/README.md), and sends a
  computer's requests to the node's router.

The same contract, spoken over sandcastle's HTTP API, is what a platform
that is not built on Workers (Finite's Core) would call. A small Worker
could serve sandcastle's API with Containers underneath, so Core, too,
could place computers on Cloudflare or on sandcastle nodes behind one API.

### On Cloudflare, against the trust floor

- **Secrets never enter the guest.** The computer's object sets an
  outbound handler (`outboundByHost` for the model's host) with
  `interceptHttps`: Cloudflare gives the container an ephemeral CA,
  which the image's entrypoint adds to its trust store, and the guest
  calls the model with a placeholder. The handler, platform code
  outside the container, puts the owner's key in and meters the call, in
  the same request. There is no credential fetch, and nothing in the guest
  is worth stealing.
- **Snapshots are out of the guest's reach.** They are the object's API.
- **An image and a durable `/data`, updated by a rebase.** Not given:
  a container's disk is fresh from its image each time it starts, and a
  container snapshot is the whole root filesystem, tied to its image's
  version, and kept 30 days from its making or last restore. So `/data`
  is the object's to keep:
  1. Hermes' state lives under `/data` (its home), as on sandcastle.
  2. The object saves `/data` to R2 as an archive: at every sleep it
     asks for, and every few minutes while the guest wrote since the last
     save. SQLite is copied with its own backup (`hermes backup`, safe
     while running), not as raw files.
  3. A container starting from any image restores `/data` from the newest
     archive before its service starts. That is the rebase: a new image
     with the same data.
  4. A snapshot of the same image's container is the fast path for its
     next start; the archive is what lasts past 30 days and past an
     image.
  5. A rollout or eviction sends SIGTERM and waits up to 15 minutes; a
     hook in the guest then saves `/data` by posting it to an internal
     host, which the outbound handler writes to R2 (the guest holds no R2
     credential). A hard crash loses what was written since the last
     save, where sandcastle's local disk loses nothing: the recovery point
     is the save interval.

### Wake and cost

| | Cloudflare Containers | sandcastle (lat-6) |
|---|---|---|
| A warm wake | none: memory is not kept | 236 ms (Hermes, paused) |
| A cold wake | container start (648 ms median, 910 ms p95) + restoring `/data` + Hermes' own start (about 5 s) | 6.1 s |
| Awake, per hour | memory and disk as provisioned, CPU only as used: about $0.058 for a Hermes of 2 vCPU, 6 GiB, and 16 GB (below), plus its CPU | the box's rent, shared |
| Asleep | nothing | the box's rent |

**A Hermes' size on Cloudflare.** Paul wants 2 vCPU and 4 GiB. A
container's custom size (`ctx.container.start({vcpu, memoryMib,
diskMb})`) takes 1 to 4 vCPU, up to 12 GiB and 20 GB, but **at least
3 GiB of memory per vCPU**, so 2 vCPU needs 6 GiB. CPU is billed only
as it is used, so the second vCPU costs nothing idle; what it costs is
the 2 GiB more memory the ratio asks for, about $0.018 an hour awake.
The default is therefore 2 vCPU, 6 GiB, and 16 GB:

| | vCPU | Memory | Awake and idle, per hour | Per minute | Awake all month |
|---|---|---|---|---|---|
| Custom (the default) | 2 | 6 GiB | $0.058 | 0.1¢ | about $42 |
| `standard-1` | ½ | 4 GiB | $0.038 | 0.06¢ | about $27 |
| `standard-3` | 2 | 8 GiB | $0.076 | 0.13¢ | about $55 |

At list prices: memory $0.009 per GiB-hour, disk $0.00025 per GB-hour,
CPU $0.072 per vCPU-hour of use (an idle Hermes uses about 6
thousandths of a vCPU: about a cent a day, even awake all day). A person whose
Hermes is awake two hours a day costs about 12¢ a day.

With no warm tier, "warm" on Cloudflare means staying awake: at about
0.1¢ a minute, the computer's object can keep Hermes up 20 to 30 minutes
after the last message and still cost cents a day.
The object decides idle itself (the raw `ctx.container` API, not the
`Container` class, which renews its timeout on every WebSocket message,
pings included: FIN-66's lesson), counting data frames as sandcastle's
router does.

## Reaching a computer: through the platform

Every computer is reached the same way, on the fragment's own origin:
`__computer/*` on the fragment's host, routed by the fragment's cell to
its computer's cell, which the fragment's roles gate. The page uses plain
`fetch` and `WebSocket`, with no CORS, no per-computer URL or certificate,
and no WASM client.

- **On Cloudflare:** the computer's object proxies to its container
  (`ctx.container.getTcpPort(port)`, WebSockets included).
- **Self-hosted:** the computer's cell proxies to its sandcastle node.
- **A person's own computer:** it runs sandcastle and dials **out**: one
  WebSocket to its node's cell on the platform, carrying many streams
  (yamux-shaped), over which the platform speaks sandcastle's API and
  reaches its computers' services. No inbound port, no DNS, no
  certificate, and no relay to run; the platform is the relay. The same
  cell code serves it on Cloudflare (a hibernatable WebSocket in a Durable
  Object) and on celld.

The platform holds each computer's service login (Hermes' password) and
adds it on the way through, so a page never holds it.

## What fragment must change

An inventory of what the cell and the agents take from their runtime
(2026-09-30, at celld fork `4f50c81`; the fork's seven commits over
upstream v0.6.0 named) finds almost all of it standard Workers API:

- Durable Objects with SQLite (six classes), alarms, hibernatable
  WebSockets, `getByName`, `wait_until`;
- the Worker Loader (`env.LOADER`) and Durable Object Facets
  (`ctx.facets.get`, `abort`, `delete`) for each fragment's app, which
  gets only `FILES` (a `WorkerEntrypoint` bound by `ctx.props`) and no
  network (`globalOutbound: null`);
- Workflows (`JOBS`: one instance per job run), Queues (`DELIVERIES`,
  with a dead-letter queue), R2 (`BLOBS`), and service bindings
  (`AGENTS`);
- no KV, D1, cron triggers, or secrets bindings.

What differs, and what each becomes on Cloudflare:

| On celld | On Cloudflare |
|---|---|
| **`KEYS`**, a native binding (`native:keys`, the fork's seam): fleet secrets in the node's environment, and each call's scope (`<Class>:<id>`) derived by the host from the active event, so a cell cannot act as another | a `keys` Worker (the same `crates/native`, compiled to wasm) behind a service binding, its secrets in its own Worker secrets, none in the cell's. Cloudflare passes no caller identity on a service binding, so a cell's scope is the caller's to state: safe inside the platform's one trusted script (a fragment's app cannot reach `KEYS`), but weaker than celld's host-derived scope (an open question) |
| `CELLD_EGRESS_PUBLIC_ONLY` (a name resolving to a private address), `CELLD_INTERNAL_PEER_ONLY` | nothing to do: a Worker reaches no private network or internal listener |
| `CELLD_DYNAMIC_LOCKDOWN` (no `eval`, `new Function`, `Atomics.wait`; a heap stop) | Workers' own defaults (no `eval`; 128 MiB isolates) |
| `CELLD_FACET_MAX_BYTES` (the node's stop at 20 MiB) | the platform's own 16 MiB cap stays; Durable Objects' limits beyond it |
| celld's 255 loaded workers per script, answered as 503 `node_full` | Dynamic Workers' own limits and price ($0.002 per unique worker per day after the beta; each fragment is its own) |
| `celld deploy --named` for the agents' script, `celld dev --with` | `wrangler deploy` of two scripts; development stays on celld |
| the fleet's bucket: deployments, replicas, leases, fencing; `cargo xtask fleet … diagnose` | Cloudflare's own durability; no fleet operations |
| code that reads celld's behaviour: the router's retry on `CapacityExhausted`, the `egress refused:` error prefix, alarms fired again past 15 s | classified per substrate: each is a small, named change |

Two things to verify on Cloudflare before relying on them: that the
Worker Loader honours the `limits` fragment sets (`cpuMs` 30 000,
`subRequests` 50), and that `ctx.exports` (how the cell binds `FILES`)
needs no compatibility flag.

## Self-hosted, entirely

The services fragment calls that it does not run, and what each becomes
(Paul, 2026-09-30):

| Service | What for | On Cloudflare | Self-hosted |
|---|---|---|---|
| code.storage | every fragment's files, history, and `live` pointer (git) | code.storage, until Cloudflare Artifacts (Git-compatible repos for agents, reached from Workers, a REST API, and git clients) leaves its private beta; Paul is on the waitlist | a replacement service the operator runs: the contract `crates/fakes/src/codestorage.rs` already implements (the subset the cell and CLI call), made real on the bucket |
| WorkOS | sign-in (`KEYS` `workos/authenticate`) | fragment's WorkOS | the operator's own WorkOS account (only configuration), or, for an internal company network, its own identity provider over OpenID Connect (the Registry's sign-in made one seam with two implementations) |
| OpenRouter | model calls, and per-person keys that carry each budget | OpenRouter | OpenRouter |
| Sprites | fragments' computers today | Containers | sandcastle |

So a code store and a sign-in seam are what self-hosting has to build;
the models stay OpenRouter's everywhere. Web push goes through the
browsers' push services, which no operator runs; it stays optional. The
hosting is any machine that runs celld and any S3-compatible bucket,
with sandcastle nodes on Linux boxes with KVM and ZFS.

## Phases

1. **Seams in the cell, no behaviour change.**
   - `KEYS` behind one client with two transports: the native binding,
     or a Worker with the scope stated.
   - Egress refusals and capacity retries classified per substrate.
   - The substrate named in the configuration.
   - *Evaluation:* every e2e section passes on celld, unchanged.
2. **fragment on Cloudflare, without computers.**
   - A `cloudflare` fleet: wrangler configuration for the cell, the
     agents, and the `keys` Worker, deployed to a Cloudflare account
     (Paul approves).
   - *Evaluation:* the hosted e2e against it (`cargo xtask e2e --fleet
     cloudflare`: todo, inbox, blobs, egress, ai); a fragment's app
     cannot reach another's facet or `KEYS`; the two checks above
     answered.
3. **Computers behind the seam** (now over `ctx.container`: the order in
   docs/containers-on-celld.md replaces this phase's `SandcastleHost`).
   - The computer host trait. The `Hermes` cell's sandcastle code
     becomes `SandcastleHost`. Presets `hermes` and `goose`.
   - `__computer/*` on the fragment's origin replaces `__hermes/access`,
     `cors_origins`, and public computer URLs.
   - *Evaluation:* the `hermes` lane against the sandcastle fake.
4. **`ContainersHost` on Cloudflare.**
   - The computer's object and its container.
   - The outbound handler for credentials and metering.
   - `/data` archived to R2, with the SIGTERM save.
   - The idle rule on data frames.
   - *Evaluation:* the `hermes` lane against a Containers fake in
     `crates/fakes`, then a Hermes chat on the Cloudflare fleet, with
     cold wake, save, and restore timed, and its cost per awake hour.
5. **A person's own computer.**
   - sandcastle dials out: an uplink to its node's cell on the platform,
     carrying sandcastle's API and its computers' streams.
   - *Evaluation:* the same on both substrates; a node behind NAT with
     no inbound port.
6. **Entirely self-hosted.**
   - The code store as a service of its own, and sign-in through the
     operator's WorkOS or their own OpenID Connect provider.
   - One box's install (celld, sandcastle, a bucket) and its runbook.
   - *Evaluation:* the hosted e2e against a self-hosted fleet that calls
     neither code.storage nor fragment's WorkOS, and one signed in
     through an OpenID Connect provider on a private network.

Every phase keeps both substrates green: CI runs the e2e on celld, and a
Cloudflare fleet run joins it from phase 2.

## Open questions

- **`KEYS`' scope on Cloudflare.** A cell's scope is stated by the
  caller, inside one trusted script. Is that enough, or does each class
  get a `keys` binding of its own, fixed by `props`?
- **Dynamic Workers leave beta** at what price and limits? At $0.002
  per unique worker per day, 10 000 fragments active in a day cost $20.
- **Containers:**
  - at least 3 GiB per vCPU (so 2 vCPU is 6 GiB), at most 4 vCPU,
    12 GiB, and 20 GB;
  - snapshots are in beta, image-tied, and kept 30 days;
  - there is no warm tier;
  - a container may start somewhere other than its object, or move on a
    restart;
- **Parity.** celld's fork must keep matching Cloudflare's semantics
  where fragment leans on them (facets, the loader, alarms): the e2e on
  both substrates is the guard.
- **Cost** per active person on each substrate, measured in phases 2
  and 4, beside Sprites' and sandcastle's.
- **The uplink's shape** (phase 5): one WebSocket and a stream
  multiplexer, served alike by a Durable Object and a celld cell.
