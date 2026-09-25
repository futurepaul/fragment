# Technical debt ledger

Shortcuts are allowed only through this ledger (engineering style §1).
Each entry names where it was observed, the risk, the first proof that
would show it biting, and the condition that deletes it. An entry
without a delete condition is unfinished design, not debt.

## Author code shares a process with every fragment and the host secrets

- **Observed:** each fragment's `app.mjs` (its operations, jobs, and
  `fetch`) runs in a loaded worker inside the same celld process that
  holds the fleet's keys (in `KEYS`'s memory and the node's environment,
  since H1: no longer in any isolate) and every other fragment's cells.
  The hardening pass took `eval` and `Atomics.wait` from loaded workers,
  capped their heaps and databases, and closed the internal listener to
  unsigned callers. Its env holds only its own capabilities, but an
  isolate escape is not ruled out: celld's own security page calls it
  not safe for hostile multi-tenant use while it is alpha.
- **Risk:** an isolate escape or side channel reads other fragments' data
  and the host's secrets.
- **First proof:** any author able to publish code they did not write
  themselves (public signup, agent-written code for strangers).
- **Delete when:** before strangers can publish code, author code runs
  somewhere that holds no platform secrets and no other tenant's state
  (a separate fleet reached only through per-fragment capability
  tokens), proven by a test that an author isolate cannot reach another
  fragment or a host secret.

## The browser half of web push is not driven by a test

- **Observed:** phase 2 slice F. The e2e proves the server half end to end
  (subscriptions, VAPID, RFC 8291 encryption decrypted by a fake push
  service, the queue, retries, drops), and the encryption agrees with the
  old runtime's (checked once by hand). `fragment.push.register` and
  `__sw.js` run only in a real browser with a real push service.
- **Risk:** a page's subscribe or the service worker's display breaks
  unnoticed.
- **First proof:** subscribing from a phone or desktop browser to a
  hosted fragment.
- **Delete when:** a manual check on a hosted fleet (phase 3) is recorded,
  or a browser test can run with a local push service.

## celld 0.5.1 holds an alarm open while its handler's timers run

- **Observed:** 2026-09-23, `spikes/celld-0.5.1/README.md`. An `alarm()`
  that leaves a timer pending (fragment's `csFetch` uses
  `AbortSignal.timeout(30 s)`) holds the alarm firing until the timer ends
  or the 15 s operation deadline; every re-arm meanwhile waits. The e2e
  check "paused trigger recorded as blocked, not run" failed on 10 of 12
  runs; 312/312 passed on 0.4.0.
- **Risk:** every schedule, retry, and inbox run shares one alarm per
  fragment, so work armed during or right after a firing waits up to 15 s.
- **First proof:** already present (the red e2e check).
- **Fix in hand:** a one-hunk patch in the fork worktree
  `celld-worktrees/alarm-fix` (branch `fix/alarm-completion`) passes
  `--only paused` 10 of 10 and the full e2e 312 of 312. Paul chose to run the fork and upstream the
  fix.
- **Delete when:** a celld release from denoland carries the fix and
  passes `--only paused` 10 of 10; until then dev and hosting run the fork
  build, and its timeout is not widened to hide a regression.

## Fragments share one origin when no hostname suffix is configured

- **Observed:** phase 2 slice B. Without `FRAGMENT_HOST_SUFFIX` the cell
  serves every fragment from `/f/<name>/` on one origin. Cookies are
  scoped by path, but a page on one fragment can still send same-origin
  requests to another's `__op` and `__file` with the visitor's cookies.
- **Risk:** a fragment acts as its visitors on another fragment they have
  a share link or anonymous identity for.
- **First proof:** a fleet serving strangers' fragments without a suffix.
- **Mitigated:** with a suffix configured, `/f/<name>/…` redirects to the
  fragment's own host and refuses writes (e2e `site`); `xtask dev` and
  the e2e run with a suffix.
- **Delete when:** phase 3 fleets always configure a suffix and path-mode
  serving is removed (keeping `/f/<name>/__watch`, which carries no
  cookies), with the `pathmode` e2e section replaced by a check that the
  cell refuses to start serving without a suffix.

## A deleted fragment can linger in a person's list

