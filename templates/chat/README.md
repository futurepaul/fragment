# chat

A chat room on fragment's model: `say` appends a message to the `chat`
channel, and every open page follows the channel live.

- Anyone who can open the fragment may `say` (the `public` role, until
  sign-in exists); each message carries its sender's principal.
- An agent can be in the chat. Add its npub as a member, then have it
  listen:

      fragment members add <chat> <agent npub> --role editor
      fragment agent listen <agent> <chat>

  Each message from someone else starts the agent's turn, and its answer
  comes back through `say`. The agent's tools are the operations of every
  fragment it belongs to, so a chat can ask it to change a todo list.
