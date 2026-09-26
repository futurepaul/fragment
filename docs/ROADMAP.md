# ROADMAP — fragment: stateful web apps, multiplayer built in

Decided 2026-09-23. This supersedes fragment's 2026-09-07 roadmap (git
history keeps it); its truth map and wire contract carry forward below.
Reshaped 2026-09-26: the fragment is the product (decisions 19–22).
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

## The model: one fragment, two add-ons

1. **Fragment.** The product: a stateful web app with multiplayer built
   in, published from the CLI. One code.storage repo (files, history,
   `main` and `live`), one supervisor cell (members, operations, jobs,
   crons, channels) with the app's own SQLite in a facet, its own origin,
   and members (identity → role). Hosting, permissions, sharing, crons,
   jobs, and channels are built in; sharing anything is a grant.
   fragment.club lists your fragments; fragment.boats serves them.
2. **Two add-ons a fragment declares** in `fragment.json`. A fragment
   that declares neither carries nothing of them.
   - **An agent** (decision 20): goose's loop in its own cell, answering
     one postable channel or a job step, calling the operations it may.
   - **A computer** (decision 21): an identity with its own key, and
     later a Sprite running the `fragment` CLI, which talks back to its
     fragment and can build and publish fancier ones.
3. **Identities underneath.** People, agents, computers, and fragments
   are identities with one or more nostr keys, in a registry shaped like
   finite.computer's BANKS (which identity a key belongs to, who owns an
   agent or a computer). Grants name identities, not keys. Resource
   permissions belong to the service that owns the resource, never to
   the registry (`docs/finite-integration.md`, decision 15).
4. **One showcase: the desktop**, a template (chats, apps, and files on
   one page) that shows off the add-ons. It is not the core.

The verbs are the CLI's: login (pairing), create, sync, deploy (preview,
rollback), members and invites, call (operations), and read and follow
(channels).

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
   outgrows). A chat guest acts with decision 17's authority, which
   replaced "a member with the owner's full authority" on 2026-09-25.
   *Amended 2026-09-26 by decision 19* (a chat declares an agent).
4. Sharing UI is **platform-owned**: the desktop's `...` menu asks the
   shell to open a trusted share sheet; invites are accepted in the
   platform bar; a direct fragment URL signs you in and shows the
   fragment with a Google-Docs-style share header. The desktop (user
   code the agent can rewrite) can never grant anything by itself.
   *Amended 2026-09-25 (Paul):* a direct URL signs you in silently only
   on your own fragments and those shared with you; any other fragment
   asks once, on the platform ("Continue to X as you?"), before it learns
   who you are, and signing out of it there makes it ask again
   (docs/fragment-boats.md, answer 1).
   *Amended 2026-09-26 by decision 19* (the desktop is one way in).
5. **All computers are Sprites** (personal computers and builder
   workspaces); Agent Substrate and GKE are dropped. A `Computer` cell per
   Sprite (ownership, quotas, sleep policy) holds the owner's Sprites org
   token; no computer does.
6. **Hosting:** celld on always-on Fly Machines (two, `ord`) over a
   Tigris single-region bucket in `ord` (conditional writes, strongly
   consistent); one shared fleet (cross-user sharing needs cells to call
   each other; crons need an always-on fleet). Domain: `fragment.club`,
   per-fragment subdomains for origin isolation. The new fleet takes
   `fragment.club` itself in phase 3, with no staging name: only Paul
   publishes there (Paul, 2026-09-23).
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
    credentials through Sprites connectors. *Amended 2026-09-23 by
    decision 14:* the platform pays for AI up to each person's budget;
    connecting your own OpenRouter account (OAuth PKCE) comes back later
    as the way past the limit. *Amended 2026-09-26 by decision 21:* a
    computer holds its own revocable key and no other credential.
12. **Sign-in for fragment.club is WorkOS** (phase 4): WorkOS
    authenticates people; the CLI and agents keep signing with NIP-98.
    *Amended 2026-09-24 (Paul):* fragment has its own WorkOS environment
    with sign-up switched off (Paul invites people), not finite.computer's;
    a person is keyed by their verified `(issuer, subject)`, so
    finite.computer's login can join later as a second issuer. The
    platform holds no key for a person: browsers use a session.
