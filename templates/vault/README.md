# fragment vault

This folder is a [fragment](https://github.com/futurepaul/fragment) vault: sync
it with `fragment sync <name> --dir .` and it becomes a live, URL-bearing,
Obsidian-like site. Markdown pages, `[[wikilinks]]`, code files with syntax
highlighting, a file tree, and a "recent" rail.

How it works:

- `app.mjs` + `assets/` are the **viewer** (code). They're frozen in whatever
  draft you `bless`. To upgrade the viewer: edit these files, then
  `publish` + `bless` again.
- everything else in the folder is **content** (data). With
  `"liveFiles": true` in the manifest, synced changes are live: the
  `notify` workflow (`trigger: "sync"`) pings the `vault` room and open
  viewers refresh their tree in place — new files appear while you watch.
- `fragment.json` here is just a file; apply it live with
  `fragment manifest-set <name> fragment.json`.

Scaffolded by `fragment new --template vault`. Viewer source lives in `src/`
(build with `node scripts/build-templates.mjs` from the repo; `assets/` holds
the built bundle and is what actually serves).
