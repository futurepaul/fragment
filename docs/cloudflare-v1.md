# fragment v1 on Cloudflare

Status: **decided 2026-10-02** (Paul, in a grilling session that merged
the "Self-hosted sandbox service" and "cloudflare-ify" threads). This
supersedes the celld/Fly hosting decisions in `docs/ROADMAP.md`
(decisions 6, 8, 24, 25 and the sandcastle lane), `docs/one-home.md`,
`docs/two-substrates.md` and `docs/runtime-seam.md` wherever they
disagree. The explainer is the "Fragment on Cloudflare" artifact; the
design prototype is the "Fragment on Cloudflare: v1" canvas.

## The product

fragment is a personal-agent product (in the space of Muse, Grok Bot and
Dot) with the magic of fragments: multiplayer apps, edited together in
git, that your agents build for you. You sign in and land in a chat at
`fragment.club/chat`. Your agents are Hermes profiles on a computer of
your own. They answer you, use your connected accounts, drive their own
screen, and publish apps, sites and brains as fragments that you share
with other people.

fragment is Finite v3. It runs entirely on Cloudflare, and anyone can
deploy it to their own Cloudflare account without forking it. The
self-hosted lane (celld for cells, sandcastle for computers, both
speaking Cloudflare's APIs) returns once this product works.

## Decisions

### Platform

1. **Cloudflare for everything.** Workers, Durable Objects, R2,
   Containers (with the Sandbox SDK 1.0 helpers), Queues, Workflows, the
   Worker Loader with Durable Object Facets, AI Gateway, Browser
   Rendering, Workers AI (speech to text) and DNS. The outside services
   are WorkOS (AuthKit for sign-in, Pipes for connections) and
   code.storage (git) until Cloudflare Artifacts has a commit API or
   reaches GA (a debt-ledger entry with that delete condition).
2. **Port the Rust cell; don't rewrite it.** The cell already runs on
   the Workers model, so it moves to real Workers (workers-rs). The
   substrate swaps are: `KEYS` becomes in-cell code over Worker secrets;
   Tigris becomes R2; the Fly fleet and the celld fork become
   `wrangler deploy`; Sprites and sandcastle become Containers. New
   platform objects (Chat, Computer, the Relay v2 connector, the usage
   ledger) are Rust in the cell, with thin JS shims where workers-rs
   lacks an API (Worker Loader and Facets, the container's `exec` and
   intercepts, the Sandbox helpers). The desktop is a TypeScript SPA.
   The in-fragment goose agent stays a Rust Worker. The CLI stays Rust.
3. **Two Cloudflare accounts:** production, and dev/CI. CI deploys from
   a clean configuration the way a stranger would.
4. **Self-deploy without forking.** Configuration lives in a file
   outside the repo (domains, WorkOS environment, AI Gateway, pricing,
   code.storage organization), and secrets in files named by path.
   `cargo xtask deploy --config <file>` builds and deploys every Worker,
   the SPA and the Hermes image. `SETUP.md` is written for the
   deployer's agent and lists the steps a person must do: make the
   Cloudflare account and API token, the WorkOS environment with a
   Google provider in Pipes, the Google OAuth client, the code.storage
   organization, buy AI Gateway credits, and point the domain at
   Cloudflare. Updating is `git pull` and the same deploy.
5. **Domains as today:** the platform on `fragment.club`, fragments on
   `<label>--<username>.fragment.boats`. Both zones move to Cloudflare.
   A self-deployer may use one zone for both, at weaker isolation.

### The places a person sees

6. **The desktop and chats are platform code**, not fragments. The
   desktop is the platform's SPA at `/chat`, with a preview URL per
   version. A chat is a Chat Durable Object. Every platform surface that
   is not expressible as a fragment is listed, with its reason, in
   `SPECIAL-CASE-INVENTORY.md`. A surface is platform only when that
   is a security win, or it is blessed UI that users can't break. The
   list stays short, and platform pages use the same public APIs as
   fragments.
7. **Layout:** a sidebar (agents across the top, then chats, then apps),
   the conversation, and a side panel whose tabs are the open apps and
   the agent's Computer tab. On a phone: the list, then a chat, then
   a sheet. It installs as a PWA.
8. **Chats are you and your agents.** Direct chats with one agent, and
   group chats of several of your agents. In a group, an `@mention`
   picks who answers; otherwise the lead (the first agent added)
   answers and may hand off with `@`. Other people collaborate with you
   in fragments, not in chats.
9. **Chat in v1:** streaming replies, tool steps as cards, approvals as
   buttons, Stop, attachments both ways, voice input (Workers AI speech
   to text), search across chats, apps and messages, push notifications,
   and rename and archive.
10. **First run:** choose a username, then "What should your first
    agent do?". The agent takes the job, names itself, draws its picture
    and greets you while its computer starts (finite-mono's onboarding
    layout).
11. **The Computer tab** is the agent's screen (live VNC, Take over,
    Give back), drawn by the platform for the owner only and opened from
    a card in the chat.
