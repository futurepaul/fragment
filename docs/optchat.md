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
                            └─ `fragment mcp mind` ─▶ any agent (Claude Code, goose, …)
```

- **The mind** is the one memory and the main agent. It is a blessed
  template (`templates/mind`), so the platform holds none of its logic,
  and every person's mind runs the same release.
- **goose** is the hands. It runs on the person's computer (Containers),
  takes hand-offs as turns through the bridge, and reports back. Each
  hand-off is a fresh goose session whose first message is the mind's
  view, so goose knows what the mind knows.
- **The MCP server** is the CLI's `fragment mcp <fragment>`. It serves
  any fragment's described operations as tools. Pointed at your mind,
  it gives any agent the same view, zoom and search.

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
    "sort": { "read": "editor" }
  },
  "triggers": [
    { "channel": "say",  "from": "person", "run": "heard" },
    { "channel": "chat", "from": "agent",  "run": "hands_said" },
    { "channel": "sort", "run": "classify" }
  ]
}
```

The `operations` are listed below. Its members are the owner and the
goose agent, an editor. The shell makes it `members` (private).

- **`say`**: the person's messages, `{text, thread, persona?}`, posted
  by the page (`fragment.post("say", …)`). `thread` is the page's id
  for the screen, `t_` plus 16 hex. `persona` is the persona chosen for
  that thread.
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

### The log, the tree, the view: OptChat's spec, in SQLite

```sql
log(i INTEGER PRIMARY KEY, kind TEXT, text TEXT, at INTEGER, thread TEXT, persona TEXT, task TEXT)
node(l INTEGER, i INTEGER, text TEXT, PRIMARY KEY (l, i))     -- the tree; never rewritten
thread(id TEXT PRIMARY KEY, title TEXT, persona TEXT, started INTEGER, last INTEGER, first_i INTEGER, last_i INTEGER)
topic(id TEXT PRIMARY KEY, name TEXT, description TEXT, made INTEGER)
thread_topic(thread TEXT, topic TEXT, p REAL, PRIMARY KEY (thread, topic))
persona(id TEXT PRIMARY KEY, name TEXT, emoji TEXT, instructions TEXT, hands INTEGER, made INTEGER)
task(id TEXT PRIMARY KEY, thread TEXT, i INTEGER, text TEXT, seq INTEGER, turn TEXT, state TEXT, report TEXT, steps TEXT, started INTEGER, ended INTEGER)  -- steps: '[]', unread (goose's are on work)
kv(k TEXT PRIMARY KEY, v TEXT)                                 -- default persona, about-me, turn lock, queue
log_fts USING fts5(text, content='log', content_rowid='i')    -- search
```

- `kind` is the spec's set: `user`, `talk`, `tool`, `echo`, `note`.
  Hand-off reports are `user` messages starting `[<task id>] `, which
  the compactor tags `work:`.
- **The spec's constants hold:** NODE 512, VIEW 128 000, TRIES 5,
  CAP 30 000, and the cut-at-limit retry. JOBS is up to 8 nodes in one
  pump step round. RETRY is on the next pump. A node a stubborn model
  wrote past twice NODE is cut there.
- **Prompts:** COMPACT, MASTER, VIEW_DOC and the subagent prompt are
  verbatim from the spec, with "OptChat" replaced by "Mind".
  - MASTER's "Use subagents only when the user asks" becomes: use
    `computer` for work that needs a computer (files, shell, browsing,
    code). Answer everything else yourself.
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
- **The 16 MiB cap is debt.** A message over 30 000 characters is
  capped at logging, as the spec caps tool results, keeping head and
  tail.

### Turns (job `heard`, triggered by `say`)

The spec's §7, as a job:

1. `hear`, a mutation: log the message (`user`), touch its thread
   (making it, titled from the first line, on first use), publish it on
   `log`, and push it on the queue. If a turn is running, stop there:
   the running turn takes it next.
2. `turn_begin`, a mutation: take the turn lock (it expires 15 minutes
   after its last touch), then take the queued messages of the oldest
   thread waiting. It answers `tail`: where the turn's view stops, before
   the newest run of messages still waiting. They go whole as block 2,
   as the spec renders the view before it logs them.
3. **Settle:** while the view has an unbuilt part before `tail`, build it
   inline, level 0 one at a time. It is normally a no-op, because the
   background pump ran after the last turn. A node a pump holds (a lease
   from `pump_plan`) is waited for, not built twice. A node that fails 3
   times, 10 s apart, ends the turn with an error, its messages logged
   and unanswered. A settle past its budget (96 steps) puts the messages
   back first in line and hands them to a fresh run.
4. Render the view (`view {upto: tail}`). Then loop, at most 40 model
   calls a turn:
   - Call `job.ai.text({model: "medium", messages, tools, draft:
     {channel: "log", turn: "turn:<thread>"}})`. The messages are
     `[system, user: [view, texts joined]]`, then the turn's steps.
   - Log each reply `talk`, each tool call `tool` (name and JSON input),
     and each result `echo` (capped). Publish each on `log`.
   - Run the tools: `zoom`, `date` and `search` are queries;
     `computer` opens a hand-off. At most 8 run per answer; past 24, an
     answer's calls are dropped (a mutation publishes 64 records).
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
- `computer(task)`: hand work to goose on the person's computer. It
  answers `[<task id>] started` at once. The report arrives later as a
  `user` message `[<task id>] <report>`, which starts a turn of its own
  when none runs (MASTER: never wait or poll for it). It is offered only
  when the persona has `hands` and the mind has an agent member.

### The compactor (job `pump`)

