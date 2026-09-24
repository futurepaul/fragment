# Phase 6: the desktop, as a fragment (and what it needs)

Status: **agreed with Paul 2026-09-24, in progress.** Paul's framing:
fragment is the core product; the desktop is a fun demonstration of what
fragment can do: "ideally it's just a fragment that the user can deploy
with a click". The goal is to get back what finite-next could do before
the move (`finite-next-worktrees/claude-cell-agent`, its README's table):
**chat with goose in the cell, have my agent make apps for me, and
connect to a computer and receive screenshots**, on the Desktop UI
session's layout (`finite-next-worktrees/desktop-ui`, `d8d5c26`).

## Paul's calls (2026-09-24)

1. **People have usernames, and fragments live under them:**
   `desktop--futurepaul.fragment.club`. A username is chosen once (not
   renamed, since renaming breaks URLs), with a profile picture stored
   with the identity.
2. **Flat hosts, one certificate** (changed the same day). The first cut
   asked Fly for a certificate per nested host
   (`desktop.futurepaul.fragment.club`), but with every chat a fragment
   that would spend Let's Encrypt's ~50 new names a week in days. A flat
   host, `<label>--<username>`, is one label under the suffix, which the
   existing `*.fragment.club` certificate covers. No `<username>.fragment.club`
   home: people sign in at fragment.club and go from there. Sessions are
   `__Host-` cookies (every fragment shares the platform's domain and may
   set a cookie for all of it).
3. **A hard cut:** fragments made before usernames are not migrated.
4. **The agent runs on fragment.club**, inside the fleet that already
   runs (no new machines), metered in its owner's budget.

## The shape

- **A fragment's name is `<label>.<username>`** (`desktop.futurepaul`),
  its host `<label>--<username>.<suffix>`, its path `/f/<label>.<username>/`,
  its code.storage repo `<label>--<username>` (repo names are global in
  the org). Labels and usernames never contain `--`. Creating with a bare
  label puts it under the creator's username (an agent's: its owner's).
- **The desktop is a template**, deployed with one click from the
  platform's "new" page like any other template. It composes other
  fragments in iframes: chats are chat fragments (their own UI), apps are
  apps, and a file opens through its own fragment's `__file`. It holds no
  authority over other fragments. The platform adds only general pieces:
  the "new from a template" page, and one capability a fragment can
  declare, **granted only when the viewer is its owner**: list the owner's
  fragments (any dashboard app can use it).
- **Your agent is a member of your chats** and answers there (phase 7's
  chat template). Its tools gain the platform's verbs, so it can make an
  app: create a fragment (under its owner), write files, deploy, read.
- **A computer** reaches its agent without a public URL (it connects out),
  and its tools gain a screenshot that lands in the chat as an image.

## Steps (each ends with its e2e and a note here)

1. **Usernames and per-user names.** The registry holds usernames (claimed
   once; reserved words refused) and pictures; the platform asks for a
   username after the first sign-in; the router serves nested hosts;
   creates qualify names; `KEYS` asks Fly for each fragment's certificate.
   Acceptance: a person without a username cannot create; two people each
   make `todo` and get two fragments; a nested host serves its fragment;
   a username is taken once; the picture round-trips; the e2e runs on
   qualified names throughout.
   **Built 2026-09-24:** local e2e 671 passed, 0 failed (fork `43c3238`:
   its lockfile gained `KEYS`'s `hmac`, which `native::scope` uses to
   derive the Fragment's address it checks a certificate request
   against). The hosted e2e's fragments now keep their names across runs
   (`todo.e2e`, …) under the username `e2e`, so each host's certificate
   is asked for once, and a run waits for a new host's certificate.
   Deploying it is a hard cut: fragments made before it are not served.
2. **Templates and one-click deploy.** A server-side create from a
   template (chat, todo, desktop, a blank app), server-side commit and
   deploy routes (which the agent's tools also use), the platform's "new"
   page, and the owner capability.
   **Built 2026-09-24** (the desktop template comes in step 3): the
   templates are one crate (`crates/templates`) the CLI and the cell both
   embed; the cell offers `blank`, `chat`, `todo`, and `inbox` (not
   `notes`, 3 MiB). `POST /api/f/{name}/files` and `/deploy`
   (`cell/src/publish.rs`); `fragment.json`'s `capabilities:
   ["fragments"]` lets its owner's page read `__fragments`; the
   platform's home lists your fragments and makes a new one. e2e lane
   `templates`: 30 checks.
3. **The desktop template**, ported from `claude/desktop-ui`.
   Acceptance (ROADMAP phase 6): a browser e2e opens apps and files into
   the viewer, reorders and closes panes, collapses both sides, works at
   phone width, and keeps the layout across a reload.
   **Built 2026-09-24** (`templates/desktop`): `layout.js`, `viewer.js`,
   and split-grid as the Desktop UI session made them; `desktop.js`
   rewritten for fragments. The chats are chat fragments (the desktop's
   own app keeps which), every other fragment of the owner's is an app,
   and each shows in its own frame, which signs in on its own origin
   (`__signin` now comes straight back when it is signed in already).
   The capability grew a second half: the owner's page may also make a
   fragment (`POST __fragments`), so "New chat" and "Add an app" work
   from the desktop. Every fragment has a `__files` page; framed, its
   links ask the page around it to open a file (only a frame of one of
   the owner's fragments is heard). The e2e lane `desktop` (17 checks,
   Chrome) runs on a node whose platform is on the fragments' domain, as
   fragment.club is: a framed fragment can sign in through the platform
   only when a browser counts both as one site.
4. **Your agent in your chats, making apps.** The agent service runs in
   the fleet; turns are metered in the owner's ledger; a new chat has the
   owner's agent in it; the agent's platform tools.
   Acceptance: in a browser, a person asks their agent for an app; the
   agent makes, deploys, and uses it; it appears in the desktop's sidebar.
   **The plan (2026-09-24):**
   - **4a. The agent is a second script in the cell's fleet**, not a
     second fleet: celld already loads a script a service binding names,
     from that script's own pointer. Our fork adds `celld deploy --named`
     (publish a script without making it the fleet's application) and
     `celld dev --with PROJECT` (co-host it locally the same way), fork
     `0d80ead`. The cell binds `AGENTS` and its router hands
     `/api/agents` and `/api/a/*` to it; the agent's script has no
     ingress of its own, so it can trust who the router says is calling
     (the router already resolves every signed request's identity), and
     it stops verifying signatures and asking the registry itself. Dev,
     the e2e, and fragment.club all co-host (one node, one deploy order:
     the agent `--named`, then the cell).
   - **4b. Metering:** an agent's model calls use its owner's org
     OpenRouter key (the one the `Ledger` mints, whose limit is the
     month's allowance), fetched per turn through the platform. The
     fleet's own OpenRouter key for agents goes away. Usage rows per step
     come later (debt ledger); the hard stop is OpenRouter's.
   - **4c. Your agent:** each person has one default agent,
     `agent.<username>` (agent names are qualified like fragments). The
     platform makes it on first need and registers it for its owner. A
     chat made from the desktop, or with `agent: true`, gets the owner's
     agent as an editor that listens.
   - **4d. The agent makes apps:** its tools gain the platform's verbs,
     through the same signed API: make a fragment (under its owner's
     username, the owner as its owner and the agent an editor), write
     files, deploy, read files. The chat template shows agents' answers
     as markdown and names people by username.
   - **4e.** The desktop notices new fragments (it asks again when the
     page regains focus and after a chat's message), so an app the agent
     made appears in the sidebar.
   **Built 2026-09-24** (local, not deployed; fork `0d80ead`): 4a–4e as
   planned, with two changes. The desktop asks again every five seconds
   while it is in view, not after a chat's message (it cannot see into
   the chat's frame). The chat names people through a new `__people`
   route on every fragment (a person's username and picture, an agent as
   "<owner>'s agent"). Acceptance met in Chrome (e2e lane `desktop`, 22
   checks): in a new chat the owner's agent answers; asked for an app, it
   makes a fragment, writes its page, deploys it, and says so; the app
   appears in the sidebar without a reload, and opened it is the page
   the agent wrote. The agents lane checks the same in the API, and that
   every model call carried the owner's own key.
5. **A computer with screenshots.** `fragment computer connect` (outbound),
   a `screenshot` tool (headless Chromium on the computer), the image in
   the chat. Acceptance: a turn opens a page on the computer and the chat
   shows its screenshot.
   **Built 2026-09-24** (local, not deployed): the computer's protocol is
   unchanged; `fragment computer connect` carries it over a long poll
   instead of a public URL. The owner attaches with `fragment agent
   computer <agent> --connect --token-file <f>` (a connect token, answered
   once; the agent keeps its hash); each request waits in the agent's
   storage until the computer fetches it (`POST /api/a/<agent>/computer/
   poll`, 25 s) and posts the answer back (`/computer/answer`), and a turn
   leaves out a computer that has not asked in a minute. The computer's
   `screenshot {url}` takes the page with headless Chrome (it waits for the
   file, then stops Chrome, which can linger). An image a tool answers is
   kept for the chat (the model reads a note, since its model may read no
   images), written to the chat's files as `shots/…png` with the answer,
   and the chat's markdown shows it (images only from the fragment's own
   `__file`). The files route now takes up to 1 MiB. e2e lane
   `screenshots` (8 checks, a real `fragment computer connect` process and
   Chrome): the tunnel, a wrong token refused, the tools through it, and a
   chat turn whose answer shows a PNG taken on the computer, drawn in the
   chat's page.
