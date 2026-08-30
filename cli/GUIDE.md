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
  (snapshots of the folder). `fragment deploy` snapshots the folder and
  points the canonical URL at that snapshot in one step; `--preview`
  snapshots without going live, and `fragment rollback` repoints at an
  older one.
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
fragment init my-thing         # scaffold + create + deploy → live URL,
                               #   share link, webhook URL (the one-command start)
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
lost. Sync skips dotfiles and `.fragment/`; in a git repo it also honors
`.gitignore` (ignored files and `.git/` never upload).

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
fragment sync my-thing                     # push/pull the folder
fragment deploy my-thing --dir .           # snapshot + GO LIVE → /f/my-thing/
fragment deploy my-thing --preview         # snapshot only → preview URL /d/<slug>/
fragment drafts my-thing                   # list snapshots (the live one marked [blessed])
```

Rollback is `fragment rollback my-thing` (the previous snapshot; `--to <slug>`
picks one). Snapshots never change and never expire; deploy as often as you
think.

## Sync in depth

One contract: **files up to 64 KiB ride inline; anything larger is pushed blob-first** — the cell's rows stay tiny documents while the bytes live in a separate content-addressed blob store (see "Blob-first pushes" below). Cells hold documents, not media dumps; a cell's content lives in SQLite and replicates as WAL frames, so big bodies tax replication, restores, and write acks if they sit in the cell itself.

```
fragment sync my-thing --dir .              # one mirror pass (default)
fragment sync my-thing --dir . --watch      # continuous: OS events + live channel + 60s sweeps
fragment sync my-thing --dir . --mode pull  # read-only copy (never deletes; --prune to apply)
fragment sync my-thing --dir . --mode push  # local→remote only
fragment verify my-thing --dir .            # full-hash audit (caches lie; this doesn't)
```

- **Fast and crash-safe**: unchanged files are detected by size+mtime
  (state in `.fragment/state.json`); writes are atomic; a second watcher on
  the same folder is refused by a lock. If the folder moves, sync stops and
  tells you (`--rebuild-state` after moving on purpose).
- **Live**: with `--watch`, the cell pushes a `changed` frame over a
  websocket the moment files mutate remotely; remote edits land in seconds.
  Sweeps every 60s are the correctness floor — live is a latency win, never
  the mechanism.
- **Conflicts merge**: when both sides changed a text or JSON file, sync
  fetches the common ancestor from the fragment's server-side history and
  three-way merges. Non-overlapping edits merge silently (reported as
  `merged`); overlapping ones write `<<<<<<<` markers locally and exit 3.
  `--conflict-strategy copy` saves the remote version as
  `<path>.conflict-<time>-<writer>` instead. The last 10 revisions of every
  file are kept server-side (`/file/history`, `/file/at`).
- **Mass-deletion guard**: deleting >max(10, 30%) of known files in one
  pass is refused (exit 4) until `--apply-mass-delete` — the folder-looks-
  unmounted protection.
- **Append-only folders**: manifest `"appendOnly": ["logs/", "drop/"]`
  makes those prefixes add-only for everyone but you — writers append,
  identical rewrites are no-ops, modifications and deletes are refused.
  Many-writer folders (logs, dropzones) become race-free by construction.
- **Exit codes** (for scripting): 0 clean/merged, 1 hard failure, 3
  conflicts present, 4 mass-deletion guard tripped.

## Blob-first pushes

The 64 KiB inline carve-out is unchanged: small files sync exactly as
before. Beyond that, sync goes blob-first — the bytes upload straight to
the blob store (a Blossom-style server signed with a kind-24242 auth
event derived from your own key) and the cell commits only a pointer row
(`{sha256, size, mime}`). You must tell the CLI where that store is:

```
export FRAGMENT_BLOB_URL=https://blobs.example.com      # env wins
# or persist it in the fragment config (~/.config/fragment/config.json):
#   { "host": "...", "secret_key": "...", "blob_url": "https://blobs.example.com" }
```

- Without a configured store, a changed file over 64 KiB **fails the sync
  with a clear error naming `FRAGMENT_BLOB_URL`** — it never silently
  falls back to a raw body the cell would refuse.
- Uploads are content-addressed and idempotent: each changed file is
  hashed, probed with a HEAD (already-present bytes skip the PUT), and
  the server's echoed hash is verified against the local one before any
  row commit. Row commits themselves stay ordinary sync commits, so
  revisions, conflicts, watchers, and notify all behave as usual.
- Pulls are the mirror image: fetched bytes stream to a temp file and
  rename in atomically, and `.fragment/cache/<sha>` short-circuits
  re-downloading content you already have (soft cap 256 MB, evicted
  oldest-accessed-first; cache misses just re-fetch).
- Files larger than the tier's 64 MiB cap are warn-skipped (the folder's
  last-good state is kept) — a bucket or CDN remains the right home for
  those.

## The manifest

`fragment manifest my-thing` prints it; `fragment manifest-set my-thing m.json`
replaces it. Shape:

```json
{
  "name": "my-thing",
  "visibility": "link",
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
- **workflows**: auto-triggered runs are single-flight — a cron/files fire
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
  "inbox"` runs when a message lands; `trigger: "files"` runs when files
  change on the editor plane (coalescing is `debounceMs`, default 4000;
  workflow writes never re-trigger, so outputs are loop-safe — files-trigger
  input carries the changed paths at `input.sync.paths`). Neither =
  manual only (`fragment run`). The fragment sleeps between
  runs; the host's durable alarms fire crons — they survive restarts and
  sleep. Cross-fragment loops are bounded: `ctx.http` stamps a hop budget,
  over-deep inbox chains are refused (`cycle.detected`), and >120
  auto-runs/hour trips auto-pause.
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
const API = "https://example.com/api"; // any JSON endpoint you can call
export async function run(ctx, input) {
  const rows = await ctx.files.list("notes/");
  const note = await ctx.files.read("notes/today.md");        // string
  const raw  = await ctx.files.readBytes("data/export.csv");  // ArrayBuffer
  await ctx.files.write("digests/" + Date.now() + ".md", "# …");

  const data = await ctx.http(API, {
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
- `ctx.http` — fetch, with a 30s default timeout (pass your own `signal` to control it).
- `ctx.ai(prompt, {model?})` — inference routed through the host (the host
  holds the platform key; you never see it).
- `ctx.image(prompt, opts?)` / `ctx.video(prompt, opts?)` — generate media
  through the host's fal.ai key and get back
  `{path, sha256, size, mime, url}`: the file is ALREADY a row in the
  fragment's working copy (under `gen/` by default), so it syncs to your
  folder like any other file and serves at `__file?path=…`. Defaults are
  cheap and fixed (a ~1MP FLUX.2 image, a 5s 768p MiniMax H3 Max clip);
  bounded opts (`duration`, `resolution`, `aspect_ratio`, `image_size`,
  `num_images`, `output_format`, `seed`, …) override them. For progress UIs,
  the pieces are `ctx.gen.start` → `ctx.gen.status` → (sleep in YOUR code) —
  a waiting cell would stall the fragment, so the platform never sleeps.
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
// workflows/reindex.mjs — trigger "files"
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
  const msgs = await ctx.inbox();
  for (const m of msgs) {
    const line = JSON.stringify({ at: m.at, source: String(m.source).slice(0, 40), payload: m.payload }) + "\n";
    if (line.length > 2000) continue;                    // refuse oversized
    const p = "log/" + new Date(m.at).toISOString().slice(0, 10) + ".jsonl";
    const prev = await ctx.files.read(p).catch(() => ""); // read throws if absent
    await ctx.files.write(p, prev + line);
  }
  await ctx.inboxAck(msgs.map((m) => m.id)); // only what we observed
  return { drained: msgs.length };
}
```

### pattern: dropzone

Collect drops (notes, uploads, pings) into an append-only inbox folder.
The manifest does the enforcing (`"appendOnly": ["inbox/"]`); content-hash
naming makes identical re-drops no-ops and simultaneous drops
collision-free. Nothing is ever lost or double-filed.

**The standard drop envelope** (producers send this, consumers accept
this — filter on the payload's shape, NEVER on the source string, or
you will silently discard other fragments' drops):

    POST {webhook URL}   {"source": "<your-name>", "payload": {"text": "...", "name": "optional sender"}}

```js
// workflows/ingest.mjs — trigger "inbox"; manifest: "appendOnly": ["inbox/"]
export async function run(ctx) {
  const msgs = await ctx.inbox();
  const filed = [];
  for (const m of msgs) {
    const text = String((m.payload && m.payload.text) || "").trim().slice(0, 5000);
    if (!text) continue;
    const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
    const hash = [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("").slice(0, 8);
    const p = "inbox/" + new Date(m.at).toISOString().slice(0, 10) + "-" + hash + ".md";
    await ctx.files.write(p, text + "\n");
    filed.push(p);
  }
  await ctx.inboxAck(msgs.map((m) => m.id)); // only what we observed
  return { filed };
}
```

Receiving drops (the other side): file any message that has a
`payload.text`, whatever its source — that is the contract.

```js
// workflows/ingest.mjs — trigger "inbox"; the RECEIVER of drops
export async function run(ctx) {
  const msgs = await ctx.inbox();
  const filed = [];
  for (const m of msgs) {
    const p = (m.payload && typeof m.payload === "object") ? m.payload : {};
    const text = String(p.text || "").trim().slice(0, 5000);
    if (!text) continue;                       // shape filter, not source filter
    const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
    const hash = [...new Uint8Array(d)].map((b) => b.toString(16).padStart(2, "0")).join("").slice(0, 8);
    const from = String(p.name || m.source || "anon").replace(/[^a-z0-9-]+/gi, "-").slice(0, 24);
    await ctx.files.write(`inbox/${from}-${hash}.md`, text + "\n");
    filed.push(m.id);
  }
  await ctx.inboxAck(msgs.map((m) => m.id));
  return { filed: filed.length };
}
```

### pattern: watcher

React to another fragment's changes the moment they happen. Put your
fragment's inbox URL in the WATCHED fragment's manifest (`notifyUrls`) —
it gets POSTed a `{type:"changed", fragment, rev, paths}` frame on every
change, which triggers this workflow instantly. Keep a cron on the same
workflow as the floor (notify is best-effort). The machine-readable tree
(`__tree`) and raw reads (`__file?path=`) are gated exactly like the
site — a `?view=` link is all a reader needs.

```js
// workflows/check.mjs — trigger "inbox" AND cron "*/2 * * * *"
const SOURCE = "https://some-fragment.fragment.club";
const DELAY_MS = 60_000; // enrichment waits, so links appear instantly
// set the token first or every run will fail:
//   fragment secret set <this-fragment> SOURCE_VIEW_TOKEN=<token>
export async function run(ctx) {
  const token = ctx.secrets.SOURCE_VIEW_TOKEN;
  const tree = await (await ctx.http(SOURCE + "/__tree?view=" + token)).json();
  const state = (await ctx.state.get("watch")) || { seen: [], pending: [] };
  const seen = new Set(state.seen);
  // phase 1 — file new items NOW: the link must never wait for anything
  for (const f of tree.files || []) {
    if (f.machinery || seen.has(f.path)) continue;
    seen.add(f.path);
    state.pending.push({ path: f.path, at: Date.now() });
  }
  // phase 2 — enrich items filed >DELAY_MS ago: summaries/mentions appear
  // on their own, a run or two after the link
  const still = [];
  for (const it of state.pending) {
    if (Date.now() - it.at < DELAY_MS) { still.push(it); continue; }
    const body = await (await ctx.http(SOURCE + "/__file?path=" + encodeURIComponent(it.path) + "&view=" + token)).text();
    it.summary = await ctx.ai("one dry sentence about this file: " + body.slice(0, 4000));
    await ctx.files.write("feed/" + it.path.replace(/\//g, "__") + ".json", JSON.stringify(it));
  }
  state.pending = still;
  await ctx.state.put("watch", { seen: [...seen].slice(-5000), pending: still });
  const msgs = await ctx.inbox();
  await ctx.inboxAck(msgs.map((m) => m.id));
  return { fresh: seen.size };
}
```

### pattern: gen

One line per medium; the result is a folder file, not a return value you
have to store. Needs the host to have a fal key (`FAL_API_KEY`).

```js
// workflows/illustrate.mjs — inbox or cron triggered
export async function run(ctx) {
  const shot = await ctx.image("a lighthouse at dawn, heavy fog, 35mm film still");
  await ctx.events.append("illustrated", { path: shot.path });
  const clip = await ctx.video("waves hitting the lighthouse rocks", { duration: 5 });
  ctx.log("made " + shot.path + " and " + clip.path);
  return { image: shot.path, video: clip.path };
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
POST {host}/api/f/{name}/inbox?t={inboxToken}   {"source": "grafana", "payload": {"alert": "disk 90%"}}
```

`{host}` is always the MAIN host — subdomains serve the site only, and
`/api` there answers 403.

When you control the HTTP client, prefer sending the token as the
`x-fragment-inbox-token` header instead of `?t=` — same result, but the
secret stays out of access logs.

No signature needed — the token is the auth. Tokens are created with the
fragment; if one leaks, `fragment rotate <name> [--inbox] [--view]`
(owner-only; default rotates both view + inbox tokens) mints new ones and
prints the new webhook URL and share link — give senders the new URL.
`fragment inbox my-thing --token <t> --payload '{"hello":"world"}'` tests it.
Inbox messages run all `trigger: "inbox"` workflows and land in the event log.

## Sites and apps

Static: files under `site/` serve at the draft/canonical URLs. `/` serves
`site/index.html` — even when the folder also has an app.

Dynamic: if the folder has `app.mjs`, every request that isn't a real
`site/` file (or a reserved platform path like `__tree`) goes to it. A
fragment with both is the normal shape — the page is a file, the app is
the API:

```js
// app.mjs
export default {
  async fetch(req, ctx) {
    // same ctx as workflows (files are read-only here; normally the draft
    // snapshot — set "liveFiles": true in the manifest to read the live
    // working copy instead). x-fragment-url is the public URL the visitor
    // used; url.pathname is the internal route and stays stable.
    const pub = req.headers.get("x-fragment-url") || req.url;
    return new Response("hello " + new URL(pub).pathname);
  },
};
```

### Authoring in TypeScript

Author fragments like a normal web app: `app.ts`, `site/*.ts`,
`workflows/*.ts`, with real imports and a `fragment.d.ts` declaring the
platform surface (`Ctx`, `ctx.files`, `ctx.inbox`… — every doc comment in
it is a real contract, including the one about acking your drains). Run
`fragment build` before deploying (or let `init`/`deploy` do it): it
strips types, fixes import specifiers to the compiled siblings,
content-hashes site assets and rewrites their references, and parse-gates
everything that would be served — a syntax error fails the build instead
of shipping. The compiled `.mjs` files land beside where the sources were
and are what syncs; `fragment.json` names workflow files by their compiled
path (`workflows/w.mjs`).

```
my-fragment/
  fragment.json       # workflows listed by compiled path
  fragment.d.ts       # platform types (in the basic template)
  app.ts              # compiles to app.mjs
  site/index.html     # references /main.js — build rewrites to the hash
  site/main.ts        # imports ./dep.ts normally
  workflows/w.ts      # compiles to workflows/w.mjs
```

Token-gated fragments and server-rendered links: the `?view=` token is
a SECRET — when your app renders links into its own pages, bake the token
in at render time (`ctx.secrets.MY_VIEW_TOKEN`); a client-side variable
that never reaches the server renders as `view=undefined` and friends get
403s. Browser-side `location.search` tokens only exist in the browser.

## Notifications

Two platform surfaces, both loaded from `/f/<name>/__rt.js` (plus the
service worker at `/f/<name>/__sw.js`):

- **Open tab** — `fragment.notify.ask()` (call it ONLY from a click
  handler), then `fragment.notify.show(title, {body, url})` fires when
  the tab is hidden; clicks focus the window and navigate to `url`.
- **Closed tab (Web Push)** — `fragment.push.register(who)` from the same
  click: registers the worker, subscribes with the fragment's VAPID key,
  and stores the subscription. Workforms send with
  `await ctx.push(who, {title, body, url})`. Subscriptions live in the
  fragment (`push_subs`), failures self-heal (410 drops, 5 strikes drop).

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

The traps, plainly: **messages echo to their sender too** (every client
in the room, including yours, receives the broadcast — dedupe by client id
or your own message id). **The initial state comes in `hello`, not in a `state`
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

From a terminal: `fragment rooms <name>` lists the fragment's rooms
(connected-client count, last activity); `fragment rooms <name> <room> --tail 20`
prints that room's most recent messages ascending — the fastest way to see
what actually flowed through a room.

## Recipes — big scaffolds

Two scaffolds ship in the CLI (`fragment new --list`): they are ordinary
fragments — code you can read, edit, and redeploy — not special modes.

**Vault** — turn any folder of text files into a live, URL-bearing,
Obsidian-like site:

```
fragment init my-vault --template vault
cd my-vault
# my-notes is the folder to show — read-only, overlaid in each pass
fragment sync my-vault --dir . --mirror-from ../my-notes --watch   # leave running
```

The viewer (`app.mjs` + `assets/`) is frozen in the deploy snapshot; the notes
flow through the working copy (`liveFiles: true`), so a synced edit appears
on reload without redeploying. `[[wikilinks]]` resolve by filename; code
files render with syntax highlighting; `_index.md`/`README.md` are folder
landings.

**Dropzone** — drop a file in a folder, get live workflow output on the
webview (and back in your folder):

```
fragment init my-drop --template dropzone
cd my-drop
fragment sync my-drop --dir . --watch
echo "hi" > drop/note.txt        # → output/note-*.md within seconds
```

Arrivals under `drop/` fire the `ingest` workflow (`trigger: "files"`); it
summarizes with `ctx.ai` when the host has an inference key, else writes a
plain digest. Outputs land in `output/`, visible on the webview and pulled
back into your folder by the next sync.

## Rules of the road

1. **The event log is truth.** Before and after claiming anything about a
   fragment, read `fragment events`. If you think something ran and the log
   disagrees, the log is right.
2. **Deploy freely, preview deliberately.** Snapshots are cheap and unguessable;
   `--preview` when you want to look before going live.
3. **Secrets by name only.** `fragment secret set <name> <KEY> <value>`
   (value may also come from the env var or stdin) — never put values in
   files, manifests, or notes.
4. **One fragment, one problem.** If a folder grows a second job, make a
   second fragment. They're free when asleep.
5. **Small files, plain formats.** Markdown, JSON, CSV. Anything an agent can
   diff later without you.

## Command reference

```
fragment login [--force]            fragment secret set <name> <KEY>
fragment whoami                     fragment secret list <name>
fragment host [<url>]               fragment secret rm <name> <KEY>
fragment init <name> [--template T] fragment grant <name> --editor/--viewer <npub|name@dom>
fragment new <dir> [--template T]
fragment create <name>              fragment revoke <name> --editor/--viewer <npub|name@dom>
fragment list                       fragment inbox <name> --token T --payload JSON
fragment status <name>              fragment run <name> <wf> [--input JSON]
fragment events <name> [--since N]  fragment runs <name> [--status S] [--limit N]
fragment manifest <name>            fragment pause <name> <wf>
fragment manifest-set <name> FILE   fragment unpause <name> <wf>
fragment sync <name> [--dir D] [--watch] [--mode M]  fragment replay <name> <run-id>
                                   [--install | --uninstall]
fragment deploy <name> [--dir D] [--preview]
fragment drafts <name>              fragment rollback <name> [--to <slug>]
fragment rotate <name> [--inbox] [--view]
fragment rooms <name> [<room>] [--tail N]
```

`fragment sync --install` writes a LaunchAgent (macOS) or systemd user
unit (Linux) so the folder stays live without a terminal — the pattern the
vault recipe uses. Everything an author needs to know is in this guide;
if it isn't here, it isn't a rule.

Global flags: `--host <url>` (or `FRAGMENT_HOST`), `--json`, `-v`/`--verbose`.
Every structured command accepts `--json` (equivalently, set
`FRAGMENT_OUTPUT=json`): stdout is exactly ONE line — on success
`{"ok":true,"data":…}`, on failure
`{"ok":false,"error":{"code","message","hint"}}` where `message` is the
human-readable text and `error.code` carries a stable machine code
(`invalid_usage auth_failed forbidden not_found name_taken conflict
too_large rate_limited unavailable server_error`). Exit codes for scripts:
0 ok, 1 failure, 2 usage. `-v` logs every signed request to stderr
(`GET /api/f/x/status -> 200 (12ms [retries=0])`) and leaves stdout clean,
so `-v --json` works together. Set a sticky default host with
`fragment host <url>` (e.g. `fragment host https://fragment.club`).

## Migration notes

Vocabulary renamed along the way; kept here so older scripts and
transcripts still decode:

- `fragment publish` → `fragment deploy`; `fragment bless <name> <slug>` →
  `fragment rollback <name> --to <slug>`. The drafts listing still marks the
  served snapshot `[blessed]`, and the wire endpoints keep their names
  (`POST /drafts`, `POST /bless`) — only the CLI verbs changed.
- Files-triggered workflows fire on `trigger: "files"` (was `"sync"`).
- Every structured command accepts `--json`; errors carry stable codes under
  `error.code` (see Global flags above).
