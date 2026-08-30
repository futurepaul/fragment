// GENERATED from runtime/ts — run scripts/build-runtime after editing sources.
const GEN_KINDS = {
  image: {
    model: "fal-ai/flux-2",
    // image_size presets are ~1MP; jpeg keeps folder syncs small
    input: { image_size: "square", num_images: 1, output_format: "jpeg" },
    ext: "jpeg",
    mime: "image/jpeg"
  },
  video: {
    model: "minimax/h3-max/text-to-video",
    input: { duration: 5, resolution: "768P", aspect_ratio: "16:9", prompt_expansion_mode: "balanced" },
    ext: "mp4",
    mime: "video/mp4"
  }
};
function falBase(env) {
  return String(env.FRAGMENT_FAL_BASE || "https://queue.fal.run").replace(/\/$/, "");
}
function falKey(env) {
  return String(env.FAL_API_KEY || "");
}
function requireFal(env) {
  const key = falKey(env);
  if (!key) {
    throw Object.assign(new Error("host has no FAL_API_KEY (set CELLD_VAR_FAL_API_KEY on the node, or FAL_API_KEY in .env for scripts/dev)"), { status: 501 });
  }
  return key;
}
async function falFetch(url, key, init = {}, timeoutMs) {
  const resp = await fetch(url, {
    ...init,
    headers: { authorization: `Key ${key}`, ...init.headers || {} },
    signal: AbortSignal.timeout(timeoutMs)
  });
  return resp;
}
async function falSubmit(env, model, input) {
  const key = requireFal(env);
  const resp = await falFetch(`${falBase(env)}/${model}`, key, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(input)
  }, 3e4);
  if (!resp.ok) {
    throw Object.assign(new Error(`fal submit ${resp.status}: ${(await resp.text()).slice(0, 300)}`), { status: resp.status >= 500 ? 502 : 400 });
  }
  const data = await resp.json().catch(() => null);
  if (!data || typeof data.request_id !== "string") {
    throw Object.assign(new Error("fal submit: no request_id in response"), { status: 502 });
  }
  return data;
}
function falUrlOk(env, url) {
  if (typeof url !== "string" || url.length > 500) return false;
  const base = falBase(env);
  if (!url.startsWith(base + "/")) return false;
  return url.includes("/requests/");
}
async function falStatus(env, statusUrl) {
  const key = requireFal(env);
  const resp = await falFetch(statusUrl, key, {}, 2e4);
  if (!resp.ok) {
    throw Object.assign(new Error(`fal status ${resp.status}: ${(await resp.text()).slice(0, 300)}`), { status: resp.status >= 500 ? 502 : 400 });
  }
  const data = await resp.json().catch(() => null);
  return {
    status: String(data?.status || "UNKNOWN"),
    queuePosition: Number.isFinite(data?.queue_position) ? data.queue_position : null
  };
}
async function falResult(env, responseUrl) {
  const key = requireFal(env);
  const resp = await falFetch(responseUrl, key, {}, 3e4);
  if (!resp.ok) {
    throw Object.assign(new Error(`fal result ${resp.status}: ${(await resp.text()).slice(0, 300)}`), { status: 502 });
  }
  return await resp.json();
}
function genOutputUrl(result, kind) {
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
export {
  GEN_KINDS,
  falBase,
  falKey,
  falResult,
  falStatus,
  falSubmit,
  falUrlOk,
  genOutputUrl
};
