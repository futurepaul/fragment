// notify-relay — the one consumer of the fragment-notify queue.
//
// Why a separate script: celld Queues constraint — "a queue can have one
// consumer script; the consumer cannot also export a fetch() handler."
// The runtime is the fetch-bearing router + cells + workflows, so the
// relay is its own tiny deployment (same fleet, same bucket):
//
//   celld deploy notify-relay --bucket <same bucket as the runtime>
//
// It does exactly one thing: for each delivered message, POST the notify
// frame to its URL with the cross-fragment hop headers. Delivery is
// at-least-once (queue semantics) — the same promise class the old
// in-cell outbox made; receivers key effects by cause (the once pattern)
// and the cycle guard absorbs loops. A delivery that fails 5xx-ish is
// retried by the queue with backoff; a permanent 4xx acks and moves on.
//
// No fetch handler on purpose (see constraint above). No config, no
// state, no secrets: the URLs and frames arrive in the messages.
const DELIVERY_TIMEOUT_MS = 10_000;
const MAX_HOP_FORWARD = 1; // notify frames always enter the receiver at hop 1

export default {
  async queue(batch) {
    for (const msg of batch.messages) {
      const { url, source, frame } = msg.body || {};
      if (typeof url !== "string" || !/^https?:\/\//.test(url) || !frame) {
        console.log("notify-relay: dropping malformed message", JSON.stringify(msg.body).slice(0, 200));
        msg.ack();
        continue;
      }
      try {
        const resp = await fetch(url, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            // cross-fragment courtesy: carry the hop budget and origin so
            // the receiver's inbox cycle guard applies to notify loops
            "x-fragment-hops": String(MAX_HOP_FORWARD),
            "x-fragment-cause": String(source || "notify"),
          },
          // envelope like a hand-posted drop: the inbox route stores
          // body.payload, so the receiver's workflows see paths/sha
          body: JSON.stringify({ source: String(source || "notify"), payload: frame }),
          signal: AbortSignal.timeout(DELIVERY_TIMEOUT_MS),
        });
        // any 2xx acks; 4xx means the receiver permanently refused this
        // frame (bad token, gone) — retrying cannot help, ack and move on
        if (resp.ok || (resp.status >= 400 && resp.status < 500)) {
          msg.ack();
        } else {
          msg.retry(); // 5xx/network-ish: let the queue retry with backoff
        }
      } catch {
        msg.retry();
      }
    }
  },
};
