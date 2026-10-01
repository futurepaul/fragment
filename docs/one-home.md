# One home: the desktop, any agent

A person signs in and lands on their desktop. Everything they talk to is
a chat, everything that runs is a computer, and everything they make is
an app. Their chats are one list, whoever answers each one: their goose
agent, their Hermes, or only people. They invite someone to a chat and
that person's face shows by what they write, the agent's too. A
computer's screen shows beside the chat, and its owner can take it over
from the agent.

The prototype: https://claude.ai/artifact/NtbMtM6vm5evbTzJ38NxQG (the
home, a new chat, an invite, settings, a multiplayer turn, the
architecture).

## Decisions (Paul, 2026-10-01)

1. **Chats live in fragments.** A chat is a fragment's `chat` channel,
   as decision 24 has it, whoever answers. Hermes keeps its own session
   per chat, but the conversation is the fragment's. So the platform
   holds chat text: the privacy of a person's own machine covers the
   computer (its screen, files, and Hermes' own state), not its chats.
2. **Hermes answers through its Relay.** Hermes' gateway has a generic
   connector protocol, Relay (experimental, contract version 1): the
   gateway dials out to a connector, which hands it messages with who
   sent them and turns its sends and edits into the platform's own. The
   platform is the connector. Hermes keys one session per chat and
   reads each person's words as `[name] …`, as it reads a Telegram
   group. The fallback, should Relay break, is a Hermes platform plugin
   with the same shapes.
3. **Relay's secret may live in the guest.** Relay authenticates the
   gateway's link with a per-gateway secret. It lives in the Hermes
   computer, an exception to docs/runtime-seam.md decision 3 ("secrets
   never enter the guest"), scoped so: it can only act as that Hermes in
   the chats that name it; it is revoked with the computer; it reaches
   no model key, OAuth token, or other fragment.
4. **Hermes queues.** A message that arrives mid-turn waits for the turn
   to end (`display.busy_input_mode: queue`); Hermes' default would let
   a second person's message cut off the first's turn.
5. **Only the owner takes over a screen.** In a shared chat, guests watch
   the computer's screen; its owner alone takes it over from the agent
   and gives it back. The screen holds the owner's logins.
6. **Guests' turns are the owner's.** A turn a guest starts in a chat is
   paid from the chat's owner's budget, as the owner's own are. The
   owner chooses who can chat.
7. **The desktop is home.** It becomes the person's home fragment, made
   on first sign-in as their memory fragment is, and `/` opens it.
   What the home page holds today (username, picture, budget, pairing
   the CLI, and later connections) moves to `/settings`. This amends
   ROADMAP decision 19, which kept the desktop a showcase with no new
   features (decision 25).

## The model

Nothing new to learn; the existing parts, with Hermes as a second kind
of answer:

- **A chat** is a fragment with `chat` and `work` channels and who
  answers it (its `agent` block). People in it are its members:
  invites, roles, presence, and faces are the fragment's.
- **Who answers:** the person's goose agent (the brain in the cell, the
  hands on their home computer or a throwaway builder: decision 24), or
  their Hermes (brain and hands on its own computer), or no one.
- **A computer** is a fragment that declares one: the default (a Sprite)
  or a preset (`hermes`, later `goose`), wherever it runs
  (docs/runtime-seam.md). A Hermes is always the person's own.
- **A screen** is a computer's display, shown beside the chat, reached
  by the computer's key.

Each answer leaves the same records: its steps as `work` records, its
reply as a `chat` record, and, new, its reply as it is written as a
`draft` that the final record replaces. So one chat page renders both.

## Phases

Each phase is a pull request with its own checks; the deploys between
them are Paul's to approve.

1. **The chat for anyone** (built 2026-10-01). The platform's chat page
   (`cell/chat.mjs`) shows each author's face (a person's picture or
   initial; an agent's, a computer's, or a Hermes' mark; your own agent
   as "Your agent"), and who else is here and who is writing, from the
   fragment's presence (each page shares `{typing}`). `__people` names a
   computer and marks a fragment's Hermes (`preset`). Each fragment's
   owner's row carries what its live manifest says it is (`kind`: a chat
   and who answers it, a computer and its preset, or an app), sent only
   when an install changes it, and the desktop's sections read that, not
   name prefixes (a fragment listed before kinds: by its name until its
   next deploy). The streaming `draft` moved to phase 2, which first
   writes one.
   - Checks (e2e, Chrome): an agent's answer shows its face and whose
     agent it is, and the owner's own as "Your agent"; a second reader
     shows as here on the first page, and the first on theirs, then as
     writing while they type, and gone once they leave; a chat made
     elsewhere under any name is listed under Chats, and the owner's
     list says what each is; a Hermes' fragment is listed as a `hermes`
     computer.