12. **Settings and profile:** profile, agents, skills, sites (fragment
    apps with preview cards), brains, connections, the computer, usage
    and credits, plan, and the CLI.

### Agents and their computer

13. **One computer per person for now.** A computer is its own entity
    that agents point at, so ephemeral computers and bring-your-own
    machines can return without a redesign.
14. **An agent is a Hermes profile** on that computer. One Hermes
    gateway serves every profile (`gateway.multiplex_profiles`).
15. **An agent's portable self is a git repo** (a fragment of kind
    `agent`): `SOUL.md` (its job and persona), `memories/`, `skills/`,
    its avatar and its settings. The computer materializes it into the
    Hermes profile at boot and commits Hermes' changes back. Memories
    and custom skills are versioned, can be undone, and a future goose
    agent can read them.
16. **Making an agent:** "What's this agent's job?", an optional name
    (otherwise it chooses), which connections it may use, and its model
    tier. It draws its own avatar in the operator's style reference.
    Approvals default to Hermes' `smart` mode.
17. **Skills are git-tracked.** A platform skills repo (the managed
    set, versioned) plus each agent's own `skills/`. The profile page
    lists them. The managed set is all of finite-skills except
    `shared-skills` (git replaces it). The Finite-specific skills are
    rewritten for fragment: sites, publishing and website building become
    one apps skill, and git, brain and Google are rewritten too.
    `fal-image-editing` moves to Cloudflare's own inference (Paul,
    2026-10-02: agents can do anything they could do in Finite).
18. **Backups.** Hermes' SQLite databases stream continuously to R2
    through Litestream, sent to an intercepted S3 endpoint so the guest
    holds no credential. `/data` is saved with `DirectoryBackup` at
    sleep and every few minutes while written. The agent repo is in git.
    CI runs a restore drill into an empty computer.
19. **Hermes updates.** The image is pinned per computer. A new default
    image reaches a sleeping computer at its next wake. Wakes restore
    `/data` anyway, so the update path is the path every wake takes.
    A canary is a per-computer pin. Rollback is the pin back, plus a
    point-in-time restore when the new image moved Hermes' schema.
20. **Preview environments.** Every branch deploys a complete, separately
    named copy to the dev account (its own Workers, DOs, containers and
    R2 prefix) with one command. The hosted e2e, including real Hermes
    on the candidate image, runs against it.
21. **Relay v2.** The platform's end of Hermes' Relay contract lives in
    the Computer DO and declares the structured operations: `draft` for
    streaming, `prompt` and `prompt_response` for approval buttons,
    `task_card` for tool steps, `send_media`, `follow_up`, `react`,
    `typing`, `edit` and `delete`. The text and emoji guessing is
    deleted. A real-Hermes test lane runs the actual image with a
    scripted model.

### Connections, models, money

