# ROADMAP — fragment-next: places for people and agents

Decided 2026-09-23. This supersedes fragment's 2026-09-07 roadmap (git
history keeps it); its truth map and wire contract carry forward below.
Engineering style: `/Users/futurepaul/dev/finite/engineering-style/
engineering-style.md` (the Finite contract), including hard cuts, the
debt ledger (`docs/technical-debt-ledger.md`), and assertions on in
release.

## The problem

People and their agents need shared, durable places that do things: a
todo list two people and an agent keep together, a chat where a guest
can drive your agent, a dashboard fed by webhooks, a multiplayer view
over a Blender session running on a computer somewhere. Each place must
be cheap while idle, safe to share, easy for an agent to build and fix,
and simple enough that the system does not collapse under its own
weight.

## The model: four nouns

1. **Identity.** A nostr key for a person, an agent, or a fragment,
   registered in BANKS (which identity a key belongs to, who owns it).
   Resource permissions belong to the service that owns the resource,
   never to BANKS.
2. **Fragment.** A place: one code.storage repo (files, history, `main`
   and `live`), one supervisor cell (members, operations, channels) with
   the app's own SQLite in a facet, URLs, and members (key → role). **Apps, chats, and desktops are all
   fragments; they differ only in their files.** Sharing anything is a
   grant. Opening any fragment URL shows it under a platform share
   header.
3. **Agent.** An identity with a brain (a libfx cell with durable turns)
   that joins fragments as a member and acts through the same signed API
   as the CLI.
4. **Computer.** A Fly Sprite owned by an identity. Fragments have keys,
   so a fragment can own a computer.

The verbs are the CLI's: create, sync, deploy (preview, rollback),
grant/revoke, call (operations), append/read (channels), and a
computer's wake/exec.

## Decisions (made 2026-09-23)

1. fragment is the base; finite-next's agent, desktop, and platform shell
   are rebuilt on it. finite-next's app cell, repo layer, and workspace
   sync/publish plumbing are hard-cut (provenance: finite-next
   `claude/cell-agent` @ `6545f08`).
