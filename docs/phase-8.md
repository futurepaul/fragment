# Phase 8, first part: an agent works on a computer

Built 2026-09-23 overnight on Paul's word ("work on 5-8, the stuff that's
obvious"). The ROADMAP keeps the acceptance; this file keeps what was
built and the choices made without him, for his review.

## What landed

- **`fragment computer serve`** (`crates/computer`, the CLI's `computer`
  feature, so the everyday CLI carries no goose): goose's developer
  tools (shell, write, edit, tree, from `goose-developer` at the fork's
  `12922e7`) over HTTP, ported from `spike/goose-agent`'s `computer/`.
  Every call is journaled on disk by its id: a finished call answers its
  stored result, a running one is re-attached, and one a restart cut off
  answers "interrupted" and does not run again. Its bearer token is a
  file (made 0600 on first start); it holds no platform credential and no
  model key.
- **An agent attaches a computer** (`PUT /api/a/{name}/computer {url,
  token, cwd}`, `fragment agent computer`): the agent checks that the
  computer answers with that token, seals the token like its own key, and
  from then on its turns offer the computer's tools beside its fragments'
  operations. A call's id is `tc-` plus the SHA-256 of the model's
  tool-call id, so a replayed step re-attaches to the same run.
- **Stop kills the command.** goose drops a tool's future when its turn
  is cancelled, so the turn driver cancels whatever computer calls are
  still in flight; and since goose's shell kills only the shell (not the
  command's children), the computer has each command record its shell's
  pid first and kills the whole tree under it.
- e2e `computer` (13 checks), with `fragment computer serve` as a local
  process: the token is made 0600 and required; a wrong token is refused
  at attach; one turn runs a shell command on the computer and a todo
  operation in the same step; stop ends the turn in well under a second
  and kills the command; SIGKILL of the agent's node mid-command, and
  the watchdog's replay runs it once (the computer's record says `runs:
  1`); SIGKILL of the computer mid-command, and after its restart the
  call answers "interrupted", the model hears it, and the command does
  not run again; detaching removes the tools.

## On a real Sprite

A smoke run the same night, on a Sprite made for it and destroyed after
(`fnx-computer-smoke`, x86_64, Ubuntu 26.04):

- The CLI built for Linux (`rust:1.97.1-bookworm`, `--features computer`,
  15 MB, 3.5 minutes under emulation) ran as the Sprite's HTTP service
  (`sprite-env services create computer --http-port 8080`); it made its
  token on first start.
- The Sprite URL stayed org-only: the local dev stack's agent reached the
  computer through `sprite proxy`. The URL itself carries the Sprites
  org auth in the same `authorization` header the computer's token
  uses, so reaching a computer at its URL still needs a design (a public
  URL with the computer's token, as the spike ran, or a token in another
  header).
- With the real model (`z-ai/glm-5.3-flash` on OpenRouter), an agent
  wrote two pages and listed them in 3 to 8 seconds a turn; each tool
  call through the proxy took 0.1 to 0.9 s. The first run put its files
  in `site/site/`: the instructions now say paths are relative to the
  project directory, and a fresh agent then wrote them where they
  belong.
- Stop: a turn running `sleep 297 && …`, and one running `sh -c 'sleep
  296; …'` (a command a shell may exec in its own place), each ended
  0.4 to 0.7 s after `fragment agent stop`, with nothing of the command
  left running.

## Choices made without Paul (review these)

- **The loop stays in the agent's cell for now; the computer runs the
  tools** (the spike's "shape 1"). MODEL.md and the spike's handoff
  chose the other shape (the loop on the computer, "where the agent's
  hands are"), and it stays the target. But it needs model access on the
  computer without a key on its disk (Sprites connectors, or a platform
  inference endpoint with a per-computer token, `docs/secrets.md`) and
  a way for the computer's loop to call the fragments' operations as the
  agent. Neither is built. This shape needs neither: the computer holds
  only its own token, and both kinds of tool meet in one turn. The
  computer's call journal is the same one the other shape uses, so
  moving the loop later adds `/drive` and the effects journal (both in
  the spike) without changing this.
- **Attaching is by URL and token, by the owner.** The `Computer` cell
  (a Sprite's lifecycle through the Sprites API, with our org token) is
  not built: it holds a platform secret and creates Sprites, which is
  infrastructure and spend.
- **The shell workaround lives in `fragment computer serve`**, not in
  goose: the fix belongs in `goose-developer` (a process group per
  command, and `killpg` on cancel and on timeout), but that means a new
  commit pushed to `futurepaul/goose`, which is Paul's to publish. The
  same bug leaves a timed-out command's children running; the workaround
  covers cancel only.
- **The e2e builds the CLI with `--features computer`.** `cargo install
  --path cli` without it answers `fragment computer serve` with how to
  build it.

## Dependency cooldown

Adding goose to the workspace pulled five packages younger than two
weeks; they are pinned to older releases (uuid 1.26.0, rustix 1.1.4,
process-wrap 9.1.0, rmcp and rmcp-macros 3.2.0). Checking every lockfile
the same way found eleven in `agent/Cargo.lock` that phase 5 had let
through (bitflags, smallvec, tinyvec, yoke-derive, zerofrom-derive,
cfg-if, syn, unicode-ident, cc, find-msvc-tools, rand); they are pinned
back too. The root and `cell/` lockfiles have none.

## Not yet

- The `Computer` cell and Sprites lifecycle (create, wake, idle, holding
  a Sprites Task during a turn), and credentials through Sprites
  connectors.
- The loop on the computer (`/drive`), once computers have model access
  without a key on disk.
- A release artifact for computers (a Linux build of the CLI with
  `computer`), and file sync between a computer and its fragment through
  git (builder workspaces).
- Phases 6 (the desktop) and 7's sharing (the share sheet, invites in the
  shell): both need phase 4's sign-in.
