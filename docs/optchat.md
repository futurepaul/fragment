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
    "work": { "read": "editor", "post": "editor" }
  },
  "triggers": [
    { "channel": "say",  "from": "person", "run": "heard" },
    { "channel": "chat", "from": "agent",  "run": "hands_said" },
    { "channel": "work", "from": "agent",  "run": "hands_worked" }
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

### The log, the tree, the view: OptChat's spec, in SQLite

```sql
log(i INTEGER PRIMARY KEY, kind TEXT, text TEXT, at INTEGER, thread TEXT, persona TEXT, task TEXT)
node(l INTEGER, i INTEGER, text TEXT, PRIMARY KEY (l, i))     -- the tree; never rewritten
thread(id TEXT PRIMARY KEY, title TEXT, persona TEXT, started INTEGER, last INTEGER, first_i INTEGER, last_i INTEGER)
topic(id TEXT PRIMARY KEY, name TEXT, description TEXT, made INTEGER)
thread_topic(thread TEXT, topic TEXT, p REAL, PRIMARY KEY (thread, topic))
persona(id TEXT PRIMARY KEY, name TEXT, emoji TEXT, instructions TEXT, hands INTEGER, made INTEGER)
task(id TEXT PRIMARY KEY, thread TEXT, i INTEGER, text TEXT, seq INTEGER, turn TEXT, state TEXT, report TEXT, steps TEXT, started INTEGER, ended INTEGER)
kv(k TEXT PRIMARY KEY, v TEXT)                                 -- default persona, about-me, turn lock, queue
log_fts USING fts5(text, content='log', content_rowid='i')    -- search
```

- `kind` is the spec's set: `user`, `talk`, `tool`, `echo`, `note`.
  Hand-off reports are `user` messages starting `[<task id>] `, which
  the compactor tags `work:`.
- **The spec's constants hold:** NODE 512, VIEW 128 000, TRIES 5,
  CAP 30 000, and the cut-at-limit retry. JOBS is up to 8 nodes in one
  pump step round. RETRY is on the next pump.
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
  any time.
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
   thread waiting.
3. **Settle:** while the view has an unbuilt part, run the pump
   inline (below). It is normally a no-op, because the background pump
   ran after the last turn.
4. Render the view. Then loop, at most 40 model calls a turn:
   - Call `job.ai.text({model: "medium", messages, tools, draft:
     {channel: "log", turn: "turn:<thread>"}})`. The messages are
     `[system, user: [view, texts joined]]`, then the turn's steps.
   - Log each reply `talk`, each tool call `tool` (name and JSON input),
     and each result `echo` (capped). Publish each on `log`.
   - Run the tools: `zoom`, `date` and `search` are queries;
     `computer` opens a hand-off.
   - Stop when the model answers with no tool calls.
5. `turn_end`, a mutation: release the lock. If messages are queued,
   go to 2 in the same run. Otherwise start the job `pump`, then
   `classify` for the thread.

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

Spec §4, as a job. Each round asks `pump_plan` (a query) for up to 8
nodes that are ready (spec §4.1 rules 1–3). It builds them in parallel
with `job.ai.text({model: "cheap", …})` steps (the spec's two-block
input, SCALE, the retry loop). Each result goes to `node_built`, a
mutation where the first write wins, which refits the view. Rounds
repeat until none is ready, or 30 rounds. A failed node is left for the
next pump.

### Topics (job `classify`)

Clef, through `job.ai.decide({model: "clef-flash", state, questions})`
(platform, below).
- **State:** the thread's title and its messages as `kind: text` lines,
  at most 48 KiB, newest kept.
- **Questions:** one `noul` per topic, at most 64 per call: `{type:
  "noul", instructions: "Is this conversation about <name>? <description>"}`.
- A thread is in a topic at p ≥ 0.6. The answers go to `thread_topic`
  through `topics_set` and are published on `log`.
- `topic_add` starts `classify` for the 100 newest threads. A turn's
  end classifies its thread.
- Clef only sorts. Topic names come from the person, or from
  `topic_suggest`, a job: one cheap `ai.text` over the view that answers
  up to 8 names, which the page offers and the person accepts.

### Hand-offs (job `computer` → `chat` → goose)

1. **The task.** The tool's step publishes
   `{text: "<task>\n\n(task <task id>, thread <thread>)", to: [<goose agent id>]}`
   on `chat` as the fragment, under `task_open`. `task_open` records
   the task and the record's `seq`.
2. **goose's turn.** The bridge admits it as a turn: the agent is the
   mind's lead, and the record is from neither the agent nor `anon:`.
   Its turn id is the hash of `<agent>|<mind>/chat/<seq>` (chat-records).
   It claims the turn on `work` (`turn.start`, whose `cause.seq` names
   the task) and posts steps there. Its replies go on `chat` (`rp:<turn>:<n>`)
   and its end on `work`.
3. **The mind follows along.** `hands_worked` (each `work` record from
   an agent) records the turn against its task, keeps up to 50 steps for
   the page, and publishes them on `log`. On `turn.end` it puts the
   report on the queue as a `user` message: `[<task id>] <the turn's
   replies joined>`, or `[<task id>] ended: <outcome>` with none. It
   then starts a turn like `heard` does, in the task's thread.
   `hands_said` keeps each reply part by turn.
4. **goose's context.** At the start of each hand-off turn, the goose
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
| `view` | query (described) | `{}` → `{text, bytes, parts, T, settled}`: the rendered `<chat>…</chat>` |
| `zoom` | query (described) | `{id, n}` → `{text}`: the spec's zoom |
| `date` | query (described) | `{id}` → `{text}`: ISO local time of message `id` |
| `search` | query (described) | `{q, limit?, thread?}` → `{results: [{i, kind, thread, at, snippet}]}` |
| `note` | mutation (described) | `{text}` → `{i}`: append a `note` (an MCP client's write) |
| `threads` | query | `{topic?, before?, limit?}` → `{threads: [{id, title, persona, started, last, summary, topics: [{id, p}], count}]}`; `summary` is the text of the smallest built node covering the thread's last message range, else its first user line |
| `thread` | query | `{id, before?, limit?}` → `{thread, messages: [{i, kind, text, at, persona, task}], more}`; `tool`/`echo` are returned so the page can fold them into a "steps" row |
| `context` | query | `{i, before?, after?}` → `{messages}` around `i`, any thread (expand) |
| `memory` | query | `{}` → `{parts: [{id, n, text, built}], bytes, T}`: the view as structured parts (the Memory screen) |
| `node` | query | `{id, n}` → `{children: [{id, n, text}]}` or, for n = 1, `{message}` |
| `topics` | query | `{}` → `{topics: [{id, name, description, count}]}` |
| `personas` | query | `{}` → `{personas: [{id, name, emoji, instructions, hands}], default}` |
| `tasks` | query | `{thread?}` → `{tasks: [{id, thread, text, state, steps, report, started, ended}]}` |
| `status` | query | `{}` → `{turn: {running, thread, since} \| null, queued, unbuilt, T, hands: bool}` |
| `settings` | query | `{}` → `{about}` |
| `topic_add` / `topic_remove` | mutation | `{name, description?}` / `{id}` |
| `persona_set` / `persona_remove` / `persona_default` | mutation | `{id?, name, emoji, instructions, hands}` / `{id}` / `{id}` |
| `settings_set` | mutation | `{about}` |
| `stop` | mutation | `{thread}`: the running turn stops at its next step |
| `topic_suggest` | job | `{}` → names, published on `log` as `{type: "suggest", names}` |

The internal mutations (editor, no description) are named here so the
two halves agree: `hear`, `turn_begin`, `turn_touch`, `turn_end`,
`logged` (log one step's messages), `pump_plan` (a query),
`node_built`, `task_open`, `hands_step`, `hands_reply`, `topics_set`.

Seeded personas:
- **Mind**, the default: plain, warm, brief.
- **Builder**: does things on the computer; `hands` is on.
- **Coach**: asks one question at a time.

### Records on `log`

- `{type: "msg", i, kind, text, thread, at, persona, task}`, where
  `text` is at most 48 KiB, cut.
- `{type: "turn", thread, state: "thinking" | "settling" | "done" | "error" | "stopped", error?}`.
- `{type: "task", id, thread, state, text, steps?, report?}`.
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

- **The image:** Debian slim, with:
  - the sandbox-shim (docs/computers.md);
  - the bridge (`BRIDGE_RUNTIME=goose`);
  - the fragment CLI;
  - goose, built from `futurepaul/goose` branch `fragment/optmem`, a
    pinned rev;
  - git, curl, python3, ripgrep, Node (pinned, checksummed);
  - Chromium, later.
- **The runtime** (`images/bridge/src/runtime/goose.rs`):
  - It speaks ACP over stdio to one `goose acp` process, started at
    boot and restarted when it dies.
  - Each turn is `session/new`, with cwd `/data/work`. Its first
    `session/prompt` is the framing, then the view (fetched as above),
    then the task text.
  - goose's events map to the runtime's: message chunks become `Draft`,
    the final message becomes `Reply`, tool calls become `Step`, and the
    prompt's end becomes `End`. Stop is `session/cancel`.
  - Fresh context per turn: no session is ever loaded again.
- **Model:** goose's OpenAI provider, pointed at `FRAGMENT_MODEL`
  (`http://model.fragment.internal/v1/chat/completions`), model
  `medium`, with `x-fragment-agent: <agent>` on every call (goose's
  custom headers, or a loopback proxy in the bridge).
- **Extensions:** developer (shell, edit), and `fragment mcp <mind>`
  (read-only) for zoom, date and search.
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