- **Observed:** phase 2 slice B. Each person's list of fragments is an
  index in their `Principal` cell, fed from the fragment's outbox with
  retries. Deleting a fragment delivers the removals once and then wipes
  the fragment, outbox included: a delivery that fails then is never
  retried.
- **Risk:** `fragment list` shows a fragment the person no longer has
  (calls to it answer 404; nothing leaks).
- **First proof:** a Principal cell unreachable during a delete.
- **Delete when:** phase 4 grows the person cell; its list checks each
  entry against the fragment (or the platform keeps delete tombstones and
  retries them), with an e2e that fails a delivery during a delete.

## The poll backstop wakes every fragment every five minutes

- **Observed:** phase 2 slice B, as in the old runtime: each
  fragment's alarm re-reads both branch heads every
  `FRAGMENT_POLL_INTERVAL_S` (300 s) in case a webhook was lost.
- **Risk:** cost and code.storage traffic grow with the number of
  fragments, not with their activity (1000 fragments: ~7 wakes and ~13
  calls a second).
- **First proof:** phase 3 load numbers on Fly.
- **Delete when:** the interval backs off for fragments with no recent
  pushes (or webhooks are proven reliable enough to poll daily), with a
  test that an idle fragment's alarm spacing grows.

## The effects sweep has no fault-injection test

- **Observed:** phase 2 slice C; reworked in the reliability pass. A
  mutation's effects apply after the facet commits it. The supervisor
  records the mutation as pending before it calls the facet; if it dies
  before applying, the next activation (or the alarm) asks the facet
  about each pending id and applies the run it recorded. The e2e proves a
  passing failure is applied by the alarm, a refusal settles for good,
  a replay applies nothing again, and a ledger row the app forges is
  never applied across a restart, but it cannot stop the node between
  the facet's commit and the supervisor's apply.
- **Risk:** a mutation whose records never appear (and no `changed`
  signal) after a crash in that window, if the sweep is wrong. The
  pending row must be stored before the facet commits: that rests on the
  runtime's output gate holding the facet call until the row is written,
  which no test shows.
- **First proof:** a crash in production between facet commit and apply.
- **Delete when:** celld (or a test build of the cell) offers a fault
  point after the facet call, and an e2e kills the node there and finds
  the records after restart.

## App channels grow without bound

- **Observed:** phase 2 slice C, as MODEL decided: app channels keep
  their records forever in the supervisor's SQLite (`events` and `ops`
  keep 90 days and 10 000 records).
- **Risk:** a busy chat grows the supervisor's database until celld's
  per-object limits or replication cost bite.
- **First proof:** a channel past ~100 000 records, or phase 3 storage
  numbers.
- **Delete when:** `fragment.json` can declare a channel's retention
  (count or age) with a platform ceiling, enforced and tested.

## A secret can go anywhere its fragment's code sends it

- **Observed:** phase 2 slice D. `{{NAME}}` in a job's fetch header is
  opened for any URL the job names. An editor cannot read a secret's
  value, but can write a job that sends it to their own server.
- **Risk:** editors are trusted with the fragment's code, so this is the
  code's authority, not an escalation; it matters once people share
  editing with others they trust less than their keys.
- **First proof:** a fragment with a secret and an editor who is not its
  owner.
- **Delete when:** a secret can name the hosts it may be sent to
  (`fragment secret set NAME --host api.example.com`), checked at the
  egress point.

## Running runs are checked a few at a time

- **Observed:** phase 2 slice D. A run whose Workflow ended without
  reporting (an error outside a step, or a lost instance) is found by the
  poll backstop, which checks the 25 longest-running runs per pass. Runs
  asleep for days are checked every pass, and could crowd out a stuck
  one when there are more than 25.
- **Risk:** a stuck run stays `running`, holding its inbox record's
  place under the cap.
- **First proof:** a fragment with more than 25 long-sleeping jobs.
- **Delete when:** runs record when they were last checked and the pass
  takes the least recently checked, or the Workflow reports its own end
  from a `finally` step.

## A blob upload is not resumable, and whole files pass through memory

- **Observed:** phase 2 slice E. A blob upload is one request (at most
  256 MiB) streamed through the router into R2; celld cannot resume a
  multipart upload on another node or after a restart, so a node that
  restarts mid-upload fails it, and the CLI retries the whole file. The
  CLI also reads each file whole (as sync always has) and holds it while
  it uploads.
