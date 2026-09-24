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
   `desktop.futurepaul.fragment.club`. A username is chosen once (not
   renamed, since renaming breaks URLs), with a profile picture stored
   with the identity.
2. **A certificate per hostname**, asked of Fly's API when a fragment is
   made (it validates on its own: the `*.fragment.club` DNS record already
   answers nested names). This also opens custom domains later. Let's
   Encrypt allows about 50 new names a week under fragment.club: enough
   for the alpha, a limit to plan for (per-user wildcards need a DNS API).
3. **A hard cut:** fragments made before usernames are not migrated.
4. **The agent runs on fragment.club**, inside the fleet that already
   runs (no new machines), metered in its owner's budget.

## The shape

- **A fragment's name is `<label>.<username>`** (`desktop.futurepaul`),
  its host `<label>.<username>.<suffix>`, its path `/f/<label>.<username>/`,
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
5. **A computer with screenshots.** `fragment computer connect` (outbound),
   a `screenshot` tool (headless Chromium on the computer), the image in
   the chat. Acceptance: a turn opens a page on the computer and the chat
   shows its screenshot.
