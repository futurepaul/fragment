# The agent computer: the brain in the cell, the hands on a computer

Status: **decided 2026-09-27** (Paul; ROADMAP decision 24, which amends
decision 10). Nothing is built yet; the slices below build it. Sources
were checked on 2026-09-27, and the dependency cooldown is 2 days.

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
| Memory, skills | `MEMORY.md`, `USER.md`, agentskills.io `SKILL.md` ([memory](https://hermes-agent.nousresearch.com/docs/user-guide/features/memory), [skills](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills)) | files in the fragment's git |
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
- The job answers `{message, code}` to the brain's alarm, as today.

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
- memory and skills, as git files (`agent/memory/*.md`,
  `agent/skills/<name>/SKILL.md`).

`fragment sync --live` brings memory and skills to `~/fragment`. They
are linked into goose's `~/.agents/skills`
([skills](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/guides/context-engineering/using-skills.md))
and `~/.config/goose/memory`
([memory](https://github.com/aaif-goose/goose/blob/v1.52.0/documentation/docs/mcp/memory-mcp.md)).
The computer commits what goose writes back to `main`.

**Grants and secrets** live on the platform, never on the computer's
disk.

**A replacement computer** seeds each chat's new session from that
chat's transcript, plus the memory and skills files.

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
| `typesafe/jev-router` (2026-09-25) | variable (−1) | a router; no parameters listed |

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

**Browser reasoning** is Jev: Stagehand's callback sends
`typesafe/jev-router` with a `json_schema` response format. Jev's
structured output is unverified. The Stagehand slice checks it, and
falls back to flash if it fails.

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
So `computer/browser-mcp.mjs` is ours: about 150 lines on
`@modelcontextprotocol/sdk` 1.30.1, offering `open`, `act`, `observe`,
`extract`, and `screenshot`. goose is the agent and Stagehand its hands,
v4's own split.

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
- Pages cannot yet read a blob by hash: that route answers on the
  platform's host, to signed callers. The blob slice adds
  `__blob/<sha>` on the fragment's origin, for viewers and up, serving
  only this fragment's blobs, cached as immutable.
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
