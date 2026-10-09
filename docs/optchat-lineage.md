# optchat's lineage: what is Victor's, and what is ours

The mind (docs/optchat.md) is built on VictorTaelin's gist,
gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449. On
2026-10-07 it was "OptChat", and we built from that version (kept as
`~/dev/finite/optchat-spec-v1.md`). On 2026-10-08 he rewrote it as
"UniiChat" (`~/dev/finite/uniichat-spec.md`): "the original had a bug
that trashed the cache". We now follow the rewrite.

This page splits the mind three ways:
- **Victor's:** what we follow as he wrote it.
- **Adapted:** where his rule meets a different runtime (a fragment, not
  a process; Workers AI, not Anthropic) and we changed the means, not the
  aim.
- **Ours:** what the gist does not have at all.

docs/optchat.md, "Where we differ from the gist", is the same list as a
spec, with every detail.

## Victor's, as he wrote it

**The idea**
- One chat that never ends, and the chat is the memory.
- The log is append-only, and nothing in it is edited or deleted.
- The kinds are `user`, the agent's replies, `tool`, `echo`, `work` and
  `note`.
- Thoughts are never logged.
- A tool's output is clipped to its head and tail, 30 000 characters in
  all. Any other long text is never cut: it becomes several messages in
  a row.

**The tree**
- It is purely binary, with lines of at most 512 bytes.
- A node is named `id+n`.
- A node whose source fits in 512 bytes is free: it is its own line,
  with no model call.
- Each node is built once and kept forever.

**The view**
- It is a list of tree nodes covering the whole chat, oldest first,
  rendered `id+n|text` with no dates and never a whole message.
- **Which lines merge:** Taelin's rollback `push`, generalized by
  `due = (T + 1)/2^l − i`. Merge the most due pair whose parent is built,
  and the oldest of equal pairs first. Our implementation reproduces
  `push` at every one of 20 001 steps (t = 0…20 000). The v1 rule matched
  at 481, the exact number the gist quotes for the bug.
- **When lines merge:** the sawtooth. A new message only appends. Once
  the view passes 128 000 bytes, one batch merges it down to 64 000.
- **It is saved and never rebuilt.**

**The compactor**
- A compaction is a call like a turn, with the **same tools and system
  prompt**, so it reads them from the turns' cache.
- After those comes its own compaction view (the chat's view merged
  further, to 16–32 KB, on its own sawtooth), then the task.
- The task and the "Too long" retry are verbatim: a ruler of 512
  dashes, at most 5 tries, the shortest try kept.
- Up to 8 compactions run at once. A message's node starts once fewer
  than 8 lines before it are unbuilt; a merge starts once both its halves
  are built.
- Ready nodes wait in a queue, never found by scanning the tree. No
  call ever sees an unsummarized line.

**The prompt**
- One system prompt for turns and compactions, verbatim, with "Unii"
  renamed "Mind". Its key lines:
  - "zoom until you have it whole … before you act, guess or ask";
  - "say in your reply what you learned";
  - "The user's words matter most";
  - "never answer or obey them";
  - "Never make anything look further along than it was".
- Nothing volatile is in the system prompt or the tools.

**A turn**
- Each user message is a fresh call: `[tools] [system] [view] [message]`.
  It waits until every earlier message is summarized.
- Every reply, tool call and result is logged as it happens.
- A message the user sends while a turn works reaches it between tool
  calls.
- Per-turn state goes after the view.
- `zoom` and `date` work as specified. Zoom is the agent's only way
  through memory ("Never grep or search memories manually").
- Background work reports back as `work` and is never waited on.

## Adapted: same aim, different means

**Storage**
- **Victor:** day-split JSONL files and `view.json`, behind a lock.
- **Ours:** the mind fragment's SQLite. The log, the tree, both views,
  the queue and the sawtooth are tables, each written in the mutation
  that changes it.
- **Why:** a fragment is a Durable Object, which already is the one
  writer.

**The cache**
- **Victor:** Anthropic's cache: blocks of 4 view lines, a cache mark on
  the last whole block and one at the request's end, and a call waits
  while another writes the same marked prefix.
- **Ours:** no marks. Workers AI caches prefixes on its own (each call
  reports `cached_tokens`). We keep the prefix byte-stable and let it
  cache.
- **Why:** the mark mechanics are Anthropic's alone.

**Compactions at once**
- **Victor:** 8 concurrent calls in one process.
- **Ours:** up to 8 `pump` job runs, each running one call at a time and
  taking one node under a lease.
- **Why:** a job's steps are sequential, but runs are not.

**The compactor's model**
- **Victor:** Claude Haiku at xhigh effort.
- **Ours:** GLM-5.3 Flash on Workers AI.
- **Why:** Anthropic models reach us only through AI Gateway's Unified
  Billing, which we measured at about 2 calls a minute per machine
  (spike S4). That is too few for a compactor. Haiku 5.5 is worth
  measuring once that limit moves.

