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
| `__chat.js`, `__chat.css` | The chat's page (phase 7, C): messages, each turn's steps grouped above its answer, the working line, a composer, Stop for a turn's starter; the chat template's `index.html` is a shell that mounts it | It renders the records agents write (`work`, docs/api.md), so it ships with the platform rather than frozen into each chat |
| `__signin`, `__signout` | A fragment origin's own session, through a single-use redemption from the platform | Sessions are the platform's; a fragment's code must not mint them |
| `__live`, `__watch` | The live socket and the CLI's watch stream, taken only from the fragment's own page (`Origin`; a socket has no CORS) | Platform protocol |
| `__people` | Profiles (usernames, pictures) by identity | Reads the registry |
| `__files`, `__file`, `__tree` | The fragment's files, read through the platform | Reads git with the platform's token |
| `__fragments` | The signed-in owner's fragments (with each one's share sheet, and the visibility and member and guest counts their list carries, for the desktop's Share item and badges), and making one | Owner-only, and only for a fragment that declares the `fragments` capability (the desktop) |
| `__sw.js`, `__preview.svg` | The push service worker, the link preview image | Platform assets |

## Served on the platform's origin

| Path | What |
|---|---|
| `/`, `/auth/*`, `/cli` | Sign-in, choosing a username, a new fragment, approving the CLI's key |
| `/share/<name>` | The share sheet, in a window the desktop opens: who is in; the owner invites by username, sets roles, removes, revokes invites, sets who can open it, copies and renews the link. Sharing grants, so no fragment's code (which its author or an agent rewrites) may do it |
| `/join/<name>?token=` | Accepting an invite: what it grants, then a click; an invite by username is its invitee's alone. Replaced a fragment-origin `__join` (a page there is its author's) |
| `/api/*` | The signed API: fragments, members, identities, budgets, agents (`/api/agents`, `/api/a/*`, co-hosted) |

Every platform page must refuse to be framed (`frame-ancestors 'none'`;
PR `platform-no-framing`): fragments share the platform's site, so its
session cookie reaches a same-site frame. And every one severs a window
that opened it (`Cross-Origin-Opener-Policy: same-origin`), so a
fragment's page cannot script the window it opened on one. The sharing
pages' forms also carry a token bound to the session and arm after a
moment (docs/api.md, Sharing).

## Behavior no fragment declares

- **The owner's agent joins every chat** made from the chat template
  (`cell/src/publish.rs`, `join_owners_agent`), as an editor that
  listens to its `chat` channel.
- **An agent's authority** in a turn is the lower of its asker's role
  and a cap (phase 7, decision 1).
- **An agent in a chat posts its work** (phase 7, C): a turn a chat
  started posts its start, each tool call, and its end to the chat's
  `work` channel, and its answer to `chat` naming the turn, when the chat
  declares them postable; a stop the turn's starter posts on `chat` stops
  it. The records' shape is docs/api.md's (the chat template).
- **Postable channels keep their newest 10,000 records**
  (`limits::POSTED_KEPT`), the oldest dropped with their posts' keys, as
  `events` and `ops` keep theirs: not declarable yet.

## Planned (phase 7)

- Postable channels (a declared channel's `post` role) are a vanilla
  feature, not a special case: any fragment may declare one.
