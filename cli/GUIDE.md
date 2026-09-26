# fragment — agent guide

A **fragment** is one small place on the web: a folder of files in git,
an app of named operations over its own SQLite, channels that pages
follow live, members with roles, and URLs. Fragments live on a celld
host, fragment.club (invite-only for now), and sleep when idle; a
request, a trigger, or an inbox delivery wakes them.

## Install and pair

You drive fragments with the `fragment` CLI. On macOS or Linux, with no
sudo (the same command updates it):

```
mkdir -p ~/.local/bin && curl -fsSL https://github.com/futurepaul/fragment/releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin
```

If `fragment` is then not found, put `~/.local/bin` on your PATH:
`export PATH="$HOME/.local/bin:$PATH"`, in `~/.zshrc` or `~/.bashrc`.
`fragment skill` prints a SKILL.md for a coding agent (Claude Code,
Codex) that points it here.

Every request is signed with this machine's nostr key. `fragment login`
makes it and opens a page on the host: sign in and approve the key,
whose ending the page and the terminal both show. On a machine without a
browser, it prints the link to open anywhere you are signed in
(`--no-wait` returns at once; run it again after approving). The host is
https://fragment.club unless `--host`, `FRAGMENT_HOST`, or `fragment
host <url>` names another. The host knows which identity (`id:…`) each
key belongs to: memberships name you, not the key. `fragment keys
rotate` replaces the key and keeps everything you have. A machine that
works for someone without being them pairs as their computer instead:
`fragment login --computer <name>`, approved by its owner. It acts only
in the fragments it is added to and those it makes (its owner's); its
owner lists and removes it with `fragment computers [rm <name>]`.

A person chooses a username once (`fragment username <name>`, or the
host's page after the first sign-in) and can create nothing before. A
fragment's name is `<label>.<username>`, served at
`<label>--<username>.<suffix>`; in a command, a bare label names one of
yours (`fragment status todo` is `todo.<your username>`).

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
fragment login                            # once per machine: sign in in a browser, approve this machine's key
fragment init my-thing                    # scaffold (todo) + create + deploy → live URL,
                                          #   share link, webhook URL
fragment init my-inbox --template inbox   # or: todo | notes | chat | desktop | blank
```

`fragment new <dir> --template T` scaffolds without creating;
`fragment new --list` lists the templates:

- `todo`: mutations over SQLite, a public activity channel, a live page.
- `inbox`: webhook deliveries start a job that fetches and records.
- `notes`: a folder of markdown as a live site; the files are the state.
- `chat`: a live chat room; an agent member can answer in it.
- `desktop`: a demo: your fragments side by side (chats, apps, files).
- `blank`: one page, to build on.

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
mints (editors and up): one per command; a watcher keeps one for at most
a minute, and mints again after that, before it expires, or when
code.storage refuses it (the host checks the role when it mints, so an
editor removed from the fragment stops syncing within a minute). No
local `.git` is made.

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
  fails loudly. An unchanged folder commits nothing. A commit whose
  answer is lost is never sent again blind: sync rereads the branch, and
  if the commit landed, what it wrote matches the folder and is adopted.
- **Large files**: 1 MiB and up upload to the blob store first, then
  their pointer is committed; a pull downloads the bytes, so the folder
  always holds real files.
- **Conflicts**: when both sides changed a file to different bytes,
  yours stays and the remote copy lands beside it as
  `<path>.conflict-<time>-<writer>` (exit 3). Both stay in git history.
  Both sides changed to the same bytes is no conflict.
- **Mass-deletion guard**: a pass that would delete more than
  max(3, 30%) of known files, or all of them, is refused (exit 4) until
  `--apply-mass-delete`.
- **Deletions converge**; a locally modified copy wins over a remote
  delete.
