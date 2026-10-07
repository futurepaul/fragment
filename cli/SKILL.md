---
name: fragment
description: Publish small stateful web apps with built-in multiplayer on fragment.club, using the `fragment` CLI. Use when the person wants to make, publish, share, or change a fragment, or wants a web page or app of theirs online at a link.
---

# fragment

A fragment is a small web app at its own link: pages, an app of
operations over its own SQLite, channels that every open page follows
live (multiplayer is built in), and members with roles. Use one when the
person wants something on the web that keeps its state or that others
open with them: a shared list, a tracker, notes, a webhook inbox, a chat,
a small site. fragment.club is invite-only: the person must have been
invited.

## Install

On macOS or Linux, with no sudo:

```
mkdir -p ~/.local/bin && curl -fsSL https://github.com/futurepaul/fragment/releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin
```

If `fragment` is then not found, `~/.local/bin` is not on the PATH: run
`export PATH="$HOME/.local/bin:$PATH"`, and add that line to
`~/.zshrc` or `~/.bashrc`. The same command updates it.

An agent on a Fragment computer (`FRAGMENT_AS_AGENT` is set) needs neither
this nor pairing: `fragment` is installed there, and acts as the agent.

## Pair

`fragment login`, once per machine. It opens fragment.club, where the
person signs in and approves this machine's key (the page and the
terminal show the same ending). With no browser at hand, `fragment login
--no-wait` prints the link: give it to the person, and run `fragment
login` again once they have approved it. A new person also chooses a
username once, on that page or with `fragment username <name>`.

## Your fragments

```
fragment whoami                                  # who you are (an agent: for whom it acts)
fragment list                                    # the fragments you have a role on, and the role
fragment status <name>                           # its links, its live commit, why its code was refused, its page's errors
fragment events <name> --tail 30                 # what happened there: believe it over your memory
fragment sync <name> --dir <folder> --mode pull  # its files, into a folder
fragment create <label> --template todo          # a new one (`fragment new --list`: the templates)
fragment write <name> site/index.html --from index.html   # one text file to main
fragment deploy <name>                           # main goes live: the site and the app
fragment call <name> <operation> --input '{}'    # one of its operations
```

A fragment is named `<label>.<username>`; a bare label names one of yours
(an agent's: its owner's).

## Then

Run `fragment guide` and read it all before you build: it is the whole
manual (the folder, operations, deploys, sharing, the ledger, errors).
