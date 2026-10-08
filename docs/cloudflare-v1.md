# fragment v1 on Cloudflare

Status: **decided 2026-10-02** (Paul, in a grilling session that merged
the "Self-hosted sandbox service" and "cloudflare-ify" threads, then
revised the same day by the rule below). It superseded
`docs/ROADMAP.md`, whose decisions that still hold are below ("Carried
from the ROADMAP", R4 to R18), `docs/one-home.md`,
`docs/two-substrates.md` and `docs/runtime-seam.md`. The explainer is the
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
   SPA, and the CLI stays Rust. The cell is the one Worker (the
   in-fragment goose agent's went: decision 33).
3. **Accounts.** Dev and previews run on Paul's personal Cloudflare
   account. Production gets its own account before cutover. CI deploys
   from a clean configuration, the way a stranger would. WorkOS'
   staging environment serves dev and previews; its production
   environment serves fragment.club.
4. **Self-deploy without forking.** Configuration lives in a file
   outside the repo: domains, the WorkOS environment, AI Gateway,
   pricing, the code.storage organization, and the default computer
   image. Secrets live in the account's Cloudflare Secrets Store, named
   in the file and set with `cargo xtask secret` (Paul, 2026-10-05;
   docs/secrets.md; until then, files named by path). `cargo xtask deploy
   --config <file>` builds and deploys every Worker, the shell and the
   computer image. `SETUP.md` is written for the deployer's agent and
   lists the steps a person must do: make the Cloudflare account and
   API token, the WorkOS environment with Pipes' providers, the Google
   OAuth client, the code.storage organization, buy AI Gateway credits,
   and point the domain at Cloudflare. Updating is `git pull` and the
   same deploy.
5. **Domains as today:** the platform on `fragment.club`, fragments on
   `<name>.fragment.boats`, a name being a label and a random suffix
   (decision 47). Both zones move to Cloudflare.
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

   Status, 2026-10-07 (branch `claude/agents-ask-each-other`): a chat's
   `@` lists all of its owner's agents, the chat's first; one not in the
   chat is added (an editor, by the shell once its person confirms it in
   the shell's own dialog; the shell hands a person's agents only to their
   own fragments' pages: docs/api.md, The shell) before the message names
   it. Agents ask each other by `@` in a shared chat, or
   with `fragment ask <agent> "…" [--wait]`, in a chat of the two and
   their owner. The answering bridge counts the hops (never fewer than a
   record claims; for its own computer's agents, from the turn the poster
   is in), so a CLI or API post resets nothing; a chat's agents start at
   most 20 turns of each other in 5 minutes, past it refused and said so;
   only an agent's own fragment and its owner start its routines
   (docs/chat-records.md; docs/bridge.md, "Agents asking each other").
9. **Chat in v1** comes from the chat template: streaming replies
   (drafts), tool steps as cards, approvals as buttons, Stop,
   attachments both ways (blobs), voice input (a voice memo, an audio
   attachment the agent transcribes itself: the platform runs no speech
   to text; Paul, 2026-10-03), push notifications (fragment push), and
   rename and archive. Search across chats, apps and messages is the
   shell's, over your fragments' channels and files.
10. **First run:** "What should your first agent do?" (no username:
    decision 47). Through public APIs the shell makes your computer (the
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
    platform, decides what runs: today, Hermes. The default size is 2 vCPU
    and 6 GiB (Paul, 2026-10-02), the smallest 2-vCPU shape Cloudflare
    allows at 3 GiB per vCPU.
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
    custom skills are versioned, can be undone, and another runtime can
    read them.
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

    Status, 2026-10-03 (branch `claude/skills`; templates/skills/README.md):
    - Built: the blessed `skills` template, its repo the managed set (42
      skills: finite-skills less shared-skills, apps, fragment git, brain,
      Google through `fragment-connection:google`, image generation through
      a fragment AI step, Finite-only paths and keys stripped). A fragment
      on a blessed template lists and reads that template's data from the
      release, beneath its own files (decision 40). The shell makes each
      person's skills fragment at setup and lists it, by category, beside
      each agent's own, in settings. Our Hermes image installs it into every
      profile (`skills.external_dirs`, an agent's own winning on a name) and
      carries the fragment CLI, whose agent mode acts as the agent through
      the API egress with no key (docs/computers.md).
    - Missing: image editing (FLUX.1 [schnell] is text to image), other
      vendors' models in the model council, and operator keys or
      connections a deployment does not offer (the README's table).

    Status, 2026-10-05 (branch `claude/agents-know-the-platform`; Paul, on
    p5 before a demo: the agent knew nothing of fragment or of its Google
    connection):
    - Built: a platform skill, `fragment`, in every profile of our Hermes
      image whatever the skills fragment holds: the image's CLI's own
      `fragment skill` after a page of the computer's (acting for its
      owner, the apps and brain skills, connections and
      `GOOGLE_OAUTH_ACCESS_TOKEN`, its desktop), written at the image's
      build, named after the managed set in `skills.external_dirs` so a
      managed or own `fragment` wins (docs/computers.md). The shell makes
      a skills fragment, once, for a person with an agent and none (set up
      before 2026-10-03), and an awake computer looks for one every minute
      while its owner has none. (That backfill was cut since: such a person
      adds one from settings, "Add the managed skills", which shell-ui
      drives.)
    - Evidence: hermes-boot's tests (the platform skill with no skills
      fragment, a managed one shadowing it); shell-ui (the backfill); the
      real-Hermes lane (with no skills fragment Hermes lists `fragment` in
      its model's skills index, and a skills fragment made while awake is
      installed within the minute: 46 and 50 s). Sections shell, shell-ui,
      computers and hermes: 257 passed, 0 failed.
    - Not yet: the same on a preview (the hosted lane), and the platform
      skill in the shell's Skills list.
18. **Backups are a computer feature.** `/data` is saved with
    `DirectoryBackup` every few minutes while written. A sleep is driven
    by the Computer DO, in this order:
    1. it holds the guest: it touches `/run/computer/hold`, and the
       guest claims no new turn (docs/computers.md);
    2. it saves `/data`;
    3. it takes the snapshot (below);
    4. it signals the guest;
    5. it waits for the guest to exit;
    6. it destroys the container.

    The inactivity timeout is only a safety net. An idle stop gives the
    guest about 5 seconds of SIGTERM, and the DO can't exec during it.

    **A wake starts from a per-computer snapshot.** At sleep, after the
    save, the DO takes `snapshotContainer()` (5–6 s, about 12 MB). A wake
    then calls `start({containerSnapshot})` when the snapshot is of the
    newest save and of the computer's pinned image (the snapshot is a
    cache of the save; a start from one that fails falls back to the
    image and the save in the same wake). Otherwise it starts the image and
    restores: the container starts with `RESTORE_PENDING=1`, the DO
    restores `/data`, and it touches `/run/computer/restored`, which the
    image waits for before its own init runs. The DO retries a
    "temporarily unavailable" start (spike S3b).

    The agent's self is in its fragment. (The Hermes image streamed
    Hermes' SQLite with Litestream, for disaster recovery, to an S3
    endpoint the platform gave every computer over its own R2 prefix,
    until step 1 of docs/durable-computers.md cut it: P4 of
    docs/explorations/pi-durable.md. Its replicas were never read, no
    restore drill existed, and a restore would have put a `state.db` of
    seconds ago into a `/data` of the last save. The saves themselves now
    carry Hermes' databases whole. The endpoint went after it, #156: no
    image used it.)

    *The design of record is now docs/durable-computers.md (Paul,
    2026-10-05): A+ now, toward E; messengers outside the computer (F).*

    *Status (2026-10-05, #136 and #137).* The hold, the snapshot as a
    cache of the save, and a wake that says what it restored are built.
    A rolled-back or lost `/data` no longer runs a turn again: a turn runs
    only in the life that claimed it on `work` (lesson 2's journal).

    *Status (2026-10-05, step 1 of docs/durable-computers.md, which is the
    newer word here).* "Every few minutes while written" became: `/data`
    is saved when a computer's work ends (its last keepalive closes, 30 s
    settle), every 15 minutes while it stays busy, and at every sleep, an
    action of the pure lifecycle. The hold is a handshake (the guest
    answers `held`), three saves are kept, a wake falls back to the save
    before one that will not restore, and a sleep whose save fails keeps
    its container for at most `computers.unsaved_max_ms` (30 minutes by
    default, Paul's to confirm). Litestream is cut from the image (P4).
    Step 2, the seam:
    `/data/work` (the tools') is saved as a record of its own beside the
    rest of `/data` (the guest's own state), restored together.
19. **Image updates.** The image is pinned per computer. A new default
    image reaches a sleeping computer at its next wake, through the
    image-plus-restore path, since the snapshot is for the old image. The
    first start of a new image at a location pays its pull (24–40 s).
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
    connection: an agent may use every connection its owner has, unless
    its owner narrows it to a list (decision 44). Templates ask before
    sending email, sharing files or accepting invites.

    Status (Paul, 2026-10-04; branch `claude/credentials`,
    docs/computers.md "Connections and operator keys"): Google only for
    now, a row of the deployment's provider catalog (`providers`, one
    typed list that replaced `FRAGMENT_CONNECTIONS` and
    `FRAGMENT_OPERATOR_KEYS`), so another connection is a catalog row and
    its provider enabled in Pipes. Each agent's placeholder is its own
    (`fcx_google_<tag>`, the tag an HMAC of computer, agent and provider
    under a key derived from the host secret) in a standard environment
    variable (`GOOGLE_OAUTH_ACCESS_TOKEN`), so any SDK or CLI works
    unmodified, with no header of ours (`x-fragment-agent` is gone from the
    swap). A guest is given a connection once Pipes says it is connected
    (its connected account, read with no token minted). Approvals are
    Hermes' own: a person who connected something lets their agents use
    it. Every call is counted by agent and month, and the shell's
    Connections page shows it.
23. **Models through AI Gateway**, Unified Billing, with zero data
    retention on and the gateway's request logs off. Three tiers: GLM-5.3
    Flash (cheap), GLM-5.3 (medium), both on Workers AI, and Claude Opus
    5.5 (high). The computer's model intercept adds the gateway credential
    and reads usage for the ledger.

    **The high tier stays off until Cloudflare raises Unified Billing's
    Opus limit** (about 2 calls a minute per edge machine; spike S4).
    Until then GLM-5.3 is the top tier. Paul, 2026-10-02: no BYOK and no
    sharding. Ask Cloudflare with the production account's limits
    request.

    Status, 2026-10-05 (branch `claude/vision-model`; Paul: "yes, vision
    model for computer_use. deepseek flash is apparently pretty good"):
    the model route takes `vision` beside the tiers, the deployment's
    vision model (`vision_model` in its config, `FRAGMENT_VISION_MODEL`;
    one the price book does not price is refused), GLM-5.3 Flash by
    default. Workers AI's catalog marks it "Vision: Yes"; GLM-5.3 reads no
    images. Our Hermes image sends every agent's image calls there,
    whatever its tier (its `computer_use` screenshots, and images people
    attach), metered to the agent's owner as any call (docs/computers.md,
    Models). It is no tier: agents and jobs cannot pick it. DeepSeek
    Flash's vision build (`deepseek-flash`, DeepSeek-V4.1-Flash) is only on
    DeepSeek's own API: Workers AI's DeepSeek-V4-Flash-0731 has no vision,
    Unified Billing offers DeepSeek V4 Pro alone, and AI Gateway's DeepSeek
    provider takes our own key, which this decision declines. Paul may
    revisit that.
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
    they have no agents, and can't create fragments (Paul, 2026-10-03):
    a fragment's hosting bills its owner, and a guest pays for nothing.
    A guest still edits a paid person's fragment shared with them, which
    bills that fragment's owner (decision 26). A $100 seat includes $50
    of credit a month and a computer that sleeps when idle. A $200 seat
    includes an always-on computer (its awake time not metered), SimpleX,
    and $100 of credit. More credit can be bought. Stripe arrives later
    through two hooks: granting credit, and a seat's state.
26. **A fragment's costs bill its owner**, with a monthly cap the owner
    sets per fragment (default $5). Past it, AI steps and agent turns
    stop for everyone but the owner.
27. **At zero credit** agents stop: no turns, no wakes, no AI steps. The
    shell says why. Fragments keep serving and taking writes on a $2
    overdraft, then go read-only until a top-up. Read-only, their cron
    and triggers start no runs either: each run they would have started
    is recorded `blocked`, with the reason, and they start again once the
    owner has credit (Paul, 2026-10-03).
28. **Sign-up is open; creating needs a seat** (decision 49, which
    replaced invite-only sign-up on 2026-10-08).

### Fragments, brains, sites

29. **All of today's fragment model is ported**, core first.
    - Core: operations over the app's SQLite (Loader and Facets), live
      updates, members and invites, files in git and deploys, and
      isolation on fragment.boats.
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
    Status, 2026-10-03 (branch `claude/screenshots`): built, taken by the fragment's own alarm after live moves (off the delivery queue since issue #156), as a visitor without an account sees the page (members-only fragments, chats and agents get none), metered as `browser` time to the owner, shown in the shell's Apps list (docs/api.md, Cards); proven on `wrangler dev`'s local Browser Rendering, Cloudflare's own browsers are the hosted lane's.
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
    - The in-fragment goose agent went too (Paul, 2026-10-07; issue
      #156's head scratcher 6): the `agent/` Worker on the goose fork, a
      fragment's `agent` block, `job.agent`, `fragment agent`, and their
      e2e sections. Agents are fragments a computer runs (decisions 14
      and 15): one runtime, with one set of turn, tool, progress and
      recovery semantics. An app's own model calls are its AI steps: the
      calories template reads what someone ate with a text step its
      channel's trigger runs. goose may come back later, as a
      fragment-native alternative to Hermes. A deployment made before
      keeps an unbound `fragment-agent[-<branch>]` Worker that no deploy
      or teardown touches: Paul's to remove (`wrangler delete --name …`).
    - Nothing on fragment.club migrates. People sign in again with the
      same WorkOS identity (there are no usernames to carry across:
      decision 47).
34. **Infra comes down after cutover**, one irreversible step at a
    time, each confirmed by Paul on the day.
35. **master becomes the Cloudflare line.** fragment.club on celld
    deploys only from the `celld` branch (tag `celld-final`, `160d3a1`,
    pushed 2026-10-02) until cutover.

### Agents acting for people (Paul, 2026-10-02)

36. **Sharing with a person shares with their agents.** Grants name
    people; a person's agents act for them, never above that person's
    role. Example: share a fragment with skyler@example.com as an
    editor, and Skyler's agents can edit it as "Skyler's agent Juniper,
    for skyler@example.com".
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
    - **Your agent can share on your behalf** (Paul, 2026-10-04: "Yes,
      your agent can share on your behalf"). Acting for its own owner,
      and not held below them, an agent shares a fragment its owner owns
      as its owner would: members, invites, visibility, and rotating the
      links. It never deletes a fragment or sets its cap, shares nothing
      acting for anyone else or on what its owner only edits, and each
      thing it shares is recorded as the agent's, for its owner
      (`fragment_core::access::agent_shares`).
37. **Agents can do anything they could do in Finite**, with
    credentials handled better.
    - Services that act as you (Gmail, Calendar, Drive, Notion, Linear,
      Monday, an X account) are your connections through WorkOS Pipes.
    - Paid APIs that only need a key (search such as Perplexity and
      Grok's X search, Google Places, music and image generation) use
      the operator's keys.
    - Both kinds are swapped in at the computer's intercept, and every
      call is metered to your credit at cost plus the margin.

    Status (Paul, 2026-10-04; branch `claude/credentials`): the
    operator's keys are Perplexity, Google Places, xAI and ElevenLabs,
    rows of the provider catalog, each priced per call at its vendor's
    list price (`fragment_core::price::DEFAULT_KEYS`, with sources: $0.005,
    $0.035, $0.12 and $0.15 before the margin; the last two estimate a
    typical call, as the swap counts calls, not posts or minutes), each
    key a store secret Paul sets (docs/secrets.md). Each agent's placeholder
    (`fck_perplexity_<tag>`) is in the variable the vendor's SDK reads
    (`PERPLEXITY_API_KEY`, `GOOGLE_PLACES_API_KEY`, `XAI_API_KEY`,
    `ELEVENLABS_API_KEY`), swapped in a header, a query parameter or basic
    auth as its row says; a person's own keys are a third kind (`own`),
    sealed by their computer and never metered. The managed skills read
    the variables.
38. **Routines are fragment cron.** An agent's routines are its
    fragment's cron triggers, not Hermes' own scheduler, which is turned
    off. A trigger posts to a channel the agent's bridge subscribes to,
    and the generic rule of decision 39 wakes the computer. Agents add
    and change routines with the fragment CLI (a skill).

### The generic pieces that make the rule work (Paul, 2026-10-02)

39. **A subscription wakes a computer, and the guest says when it is
    busy.** A computer subscribes to fragment channels (its bridge does,
    through the API). A record on a subscribed channel wakes a sleeping
    computer, and the bridge catches up from its last sequence number.
    Chats, routines, webhooks and anything else reach agents this way.

    It stays awake while it has:
    - an open port tab;
    - or a keepalive socket that the guest holds to the Computer DO
      itself.

    **A page wakes it early.** When a page opens a fragment the computer
    subscribes to (a chat), or someone starts typing there, the platform
    starts the computer at once. It stops again after 60 s if nothing
    arrives. An unused pre-wake costs about $0.002, and it hides the
    whole wake if it fires 3 s before the message is sent (spike S3b).
    The rule names no runtime; it is presence on a subscribed channel.

    Twenty minutes after neither has been true, it sleeps. Spike S3
    found that traffic from the container, even a socket to another DO,
    doesn't count as activity; only a socket the Computer DO accepts
    does.

    **Adding an agent wakes it too** (Paul, 2026-10-03: wake agents
    aggressively to hide latency, and leave templates no convention to
    remember). When an agent that runs on a computer becomes a member of
    any fragment, the platform itself posts `{kind: "joined", fragment}`
    on the agent fragment's `tasks` and wakes its computer, so the guest
    follows the new fragment before anyone speaks there.
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

### Defaults taken 2026-10-02 (from the cloudflare/agents study)

42. **Only an agent's owner answers its approvals.** In your chats that
    is you. When Skyler's agent works in your fragment, its approvals go
    to Skyler. An approval has an id and an expiry, and the first answer
    wins. While a turn waits on an approval, the bridge drops its busy
    flag, so the computer can sleep instead of billing for hours. An
    answer that arrives after a sleep resumes the turn, or the card says
    it expired. *Amended 2026-10-05 (docs/durable-computers.md, P6 for
    now):* an open card holds the busy flag until it is answered or
    expires, at most its life (an hour). A sleep under it cut the turn,
    and Hermes met the next message with the cut command asked again
    (docs/bridge.md, "A card keeps its computer awake").
43. **Computers reach the internet.** Browsing is the point, so
    `enableInternet` is on. The guest holds no secrets, so traffic that
    bypasses the intercepts (ports other than 80 and 443) carries nothing
    of ours.

### A person's agents (Paul, 2026-10-03)

44. **A person's agents aren't fenced from each other.** Paul: "agents
    are owned by a person, they don't need to be super fenced from each
    other." Assume that, in the end, every agent and computer a person
    owns sees everything that person has on any of their agents or
    computers. Roles, personas and permissions are a UX matter, and a way
    to give agents some specialization; they are not walls between one
    person's agents. So:
    - an agent may use every connection its owner has, by default; its
      owner may narrow one agent to a list (`connections: null` is every
      one, a list only those: docs/computers.md);
    - limits between one person's agents are specialization, never a
      security boundary. Walls stand between people (decision 36), and
      an agent is held below its owner only by its owner's choice.

### People: npubs, emails, no usernames (Paul, 2026-10-08)

This is BANKS as fragment builds it. finite.computer's version (Linear
FIN-11) was canceled with the rest of V3 on 2026-10-05. WorkOS proves
an email; the registry ties emails, npubs and keys together; the rest
of the platform sees npubs. Paul: the platform "FEELS to the user like
a normal email login thing, and shared fragments feel like sharing
google docs", while "agents can do raw pubkey stuff without the auth
runaround".

45. **A person is an npub; others find them by email** (Paul: "email
    over a stable id which is an npub"). At a person's first sign-in
    the registry makes them a key, and its npub is their identity for
    good: every grant, ledger and list names it, and the `id:` form
    goes. A key can be retired; its npub still names the identity. An
    agent's identity is its fragment's npub.
    - A person's email is the verified email of their sign-in (WorkOS's
      `email_verified`; a sign-in without one is refused). An email
      belongs to at most one person; `/auth/link` adds a second sign-in
      and its email.
    - The share sheet, member lists and the shell show people by email.
      A changed email follows at the next sign-in, and no grant moves,
      because grants name the npub.
46. **The registry keeps a person's key**, sealed under the host secret
    as fragment and agent keys are (`cell/src/keys.rs`). It is never
    kept in WorkOS, which holds only the login (and which a self-hosted
    deployment does not have).
    - Inside the platform a browser still acts through its session
      (Paul: "inside platform it's fine to just do normie sessions"),
      and a fragment's origin gets its own cookie through a single-use
      redemption, as finite-sites does (its ADR 0029).
    - The platform signs with a person's key only to cross into a
      service outside it, one named purpose at a time, as finite-sites'
      hosted signer signs only `authorizeViewerSession`. It never signs
      for a page's script, and it is never a general signer.
    - Nothing uses the key yet. It is there so a person's npub can do
      cryptographic things when something needs them; brains stay
      unencrypted (decision 30).
    - A person's CLIs keep their own keys, paired as today (`fragment
      login`).
47. **No usernames.** A fragment's name is one DNS label: the label its
    maker gives it and a short random suffix the platform adds at
    create (`todo-k3x9`). It is unique in the fleet and fixed for the
    fragment's life, and served at `todo-k3x9.fragment.boats`
    (`todo-k3x9--<branch>.<zone>` on a branch copy). The suffix makes
    names unique without a namespace to claim: nobody squats a label,
    and nothing in a URL ties one person's fragments together. Where
    the CLI or the API takes a fragment, a bare label means the
    caller's own fragment with that label; two such are an error that
    names both. R16 keeps the one-DNS-label rule.
48. **Sharing is by email** (Paul: shared fragments "feel like sharing
    google docs"). The share sheet and the CLI take an email and a
    role.
    - A person who holds that verified email is a member at once.
    - Otherwise the invite waits on the email, and the platform mails a
      link to the fragment. Whoever signs in with that verified email
      becomes a member as the invite said, from then on by their npub.
      Signing in never makes a share; it meets one addressed to its
      email.
    - Everyone with access sees the members' emails.
    - Agents have no email: one is picked from the sharer's own agents,
      or named by its npub.
    - Anything shaped like an email is an email: the CLI's NIP-05
      lookup goes.
    - The invites a person can have mailed are capped per day.
49. **Sign-up is open; creating needs a seat.** This replaced
    invite-only sign-up. Anyone signs in through WorkOS's own sign-in
    methods (fragment mails no login links) and is a guest (decision
    25): they see and edit what is shared with them, and create
    nothing. Creating needs a paid user, a seat (Paul: "you have to be
    a paid user (stripe) to create stuff"). A seat needs an invite, an
    operator's grant until Stripe is built (Paul: "still need invite to
    become a full paid user").
50. **Later: paying with a key** (Paul: "a future plan", "useful as a
    design constraint right now"; Bitcoin Lightning first). A CLI makes
    its own key and pays over HTTP 402 (x402's shape). That gives it an
    identity with no sign-in and no email. It may pair a WorkOS sign-in
    later, and its key never leaves the CLI. None of this is built. What
    it constrains now:
    - an identity is an npub whether or not a sign-in names it, and an
      email is how people find each other, never what an identity is;
    - nothing the platform does for a person may need the registry to
      hold their secret: a person whose key is in their CLI signs for
      themselves;
    - a payment is credit granted on a ledger, as Stripe's will be
      (docs/ledger.md).

    What such an identity may create is decided when it is built.

### Carried from the ROADMAP (Paul, 2026-09-23 to 09-25)

`docs/ROADMAP.md` (2026-09-23, before this plan) is gone; it is in git
history and at the tag `celld-final`. These of its decisions still hold
and are cited by their old numbers, as R-labels. The others were
superseded by the decisions above or went at the cut.

- **R4. Sharing is the platform's.** The share sheet and accepting an
  invite are platform pages, which no fragment's code can drive
  (docs/api.md, Sharing). A fragment's page is code its author or an
  agent rewrites, so it never grants anything. A direct URL signs you
  in: at once on your own fragments and those shared with you. On anyone
  else's, the platform asks once ("Continue to X as you?") before X
  learns who you are, and signing out of X there makes it ask again
  (docs/api.md, Asking first; Opening a fragment by its URL). The share
  header over a fragment is not built.
- **R15. Identity follows finite.computer's BANKS model (FIN-11).**
  People, agents and fragments have stable identities, and grants name
  identities. Each identity holds one or more public keys, each added
  with proof of possession and revoked on its own. Every agent has one
  human owner, who can read what the agent can read. Keys stay with
  their callers: browsers hold sessions, the CLI and agents sign.
  Lookups are live and fail visibly. Since 2026-10-08 an identity is
  an npub, and the registry keeps a person's own key (decisions 45 and
  46). FIN-11 itself was canceled on 2026-10-05. What Core will own is a stand-in
  in V3's shape (docs/finite-integration.md). `link` means anyone with
  the unguessable link is a viewer; `public` means anyone.
- **R16. One DNS label per fragment.** A fragment is served at one DNS
  label under the suffix, so the suffix's one wildcard certificate
  covers every fragment (a certificate per host ran into Let's
  Encrypt's limit of about 50 new names a week). Sessions are `__Host-`
  cookies. The label was `<label>--<username>` until usernames went on
  2026-10-08 (decision 47).
- **R17. An agent acts for whoever asked, capped.** Each call in a turn
  acts with the lower of two roles: the asker's, and a cap. The cap is
  the agent's own role, or its owner's on a fragment the owner belongs
  to, and never above `editor` (docs/api.md, Principals and access;
  decision 36 adds its limits). So a guest who drives someone's agent
  reaches only what the guest could.
- **R18. Channels a fragment declares postable.** A declared channel may
  name a `post` role. The platform then appends a member's record
  itself, with the role check, a rate limit, a size cap, and dedup by
  principal and id. A chat therefore needs no app code. This is the one
  exception to "clients never append" (docs/MODEL.md). `"signedIn":
  true` refuses an anonymous poster.

## Lessons from cloudflare/agents

Read 2026-10-02 at commit `2f3176b`: `agents` 0.26.0, especially
`packages/agents/src/harness/pi`; the full notes are in the session's
`research/RESULTS.md`. Cloudflare runs pi inside the DO, not in a
container. That is a choice our rule rules out, but its patterns carry
over. Each lesson names the smallest change it makes here:

1. **Split work by who has authority over it.**
   - The chat channel owns what was said, and its ids.
   - Hermes owns the model context and the in-flight turn.
   - The Computer DO owns the computer's lifecycle.
   - The ledger owns usage.

   The bridge translates between them and owns nothing.
2. **A turn is admitted once, by its operation id.** The bridge uses a
   record's `(channel, seq)` as Hermes' message id, so catching up after
   a deploy or a wake never starts a second turn. Routine records carry
   `(trigger, scheduled minute)`, because cron has no overlap guard.
3. **One job queue drives each DO's one alarm.** It is a pure state
   machine in `crates/core`, shared by the Computer and Fragment DOs:
   - the newest push wins, which settles a wake racing a sleep;
   - a deadman alarm catches a hung job;
   - delivery is at least once.
4. **A failure is classified before it is retried.** A platform
   transient, such as a code-update reset or a lost network connection,
   is deferred to a fresh invocation and never retried in the dying
   isolate. A memory-limit reset gets three strikes and is then sealed.
   A computer whose wake keeps failing shows "won't wake" instead of
   paying for container starts. A throwing alarm is retried forever.
5. **Deploys are routine and noisy.** In-flight work gets about 30 s;
   sockets drop several times over 11–22 s; code-update evictions happen
   once or twice a day anyway. The bridge reconnects with jitter, and
   the hosted lane runs several real deploys mid-turn, then asserts one
   reply, no duplicate turn and no duplicate meter row.
6. **A container can outlive a deploy.** A new isolate may find
   `ctx.container.running` true with no monitor attached. The Computer
   DO's constructor attaches `monitor()` again, sets the timeout again,
   and registers its intercepts again (idempotently; S3 saw them
   survive). It never destroys the container, since `/data` may be
   unsaved. After any `destroy()`, it polls `running` to false before
   `start()`.
7. **The model intercept is bounded.** It allows only the provider's
   endpoints and methods, the tier's model, and a capped `max_tokens`.
   It strips auth headers, passes the native wire format through, and
   persists only the final usage, never the request or its stream.
8. **The two gateway paths differ.**
   - `env.AI.run` with a catalog model streams the provider's native SSE
     and gives a run id that can resume a dropped stream.
   - `env.AI.gateway(id).run` gives a log id, caching and ZDR.

   GLM calls send `x-session-affinity` set to the agent id, for
   prefix-cache hits. Each meter row is keyed on the log id or run id,
   which is the ledger's replay key. Read the id from the response
   header, not the binding property, which concurrent calls overwrite.
9. **Native snapshots exist in the runtime types:** `snapshotDirectory`,
   `snapshotContainer`, and `start({directorySnapshots,
   containerSnapshot})`. The wake investigation measures them against
   our exec-driven restore (decision 18).
10. **Staying awake means an alarm, and facets have none.** This agrees
    with S1's lock on `setAlarm` and S3's keepalive rule.
11. **A late page gets a snapshot plus the in-flight partial, and the
    final replaces it by id.** A chat's final record carries its draft's
    id, and push notifications fire only on finals.
12. **The person's index is a fenced projection.** Each fragment pushes
    its snapshot after a commit; the Principal applies it with a
    constrained update, never an upsert, and repairs by pulling. The
    shell's search (decision 9) is FTS5 in the Principal.
13. **A layered test ladder:**
    - per PR: vitest-pool-workers, crashing with
      `abortAllDurableObjects` and `runDurableObjectAlarm`;
    - nightly: SIGKILL a `wrangler dev --persist-to`;
    - containers under `wrangler dev --local` with Docker;
    - fixtures that assert meaning, not bytes, with a scripted model that
      is a pure function of the transcript.
14. **Gotchas:**
    - `enable_abortsignal_rpc` when an AbortSignal crosses RPC to a
      container's DO;
    - `run_worker_first` so the shell's WebSocket upgrades reach the
      Worker past static assets;
    - `ctx.waitUntil` is cut about 30 s after the response;
    - never wrap a turn in a Workflow;
    - log one JSON line per event, because `wrangler tail` drops lines.

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
   fragment API → the computer's identity
 R2: blobs, site copies, screenshots, backups
 code.storage: every fragment's git (apps, chats, agents, brains, skills)
 Queues: deliveries, push, ledger batches   Workflows: jobs and cron runs
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

   Status, 2026-10-02:
   - Done (see "Spike results"): Loader and Facets (S1), Hermes on
     Containers (S3) and its wake (S3b), Pipes (S5), AI Gateway (S4).
   - Folded into phase 2's first step: the cell on real Workers, and a
     per-branch deployment on `finite.place`.
   - Phase 0 is complete.
1. **Freeze and cut. Done 2026-10-02.**
   - `celld-final` was tagged and the `celld` branch pushed.
   - The product half of decision 33 went: about 64k lines.
   - The fleet's deploy path went with it: `fleets/`, `crates/node`,
     `xtask deploy`, `fleet` and `e2e --fleet`. So the cut can't reach
     fragment.club.
   - The substrate stays until phase 2 replaces it: the celld fork,
     native `KEYS` and the devstack.

   Evidence: `cargo xtask check` green, and the full remaining
   `cargo xtask e2e` at 1057 passed and 0 failed across 40 sections (1163
   before; the difference is the deleted lanes).

   Kept for later phases:
   - **The frame-session redeem half** (the partitioned cookie,
     `embedder`, `frame-ancestors`). Nothing mints a frame session now
     (a ledger entry). Phase 5 adds a platform-origin mint and a frames
     lane, or deletes it.
   - **The channel drafts API** for decision 21's bridge. It lost its
     e2e (a ledger entry), and phase 4's exit covers it.
   - `/` and `/settings` as stand-ins for the shell until phase 5.
2. **The cell on Cloudflare.** `KEYS` in the cell; R2; wrangler; the
   dev stack on `wrangler dev`; the e2e on workerd plus a hosted lane
   on a preview. The celld fork and native `KEYS` are deleted here. The
   wrangler migrations restart from scratch, since nothing migrates:
   celld refused `deleted_classes`, so the Computer and Hermes tags are
   still listed. CI got the e2e back: the celld e2e on macOS left CI on
   2026-10-03, leaving only `check`, so phase 2 adds the workerd e2e on
   Linux and the hosted lane on a branch preview. Exit: core fragment
   sections green locally, in CI, and on a preview.

   Status, 2026-10-03 (futurepaul/fragment#115, merged):
   - Built: the cell on workerd (wrangler 4.145.0, pinned); keys in the
     cell (`crates/core/src/seal.rs`: AES-256-GCM, sealed per Durable
     Object); blobs in R2; the dev stack and the e2e on `wrangler dev`;
     `cargo xtask deploy` and `teardown` from a deployment's config file
     (`deploy/example.jsonc`; a branch at `<b>.<zone>`); the e2e job on
     Linux in CI. The celld fork and native `KEYS` are gone, and the
     migrations restart at `v1`.
   - Evidence: the full e2e on workerd, locally (1076 passed, 0 failed
     with phase 3) and in CI on Linux (green on `8618a79`, after two
     fixes to the retention check's wait on the slower runner).
   - Waiting on Paul: the hosted lane on a preview needs the
     `*.finite.place` wildcard record and a choice of sign-in for
     previews (escalations).
3. **The ledger.** Meters, plans, guests, caps, the overdraft and
   read-only, and operator commands. Exit: metering matches the price
   book, and caps and zero behave; valid, invalid and replay tests for
   each mutation.

   Status, 2026-10-03 (branch `claude/phase-3-ledger`; docs/ledger.md):
   - Built: the `Ledger` Durable Object on the pure core; the model
     route (`cheap` and `medium` on Workers AI through the gateway,
     `high` refused) for the agents' Worker and, from phase 4, the
     computer's intercept (`models::complete`); AI steps on it (bugs 2
     and 3 fixed; since 2026-10-03 images are FLUX.1 [schnell] on Workers
     AI, metered in neurons, bugs 1 and 5 fixed for them, and video steps
     are off until they run on Cloudflare: OpenRouter is gone); the gates (at
     zero, agents and AI stop; past the overdraft, a fragment refuses
     writes); each fragment's meters (requests, dynamic workers,
     storage) through the `fragment-ledger` queue; `fragment ledger` and
     `fragment cap`. The OpenRouter-key ledger and `budget.rs` went (a
     hard cut).
   - Evidence: the e2e's `ledger` section on workerd, with the Workers
     AI fake behind the model route (a lower rung). The binding itself,
     the gateway's metadata and its logs-off rows are the hosted lane's.
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
     with a stub image whose bridge speaks the fragment API;
   - drafts: the bridge streams through the channel drafts API, and a
     page sees the draft live and then the final record.

   Status, 2026-10-03 (branch `claude/phases-3-4`; docs/computers.md):
   - Built: the Computer DO (its lifecycle a pure state machine; wakes
     on a subscription, a page, a port and its owner; adoption after a
     platform crash; `/data` saved and restored, snapshots by image;
     image pins); ports on the computer's own origin behind one-time
     tickets; the intercepts (the API signed as the agent, models through
     the platform's model route, storage over R2, and the swap:
     connections through WorkOS Pipes per agent, operator keys priced and
     metered); wakes gated on the owner's credit and awake time metered;
     delegation (decision 36: people-only shares, agents held below their
     owner); the images (`images/`: the bridge, the stub, our Hermes).
   - Evidence, all on workerd under `wrangler dev` with Docker, the
     vendors faked at their boundaries (a lower rung):
     - the `computers` section on the stub image: the platform holds no
       Hermes code;
     - the `delegation` section;
     - the `hermes` section on our Hermes image, the exit list above
       in full (`cargo xtask e2e --only hermes`: it builds a 3.8 GB
       image, so it runs by name and not in CI). A wake follows its
       chats in 2.5 s and answers a first message in about 5 s.
   - Not yet: any of it on Containers. That is the hosted lane's, after
     phase 2's preview.
5. **The shell and the blessed templates.** The shell (onboarding,
   sidebar, tabs, profile, search, connections) and the chat and agent
   templates on decision 40. The shell's tabs sign in to their fragments
   through a platform-origin frame-session mint, with a frames lane; the
   shell replaces `/` and `/settings`. Exit: the browser lane at desktop
   and phone sizes, on a preview.

   Status, 2026-10-03 (futurepaul/fragment#117; Skyler's design handoff
   of 2026-10-02 for every part it draws):
   - Built:
     - **The shell** at `/` and `/settings` (cell/shell/; the server's
       pages went):
       - First run: a username, then "Creating your agent…". The person's
         default agent, in charge, is made with its computer and its chat,
         and the chat opens once the agent follows it (Paul, 2026-10-03:
         no job asked, and no wait in the chat).
       - A sidebar of agents, group chats and apps; per-person archive.
       - Search over titles and messages (FTS5 in the Principal).
       - Settings: the account, credit, computer, agents, connections,
         the CLI, and the wallpaper's credit.
       - An update prompt when the computer's image is behind.
       - It calls the API with the platform session (docs/api.md, The
         shell).
     - **The frame mint** (`/auth/frame`). The shell's tabs, and a
       computer's ports, sign in on their own origins with partitioned
       cookies.
     - **The blessed `agent` and `chat` templates** (decision 40),
       served from the release, their app code included.
       - The chat, on Skyler's design: drafts, steps as cards, approvals,
         Stop, @mentions, and attachments both ways.
       - Voice memos: an audio attachment the agent transcribes (decision
         9, as Paul changed it).
       - Push on an agent's final reply.
     - **Paul's 2026-10-03 rules:**
       - guests make no fragments;
       - past the overdraft, triggers start no runs;
       - the platform posts `joined` and wakes the computer;
       - decision 44.
     - A chat's files are kept while their records are.
     - Computers start at the size their price names, decision 13's
       2 vCPU and 6 GiB.
     - `cargo xtask deploy` makes the proxied DNS record its routes need.
   - Evidence:
     - local, on workerd: `cargo xtask check` and the e2e (shell, shell-ui
       in the browser at desktop and phone sizes, chat, frames, computers,
       push, signin);
     - the preview **p5.finite.place**: real WorkOS staging, code.storage,
       Workers AI through the `fragment-dev` gateway, and a Hermes computer
       on Containers. Paul signed in, made agents, and chatted; messages
       queued, one turn at a time.
   - Found on the preview:
     - a computer's first start pulls the 3.8 GB image (about 29 s), now
       hidden behind setup;
     - an agent added to an awake Hermes computer had no profile (a 401):
       hermes-boot now follows its computer's agents while it runs and has
       Hermes take a new profile live (about 2 to 3 s from assign to its
       first answer, nothing restarted; docs/computers.md).
   - Not yet: the hosted e2e lane on the preview; files in search.
6. **Brains, skills, sites.** The brain and skills templates, Hermes'
   fragment skills, screenshots. Exit: from a chat, an agent builds,
   publishes and shares an app, ingests into a brain and searches it;
   the skills list matches the skills fragment.

   Status, 2026-10-03 (futurepaul/fragment#118, stacked on #117):
   - Built:
     - **The brain** (decision 30): a blessed `brain` template on the
       notes vault UI.
       - Finite Brain's layout and wiki conventions.
       - FTS5 sections ranked by BM25, kept current by a file trigger,
         behind a `search` operation.
       - Assets are blobs. It is offered in the shell's catalog.
     - **The skills** (decision 17): a blessed `skills` template of the
       managed set, finite-skills ported.
       - The Finite-specific skills are rewritten: apps, git, brain,
         Google through the swap, images on FLUX.
       - hermes-boot installs the set read-only into every profile, beside
         each agent's own, its own winning.
       - The shell's settings list them.
     - **The `fragment` CLI in the Hermes image**, acting as the agent
       with no key in it.
       - `fragment create --template`.
       - `fragment write`: one file through the platform.
       - `fragment deploy` without `--dir`: the platform moves live.
     - **Cards** (decision 31): each move of live is shot with Browser
       Rendering for the app's card in the shell, metered to the owner.
     - An app's redirect now reaches the browser (both hops into a
       fragment had followed it).
   - Evidence, local on workerd with the vendors faked at their
     boundaries:
     - **The exit** (the real-Hermes lane, 40 passed, 0 failed): from its
       chat, the agent, acting for its owner,
       - makes a todo app (its owner's), writes a page, and deploys it;
       - gets its share link, which an anonymous visitor opens;
       - makes a brain, ingests a source, and finds it by searching.
     - **The skills list** matches the skills fragment: the shell-ui and
       templates sections.
     - **Sections** templates, brain, notes, deploy, site, shell,
       shell-ui, ledger, computers: 427 passed, 0 failed.
   - The agent now shares as its owner would (decision 36, Paul,
     2026-10-04): the exit's agent also makes the app public, which an
     anonymous visitor opens, and adds a person as a viewer, who sees it
     in their list (the real-Hermes lane, 42 passed, 0 failed). The
     delegation section shares over the API with a signed agent: each
     action, its record, and each refusal.
   - Not yet: the same on a preview (phase 7's hosted lane).
7. **The rest of fragments.** Jobs, cron, deliveries, webhooks, push,
   blobs, secrets, AI steps. Exit: their e2e sections green, plus a test
   for the in-fragment agent's reply-operation answer path (the agents
   section's, since 2026-10-03).

   Status, 2026-10-03 (futurepaul/fragment#118):
   - Built:
     - **The in-fragment agent's answer path** has its test, in the agents
       section: valid, invalid, replay, and a crash after the answer.
       - It found a bug: any 404 on posting an answer dropped the chat's
         listen. Now only a refusal or a fragment that is gone does.
     - **The hosted lane**: `cargo xtask e2e --hosted --config <file>
       --branch <b>`, with `--dry-run` for its plan, `--rehearse` for it
       on the local node, and `--sweep`.
       - It runs on a branch preview, choosing sections by what each
         declares it needs.
       - Its people sign in through a branch-only, secret-gated,
         e2e-scoped lever (docs/secrets.md); a deployment of its own
         refuses the secret.
       - Its paid calls are lent from a capped budget.
   - Evidence:
     - Local: the full e2e, 1475 passed, 0 failed, 2 skipped (both
       hosted-only).
     - Hosted, on **e2e.finite.place**, on real vendors: WorkOS staging,
       code.storage, Workers AI through `fragment-dev`, Browser Rendering,
       and our Hermes image on Containers.
       - First run: 354 passed, 2 failed, 57 skipped, for 11 model calls,
         $0.16. Both failures were the lane's timing on a real
         deployment, fixed in it.
       - The two sections again: 79 passed, 0 failed.
       - Then a sweep removed the run's 25 fragments.
   - Not yet hosted: jobs, triggers, push, AI steps, blobs, sync and
     appfiles still need local fakes or servers, so they are green locally
     and skipped on a preview (the debt ledger).
8. **Self-deploy.** An agent following `SETUP.md` deploys into a fresh
   account from a clean config, and the hosted e2e passes there.
9. **SimpleX** on $200 seats, always on.
10. **Cutover.** Zones to Cloudflare, the production account and its
    raised Workers AI and gateway limits, the production deploy, seats
    for the people invited; then the old infra comes down (decision 34).

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

**S3, computers on Containers and Hermes on them (2026-10-02,
deployed; evidence in `spikes/s3-computer/RESULTS.md`; about $0.15).**
All ten items work.

The generic computer:
- A cold start of a small image is 2–3 s at p50. The first start of a
  new image at a location pays the pull: 10–11 s.
- Nothing on disk survives a stop, and neither do intercepts, so they
  are registered again at every start. A hostname costs 2 of the 128
  entries, or 4 for HTTP and HTTPS together.
- Placeholder swaps work over HTTP and HTTPS. HTTPS needs the image to
  wait for Cloudflare's CA, which appears shortly after boot.
- SSE streams through. A WebSocket from the guest to a DO works for
  both `ws://` and `wss://` (68–318 ms to connect, about 5 ms per round
  trip), and drops at a deploy (the guest reconnects in about 1 s).
- Ports work over HTTP, WebSockets and raw TCP from the DO, at the
  client's line rate.
- `DirectoryBackup` saved 52 MB in 3.3 s and restored it in 1.3 s.
- Litestream works through a 170-line S3 gateway over the R2 binding, so
  no R2 token is needed (a restore took 4.6 s).
- A running container keeps its image across a deploy; the next start
  takes the new one.

Hermes:
- Its `-desktop` image is 1.37 GB compressed.
- Start to ready takes 26–32 s on a fresh disk, 21 s for a wake with
  restore, and 42–82 s for a first start or a new image version. Most
  of that is Hermes' own boot.
- A turn through the model intercept to GLM-5.3 Flash works: about 27K
  input tokens and $0.004 for a simple turn with 23 tools. Flash takes a
  1M-token context.
- Multiplexed profiles work. `API_SERVER_KEY` needs at least 16
  characters, and Hermes' own cron must be turned off (decision 38).
- Memory survived a sleep and a wake.
- Hermes' screen is an Xvnc on a Unix socket, with no viewer of its own.
  So the image ships its own viewer page and TCP bridge, and keeps Take
  over / Give back in that page.

What it changes:
- decision 18 (sleep is driven by the DO; the wake contract);
- decision 39 (staying awake);
- the image's boot script: wait for the CA, write config files instead
  of running CLI calls, wait for the restore marker, list each profile's
  `state.db` for Litestream, and keep the screen's lease;
- a risk: cold wakes are slow.

**S3b, a Hermes computer's wake (2026-10-02; evidence in
`spikes/s3b-wake/RESULTS.md`; about $0.4).** Ready went from 13.1 s to
2.95 s (p50, 2 vCPU / 6 GiB), and the first token from 18.8 s to 5.9 s.

The baseline waterfall:

| Phase | Time |
|---|---|
| Container start | 1.6 s |
| Restore | 0.4 s |
| Setup scripts | 1.1 s |
| Profile reconcile | 1.3 s |
| `update-ca-certificates` | 1.0 s |
| Hermes' gateway importing without bytecode | 6.6 s |

The cuts, cumulative, in the order measured:

| Cut | Ready after it |
|---|---|
| Bytecode compiled into the image | 7.0 s |
| Gateway imports preloaded during the restore | 6.2 s |
| Unused messaging-platform plugins disabled | 4.5 s |
| Gateway's own skills syncs skipped, its files read ahead | 3.8 s |
| A single-layer image | 3.6 s |
| A per-computer snapshot in place of the restore | 2.95 s |

What didn't help:
- 4 vCPU gives no gain, because the boot is one Python thread.
- A golden snapshot shared by every computer saves nothing once
  bytecode is in the image.
- Native directory snapshots (`snapshotDirectory`) are only behind the
  `experimental` flag in production.
- The pre-distributed `cloudflare/debian-trixie` base wouldn't start on
  this account, and would save only about 1 s of a first pull.

What goes where:
- **The Hermes image's boot list** (phase 4): bytecode with
  `unchecked-hash`, `plugins.disabled` for platforms and dashboard auth,
  a gateway preloaded before the restore gate, the CA appended rather
  than regenerated, a readahead list, a single-layer image, and the
  dashboard and screen lazy. (Setup gated on stamps went on 2026-10-06,
  issue 156: the image carries no bundled skills, so nothing syncs them,
  and the config migration runs at every boot, a no-op of 0.07 s on a
  desktop CPU.)
- **Upstream Hermes:** load only the configured platforms, key the skills
  sync on the image revision, use lazy imports, and ship bytecode.
- **The generic Computer DO:** snapshot-backed wakes (decision 18) and a
  pre-wake on presence (decision 39).

Snapshot facts:
- They work only with the `durable_object` scheduling policy.
- They hold files only, are tied to the image version, are immutable,
  are capped at 20 GB, and are kept 30 days.
- There is no deletion API and no published price.
- They are stored as tags in the image's registry repository; whether
  they count against the 50 GB limit is unverified.

**S4, AI Gateway (2026-10-02, both halves; evidence in
`spikes/s4-gateway/RESULTS.md`; about $0.62 of Workers AI).** Wrangler's
OAuth login has no AI Gateway scope, so the gateway, Unified Billing,
Opus, ZDR and spend limits wait on an API token with AI Gateway Edit.
Self-deploy's `SETUP.md` must ask for that token too.

What ran, against Workers AI directly:
- Both GLM tiers do OpenAI-style tool calls over streaming.
- GLM-5.3 takes a 1M-token context; a 308K-token prompt answered, so the
  64K listing was wrong.
- Cost is exact from usage: neurons × $0.000011 matched token ×
  catalog price on all 18 calls. Prices come from the catalog API
  (`/ai/models/search`).

Intercept facts:
- Call through the Worker's `env.AI` binding with a named gateway
  (`{gateway: {id, metadata: {user_id, agent_id}, collectLog: false}}`),
  so no token is held. The id `default` silently creates a logging
  gateway, so always name ours.
- Usage arrives on every chunk; meter only the last, cumulative one.
- Clamp GLM's `reasoning_effort`, which otherwise defaults to `max`.
- ZDR covers only Unified Billing's third-party providers (Opus), and the
  gateway's logs are a separate setting.
- Gateway spend limits are eventually consistent, so they are only a
  backstop. Our ledger stays authoritative.

Phase two (2026-10-02, with an AI Gateway token, on gateway `spike-s4`
with ZDR on and logs off; about $0.27 of credits):
- **All three tiers work through the gateway** with tool calls and
  streaming usage.
  - GLM goes through `env.AI.run` with the gateway option.
  - Opus goes in Anthropic's native Messages shape through `env.AI.run`,
    with `system` flattened to a string and top-level `cache_control`.
    Its model id there is `anthropic/claude-opus-5.5`.
  - Hermes uses its Anthropic provider for the high tier, so the
    intercept passes Messages through rather than translating from
    OpenAI.
  - The compat path also works, but it ignores caching. Native
    non-streaming responses drop the cache fields, so Opus is metered
    from the stream.
- **Unified Billing's limit on third-party models is per edge machine.**
  From one DO, Opus allows a burst of 3 calls, then about 2 a minute;
  the 4th answers 429 (code 2018, "Wholesale rate limit exceeded"). A
  computer's model calls all leave from its DO's machine. **So the high
  tier is unusable** until Cloudflare raises this, or Opus goes BYOK, or
  calls are sharded across gateways. GLM is unaffected.
- **"Logs off" still keeps one row per call**: model, tokens, cost, our
  `user_id` and `agent_id`, and timings. Prompts and responses are not
  kept. So metadata is always opaque ids.
- **Per-user spend rules work.** A rule partitioned on `user_id` blocks
  that user on every model with a 429 (code 2045) on the next request;
  other users still pass. A new rule takes about 40 s to apply.
- **Dynamic-route fallback to a cheaper model fails** (400, code 5006),
  so the downgrade happens in the intercept.
- **Cost:** the gateway's log rows carry the cost even with logs off,
  matching tokens × catalog price exactly. The credit balance is
  readable (`GET /ai-gateway/billing/credit-balance`). The ledger's cost
  basis is list price × 1.05, because credits carry a 5% fee, and the
  charge is that times 1.5.

**S5, WorkOS Pipes for Google (2026-10-02, staging, with Paul).** It
works end to end.

The setup:
- Staging's Pipes offers **no shared credentials** for Google or Gmail;
  both need a client ID and secret. We reuse Finite's Google OAuth
  client (project `714116971392`), with WorkOS' redirect URI added.
- The provider is WorkOS' general `google`, user-owned, with Finite's 11
  scopes: `drive`, `documents`, `spreadsheets`, `gmail.readonly`,
  `gmail.send`, `gmail.modify`, `calendar`, `contacts.readonly`,
  `openid`, `userinfo.email`, `userinfo.profile`.

The calls:
- `POST /data-integrations/google/authorize {user_id}` returns
  `{url, state}`, a plain redirect with no secret in it. Google's
  consent asks for offline access.
- `POST /data-integrations/google/token {user_id}` returns
  `{active, access_token: {access_token, expires_at, scopes,
  missing_scopes}}`. The token lasts about an hour, and WorkOS refreshes
  it on the next call.

With that token, read-only calls to Gmail, Calendar, Drive, Docs and
Sheets all answered.

Contacts was refused at first because the People API was disabled in
that project, so finite.computer's own `contacts.readonly` never worked
there either. Paul enabled it, and Contacts then answered 200.

The placeholder swap at the computer's intercept is tested in phase 4,
against Google's own client libraries.

## Bugs the port must fix

Found 2026-10-02 building `avatar-lab` on fragment.club (`celld-final`).
Each one needs a test in the phase that ports its feature.

1. **AI images between 256 KiB and 1 MiB can't be saved.** `store_media`
   (`cell/src/ai.rs`) commits through `commit_files`, which caps a write
   at 256 KiB, while blobs start at 1 MiB. The error was "the files
   written is 347234 bytes; the limit is 262144". Phase 7: a generated
   file is a blob from the first byte, or the two limits meet. *Fixed
   2026-10-03:* `store_media` commits as the platform, past an app's
   write limit, so the limits meet (the e2e's `ai` section).
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
   *Fixed for images 2026-10-03:* the model draws JPEGs, and
   `job.ai.image` refuses a path that does not end in `.jpg` or `.jpeg`.
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
- **Opus through Unified Billing is limited to about 2 calls a minute per
  edge machine**, so about 2 a minute per computer (spike S4). Paul chose
  to wait for Cloudflare to raise it (decision 23): the high tier is off
  until then. BYOK and sharding were declined.
- **Workers AI rate limits.** GLM-5.3 and Flash are "paid models":
  20 requests a minute per model per account on standard billing, and 50
  with prepaid credits through a gateway. At about 4 calls a Hermes
  turn, that is roughly 12 turns a minute for the whole account.
  Escalation: ask Cloudflare for an increase through the Custom
  Requirements Form (https://forms.gle/axnnpGDb6xrmR31T6), together with
  raising the gateway's own cap of 200 requests a minute. Paul,
  2026-10-02: don't worry about it while building; request it in the
  production account during cutover (phase 10).
- **Cold wakes**, after spike S3b's cuts:
  - Hermes is ready in about 3 s, and the first token arrives in about
    6 s (4 s with a first-turn warm-up that needs a hook upstream);
  - with a pre-wake, about 1 s is perceived;
  - a new image version's first start at a location still takes 24–40 s.

  Hermes' Python start stays about 2 s on this CPU, and Cloudflare has
  no memory snapshots. The chat shows the computer waking from t = 0.
- **Unproven paths:**
  - a bridge's long-lived connection to a fragment through the egress
    swap;
  - restore time for a large `/data`.
- **Isolation between agents.** Agents on one computer share it, so
  per-agent connection limits are not a security boundary. Decision 44
  takes that as the design: one person's agents are not fenced from
  each other, and a per-agent list is specialization.
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
