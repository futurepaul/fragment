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

## Pages loaded before presence changes came one at a time hear the whole list

- **Observed:** round 3 (S8) made a presence change one socket's change
  (`{type: "presence", id, principal, data}`). A page loaded before that
  deploy runs the library it loaded, which reads `m.list` and hands it to
  its presence handlers; with the new frame that is `undefined`, and a
  template that maps it throws until the page reloads. So a socket that
  connects without `?v=2` is tagged `live1` (`LEGACY_TAG` in
  cell/src/live.rs) and hears the whole list, once after `hello` and on
  each change, read from every socket's attachment when one is open. The
  cell logs `live.legacy-page` when one connects.
- **Risk:** while such a page is open, each presence change reads every
  socket's attachment again (the O(N) read S8 removed), and the page's
  own frame is O(N) bytes. Bounded by `LIVE_SOCKETS_MAX` and the presence
  pace; nothing else is.
- **First proof:** a `live.legacy-page` event on the fleet: someone kept a
  page open across the deploy.
- **Delete when:** no fleet has logged `live.legacy-page` for a week:
  remove `LEGACY_TAG`, `legacy_presence_frame`, the `v` check (a socket
  without it is refused), and the live lane's old-page check.

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
- **Delete when:** the list checks each entry against the fragment (or
  the platform keeps delete tombstones and retries them), with an e2e
  that fails a delivery during a delete.

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
  builders accept), or CI builds the image.

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

## A deleted fragment leaves its agent registered

