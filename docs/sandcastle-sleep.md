# sandcastle phase 5: budgets, sleep, and wake

Status: proposed 2026-09-29 (Paul's calls below). Phase 5 of
`docs/sandbox.md` (R7: sleep when idle, wake fast on contact, crons that
fire while asleep), after the rewrite (`docs/sandcastle-rewrite.md`).

## Paul's calls (2026-09-29)

1. **Tiers:** hot, warm, and cold, each capped; only `/data` leaves the
   box (backups). The goal is Sprites' warm wake.
2. **A node keeps inside a reserve its operator chose:** no OOM and no
   full disk, ever; a warning when something else on the host encroaches
   on it; and it can say how many hot, warm, and cold computers it holds.
3. **Budgets come first, and are tuned from measurements, not guesses.**
4. **A Hermes computer is 4 GiB.** Turns are short and it should sleep
   soon after; a browser tool needs the headroom.
5. **Idle after 30 s.** Quick resume is what wins.
6. **Cron through Hermes' cron-provider plugin interface.** On fragment
   the scheduler is a cell; sandcastle stays generic, so a platform or
   anything else can provide it.
7. **Run Hermes the intended way in microsandbox,** with the fewest
   upstream changes; a custom image is fine, built from Hermes' source if
   possible.

## What was measured (finite-lat-6, 2026-09-29)

| Measure | Result |
|---|---|
| A Hermes v0.21.5 computer (4 GiB), idle 26 min after its start | 445 MiB used in the guest, 425 MiB resident on the host (`msb metrics`) |
| The VMM's own memory over its guest | about 20–40 MiB (the VM process's RSS against the guest's use) |
| `msb pause` / `msb resume` (a stand-in HTTP service, five cycles) | 7 ms / 8 ms; the first request answered 14–15 ms after the resume began; while paused, 0% CPU and the same resident memory |
| A request to a paused machine's port | no answer (the VMM holds the port; the guest is frozen) |
| Stop, then start, of a Hermes computer through the API (ten cycles) | serving again in a median 4.5 s (4.2–5.6), up to 2 s of it the node's tick |
| Where the VMs live | in `sandcastled.service`'s cgroup (`KillMode=process`), so the unit's `MemoryMax=` caps the daemon and every machine together |
| The host | 125 GiB RAM, 122 GiB available; the pool 1.68 TiB free; the root filesystem (msb's images and each machine's writable layer, up to about 4 GiB each) 408 GiB free |

And from reading the sources (msb 0.7.4, Hermes v0.21.5):

- **msb's full memory snapshots do not fit this design**: they refuse a
  disk the host supplies (our ZFS volume), and a restored machine has no
  credential swap. So warm is `msb pause` (memory kept, CPU freed), and
  cold is a stop (memory freed). A warm tier that frees memory waits on
  msb.
- **A paused machine refuses exec, credential rotation, a graceful stop,
  and removal**; `msb stop -f` still works. `msb ls` shows it `Paused`,
  which the node reads as `Other` today and would replace: pause needs
  its own state in the core.
- **`msb metrics --format json`** gives, per machine and without entering
  it: resident memory, guest memory used and available, vCPU time, and
  network bytes in and out (the guest's traffic through the engine's
  netstack). That is activity for egress and CPU work; the router sees
  ingress.
- **msb's stop gives a guest's processes about 300 ms** between SIGTERM
  and SIGKILL, so the node's quiesce (SIGTERM, up to 10 s, then sync) is
  the only graceful shutdown a service gets.
- **Hermes' cron runs in `hermes gateway`, not `hermes serve`.** The
  image starts the gateway only under its own init (s6) as PID 1; the
  node launches `serve` on agentd's non-PID-1 path, so its computers run
  no cron today. microsandbox's own Hermes recipe runs the image with
  `--init auto` (made to work detached in msb 0.5.7); for v0.21 images,
  whose entrypoint is a dispatcher, spike 1 found `--init auto` found
  nothing. How to run it is open (below).
- **Hermes says when it is busy**: `GET /api/status` (running agents,
  gateway busy), a token-gated `/api/health/idle`, and a prepare/commit
  retirement handshake that freezes new work only if it is idle. Its
  SIGTERM interrupts a turn after 0.5 s. Its cron jobs' next fire times
  are stored (`cron/jobs.json`), a missed recurring job fires once on the
  first tick, and a one-shot more than 120 s late never fires.

## Budgets

**The reserve** is the operator's: memory, the ZFS parent's space, and
the space for msb's images and writable layers. vCPUs are shared, never
reserved. The node never commits past its reserve, and the kernel and
ZFS make that hold even if the node is wrong:

- **Memory.** The unit's `MemoryMax=` is the reserve plus the daemon's
  own. What the node commits:
  - a hot computer: its full allocation plus the VMM's overhead (it can
    grow to that at any moment);
  - a warm computer: what it held when it paused (frozen, measured), plus
    the overhead;
  - a cold one: nothing.