22. **Connections through WorkOS Pipes.** WorkOS holds and refreshes
    Google's tokens. The computer holds only placeholders. Its HTTPS
    intercept swaps in a short-lived token for an agent allowed that
    connection. Sending email, sharing files and accepting invites ask
    in the chat. Per-agent limits are a guardrail, not a wall, because
    agents share a computer.
23. **Models through AI Gateway**, Unified Billing, with zero data
    retention on and the gateway's request logs off. The operator maps
    three tiers (fast, balanced, most capable) to models. The computer's
    model intercept adds the gateway credential and reads usage for the
    ledger.
24. **Every per-person cost is metered** in integer micro-dollars into
    a per-person usage ledger: AI, compute (awake time at the instance's
    rate), storage (R2, SQLite, git), requests, unique dynamic workers
    per day, screenshots and images. Price is Cloudflare's cost times
    the operator's margin. Each meter batches into the payer's ledger,
    so no global object sits on a hot path.
25. **Plans.** Guests sign in free and use fragments shared with them;
    they have no agents. A $100 seat includes $50 of credit a month and
    a computer that sleeps when idle. A $200 seat includes an always-on
    computer (its awake time not metered), SimpleX, and $100 of credit.
    More credit can be bought. Stripe arrives later through two hooks:
    granting credit, and a seat's state.
26. **A fragment's costs bill its owner**, with a monthly cap the owner
    sets per fragment (default $5). Past it, AI steps and agent turns
    stop for everyone but the owner.
27. **At zero credit** agents stop (no turns, wakes or AI steps), and
    the chat says why. Fragments keep serving and taking writes on a
    $2 overdraft, then go read-only until a top-up.
28. **Sign-up is invite-only** at cutover.

### Fragments, brains, sites

29. **All of today's fragment model is ported**, core first: operations
    over the app's SQLite (Loader and Facets), live updates, members and
    invites, files in git and deploys, isolation on fragment.boats, the
    in-fragment goose agent, then jobs and cron (Workflows), deliveries,
    webhooks and push (Queues), blobs (R2), secrets and outbound fetch,
    and AI steps. The cutover waits for all of it.
30. **A brain is a fragment app** (template `brain`) on the platform's
    vault UI and the same CLI. It keeps Finite Brain's data architecture
    (top-level folders as wikis; `raw/ wiki/ inventory/ datasets/
    output/`; `AGENTS.md`, `index.md`, `log.md`), its wiki skill, and
    its search (FTS5 sections with BM25 in the app's SQLite, rebuilt on
    push). Assets are blobs referenced by source notes. Access is per
    brain. There is no encryption.
31. **Sites are fragment apps.** Every deploy takes a screenshot with
    Browser Rendering for the app's preview card.
32. **SimpleX on $200 seats only.** The `simplex-chat` daemon runs on
    the always-on computer, and Hermes' SimpleX adapter runs next to
    Relay. A wake service for sleeping computers comes later. Telegram
    is dropped. Finite Chat is out of scope.

### The cut

33. **Hard cut.** Deleted: goose on a computer (hand-offs, `computer.rs`,
    `memory.rs`, `crates/computer`, the builder and pet templates,
    Sprites); the desktop, chat and hermes templates; sandcastle's msb
    control plane, iroh and its web client; the celld fork, the native
    `KEYS`, `crates/node` and the Fly fleet; and every e2e lane for them.
    The krun engine moves to its own repo, which Paul creates. Kept: the
    in-fragment goose agent. Nothing on fragment.club migrates. People
    sign in again with the same WorkOS identity, and one seed carries
    usernames across.
34. **Infra comes down after cutover**, one irreversible step at a
    time, each confirmed by Paul on the day.
35. **master becomes the Cloudflare line.** fragment.club on celld
    deploys only from the `celld` branch (tag `celld-final`, `160d3a1`,
    pushed 2026-10-02) until cutover.

### Agents acting for people (Paul, 2026-10-02, after the audit)

