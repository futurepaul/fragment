# sandcastle: the rewrite to engineering style

Status: **done 2026-09-29** (Results, at the end), and running on
finite-lat-6. Proposed the same day, after phase 4, before sleep and wake
and before any computer that matters runs on it. Paul asked whether
sandcastle follows `engineering-style/engineering-style.md` (and
TigerStyle), and to rewrite it fearlessly where it does not: it will be a
foundational service.

## Where it stands (audit, 2026-09-29)

It works: phases 1 to 4 are proven on finite-lat-6. It was built as a
sequence of spikes that each proved one thing on the real engine, and it
shows. Measured in `crates/sandcastled` (non-test code):

| Guide | Now |
|---|---|
| About two assertions per nontrivial function | about 20 in some 200 functions |
| Typed errors at boundaries; never match error text | 12 `Result<_, String>`; 4 decisions by matching message text ("not found", "already exists") |
| Handle every error | 18 discarded results (`let _ =`); 46 log-and-continue prints |
| Make it logical, simulate it deterministically; shim clock, storage, network | the supervisor reads wall clocks (10 places) and drives I/O directly; tests use real TLS, real sockets, and 6 real sleeps; no simulation |
| Authoritative state in schema and constraints, not JSON | a computer's spec and its good spec are JSON columns |
| State that decides safety survives a restart | failure counts, backoff, launch times, snapshot and credential schedules, and what credentials a machine holds live in memory (`Track`) |
| Functions fit a screen; the parent owns control flow | `converge` 84 lines, `run_step` 78, `put_computer` 87, `main` 114 |
| Prove the product, not a harness | the real engine is proven by hand (shell and ad hoc Python on lat-6) |

Two independent reviews against the guide (safety and control flow;
state, concurrency, and testing), 2026-09-29, found these, most severe
first. **[repro]** marks one reproduced with a failing test in a scratch
copy. None is live beyond the test node: fragment.club runs no sandcastle
code.

**Defects in the shipped node:**

1. **[repro] A daemon restart stops every serving computer with
   credentials.** After a restart the supervisor holds no record of what
   a machine was handed, so its first credential refresh reads "shape
   changed" and stops the machine (`supervisor.rs`, `scheduled_credentials`).
   The restart tests missed it: one has no credentials, the other kills
   the service first.
2. **[repro] Backup shipping breaks after 10,000 backups** (about 35 days
   at the 300 s default). The listing keeps the *oldest* 10,000
   (`ORDER BY created_at … LIMIT`), so "the last shipped" freezes. The
   shipper then re-sends a shipped snapshot forever, failing on its
   unique key every 10 s. The manifest stops growing, and restores of new
   backups 404.
3. **A failed assertion does not crash the node.** Panics unwind (no
   `panic = "abort"`), the supervisor and shipper run as detached tasks,
   and mutex poisoning is swallowed. An assertion in the supervisor kills
   only that task, while the API keeps accepting work nothing will
   converge.
4. **Infrastructure faults roll back good specs.** Every string error
   becomes the spec's fault (`impl From<String> for Step`). A snapshot
   error, a full pool, an SQLite error, or an exec timeout during a
   rebase marks the new image failed and rolls it back until someone
   starts it again. The shipper and the supervisor also race on the same
   snapshots (pruning one being sent), which can trigger exactly that.
5. **A restore counts as done when its disk exists.** A crash after the
   whole stream but before the last incremental, or a failed cleanup
   (`let _ = destroy`), boots an older snapshot as "restored".
6. **Multi-step work has no persisted intent.** Shipping is upload,
   then record, then manifest, and a failure between them leaves the store
   and the manifest disagreeing. Nothing reconciles them, and aborted
   multipart uploads are dropped. A crash between a stop and its snapshot
   loses the snapshot.
7. **Unbounded waits inside the control loop.** Only the S3 connect has a
   deadline (not TLS, the send, or body reads), and a restore runs inside
   the supervisor's tick, so one stalled bucket halts every computer.
   `zfs send` reads, proxied requests, upgraded connections (outliving
   their connection slot), and API body reads have no deadline either.
