# fragment — agent guide

A **fragment** is one small place on the web: a folder of files in git,
an app of named operations over its own SQLite, channels that pages
follow live, members with roles, and URLs. Fragments live on the
platform at {{platform}}, where people sign in and find their apps, each
served at its own origin, {{fragment}}; they run on Cloudflare, and
sleep when idle: a request, a trigger, or an inbox delivery wakes them.

This manual is for an agent building for a person: a coding agent
(Claude Code, Codex, Claude desktop's Code tab) with the CLI, or an
agent on a Fragment computer. {{platform}}/llms.txt (`fragment skill`)
is its one-page handoff.

## Install and pair

You drive fragments with the `fragment` CLI. On macOS or Linux, with no
sudo (on Windows, in WSL; the same command updates it):

```
mkdir -p ~/.local/bin && curl -fsSL https://github.com/finitecomputer/fragment/releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin
```

If `fragment` is then not found, run it as `~/.local/bin/fragment`, or
put `~/.local/bin` on your PATH: `export PATH="$HOME/.local/bin:$PATH"`,
in `~/.zshrc` or `~/.bashrc`.
`fragment skill` prints a SKILL.md for a coding agent (Claude Code,
Codex) that points it here.

Every request is signed with this machine's nostr key. Pair it once:

```
fragment login --host {{platform}}
```

It makes the key, asks the host for a code (eight letters, good for ten
minutes, once), prints it, and opens the host's `/cli` page: your person
signs in there, types the code, and adds the key, whose ending the page
and the terminal both show. The page reads nothing from its address, so
no link anyone sends can add a key: only the code typed from this
terminal. Then it prints whose account it acts as, by email, and the
host mails that person each key added, with `fragment keys revoke` for
one they did not add. On a machine without a browser, it prints the
code and the page to open anywhere they are signed in (`--no-wait`
returns at once; run it again once the code is typed).
The CLI keeps the host `--host` named once it answered, so later
commands need none; `FRAGMENT_HOST` (an environment's, never saved) or
`fragment host <url>` names another. The host knows which identity (an npub) each key belongs to:
memberships name you, not the key. `fragment keys rotate` replaces the
key and keeps everything you have.

A fragment's name is its label and a random suffix the platform adds
(`fragment create todo` makes `todo--k3x9`), served at {{fragment}};
in a command, a bare label names the one of yours with it (`fragment
status todo`), and a name in full names any fragment.

## As an agent, on a computer

An agent on a computer (docs/computers.md) holds no key and logs in to
nothing: its computer's API signs each request as the agent. The CLI
does what that needs when the computer sets:

- `FRAGMENT_AS_AGENT=<agent fragment>` (`juniper--k3x9`): every request
  names the agent (`x-fragment-agent`) and carries no signature of its
  own; the computer's egress signs it as that agent, which then holds
  exactly its grants.
- `FRAGMENT_FOR=<npub>`, optional: the person the agent acts for (its
  owner), named as `for` on a fragment's routes and the fragment list,
  the routes that honor it. The agent then holds that person's role,
  never above it (and never above an editor): `fragment list` lists their
  fragments, and what `fragment create` makes is theirs, the agent its
  editor.
- The host is `--host`, else `FRAGMENT_HOST`, else the computer's
  `FRAGMENT_API` (`http://api.fragment.internal`), never the config's or
  the default: an unsigned request means nothing anywhere else.

```
FRAGMENT_AS_AGENT=juniper--k3x9 FRAGMENT_FOR=npub1… fragment list
```

Our Hermes image sets all three in each agent's terminal. `fragment
whoami` says which agent it is and for whom it acts; `fragment login` and
`fragment keys` refuse (exit 2): an agent's keys are its owner's to
manage.

An agent acting for its owner shares its owner's fragments as its owner
would: `fragment members add|rm`, `fragment invite list|revoke`,
`fragment visibility`, and `fragment rotate`, on a fragment its owner
owns. Do it when your owner asked for exactly that, or once they said
yes when you asked them in plain words (never with a command), and say
so in the chat: who you added and at what role, what is public now,
which link you rotated.
Your owner's events and members name you as who did it: each event's
summary says "(an agent, for …)" (`by` and `for` in `fragment events
--json`), and a member you added has you as `addedBy` (`fragment members
list --json`). The
platform refuses (403) sharing acting for anyone else, on a fragment
your owner only edits or views, or while your owner holds you below
them, and never lets an agent delete a fragment or set its cap. Links meant for people (a share link, a webhook URL) name the
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

## Who can do what, where

| | call operations | read or write records | use models | decide | share and invite | deploy |
|---|---|---|---|---|---|---|
| **the person, in a browser** | on an app's pages (`fragment.call`), at their role | the channels their role reads, live; posts where a channel's `post` role lets them | through their agents and their apps' jobs | through their apps' jobs | the owner, from the share sheet: members by email, the link, visibility | no: they ask an agent for an app (the shell's Add suggests what to ask) |
| **their agent, on its computer** | `fragment call`, at its own grants, or for its owner at their role (at most an editor's) | `fragment channel`, `fragment post`; its chats are records | its turns, on its tier, paid by its owner | `POST $FRAGMENT_MODEL/v1/decide` from its computer (Clef, its media inline), paid by its owner; and through its owner's apps' jobs | for its owner, on what its owner owns, when asked (never a delete or a cap) | `fragment write`, `fragment deploy` (an editor's); what it makes is its owner's |
| **an app's own jobs** | its own operations (`job.call`); a triggered run acts as the fragment, an editor | `job.publish` to its own channels, and a text step's draft to them as it streams; a trigger hands it the record | `job.ai.text` (with tools, run by the job), `job.ai.image`, paid by the fragment's owner | `job.ai.decide` (Clef, about text, JSON and the fragment's blobs), paid by the fragment's owner | no | no: `job.files.write` writes to `main`, live at the next deploy |
| **a coding agent, with the CLI** | `fragment call`, as the person whose key it holds | `fragment channel --follow`, `fragment post` | its own; the platform's through the jobs it calls | through the jobs it calls | `fragment members`, `invite`, `visibility`, `rotate` (the owner's) | `fragment deploy`, `rollback` (an editor's) |
| **an MCP client, through `fragment mcp`** | the described operations, as tools: queries, and with `--write` mutations and jobs | only through operations | its own; the platform's through a described job | through a described job | no | no |

Every row acts as someone: a person, their agent, or the fragment
itself, each held to that one's role on the fragment (People, below).
What costs money is paid from an owner's ledger: a fragment's owner for
its jobs, an agent's owner for its turns (Your ledger, below).

## First moves

```
fragment login --host {{platform}}        # once per machine: type its code at the host's /cli, signed in
fragment init my-thing                    # scaffold (todo) + create + deploy → live URL,
                                          #   share link, webhook URL
fragment init my-inbox --template inbox   # or: todo | notes | calories | doc | blank
```

`fragment new <dir> --template T` scaffolds a folder without creating
it; `fragment init` scaffolds, creates and deploys. `fragment new --list`
lists their templates:

- `todo`: mutations over SQLite, a public activity channel, a live page.
- `inbox`: webhook deliveries start a job that fetches and records.
- `notes`: a folder of markdown as a live site; the files are the state.
- `calories`: a food log you tell what you ate; a text step logs it.
- `doc`: a page of writing its readers leave notes on, paragraph by
  paragraph, in the margin (Notes on a page, below).
- `blank`: one page, to build on.

`fragment create <name> --template T` makes one on the platform with no
folder, from the platform's own templates: one of `blank`, `todo`,
`inbox`, `calories`, `doc` is copied in as its first commit (`notes` is
`new`'s and `init`'s alone); a blessed one, `agent`, `chat`, `brain`,
`skills`, runs the platform's current release and is named in its
`fragment.json`. Either way `--title` gives it its own title, its
`fragment.json`'s `meta.title`, over the template's. Any other is
refused, naming them.

`fragment status my-thing` shows the URLs, the view token (the share
link's `?view=`: its owner's alone), and the inbox token. `fragment open my-thing` prints
the links again.

Making one for someone, go in this order:

1. State the app in one sentence: who it is for, and the job it does.
2. Build the smallest version that does that job, and deploy it.
3. Check its state lives in the app, not the page: reload, and it is all
   still there. Then check a second person sees it: open it in two tabs
   (or keep one open and change it with `fragment call`), and a change
   in one shows in the other without a reload. Live multiplayer is what
   a fragment has that a static page doesn't: use it (`fragment.live`,
   Pages below).
4. Hand back the link, with one suggested next step.
5. Add nothing they didn't ask for.

## The folder

```
fragment.json      # operations, channels, triggers, meta
app.mjs            # class App: one method per operation (at most 1 MiB)
applib/            # modules app.mjs imports (at most 64 modules, 4 MiB with app.mjs)
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

Then check the page. Shortly after each deploy (not in its answer), the
platform loads the page once in a browser for its preview card, as a
visitor without an account sees it, and `fragment status` shows what
the page reported under `page`: uncaught exceptions, console errors,
failed loads, and Content Security Policy violations, each with its
`kind`, `text`, and `source` (the script and line). Once `page.live` is
the commit you deployed (`pins.live`), `page.errors` should be empty;
`fragment events` says `page.errors` when it is not. It is one load with
no clicks: it catches a syntax error, a missing import, a blocked
script, not a bug behind a button. A members-only fragment's page is not
loaded, so it gets no report.

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
                "description": "Add an item with this text. Answers its {id}.",
                "input": { "type": "object", "required": ["text"], "additionalProperties": false,
                           "properties": { "text": { "type": "string", "maxLength": 200 } } } },
    "list":   { "kind": "query", "role": "public", "description": "Every item, oldest first: {items: [{id, text}]}." },
    "ingest": { "kind": "job", "description": "The inbox's trigger: adds each webhook delivery as an item." }
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
- `description`: say what every operation does and what it answers, in
  plain text (1 to 1024 characters). It is how a person reading
  `fragment status` (under `code.operations`), or an agent choosing among
  your operations, knows what to call. An empty or longer one is refused
  at deploy. A described operation is a tool (Your operations as tools,
  below); one without a description is no tool.
- `input` is a JSON Schema (types, enums, lengths, ranges, `properties`,
  `required`, `additionalProperties`, `items`). A call that does not fit
  is refused before your code runs, naming the field.
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
  happen once, after it commits: at most 64 of them in all.
- **Jobs** re-run from the top at every step with the results so far, so
  reach steps in the same order every time and change nothing except
  through steps. The steps: `job.call(op, input)`, `job.fetch(url,
  init)` (the app's only way out; `{{NAME}}` in a header value is the
  secret `NAME`, filled in outside your code), `job.publish(channel,
  body)`, `job.sleep("2 hours")`, `job.files.read|list|stat|write|remove`
  (`write(path, content, {expect: sha})` compares and swaps),
  `job.push(who, payload)`, `job.ai.text|decide|image|video(...)`. A step that
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
- **AI** (jobs): `job.ai.text({model, prompt | messages, max_tokens,
  reasoning_effort, tools, tool_choice, draft})`, `job.ai.image({prompt,
  path, steps})`. Text runs on a tier, `model: "cheap"` (the default) or
  `"medium"`; an image is a JPEG (FLUX.1 [schnell], 1 to 8 `steps`, 4 by
  default) written to `path` on `main`, which ends in `.jpg` or `.jpeg`.
  You pay for them, from your ledger (below). `job.ai.video` is off until
  videos run on Cloudflare. GLM can spend a small `max_tokens` thinking:
  `reasoning_effort` is `low` unless you ask for `high`. A text step
  answers `{text, message, finish_reason, model, tier, usage, timing}`
  (`timing`: `{first_ms, ms, model, calls, tries}`). It takes OpenAI's
  `tools` (at most 64, each `{type: "function", function: {name,
  description, parameters}}`, `parameters` an object's JSON Schema) and
  `tool_choice`, so a job runs its own tool loop with no computer: when
  `finish_reason` is `tool_calls`, push `message` onto `messages`, run
  each of `message.tool_calls` yourself, push `{role: "tool",
  tool_call_id, content}` for each, and call again. Each call is a step
  (a run takes at most 256): bound the loop. With `draft: {channel,
  turn}` its text streams into that channel's drafts as it comes (at
  most 4 frames a second; `{text: "", thinking: ms}` while the model has
  no words yet), and the record you then publish with the same `turn`
  replaces it on a page. A step's arguments are at most 512 KiB.

A job that answers a chat with a tool, its reply streaming into the
chat (`fragment.json`: `"channels": {"chat": {"read": "viewer", "post":
"viewer", "signedIn": true}}`, `"triggers": [{"channel": "chat", "run":
"answer", "from": "person"}]`, so its own replies start no run):

```js
const WEATHER = { type: "function", function: { name: "weather", description: "Today's weather in a city.",
  parameters: { type: "object", properties: { city: { type: "string" } }, required: ["city"] } } };

async answer({ record }, job) {
  const turn = `reply:${record.seq}`;
  const messages = [{ role: "user", content: String(record.body.text ?? "") }];
  for (let i = 0; i < 6; i++) {
    const r = await job.ai.text({ messages, tools: [WEATHER], draft: { channel: "chat", turn } });
    messages.push(r.message);
    if (r.finish_reason !== "tool_calls") return job.publish("chat", { turn, text: r.text });
    for (const call of r.message.tool_calls) {
      let city = "";
      try { city = JSON.parse(call.function.arguments).city; } catch {}
      const res = await job.fetch(`https://wttr.in/${encodeURIComponent(city)}?format=3`);
      messages.push({ role: "tool", tool_call_id: call.id, content: res.text() });
    }
  }
  return job.publish("chat", { turn, text: "I stopped: too many tool calls." });
}
```

Its page shows the reply as it streams: `fragment.subscribe("chat",
show, { onDraft: (d) => d.text === null ? clear(d.turn) : d.thinking !==
undefined ? showThinking(d.turn, d.thinking) : showDraft(d.turn, d.text)
})`, and the record with the same `turn` replaces the draft.
- **Decisions** (jobs): `job.ai.decide({model, input, questions,
  media})` answers typed questions about text, JSON, photos, voice notes
  or video clips, with no prose to parse. Use it to sort, route and gate;
  use text for writing or for pulling out arbitrary fields.
  - `model`: `"clef-flash"` (the default: cheapest, text and images),
    `"clef"` (larger: text and images), or `"clef-omni"` (text, images,
    audio and video).
  - `input`: text or JSON about the thing decided.
  - `questions`: 1–64, each `{type, instructions}`: `choice` with
    `options: {id: description}` (2–255), `predicate`, or `score` with
    ordered `levels: [description, ...]` (2–10). Read
    `out.answers[id].choice`, `.probability` or `.score` (a weighted
    zero-based level); choice and score also give `probabilities` and
    `confidence`.
  - `media`: up to 4 items (2 videos). A file people uploaded from your
    page is `{sha256, type}`, exactly what `fragment.blob(file)` returns
    (a chat's `attachments` pass as they are); a small image can go
    inline as `{data: "data:image/png;base64,…"}` (256 KiB in all).
    Types: PNG, JPEG, WebP images on any model; WAV or MP3 audio and MP4
    or WebM video on `clef-omni` only. A browser's own recording
    (`audio/webm`) is refused: record or convert to WAV. At most 4 MiB
    an image, 8 MiB an audio clip or video, 8 MiB in all.

  A wrong model, type, size or shape fails the step before anything is
  spent, saying why. You pay for input tokens only (media counts as
  input), from your ledger: Clef-flash $0.038, Clef $0.24, Clef-omni
  $0.15 per million, plus the platform's fee and margin. A page
  uploading a photo or a voice note, and the job sorting it:

  ```js
  // the page, from a file input
  const blob = await fragment.blob(input.files[0]); // an editor's page: {sha256, size, type, name}
  await fragment.call("sort", { sha256: blob.sha256, type: blob.type });
  ```

  ```js
  async sort({ sha256, type }, job) {
    const audio = type.startsWith("audio/");
    const out = await job.ai.decide({
      model: audio ? "clef-omni" : "clef-flash",
      input: audio ? "A voice note left for the household" : "A photo left for the household",
      media: [{ sha256, type }],
      questions: {
        kind: { type: "choice", instructions: "What is this about?", options: {
          groceries: "Food or supplies to buy", chores: "A job around the house", other: "Anything else" } },
        urgent: { type: "predicate", instructions: "Does it need doing today?" },
      },
    });
    const { kind, urgent } = out.answers;
    return { kind: kind.choice, urgent: urgent.probability > 0.7, sure: kind.confidence };
  }
  ```

  Typed answers can still be wrong: keep the corrections people make,
  and tune thresholds against examples from your app.

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
platform's look, the shell's: Funnel Sans, warm greys and one blue,
light and dark, as variables (`--bg`, `--fg`, `--muted`, `--faint`,
`--line`, `--line-strong`, `--surface`, `--soft`, `--accent`,
`--on-accent`, `--success`, `--danger`, `--font`, `--font-display`,
`--font-mono`, `--text-sm`, `--text-lg`, `--text-xl`, `--radius`),
and base styles for text, forms and pill buttons. Your rules override
the base styles, which have zero specificity. The system chooses light
or dark; a page can choose with `color-scheme` on `:root` or
`data-theme="light"` / `"dark"` on `<html>`. A page that doesn't link
it gets none of it.

Visitors without a key call as an anonymous principal (a cookie), so
`public` operations work on a public fragment with no login.
`await fragment.push.register(who)` (from a click) subscribes the
browser to web push; `call.push(who, payload)` or `job.push(...)` reaches
every browser registered with that `who` (`*` for all).

The share link's `?view=` token is a secret, its owner's to give. A
browser that opens the link keeps the token in an HttpOnly cookie and
goes on to the page's URL without it, so your page's code never sees it
(`location`, `document.cookie`, an app's `fetch`, and the preview card's
error report all lack it), and no page registers a service worker but
the platform's (one would see the link's navigation first): an editor's
code cannot capture it. Never build links that carry it.

## Notes on a page

The `doc` template gives a page notes on its paragraphs, as a shared
document takes comments: each block with a `data-a` id takes them, in
the margin on a wide window and under the paragraph on a narrow one,
live. They are four operations, so you read and answer them as a reader
does (a page's ids are in its `site/index.html`):

```
fragment call <doc> notes                                                  # every note: {id, anchor, quote, text, author, reply_to, resolved, …}
fragment call <doc> annotate --input '{"anchor": "intro", "text": "Why?"}'  # a note on the block data-a="intro"
fragment call <doc> annotate --input '{"reply_to": 3, "text": "Because."}'  # a reply in note 3's thread
fragment call <doc> resolve --input '{"id": 3, "resolved": true}'           # the thread's author or the page's owner
fragment call <doc> remove_note --input '{"id": 4}'                         # likewise; for good
fragment channel <doc> notes --follow                                       # each change as it happens
```

Signed-in people and agents write notes; an anonymous visitor reads
them. Any fragment can have them: copy the template's
`applib/notes.mjs`, `site/notes.js` and `site/notes.css`, extend your
App with `withNotes`, and merge its operations and `notes` channel into
your `fragment.json` (`fragment new <dir> --template doc`: its README.md
says how).
Readers' cursors are off unless `<html data-cursors>`.

## Design

A page should look made for its job, by someone who cared.

- **Design for the job.** Put what the person came for first: who owes
  whom, the vote so far, today's chores. A list with an add button is
  rarely it.
- **Apps default to no header.** No title bar, banner, hero, or
  page-title heading unless the person asks for one. The shell already
  frames the app with its title: start with useful content. Design for
  a narrow pane (about 380 px). A heading names content (a poll's
  question) or a section, rather than repeating the app's name.
- **Mobile first.** Links get opened on phones: one column, thumb-sized
  targets, nothing that needs a hover. Then widen.
- **The platform's look:** link `./__fragment.css` and build on its
  variables. Warm greys and **one accent color**, used sparingly (a
  link, the current state); the main action a solid pill.
- **A quiet type hierarchy:** muted small text for what matters less,
  sizes from a short scale. Spacing from a scale too, with more space
  between groups than within them.
- **None of:** gradients, glow, decorative emoji, heavy shadows.

## The inbox (webhooks in)

Every fragment has an inbox URL: `POST {{platform}}/api/f/<name>/inbox`
with the token in `x-fragment-inbox-token` (or `?t=<inbox token>` for a
sender that cannot set a header), a JSON body of at most 64 KiB. Each
delivery is a record on the `inbox` channel; a trigger
`{"channel": "inbox", "run": "<op>"}` handles it.

```
FRAGMENT_INBOX_TOKEN=<inbox token> fragment inbox my-thing --payload '{"url":"https://example.com"}'
fragment rotate my-thing --inbox        # a new inbox token (the old one stops working)
```

`fragment inbox` reads the token from `FRAGMENT_INBOX_TOKEN`, else
stdin, never its command line (`ps` and shell history keep that), and
sends it in the header.

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

## Your operations as tools (MCP)

`fragment mcp <name>` is an MCP server over stdio: another agent (Claude
Code, Codex, any MCP client) calls a fragment's operations as its tools.

```
claude mcp add my-thing -- fragment mcp my-thing             # Claude Code: its queries
claude mcp add my-thing -- fragment mcp my-thing --write     # its mutations and jobs too
codex mcp add my-thing -- fragment mcp my-thing              # Codex (or in ~/.codex/config.toml:
                                                             #   [mcp_servers.my-thing]
                                                             #   command = "fragment"
                                                             #   args = ["mcp", "my-thing"])
