# hermes

Your own Hermes (github.com/NousResearch/hermes-agent), and a page to
chat with it. `"computer": {"preset": "hermes"}` in `fragment.json` has
the platform make it on the fleet's sandcastle node the first time you
deploy; its steps are in `fragment events`. Its model calls come out of
your budget.

The page talks to Hermes directly, by its key: Hermes has no public
address. The page makes a key of its own, asks the platform for an
admission for it (`POST ./__hermes/access`, which only you and the
fragment's editors get), and connects to Hermes' computer through its
relay with the platform's computer client (`/__computer/client.js`).
Then it reads its chats over REST and chats over its socket:

- `site/hermes.js`: the client. An admission renewed before it ends,
  `sessions()`, `messages(id)`, and `gateway()` (one socket:
  `session.create`, `session.resume`, `prompt.submit`,
  `session.interrupt`, and the `message.*` and `tool.*` events it
  answers with). It has no fragment in it but the admission's default
  address; lift it anywhere a page can get an admission for a Hermes.
- `site/chat.js`: the chat, on top of it. While a turn runs it pings
  every 15 s, so the node does not put Hermes to sleep mid-reply.

Drop the `computer` block and deploy to remove it: its computer and its
chats go (the node's backups of it stay). Declaring it again makes a new
one.
