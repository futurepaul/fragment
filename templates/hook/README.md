# hook

A live board of what your webhooks say: CI runs, deploys, payments.
Each sender is a tile, red while it fails; the latest events and the
last two weeks follow below, and every open page moves as deliveries
land.

- Point any webhook at the fragment's inbox: `fragment open <name>`
  prints its URL, token included (GitHub: Settings, Webhooks, content
  type `application/json`, the events Workflow runs and Deployment
  statuses; Stripe: Developers, Webhooks). `fragment rotate <name>
  --inbox` makes a new token.
- `fragment.json` declares a trigger, `{"channel": "inbox", "run":
  "heard"}`: each delivery runs `heard`, a mutation, with the record as
  its input. It reads GitHub's `workflow_run` and `deployment_status`,
  Stripe's events, and anything else as `{"source", "payload": {"name",
  "status", "detail", "url", "id"}}`; a body of no known shape is kept as
  a message, so no delivery is held for its shape.
- An event with an id (a run, a Stripe event, a payload's `id`) is kept
  once: a later status of the same run updates it, and a sender's
  redelivery changes nothing.
- A tile that starts failing, or recovers, pushes to every browser
  subscribed as `alerts` ("Notify me of failures").
- `board` is a `public` query: who sees the board is the fragment's
  visibility (a link by default; `fragment visibility <name> public`
  makes it a status page).

Try it: `fragment inbox <name> --token <inbox token> --source deploys
--payload '{"name":"web","status":"failed","detail":"v1.2"}'`.