13. **Bring your own compute is a later phase (10)**, per org: an org's
    own celld fleet, Tigris bucket, Sprites org, and OpenRouter account
    under `<org>.fragment.club`. The shared fragment.club is deployed the
    same way, as our own org. Four things stay configuration from now on,
    so this needs no migration: the hostname suffix, the fleet's bucket,
    the Sprites org and token per owner, and the fleet `xtask deploy`
    targets.
14. **Every person has a monthly budget, with a hard stop** (Paul,
    2026-09-23): $20 a month by default, reset on the 1st (UTC), kept in
    the person's cell. People spin up AI calls, image and video
    generations, and (phase 8) computers freely within it and always see
    what they have spent: remaining budget in the platform bar and in
    `fragment budget`, a cost on every run, a warning at 80%. A
    fragment's owner pays for everything the fragment spends, whoever
    triggered it. A paid step reserves its worst case before it runs and
    settles to the reported cost after; a step whose reservation does
    not fit fails ("budget used up", the run held, replayable after a
    top-up), so spend never passes the budget. Running out stops only
    paid steps, never sites or mutations. As a backstop, each person's
    AI runs on their own OpenRouter key, minted by the platform's with a
    credit limit equal to their budget. A fragment that sets its own
    `OPENROUTER_API_KEY` pays for itself and is not metered. Paul's
    exposure is at most $20 a month per invited person, plus hosting.
15. **Identity follows finite.computer's BANKS model (FIN-11)** (Paul,
    2026-09-24), so that fragment's people can move to finite.computer
    without rewriting a grant. Stable identities for people, agents, and
    fragments; grants name identities; each identity has one or more
    public keys, added with proof of possession and revoked one by one;
    every agent has a designated human owner, who can read what the agent
    can read; keys stay with their callers (browsers use sessions, the
    CLI and agents sign); live lookups that fail visibly. Everything
    finite.computer's Core will own (sign-in, the registry, billing,
    model access, connections, chat channels) is a stand-in built in
    V3's shape behind a seam, tracked in `docs/finite-integration.md`.
    Visibility keeps fragment's own link: `link` means anyone holding the
    unguessable link is a viewer; `public` means anyone, no link needed.

### 17. An agent acts for whoever asked, capped (Paul, 2026-09-25)

Each call in an agent's turn acts with the lower of the role of the
person who started the turn and a cap: the agent's own membership, or
`editor` on fragments its owner belongs to. Owner-only actions never go
through an agent. Replaces decision 3's "a chat guest has the owner's
full authority", which let a guest reach everything the owner has.
`docs/phase-7.md`.

### 18. Channels a fragment declares postable (Paul, 2026-09-25)

A declared channel may name a `post` role; the platform appends a
member's record itself (role check, rate limit, size cap, deduped by
principal and id). Chats use it, so a chat needs no app code and no
worker. The one exception to "clients never append" (docs/MODEL.md).

### 16. Usernames, and fragments under them (Paul, 2026-09-24)

A person chooses a username once (with a profile picture). A fragment's
name is `<label>.<username>`, served at `<label>--<username>.<suffix>`:
one DNS label, so the suffix's one wildcard certificate covers every
fragment (Paul, the same day, after per-host certificates met Let's
Encrypt's ~50 new names a week with every chat a fragment; ChatGPT's
`site.user.chatgpt.site` needs Cloudflare issuing a certificate per
site). Sessions are `__Host-` cookies, since every fragment shares the
platform's domain. The fragments made before this were not migrated (a
hard cut). *Amended by decision 23:* fragments share `fragment.boats`
with each other, not the platform's domain; sessions stay `__Host-`
cookies for that.

### 19. Fragment is the product (Paul, 2026-09-26)

Fragment is easy stateful web-app publishing with multiplayer built in,
the successor to finite sites: sign up, pair the CLI, publish (the
model, above). Agents and computers are add-ons a fragment declares; a
fragment that declares neither carries nothing of them. The desktop is a
showcase template kept at its current quality: fix breakage, no new
features. Amends decisions 3 and 4 where they make chats and the desktop
central.