- **Risk:** large media over slow links, and memory spikes on the
  machine that syncs.
- **First proof:** a video over a few hundred MB, or a laptop syncing a
  folder of them.
- **Delete when:** uploads go in resumable parts the cell tracks (or
  straight to the bucket with a signed URL once the fleet bucket offers
  one), and the CLI streams files from disk.

## A bad upload can delete a good copy of the same blob

- **Observed:** phase 2 slice E. When uploaded bytes do not hash to the
  sha they claim, the cell deletes the key they were stored under. An
  editor uploading wrong bytes for a sha at the same moment someone
  uploads the right ones can delete the right ones.
- **Risk:** a pointer whose bytes are gone until the file is synced
  again. Only an editor of the fragment can do it.
- **First proof:** a report of a blob gone right after an upload.
- **Delete when:** uploads land under a temporary key and move to the
  content key only once verified (R2 has no rename: this needs a copy,
  or celld's native blob API).

## Blobs in git history lose their bytes after the grace period

- **Observed:** phase 2 slice E, as MODEL decided (latest versions only).
  A rollback of `live` to a commit older than the grace period (7 days)
  that named a large file serves pointers whose bytes are gone (404).
- **Risk:** surprise when rolling far back a fragment with large media.
- **First proof:** a rollback past a week in a fragment with media.
- **Delete when:** someone needs old versions of large files (the
  "Photoshop file" exception in MODEL), and a per-fragment policy keeps
  the bytes named by the last N live commits.

## workers-rs cannot take celld's queue binding

- **Observed:** phase 2 slice F. `env.queue()` in workers-rs 0.8.5 checks
  the binding's constructor name (`WorkerQueue`); celld's is `Queue`, so
  the cell sends through the binding with `Reflect` (`js::queue_send`).
  The service-binding spike found the same kind of mismatch.
- **Risk:** none today; a surprise when another binding type is added.
- **First proof:** already present.
- **Delete when:** workers-rs or celld agree on the names (a one-line
  upstream change on either side).

## The notes viewer is a prebuilt bundle

- **Observed:** phase 2 slice G. `templates/notes/site/assets/` is
  committed esbuild output (marked 18.0.4 and @pierre/diffs 1.2.2, with
  Shiki's language chunks trimmed to a list); its source is
  `templates/notes/src/viewer.mjs`. The repo has no Node tooling, so a
  rebuild is by hand (the recipe is at the top of the source).
- **Risk:** a viewer change needs a toolchain the repo no longer has;
  third-party code ships in every notes fragment without a build the
  repo can reproduce.
- **First proof:** the first change to the viewer, or an advisory
  against marked or Shiki.
- **Delete when:** the viewer is small enough to ship as source (no
  bundler), or the desktop phase replaces it with its own file viewer.

## Fly's remote builders cannot push the node image

- **Observed:** phase 3 slice B. `flyctl deploy` (0.3.145) builds the
  image on Fly's builder and on Depot, and both pushes to the registry
  are refused (401 from the builder's registry proxy) with the org token
  that pushes fine from this machine. `cargo xtask deploy --nodes` builds
  with the local Docker (OrbStack, `linux/amd64` under Rosetta: a cold
  build is about 20 minutes) and pushes directly.
- **Risk:** a node deploy needs this machine (or one like it) with Docker.
- **First proof:** already present.
- **Delete when:** a remote build pushes (a newer flyctl, or a token the
  builders accept), or CI builds the image once a remote exists.

## Three fleet secrets that sat in the bucket are not rotated yet

- **Observed:** phase 3 slice A until the hardening pass (H1). The fleet's
  secrets were Worker `vars`, stored in each deployment's manifest in the
  bucket in plaintext and written into every isolate. H1 moved them to
  the node's environment (only `KEYS` reads them; a cell deploy refuses a
  var that holds one). On 2026-09-24 Paul deleted the earlier
  deployments' manifests from the bucket, and the host secret was rotated
  (the old one stays as `FRAGMENT_KEYS_HOST_SECRET_PREVIOUS`, so values
  sealed under it still open and are resealed as they are read). The
  WorkOS API key, the OpenRouter management key, and the code.storage org
  key are the same values that sat in those manifests; the node's Tigris
  key is the project-wide one `flyctl storage create` made.
- **Risk:** a copy of the bucket taken before the clean-up (a backup, a
  replica) still reveals those three keys; a leak of the node's Tigris
  key reaches every bucket in the Tigris project.
- **First proof:** any bucket copy outside the fleet, or a node compromise.
- **Delete when:** new values for the three keys at their issuers (written
  over their files, then `cargo xtask deploy fragment-club --secrets`,
  then the old ones revoked), and a Tigris key scoped to
  `fragment-club-ord` in the credentials file the same way. Drop
  `FRAGMENT_KEYS_HOST_SECRET_PREVIOUS` only once no value sealed under it
  is left (a sweep that reseals every cell's values, then a check).

## The fleet shares Fly's default private network

- **Observed:** the hardening pass (H4, deferred with Paul's agreement
  2026-09-24). fragment-club's Machines are on the `personal` org's
  default private network (6PN), which every app in the org can reach.
  Since H3 the nodes' internal listener answers only fleet-signed peers,
  so what an app in the org reaches is the peer routes (which refuse it)
  and the public port.
- **Risk:** another app in the org, if compromised, can probe the fleet's
  private addresses; a celld bug in a peer route would be reachable from
  there.
- **First proof:** another app in the `personal` org that runs code we
  did not write, or strangers' code on the fleet.
- **Delete when:** the fleet runs as an app created on its own network
  (`flyctl apps create --network`: a new app, new volumes, certificates,
  and a DNS change), done with Tier 3's cordons (a fleet per trust tier)
  before public sign-up; proven by a check from another app in the org
  that the fleet's private address does not answer.

## celld runs as root in the node image

- **Observed:** H4, deferred. `fragment-node` and celld run as root in
  the Machine (a Firecracker VM).
- **Risk:** small: a celld compromise already holds the node's keys (its
  environment) and data; root adds the rest of the VM.
- **First proof:** a node deploy that touches the image anyway.
- **Delete when:** the image has a `celld` user that owns `/data`
  (`fragment-node` chowns it once, then drops privileges before exec),
  shipped in a node deploy and checked on the fleet.

## Loaded workers are never released

- **Observed:** the isolation spike (2026-09-23); the hardening pass (H2)
  made it visible. celld holds at most 255 loaded workers per script (256
  per process) and releases a named one (`LOADER.get`) only when the node
  restarts. We load one per fragment with an app, so a node that has
  served 255 fragments' apps answers 503 `node_full` for the next one
  (the e2e's `node-full` lane, with the bound lowered through our fork's
  `CELLD_LOADED_WORKERS_MAX`); the apps it holds keep serving.
- **Risk:** past about 255 active apps per node, new apps stop loading
  until a restart; a tenant that makes many fragments can fill a node.
- **First proof:** `node_full` answered on the fleet, or a node's
  loaded-worker count (its `/state`, by the operator) near 255.
- **Delete when:** the fork releases an idle loaded worker (evicting it
  from the registry and the harness's `byName` memo, and aborting the
  facet that used it) with per-tenant accounting, proven by a test that
  loads more apps than the bound and every one still answers.

## KEYS signs code.storage tokens for any repo a fragment names

- **Observed:** H1. `KEYS` signs a code.storage JWT only for a `Fragment`
  cell (the host attests the class), but for whatever repo that cell
  names: the binding between a fragment and its repo is the cell's own
  record. A repo's id is opaque (code.storage makes it), so `KEYS` cannot
  derive it from the caller.
- **Risk:** a supervisor made to ask for another fragment's repo gets a
  token for it (at most fifteen minutes). Supervisors share a V8 context
  (up to 32 cells), so a compromised one could act as any cell there
  anyway; the org key itself stays in the node.
- **First proof:** a bug in the cell that lets a caller choose the repo
  a token names.
- **Delete when:** `KEYS` makes the repo (or answers a binding it signed
  when it did) and checks it on every token.

## ArrayBuffer memory is outside the heap ceiling

- **Observed:** H3. Our fork ends an isolate's execution once its V8
  heap passes twice its limit (the e2e's `app-lockdown` lane), but an
  ArrayBuffer's bytes live outside the V8 heap and are not counted.
- **Risk:** an app that allocates large ArrayBuffers can still grow the
  node's memory.
- **First proof:** a node's memory growing with one fragment's traffic.
- **Delete when:** the fork gives each loaded worker an ArrayBuffer
  allocator with a budget (ending the isolate past it), with a test.

## WebAssembly compiled from bytes never settles in an app

- **Observed:** H3. With `CELLD_DYNAMIC_LOCKDOWN=1`, `eval` and `new
  Function` throw in an app, but `WebAssembly.compile(bytes)` neither
  resolves nor rejects (the call hangs until the client gives up). Only
  the app's own call waits, as a never-settling promise would.
- **Risk:** an author who tries Wasm gets a hang instead of an error.
- **First proof:** an author asking why their Wasm module hangs.
- **Delete when:** the fork refuses Wasm from bytes with an error in a
  locked-down worker (or allows it deliberately), with a test.

## An agent's turns have no usage rows

- **Observed:** phase 6 step 4b. An agent's model calls spend its
  owner's month on the owner's own OpenRouter key (the `Ledger` mints it,
  its limit the allowance, so OpenRouter stops the owner there), fetched
  per turn through `POST /api/budget/key`. But nothing reserves or
  settles per step, so the month's view (`fragment budget`) does not show
  what agents spent until OpenRouter refuses the key, and the key is in
  the agent script's heap for the turn (as a `job.ai` step's is in the
  cell's).
- **Risk:** a person's budget looks unspent while their agent spends it;
  their fragments' AI steps then fail at OpenRouter's limit with less
  warning than the ledger gives.
- **First proof:** a person whose agent works a lot.
- **Delete when:** each turn step reserves and settles in the owner's
  ledger as `job.ai` steps do (reserve the step's worst case, settle to
  the stream's `usage.cost`, one usage row per step keyed by agent, turn,
  and step).

## An agent forgets what falls out of its conversation window

- **Observed:** the reliability pass (audit R10). Each step loads the
  newest 256 messages, cut to start at a turn's first message: the
  running turn whole, and earlier turns while they total 256 KiB
  (`fragment_core::history`). Nothing summarizes what falls out, and the
  agent's tables (`messages`, `steps`, `tool_runs`, `steer`, `heard`)
  keep every row; the owner's view shows the newest 256 of each.
- **Risk:** an agent answers without context it had long ago, with no
  sign that it lost it; its database grows with its age.
- **First proof:** an agent asked about something said before its
  window.
- **Delete when:** goose's compaction runs on the window's edge (a
  summary message stands in for what falls out), and rows older than
  the summary are deleted, with a test that a fact from before the
  window survives in the summary.

## The computer kills a shell command's tree itself

- **Observed:** phase 8. `goose-developer`'s shell runs `$SHELL -c
  <command>` in the computer's own process group and, on cancel or
  timeout, kills only that shell: the command's children run on.
  `fragment computer serve` starts each command by writing the shell's
  pid under its journal (`echo $$ > <state>/pids/<id>;`, a pid a shell
  keeps when it execs the command in its place) and, on cancel, kills
  the tree under that pid first (`crates/computer`).
- **Risk:** a command that detaches into its own session escapes the
  kill; a timed-out command's children still run on, since the timeout
  is goose's.
- **First proof:** a builder's dev server, or a runaway build, still
  running after its turn was stopped or its call timed out.
- **Delete when:** `goose-developer` gives each command its own process
  group and kills the group on cancel and on timeout (a commit on
  `futurepaul/goose`, and upstream), proven by the unit tests
  `cancelling_a_shell_call_kills_its_children` and
  `cancelling_kills_the_children_of_an_execed_command` with the pid
  file removed.

## A socket opened with a key outlives that key's revocation

- **Observed:** phase 4 slice A. Every request resolves its key live, so
  a revoked key is refused from its next request. A `__watch` or
  `__live` socket opened before the revocation stays open until it
  reconnects, or until the membership it rests on changes (member and
  agent removals do close sockets).
- **Risk:** a stolen, revoked key keeps a change feed it already had:
  reads only, and only of fragments that identity can still read.
- **First proof:** a revoked key's feed still receiving frames.
- **Delete when:** revoking a key tells the fragments in its identity's
  index to close the sockets tagged with that key (the index cell
  already lists them), with an e2e check; or sockets re-resolve their
  key on a timer.

## A paid step's reservation is an estimate per kind of step

- **Observed:** phase 4 slice C. A step reserves a fixed worst case (text
  $0.05, an image $0.10, a video $0.10 a second), not the model's price
  times its tokens. A long answer from an expensive model can cost more
  than it reserved; the ledger records what it did cost, so a month can
  pass its allowance by that difference.
- **Risk:** the ledger's month a little past the allowance. OpenRouter
  itself cannot pass it: each org's key carries the allowance as its
  limit, and a call past it is refused (402, the step held).
- **First proof:** a settled cost above its reservation (the usage row
  shows both).
- **Delete when:** reservations come from the model's prices
  (`/api/v1/models`) and the step's `max_tokens`, with a test that a
  month never passes its allowance.

## A held run's video that finishes anyway is not charged to the month

- **Observed:** the reliability pass (audit R13). A run held while its
  video still waits for its cost gives the reservation back, since
  nothing polls the video any more. OpenRouter may still finish it and
  charge the org's key; the ledger never learns that cost.
- **Risk:** the ledger's month under what the key really spent, by at
  most one video per held run. OpenRouter cannot pass the allowance: the
  key carries it as its limit.
- **First proof:** the key's usage (`GET /api/v1/key`) above the month's
  `spentMicros`.
- **Delete when:** a released video is polled once more on the alarm
  until OpenRouter says it ended, and settled at what it reports, with a
  budget check that holds a run mid-video and sees the cost arrive.

## A paid step whose result was not stored is paid again

- **Observed:** phase 4 slice C. A paid step settles once its whole
  result is in hand (an image is written to `main` first). If that write
  fails for now, the step is retried on the reservation it holds and
  calls OpenRouter again; only the retry's cost is recorded.
- **Risk:** a second charge at OpenRouter that the ledger does not show
  (the key's limit still counts it).
- **First proof:** an image step retried after a code.storage failure.
- **Delete when:** a paid step's answer is kept before anything else
  runs (the image as a blob by its hash first), so a retry settles
  without calling again.

## A node restarted after a kill has twice died of SIGILL

- **Observed:** phase 6, 2026-09-24. In two of about six full e2e runs
  (both with the machine's load at 20 to 30 from other sessions' runs),
  the computer lane's check that kills the node mid-command ended with
  the restarted node exiting on SIGILL: a V8 fatal error, most likely,
  as hundreds of cells from the earlier lanes woke at once, the agents'
  script co-hosted beside the cell. It never happened with the lane run
  alone (five runs, one at load 30), nor in the full run with the node's
  own logs on (848 passed).
- **Risk:** a fragment.club node that dies hard, restarts, and wakes
  everything at once could die again.
- **First proof:** the next time it happens, with `FRAGMENT_NODE_LOGS=1`
  (the node's own logs, kept across restarts in
  `target/devstack/celld-<port>.log`), or a node on the fleet restarting
  twice.
- **Delete when:** the fatal is caught and fixed in celld or here, or a
  run of full e2es under load no longer shows it.

## After a restart the registry waits behind every alarm wake

- **Observed:** phase 6, 2026-09-24. On CI's runners the checks right
  after a node restart failed with `registry_unavailable` ("route
  failed: CapacityExhausted"): celld admits cold cells first come, first
  served, and the registry, the one cell every signed request needs,
  waited out its 15 s deadline behind the fragments whose alarms all
  woke at once. `ask_registry` now asks again twice (after 250 ms, then
  500 ms), so a request can wait about 45 s instead of failing.
- **Risk:** on a fleet with many fragments, a restarted node answers
  signed requests slowly, or with 503s, until its alarm wakes drain.
- **First proof:** CI's e2e (three-core macOS runners), the checks that
  restart the node in the agents and restart lanes. A gate narrowed
  locally with `CELLD_ACTIVATIONS=1` is not a stand-in: celld's own
  readiness gives up first.
- **Delete when:** the registry is admitted ahead of alarm wakes (a
  priority in celld's gate, or a registry kept warm), or the node wakes
  its alarms in a bounded trickle.
