# MODEL — the fragment-next core model

Status: proposed 2026-09-23, revised the same day by the phase 1 spikes
(`spikes/README.md` at the tag `celld-final`). This is the target shape the runtime is hard-cut to
(ROADMAP decisions 2-4 and the five changes Paul approved). Every
mechanism names the celld primitive it uses; read with the celld docs
(https://celld.dev/docs/, v0.5.1).

## The five changes

1. **Files in git, state in SQL, history in channels.** Every file is in
   git (code.storage): code, templates, documents, uploads, generated
   images and video. git is the one file tree, its history, and the sync
   to computers and local folders. A file of 1 MiB or more is stored in
   git as a pointer (hash, size, type) whose bytes live in Tigris under
   that hash; syncing resolves pointers, so computers and agents see
   real files. App state lives in SQL. What happened lives in channels.
   File-based apps (a vault, a wiki) stay first-class: for them the files
   *are* the state, and operations read and write them through
   `this.files`, `call.files`, and `job.files` (phase 2 slice E; reads at
   `main`, the working copy).
2. **One execution primitive: the operation.** Named, schema-typed,
   role-checked, idempotent by operation id, ledgered. Every trigger
   (browser, CLI, agent tool call, cron, webhook, channel message, file
   change) is a way to invoke an operation.
3. **One log: channels.** Append-only, ordered, paged, per fragment. The
   audit trail, the inbox, room messages, and chat transcripts are all
   channels. Presence stays ephemeral.
4. **Membership is live cell state; every actor is an identity.** People
   and agents are identities (`id:…`) holding keys, resolved live by the
   registry on every signed request whose answer depends on who is asking
   (a page everyone who may see it gets alike asks nothing). Grants,
   invites, and revocations are transactional and take effect on the
   next request. Each fragment is its own browser origin.
5. **Agents are add-ons a fragment declares** (ROADMAP decisions 19–20);
   a fragment that declares none carries nothing of them. Agents are
   hosted members with durable turns. Computers (decision 21) went at the
   cut (tag `celld-final`) and come back on Cloudflare
   (docs/cloudflare-v1.md, phase 4).

## Anatomy of a fragment, in celld terms

| Part | celld primitive | Holds |
|---|---|---|
| Supervisor | a Durable Object (`Fragment`), platform code in Rust | members, invites, the audit copy of the operation ledger, channels, schedules, wrapped secrets, the file-plane pins and tree index, the live code pin |
| App | a **Durable Object Facet** named `app@<incarnation>` (each life of a fragment's name has its own; `app` for fragments made before 2026-09-27), started through the **Worker Loader** at `live@SHA` from `platform.js` (platform code) wrapping the author's `App` class | the author's SQLite database (`this.ctx.storage.sql`), which the supervisor's tables never share, and the mutation ledger `_fragment_ops` beside it |
| Files | code.storage git (wire contract unchanged) | every file; one of 1 MiB or more as a pointer to its blob |
| Blobs | celld's **R2** binding, which stores objects in the fleet bucket (Tigris) under `r2/<bucket>/`; no Cloudflare R2 | the bytes of large files, content-addressed by SHA-256; only blobs a pointer at a branch tip references are kept |
| Jobs | **Workflows** (each instance is a cell) | multi-step or long operations, and agent turns |
| Deliveries | **Queues** (at-least-once, dead-letter queue, `message.id` as idempotency key) | outbound webhooks and web push |
| Schedules | the supervisor's alarm (facets cannot set alarms) | per-fragment cron and retry backoff, multiplexed onto one alarm |

The supervisor is on the path of every call into the app: it checks the
caller's role, validates input, applies quotas, and ledgers the result
before the app code runs or its result leaves. The app facet is built
with an empty binding set, so it reaches only what the supervisor hands
it in `env`: capability objects (`ctx.exports.X({ props })`) for files,
channels, blobs, and AI, plus a
`globalOutbound` Fetcher that brokers egress (hop headers, allowlist,
secret injection) or `null` to remove ambient network entirely.
`WorkerCode.limits` bounds `cpuMs` and `subRequests` per invocation.

Why a facet and not our own SQL loopback: a facet gives author code a
real, private SQLite database that cannot see platform tables.
finite-next filtered SQL strings with a regex instead; that is deleted.
Since celld v0.6.0 a facet is a SQLite file of its own with its own
replication stream, as a facet is on Cloudflare: a facet write no longer
copies the facet's database into the root or joins a root transaction.
So the supervisor's transaction never encloses a facet call (under
v0.5.1 it could not either: spike 2 found the image copy failing past
~1.6 MB and a capability call inside it deadlocking), and atomicity lives
inside the facet: platform code runs each mutation and its ledger row in
the facet's own `transactionSync`.

## Operations

Declared in `fragment.json`:

```json
{
  "operations": {
    "list":     { "kind": "query",    "role": "viewer", "input": { "type": "object", "properties": {} } },
    "add_todo": { "kind": "mutation", "role": "editor", "input": { "type": "object", "required": ["text"], "properties": { "text": { "type": "string", "maxLength": 500 } } } },
    "digest":   { "kind": "job",      "role": "editor", "input": { "type": "object" } }
  }
}
```

Implemented as methods of the author's `App` class (the facet answers
RPC methods), each receiving `(input, call)`, where `call` names the
caller (`principal`, `role`) and, in a mutation, collects its effects
(`call.publish(channel, body, kind)`; phase 2 slice C):

