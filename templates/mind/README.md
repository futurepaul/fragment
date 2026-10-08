# Mind

One memory for every chat (docs/optchat.md, the contract; UniiChat's
design, VictorTaelin's gist as rewritten on 2026-10-08, which it follows
but for docs/optchat.md's "Where we differ from the gist"). A blessed
template: every mind runs the platform's release of this code, and its
repo holds only its face.

- `app.mjs`: the App. Its SQLite holds the log (every message, verbatim,
  append-only; a text past 30,000 characters as several in a row), the
  tree of summaries, the two views (`vline`, `cline`), the nodes ready to
  build (`ready`, with the compactor's leases), threads, topics, personas,
  hand-offs (`task`), and `kv` (the default persona, the about-me, the
  turn lock, the queue, the sawtooths' state, pumps starting, failures).
  Its operations are docs/optchat.md's table.
- `applib/optmem.mjs`: the memory itself, pure: the merge order (the most
  due pair, `(T + 1)/2^l - i`, its parent built, the oldest first), the
  chat's view's sawtooth (128 KB down to 64 KB, merging only at a new
  message) and the compaction view's (32 KB down to 16 KB, made again
  from the chat's whenever that one merges), free nodes and what becomes
  ready, a journal of what each change did (for the App to write), load
  and the one-time fold for a mind saved before the views were, render
  (`id+n|text`), zoom in pages, a compaction's view and task (UniiChat
  §4, verbatim, its ruler 512 dashes), the "Too long" retry, CAP for a
  tool's output, and long texts split.
- `applib/prompts.mjs`: PROMPT, the one system prompt of every call
  (UniiChat §5 with "Mind" and the mind's tools), the tools every call
  offers, what starts each turn's message (the time, the chat, the
  persona, the hands), the subagent framing goose's copy follows,
  topic_suggest's prompt and research's.
- `applib/web.mjs`: the web tools, through `job.fetch`: `web_search`
  (Perplexity, Brave or Tavily by the secret the owner set, else
  DuckDuckGo's HTML page, else Wikipedia's search), `web_fetch` (a page
  as readable text), and `research` (Perplexity's sonar, else a search,
  three pages read and one cheap call answering from them, with sources).
- `applib/files.mjs`: a message's files (attachments): their checks, a
  small text one's text, and the message as the memory reads it with them.

## How it runs

- **The views are saved, never rebuilt.** Each change to the memory
  writes what it did in its mutation; an instance loads the views at its
  first call. Each mutation that changes the memory bumps `kv.rev`; an
  instance whose memory is of another `rev` (a rolled-back mutation)
  loads it again. A mind saved before the views were is folded from its
  log once, by its first instance (`folds`: 1).
- **`heard`** (`say`'s trigger) logs the message (`hear`), then runs turns
  while messages wait: `turn_begin` takes the lock and the oldest
  thread's queued messages; the wait builds the unsummarized messages
  before them it may (`pump_step` with `upto`) and starts pumps for the
  rest; `turn_view` fits the views and renders the chat's once for the
  turn, stopping before the newest messages still waiting (they go whole
  after the turn's state: the gist renders before it logs); then model
  calls (`medium`, drafting on `log` as `turn:<thread>`), every one with
  the same tools and system prompt, each answer logged (`logged`: talk,
  tool, echo; it also takes the thread's messages queued since, which
  reach the model between its tool calls, and starts a pump when none is
  at work). `turn_end` lets the lock go; the thread is classified; when
  nothing waits, `pump` runs.
- **Files:** a `say` record's `attachments` (the mind's blobs) are
  logged with the message; `heard` reads the small text ones first
  (`job.blob`), so the memory (the compactor, zoom, a turn) reads a
  message with its files. A hand-off carries the turn's files on `chat`,
  and goose's reply's files come back on its report.
- **Keys** for the web are the owner's, as the mind's secrets:
  `fragment secret set <mind> PERPLEXITY_API_KEY` (or `BRAVE_API_KEY`,
  `TAVILY_API_KEY`). With none, search is DuckDuckGo's, then Wikipedia's
  (DuckDuckGo CAPTCHAs a datacenter's address, so on Workers it is, in
  practice, Wikipedia's).
- **`pump`** is one of up to 8 compactors at once (a job's steps run one
  at a time, so 8 calls at once are 8 runs): `pump_step` writes the node
  it built and takes the next under a lease (the oldest by its last
  message, of the merges ready and the first 8 unbuilt messages, while
  fewer than 8 are leased), and says how many more pumps to start; each
  node is one conversation of up to TRIES `cheap` calls with the turns'
  tools (`tool_choice: "none"`) and system prompt, the compaction view up
  to the node, and UniiChat's task.
- **Hand-offs:** `computer` publishes the task on `chat` (to the agent),
  then `task_open` records it with the turn the agent's bridge gives that
  record (24 hex of SHA-256 of `<agent fragment>|<mind>/chat/<seq>`).
  goose posts one reply a hand-off, its report; `hands_said` (`chat`'s
  trigger) logs it as a `work` message `[<task>] …` and queues it, which
  runs a turn (or reaches the running one). A reply that comes before its
  `task_open` is kept for it. The page follows goose's steps on `work`
  itself, by `turn`; the mind runs nothing for them, and a turn's
  `zoom("<task>")` reads them when it zooms (`job.records`, by `turn`),
  for the task's whole run. A task with no reply
  in 30 minutes is `lost` (a later reply still reports).
- **Topics:** `classify` asks Clef (`job.ai.decide`, `clef-flash`) one
  `noul` per topic about a thread; a thread is in a topic at p ≥ 0.6.
  `topic_add` publishes `{topic}` on `sort`, whose trigger classifies the
  100 newest threads (a mutation starts no job). `topic_suggest` offers
  names on `log`.

**Jobs re-run from the top at each step**, so their control flow follows
only their steps' answers. A compaction's input (the compaction view up
to its node) and Clef's state are read from the instance as their step
is built, not carried in a step's answer: what a past step was sent does
not matter. A turn's view is a step's answer, so every call of the turn
sees the same one.

**Budgets** (a run takes at most 256 steps and 4 MiB of answers): a turn
makes at most 40 model calls, runs at most 8 tool calls an answer
(answers 24; past that, dropped), and makes its last call without tools
when the run nears its steps, its answers' bytes, or 512 KiB of
conversation (a step's arguments travel in a Workflow step of at most 1
MiB). A run takes at most 4 turns and starts one only below 64 steps; a
turn's wait takes at most 96 steps; a pump keeps steps for a node and
its spawns; past any of these the rest goes to a fresh run (`heard
{resume}`, `pump`), one hop deeper each (the platform blocks a chain past
16).

## Debt and open problems

- **The database.** Everything lives in the app's SQLite, which the mind
  declares at 1 GiB (`storage.maxBytes`; the platform's 16 MiB holds a
  few tens of thousands of messages, less than an imported history may
  be). No meter counts it, and the instance keeps every node's text in
  memory. Moving the log out (R2, git) is not in the spike.
- **An import's backlog holds turns** (docs/optchat.md, "Importing
  chats"): no call sees a placeholder, so a turn after a long import
  waits, settling, while the pumps catch up.
- **`log` is never trimmed** (no post role): every message is a record
  there too, at most 48 KiB.
- **Triggered runs pause past 120 an hour** (docs/api.md, Jobs): one
  `hands_said` a hand-off, and one `heard` a message, so only a person
  saying 120 things an hour reaches it. Pumps are job calls, not
  triggered runs.
- **A turn that dies holds the lock** until it runs out (15 minutes after
  its last touch); its messages wait for the next message or report.
- **A node that never builds** (a model that refuses one message) blocks
  every turn after it: a turn ends with an error once that message failed
  3 times, its message logged and unanswered; pumps try it again 10 s
  after each failure, and at each new message.
- `date` answers UTC: the mind does not know its person's time zone.
- **A web page is kept whole in its run.** `job.fetch` answers up to 1
  MiB, kept with the run's 4 MiB of answers, so a turn reads three or so
  large pages at most, and `research` reads fewer when the turn has read
  much already. A page drawn by its scripts reads as little, and a PDF
  is the computer's.
- **The app cannot tell which secrets are set**: each web tool tries
  the keyed providers in order, one failed step for each key that is
  not set, once a turn.
- Internal operations are editors': the agent may call them as the owner
  can.
