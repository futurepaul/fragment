# Technical debt ledger

Shortcuts are allowed only through this ledger (engineering style §1).
Each entry names where it was observed, the risk, the first proof that
would show it biting, and the condition that deletes it. An entry
without a delete condition is unfinished design, not debt.

Entries whose subject went at the cut (computers, sandcastle, Hermes,
the desktop, chats; docs/cloudflare-v1.md, decision 33) went with it
and are at the tag `celld-final`. Entries about the hosted fleet (Fly,
the node image, its secrets) are the `celld` branch's, which runs
fragment.club until cutover (decisions 34–35).

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
  `__sw.js` run only in a real browser with a real push service. The
  chat's push (docs/chat-records.md, Push) is the same: its chat lane
  subscribes the push fake by `__push-sub`, as the page's bell would.
- **Risk:** a page's subscribe or the service worker's display breaks
  unnoticed.
- **First proof:** subscribing from a phone or desktop browser to a
  hosted fragment.
- **Delete when:** a manual check on a hosted fleet (phase 3) is recorded,
  or a browser test can run with a local push service.

## A chat the shell frames is "looked at" while the shell is, and asks for notifications in a tab

- **Observed:** 2026-10-03, the chat's push (docs/chat-records.md, Push).
  A chat's page says it is `looking` while its document is visible, so a
  chat open in a tab of the shell that is not shown (its frame is still
  in a visible page) keeps its person from being pushed. And a browser
  does not let a cross-origin frame ask for notifications, so the bell of
  a chat the shell frames opens the chat in a tab of its own, where it may
  ask.
- **Risk:** a person with the shell open on another chat misses a
  reply's push; turning notifications on from the shell takes a detour.
- **First proof:** a person who keeps the shell open says a reply came
  without a notification.
