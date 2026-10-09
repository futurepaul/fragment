---
name: image-generation-finite
description: Generate images from a text prompt with Cloudflare's own image model (FLUX.1 [schnell] on Workers AI) through a fragment's AI step, billed to your owner. Use when the human asks you to draw, generate, or illustrate an image. Replaces fal-image-editing on fragment; editing an existing image is not available here.
triggers:
  - generate an image
  - draw a picture
  - make an illustration
---

# Image generation

Images come from the platform's image model, FLUX.1 [schnell] on Cloudflare
Workers AI, through a fragment's AI step (`job.ai.image`): a JPEG written to
a path in the fragment, billed to your owner. Hermes' own `image_generate`
tool has no provider configured on this computer: do not use it, and never
call an image provider directly or look for its keys.

## What is missing

The platform's image step is text to image only. It has no edits of an
existing image, no reference images, no masks, inpainting or
outpainting, no model choice and no aspect ratio. When the human asks for
an edit, say so plainly, and offer one of:

- a new image from a prompt that describes the edited result;
- an edit you can make with code (crop, resize, composite, text overlay
  with Pillow).

## Set up once

Each image is a run of the `draw` job in an images fragment of your
owner's. Make it the first time (it is reused after):

```sh
fragment list --json                                  # an images fragment already? reuse it
mkdir -p ~/apps/images && cp -R ${HERMES_SKILL_DIR}/images-app/. ~/apps/images/
fragment create images                                  # prints images--<suffix>; the bare label resolves it
fragment deploy images --dir ~/apps/images
```

## Generate

```sh
fragment call images draw --input '{"prompt": "a watercolor of a tomato plant on a sunny balcony, soft morning light", "path": "images/2026-10-03-balcony-tomato.jpg"}'
# → {"run": 7, "status": "queued"}
fragment runs images 7                                # wait until it says succeeded (a few seconds)
mkdir -p ~/images && fragment sync images --dir ~/images --mode pull
```

The JPEG is then `~/images/images/2026-10-03-balcony-tomato.jpg`: attach it
to your reply with `MEDIA:<its absolute path>`, or link the images
fragment's page (`fragment open images`), which shows every image.

- `path` is `images/<name>.jpg`: name each by date and subject, never
  reusing one (a new image at an old path replaces it).
- `steps` is 1 to 8 (4 by default): more is slower and a little finer.
- A run that fails is held, saying why (`fragment runs images <run>`): a
  refused prompt, or your owner's credit (`budget_used_up`). Replay it with
  `fragment replay images <run>` once fixed.

## Prompting guidance

- Describe the subject, the medium (photo, watercolor, flat illustration),
  the composition, the light and the mood, in one or two sentences.
- Text inside the image is unreliable at this model's size: add lettering
  afterwards with code when it matters, and never claim exact typography.
- Look at the result before you send it, and make one focused retry with a
  sharper prompt rather than many broad ones.
