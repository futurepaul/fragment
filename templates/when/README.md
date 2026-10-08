# when

Find a time, or run a quick poll: send the link, and everyone who opens
it votes, with no account. The tally moves on every open page as people
vote.

- `setup` (an editor's) asks the question and lists the options: times
  ("Find a time", each marked "works" or "if need be") or a quick poll
  (one pick each). An option whose label stays keeps its votes.
- `vote` is `public`: anyone who can open the fragment calls it, signed
  in or not. A visitor with no session is an anonymous principal of their
  own (a cookie on this origin), so their ballot is theirs, and the
  platform holds them to 60 calls a minute. A ballot is the voter's whole
  answer and their name; a vote replaces the one before.
- `close` stops the voting, with the option it settled on.
- `poll` is the page's live query: each ballot by its voter's name, the
  caller's marked `you`, never by principal.
- `site/index.html` re-runs `poll` live and shows who is here now
  (`presence`, by the name each gave).

A link fragment (the default) lets whoever holds the link vote; make it
`public` for anyone. A visitor who clears their cookies is a new voter:
for a vote that counts people, take ballots from people signed in.

From the CLI: `fragment call <name> setup --input '{"question":"Dinner?",
"mode":"choice","options":["Thai","Pizza"]}'`, `fragment call <name>
poll`.
