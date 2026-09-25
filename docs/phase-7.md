# Phase 7: chats and sharing

Status: **planned 2026-09-25 with Paul; wave 1 starting.** The ROADMAP
keeps the acceptance (phase 7); this file keeps the decisions, the
slices, and who owns which files while slices run in parallel.
`docs/platform.md` lists every place the platform does something a
fragment's own code cannot.

## Why

Paul, after using phase 6: the chat is a low-effort page (no working
state, no tool calls, not the look ported from finite-mono), nothing can
be shared from a browser, the agent answers twice, and it cannot edit an
app he made himself (403: it is a member only of fragments it made).
Every "New chat" is a whole fragment with its own app worker and a
five-minute poll. Four read-only investigations (2026-09-25) found the
rest:

- **Double replies:** the chat's own `say` was one of the agent's tools,
  and the platform posts the turn's answer through `say` again. Fixed
  separately (PR `agent-one-reply`), with idempotent `listen`.
- **Clickjacking:** no platform page refused framing, and fragments are
  same-site, so a fragment could frame the CLI key approval and take an
  account with one click. Fixed separately (PR `platform-no-framing`).
- **One conversation for all chats:** a message from chat B during chat
  A's turn steers A's turn and is answered in A; any chat can ask about
  the others.
- **Anyone with a chat's link drives the agent:** chats default to
  `link`, `say` is public, and the agent acts with every membership it
  has.
- **An agent listens to at most 16 channels**, and nothing prunes a
  listen, so from the seventeenth chat on the agent never joins (the
  alarm retries forever).
- **What a chat costs:** a repo and 14 code.storage calls at creation;
  idle, 288 wakes and 576 code.storage calls a day (the poll backstop);
  open, its own loaded worker (~2.7 MiB, under the node's cap of 255,
  never released).

## Decisions (Paul, 2026-09-25)

1. **An agent acts with the authority of whoever asked, capped.** Each
   call in a turn acts with the lower of the role of the person who
   started the turn and a cap: the agent's own membership, or `editor`
   on fragments its owner belongs to. Owner-only actions (members,
   invites, visibility, link rotation, deletion) never go through an
   agent. The owner's turns reach every app the owner has (the 403
   goes); a guest's turns reach only what the guest could reach. This
   replaces ROADMAP decision 3's "a chat guest is a member with the
   owner's full authority" (ROADMAP decision 17).
2. **No stopgap:** until slice A lands, anyone with a chat's link still
   drives the agent as today.
3. **Chats stay vanilla fragments; channels can be posted to.** A
   fragment declares a channel people may post to (`"chat": {"read":
   "public", "post": "public"}`); the platform appends the record itself
   (role check, rate limit, 64 KiB, deduped by principal and id). A chat
   then needs no app code, so no worker. This reverses MODEL's "clients
   never append" for declared channels only (ROADMAP decision 18). The
   poll backstop becomes quiet for fragments no one outside the platform
   has written to recently.
4. **The share sheet is a platform page in a popup window**,
   `fragment.club/share/<name>`, opened from the desktop. It cannot be
   framed, and it takes no grant from its URL, so a fragment's code
   (which its author or an agent can rewrite) cannot share by fetching,
   framing, or scripting it.

## Slices

Wave 1 runs in parallel; wave 2 follows A and B1.

| Slice | What | Owns (while it runs) |
|---|---|---|
| A. Agent authority | One conversation per chat; a turn records who started it; every agent call carries `for=<id>` (covered by its NIP-98 signature) and the cell applies decision 1; steers only from the turn's starter; anonymous messages ignored; listens deduped by fragment and channel, pruned when the fragment answers 404, cap raised to 500 | `agent/`, `cell/src/agents.rs`, `cell/src/routed.rs`, `crates/core/src/access.rs`, the agents lane |
| B1. Postable channels, quiet poll | `post` role on a declared channel; the platform appends; `__fragment.js` gains `fragment.post`; the poll backstop quiets for untouched fragments | `crates/core` (manifest, channels), `cell/src/channels.rs`, the channel route in `cell/src/lib.rs`, `cell/client.mjs`, `cell/src/fragment.rs` and `cell/src/plane.rs` (poll), the channels lane |
| D. Share sheet | `/share/<name>` and `/join/<name>` on the platform origin (session cookie, `frame-ancestors 'none'`, an Origin that must equal the platform, a CSRF token, nothing prefilled); the desktop's Share item and shared badges; `__fragments` adds member counts | `cell/src/auth.rs` (after `platform-no-framing` merges), `cell/src/members.rs`, `cell/src/serve.rs`, `templates/desktop/`, a new share lane |
| C. The new chat (wave 2) | The chat template drops its app code and posts to its channel; the agent posts progress records (turn start, each step: tool, short args, ok or error, a 300-character excerpt; turn end) to a `work` channel readers of `viewer` and up see; the chat UI ported from finite-mono (tool groups, working line, composer, markdown) served by the platform as `__chat.js` and `__chat.css` | `templates/chat/`, `agent/` (reply path, progress), `cell/src/serve.rs` (the two files) |
| E. Acceptance (wave 2) | The ROADMAP's phase 7 checks: two people, one chat; invite and accept; both see messages live, labeled; the guest drives the owner's agent within the guest's reach; revoking closes the guest's socket and answers 403; a rewritten desktop cannot share by fetch, frame, direct API call, or scripting the sheet | e2e lanes |

Every slice: its own worktree and branch from master, `cargo xtask
check` and its lanes green, the full e2e in CI, a PR the coordinator
merges. A slice that needs a file another running slice owns asks the
coordinator first.
