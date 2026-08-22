# fragment

A fragment is a folder of files, a SQLite database, some URLs, and an inbox —
wrapped around exactly one problem. This repo is the second implementation of
the idea (the first was a throwaway Cloudflare prototype, never published).
This one is built on [celld](https://celld.dev): self-hosted Durable Objects
that keep their state in a bucket you own.

There is no agent inside a fragment. A fragment is a *place*: agents and
people use the `fragment` CLI to put files, code, workflows, and permissions
into it. What makes it different from a static site host is that it is
stateful, multiplayer, and can wake itself up.

## The two pieces

**`fragment` — the CLI (Rust, `cli/`).** The whole control surface. An agent
with this CLI and a nostr key can do everything: make a fragment, sync a local
folder into it, publish drafts, bless one, set secrets, grant access, trigger
workflows, read the event log. `fragment guide` prints the agent-facing skill
doc. There is no other API to learn.

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
                            │                        │  /f/<name>/…  → blessed draft of fragment <name>
                            │                        │  /d/<slug>/…  → one draft snapshot
                            │                        │  /api/…       → control (NIP-98 signed)
                            │                        ▼
                            │                 FragmentCell (one per fragment)
                            │                        │  files / drafts / workflows / secrets / inbox / events
                            ▼
                     bucket (azurite locally; S3/R2/GCS in prod)
                     = durability, ownership, coordination. RPO=0.
```

## What a fragment is made of

A fragment folder, as the CLI sees it locally:

```
my-fragment/
  fragment.json      # manifest (see below)
  site/              # static files → the fragment's URLs
  app.mjs            # optional: dynamic request handler for the site
  rooms.mjs          # optional: server-side reactions to realtime messages
  workflows/         # *.mjs durable-ish workflows, cron or inbox triggered
  …anything else     # just files (data, notes, sqlite exports, whatever)
```

`fragment.json`:

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
  `token` (anyone with the `?view=` link token). Viewing never grants
  editing. Editing (sync/publish/bless/secrets) requires the owner or an
  editor npub, proven per request with a NIP-98 signed event.
- **workflows**: `cron` (5-field, UTC, via the cell's own durable alarm —
  survives sleep) or `trigger: "inbox"` (a POST to the fragment's inbox runs
  it). `fragment run <name> <workflow>` triggers manually.
- **secrets**: declared by name only. Values are set with
  `fragment secret set` and injected into workflow runs. They never appear in
  files, drafts, or logs.

The manifest is decoded exactly once at the door: PUT validates it against
the TypeBox schema in `runtime/ts/manifest.ts`, applies defaults, and stores
the normalized form; the cell serves the parsed object from memory. Nothing
downstream re-parses or re-validates it.

## Drafts and blessing

`fragment publish` snapshots the fragment's current files + code and returns a
random draft URL (`/d/x7k2q9/`). Drafts are immutable and unguessable — safe
to share for review, safe to publish constantly. Nothing changes the live site
until `fragment bless <name> <slug>` points the canonical URL (`/f/<name>/`)
at a draft. Rollback = bless an older draft.

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

Workflow code runs in a **separate isolate per run** (celld's Worker Loader),
not inside the cell isolate — so a wedged workflow can't wedge the fragment.
`ctx`: `http` (fetch), `files` (read/write/list the folder), `secrets`,
`inbox` (pending messages), `events` (append to the ledger), `ai`
(platform-routed inference, host holds the key), `state` (per-workflow kv),
`log`. Every run appends to the fragment's **event log** (start, finish,
error, output digest) — the ledger is ground truth, `fragment events` reads it.

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
   recorded no-op (`{deduped: true}`). Copy-loops die on pass two; unchanged
   rewrites don't churn `updatedAt`, so revcron feeds don't re-see old items.
2. *Hop budget* — runs carry a cause chain `{origin, depth}`; `ctx.http`
   stamps `x-fragment-hops: depth+1` on every outbound request; an inbox
   trigger above 16 hops refuses to fire and records `cycle.detected`.
   Workflows that intend recursion set `cycles: true`.
3. *Rate ceiling* — more than 120 auto-triggered runs in a rolling hour
   trips the auto-pause breaker. The only layer that catches a loop which
   genuinely mutates content every pass (the AI-agent ping-pong shape).

**Inbox, hardened at the door.** An `Idempotency-Key` header collapses
redeliveries (24h retention) before any author code runs. Pending inbox
messages cap at 1000; beyond that the POST gets a 429 and the ledger gets
`queue.rejected` — overload is a signal, not memory pressure.

**Delivery contract, one paragraph.** Any trigger may fire more than once
for one logical change. Files are safe by construction (suppression);
external side effects should be keyed by cause (`lib/once.mjs`). Sync
coalescing is debounce — `debounceMs` in the manifest names the knob.

`fragment runs <name> [--status held]` reads the table;
`fragment pause|unpause|replay` drive it.

## The event log

Everything that changes a fragment appends to `events`: syncs, publishes,
blesses, secret sets, grants, workflow runs, inbox arrivals. The log is the
answer to "what happened" and the runtime's own memory. (Learned the hard way
in the first prototype: never let a report disagree with the ledger.)

## Auth, plainly

- One keypair per CLI user: `fragment login` generates a nostr secret key in
  `~/.config/fragment/`.
- Every control request carries a NIP-98 event (kind 27235, url+method+payload
  tags), verified in the runtime with pure-JS secp256k1 schnorr.
- Each fragment also *has* an npub (generated at create, secret stays in the
  cell) so fragments can be addressed and can sign later.
- No email, no passwords. The `?view=` token exists for "send a link to a
  human" (visibility `token`).

## Isolation and ingress

- A fleet runs one deployment (the runtime). Fragments are cells: one SQLite
  each, one writer each, fenced by celld epochs. A broken fragment can only
  damage its own database.
- Workflow code runs in loader isolates with no access to other cells.
- The internal plane (`/__internal/f/<name>/…`) serves exactly one caller:
  ctx loopback from loader isolates, authenticated per cell by run tokens.
  The registry and cell-init are never reachable over HTTP — only via the
  router's own DO binding. Hosts that want a second lock set
  `FRAGMENT_HOST_SECRET` (celld: `CELLD_VAR_FRAGMENT_HOST_SECRET`; CF:
  wrangler secret), which every loopback call must then carry.
- celld does not terminate TLS and does not authenticate users. That is the
  ingress's job (Caddy in prod; nothing locally). Wildcard subdomain →
  `Host: <name>.frag.example` reaches the same router; path-based URLs work
  everywhere, so subdomains are sugar, not load-bearing.
- cellds don't talk to each other: there is one fleet; cells can't reach other
  cells except through the public URLs, where normal auth applies.

## Local dev

No docker, no cloud account:

```
scripts/dev up       # azurite (npm, in .dev/) + celld on :8789
scripts/dev deploy   # build + deploy runtime/ to the fleet
scripts/dev down
```

Azurite is the bucket (Azure emulator, documented celld dev path). State lives
in `.dev/` — delete it and you've reset the world. Production swaps azurite
for a real bucket and adds Caddy; nothing else changes.

## What this deliberately does not have (yet)

- No in-fragment planning agent. The mind is external; fragments are places.
- No local test-run story. Publish drafts instead — drafts are the rehearsal.
- No fragment-to-fragment private channels. They use public URLs + npub auth
  like everyone else.
- No CRDT file sync. Sync is last-writer-wins with conflict files, on purpose.

## Non-goals

KV-style global state, blob storage, multi-tenant hostile isolation (celld is
alpha; fleet = one trust domain), replacing the first prototype's hosts.
