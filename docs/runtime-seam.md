# One computer, wherever it runs

A computer in fragment (and later in Finite) should feel the same to a
developer and a person wherever it runs: on fragment's own boxes, on a
rented cloud, on an org's machines, or on a person's spare Mac mini. Until
now fragment grew two stacks for it: the `Computer` cell on Sprites
(`"computer": {}`) and the `Hermes` cell on sandcastle (`"hermes": {}`),
each with its own shape. This is the seam that replaces them, the
decisions behind it, and the measurement that comes before any cut.

## Decisions (Paul, 2026-09-30)

1. **What runs is a preset.** A computer runs a preset (`hermes`, `goose`
   for now) or an image and a service. `"computer": {"preset": "hermes"}`
   replaces `"hermes": {}`, a hard cut: `"hermes": {}` never reached
   fragment.club.
2. **Where it runs is not in the manifest.** A computer is placed on a
   site:
   - fragment's own (our sandcastle boxes, or a rented cloud when they
     are full);
   - an org's sandcastle nodes;
   - a person's own machine running sandcastle.

   Its owner or their org chooses. The platform may choose by room.
3. **One trust floor, everywhere.**
   - Secrets never enter the guest; they are swapped in at egress.
   - A computer is an image and a durable `/data`, updated by a rebase.
   - Snapshots are out of the guest's reach.

   A provider that cannot meet the floor does not hold computers for
   untrusted people. Sprites keep secrets in the guest and update in
   place, so they fall short on the first two. The rented placement's
   candidate is microsandbox's cloud, which runs the engine sandcastle
   already runs (open questions below).
4. **The transport is iroh.** A computer is reached by its key: no
   per-computer DNS, certificate, public URL, or CORS. Finite's spike is
   the starting point (finite-mono, branch
   `codex/core-authorized-iroh-hermes` at `14446e05`: Core admits a
   browser's throwaway key, the browser's WASM peer sends HTTP over iroh
   through a relay to Hermes on loopback, and the answer never passes
   through Core).
5. **Admissions are separate from management.** An admission is a
   short-lived signed note: "browser key B may reach computer C until
   T". It is signed by the site's authority:
   - the platform, for a computer fragment manages on its own site;
   - the person's own key, for their own machine;
   - the org's key, for the org's.

   Management (grants and the computer's owner key: make, update,
   restart, repair) is the platform's everywhere. So the platform can
   manage a person's machine without being able to read what runs on it.
6. **Held** until the measurement below lands: the fragment.club deploy
   of the `Hermes` cell, the wildcard certificate for
   `*.sandcastle.fragment.club`, and the hosted proof in docs/hermes-chat.md.

## The model

Three layers, each owned by one party:

- **What it is** (the manifest): the preset or image and service, its
  size, the directory promised to persist, the credentials it needs and
  for which hosts.
- **Where it runs** (the owner's or org's choice): a site, whose
  grantors are its owner's keys. The platform's key is listed as a
  grantor on every site it manages, and the site's owner can remove it.
- **How** (a driver per engine): make or converge, look without waking,
  wake, sleep, exec and files, update with rollback, snapshot and
  restore, destroy. sandcastle is the driver on our boxes, an org's, and
  a person's machine; a rented cloud gets one of its own.

A page asks for an admission for a computer and talks to it through the
platform's shared iroh client, whatever the site.

## Wake

iroh delivers a connection to whoever holds the dialed key. So the key
lives on the host, where something is always awake, and never in the
guest:

1. The page gets an admission for its throwaway key.
2. Its WASM peer dials the computer's key through a relay (a native app
   on the same network can connect directly).
3. The sandcastle daemon holds that computer's key and accepts. It
   checks the admission locally, with no call out.
4. Awake, the stream passes through to the service. Warm (paused), the
   daemon resumes the guest first (about 8 ms). Cold, it boots it
   (about 6 s for Hermes) and holds the stream meanwhile. This is the
   router's hold-and-wake, fed by iroh streams in place of TLS
   connections.
5. The stream's bytes are the computer's activity; 30 s quiet and it
   sleeps, as now. The connection ends at the daemon, so a guest can
   sleep under an open connection and the next message wakes it.

Keeping the key on the host also keeps it from a compromised tool and
from a restored old snapshot.

