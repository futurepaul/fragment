# A chat's records

Status: **the contract phase 4's bridge writes and phase 5's chat
template reads** (docs/cloudflare-v1.md, decisions 8, 9, 38, 42). The
bridge's code for it is `images/bridge/src/records.rs`. A person's export
(docs/api.md, Export) keeps them whole, and reads them as a transcript,
`transcript.md` (`fragment_core::export::transcript_part`: a message's
text and files, a turn's steps, prompts, notices and an error's end; the
debt ledger names it).

A chat is a fragment with two channels, and one job of the template's
own code, which runs from the platform's release as its page does
(decision 40: "Push", below):

```json
"channels": {
  "chat": { "read": "public", "post": "viewer" },
  "work": { "read": "viewer", "post": "editor", "from": "agent" }
},
"operations": { "notify_reply": { "kind": "job" } },
"triggers": [{ "channel": "chat", "from": "agent", "run": "notify_reply" }]
```

- `chat` holds what is said: people's messages, agents' replies, Stop,
  prompt answers, and an agent's owner's commands. Anyone who may post
  may say them.
- `work` holds an agent's progress: turns, steps, prompts, and the
  commands its runtime takes. Only the chat's agent members post there,
  each as itself (`"from": "agent"`: the platform refuses every other
  poster, an editor person included, and an agent acting for someone),
  so no person can forge a step, a card or a menu. The page counts a
  turn's records only from the agent whose `turn.start` began it, so
  one agent cannot write into another's turn either.

An agent fragment has one more channel its computer follows:

```json
"channels": { "tasks": { "read": "editor", "post": "editor" } }
```

Every body below is a record's `body`; the record carries `seq`, `at`,
and `principal` (who posted it). Ids are each poster's own (docs/api.md:
the same id and body again append nothing; another body is 409).

## Turns

A **turn** is what one agent does about one record. Its id is 24 hex of
SHA-256 of `<agent fragment>|<fragment>/<channel>/<seq>` (the record
that started it), so a record read twice (a catch-up, a restart, a
reconnect) is one turn, and two agents answering one message are two
turns. A message an agent's runtime sends on its own (no turn running)
gets a turn of its own, from the bridge's life and counter (`work`,
below), so a counter that a restored `/data` sent back names a new turn.

One turn of an agent runs in a chat at a time; the rest wait, in order,
at most 16. A turn's records, in order:

1. `turn.start` on `work`: its claim, posted before its runtime hears of
   it (`work`, below);
2. any number of `turn.step`, replies on `chat`, `turn.prompt` (each
   followed by its `turn.prompt.closed`), `turn.asked` and `turn.notice`
   (and a notice may follow the end: `turn.notice`, below);
3. `turn.end` on `work`.

Every turn has both its start and its end, one of each: a turn refused
(too many waiting) or stopped while it waited posts its `turn.start` and
then its `turn.end`, and runs nothing. The bridge keeps a turn until its
last records are answered, and a bridge that stops before then leaves
them to the next, which posts them again with the same ids and bodies
(docs/bridge.md, "State").

While a turn writes a reply, the chat's **draft** for it holds the
reply's whole text so far (`PUT …/channels/chat/draft {turn, text}`,
never stored; `text: null` stops it). A page shows a turn's draft after
the turn's last record; a reply record with the same `turn` replaces
it, and a new draft may follow (the next part).

## `chat`

**A person's message:**

```json
{ "text": "hi @juniper", "to": ["npub1…"], "attachments": [ATTACHMENT], "reply_to": 12 }
```

- `text`, at most 32 KiB (more is cut). `kind` absent, or `"message"`.
- `reply_to`, optional: the seq on `chat` of the message it quotes (a
  person's reply to it). The agent's runtime is handed the quoted
  message's text (its first 4 KiB; a command as typed; a file without
  words, its name), read from the chat as the turn starts, and whether it
  is the agent's own; one that is not there, or is no message, quotes
  nothing. Anything but a positive integer makes the body no message.
- `to`, optional: the agents it is for. The page fills it from the
  `@mentions` it resolved (an agent of its owner's not in the chat yet is
  added first: "The page", below). Without `to`, an agent answers when the
  text `@mentions` its name or its fragment's label, else the **lead**
  (the first agent added to the chat, by `addedAt`) answers.
