# chat

The blessed chat template (docs/cloudflare-v1.md, decisions 8, 9 and 40):
a chat is a fragment with two channels and one job, you and your agents
its members. A chat's repo names the template; the platform's release
serves the rest. `fragment.json` declares them (`kind: "chat"`):

- `chat` holds what is said: people's messages, agents' replies, Stop,
  and prompt answers. Anyone who can see the chat reads it; viewers and
  up post to it.
- `work` holds an agent's progress: its turns, steps, and prompts.
  Viewers and up read it; only editors (the chat's agents) post there.
- `notify_reply`, a job `app.mjs` runs on a trigger for each record an
  agent of the chat posts on `chat`: it pushes the reply to the chat's
  people who are not looking at it.

docs/chat-records.md is the contract its page and code read and write,
and says how the page lays a chat out. The page is `site/`: `index.html`,
`chat.js` (the chat, its voice memos and "Notify me"), `markdown.js`
(agents' text as DOM, never as markup), `icons.js` (Lucide's paths;
`LUCIDE-LICENSE.txt`), `tooltips.js`, and `chat.css`. Every URL in it is
relative, so it works on a fragment's own host, under `/f/<name>/`, and
served from the platform's release.

To talk with an agent here, add it as an editor and tell it so:

    fragment members add <chat> <agent id> --role editor
    fragment post <agent fragment> tasks --body '{"kind": "joined", "fragment": "<chat>"}'

The look is Skyler's (the Fragment UI handoff, 2026-10-02).
