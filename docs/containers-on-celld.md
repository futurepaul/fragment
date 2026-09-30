# Cloudflare's container API, self-hosted

*An audit, 2026-09-30.* Paul asked what sandcastle and our celld fork
must change so that self-hosting feels as if it already had Cloudflare's
new container API, with no second codepath for Cloudflare and
self-hosted. Read with docs/two-substrates.md.

## The finding

**Upstream celld v0.6.0 already has `ctx.container`**, on a local Docker
or Podman Engine (over its unix socket). It runs `@cloudflare/containers`
and `@cloudflare/sandbox` 0.12.9 as published (celld's
`docs/services/containers.md`, `examples/container`, `examples/sandbox`).
Its JS surface is `js/harness.js:2387-2536`, its ops
`js/container.rs`, its engine `container.rs`.

So the seam is not ours to invent. fragment writes a computer against
`ctx.container` alone, and that one codepath runs:

- **on Cloudflare**, natively;
- **on celld with Docker or Podman**: the dev stack and CI, on a laptop;
- **on celld with sandcastle**: self-hosted production, on microVMs.

docs/two-substrates.md's computer-host trait collapses to one
implementation over `ctx.container`, and `SandcastleHost` becomes
celld's sandcastle engine.

## The API, method by method

What Cloudflare's reference defines (`ctx.container`, the Durable Object
scheduling policy), what celld has, and what each side must add. "Engine"
is the sandcastle side.

| Cloudflare | celld v0.6.0 | celld must | sandcastle must |
|---|---|---|---|
| `running`, `monitor()` (resolves at exit 0, rejects with `exitCode`) | has: Docker's `/wait` | take exits from the engine | report exits with their codes (`msb wait` has none yet) |
| `start({image, entrypoint, env, enableInternet, labels})` | has | send them to the engine | run a computer shaped like a container: a fresh root each start (`msb --root-disk`), the image's entrypoint or an override (`--entrypoint`), env, labels, the network default (`--no-net`, `--net`) |
| `start({instance})`, `{vcpu, memoryMib, diskMb}` | no: sizes come from the config's `instance_type` | accept them, and refuse what Cloudflare refuses (at least 3 GiB per vCPU; at most 4 vCPU, 12 GiB, 20 GB), so code that runs here runs there | take a size per start |
| `images` (named, digest-pinned) | no | the map, from the `containers` config | load images from celld's bucket (`msb load`) or a registry |
| `destroy(error?)`, `signal(n)` | has | send them to the engine | a signal endpoint |
| `setInactivityTimeout(ms)`, at most 6 h | has (10 min kept by default) | the same | stop at the object's timeout; its own warm pause stays beneath, invisible while "running" |
| `exec(cmd, {stdin, stdout, stderr, cwd, env, user, pty, signal})`, `kill`, `resize` | has (Docker exec, streamed) | stream it from the engine | a streamed exec (`msb exec`: a PTY, or byte-faithful stdin and stdout; env, workdir, user, timeout) |
| `getTcpPort(p).fetch()`, `.connect()`, WebSockets included | has, by rewriting the host to the container's address and calling the global `fetch` (which the fork's public-only egress refuses for a bridge address) | ops that return a stream (`js/tcp.rs` takes any stream) from the engine, in place of the address | a tunnel to any guest port, HTTP and raw TCP, for celld alone (see Open questions) |
| `inspect()` | a stub | `{image, labels}` from the engine | its view has them |
| `snapshotContainer({name})`, `start({containerSnapshot})` | stubs | the handle `{id, size, name}`, kept by the object; a 30-day TTL, refreshed on restore | `msb snapshot create` (a disk snapshot, or `--full` with memory and execution state), exported to the bucket (`msb snapshot export`), restored into a new computer |
| `interceptOutboundHttp/Https(addr, fetcher)`, `interceptAllOutbound*` | stubs | validate the Fetcher as the Worker Loader's `globalOutbound` is (`__outboundMeta`); keep the rules; deliver each intercepted request to it (`runtime.fetch_service`) | per computer: a proxy that intercepts TLS, its CA at Cloudflare's path (`/etc/cloudflare/certs/cloudflare-containers-ca.crt`); the rules (host, glob, `ip:port`, CIDR; 128 entries); with `enableInternet: false`, DNS only for intercepted names; each match sent to celld's callback |

