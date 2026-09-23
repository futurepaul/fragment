# MODEL — the fragment-next core model

Status: proposed 2026-09-23, for review. This is the target shape the
runtime is hard-cut to (ROADMAP decisions 2-4 and the five changes Paul
approved the same day). Every mechanism names the celld primitive it
uses; read with the celld docs (https://celld.dev/docs/, v0.5.1).

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
| Supervisor | a Durable Object (`Fragment`), platform code | members, invites, the operation ledger, channels, schedules, wrapped secrets, the file-plane pins and tree index, the live code pin |
| App | a **Durable Object Facet** named `app`, started from the author's `App` class loaded with the **Worker Loader** at `live@SHA` | the author's SQLite database (`this.ctx.storage.sql`), which the supervisor's tables never share |
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
upload, commits or rolls back with the supervisor's transaction, and
cannot see platform tables. finite-next filtered SQL strings with a
regex instead; that is deleted.

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

- **query** — read-only; not ledgered; may be re-run on change signals to
  drive live views.
- **mutation** — one facet turn, transactional; ledgered by operation id.
- **job** — runs as a Workflow instance whose id is the namespaced
  operation id; each `step.do` calls back into the supervisor, so
  retries, backoff, `waitForEvent` (approvals, invites), and sleeps are
  celld's. fragment's failure leg (held runs, replay, auto-pause, hop
  budget, loop suppression) moves onto this.

The call path, for every trigger:

```
trigger ─▶ principal + op id + input ─▶ Fragment (supervisor)
   role check ─▶ schema check ─▶ ledger lookup
     ├─ same id, same input, done   → return the stored result
     ├─ same id, different input    → reject (conflicting body)
     └─ new                         → App facet method (or Workflow for jobs)
   ─▶ ledger result ─▶ append to channels ─▶ change signal to subscribers
```

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
  `members`.
- Origins: each fragment is served from `<name>.fragment.club`; the
  platform (login, share sheet, invites, the share header) from
  `fragment.club`. celld does not vouch for `Host`, so the router checks
  the hostname against the configured suffix before it trusts it.

## Agents

- An agent is a Durable Object with a key, memory in SQL (with the
  `sqlite_vec` flag for recall), and memberships in fragments.
- A turn is a Workflow instance: model calls (OpenRouter through the
  host) and tool calls (operations) are steps, so a crashed turn replays
  from recorded results instead of repeating effects. This depends on
  driving libfx deterministically from step results (spike below);
  finite-next's lease, watchdog, and operation log are the fallback.
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
| app facet database | 64 MiB | celld copies the whole facet image into the root after each changed turn |
| `cpuMs` per invocation | 30 000 | a runaway loop cannot hold the fragment |
| `subRequests` per invocation | 50 | bounds fan-out |
| inbox pending | 1000 | overload is a 429, not memory pressure |
| hop depth | 16 | carried from fragment's loop guard |

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

## Spikes before the cut

1. **Rust cells.** celld runs workers-rs Durable Objects (WASM). Build the
   supervisor skeleton in Rust: SQL, hibernatable WebSockets, alarms, and
   the Worker Loader plus facets (through a thin JavaScript shim if
   workers-rs lacks them). Measure bundle size and cold activation. If it
   holds, the platform cells are Rust and the TypeScript-runtime debt
   entry is deleted; author code stays JavaScript.
2. **Facet as author SQL.** An `App` class with a schema, a mutation, and a
   query; replay and conflicting-body tests through the supervisor;
   facet image size vs turn latency at 1, 16, and 64 MiB.
3. **Deterministic agent turns.** Drive a libfx turn inside a Workflow with
   model and tool calls as steps; kill the node mid-turn and prove the
   replay repeats no effect.
4. **celld v0.5.1.** Move dev and CI from v0.4.0 to v0.5.1 (a wake-format
   upgrade; dev state resets with `celld dev --clean`).

## Open questions for Paul

- Authoring shape: one `app.mjs` with an `App` class whose methods are the
  operations (proposed), or one file per operation?
- Anonymous callers on `public` fragments: may they run `viewer`
  operations?
- Default channel retention (proposed: `events` 90 days, `inbox` until
  acknowledged, app channels forever).
