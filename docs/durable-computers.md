# Durable computers: the design

The design of record for how a computer's state survives a sleep, a
crash, a host restart or a rolled-back disk (Paul, 2026-10-05). The
analysis behind it is docs/explorations/pi-durable.md, where the
findings (F1–F11), invariants (I1–I9) and proposals (P1–P8) are named.
The discussion is in the Claude Doc "Durable computers: what's left".
Where this file and docs/cloudflare-v1.md decision 18 disagree, this
file is the newer word, and decision 18 points here.

## The rule

> One authority for each fact. Everything else is a cache, and a cache
> may be lost or go back in time without changing what happens.

| Fact | Its one authority | Everything else |
|---|---|---|
| What was said | the chat's `chat` channel | Hermes' session transcript, derived |
| Which turns started, what they did, how they ended, each prompt's answer | the chat's `work` channel (the journal) | the bridge's `state.json`, a cache of where to resume |
| The agent's self (`SOUL.md`, memories, skills) | the agent fragment's repo | the profile's checkout, a working copy synced every minute |
| The model's context and the agent's working files | `/data`, as of its newest save | the snapshot, a cache of that save for one image |
| Which save is current, and what a wake restored | the Computer DO | |
| Usage | the ledger | |
| A messenger's encryption state (Signal, SimpleX) | its connector's Durable Object (F, below; not built) | nothing |

## Built (2026-10-05)

