# doc

A page of writing that its readers leave notes on, paragraph by
paragraph, the way a shared document takes comments: in the margin on a
wide window, under the paragraph on a narrow one, live for everyone who
has it open. For explainers, specs, drafts: anything you want questions
on.

- `site/index.html` is the page. Each block with a `data-a` id takes
  notes; keep a block's id when you edit it (a note follows its id; a
  note whose id is gone is listed at the end of the page).
- `site/notes.js` and `site/notes.css` are the notes on the page: the
  margin, the note button (it quotes the words selected), threads,
  replies, sign-in for anonymous readers. `site/doc.css` is the reading
  column and its figures.
- `applib/notes.mjs` is the notes in the app: one table and four
  operations, a mixin `app.mjs` takes whole
  (`class App extends withNotes(DurableObject) {}`).
- `fragment.json` declares the operations, each described (so they show
  in `fragment status` and are tools of `fragment mcp`), and the `notes`
  channel.

## The rules

| Operation | Kind | Who | Answers |
|---|---|---|---|
| `notes` | query | whoever may open the page | `{notes: [{id, anchor, quote, text, author, agent, role, reply_to, at, resolved}]}`, oldest first |
| `annotate` | mutation | signed-in people and agents (anonymous: refused) | `{id, thread}`: a note on `anchor` (and its `quote`), or a reply with `reply_to` |
| `resolve` | mutation | the thread's author, or the page's owner | `{id, resolved}`; a reply to a resolved thread reopens it |
| `remove_note` | mutation | the note's author, or the page's owner | `{removed}`; a thread's first note takes its replies; for good |

The rules live in the app, so they hold for a page, the CLI and an MCP
client alike. The operations' role is `public`: the page's visibility
decides who reads the notes (`fragment visibility <doc> public` for
everyone), and the app decides who writes them. A retry with the same id
is a replay (the platform's ledger), never a second note, and an id is
never reused after a delete. Every change is also a record on the
`notes` channel (kinds `annotated`, `resolved`, `reopened`, `removed`)
for whoever wants the stream: `fragment channel <doc> notes --follow`.

Limits: a page holds 1000 notes and 640 KiB of their text and quotes (so
`notes` always answers in one call, under its 1 MiB); a note is up to
2000 characters, a quote 280, an anchor 64 of `A-Z a-z 0-9 _ -`.

## Cursors

Off by default: the page sends no presence. `<html data-cursors>` turns
them on: each reader's pointer, anchored to the paragraph under it, and
who is here in the corner.

## An agent's notes

```sh
fragment call <doc> notes
fragment call <doc> annotate --input '{"anchor": "lede", "quote": "the way a shared document", "text": "Which document?"}'
fragment call <doc> annotate --input '{"reply_to": 1, "text": "Any you share: a spec, a draft."}'
fragment call <doc> resolve --input '{"id": 1, "resolved": true}'
fragment call <doc> remove_note --input '{"id": 2}'
```

Or as tools: `claude mcp add <doc> -- fragment mcp <doc> --write`. The
paragraphs' ids are the page's: `fragment sync <doc> --dir <dir> --mode
pull` and read `site/index.html`.

## Notes on a fragment you already have

1. Copy `applib/notes.mjs` into your fragment's `applib/`, and
   `site/notes.js` and `site/notes.css` into its `site/`.
2. Wrap your App: `import { withNotes } from "./applib/notes.mjs";` and
   `export class App extends withNotes(DurableObject) { … }`. Your own
   operations stay in the class; none may be named `notes`, `annotate`,
   `resolve` or `remove_note` (the App refuses to start if one is). With
   no `app.mjs` yet, copy this one.
3. Merge this `fragment.json`'s four operations into yours, and its
   `notes` channel into your `channels`.
4. In your page: link `./notes.css`, load `./notes.js` as a module, mark
   the column with `data-notes` (else `<main>` is the column) and each
   block that takes notes with a `data-a` id. For the margin, put the
   column in `<div class="notes-page">` (notes.css sets
   `--notes-column`, its width beside the margin, to 44rem); without it,
   notes open under their paragraph at every width.
5. `fragment deploy <name> --dir <dir>`.
