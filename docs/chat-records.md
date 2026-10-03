# A chat's records

Status: **the contract phase 4's bridge writes and phase 5's chat
template reads** (docs/cloudflare-v1.md, decisions 8, 9, 38, 42). It
extends docs/api.md's "A chat's records"; where they differ, this file
wins. The bridge's code for it is `images/bridge/src/records.rs`.

A chat is a fragment with two channels and no app code:

```json
"channels": {
  "chat": { "read": "public", "post": "viewer" },
  "work": { "read": "viewer", "post": "editor" }
}
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
gets a turn of its own, from the bridge's counter.

One turn of an agent runs in a chat at a time; the rest wait, in order,
at most 16. A turn's records, in order:

1. `turn.start` on `work`, as the agent hands it to its runtime;
2. any number of `turn.step`, replies on `chat`, and `turn.prompt`
   (each `turn.prompt` followed by its `turn.prompt.closed`);
3. `turn.end` on `work`.

While a turn writes a reply, the chat's **draft** for it holds the
reply's whole text so far (`PUT …/channels/chat/draft {turn, text}`,
never stored; `text: null` stops it). A page shows a turn's draft after
the turn's last record; a reply record with the same `turn` replaces
it, and a new draft may follow (the next part).

## `chat`

**A person's message:**

```json
{ "text": "hi @juniper", "to": ["id:…"], "attachments": [ATTACHMENT] }
```

- `text`, at most 32 KiB (more is cut). `kind` absent, or `"message"`.
- `to`, optional: the agents it is for. The page fills it from the
  `@mentions` it resolved. Without `to`, an agent answers when the text
  `@mentions` its name or its fragment's label, else the **lead** (the
  first agent added to the chat, by `addedAt`) answers.
- An anonymous visitor's message (`anon:`) starts nothing, nor does one
  posted before the agent joined the chat (its membership's `addedAt`).
- A body that is a bare string is a message with that text.

**An agent's reply:**

```json
{ "text": "…", "turn": "<turn>", "attachments": [ATTACHMENT], "to": ["id:…"], "hop": 1 }
```

- Posted by the agent with the id `rp:<turn>:<n>`, its replies numbered
  from 1. A turn may reply more than once (text, a tool call, more text);
  each reply is whole.
- `to` and `hop` when the reply `@mentions` another agent of the chat:
  a hand-off. An agent answers another agent's reply only when its `to`
  names it, and only `hop` ≤ 3 (so two agents stop handing off).

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

## `work`

Posted by the agent, each with the id `wk:<turn>:<part>`, so a replayed
step posts the same record:

```json
{ "kind": "turn.start", "turn": "…", "asker": "id:…", "agent": "id:…",
  "cause": { "fragment": "…", "channel": "chat", "seq": 12 } }
```

Part `start`. `asker` may stop it; `agent` is the agent's identity.

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
`expiresAt` (ms). While every open turn of a computer waits on a person,
its computer may sleep (decision 42).

```json
{ "kind": "turn.prompt.closed", "turn": "…", "prompt": "<prompt>",
  "outcome": "answered", "option": "once", "by": "id:…" }
```

Part `pc:<prompt>`. `outcome` is `answered` (with `option` and `by`),
`expired` (no answer by `expiresAt`, or the computer restarted while it
waited: Hermes cannot resume a turn across a restart), or `stopped`.

```json
{ "kind": "turn.end", "turn": "…", "outcome": "idle" }
```

Part `end`. `outcome` is `idle` (it finished, with or without words),
`stopped`, or `error` with `error` (at most 300 characters): the
runtime failed, the agent stopped answering for 15 minutes, the turn
was lost in a restart, or it was refused (too many waiting).

## Attachments

```json
{ "sha256": "<64 lowercase hex>", "size": 12345, "type": "image/png", "name": "cat.png" }
```

A file is one of the chat fragment's blobs: its poster uploads it first
(`PUT /api/f/{chat}/blobs/{sha256}`, which takes editors), then posts
the record naming it; a page reads it at `__blob/<sha256>`. At most 8 a
record, 25 MiB each.

## `tasks` (an agent fragment's)

```json
{ "kind": "routine", "text": "Water the plants", "chat": "<chat fragment>" }
```

A routine (decision 38): the agent fragment's cron posts it, which wakes
the computer; the agent takes it as a turn in `chat`, asked by its
owner. One older than an hour when its computer first reads it is
skipped, as cron skips a missed run.

```json
{ "kind": "joined", "fragment": "<fragment>" }
```

Posted after adding the agent to a fragment: it wakes the computer,
which lists its agent's fragments again and follows the new chat at
once (the listing is otherwise read every 5 minutes while awake).
