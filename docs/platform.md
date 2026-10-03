# The platform's special cases

A fragment is vanilla: its files, its `fragment.json`, and its app code,
all of which its author (or an agent that edits it) can rewrite. The
platform is what no fragment can rewrite: the router, the platform cells,
and the few pages and scripts every fragment is served with. This list
is every place the platform does something a fragment's own code cannot,
and why. Add a row when adding one; a feature that fits in a fragment
belongs in a template instead.

## Served on every fragment's origin

| Path | What | Why the platform |
|---|---|---|
| `__fragment.js` | The browser API: calls, live queries, channels, presence, push | One version for every fragment; it speaks the platform's wire protocol |
| `__signin`, `__signout` | A fragment origin's own session, through a single-use redemption from the platform; `__signin` only as a navigation of a page, `__signout` a POST from the fragment's own page | Sessions are the platform's; a fragment's code must not mint them, and another page must not set them off |
| `__live`, `__watch` | The live socket and the CLI's watch stream, taken only from the fragment's own page (`Origin`; a socket has no CORS) | Platform protocol |
| `__people` | Profiles (usernames, pictures) by identity | Reads the registry |
| `__files`, `__file`, `__tree` | The fragment's files, read through the platform; `__files` is a viewer (`__files.js`, `__files.css`): a tree beside a reader for markdown, text, and pictures | Reads git with the platform's token; the viewer renders any file on the fragment's origin, so a file's text becomes DOM as text only |
| `__sw.js`, `__preview.svg` | The push service worker, the link preview image | Platform assets |

## Served on the platform's origin

| Path | What |
|---|---|
| `/`, `/settings` | The shell (docs/api.md, The shell): your chats and apps, and at `/settings` its settings (your account, credit, computer, connections, pairing your CLI) |
| `/auth/*`, `/cli` | Sign-in, approving the CLI's key |
| `/share/<name>` | The share sheet, in a dialog in the shell, or on a page of its own: who is in; the owner invites by username, sets roles, removes, revokes invites, sets who can open it, and copies and renews the link. Sharing grants, so no fragment's code (which its author or an agent rewrites) may do it |
| `/auth/fragment` | Signing in on a fragment's origin; on one that is not the person's nor shared with them, it asks "Continue to X?" first, once (docs/api.md, Asking first) |
| `/join/<name>?token=` | Accepting an invite: what it grants, then a click; an invite by username is its invitee's alone. Replaced a fragment-origin `__join` (a page there is its author's) |
| `/api/*` | The signed API: fragments, members, identities, budgets, agents (`/api/agents`, `/api/a/*`, co-hosted) |

No page on another origin may frame a platform page: every one answers
`frame-ancestors 'none'` (PR `platform-no-framing`) but the share sheet,
which answers `frame-ancestors 'self'` (and `X-Frame-Options:
SAMEORIGIN`) so the shell can show it in a dialog. `'self'` is the
platform's origin alone, and nothing but the platform's own pages is
served there: every fragment is on an origin of its own (a fleet without
a hostname suffix, where fragments share the platform's origin, is the
debt ledger's, and there a fragment's page could read the sheet with a
fetch anyway). Since the move to fragment.boats
(docs/fragment-boats.md, ROADMAP decision 23) the platform is cross-site
from every fragment, so its session cookie never reaches a fragment's
frame; the header stays for any fleet whose platform shares the
fragments' domain, and against every other site. And every one severs a
window that opened it (`Cross-Origin-Opener-Policy: same-origin`), so a
fragment's page cannot script the window it opened on one. The sharing
pages' forms also carry a token bound to the session and arm after a
moment (docs/api.md, Sharing).

## An agent a fragment declares

`fragment.json`'s `agent` block (docs/api.md, A fragment's agent) is a
vanilla feature: any fragment may declare one, and a fragment that does
not carries nothing of it. What the platform does for it, a fragment's
code cannot: its deploy makes the agent (named as the fragment, its
owner's, an editor of that fragment alone, listening to the declared
channel) or removes it, offers it only the operations the block names,
runs its turns for the fragment's jobs (`job.agent`), and pays for its
model calls from the owner's budget (`cell/src/agents.rs`,
`sync_agent`).

## Behavior no fragment declares

- **Which of a browser's cookies count** on a fragment's origin follows
  its Fetch Metadata (docs/api.md, Which cookies count): another
  fragment's page, one site with it, reaches it as a stranger does.
- **A browser opening a fragment's URL** that no session there admits is
  sent through the platform's sign-in for it and back; any other refusal
  a browser navigates to is the platform's page, not JSON (docs/api.md,
  Opening a fragment by its URL). An app's own answers pass as they are.
- **An agent's authority** in a turn is the lower of its asker's role
  and a cap (phase 7, decision 1).
- **An agent in a chat posts its work** (phase 7, C): a turn a chat
  started posts its start, each tool call, and its end to the chat's
  `work` channel, and its answer to `chat` naming the turn, when the chat
  declares them postable; a stop the turn's starter posts on `chat` stops
  it. The records' shape is docs/api.md's (A chat's records).
- **Postable channels keep their newest 10,000 records**
  (`limits::POSTED_KEPT`), the oldest dropped with their posts' keys, as
  `events` and `ops` keep theirs: not declarable yet.

## Planned (phase 7)

- Postable channels (a declared channel's `post` role) are a vanilla
  feature, not a special case: any fragment may declare one.
