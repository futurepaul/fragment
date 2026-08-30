// fal queue client, client-side edition. Relocated from the cell (where it
// lived as runtime/ts/fal.ts) into the ai module: all vendor knowledge is
// author-side now, riding the keyed egress route.
//
// Wire (https://fal.ai/docs "Asynchronous Inference"):
//   POST {base}/{model}                        → {request_id, status_url, response_url}
//   GET  {base}/{model}/requests/{id}/status   → {status: IN_QUEUE|IN_PROGRESS|COMPLETED}
//   GET  {base}/{model}/requests/{id}          → the model output (4xx JSON on failure)
//
// TWO hard-won invariants, kept from the first build:
// 1. Carry fal's OWN status_url/response_url from the submit response —
//    their path shape is per-namespace (the minimax app drops the endpoint
//    suffix the submit path carries), so constructing them 405s. This is
//    also where the official @ai-sdk/fal provider breaks on h3-max.
// 2. Sleeping between polls happens HERE (author isolate), never in the
//    cell — a waiting cell stalls the fragment's own webview.

const randHex = (n) => [...crypto.getRandomValues(new Uint8Array(n))].map((b) => b.toString(16).padStart(2, "0")).join("");

export function makeFal(ai) {
  // egress wraps every keyed call: <internal>/egress/<fal-host>/<path>,
  // authorized by the run token in the Bearer slot (the cell swaps in the
  // real fal key)
  const host = new URL(ai.falBase).host;
  const call = (path, init = {}) =>
    fetch(`${ai.base}/egress/${host}/${path}`, {
      ...init,
      headers: {
        "content-type": "application/json",
        authorization: `Bearer ${ai.tok}`,
        ...(ai.hsec ? { "x-fragment-host-secret": ai.hsec } : {}),
      },
    });

  async function submit(model, input) {
    const resp = await call(model, { method: "POST", body: JSON.stringify(input) });
    if (!resp.ok) throw new Error(`fal submit ${resp.status}: ${(await resp.text()).slice(0, 300)}`);
    const data = await resp.json().catch(() => null);
    if (!data || typeof data.request_id !== "string") throw new Error("fal submit: no request_id in response");
    return data;
  }

  async function status(statusUrl) {
    const path = statusUrl.replace(/^https?:\/\/[^/]+\//, "");
    const resp = await call(path);
    if (!resp.ok) throw new Error(`fal status ${resp.status}: ${(await resp.text()).slice(0, 300)}`);
    const data = await resp.json().catch(() => null);
    return {
      status: String(data?.status || "UNKNOWN"),
      queuePosition: Number.isFinite(data?.queue_position) ? data.queue_position : null,
    };
  }

  async function result(responseUrl) {
    const path = responseUrl.replace(/^https?:\/\/[^/]+\//, "");
    const resp = await call(path);
    if (!resp.ok) throw new Error(`fal result ${resp.status}: ${(await resp.text()).slice(0, 300)}`);
    return await resp.json();
  }

  // sleep-and-poll until the lamp goes green; timeouts throw
  async function wait(sub, { pollMs = 1500, timeoutMs = 300_000 } = {}) {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      const st = await status(sub.status_url || `${sub.response_url}/status`);
      if (st.status === "COMPLETED") return;
      if (Date.now() > deadline) throw new Error(`fal: timed out after ${Math.round(timeoutMs / 1000)}s (still ${st.status})`);
      await new Promise((r) => setTimeout(r, pollMs));
    }
  }

  // plan the output path before submitting so results are deterministic
  const stamp = () => new Date().toISOString().replace(/[-:T]/g, "").slice(0, 14);
  const pathFor = (dir, ext) => `${dir || "gen"}/${stamp()}-${randHex(4)}.${ext}`;

  // place an output URL as a fragment file via the ingest primitive, and
  // wrap it with lazy byte access — media stays URL-shaped until someone
  // actually asks for bytes (the tier streams; the isolate need not buffer)
  async function fileMedia(url, mediaType, path) {
    const placed = await ai.ingest(url, path);
    const self = {
      mediaType: placed.mime || mediaType,
      path: placed.path,
      url: placed.url,
      sha256: placed.sha256,
      size: placed.size,
      async bytes() {
        const r = await fetch(`${ai.base}/files/read?path=${encodeURIComponent(placed.path)}`, {
          headers: { "x-fragment-token": ai.tok, ...(ai.hsec ? { "x-fragment-host-secret": ai.hsec } : {}) },
        });
        if (!r.ok) throw new Error(`reading ${placed.path}: ${r.status}`);
        return new Uint8Array(await r.arrayBuffer());
      },
      async base64() {
        const b = await self.bytes();
        let s = "";
        for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
        return btoa(s);
      },
    };
    return self;
  }

  return { submit, status, result, wait, pathFor, fileMedia };
}

// Pull the primary output URL out of a fal result. Shapes are convergent
// across media endpoints (images[] / video) — stay defensive so a model
// that moves its output field fails with a readable description.
export function outputUrlOf(result, kind) {
  if (!result || typeof result !== "object") return null;
  if (kind === "image") {
    const img = Array.isArray(result.images) ? result.images.find((i) => i && typeof i.url === "string") : null;
    if (img) return { url: img.url, mime: String(img.content_type || "") };
    if (result.image && typeof result.image.url === "string") return { url: result.image.url, mime: String(result.image.content_type || "") };
    return null;
  }
  if (result.video && typeof result.video.url === "string") {
    return { url: result.video.url, mime: String(result.video.content_type || "") };
  }
  return null;
}
