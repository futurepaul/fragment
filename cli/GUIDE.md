# fragment — agent guide

A **fragment** is a folder of files, a SQLite database, some URLs, and an
inbox, wrapped around exactly one problem. Fragments live on a celld host;
each fragment is one durable object with its own storage. Fragments sleep when
idle and wake on requests, cron alarms, or inbox messages — they cost nothing
while asleep.

You drive fragments with the `fragment` CLI. There is no other control API to
learn. Everything is signed with your nostr key (created by `fragment login`),
so keep using the same machine/user account.

## The mental model

- A fragment has a **working copy** (its folder) and immutable **drafts**
  (snapshots of the folder). Publishing makes a draft; **blessing** a draft
  points the canonical URL at it. The live site only changes when you bless.
- URLs: canonical `/f/<name>/`, drafts `/d/<slug>/` (unguessable slugs, safe to
  share for review).
- **Workflows** (`workflows/*.mjs`) are the fragment's machinery: they run on a
  cron, on inbox messages, or when you trigger them. They read and write the
  working copy. Every run is recorded in the fragment's **event log** — the
  event log is ground truth; never trust your own memory over
  `fragment events`.
- **Rooms** give every fragment realtime multiplayer: browser clients connect
  over a websocket, share presence, messages, and one persisted JSON document
  per room.
- **Secrets** are stored by name and injected into workflows. Never write
  secret values into files.

## First moves

```
fragment login                 # once per machine; prints your npub
fragment whoami                # sanity check
fragment create my-thing       # prints the fragment's npub + tokens
```

The create output shows the **view token** and **inbox token**. You can always
get them again with `fragment status my-thing` or `fragment open my-thing`.

## The folder, locally

Any local folder can be the fragment's mirror:

```
mkdir my-thing && cd my-thing
fragment sync my-thing          # first run links the folder (creates .fragment/)
```

Sync is bidirectional and last-writer-wins. If both sides changed a file since
the last sync, the remote copy is saved as `<path>.remote-<timestamp>` next to
your local file and reported as a conflict. Nothing is ever silently merged or
lost. Sync skips dotfiles and `.fragment/`.

What the folder means to the runtime:

```
fragment.json        # the manifest (see below) — optional locally; manage via CLI
site/                # static files, served at the fragment's URLs
app.mjs              # optional: dynamic request handler (replaces static serving)
applib/              # optional: modules app.mjs can import
workflows/*.mjs      # workflows (cron / inbox / manual)
everything else      # just files: data, notes, exports — synced, versioned, served nowhere
```

## The daily loop

```
fragment sync my-thing                          # push/pull the folder
fragment publish my-thing --note "first cut"    # snapshot → draft URL /d/<slug>/
fragment bless my-thing <slug>                  # promote draft → /f/my-thing/
fragment drafts my-thing                        # list drafts ([blessed] marked)
```

Rollback is `fragment bless` on an older draft. Drafts never change and never
expire; publish as often as you think.

## The manifest

`fragment manifest my-thing` prints it; `fragment manifest-set my-thing m.json`
replaces it. Shape:

```json
{
  "name": "my-thing",
  "visibility": "token",
  "editors": ["npub1…"],
  "viewers": ["npub1…"],
  "workflows": [
    { "name": "digest",  "file": "workflows/digest.mjs", "cron": "0 8 * * *" },
    { "name": "on-hook", "file": "workflows/hook.mjs",   "trigger": "inbox" },
    { "name": "adhoc",   "file": "workflows/adhoc.mjs" }
  ],
  "secrets": ["GRAFANA_TOKEN"]
}
```

