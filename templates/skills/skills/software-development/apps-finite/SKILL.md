---
name: apps-finite
description: Make, publish, and share web apps, sites, pages, dashboards, documents, games, and stateful tools as fragments with the `fragment` CLI. Use whenever a human wants something of theirs on the web at a link, with its own state, live for everyone who has it open, or shared with other people.
---

# Apps

A fragment is one small web app at its own link: a folder of files in git,
an app of named operations over its own SQLite, channels every open page
follows live (multiplayer is built in), and members with roles. Websites,
landing pages, dashboards, documents, browser games, trackers, inboxes and
tools are all fragments. You make them with the `fragment` CLI, from your
computer's terminal.

This skill replaces Finite Sites publishing (`fsite`), `publish-web-apps`
and `website-building` on fragment.

## On your computer

The `fragment` CLI is installed, and already acts as you: no login, no key.
Your computer's API signs each request as your agent
(`FRAGMENT_AS_AGENT`), acting for your owner (`FRAGMENT_FOR`), so you hold
exactly your owner's grants and never more.

```sh
fragment whoami            # who you are, and for whom you act
fragment list              # your owner's fragments you can reach, and your role in each
fragment guide             # the whole manual: read it before your first app
```

- What you make is your owner's: `fragment create garden` makes
  `garden.<owner's username>`, owned by them, with you as its editor. A
  bare label in any command names one of your owner's (`fragment status
  garden`).
- Its hosting and its AI bill your owner, as do your own model calls.
- `fragment login` and `fragment keys` are not for you: you have no key.

## Fast first reveal

Optimize for a fast, useful first reveal. Do not spend a long hidden turn polishing
an aesthetic direction the human has not seen or approved.

For a new app or substantial redesign:

1. Infer a credible initial direction from the prompt and existing brand context.
   When using `impeccable-finite`, form its compact direction card and carry that
   premise into the draft. Ask a question first only when the answer is truly blocking.
2. Build the smallest coherent draft that makes the direction tangible. Prefer a
   strong above-the-fold experience plus one representative section or state over
   a complete but generic site.
3. Deploy it and run reveal QA only: confirm that the page loads at its link, the
   primary view renders, and there are no fatal browser errors. Make at most one
   automatic correction pass.
4. Reveal the draft before exhaustive QA: send its link (the share link for a
   link-visibility fragment) and a screenshot, label what is provisional, and name
   2-3 meaningfully different aesthetic options. Recommend one and explain each option
   in concrete terms such as subject cues, signature, type, density, color, imagery,
   and motion, not vague labels alone.
5. Ask the human to choose a direction or react in their own words. Make the cost
   of the next step legible: distinguish a quick visual revision from completing
   the build and running full QA.

If the human already supplied precise art direction, still reveal the first coherent
draft early, but do not manufacture an unnecessary choice. If the task is a small,
well-specified edit, skip this checkpoint. The feeling of rapid creation is part of
the product.

## Make one

```sh
mkdir -p ~/apps && cd ~/apps
fragment init garden --template blank     # scaffold, create, first deploy: prints its link and share link
cd garden                                 # the folder is the fragment's working copy
```

Templates (`fragment new --list`): `blank` (one page), `todo` (operations,
a channel, a live page), `inbox` (webhooks in, a job), `notes` (a folder
of markdown as a live site), `calories` (a channel trigger whose job asks
a model, then logs), `split` (shared costs: a job reads a receipt photo, live balances). Start from the closest one and read its files: they are working examples.

When the human wants one of the platform's templates as it is, with no
folder of yours, make it on the platform alone:

```sh
fragment create shopping --template todo                  # a todo, live at once, copied in as its first commit
fragment create garden-talk --template chat --title "Garden talk"   # a chat on the platform's own chat
```

A blessed template (`chat`, `agent`, `skills`, `brain`) is named, not
copied: it runs the platform's current release, and `--title` is its own
title. Pull any fragment into a folder later with `fragment sync <name>
--dir . --mode pull`.

The folder:

```
fragment.json      operations, channels, triggers, meta (its title and link preview)
app.mjs            class App: one method per operation (optional)
applib/            modules app.mjs imports
site/              the pages and assets, served from live
everything else    files: data, notes, media, synced and versioned
```

A page imports the browser library from its own fragment:

```html
<script type="module">
  import * as fragment from "./__fragment.js";
  fragment.live("list", {}, (r) => render(r.items));            // re-runs after every change
  await fragment.call("add", { text: "hello" });
  fragment.subscribe("activity", (rec) => console.log(rec.body));
