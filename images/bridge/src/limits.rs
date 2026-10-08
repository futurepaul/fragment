//! Every bound the bridge keeps, with its reason (docs/engineering-style.md:
//! "put a limit on everything"). A bound reached is either refused as a
//! value (a message past `QUEUED_PER_CHAT_MAX` gets a `turn.end` saying so)
//! or is a bug the bridge crashes on, as each use says.

/// A WebSocket frame (`__live`'s) is at most this many bytes: a `__live`
/// page of records is about 1 MiB (docs/api.md, Serving).
pub const FRAME_MAX_BYTES: usize = 1024 * 1024;

/// An HTTP answer the bridge reads whole is at most this many bytes: a page
/// of 1000 records is about 1 MiB, and nothing else it reads is larger.
pub const ANSWER_MAX_BYTES: usize = 2 * 1024 * 1024;

/// The agents one computer runs, at most (`GET /api/computer`). A person has
/// a handful; past this the computer's guest refuses to start rather than
/// thrash.
pub const AGENTS_MAX: usize = 32;

/// The fragments one agent follows (its chats, and its own fragment), at
/// most: each is a socket held open while awake, and the platform allows a
/// principal 8 sockets on one fragment and 32 subscriptions per fragment.
pub const FOLLOWS_PER_AGENT_MAX: usize = 200;

/// Fragments one discovery asks about (`GET /api/fragments` lists every
/// membership of the agent, apps included), at most.
pub const DISCOVER_FRAGMENTS_MAX: usize = 500;

/// Turns waiting in one chat for one agent (the one running not counted).
/// Past it a message is answered with a `turn.end` saying so: a person who
/// sends this many while one turn runs is better told than queued.
pub const QUEUED_PER_CHAT_MAX: usize = 16;

/// Turns the bridge holds open across every agent and chat, at most: the
/// state file stays small, and a computer past this is stuck, not busy.
pub const TURNS_OPEN_MAX: usize = 512;

/// A message's text handed to a runtime, at most (a record's body is at most
/// 64 KiB; a message longer than this is cut, marked with `…`).
pub const MESSAGE_TEXT_MAX_BYTES: usize = 32 * 1024;

/// A reply's text posted as one record, at most: under the platform's
/// 64 KiB body cap with room for the turn, attachments, and `to`.
pub const REPLY_TEXT_MAX_BYTES: usize = 56 * 1024;

/// A draft's text, at most (the platform caps a draft at 64 KiB).
pub const DRAFT_TEXT_MAX_BYTES: usize = 56 * 1024;

/// Drafts of one turn are sent at most this often; the platform allows a
/// fragment 10 a second, and the next draft carries the whole text anyway.
pub const DRAFT_INTERVAL_MS: u64 = 250;

/// Steps one turn posts, at most; the rest are counted, not posted.
pub const STEPS_PER_TURN_MAX: u32 = 200;

/// A turn's timing record's fields, at most this many bytes of JSON (well
/// under a record's 64 KiB: a few dozen numbers and a list a step).
pub const TIMING_MAX_BYTES: usize = 16 * 1024;

/// Replies one turn posts, at most (a runtime that sends more is looping).
pub const REPLIES_PER_TURN_MAX: u32 = 64;

/// Prompts one turn asks, at most.
pub const PROMPTS_PER_TURN_MAX: usize = 16;

/// Options one prompt offers, at most (a card has room for a few buttons).
pub const PROMPT_OPTIONS_MAX: usize = 8;

/// A prompt's text, at most.
pub const PROMPT_TEXT_MAX_CHARS: usize = 2000;

/// A step's tool name and its arguments, at most (as the in-fragment
/// agent's steps: crates/core/src/work.rs).
pub const STEP_TOOL_MAX_CHARS: usize = 140;
pub const STEP_ARGS_MAX_CHARS: usize = 140;
/// A step's result excerpt, and the model's text before it, at most.
pub const STEP_EXCERPT_MAX_CHARS: usize = 300;
/// A failed turn's error, at most.
pub const ERROR_MAX_CHARS: usize = 300;

/// Attachments one message or reply carries, at most.
pub const ATTACHMENTS_MAX: usize = 8;
/// One attachment's bytes, at most: a chat's own cap (docs/chat-records.md),
/// well under the platform's 256 MiB blob cap.
pub const ATTACHMENT_MAX_BYTES: u64 = 25 * 1024 * 1024;

