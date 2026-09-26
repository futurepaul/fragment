# Computers: a fragment's own machine (design, the next step)

Status: design only, 2026-09-26 (ROADMAP phase E's second half). Nothing
here is built. What is built (decision 21): a computer is an identity
(docs/api.md, Identities). A person pairs a
machine with `fragment login --computer <name>`, and from then on it signs
as that computer, never as them. It works only in fragments where it is a
member and in the fragments it makes. Those are its owner's, on its
owner's budget. `fragment computers rm` revokes its keys and removes its
memberships.

The next step: a fragment declares a computer, and the platform provisions
a Sprite for it (ROADMAP decision 5).

## The shape

1. **Declared.** `fragment.json` asks for one:
   `"computer": {"services": ["desktop"], "setup": "computer/setup.sh"}`.
   `services` names platform-provided Sprite services. `desktop` is Xvfb,
   Chromium, and a control endpoint for screenshots, clicks, and typing,
   ported from finite-next's `computer/*`. `setup` names a script in the
   repo that runs once on the Sprite. Declaring works like the `frame`
   capability (docs/platform.md): the owner allows it once in the share
   sheet, because a computer costs money and an agent can rewrite a
   manifest.
2. **Provisioned.** On a deploy whose live manifest declares a computer
   the owner allowed, the fragment asks its `Computer` cell (one per
   Sprite, decision 5) to ensure one. The cell creates the Sprite
   through `KEYS`, which holds the Sprites token. It installs the
   `fragment` CLI, a Linux release built with `computer`. Then it runs
   `fragment login --computer <label> --no-wait --json` there and
   registers the returned key and proof directly with the registry, as
   the owner (a `PairComputer` by `By::Identity`, as
   `agents::own_agent` registers an agent). The key is made on the Sprite
   and never leaves it. The proof of possession is checked as on `/cli`.
3. **A member.** Platform code adds the computer to the fragment as an
   editor for its owner, as `join_owners_agent` does for a chat's agent.
   It is a member like any other: it can call, post, sync, and deploy,
   and it can make fragments for its owner. One fragment spawning a
   computer to build a fancier fragment needs nothing more.
4. **Running.** The Sprite runs `fragment sync <fragment> --watch` (files
   both ways) and the declared services. It talks back with the CLI as
   itself: `fragment call`, `fragment post`, `fragment channel --follow`.
   The fragment reaches it by connecting out, as `fragment computer
   connect` does today, but signed with its key instead of a bearer
   token. That settles docs/finite-integration.md's "reaching a computer
   on a Sprite" for fragment: no Sprite URL and no second token. App
   code gets `job.computer(tool, args)`, a durable job step over the
   `computer serve` call protocol, keyed by step id.
5. **Removed.** Deleting the fragment, or `fragment computers rm`,
   revokes the computer's keys and destroys its Sprite. Destroying a
   Sprite is irreversible, so the owner confirms it.

The only credential a computer holds is its own key, in its CLI config
(0600) on the Sprite's disk (decision 21). Proposed:
its model calls go through the platform, signed with that key and billed
to the fragment's owner, instead of the Sprites connectors docs/secrets.md
planned. Then revoking the key cuts off everything at once.

## How a page shows it: a CUA "pet"

The page is the fragment's own UI. The computer stays out of sight.

- The desktop service takes a screenshot on each change (at most one a
  second). The computer stores it as a blob and posts `{sha, at}` to a
  declared `screen` channel. The page shows the newest frame. Everyone
  viewing sees the same frame live, because channels are multiplayer
  already.
- Input goes the other way. The page calls operations (`poke {x, y}`,
  `type {text}`) that append to a `control` channel. The computer
  follows that channel, applies each record once (keyed by its seq), and
  then posts the next frame.
- An agent drives it the same way, through those operations. A
  VNC-grade stream, a Sprite HTTP service proxied by the platform, can
  come later if a frame a second is too slow.

## What it needs from Paul

- **The Sprites token and where it lives.** Today's token is org
  `paul-miller` (`~/Downloads/sprite-token.txt`, per
  docs/finite-next-lessons.md). The proposal is a dedicated org for
  fragment.club. Its token becomes a node secret
  (`FRAGMENT_SPRITES_TOKEN`, set as a Fly secret from a file) that only
  `KEYS` uses, as with the WorkOS and OpenRouter management keys. No
  cell and no computer ever holds it. Per decision 13, an owner's own
  Sprites token comes later.
- **The cost.** At $0.07 per CPU-hour and $0.04375 per GB-hour, a mostly
  idle 1.5 GB computer costs about $0.076 per awake hour. A 10 GB disk
  costs about $5 a month hot and $0.20 a month cold. The proposal is to
  charge awake hours and disk to the owner's monthly budget as usage
  rows (decision 14), reserving an hour at each wake. On the default
  $20 budget, that is about 250 awake hours. The org is capped at 10
  running and 10 warm Sprites, so the fleet runs at most 10 at once
  until Fly raises it. Paul decides whether it is list price or a
  markup, and whether there is a per-person cap on computers.
- **The idle policy.** Sprites pause themselves about 30 s after
  activity (warm) and go cold later. The `Computer` cell's alarm needs:
  how long a job or an open page keeps it awake (proposed: while a job
  step runs, through a Sprites Task, plus 5 minutes after the last page
  viewer), and whether and when an unused computer is destroyed
  (proposed: never automatically; a cold disk costs cents).
- **The grant.** Does the owner allow each fragment's computer once in
  the share sheet (proposed), or does declaring it provision one on
  deploy?
- **Model access.** Should a computer's model calls go through the
  platform, signed by its key (proposed), instead of Sprites connectors?

## Order of work

1. The Sprites adapter in `KEYS` and the `Computer` cell (create, wake,
   destroy, alarm), against a Sprites fake in `crates/fakes`.
2. A Linux release artifact of the CLI with `computer`.
3. Platform pairing (`By::Identity`) and the computer's membership.
   Connect-out signed by the computer's key replaces the connect token.
4. The desktop service and a `pet` template, with a browser e2e.
5. Budget rows for awake hours and disk. Then one live Sprite on
   fragment.club, with Paul's go.