On a rented cloud where we do not run the host, the endpoint must sit in
the guest. Then a sleeping computer cannot answer: the admission step
wakes it through the provider's API before the browser dials. That path
is slower (the guest's endpoint reconnects to its relay after a resume),
and its idle is fuzzier, since the host sees only encrypted traffic to
the relay (FIN-66: an idle socket kept a Sprite awake). Whether
microsandbox's cloud offers a host-side hook is an open question.

A person's own laptop that is itself asleep cannot be woken. A machine
offered as a computer is set not to sleep, and shows offline when it
does not answer.

## What stays and what goes

- **Stays:** grants and owner keys; the tiers, wake, and activity
  accounting; the credential swap; sealed backups; presets as images with
  `/data`.
- **Goes, as the main path:** per-computer public URLs, the router's
  `cors_origins`, wildcard certificates, and the `Hermes` cell's
  password and native-login exchange. Admission replaces the gate
  (Hermes runs in loopback mode behind it, as in Finite's spike).
- **Kept for a few UIs:** an HTTPS door on boxes we run, for what must
  open in a browser tab (Hermes' own dashboard, a dev server's preview).
  Not offered for a person's own machine.

## Open questions

- **microsandbox's cloud** (private beta, 2026-09-30):
  - how a sandbox is reached from outside;
  - whether it sleeps to zero, and whether a connection can wake it;
  - regions, API stability, and when it leaves beta.

  Its list prices are about half of Sprites': $0.05 per vCPU-hour and
  $0.0162 per GiB-hour, against $0.07 per CPU-hour and $0.04375 per
  GB-hour.
- **iroh in the browser:**
  - The WASM client's size.
  - Streaming and WebSocket over iroh, which Finite's spike did not do
    (GET only, 1 MiB).
  - Browsers reach a computer only through a relay, since they have no
    UDP. So relay uptime, bandwidth, and distance are ours to run.
  - Once `fragment.boats` is on the Public Suffix List, browsers cache
    per fragment, so each fragment downloads the client once.
- **Keys:** iroh keys are ed25519 and fragment's are nostr (secp256k1).
  A computer's iroh key is its address, bound to its registry identity
  by a signed record.

## The measurement

**Problem.** Before any cut to iroh, know what a browser pays for it and
whether sandcastle's wake survives the move. The measurement runs on
finite-lat-6.

**Acceptance.** Each number is recorded here, measured on lat-6 from a
Mac:

1. The WASM client's size, raw and compressed.
2. For an awake computer, reached by its key through our relay, the
   browser's time to connect and to its first answer.
3. A warm and a cold wake triggered by an incoming iroh connection,
   timed to the first byte and set against the router's (Hermes: 236 ms
   warm, 6.1 s cold).
4. A Hermes turn streamed over a WebSocket through iroh from the
   browser: first words, set against HTTPS (4.0 to 4.5 s).
5. An open iroh connection does not keep the guest awake (it pauses 30 s
   after the last message), and the next message wakes it.
6. Admissions checked before any byte reaches the guest: none, expired,
   the wrong signer, and the wrong peer are all refused.

**Constraints.**

- sandcastle stays independent of fragment's crates.
- The endpoint and admission check live in `sandcastled` behind a flag,
  beside the router, which stays the known-good path.
- A computer's iroh key lives on the host.
- The measurement uses our own `iroh-relay` on lat-6 (port 8443, under
  the existing certificate's `demo` name), not n0's public relays.
- Nothing on fragment.club changes.
- Escalate if the client is over about 5 MB compressed, or if streaming
  over iroh in WASM needs a fork.

**Phases.**

1. The relay on lat-6.
2. `sandcastled`: a key per computer, an endpoint each, admission
   verification, and each admitted stream served by the router's handler
   (so wake, hold, and activity are reused). Daemon tests run on a local
   relay.
3. The WASM client (HTTP and WebSocket over iroh streams) and a test
   page.
4. Measurements from the Mac in headless Chrome, with a native client
   beside it for comparison.
5. The results recorded here.

**Evaluation.**

- Daemon tests for valid, invalid, expired, replayed, and wrong-peer
  admissions; wake on connect; and an open connection that carries no
  messages letting the guest sleep.
- The real-engine run on lat-6: ten samples each where cheap, with their
  spread.

## The measurement, as built (2026-09-30)

What runs on finite-lat-6:

- **The relay.** Our own `iroh-relay` 1.3.0, at
  `https://demo.sandcastle.fragment.club:8443/` (the existing
  certificate's `demo` name). It has its own unit, and the firewall now
  admits 8443/tcp. Its plain HTTP and metrics are on loopback, and its
  UDP address discovery is off.
- **`sandcastled --iroh-relay`** (`sandcastle/crates/sandcastled/src/iroh.rs`):
  - An endpoint per computer, whose key is HKDF of the node's key and
    the computer's id. So the key holds across restarts, a computer made
    again gets a new one, and the guest never sees it.
  - A connection (ALPN `sandcastle/1`) opens with an admission: a nostr
    event of kind 27237 with `peer`, `computer`, `node`, and
    `expiration` tags, lasting at most 10 minutes, verified by the same
    BIP-340 code as NIP-98. It is signed by one of the node's
    `--admitter`s, or by the computer's owner when the node names none.
  - Then each stream is one HTTP/1.1 connection, served by the router's
    own path from its gate on (`proxy::pass`). So activity, wake, hold,
    forward, and upgrades are the router's.
  - A connection closes when its admission ends.
  - The view (`GET /v1/computers/{name}`) names the computer's key and
    relay.
- **`sandcastle-web`** (`sandcastle/crates/web`): the WASM client, which
  gives a page a peer, an admitted connection, `fetch`, and a WebSocket
  with its pings answered.
- **Tests:**
  - The daemon's tests: who may admit whom, key derivation, and a
    computer reached by its key over real endpoints. They cover each
    wrong admission, expiry, two streams and an upgrade on one
    connection, the key across a restart, admitters replacing the owner,
    and a warm wake over iroh with a connection held open.
  - The real-engine e2e's new `iroh` and `web` sections.

**Measured from the Mac, through the relay on lat-6** (the e2e run
`target/e2e/sandcastle-e2e-iroh.json`, 57 checks, with Hermes v0.21.5):

| | By its key (iroh) | By its URL (HTTPS) |
|---|---|---|
| First contact | endpoint bound in 3 ms; connected 179 ms; admitted 249 ms; first answer 303 ms | |
| An awake request | median 49 ms (47–64, 10) | median 222 ms (209–250, 10) |
| A warm wake | median 68 ms (68–69, 5) | 223 ms |
| A cold wake | median 6.96 s (6.4–8.2, 3) | 6.8 s |
| Hermes' login; its socket opening | 71 ms; 62 ms | |
| A Hermes turn, first words | 2.9 s | 4.4 s |

- **Relayed throughout.** Hole punching did not happen, since lat-6's
  firewall drops inbound UDP. The relay and the node share a host, so
  the relay costs little here; elsewhere it is one more hop.
- **The URL column pays a fresh TLS connection per request**, and the
  key column reuses one QUIC connection. So the awake gap is mostly the
  handshake, not the relay. A warm wake adds about 20 ms either way.
- **The refusals held over the real relay:** a stream with no admission
  closed the connection, and an admission by someone other than the
  owner was refused.
- **Sleep under an open connection.** The guest went warm with the
  connection open, and the next request on it woke it in 68 ms. Going
  warm took 58.9 s after the turn, not the 31 s expected; something
  held it busy for about half a minute after a turn, not yet
  explained.
- **The WASM client** is 3.97 MB raw, 1.43 MB gzipped, and 1.01 MB
  with brotli. `wasm-opt -Oz` saves raw bytes, not compressed ones.

**From a browser** (headless Chrome on the Mac, the test page on the
WASM client, the e2e's `web` section, `target/e2e/sandcastle-e2e-web.json`):

| | By its key, from the browser |
|---|---|
| The client compiled and started; a peer on the relay | 10 ms; 7 ms |
| Connected and admitted; the first answer | 349 ms; 61 ms more |
| An awake request | median 53 ms (49–56, 10) |
| A warm wake | median 71 ms (68–350, 5) |
| A cold wake | 6.3–7.7 s (2) |
| Hermes' login; its socket opening | 76 ms; 64 ms |
| A Hermes turn, first words | 3.8 s (the model's time, mostly) |

- **The browser pays nothing over native** past the first contact: its
  requests and wakes are within a few milliseconds of the native
  client's.
- **Its first contact is about a third of a second:** a new peer, the
  relay, the admission, and the first answer.

**What the measurement settles.** Acceptance 1 to 6 are met on lat-6:
- a page reaches a computer by its key alone;
- sandcastle's wake survives the move;
- the admission gate holds before any byte reaches the guest;
- the client's weight is about 1 MB.

**Hermes behind the admission alone** (the e2e run
`target/e2e/sandcastle-e2e-loopback.json`, 64 checks):

- **Loopback mode on a node.** Hermes' dashboard runs on the guest's
  loopback (`HERMES_DASHBOARD_HOST=127.0.0.1`, port 9120). There its
  login gate is off, and no password exists anywhere.
- **A bridge in the guest.** msb publishes only to the guest's external
  interface, and connects from its gateway's address (`172.16.0.5`),
  where loopback mode refuses a peer. So a small bridge in the guest (a
  Python relay, started by the preset's init command before the image's
  own) carries the published port 9119 to Hermes on `127.0.0.1:9120`.
- **The checks, over the same iroh connection:**
  - a read without Hermes' session token is refused (401);
  - with it, it is answered;
  - a Host that is not loopback is refused (400, Hermes' own
    DNS-rebinding guard);
  - its socket opens with `?token=`, and a chat turn runs with no login
    (first words in 5.8 s).
- **The token** is pinned in the spec for now
  (`HERMES_DASHBOARD_SESSION_TOKEN`). So the admission is the gate, and
  the token is Hermes' own second check.
- **For the product,** the bridge belongs in the preset's image, a
  derived Hermes image with an s6 service for it, as Finite builds its
  own runtime images, and not in an init command.

**What it leaves open:**
- The relay's cost off the node's own host, and direct paths for native
  apps (lat-6 drops inbound UDP).
- Why a guest took 59 s, not 31, to sleep after a turn.
- Hermes' session token read from its own `/` by the page, as Finite's
  spike does, rather than pinned in the spec.
- Where a page gets its admission: fragment's platform, or the person's
  own key.


## The cut (2026-09-30)

After the measurement, the cut to one computer resource, in steps:

1. **`KEYS` signs admissions** (merged): `nostr/sign` with kind
   `admission`, a cell's sealed key signing the event sandcastle checks,
   at most ten minutes long.
2. **The `Hermes` cell by its key:** Hermes in loopback mode behind the
   bridge, its session token made and sealed by the cell; the computer's
   key, relay, and node recorded; `__hermes/access {peer}` answers an
   owner's or editor's page with an admission for the page's key, signed
   by the computer's key (five minutes). The sandcastle fake serves its
   computers by their keys through an in-process relay.
3. **The shared client:** sandcastle-web, built by `cargo xtask build`
   (xtask/src/client.rs) and served on every host at `/__computer/` as
   celld's static assets, so its module never enters an isolate:
   - an entry, `/__computer/client.js`, revalidated on each load, names a
     build (`/__computer/<digest>/`) whose files are kept a year;
   - the module is gzipped (1.43 MB) and declared so in `_headers`,
     since celld serves an asset's bytes as they are; it is named
     `.wasm.gz` because celld takes every `*.wasm` below a project into
     its bundle;
   - the build needs an LLVM clang for wasm32 (ring's C), which xtask
     finds (the environment, Homebrew's, the PATH's, or Nix's); CI builds
     it on the macOS kit.
   - A page makes its key before it knows the relay (it names its key to
     get the admission that names the relay), so `new Peer()` makes the
     key and the endpoint binds at the first `connect`.
4. **The preset and the page:** `"computer": {"preset": "hermes"}` in
   place of `"hermes": {}` (decision 1; `start` with a preset is
   refused), and the `hermes` template's page on the client: its key, an
   admission, REST and the socket by the computer's key, the admission
   renewed on the same connection a minute before it ends.

The e2e lane `hermes` proves 2 to 4 on fakes (49 checks), the template's
page in headless Chrome included.

Still to do:

- **A derived Hermes image** with the bridge as an s6 service, in place
  of the init command.
- ~~fragment.club~~ live 2026-10-01: the node image, then the cell with
  its static assets, no wildcard certificate; the hosted proof passes
  (docs/hermes-chat.md, "Phase 5, live").
- **The `goose` preset**, and presets on a rented site.
