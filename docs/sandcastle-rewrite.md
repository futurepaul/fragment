# sandcastle: the rewrite to engineering style (proposed)

Status: proposed 2026-09-29, after phase 4, before sleep and wake and
before any computer that matters runs on it. Paul asked whether
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
