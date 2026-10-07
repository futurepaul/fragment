# Durable turns: what Pi Durable teaches our computers

Exploration, 2026-10-05, read against master at `d4c2d09`. Written for
the agent who implements it and the one who reviews it: it carries the
motivation, where each claim comes from, and the tests that would prove
each failure and each fix. It is the analysis, as of `d4c2d09`:
docs/durable-computers.md is the design of record built from it, and
says which proposals are built (P1, P2 and P5 among them). Where this
file disagrees with that one or with docs/cloudflare-v1.md, they win;
the decisions this would change are listed under "Decisions that are
Paul's".

How each claim was checked is said beside it:

- **run**: reproduced on master (the test is in this file);
- **read**: read in the code, or in a vendor's own types, at the place
  named;
- **inferred**: follows from what was read; not seen happening;
- **unknown**: needs a vendor's answer or a hosted run.

Line numbers are master's at `d4c2d09`; function names are given too,
since lines move.

## The short version

Pi Durable (Earendil, 2026-10-01) makes an agent's conversation durable
by journaling every step, and leaves the agent's machine alone. Our
computers do the opposite: they save the machine (`/data`) and drop
whatever turn was in flight. Each half is missing what the other has.

Paul's frame for this work (2026-10-05): we cannot have five competing
sources of truth; the system has to be deterministic; and the lessons
are worth taking because they simplify it.

The one idea underneath every proposal here:

> **One authority for each fact. Everything else is a cache, and a
> cache may be lost or go back in time without changing what happens.**

Today `/data` is trusted to say which turns have run, and `/data` is the
one store that goes back in time. A computer that crashes wakes with the
`/data` of its last sleep, and its bridge runs again every turn since
(run: "F2", below). Nothing saves `/data` while a computer is awake
(read: "F1").

What to do about it, in three sentences:

1. A turn runs only in the life of the bridge that wrote its
   `turn.start` to the chat's `work` channel, which never goes back in
   time (P1).
2. `/data` has one kind of saved copy, taken between turns with its
   writers held; a wake restores the newest one that opens; the
   container snapshot is a cache of it, and Litestream goes (P2–P4).
3. What a restart cut short is told to the model, and an approval asked
   before it can still be answered after it, both from the journal
   (P5, P6).

Once (1) holds, restoring an older `/data` is always safe, and every
question of the form "which copy is newest, and is it whole?" becomes
"restore the newest save that opens". That is where the simplification
comes from.

## Words

- **life**: one run of the guest's processes, from a container start to
  its end. The Computer DO's `generation` counts container starts and
  the bridge's `boot` counts loads of its state file; a life is what
  both mean.
- **save**: a copy of `/data` the Computer DO can restore. Today that is
  two things, the `DirectoryBackup` record and the container snapshot.
- **rollback**: a wake whose `/data` is older than the end of the life
  before it. Today every crash is one.
- **journal**: a chat's `work` channel (docs/chat-records.md): each
  turn's start, steps, prompts and end, kept by the Fragment DO, outside
  the computer.
- **claim**: a `turn.start` record, read as "this life runs this turn".
- **run**: a runtime being given a turn (`Command::Start` reaching
  Hermes or the scripted agent). A run is what costs money and has
  effects; a record is only what it left behind.

## Pi Durable, as far as it matters here

Source: <https://earendil.com/posts/pi-durable/>, and `packages/durable`
in github.com/earendil-works/pi (its README and thirty examples). It is
a harness in TypeScript, about 15,000 lines, marked experimental. It
replaces an agent's loop, so it is no drop-in for Hermes. What carries
over is its rules:

| Its rule | In its words | Ours today |
|---|---|---|
| Every step checkpoints before it moves on | "every step of a run is a task that stores a checkpoint before it moves on. If the process dies, a new process opens the same storage, finds the unfinished tasks, and continues each one from its last checkpoint." | The bridge writes `state.json` before a step's effects: the same idea, in a store that goes back in time. |
| Intent is stored before the effect | "Every tool call runs as its own durable task, and its intent is stored before it runs." | `turn.start` is posted beside the hand-off, not before it, and nothing reads it back. |
| Only what says it is safe is run again | "After a crash, a tool reruns only if it says that is safe. Otherwise the model is told the call was interrupted, with the output stored so far, and decides what to do." | A cut turn ends as an error the person sees. The model is told nothing. |
| A repeat by id is the same thing | "A requestId makes a submission exactly-once, so a client that retries after a crash gets the original submission back instead of asking twice." | Yes: turn ids, post ids, meter references. |
| A decision is a memo | "a hook that makes a decision stores it in a memo: a small value stored with the task, where the first write wins." | `turn.prompt.closed` is first-write-wins, but its turn dies with its life. |
| State commits with the transcript | "changed in the same atomic commits, so the state never disagrees with the transcript that produced it." | Five stores, at five points in time. |
| One owner of a storage | "One process owns a storage at a time, and other clients attach to that process." | Generations fence container starts. Fine. |
| Names, never code | "Conversations store extension and tool names, never code, so after a restart they pick up whatever the new process installs." | An image pin; profiles written whole at each boot. Fine. |

What Pi Durable does not cover is the files and processes of the
machine its tools run on: a tool marked safe to run again assumes its
machine is still there. That half is ours, and it is the half we already
have. The two fit together: the journal says what happened, and the save
says what the disk looked like at a known point in it.

## What we have today

### The stores

Everything that holds a part of a computer's state. The first six each
answer "what has this agent done, and what does it know?", as of a
different moment:

| Store | Holds | Written | Lives in | Goes back in time? |
|---|---|---|---|---|
| Channels `chat`, `work`, `tasks` | what was said; each turn's start, steps, prompts, end | as it happens, each record by its id | the Fragment DO | no |
| The agent fragment's repo | `SOUL.md`, `memories/`, `skills/` | `hermes-boot`'s sync, every 60 s (`SYNC_EVERY_MS`), three ways | code.storage | no |
| `/data`, the live disk | Hermes' `state.db` per profile with its `sessions/`, `workspace/`, `home/`; `/data/bridge/state.json` (cursors, open turns); `/data/hermes-sync/*.json` (the sync's merge bases); the managed skills | all the time | the container | it is gone at every stop |
| The backup | `/data` as one `.tar.zst` | at sleep only | R2, `computers/<id>/backups/`; one is kept | it is what `/data` goes back to |
| The snapshot | the container's files, `/data` among them | at sleep only, after the backup | the image's registry, 30 days | the same |
| Litestream's replicas | each profile's `state.db` | every 10 s | R2, `litestream/<profile>`, through the storage intercept | never read |
| The Computer DO | the lifecycle, the backup's and the snapshot's records, its agents, uses, unsent awake time | | its own SQLite | no |
| The ledger | usage, each row by its reference | | its own DO | no |

