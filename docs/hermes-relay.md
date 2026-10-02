# Hermes' Relay, with the platform its connector

How a fragment's Hermes answers its chats (docs/one-home.md, phase 2).
Hermes' gateway has a generic connector protocol, Relay: it dials out
to a connector over a WebSocket, and the connector hands it messages
with who wrote them and turns its sends and edits into its own
platform's. Here each Hermes' cell is its connector (`cell/src/relay.rs`;
its pure parts in `crates/core/src/relay.rs`).

Pinned to Hermes v0.21.5 (tag `v2026.9.24`), contract version 1, read
from its code (`gateway/relay/`), which wins where its contract document
(`relay-connector-contract.md`) disagrees. Relay is marked experimental:
its contract may change without a deprecation cycle until it has been
proven on Discord and Telegram, so a Hermes upgrade re-reads it.

## The dial

- Hermes is told, in its computer's spec: `GATEWAY_RELAY_URL`
  (`<platform>/api/hermes/<fragment>`; it appends `/relay`),
  `GATEWAY_RELAY_ID` (the fragment's name), `GATEWAY_RELAY_SECRET` (the
  cell's, sealed there; in the guest by decision 3 of docs/one-home.md),
  `RELAY_HOME_CHANNEL`, and `HERMES_GATEWAY_BUSY_INPUT_MODE=queue`. With
  the id and secret pinned it provisions nothing; the URL turns its other
  messaging platforms off.
- Its token: `Authorization: Bearer base64url_unpadded("{id}:{exp}:{hex(HMAC-SHA256(key = secret's UTF-8 bytes, msg = "{id}:{exp}"))}")`,
  `exp` five minutes on, fresh each dial. The cell allows two minutes past
  `exp` (the guest's clock), and closes a refused dial 4401: `expired`
  (it dials again) or `unauthorized` (a second after a handshake stops
  it until it restarts).
- Frames are JSON objects, one per line, each ending with `\n`, under
  1 MiB. Keepalive is WebSocket pings, from Hermes.

## Frames

- Hermes: `hello` (one each dial) → the cell: `descriptor` (nine keys
  Hermes requires, and `supported_ops: send, edit, typing, react,
  delete`).
- The cell: `inbound {event, bufferId}` → Hermes: `inbound_ack
  {bufferId}` once its turn is scheduled. The event is a group message:
  `{text, message_id: <the record's seq>, source: {platform: "relay",
  chat_id: <the chat fragment>, chat_type: "group", chat_name, user_id,
  user_name}}`. A leading `/` is a command to Hermes (`/new` resets the
  shared session), so it is sent behind a zero-width space.
- Hermes: `outbound {requestId, action}` → the cell: `outbound_result
  {requestId, result}`, every one answered (Hermes waits 30 s):
  - `send {chat_id, content, reply_to?}`: a reply (it answers a message:
    `reply_to`, or `metadata.reply_to_message_id`) or its tool progress
    (it answers none); answered with the `message_id` its edits name.
  - `edit {message_id, content}`: the whole text again, with the cursor
    ` ▉` while it streams.
  - `react {message_id, emoji, remove}` on the message it answers: `👀`
    on as its turn starts, off as it ends, then `✅` or `❌`. Hermes has
    no other end-of-turn signal.
  - `delete {message_id}`: a reply taken back (a stopped turn's).
  - `typing`: answered; another op: `{success: false}`.
- The cell: `interrupt_inbound {session_key, chat_id}`: the chat's Stop
  (never a `/stop` message: one while the session is busy stalls Hermes'
  reader for 30 s).
- Hermes: `going_idle` → the cell: `going_idle_ack`.

## What the cell does

- **One message of a chat's at a time.** Hermes in queue mode keeps one
  pending message and one per writer's debounce, and drops a third
  writer's mid-turn, so the cell hands it the chat's next message only
  once its turn ends (its `👀` off, or `TURN_MAX_MS`).
- **Approvals from the owner and editors, at once.** Hermes asks before a
  risky command (its `manual` approvals: a progress line, "Reply
  `/approve`…"), and its turn waits. The chat's next message would wait
  for the turn, and a leading `/` is escaped, so an answer would never
  arrive: `/approve`, `/approve session`, `/approve always` and `/deny`
  alone go on as the command itself, at once, mid-turn, from the chat's
  owner or an editor (trusted as the owner is: docs/one-home.md,
  decision 5). A guest's is text, in its turn. Hermes confirms it as a
  reply of the running turn.
- **Kept until acked.** Each message is kept until Hermes acks it, and
  handed again on its next dial (it drops one it saw, by `(chat_id,
  message_id)`).
- **Into the chat's records**, as the Hermes' computer identity: a
  reply's edits as a `draft` (`PUT …/channels/chat/draft`), the reply as
  `{text, turn}` once the turn ends or a later reply of the turn begins;
  its tool progress's new lines as `turn.step`s on `work`, between
  `turn.start` (as the message is handed) and `turn.end` (`done`, or
  `stopped` with no reply).
- **Away and woken.** Each message handed wakes its computer, as its
  owner wakes it (sandcastle's `POST /v1/computers/{name}/wake`, signed by
  the computer's key, which the cell holds: a paused guest resumes, an
  awake one stays awake an idle time from now), since a paused guest
  keeps its socket open here and hears nothing until it is resumed. With
  no socket at all, messages wait and the cell wakes it again every 10 s
  while they do; at most one wake every 5 s. (Relay's own wake is an
  unsigned GET the gateway registers; the cell needs none, holding the
  computer's key.)

## Hermes' settings

Its init writes Hermes' managed overlay (`/etc/hermes/config.yaml`,
deep-merged over its own config) before it starts:

```yaml
group_sessions_per_user: false   # one session per chat, "[name] …" each message
streaming: {enabled: true, transport: edit}
display:
  busy_input_mode: queue
  tool_progress: all
  tool_progress_grouping: accumulate
  long_running_notifications: false
platforms:
  relay:
    gateway_restart_notification: false
```

## Tests

- `crates/core/src/relay.rs`: the token against Hermes' own test vector,
  frames, the inbound event, the cursor, progress lines.
- The e2e lane `hermes`, on the sandcastle fake's gateway
  (`crates/fakes/src/relay_gateway.rs`, which dials and turns as Hermes
  does): a chat's owner and an invited guest answered by name, someone
  not in the chat unheard, one message at a time in order, tool steps, a
  Stop, a Hermes away woken for a message, one kept across a node
  restart, each answered once, the draft in Chrome, 4401 for a removed
  Hermes, and its chats joining the new one.
