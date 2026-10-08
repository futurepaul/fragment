# optchat: one memory, every chat (spike)

Status: **a spike on branch `claude/optchat`**, deployed as the preview
`claude-optchat.finite.place`. It is a draft PR and is never merged. Paul
asked for it on 2026-10-07: a fork of fragment that removes Hermes,
goes back to goose as the agent, and rebuilds the personal agent around
OptChat's memory design.

This document is the contract every part of the spike builds against:
the template, the platform additions, the goose image, and the MCP
server. Where it disagrees with docs/cloudflare-v1.md, it wins on this
branch only.

## Paul's sketch (2026-10-07)

- There's one chat. Every chat running anywhere gets the same optmem
  context, and every turn starts with a fresh context.
- Importing your chats from other providers is easy (future).
- Auto topics: the user creates topics; clicking one shows every chat
  related to it. Categorization is by Clef (`@cf/cloudflare/clef`,
  Cloudflare's decision model on Workers AI).
- Wherever you see a snippet of an old chat, you can expand the view or
  context.
- New chats are free and fun, and you never have to go back to them.
  Everything you say is visible to the next chat too, and gets put into
  topics automatically.
- "Bots" are personas: your chat plus a persona and the skillset it
  takes on. All of them share the same memories. You pick a default
  persona.
- Your optmem is a fragment, and a fragment MCP talks to it, so you can
  pull it into any agent, read-only or read/write.
- Your list of old chats shouldn't feel like email anymore: great
  search, optmem compression, materialized views, auto categorization.
- It should feel as good and simple as Grok Bot, Muse or Dot, with the
  power of optmem and fragment.
- As in the original goose design, the main agent chat lives in a
  fragment and hands off to goose on a computer for computer work.

He chose (2026-10-07):
- a branch in this repo, deployed as a `claude-*` preview;
- Clef for topics;
- "fresh screens, one log": New chat opens an empty screen (a
  *thread*), and every turn goes into the one log.

Inspiration:
- OptChat's spec (VictorTaelin's gist 91837951…, kept as
  `~/dev/finite/optchat-spec.md` on the build box; gist.github.com/VictorTaelin/91837951a5ce5b38f341ec1ba1df6449). **Follow it exactly
  unless this document says otherwise.**
- Earendil's pi-durable: every step is a checkpointed task, many clients
  watch one conversation, and a handoff resets context. Fragment's jobs
  already give us checkpointed steps.
- Sawyer Hood's personal-agent UI: personas in a sidebar, one chat pane,
  and a "what it did here" panel.

## The shape

```
 you (any device) ──say──▶ mind.<you>  (a fragment, blessed template `mind`)
                            │  SQLite: log, tree, threads, topics, personas
                            │  job `heard`: settle → turn loop (ai.text + tools)
                            │  job `pump`: the compactor (ai.text, cheap tier)
                            │  job `classify`: topics (ai.decide, Clef)
                            │
                            ├─ chat/work ─▶ goose on your computer (bridge, runtime `goose`)
                            │               fresh ACP session per hand-off,
                            │               seeded with the mind's view
                            │
                            ├─ mind--<you>.<zone>/__mcp (OAuth) ─▶ claude.ai, ChatGPT, Claude Code, …
                            └─ `fragment mcp mind` (stdio) ─▶ any agent with a shell (Claude Code, goose, …)
```

- **The mind** is the one memory and the main agent. It is a blessed
  template (`templates/mind`), so the platform holds none of its logic,
  and every person's mind runs the same release.
- **goose** is the hands. It runs on the person's computer (Containers),
  takes hand-offs as turns through the bridge, and reports back. Each
  hand-off is a fresh goose session whose first message is the mind's
  view, so goose knows what the mind knows.
