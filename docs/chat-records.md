# A chat's records

Status: **the contract phase 4's bridge writes and phase 5's chat
template reads** (docs/cloudflare-v1.md, decisions 8, 9, 38, 42). The
bridge's code for it is `images/bridge/src/records.rs`.

A chat is a fragment with two channels, and one job of the template's
own code, which runs from the platform's release as its page does
(decision 40: "Push", below):

```json
"channels": {
  "chat": { "read": "public", "post": "viewer" },
  "work": { "read": "viewer", "post": "editor" }
},
"operations": { "notify_reply": { "kind": "job" } },
"triggers": [{ "channel": "chat", "from": "agent", "run": "notify_reply" }]
```

- `chat` holds what is said: people's messages, agents' replies, Stop,
  and prompt answers. Anyone who may post may say them.
- `work` holds an agent's progress: turns, steps, prompts. Only editors
  post there (the chat's agents are editors), so no person can forge a
  step or a card.

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
2. any number of `turn.step`, replies on `chat`, and `turn.prompt`
   (each `turn.prompt` followed by its `turn.prompt.closed`);
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
{ "text": "hi @juniper", "to": ["npub1…"], "attachments": [ATTACHMENT] }
```

- `text`, at most 32 KiB (more is cut). `kind` absent, or `"message"`.
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
{ "kind": "prompt_response", "prompt": "<prompt>", "option": "<option>" }
```

Only the agent's owner answers its prompts (decision 42); anyone else's
is ignored. The first answer on the channel wins; later ones are
ignored. A page posts it with the id `pr:<prompt>`, so a second tap is
the same record (or 409, for another option).

Any other `kind` on `chat` is the page's own and is never a message.

**Search.** A person's message and an agent's reply are what the
shell's search finds (docs/api.md, The shell, Search): a body whose
`kind` is absent or `"message"`, by its `text` alone (its first 4 KiB).
No other record here is searched, Stop, prompt answers and everything on
`work` included.

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
  "args": "`ls -la`", "ok": true, "excerpt": "…", "text": "…" }
```

Part `<step>`, from 1: one tool call. `tool` and `args` at most 140
characters, `excerpt` (its result) and `text` (the model's words before
it) at most 300; empty when the runtime does not say. At most 200 a
turn.

```json
{ "kind": "turn.prompt", "turn": "…", "prompt": "<prompt>", "text": "Run `rm -rf x`?",
  "options": [ { "id": "once", "label": "Allow once", "style": "primary" },
               { "id": "deny", "label": "Deny", "style": "danger" } ],
  "asks": "id:<owner>", "expiresAt": 1791000000000 }
```

Part `p:<prompt>`: a card with buttons. `prompt` and option ids are
`^[A-Za-z0-9._-]{1,64}$` (options `{1,32}`), 1 to 8 options, `style`
optional (`primary`, `danger`). Only `asks` may answer, until
`expiresAt` (ms). An open card keeps its computer awake until it is
answered or expires (docs/bridge.md, "A card keeps its computer awake"),
so it expires with its runtime there, and its turn ends as the runtime
ends it.

```json
{ "kind": "turn.prompt.closed", "turn": "…", "prompt": "<prompt>",
  "outcome": "answered", "option": "once", "by": "npub1…" }
```

Part `pc:<prompt>`. `outcome` is `answered` (with `option` and `by`),
`expired` (no answer by `expiresAt`, or the computer restarted while it
waited, as an owner's sleep or a crash does: Hermes cannot resume a turn
across a restart, and the agent's next turn is told the card was cut
with it), or `stopped`.

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

The chat template's page reads and writes only these records. It
follows `chat` from its last 400 records and, for a viewer, `work` from
its last 1000, and lays them out in time: a person's message; an
agent's consecutive steps as one card; a prompt as a card whose buttons
only `asks` may press (enabled for them alone), then how it closed; a
reply; and a turn's end when it was not `idle` (Stopped, or the error,
quietly). A turn's draft shows after the turn's last record, and while
a turn runs with no draft nor open card, a working line does. It posts:

- a message with a fresh id of its own (`crypto.randomUUID()`), the same
  id again only for the same body sent again after a failure; its `to`
  the agents its `@mentions` name, when they name any (an owner's agent
  not in the chat is added first: "Your other agents", below); its files
  uploaded first, at most 8 of at most 25 MiB each, and its text cut to
  32 KiB;
- Stop, `{kind: "stop", turn}` with the id `stop:<turn>`, from the turn's
  asker while it runs (the send circle is Stop then);
- a prompt's answer with the id `pr:<prompt>`;
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
(the lead first), and `__people` names them (an agent's `name` is its
agent fragment's label, which `@mentions` it). An agent's color is one
of six, chosen by its identity until agents carry one; the chat takes
its lead's. The shell that frames it may send `postMessage({fragment:
"theme", mode: "light"|"dark"})`; otherwise it follows
`prefers-color-scheme`.

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
3. names the agent (`job.people`: its fragment's label, capitalized);
4. pushes `{title: <the agent's name>, body: <the reply's first 120
   characters, on one line; one without words says what it carries>, tag:
   <the chat's name>, url: "./"}` to each of the rest, at most 32, by
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
