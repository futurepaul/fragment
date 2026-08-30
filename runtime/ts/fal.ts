// fal.ai queue client for the generation tools (ctx.image / ctx.video /
// ctx.gen.*). The FAL_API_KEY lives on the host (CELLD_VAR_FAL_API_KEY →
// env.FAL_API_KEY, same ride as OPENROUTER_API_KEY) and NEVER crosses the
// loopback into author isolates: author code submits through the cell, the
// cell holds the key.
//
// Wire: fal's queue API (https://fal.ai/docs, "Asynchronous Inference"):
//   POST {base}/{model}                        → {request_id, status_url, response_url}
//   GET  {base}/{model}/requests/{id}/status   → {status: IN_QUEUE|IN_PROGRESS|COMPLETED, queue_position?}
//   GET  {base}/{model}/requests/{id}          → the model output (4xx JSON on failure)
// The SLEEPING between polls deliberately lives in the caller's isolate
// (ctx.gen.wait), never here: a cell-side wait would hold the DO's single
// thread hostage for the whole generation and stall the fragment's own
// webview. The cell only ever does short request-scoped fal calls.

// Per-kind defaults, chosen for cheap-and-fast (the tools' contract is
// "sane defaults, no dials"): images bill by the megapixel so FLUX.2 [dev]
// at 1MP ≈ $0.012; MiniMax H3 Max is fal's speed-tuned H3 (5s clip in
// seconds of wall time, $0.08/s at 768P) — "balanced" prompt expansion
// adds ~1s instead of up to 30s for "quality".
export const GEN_KINDS = {
  image: {
    model: "fal-ai/flux-2",
    // image_size presets are ~1MP; jpeg keeps folder syncs small
    input: { image_size: "square", num_images: 1, output_format: "jpeg" },
    ext: "jpeg",
    mime: "image/jpeg",
  },
  video: {
    model: "minimax/h3-max/text-to-video",
    input: { duration: 5, resolution: "768P", aspect_ratio: "16:9", prompt_expansion_mode: "balanced" },
    ext: "mp4",
    mime: "video/mp4",
  },
};

export function falBase(env) {
  // overridable for tests and fal-compatible proxies; the default is fal's
  // public queue host
  return String(env.FRAGMENT_FAL_BASE || "https://queue.fal.run").replace(/\/$/, "");
}

export function falKey(env) {
  return String(env.FAL_API_KEY || "");
}

function requireFal(env) {
  const key = falKey(env);
  if (!key) {
    throw Object.assign(new Error("host has no FAL_API_KEY (set CELLD_VAR_FAL_API_KEY on the node, or FAL_API_KEY in .env for scripts/dev)"), { status: 501 });
  }
  return key;
}

async function falFetch(url: string, key: string, init: any = {}, timeoutMs: number) {
  const resp = await fetch(url, {
    ...init,
    headers: { authorization: `Key ${key}`, ...(init.headers || {}) },
    signal: AbortSignal.timeout(timeoutMs),
  });
  return resp;
}

// Submit a generation to the queue. Returns fal's own submit body — the
// request_id AND the authoritative status/response URLs. Those URLs are the
// ONLY safe way to address the job later: their path shape is per-namespace
// (the minimax app drops the endpoint suffix that the submit path carries),
// so constructing them from the model id 405s on some models.
// A 4xx here is a caller-facing verdict (bad model id, bad input, exhausted
// credits) — surfaced with fal's detail text.
export async function falSubmit(env, model, input) {
  const key = requireFal(env);
  const resp = await falFetch(`${falBase(env)}/${model}`, key, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input),
  }, 30_000);
  if (!resp.ok) {
    throw Object.assign(new Error(`fal submit ${resp.status}: ${(await resp.text()).slice(0, 300)}`), { status: resp.status >= 500 ? 502 : 400 });
  }
  const data = await resp.json().catch(() => null);
  if (!data || typeof data.request_id !== "string") {
    throw Object.assign(new Error("fal submit: no request_id in response"), { status: 502 });
  }
  return data;
}

// A job URL is trustworthy only inside the host's own fal base: gen jobs
// round-trip through browsers, and the cell attaches the FAL key to these
// fetches — a URL pointing elsewhere would hand the key to that server.
// (Constructed fallback paths always pass; they're built from the base.)
export function falUrlOk(env, url) {
  if (typeof url !== "string" || url.length > 500) return false;
  const base = falBase(env);
  if (!url.startsWith(base + "/")) return false;
  return url.includes("/requests/");
}

// One poll. Never sleeps — pacing belongs to the caller's isolate.
export async function falStatus(env, statusUrl) {
  const key = requireFal(env);
  const resp = await falFetch(statusUrl, key, {}, 20_000);
  if (!resp.ok) {
    throw Object.assign(new Error(`fal status ${resp.status}: ${(await resp.text()).slice(0, 300)}`), { status: resp.status >= 500 ? 502 : 400 });
  }
  const data = await resp.json().catch(() => null);
  return {
    status: String(data?.status || "UNKNOWN"),
    queuePosition: Number.isFinite(data?.queue_position) ? data.queue_position : null,
  };
}

// Fetch the completed output. fal signals generation failures as 4xx here
// (queue status stays a 3-state lamp), so the error text is the model's own
// verdict and travels to the caller verbatim.
export async function falResult(env, responseUrl) {
  const key = requireFal(env);
  const resp = await falFetch(responseUrl, key, {}, 30_000);
  if (!resp.ok) {
    throw Object.assign(new Error(`fal result ${resp.status}: ${(await resp.text()).slice(0, 300)}`), { status: 502 });
  }
  return await resp.json();
}

// Pull the primary output file out of a model result. Shapes are convergent
// across fal's media endpoints (images[] / video), but stay defensive: a
// model that moves its output field should fail with a readable shape
// description, not an undefined deref.
export function genOutputUrl(result, kind) {
  if (!result || typeof result !== "object") return null;
  if (kind === "image") {
    const img = Array.isArray(result.images) ? result.images.find((i) => i && typeof i.url === "string") : null;
    if (img) return { url: img.url, mime: String(img.content_type || "") };
    // some image models return a bare image object
    if (result.image && typeof result.image.url === "string") return { url: result.image.url, mime: String(result.image.content_type || "") };
    return null;
  }
  if (result.video && typeof result.video.url === "string") {
    return { url: result.video.url, mime: String(result.video.content_type || "") };
  }
  return null;
}
