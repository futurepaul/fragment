# split

Shared costs for a trip or a house: snap a receipt (or type it in), and
everyone sees who owes whom, live.

- People signed in `join` the split, by a name of their choosing; a
  visitor with no session reads it and changes nothing. Each row is a
  person's: `add` names who paid (the caller unless `paidBy` says) and
  shares the cost equally among `among` (everyone unless named), a cent
  left over going to the first; `settle` is a payment the caller made to
  someone; `remove` is for who paid or added it, or an editor.
- `ledger` is the page's live query: what each has paid and owes, summed
  in SQL; the newest 200 expenses; and the payments that settle it all,
  the one who owes most paying the one owed most first. Money is whole
  cents in the split's currency (`about`, an editor's: `EUR`, `USD`, …).
- `scan` is a job: the page shrinks the photo to a JPEG of at most 240 000
  characters of base64 (an operation's input is at most 256 KiB), and one
  text step on the cheap tier, whose model reads images, answers the
  receipt as JSON. What it reads is added as the snapper's expense,
  shared among everyone, its lines kept beside it; what the scan says
  goes to them on `scans` (`{for, text, expense}`). The photo itself is
  not kept. The owner pays for the model calls, from their monthly
  budget; a scan by someone not in the split stops before it calls one.

At most 50 people and 5000 expenses a split; a split's people are those
who joined it, not its members, so a link holder who signs in can join.
For a split only its members see, make it `members`.

From the CLI: `fragment call <name> join --input '{"name":"Sam"}'`,
`fragment call <name> add --input '{"what":"Dinner","cents":6000}'`,
`fragment call <name> ledger`.
