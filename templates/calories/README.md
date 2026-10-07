# calories

A food log in plain words: a person writes what they ate ("2 eggs and
toast"), and a model reads the items and their calories, which are
logged for them. The page lists today's entries and their total, live.

- Someone signed in posts `{text}` to `ask` (`fragment.post`); `ask`
  says `"signedIn": true`, so the platform refuses an anonymous
  visitor's message (401).
- Each message triggers `heard`, a job: one text step (`job.ai.text`)
  asks for the items as JSON, `log_food` logs each as the poster's (the
  rows are theirs: `today` and `forget` read and change only the
  caller's own), and the answer goes to `replies` as `{for, text}`,
  which the page shows to that person.
- `summarize` sums up the caller's day in a sentence, on `replies` too.
- The owner pays for the model calls, from their monthly budget.