- **What syncs**: everything but dot files and folders (`.fragment/`,
  `.git/`, `.obsidian/`, `.DS_Store`), the top-level `node_modules/` (one
  deeper down, as under `site/`, is served and syncs), editor droppings
  (`~` backups, `~$` locks, `.swp`), and sync's own `.conflict-` copies.
  One rule for both directions: such a file in the repo is never pulled,
  and never deleted for being absent from your folder. In a git repo,
  `.gitignore`d files never upload.
- **Watching** (`--watch`): a save is one pass, and so is a burst of
  saves; the change feed's echo of your own commit, and the folder's
  echo of a pull, cost nothing. After 60 s of quiet a sweep compares the
  folder with its journal and main with the last head it saw (one
  request), and passes only when either moved or the change feed is
  down.
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
- `"computer": {}` gives the fragment a Linux machine of its own (a Fly
  Sprite) with this CLI installed, paired as your computer and an editor
  of the fragment. It is awake while a page of the fragment is open (and
  5 minutes after), billed to your budget; `fragment computers rm
  <fragment>` destroys it. On it, `fragment model "<prompt>"` (or
  `--request` with an OpenAI-style chat request on stdin) asks the model
  through the platform, on your budget, with no key of its own.

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
  `job.push(who, payload)`, `job.ai.text|image|video(...)`,
  `job.agent({prompt})`, `job.computer.exec(command)` (below). A step that
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
- **AI** (jobs): `job.ai.text({model, prompt, max_tokens, reasoning})`,
  `job.ai.image({prompt, path})`, `job.ai.video({prompt, path})` call
  OpenRouter with the fragment's `OPENROUTER_API_KEY` secret; images and
  video are written to `path` on `main`. A reasoning model can spend a
  small `max_tokens` thinking: pass `reasoning: {effort: "low"}`.

## An agent in your fragment

A fragment can bring its own agent: signed-in people talk to it through
a channel, and it calls your operations for them. Declare it, with its
instructions in a file of the fragment (`fragment new --template
calories` is a working example):

```json
{
  "operations": { "log_food": { "kind": "mutation", "role": "viewer", "input": { … } },
                  "today":    { "kind": "query",    "role": "viewer" } },
  "channels":   { "ask":  { "read": "viewer", "post": "viewer" },
                  "work": { "read": "viewer", "post": "editor" } },
  "agent": { "instructions": "agent.md", "tools": ["log_food", "today"], "channel": "ask",
             "model": "z-ai/glm-5.3-flash" }
}
```

- Deploying makes it: an agent named as the fragment is, yours, an
  editor of this fragment and of nothing else, listening to `channel`
  (declared, with a `post` role). Redeploying updates it; a deploy
  without the block removes it. A fragment with no block carries nothing
  of one.
- `tools` are operations of this fragment (never an owner-only one): the
  model is offered those, as the person asking may call them, and no
  other fragment's, no files, no deploy. `model` is optional.