8. **Invariants by read-then-write, not transactions.** The grant cap
   is checked and the row inserted under separate locks, so parallel
   creates exceed it. A stop racing a delete can un-delete a computer
   whose disk was destroyed. A second DELETE, and a replayed
   restore-create, answer 409: not replay-safe.
9. **Unbounded retries:** stopped and deleted computers retry every tick
   forever. A failed `set_failed` resets the failure count. Permanent
   accept errors loop.
10. **Security:**
    - An image may start with `-`, and it is the last argument to `msb
      create` with no `--` before it.
    - `data_path` allows `:`, `,` and `=` and is spliced into
      `--mount-disk`.
    - Any key can fill the global replay cache before authorization,
      locking every caller out.
    - A 429 or 408 from a credential source counts as a refusal and
      withdraws live credentials.
    - The credential origin is checked at PUT but not at fetch.
    - The proxy does not strip `Forwarded`.
    - `env_file` asserts values but not keys, which reach a root shell.
11. **External output truncated silently** (msb and ZFS output past the
    cap, and read errors taken as end of file), then parsed as if whole.
12. **State by name, not id:** the supervisor's memory and the observed
    map are keyed by the reusable name ([repro]: a re-created name
    inherits the old one's backoff). The observed map is never pruned.
    Backoff and the snapshot schedule reset on restart, so a node that
    restarts more often than `snapshot_every_s` never snapshots.
13. **Brittle identity:** the rebase trigger is a hash of the wire
    structs' JSON (a new serde field would rebase every computer). The
    restore source is a combined `id@snap` string. The snapshot kind lives
    only in its name, and same-second ties are broken by comparing names.
14. **Test gaps:** replays of start, stop, and delete; grant replays and
    lowered grants; the replay cache across a restart; restarts
    mid-rebase, mid-restore, and mid-ship; engine `list` failing; prune
    against ship; any bucket failure; a full disk; backoff expiry (it
    cannot be tested: real clock). About 20 s of real sleeps. `cfg(test)`
    variants in production types and `cfg!(test)` bypasses in the config
    check. docs/sandbox.md marks "kill the daemon mid-rebase" as met with
    no test and no evidence row.

**Keep:**

- **Wire contract (proto):**
  - named limits with valid/invalid tables;
  - `deny_unknown_fields`;
  - the redacted credential `Debug`.
- **Auth (nip98):**
  - typed errors;
  - size checks before crypto;
  - the signed URL rebuilt from the node's own config.
- **Sealed backups:** the seal's STREAM construction and its
  tamper/reorder/truncate tests.
- **Store:**
  - CHECK constraints;
  - `StoreError::Corrupt`;
  - validation on read;
  - the node cap inside its transaction;
  - `synchronous=FULL`.
- **Engine gate:**
  - `env_clear`;
  - `kill_on_drop` and timeouts on every call;
  - credential values only in the environment;
  - `--` before exec argv.
- **Disks:** formatting only when blkid finds nothing.
- **The fd 3 fix:** close-on-exec, with its SAFETY comment.
- **Shape:** the API records desire and the node converges at its own
  pace, probing the service rather than trusting the engine.

**Semantics the rewrite changes** (Paul's to veto; everything else is
engineering):

- Only a fault of the spec (its image fails to create, its service fails
  to launch or answer) rolls a generation back. A fault of the node or
  the host is retried with backoff, and shown, never blamed on the spec.
- A credential source's 401, 403, and 404 withdraw. Anything else (429,
  408, 5xx, a timeout) keeps what the machine holds.
- A restore is complete only when the requested snapshot is on the disk.
  Otherwise the disk is destroyed and the restore starts over.
- DELETE, stop, start, and a replayed restore-create are idempotent: the
  same answer again, never 409.
- An assertion failure crashes the node, and systemd restarts it (its
  computers keep running, as now).

## The prompt, in five parts

### 1. Problem statement