- An anonymous visitor's message (`anon:`) starts nothing, nor does one
  posted before the agent joined the chat (its membership's `addedAt`).
- While an agent's turn asks its asker something to answer in words (the
  question is one of its replies), the asker's next message to it is that
  answer: it goes to the running turn and starts no turn of its own. A
  turn a restart ended asks nothing, so the message is then a turn of its
  own (docs/bridge.md, "State").
- A body that is a bare string is a message with that text.

**An agent's reply:**

```json
{ "text": "…", "turn": "<turn>", "attachments": [ATTACHMENT], "to": ["npub1…"], "hop": 1 }
```

- Posted by the agent with the id `rp:<turn>:<n>`, its replies numbered
  from 1. A turn may reply more than once (text, a tool call, more text);
  each reply is whole.
- `to` and `hop` when the reply `@mentions` another agent of the chat:
  a hand-off. An agent answers another agent's record (a reply, or a
  message an agent posts itself: `fragment ask`, the API) only when its
  `to` names it, and only at most 3 hops deep (`HOPS_MAX`), so two agents
  stop handing off.

**The hop is the answering bridge's to count**, never the poster's to
say (images/bridge, `engine.rs` `hop_of`). `hop` in a body only ever
raises it. For an agent of the answering bridge's own computer (a
person's agents all run on one: decision 13) the bridge knows the turns
it runs, so a record that agent posts is one hop past the turn it is in:
its turn running in that chat, or the turn there its `turn` names when that
ended within 5 minutes (its reply, read just after the turn was let go);
and its deepest turn running in another chat (a `fragment ask` from one
chat into another), the deeper of the two; in no turn at all, the last hop
allowed (answered once, its answer handing on nothing). A post
made around the bridge, with the CLI or the API and no `hop`, so counts as
a reply would; a `hop: 0` resets nothing. Another computer's agent is held
to the hop it claims, at least 1. This is the image's rule, on ordinary
records: the platform holds no chat record (docs/cloudflare-v1.md, the
rule).

