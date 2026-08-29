# celld Queues vs the fragment inbox

Evaluated against celld v0.4.0 (docs/cloudflare-compat.md) after asking:
if upstream did queues well, can we shrink our system in that domain?

## What celld 0.4.0 gives you

- Cloudflare-compatible Queues JS API, running on self-hosted fleets via
  `celld deploy` (wrangler `queues` config) — not a cloud-only binding.
- One consumer script per queue (which may not export `fetch`), single
  writer per queue, two in-flight producer ops max, 4-day retention,
  no pull consumers / HTTP API.
- Delivery semantics follow Cloudflare's (at-least-once) — the same
  promise class our runtime already makes ("any trigger may fire more than
  once; key your effects").

## Verdict: the fragment inbox stays ours

The inbox is not a queue with a fragment theme; it is part of the cell's
contract, and Queues can't host that contract:

1. **Lifecycle.** Queues are deploy-time config; fragments are data. A
   queue per fragment would need dynamic queue creation/deletion riding
   fragment create/rm — the opposite of the static wrangler model.
2. **Routing.** The queue consumer would be the runtime itself, which would
   then re-implement per-fragment routing, visibility, and drain semantics
   one layer down. Net code: more, not less.
3. **What we'd lose.** Ledger integration (`inbox — inbox #N` events, run
   causality chains), Idempotency-Key dedup at the door, the 1000-cap 429
   backpressure, and the hop budget that kills watcher cycles. Those are
   the parts that made meatproxy survivable; they live in the inbox, not
   around it.

## The one honest adoption candidate

`notify_outbox` (runtime) is a hand-rolled queue: url, attempts, next_at,
a retry loop on the alarm. It has exactly one writer (the runtime) and one
consumer shape (watcher pokes), which fits celld Queues' constraints. A
swap would delete the retry loop and gain upstream-owned delivery — worth
a spike when the outbox grows fan-out or backpressure needs it. Today it
is ~50 quiet lines; replacing it now would be churn, not shrink.

## Also noted (not adopted)

- **KV**: no edge cache, 1 MiB ceiling, one writer per namespace — our
  per-cell SQLite + wstate already beats it for our shape.
- **Workflows (celld's)**: replay-based step semantics; our runs/held/
  auto-pause ledger is the product surface agents reason about. Keep.
- **Zero-restart deploys**: adopted implicitly by the 0.4.0 upgrade — the
  "restart both nodes after `celld deploy runtime`" step leaves the runbook.
