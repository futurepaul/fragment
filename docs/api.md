# fragment wire contract

The cell (`cell/`, Rust on celld) answers everything below; the CLI and
the browser library are its clients. Errors are `{"error": "<code>",
"message": "..."}` (codes in `crates/proto`). `cargo xtask e2e` proves
every route. The TypeScript runtime this replaced was deleted in phase 2
slice G (its contract is in git history, last at `35f5e18`).

## Configuration

Worker variables, rendered from the fleet's settings (ROADMAP decision
13; `cell/.dev.vars` in dev):

| Variable | Meaning |
|---|---|
| `FRAGMENT_HOST_SECRET` | seals secrets at rest (at least 32 bytes); required to create |
| `FRAGMENT_HOST_SECRET_PREVIOUS` | the secret before a rotation; values sealed under it still open |
| `CODESTORAGE_ORG`, `CODESTORAGE_PRIVATE_KEY` | the org and its PKCS#8 P-256 key (a one-line PEM may carry literal `\n`) |
| `CODESTORAGE_API_URL` | the API base (default `https://api.<org>.code.storage`) |
| `FRAGMENT_HOST_SUFFIX` | fragments are served from `<name>.<suffix>`; unset, from `/f/<name>/` |
| `FRAGMENT_POLL_INTERVAL_S` | the webhook backstop (default 300); also how often running runs are checked against their Workflows |
| `FRAGMENT_JOB_RETRY_DELAY_S` | a failed job step's first retry delay, doubling over 4 retries (default 10) |
| `FRAGMENT_EGRESS_LOCAL` | `allow` lets jobs fetch loopback and private addresses (dev and e2e fakes); never on a shared fleet |
| `FRAGMENT_BLOB_GRACE_S` | how long a blob no branch names is kept before it is deleted (default 7 days) |
| `FRAGMENT_PUSH_SUBJECT` | who push services may contact about this fleet's pushes (a `mailto:` or https URL; RFC 8292) |
| `FRAGMENT_DELIVERY_RETRY_S` | the shortest wait before a delivery is retried (default 10; the wait grows with the delivery's age, up to an hour) |
| `OPENROUTER_API_URL` | where AI calls go (default https://openrouter.ai) |
| `FRAGMENT_DEPLOY_ID` | which deployment this is (`cargo xtask deploy` sets it); `GET /healthz` answers it in `x-fragment-deploy` |
| `FRAGMENT_CREATORS` | until sign-in exists, the identities (`id:…`) and keys (npubs or hex), comma-separated, that may create fragments; unset, anyone registered; a list that does not parse lets nobody create (500) |
| `FRAGMENT_TEST_HOOKS` | `allow` on dev and e2e fleets only: `POST /api/test/registry {down}` makes the registry answer 503 |

Bindings (`cell/wrangler.jsonc`): `FRAGMENT` and `PRINCIPAL` (Durable
Objects), `LOADER` (the Worker Loader), `JOBS` (the Workflow that runs
jobs), `BLOBS` (R2 over the fleet bucket: the bytes of large files),
`DELIVERIES` (the `fragment-deliveries` queue, and its dead-letter queue
`fragment-deliveries-dead`, both consumed by the cell).

## Principals and access

A principal is an identity (`id:` + 32 hex: a person or an agent) or an
anonymous visitor (`anon:` + 32 hex, the hash of a random cookie on the
fragment's origin). Grants, records, runs, and the ledger name
principals. A request is signed by a key (64 hex inside, an npub in
answers; requests may use either); the router asks the registry which
identity holds it, live, on every signed request, and passes the fragment
both. A key no one registered, or a revoked one, is 401; when the
registry cannot answer, a signed request is 503 `registry_unavailable`,
never let through (docs/finite-integration.md, rule 7). Control routes
need NIP-98; site requests may carry it. A fragment's own key is the
principal of its triggered runs and is not registered.

NIP-98 (`crates/nip98`): `Authorization: Nostr <base64 of the event>`, an
event of kind 27235 with empty content and the tags `["u", <the absolute
request URL>]`, `["method", <the method>]`, and, when the body is not
empty, `["payload", <hex SHA-256 of the body>]`; `created_at` within 60
seconds of the cell's clock; `id` and a BIP-340 `sig` as NIP-01 defines.
A blob upload is signed without `payload`: its URL names the bytes' hash,
which the cell checks as they arrive.

Roles, weakest first: `public`, `viewer`, `editor`, `owner`. A member's
role is their membership. Otherwise visibility decides: on a `public`
fragment everyone holds `public`; on a `public` or `link` fragment the
share link (`?view=<token>`, which sets an HttpOnly cookie on the
fragment's origin) counts as `viewer`; a `members` fragment gives
non-members nothing. A refusal is 401 for an unsigned caller and 403 for
a signed one.

An agent's owner reads what the agent reads (FIN-11): someone who is not
a member but owns an agent that is reads the fragment as a `viewer`
(status, files, channels, events, runs, its queries, its site), and never
acts through it: a mutation or a job needs a membership of their own
(403, saying so), and they get nothing the agent's own role opens beyond
`viewer` (an editor's channels, tokens, or secrets).

Membership is cell state: `fragment.json`'s `visibility`, `editors`, and
`viewers` grant nothing (the cell records a `manifest.ignored` event).
Only the owner manages members, invites, visibility, and tokens; a
member may leave. Each identity's list of fragments is kept in its
`Principal` cell, fed from each fragment's outbox.

## Identities (phase 4 slice A)

The registry (`cell/src/registry.rs`; finite.computer's BANKS stands
behind the same routes later) holds identities, the public keys each has
held, each agent's owner, and, from slice B, sign-in subjects. It holds
no grant and no private key. A **key proof** is a NIP-98 event by a new
key for the same method and URL as the request that carries it, with
`["p", <the signing key, 64 hex>]`: whoever sent the request also holds
the new key and meant it for this signer.

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/identities` | the signing key | `{kind: "person"}` → the identity: until sign-in (slice B), a key registers as a new person holding it; again, the same one (`created: false`); a revoked key is 401 |
| `POST /api/identities` | a person | `{kind: "agent", proof}` → a new agent identity they own, holding the proof's key (FIN-11's trusted initial registration); again, the same one; a key someone else holds is 409; an agent owns no agents (403) |
| `GET /api/identities/{id\|me}` | the identity, or its owner | → `{id, kind, owner?, createdAt, keys: [{npub, addedAt, addedBy, revokedAt?}], agents: [id]}`; anyone else 404 |
| `POST /api/identities/{id\|me}/keys` | a person for themselves; an owner for their agent | `{proof}` → the identity with the key added (at most 64 keys, revoked ones included); a key someone else holds, or a revoked one, is 409 |
| `DELETE /api/identities/{id\|me}/keys/{npub}` | the same | → the identity; the key is 401 from the next request and never comes back; the last active key cannot be revoked (400) |
| `GET /api/identities/{id}/keys/{npub}` | the identity, or an agent it owns | → `{active}` (an agent's runtime checks its owner's keys with it) |

## Control API

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/fragments` | any registered signer (on a fleet with `FRAGMENT_CREATORS`, a listed identity or key; others 403) | `{name, fragmentSecret, visibility?}` → `{name, npub, owner, visibility, viewToken, inboxToken, webhookSecret, repo, canonical}`. `fragmentSecret` is the fragment's own key, made by the client; it is stored sealed. The cell creates (or, for a name deleted before, finds) the code.storage repo. |
| `GET /api/fragments` | any signer | → `{fragments: [{name, role}]}` |
| `DELETE /api/f/{name}` | owner | → `{ok, deleted}`; the app's database goes too; the repo stays |
| `GET /api/f/{name}/status` | viewer | → `{name, npub, owner, role, visibility, repo, pins: {main, live}, counts: {files, events, members}, code: {sha, operations, error}, viewToken, inboxToken (editor), urls: {canonical}, blobMinBytes}` |
| `GET /api/f/{name}/manifest` | viewer | → `fragment.json` at main (404 when there is none) |
| `GET /api/f/{name}/members` | viewer | → `{members: [{principal, role, addedBy, addedAt, kind, owner?}]}` (`owner`: an agent member's) |
| `PUT /api/f/{name}/members/{id\|npub}` | owner | `{role: viewer\|editor}` → the member; a key names the identity holding it (404 when no one registered it) |
| `DELETE /api/f/{name}/members/{id\|npub\|me}` | owner, or the member | → `{ok, removed}`; closes that member's change feeds (and its owner's, when an agent's membership was their only view) |
| `POST /api/f/{name}/invites` | owner | `{role, uses? (1), ttlS? (7 days, at most 30)}` → `{id, role, usesLeft, expiresAt, createdBy, token}`; the token is shown once |
| `GET /api/f/{name}/invites` | owner | → `{invites: [...]}` without tokens |
| `DELETE /api/f/{name}/invites/{id}` | owner | → `{ok, revoked}` |
| `POST /api/f/{name}/join` | any signer | `{token}` → `{name, role, joined}`; a stronger existing role is kept |
| `PUT /api/f/{name}/visibility` | owner | `{visibility}` → `{ok, visibility}` |
| `POST /api/f/{name}/rotate` | owner | `{scopes?: [inbox, view, webhook]}` → `{ok, inbox_token, view_token, webhook_secret, rotated}`; a new view token closes link holders' feeds |
| `PUT /api/f/{name}/secrets/{KEY}` | editor | raw body (at most 64 KiB) → `{ok, name}`; sealed (AES-256-GCM, key HKDF'd from the host secret and the fragment's npub) |
| `GET /api/f/{name}/secrets` | editor | → `{names}`; values never leave |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor | → `{ok, removed}` |
| `GET /api/f/{name}/storage-token` | editor | → `{token, repo, api, expiresAt}`: ES256, this repo, `git:read`+`git:write`, 15 minutes |
| `POST /api/f/{name}/refresh` | editor | → `{ok, refs: {main: {pin, moved} \| {absent}, live: ...}}` |
| `POST /api/f/{name}/webhook` | code.storage | signed with the fragment's webhook secret (`X-Pierre-Signature`, 5 minutes); validate, remember (redeliveries are acknowledged), then move the pin to the branch's head as read now |
| `GET /api/f/{name}/files` | viewer | → `{ref, files: [{path, size, mode, lastCommitSha, machinery, blob?}]}` at main; a pointer's `size` is its bytes' |
| `GET /api/f/{name}/file?path=` | viewer | → the bytes at main (`x-fragment-ref`); a pointer's come from the blob store |
| `PUT /api/f/{name}/blobs/{sha256}` | editor | the bytes as the body (`content-length` required, at most 256 MiB), streamed through and hashed on the way in: → `{ok, sha, size, stored}`; bytes that hash to anything else are deleted and refused (400) |
| `GET`, `HEAD /api/f/{name}/blobs/{sha256}` | viewer | → the bytes (ranges answer 206) |
| `GET /api/f/{name}/file/stat?path=` | viewer | → `{stat: {path, size, blobSha, lastCommitSha, present}, ref}` |
| `GET /api/f/{name}/events?since=` | viewer | → `{events: [{id, at, kind, summary, data}]}` (500 a page; 5000 kept) |
| `POST /api/f/{name}/ops/{op}` | the operation's role | `{id, input}` → `{result, replayed}`; for a job, `result` is `{run, status}` (the same id answers the same run) |
| `GET /api/f/{name}/runs?status=&op=&limit=` | viewer | → `{runs: [{id, op, via, trigger, principal, status, attempt, depth, createdAt, finishedAt, error}], counts: {<status>: n}, paused}` newest first (30, at most 200) |
| `GET /api/f/{name}/runs/{id}` | viewer | → one run with its `input` and `output` |
| `POST /api/f/{name}/replay` | editor | `{run}` → `{ok, run, attempt}`: a `held` or `blocked` run again, as its next attempt, with its input |
| `POST /api/f/{name}/pause` | editor | `{op, paused}` → `{ok, op, paused}`: the operation's triggers stop or start; calls are never paused |
| `GET /api/f/{name}/triggers` | viewer | → `{triggers: [{cron\|channel\|files, run, paused, nextAt?}], paused}` |
| `POST /api/f/{name}/inbox` | the inbox token | token in `x-fragment-inbox-token` or `?t=`; a JSON body `{source?, payload}` (any other JSON, or text, is the payload), at most 64 KiB → `{ok, seq, runs}`. Bad token 403; 1000 pending, 429 |

| `POST /api/f/{name}/subscriptions` | a member who may read the channel | `{channel, url}` → `{id, channel, url}`: each new record of the channel is POSTed to `url` through the delivery queue as `{type: "record", fragment, channel, record}` (unsigned: the URL is the subscriber's capability; egress-checked; at most 32 a fragment; a 404 or 410 drops it; a removed member's go with it) |
| `GET /api/f/{name}/subscriptions` | a member (the owner sees all) | → `{subscriptions: [{id, principal, channel, url, createdAt}]}` |
| `DELETE /api/f/{name}/subscriptions/{id}` | its subscriber, or the owner | → `{ok, removed}` |
| `GET /api/f/{name}/channels` | viewer | → `{channels: [{name, read, seq}]}`: `events`, `ops`, `inbox`, and the app's |
| `GET /api/f/{name}/channels/{channel}?after=&limit=` | the channel's reader | → `{channel, records: [{channel, seq, at, principal, kind, body}], next}` (1000 a page) |

## Apps

Code comes from git: when `live` moves, the cell reads `fragment.json`
and `app.mjs` (with `applib/**.mjs|js`, at most 64 modules and 4 MiB in
all) from the live commit. A live commit with an invalid `fragment.json`
keeps the last good code and says why in `status.code.error`.

```json
{
  "operations": {
    "add":  { "kind": "mutation", "role": "editor", "input": { "type": "object", "required": ["text"],
              "properties": { "text": { "type": "string", "maxLength": 200 } }, "additionalProperties": false } },
    "list": { "kind": "query" }
  },
  "channels": { "activity": { "read": "viewer" } },
  "meta": { "title": "Todo", "description": "…", "image": "https://…" }
}
```

- `role` defaults to `viewer` for a query and `editor` for a mutation.
  `fetch` and `alarm` are not operation names.
- `input` is a JSON Schema in a bounded subset (`crates/core/src/schema.rs`:
  types, `enum`, `const`, lengths, ranges, `items`, `properties`,
  `required`, `additionalProperties`, counts; annotations allowed; any
  other keyword is refused at deploy). A call whose input does not fit is
  400 naming the JSON pointer (`input /text: is required`).
- `channels` declares the app's channels and their readers (default
  `viewer`); `events`, `ops`, and `inbox` are built in (readers: viewers).
- `kind` is `query`, `mutation`, or `job` (below); a job's `role`
  defaults to `editor`.
- `triggers` (at most 32) start runs of an operation: `{"cron": "0 9 * *
  *", "run": op}` (five fields, UTC, 1 = Sunday), `{"channel": "inbox" |
  <app channel>, "run": op}` (each new record), `{"files": "notes/**",
  "run": op}` (a move of `main` changing a matching path; `*`, `**`, `?`,
  a trailing `/`). The operation must be a mutation or a job an editor
  may call.

`app.mjs` exports `class App extends DurableObject` with one method per
operation, each called `(input, call)`: `call.principal` (an identity,
`anon:…`, or the fragment's own npub for its triggered runs) and
`call.role`. A mutation is synchronous over the app's own
SQLite; `call.publish(channel, body, kind = "message")` appends a record
(body at most 64 KiB, 64 per mutation) once the mutation commits, and an
exception rolls back its writes and its records (422). The ledger keys a
mutation by (principal, id) for seven days: a retry with the same id
returns the stored result; the same id with another input is 409. Every
applied mutation also appends `{op, id}` to `ops`. An optional
`fetch(request)` answers every path that is not a site file (any
method), with `x-fragment-principal` and `x-fragment-role` set.

### Files

The app's files are the fragment's files in git. Reads come from `main`
(the working copy) at its pin; writes land on `main` as commits the cell
makes, and move the pin at once.

- `this.files.read(path)` (text) / `readBytes(path)` / `list(prefix)`
  (`[{path, size, blob?}]`) / `stat(path)` (`{path, size, sha, commit}`,
  `sha` the git blob) from anything async: queries, `fetch`, jobs. A
  file over 1 MiB (a blob) is not read into the app (413); the site
  serves it. An absent file reads as `null`.
- In a mutation, `call.files.write(path, content)` / `remove(path)`
  (content a string, `Uint8Array`, or `ArrayBuffer`; at most 16 files and
  256 KiB): applied once the mutation commits, as one commit, once (a
  replay commits nothing). Last writer wins.
- In a job, `job.files.read / list / stat / write / remove` are steps.
  `write(path, content, {expect})` and `remove(path, {expect})` compare
  and swap: `expect` is the blob `sha` the file must have, or `null` for
  "must not exist"; a mismatch fails the step with a `conflict`.
- A move of `main` the cell's own commit made carries its writer's depth:
  a file trigger started by it is one hop deeper.

Each fragment's app runs in its own loaded worker: its env holds only its
own capabilities (`FILES`, bound to it), and no module state is shared
with another fragment running the same code.

### Blobs

A file of 1 MiB or more is a git-lfs v1 pointer in git (`version
https://git-lfs.github.com/spec/v1`, `oid sha256:…`, `size …`), and its
bytes are a blob in the fleet's blob store under the fragment and the
hash. The CLI uploads a large file's bytes before it commits the pointer,
and downloads them when it pulls one, so a synced folder holds real
files; it does so where status answers `blobMinBytes`. The site (`/`, `__file`)
and `GET file` serve a pointer's bytes, with ranges. A blob that no
pointer at `main` or `live` has named for `FRAGMENT_BLOB_GRACE_S` (7
days) is deleted, so only the latest versions' bytes are kept: a
rollback older than that has pointers without bytes. A deleted
fragment's blobs go with it.

### Deliveries: web push and notifyUrls

A page subscribes with `await fragment.push.register(who)` (from a click:
it registers `./__sw.js`, reads the fragment's VAPID key from
`./__push-key`, and stores the subscription at `./__push-sub` tagged with
`who`); `fragment.push.unregister()` drops it (`./__push-unsub`, by its
endpoint). Anyone who can see the fragment may subscribe (at most 10 000
subscriptions). `call.push(who, payload)` in a mutation (sent once it
commits) and `job.push(who, payload)` in a job (a step, answering
`{queued}`) push `payload` (`{title, body, tag, url}`, at most 3800 bytes)
to the subscriptions tagged `who`, or all of them with `*`, once per
mutation or step. `fragment.notify.{supported, permission, ask, show}`
wrap the Notification API.

`fragment.json`'s `notifyUrls` (at most 3) receive `{type: "changed",
fragment, sha, paths}` (JSON POST, unsigned, as before) on each move of
`main`.

Every delivery is built whole by the fragment (a push is encrypted for
its browser, RFC 8291, and signed with the fragment's VAPID key, RFC
8292) and sent from the `fragment-deliveries` queue: a 429, 5xx, or
network failure is retried with a growing wait; a push service's 404 or
410 drops the subscription (`push.gone`); a delivery out of retries is
reported (`delivery.failed`).

### AI

A job calls OpenRouter with the fragment's own `OPENROUTER_API_KEY`
secret (added at the egress point; without it the step fails saying so):

- `job.ai.text({model, prompt | messages, max_tokens, reasoning})` →
  `{text, model, usage}` (chat completions). `reasoning` is OpenRouter's
  (`{effort: "low"}`, `{enabled: false}`, `{max_tokens}`), passed as
  given: a reasoning model can spend a small `max_tokens` thinking and
  answer nothing.
- `job.ai.image({prompt, path, model?, aspect_ratio?})` (default
  `google/gemini-3.1-flash-lite-image`) → `{path, size, sha256,
  mediaType}`: the image is written to `main` at `path` (a blob when 1 MiB
  or more).
- `job.ai.video({prompt, path, model?, duration?, resolution?,
  aspect_ratio?})` (default `minimax/hailuo-3-max`) → the same, for the
  video: the job starts it, polls every 20 seconds (up to about 15
  minutes) as steps, and saves it as a blob.

An OpenRouter 429 or 5xx is retried; 402 (out of credits) and other
refusals fail the step with OpenRouter's message.

### Jobs and triggers

A job is a method called `(input, job)` that runs as a celld Workflow,
outside any request. Each `await` on the job's four steps is durable:

- `job.call(op, input)`: an operation of this fragment as the run's
  principal, with `job:<run>:<step>` as its id (a retried or replayed
  step is a replay). Calling a job starts it: `{run, status}`, one hop
  deeper.
- `job.fetch(url, {method, headers, body})` → `{status, ok, headers,
  text(), json()}`: the app's one way out. `{{NAME}}` in a header value
  is the fragment's secret `NAME`, added by the platform at the egress
  point; the app never holds it. http(s) only; loopback, private, and
  `.internal` addresses are refused (on hosted fleets celld also checks
  where a name resolves: a name with no public address fails the step
  for good, `egress refused: …`); redirects are answered, not
  followed; a body of at most 256 KiB, a response of at most 1 MiB, 120
  seconds. Every fetch carries `x-fragment-hops`.
- `job.publish(channel, body, kind)`: a record, once per step.
- `job.sleep(ms | "N seconds|minutes|hours|days")`, up to 30 days.

`job.principal`, `job.role`, `job.run`, and `job.attempt` say who and
which. The method re-runs from the top at every step with the results so
far, so it must reach its steps in the same order each time and change
nothing except through steps. A step that fails for a reason that may
pass (an upstream 429 or 5xx, a timeout, a platform error) is retried 4
times with doubling delays; one that fails for good (a refused URL, an
unknown operation, a call that threw), or runs out of retries, makes the
`await` throw a `StepError` the job may catch. A job that throws is
**held**: its run keeps the input and error until someone replays it. At
most 100 steps and 4 MiB of step results per run; a result of at most
1 MiB.

Every job call and every trigger is a **run**: `queued`, `running`,
`succeeded`, `held`, or `blocked`. A triggered mutation is a run of one
step. Triggered runs act as the fragment itself (its npub) with an
editor's role, `via` `cron`, `channel`, or `files`, and a `depth`: a
record appended by a run at depth d triggers runs at d + 1, and a
delivery's `x-fragment-hops` is its depth. Deeper than 16 is `blocked`
(`cycle.detected`). An operation's triggers pause themselves (`op.
auto-paused`) after 5 held runs in 10 minutes or 120 triggered runs in
an hour (blocked runs do not count); while paused, a trigger records a
`blocked` run and cron skips.
A cron tick whose previous run is still going is skipped. The inbox's
pending records are its runs that have not succeeded; at 1000 a post is
429 (`inbox.rejected`). Finished runs are kept 30 days (at most 10 000).

CLI: `fragment runs <name> [<run>] [--status S]`, `fragment triggers
<name>`, `fragment replay <name> <run>`, `fragment pause|unpause <name>
<op>`, `fragment inbox <name> --token T --payload JSON`.

## Serving

`<name>.<suffix>/<path>` (or `/f/<name>/<path>` without a suffix; with
one, those redirect to the fragment's host, except `__watch`). Every path
on a fragment's host is the fragment's, `/api/…` included (the platform
API answers on the platform's host):

| path | |
| --- | --- |
| `/`, `/<page>` | `site/` in the live commit (`index.html` for directories); Open Graph tags from `fragment.json`'s `meta` |
| `__tree` | `{type, ref: "live", sha, count, files}`, content only |
| `__file?path=` | a content file from live, else main |
| `__preview.svg` | the placeholder preview image |
| `POST __op/{op}` | a browser's call: `application/json` `{id, input}`; an unsigned caller gets an anonymous principal cookie; callers holding only `public` get 60 calls a minute each, 600 per fragment |
| `__fragment.js` | the browser library (below) |
| `__live` | WebSocket, anyone who can see the fragment: channel subscriptions from a cursor, presence, change signals (below) |
| `__watch` | WebSocket, viewers and up (the share link, or a signed upgrade): `{type: "hello", ref, sha}`, then `{type: "changed", ref: "main", sha, paths}` per external move of main |
| anything else | the app's `fetch`, when it has one |

`__live` and `__watch` are also served in place at `/f/<name>/…` for the
CLI. The `__live` protocol (JSON frames):

- client → server: `{type: "subscribe", channel, after}`,
  `{type: "unsubscribe", channel}`, `{type: "presence", data}` (at most
  4 KiB; `null` clears), `{type: "ping"}`
- server → client: `{type: "hello", id, principal, role}`,
  `{type: "record", channel, seq, at, principal, kind, body}`,
  `{type: "subscribed", channel, next, more}` (after the backlog),
  `{type: "presence", list: [{id, principal, data}]}`,
  `{type: "changed", op}` (after every applied mutation),
  `{type: "error", message}`

A socket's role is fixed when it connects. Removing a member closes their
sockets; rotating the share link closes link holders'; a fragment that
stops being public closes its anonymous visitors'.

The browser library (`import * as fragment from "./__fragment.js"`):
`call(op, input, {id?})` (retries keep the id), `live(op, input,
onResult, onError?)` (re-runs a query after every change), `subscribe(
channel, onRecord, {after?})`, `presence.set(data)`, `presence.on(fn)`,
`me()`.

CLI: `fragment call <name> <op> --input '{...}' [--id ID]`, `fragment
channel <name> [<channel>] [--after N] [--follow]`.

## Agents (`agent/`, phase 5)

A separate celld project: agents act on fragments through the API above,
signing with their own keys. An agent is an identity its owner registers:
`POST /api/agents` answers a key proof by the agent's key, and the owner
sends it in `POST /api/identities {kind: "agent", proof}` (the CLI's
`fragment agent create` does both). Every owner route checks, live, that
the signing key is one of the owner's active keys
(`GET /api/identities/{owner}/keys/{npub}`, signed by the agent). Its variables: `FRAGMENT_HOST_SECRET` (seals
each agent's key), `FRAGMENT_API` (the platform it acts on),
`OPENROUTER_API_KEY` and `OPENROUTER_API_URL` (its model service),
`AGENT_URL` (its own base, for the inboxes it hands out),
`AGENT_TEST_HOOKS=allow` (dev and e2e only).

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/agents` | any signer | `{name, model? ("z-ai/glm-5.3-flash"), instructions?}` → `{name, npub, model, proof}`; its maker asking again before it is registered gets a fresh proof (`replayed`); otherwise 409 when taken; owner routes answer 400 until it is registered |
| `GET /api/a/{name}` | owner | → `{name, id, owner, npub, model, active, driving, outcome (running, idle, stopped, error), error, tokens, watchdogRestarts, messages: [{id, role, text, tool_requests, tool_responses, steer}], steer, toolRuns, steps}` |
| `POST /api/a/{name}/turns` | owner | `{text}` (at most 16 KiB) → `{started}`; during a turn, `{steered: true}` (read between steps) |
| `POST /api/a/{name}/stop` | owner | → `{active, driving}`; a tool in flight is interrupted |
| `GET /api/a/{name}/tools` | owner | → `{tools: ["<fragment>__<op>", ...]}` |
| `POST /api/a/{name}/listen` | owner | `{fragment, channel? ("chat"), reply? ("say")}` → `{fragment, channel, reply, subscription}`: the agent subscribes itself to the channel (it must be a member) with an inbox URL of its own (`AGENT_URL`); at most 16 |
| `POST /api/a/{name}/inbox/{token}` | the fragment's delivery (the token is the capability) | a record: someone else's starts a turn (or steers the running one); the agent's own, and one heard before, are ignored; the turn's last answer goes back as `POST /api/f/{fragment}/ops/{reply}` `{text}` with the id `rp:<40 hex of SHA-256 of its message id>`; an unknown token is 404 |
| `PUT /api/a/{name}/computer` | owner | `{url, token, cwd? ("work")}` → `{url, cwd, tools}`: attaches a computer once it answers `GET /tools` with that token (400 when it refuses it, 502 when it does not answer); the token is sealed like the agent's key |
| `DELETE /api/a/{name}/computer` | owner | → `{detached}` |
| `POST /api/a/{name}/test` | owner, test fleets | `{hold_in_tool_ms?, hold_after_tool_ms?, watchdog_ms?}` |

An agent's tools are the operations of the fragments whose members include
it, those its role there may call (at most 16 fragments, 128
tools), named `<fragment>__<op>` with the operation's input schema. A call
is `POST /api/f/<fragment>/ops/<op>` signed by the agent with the id
`tc:<40 hex of SHA-256 of the tool-call id>`: a replayed call replays the
operation. At most 64 steps a turn.

### Computers (`fragment computer serve`, phase 8)

A computer answers its agent with goose's developer tools. Every route
but `/health` takes `authorization: Bearer <its token>` (the file
`--token-file` names, made 0600 on first start).

| method & path | body → answer |
| --- | --- |
| `GET /health` | → `ok` |
| `GET /tools` | → `{tools: [shell, write, edit, tree as MCP tools], instructions}` |
| `POST /calls` | `{id, name, arguments, cwd, wait_ms? (at most 25000)}` → the call's record `{id, name, cwd, status (running, done, interrupted), started_at, finished_at, runs, result}`: starts the call, or re-attaches to the one with that id; waits up to `wait_ms` for it |
| `GET /calls/{id}?wait_ms=` | → the record; 404 for an unknown id |
| `POST /calls/{id}/cancel` | → `{cancelled}`; a shell command's process tree is killed |

The agent names a call `tc-<40 hex of SHA-256 of the tool-call id>` and
sends the attached `cwd` (`[a-z0-9-]{1,64}`, a directory under `--work`).
Records live under `--state`: a record still `running` when the computer
restarts answers `interrupted` and does not run again. While a computer is
attached, a turn offers its tools beside the fragments' operations.
