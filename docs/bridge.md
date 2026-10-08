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
- `screen.rs`: each agent's screen (`?agent=<agent fragment>` on its
  sockets): a page, and an RFB proxy onto that agent's own display with
  Take over / Give back. Its input gate follows every message noVNC 1.7.0
  sends (the extended clipboard's negative length, the extended pointer
  event), and its control socket answers on an image with no display (the
  stub's) too. An agent the bridge does not run, or one the image names
  no screen for, is 404; no agent's name, 400.
- `screens.rs`: which display is each agent's, when the image says
  (`BRIDGE_SCREENS_FILE`), with its runtime's lease and activity file.
- `lease.rs`: who drives a screen, the agent or one person, as Hermes'
  Bot Desktop lease file has it (`lease.json` under its `lease.lock`
  flock, its epoch one on at every change): Take over is that lease, so
  the agent's tools refuse while a person holds it (`human_has_control`).
  A screen with no lease file keeps one of its own, by the same rules.

## A runtime

A runtime gets commands and sends events, each naming its turn
(`runtime/mod.rs`):

| Command | |
|---|---|
| `Start(TurnStart)` | a turn: agent, chat, asker and their name, text, attachments as local files, whether it is a routine, and its `note` when the agent's turn before it in the chat was cut by a restart (below, "The turn after a cut one is told") |
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
| `Prompt {prompt, text, options, ttl?}` | a card; the turn waits, its computer kept awake until the card is answered or expires |
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
| `GET /api/computer/keepalive` (no agent), WebSocket | | held while a turn waits to run, runs, or waits on its card (below, "A card keeps its computer awake") |
| `GET /api/fragments` | | `{fragments: [{name, role}]}` |
| `GET /api/f/{f}/channels` | | `{channels: [{name, post, seq}]}`: a postable `chat` is a chat; the agent's own `tasks` |
| `GET /api/f/{f}/members` | | `{members: [{principal, kind, addedAt}]}`: the lead (first `kind: agent` by `addedAt`), and when this agent joined; read again after 30 s, and at once after a `joined` of one of the computer's agents to that fragment (so its lead never answers an `@mention` of an agent that just joined) |
| `GET /f/{f}/__people?id=…` | | `{profiles: {id: {username}}}`: what to call a writer |
| `GET /api/f/{f}/subscriptions` | | `{subscriptions: [{id, principal, channel, wake}]}` |
| `POST /api/f/{f}/subscriptions` | `{channel, wake: true}` | `{id, channel, wake}`; only when the list has none |
| `GET /api/f/{f}/channels/{c}?after=&limit=1000` | | `{records: [{channel, seq, at, principal, kind, body}], next}`: the catch-up, at most 20 pages; and a turn's note, read back from its claim on `work` (and from `chat`'s tail) in pages of 100, at most 1000 records each |
| `GET /f/{f}/__live`, WebSocket | `{type: "subscribe", channel, after}`, `{type: "ping"}` | `hello`, `record`, `subscribed {next, more}`; 4003/4004 end the follow |
| `POST /api/f/{f}/channels/{chat\|work}` | `{id, body}` | `{record, replayed}` (a claim's `record.seq` is where the turn's note is read back from); retried 10 times with jitter on a transport error, 429 or 5xx. A turn's claim (its `turn.start`) is answered back to the engine: posted or replayed, this life runs it; 409, another life claimed it; 403/404, the agent may not post there (it left the chat, or its owner holds it below editor), and the turn is dropped; anything else, no answer. Any other 409 is another life's post of the id, and a 403/404 is no longer the agent's to post in: dropped and logged |
| `PUT /api/f/{f}/channels/chat/draft` | `{turn, text \| null}` | at most 4 a second a turn; a 429 is ignored |
| `PUT /api/f/{f}/blobs/{sha256}` | the bytes, `content-type` the file's | a reply's files, before its record |
| `GET /api/f/{f}/blobs/{sha256}` | | a message's files (at most 25 MiB), checked against their hash |

