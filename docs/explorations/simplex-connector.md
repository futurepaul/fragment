# A SimpleX connector outside the computer (spike)

Exploration, 2026-10-05, on branch `spike/simplex-connector` from
master at `2c313c39`. This is option F of docs/durable-computers.md
("F: messengers outside the computer"). Paul asked for it on 2026-10-05:
"the spike to do btw would be simplex". Nothing here is product code.
The spike's code is under `spikes/simplex/`, plus a six-line hook in
`crates/e2e/src/lanes/mod.rs` that adds its section. Where this file
disagrees with docs/cloudflare-v1.md (decision 32), that file wins until
Paul decides; "What it changes in decision 32", below, lists the changes.

Every claim is marked with how it was checked:

- **run**: done in this spike. The command that reproduces it is given.
- **read**: read in a vendor's documentation or source, at the place named.
- **inferred**: follows from what was run or read, but was not seen happening.
- **unknown**: needs a vendor's answer or a hosted run.

All runs used local Docker (OrbStack) on an Apple Silicon Mac and **one
local SMP server**. No public SimpleX server and no third-party account
was used. Latencies are loopback numbers. On the internet, each SimpleX
hop also pays the round trip to the SMP servers, and a proxy server when
SimpleX's private routing is on.

## The short version

- **Recommendation.** Run one small always-on container per linked
  account. It runs SimpleX's own `simplex-chat` core as a bot. Its
  database's home is the connector's Durable Object, which owns
  everything else.
  - The Durable Object posts each message as a record on the person's
    chat, sends the agent's replies back, keeps the read cursor, and is
    the lock that lets only one instance run.
  - It costs about **$2 per account-month at list price** (a `lite`
    container), about half what a Durable Object holding the socket
    itself would cost.
  - It works today: the whole path ran end to end on the platform
    (below).
- **Rollback breaks a connection; a restart does not.**
  - A connector whose database goes back in time stops being able to
    decrypt or send (run).
  - It heals in about 0.2 s if the connector asks SimpleX to
    re-synchronize the ratchet (run). The message that showed the break
    is lost. The person sees it as sent, never delivered (run).
  - A plain crash that keeps the database as it was loses nothing (run).
    So what matters is that the database's newest commit survives.
- **Where this shape falls short of F.**
  - The container's disk is ephemeral, so the Durable Object's copy can
    lag the live database.
  - A planned stop (SIGTERM) loses nothing: save, then exit.
  - An unplanned loss of the host rolls back to the last save. It then
    heals automatically at the cost of the messages in flight.
- **A connector fully inside a Durable Object (pure Workers)** is the
  only shape that could meet F completely (inferred), but it is a large
  build.
  - SMP's transport works from a Worker: userland TLS over `connect()`
    reached SMP's handshake and a PING/PONG (run).
  - Nothing exists to build the rest on: no maintained Rust or
    TypeScript implementation of SimpleX's agent or its post-quantum
    ratchet (read). I estimate 1.5–2 months to a basic one-to-one text
    client (inferred), plus following SimpleX's protocol changes forever.
- **Latency, measured end to end.** From the person's SimpleX client
  through the platform and back:
  - computer awake: **475 ms** median;
  - computer asleep (the record wakes it): **1.96 s** median.
  - SimpleX itself accounts for 35–110 ms each way; the rest is the
    computer's stub agent and its wake.

## The setup (run)

Pinned versions:

- SMP server: `simplexchat/smp-server:v7.0.1@sha256:7d825822839a9d5ee9e9a99563d77a319816c7a8e0b4b7bf4dcebe448897af54`
  (multi-arch index digest, Docker Hub).
- Client: `simplex-chat` **v7.0.3**, the latest stable release on
  2026-10-05.
  - It is the release's Ubuntu 24.04 binary, checked by sha256 in
    `spikes/simplex/docker/chat.Dockerfile`: arm64 `2d2e6235…`, amd64
    `895fb14c…`.
  - The base is `ubuntu:24.04@sha256:534baea6…`.
  - The binary reports `SimpleX Chat v7.0.0.12`.

The code:

