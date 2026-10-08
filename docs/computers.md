# Computers: the platform's contract with an image

Status: **the contract phase 4 builds** (docs/cloudflare-v1.md, decisions
13, 18, 19, 22, 23, 39, 41–44). The platform side is the generic Computer
Durable Object in the cell. The image side is any image: ours runs
goose (`images/goose/`, docs/optchat.md), and a stub image
(`images/stub/`) proves the platform needs nothing runtime-specific.
Nothing here names an agent runtime. A change that would have to is a
design bug (the rule).

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
- A computer is deleted only with its owner, by an operator's wipe of them
  (docs/api.md, Operators; below, "Deleted with its owner"). A new
  identity is a new computer: its id is its owner's.

## Lifecycle

States: `asleep` → `starting` → `awake` → `sleeping` → `asleep`, plus
`failed` (a wake that keeps failing, lesson 4: "won't wake", no more
container starts until someone asks again).

- **Wakes:** a record on a channel one of its agents subscribed to with
  `wake: true`; an open port tab; a pre-wake (a page opened a subscribed
  fragment, or someone started typing there: it starts at once and stops
  after 60 s if nothing arrives; never by its own agents' sockets, which
  its guest opens as it boots and again after one drops, so its own guest
  never starts it again as it goes to sleep); one of its agents added as
  a member of any fragment (below: the platform posts `joined` and wakes
  it, held as a record holds it); the owner's `POST
  /api/computers/{id}/wake`.
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
- `sh` (and its `test`), `true`, `mkdir`, `touch`, `rm` and `cat`: the
  DO polls with `true`, opens the restore gate and makes a hold with
  `touch`, looks for the guest's answer with `test -e` and reads it with
  `cat`, and lets go of a hold with `rm`.
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
- **A hold not answered.** Until it answers, a guest may say why in
  `/run/computer/unheld` (optional; at most 1 KiB of text: what it waits
  on, or why it cannot answer). Only once the 20 s run out does the DO
  read it, and it logs it with the hold, in the hold's one line
  (`{computer, hold, held, sleep, ms, tries, last, unheld}`: `tries` the
  DO's looks for the answer, `last` the last one's exit code or error;
  `fragment_core::computer::unheld`). Nothing is decided by it. The DO
  removes it with the hold's other files. The stub says nothing.
- **What the save leaves out.** The guest's `held` may name, one per
  line, what its save leaves out: gitignore patterns relative to `/data`,
  of letters, digits and `._-/*?[]`, at most 512 of at most 256 bytes, the
  answer at most 64 KiB (`fragment_core::computer::left_out`). They are
  files it copied under the hold to names the save keeps (its SQLite
  databases, say), so a hot copy of one, which could
  tear (F4), is never what a wake restores. An empty answer leaves nothing
  out; an answer the DO refuses saves the guest whole; a save not held
  leaves nothing out. Our bridge answers for the stub, naming what
  `BRIDGE_HELD_LEAVE_OUT` names (the stub's `*.scratch`, only so the
  platform's lanes can see a save leave something out).

### Data and the restore gate

- `/data` is the only directory kept across sleeps. Everything else is
  the image's. The image does not ship `/data`: the restore makes it,
  swapping a restored directory into place, which overlayfs refuses for
  a directory from an image layer (EXDEV); on a first start the image
  makes it itself.
- **The seam** (step 2 of docs/durable-computers.md): `/data/work` is
  what the guest's tools write (a terminal's working directory and home,
  projects, a browser's profile, scratch files), and the rest of `/data` is the
  guest's own state (its runtime's databases, sessions and config, and the
  bridge's state). Each save is two
  `DirectoryBackup` records, `/data` (its work left out, and what a held
  guest names) and `/data/work` (nothing left out: its SQLite files are
  copied as they are), restored together, `/data` first. The DO makes
  `/data/work` with its hold, so it always exists to save. Nothing in the
  work is copied under the hold: that it lives apart is what lets it move
  to a sandbox of its own later (step 4, E). A save taken before the seam
  is one record of `/data`, which restores whole as before.
- With `RESTORE_PENDING=1`, the image waits for `/run/computer/restored`
  before it reads `/data`. Without it, `/data` is ready at start (a
  snapshot wake, or a first start with an empty `/data`).
- **The restore's check** (optional): an image may carry an executable
  `/usr/local/bin/computer-check`. The DO runs it, as root, after it
  restores `/data` and before it opens the gate, so nothing reads `/data`
  meanwhile (at most 2 minutes: past them it is killed, SIGKILL, as any
  exec of the DO's is past its bound, and the start fails). It may put
  what it copied under the hold back in place, and check what it
  restored. Exit 0: whole. Exit 3: the save is unusable, and the DO
  marks it so and starts again from the save before it. Any other exit:
  the check itself failed, a failed start like any (tried again, on the
  same save).
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
  one is kept (and what a save that failed had taken), keeping each
  record until its delete worked, so a delete that failed is tried again
  at the next save (at most 16 records wait; past that the oldest's
  archive stays in R2, logged). The view lists them (`saves`), newest
  first.
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

### Deleted with its owner

An operator's wipe of its owner (docs/api.md, Operators) asks the Computer
DO to wipe itself (`computer/wipe`, an internal route), until it says
nothing is left:

- it is **marked wiped first**, in its storage: from the mark on no write
  of its lands (a save, a start's report, a meter under way when the wipe
  began fails at its next write), its alarm is gone, and every route
  answers as a computer never made (404), so nothing wakes or starts it
  again;
- its **container is destroyed**, whichever start runs it, and waited
  out;
- **every save goes from R2**: each record its saves name (kept, and let
  go of and not yet deleted), through the same `DirectoryBackup` delete a
  save's own `forget` makes, then whatever else is under its saves'
  prefix (`computers/<object id>/backups/`: an upload its destroy cut
  short), ten pages a call;
- once none is left, **its record is emptied** (its tables dropped and
  made again empty, its mark kept): its saves, lifecycle, agents, uses,
  own keys, tickets and sessions, and the **snapshot's** id.

Cloudflare deletes no container snapshot: the Worker's container binding
takes one (`snapshotContainer`) and starts from one, and has no delete,
and Cloudflare keeps one for 30 days after it was last restored. A wiped
computer's snapshot is forgotten, so nothing ever restores it, and it
expires within 30 days. The container application is the deployment's
(every computer on an image shares it), so a wipe leaves it.

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
    computer stays awake. Hold it while busy, and while a turn waits on
    its card, until the card is answered or expires (decision 42 as
    amended; docs/bridge.md, "A card keeps its computer awake").
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
  bridge).
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
  a minute on the stub. A sleeping
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
- Any other path is 404, unmetered. Anthropic's shape
  (`/anthropic/v1/messages`) comes with the high tier, which is off
  (decision 23).
- `model` may also be `vision`: the deployment's vision model (its
  config's `vision_model`, the cell's `FRAGMENT_VISION_MODEL`), GLM-5.3
  Flash unless it names another (Paul, 2026-10-05). Workers AI's catalog
  marks GLM-5.3 Flash "Vision: Yes"; GLM-5.3, the medium tier's, reads no
  images. The vision model must be one the price book prices, or the
  deploy is refused (and the cell, at its first request); GLM-5.3 Flash
  is the cheap tier's own row, so no new book version. `vision` is
  bounded, reserved and settled as a tier's call is, at its model's
  price, to the agent's owner. It is no tier: an agent's `agent.json`,
  a job's step and a manifest name only tiers.
- A call is at most 6 MiB (`fragment_core::models::MODEL_BODY_MAX_BYTES`),
  room for an image of 5 MiB of base64
  (`fragment_core::models::IMAGE_DATA_URL_MAX_BYTES`). A call reserves its
  bytes as input tokens, so a 5 MiB screenshot holds about $1.25 of its
  payer's credit (at
  GLM-5.3 Flash's price, the fee and the margin) until it settles at what
  the model counted; a payer with less is refused it (402).
- A call without `x-fragment-agent` is refused (401: no one to bill).
- The intercept names no fragment, so a call bills its agent's owner
  and no fragment's cap applies (decision 36: an agent's model calls are
  its owner's).

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
    minute; the owner's own read of their connections, and each swap's
    answer from Pipes, tell the computer at once);
  - an operator key the deployment holds;
  - an own key its owner gave (`PUT /api/connections/{provider}/key`,
    sealed by the computer);
  - none its owner narrowed it from.

  And `credentialEnv`: every environment variable of the catalog, held now
  or not, so an image can pass them all through once. A guest reads the view
  again while awake and puts each placeholder
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
  - is an operator key's, and its owner's ledger refuses to hold the
    call's price: 402 or 403, with the ledger's reason. A ledger that does
    not answer refuses it too (5xx), since a key is the operator's money.

  Otherwise each placeholder's place gets its credential: a header is its
  format around it (`Bearer sk-…`, whatever scheme word the guest wrote), a
  query parameter the credential (percent-encoded), a half of basic auth
  the credential (the other half as it came). No `x-fragment-agent` is read
  or needed (a hard cut), and no `x-fragment-…` header goes to a provider.
  A connection's token is asked of Pipes for each request, never held:
  Pipes holds and refreshes it, so a connection disconnected or revoked
  stops at the next request, and the state the guest's view lists follows
  Pipes' answer. The owner's WorkOS user, which Pipes asks for, is the
  registry's once and then kept by the computer: the first subject a
  person signed in as with an issuer never changes. A request to such a
  host with no placeholder goes on as it
  came; a body is sent as it came, read whole (at most 32 MiB); a redirect
  is never followed (the guest follows it, without the credential).
- **An operator key's call is held first**, as a model call is
  (docs/ledger.md): its price (the price book's and the margin) is reserved
  on the agent's owner's ledger (`key:<computer>:…`) before the request
  goes on. Once the provider answers (anything under 500), the call
  settles at that price; one it did not answer is released. Then every
  call is counted as the agent's
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
- **Approvals** are the agent runtime's own (Paul, 2026-10-04): a person
  who connected something lets their agents use it, with no approval of
  the platform's.
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
first (an RFB server does), and its first word reaches the page. A close
at either end closes the other, with its code, or 1000 for one that only
a receiver reports (1005, none given; 1006, dropped), which workerd
refuses to send: passed on as it was, it left the page's end open (p5,
2026-10-05: a desktop that restarted froze its screen's page). Nothing else reaches the
container from outside. By convention the screen is a page on port 6080
(decision 11), one for each agent: the shell opens an agent's at
`/p/6080/?agent=<agent fragment>` (a ticket that lands there: `POST
…/ports/6080/ticket {path: "/?agent=…"}`), from a chat with the agent
("Its screen" in the chat's menu), or with the agent picked from a group
chat's menu ("<name>'s screen"), or from settings. The platform carries
the query and reads nothing in it: which desktop is that agent's, and
the refusal of an agent the computer does not run, are the image's. The
page is served at the port's root and reaches its sockets by relative
URLs, naming the agent again (our images': `websockify?viewer=&agent=`
and `control?viewer=&agent=`), so it works under the port's prefix
(`/p/6080/`).

## Our images

Each is under `images/` (docs/bridge.md has the bridge's routes,
settings and state):

- `images/bridge`: the process each image runs between an agent runtime
  and the fragment API, with the chat records of docs/chat-records.md.
  Its core names no runtime; `script` (a deterministic agent) and `goose`
  are its runtimes.
- `images/stub`: the bridge with `script`, and a screen page. 9.6 MB; a
  local start follows its chats in 0.3 s. The platform's own lanes run
  against it.
- `images/goose`: the deployment's default image (`deploy/example.jsonc`),
  the bridge with its `goose` runtime and goose itself. Its design is
  docs/optchat.md, "goose on the computer"; its build is `images/goose`.

## Billing

- A computer's container starts at the size its awake time is priced at:
  every computer is the price book's instance `fragment_core::price::INSTANCE`
  (`2vcpu-6gib`, decision 13), and its size goes to `ctx.container.start`
  as `instance` (`fragment_core::price::instance_size`: a Containers type
  by name, or `<n>vcpu-<m>gib`, a custom size with 2 GB of disk a GiB).
- Awake time is metered at the instance's rate to the computer's owner (decision 24):
  an interval every five minutes awake and one at each sleep, kept by the
  Computer DO until the owner's ledger has it (each once, by its
  reference `awake:<computer>:<from>`). A $200 seat's awake time is not
  charged.
- Model calls bill the agent's owner, through the platform's model
  route. Each operator key's call is held on the agent's owner's ledger
  before it is made, and settled once the provider answered, at the key's
  price and the margin (`key:<computer>:…`; decision 37); a catalog's
  operator key always has a price (its own or
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
- The e2e's `wipe` section (crates/e2e/src/lanes/wipe.rs): a computer
  whose `/data` holds a file its agent wrote and a save, wiped with its
  owner (its first step alone, then across a node's crash): no computer
  from that step, its saves gone, its owner's next identity's computer a
  new one that restores nothing and reads no such file; hosted, the same
  on the deployment's own image.
- The ledger lane: the route's `vision` on the Workers AI fake, which
  reads images only on a model the catalog marks "Vision: Yes" (GLM-5.3
  Flash) and answers 400 for one sent to another, and a call carrying the
  largest image the route allows.
- The images' own (`images/`, its own workspace: `cargo test` and
  `cargo clippy --all-targets -- -D warnings` there): the bridge's engine,
  pure; the bridge against an in-process fake fragment API; and, with
  Docker, the images built and run against the fake API and a scripted
  model on the host
  (`cargo test -p fragment-bridge --test docker -- --ignored`). These are
  lower rung: fakes at the platform's edge.
