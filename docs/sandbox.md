# Self-hosted computers: research and a proposal

Status: **research done 2026-09-29; Paul's calls the same day are under
Decisions; the rest of the proposal is not agreed.**
Paul's ask: prove out our own sandbox service, a self-hostable stand-in
for Fly Sprites that runs Hermes, on a VPS and on people's own machines,
authorized the way Sites and Brain are (nostr keys, "the BANKS way"),
with a very thin contract to the platform that drives it. fragment plays
finite.computer's Core in the experiment; the service itself knows
nothing about fragment. Its name is **sandcastle** (decision 4).

## Decisions (Paul, 2026-09-29)

1. **Test host: `finite-lat-6`** (`ubuntu@206.223.228.129`), a Latitude
   bare-metal box rented for this: EPYC 4564P (16 cores), 124 GiB, KVM,
   two empty 1.7 TB NVMe drives, Ubuntu 24.04.
2. **Engine: microsandbox.** smolvm stays the challenger behind the same
   engine trait.
3. **Storage is chosen per computer:**
   - `data` (the default, and the one that matters first): the image is
     the system, `/data` is durable, and an update is a rebase onto a
     new image. This is the Kata tradeoff, and it is what keeps
     updates possible when we manage the whole agent for people.
   - `pet`: the whole machine is durable and snapshotted (Sprites'
     model), with no image updates.
   - `ephemeral`: nothing durable.
4. **Home and name:** a separate Cargo workspace inside fragment-next,
   `sandcastle/`, which must not depend on fragment's crates, so it can
   move to Finite later.
5. **Backups go to fragment's Tigris bucket** for now.
6. **Credentials:**
   - Telegram is being deprecated, so it is out of scope.
   - The agent's nostr key lives in `/data` for now.
   - OAuth refresh done in the guest is fine for now.
   - What matters is heading the right way: later, a fragment
     "connections" feature (authorize GitHub or Google once, in the
     dashboard; any computer the person blesses uses that connector)
     would sit on the same credential source.

## What it must do, in Paul's order

| # | Need |
|---|---|
| R1 | A computer that runs Hermes (`hermes serve`, v0.21.5: image `nousresearch/hermes-agent:v2026.9.24`, amd64 and arm64, about 1 GB) |
| R2 | A durable per-computer `data` disk that outlives the machine: restore it onto a new machine, and move a machine to a new image without losing it |
| R3 | Continual backups, or at least backups at a quiesced moment (before updates) |
| R4 | One port reachable at a URL; nothing else reachable from outside; defense in depth on that port |
| R5 | Credentials loaded from a URL (one source of truth) that the agent cannot read |
| R6 | SQLite state backed up continually, or `hermes backup` on a cadence |
| R7 | Sleep when idle, wake fast on contact, crons that fire while asleep |

Hosts: x86_64 Linux servers we run, and people's own machines (bring
your own compute), Apple Silicon Macs included.

## What we found

### The old VPS cannot run a microVM

fragment.club 1.0's VPS (`ubuntu@152.236.5.66`, "fragment-club-1") is a
KubeVirt guest: 4 vCPUs (EPYC Genoa), 15 GiB, 154 GB disk, Ubuntu 24.04,
**no `/dev/kvm` and no `svm` flag**. Every microVM engine below needs
KVM. Its 1.0 stack (celld, Caddy, MinIO, blobsd) still runs; nothing was
changed. Its neighbour `152.236.34.15` is Finite's `lat4`, so it is
probably a Latitude.sh VM (unconfirmed).

