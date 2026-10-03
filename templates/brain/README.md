# brain

The blessed brain template (docs/cloudflare-v1.md, decisions 30 and 40): a
brain is a fragment app whose files are a knowledge base that people and
their agents keep together, searched by section. A brain's repo names the
template and holds only its face (`fragment.json`'s `meta`) and its files;
the platform's release serves the rest, so every brain runs the same code
and one deploy updates them all. The platform knows nothing of brains:
everything here is this template's app code and files.

Make one from the shell (Add an app, Brain) or the API:

    POST /api/fragments {"name": "garden", "template": "brain", "title": "Garden"}

## The files

Kept as Finite Brain keeps them (its wiki skill, adapted to the fragment
CLI in the brain's AGENTS.md, `applib/guide.mjs`):

- Each top-level folder is a wiki: `raw/` (sources as captured, never
  edited), `wiki/` (synthesized pages), `inventory/`, `datasets/`,
  `output/`, with its own `index.md` and append-only `log.md`, and an
  optional `AGENTS.md` of its own conventions.
- An asset (an image, a PDF, a recording) is a file under `raw/assets/`
  with one source note under `raw/` whose frontmatter names it
  (`type: asset`, `title`, `resource: raw/assets/<file>`, and
  `finite_asset: {content_type, size, content_hash}`). A file of 1 MiB or
  more is one of the fragment's blobs (a pointer in git: docs/api.md,
  Blobs), which the CLI uploads when it syncs, and which a pointer at
  `main` keeps: no record needs to name it.
- Agents sync a folder with `fragment sync <brain> --dir <folder>` and
  write with ordinary file tools; a brain needs no deploy (it reads its
  files at `main`).
- Access is the fragment's members: everyone it is shared with reads all
  of it. There are no folder permissions and no encryption.

## Search

`search {q, limit?, wiki?}` (a query, viewers and up) answers ranked
sections, best first: `{q, words, results: [{rank, path, wiki, title,
heading, ancestry, snippet}], pending}`.

- **The index** is FTS5 in the app's own SQLite (`unicode61`, accents
  removed): a row per section (the text under a heading, with the headings
  above it and the page's title, `applib/sections.mjs`), ranked by BM25
  weighted title 4, heading 3, path and ancestry 2, body 1. It indexes
  markdown, not a wiki's `AGENTS.md` or a made `_index.md`.
- **Kept by a file trigger.** `{"files": "**", "run": "changed"}`: each
  move of `main` runs the job `changed`, which marks the paths that moved
  (`mark`, a mutation, which open pages follow through `last_change`),
  then indexes them 32 files a step (`reindex`, an editor's query that
  reads the files at `main` and writes the index) until none are left.
  Each path is marked at a generation, so a read overtaken by a newer
  mark writes nothing. A page whose bytes hash the same keeps its sections:
  the same files again change nothing. More than 200 paths in one move,
  or an index of an older shape (`FORMAT`), read every file again. A
  search first takes up to 8 pending files itself, so an index behind a
  sync catches up even when no trigger runs.
- **A query is words**: runs of letters, digits and marks, each a quoted
  prefix phrase, all required. FTS5's syntax (`OR`, `NOT`, `NEAR(…)`,
  quotes, `column:`, `*`, `^`) is never read.
- **Limits.** `q` is 1 to 256 characters (the schema: 400 past it) and at
  most 16 words (422); `limit` 1 to 50 (10 by default); a snippet at most
  300 bytes. A page of 1 MiB or more is a blob, not read, and is listed as
  skipped; a page keeps at most 500 sections of 8000 characters. Past 12
  MiB of database the index takes no new page (they are skipped "the index
  is full", and tried again at their next change), so the app's mutations
  keep room under its 16 MiB.
- `index_status {}` answers `{format, pages, sections, pending, writes,
  skipped}`; `reindex {full: true}` (editors) reads every file again.

## The page

`site/` is the notes template's viewer (`site/assets`, a symlink to
`templates/notes/site/assets`, embedded once: crates/templates/build.rs),
which reads `api/tree` and `api/file` from `app.mjs`, plus the brain's
search box (`brain.js`, `brain.css`). The viewer's bundle is prebuilt and
left as it is (docs/technical-debt-ledger.md). `api/tree` lists the
brain's files and two it makes: `_index.md` (its wikis, the landing) and
`AGENTS.md` (the guide), each unless the brain holds one of its own. An
asset's bytes come from the platform's `__file`.