sandcastle's node decides everything that matters about a person's
computer (when a machine is replaced, when a disk is snapshotted, shipped,
restored, or destroyed, which credentials reach it), but its decisions are
entangled with I/O and wall-clock time, so they cannot be simulated; the
state those decisions rest on is partly in memory; its errors are strings;
its invariants are mostly unasserted; and the real engine is proven by
hand. Rewrite the node so its correctness is designed, asserted at
runtime, simulated deterministically under faults, and proven on the real
engine by a program, not by hand, keeping the contract (the API, the spec,
NIP-98, grants, tickets) and what the phases proved.

### 2. Acceptance criteria

- **A pure core.** One crate (`sandcastle-core`) holds the node as a
  state machine: validated commands and observations in, actions out. No
  tokio, no clock reads, no I/O, no randomness it does not take as input.
  Every decision lives there: convergence, rebase and rollback, the
  snapshot schedule, backup shipping and pruning, restore, credential
  refresh and withdrawal, deletion.
- **State in schema.** Everything a decision rests on is a column with a
  constraint, written in one transaction per step: the spec as columns and
  child rows (not JSON); generations as rows; every multi-step operation
  (create, rebase, stop, delete, restore, ship) as a persisted intent with
  its step, so a crash at any point resumes and never repeats an
  irreversible step; backoff and schedules as timestamps. Values read back
  are asserted (paired with the checks before the write).
- **Narrow gates.** The engine, disks, the bucket, the credential source,
  the prober, and the clock are traits with typed errors. The msb gate
  classifies failures by exit status and JSON, never by message text.
- **Deterministic simulation.** A seeded simulator drives the core and the
  executor against simulated gates with injected faults (a crash and
  restart at any step, a gate error or timeout, a slow operation, a full
  pool, a bucket that fails) and checks the node's invariants after every
  step. CI runs many seeds; a failing seed replays exactly.
- **A real-engine e2e.** A Rust binary drives a real node through the
  product path (Hermes created and served at its URL behind a ticket and
  its own login; a rebase and a rollback; snapshots, a sealed backup, a
  restore after the node's state is wiped; credentials through a
  platform; the guest's egress and the host's ports) and writes
  machine-readable evidence. It replaces the hand-run checks.
- **The guide, measurably:** about two assertions per nontrivial
  function in the core and the store; no function over a screen; no
  `Result<_, String>` or error-text matching at a gate or the core; every
  loop bounded or saying why; `u32`/`u64` at boundaries; every API
  mutation with valid, invalid, and replay tests (idempotent ones also
  with conflicting bodies); a restart test for every storage invariant.
- **Parity:** everything phases 1 to 4 proved still holds, checked by the
  simulator, the ported end-to-end tests, and the real-engine e2e.

### 3. Constraints

- **Musts:** the engineering style; a separate workspace with no fragment
  dependency; msb 0.7.4 through its CLI (no SDK: several hundred crates);
  ZFS disks; assertions on in release; clippy with warnings denied.
- **Must-nots:** no compatibility with the old store (a hard cut, with a
  documented reset); no test-only shims in production paths; no new
  dependency without an audit.
- **Keep:** the wire contract (`proto`) and its validation, NIP-98 and the
  replay cache, the seal format, the SigV4 client, the router and proxy's
  shape, the CLI.
- **Escalate to Paul:** anything that changes what a computer's owner or
  the platform sees (rollback, deletion, credential withdrawal
  semantics); wiping lat-6's computers and backups for the hard cut;
  anything that touches fragment.club.

### 4. Decomposition

1. **Design** (this doc, extended): the core's states, commands,
   observations, actions, and invariants; the schema; the gates and their
   typed errors; the executor's contract. Reviewed before code.
2. **Core and store:** written new beside the old node, with unit tests
   (valid, invalid, replay, restart). Nothing wired yet.
3. **Simulator:** simulated gates, fault injection, invariants, seeds in
   CI; fix what it finds in the core.
4. **Executor and node:** the async shell running the core's actions
   through the real gates; the API and router on top; the old supervisor,
   shipper, and store deleted; the existing end-to-end tests ported.
5. **Real-engine e2e:** the Rust binary; the hard cut on lat-6 (reset,
   recreate); the evidence.
6. **Converge:** README, docs/sandbox.md, the debt ledger, the PR.