- **query** — read-only; not ledgered; may be async and may call read
  capabilities (files, channels, blobs); may be re-run on change signals
  to drive live views.
- **mutation** — synchronous, over the app's own SQL only: no `await`, no
  capabilities, no network. Platform code in the facet runs it and its
  ledger row in one `transactionSync` keyed by (principal, operation id).
  What it wants to happen next (channel records now; notifications,
  starting a job, a vault's file writes in later slices) it declares as
  **effects** through `call`; the ledger row keeps them, so the ledger is
  also an outbox. Ledger rows are kept seven days: a replay after that
  runs again.
  An **ephemeral** mutation (`"ephemeral": true`, for a "latest value"
  write such as a screen's frame) keeps no ledger row, which is a few
  hundred bytes of the app's 16 MiB a call. It keeps: its writes and its
  answer in one `transactionSync` (a throw rolls them back), the role and
  schema checks, the database's cap, and the change signal to live
  views. It gives up: the replay (the same id runs again, with any input:
  a retried call, job step, or triggered run may run it twice), its
  effects (their outbox is the ledger row, so publishing, pushing, or
  writing files rolls it back, 422), the `ops` record, and the pending
  row: nothing is left to settle after a crash (its writes committed or
  did not).
- **job** — runs as a Workflow instance, one per run; its method gets
  `(input, job)`, and each `await job.call(op)` (a query, a mutation, or
  another job), `job.fetch(url)` (the external effect: secrets are added
  at the egress point), `job.publish`, or `job.sleep` is a durable step,
  so retries, backoff, and sleeps are celld's. The body runs in the app
  facet by replay up to its next step; the Workflow, platform code, takes
  the step through the supervisor, which keeps each step's answer (the
  replay reads them from there, and a step retried after its answer was
  lost is not taken twice). fragment's failure leg (held runs,
  replay, auto-pause, the hop budget) is on runs (phase 2 slice D;
  `docs/api.md`, Jobs and triggers). `waitForEvent` (approvals, invites)
  comes with agents.

The call path, for every trigger:

```
trigger ─▶ principal + op id + input ─▶ Fragment (supervisor)
   role check ─▶ schema check ─▶ pending row (principal, op, depth; its seq is the run)
   ─▶ App facet __mutate(id, name, sha(input), input, run)
      facet transactionSync: ledger lookup (a week's window)
        ├─ same id, same input      → the stored result and run (a replay)
        ├─ same id, different input → reject (conflicting body)
        └─ new                      → the author's method, then its ledger row with the run
   ─▶ the pending run's effects, checked in Rust, applied keyed by (id#run, index)
   ─▶ `ops` record + pending row dropped ─▶ change signal to subscribers
```

A supervisor that dies after the facet committed loses nothing: its
pending row outlives it, and on activation the supervisor asks the facet
about each pending id and applies the run it recorded (or drops the row
when the facet never committed that run). Who called, and what, are the
pending row's facts, never the app's: the app can write its own ledger
table, so a row it writes there is never applied. A replay applies
nothing again, since only a pending run is applied; a refused effect
settles its run for good, so it cannot block the app; a passing failure
leaves the run pending for a later try (docs/api.md, Apps).

Triggers: an HTTP call from the UI (`POST /__op/<name>`), `fragment call`
from the CLI, an agent tool call (an operation's schema *is* its tool
schema), a cron entry, an inbox webhook, a channel message, and a pin
move (file change). Cron entries, channel messages, and pin moves are
declared in `fragment.json`'s `triggers` and start runs as the fragment's own key;
a triggered mutation is a run of one step, so it retries and is held
like a job. A custom `App.fetch` stays
available for routes that are not operations (dynamic HTML, file
serving), but agents and the platform only use operations.

