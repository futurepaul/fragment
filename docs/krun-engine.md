# The libkrun engine: from spike to production

*Plan, 2026-10-01.* Paul, after the spike (docs/krun-spike.md):

- "we're fine without warm... that's parity with cloudflare anyway";
- "proceed with whatever you need to make this production grade (full
  parity with what we need from cloudflare)";
- the seccomp filter becomes an allowlist;
- the other calls are mine, made for **speed first, then Cloudflare
  parity**.

Nothing merges to master until this is fully proven. The work continues
on branch `krun-spike` (draft PR #103) and on a branch of the celld fork.

## Problem

The spike proved that libkrun, driven directly, boots, isolates, and
intercepts better and faster than msb. What exists is crates and a test
driver, not an engine: nothing holds VMs on a node's behalf, and nothing
speaks Cloudflare's container API. celld's `ctx.container` is wired to
Docker, with snapshots and intercepts stubbed.

The goal is an engine that:

- a node runs as a service;
- implements Cloudflare's Durable Object container API method by method,
  at the speed the spike measured;
- is reached by celld through an engine interface that Docker also
  implements, so fragment writes against `ctx.container` alone.

## The calls, for speed then parity

| Call | Decision | Why |
|---|---|---|
| Warm tier (pause) | None: an idle VM stops, and a wake is a cold start | Paul; Cloudflare has none |
| seccomp | An allowlist from what libkrun calls | Paul |
| Launching VMs | A root engine service jails each VM itself (no `sudo` or `systemd-run`); celld and sandcastled stay unprivileged and call it over a unix socket | About 20 ms off every start; how Docker's daemon works |
| Guest ports | Measure the VM's NIC (kernel TCP) against vsock and keep the faster; vsock stays for servers listening on loopback only | Speed: vsock carries about 200 MiB/s |
| Images | Unpack as Docker does: xattrs (file capabilities), device nodes, every zstd form; accept `docker save` tars, which is how celld ships images | Parity |
| Roots and snapshots | Keep overlays (a fresh root in 5 ms, no ZFS). `snapshotContainer` copies the writable layer; storage with reflinks only if that measures slow | Speed; Cloudflare's snapshot is the writable root only |
| Proxy and NIC | Keep: the proxy on the node outside the jail, a real NIC rather than TSI | No speed cost; safer, more reliable |
| Forwarder | Keep it in the VM process | An extra process per VM costs start time; the allowlist covers it |
| Intercepted requests | Go to the Durable Object through celld's callback route | Parity: `interceptOutboundHttp` hands a request to a Fetcher |
| celld | An engine trait in the fork, Docker and this engine behind it; one conformance Worker for both | Snapshots and intercepts are stubs on Docker anyway |
| Internet off, a name not intercepted | NXDOMAIN at once; Cloudflare lets the lookup time out | Speed; nothing waits on a timeout |
| Memory | 4 KiB free-page reporting by default, and a reclaim when a VM idles | 604 MiB against 906 for an idle Hermes |
| libkrunfw | Shipped as its own library with its source, as Podman and msb do | Nothing to do until we distribute binaries |

## Acceptance

Measured on finite-lat-6 under the spike's rules (docs/krun-spike.md,
Constraints), with evidence in `docs/krun-spike-evidence/`.

1. **The engine API is Cloudflare's container API**, every call, with
   Cloudflare's validation and limits:
   - `start({image | containerSnapshot, entrypoint, env, enableInternet,
     instance, labels})`, with instance `lite`, `basic`, `standard-1` to
     `standard-4`, or custom `{vcpu, memoryMib, diskMb}`;
   - `running`, `inspect`, `destroy(error)`, `signal(n)`, `monitor`;
   - `exec(cmd, {stdin, stdout, stderr: pipe | ignore | combined, cwd,
     env, user, pty})`, with `kill(signal)`, `resize`, exit codes, and
     exec's env not inheriting `start`'s except `PATH`;
   - `getTcpPort(p)` for any port, both `fetch` and `connect`;
   - `snapshotContainer({name})` returning `{id, size, name}`, restored by
     `start({containerSnapshot})`, image-tied, kept 30 days and refreshed
     on restore;
   - `interceptOutboundHttp` and `interceptOutboundHttps` (host, glob,
     `ip:port`, CIDR, `*`, an optional port), and `interceptAllOutbound`,
     added while running; 128 entries counted as Cloudflare counts them
     (a hostname is two); the CA at Cloudflare's path;
   - logs.
