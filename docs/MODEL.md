# MODEL — the fragment-next core model

Status: proposed 2026-09-23, revised the same day by the phase 1 spikes
(`spikes/README.md`). This is the target shape the runtime is hard-cut to
(ROADMAP decisions 2-4 and the five changes Paul approved). Every
mechanism names the celld primitive it uses; read with the celld docs
(https://celld.dev/docs/, v0.5.1).

## The five changes

1. **Code in git, state in SQL, bytes in R2, history in channels.** git
   (code.storage) holds what you deploy: code, templates, and deliberate
   documents. App state lives in SQL. Large bytes (uploads, generated
   media) live in R2. What happened lives in channels. File-based apps
   (a vault, a wiki) stay first-class: for them the files *are* the
   state, and operations read and write them through `ctx.files`.
2. **One execution primitive: the operation.** Named, schema-typed,
   role-checked, idempotent by operation id, ledgered. Every trigger
   (browser, CLI, agent tool call, cron, webhook, channel message, file
   change, computer event) is a way to invoke an operation.
3. **One log: channels.** Append-only, ordered, paged, per fragment. The
   audit trail, the inbox, room messages, and chat transcripts are all
   channels. Presence stays ephemeral.
4. **Membership is live cell state; every actor is a key.** People,
   agents, and fragments are principals identified by npub. Grants,
   invites, and revocations are transactional and take effect on the
   next request. Each fragment is its own browser origin.
5. **Agents and computers are participants.** Agents are hosted members
   with durable turns; fragments can own computers (Sprites).

## Anatomy of a fragment, in celld terms

| Part | celld primitive | Holds |
|---|---|---|
| Supervisor | a Durable Object (`Fragment`), platform code in Rust | members, invites, the audit copy of the operation ledger, channels, schedules, wrapped secrets, the file-plane pins and tree index, the live code pin |
| App | a **Durable Object Facet** named `app`, started through the **Worker Loader** at `live@SHA` from `platform.js` (platform code) wrapping the author's `App` class | the author's SQLite database (`this.ctx.storage.sql`), which the supervisor's tables never share, and the mutation ledger `_fragment_ops` beside it |
| Files | code.storage git (wire contract unchanged) | code, templates, documents |
| Bytes | an **R2** binding (the fleet bucket under `r2/<bucket>/`) | uploads, generated images and video, exports |
| Jobs | **Workflows** (each instance is a cell) | multi-step or long operations, and agent turns |
| Deliveries | **Queues** (at-least-once, dead-letter queue, `message.id` as idempotency key) | outbound webhooks and web push |
| Schedules | the supervisor's alarm (facets cannot set alarms) | per-fragment cron and retry backoff, multiplexed onto one alarm |

The supervisor is on the path of every call into the app: it checks the
caller's role, validates input, applies quotas, and ledgers the result
before the app code runs or its result leaves. The app facet is built
with an empty binding set, so it reaches only what the supervisor hands
it in `env`: capability objects (`ctx.exports.X({ props })`) for files,
channels, blobs, AI, and the fragment's computer, plus a
`globalOutbound` Fetcher that brokers egress (hop headers, allowlist,
secret injection) or `null` to remove ambient network entirely.
`WorkerCode.limits` bounds `cpuMs` and `subRequests` per invocation.

Why a facet and not our own SQL loopback: a facet gives author code a
real, private SQLite database that replicates with the supervisor in one
upload and cannot see platform tables. finite-next filtered SQL strings
with a regex instead; that is deleted. The supervisor's root transaction
cannot enclose a facet call (spike 2: celld writes the facet's whole
image into one root row at commit, which fails past ~1.6 MB, and a
capability call inside it deadlocks), so atomicity lives inside the
facet: platform code runs each mutation and its ledger row in the facet's
own `transactionSync`.

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
RPC methods), each receiving `(input, ctx)`:

- **query** — read-only; not ledgered; may be async and may call read
  capabilities (files, channels, blobs); may be re-run on change signals
  to drive live views.
