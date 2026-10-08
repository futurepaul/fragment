# fragment — agent guide

A **fragment** is one small place on the web: a folder of files in git,
an app of named operations over its own SQLite, channels that pages
follow live, members with roles, and URLs. Fragments live on
fragment.club (invite-only for now), on Cloudflare, and sleep when idle;
a request, a trigger, or an inbox delivery wakes them.

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
rotate` replaces the key and keeps everything you have.

A person chooses a username once (`fragment username <name>`, or the
host's page after the first sign-in) and can create nothing before. A
fragment's name is `<label>.<username>`, served at
`<label>--<username>.<suffix>`; in a command, a bare label names one of
yours (`fragment status todo` is `todo.<your username>`).

## As an agent, on a computer

An agent on a computer (docs/computers.md) holds no key and logs in to
nothing: its computer's API signs each request as the agent. The CLI
does what that needs when the computer sets:

- `FRAGMENT_AS_AGENT=<agent fragment>` (`juniper.paul`): every request
  names the agent (`x-fragment-agent`) and carries no signature of its
  own; the computer's egress signs it as that agent, which then holds
  exactly its grants.
- `FRAGMENT_FOR=<id:…>`, optional: the person the agent acts for (its
  owner), named as `for` on a fragment's routes and the fragment list,
  the routes that honor it. The agent then holds that person's role,
  never above it (and never above an editor): `fragment list` lists their
  fragments, and what `fragment create` makes is theirs, the agent its
  editor.
- The host is `--host`, else `FRAGMENT_HOST`, else the computer's
  `FRAGMENT_API` (`http://api.fragment.internal`), never the config's or
  the default: an unsigned request means nothing anywhere else.

```
FRAGMENT_AS_AGENT=juniper.paul FRAGMENT_FOR=id:… fragment list
```

A computer's image sets all three in each agent's terminal. `fragment
whoami` says which agent it is and for whom it acts; `fragment login` and
`fragment keys` refuse (exit 2): an agent's keys are its owner's to
manage.

An agent acting for its owner shares its owner's fragments as its owner
would: `fragment members add|rm`, `fragment invite create|list|revoke`,
`fragment visibility`, and `fragment rotate`, on a fragment its owner
owns. Do it when your owner asked for it, and say so in the chat: who
you added and at what role, what is public now, which link you rotated.
Your owner's events and members name you as who did it: each event's
summary says "(an agent, for …)" (`by` and `for` in `fragment events
--json`), and a member you added has you as `addedBy` (`fragment members
list --json`). The
platform refuses (403) sharing acting for anyone else, on a fragment
your owner only edits or views, or while your owner holds you below
them, and never lets an agent delete a fragment or set its cap. Links meant for people (an invite, a webhook URL) name the
platform's public origin, which a fragment's status reports
(`urls.platform`), not the computer's internal host. `fragment write`
(one text file to main, through the platform) and `fragment deploy`
without `--dir` (the platform moves live) need nothing but the API, so
they are an agent's way to build and publish. `fragment sync` and
`deploy --dir` still talk to code.storage directly, with the
short-lived, repo-scoped token the platform mints for the agent.

An agent asks another of its owner's agents (one its computer runs)
with `fragment ask`:

```
fragment ask fred "what's on the calendar Friday?" --wait   # prints fred's answer
fragment ask fred "draft the reply" --chat team-chat         # asks in that chat; the answer comes there
```

