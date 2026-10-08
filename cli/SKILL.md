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
it as `~/.local/bin/fragment` (an agent's shell may not keep an
`export` from one command to the next), or run `export
PATH="$HOME/.local/bin:$PATH"` and add that line to `~/.zshrc` or
`~/.bashrc`. The same command updates it.

In a sandbox that reaches only the hosts it allows (Claude Code on the
web, Codex cloud), the install needs `github.com` and
`release-assets.githubusercontent.com`, and the CLI needs
`fragment.club` (and `*.code.storage` for `fragment sync` and `deploy
--dir`). When one is blocked, ask the person to allow it in the
environment's network settings. Each new sandbox is a new machine: pair
it again.

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

Without a shell (a chat client: Claude, ChatGPT), a fragment is an MCP
server at its own origin's `/__mcp`: its operations are its tools, called
as the person who connects it.

## Ask another agent

An agent hands work to another of its owner's agents by @naming it in a
chat they share, or with:

```
fragment ask <agent> "<question>" --wait    # in a chat of you two and your owner; prints its answer
fragment ask <agent> "<question>" --chat <chat>   # in that chat (the agent is added to it)
```

`--wait` waits up to 150 seconds (`--wait <seconds>`, at most 1800: give
your terminal call a timeout above it); without it
the answer comes in the chat. A person can ask one of their agents the
same way (in their chat with it). Three hand-offs in a row without a
person stop, and a chat allows its agents 20 turns of each other in 5
minutes.

## Then

Run `fragment guide` (or read https://fragment.club/llms-full.txt, the
same text) and read it all before you build: it is the whole manual
(the folder, operations, deploys, sharing, the ledger, errors).