36. **Sharing with a person shares with their agents.** Grants name
    people; a person's agents act for them, never above that person's
    role. Example: share a fragment with @skyler as an editor, and
    Skyler's agents can edit it as "Skyler's agent Juniper, for skyler".
    This is BANKS (Linear FIN-11): people and agents are distinct
    identities, each agent has one owner, the owner can read what the
    agent can read, and an agent may be held below its owner.
    - Skyler can limit which of his agents act in what is shared with
      him (all of them by default).
    - The fragment's owner can mark one share "people only".
    - Something an agent makes is owned by its person; the agent is its
      creator.
    - Each agent's own model and compute bill its owner. The fragment's
      hosting bills the fragment's owner (decision 26).
37. **Agents can do anything they could do in Finite**, with
    credentials handled better. Services that act as you (Gmail,
    Calendar, Drive, Notion, Linear, Monday, an X account) are your
    connections through WorkOS Pipes. Paid APIs that only need a key
    (search such as Perplexity and Grok's X search, Google Places, music
    and image generation) use the operator's keys. Both kinds are
    swapped in at the computer's intercept, and every call is metered to
    your credit at cost times the margin.
38. **Routines wake the computer.** Hermes' cron jobs (Bot Mode
    routines) live on the computer. Before it sleeps, the Computer DO
    reads the next due time and sets its alarm, so a routine on a $100
    seat runs on time.

## Architecture

```
browser ── fragment.club ────────────┐      ┌── <label>--<user>.fragment.boats
                                     ▼      ▼
                         router Worker (the cell, Rust + JS shims)
   ┌────────────┬───────────┬───────────┬──────────────┬─────────────┐
 Registry     Person      Chat       Computer       Fragment        Ledger
 usernames,   profile,    members,   container,     members, ops,   usage,
 sessions,    agents,     messages,  Relay v2,      live, deploys,  credit,
 CLI keys     plan        drafts,    intercepts,    jobs, schedules caps
                          turns      backups          │
                                        │             └─ app facet (Worker
                                        │                Loader, own SQLite)
                              Container: Hermes (profiles), simplex-chat
                              egress only through intercepts:
                                model.internal → AI Gateway (+ usage)
                                *.googleapis.com → token from WorkOS Pipes
                                r2.internal (S3) → R2 (Litestream)
                                relay → the Computer DO
 R2: blobs, site copies, screenshots, /data backups, Litestream replicas
 code.storage: every fragment's git, agent repos, the skills repo
 Queues: deliveries, push, ledger batches   Workflows: jobs
 goose agent Worker (Rust): a fragment's own agent, behind a service binding
```

The SPA talks to Person and Chat over WebSockets. The Computer DO is the
only thing that talks to its container: it starts it, keeps the Relay
socket, bridges the screen's WebSocket from `getTcpPort`, runs the
intercept handlers, and decides when the computer sleeps.

## Phases

Each phase ends with evidence: the tests that prove it, run where its
exit says.

0. **Spikes on the dev account.**
   - Loader and Facets: a private SQLite per facet, FTS5 in it,
     `limits` honored, `globalOutbound: null`, local dev.
   - The cell on real Workers: startup, `entry.mjs` classes, RPC,
     alarms, hibernation.
   - Hermes on Containers:
     - image size and start time;
     - multiplexed profiles;
     - Relay through an intercept (WebSocket upgrade);
     - the model intercept with streaming and usage capture;
     - the Google swap;
     - the screen over `getTcpPort`;
     - Litestream through the S3 intercept;
     - a `DirectoryBackup` round trip, and a save at SIGTERM.
   - AI Gateway: Unified Billing, ZDR, metadata, the 200 requests per
     minute cap, usage fields.
   - WorkOS Pipes: Google tokens, and Google's client libraries against
     placeholder credentials.
   - A per-branch full deployment with wildcard hosts.

   Exit: a verdict and evidence for each spike.
1. **Freeze and cut.** Tag `celld-final` (done 2026-10-02). Delete the
   product half of decision 33 (goose on a computer, Sprites, the
   desktop, chat and hermes templates, the sandcastle-backed Hermes and
   its screen, sandcastle's msb stack). Keep the substrate (the celld
   fork, native `KEYS`, the fleet) until phase 2 replaces it, so the
   remaining e2e stays green on celld as the baseline.
