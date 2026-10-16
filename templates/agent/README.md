# agent

The blessed agent template (docs/cloudflare-v1.md, decisions 14–16 and
40). An agent is a fragment: its repo holds its job (`SOUL.md`), its
settings (`agent.json`: `{"tier", "model"?}`, `model` a model of a
provider its owner connected, `{provider, id}`: docs/computers.md, "An
agent's own model"), its picture (`avatar.png`: a PNG, JPEG, WebP or GIF
of at most 256 KiB), and what its runtime keeps
(`memories/`, `skills/`); the platform serves this template's manifest and
page from its release. A fragment on it names it in its own
`fragment.json`: `{"template": "agent", "meta": {"title": "Juniper"}}`.

Its name is that title, and its picture that `avatar.png`, as live has
them: every page and the platform show the agent by them (docs/api.md,
"An agent's name and picture"). Its owner changes them in Settings, and
the agent with `fragment agent --name <name> --picture <file>`; either
writes them and puts them live. A runtime's own files (Hermes'
`profile.yaml`) are written from them, never the other way. Its colour
is chosen by its identity (`site/agent-color.js`, which the chat
template's page and the shell link to), never stored.

The `tasks` channel is what its computer follows besides its chats
(docs/chat-records.md): routines, and the platform's `joined` record when
the agent is added to a fragment.
