# Technical debt ledger

Shortcuts are allowed only through this ledger (engineering style §1).
Each entry names where it was observed, the risk, the first proof that
would show it biting, and the condition that deletes it. An entry
without a delete condition is unfinished design, not debt.

## Author code shares a process with every fragment and the host secrets

- **Observed:** each fragment's `app.mjs` (its operations, jobs, and
  `fetch`) runs in a loaded worker inside the same celld process that
  holds `FRAGMENT_HOST_SECRET`, the code.storage org key, and every other
  fragment's cells. Its env holds only its own capabilities, but an
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

- **Observed:** phase 2 slice C. A mutation's effects apply after the
  facet commits it; if the supervisor dies in between, the next
  activation sweeps the newest 200 ledger rows and applies what has no
  `ops` record. The e2e proves re-application is idempotent (a replay
  appends nothing twice) but cannot stop the node between the two
  commits.
- **Risk:** a mutation whose records never appear (and no `changed`
  signal) after a crash in that window, if the sweep is wrong.
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

## AI spend has no limit of the platform's own

- **Observed:** phase 2 slice F. `job.ai.*` spend the fragment's own
  OpenRouter key; a video is about $0.05 to $0.08 a second. Triggered
  runs have the hop budget and 120 runs an hour, but a job an editor calls
  in a loop, or a public job, spends until the key's own limit.
- **Risk:** a surprise bill on the owner's key (their money, their key's
  limits: OpenRouter lets a key carry a spend cap).
- **First proof:** a public fragment whose job generates media.
- **Delete when:** the platform counts spend per fragment (OpenRouter
  answers `usage.cost`) and an owner can cap it, or people connect their
  own keys with caps (phase 4).

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

## The fleet's secrets live in the bucket

- **Observed:** phase 3 slice A. celld 0.5.1 removed the environment
  passthrough for Worker variables (`CELLD_VAR_`): they come only from
  the deployed config, which celld stores in the bucket. The host secret
  and the code.storage org key are there in plaintext.
- **Risk:** a leak of the bucket (or a backup of it) reveals them.
  celld already makes the bucket the fleet's root of authority (its keys
  can deploy code that reads any variable), so this adds no new holder.
- **First proof:** a bucket copy outside the fleet (a backup, a
  migration).
- **Delete when:** celld (or the fork) gives Workers node-held secrets,
  as the "fork forever?" thread proposes.

## Agents spend the agent fleet's model key

- **Observed:** phase 5. An agent's model calls use the agent fleet's
  `OPENROUTER_API_KEY`, whoever owns the agent; nothing counts them per
  person.
- **Risk:** spend that no person's budget shows, once agents are hosted.
- **First proof:** the agent fleet hosted with people other than Paul.
- **Delete when:** phase 4's budgets (decision 14) meter model calls per
  owner, with the person's own OpenRouter key and its limit.

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
