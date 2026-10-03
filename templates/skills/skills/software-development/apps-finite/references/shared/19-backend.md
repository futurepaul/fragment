# Backends On Fragment

A fragment's backend is its app: named operations over its own SQLite,
declared in `fragment.json` and implemented in `app.mjs`. There is no
server to run, no port, no `DATA_DIR`: the platform runs the operations
and keeps the database. `fragment guide` has the whole contract; this is
the shape to reach for.

## Preferred Shape

```json
{
  "operations": {
    "add":    { "kind": "mutation", "role": "editor",
                "input": { "type": "object", "required": ["text"], "additionalProperties": false,
                           "properties": { "text": { "type": "string", "maxLength": 200 } } } },
    "list":   { "kind": "query", "role": "public" },
    "ingest": { "kind": "job" }
  },
  "channels": { "activity": { "read": "viewer" } },
  "triggers": [{ "channel": "inbox", "run": "ingest" }]
}
```

```js
// app.mjs
import { DurableObject } from "cloudflare:workers";

export class App extends DurableObject {
  constructor(ctx, env) {
    super(ctx, env);
    ctx.storage.sql.exec("CREATE TABLE IF NOT EXISTS items (id INTEGER PRIMARY KEY, text TEXT NOT NULL)");
  }
  add({ text }, call) {
    const { id } = this.ctx.storage.sql.exec("INSERT INTO items (text) VALUES (?) RETURNING id", text).one();
    call.publish("activity", { id, text, by: call.principal });
    return { id };
  }
  list() {
    return { items: this.ctx.storage.sql.exec("SELECT id, text FROM items ORDER BY id").toArray() };
  }
  async ingest({ record }, job) {
    const page = await job.fetch(record.body.payload.url);
    return await job.call("add", { text: `${page.status} ${record.body.payload.url}` });
  }
}
```

- A **query** reads; a **mutation** changes the database, all or nothing,
  once per id; a **job** runs durable steps (`job.fetch`, `job.ai.*`,
  `job.sleep`, `job.call`, `job.files.*`) outside any request.
- `role` says who may call it: `public`, `viewer`, `editor`, `owner`.
  `input` is a JSON Schema checked before your code runs.
- **Channels** are append-only feeds every open page follows live:
  publish from a mutation (`call.publish`), or let people post to one with
  a `post` role.
- **Triggers** run an operation on a cron, a record on a channel (every
  fragment has an `inbox` for webhooks), or changed files.
- A `fetch(request)` method on the App answers every site path that is not
  a file in `site/`: custom routes and server-rendered pages.

## Local Development

Write the operations, deploy, and call them:

```sh
fragment deploy garden --dir .
fragment call garden add --input '{"text":"hello"}'
fragment call garden list
fragment channel garden activity --follow
fragment runs garden --status held          # a job that threw is held, its input kept
fragment replay garden <run>                # after fixing the code
```

## Persistence

- The database is the fragment's: it survives deploys, rollbacks and
  sleeps. Migrate in the constructor with `CREATE TABLE IF NOT EXISTS` and
  additive `ALTER TABLE`s; never drop data on start.
- Files are the other state: `call.files.write` in a mutation,
  `job.files.*` in a job, `this.files.read` anywhere async, at `main`.
- An app's database has a cap (16 MiB with its ledger of mutation ids): a
  write past it is rolled back (`storage_full`).

## Secrets

- `fragment secret set garden API_KEY` (the value from the human, or from
  stdin), then `{{API_KEY}}` in a `job.fetch` header: the platform fills it
  outside your code. Never write a secret into a file or a page.

## LLM And Media Features

Read `20-llm-api.md`: AI steps run as jobs, billed to the owner.

## Validation

Before you share it:

- call every operation you changed, valid and invalid input both;
- replay a mutation with the same id and see the same answer;
- open the live page and use it end to end;
- deploy again and check the data is still there;
- read `fragment events garden --tail 30` for refusals and held runs.