**A chat's budget.** The agents of a chat start at most 20 turns of each
other in 5 minutes (`AGENT_TURNS_PER_CHAT_MAX`, `AGENT_TURNS_WINDOW_MS`),
counted by the causing records' own times and kept in each bridge's
state, so a restart spends none of it again. A hand-off past it is
refused: its `turn.start`, then its `turn.end` with `error` saying so
("agents in this chat started 20 turns of each other in 5 minutes, the
most they may; ask again in a few minutes"), which the page shows as the
agent could not finish. A person's message is never counted, nor refused.

**Stop:**

```json
{ "kind": "stop", "turn": "<turn>" }
```

Only the turn's asker stops it (the person whose message started it; for
a routine, the agent's owner). Without `turn`, the asker's running turn.
From anyone else it is ignored. A waiting turn named by Stop never runs.

**A prompt's answer:**

```json
{ "kind": "prompt_response", "prompt": "<prompt>", "option": "<option>", "text": "purple" }
```

Only the agent's owner answers its prompts (decision 42); anyone else's
is ignored. The first answer on the channel wins; later ones are
ignored. A page posts it with the id `pr:<prompt>`, so a second tap is
the same record (or 409, for another option). `text`, only for an option
answered in words (`words`, below): what was typed, trimmed, at most 32
KiB (more is cut); with any other option it is ignored, and without it
such an option is answered as a question in words is (its runtime asks
for them).

**A command** (for an agent's runtime, from the agent's owner):

```json
{ "kind": "command", "command": "steer", "args": "use the blue one", "to": ["npub1…"] }
```

- `command`, `^[a-z0-9_-]{1,32}$`: one of the agent's runtime's commands,
  its menu (`commands` on `work`, below). `args`, optional: its words,
  trimmed, at most 32 KiB (more is cut). `to` as a message's; without it,
  the lead. Another shape (a `command` that is not one, `args` not a
  string, a `to` of no identities) is no command, and starts nothing.
- Only the agent's owner commands it: anyone else's is passed over, as is
  a command not on its menu. Nothing outside the menu reaches a runtime
  as a command, and a message whose text starts with `/` is words, never
  one (the bridge keeps it from reading as one).
- How each is carried is its menu's (images/bridge, `runtime::How`):
  - most are a **turn of their own**, waiting their turn as a message
    does: the runtime is handed `/<command> <args>` as it is, and what it
    answers is the turn's reply;
  - `stop` stops the agent's running turn in the chat, whoever asked it,
    and ends its waiting turns there (each its `turn.start`, then its
    `turn.end` `stopped`; they never run); no turn of its own;
  - `new` stops as `stop` does, then is a turn of its own: a new session
    for this chat, the runtime's (Hermes keeps one per chat; the chat's
    records stay);
  - `queue`'s words are a message, a turn like any;
  - `steer`'s words go into the agent's turn running in the chat at once,
    no turn of their own and no record of the bridge's; with none
    running, they are a message;
  - `btw` is said beside whatever runs, no turn of its own;
  - what the runtime answers a `steer` or a `btw` with is a message of
    its own (a turn of the runtime's own, `work` below);
  - `queue`, `steer` and `btw` with no words are each a turn of their
    own (the runtime says how to use it).
- A command its menu refuses for a word of its `args` (`--global`, for
  Hermes' `model`: images/bridge, `relay/menu.rs`) gets both its records,
  its `turn.end` an `error` saying why; the runtime never hears it.
- Read once, as every record is (the bridge's cursor): one read again
  after a restart starts nothing twice; one waiting its turn waits again
  in the next life, still a command.

Any other `kind` on `chat` is the page's own and is never a message.

**Search.** A person's message and an agent's reply are what the
shell's search finds (docs/api.md, The shell, Search): a body whose
`kind` is absent or `"message"`, by its `text` alone (its first 4 KiB).
No other record here is searched, Stop, prompt answers, commands and
everything on `work` included.

## `work`

Posted by the agent, each with the id `wk:<turn>:<part>`, so a replayed
step posts the same record:

```json
{ "kind": "turn.start", "turn": "…", "asker": "npub1…", "agent": "npub1…",
  "cause": { "fragment": "…", "channel": "chat", "seq": 12 },
  "life": "<32 lowercase hex>" }
```

Part `start`. `asker` may stop it; `agent` is the agent's identity;
`life` is the bridge process that claimed it: 128 random bits each
bridge makes as it starts and never writes to `/data`. The record is the
turn's claim, "this life runs this turn": the runtime is given the turn
only once the platform answers it as this life's (appended, or a replay
of this life's own retry). A 409 is another life's claim of the turn, so
this life never runs it, posts its `turn.end` as lost (a 409 too when
that life ended it), and forgets it; no answer runs nothing, and the turn
is claimed again. An agent that may not post on `work` (its owner holds it
below editor) claims nothing, so it runs no turn in the chat: its turn is
dropped, with no record. So a turn runs in one life at most, whatever `/data` a
computer wakes with, an older one or none: a turn whose life ended before
it did is lost, and said so, never run twice. A turn is claimed only
while its runtime can take it, and never while the platform holds the
computer (docs/computers.md, the hold).

```json
{ "kind": "turn.step", "turn": "…", "step": 1, "tool": "terminal",
  "category": "shell", "args": "ls -la", "ok": true, "excerpt": "…", "text": "…" }
```

Part `<step>`, from 1: one tool call. `tool` (the runtime's own name for
it) and `args` (the call's preview) at most 140 characters, `excerpt`
(its result) and `text` (the model's words before it) at most 300; empty
when the runtime does not say. At most 200 a turn. `category` is what it
does, whatever the runtime calls the tool: `shell` (a command or code
run), `web` (a search of the web), `read` (a file, page or skill read, or
files searched), `write` (a file written or edited), `browser` (its
browser or desktop driven), `image` (an image made), `delegate` (work
handed to another agent), `memory` (what it remembers, changed), or
`other`; a page shows each with its own icon and verb ("Running: ls -la",
"Searching the web: …"), and a step with another or none (one from before
steps said) as `other`, by its `tool`. The bridge's runtime says which
(our Hermes image's: one table of Hermes' tools, docs/bridge.md).

```json
{ "kind": "turn.prompt", "turn": "…", "prompt": "<prompt>",
  "text": "Make your recipe app public, so anyone with its link can open it?",
  "options": [ { "id": "c0", "label": "Yes, make it public", "style": "primary" },
               { "id": "c1", "label": "No, keep it private" },
               { "id": "other", "label": "Something else", "words": true } ],
  "asks": "id:<owner>", "expiresAt": 1791000000000 }
```

Part `p:<prompt>`: a card with buttons, a question its runtime asks in
plain words (our Hermes image asks none about a command, a file or code:
docs/computers.md, "Approvals in our Hermes image"). `prompt` and option ids are
`^[A-Za-z0-9._-]{1,64}$` (options `{1,32}`), 1 to 8 options, `style`
optional (`primary`, `danger`). An option with `"words": true` is
answered in words (a clarify's "Other": docs/bridge.md, Relay): a field
on the card, whose answer carries what was typed (`text`, above), so
nothing asks for it after. Only `asks` may answer, until
`expiresAt` (ms). An open card keeps its computer awake until it is
answered or expires (docs/bridge.md, "A card keeps its computer awake"),
so it expires with its runtime there, and its turn ends as the runtime
ends it.

```json
{ "kind": "turn.prompt.closed", "turn": "…", "prompt": "<prompt>",
  "outcome": "answered", "option": "c0", "by": "npub1…" }
```

Part `pc:<prompt>`. `outcome` is `answered` (with `option` and `by`, and
`text` for an option answered in words),
`expired` (no answer by `expiresAt`, or the computer restarted while it
waited, as an owner's sleep or a crash does: Hermes cannot resume a turn
across a restart, and the agent's next turn is told the card was cut
with it), or `stopped`.

```json
{ "kind": "turn.asked", "turn": "…", "asks": "npub1…" }
```

Part `q<part>`: the turn asks `asks` (its asker) something to answer in
words, the question being its reply part `<part>`, and waits, running,
for their next message to the agent in the chat, which is the answer
(above, "A person's message"). Posted once a question, after the reply
that asks it; a runtime saying it asks again before an answer posts
nothing more. Nothing records the answer on `work`: a page takes the
turn as waiting on `asks` until `asks` posts a message on `chat` (no
`turn`) at or after the record's `at`, or a later record of the turn's
on `work` (a step, a card, a notice, its end) says it went on. A
`turn.asked` without `asks` as a string is not one.

```json
{ "kind": "turn.notice", "turn": "…", "category": "memory",
  "text": "Self-improvement review: Memory updated" }
```

Part `n<k>`, from 1: a notice of the agent's runtime, neither a step nor
a reply (it seems stuck, its context was compressed, an approval's
acknowledgment, it updated its memory). `category` is what it is about:
`info`, `warning`, or `memory` (a page shows any other as `info`); `text`
at most 300 characters. At most 32 a turn. A notice the runtime sends
after the turn's end (its review of the turn, once the turn is done) is
still that turn's, posted after its `turn.end`: the turn of the agent's
the bridge last ran in that chat, in its life. With none (the agent's
last turn there ran in an earlier life), a notice belongs to no turn and
is not posted. A page shows each as a quiet line, never in a card of
steps nor as the agent's words, and never pushes it.

```json
{ "kind": "turn.end", "turn": "…", "outcome": "idle" }
```

Part `end`. `outcome` is `idle` (it finished, with or without words),
`stopped`, or `error` with `error` (at most 300 characters): the
runtime failed, the agent stopped answering for 15 minutes, the turn
was lost when its computer restarted (the next life ends every turn an
earlier life claimed and did not finish, `lost when the computer
restarted`, and never runs it again), or it was refused (too many
waiting).

```json
{ "kind": "commands", "agent": "npub1…",
  "commands": [ { "name": "usage", "description": "Show token usage for this session" },
                { "name": "steer", "description": "Inject a message after the next tool call (no interrupt)",
                  "args": "What to tell the agent" } ] }
```

The agent's **menu**: the commands its runtime takes from its owner
(above, "A command"), each its name, what it does, and what its words
are (`args`, absent for one that takes none). Posted by the agent, as
itself, with the id `cm:<life>`, as it first runs a turn in the chat in
a life: once a life in a chat (posted again, a replay), and again in the
next life, so the page's window of `work` holds it while the chat is used.
The page takes an agent's latest, and only one whose `agent` is its poster.
A runtime with no commands posts none.

**After a lost turn.** The agent's next turn in that chat is told what
was cut, once (docs/durable-computers.md, P5; docs/bridge.md, "The turn
after a cut one is told"). The note is built from these records alone,
so it is the same in every life: on `work`, the agent's latest
`turn.start` before the new turn's own (a refusal's passed over), whose
`turn.end` is `lost when the computer restarted`, with that turn's
`turn.step`s and `turn.prompt`s (and how they closed); on `chat` (or the
agent's `tasks`, for a routine), its cause's text and the replies it had
posted (`rp:<turn>:<n>`). Nothing records the note itself: at the turn
after, the turn before is the noted one, which did not end as lost.

**After a rollback.** Turns of the agent's that another life ran after the
save its computer went back to (their `turn.start` names that life, so the
claim of a life reading them again is a 409) are in no memory of its
runtime. Its next turn in the chat is told what each was asked and replied,
from these records too (docs/bridge.md, "What a rollback forgot").

## Attachments

```json
{ "sha256": "<64 lowercase hex>", "size": 12345, "type": "image/png", "name": "cat.png" }
```

A file is one of the chat fragment's blobs: its poster uploads it first
(`PUT /api/f/{chat}/blobs/{sha256}`, which takes editors; a page, `PUT
__blob/<sha256>` through `fragment.blob(file)`), then posts the record
naming it; a page reads it at `__blob/<sha256>`. At most 8 a record, 25
MiB each. The record keeps the file while the channel keeps the record
(docs/api.md, Blobs); an upload no record names goes after the grace
period.

**A voice memo** is an attachment whose `type` is audio. The page records
one (MediaRecorder) and sends it as the recorder made it: `audio/webm`
(Chrome and Firefox, Opus), `audio/ogg` (Opus), or `audio/mp4` (Safari,
AAC), named `voice-memo.webm`, `.ogg` or `.m4a`, at most 5 minutes and 25
MiB, with the message's text beside it or none. A page shows an
attachment of type `audio/webm`, `audio/ogg`, `audio/mp4`, `audio/mpeg`
or `audio/wav` as a player, in a person's message and in an agent's reply
alike (`__blob` serves those types as themselves). The platform does
nothing with the audio: an agent's runtime hears it, and transcribes it
on its own side if it does.

## The page (`templates/chat`)

**On a phone, an app's page.** The page never scrolls, bounces or
zooms, framed or on its own: its html and body are pinned
(`overscroll-behavior: none`) and the messages scroll, holding their own
overscroll. It is as tall as the visual viewport, from where it is (its
`resize` and `scroll`), so the composer sits just above a phone's keyboard
(Safari shrinks and pans only that viewport); framed, the shell does the
same for the frame. Its fields are 16px or more on a phone's width (no
zoom as one takes focus), its buttons neither selected nor held; a table's
cells keep their words whole, and a wide table or a long line of code
scrolls in its own box, never the page.

**A phone's return is a newline** (Paul, 2026-10-14). On a touch screen
with no finer pointer beside it (`(pointer: coarse) and (not (any-pointer:
fine))`: no mouse or trackpad, so most likely no keyboard), the
composer's return key adds a line, labelled so (`enterkeyhint="enter"`),
and only the send button sends (or ⌘ or Ctrl+Enter, from a keyboard after
all). Anywhere else Enter sends (`enterkeyhint="send"`) and Shift+Enter
adds a line. A field of one line (a card's words, the shell's launcher)
sends on Enter everywhere. The bar Safari puts above an iPhone's keyboard
(its up and down arrows and its ✓) is not the page's: WebKit shows it for
every text field on a phone (`-[WKContentView requiresAccessoryView]`),
and only an app embedding WebKit may hide it, so no attribute, form or
field of a page removes it.

**Sending, then sent.** A message or command sent from the page shows
at once as **sending**: its bubble dimmed, "Sending…" where its time
goes, no Reply, never queued. It is sent once the platform answers with
its record (or the socket brings the record first: the same body from
the same person ends its sending), and then shows as any message does;
one the platform refused, or that never reached it, shows its error and
Retry (the same id: one record however often it is tried). Nothing looks
sent before the platform has it. As it shows, it rises straight up from
the composer into the thread at its final horizontal position (no
sideways movement, no scaling): a visual copy crosses the scroll
boundary, as dim as the bubble is, while the real message keeps its
layout and accessibility; its record replacing it on the way keeps the
same entrance. Incoming messages, history and failed sends do not
replay it; reduced motion skips it; scrolling or resizing ends it.

**The first run.** The shell opens its person's first chat before their
first agent is in it (`?setup=1`; docs/api.md, "The first run, in its
chat"). The page then shows a card above the chat, "Setting up your
agent", with the steps the shell sends with its roster (`setup: {steps:
[{text, state}], error}`, at most 6 steps of 120 characters, `state`
one of `done`, `doing`, `todo`, `failed`): what the platform says of
the making, never a message, and nothing in the agent's name. The
composer takes typing at once; Send (and Enter) wait until the page's
own member list has an agent, so nothing is posted before the agent
joined (a message from before then starts nothing: above). When the
shell says a step changed, the page reads `__members` again. A failed
step says why, with Try again (`{fragment: "retry-setup"}` to the
shell), in the same frame, keeping what was typed. When the shell sends
`setup: null` the card goes.

The chat template's page reads and writes only these records, and
attributes each by the poster the platform stamps on it
(`record.principal`), never by what a body says: a turn is its agent's
from that agent's own `turn.start`, its later records count only from
that agent, and records naming it from anyone else (or from before its
start is read) show nothing. It
follows `chat` from its last 400 records and, for a viewer, `work` from
its last 1000 (each agent's menu among them), and lays them out in time:
a person's message (or command); an
agent's consecutive steps as one card (each its category's icon, verb and
arguments; while it works, the card's head says what it does now); a
prompt as a card whose buttons
only `asks` may press (enabled for them alone, and only their answer
marks it sent: anyone else's changes nothing on it), an option answered in
words a field there that Enter sends, then how it closed (the words, for
one answered in words); a
reply; a runtime's notice, quietly, its category's icon beside its
words; and a turn's end when it was not `idle` (Stopped, or the error,
quietly). A turn's draft (its agent's own: a draft
naming another poster's turn shows nothing) shows after the turn's last
record, and while
a turn runs with no draft nor open card, a working line does: "Waiting
for your answer" in its place while the turn waits on the page's person's
words (`turn.asked`), its composer saying what they type answers it
("Your answer for Juniper"), or whose answer it waits for, to anyone
else.

Commentary uses the replies' safe markdown, smaller and muted; a long note
folds at a whole block or sentence and expands on request. A step's line
has a check for `ok: true`, an error mark for `ok: false`, or a spinner
without a result; an `excerpt` shows below it, with failures in red. The
posting agent's own `/data/work/<agent>/home/` shows as `~/`, its workspace
as `work/`, and `/data/hermes/managed-skills/` as `skills/`; the full
arguments stay in a title on their text. Paths wrap to two lines before
directories are shortened in the middle, keeping the filename. These
rules also apply to steps without a category.

Load earlier messages reads older pages of both channels, keeping the
visible message in place and the live cursor at its end. When scrolled
up, Jump to latest counts new messages and returns to the end. Replies
use DOM-only markdown (strikethrough, tasks, nested lists, tables with
their own horizontal scroll, autolinks, quotes, headings, rules, and
fenced code with a language label and Copy). Share reply uses the native
share sheet where available, otherwise copies the reply. Cancelling a
share sheet copies nothing. Video and PDFs show inline beside open,
download and share actions; PDFs retain those actions when a browser
cannot embed them.

A channel post that fails has its error and Retry on the message itself.
Retry keeps its id and body so a lost answer cannot write it twice. Until
a turn starts, the waiting line uses the shell roster's optional `phase`:
`asleep` or `starting` says the agent is starting up, `sleeping` says its
computer is going to sleep, and `wont_wake` says it could not start.
For `awake` and views without a phase, the line uses presence and waits
plainly. After 90 seconds without a turn or a known startup state it
says the agent has not started a reply yet; a later `turn.start` replaces
the line. A known startup stays explained until then. No transport
state is persisted as a chat record. It posts:

- a message with a fresh id of its own (`crypto.randomUUID()`), the same
  id again only for the same body sent again after a failure; its `to`
  the agents its `@mentions` name, when they name any (an owner's agent
  not in the chat is added first: "Your other agents", below); its files
  uploaded first, at most 8 of at most 25 MiB each, and its text cut to
  32 KiB; `reply_to`, the message its person chose Reply on (a person's
  or an agent's), which the composer shows above what is typed (who, and
  its first words; the x lets it go), and the message sent above its
  bubble (a click shows the one it quotes);
- a command, `{kind: "command", command, args?, to}`, with a fresh id as
  a message's: text typed whole as `/<command> <words>` by the agent's
  owner (`__members` names an agent's `owner`), when `<command>` is on
  that agent's menu, sent to the agent working for them, else the lead.
  For its owner, `/` at the start of the composer lists the agent's
  commands whose names start with what follows, each with what it does
  and what its words are; Tab or Enter writes the one picked, and Enter
  on one typed whole that takes no words sends it. Anyone else's `/` is
  words: a message. The page shows a command as typed;
- while an agent's turn runs, a message to it waits its turn: the
  composer says so (`Juniper is working: a message waits its turn`), and
  the message is marked queued until its turn starts (no `turn.start`
  names it while a turn of its agent that started before it runs; one
  sent to the turn's asker just after the turn's own reply is taken for
  that turn's answer, unmarked). For the agent's owner, when its menu has
  `steer`, words typed then offer Steer beside Send: the words as
  `/steer`, into the running turn now;
- Stop, `{kind: "stop", turn}` with the id `stop:<turn>`, from the turn's
  asker while it runs (the send circle is Stop then, until they type: then
  it sends);
- a prompt's answer with the id `pr:<prompt>`, with `text` for an option
  answered in words;
- a voice memo (Attachments, above): the composer's mic records until it
  is pressed again, or Send, or 5 minutes pass, then sends the clip as
  the message's audio file with whatever was typed (the x lets it go).
  The mic is enabled for editors alone, as uploads are; while it records,
  a quiet clock shows beside it, and a microphone the browser refused, or
  a recorder that failed, says so in the page's banner.

Its person's presence on the fragment is `{typing, looking}`: `typing`
while they write, and `looking` while the chat is on screen
(`document.visibilityState`), which keeps their agents' replies from
being pushed to them ("Push", below). For a person signed in who may
post, a bell beside Send is "Notify me": from a click only it subscribes
this browser for them (`fragment.push.register(<their identity>)`), and
says whether it is on (a click again turns it off), blocked in the
browser's settings, or failed. A chat framed by the shell is a
cross-origin frame, which a browser does not let ask for notifications:
there the bell says to open the chat in a tab of its own, and does.

Who is who comes from the fragment: `__members` lists the chat's agents
(the lead first), and `__people` names them. An agent shows by its agent
fragment's title and picture (docs/api.md, "An agent's name and
picture"): its replies' byline, its face, the header, the `@` list and
the waiting line. Its `name` is its agent fragment's label, which
`@mentions` it, whatever it is called. Framed by its owner's shell, the
roster's `title` and `picture` come first, so a rename shows as the
shell's list says it; anywhere else, an agent's profile is asked again
as each of its turns ends (it renames itself in a turn). An agent with
no picture shows its image in its color, one of six, chosen by its
identity; the chat takes its lead's color. The shell that frames it may send `postMessage({fragment:
"theme", mode: "light"|"dark", mobile: boolean})`; otherwise it follows
`prefers-color-scheme`. The optional `mobile` flag follows the shell's 760px
breakpoint, so a narrow desktop chat does not pick up the phone's rounded
composer and larger input text. Mobile inputs use 16px text to avoid focus
zoom; messages remain 14px. The shell reserves the home-indicator safe area,
and the mobile composer adds bottom clearance.

**A way to ask for an app, in the composer.** The shell's Add (and its
empty desktop) offers ways to ask the person's agent for an app; one
pressed comes to their chat's page as `{fragment: "compose", text}`
(docs/api.md, The shell). The page takes it only from its parent at the
platform's origin (its hello's `platform`), from someone who may post
here, and at most `TEXT_MAX_BYTES`: it puts the words in the composer in
place of what was typed and of any quote, selects their first `…` (the
part to make theirs) and focuses it. It sends nothing; the person does.

**Desktop launcher.** Command K (or Control K) opens the shell's shared
search/new-chat input, including while typing in the framed chat. The chat
sends `{fragment: "open-launcher"}` to its parent. The shell accepts it
only from its focused, listed fragment iframe with the expected origin.
Standalone chats leave the shortcut to the browser.

**Your other agents.** Framed, the page asks the shell for its person's
agents as it mounts (`{fragment: "agents?"}`). A shell answers only a
frame of a fragment its person owns (docs/api.md, The shell), so in its
owner's chat `@` lists the chat's agents (the lead first), then the
owner's others, each marked "adds them to this chat"; a guest's page, one
someone else owns, and one opened in a tab of its own (no shell) list the
chat's agents alone. A message whose `@mentions` name one of the others
asks the shell to add it first (`{fragment: "add-agent", identity,
nonce}`): the shell asks its person in its own dialog ("Add Fred to
<chat>?"), and on their Add adds it as an editor, as making a chat does,
and answers `agent-added`. The page then reads the members again, and
only then posts the message, its `to` naming it: the agent is a member
before the message, so its bridge takes it (a message from before an
agent joined starts nothing). So the first message to an agent new to the
chat costs one click. An add declined, refused, or unanswered for 120 s
sends nothing and says why in the banner, the message kept in the
composer.

**An app beside the chat.** Framed, a link in an agent's reply to a page
of another fragment of the deployment (its host the page's own with
another fragment's name in front: an app the agent made) is followed by
"Open", which asks the shell to open that page in its viewer beside the
chat (`{fragment: "open", url}`; the shell checks the URL again:
docs/api.md, The shell). The link itself still opens a tab, and nothing
is offered on a page with no shell around it.

## Push

The template's code (`templates/chat/app.mjs`) pushes an agent's reply to
the chat's people who are away. Its trigger starts a run for each record
an agent of the chat posts on `chat` (`"from": "agent"`: its replies; a
draft is never a record, and a person's message starts nothing). The run,
a job acting as the chat itself:

1. reads the chat's members (`job.members()`): the poster must be an
   agent still, and the chat's people are its members of kind `person`;
2. leaves out each person a page of the chat shares `{looking: true}`
   for (`job.presence()`);
3. names the agent (`job.people`: its agent fragment's title and
   picture, as every page shows it; with no title, its label,
   capitalized);
4. pushes `{title: <the agent's name>, body: <the reply's first 120
   characters, on one line; one without words says what it carries>, tag:
   <the chat's name>, url: "./", icon?: <its picture>}` to each of the
   rest, at most 32, by
   their identity (`job.push(<identity>, …)`). A subscription's `who` that
   is an identity is that identity's own (docs/api.md, Deliveries), so a
   reply reaches its people's browsers, never an agent's.

A member who calls `notify_reply` itself pushes nothing (`job.via` is not
`channel`). A turn that replies more than once pushes each reply under
the chat's tag, so a browser shows the latest. `looking` is the page's
own visibility, so a chat open in a tab of the shell that is not shown
counts as looked at, and a chat whose agents reply more than 120 times
in an hour pauses its push (the debt ledger has both).

## `tasks` (an agent fragment's)

```json
{ "kind": "routine", "text": "Water the plants", "chat": "<chat fragment>" }
```

A routine (decision 38): the agent fragment's cron posts it, which wakes
the computer; the agent takes it as a turn in `chat`, asked by its
owner. One older than an hour when its computer first reads it is
skipped, as cron skips a missed run.

**The agent's hello.** The shell posts one as its owner, with the id
`hello`, once it has made an agent and added it to its chat (the first
run's default agent too): asked to say hello there, who it is and what
it will do (its job is its `SOUL.md`), it speaks first, and the chat
holds no message of its person's before it (cell/shell/shell.js,
`hello`).

Only the agent fragment itself (its cron, the platform's `joined`: a
principal that is no identity) and the agent's owner ask anything on
`tasks`. Any other poster's record is passed over, whatever it says:
another of the owner's agents may post here (it acts for the owner, an
editor), but a routine it posted would start a turn asked as the owner
and at no hop, a hand-off no count would hold. An agent asks another in
a chat instead (`fragment ask`).

```json
{ "kind": "joined", "fragment": "<fragment>" }
```

Posted by the platform, as the agent fragment itself (its own key),
when the agent is added as a member of `fragment` (docs/computers.md,
"An agent added to a fragment wakes its computer"): once for that
membership, and the platform wakes the computer too. The guest lists its
agent's fragments again and follows the new chat at once (the listing is
otherwise read every 5 minutes while awake). No template, page or shell
posts it: whatever adds an agent to a fragment needs to do nothing more.