- **Disk.** A quota on the ZFS parent equal to the disk reserve, and a
  reservation per volume equal to its `data_gib` (guaranteed, not
  sparse: this deletes the ledger's "no space reserved" entry). Snapshots
  share the rest, so the node keeps headroom for them, sized from what
  they measure (`usedbysnapshots`).
- **The engine's disk.** Images, and each machine's writable layer,
  measured (`upper_host_allocated_bytes`) against its reserve.

**Admission.** A create, a start, or a wake that would commit past the
reserve first demotes the least recently used warm computer to cold, and
then, if it still does not fit, is refused (503, with the reason and a
retry hint). A create also needs its disk's reservation to fit.

**Measured, not guessed.** The node samples every machine's metrics
(every 10 s) and keeps, per computer: resident memory while hot (p50,
p95, max), at pause, and at wake; wake latencies; idle spans. A capacity
report (`GET /v1/node` for grantors, and `sandcastled capacity` on the
host) shows the reserve, what is committed and measured, the tiers'
counts, and what more fits: guaranteed (by allocation) and observed (by
the measured distribution). The commitments' inputs are settings the
operator tunes from that report (the VMM overhead, snapshot headroom,
and, for a node that accepts risk, committing hot computers at a
measured percentile instead of their allocation); every default is the
guaranteed one.

**Encroachment.** The node compares the host's available memory and the
pool's free space against what its reserve still needs, and warns (the
report, and the journal) when something else has taken it.

**Setup.** `sandcastled setup` reads the hardware, asks how much to
reserve, prints what that holds for a computer size, and writes the
unit's settings, including `MemoryMax=`.

## Sleep and wake (after budgets)

- **Tiers as states in the core**: hot (running), warm (paused), cold
  (stopped). The owner's desire stays `running`; sleeping is the node's
  business, and a view says which tier a computer is in.
- **Idle** means no bytes through the router (an open, quiet WebSocket
  does not count), no guest network or CPU beyond a floor (`msb
  metrics`), and the service not busy (its own signal, when it has one:
  for Hermes, `/api/status` and the retirement handshake). Idle for 30 s:
  pause. Warm and untouched for a day, or needed for room: stop.
- **Wake**: the router holds a request for a warm or cold computer,
  resumes or starts it (nudging its batch at once, not at the next tick),
  and forwards when it answers. The API's start wakes too.
- **Timers**: a computer's service may name its next wake time; the node
  wakes it ahead of that, and keeps it awake while it is busy. How a
  service names it is generic: sandcastle offers the timer and the wake;
  a scheduler (a cell on fragment, Hermes' own, anything) decides.
- **Credentials and snapshots across tiers**: a paused machine cannot
  rotate, so a refresh waits for its wake; a scheduled snapshot of a
  paused machine's disk is clean if the pause flushed the guest
  (`--guest-flush required`).

## Open

- **How Hermes runs** (serve, gateway, cron) in an msb 0.7.4 machine:
  `--init` handoff to the image's s6 as microsandbox documents, a custom
  image built from Hermes' source, or a two-process launcher. Being
  researched.
- **The cron provider**: a Hermes plugin (like Nous' Chronos) that hands
  each job's next fire time to a scheduler, which fires it through
  `POST /api/cron/fire`; on fragment, a cell.