## Channels

A channel is a supervisor table: `(channel, seq)` → `{at, principal,
kind, body, op_id}`, append-only, with a per-channel retention policy.

- Built in: `events` (audit, platform-written), `inbox` (webhooks,
  appended by the inbox route; its pending records are the runs they
  triggered that have not succeeded, and at 1000 a post is 429), `ops`
  (`{op, id}` per applied
  mutation: the ledger's public view, and the marker that its effects
  were applied), and app-declared channels (a chat transcript, a room's
  messages), declared in `fragment.json` with the role that may read
  them. Clients never append: records come from the platform and from
  mutations' effects, except on a channel the fragment declares
  postable, where the platform appends a member's record for them
  (ROADMAP decision 18); one that says `"signedIn": true` takes posts
  from signed-in posters only (an anonymous visitor holding the role is
  refused).
- Subscribers: hibernatable WebSockets that resume from a cursor (a
  WebSocket closes when the cell moves, so clients reconnect with their
  last `seq`); channel-triggered operations; agents; and outbound
  deliveries (webhooks, web push) through a Queue.
- Presence is per socket and never stored.
- fragment's per-room persisted document becomes a query operation plus
  a change signal; `ctx.state` becomes the app's SQL.

## Principals and membership

- A principal is an identity: a person, an agent, or a fragment, with
  one or more public keys in the registry (finite.computer's BANKS
  model, ROADMAP decision 15). The CLI proves a key with NIP-98 and the
  registry names its identity; a browser has a platform session that
  maps to the person (phase 4 slice B); an agent signs with its cell's
  key; a fragment with its own key (unregistered: it is the principal of
  its own triggered runs only). The platform holds no person's private
  key. An agent's designated owner reads what the agent can read, as a
  viewer, and never acts through it (docs/api.md, Principals and access).
- Members and invites are supervisor tables. A grant or revoke is one
  transaction; a revoke closes that principal's sockets. The `events`
  channel records every change. Membership leaves `fragment.json` (a git
  commit can no longer grant access). Only the owner manages members,
  invites, visibility, and tokens; a member may leave. Members name
  identities, so replacing a key rewrites no grant. Each identity's list
  of fragments is an index in its own `Principal` cell, fed from the
  fragment's outbox (phase 2 slice B; keyed by identity since phase 4
  slice A). A request sends at most one round of the outbox (32 lists at
  once, its own change first) and the alarm the rest, so no request waits
  on a list per member. A delete ends the life at once and leaves its
  members' lists, the app's database and the blobs to the alarm
  (cell `ended.rs`).
- Visibility: `public`, `link` (a token that is a capability), or
  `members`. **A public fragment is a website anyone can use, writes
  included** (a public chat, a guestbook): an operation may declare
  `"role": "public"`, callable by anyone who can see the fragment. An
  anonymous visitor gets an ephemeral principal (a random value held in
  an HttpOnly cookie on the fragment's origin; the principal is `anon:`
  plus its hash), so their writes are attributed and rate-limited like
  anyone else's. On a `link` or `public` fragment the link holder counts
  as a viewer. Operation ids belong to their caller: the ledger keys a
  mutation by principal and id.
- Origins: each fragment is served from
  `<label>--<username>.fragment.boats` (its old host on fragment.club
  redirects there); the platform (login, share sheet, invites, the share
  header) from `fragment.club`, another site (ROADMAP decision 23).
  celld does not vouch for `Host`, so the router checks the hostname
  against the configured suffix before it trusts it.

## Agents