2. **Hermes answers a chat** (built 2026-10-01; the wire:
   docs/hermes-relay.md). A chat names its owner's Hermes as who answers:
   `"agent": {"channel": "chat", "computer": true}` (its own, the `hermes`
   template) or `"computer": "<fragment>"`. The deploy makes the Hermes'
   computer identity an editor of the chat and has the Hermes' cell
   follow its channel. The Hermes cell is the Relay connector: Hermes'
   gateway dials `/api/hermes/<fragment>/relay` with its secret (made with
   its computer, in its spec, with Hermes' managed settings: a shared
   session per chat, streaming, queue mode); the cell hands it the chat's
   messages one at a time, each with its writer's name, kept until acked;
   it turns Hermes' reply into a `draft` (a new, never-stored record on
   `__live`: `PUT …/channels/<channel>/draft`) and then `{text, turn}`,
   its tool progress into `turn.step`s, its reactions into the turn's
   start and end, a Stop into `interrupt_inbound`. It wakes the computer
   as its owner (sandcastle's signed wake) with each message, and while
   Hermes is away. A Hermes removed
   is closed 4401, and its chats join the new one. The `hermes` template
   is now a chat its own Hermes answers, on the platform's chat page; the
   iroh page it had is gone (the client stays, for phase 4's screen).
   - Checks (e2e, on the sandcastle fake's gateway, which speaks Relay as
     Hermes does; 65 in the `hermes` lane): the chat's owner and an
     invited guest answered by name; someone not in it unheard; a
     message during a turn waits, then both answered in order; tool
     steps; a Stop interrupts with no answer; an away Hermes woken and
     answering once; a message kept across a node restart answered once;
     the draft streaming in Chrome, then the answer; 4401 for a removed
     Hermes; its chat joining the new one.
3. **The Hermes image** (built 2026-10-01, but for its proof on lat-6).
   Hermes' own `v2026.9.24-desktop` image (its screen: TigerVNC and Xfce,
   and a browser for it), no image of ours: its init still writes the
   platform's settings (now its screen started with it, its browser shown
   there) and starts the bridge. A Hermes is made with 6 GiB (its screen
   and a headed browser on it); one made before keeps its 4 GiB, since a
   node refuses a computer's new size. Waking needs nothing new of
   sandcastle: the cell holds the computer's key and uses the node's own
   wake. A derived image (the bridge as an s6 service, the settings
   baked, `cua-driver` pinned for `computer_use`) waits for a registry to
   publish to, Paul's to approve.
   - Checks: the spec's (core tests: the image's settings, its size, a
     legacy one's); the proof is a real Hermes on lat-6 answering a chat
     on fragment.club through its Relay, woken from sleep, its turn
     driving its browser on its screen: after the deploy.
4. **The screen.** The computer pane shows Hermes' screen by the
   computer's key (binary WebSockets in the computer client), with Take
   over and Give back for its owner.
   - Checks (e2e, Chrome, fakes): the owner sees frames, takes over and
     gives back; a guest sees frames and has no take-over.
5. **Home.** `/` opens the person's desktop (made on first sign-in);
   `/settings` holds the rest; a new chat asks who answers; an invite
   from the chat.
   - Checks (e2e, Chrome): a new person's first sign-in lands on their
     desktop; settings pairs a CLI; a new Hermes chat, an invite, and the
     guest's first message answered.
6. **The hosted proof.** On fragment.club and lat-6: two people in one
   Hermes chat, a turn each, Hermes' screen in the owner's pane;
   recorded here.

## Open questions

- Relay is experimental: its contract may change without a deprecation
  cycle until Discord and Telegram have validated it. It is pinned to
  Hermes v0.21.5's contract version 1, and the e2e speaks it.
- Hermes reports tool progress as edited text over Relay, not as
  structured tool events, so its `work` steps may be coarser than
  goose's.
- Whether a cell holds Hermes' Relay socket through hibernation on the
  fleet (celld accepts it hibernating, as `__live`; the e2e's restarts
  show the gateway dialing back), and the real gateway's pings against
  it: phase 3's run on lat-6.