Providers that give a guest KVM (the survey's sources): AWS C8i/M8i/R8i
(since 2026-02), GCP Intel series, Azure Dv5/Ev5, DigitalOcean
(unofficially); not Hetzner Cloud, Vultr or Linode cloud. Bare metal is
cheap: Hetzner dedicated/auction from about €35/month, Latitude
`m4.metal.small` $0.41/hour, Scaleway Elastic Metal from €0.077/hour.

### The field has converged on one shape

Every serious agent sandbox is a microVM (or gVisor) plus a separate
data disk, a userspace or nftables network with an egress policy, and a
host-side TLS-intercepting proxy that swaps placeholder credentials for
real ones (Fly's own `superfly/tokenizer`, smolvm, microsandbox,
CubeSandbox, Matchlock, Gondolin). What differs is how much of the
control plane comes with it.

| Engine | Needs KVM | Mac | Control plane included | Credential swap | Memory snapshots | Verdict |
|---|---|---|---|---|---|---|
| **smolvm** 1.20.2 (libkrun fork) | yes | yes (unsigned builds) | `smolvm serve`: HTTP + OpenAPI, no auth; survives its own restart under systemd | headers only, name-constrained CA, pluggable resolver | pause/resume in place, branch, portable checkpoints | spike |
| **microsandbox** 0.7.4 (libkrun fork) | yes | yes | none: a Rust library and CLI, one process per VM | headers (query, body opt-in), live rotation | 0.7.0 (two weeks old): restore to a new name, fork | spike |
| gVisor (`runsc`) | **no** | in a Linux VM | none | build it | checkpoint/restore without KVM | the no-KVM tier; not now |
| E2B runtime | yes | no | yes: URLs, auto-pause, wake on traffic | undocumented | yes | a platform to adopt, in Go, heavy; not ours |
| CubeSandbox | yes (or PVM) | no | yes | yes (L7 proxy) | yes | v0.7, needs MySQL, Redis, XFS |
| Cloud Hypervisor / Firecracker | yes | no | none | build it | yes, mature | the fallback if libkrun disappoints on Linux |
| BoxLite | yes | yes | a crate + REST | yes | not yet | a close third in the libkrun lane |

Out: Kata (no checkpoints, Kubernetes-shaped), Daytona (went private in
June 2026), Unikraft (not a full Linux), Drafter and ignite (archived),
the rest too small or SaaS-only.

**No engine is a Sprites replacement by itself.** Per-computer URLs, auth
on the API, idle detection and wake on request, timers, backups and
placement are ours to build whichever we choose (E2B and Cube include
some, in their own API shape and stack). So the engine sits behind our
own daemon, and the choice between smolvm and microsandbox is
reversible.

### smolvm (read at `209e0b4`, 2026-09-29)

- The VM boots an Alpine agent; the OCI image runs inside it as a
  crun container with every capability, so systemd and s6 images boot.
- Durable data: `--disk PATH` (a raw image or block device), virtiofs
  directories, S3 volumes. **No in-place image update**: a new image
  means a new machine with the disk re-attached.
- **Pause refuses a machine that has host mounts or `--credential`
  secrets.** `pause` → `capture_and_stop_to_path` →
  `validate_capture_profile` (`src/portable_checkpoint.rs:2518`), which
  rejects host mounts, published sockets, remote volumes and secret
  refs. Attached `--disk`s are not rejected (whether they are copied
  into every checkpoint is unclear). Credentials supplied through the
  serve API are held in memory, not as secret refs, so they may pass.
  This is the first thing to settle.
- `--expose-socket` relays a guest Unix socket to a host socket: a way
  in with no TCP port at all.
- No idle stop, TTL or wake in the open-source code (smol cloud only).
- One maintainer wrote about 90% of it; 20 releases in three weeks.

### microsandbox (read at `2110af1`, 0.7.4, 2026-09-29)

- The OCI image is the root filesystem; `--init auto` makes the image's
  own init PID 1. **Its docs carry an official Hermes recipe**: the
  official image, s6 as PID 1, a named volume at `/opt/data`, and
  `--replace` to move to a new image with the volume kept
  (`docs/examples/agents/hermes-agent.mdx`).
- Named volumes: directory (virtiofs, quota enforced) or disk (ext4 on
  virtio-blk). The root disk now survives stop and start and is dropped
  on replace. Volume snapshots export as `.msb` archives, incrementally
  (`--since`).
- `idle_timeout` stops a VM; nothing wakes it. Restoring a memory
  snapshot always makes a new sandbox, so sleep is snapshot, remove,
  restore under the old name.
- No daemon ("we are no longer going the server route", #611). Secret
  values given through the SDK are **persisted in the host's SQLite**;
  values given as `ENV@host` are resolved at spawn.
- Published ports reach only services bound to the guest's interface,
  not its loopback. Open bugs to watch: #1558 (volume locks leak),
  #1684 (creates fail after 64 in one process), #1705 (published-port
  connections never see EOF), #1676 (snapshot restore drops `--init`).
- It failed one step of the ComputeSDK DAX benchmark (6 of 7).

### What Finite has already learned (V3, Linear, finite-mono)

- **FIN-21 chose Sprites (2026-09-14), with Machines as the fallback,**
  and lists "operating a new bare-metal Firecracker fleet" and
  "multiple provider adapters" as out of scope for the first
  implementation. Self-hosting is "preserved as an option rather than
  implemented", and compute lifecycle is Core-only (owner-authenticated
  Core APIs), not BANKS-authorized.
  - *Paul, 2026-09-29:* things have changed. Hosted Finite should still
    use Sprites, but Finite must now support self-hosting as a
    first-class path. This service is that path. If it goes well, it
    also becomes the better, non-Kata bare-metal fleet.
  - The win depends on the fleet not being tangled up with Core, the
    Runner and agentd. finite-mono's Runner polls Core for leases and
    carries its own state, agentd sits inside the guest and handles
    credentials, and the launch env is copied forward across upgrades.
    That is why the contract is BANKS-shaped.
  - fragment is the greenfield where the answer is found before it
    moves to Finite.
