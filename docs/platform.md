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
| `__signin`, `__signout` | A fragment origin's own session, through a single-use redemption from the platform | Sessions are the platform's; a fragment's code must not mint them |
| `__join` | Accepting an invite on the fragment's origin | Grants membership; to move to the platform's `/join/<name>` (phase 7, D) |
| `__live`, `__watch` | The live socket and the CLI's watch stream, taken only from the fragment's own page (`Origin`; a socket has no CORS) | Platform protocol |
| `__people` | Profiles (usernames, pictures) by identity | Reads the registry |
| `__files`, `__file`, `__tree` | The fragment's files, read through the platform | Reads git with the platform's token |
| `__fragments` | The signed-in owner's fragments, and making one | Owner-only, and only for a fragment that declares the `fragments` capability (the desktop) |
| `__sw.js`, `__preview.svg` | The push service worker, the link preview image | Platform assets |

## Served on the platform's origin

| Path | What |
|---|---|
| `/`, `/auth/*`, `/cli` | Sign-in, choosing a username, a new fragment, approving the CLI's key |
| `/api/*` | The signed API: fragments, members, identities, budgets, agents (`/api/agents`, `/api/a/*`, co-hosted) |

Every platform page must refuse to be framed (`frame-ancestors 'none'`;
PR `platform-no-framing`): fragments share the platform's site, so its
session cookie reaches a same-site frame.

## Behavior no fragment declares

- **The owner's agent joins every chat** made from the chat template
  (`cell/src/publish.rs`, `join_owners_agent`), as an editor that
  listens to its `chat` channel.
- **An agent's authority** in a turn is the lower of its asker's role
  and a cap (phase 7, decision 1).

## Planned (phase 7)

- `/share/<name>` and `/join/<name>` on the platform origin: the share
  sheet in a popup window, and accepting invites.
- `__chat.js` and `__chat.css` on every fragment's origin: the chat UI,
  which renders the records the agent writes, so it ships with the
  platform rather than frozen into each chat.
- Postable channels (a declared channel's `post` role) are a vanilla
  feature, not a special case: any fragment may declare one.
