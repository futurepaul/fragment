# fragment — agent guide

A **fragment** is one small place on the web: a folder of files in git,
an app of named operations over its own SQLite, channels that pages
follow live, members with roles, and URLs. Fragments live on a celld
host and sleep when idle; a request, a trigger, or an inbox delivery
wakes them.

You drive fragments with the `fragment` CLI. Every request is signed with
your nostr key (made by `fragment login`), so keep using the same machine
and user account.

## The model in one screen

- **Files.** One git repo per fragment (on code.storage). `main` is the
  working copy; `live` is what the site serves. `fragment sync` moves a
  folder to and from `main`; `fragment deploy` moves `live` to `main`'s
  tip; `fragment rollback` moves it back. Files of 1 MiB or more are
  stored as blobs, with a small pointer in git; the CLI handles both
  directions.
- **Operations.** `fragment.json` names them; `app.mjs` implements them.
  A **query** reads; a **mutation** changes the app's SQLite, all or
  nothing, once per id; a **job** runs durable steps (fetch, AI, sleep,
  calls) outside any request.
- **Channels.** Append-only feeds of records. A mutation publishes to
  one; pages and the CLI follow them live. `events`, `ops`, and `inbox`
  are built in.
- **Triggers.** A cron, a new record on a channel (the inbox is one),
  or a change to matching files starts a run of an operation.
- **Members.** Owner, editor, viewer; visibility (`public`, `link`,
  `members`) decides what everyone else gets.
- **The event log is ground truth.** `fragment events <name>` says what
  happened; believe it over your memory.

## First moves

```
fragment login                            # once per machine; prints your npub
fragment init my-thing                    # scaffold (todo) + create + deploy → live URL,
                                          #   share link, webhook URL
fragment init my-inbox --template inbox   # or: todo | inbox | notes
```

`fragment new <dir> --template T` scaffolds without creating;
`fragment new --list` lists the templates:

- `todo`: mutations over SQLite, a public activity channel, a live page.
- `inbox`: webhook deliveries start a job that fetches and records.
- `notes`: a folder of markdown as a live site; the files are the state.

