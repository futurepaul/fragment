# Phase 5: agents as members

Started 2026-09-23 overnight on Paul's word ("work on 5-8, the stuff
that's obvious"). The ROADMAP keeps the acceptance; this file keeps what
was built and the choices made without him, for his review.

## What landed

*Core done 2026-09-23.* `agent/` is its own celld project (MODEL.md,
Agents): a workers-rs Durable Object per agent, goose's loop
(`goose-agent`, the fork at `12922e7`) as its turn, its conversation in
its SQL. 6.5 MB of wasm, as the spike measured; fragment cells carry none
of it.

- **An agent has its own key.** The owner creates it (`POST
  /api/agents`, NIP-98); the cell makes a secp256k1 key from the
  platform's randomness and keeps it sealed under the fleet's host
  secret. Only its owner drives it (turns, stop, view).
- **Its tools are the operations of the fragments it belongs to.** An
  owner adds the agent's npub as a member of a fragment, like anyone;
  the agent reads its memberships (`GET /api/fragments`, signed with its
  key) and each fragment's operations (`status.code.operations`), keeps
  those its role there may call, and offers each as a tool named
  `<fragment>__<op>` with the operation's input schema. A call is the
  operation, over the platform's signed API, as the agent.
- **A replayed call is a replayed operation.** The operation id is
  `tc:` plus the SHA-256 of the model's tool-call id (`crates/core/src/
  tools.rs`), so a step a crash interrupted calls again with the same
  id and the fragment's ledger answers without running it twice.
- **Turns** are the spike's driver: each step loads the conversation,
  runs one goose operation, and applies its effects; a watchdog alarm
  replaces a driver that died with its node. A message during a turn
  steers it (a durable queue read between steps); stop cancels a tool in
  flight.
- **Dev and e2e:** `cargo xtask dev` runs agents on :8793 beside the
  cell (their model key from the file `OPENROUTER_API_KEY_FILE` names);
  `cargo xtask build` and `check` build and lint `agent/`; the e2e
  section `agents` (18 checks) drives a scripted OpenRouter fake, which
  now streams and answers scripted tool calls.

e2e `agents`: create (401 unsigned, 409 taken, 403 a stranger); no tools
without a membership; a membership's operations as tools, and nothing of
a fragment it is not in; a turn in which the model calls `add_todo` and
answers, the todo landing as the agent's key; steer mid-tool (the model
reads the steer after the tool); stop mid-tool well before the tool
would end; SIGKILL after the operation ran and before its result was
saved (the watchdog replays the call by its id, and the todo lands
once); SIGKILL between steps (nothing runs again).

## Choices made without Paul (review these)

- **Agents reach fragments over the public signed API** (`FRAGMENT_API`),
  not a binding: the agent fleet can be a separate celld app (a fleet
  runs one application), and an agent can do on a fragment exactly what
  its membership allows, through the one API the CLI uses.
- **The model key is the agent fleet's own** (`OPENROUTER_API_KEY`) until
  phase 4's per-person budgets meter it (debt ledger).
- **The turn driver is the spike's alarm-and-`waitUntil` driver**, not
  Workflows (MODEL.md already chose this).
- **Only the owner drives an agent.** Other people reach it through the
  fragments it is in (a chat, phase 7).
- **Test controls** (holds, the watchdog period) answer only where
  `AGENT_TEST_HOOKS=allow` (dev and e2e fleets).

## Not yet

- Hosting the agent fleet: a second celld app beside the cell's on Fly
  (its own bucket prefix and Machines, or a process beside each node).
  Infrastructure and spend: Paul's call.
- Streaming a turn's tokens to viewers; an owner's list of their agents.
- The computer shape (phase 8): the loop on the agent's Sprite.