2. **The cell on Cloudflare.** `KEYS` in the cell; R2; wrangler; the
   dev stack on `wrangler dev`; the e2e on workerd plus a hosted lane
   on a preview. The celld fork, native `KEYS`, `crates/node` and the
   Fly fleet are deleted here. Exit: core fragment sections green
   locally and on a preview.
3. **The ledger.** Meters, plans, guests, caps, the overdraft and
   read-only, and operator commands. Exit: metering matches the price
   book, and caps and zero behave; valid, invalid and replay tests for
   each mutation.
4. **The computer.** The Computer DO, the Hermes image, Relay v2, the
   intercepts, backups and updates. Exit: the real-Hermes lane covers:
   - a reply, streaming, steps, an approval, and Stop;
   - attachments;
   - a restart mid-turn;
   - a wake with restore;
   - an upgrade, then a rollback;
   - two profiles, and a group `@mention`;
   - a routine that wakes a sleeping computer on time;
   - delegation (decision 36): another person's agent edits a fragment
     shared with that person as an editor; a people-only share refuses
     it; an agent held below its owner is refused.
5. **The desktop.** The SPA and Chat DO: onboarding, agents,
   connections, settings and profile, search, push, voice, the phone
   layout. Exit: the browser lane at desktop and phone sizes, on a
   preview.
6. **Brains, skills, sites.** The brain template, the skills repo,
   Hermes' fragment skills, and screenshots. Exit: from a chat, an
   agent builds, publishes and shares an app, ingests into a brain and
   searches it; the skills list matches the repo.
7. **The rest of fragments.** Jobs, cron, deliveries, webhooks, push,
   blobs, secrets, AI steps. Exit: their e2e sections green.
8. **Self-deploy.** An agent following `SETUP.md` deploys into a fresh
   account from a clean config, and the hosted e2e passes there.
9. **SimpleX** on $200 seats, always on.
10. **Cutover.** Zones to Cloudflare, the production deploy, the username
    seed, invites; then the old infra comes down (decision 34).

## Evaluation

- **Pure state machines** in `crates/core`: Relay v2, chat turns, the
  ledger, price math. Each mutation gets valid, invalid, replay and
  restart tests. Relay and the ledger also get deterministic simulation
  (reconnects, duplicate and reordered acks, Stop during an approval,
  crash between a meter and its flush).
- **The e2e on workerd**, with fakes only at vendor boundaries:
  code.storage, WorkOS, and a scripted model behind the model
  intercept. These are labeled lower-rung.
- **The real-Hermes lane:** the actual image in a local container with
  a scripted model, covering the list in phase 4.
- **The hosted lane on a preview:** the same suites against a real
  deployment, with real AI Gateway (cents), WorkOS' dev environment,
  and Pipes' shared credentials.
- **The browser lane:** headless Chrome on the SPA, desktop and phone.
- **The self-deploy lane** (phase 8), and **restore drills** into an
  empty computer.

## Risks

- **Gmail scopes.** Google's verification (a CASA assessment) is needed
  before more than 100 users can connect Gmail.
- **Beta platform pieces:**
  - AI Gateway's Unified Billing allows 200 requests a minute per
    gateway until raised;
  - the Worker Loader, Facets and the containers' `durable_object`
    scheduling policy are betas.
- **Unproven paths:**
  - a WebSocket through a container intercept;
  - restore time for a large `/data`.
- **Isolation between agents.** Agents on one computer share it, so
  per-agent connection limits are not a security boundary.
- **SimpleX cost.** An always-on computer costs about $42 a month at
  list price.

## Escalations (ask Paul)

- Product semantics, pricing, privacy posture.
- Production deploys.
- Zone and DNS moves.
- Every teardown or delete.
- New vendor accounts and secrets.
- Creating repos (sandcastle's).
- Spending AI Gateway credit beyond test cents.
- Google's verification.
