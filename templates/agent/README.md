# agent

The blessed agent template (docs/cloudflare-v1.md, decisions 14–16 and
40). An agent is a fragment: its repo holds its job (`SOUL.md`), its
settings (`agent.json`: `{"tier", "color", "model"?}`, `model` a model of a
provider its owner connected, `{provider, id}`: docs/computers.md, "An
agent's own model"), and what its runtime keeps
(`memories/`, `skills/`); the platform serves this template's manifest and
page from its release. A fragment on it names it in its own
`fragment.json`: `{"template": "agent", "meta": {"title": "Juniper"}}`.

The `tasks` channel is what its computer follows besides its chats
(docs/chat-records.md): routines, and the platform's `joined` record when
the agent is added to a fragment.
