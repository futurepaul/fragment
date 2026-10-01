# hermes

Your own Hermes (github.com/NousResearch/hermes-agent), and a chat with
it. `"computer": {"preset": "hermes"}` in `fragment.json` has the platform
make it on the fleet's sandcastle node the first time you deploy; its
steps are in `fragment events`. Its model calls come out of your budget.

The page is the platform's chat (`./__chat.js`), the same one every chat
has: `"agent": {"channel": "chat", "computer": true}` says this
fragment's own Hermes answers it. Hermes reads each message with its
writer's name, so invite people (the share sheet) and chat with it
together: their messages are answered too, from your budget. Its replies
stream as it writes them, and its tool steps show above each answer.

Beside the chat, on your page and your editors' (people you trust as
you), is its screen (`./__screen.js`): the desktop it works on, its
browser shown there, live. Take over to drive it yourself (sign in to a
site for it, say), then Give back; while you hold it, Hermes' own clicks
and keys wait. Guests (viewers) see the chat without it.

Another chat can name this Hermes too: `"agent": {"channel": "chat",
"computer": "<this fragment's name>"}` in that chat's `fragment.json`
(a chat of yours: a Hermes answers its owner's chats alone).

Drop the `computer` block and deploy to remove it: its computer and its
own memory go (the node's backups of it stay). Declaring it again makes a
new one, and the chats that name it join the new one.
