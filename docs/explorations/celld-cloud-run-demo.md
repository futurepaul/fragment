# celld-cloud-run-demo — idiomatic celld from the outside

Exploration 2, evaluated 2026-09-07 against
[taeold/celld-cloud-run-demo](https://github.com/taeold/celld-cloud-run-demo)
(celld v0.4.1 on GCP). Read as "idiomatic celld usage from someone who
understands the concepts": what fragment could learn, what the opencode
example implies for agents-as-fragments, and whether a "$20/month per user
buys their own fleet" product shape pencils out. Spoiler: it does.

## What the repo demonstrates

A single celld node on a Cloud Run Instance (~$5.70/mo, 1 vCPU/1Gi), or a
scale-out fleet: a stateless Cloud Run ingress (`CELLD_MAX_RESIDENT_CELLS=0`,
pure router) in front of a Worker Pool of celld nodes over Direct VPC, with
**GCS as the cluster plane** — fleet discovery (`nodes/node_*.json`), room
leases, and the LTX WAL all live in the bucket. Three examples: a WebSocket
counter, an OpenTelemetry-instrumented workflow, and an OpenCode agent.

## example-opencode: the gem

The whole agent is **54 lines** hosting a full coding agent in one Durable
Object via `@opencode-ai/sdk/workerd`. The idioms:

- **`OpenCodeWorkerd.create({ storage: state.storage })`** — the SDK takes
  the DO's storage directly. No adapter, no own schema; sessions and
  messages live in the object's SQLite because the SDK was built to. Proof
  that a heavy third-party stateful thing composes with a DO cleanly.
- **`permission: "deny"`** — the agent boots with zero tools. The HTTP
  surface is the entire capability boundary, auth is delegated to the
  platform (Cloud Run IAM), and the README states it: "the Worker has no
  additional authentication or shell tools." Defense by construction.
  Fragment's translation: tool grants in the manifest — agents
  denied-by-default, owner grants capabilities per fragment.
- **`idFromName("demo")`** — one named DO instance = one agent. Per-user
  agents are a naming scheme, not an architecture change.
- **Honest idempotency**: `prompt()` enqueues a turn, `wait()` blocks for
  it, and the 502 handler's message is "Read the session before retrying a
  prompt" — retry guidance baked into the error surface. Very
  event-log-is-truth.
- **Free model** (`opencode/nemotron-3.5-lightning-free`): a deployable
  agent with zero key management; fragment secrets take over for real
  models.
- The smoke test is a durable-memory contract test (random marker, prompt,
  resume fresh, assert recall) — the self-test an agent template wants.

**The concrete steal: an `agent` recipe.** OpenCode host inside a fragment
cell; prompts arrive via the inbox (`trigger: inbox` → `sessions.prompt`);
turns stream to a room so the webview is a live chat; session history in
the cell's SQLite; model key as a fragment secret; tools denied by default
with manifest grants. Every ingredient exists in fragment today — this
example is the assembly instructions.

## The cloud topology: what idiomatic celld teaches

- **The bucket is the control plane.** `celld deploy <dir> --bucket gs://…`
  from a laptop; GCS holds code, state, discovery, leases, WAL. No API
  server, no registry service. The demo's Python dashboard reads workflow
  history directly from LTX records in the bucket, no runtime involved —
  observable state without waking cells. A fragment status page could do
  the same from our bucket.
- **Fleet scale-out exists and is boring.** Nodes advertise a private VPC
  IP into the bucket; the ingress reads live node records and routes rooms
  across workers; scaling is `worker-pools update --instances=5`.
  Fragment's prod is single-node + Caddy; the ceiling is a gcloud command,
  not a rewrite.
- **Measured numbers** (us-west1): 13 durable writes/sec, p50 90ms /
  p95 119ms / p99 377ms including the GCS WAL sync, 100% success, one GCS
  object per durable write. Memory density: ~30.5MB base RSS, ~1.43MB per
  resident cell, 8 FDs per cell.
- Smaller idioms worth copying: `/.well-known/celld/health` startup probe
  (v0.4.0+ path), `restart-policy=always` (instances exit; celld tolerates
  it), the counter's canonical-URL 308s + reserved-room-ID denylist +
  hibernating WebSockets + exponential-backoff reconnect client, and the
  workflow's OpenTelemetry spans emitted from inside `step.do` to a
  loopback collector — with spanIds derived deterministically from traceId
  so step replays don't duplicate spans.

## The $20/month fleet — unit economics

Cloud Run Instance at 1 vCPU/1Gi runs ~$5.70/mo continuous. So "each user
gets their own fleet" is a price list, not a metaphor:

| Tier | Shape | COGS/user | Isolation |
|---|---|---|---|
| Dedicated ($20/mo) | own celld node + own bucket | ~$7–9 (node + light GCS) | physical — separate node, bucket, IAM |
| Shared | users packed per node, bucket prefixes | ~$1–2 | logical again |
| Dedicated+ | own Worker Pool mini-fleet | from ~$32.79 | physical, with headroom |

At ~1.43MB per resident cell, a 1Gi node holds hundreds of awake fragments;
asleep cells cost only bucket pennies. The node is the cost floor, and
isolation aligns with billing: a runaway agent burns its own user's
CPU/ops/egress and nobody else's.

**Cross-fleet comms is already solved by the inbox.** Inboxes are
authenticated HTTP POSTs with tokens — they cross node, host, and cloud
boundaries for free. Any fragment in fleet A can reach the user's opencode
fragment in fleet B via `POST /inbox?t=…`. What's missing for a product is
only discovery (fragment name → current host URL; fragment.club's registry
role or DNS) and per-fleet key hygiene. Rooms/WebSockets do not cross
fleets — async-only between users — which is the right constraint per the
queues-eval: at-least-once inbox semantics are exactly what made meatproxy
survivable.

**The agent is the dedicated tier's killer app**: every user gets their own
mini-opencode with durable sessions in their own bucket, tools denied by
default, granted per fragment. "Your agent's whole memory is physically
yours" is a differentiated pitch against shared-tenant everything.

## Caveats before betting on it

- Cloud Run **Instances and Worker Pools are Preview/Pre-GA**, limited
  regions, pricing distinct from regular Cloud Run services.
- p50 90ms durable writes are the object-storage-WAL tax — fine for
  fragments, wrong for chatty UI loops.
- The workflow demo's run registry is in-memory and the repo says so
  honestly ("not a durable catalog").
- The demo's IAM/IAP auth is a developer story, not a product story —
  nostr keys + view tokens remain the right user-facing layer, with
  per-user fleets underneath.

## Verdict

Three adoptables, in order: the **agent recipe** (opencode-in-a-fragment —
highest user value, all parts exist), the **bucket-as-control-plane
observability** (read state without waking cells), and the **per-user fleet
economics** (dedicated nodes at ~$7–9 COGS under a $20 price, physical
isolation as a selling point, inboxes as the inter-fleet fabric). The
Preview/GA status of the GCP primitives is the main thing to re-check
before committing to the fleet shape.
