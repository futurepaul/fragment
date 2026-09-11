# fragment

A fragment is a folder of files, a SQLite database, some URLs, and an inbox —
wrapped around exactly one problem. This repo is the second implementation of
the idea (the first was a throwaway Cloudflare prototype, never published).
This one is built on [celld](https://celld.dev): self-hosted Durable Objects
that keep their state in a bucket you own, and on
[code.storage](https://code.storage): one git repo per fragment, which is
where the files live.

There is no agent inside a fragment. A fragment is a *place*: agents and
people use the `fragment` CLI to put files, code, workflows, and permissions
into it. What makes it different from a static site host is that it is
stateful, multiplayer, and can wake itself up.

## The two pieces

**`fragment` — the CLI (Rust, `cli/`).** The whole control surface. An agent
with this CLI and a nostr key can do everything: make a fragment, sync a local
folder into its repo, deploy, set secrets, grant access, trigger workflows,
read the event log. `fragment guide` prints the agent-facing skill doc. There
is no other API to learn.

**The runtime (JavaScript, `runtime/`).** One Worker + Durable Object bundle,
deployed once per host fleet. Every fragment is one **cell** (a Durable
Object addressed by name, with its own SQLite). The runtime is shared
machinery; fragments are data. Upgrading the runtime upgrades every fragment's
platform; a fragment's own content and code never touch other fragments.

**Two hosts, one bundle.** The runtime is plain Workers+Durable Objects code:

- **celld** (self-hosted): `scripts/dev` for local, a node + bucket + Caddy
  for production. State lives in a bucket you own, RPO=0.
- **Cloudflare** (zero-ops): `scripts/deploy-cf` — same bundle, the Worker
  Loader comes from a `worker_loaders` binding, and `FRAGMENT_HOST_KIND=cf`
  selects CF's module-object form. State lives in DO storage on CF's edge.

The CLI doesn't care which host it talks to; `--host` (or `FRAGMENT_HOST`)
picks. An org that wants Cloudflare's isolation deploys the bundle to their
own account and hands out the CLI.

```
agent ──fragment CLI──▶ celld public listener ──▶ router (Worker)
                            │                        │  /f/<name>/…  → fragment <name> served from live@SHA
                            │                        │  /api/…       → control (NIP-98 signed)
                            │                        ▼
                            │                 FragmentCell (one per fragment)
                            │                        │  pins + tree index / workflows / secrets / inbox / events
                            ▼                        ▼
                     bucket (MinIO locally — the CI image; S3/R2 in prod)   code.storage git (file truth:
                     = celld state only. RPO=0.                              main + live refs, per fragment)
```

## Files: code.storage is the truth

One git repo per fragment on code.storage. `main` is the working branch;
`live` is the blessed serve point. The cell never owns file bytes:

- **Pins.** The cell pins `main@SHA` for working-copy reads and `live@SHA`
  for serving. Pins move on webhooks (HMAC-verified push events), on the
  cell's own commits, and on a 5-minute poll backstop. Webhook `after`
  values are never trusted for the pin — the branch head is re-read — so a
  late or reordered delivery cannot move a pin backward.
- **Tree index.** Path/size/mode/last-commit per pinned SHA, in cell
  SQLite. Metadata only: **no file bytes ever persist in the cell or the
  bucket.** The bucket holds celld state; nothing approaches its
  conditional-write limits by construction.
- **Contents.** Streamed from code.storage on demand through a RAM LRU
  (64 MiB total, per-entry cap; oversized files stream through uncached).
  The stream path never buffers a whole body and has no size ceiling.
- **Writes.** Two planes, one protocol: workflows commit server-side via
  the cell-held JWT (expected-parent CAS, content dedup — identical writes
  are recorded no-ops, so replays commit exactly once), and the CLI commits
  directly with a short-lived **storage token**
  (`GET /api/f/{name}/storage-token`, editor+; repo-scoped, git-only
  scopes, minutes-scale expiry, every mint on the event ledger). Conflicts
  keep your copy and save the remote one beside it as `.conflict-*`; the
  old content also lives on in git history.

Repo identity is the url-form id code.storage returns at create time — the
human name 404s on repo-scoped calls — recorded in the cell at create and
carried by every call and minted token after that.

## The manifest

`fragment.json` is a git file at the repo root — the repo is its authority.
The CLI commits it (`manifest-set`, `grant`, `revoke` are edit-and-commit);
the cell caches the pinned copy and serves the parsed object. An invalid
manifest at a new pin is rejected with a ledger event and the last good
cache keeps serving. Shape:

```json
{
  "name": "dose-tracker",
  "visibility": "viewers",
  "editors": ["npub1…"],
  "viewers": ["npub1…"],
  "workflows": [
    { "name": "daily-digest", "file": "workflows/digest.mjs", "cron": "0 8 * * *" },
    { "name": "on-upload", "file": "workflows/upload.mjs", "trigger": "inbox" }
  ],
  "secrets": ["GRAFANA_TOKEN"]
}
```

- **visibility**: `public` (anyone can view), `viewers` (listed npubs), or
  `link` (anyone with the `?view=<token>` share link — the default, good
  for "send a human a link"; a valid token also mints a scoped cookie so
  subresources load). Viewing never grants editing. Editing
  (sync/deploy/secrets/rotate) requires the owner or an editor npub,
  proven per request with a NIP-98 signed event.
- **workflows**: `cron` (5-field, UTC, via the cell's own durable alarm —
  survives sleep), `trigger: "inbox"` (a POST to the fragment's inbox runs
  it), or `trigger: "files"` (fires when the working copy changes on an
  external push; workflow writes never re-trigger). `fragment run <name>
  <workflow>` triggers manually.
- **secrets**: declared by name only. Values are set with `fragment secret
  set`, stored wrapped (HKDF+AES-GCM under `FRAGMENT_HOST_SECRET`), and
  injected into workflow runs. They never appear in the repo, by rule.

## Deploys, previews, rollback

Deploy = commit + move `live`: `fragment deploy` syncs the folder (one
commit-pack with expected-parent CAS) and fast-forwards `live` to main's
tip (a merge commit after a rollback). `--preview` points an unguessable
ephemeral ref at the pending state instead. `fragment rollback` appends a
restore-commit on `live` whose tree matches an earlier deploy. `fragment
drafts` lists the `live` ref's commit history — deploys are git commits,
so history, authors, and messages come free and nothing expires.

## Multiplayer

The cell terminates WebSockets itself (celld hibernation: sockets survive cell
sleep). The runtime provides **rooms**: named channels with presence,
JSON messages, and one persisted JSON document per room. Browser code loads
`/f/<name>/__rt.js` and gets `fragment.room("notes")` with
`send/on/state/presence`. If the fragment ships `rooms.mjs`, its exported
`onMessage(room, msg, ctx)` runs server-side on each message. That's the whole
model — post-its, shared cursors, live dashboards are all rooms.

## Workflows

A workflow is a `.mjs` file exporting `async run(ctx)`:

```js
export async function run(ctx) {
  const grafana = await ctx.http("https://grafana…/api/…", {
    headers: { authorization: `Bearer ${ctx.secrets.GRAFANA_TOKEN}` }
  });
  const notes = await ctx.files.read("notes/today.md");
  const summary = await ctx.ai(`summarize in one line: …`);
  await ctx.files.write("digests/" + Date.now() + ".md", summary);
  ctx.log("digest written");
}
```

Each run attempt executes on the host's **native Workflows engine** (one
instance per attempt; steps survive restarts), while everything
product-shaped — the runs ledger, triggers, guards, retry classification —
stays in the cell. Author code runs in a **separate loader isolate**, not
inside the cell isolate, so a wedged workflow can't wedge the fragment.
`ctx`: `http` (fetch), `files` (read/write/list the repo — writes are git
commits under CAS, with `{ifSha}` compare-and-swap and blob-identity
suppression), `secrets`, `inbox` (pending messages), `events` (append to the
ledger), `ai` (platform-routed inference, host holds the key), `state`
(per-workflow kv), `log`. Every run appends to the fragment's **event log**
(start, finish, error, output digest) — the ledger is ground truth,
`fragment events` reads it.

## Runs: the failure leg

Success-path machinery (triggers, single-flight, coalescing) is small; nearly
all real-world fragment pain lives on the failure path. The platform carries
that leg so authors don't have to. Everything below is default-on; an author
who learns none of it still gets all of it.

**Every execution is a run row** (`runs` table) — the single record of what
happened: workflow, trigger (`cron|sync|inbox|manual`), input (JSON), status,
attempt, cause chain, timings. Events are the narrative; runs are the facts.

```
 trigger fired ──▶ [guards] ──▶ running ──▶ success
                    │  │            │ retryable error, attempts left
                    │  │            ▼
                    │  │         backoff ──(alarm)──▶ running, attempt+1
                    │  │            │ attempts exhausted | terminal error
                    │  │            ▼
                    │  └─▶       held   (input + error parked, replayable)
                    ▼
             blocked | skipped   (paused / rate / cycle / single-flight)
```

The transitions live in exactly one module (`runtime/ts/runs.ts`), as
straight-line code: guards in a fixed order, one attempt per invocation,
backoff waits scheduled on the cell's single alarm — never a blocking sleep.

**Retry.** Failed runs are classified retryable (network, timeout, 429/5xx)
or terminal (code errors, 4xx, bad parse). Retryable failures re-run with
exponential backoff + jitter (default 3 attempts, 30s base, 5min cap;
`retry` in the workflow config tunes it, `retry: false` opts out). A run
killed by a host restart (lease expired without a terminal status) counts as
retryable — crashed ≠ dead.

**Held, never dropped.** When attempts exhaust or the error is terminal, the
run parks as `held` with its input and last error. `fragment replay <name>
<run-id>` re-executes it with the original input. This is the dead-letter
queue, as a status on the ledger rather than a second system.

**Auto-pause.** Five held runs in ten minutes trip the circuit breaker: the
workflow pauses itself (loud `workflow.auto-paused` event; the human is the
one escalation level). Manual runs still work while paused — pause means
"stop reacting, keep debugging." `fragment pause|unpause` does it by hand.

**Loops, three layers.** Two fragments watching each other can only livelock
(pull-based watching can't deadlock), and three cheap layers bound it:
1. *Write-suppression* — `ctx.files.write` with unchanged content is a
   recorded no-op (`{deduped: true}`). Copy-loops die on pass two.
2. *Hop budget* — runs carry a cause chain `{origin, depth}`; `ctx.http`
   stamps `x-fragment-hops: depth+1` on every outbound request; an inbox
   trigger above 16 hops refuses to fire and records `cycle.detected`.
   Workflows that intend recursion set `cycles: true`.
3. *Rate ceiling* — more than 120 auto-triggered runs in a rolling hour
   trips the auto-pause breaker. The only layer that catches a loop which
   genuinely mutates content every pass (the AI-agent ping-pong shape).

**Inbox, hardened at the door.** Pending inbox messages cap at 1000; beyond
that the POST gets a 429 and the ledger gets `queue.rejected` — overload is a
signal, not memory pressure. Inbox-triggered runs ack their own messages
when they drain them.

**Delivery contract, one paragraph.** Any trigger may fire more than once
for one logical change. Files are safe by construction (suppression);
external side effects should be keyed by cause (the once pattern in
`fragment guide`). Sync coalescing is debounce — `debounceMs` in the
manifest names the knob.

`fragment runs <name> [--status held]` reads the table;
`fragment pause|unpause|replay` drive it.

## The event log

Everything that changes a fragment appends to `events`: pin refreshes,
deploys, token rotations, secret sets, grants, workflow runs, inbox arrivals,
webhook deliveries (deduped, redeliveries recorded as no-ops), notify
enqueues. The log is the answer to "what happened" and the runtime's own
memory. (Learned the hard way in the first prototype: never let a report
disagree with the ledger.)

## Auth, plainly

- One keypair per CLI user: `fragment login` generates a nostr secret key in
  `~/.config/fragment/`.
- Every control request carries a NIP-98 event (kind 27235, url+method+payload
  tags), verified in the runtime with pure-JS secp256k1 schnorr.
- Each fragment also *has* an npub — the keypair is generated client-side at
  create and crosses the wire exactly once, inside the creator's
  authenticated request; the cell stores it wrapped (HKDF+AES-GCM under
  `FRAGMENT_HOST_SECRET`) so fragments can be addressed and can sign later.
- No email, no passwords. The `?view=` token exists for "send a link to a
  human" (visibility `link`).
- code.storage auth is customer-signed JWTs: the host holds only the org
  private key (`PIERRE_PRIVATE_KEY`, via systemd LoadCredentialEncrypted on
  celld) and mints short-lived tokens — repo-scoped and git-only for the
  CLI's direct commits; the CLI never sees the org key.

## Isolation and ingress

- A fleet runs one deployment (the runtime). Fragments are cells: one SQLite
  each, one writer each, fenced by celld epochs. A broken fragment can only
  damage its own database.
- Workflow code runs in loader isolates with no access to other cells.
- The internal plane (`/__internal/f/<name>/…`) serves exactly one caller:
  ctx loopback from loader isolates and the native workflow driver,
  authenticated per cell by run tokens. The registry and cell-init are never
  reachable over HTTP — only via the router's own DO binding. Hosts that
  want a second lock set `FRAGMENT_HOST_SECRET` (celld:
  `CELLD_VAR_FRAGMENT_HOST_SECRET`; CF: wrangler secret), which every
  loopback call must then carry.
- celld does not terminate TLS and does not authenticate users. That is the
  ingress's job (Caddy in prod; nothing locally). Wildcard subdomain →
  `Host: <name>.frag.example` reaches the same router; path-based URLs work
  everywhere, so subdomains are sugar, not load-bearing.
- cellds don't talk to each other: there is one fleet; cells can't reach other
  cells except through the public URLs, where normal auth applies.
- Notifications (manifest `notifyUrls`) enqueue onto celld Queues; a separate
  tiny deployment, `notify-relay/`, is the queue's one consumer and POSTs
  each frame with the cross-fragment hop headers. Delivery is at-least-once;
  receivers key effects by cause.

## Local dev

No docker, no cloud account, no bucket:

```
scripts/dev up       # celld dev (local object store) + the mock code.storage
scripts/dev deploy   # rebuild runtime/ and restart the dev host
scripts/dev down
```

The whole stack is native processes: `celld dev` gives the fleet a persistent
local object store, and `runtime/test/mock-codestorage.mjs` stands in for
code.storage — repos (url-form identity), commit-packs with expected-parent
CAS, branch moves, signed push webhook delivery — with a throwaway org key
the mock uses to verify the JWTs the runtime mints. What the mock can't
stand in for (the real service's scale, its dashboard) is the only
production-only surface left. State lives in `.dev/celld-state` (cells,
reached through a `runtime/.celld` symlink so celld dev's own watcher never
sees its state writes) and `.dev/cs-mock-root` (repos); `dev wipe` resets
both together.

## What this deliberately does not have (yet)

- No in-fragment planning agent. The mind is external; fragments are places.
- No fragment-to-fragment private channels. They use public URLs + npub auth
  like everyone else.
- No CRDT file sync. Sync is last-writer-wins with conflict copies, on
  purpose — the old content lives on in git history.
- No large-media tier. All files ride the chunked commit-pack; the read path
  streams with no size ceiling (Range pass-through is recorded as future
  work, not built).

## Non-goals

KV-style global state, a second blob tier (deleted with blobsd — git is the
file store now), multi-tenant hostile isolation (celld is alpha; fleet = one
trust domain), replacing the first prototype's hosts.