- A signed-in person's post to the channel (`fragment.post("ask",
  {text})`) starts a turn; an anonymous one starts nothing. Someone
  signed in who holds the link counts as a viewer for it, as they do on
  the page. Each person has a conversation of their own with it, and each call acts for them,
  with the lower of their role and the agent's: `call.principal` is the
  person (`call.agent` the agent), so what it logs is theirs.
- Its answer lands on the channel as `{text, turn}`, and its steps on
  `work` (a start naming who asked, each tool call, an end), the chat
  template's records: render them as you like.
- You pay for its model calls, from your budget.

A job asks it too, for the run's principal (a triggered run's: the
fragment, as an editor), and waits for the answer:

```js
async summarize(input, job) {
  const { text, turn } = await job.agent({ prompt: "Summarize what I ate today.", channel: "ask" });
  return { text };
}
```

`conversation` (a key you choose) continues one across runs; without it
each run has its own. `channel` posts the turn's steps and answer there
too. A replayed run reattaches to the turn it started; a turn that fails
or is stopped throws a `StepError`.

`"agent": {"personal": true, "channel": "chat"}` (the chat template's)
has your own agent answer there instead, with its own tools.

## A computer in your fragment

With `"computer": {}` in `fragment.json`, a job runs shell commands on
the fragment's own Linux machine:

```js
async build(input, job) {
  const r = await job.computer.exec("npm ci && npm test", { timeout: "20 minutes", env: { CI: "1" } });
  if (r.code !== 0) return { failed: r.stderr };
  return { ok: r.stdout };
}
```

- It is `bash -lc <command>` as the computer, in `~/fragment` (or
  `cwd`), with this CLI on its PATH signed in as the computer: `fragment
  post`, `fragment call`, `fragment sync` reach your fragments as it.
- It answers `{code, stdout, stderr, truncated}`; a nonzero exit is an
  answer, not a throw. Each stream keeps its first 256 KiB. `timeout` is
  ms or "N seconds|minutes" (default 10 minutes, at most 60); past it the
  command is stopped and answers code 124.
- It runs once: a retried or replayed run gets the same command's answer
  and never runs it again. `env` takes plain values; for a secret, use
  `job.fetch`.
- The computer is woken for it and sleeps 5 minutes after, billed to
  your budget. No computer declared, or no budget left, throws a
  `StepError`.

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
fragment events my-thing --tail 30          # the log's newest 30 (at most 500)
```

An operation pauses its own triggers after 5 held runs in 10 minutes or
120 triggered runs in an hour; the event log says so (`op.auto-paused`).

## Calling and following

```
fragment call my-thing add --input '{"text":"hi"}' [--id ID]   # a retry with the same --id is a replay
fragment channel my-thing                                      # list channels
fragment channel my-thing activity --follow                     # the backlog a page at a time, then new records, as JSON lines
```

## People

```
fragment visibility my-thing [public|link|members]
fragment members list my-thing
fragment members add my-thing <id:… | npub | name@domain> --role editor   # a key names its holder
fragment members rm my-thing <id:… | npub>
fragment members leave my-thing
fragment invite create my-thing --role viewer --uses 5    # prints a link to open in a browser (once)
fragment join my-thing <token>                            # or join from a CLI
fragment rotate my-thing --view                            # a new share link
```

Only the owner manages members, invites, visibility, and tokens.
`fragment.json` grants nothing.

## Your AI budget

The fragments you own pay for their AI (`job.ai`) from your monthly
budget, whoever started the run, unless a fragment sets its own
`OPENROUTER_API_KEY`; so do your agents' model calls, whoever asked them. A step reserves its worst case first; one the month
cannot cover is held ("budget used up"): replay it after a top-up or next
month.

```
fragment budget              # what is left this month, and what spent it
fragment budget usage        # every paid step this month (--period 2026-09)
fragment runs my-thing       # each run shows what it cost
```

## You and your keys

```
fragment whoami              # your identity, this key, your other keys, your agents
fragment keys rotate         # a new key replaces this one: every membership stays
fragment keys revoke <npub>  # revoke another of your keys (never the last)
```

A revoked key is refused from its next request and never comes back.

## Agents

An agent is an identity with its own key, a model, and a conversation
of its own (`agent/`, goose's loop), and you own it. Its tools are the
operations of the fragments it belongs to, so what it may do is what its
memberships allow. As its owner you can read whatever it can read (as a
viewer), but you never act through it.

```
fragment agent create my-bot                          # makes it and registers it as yours; prints its id
fragment members add my-thing <bot id> --role editor     # now it has my-thing's operations as tools
fragment agent tools my-bot
fragment agent say my-bot "add milk to the list"      # waits, prints the answer
fragment agent show my-bot                            # its turn and recent messages
fragment agent stop my-bot
```

A message sent while it works steers the running turn. A chat (the
`chat` template) with the agent as a member, after `fragment agent listen
my-bot my-chat`, gets an answer to every message from someone else. The
agents answer on the platform's own host (their script is co-hosted in
its fleet); `FRAGMENT_AGENTS` names another.

