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
2. The core model is `docs/MODEL.md`: files in git (large ones as
   pointers to blobs in Tigris), state in SQL, history in channels; one execution primitive (the typed, durable
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
   workspaces); Agent Substrate and GKE are dropped. A `Computer` cell per
   Sprite (ownership, quotas, sleep policy) holds the owner's Sprites org
   token; no computer does.
6. **Hosting:** celld on always-on Fly Machines (two, `ord`) over a
   Tigris single-region bucket in `ord` (conditional writes, strongly
   consistent); one shared fleet (cross-user sharing needs cells to call
   each other; crons need an always-on fleet). Domain: `fragment.club`,
   per-fragment subdomains for origin isolation.
7. **Inference is OpenRouter for everything**: text `z-ai/glm-5.3-flash`
   (reads images and video, so screenshots work), images
   `google/gemini-3.1-flash-lite-image`, video `minimax/hailuo-3-max`
   (H3 Max). fal is removed. Each person uses their own model credential
   ("Connect OpenRouter"), not one shared key (decision 11).
8. **Rust wherever it runs.** Tooling, e2e, fakes, the computer binary,
   and deploy are Rust; the platform cells are Rust (workers-rs on celld;
   spike 1 held). Author code stays JavaScript. No shell or Python. The
   Cloudflare host path is deleted (celld only: v0.5.1 plus our fork's
   alarm fix, `futurepaul/celld@b5f57ea`, until upstream ships it).
9. fragment.club's published fragments are not migrated; every primitive
   they used stays expressible and proven (`docs/published-fragments.md`).
10. **One agent loop: goose** (`goose-agent`, pinned to the fork
    `futurepaul/goose` at `12922e7`) replaces libfx in cells and fx over
    ACP on computers. The loop runs where the agent's hands are (its
    computer, else its cell); the conversation lives in the agent's cell.
    finite-next's fx path (`acp-job.mjs`, `run-job.sh`, the inference
    shim, ACP event mapping, the cancel file) is not ported.
11. **Secrets have one home, and code holds capabilities, not keys**
    (`docs/secrets.md`): encrypted in the owning cell; computers reach
    credentials through Sprites connectors; each person connects their own
    OpenRouter account (OAuth PKCE), with Stripe Projects as a later
    origin if it gains a platform API.
12. **Sign-in for fragment.club is WorkOS** (phase 4): WorkOS
    authenticates people; the platform holds one key per person behind
    it; the CLI and agents keep signing with NIP-98.
13. **Bring your own compute is a later phase (10)**, per org: an org's
    own celld fleet, Tigris bucket, Sprites org, and OpenRouter account
    under `<org>.fragment.club`. The shared fragment.club is deployed the
    same way, as our own org. Four things stay configuration from now on,
    so this needs no migration: the hostname suffix, the fleet's bucket,
    the Sprites org and token per owner, and the fleet `xtask deploy`
    targets.

## Truth map (every change is checked against this)

| Thing | Source of truth | Derived/copies must be |
|---|---|---|
| File bytes + history + live pointer | code.storage git | local folder = disposable working copy; cell RAM = LRU cache, never persisted |
| Tree index (path, size, sha per pinned commit) | derived from git | cell SQLite; names its pinned SHA; invalidated by webhook/poll; serves pinned until refreshed |
| Manifest and declared operations | `fragment.json` in git | cell caches the pinned copy; an invalid manifest at a new pin keeps the last good cache and records an event |
| Members, roles, invites | the fragment's supervisor cell | grants and revokes are transactional; the `events` channel records each change |
| Cell state (supervisor tables, operation ledger, channels, the app facet's SQL) | the S3 bucket (Tigris), via celld replication | — |
| Large file bytes (1 MiB or more: uploads, generated media) | blobs in Tigris (celld's R2 binding, the fleet bucket under `r2/`), keyed by SHA-256 | git holds a pointer; a sync resolves it to the real file; blobs no branch tip references are deleted; never in cell SQL |
| Identities ↔ keys, designated owners | BANKS registry cell | sessions and caches name it and never outlive a revocation |
| Browser sessions | platform session cookie (platform origin only) | maps to one identity key; re-checked against grants per request |
| Agent conversations and turns | the agent's cell (goose's conversation in SQL) | a computer holds a working copy and a tool journal; effects dedupe at their owners by tool-call id |
| Computer disks | Sprites durable storage | the front door's registry records ownership only |
| Secrets (personal, fragment, host, OpenRouter, code.storage, Sprites) | the owning cell, encrypted; fleet secrets in the deployed config (`docs/secrets.md`) | never in a repo, a bucket in plaintext, a log, a command line, or a computer's disk; computers use Sprites connectors |
| Compute/audit trail | events ledger | webhook deliveries recorded as events, deduped by delivery key |

Hard rule kept: **no file bytes persist in cell SQLite.** File bytes live
in git, or, at 1 MiB and above, in the bucket as content-addressed blobs
that a pointer in git names.

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
- Done 2026-09-23, all four adopted (`spikes/README.md`): Rust platform
  cells (workers-rs plus a ~35-line JS shim); the app facet with
  synchronous mutations and a facet-local ledger (the root transaction
  cannot enclose a facet past ~1.6 MB); agent turns as Workflows (SIGKILL
  mid-turn repeats no finished call); celld v0.5.1 (311/312, the red check
  is a celld alarm regression in the ledger). `docs/MODEL.md` is updated.
- Paul (2026-09-23): synchronous mutations yes; facet database capped at
  16 MiB; every file in git, 1 MiB and up as pointers to Tigris blobs,
  latest version only; fix the celld alarm regression in a fork
  (`celld-worktrees/alarm-fix`) and upstream it.
- **Acceptance:** each spike ends with a written verdict and numbers
  (bundle size, cold activation, facet image cost at 1/16/64 MiB, replay
  without repeated effects); `docs/MODEL.md` updated to match; Paul's
  open questions answered.

### 2. The core cut
- Started 2026-09-23 on Paul's go; slices, checkpoints, and the
  primitive mapping live in `docs/phase-2.md`.
- Operations, channels, membership, the app facet, blob pointers, and
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
  cutover. `cargo xtask deploy <fleet>` reads that fleet's configuration
  (hostname suffix, bucket, Sprites org) rather than constants, so
  phase 10 adds fleets without a migration.
- **Acceptance:** the e2e suite passes against the hosted URL; durable
  write latency (fleet proof), cold cell load, and cell-to-Sprite
  latency recorded; an operator runbook.

### 4. Identity and browser sessions
- BANKS registry cell: identities (person, agent, fragment) ↔ npub and
  designated owners. Sign-in is WorkOS (decision 12; a dev login stands in
  locally), creating a person with a custodial key encrypted in their
  cell. A platform session maps a browser to that principal; each
  fragment is served from its own origin (`<name>.fragment.club`, the
  suffix from configuration), with the hostname checked before it is
  trusted. "Connect OpenRouter" (OAuth PKCE) stores the person's model
  credential (`docs/secrets.md`).
- **Acceptance:** identical allow/deny decisions for CLI and browser
  across public, link, and members; negative tests (no session, revoked
  member, cross-fragment cookie) and a session restart test.

### 5. Agents as members
- The agent cell: goose's loop as a workers-rs Durable Object in its own
  celld project (`agent/`), ported from `spike/goose-agent`'s `cell/`
  (store, effects, durable steer queue, watchdog). Its tools are the
  operations of the fragments it belongs to, keyed by tool-call id; the
  turn's visible messages stream into the chat's channel. Model calls use
  the owner's credential.
- **Acceptance:** the goose spike's checks, ported to the Rust e2e (steer
  mid-tool, stop, SIGKILL mid-tool runs the effect once, SIGKILL between
  steps); an agent uses the todo fragment only through its operations.

