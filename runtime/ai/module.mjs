// The platform ai module — what author code gets from `import … from "fragment:ai"`.
//
// Surface follows the established SDK ergonomics (generateText /
// generateImage / generateVideo: one options object, results you
// destructure) so knowledge transfers; fragment-ness shows up as EXTRA
// fields on media results (the output is already a file in the fragment's
// working copy — path/url/sha256/size — and bytes are lazy).
//
// Text is xsai (vendored, ~KBs) pointed at the host's OpenRouter key
// through the keyed egress route; image/video are our own fal queue client
// (the official providers get the queue paths wrong for the minimax
// namespace — probed live). Defaults are host config (FRAGMENT_AI_MODEL /
// FRAGMENT_IMAGE_MODEL / FRAGMENT_VIDEO_MODEL), overridable per call;
// `model` as a string swaps the endpoint for anything on the same host,
// `providerOptions.fal` passes raw fal input through (the escape hatch
// that replaces a curated option whitelist).
import {
  generateText as xsGenerateText,
  streamText as xsStreamText,
  generateObject as xsGenerateObject,
  tool,
  XSAIError,
} from "xsai";
import { makeFal, outputUrlOf } from "./fal.mjs";

export { tool, XSAIError };

export class NoImageGeneratedError extends Error {
  constructor(message, options) { super(message, options); this.name = "NoImageGeneratedError"; }
}
export class NoVideoGeneratedError extends Error {
  constructor(message, options) { super(message, options); this.name = "NoVideoGeneratedError"; }
}

// module-scoped binding, set ONCE per worker by init(env) — which the
// loader-injected main module calls only when this module was included for
// the fragment. No globals, no per-request state: the binding is
// env-derived (per worker), never request-derived.
let binding = null;

export function init(env) {
  binding = {
    base: env.FRAGMENT_INTERNAL_URL,
    tok: env.FRAGMENT_RUN_TOKEN,
    hsec: env.FRAGMENT_HOST_SECRET || "",
    models: {
      text: env.FRAGMENT_AI_MODEL || "deepseek/deepseek-v4-flash-0731",
      image: env.FRAGMENT_IMAGE_MODEL || "fal-ai/flux-2",
      video: env.FRAGMENT_VIDEO_MODEL || "minimax/h3-max/text-to-video",
    },
    falBase: env.FRAGMENT_FAL_BASE || "https://queue.fal.run",
  };
}

const ai = () => {
  if (!binding) throw new Error('fragment:ai is available inside fragment workflows and apps');
  return binding;
};

// xsai talks OpenAI-compatible to OpenRouter THROUGH the egress route: the
// run token rides the Bearer slot, the cell swaps in the real key
const wired = (opts = {}) => {
  const g = ai();
  return {
    baseURL: `${g.base}/egress/openrouter.ai/api/v1`,
    apiKey: g.tok,
    model: g.models.text,
    ...opts,
  };
};

const toMessages = (opts = {}) =>
  opts.messages ||
  (opts.prompt !== undefined ? [{ role: "user", content: String(opts.prompt) }] : undefined);

export async function generateText(opts = {}) {
  const { prompt, ...rest } = opts;
  return xsGenerateText(wired({ ...rest, messages: toMessages(opts) }));
}

export async function streamText(opts = {}) {
  const { prompt, ...rest } = opts;
  return xsStreamText(wired({ ...rest, messages: toMessages(opts) }));
}

export async function generateObject(opts = {}) {
  const { prompt, ...rest } = opts;
  return xsGenerateObject(wired({ ...rest, messages: toMessages(opts) }));
}

// ---- image / video: fal queue via our client, Vercel-shaped ----

async function runFal({ kind, model, input, ext, errCtor, what }) {
  const g = ai();
  const fal = makeFal({ ...g, ingest: (url, path) => ingestFile(url, path) });
  const useModel = model || g.models[kind];
  let sub;
  try {
    sub = await fal.submit(useModel, input);
  } catch (e) {
    throw new errCtor(String((e && e.message) || e), { cause: e });
  }
  await fal.wait(sub);
  let out;
  try {
    out = outputUrlOf(await fal.result(sub.response_url), kind);
  } catch (e) {
    throw new errCtor(String((e && e.message) || e), { cause: e });
  }
  if (!out) throw new errCtor(`fal returned no ${what} in its result`);
  return { out, fal, ext: ext || (kind === "image" ? "jpeg" : "mp4") };
}

// shared ingest helper against the files plane
async function ingestFile(url, path) {
  const g = ai();
  const r = await fetch(`${g.base}/files/ingest`, {
    method: "POST",
    headers: { "content-type": "application/json", "x-fragment-token": g.tok, ...(g.hsec ? { "x-fragment-host-secret": g.hsec } : {}) },
    body: JSON.stringify({ url, path }),
  });
  if (!r.ok) throw new Error(`files/ingest ${r.status}: ${(await r.text()).slice(0, 200)}`);
  return (await r.json()).file;
}

export async function generateImage(opts = {}) {
  const { prompt, model, n, numImages, size, seed, dir, providerOptions } = opts;
  if (prompt === undefined) throw new NoImageGeneratedError("prompt required");
  const count = Math.min(Math.max(Number(n ?? numImages ?? 1), 1), 4);
  const raw = (providerOptions && providerOptions.fal) || {};
  // jpeg default (folder syncs stay small); a raw output_format wins
  const format = ["png", "jpeg", "webp"].includes(raw.output_format) ? raw.output_format : "jpeg";
  const input = {
    prompt: String(prompt),
    output_format: format,
    ...(["square", "square_hd", "portrait_4_3", "portrait_16_9", "landscape_4_3", "landscape_16_9"].includes(size) ? { image_size: size } : {}),
    ...(typeof size === "string" && /^\d+x\d+$/.test(size) ? (() => { const [w, h] = size.split("x").map(Number); return { image_size: { width: w, height: h } }; })() : {}),
    ...(Number.isInteger(seed) ? { seed } : {}),
    ...(count > 1 ? { num_images: count } : {}),
    ...raw,
  };
  const { out, fal } = await runFal({ kind: "image", model, input, ext: format, errCtor: NoImageGeneratedError, what: "image" });
  const images = [await fal.fileMedia(out.url, out.mime, fal.pathFor(dir, format))];
  return { image: images[0], images };
}

export async function generateVideo(opts = {}) {
  const { prompt, model, duration, aspectRatio, resolution, size, seed, dir, providerOptions } = opts;
  if (prompt === undefined) throw new NoVideoGeneratedError("prompt required");
  const raw = (providerOptions && providerOptions.fal) || {};
  const input = {
    prompt: String(prompt),
    prompt_expansion_mode: "balanced",
    ...(Number.isInteger(duration) ? { duration } : {}),
    ...(["21:9", "16:9", "4:3", "1:1", "3:4", "9:16"].includes(aspectRatio) ? { aspect_ratio: aspectRatio } : {}),
    ...(["480P", "768P"].includes(resolution) ? { resolution } : {}),
    ...(Number.isInteger(seed) ? { seed } : {}),
    ...raw,
  };
  const { out, fal } = await runFal({ kind: "video", model, input, errCtor: NoVideoGeneratedError, what: "video" });
  const video = await fal.fileMedia(out.url, out.mime, fal.pathFor(dir, "mp4"));
  return { video };
}