- **Delete when:** the shell tells each frame whether it is shown (a
  message the chat's page reads, beside its theme), and asks for
  notifications on the platform's origin for the fragments it frames, or
  browsers let a frame ask.

## A busy chat's push pauses itself

- **Observed:** 2026-10-03. The chat's push job runs on a channel trigger
  (`"from": "agent"`), so each agent reply is a triggered run, and an
  operation's triggers pause themselves after 120 triggered runs in an
  hour (`limits::TRIGGERED_RUNS_PER_HOUR`). Nothing unpauses a chat's.
- **Risk:** a chat whose agents reply more than about twice a minute for
  an hour stops pushing, for good, until someone runs `fragment unpause`.
- **First proof:** an `op.auto-paused` event for `notify_reply` on a
  chat.
- **Delete when:** a trigger's breaker counts runs that fail or loop
  rather than every run (or an automatic pause expires), or a chat's push
  fires once a turn, on its last reply.

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

## The notes viewer is a prebuilt bundle

- **Observed:** phase 2 slice G. `templates/notes/site/assets/` is
  committed esbuild output (marked 18.0.4 and @pierre/diffs 1.2.2, with
  Shiki's language chunks trimmed to a list); its source is
  `templates/notes/src/viewer.mjs`. The repo has no Node tooling, so a
  rebuild is by hand (the recipe is at the top of the source). Since
  phase 6 the blessed brain template serves the same bundle
  (`templates/brain/site/assets` is a symlink to it, embedded once), and
  its search box is a separate script beside it (`site/brain.js`), not a
  change to the viewer: nothing pins the bundle's transitive dependencies
  (Shiki's among them), so a rebuild could not be checked against the
  committed bytes.
- **Risk:** a viewer change needs a toolchain the repo no longer has;
  third-party code ships in every notes fragment and every brain without
  a build the repo can reproduce.
- **First proof:** the first change to the viewer, or an advisory
  against marked or Shiki.
- **Delete when:** the viewer is small enough to ship as source (no
  bundler), or a viewer of the platform's replaces it.

## Local nodes keep their computers apart through wrangler's and workerd's internals

- **Observed:** 2026-10-05, several sessions running the e2e on one Mac.
  wrangler 4.145.0, at a dev session's teardown, removes `docker ps
  --filter ancestor=<tag>` for each image tag it built; Docker resolves
  the filter to the image's ID, which the same source builds in every
  worktree, so one node's stop removed other runs' computers mid-run (a
  run's end removed another's live container, both runs on one commit).
  It removes the containers and not the `-proxy` sidecars workerd
  1.20260930 runs beside them, and miniflare stops workerd with SIGKILL,
  so workerd's own cleanup (which removes both) never runs: 109 orphaned
  sidecars were running at once. crates/devstack/src/containers.rs builds
  each project's images from Dockerfiles of its own, labeled for it (a
  distinct image ID, every layer cached), and removes what its nodes left
  by the names workerd gives them, `workerd-<worker>-<class>-<id>` and
  `…-proxy` for each object in its state's `v3/do/<worker>-<class>/`, a
  container only when it carries the project's label.
- **Risk:** it leans on three internals at the pinned versions: the
  teardown's ancestor filter, workerd's container names, and miniflare's
  state layout. A wrangler or workerd that changes the names or the layout
  leaves the containers again (the removal finds none to remove; it
  cannot reach another project's, whose computers carry its own label);
  one that matches containers some other way could reach other runs'
  computers again. A per-run image is also a new image ID per run: an
  image config each, every layer shared.
- **First proof:** `docker ps` lists `workerd-fragment-*` containers
  after every run that made them has ended, or a run's computers die
  when another worktree's node stops.
- **Delete when:** wrangler removes a dev session's containers by its own
  tag or label (not the image ID) and its sidecars with them, or lets
  workerd drain at shutdown: then `scope_images`, `remove`, and their
  calls in the e2e and `xtask dev` go. Moving the wrangler pin checks it.

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

## The cell signs code.storage tokens for any repo a fragment names

- **Observed:** H1, and in the cell since phase 2 (`cell/src/keys.rs`).
  A code.storage JWT is signed for whatever repo the fragment's
  supervisor names: the binding between a fragment and its repo is the
  cell's own record. A repo's id is opaque (code.storage makes it), so
  the signer cannot derive it from the caller.
- **Risk:** a supervisor made to ask for another fragment's repo gets a
  token for it (at most fifteen minutes).
- **First proof:** a bug in the cell that lets a caller choose the repo
  a token names.
- **Delete when:** tokens are signed only for the repo the fragment's
  creation recorded, checked on every token, with a test.

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

## Video steps are off

- **Observed:** 2026-10-03, when OpenRouter was cut (Paul: "I thought we
  didn't need openrouter now that we're using ai gateway?"). Images moved
  onto Workers AI (FLUX.1 [schnell] through the AI binding,
  cell/src/ai.rs); Workers AI's catalog has no video model (2026-10-03), so
  `job.ai.video` is a step the platform refuses ("video steps are off
  until they run on Cloudflare": `fragment_core::media::Refusal::VideoOff`),
  and OpenRouter's key, its fake and `Usage::Billed` went with it.
- **Risk:** a fragment that made videos (meatproxy's `fragment:ai`,
  docs/published-fragments.md) cannot, and an agent asked for one can
  only say so.
- **First proof:** a held run whose error is that refusal.
- **Delete when:** a video model runs on Cloudflare (Workers AI's
  catalog, or a provider through AI Gateway's Unified Billing), metered
  from its usage as images are, with `job.ai.video` back as steps and an
  e2e check that a video is drawn, stored as a blob and charged; or Paul
  drops videos, and `job.ai.video` goes with this entry.

## The price book is the core's defaults

- **Observed:** phase 3. Every ledger charges with
  `PriceBook::defaults()` (cell/src/ledger.rs `configured_book`): the
  list prices of 2026-10-02 and a 50% margin, version 1. Decision 4 puts
  pricing in the deployment's configuration.
- **Risk:** a self-deployer cannot set their own margin or prices without
  a fork; a Cloudflare price change needs a code change and a deploy.
- **First proof:** a deployment that wants another margin.
- **Delete when:** `xtask deploy` renders a price book from the
  deployment's config (a versioned file) into a Worker variable that
  `configured_book` reads, and each ledger takes the newer version at its
  next call (`take_book` already does), with a test of a book's change.

## A fragment's git repository is not metered, and storage is sampled daily

- **Observed:** phase 3 (cell/src/meter.rs). A fragment's storage meter
  samples its SQLite (its own and its app facet's) and its blobs once a
  day from its alarm, as byte-hours since the last sample. Its
  code.storage repository is not sampled (code.storage publishes no size
  or price to us), and a computer's backups do not exist yet.
- **Risk:** git storage is free to its owner; a fragment that grows and
  shrinks within a day is billed at the day's sample.
- **First proof:** a repository at code.storage larger than a fragment's
  SQLite.
- **Delete when:** code.storage reports a repository's size (or we host
  git), sampled as `Storage { class: git }`, and samples run hourly from
  a cron the plan named (docs/ledger.md).

## A run in flight and the platform's own writes go on past the overdraft

- **Observed:** phase 3. Past its owner's overdraft a fragment refuses
  the writes a principal asks for (mutations and jobs, posts, file
  writes, deploys, storage tokens, blobs, its inbox, replays: `writable`
  in cell/src/meter.rs), and since 2026-10-03 its cron, channel and file
  triggers start no runs (each recorded `blocked` with the ledger's
  reason: `start_run` in cell/src/jobs.rs, the ledger section's e2e). But
  a run already in flight still writes (its mutations, publishes, file
  writes) and starts the jobs it calls, and the platform's own records
  (an agent's `joined` on its `tasks`: cell/src/runs_on.rs) still append.
- **Risk:** a run that started before the overdraft writes a read-only
  fragment for as long as it runs (its AI steps are refused by the
  ledger all the same); the platform's own records are a few bytes each.
- **First proof:** a read-only fragment whose events show a run's
  writes landing after its owner went past the overdraft.
- **Delete when:** each step of a run asks the owner's standing before
  it writes (a step past the overdraft fails for good, saying why, so
  the run is held and replays after a top-up), with an e2e check; the
  platform's own records stay (they cost nothing, and an agent must hear
  where it joined).

## An app's database size is read from its own realm

- **Observed:** phase 3. The storage meter asks the app facet's platform
  code for its database size (`__size` in cell/platform.mjs), which runs
  in the author's realm: an app can override it.
- **Risk:** an app understates its own storage, by at most its cap
  (`limits::APP_DB_MAX_BYTES`, which the meter clamps to).
- **First proof:** a `__size` far under the facet's real size.
- **Delete when:** the runtime lets the supervisor read a facet's storage
  size itself.


## An agent runs one turn at a time, across all its chats

- **Observed:** phase 7 slice A keeps one conversation per chat, but one
  turn runs at a time per agent (agent/src/lib.rs `begin`, `next_turn`):
  a message for another chat, or from someone other than the running
  turn's starter, waits (at most 64) until the running turn ends. One
  driver, one watchdog alarm, and one cancel token per agent stay as
  phase 5 built them.
- **Risk:** a long turn in one chat (a slow model, a long tool call)
  delays every other chat's answer; a busy agent turns messages away
  (429, redelivered) past 64 waiting.
- **First proof:** an owner whose agent is in several busy chats sees
  answers arrive minutes late, or the delivery dead-letter queue holds
  an agent's inbox.
- **Delete when:** turns of different conversations run at once (a
  driver, a watchdog, and a cancel token per conversation), proven by an
  e2e where chat B's answer lands while chat A's turn is held in a tool.

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

## Files depend on code.storage, a hosted service with no self-hosted twin

- **Observed:** 2026-09-29, while scoping self-hosting (the sandbox
  investigation, docs/sandbox.md at the tag `celld-final`). Every
  fragment's files and history live in a code.storage repo
  (`cell/src/cs.rs`, `cli/src/sync.rs`), reached only through its REST
  API: repos, repo urls, branches, file metadata and
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

## Channel drafts' refusals have no e2e

- **Observed:** the cut (2026-10-02) took the lane that drove
  `PUT /api/f/<name>/channels/<channel>/draft` (cell/src/channels.rs
  `draft_api`: a record its poster is writing, sent to the channel's
  readers as `draft` frames, never stored). Phase 4's computers and
  real-Hermes lanes drive it again through the bridge, and a page's
  socket hears the frames; nothing drafts as a stranger or past the pace.
- **Risk:** a change lets a stranger draft on a channel, or drops the
  pace, and nothing says so.
- **First proof:** a draft from someone who may not post.
- **Delete when:** a lane checks that a stranger's draft is refused and
  that a draft past the pace is refused (429).

## The hosted lane runs part of the suite, and not from CI

- **Observed:** phase 2 (2026-10-03): the e2e runs on local workerd,
  whose limits differ from Cloudflare's: it enforces no CPU, memory,
  subrequest or concurrency limit (spike S1), and its Workflows keep a
  sleep as a timer in the process, so one never wakes after a crash of
  `wrangler dev`. Those checks are `skip`s, counted in every run. Phase 7
  (2026-10-03, `claude/phase-7`) built the hosted lane (`cargo xtask e2e
  --hosted`, crates/e2e/src/hosted.rs): it runs on a preview the sections
  whose declared needs a preview meets. The rest are skips there, each
  saying why: those that script a vendor fake (the model: ai, ledger,
  agents, addon, chat; code.storage's git or webhooks: create, files,
  ops, effects, site, sync, blobs, appfiles; a local upstream or push
  service: jobs, triggers, push, channels' posts), the node's (restart,
  lockdown, isolation, share, pathmode), and the whole deployment's
  (identities, signin, ledger's operator). Nothing runs it from CI.
- **Risk:** a limit, a Workflow resume, or a skipped section's behaviour
  on real vendors breaks unseen.
- **First proof:** any skip in a run's summary, local or hosted.
- **Delete when:** the hosted lane runs from CI against a branch
  deployment, each section that needs a fake today reaching a target on
  the deployment itself instead (a fragment's own route as a job's
  upstream, its inbox as a webhook's, the real model within the run's
  paid calls), and the local skips made there.

## A query can write past its app's cap

- **Observed:** phase 2. A mutation that leaves the app's database over
  16 MiB rolls back (507), but a query (or a route) that writes anyway is
  not stopped: celld's hard stop 4 MiB above the cap went with the fork.
- **Risk:** an app grows its database without bound outside mutations,
  at its owner's storage cost.
- **First proof:** an app's database over 16 MiB.
- **Delete when:** writes outside a mutation and the constructor are
  refused (or capped) by `platform.mjs`, with a test.

## A blob larger than the zone's request limit cannot be uploaded

- **Observed:** phase 2. A blob route takes up to 256 MiB, but
  Cloudflare refuses a request body past the zone plan's limit (100 MB
  on Free and Pro) before the Worker sees it.
- **Risk:** `fragment sync` of a file between 100 MB and 256 MiB fails.
- **First proof:** a 101 MB file synced to a deployment on a Free zone.
- **Delete when:** blobs upload in parts (R2 multipart through the
  cell), each under the limit, with a test of a 150 MB file.

## The dev proxy may answer an early refusal with its own 500

- **Observed:** phase 2. Under `wrangler dev`, a Worker that answers
  before a chunked upload ends (the body limit's 413) sometimes reaches
  the client as miniflare's own 500, "Network connection lost". The e2e
  accepts that answer locally, saying so; Cloudflare's edge has no such
  proxy.
- **Risk:** none in production; a weaker local check.
- **First proof:** already present.
- **Delete when:** the hosted lane checks the 413 itself, or miniflare
  passes the early answer through.

## The Hermes image patches Hermes' own boot

- **Observed:** phase 4 (`images/hermes/Dockerfile`). The preloaded
  gateway is hermes-boot's main program (spike S3b), so the image removes
  upstream's `/etc/cont-init.d/02-reconcile-profiles`, whose
  `hermes_cli.container_boot` would start an s6-supervised gateway beside
  it from a restored `gateway_state.json`. The build fails if that script
  is not there to remove, or another cont-init script names
  `container_boot`.
- **Risk:** a Hermes release that starts a gateway at boot some other
  way: two gateways serve one home.
- **First proof:** the first Hermes upgrade after v0.21.5.
- **Delete when:** upstream lets a container whose main program is the
  gateway skip its reconciler (or ships a preloadable, unsupervised
  gateway main program: S3b's "Upstream Hermes" list), proven by the
  real-Hermes lane on an image without the removal.

## A Hermes turn's end is read from its reactions

- **Observed:** phase 4, against the real image. Relay has no
  turn-level end; the bridge reads its processing hooks' reactions
  (`👀` off, then `✅`/`❌`). Hermes brackets a message it took while its
  gateway was starting twice, the first empty (docs/hermes-relay.md), so
  the relay runtime ends a turn at `❌`, at `✅` only once it said
  something (Hermes ends no person's turn without a word), and, stopped,
  at `👀` off. No clock (#156 cut the 1.5 s and 20 s windows).
- **Risk:** a Hermes release that ends a person's turn saying nothing,
  or cancels one the bridge did not stop, leaves it running until the
  idle bound (15 minutes, then an error); one that stops reacting does
  so for every turn (loudly: the Docker rung's turns never end). Hermes'
  clarify questions are read by their glyphs (`❓`, `✏️`) the same way.
- **First proof:** a turn that ends "the agent stopped answering" though
  Hermes answered,
  or a reply posted under a `said` turn rather than its message's.
- **Delete when:** Hermes' Relay sends a turn's end and its questions as
  structure, proven by the real-Hermes lane.

## Hermes' tool steps are its progress text

- **Observed:** phase 4. v0.21.5 sends `task_card` only for Slack chats,
  so a Relay turn's steps are the lines of its progress message: a tool's
  name and its preview, `ok` always true, no excerpt of its result.
- **Risk:** a step card cannot say a tool failed or what it returned; a
  change to the progress lines' format changes the cards.
- **First proof:** phase 5's chat template showing a failed tool as ok.
- **Delete when:** Hermes sends structured tool events (task cards, or
  their like) to Relay connectors other than Slack's, and the relay
  runtime maps them, proven by the real-Hermes lane's step assertions.

## A quick tool's step can be lost in Hermes

- **Observed:** the Docker rung's `the_hermes_image`, twice, warm.
  Hermes v0.21.5's progress sender (`gateway/run_turn_runner.py`,
  `TurnRunner.send_progress_messages`) takes a tool's progress line from
  its queue every 0.3 s; when the turn's cleanup cancels it, it edits a
  progress message it already sent but never sends a first one. A turn
  that ends before the sender's next poll after its tool starts sends no
  progress line at all, as a message or in a draft (its one draft is the
  answer's), so the relay runtime has no step to record; a probe of that
  code in the image lost 21 of 48 lines whose turns ended within 0.35 s
  of their tool's start. The scripted model answers at once and `echo` is
  quick: the failing turns took about 350 ms, and a passing one sent its
  line 65 ms before its answer. So the rung's terminal command sleeps 2 s
  first (`images/bridge/tests/support/model.rs`). The e2e's `hermes` lane
  runs `run: echo tool-ran` through the Workers AI fake and has the same
  race, so far unseen.
- **Risk:** a real turn whose tool and next model call take under 0.3 s
  together shows its answer with no step. Rare with a real model, whose
  next call alone is slower.
- **First proof:** a Hermes turn with a tool call in its session and no
  `turn.step` on work.
- **Delete when:** Hermes sends what its progress queue holds when its
  sender is cancelled (or the entry above goes), proven by the Docker
  rung with an instant command.

## The Hermes image patches Hermes' progress sender

- **Observed:** the hosted `agent-smoke` (2026-10-06): asked to list
  fragments with the CLI, 2 runs of 3 recorded only the skill read's step
  though the reply named the fragments a terminal call listed. Hermes
  v0.21.5's progress sender (`gateway/run_turn_runner.py`,
  `TurnRunner.send_progress_messages`) edits at most every 1.5 s: a line
  that comes sooner waits out the interval, then the sender goes back to
  its queue and sends that line only with a newer one. A tool call within
  1.5 s of the last progress line that is its turn's last (a skill read,
  then a quick model call to the terminal) is never sent, and the next
  text segment's new-message marker clears it. A probe of that code in
  the image (the sender driven with a scripted queue) lost the line in
  each such order and sent it once the patch removes the loop's
  `continue` after the wait; the Docker rung's `use the terminal twice`
  turn proves the patched image. The image applies it with Python before
  its bytecode step (`images/hermes/Dockerfile`), and the build fails if
  the loop no longer reads as it did.
- **Risk:** a Hermes release changes the loop: the build fails (loudly).
  Edits stay at most one per 1.5 s; the patch only stops a held line
  from waiting for a newer one.
- **First proof:** the first Hermes upgrade after v0.21.5.
- **Delete when:** upstream sends a held progress line once its edit
  interval passes, proven by the Docker rung's `use the terminal twice`
  turn on an unpatched image.

## The screen's Take over is the image's, not Hermes'

- **Observed:** phase 4 (`images/bridge/src/screen.rs`). The screen
  proxies raw RFB from Hermes' desktop socket and passes input only from
  the viewer holding control. Hermes' own take-over lease (its
  dashboard's ticketed display socket) is not used, so the agent's
  computer-use tools do not know a person holds the screen.
- **Risk:** a person and the agent move the pointer at once.
- **First proof:** a person taking over while a computer-use turn runs.
- **Delete when:** the screen goes through Hermes' lease (its ticketed
  `/api/display/ws`, or a lease the image can set), proven by a turn
  that waits while a person holds control.

## Each agent's Hermes profile config is rewritten at every boot

- **Observed:** phase 4 (`hermes-boot`). A profile's `config.yaml` is
  its model block (tier, the model intercept, `x-fragment-agent`),
  written whole at each boot; anything Hermes or the agent wrote there
  is lost.
- **Risk:** an agent's own `hermes config set` lasts until the computer
  sleeps.
- **First proof:** an agent that changes its own Hermes settings.
- **Delete when:** an agent's settings live in its fragment (its
  `agent.json`, phase 5's agent template) and the boot merges them into
  the profile's config rather than replacing it, proven by a setting
  that survives a wake.

## The swap's HTTPS is proven only under wrangler dev

- **Observed:** phase 4 (`cell/src/computer.rs` `egress_swap`). The
  computers lane drives the swap from the stub's scripted agent over
  plain HTTP; the real-Hermes lane sends a stock `curl https://…` from
  Hermes' terminal, with the placeholder from its environment, through
  `interceptOutboundHttps` with Cloudflare's local CA. Both send the
  swapped request to `FRAGMENT_SWAP_UPSTREAM`, not a real provider, and
  neither runs on Containers. The four operator keys and the Google
  connection have never reached their real vendors through it.
- **Risk:** Containers' CA, or a real provider's TLS and auth, differ
  from the local proxy's, so a connection works in dev and not hosted.
- **First proof:** the first hosted computer using a connection.
- **Delete when:** the hosted lane makes one swapped call to a real
  provider from a computer on a preview deployment.

## An operator key is metered per call, not per what its vendor counts

- **Observed:** Paul, 2026-10-04 (`fragment_core::price::DEFAULT_KEYS`).
  The swap meters one `key` unit a call the provider answered. Perplexity's
  Sonar Pro briefs also bill tokens and a request fee ($0.02 to $0.04 a
  brief at list), priced as a $0.005 search; xAI's X Search bills posts and
  profiles fetched, priced as a 20-post call ($0.12); ElevenLabs bills
  minutes of music (and characters of speech), priced as a minute ($0.15).
- **Risk:** a brief or a long composition costs the operator more than
  it charges; a short jingle or a speech call charges the person more than
  it cost.
- **First proof:** a month's vendor invoice against the ledger's `key`
  rows for that key.
- **Delete when:** the swap reads each answer's usage (a header or the
  body, as the model route reads tokens) and meters the vendor's own
  units, each catalog row naming how, proven by a lane whose upstream fake
  answers usage and is charged by it.

## Hermes keeps its providers' variable names from its terminal

- **Observed:** Paul, 2026-10-04 (`images/hermes/boot`: `hermes.rs`
  `credentials_sh`). Hermes v0.21.5 never passes a name of its own
  providers' keys to its terminal (`_HERMES_PROVIDER_ENV_BLOCKLIST`:
  `PERPLEXITY_API_KEY`, `XAI_API_KEY`, `ELEVENLABS_API_KEY` among them),
  whatever `terminal.env_passthrough` lists. The image also writes them to
  the profile's `credentials.sh`, which its terminal sources as a session's
  shell starts (`terminal.shell_init_files`), so they are in its snapshot.
- **Risk:** a Hermes release that scrubs those names from the snapshot, or
  ignores `shell_init_files`, takes the operator keys from the agents'
  terminals (their skills say the key is not offered); and one sourced at
  a session's start keeps a value removed meanwhile until the session
  ends (a placeholder then refused at the swap, never a key).
- **First proof:** the hermes lane's check that a stock curl in Hermes'
  terminal finds `$PERPLEXITY_API_KEY`, failing on a Hermes upgrade.
- **Delete when:** Hermes lets a profile pass a name it keeps (an
  allowlist of its own), or the catalog's names for those providers are
  ones Hermes does not keep, proven by the same check with no
  `credentials.sh`.

## Own keys are kept by the person's computer

- **Observed:** Paul, 2026-10-04 (`computer.rs` `own_keys`). A person's
  own key for an `own` provider is sealed in their computer's Durable
  Object, since a person has one computer (decision 13) and the swap
  resolves there.
- **Risk:** a person with a second computer gives their key again; one
  whose computer is deleted loses it.
- **First proof:** a second computer per person.
- **Delete when:** own keys live in the person's own cell (docs/secrets.md)
  and every computer of theirs asks it, proven by two computers swapping
  one key.

## A connection's state comes from Pipes' connected-account API, untried on staging

- **Observed:** Paul, 2026-10-04 (`keys.rs` `pipes_state`). A guest is
  given a connection once `GET
  /user_management/users/{user}/connected_accounts/{provider}` says
  `connected` (workos.com/docs/reference/pipes/connected-account), so no
  token is minted to tell. Spike S5 proved the token and authorize calls
  on staging, not this one; the fake answers as the reference does.
- **Risk:** staging answers it otherwise (another path, a 404 for every
  account), and no agent is ever given Google.
- **First proof:** the hosted lane with a connected Google account.
- **Delete when:** the hosted lane reads a connected account's state on
  staging, proven by its guest given the connection.

## The swap reads a request's body whole

- **Observed:** phase 4. `egress_swap` reads the guest's body (at most
  32 MiB) before it sends it on, as `egress_api` does, rather than
  streaming it.
