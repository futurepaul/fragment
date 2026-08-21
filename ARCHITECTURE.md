# fragment

A fragment is a folder of files, a SQLite database, some URLs, and an inbox —
wrapped around exactly one problem. This repo is the second implementation of
the idea (the first, a Cloudflare prototype, lives at `../fragment`). This one
is built on [celld](https://celld.dev): self-hosted Durable Objects that keep
their state in a bucket you own.

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
