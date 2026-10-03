# Computers: the platform's contract with an image

Status: **the contract phase 4 builds** (docs/cloudflare-v1.md, decisions
13, 18, 19, 22, 23, 39, 41–44). The platform side is the generic Computer
Durable Object in the cell. The image side is any image: ours runs
Hermes (`images/hermes/`), and a stub image (`images/stub/`) proves
the platform needs nothing Hermes-specific. Nothing here names an agent
runtime. A change that would have to is a design bug (the rule).

## What a computer is

- A computer is an entity owned by one person: `computer:<24 hex>`. For
  now each person has one (decision 13), made by the shell at first run
  through the public API (`POST /api/computers`, which answers the one
  the person has when called again: its id is derived from its owner).
  It runs one image, pinned per computer (decision 19), at the
  operator's default size (2 vCPU, 6 GiB).
- An agent fragment is assigned to its owner's computer (`PUT
  /api/computers/{id}/agents/{fragment}`). Its own key then becomes the
  agent's identity, registered to the fragment's owner, and it signs the
  guest's requests only while it is assigned there.
- Agents are fragments of kind `agent` that name the computer that runs
  them (decision 14). An agent's identity is its own (`id:` of the agent
  fragment's identity), and its owner is a person. The computer's guest
  acts *as* its agents, never as its owner.
- The Computer DO is the only thing that talks to its container. It
  starts and stops it, saves and restores `/data`, runs the egress
  intercepts below, proxies its ports, and wakes it. It never reads what
  the image runs.

## Lifecycle

States: `asleep` → `starting` → `awake` → `sleeping` → `asleep`, plus
`failed` (a wake that keeps failing, lesson 4: "won't wake", no more
container starts until someone asks again).

- **Wakes:** a record on a channel one of its agents subscribed to with
  `wake: true`; an open port tab; a pre-wake (a page opened a subscribed
  fragment, or someone started typing there: it starts at once and stops
  after 60 s if nothing arrives); one of its agents added as a member of
  any fragment (below: the platform posts `joined` and wakes it, held as
  a record holds it); the owner's `POST /api/computers/{id}/wake`.
- **Awake while:** a port tab is open, or the guest holds the keepalive
  socket (below). Traffic from the container does not count (spike S3).
  Twenty minutes after neither holds, it sleeps. A $200 seat's computer
  never sleeps (decision 25).
- **Sleep**, driven by the DO, in order: save `/data` (`DirectoryBackup`),
  take a container snapshot, send SIGTERM, wait up to 5 s for the guest
  to exit, destroy. An idle stop by the runtime is only the safety net.
- **Wake from a snapshot** when the snapshot's image is the pinned image
  (`start({containerSnapshot})`); otherwise start the image with
  `RESTORE_PENDING=1`, restore `/data`, then touch
  `/run/computer/restored`. A start answered "temporarily unavailable" is
  retried with backoff.
- **A new isolate** that finds the container running attaches its
  monitor, timeout and intercepts again and never destroys it (lesson 6).
  After any `destroy()` it waits for `running` to be false before
  `start()`.

## The guest's contract

What every image may rely on, and must do.

### Environment

| Variable | Value |
|---|---|
| `FRAGMENT_COMPUTER` | its id, `computer:<hex>` |
| `FRAGMENT_API` | `http://api.fragment.internal` |
| `FRAGMENT_MODEL` | `http://model.fragment.internal` |
| `FRAGMENT_STORAGE` | `http://storage.fragment.internal` (S3; any access key) |
| `FRAGMENT_IMAGE` | the pinned image's name and digest |
| `RESTORE_PENDING` | `1` when `/data` is being restored |

The guest holds no credential. Every outbound request to the hosts above
is caught by the Computer DO's intercepts, which add what the platform
holds. Other internet traffic goes out as it is (decision 43).

### Its process

The image's entrypoint is PID 1, in a PID namespace of its own (the
cell's `containers_pid_namespace` compatibility flag, so `wrangler dev`
matches Containers). PIDs repeat from one start to the next: a lock
that names a PID from before a sleep can name a live process after it.

### What every image carries

- `/usr/local/bin/sandbox-shim` from `cloudflare/sandbox:1.0.0`: the
  DO's `DirectoryBackup` saves and restores `/data` through it.