### 6. The desktop
- The person's home fragment, laid out as the Desktop UI session designed
  it (`finite-next-worktrees/desktop-ui`, branch `claude/desktop-ui`):
  a collapsible sidebar of chats and apps, the chat in the middle, and a
  collapsible viewer on the right that stacks app and file panes, newest
  on top, reordered by dragging a header. split-grid (vendored, ~3 KB),
  Finite's look (not a code-tool look); files are `{fragment, path}` read
  through the fragment file API. Forkable and resettable.
- **Acceptance:** a browser e2e opens apps and files into the viewer,
  reorders and closes panes, collapses both sides, and works at phone
  width; the layout survives a reload.

### 7. Chats and sharing
- A chat template (a `chat` channel) whose agent member answers new
  messages. Platform shell: share sheet, invites, direct URLs with the
  share header, shared badges in the desktop's sidebar.
- **Acceptance:** two users, one chat: invite, accept, both see messages
  stream live and labeled by sender; the guest drives the owner's agent;
  revoking closes the guest's socket and returns 403; a rewritten
  desktop cannot share without the sheet click.

### 8. Computers and builder workspaces on Sprites
- A `Computer` cell per Sprite (ownership by principal, fragments
  included; lifecycle and idle policy on its alarm; the owner's Sprites
  org and token, ours by default). The computer runs `fragment computer
  serve` (goose's loop and tools, a journal keyed by tool-call id, file
  sync through git), ported from `spike/goose-agent`'s `computer/`, and
  holds a Sprites Task while a turn runs. Credentials arrive through
  Sprites connectors (`docs/secrets.md`). The personal computer adds
  Sprite services (Chrome, display, control).
- **Acceptance:** a builder takes a task to `deploy --preview` and then
  live; Stop cancels the running command and leaves nothing published;
  a platform kill mid-tool runs the tool once; a computer restart
  resumes the turn from the cell's conversation; no credential is on the
  Sprite's disk.

### 9. Cutover
- `fragment.club` DNS to Fly (owner action), the VPS retired, and the
  example fragments re-created from templates.

### 10. Bring your own compute (after everything above)
- An org signs up and gets its own deployment under `<org>.fragment.club`:
  its own celld fleet on its Fly org, Tigris bucket, Sprites org, and
  OpenRouter account. A small control plane holds the directory (org to
  fleet) and provisions with the org's credentials. Neither Fly nor
  Sprites offers third-party OAuth, so "connect" is `fragment connect fly`
  (a scoped, expiring org token minted from the person's own flyctl
  login) plus a Sprites org token. DNS and certificates: a record per org
  pointing at its app, Fly certificates on that app, a wildcard for
  per-fragment origins through a DNS challenge we answer. Releases ship
  into every fleet (celld upgrades can need a whole-fleet stop). WorkOS
  organizations map to our orgs, including an org's own SSO. Open: a
  shared code.storage org with a repo per org, or the org's own.
- A dedicated fleet also retires the shared-process isolation debt for
  that org (`docs/technical-debt-ledger.md`).

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
