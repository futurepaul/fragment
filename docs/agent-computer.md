# The agent computer: the brain in the cell, the hands on a computer

Status: **decided 2026-09-27** (Paul; ROADMAP decision 24, which amends
decision 10). Slices 1 to 4 are built (below, "Slice 1 as built",
"Slice 2 as built", "Slice 3 as built" and its "Memory follow-ups",
"Slice 4 as built"); slice 1 is measured on Paul's pet, and slices 2
and 3 wait for their real Sprite acceptance. Sources were checked on
2026-09-27, and the dependency cooldown is 2 days.

## The decision

The blessed path is **chat → agent**: the brain runs in the cell, and
the hands are on a computer. It is the same split as Grok's bot and
Meta's Muse.

- **One interface: the chat.** A chat is a fragment's `chat` channel.
  The desktop, custom pages, and bridges (Telegram bridge fragments)
  read and post to it.
- **The brain** is the in-cell agent. It is thin and friendly, with a
  minimal system prompt. It answers, and calls operations on the owner's
  fragments through APIs: cheap, instant, and no computer. It hands off
  anything that writes code, uses a browser or desktop, or takes more
  than a few calls.
- **The hands** are goose on a computer. They are technical, with a
  generic, well-evaluated system prompt, and have files, bash, Stagehand
  with system Chrome, and Cua Driver for desktop apps.
  - There is one long-lived session per chat. goose's own compaction
    keeps the prefix stable, so the cache holds.
  - Steps and answers go into that chat.
  - A hand-off goes to the owner's computer, into that chat's session.
    Throwaway computers are extra hands, for parallel or risky work.
  - The hands are the backstop: a full Linux computer.
- **Fragment agents** (the `agent` block) follow the same pattern scoped
  to one fragment. They hand off only when the fragment declares a
  computer.
- **No multiple agents per person.** Work is organized by project, into
  fragments.
- **Not now:** Sprites checkpoints (the disk survives sleep), and a
  second main agent.

## Why: what we measured (2026-09-27)

The pet's `do` took 6–12 s a step, with 30–80 s gaps. The causes:
- **A trimming proxy.** `do.mjs`'s proxy rewrote earlier messages every
  turn, so the cache missed.
- **Random providers.** Requests went to random OpenRouter providers.
- **Mandatory reasoning.**
- **Two turns per look:** a screenshot saved to a file, then
  `read_image`.
