# sandcastle on libkrun, without msb

*An investigation, 2026-09-30.* Paul: "I'm a little worried how much
we're fighting msb… what would it take to build sandcastle on top of
libkrun directly, with exactly our use case in mind?" Read with
docs/containers-on-celld.md, whose bar (Cloudflare's container
capabilities) this assumes.

## Where we fight msb

Each from this repository's history or the audit's probes on lat-6:

1. **The CLI is the boundary.** Every engine call is a subprocess,
   parsed from text or JSON, across msb versions. Secret configs are left
   behind by a create that does not finish (`remove_stray_secret_configs`).
2. **A sandbox outlives its entrypoint.** `msb create` keeps the VM up
   whatever the entrypoint does, and `msb wait` reports no exit code.
3. **Ports.** Forwards are set at create (`-p`); `msb modify` cannot add
   one; the host has no route to a guest's address.
4. **Interception** substitutes a secret's value and cannot hand a
   request to code.
5. **Pause.** A flushed pause is refused for an image whose init takes
   PID 1, and a graceful stop of a paused machine is refused.
6. **Memory.** A guest's freed memory is never returned (a warm Hermes
   holds about 900 MiB, twice an idle one's need).
7. **Isolation** (checked on lat-6). The VM process (`msb machine`, the
   libkrun VMM) runs as the node's user (uid 1000, the daemon's), with no
   capabilities and no new privileges, but no seccomp, and in the host's
   user, mount, network, and pid namespaces. libkrun's own security model
   says "both the guest and the VMM pertain to the same security context"
   and that the host must isolate the VMM, so today a guest that escapes
   into its VMM is the node's user: the node's key, its state, and other
   computers' disks are in reach. This stands whatever we decide
   (docs/technical-debt-ledger.md).

Most of these are msb's product choices (a sandbox as a long-lived thing
you exec into, substitution as the swap, a CLI as the API), not limits of
libkrun.

## What libkrun gives directly

Its current C API (`include/libkrun.h`, a builder):

- vCPUs and memory; a payload: libkrunfw's bundled kernel, or an
  external kernel with an initrd;
- virtio-blk disks (read-only, direct I/O, sync mode), virtio-fs shares,
  virtio-net (a unix datagram or stream socket, or a tap), vsock (port
  forwards, unix ports), consoles, **a balloon with free-page
  reporting**, rng, GPU;
- a handle, usable from another thread: **pause, resume**, shutdown;
- `krun_vmm_run` takes over its process and never returns: one process
  per VM;
- Linux on KVM and macOS on HVF; SEV and TDX variants;
- TSI (transparent socket impersonation) as an alternative to virtio-net:
  the guest's sockets are opened by the VMM on the host.

It has no memory snapshot or restore. msb's "full snapshots" are msb's
own, and Cloudflare has none either.

## What sandcastle would build

| Piece | What | What it replaces |
|---|---|---|
| **A VM runner** | `sandcastle-vm`, one process per computer, controlled over a socket (pause, resume, shutdown), **jailed**: its own uid (a user namespace), a mount namespace holding only its disks, its own network namespace, seccomp, a cgroup with its memory and CPU | `msb machine`, unjailed |
| **Roots** | an OCI image pulled and unpacked once per digest into an ext4 image on a zvol; each start a `zfs clone` of it, a fresh root in milliseconds (Cloudflare's semantics exactly), destroyed at stop; `/data` its own zvol, as now; snapshots are ZFS's | msb's image store and root disks |
| **An init and agent in the guest** | one static binary on a small read-only disk: mounts the root and `/data`, puts the CA where Cloudflare's images expect it (`/etc/cloudflare/certs/…`), runs the entrypoint in a PID namespace as its PID 1 (so s6's `/init` works) and reports its exit code. Over vsock: exec (streams, a PTY and its resize, kill), a connection to any guest port, files, and a sync and freeze before a pause or a snapshot | agentd, `msb exec`, port forwards, the flush refusal |
| **The network** | a network namespace per VM: nftables for the egress policy (public only, the node's own addresses denied, allow and deny lists) and transparent redirection of 80 and 443 to sandcastle's proxy, which intercepts TLS with the node's CA and either substitutes a secret or hands the request to a handler (celld's callback). With TSI the VMM opens the guest's connections, so the namespace's rules apply unchanged | msb's network stack, its swap, and its policy |
| **Memory back** | the balloon, with free-page reporting | none (debt) |
| **Logs** | a console to a file | `msb logs` |

Dropped: memory snapshots. The warm tier (a paused VM, 8 ms to resume)
covers fast wakes, and Cloudflare offers neither.

sandcastle's pure core and its simulator do not change: the engine is a
gate (`sandcastle_node::gates`), and a libkrun gate replaces the msb
gate behind it.

## What it would take

Rough, with the engineering style's tests (valid, invalid, replay,
restart) for each:

| Piece | Weeks |
|---|---|
| The runner, its jail, and its cgroups | 1 to 2 |
| Images into ZFS roots | 1 |
| The init and agent, their protocol, the host's client | 2 |
| The network namespace, nftables, DNS, the intercepting proxy | 2 |
| The gate, the lat-6 e2e, the simulator's engine model | 1 to 2 |
| **In all** | **7 to 9** |

## The middle paths

- **microsandbox's crates as libraries** (Apache-2.0): `msb_krun` (its
  libkrun wrapper), `microsandbox-network` (a smoltcp stack with policy,
  DNS, TLS interception, and placeholder secrets, but no handler hook:
  we would patch one in), and agentd. Less to build, but their
  semantics and their jail stay ours to fight.
- **msb's Rust SDK instead of the CLI** (`microsandbox` 0.7.5): typed exec
  events (an `Exited` event carries the code), file operations, network
  policy, fork. It still drives the external `msb` runtime, so it keeps
  the substitution-only swap, the port model, and the VMM's isolation.

## Recommendation

A spike first, three to five days on lat-6: `sandcastle-vm` booting the
Hermes image from a ZFS-cloned root, with the init and agent over
vsock and TSI inside a jailed namespace. Measure:

- boot to serving;
- pause and resume;
- exec;
- a connection to an undeclared port;
- a transparent HTTPS intercept handed to a handler;
- the balloon returning an idle guest's memory.

If it holds, a libkrun gate replaces msb's behind sandcastle's engine
trait, with msb kept as an alternate engine until the e2e passes on both.
Finding 7 is closed first either way. With libkrun, the runner's jail
closes it. While msb remains, a smaller step narrows it: msb and its VMs
run as a user of their own, apart from the daemon that holds the node's
key, its state, and the backup key, so an escape into a VMM reaches
neither.