```

- A tool is an operation with a `description` (The app, above) that
  your role there may call and whose input is an object. Its name and
  description are the operation's, and its input schema is the
  operation's `input` (any object when it declares none). An operation
  without a description is no tool.
- Read-only by default: only queries. `--write` serves the mutations and
  jobs too, which change state. A call to one without `--write` is
  refused, and nothing is sent.
- A tool's arguments are the operation's input, as they are (`{"text":
  "milk"}`, not `{id, input}`). Each call is a new call with a fresh id
  (`mcp-…`), so a model asking twice runs it twice. A job's call answers
  its run (`{run, status}`); `fragment runs` follows it.
- Calls are signed with this machine's key, or on a computer as the
  agent (`FRAGMENT_AS_AGENT`), with your role there. The tool list is
  read again at each `tools/list`, so a deploy shows up.
- A refusal is the tool's error, carrying the platform's own error
  (`structuredContent.error`: `{code, status, message}`, its code as the
  host named it: `invalid_request`, `forbidden`, `app_failed`,
  `budget_used_up`, …) and the call's id (`structuredContent.id`). A
  call that got no answer says `unavailable`, or `outcome_unknown` when
  it may have run: `fragment call <name> <op> --id <id>` finds out, and
  never runs it twice.
- One server serves one fragment: add one for each. Claude desktop
  takes one in its config's `mcpServers`: `{"my-thing": {"command":
  "/Users/<you>/.local/bin/fragment", "args": ["mcp", "my-thing"]}}` (a
  full path: an app does not read your shell's PATH). So your person's
  apps are tools in whatever they chat with: `fragment list` shows them.

## People

```
fragment visibility my-thing [public|link|members]
fragment members list my-thing
fragment members add my-thing bea@example.com --role editor   # a person, by email
fragment members add my-thing <npub> --role viewer            # an identity (an agent's too), or any key of one
fragment members role my-thing bea@example.com editor         # someone in already: their role
fragment members rm my-thing <email | npub>
fragment members leave my-thing
fragment invite list my-thing                             # emails no one signs in as yet, mailed and waiting
fragment invite revoke my-thing bea@example.com
fragment rotate my-thing --view                            # a new share link
fragment access                                            # who can open each fragment you own, by email
```

Anything with an `@` is an email. Someone who signs in as it is a member
at once, and their agents with them (a share is the person's and their
agents'); when no one does yet, the platform mails them a link to it,
and they are in once they sign in as that email (within 30 days).
Someone in already is refused: `members role` changes a role. A chat
or an agent is made `members` (only its people open it); the share
link is its owner's to give. The mails
you can have sent are capped by the day. Only the owner manages members,
invites, visibility, and tokens.
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
fragment operator people --q ann@                 # operators: people, orgs, comps,
fragment operator comp ann@example.com --kind always-on   # trial codes, health, the log
fragment operator pin <npub> hermes-canary        # a person's computer onto the canary (or back)
```

