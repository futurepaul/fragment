# Computers: the platform's contract with an image

Status: **the contract phase 4 builds** (docs/cloudflare-v1.md, decisions
13, 18, 19, 22, 23, 39, 41–43). The platform side is the generic Computer
Durable Object in the cell. The image side is any image: ours runs
Hermes (`images/hermes/`), and a stub image (`images/stub/`) proves
the platform needs nothing Hermes-specific. Nothing here names an agent
runtime. A change that would have to is a design bug (the rule).

## What a computer is

- A computer is an entity owned by one person: `computer:<24 hex>`. For
  now each person has one (decision 13), made by the shell at first run
  through the public API. It runs one image, pinned per computer
  (decision 19), at the operator's default size (2 vCPU, 6 GiB).
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
  after 60 s if nothing arrives); the owner's `POST /api/computers/{id}/wake`.
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

### Data and the restore gate

- `/data` is the only directory kept across sleeps. Everything else is
  the image's.
- With `RESTORE_PENDING=1`, the image waits for `/run/computer/restored`
  before it reads `/data`. Without it, `/data` is ready at start (a
  snapshot wake, or a first start with an empty `/data`).
- SIGTERM means stop now: flush and exit within 5 s. `/data` was saved
  before the signal; whatever is written after it may be lost.
- The Computer DO runs commands in the container: `true` while it polls
  for a started container, `touch /run/computer/restored`, and
  `sandbox-shim` for `DirectoryBackup`. Every image carries the three
  (the stub's are busybox's).

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
- A subscription with `{channel, wake: true}` (in place of `url`) wakes
  the computer on each new record. Nothing is pushed into the guest: on
  waking, the guest reads each channel after its last sequence number
  (`GET …/channels/{channel}?after=`) and follows it live on `__live`.
  A record's `(fragment, channel, seq)` is the id of whatever it starts,
  so a catch-up never starts something twice (lesson 2).
  - It is its principal's: each agent subscribes for itself, once per
    channel. `GET …/subscriptions` lists the agent's own with `wake: true`
    (`{id, principal, channel, wake}`), and the guest makes one only when
    that list has none, so a repeated `POST` need not be idempotent. A
    member's wake subscriptions go with its membership.
  - A record wakes the computer whoever posted it, the agent's own
    included; the guest ignores its own.
- What a guest follows, by convention (docs/chat-records.md): for each
  agent, `chat` of every fragment it is a member of whose channels
  include a postable `chat` (a chat), and `tasks` of the agent's own
  fragment (its routines, and `joined` when it is added to a fragment).
  The list comes from `GET /api/fragments` and `GET /api/f/{name}/channels`
  as the agent, read again every 5 minutes and on `joined`. So a new
  chat reaches a sleeping computer only through `joined`: whatever adds
  an agent to a fragment (the shell, the chat template) posts
  `{kind: "joined", fragment}` to the agent's `tasks` after.
- What was said before an agent joined a chat is not for it: the guest
  skips a record whose `at` is before the agent's membership's `addedAt`
  (`GET /api/f/{name}/members`).

### Models

- `POST http://model.fragment.internal/v1/chat/completions`, OpenAI's
  shape, with `model` a tier (`cheap`, `medium`, or `high` while it is
  on) and `x-fragment-agent`. The intercept allows only these
  endpoints, the tier's model and a capped `max_tokens`, strips any auth
  header, calls AI Gateway, streams the provider's answer back, and
  meters the final usage to the agent's owner (lesson 7).
- `POST http://model.fragment.internal/anthropic/v1/messages` is
  Anthropic's shape for the high tier, passed through as it is.
- An owner at zero credit, or a fragment past its cap, gets 402 with the
  reason, and no call is made.
- Any other path is 404, unmetered. Hermes probes `GET /api/show`
  (Ollama's model metadata) with no `x-fragment-agent`; it falls back to
  its configured context length.
- A call without `x-fragment-agent` is refused (no one to bill). Our
  Hermes image sets it on every call of an agent's profile (its
  `model.default_headers`), the main model's and the auxiliary ones'
  (titles, the smart-approval guardian).

### Storage

`http://storage.fragment.internal` is an S3 endpoint over the
computer's own R2 prefix: any bucket name and key, scoped by the
intercept. It is for an image's own disaster recovery (Litestream).

### Connections and operator keys

- A connection (Google, Notion, …) is used by sending its placeholder
  to the provider's real HTTPS host: `Authorization: Bearer
  fragment-connection:<provider>`, with `x-fragment-agent`. The
  intercept swaps in a short-lived token from WorkOS Pipes when that
  agent may use that connection, and refuses otherwise (decision 22).
- An operator key (a paid API that needs only a key) is
  `fragment-key:<name>` in the provider's own auth header. The swap
  meters the call to the agent's owner (decision 37).
- HTTPS interception needs Cloudflare's CA: the image waits for it at
  boot and appends it to its trust store.

### Ports

The image may serve HTTP and WebSockets on any port. The platform
proxies `/api/computers/{id}/ports/{port}/…` on the platform's origin,
for the computer's owner and their delegates only (decision 41).
Nothing else reaches the container from outside. By convention the
screen is a page on port 6080 (decision 11). The page is served at the
port's root and reaches its sockets by relative URLs (our images':
`websockify?viewer=` and `control?viewer=`), so it works under the
proxy's prefix.

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
  committed back. An agent fragment's optional `agent.json`
  (`{"tier": "cheap"|"medium"|"high"}`) picks its model tier (medium by
  default; `high` only with `FRAGMENT_HIGH_TIER=on`, decision 23).

## Billing

Awake time is metered at the instance's rate to the computer's owner
(decision 24), except on a $200 seat. Model calls and swaps bill the
agent's owner. At zero credit no wake starts (decision 27).

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
