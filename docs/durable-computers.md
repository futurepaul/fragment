# Durable computers: the design

The design of record for how a computer's state survives a sleep, a
crash, a host restart or a rolled-back disk (Paul, 2026-10-05). The
analysis behind it is docs/explorations/pi-durable.md, where the
findings (F1–F11), invariants (I1–I9) and proposals (P1–P8) are named.
The discussion is in the Claude Doc "Durable computers: what's left".
Where this file and docs/cloudflare-v1.md decision 18 disagree, this
file is the newer word, and decision 18 points here.

## The rule

> One authority for each fact. Everything else is a cache, and a cache
> may be lost or go back in time without changing what happens.

| Fact | Its one authority | Everything else |
|---|---|---|
| What was said | the chat's `chat` channel | Hermes' session transcript, derived |
| Which turns started, what they did, how they ended, each prompt's answer | the chat's `work` channel (the journal) | the bridge's `state.json`, a cache of where to resume |
| The agent's self (`SOUL.md`, memories, skills) | the agent fragment's repo | the profile's checkout, a working copy synced every minute |
| The model's context and the agent's working files | `/data`, as of its newest save | the snapshot, a cache of that save for one image |
| Which save is current, and what a wake restored | the Computer DO | |
| Usage | the ledger | |
| A messenger's encryption state (Signal, SimpleX) | its connector's Durable Object (F, below; not built) | nothing |

## Built (2026-10-05)

