# board

A family's chores or a small team's tasks: cards in To do, Doing and
Done, each for someone. Every open page follows the board live and
shows who is here; giving someone a card pushes it to their phone.

- `add`, `move`, `assign` and `remove` are mutations a viewer may call
  (members, and whoever holds the link), and each takes a person signed
  in: a card names who made it, so a visitor with no session reads the
  board and changes nothing ("sign in to change the board").
- `assign` gives a card to a person (`id:…`) or to no one. Given to
  someone else, it pushes `{title, body, tag, url}` to them with
  `call.push(<their id>, …)`, sent once the mutation commits (a replay
  pushes nothing again). A push to an identity reaches only the browsers
  that person subscribed: the page's "Notify me" calls
  `fragment.push.register(<their id>)`, which the platform takes from
  them alone.
- `board` is the page's live query: the open cards in order (at most
  500; a new one past that is refused), then the newest 50 done. The
  page offers a card to the fragment's people (`__members`), names them
  through `__people`, and shows who is here (`presence`) by face.

For a board only its members see, make it `members` (`fragment
visibility <name> members`) and add each person (`fragment members add
<name> <id> --role viewer`).

From the CLI: `fragment call <name> add --input '{"title":"Bins out"}'`,
`fragment call <name> board`.
