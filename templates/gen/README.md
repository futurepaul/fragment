# gen

A prompt box that makes images and videos. That's the whole app.

- **image** — fal FLUX.2 [dev] default (~1MP jpeg, ≈ $0.01 each)
- **video** — MiniMax H3 Max default (5s, 768p, ≈ $0.40 each at list; watch
  for fal's launch discounts)

No dials, no settings: the defaults live in the platform (`ctx.image` /
`ctx.video`), not in this app, so tuning them is a host concern
(`FRAGMENT_IMAGE_MODEL` / `FRAGMENT_VIDEO_MODEL`) rather than an app
concern.

## Run it

```
fragment init mygen --template gen    # scaffold + create + deploy
fragment open mygen                   # prints the ?view= link
```

The host needs a fal key (`CELLD_VAR_FAL_API_KEY`; `FAL_API_KEY=...` in
`.env` for `scripts/dev`). Without it the app loads and generations return
the host's "no FAL_API_KEY" error — nothing else breaks.

## Where things go

Every generation becomes a file in the fragment's working copy under
`gen/<timestamp>-<rand>.{jpeg,mp4}` — the grid on load is just that
directory. `fragment pull` materializes them into your local folder;
they sync like any other file.

## Pieces

- `site/index.html` — the page: toggle, prompt, optimistic card per
  generation, polls `status` from the tab (the sleep is browser-side; the
  fragment never blocks on a generation).
- `app.mjs` — three routes over `ctx.gen.start/status` + `ctx.files.index`.

Preview drafts (`/d/<slug>/…`) read the frozen snapshot, so files generated
after the preview was cut won't render there — generate from the live app.
