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
- **Saved** (below, "Saves and what a wake restores"): when its work
  ends (the guest's last keepalive closes, and 30 s pass with none opened
  again), every 15 minutes while a keepalive stays open, and at every
  sleep. Awake, a save is the hold (below), the save, and the hold let go.
- **Sleep**, driven by the DO, in order: hold the guest (below), save
  `/data` (`DirectoryBackup`), take a container snapshot (only after a
  save that worked), send SIGTERM, wait up to 5 s for the guest to exit,
  destroy. A keepalive that opens while an idle sleep holds its guest
  cancels the sleep: the guest took work as it was held, so it stays
  awake, and the hold is let go (its owner's sleep goes on). A sleep whose
  save fails keeps its container: it is awake again, held no more, its
  view's `why` says so, and its sleep is tried again after a pause (a
  minute, doubled each time, at most 15); after
  `computers.unsaved_max_ms` (30 minutes by default) of failed tries it
  sleeps unsaved, says so, and its next wake is a rollback. An idle stop
  by the runtime is only the safety net.
- **Wake from a snapshot** when it caches the current save for the pinned
  image (`start({containerSnapshot})`; below, "Saves and what a wake
  restores"); otherwise start the image with `RESTORE_PENDING=1`, restore
  `/data`, then touch `/run/computer/restored`. A start answered
  "temporarily unavailable" is retried with backoff. A start from the
  snapshot that fails (or whose container stops before it comes up)
  forgets the snapshot, and the computer starts again from the image and
  the save in the same wake, with no strike against it. A start whose
  save will never restore (its archive is gone or altered: the SDK's
  `BACKUP_NOT_FOUND` or `BACKUP_INTEGRITY`) marks that save unusable and
  starts again at once from the save before it, no strike either; with
  none left, it is a strike like any.
- **A crash** (the container stopped on its own while starting or awake)
  counts against its starts, and the computer starts again while
  something wants it. A guest that died holding its keepalive (busy) is
  held as a record holds it, so it starts again however long its turn
  ran, and its next life ends what was cut.
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
- `sh` (and its `test`), `true`, `mkdir`, `touch` and `rm`: the DO polls
  with `true`, opens the restore gate and makes a hold with `touch`, reads
  the guest's answer with `test -e`, and lets go of a hold with `rm`.
- **The hold**, a handshake before every save (P2 of
  docs/explorations/pi-durable.md). The DO removes `/run/computer/held`,
  touches `/run/computer/hold`, and waits up to 20 s for the guest to
  touch `/run/computer/held`: its answer that it claims nothing now (no
  claim in flight, none new while the hold exists) and has copied what it
  keeps that a hot copy would tear. Then the DO saves. An awake save then
  lets go (it removes both files); a sleep keeps the hold until its
  container is gone. An image that never answers is saved anyway, the
  save recorded as not held. What the guest already claimed may run on: a
  sleep cuts it with the container, and an awake save saves it as it is.
  So a save has every turn its guest claimed, and a message that arrives
  as it goes to sleep is left for the next life (or, at an idle sleep's
  hold, wakes the guest's keepalive, which cancels the sleep). A fresh
  container never has either file: an image's is made without them, and
  the DO removes them from a container started from a snapshot (the sleep
  that took the snapshot held it) before that start is ready. A container
  that outlives its sleep (a destroy that did not take) has them removed.
  Our bridge answers for both our images (docs/bridge.md, `BRIDGE_HELD`).

### Data and the restore gate

- `/data` is the only directory kept across sleeps. Everything else is
  the image's. The image does not ship `/data`: the restore makes it,
  swapping a restored directory into place, which overlayfs refuses for
  a directory from an image layer (EXDEV); on a first start the image
  makes it itself.
- With `RESTORE_PENDING=1`, the image waits for `/run/computer/restored`
  before it reads `/data`. Without it, `/data` is ready at start (a
  snapshot wake, or a first start with an empty `/data`).
- The hold (above, `/run/computer/hold`) is read the same way: our
  bridge checks it before every claim (docs/bridge.md, `BRIDGE_HOLD`).
- SIGTERM means stop now: flush and exit within 5 s. `/data` was saved
  before the signal; whatever is written after it may be lost.

### Saves and what a wake restores

P2, P3 and P7 of docs/explorations/pi-durable.md, and step 1 of
docs/durable-computers.md. A computer keeps its newest three saves of
`/data`, and a snapshot is only ever a cache of the current one:

- **A save** is a `DirectoryBackup` of `/data`, taken under the hold
  (above). The Computer DO keeps its record with a number (1 for the
  computer's first, one more for each after), when it was taken, the
  start it was of, and whether its guest answered the hold (`held`). It
  keeps the newest three and deletes each older one's archive as a new
  one is kept. The view lists them (`saves`), newest first.
- **When.** A save is asked for when the computer's work ends (its
  guest's last keepalive closes, and 30 s pass with none opened again, so
  turns back to back save once), every 15 minutes while a keepalive stays
  open (Cloudflare's auto-save guide: a computer that never goes idle is
  saved on a timer), and at every sleep. An always-on computer, which
  never sleeps, is saved the same way. A save that fails awake is tried
  again after a pause (a minute, doubled each time, at most 15). So a
  crash loses what its life did since its last save, at most a turn and
  its settle, or 15 minutes of one.
- **A sleep whose save fails** keeps its container (I5): it is awake
  again, held no more, and its view's `why` says so; its sleep is tried
  again after the same pause. Only after `computers.unsaved_max_ms` (30
  minutes by default: a default for Paul to confirm) of failed tries does
  it sleep unsaved, its `why` saying so until a save works, and its next
  wake is a rollback.
- **A wake restores the current save:** the newest one not found
  unusable. A start whose save will never restore (its archive gone or
  altered) marks it unusable and starts again at once from the save before
  it (F5), with no strike against the computer; with none left to try, it
  is a strike like any other failed start. A start that fails for another
  reason (an image pull, the platform) is a strike, and tries the same
  save again.
- **The snapshot's record** is `{id, image, save}`: the image's reference
  its start ran (a pin while it runs is the next start's) and the id of
  the save its sleep took just before it. A wake uses it only when `save`
  is the current save and `image` the pinned image's reference; otherwise
  it is the image and the save. A sleep whose save failed takes none, and
  a snapshot that fails forgets nothing: the one kept is still a cache of
  its own save. So a snapshot only ever makes a wake faster, never
  different (I9).
- **Why the two agree.** The snapshot is taken after the backup within
  one sleep, with the guest held from before the backup (the hold, above)
  until it is gone: it claims no new turn in between. A turn it had
  claimed before the hold can still write between the two.
- **A broken snapshot** (one that will not start: expired, say) is
  forgotten as its start fails, and the same wake starts the image and
  restores the save, with no strike against the computer (F7). Only the
  hosted lane can show what Cloudflare answers for one (local workerd
  takes no snapshots: the lifecycle's fallback is proven pure, in
  `crates/core`).
- **What a wake restored.** Each start that comes up records which save
  it restored (`snapshot`, `backup`, or `nothing`: a computer never
  saved), that save's id, when it was taken and how old it was, and how
  the life before it ended (`sleep`, `exit`, or none for its first). It
  logs one line (`"restored"`) and shows it in its owner's view
  (`restored`, docs/api.md). A start that went back in time is a
  **rollback**, counted in the view's `rollbacks`: the life before it
  ended by a crash, or by a sleep that slept unsaved, or its start fell
  back to a save older than that life's newest, so what that life did
  since some save is in none. A start from the save its life's sleep took
  is none. Nothing's correctness depends on the guest reading either.

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
    identity, name, owner, connections, credentials: [{provider, kind,
    env, placeholder, hosts}]}], credentialEnv}`: the agents to run, and
    each one's credentials (below, Connections and operator keys). **They
    may change while the computer runs** (its owner assigns or unassigns
    an agent, connects an account, narrows an agent): the guest reads them
    again while awake and runs the new set, with nothing restarted. The
    platform never restarts a computer for a change of its agents, so an
    image must not rely on reading them once, at its start. An agent
    unassigned signs nothing from that moment (the intercept refuses it),
    and its placeholders are refused, whatever the guest still runs.
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
  themselves from `GET /api/computer`, read again every minute (the
  bridge), or every 3 s (our Hermes image, below).
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
  computer is followed while it is awake, with nothing restarted: within
  a minute on the stub, within seconds on our Hermes image. A sleeping
  one reads it as it starts (wake it: `POST /api/computers/{id}/wake`, or
  add the agent to a fragment).
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

Paul, 2026-10-04 (decisions 22, 37 and 44): every credential a guest uses
is a per-agent placeholder in the environment variable its provider's own
SDK reads, so any SDK or CLI works unmodified, with no header of ours. The
real credential is added only at the computer's egress, and only toward
the provider's own hosts.

- **The catalog.** What a deployment offers is one typed list, its
  configuration's `providers` (`FRAGMENT_PROVIDERS`;
  `fragment_core::catalog`, at most 64 rows). Each row is a provider:
  - its `name` and `kind`: a `connection` (WorkOS Pipes: a short-lived
    token for the person's account, acting as them), an `operator` key (the
    operator's, lent to every agent, each call metered to the agent's owner
    at its price and the margin), or an `own` key (the person's own, which
    they give the platform: never metered);
  - its `hosts` (1 to 16): the only hosts its credential is sent to;
  - its `placements` (1 to 4), where the credential goes on a request: a
    header and its format (`{"header": "authorization", "format": "Bearer
    {}"}`, `{"header": "xi-api-key"}`), a query parameter (`{"query":
    "key"}`), or a half of basic auth (`{"basic": "password"}`);
  - its `env` (1 to 4): the environment variables a guest is given its
    placeholder in, the vendor SDK's names where it has one;
  - an operator key's `price` per call at list, or by default the price
    book's for its name (`fragment_core::price::DEFAULT_KEYS`, each with its
    source); a row with none is refused before a deploy.

  Adding a provider is a catalog row (and, for a connection, the provider
  enabled in the WorkOS environment); nothing in code names one. The
  platform's catalog (`deploy/example.jsonc`, `deploy/e2e.jsonc`):

  | Provider | Kind | Hosts | Placement | Environment variable | Price per call (list) |
  |---|---|---|---|---|---|
  | `google` | connection | `gmail`, `www`, `people`, `sheets`, `docs` `.googleapis.com` | `Authorization: Bearer {}` | `GOOGLE_OAUTH_ACCESS_TOKEN` (Terraform's Google provider reads it; Google's client libraries take a token as given) | none |
  | `perplexity` | operator | `api.perplexity.ai` | `Authorization: Bearer {}` | `PERPLEXITY_API_KEY` (Perplexity's SDKs) | $0.005 |
  | `google-places` | operator | `places.googleapis.com` | `X-Goog-Api-Key: {}`, or `?key=` | `GOOGLE_PLACES_API_KEY` (no Google SDK reads one; the goplaces skill's helper does) | $0.035 |
  | `xai` | operator | `api.x.ai` | `Authorization: Bearer {}` | `XAI_API_KEY` (xAI's SDK) | $0.12 |
  | `elevenlabs` | operator | `api.elevenlabs.io` | `xi-api-key: {}` | `ELEVENLABS_API_KEY` (ElevenLabs' SDKs) | $0.15 |

- **A placeholder** is `fcx_<provider>_<tag>` for a connection and
  `fck_<provider>_<tag>` for a key. `<tag>` is 32 hex of HMAC-SHA256 over
  (computer, agent fragment, provider) under a key only the platform holds,
  derived from its host secret (`swap::TagKey`; docs/secrets.md). So a
  placeholder names its agent and its computer, no one can make one, and a
  guest that leaks one leaks nothing usable from anywhere else. It is
  stable (the same agent, computer and provider make the same tag), so an
  image may write it once.
- **The guest learns its credentials** from its own view (`GET
  /api/computer`, below): each agent's `credentials`, `[{provider, kind,
  env, placeholder, hosts}]`, those it may use now:
  - a connection its owner has connected (Pipes says `connected`: the
    connected account's state, read with no token minted, believed for a
    minute; the owner's own read of their connections, and a swap Pipes
    refused, tell the computer at once);
  - an operator key the deployment holds;
  - an own key its owner gave (`PUT /api/connections/{provider}/key`,
    sealed by the computer);
  - none its owner narrowed it from.

  And `credentialEnv`: every environment variable of the catalog, held now
  or not, so an image can pass them all through once. A guest reads the view
  again while awake (our Hermes image every 3 s) and puts each placeholder
  in its variables.
- **The intercept** catches the catalog's hosts on HTTPS and on plain HTTP
  alike, and always sends on over HTTPS. For a request to one of them it
  finds each placeholder (`swap::Plan`) in a header value, in a half of
  `Authorization: Basic`, in the query string, or in the path, wherever
  one begins as a token of its own (one inside a JWT is none), and refuses
  the request, saying why and reaching no provider, when a placeholder:
  - is malformed (a prefix, a provider and `_`, then no 32-hex tag): 400;
  - names a provider the deployment does not offer, or has the other
    kind's prefix: 400;
  - is sent to a host that is not its provider's: 403 `forbidden`;
  - is in a place its provider does not take it (another header, a query
    parameter it does not name, the path), or is not alone where it must
    be (a query value, a half of basic auth), or there are more than 4: 400;
  - has a tag that names no agent on this computer now: forged, another
    computer's, or an agent removed since (removing an agent revokes its
    tags): 403 `forbidden`;
  - names more than one agent in one request: 400;
  - names an agent its owner narrowed from that provider: 403 `forbidden`;
  - is a connection its owner has not connected, or must authorize again:
    403 `not_connected`; an own key its owner has not given: 403
    `not_connected`;
  - is an operator key's whose owner's ledger refuses a paid call: 402 or
    403, the ledger's reason.

  Otherwise each placeholder's place gets its credential: a header is its
  format around it (`Bearer sk-…`, whatever scheme word the guest wrote), a
  query parameter the credential (percent-encoded), a half of basic auth
  the credential (the other half as it came). No `x-fragment-agent` is read
  or needed (a hard cut), and no `x-fragment-…` header goes to a provider.
  A connection's token is held until a minute before it expires, at most
  ten minutes. A request to such a host with no placeholder goes on as it
  came; a body is sent as it came, read whole (at most 32 MiB); a redirect
  is never followed (the guest follows it, without the credential).
- **After the provider answers** (anything under 500), each operator key's
  call is metered to the agent's owner (`key:<computer>:…`, at the price
  book's price and the margin), and every call is counted as the agent's
  in the computer's `uses`: by month, provider and agent, its calls and
  what they were charged (a connection's and an own key's are counted,
  never charged). Its owner reads them (`GET /api/computers/{id}/uses`,
  docs/api.md), and the shell's Connections page shows them.
- **Who may use what.** An agent may use every provider its owner has
  (decision 44: a person's agents are not fenced from each other); its
  owner may narrow one agent to a list (`PUT
  /api/computers/{id}/agents/{fragment}/connections {connections:
  [provider]}`, and `{connections: null}` for every one again, the
  default). One agent's guest could send another's placeholder, and that
  is by design: the call is that agent's, and a narrowed list is an agent's
  specialization, never a wall. Walls stand between people (decision 36).
- **Approvals** are Hermes' own (Paul, 2026-10-04): a person who connected
  something lets their agents use it, with no approval of the platform's.
- HTTPS interception needs Cloudflare's CA: the image waits for it at
  boot and appends it to its trust store.
- What the catalog cannot express is never swapped: a key in a path is
  refused, and a body goes as it came (a placeholder in it reaches the
  provider, which refuses it). A vendor that signs its requests with the
  key needs more than a placement.

### Ports

The image may serve HTTP and WebSockets on any port. The platform
serves them on the computer's own origin, `<24 hex>--computer.<suffix>`
(cross-site from the platform, so a page the guest serves can act as no
one), at `/p/<port>/…`, for its owner only for now (delegates come with
decision 41's sharing). A browser gets there with a one-time ticket its
owner mints (`POST /api/computers/{id}/ports/{port}/ticket` → `{url}`),
which that origin redeems into a session cookie of its own; a signed
request (the CLI's) needs none. The shell shows a port in a tab of its
own page (decision 11): the ticket's URL as a frame's `src`, redeemed in
the frame into a partitioned cookie for the platform's page
(`fragment_computer_frame`, `SameSite=None; Partitioned`), which
browsers that block third-party cookies keep. Only the platform's page
may frame a port (every answer carries `frame-ancestors <platform>`),
and every fragment's page is one site with the computer's origin, so
its cookies count only as on a fragment's (docs/api.md, Computers): on
the port's own page's requests, a top-level visit (the tab's cookie) or
a frame's navigation (the frame's), never another page's fetch or
frame; a socket only from the port's own page. So an image's page may
not frame its own ports either: a screen is one page, its sockets
relative to it. A WebSocket on a port is bridged through
the Computer DO and holds it awake while open; the container may speak
first (an RFB server does), and its first word reaches the page. Nothing else reaches the
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
  (docs/chat-records.md). The managed skills and the fragment CLI: below,
  "Skills and the CLI in our Hermes image". An agent fragment's optional
  `agent.json`
  (`{"tier": "cheap"|"medium"|"high"}`) picks its model tier (medium by
  default; `high` only with `FRAGMENT_HIGH_TIER=on`, decision 23).

  Its agents change while it runs (`images/hermes/boot/src/agents.rs`),
  and nothing restarts for it: no container, gateway or bridge, so no
  other agent's turn is cut or waits. `hermes-boot` reads `GET
  /api/computer` every 3 s. For an agent assigned since, it writes the
  agent's profile as a boot does (its directories, its config whole
  before its `.env`, its repo pulled), then asks the gateway to serve it
  (the `rescan-profiles` verb on its control socket, `gateway.sock`, as
  Hermes' own `profile create` does: Hermes v0.21.5's multiplexed gateway
  serves a profile made after it started, and a relayed turn resolves
  its profile's directory as it arrives), and only then names it in the
  bridge's ready file (`BRIDGE_AGENTS_FILE`, docs/bridge.md): the bridge
  runs only the agents that file names, so no turn reaches a profile
  that is not whole (the 401 of a turn run in Hermes' default profile,
  which names no agent to bill). On the lower rung (`cargo test -p
  fragment-bridge --test docker -- --ignored`) an agent assigned to a
  running image answers its first message about a second after it is
  assigned. An agent unassigned leaves the ready file at once, so the
  bridge stops running it; its profile is retired at the next start, when
  no Hermes could be winding down a turn in it. The screen is the first
  agent's desktop through a link (`/var/lib/fragment-run/screen.sock`)
  that moves with the first agent, started by `hermes-boot screen-start`
  for the screen's first viewer (about a second on the lower rung, and
  about 300 MiB more while it runs), or by Hermes at an agent's first
  `computer_use` or browser call (`bot_desktop.auto_start`), never at
  boot. On that desktop an agent operates: Hermes' `computer_use` (its
  backend, cua-driver 0.28.3, is in the image, pinned, and named by
  `HERMES_CUA_DRIVER_CMD`; Hermes lists the tool in its `tool_search`
  bridge and the agent calls it through `tool_call`), and its built-in
  browser tools, headed there (`browser: {headed: true, backend: off}` in
  each profile's own config, the only place Hermes reads `browser` from;
  with no backend named, Hermes would fetch the Browser Use CLI into
  `/data` at the first call);
  Litestream starts again when the set of databases it streams changes
  (a new profile's appears at its first turn). Events: `agents.changed`,
  `profile.written`, `agents.served` (the gateway's answer and its
  `ms`), `agents.ready` (the whole change's `ms`).

### Skills and the CLI in our Hermes image

The platform serves skills as any fragment's files; what installs them,
and how a runtime finds them, is the image's.

- **The managed set** (decision 17) is the owner's skills fragment's
  `skills/`: a fragment on the blessed `skills` template lists and reads
  the platform release's managed set as its files (templates/skills/README.md),
  beneath any file of its own. `hermes-boot` finds it as the computer's
  first agent acting for the computer's owner (`GET /api/fragments?for=`:
  of kind `skills`, named under the owner's username, `skills.<username>`
  first), lists it (`GET /api/f/{skills}/files?for=`) and fetches what is
  new or changed (`…/file?path=&for=`, eight at once), each file by its
  listed version. It installs them read-only (the boot's, mode 0644) at
  `/data/hermes/managed-skills`, `skills/<category>/<name>/…` as
  `<category>/<name>/…`, and removes what the listing no longer has; no
  skills fragment means no managed skills. It does so off the boot's path,
  at each start and every ten minutes while awake, or every minute while
  the owner has no skills fragment (`skills.installed`, `skills.failed`);
  `/data` keeps the last install, so a wake fetches only what a release
  changed. Bounds: 1,000 files, 256 KiB each, 8 MiB in all (`skills.rs`);
  a file past one, or at a path that is no safe relative path, is refused
  and the rest installs. A person whose agents predate their skills
  fragment (setup makes it since 2026-10-03) gets one as the shell loads,
  once, from the blessed template, as setup makes it (shell.js,
  `backfillSkills`); their awake computers install it within the minute.
- **The platform skill**, `fragment`, is every profile's, whatever the
  skills fragment holds: what an agent knows of the platform it is on. It
  is the image's own `fragment` CLI's skill (`fragment skill`, cli/SKILL.md:
  what a fragment is, the commands for the agent's fragments) after a page
  of what the computer adds (`images/hermes/boot/src/computer.md`: that it
  is an agent on a Fragment computer acting for its owner with no login,
  the apps and brain skills to load, its connections as placeholders in
  its environment, `GOOGLE_OAUTH_ACCESS_TOKEN` and the Google Workspace
  skill, and its desktop, which its owner watches and can take over from
  "Its computer's screen"), with a description for Hermes' skills index.
  `hermes-boot build-info` writes it at the image's build
  (`/opt/fragment/skills/platform/fragment/SKILL.md`, read-only to the
  agents), so it is always the binary's in the image, and costs a boot
  nothing; the build fails if `fragment skill` is no skill named
  `fragment`. A missing `fragment skill` instruction belongs in cli/SKILL.md.
- **Every profile** names the managed directory, then the platform skill's,
  in `skills.external_dirs`, after its own `skills/` (its agent fragment's,
  synced both ways: an agent's own skills are versioned in its fragment).
  Hermes takes the first skill of a name, so an agent's own wins over a
  managed one, and either over the platform skill. Hermes' bundled skills
  are the default profile's only; an agent's profile has its own, the
  managed set, which is what the shell's Skills section lists, and the
  platform skill. A managed skill a session has not yet seen appears at its
  next session.
- **The fragment CLI** is in the image (`/usr/local/bin/fragment`, built
  from `cli/` with the image: the Hermes image's build context is the
  repo's root). Each profile's `.env` names its agent and its owner
  (`FRAGMENT_AS_AGENT`, `FRAGMENT_FOR`), and its config passes those two to
  its terminal (`terminal.env_passthrough`, scoped to the profile under the
  one gateway), so a command in an agent's terminal acts as that agent:
  the CLI's agent mode (cli/GUIDE.md, "As an agent") names the agent in
  `x-fragment-agent`, signs nothing, and reaches the platform at
  `FRAGMENT_API`, acting for the owner on the routes that honor `for`. The
  egress signs. No key is in the container. `fragment sync` and `deploy`
  still reach code.storage directly, with the short-lived, repo-scoped
  token the platform mints for the agent.
- **Its credentials** (Connections and operator keys, above):
  `hermes-boot` writes each agent's placeholders into its profile's `.env`
  under their variables (`PERPLEXITY_API_KEY=fck_perplexity_…`), which
  Hermes reads again at every turn, so its own tools (its Perplexity web
  search, ElevenLabs speech, xAI) use them too; and the profile's config
  passes every variable of the catalog (`credentialEnv`) to its terminal,
  named once since Hermes reads the list once per gateway. Hermes never
  passes a name it keeps for its own providers' keys (its
  `_HERMES_PROVIDER_ENV_BLOCKLIST`: `PERPLEXITY_API_KEY`, `XAI_API_KEY`,
  `ELEVENLABS_API_KEY` among them), so the profile's terminal also sources
  `credentials.sh` (`terminal.shell_init_files`, after Hermes' own three),
  which exports each one as a terminal session's shell starts. A change
  (a connection made or lost, a narrowing) rewrites both within 3 s,
  nothing restarted: a passed-through variable is current at the next
  command, one from `credentials.sh` at the next terminal session (an
  operator key's placeholder does not change). So a skill's helper, a
  stock `curl` or a vendor's SDK in the terminal finds its variable, with
  no header of ours (`profile.credentials` in the events).
- **`gws`**, Google's Workspace CLI (github.com/googleworkspace/cli,
  v0.22.5, pinned by its checksum), is in the image for the `google`
  connection: `/usr/local/bin/gws` hands it `GOOGLE_OAUTH_ACCESS_TOKEN`
  as its pre-obtained access token (`GOOGLE_WORKSPACE_CLI_TOKEN`, which it
  sends as `Authorization: Bearer`), so the swap fills it on Google's
  hosts; it trusts the system's CA bundle (rustls with native roots), so
  the interception CA appended at boot is its too. The image holds no
  secret of it. The google-workspace skill prefers it.

## Billing

- A computer's container starts at the size its awake time is priced at:
  `FRAGMENT_COMPUTER_INSTANCE` names the price book's instance (default
  `2vcpu-6gib`, decision 13), and its size goes to `ctx.container.start`
  as `instance` (`fragment_core::price::instance_size`: a Containers type
  by name, or `<n>vcpu-<m>gib`, a custom size with 2 GB of disk a GiB).
- Awake time is metered at the instance's rate to the computer's owner (decision 24):
  an interval every five minutes awake and one at each sleep, kept by the
  Computer DO until the owner's ledger has it (each once, by its
  reference `awake:<computer>:<from>`). A $200 seat's awake time is not
  charged.
- Model calls bill the agent's owner, through the platform's model
  route. Each operator key's call the provider answered is metered to
  the agent's owner at the key's price and the margin (`key:<computer>:…`;
  decision 37); a catalog's operator key always has a price (its own or
  the price book's list price). A connection's call and an own key's are
  counted, never charged; every call is in the computer's `uses`.
- At zero credit, or with agents stopped, no wake starts (decision 27):
  the owner's wake is refused with the ledger's reason (402
  `budget_used_up` at zero credit or a canceled seat; 403 for a guest,
  who pays for nothing), which the view's `why` keeps; a record, a join or a page wakes nothing; no model call or
  key call is made. A computer already awake runs on until it sleeps.

## Tests

- `crates/core`: the lifecycle as a pure state machine (wake racing
  sleep, the newest push wins, a deadman alarm, a failing wake sealed, a
  crash while busy started again, a broken snapshot's fallback, each life
  ending once; saved when work ends, on its busy timer and always on, a
  sleep's hold, save and stop, a keepalive that cancels an idle sleep's
  hold, a failed sleep's save keeping its container within its bound, a
  save that will not restore falling back to the one before), and its
  saves (three kept, which a wake restores, the snapshot as a cache of one
  save and one image, a slow sleep asked twice, what a wake restored and
  its rollbacks), under seeded interleavings with crashes, holds the
  guest answers or not, and saves that fail; among their invariants, a
  computer is never asleep with work newer than its newest save unless a
  crash or the bounded failure put it there.
- The e2e on workerd: the Computer DO's routes and its intercepts,
  against `images/stub/` under `wrangler dev` with Docker. A crash is the
  lever's (`POST /api/test/computer {computer, op: "kill"}`: SIGKILL to
  the guest's PID 1, so the real exit is reported), and wakes from the
  save its last turn's end took; a failed save is the lever's
  (`fail-saves`), and so is an always-on plan (`always-on`). A check of
  what ran counts runs (the model fake's calls, the ledger's rows, the
  computer's `uses`), never records, which a second run replays.
- The real-Hermes lane: `images/hermes/` with a scripted model (phase
  4's exit list), a second agent assigned to the awake computer while the
  first's turn runs included.
- The images' own (`images/`, its own workspace: `cargo test` and
  `cargo clippy --all-targets -- -D warnings` there): the bridge's engine,
  pure; the bridge against an in-process fake fragment API, with the
  `script` runtime and with the `relay` runtime against a scripted
  Hermes gateway; and, with Docker, both images built and run against
  the fake API and a scripted model on the host
  (`cargo test -p fragment-bridge --test docker -- --ignored`), real
  Hermes included. These are lower rung: fakes at the platform's edge.
