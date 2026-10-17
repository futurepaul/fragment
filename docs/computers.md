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
- A computer's agents follow their fragments: an agent fragment deleted
  leaves its computer (its delete's cleanup, retried until done:
  cell/src/ended.rs), as an unassign does: the guest is given it no
  more, its owner's pages are told, and it is counted in the computer's
  cap (16 agents) no more. The registry names its agent by the fragment
  no more; its identity stays, its key held by no one (that life's
  alone). A life made again under the name is a new agent, assigned
  anew.
- Agents are fragments of kind `agent` that name the computer that runs
  them (decision 14). An agent's identity is its own: its agent
  fragment's npub (decision 45), and its owner is a person. The computer's guest
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
  Each of its steps is bounded (below, "Bounds"): a save whose backups
  never answer (a sick container's, r4 and r6) fails at its bound as one
  that says so does, so it too is kept, tried again and ended by the
  bound; and a save that never reports (its isolate gone with it) failed
  at the lifecycle's deadline for it (`SAVE_DEADLINE_MS`, 10 minutes).
  A try under way as the bound passes ends within its own bounds first.
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
  the save in the same wake, with no strike against it. So does one that
  comes up dead (#362): a container Cloudflare restores from a snapshot
  can take the DO's execs for a second and then answer nothing, its guest
  never heard from, while its start looked fine. A start from a snapshot
  is up only once its guest has read its own view (`GET /api/computer`,
  which an image reads as it starts: below, "The guest's contract")
  within 45 s (`ALIVE_MS`: a healthy guest reads it within 2 s; one whose
  first tries came before its intercepts were set waits out its backoff);
  one unheard by then is dead, its snapshot forgotten and its container
  destroyed, logged as `{snapshot: "dead"}`, and the same wake starts the
  image and the save. A start from the image is not waited for so. A start whose
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
| **A sleep whose save fails** | any sleep; it is kept awake (I5) and tried again after 1, 2, 4, 8, 15 minutes | Settings' `why`, read only when Settings was opened after it / nothing; its owner charged for the time it was kept | an `unsaved` notice at once, over every chat and Settings, pushed to open pages: since when its work is in no save, the save a stop goes back to, the time the bound stops it, and Restart; the time it is kept, from its failing try's start to the bound, is free |
| **Awake saves that keep failing** (a turn's end, every 15 minutes busy) | tried again after a growing pause, unbounded while awake | nothing anywhere: no `why` for an awake save | the `unsaved` notice once two fail in a row (no stop time: none is due until it would sleep) |
| **The bound runs out**: put to sleep unsaved | the sleep tried at `unsaved_since + unsaved_max_ms` (exactly: the last try is the bound's), only when nothing holds it | Settings' `why` ("slept unsaved"), which then stayed for good (only a save of the same life cleared it) / nothing | a `went_back` notice (pending) while it is asleep: what it will go back to; the `why` goes at the next save that works |
| **A crash**: the container stops on its own (out of memory, a host restart, the runtime's idle stop, or dying while its sleep held or saved it) | `Exited`; it starts again at once while anything wants it, else at its next wake | nothing (`restored` and `rollbacks` were in the API only) / a turn it cut ends quietly as "lost when the computer restarted"; the agent's next turn there is told what was cut (docs/bridge.md) | a `went_back` notice: stopped unexpectedly, back to its save of `HH:MM`, how much before it stopped; the chat and the agent as before |
| **A wake past an unusable save** (its archive gone, or the image's check failed) | `RestoreFailed`, the save before it at once | nothing | a `went_back` notice: the newest save could not be restored |
| **Its owner's restart** | `POST …/restart` | (no restart) | saved first: nothing to tell; its save failing, a `went_back` notice for the restart |
| **A wake whose snapshot will not start** | the image and the same save, in the same wake | nothing | nothing: nothing is lost |
| **A wake from a snapshot that comes up dead** (#362: started, then no exec answered and its guest never read its view) | unheard 45 s after its start; the image and the same save, in the same wake | it was awake and answered no one: messages unanswered, and its sleep's save hung for good (r4, r6) | nothing: nothing is lost, its wake 45 s slower |
| **A sleep whose backups never answer** (a sick container's) | its save fails at its bound (8 minutes), as one that fails | it stayed `sleeping`, held, answering no one, and owed as awake, for good; its owner's sleep never answered (r4, r6) | as a sleep whose save fails: kept, the `unsaved` notice, tried again, and the bound ends it |
| **Won't wake**: three failed starts in a row, three crashes, or no save left to fall back to | `Phase::Failed` | Settings' "State: wont wake" and the platform's words / messages go unanswered, nothing said; and the shell's own presence wake (an owner's) silently tried it again every minute the person was there | a `wont_wake` notice in plain words, the platform's beside them, and Restart; the shell no longer wakes it on presence; after a restart that fails again, "still won't start", and the deployment's support link, or the details to give whoever runs it |
| **A slow or failed start** (fewer than three) | tried again in 5, 10 s | first run's "taking longer than usual" / nothing | the same (passing) |
| **At zero credit, or a guest**: no wake starts | the ledger refuses (402, 403) | Settings' credit warning and `why` / unanswered | the same; the refusal's `why` now goes once a start comes up (it stayed for good); a restart is refused the same way |
| **A deploy that changes its image** (the common update: the same name, a new image) | the deploy pushes it; awake, it keeps the image it started on until its next sleep or a restart | nothing: it ran the old image until it slept, and its owner restarted it to be sure | an `update` notice while it is awake and no turn runs: an update is ready, Restart (about a minute; its chats and work kept), or Later; nothing while it sleeps or is asleep, since its next start takes it. Settings shows the version it runs and the latest |
| **A pin to another image** (a canary, a rollback: below, "Stable and canary") | its owner's pin, or an operator's | the update pill (pinned to the default, then a sleep and a wake) | the `update` notice while it is awake on the old image; nothing asleep, since its next start takes it (the pill went with the renamed default it served) |

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
- `update {version}`: it is awake on another version than its next start
  runs (`version`, the view's `latest`; the view's `version` is the one it
  runs), and no turn runs (no keepalive open). Told until a restart or a
  sleep takes it, or its owner puts that version off (`POST
  …/notices/later {version}`); another version is told again.

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

**An update is told, never forced** (Paul, 2026-10-09: "should it be a
notification in the shell? (new update, click to restart). that might be
simpler than trying to ninja restart people"). A deploy that changes the
computer image pushes it under the same name (`hermes`), so the name a
computer is pinned to stands for the new image from then on. Asleep, a
computer takes it at its next wake (decision 19); awake, it runs the image
it started on until it sleeps, which one in use, or always on, may not do
for days.

- **Where the version comes from.** The runtime's own reference for the
  pinned name (entry.mjs's `imageRef`: `ctx.container.images`, from the
  Worker version's container config; a deployed image is the digest the
  deploy pushed, `…@sha256:<hex>`): exactly what the computer's next start
  runs. Beside it, the reference its running start came up on, which the
  Computer DO already kept (the image a snapshot is of). A version is the
  digest's first 12 hex digits (`fragment_core::computer::version_of`; a
  reference with no digest, as `wrangler dev` tags its builds, the first
  12 of its SHA-256). One image is one version: a deploy that rebuilt
  nothing tells nothing, where a deploy id would after every deploy.
- **When it is noticed.** The DO looks at the reference at every event
  (its alarm, which fires at least every five minutes awake; a keepalive;
  a wake) and every read of its view (the shell as it opens, and each of
  its owner's routes), and records it (`Images`). A deploy restarts every
  Durable Object, so its next touch looks, finds the new reference, and
  tells its owner's open pages as any change of what they are told is told
  (`/api/fragments/watch`'s `changed`): no reload. A start runs what it
  looked at and records it, so a restart, or a sleep and a wake, takes the
  update and the notice goes.
- **Never pushy.** Not while it sleeps, is asleep, or starts: its next
  start takes the update anyway, with nothing for its owner to do. Not
  while a turn runs (its guest holds the keepalive while a turn is queued,
  runs, or waits on its card): a restart would cut it; it is told as the
  turn ends. Never restarted for it: the restart is the person's, the
  notice's Restart, guarded by its generation as every notice's is. Later
  puts that version off, kept by the DO, so every page and every reload
  agrees; a version other than it (the next deploy's) is told again. An
  always-on computer never sleeps, so its notice does not say it updates
  by itself.
- **What the person sees**, over every chat and Settings: "An update is
  ready for your computer. Restart to get it — about a minute; your chats
  and work are kept. It also updates by itself the next time your
  computer sleeps." with Restart and Later. Settings' Computer shows the
  version it runs (`9f86d081884c`; asleep, the one it starts on)
  and, running an older one, the latest.

A default is never renamed (a promotion moves the name: below, "Stable
and canary"), so the shell's update pill, which pinned a computer to a
renamed default, went (2026-10-13): pinned to a canary, its person would
have been offered stable as an update.

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
for saves the platform failed is the platform's, the whole failing try
included: the coordinator, 2026-10-16). Its awake time is not metered
from the start of the try whose save failed first (its sleep's hold:
a save that hung to its bound, below, "Bounds", was the platform's for
all its minutes) to the bound, which counts from that failure as its
owner is told; nor, past the bound, the try under way at it, to that
try's failure. The lifecycle meters up to the try's start, then passes
the window over (`Lifecycle::unmetered`, its meters at most two
intervals, before it and after it), so nothing is charged and nothing
refunded. The window ends early when a save works (that try's own time
free, metering again from its end), its owner restarts it (free to its
restart's save's end), or its container goes; what comes after it, the
stop's seconds after its last failed try, and whatever holds it past the
bound (its person using it) and the try begun after that hold, are
metered as any.

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

## Stable and canary

A new image reaches a few people first, and everyone after it has proved
itself (decision 19: a canary is a per-computer pin; Paul, 2026-10-13:
"a few rounds of successful upgrades to the hermes image so we know
that's seamless for users (including a canary path)"). A deployment's
config names two images (xtask/src/deploy.rs, `computers.images`):

```jsonc
"default_image": "hermes",
"images": {
  "hermes":        { "image": "registry.cloudflare.com/<account_id>/<repository>@sha256:<64 hex>" },
  "hermes-canary": { "dockerfile": "images/hermes/Dockerfile", "build_context": "." }
}
```

- **Stable** is the default, kept at a digest: wrangler builds nothing
  for it, prepares the image the account's registry already holds, and
  refuses the deploy when it does not hold it; the deploy refuses, before
  it builds anything, a reference that is no digest in its own account's
  registry. It changes only when its line does, so a deploy tells the
  computers on it nothing. New computers start on it.
- **The canary** is built from the tree at every deploy (on the preview,
  master's). A computer pinned to it is on the canary: every deploy that
  changes its image tells it an update is ready, as any deploy that
  changes an image tells (above).
- **What changes a version** (Paul, 2026-10-14: "changes to the cells, or
  frontend, that don't change the hermes image shouldn't require a hermes
  image rollout"). The image's inputs are what it runs, and no more:
  `images/` (the bridge, hermes-boot, the image's own files, its
  Dockerfile), the Hermes base by its digest, and the fragment CLI's build:
  `cli/` (its code, GUIDE.md and SKILL.md), the crates it builds from
  (`crates/core`, `nip98`, `proto`, `templates`), the templates it
  scaffolds (`blank`, `calories`, `doc`, `inbox`, `notes`, `todo`), and the root
  manifest and lock. A change to anything else (the cell, the shell, the
  chat page and the other blessed templates, the e2e, xtask, the docs)
  leaves every build step cached, so the canary's digest, and its
  version, stay as they were, and no one is told of an update. The CLI
  builds without the platform's own files (crates/templates' `platform`
  feature: the blessed templates and the shell's), which no one
  scaffolds, and the image's build copies the workspace's other members as
  manifests alone (xtask's `the_cli_stage_copies_what_the_cli_builds_from`
  holds the Dockerfile to cargo's account of what the CLI builds from). A
  change to a crate the CLI shares with the cell (`crates/core`, `proto`)
  rebuilds the CLI; when its binary comes out the same bytes, the build's
  cache, which goes by content, keeps the digest too. Two limits: a
  deploying machine that lost its build cache rebuilds every layer, each
  stamped anew, and pushes a new digest (and the whole base); and a change
  to the CLI is a change to the image, whatever it is for.
- **Who is on it** is the operators' to say, a person at a time: `fragment
  operator pin <npub> hermes-canary` (`PUT
  /api/admin/people/{npub}/computer/image`, logged as `pin`; docs/api.md,
  Operators) pins their computer as its owner's pin does. The pin is the
  one record of what a computer runs, so there is no list beside it; the
  group (the finite team, by npub, an email found with `fragment operator
  people --q`) is the operators', outside the repo.
- **Where a digest comes from.** A deploy names each image's reference as
  wrangler prepares it (`hermes-canary is ready to run`, then
  `registry.cloudflare.com/…@sha256:…`), and an operator's view of a person
  (`fragment operator person <npub>`) names the whole reference their
  computer runs and the one its next start runs (`references.running`,
  `references.next`); the version Settings shows is a digest's first 12
  hex digits.
- **Promotion**: the canary's digest becomes stable's line, and a deploy.
  Every computer on stable then runs another version than the one its
  next start runs: asleep, its next wake takes it (from the image and its
  save: the snapshot is of the old image), paying the new image's pull;
  awake, its owner is told an update is ready (Restart, or Later, or its
  next sleep); always on, only told. The canary's computers are told
  nothing: they run that digest already.
- **Rollback**: a canary computer is pinned back to `hermes` (`fragment
  operator pin <npub> hermes`), told as any pin is (the `update` notice;
  "an update" is another version, older or newer). Stable goes back by
  its previous line, and a deploy. Keep the line a promotion replaced,
  commented, for that.
- **A name stays.** A computer pinned to a name the deployment no longer
  has has no image to start. `hermes-canary` is kept: between rounds its
  digest is stable's, or master's ahead of it.
- **The cell is ahead of stable.** The cell deploys from the tree at every
  deploy; stable lags by a round. An awake computer already runs its old
  image across a deploy, for days if always on, but a round makes that
  the rule: a change to what the guest relies on (this contract, the
  bridge's routes, the CLI's) is additive, and what it replaces goes only
  once stable has moved past it.
- **A Hermes release is forward only** (Paul, 2026-10-13). A round that
  changes only our layers (the bridge, hermes-boot, the CLI and the
  image's own files) on the same Hermes base can be pinned back: the
  older image runs on a `/data` the newer wrote, files our code reads
  either way. A round that moves the base to a new Hermes release (v0.21.6
  to v0.21.7 is the first) cannot: a new release may move its databases'
  schema forward, and a computer keeps three saves, each turn's end
  replacing the oldest, so within a day no save from before the canary is
  left (decision 19's point-in-time restore has none to go back to):
  pinned back, an older Hermes would start on a newer schema. So a bad
  Hermes release is fixed by the next one, until a computer keeps the save
  an image change started from (docs/technical-debt-ledger.md). v0.21.7
  happens to move none (`state.db` is schema 31 and the config version 50
  in both), but the rule does not read each release's schema.
- **Production** keeps its images in its own account's registry, which
  holds none of the preview's digests (the deploy refuses another
  account's). Its first deploy builds `hermes` from the release commit;
  its line is then pinned to the digest that deploy names, and its own
  canary group goes ahead of it as the preview's does.

### One round

On the preview (`preview.finite.build`, master deployed continuously),
with the operators' key (`--key-file`, or `FRAGMENT_OPERATOR_KEY_FILE`)
and `FRAGMENT_HOST` naming it. Once: stable's line is the digest its
computers run (`references.running` of one awake on it), the canary's is
the build, and the canary group is pinned to `hermes-canary`.

1. **The change lands** on master; its deploy rebuilds the canary. Its
   version: `fragment operator person <npub> --json | jq -r .computer.latest`
   for one of the group.
2. **The group takes it**: awake, each is told an update is ready and
   presses Restart (the path under test); asleep, its next wake takes it.
   Each checks, on their own computer: an old chat goes on where it was;
   a new chat recalls a memory; their own skills are listed and used; a
   routine fires after the update; the screen opens and Take over works;
   Google answers (Gmail or Calendar); a Bot Chat goes on; Settings shows
   the canary's version and no notice of going back. These are a lived-in
   computer's, which no hosted run has: its people are new.
3. **The hosted runs** (`cargo xtask e2e --hosted --config <the preview's
   config> --branch preview`), each followed by `--sweep`: the candidate,
   `--candidate hermes-canary --only
   computers,agent-restart,agent-smoke,agent-ask,bot-mode --max-paid-calls
   190` (new computers on it: a routine, the CLI and managed skills, an
   app, the screen, an approval, saves held across a sleep and a wake,
   agents asking each other, Bot Mode; and agent-restart's upgrade from
   stable into it, the chat going on, and its rollback); then stable under
   the new cell, `--only computers,agent-restart --max-paid-calls 50`.
4. **Watch** each canary computer (`fragment operator person <npub>`):
   `computer.version == computer.latest`, `rollbacks` unchanged, every
   save `held`, and no `wont_wake`, `unsaved` or `went_back` notice (a
   `went_back` of cause `unusable` is the new image refusing the old
   save); `fragment operator health`; the Computer DO's lines in the
   Workers logs (a start that failed, `restored`); the ledger for spend
   out of the ordinary.
5. **The soak** (Paul, 2026-10-13): 24 hours, every canary computer woken
   on the canary and having answered a turn on it, and the hosted runs
   green.
6. **Promote**: the reference a watched canary computer runs
   (`fragment operator person <npub> --json | jq -r .references.running`,
   its first 12 hex digits the version step 1 named) becomes `hermes`'s
   `image` in the preview's config, the old line kept commented; deploy.
   One computer on stable awake is told; one asleep wakes onto it.
7. **Or roll back**: `fragment operator pin <npub> hermes` for each of the
   group (a Hermes release is forward only, above); after a promotion,
   the commented line back, and a deploy.

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

As its entrypoint starts, an image reads its own view (`GET
/api/computer`, below, "The fragment API"), as our bridge and our Hermes
image's boot do, retrying at least every 30 s until it answers: that read
is how the DO knows a start from a snapshot came up alive (above,
Lifecycle, #362). An image that never reads it wakes as a dead one does:
45 s later, from the image and its save. A snapshot keeps the files, not
the processes: a start from one runs the entrypoint again.

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

### Bounds

Every step the Computer DO takes in a container ends within its bound,
and one past it fails, saying which and what bound (entry.mjs's
`Overdue`: "the backup of /data: no answer within 480000 ms"): the
Sandbox SDK "never times out anything", and neither the runtime's
snapshot nor its destroy takes a signal. On r4 and r6 (2026-10-16) a
container that stopped answering after a wake held its sleep's backup,
and its owner's sleep with it, for good. Each bound is a const beside the
DO's others (cell/src/computer.rs), with why it is what it is:

| Step | Bound | Why |
|---|---|---|
| a marker's exec (the hold, the restore gate, letting go) | 30 s | `MARK_EXEC_MS`, as before |
| the hold's wait for `held`; the read of `unheld` | 20 s; 5 s | as before (above, "The hold") |
| a save's backups, its records together | 8 minutes | spike S3 moved 52 MB in 3.3 s, so a few GiB fit |
| a pass of deletes of records let go of | 30 s | an R2 delete takes well under a second; what a pass leaves is the next save's |
| a sleep's snapshot | 30 s | one takes 5–6 s (spike S3b); past it the sleep goes on without one |
| the guest's exit after SIGTERM | 5 s | its contract (below) |
| a destroy, and the wait for it to take | 15 s | the wait was 10 s, after a destroy awaited for good |
| a word to a container that outlived its destroy | 5 s | one that took no destroy is likely sick |
| a start's restores, its records together | 3 minutes | the start's own deadline |
| a start from a snapshot's wait for its guest | 45 s | above, Lifecycle, "Wake from a snapshot" (#362) |

So a hold ends within 55 s, a save within 9 minutes and a sleep's stop
within 55 s, each before the lifecycle's deadline for it (90 s, 10
minutes, 60 s: `fragment_core::computer`; checked as the cell compiles),
which then only catches a report its isolate lost: a hold or a stop past
it is asked for again, and a save past it failed. An owner's sleep
answers within the three together, 10 minutes 50 s (docs/api.md), with
the calls it makes to the platform's own Durable Objects (its owner's
ledger and open pages); a wake that came as it slept starts it again in
the same call, within the start's bounds.

### Exported for its owner

Its owner's export (docs/api.md, Export) holds its view and its newest
save that restores, each record as stored (a tar compressed with zstd,
its `sha256` the record's), read through the Computer DO, which checks its
owner (`computer/export`, `computer/export-read`: a range of a record at a
time, at most 8 MiB). Nothing is saved for it: what the computer did since
that save is in no save yet. A record pushed out by three newer saves as
it is copied fails the export (`source_changed`; the debt ledger).

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
    env, placeholder, hosts, modelBase?}]}], credentialEnv, models: {cheap,
    medium, fallback, vision}}`: the agents to run, each one's credentials
    (below, Connections and operator keys), and the model route's models'
    ids by its names (below, Models, "A model by its id"). **They
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
  computer on each new record (the fragment asks the computer itself just
  after the record's write answers, its delivery outbox the retry: never
  the delivery queue, docs/api.md); the agent must be a member who may read
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
  as the agent, read again every 5 minutes and on `joined`, and within
  seconds when a read of it fails (the bridge's backoff, docs/bridge.md):
  until a start's first listing, nothing is followed; the agents
  themselves from `GET /api/computer`, read again every minute (the
  bridge), or every 3 s (our Hermes image, below).
- **An agent added to a fragment wakes its computer** (Paul, 2026-10-03:
  agents are woken eagerly, to hide a wake's latency). Whatever adds an
  agent as a member (`PUT /api/f/{name}/members/{agent}`, a fragment an
  agent makes for its owner), the platform tells
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
  on), or one of the route's models by its own id (below, "A model by
  its id"), and `x-fragment-agent`. The intercept makes it the platform's
  model route as that agent (`POST /api/models/v1/chat/completions`,
  signed as the agent: docs/api.md, docs/ledger.md), which allows only
  the tier's model and a capped `max_tokens`, drops the guest's auth
  headers, reserves the call's worst case on the agent's owner's ledger,
  calls the model where the deployment runs it, streams its answer back,
  and settles the final usage at its `[DONE]` (lesson 7; not at the
  stream's end, which the gateway has held open ~150 s past it), its
  timing on its `model.settled` line (docs/api.md, Models). A call whose
  model fails before
  answering (not reached, 429, or a 5xx; on an OpenAI-compatible API,
  where the key is the platform's, also 401, 402, 403 or 404) is made once
  more on the deployment's fallback model (docs/api.md, Models): the guest
  sees only the answer.
- **Where each model runs** is the deployment's config's `models`
  (`FRAGMENT_MODELS`; `fragment_core::models::Models`; decision 61): each
  of the route's names (`cheap`, `medium`, `fallback`, `vision`) on a
  Workers AI model through AI Gateway, or on a model of an
  OpenAI-compatible API, whose key is a Secrets Store secret bound for
  that name (`MODEL_KEY_<NAME>`; docs/secrets.md). A name the config
  leaves out is on Workers AI:

  | Name | Finite's own deployment (deploy/example.jsonc) | Unnamed (a preview's) |
  |---|---|---|
  | `cheap`, every agent's default | GLM-5.3 Flash through OpenRouter (`z-ai/glm-5.3-flash`), or by config Finite Private's (`glm-5-3-flash`) | GLM-5.3 Flash (`@cf/zai-org/glm-5.3-flash`) |
  | `medium` | the same as `cheap` | GLM-5.3 (`@cf/zai-org/glm-5.3`) |
  | `fallback` | Workers AI's GLM-5.3 Flash | DeepSeek V4 Flash (`@cf/deepseek-ai/deepseek-v4-flash-0731`) |
  | `vision` | Workers AI's GLM-5.3 Flash | GLM-5.3 Flash |

  **A model by its id** (Paul, 2026-10-16: "instead of 'cheap model' can
  we pass through to hermes the actual model name in use? … it just
  seems good practice to be able to run `/model` with hermes and see the
  real model"; `fragment_core::models::Models::named`):
  - `GET http://model.fragment.internal/v1/models` lists the ids a call
    may name: OpenAI's list (`{object: "list", data: [{id, object:
    "model", created: 0, owned_by: "fragment"}]}`), each slot's model
    once, in the order `cheap`, `medium`, `fallback`, `vision`, and
    nothing of where each runs (no base URL, no key, no `body`). The route's
    names are taken and not listed. The intercept answers it itself,
    naming no agent and metering nothing: every agent on the computer may
    name every id, and Hermes' `/model` asks it with no key and no header
    of ours. The platform's own is `GET /api/models/v1/models`, an agent's
    (docs/api.md, Models).
  - A call naming an id runs on the slot of the caller's tier when that
    slot runs it (`x-fragment-tier: cheap` or `medium`, optional: the
    intercept sends it on), else on the first slot that does, in that
    order: a tier's before `fallback` and `vision`. The slot is the call's
    as if it named the slot: its key, its `body`, its fallback (a tier's
    falls back; `fallback`'s and `vision`'s do not), its price. An id no
    slot runs is refused, 400, the message listing those it may name;
    nothing is reserved or sent. The route's names keep naming their slots,
    whatever tier the caller says.
  - An id is one model: a deployment whose two slots name one id at two
    places (Workers AI and an API, or two APIs) is refused by its deploy
    and at the cell's first request. Two slots naming one id at one place
    (Finite's tiers both on OpenRouter's GLM-5.3 Flash) are fine, and the
    caller's tier tells them apart.
  - The guest reads each slot's id by its name in its view (`GET
    /api/computer`'s `models`: `{cheap, medium, fallback, vision}`; The
    fragment API, above), as the deployment names them now. A change to
    the deployment's `models` is a deploy, and no new image: a guest that
    reads its view while awake (ours, every 3 s) names the new ids within
    seconds, nothing restarted, and one that read them once names ids no
    slot may run any more, refused (400) until it reads them again. Not an
    environment variable read at the start: a computer that stays awake
    (always on) would keep naming the ids it started with. A guest that
    names the route's names (an image from before the ids, the stub's
    `think`) runs on as before.

  **What an OpenAI-compatible API is sent.** The agent's chat completion
  fields the route, our jobs and Hermes use (`CLIENT_KEYS`: `messages`,
  `tools`, `tool_choice`, `temperature`, `top_p`, `max_tokens`,
  `max_completion_tokens`, `stop`, `response_format`, `seed`,
  `reasoning_effort`, `parallel_tool_calls`, `stream`, `stream_options`),
  bounded as above, and no other: `plugins`, `web_search_options`,
  `transforms`, `provider`, `models`, `n`, `user` and the rest are dropped
  (a paid extra the book does not price, another model, or a field its
  endpoints may not take). Workers AI's calls are as before. Either way a
  message's own fields pass as the agent wrote them: since Hermes v0.21.7
  a session's earlier assistant messages carry the model's reasoning back
  (`reasoning_content` and `reasoning`, which v0.21.6 dropped for a
  `custom` provider; its `message_sanitization`), and a 4xx naming one of
  those fields has Hermes drop it for that model from then on.

  **A slot's `body`.** A slot on an OpenAI-compatible API may name `body`:
  fields of that API's own that every call to it carries (an agent's and
  a job's `ai.text` alike), each over the agent's own field of that name,
  so an agent cannot loosen them. It is a JSON object of at most 4 KiB
  naming none of the fields the route writes itself (`model`, `messages`,
  `stream`, `stream_options`, `tools`, `tool_choice`, `max_tokens`,
  `max_completion_tokens`, `reasoning_effort`); the deploy refuses
  anything else, and the cell at its first request (`fragment_core::
  models::Models::parse`). It is in the cell's `FRAGMENT_MODELS`, which
  is a Workers variable (5 KB in all), never a secret.

  OpenRouter (Paul, 2026-10-15; decision 61) is the platform's own
  OpenRouter account at `https://openrouter.ai/api/v1`, one key
  (`fragment-openrouter-key`), and its slots' `body` is OpenRouter's
  provider routing (openrouter.ai/docs/guides/routing/provider-selection,
  read 2026-10-15):

  ```json
  "body": {"provider": {"zdr": true, "sort": "throughput", "require_parameters": true,
                        "quantizations": ["fp8", "bf16"],
                        "max_price": {"prompt": 0.15, "completion": 0.5}}}
  ```

  - `zdr: true`: only endpoints with a zero-data-retention policy (it
    can only add to the account's own ZDR setting, never lift it).
  - `sort: "throughput"`: the fastest endpoint first, no load balancing
    by price.
  - `require_parameters: true`: only endpoints that take every parameter
    the call sends (`tools`, `tool_choice`, `max_tokens`,
    `reasoning_effort`, and the agent's other chat completion fields: the
    route sends only those of `fragment_core::models::CLIENT_KEYS`, so a
    field no endpoint lists never reaches OpenRouter; none left would be
    its 404, which falls back).
  - `quantizations`: fp8 or bf16 weights (an endpoint that lists none is
    `unknown`, and out).
  - `max_price`: dollars per million tokens, prompt and completion; an
    endpoint dearer than either is out, and a call none fits is refused.
    The price book charges this ceiling (docs/ledger.md, "OpenRouter's
    price"): raise it and the book together.

  On 2026-10-15 GLM-5.3 Flash had 34 endpoints on OpenRouter, each taking
  tools and `reasoning_effort` (`/api/v1/models/z-ai/glm-5.3-flash/endpoints`);
  30 were ZDR (`/api/v1/endpoints/zdr`), 8 of those fp8 (none bf16), and
  6 of those under the ceiling: BaseTen's two, Near AI, Novita, SiliconFlow
  and Z.AI (Inceptron's $0.55 out and Morph's $1.00 are past it). The
  routing leaves those 6; the fp4 and nvfp4 endpoints and those whose
  quantization is `unknown` (Fireworks, Together among them) are out. OpenRouter's stream carries its usage on a last chunk that
  keeps one content-free choice (its "accounting frame"): the route
  meters it there, and the agent gets OpenAI's last chunk, usage with no
  choices. Its answers name the provider that served each call; its
  `usage.cost` is not read (the book prices the ceiling).

  Finite Private is Finite's own GLM-5.3 Flash (`zai-org/GLM-5.3-Flash`
  on SGLang, 393,216 tokens of context) on Tinfoil, behind finite-mono's
  `finite-private-limiter` at
  `https://finite-private.finite.containers.tinfoil.dev/v1` (read from
  finite-mono 2026-10-14: `infra/tinfoil/confidential-finite-private`,
  `finitecomputer-v2/crates/finite-private-limiter`):
  - **The call.** `POST <base_url>/chat/completions` with the deployment's
    key (`fpk_live_…`) as `Authorization: Bearer`, the route's bounded
    input and the model's id, nothing of the guest's or the payer's (no
    gateway metadata: its limit is the key's).
  - **What it serves.** Streaming (its limiter always asks for the usage,
    which comes on a last chunk with no choices), tools (SGLang's
    `glm47` tool-call parser, streamed tool calls included) and GLM's
    reasoning (`reasoning_effort`; the limiter fills in `high` only when
    none is sent, and the route always sends one). Images: finite.computer
    declares it vision-capable, but nothing tests it and its limiter takes
    at most 2 MiB a request, so `vision` stays on Workers AI. It reports
    no cached tokens.
  - **Its refusals.** Past 2 MiB, 413; a model it does not serve, 400;
    each the call's. A key missing, unknown or revoked, 401 (the
    platform's key: logged aloud); its key's window spent, 429;
    finite.computer (which admits each call) not answering, 503; its model
    failing, 502: each falls back to Workers AI. Its own
    deadlines: 120 s to a first byte, 600 s of a stream's silence.
  - **Its limit** is its key's grant's: weighted tokens in a 5-hour
    window, no rate in calls, shared by every call the deployment makes
    (cloudflare-v1.md, decision 61, sizes it).
- A call that answers nothing (no first byte, a stream that stalls) is
  the runtime's own to give up on. Our Hermes image's text-tier profiles
  (cheap and medium) use Hermes' own mechanism (Paul, 2026-10-09: a
  time-based fallback "only if time-based fallback is built into
  hermes"): its stale detector kills a call that streams nothing for 20 s
  (`providers.custom.models.<its tier's model's id>.stale_timeout_seconds`,
  that model's alone: `vision`, whose answers come whole and slower, keeps
  Hermes' default), tries it once more (`HERMES_STREAM_RETRIES=1`, the
  gateway's environment), then, its one retry spent
  (`agent.api_max_retries: 1`), switches to its `fallback_providers`: the
  route's `fallback` by its model's id (the deployment's fallback model,
  its key `agent:<name>` naming the agent;
  Workers AI's GLM-5.3 Flash on Finite's own deployment, so a stall on
  OpenRouter or Finite Private ends on Workers AI), about 40 s after the stall began. The next turn starts on its tier
  again (Hermes' `restore_primary_runtime`). An owner's own model and the
  high tier have none. The Docker lane stalls the scripted model to prove
  it (images/bridge/tests/docker.rs).
- A call its owner's ledger refuses (zero credit or a canceled seat:
  402 `budget_used_up`; a guest: 403) gets the ledger's reason, and no
  call is made.
- Any other path is 404, unmetered (`GET /v1/models` answers: above, "A
  model by its id"). Hermes probes `GET /api/show` (Ollama's model
  metadata) with no `x-fragment-agent`; it keeps its configured context
  length. Anthropic's shape
  (`/anthropic/v1/messages`) comes with the high tier, which is off
  (decision 23).
- `model` may also be `vision`: the deployment's vision model (its
  config's `models.vision`), GLM-5.3 Flash unless it names another (Paul,
  2026-10-05). Workers AI's catalog
  marks GLM-5.3 Flash "Vision: Yes"; neither DeepSeek V4 Flash, the
  default fallback, nor GLM-5.3, the default medium tier's, reads images. The vision
  model must be one the price book prices, or the deploy is refused (and
  the cell, at its first request); GLM-5.3 Flash is the cheap tier's own
  row. `vision` is
  bounded, reserved and settled as a tier's call is, at its model's
  price, to the agent's owner. It is no tier: an agent's `agent.json`,
  a job's step and a manifest name only tiers.
- Our Hermes image's profiles send their image calls to `vision`
  (`auxiliary.vision`: provider `custom`, model `vision`, through the
  intercept), whatever the agent's tier: by the route's name, not its
  model's id, since a deployment may run vision on a tier's model (every
  preview: GLM-5.3 Flash), and an id a tier runs names the tier's slot,
  whose call falls back to the fallback model, which may read no images
  (DeepSeek V4 Flash); `vision` has no fallback. Hermes describes each
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
- `POST http://model.fragment.internal/v1/decide`: a typed decision on
  the Clef family, the contract a job's `ai.decide` takes, its media
  inline (docs/api.md, Decisions). It is the platform's decision route
  as the agent (`POST /api/models/v1/decide`): `clef-flash` unless it
  names `clef` or `clef-omni` (which hears audio and watches video),
  reserved at its model's context window and settled at the input tokens
  it counted, metered to the agent's owner. A tool the agent builds on
  its computer calls it as it calls a model, with the same base URL and
  credentials (`$FRAGMENT_MODEL/v1/decide`, `x-fragment-agent` or the
  key `agent:<name>`).
- **Whose call it is**, one rule for every path
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
  auxiliary ones' (titles, memory reviews, vision), and a tier agent's
  `x-fragment-tier`. Its
  speech-to-text, whose client takes no header, names the agent by its
  key (each profile's `stt.openai.api_key`, `agent:<name>`).
- **Our Hermes image names its models by their ids** (Paul, 2026-10-16):
  a tier agent's profile's model is its tier's model's id (`model.default:
  "z-ai/glm-5.3-flash"` on Finite's deployment), and its fallback the
  fallback's (`fallback_providers[0].model`), read from the computer's view
  (`models`) at boot and every 3 s after, a change rewriting each profile's
  config, which Hermes reads at the agent's next turn. So Hermes knows the
  model it runs: its owner's `/model` in the chat says `Current:
  \`z-ai/glm-5.3-flash\` on Custom endpoint`, and its prompt's `Model:`
  line names it. Its prompt is otherwise the one the tiers were validated
  on, named `cheap` and `medium`: each tier's profile turns off the
  guidance Hermes adds for a model's family by its name
  (`agent.tool_use_enforcement: false` and `agent.execution_guidance:
  false`; "auto" gives a GLM, DeepSeek or Qwen id both, and given them
  canary round 5's GLM ran every command through `execute_code`), to be
  revisited after launch. A view that names no ids (a platform from
  before them) leaves the route's names. The Docker lane reads the
  config, the prompt Hermes sends, `/model`, and a change of the ids
  while it runs (images/bridge/tests/docker.rs).
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
    source); a row with none is refused before a deploy;
  - an own key's `oauth` sign-in, `{authorize, exchange, manage}`, all
    https (a local fleet's fake may be `http://127.0.0.1:<port>`): the
    person connects it from settings through the provider's own page
    instead of pasting it (below, "An agent's own model");
  - an own key's `models`, `{base_url, offer}`: the provider's
    chat-completions base URL (https, on one of its `hosts`, so the swap
    adds the key), and the 1 to 16 models settings offers for an agent,
    each `{id, name}`.

  Adding a provider is a catalog row (and, for a connection, the provider
  enabled in the WorkOS environment); nothing in code names one. The
  platform's catalog (`deploy/example.jsonc`, `deploy/e2e.jsonc`):

  | Provider | Kind | Hosts | Placement | Environment variable | Price per call (list) |
  |---|---|---|---|---|---|
  | `google` | connection | `gmail`, `www`, `people`, `sheets`, `docs` `.googleapis.com` | `Authorization: Bearer {}` | `GOOGLE_OAUTH_ACCESS_TOKEN` (Terraform's Google provider reads it; Google's client libraries take a token as given) | none |
  | `firecrawl` | operator | `api.firecrawl.dev` | `Authorization: Bearer {}` | `FIRECRAWL_API_KEY` | $0.01 (10-result search, Hobby PAYG) |
  | `fal` | operator | `queue.fal.run`, `fal.run`, `rest.fal.ai` | `Authorization: Key {}` | `FAL_KEY` | $0.006 (1 MP Klein 9B image) |
  | `browser-use` | operator | `api.browser-use.com` | `X-Browser-Use-API-Key: {}` | `BROWSER_USE_API_KEY` | $0.02/12 (five browser minutes per control call) |
  | `perplexity` | operator | `api.perplexity.ai` | `Authorization: Bearer {}` | `PERPLEXITY_API_KEY` (Perplexity's SDKs) | $0.005 |
  | `google-places` | operator | `places.googleapis.com` | `X-Goog-Api-Key: {}`, or `?key=` | `GOOGLE_PLACES_API_KEY` (no Google SDK reads one; the goplaces skill's helper does) | $0.035 |
  | `xai` | operator | `api.x.ai` | `Authorization: Bearer {}` | `XAI_API_KEY` (xAI's SDK) | $0.12 |
  | `x` | operator | `api.x.com`, `api.twitter.com` | `Authorization: Bearer {}` | `X_API_BEARER_TOKEN` (`x-api-finite` calls `api.x.com/2`) | $0.05 (ten post resources) |
  | `elevenlabs` | operator | `api.elevenlabs.io` | `xi-api-key: {}` | `ELEVENLABS_API_KEY` (ElevenLabs' SDKs) | $0.15 |
  | `openrouter` | own, signed in to, with models | `openrouter.ai` | `Authorization: Bearer {}` | `OPENROUTER_API_KEY` (OpenRouter's and Hermes' name) | none: the person's OpenRouter credit pays |

  Hermes' tool selections are the image's `profile_config`, conditional on
  the agent's currently held placeholders, never just `credentialEnv`:
  Firecrawl sets `web.backend`, `search_backend` and `extract_backend`;
  FAL sets `image_gen.provider: fal`, `model: fal-ai/flux-2/klein/9b`;
  ElevenLabs sets `tts.provider: elevenlabs`. Without those credentials
  the category is untouched, so Hermes keeps its keyless defaults. With
  only Perplexity, `web.search_backend: perplexity`; when Firecrawl is
  also offered it wins for full-page extraction (Perplexity returns
  query-relevant snippets). X uses native `x_search`, not `web.backend`.
  Browser Use is offered as an extra: `browser.cloud_provider: local`
  prevents its key being auto-selected by Hermes' built-in browser tools;
  the agent chooses a separate cloud session with Browser Harness from
  its terminal (the platform skill's computer page has the commands).
  Browserbase would need a project id too. Native image generation serves
  chat; the platform skill's Workers AI image step remains available for
  app pipelines through the AI binding. All three optional SDK extras
  are baked in from upstream's frozen lock. Direct FAL also needs the
  scoped-client adapter recorded in the debt ledger. These API hosts and
  SDK headers were read from the pinned Hermes v0.21.6 image, including
  its pinned SDKs, on 2026-10-09; no CDN hostname receives the FAL key.

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
  env, placeholder, hosts, modelBase?}]` (`modelBase` an own key's models'
  base URL, below, "An agent's own model"), those it may use now:
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
  something lets their agents use it, with no approval of the platform's
  (what Hermes asks about: "Approvals in our Hermes image").
- HTTPS interception needs Cloudflare's CA: the image waits for it at
  boot and appends it to its trust store.
- What the catalog cannot express is never swapped: a key in a path is
  refused, and a body goes as it came (a placeholder in it reaches the
  provider, which refuses it). A vendor that signs its requests with the
  key needs more than a placement.

### An agent's own model

Paul, 2026-10-08 (decision 60): "I don't want the user to need to use the
cli or paste an api key though, they should be able to connect them from
settings ideally." A person connects their own OpenRouter account from
settings, and picks one of its models for an agent; their OpenRouter
credit pays for those calls. ChatGPT and Claude subscriptions are not
offered (decision 60 says why, with the vendors' words).

- **Connecting** (`fragment_core::own_signin`, OpenRouter's PKCE key
  exchange; docs/api.md, Connections):
  1. Settings' Connect calls `POST /api/connections/{provider}/authorize`.
     The person's computer makes a nonce and a verifier and keeps them for
     10 minutes, the verifier sealed (at most 8 sign-ins under way; past
     that, 429). The answer is the provider's page, which the shell opens
     in a window: the row's `authorize`, with the platform's callback, the
     verifier's S256 challenge, the state (`<computer's 24 hex>.<nonce>`)
     and the key's label (`Fragment`).
  2. OpenRouter asks the person and makes a key that is theirs. It sends
     the browser back to `GET /api/connections/{provider}/callback?code=…
     &state=…` on the platform's host.
  3. The state names the computer and the sign-in it began, once. No
     session is needed, and the key can reach no one else's computer. A
     state replayed, expired, made for another provider, or never made is
     a page that says to connect again, and nothing is exchanged. A
     person who declined comes back with no code: the sign-in is let go
     and nothing is set.
  4. The platform exchanges the code with the verifier at the row's
     `exchange`. Their computer seals the key as `PUT …/key` does, and the
     page says it is connected. No answer ever holds the key.
- **The guest** is given the provider as any own key: its placeholder
  (`fck_openrouter_<tag>`) in `OPENROUTER_API_KEY`. The credential also
  carries `modelBase`, its models' base URL (`https://openrouter.ai/api/v1`).
  The swap adds the key only toward `openrouter.ai`. Its calls are counted
  as the agent's in the computer's `uses` and charged nothing (no platform
  fee: Paul).
- **Picking a model.** Settings' Agents section offers each agent the
  connected provider's models beside "Workers AI" (included in their credit). The pick
  is written to the agent fragment's `agent.json`: `"model": {"provider":
  "openrouter", "id": "anthropic/claude-sonnet-5.5"}`, beside its `tier`.
  "Workers AI" removes `model`. Any model the provider serves may be named
  by hand; settings offers the catalog's `offer`. A person's agent picks
  one for another of their agents the same way, `fragment agent <agent>
  --model openrouter:<id>` (and `--no-model`), when its owner asked or
  agreed (decision 62, proposed: the agents-finite skill, which treats a
  teammate on its owner's own provider as outside the platform).
- **Our Hermes image** reads each agent's `agent.json` again every 10 s
  while awake, and whenever its credentials change. When the agent's
  credentials hold the named provider with a `modelBase`, its profile's
  main model becomes that provider's:
  - `provider: custom`, `base_url` the `modelBase`, `default` the model's
    id, which its owner's `/model` shows (the Docker lane's
    `an_agent_runs_on_its_owners_model`);
  - its key is the placeholder;
  - no `context_length`: Hermes reads OpenRouter's.

  Otherwise the agent runs on its tier, through the route, and
  `profile.model_unused` says why: not connected, or narrowed from it. The
  config is rewritten only when it changes. Hermes reads it at the next
  turn and rebuilds the agent on the new model, with nothing restarted.
  Vision (`auxiliary.vision`) and voice notes (`stt`) stay on the route's
  `vision` and `whisper`, metered to the owner as before.
- **Disconnecting** (`DELETE /api/connections/{provider}/key`, settings'
  Disconnect) takes the key from the computer. The agent's next turn is
  its tier's again. The key itself is revoked at OpenRouter
  (`https://openrouter.ai/settings/keys`, which settings links to).
- The platform holds no Hermes code for this. The catalog row says what a
  provider offers, and the guest view says where its models are.

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
chat's menu ("<name>'s screen"), or from a teammate's ⋯ in the shell's
team ("Its screen"), or from settings. The platform carries
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
- `images/hermes`: Hermes v0.21.7's desktop image, `hermes-boot`, the
  bridge as Hermes' Relay connector, the screen. The base is pinned by
  digest to v0.21.7's release build (its versioned tag
  `rc.15-v0.21.7-desktop`, 2026-10-12; its install stamp says 0.21.7,
  commit `71d34bad`); upstream tags no `v0.21.7-desktop`, and a bare
  version tag is not always the release (`v0.21.6-desktop` was an
  earlier attempt's build). Hermes there is Python
  3.14.7 in a venv its own package manager (PM) builds, its tools (its
  Python, Node 26.7, npm 12, uv, ffmpeg, ripgrep, Chromium 145) in PM's
  store at `/opt/hermes/tools`. Its layers go by how often each changes:
  the base, one layer (Hermes' image flattened, with everything pinned
  on it: 4.3 GB, 1.4 GB compressed), then ours (the bridge, `hermes-boot`,
  the fragment CLI and the image's own files: about 21 MB, 9 MB
  compressed), then a few KB `hermes-boot build-info` writes and the
  image's version. A deploy that changes only our code pushes the small
  ones; the base changes only with a pin, `document-requirements.txt` or
  one of its steps (or when the deploying machine's build cache has lost
  it). One Hermes
  profile per agent (named as its agent fragment, `juniper--k3x9`), its agent
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
  default; `high` only with `FRAGMENT_HIGH_TIER=on`, decision 23), which
  its profile names by its model's id (above, Models). One the
  platform does not answer for (no 200, 403 or 404) is said
  (`profile.tier_unread`), and the profile's config stays as the last boot
  wrote it, its tier with it; a profile with none yet takes the cheap
  tier (GLM-5.3 Flash, DeepSeek V4 Flash its fallback; Paul, 2026-10-09). The shell writes `cheap` for
  both the first agent and each new agent. Its `model: {provider, id}`,
  beside the tier, is a model of a provider its owner connected, which it
  runs on while its credentials hold that provider (above, "An agent's
  own model"); the image reads `agent.json` again every 10 s while awake
  (`HERMES_BOOT_MODEL_MS`, a test's), so a pick is its next turn's.

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

### Approvals in our Hermes image

Nothing technical asks a person (Paul, 2026-10-14: "Our users are
non-technical: if hermes asks them a technical approval it makes no sense
to them … hermes asking for approvals for stuff like 'can I make this
site public?' would make sense, but for technical command line things it
can't be asking"). No command, file, path or code is ever put to the
agent's owner. The computer is the person's own sandboxed container, and
the agent acts on the platform with at most its owner's role (R17), so a
command reaches no further than those. finite-mono ran Hermes the same
way (`approvals.mode: off`). What asks is `hermes-boot`'s to set, with
Hermes' own settings (images/hermes/boot/src/hermes.rs, `managed_config`,
the overlay over every profile), against Hermes v0.21.7. A card that
still comes is its owner's alone to answer (docs/bridge.md; decision 42).

Every ask Hermes has, with what the owner saw before 2026-10-14 (each a
card on `work`) and what happens now:

| What asks | Its setting | Before | Now |
| --- | --- | --- | --- |
| Its terminal: a command matching Hermes' dangerous patterns (`rm -rf`, `chmod 777`, `curl … \| sh`, a `-c` script, a write to a system file, …) | `approvals.mode` | `smart`: its guardian first (a call to the agent's model, metered to its owner), then a card, "Hermes wants to run a command that needs your OK", the command, why it was flagged, and Allow Once, Allow Session, Always Allow, Deny | `off`: it runs; no guardian |
| `execute_code`: each script | `approvals.mode` | the guardian, then a card with the script | `off`: it runs |
| `computer_use`: every action but `capture`, `wait`, `list_apps` and `list_windows`, and raising a window | `approvals.mode`, `command_allowlist` | nothing: each profile held them all granted (`cua:<action>:<mode>`, #284) | `off`: it runs; no grant is held |
| A write to an SSH client config (`~/.ssh/config`) | `approvals.mode` | a card, "Write to SSH client config file(s): …" | `off`: it runs |
| A plugin's `pre_tool_call` rule that asks | `approvals.mode` | none of the image's asks | `off`: it runs |
| A write through Hermes' file tools to a project's `AGENTS.md`, `CLAUDE.md`, `SOUL.md`, `.cursorrules` or `.hermes/` file (a brain's guide, an app's notes; its profile's own `SOUL.md`, in Hermes' home, never asked) | `security.protected_instruction_files` | a card, "Write to protected agent-instruction file(s): AGENTS.md …", Allow Once, Deny, whatever `approvals.mode` says | `false`: it runs |
| `/new`, `/undo` from the menu | `approvals.destructive_slash_confirm` | `false` | `false` |
| `/reload-mcp`; an MCP server's elicitation, or a write-capable tool of an untrusted one | `approvals.mcp_reload_confirm`; `mcp_servers.<name>` | on no menu; no MCP server is configured | the same |
| A skill's or a memory's write staged for a yes | `skills.write_approval`, `memory.write_approval` | off (Hermes' default) | the same |
| Filling a saved payment card into a page (`browser_vault_fill`) | none: Hermes asks whatever `approvals.mode` says | "Confirm payment card fill?", naming the site: a person's question, about their money | it still asks |
| The agent's own question (`clarify`) | `agent.clarify_timeout` | with choices a card, else a question in words (`turn.asked`) | the same: how it asks about outcomes |

Hermes' rails that ask no one stand, before any of these: its floors for
the terminal (a recursive delete of `/`, a system directory or the home
directory, `mkfs`, `dd` or a redirect to a block device, a fork bomb,
`kill -1`, a shutdown, deleting Hermes' own runtime, a password piped to
`sudo -S`), its desktop's (a log-out or lock key, `curl … | sh` typed),
its file tools' denials (a credential store, a `.env` file, a system
file: `agent/file_safety.py`), and another profile's files. Each is a
refusal its model reads, never a card. `off` runs the desktop's driver
(cua-driver) in its `unrestricted` mode, a private daemon of the
session's, where `standard` was its machine-wide one: Hermes ties the
driver's mode to its approval bypass. The desktop is the agent's own, and
Hermes' desktop rails come before the driver.

What a person is asked instead is an outcome, in plain words. Every agent
is told so in its system prompt, on every turn in our chats (Hermes'
`platform_hints.relay`: `hermes::ASKING`), and the `fragment` skill's
first page says it at more length (`computer.md`, "Asking your owner"):
before it makes something of theirs public or shares it with someone new,
sends an email or a message in their name, deletes something of theirs,
or spends their money, unless they asked for exactly that, it asks with
`clarify` and its choices, a card only its owner answers. That is the
agent's to keep: the platform itself holds no agent's share for a yes (an
agent shares for its owner as they would, and each share names it:
docs/api.md, Sharing), and refuses every agent a fragment's deletion and
its cap. A card, a clarify's or the payment card's, waits as long as the
bridge's prompt lives (`approvals.timeout` and `agent.clarify_timeout`
are both `BRIDGE_PROMPT_TTL_MS`'s, an hour; `HERMES_BOOT_CARD_TIMEOUT_S`
is a test's shorter one).

### Its name, in our Hermes image

An agent's name is its agent fragment's title (docs/api.md, "An agent's
name and picture"), and the image tells the agent it at every turn, in
every chat: each profile's own config holds it in `agent.system_prompt`
(`hermes::told_name`: "Your name is Juniper: your agent fragment's
title, as it is now", and that a name an earlier message or its memory
uses was from before a rename), which Hermes reads from the profile's
config at each turn and adds to that turn's system prompt alone (its
ephemeral system prompt, never kept in a session's). The boot writes it
from the title it reads for Bot Mode (below), once a round for both: at
each boot, and every `HERMES_BOOT_MODEL_MS` (10 s) while awake, so a
rename reaches the agent's next turn, its Bot Chat's lasting session
too, nothing restarted; a title unread leaves the config as it was. A
`${` in a name is broken apart (`$ {`): Hermes expands `${VAR}` in a
config's strings from its environment. Nothing else tells the agent its
name: its `SOUL.md` is its job (the shell's first agent's names none),
and Hermes' Bot Mode tells it its `@handle` alone. An agent made before
this whose `SOUL.md` still says its old name is told its name all the
same; its owner, or the agent, may take the name out of its job.

### Bot Mode in our Hermes image

Hermes' Bot Mode is on: each agent is a bot, and in its Bot Chat it has
Hermes' teammate roster (each other agent of the computer, by name and
role) and its `message_agent` tool, with which it messages another agent.
A person's default agent makes and keeps the others (decision 62,
proposed): `fragment agent --new` starts one and makes its Bot Chat as
the shell does, and the new teammate is on the image's roster within
seconds (on a bot's own roster from its next session: Hermes builds a
session's prompt once).
Hermes turns both on (v0.21.6: `tools/bot_mode_probe.py`,
`tools/bot_mode_dm.py`, `agent/system_prompt.py`) only for a session
titled exactly `Bot Chat`, on an install where a profile's `profile.yaml`
carries `ui_meta: {hermes-bots: …}`. A headless gateway, ours, meets
neither on its own (Hermes' `website/docs/user-guide/bot-mode.md`, "What
actually makes a chat a Bot Chat"), so the image does both
(`images/hermes/boot/src/bots.rs`):

- Each agent's `profile.yaml`, written by the boot with its profile, and
  again whenever it would say something else: its agent fragment's title
  (its status's, read again with its model every 10 s while awake,
  `HERMES_BOOT_MODEL_MS`) as its display name and its bot's title, which
  is the agent's name everywhere (docs/api.md, "An agent's name and
  picture": the shell titles an agent fragment by the agent's name,
  Settings and `fragment agent --name` rename it; with none, its name on
  the computer, its label), and its job's first line (its `SOUL.md`'s,
  at most 120 characters, as a sync pulls it) as its description.
  Hermes' roster reads both, and signs a bot's messages to its teammates
  with its title (`Message from 🤖 Juniper (@juniper--k3x9): …`). A
  rename reaches Bot Mode within seconds, nothing restarted (event
  `profile.identity`); a title unread leaves the file as it was. The
  file is the boot's alone: an agent that writes it is overwritten, and
  the platform skill tells every agent to rename itself with `fragment
  agent` instead. Bot Mode carries no picture (Hermes v0.21.6 reads a
  profile's title, display name and description alone), so an agent's
  picture is shown by the platform's pages alone. Its roster
  always lists the gateway's own profile too, as `@hermes`; that profile
  runs no agent's turns, and its `profile.yaml` says so ("this computer's
  gateway, not an agent: never message it").
- An agent's Bot Chat is its own chat with its owner: the chat the shell
  makes with it, labelled `<label>-chat` beside its `<label>`, its suffix
  its own (`bots::bot_chat`, which finds it in the agent's list for its
  owner: at a boot, as agents come, and every few seconds while one is
  missing, since the shell makes the chat after it assigns the agent; the
  shell's `makeAgent` and `fragment ask`'s `direct_label` make and find
  the same). Hermes'
  gateway makes that chat's session at its first message, and builds a
  session's prompt once, at its first turn, keeping it with the turn's
  agent for the next ones; so the title has to be there before. The image's
  gateway hook (`images/hermes/hooks/fragment-bot-chat`, linked into each
  profile's `hooks/` by the boot; Hermes' gateway/hooks.py) does it: as
  each of the agent's turns starts (`agent:start`), before its prompt is
  built, a turn in its own chat (the boot's bots file,
  `/var/lib/fragment-run/bots.json`, names it, its profile's home, and the
  key Hermes keeps its session under) titles that session `Bot Chat`
  through Hermes' session API, as a person's `/title` would: its row
  recorded with its gateway identity first when the turn has not yet
  written it, and any other session of the agent's holding the title made
  to give it up (its history kept). So the agent's first turn there has
  the roster. The title follows a compression to the new session (Hermes'
  own). Event: `botmode.titled` (the gateway's output).
- A message one agent sends another is a hand-off in the other's own
  chat, which its owner sees, and the other's answer a turn of the
  bridge's there (drafts, steps, cards, Stop, its screen; the hop count
  and the chat's budget, docs/bridge.md). Hermes would run that answer as
  a turn of its own, outside the gateway (`hermes -p <agent> chat -c "Bot
  Chat" -Q`), unless the Bot Chat has a live owner, as Hermes Desktop is
  (`tools/bot_live_delivery.py`): then the message waits in the agent's
  mailbox for its owner. The image's Bot Mode keeper
  (`images/hermes/botmode.py`, in Hermes' own Python as the hermes user,
  started and restarted by the boot) is that owner. About once a second,
  for each agent in the bots file, it holds the agent's Bot Chat session
  (the compression tip of the session titled `Bot Chat`, when that is its
  own chat's) with an active-session lease marked as a live-delivery
  consumer, and takes each message waiting in its mailbox: it posts it
  into the agent's own chat as the agent that sent it, naming the agent
  in `to` (id `dm-<delivery>`, acting as the sender for their owner, as
  `fragment ask --chat` posts a question, but adding the sender to no
  chat: the bridge answers a record by `to`, and the chat's page names an
  agent that is no member by its profile), and once the agent's turn of
  it ends (read from the chat's `work`, as `fragment ask --wait` reads it,
  for up to 30 minutes), settles the delivery with its
  replies (up to 16,000 characters; a failure with why). That wakes the
  sender, whose turn says the answer in the sender's own chat (a message
  of its runtime's own: docs/chat-records.md). The message's text is
  Hermes' own, its attribution first (`Message from 🤖 Juniper
  (@juniper--k3x9): …`). At most four messages per agent are delivered at
  once; the rest wait in its mailbox. While the platform holds the
  computer the keeper writes nothing. Events: `botmode.owned`,
  `botmode.taken`, `botmode.delivered` (its status, sender and `ms`).
- The answer wakes the sender after a restart too. A session Hermes'
  gateway reads back from disk (every session from before a deploy or a
  sleep) holds no live adapter, and an agent's profile has none of its
  own (the relay is the default profile's), so a background process's
  completion in such a session (message_agent's answer among them) had
  no adapter to go through: the gateway retried it every 5 s, silently,
  for ever (p5, 2026-10-09). The
  boot routes each agent to its profile in the gateway's config
  (`profile_routes`, in the managed overlay: `bots::profile_routes`), by
  one chat: its Bot Chat, else a chat it is in (an agent in no chat has
  no session to lose one in). A route makes its profile one the gateway
  reaches through the default profile's relay (v0.21.6:
  `gateway/authz_mixin.py`, `_is_shared_bot_satellite`), and Hermes asks
  only whether any route names the profile, so one reaches the agent's
  sessions in every chat it is in (its other chats, the shell's
  `<label>-chat-<n>`, group chats, an app's chat). No route picks a
  message's profile: the relay names it on each. The gateway reads them
  as it starts, and no verb of Hermes' reloads them (`rescan-profiles`
  serves a new profile, not new routes); none is needed between starts:
  a chat or an agent come since has its sessions in that gateway's life,
  which hold their live adapter, and every start is a boot's, which
  writes the routes first.
- An agent with no such chat (made by other means than the shell) has no
  Bot Chat of ours, and no owner: Hermes runs a teammate's message in a
  Bot Chat of its own, outside the bridge, which the person sees nowhere
  until the agent has its own chat, whose session then takes the title.
  So does every agent while the keeper is down.
- A bot answers a teammate once. Hermes' roster tells a bot that a
  teammate messaged to answer with `message_agent`, and the delivery's
  answer wakes the sender too, so each said it twice (the sender relaying
  the answer again, the teammate acknowledging that: two redundant lines
  a run on the preview). So the posted message ends with a line telling
  the bot its reply there goes back to the sender on its own, and not to
  message the sender about it (`ANSWER_HERE`). When a bot messages its
  sender back anyway while answering (the keeper sees its message taken
  while it answers the sender's, or within 3 s of that turn's end), both
  deliveries settle with their answer under a line telling the woken bot it
  has had it already, and to end its turn with exactly `[SILENT]` if it
  adds nothing (`HAD_IT`), Hermes' silence marker, for which its gateway
  says nothing (gateway/response_filters.py). Event: `botmode.delivered`'s
  `had` (`answered`, `reply`).

Proven by the Docker lane's `bots_message_each_other` (the scripted
model: the roster and the tool from an agent's first turn in its own
chat, a message posted into the other's chat as the sender, answered
there by a turn of the bridge's, settled once, and said in the sender's
chat, and so again after the container restarts, with a background
job's completion in a chat that is no Bot Chat, and in the chat of an
agent with none), and on a preview by the hosted `bot-mode` section, run by name
(`--only bot-mode`): two real agents. Juniper, asked to have Maple make
and publish a todo app and show it in her browser, runs `message_agent`.
Maple's turn of it, in her own chat, makes the app, which goes live, and
opens it on her desktop. Juniper then tells the person. Both chats and
both screens are shot as the run's evidence; Maple's screen is checked
to connect.

### Skills and the CLI in our Hermes image

The platform serves skills as any fragment's files; what installs them,
and how a runtime finds them, is the image's.

- **The managed set** (decision 17) is the owner's skills fragment's
  `skills/`: a fragment on the blessed `skills` template lists and reads
  the platform release's managed set as its files (templates/skills/README.md),
  beneath any file of its own. `hermes-boot` finds it as the computer's
  first agent acting for the computer's owner (`GET /api/fragments?for=`:
  of kind `skills` the owner owns (the `?for=` list's `owned`), one labelled `skills`
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
  what a fragment is, the commands for the agent's fragments; its
  platform's URL the CLI's default, production's, since nothing names
  another at the image's build, and the agent skips the install and
  pairing it names) after a page
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
  `fragment skill` is no skill named `fragment`, or any bundled or platform
  skill mentions a reference path it does not ship. A missing `fragment
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
  lists, the platform skill, and Hermes' pinned bundled skills. The managed
  release has twelve workflows: apps, git, brains, connected Google,
  image steps and model tiers, plus Cashu, Nostr CLI, X API, direct music,
  market analysis and Polymarket (templates/skills/README.md). Generic skills
  come from Hermes; optional ones remain in its hub. At build, the image
  moves Hermes' bundled tree to `/opt/fragment/bundled-skills`, so stage2
  and the gateway sync nothing into a saved profile. The boot copies each
  skill and its helpers into the read-only platform view at each start
  and after each managed install. A managed name or alias suppresses the
  corresponding bundled copy; removing it restores the image's skill.
  Hermes' standalone `google-workspace` is omitted: its OAuth setup and
  token files conflict with the platform's connected-account placeholders,
  so `google-workspace-finite` owns that workflow. A managed skill
  installed or removed during a session reaches it at its next turn: since
  v0.21.7 Hermes compares the skills its prompt lists with the ones it
  finds now and says the difference once, in that turn's note
  (`agent/skills_index_delta.py`), with no prompt rebuild.
- **Documents:** Hermes' `pdf`, `docx`, `powerpoint` and `xlsx` skills and
  their helpers are visible to every agent. Their Python dependencies are
  baked into the image at versions and SHA-256s pinned in
  `images/hermes/document-requirements.txt`: ReportLab, pypdf, pdfplumber,
  PyMuPDF, pypdfium2, python-docx, python-pptx and openpyxl. For a quick
  structured PDF, use Hermes' `pdf_create.py` with a JSON spec; for a
  designed report, use the image's Chromium with HTML/CSS and
  `--headless --no-pdf-header-footer --print-to-pdf=<path>` (its wrapper,
  `/opt/fragment/bin/chromium`, adds the container flags). Render every
  page with `pdf_page_image.py` and inspect it before delivery. No
  WeasyPrint, Pandoc, LaTeX, LibreOffice or nano-pdf is preinstalled;
  document conversion, spreadsheet recalculation and AI PDF edits that
  need those remain optional. The audit and two offline sample paths are
  in docs/skills-audit.md.
- **Automatic learning and local forks (2026-10-09):** automatic memory and
  skill reviews are on (`auxiliary.background_review: { enabled: true }`).
  Managed skills stay read-only in `/data/hermes/managed-skills`; sync
  replaces that baseline whole. Hermes and the user own each profile's
  writable `skills/`, synced both ways to the agent fragment. The image's
  `fragment-skill-fork` plugin intercepts a flat `skill_manage` patch, edit,
  write_file or remove_file targeting our managed/platform external roots:
  after Hermes' fresh-read check, it copies the whole category/name package
  locally, then Hermes edits the copy with its normal validation and scan.
  The copy shadows the baseline by name, and a later managed release never
  overwrites it. Remove the local copy to follow the updated baseline again.
  External batch writes must first fork with a single write; deletions retain
  Hermes' ownership guards. Manual customization follows the same whole-copy
  rule. Explicit memory/skill tools and the separate periodic curator remain
  available. Background reviews still spend model calls; Paul's memory-review
  decision keeps them on despite the shared account limit.

- **The fragment CLI** is in the image (`/usr/local/bin/fragment`, built
  from `cli/` with the image: the Hermes image's build context is the
  repo's root, and its CLI stage copies only what the CLI builds from:
  "Stable and canary", above). Each profile's `.env` names its agent and its owner
  (`FRAGMENT_AS_AGENT`, `FRAGMENT_FOR`), and its config passes those two to
  its terminal (`terminal.env_passthrough`, scoped to the profile under the
  one gateway), so a command in an agent's terminal acts as that agent:
  the CLI's agent mode (cli/GUIDE.md, "As an agent") names the agent in
  `x-fragment-agent`, signs nothing, and reaches the platform at
  `FRAGMENT_API`, acting for the owner on the routes that honor `for`. The
  egress signs. No key is in the container. `fragment sync` and `deploy`
  still reach code.storage directly, with the short-lived, repo-scoped
  token the platform mints for the agent.
- **Hermes' own CLI** runs by name in an agent's terminal
  (`hermes --version`, `hermes config`): `/usr/local/bin/hermes` runs
  upstream's `/opt/hermes/bin/hermes`. Hermes runs a terminal's commands,
  and a background process, from a login shell (`bash -l`, `bash -lic`),
  where Debian's `/etc/profile` sets a user's PATH to
  `/usr/local/bin:/usr/bin:/bin:…`, without the image's `/opt/hermes/bin`;
  upstream links its other tools into `/usr/local/bin` (node, npm,
  python3, rg, ffmpeg), not `hermes`. execute_code's scripts keep the
  gateway's PATH. The Docker lane's `an_agent_runs_hermes_by_name` runs
  it each way.
- **Its credentials** (Connections and operator keys, above):
  `hermes-boot` writes each agent's placeholders into its profile's `.env`
  under their variables (`PERPLEXITY_API_KEY=fck_perplexity_…`), which
  Hermes reads again at every turn, so its own tools (its Perplexity web
  search, ElevenLabs speech, xAI) use them too; and the profile's config
  lists the catalog's allowed variables (`credentialEnv`) in its terminal's
  passthrough, including `GOOGLE_OAUTH_ACCESS_TOKEN` and `X_API_BEARER_TOKEN`.
  Provider names are left out of that list (Hermes otherwise warns on
  every turn); the pinned policy is checked by the image's Docker test.
  Hermes never
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
  opens nothing of anyone else's. A fenced agent (decision 63, proposed)
  is the exception: its commands run as a user of its own, with no sudo
  ("A fenced agent in our Hermes image", below).
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

### A fenced agent in our Hermes image

Decision 63 (proposed): an agent its owner, or their agent, fences acts
as itself alone. The platform refuses its `for` (docs/api.md), and its
computer's guest view says `fenced` and gives it no connection. On the
computer it is the same profile under the same gateway, walled off by
Hermes' own per-profile terminal and the kernel's users and namespaces;
no second computer.

- **Its user.** `hermes-boot` (root) makes it a Linux user of its own,
  `fence-<n>`, its uid kept for the agent in `/data` (root's), never the
  `hermes` user, never in its group, with no sudo and no password (`*`,
  so sshd takes its key). Its work directory (`/data/work/<agent>`: its
  home, its `tmp`) is its user's and the `hermes` group's (2770, so the
  gateway still reads and writes it: attachments, generated images);
  every other agent's work directory is `hermes`' alone (0700), and so
  are Hermes' home (`/data/hermes`, with every profile's `.env` and
  their desktops' sockets) and the boot's runtime files. `/data/work`
  itself is 0711: no one lists it. Fenced again after it ran unfenced,
  its work becomes its user's; let go, the `hermes` user's again.
- **Its sandbox.** One process of its user's holds a user, network and
  mount namespace open for it while it runs (`unshare -r -n -m`, its
  loopback up, its `tmp` bound on `/tmp`): its own loopback and no
  other interface, so its commands reach no network at all (not the
  platform's intercepts, not the internet, not the computer's own
  services: the relay, Hermes' API, the screen, another agent's
  browser), and its background servers answer its later commands.
  `/proc` is mounted `hidepid=invisible` with root's group exempt (the
  boot watches every process, and a container's root has no
  `CAP_SYS_PTRACE` to see past it), so a fenced user sees only its own
  processes, never another agent's command lines, and the `hermes` user
  sees only its own.
- **How its commands get there.** The computer's sshd listens on
  loopback alone (`127.0.0.1:2222`), lets in only fence users by a key
  the gateway's user holds (made at each boot, under the boot's runtime
  files), forwards nothing, and runs every session through its
  `ForceCommand`, which joins the session to its user's sandbox
  (`nsenter --preserve-credentials -U -n -m`), found by its uid, never a
  name it is told, or refuses it when there is none. Its host key is the
  computer's, root's, under `/data`: Hermes' `ssh` backend remembers the
  first key it meets, so it never meets another. The profile's terminal
  is Hermes' own `ssh` backend (`terminal.backend: ssh` to that user),
  which also carries its file tools and `execute_code` (Hermes runs them
  through the terminal's environment), so every tool that reads or
  writes files runs as the fenced user, in the sandbox.
- **What is off for it.** In its profile: the browser, vision of a local
  path, video, the desktop and computer use, projects, kanban, catalog
  and connections management (`agent.disabled_toolsets`), each of which
  reads files or reaches services in the gateway's own process; Bot Mode
  (it is in no Bot Mode roster: `message_agent` acts for its owner, which
  the platform refuses it), so it is reached by its chats; and
  `FRAGMENT_FOR` (its terminal names no one to act for). Its web is
  Hermes' `web_search` and `web_extract`, which refuse private and
  internal addresses.
- **The bridge.** What a person attaches for a fenced agent lands in its
  work directory as its user's and the `hermes` group's; an attachment
  it sends is read from its work directory alone.
- **Fenced or let go while awake.** The boot reads the change with its
  agents (every 3 s) and writes the profile again; nothing restarts.
  Hermes rebuilds a session's agent when `agent.disabled_toolsets`
  changes, but keeps a session's terminal, and the background processes
  it started, for the session's life. The image's
  `fragment-terminal-follows` plugin (a `pre_tool_call` hook, as
  `fragment-skill-fork` is) ends a session's kept terminal that is not
  the one its profile now names (local, or ssh to which user), with its
  background processes, before the next tool that runs in it; a call it
  cannot check is blocked. So a fence put up while a turn runs holds from
  that turn's next command, and a process started before it never runs
  on as the `hermes` user.

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
  the start of a sleep's try whose save failed (a hung save's minutes
  included) to `computers.unsaved_max_ms` after that failure (and the try
  under way then, to its own failure), or to the
  save that works first (Paul, 2026-10-08; "What its owner is told").
- Model calls bill the agent's owner, through the platform's model
  route. Each operator key's call is held on the agent's owner's ledger
  before it is made, and settled once the provider answered, at the key's
  price and the margin (`key:<computer>:…`; decision 37); a catalog's
  operator key always has a price (its own or
  the price book's list price). A connection's call and an own key's are
  counted, never charged; every call is in the computer's `uses`.
- Its saves in R2 are not metered: the platform pays for them for now
  (docs/technical-debt-ledger.md, "A computer's saves are not metered").
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
  told until a restart; an update told while awake on another version
  than its next start runs, between turns, never asleep or starting,
  taken by a restart or a sleep and a wake, put off for that version only,
  one image one version wherever it is pulled from. The restart: a sleep
  that saves then a start,
  held as an owner's wake, never cancelled by a keepalive, asked twice
  made once, an earlier start's nothing, an owner's sleep after it
  winning; its failed save stopping unsaved and told as the restart's;
  its start fresh from the image, no snapshot kept until one comes up.
  The simulation restarts and deploys too, and checks every notice against
  the state (a loss told is unseen and a life it had; never a stop before
  the bound; an update told awake, between turns, never for the version it
  runs or one put off),
  and that no meter falls in a free window. The free window: nothing
  metered from the start of a sleep's try whose save failed to its bound,
  before and after it as any, a late report or an alarm again metering
  nothing twice, the same across a restart of the DO
  (`the_whole_failing_try_is_free`: a hung first save free from its
  start, a quick failure as before, metering again once a save works, a
  restart of the DO or its owner's mid-window, the try at the bound free
  to its failure, one begun past it metered, and no window over time
  metered already). A save that never reports failed at its
  deadline (`a_save_that_never_answers_fails_at_its_deadline`): a
  sleep's kept, tried again, each try hung, and ended unsaved by the
  bound, its window free; awake, counted and told from the second; its
  late answer nothing, the same across a restart of the DO. A start from a
  snapshot up once its guest is heard, dead unheard past its wait, an
  earlier container's read not its, and a dead one started again from the
  save in the same wake, no strike (#362).
- The e2e on workerd: the Computer DO's routes and its intercepts,
  against `images/stub/` under `wrangler dev` with Docker. A crash is the
  lever's (`POST /api/test/computer {computer, op: "kill"}`: SIGKILL to
  the guest's PID 1, so the real exit is reported), and wakes from the
  save its last turn's end took; a failed save is the lever's
  (`fail-saves`), and so is an always-on plan (`always-on`). A sick
  container's backups are the lever's too (`hang-backups`: its
  `sandbox-shim` never answers, and its saves' backups get the lever's
  few seconds): its owner's sleep answers at their bound, awake and kept,
  saying why, and once they answer again its sleep saves. Local workerd
  takes no snapshots, so the lever's `alive-check` makes a start from the
  save wait for its guest as one from a snapshot does: heard, it is up;
  with `freeze` (SIGSTOP to its PID 1) never heard, it is dead, and the
  same wake starts it again from the save (#362). A check of
  what ran counts runs (the model fake's calls, the ledger's rows, the
  computer's `uses`), never records, which a second run replays. The
  e2e's bound is 45 s (`crate::UNSAVED_MAX_MS`), so the `computers`
  lane's recovery checks see it run out: a failed sleep told at once with
  its stop time, a restart asked twice (its save failing too) going back
  once and saying so until seen, the unsaved stop at the time it said and
  the wake after told once, a restart of a sleeping computer, one
  refused at zero credit, and no awake time on its owner's ledger in
  either free window. Its image pin, which is what a deploy that changed
  the image is to a computer: the update told awake, not while a turn
  waits on its card, again when it ends, put off by Later (again the
  same; another version, or none, refused), taken by a restart (the next
  build served), and not told asleep. The `shell-ui` lane sees the same in
  Chrome: the warning on Settings with no reload, its Restart, the notice
  of what it went back to and its OK (gone after a reload), Settings'
  Restart computer, and an update: shown with no reload, Settings' version
  and the latest, its Restart taking it, its Later holding after a
  reload. On a preview, the hosted `agent-restart` section (by name)
  restarts a real Hermes computer awake after a reply: it comes back
  running from its newest save, the restart's own and held, from the image
  and not a snapshot, with no rollback; the same press again restarts
  nothing; and its agent answers after it, nothing left to tell its owner.
  Given a round's candidate (`--candidate`), that restart is the upgrade:
  pinned to the candidate awake after a reply that gave it a word, told
  an update is ready, restarted onto the candidate's version from its own
  save, and the same chat names the word; then pinned back, told again,
  restarted onto the default's version, and the word named once more
  (the rollback). The candidate's other real-agent sections (agent-smoke,
  agent-ask, bot-mode) and the hosted `computers` start their computers on
  it.
- The e2e's `wipe` section (crates/e2e/src/lanes/wipe.rs): a computer
  whose `/data` holds a file its agent wrote and a save, wiped with its
  owner (its first step alone, then across a node's crash): no computer
  from that step, its saves gone, its owner's next identity's computer a
  new one that restores nothing and reads no such file; hosted, the same
  on the deployment's own image.
- The e2e's `admin` section: an operator pins a person's computer (none
  yet is 404) to the stub's next build, its view and the person's page
  naming it and both images' whole references; an image the deployment
  lacks is 400, anyone else 403; `fragment operator pin` pins it back, and
  the log holds both pins.
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
  Approvals (`nothing_technical_asks_its_owner`): a scroll, a focus
  change that raises its window, a foreground click, `rm -rf`, an
  `execute_code` script, and writes to a project's `AGENTS.md` and to
  `~/.ssh/config` each run with no card and no guardian call, and the
  model's system prompt says how to ask (`hermes::ASKING`).
- The platform's side, with the stub: the `computers` lane's ticket that
  lands on `?agent=` and refuses a path off its port, and an agent's
  screen's control socket through the port (refused for an agent not on
  the computer, 404, and for no name, 400: the image's answers); the
  `shell-ui` lane's menus (a direct chat's "Its screen", a group's
  screen per agent, a teammate's ⋯), each frame landing on
  `?agent=<that agent>`. The
  hosted `agent-smoke` checks that its chat's agent's screen is the one
  served: the RFB stream's desktop is that agent's (`hermes:<profile>`),
  the page names it, and another agent's is refused.