- **The MCP servers** serve any fragment's described operations as
  tools, by one rule (`fragment_core::mcp`): the fragment's own `__mcp`
  over HTTP, for a client its person connects with OAuth (issue #232's
  PRs #238 and #240, merged here), and the CLI's `fragment mcp
  <fragment>` over stdio. Pointed at your mind, either gives any agent
  the same view, zoom, date and search, and `note` when you let it
  write ("Connect another agent", below).

## The mind template (`templates/mind`)

`fragment.json`:

```json
{
  "kind": "mind",
  "channels": {
    "say":  { "read": "editor", "post": "editor", "signedIn": true },
    "log":  { "read": "editor" },
    "chat": { "read": "editor", "post": "editor" },
    "work": { "read": "editor", "post": "editor" },
    "sort": { "read": "editor" },
    "compact": { "read": "editor" }
  },
  "triggers": [
    { "channel": "say",  "from": "person", "run": "heard" },
    { "channel": "chat", "from": "agent",  "run": "hands_said" },
    { "channel": "sort", "run": "classify" },
    { "channel": "compact", "run": "pump" }
  ],
  "storage": { "maxBytes": 1073741824 },
  "capabilities": ["owner"]
}
```

The `operations` are listed below. Its members are the owner and the
goose agent, an editor. The shell makes it `members` (private).

- **`say`**: the person's messages, `{text, thread, persona?,
  attachments?}`, posted by the page (`fragment.post("say", …)`).
  `thread` is the page's id for the screen, `t_` plus 16 hex. `persona`
  is the persona chosen for that thread. `attachments` are files the
  page uploaded to the mind first (`fragment.blob(file)`), at most 8,
  each docs/chat-records.md's ATTACHMENT, `{sha256, name, type, size}`
  ("Attachments", below). A message is words, files, or both.
- **`log`**: what the mind publishes from its mutations, for pages to
  follow live (below, "Records on `log`"). It has no `post` role, so it
  is never trimmed. The main agent's streaming draft is on `log` too,
  with draft turn `turn:<thread>`.
- **`chat` and `work`**: the hand-off lane, in docs/chat-records.md's
  contract unchanged. The mind's jobs publish a task to `chat` as a
  message. The goose agent, its lead, answers on `chat` and `work`
  exactly as an agent answers a person in a chat fragment.
- **`sort`**: the app's alone. `topic_add` publishes `{topic}` there, and
  its trigger starts `classify`: a mutation starts no job.
- **`compact`**: the app's alone. `import` publishes `{at}` there when no
  pump is at work, and its trigger starts `pump` ("Importing chats").
- **`storage`**: the mind declares the most an app may (1 GiB; docs/api.md,
  Apps): a long imported history outgrows the platform's 16 MiB.
- **`capabilities`**: `owner`, so its turns use the person's other apps
  as them ("The user's apps", below).

### The log, the tree, the view: OptChat's spec, in SQLite

```sql
log(i INTEGER PRIMARY KEY, kind TEXT, text TEXT, at INTEGER, thread TEXT, persona TEXT, task TEXT, attachments TEXT)  -- attachments: JSON, null with none
node(l INTEGER, i INTEGER, text TEXT, PRIMARY KEY (l, i))     -- the tree; never rewritten
thread(id TEXT PRIMARY KEY, title TEXT, persona TEXT, started INTEGER, last INTEGER, first_i INTEGER, last_i INTEGER)
topic(id TEXT PRIMARY KEY, name TEXT, description TEXT, made INTEGER)
thread_topic(thread TEXT, topic TEXT, p REAL, PRIMARY KEY (thread, topic))
persona(id TEXT PRIMARY KEY, name TEXT, emoji TEXT, instructions TEXT, hands INTEGER, made INTEGER)
task(id TEXT PRIMARY KEY, thread TEXT, i INTEGER, text TEXT, seq INTEGER, turn TEXT, state TEXT, report TEXT, steps TEXT, started INTEGER, ended INTEGER)  -- steps: '[]', unread (goose's are on work)
kv(k TEXT PRIMARY KEY, v TEXT)                                 -- default persona, about-me, turn lock, queue
log_fts USING fts5(text, content='log', content_rowid='i')    -- search
import(source TEXT, conv TEXT, thread TEXT, n INTEGER, at INTEGER, PRIMARY KEY (source, conv))  -- an import landed conv's first n messages
```

- `kind` is the spec's set: `user`, `talk`, `tool`, `echo`, `note`.
  Hand-off reports are `user` messages starting `[<task id>] `, which
  the compactor tags `work:`.
- **The spec's constants hold:** NODE 512, VIEW 128 000, TRIES 5,
  CAP 30 000, and the cut-at-limit retry. JOBS is up to 8 nodes in one
  pump step round, built in batched calls ("The compactor"). RETRY is on
  the next pump. A node a stubborn model wrote past twice NODE is cut
  there.
- **Prompts:** COMPACT, MASTER, VIEW_DOC and the subagent prompt are
  verbatim from the spec, with "OptChat" replaced by "Mind".
  - MASTER's "Use subagents only when the user asks" becomes: check on
    the web what may have changed or what it is unsure of (`web_search`
    to find pages, `web_fetch` to read one, `research` for a question
    that needs several sources), saying where it came from; use
    `computer` for real computer work (files, code, running programs,
    anything that needs the user's accounts); use the user's apps
    (fragments) directly, as the user (`apps`, `app_ops`, `app_call`: "add
    milk to my todo", "what's on my board"); the computer has the
    fragment CLI and its skill, so only making an app or changing an
    app's code is handed to it. Answer everything else yourself.
  - The system prompt is MASTER + VIEW_DOC + the persona's
    instructions + the person's about-me. It is byte-identical across
    turns for one persona, with no dates.
- **The view is folded incrementally** (spec §5.2) and never stored. It
  is rebuilt from the log at the app's first call (append + fit per
  message) and kept on the App instance, which the facet may evict at
  any time. Each mutation that changes the log or the tree bumps
  `kv.rev`, and an instance whose view is of another `rev` (a mutation
  rolled back) folds it again.
- **A job reads the instance to build a step's arguments** (the
  compactor's context, Clef's state): they would fill a run's 4 MiB of
  answers if they were a step's answer, and a past step's arguments do
  not matter. Its control flow follows only its steps' answers. A
  turn's view is the exception: a step's answer (`view {upto}`), so every
  call of the turn sees the same one.
- **The database's cap is debt.** The mind declares 1 GiB: the
  platform's 16 MiB holds a few tens of thousands of messages, and an
  imported history can be more. No meter counts it, and the folded memory
  keeps every node's text on the instance (docs/technical-debt-ledger.md).
  A message over
  30 000 characters is capped at logging, as the spec caps tool results,
  keeping head and tail.

### Turns (job `heard`, triggered by `say`)

The spec's §7, as a job:

1. `hear`, a mutation: log the message (`user`) with its files (their
   small text ones read first: "Attachments"), touch its thread (making
   it, titled from the first line, or the first file's name, on first
   use), publish it on `log`, and push it on the queue. If a turn is
   running, stop there: the running turn takes it next.
2. `turn_begin`, a mutation: take the turn lock (it expires 15 minutes
   after its last touch), then take the queued messages of the oldest
   thread waiting. It answers `tail`: where the turn's view stops, before
   the newest run of messages still waiting. They go whole as block 2,
   as the spec renders the view before it logs them.
3. **Settle:** while the view has an unbuilt part before `tail`, build it
   inline, level 0 in order, a batch at a time ("The compactor"), and
   merges too while the view is over VIEW (a backlog an import left). It
   is normally a no-op, because the background pump ran after the last
   turn. A node a pump holds (a lease from `pump_plan`) is waited for,
   not built twice. A level-0 node that fails 3 times, 10 s apart, ends
   the turn with an error, its messages logged and unanswered. A settle
   past its budget (96 steps) puts the messages back first in line and
   hands them to a fresh run.
4. Render the view (`view {upto: tail}`). Then loop, at most 40 model
   calls a turn:
   - Call `job.ai.text({model: "medium", messages, tools, draft:
     {channel: "log", turn: "turn:<thread>"}})`. The messages are
     `[system, user: [view, texts joined]]`, then the turn's steps.
   - Log each reply `talk`, each tool call `tool` (name and JSON input),
     and each result `echo` (capped). Publish each on `log`.
   - Run the tools: `zoom`, `date` and `search` are queries; the web's
     are fetches ("The web"); the apps' are the owner's steps ("The
     user's apps"); `computer` opens a hand-off. At most 8 run
     per answer; past 24, an answer's calls are dropped (a mutation
     publishes 64 records). Past 512 KiB of conversation, an answer's
     next tool is answered "this turn has read all it can hold" and the
     next call is the last.
   - Stop when the model answers with no tool calls.
   - The last call offers no tools (`tool_choice: "none"`): the 40th, or
     one past 512 KiB of conversation (a step's arguments travel in a
     Workflow step of 1 MiB), or near the run's 256 steps or 3 MiB of
     answers.
5. `turn_end`, a mutation: release the lock, and classify the thread
   when the mind has topics. If messages are queued, go to 2 in the same
   run; otherwise start the job `pump`. A run takes at most 4 turns, and
   starts one only below 64 steps; past that, `heard {resume}` takes the
   rest in a fresh run.

Tools (descriptions verbatim from the spec where it has them):

- `zoom(id, n)` and `date(id)`, as the spec defines them.
- `search(q, limit?)`: FTS5 over the log. It answers `id+1|kind:
  snippet` lines, newest first, at most 20.
- `web_search(q, limit?)`: numbered results, each `title`, its URL and
  a snippet (6 unless named, at most 10), then what failed on the way.
- `web_fetch(url)`: a page's `# title`, its URL, and its readable text
  (markdown-ish), capped at CAP, head and tail, as every result.
- `research(question)`: an answer with numbered sources.
- `apps()`, `app_ops(fragment)`, `app_call(fragment, op, input)`: the
  user's apps ("The user's apps", below).
- `computer(task)`: hand work to goose on the person's computer. Its
  description says it has the fragment CLI and its skill, and makes the
  user's apps and changes their code (to use one, `app_call`). It
  answers `[<task id>] started` at once.
  The report arrives later as a `user` message `[<task id>] <report>`,
  which starts a turn of its own when none runs (MASTER: never wait or
  poll for it). It is offered only when the persona has `hands` and the
  mind has an agent member. The turn's files go with it.

Every persona has `zoom`, `date`, `search`, the web's three and the
apps' three, in that order, then `computer` with hands: the same tools
every turn, so the prompt cache holds.

### The user's apps (`apps`, `app_ops`, `app_call`)

Paul (2026-10-07): "the in-fragment agent doesn't know about fragment
operations, which is something the old design had" (the goose agent cut
in 1dd80a4f had a tool per operation of the fragments it was in, and
`platform__list_fragments`, `platform__operations`, `platform__call`).
The mind uses the person's apps itself, as them, through the platform's
`owner` steps (docs/api.md, Jobs and triggers; "Platform additions" 6),
and hands goose only the making of an app or a change to its code.

- **`apps()`**: one `job.owner.fragments()` step, read once a turn (a
  later `app_ops` in the turn reads the same answer). The person's other
  fragments (the mind is never among them), each `- <name>: "<title>"
  (<kind>, <their role>) <url>`, then its operations, each `  <op>
  (<kind>): <its description's first 160 characters>`; one with none
  says the computer changes it.
- **`app_ops(fragment)`**: that app's operations in full: each one's
  description and its input's JSON Schema.
- **`app_call(fragment, op, input)`**: one `job.owner.call` step. Its
  echo's first line is `<fragment> <op> at <url>`, then the result: a
  string `text` as it is, else its JSON (the MCP servers' rule); capped
  at CAP like every result. A refusal (no such app, an operation it does
  not describe, the person's role, the schema) is the echo, `Error: …`,
  for the model to act on.
- **What only the platform decides:** which operations are tools (the
  described ones, by `fragment_core::mcp`'s rule, mutations and jobs
  too); the role (the person's own, in fragments they are a member of);
  each call once (its id is the turn's run and step: a retried step, or
  a replayed run, applies nothing twice); and the gate: the mind's release
  declares `owner`, and the mind is `members` with no member but its
  owner and their own agents. Shared with anyone else, `apps` and
  `app_call` answer why, and use nothing.
- **What the app sees:** `call.principal` is the person (a todo's item
  is "by" them), `call.agent` the mind's key; its `ops` records name the
  mind's key, and its `events` say `fragment.called`, "add job:… by id:…
  through mind.paul".
- **The page:** a turn's steps row that used apps reads "Used todo: add"
  ("Using" while it runs), each app's label a link to its canonical URL,
  which the shell opens beside the mind; one that only listed them,
  "Looked at your apps".

### The web (`applib/web.mjs`)

The web tools are a few steps of the turn that calls them, all
`job.fetch` (the app's one way out). Their results are logged as any
tool's (`tool`, then `echo`, capped); `research`'s own searches and
reads are its steps, not the log's, as a subagent's are (spec 9).

- **Providers, by the secret that names them.** The owner sets a key as
  one of the mind's secrets (`fragment secret set <mind> NAME`, or `PUT
  /api/f/<mind>/secrets/<NAME>`). The app never sees a secret nor which
  exist: a fetch names one as `{{NAME}}` in a header, and the platform
  fails the step "no secret named NAME" when it is not set. So the
  keyed providers are tried in order, and a secret found missing is
  passed over for the rest of the turn (one failed step each, at most,
  per turn).
  - `web_search`: Perplexity's Search API (`PERPLEXITY_API_KEY`, `POST
    api.perplexity.ai/search`), Brave Search (`BRAVE_API_KEY`), Tavily
    (`TAVILY_API_KEY`), then **with no key** DuckDuckGo's HTML page
    (`html.duckduckgo.com/html/`, read for its results, its ads left
    out). When DuckDuckGo gives nothing, Wikipedia's search, which
    answers any server; its results say they are Wikipedia's alone and
    name the keys. DuckDuckGo answers a datacenter's address with a
    "prove you are human" page (2026-10-07: its duck CAPTCHA, fetched
    from a datacenter; a home address gets results), which is read as
    such, and DuckDuckGo is passed over for the rest of the turn: on
    Workers the no-key search is, in practice, Wikipedia's. A keyed
    provider that fails (a refused key, a 5xx) is noted and the next is
    tried.
  - `research`: with `PERPLEXITY_API_KEY`, one call of Perplexity's
    `sonar` (`POST api.perplexity.ai/chat/completions`), which searches
    and cites itself; its sources are its `search_results` (or
    `citations`). Otherwise a `web_search` of 5, the first 3 pages that
    read (each cut to 12 000 characters), and one `cheap` call
    (`RESEARCH`) that answers from them alone, citing them as `[n]`.
- **`web_fetch`** sends a browser-like `user-agent`, follows at most 3
  redirects itself (a step's fetch answers them), and asks a page over
  a fetch's 1 MiB again with `range: bytes=0-524287` (honoured by some
  servers). A Wikipedia article (`<lang>.wikipedia.org/wiki/…`) is read
  through Wikipedia's API instead (`prop=extracts`, plain text, its
  headings as `#`s): its page is up to megabytes of HTML, sent whole
  whatever range is asked (United States: 2.9 MB, past a fetch; its
  text, 94 KB), and Wikipedia is the no-key search's source on Workers.
  HTML becomes text: its `<main>` (else its `<article>`s, else
  its body), less scripts, styles, media, `nav`, `footer`, `aside`, and
  elements hidden or marked as chrome by a role or a class (menus,
  dropdowns, navboxes, a Wikipedia section's "edit"); headings as `#`,
  list items as `- `, links as `[words](absolute url)`, `<pre>` fenced,
  table cells split by ` | `. Text types pass as they are; anything else
  (a PDF, an image) is an error that says to hand it to the computer.
- **Budgets.** A fetch's answer is kept whole in the run's 4 MiB of
  answers, so one is taken only while the run has 1 MiB and a margin
  left; a web tool takes a step only while 3 are left for its answer's
  log, a last call and its log. Either way the tool answers why it
  stopped, and the agent answers with what it has.

### Attachments (`applib/files.mjs`)

- **In.** A `say` record's `attachments` are the mind's blobs. The
  `heard` job reads each small text one (a type `text/*`, JSON, XML,
  YAML…, or a name like `.md`, `.csv`, `.py`; at most 64 KiB, and 128
  KiB a message in all) with `job.blob` (Platform additions), and
  `hear` logs them with the message: the log's `attachments` column,
  `[{sha256, name, type, size, text?}]`, `text` capped as a message is.
- **The memory reads a message with its files**: the tree's level 0
  (the compactor), `zoom(id, 1)` and a turn's new messages are its words,
  then each file as `[file: <name> (<type>, <size>)]`, a read one fenced
  whole below it. So the agent sees every file's name, type and size,
  and a small text file's words. `search` indexes the words alone.
- **Out.** `msg` records, and `thread`, `context` and `node`'s
  messages, name a message's files without their text (`attachments`:
  on a record only when it has some; on a query's message always, `[]`
  with none). `export` carries the text too.
- **To the hands.** A `computer` hand-off's `chat` record carries the
  turn's files (its taken messages', at most 8): goose's bridge
  downloads them as local files for the runtime. goose's one reply may
  carry files (its bridge uploads them to the mind); `hands_said` reads
  the small text ones the same way, and they are the report message's
  `attachments`. A reply of files alone reports `(files)`.
- **Images** are named, not seen: the route's `vision` model is no tier
  a job's step may name. Hand an image to the computer.
- The records that name a file keep its blob (docs/api.md, Blobs): `say`
  (its newest 10 000), `log` (never trimmed) and `chat`.

### The compactor (job `pump`)

Spec §4, as a job. Each round asks `pump_plan` (a query that writes
its leases in `kv`) for the nodes that are ready (spec §4.1 rules 1–3):
a level-0 batch and up to 8 merges. It builds them with
`job.ai.text({model: "cheap", …})` steps (the spec's two-block input,
SCALE, the retry loop): the level-0 batch, then the merges. One node
alone is the spec's call exactly; two or more go in one call (below).
Each result goes to `node_built` (one) or `nodes_built` (a batch),
mutations where the first write wins, which refit the view. Rounds
repeat until none is ready. A failed node is left for the next pump.
Past 30 rounds or the run's budget with work left, a fresh `pump` takes
the rest.

**Batched calls (a deviation from the spec, 2026-10-07).** The spec runs
JOBS calls at once, one node each. A job's steps run one at a time
(cell/platform.mjs), so here each call would wait for the one before,
and each sends the whole view (up to 128 KB) as its context: an imported
history of thousands of messages took hours and two calls a message.
So:
- **Level 0:** a call builds `first`'s node (the one rule 3 makes ready)
  and each later message not built whose line is over NODE, up to 8 in
  all, at most 192 KiB of them, within 64 messages of `first`
  (`M.batch`). The context is the view before the first of them, as the
  spec's; the step is SCALE, then the messages in order, each whole
  under `--- n ---`, a short one between them shown as the line it
  already is (`--- a line already ---`), then "Answer with exactly k
  lines … each starting with its number and `) `". A later message of a
  batch sees the earlier ones whole instead of their lines.
- **Merges:** up to 8 ready merges in one call, each its two lines under
  `--- n ---`. The context is the view before the earliest end among
  them, so no merge sees past its own stretch (a later one sees less
  than the spec's context, its own lines given whole in the step).
- **Ids in the output:** the spec keeps ids out of a compactor's input
  because a model copies them into its line. A batch needs numbers to
  match lines to nodes; they are stripped (`M.batchLines`), and the view's
  lines still carry none.
- **The size rules hold line by line:** a line over NODE is asked again
  in the same conversation, cut where the limit falls, with any line
  that did not come; after TRIES calls each line is its shortest try
  (`M.batchTry`). A line that never came, or a call that failed, fails
  its node alone. Free nodes stay free: never sent to a model.
- **Merges alongside level 0:** a round builds its merges after its
  level-0 batch (the spec's pump runs them alongside), and a turn's settle
  builds merges while the view is over VIEW: with level 0 far behind
  (an import), the view would otherwise grow past its budget, and every
  call's context with it.

### Importing chats (`import`, `fragment mind import`, the page's upload)

Paul (2026-10-07): "chat upload … at least claude and codex and hermes
sessions … import should simply 'play' the chats through the memory
system so they get added just like any other messages". The spec's §10:
old chats become messages, "the user's messages and the agent's final
replies, without repeated pastes and tool noise", and the compactor
builds the tree over them like any other.

- **`import`**, a mutation (editor): one part of one conversation, its
  messages `from` on, appended to the log in order as `user` and `talk`
  with their **original times** (`at`), in a thread of the conversation
  (made on its first part: `t_` and 16 hex of FNV-1a 64 over its source
  and id, titled from its title or its first words, `started` its start,
  the default persona's), publishing no `msg` records (one `{type:
  "import", source, conversation, thread, n, total, T}` a part, and
  `thread` when it is made). No turn starts: an import plays history and
  asks the agent nothing. `kv`'s `import` table keeps how many messages of
  each conversation landed: a part already in changes nothing (a retry,
  a rerun), one ahead of them is refused (parts go in order), and one
  that overlaps them appends only its new messages (a conversation that
  grew). When no pump planned work in 3 minutes (and none was started in
  the last minute), it publishes `{at}` on `compact`, whose trigger runs
  `pump`.
- **Deviations from the spec's import:** the spec imported keeping ids;
  this log already has messages, so imported ones land after them (the
  log stays append-only, and an import may interleave with live chat, a
  thread's messages then not contiguous). Their times are their own, so
  `date` answers when they were said, and `threads` lists them by when
  they were last said, not when imported.
- **`fragment mind import <paths…> [--from claude-code | claude-export |
  codex | hermes | auto] [--since DATE] [--limit-conversations N]
  [--dry-run [--show N]] [--mind <name>] [--no-wait]`** (cli/src/import.rs
  reads, cli/src/mind.rs sends):
  - **Formats:** Claude Code's sessions (`~/.claude/projects`; a
    `subagents/` folder skipped; a forked or resumed session's copied
    records are its original's, merged once); claude.ai's export
    (`conversations.json`); Codex's rollouts (`~/.codex/sessions`; its
    first format, its `response_item`s and `event_msg`s, and its turn
    items; a subagent's or a guardian's rollout skipped); Hermes's
    `hermes sessions export` lines (a compaction's continuation joined to
    its session; rows undone, summaries, injected memory, skill scaffolds
    and agent sessions dropped) and its older `session_<id>.json`
    snapshots and `<id>.jsonl` transcripts. Its `state.db` is SQLite,
    which the CLI does not read: it names it and says to export it.
  - **What goes:** the person's words (no harness text: system reminders,
    slash commands without arguments and their output, task
    notifications, injected environment and AGENTS.md, a Codex Desktop
    message's attached context above "My request for Codex") and each
    turn's final reply (the agent's last text after its last tool call;
    a turn the harness started, a subagent's report, keeps its reply and
    not its words). Tool calls, results, reasoning and empty turns go; a
    user message of 1 KiB or more repeating an earlier one (a paste) goes;
    each message is capped at CAP as the mind caps it. Oldest
    conversation first.
  - **`--dry-run`** counts conversations, messages and bytes by source,
    and estimates the compactor's work by simulating the tree (a free
    node for a line within NODE, a free merge for two that fit; every
    other a call): level-0 calls, alone and batched; merges; tokens (the
    view as each call's context); cost at the cheap tier's prices in the
    default price book, all context cached or none; and hours at ~8 s a
    call. `--show N` prints the first N conversations a line a message.
  - **An import** asks `imported` for each conversation, sends the rest
    in parts (at most 64 messages and 192 KiB), then follows `status`
    every 15 s until nothing is unbuilt or ready, calling `pump` when
    none is at work (a pump chain ends at the platform's 16 hops); it
    stops after 5 such starts with no progress. Ctrl-C and a rerun
    resume.
- **The page's upload** (`site/import.js`, `mountImport(container)`):
  one `conversations.json` or one Claude Code or Codex `.jsonl`, parsed
  in the page by the same rules, sent the same way, then the compactor
  followed through `status`.
- **While an import compacts**, a turn waits (`settling`): no call sees a
  placeholder (spec §6), and the imported messages come before it. Its
  settle builds alongside the pump; past its budget it hands on (a chain
  of at most 16 runs), and the next message resumes it.
- **The archive on the build box (2026-10-07, dry run):** Paul's Claude
  Code and Codex sessions from his Mac and this box, 377 conversations,
  15 437 messages, 12.2 MB: 6 839 messages over NODE (855 batched calls;
  6 839 one a call), 12 029 merges needing a call (1 504 batched), about
  135M tokens in (124M of them the view as context), $7.67 to $22.67 at
  list price, about 6.6 hours.

### Topics (job `classify`)

Clef, through `job.ai.decide({model: "clef-flash", state, questions})`
(platform, below).
- **State:** the thread's title and its messages as `kind: text` lines,
  each at most 1 KiB, at most 48 KiB in all, newest kept.
- **Questions:** one `noul` per topic, by the topic's id, at most 64 per
  call: `{<topic id>: {type: "noul", instructions: "Is this conversation
  about <name>? <description>"}}`.
- A thread is in a topic at p ≥ 0.6. The answers go to `thread_topic`
  through `topics_set` and are published on `log`.
- `topic_add` starts `classify` for the 100 newest threads (its record
  on `sort`). A turn's end classifies its thread.
- Clef only sorts. Topic names come from the person, or from
  `topic_suggest`, a job: one cheap `ai.text` over the view that answers
  up to 8 names, which the page offers and the person accepts.

### Hand-offs (job `computer` → `chat` → goose)

1. **The task.** The tool's step publishes
   `{text: "<task>\n\n(task <task id>, thread <thread>)", to: [<goose agent id>], attachments?}`
   (the turn's files) on `chat` as the fragment (`job.publish`, which answers the record's
   `seq`); then `task_open` records the task, that `seq`, and the turn
   the agent's bridge gives that record: 24 hex of SHA-256 of `<agent
   fragment>|<mind>/chat/<seq>` (images/bridge `turn_id`; the agent
   fragment's full name from `job.people`). The task id is
   `w<run>-<step>`.
2. **goose's turn.** The bridge admits it as a turn: the agent is the
   mind's lead, and the record is from neither the agent nor `anon:`.
   It claims the turn on `work` (`turn.start`, whose `cause.seq` names
   the task) and posts its steps and end there. It posts **one reply** on
   `chat` (`rp:<turn>:1`): its report, or `(ended: <outcome>: <why>)`
   when it stopped or failed.
3. **The mind takes the reply.** `hands_said` (`chat`'s trigger, each
   record an agent posts there) matches the reply to its task by
   `body.turn`. The first is the report: it puts it on the queue as a
   `user` message, `[<task id>] <reply>`, with the reply's files, and
   starts a turn like `heard` does, in the task's thread. A later part changes nothing. A reply
   that comes before its `task_open` is kept for it. The task's state is
   `done`, or `stopped` or `error` from an `(ended: …)` reply. The mind
   runs nothing for `work`: a page follows goose's steps there itself,
   mapping `turn` to its task, so a hand-off is one triggered run, well
   under the platform's 120 an hour.
4. **Lost.** A task with no reply 30 minutes after it opened is `lost`:
   `tasks` says so, and a turn's start records it and publishes it. A
   reply that comes later still reports.
5. **goose's context.** At the start of each hand-off turn, the goose
   runtime calls `POST /api/f/<mind>/ops/view` (a query) for the
   rendered view. It starts a fresh session whose first message is the
   spec's subagent framing, VIEW_DOC, the view, and then the task.
   Nothing of a session carries to the next.

### Operations (the contract for the page, the MCP server and goose)

Queries are viewer by default. Everything here also needs the mind's
membership, which only its owner and the agent hold. Operations with a
`description` are MCP tools (below).

| op | kind | input → result |
|---|---|---|
| `view` | query (described) | `{upto?}` → `{text, bytes, parts, T, settled}`: the rendered `<chat>…</chat>`, the parts that start before `upto` (all by default) up to the first not summarized yet (no call sees a placeholder); `settled` says none was left out |
| `zoom` | query (described) | `{id, n}` → `{text}`: the spec's zoom |
| `date` | query (described) | `{id}` → `{text}`: ISO time of message `id`, in UTC (the mind knows no time zone) |
| `search` | query (described) | `{q, limit?, thread?}` → `{results: [{i, kind, thread, at, snippet}]}` |
| `note` | mutation (described) | `{text}` → `{i}`: append a `note` (an MCP client's write) |
| `threads` | query | `{topic?, before?, limit?}` → `{threads: [{id, title, persona, started, last, summary, topics: [{id, p}], count}]}`, newest `last` first (`before` is a `last`); `summary` is the text of the smallest built node covering the thread's messages (`first_i` to `last_i`), else its first user line; `count` is its `user` and `talk` messages |
| `thread` | query | `{id, before?, limit?}` → `{thread, messages: [{i, kind, text, at, persona, task, attachments}], more}`; `tool`/`echo` are returned so the page can fold them into a "steps" row; `attachments` are `[{sha256, name, type, size}]` (`[]` with none), each read at `__blob/<sha256>` |
| `context` | query | `{i, before?, after?}` → `{messages}` around `i`, any thread (expand), as `thread`'s |
| `export` | query (editor) | `{after?, limit?}` → `{entries: [{i, kind, text, at, thread, persona, task, attachments}], next}`: the raw log oldest first, the messages after `after` (from the first by default), at most `limit` (500 unless named, at most 2000) and 768 KiB a page; `attachments` with the `text` the mind read of them; `next` is the next page's `after`, `null` at the end (the page's "Export memory") |
| `memory` | query | `{}` → `{parts: [{id, n, text, built}], bytes, T, cut?}`: the view as structured parts (the Memory screen); past 768 KiB its last `cut` parts are left out |
| `node` | query | `{id, n}` → `{children: [{id, n, text, built}]}` or, for n = 1, `{message}` (as `thread`'s) |
| `topics` | query | `{}` → `{topics: [{id, name, description, count}]}` |
| `personas` | query | `{}` → `{personas: [{id, name, emoji, instructions, hands}], default}` |
| `tasks` | query | `{thread?}` → `{tasks: [{id, thread, i, turn, text, state, report, started, ended}]}`, the newest 50; `i` is the `tool` message that opened it; `turn` the agent's (its steps are on `work` under it); `state` is `running`, `done`, `stopped`, `error` or `lost`; `text` cut to 4 KiB and `report` to 16 KiB (the report whole is its message) |
| `status` | query | `{}` → `{turn: {running, thread, since} \| null, queued, unbuilt, T, hands: bool, failing: [{id, n, error, tries}], ready, pump: {at} \| null, view, nodes, import: {conversations, messages, last} \| null, now}`; `hands` is whether the last turn saw an agent member; `failing`, the nodes whose last build failed; `ready`, whether the compactor has a node to build now; `pump`, when a pump last planned work (none: no pump at work); `view`, the view's bytes; `nodes`, the tree's built nodes; `import`, what imports landed |
| `import` | mutation | `{source, conversation: {id, title?, started?}, from, total?, messages: [{role: user \| assistant, text, at}]}` (1 to 64) → `{thread, landed, appended, T}`: "Importing chats" |
| `imported` | query | `{conversations: [{source, id}]}` (at most 200) → `{landed: [n]}`: how many of each conversation's messages are in |
| `settings` | query | `{}` → `{about}` |
| `topic_add` / `topic_remove` | mutation | `{name, description?}` / `{id}` |
| `persona_set` / `persona_remove` / `persona_default` | mutation | `{id?, name, emoji, instructions, hands}` / `{id}` / `{id}` |
| `settings_set` | mutation | `{about}` |
| `stop` | mutation | `{thread}`: the running turn stops at its next step |
| `topic_suggest` | job | `{}` → names, published on `log` as `{type: "suggest", names}` |

The internal mutations (editor, no description) are named here so the
two halves agree: `hear`, `turn_begin`, `turn_touch`, `turn_end`,
`logged` (log one step's messages; it touches the lock and answers
whether Stop was asked), `pump_plan` (a query, editor), `node_built`,
`nodes_built`, `task_open`, `hands_reply`, `topics_set`. The jobs are `heard`, `pump`,
`classify`, `topic_suggest` and `hands_said`.

Seeded personas (a persona's instructions say what it is for; each
is seeded once, so a mind made before one was gains it, and one its
person removed stays removed: `kv.seeded`):
- **Mind**, the default: plain, warm, brief.
- **Builder**: does things on the computer, apps (fragments)
  included; `hands` is on.
- **Coach**: asks one question at a time.
- **Researcher**: checks the web before it answers (`research`,
  `web_search`, `web_fetch`), gives its sources, and says what it could
  not confirm.

### Records on `log`

- `{type: "msg", i, kind, text, thread, at, persona, task,
  attachments?}`, where `text` is at most 48 KiB, cut, and
  `attachments` (when it has files) name them, `[{sha256, name, type,
  size}]`.
- `{type: "turn", thread, state: "thinking" | "settling" | "done" | "error" | "stopped", error?}`.
- `{type: "task", id, thread, state, text, turn, report?}`: as it opens,
  as its reply reports, and as it is found lost. Its live steps are
  goose's own records on `work` under `turn`, which the page follows.
- `{type: "topics", thread, topics: [{id, p}]}`.
- `{type: "thread", id, title}`.
- `{type: "suggest", names}`.
- A draft with turn `turn:<thread>` is the main agent's reply as it
  streams. The `msg` record that follows replaces it.

### The page

One screen at a time, and calm, in the shell's look (Paul, 2026-10-07):
the platform's stylesheet (`__fragment.css`), which is the shell's look;
`mind.css` holds only the mind's own values (its ground, the middle
column's, and a chat's surfaces).
In the shell (`?embed=shell`, docs/api.md "The mind in the shell") the
shell's sidebar is the rail and its topbar the header, and apps open
beside the mind; opened on its own origin the page shows its own rail.
- **Left rail:**
  - the persona switcher at the top (the default marked);
  - **New chat** (an empty screen with the current persona; nothing is
    made until the first message);
  - **Search** (instant FTS, results grouped by thread, each snippet
    expandable in place to its context);
  - **Topics**, with counts, plus "+ topic" and suggestions;
  - **Memory**: the view, line by line, each line expandable down to
    its messages, as the agent sees it.
  - It has no list of chats. Recent threads appear under a small
    "Recent", as one-line summaries rather than subject lines.
- **Center:** the thread.
  - Messages are markdown.
  - The steps of a turn (tool and echo) fold into one "looked at memory
    ×3" row; one that used the person's apps reads "Used todo: add", the
    app's name a link to it.
  - A hand-off is a card that shows goose's live steps and its report.
  - Snippets of other threads (a search hit, a zoom result) are
    expandable.
  - The composer sits at the bottom, with the persona chip, Stop, and
    files (paperclip, drop, paste): each uploaded with `fragment.blob`
    and posted as `say`'s `attachments: [{sha256, name, type, size}]`
    (a message may be files alone); a message's and a report's
    files show as pictures, players or download chips (`__blob`).
- **Right panel** (toggle): the thread's topics, its hand-offs ("what it
  did here"), and the computer's state.
- **Topic screen:** the threads in the topic, each a summary card that
  expands in place.
- **The mobile layout** comes first: the rail is a drawer.
- **Settings** (`site/settings.js`): a screen (`#/settings`), never a
  sheet (Paul, 2026-10-07: "I prefer using the center column rather than
  a modal"). About you; the memory and **Export memory** (the whole log
  as one JSON file, the `export` query paged); **Import chats** (the
  page's upload, below); and **Connect another agent**: the claude.ai
  connector's URL (the mind's origin's `/__mcp`), Claude Code's `claude
  mcp add --transport http`, and `claude mcp add mind -- fragment mcp
  mind.<username>`. In the shell they are the **Mind** section of its
  own Settings, in the middle column: the page alone, framed with
  `?embed=settings` (docs/api.md, "The mind in the shell"), which the
  heading menu's Mind settings opens. On its own origin the rail's foot
  opens the screen.

## Platform additions (generic: no platform code names the mind)

1. **`job.ai.text` takes tools and streams drafts.**
   - **New keys:**
     - `tools`: OpenAI's shape, at most 64 tools and 64 KiB.
     - `tool_choice`.
     - `draft: {channel, turn}`: the call streams, and its text so far
       is put as that channel's draft (as `PUT …/draft` does), at most
       4 a second. The step's poster is the fragment, and the channel
       must be one of the app's.
   - **What it answers** is the old `text` plus `message` (`{role,
     content, tool_calls?}`; reasoning is never returned) and
     `finish_reason`. Kept answers and metering are unchanged.
2. **`job.ai.decide({model: "clef" | "clef-flash", state, questions, images?})`**
   → `{answers, model, usage}`: Clef on Workers AI
   (`@cf/cloudflare/clef`, `@cf/cloudflare/clef-flash`), through the
   model route's transport, priced in the price book (0.24 and 0.09 USD
   per M input tokens), reserved and settled like a text step. The fake
   answers deterministically: `noul` is 0.9 when a word of the question
   over 3 letters appears in the state, else 0.1; `choice` picks the
   first option named in the state.
3. **The blessed template `mind`** (`FragmentKind::Mind`): made with
   `POST /api/fragments {template: "mind", visibility: "members"}`.
4. **Operation `description`** in `fragment.json`: an optional string of
   at most 1024 characters, shown in `status.code.operations`. It makes
   the operation an MCP tool. #240 (merged here) added the same field;
   the two are one now (`OpDecl.description`, `code_ops.description`).
5. **`job.blob(sha256)`** → `{sha256, size, text, cut}`: a job reads one
   of its fragment's blobs, its first 64 KiB as text when they are UTF-8
   (docs/api.md, Jobs). The mind reads a message's small text files
   with it.
6. **`job.owner.fragments()` and `job.owner.call(fragment, op, input)`**
   (docs/api.md, Jobs and triggers; cell/src/owner.rs): a job acting as
   its fragment's owner on their other fragments, for the mind's apps
   tools. Lent by the manifest's new `capabilities: ["owner"]`, which only
   a blessed template's release declares (a fork, or any fragment with its
   own code, is refused it at deploy), and only while the fragment is
   `members` with no member but its owner and their agents (asked at each
   step). The listing is the owner's list (at most 64 fragments, by
   name), each with their role, its canonical URL and its described
   operations (`fragment_core::mcp::described`); a call goes to the target
   behind an internal route, as the owner with their role there, recorded
   as an agent's `for` call is, keyed by the run and the step. The old
   design's `fragments` capability (8edc2783 cut it) was a page's, gated
   by the owner viewing it; this one is a job's, gated by who can reach
   the fragment.
7. **The shell's first run:**
   - it makes the agent (on the default image, goose), assigns it to
     the computer, makes `mind` (members) and adds the agent there as
     an editor;
   - it no longer makes a `<agent>-chat`;
   - signed in with a mind, `/` is the shell with the mind in its
     middle column (2026-10-07: it was the mind full-screen on its own
     origin, which left the apps a link away).

## goose on the computer (`images/goose`, bridge runtime `goose`)

- **The image:** Debian trixie-slim, 499 MB compressed (1.28 GB on
  disk; Chromium is most of it), with:
  - tini as PID 1;
  - the sandbox-shim (docs/computers.md);
  - the bridge (`BRIDGE_RUNTIME=goose`, built for musl);
  - the fragment CLI;
  - goose, built in a stage of its own from `futurepaul/goose` at a pinned
    rev of `fragment/optmem`;
  - git, curl, jq, python3, ripgrep, Node 24.21.0;
  - a desktop per agent (`images/goose/desktop`, `fragment-desktop`):
    Xvnc (TigerVNC, on a Unix socket only), the matchbox window manager,
    and the agent's own Chromium over CDP, all under `/run/desktop`, never
    `/data`. It starts at its screen's first viewer or its agent's first
    browser or computer call, and stops after 10 idle minutes (not while
    watched or held). The owner watches it on the agent's screen (#230's
    per-agent screens: `BRIDGE_SCREENS_DIR`, noVNC 1.7.0).
- **The runtime** (`images/bridge/src/runtime/goose.rs`):
  - It runs one `goose acp` per agent, because goose's custom headers
    and its shell's environment are per process. Each goose starts at
    its agent's first turn and again at the turn after it dies.
  - Each turn is `session/new` (cwd `/data/work`), then `session/close`.
  - In a mind:
    - the framing and VIEW_DOC go in through
      `_goose/unstable/session/system-prompt/set` (append, the same bytes
      every turn);
    - the prompt is two text blocks, the view (fetched as above) and then
      the task;
    - `mcpServers` adds `fragment mcp <mind>`.
  - In any other fragment, the task goes alone.
  - goose's events map to the runtime's: message chunks become `Draft`,
    and a finished tool call becomes a `Step` (carrying the words said
    before it).
  - **Exactly one `Reply` per turn:** the words after the last tool
    call, or `(ended: <outcome>: <why>)` when there are none. That one
    reply is the hand-off's report. Stop is `session/cancel`.
  - Fresh context per turn: no session is ever loaded again. goose's
    state lives in `/tmp/goose/<agent>`, outside `/data` and its saves.
- **Model:** goose's OpenAI provider, pointed at `FRAGMENT_MODEL`, model
  `medium`, with `OPENAI_CUSTOM_HEADERS=x-fragment-agent=<agent>`.
  Compaction is off (`GOOSE_AUTO_COMPACT_THRESHOLD=0`,
  `GOOSE_NO_COMPACTION=1`), and `GOOSE_STABLE_SYSTEM_PROMPT=1` keeps the
  system prompt fixed.
- **Extensions:** goose's `developer` and `skills` builtins
  (`EXTENSIONS={}` drops the rest), plus four MCP servers a session:
  - `mind`: `fragment mcp <mind>` (read-only: view, zoom, date, search);
  - `browser`: Playwright MCP 0.0.83 (18 of its tools) on the agent's
    visible Chromium, reading pages as accessibility snapshots;
  - `computer`: cua-driver 0.28.3 (17 tools: windows, keys, mouse,
    clipboard), plus `screen_look` (a screenshot and a question to the
    route's `vision` model) and `screen_click` (Clef picks a cell of a
    numbered grid drawn on the screenshot, then a finer grid inside it;
    xdotool clicks);
  - `web`: `web_search` (DuckDuckGo's HTML, then Bing, then DuckDuckGo
    lite) and `web_read` (a page as Markdown, in parts; files to
    `/data/work/downloads`).
  - A gate before `browser` and `computer` refuses calls while a person
    holds the screen, starts the desktop, strips images from results
    (GLM reads none) and cuts results past 40k characters.
- **Skills:** each turn installs the agent's skills into goose's skills
  directory: the `fragment` skill (the computer page plus `fragment
  skill`), our `web-search` skill, and the owner's managed skills (26 by
  default, 12 more only with their provider's credential). The system
  prompt's `HANDS` text says to load `fragment` then `apps-finite`
  before any app work.
- **Clef from a computer:** `POST /api/models/v1/decide` beside chat
  completions (docs/api.md, Models), metered as `job.ai.decide` is.
- **Hermes is gone** on this branch: `images/hermes`, the bridge's
  Relay runtime, the hermes and agent-smoke lanes, and the docs about
  them. The deploy config's default image is `goose`.

### The goose fork

`futurepaul/goose`:
- `main` follows upstream `aaif-goose/goose`. It was fast-forwarded to
  `9560429f` on 2026-10-07.
- `fragment/optmem` is `4cfb2d7d`: upstream's v1.53.0 plus two
  switches. It is built with `cargo build --release -p goose-cli --bin
  goose --no-default-features --features portable-default` on
  `rust:1-trixie`, which takes about 150 s and makes a 126 MB binary.
  - `GOOSE_NO_COMPACTION=1`: a session never compacts or summarizes
    itself. When the model reports an overflow, the turn ends and says
    why, instead of rewriting the hand-off goose was given.
  - `GOOSE_STABLE_SYSTEM_PROMPT=1`: the system prompt never changes
    within a session. Without it, the hints of a subdirectory a tool
    touches (`AGENTS.md`, `.goosehints`) join the prompt mid-turn and
    break the cache.
- **What upstream already had:**
  - per-session instructions, through ACP
    `_goose/unstable/session/system-prompt/set` (`mode: append`);
  - the model route's header, through `OPENAI_CUSTOM_HEADERS`, which
    goose reads only when `OPENAI_API_KEY` is set, even to empty;
  - a system prompt with no date in it.
- **`EXTENSIONS={}`** limits goose to `developer` and the session's
  ACP `mcpServers`.

## The MCP servers (`__mcp` and `fragment mcp`)

A fragment has two MCP servers with the same tools, by one rule and one
piece of code (`fragment_core::mcp`: `served`, `tool`, `tools_of`,
`call_of`, `called`), each serving the tools to its kind of client:

- **`<fragment origin>/__mcp`** (cell/src/mcp.rs; docs/api.md, A
  fragment's MCP server), Streamable HTTP for a client its person
  connects through the platform's OAuth 2.1 authorization server
  (docs/api.md, Connected clients). Its token acts as the person, on
  that fragment alone.
- **`fragment mcp <fragment> [--write]`** (cli/src/mcp.rs), stdio for
  an agent with a shell, signed with the CLI's key; inside a computer it
  goes to `FRAGMENT_API` with `x-fragment-agent` (goose's `mind`
  server).

The rule:
- **Tools:** the fragment's operations that have a `description` and
  take an object, that the caller's role may call, named as the
  operation. Queries always; mutations and jobs only when the person
  allowed changes: the consent page's box "Also let it change things"
  (a connection's `writes`), or `--write`. A client that only reads is
  refused a mutation by the fragment too, whatever the route.
- **Arguments** are the operation's input, its schema the operation's.
  Each call is a call of its own (a fresh operation id).
- **A result** whose `text` is a string (the mind's view, zoom and date)
  answers as that text, anything else as JSON; over HTTP the whole
  answer is `structuredContent` too. A refusal is the tool's error,
  `<code>: <message>`; an operation that is no tool is -32602, saying
  why.

This is #240's `__mcp` and the spike's `fragment mcp` made one (#240
listed every operation the person may call, its arguments `{id,
input}`; the spike's flat arguments were kept, being the operation's own
schema, as the mind's descriptions say `zoom(id, n)`: decision for
Paul).

### Connect another agent

The mind's tools are `view`, `zoom`, `date` and `search`, and `note` when
you let the agent write. On the preview, a person `paul`'s mind is
`https://mind--paul--claude-optchat.finite.place/__mcp` (another
deployment: `https://mind--<username>.<its fragments' domain>/__mcp`;
`fragment status mind` prints its origin).

- **claude.ai** (or Claude Desktop): Settings → Connectors → Add custom
  connector. Name it "Mind", give it the URL above, and leave the
  advanced settings' OAuth client empty (Claude registers itself, or
  names its metadata document). Connect: a browser page at the platform
  asks you to sign in if you are not, then "Connect Claude?", naming
  your mind and sending you back to claude.ai. Tick "Also let it change
  things" to let it `note`; leave it unticked to let it only read. Allow.
  In a chat, turn the connector on (the tools menu): Claude can now read
  your memory (`view`), open a line (`zoom`), date a message, search
  every chat, and note something.
- **Claude Code over HTTP:** `claude mcp add --transport http mind
  https://mind--paul--claude-optchat.finite.place/__mcp`, then `/mcp` in
  Claude Code, choose `mind`, Authenticate. The same page opens in your
  browser; it warns that it sends you back to a program on this computer
  (Claude Code's own `localhost` port), which is right here.
- **Claude Code over stdio** (no OAuth: the CLI's own key): `fragment
  host https://claude-optchat.finite.place && fragment login` once, then
  `claude mcp add mind -- fragment mcp mind` (read-only) or `claude mcp
  add mind -- fragment mcp mind --write`.
- **Ending one:** the shell's settings, Connected clients, End (each
  says whether it reads only or also changes things); a client's own
  revocation ends it too. What a connected client wrote names it in the
  mind's `events` (`client.called`, "note … through Claude").

The local proof is the e2e's `mcp` section (crates/e2e/src/lanes/mcp.rs,
`mind_server`): a mind made from the template, connected as claude.ai
connects one, from its `__mcp`'s 401 alone (its metadata, the platform's,
a registration with claude.ai's callback, the consent page, the code
with PKCE), first reading only (`date`, `search`, `view`, `zoom`; `note`
refused) and then allowed changes (`note`, then `search` finding it,
`zoom` opening it whole, `date` dating it, `threads` no tool), and the
mind's events naming Claude.

## Deploy

- **Config:** `~/.config/fragment/finite-place-optchat.jsonc`. It is
  `finite-place.jsonc` with `computers: {default_image: "goose", images:
  {goose: {dockerfile: "images/goose/Dockerfile", build_context: "."}}}`.
- **Deploy:** `cargo xtask deploy --config … --branch claude-optchat`.
  The mind of a person `paul` is at
  `https://mind--paul--claude-optchat.finite.place/`.
- **Models:**
  - turns run on `medium` (GLM-5.3);
  - the compactor and suggestions run on `cheap` (GLM-5.3 Flash);
  - topics run on `clef-flash`;
  - goose runs on `medium`.
- **The live check:** `cargo xtask e2e --hosted --config … --branch
  claude-optchat --only mind-live --max-paid-calls 60`
  (crates/e2e/src/lanes/mind_live.rs).
  It runs only by name, and only on a preview (`Need::RealModels`): a
  local run's `mind` section checks the same loop on the fakes. An e2e
  person's first run, as the shell makes it; a fact told in one thread
  and recalled in a fresh one; a topic Clef sorts that thread into; a
  Builder hand-off that goose runs on a computer from cold; a browsing
  one (Hacker News's top three titles, read in the browser on the
  agent's desktop, at least two of them among the front page's top ten
  as the test fetches it); and, measured but not checked, a click of the
  page's "More" link by `screen_click` (Clef), whose landing and
  coordinates it prints. It takes up to 45 minutes, lends its person 60
  paid calls (the run before the browsing steps spent 11), and prints each latency, the recall's zoom
  and search calls, the browser calls, and the paid calls made.

## Merged from issue #232 (agent-friendly fragment)

The spike carries #232's open draft PRs, so the fragment skill and its
platform are the newest an agent gets: #233 (GUIDE's build discipline
and Design), #235 (`/llms.txt`, `/llms-full.txt`), #236 (the
`contributor` role, the share sheet's Use), #237 (the page's errors on
status and in events), #238, #240 and #242 (OAuth for MCP clients, each
fragment's `__mcp`, the platform's `/mcp`), #249 (drafts before an
account), and the templates When, Wall, Board, Watch, Brief, Hook, Wiki
and Split (#239, #241, #243 to #248). #234 (`__fragment.css`) comes with
the UI's merge. None of them touches the mind, but #240's `__mcp` and
the spike's `fragment mcp` are one rule now (above), and an unclaimed
draft holds `ai.decide` steps as it holds every other AI step.

## Not in the spike

- Importing ChatGPT's export, and reading Hermes's `state.db` directly
  (the CLI has no SQLite: `hermes sessions export` first). Classifying
  imported threads into topics: only a topic added later sorts them (its
  newest 100 threads).
- MCP Apps (the mind's page inline in a chat client): docs/api.md,
  "Inline views (MCP Apps): not yet".
- Prompt-cache breakpoints. Workers AI caches prefixes by itself, and
  the incremental fold keeps the prefix stable.
- Mid-run injection of a new message between tool calls. A message
  sent mid-turn starts the next turn.
- Moving the log out of the app's SQLite (to R2 or git).
- The web as a browser: `web_fetch` reads what a server sends, so a
  page its scripts draw reads as next to nothing, and a PDF is the
  computer's. Cloudflare's Browser Rendering is no step a job has
  (its REST API would take the owner's own Cloudflare token).
- A whole-web search with no key from a Worker. DuckDuckGo's HTML page
  answers a home address and CAPTCHAs a datacenter's (checked
  2026-10-07; from a Worker itself it is untried), so the no-key search
  there is Wikipedia's. Bing's `format=rss` answers both, but its feed
  says its results are for a personal RSS reader alone (decision for
  Paul). A key (Perplexity's, Brave's or Tavily's) is the way.
- An app's files to the main agent (a `job.owner` step reading one at
  `main`): an app whose state is its files (notes) is reached through its
  described operations alone, else through the computer. And past 64 of
  the person's apps, `apps` lists the first 64 by name.
- Images to the main agent (the `vision` model is no tier a job names),
  and `search` over a file's words (FTS indexes the message's own).