- **One life per turn (P1, #137).** A turn runs only in the life of the
  bridge that wrote its `turn.start` to `work`, and only once that post
  is answered. A rolled-back or lost `/data` runs no turn twice; a turn
  a crash cut ends once, as lost. This is the at-most-once choice: a
  second run of a turn with effects (an email sent twice) is worse than
  a turn the person is told was lost. docs/bridge.md, docs/chat-records.md.
- **The sleep's hold (#136, #137).** The DO touches
  `/run/computer/hold` before a sleep's backup; the bridge claims no
  turn while it exists. docs/computers.md.
- **The snapshot is a cache of the save (P3, #136).** Its record names
  the backup it was taken with and the image it ran; a wake uses it only
  when both match, and a start from one that fails falls back to the
  image and the backup in the same wake.
- **A wake says what it restored (P7, #136).** `restored` and
  `rollbacks` in the computer's view, and a `"restored"` log line.
- **Two lifecycle fixes (P8, #136):** a crash while busy starts the
  computer again; a slow sleep's second ask keeps the first's snapshot.
- **A hold limits what an agent does for its owner, never its own
  memberships** (Paul, with #137): a held agent still records its turns.
- **Step 1, A+ (#147, #148).** Saving is an action of the pure lifecycle:
  when work ends (30 s settle), every 15 minutes busy, at every sleep;
  the hold is a handshake (`held`, within 20 s); three saves are kept and
  a wake falls back to the save before one that will not restore; a sleep
  whose save fails keeps its container for at most
  `computers.unsaved_max_ms` (30 minutes by default, Paul's to confirm).
  Our Hermes image copies every database with SQLite's online backup
  (from Rust: Hermes' own covers a fixed list, and restarts a busy copy)
  and its answer names exactly what it copied, so the save leaves out the
  live files (a database made after the copy is kept hot, not lost); a
  restore puts the copies back and `quick_check`s each before a turn.
  Measured in Docker: none of 54 held databases tore (none of 54 hot ones
  either: the hold makes rarely into never). docs/computers.md.
- **Step 2, the seam (#149).** `/data/work` is the tools' (Hermes'
  terminal's cwd, its home, and its browser's profile, per agent), saved
  as a record of its own; the rest of `/data` is Hermes' home and the
  bridge's state, saved beside it, restored together. An agent's home
  (its terminal's `HOME`, its file tools' `~`) joined it on 2026-10-07
  (Paul: the whole `~`, not only the browser), a hard cut: what agents
  had written under `~` stayed where it was.
- **An agent's temp files joined its work (2026-10-07).** Hermes (v0.21.5,
  and v0.21.6 alike) points `TMPDIR`, `TMP` and `TEMP` at
  `<home>/cache/scratch` for itself
  and for each process it runs, derived again from the home it runs that
  process under (`hermes_constants.apply_scratch_tmp_env`, which leaves
  alone a value it did not set, knowing its own by its
  `HERMES_SCRATCH_DIR` marker). An
  agent's commands run under its profile's home, so a tool's temp file (a
  `mktemp`, a build's, an install's) was in Hermes' home,
  `/data/hermes/profiles/<profile>/cache/scratch` (seen in the Docker lane
  while working on #225). That directory is now a link to
  `/data/work/<profile>/tmp`, made as the link to its home is (a hard cut:
  one with something in it is set aside, unmoved, as
  `cache/scratch.before-work`). A link, not a `TMPDIR` of ours, because
  nothing short of patching Hermes can give each agent one:
  - a profile's `.env` cannot (`terminal.env_passthrough` reads `TMPDIR`
    from the process, never from a profile's scope: Hermes keeps that name
    process-wide);
  - an export in the terminal's `shell_init_files` reaches its foreground
    commands only, never a background process, which starts from the
    gateway's environment;
  - and one set for the gateway is passed to every child as it is (Hermes
    re-points only its own), so every agent would share it.

  Hermes' own path is what each way it starts a command reads, so the
  link catches them all, and Hermes' own uses of it work through it (its
  prune of entries idle 24 hours; a file it sends from a reply's `MEDIA:`
  tag). Two kinds of temp files stay in Hermes' home. The gateway's own
  (`/data/hermes/cache/scratch`: its terminal's snapshots, its browser's
  sockets, execute_code's staging) are Hermes'. And execute_code's
  scripts' are there too, because Hermes' sandbox drops the marker from
  their environment: a debt-ledger entry, until our terminal backend
  (step 3) runs them or Hermes keeps its marker. And the image's Chromium
  opts out of both on purpose: it sets its own `TMPDIR`, the container's
  `/tmp` (#231), so a browser's throwaway profile and shared memory are in
  no save at all. docs/computers.md, "Our images"; the Docker lane's
  `a_tools_temp_files_are_its_work`.
- **Litestream is cut (P4, #150).** Its replicas were never read; the
  saves carry Hermes' databases whole. The S3 endpoint it wrote through
  (`storage.fragment.internal`) went after it (#156): no image used it.
- **The hold is answered once the desktop has drawn (2026-10-06).** On
  the e2e preview every hold after an agent's desktop first drew went
  unanswered (`held: false` after the 20 s), so those saves carried hot
  copies of Hermes' databases. The desktop keeps Mesa's shader cache under
  the profile as `mesa_cache.db`, in Mesa's own format, and our image took
  every `*.db` for a SQLite database: its copy failed, and a failed copy
  answers nothing. Once that was fixed, the next hold refused was a
  running Chromium's: it keeps its own databases (then under Hermes'
  home, the agent's `~`; now in its work) in SQLite's exclusive locking
  mode, so no copy of one could be had. A
  database is now a `*.db` file that begins with SQLite's header, and one
  its owner holds locked through the copy is kept hot, as one made after
  the copy is, while the rest are copied and answered. A guest may say
  why it has not answered (`/run/computer/unheld`), which the DO logs
  with the hold: that is how the second cause showed.
  docs/computers.md, "The hold".
- **The model is told what was cut (P5, 2026-10-06).** A turn a restart
  cut is never redone by the chat's next message (F10, bit on p5: Hermes
  kept the cut request as its session's last message and joined the next
  message to it, so its model did the cut request again). Two parts. The
  first turn an agent runs in a chat after one of its turns there ended as
  lost carries a note, built from the journal alone (`work` and `chat`):
  that a restart cut it and to check what was done before doing any of it
  again, what was asked, the steps and cards it recorded, what it had
  replied; said once, at most 4 KiB, handed to every runtime
  (`TurnStart::note`; Relay's read-only `context`, the scripted agent
  echoes it). And our image's boot closes the cut turn in Hermes' session
  before the gateway starts, with the row Hermes writes itself when a turn
  ends without an answer (its failed-turn boundary), through Hermes' own
  session code, so the next message is a turn of its own. Hermes' own
  recovery notes stay off. docs/bridge.md, "The turn after a cut one is
  told"; docs/chat-records.md; docs/computers.md.
- **What Hermes writes under the hold is chosen (2026-10-07).** Its
  model-catalog refresh is off and Node's compile cache is out of
  `/data`; the rest of what its gateway writes on its own timers is kept
  hot, each named, with why a save cannot tear it (below).

## What changes under the hold (2026-10-07)

The hold promises that no file the save keeps tears. Held, our image
claims no turn, starts no round of its own (repo sync, skills, the
agents' reads), and copies every database as of one moment. Hermes'
gateway is not paused: it cannot be asked, and freezing it is option B.
So its own timers run on through a hold. `held_nothing_under_data_changes`
(images/bridge/tests/docker.rs) failed once on 2026-10-07, in a run with
9 containers starting, when one of those timers landed in its 5 s
window: four caches under `/data/hermes/cache` and Node's compile cache
under `cache/scratch` changed while held. Read from Hermes v0.21.5 (tag
v2026.9.24) and measured idle in Docker, these are the gateway's writers
and what each now does.

| What | Its writer and when | Choice |
|---|---|---|
| `cache/model_catalog.json`, `openrouter_curated_catalog.json`, `nous_recommended_cache.json`, `reasoning_caps.json` | the gateway's `_model_catalog_refresh_watcher` (`gateway/run_watchers.py`): `refresh_catalogs()` 30 s after it starts (written about 47 s in the failing run, the fetch slow), then every 20 minutes | **off**: `model_catalog.enabled: false` in the managed overlay (`hermes.rs`, `managed_config`) |
| `cache/scratch/node-compile-cache/…` | npm turns on Node's compile cache at every start (`enableCompileCache()`), under `TMPDIR`, which Hermes points at its home's `cache/scratch`; its `npx --version` probes run it | **out of `/data`**: `NODE_COMPILE_CACHE=/tmp/node-compile-cache` (Dockerfile); a cache every start rebuilds |
| `state/gateway.heartbeat` | its loop heartbeat, every 30 s | kept hot: replaced whole; no switch |
| `gateway_state.json`, `channel_directory.json` | its housekeeping thread, every 60 s and every 5 minutes | kept hot: replaced whole; no switch |
| `cron/ticker_heartbeat`, `ticker_last_success` (`ticker_last_error`), `cron/.tick.lock`, in its home and each profile's | its cron ticker, every 60 s, though its cron tool is off (decision 38) | kept hot: the stamps replaced whole, the lock empty; no switch (with more than one profile the built-in ticker always runs) |
| `logs/*.log` | appended | kept hot: a save may end mid-line |
| `backups/config/config.yaml.good.<time>`, in its home and each profile's | `load_config()`, the first time a process reads a `config.yaml` whose bytes its newest copy lacks: on a first start, the catalog watcher's tick 30 s in (it reads the config with the catalog off) | kept hot: made once, in place, and read only when that `config.yaml` will not parse, which ours never fail to (the boot writes each whole at every start) |

**Kept hot** means the save keeps whichever version it reads. Hermes
writes each stamp and state as its `atomic_json_write` does: a temp file
beside it, fsynced, then renamed over it. So any reader, a save included,
has the old file or the new one, never a mix. The temp file
(`.<name>_*.tmp`, `.hb_*.tmp`) may be caught mid-write, but nothing
reads one. Each is a stamp of the life that wrote it (a heartbeat, a
status, the ticker's last run), so a wake's copy is stale either way.
None is a fact with an authority of its own (the rule, above). So the
test now holds for at least 65 s, until Hermes' heartbeat and its cron
ticker have both run inside the hold: longer than each of these timers
but the 5-minute one, so every run sees them inside it. It fails on any
other file that changes (our own sync, every 60 s, among them) and
checks that each stamp it saw rewritten parses whole.

**Why the catalogs are off rather than kept hot, left out, or paused:**

- Nothing of ours reads them. Every profile's model is the platform's
  route (`custom`, or `anthropic` for the high tier, each with a
  `base_url`). The catalogs feed Hermes' `/model` picker and reasoning
  hints for OpenRouter's and Nous' routes, nothing else. No person
  reaches the picker: the bridge keeps a leading `/` from reading as a
  command. In v0.21.5 no turn of such a profile reads them: their
  readers (`agent/reasoning_params.py`, `agent/auxiliary_reasoning_floor.py`,
  `agent/turn_recovery.py`) are for those routes, and `gateway/run_turn.py`
  reads one only for a profile that names no model. Ours always name one.
- The refresh was the gateway's only idle egress: four third-party hosts
  (hermes-agent.nousresearch.com, raw.githubusercontent.com, openrouter.ai,
  portal.nousresearch.com), from every computer, every 20 minutes.
- A torn `model_catalog.json` would stop the refresh for good. Hermes'
  read (`_read_disk_cache`) lets the decode error of a file that is not
  UTF-8 out. Its watcher logs that at debug and never rewrites the file.
  Turned off, `get_catalog` returns nothing before it reads the file. The
  one reader on a turn's path that ignores the switch
  (`get_default_model_for_provider`, for a profile that names no model)
  catches the error. Kept hot, the file could
  never tear in a save (it is renamed into place). But keeping it would
  keep the egress and that failure for nothing we use.
- Pausing the refresh while held needs a hook in Hermes' gateway (a
  patch). Leaving the caches out of the save would make every wake start
  them cold, and fetch them again at once.

Turned off, the refresh writes nothing even when forced inside a hold,
and a wake from a save whose four caches are torn answers like any
other: `a_catalog_refresh_forced_under_the_hold_writes_nothing` (the
Docker lane). That test also runs the same refresh with the catalog on,
as a counterfactual: it rewrites `model_catalog.json` under the hold, as
the failing run saw. Two cautions. Hermes reads a managed overlay that
will not parse as empty, which would turn the catalog back on. Ours is
the boot's (unit-tested), and the lane checks that Hermes reads it off.
Also, a save from before this change keeps its stale caches, unread.

## The open problem

With steps 1 and 2 built, `/data` is saved between turns and on a timer,
Hermes' databases whole. A save asked of the guest only covers writers we
know of: a messenger
adapter's SQLite, a browser profile or something a skill installed can
be mid-write. And some state must never go back in time at all: a
messenger's encryption ratchets, which a clean but older copy breaks.
Neither Cloudflare nor Hermes solves that on a disk that can be lost;
Cloudflare restarts hosts on an irregular cadence, even for always-on
containers ([Containers FAQ](https://developers.cloudflare.com/containers/faq/)).

## The options

| Option | How it works | Unknown writers | Never-rewind state |
|---|---|---|---|
| A. Ask the guest to pause | The guest pauses its known writers | torn if writing | no |
| **A+. Cloudflare's intended design** | Save when idle; Hermes' databases copied by its own online backup (`hermes backup`, `sqlite3.backup()`); three saves kept; a wake falls back to one that opens | torn files survived by falling back | no |
| B. Freeze the guest | Stop every guest process for the copy; Cloudflare has no freeze call, so it is image-side, and may stop Cloudflare's own exec path | yes | no |
| C. No authoritative `/data` | Everything that matters lives outside | nothing to tear | once moved out |
| D. C as the direction, B as the mechanism | | B's | C's |
| **E. Hermes' intended design** | Split by writer: the gateway keeps only Hermes' home; its tools run in a separate Cloudflare Sandbox, as a Hermes terminal backend, whose Durable Object starts every command and so knows when it is idle | yes | no |
| **F. Never-rewind state outside the computer** | Messenger connectors in Durable Objects | not its job | yes |

Sources for A+ and E, read 2026-10-05: Cloudflare's
[sandbox lifetime](https://developers.cloudflare.com/sandbox/concepts/lifetime/),
[files](https://developers.cloudflare.com/sandbox/files/),
[auto-save guide](https://developers.cloudflare.com/sandbox/files/save-a-sandbox-automatically/),
[directory backups](https://developers.cloudflare.com/sandbox/reference/directory-backups/)
and [S3 mounts](https://developers.cloudflare.com/sandbox/reference/s3-mounts/);
Hermes v0.21.5 (tag v2026.9.24): `website/docs/user-guide/docker.md`
("the single source of truth"), `session-storage-recovery.md`,
`hermes_cli/backup.py`, `gateway/scale_to_zero.py` and
`website/docs/developer-guide/terminal-environment-plugin.md`.

## The decision: A+ now, toward E (Paul, 2026-10-05)

The shape finite-next's goose agent already had: the loop and its
journal in a durable cell that checkpointed only at idle, the work in
Sprite workspaces that paused warm (docs/finite-next-lessons.md, at the
tag `celld-final`). Five steps, each its own pull request with its
tests, each leaving master whole:

1. **A+ (P2, reframed).** *Built (#147, #148; Litestream cut in
   #150), with two refinements: the copy is SQLite's online backup from
   Rust, since Hermes' own covers a fixed list of files and restarts a
   busy copy; and the save leaves out exactly what the image names as
   copied, never `*.db`, so a database Hermes makes after the copy is kept
   hot rather than lost.*
   - The Computer DO saves `/data` when work ends (the last keepalive
     closes, after a settle), at every sleep, and every 15 minutes of
     activity, as Cloudflare's auto-save guide does. Saving is an action
     of the pure lifecycle (crates/core/src/computer.rs), tested there
     first.
   - Under the hold, the image copies each of Hermes' databases with
     Hermes' own online backup into a staging directory inside `/data`,
     and the save leaves the live `*.db`, `-wal` and `-shm` files out
     (`DirectoryBackup`'s `exclude`). A restore puts the copies back
     before Hermes starts. Nothing pauses Hermes' gateway.
   - The DO keeps the newest three saves. A wake restores the newest;
     the image checks each restored database (`PRAGMA quick_check`)
     before the guest takes a turn; a start that keeps failing on save
     `n` tries `n − 1`.
   - A sleep whose save fails is retried and does not destroy the
     container, within a bound; the computer's view says so.
   - Litestream goes: it is neither vendor's intended store, and its
     replicas are never read (F8).
2. **The seam.** `/data` splits into Hermes' home (its databases,
   sessions, profiles) and a work directory for everything its tools
   write (projects, scratch files, its home, the browser profile), each saved on
   its own. Nothing moves yet; the split is what lets each live
   elsewhere later. *Built (#149): the platform names no runtime, so its
   contract is `/data/work` (the tools') and the rest of `/data` (the
   guest's own: for ours, Hermes' home at `/data/hermes` and the bridge's
   state), two records a save.*
3. **Our terminal backend.** Hermes runs its tools' commands through a
   terminal-backend plugin of ours, at first locally in the work
   directory of the same container. We then know when tools are busy,
   which the save schedule can use instead of the keepalive. execute_code's
   scripts then run through it too, so their temp files join the work.
4. **E.** That backend runs commands in a separate Cloudflare Sandbox
   per computer, whose Durable Object starts every command and saves
   its disk when idle. The gateway's container keeps only Hermes' home.
   To settle first: the latency each tool call gains as a round trip
   through a Durable Object; where the browser and desktop live; the
   credentials swap and the `fragment` CLI inside the sandbox.
5. **Toward C,** as each piece of Hermes' home gains an outside
   authority.

## F: messengers outside the computer

A connector Durable Object per linked messenger account:

- holds the protocol's keys and session state in its own SQLite,
  sealed at rest like every secret, so it never goes back in time;
- receives and decrypts each message and posts it as a record on the
  person's chat, which wakes a sleeping computer the way any chat record
  already does; the agent's reply is a record the connector encrypts and
  sends;
- so the computer never sees the protocol: Hermes talks only to the
  relay, which is also the condition for Hermes' own scale-to-zero.

F does two jobs: it keeps never-rewind state off a disk that can
rewind, and it answers how a sleeping computer hears an encrypted
message. Open questions: encryption now ends in the platform's Durable
Object rather than the person's computer (the same operator either
way, but to be said plainly); a Durable Object holding an outbound
connection is billed while connected (hibernation covers incoming
sockets only); and whether the protocol's library can run on Workers or
needs a small stateless container doing the cryptography while the
Durable Object keeps the state.

**The SimpleX spike (2026-10-05) is parked** (Paul: "too much of a
lift for now"; docs/explorations/simplex-connector.md). It ran end to
end against a local SMP server, but no SimpleX library exists outside
the Haskell app: the practical shape is a small always-on container per
account running `simplex-chat`, and only a pure-Workers client (about
1.5 to 2 months) fully meets "never goes back in time". Not built now.
SimpleX stays on an always-on computer (decision 32), whose disk can
still go back in time at a host restart; the exploration's rollback
runs show `/_sync` heals the connection in about 0.2 s, losing the
messages in flight.

## Deferred, with the recommendation

- **P6, approvals outlive a restart:** for now, an open approval card
  holds the keepalive until it is answered or expires (decision 42
  changes; built 2026-10-05, after Paul's missed card on p5:
  docs/bridge.md, "A card keeps its computer awake"); later, a late
  answer starts a new turn.
- **A backlog's age:** chat messages have no age limit after a long
  outage (routines have an hour). Decide before production.
