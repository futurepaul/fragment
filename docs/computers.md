# Computers: a fragment's own machine

Status, 2026-09-26 (ROADMAP phase E). Built:

- **Computers are identities** (decision 21; docs/api.md, Identities). A
  person pairs a machine with `fragment login --computer <name>`, and from
  then on it signs as that computer, never as them. It works only in
  fragments where it is a member and in the fragments it makes, which are
  its owner's, on its owner's budget. `fragment computers rm` revokes its
  keys and removes its memberships.
- **A fragment declares one** (`"computer": {}`; docs/api.md, Apps), and
  the platform gives it a Sprite: the rest of this page.

## Paul's answers (2026-09-26)

1. **Sprites org:** the org used for Sprites so far. Its token is the
   node secret `FRAGMENT_KEYS_SPRITES_TOKEN`, used only by `KEYS`.
2. **Cost:** list price for awake time and disk, to the fragment owner's
   budget.
3. **Idle policy:** awake while a job step needs it, then asleep 5
   minutes after the last page viewer leaves. Never destroyed
   automatically. Alerts about "accidentally awake" Sprites come later
   (docs/technical-debt-ledger.md).
4. **Grant:** none. Resources are declarative: declaring `"computer"`
   provisions one on deploy, and the owner pays, as for everything a
   fragment spends.
5. **Model calls:** through the platform, signed with the computer's own
   key, on a model from a short list (the platform's by default):
   `fragment model` (below).

## How it works

1. **Declared.** A deploy whose live `fragment.json` has `"computer": {}`
   tells the fragment's `Computer` cell (one per fragment, named by it;
   `cell/src/computer.rs`). A deploy without it tells it too.
2. **Made.** The cell's alarm charges a tick, makes the Sprite through
   `KEYS`, and runs its first boot there: the one-line install of the
   CLI's release, then `fragment login --pair`. A `Computer` cell reaches
   only the Sprite named for its own id (`fragment-` and 24 hex:
   crates/native `sprite_of`), so no cell can touch another Sprite in the
   org.
3. **Paired.** The boot's stdin carries a single-use token the registry
   minted for the fragment's owner (an hour good). The CLI makes its key
   on the Sprite and pairs with the token (`POST /api/computers/pair`,
   signed by that key), so the key never leaves the Sprite. The computer
   is named by its fragment (`pet.paul`), and the platform makes it an
   editor there.
4. **Awake and asleep.** A page opening on the fragment wakes it (a
   computer's own live socket, such as the pet's follower, is no page). Each
   tick (a minute) it is charged first, then held awake through the
   Sprite's Tasks API (a hold that expires after two ticks). Each tick,
   the cell asks the fragment for open pages; 5 minutes after the last
   one closes, the hold is released and the Sprite pauses itself. A job's
   command wakes it the same way (below). A tick that does not fit the
   budget lets it sleep (`computer.budget`).
5. **Billed at list price.** Sprites meter the CPU and memory a Sprite
   actually uses, which the platform cannot see, so an awake hour is
   billed as the idle footprint measured on one: a tenth of a CPU
   ($0.07 a CPU-hour) and 1.5 GB ($0.04375 a GB-hour), $0.0726 an hour.
   Its disk (its home directory, measured each time it wakes) costs
   $0.000683 a GB-hour awake and $0.000027 asleep, charged when it next
   wakes (`fragment_core::budget::computer`).
6. **Kept or destroyed.** Dropping the block lets it sleep and keeps it.
   `fragment computers rm <fragment>` destroys its Sprite and revokes its
   keys. The fragment's next deploy that declares one makes a new one.

The only credential a computer holds is its own key, in its CLI config
(0600) on the Sprite's disk (decision 21). It reaches the model through
the platform: `fragment model "<prompt>"`, or `fragment model --request`
with an OpenAI-style chat request on stdin (`POST
/api/model/chat/completions`, signed by its key). The request names its
model from a short list (`fragment_proto::COMPUTER_MODELS`), or none:
`z-ai/glm-5.3-flashx`, the platform's (the agents' too) and the default,
the high-speed variant of `z-ai/glm-5.3-flash`, which stays on the list
at an eighth of its price; and `typesafe/jev-router`, a router with
variable pricing. Any other model is
refused, and so are OpenRouter's `models` fallbacks. The platform asks
OpenRouter for the provider that answers soonest (`provider: {"sort":
"latency"}`) unless the request names its own; a sort pins no provider, so
OpenRouter's sticky routing keeps a repeated prompt on the provider that
cached it (measured from a Sprite, 2026-09-27: 0.4–0.7 s for a tiny call on
the fastest provider, against 0.5–3 s spread across four; an 11.8k-token
prompt repeated came back 11,712 tokens cached, in 1.3–1.5 s against 4 s,
at a fifth of the cost). The request's `reasoning` and `session_id` go as
they came, and the platform adds no reasoning of its own (this model's
cannot be turned off; `{"effort": "low"}` often spends none); a
`transforms` is dropped. It calls OpenRouter with the owner's own key,
which never reaches the computer, and reserves and settles on the owner's
month as `job.ai` does: $0.20 a call (150,000 tokens in and 4,096 out on
flashx), or $0.50 on the router, whose price is known only once it
answers, each settled to the cost reported. A request is at most 8 MiB
(screenshots), where every other request the platform reads is 2 MiB. Any
computer may call it:
a fragment's own, or a machine a person paired. A streamed request's
chunks are relayed as they arrive, and it settles once, as the stream
ends, to the cost its last chunk reports (docs/api.md).

