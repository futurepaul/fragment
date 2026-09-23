# notes

A folder of markdown as a live site, on fragment's file model: the notes
are files in git, and the viewer follows them.

- Everything outside `site/`, `app.mjs`, and `fragment.json` is a note:
  markdown with `[[wikilinks]]`, code with highlighting, images inline.
  Files of 1 MiB or more are stored as blobs (pointers in git); the site
  serves their bytes.
- Edit by syncing the folder: `fragment sync <name> --dir . --watch`
  keeps it live both ways. Notes do not need a deploy: the viewer reads
  them at `main`. Only a change to `site/` or `app.mjs` needs `fragment
  deploy`.
- `fragment.json` declares a file trigger, `{"files": "**", "run":
  "changed"}`: each move of `main` runs `changed`, and every open page
  re-runs its `last_change` query and refreshes the tree.
- `app.mjs` answers `api/tree` and `api/file` from `this.files`, the
  app's read access to its files at `main`.

The viewer (`site/assets/`) is a prebuilt bundle of marked and
@pierre/diffs.
