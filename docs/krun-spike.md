# The libkrun spike

*Plan, 2026-09-30 (Paul: "can you do this on lat-6 without disrupting
the existing work? new worktrees / branches for everything until we've
proved it. and make sure we follow engineering-style").* Why:
docs/sandcastle-on-libkrun.md. The bar: docs/containers-on-celld.md
(Cloudflare's container capabilities). Paul also sees what this
becomes: a close-to-the-metal alternative to Docker for celld's
container API, so it is built as an engine with Cloudflare's container
semantics, not a sandcastle-only shortcut.

This is a **spike**: a lower-rung diagnostic that proves or disproves one
design on real hardware and records the evidence. It is written to the
engineering style, so what it proves can become the product without a
rewrite. It merges nowhere until Paul decides.

## Problem

sandcastle runs its computers through msb's CLI, and fights it: a
sandbox outlives its entrypoint, ports are fixed at create, interception
only substitutes values, a flushed pause is refused for PID-1 inits,
freed memory never returns, and the VM process runs as the node's user
in the host's namespaces. The question is whether running libkrun
directly, with our own jail, roots, guest agent, and network, gives
Cloudflare's container semantics exactly, at msb's speed or better, and
isolates each VM from the node and from each other.

## Acceptance

The spike is done when `docs/krun-spike.md`'s Results section holds each
of these, measured on finite-lat-6 (10 samples where cheap, median and
range), with the evidence the driver printed:

1. **Boot.** A busybox guest's boot to its agent ready; the Hermes image
   (`nousresearch/hermes-agent:v2026.9.24`) from its OCI image to serving
   on its port. Compared with msb: Hermes cold 6.0 to 6.2 s.
2. **Pause and resume.** Each, timed; a connection that arrives while
   paused resumes the VM and is served. Compared with msb: 8 ms each.
3. **Exec.** A round trip; a PTY with resize; stdin; kill; the exit code.
   The entrypoint's own exit code, reported, and the VM stopped then.
   Compared with msb: 9 to 20 ms.
4. **Any port.** A connection to a port nobody declared: first byte, and
   throughput over 100 MB; HTTP and a WebSocket through it.
5. **Interception.** From the guest, with no proxy setting: an HTTPS
   request to a named host handed to a handler, which answers it; a
   placeholder substituted with a secret; with the internet off, a name
   that is not intercepted does not resolve; a private address refused.
6. **Memory back.** An idle Hermes' resident memory with the balloon's
   free-page reporting, against msb's 892 to 925 MiB.
7. **Isolation.** From inside a VM process's jail, an escape probe finds
   the node's key, the node's state, `ubuntu`'s home, and another VM's
   disk unreachable, and sees its own uid, not `ubuntu`'s.
8. **Fresh roots.** Two starts from one image see a fresh root each
   time (a file written in the first is gone in the second), and
   `/data` keeps it.
9. **The build is clean.** Every new crate passes `cargo clippy
   --workspace --all-targets --all-features -- -D warnings` and `cargo
   test --workspace --all-features` from `sandcastle/`, on the Mac and
   in CI, with no libkrun installed there.
10. **Nothing else moved on lat-6** (the constraints below), shown by the
    other services' start times and datasets before and after.

## Constraints

### Musts

- **Separate everything on lat-6:**
  - source and builds under `/home/ubuntu/krun-spike/` (a checkout of
    this branch);
  - libkrun and libkrunfw built at pinned commits into
    `/home/ubuntu/krun-spike/prefix/`, loaded from there by path, never
    installed system-wide (msb's own `libkrunfw.so.5.6.1` is untouched);
  - ZFS under `tank/krun-spike` only, with a 60 GiB quota;
  - VMs under a `krun-spike.slice` with `MemoryMax=8G` and
    `CPUQuota=800%`. The host has 124 GiB and `sandcastled`'s unit may
    take 113 GiB, which leaves 11 GiB; 8 keeps 3 for the host.
  - At most two spike VMs at once: Hermes at 2 vCPU and 4 GiB, test
    guests at 1 vCPU and 512 MiB.
- **Nothing public:** nothing binds a non-loopback address, and the
  firewall is unchanged. The driver runs on lat-6 itself.
- **The engineering style** (below), from the first commit.
- **Pinned and audited dependencies**, each named in this doc with why.
- **One reset:** `sandcastle-krun-spike reset` removes `tank/krun-spike`'s
  children and any spike VM, mount, or network namespace left behind.
  It is the only cleanup, and it deletes whole explicit roots.
- **Every privileged action is listed here, and there are no others:**
  - `sudo zfs create|set|clone|destroy`, under `tank/krun-spike` only;
  - `sudo systemctl set-property --runtime krun-spike.slice MemoryMax=8G
    CPUQuota=800%` (not kept across a reboot);
  - `sudo systemd-run --slice=krun-spike.slice --scope sandcastle-vm jail
    …`: the jailer, which drops to the VM's own uid before libkrun runs;
  - `apt install` of build-only packages, and only if libkrunfw's kernel
    build needs them (flex, bison, and the like), each named in Results.
  - `rustup target add x86_64-unknown-linux-musl` is `ubuntu`'s own, not
    the system's.

### Must-nots

- Touch `sandcastled` (its unit, binary, state, `tank/sandcastle`,
  `/etc/sandcastle`), msb (`/home/ubuntu/.microsandbox`,
  `/home/ubuntu/.local/bin/msb`), `iroh-relay`, the firewall, or the
  earlier spikes in `/home/ubuntu/spike*`. Restart none of them.
- Stop or change the paused Hermes computer on lat-6 (another session's).
- Merge to master, deploy anything, or put secrets on lat-6. Model calls
  go to a stand-in handler, labeled as one.
- Depend on fragment's crates or on `sandcastle-node` from the new
  crates: they must be able to leave for celld's ecosystem.

### Preferences

- libkrun's newer builder API (`krun_vmm_builder_*`, with a pause and
  resume handle and a balloon) over the 1.x context API, if a pinned
  commit builds and boots.
- TSI networking first: the VMM opens the guest's sockets inside the
  VM's network namespace, so nftables and a transparent redirect apply
  unchanged. virtio-net and a tap in that namespace is the fallback.
- Load libkrun at run time (`dlopen` through `libc`), so nothing links it
  at build time and CI builds without it.
- Keep Linux-only code (namespaces, seccomp, vsock, the guest) behind
  `cfg(target_os = "linux")`, so the workspace still builds and tests on
  the Mac.
- Build libkrunfw from its pinned source. Copying msb's own
  `libkrunfw.so.5.6.1` into the prefix is the fallback if that build
  blocks, labeled as such in Results.

### Escalations (stop and ask Paul)

- No pinned libkrun offers pause, resume, and the balloon together.
- The jail needs more privilege than the one root jailer step: this is
  the production privilege model, and it is Paul's call.
- Any need to touch what the must-nots name, open a port, or pass the
  budget.
- TSI escapes the namespace's rules and interception needs virtio-net and
  a userspace stack: a larger scope.
- libkrunfw's licensing (a GPL kernel inside a shared library) bears on
  shipping it.
- A real model call would need a key on lat-6.

## Layout

On branch `krun-spike` (worktree `../fragment-next-worktrees/krun-spike`,
stacked on `two-substrates`), new crates in the `sandcastle/` workspace,
none depending on `sandcastle-node` or fragment:

| Crate | What | Tested on the host |
|---|---|---|
| `sandcastle-wire` (`crates/wire`) | the host's and guest's protocol over vsock: length-prefixed frames, typed messages, every limit a constant | encode and decode, round-trip, oversized and malformed frames refused, a fuzz target |
| `sandcastle-guest` (`crates/guest`) | the guest's init and agent, one static binary (`x86_64-unknown-linux-musl`): mounts, the CA, the entrypoint in a PID namespace as its PID 1 with its exit code reported, exec, port connections, sync and freeze | the mount plan and the exec state machine as pure functions |
| `sandcastle-vm` (`crates/vm`) | the runner: a pure lifecycle (created, booting, ready, paused, stopped) and its validated configuration; the libkrun gate (`dlopen`); the jailer (namespaces, uid and gid maps, the mount namespace, seccomp, the cgroup) | the lifecycle's transitions, valid and invalid; configuration refused at every limit |
| `sandcastle-egress` (`crates/egress`) | the rules (host, glob, `ip:port`, CIDR; 128 entries, as Cloudflare), the nftables rendering, and the intercepting proxy with the node's CA | rule matching at the limits; the proxy on loopback, a handler answering |
| `sandcastle-rootfs` (`crates/rootfs`) | an OCI image pulled and unpacked once per digest (whiteouts applied), made an ext4 image (`mke2fs -d`), written to a zvol | manifest and layer parsing; whiteouts; digests checked twice (on download and before use) |
| `sandcastle-krun-spike` (`crates/krun-spike`) | the driver: each scenario below, its evidence as JSON, and `reset` | none: it is the spike |

### Dependencies (to audit before adding)

- libkrun and libkrunfw from `github.com/containers`, at pinned commits,
  built on lat-6. Recorded: the commits, their licenses, and the API used.
- Rust, already in the workspace: `tokio`, `hyper`, `rustls`,
  `tokio-rustls`, `rcgen`, `serde`, `serde_json`, `sha2`, `thiserror`.
- New, each audited: `libc` (namespaces, `dlopen`, seccomp's `prctl`);
  a `tar` reader and gzip and zstd decoders for layers. An OCI registry
  client is written over hyper (tokens, manifests, blobs), not pulled in.
- On lat-6's host: `mke2fs` (e2fsprogs 1.47.0), `nft`, `zfs`. Present.

## Phases

Each phase ends with its tests green and its numbers recorded before the
next begins.

1. **Pin, build, boot.**
   - libkrun and libkrunfw at chosen commits, built into the prefix.
   - `sandcastle-wire` and the runner's pure lifecycle, with their tests.
   - A busybox root made an ext4 zvol, cloned per start.
   - `sandcastle-guest` as init says it is ready over vsock.
   - *Evidence:* boot to ready (10); a fresh root each start (acceptance
     8, the root half).
2. **The jail.**
   - The root jailer creates a user namespace mapping a per-VM uid from
     `ubuntu`'s subordinate range (100000 to 165535), keeps the `kvm`
     group, makes a mount namespace holding only `/dev/kvm`, the VM's
     disks, and the libraries, a network namespace, and the cgroup; then
     drops privileges and applies seccomp before libkrun starts.
   - The escape probe runs in the same jail in place of the VM.
   - *Evidence:* acceptance 7. *Tests:* the jail's plan (maps, mounts,
     rules) as pure data, refused when it would leak.
3. **Exec and exit.**
   - The agent's exec: streams, PTY and resize, stdin, kill, exit codes.
   - The entrypoint supervised as the PID namespace's PID 1: its code
     reported, the VM stopped.
   - *Evidence:* acceptance 3. *Tests:* frames valid and invalid, a
     repeated request id refused, an unknown process, a closed stream,
     the limit on processes at once.
4. **Any port.**
   - A connection over vsock to the guest's `127.0.0.1:<port>`; HTTP and
     a WebSocket through it.
   - *Evidence:* acceptance 4.
5. **The network and interception.**
   - TSI in the VM's namespace: nftables for public-only egress (the
     node's own addresses denied) and redirection of 80 and 443 to the
     proxy, in the same namespace, jailed with it.
   - The proxy matches the rules, substitutes a placeholder or calls the
     handler (a stand-in on a unix socket, labeled as the stand-in for
     celld's callback).
   - With the internet off, DNS answers only for intercepted names.
   - *Evidence:* acceptance 5. *Tests:* the matcher at every boundary;
     the rendering of rules; the proxy against a loopback handler.
6. **Hermes.**
   - The real image pulled, unpacked, and made a root; a `/data` zvol;
     s6's `/init` under the guest's init.
   - Its port served through phase 4's path.
   - Pause and resume, and a connection waking a paused VM.
   - The balloon's reporting on, and its resident memory idle.
   - *Evidence:* acceptance 1 (Hermes), 2, 6, and 8 (`/data` kept).
7. **Crash and reset.**
   - `kill -9` of a VM mid-run leaves no process, mount, or network
     namespace behind once reset runs, and `/data` is intact.
   - The driver's `reset` restores lat-6 to its before state.
   - *Evidence:* acceptance 10.

## Evaluation

- **Host tests** (CI and the Mac): every pure piece above, each mutation
  with a valid and an invalid path, frames round-tripped, limits
  exercised at the edge and one past it, and a fuzz target for the frame
  decoder.
- **The driver on lat-6**: each scenario prints JSON evidence (samples,
  median, range, versions, commits) that goes into Results as is.
- **Honesty:** the stand-in handler is named as one, and no number from
  a harness is reported as the product's.

## How the engineering style applies here

- **Limits, named and asserted:** a frame (1 MiB), processes per VM
  (64), port connections per VM (256), intercept rules (128), boot
  (60 s), a paused VM's resume (1 s), each an asserted bound or a typed
  refusal; loops over bounded sources say so.
- **Typed errors** per crate (`thiserror`), matched by callers; no
  `anyhow` outside the driver.
- **Assertions stay on in release** (the workspace's profile already
  aborts on panic and checks overflow). About two a function: what it
  assumes on entry, what it produces on exit. Paired where data crosses
  (a frame checked when written and again when read; an image digest
  checked on download and again before use).
- **No recursion; iterative, bounded state machines** (the lifecycle,
  the exec session, the proxy's connection).
- **Pure core, narrow gates:** the lifecycle, the jail's plan, the rules,
  and the mount plan are pure and host-tested; KVM, ZFS, and the network
  come in through gates.
- **Names:** nouns, units last (`boot_ms_max`, `frame_bytes_max`).
- **Comments say why; tests say how** (a preamble: goal, then method).
- **clippy with warnings denied**, from the first commit.
- **Hard cuts:** nothing here keeps msb's shape for compatibility; if the
  spike holds, the engine replaces msb's gate behind sandcastle's engine
  trait, with msb kept only as an alternate engine until parity.
- **Debt:** anything the spike skips on purpose gets a ledger entry
  before it could ship.

## Results

*(Filled as each phase lands.)*