### 20. The agent add-on (Paul, 2026-09-26)

A fragment declares an agent in `fragment.json`: its instructions, the
operations it may call, the postable channel it answers (decision 18),
and its model. A turn starts from a message on that channel, or from a
job step, `job.agent`. Only signed-in visitors can start one; an
anonymous visitor's message starts nothing. The owner pays, as for
everything a fragment spends (decision 14); there are no per-visitor
budgets yet. Think of a calorie tracker you tell "2 eggs and toast", or
an image iterated on with a little agentic help.

### 21. Computers are identities (Paul, 2026-09-26)

A computer has its own npub and is owned by a person. It works on
whatever its owner delegated or authorized it to do, including fragments
it publishes. It holds its own revocable key and no other credential
(amends decision 11). A fragment can declare a computer, and the
platform provisions a Sprite for it that runs the `fragment` CLI to talk
back to its parent fragment. That is a later step, which needs Paul's
call on the Sprites token.

### 22. Sign-up stays invite-only (Paul, 2026-09-26)

For now; decision 12 is unchanged.

### 23. Fragments on fragment.boats, the platform on fragment.club (Paul, 2026-09-25)

Fragments are served at `<label>--<username>.fragment.boats`; the
platform (sign-in, the share sheet, `/join`, `/cli`, `/api`) stays on
`fragment.club`, cross-site from every fragment, so its session cookie
reaches a fragment's page only on a top-level visit. The move is a hard
cut: a fragment's old host redirects to its new one (a write there is
410), `fragment.boats` itself to the platform, and fragment.club's
records and certificates stay at least a year. The fragments are one
site with each other until the Public Suffix List lists
`fragment.boats`, which waits for thousands of people
(docs/fragment-boats.md; the fleet: docs/operate.md).

## Truth map (every change is checked against this)

| Thing | Source of truth | Derived/copies must be |
|---|---|---|
| File bytes + history + live pointer | code.storage git | local folder = disposable working copy; cell RAM = LRU cache, never persisted |
| Tree index (path, size, sha per pinned commit) | derived from git | cell SQLite; names its pinned SHA; invalidated by webhook/poll; serves pinned until refreshed |
| Manifest and declared operations | `fragment.json` in git | cell caches the pinned copy; an invalid manifest at a new pin keeps the last good cache and records an event |
| Members, roles, invites | the fragment's supervisor cell | grants and revokes are transactional; the `events` channel records each change |
| Cell state (supervisor tables, operation ledger, channels, the app facet's SQL) | the S3 bucket (Tigris), via celld replication | — |
| Large file bytes (1 MiB or more: uploads, generated media) | blobs in Tigris (celld's R2 binding, the fleet bucket under `r2/`), keyed by SHA-256 | git holds a pointer; a sync resolves it to the real file; blobs no branch tip references are deleted; never in cell SQL |
| Identities ↔ keys, designated owners | the registry cell (BANKS's shape; BANKS itself later, `docs/finite-integration.md`) | sessions and caches name an identity and never outlive a revocation |
| Browser sessions | platform session cookie (platform origin only) | maps to one identity key; re-checked against grants per request |
| Agent conversations and turns | the agent's cell (goose's conversation in SQL) | a computer holds a working copy and a tool journal; effects dedupe at their owners by tool-call id |
| Computer disks | Sprites durable storage | the front door's registry records ownership only |
| Secrets (personal, fragment, host, OpenRouter, code.storage, Sprites) | the owning cell, encrypted by the node's `KEYS` for that cell alone; fleet secrets in the node's environment, used only by `KEYS` (`docs/secrets.md`, `docs/hardening.md`) | never in a repo, a bucket in plaintext, a log, a command line, or a computer's disk; computers use Sprites connectors |
| Compute/audit trail | the `events` channel | webhook deliveries recorded as events, deduped by delivery key |

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
- Webhooks: validate → persist (dedupe) → interpret; a poll backstop,
  every 5 minutes while something outside the platform may be writing
  (a storage token or a webhook in the last day), else daily.

## Phases

Each phase lands as hard cuts with its tests. A phase is done when its
acceptance checks pass in CI and, from phase 3 on, against the hosted
deployment.

**Where it stands (2026-09-26).** fragment.club runs master `9a5c518`
(hosted e2e 29/29): phases 3 to 7 (7 without the share header), the
hardening pass's H1–H3, fragment.boats slice 1 (isolation, frames
through `__frame`, asking first), and phase 8's first part. The plan
from here (Paul, 2026-09-26) is phases A to F in order, then 9 and 10;
phase 7's chat half and phase 8 fold into D to F and stay as records.

### 0. Foundation (done 2026-09-23)
- fragment-next created from fragment's full history (`9a381f8`, pushed
  to `futurepaul/fragment` first).
