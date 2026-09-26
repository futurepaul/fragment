# calories

A food log with an agent: a person writes what they ate in plain words
("2 eggs and toast"), and the fragment's own agent logs each item for
them. The page lists today's entries and their total, live.

- `fragment.json`'s `agent` block declares the agent: its instructions
  (`agent.md`), the operations it may call (`log_food`, `today`; `forget`
  is the page's alone), and the channel whose messages start its turns
  (`ask`). Deploying makes it (an editor here, and nowhere else, owned by
  the fragment's owner); a deploy without the block removes it.
- Someone signed in posts `{text}` to `ask` (`fragment.post`); the agent
  answers there as `{text, turn}`, and its steps go to `work` (as a chat's
  do). Each person has a conversation of their own with it, and each of
  its calls acts for them (`call.principal` is them), so the rows it
  logs are theirs. An anonymous visitor's message starts nothing.
- The owner pays for its model calls, from their monthly budget.
