# The bridge

`images/bridge` (Rust, `images/`'s own workspace): the one process a
computer image runs between its agent runtime and the fragment API
(docs/computers.md). It owns nothing (lesson 1): the chat
channel owns what was said, the runtime owns the in-flight turn, the
Computer DO owns the computer. The bridge translates, and keeps only a
cursor per followed channel and the turns it admitted that have not
ended.

- `engine.rs`: the rules, as a pure state machine (records and runtime
  events in, posts and runtime commands out). docs/chat-records.md is
  what it writes.
- `driver.rs`: the restore gate, the state file, followers, posting
  lanes, the keepalive.
- `runtime/`: `goose` (goose over ACP, our goose image's: below, "goose,
  as the bridge speaks it", with the skills each turn installs,
  `skills.rs`, and the computer's page of the platform skill,
  `computer.md`) and `script` (a deterministic agent, the stub image's).
- `screen.rs`: each agent's screen (`?agent=<agent fragment>` on its
  sockets): a page, and an RFB proxy onto that agent's own display with
  Take over / Give back. Its input gate follows every message noVNC 1.7.0
  sends (the extended clipboard's negative length, the extended pointer
  event), and its control socket answers on an image with no display (the
  stub's) too. An agent the bridge does not run, or one the image names
  no screen for, is 404; no agent's name, 400.
- `screens.rs`: which display is each agent's, when the image says, with
  its runtime's lease and activity file: a file naming them
  (`BRIDGE_SCREENS_FILE`), or a directory naming every agent's by
  convention (`BRIDGE_SCREENS_DIR`: our goose image's).
- `lease.rs`: who drives a screen, the agent or one person (`lease.json`
  under its `lease.lock` flock, its epoch one on at every change, the
  shape Hermes' Bot Desktop lease had): Take over is that lease, so the
  agent's screen tools refuse while a person holds it
  (`human_has_control`). A screen with no lease file keeps one of its
  own, by the same rules.

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
| `Connected(bool)` | the runtime can take turns now, or cannot: the bridge claims a turn only while it can (`goose` and `script`: from their start) |
| `Draft {text}` | the reply so far, shown live; empty, the draft stops (its words went to a step) |
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
| `GET /api/computer` (no agent) | | `{computer, owner, image, agents: [{fragment, identity, name, owner}]}`; at start, then every minute |
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
| `POST /api/f/{f}/ops/view` | `{id: "<turn>-view", input: {}}` | `{result: {text, …}}`: the `goose` runtime's, at each turn's start, the fragment's view (a mind's: docs/optchat.md); 400, 403 or 404 (`unknown_operation`: a chat), none; tried 3 times on a transport error, 429 or 5xx, then none |

goose itself calls `FRAGMENT_MODEL` as the agent (below), and a mind's
`fragment mcp` calls `GET /api/f/{mind}/status` and `POST
/api/f/{mind}/ops/{view,zoom,date,search}` as the agent (cli/src/mcp.rs).
On our goose image the agent's screen tools call `FRAGMENT_MODEL` as the
agent too: `screen_look` its `/v1/chat/completions` with `model:
"vision"` and a screenshot, `screen_click` its `/v1/decide` (Clef).

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
| `BRIDGE_RUNTIME` | `goose` | `goose` or `script` |
| `BRIDGE_STATE_DIR` | `/data/bridge` | its state; made as it starts (past the restore gate), so a first start has a `/data` to save |
| `BRIDGE_MEDIA_DIR` | `/tmp/bridge-media` | attachments, scratch |
| `BRIDGE_PROMPT_TTL_MS` | 3 600 000 | a card's life unless the runtime says |
| `BRIDGE_TURN_IDLE_MS` | 900 000 | a running turn this quiet ends as an error |
| `FRAGMENT_MODEL` | required for `goose` | goose's model host (its OpenAI provider's `OPENAI_HOST`) |
| `BRIDGE_GOOSE_BIN` | `/usr/local/bin/goose` | goose, run as `goose acp --with-builtin <BRIDGE_GOOSE_BUILTINS>` |
| `BRIDGE_GOOSE_BUILTINS` | `developer,skills` | goose's builtins: its shell and editor, and its skills (`load_skill`) |
| `BRIDGE_GOOSE_DESKTOP` | | the image's `fragment-desktop`: set, every session gets the agent's `browser`, `computer` and `web` MCP servers (`fragment-desktop mcp …`), and each goose runs with `DISPLAY` naming its agent's own display (`fragment-desktop display <agent>`); the goose image's `/usr/local/bin/fragment-desktop` |
| `BRIDGE_GOOSE_TOOLS` | the place's | which of the desktop's MCP servers every session gets, commas between (`none` for none): on a computer `browser,computer,web`; on a machine `browser,web` (`computer` refused there) |
| `BRIDGE_GOOSE_PLACE` | `computer` | `machine`: the agent's goose runs on its owner's own machine, paired as its hands (`fragment hands run`; docs/optchat.md, "A machine as hands"): every session is told so (`HANDS_MACHINE` in `HANDS`' place, the same bytes every turn), the platform skill's page is the machine's (`runtime/machine.md`), the browser is headless (`fragment-desktop mcp browser --headless`), and no display is asked for |
| `BRIDGE_GOOSE_SKILLS` | | `1`: each turn installs its agent's skills first (below) |
| `BRIDGE_GOOSE_WORK` | `/data/work` | each session's cwd |
| `BRIDGE_GOOSE_HOME` | `/data/work/home` | goose's and its tools' `HOME` |
| `BRIDGE_GOOSE_ROOT` | `/tmp/goose` | each agent's goose's own state, `<root>/<agent>` (`GOOSE_PATH_ROOT`): scratch, never `/data` |
| `BRIDGE_GOOSE_TIER` | `medium` | the model tier goose's calls name |
| `BRIDGE_GOOSE_CLI` | | the fragment CLI: set, a mind's sessions get `fragment mcp <mind>` (the goose image's `/usr/local/bin/fragment`) |
| `BRIDGE_TRUST_CA` | | the interception CA, appended to `/etc/ssl/certs/ca-certificates.crt` once it appears (at most 15 s: docs/computers.md, "Connections and operator keys"); the goose image's `/etc/cloudflare/certs/cloudflare-containers-ca.crt` |
| `BRIDGE_SCRIPT_PACE_MS` | 40 | the scripted agent's draft pace |
| `BRIDGE_SCREEN_LISTEN` | | `0.0.0.0:6080`: serve the screens. Their sockets wait (at most 30 s) for the agents, told past the restore gate, so no lease under `/data` is touched before the restore |
| `BRIDGE_SCREEN_DIR` | `/opt/fragment/screen` | its page |
| `BRIDGE_SCREENS_FILE` | | each agent's screen (`screens.rs`): `{"screens": [{agent, rfb, lease?, activity?}]}`, `rfb` as `unix:<path>` or `tcp:<host:port>`, written whole and renamed into place, read again when it changes. Unset (and no `BRIDGE_SCREENS_DIR`), no agent has a display (the stub); set, an agent it does not name has no screen. While someone watches a screen its `activity` file's time is set every 10 s (`ACTIVITY_EVERY_MS`), so the image's idle stop leaves a watched desktop up |
| `BRIDGE_SCREENS_DIR` | | in place of the file, a directory naming every agent's screen by convention: `<dir>/<16 hex of SHA-256 of the agent's fragment>/` holds its display `rfb.sock`, its `lease.json` and its `activity`, whether or not its desktop has started (an absolute path of at most 64 bytes, so the socket's fits). Our goose image's is `/run/desktop` (docs/computers.md) |
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
  writes nothing on `work`). A Stop answers the question too, so the
  asker's next message queues behind the stopping turn.
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
runtime there, its turn ends as the runtime ends it, and the chat's next
message is answered.

It used to be let go (decision 42 as first written), so an idle computer
slept 20 minutes into an unanswered card, and the next life ended its turn
as lost; the runtime of then had kept the cut turn's message as its
session's last, folded the next message into it, and asked the cut
command's approval again, under every message after (Paul on p5,
2026-10-05). A restart for any other reason while a card is open (an
owner's sleep, a crash, a deploy) still cuts the turn, which ends as lost;
the next message is a turn of its own, told what was cut (below). The
card's promise outliving a restart is P6 of
docs/explorations/pi-durable.md, and the debt ledger. goose shows no
cards (below): of our runtimes only the scripted agent (the stub's) does.

## The turn after a cut one is told

A turn a restart cut ends as lost (one life per turn: "State", above),
and is never run again. What it did before the cut may have effects (an
email sent, a file half written), and a runtime that keeps sessions may
still hold its request and join the next message to it (F10 of
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
  gives no note: a turn is never held back longer for one. `goose` puts it
  before the turn's text in its task; the scripted agent echoes it after
  its reply (`(told: …)`).
- **No session is loaded again.** goose runs each turn in a fresh
  session, so nothing of a cut turn's request is ever handed to the next.

## goose, as the bridge speaks it

`src/runtime/goose.rs`: goose (github.com/aaif-goose/goose), in our image
our fork's build (futurepaul/goose `fragment/optmem` at `4cfb2d7d`:
upstream v1.53.0 and docs/optchat.md's surgery; images/goose/Dockerfile),
spoken to over ACP, the Agent Client Protocol (agentclientprotocol.com):
JSON-RPC 2.0, one message a line each way, on the stdio of `goose acp
--with-builtin developer`. Upstream's v1.53.0 speaks it alike, less the
fork's two settings.

- **One goose per agent.** An agent's goose starts at its first turn, and
  again at the turn after it died (every turn it ran ends as an error:
  `goose: it stopped`). Its environment is the agent's, because a goose's
  provider headers and its tools' environment are per process:
  - its model is goose's OpenAI provider at `FRAGMENT_MODEL`
    (`OPENAI_HOST`, `v1/chat/completions`), the tier `medium`, no key
    (`OPENAI_API_KEY` empty: goose then reads `OPENAI_CUSTOM_HEADERS` from
    the environment too), every call naming the agent
    (`OPENAI_CUSTOM_HEADERS=x-fragment-agent=<agent>`: docs/computers.md,
    "Models"). It also asks `GET /v1/models` once a session, which the
    intercept answers 404, unmetered; `GOOSE_CONTEXT_LIMIT=128000` spares
    it the context probe;
  - its shell's `fragment` acts as the agent, for its owner
    (`FRAGMENT_AS_AGENT`, `FRAGMENT_FOR`, `FRAGMENT_API`: cli/GUIDE.md, "As
    an agent"), and each credential's placeholder is in its variables
    (docs/computers.md, "Connections and operator keys"). A goose started
    with other credentials is started again at the agent's next turn when
    none of its turns runs;
  - nothing of goose's own runs beside the turn: no compaction
    (`GOOSE_AUTO_COMPACT_THRESHOLD=0`, and the fork's `GOOSE_NO_COMPACTION=1`
    for an overflow too), a system prompt that never changes within a
    session (the fork's `GOOSE_STABLE_SYSTEM_PROMPT=1`), no extension of
    its config (`EXTENSIONS={}`: its builtins, `developer` and `skills`, and
    a session's own alone; no subagents, scheduler or memory of goose's),
    no session naming
    (`GOOSE_DISABLE_SESSION_NAMING=true`), `GOOSE_MODE=auto` (it asks no
    permission), no keyring, no telemetry;
  - its state is `<BRIDGE_GOOSE_ROOT>/<agent>` (`GOOSE_PATH_ROOT`), never
    `/data`: no session is ever loaded again. Its `HOME` is
    `BRIDGE_GOOSE_HOME`, in the work (docs/computers.md, the seam).
- **A turn is a fresh session**: `initialize` once per goose (protocol 1,
  no file system or terminal of the client's), then per turn
  `session/new` (`cwd` `/data/work`, a title of ours), and
  `session/close` when it ends (its MCP servers stop with it). Never
  `session/load`.
- **A mind's turn** (its fragment answers `ops/view`, above): the session
  gets `fragment mcp <mind>` as the agent (`mcpServers`, named `mind`:
  `mind__view`, `mind__zoom`, `mind__date`, `mind__search`, read-only) and,
  through `_goose/unstable/session/system-prompt/set` (`append`, key
  `fragment`), OptChat's subagent framing and VIEW_DOC, "OptChat" read
  "Mind" (its spec, §9 and §7.2: the same bytes every turn, so the prefix
  caches). Its prompt is two text blocks, the view, then the task. Any
  other turn's prompt is the task alone. The task is the turn's note
  (above), its text, and its files' paths on this computer.
- **What goose says** (`session/update`): `agent_message_chunk` text is
  the draft (another message with no tool call between is another
  paragraph); at a `tool_call` the words so far are that step's (`text`)
  and the draft stops; the call is a step once its `tool_call_update`
  says `completed` or `failed` (`tool` its name less its extension,
  `args` its command or path, else its input; `excerpt` its result's
  text, else its structured output); thoughts, usage and goose's own
  notices say nothing. The prompt's answer ends the turn: `end_turn`,
  `max_tokens` and `max_turn_requests` are `idle`, `cancelled` after a
  Stop `stopped`, anything else an error. A model's refusal of a call (402
  `budget_used_up`) reaches the chat as goose's words, then `end_turn`.
- **Exactly one reply a turn, at its end.** A mind takes a hand-off's one
  `chat` reply as its report (one run of its trigger each: the platform's
  breaker allows a fragment 120 triggered runs an hour), and follows the
  steps on `work`. So the words between tool calls are only ever drafts
  and steps' `text`, never a reply; the reply is the words after the last
  tool call of a turn that ended `idle`, and otherwise (no such words, a
  Stop, an error, or a turn that ended before goose ran it: a Stop while
  its session was made, a goose that did not start)
  `(ended: <outcome>: <why>)`, as `(ended: error: goose: it stopped)`.
- **Stop** is `session/cancel`; a Stop before the session is made ends the
  turn at once. The bridge's own end (`Forget`) cancels and closes it,
  and says nothing (the engine ended the turn itself).
- **goose asks nothing a person answers.** It shows no card, and asks no
  question in words: in `auto` mode it asks no permission, and one it asks
  anyway (`session/request_permission`, a security inspector's) is refused
  at once (`reject_once`). Anything else it asks of the client is not
  offered (-32601). So no `Answer` or `Tell` comes for its turns. (A
  person's message mid-turn could reach it between tool calls by
  `_goose/unstable/session/steer`; the engine hands one only to a turn
  that asked, so it is not used.)
- Bounds: a line from goose is at most 16 MiB (past it, that goose is
  stopped), its answers to `initialize`, `session/new` and the system
  prompt are waited for 60 s, a turn's open tool calls are at most 256, and
  what it says is kept to 256 KiB.

- **What every session is told.** Its system prompt gets `HANDS` (through
  the same `system-prompt/set`, key `fragment`, before a mind's framing):
  the computer, its web, browser and computer tools, that the model reads
  no images, that `human_has_control` means its owner holds its screen,
  and that it makes its owner's apps as fragments with the `fragment` CLI,
  loading the `fragment` skill (then `apps-finite`) before any app work.
  The same bytes every turn.
- **Its tools, on our image** (`BRIDGE_GOOSE_DESKTOP`): each session's
  `mcpServers` are the agent's `browser` (`fragment-desktop mcp browser
  <agent>`), `computer` (`… mcp computer <agent>`) and `web` (`… mcp
  web`), beside a mind's `mind`: goose prefixes their tools with their
  names (`browser__browser_navigate`). They and goose's shell run on the
  agent's own display (`DISPLAY`). What they are: docs/computers.md, "Our
  images".
- **On a paired machine** (`BRIDGE_GOOSE_PLACE=machine`): the same
  `browser` and `web` when the machine has them (`BRIDGE_GOOSE_TOOLS`),
  the browser a headless Chromium of Playwright MCP's, fresh each session
  (`--isolated`), behind the same gate (no images, the same 18 tools), and
  never `computer`: a machine's hands never drive their owner's screen.
  Every session is told it works on its owner's own machine, in its own
  folder (`HANDS_MACHINE`).
- **Its skills** (`BRIDGE_GOOSE_SKILLS`, `src/runtime/skills.rs`): before
  each turn's session, the agent's skills are installed where its goose
  reads them (`<BRIDGE_GOOSE_ROOT>/<agent>/config/skills`, scratch): the
  platform skill `fragment` (the computer's page, `computer.md`, then
  what `<BRIDGE_GOOSE_CLI> skill` prints, made once a life),
  `web-search`, and the owner's managed set from their skills fragment
  (`GET /api/fragments?for=`, `GET /api/f/{skills}/files?for=` and
  `…/file?path=&for=`, as the agent acting for its owner), only what
  changed since the last install of this life fetched, adapted and
  filtered as docs/computers.md says. A turn waits for it at most 8 s
  (`INSTALL_MS_MAX`), then runs with what is there.

Tests: the mapping, pure (`src/runtime/goose.rs`); the bridge with a
scripted ACP goose on an in-process pipe (`tests/goose.rs`,
`tests/support/acp.rs`: a mind's view, framing and MCP, a step's words,
Stop, a goose that dies, a refused permission, each turn's skills
installed and adapted, a replay fetching nothing); the real goose on this
host with the scripted model and the real `fragment mcp`
(`FRAGMENT_GOOSE_BIN=… FRAGMENT_CLI_BIN=… cargo test -p fragment-bridge
--test goose -- --ignored`); and the goose image in Docker
(`the_goose_image`, tests/docker.rs).
