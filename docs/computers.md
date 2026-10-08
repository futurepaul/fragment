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
  minute, doubled each time, at most 15, the last try at the bound
  itself); after `computers.unsaved_max_ms` (30 minutes, Paul's,
  2026-10-08) of failed tries it sleeps unsaved, says so, and its next
  wake is a rollback. An idle stop by the runtime is only the safety net.
- **Restart** (its owner's: below, "What its owner is told"): running,
  its sleep (the hold, the save, the stop) then a start at once; a save
  that fails keeps no container, since its owner asked. Asleep or won't
  wake, a start. Either way the start is fresh: from the image and the
  newest good save, never the snapshot.
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
  something wants it. One that went while its sleep held or saved it,
  before the sleep stopped it, ended on its own too: its life's end is an
  exit, not a sleep. A guest that died holding its keepalive (busy) is
  held as a record holds it, so it starts again however long its turn
  ran, and its next life ends what was cut.
- **A new isolate** that finds the container running attaches its
  monitor, timeout and intercepts again and never destroys it (lesson 6).
  After any `destroy()` it waits for `running` to be false before
  `start()`.

## What its owner is told

Paul, 2026-10-08, on the unsaved bound: "a user will be still using it and
then it will die and they won't know why. need user-facing recovery so they
can get back to working." Each way a computer stops, starts again, goes
back to an older save or will not start, read from the code
(`fragment_core::computer`, `cell/src/computer.rs`, `cell/shell/shell.js`,
the bridge), what its person saw before 2026-10-08, and what they see now:

| What happens | When | Before: the shell / the chat | Now |
|---|---|---|---|
| **An idle sleep**, saved | nothing holds it for 20 minutes | Settings' "State" only / nothing (a message wakes it) | the same: nothing is lost, nothing to tell |
| **Its owner's sleep**, saved (the shell's update is one) | `POST …/sleep` | the same | the same |
| **A sleep whose save fails** | any sleep; it is kept awake (I5) and tried again after 1, 2, 4, 8, 15 minutes | Settings' `why`, read only when Settings was opened after it / nothing; its owner charged for the time it was kept | an `unsaved` notice at once, over every chat and Settings, pushed to open pages: since when its work is in no save, the save a stop goes back to, the time the bound stops it, and Restart; the time it is kept, up to the bound, is free |
| **Awake saves that keep failing** (a turn's end, every 15 minutes busy) | tried again after a growing pause, unbounded while awake | nothing anywhere: no `why` for an awake save | the `unsaved` notice once two fail in a row (no stop time: none is due until it would sleep) |
| **The bound runs out**: put to sleep unsaved | the sleep tried at `unsaved_since + unsaved_max_ms` (exactly: the last try is the bound's), only when nothing holds it | Settings' `why` ("slept unsaved"), which then stayed for good (only a save of the same life cleared it) / nothing | a `went_back` notice (pending) while it is asleep: what it will go back to; the `why` goes at the next save that works |
| **A crash**: the container stops on its own (out of memory, a host restart, the runtime's idle stop, or dying while its sleep held or saved it) | `Exited`; it starts again at once while anything wants it, else at its next wake | nothing (`restored` and `rollbacks` were in the API only) / a turn it cut ends quietly as "lost when the computer restarted"; the agent's next turn there is told what was cut (docs/bridge.md) | a `went_back` notice: stopped unexpectedly, back to its save of `HH:MM`, how much before it stopped; the chat and the agent as before |
| **A wake past an unusable save** (its archive gone, or the image's check failed) | `RestoreFailed`, the save before it at once | nothing | a `went_back` notice: the newest save could not be restored |
| **Its owner's restart** | `POST …/restart` | (no restart) | saved first: nothing to tell; its save failing, a `went_back` notice for the restart |
| **A wake whose snapshot will not start** | the image and the same save, in the same wake | nothing | nothing: nothing is lost |
| **Won't wake**: three failed starts in a row, three crashes, or no save left to fall back to | `Phase::Failed` | Settings' "State: wont wake" and the platform's words / messages go unanswered, nothing said; and the shell's own presence wake (an owner's) silently tried it again every minute the person was there | a `wont_wake` notice in plain words, the platform's beside them, and Restart; the shell no longer wakes it on presence; after a restart that fails again, "still won't start", and the deployment's support link, or the details to give whoever runs it |
| **A slow or failed start** (fewer than three) | tried again in 5, 10 s | first run's "taking longer than usual" / nothing | the same (passing) |
| **At zero credit, or a guest**: no wake starts | the ledger refuses (402, 403) | Settings' credit warning and `why` / unanswered | the same; the refusal's `why` now goes once a start comes up (it stayed for good); a restart is refused the same way |
| **An image update** | its owner's pin, sleep, wake | the update pill | the same |

**The notices** (`notices` in the owner's view, `fragment_core::computer::notices`,
proto's `ComputerNotice`), the most pressing first, each derived from the
Computer DO's own state with no clock, so the same state tells the same
whoever reads it:

- `wont_wake {why}`: its starts kept failing.
- `unsaved {since, save, failingSince, failures, why, stopsAt}`: this
  life's saves are failing (a sleep's at once; awake ones from the second
  in a row, `UNSAVED_NOTICE_FAILURES`, since one is retried a minute later).
  `since` is this life's newest save, or its start; `save` what a stop
  goes back to; `stopsAt` when the bound puts it to sleep unsaved if no
  save works first and nothing holds it, none while a port tab or a
  keepalive is open (in use) or for an always-on computer.
- `went_back {life, cause, endedAt, pending, save, savedAt, at}`: a start
  went back to an older save (`crash`, `unsaved`, `restart`, `unusable`),
  or will as it next starts (`pending`). Told until its owner says they
  saw it (`POST …/notices/seen {life}`), whatever starts come after; the
  newest loss only.

When they change, the Computer DO tells its owner's open pages through
their list's socket (`/api/fragments/watch`'s `changed`), so the shell
reads the computer again and the warning is shown while there is still
time, with no reload. The shell shows them in a band over the open chat
and Settings, in plain words, the times in the person's own clock:
"Your computer can't save right now. It hasn't saved since 14:05, so what
its agents did after that isn't kept yet. It keeps trying. If it still
can't by 14:35, it stops and starts again from that save." Its chats keep
everything said (their records are the chat's own), so a notice says the
work on the computer after the save is what went.

**Not in the chat.** The chat's records are its agents' and people's: the
platform posts none there (the rule; docs/chat-records.md), and a notice
in it would need a new platform record kind every template is to show.
Every chat is shown in the shell, under its band; a chat opened in a tab
of its own shows the turn a crash cut, as before. Notices in a chat
outside the shell are parked (Paul, 2026-10-08).

**One way back: Restart** (`POST /api/computers/{id}/restart
{generation}`; Settings' "Restart computer", asked once more, and every
notice's Restart). Running, it is a sleep that saves if it can and stops
whether or not it did (a failed save keeps no container: its owner
asked), then a start at once; asleep, or won't wake, a start (lifting
won't wake, as its owner's wake does). Its start is fresh: from the image
and the newest good save, or past one that fails its check an older one,
never the snapshot, which could carry what broke it outside `/data`; no
snapshot is kept until a start comes up. It names the start its owner saw
(`generation`, in the view): a restart of a start already restarted is
nothing, so pressed twice, or in two tabs, it restarts once. It starts a
container, so the ledger is asked first, as for a wake: at zero credit it
is refused, and what runs runs on (decision 27). A restart asked as the
computer starts is that start.

**Does use extend the bound? No: it never stops a computer in use.** A
computer is stopped for its bound only at a sleep, and a sleep is tried
only when nothing holds it: no screen or other port tab open, no keepalive
(a turn running, or waiting on its card), no record or page's presence
within its hold (20 minutes after the last message), and no always-on
plan. While anything holds it the sleep waits, and `stopsAt` moves past
the hold or says none. So what the bound limits is the time a failing
computer is kept, to `unsaved_max_ms` past its first failed sleep: a hard
cap, which is also the cost's. Re-arming the bound after use would keep
an idle container longer for a person who is not there, and could not
save their work: a stop never saves, so a longer window only postpones
the loss unless saves recover by then. So the bound is kept as decision
18 has it.

**The window is free** (Paul, 2026-10-08: the time a computer is kept
for saves the platform failed is the platform's). Its awake time from a
sleep's failed save to the bound is not metered: the lifecycle meters up
to the failure, then passes the window over (`Lifecycle::unmetered`, its
meters at most two intervals, before it and after it), so nothing is
charged and nothing refunded. The window ends early when a save works,
its owner restarts it, or its container goes; what comes after it, and
whatever holds it past the bound (its person using it), is metered as
any.

**The agent.** A turn a restart cut ends as lost, and the agent's next
turn there is told what was cut (docs/bridge.md, "The turn after a cut one
is told"). After a rollback, the turns another life ran since the save its
`/data` came from are in no memory of its runtime: our bridge finds them
(their claims answer 409) and tells the agent's next turn in each chat what
they were, from the chat's journal (docs/bridge.md, "What a rollback
forgot"). The guest's own view also names what its last start restored
(`restored`, `rollback`), for any image to use.

**Getting help.** A deployment may name where a person whose computer will
not start gets help (`support_url`: an `https:` page or a `mailto:`
address; `FRAGMENT_SUPPORT_URL`, given with `GET /api/computers` as
`support`). The shell links it once a restart has failed too; without
one, the notice shows the computer's id and the platform's words, to give
whoever runs it.

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
  removes it with the hold's other files. Our Hermes image says it
  (below); the stub says nothing.
- **What the save leaves out.** The guest's `held` may name, one per
  line, what its save leaves out: gitignore patterns relative to `/data`,
  of letters, digits and `._-/*?[]`, at most 512 of at most 256 bytes, the
  answer at most 64 KiB (`fragment_core::computer::left_out`). They are
  files it copied under the hold to names the save keeps (our Hermes
  image: its SQLite databases, below), so a hot copy of one, which could
  tear (F4), is never what a wake restores. An empty answer leaves nothing
  out; an answer the DO refuses saves the guest whole; a save not held
  leaves nothing out. Our bridge answers for the stub, naming what
  `BRIDGE_HELD_LEAVE_OUT` names (the stub's `*.scratch`, only so the
  platform's lanes can see a save leave something out), and for our Hermes
  image, which answers itself once its bridge has (docs/bridge.md,
  `BRIDGE_HELD`).

### Data and the restore gate

- `/data` is the only directory kept across sleeps. Everything else is
  the image's. The image does not ship `/data`: the restore makes it,
  swapping a restored directory into place, which overlayfs refuses for
  a directory from an image layer (EXDEV); on a first start the image
  makes it itself.
- **The seam** (step 2 of docs/durable-computers.md): `/data/work` is
  what the guest's tools write (a terminal's working directory and home,
  projects, a browser's profile, scratch files), and the rest of `/data` is the
  guest's own state (for ours, Hermes' home: its databases, sessions,
  profiles, config; and the bridge's state). Each save is two
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
  same save). Our Hermes image's puts its
  databases' copies back and runs `PRAGMA quick_check` on each.
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
  again, held no more, and its view's `why` and its owner's notice say so
  (above, "What its owner is told"); its sleep is tried again after the
  same pause, the last try at the bound itself, so the time its owner is
  told is the time it stops. Only after `computers.unsaved_max_ms` (30
  minutes; Paul confirmed it on 2026-10-08) of failed tries does it sleep
  unsaved, its `why` saying so until a save works, and its next wake is a
  rollback. Its owner's restart keeps no container for a failed save.
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
  ended by a crash, or by a sleep that slept unsaved (a restart's
  included), or its start fell back to a save older than that life's
  newest, so what that life did since some save is in none. A start from
  the save its life's sleep took is none. Its owner is told of the newest
  until they say they saw it (`went_back`: "What its owner is told").
  Nothing's correctness depends on the guest reading either.

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
- Our Hermes image's profiles send their image calls to `vision`
  (`auxiliary.vision`: provider `custom`, model `vision`, through the
  intercept), whatever the agent's tier. Hermes describes each
  `computer_use` screenshot with it and hands the main model the words
  (named outright, Hermes routes every capture so: its
  `tools/computer_use/vision_routing.py`), and an image a person attaches
  too. Before, a capture went to the agent's own tier's model, which on
  the medium tier reads no images. DeepSeek Flash's vision build is only
  on DeepSeek's own API (decision 23, its status).
- **Its speech-to-text** goes to `whisper`, whatever the agent's tier.
  - **Config.** Each profile has `stt: {provider: openai, language: "",
    openai: {base_url: <route>/v1, api_key: "agent:<name>", model:
    whisper}}`.
  - **When it runs.** A voice note a person attaches is transcribed
    before its turn, and the agent is given the words. Hermes does this
    under the routed profile's scope for every inbound message, even
    one that answers a question or arrives while the agent works.
  - **No language is forced.** Whisper detects it; Hermes' default,
    `en`, mangles the rest.
  - **No transcript of its own.** The managed overlay's
    `stt.echo_transcripts: false` keeps a memo to one reply a turn, with
    no 🎙️ message before it.
  - **Before (2026-10-07).** Hermes tried local Whisper first: a
    person's first voice note installed faster-whisper and its model
    into `/data`.

  The Docker lane's `a_voice_memo_is_transcribed_through_the_route`
  proves it.
- A call is at most 6 MiB (`fragment_core::models::MODEL_BODY_MAX_BYTES`):
  Hermes shrinks a screenshot (a 1456-pixel long side for a capture) and
  sends it whole; one refused as too large (413) it shrinks to 5 MiB of
  base64 and sends once more (its `_RESIZE_TARGET_BYTES`, which its config
  does not set), which fits. A call reserves its bytes as input tokens,
  so a 5 MiB screenshot holds about $1.25 of its payer's credit (at
  GLM-5.3 Flash's price, the fee and the margin) until it settles at what
  the model counted; a payer with less is refused it (402).
- `POST http://model.fragment.internal/v1/audio/transcriptions`,
  OpenAI's multipart shape with `model` `whisper`: a voice memo
  transcribed (decision 9, its status). It is the platform's
  transcription route as the agent: Workers AI's Whisper, at most 10 MiB
  of audio, metered to the agent's owner at 46.63 neurons a minute of
  audio (docs/api.md, Models).
- **Whose call it is**, one rule for both paths
  (`fragment_core::models::agent_named`):
  - its `x-fragment-agent`; or
  - from a client that sends no header of its own (OpenAI's SDKs take a
    base URL and a key, nothing more), its key: `Authorization: Bearer
    agent:<label>.<username>`. Any other key (Hermes' `fragment-model`)
    is the guest's own placeholder and names no one.

  Named both ways, the two must agree. Refusals:
  - none named, a malformed name, or two that disagree: 401;
  - an agent that does not run on this computer: 403 (the computer signs
    only for its own).

  The guest's auth headers, its key among them, go no further than the
  computer: only the agent's name, the content type and `accept` are
  sent on, and nothing of them reaches the gateway, Workers AI or a log.
- Our Hermes image sets `x-fragment-agent` on every call of an agent's
  profile (its `model.default_headers`), the main model's and the
  auxiliary ones' (titles, the smart-approval guardian, vision). Its
  speech-to-text, whose client takes no header, names the agent by its
  key (each profile's `stt.openai.api_key`, `agent:<name>`).
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

- `images/bridge`: the process both images run between an agent runtime
  and the fragment API, with the chat records of docs/chat-records.md.
  Its core names no runtime; `relay` (Hermes' Relay) and `script` (a
  deterministic agent) are its two runtimes.
- `images/stub`: the bridge with `script`, and a screen page. 9.6 MB; a
  local start follows its chats in 0.3 s. The platform's own lanes run
  against it.
- `images/hermes`: Hermes v0.21.6's desktop image, `hermes-boot`, the
  bridge as Hermes' Relay connector, the screen. The base is pinned by
  digest to the build upstream's `stable-desktop` named on 2026-10-08
  (its versioned tag `rc.4-v0.21.6-desktop`; its install stamp says
  0.21.6, commit `818c13be`); the tag named `v0.21.6-desktop` is an
  earlier attempt's build, not that release. Hermes there is Python
  3.14.7 in a venv its own package manager (PM) builds, its tools (its
  Python, Node 26.7, npm 12, uv, ffmpeg, ripgrep, Chromium 145) in PM's
  store at `/opt/hermes/tools`. One Hermes
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
  (docs/chat-records.md). Then it closes each turn a restart cut in its
  Hermes session (`hermes::CLOSE_CUT_TURNS`): a gateway session whose last
  message has no answer gets the row Hermes itself writes when a turn ends
  without one, its failed-turn boundary, through Hermes' own session code;
  left open, Hermes would join the next message to the cut request and its
  model would do it again (docs/durable-computers.md, P5; the bridge tells
  the next turn what was cut, docs/bridge.md). The managed skills and
  the fragment CLI: below,
  "Skills and the CLI in our Hermes image". An agent fragment's optional
  `agent.json`
  (`{"tier": "cheap"|"medium"|"high"}`) picks its model tier (cheap by
  default; `high` only with `FRAGMENT_HIGH_TIER=on`, decision 23). One the
  platform does not answer for (no 200, 403 or 404) is said
  (`profile.tier_unread`), and the profile's config stays as the last boot
  wrote it, its tier with it; a profile with none yet takes the cheap
  tier (GLM-5.3 Flash, Paul, 2026-10-08). The shell writes `cheap` for
  both the first agent and each new agent.

  Its work (the seam, above): each agent's is `/data/work/<profile>`.
  Its profile's config makes it the terminal's working directory
  (`terminal.cwd`; left unset, Hermes' multiplexed gateway ran every
  agent's commands in the gateway's own home, `/data/hermes`). Its home,
  its terminal's `HOME` and its file tools' `~` (Hermes' profile `home`:
  "Hermes' container guesses", below), is a link to
  `/data/work/<profile>/home`, so what its tools write under `~` (a
  browser's default profile and its databases, a CLI's login) is its
  work, never Hermes' home (Paul, 2026-10-07). A hard cut: a profile's
  `home` that already had something in it is set aside in place as
  `home.before-work` (numbered when taken), unmoved, and the link made;
  nothing is carried over or deleted (event `profile.set_aside`, `what`
  naming it).
  Its temp files are its work too (2026-10-07). Hermes (v0.21.5, and
  v0.21.6 alike) gives each process it runs `TMPDIR`, `TMP` and `TEMP`
  pointing at the scratch
  directory of the home it runs it under
  (`hermes_constants.apply_subprocess_home_env`, then
  `apply_scratch_tmp_env`), an agent's being its profile's
  `cache/scratch`; the image makes that a link to
  `/data/work/<profile>/tmp`, by the same hard cut as its home (set aside
  as `cache/scratch.before-work`). An agent still sees Hermes' path in
  `$TMPDIR`; what it writes there is in its work. A link, and no
  `TMPDIR` of the image's, because:
  - Hermes re-points the temp variables only while they hold a value it
    set itself (its `HERMES_SCRATCH_DIR` marker), and passes any other as
    it is. So a `TMPDIR` in the gateway's environment would reach every
    agent's commands unchanged: one directory for all of them.
  - A profile cannot give its terminal one. `terminal.env_passthrough`
    reads `TMPDIR` from the gateway's process, never from the profile's
    `.env` (Hermes keeps the name process-wide, `agent/secret_scope.py`'s
    `_GLOBAL_ENV_EXACT`). An export in `shell_init_files` reaches the
    terminal's snapshot, so its foreground commands, but not a background
    process: that is a `bash -lic` of the gateway's environment
    (`tools/process_registry.py`).
  - Hermes' own path is what its every way of starting a command reads, so
    the link needs nothing else to agree with it.

  Hermes' own uses of the scratch work through the link: its prune of
  entries idle 24 hours (it compares real paths), and a file it sends from a
  reply's `MEDIA:` tag (any file it can read that its denylist does not
  name: `validate_media_delivery_path`, strict mode off). The gateway's own
  scratch, `/data/hermes/cache/scratch`, stays where Hermes puts it: it is
  the gateway process's (its terminal's session snapshots, its browser
  tool's sockets, execute_code's staging), and it is what Hermes derives
  each profile's from. The image's Chromium opts out of both on purpose: it
  sets its own `TMPDIR`, the container's `/tmp` (below; `hermes.rs`,
  `CHROMIUM_TMP`), so a browser's throwaway profile and shared memory are
  in no save, the work's included. Kanban's "scratch" workspaces are not
  this directory: they are `/data/hermes/kanban/workspaces/<task>`, one
  board for every profile by Hermes' design, and unchanged. One way of
  running misses: an execute_code script's temp files are in the gateway's
  scratch, not its agent's, because Hermes' sandbox drops the marker
  (docs/technical-debt-ledger.md, "An agent's execute_code makes its temp
  files in Hermes' home"). The lower rung's `a_tools_temp_files_are_its_work`
  asks each way: the terminal's `mktemp`, a background process's, and
  execute_code's `tempfile` (pinned in the gateway's scratch), then a temp
  file sent as `MEDIA:`.
  Its desktop browser's profile (`<profile>/bot-desktop/browser-profile`,
  where Hermes keeps it) is a link to `/data/work/<profile>/browser-profile`.

  Its saves (`images/hermes/boot/src/held.rs`; "The hold", above): its
  bridge answers to `/var/lib/fragment-run/bridge-held`, and `hermes-boot`
  answers the platform. Held, it starts no new round of its repo sync, its
  skills install or its agents' reads, and waits (at most 8 s) for those
  under way. Once its bridge has answered it copies every SQLite database
  under `/data` but its work (Hermes' `state.db` of each profile, and
  whatever else Hermes or a plugin keeps: each file named `*.db` that
  begins with SQLite's header) with SQLite's online backup, one step each (one read
  transaction: a copy of one moment whatever writes beside it, never
  restarted), as the hermes user, into `/data/held-copies/<n>.sqlite`,
  and writes its manifest last; then it answers, naming exactly what it
  copied as left out: each database's path and its `-wal`, `-shm` and
  `-journal`, anchored (`/hermes/state.db`, …). A database Hermes makes
  after the copy (its kanban board appears a few seconds after a first
  start) is named by none, so the save keeps it hot rather than losing it;
  so is one whose path no pattern can name. A file named `*.db` that is
  no SQLite database is no database to copy, and the save keeps it as it
  is: the agent's desktop keeps Mesa's shader cache, in its own format,
  as `bot-desktop/xdg/.cache/mesa_shader_cache_db/part<n>/mesa_cache.db`
  under its profile once it has drawn (until 2026-10-06 the copy took it
  for one and failed, so every hold after the desktop's first use went
  unanswered, and a restore's check would have found every such save
  unusable). A database its owner holds locked through the copy's tries
  (1 s: SQLite's exclusive locking mode, in which a running Chromium keeps
  its own, such as `Default/declarative_performance_observer.db`) cannot
  be copied as of one moment while it runs, so
  it is kept hot too, and the rest copied and answered (until 2026-10-06
  it failed the whole copy, so no hold was answered while a browser ran).
  Any other copy that fails answers nothing: the platform then saves
  it whole, not held. Until it answers, `/run/computer/unheld` says what
  it waits on (the bridge, the rounds under way, the copy) or why its
  copy failed. Hermes'
  own `hermes backup --quick` was not used: it copies a fixed list of files
  under one home, and its `_safe_copy_db` copies 256 pages a step with
  0.1 s between, which a busy database restarts. Once the hold goes the
  copies go. Held, Hermes' gateway still writes on its own timers: its
  heartbeat (every 30 s), its status and its cron ticker's stamps (every
  60 s), its logs, and once per changed config its known-good copy of it.
  Each is replaced whole, appended, or read only as a broken config's
  fallback, so the save keeps it as it reads it. Its model-catalog
  refresh, the one such writer that fetched from outside, is off
  (`model_catalog.enabled: false` in the managed overlay). Node's compile
  cache, which npm put in Hermes' home (its `TMPDIR`), is under /tmp
  (`NODE_COMPILE_CACHE`). docs/durable-computers.md, "What changes under
  the hold", has each writer and why. At a start, before the gateway
  opens a database, the copies go back over their live paths (each one's
  `-wal`, `-shm` and `-journal` removed, its owner and mode as they were): after a restore its
  `computer-check` does it, then `quick_check`s every database but its work's as the
  hermes user and exits 3 on one that fails; after a snapshot, `pre-init`
  does it, so both wakes leave the same `/data`.

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
  no Hermes could be winding down a turn in it.

  Each agent has a desktop of its own: Hermes v0.21.5 gives every profile
  its Bot Desktop (its own Xvnc, browser profile, lease and activity
  file, under `<profile>/bot-desktop/`), and each agent's screen is its
  own (`images/hermes/boot/src/desktop.rs`). `hermes-boot` names each
  agent's display, lease and activity file in the bridge's screens file
  (`/var/lib/fragment-run/screens.json`, written with the ready file:
  docs/bridge.md), so a socket that names an agent is that agent's
  desktop, and one that names an agent not on this computer is refused.
  A desktop starts for its screen's first viewer (`hermes-boot
  screen-start <agent>`, which starts only an agent the ready file names:
  about a second on the lower rung, and about 220 MiB more while it runs,
  one Xvnc and Xfce per agent), or at the agent's first `computer_use` or
  browser call (`bot_desktop.auto_start`), never at boot. Hermes refuses
  to start one below 1.5 GB of free memory (`bot_desktop.min_free_memory_mb`),
  which a 6 GiB computer reaches with a few agents' desktops, each with a
  browser open.

  Take over is the agent's own Bot Desktop lease (`lease.json`), which
  Hermes' `computer_use` and browser tools read before every action: while
  a person holds it they refuse (`human_has_control`, captures included),
  and an action during which it changed hands is voided. The bridge
  writes it as Hermes does, under its `lease.lock` flock with its epoch
  bumped; Give back, or the taker's page leaving, gives it back to the
  agent; a change by Hermes (its `screen stop --force`) reaches the
  viewers within a quarter second, and the bridge's own input gate reads
  the lease at each input. A lease a person held when the bridge last
  stopped is given back as the next one starts: no viewer survives a
  restart. Hermes' own lease RPCs (`display.lease.*`) are its TUI
  gateway's, which this image does not run.

  A desktop no one uses is stopped: up and unused for 10 minutes
  (`desktop::IDLE_STOP_MS`: no `computer_use` action, no browser command
  on it, no take over, and no one watching it, since the bridge touches
  its activity file every 10 s while someone does), `hermes-boot` stops it
  with Hermes' own `computer-use screen stop`, which refuses while a
  person holds its lease (event `screen.idle_stopped`). Hermes stops an
  idle desktop itself only from its TUI's gateway, which this image does
  not run; the managed config names the same bound
  (`bot_desktop.idle_stop_minutes`). Its next use starts it again, and
  what it kept is there: its browser's profile is in the agent's work.

  All of a computer's agents run as one user, the hermes user (decision
  44: a person's agents are not fenced from each other), so nothing stops
  one agent from driving another's display: the screen, the lease and
  each agent's own tools keep each on its own desktop, and the platform
  skill tells every agent never to touch another's.

  The screen's page names the agent it shows (its `?agent=`, then the
  name the control socket says) and follows the desktop: a stream that
  ends (the desktop stopped or restarted, its viewers' streams ending with
  it) is opened again on its own, from 1 s backing off to 10 s, while the
  page's control socket stays open, and the bridge starts the desktop for
  it at once; who holds it is the lease's, as the control socket says. On
  that desktop an agent operates: Hermes'
  `computer_use` (its backend, cua-driver 0.28.3, is in the image, pinned, and named by
  `HERMES_CUA_DRIVER_CMD`; Hermes lists the tool in its `tool_search`
  bridge and the agent calls it through `tool_call`; each screenshot is
  described by the route's vision model: Models), and its built-in
  browser tools, headed there (`browser: {headed: true, backend: off}` in
  each profile's own config, the only place Hermes reads `browser` from;
  with no backend named, Hermes would offer Browser Use's one
  `browser_exec` tool instead), which drive `agent-browser` (Hermes'
  image has none: the image installs the one Hermes' lock pins, through
  Hermes' own PM, on PATH). Its browser, and the desktop's Browser icon a
  person uses after Take over, are the image's Chromium
  (`/opt/fragment/bin/chromium`, named to Hermes by
  `AGENT_BROWSER_EXECUTABLE_PATH`): the full Chromium Hermes' image pins
  (its PM's, named in `/etc/hermes/agent-browser-executable-path`), always
  started with `--no-sandbox --disable-dev-shm-usage`, its scratch
  (`TMPDIR`: a headless one's temporary profile, and its shared memory)
  the container's `/tmp`, never Hermes' home's `cache/scratch` under
  `/data`. Hermes adds those flags itself only
  where it sees Docker's marker (`/.dockerenv`), and Containers gives a
  container neither that marker nor a `/dev/shm` (Docker mounts one in
  every container), so there Chromium died as it started (p5,
  2026-10-05); the lower rung runs the desktop as Containers does
  (`--ipc=none`, no marker). Events: `agents.changed`,
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
  and the rest installs. A person without one (setup makes it since
  2026-10-03) adds it from settings ("Add the managed skills", from the
  blessed template, as setup makes it); their awake computers install it
  within the minute.
- **The platform skill**, `fragment`, is every profile's, whatever the
  skills fragment holds: what an agent knows of the platform it is on. It
  is the image's own `fragment` CLI's skill (`fragment skill`, cli/SKILL.md:
  what a fragment is, the commands for the agent's fragments) after a page
  of what the computer adds (`images/hermes/boot/src/computer.md`: that it
  is an agent on a Fragment computer acting for its owner with no login,
  the apps and brain skills to load, its connections as placeholders in
  its environment, `GOOGLE_OAUTH_ACCESS_TOKEN` and the Google Workspace
  skill, and its own desktop, which its owner watches and can take over
  from "Its screen", its computer-use and browser tools answering
  `human_has_control` meanwhile, and never another agent's), with a
  description for Hermes' skills index.
  `hermes-boot build-info` writes it at the image's build
  (`/opt/fragment/skills/platform/fragment/SKILL.md`, read-only to the
  agents), so it is always the binary's in the image; the build fails if
  `fragment skill` is no skill named `fragment`. A missing `fragment
  skill` instruction belongs in cli/SKILL.md. The profiles find it in its
  view, `/var/lib/fragment-run/platform-skills` (the boot's, read-only to
  the agents, never saved): a copy `hermes-boot` makes at each start and
  after each install of the managed set, unless a managed skill takes its
  name (`fragment`), when it leaves the view (`skills.installed`'s
  `platform`).
- **Every profile** names the managed directory and the platform skill's
  view in `skills.external_dirs`, below its own `skills/` (its agent
  fragment's, synced both ways: an agent's own skills are versioned in its
  fragment). Hermes ranks a profile's own skills above its external dirs,
  so an agent's own wins over a managed one or the platform's; its
  external dirs are one rank, in which two skills of one name are
  ambiguous and Hermes finds neither by it (since v0.21.6; v0.21.5 took
  the first dir's), so a managed `fragment` wins over the
  platform skill by the platform skill leaving the view. An agent's profile has
  its own, the managed set, which is what the shell's Skills section
  lists, and the platform skill. The image carries none of Hermes' bundled
  skills: Hermes copies them only into the home its sync runs in, the
  gateway's default profile, which runs no turns (and stage2 and the
  gateway then sync nothing at a boot). A managed skill a session has not
  yet seen appears at its next session.
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

### Root in our Hermes image

Paul, 2026-10-05: "hermes needs to be able to install binaries (not
persisted)".

- **Passwordless sudo.** Hermes runs as its unprivileged user, as
  upstream runs it, and its user may run anything as root with `sudo`
  (no password, so Hermes' terminal runs `sudo` as written, never asking
  for one). The system's directories stay root's, as on a CI runner, so
  `sudo apt-get install`, `sudo install … /usr/local/bin/` and `sudo npm
  install -g` are how an agent installs software. The computer is one
  person's VM with no secret in it (decisions 13, 43), and one person's
  agents are not fenced from each other (decision 44), so root inside it
  opens nothing of anyone else's.
- **How long an install lasts.** `/data` is the only place a computer
  keeps; an install outside it lasts until the computer's next start from
  its image (a crash, or an image update), and a wake from a snapshot
  keeps it.
- **Hermes' own settings.** Its file tools (`write_file`, `patch`) may
  write its home, its agents' work directories (`/data/work`, where its
  terminal works) and `/tmp` (`HERMES_WRITE_SAFE_ROOT`, which binds
  only them, not the terminal: defense in depth, as Hermes says), for the
  scratch an install is made from; they run as its user, so `/usr/local`
  is not theirs. Lazy installs are off (upstream's image turns them on;
  `security.allow_lazy_installs: false` in the managed overlay): they are
  Hermes' own optional backends (providers, platforms, speech), which a
  computer configures none of. What its agents use is in the image:
  Edge's speech SDK, `text_to_speech`'s default provider, which Hermes'
  image leaves to a first-use install (without it, with installs off,
  Hermes offers no `text_to_speech`), is installed at build at its
  `uv.lock` pins; local Whisper, a voice note's fallback, is never
  installed. Nothing is installed under `/data` at run time (the Docker
  lane's `nothing_is_installed_at_run_time`). npm's global prefix is `/usr/local`
  (`npm_config_prefix`, which sudo keeps): npm's own, since Hermes' PM
  ships Node, is its Node's directory in Hermes' tool store, on no PATH.
- **The network.** apt reaches `deb.debian.org` over plain HTTP, which no
  intercept catches (decision 43); the image keeps apt's lists as of its
  build, and a `.deb` on disk installs offline. An intranet computer
  needs an apt mirror (and PyPI's and npm's) named in the image.

### Hermes' container guesses

Hermes v0.21.5 does some things differently when it believes it runs in
a container, and it guesses: `is_container()`
(`hermes_platform/host/runtime.py`, once per process) is true on
`/.dockerenv`, `/run/.containerenv`, `KUBERNETES_SERVICE_HOST`, a
runtime's name in `/proc/1/cgroup`, or `kubepods`, `containerd` or `crio`
in the root mount; a few places read `/.dockerenv` (or `docker` in
`/proc/1/cgroup`) alone. Docker gives every container `/.dockerenv`, so
the lower rung always reads as a container; Containers likely gives none
of these (only p5 can confirm), and there Hermes would take the computer
for a host. The image fakes no marker: it pins every guess its paths
reach (the gateway, an agent's terminal and file tools, its browser and
desktop) to what Hermes does in a container, as the Docker rung has
always run it, in the environment `hermes-boot pre-init` gives everything
after it (`RUNTIME_ENV`, `images/hermes/boot/src/hermes.rs`).

| Guess | What it decides | Pinned |
|---|---|---|
| `get_subprocess_home` (`TERMINAL_HOME_MODE`, `auto` by default) | `HOME` for an agent's terminal and `execute_code`, its file tools' `~`, a skill's paths, the write guard's homes: in a container its profile's `home` (`/data/hermes/profiles/<profile>/home`, a link to its home in its work, above); on a host the gateway's own, `/data/hermes`, one for every agent | `TERMINAL_HOME_MODE=profile`. Only the environment counts: a multiplexed gateway reads no profile's `terminal.home_mode` |
| `apply_secure_dir_policy`, `_secure_file` | on a host, Hermes' home made owner-only (0700, 0600) at each start; in a container left as made | `HERMES_SKIP_CHMOD=1` |
| `browser_tool_install._running_in_docker` | Chromium's `--no-sandbox --disable-dev-shm-usage`; Chromium's auto-install | the image's Chromium (above): always those flags, and never missing |

The rest are not reached, or only say something: the install method (the
image's `docker` stamp in `/opt/hermes` is read first), NixOS container
mode (no `.container-mode`), `/restart`'s exit for a supervisor (the bridge
passes no slash command), the startup audit's log line about the home's
mount, the CLI's service, status, setup and doctor commands, CLI voice, the
dashboard (off) and OTLP export (off). A skill tagged `environments:
[docker]` would be offered only on Docker; none of Hermes', ours or the
templates' is. The lower rung's `an_agents_home_is_the_same_on_either_runtime`
runs the image both ways (`Runtime::Hosted`: no marker, which Hermes takes
for a host) and finds the same `HOME`, `~` and modes.

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
  charged. Nor is the time a computer is kept for its failed saves: from
  a sleep's failed save to `computers.unsaved_max_ms` after it, or to the
  save that works first (Paul, 2026-10-08; "What its owner is told").
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
  crash or the bounded failure put it there. What its owner is told
  (`notices`): saves failing told from the second awake failure and a
  sleep's first, with the stop at the bound itself, later while a record
  holds it, never while in use or always on; a start that went back told
  with its cause and save, pending while asleep, once until seen, a seen
  older one hiding only itself, across a restart of the DO; won't wake
  told until a restart. The restart: a sleep that saves then a start,
  held as an owner's wake, never cancelled by a keepalive, asked twice
  made once, an earlier start's nothing, an owner's sleep after it
  winning; its failed save stopping unsaved and told as the restart's;
  its start fresh from the image, no snapshot kept until one comes up.
  The simulation restarts too, and checks every notice against the state
  (a loss told is unseen and a life it had; never a stop before the bound),
  and that no meter falls in a free window. The free window: nothing
  metered from a sleep's failed save to its bound, before and after it as
  any, a late report or an alarm again metering nothing twice, the same
  across a restart of the DO.
- The e2e on workerd: the Computer DO's routes and its intercepts,
  against `images/stub/` under `wrangler dev` with Docker. A crash is the
  lever's (`POST /api/test/computer {computer, op: "kill"}`: SIGKILL to
  the guest's PID 1, so the real exit is reported), and wakes from the
  save its last turn's end took; a failed save is the lever's
  (`fail-saves`), and so is an always-on plan (`always-on`). A check of
  what ran counts runs (the model fake's calls, the ledger's rows, the
  computer's `uses`), never records, which a second run replays. The
  e2e's bound is 45 s (`crate::UNSAVED_MAX_MS`), so the `computers`
  lane's recovery checks see it run out: a failed sleep told at once with
  its stop time, a restart asked twice (its save failing too) going back
  once and saying so until seen, the unsaved stop at the time it said and
  the wake after told once, a restart of a sleeping computer, one
  refused at zero credit, and no awake time on its owner's ledger in
  either free window. The `shell-ui` lane sees the same in Chrome:
  the warning on Settings with no reload, its Restart, the notice of what
  it went back to and its OK (gone after a reload), and Settings' Restart
  computer. On a preview, the hosted `agent-restart` section (by name)
  restarts a real Hermes computer awake after a reply: it comes back
  running from its newest save, the restart's own and held, from the image
  and not a snapshot, with no rollback; the same press again restarts
  nothing; and its agent answers after it, nothing left to tell its owner.
- The e2e's `wipe` section (crates/e2e/src/lanes/wipe.rs): a computer
  whose `/data` holds a file its agent wrote and a save, wiped with its
  owner (its first step alone, then across a node's crash): no computer
  from that step, its saves gone, its owner's next identity's computer a
  new one that restores nothing and reads no such file; hosted, the same
  on the deployment's own image.
- The real-Hermes lane: `images/hermes/` with a scripted model (phase
  4's exit list), a second agent assigned to the awake computer while the
  first's turn runs included, and an install as root (a `.deb` through
  apt and a program into `/usr/local/bin`, offline), its home intact
  after a sleep and a wake; and that agent (on the medium tier) looking
  at its screen: its `computer_use` screenshot goes to the route's
  `vision` as that agent, settled on its owner's ledger at GLM-5.3
  Flash's price, and its answer is what the vision model saw. The
  Workers AI fake reads images only on a model the catalog marks
  "Vision: Yes" (GLM-5.3 Flash) and answers 400 for one sent to another,
  as the ledger lane checks with `vision` itself and a call of Hermes'
  shrunk-screenshot size.
- The images' own (`images/`, its own workspace: `cargo test` and
  `cargo clippy --all-targets -- -D warnings` there): the bridge's engine,
  pure; the bridge against an in-process fake fragment API, with the
  `script` runtime and with the `relay` runtime against a scripted
  Hermes gateway; and, with Docker, both images built and run against
  the fake API and a scripted model on the host
  (`cargo test -p fragment-bridge --test docker -- --ignored`), real
  Hermes included. CI runs the Docker ones (images.yml's `docker`) on
  pull requests and master's pushes that touch images/hermes,
  images/bridge or images/stub, the rest on every change to `images/`.
  These are lower rung: fakes at the platform's edge.
  Among them, saves of our Hermes image taken as the DO takes them
  (`a_save_taken_while_it_writes_opens`: during turns whose tool writes a
  SQLite database every few ms, half under the hold and half hot, each
  restored into a fresh container, its `computer-check` run, every SQLite
  file then `quick_check`ed; and `held_nothing_under_data_changes`: once
  held, no file the save keeps changes). Measured 2026-10-05: none of 54
  held databases tore, and none of 54 hot ones either (a WAL database's
  hot copy rarely tears; the hold makes it never). Hermes itself writes
  while held, which is why a save names what it copied: its kanban
  dispatcher makes its board's database some seconds after a first start,
  and opens it on a timer.
  The screens: in process against fake displays (`tests/screen.rs`:
  each agent's socket is its own display, and an agent not on the
  computer, one the image names no screen for, or no name, is refused;
  Take over writes the agent's lease as Hermes does, the input gate
  follows it whoever changes it, and a person's lease from the bridge's
  last life is given back at its start); and with Docker
  (`two_agents_two_desktops`): two agents, two desktops, each served by
  its agent; juniper's taken over, its lease reads `human` to Hermes' own
  code and its `computer_use` answers `human_has_control` while fred's
  captures its own; given back, juniper's works again; an unwatched,
  unused desktop stops after the bound (30 s there:
  `HERMES_BOOT_SCREEN_IDLE_MS`), a watched one does not, and the next
  viewer starts it again with its browser's profile kept.
- The platform's side, with the stub: the `computers` lane's ticket that
  lands on `?agent=` and refuses a path off its port, and an agent's
  screen's control socket through the port (refused for an agent not on
  the computer, 404, and for no name, 400: the image's answers); the
  `shell-ui` lane's chat menus (a direct chat's "Its screen", a group's
  screen per agent), each frame landing on `?agent=<that agent>`. The
  hosted `agent-smoke` checks that its chat's agent's screen is the one
  served: the RFB stream's desktop is that agent's (`hermes:<profile>`),
  the page names it, and another agent's is refused.