### A sleep and a wake, as coded

A sleep (`ComputerCell::sleep`, cell/src/computer.rs:633), in order:

1. `DirectoryBackup.backup` of `/data`, the guest still running. If it
   works, its record is stored and the one before it is deleted
   (`forget`). If it fails, a note is written and the sleep goes on.
2. `snapshotContainer`, unless `FRAGMENT_COMPUTER_SNAPSHOTS=off` (local
   workerd takes none). If it fails, the snapshot's record is forgotten.
3. SIGTERM, then up to 5 s for the guest to exit (`EXIT_WAIT_MS`).
4. `destroy`.

A wake (`try_start`, cell/src/computer.rs:590) starts from the snapshot
when its image reference equals the pinned image's. Otherwise it starts
the image with `RESTORE_PENDING=1`, restores the backup's record, and
touches `/run/computer/restored`.

A crash is `Event::Exited` (crates/core/src/computer.rs, `apply`). The
container is gone, and its disk with it. `gone()` starts it again when
something still wants it, and that start takes the same snapshot or
backup any wake takes: the last sleep's.

The lifecycle's actions are `Start`, `Sleep` and `Meter`
(crates/core/src/computer.rs, `Action`). No action saves.

### What a restart does to a turn

- The bridge loads `state.json` and `Engine::recover`
  (images/bridge/src/engine.rs:255) goes through its open turns: a
  queued one stays queued; one handed to the runtime but not taken is
  handed again; one the runtime had, or one waiting on a person, is
  ended (`error: "lost when the computer restarted"`, its cards
  `expired`).
- It then catches up each followed channel from its cursor
  (images/bridge/src/driver.rs:582; no cursor means from 0) and follows
  it live.
- `hermes-boot` (`end_previous_life`, images/hermes/boot/src/main.rs:605)
  writes Hermes' clean-exit receipt, so Hermes discards the turns that
  were cut rather than resuming them, and clears Hermes' leases, whose
  PIDs name live processes again. `HERMES_AUTO_CONTINUE_FRESHNESS=1`
  (hermes.rs, `gateway_env`) turns off Hermes' own recovery notes.

### What already holds, and should be kept

- **Ids make a repeat the same thing.** A turn's id is a hash of the
  record that caused it; a post's id is `wk:<turn>:<part>` or
  `rp:<turn>:<n>`; a meter row has its reference. The platform answers
  the same id and body again as a replay and another body as 409
  (docs/api.md, `POST /api/f/{name}/channels/{channel}`).
- **State before effects.** The bridge writes `state.json` whole, synced
  and renamed, before the effects of the step that changed it.
- **The wake is crash-only.** `/data` is saved before the guest is
  signalled, so every wake is an unclean exit, and there is one recovery
  path, run at every wake. Do not add a clean-shutdown path that only
  some wakes take.
- **Generations.** A late answer from an earlier start changes nothing
  (crates/core/src/computer.rs, cell/entry.mjs `ContainerHost`).
- **The repo sync survives a rollback.** It merges three ways (main, the
  profile, and what both agreed on at the last sync). A profile that
  went back in time equals its merge base, so main's newer files win
  (images/hermes/boot/src/sync.rs). (read)
- **Lesson 1 of docs/cloudflare-v1.md:** the bridge owns nothing. The
  proposals below finish that sentence: today it owns the one copy of
  "which turns have run".

## How it goes wrong

### F1. `/data` is saved only when a computer sleeps (read)

Decision 18 says `/data` "is saved with `DirectoryBackup` every few
minutes while written". The only `backup` call is in `sleep()`
(cell/src/computer.rs:636), and the lifecycle has no action that saves.
So:

- a crash loses everything since the last sleep, and the next start is a
  rollback to it;
- an always-on computer (`always_on`, which `wanted()` always honors)
  never sleeps, so it is never saved, unless its owner asks for a sleep;
- a computer that has never slept and crashes wakes with an empty
  `/data`.

What survives a crash today: the channels, what the sync has pushed to
the agent's repo (it runs every minute), and Litestream's replicas,
which nothing reads (F8).

### F2. A `/data` that goes back in time runs turns again (run)

The bridge's cursors are in `/data/bridge/state.json`. After a rollback
they are behind, the catch-up reads the records after them again, and
`Engine::record` admits each as a new turn: nothing asks the journal
whether the turn already ran. The runtime is given the turn again. That
much was run, with the scripted agent. What follows for Hermes is
inferred: its `state.db` went back with the cursor, so it does not know
the message either; its model calls are paid again and its tools run
again (an email is sent twice).

The person sees nothing. The second run's `turn.start` and `turn.end`
have the ids and bodies of the first, so they are replays. Its reply has
the first reply's id: with the same text it is a replay, and with other
text it is a 409, which the bridge logs and drops
(images/bridge/src/driver.rs:832). So the records look right and a test
that counts records passes. The existing checks ("its restored /data
knew what it had answered: nothing twice", crates/e2e/src/lanes/computers.rs;
`catch_up_after_a_restart_without_a_duplicate`,
images/bridge/tests/bridge.rs) count replies and `turn.start` records,
and restore the state the bridge last wrote, so they cannot see this.

Only routines have a guard: a `tasks` record older than an hour when
first read is skipped (`TASKS_BACKLOG_MS`). A chat message has none. Up
to 16 turns a chat wait at once (`QUEUED_PER_CHAT_MAX`), and a message
past that is refused, so a long rollback runs its first old turns and
refuses the rest as they pile up behind them.