- **Observed:** the agent add-on (`fragment.json`'s `agent` block). A
  fragment's own agent is an identity in the registry, owned by the
  fragment's owner and named as the fragment. Deleting the fragment
  wipes the fragment's cell and so the agent's membership and
  subscription, but not the agent: its identity, key, and conversations
  stay, and it counts toward its owner's agents
  (`limits::AGENTS_PER_OWNER_MAX`).
- **Risk:** an owner who makes and deletes many fragments with agents
  runs into the agents limit; a fragment made again under the same name
  gets the old agent back, with its conversations.
- **First proof:** an owner whose agents count nears the limit, or a
  re-made fragment whose agent recalls the old one's visitors.
- **Delete when:** deleting a fragment retires its own agent (its
  registry entry and key revoked, its cell wiped), with an e2e that
  deletes a fragment with an agent and checks both.

## An agent forgets what falls out of its conversation window

- **Observed:** the reliability pass (audit R10). Each step loads the
  newest 256 messages, cut to start at a turn's first message: the
  running turn whole, and earlier turns while they total 256 KiB
  (`fragment_core::history`). Nothing summarizes what falls out, and the
  agent's tables `messages`, `steps`, and `tool_runs` keep every row
  (`steer` drops what the model read at the next turn, `heard` forgets
  after a day); the owner's view shows the newest 256 of each.
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
  a revoked key is refused from its next request. Since the speed pass
  (#13), a signed-in `__live` socket asks the registry again at its
  first frame after `LIVE_IDENTITY_MS` (60 s) and is closed with 4001
  when its credential no longer names its principal. But the check runs
  only when the client sends a frame: a `__live` socket that only
  listens, and every `__watch` socket, stays open until it reconnects or
  the membership it rests on changes (member and agent removals do
  close sockets).
- **Risk:** a stolen, revoked key keeps a change feed it already had:
  reads only, and only of fragments that identity can still read.
- **First proof:** a revoked key's feed still receiving frames.
- **Delete when:** revoking a key tells the fragments in its identity's
  index to close the sockets tagged with that key (the index cell
  already lists them), with an e2e check; or every socket re-resolves
  its key on a timer, not only at a frame it sends.

## A paid step's reservation is an estimate per kind of step

- **Observed:** phase 4 slice C. A step reserves a fixed worst case (text
  $0.20, an image $0.10, a video $0.10 a second), not the model's price
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
- **First proof:** the next time it happens: the e2e runs its nodes with
  their own logs on and keeps a failing run's scratch, one log per boot
  (`target/e2e/<run>/celld-<port>-<boot>.log`, the killed node's among
  them); or a node on the fleet restarting twice.
- **Delete when:** the fatal is caught and fixed in celld or here, or a
  run of full e2es under load no longer shows it.

## After a restart the registry waits behind every alarm wake

- **Observed:** phase 6, 2026-09-24. On CI's runners the checks right
  after a node restart failed with `registry_unavailable` ("route
  failed: CapacityExhausted"): celld admits cold cells first come, first
  served, and the registry, the one cell every signed request needs,
  waited out its 15 s deadline behind the fragments whose alarms all
  woke at once. `ask_registry` now asks again twice (after 250 ms, then
  500 ms) when the node refused the route that way, so a request can wait
  about 45 s instead of failing. It retries only that refusal, which
  celld answers for a request still queued at its gate: any other failure
  may come after the registry acted, and its calls are not idempotent.
- **Risk:** on a fleet with many fragments, a restarted node answers
  signed requests slowly, or with 503s, until its alarm wakes drain.
- **First proof:** CI's e2e (three-core macOS runners), the checks that
  restart the node in the agents and restart lanes. A gate narrowed
  locally with `CELLD_ACTIVATIONS=1` is not a stand-in: celld's own
  readiness gives up first.
- **Delete when:** the registry is admitted ahead of alarm wakes (a
  priority in celld's gate, or a registry kept warm), or the node wakes
  its alarms in a bounded trickle.

## App data from before celld v0.6.0 reads as empty

- **Observed:** 2026-09-27, after fragment.club's nodes moved from the
  v0.5.1 fork to v0.6.0 (#57, `--nodes --stop-first`, ~12:50Z). Every
  app's own database written before then read as empty (a guestbook's
  rows, the desktop's chats, the pet's frame); the supervisors' data
  (channels, events, members) was intact, and the hosted e2e passed,
  since it makes fresh fragments. An upstream bug, reproduced with
  denoland's own 0.5.1 and 0.6.0 binaries: v0.6.0 imports a facet's
  v0.5.1 image (a row of its root's `_cf_FACETS`) only into a new file
  that is empty (`PRAGMA page_count` 0) and not restored, but under
  replication the facet's stream creates the file first, with ltx's
  control tables, so the import never runs.
- **Risk:** none now: Paul, the only user, chose to start over rather than
  restore, and the fork stays at `4f50c81` (no patch). The old images
  still sit in each fragment's root database; deleting the fragment
  removes its image (`facets.delete`). A later celld upgrade can lose data
  the same way unless it is tested (docs/operate.md, celld upgrades).
- **First proof:** already present.
- **Fix in hand, not used:** a fork commit that imports the image table by
  table, once, keeping ltx's control tables (a whole-file backup would
  wedge replication), on the unpushed local branch
  `facet-legacy-import-unpushed` of `celld-worktrees/v060` (`6b296a2`).
- **Delete when:** the fragments from before 2026-09-27 are deleted or
  re-created (their images go with them), or upstream fixes the import
  and nothing needs it.

## A computer's long steps run inside its alarm

- **Observed:** 2026-09-27, fragment.club. A `Computer` cell's alarm
  makes Sprites calls that can take minutes (a boot's install and
  pairing, a sync, a hold on a Sprite slow to wake; `KEYS` waits up to
  180 s), and celld fires an alarm again whenever its handler outlives
  the operation deadline (15 s; docs/finite-next-lessons.md). Two steps
  at once gave back each other's holds on the month, and a wake failed on
  `no such reservation`. The cell now runs one step at a time
  (`stepping`, cell/src/computer.rs): a handler fired again waits for the
  one before it.
- **Risk:** a long step gathers a waiting handler every ~15 s, each of
  which then steps in turn (cheaply, from the row the long one left); a
  step that hangs holds every later one until `KEYS`' timeout. A job's
  command's start still waits on it (20 s at most, then retried); its
  polls do not (2026-09-27).
- **First proof:** a boot on a real Sprite that takes minutes, or a node
  log with many of one `Computer`'s alarm handlers waiting.
- **Delete when:** the long Sprites calls leave the alarm (run from
  `waitUntil`, with a watchdog alarm armed first, as the lessons say), so
  no step outlives the deadline.

## A deleted fragment's app stream can outlive it

- **Observed:** 2026-09-27, the hosted e2e on celld v0.6.0 (fork
  `4f50c81`): a fragment deleted and made again under the same name failed
  its app calls with "refusing to restore Fragment:…/facets/… from used
  epoch 10 into writer epoch …". Since v0.6.0 an app's database is a
  stream of its own under the fragment's cell, and the old life's objects
  were in the bucket at an epoch the new life could not start from. Not
  reproduced on one local node (bucket durability); the fleet runs fleet
  durability, whose uploads trail, and `rm` did not wait for
  `facets.delete`.
- **Fix:** each life of a name has its own app facet, `app@<incarnation>`
  (`MetaKey::AppFacet`; fragments made before keep `app`), so a new life
  never opens an old stream; `rm` waits for `facets.delete`. The `ops`
  e2e deletes a fragment with app data and makes it again.
- **Risk:** an old life's stream that `facets.delete` did not remove stays
  in the bucket as garbage (bytes, never read).
- **First proof:** objects under `cells/Fragment:<id>/facets/` for a
  facet no live fragment names.
- **Delete when:** celld's facet delete is proven complete under fleet
  durability (or reported and fixed upstream), and a sweep has removed
  what earlier deletes left.

## An agent runs one turn at a time, across all its chats

- **Observed:** phase 7 slice A keeps one conversation per chat, but one
  turn runs at a time per agent (agent/src/lib.rs `begin`, `next_turn`):
  a message for another chat, or from someone other than the running
  turn's starter, waits (at most 64) until the running turn ends. One
  driver, one watchdog alarm, and one cancel token per agent stay as
  phase 5 built them.
- **Risk:** a long turn in one chat (a computer's build, a slow model)
  delays every other chat's answer; a busy agent turns messages away
  (429, redelivered) past 64 waiting.
- **First proof:** an owner whose agent is in several busy chats sees
  answers arrive minutes late, or the delivery dead-letter queue holds
  an agent's inbox.
- **Delete when:** turns of different conversations run at once (a
  driver, a watchdog, and a cancel token per conversation), proven by an
  e2e where chat B's answer lands while chat A's turn is held in a tool.

## Chats made before phase 7 answer through `say`

- **Observed:** phase 7 slice C made the chat template two postable
  channels and no app code; the agent posts its answer to a chat whose
  `chat` channel takes posts. A chat made before still has the old
  template's `app.mjs` (a `say` operation, a worker) and its old page, so
  the agent reads each chat's channels as a turn starts
  (agent/src/progress.rs `shape`) and answers such a chat through its
  listen's reply operation (agent/src/lib.rs `reply`), with no `work`
  records. The e2e keeps the old template as a fixture
  (crates/e2e/fixtures/old_chat.*).
- **Risk:** two answer paths in the agent, and old chats keep a worker
  each and the low-effort page.
- **First proof:** a change to how answers are posted that the old path
  misses (the e2e's old chat stops getting answers).
- **Delete when:** no chat on the hosted fleet has a `say` operation
  (each rewritten to the template's `fragment.json` and page, or
  deleted), proven by a fleet listing; then `reply` posts only, and the
  listen's `reply` field goes.

## A postable channel's retention is fixed

- **Observed:** a channel people may post to keeps its newest
  `limits::POSTED_KEPT` (10 000) records (cell/src/channels.rs `append`),
  whatever the fragment would choose; fragment.json cannot declare
  another number (it would need a column in the installed code's channel
  table, and its parsing).
- **Risk:** a busy chat loses its oldest messages past 10 000, and a
  small one could not ask for less.
- **First proof:** a person asks where the start of a long chat went.
- **Delete when:** a channel's `keep` is declarable (bounded, with this
  as its default) and tested past it.

## A removed computer leaves only the fragments its list names

- **Observed:** `DELETE /api/identities/{computer}` revokes its keys in
  the registry, then has it leave each fragment its `Principal` list
  names (cell/src/lib.rs `remove_computer`). That list is fed from each
  fragment's outbox, so a membership whose delivery is still being
  retried is not in it, and that grant stays.
- **Risk:** a members list shows a removed computer. It cannot act there,
  because no key signs as it, and nothing adds one again.
- **First proof:** a removed computer still listed among a fragment's
  members.
- **Delete when:** the registry answers "removed" for an identity (a
  fragment drops such a member when it reads it), or the removal waits
  for every fragment's outbox, tested with a delivery held.

## A computer is billed for a footprint, not what it used

- **Observed:** Sprites meter a Sprite's actual CPU and memory, which the
  platform cannot read, so each awake tick is charged as the idle
  footprint measured on one ($0.0726 an hour), and its disk as its home
  directory's size when it last woke (`fragment_core::budget::computer`,
  cell/src/computer.rs). Nothing alerts anyone to a computer that stays
  awake longer than expected (Paul, 2026-09-26: later).
- **Risk:** a busy computer (a build, image generation) costs Paul more
  than its owner is charged; files written outside its home are not
  charged; a page left open keeps one awake, charged, and unnoticed.
- **First proof:** the Sprites org's bill for a month against the
  `computer.awake` and `computer.asleep` usage rows.
- **Delete when:** the charge comes from what the Sprite reports it used
  (Sprites' own usage, or its cgroup's `cpu.stat` and memory read on each
  tick), and an owner hears of a computer awake past a bound.

## A chat's session takes one task at a time, unqueued

- **Observed:** 2026-09-27, slice 1 of docs/agent-computer.md. Each chat
  has one goose session on its computer, and the task client
  (crates/core/src/computer/task.mjs) sends its task to that session
  whatever else runs there: nothing queues a second hand-off from the
  same chat behind the first, and what goose serve does with a prompt
  for a session that is mid-turn is its own (it keeps a registry of a
  session's active runs).
- **Risk:** two hand-offs from one chat at once: the second fails, or
  both run in one history, interleaved.
- **First proof:** a chat whose agent hands off twice before the first
  ends, on a real Sprite (the chat hears "could not finish", or one
  task's steps land in the other's).
- **Delete when:** a chat's tasks wait their turn (a lock per session in
  the task client, or the agent holding a chat's second hand-off until
  the first ends), proven by an e2e that hands off twice from one chat
  at once and gets both answers, in order.

## The agent's remembered fact can lose a computer's write to the same file

- **Observed:** 2026-09-27, `memory-followups`. `platform__remember`
  (agent/src/memory.rs) reads `memory/<topic>.md` at main, then commits
  the whole file (`POST /api/f/<memory>/files`), which expects nothing of
  main. A computer's sync that commits the same file between the read
  and the write is overwritten; git keeps it in history.
- **Risk:** a fact goose wrote in a task that ends while the owner tells
  the agent another, to the same topic, drops out of the memory.
- **First proof:** a memory file whose history shows a computer's line
  gone at the agent's next commit.
- **Delete when:** the files API takes an expected blob per path (the
  cell's commit already takes `expect`), the agent passes the one it
  read and reads again on a conflict, proven by an e2e that commits
  between the read and the write and keeps both lines.

## The platform carries two markdown renderers

- **Observed:** 2026-09-28, `files-view`. The files viewer
  (`cell/files.mjs`) renders markdown to DOM with textContent only, as
  the chat's page (`cell/chat.mjs`, `renderMarkdown`) does; the viewer's
  also takes nested lists, wikilinks, links to the fragment's own files,
  and front matter. The chat's was left as it was to keep the change off
  the chat.
- **Risk:** a fix to one (a rendering bug, a safety rule for links) is
  missed in the other.
- **First proof:** a markdown case that renders in one and not the other
  in a way someone reports.
- **Delete when:** the chat renders with the viewer's renderer (one
  module served to both, the chat's picture links kept), proven by the
  chat, desktop, and files e2e sections.

## Files depend on code.storage, a hosted service with no self-hosted twin

- **Observed:** 2026-09-29, while scoping self-hosting (the sandbox
  investigation). Every fragment's files and history live in a
  code.storage repo (`cell/src/cs.rs`, `cli/src/sync.rs`), reached only
  through its REST API: repos, repo urls, branches, file metadata and
  reads, commits, commit packs with expected-parent CAS, merges,
  restore commits, and signed push webhooks, under ES256 JWTs minted with
  the org key. No git smart-HTTP is used. The only other implementation
  is the test fake (`crates/fakes/src/codestorage.rs`), which keeps its
  state in memory or one JSON file.
- **Risk:** fragment is not self-hostable while one of its planes is a
  vendor's hosted service; an outage, a price change, or a deprecation
  at code.storage stops every fragment's files at once, and a
  self-hoster (bring your own compute, ROADMAP phase 10) must hold a
  code.storage org.
- **First proof:** the first deployment that cannot or will not use
  code.storage (a self-hosted or air-gapped fleet), or a code.storage
  outage on fragment.club.
- **Delete when:** a self-hostable service answers the same REST subset
  on durable storage (git on disk or in the bucket), with the fake's
  rules as its conformance suite, and the e2e passes against it as well
  as against the fake.

## sandcastle copies fragment's NIP-98 crate

- **Observed:** 2026-09-29. `sandcastle/crates/nip98` is fragment's
  `crates/nip98` adapted (verify also returns the event id; signing adds a
  `nonce` tag; key proofs left out), because sandcastle must not depend on
  fragment's crates (docs/sandbox.md, decision 4).
- **Risk:** a fix to one copy misses the other.
- **First proof:** a verification bug fixed in one crate and found later
  in the other.
- **Delete when:** both use one crate: Finite's `finite-nostr` once
  sandcastle moves to Finite, or a NIP-98 crate both workspaces depend on.

## sandcastle's real-engine check is by hand

- **Observed:** 2026-09-29. The node's tests run everything but the
  microVM runtime (a fake engine). The real engine, Hermes, TLS, and the
  host's firewall were proven by a hand-driven run against `finite-lat-6`
  (docs/sandbox.md, Phases 1 and 2 on the real engine).
- **Risk:** an msb upgrade or a node change breaks the real path with
  every test green. Three of the day's bugs showed only on the real
  engine.
- **First proof:** the next msb release.
- **Delete when:** a Rust e2e (a `sandcastle` crate or lane) drives a real
  node on a KVM host through the CLI and HTTPS, covering the table in
  docs/sandbox.md, and runs before every deploy.

## A sandcastle node keeps service env in plaintext

- **Observed:** 2026-09-29. A computer's `service.env` (Hermes' dashboard
  password and session secret) is stored in the node's SQLite as part of
  the spec, and in the guest's `/run/sandcastle/service.env`. Views
  redact it.
- **Risk:** a copy of the node's state file discloses every service's
  settings.
- **First proof:** a backup of `/var/lib/sandcastle` leaving the host.
- **Delete when:** env values are sealed at rest with a key the node holds
  apart from its state, or come from the credential source like every
  other secret.

## A sandcastle node converges one computer at a time

- **Observed:** 2026-09-29. The supervisor walks every computer in one
  task each tick. A slow step (an image pull, up to the engine's 600 s
  create timeout) delays every other computer's convergence, wake, and
  health.
- **Risk:** on a busy node, one new computer's first pull stalls everyone
  else's recovery.
- **First proof:** a node with a handful of computers when one pulls a new
  image.
- **Delete when:** each computer converges in its own task, with a
  per-computer lock and a bound on concurrent engine calls.

## A sandcastle node has no per-signer rate limit

- **Observed:** 2026-09-29. The API bounds bodies, headers, connections,
  tickets, sessions, and the replay cache, but not how often one key
  calls. Brain limits per signer.
- **Risk:** one key, granted or not, spends the node's CPU on signature
  checks and its disk on replay rows.
- **First proof:** a misbehaving client in a loop.
- **Delete when:** a per-signer token bucket answers 429 before signature
  verification's cost, with its limits documented and tested.

## A failed sandcastle rebase backs off instead of rolling back

- **Observed:** 2026-09-29. When a new image never serves, the supervisor
  retries, then backs off, leaving the computer down; the old image is
  not restored by itself (docs/sandbox.md, open question 2).
- **Risk:** a bad release takes a person's agent down until the platform
  notices and PUTs the old image back.
- **First proof:** the first Hermes release that fails to start on an
  existing home.
- **Delete when:** a rebase keeps the previous generation, returns to it
  when the new one does not serve within the grace, reports that it did,
  and a test drives it with the fake and on the real engine.

## sandcastle's test node has per-name certificates renewed by hand

- **Observed:** 2026-09-29. `finite-lat-6` serves
  `*.sandcastle.fragment.club` (Paul's A record) with one Let's Encrypt
  certificate naming `api`, `hermes`, and `demo`. It was issued by certbot
  over HTTP-01, with port 80 opened for the issue only, and copied to
  `/etc/sandcastle/le-*.pem` for the daemon.
- **Risk:**
  - A computer with any other name fails TLS.
  - certbot's renewal timer will fail with port 80 closed, and even when
    it succeeds it does not copy the files or restart the daemon, so the
    certificate lapses on 2026-12-28.
- **First proof:** the first computer named otherwise, or 2026-12-28.
- **Delete when:** the router issues and renews each computer's
  certificate itself on first use (ACME TLS-ALPN-01 on 443, only for
  names of existing computers: a self-hoster then needs only a wildcard A
  record), or a wildcard certificate by DNS-01 renews itself. Either is
  tested against a staging CA.

## A sandcastle node does not notice engine machines it has no row for

- **Observed:** 2026-09-29. When a node's state is lost or reset, its
  `sc-…` machines and disks keep running and holding space with no row,
  and nothing reports them (it happened once on `finite-lat-6`, cleaned up
  by hand).
- **Risk:** leaked machines and disks, and a person's data orphaned with
  no owner on record.
- **First proof:** any state restore or reset on a node with computers.
- **Delete when:** the supervisor lists `sc-…` machines and volumes with
  no row and reports them to the operator (never deleting data on its
  own), with a test.

## The test node's daemon runs as a login user

- **Observed:** 2026-09-29. On `finite-lat-6`, `sandcastled` runs as
  `ubuntu`, whose home holds microsandbox's state, because msb was
  installed there first.
- **Risk:** anything else running as that user (an operator's shell) can
  reach every computer's engine state.
- **First proof:** a second use of the test host.
- **Delete when:** the node runs as a dedicated system user with its own
  `MSB_HOME`, set up by the node's install step.
