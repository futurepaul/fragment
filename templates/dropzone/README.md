# fragment dropzone

A vault with a live ingest pipeline: drop files into `drop/`, the `ingest`
workflow fires (trigger: `sync`), and results land in `output/` — visible on
the webview and synced back to this folder within seconds.

```
fragment new mydrop --template dropzone
cd mydrop
fragment create mydrop
fragment manifest-set mydrop fragment.json
fragment publish mydrop --dir . --bless
fragment sync mydrop --dir . --watch 2     # leave running
echo "hello" > drop/note.txt               # watch output/ appear
```

The viewer is the vault template's (see its README). `workflows/ingest.mjs`
is the whole pipeline: arrivals in `drop/` → summary (ctx.ai if the host has
an inference key, else a word/heading digest) → `output/*.md`.