The reproduction, appended to images/bridge/tests/bridge.rs and run from
`images/` with `cargo test -p fragment-bridge --test bridge twice` (it
is not committed; P1's tests replace it):

```rust
/// Goal (I3): a turn that started and ended is never run again, whatever
/// state the bridge wakes with. Answers the posts the restored bridge
/// made again, on `work` and on `chat`.
async fn rollback_case(lose_it_all: bool) -> (usize, usize) {
    let fake = Fake::start("127.0.0.1:0", &["juniper"]).await;
    let chat = fake.chat("talk", &["juniper"]);
    let dir = support::dir("rollback");
    let cfg = support::config(&fake.url(), &dir, support::settings());
    let state = dir.join("bridge").join("state.json");
    let ended = |w: &World, turn: &str| w.bodies(&chat, "work", "turn.end").iter().any(|e| e["turn"] == turn);
    let posts = |w: &World, ch: &str| w.calls.iter().filter(|c| c.as_str() == format!("POST /api/f/{chat}/channels/{ch}")).count();

    let bridge = support::start(cfg.clone(), support::script());
    following(&fake, 2).await;
    let one = fake.say(&chat, &person("paul"), json!({ "text": "one" }));
    let t1 = turn_of("juniper", &chat, seq(&one));
    fake.until(WAIT, "one's end", |w| ended(w, &t1)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // the save: /data as it is after turn one
    let saved = std::fs::read(&state).expect("a state file");

    let two = fake.say(&chat, &person("paul"), json!({ "text": "two" }));
    let t2 = turn_of("juniper", &chat, seq(&two));
    fake.until(WAIT, "two's end", |w| ended(w, &t2)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let before = fake.with(|w| (posts(w, "work"), posts(w, "chat")));
    bridge.stop().await;
    fake.until(WAIT, "its sockets closed", |w| w.live_sockets() == 0).await;

    // the crash's restore: /data goes back to the save, or is gone
    if lose_it_all { std::fs::remove_file(&state).expect("removed") } else { std::fs::write(&state, saved).expect("restored") }
    let bridge = support::start(cfg, support::script());
    following(&fake, 2).await;
    tokio::time::sleep(Duration::from_millis(2000)).await;
    let again = fake.with(|w| (posts(w, "work") - before.0, posts(w, "chat") - before.1));
    bridge.stop().await;
    again
}

#[tokio::test]
async fn a_rollback_runs_nothing_twice() {
    assert_eq!(rollback_case(false).await, (0, 0), "posts made again, on work and on chat");
}

#[tokio::test]
async fn a_lost_state_runs_nothing_twice() {
    assert_eq!(rollback_case(true).await, (0, 0), "posts made again, on work and on chat");
}
```

On master, with nothing new said after the restore:

| `/data` after the crash | Posts made again | Meaning | Replies | `turn.start` records |
|---|---|---|---|---|
| back to the save after turn one | 2 on `work`, 1 on `chat` | turn two ran again | 2, as before | 2, as before |
| gone | 4 on `work`, 2 on `chat` | both turns ran again | 2, as before | 2, as before |

The same test first counted drafts and undercounted (one turn's drafts
were replaced by later ones before they were sent): a draft count is not
a run count. See "Count runs, not records".

### F3. The guest is live after its save (inferred)

The save comes first in a sleep, then the snapshot (5–6 s, spike S3b),
then SIGTERM. For those seconds the bridge still follows its channels.
A message that arrives then is admitted, handed to the runtime and
perhaps begun, and then the container is destroyed. The wake that
message causes (the newest push wins) restores a `/data` from before it:

- from the backup, the turn is unknown and runs again from its start
  (F2, for one turn);
- from the snapshot, whatever `state.json` said when the snapshot read
  it: a turn the runtime had is ended as an error; one only handed is
  handed again.

The snapshot is taken after the backup with the guest writing between
them, so the two saves of one sleep can differ.

### F4. The backup is a hot copy (read, the vendor's types)

`@cloudflare/sandbox` 1.0.0, `DirectoryBackup.backup`
(`dist/index.d.mts`): "Backs up `dir` and returns its record. Pause
writers in `dir` first: files that change while it's read are captured
as they are at that moment." Nothing pauses a writer before `sleep()`
calls it. A SQLite database and its WAL read at different instants can
make a database that does not open, or opens to an older state than its
neighbors. An idle sleep is mostly quiet, so the risk is low there; it
is real for an owner's sleep during a turn, and for any save added while
awake to fix F1.

Other facts from the same types, useful below: a backup is one
`.tar.zst`, its SHA-256 "checked on every restore"; a restore is
"extracted beside the target and swapped in only after the download is
verified"; one backup or restore runs at a time per container; and "the
application stores the returned records and decides when to delete
them", so a backup never expires on its own.

### F5. One backup is kept, so a save that does not open has nothing behind it (read; its effect inferred)

`sleep()` deletes the backup before the new one as soon as the new one's
record is stored. If the newest does not open (F4), every start restores
the same bytes. If Hermes will not start on them, `hermes-boot` exits
when its gateway does, the container exits, and three such starts make
the computer "won't wake"; its owner's wake is a fourth of the same.

### F6. A save that fails does not stop the destroy (read)

`sleep()` on a failed backup writes `MetaKey::Note` and goes on to the
snapshot, the signal and the destroy (cell/src/computer.rs:644). With
snapshots off, or the snapshot failing too, the container is destroyed
with `/data` unsaved, and the next wake is a rollback to the save before.
With the snapshot working, the loss is deferred: the next wake is fine,
until the image's reference changes (a deploy that moves it, or a pin),
when the wake restores the older backup.

### F7. A snapshot that will not start has no fallback, and is not tied to a save (read; the vendor's behavior unknown)

The snapshot's record is `{id, image}`. `start()` and `try_start()`
never forget it when a start from it fails, so three failures make
"won't wake", and the owner's wake tries the same snapshot. Snapshots
are kept 30 days (docs/cloudflare-v1.md, "Snapshot facts"); what
Cloudflare answers for one past that is unknown. A computer asleep for a
month may be the first to find out. The backup it could fall back to is
there, and does not expire on its own.

### F8. Litestream's replicas are written and never read (read)

`hermes-boot` runs `litestream replicate` (main.rs, `start_litestream`).
Nothing in the repo runs `litestream restore`, and decision 18's
"CI runs a restore drill into an empty computer" does not exist. If a
restore were added, it would put a `state.db` from seconds ago into a
`/data` from the last sleep: a disk from two points in time, beside a
bridge state from the older one.

### F9. An approval card promises an hour and has about twenty minutes (read)

A prompt's life is an hour by default (`PROMPT_TTL_MS_DEFAULT`), and its
card says so (`expiresAt`). A turn waiting on a person does not hold the
keepalive (engine.rs, `finish`: only queued, handed and running turns
do), the computer sleeps 20 minutes later (`IDLE_MS`), and at the next
start `recover()` ends the turn and marks its card `expired`.
docs/chat-records.md says as much ("Hermes cannot resume a turn across a
restart"). It is the gap Pi Durable's memo closes.

*Bit on p5, 2026-10-05, and closed for the idle sleep:* the cut turn's
message stays the last of Hermes' session, unanswered, and Hermes folds
the next message into it, so the model was given `[paul] do the risky
thing\n\n[paul] good morning` and asked the cut command's approval
again: every message after a missed card met the card again, and the
computer slept under each. An open card now holds the keepalive (P6 for
now; docs/bridge.md, "A card keeps its computer awake"). Any other
restart under a card (an owner's sleep, a crash, a deploy) still cuts its
turn; F10 is the same fold, for any cut turn, and P5 closed it
(2026-10-06): the next message is answered, told what was cut.

### F10. The model is never told a turn was cut (read)

The person gets `turn.end` with an error. Hermes is made to forget: the
clean-exit receipt discards the cut turn's markers and its recovery
notes are off, for a good reason (a resumed turn would carry the old
message id and fold the person's next message into it, while the bridge
has already ended it). So the next turn's model does not know it sent
two of five emails, or left a file half written. Worse (run on the
real image, 2026-10-05): Hermes persists a turn's message as it starts
and the rest as it ends, so a cut turn leaves its message the session's
last, and the next message is folded into it. The model is handed the
cut request again, as if it were new, beside the next message, and does
it again (the scripted model does; what a real model does is a hosted
run's to see).

*Closed by P5 (2026-10-06):* Hermes v0.21.5 joins the two in
`_merge_consecutive_users` (agent/agent_runtime_helpers.py), its
pre-call repair, and in `get_messages_as_conversation(repair_alternation=
True)` as the gateway loads a transcript; no Relay field changes that, and
Hermes closes an unanswered tail only for a turn that fails in its own
process (its failed-turn boundary, `agent/turn_failure_copy.py`). So the
image's boot writes that same boundary, through Hermes' session code, to
each relay session a restart left unanswered, before the gateway starts;
and the bridge tells the next turn what was cut, from the journal (P5,
below; docs/bridge.md). Proven with real Hermes:
`a_turn_cut_by_a_restart_is_closed_and_told` (tests/docker.rs).

### F11. Smaller ones

- **An agent's unprompted message can vanish after a rollback** (read;
  not run). Its turn id is `said_turn_id(agent, n)` with `n` a counter
  in `state.json` (engine.rs:647, records.rs:240). After a rollback `n`
  repeats, the reply's id exists with other text, the post is a 409, and
  the bridge drops it, until the counter passes its old mark.
- **A crash late in a long turn is not started again** (read). `gone()`
  zeroes the sockets before it asks `wanted()`, and a turn that has run
  past `IDLE_MS` since its record has no hold left. The computer stays
  asleep, the turn has no `turn.end`, and its chat shows it working
  until something else wakes it.
- **A slow save can be asked for twice** (inferred, narrow). A sleep
  past `SLEEP_DEADLINE_MS` (60 s) is asked for again. An idle sleep runs
  in the alarm handler, which cannot overlap itself. An owner's sleep
  runs in a request, so the alarm's second sleep of the same generation
  can run beside it once the backup takes over a minute (spike S3: 52 MB
  in 3.3 s, so about 1 GB). The SDK queues the second backup; the first
  sequence's destroy then fails the second's backup and snapshot, which
  forgets the good snapshot's record. Nothing is lost; the next wake is
  the slow one. "Restore time for a large `/data`" is already an
  unproven path in the plan.

## The direction: one authority for each fact

### The authority map

| Fact | Its one authority | Everything else |
|---|---|---|
| What was said | the chat's `chat` channel | Hermes' session transcript is the model's context, derived, Hermes' own |
| Which turns have started, what they did, how they ended, and each prompt's answer | the chat's `work` channel (the journal) | the bridge's `state.json` is a cache: where to resume reading, and what this life has in hand |
| The agent's self (`SOUL.md`, memories, skills) | the agent fragment's repo | the profile's checkout is a working copy |
| The model's context and the agent's working files | `/data`, as of its newest save | the snapshot is a cache of that save for one image; nothing else holds a copy |
| Which save is current, and what a wake restored | the Computer DO | |
| Usage | the ledger | |

Two stores go: Litestream's replicas (P4), and the snapshot as a save in
its own right (P3). One role goes: `/data` as the authority on which
turns ran (P1).

### What "deterministic" means here

These are the invariants. Each is a test, and the tests are the
definition of done whatever mechanism is chosen.

- **I1. One life per turn.** A turn id has at most one `turn.start` on
  `work`, and a runtime is given a turn only by the life that wrote it.
- **I2. Every started turn ends once.** Each `turn.start` gets exactly
  one `turn.end`, written by its own life or by a later one that finds
  it open.
- **I3. `/data` is a cache of the journal.** Losing `/data`, or
  restoring any earlier save of it, causes no run, no model call, no key
  call and no meter row for a turn that had started.
- **I4. Nothing said is dropped.** A message no life has started is run
  by the next life, after any sleep, crash or rollback.
- **I5. A sleep never throws `/data` away.** A container whose save
  failed is not destroyed by the sleep (within a bound Paul sets).
- **I6. One restore rule.** A wake restores the newest save that opens.
  A snapshot is used only when it is of that save and of the pinned
  image, and a start from one that fails falls back within the same
  wake.
- **I7. A bounded loss.** While a computer is awake, `/data` is never
  more than one turn (or a set number of minutes) ahead of its newest
  save.
- **I8. A save opens.** A save is taken with its writers held, and a
  restored `/data` is checked before the guest takes a turn.
- **I9. The same guest, whichever way it woke.** A wake from the
  snapshot and a wake from the image and the save leave the same `/data`.

## Proposals

P1 is the one that makes the rest safe; it touches only the bridge and
docs/chat-records.md. P2 to P4 are the platform's. P5 and P6 build on
the journal P1 makes authoritative.

### P1. A turn runs only in the life that claimed it

The rule: **a turn is started by exactly one life, and the proof is its
`turn.start` on `work`.** This is Pi Durable's "intent is stored before
it runs", in the store we already have that cannot go back in time, and
using the compare-and-set the platform already offers: the same id and
body is a replay, the same id and another body is 409.

The shape:

- Each bridge process makes a random `life` at its start (128 bits). It
  is never written to `/data`.
- `turn.start` carries it: `{"kind": "turn.start", …, "life": "<hex>"}`,
  under the id it has today, `wk:<turn>:start`.
- The runtime is given the turn only once that post is answered. Today
  `pump` (engine.rs:473) emits the post and `Command::Start` in one
  step, and the driver sends them down separate lanes
  (driver.rs, `act`). The answer decides:
  - appended: this life owns the turn, so start it;
  - a replay: this life's own retry had landed, so start it, once;
  - 409: another life started it. Never start it. Post its `turn.end`
    (`error: "lost when the computer restarted"`), which is a 409 of its
    own when it had ended, and forget the turn;
  - no answer after the retries: start nothing, keep it queued, and try
    again. A claim that cannot be confirmed fails closed.
- At a start, every turn the state holds that is not queued belongs to
  an earlier life and is ended as lost. Queued turns stay queued and are
  claimed like any other.

Why `life` is in the body: without it a second life's `turn.start` is
byte-for-byte the first's and is answered as a replay, which is also
what a life's own retry after a lost response gets. With it, the same
life's retry is a replay and another life's is a 409. A persisted claim
would not do: a save taken after the claim and restored later would
carry it, and that life would run the turn again.

Why this is enough after a rollback, with no scan of the journal: a
state that does not know a turn is older than the turn's admission, and
the cursor passes a record at its admission, so the rolled-back cursor
is before the record that caused the turn. The catch-up reads it again,
the claim is a 409, and the turn is ended if nobody ended it. A turn the
old state does hold is either queued there, and its claim is a 409, or
past queued, and the start ends it.

What it costs, and what goes with it:

- **"Handed, never taken, is handed again" goes.** Today a turn given to
  the runtime but not yet accepted is given again after a restart
  (`Phase::Handed`, `Event::Accepted`). After a rollback a life cannot
  tell "handed, never taken" from "taken, run, crashed" from its state,
  and the journal has only the start. The simple and exact rule is one
  life per turn: a life that ends between its claim and the turn's end
  loses the turn, and the person sends it again. The engine test
  `a_restart_starts_nothing_twice` ends by asserting the old rule, and
  that assertion changes on purpose. Within a life, `Handed` and
  `Running` may then be one phase.
- **Claim when the runtime can take it.** `hermes-boot` starts the
  bridge just before Hermes' gateway, so the bridge has read the message
  that woke its computer seconds before Hermes dials in. If the turn
  were claimed then and the start failed (a start is tried up to three
  times), the first message after a wake would end as lost. So a turn
  is claimed when its runtime is connected, and until then it is queued,
  unclaimed, and any later life can run it. The relay runtime keeps a
  message "until Hermes acks it, handed again on each dial"
  (docs/bridge.md) within one life; that stays.
- **A sleep must stop claims before its save (F3).** Without that, P1
  turns F3's second run into a turn the dying life claimed and the next
  life must end: a lost message where there was a double run. P2's hold
  closes it. P1 should ship with at least the simple form: the Computer
  DO marks the sleep before the backup, and the bridge claims nothing
  once it sees the mark.
- **A turn that ends without starting has no claim.** A turn refused
  ("too many messages are waiting") or stopped while queued gets a
  `turn.end` and no `turn.start`, so after a rollback it could run,
  late. Either every turn gets both records (a refusal posts its start,
  then its end), or this is accepted and written down. The first makes
  I1 and I2 hold for every turn with no exception; check what
  `templates/chat` draws for it.
- **The lost-state case is slow, not wrong.** With `/data` gone the
  catch-up reads each chat from 0 (20 pages of 1000 at most), and every
  old turn costs one 409. A floor makes it one read: before catching up
  a chat, read the tail of its `work` (the channel list gives its `seq`),
  take the highest `cause.seq` among this agent's `turn.start` records,
  and start from the larger of that and the cursor. Turns of an agent in
  a chat are claimed in order, so everything at or before the floor was
  decided by an earlier life. It is an optimization; the claim is what
  keeps I1.
- **Unprompted messages take the life into their id** (F11):
  `said_turn_id(agent, life, n)`, so a counter that went back collides
  with nothing.

What this deletes: `/data` as an authority. `state.json` can be lost, or
restored from any save, and I1 to I4 still hold. The questions "is this
backup consistent with that replica?" and "which is newer?" stop
mattering for turns.

Docs to change with it: docs/chat-records.md (`turn.start`'s `life`, and
the sentence on what a restart does), docs/bridge.md ("State, and what
it never does twice"), and lesson 2 of docs/cloudflare-v1.md.

### P2. One kind of save, taken between turns with writers held

- **A save** is a `DirectoryBackup` of `/data` with its writers held (the
  SDK's own condition, F4), kept by the Computer DO with a number, a
  time, the generation it is of, and whether the guest was held. The DO
  keeps the last few (three, say), newest first, and deletes the oldest
  when a new one is stored. P1 is what makes restoring an older one
  safe.
- **When.**
  1. When work ends: the last keepalive socket closes while awake
     (`Event::Closed` with none left), after a short settle so turns
     back to back do not each save.
  2. At sleep.
  3. Later, on a timer while something has held it awake since the last
     save, as a backstop for a computer that never goes idle.

  Saving between turns is simpler than saving every few minutes, and
  also more coherent. Under P1 a crash during a turn loses the turn
  whatever was saved, and a `/data` from before the turn is a cleaner
  place to wake than one with half its files written. What a restore
  cannot undo is the turn's effects outside the computer, and P5 is for
  those. An always-on computer is covered by (1).
- **Holding the writers.** Only the image knows its writers. The
  contract (docs/computers.md, "What every image carries") gains a hold:
  the DO asks, the guest stops claiming turns and stops writing `/data`,
  and says so; the DO saves; the DO lets go. A marker pair shaped like
  the restore gate would need nothing an image does not already carry
  (`sh`, `touch`): the DO touches `/run/computer/hold`, waits a bounded
  time for the guest's `/run/computer/held`, saves, and removes both. An
  image that never answers is saved anyway, and its save is recorded as
  not held. Ours:
  - the bridge claims nothing while held, and writes `held` only with no
    claim in flight;
  - `hermes-boot` pauses its sync, its skills install and Litestream
    (while it exists), and checkpoints each `state.db`
    (`PRAGMA wal_checkpoint(TRUNCATE)`);
  - Hermes' gateway cannot be asked to pause. Between turns it is idle.
    For a save during a turn (the timer), stopping it for the copy
    (`SIGSTOP`, `SIGCONT`) gives what a crash would leave, which SQLite
    is built to open; measure the pause before relying on it.
- **A sleep** becomes: hold, save, snapshot, SIGTERM, destroy. The guest
  is held from before the save until it is gone, which closes F3.
- **A message that arrives as a sleep begins** is either claimed by the
  old life, which then is not put to sleep (a keepalive that opens
  during the hold cancels the sleep), or left unclaimed for the next
  life. It is never claimed and then killed. This is a new edge in the
  pure lifecycle and belongs in its interleaving test.
- **A save that fails** (I5): try again with a pause, and do not destroy.
  When it keeps failing, the computer says so in its view and stays up,
  for at most a bound that is Paul's to set; the runtime's own idle stop
  (`RUNTIME_IDLE_MS`) will end it in the end, and that wake is then a
  rollback, counted (P7).
- **A restored `/data` is checked** (I8): the SDK checks the archive's
  hash; the image checks what it can open (`PRAGMA quick_check` on each
  `state.db`) before it takes a turn. A start that keeps failing on save
  `n` tries save `n − 1` (F5), rather than striking out on the same
  bytes.

### P3. The snapshot is a cache of a save

Its record becomes `{id, image, save}`. It is used when `save` is the
newest save's number and `image` is the pinned image's reference;
otherwise the wake is the image and the save. A start from a snapshot
that fails forgets the snapshot and starts from the image and the save
within the same wake, and is not a strike against the computer (F7). The
snapshot then only ever makes a wake faster, and I9 is a test: both
paths, the same `/data`.

### P4. Litestream goes, or becomes real

Recommended: cut it (the image's binary, `litestream_config`,
`start_litestream`, the restart on a changed set of databases, and
decision 18's paragraph). With a save at each turn's end it would
recover at most the turn in flight, which P1 ends as lost anyway; and
its restore can only make a `/data` from two points in time (F8). The
storage intercept (`storage.fragment.internal`, cell/storage.mjs) stays:
it is the image's, for whatever an image wants to keep. *(It went too,
#156: no image used it.)*

If it stays instead, it needs what decision 18 already promised: a
restore procedure that is never mixed with an older `/data`, and a drill
that runs it into an empty computer in CI.

### P5. Tell the model what was cut

*Built 2026-10-06 (docs/durable-computers.md, "Built"), with the cut
turn also closed in Hermes' session at boot, which F10 showed is what
keeps the next message from being joined to it.*

The first turn an agent starts in a chat after a turn of its own there
was ended as lost carries a note, built from the journal alone: what was
asked (the cause's text), the steps `work` recorded (`turn.step`: tool,
arguments, whether it worked), what it had replied, and that it was cut
by a restart and should check what was done before doing it again.

- It is built from `work` and `chat`, so it is the same in any life and
  after a rollback.
- It is said once: the rule is "this agent's turn before this one, in
  this chat, ended as lost", which stops being true at the next turn.
- It is bounded, and it is part of what every runtime is handed
  (`TurnStart`), never Hermes' own: the scripted agent echoes it, which
  is how the stub's lanes see it.

This is one mechanism in place of Hermes' recovery notes, which stay
off: they resume a turn under its old message id, which one life per
turn rules out.

### P6. An approval outlives its turn

The answer to a card is already a memo: `turn.prompt.closed` has the id
`wk:<turn>:pc:<prompt>`, so the first answer wins. What is missing is a
turn to give it to.

- A card stays answerable until its `expiresAt`, whatever its computer
  does. A restart ends the turn that asked (one life per turn) and does
  not close its open cards.
- An answer whose turn is gone starts a new turn: its cause is the
  answer's record on `chat`, so it has an id like any other and runs
  once; its text is what was asked and what was chosen, with P5's note.
- A card past `expiresAt` is closed by whichever life next reads it.

Open, and for the real-Hermes lane to settle: how the cut turn's end
reads in the chat (it is no error); where the bridge finds a card's text
when its state is gone (the journal's tail); and whether Hermes, handed
"the owner approved this", asks again. If it does, the cheaper course is
to hold the keepalive while a card is open: the card's promise is then
true, at the cost of at most its life awake (an hour by default). That
revisits decision 42 and is Paul's.

### P7. A wake says what it restored

The Computer DO records, for each start, which save it restored, how old
it was, and whether the life before it ended by a sleep. It logs it as
one event and shows it in the computer's view. A rollback is then a
thing that is counted, which a test can assert and an operator can see,
and it is the "first proof" of the debt ledger's entries for F1 and F2.
Nothing's correctness depends on the guest reading it.

### P8. Small fixes

- A crash while a keepalive was open starts the computer again, so the
  next life ends its turns (F11).
- The second ask of a slow sleep does not forget a snapshot the first
  one stored (F11).

## Testing the failure cases

### Count runs, not records

Records are deduplicated by id, so a second run of a turn leaves the
records as they were (F2). Drafts are replaced by later drafts before
they are sent, so they undercount. A test of I1 and I3 must count what a
second run cannot hide:

| Rung | What counts a run |
|---|---|
| The engine, pure | `Effect::Runtime(Command::Start)` in the steps (`started()` in engine_tests.rs) |
| The bridge and the fake API | a runtime that records each `Command::Start` it is given; `World::calls` (every request the fake had) as a check on it |
| The relay rung | what the scripted Hermes saw (`support/hermes.rs`, `Seen`): each inbound message id |
| The images in Docker | the scripted model's `calls` (`support/model.rs`); the stub's `think` is one call a turn |
| The e2e | the Workers AI fake's calls (`s.ai.chats()`), the ledger's entries (`/api/test/ledger`, `entries {prefix}`), and `GET /api/computers/{id}/uses` after a `fetch … with $KEY` turn |

And prove a negative by a later positive, not by a sleep. Turns of a
chat run in order, and after a rollback the old records are read before
anything new. So say one more thing, wait for that turn's end, and then
assert the counts: every older turn has by then been run again or
fenced. A wait that runs out on a passing run costs every run
(AGENTS.md).

### What has to be built first

1. **An abrupt stop** for the in-process bridge (`support::Running`):
   `stop()` is SIGTERM's path. A `kill()` that aborts the task leaves
   the state file as the last step wrote it, which is what a crash
   leaves.
2. **A save and a restore of the state directory** in the bridge's
   tests: copy it (the save), put the copy back (the restore), or
   remove it. The reproduction above does this for one file.
3. **A counting runtime** (above).
4. **A lever on a computer**, `POST /api/test/computer {computer, op}`,
   beside the others (cell/src/levers.rs, docs/api.md):
   - `kill`: SIGKILL to the guest's PID 1, so the container exits as a
     crash does and the real `Exited` path runs;
   - `fail-saves {times}`: its next saves fail;
   - `saves`: what it keeps (each save's number, time, generation, and
     whether it was held) and what its last start restored (P7);
   - `break-snapshot`: its snapshot's record names one that does not
     exist (the hosted lane's).
5. **In the Docker rung** (images/bridge/tests/docker.rs): `docker kill
   --signal KILL`, and `/data` taken out of one container and put into
   the next (`docker cp`, or a volume), which then waits at the restore
   gate as it does today.

### Rung 0: the lifecycle, pure (crates/core/src/computer.rs)

- `it_is_saved_when_work_ends`: awake, the last keepalive closes, and a
  save is asked for after the settle; a keepalive that opens first
  cancels it.
- `always_on_is_saved_without_sleeping` (F1).
- `a_sleep_holds_then_saves_then_stops`: the order of a sleep's steps.
- `a_keepalive_during_the_hold_cancels_the_sleep` (P2's race).
- `a_sleep_whose_save_fails_keeps_the_container` (I5), and its bound.
- `a_failed_snapshot_start_falls_back_in_the_same_wake` (I6), if the
  choice of what a start restores moves into the lifecycle (today it is
  the DO's, in `try_start`); otherwise rung 6 has it.
- `a_start_that_keeps_failing_tries_the_save_before` (F5).
- `a_crash_while_busy_starts_it_again` (F11).
- `simulated_interleavings_keep_the_invariants`, which exists: add
  crashes, saves, failed saves and holds to its events, and the
  invariant that a computer is never asleep with work newer than its
  newest save unless a crash or a bounded failure put it there.

### Rung 1: the engine, pure (images/bridge/src/engine_tests.rs)

- `a_turn_starts_only_once_its_claim_is_answered`.
- `a_claim_another_life_holds_ends_the_turn`: 409, so no start, and one
  `turn.end`.
- `a_claim_with_no_answer_starts_nothing`.
- `a_restart_ends_what_was_handed`: replaces the end of
  `a_restart_starts_nothing_twice`.
- `a_rollback_starts_nothing_twice`: keep the state after turn one, run
  turn two to its end, build an engine from the kept state with a new
  life, feed the records again against a model journal, and count
  starts.
- `said_before_a_crash_and_never_started_runs_in_the_next_life` (I4).
- `any_history_of_crashes_and_rollbacks_keeps_the_invariants`: a seeded
  simulation in the manner of the lifecycle's. Its world is a model
  journal (ids to bodies, with the platform's replay and 409 rule) and
  the list of every state the engine ever persisted. Its events: a
  record arrives, a runtime event, a post is answered or lost, a crash,
  a start from any earlier state or from none. After each, I1, I2 and
  I4 are checked. This is the test that finds the cases nobody listed.
- `the_turn_after_a_lost_one_carries_the_note`, and
  `the_note_is_said_once` (P5).
- `an_answer_to_a_lost_turns_card_starts_a_turn`, and
  `a_second_answer_starts_nothing` (P6).
- `what_an_agent_says_unasked_survives_a_rollback` (F11).

### Rung 2: the bridge against the fake API (images/bridge/tests/bridge.rs, relay.rs)

- `a_rollback_runs_nothing_twice` and `a_lost_state_runs_nothing_twice`:
  the reproduction, counting runs. Both fail on master.
- `a_turn_cut_by_a_crash_ends_once_whatever_state_wakes`: kill during a
  `slow` turn, start from the state before it, from the state during
  it, and from none: one `turn.end` each time, an error, and no second
  run.
- `said_while_it_was_down_is_answered_once_after_a_rollback` (I4).
- `held_it_claims_nothing`, and
  `a_message_during_a_hold_waits_for_the_next_life`.
- `a_claim_the_platform_never_answers_runs_nothing`: the fake's
  `fail_posts` and `down` exist.
- In relay.rs, `hermes_hears_each_message_once_across_a_rollback`.

### Rung 3: the images in Docker (images/bridge/tests/docker.rs)

For the stub and for our Hermes image:

- `a_killed_container_wakes_from_its_save`: a turn, a save, a second
  turn with a model call, SIGKILL, a new container with the saved
  `/data` behind the restore gate. The model's calls for the second turn
  are unchanged, its turn has one end, and a third message is answered.
- `a_save_taken_while_it_writes_opens`: take `/data` during a turn's
  writes, many times, restore each, and run `PRAGMA quick_check` on each
  `state.db`. Without the hold this measures how often a hot copy tears
  (F4), which is the evidence for or against the hold. With the hold it
  must be never.
- `held_nothing_under_data_changes`: after the guest says held, a
  listing of `/data` (paths, sizes, times) is the same a few seconds
  later.
- `the_model_is_told_what_was_cut` (P5): the next call's messages hold
  the note, and the call after does not.
- `an_approval_answered_after_a_restart` (P6): a card, a kill, a
  restore, the owner's answer, a new turn, and what Hermes does with it.

### Rung 4: the e2e on workerd, with the stub (crates/e2e/src/lanes/computers.rs)

Local workerd takes no snapshots, so these are the image-and-save path.

- "a crash wakes it from its last save, and nothing that started runs
  again": a turn; its owner's sleep (the save); a wake; a `think` turn
  and a `fetch … with $KEY` turn; the lever's `kill`; then one more
  message and its turn's end. The model fake's calls, the ledger's
  entries and the computer's `uses` for the turns before the kill are
  what they were. This fails on master.
- "a turn cut by a crash ends once, as an error, and the next message is
  answered".
- "said while it was down is answered once".
- "awake and idle after a turn, it has a save newer than the turn"
  (`saves`); "an always-on computer is saved without sleeping" (F1).
- "a sleep whose save fails keeps its container" (`fail-saves`); "and a
  save that then works lets it sleep".
- "a message as it goes to sleep is answered once", if a lever can hold
  a sleep between its hold and its save; rungs 0 and 2 have the race
  either way.
- "its wake says what it restored" (P7): after the kill, a rollback is
  counted; after a sleep, none.
- The existing "nothing twice" checks count replies. Change them to
  count runs.

### Rung 5: the real-Hermes lane (`cargo xtask e2e --only hermes`)

The same crash and rollback with real Hermes: the Workers AI fake's
calls do not grow for a turn that had started (`s.ai.chats()`);
`model_saw()` (it exists) shows the note in the next call; the approval
flow of P6; and each profile's `state.db` opens after a restore.

### Rung 6: the hosted lane (a preview; it spends test cents, so it is Paul's or the coordinating session's to run)

- I9: a wake from the snapshot and a wake from the image and the save
  leave the same `/data` (a listing with hashes, read through a port or
  a scripted turn).
- A broken snapshot (`break-snapshot`) wakes from the save, in one wake
  (F7).
- A failed save, then a pin to another image, then a wake (F6): it does
  not sleep until it is saved, so there is nothing older to restore.
- Deploys during a turn (lesson 5 of docs/cloudflare-v1.md already
  plans this): one reply, one run, no second meter row.
- A large `/data` (a few GiB): the time of a save and of a restore, and
  what the sleep's deadline does (F11). This is the plan's "restore time
  for a large `/data`".

### What fails on master today

Written before any fix, these fail, and are the proof of the findings:

| Test | Finding | Checked here |
|---|---|---|
| Rung 2, `a_rollback_runs_nothing_twice`, `a_lost_state_runs_nothing_twice` | F2 | run |
| Rung 4, "a crash wakes it from its last save, and nothing that started runs again" | F1 with F2 | not run (needs the lever) |
| Rung 4, "an always-on computer is saved without sleeping" | F1 | not run |
| Rung 4, "a sleep whose save fails keeps its container" | F6 | not run |
| Rung 6, a broken snapshot wakes from the save | F7 | not run |
| Rung 1, `what_an_agent_says_unasked_survives_a_rollback` | F11 | not run |

## An order of work

Small pull requests, each with its tests, each leaving master whole:

1. **The tests' tools, and the failing tests**: the abrupt stop, the
   state's save and restore, the counting runtime, and the rung 1 and 2
   tests of I1 to I4, each ignored with its reason until (2) takes the
   mark off.
2. **P1** in the bridge, with the simple hold (the DO's mark before a
   sleep's backup; the bridge claims nothing past it). After this a
   rollback costs nothing but the turn in flight.
3. **The lever and P7**: `kill`, `saves`, and a wake that says what it
   restored. The rung 4 crash test now runs.
4. **P3** and P8: the snapshot's fallback and the two small fixes.
5. **P2**: the save as an action of the lifecycle (rung 0 first), saves
   kept and tried in order, the image's hold, and the failed save's
   rule.
6. **P4**: Litestream, as Paul decides.
7. **P5**, then **P6**, each proven on the real-Hermes lane.

Each one updates the docs it makes untrue (docs/computers.md,
docs/chat-records.md, docs/bridge.md, docs/api.md for the lever,
docs/cloudflare-v1.md's decision 18 and lessons 2 and 3, and
docs/finite-integration.md's agent runtimes row), and deletes its entry
in docs/technical-debt-ledger.md.

## Decisions that are Paul's

- **Litestream** (P4): cut it, or build its restore and its drill.
  Decision 18 changes either way.
- **A save that keeps failing** (P2, I5): how long a computer stays
  awake unsaved before it is let go, and who is told.
- **When to save** (P2): at each turn's end and at sleep, with a timer
  later; or decision 18's "every few minutes" as written. And how many
  saves are kept.
- **Approvals** (P6): a new turn for a late answer, or the keepalive
  held while a card is open (decision 42).
- **One life per turn** (P1): a turn whose life ends before the runtime
  finishes it is lost and said so, never handed again. This is the
  at-most-once choice; the other choice is a second run of a turn that
  may have had effects.
- **A backlog's age**: after a long outage (a computer that would not
  wake, or an owner at zero credit), the next wake answers every message
  sent meanwhile, up to 16 a chat. Routines have an hour's limit; chat
  messages have none. Left as it is here.

## What was not looked at

- `agent/`, goose's loop on Workers, which we own outright. Pi
  Durable's rules would apply there most directly (the debt ledger's
  "An agent runs one turn at a time, across all its chats" is one of
  its "many conversations at once").
- Hermes' own source, beyond what `hermes-boot`'s comments say of it:
  what it does with a `state.db` that does not open, and whether it asks
  again for an approval it is told was given.
- Cloudflare's answers: a start from an expired snapshot, whether a
  snapshot is atomic, and a save or restore of several GiB.
- The hosted lane: nothing here was run against a preview.

## Sources

- Pi Durable: <https://earendil.com/posts/pi-durable/> (2026-10-01).
- `@cloudflare/sandbox` 1.0.0, `dist/index.d.mts`: `DirectoryBackup`.
- docs/cloudflare-v1.md: decisions 18, 19, 39, 42; "Lessons from
  cloudflare/agents"; spikes S3 and S3b.
- docs/computers.md, docs/bridge.md, docs/chat-records.md.
- cell/src/computer.rs, cell/entry.mjs, crates/core/src/computer.rs,
  images/bridge/src/engine.rs, driver.rs, records.rs, limits.rs,
  images/hermes/boot/src/main.rs, hermes.rs, sync.rs.