- **Risk:** an upload to a connection (Drive, a large attachment) over
  32 MiB is refused, and a large one holds the isolate's memory while it
  goes.
- **First proof:** an agent uploading a large file through a
  connection.
- **Delete when:** the swap streams the body (its length passed on),
  proven by an upload larger than the cap.

## An agent's sync and deploy reach code.storage directly

- **Observed:** decision 17's branch (`claude/skills`). The fragment CLI
  in our Hermes image acts as its agent through the API egress, with no
  key (cli/GUIDE.md, "As an agent"), but `fragment sync`, `deploy`,
  `rollback` and `drafts` still go to code.storage itself, with the
  15-minute, repo-scoped token the platform mints for the agent.
- **Risk:** that token is a credential in the guest, which otherwise
  holds none (decision 43), for as long as the command runs; and the
  local lanes cannot prove an agent's deploy at all, since a container
  under `wrangler dev` cannot reach the dev stack's code.storage fake.
- **First proof:** phase 6's exit, an agent building and deploying an
  app from a chat, on a preview.
- **Delete when:** a computer's sync and deploy go through its API egress
  (the files and deploy routes, or a code.storage intercept that swaps
  the token in), proven by the hermes lane deploying an app from a chat.

## `fragment rollback` right after a two-step deploy picks its first step