- `sh`, `true`, `mkdir` and `touch`: the DO polls with `true` and opens
  the restore gate with `touch`.

### Data and the restore gate

- `/data` is the only directory kept across sleeps. Everything else is
  the image's. The image does not ship `/data`: the restore makes it,
  swapping a restored directory into place, which overlayfs refuses for
  a directory from an image layer (EXDEV); on a first start the image
  makes it itself.
- With `RESTORE_PENDING=1`, the image waits for `/run/computer/restored`
  before it reads `/data`. Without it, `/data` is ready at start (a
  snapshot wake, or a first start with an empty `/data`).
- SIGTERM means stop now: flush and exit within 5 s. `/data` was saved
  before the signal; whatever is written after it may be lost.

### The fragment API

- `http://api.fragment.internal/<path>` is the platform's API
  (`docs/api.md`), and `http://api.fragment.internal/f/<fragment>/<path>`
  is a fragment's own routes (`__live` sockets included), as on its host.
- `x-fragment-agent: <agent fragment name>` says which of its agents the
  request acts as. The intercept refuses an agent not assigned to this
  computer, then signs the request (NIP-98) as that agent's identity.
  The agent then holds exactly its grants: a person's agents act for
  them, never above them (decision 36).
- Without the header only the computer's own routes answer:
  - `GET /api/computer` → `{computer, owner, image, agents: [{fragment,
    identity, name, owner}]}`: the agents to run.
  - `GET /api/computer/keepalive` (a WebSocket): while it is open the
    computer stays awake. Hold it while busy; drop it while waiting on a
    person (decision 42).
- A subscription with `{channel, wake: true}` (in place of `url`), sent
  as an agent to `POST /f/<fragment>/api/subscriptions`, wakes the
  computer on each new record; the agent must be a member who may read
  the channel. Only a computer's egress can ask for one. Nothing is
  pushed into the guest: on waking, the guest reads each channel after its last sequence number
  (`GET …/channels/{channel}?after=`) and follows it live on `__live`.
  A record's `(fragment, channel, seq)` is the id of whatever it starts,
  so a catch-up never starts something twice (lesson 2).
  - It is its principal's: each agent subscribes for itself, once per
    channel. `GET …/subscriptions` lists the agent's own with `wake: true`
    (`{id, principal, channel, wake}`), and the guest makes one only when
    that list has none, so a repeated `POST` need not be idempotent. A
    member's wake subscriptions go with its membership.
  - A record wakes the computer whoever posted it, except the agents of
    that computer (a poster holding a wake subscription to it): it was
    awake to post, and a late record of theirs must not wake it back as it
    goes to sleep.
- What a guest follows, by convention (docs/chat-records.md): for each
  agent, `chat` of every fragment it is a member of whose channels
  include a postable `chat` (a chat), and `tasks` of the agent's own
  fragment (its routines, and `joined` when it is added to a fragment).
  The list comes from `GET /api/fragments` and `GET /api/f/{name}/channels`
  as the agent, read again every 5 minutes and on `joined`; the agents
  themselves from `GET /api/computer`, read again every minute.
- **An agent added to a fragment wakes its computer** (Paul, 2026-10-03:
  agents are woken eagerly, to hide a wake's latency). Whatever adds an
  agent as a member (`PUT /api/f/{name}/members/{agent}`, an invite it
  accepts, a fragment an agent makes for its owner), the platform tells
  its computer: the agent fragment itself posts `{kind: "joined",
  fragment}` on its `tasks` (when it declares a postable `tasks`), once
  for that membership, and the computer wakes (`joined`, held as a
  record holds it). Awake, the guest lists its fragments again on
  `joined`; woken, it lists them as it starts; either way it follows the
  new chat before anyone speaks there. Nothing a template or the shell
  does is needed, and nothing of theirs posts `joined`. The fragment the
  agent joined keeps the notice in an outbox of its own until the
  computer has it (`agent.told` in its events); a role change, or the
  same member added again, is no new join. An agent newly assigned to a
  computer is followed within a minute while it is awake; to a sleeping
  one, wake it (`POST /api/computers/{id}/wake`).
- What was said before an agent joined a chat is not for it: the guest
  skips a record whose `at` is before the agent's membership's `addedAt`
  (`GET /api/f/{name}/members`).

### Models

- `POST http://model.fragment.internal/v1/chat/completions`, OpenAI's
  shape, with `model` a tier (`cheap`, `medium`, or `high` while it is
  on) and `x-fragment-agent`. The intercept makes it the platform's
  model route as that agent (`POST /api/models/v1/chat/completions`,
  signed as the agent: docs/api.md, docs/ledger.md), which allows only
  the tier's model and a capped `max_tokens`, drops the guest's auth
  headers, reserves the call's worst case on the agent's owner's ledger,
  calls the model through AI Gateway, streams its answer back, and
  settles the final usage (lesson 7).
- A call its owner's ledger refuses (zero credit or a canceled seat:
  402 `budget_used_up`; a guest: 403) gets the ledger's reason, and no
  call is made.