- **mutation** — synchronous, over the app's own SQL only: no `await`, no
  capabilities, no network. Platform code in the facet runs it and its
  ledger row in one `transactionSync` keyed by operation id. What it
  wants to happen next (channel records, notifications, starting a job, a
  vault's file writes) it returns as **effects**; the ledger row keeps
  them, so the ledger is also an outbox.
- **job** — runs as a Workflow instance whose id is the namespaced
  operation id; each `step.do` is a query, a mutation, or an external
  effect, so retries, backoff, `waitForEvent` (approvals, invites), and
  sleeps are celld's. fragment's failure leg (held runs, replay,
  auto-pause, hop budget, loop suppression) moves onto this.

The call path, for every trigger:

```
trigger ─▶ principal + op id + input ─▶ Fragment (supervisor)
   role check ─▶ schema check ─▶ App facet __mutate(id, name, sha(input), input)
      facet transactionSync: ledger lookup
        ├─ same id, same input      → the stored result (a replay)
        ├─ same id, different input → reject (conflicting body)
        └─ new                      → the author's method, then its ledger row
   ─▶ apply returned effects keyed by (op id, index) ─▶ mark the row applied
   ─▶ append to channels ─▶ change signal to subscribers
```

A supervisor that dies after the facet committed loses nothing: the
caller's retry is a replay that returns the same effects, and on
activation the supervisor sweeps ledger rows the facet committed but it
never marked applied.

Triggers: an HTTP call from the UI (`POST /__op/<name>`), `fragment call`
from the CLI, an agent tool call (an operation's schema *is* its tool
schema), a cron entry, an inbox webhook, a channel message, a pin move
(file change), and a computer event. A custom `App.fetch` stays
available for routes that are not operations (dynamic HTML, file
serving), but agents and the platform only use operations.

## Channels

A channel is a supervisor table: `(channel, seq)` → `{at, principal,
kind, body, op_id}`, append-only, with a per-channel retention policy.

- Built in: `events` (audit, platform-written), `inbox` (webhooks; the
  pending cap answers 429), `ops` (the operation ledger's public view),
  and app-declared channels (a chat transcript, a room's messages).
- Subscribers: hibernatable WebSockets that resume from a cursor (a
  WebSocket closes when the cell moves, so clients reconnect with their
  last `seq`); channel-triggered operations; agents; and outbound
  deliveries (webhooks, web push) through a Queue.
- Presence is per socket and never stored.
- fragment's per-room persisted document becomes a query operation plus
  a change signal; `ctx.state` becomes the app's SQL.

## Principals and membership

- A principal is an npub: a person, an agent, or a fragment. The CLI
  proves one with NIP-98; a browser with a platform session that maps to
  the person's key; an agent with its cell's key; a fragment with its
  own key.
- Members and invites are supervisor tables. A grant or revoke is one
  transaction; a revoke closes that principal's sockets. The `events`
  channel records every change. Membership leaves `fragment.json` (a git
  commit can no longer grant access).
- Visibility: `public`, `link` (a token that is a capability), or
  `members`. **A public fragment is a website anyone can use, writes
  included** (a public chat, a guestbook): an operation may declare
  `"role": "public"`, callable by anyone who can see the fragment. An
  anonymous visitor gets an ephemeral principal (a key minted per browser
  and held in a cookie on the fragment's origin), so their writes are
  attributed and rate-limited like anyone else's. On a `link` fragment
  the link holder counts as a viewer.
- Origins: each fragment is served from `<name>.fragment.club`; the
  platform (login, share sheet, invites, the share header) from
  `fragment.club`. celld does not vouch for `Host`, so the router checks
  the hostname against the configured suffix before it trusts it.

## Agents

- An agent is a Durable Object with a key, memory in SQL (with the
  `sqlite_vec` flag for recall), and memberships in fragments.
- A turn is a Workflow instance: model calls (OpenRouter through the
  host) and tool calls (operations keyed `<turn>:tool-<n>`) are steps, so
  a crashed turn replays from recorded results instead of repeating
  effects (spike 3: SIGKILL mid model call and mid tool effect, no
  finished call repeated, the in-flight effect applied once). Each model
  step records the request's hash; a replay that asks a different
  question fails the turn.
- A chat is a fragment with a `chat` channel. The agent member
  subscribes; a member's message triggers a turn. Guests act with the
  owner's full authority (ROADMAP decision 3).

## Computers

- A computer is a Sprite with an owner principal (a person, an agent, or
  a fragment). One `Computer` Durable Object per Sprite holds ownership,
  lifecycle, and idle policy (an alarm), and speaks to the Sprites API
  with the Fly token as a host secret; nothing else holds that token.
- A fragment that owns a computer calls it through a capability in its
  app `env`; output streams into a channel (the Blender example).
- Builder workspaces are computers owned by the fragment they build,
  running the `fragment` CLI and fx with an editor key delegated for that
  one fragment.

## Limits (initial; each enforced and tested)

| Limit | Value | Why |
|---|---|---|
| operation input | 256 KiB | a request, not an upload (bytes go to R2) |
| operation or step result | 1 MiB | the Workflows step-result limit |
| channel record body | 64 KiB | records are messages, not files |
| channel page | 1000 records | bounded reads |
| app facet database | 16 MiB | celld copies the whole facet image into the root after each changed turn: a mutation took ~9 ms at 1 MiB, ~18 ms at 16 MiB, ~55–61 ms at 64 MiB, where root snapshots carrying the image added ~2 MB of replication per mutation (spike 2) |
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

## Spikes (done 2026-09-23; `spikes/README.md`)

1. **Rust cells** — adopted: workers-rs 0.8.5 covers SQL, alarms, and
   hibernatable WebSockets; the Worker Loader and facets are reached
   through `js_sys`; a ~35-line JavaScript shim adds `extends
   DurableObject` (celld refuses RPC otherwise) and the capability
   `WorkerEntrypoint` classes. ~9 ms once per isolate, 0.05–0.2 ms per
   request.
2. **Facet as author SQL** — adopted with the synchronous-mutation rule
   above; the root transaction is out.
3. **Deterministic agent turns** — adopted: a libfx turn is a Workflow.
4. **celld v0.5.1** — adopted; one alarm regression is in the debt
   ledger.

## Answered (2026-09-23)

- **Authoring shape:** one `app.mjs` whose exported `App` class (a
  Durable Object class the supervisor starts as the `app` facet) has one
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

## Open

- **Where large bytes go.** Paul (2026-09-23): "large files should be
  landing in git storage." This row of the anatomy table still says R2
  for uploads and generated media; which one holds them is to be settled
  before phase 2 builds the bytes path.