## celld: what the fork (or upstream) changes

1. **An engine trait.** `ContainerEngine` (`container.rs:288-306`) is a
   struct over Docker. It becomes a trait, Docker or Sandcastle, covering
   `engine()`, `attach`, `start`/`create_and_start`, `watch_exit`,
   `destroy`, `signal`, `release`, `exec`, `container_name`, and memory
   accounting. The sandcastle URL and key are read where the runtime is
   configured (`runtime.rs:793`).
2. **A container follows its object, not its node.** Today a container's
   name hashes the node and the scope, and a move destroys it
   (`container.rs:13-19, 1584-1588`). With a remote engine:
   - its name is the scope's;
   - the activation's epoch (`start_cell(cell, epoch, ..)`) goes to
     sandcastle as a fencing token, so a new owner adopts the container
     and a stale one is refused;
   - a remote container counts no memory against the node
     (`container.rs:106-117, 832-840`), or the node sheds cells for
     memory it does not hold.
3. **Ports as streams.** `__container_fetch` and `__container_connect`
   ops return a signed stream to the engine's tunnel, registered through
   `js/tcp.rs`'s `insert`, in place of `__container_address` and the
   global `fetch`.
4. **The stubs, filled:** `inspect`, `snapshotContainer` with
   `start({containerSnapshot})`, and the intercepts; and the start
   options above, validated as Cloudflare validates them.
