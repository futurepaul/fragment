# Mind

One memory for every chat (docs/optchat.md, the contract; OptChat's spec,
which it follows). A blessed template: every mind runs the platform's
release of this code, and its repo holds only its face.

- `app.mjs`: the App. Its SQLite holds the log (every message, verbatim,
  append-only), the tree of summaries, threads, topics, personas,
  hand-offs (`task`), and `kv` (the default persona, the about-me, the
  turn lock, the queue, the compactor's leases). Its operations are
  docs/optchat.md's table.
- `applib/optmem.mjs`: the memory itself, pure: the fold (append, then
  merge the most due pair; never split), render (`id+n|text`), zoom, the
  compactor's readiness (spec 4.1, rules 1 to 3), free nodes, its
  two-block input with SCALE (512 bytes, checked at load), the cut-at-limit
  retry, CAP, and byte-safe cuts.
- `applib/prompts.mjs`: COMPACT, MASTER, VIEW_DOC and the subagent framing,
  the spec's verbatim with "OptChat" as "Mind" and MASTER's one change;
  the tools; topic_suggest's prompt.

## How it runs

- **The view is never stored.** It is folded from the log and the tree at
  the app's first call and kept on the instance. Each mutation that
  changes the log or the tree bumps `kv.rev`; an instance whose memory
  is of another `rev` (a rolled-back mutation, another instance) folds
  again (2,000 messages: about 10 ms; 20,000: about 160 ms).
- **`heard`** (`say`'s trigger) logs the message (`hear`), then runs turns
  while messages wait: `turn_begin` takes the lock and the oldest
  thread's queued messages; the settle builds the view's unsummarized
  lines before them (level 0, one at a time, the spec's rule 3) or waits
  for a pump that holds them; `view {upto}` renders the view once for
  the turn, stopping before the newest messages still waiting (they go
  whole, as block 2: the spec renders before it logs); then model calls
  (`medium`, drafting on `log` as `turn:<thread>`) with zoom, date,
  search, and computer (a persona with hands, and an agent member),
  each answer logged (`logged`: talk, tool, echo). `turn_end` lets the
  lock go; the thread is classified; when nothing waits, `pump` runs.
- **`pump`** builds what is ready, the level-0 node alone first (a turn
  waits on it), merges when none is: each node one conversation of up to
  TRIES `cheap` calls, written by `node_built` (first write wins). A job's
  steps run one at a time, so the spec's JOBS is how many nodes a round
  takes, not how many calls run at once. Leases (`pump_plan`, a query
  that writes them) keep a pump and a turn's settle from building the same
  node.
- **Hand-offs:** `computer` publishes the task on `chat` (to the agent),
  then `task_open` records it with the record's `seq`. goose's records
  (`hands_worked`, `hands_said`) follow it; one that arrives before
  `task_open` is kept as an orphan by `seq` and adopted. At `turn.end`
  (after 2 s, for its last replies) the report is queued as `[<task>] …`
  and runs a turn.
- **Topics:** `classify` asks Clef (`job.ai.decide`, `clef-flash`) one
  `noul` per topic about a thread; a thread is in a topic at p ≥ 0.6.
  `topic_add` publishes `{topic}` on `sort`, whose trigger classifies the
  100 newest threads (a mutation starts no job). `topic_suggest` offers
  names on `log`.

**Jobs re-run from the top at each step**, so their control flow follows
only their steps' answers. The compactor's input (the view up to a
node, up to 128 KB) and Clef's state are read from the instance as their
step is built, not carried in a step's answer: they would fill a run's 4
MiB of answers, and what a past step was sent does not matter. A turn's
view is a step's answer, so every call of the turn sees the same one.

**Budgets** (a run takes at most 256 steps and 4 MiB of answers): a turn
makes at most 40 model calls, runs at most 8 tool calls an answer
(answers 24; past that, dropped), and makes its last call without tools
when the run nears its steps, its answers' bytes, or 512 KiB of
conversation (a step's arguments travel in a Workflow step of at most 1
MiB). A run takes at most 4 turns and starts one only below 64 steps; a
settle takes at most 96 steps; past any of these the rest goes to a
fresh run (`heard {resume}`, `pump`), one hop deeper each (the platform
blocks a chain past 16).

## Debt and open problems

- **The 16 MiB cap.** Everything lives in the app's SQLite, which holds
  16 MiB (with FTS5 over the log): a few tens of thousands of messages.
  Every logged message is capped at 30,000 characters, head and tail
  kept. Moving the log out (R2, git) is not in the spike.
- **`log` is never trimmed** (no post role): every message is a record
  there too, at most 48 KiB.
- **Triggered runs pause past 120 an hour** (docs/api.md, Jobs):
  `hands_worked` runs once per goose `work` record, so a hand-off with
  many steps can pause it, and its report would not arrive.
- **A turn that dies holds the lock** until it runs out (15 minutes after
  its last touch); its messages wait for the next message or report.
- **A node that never builds** (a model that refuses one message) blocks
  every turn after it: the settle tries it 3 times, 10 s apart, then ends
  the turn with an error, its message logged and unanswered; each pump
  tries again.
- `date` answers UTC: the mind does not know its person's time zone.
- Internal operations are editors': the agent may call them as the owner
  can.