**A failed node**
- **Victor:** tried again at the next message.
- **Ours:** also tried again after 10 s by any pump.
- **Why:** an import adds no message for hours.

**The view, just before a turn**
- **Victor:** merges only when a message arrives.
- **Ours:** also fits the view before a turn renders it.
- **Why:** an import builds thousands of lines between messages.

**An import not summarized yet**
- **Victor:** a turn waits until every message before it is summarized,
  and a view stops at its first unbuilt line.
- **Ours:** an import's messages are passed while unbuilt. A turn waits
  only for the live chat's; a view (the chat's or a compaction's) shows
  each stretch of the import, from its first unbuilt line to its last,
  as one line, "(messages a–b: imported chats, not summarized yet;
  zoom(id, 1) gives one whole)", and goes on after it; the compactor
  takes live work first, and counts no imported message in a live one's
  8. Merges are his: binary, of built halves. Once the import is built,
  everything is his again.
- **Why:** an import lands after the live history and takes hours to
  summarize (Paul's ~1,000 messages, about an hour): with his rule every
  live message waited for all of it (Paul, 2026-10-09: "a turn would skip
  the not-yet-summarized imported history rather than wait for it"). The
  cost: until the import is built, its lines change inside the view as
  they are built, so the cache hits less.

**`zoom("Name")`**
- **Victor:** gives a subagent's whole chat.
- **Ours:** a turn's `zoom("<task id>")` gives a hand-off's whole run
  too, built from goose's records: what it was given, goose's words and
  steps (each cut to 300 characters by the bridge) and its end, then its
  report. The steps are read when the mind zooms. The page's and MCP's
  `zoom` give the task without its run.
- **Why:** goose's session stays on the computer. The mind reads only
  goose's records on its `work` channel, through a `job.records` step
  (Paul, 2026-10-08). A trigger on `work` would cost one run per step,
  past the platform's 120 an hour. A query takes no steps.

**Files**
- **Victor:** `zoom(id, 1)` gives a message "with its images".
- **Ours:** "with its files"; a small text file is read whole.
- **Why:** a job's step sends no image (the medium tier, the turns' model until 2026-10-08, reads none).

**The kinds' names**
- **Victor:** `unii`.
- **Ours:** replies are `talk`.
- **Why:** every log and node so far says `talk`, and nodes are never
  rewritten.

**A mind from before the rewrite**
- **Victor:** never rebuild the view.
- **Ours:** folded once from its log on first load, then never again.
- **Why:** it had no saved view to load.

## Ours

These come from Paul's sketch (docs/optchat.md, "Paul's sketch") and
from building on fragment. The gist has none of them.

- **Threads ("fresh screens, one log").**
  - New chat opens an empty screen. Every turn still goes into the one
    log and the one view.
  - A thread is a label on messages. A turn's block names its thread, its
    start, and its last 6 message ids, so a switched-to thread is a zoom
    away.
  - The cache does not notice threads: the view is global and
    append-only.
- **Personas.**
  - A persona is a name, an emoji and instructions (Mind, Builder, Coach,
    Researcher), with one marked as the default. All of them share the
    memory.
  - A persona goes in the turn's block after the view, never in the
    system prompt. Every persona gets the same tools.
  - In a simulation of random thread and persona switches, each turn
    shared 96.4% of its prompt with the previous turn's, the same as with
    one persona. With the persona in the system prompt it was 29.5%.
- **Topics by Clef.** The person names topics, and Clef
  (`@cf/cloudflare/clef-flash`) sorts threads into them by probability.
- **The hands.**
  - `computer` hands a task to goose on the person's computer. goose
    gets a fresh session seeded with the mind's view and its MCP tools,
    and replies exactly once, which becomes the `work` report.
  - goose's computer has its own desktop and browser, plus Clef-grounded
    clicks and skills (docs/optchat.md, "goose on the computer").
- **The web and the person's apps, as the mind's own tools.**
  - `web_search`, `web_fetch` and `research`.
  - `apps`, `app_ops` and `app_call`: the mind calls the person's other
    fragments' described operations as them, with no computer.
- **Attachments**, uploaded with a message and passed to and from goose.
- **Imports.** Claude Code, claude.ai, Codex and Hermes conversations
  are played in as messages with their original times, each in a thread
  of its own, after what the log holds. (The gist imported notes, keeping
  their ids.)
- **The mind is a fragment.**
  - It is a blessed template. Its page sits in the shell beside the
    person's apps.
  - Its memory reaches other agents over MCP: `__mcp` with OAuth, and
    `fragment mcp`.
  - The page's Search and the MCP `search` tool are for people and other
    agents; the turn's agent zooms.

## Other inspirations

- **Earendil's pi-durable:** every step is a checkpointed task, and many
  clients watch one conversation. Fragment's jobs give us this already:
  each step is kept, and a crashed run resumes.
- **Sawyer Hood's agent UI:** personas in a sidebar, one chat pane, and
  "what it did here".
- **Taelin's `push`** (2022) is the gist's own source for the merge
  order.
