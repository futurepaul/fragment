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
   key, on the model the agents in cells use: `fragment model` (below).

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
/api/model/chat/completions`, signed by its key). The platform forces the
agents' model (`fragment_proto::AGENT_MODEL`), calls OpenRouter with the
owner's own key, which never reaches the computer, and reserves and
settles on the owner's month as `job.ai` does. Any computer may call it:
a fragment's own, or a machine a person paired. A streamed request's
chunks are relayed as they arrive, and it settles once, as the stream
ends, to the cost its last chunk reports (docs/api.md).

`fragment model --serve [--port N]` (8765 by default; 0 picks one) serves
the same as an OpenAI-compatible endpoint on this machine alone:
`http://127.0.0.1:N/v1`, `POST /v1/chat/completions`, streamed or not,
and `GET /health`. It binds the loopback address only (there is no flag
to bind another), forwards each request's bytes to the platform signed
with the computer's key, sends each once (a call that may have reached
the platform is never made again blind), and relays the answer's status
and bytes as they arrive. Whatever speaks to an OpenAI-compatible
provider (goose's `openai` provider with `OPENAI_HOST` pointed at it)
then needs no key: any key it sends is ignored, and the platform picks
the model. Any process on the machine may call it, on the owner's
budget: a computer is one person's machine (a Sprite has one user).

## A job's commands (`job.computer.exec`)

A job runs `bash -lc <command>` on its fragment's computer (docs/api.md,
Jobs and triggers), in shell over `KEYS`' exec only
(`fragment_core::computer`), so it needs nothing of the CLI's release
beyond pairing. The start wakes the computer as a page does and waits up
to 20 s for the alarm to hold it (its first tick paid, or refused: a
`StepError`). It then makes `~/.fragment/exec/<id>` (the run's and
step's id, never the attempt's) as its lock and starts a detached runner
(`setsid nohup`) that records its pid, runs the command with its output
in files, stops it at its timeout, keeps 256 KiB of each stream, and
writes its code last; a start that finds the directory starts nothing.
Each poll waits there up to 100 s for the code (a blank line every 10 s),
then reads the output in chunks that fit `KEYS`' 64 KiB answer. No code
and no runner, or 60 s past the deadline, is interrupted: it never runs
again. Every answer is base64 between marker lines (whatever else a
Sprite's exec adds is dropped). The journal is kept 30 days, as long as a
run can be replayed.

## Its files, and `start`

The `Computer` cell keeps what live declares (its commit, and `start`)
and what the computer last synced, in its own `files` table. After the
first boot, and on any tick awake while they differ (a deploy while it is
awake arms the alarm at once), one exec runs `exec::SYNC`: a CLI from
before `sync --live` updates itself from the release, `fragment sync
<name> --dir ~/fragment --live` pulls live, and `~/.fragment/start.sh`
(a job script for `start`) and `serve.sh` are written. Then `sprite-env
services delete fragment`, and `create fragment --cmd bash --args
~/.fragment/serve.sh`: a Sprites service, so the runtime keeps it up
across cold boots and restarts it if `serve.sh` dies. `serve.sh` itself
runs `start` again when it exits, with backoff, logs to
`~/fragment.log`, and on TERM stops `start` and its children. A deploy
while it is asleep is synced when it next wakes: `start` cannot run while
it sleeps anyway. Only awake time is billed (the seconds of a boot's sync
are the boot's).

**Holding it awake from the service** (not built). The cell holds the
Sprite with the Tasks API, a tick at a time, and whether that keeps a
real Sprite running between ticks is the next smoke's to show. Sprites
also counts an open connection to a service as activity, so a `start`
that serves the page could be what holds it: made with `--http-port`,
the page's open connection to the Sprite's URL (the pet's screen) keeps
it awake while the page shows it, and it pauses when the page goes. The
cell would still charge a tick at a time while its fragment has viewers,
and would hold it with the Tasks API only for a job's commands.

## goose on a computer: the builder template

`templates/builder` is a fragment that builds fragments: it declares a
computer, and its job `build({task})` runs goose there, whose shell has
the `fragment` CLI (signed in as the computer), so goose makes and
deploys a new fragment, its owner's. Each step is a durable
`job.computer.exec`:

1. **goose, installed once**: the upstream CLI release **v1.50.0**
   (`goose-x86_64-unknown-linux-musl.tar.gz`, from
   github.com/aaif-goose/goose), checked against its SHA-256
   (`ff8c5142…c84af`, the digest GitHub lists for the asset) before it
   is unpacked to `~/.local/bin/goose-1.50.0`. Why this one: a release
   is a single file with nothing to build or host; the musl build is
   static, so it runs on any x86_64 Linux whatever its libc; v1.50.0 is
   18 days old (the two-week cooldown), and it is the release the
   repo's goose fork (`futurepaul/goose` at `12922e7`, decision 10) is
   based on, so the cell's loop and the computer's are the same goose.
   Building goose's CLI from the fork instead would mean a Linux build
   and a place to host it, for no difference in behaviour. The
   template's constants pin the version and the digest; changing them
   installs the new one beside the old.
2. **The CLI's guide as goose's hints**: `fragment guide >
   ~/.config/goose/.goosehints`, rewritten each build, so what goose
   knows of the CLI is the installed CLI's own manual.
3. **goose, headless**, in `~/builds/run-<run>`: `goose run --quiet
   --no-session --with-builtin developer --max-turns 60 --text
   <prompt>`, with `GOOSE_PROVIDER=openai`, `GOOSE_MODE=auto` (no
   approvals), `GOOSE_DISABLE_KEYRING=1` (a Sprite has none), and
   `GOOSE_MAX_TOKENS=4096` (the platform's model call must finish within
   its 120 s). Its model is the platform's: the step
   starts `fragment model --serve --port 0` beside it, points
   `OPENAI_BASE_URL` at it, and stops it with goose, so each run has its
   own and a crash leaves none behind. goose's step is capped at 10
   minutes; each other step at 3.
4. **What it built**: the fragments listed after goose ran that were not
   listed before, each with its URL and whether it is live (`fragment
   status`). The run answers `{url, built, message, code}`, `message`
   being the end of what goose said.

The e2e (`builder`) runs every step on the Sprites fake with a stand-in
for goose (the e2e binary run as `goose`, put where the pinned release
would be installed): it speaks to the run's `fragment model --serve` as
goose does, streaming, with one `shell` tool, and the OpenRouter fake's
script calls it to `fragment create` and `deploy`. It proves the
plumbing, not goose; the first real run on a Sprite proves goose.

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

## How a page shows it: the pet (`templates/pet`)

A computer everyone who has the fragment watches live and drives
together, built only from what any fragment may declare:

- **Its computer** is declared with `"start": "node computer/pet.mjs"`
  (a Sprite has Node 24). On its first start the script installs what is
  missing (apt with `sudo -n`: `xvfb openbox xdotool imagemagick
  fonts-liberation fonts-noto-color-emoji`; Chromium from Playwright
  1.63.0, since Ubuntu's own is a snap), then runs a 1024×640 display
  with Chromium on the fragment's `computer/start.html`.
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
(docs/api.md, Agents), so it reads what the command printed.

## Next

- **The builder on a real Sprite**: `fragment new b --template builder`,
  create, deploy, and a task from its page. It needs a CLI release with
  `model --serve` first (a computer made before then has an older CLI:
  the install step says so).
- **The pet's agent**: `click`, `type`, and `open` operations an `agent`
  block offers, which publish to `control`. It waits for tool results
  that carry an image: an agent that cannot see the screen can only open
  addresses.
- **Alerts** about computers awake longer than expected, and what a
  deleted fragment's computer becomes (today it stays, asleep, until
  `fragment computers rm`).