- **goose v1.50.0's own summaries.** Its tool-pair summarization is on
  by default and rewrites history too
  ([v1.50.0](https://github.com/aaif-goose/goose/blob/v1.50.0/crates/goose/src/context_mgmt/mod.rs)).
  v1.52.0 turns it off
  ([3952f64](https://github.com/aaif-goose/goose/commit/3952f64e7)).

With a stable prefix, caching works: 11.7k of 11.8k tokens were cached,
and a call took 1.4 s instead of 4 s, at a fifth of the cost.
`provider.sort=latency` gave tiny calls of 0.4–0.7 s, because a sort
replaces load balancing with a fixed order
([routing](https://openrouter.ai/docs/features/provider-routing)).

## Hermes: what we match

Hermes Agent is Nous Research's MIT-licensed agent, at v0.21.5 on
2026-09-24 ([releases](https://github.com/NousResearch/hermes-agent/releases)).
Its "Bot Screen" gives each bot an Xvnc desktop, streamed to Hermes
Desktop, where a person can take over and hand back
([bot-screen](https://hermes-agent.nousresearch.com/docs/user-guide/features/bot-screen)).

| Capability | Hermes | Here |
| --- | --- | --- |
| Sessions | SQLite; one agent per session "so a conversation reuses its cached prompt prefix" ([config](https://hermes-agent.nousresearch.com/docs/user-guide/configuration)) | one goose session per chat |
| Compaction | summarizes the middle, keeps head and tail ([compression](https://hermes-agent.nousresearch.com/docs/developer-guide/context-compression-and-caching)) | goose's compaction |
| Memory, skills | `MEMORY.md`, `USER.md`, agentskills.io `SKILL.md` ([memory](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory), [skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills)) | files in a private fragment of the person's, `memory.<username>` (slice 3 as built) |
| Browser, desktop | CDP snapshots; cua-driver over MCP ([browser](https://hermes-agent.nousresearch.com/docs/user-guide/features/browser), [computer-use](https://hermes-agent.nousresearch.com/docs/user-guide/features/computer-use)) | Stagehand on system Chrome; Cua Driver |
| Screen | Xvnc, noVNC, a relay | step blobs, then the VNC relay |
| Scheduling, messaging | gateway cron, 30+ chat apps ([cron](https://hermes-agent.nousresearch.com/docs/user-guide/features/cron), [messaging](https://hermes-agent.nousresearch.com/docs/user-guide/messaging/)) | crons and jobs; chats and bridge fragments |

## The hands: `goose serve` on the computer

**Which goose.** v1.52.0 (2026-09-23) is the newest release; its musl
tarball's SHA-256 is `fdc86653…9f9f`
([release](https://github.com/aaif-goose/goose/releases/tag/v1.52.0)).
`goosed` and `/reply` are gone since v1.42.0
([v1.42.0](https://github.com/aaif-goose/goose/releases/tag/v1.42.0)),
and there is no `goose web`.

**What `goose serve` is.** It speaks ACP (the Agent Client Protocol) at
`127.0.0.1:3284/acp` and needs `GOOSE_SERVER__SECRET_KEY`
([CLI](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/guides/goose-cli-commands.md)).
goose Desktop runs it
([remote](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/guides/remote-goose-server.md),
[ACP](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/gdk/acp/index.md)).
It supports `loadSession`, session list/delete/close, and image prompts
([server.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/acp/server.rs)).

**How a client uses it.** A client sends `session/new` or
`session/load`, then `session/prompt`. It reads `session/update`
(`tool_call`, `agent_message_chunk`), and may send `session/cancel`
([setup](https://agentclientprotocol.com/protocol/session-setup),
[turn](https://agentclientprotocol.com/protocol/prompt-turn)).
Sessions persist in `~/.local/share/goose/sessions/sessions.db`
([sessions](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/guides/sessions/session-management.md)).

**What runs on the computer.** Two Sprites services run beside `start`:
- `fragment model --serve --port 8765`, now long-lived;
- `goose serve` on loopback, with a 0600 secret made on the machine,
  and this environment:

```
GOOSE_PROVIDER=openrouter  OPENROUTER_HOST=http://127.0.0.1:8765
OPENROUTER_API_KEY=unused  GOOSE_MODEL=z-ai/glm-5.3-flashx
GOOSE_CONTEXT_LIMIT=64000  GOOSE_MODE=auto  GOOSE_DISABLE_KEYRING=1
GOOSE_DISABLE_SESSION_NAMING=1
```

goose's extensions:
- `developer`: the shell, with the `fragment` CLI signed in as the
  computer;
- `browser`: our Stagehand MCP server;
- `cua`: Cua Driver, where there is a display.

**One session per chat.** Each chat has one session, named by the chat
(`<fragment>/<channel>`) and kept in `~/.fragment/agent/sessions/`. The
chat's next hand-off loads it.

**What slice 1 must check.** That a reconnect reuses the loaded agent,
so the next task's first call is cached. If it does not, a small
resident client holds `goose acp` over stdio instead, on the same
protocol.

## Hand-off, steps, and answers

**How a task reaches the hands.**
1. The brain calls `platform__hand_off({task})` (docs/api.md). By
   default this goes to the owner's computer, naming the asking chat. A
   throwaway builder is used only when the brain asks for extra hands.
2. The computer fragment's `do({task, chat})` runs one
   `job.computer.exec`. That wakes the computer, holds it awake, and
   runs the task client (`computer/do.mjs`, later the CLI).
3. The task client loads the chat's session and runs `session/prompt`.

**What comes back.**
- Each `tool_call` becomes a step on the chat's `work`:
  `{kind: "step", n, tool, args, said, shot}`.
- The final text becomes the answer on its `chat`.
- The computer posts there as its owner's computer; the hand-off grants
  it that.
- The job answers `{message, code}` to the brain's alarm, which gives the
  brain the result as a note (below).

**The brain never speaks for the hands** (Paul, 2026-09-27). In Paul's
chat on fragment.club the brain, asked a follow-up, handed it off and
wrote in the same reply "The computer is done." and an invented answer
(1908; the real run said 1886), twice. Hand-off results were stored in
its conversation as its own messages, opening "The computer is done.",
and the model learned to complete "on its way …" with one. Now:
- **The computer answers as itself.** The task client posts goose's
  answer on the chat's `chat`, `{text, turn}` under the hand-off's turn,
  so the page shows it as the computer's, its steps above it. The brain
  posts in the chat only what the computer could not say (a run that
  failed).
- **The brain reads results as input.** A finished hand-off lands in its
  conversation as a note only the model reads, labeled as the computer's
  words and naming the task, so later questions build on it, with
  nothing in the brain's voice to imitate. Results stored the old way
  become notes too.
- **A turn that hands off ends there.** Once the hand-off's result is
  stored, the platform says `On its way: <computer> has it, …` and the
  turn ends; the model is not asked again. A fixed acknowledgement is the
  simplest robust shape: nothing it writes after the call can be kept or
  cut wrong, it is read from the stored turn (so a replaced driver ends
  it alike), and it saves a model call. What the model says before the
  call is its step's text on `work`. The work guide says the turn ends
  there, so do its own part first, and not to describe results it has
  not received.

**Other callers.** A cron that calls `do` takes the same path, and
triggers wake the computer, so goose's own scheduler is not used. A
fragment's own agent hands off the same way, to that fragment's
computer.

## Where state lives

**The computer owns working state:** goose sessions, browser profiles,
installs, caches, and scratch. Losing it costs working memory and logins
only.

**The fragment owns what is useful or shared:**
- the conversation (channels);
- step summaries, with screenshot blobs;
- outputs (fragments, files, artifacts);
- the task log and schedules;
- memory and skills, as git files: `memory/*.md` and
  `skills/<name>/SKILL.md` in the person's own private fragment,
  `memory.<username>` (slice 3 as built, below).

The task client syncs that fragment to `~/memory` before and after each
task, and commits what goose wrote to `main`. Its skills are linked into
goose's `~/.agents/skills`
([skills](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/guides/context-engineering/using-skills.md));
its facts reach each session as prompts. goose's own memory extension
([memory](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/mcp/memory-mcp.md))
is not used.

**Grants and secrets** live on the platform, never on the computer's
disk.

**A replacement computer** seeds each chat's new session from that
chat's transcript, plus the memory and skills files (the memory and
skills are built; the transcript is not yet).

## Compaction and caching

**Compaction.** goose compacts at `GOOSE_AUTO_COMPACT_THRESHOLD` (0.8)
of the context limit, which `GOOSE_CONTEXT_LIMIT` overrides
([env](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/guides/environment-variables.md)).
It counts the tokens the provider reports. One call summarizes the
visible history, and the system prompt and tools stay. So each
compaction costs one cache miss. Old images leave at compaction; nothing
strips them turn by turn
([context_mgmt](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/context_mgmt/mod.rs)).
With 64k, compaction comes near 51k tokens, not at 80% of flashx's 1M.

**A stable prefix.** Between compactions, each request is the last one
plus new messages, so each screenshot is paid in full once. The system
prompt's time is fixed per agent, truncated to the hour, "so that prompt
cache can be used"
([prompt_manager.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/agents/prompt_manager.rs)).
A long-lived agent keeps its prefix; a process per task does not.

**goose's OpenRouter provider.**
- It takes `OPENROUTER_HOST` and posts to `api/v1/chat/completions`
  ([openrouter_def.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/providers/openrouter_def.rs)).
- It merges `OPENROUTER_PARAMETERS`, sets `session_id` to the goose
  session, and always adds `transforms: ["middle-out"]`
  ([openrouter.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose-providers/src/openrouter.rs)).
- The `session_id` keeps one provider until 10 idle minutes pass, and
  Z.AI caches implicitly
  ([caching](https://openrouter.ai/docs/features/prompt-caching)).

**The platform endpoint.**
- It must pass `session_id` through.
- It should drop `transforms`, so an overflow errors and goose compacts.
- Its 2 MiB cap (docs/api.md) becomes 8 MiB, with `--serve`'s to match,
  since no proxy trims any more.

Cua Driver's `max_image_dimension` and Stagehand's text steps keep
requests small. goose v1.52.0 also compacts on image-limit errors
([#12208](https://github.com/aaif-goose/goose/pull/12208)).

## Models

Prices are per 1M tokens, from
[OpenRouter's models API](https://openrouter.ai/api/v1/models).

| Model | In / out / cached | Notes |
| --- | --- | --- |
| `z-ai/glm-5.3-flashx` (2026-09-18) | $0.37 / $1.25 / $0.09 | reads images; one provider (Z.AI); `response_format`, no `structured_outputs` |
| `z-ai/glm-5.3-flash` (the brain's) | $0.045 / $0.14 / $0.01 | lists `structured_outputs` |
| `typesafe/jev-router` (2026-09-25) | variable (−1) | a router: picks the model (stealth ones too, which Paul accepted) and its reasoning effort; no parameters listed |

**Images.** goose sends a tool's image only to a model its catalog says
reads images, looked up by (provider, model)
([model.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose-provider-types/src/model.rs)).
The catalog lists `openrouter/z-ai/glm-5.3-flashx` with image input
([catalog](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose-provider-types/src/canonical/data/canonical_models.json)).
So the built-in `openrouter` provider with the real model name replaces
the `GOOSE_MODEL=gpt-4o` hack. Custom providers have no vision flag
([#12520](https://github.com/aaif-goose/goose/issues/12520)).

**The hands' model** is flashx. It is admitted by the platform's model
allowlist, which another PR is adding along with latency routing and
reasoning passthrough. Reasoning is set once, in
`OPENROUTER_PARAMETERS`.

**Browser reasoning** is flashx, in JSON mode, or with one setting Jev
first: Stagehand's callback sends `typesafe/jev-router` with a
`json_schema` response format and no reasoning effort, which Jev picks,
and flashx answers when Jev's answer does not fit (Browser fixes, below).

No key is on the computer.

## Tools

**Stagehand.** Stagehand 4.1.0 (2026-09-09) is `latest`, and 3.7.3 is
`v3-latest`
([npm](https://www.npmjs.com/package/@browserbasehq/stagehand?activeTab=versions)).
- **Browser.** v4 drives the system Chrome over CDP, with no
  Playwright, and never downloads a browser
  ([localBrowser.ts](https://github.com/browserbase/stagehand/blob/main/packages/sdk-ts/src/browser/localBrowser.ts)).
  `localBrowser.connect({cdpUrl})` attaches to a running Chrome. Its
  runtime is an extension loaded over CDP, which needs
  `--enable-unsafe-extension-debugging`
  ([browser](https://docs.stagehand.dev/v4/configuration/browser)).
- **Model.** Without a base-URL option, any other endpoint is a
  `generate` callback returning JSON-schema "structured generations"
  ([models](https://docs.stagehand.dev/v4/configuration/models)).
- **Replay.** `act` on an observed `Action` "replays it
  deterministically with no inference"
  ([act](https://docs.stagehand.dev/v4/basics/act)).
- **Caching.** v4 caches only on Browserbase's servers, and v3's local
  `cacheDir` and `agent()` are gone
  ([caching](https://docs.stagehand.dev/v4/best-practices/caching),
  [v3 → v4](https://docs.stagehand.dev/v4/migrations/v3)).

**Our browser MCP server.** Browserbase's MCP server needs cloud
browsers
([sessionManager.ts](https://github.com/browserbase/mcp-server-browserbase/blob/main/src/sessionManager.ts)),
and Stagehand's own ships only as source
([integrations](https://docs.stagehand.dev/v4/integrations/overview)).
So `computer/browser/browser-mcp.mjs` is ours: JSON-RPC by hand (slice
2 as built), offering `open`, `act`, `observe`, `extract`, and
`screenshot`. goose is the agent and Stagehand its hands, v4's own
split.

**Chrome, on the Sprite's Ubuntu** (26.04, per phase 8's smoke).
- **Why not Ubuntu's.** Its `chromium-browser` is a transitional
  package that installs the snap
  ([Ubuntu](https://ubuntu.com/blog/chromium-in-ubuntu-deb-to-snap-transition),
  [resolute](https://packages.ubuntu.com/resolute/chromium-browser)).
- **What we install.** Google's repository serves
  `google-chrome-stable` 154.0.8037.57-1: 142 MB, SHA-256
  `66c0645f…5a3e`
  ([Packages](https://dl.google.com/linux/chrome/deb/dists/stable/main/binary-amd64/Packages)).
  The .deb configures apt ([repos](https://www.google.com/linuxrepositories/)),
  and Chrome supports 64-bit Ubuntu 18.04+
  ([requirements](https://support.google.com/chrome/a/answer/7100626)).
- **Cua Driver needs it too.** It launches only "a platform-attested
  system Chrome/Edge installation (or a root-owned package payload on
  Linux)"
  ([Linux tools](https://github.com/trycua/cua/blob/cua-driver-rs-v0.28.3/docs/content/docs/reference/cua-driver/mcp-tools-linux.mdx)).
  Playwright's Chromium in `~/.cache` is neither.
- **How it runs.** Chrome runs on the display (`:99`), so viewers watch
  the window the agent drives. It starts with
  `--remote-debugging-port=9222` on loopback and
  `--enable-unsafe-extension-debugging`.

**Cua Driver 0.28.3** (2026-09-24) is the newest release past the
cooldown ([releases](https://github.com/trycua/cua/releases)). It is
narrowed to `launch_app`, `list_windows`, `get_window_state`, `click`,
`type_text`, `press_key`, `hotkey`, and `scroll`. `get_window_state`
returns the tree and screenshot inline. Only `screenshot_out_file`
writes a file, which was our two-turn look (same page), and the
instructions forbid it.

## Screens

**What the model sees.** Screenshots live inside model requests. goose's
`sessions.db` keeps them on the computer; the platform stores none.

**What people see.** After each step, the task client captures the
display as a JPEG of at most 80 KB. It uploads it with a new
`fragment blob put`, and the step names the hash.
- A computer is an editor, so it may already
  `PUT /api/f/{name}/blobs/{sha256}`.
- A blob no pointer names is deleted after 7 days
  (`FRAGMENT_BLOB_GRACE_S`), by a sweep that runs at most daily. This
  includes uploads never committed (cell/src/blobs.rs).
- Pages read a blob by hash at `__blob/<sha>` on the fragment's origin,
  for viewers and up, serving only this fragment's blobs, cached as
  immutable (slice 4; before it, only the platform's host answered, to
  signed callers).
- The store is Tigris: $0.02 a GB-month, $0.005 per 1,000 PUTs, and
  free egress ([pricing](https://www.tigrisdata.com/pricing/)), with
  per-prefix expiry
  ([expiration](https://www.tigrisdata.com/docs/buckets/objects-expiration/)).

**The live view** is the VNC relay, later
(docs/screen-streaming.md, option 1). Its dial-out bridge can also serve
`__screen/now.jpg`, the current screen, stored nowhere. Until then, the
pet's `frame` row stays: it is one replaced row, not history.

## What we delete

- **Slice 1:**
  - the trimming proxy;
  - the per-run model endpoint;
  - `GOOSE_MODEL=gpt-4o`;
  - `goose run --no-session`;
  - the throwaway builder as the default hand-off.
- **Slice 2:** Playwright's Chromium.
- **Slice 4:** the frame's base64 JPEG (in `frame`'s input, the pet's
  row, and its page's data URL).
- **The relay:**
  - `frame`, and `screen` and its row;
  - the capture loop;
  - `control` and the xdotool follower.

## Costs (estimates)

A step here is a 30k-token prefix (mostly cached), plus 2k new tokens and
300 out.

| Item | Cost |
| --- | --- |
| Awake time (docs/computers.md) | $0.0726 an hour |
| A flashx step, cached / uncached (the old proxy) | about $0.0036 / $0.012 |
| A flash step, cached | about $0.0004 |
| An hour of nonstop hands (360 steps) | about $1.30 on flashx, $0.15 on flash |
| Jev router | varies; charged as OpenRouter reports |
| Step screenshots (1,000 a day at 60 KB, kept 7 days) | about $0.01 a month in storage, $0.15 in PUTs |
| Disk: Chrome, goose, Node modules (under 1 GB) | about $0.02 a month asleep |

With awake time included, a $20 month buys about 14 hours of flashx
work, or about 90 on flash.

## Slices

**1. The long-lived per-chat session service; hand-off defaults to it.**
- **What changes:**
  - goose v1.52.0;
  - `fragment model --serve` and `goose serve` as services;
  - `do.mjs` becomes an ACP client (`@agentclientprotocol/sdk` 1.5.0).
    It loads the chat's session, prompts it, and posts steps and the
    answer into the chat;
  - `platform__hand_off` defaults to the owner's computer, naming the
    chat;
  - the proxy and `gpt-4o` go;
  - `--serve` answers `/api/v1/chat/completions`;
  - the model route's cap is raised to 8 MiB;
  - flashx arrives with the allowlist PR.
- **Acceptance on a real Sprite** (usage rows, goose's usage updates):
  - two hand-offs from one chat share a session;
  - the second one's first call is at least 80% cached;
  - the median step takes 3 s or less, and no gap is over 10 s;
  - another chat gets its own session;
  - after a restart, a chat's next task recalls its earlier one.
- **Acceptance in the e2e** (an ACP stand-in for `goose serve`): the
  task becomes a prompt, steps and the answer reach the asking chat, and
  requests pass through unedited.
- **Size:** template +150/−120, CLI +10, cell +10, e2e +200.

**Slice 1 as built** (docs/computers.md, the hands; docs/api.md,
Hand-offs). Where it differs from the plan above:
- **One service, `hands`**, runs `fragment model --serve` and `goose
  serve` together (a script the platform writes at each sync, on every
  computer), so goose always points at the model endpoint that is up.
  Each binds a free port (the model's `--port 0`; goose the first free
  from 3284, in `~/.fragment/agent/port`), since the e2e runs every
  Sprite on one machine.
- **The task client is the platform's** (`~/.fragment/agent/task.mjs`,
  written with the service), so the pet's `do` and the builder's `build`
  share one. It speaks ACP over goose serve's WebSocket, JSON-RPC by
  hand (about 60 lines), instead of `@agentclientprotocol/sdk`: nothing
  is installed at run time for it.
- **Steps are `turn.step` records**, the ones the chat page renders
  already, under the hand-off's turn, which the computer's answer names.
- **The grant**: the hand-off makes the computer an editor of the chat
  (the second owner-only action an agent takes; the platform takes it
  only for its owner's own computer and chat).
- **A reconnect does not reuse the loaded agent** (goose's source: each
  connection gets its own `AcpServer::create_agent`, and a session load
  builds a new one). A task's first call is cached as the last task's
  within the hour its system prompt names; after the hour turns, it
  misses once. The resident client over `goose acp` stays the fallback,
  if the measured miss matters.
- **Reasoning** is `OPENROUTER_PARAMETERS={"reasoning":{"effort":"low"}}`,
  which every model takes; `{"enabled": false}` is the knob to try if
  flashx allows it.

**2. Stagehand on system Chrome.**
- **What changes:** the checked Chrome .deb replaces Playwright's
  Chromium; `browser-mcp.mjs` is added; Cua Driver goes to 0.28.3,
  narrowed to desktop apps.
- **Acceptance:**
  - a web task (find a price, add it to a todo) sends the model no
    screenshot;
  - 20 Jev act/extract calls pass Stagehand's schema check, or flash
    takes over;
  - Blender (from apt) opens through Cua, and a menu click lands.
- **Size:** +250/−60.

**Slice 2 as built** (docs/computers.md, the pet's agent). Measured on
Paul's pet after slice 1: a model call took 4–5 s for 10–70 tokens out,
where the same model answers a 22k-token cached text-only prompt in
about 1.5 s; the difference was the screenshots in context from Cua
Driver's `get_desktop_state`. So web tasks go through each page's
structure. Where it differs from the plan above:
- **Chrome** is `google-chrome-stable` 154.0.8037.57-1 (stable since
  2026-09-22), its .deb from Google's pool checked against the SHA-256
  in Google's apt index, installed by `pet.mjs` with apt and without
  Google's apt source (`repo_add_once="false"`), so nothing upgrades it.
  It replaces Playwright's Chromium on the pet's screen. Its CDP is on
  loopback port 9222, with `--enable-unsafe-extension-debugging`, and
  `--remote-allow-origins` naming Stagehand's runtime alone: that
  unpacked extension's service worker opens its own socket to CDP, which
  Chrome refuses from an origin it was not told of ("CDP websocket failed
  to open"); the extension's id is its path's hash, which `pet.mjs`
  computes.
- **The server is JSON-RPC by hand**, as the task client is, not on
  `@modelcontextprotocol/sdk`: nothing but Stagehand is installed for it.
  No maintained server runs Stagehand locally: Browserbase's
  `mcp-server-browserbase` 2.4.3 and `@browserbasehq/mcp` 3.0.0 are on
  Stagehand v3 and cloud browsers, Stagehand's own integrations ship as
  source, and `stagehand-mcp` 1.0.10 is on Stagehand v2. It is
  `templates/pet/computer/browser/browser-mcp.mjs` (about 180 lines),
  and it attaches to Chrome at its first call, so a task that never
  browses never touches Chrome.
- **Installed per lockfile**: the pet's `do` runs `npm ci
  --ignore-scripts` from `computer/browser/package-lock.json` (Stagehand
  4.1.0 and the 41 packages under it, each pinned and checked by its
  `integrity`, all past the cooldown, none with install scripts) into
  `~/.local/share/pet-browser`, and again only when the lockfile
  changes. Still in the pet (slice 3 moves the runtime into every
  declared computer).
- **Its model**: Stagehand's `generate` callback posts to goose's
  `OPENROUTER_HOST` (the extension inherits goose's environment: `fragment
  model --serve`), unstreamed, with Stagehand's JSON schema as
  `response_format` and `reasoning.effort: low` (both changed since:
  Browser fixes, below). Jev first; when its
  answer is not JSON that fits the schema (checked in the callback, as
  far as Stagehand's schemas go), flashx answers the same request, not
  flash (Paul: flashx by default everywhere). Each call is a line of
  `~/.fragment/agent/browser.log`: `{model, schema, ms, ok, why}`. A
  Jev call holds $0.50 of its owner's month until it settles
  (`ROUTER_RESERVE`), so with less than that left Jev is refused (402)
  and flashx answers.
- **Whether Jev works with Stagehand is not known yet.** Nothing here
  called the real router. On Stagehand 4.1.0 with a real Chrome (153,
  locally) and a stand-in model, this server opened a page, filled a box
  and clicked a button, observed, and extracted; the fallback took over
  when the first answer was not JSON. The real pet's `browser.log` says
  which model answers; if Jev rarely fits, `MODELS` loses it.
- **Stagehand's traces**: its runtime exports OpenTelemetry traces to
  `https://example.com/v1/traces` unless told otherwise; the server
  points them at a closed loopback port.
- **Cua Driver** is narrowed to desktop apps: `get_desktop_state` goes,
  `launch_app` comes (the plan's eight). `~/.cua-driver/config.json`
  caps its screenshots' long edge at 768 (the default, 1568, was over
  the pet's whole 1024×640 screen; now 768×480). The task's first
  paragraph says which tools are for what; goose's system prompt stays
  its own.
- **What a step costs**: `act` and `observe` are one model call each,
  `extract` two (Stagehand's extraction, then its "is it complete"
  check). On a local Chrome, `act` also waited about 0.5 s for the page
  to settle (Stagehand's `domSettleTimeoutMs`; at 100 ms it took 0.13
  s): a knob for the real pet's numbers, left at its default.
- **The e2e** (`templates`; stand-ins for goose, Stagehand, and Cua
  Driver): the hands' goose is offered the browser's five tools and Cua
  Driver's desktop ones; a browser task (`open`, `extract`, `act`) goes
  through the real server to the model through `--serve`, as Jev with
  Stagehand's schema, flashx when Jev's answer does not fit, billed to
  the owner, in `browser.log`; no browser step's request carries a
  screenshot, and Cua Driver's still reaches the model.
- **Size:** template +251/−44 (the server 180; the lockfile, 490 lines
  of generated JSON, not counted), e2e +123/−25, the OpenRouter fake
  +10/−5 (an unstreamed answer takes the script's text), docs.

**Browser fixes** (after the first real run, 2026-09-27). Asked in a new
chat for Hacker News' top 3 stories, the pet's goose used `open`,
`extract` twice, `observe`, then `screenshot`, and answered right from
the screenshot: every Stagehand model call had failed (`browser.log`).
- **Jev refused our reasoning effort**: `OpenRouter (400): No configured
  model/effort candidate satisfies the requested reasoning effort…`
  (Extraction, Observation). The effort was the browser server's own
  (`reasoning.effort: low`); nothing on the way adds one. Jev picks a
  model and its effort itself, so its requests name none. flashx keeps
  `low`, as goose's do.
- **Jev picked a stealth model** (`stealth/space-bunny-alpha`, provider
  "Stealth", free), whose markdown list did not parse. **Jev may route to
  stealth models: Paul accepted this on 2026-09-27** ("it's fun"), so
  its requests name no provider preferences, and a stealth pick's
  malformed answer falls back to flashx like any misfit. Should that
  change: OpenRouter's `provider: {ignore: ["stealth"]}` (the cloaked
  models' provider slug) and `data_collection: "deny"` (only providers
  that neither store nor train on prompts; space-bunny-alpha's endpoint
  keeps them) exclude them, though whether Jev's router applies
  preferences to its pick is not documented (the Auto Router applies
  them to "the endpoints of whichever models the router resolves"). Jev's
  own listing lists no parameters.
- **flashx ignored the JSON schema**: its one provider lists
  `response_format` (JSON mode) but not `structured_outputs` (a schema),
  and OpenRouter drops a parameter no provider takes, so it answered
  Stagehand's prompt (which never states the shape) in markdown: "1. …"
  ("Unterminated fractional number in JSON at position 2") and a bare
  array of elements for Observation ("does not fit"). Now flashx is asked
  for JSON mode (`{type: "json_object"}`) from a provider that takes it
  (`require_parameters`), Jev for the schema, and the schema is said in
  the system prompt. `answer.mjs` reads the reply: the JSON in it (fences,
  words or a number before it, reasoning after it), a schema's only field
  given bare (the bare array), or plain words where that field is text
  (an extraction); JSON cut short, or JSON of another shape, is refused,
  so the next model answers. A unit test runs it on the replies that
  failed (crates/templates). `browser.log` also names the model that
  answered (`by`: Jev's pick).
- **Jev is not forced** (Paul: "obviously we don't force jev if it's not
  actually helping"). One setting in `browser-mcp.mjs`, `JEV_FIRST`,
  picks flashx alone (the default) or Jev and then flashx. flashx alone
  is the default on the evidence there is: on the real run flashx's three
  answers held the right content in the wrong wrapper, which
  `answer.mjs` now reads (3 of 3, replayed in the unit test), while Jev
  fitted none of three (two refusals this fixes, one stealth list that
  `answer.mjs` would now read as an extraction). Jev first also costs a
  routing decision on each call and, when its answer misfits, a whole
  wasted call before flashx, and holds $0.50 of the month per call
  until it settles. Nothing here measured either model's latency (no
  real OpenRouter calls). **What flips it**: run 20 or more Stagehand
  calls each way on the pet and compare `browser.log`. Jev first wins
  when its first-try `ok` rate is at least flashx's and the median `ms`
  from a call's start to its answer (Jev's, or flashx's after a Jev
  miss) is below flashx alone's.
- **A session kept its old tools.** goose stores a session's extensions
  when it is made and loads it with those, not with its config's
  (`session/load` adds only the `mcpServers` a client passes; a failed
  extension is dropped from the session too). So Paul's chat from before
  slice 2 had Cua Driver's tools and never the browser. The task client
  now compares a loaded session's MCP extensions with the config's
  enabled ones (goose's ACP methods
  `_goose/unstable/config/extensions/list` and
  `_goose/unstable/session/extensions/list`) and removes and adds where
  they differ (`…/session/extensions/remove`, `…/add`), which goose
  persists: one cache break per change, none otherwise. goose adds no
  extension with inline `envs` that way, so Cua Driver's environment is
  in its command (`/usr/bin/env DISPLAY=:99 … cua-driver mcp`).
- **The e2e**: goose's stand-in keeps each session's extensions as goose
  does, with those ACP methods; a session made on a Cua-only config gets
  the new tools on its next task, in the same session, and the task after
  changes nothing (the same tools, in order). By default every Stagehand
  call is flashx's, in JSON mode, and its markdown list and bare array
  are read as Stagehand's shapes. Then `JEV_FIRST` is turned on in the
  pet's own files: the OpenRouter fake refuses a Jev effort as Jev did,
  Jev's asks name none and no provider, and Jev's JSON cut short falls
  back to flashx.
- **Size**: template +137/−48 (`answer.mjs` 93, 30 of them the schema
  check moved out of the server), task client +20/−1, e2e +233/−63, the
  unit test +50, the fake +6/−1, docs.

**Session tools refresh** (after the browser fixes deployed,
2026-09-27). On Paul's pet a new chat's session used Stagehand (`open`,
`extract`: 2 of 2 calls ok, 1.5 s each), but his older chat, its session
from before slice 2, given a web task, still used Cua Driver's
`get_desktop_state` and `hotkey` and never the browser.
- **Not a name-only comparison.** The refresh already compared each
  extension's whole definition as goose says it (command, args, tool
  allowlist, timeout, description). goose v1.52.0's source agrees with
  the e2e's stand-in: `…/session/extensions/add` restarts a same-named
  extension whose definition differs and stores the session's new set.
- **Most likely, the pet never ran the new task client** (not confirmed
  there; its `hands.log` now says). The platform writes `task.mjs` at a
  computer's sync, and a synced computer synced again only for a new
  commit of its own fragment, `start`, goose, or CLI, not for a new task
  client. A pet that synced its new template files before the platform
  deploy that carried the refresh kept the old client, which has none:
  the old tools, and no browser. Now what a computer synced names a
  digest of the hands' service and task client, so a platform deploy
  that changes either syncs each computer once (awake, at its next tick;
  asleep, when it wakes), as a CLI bump does, `start` restarting too.
- **And nothing could be seen.** A failed refresh was said on the task's
  stderr, which a run shows only when goose said nothing. And the
  refresh read only goose's stored list, which goose answers from the
  config when it cannot read the session's own, so a session that runs
  other tools looked current.
- **Now**, before the prompt: each of the config's enabled MCP extensions
  is added (goose restarts a same-named one on the new definition) when
  the session's copy is not its whole definition, or when goose's tool
  list for the session (`_goose/unstable/tools/list`, what the model is
  sent) has none of its tools or one its allowlist leaves out; one no
  longer configured is removed; built-ins stay. A replaced one is added
  over, not removed first, so an add that fails leaves the old one. One
  cache break per change; with nothing to change, the three lists are all
  it asks. Each change is a line of `~/.fragment/agent/hands.log`, names
  only, with whether goose then offers the configured tools:
  `[task] <time> <chat> session <id>: browser added, cua replaced; goose
  offers the configured tools`. A failure is a line there too (and on
  stderr). goose says no extension's `envs`, so a change of those alone
  is not seen: configs keep their environment in the command.
- **The e2e** (`templates`): the stand-in Cua Driver lists the pet's
  eight tools and two it leaves out, and the stand-in goose answers
  `tools/list`. The session is made on the pet's Cua Driver definition
  with the tools from before the browser (`get_desktop_state`, no
  `launch_app`) and no browser; its next `do`, in the same session, is
  offered exactly the shell, Cua Driver's eight, and the browser's five,
  the config (checked) differing from the old only in the tool list, and
  `hands.log` names the change; the task after changes nothing and logs
  nothing.
- **Size**: task client +40/−10, the sync's key +14/−5, e2e +72/−32,
  docs.
- **On the pet after this deploys**: it syncs once, and the older chat's
  next task either logs `browser added, cua replaced` (the pet had run
  the old client) or logs nothing: goose already offered the config's
  tools, and the model called `get_desktop_state` from its history (that
  step's `ok` says it failed).

**Told of the change** (after the refresh deployed, 2026-09-27). On
Paul's pet the older chat's session was refreshed (`hands.log`: `cua
replaced, browser added; goose offers the configured tools`), but goose
still drove the browser through Cua (`list_windows`, `hotkey`,
`type_text`, `get_window_state`), copying its own earlier steps from the
session's history; a fresh session uses `open` and `extract` and is
about twice as fast.
- **Now** a task whose refresh changed the session's tools starts with a
  note built from the change and what goose then offers: `Your tools
  changed since your earlier work here.`, then per extension `New
  <key> tools: …`, `The <key> tools are now: …`, or `The <key> tools
  are gone.`, with a one-line hint for the known ones (`browser`: for
  anything on a web page; `cua`: only for native apps). Only that task:
  no change, no note, so no cache break; the note stays in the history
  like any prompt.
- **The e2e** (`templates`): the refreshed task's prompt is exactly the
  note, then the pet's own; the new session's task before and the task
  after are told nothing.
- **Size**: task client +25/−12, e2e +14, docs.
- **On the pet**: its older chat was refreshed already, so it gets no
  note until its tools change again; a new chat needs none.

**What slice 2 leaves to the real pet**: Chrome's install and
Stagehand's runtime loading there; per-call ms and cached share in
`model.log` for a web task (a price found, added to a todo), with no
screenshot in its requests; whether a browser step approaches 1.5–2 s;
and how many of 20 Jev calls fit (`browser.log`). A pet made before
slice 2 keeps Playwright's Chromium in `~/.cache/ms-playwright` until
someone removes it.

**3. Memory and skills in git.**
- **What changes:**
  - `agent/memory/` and `agent/skills/` are synced into goose's paths;
  - what goose writes is committed back;
  - a replacement computer seeds sessions from the transcript and these
    files;
  - the runtime moves from the pet into every declared computer.
- **Acceptance:**
  - a skill written in one chat is used in another;
  - a destroyed and replaced computer answers a follow-up about an
    earlier task;
  - a fragment without a computer gets none of it.
- **Size:** CLI about 400, cell 100, templates −250, e2e 250.

**Slice 3 as built: memory and skills** (docs/computers.md, the hands;
docs/api.md, Hand-offs and Agents). Where it differs from the plan above:
- **Where they live: one private fragment per person,
  `memory.<username>`**, not files in each computer's fragment. A person
  has several computers (Paul's answer 6) and throwaways, while memory is
  theirs; a pet's files are served from live, and a commit to its main
  would wait for a deploy. The agent makes it (members only, the owner's,
  with the agent an editor, as for anything it makes) at its owner's
  first hand-off, and makes each computer it hands work to an editor
  there, as it does the chat: the same owner-only grant, `PUT
  /api/f/memory.<username>/members/<computer>`. It holds `memory/*.md`
  (facts and preferences, a few lines per topic) and
  `skills/<name>/SKILL.md` (agentskills.io: `name` and `description`
  frontmatter, then the steps). The owner reads and edits it like any
  fragment (`fragment sync memory.<you> --dir …`). Every write goes to
  `main` (Paul's answer 5), and git history is the review and the undo.
  Nothing in it is deployed.
- **The task client syncs it** to `~/memory` before and after each task
  (`fragment sync memory.<username> --dir ~/memory --apply-mass-delete`,
  both ways; a deletion is goose's own, and git keeps what it removed).
  What goose changed is committed to `main` when the task ends, as the
  computer (the CLI signs; the commit's author is `fragment/<8 hex of its
  key>`), and what other chats and computers wrote is pulled. A computer
  that is not a member (a pet driven only from its page) has no memory,
  and says nothing about it (the sync answers `not_found` or
  `forbidden`). Any other failure is a line of `hands.log`. No CLI change
  was needed, so no CLI release.
- **Skills are goose's own.** goose v1.52.0's Skills platform extension
  is on by default ([platform_extensions](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/agents/platform_extensions/mod.rs)).
  It finds each `SKILL.md` under `~/.agents/skills`, among other places
  ([skills/mod.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/skills/mod.rs)).
  At every model call it lists them in the system prompt as `• name -
  description`, and its `load_skill` tool loads one
  ([skills/client.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/skills/client.rs)).
  The task client links `~/.agents/skills` to `~/memory/skills`, unless
  something is there already, so a skill goose writes lands in the
  memory. The cost: a new or changed skill changes the system prompt, so
  each session's next call misses the cache once, as it does when the
  hour turns (the system prompt names the hour).
- **Facts reach a session as prompts, not in its system prompt.** goose
  rebuilds its system prompt on every model call, reading its hints from
  disk again
  ([reply_parts.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/agents/reply_parts.rs),
  [prompt_manager.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose/src/agents/prompt_manager.rs)).
  Its memory extension puts every global memory into its instructions
  when it starts
  ([memory/mod.rs](https://github.com/aaif-goose/goose/blob/v1.52.0/crates/goose-mcp/src/memory/mod.rs)).
  Either way, each change would miss the cache for every session. So
  instead:
  - a session's first task starts with every `memory/*.md`: `Your
    owner's memory (memory.<username>, at ~/memory):`, then each file
    under its path;
  - a later task starts with the files changed since and those removed:
    `Your owner's memory changed since your earlier work here:`;
  - either note carries at most 8,000 characters, and names the files it
    leaves out, to be read.

  What a session was told is kept beside its id
  (`sessions/<chat>.memory`: each file's SHA-256), written once goose has
  taken the prompt. What goose wrote itself comes back once in the next
  task's note, since the task client cannot tell it from another chat's.
  That costs a few tokens and is never wrong. goose's compaction
  summarizes the history, notes included; the hints say where the files
  are, so goose can read them again. Hermes does the same thing
  differently: a frozen snapshot in the system prompt at session start,
  rebuilt at compaction
  ([memory](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory),
  [prompt assembly](https://hermes-agent.nousresearch.com/docs/developer-guide/prompt-assembly)).
- **How goose writes it**: with its own text editor and shell, as its
  hints say. The hints are a paragraph after the CLI's guide in
  `~/.config/goose/.goosehints`. They are static, so they break the cache
  only at the deploy that brings them. There is no tool of ours: the task
  client commits what goose wrote. Hermes has `memory` (add, replace,
  remove) and `skill_manage` tools instead
  ([memory_tool.py](https://github.com/NousResearch/hermes-agent/blob/v2026.9.24/tools/memory_tool.py),
  [skill_manager_tool.py](https://github.com/NousResearch/hermes-agent/blob/v2026.9.24/tools/skill_manager_tool.py)).
  goose's memory extension is not used, for three reasons: its files are
  `<category>.txt`, its instructions tell the model to confirm with the
  user before saving (the hands have no one to ask), and it adds four
  tools to every request.
- **The brain reads the facts.** Each of the owner's turns of their agent
  is told the facts at `main`, after its instructions, up to 2,000
  characters (Hermes' `MEMORY.md` holds 2,200). It reads `Your owner's
  memory, which their computer keeps in memory.<username> (you only read
  it: to add to it or change it, hand it off):`, then each file, or
  `nothing yet.`.
  - It is read as a turn starts: one listing (`GET
    /api/f/memory.<username>/files`, for the owner), and the files again
    only when `main` has moved (the agent keeps the view in its kv).
  - So "what do I drink?" is answered in the cell, with no computer.
  - A guest's turn is told nothing of it, and a fragment's own agent has
    none.
  - The brain's system prompt changes when the memory does. Its prefix
    already changes every turn, since earlier turns' results are cut.
- **Not built from the plan above**:
  - seeding a replacement computer's session from the chat's transcript.
    A replacement gets the memory and skills (its first session is told
    the facts) but not the chat's history;
  - moving the pet's browser and desktop runtime into every declared
    computer.
- **The e2e** (`builder`, after the home computer's sessions; the
  stand-in goose lists `~/.agents/skills` in its system prompt as goose
  does):
  - the hand-offs made `memory.<username>`, the owner's and members
    only, with the home computer an editor;
  - in one chat, goose remembers a fact and writes a skill (through
    `~/.agents/skills`), and both land on the memory's `main`;
  - a second chat's new session on the computer is told the fact in its
    first task and offered the skill, and its goose remembers another;
  - the first chat's next task starts with what changed, the other
    chat's fact among it, and its first request carries that chat's last
    request as its prefix, message for message;
  - the owner's agent answers "what do I drink?" from the facts in its
    prompt, with no computer;
  - a guest's turn is told nothing of it;
  - the hints hold the memory's paragraph;
  - the pet (`templates`), its computer no member until a hand-off
    reaches it, says nothing of a memory in its prompts or `hands.log`.
- **Size**: task client +62/−2, the hands' hints +12/−4, agent +130/−23
  (`memory.rs` 102), e2e +136/−4, docs. No CLI or cell change, where the
  plan guessed CLI 400 and cell 100.

**Memory follow-ups** (Paul, 2026-09-27, approving all four
recommendations after slice 3; `memory-followups`). These supersede
slice 3's first bullet, how a computer is granted, and "the brain reads
the facts" (read-only):
- **The platform records each person's memory**
  (`cell/src/memory.rs`; a `memories` table in the registry, one row per
  person; docs/api.md, `/api/memory`).
  - It is made on first need: members only, the person's, named
    `memory.<username>`, or the next free of `memory-2` … `memory-9`.
  - It is found by that record, never by its name, so a fragment the
    person made called `memory` is never taken over.
  - Two first needs at once record one; the other's fragment is deleted
    again.
  - A memory its owner deleted is made again, under its recorded name,
    at the next grant.
  - Its owner may name a fragment of their own as it: `fragment memory
    use <fragment>` (`PUT /api/memory`; never an agent's or a computer's).
    This is for the `memory.<username>` slice 3's agent made (#79 was
    deployed alone, so Paul has `memory.futurepaul`), which would
    otherwise be an ordinary fragment beside a new `memory-2`. Every
    computer of theirs is made an editor there, and each fragment's own
    syncs again (`Ask::Resync`), so its task client syncs the named one.
    Each memory has its own folder, `~/memories/<name>`, which `~/memory`
    links to (a sync folder is bound to one fragment); slice 3's
    `~/memory` folder is moved under the name it synced.
    Replacing a recorded memory needs `--replace`; the old one is left as
    it is. `fragment memory` shows it. This is a new CLI command
    (computers need none of it: `CLI_VERSION` stays).
- **Computers are granted as they pair, and at each sync.** Each of
  these makes the computer an editor of its owner's memory (the memory
  made first if there is none):
  - a fragment's own computer's pairing (`POST /api/computers/pair`);
  - a machine its owner approves as a computer (`POST /cli/approve`,
    whose page now says so);
  - making the memory, which makes every computer the person has now an
    editor at once;
  - each sync of a fragment's own computer
    (`cell/src/computer.rs`), which also writes the memory's name to
    `~/.fragment/agent/memory` for the task client. So a computer paired
    before this shipped gets both at its first sync, which the new task
    client's digest brings anyway.

  A failed grant does not fail a pairing (the sync tries again), and it
  fails a sync, which is tried again when the computer next wakes.
  Because every sync grants it again, an owner cannot take a computer out
  of their memory except by removing the computer. The hand-off's
  per-hand-off memory grant is gone; the chat grant stays.
- **The brain keeps a fact itself**: `platform__remember({topic, fact,
  replaces?})`, in its owner's turns only (`agent/src/memory.rs`).
  - It adds `- <fact>` as a line of `memory/<topic>.md`, or puts the fact
    in place of `replaces`.
  - It makes one commit to the memory's main through the files API, keyed
    by the tool call, and makes the memory first if there is none.
  - Its limits: the fact is one line of at most 500 characters, the topic
    is `[a-z0-9-]{1,40}`, and the file stays under 4 KiB (past that, it
    says to tidy the file first).
  - It writes facts only; skills and code stay the hands'.
  - The work guide gains a line (about 40 tokens): keep a lasting fact
    the owner tells you, or asks you to remember, with
    `platform__remember` yourself, with no computer.
  - The facts view now says so, and names the recorded memory.
  - One shortcut is in the debt ledger: it reads the file, then writes
    it whole, so a computer's commit to the same file in between is lost
    from main (git keeps it).
- **The e2e** (`builder`):
  - the owner's own `memory.<username>` (with a file) exists before the
    builder deploys;
  - the builder's pairing makes and records `memory-2.<username>`,
    members only, with the computer an editor, before any hand-off, and
    the owner's own `memory` is untouched;
  - asked again, the same one is answered, and a stranger has none;
  - `fragment memory use memory` is refused without `--replace` and
    never taken from a computer; with it, the owner's own becomes their
    memory, the computer an editor there, `memory-2` left as it was, and
    the rest of the section runs on it;
  - the agent keeps "tea with milk" itself: one commit, no build run, no
    goose request; a 600-character fact is refused; then it corrects the
    tea line in place;
  - the computer is taken out of the memory and its name file removed
    (as one paired before), and the builder deploys: its next sync makes
    it an editor again and writes the name, its next task starts with
    the facts the agent kept, and no memory sync failed;
  - a guest's turn is offered no `platform__remember` (the `agents`
    section's tool list gains it).
- **Size**: cell +206/−19 (`memory.rs` 139, the registry's record 41),
  agent +139/−84, task client and the hands' script +19/−12, e2e
  +119/−27, docs.

**4. Screenshots as blobs on pages.**
- **What changes:** `__blob/<sha>`, `fragment blob put`, `shot` in each
  step, and thumbnails in the chat's steps.
- **Acceptance (e2e):**
  - a viewer loads a shot;
  - another fragment's hash is 404;
  - an anonymous visitor to a `members` fragment is refused;
  - an unreferenced blob is collected after the grace period;
  - no row or record holds image bytes.
- **Size:** cell 60, CLI 50, template 40, e2e 100.

**Slice 4 as built** (docs/api.md, Blobs and Serving; docs/computers.md).
Paul: "Screenshots should be going in blob storage or served as urls
from the machine, not db rows." Where it differs from the plan above:
- **`__blob/<sha256>`** answers on the fragment's origin, `GET` or `HEAD`,
  to viewers and up, through the same check as its pages (so a `members`
  fragment's blobs are its members', and on a `public` one whoever holds
  only `public` is refused, as its `work` is). It serves only this
  fragment's blobs (their keys are under its npub: another's hash is
  404), `private, max-age=31536000, immutable`, `nosniff`, with ranges and
  304s. Its type is the one the upload declared, recorded beside the
  blob, when that is passive media (images but SVG, MP4, WebM, MP3, WAV,
  PDF); anything else, and every blob from before, is
  `application/octet-stream`, so an editor's upload never runs as a page
  there.
- **`fragment blob put <name> <file> [--frame]`** uploads a file typed by
  its extension and prints its sha256. Computers get it with a CLI
  release and `CLI_VERSION` raised to it; until then a pet made from this
  template shows no screen (its uploads fail), and steps carry no shot.
- **The pet's screen:** `pet.mjs` uploads each changed frame as a blob,
  then calls `frame` with `{shot, size, width, height, title, driver}`;
  the app's one row (`shown`) holds that, and `screen` answers it. The
  page shows `./__blob/<shot>`, and the waking note when a frame is gone.
  The base64 path is deleted: `jpeg` in `frame`'s schema and the row,
  the JPEG check, and `--input @file`. A pet made before keeps its own
  copy of the old code (a template is copied at creation), which the
  platform still serves; given the new files, it drops its old `screen`
  table, the JPEG with it.
- **Frames have a class of their own.** At about a frame a second, the
  week's grace would hold ~600,000 frames (86,400 a day; 60 KB each is
  ~5 GB a day, ~36 GB held per pet), and worse, the daily sweep deletes
  at most 1,000 blobs a run, so ~85,000 a day would pile up for good. So
  an upload with `?frame` marks the blob a frame, and a frame's upload,
  at most once a minute, deletes the frames last uploaded more than a
  minute before (the grace period, when shorter; never one a pointer
  names). A pet driven nonstop keeps about two minutes of frames (~120,
  ~10 MB) and makes one bulk delete a minute; its last frame stays while
  it sleeps, until the grace period. It is platform-side and needs no
  bucket rule. PUTs still cost what they cost: $0.005 per 1,000 is about
  $0.43 a day at a frame a second (a quarter of the $1.74 of awake time),
  $0.09 at one each 5 seconds.
- **Step screenshots are the tools' own**, not a capture after each step
  as Screens (above) planned: a web step through Stagehand shows no
  picture and needs none, and a capture per step is an upload per step.
  When a tool call's result carries an image
  (Cua Driver's `get_window_state`, the browser's `screenshot`: goose
  v1.52.0 passes MCP images through in `tool_call_update`), the task
  client writes it to a temporary file, uploads it to the fragment its
  steps go to (`fragment blob put`; the computer is an editor there),
  and the step carries `shot`. An upload that fails leaves the step
  without one; the step still posts. The chat's page shows it 180 px
  wide under the step's output, a link to it whole (the desktop opens it
  in its viewer). What the model is sent is unchanged. Step screenshots
  are unreferenced blobs, kept a week; at more than 1,000 a day in one
  fragment, the daily sweep's cap would fall behind them too.
- **The e2e** (`blobs`, `templates`): `blob put` prints the hash; a
  member viewer reads it as its PNG (immutable, nosniff), revalidates it
  (304); an outsider is 403 and an anonymous visitor 401 on a `members`
  fragment; an HTML upload is served as bytes; another fragment's hash
  is 404; on a fragment the daily pass leaves alone, a frame is gone at
  the next frame's upload after its grace, and the new frame and a
  non-frame blob stay. The pet: the old JPEG input is refused (400);
  `screen` answers the hash and metadata and nothing more; its computer
  (on a fake screen) uploads its frame and names it, a link holder reads
  it, and the page, in Chrome, shows it from `__blob`; Cua Driver's step
  names its PNG, readable on the pet; a hand-off's step in the chat names
  its blob, readable on the chat's origin, and the chat's page shows the
  thumbnail; no `work` record holds the image.
- **Size:** cell +135/−41 (the route, the frame class, the collection
  shared by both sweeps), the chat page +8/−4, core +19 (the served
  types and their test), task client +25/−4, CLI +46/−12, the pet
  +43/−34, e2e +159/−13, docs. Over the plan: the frame class, which
  the plan did not foresee.

**Later: the live view.** This is screen-streaming.md's first slice,
plus `__screen/now.jpg`. It removes the `frame` row (about −200 lines in
the pet).

**What only a real Sprite shows:**
- whether a reconnect reuses the loaded agent;
- how many screenshots fit before compaction at 64k;
- whether Stagehand's extension loads in the Sprite's Chrome;
- whether Jev honours `json_schema`.

## Paul's answers (2026-09-27)

1. **Model: flashx by default everywhere**, not only on computers: the
   in-cell agents, fragment agents, and the computer endpoint. Compaction
   starts at 64k tokens and gets measured.
2. **Jev: yes.** `typesafe/jev-router` is on the allowlist, with a capped
   reservation settled to its reported cost ("Jev will bring down costs
   considerably").
3. **The model route's cap: 8 MiB.** Every other route keeps 2 MiB.
4. **Stagehand: the latest** (v4.1.0 at the time of writing).
5. **Memory and skills: straight to `main`.** Git history is the review
   and the undo.
6. **The owner's computer:** a chat binds to one computer; the owner
   sets a default ("home") computer once. A person may have several
   long-lived computers ("pets" may be plural) beside throwaways, and
   the platform need not distinguish the two: a throwaway is only a
   hand-off's lifecycle.
7. **Pins: yes.** goose v1.52.0 and Cua Driver 0.28.3, the builder's
   goose included. The dependency cooldown is 2 days (Paul, 2026-09-27).
