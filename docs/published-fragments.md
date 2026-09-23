# Published fragments: the expressiveness floor

fragment.club's testbed is being retired without migrating its fragments
(decided 2026-09-23). What must survive is what they could *do*: every
primitive below stays expressible in fragment-next, and each one keeps an
end-to-end check that drives it the way the fragment did. When a primitive
changes shape, this table changes in the same commit.

Inventory taken 2026-09-23 from `fragment list` (owner npub
`npub1w352…qf9h2`) with read-only pulls of each repo.

| Fragment | What it is | Primitives it depends on |
|---|---|---|
| `linecount` (public) | "Delete-A-Thon": a live dashboard of lines deleted per merged PR | inbox webhook from a GitHub Action (idempotent by commit sha), files as ground truth (`data/history.json`), room state as the live ping, web push to subscribers |
| `meatproxy` (link) | Prompts-and-pastes room: a pastebin that is a chatroom; every item is a body/meta file pair so the synced folder is the agent conduit | dynamic `app.mjs`, inbox-triggered handler with retries, cron sweep (paused), files read/write/list/stat, event ledger appends, per-workflow state, secrets (`OPENROUTER_API_KEY`), `fragment:ai` summaries, web push |
| `strategy-vault` (link) | A 110-file markdown vault with a live viewer | dynamic `app.mjs` serving a bundled viewer, `trigger: "files"` workflow that bumps a room so viewers refresh, files index/readBytes |
| `events-rfc` (link) | A design doc open for annotation (highlights, pins, section notes) | dynamic `app.mjs`, inbox with a per-fragment secret token header (`x-fragment-inbox-token`), rooms get/set state, event ledger, files as the annotation store |
| `sycamore` (link) | A fictional product page to review with the same annotation kit | same as `events-rfc` |

## Primitive coverage

"Proven by" names the check that exercises the primitive today: an
`scripts/e2e.mjs` section (312/312 green once the workflow-instance fix
landed) or a `runtime/test` file. Rows marked **gap** have no check; each
gets one before its primitive is touched, and the Rust harness that
replaces `scripts/e2e.mjs` must cover every row.

| Primitive | Used by | Proven by |
|---|---|---|
| Rooms: browser `fragment.room` (`__rt.js`), messages, persisted document | linecount, strategy-vault, events-rfc, sycamore | e2e `rooms`, `runtime`. Rust: channels + live queries + `__fragment.js` (e2e `channels`, `live`, `browser`) |
| Room state from workflows: `ctx.rooms.getState/setState` on a named room | linecount, strategy-vault, events-rfc, sycamore | `runtime/test/rooms-state.test.mjs` |
| Room presence | (rooms clients) | Rust: e2e `live` (join, leave, size limit) and `browser` (two tabs) |
| Web push: browser `fragment.push`/`fragment.notify`, server `ctx.push`, VAPID keys | linecount, meatproxy | **gap** (no test anywhere) |
| Inbox webhooks: token (query or `x-fragment-inbox-token`), `ctx.inbox`/`ctx.inboxAck` | linecount, meatproxy, events-rfc, sycamore | e2e `workflows`, `runs`, `paused`. Rust: the `inbox` channel and its trigger (e2e `triggers`: both token forms, text bodies, the `{source, payload}` shape, the CLI's `fragment inbox`, `templates/inbox`) |
| Inbox pending cap (1000, then 429 + `queue.rejected`) | all inbox fragments | Rust: e2e `triggers` (the 1001st pending delivery is 429, `inbox.rejected`) |
| Workflow triggers: `cron`, `inbox`, `files`/`sync`, manual `run` | all | e2e `cron`, `workflows`, `filesync`, `runs`. Rust: triggers name an operation (e2e `triggers`: cron, inbox, channel, files); a manual run is a call to a job (e2e `jobs`) |
| Runs ledger: retry classes, held runs, replay, auto-pause, hop budget | meatproxy | e2e `runs`, `paused`; `runtime/test/runs.test.mjs`, `flagship-replay.test.mjs`. Rust: runs as Workflows (e2e `jobs`: retries, held, replay; `triggers`: auto-pause, the rate ceiling, the hop budget; `restart`: a job sleeping through a SIGKILL) |
| Dynamic `app.mjs` + `applib/` from the live pin | meatproxy, strategy-vault, events-rfc, sycamore | e2e `app`, `static-root`, `build`. Rust: e2e `site` (static `site/` from live), `routes` (`App.fetch`, `applib/` imports), and `notes` (a viewer's `/api/` routes on the fragment's own host) |
| Files: `ctx.files` read/write/list/stat/index/readBytes/ingest, CAS, write suppression | all | e2e `files`, `workflows`; `runtime/test/git-plane.test.mjs`. Rust: e2e `appfiles` (reads at main, mutation writes as one commit, job steps with compare-and-swap, a self-feeding file trigger stopped by the hop budget, per-fragment isolation), `blobs` (files of 1 MiB or more), `notes` (a vault read through `App.fetch`, live in a browser) |
| Event ledger: `ctx.events.append`, `fragment events` | meatproxy, events-rfc, sycamore | e2e `runs`, `workflows`, `platform`. Rust: the `events` channel (e2e `channels`, `members`); app-side appends are channel publishes |
| Per-workflow state: `ctx.state` | meatproxy | e2e `app`, `workflows`. Rust: the app's SQL, read and written through a job's call steps (e2e `jobs`) |
| Secrets: declared by name, wrapped at rest, injected into runs | meatproxy, events-rfc, sycamore | e2e `auth`, `lockdown`, `workflows`; `runtime/test/secretwrap.test.mjs`. Rust: e2e `secrets` (set, list by name, never returned, limits), `crates/core` sealing tests, and e2e `jobs` (`{{NAME}}` in a fetch header reaches the upstream and appears in no run, record, or event) |
| Platform AI: `fragment:ai` text, image, video | meatproxy | e2e `gen`, `cron` (moves from fal to OpenRouter) |
| Visibility: `public`, `link` (view token → per-fragment cookie), `viewers` (now `members`) | all | e2e `platform`, `lockdown`, `auth`. Rust: e2e `site`, `public`, `members`, `lockdown` |
| Deploy, preview, rollback, drafts (git `main`/`live`) | all | e2e `deploy`. Rust: e2e `deploy` (the CLI against the cell), `ops` (code installs from live) |
| Folder sync: conflict copies, mass-delete guard, journals bound to repo identity | all (authoring) | e2e `filesync`, `cli-lane`, `converge`; CLI unit tests. Rust: e2e `sync` (conflict, modes, verify, mirror, guard, 5 MiB chunks, continuous with the change feed); the CLI unit tests run on `crates/fakes` |

Note: inbox `Idempotency-Key` handling was deliberately removed in
`5a53bb5` (content-hash naming makes redeliveries harmless); linecount's
comment claiming it is stale, and linecount dedupes by commit sha itself.