5. **A callback route for the engine**, beside `/runtime/` in
   `main.rs:2942-3121`, allowed under `CELLD_INTERNAL_PEER_ONLY`, signed
   with a key of sandcastle's own (on the model of `peer_auth.rs`; never
   the fleet's peer key, which is full peer authority):
   - an intercepted request goes to `runtime.fetch_service` (the
     `globalOutbound` broker's path);
   - an exit goes to `submit_do_call`, resolving `monitor()`.
6. **Deploy:** images published where sandcastle reads them, and no
   fence image when there is no local engine (`deploy.rs:764-850,
   2087-2180`).
7. **Two security fixes found on the way** (see below).

None of this goes through the native seam: `native:*` takes whole
request bodies and cannot carry a WebSocket (`main/native_seam.rs`,
`ws_registry.rs`). It is fine for control calls, not for ports or exec.
The engine trait and the stubs belong upstream: celld calls its
containers experimental, and each gap is a place celld differs from
Cloudflare. The fork carries only what upstream declines.

## sandcastle: an engine API beside its own

celld's fleet key is a grantor and the owner of every computer celld
makes, so grants, NIP-98, and the reserve stay as they are. What is new:

1. **Container-shaped computers:** a fresh root each start, the image's
   own entrypoint, labels, a size per start, and no service port or
   health path required (celld checks readiness itself, as Cloudflare's
   callers do).
2. **Adopt by name, fenced by epoch.**
3. **Exit events with their codes**, which celld long-polls or is sent.
4. **Streamed exec**, over one WebSocket per process: stdin, stdout,
   stderr, a PTY and its resize, kill.
5. **A port tunnel** to any guest port, HTTP and raw TCP, for celld's key
   only.
6. **Signals.**
7. **Snapshots of the whole computer**, in the bucket, restored into a
   new one, kept 30 days from their making or last restore.
8. **Outbound interception with callbacks** (the table's last row): the
   largest new piece, since msb's own swap substitutes values and does
   not hand a request to code.
9. **Images from celld's bucket.**

Beneath the API, sandcastle keeps what makes it good: the warm tier
(paused while "running", so a request wakes it in milliseconds where
Cloudflare boots), the reserve, ZFS, and sealed backups. Off the celld
path it keeps its public router, tickets, `cors_origins`, and credential
source, for platforms that call it directly (Finite's Core).

## What fragment does

- One computer host, over `ctx.container`, on all three.
- `/data` kept as docs/two-substrates.md says: an archive in R2, saved
  through `exec` (`hermes backup`, then the archive) and restored into
  any image. It is the same code everywhere, since celld's R2 is its
  bucket.
- The model's key put in by an outbound intercept, the same everywhere.
- The dev stack and the e2e run computers on Docker or Podman, with no
  Cloudflare account and no lat-6: the `hermes` lane can test against a
  real container.

## Conformance

One test Worker exercises the API as fragment uses it:

- start, exec (a PTY included), `getTcpPort` (`fetch`, `connect`, and a
  WebSocket);
- `monitor` at a clean exit and a failing one, `signal`, `destroy`, the
  inactivity timeout;
- a snapshot and its restore;
- intercepts: HTTP, HTTPS, and `enableInternet: false` resolving only
  intercepted names.

It runs against Cloudflare, celld with Docker, and celld with sandcastle
on lat-6, and each difference it finds is a bug on the self-hosted side,
or a named, documented divergence. celld already tests its conformance
against workerd (`docs/testing.md`); this extends that to containers.

## Order

1. **celld, still on Docker:** the engine trait, the container ops'
   scope check, ports as streams, and the start options validated. The
   conformance suite's first version passes on Cloudflare and celld with
   Docker.
2. **fragment on `ctx.container`,** on celld with Docker in the dev stack
   and the e2e. This replaces two-substrates.md's phase 3
   (`SandcastleHost`).
3. **sandcastle's engine API** (items 1 to 6) and celld's Sandcastle
   engine: the suite on lat-6.
4. **Snapshots**, on both.
5. **Intercepts**, on both. Then fragment's model key goes through an
   intercept on all three.
6. **The Sandbox SDK 1.0** (`@cloudflare/sandbox@next`) on celld: it needs
   the `images` map, snapshots, and the start sizes.

## Security, found on the way

- **The container ops check no scope** (checked first-hand). Unlike
  storage's `sync` (`js/storage_ops.rs:268-286`), `js/container.rs`
  looks its container up by the scope the JS passes (`cell_for`). A cell in a shared isolate (up to 32) holding another
  cell's `ctx.container` could drive that container. It is fixed in step
  1, before containers carry anything.
- **The fork's public-only egress has a bypass** (checked first-hand
  2026-09-30). `CELLD_EGRESS_PUBLIC_ONLY` covers a Worker's own `fetch`
  alone (`egress.rs:1`): `connect()` (`js/tcp.rs:166`) and an outbound
  WebSocket (`ws_client.rs:135`) open raw TCP with no check. A fragment's
  app cannot use it (its `globalOutbound` is null, and celld refuses
  `connect()` then), so only the platform's own code could, toward a host
  someone else chose. It is closed on its own, whatever becomes of
  containers.
- **Docker is not a microVM.** Containers there share the host's kernel
  (celld names gVisor's `runsc` and Kata as runtimes). Docker is for
  development and for people who trust each other. Strangers' computers
  run on Cloudflare or on sandcastle.

## Open questions

- **Reaching any guest port.** sandcastle forwards one declared port per
  computer (`msb create -p`). Can the host reach a guest's address on
  msb's network for any port, or add a forward while the guest runs, or
  does the tunnel run through msb's agent in the guest?
- **Transparent interception:** msb's network rules and gateway DNS can
  hold a guest to the proxy, but transparent redirection of 80 and 443 is
  msb's to offer. Without it the guest is pointed at the proxy
  (`HTTPS_PROXY`), which a program may ignore; `--no-net` with only the
  proxy allowed makes ignoring it fail closed.
- **Exit codes** from `msb wait` ("currently unavailable").
- **Where the engine runs relative to celld:** in the same fleet but on
  other machines (celld on Fly, sandcastle on lat-6 today), or one box
  running both.
- **Upstream:** whether celld takes the engine trait and the filled stubs.