| Path | What it is |
|---|---|
| `spikes/simplex/src/sx.rs` | The lab: a Docker network, the SMP server, and simplex-chat clients, each driven over its WebSocket bot API (`-p`). It uses std, serde_json, anyhow and tungstenite only. |
| `spikes/simplex/src/main.rs` | `simplex-spike`: `pair`, `rollback`, `autosync`, `crash`, `coldstart` (the experiments below). |
| `spikes/simplex/lane.rs` | The e2e section `simplex`, which runs only by name: `cargo xtask e2e --only simplex`. |
| `spikes/simplex/smp-probe/` | Question 3's transport probe. The host binary uses tokio; the Worker build uses workers-rs `connect()`. |
| `spikes/simplex/docker/` | The client image. Its entrypoint forwards the bot API, which listens on 127.0.0.1 only, out of the container with socat. |

To run them:

```
cd spikes/simplex && cargo run -- pair      <scratch dir>   # connect, 5 messages each way
cargo run -- rollback <dir> both|inbound|none [connector|person]
cargo run -- autosync <dir>                                  # rollback, then the automatic fix
cargo run -- crash <dir>                                     # SIGKILL mid-burst, no rollback
cargo run -- coldstart <dir>                                 # stop, queue a message, start
cargo xtask e2e --only simplex                               # from the repo root: the platform path
```

## 1. Implementation options outside the Haskell app

### Libraries (read)