2. **Speed**, each with its median and range:
   - a jailed busybox start to ready at or under the spike's unjailed
     107 ms (now 124 to 129 jailed);
   - exec under 5 ms;
   - a guest port at least 1 GiB/s if the NIC path holds (otherwise the
     reason recorded);
   - a snapshot and a restore timed against the bytes changed;
   - Hermes to serving no slower than 4.37 s.
3. **celld on the engine.** A conformance Worker calling every method of
   `ctx.container`:
   - passes on celld with this engine;
   - passes on celld with Docker for every method Docker supports;
   - is ready to run on Cloudflare (Paul's account).

   Intercepted requests reach the Durable Object through celld's callback
   route.
4. **Images.** These build and run as Docker runs them: busybox, alpine,
   debian, ubuntu, python, node, nginx, Hermes, and
   `cloudflare/sandbox`, whose control server serves. File capabilities
   are kept (ping as a non-root user), and a `docker save` tar loads.
5. **Hardening:**
   - the seccomp allowlist holds every scenario, and the escape probe
     still reaches nothing;
   - a restart of the engine keeps its running VMs (adopted, not
     killed);
   - a crash of any piece leaves nothing that `reset` cannot clear;
   - limits on VMs and memory per node, refused with a typed error.
6. **Clean:**
   - clippy with warnings denied and every test, on the Mac, on Linux,
     and in CI;
   - lat-6 undisturbed, shown before and after.

## Constraints

The spike's musts and must-nots hold (docs/krun-spike.md): its own
directory, prefix, slice, and uid range on lat-6; nothing public;
`sandcastled`, msb, `iroh-relay`, the firewall, and other sessions' work
untouched; no secrets on lat-6.

New this time:

- **The engine runs as root**, as a transient unit in the spike's slice
  with a cgroup subtree of its own: `sudo systemd-run --unit=krun-engine
  --slice=krun-spike.slice -p Delegate=yes`. Nothing is installed.
- **Its socket** is in the spike's directory, owned by `ubuntu`.
- **The celld fork** changes on a branch of `futurepaul/celld`. fragment
  pins that branch's commit only on `krun-spike`.
- **Escalate** a need to change anything outside the spike's slice and
  directory, a shipping or licensing question, and a Cloudflare account
  action.

## Phases

Each phase ends with its tests green, its numbers in Results, and a
commit.

1. **E1, the engine.** A crate, `sandcastle-engine`, with a library and
   a `sandcastle-engine` binary:
   - It holds a table of VMs and a state directory.
   - It forks the jailer directly and makes each VM's cgroup itself
     (`memory.max`, `cpu.max` for fractional vCPUs, `pids.max`).
   - It serves an API on a unix socket: HTTP/1.1 with JSON, and upgraded
     streams for exec and ports.
   - Images: pulls, loads `docker save` tars, and builds each image once
     per digest.
   - The driver becomes a client of the engine. Start latency is
     measured against the spike's.
2. **E2, parity calls:**
   - instance types and their limits;
   - labels;
   - `inspect`;
   - `signal` to the entrypoint;
   - `monitor`'s exit code;
   - logs: the entrypoint's stdout and stderr, apart from the console,
     bounded and rotated;
   - exec's `stdout`/`stderr` modes, env rule, and PTY sizes;
   - intercept rules added while running, counted as Cloudflare counts
     them;
   - snapshots (create, restore, list, expire).
3. **E3, images:** xattrs, device nodes, multi-frame zstd, `docker save`,
   and the corpus.
4. **E4, speed:** guest ports over the NIC against vsock, scratch disks
   from a template, and the start path profiled down.
5. **E5, hardening:**
   - the seccomp allowlist;
   - adoption across an engine restart;
   - limits per node;
   - crash recovery;
   - the escape probe again.
6. **E6, celld:**
   - an engine trait in the fork, with Docker and this engine behind it;
   - the callback route for intercepted requests;
   - `inspect`, `snapshotContainer`, and the intercepts wired to the
     engine;
   - the conformance Worker, run on lat-6 against both engines.
7. **E7, results:** CI, reset, and the evidence.

## Evaluation

- **Host tests** for every pure piece:
  - the API's validation at each limit;
  - instance arithmetic;
  - the intercept accounting;
  - snapshot expiry;
  - cgroup values;
  - adoption from the state directory;
  - valid and invalid paths, and replay where a call can repeat.
- **The driver on lat-6**: each acceptance item as a scenario, its
  evidence as JSON.
- **The conformance Worker**: the same file run against every engine,
  method by method, with each result recorded.
- **Honesty:** the stand-in handler goes away once celld's callback route
  exists, and nothing measured through a harness is reported as the
  product's number.

## Results

*(Filled as each phase lands.)*
