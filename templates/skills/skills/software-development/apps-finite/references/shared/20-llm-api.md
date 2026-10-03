# AI And Outside APIs In A Fragment App

A fragment app calls models and outside APIs from its jobs, never from
the browser: keys and costs stay on the platform's side.

## AI Steps

```js
async summarize({ text }, job) {
  const out = await job.ai.text({ model: "cheap", prompt: `Summarize:\n${text}`, max_tokens: 400 });
  return { summary: out.text };
}

async draw({ prompt }, job) {
  // a JPEG (FLUX.1 [schnell] on Workers AI) written to `path` on main
  await job.ai.image({ prompt, path: "images/cover.jpg", steps: 4 });
  return { path: "images/cover.jpg" };
}
```

- Text runs on a tier: `cheap` (the default) or `medium`. GLM can spend a
  small `max_tokens` thinking: `reasoning_effort` is `low` unless you ask
  for `high`.
- An image is a JPEG written to a path ending in `.jpg` or `.jpeg`, served
  from the fragment like any file. There is no image editing, no masks and
  no reference images: text to image only.
- Video steps are off until they run on Cloudflare.
- Every step bills the fragment's owner, reserved before it runs; one the
  ledger cannot cover is held, saying why. Each fragment has a monthly cap
  ($5 unless set: `fragment cap garden 10`).

## Outside APIs

`job.fetch(url, init)` is an app's only way out. A key goes in a header as
`{{NAME}}`, filled from the fragment's secrets outside your code:

```js
const r = await job.fetch("https://api.example.com/v1/search", {
  method: "POST",
  headers: { "authorization": "Bearer {{EXAMPLE_KEY}}", "content-type": "application/json" },
  body: JSON.stringify({ q }),
});
```

```sh
fragment secret set garden EXAMPLE_KEY      # the human gives the value; it never comes back
```

The placeholders in your computer's environment (`PERPLEXITY_API_KEY=fck_…`,
which its swap fills) are for your own calls from your computer, and work
nowhere else: an app's key is the fragment's secret, which its owner
provides.

## Preferred Architecture

- The page collects input and renders results.
- A job calls the model or the API and records what it got.
- The page follows a channel, or re-runs a query, to show it.

## Publish Guardrail

If an app exposes model-backed features to people other than its owner:

- explain that its owner pays for every call, and that the cap stops it;
- prefer `members` visibility, or operations with `role: "viewer"` or above;
- require an explicit yes before `public`.
