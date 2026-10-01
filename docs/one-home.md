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

1. **The chat for anyone.** The platform's chat page (`cell/chat.mjs`)
   shows each author's face (a person's picture; an agent's or a
   computer's own mark beside its owner's), who is here (presence), and
   a reply as it streams (`draft`). `__fragments` says what each
   fragment is from its live manifest (a chat and who answers it, a
   computer and its preset, an app), and the desktop reads that in place
   of name prefixes.
   - Checks (e2e, Chrome): two people in a chat see each other's
     messages with names and faces, and each other's presence; a draft
     shows as it grows and gives way to the final record; the desktop
     lists a chat, a computer, and an app by their manifests.
2. **Hermes answers a chat.** A chat names its owner's Hermes as who
   answers. The Hermes cell is the Relay connector: Hermes' gateway
   dials it, authenticated by its secret; the cell hands it the chat's
   new records and turns its sends, edits, and tool progress into
   `draft`, `work`, and `chat` records, posted as the Hermes' computer
   identity. While Hermes is away, the cell keeps the records and pokes
   its computer's wake URL; Hermes takes them on reconnect, in order,
   once. The sandcastle fake's Hermes speaks Relay.
   - Checks (e2e, fakes): two people's messages reach Hermes with their
     names; both see the reply stream and land; a message mid-turn
     waits; someone who cannot chat is not heard; an asleep Hermes is
     woken and gets each message once, across a node restart; a removed
     Hermes is refused (4401).
3. **The Hermes image.** Derived from Hermes' `-desktop` image: its
   screen and computer-use tools, the loopback bridge as a service, and
   the defaults decisions 2 and 4 need. sandcastle gains a wake URL per
   computer: an unauthenticated poke that only wakes it. Publishing the
   image is Paul's to approve.
   - Checks: the sandcastle e2e on lat-6 runs a chat turn through the
     relay from a sleeping Hermes, and Hermes drives a browser on its
     own screen.
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
- Whether a cell can hold Hermes' Relay socket while it hibernates.