`fragment model --serve [--port N]` (8765 by default; 0 picks one) serves
the same as an OpenAI-compatible endpoint on this machine alone:
`http://127.0.0.1:N/v1`, `POST /v1/chat/completions` (and OpenRouter's
path, `/api/v1/chat/completions`, for goose's `openrouter` provider),
streamed or not, and `GET /health`. It binds the loopback address only (there is no flag
to bind another), forwards each request's bytes to the platform signed
with the computer's key, sends each once (a call that may have reached
the platform is never made again blind), and relays the answer's status
and bytes as they arrive. Whatever speaks to an OpenAI-compatible
provider (goose's `openai` provider with `OPENAI_HOST` pointed at it)
then needs no key: any key it sends is ignored, and the platform refuses
a model past its list. Any process on the machine may call it, on the owner's
budget: a computer is one person's machine (a Sprite has one user). Each
call is one line of JSON on its stderr, `{call: {at, status, ms,
first_ms, model, provider, prompt_tokens, cached_tokens,
completion_tokens, cost}}`: how long to the answer's head and to its end,
and what the answer's usage says (read from its last chunks), so a
computer's `~/.fragment/agent/model.log` shows each call's latency,
provider, and cache.

## A job's commands (`job.computer.exec`)

A job runs `bash -lc <command>` on its fragment's computer (docs/api.md,
Jobs and triggers), in shell over `KEYS`' exec only
(`fragment_core::computer`), so it needs nothing of the CLI's release
beyond pairing. The start wakes the computer as a page does, asking it
awake through a poll's wait (110 s, or the idle wait if longer), and
waits up to 20 s for the alarm to hold it (its first tick paid, or
refused: a `StepError`). It then makes `~/.fragment/exec/<id>` (the run's and
step's id, never the attempt's) as its lock and starts a detached runner
(`setsid nohup`) that records its pid, runs the command with its output
in files, stops it at its timeout, keeps 256 KiB of each stream, and
writes its code last; a start that finds the directory starts nothing.
Each poll asks the computer awake the same way and for the idle wait
after it, waits on no alarm step (an exec reaches a Sprite that paused),
waits there up to 100 s for the code (a blank line every 10 s), then
reads the output in chunks that fit `KEYS`' 64 KiB answer. No code
and no runner, or 60 s past the deadline, is interrupted: it never runs
again. Every answer is base64 between marker lines (whatever else a
Sprite's exec adds is dropped). The journal is kept 30 days, as long as a
run can be replayed.

## Its files, and `start`

The `Computer` cell keeps what live declares (its commit, and `start`)
and what the computer last synced, in its own `files` table. After the
first boot, and on any tick awake while they differ (a deploy while it is
awake arms the alarm at once), one exec runs `exec::SYNC`: a CLI older
than the platform expects (`fragment_core::computer::CLI_VERSION`, the
CLI's own version at the cell's commit: a test holds them together) is
replaced from the release, `fragment sync
<name> --dir ~/fragment --live` pulls live, and `~/.fragment/start.sh`
(a job script for `start`) and `serve.sh` are written. Then `sprite-env
services delete fragment`, and `create fragment --cmd bash --args
~/.fragment/serve.sh`: a Sprites service, so the runtime keeps it up
across cold boots and restarts it if `serve.sh` dies. `serve.sh` itself
runs `start` again when it exits, with backoff, logs to
`~/fragment.log`, and on TERM stops `start` and its children. A deploy
while it is asleep is synced when it next wakes: `start` cannot run while
it sleeps anyway. Only awake time is billed (the seconds of a boot's sync
are the boot's). Then the hands (below) are written, and their service
made if it is not there (`exec::HANDS`). What a computer last synced
names the hands' goose, the CLI, and which hands' service and task client
(`exec::hands_digest`) too, so one synced before them, or with another
goose, an older CLI, or other hands, syncs again at its next tick awake or
when it next wakes: a platform deploy that changes the task client brings
it to every computer (before 2026-09-27 only a deploy of the computer's
own fragment did).

**Its CLI stays current** (fragment.club, 2026-09-27: a pet's hands ran
CLI 0.11.1's `model --serve`, which missed goose's model path, until they
were restarted by hand, after the CLI was updated by hand). Each sync
compares `fragment --version` with `CLI_VERSION`, which the cell sends;
an older CLI is replaced by the release's tarball (the one-line install's
URL, `latest`) once the binary in it runs and says a newer version, moved
into place whole; a CLI is never replaced by an older one. The release
publishes no checksum file, so that is what is verified: HTTPS from
GitHub, and the version the new binary says. The hands notice a CLI
changed under them (its inode, size, and time, looked at each second, as
their script's checksum is), whoever changed it, and run again on it;
`start`'s service is made again after every sync anyway. A release older
than `CLI_VERSION` (a cell deployed before its CLI's release) leaves the
newest it has, and the sync fails, saying so (`computer.failed`), to be
tried again when it next wakes.

**A task across a restart of the hands.** A job's command runs detached
from both services (`job.computer.exec`), so its polls are unaffected. The
task client waits for goose (its `port`, up to 3 minutes), so a task that
starts while the hands restart waits for them. One in progress loses its
connection: the task client prints goose's words so far and that goose
serve closed the connection and the task did not finish, and exits 1, so
the job's answer says so (`code` 1; the builder records the build failed,
and a hand-off's chat gets that message). It is not retried: goose may
have acted partway. goose keeps the session, so the chat's next task
carries on from it. A sync that restarts the hands holds a waking job's
command until it ends (the alarm holds the computer after its sync), so
only a deploy while a task runs, with a new CLI or a new `hands.sh`, cuts
one short.

**Holding it awake from the service** (not built). The cell holds the
Sprite with the Tasks API, a tick at a time, and whether that keeps a
real Sprite running between ticks is the next smoke's to show. Sprites
also counts an open connection to a service as activity, so a `start`
that serves the page could be what holds it: made with `--http-port`,
the page's open connection to the Sprite's URL (the pet's screen) keeps
it awake while the page shows it, and it pauses when the page goes. The
cell would still charge a tick at a time while its fragment has viewers,
and would hold it with the Tasks API only for a job's commands.

## The hands: goose on every computer (`goose serve`)

Every declared computer runs goose, one long-lived session per chat
(docs/agent-computer.md, slice 1; ROADMAP decision 24). A job hands a
task to it and waits; the pet's `do` and the builder's `build` do.

- **goose v1.52.0**, the upstream release
  (`goose-x86_64-unknown-linux-musl.tar.gz`, from
  github.com/aaif-goose/goose: static, one file, nothing to build or
  host), checked against its SHA-256 (`fdc86653…9f9f`, the digest GitHub
  lists for the asset) and unpacked whole to
  `~/.local/share/goose-1.52.0` on the service's first start
  (`fragment_core::computer::GOOSE_VERSION`, `GOOSE_SHA256`). v1.52.0
  turns off the tool-pair summaries that rewrote v1.50.0's history, so a
  session's prefix holds between compactions.
- **The service** (`~/.fragment/agent/hands.sh`, the Sprites service
  `hands`, beside `start`'s): `fragment model --serve --port 0`, and
  `goose serve` on it, kept up together with backoff (`hands.log`), each
  logging to `model.log` and `goose.log` (each cut to its last 512 KiB
  past 1 MiB). goose speaks ACP (a WebSocket at `/acp`) on the first free
  port from 3284, which it writes to `port` once `/health` answers, to
  clients holding `secret` (made on the machine, 0600). Its environment:
  `GOOSE_PROVIDER=openrouter` with `OPENROUTER_HOST` at the model
  endpoint (its OpenRouter path, `/api/v1/chat/completions`) and a key
  that is never used, `GOOSE_MODEL=z-ai/glm-5.3-flashx` (which reads
  images, as goose's catalog knows), `GOOSE_CONTEXT_LIMIT=64000`
  (compaction near 51k), `GOOSE_MODE=auto`, `GOOSE_DISABLE_KEYRING=1`,
  `GOOSE_DISABLE_SESSION_NAMING=1`, `GOOSE_MAX_TOKENS=4096` (a model
  call finishes within the platform's 120 s), and
  `OPENROUTER_PARAMETERS={"reasoning":{"effort":"low"}}`. Its hints are
  the CLI's guide (`fragment guide > ~/.config/goose/.goosehints`, each
  start), and its extensions its default (the developer tools) and what
  `~/.config/goose/config.yaml` enables (the pet's browser and Cua
  Driver): a new session takes the config's, and goose loads a session
  with those it keeps, so the task client brings them to the config's
  (below). A script the platform changed restarts itself at its next
  second, and so does one whose CLI changed (above).
- **The task client** (`~/.fragment/agent/task.mjs`, Node; written with
  the service): one task, `PROMPT`, in the session of `CHAT`
  (`<fragment>/<channel>`; none: the job's own fragment's `work`). The
  chat's session id is kept in `sessions/<chat>`: the first task makes
  it (`session/new`, in `~/chats/<chat>`), each later one loads it
  (`session/load`), and one goose lost is made again. A loaded session's
  MCP extensions are made the config's enabled ones where they differ,
  in their whole definition or in the tools goose offers the model
  (goose's `_goose/unstable/…/extensions/list`, `add`, `remove`, and
  `tools/list`: one cache break per tool change; each change a line of
  `hands.log`; an extension's `envs` do not carry over, so a config puts
  them in its command: docs/agent-computer.md). It sends
  `session/prompt`, and posts each tool call, once it ended, as a
  `turn.step` on the chat's `work` (`fragment post`, as the computer:
  docs/api.md, the chat template), trying a failed post again for a
  minute. It prints goose's last words; it exits 0 when goose ended its
  turn, and 124 past `TIME_S` (540), when it cancels the prompt.
- **Requests go as goose made them**: nothing between goose and the
  platform edits one, and goose sends its session's id as `session_id`,
  so a chat's calls stay on the provider that caches its prefix (the
  platform passes it on and drops `transforms`; docs/api.md).

goose serve makes an agent for each connection (its source:
`AcpServer::create_agent`), and each task is a connection, so a session
is loaded afresh for each task. Its system prompt names the hour
("so that prompt cache can be used"): within the hour a task's first
call is cached as the last task's; the first call after the hour turns
misses once.

## The builder template

`templates/builder` is a fragment that builds fragments: it declares a
computer, and its job `build({task, chat?})` hands the task to the
computer's hands, whose shell has the `fragment` CLI (signed in as the
computer), so goose makes and deploys a new fragment, its owner's. Each
step is a durable `job.computer.exec`:

1. **What there is**: `fragment list`.
2. **The task**, capped at 10 minutes: the task client, in the session
   of the chat that asked (`chat`; none: the builder's own), with a
   prompt that says to build it as a new fragment and deploy it.
3. **What it built**: the fragments listed after the task that were not
   listed before, each with its URL and whether it is live (`fragment
   status`). The run answers `{url, built, message, code}`, `message`
   being the end of what goose said.

The e2e (`builder`) runs every step, and the hands' service, on the
Sprites fake with a stand-in for goose (the e2e binary run as `goose`,
seeded where the hands install the pinned release): `goose serve`, an
ACP WebSocket whose sessions persist in its home, asking the model
through `fragment model --serve` as goose's OpenRouter provider does,
streaming, with a `shell` tool, and the OpenRouter fake's script calls it
to `fragment create` and `deploy`. It proves the plumbing, not goose;
the first real run on a Sprite proves goose.

## Running the first real one

`fleets/fragment-club.json` names the token's file
(`~/.config/finite-next/secrets/sprites-token`, in `node_secrets`). Until
that file exists, a deploy leaves the secret out and says so
(`xtask/src/deploy.rs`, `OPTIONAL_NODE_SECRETS`): everything else deploys,
and a fragment that declares a computer waits (`computer.failed`, "not set
on this node", retried at least hourly). Once Paul makes the file, a
`cargo xtask deploy fragment-club --secrets` sets it (and restarts the
Machines), and the node and cell deploy as usual. Then, as one person, on a scratch fragment:

1. `fragment create pet-smoke`, then deploy a folder whose
   `fragment.json` is `{"computer": {}}`.
2. Within a minute or two, `fragment events pet-smoke` shows
   `computer.ready`, `fragment computers` lists `pet-smoke.<you>`, and
   `fragment members list pet-smoke` shows it as an editor. A
   `computer.failed` event names the step and Sprites' answer.
3. Open the fragment's page: `sprite list` shows its Sprite running.
   Close it: 5 minutes after, it is warm, then cold. `fragment budget
   usage` shows `computer.awake` rows.
4. `fragment computers rm pet-smoke.<you>`: the Sprite is gone from
   `sprite list`.

Things only the real one shows: the exec answer's shape (the cell reads
the last number `du -sk` printed), the install's time (the exec waits up
to 180 s), and whether `sprite-env curl` holds it awake from an exec. For
`job.computer.exec`: that the detached runner outlives the exec that
started it (`setsid`), and that a poll's 100 s exec, a blank line every
10 s, comes back whole. For `start`: that `sprite-env services create`
and `delete` behave as the docs say (the cell reads only the exec's
status), and how the runtime stops a service (TERM to `serve.sh`, which
stops `start`).

### What the first one showed (2026-09-26)

It booted, paired, woke for a page, slept, answered `fragment model`,
and was destroyed. Fixed since:

- **Failed boots were charged.** A boot charged a tick before asking
  Sprites anything, so each attempt against a bad token charged one. Now
  a tick is held on the month before the Sprite is, and settled only
  once the Sprite held (a boot: once its command ran, for the time it
  ran); a step that fails first gives it back (`ledger` `hold`, then
  `settle` or `release`).
- **The idle wait was short** (about 4 minutes): it ran from the last
  tick that saw a page open, up to a tick before the page closed. Now a
  page's close starts it (the fragment tells its computer), and the
  alarm comes at its end, not at the next tick.
- **The hold looked broken.** Sampling `status` every 8 s showed a held
  Sprite `cold` for a moment, and, after release, `warm` and `cold`
  alternating about once a minute. The platform makes no Sprites call
  after a release (the e2e counts them), so the alternating is Sprites'
  own, or the sampling's. Whether the hold took is now checked once each
  wake (`sprite-env curl /v1/tasks/fragment` must show `expires_at`; if
  not, the fragment's events say `computer.unheld`, with the answer).
  If the hold takes and the Sprite still pauses, the Tasks API is not
  the mechanism for a CUA pet: an open exec session (a websocket exec
  that keeps running after disconnect, `max_run_after_disconnect`) or a
  Service with an open connection counts as activity too, and the pet's
  own desktop service, holding a connection while a page shows it, is
  the natural holder. That is the pet-desktop PR's choice.

### What a restart showed (2026-09-27)

After both nodes restarted (celld v0.6.0), Paul's pet did not wake: its
events said `computer.failed: no such reservation`, again and again.
celld fires an alarm again when its handler outlives the node's operation
deadline (15 s; cold cells after a restart, a Sprite slow to wake), while
the first still runs. The second wake took the first's hold on the month
for one a dead wake had left, and gave it back; the first then held the
Sprite and settled a hold that was gone. Now a `Computer` cell runs one
alarm step at a time (`stepping`): one fired again waits, then steps from
the row the one before it left. The e2e (`sprites`) wakes one whose Sprite
answers its hold past the deadline.

### What a long command showed (2026-09-27)

Paul's pet ran `do({task})`: its goose command finished on the Sprite
after about 7 minutes (the journal's `code` 0, 403 bytes of stdout), and
8 minutes later the run was still `running`, with no events after
`run.started`. On the Sprites fake, a command is never collected when its
Sprite answers each hold past the node's operation deadline (e2e
`computer-runtime`), three ways. The start asked the computer awake for
the idle wait alone, shorter than the wake it waited on, so the alarm
fired again let it go as soon as its hold was paid, and every start
waited its 20 s in vain. Each poll waited the same way on an alarm queued
behind slow holds (`stepping`). And an alarm step saved the row it read
before its Sprites calls, undoing the wake a poll asked for meanwhile, so
the computer was let go mid-command (a real Sprite then pauses). Now a
command asks its computer awake through a poll's wait, a poll waits on
no alarm step, and a step saves over what was asked while it ran (a later
wake, `declared`, a removal). What the fake does not show is why the
pet's run stayed `running` rather than held: a poll that fails is retried
four times, then fails the step, which the run's events would say. The
node's logs for that hour, or the next long command on a real Sprite,
show whether a poll's 100 s exec comes back.

## How a page shows it: the pet (`templates/pet`)

A computer everyone who has the fragment watches live and drives
together, built only from what any fragment may declare:

- **Its computer** is declared with `"start": "node computer/pet.mjs"`
  (a Sprite has Node 24). On its first start the script installs what is
  missing (apt with `sudo -n`: `xvfb openbox xdotool imagemagick
  fonts-liberation fonts-noto-color-emoji`, and for its agent's Cua
  Driver `libxi6 at-spi2-core dbus`), and **Google's Chrome**
  (`google-chrome-stable` 154.0.8037.57-1, stable since 2026-09-22: its
  .deb from Google's pool, checked against the SHA-256 Google's apt index
  lists, `66c0645f…5a3e`, installed with apt, which brings its
  libraries, and without the apt source it would add, so it stays the
  pinned one). Ubuntu's own browser is a snap, and Stagehand and Cua
  Driver both want a system one. Then it runs a 1024×640 display, a
  session bus (`~/.pet/bus`), and Chrome on the fragment's
  `computer/start.html`, with CDP on loopback (port 9222) for its
  agent's Stagehand: `--enable-unsafe-extension-debugging`, and
  `--remote-allow-origins` naming only Stagehand's runtime (an unpacked
  extension whose id is its path's hash; its service worker opens its own
  socket to CDP, which Chrome otherwise refuses).
- **Private by default**: a template that declares a computer starts
  `members` when made on the platform (its owner pays while anyone has
  it open), until they share it (`first_visibility`).
- **Driving** is a `control` channel viewers post to (decision 18):
  `{kind: "click", x, y}` in screen pixels (the page scales its click),
  `{kind: "type", text}`, `{kind: "key", key}`, `{kind: "open", url}`. The
  computer follows it with the CLI (`fragment channel <name> control
  --follow`) and applies each record once, by seq, with xdotool. Its
  cursor (`~/.pet/applied`) moves before it applies a record, so a crash
  skips one rather than applying it twice. It skips records older than
  30 seconds. `control` says `"signedIn": true`: on a `link` fragment an
  anonymous link holder is a viewer, and the platform refuses their post
  (401), so every record names an identity.
- **Frames** go through `frame`, an ephemeral mutation editors call (the
  computer is one), with `fragment call`: a JPEG of at most 85 KB as
  base64, when the screen changed, at most one a second for a minute
  after someone drives it and one each 5 seconds otherwise. The app keeps one row (the
  latest frame, what is on screen, who drove it last), which `screen`
  answers, and every page follows it live. Not a blob: a page cannot read
  one (the site serves files at live), and a frame a second would keep a
  blob a second for the week's grace. Not a channel: a channel is
  history, and a record is at most 64 KiB. A frame goes from a file
  (`--input @file`): as an argument, Linux caps it at 128 KiB.
- **Awake** is the platform's to decide; the pet holds nothing itself.
  Its follower is a live socket, but a computer's socket is no page: its
  opening and its close neither wake the computer nor hold it. If the
  Tasks API hold turns out not to keep a Sprite from pausing, the pet's
  service is where a holding connection would go (above).
- **Its limit:** `frame` is ephemeral (`"ephemeral": true`), so its calls
  leave no ledger row: the app's database holds the one row however long
  it is driven (before, each frame's id was kept a week, and a pet driven
  nonstop filled the 16 MiB within a day). What is left is the path: each
  frame is a whole JPEG of up to 85 KB through a mutation, and every open
  page re-runs `screen` after it, so a frame a second costs about 85 KB a
  second per viewer. Streaming the screen to pages directly is the end
  state (being scoped); this is the quick fix.

## Your computer on the desktop, and your agent on it

The desktop (`templates/desktop`, ROADMAP phase F) lists your computers
in its sidebar: the fragments New computer named (`computer-…`, made from
the pet template through `__fragments`, as New chat makes a chat),
wherever they were made. It tells them by their names, so reading the
list wakes none of them; the platform's list carries no flag for them.
Opening one shows its page as a pane, framed like any app; the open page
is what wakes the computer, so it is awake while its pane is open (the
viewer collapsed included) and asleep 5 minutes after it closes. Awake
all day, one would spend a $20 month in about 12 days ($0.0726 an hour).

The pet declares one job for commands, `run({command})`: editors only
(its owner, their agent, the computer itself), `bash -lc` on the computer
through `job.computer.exec`, answered with `{code, stdout, stderr,
truncated}`. A viewer is refused. Your own agent reaches it from any chat
through the platform's verbs, `platform__operations` and `platform__call`
(it is no member of the pet, so no tool of its own names it; decision 17
caps it at editor there), and a job's call answers once its run ends
(docs/api.md, Agents), so it reads what the command printed. It calls
`do({task})` (below) the same way, to hand work to the pet's own agent.

## The pet's agent: `do`

The pet's computer is one machine that people and its agent both drive.
`do({task, chat?})` is an editor-only job (the agent spends its owner's
budget; signed-in viewers still drive by hand): the computer's hands do
the task on the display the pet frames, the web through Stagehand in its
Chrome and other apps through Cua Driver, so everyone watching sees each
click and anyone may click in between. Its steps, each durable:

1. **Installed once** (`job.computer.exec`, 3 minutes): **Cua Driver
   v0.28.3** (github.com/trycua/cua, MIT; released 2026-09-24, past the
   two-day cooldown), its release asset
   `cua-driver-rs-0.28.3-linux-x86_64-binary.tar.gz` checked against the
   SHA-256 GitHub lists (`51de56e3…6fcf`), unpacked whole, as it ships,
   to `~/.local/share/cua-driver-0.28.3` (it keeps its cursor-theme
   helper beside its binary; it links libXi, which `pet.mjs` installs
   with the display). Not the one-line `install.sh`: it fetches a second
   script from cua.ai unpinned, takes the newest release, and edits
   shell rc files. Its screenshots' long edge is capped at 768
   (`~/.cua-driver/config.json`; its default, 1568, was over the pet's
   whole screen). **The browser's server**
   (`computer/browser/browser-mcp.mjs`, below) and Stagehand 4.1.0: `npm
   ci --ignore-scripts` from `computer/browser/package-lock.json` (41
   packages under Stagehand, each pinned and checked by its `integrity`,
   all past the cooldown, none with install scripts) into
   `~/.local/share/pet-browser`, only when the lockfile changed. Then
   goose's config names both as extensions: stdio `cua-driver mcp` (on
   Linux it owns its runtime and ends with its session), offering 8 of its
   62 tools, those for desktop apps (`launch_app`, `list_windows`,
   `get_window_state`, `click`, `type_text`, `press_key`, `hotkey`,
   `scroll`: each is described at length, and every model request
   carries the tools offered), with `DISPLAY=:99`, the pet's session bus,
   telemetry off, and no update checks (in its command, `/usr/bin/env
   … cua-driver mcp`, which a loaded session takes too); and `browser`,
   the server on the Node a job finds.
2. **The task** (capped at 10 minutes): the hands' task client, in the
   session of the chat that asked (none: the pet's own page's), the task
   after a paragraph on the screen and which tools are for what: the
   browser's for anything on the web, Cua Driver's only for other apps
   (goose's system prompt stays its own).
3. **Its answer**: `{message, code}`, goose's last words and the task's
   exit code.

**The browser** (docs/agent-computer.md, slice 2) is a stdio MCP server,
JSON-RPC by hand like the task client, whose tools are `open` (an
address, answered with the page's title), `act` (one thing, said
plainly), `observe`, `extract` (text off the page), and `screenshot`
(for when the look matters). Stagehand, in local mode, attaches to the
pet's Chrome over CDP at the first call and loads its runtime extension
there; each step reads the page's accessibility tree, and none sends the
model a picture. act, observe, and extract reason through the hands' own
endpoint (the server inherits goose's `OPENROUTER_HOST`, `fragment model
--serve`: signed as the computer, billed to its owner), unstreamed, the
schema said in the system prompt. One setting in the server,
`JEV_FIRST`, picks the models: flashx alone (the default), in JSON mode
(its one provider takes no schema; `provider: {sort: "latency",
require_parameters: true}`) with `reasoning.effort: low`; or Jev
(`typesafe/jev-router`) first, asked for Stagehand's JSON schema as
`response_format`, with no reasoning effort (it picks its own, and
refused one named) and no provider preferences (it may pick a stealth
model, which Paul accepted), then flashx for the same request when Jev's
answer does not fit or Jev is refused (a Jev call holds $0.50 of the
month until it settles). `computer/browser/answer.mjs` reads an answer:
the JSON in it, a schema's only field given bare, or words where that
field is text. Each call is a line of `~/.fragment/agent/browser.log`
(`{model, by, schema, ms, ok, why}`, `by` the model that answered),
beside `model.log`'s, which is what settles the setting
(docs/agent-computer.md, Browser fixes). Stagehand's own traces, which
go to example.com unless told otherwise, go to a closed loopback port.

**Screenshots reach the model, and stay.** goose sends an MCP tool's
image (Cua Driver's `get_window_state`, the browser's `screenshot`) as a
user message after the tool's (`image_url`, a data URL) to a model its
catalog lists as reading images, which flashx is
(`openrouter/z-ai/glm-5.3-flashx`); the platform passes image content
through unchanged. Nothing trims a request: a request is at most 8 MiB
(the CLI's `--serve` and the cell), goose compacts its session near 51k
tokens, and it compacts on an image-limit error too.

**Its steps are live**: on the pet's own `work` channel (viewers read,
editors post) the job publishes `{run, kind: "start", task, asker}` and
`{run, kind: "end", code, message}` (or `error`); each tool call is a
`turn.step` the computer posts on the asking chat's `work`, or the pet's
own when its page asked. The page shows the latest run's.

**Who drives it**: each step also writes the asker's id to
`~/.pet/agent` (the task client's `MARK`); `pet.mjs` names
`agent:<asker>` as the driver once that file is newer than anyone's last
`control` record, so the page says "last driven by its agent, for
@someone" until a person drives it again.

The e2e (`templates`) runs it on the Sprites fake with stand-ins for
goose and Cua Driver (the e2e binary, where the pinned releases go) and
for Stagehand (a module where the job installs it, beside the pet's own
lockfile, so nothing is installed; its act, observe, and extract ask the
model once each through the `generate` callback, with a fixed tree as
the page). goose's stand-in keeps each session's extensions as goose
does (a new session takes the config the job writes, with goose's ACP
methods to list, add, and remove them), starts its MCP servers (the
browser's is the real one), offers its shell and their tools, and sends
screenshots on as goose does. A session made on an older, Cua-only
config gets the new tools on its next task, in the same session, and
the task after changes nothing. It
checks the refusal for a viewer, the answer, goose's model calls
(through the platform, on flashx, offered the browser's tools and Cua
Driver's desktop ones), a browser task's steps (`open`, `extract`,
`observe`, `act`) reaching the model through the hands' endpoint, on
flashx in JSON mode, its markdown list and bare array of elements read
as Stagehand's shapes, and in `browser.log`; then, `JEV_FIRST` turned on
in the pet's files, Jev with Stagehand's schema and no reasoning effort
(the OpenRouter fake refuses one, as Jev did) or provider, flashx
taking over when Jev's JSON is cut short, billed to the owner; that no
browser step sends a
screenshot, while Cua Driver's reaches the model, capped; the click on
`:99`, the steps on `work`, and the driver. It proves the plumbing, not
goose, Stagehand, or Cua Driver.

**What only a real Sprite shows**: that goose v1.52.0 loads the
extensions from the JSON `config.yaml` and offers their tools; that its
ACP updates are the shapes the task client reads (`tool_call` with
`_meta.goose.toolCall.toolName` and `rawInput`, `tool_call_update`,
`agent_message_chunk`), and its extension methods' the shapes it
compares (read from goose v1.52.0's source); that Chrome installs and
Stagehand's runtime loads in it there (on a local Chrome 153, Stagehand
4.1.0 through this server opened a page, filled and clicked, observed,
and extracted); how often flashx's answers, and Jev's, fit Stagehand's
schema, and how soon (`browser.log`), which settles `JEV_FIRST`; a
browser step's time; that `cua-driver mcp` starts under Xvfb and
openbox, and its clicks land in a desktop app; how many screenshots fit
before compaction at 64k; and how well flashx drives a screen.

## Your agent hands its work to a computer

A person's agent builds nothing in its cell (Paul, 2026-09-27; docs/api.md,
Agents, Hand-offs). It answers questions and makes a few calls on your
fragments itself (a todo added, a list read); anything longer it hands to
a computer's hands with `platform__hand_off({task, computer?,
throwaway?})`, in your own turns only, and its turn ends there: the
platform says the work is on its way, and the agent's model is not asked
again, so it cannot make up a result it has not received.

- **Your home computer, by default**: `fragment agent home agent.<you>
  <computer>`, once, names one of your fragments whose job `do` (or
  else `build`) takes a task: a pet, or a builder. A chat's first
  hand-off binds it there, and each later one goes to the same session,
  so the computer remembers what the chat handed it before.
- **A computer you name**: the same, for that hand-off.
- **A throwaway**, when asked for (extra hands), or when you have no home
  computer: a private builder of yours, `handoff-<12 hex>.<you>`, made
  for the task. Its computer boots, goose runs `build` and deploys what
  it made as a fragment of yours, and when the run ends the agent removes
  the throwaway: its computer (keys revoked, the Sprite destroyed), then
  the fragment. What it built stays. A crash's leftovers are recognized
  by that name, but the name grants nothing: the platform recorded the
  throwaway as the agent's when it was made, and only that lets the
  agent delete it.

The computer is made an editor of the chat, and posts each step there as
your computer, then goose's answer, under the steps, with no one asking
again. The agent's alarm watches the run, and its result reaches the
agent's conversation as a note (the task, what the computer said, a
build's URL), so later questions can build on it; the agent says in the
chat only what the computer could not (a run that failed). A throwaway
costs what any computer does: its boot, and its awake time while the
build runs, on your budget.

## Next

- **A hand-off on a real Sprite**: a throwaway's boot, goose, and its
  Sprite destroyed after (`sprite list`). The e2e proves the plumbing
  with the stand-in goose.
- **The hands on a real Sprite** (docs/agent-computer.md, slice 1's
  acceptance): two hand-offs from one chat share a session, the second's
  first call at least 80% cached; a median step of 3 s or less, no gap
  over 10 s; another chat gets its own session; after a restart, a
  chat's next task recalls its earlier one. `model.log` has each call's
  time, provider, and cached tokens, and the chat's `work` each step's
  time. Its CLI is brought to 0.12.0 at its next sync (`model --serve`
  answering OpenRouter's path; an older one failed each call: `goose.log`).
- **The builder on a real Sprite**: `fragment new b --template builder`,
  create, deploy, and a task from its page.
- **The pet's agent on a real Sprite** (`do`, above): a task from the
  pet's page, and the list of what only a real Sprite shows. Slice 2's
  measure: a web task (a price found, added to a todo) whose model calls
  in `model.log` carry no screenshot, each call's ms and cached share,
  and in `browser.log` how many of 20 Jev calls fit Stagehand's schema.
  A pet made before slice 2 keeps Playwright's Chromium in
  `~/.cache/ms-playwright` until someone removes it (`run`).
- **Alerts** about computers awake longer than expected, and what a
  deleted fragment's computer becomes (today it stays, asleep, until
  `fragment computers rm`).