`hermes-boot` also calls, as each agent, on its own fragment:
`GET /api/f/{agent}/files`, `GET /api/f/{agent}/file?path=` (its
`SOUL.md`, `memories/`, `skills/`, `agent.json`), and
`POST /api/f/{agent}/files {files, message, key}` (at most 16 files and
180 KiB a commit; `key` is the batch's hash).

## Settings

`fragment-bridge run`:

| Variable | Default | |
|---|---|---|
| `FRAGMENT_API` | required | the fragment API |
| `RESTORE_PENDING` | | `1`: wait for `BRIDGE_RESTORED` first |
| `BRIDGE_RESTORED` | `/run/computer/restored` | |
| `BRIDGE_HOLD` | `/run/computer/hold` | while this file exists the bridge claims no turn (the platform's hold before every save; looked at before each claim's every try) |
| `BRIDGE_HELD` | `/run/computer/held` | the bridge's answer to the hold: it writes this file while the hold exists and no claim's try is in flight, and removes it otherwise (looked at every 100 ms). An image with more to quiet than the bridge names another file and answers the platform itself |
| `BRIDGE_HELD_LEAVE_OUT` | none | what the save may leave out, written into the answer one per line (whitespace between here): gitignore patterns relative to `/data`, of letters, digits and `._-/*?[]`, at most 16 (docs/computers.md, "The hold"). The stub names `*.scratch` |
| `BRIDGE_RUNTIME` | `relay` | `relay` or `script` |
| `BRIDGE_STATE_DIR` | `/data/bridge` | its state; made as it starts (past the restore gate), so a first start has a `/data` to save |
| `BRIDGE_AGENTS_FILE` | | the agents the image has made ready (`src/ready.rs`): `{"agents": [fragment]}`, written whole and renamed into place. Set, the bridge runs only those of `GET /api/computer`'s, in the platform's order, and reads the computer again within a second of the file's change; missing, no agent is ready; one that does not read keeps the set before it. Unset, every agent the platform lists (the stub). Our Hermes image's is `/var/lib/fragment-run/agents.json`, written once each new agent's profile is whole (docs/computers.md) |
| `BRIDGE_MEDIA_DIR` | `/tmp/bridge-media` | attachments, scratch |
| `BRIDGE_PROMPT_TTL_MS` | 3 600 000 | a card's life unless the runtime says |
| `BRIDGE_TURN_IDLE_MS` | 900 000 | a running turn this quiet ends as an error |
| `BRIDGE_RELAY_LISTEN` | `127.0.0.1:8650` | where Hermes dials |
| `BRIDGE_RELAY_SECRET_FILE` | required for `relay` | the per-boot secret (32+ characters) |
| `GATEWAY_RELAY_ID` | `fragment-computer` | the gateway id both sides name |
| `BRIDGE_SCRIPT_PACE_MS` | 40 | the scripted agent's draft pace |
| `BRIDGE_SCREEN_LISTEN` | | `0.0.0.0:6080`: serve the screens. Their sockets wait (at most 30 s) for the agents, told past the restore gate, so no lease under `/data` is touched before the restore |
| `BRIDGE_SCREEN_DIR` | `/opt/fragment/screen` | its page |
| `BRIDGE_SCREENS_FILE` | | each agent's screen (`screens.rs`): `{"screens": [{agent, rfb, lease?, activity?}]}`, `rfb` as `unix:<path>` or `tcp:<host:port>`, written whole and renamed into place, read again when it changes. Unset, no agent has a display (the stub); set, an agent it does not name has no screen. While someone watches a screen its `activity` file's time is set every 10 s (`ACTIVITY_EVERY_MS`), so the image's idle stop leaves a watched desktop up. Our Hermes image's is `/var/lib/fragment-run/screens.json` (docs/computers.md) |
| `BRIDGE_SCREEN_START` | | the command that starts an agent's display, run with the agent's fragment as its last argument by a viewer that finds it down: at once when it answered since the last start (it stopped or restarted under its viewers, whose streams end with it), else at most once a minute while it stays down |

## State, and what it never does twice

`<state>/state.json`, written whole (a temporary file, synced, renamed)
after every step that changed it and before that step's effects: the
cursors (`agent|fragment|channel` → seq), the turns not yet over
(queued, running, waiting, or ended and owing their last records), and
each chat's budget of agent turns ("Agents asking each other", below). It
is a cache. The authority on which turns have started is the chat's `work`
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

## Agents asking each other

A person's agents hand work to each other in a chat they share, by
`@name` in a reply (the bridge stamps its `to`), or by a message one posts
itself naming the other in `to` (`fragment ask`, cli/src/ask.rs). The
bridge that answers decides whether it is a turn, and how deep
(docs/chat-records.md, "An agent's reply"):

- **The hop is counted here, not read.** A record by an agent of this
  computer is one hop past the turn that agent is in: its turn running in
  that chat, or the one there the record's `turn` names when that ended
  within `ENDED_HOPS_MS` (remembered, never written: a reply read just
  after its turn was let go); and its deepest turn running anywhere else,
  the deeper of the two; in none at all, `HOPS_MAX` (answered, handing on
  nothing). A CLI post names no turn, so a turn just ended does not count
  for it (`an_ended_turn_counts_only_for_the_reply_that_names_it`). A record's `hop` only raises it. So a post made
  around the bridge (the CLI, the API, a script the agent left running)
  resets nothing: `a_hand_off_loop_stops_at_the_cap` runs the same
  A, B, A, B loop with each hand-off posted by its turn as the CLI does (no
  `hop`, no `turn`) and stops at the same place, and `a_scripted_hand_off_loop_stops_at_the_cap`
  (tests/bridge.rs) runs it with the scripted runtime, then an agent's
  CLI-shaped post, answered once. What is remembered of ended turns is
  never written to `/data`: a reply the next life reads first is the last
  hop (`a_reply_read_by_the_next_life_resets_nothing`). Another
  computer's agent (another person's, in a shared chat) is held to the hop
  it claims, at least 1, and by the budget.