/// A prompt waits this long for its answer unless the runtime says
/// otherwise. An hour: the person may be away from the chat. An open card
/// holds the computer awake, so this is also how long one unanswered card
/// keeps it awake (docs/durable-computers.md, P6 for now).
pub const PROMPT_TTL_MS_DEFAULT: u64 = 60 * 60 * 1000;
/// The note a turn after a cut one carries (note.rs), at most: its bytes
/// (its first line says what it is and what to do, so a cut keeps that,
/// and its parts below fit whole unless they are far from ASCII); what the
/// cut turn was asked, each step's tool and arguments, each card's text and
/// each reply it had said, in characters; and the steps, cards and replies
/// it names (the rest of the steps are counted).
pub const NOTE_MAX_BYTES: usize = 4096;
pub const NOTE_ASKED_MAX_CHARS: usize = 300;
pub const NOTE_TOOL_MAX_CHARS: usize = 40;
pub const NOTE_ARGS_MAX_CHARS: usize = 80;
pub const NOTE_CARD_MAX_CHARS: usize = 200;
pub const NOTE_REPLY_MAX_CHARS: usize = 200;
pub const NOTE_STEPS_MAX: usize = 8;
pub const NOTE_CARDS_MAX: usize = 3;
pub const NOTE_REPLIES_MAX: usize = 2;
/// The journal a note reads, at most: pages of this many records, back from
/// the turn's claim on `work` (to the agent's turn before it) and back from
/// the chat's tail (to that turn's start, for its replies), at most
/// `NOTE_SCAN_RECORDS_MAX` records each. A turn before it further back than
/// that is told nothing.
pub const NOTE_PAGE_RECORDS: u32 = 100;
pub const NOTE_SCAN_RECORDS_MAX: usize = 1000;
/// A note's reads are given this long in all: a turn waits for its note
/// at most this, and past it is handed without one.
pub const NOTE_READ_MS_MAX: u64 = 5_000;

/// A runtime's own prompt lifetime is honored within these bounds.
pub const PROMPT_TTL_MS_MIN: u64 = 10 * 1000;
pub const PROMPT_TTL_MS_MAX: u64 = 24 * 60 * 60 * 1000;

/// A running turn that hears nothing from its runtime this long is ended as
/// an error.
pub const TURN_IDLE_MS_MAX: u64 = 15 * 60 * 1000;

/// A message an agent writes to another agent (`to`) is answered only this
/// many hops deep, so two agents handing off to each other stop. The bridge
/// that answers counts the hops itself (engine.rs, `hop_of`): never fewer
/// than the record claims, and for an agent of its own computer from the
/// turn that agent is in, so a post made outside the bridge (the CLI, the
/// API) resets nothing.
pub const HOPS_MAX: u32 = 3;

/// Turns agents start of each other in one chat (a record an agent posts
/// whose `to` names an agent), at most this many in `AGENT_TURNS_WINDOW_MS`
/// by the causing records' own times (the platform's clock, so every life
/// counts alike). Past it the turn is refused, and its end says why in the
/// chat. A backstop under `HOPS_MAX`: a hand-off fanned out to several
/// agents multiplies at each hop, and an agent of another computer is held
/// only by the hop it claims. A person's message is never counted.
pub const AGENT_TURNS_PER_CHAT_MAX: usize = 20;
pub const AGENT_TURNS_WINDOW_MS: i64 = 5 * 60 * 1000;
/// Chats whose agent turns are counted at once, at most (the state keeps
/// them; past it the chat counted least lately is let go).
pub const AGENT_TURN_CHATS_MAX: usize = 256;

/// A turn of this computer's that ended is remembered this long, at most
/// this many at once (never written to `/data`): its agent's reply, read
/// after the turn was let go, is one hop past it. One read later than that
/// (a follower that was down, a restart) counts as said outside any turn.
pub const ENDED_HOPS_MS: u64 = 5 * 60 * 1000;
pub const ENDED_HOPS_MAX: usize = 256;

/// Reconnects wait a jittered backoff (lesson 5): from this, doubling, up to
/// the max, each wait a uniform 0.5–1.5 of it.
pub const RECONNECT_MS_MIN: u64 = 1_000;
pub const RECONNECT_MS_MAX: u64 = 30_000;

