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
- **workflows**: `cron` is 5-field UTC (`*` lists ranges steps, month/day
  names OK; day-of-week 1=Sunday..7=Saturday, 0 is refused). `trigger:
  "inbox"` runs when a message lands; `trigger: "sync"` runs when files
  change on the editor plane (sync pushes, CLI writes — coalesced a few
  seconds; workflow writes never re-trigger, so outputs are loop-safe).
  Neither = manual only (`fragment run`). The fragment sleeps between
  runs; the host's durable alarms fire crons — they survive restarts and
  sleep.
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

## Inbox (webhooks in)

```
POST {host}/api/f/{name}/inbox?t={inboxToken}   {"source": "grafana", "payload": {...}}
```

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

## Recipes

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
fragment events <name> [--since N]  fragment open <name>
fragment manifest <name>            fragment guide
fragment manifest-set <name> FILE
fragment sync <name> [--dir D] [--watch N]
fragment publish <name> [--dir D] [--note N] [--bless]
fragment drafts <name>              fragment bless <name> <slug>
```

Global flags: `--host <url>` (or `FRAGMENT_HOST`), `--json`. Set a sticky
default with `fragment host <url>` (e.g. `fragment host https://fragment.club`).