- Baseline: runtime 32/32, CLI 38/38, `clippy -D warnings` clean, e2e
  312/312 (`E2E_FAL_FAKE=1`) after one fix. Workflow instance ids were
  fleet-global (`r1a1`), so two fragments' first runs collided and one
  fragment's run could replace another's finished instance; ids are now
  namespaced by the fragment's key, with a regression test proven to
  fail on the old ids.
- Published-fragment inventory and its coverage gaps recorded.

### 1. Model spikes (built 2026-09-23)
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

### 2. The core cut (done 2026-09-23)
- Done in seven slices on Paul's go; the record (what landed, every
  decision, what was deferred and to whom) is `docs/phase-2.md`. The
  Rust e2e passes 444 of 444; the TypeScript runtime, `scripts/`,
  `deploy/`, `notify-relay/`, and the Node packages are deleted.
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
  the JavaScript e2e was the safety net until the Rust one superseded
  it (slice G).
- `fragment call`, `fragment channel`, `fragment members` in the CLI;
  the `todo`, `inbox`, and `notes` templates as the reference apps.
- **Acceptance:** valid, invalid, replay, and conflicting-body tests for
  every mutation; restart tests for the supervisor and facet storage;
  the published-fragment table fully green with no gap rows; no `.sh` or
  `.py` in the repo.

### 3. Hosted on Fly (built 2026-09-23)
- `celld diagnose` against a new Tigris `ord` bucket before any state
  lands there. celld v0.5.1 on two always-on Fly Machines in `ord` with a
  restart-always policy (celld self-fences and must be restarted),
  peers on Fly's private network; real code.storage (org `finite`);
  secrets set from files, never printed. No staging name: the fleet
  passes the e2e on its `fly.dev` address, then `fragment.club` and
  `*.fragment.club` move to it (Paul adds the records at Namecheap; the
  VPS keeps running until he retires it). *Cut over 2026-09-23; the
  record is `docs/phase-3.md`.* Until sign-in exists, only
  Paul's key may create fragments. An outbound firewall keeps jobs off
  the fleet's private network. Live checks: one OpenRouter call (text,
  one small image) and a push to a real phone.
  `cargo xtask deploy <fleet>` reads that fleet's configuration
  (hostname suffix, bucket, Sprites org) rather than constants, so
  phase 10 adds fleets without a migration.
- **Acceptance:** the e2e suite passes against the hosted URL; durable
  write latency (fleet proof), cold cell load, and cell-to-Sprite
  latency recorded; an operator runbook.