### 5. Evaluation

- **Simulation invariants** (checked after every simulated step), at
  least: at most one machine per computer; a data disk attached to at
  most one machine; a data disk destroyed only after its computer's
  deletion was recorded, and its machine removed; a rebase always
  preceded by a clean stop and a snapshot; a failed generation rolled back
  to the last that served, never flipping between the two; no step of a
  persisted intent run twice when it is irreversible; a shipped chain
  always restorable (every incremental's base shipped and kept); no
  credential value in the store, the disk, or a log line; a revoked grant
  leaves no running machine; ports unique.
- **Faults:** a crash at every step of every intent, gate errors and
  timeouts at every call, slow calls past their deadlines, a full pool, a
  bucket that fails mid-upload, a source that is down or refuses.
- **Unit and restart tests** per the acceptance criteria.
- **Real-engine e2e** as above, with its evidence kept in the repo's
  results table (docs/sandbox.md).

## The design (phase 1, 2026-09-29)

Paul approved the plan and the semantics above on 2026-09-29 ("proceed
with fearless rewrite"). This is the blueprint the code follows.

### One idea

The node is a set of per-computer state machines. Each is a row in
SQLite (plus its generations and backups). Each step the executor asks
the pure core what to do next, given the row and what it has observed
this step, and gets one answer:

- `Observe(what)`: look at the world (the disk's facts, the service's
  health, the credential source);
- `Do(effect)`: change the world (make, start, stop, or remove a machine,
  launch or quiesce the service, snapshot, prune, destroy, ship, write a
  manifest, receive a restore);
- `Rest(until)`: nothing to do before then.

The executor runs the effect through a gate, with a deadline, and hands
the outcome back to the pure core (`apply`), which returns the row's next
state. The store writes that in one transaction. Nothing the node decides
lives only in memory: a crash between any two steps restarts from the
row, and every step is either idempotent or checked by observing first.

```
          plan(row, knowledge, now) ─▶ Observe ─▶ gate ─▶ knowledge ┐
store ─▶ row                          Do     ─▶ gate ─▶ outcome ──▶ apply(row, effect, outcome, now) ─▶ store (one txn)
          ▲                            Rest   ─▶ next tick           │
          └──────────────────────────────────────────────────────────┘
```

Knowledge (engine state, disk facts, a probe, fetched credentials) is
ephemeral: it lives for one step batch and is observed again after a
restart. Credential values are knowledge only; they never reach a row.

### Crates

- `proto`: kept. Tightened:
  - an image may not start with `-`;
  - `data_path` is `[A-Za-z0-9/_.-]` only;
  - service env keys are checked where they are written.
- `nip98`: kept.
- `core` (new): the model, `plan`, `apply`, the invariants, and every limit
  (with compile-time assertions between related ones). It depends on
  `proto` alone: no tokio, no clock, no I/O.
- `sandcastled`, rewritten on the core. It has four layers:
  - the store;
  - the gates;
  - the executor;
  - the commands (the API's mutations as functions: validate, one
    transaction, a typed error), with the HTTP API, router, and proxy
    thin above them.
- `sim` (new, a test crate): the deterministic simulator.
- `e2e` (new): the real-engine end-to-end binary.
- `cli`: kept.

### The row

The rebase trigger is a generation *number*, not a hash of JSON.

- **Fixed for a computer's life:**
  - `id`, `name`, `owner`, `host_port`;
  - `vcpus`, `memory_mib`, `storage`, `data_gib`, `data_path`.
- **Changeable:**
  - `url_auth`: a column, changed in place, no rebase;
  - the generation fields: `image`, the service's argv, port, health path
    and env, and `credentials_url`. These are one row of `generations`
    each, with argv and env as child rows. A PUT that changes any of them
    adds generation `n + 1`.
- **Desire:** `desired` (`running`, `stopped`, `deleted`; nothing leaves
  `deleted`), and `spec_seq`, the generation asked for.
- **What the world holds:**
  - `applied_seq`: the machine was made from this generation;
  - `good_seq`: the last generation that served;
  - `failed_seq` with `failure` (`kind`: spec, node, or source; the step;
    a reason of at most 512 bytes). A spec failure of generation
    `spec_seq` rolls back to `good_seq`.
- **Retries:** `failures` and `retry_at_ms`.
- **Service:** `launched_at_ms`, `served_at_ms` (the grace survives a
  restart).
- **Snapshots:**
  - `snapshot_seq`: the next snapshot's number;
  - `snapshot_due`: a kind the node owes;
  - `snapshot_at_ms`: when the scheduled one is due.

  A snapshot's name is `sc-<seq>-<kind>`: ordered by its number, never
  by a clock or by comparing names.
- **Credentials:** `credentials_digest`, `credentials_shape` (names, hosts,
  and placeholders: never a value), and `credentials_at_ms`.
- **Restore:** `restore_source`, `restore_snapshot`, and the chain as child
  rows. Progress is observed: element `i` is received exactly when the
  disk holds its snapshot.
- **Shipping:**
  - `ship_head` (the newest shipped snapshot and its number);
  - `ship_since_whole`;
  - `ship_upload`, an open multipart upload's id, aborted before anything
    else after a crash;
  - `manifest_due`.
- **For views:** `status` (`absent`, `starting`, `serving`, `stopped`,
  `failed`) and `status_reason`. They are persisted, so a view never
  depends on a name-keyed map in memory.
- **Concurrency:** `version`, bumped by every write.

### Steps, and what makes each safe

| Desire / state | Steps | Safe because |
|---|---|---|
| a machine is due (none, or a new generation) | fetch credentials (if any); if running: quiesce, stop, `snapshot_due = rebase`; snapshot; ensure or restore the disk; create; launch; probe until served or past the grace | credentials first, so a source that is down leaves the old machine serving. The rebase snapshot is owed in the row before it is taken, and a snapshot already on the disk counts as taken. Create replaces by name. |
| stopped, wanted running | fetch credentials; start; launch; probe | start and launch are idempotent (the launch script keeps a live service) |
| running, wanted stopped | quiesce; stop, `snapshot_due = stop`; snapshot | owed in the row before it is taken |
| wanted deleted | quiesce; stop; remove; destroy the disk (only when the machine is observed gone); delete the row | each step is observed first, and nothing leaves `deleted` |
| restore owed | observe the disk: if its snapshots are not a prefix of the chain, destroy it; receive the next element; done when it holds the target, then `snapshot_seq` resumes past the restored ones | never "the disk exists, so it is done" |
| serving | probe; a scheduled snapshot when written; credentials refreshed when due (rotate live; new shape: stop, start); ship the oldest unshipped snapshot; write the manifest when due; prune | one machine per computer runs these in order, so pruning never races shipping. A shipped head and an open ship are never pruned. |
| a failure | `failures + 1`, `retry_at` backs off (2 s doubling to 60 s), `failure` recorded; a spec failure of a new generation rolls back | backoff is in the row, so a restart does not reset it |

Faults are typed at the gate and classed in the core:

- **Spec:** creating a new generation's machine, or launching its
  service, fails, or it stays silent past the grace.
- **Node:** everything else the host does.
- **Source:** the credential source. A 401, 403, or 404 withdraws the
  credentials; anything else keeps what the machine holds.

### Gates

Each gate is a trait with typed errors, with a real implementation and a
simulated one:

- **Engine** (msb): list, create, start, stop, remove, rotate, launch,
  quiesce, sync.
- **Disks** (zfs): facts, ensure, snapshot, destroy a snapshot, destroy,
  send, receive.
- **Objects** (S3, with a deadline on every phase).
- **Source** (credentials).
- **Prober.**
- **Clock.**
- **Randomness.**

No gate decides anything, and none matches error text: a caller that
needs to know whether something exists observes first. Output past a cap
is an error (`OutputTooLarge`), never silently cut. Streams are
associated types, so no production enum carries a test variant.

### The executor

A step is three public calls:

1. `plan`: read the row, ask the core.
2. `perform`: run the effect through its gate.
3. `record`: apply the outcome in one transaction, applied to the row as
   it is now: the API may have changed desire meanwhile, and an
   outcome is a fact about the world either way.

The loop runs every computer every tick, at most a bounded number of
steps each. Its tasks are joined, and `panic = "abort"`, so an assertion
takes the node down (systemd restarts it; machines keep running).

### The simulator

It drives the real core, store (in-memory SQLite), and executor against
simulated gates over one simulated world. The world holds the machines,
the disks (each with a content version, bumped by the guest's writes;
snapshots remember theirs), and the bucket. It persists across simulated
crashes. The simulator also drives the commands.

- **A seed chooses:**
  - the workload: grants, creates, updates, restores, starts, stops,
    deletes, tickets, and exact and conflicting replays;
  - the guest's writes and service deaths;
  - faults at every gate call: errors, timeouts, a full pool, a failing
    bucket, a source down or refusing;
  - crashes: between `perform` and `record` (the effect happened, the
    row does not know it), and between steps;
  - the clock's advance.
- **Invariants after every step:**
  - one machine per computer, and none without a row;
  - a disk destroyed only when its row says deleted and its machine is
    gone;
  - a disk in at most one machine;
  - every replace of a machine preceded by a clean stop, and by a
    snapshot when the disk was written;
  - a disk's content never rewinds except by a restore, which yields
    exactly the requested snapshot's content;
  - `failed_seq` only after a spec fault;
  - the shipped chain restorable: every base shipped and in the bucket,
    the head kept locally;
  - no credential value in the store's bytes;
  - ports unique;
  - per-owner counts within grants when made;
  - replays answer the same.
- **At quiescence** (faults off): status matches desire, `applied_seq`
  is the target, nothing is owed, and nothing leaks.

A failing seed prints and replays exactly. CI runs a few thousand seeds,
and each audited defect is a named regression seed or test.

### The real-engine e2e

A Rust binary (`sandcastle/crates/e2e`) run from a workstation against
a node over its API, with SSH for host-side checks. It drives the
product:

- Hermes created and served at its URL behind a ticket and its own
  login, with a WebSocket;
- a rebase keeping a marker on the disk;
- both rollbacks;
- a stop and a start;
- snapshots, a sealed backup, and a restore after the node's state is
  wiped (the documented reset);
- credentials through a platform;
- the guest refused its host's addresses and private ranges;
- only 22 and 443 open;
- a SIGKILL of the daemon at each step of a rebase and a restore,
  converging after;
- no leaks: every `sc-*` machine and volume has a row.

It writes its evidence as JSON.

### The reset (the hard cut)

`sandcastled reset --state-dir … --zfs-parent … --node-name …` removes
whole explicit roots, and nothing else:

- every `sc-*` machine;
- every volume under the parent;
- the bucket prefix `nodes/<node>/`;
- the state file.

It asks for the node name typed again. lat-6 is reset with it before the
rewrite first runs there (Paul, 2026-09-29: wipe lat-6).

## Results (2026-09-29)

Built in the order of the decomposition, each part committed with its
tests: the design (above); `crates/core` (the pure core, 24 path tests);
`crates/node` (the store, the commands, the seal, the manifest, the
gates, the executor, the scheduler; 34 tests); `crates/sim` (the
simulator); `crates/sandcastled` rewritten on top (11 tests, the old
supervisor, shipper, store, and gates deleted); `crates/e2e`. lat-6 was
reset with `sandcastled reset` and runs the new node; the real-engine
e2e passes 78 checks there (docs/sandbox.md, The rewrite on the real
engine). 87 tests in CI (a `sandcastle` job), 1024 seeds pass.

**What the simulator found** that the hand-written tests had not, each
fixed in the core with a regression test:

1. *Seed 80:* a crash after a rebase's stop, before its snapshot, and the
   machine was replaced over writes no snapshot held. Now the snapshot
   before a replacement is decided from what the disk shows, and a stop's
   snapshot is owed in the row before the stop.
2. A snapshot taken but not recorded (a crash between the two) was then
   shipped as the head. Now a batch records any such snapshot before it
   snapshots or ships.
3. *Seed 38:* a guest that would not take a launch was asked again
   forever, at the relaunch of a service that had served and died. Now
   the machine restarts first, at both launch sites.
4. *Seed 22:* a failed guest sync ended the batch, so every batch retried
   it and none reached the snapshot or the duties (an open upload was
   never aborted). A sync is advisory now, and the executor and the
   simulator share one rule for whether a batch goes on (they had
   drifted).
5. *Seed 87:* a restart for a failed launch lost its intent when its
   stop failed, and relaunched in a loop. The restart now goes on through
   a failed quiesce and a failed stop.

**What reading msb 0.7.4's source found:** a graceful `msb stop` waits
for the guest to power off with no deadline (now: a stop that failed is
followed by `msb stop -f`); `msb exec` boots a stopped machine for the
command and stops it after (documented: the core execs only a machine its
listing just showed running); `Starting` and `Draining` are visible only
after a daemon died mid-operation (they read as `Other`); an image is a
host path only when it starts `.`, `/`, `./`, or `../`, which the spec
already refuses.

**What writing the tests found:** a computer past 100,000 backups (about
a year of 5-minute snapshots) could not record another, and the node
would then crash on every restart. Recording never fails on the count
now, and the manifest lists the newest 100,000.

**A semantic refinement** within Paul's call ("only spec faults roll
back"): a spec fault is the engine *refusing* to make or launch an
unproven generation, or its service silent past the grace. An engine
that timed out or could not run is the node's fault (an image pull on a
slow link is not a bad image).

**The audit's items, and where each went:**

| # | Defect | Now | Proof |
|---|---|---|---|
| 1 | A restart stops credentialed computers | the row holds each machine's credential shape and digest | core: `credentials_refresh_by_what_the_source_says` |
| 2 | Shipping breaks after 10,000 backups | the head shipped is in the row; the manifest lists the newest | node: `a_computer_past_its_manifest_cap_keeps_shipping` |
| 3 | An assertion kills a task, not the node | `panic = "abort"` in both profiles; the scheduler crashes the node when the store fails | the profiles; systemd restarts it (the e2e's SIGKILLs) |
| 4 | Infrastructure faults roll back good specs | faults typed at the gate and classed in the core | core: `a_node_fault_during_a_rebase_does_not_roll_back`, `an_engine_that_times_out_…`; the simulator checks every rollback's cause |
| 5 | A partial restore boots as complete | done only when the target snapshot is on the disk; a prefix resumes, anything else starts over | core restore tests; the e2e's mid-restore SIGKILL |
| 6 | Multi-step work has no persisted intent | owed snapshots, the open upload, the owed manifest, and backoff are columns | the simulator's crashes after every effect |
| 7 | Unbounded waits | a deadline on every gate phase and stream; one batch per computer, 32 at once; API bodies, proxy answers, and tunnels bounded | the gates' tests; the scheduler |
| 8 | Invariants by read-then-write | each command one transaction, caps inside it; nothing leaves `deleted` | node: the command tests |
| 9 | Unbounded retries | backoff in the row, 2 s doubling to 60 s; a listener that keeps failing ends the node | core: `a_first_generation_that_fails_backs_off` |
| 10 | Security | the image and mount checked by one predicate the gate asserts; the replay cache only for known signers; only 401, 403, 404 withdraw; the origin checked at fetch; `Forwarded` stripped; env names asserted | proto, node, and daemon tests; the e2e's `auth` |
| 11 | Output truncated silently | past a cap is `BadOutput`; a read error is never end of file | node: `a_program_is_run_bounded_and_classified` |
| 12 | State by name | by `ComputerId` everywhere; a batch's knowledge dies with it | the store; the simulator |
| 13 | Brittle identity | generations numbered, snapshots `sc-<n>-<kind>`, a restore's source an id and a chain | the core's model |
| 14 | Test gaps | valid, invalid, replay, and restart tests per command; the simulator; the daemon's tests on the simulated world with no sleeps; no test variant in a production type | the suites |

**Left, in the debt ledger:** the real-engine e2e runs by hand (no KVM
host in CI); the node does not report machines with no row; backups
never expire; the test node's certificate names three hosts; the node
runs as a login user.
