---
name: brain-finite
description: Keep and search a brain — a knowledge base of markdown wikis in a brain fragment — with the fragment CLI. Use to ingest sources, write and connect wiki pages, search what a brain knows and answer from it with citations, or share a brain. Replaces FiniteBrain (fbrain) and llm-wiki on fragment.
---

# Brain

A brain is a fragment on the platform's `brain` template: a knowledge base
of markdown files that people and their agents keep together. Its files
are in git; the `fragment` CLI syncs them with a folder, and the brain's
`search` operation finds them by section. You are the wiki's compiler and
its query engine: sources go in as they were, knowledge comes out as
connected pages, and answers cite both.

This skill replaces FiniteBrain (`fbrain`, Brain Working Trees, Folder
keys) and `llm-wiki` on fragment. Its wiki method is adapted from
nvk's llm-wiki (MIT, `LICENSE` beside this file).

## The contract

What every brain answers:

- **Files.** Markdown pages at main, written one at a time with `fragment
  write`, or synced as a folder by `fragment sync`. A brain needs no deploy: it reads its files at
  main, and a file trigger reindexes what changed. A file of 1 MiB or more
  becomes one of its blobs on its own as it syncs.
- **Wikis.** Each top-level folder is a wiki, with `raw/`, `wiki/`,
  `inventory/`, `datasets/` and `output/`, and its own `AGENTS.md`,
  `index.md` and `log.md`. The bytes of a source that is not markdown go
  under `raw/assets/`, each with one source note whose frontmatter names
  its `resource` and `finite_asset.content_hash`.
- **`search {q, limit?, wiki?}`**, a query operation:

  ```sh
  fragment call <brain> search --input '{"q": "battery storage"}'
  fragment call <brain> search --input '{"q": "tariff", "wiki": "garden", "limit": 20}'
  ```

  answers ranked sections, best first: `{"results": [{rank, path, wiki,
  title, heading, ancestry, snippet}], "pending"}`. `q` is plain words (1
  to 256 characters, at most 16 words), each a prefix match and all of them
  required; search syntax means nothing. `limit` is 1 to 50 (10 unless
  you ask); `wiki` narrows it to one wiki. `pending` above 0 means the
  index is still taking in a sync: ask again in a moment.
- **`guide`**: the brain's own AGENTS.md, how to ingest into it and keep
  it. Read it before you write:

  ```sh
  fragment call <brain> guide --input '{}'
  fragment call <brain> index_status --input '{}'   # what is indexed, pending, or skipped and why
  ```

Where the brain's guide and a wiki's own `AGENTS.md` speak, they win over
this skill.

## Find or make one

```sh
fragment list --json                       # brains are kind "brain"
fragment create garden-brain --template brain --title "Garden notes"   # a new brain, on the platform's brain template
```

Ask which brain when several could fit; make one only when the human
wants a new one. What you make is your owner's, with you as its editor.

## The loop

```sh
fragment write <brain> garden/raw/<slug>.md --from note.md   # one source in, through the platform
fragment sync <brain> --dir ~/brains/<brain>          # or read and write it as a folder
# orient, edit with ordinary file tools…
fragment sync <brain> --dir ~/brains/<brain>          # one commit; conflicts land beside as .conflict- copies
fragment call <brain> search --input '{"q": "…"}'     # check that what you added is found
```

1. **Orient before writing.** Read the brain's guide, the wiki's
   `AGENTS.md`, its `index.md`, the end of its `log.md`, and search for what
   you are about to add, so you update a page rather than duplicate it.
2. **Ingest a source** once, as markdown under the wiki's `raw/`, named by
   date and subject (`raw/2026-10-03-solar-tariffs.md`), with frontmatter
   `type: source`, `title`, `source` (its URL) and `captured`; then its
   text, cleaned but not summarized. A source that is not markdown (an
   image, a PDF, a recording) keeps its bytes under `raw/assets/` and gets
   one source note under `raw/` (`type: asset`, its `resource` path, and
   `finite_asset` with its `content_type`, `size` and `content_hash`).
3. **Synthesize into `wiki/`**: update the pages it bears on, or make one
   per durable topic, with frontmatter `title`, `summary`, `tags`,
   `sources` (the raw notes it draws from), `created` and `updated`.
   Connect claims, dates, people and open questions; do not copy the
   source. Link pages with wikilinks (`[[solar-tariffs]]`, or by path from
   the brain's top).
4. **Close the change**: give every new page a line in the wiki's
   `index.md`, append one dated entry to its `log.md` (what came in, what
   pages changed), put follow-ups in `inventory/`, sync, and search for
   what you added.

## Answering from a brain

- Search first; treat results as entry points, then open each page whole
  (in your synced folder) before you answer from it.
- Follow the links that bear on the question, and cite the pages and the
  sources you used.
- When the brain has no answer, say so, and say what to ingest. Never
  answer from memory as though the brain said it.

## Rules

- Raw is immutable: correct and interpret in `wiki/`, never in `raw/`.
- Each wiki's `index.md` and `log.md` speak of that wiki alone; never
  rewrite old log entries.
- Prefer updating a page to making a near-duplicate. Delete a page only
  when someone asks you to.
- Never write a brain's code (`app.mjs`, `applib/`, `site/`) or a file its
  guide says is generated: a brain that carries code of its own is a fork.
- Everyone the brain is shared with reads all of it: there are no folder
  permissions and no encryption. Put what a narrower group should see in
  a brain of its own, and never copy from a narrower brain into a wider
  one.
- Write long pages in pieces, a section at a time.

## Sharing

```sh
fragment members add <brain> <npub> --role editor   # they and their agents keep it with you
fragment members add <brain> <npub> --role viewer   # they read and search it
fragment visibility <brain> members                        # nobody else
```
