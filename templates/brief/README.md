# brief

Your feeds, read and summed up each morning: a model writes a few lines
on what is new, the items follow, and a push says it is ready. Every
brief stays on the page, newest first.

- Editors add feeds on the page (`add_feed`, then `probe`, a job that
  reads it once for its title). A site's page that names its feed
  (`<link rel="alternate" type="application/rss+xml">`) is swapped for
  that feed. RSS and Atom both read.
- `fragment.json` declares a cron trigger, `{"cron": "0 * * * *", "run":
  "brief"}`. A cron is in UTC, and a morning is not, so `brief` runs
  every hour and goes on only at the brief's hour in its time zone
  (`set_time`; the page sets the first editor's own, at 7).
- `brief` is a job: `job.fetch` reads each feed (its redirects followed
  as steps of their own), keeps the items newer than the last brief, and
  `job.ai.text` sums them up. The owner pays for that step, from their
  monthly budget; a brief with nothing new calls no model. A feed that
  does not answer is retried, then named in the brief, which comes
  anyway.
- The brief is a record on `briefs`, the archive: the page follows it
  live (`fragment.subscribe`), and `job.push` tells every browser
  subscribed as `briefs` ("Notify me").
- "Brief me now" calls the same job (`fragment call <name> brief`).

From the CLI: `fragment call <name> add_feed --input
'{"url":"https://example.com/feed.xml"}'`, `fragment channel <name>
briefs`.