</script>
```

## The loop

```sh
fragment write garden site/index.html --from index.html   # one text file to main, through the platform
fragment deploy garden                    # move live to main: the site and the app
fragment deploy garden --dir .            # or sync a whole folder to main, then move live
fragment status garden                    # its link, visibility, live commit, and code.error if its code was refused
fragment events garden --tail 30          # what happened: believe it over your memory
fragment rollback garden                  # live back to the deploy before
```

- Deploys are commits; deploy freely and roll back in one command.
- A deploy whose `fragment.json` or `app.mjs` the platform refuses keeps
  the last good code serving: `fragment deploy` says why and exits 1.
- Fragments run no builds. For React, Vite or another toolchain, build in
  the folder and deploy the built output under `site/`; keep the source
  beside it (a top-level `node_modules/` never syncs).
- Commit after each meaningful milestone: each deploy is one.

## Choose the shape

- **Static site or document**: files under `site/`, no `app.mjs`.
  Markdown that should stay the source belongs in the `notes` template.
- **Stateful app**: operations in `fragment.json` and `app.mjs` over the
  fragment's own SQLite; pages call them and follow channels live. Read
  `references/shared/19-backend.md`.
- **Webhooks, schedules, background work**: a job, and a trigger on the
  inbox, a cron, or changed files.
- **AI in the app**: `job.ai.text` and `job.ai.image` steps, billed to your
  owner. Read `references/shared/20-llm-api.md`.

## Check it in a real browser

Open the live link in a browser and check it at desktop and phone widths
before you share it: a page is not validated because its files deployed.
For rich apps, use Playwright (`references/shared/12-playwright-interactive.md`)
or your browser tools. A `link` fragment opens with its share link (`fragment
open garden`); a `members` fragment needs a signed-in member, so check it
while it is `link` and close it after if it must stay private.

## Share it

```sh
fragment open garden                                   # its link, and its share link (?view=…, a secret)
fragment visibility garden link                        # anyone with the share link (the default)
fragment visibility garden members                     # members only
fragment members add garden <id:… | npub> --role editor # a person (their agents act for them, too)
fragment invite create garden --role viewer --uses 5   # a link a person opens to join
```

- Send the person the links the CLI prints; never construct one.
- `public` makes it anyone's to open, no share link needed. Before it, say plainly
  that anyone on the internet will see it, check it holds no secrets,
  private files, drafts or personal data, and wait for an explicit yes.
  Never make it public merely to preview it.
- You share your owner's fragments as they would: members, invites,
  visibility and the links (`fragment rotate`), on the ones they own. Do it
  when your owner asked, and say so in the chat each time: who you added and
  at what role, what is public now, which link changed. Their events and
  members list name you as who did it, for them.
- You never delete a fragment or set its cap, and you share nothing on a
  fragment your owner only edits or views, for anyone but your owner, or
  while your owner holds you below them: the platform refuses it (403).
  Ask your owner to do it.

## Routing to the design references

Choose one route, then load the matching references.

- Informational / marketing / editorial:
  Read `references/shared/01-design-tokens.md`, `references/shared/02-typography.md`, `references/shared/04-layout.md`, `references/shared/05-taste.md`, `references/shared/08-standards.md`, `references/shared/09-technical.md`, and `references/informational/informational.md`.
- Dashboard / data-heavy app:
  Read `references/shared/01-design-tokens.md`, `references/shared/02-typography.md`, `references/shared/04-layout.md`, `references/shared/05-taste.md`, `references/shared/08-standards.md`, `references/shared/09-technical.md`, `references/shared/10-charts-and-dataviz.md`, `references/shared/12-playwright-interactive.md`, `references/shared/19-backend.md`, and `references/shared/20-llm-api.md` when applicable.
- Browser game / immersive interactive:
  Read `references/shared/01-design-tokens.md`, `references/shared/02-typography.md`, `references/shared/03-motion.md`, `references/shared/08-standards.md`, `references/shared/09-technical.md`, `references/shared/12-playwright-interactive.md`, plus `references/game/game.md` and companions as needed.

If the main problem is art direction, visual quality, hierarchy, polish, or "make
this feel designed," read the sibling `impeccable-finite` skill first, then return
here for implementation.

### Fragment overrides

The design references were adapted from a broader web skill. On fragment,
these rules win over anything they say:

- Publish with `fragment deploy`, never `deploy_website`, a port, or a
  tunnel; do not edit DNS, proxies or networking.
- A fragment's pages and files are served from its own origin, not S3 and
  not a sandboxed iframe: same-origin `fetch()` of its files works, and a
  file of 1 MiB or more is served from the blob store at its path.
- `localStorage` works on a fragment's own origin; for state people share,
  use the fragment's operations and channels instead.
- Keep secrets out of files: an app's outbound calls fill `{{NAME}}` from
  the fragment's secrets (`fragment secret set`), outside your code.

## Design standards

- Avoid interchangeable AI-looking layouts.
- Use expressive typography and intentional spacing.
- Create visual rhythm with real imagery, diagrams, or illustration.
- Make dashboards feel like products, not admin templates.
- Treat screenshots as product review, not just bug checks, and use them to
  align with the human early.

## Guardrails

- One fragment, one problem: a second job is a second fragment.
- Prefer plain files (Markdown, JSON, CSV) the next agent can diff.
- Reuse an operation's id when you retry it (`fragment call … --id`); use a
  new one for a new action.
- The share link's `?view=` token is a secret: send it to the people the
  human named, and never render it into public pages.
- Do not claim it works until you opened its link and saw it work.

## Reference map

- `references/shared/01-design-tokens.md`: base tokens and CSS system
- `references/shared/02-typography.md`: font pairing and type rules
- `references/shared/03-motion.md`: animation and motion systems
- `references/shared/04-layout.md`: responsive structure and composition
- `references/shared/05-taste.md`: polish, empty states, and finishing passes
- `references/shared/06-css-and-tailwind.md`: Tailwind / CSS implementation patterns
- `references/shared/07-toolkit.md`: libraries and supporting tools
- `references/shared/08-standards.md`: accessibility, performance, anti-patterns
- `references/shared/09-technical.md`: the fragment folder, deploys, and project rules
- `references/shared/10-charts-and-dataviz.md`: charts and dashboard patterns
- `references/shared/11-web-technologies.md`: compatibility notes
- `references/shared/12-playwright-interactive.md`: browser QA on your computer
- `references/shared/19-backend.md`: operations, channels, jobs and triggers
- `references/shared/20-llm-api.md`: AI steps and outside APIs in an app
- `references/informational/informational.md`: informational / marketing site guidance
- `references/game/game.md`: browser game guidance
- `references/game/2d-canvas.md`: 2D canvas specifics
- `references/game/game-testing.md`: game QA
