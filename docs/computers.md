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
4. **Awake and asleep.** A page opening on the fragment wakes it. Each
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
   Close it: after 5 minutes and a tick, it is warm, then cold.
   `fragment budget usage` shows `computer.awake` rows.
4. `fragment computers rm pet-smoke.<you>`: the Sprite is gone from
   `sprite list`.

Things only the real one shows: the exec answer's shape (the cell reads
the last number `du -sk` printed), the install's time (the exec waits up
to 180 s), and whether `sprite-env curl` holds it awake from an exec. For
`job.computer.exec`: that the detached runner outlives the exec that
started it (`setsid`), and that a poll's 100 s exec, a blank line every
10 s, comes back whole.

## Next

- **The builder on a real Sprite**: `fragment new b --template builder`,
  create, deploy, and a task from its page. It needs a CLI release with
  `model --serve` first (a computer made before then has an older CLI:
  the install step says so).
- **How a page shows it: a CUA "pet".** A declared `desktop` service
  (Xvfb, Chromium, and a control endpoint, from finite-next's
  `computer/*`) takes a screenshot on each change (at most one a second),
  stores it as a blob, and posts `{sha, at}` to a declared `screen`
  channel: every viewer sees the same frame live. The page calls
  operations (`poke {x, y}`, `type {text}`) that append to a `control`
  channel, which the computer follows and applies once each (keyed by
  seq). An agent drives it through the same operations.
- **Alerts** about computers awake longer than expected, and what a
  deleted fragment's computer becomes (today it stays, asleep, until
  `fragment computers rm`).
