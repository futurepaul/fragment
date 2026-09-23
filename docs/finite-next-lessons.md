# finite-next: what to port, and what it taught us

finite-next (`/Users/futurepaul/dev/finite/finite-next-worktrees/
claude-cell-agent`, branch `claude/cell-agent`, commit `6545f08`) was the
spike that proved an agent can live in celld cells, build apps on Sprite
workspaces over ACP, and drive a pet computer. fragment-next rebuilds it
on fragment. This file is the porting map and the gotchas that cost real
time; read it before touching the matching phase.

## Porting map

| finite-next path | What it is | Where it goes |
|---|---|---|
| `src/agent.js` | AgentCell: libfx turn loop, durable turns accepted before work, per-conversation ordering with retry backoff (5 s × 4^attempt, 4 attempts), lease + watchdog runner, operation log with `effectKey` dedupe, notes injected into the next turn, workspace progress cards | phase 5 (the agent cell; the lease/ledger is the fallback if Workflows replay fails) |
| `src/inference.js` | libfx speaks the Vercel AI Gateway protocol; this adapts it to an OpenAI-compatible endpoint and lifts tool-result images into a user message (`liftToolImages`, `KEEP_IMAGE_STEPS = 2`) | phase 5, pointed at OpenRouter (`z-ai/glm-5.3-flash`) |
| `src/tools.js` | tool definitions and the agent's instructions (apps, code, computer, builds, memory) | phase 5, re-expressed as operations of the fragments the agent belongs to |
| `src/workspace.js`, `gateway/workspaces.mjs`, `workspace/kit/*` | Sprite builder workspaces: versioned kit, bootstrap, ACP runner (`acp-job.mjs`), event log read by byte offset, cancel file → `session/cancel`, tree reset, one job per app, watchdog alarm | phase 7 (the runner becomes Rust; the kit installs the `fragment` CLI instead of `finite-app-dev`) |
| `upstream/desktop/*` | Finite-styled chat and desktop UI (tokens from finitecomputer-v2 `ocean-shell.css`; transcript with tool rollups, streaming, live build cards, a DOM-only markdown renderer) | phase 6 (the desktop template) |
| `src/index.js`, `src/shell.js`, `src/auth.js` | platform shell: dev login, session cookie (SameSite=Strict, same-origin checks), approvals and recovery in a trusted bar, the desktop in a sandboxed opaque-origin iframe with a capability token in its path | phases 4 and 6 |
| `computer/*` | the Substrate pet computer (Chromium, Xvfb, Openbox, `control.py` with screenshot/open/click/type/key/shell and op-idempotency records) | phase 7: the same pieces as Sprite services; Substrate itself is dropped |
| `scripts/e2e.mjs` | product checks, including crash-mid-tool, workspace create/continue/stop, share-then-multiplayer | phase 2+ as cases in the Rust harness |

## celld

- `celld dev` rebuilds on any file change under the project, including
  files a test writes. Use `--watch-ignore` (finite-next symlinked its
  state directory out of the tree instead), and never edit project files
  while an e2e run is in flight.
- An alarm handler that outlives `CELLD_OPERATION_DEADLINE_MS` (15 s) is
  fired again. Never do long work inside `alarm()`: hand it to
  `waitUntil` (or a Workflow) and return.
- A reload can kill work started from an alarm with no alarm left
  pending, and the job then sits forever. Arm a watchdog alarm *before*
  starting the work, and delete the alarm when nothing remains.
- During a reload, outbound calls can fail with "refusing to send: the
  write this request follows is not durable (NodeFenced)"; retries must
  be idempotent.
- A dev node self-fenced after about five hours when its local store
  stopped opening ("unable to open database file", exit 3). Production
  needs a restart-always supervisor anyway (celld docs, guarantees).
- `atob` needs padded base64; `Referrer-Policy: no-referrer` makes
  `Origin: null` on same-origin form posts.
- celld 0.5 removed `CELLD_WORKER_LOADER` and the `CELLD_VAR_*`
  passthrough: loaders are `worker_loaders` in the config; variables are
  `vars` in the config or `.dev.vars` (dotenv, one line per value) under
  `celld dev`. `celld deploy` never reads `.dev.vars`, so production
  secrets are `vars` rendered into the deployed config
  (`spikes/celld-0.5.1/README.md`).
- A process started with `nohup … &` from a tool shell can die when that
  shell exits; long-lived dev servers run as background tasks or through
  `scripts/dev`.
- `celld dev` runs the node as a child process (`celld
  --no-control-plane …`). Killing `celld dev` with SIGKILL orphans the
  node, which keeps the port; a crash test kills the child
  (`spikes/driver/src/celld.rs`).
- celld module `rules` accept only `**/*.ext` globs.
- workers-rs Durable Objects do not `extend DurableObject`, so celld
  refuses RPC to them; a JavaScript class that extends it and forwards to
  the Rust object fixes that (`spikes/cells-rs/entry.mjs`).
- A root storage transaction cannot enclose a facet call past ~1.6 MB of
  facet database, and a capability call inside one deadlocks
  (`spikes/apps/README.md`).
- libfx's host `fetch` must speak AI SDK LanguageModelV3 stream parts
  (`finishReason: {unified, raw}`, nested usage); a V2-shaped `finish`
  fails with `InvalidProviderFinishReason`.
