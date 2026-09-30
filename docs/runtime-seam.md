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

**Refined later the same day (Paul):** keeping a person's data from the
platform is not a design driver. On a hosted platform fragment holds
what it hosts, as it holds published fragments and its agents; a person
who connects their own computer knows the platform could see what passes
through; privacy is what self-hosting gives. So a computer is reached
**through the platform**, on the fragment's own origin, and a person's
own computer dials out to it. That supersedes 4 and 5: iroh and signed
admissions become a later optimization (a native app on the same
network), not the transport, and the measurement below waits behind
docs/two-substrates.md (fragment on Cloudflare, or entirely self-hosted),
which also names Cloudflare Containers as a rented placement.

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
