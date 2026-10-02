# fragment v1 on Cloudflare

Status: **decided 2026-10-02** (Paul, in a grilling session that merged
the "Self-hosted sandbox service" and "cloudflare-ify" threads, then
revised the same day by the rule below). This supersedes the celld/Fly
hosting decisions in `docs/ROADMAP.md` (decisions 6, 8, 24, 25 and the
sandcastle lane), `docs/one-home.md`, `docs/two-substrates.md` and
`docs/runtime-seam.md` wherever they disagree. The explainer is the
"Fragment on Cloudflare" artifact; the design prototype is the
"Fragment on Cloudflare: v1" canvas.

## The rule: fragments and computers

The platform is **fragments and computers**, plus only what those two
need: identity, sign-in, metering and credentials. It stays as thin and
generic as possible (Paul, 2026-10-02: "the thicker our platform grows
outside that and is special-cased around hermes the harder it will be to
change"; finite v2 is the warning).

- We happen to run **Hermes** on computers today, and we chat with it
  through a fragment. Hermes is not a platform concept. Its image, the
  bridge in that image, and the chat and agent templates are data the
  platform runs. The platform contains no Hermes code.
- Anything not expressible as a fragment or a computer is listed, with
  its reason, in `SPECIAL-CASE-INVENTORY.md`. A thing is platform only
  when that is a security win, or it is the thin shell users can't
  break. Platform pages use the same public APIs as fragments.
- A change that teaches the platform about one agent runtime, one
  template or one vendor is a design bug. Move it into an image, a
  template or a skill.

## The product

fragment is a personal-agent product (in the space of Muse, Grok Bot and
Dot) with the magic of fragments: multiplayer apps, edited together in
git, that your agents build for you. You sign in to the shell at
`fragment.club/chat`. Your agents are fragments run by a computer of
your own (today, Hermes profiles). You chat with them in chat fragments.
They use your connected accounts, drive their own screen, and publish
apps, sites and brains as fragments that you share with other people.

fragment is Finite v3. It runs entirely on Cloudflare, and anyone can
deploy it to their own Cloudflare account without forking it. The
self-hosted lane (celld for cells, sandcastle for computers, both
speaking Cloudflare's APIs) returns once this product works.

## Decisions

### Platform

1. **Cloudflare for everything.** Workers, Durable Objects, R2,
   Containers (with the Sandbox SDK 1.0 helpers), Queues, Workflows, the
   Worker Loader with Durable Object Facets, AI Gateway, Browser
   Rendering, Workers AI and DNS. The outside services are WorkOS
   (AuthKit for sign-in, Pipes for connections) and code.storage (git),
   which stays until Cloudflare Artifacts has a commit API or reaches GA
   (a debt-ledger entry with that delete condition).
2. **Port the Rust cell; don't rewrite it.** The cell already runs on
   the Workers model, so it moves to real Workers (workers-rs). The
   substrate swaps are: `KEYS` becomes in-cell code over Worker secrets;
   Tigris becomes R2; the Fly fleet and the celld fork become
   `wrangler deploy`; Sprites and sandcastle become Containers. The new
   platform pieces are the generic Computer DO and the usage ledger.
   They are Rust in the cell, with thin JS shims where workers-rs lacks
   an API (Worker Loader and Facets, the container's `exec` and
   intercepts, the Sandbox helpers). The shell is a small TypeScript
   SPA. The in-fragment goose agent stays a Rust Worker, and the CLI
   stays Rust.
3. **Accounts.** Dev and previews run on Paul's personal Cloudflare
   account. Production gets its own account before cutover. CI deploys
   from a clean configuration, the way a stranger would. WorkOS'
   staging environment serves dev and previews; its production
   environment serves fragment.club.
4. **Self-deploy without forking.** Configuration lives in a file
   outside the repo: domains, the WorkOS environment, AI Gateway,
   pricing, the code.storage organization, and the default computer
   image. Secrets live in files named by path. `cargo xtask deploy
   --config <file>` builds and deploys every Worker, the shell and the
   computer image. `SETUP.md` is written for the deployer's agent and
   lists the steps a person must do: make the Cloudflare account and
   API token, the WorkOS environment with Pipes' providers, the Google
   OAuth client, the code.storage organization, buy AI Gateway credits,
   and point the domain at Cloudflare. Updating is `git pull` and the
   same deploy.
5. **Domains as today:** the platform on `fragment.club`, fragments on
   `<label>--<username>.fragment.boats`. Both zones move to Cloudflare.
   A self-deployer may use one zone for both, at weaker isolation.

### What a person sees

6. **The shell is the platform's one page.** Sign-in, a sidebar of your
   fragments (by kind: agents, chats, apps and brains) and your
   computers, and tabs that frame them. It knows nothing about Hermes.
   Everything inside a tab is a fragment or a computer. It ships with a
   preview URL per version.
7. **Layout:** the sidebar (agents across the top, then chats, then
   apps); the open chat fragment; a side panel whose tabs are other
   open fragments and a computer's screen. On a phone: the list, then a
   chat, then a sheet. It installs as a PWA.
8. **Chats are chat fragments**, with you and your agents as members:
   a direct chat with one agent, or a group of several. In a group, an
   `@mention` picks who answers; otherwise the lead (the first agent
   added) answers and may hand off with `@`. The chat template and each
   agent's bridge implement this through ordinary channels. Other
   people collaborate with you in other fragments, not in chats.
9. **Chat in v1** comes from the chat template: streaming replies
   (drafts), tool steps as cards, approvals as buttons, Stop,
   attachments both ways (blobs), voice input (an AI step running
   Workers AI speech to text), push notifications (fragment push), and
   rename and archive. Search across chats, apps and messages is the
   shell's, over your fragments' channels and files.
10. **First run:** choose a username, then "What should your first
    agent do?". Through public APIs the shell makes your computer (the
    operator's default image), an agent fragment, and a chat fragment
    with both of you in it. The agent takes the job, names itself, draws
    its picture and greets you while the computer starts (finite-mono's
    onboarding layout).
11. **A computer serves its own screen.** The image serves a VNC client
    page, with Take over and Give back. The platform gives the
    computer's owner, and their delegates, an authenticated tab onto
    that port. A chat card can ask the shell to open it.
12. **Your profile** in the shell: your account, your fragments by kind
    (agents with their skills, sites and apps with preview cards,
    brains), your computers, connections, usage and credits, plan, and
    the CLI.

### Agents and computers

13. **One computer per person for now.** A computer is its own entity,
    so ephemeral computers and bring-your-own machines can return
    without a redesign. A computer runs an image. The image, not the
    platform, decides what runs: today, Hermes.
14. **An agent is a fragment** (kind `agent`) with its own identity.
    Fragments are how agents keep state:
    - its repo: `SOUL.md` (its job and persona), `memories/`, `skills/`,
      its avatar;
    - its SQLite;
    - its cron (its routines; decision 38);
    - its members (its owner, and delegation, decision 36);
    - its page (the agent's settings);
    - its usage cap.

    The fragment names the computer that runs it.
15. **Today an agent runs as a Hermes profile.** The Hermes image's
    boot script checks out the agent fragments assigned to its computer
    into profiles, and commits Hermes' changes back. One Hermes gateway
    serves every profile (`gateway.multiplex_profiles`). Memories and
    custom skills are versioned, can be undone, and a future goose agent
    can read them.
16. **Making an agent** starts the agent template: "What's this agent's
    job?", an optional name (otherwise it chooses), which connections it
    may use, and its model tier. It draws its own avatar in the
    operator's style reference; the `avatar-lab` fragment on fragment.club
    is where that style is iterated. Approvals default to Hermes' `smart`
    mode.
17. **Skills are git-tracked.** The managed set is a skills fragment
    (a blessed template's repo, versioned). Each agent's own skills are
    in its fragment's `skills/`. The managed set is all of finite-skills
    except `shared-skills`, which git replaces. The Finite-specific skills
    are rewritten for fragment: sites, publishing and website building
    become one apps skill, and git, brain and Google are rewritten too.
    `fal-image-editing` moves to Cloudflare's own inference (Paul,
    2026-10-02: agents can do anything they could do in Finite).
18. **Backups are a computer feature.** `/data` is saved with
    `DirectoryBackup` at sleep and every few minutes while written. The
    platform gives every computer an S3 endpoint scoped to its own R2
    prefix through an intercept, so the guest holds no credential. The
    Hermes image uses it for Litestream, streaming Hermes' SQLite
    continuously. The agent's self is in its fragment. CI runs a restore
    drill into an empty computer.
19. **Image updates.** The image is pinned per computer. A new default
    image reaches a sleeping computer at its next wake. Wakes restore
    `/data` anyway, so the update path is the path every wake takes.
    A canary is a per-computer pin. Rollback is the pin back, plus a
    point-in-time restore when the new image moved its data's schema.
20. **Preview environments.** Every branch deploys a complete, separately
    named copy to the dev account with one command: its own Workers, DOs,
    Workflows, Queues, containers and R2 prefix. It is routed on the dev
    zone `finite.place` (Paul's, 2026-10-02), with one proxied wildcard
    DNS record that Universal SSL covers:
    - the shell for branch `b` is `b.finite.place`;
    - its fragments are `<label>--<user>--b.finite.place`, routed by
      `*--b.finite.place/*`;
    - the long-lived dev deployment is the branch `dev`. The hosted e2e runs against it,
    including real Hermes on the candidate image. A teardown command
    removes a branch's copy, since an account holds at most 500 DO
    classes (about 80 branches).

    Cloudflare's Worker Previews can't be this. Spike S1 (2026-10-02)
    found that a Preview's Workflows, Queue consumers and service bindings
    stay on production, and that it can't host per-fragment origins.
    Previews may still serve the shell alone.
21. **The Hermes bridge lives in the Hermes image.** It is a small process
    next to Hermes. It speaks Hermes' Relay contract locally, with the
    structured operations:
    - `draft` for streaming;
    - `prompt` and `prompt_response` for approval buttons;
    - `task_card` for tool steps;
    - `send_media`, `follow_up`, `react`, `typing`, `edit` and `delete`.

    Outward, it speaks the ordinary fragment API to the chat and agent
    fragments: channels, drafts and operations, with the computer's
    identity swapped in at egress. The platform has no Hermes code, and
    another runtime brings its own bridge. A real-Hermes test lane runs
    the actual image with a scripted model.

### Connections, models, money

22. **Connections through WorkOS Pipes.** WorkOS holds and refreshes the
    tokens. The computer holds only placeholders. Its HTTPS intercept
    swaps in a short-lived token for an identity allowed that
    connection. Templates ask before sending email, sharing files or
    accepting invites. Per-agent limits are a guardrail, not a wall,
    because agents share a computer.
23. **Models through AI Gateway**, Unified Billing, with zero data
    retention on and the gateway's request logs off. Three tiers: GLM-5.3
    Flash (cheap), GLM-5.3 (medium), both on Workers AI, and Claude Opus
    5.5 (high). The computer's model intercept adds the gateway credential
    and reads usage for the ledger.
24. **Every per-person cost is metered** in integer micro-dollars into
    a per-person usage ledger:
    - AI;
    - compute (awake time at the instance's rate);
    - storage (R2, SQLite, git);
    - requests;
    - unique dynamic workers per day;
    - screenshots and images.

    Price is Cloudflare's cost plus a 50% margin, set by the operator.
    Each meter batches into the payer's ledger, so no global object sits
    on a hot path.
25. **Plans.** Guests sign in free and use fragments shared with them;
    they have no agents. A $100 seat includes $50 of credit a month and
    a computer that sleeps when idle. A $200 seat includes an always-on
    computer (its awake time not metered), SimpleX, and $100 of credit.
    More credit can be bought. Stripe arrives later through two hooks:
    granting credit, and a seat's state.
26. **A fragment's costs bill its owner**, with a monthly cap the owner
    sets per fragment (default $5). Past it, AI steps and agent turns
    stop for everyone but the owner.
27. **At zero credit** agents stop: no turns, no wakes, no AI steps. The
    shell says why. Fragments keep serving and taking writes on a $2
    overdraft, then go read-only until a top-up.
28. **Sign-up is invite-only** at cutover.

### Fragments, brains, sites

29. **All of today's fragment model is ported**, core first.
    - Core: operations over the app's SQLite (Loader and Facets), live
      updates, members and invites, files in git and deploys, isolation
      on fragment.boats, and the in-fragment goose agent.
    - Then: jobs and cron (Workflows); deliveries, webhooks and push
      (Queues); blobs (R2); secrets and outbound fetch; and AI steps.

    The cutover waits for all of it.
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
    the always-on computer, and Hermes' SimpleX adapter runs next to the
    bridge. A wake service for sleeping computers comes later. Telegram
    is dropped. Finite Chat is out of scope.

### The cut

33. **Hard cut.**
    - Deleted:
      - goose on a computer: hand-offs, `computer.rs`, `memory.rs`,
        `crates/computer`, the builder and pet templates, Sprites;
      - the personal-agent chat and the platform chat page;
      - the desktop template and its capabilities;
      - the sandcastle-backed Hermes, Relay connector and screen;
      - sandcastle's msb control plane, iroh and its web client;
      - the celld fork, the native `KEYS`, `crates/node` and the Fly
        fleet;
      - every e2e lane for them.
    - The chat and agent templates are rebuilt on the rule above.
    - The krun engine moves to its own repo, which Paul creates.
    - Kept: the in-fragment goose agent.
    - Nothing on fragment.club migrates. People sign in again with the
      same WorkOS identity, and one seed carries usernames across.
34. **Infra comes down after cutover**, one irreversible step at a
    time, each confirmed by Paul on the day.
35. **master becomes the Cloudflare line.** fragment.club on celld
    deploys only from the `celld` branch (tag `celld-final`, `160d3a1`,
    pushed 2026-10-02) until cutover.

### Agents acting for people (Paul, 2026-10-02)

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
    credentials handled better.
    - Services that act as you (Gmail, Calendar, Drive, Notion, Linear,
      Monday, an X account) are your connections through WorkOS Pipes.
    - Paid APIs that only need a key (search such as Perplexity and
      Grok's X search, Google Places, music and image generation) use
      the operator's keys.
    - Both kinds are swapped in at the computer's intercept, and every
      call is metered to your credit at cost plus the margin.
38. **Routines are fragment cron.** An agent's routines are its
    fragment's cron triggers, not Hermes' own scheduler, which is turned
    off. A trigger posts to a channel the agent's bridge subscribes to,
    and the generic rule of decision 39 wakes the computer. Agents add
    and change routines with the fragment CLI (a skill).

### The generic pieces that make the rule work (Paul, 2026-10-02)

39. **A subscription wakes a computer.** A computer subscribes to
    fragment channels (its bridge does, through the API). A record on a
    subscribed channel wakes a sleeping computer, and the bridge catches
    up from its last sequence number. Chats, routines, webhooks and
    anything else reach agents this way.
40. **Blessed templates run the platform's current version.** A chat,
    agent, brain or skills fragment names its template, and it serves
    that template's code from the current platform release. One deploy
    updates every chat, with no frozen copies and no drift. Forking a
    fragment makes its code your own.
41. **A computer exposes ports to its owner.** The platform proxies an
    authenticated tab or socket to a named port, for the computer's
    owner and their delegates. This is how the screen (decision 11) and
    any other UI an image serves are reached. Nothing else is
    reachable from outside.

## Architecture

```
browser ── fragment.club (the shell) ─┐      ┌── <label>--<user>.fragment.boats
                                      ▼      ▼
                          router Worker (the cell, Rust + JS shims)
   ┌──────────────┬──────────────┬────────────────┬───────────────┐
 Registry       Principal      Fragment          Computer        Ledger
 identities,    a person's     members, ops,     its container,  usage, credit,
 owners and     fragments      live, channels,   wake, ports,    plans, caps,
 agents,        and computers  deploys, cron,    egress swaps,   per payer
 sessions,                     jobs, blobs       backups, pin
 CLI keys                         │                  │
                                  └─ app facet       └─ Container (the image's):
                                     (Worker Loader,    today Hermes + its bridge
                                     own SQLite)        + simplex-chat on $200 seats
 egress intercepts (generic, configured per computer):
   model route → AI Gateway (+ usage)   connections → WorkOS Pipes tokens
   storage → its R2 prefix (S3)         fragment API → the computer's identity
 R2: blobs, site copies, screenshots, backups, replicas
 code.storage: every fragment's git (apps, chats, agents, brains, skills)
 Queues: deliveries, push, ledger batches   Workflows: jobs and cron runs
 goose agent Worker (Rust): a fragment's own agent, behind a service binding
```

The shell talks to the platform's public API and frames fragments. The
Computer DO is the only thing that talks to its container: it starts and
stops it, restores and saves its data, runs the generic intercept
handlers, proxies its ports, and wakes it when a subscribed channel gets
a record. It knows nothing about what the image runs.

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
     - the bridge to a fragment's channels through the egress swap;
     - the model intercept with streaming and usage capture;
     - the Google swap;
     - the screen through a port proxy;
     - Litestream through the S3 intercept;
     - a `DirectoryBackup` round trip, and a save at SIGTERM.
   - AI Gateway: Unified Billing, ZDR, metadata, the 200 requests per
     minute cap, usage fields, GLM-5.3's 64K context under Hermes.
   - WorkOS Pipes: Google tokens, and Google's client libraries against
     placeholder credentials.
   - A per-branch full deployment with wildcard hosts.

   Exit: a verdict and evidence for each spike.
1. **Freeze and cut.** Tag `celld-final` (done 2026-10-02). Delete the
   product half of decision 33. Keep the substrate (the celld fork,
   native `KEYS`, the devstack) until phase 2 replaces it, so the
   remaining e2e stays green on celld as the baseline. The fleet's
   deploy path leaves master, so the cut can never reach fragment.club.
2. **The cell on Cloudflare.** `KEYS` in the cell; R2; wrangler; the
   dev stack on `wrangler dev`; the e2e on workerd plus a hosted lane
   on a preview. The celld fork, native `KEYS` and `crates/node` are
   deleted here. Exit: core fragment sections green locally and on a
   preview.
3. **The ledger.** Meters, plans, guests, caps, the overdraft and
   read-only, and operator commands. Exit: metering matches the price
   book, and caps and zero behave; valid, invalid and replay tests for
   each mutation.
4. **Computers.** The generic Computer DO (lifecycle, wake on
   subscription, intercepts, ports, backups, pins), then our Hermes
   image (bridge, boot script, Litestream). Exit: the real-Hermes lane
   covers:
   - a reply, streaming, steps, an approval, and Stop;
   - attachments;
   - a restart mid-turn;
   - a wake with restore;
   - an upgrade, then a rollback;
   - two profiles, and a group `@mention`;
   - a routine (fragment cron) that wakes a sleeping computer on time;
   - delegation (decision 36): another person's agent edits a fragment
     shared with that person as an editor; a people-only share refuses
     it; an agent held below its owner is refused;
   - a test that the platform has no Hermes code: the same lane passes
     with a stub image whose bridge speaks the fragment API.
5. **The shell and the blessed templates.** The shell (onboarding,
   sidebar, tabs, profile, search, connections) and the chat and agent
   templates on decision 40. Exit: the browser lane at desktop and phone
   sizes, on a preview.
6. **Brains, skills, sites.** The brain and skills templates, Hermes'
   fragment skills, screenshots. Exit: from a chat, an agent builds,
   publishes and shares an app, ingests into a brain and searches it;
   the skills list matches the skills fragment.
7. **The rest of fragments.** Jobs, cron, deliveries, webhooks, push,
   blobs, secrets, AI steps. Exit: their e2e sections green.
8. **Self-deploy.** An agent following `SETUP.md` deploys into a fresh
   account from a clean config, and the hosted e2e passes there.
9. **SimpleX** on $200 seats, always on.
10. **Cutover.** Zones to Cloudflare, the production deploy, the username
    seed, invites; then the old infra comes down (decision 34).

## Evaluation

- **Pure state machines** in `crates/core`: the ledger, price math,
  subscription wakes, delegation. Each mutation gets valid, invalid,
  replay and restart tests. The ledger and wakes also get deterministic
  simulation (a crash between a meter and its flush, a wake racing a
  sleep, duplicate records).
- **The bridge's own tests** live with the Hermes image: Relay v2
  against a scripted Hermes, with reconnects, reordered acks, and Stop
  during an approval.
- **The e2e on workerd**, with fakes only at vendor boundaries:
  code.storage, WorkOS, and a scripted model behind the model
  intercept. These are labeled lower-rung.
- **The real-Hermes lane:** the actual image in a local container with
  a scripted model, covering the list in phase 4.
- **The hosted lane on a preview:** the same suites against a real
  deployment, with real AI Gateway (cents), WorkOS' staging environment,
  and Pipes' shared credentials.
- **The browser lane:** headless Chrome on the shell, desktop and phone.
- **The self-deploy lane** (phase 8), and **restore drills** into an
  empty computer.

## Spike results

**S1, the Worker Loader and Facets (2026-10-02, deployed and local;
evidence in the session's `spikes/s1-loader/RESULTS.md`).** The cell's
shape (`js.rs`) runs on real Workers essentially unchanged.

What held:
- each facet has its own SQLite, invisible to the supervisor and to
  other facets;
- a facet's data survives eviction, a parent redeploy, and new code
  (`facets.abort` then `get` with the new class);
- FTS5 and JSON1 work;
- `limits` are enforced;
- `globalOutbound: null` and the Egress loopback behave as fragment
  needs;
- capability props can't be forged from inside.

What the port must add:
- **`platform.mjs` locks the author's storage.** It wraps `setAlarm`,
  `transaction` and `ctx.facets` before `super()`. A facet that sets an
  alarm wedges, answering `internal error` with `durableObjectReset`,
  until the platform Worker is redeployed. Author code could also start
  nested facets outside its database cap.
- **The capability API is batched, or `subRequests` raised.** Capability
  RPC calls count against `subRequests`, so the current cap of 50 limits
  a query to 50 capability calls.
- **`facet_error` mapping:** the 10-in-flight concurrency error maps to
  `NodeFull`, and the CPU error to `AppFailed`. Never retry on
  `overloaded`.
- **Runaway-CPU tests run only on a deployment.** Local workerd enforces
  no `cpuMs`, `subRequests` or concurrency limit, and a busy loop takes
  the whole dev node down.
- **A Loader id is per fragment and per code version.** The isolate, and
  with it its env and props, is shared by everything that uses one id.
  That also gives the billing unit: $0.002 per fragment per version per
  day, past 1,000 a month.

## Bugs the port must fix

Found 2026-10-02 building `avatar-lab` on fragment.club (`celld-final`).
Each one needs a test in the phase that ports its feature.

1. **AI images between 256 KiB and 1 MiB can't be saved.** `store_media`
   (`cell/src/ai.rs`) commits through `commit_files`, which caps a write
   at 256 KiB, while blobs start at 1 MiB. The error was "the files
   written is 347234 bytes; the limit is 262144". Phase 7: a generated
   file is a blob from the first byte, or the two limits meet.
2. **A retried image step buys a new image.** Every `store_media` error
   is retryable, so each retry calls the model again, and only the last
   attempt is settled in the ledger. Phase 3/7: a storage failure after
   a paid call is not retried against the model, and every paid call is
   metered.
3. **A failed step keeps its reservation.** A step that fails for good
   still holds its budget reservation. Phase 3: a final failure releases
   it, with a test.
4. **`fragment deploy` reports success when the code is refused.** It
   printed `live:` while the status said an operation name broke
   `^[a-z][a-z0-9_]{0,63}$`, and the guide never names that rule. Phase 2:
   a refused deploy exits non-zero with the reason; the guide states the
   rule.
5. **An image is served by its path's extension, not its bytes.** A JPEG
   saved at `.png` is served as `image/png`. Phase 7: `job.ai.image`
   picks the extension from the media type, or `__file` sniffs it.
6. **Small job-written files have no cacheable URL.** `__file` is
   `no-store`, and only blobs (1 MiB and up) have `__blob/<sha>`. Phase 7:
   a content-addressed, immutable URL for any committed file.
7. **`fragment create` prints secrets.** The `?view=` token and the
   inbox token go to stdout. Phase 2: they're shown only on request.

## Risks

- **Gmail scopes.** Google's verification (a CASA assessment) is needed
  before more than 100 users can connect Gmail.
- **Beta platform pieces:**
  - AI Gateway's Unified Billing allows 200 requests a minute per
    gateway until raised;
  - the Worker Loader, Facets and the containers' `durable_object`
    scheduling policy are betas.
- **Model context.** GLM-5.3 lists a 64K context window, which may be
  tight for long Hermes runs.
- **Unproven paths:**
  - a bridge's long-lived connection to a fragment through the egress
    swap;
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
