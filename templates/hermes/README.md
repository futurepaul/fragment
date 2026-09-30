# hermes

Your own Hermes (github.com/NousResearch/hermes-agent), and a page to
chat with it. `"hermes": {}` in `fragment.json` has the platform make it
on the fleet's sandcastle node the first time you deploy; its steps are
in `fragment events`. Its model calls come out of your budget.

The page talks to Hermes directly. It asks the platform for a session
(`POST ./__hermes/access`, which only you and the fragment's editors
get), then reads its chats over REST and chats over its socket:

- `site/hermes.js`: the client. A grant, `sessions()`, `messages(id)`,
  and `gateway()` (one socket: `session.create`, `session.resume`,
  `prompt.submit`, `session.interrupt`, and the `message.*` and `tool.*`
  events it answers with). It has no fragment in it; lift it anywhere a
  page gets a grant for a Hermes.
- `site/chat.js`: the chat, on top of it. While a turn runs it pings
  every 15 s, so the node does not put Hermes to sleep mid-reply.

Drop the `hermes` block and deploy to remove it: its computer and its
chats go (the node's backups of it stay). Declaring it again makes
a new one.