- **visibility**: `public` (anyone), `token` (anyone with the `?view=<token>`
  link — the default, good for "send a human a link"; a valid token also
  mints a scoped cookie so subresources load), `viewers` (listed npubs
  only — agents authenticate; browsers can't).
- **workflows**: auto-triggered runs are single-flight — a cron/sync fire
  while a previous run of the same workflow is active or retrying is
  skipped (`run.skipped` in the event log; manual `fragment run` always
  proceeds). Failed runs retry with backoff (default 3 attempts, 30s base —
  tune with `retry: {attempts, backoffMs}` or disable with `retry: false`);
  exhausted failures park as **held** with their input, replayable after a
  fix (`fragment runs <name> --status held` → `fragment replay <name> <id>`).
  5 held runs in 10 minutes auto-pause the workflow (loud
  `workflow.auto-paused` event); `fragment pause|unpause <name> <workflow>`
  does it by hand. Files are idempotent by construction (identical writes
  are no-ops); key external effects by cause — the once pattern below is
  exactly that, in five lines.
- **workflows**: `cron` is 5-field UTC (`*` lists ranges steps, month/day
  names OK; day-of-week 1=Sunday..7=Saturday, 0 is refused). `trigger:
  "inbox"` runs when a message lands; `trigger: "sync"` runs when files
  change on the editor plane (coalescing is `debounceMs`, default 4000;
  workflow writes never re-trigger, so outputs are loop-safe). Neither =
  manual only (`fragment run`). The fragment sleeps between
  runs; the host's durable alarms fire crons — they survive restarts and
  sleep. Cross-fragment loops are bounded: `ctx.http` stamps a hop budget,
  over-deep inbox chains are refused (`cycle.detected`), and >120
  auto-runs/hour trips auto-pause.
- **liveFiles**: `true` makes a served `app.mjs` read the live working copy
  instead of its draft snapshot. Code stays frozen in whatever draft you
  blessed; only the data it reads flows live. This is what makes a folder
  a live vault (see Recipes).
- **secrets**: declare names here, set values with `fragment secret set`.

Grants: `fragment grant my-thing --editor npub1… --viewer npub1…`
(revoke with `fragment revoke …`). Identifiers may also be NIP-05 names
(`name@domain`) — resolved via the domain's `/.well-known/nostr.json`, the
same lookup the other finite CLIs use. Editors can do everything except
transfer ownership; viewers can read files/events/manifest and view
restricted sites.

## Workflows

A workflow is a module exporting `run`:

```js
// workflows/digest.mjs
export async function run(ctx, input) {
  const rows = await ctx.files.list("notes/");
  const note = await ctx.files.read("notes/today.md");        // string
  const raw  = await ctx.files.readBytes("data/export.csv");  // ArrayBuffer
  await ctx.files.write("digests/" + Date.now() + ".md", "# …");

  const data = await ctx.http("https://example.com/api", {
    headers: { authorization: "Bearer " + ctx.secrets.SOME_TOKEN },
  }).then(r => r.json());

  const text = await ctx.ai("summarize in one line: " + note); // host-routed LLM
  const n = (await ctx.state.get("runs")) || 0;
  await ctx.state.put("runs", n + 1);

  const pending = await ctx.inbox();        // unprocessed inbox messages
  await ctx.events.append("digest", { rows: rows.length });
  await ctx.log("done");                    // lands in the event log
  return { ok: true };                      // returned to the caller
}
```

- `ctx.files` — the fragment's folder (read/write/list).
- `ctx.secrets` — plain object of secret values by name.
- `ctx.http` — fetch.
- `ctx.ai(prompt, {model?})` — inference routed through the host (the host
  holds the platform key; you never see it).
- `ctx.state` — per-workflow persistent key-value store.
- `ctx.inbox()` — pending inbox messages (inbox-triggered runs auto-ack theirs).
- `ctx.events.append` / `ctx.log` — write to the event log.
- `ctx.rooms.getState/setState(room)` — read/write a room's persisted document.

Trigger one: `fragment run my-thing digest --input '{"x":1}'`. The result and
the run's events come back. Check `fragment events my-thing` after cron runs.

Workflows run in an isolated loader sandbox, one isolate per run, with their
own copy of the folder — a wedged workflow cannot wedge the fragment itself.
Split helpers into `lib/` and import them from a workflow
(`import { x } from "../lib/util.mjs"`) — relative and map-path imports
both work.

## The authoring contract — three habits, that's all

The platform carries the failure leg for you: retries with backoff, held
runs with replay, auto-pause, loop protection, write-suppression. You get
all of it without learning anything. What's left for you:

1. **Throw, don't catch-and-continue.** If something unexpected happens,
   `throw` and let the run fail. The host retries what's transient, parks
   what isn't (`held`), and pauses the workflow if it keeps failing.
   Recovery code you don't write is recovery code you can't get wrong.
2. **Any trigger can fire twice; key effects by cause.** Writing files is
   already safe — identical content is a no-op. For external side effects,
   mark them done in `ctx.state` keyed by what caused them (see the once
   pattern below).
3. **React to state, write state.** Read the current state of what you
   watch, write your output as a function of it. Fragments that mirror
   state converge; fragments that emit events in response to events
   oscillate.

## Patterns — copy these

Each is a complete workflow file (the e2e suite executes these exact
blocks, so they cannot rot). Edit the ALL-CAPS constants and go.

### pattern: poller

Watch something on a schedule and process only what's new. The seen-set
lives in `ctx.state`; it advances only when the whole pass succeeds, so a
crashed pass re-sees (and skips, via once-style markers) its items. Tree
responses mark a fragment's own organs with `machinery: true` — skip those.

```js
// workflows/watch.mjs — cron "*/2 * * * *"
const SOURCE = "https://a-vault.fragment.club/api/tree"; // returns {files:[{path,size,machinery?}]}
export async function run(ctx) {
  const tree = await (await ctx.http(SOURCE)).json();
  const seen = new Set((await ctx.state.get("seen")) || []);
  for (const f of tree.files || []) {
    if (f.machinery || seen.has(f.path)) continue;
    seen.add(f.path);
    await ctx.files.write("feed/" + f.path.replace(/\//g, "__") + ".md",
      "# " + f.path + "\n\n" + f.size + " bytes\n");
  }
  await ctx.state.put("seen", [...seen].slice(-5000));
  return { fresh: [...seen].length };
}
```

### pattern: once

Do an external side effect exactly once per cause. The marker survives
crashes, restarts, and replays — a redelivered trigger skips the effect.

```js
// workflows/notify.mjs — trigger "inbox"
const WEBHOOK = "https://example.com/hook";
export async function run(ctx, input) {
  const id = input && input.inbox && input.inbox.id;
  if (!id || (await ctx.state.get("sent:" + id))) return { skipped: true };
  await ctx.http(WEBHOOK, { method: "POST", body: JSON.stringify(input.inbox.payload ?? null) });
  await ctx.state.put("sent:" + id, Date.now());   // after the effect, not before
  return { sent: true };
}
```

### pattern: sync-reaction

Rebuild derived data when files change. Runs are level-triggered (input is
"the files changed", not a diff) and writes are suppressed when unchanged,
so running it twice is free.

```js
// workflows/reindex.mjs — trigger "sync"
export async function run(ctx) {
  const paths = await ctx.files.list("notes/");
  const index = paths.sort().map((p) => "- [" + p + "](" + p + ")").join("\n");
  await ctx.files.write("INDEX.md", "# Notes\n\n" + index + "\n");
  return { indexed: paths.length };
}
```

### pattern: inbox-log

Receive webhooks, validate, append. Bounded shape, no growth surprises.

```js
// workflows/log.mjs — trigger "inbox"
export async function run(ctx) {
  for (const m of await ctx.inbox()) {
    const line = JSON.stringify({ at: m.at, source: String(m.source).slice(0, 40), payload: m.payload }) + "\n";
    if (line.length > 2000) continue;                    // refuse oversized
    const p = "log/" + new Date(m.at).toISOString().slice(0, 10) + ".jsonl";
    const prev = await ctx.files.read(p).catch(() => ""); // read throws if absent
    await ctx.files.write(p, prev + line);
  }
  return { drained: true };
}
```

### pattern: ai-pass

Read, summarize, write — and let failures fail. A transient model error
retries on its own; a terminal one parks as held for you to replay after a
fix. Needs the host to have an inference key.

```js
// workflows/digest.mjs — cron "0 8 * * *"
export async function run(ctx) {
  const notes = await ctx.files.list("notes/");
  const bodies = [];
  for (const p of notes.slice(0, 20)) bodies.push("## " + p + "\n" + (await ctx.files.read(p)).slice(0, 2000));
  const summary = await ctx.ai("One paragraph on today's notes:\n\n" + bodies.join("\n\n"));
  await ctx.files.write("digests/" + new Date().toISOString().slice(0, 10) + ".md", summary + "\n");
  return { notes: notes.length };
}
```

## Inbox (webhooks in)

```
POST {host}/api/f/{name}/inbox?t={inboxToken}   {"source": "grafana", "payload": {...}}
```

When you control the HTTP client, prefer sending the token as the
`x-fragment-inbox-token` header instead of `?t=` — same result, but the
secret stays out of access logs.

No signature needed — the token is the auth (rotate by asking the owner to
re-create… no, tokens are fixed at create; treat them as passwords).
`fragment inbox my-thing --token <t> --payload '{"hello":"world"}'` tests it.
Inbox messages run all `trigger: "inbox"` workflows and land in the event log.

## Sites and apps

Static: files under `site/` serve at the draft/canonical URLs. `/` serves
`site/index.html`.

Dynamic: if the folder has `app.mjs`, every request to the fragment goes to it:

```js
// app.mjs
export default {
  async fetch(req, ctx) {
    // same ctx as workflows (files are read-only here; normally the draft
    // snapshot — set "liveFiles": true in the manifest to read the live
    // working copy instead)
    return new Response("hello " + new URL(req.url).pathname);
  },
};
```

## Multiplayer (rooms)

Every fragment has realtime rooms: named websocket channels with presence, a
recent-message log, and one persisted JSON document per room.

Browser side — the fragment serves its own client at `__rt.js`:

```html
<script src="/f/<name>/__rt.js"></script>
<script>
  const room = fragment.room("notes");

  // The FIRST event you get is always "hello" — the full bootstrap:
  //   hello.state     the room's persisted document (or null)
  //   hello.presence  who is connected
  //   hello.history   the last ~50 messages: [{from, data, at}]
  room.on("hello", (h) => { render(h.state); backfill(h.history); });

  // AFTER hello, changes arrive as separate events:
  room.on("state", (s) => render(s));            // someone set the document
  room.on("msg", (m) => append(m.from, m.data)); // someone sent a message
  room.on("presence", (p) => online(p));         // joins/leaves

  room.send({ text: "hi" });       // -> others get {type:"msg", from, data, at}
  room.setState({ doc: "shared" }); // persisted server-side, broadcast as "state"
  room.setPresence({ label: "paul" });
</script>
```

The traps, plainly: **the initial state comes in `hello`, not in a `state`
event** — if you only listen for `state` your UI sits empty until somebody
changes something. And `msg.data` is whatever the sender passed to
`room.send(...)` — the envelope is `{from, data, at}`.

Which is authoritative? **`state`** — it's your app's single persisted
document, you set it, you read it. `history` is just the recent-message log
for backfilling a chat-style UI; don't reconstruct app state from it.

Server side (optional) — `rooms.mjs` in the folder, from the *served draft*:

```js
export async function onMessage(room, msg, ctx) {
  // msg = { from: <clientId>, data: <what the browser sent>, at: <ms> }
  // Your payload is in msg.data — NOT directly on msg.
  const text = (msg.data?.text || "").trim().slice(0, 500);
  if (!text) return { drop: true, reason: "empty message" };
  return { broadcast: { text, name: msg.data.name } };  // rewrite before broadcast
  // other options: { state: {...} } to set the room document,
  //                  { drop: true } to swallow the message
}
```

Debugging: if `rooms.mjs` throws or returns `{error}`, the event log shows
`room-error`; a drop shows `room-drop` (with `reason` if given). Read
`fragment events <name>` when realtime misbehaves.

## Recipes — big scaffolds

Two scaffolds ship in the CLI (`fragment new --list`): they are ordinary
fragments — code you can read, edit, and re-bless — not special modes.

**Vault** — turn any folder of text files into a live, URL-bearing,
Obsidian-like site:

```
fragment new my-vault --template vault
cd my-vault
fragment create my-vault
fragment manifest-set my-vault fragment.json
fragment publish my-vault --dir . --bless
fragment sync my-vault --dir . --watch 2     # leave running; edits go live
```

The viewer (`app.mjs` + `assets/`) is frozen in the blessed draft; the notes
flow through the working copy (`liveFiles: true`), so a synced edit appears
on reload without republishing. `[[wikilinks]]` resolve by filename; code
files render with syntax highlighting; `_index.md`/`README.md` are folder
landings.

**Dropzone** — drop a file in a folder, get live workflow output on the
webview (and back in your folder):

```
fragment new my-drop --template dropzone
cd my-drop            # same create/manifest-set/publish --bless as above
fragment sync my-drop --dir . --watch 2
echo "hi" > drop/note.txt        # → output/note-*.md within seconds
```

Arrivals under `drop/` fire the `ingest` workflow (`trigger: "sync"`); it
summarizes with `ctx.ai` when the host has an inference key, else writes a
plain digest. Outputs land in `output/`, visible on the webview and pulled
back into your folder by the next sync.

## Rules of the road

1. **The event log is truth.** Before and after claiming anything about a
   fragment, read `fragment events`. If you think something ran and the log
   disagrees, the log is right.
2. **Publish freely, bless deliberately.** Drafts are cheap and unguessable.
   Bless only what you checked.
3. **Secrets by name only.** `fragment secret set` reads from the environment
   or stdin — never put values in files, manifests, or notes.
4. **One fragment, one problem.** If a folder grows a second job, make a
   second fragment. They're free when asleep.
5. **Small files, plain formats.** Markdown, JSON, CSV. Anything an agent can
   diff later without you.

## Command reference

```
fragment login [--force]            fragment secret set <name> <KEY>
fragment whoami                     fragment secret list <name>
fragment host [<url>]               fragment secret rm <name> <KEY>
fragment new <dir> [--template T]   fragment grant <name> --editor/--viewer <npub|name@dom>
fragment create <name>              fragment revoke <name> --editor/--viewer <npub|name@dom>
fragment list                       fragment inbox <name> --token T --payload JSON
fragment status <name>              fragment run <name> <wf> [--input JSON]
fragment events <name> [--since N]  fragment runs <name> [--status S] [--limit N]
fragment manifest <name>            fragment pause <name> <wf>
fragment manifest-set <name> FILE   fragment unpause <name> <wf>
fragment sync <name> [--dir D] [--watch N]   fragment replay <name> <run-id>
                                   [--install | --uninstall]
fragment publish <name> [--dir D] [--note N] [--bless]
fragment drafts <name>              fragment bless <name> <slug>
```

`fragment sync --install` writes a LaunchAgent (macOS) or systemd user
unit (Linux) so the folder stays live without a terminal — the pattern the
vault recipe uses. Everything an author needs to know is in this guide;
if it isn't here, it isn't a rule.

Global flags: `--host <url>` (or `FRAGMENT_HOST`), `--json`. Set a sticky
default with `fragment host <url>` (e.g. `fragment host https://fragment.club`).
