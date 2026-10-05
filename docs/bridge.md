# The bridge

`images/bridge` (Rust, its own workspace with `images/hermes/boot`): the
one process a computer image runs between its agent runtime and the
fragment API (docs/computers.md). It owns nothing (lesson 1): the chat
channel owns what was said, the runtime owns the in-flight turn, the
Computer DO owns the computer. The bridge translates, and keeps only a
cursor per followed channel and the turns it admitted that have not
ended.

- `engine.rs`: the rules, as a pure state machine (records and runtime
  events in, posts and runtime commands out). docs/chat-records.md is
  what it writes.
- `driver.rs`: the restore gate, the state file, followers, posting
  lanes, the keepalive.
- `ready.rs`: which of the computer's agents to run, when the image says
  (`BRIDGE_AGENTS_FILE`): a computer's agents may change while it runs.
- `runtime/`: `relay` (Hermes' Relay connector) and `script` (a
  deterministic agent, the stub image's).
- `screen.rs`: a screen page and an RFB proxy with Take over / Give back:
  its input gate follows every message noVNC 1.7.0 sends (the extended
  clipboard's negative length, the extended pointer event), and its
  control socket answers on an image with no display (the stub's) too.

## A runtime

A runtime gets commands and sends events, each naming its turn
(`runtime/mod.rs`):

| Command | |
|---|---|
| `Start(TurnStart)` | a turn: agent, chat, asker and their name, text, attachments as local files, whether it is a routine |
| `Stop {turn}` | the asker pressed Stop |
| `Answer {turn, prompt, option?}` | a prompt's answer, or its expiry (`None`), once |
| `Forget {turn}` | the bridge ended it (it went quiet 15 minutes, or its agent left) |
| `Tell {turn, seq, by, text}` | the asker's next message while the turn asked them in words (`Asked`): its answer, handed to the running turn, never a turn of its own; only ever to a turn this life runs |

| Event | |
|---|---|
| `Connected(bool)` | the runtime can take turns now, or cannot: the bridge claims a turn only while it can (Relay: Hermes on its socket, greeted; `script`: from its start) |
| `Draft {text}` | the reply so far, shown live |
| `Reply {part, text}` | reply `part` (from 1) whole; posted at the next part, step, prompt, or end |
| `Attachment {part, file}` · `Retract {part}` | a file on a reply; a reply taken back |
| `Step {tool, args, ok, excerpt, text}` | a tool call |
| `Prompt {prompt, text, options, ttl?}` | a card; the turn waits (and the computer may sleep) |
| `Asked` | the turn asked its asker something to answer in words (the question is a reply part before it): their next message to the agent in that chat is its answer (`Tell`); it waits, running, as long as a prompt's life |
| `End {outcome}` | `idle`, `stopped`, or `error` |
| `Say {agent, fragment, text}` | said with no turn running: a turn of its own |

Another runtime is another module here, or a bridge of its own.

## The routes it calls

Every request goes to `FRAGMENT_API` (`http://api.fragment.internal`)
over plain HTTP, with `x-fragment-agent: <agent fragment>` unless noted.
Bodies are JSON. `api.rs` has one method for each.

| Method and path | Body | Reads |
|---|---|---|
| `GET /api/computer` (no agent) | | `{computer, owner, image, agents: [{fragment, identity, name, owner}]}`; at start, then every minute, and within a second of a change to the ready file (below) |
| `GET /api/computer/keepalive` (no agent), WebSocket | | held while a turn waits to run or runs |
| `GET /api/fragments` | | `{fragments: [{name, role}]}` |
| `GET /api/f/{f}/channels` | | `{channels: [{name, post, seq}]}`: a postable `chat` is a chat; the agent's own `tasks` |
| `GET /api/f/{f}/members` | | `{members: [{principal, kind, addedAt}]}`: the lead (first `kind: agent` by `addedAt`), and when this agent joined; read again after 30 s, and at once after a `joined` of one of the computer's agents to that fragment (so its lead never answers an `@mention` of an agent that just joined) |
| `GET /f/{f}/__people?id=…` | | `{profiles: {id: {username}}}`: what to call a writer |
| `GET /api/f/{f}/subscriptions` | | `{subscriptions: [{id, principal, channel, wake}]}` |
| `POST /api/f/{f}/subscriptions` | `{channel, wake: true}` | `{id, channel, wake}`; only when the list has none |
| `GET /api/f/{f}/channels/{c}?after=&limit=1000` | | `{records: [{channel, seq, at, principal, kind, body}], next}`: the catch-up, at most 20 pages |
| `GET /f/{f}/__live?v=2`, WebSocket | `{type: "subscribe", channel, after}`, `{type: "ping"}` | `hello`, `record`, `subscribed {next, more}`; 4003/4004 end the follow |
| `POST /api/f/{f}/channels/{chat\|work}` | `{id, body}` | `{replayed}`; retried 10 times with jitter on a transport error, 429 or 5xx. A turn's claim (its `turn.start`) is answered back to the engine: posted or replayed, this life runs it; 409, another life claimed it; 403/404, the agent may not post there (it left the chat, or its owner holds it below editor), and the turn is dropped; anything else, no answer. Any other 409 is another life's post of the id, and a 403/404 is no longer the agent's to post in: dropped and logged |
| `PUT /api/f/{f}/channels/chat/draft` | `{turn, text \| null}` | at most 4 a second a turn; a 429 is ignored |
| `PUT /api/f/{f}/blobs/{sha256}` | the bytes, `content-type` the file's | a reply's files, before its record |
| `GET /api/f/{f}/blobs/{sha256}` | | a message's files (at most 25 MiB), checked against their hash |

`hermes-boot` also calls, as each agent, on its own fragment:
`GET /api/f/{agent}/files`, `GET /api/f/{agent}/file?path=` (its
`SOUL.md`, `memories/`, `skills/`, `agent.json`), and
`POST /api/f/{agent}/files {files, message, key}` (at most 16 files and
180 KiB a commit; `key` is the batch's hash).

## Settings

`fragment-bridge run` (and `screen`, the screen alone):

| Variable | Default | |
|---|---|---|
| `FRAGMENT_API` | required | the fragment API |
| `RESTORE_PENDING` | | `1`: wait for `BRIDGE_RESTORED` first |
| `BRIDGE_RESTORED` | `/run/computer/restored` | |
| `BRIDGE_HOLD` | `/run/computer/hold` | while this file exists the bridge claims no turn (the platform's hold before every save; looked at before each claim's every try) |
| `BRIDGE_HELD` | `/run/computer/held` | the bridge's answer to the hold: it writes this file while the hold exists and no claim's try is in flight, and removes it otherwise (looked at every 100 ms). An image with more to quiet than the bridge names another file and answers the platform itself |
| `BRIDGE_RUNTIME` | `relay` | `relay` or `script` |
| `BRIDGE_STATE_DIR` | `/data/bridge` | its state; made as it starts (past the restore gate), so a first start has a `/data` to save |
| `BRIDGE_AGENTS_FILE` | | the agents the image has made ready (`src/ready.rs`): `{"agents": [fragment]}`, written whole and renamed into place. Set, the bridge runs only those of `GET /api/computer`'s, in the platform's order, and reads the computer again within a second of the file's change; missing, no agent is ready; one that does not read keeps the set before it. Unset, every agent the platform lists (the stub). Our Hermes image's is `/var/lib/fragment-run/agents.json`, written once each new agent's profile is whole (docs/computers.md) |
| `BRIDGE_MEDIA_DIR` | `/tmp/bridge-media` | attachments, scratch |
| `BRIDGE_PROMPT_TTL_MS` | 3 600 000 | a card's life unless the runtime says |
| `BRIDGE_TURN_IDLE_MS` | 900 000 | a running turn this quiet ends as an error |
| `BRIDGE_RELAY_LISTEN` | `127.0.0.1:8650` | where Hermes dials |
| `BRIDGE_RELAY_SECRET_FILE` | required for `relay` | the per-boot secret (32+ characters) |
| `GATEWAY_RELAY_ID` | `fragment-computer` | the gateway id both sides name |
| `BRIDGE_RELAY_SETTLE_MS`, `BRIDGE_RELAY_EMPTY_SETTLE_MS` | 1 500, 20 000 | the end of a Hermes turn (below) |
| `BRIDGE_SCRIPT_PACE_MS` | 40 | the scripted agent's draft pace |
| `BRIDGE_SCREEN_LISTEN` | | `0.0.0.0:6080`: serve the screen |
| `BRIDGE_SCREEN_DIR` | `/opt/fragment/screen` | its page |
| `BRIDGE_SCREEN_RFB` | | `unix:<path>` or `tcp:<host:port>`: the display; none, the page alone |
| `BRIDGE_SCREEN_START` | | the command that starts the display, run by a viewer that finds it down, at most once a minute while it stays down |

## State, and what it never does twice

`<state>/state.json`, written whole (a temporary file, synced, renamed)
after every step that changed it and before that step's effects: the
cursors (`agent|fragment|channel` → seq) and the turns not yet over
(queued, running, waiting, or ended and owing their last records). It is
a cache. The authority on which turns have started is the chat's `work`
channel, which never goes back in time, so `/data` restored from any
earlier save, or lost, costs reads and runs nothing twice
(docs/explorations/pi-durable.md, P1).

- **One life per turn.** Each bridge process is a life, with 128 random
  bits of its own, never written to `/data`. A turn is run only by the
  life whose claim, its `turn.start` naming the life, the platform
  answered as its own (docs/chat-records.md); a 409 is another life's,
  and the turn is ended as lost; a 403/404 (the agent may not post on the
  chat's `work`) drops the turn, run and recorded nowhere, and is not
  asked again; no answer runs nothing, and the turn is claimed again at
  the next tick. A cursor a rollback sent back reads a record again, and
  its claim is the 409.
- **At a start**, every turn the state holds that is not queued belongs
  to an earlier life and is ended (`error: lost when the computer
  restarted`, its cards `expired`), never handed again: this is the
  at-most-once choice, so a turn whose life ended before it did is lost,
  and said so. Queued turns stay queued and are claimed like any other.
- **A claim waits for the runtime and the hold**: a turn is claimed only
  while the runtime says it can take one (`Connected`), and not while
  `BRIDGE_HOLD` exists (the platform's hold before a save; the file is
  looked at before every try of every claim). Until then it waits,
  unclaimed, and any later life may run it. The bridge answers the hold
  (`BRIDGE_HELD`) only once no try is in flight (from its look at the
  hold to its answer), so a save never misses a claim that lands after
  it; a turn already claimed runs on, and its keepalive is what cancels an
  idle sleep's hold (docs/computers.md).
- **Every turn has both records**: a refusal, and a Stop of a turn that
  waited, post its `turn.start` and then its `turn.end`.
- **An answer in words goes only to its turn's life.** A turn that asks
  its asker something in words (`Asked`) is a running turn of its life,
  and their next message to the agent in that chat is its answer
  (`Tell`), starting no turn. A restart ends it as lost, as it ends every
  running turn, so a later life tells it nothing: the message, read
  there, is a turn of its own, claimed and run once. So is one a rollback
  sends a cursor back before, though an earlier life told it (a Tell
  writes nothing on `work`). A Stop answers the question too (Relay says
  "Stop."), so the asker's next message queues behind the stopping turn.
- What the runtime says unasked is a turn of the life's counter and the
  life, so a counter a rollback sent back collides with nothing.

A state that contradicts itself (a turn past its cursor, an id not its
cause's) is refused at start, never repaired. A state of another format
(an older bridge's) is set aside for a fresh one: the journal says which
turns ran.

- **A turn leaves the state only once its last records are answered.**
  The state is written before a step's effects, so a turn that has ended
  there would otherwise have its `turn.end` only in its fragment's lane,
  and a crash then would leave it open for good (its start, and no end),
  or leave a refused or queued-and-stopped turn with no record at all.
  So an ended turn is kept (`ended`), holding what it owes `work`: its
  `turn.end`, and for a turn that never ran its `turn.start` too, with
  their bodies. It is let go once the lane is done with each: answered
  (appended, a replay, or refused: a 409 is another life's end, a
  403/404 a chat it may not post in), or given up by the lane's rule for
  every post, `POST_TRIES_MAX` (10) tries with a jittered backoff on a
  transport error, 429 or 5xx. A life that ends first leaves them to the
  next, which posts every one again at its start under the same id and
  body: a replay when it had landed. This closes the gap the simulation
  found (`any_history_of_crashes_and_rollbacks_keeps_the_invariants`
  checks I2 with no case let off; `a_crash_that_drops_a_turns_end_leaves_
  nothing_open` and `an_end_a_crash_kept_from_the_platform_is_posted_by_
  the_next_life` each way it happened). A record that keeps failing holds
  its turn for one life's tries, and each later life tries it again once.
  Not kept: the turn's last reply, and its cards' closings, posted in the
  same step; a crash before they land loses them. A turn refused past
  `TURNS_OPEN_MAX` is not kept either.

Every bound is a const in `limits.rs`, with its reason. SIGTERM: gone
within 3 s.

## Hermes' Relay, as the bridge speaks it

Hermes v0.21.5 (tag `v2026.9.24`, the newest release on 2026-10-03;
`gateway/relay/`), contract version 1. The bridge serves
`ws://127.0.0.1:8650/relay` and `/relay/media` and checks Hermes'
token; the descriptor names platform `relay` with draft streaming and
the ops `send, edit, delete, typing, react, draft, prompt, send_media,
get_chat_info`.

- Each turn is an `inbound` group message routed to the agent's profile
  (`source.profile`), its chat id `<chat fragment>/<agent fragment>` so
  two agents in one chat are two chats to Hermes, its message id the
  turn id. Claimed only while Hermes is on its socket and has said
  `hello` (the bridge starts just before Hermes' gateway, so the message
  that woke the computer waits for it, unclaimed); once handed, kept until
  Hermes acks it and handed again on each dial, within that life.
- A reply streams as `draft` frames (the chat's draft), and arrives as a
  `send` answering the turn's message; tool progress is a `send`
  answering nothing whose lines grow by `edit`, each new line a step.
- An approval is a `prompt` op (`once`, `session`, `always`, `deny`);
  the owner's answer goes back at once as an inbound `prompt_response`.
  Expiry needs no word: Hermes' `approvals.timeout` is the card's life.
- A file Hermes sends is uploaded to `/relay/media`, then `send_media`;
  a message's attachments are served at `/relay/media/<id>`, behind the
  token.
- A question to answer in words: an open `clarify` (`❓ …`, the base
  adapter's text prompt), or `✏️ Type your answer:` after "Other" on a
  clarify's card. It is a `send`: the bridge shows it as a reply part and
  the turn asks (`Asked`). Hermes blocks until the person's next message,
  which its gateway's clarify intercept takes even mid-turn, so the
  asker's next message goes back at once as an inbound in the same chat
  (`Tell`), not queued as a turn behind this one (which would wait out
  Hermes' clarify timeout, an hour).
- Stop is `interrupt_inbound` for the profile's session key. A clarify
  waiting on words never sees it, so a Stop while the turn asks is
  followed by the words "Stop.", which let the wait go.
- The end: `👀` on, `👀` off, then `✅` or `❌`. Hermes' multiplexed
  gateway brackets a message twice, an empty dispatch bracket first, so
  a turn ends at `✅` only once it said something, after
  `BRIDGE_RELAY_SETTLE_MS` when stopped, and an empty one after
  `BRIDGE_RELAY_EMPTY_SETTLE_MS` with no new `👀`.

Of decision 21's list, v0.21.5 has every op, but sends `task_card` only
for Slack chats (`gateway/run_turn.py`), so steps come from progress
text; `follow_up` is Discord's interaction tokens; neither is
advertised. It also has `thread_create` and `thread_rename` (threads are
off here).