- Any other path is 404, unmetered. Hermes probes `GET /api/show`
  (Ollama's model metadata) with no `x-fragment-agent`; it falls back to
  its configured context length. Anthropic's shape
  (`/anthropic/v1/messages`) comes with the high tier, which is off
  (decision 23).
- A call without `x-fragment-agent` is refused (401: no one to bill).
  Our Hermes image sets it on every call of an agent's profile (its
  `model.default_headers`), the main model's and the auxiliary ones'
  (titles, the smart-approval guardian).
- The intercept names no fragment, so a call bills its agent's owner
  and no fragment's cap applies (decision 36: an agent's model calls are
  its owner's).

### Storage

`http://storage.fragment.internal` is an S3 endpoint over the
computer's own R2 prefix: any bucket name and key, scoped by the
intercept. It is for an image's own disaster recovery (Litestream).

### Connections and operator keys

- A connection (Google, Notion, GitHub, …) is used by sending its
  placeholder, `fragment-connection:<provider>`, in a header of a
  request to one of the provider's own hosts, with `x-fragment-agent`:
  `Authorization: Bearer fragment-connection:github` to
  `api.github.com`. The intercept swaps in a short-lived token from
  WorkOS Pipes for the agent's owner's account at that provider. An
  agent may use every connection its owner has (decision 44: a person's
  agents are not fenced from each other); its owner may narrow one agent
  to a list (`PUT /api/computers/{id}/agents/{fragment}/connections
  {connections: [provider]}`, and `{connections: null}` for every one
  again, the default). It refuses (decision 22) with 403 `forbidden`
  when the owner narrowed the agent to a list without that provider,
  and 403 `not_connected` when the owner has connected no account
  there, or must authorize it again. The token is held until a minute
  before it expires, at most ten minutes.
- An operator key (a paid API that needs only a key) is
  `fragment-key:<name>` in whichever header the provider takes it in
  (`x-api-key: fragment-key:search`). Any agent of the computer may use
  it; the swap meters the call to the agent's owner (decision 37).
- Which providers and keys a deployment offers, and each one's hosts,
  are its configuration (`FRAGMENT_CONNECTIONS`,
  `FRAGMENT_OPERATOR_KEYS`: `{"github": ["api.github.com"]}`; a key's
  value is the secret `FRAGMENT_KEY_<NAME>`). Only those hosts are
  intercepted; the rest of the internet is reached as it is (decision
  43).
- A placeholder sent to a host that is not its credential's is refused
  (403), so a token never reaches a host it was not made for. Only
  headers are swapped: a URL or a body is sent as it came, and at most
  4 placeholders a request.
- The intercept catches those hosts on HTTPS and on plain HTTP alike,
  and always sends on over HTTPS. It strips `x-fragment-agent`, never
  follows a redirect (the guest follows it, without the token), and
  reads a request's body whole (at most 32 MiB). A request to such a
  host with no placeholder goes on as it came.
- HTTPS interception needs Cloudflare's CA: the image waits for it at
  boot and appends it to its trust store.
- Agents share a computer, so one agent's guest could send another's
  `x-fragment-agent`, and that is by design: a person's agents are not
  fenced from each other (decision 44), so a narrowed list is an agent's
  specialization, never a wall. Walls stand between people (decision 36).

### Ports