2. The core model is `docs/MODEL.md`: code in git, state in SQL, bytes in
   R2, history in channels; one execution primitive (the typed, durable
   **operation**); one log (**channels**); membership as live cell state
   with every actor a key; agents and computers as participants.
   Author-facing SQL is a Durable Object Facet (the app's own SQLite).
   File-based apps (a vault) stay first-class.
3. **Chats are fragments.** The transcript is an append-only, paged room
   history (rooms today persist one document, which a transcript
   outgrows). A chat guest is a member with the owner's full authority
   ("you can do everything I can"); a less privileged guest mode may
   follow.
4. Sharing UI is **platform-owned**: the desktop's `...` menu asks the
   shell to open a trusted share sheet; invites are accepted in the
   platform bar; a direct fragment URL signs you in and shows the
   fragment with a Google-Docs-style share header. The desktop (user
   code the agent can rewrite) can never grant anything by itself.
5. **All computers are Sprites** (personal computers and builder
   workspaces); Agent Substrate and GKE are dropped. A Rust machine
   front door (nostr-key auth, ownership, quotas, sleep policy) holds the
   Fly token; nothing else does.
6. **Hosting:** celld on always-on Fly Machines (two, `ord`) over a
   Tigris single-region bucket in `ord` (conditional writes, strongly
   consistent); one shared fleet (cross-user sharing needs cells to call
   each other; crons need an always-on fleet). Domain: `fragment.club`,
   per-fragment subdomains for origin isolation.
7. **Inference is OpenRouter for everything**, and the host holds the
   key: text `z-ai/glm-5.3-flash` (reads images and video, so
   screenshots work), images `google/gemini-3.1-flash-lite-image`, video
   `minimax/hailuo-3-max` (H3 Max). fal is removed.
8. **Rust wherever it runs.** Tooling, e2e, mocks, the ACP runner, and
   deploy are Rust; platform cells become Rust (workers-rs on celld) if
   the spike in `docs/MODEL.md` holds, otherwise they stay TypeScript
   (ledgered). Author code stays JavaScript. No shell or Python. The
   Cloudflare host path is deleted (celld only, v0.5.1).
9. fragment.club's published fragments are not migrated; every primitive
   they used stays expressible and proven (`docs/published-fragments.md`).

## Truth map (every change is checked against this)

| Thing | Source of truth | Derived/copies must be |
|---|---|---|
| File bytes + history + live pointer | code.storage git | local folder = disposable working copy; cell RAM = LRU cache, never persisted |
| Tree index (path, size, sha per pinned commit) | derived from git | cell SQLite; names its pinned SHA; invalidated by webhook/poll; serves pinned until refreshed |
| Manifest and declared operations | `fragment.json` in git | cell caches the pinned copy; an invalid manifest at a new pin keeps the last good cache and records an event |
| Members, roles, invites | the fragment's supervisor cell | grants and revokes are transactional; the `events` channel records each change |
| Cell state (supervisor tables, operation ledger, channels, the app facet's SQL) | the S3 bucket (Tigris), via celld replication | — |
| Large bytes (uploads, generated media) | R2 (the fleet bucket under `r2/`) | referenced by content hash; never in git or cell SQL |
| Identities ↔ keys, designated owners | BANKS registry cell | sessions and caches name it and never outlive a revocation |
| Browser sessions | platform session cookie (platform origin only) | maps to one identity key; re-checked against grants per request |
| Agent turns and operation log | the agent's cell | effects dedupe at their owners by operation id |
| Computer disks | Sprites durable storage | the front door's registry records ownership only |
| Secrets (fragment, host, OpenRouter, code.storage, Fly) | cell (wrapped under `FRAGMENT_HOST_SECRET`) or the host secret store | never in a repo, a bucket in plaintext, a log, or a command line |
| Compute/audit trail | events ledger | webhook deliveries recorded as events, deduped by delivery key |

Hard rule kept: **no file bytes persist in cell SQLite or the bucket.**

## Wire contract (carried forward, unchanged)

- One code.storage repo per fragment. `main` = working files; `live` =
  the blessed serve point. Preview = ephemeral ref; promote = go live;
  rollback = re-point `live`.
- Runtime file plane: the cell pins `main@SHA` for working-copy reads,
  serves from `live@SHA`, and streams contents on demand (RAM LRU,
  64 MiB).
- Workflow writes commit server-side through the cell-held JWT under
  expected-parent CAS; the CLI commits directly with a short-lived,
  repo-scoped storage token (editor+, minted per request, audited).
- Fragment identity: the npub secret is generated client-side at create,
  crosses the wire once, and is stored wrapped.
- Webhooks: validate → persist (dedupe) → interpret; a 5-minute poll
  backstop.

## Phases

Each phase lands as hard cuts with its tests. A phase is done when its
acceptance checks pass in CI and, from phase 3 on, against the hosted
deployment.

### 0. Foundation (done 2026-09-23)
- fragment-next created from fragment's full history (`9a381f8`, pushed
  to `futurepaul/fragment` first); no remote of its own yet.
- Baseline: runtime 32/32, CLI 38/38, `clippy -D warnings` clean, e2e
  312/312 (`E2E_FAL_FAKE=1`) after one fix. Workflow instance ids were
  fleet-global (`r1a1`), so two fragments' first runs collided and one
  fragment's run could replace another's finished instance; ids are now
  namespaced by the fragment's key, with a regression test proven to
  fail on the old ids.
- Published-fragment inventory and its coverage gaps recorded.

### 1. Model spikes
- The four spikes in `docs/MODEL.md`: Rust platform cells (workers-rs,
  with Worker Loader and facets), the app facet as author SQL,
  deterministic agent turns in Workflows, and celld v0.5.1. Verdicts live
  in `spikes/<name>/README.md`.
- Done: celld v0.5.1 (adopted; 311/312, the one red check is a celld
  alarm regression recorded in the ledger).
- **Acceptance:** each spike ends with a written verdict and numbers
  (bundle size, cold activation, facet image cost at 1/16/64 MiB, replay
  without repeated effects); `docs/MODEL.md` updated to match; Paul's
  open questions answered.

### 2. The core cut
- Operations, channels, membership, the app facet, R2 bytes, and
  OpenRouter (text, image, video) replace workflows-as-files, rooms
  documents, inbox tables, `ctx.state`, git grants, and fal, in hard
  cuts. Every published-fragment primitive is re-expressed on the new
  model and proven.
- The tooling moves to Rust in the same phase, because the new harness
  is written against the new model: an `xtask` crate (`dev`, `build`,
  `e2e`, `deploy`), a Rust e2e that drives the real server, CLI, and a
  browser, and Rust fakes for code.storage, OpenRouter, and a push
  service. `scripts/`, `deploy/`, and the Cloudflare path are deleted;
  the JavaScript e2e is the safety net until the Rust one supersedes it.
- `fragment call`, `fragment channel`, `fragment members` in the CLI; a
  todo template and a vault template as the reference apps.
- **Acceptance:** valid, invalid, replay, and conflicting-body tests for
  every mutation; restart tests for the supervisor and facet storage;
  the published-fragment table fully green with no gap rows; no `.sh` or
  `.py` in the repo.

### 3. Hosted on Fly
- `celld diagnose` against a new Tigris `ord` bucket before any state
  lands there. celld v0.5.1 on two always-on Fly Machines in `ord` with a
  restart-always policy (celld self-fences and must be restarted),
  peers on Fly's private network; real code.storage (org `finite`);
  secrets set from files, never printed. A staging hostname until the
  cutover.
- **Acceptance:** the e2e suite passes against the hosted URL; durable
  write latency (fleet proof), cold cell load, and cell-to-Sprite
  latency recorded; an operator runbook.

### 4. Identity and browser sessions
- BANKS registry cell: identities (person, agent, fragment) ↔ npub and
  designated owners. Dev login (`dev:<handle>`, WorkOS later) creates a
  person with a custodial key wrapped under `FRAGMENT_HOST_SECRET`. A
  platform session maps a browser to that principal; each fragment is
  served from its own origin (`<name>.fragment.club`), with the hostname
  checked before it is trusted.
- **Acceptance:** identical allow/deny decisions for CLI and browser
  across public, link, and members; negative tests (no session, revoked
  member, cross-fragment cookie) and a session restart test.

### 5. Agents as members
- The agent cell (libfx WASM, memory in SQL with `sqlite_vec`) with its
  own key; a turn is a Workflow (or finite-next's lease-and-ledger turn
  if the determinism spike fails). Its tools are the operations of the
  fragments it belongs to. Inference goes through the host's OpenRouter
  route.
- **Acceptance:** finite-next's crash-mid-tool and replay checks,
  ported; an agent uses the todo fragment only through its operations.

### 6. Chats, desktops, sharing
- A chat template (a `chat` channel) whose agent member answers new
  messages. The desktop as the user's home fragment (from an
  upstream template; forkable and resettable). Platform shell: share
  sheet, invites, direct URLs with the share header, shared badges.
