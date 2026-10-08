# How this wiki works

## Editing

Press **Edit** on a page and save: your change is a commit, and
everyone with the page open sees it at once. Who else is reading or
editing a page shows at its top.

## Linking

- `[[Page name]]` links to a page by its name, and `[[page-name|other
  words]]` shows other words.
- A link to a page that does not exist yet is red: follow it to write it.
- An image beside a page shows inline: `![a diagram](diagram.png)`.

## The folder

Every page is a file under `wiki/`. Sync the folder to work on it in
any editor, or to let an agent keep it:

```
fragment sync <name> --dir ./wiki-folder --watch
```

A save there is a commit too: it shows here at once, and Recent
changes lists it as from the folder. Every version stays in the
fragment's git history.
