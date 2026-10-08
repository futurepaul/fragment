---
name: git-finite
description: Keep files in git on fragment — a fragment's repo, synced with a folder by the fragment CLI, its history, deploys and rollbacks. Use when creating, syncing, reading, restoring, or sharing a versioned folder of files, or when the human asks where something's history is or to undo a change.
---

# Git on fragment

Every fragment is a git repository: `main` is the working copy, `live` is
what its site and app run. The `fragment` CLI moves a folder to and from
it, with no local `.git` and no git credentials: your computer's API signs
each request as you. A fragment needs no code to be a repository; a
folder of notes, data or source is a fragment like any app.

This skill replaces Finite Sites Project Repositories (`fsite` and its git
remotes) on fragment.

## Make a repository

```sh
fragment create notes-project            # an empty fragment: notes-project-<suffix>, your owner's
mkdir -p ~/repos/notes-project && cd ~/repos/notes-project
# add files…
fragment sync notes-project --dir .      # one commit with everything new, then main is the folder
```

`fragment init <name> --template blank` makes, scaffolds and deploys in
one step when the repository is also a site.

## Work on one

```sh
fragment list                                        # what you can reach, and your role in each
fragment sync notes-project --dir ~/repos/notes-project              # push and pull (a mirror pass)
fragment sync notes-project --dir ~/repos/notes-project --mode pull  # a read-only copy (never deletes)
fragment sync notes-project --dir ~/repos/notes-project --mode push  # the folder up, nothing down
fragment sync notes-project --dir ~/repos/notes-project --live       # what is live, not main
fragment verify notes-project --dir ~/repos/notes-project            # a full-content audit
```

- **One commit per pass**, against the head it read. If `main` moved
  underneath, sync reads again and retries (at most 3 times), then fails
  with `conflict`. An unchanged folder commits nothing.
- **Conflicts**: when both sides changed a file to different bytes, yours
  stays and the other lands beside it as
  `<path>.conflict-<time>-<writer>` (exit 3). Read both, merge by hand,
  delete the copy, and sync again.
- **Deletions converge**; a locally modified copy wins over a remote
  delete. A pass that would delete more than max(3, 30%) of the known
  files is refused (exit 4) until `--apply-mass-delete`: check first.
- **Large files**: 1 MiB and up are blobs; sync uploads and downloads the
  bytes, so the folder always holds real files.
- **What syncs**: everything but dot files and folders (`.git/` included),
  the top-level `node_modules/`, editor droppings, and the `.conflict-`
  copies. In a git working tree, `.gitignore`d files never upload.
- Exit codes: 0 clean, 1 failure, 3 conflicts, 4 guard tripped.

To keep a folder synced while you work in it, `fragment sync <name> --dir
. --watch` runs in the foreground; run it under your terminal's background
process tools, and stop it when the task ends.

## History, deploys, and rollback

```sh
fragment drafts notes-project              # the deploy history: live's commits
fragment deploy notes-project              # move live to main's tip (its site and app)
fragment rollback notes-project            # live back to the deploy before
fragment rollback notes-project --to <sha> # or to a named one
fragment events notes-project --tail 30    # what happened, who did it
```

Rolling back moves `live` only: `main` keeps every commit, so nothing is
lost, and the next deploy goes forward again. To undo a change on `main`
itself, put the file back as it was (a `--live` pull into another folder
has the deployed version) and sync: the undo is a new commit.

## Sharing a repository

```sh
fragment members add notes-project <npub> --role editor   # they (and their agents) can sync it
fragment members add notes-project <npub> --role viewer   # they can read it
fragment invite create notes-project --role editor               # a link a person opens to join
fragment visibility notes-project members                        # nobody else, not even with the link
```

Edit access to the files and who can open its site are the same grants
on fragment: a viewer can read every file. Keep what a narrower group
should see in a fragment of its own.

## Guardrails

- Keep `.env*`, private keys, tokens, credentials and build caches out of
  the folder you sync.
- Never reconstruct source from a served page: sync its fragment.
- Do not move other people's work aside to make a sync pass: read the
  conflict copies and merge.
- The repository is the truth; your folder is a working copy.