- **A chat's budget.** Turns its agents start of each other are kept per
  chat in the state (`agent_turns`: the causing records' `at`s, the
  platform's clock, so every life counts alike), at most
  `AGENT_TURNS_PER_CHAT_MAX` in `AGENT_TURNS_WINDOW_MS`; past it a
  hand-off's turn is refused with both its records, its end saying why
  (`a_chats_agents_have_a_budget`: valid, past it, a person never
  counted, replay, restart, another chat's own, the window sliding). A
  state from before the budget loads with none.
- **`tasks` is the agent's own.** A routine or `joined` starts anything
  only from the agent fragment's own key (not an identity: its cron, the
  platform) or its owner; another agent acting for the owner, who may post
  there, is passed over (`tasks_hear_only_the_owner_and_the_fragment`).

Why here: the platform holds no chat record (docs/cloudflare-v1.md, the
rule), and the bridge is the one place that knows which turn an agent is
in. A person's agents all run on one computer (decision 13), so every
hand-off between them meets this count.

## A card keeps its computer awake

A turn waiting on its card holds the keepalive until the card is
answered or expires, at most the card's life (`BRIDGE_PROMPT_TTL_MS`, an
hour; docs/durable-computers.md, P6 for now). So a card expires with its
runtime there: Hermes' approval times out with it (the image sets
`approvals.timeout` to the card's life), the command does not run, Hermes
says so and ends the turn, and the chat's next message is answered.

It used to be let go (decision 42 as first written), so an idle computer
slept 20 minutes into an unanswered card, and the next life ended its turn
as lost. Hermes, though, had kept the cut turn's message as its session's
last, unanswered, and folds the next message into it (two user messages
in a row are one): the model was given `[paul] do the risky thing\n\n[paul]
good morning`, and asked the cut command's approval again. Every message
after a missed card was met by the card again, and the computer slept
under each one (Paul on p5, 2026-10-05: "I missed the 1hr window and now
it's not responding to chats"; `an_expired_approval_ends_its_turn` in
tests/docker.rs, and its scripted twin in tests/relay.rs). A restart for
any other reason while a card is open (an owner's sleep, a crash, a
deploy) still cuts the turn, which ends as lost; the next message is now
a turn of its own, told what was cut (below), never folded into the cut
request. The card's promise outliving a restart is P6 of
docs/explorations/pi-durable.md, and the debt ledger.

## The turn after a cut one is told

A turn a restart cut ends as lost (one life per turn: "State", above),
and is never run again. What it did before the cut may have effects (an
email sent, a file half written), and its runtime may still hold its
request: Hermes keeps a turn's message as its session's last until the
turn answers, and joins the next message to it (two user messages in a
row are one), so its model was handed `[paul] do the risky thing\n\n[paul]
good morning` and did the risky thing again (F10 of
docs/explorations/pi-durable.md; P5 of docs/durable-computers.md). Two
parts close it:

- **The note (every runtime).** The first turn an agent runs in a chat
  after one of its turns there ended as lost carries a note
  (`TurnStart::note`, `src/note.rs`): its first line says the turn before
  was cut short by a restart and to check what it already did before
  doing any of it again (the message below is new); then what that turn
  was asked (its cause's text), the steps `work` recorded of it (tool,
  arguments, ok or failed; at most 8, the rest counted), its cards and how
  they closed, and the replies it had said (the last two). At most 4 KiB.
  It is built from the chat's journal alone: the driver reads `work` back
  from the turn's claim (the claim's answer names its seq) to the agent's
  turn before it there, a refusal's passed over, and, when that turn ended
  `lost when the computer restarted`, its cause and its replies (`chat`,
  back from its tail to that turn's cause or start). So it is the same in
  any life and after any rollback of `/data`, and said once: at the turn
  after, the turn before is this one. A journal that does not answer
  within 5 s, or an agent's turn before further back than 1000 records,
  gives no note: a turn is never held back longer for one. Relay hands it as the inbound's
  read-only `context` (below); the scripted agent echoes it after its
  reply (`(told: …)`).
- **The cut turn closed in Hermes' session (our image).** Before the
  gateway starts, `hermes-boot` appends Hermes' own failed-turn boundary
  (the assistant row Hermes writes itself when a turn ends without an
  answer, `display_kind` `failed_turn`) to each relay session whose last
  message is unanswered (a user row, a tool call, or a tool result),
  through Hermes' session code (`hermes::CLOSE_CUT_TURNS`;
  docs/computers.md). The next message is then a user message of its own.

