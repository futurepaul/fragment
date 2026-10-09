# goose in the mind: the design (Paul, 2026-10-09)

Paul: "the same agent and same memories running in DO or on the
computer, just different runtime and tools available". goose is the
agent in both places. The mind keeps UniiChat's memory (docs/optchat.md).
We grow no pi or goose of our own (#300).

Three asks shape it:
1. rely on goose more heavily, keeping our memory model;
2. pay for what you use: no fragment and no cell carries goose unless it
   runs it, and the cell's 10.1 MB is already too big;
3. cost per turn done right, with goose.

#301 (closed) is the counter-example. It used goose's GDK as a sequencer:
- its provider sent our wire, not goose's conversation;
- tools were JS callbacks, and every policy stayed in app.mjs;
- 5.7 MB of WASM went into the cell and every fragment's app;
- each Workflow step replayed the machine from its seed.

## What we measured (2026-10-09)

**goose upstream** (aaif-goose/goose main `34c61eb5`)
- The GDK is `goose-agent` plus `goose-provider-types`. It is alive:
  4 alphas in September, and more moving into it (subagents #12792,
  PromptManager and skills #11761, an optional catalog #12659).
- Since #12760 (10-08), the CLI's `goose::Agent` runs only on the
  GDK's `StateMachine`. One loop crate serves both runtimes.
- wasm32 is upstream: #12569 (merged 09-29, co-authored by Paul). CI
  checks goose-agent and goose-provider-types for wasm32.
- On wasm32 today:
  - **yes:** the loop (`StateMachine`, `InferenceRunner`,
    `ToolOperation`), the request and stream formats (openai, anthropic,
    …), in-process tools (`ToolProvider`), usage effects, and a pluggable
    store (`SessionLoader`/`EffectHandler`);
  - **no:** goose-providers' HTTP and goose-context-management (Send
    bounds; mechanical), and everything in the `goose` crate itself (sqlx,
    config, extensions, Summon, recipes, steer and max-turns operations).
- Our fork: `fragment/optmem` is 52 behind main and still on the legacy
  loop. `12922e7` (#301's) is 195 behind.

**goose's size**
- #301's 5.7 MB was 77% one model catalog (`canonical_models.json`,
  4.36 MB).
- A probe running the real loop with a host tool provider, steer and
  max turns, the openai format and host fetch was measured at three
  levels of cuts:
  - as main is: 1.39 MB (632 KB gz);
  - without the catalog: 1.05 MB (412 KB gz);
  - also without regex's Unicode tables: 743 KB (323 KB gz).
- goose-agent itself is about 22 KB of it.
- The catalog also costs 26.7 MiB of memory and 41–73 ms on a first
  turn, against 2.1 MiB and 9–16 ms without it.

**The cell's 10.1 MB**
- 5.17 MB of data and 4.06 MB of code.
- 4.79 MB of the data is embedded files:
  - templates 4.15 MB, of which the notes viewer (through brain's
    symlink) is 2.46 MB and skills 0.75 MB;
  - the shell 0.56 MB, of which the wallpaper is 313 KB.
- Every cell instance copies the data segments into its memory: about
  5 MB per isolate.
- Startup is 16 ms locally. Workers' limits (64 MiB, 1 s startup) are far
  off: the cost is memory per isolate, deploy time and the principle.

**A turn's cost today** (local, the fake model, platform overhead only)

| turn | job steps | Workflow steps | workerd CPU |
|---|---|---|---|
| word | 5 | 11 | 50–100 ms |
| 4 tools | 21 | 43 | 0.42 s |
| 12 tools | 53 | 107 | 1.35 s |

- Each job step costs about 25 ms CPU and 40 ms wall locally, and about
  0.25 s hosted. That is roughly 1 s of a word turn and about 13 s of a
  12-tool turn spent on the platform, not the model.
- On #301, replay per step grew with the calls made: a 12-tool turn
  replayed 493 ms against 155 ms today.
- Dollars: the platform is 1–2% of a turn. Model tokens are the rest
  ($0.008 for a word turn on a big view, $0.11 for 12 tools, uncached).
  The real cost of steps is latency.
- The ledger meters no Workflow step, Workflow storage or CPU.
- Workflow state keeps every model call's whole prompt for 30 days.

## The design

### 1. One goose, two runtimes

- One fork rev builds both goose runtimes: upstream main plus our
  switches. It is a new branch on futurepaul/goose (`fragment/main`);
  `fragment/optmem` stays as it is.
  - **On the computer:** the goose CLI, as today, over ACP. It moves to
    the state-machine loop: Stop's semantics and a `<turn-context>`
    block change, so the goose lanes run again.
  - **In the mind:** a small Rust crate, `templates/mind/goose/`, built to
    WASM. It pins `goose-agent` and `goose-provider-types` at the same
    rev.
- The fork carries, until each is merged upstream:
  - `GOOSE_NO_COMPACTION` and `GOOSE_STABLE_SYSTEM_PROMPT`;
  - an off switch for `<turn-context>`;
  - the optional catalog (#12659).
- The same agent means:
  - the same system prompt;
  - the same view and per-turn block;
  - the same mind tools: in the DO a Rust `ToolProvider`, on the
    computer `fragment mcp mind`, from one schema set;
  - the same loop crate.
- What differs is the runtime and the tools. The DO has
  memory, web, apps and `computer`. The computer adds shell, browser
  and desktop.

### 2. goose owns the agent; the mind owns the memory

**goose owns:**
- the turn's conversation, as goose `Message`s;
- the loop and its policy: max turns, unknown tools, empty answers,
  retries, the tool-call cap;
- the request format;
- usage effects;
- later, subagents (#12792) and skills (#11761), once they are in
  goose-agent.

**The mind owns:**
- the log, the tree, the view, the compactor, threads, personas and
  topics (`applib/optmem.mjs`, unchanged);
- the tools' implementations: zoom, date, web, apps, computer.

The Rust crate is host glue only, about 300 lines:
- a store whose session lives in the mind's SQLite;
- a provider that uses goose's format code for the request and the
  platform's model route for the HTTP;
- a bridge from the tool provider to the mind's tools;
- a steer operation that reads the mind's queue.

Goose's effects feed the log: each answer, tool call and result is
logged as it lands, as today. A turn is still a fresh call:
`[tools][system][view][block][message]`. The steer and max-turns
operations are ours until they move into goose-agent; then they are
goose's own.

**Cache marks.**
- On Workers AI, prefix caching is automatic, and goose's openai format
  keeps the prefix byte-stable.
- For Claude, the view's 4-line-block marks need caller-placed cache
  breakpoints, which goose's formats lack. That goes upstream. Until it
  lands, a fork patch keeps the view as its own content part with its
  hint.

### 3. Pay for what you use

**An app may ship WASM.** A generic platform capability, named for no
runtime:
- `applib/*.wasm` is an app module, typed `{wasm}` for the Worker
  Loader;
- the code row keeps its path, sha and size, never the bytes;
- the Loader's code callback becomes async and reads the bytes only on
  a miss;
- limits: `APP_WASM_MAX_BYTES` 8 MiB, and a module count;
- the template's JS imports it lazily, in the one job that runs goose.

Where the bytes live:
- a blessed release's in the cell's Static Assets;
- any other fragment's as a blob in R2.

A fragment without WASM loads none. The cell carries a few hundred bytes
of generic Rust. `SPECIAL-CASE-INVENTORY.md` gains nothing: goose is
template data, the "fragment-native" goose decision 33 left room for.

**The cell's diet, done first:**

| cut | raw | gz |
|---|---|---|
| templates into Static Assets (`run_worker_first`, read through `env.ASSETS`; the CLI keeps embedding them) | −4.15 MB | −0.92 MB |
| the shell into Static Assets, with the cell still adding its headers | −0.56 MB | −0.42 MB |
| the mind's dev-only `site/mock.js` out of the release | −0.06 MB | |

- That takes the cell to about 5.2 MB raw (1.7 MB gz).
- A `cargo xtask check` budget then holds it (≤ 6 MB).
- Stripping the name section (−0.87 MB raw, −0.1 MB gz) costs named
  frames in panics: not now.

### 4. Cost per turn: a job that drives, not replays

A job today replays its body from the top at every step, and every
effect costs two Workflow steps. With goose that is wrong twice: replay
re-runs the machine, and the steps are most of a turn's latency.

**Drive mode** (generic, chosen by the job's declaration):
- The run's Workflow calls the facet's `__drive(run, cap)` in one step.
- The body runs live, with goose's machine in memory, until it is done
  or reaches a budget (about 40 effects or 2 minutes, under the facet's
  50 subrequests and the step's 5 minutes). It then returns `more` or
  `done`.
- `cap` is a run-scoped capability for the step kinds a job has
  (text with draft, fetch, owner.call, call, publish, members, …). Each
  is keyed `(run, key)`, and its answer is kept in the run.
- One live driver per run: a lease.

**Recovery, with no replay engine (#300):**
- A drive that dies is retried by the Workflow, and resumes from goose's
  session in SQLite.
- An effect asked again under a key with a kept answer gets that answer,
  with no second charge.
- A model call with no kept answer is made again.
- An effectful call that started without a kept answer (fetch,
  owner.call, a hand-off) ends `interrupted`, and the model sees that
  as the tool's result.
- At most one effect per crash is uncertain.
- A deploy mid-turn resumes on the new code: the session's schema
  carries a version.

**What changes for the person:**
- A message sent mid-turn reaches the loop between tool calls with no
  step at all: `hear` runs beside the drive in the same facet.
- Stop is a flag checked between machine steps.
- Drafts stream as today.

**Expected** (estimates, to be measured):
- 3–4 Workflow steps a turn instead of 11, 43 or 107;
- no prompts kept in Workflow state;
- CPU linear in effects;
- a 12-tool turn's platform overhead from about 13 s to about 1 s
  hosted.

Drive mode helps today's JS loop as much as goose. It lands first, and
goose drops into it.

**What a turn records:**
`timing.cost = {micros, tokens{in, cached, out}, calls, steps,
wf_steps, platform_ms, model_ms, tools_ms}` on the turn's record.
- The charge per call comes from the settled charge `ai.text` already
  has but doesn't return.
- `cpu_ms` comes later, hosted, from a tail consumer summing each
  invocation's CPU by run.
- The page shows a one-line cost under a turn, behind a toggle.
- mind-live prints p50 and p90 per kind of turn, and the cached share.

## Phases

| phase | what | lands on |
|---|---|---|
| 0 | the cell's diet: templates and shell into Static Assets; a size budget in `check` | master (PR), then optchat |
| 1 | goose `fragment/main`: upstream main + our switches + the turn-context switch + #12659; the computers on it; goose lanes again | the fork; optchat's image |
| 2 | apps may ship WASM (`applib/*.wasm`, bytes in Assets or R2, async loader, limits; e2e: no WASM in a fragment that has none) | master (PR), then optchat |
| 3 | drive-mode jobs (run-scoped capability, lease, interrupted effects); the mind's JS turn moves onto it; `timing.cost` | master (PR), then optchat |
| 4 | the mind on goose: `templates/mind/goose/`, goose owning the conversation and loop; the same prompt and tool schemas as the computer's goose; mind-live against today's numbers | optchat |
| 5 | #300 on top: outbound MCP (rmcp's streamable-HTTP client over host fetch, so goose in the DO gets MCP extensions), Worker tools, Summon subagents once #12792 lands, nested task views, direct goose chat | optchat |

Phase 1 is built (2026-10-09): `fragment/main` is `a29b6aa9`, upstream
main `3bd85200` plus #12659's two commits and the three switches
(docs/optchat.md, "The goose fork"); the goose image pins it. Stop on the
state machine answers `cancelled` at once, as v1.53.0 did. The computer
keeps goose's `<turn-context>`: the bridge's prompt (the mind's `view`, then
the task) carries no per-turn block, so it is goose's only clock; the
switch waits for phase 4, where the mind's per-turn block says the time.

**Upstream** (drafted by us, posted by Paul):
- the optional catalog (#12659, open);
- regex off the request path;
- caller-placed cache breakpoints;
- steer, max turns, unknown tool, empty response and retry into
  goose-agent;
- a `<turn-context>` off switch;
- our two switches.

## Decisions for Paul

1. **Drive-mode jobs as a platform addition.** It is generic and names
   no runtime, but it changes how a job runs: live and resumable, not
   replayed. It is the cost-per-turn fix, for JS jobs too.
2. **The computers move to upstream main's goose.** That means the state
   machine loop and new Stop semantics, so one rev serves both runtimes.
3. **Templates and the shell move into Static Assets.** The cell drops
   to about 5 MB. Names stay in the WASM for now.
4. **Phases 0, 2 and 3 go to master as PRs**, as platform work. Phases 4
   and 5 stay on optchat.
