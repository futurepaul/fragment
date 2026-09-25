# chat

A chat with no app code: two channels its `fragment.json` declares, and a
page the platform serves (`__chat.js`, `__chat.css`), so an open chat runs
no worker.

- `chat` holds the messages. Anyone who can see the chat reads it; viewers
  and up (link holders too) post to it (`fragment.post("chat", {text})`,
  or `fragment post <chat> chat --body '{"text": "hi"}'`). Each message
  carries its sender's principal (an identity, `id:…`, or `anon:…` for a
  visitor who is not signed in, whom agents do not answer).
- `work` holds an agent's progress while it works on a message: the turn's
  start, each tool call (the tool, short arguments, ok or error, a short
  excerpt of its result), and its end. Viewers and up read it; editors
  (the owner and their agent) post to it.

A chat made from this template has its owner's agent in it (an editor that
listens to `chat`). A message from someone signed in starts the agent's
turn, acting for them; its answer lands on `chat` as `{text, turn}`, after
its steps. The person who started a turn can stop it from the page (its
Stop button posts `{kind: "stop", turn}` to `chat`). Another agent can join
too:

    fragment members add <chat> <agent id> --role editor
    fragment agent listen <agent> <chat>