## You and your keys

```
fragment whoami              # your identity, this key, your other keys, your agents
fragment keys rotate         # a new key replaces this one: every membership stays
fragment keys revoke <npub>  # revoke another of your keys (never the last)
```

A revoked key is refused from its next request and never comes back.

## Your data

Everything that is yours, as one tar file: your fragments and your
agents' (their files, app data, every channel's records, a chat's
transcript, the files records attach), your computer's newest save, and
the names of the fragments others share with you. Its `README.md` says
what each file is. It is made in the background (you are emailed when it
is ready, and it is kept seven days), three a day at most, and only you
start or download it: an agent cannot, for anyone.

```
fragment export                          # start one (or find the one building), wait, download it here
fragment export --out ~/me.tar           # somewhere else
```

## Agents

An agent is an identity of its own, and you own it: an agent fragment
your computer runs (you make one in the shell; "As an agent, on a
computer" above). What it may do is what its memberships allow, and
acting for you it never holds more than you do. As its owner you can
read whatever it can read (as a viewer), but you never act through it.

```
fragment members add my-thing <agent id> --role editor   # now it can call my-thing's operations
```

An agent's name and picture are its agent fragment's: its title
(`fragment.json`'s `meta.title`) and its `avatar.png` (a PNG, JPEG, WebP
or GIF of at most 256 KiB), as live has them. Every page shows the agent
by them (your shell, its chats, the share sheet, push notifications),
and so does its runtime (Hermes' Bot Mode), within seconds of a change:

```
fragment agent juniper--k3x9 --name Frank          # its name
fragment agent juniper--k3x9 --picture frank.jpg   # its picture (--no-picture takes it away)
fragment agent juniper--k3x9                       # what they are now
```

`fragment agent` checks the picture, writes both in one commit and puts
it live; an agent on a computer leaves its own name out. A picture too
large or that is no picture is refused before anything is written; one
committed by hand is refused when it is deployed (`fragment deploy` and
`fragment status` say why), and the last good name and picture stay.
Its `@name`, the word that mentions it, stays its label. Its runtime's
own files (Hermes' `profile.yaml`) are written from its name: never edit
them to rename it.

## Secrets

```
fragment secret set my-thing API_KEY --to api.example.com   # value from $API_KEY, else stdin
fragment secret list my-thing                # names and hosts; values never come back
fragment secret rm my-thing API_KEY
```

Your code never holds a secret: `job.fetch` fills `{{API_KEY}}` in a
header at the way out, and only toward a host the secret names, over
https (`--to`, once per host, at most 8; a host is exact:
`api.example.com` is not `eu.api.example.com`, and any port of it is
it). A fetch elsewhere, over `http:`, or with a `Host`, `Forwarded`, or
`X-Forwarded-Host` header fails its step, so no editor's code sends a
secret where its setter did not mean it to go. A secret's hosts are its
editors' to know: the event log and a refused fetch name none. The hosts change only with the value:
set it again, value and all. The value comes from the environment
variable of its name, else stdin, never the command line. A secret set
before secrets named hosts fills nowhere until it is set again with
`--to`. Never write secret values into files.

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
fragment login [--force] [--no-wait] [--no-browser]
fragment whoami                          fragment call <name> <op> [--input JSON|@file|-] [--id ID]
fragment keys [list|rotate|revoke <npub>]
fragment ledger [grant <who> <usd> --why W [--id ID]]
fragment export [--out FILE]             fragment channel <name> [<channel>] [--after N] [--follow]
fragment cap <name> <usd>|default        fragment post <name> <channel> --body JSON [--id ID]
fragment host [<url>]                    fragment runs <name> [<run>] [--status S] [--limit N]
fragment init <name> [--template T]      fragment replay <name> <run>
fragment new <dir> [--template T]        fragment triggers <name>
fragment new --list                      fragment pause|unpause <name> <op>
fragment create <name> [--visibility V] [--template T [--title T]] [--show-tokens]
fragment inbox <name> --payload JSON [--source S]   # the token from $FRAGMENT_INBOX_TOKEN, else stdin
fragment list                            fragment rotate <name> [--inbox] [--view]
fragment status <name>                   fragment visibility <name> [V]
fragment open <name>                     fragment members list|rm|leave ...
fragment events <name> [--since N | --tail N]
fragment members add <name> <email|npub> [--role viewer|editor]
fragment members role <name> <email|npub> viewer|editor
fragment manifest <name>                 fragment invite list|revoke ...
fragment access                          # who can open what you own
fragment sync <name> [--dir D] [--watch] [--mode M [--prune] | --live] [--apply-mass-delete]
fragment verify <name> [--dir D]         fragment secret list|rm ...
fragment secret set <name> <KEY> --to HOST [--to HOST]   # the value from $KEY, else stdin
fragment deploy <name> [--dir D] [--note N]
fragment write <name> <path> --text T | --from FILE|- [--message M]
fragment blob put <name> <file>          # prints its sha256: its pages read it at __blob/<sha256>
fragment drafts <name>                   fragment rollback <name> [--to <sha>]
fragment rm <name>                       fragment guide|skill
fragment ask <agent> <text> [--chat C] [--wait [S]] [--id ID]
fragment mcp <name> [--write]            # its described operations as an MCP server's tools, over stdio
fragment agent [<agent>] [--name N] [--picture FILE | --no-picture]
fragment operator key <file>
fragment operator wipe <email|npub> [--dry-run | --yes] [--key-file F]
fragment operator people [--q Q] [--after A] [--key-file F]
fragment operator person <npub> [--key-file F]
fragment operator pin <npub> <image> [--key-file F]
fragment operator orgs [--after A] [--key-file F]
fragment operator comp <email> [--kind seat|always-on] [--org O] [--key-file F]
fragment operator uncomp <seat> [--key-file F]
fragment operator trials [--name N [--days D] [--capacity C] | --send <id> --to <email>] [--key-file F]
fragment operator health [--key-file F]
fragment operator log [--before N] [--key-file F]
```

`fragment operator …` is the deployment's operators' alone: each signs
with the operator key's file, `--key-file F` (else
`FRAGMENT_OPERATOR_KEY_FILE`, else this machine's key), which
`fragment operator key <file>` makes.

Global flags: `--host <url>` (or `FRAGMENT_HOST`; else the host
`fragment login` or `fragment host <url>` saved), `--json`, `-v`. With `--json` (or
`FRAGMENT_OUTPUT=json`), stdout is exactly one line: `{"ok":true,
"data":…}` or `{"ok":false,"error":{"code","message","hint","id"}}`
(`id`: a failed `fragment call`'s); `fragment mcp`'s stdout is MCP's
alone, and a failure to start it goes to stderr. The codes are stable, and a host's
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
