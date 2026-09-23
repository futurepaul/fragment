# inbox

A webhook inbox on fragment's job model: every delivery to the
fragment's inbox runs `ingest`, a durable job, and the page shows the
result live.

- `fragment.json` declares a trigger, `{"channel": "inbox", "run":
  "ingest"}`: each record the inbox receives starts a run of `ingest`
  (as the fragment itself), with the record as its input.
- `ingest` is a `job`: each `await job.fetch(…)` and `await job.call(…)`
  is a durable step. A fetch that fails for a reason that may pass (a
  5xx, a timeout) is retried with backoff; a run that still fails is
  held, kept with its input: `fragment runs <name> --status held`, then
  `fragment replay <name> <run>` once the cause is fixed.
- `add` is the mutation the job calls; it publishes each line to `feed`.
- `site/index.html` re-runs `list` live as lines arrive.

Post to it: `fragment inbox <name> --token <inbox token> --payload
'{"text":"hello"}'` (the token is in `fragment status <name>`), or any
webhook sender: `POST /api/f/<name>/inbox` with the token in
`x-fragment-inbox-token` and a JSON body (`{"source", "payload"}`, or
any JSON as the payload).

A job reaches the network only through `job.fetch`; to call an API with
a key, store it (`fragment secret set <name> API_KEY`) and write
`headers: { authorization: "Bearer {{API_KEY}}" }`: the platform adds
it on the way out, and the app never holds it.