### 4. Friends alpha: invite-only sign-in and budgets (built 2026-09-24)
- Pulled forward so Paul can share fragment.club with friends without an
  obvious problem (2026-09-23); reshaped to finite.computer's identity
  model (decision 15, 2026-09-24; slices in `docs/phase-4.md`; slices A,
  the registry and identities, B, sign-in and sessions, and C, budgets,
  built 2026-09-24; D, the deploy, live on fragment.club the same day). A
  registry of identities (person, agent, fragment), their public keys,
  and each agent's owner, in BANKS's shape. Members name identities.
  Sign-in is fragment's own WorkOS environment with sign-up switched off;
  Paul invites people from the WorkOS dashboard (decision 12; a fake
  stands in locally). Signing in finds or creates the person for that
  `(issuer, subject)`; the platform holds no key for them. A session on
  the platform origin maps a browser to that person; each fragment is
  served from its own origin (`<label>--<username>.fragment.club`, the suffix from
  configuration) and gets its own cookie through a single-use exchange,
  with the hostname checked before it is trusted. `fragment login`
  becomes a browser sign-in that adds the CLI's key to the person, with
  proof of possession; creating a fragment needs a person (replacing
  phase 3's allowlist).
- Budgets (decision 14): the ledger in the billing org's cell (for now
  always the person's own), reservations and settlement on every
  `job.ai` step, usage rows in the shape finite.computer's Core will
  take (`docs/finite-integration.md`), per-person OpenRouter keys with
  credit limits, the monthly reset, `fragment budget`, the platform bar,
  per-run costs. Computers join the same ledger in phase 8.
- **Acceptance:** identical allow/deny decisions for CLI and browser
  across public, link, and members; negative tests (no session, not
  invited, revoked member, revoked key, cross-fragment cookie, the
  registry down) and a session restart test; a key replaced without any
  grant rewritten; an agent's owner reads what the agent reads and
  cannot write through it. Budgets, against the OpenRouter fake: a step over the remaining
  budget is held and succeeds on replay after a top-up; spend matches
  the fake's reported costs; two jobs racing for the last dollar cannot
  both run; the monthly reset; a public visitor's call bills the owner;
  a fragment with its own key is not metered; the person's OpenRouter
  key carries their limit.

### 5. Agents as members (built 2026-09-23)
- The agent cell: goose's loop as a workers-rs Durable Object in its own
  celld project (`agent/`), ported from `spike/goose-agent`'s `cell/`
  (store, effects, durable steer queue, watchdog). Its tools are the
  operations of the fragments it belongs to, keyed by tool-call id; the
  turn's visible messages stream into the chat's channel. Model calls use
  the owner's credential.
- **Acceptance:** the goose spike's checks, ported to the Rust e2e (steer
  mid-tool, stop, SIGKILL mid-tool runs the effect once, SIGKILL between
  steps); an agent uses the todo fragment only through its operations.
  *Met 2026-09-23 (`docs/phase-5.md`); hosted since phase 6, co-hosted
  on the cell's nodes, on the owner's budget.*

### 6. The desktop (built 2026-09-24)
- **Reframed 2026-09-24 (Paul): `docs/phase-6.md` is the plan.** Fragment
  is the core product and the desktop is a template anyone deploys with a
  click; people get usernames and fragments live at
  `<label>--<username>.fragment.club` (decision 16); the phase also brings
  back chatting with goose, the agent making apps, and computer
  screenshots.
- The person's home fragment, laid out as the Desktop UI session designed
  it (`finite-next-worktrees/desktop-ui`, branch `claude/desktop-ui`):
  a collapsible sidebar of chats and apps, the chat in the middle, and a
  collapsible viewer on the right that stacks app and file panes, newest
  on top, reordered by dragging a header. split-grid (vendored, ~3 KB),
  Finite's look (not a code-tool look); files are `{fragment, path}` read
  through the fragment file API. Forkable and resettable.
- **Acceptance:** a browser e2e opens apps and files into the viewer,
  reorders and closes panes, collapses both sides, and works at phone
  width; the layout survives a reload. **Met 2026-09-24** (e2e lane
  `desktop`, 17 checks; the desktop is `templates/desktop`), and live on
  fragment.club the same day with the rest of `docs/phase-6.md`.

### 7. Chats and sharing (built 2026-09-25, except the share header)
- A chat template (a `chat` channel) whose agent member answers new
  messages. Platform shell: share sheet, invites, direct URLs with the
  share header, shared badges in the desktop's sidebar.
- **Acceptance:** two users, one chat: invite, accept, both see messages
  stream live and labeled by sender; the guest drives the owner's agent;
  revoking closes the guest's socket and returns 403; a rewritten
  desktop cannot share without the sheet click.
- *Built:* the chat template, the owner's agent in every new chat, the
  share sheet, invites, direct URLs, and shared badges (`docs/phase-7.md`,
  e2e `phase7`). *Not built:* the share header (decision 4), unscheduled.
- *Folded 2026-09-26 (decision 19):* the chat half becomes the agent
  add-on (D) and the desktop rebuilt on it (F).

