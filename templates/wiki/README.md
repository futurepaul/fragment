# wiki

A team's wiki in markdown, on fragment's file model: the pages are the
files under `wiki/`, so people edit them on the page and agents edit
them through the folder, and every open page follows either at once.

- A page is `wiki/<name>.md`, at `#/<name>`. `[[Page name]]` links to a
  page by its name (red until it is written), and an image beside a page
  shows inline (`![](diagram.png)`, served from `__file`).
- Editors edit on the page: `save`, a mutation, writes the file
  (`call.files.write`), one commit to `main` once it commits. It writes
  only pages: a `.md` file under `wiki/`, never the site or the code.
- Or edit through the folder: `fragment sync <name> --dir . --watch`
  (or `fragment write <name> wiki/x.md --text …`). Pages need no deploy:
  they are read at `main`.
- `fragment.json` declares a file trigger, `{"files": "wiki/**", "run":
  "changed"}`: each move of `main` that touches a page runs `changed`,
  and every open page re-runs its live queries (`page`, `pages`,
  `recent`). `changed` keeps Recent changes: a page's edit with who made
  it and its commit, the folder's with its commit.
- Presence says who else is reading or editing a page; an editor whose
  page changed under it is told before it saves (last writer wins, and
  git keeps both).

The page is `site/index.html`, `site/wiki.js`, and `site/markdown.js`
(text only, never HTML from a page).