`fragment status my-thing` shows the URLs, the view token (the share
link's `?view=`), and the inbox token. `fragment open my-thing` prints
the links again.

## The folder

```
fragment.json      # operations, channels, triggers, meta
app.mjs            # class App: one method per operation
applib/            # modules app.mjs imports (at most 64 modules, 4 MiB)
site/              # static files, served from live
everything else    # files: notes, data, media; synced and versioned
```

Code runs from `live`: a deploy changes what runs. Files the app reads
come from `main`: a synced note is visible without a deploy.

## The daily loop

```
fragment sync my-thing --dir .            # push and pull the folder
fragment deploy my-thing --dir .          # sync, then move live → the site and the app
fragment deploy my-thing --preview        # an ephemeral ref at main (no URL); deploy to promote
fragment drafts my-thing                  # deploy history (the live ref's commits)
fragment rollback my-thing [--to <sha>]   # live back to an earlier deploy
```

A deploy whose `fragment.json` does not check keeps the last good code:
`fragment status` says why under `code.error`.

## Sync in depth

The repo is the truth; your folder is a working copy. Sync talks to
code.storage directly with a short-lived, repo-scoped token the host
mints for each pass (editors and up). No local `.git` is made.

```
fragment sync my-thing --dir .              # one mirror pass (default)
fragment sync my-thing --dir . --watch      # continuous: OS events + the change feed + 60 s sweeps
fragment sync my-thing --dir . --mode pull  # read-only copy (never deletes; --prune to apply)
fragment sync my-thing --dir . --mode push  # local → repo only
fragment sync my-thing --dir . --install    # keep syncing after logout (LaunchAgent / systemd unit)
fragment verify my-thing --dir .            # full-content audit
```

- **One commit per pass**, against the branch head it read. If `main`
  moved underneath, sync rereads and retries (at most 3 times), then
  fails loudly. An unchanged folder commits nothing.
- **Large files**: 1 MiB and up upload to the blob store first, then
  their pointer is committed; a pull downloads the bytes, so the folder
  always holds real files.
- **Conflicts**: when both sides changed a file, yours stays and the
  remote copy lands beside it as `<path>.conflict-<time>-<writer>`
  (exit 3). Both stay in git history.
- **Mass-deletion guard**: a pass that would delete more than
  max(10, 30%) of known files, or all of them, is refused (exit 4) until
  `--apply-mass-delete`.
- **Deletions converge**; a locally modified copy wins over a remote
  delete. Dotfiles, `.fragment/`, and (in a git repo) `.gitignore`d
  files never upload.
- **Exit codes**: 0 clean, 1 failure, 3 conflicts, 4 guard tripped.

## The app

```json
{
  "operations": {
    "add":    { "kind": "mutation", "role": "editor",
                "input": { "type": "object", "required": ["text"], "additionalProperties": false,
                           "properties": { "text": { "type": "string", "maxLength": 200 } } } },
    "list":   { "kind": "query", "role": "public" },
    "ingest": { "kind": "job" }
  },
  "channels": { "activity": { "read": "viewer" } },
  "triggers": [{ "channel": "inbox", "run": "ingest" }],
  "meta": { "title": "My thing", "description": "Shown in link previews." }
}
```

- `role` is who may call it: `public`, `viewer` (default for queries),
  `editor` (default for mutations and jobs), `owner`.
- `input` is a JSON Schema (types, enums, lengths, ranges, `properties`,
  `required`, `additionalProperties`, `items`). A call that does not fit
  is refused before your code runs, naming the field.

```js
// app.mjs
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, text TEXT NOT NULL)");
  }

  // a mutation: synchronous; a throw rolls back its writes and its records
  add({ text }, call) {
    const { id } = this.ctx.storage.sql.exec("INSERT INTO items (text) VALUES (?) RETURNING id", text).one();
    call.publish("activity", { id, text, by: call.principal });   // appended once it commits
    return { id };
  }

  // a query
  list() {
    return { items: this.ctx.storage.sql.exec("SELECT id, text FROM items ORDER BY id").toArray() };
  }

  // a job: each await on job.* is a durable step
  async ingest({ record }, job) {
    const page = await job.fetch(record.body.payload.url);
    return await job.call("add", { text: `${page.status} ${record.body.payload.url}` });
  }
}
```

- **Mutations** are keyed by (caller, id) for seven days: a retry with
  the same id gets the stored result; the same id with another input is
  409. In a mutation, `call.publish(channel, body)`, `call.files.write(
  path, content)` / `remove(path)`, and `call.push(who, payload)` all
  happen once, after it commits.
- **Jobs** re-run from the top at every step with the results so far, so
  reach steps in the same order every time and change nothing except
  through steps. The steps: `job.call(op, input)`, `job.fetch(url,
  init)` (the app's only way out; `{{NAME}}` in a header value is the
  secret `NAME`, filled in outside your code), `job.publish(channel,
  body)`, `job.sleep("2 hours")`, `job.files.read|list|stat|write|remove`
  (`write(path, content, {expect: sha})` compares and swaps),
  `job.push(who, payload)`, `job.ai.text|image|video(...)`. A step that
  may pass later (429, 5xx, timeout) is retried with backoff; one that
  cannot throws a `StepError` you may catch. A job that throws is
  **held** until someone replays it.
- **Triggers** (at most 32) run a mutation or a job:
  `{"channel": "inbox", "run": op}` gets `{channel, record}` (an inbox
  record's `body` is `{source, payload}`); `{"cron": "0 9 * * 1", "run":
  op}` (UTC, five fields) gets `{cron, at}`; `{"files": "notes/**",
  "run": op}` gets `{ref, commit, paths, more}` when `main` moves. A
  triggered run acts as the fragment itself, with an editor's role.
- **Files** from anything async: `this.files.read(path)` / `readBytes` /
  `list(prefix)` / `stat`, at `main`. Blobs are served, not read into
  the app.
- **`fetch(request)`**, if the App has one, answers every site path that
  is not a file in `site/` (any method); `x-fragment-principal` and
  `x-fragment-role` say who.
- **AI** (jobs): `job.ai.text({model, prompt})`, `job.ai.image({prompt,
  path})`, `job.ai.video({prompt, path})` call OpenRouter with the
  fragment's `OPENROUTER_API_KEY` secret; images and video are written
  to `path` on `main`.

`fragment build [dir]` compiles `.ts` sources to `.mjs` beside them,
hashes site assets, and refuses files that would not parse. It is
optional; plain `.mjs` needs nothing.

## Pages

A page imports the browser library from its own fragment:

```html
<script type="module">
  import * as fragment from "./__fragment.js";
  await fragment.call("add", { text: "hello" });                // retries keep the id
  fragment.live("list", {}, (r) => render(r.items));             // re-runs after every change
  fragment.subscribe("activity", (rec) => console.log(rec.body)); // channel records, live
  fragment.presence.set({ cursor: 3 }); fragment.presence.on((list) => draw(list));
</script>
```

Visitors without a key call as an anonymous principal (a cookie), so
`public` operations work on a public fragment with no login.
`await fragment.push.register(who)` (from a click) subscribes the
browser to web push; `call.push(who, payload)` or `job.push(...)` reaches
every browser registered with that `who` (`*` for all).

The share link's `?view=` token is a secret. If your app renders links
into its own pages, don't leak it into places the page doesn't need.

## The inbox (webhooks in)

Every fragment has an inbox URL: `POST <host>/api/f/<name>/inbox?t=<inbox
token>` (or the token in `x-fragment-inbox-token`), a JSON body of at
most 64 KiB. Each delivery is a record on the `inbox` channel; a trigger
`{"channel": "inbox", "run": "<op>"}` handles it.

```
fragment inbox my-thing --token <inbox token> --payload '{"url":"https://example.com"}'
fragment rotate my-thing --inbox        # a new inbox token (the old one stops working)
```

## Runs: when something goes wrong

Every job call and every triggered operation is a run: `queued`,
`running`, `succeeded`, `held` (it threw; its input is kept), or
`blocked` (paused, or a trigger chain deeper than 16 hops).

```
fragment runs my-thing [--status held]     # newest first
fragment runs my-thing <run>                # one run: input, output, error
fragment replay my-thing <run>              # after fixing the code: the same input again
fragment triggers my-thing                  # what starts runs, and what is paused
fragment pause my-thing <op>                # stop its triggers (calls still work)
fragment unpause my-thing <op>
fragment events my-thing --tail 30          # the log
```

An operation pauses its own triggers after 5 held runs in 10 minutes or
120 triggered runs in an hour; the event log says so (`op.auto-paused`).

## Calling and following

```
fragment call my-thing add --input '{"text":"hi"}' [--id ID]   # a retry with the same --id is a replay
fragment channel my-thing                                      # list channels
fragment channel my-thing activity --follow                     # stream records as JSON lines
```

## People

```
fragment visibility my-thing [public|link|members]
fragment members list my-thing
fragment members add my-thing <npub | name@domain> --role editor
fragment members rm my-thing <npub>
fragment members leave my-thing
fragment invite create my-thing --role viewer --uses 5    # prints the token once
fragment join my-thing <token>
fragment rotate my-thing --view                            # a new share link
```

Only the owner manages members, invites, visibility, and tokens.
`fragment.json` grants nothing.

## Secrets

```
fragment secret set my-thing API_KEY         # value from argv, else $API_KEY, else stdin
fragment secret list my-thing                # names only; values never come back
fragment secret rm my-thing API_KEY
```

Your code never holds a secret: `job.fetch` fills `{{API_KEY}}` in a
header at the way out, and AI steps use `OPENROUTER_API_KEY`. Never write
secret values into files.

## Rules of the road

1. **The event log is truth.** Read `fragment events` before and after
   claiming what a fragment did.
2. **Ids make retries safe.** Reuse an operation's id when you retry it;
   use a new one for a new action.
3. **Deploy freely.** Deploys are commits; rollback is one command.
4. **One fragment, one problem.** A second job is a second fragment.
5. **Plain files.** Markdown, JSON, CSV: things the next agent can diff.

## Command reference

```
fragment login [--force]                 fragment call <name> <op> [--input JSON] [--id ID]
fragment whoami                          fragment channel <name> [<channel>] [--after N] [--follow]
fragment host [<url>]                    fragment runs <name> [<run>] [--status S] [--limit N]
fragment init <name> [--template T]      fragment replay <name> <run>
fragment new <dir> [--template T]        fragment triggers <name>
fragment new --list                      fragment pause|unpause <name> <op>
fragment create <name> [--visibility V]  fragment inbox <name> --token T --payload JSON
fragment list                            fragment rotate <name> [--inbox] [--view]
fragment status <name>                   fragment visibility <name> [V]
fragment open <name>                     fragment members list|add|rm|leave ...
fragment events <name> [--since N | --tail N]
fragment manifest <name>                 fragment invite create|list|revoke ...
fragment manifest-set <name> FILE        fragment join <name> <token>
fragment sync <name> [--dir D] [--watch] [--mode M] [--install | --uninstall]
fragment verify <name> [--dir D]         fragment secret set|list|rm ...
fragment deploy <name> [--dir D] [--preview] [--note N]
fragment drafts <name>                   fragment rollback <name> [--to <sha>]
fragment build [DIR]                     fragment rm <name>
fragment guide
```

Global flags: `--host <url>` (or `FRAGMENT_HOST`, or `fragment host
<url>` to save one), `--json`, `-v`. With `--json` (or
`FRAGMENT_OUTPUT=json`), stdout is exactly one line: `{"ok":true,
"data":…}` or `{"ok":false,"error":{"code","message","hint"}}`, with
stable codes (`invalid_usage auth_failed forbidden not_found name_taken
conflict too_large rate_limited unavailable server_error`). Exit codes:
0 ok, 1 failure, 2 usage. `-v` logs each signed request to stderr and
leaves stdout clean.

The code.storage server is named by the host; to point the CLI at
another, set `FRAGMENT_CODESTORAGE_URL` or `"codestorage"` in
`~/.config/fragment/config.json`.