### 8. Computers and builder workspaces on Sprites (first part built)
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
- *Live:* the first part (`docs/phase-8.md`): `fragment computer serve`
  attached to an agent by URL and token, the loop still in the agent's
  cell; phase 6 added `fragment computer connect` (no public URL) and
  screenshots in the chat. *Not built:* the `Computer` cell and Sprites
  lifecycle, the loop on the computer, credentials through connectors,
  builder workspaces.
- *Folded 2026-09-26 into phase E (decision 21):* what is not built is
  E's, reshaped around computers as identities.

### A. The front door
- A released `fragment` CLI (macOS and Linux) with a one-line install; a
  skill that teaches a coding agent to install, pair, and publish, made
  from `fragment guide` so they cannot drift; fragment.club's home as the
  product (what it is, signed out; your fragments and pairing, signed in).
- **Acceptance:** on clean macOS and Linux machines the one line installs
  the release, `fragment login` pairs it, and it publishes a todo; a
  coding agent given only the skill does the same from an empty folder;
  a browser e2e checks the home page; the hosted e2e runs the release.

### B. fragment.boats, slice 2
- The move (`docs/fragment-boats.md`): fragments at
  `<label>--<username>.fragment.boats`, old hosts and the apex
  redirecting. DNS, certificates, and the deploy are Paul's.
- **Acceptance:** that doc's e2e shapes pass; the hosted e2e passes on
  fragment.boats, redirects included; Paul checks it in three browsers.
- *Built 2026-09-26 (PR `boats-move`, decision 23):* waits for the DNS,
  the certificates, and the deploy.

### C. Declutter
- Take the desktop's and chats' special cases out of the core
  (`docs/platform.md`: the chat page, the owner's agent in every chat,
  the `work` channel, "is it a chat", the new desktop's visibility and
  framing). Each becomes something any fragment may declare, moves into
  its template, or waits behind one seam that D replaces.
- **Acceptance:** no platform path names a chat or the desktop outside
  that seam; the chat, desktop, and phase7 lanes stay green; the phase
  deletes more lines than it adds.
- *Done (PR #45):* "is it a chat" (the desktop tells by its New chat's
  names) and the desktop's framing (one consent, the new-fragment form's).
  The new desktop's visibility stays (`publish.rs`, `first_visibility`):
  fragment.json holds no access. The chat's three wait for D.

### D. The agent add-on
- Decision 20: `agent` in `fragment.json`; turns from signed-in
  visitors' messages and `job.agent` steps, acting as decision 17 says,
  on the owner's budget. The chat template declares one; C's seam goes.
- **Acceptance:** in a calorie-tracker template, "2 eggs and toast" from
  a signed-in member is logged through a declared operation, and an
  undeclared one is refused; an anonymous message starts nothing; a
  `job.agent` step killed mid-turn runs each tool call once; a turn over
  the owner's budget is held; a fragment without an agent gets no agent
  identity, member, or code.

### E. Computers
- Computers as identities first (decision 21): a computer's npub in the
  registry, owned by a person, revocable, acting on what its owner
  granted and on fragments it publishes. Then, on Paul's call on the
  Sprites token, fragment-declared computers: a Sprite (a `Computer`
  cell for its lifecycle) running the `fragment` CLI as that computer,
  which talks back to its parent and can build and publish fragments.
- **Acceptance:** a revoked computer is refused; it reaches only what it
  was granted; its key is the only credential on its disk. On Sprites: a
  declared computer publishes a fragment; Stop cancels its command and
  publishes nothing; a kill mid-tool runs the tool once; a fragment
  without a computer carries nothing of one.

### F. The desktop rebuilt on the add-ons
- `templates/desktop` becomes an ordinary fragment that declares an
  agent (and a computer, after E) and uses only what any fragment may
  declare. Phase 6's look and quality; no new features.
- **Acceptance:** the desktop lane and the phase7 flow pass, and
  `docs/platform.md` has no special case for the desktop.

### 9. Cutover
- `fragment.club` moves to Fly in phase 3. What remains: the VPS
  retired (Paul's call) and the example fragments re-created from
  templates.

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