An agent is a fragment (kind `agent`) with an identity of its own, owned
by a person and run by a computer of theirs: today a Hermes profile,
whose bridge turns the records of the channels it follows into turns
(docs/cloudflare-v1.md, decisions 14 and 15; docs/computers.md;
docs/chat-records.md). It acts on fragments through the platform's API
as their member, for whoever asked: every call names them (`for`), and
acts with the lower of their role and the agent's cap (ROADMAP decision
17). The in-fragment goose agent (a loop in a Durable Object of its own,
a fragment's `agent` block, `job.agent`) went on 2026-10-06 (issue
#156): one runtime, with one set of turn semantics.

## Computers

Computers (an identity per machine, a Sprite each behind a `Computer`
cell, `fragment computer serve`, builder workspaces) went at the cut
(tag `celld-final`). The generic Computer comes back on Cloudflare
(docs/cloudflare-v1.md, phase 4).

## Limits (initial; each enforced and tested)

| Limit | Value | Why |
|---|---|---|
| operation input | 256 KiB | a request, not an upload (uploads go to the file plane) |
| inline file in git | under 1 MiB | a file of 1 MiB or more is a pointer to a blob |
| operation or step result | 1 MiB | the Workflows step-result limit |
| channel record body | 64 KiB | records are messages, not files |
| channel page | 1000 records | bounded reads |
| app facet database | 16 MiB | Paul's cap (large files belong in git storage). It was set when celld v0.5.1 copied the whole facet image into the root after each changed turn (spike 2: ~9 ms a mutation at 1 MiB, ~18 ms at 16 MiB, ~55–61 ms at 64 MiB, and ~2 MB of replication a mutation at 64 MiB); since v0.6.0 a facet replicates its own changes: the same benchmark (JS, 30 mutations, `celld dev`, 2026-09-26) gives ~15 ms a mutation at 1, 16, and 64 MiB alike and ~2 KB replicated a mutation, against 8.5 / 17.6 / 54 ms before (a small app pays ~7 ms more; a supervisor-only write is ~8 ms either way). Size no longer costs per write, so the cap can rise if Paul wants |
| `cpuMs` per invocation | 30 000 | a runaway loop cannot hold the fragment |
| `subRequests` per invocation | 50 | bounds fan-out |
| inbox pending | 1000 | overload is a 429, not memory pressure |
| hop depth | 16 | carried from fragment's loop guard |
| `public`-role calls | 60 per minute per anonymous principal, 600 per minute per fragment | public writes must not become an abuse amplifier; tunable per operation |

## Capacity notes (from the celld docs)

- A process holds at most 256 live Dynamic Workers (255 per script
  generation), so at most ~255 fragments with loaded code are resident
  per node; idle eviction (`CELLD_IDLE_EVICT_S`) must keep that below
  the cap. Measure before production.
- Two or more nodes prove writes through a follower fsync (~25 ms in
  celld's lab) instead of the bucket (~600 ms); run at least two.
- celld self-fences (exit 3) when its lease lapses and needs a supervisor
  that restarts it without an attempt limit, waiting at least one lease
  lifetime (10 s).
- The internal listener is plaintext and unauthenticated beyond the fleet
  HMAC; it must stay on Fly's private (WireGuard) network.

## Spikes (done 2026-09-23; `spikes/README.md` at the tag `celld-final`)

1. **Rust cells** — adopted: workers-rs 0.8.5 covers SQL, alarms, and
   hibernatable WebSockets; the Worker Loader and facets are reached
   through `js_sys`; a ~35-line JavaScript shim adds `extends
   DurableObject` (celld refuses RPC otherwise) and the capability
   `WorkerEntrypoint` classes. ~9 ms once per isolate, 0.05–0.2 ms per
   request.
2. **Facet as author SQL** — adopted with the synchronous-mutation rule
   above; the root transaction is out.
3. **Deterministic agent turns** — passed (a libfx turn as a Workflow,
   SIGKILL-tested), then superseded for agent turns by goose's own
   step loop (gone since: Agents); Workflows stay for app jobs.
4. **celld v0.5.1** — adopted, with an alarm regression fixed in our
   fork; celld v0.6.0 (2026-09-26) fixed it upstream (#228), and the
   fork now carries only our own additions (docs/hardening.md).

## Answered (2026-09-23)

- **Authoring shape:** one `app.mjs` whose exported `App` class (a
  Durable Object class the supervisor starts as the app facet) has one
  method per operation, plus an optional `fetch` for custom routes.
- **Anonymous callers:** public fragments are websites; anonymous
  visitors may read and write through `public`-role operations, with an
  ephemeral principal each (above).
- **Channel retention:** `events` 90 days, `inbox` until acknowledged,
  app-declared channels forever.

## Answered from the spikes (2026-09-23)

- **Mutations are synchronous and return their effects** (spike 2): a
  mutation cannot `await`, call a capability, or fetch; queries and jobs
  can. Paul: yes.
- **The app facet database is capped at 16 MiB** (~18 ms per mutation at
  the cap). Paul: large files belong in git storage, so a 16 MiB cap.
- **Every file is in git; large ones are pointers** (Paul, 2026-09-23).
  Files of 1 MiB or more are pointers in git with their bytes in Tigris
  (code.storage's HTTP API has no LFS path). Syncing a fragment to a
  computer or a folder delivers real files, generated media included.
  Only the latest version's bytes are kept: a blob no branch tip
  references is deleted. A future exception (keeping every version of,
  say, a large Photoshop file) is not needed now.