The image may serve HTTP and WebSockets on any port. The platform
serves them on the computer's own origin, `<24 hex>--computer.<suffix>`
(cross-site from the platform, so a page the guest serves can act as no
one), at `/p/<port>/…`, for its owner only for now (delegates come with
decision 41's sharing). A browser gets there with a one-time ticket its
owner mints (`POST /api/computers/{id}/ports/{port}/ticket` → `{url}`),
which that origin redeems into a session cookie of its own; a signed
request (the CLI's) needs none. A WebSocket on a port is bridged through
the Computer DO and holds it awake while open. Nothing else reaches the
container from outside. By convention the screen is a page on port 6080
(decision 11). The page is served at the
port's root and reaches its sockets by relative URLs (our images':
`websockify?viewer=` and `control?viewer=`), so it works under the
port's prefix (`/p/6080/`).

## Our images

Each is under `images/` (docs/bridge.md has the bridge's routes,
settings and state):

- `images/bridge`: the process both images run between an agent runtime
  and the fragment API, with the chat records of docs/chat-records.md.
  Its core names no runtime; `relay` (Hermes' Relay) and `script` (a
  deterministic agent) are its two runtimes.
- `images/stub`: the bridge with `script`, and a screen page. 9.6 MB; a
  local start follows its chats in 0.3 s. The platform's own lanes run
  against it.
- `images/hermes`: Hermes v0.21.5's desktop image, `hermes-boot`, the
  bridge as Hermes' Relay connector, Litestream, the screen. One Hermes
  profile per agent (`juniper.paul` is `juniper-paul`), its agent
  fragment's `SOUL.md`, `memories/` and `skills/` checked out into it and
  committed back. Each start clears Hermes' cross-process leases (a
  session's turn, a compression) before the gateway takes a turn: a
  lease restored with `/data` names a PID that is live again in the new
  start, and Hermes would wait out its five-minute TTL. And it writes
  Hermes' clean-exit receipt, so Hermes discards the turns a restart cut
  short rather than resuming them: `/data` is saved before the guest is
  signalled, so a restored one always reads as an unclean exit, and a
  resumed turn would carry the old turn's message id and fold the
  person's next message into it, while the bridge has already ended it
  (docs/chat-records.md). An agent fragment's optional
  `agent.json`
  (`{"tier": "cheap"|"medium"|"high"}`) picks its model tier (medium by
  default; `high` only with `FRAGMENT_HIGH_TIER=on`, decision 23).

## Billing

- Awake time is metered at the instance's rate (`FRAGMENT_COMPUTER_INSTANCE`,
  the price book's name for it) to the computer's owner (decision 24):
  an interval every five minutes awake and one at each sleep, kept by the
  Computer DO until the owner's ledger has it (each once, by its
  reference `awake:<computer>:<from>`). A $200 seat's awake time is not
  charged.
- Model calls bill the agent's owner, through the platform's model
  route. Each operator key's call the provider answered is metered to
  the agent's owner at the key's price and the margin (`key:<computer>:…`;
  decision 37); a key with no price on the deployment is not lent.
  Connections cost nothing to swap.
- At zero credit, or with agents stopped, no wake starts (decision 27):
  the owner's wake is refused with the ledger's reason (402
  `budget_used_up` at zero credit or a canceled seat; 403 for a guest,
  who pays for nothing), which the view's `why` keeps; a record, a join or a page wakes nothing; no model call or
  key call is made. A computer already awake runs on until it sleeps.

## Tests

- `crates/core`: the lifecycle as a pure state machine (wake racing
  sleep, the newest push wins, a deadman alarm, a failing wake sealed).
- The e2e on workerd: the Computer DO's routes and its intercepts,
  against `images/stub/` under `wrangler dev` with Docker.
- The real-Hermes lane: `images/hermes/` with a scripted model (phase
  4's exit list).
- The images' own (`images/`, its own workspace: `cargo test` and
  `cargo clippy --all-targets -- -D warnings` there): the bridge's engine,
  pure; the bridge against an in-process fake fragment API, with the
  `script` runtime and with the `relay` runtime against a scripted
  Hermes gateway; and, with Docker, both images built and run against
  the fake API and a scripted model on the host
  (`cargo test -p fragment-bridge --test docker -- --ignored`), real
  Hermes included. These are lower rung: fakes at the platform's edge.