- **Acceptance:** two users, one chat: invite, accept, both see messages
  stream live and labeled by sender; the guest drives the owner's agent;
  revoking closes the guest's socket and returns 403; a rewritten
  desktop cannot share without the sheet click.

### 7. Computers and builder workspaces on Sprites
- A `Computer` cell per Sprite (ownership by principal, fragments
  included; lifecycle and idle policy on its alarm; the only holder of
  the Fly token). The personal computer as Sprite services
  (Chrome, display, control). Builder workspaces: the precompiled
  `fragment` CLI and fx over ACP (a Rust runner), with an editor key
  delegated for one fragment; the host proxies inference so no model
  key lands on a Sprite.
- **Acceptance:** a signed-in Chrome survives deep sleep; a builder
  takes a task to `deploy --preview` and then live; Stop cancels over
  ACP and leaves nothing published.

### 8. Cutover
- `fragment.club` DNS to Fly (owner action), the VPS retired, and the
  example fragments re-created from templates.

## Evaluation

- Every mutation: valid and invalid/replay tests. Idempotent mutations:
  success replay, rejected replay, conflicting body. Every storage
  invariant: a restart test (engineering style §6).
- E2E proves the product: the real server, the real CLI, and real
  browsers where the browser is the product surface. Lower-rung fakes
  are named as fakes (OpenRouter fake, code.storage mock, push-service
  fake).
- The published-fragment table is a regression checklist; a primitive
  without a check is a gap, not a pass.

## Escalations (ask Paul)

Product semantics (roles, what a chat guest can do), privacy posture,
irreversible actions (DNS cutover, deleting the VPS or Sprites, pushing
to a new remote), spend beyond the agreed budget, and any change to a
decision above.

## Open decisions

- A GitHub remote for fragment-next (none yet).
