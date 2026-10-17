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

async categorize({ item }, job) {
  const out = await job.ai.decide({
    input: item,
    questions: {
      aisle: {
        type: "choice",
        instructions: "Choose the aisle for the finished product.",
        options: {
          snacks: "Chips, crackers and popcorn",
          produce: "Fresh fruit and vegetables",
          grocery: "Shelf-stable foods, including nut butter",
          frozen: "Frozen foods, including ice cream",
          other: "Other items",
        },
      },
    },
  });
  return { aisle: out.answers.aisle.choice };
}

// a photo or a voice note someone uploaded from the page:
// const { sha256, type } = await fragment.blob(file); fragment.call("sort", { sha256, type })
async sort({ sha256, type }, job) {
  const out = await job.ai.decide({
    model: type.startsWith("image/") ? "clef-flash" : "clef-omni", // audio and video: clef-omni
    input: "Something left for the household",
    media: [{ sha256, type }],
    questions: {
      kind: { type: "choice", instructions: "What is this about?", options: {
        groceries: "Food or supplies to buy", chores: "A job around the house", other: "Anything else" } },
      urgent: { type: "predicate", instructions: "Does it need doing today?" },
    },
  });
  return { kind: out.answers.kind.choice, urgent: out.answers.urgent.probability > 0.7 };
}
```

- Text runs on a tier: `cheap` (the default) or `medium`. GLM can spend a
  small `max_tokens` thinking: `reasoning_effort` is `low` unless you ask
  for `high`.
- Use `ai.decide` when you need a category, route or gate, about text,
  JSON, photos, voice notes or video. It returns typed answers, never
  prose to parse: `choice` with `options: {id: description}`, `predicate`
  with `.probability` of true, or `score` with ordered `levels:
  [description, ...]` and a weighted zero-based `.score`. Choice and
  score return full `probabilities` and the model's `confidence`.
  - `model`: `clef-flash` (the default, cheapest: text and images),
    `clef` (larger), `clef-omni` (also audio and video).
  - `media`: up to 4 items. A file uploaded with `fragment.blob(file)`
    is `{sha256, type}` as it returns (a chat's attachments pass as they
    are); a small image may go inline as `{data: "data:image/png;base64,…"}`.
    PNG, JPEG, WebP; WAV, MP3, MP4, WebM on `clef-omni` only (a
    browser's `audio/webm` recording is refused: convert to WAV). 4 MiB an
    image, 8 MiB an audio or video clip, 8 MiB in all.
  - 1–64 questions, 2–255 choices or 2–10 levels; nonempty instructions
    and descriptions; input and questions at most 16 KiB on clef-flash,
    48 KiB on the others. A wrong model, type, size or shape fails the
    step before anything is spent, saying why.
  - Billed for input tokens only (media counts as input): Clef-flash
    $0.038, Clef $0.24, Clef-omni $0.15 per million, plus the fee and
    margin.
  - Typed answers can still be wrong. Prefer saved exact-item
    corrections before inference; keep a correction UI and tune any
    review threshold against labeled examples rather than trusting
    confidence alone.
- From your own computer (a tool or a script you build, or a decision of
  your own), the same contract is `POST $FRAGMENT_MODEL/v1/decide` with
  your usual model credentials (`x-fragment-agent: $FRAGMENT_AS_AGENT`, or
  `Authorization: Bearer agent:<you>`), its media inline as `{data}`
  (a 6 MiB request at most), billed to your owner:

  ```sh
  curl -s "$FRAGMENT_MODEL/v1/decide" -H "x-fragment-agent: $FRAGMENT_AS_AGENT" \
    -H 'content-type: application/json' \
    -d '{"input": "Invoice 42 is 30 days overdue", "questions": {"urgent": {"type": "predicate", "instructions": "Does this need a reply today?"}}}'
  # {"model":"@cf/cloudflare/clef-flash","answers":{"urgent":{"type":"predicate","probability":0.91}},"usage":{…}}
  ```
- An image is a JPEG written to a path ending in `.jpg` or `.jpeg`, served
  from the fragment like any file. There is no image editing, no masks and
  no reference images: text to image only.
- Video steps are off until they run on Cloudflare.
- `job.ai.text` takes OpenAI's `messages`, `tools` and `tool_choice`, and
  answers `{text, message, finish_reason, timing, …}`: a job runs its own
  tool loop (on `finish_reason: "tool_calls"`, push `message`, run each of
  `message.tool_calls`, push `{role: "tool", tool_call_id, content}`, call
  again; bound the loop, each call is a step). With `draft: {channel,
  turn}` the reply streams into that channel's draft, and the record you
  publish with the same `turn` replaces it. `fragment guide`, AI, has a
  worked example.
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
EXAMPLE_KEY=… fragment secret set garden EXAMPLE_KEY --to api.example.com   # the human gives the value; it never comes back
```

A secret is filled only toward the hosts it names, over https: name each
host the key is for (`--to`, once each), and a fetch anywhere else fails.

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
