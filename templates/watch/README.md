# watch

Pages and prices, checked every hour: when the part you picked changes,
the page shows what it was, and everyone who asked gets a push.

- A watch is a URL and, optionally, what to watch on it: a CSS selector
  (`.price`) on an HTML page, or a path (`data.price`) in a JSON answer;
  without one, the page's text. Editors add them on the page (`add`,
  then a first `check`).
- `fragment.json` declares a cron trigger, `{"cron": "0 * * * *", "run":
  "sweep"}`: every hour `sweep` starts one `check` run per watch.
- `check` is a job: `job.fetch` gets the page (a redirect is answered,
  not followed, so it follows one itself, as a step), and `seen`, a
  mutation, keeps the value. A new value publishes a record to `changes`
  and pushes to every browser subscribed as `changes` ("Notify me").
- A page that does not answer (a 5xx, a timeout) is retried with
  backoff; one that still fails holds its run, and the card says which:
  `fragment runs <name> --status held`, then `fragment replay <name>
  <run>` once it is back (the next hourly check runs anyway). A page
  that answers without what the watch picks says so on `problems`, and
  its run succeeds: checking again would not help.
- A page behind a key: store it (`fragment secret set <name> SHOP_KEY`)
  and give the watch a header, `Authorization: Bearer {{SHOP_KEY}}`. The
  platform fills it in on the way out; the app and the page never hold
  it, and it goes only to the watch's own site.

From the CLI: `fragment call <name> add --input
'{"url":"https://example.com","pick":"h1"}'`, `fragment call <name>
check --input '{"id":1}'`, `fragment channel <name> changes --follow`.