The question goes to a chat of the two agents and their owner
(`<a>-<b>`, made the first time), or to `--chat`; whichever of the two
is not in that chat is added as an editor (the owner's to do, so its
agent may, on the owner's chats). It is a message whose `to` names the
asked agent; its turn's replies come in that chat, where it can @name
you back. `--wait [secs]` (150 by default, at most 1800) follows the
chat for that turn and prints its replies once it ends, or says none
came. `--json`: `{chat, asked: {fragment, identity, name}, record,
replayed, created, added, answer?: {turn, outcome, error?, replies:
[{seq, text, attachments}]}}`. A person asks one of their own agents the
same way, in their direct chat with it (`<agent>-chat`). The answering
agent's computer counts the hand-offs: three in a row without a person
stop, and a chat allows its agents 20 turns of each other in 5 minutes
(docs: chat records).

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
fragment init my-inbox --template inbox   # or: todo | notes | calories | blank
```

`fragment create <name> --template T` makes one on the platform with no
folder: a blessed template (`chat`, `agent`, `skills`, …) runs the
platform's current release and names it in `fragment.json`, and `--title`
gives it its own title; any other is copied in as its first commit.
`fragment new <dir> --template T` scaffolds without creating;
`fragment new --list` lists the templates:

- `todo`: mutations over SQLite, a public activity channel, a live page.
- `inbox`: webhook deliveries start a job that fetches and records.
- `notes`: a folder of markdown as a live site; the files are the state.
- `calories`: a food log you tell what you ate; a text step logs it.
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
fragment sync my-thing --dir . --live       # what is live, not main: repo → folder, deletions included
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
    "list":   { "kind": "query", "role": "public", "description": "Every item, oldest first." },
    "ingest": { "kind": "job" }
  },
  "channels": { "activity": { "read": "viewer" } },
  "triggers": [{ "channel": "inbox", "run": "ingest" }],
  "meta": { "title": "My thing", "description": "Shown in link previews." }
}
```

- An operation's name is a method of your `App`, matching
  `^[a-z][a-z0-9_]{0,63}$`. A deploy whose `fragment.json` the platform
  refuses (a bad name, an unknown kind) keeps the last good code serving,
  and `fragment deploy` says why and exits 1.
- `role` is who may call it: `public`, `viewer` (default for queries),
  `editor` (default for mutations and jobs), `owner`.
- `input` is a JSON Schema (types, enums, lengths, ranges, `properties`,
  `required`, `additionalProperties`, `items`). A call that does not fit
  is refused before your code runs, naming the field.
- `description` (1 to 1024 characters) says what it does and answers,
  for an agent: it makes the operation a tool of `fragment mcp` ("Use it
  from another agent", below), and `fragment status` shows it.
- `"ephemeral": true` on a mutation you call often with a "latest value":
  its calls keep no ledger row (a mutation's id is
  otherwise kept a week in your app's 16 MiB database), so the same id
  runs again rather than replaying, and it may not publish, push, or
  write files.
- A channel with a `post` role (`"post": "viewer"`) takes records people
  post (`fragment.post`, `fragment post`); `"signedIn": true` beside it
  refuses anyone not signed in (a link holder is a viewer).
- Your `App` is a Durable Object with its own SQLite, and some of its
  Durable Object is not yours to use: no alarm (`triggers` run it on a
  schedule), no `ctx.storage.transaction` (a mutation is the
  transaction), no `ctx.storage.put` or `delete` (write SQL in a
  mutation), and no facets of its own. Each throws, saying so.

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
- **AI** (jobs): `job.ai.text({model, prompt, max_tokens, reasoning_effort})`,
  `job.ai.image({prompt, path, steps})`. Text runs on a tier,
  `model: "cheap"` (the default) or `"medium"`; an image is a JPEG
  (FLUX.1 [schnell], 1 to 8 `steps`, 4 by default) written to `path` on
  `main`, which ends in `.jpg` or `.jpeg`. You pay for them, from your
  ledger (below). `job.ai.video` is off until videos run on Cloudflare.
  GLM can spend a small `max_tokens` thinking: `reasoning_effort` is
  `low` unless you ask for `high`.

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

`<link rel="stylesheet" href="./__fragment.css">` gives a page the
platform's theme: warm neutrals and one accent, light and dark, as
variables (`--bg`, `--fg`, `--muted`, `--line`, `--accent`, `--danger`,
`--font`, `--radius`), and plain base styles for text, forms and
buttons. Your rules after it override any of it; a page that doesn't
link it gets none of it.

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
fragment call my-thing add --input @input.json                 # or - for stdin: an input over 128 KiB
fragment channel my-thing                                      # list channels
fragment channel my-thing activity --follow                     # the backlog a page at a time, then new records, as JSON lines
```

## Use it from another agent

`fragment mcp <name>` serves a fragment's operations to any agent that
speaks MCP (Claude Code, goose, …) as tools, over stdio: each operation
whose `fragment.json` entry has a `description` and that your role may
call, with its input schema. Its queries always; with `--write`, its
mutations and jobs too.

```
claude mcp add my-thing -- fragment mcp my-thing            # read-only: its described queries
claude mcp add my-thing -- fragment mcp my-thing --write    # and its described mutations and jobs
```

A tool's call is a `fragment call` with a fresh id, signed with this
machine's key as every command is (`fragment login` first). A result
with a string `text` (a view the operation rendered) answers as that
text, any other as JSON, and a refusal is the tool's error, with the
platform's message. The tools are read when the client connects and
each time it lists them. Stdout carries the protocol alone: a failure
to start goes to stderr, and `-v` logs each request there.

On a computer it runs in the agent mode ("As an agent, on a computer"),
as the agent, with no key: name the fragment in full.

```
FRAGMENT_AS_AGENT=juniper.paul FRAGMENT_API=http://api.fragment.internal fragment mcp mind.paul
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

## Your ledger

You pay for what is yours: your fragments' hosting (requests, storage, a
code version's day) and their AI (`job.ai`), whoever started the run, and
your agents' model calls, whoever asked them. A seat includes credit
each month; operators grant more. A paid step reserves its worst case
first, and one your ledger cannot cover is held, saying why: replay it
after a top-up or next month.

- At zero, agents and AI stop; your fragments keep serving and taking
  writes.
- $2 below zero, your fragments go read-only (reads still serve) until a
  top-up brings you above zero: their cron and triggers start no runs
  meanwhile (each shows in `fragment runs` as `blocked`, saying why), and
  you make no new fragment.
- A guest makes no fragments (`fragment create` is refused, 403): they
  edit the fragments shared with them, whose owners pay.
- Each fragment has a monthly cap, $5 unless you set one: past it, AI
  steps and agent turns there stop for everyone but you.

```
fragment ledger                    # your credit, your plan, what is stopped, this month's spend by fragment
fragment cap my-thing 10           # my-thing's cap: $10 a month (or `default`)
fragment runs my-thing             # each run shows what it cost
fragment ledger grant ann 20 --why "a top-up"   # operators only
```

## You and your keys

```
fragment whoami              # your identity, this key, your other keys, your agents
fragment keys rotate         # a new key replaces this one: every membership stays
fragment keys revoke <npub>  # revoke another of your keys (never the last)
```

A revoked key is refused from its next request and never comes back.

## Agents

An agent is an identity of its own, and you own it: an agent fragment
your computer runs (you make one in the shell; "As an agent, on a
computer" above). What it may do is what its memberships allow, and
acting for you it never holds more than you do. As its owner you can
read whatever it can read (as a viewer), but you never act through it.

```
fragment members add my-thing <agent id> --role editor   # now it can call my-thing's operations
```

## Secrets

```
fragment secret set my-thing API_KEY         # value from argv, else $API_KEY, else stdin
fragment secret list my-thing                # names only; values never come back
fragment secret rm my-thing API_KEY
```

Your code never holds a secret: `job.fetch` fills `{{API_KEY}}` in a
header at the way out. Never write secret values into files.

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
fragment login [--force] [--no-wait]     fragment call <name> <op> [--input JSON|@file|-] [--id ID]
fragment whoami                          fragment channel <name> [<channel>] [--after N] [--follow]
fragment username [<name>]
fragment keys [list|rotate|revoke <npub>]
fragment ledger [grant <who> <usd> --why W]
fragment cap <name> <usd>|default
fragment host [<url>]                    fragment runs <name> [<run>] [--status S] [--limit N]
fragment init <name> [--template T]      fragment replay <name> <run>
fragment new <dir> [--template T]        fragment triggers <name>
fragment new --list                      fragment pause|unpause <name> <op>
fragment create <name> [--visibility V] [--template T [--title T]] [--show-tokens]
fragment inbox <name> --token T --payload JSON
fragment list                            fragment rotate <name> [--inbox] [--view]
fragment status <name>                   fragment visibility <name> [V]
fragment open <name>                     fragment members list|add|rm|leave ...
fragment events <name> [--since N | --tail N]
fragment manifest <name>                 fragment invite create|list|revoke ...
fragment join <name> <token>
fragment sync <name> [--dir D] [--watch] [--mode M | --live]
fragment verify <name> [--dir D]         fragment secret set|list|rm ...
fragment deploy <name> [--dir D] [--note N]
fragment write <name> <path> --text T | --from FILE|- [--message M]
fragment drafts <name>                   fragment rollback <name> [--to <sha>]
fragment rm <name>                       fragment guide | skill
fragment ask <agent> <text> [--chat C] [--wait [S]] [--id ID]
fragment mcp <name> [--write]
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
- `forbidden`: signed, but your role, or your plan, does not allow it (403); the message says which (a guest makes no fragments)
- `not_found`: no such fragment, route, or operation (404)
- `name_taken`: it exists already (409)
- `conflict`: the branch moved under a sync or a deploy
- `conflicting_body`: that operation id already ran with another input (409)
- `too_large`: over a limit the message names (413)
- `app_failed`: the app's code refused or threw (422)
- `rate_limited`: too many calls; back off (429)
- `budget_used_up`: your ledger refused it: no credit, a fragment's cap, or read-only past the overdraft (402); `fragment ledger` says which

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