/// An HTTP call to the fragment API is given this long.
pub const HTTP_TIMEOUT_MS: u64 = 15_000;
/// The bridge looks at the platform's hold this often, to answer it (the
/// platform waits 20 s for the answer: docs/computers.md).
pub const HOLD_POLL_MS: u64 = 100;
/// An answer to the hold (what the save may leave out) is at most this
/// long: the platform reads it whole, and refuses a longer one.
pub const HELD_ANSWER_MAX_BYTES: usize = 2048;
/// Patterns an answer names at most, each at most this long.
pub const HELD_PATTERNS_MAX: usize = 16;
pub const HELD_PATTERN_MAX_BYTES: usize = 128;
/// A WebSocket's connect and its first frame are given this long.
pub const WS_OPEN_TIMEOUT_MS: u64 = 10_000;
/// A `__live` socket pings this often, and is given up on after twice that
/// with nothing heard.
pub const LIVE_PING_MS: u64 = 30_000;

/// A post the platform did not take is tried this many times, waiting a
/// jittered backoff between (ids make every retry the same record).
pub const POST_TRIES_MAX: u32 = 10;

/// A catch-up reads at most this many pages (1000 records each) before it
/// goes live; `__live`'s own paging reads the rest.
pub const CATCHUP_PAGES_MAX: u32 = 20;
/// A page of records asked for (the platform's own page size).
pub const CATCHUP_PAGE_RECORDS: u32 = 1000;

/// How long a chat's members (the lead, the agents in it) and its writers'
/// names are reused before they are read again (sooner when one of the
/// computer's agents joins it).
pub const VIEW_TTL_MS: u64 = 30_000;

/// Joins of the computer's agents to fragments remembered at once, each for
/// a view's life (`VIEW_TTL_MS`): past it they are forgotten, and a view
/// may be stale until its life ends.
pub const JOINS_HELD_MAX: usize = 1024;

/// A `tasks` record (a routine firing) older than this when first read is
/// history: a routine missed while the computer could not wake is skipped,
/// as cron skips a missed run.
pub const TASKS_BACKLOG_MS: u64 = 60 * 60 * 1000;

/// `GET /api/computer` is read again this often while awake, so an agent
/// assigned to the computer meanwhile is followed within it.
pub const COMPUTER_EVERY_MS: u64 = 60 * 1000;

/// The agent's fragments are listed again this often while awake (a new
/// chat is also announced on its `tasks` channel, which is immediate).
pub const DISCOVER_EVERY_MS: u64 = 5 * 60 * 1000;

/// SIGTERM means stop now (docs/computers.md): the bridge is gone within
/// this, under the platform's 5 s.
pub const SHUTDOWN_MS_MAX: u64 = 3_000;

/// The state file is at most this many bytes (cursors and open turns only).
pub const STATE_FILE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The engine's inbox holds at most this many inputs; a follower waits for
/// room (back pressure, never a drop).
pub const INBOX_MAX: usize = 1024;

/// Effects queued for one fragment, at most; past it the producer waits.
pub const LANE_MAX: usize = 4096;

// Design checks that run before the program does.
const _: () = assert!(REPLY_TEXT_MAX_BYTES < 64 * 1024);
const _: () = assert!(DRAFT_TEXT_MAX_BYTES <= 64 * 1024);
const _: () = assert!(MESSAGE_TEXT_MAX_BYTES <= REPLY_TEXT_MAX_BYTES);
const _: () = assert!(RECONNECT_MS_MIN < RECONNECT_MS_MAX);
const _: () = assert!(PROMPT_TTL_MS_MIN <= PROMPT_TTL_MS_DEFAULT && PROMPT_TTL_MS_DEFAULT <= PROMPT_TTL_MS_MAX);
const _: () = assert!(SHUTDOWN_MS_MAX < 5_000, "the platform kills the guest 5 s after SIGTERM");
const _: () = assert!(QUEUED_PER_CHAT_MAX < TURNS_OPEN_MAX);
const _: () = assert!(LIVE_PING_MS > HTTP_TIMEOUT_MS);
const _: () = assert!(NOTE_MAX_BYTES < MESSAGE_TEXT_MAX_BYTES, "a note is small beside the message it comes with");
const _: () = assert!(NOTE_PAGE_RECORDS as usize <= NOTE_SCAN_RECORDS_MAX && NOTE_PAGE_RECORDS <= CATCHUP_PAGE_RECORDS);
const _: () = assert!(HOPS_MAX >= 1, "an agent's record is one hop at least");
const _: () = assert!(AGENT_TURNS_PER_CHAT_MAX > HOPS_MAX as usize, "the budget is a backstop, past one chain of hand-offs");
const _: () = assert!(AGENT_TURNS_WINDOW_MS > 0 && ENDED_HOPS_MS > 0);
