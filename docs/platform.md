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
| `__signin`, `__signout` | A fragment origin's own session, through a single-use redemption from the platform; `__signin` only as a navigation of a page, `__signout` a POST from the fragment's own page | Sessions are the platform's; a fragment's code must not mint them, and another page must not set them off |
| `__frame` | A frame of this page, signed in on one of its owner's fragments: the platform mints a frame redemption from this origin's session and sends the frame on to that fragment's `__signin`, which sets a partitioned cookie for this page only (docs/api.md, Frames) | Only for a page whose `fragment.json` asks for `frame` and whose owner allows it (below); the page's code never holds the redemption |
| `__live`, `__watch` | The live socket and the CLI's watch stream, taken only from the fragment's own page (`Origin`; a socket has no CORS) | Platform protocol |
| `__people` | Profiles (usernames, pictures) by identity | Reads the registry |
| `__files`, `__file`, `__tree` | The fragment's files, read through the platform | Reads git with the platform's token |
| `__fragments` | The signed-in owner's fragments (with each one's share sheet, and the visibility and member and guest counts their list carries, for the desktop's Share item and badges), whether this page may show them inside it (`frame`), and making one | Owner-only, and only for a fragment that declares the `fragments` capability (the desktop) |
| `__sw.js`, `__preview.svg` | The push service worker, the link preview image | Platform assets |

## Served on the platform's origin

| Path | What |
|---|---|
| `/`, `/auth/*`, `/cli` | Sign-in, choosing a username, the home (your fragments, a new one, pairing your CLI), approving the CLI's key or pairing a computer |
| `/share/<name>` | The share sheet, in a window the desktop opens: who is in; the owner invites by username, sets roles, removes, revokes invites, sets who can open it, copies and renews the link, and allows `frame` (below). Sharing grants, so no fragment's code (which its author or an agent rewrites) may do it |
| `/auth/fragment` | Signing in on a fragment's origin; on one that is not the person's nor shared with them, it asks "Continue to X?" first, once (docs/api.md, Asking first) |
| `/join/<name>?token=` | Accepting an invite: what it grants, then a click; an invite by username is its invitee's alone. Replaced a fragment-origin `__join` (a page there is its author's) |
| `/api/*` | The signed API: fragments, members, identities, budgets, agents (`/api/agents`, `/api/a/*`, co-hosted) |

Every platform page must refuse to be framed (`frame-ancestors 'none'`;
PR `platform-no-framing`). Since the move to fragment.boats
(docs/fragment-boats.md, ROADMAP decision 23) the platform is cross-site
from every fragment, so its session cookie never reaches a fragment's
frame; the header stays for any fleet whose platform shares the
fragments' domain, and against every other site. And every one severs a
window that opened it (`Cross-Origin-Opener-Policy: same-origin`), so a
fragment's page cannot script the window it opened on one. The sharing
pages' forms also carry a token bound to the session and arm after a
moment (docs/api.md, Sharing).

## Capabilities a fragment declares, and who grants them

`fragment.json`'s `capabilities` asks for platform powers; each is
honored only for the fragment's owner viewing its page.

- `fragments` (`__fragments`): the owner's list, and making fragments.
  Declaring it is enough.
- `frame` (`__frame`): showing the owner's fragments inside the page,
  signed in as them. The desktop is the first user; a chat showing an app
  inline is the next. Declaring it is not enough: the owner allows it in
  the fragment's share sheet ("Your fragments inside it"; `PUT
  /api/f/<name>/grants/frame`, owner-only, never an agent), because a
  page that frames a fragment signed in could lay its own buttons over
  it. The grant stays with the fragment until the owner stops it. A
  framed fragment's pages answer `frame-ancestors` naming only the page
  that framed it through `__frame`, so no other page shows it signed in.
  The platform's new-fragment form is the other place to allow it: for a
  template that asks for `frame` (the desktop), it says so plainly, and
  its submit records the same grant (`auth.rs`, `/auth/new`).

## Behavior no fragment declares

- **Which of a browser's cookies count** on a fragment's origin follows
  its Fetch Metadata (docs/api.md, Which cookies count): another
  fragment's page, one site with it, reaches it as a stranger does.
- **A browser opening a fragment's URL** that no session there admits is
  sent through the platform's sign-in for it and back; any other refusal
  a browser navigates to is the platform's page, not JSON (docs/api.md,
  Opening a fragment by its URL). An app's own answers pass as they are.

- **A new desktop is its owner's alone**: made from the platform's
  `desktop` template with no visibility asked, it is `members`
  (`publish.rs`, `first_visibility`); every other template's is `link`.
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