An agent with a computer also gets goose's developer tools (shell,
write, edit, tree) there, and `screenshot {url}` (headless Chrome; a
turn in a chat shows the image there). On the computer (a CLI built with
`--features computer`):

```
fragment computer serve --listen 0.0.0.0:8080   # makes .fragment-computer/token the first time
```

and from anywhere, with a copy of that token file:

```
fragment agent computer my-bot --url https://my-computer.example --token-file token --cwd site
fragment agent computer my-bot --detach
```

or, with no public URL, the computer connects out:

```
fragment agent computer my-bot --connect --token-file token   # prints the command below
fragment computer connect --agent <its URL> --token-file token   # on the computer
```

Its commands run in `work/<cwd>` on the computer. Stop kills a running
command; a replayed call never runs twice, and one the computer's own
restart cut off comes back "interrupted".

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
fragment login [--force] [--no-wait]     fragment call <name> <op> [--input JSON] [--id ID]
fragment login --computer <name>         fragment computers [rm <name>]
fragment model <prompt> | --request      (a computer: the model, on its owner's budget)
fragment whoami                          fragment channel <name> [<channel>] [--after N] [--follow]
fragment username [<name>]
fragment keys [list|rotate|revoke <npub>]
fragment budget [usage [--period P] | top-up <id> <usd>]
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
fragment rm <name>
fragment agent create|show|say|stop|tools|listen|computer ...
fragment computer serve [--listen A]     fragment guide | skill
fragment computer connect --agent U --token-file F
```

Global flags: `--host <url>` (or `FRAGMENT_HOST`, or `fragment host
<url>` to save one), `--json`, `-v`. With `--json` (or
`FRAGMENT_OUTPUT=json`), stdout is exactly one line: `{"ok":true,
"data":…}` or `{"ok":false,"error":{"code","message","hint","id"}}`
(`id`: a failed `fragment call`'s). The codes are stable, and a host's
refusal gets its code from the error code in its answer, never from
its wording:

- `invalid_usage`: the command was called wrong (exit 2)
- `invalid_request`: the host refused the request; the message says why (400)
- `auth_failed`: no key here, or the host does not know it (401)
- `forbidden`: signed, but your role does not allow it (403)
- `not_found`: no such fragment, route, or operation (404)
- `name_taken`: it exists already (409)
- `conflict`: the branch moved under a sync, a deploy, or a manifest-set
- `conflicting_body`: that operation id already ran with another input (409)
- `too_large`: over a limit the message names (413)
- `app_failed`: the app's code refused or threw (422)
- `rate_limited`: too many calls; back off (429)
- `budget_used_up`: this month's AI budget cannot cover it (402)
- `storage_full`: the app's database is at its cap; the change rolled back (507)
- `unavailable`: the host, or a service behind it, did not answer; retrying is safe
- `outcome_unknown`: a write's answer was lost; it may have been applied
- `server_error`: the host failed (500), or the CLI did

Exit codes: 0 ok, 1 failure, 2 usage. Without `--json`, a refusal
prints its message and a hint on stderr. `-v` logs each signed request
to stderr and leaves stdout clean.

A network failure is retried (three attempts in all) for reads, blob
uploads, and `fragment call`, which are safe to repeat (a call carries
its id, and the fragment answers a repeated id with the first answer);
any other write is retried only when the connection never opened. A
write that reached the host but lost its answer fails with
`outcome_unknown`: it may have been applied, so check before repeating
it. A failed `fragment call` names its id (in the message, and as
`error.id` with `--json`, whether you gave `--id` or the CLI made one
up); when its outcome is unknown, call it again with that `--id`, which
replays it and never runs it twice. A request may take 30 s plus a
second for every 32 KiB it uploads.

The code.storage server is named by the host; to point the CLI at
another, set `FRAGMENT_CODESTORAGE_URL` or `"codestorage"` in
`~/.config/fragment/config.json`.