- **Observed:** 2026-10-03, deploys after a rollback. code.storage merges
  three ways, so a deploy whose `live` has diverged from `main` (a
  rollback's restore commit, then main moved) first restores live to
  their merge base, then merges main in, which then takes main's files
  whole (`fragment_core::codestorage::Promotion`). Every deploy is the
  cell's (`go_live`, `POST /api/f/{name}/deploy`; `fragment deploy` asks
  it), which holds its plane lock across both moves, so a push webhook
  for the first waits and pins the second: the first is never served.
- **Risk:** `fragment rollback` without `--to` right after such a deploy
  restores the first move's files (the deploy the earlier rollback
  undid), not the files the rollback served (by first parents, as the
  fake lists; the service's date order picks among main's commits, the
  contract's question 4).
- **First proof:** `fragment drafts` after a deploy that followed a
  rollback listing its "(first, live back to …)" commit as the one before
  live, and a bare `fragment rollback` landing on it.
- **Delete when:** `rollback`'s default target is the commit live served
  before its tip (skipping a deploy's first step), or code.storage merges
  with the source's tree.

## An open card keeps its computer awake, and any other restart under it still loses its card

- **Observed:** 2026-10-05, Paul on p5 missed an approval card's hour and
  his agent stopped answering: the idle sleep under the card cut its
  turn, and Hermes folded every later message into the cut request,
  asking its approval again. The bridge now holds the keepalive while a
  turn waits on its card (images/bridge/src/engine.rs, `finish`;
  docs/bridge.md, "A card keeps its computer awake"), the P6 stopgap
  Paul agreed (docs/durable-computers.md). The fold itself is closed by
  P5 (2026-10-06: the boot closes a cut turn in Hermes' session, and the
  next turn is told what was cut; `a_turn_cut_by_a_restart_is_closed_and_told`
  in images/bridge/tests/docker.rs).
- **Risk:** an unanswered card costs its life awake (an hour by default;
  a runtime may ask up to `PROMPT_TTL_MS_MAX`, a day). A restart for any
  other reason while a card is open (an owner's sleep, a crash, a deploy,
  an image pinned) still cuts the turn and closes its card `expired`
  before its `expiresAt`: the owner's answer has no turn to go to (F9 of
  docs/explorations/pi-durable.md).
- **First proof:** a computer's awake time dominated by turns waiting on
  cards; or an owner who answers a card after a restart and sees nothing
  happen.
- **Delete when:** P6 lands: a card outlives its turn (a late answer
  starts a new turn, told what was cut), so the keepalive can be let go
  while a card waits again, proven on the real-Hermes lane by a sleep
  under a card whose answer, after the wake, is acted on once.

## A running browser's databases under Hermes' home are saved hot

- **Observed:** 2026-10-06, the hosted hold (docs/durable-computers.md,
  "The hold is answered once the desktop has drawn"). A Chromium the
  agent runs with its default profile keeps it under Hermes' home
  (`/data/hermes/.config/chromium`), not the work, and holds its SQLite
  databases there in SQLite's exclusive locking mode while it runs, so no
  copy of one moment can be had. Our image keeps such a database hot
  (images/hermes/boot/src/held.rs, `copy_all`; its `held` event's
  `locked`) and copies and answers the rest, rather than answering
  nothing and leaving every database hot, as it did before.
- **Risk:** a save taken while that browser writes one may carry it torn:
  the browser then opens a damaged cache or history; and a wake that
  restores the save from its backup (not a snapshot) runs the image's
  check, which `quick_check`s every SQLite database under `/data` but the
  work, and would find that save unusable and fall back to the one
  before.
- **First proof:** a `held` event whose `locked` names a database Hermes
  keeps (not a browser's); or a `restore.checked` with code 3 naming a
  browser's database, or a rollback after one.
- **Delete when:** every browser the agent runs keeps its profile in its
  work (`/data/work/<profile>`, as the desktop's browser already does: the
  seam's rule, docs/computers.md), so nothing under Hermes' home is held
  locked by one, and `locked` is empty on the real-Hermes lanes.
