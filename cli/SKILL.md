---
name: fragment
description: Build, publish and share small stateful, multiplayer web apps (fragments) for the person you work with, on {{platform}}, with the `fragment` CLI. Use when they want an app, a page, a tracker, a shared list or a webhook inbox online at a link, want to change or share one, or want to use their apps as tools.
---

# fragment

You are helping a person build on Fragment, at {{platform}}. A fragment
is a small web app at its own link, {{fragment}}: pages, an app of
named operations over its own SQLite, channels that every open page
follows live (multiplayer is built in), and members with roles. Make one
when your person wants something on the web that keeps its state or
that others use with them: a shared list, a tracker, a poll, notes, a
webhook inbox, a small site. Your person signs in at {{platform}}, where
their apps are, and shares them from there or through you.

This page is the handoff. {{platform}}/llms-full.txt (the same text as
`fragment guide`) is the whole manual (the folder, operations, jobs and
their steps, deploys, sharing, who can do what where, the ledger,
errors): read it all before you build past a template.

## Install

macOS or Linux, with no sudo (on Windows, run it in WSL). The same line
updates it:

```
mkdir -p ~/.local/bin && curl -fsSL https://github.com/finitecomputer/fragment/releases/latest/download/fragment-$(uname -s)-$(uname -m).tar.gz | tar -xzf - -C ~/.local/bin
```

If `fragment` is then not found, `~/.local/bin` is not on the PATH: run
it as `~/.local/bin/fragment` (an agent's shell may not keep an
`export` from one command to the next), or run `export
PATH="$HOME/.local/bin:$PATH"` and add that line to `~/.zshrc` or
`~/.bashrc`.

In a sandbox that reaches only the hosts it allows (Claude Code on the
web, Codex cloud), the install needs `github.com` and
`release-assets.githubusercontent.com`, and the CLI needs {{platform}}
(and `*.code.storage` for `fragment sync` and `deploy --dir`). When one
is blocked, ask your person to allow it in the environment's network
settings. Each new sandbox is a new machine: pair it again.

An agent on a Fragment computer (`FRAGMENT_AS_AGENT` is set) needs neither
this nor pairing: `fragment` is installed there, and acts as the agent.

## Pair

```
fragment login --host {{platform}}
```

Once per machine. It prints a code and opens {{platform}}/cli, where
your person signs in and types it (both show the key's ending); it says
whose account it acts as, and keeps the host. With no browser,
`--no-wait` prints the code and the page: give both to your person (ten
minutes, once), then run `fragment login` again; paired, it says whose
it is. From then on the CLI acts as them. Their account must make apps:
a guest only edits what is shared with them.

## Build, deploy, share

```
fragment init my-app --template todo             # a folder my-app/, created and live: prints its links
fragment deploy my-app --dir my-app              # after an edit: sync the folder, then put it live
fragment status my-app                           # its links, its live commit, why its code was refused, its page's errors
fragment events my-app --tail 30                 # what happened there: believe it over your memory
fragment call my-app add --input '{"text":"hi"}' # one of its operations
fragment visibility my-app link                  # public | link (anyone with the share link) | members
fragment members add my-app bea@example.com --role editor   # by email; viewer | editor
fragment list                                    # your person's fragments
fragment create <label> --template todo          # one with no folder: blank|todo|inbox|calories|doc
fragment write <name> site/index.html --from index.html     # one text file to main
fragment deploy <name>                           # main goes live: the site and the app
fragment mcp <name> [--write]                    # its described operations as an MCP server's tools
fragment agent <agent> --name Frank --picture frank.jpg   # an agent's name and picture (an agent: its own, no <agent>)
```

A fragment is named `<label>--<suffix>` (`todo--k3x9`: the platform adds
the suffix); a bare label names the one of your person's with it.

Go in this order: say the app in one sentence (who it is for, the job it
does); build the smallest version that does that job, and deploy it;
reload it and check its state is still there, then open it twice and
check a change in one shows in the other without a reload; hand back its
link with one suggested next step. Add nothing they didn't ask for.

Who can open a fragment is the person's to decide: before you make one
public or share it with someone new, ask them in plain words, unless
they asked for exactly that, and then say what you did.

## Design

Apps default to no header: no title bar, banner, hero, or page-title
heading unless the person asks for one. The shell already frames the app
with its title; start with useful content. Design for a narrow pane
(about 380 px) and phones: one column, thumb-sized targets, nothing that
needs a hover. Link `./__fragment.css` for the platform's look, then
build on its theme variables and base styles: warm greys and one accent
color, used sparingly. None of: gradients, glow, decorative emoji, heavy
shadows.

## What an app has

- **Operations**, named in `fragment.json` and written in `app.mjs`: a
  query reads, a mutation changes the app's SQLite (all or nothing, once
  per id), a job runs durable steps. Each has a `role` (who may call it)
  and an `input` JSON Schema. Give every operation you write a
  `description`: what it does and what it answers, in plain text
  (`"description": "Add a todo with this text. Answers its {id}."`).
  People and agents read it in `fragment status` to know what to call,
  and a described operation is a tool (below).
- **Pages** import `./__fragment.js`: `fragment.call`, `fragment.live` (a
  query that re-runs after every change), `fragment.subscribe` (a
  channel's records, live, and with `onDraft` its drafts as they
  stream), presence, web push.
- **Channels**: records a mutation publishes and pages follow live;
  `inbox` takes webhooks, `events` is the log.
- **Triggers**: a cron, a record on a channel, a change to files.
- **Job steps**: `job.fetch` (a secret `{{NAME}}` in a header is filled
  outside your code, toward the hosts it names: `fragment secret set
  <name> NAME --to api.example.com`), `job.sleep`, `job.files`, `job.push`, and AI,
  which the fragment's owner pays for: `job.ai.text` (text on the
  platform's model tiers, `cheap` or `medium`; with OpenAI's `tools` the
  model asks for calls your job runs, a tool loop with no computer, and
  with `draft: {channel, turn}` its words stream to the pages following
  that channel until the record you publish under the turn replaces
  them), `job.ai.decide` and `job.ai.image` (a JPEG into the fragment's
  files).
  - `job.ai.decide`: typed answers (one of named options, a yes-or-no
    probability, a score on ordered levels) about text, JSON, or a
    photo, voice note or video someone uploaded (`fragment.blob(file)`'s
    `{sha256, type}`), with no prose to parse: to sort, route and gate.
    `model`: `clef-flash` (the default), `clef`, or `clef-omni` (audio
    and video). An agent asks the same from its computer, `POST
    $FRAGMENT_MODEL/v1/decide`.

## Use their apps as tools

`fragment mcp <name>` serves a fragment's described operations as MCP
tools over stdio, as your person: its queries, and with `--write` its
mutations and jobs too.

```
claude mcp add groceries -- fragment mcp groceries            # Claude Code: read only
claude mcp add groceries -- fragment mcp groceries --write    # and change things
codex mcp add groceries -- fragment mcp groceries             # Codex
```

`fragment list` shows what your person has. An operation without a
`description` is no tool: describe the ones a model should use.