- **FIN-66's Sprite lessons**, each a requirement here:
  - A checkpoint restored from inside the guest rewound newer files,
    brought back an old delivered secret, and revived a stopped service
    (a second writer). Snapshots and restores must be unreachable from
    the guest, and service definitions and secrets must live outside
    what a restore rewinds.
  - An idle authenticated WebSocket kept the Sprite awake. Idle must be
    measured in traffic, not open connections.
  - Provider metadata said `cold` while the process was alive (the
    24-hour canary failed at 12m49s). State must come from the guest's
    own evidence.
  - Long tools need a hold (Sprites' task leases) or they are
    suspended mid-work.
- **V3's secrets reach Hermes as environment variables under the same
  UID as its tools** (FIN-60, FIN-63), so a tool can read them.
  finite-mono today is worse: one host-wide file of shared provider
  keys, copied into every agent's environment, carried forward from
  `nerdctl inspect` on upgrade. A host-side credential swap fixes both.
- **The primitives V3's Sprite adapter actually uses**: idempotent
  create by a derived name, root exec, file read and push, a service
  with `http_port` restarted on crash, URL auth `public` or `sprite`
  (owner), in-guest task holds with expiry, stop that stays stopped,
  warm/cold status that never wakes the guest, `/data` owned by a
  non-root UID. A self-hosted service that offers these can sit under
  the same adapter.
- **Hermes v0.21.5 already has the hooks** (checked at `v2026.9.24`):
  `hermes serve` (FastAPI on 9119, its own web chat, a required auth
  gate on non-loopback binds); `hermes backup` copies each database with
  `sqlite3.backup()`, so it is safe while running; a gateway drain; and
  a pluggable cron provider whose external trigger is `POST
  /api/cron/fire`. That interface says it is "EXPERIMENTAL … until a
  second provider validates it": ours would be the second. Hermes
  honours `HTTPS_PROXY` and `SSL_CERT_FILE`/`REQUESTS_CA_BUNDLE`, which
  the credential swap needs. Most messaging connectors (Signal, Slack
  socket mode, Telegram long-poll) hold a connection open, so a
  computer using them stays awake.
- **How Sites and Brain authorize**: NIP-98 (`Authorization: Nostr
  <event>`, kind 27235, `u`, `method`, `payload` tags, ±60 s). Brain
  adds a replay cache on the event id and per-signer rate limits, and
  since its "auth kernel cut" calls neither Core nor Identity: grants
  name npubs. Sites keeps `publish_grants` rows whose source is
  `operator`, `core` or `self`. The service owns its grant rows; that
  is the pattern to copy.

## The proposal

### The line

The sandbox service knows **public keys, grants, computers, and three
URLs** (a grantor's, a credential source's, a usage sink's). It does not
know people, organizations, agents, billing, fragments or Hermes.
Everything that names those lives in the platform (fragment now, Core
later).

| The sandbox service owns | The platform owns |
|---|---|
| Computers: image, size, the data disk, the service, the URL, the egress policy | Who a key belongs to (a person, an agent, a fragment) |
| Admission: grant rows per public key (how many computers, how big) | Deciding grants (billing, plans) and writing them |
| Snapshots, backups, restores, and their storage | What to back up when, beyond the service's own schedule |
| Idle, sleep, wake, timers | Which jobs exist (Hermes keeps them; a plugin mirrors their next fire time) |
| Swapping credentials into outbound requests | The credentials themselves, served from its credential URL |
| Usage per computer per owner key | Turning usage into charges |

### Auth, the BANKS way

- Every API call is NIP-98, with a replay cache and a skew limit (as
  Brain does).
- **Grants** are rows the service owns, written only by keys the
  operator configured as grantors: `PUT /v1/grants/{pubkey}` with
  limits. In V3 the grantor is Core's key; in fragment, the platform's;
  on a person's own machine, their own key. Revocation takes effect at
  once, because nothing else holds the answer.
- **A computer belongs to keys**: the one that created it, plus keys an
  owner adds or removes. Key rotation is adding the new key and removing
  the old, done by the owner.
- This fits FIN-21's Core-only rule as a matter of policy: when Core
  holds every grant and owns every computer, only Core can act. It also
  lets a self-hoster's own key hold computers with no Core at all. The
  contract is the same.

### API sketch (names are placeholders)

```
PUT    /v1/grants/{pubkey}                     grantor only
PUT    /v1/computers/{name}                    create or converge (idempotent): image, size, data_gib,
                                               service {cmd, http_port}, url_auth owner|public,
                                               egress, credentials_url, idle_after_s
GET    /v1/computers/{name}                    desired and observed state; never wakes it
POST   /v1/computers/{name}/start|stop|restart stop stays stopped: no request or timer overrides it
PUT    /v1/computers/{name}/image              rebase onto a new image, keeping data (below)
POST   /v1/computers/{name}/exec               streamed; root or the service user
GET|PUT /v1/computers/{name}/fs/{path}
POST   /v1/computers/{name}/snapshots          and list, and restore into a new or empty computer
POST   /v1/computers/{name}/tickets            a short-lived, single-use ticket for its URL
PUT    /v1/computers/{name}/timers/{id}        at a time: wake and deliver {method, path, body}
PUT    /v1/computers/{name}/keys/{pubkey}      add or remove an owner key
```

Inside the guest, a small local endpoint (vsock) answers for that
computer only: hold (keep awake until a time), timers, and who am I. It
offers nothing destructive: no snapshots, restores, stops or exec.

### Storage, updates, backups (R2, R3, R6)

- A computer is an image, a data disk, and its settings. The image is
  replaceable; `/data` is not. Hermes' home is `/data/hermes`.
- **An update is a rebase**:
  1. Drain Hermes through its API.
  2. Take a final snapshot.
  3. Stop the computer and recreate it on the new image with the same disk.
  4. Gate on health.
  5. If health fails, go back to the old image (never rewind the data).
  
  This is microsandbox's `--replace` with a named volume, finite-mono's
  candidate-and-rollback, and V3's upgrade rule, in one.
- Anything installed outside `/data` is lost at the next update. Tools a
  person or agent installs go under `/data` (uv, npm prefixes, a Nix
  profile). Open question 4 asks whether that is acceptable.
- **Backups are taken by the service, from the host, and the guest cannot
  reach them.** The spike tries two ways to take them:
  - (a) a ZFS (or btrfs) snapshot of the data disk every five minutes
    while awake and at every sleep and update boundary, sent
    incrementally to object storage;
  - (b) the engine's own incremental export (microsandbox `--since`).

  A crash-consistent snapshot is enough for SQLite in WAL mode if the
  engine honours flushes. The spike checks that. If it doesn't hold,
  freeze `/data` (`fsfreeze`) around the snapshot.
- `hermes backup` on a cadence stays the portable, app-level export (a
  platform job through `exec`), so a home can move between our service
  and Sprites. Litestream is not needed for a five-minute RPO.

### Ingress (R4)

- The host opens 443 (and 22 for operators) and nothing else.
- The guest has no address anyone outside can reach. Its one service
  port reaches the host as a Unix socket, not a TCP port, where the
  engine allows it (smolvm `--expose-socket`, microsandbox `--vsock`).
- The router serves `https://<computer>.<domain>/`.
  - With `url_auth=owner`, it admits only a request carrying a
    host-only `__Host-` cookie from a redeemed ticket. This is the same
    per-origin exchange fragment's sessions and Sites v2 use.
  - Hermes' own auth gate sits behind that.
  - The router also caps request sizes and rates and times out idle
    WebSockets.
- The API and the URL are separate hostnames. Exec and files never go
  through the URL.
- The default egress policy is the public internet only (no host, LAN,
  metadata or other computers). Both engines have this floor.

### Credentials (R5)

- A computer names a `credentials_url`. The host-side daemon fetches it
  with a NIP-98 request signed by **the node's key**, naming the
  computer. The response is a list of `{name, value, hosts}`. The daemon
  keeps it in memory and gives it to the engine's swap. The guest sees
  `SANDBOX_PLACEHOLDER_…` (plain ASCII, as Hermes' env loader requires)
  and a CA it trusts only for those hosts.
- Refetch happens on a TTL, on the platform's signal, and at every
  start. A restore cannot bring back an old value, because none is on
  the disk.
- **Limits:**
  - It works only for credentials that travel in HTTP headers to known
    hosts.
  - Not covered: Telegram's token (it is in the URL path), clients that
    pin certificates, WebSockets on credential hosts, and signing keys
    such as the agent's nostr key. Those stay in the guest, or later
    move to a remote signer.
  - A response can echo a secret back.
- FIN-60 rules out a guest boot-token fetch. This design is not one: the
  trusted host fetches, which is FIN-60's "trusted delivery" without the
  copy in the guest's environment.

### Sleep, wake, timers (R7)

- **Idle** means no traffic through the router or the egress proxy, no
  exec session, and no hold, for `idle_after_s`. An open but quiet
  WebSocket does not count as activity.
- **Sleep** starts as stop: a cold boot on wake. FIN-21 requires
  starting from disk anyway. The spike then measures a memory snapshot
  as the fast path: smolvm pause/resume; microsandbox snapshot, remove,
  restore.
- **Wake**: the router holds the request, starts or restores the
  computer, waits for the service port, and forwards. Targets: under
  5 s warm and under 30 s cold (V3's).
- **Timers**:
  - The daemon wakes a computer at a time and POSTs to its service.
  - A small Hermes cron-provider plugin mirrors each job's next fire time
    into the guest endpoint.
  - It receives `/api/cron/fire`, and Hermes claims the job with its own
    compare-and-set.
  - The plugin is Hermes-specific, so it lives with the platform, not
    in the service.

### Engines

Spike **microsandbox first** and **smolvm second**, behind one engine
trait in our daemon:

- microsandbox, as a library, is the natural thing to embed. Its image
  replace plus named volume is R2 as documented, and it runs Hermes by
  an official recipe.
- smolvm is the challenger. Its pause is in place, its socket exposure
  is cleaner, and its serve daemon survives its own restarts.

Both are young libkrun forks, which is the risk. If either fails on
Linux, Cloud Hypervisor is the mature engine to put behind the same
trait for our servers, while a libkrun engine stays the Mac path. gVisor
is the no-KVM tier (it would run on the old VPS), noted, not spiked.

## Where to test

`finite-lat-6` (decision 1). The old VPS cannot run a microVM. It would
suit gVisor, the no-KVM tier, if that is ever wanted. Early work can run on
a Mac against microsandbox's HVF engine with arm64 images; anything that
must hold on x86 Linux is proven on `finite-lat-6`.

## Spike 1: Hermes on microsandbox, by hand (2026-09-29)

A lower-rung diagnostic, not product proof: a hand-driven script on
`finite-lat-6` (msb 0.7.4, KVM, guest kernel 6.12.109), with Hermes'
official images and a 10 GiB disk volume (ext4 on virtio-blk) at
`/opt/data`. The script and its log are on the host (`~/spike1.sh`,
`~/spike1.log`).

| Step | Result |
|---|---|
| Idle VM from the Hermes image (`msb create`) | up in 0.27 s |
| `hermes serve` launched by the host, answering | 3.3 s after create |
| Hermes' own gate | anonymous ws-ticket 401, wrong password 401, right password 200, ticket 200; anonymous `/api/status` answers 200 (a public status route: the router's gate is not optional) |
| Graceful stop (SIGTERM, then the VM) | 0.23 s for Hermes, 0.1 s for the VM |
| Start, relaunch | ready in 3.3 s; marker file and both databases intact |
| Rebase v0.21.4 (`v2026.9.21`) → v0.21.5 (`v2026.9.24`) on the same disk | ready in 3.3 s; marker, `state.db`, and the auth config intact |
| Host listeners | only `127.0.0.1:19119`, owned by the VM process |

What it changed in the design:

- **s6 cannot be PID 1 in a detached sandbox.** From v0.21 the image's
  ENTRYPOINT is a dispatcher, not `/init`. `--init auto` finds nothing,
  and pointing the entrypoint at `/init` stopped the VM at once. So
  agentd stays PID 1, and Hermes runs its documented non-PID-1 path
  (the stage-2 hook, then `hermes serve`). Hermes' own supervised
  per-profile gateways are unavailable in that mode, which serving the
  dashboard does not need.
- **`msb start` does not rerun a startup command.** So the host launches
  the service on every boot, through exec and `setsid`, which reparents
  it to agentd so it outlives the exec session. Service definitions
  therefore live in the daemon, outside anything a guest restore can
  rewind: FIN-66's lesson, by construction.
- **`msb exec` waits on an open stdin.** Every exec gives it `/dev/null`.
- **The engine gate is the `msb` CLI, not the Rust SDK.** The SDK's
  `local` feature pulls several hundred crates (sea-orm, russh, reqwest,
  …) in lockstep with the runtime. The CLI already offers `--format
  json`, and `--secret ENV@HOST` reads the value from the spawning
  process's environment, never argv. The daemon checks the exact `msb`
  version at startup. The SDK stays a swap behind the engine trait.

## Phases 1 and 2 on the real engine (2026-09-29)

`sandcastle/` (a Cargo workspace of its own: `proto`, `nip98`,
`sandcastled`, the `sandcastle` CLI; `sandcastle/README.md`) was deployed to
`finite-lat-6` under systemd. It was driven from a Mac through the CLI and
curl, with a private CA, `--connect`/`--resolve` in place of DNS, the
official Hermes images, and no fakes. This is still a hand-run check, not a
Rust e2e lane (debt ledger).

| Check | Result |
|---|---|
| Unsigned, stale, wrong-URL, replayed, non-grantor, no-grant, over-grant calls | refused (also covered by the node's tests) |
| Signed create of a Hermes computer (v0.21.4) | serving 6 s after the call |
| From outside: ports 1–1100, 8642, 9119, 19119, 20000–20020 | only 22 and 443 open |
| From the guest: the node's IPv4 and IPv6 (sshd, router), metadata, private ranges, the host's loopback | refused; the public internet (OpenRouter) reached |
| The URL without the router's session; a ticket's second use; an unknown computer | 401, 401, 404 |
| Hermes' own gate behind it: no login, wrong password, right password | 401, 401, 200; a Hermes session without the router's session is 401 |
| A Hermes WebSocket (`/api/ws?ticket=…`) through the proxy | 101 Switching Protocols |
| A marker written 1 s before a rebase v0.21.4 → v0.21.5 | survives; `state.db` intact; serving 2–5 s after the call |
| Router and Hermes sessions across the rebase | both survive (Hermes needs `HERMES_DASHBOARD_BASIC_AUTH_SECRET`) |
| `systemctl restart sandcastled` with requests in flight | 40 of 40 answered; same VM and Hermes processes, no relaunch |
| Stop, then start, through the API | stopped in 1.7 s (URL 503); serving 5.3 s after start; marker and sessions intact |
| An idle Hermes computer (2 vCPU, 4 GiB) | about 455 MB of host RAM |

What the real engine taught (each now fixed and, where the fake can hold
it, tested):

- **A rebase must sync and stop cleanly before `--replace`.** The first
  cut SIGTERMed Hermes and replaced the running machine, and a file written
  a second earlier was lost. Now the stop script ends with `sync`, and the
  machine is stopped before it is replaced.
- **`msb rm -f` refuses a running machine, and `msb volume rm` refuses an
  attached volume.** Deletion is now: service, machine stop, remove, disk.
  The fake engine now refuses the same things.
- **No capabilities anywhere.** With `AmbientCapabilities=
  CAP_NET_BIND_SERVICE`, every VM process inherited the capability, and
  Linux denied an unprivileged `msb stop` its `/proc` access. The target's
  capabilities must be a subset of the caller's. systemd now binds 443
  (socket activation), and the daemon's capability set is empty.
- **msb's egress rules need IPv6 in brackets** (`deny@[2605:…::/64]`). The
  failure surfaced through the API as the engine's own message, with
  backoff.
- **A deletion answered 404 before it was done**, so a caller (and my own
  check) could act too early. A deleting computer now reads
  `desired: deleted` until its machine and disk are gone.
- **Updates are generations, not images.** Adding Hermes' session secret
  was a service change, and the API allowed only image changes. Now the
  image, the service, and `url_auth` can change (any change to the first
  two rebases), while storage and size are fixed.
- **Views redact service env values** (`(set)`): a GET never echoes the
  dashboard password.

## Phases, each with its check

1. **Hermes at a URL (R1, R4).** *Met on the real engine 2026-09-29, with
   a private CA and no DNS; a browser on a real domain is still to do.* The official v0.21.5 image in a
   microVM on x86 KVM with `/data` on its own disk. `hermes serve` is
   reached at `https://<computer>.<domain>/` behind an owner ticket.
   Checks:
   - A port scan from outside finds 443 and 22.
   - From inside the guest, the host, the metadata address, the LAN
     and another computer are refused.
   - No ticket, an expired ticket and a replayed ticket are refused at
     the router.
2. **Durable data and updates (R2).** *Met by hand 2026-09-29 (rebase
   and restart); the broken-image rollback is not built: a failed rebase
   backs off rather than returning to the old image.* Checks:
   - A conversation survives stop/start and a rebase from an older
     Hermes image to v0.21.5.
   - A rebase onto a broken image returns to the old image with the
     data intact.
   - Killing the daemon mid-rebase and restarting it converges to one
     running computer.
3. **Backups and restore (R3, R6).** Checks:
   - Five-minute snapshots while a turn is writing.
   - A restore onto an empty disk on another host passes SQLite
     `integrity_check` and reopens the same conversation, then runs a
     fresh model and tool turn.
   - Nothing in the guest can list or trigger a snapshot or restore.
4. **Credentials (R5).** A real OpenRouter call through the swap. Then
   `env`, `/proc/*/environ`, a grep of `/data` and a memory dump of the
   guest find no key. Rotating the key at the source takes effect
   without touching the guest. A restore brings back no old value.
5. **Sleep and wake (R7).** Checks:
   - Idle sleep happens with a quiet WebSocket open.
   - A browser request wakes the computer, measured as ten warm and ten
     cold wakes each with a spread.
   - A Hermes cron job fires on time while the computer is asleep.
   - A held long tool is never suspended.
6. **The contract.** NIP-98 on every call, grants, and owner keys:
   valid, invalid and replay tests for each mutation, and a restart test
   for each stored invariant. fragment's `Computer` cell drives it.
   A `hermes` template gives a fragment whose chat talks to `hermes
   serve` and survives an update. A lane goes in `crates/e2e`.
7. **Bring your own compute.** The same daemon on an Apple Silicon Mac,
   holding its owner's computers under their own key.

## Open questions

All six first ones were answered 2026-09-29 (see Decisions; question 2
under What we found). New ones:

1. **A real domain for the test node**, for a browser check with a real
   certificate: say `*.sandcastle.fragment.club` pointing at `finite-lat-6`
   (206.223.228.129), with a wildcard certificate by DNS-01. DNS changes are
   Paul's.
2. **Rollback on a failed rebase.** Should the node return to the previous
   image by itself when a new one never serves (Kata's candidate and
   rollback), or report the failure and wait for the platform's next PUT?
   Proposed: return by itself, keeping the data, and report it.