- **One life per turn (P1, #137).** A turn runs only in the life of the
  bridge that wrote its `turn.start` to `work`, and only once that post
  is answered. A rolled-back or lost `/data` runs no turn twice; a turn
  a crash cut ends once, as lost. This is the at-most-once choice: a
  second run of a turn with effects (an email sent twice) is worse than
  a turn the person is told was lost. docs/bridge.md, docs/chat-records.md.
- **The sleep's hold (#136, #137).** The DO touches
  `/run/computer/hold` before a sleep's backup; the bridge claims no
  turn while it exists. docs/computers.md.
- **The snapshot is a cache of the save (P3, #136).** Its record names
  the backup it was taken with and the image it ran; a wake uses it only
  when both match, and a start from one that fails falls back to the
  image and the backup in the same wake.
- **A wake says what it restored (P7, #136).** `restored` and
  `rollbacks` in the computer's view, and a `"restored"` log line.
- **Two lifecycle fixes (P8, #136):** a crash while busy starts the
  computer again; a slow sleep's second ask keeps the first's snapshot.
- **A hold limits what an agent does for its owner, never its own
  memberships** (Paul, with #137): a held agent still records its turns.

## The open problem

`/data` is still saved only at sleep (F1), as a hot copy (F4), with one
save kept (F5), and a failed save still destroys the container (F6). A
save asked of the guest only covers writers we know of: a messenger
adapter's SQLite, a browser profile or something a skill installed can
be mid-write. And some state must never go back in time at all: a
messenger's encryption ratchets, which a clean but older copy breaks.
Neither Cloudflare nor Hermes solves that on a disk that can be lost;
Cloudflare restarts hosts on an irregular cadence, even for always-on
containers ([Containers FAQ](https://developers.cloudflare.com/containers/faq/)).

## The options

| Option | How it works | Unknown writers | Never-rewind state |
|---|---|---|---|
| A. Ask the guest to pause | The guest pauses its known writers | torn if writing | no |
| **A+. Cloudflare's intended design** | Save when idle; Hermes' databases copied by its own online backup (`hermes backup`, `sqlite3.backup()`); three saves kept; a wake falls back to one that opens | torn files survived by falling back | no |
| B. Freeze the guest | Stop every guest process for the copy; Cloudflare has no freeze call, so it is image-side, and may stop Cloudflare's own exec path | yes | no |
| C. No authoritative `/data` | Everything that matters lives outside | nothing to tear | once moved out |
| D. C as the direction, B as the mechanism | | B's | C's |
| **E. Hermes' intended design** | Split by writer: the gateway keeps only Hermes' home; its tools run in a separate Cloudflare Sandbox, as a Hermes terminal backend, whose Durable Object starts every command and so knows when it is idle | yes | no |
| **F. Never-rewind state outside the computer** | Messenger connectors in Durable Objects | not its job | yes |

Sources for A+ and E, read 2026-10-05: Cloudflare's
[sandbox lifetime](https://developers.cloudflare.com/sandbox/concepts/lifetime/),
[files](https://developers.cloudflare.com/sandbox/files/),
[auto-save guide](https://developers.cloudflare.com/sandbox/files/save-a-sandbox-automatically/),
[directory backups](https://developers.cloudflare.com/sandbox/reference/directory-backups/)
and [S3 mounts](https://developers.cloudflare.com/sandbox/reference/s3-mounts/);
Hermes v0.21.5 (tag v2026.9.24): `website/docs/user-guide/docker.md`
("the single source of truth"), `session-storage-recovery.md`,
`hermes_cli/backup.py`, `gateway/scale_to_zero.py` and
`website/docs/developer-guide/terminal-environment-plugin.md`.

## The decision: A+ now, toward E (Paul, 2026-10-05)

The shape finite-next's goose agent already had: the loop and its
journal in a durable cell that checkpointed only at idle, the work in
Sprite workspaces that paused warm (docs/finite-next-lessons.md). Five
steps, each its own pull request with its tests, each leaving master
whole:

1. **A+ (P2, reframed).**
   - The Computer DO saves `/data` when work ends (the last keepalive
     closes, after a settle), at every sleep, and every 15 minutes of
     activity, as Cloudflare's auto-save guide does. Saving is an action
     of the pure lifecycle (crates/core/src/computer.rs), tested there
     first.
   - Under the hold, the image copies each of Hermes' databases with
     Hermes' own online backup into a staging directory inside `/data`,
     and the save leaves the live `*.db`, `-wal` and `-shm` files out
     (`DirectoryBackup`'s `exclude`). A restore puts the copies back
     before Hermes starts. Nothing pauses Hermes' gateway.
   - The DO keeps the newest three saves. A wake restores the newest;
     the image checks each restored database (`PRAGMA quick_check`)
     before the guest takes a turn; a start that keeps failing on save
     `n` tries `n − 1`.
   - A sleep whose save fails is retried and does not destroy the
     container, within a bound; the computer's view says so.
   - Litestream goes: it is neither vendor's intended store, and its
     replicas are never read (F8).
2. **The seam.** `/data` splits into Hermes' home (its databases,
   sessions, profiles) and a work directory for everything its tools
   write (projects, scratch files, the browser profile), each saved on
   its own. Nothing moves yet; the split is what lets each live
   elsewhere later.
3. **Our terminal backend.** Hermes runs its tools' commands through a
   terminal-backend plugin of ours, at first locally in the work
   directory of the same container. We then know when tools are busy,
   which the save schedule can use instead of the keepalive.
4. **E.** That backend runs commands in a separate Cloudflare Sandbox
   per computer, whose Durable Object starts every command and saves
   its disk when idle. The gateway's container keeps only Hermes' home.
   To settle first: the latency each tool call gains as a round trip
   through a Durable Object; where the browser and desktop live; the
   credentials swap and the `fragment` CLI inside the sandbox.
5. **Toward C,** as each piece of Hermes' home gains an outside
   authority.

## F: messengers outside the computer

A connector Durable Object per linked messenger account:

- holds the protocol's keys and session state in its own SQLite,
  sealed at rest like every secret, so it never goes back in time;
- receives and decrypts each message and posts it as a record on the
  person's chat, which wakes a sleeping computer the way any chat record
  already does; the agent's reply is a record the connector encrypts and
  sends;
- so the computer never sees the protocol: Hermes talks only to the
  relay, which is also the condition for Hermes' own scale-to-zero.

F does two jobs: it keeps never-rewind state off a disk that can
rewind, and it answers how a sleeping computer hears an encrypted
message. Open questions: encryption now ends in the platform's Durable
Object rather than the person's computer (the same operator either
way, but to be said plainly); a Durable Object holding an outbound
connection is billed while connected (hibernation covers incoming
sockets only); and whether the protocol's library can run on Workers or
needs a small stateless container doing the cryptography while the
Durable Object keeps the state.

**The SimpleX spike (2026-10-05) is parked** (Paul: "too much of a
lift for now"; docs/explorations/simplex-connector.md). It ran end to
end against a local SMP server, but no SimpleX library exists outside
the Haskell app: the practical shape is a small always-on container per
account running `simplex-chat`, and only a pure-Workers client (about
1.5 to 2 months) fully meets "never goes back in time". Not built now.
SimpleX stays on an always-on computer (decision 32), whose disk can
still go back in time at a host restart; the exploration's rollback
runs show `/_sync` heals the connection in about 0.2 s, losing the
messages in flight.

## Deferred, with the recommendation

- **P5, tell the model what was cut:** the first turn after a lost one
  carries a note built from the journal. Recommended next, before P2's
  later steps; independent of everything above.
- **P6, approvals outlive a restart:** for now, an open approval card
  holds the keepalive until it is answered or expires (decision 42
  changes); later, a late answer starts a new turn.
- **A backlog's age:** chat messages have no age limit after a long
  outage (routines have an hour). Decide before production.