- `~/.npmrc` pins Finite's package cooldown as `before=2026-05-22`; a
  package under a cooldown exception (libfx, celld, code.storage, Vercel,
  Sprites) installs with a one-off `--before=<today>`.

## libfx (0.0.10, WASM in the cell)

- `import fxWasm from ".../libfx/fx-core.wasm"`, then `createFxAgent({
  wasm, apiKey: "host-managed", fetch, tools, instructions, checkpoint })`.
- Host tools do not receive the tool-call id; derive operation ids from
  the turn, the attempt, and a sequence number.
- Checkpoints happen only at idle; `allow_native_tools` is false in WASM.
- The model catalog entry must carry both `vision` and `file-input` tags
  or image inputs are refused.

## fx on Sprites, over ACP

- fx 0.0.10 knows only the gateway, codex, and grok providers, so a
  loopback shim on the Sprite (`127.0.0.1:18777`, `FX_GATEWAY_CHAT_URL`,
  `AI_GATEWAY_API_KEY=host-managed`) adapts it. fx reads `~/.fx/AGENTS.md`.
  `FX_PERMISSION_MODE=full-access` on a credential-free workspace.
- ACP: `initialize` (protocol 1); `session/list {cwd}`;
  `session/resume` (no history replay) vs `session/load` (replays);
  `session/prompt` returns `{stopReason, usage}`; the `session/cancel`
  notification stops a running shell command within about a second and
  returns `stopReason: "cancelled"`.
- Updates: `agent_message_chunk`, `agent_thought_chunk`, `tool_call`
  (`name`, a generic `title`, `kind`, and `rawInput` with the `command`
  or `path`), `tool_call_update` (`status`, streamed `content`, and
  `command_result` with `exit_code`). Streamed output arrives as many
  `in_progress` updates: report status changes only.
- The model often restates its answer after a final tool call; the
  report is the text after the last tool call.
- Run the ACP client on the Sprite, writing an event log to disk, so a
  job outlives any connection to the platform.

## Sprites

- `@fly/sprites` needs Node 24+. `exec()` splits its command on spaces;
  use `execFile("bash", ["-lc", script])`. `pgrep -f` matches its own
  shell; use pid files.
- A Sprite pauses warm about 30 s after activity stops (wake 100–500 ms,
  processes kept) and later cold (wake 1–2 s, processes gone; Services
  restart). The Tasks API (`/.sprite/api.sock`, one hour per task,
  renewable) holds it awake during a job.
- Our Sprites run in Fly `dfw`; their disks are JuiceFS on the host, and
  the backing bucket is not exposed. The org is capped at 10 running and
  10 warm.
- Ubuntu 26.04, user `sprite` with passwordless sudo, Node 24, many
  coding agents preinstalled. A first bootstrap (fx, npm, Playwright
  Chromium) took about 45 s; a kit refresh about 10 s.

## Agents building apps

- Without the app contract in hand, the agent invents an API; hand it
  the contract (now: the operation schemas) and a render-first template.
- Report UI errors from served pages back to the agent.
- Builders must re-run their checks after their last edit.
- An invalid manifest published once broke the app list; validate at the
  boundary and keep listing resilient to one broken app.
- The agent makes small tweaks itself instead of starting a workspace
  build; keep both paths.
- Undo must be grouped by turn, and its result must say what it reverted,
  or the model undoes twice.

## Prices checked 2026-09-23

- Sprites: $0.07/CPU-hour, $0.04375/GB-hour, hot storage $0.000683/GB-hour,
  cold $0.000027/GB-hour; a mostly idle 1.5 GB computer ≈ $0.076 per
  awake hour.
- Fly Machines (Dallas): shared-cpu-1x 1 GB ≈ $5.13/month; shared-cpu-4x
  4 GB ≈ $23/month. Tigris: $0.02/GB-month, $0.005 per 1,000 writes,
  $0.0005 per 1,000 reads, no egress.
- OpenRouter: `z-ai/glm-5.3-flash` $0.15 in / $0.50 out per million
  tokens (text, image, and video input); images
  `google/gemini-3.1-flash-lite-image`; video `minimax/hailuo-3-max`.
  Media models are missing from the default `/models` list; add
  `?output_modalities=image` or `video`.
- Rejected: GKE (~$300/month fixed for ~15–25 awake computers; half of
  Sprites only past ~250 light users) and exe.dev ($20–25 per user per
  month, an always-on 2 vCPU / 8 GB pool per user).

## Resources (paths only; never print the contents)

- code.storage key (org `finite`):
  `~/.config/finite-next/secrets/codestorage-private-key.pem`
- OpenRouter key: `~/.config/finite-next/secrets/openrouter-api-key`.
- Fly Sprites token (org `paul-miller`): `~/Downloads/sprite-token.txt`.
- Fly API token, org-scoped (org `personal`, name `fragment-next`, expires
  2026-12-22): `~/.config/finite-next/secrets/fly-api-token`. flyctl on
  this Mac is also signed in to Paul's account.
- The six test Sprites were deleted 2026-09-23. `ws-d519eb416e04407c85ee825b`
  (created 2026-09-23 12:32 UTC) is from Paul's own session on the
  finite-next stack; it stays until Paul says otherwise.
- GCP project `finite-next-test` was deleted 2026-09-23 (recoverable for
  30 days); its budget lives on the billing account.
- The fragment.club VPS still serves the old fragments until the cutover.