Proven with the real Hermes in `a_turn_cut_by_a_restart_is_closed_and_told`
(tests/docker.rs): an owner's sleep under a card, then "good morning": no
second call of the cut command, no card again, an answer, and the model's
request ends with the note and the message after the boundary. Whether a
real model, so told, checks before it redoes is a hosted run's to see.

## Hermes' Relay, as the bridge speaks it

Hermes v0.21.6 (tag `v0.21.6`, 2026-10-08; read first from v0.21.5,
whose wire it keeps; `gateway/relay/`), contract version 1. The bridge serves
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
  Hermes acks it and handed again on each dial, within that life. A
  turn's note (above, "The turn after a cut one is told") is the
  inbound's `context`, one item with no source, never its text: Hermes
  renders it before the message, reference only and never as the person's
  words:
  `[Recent channel messages]\n<note>\n\n[New message]\n[paul] good morning`.
- A reply streams as `draft` frames (the chat's draft), and arrives as a
  `send` answering the turn's message; tool progress is a `send`
  answering nothing whose lines grow by `edit`, each new line a step.
  Hermes edits it at most every 1.5 s, and upstream held a line that came
  sooner until a newer one, so a turn's last quick tool call showed no
  step; our image patches its sender to send the line once the interval
  is out (images/hermes/Dockerfile; the debt ledger).
- One reply a turn, its answer (Paul, 2026-10-05). Hermes ends a draft
  segment at every tool boundary with a `send` answering the message, so
  the model's text beside a tool call ("Let me check that.") arrives as
  a reply, whatever its `display.interim_assistant_messages` (off in our
  image: docs/hermes-relay.md, "Hermes' settings"). The step that follows
  takes it back (`Retract`) as its words, the step's `text`; when the
  step reaches the bridge first, the `send` ending drafts that began
  before it is its words, never a reply. A turn that ends idle having
  said nothing since says the last words a step took as its reply:
  Hermes' answer written beside a housekeeping call (`memory`), which it
  sends once.
- An approval is a `prompt` op (`once`, `session`, `always`, `deny`);
  the owner's answer goes back at once as an inbound `prompt_response`.
  Expiry needs no word: Hermes' `approvals.timeout` is the card's life
  (`hermes-boot` gives the bridge the same; `HERMES_BOOT_APPROVAL_TIMEOUT_S`
  is a test's shorter one). At its timeout Hermes tries to edit the card
  (the bridge refuses: it is no message of the turn), sends `⌛ Approval
  timed out …` (a step), hands the model `BLOCKED: Command timed out
  without user response`, and the turn goes on to its reply.
- A file Hermes sends is uploaded to `/relay/media`, then `send_media`;
  a message's attachments are served at `/relay/media/<id>`, behind the
  token.
- A question to answer in words: an open `clarify` (`❓ …`, the base
  adapter's text prompt), or `✏️ Type your answer:` after "Other" on a
  clarify's card, each read by its glyph (Hermes' translations keep
  them). It is a `send`: the bridge shows it as a reply part and
  the turn asks (`Asked`). Hermes blocks until the person's next message,
  which its gateway's clarify intercept takes even mid-turn, so the
  asker's next message goes back at once as an inbound in the same chat
  (`Tell`), not queued as a turn behind this one (which would wait out
  Hermes' clarify timeout, an hour).
- Stop is `interrupt_inbound` for the profile's session key. A clarify
  waiting on words never sees it, so a Stop while the turn asks is
  followed by the words "Stop.", which let the wait go.
- The end: `👀` on, `👀` off, then `✅` or `❌`. A turn ends at its `❌`,
  at its `✅` once it said something, and, stopped, at its `👀` off.
  Hermes brackets a message it took while its gateway was starting
  twice, the first empty (docs/hermes-relay.md), and ends no person's
  turn without saying something, so an empty bracket's `✅` ends
  nothing. No clock: a turn whose bracket never ends is the idle
  bound's (`BRIDGE_TURN_IDLE_MS`: "the agent stopped answering").

Of decision 21's list, v0.21.5 has every op, but sends `task_card` only
for Slack chats (`gateway/run_turn.py`), so steps come from progress
text; `follow_up` is Discord's interaction tokens; neither is
advertised. It also has `thread_create` and `thread_rename` (threads are
off here).