| Project | What it implements | State |
|---|---|---|
| SimpleGo (github.com/saschadaemgen/SimpleGo), C, ESP32 firmware, AGPL | The whole client stack: TLS 1.3 with a pinned key hash, SMP queues, the agent handshake, X3DH (X448) plus the double ratchet plus sntrup761, queue rotation, receipts | Beta, active (pushed 2026-09-28). Firmware, not a library. **The only independent client that works with today's network.** |
| simplexmq-js (simplex-chat org), TypeScript | The 2021 SMP: WebSocket transport, RSA keys, 4096-byte blocks | Dead since 2021-10. Incompatible with today's SMP (TLS 1.3, Ed25519, v6–v20). |
| GrizzlT/simplex-smp, Rust | Nothing: `main.rs` is "Hello, world!" | Abandoned (2024). |
| simploxide (crates `simploxide-*` 0.14), Rust | A bot SDK: a WebSocket client to the CLI, or FFI to `libsimplex` | Maintained (listed in simplex-chat's `bots/README.md`). Wraps the Haskell core; implements no protocol. |
| npm `simplex-chat` 7.0.2 (simplex-chat's `packages/simplex-chat-nodejs`) | A node-gyp addon linking `libsimplex`, the compiled Haskell core | Official. Prebuilt for linux-x86_64 (also with Postgres), macOS and Windows; no linux-aarch64. Cannot run in workerd (inferred). |
| `@simplex-chat/webrtc-client` (simplex-chat's `packages/simplex-chat-client/typescript`) | A WebSocket client for `simplex-chat -p` | Marked deprecated in favour of the Node.js library. |

Searches of crates.io and npm for "simplex" and "smp" (2026-10-05)
found no other SMP, agent or ratchet implementation (read).

**So no maintained Rust or TypeScript library implements SMP, the agent
protocol or SimpleX's ratchet.** Every official SDK drives the Haskell
core, either over its WebSocket or by FFI.

### What a minimal client needs, and the size of each piece

The specs were read at simplexmq `stable` `27a37387` (2026-07-31,
v7.0.1), the version of the SMP server used here. The Haskell line
counts are a rough measure of how much code each piece takes.

| Piece | Spec | Haskell today | Judged size |
|---|---|---|---|
| **Transport.** TLS 1.3 restricted to ChaCha20-Poly1305 / X25519 / Ed25519; the router pinned by its identity certificate's SHA-256 (no WebPKI, no SNI); ALPN `smp/1`; 16 KiB padded blocks; both hellos; batching; PING | `simplex-messaging.md` §§ TLS transport, Router certificate, ALPN, Transport handshake | `Transport.hs` 941 lines | **Built here**: ~250 lines of Rust over rustls (run, question 3). |
| **SMP queues.** NEW, KEY or SKEY, SUB, SEND, ACK, GET, DEL; Ed25519 signatures or crypto_box authenticators over `sessionId‖corrId‖entityId‖cmd`; the per-queue X25519 end-to-end layer; optional block encryption inside TLS (v11+); short-link data (LGET) | `simplex-messaging.md`, 1,700 lines (SMP v20) | `Protocol.hs` 2,440 + `Client.hs` 1,471 | About a week (inferred). |
| **The agent.** Duplex connections from two queues (invitation, confirmation, HELLO, or the fast SKEY form); contact addresses; envelopes with message ids and previous-message hashes; receipts; queue rotation (QADD, QKEY, QUSE, QTEST); ratchet re-sync (AgentRatchetKey, EREADY); PQ upgrade rules | `agent-protocol.md`, 734 lines (agent v7) | `Agent.hs` 3,902 + `Agent/Protocol.hs` 2,231 + `Agent/Client.hs` 2,903 + store | 2–4 weeks (inferred). |
| **The PQ double ratchet.** Header-encrypted double ratchet over X448, with sntrup761 KEM (encapsulation key 1,158 B, ciphertext 1,039 B) and AES-256-GCM with a 16-byte IV. WebCrypto has neither X448 nor sntrup761, so both would be wasm | `pqdr.md`, 299 lines | `Crypto/Ratchet.hs` 1,239 + C sntrup761 | 1–2 weeks with test vectors (inferred). |
| **The chat protocol.** `x.msg.new`, `x.info` (profiles), contact requests, zstd-compressed batches (groups out of scope) | simplex-chat `docs/protocol/simplex-chat.md`, 375 lines | part of simplex-chat's 61k lines | Days (inferred). |
| **Files (XFTP)** | `xftp.md`, 676 lines | | Out of scope. |

Total: **about 1.5–2 months to a one-to-one text client (inferred)**.
After that, the client has to keep pace with SimpleX's protocol:

- SMP reached v20 by 2026-05 (read: `Transport.hs`
  `currentClientSMPRelayVersion = 20`).
- The agent is at v7 (read).
- The ratchet-resync RFC was promoted on 2026-03-09 (read: simplexmq
  `rfcs/standard/`).

A third-party implementation chases a moving target maintained by one
team (inferred).

## 2. The simplex-chat core as a bot

### Where its state lives (run)

- **Two SQLite files**, `<prefix>_chat.db` and `<prefix>_agent.db`, from
  `-d <prefix>`.
  - After pairing and a few messages they were 1.27 MB and 0.49 MB,
    1.74–1.76 MB together.
  - No `-wal` or `-shm` files were left after a clean stop.
  - `-k` adds SQLCipher encryption (read: `--help`).
  - simplex-chat's `docs/CLI.md` still says `<prefix>_v1_chat.db`; the
    code writes `<prefix>_chat.db` (read and run).
- **The bot API.**
  - `-p <port>` serves a WebSocket on 127.0.0.1 only (run: `/proc/net/tcp`
    shows it bound to 127.0.0.1).
  - It has no authentication (read: `bots/README.md`, "Security
    considerations").
  - `--create-bot-display-name` makes a bot profile on first start. Then
    `/ad` makes an address and `/auto_accept on` accepts every contact
    request.
  - The bot receives `newChatItems` events and sends with
    `/_send @<contactId> json [{"msgContent":…}]` (run).
- **Footprint** (run, `docker stats`):
  - 18 MiB at start, 31–34 MiB idle, 0% CPU idle;
  - the image is 221 MB, of which the binary is 102 MB.
- **Encryption.** A direct contact is post-quantum by default in v7
  (run: `pqEncryption: true`; the chat's first item reads "quantum
  resistant end-to-end encryption").

### Can it be a stateless worker whose state a Durable Object keeps? (run, inferred)

The cycle is: load the database, start, work, stop, save, under a lock.

- **The cycle itself is harmless** when the saved copy is the newest.
  - Run `rollback <dir> none`: the connector is stopped, its files are
    copied out and back, and it is started. Both directions keep
    working, with no errors and the ratchet state `ok`.
- **A start is cheap.** Run `coldstart` (5 starts):
  - From `docker run` to the bot API answering: **0.46–0.48 s**.
  - A message queued while the connector was down arrived within 1.5 ms
    of the API coming up.
  - Its reply reached the person 23–59 ms later.

**But it has nothing to wake it.**

- SimpleX pushes notifications only through APNs (read: simplexmq
  `Notifications/Protocol.hs`, `data PushProvider`, whose providers are
  APNs dev, prod, test and null).
- A notifier subscription (NKEY, NSUB, then NMSG) is itself a held
  subscription (read: `simplex-messaging.md`).

So a worker that runs only when there is work either:

- polls, at a latency of the polling interval; or
- relies on something else holding a subscription for it, which is
  question 3's always-on cost again, or a partial pure-Workers client
  (inferred).

**And it cannot be made rollback-proof from outside.**

- The core acknowledges messages to the router (ACK deletes them there)
  and advances its ratchets as it runs, before any save of ours.
- A worker that dies between those and our save leaves the Durable
  Object's copy behind, which is exactly the rollback below (inferred
  from the runs below).
- The simplex-chat guide says an exported database must not be used on
  two devices at once, and only the latest copy should be used (read:
  `docs/guide/chat-profiles.md`).

### What a rollback does to a connection (run)

**`rollback <dir> both`**: three messages each way, a snapshot, three
more each way, then the connector is put back to the snapshot.

1. The person's next message fails to decrypt at the connector:
   `rcvDecryptionError "ratchetHeader"`, and the contact's
   `ratchetSyncState` becomes `required`.
2. The connector can then send nothing: `/_send` fails with
   `CMD PROHIBITED "send prohibited"`.
3. The person sees nothing wrong. Their state stays `ok`, and their
   messages show `sndSent`, never `sndRcvd`.
4. Ten seconds later it had not healed by itself.
5. After `/_sync @<id>` from the connector:
   - both sides went through `started`, `agreed` and `ok`;
   - the person was synchronized 108–124 ms after the ask, the connector
     173–230 ms after;
   - the person's chat shows "connection synchronization agreed" and
     "connection synchronized";
   - new messages flowed both ways.
6. **The two messages the person sent while out of sync never arrived**:
   `sndSent` forever on their side, and one "decryption error (header, 2
   messages)" item on the connector's.

**`rollback <dir> inbound`**: only the person spoke after the snapshot,
so the connector had received and acknowledged three messages it then
"forgot". The result was the same: a header decryption error,
`required`, sending prohibited, and the same fix. A rollback across
received messages alone also breaks the connection.

**`autosync <dir>`**: the recovery a connector would run on its own.

- It watches for `contactRatchetSync: required` and asks `/_sync` at
  once.
- Timeline after the person's first message following the rollback:
  - detected at 32 ms;
  - the connector synchronized at 172 ms, the person at 223 ms;
  - the person's next message reached the connector at 301 ms;
  - the connector's reply reached the person at 377 ms.
- The one message that revealed the break was lost.

**`crash <dir>`**: a SIGKILL with **no** rollback. The connector is
killed mid-burst and restarted on the database exactly as the kill left
it, 3 rounds.

- Nothing was lost: its chat held each of the 5 inbound messages once,
  every round.
- The ratchet stayed `ok`, and both directions worked afterwards.
- One round logged a harmless `INTERNAL SEMsgNotFound "setMsgUserAck"`.
- **But the bot API had announced only 1–3 of the 5.** The rest were
  committed before the kill, and their `newChatItems` events died with
  it and were never sent again.
- So a connector must read the chat from its own cursor
  (`/_get chat @<id>` after its last item id) when it starts, not trust
  events alone.

So the core commits before it acts, and **a durable database is enough;
an old one is not**.

### Ratchet re-synchronization (read)

Sources: simplexmq `rfcs/standard/2026-03-09-resync-ratchets.md`
(implemented 2023-06-30 in #774, shipped in chat v5.2), and
`cryptoErrToSyncState` in `Agent/Protocol.hs`.

- **When the agent asks for it.** RATCHET_HEADER, RATCHET_SKIPPED and
  RATCHET_SYNC errors set `RSRequired`. DECRYPT_AES, DECRYPT_CB and
  RATCHET_EARLIER set `RSAllowed`.
- **What the two sides exchange.** Each side sends new ratchet keys in
  an `AgentRatchetKey` envelope, encrypted only with the per-queue key.
  `EREADY` follows.
- **It is never automatic.** It starts from the API (`/sync`, `/_sync`,
  `force=on`).
- **Messages that fail to decrypt are lost.** They are acknowledged off
  the router after the RSYNC event, and nothing resends them.

All of this matches the runs.

## 3. Transport from a Durable Object

### `connect()` with the platform's TLS: no (run, read)

Run `smp-probe` as a Worker under `wrangler dev` (wrangler 4.145.0,
the repo's pinned Node), mode `on`:

| Target | Result |
|---|---|
| example.com:443 | `opened` resolves, and a write and a read work |
| self-signed.badssl.com:443 | "proxy request failed, cannot connect to the specified address" |
| the local SMP router | The same failure |

A router's chain is self-signed, and clients pin it by fingerprint.

- The SocketOptions documentation lists only `secureTransport` (`off`,
  `on`, `starttls`) and `allowHalfOpen`. There is no CA, no pin, and no
  peer-certificate or channel-binding accessor (read:
  developers.cloudflare.com/workers/runtime-apis/tcp-sockets/, updated
  2026-06-19).
- That page also forbids localhost and private IPs, so a hosted run needs
  a public router (read). The local runs only worked because workerd
  under `wrangler dev` allows 127.0.0.1.

### `connect()` plain, with TLS 1.3 in wasm: yes, as far as PING (run)

`spikes/simplex/smp-probe` (rustls 0.23 with ring, built for
`wasm32-unknown-unknown` by worker-build, about 1.0 MB of wasm) connects
with `secureTransport: "off"` and runs TLS itself:

- TLS 1.3 with ChaCha20-Poly1305 and X25519;
- the router's identity certificate pinned by SHA-256;
- the TLS 1.3 signature verified;
- no SNI, and ALPN `smp/1`.

Then SMP's hellos, and a PING:

```
secureTransport off + rustls (wasm32), 127.0.0.1:15223
alpn: Some("smp/1")
cipher: Some("TLS13_CHACHA20_POLY1305_SHA256")
router SMP versions: 6..=20
session identifier: 32 bytes
certificates in the router's hello: 2
PING answered with PONG: true
```

- Local times were 3–36 ms for the whole probe (5 runs). Workers'
  clock advances only across I/O, so these are coarse.
- The same code on the host (tokio) took 18.5 ms.
- A wrong fingerprint is refused: "the router's identity certificate is
  not the pinned one".

To build ring for wasm32 the probe needs a clang that targets it. Nix's
clang 21 and llvm-ar did; Apple's clang does not
(`CC_wasm32_unknown_unknown`, `AR_wasm32_unknown_unknown`).

What the probe leaves out:

- **The session identifier is taken from the router's hello.** SMP's
  client asserts that it equals `tls-unique`, the client's Finished
  message (read: `Transport.hs` `withTlsUnique`), and rustls exposes no
  `tls-unique`. SimpleGo does the same as the probe (read: its
  `main/main.c`). Inside a TLS session already authenticated by the pin,
  that loses little (inferred).
- **No client key is sent**, so the router adds no block encryption
  inside TLS. The router accepts that (read: `smpTHandleServer`, where a
  missing key means no `encryptBlock`).
- **The leaf certificate's signature by the identity certificate is not
  checked.**

### Staying subscribed (read, inferred)

- **Socket lifetime.** An open TCP socket keeps a Durable Object in
  memory, and billed, "for up to 15 minutes per connection"; after that
  it no longer keeps it alive (read: the tcp-sockets page, and the
  Durable Object lifecycle page, updated 2026-09-30).
- **Restarts.** A Durable Object also restarts on deploys and runtime
  updates (read: the lifecycle page).
- So a Durable Object holding a subscription must:
  - keep itself busy, for example with an alarm well under a minute and
    PINGs;
  - reconnect and send SUB again after each restart (inferred).
- Its state lives in its SQLite, so reconnecting is safe (inferred).
- Not run: local workerd does not evict as production does.

### What a connected account costs

**A Durable Object holding the socket** (read: Durable Objects pricing,
updated 2026-09-30):

- Duration is "$12.50/million GB-s" beyond 400,000 GB-s a month, billed
  "for the 128 MB of memory your Durable Object is allocated, regardless
  of actual usage".
- 0.125 GB × 3,600 s × $12.50 / 10⁶ = **$0.005625 per connected
  account-hour**.
- That is **$4.05 per 30-day month** (or $4.15 if 128 MB counts as 0.128
  GB, as Cloudflare's own examples do).
- The account's 400,000 GB-s cover about 1.2 such objects.
- Keep-alive alarms cost $0.15 per million requests: about $0.0065 a
  month at one a minute.

**A `lite` container always on** (read: Containers pricing, updated
2026-08-28). A `lite` container has 1/16 vCPU, 256 MiB and 2 GB of disk.

- Memory: 0.25 GiB × $0.0000025 per GiB-s.
- Disk: 2 GB × $0.00000007 per GB-s.
- CPU: "active usage only", and idle was 0% here (run).
- Total: $0.000000765/s = **$0.00275 per account-hour, $1.98 per 30-day
  month**.
- At 1% average CPU it would add about $0.03 a month.
- Its own Durable Object, woken by an alarm to keep the container
  running, costs cents (inferred).

**Today's decision 32**: the always-on computer costs about $42 a month
at list price (docs/cloudflare-v1.md, Evaluation).

### A cheaper pull or notification path? (read, inferred)

- **SimpleX's notification servers (NTF) push only to APNs** (read,
  above). No webhook or other provider exists, so nothing can push to a
  Worker.
- **SMP's GET** returns one message without subscribing (read:
  `simplex-messaging.md` §"Get message command": "used when processing
  push notifications"; it may not be mixed with SUB on one connection).
  A Durable Object could poll with an alarm:
  - connect, GET and ACK each queue, then close;
  - a few seconds of duration per poll, about $0.10–0.20 a month at one
    poll a minute;
  - but the latency is up to the interval (inferred).
  - It needs the pure-Workers client to decrypt what it gets. It could
    instead serve as a cheap waker for the container (below).

## 4. End to end, on the platform (run)

`cargo xtask e2e --only simplex`: 23 passed, 0 failed (second run; the
first, 19 checks, also passed).

The platform half is the computers section's:

- a person's computer runs an agent fragment on the stub image, whose
  scripted runtime echoes;
- the agent is in a chat;
- the guest subscribes to the chat to be woken, and the computer is put
  to sleep.

The SimpleX half is the lab: the person's client connects to the
connector's address. The connector prototype, run in the test process,
does the connector Durable Object's job:

1. It hears the person's message on SimpleX.
2. It posts the message on the chat **as the person**, with the record
   id `sx:<contact>:<itemId>`, so a retry appends nothing.
3. The record wakes the computer.
4. The connector waits for the agent's reply record, then sends it back
   on SimpleX, where the person's client receives it.

Medians in ms (second run; 5 rounds each):

| Hop | Asleep | Awake |
|---|---|---|
| Person's client → connector's client (SimpleX) | 35 | 106 |
| Connector posts the record (the platform's answer) | 6 | 11 |
| Record → the agent's reply record, seen by the connector (a wake and a turn; polled every 50 ms) | 1,858 | 314 |
| Connector → person's client (SimpleX) | 50 | 43 |
| **Total** | **1,962** | **475** |

Each round's total in the second run was within 4% of its median. In
the first run, the first wake after the first sleep took 13 s; the
cause was not investigated, and it did not happen again in the second
run. Later wakes took 2 s.

The "agent" hop is the stub's scripted turn (two drafts, then the
reply). A real agent's turn is the model's time, typically seconds.

The SimpleX lab alone, without the platform (`pair`): 49–89 ms from
person to connector, 70–128 ms back.

## The recommended shape

**A small always-on container per linked account, whose database's
home is the connector's Durable Object.**

- **The connector Durable Object** (one per linked account) holds:
  - the account's simplex-chat database, sealed at rest as every secret
    is (`seal.rs`), in its own SQLite and chunked;
  - the account's SQLCipher key (`-k`);
  - the person and the chat it speaks for, and its read cursors (the
    SimpleX item id, and the chat's record seq);
  - the instance lease, an epoch that fences out an older container's
    saves, since two live copies of one database are a guaranteed
    desync (read: the simplex-chat guide).
- **What the Durable Object does:**
  - It posts the person's messages on the chat as them. It can call the
    fragment's Durable Object directly rather than through the HTTP API.
  - It hears the agent's replies through the chat's channel trigger
    (`"from": "agent"`, as Push does in docs/chat-records.md) and passes
    them to the container to send.
  - It starts the container, keeps it alive (an alarm), restarts it, and
    meters it.
- **The container** runs the pinned `simplex-chat` with a small Rust
  supervisor:
  - At start, it fetches the database from its Durable Object, starts
    the core, and reconciles from its cursor (the `crash` run's lesson).
  - It forwards each received message and takes sends.
  - After each state change, it saves the database to the Durable
    Object with SQLite's online backup (`VACUUM INTO`, about 1.8 MB),
    so nothing is paused.
  - On SIGTERM, it stops the chat (`/_stop`), saves, and exits.
    Cloudflare allows up to 15 minutes before SIGKILL (read: Containers
    FAQ, updated 2026-10-02), so planned host restarts lose nothing.
  - On `contactRatchetSync: required`, it asks `/_sync` at once and
    posts a notice on the chat that a message may not have arrived.

Why this shape and not the others:

- **Against a pure-Workers client.** It is the only shape that meets F
  completely.
  - Durable Objects' output gates should hold an outgoing message until
    the storage write before it is durable (inferred; whether gates
    cover `connect()` writes is unknown).
  - But it is 1.5–2 months of protocol work with no library under it,
    and permanent upkeep (question 1).
  - And it costs about twice the container if it holds the socket
    ($4.05 against $1.98 a month).
  - The transport is no longer a blocker (run). This is the direction
    to take if SimpleX becomes central.
- **Against a stateless worker with Durable Object–kept state.** Nothing
  in SimpleX can wake it (NTF is APNs-only), and it cannot close the
  rollback window from outside the core (question 2). It is the same
  core as the recommended shape, with worse latency.
- **For the container.**
  - It is SimpleX's own core, so it moves with the protocol.
  - Tens of milliseconds of SimpleX latency.
  - Half a Durable Object's price.
  - A planned stop loses nothing.
  - **Its gap:** an unplanned host loss rolls back to the last save,
    which then heals by itself in about 0.2 s and loses the messages in
    flight (run). Saving after every state change keeps that window to
    the save's own duration (inferred).
- **What would close the gap without a pure-Workers client.** Give the
  core a database that is durable before the core acts on the network:
  - simplex-chat's Postgres backend (a build flag, `client_postgres`;
    prebuilt only in the npm package for linux-x86_64; read) on a
    managed Postgres.
  - That is a new vendor, so it is Paul's call (question 3 below).
    Untested.

## Costs, per connected account

| Shape | Per hour | Per 30-day month (list) |
|---|---|---|
| `lite` container, always on (recommended) | $0.00275 | **$1.98**, plus cents for its Durable Object and storage |
| Durable Object holding the SMP socket (pure Workers) | $0.005625 | $4.05 (400,000 GB-s included per account) |
| Durable Object polling with GET every minute (pure Workers) | | about $0.10–0.20, with up to a minute of latency (inferred) |
| Decision 32 today: the always-on computer | | about $42 |

At the platform's 50% margin (decision 24), the recommended shape is
about $3 per account-month.

## What's needed to build it for real

Each step is its own pull request, tested (valid, invalid, replay,
restart), leaving master whole.

1. **The connector's contract** (docs first).
   - Linking: the connector's one-time address, the first contact bound
     to the person, others refused.
   - Record ids `sx:<contact>:<itemId>`.
   - Which replies go back: replies in the linked chat; whether replies
     to web messages go too is a question for Paul.
   - The notice after a re-sync.
   - Limits: text up to 32 KiB, as the chat allows; files out.
   - An update to docs/chat-records.md and docs/computers.md (a
     connector is a poster like a page).
2. **The connector Durable Object** in the cell, its state in its
   SQLite, sealed.
   - Tests in crates/core: the lease and epoch, the cursors, the
     idempotent post, and the reply forwarding, as a pure state machine.
3. **The connector image** in `images/connector/`: simplex-chat pinned
   by checksum and the supervisor in Rust.
   - The bot API never leaves the container.
   - The supervisor's tests against a fake core.
4. **The e2e section**, from this spike's lane.
   - A message wakes a sleeping computer, and the reply comes back.
   - A replay posts once.
   - SIGTERM mid-conversation loses nothing.
   - SIGKILL with the database lost falls back to the last save,
     re-syncs, and posts the notice.
   - A second container under an old epoch cannot save.
5. **The shell.** "Link SimpleX" shows the address as a QR code, shows
   status (linked, out of sync, stopped), and offers unlink, which
   deletes the account's database and its queues.
6. **Metering** of the container's time to the owner's ledger (decision
   24), or inclusion in a seat.
7. **The hosted lane.**
   - Needs a public SMP router: the preset servers or our own, Paul's
     call (question 2).
   - Containers take no inbound TCP, so our own router would live
     elsewhere (inferred).
   - Checks the transport through a real Cloudflare egress, where
     `connect()` refuses private addresses (read).

## What it changes in decision 32

Decision 32 now reads: "SimpleX on $200 seats only. The simplex-chat
daemon runs on the always-on computer, and Hermes' SimpleX adapter runs
next to the bridge. A wake service for sleeping computers comes later."

With this design:

- **SimpleX leaves the computer.** A connector per linked account (a
  Durable Object and a `lite` container) posts to the person's chat.
  - Hermes' SimpleX adapter goes, and Hermes talks only to the relay,
    which is also the condition for Hermes' scale-to-zero (durable
    computers, F).
  - The image carries no simplex-chat.
- **The wake service is the chat record.** A sleeping computer hears a
  SimpleX message through the chat-record wake it already has (run).
  Nothing else is needed.
- **It need not be tied to the $200 seat.** At about $2 a month (list),
  any paid seat could have it. Which seats get it is Paul's call
  (question 4).
- **The always-on computer is no longer needed for SimpleX**, so
  docs/cloudflare-v1.md's "SimpleX cost" line (about $42 a month)
  becomes about $2.
- **The messenger state's one authority is the connector's Durable
  Object.** That matches docs/durable-computers.md's table.
  - It never goes back for planned stops.
  - For unplanned host loss it goes back to the last save and heals.
  - It never goes back at all only with a pure-Workers client or a
    durable database under the core.

## Questions for Paul

1. **Where encryption ends.** SimpleX's end-to-end encryption would end
   in the platform's connector (its container and Durable Object), not
   in the person's computer. The operator is the same either way, and
   the plaintext lands in the person's chat on the platform regardless.
   Do we say "end-to-end between your SimpleX app and fragment's
   connector", plainly, in the shell?
2. **Which SMP routers the connector's queues live on.** The spike used
   only a local router. The options:
   - SimpleX's preset routers (SimpleX Chat's and Flux's): no account,
     but a third party;
   - a router we run: outside Cloudflare, since Containers take no
     inbound TCP (inferred).
3. **A crash that loses the host can lose messages in flight.**
   - The run lost the one or two messages sent while out of sync. The
     person sees them as sent, never delivered, and the connector posts
     a notice.
   - Is that acceptable for v1? If not, the options are:
     - simplex-chat's Postgres backend on a managed Postgres (a new
       vendor);
     - the pure-Workers client (1.5–2 months).
4. **Who gets it, and at what price.** About $2 per account-month at
   list ($3 with the margin), against $42 on the always-on computer.
   Should $100 seats have it? Included, or metered?
5. **Who may talk through it.**
   - Only the owner (one linked contact), or an address others can use
     to reach the owner's agent?
   - Which chat does it post to: a dedicated chat per linked account,
     or the owner's existing chat with that agent?
   - Do replies to the owner's web messages also go to SimpleX?
6. **Scope for v1.** Text only (no files or voice through XFTP, no
   groups)?
7. **The pure-Workers client.** Build it later only if SimpleX usage
   justifies it, or plan it as F's end state?