Spec §4, as a job. Each round asks `pump_plan` (a query that writes
its leases in `kv`) for up to 8 nodes that are ready (spec §4.1 rules
1–3). It builds them with `job.ai.text({model: "cheap", …})` steps (the
spec's two-block input, SCALE, the retry loop), one after another: a
job's steps run one at a time (cell/platform.mjs), so JOBS is how many
nodes a round takes, not how many calls run at once. A level-0 node is
built alone in its round (a turn waits on level 0, and its next is ready
only once it is built); merges wait for a round with none. Each result
goes to `node_built`, a mutation where the first write wins, which
refits the view. Rounds repeat until none is ready. A failed node is
left for the next pump. Past 30 rounds or the run's budget with work
left, a fresh `pump` takes the rest.

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
   `{text: "<task>\n\n(task <task id>, thread <thread>)", to: [<goose agent id>]}`
   on `chat` as the fragment (`job.publish`, which answers the record's
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
   `user` message, `[<task id>] <reply>`, and starts a turn like `heard`
   does, in the task's thread. A later part changes nothing. A reply
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
| `thread` | query | `{id, before?, limit?}` → `{thread, messages: [{i, kind, text, at, persona, task}], more}`; `tool`/`echo` are returned so the page can fold them into a "steps" row |
| `context` | query | `{i, before?, after?}` → `{messages}` around `i`, any thread (expand) |
| `memory` | query | `{}` → `{parts: [{id, n, text, built}], bytes, T, cut?}`: the view as structured parts (the Memory screen); past 768 KiB its last `cut` parts are left out |
| `node` | query | `{id, n}` → `{children: [{id, n, text, built}]}` or, for n = 1, `{message}` |
| `topics` | query | `{}` → `{topics: [{id, name, description, count}]}` |
| `personas` | query | `{}` → `{personas: [{id, name, emoji, instructions, hands}], default}` |
| `tasks` | query | `{thread?}` → `{tasks: [{id, thread, i, turn, text, state, report, started, ended}]}`, the newest 50; `i` is the `tool` message that opened it; `turn` the agent's (its steps are on `work` under it); `state` is `running`, `done`, `stopped`, `error` or `lost`; `text` cut to 4 KiB and `report` to 16 KiB (the report whole is its message) |
| `status` | query | `{}` → `{turn: {running, thread, since} \| null, queued, unbuilt, T, hands: bool, failing: [{id, n, error, tries}]}`; `hands` is whether the last turn saw an agent member; `failing`, the nodes whose last build failed |
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
`task_open`, `hands_reply`, `topics_set`. The jobs are `heard`, `pump`,
`classify`, `topic_suggest` and `hands_said`.

Seeded personas:
- **Mind**, the default: plain, warm, brief.
- **Builder**: does things on the computer; `hands` is on.
- **Coach**: asks one question at a time.

### Records on `log`

- `{type: "msg", i, kind, text, thread, at, persona, task}`, where
  `text` is at most 48 KiB, cut.
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

One screen at a time, and calm. Dark, warm, generous type.
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
    ×3" row.
  - A hand-off is a card that shows goose's live steps and its report.
  - Snippets of other threads (a search hit, a zoom result) are
    expandable.
  - The composer sits at the bottom, with the persona chip and Stop.
- **Right panel** (toggle): the thread's topics, its hand-offs ("what it
  did here"), and the computer's state.
- **Topic screen:** the threads in the topic, each a summary card that
  expands in place.
- **The mobile layout** comes first: the rail is a drawer.

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
   the operation an MCP tool.
5. **The shell's first run:**
   - it makes the agent (on the default image, goose), assigns it to
     the computer, makes `mind` (members) and adds the agent there as
     an editor;
   - it no longer makes a `<agent>-chat`;
   - signed in with a mind, `/` opens the mind full-screen; the shell's
     own UI is one link away ("Apps").

## goose on the computer (`images/goose`, bridge runtime `goose`)

- **The image:** Debian trixie-slim, 152 MB compressed, with:
  - tini as PID 1;
  - the sandbox-shim (docs/computers.md);
  - the bridge (`BRIDGE_RUNTIME=goose`, built for musl);
  - the fragment CLI;
  - goose, built in a stage of its own from `futurepaul/goose` at a pinned
    rev of `fragment/optmem`;
  - git, curl, jq, python3, ripgrep;
  - no Node, no Chromium and no display yet: port 6080 serves a "no
    screen yet" page.
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
- **Extensions:** `EXTENSIONS={}`, which leaves `developer` (shell,
  edit, tree), plus the session's `mind` MCP server (read-only: view,
  zoom, date, search).
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

## The MCP server (`fragment mcp`)

`fragment mcp <fragment> [--write]`: an MCP server over stdio (JSON-RPC,
protocol 2025-06-18: `initialize`, `tools/list`, `tools/call`).
- **Tools:** the fragment's operations that have a `description`,
  named as the operation, with its input schema. Queries are always
  served; mutations and jobs only with `--write`.
- **A call** is `POST /api/f/<f>/ops/<op>`. A person's call is signed
  with their key, as every CLI call is. Inside a computer it goes to
  `FRAGMENT_API` with `x-fragment-agent`.
- **For Claude Code:** `claude mcp add mind -- fragment mcp mind`
  (read-only) or `… --write`.
- A remote HTTP MCP server for chat clients without a shell is issue
  #232's, and later.

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

## Not in the spike

- Importing other providers' chats. `note` and an `import` job are the
  door for it.
- A remote HTTP MCP server with OAuth.
- Prompt-cache breakpoints. Workers AI caches prefixes by itself, and
  the incremental fold keeps the prefix stable.
- Mid-run injection of a new message between tool calls. A message
  sent mid-turn starts the next turn.
- Moving the log out of the app's 16 MiB SQLite (to R2 or git).
