# fragment wire contract

The cell (`cell/`, Rust on celld) answers everything below; the CLI and
the browser library are its clients. Errors are `{"error": "<code>",
"message": "..."}` (codes in `crates/proto`). `cargo xtask e2e` proves
every route. The TypeScript runtime this replaced was deleted in phase 2
slice G (its contract is in git history, last at `35f5e18`).

## Configuration

Worker variables, rendered from the fleet's settings (ROADMAP decision
13; `cell/.dev.vars` in dev). None is a secret: the fleet's secrets are
the node's environment, where only `KEYS` reads them (below).

| Variable | Meaning |
|---|---|
| `CODESTORAGE_ORG` | the code.storage org |
| `CODESTORAGE_API_URL` | the API base (default `https://api.<org>.code.storage`) |
| `FRAGMENT_HOST_SUFFIX` | fragments are served from `<label>--<username>.<suffix>` (any other name under it is 404, never the platform); unset, from `/f/<name>/` |
| `FRAGMENT_POLL_INTERVAL_S` | the webhook backstop (default 300); also how often running runs are checked against their Workflows |
| `FRAGMENT_JOB_RETRY_DELAY_S` | a failed job step's first retry delay, doubling over 4 retries (default 10) |
| `FRAGMENT_EGRESS_LOCAL` | `allow` lets jobs fetch loopback and private addresses (dev and e2e fakes); never on a shared fleet |
| `FRAGMENT_BLOB_GRACE_S` | how long a blob no branch names is kept before it is deleted (default 7 days) |
| `FRAGMENT_PUSH_SUBJECT` | who push services may contact about this fleet's pushes (a `mailto:` or https URL; RFC 8292) |
| `FRAGMENT_DELIVERY_RETRY_S` | the shortest wait before a delivery is retried (default 10; the wait grows with the delivery's age, up to an hour) |
| `OPENROUTER_API_URL` | where AI calls go (default https://openrouter.ai) |
| `FRAGMENT_BUDGET_USD` | each person's monthly budget (default 20) |
| `FRAGMENT_OPERATORS` | identities and keys that may top up budgets (as `FRAGMENT_CREATORS` once read them) |
| `FRAGMENT_DEPLOY_ID` | which deployment this is (`cargo xtask deploy` sets it); `GET /healthz` answers it in `x-fragment-deploy` |
| `WORKOS_CLIENT_ID` | sign-in: fragment's WorkOS environment; unset, sign-in answers 500 |
| `WORKOS_API_URL` | where WorkOS is (default https://api.workos.com; dev and the e2e: the fake) |
| `FRAGMENT_PLATFORM_URL` | the platform's origin, where sign-in and the platform session live (default: the hostname suffix itself, e.g. https://fragment.club) |
| `FRAGMENT_SIGNINS_PENDING_MAX` | sign-ins begun and not finished that the registry keeps (default 100000; at least 1): a sign-in is kept through this many later starts, so the oldest is let go only past this many starts in its ten minutes (Sign-in, below) |
| `FRAGMENT_TEST_HOOKS` | `allow` on dev and e2e fleets only: `POST /api/test/registry {down}` makes the registry answer 503, and `{signins: "count"\|"expire"\|"sweep"}` counts sign-in's rows (`{logins, redemptions, sessions}`), expires every pending sign-in and unspent redemption, or runs its sweep now; `GET /api/test/env` answers the Worker variables; `POST /api/test/keys {fragment, op, plaintext\|sealed}` seals or opens through `KEYS` as that fragment; `POST /api/test/fragment {fragment, op, …}` pulls a lever on that fragment: `fail-deliveries {times}` fails its next queue sends, `fail-outbox {times}` fails its next records' outbox writes just after their append, `fail-triggers {times}` fails its next trigger steps just before their last run starts, `drop-live {code}` drops its live sockets, `ledger {ms \| null}` shortens (or restores) its operation ledger's window, `age {ms}` forgets its write keys as if `ms` had passed, and `members {fill}` adds placeholder members until there are `fill` |

The node's environment (Fly secrets on a fleet; `devstack` in dev and
the e2e), read by `KEYS`, the native service in our celld fork
(`crates/native`, docs/hardening.md). The cell asks `KEYS` to seal and
open (for the calling cell only), to sign code.storage tokens (for a
`Fragment`), to exchange WorkOS's code (for the `Registry`), and to call
OpenRouter's key API (for a `Ledger`); it never holds these keys:

| Variable | Meaning |
|---|---|
| `FRAGMENT_KEYS_HOST_SECRET` | seals secrets at rest, per cell (at least 32 bytes) |
| `FRAGMENT_KEYS_HOST_SECRET_PREVIOUS` | the secret before a rotation; values sealed under it open and come back resealed |
| `FRAGMENT_KEYS_CODESTORAGE_ORG`, `FRAGMENT_KEYS_CODESTORAGE_PRIVATE_KEY` | the org and its PKCS#8 P-256 key (a one-line PEM may carry literal `\n`) |
| `FRAGMENT_KEYS_WORKOS_API_KEY`, `FRAGMENT_KEYS_WORKOS_URL` | WorkOS's API key and base (default https://api.workos.com) |
| `FRAGMENT_KEYS_OPENROUTER_MANAGEMENT_KEY`, `FRAGMENT_KEYS_OPENROUTER_URL` | mints each billing org's own OpenRouter key, its limit the org's monthly allowance; unset, only a fragment's own `OPENROUTER_API_KEY` pays for AI |

And the fork's settings the fleet turns on: `CELLD_FACET_MAX_BYTES`
(the app database's hard stop, 20 MiB), `CELLD_DYNAMIC_LOCKDOWN=1`
(loaded workers without `eval` or `Atomics.wait`),
`CELLD_INTERNAL_PEER_ONLY=1` (the internal listener serves only
fleet-signed peers), with `CELLD_EGRESS_PUBLIC_ONLY=1` from phase 3, and
`CELLD_MAX_REQUEST_BODY_BYTES` (256 MiB, the largest body any route
takes: a blob; celld's default is 1 GiB).

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
Every field is required and typed (a tag is an array of strings). A
`payload` tag binds the body even when it is empty: a signature over a
body, sent without one, is 401. A blob upload is signed without
`payload` (with one, 401): its URL names the bytes' hash, which the cell
checks as they arrive. Every other body the router reads
is at most 2 MiB, measured as it arrives: a longer declared
`content-length`, or a chunked body that runs past it, is 413 before
anything is authenticated.

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
| `POST /api/identities` | a person | `{kind: "agent", proof}` → a new agent identity they own, holding the proof's key (FIN-11's trusted initial registration); again, the same one; a key someone else holds is 409; an agent owns no agents (403) |
| `GET /api/identities/{id\|me}` | the identity, or its owner | → `{id, kind, owner?, createdAt, keys: [{npub, addedAt, addedBy, revokedAt?}], agents: [id], subjects: [{issuer, email, linkedAt}]}`; anyone else 404 |
| `POST /api/identities/{id\|me}/keys` | a person for themselves; an owner for their agent | `{proof}` → the identity with the key added (at most 64 keys, revoked ones included); a key someone else holds, or a revoked one, is 409 |
| `DELETE /api/identities/{id\|me}/keys/{npub}` | the same | → the identity; the key is 401 from the next request and never comes back; an agent's last active key cannot be revoked (400); a person who signs in may hold none |
| `GET /api/identities/{id}/keys/{npub}` | the identity, or an agent it owns | → `{active}` (an agent's runtime checks its owner's keys with it) |
| `PUT /api/identities/me/username` | a person | `{username}` → `{username, claimed}`: chosen once (3 to 32 of lowercase letters, digits, and single dashes, not starting or ending with one, and not a reserved word); taken 409, another after yours 409, yours again `claimed: false` |
| `PUT /api/identities/me/picture` | a person with a username | the image (PNG, JPEG, WebP, or GIF, told by its bytes; at most 256 KiB) → `{sha, mime}` |
| `GET /api/users/{username}` | anyone | → `{id, kind, username, picture}` (`picture`: its URL, or null) |
| `GET /api/users/{username}/picture` | anyone | the picture's bytes |
| `DELETE /api/users/{username}` | the fleet's operators | → `{username, identity, released}`: undoes a username taken by mistake, so its person chooses again; refused (409) while they own a fragment under it (its URLs name it) |

## Names (decision 16)

A person chooses a **username** once (above; the platform's page asks
after the first sign-in, and `fragment username <name>` does too). A
fragment's **name** is `<label>.<username>`: `desktop.futurepaul`,
served at `desktop--futurepaul.<suffix>` (one DNS label, under the
suffix's one wildcard certificate; or `/f/desktop.futurepaul/` on a
fleet without a suffix), its code.storage repo `desktop--futurepaul`.
A label and a username never contain `--`. Creating with a bare label
puts it under the creator's username; creating under someone else's is
403. In a signed request's path, a bare label names the signer's own
fragment (`/api/f/todo/status` is `todo.<your username>`); anything
unsigned (an inbox, a webhook, a site) names it in full. Each new
fragment asks for its host's TLS certificate (`KEYS` asks Fly, for the
fragment of that name only).

## Sign-in (phase 4 slice B)

A person is keyed by their verified `(issuer, subject)`: the issuer is
`workos:<client id>` (the environment), the subject WorkOS's user id. The
email is an attribute, refreshed at each sign-in and never matched. The
platform holds no key for a person; browsers hold sessions, each looked up
live in the registry on every request (a 30-day lifetime; tokens are 32
random bytes, the registry keeps their SHA-256).

| method & path (platform origin) | what |
| --- | --- |
| `GET /` | who is signed in, with links to sign in, sign out, and link another sign-in; with a username, their fragments and the "new fragment" form |
| `POST /auth/new` | the form (`label`, `template`): makes `<label>.<username>` from the template, then → `/auth/fragment?name=…&return=/` (signed in on its origin, and there); a refusal is a 400 page saying why; another origin 403 |
| `GET /auth/login?return=&login_hint=` | → WorkOS's authorize URL (`provider=authkit`, `redirect_uri` `<platform>/auth/callback`, a state); the state is bound to the browser by `fragment_login` (HttpOnly, SameSite=Lax, `Path=/`, ten minutes) |
| `GET /auth/link?return=` | the same from a signed-in browser: the sign-in that comes back joins this person (409 when it is someone else's) |
| `GET /auth/callback?code=&state=` | the state must match the browser's cookie (400 otherwise); the code is exchanged server-side; → `fragment_session` (HttpOnly, SameSite=Lax, `Path=/`) and back to `return`; a WorkOS `error` is shown (400) |
| `POST /auth/logout` | ends the session and every fragment session made from it, clears the cookie, and sends the browser to WorkOS's logout (`session_id` from the access token's `sid`); from another origin, 403 (`GET` shows the button) |
| `GET /auth/fragment?name=&return=` | signed in: → `<fragment origin>/__signin?token=<a single-use redemption, 60 s, for that fragment only>` (a session holds at most 16 unspent; past that, the oldest is refused); signed out: → sign in first |
| `GET /cli?key=<npub>&proof=` | the link `fragment login` prints: `proof` is the key's own NIP-98 event for `POST <platform>/cli/approve`, good for ten minutes (the proof of possession; without it, stale, or by another key: 400). Signed in: a page showing the key's last eight characters, to compare with the terminal, and an Add button; signed out: → sign in first, keeping the link |
| `POST /cli/approve` | the page's form (`key`, `proof`): the key joins the signed-in person at once (a key someone else holds, or a revoked one, is 409; another origin 403); the CLI waits for `GET /api/identities/me` to answer. People themselves come only from sign-in (`POST /api/identities {kind: "person"}` is 400) |

Over https every session cookie whose path is `/` is named with the
`__Host-` prefix (`__Host-fragment_session`, `__Host-fragment_login`,
`__Host-fragment_site`) and read under that name only: a fragment's page
may set a cookie for all of the suffix's domain, but never a `__Host-`
one, so none can stand in for the platform's session or another
fragment's.

On a fragment's origin, `GET __signin?token=` redeems the redemption for
this fragment only (another fragment's is 401 and stays unspent) and sets
`fragment_site` (HttpOnly, SameSite=Lax, host-only, `Path=/` or
`/f/<name>/`); without a token it starts at the platform, unless this
origin's session is live already (then straight back). A request with
that cookie is its person, exactly as the same request signed by one of
their keys: the same decision either way. A cookie for another fragment,
or whose platform session ended, is nobody. A platform session keeps its
newest 4 sessions on each fragment (the oldest ends). `__signout` ends
the session in the registry and clears the cookie: a copy of it is
nobody from then on. The cookie is cleared whatever the registry
answers; when the registry cannot end the session, the failure is
logged and a copy lasts until the session expires or its platform
session ends (`/auth/logout`).

`return` is a path on the origin it returns to, kept only when it begins
with one `/` and holds no byte at or below 0x20, no DEL, and no
backslash, and when, percent-decoded once more, it still does not begin
with `//`, `/\`, or `/` and a control byte; anything else returns to `/`.
A path with a space in it arrives encoded (`return=%2Fa%2520b` returns to
`/a%20b`). The way back is always an absolute URL on that origin.

A pending sign-in (begun and not finished) is good for ten minutes and
is kept through the next `FRAGMENT_SIGNINS_PENDING_MAX` starts (default
100000): past that, the oldest is let go, and finishing it is 400; a
fresh start always works. At the default, a sign-in in progress is lost
only while someone starts more than 100000 in ten minutes, about 166 a
second, sustained. Expired sign-ins, redemptions, and sessions are
deleted in batches on the registry's alarm, never on a request.

## Control API

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/fragments` | a person with a username (an agent is 403) | `{name, visibility?, template?}`: `name` a label, or `<label>.<your username>` → `{name, npub, owner, visibility, viewToken, inboxToken, webhookSecret, repo, canonical}` (`name` in full). The fragment's own key is made by the node's `KEYS` and stays sealed there. The cell creates (or, for a name deleted before, finds) the code.storage repo. With `template` (`desktop`, `chat`, `todo`, `inbox`, `blank`; any other is 400 and nothing is made), the template's files are main's first commit (its `fragment.json` stamped with the fragment's name) and live at once; one that fails to land is retried by the fragment's alarm (`template.failed` events). `notes` is the CLI's only (`fragment new --template notes`). |
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
| `POST /api/f/{name}/join` | any signer | `{token}` → `{name, role, joined}`; a stronger existing role is kept; a fragment at its 1000 members is 400, and the invite keeps its use |
| `PUT /api/f/{name}/visibility` | owner | `{visibility}` → `{ok, visibility}` |
| `POST /api/f/{name}/rotate` | owner | `{scopes?: [inbox, view, webhook]}` → `{ok, inbox_token, view_token, webhook_secret, rotated}`; a new view token closes link holders' feeds |
| `PUT /api/f/{name}/secrets/{KEY}` | editor | raw body (at most 64 KiB) → `{ok, name}`; sealed (AES-256-GCM, key HKDF'd from the host secret and the fragment's npub) |
| `GET /api/f/{name}/secrets` | editor | → `{names}`; values never leave |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor | → `{ok, removed}` |
| `GET /api/f/{name}/storage-token` | editor | → `{token, repo, api, expiresAt}`: ES256, this repo, `git:read`+`git:write`, 15 minutes |
| `POST /api/f/{name}/refresh` | editor | → `{ok, refs: {main: {pin, moved} \| {absent}, live: ...}}` |
| `POST /api/f/{name}/files` | editor | `{files: [{path, text \| base64} \| {path, delete: true}], message?, key?}` → `{commit}`: one commit to main, as a sync makes (at most 16 files and 256 KiB; paths relative, no `.` or `..`). The same `key` from the same person answers the first commit again. Main's pin moves at once; live does not |
| `POST /api/f/{name}/deploy` | editor | `{note?}` → `{live, canonical}`: live to main's tip, as `fragment deploy` does (a first deploy makes the branch; after a rollback, a merge commit), guarded against a live that moved meanwhile; the app installs from it at once |
| `POST /api/f/{name}/webhook` | code.storage | signed with the fragment's webhook secret (`X-Pierre-Signature`, 5 minutes); validate, remember (redeliveries are acknowledged), then move the pin to the branch's head as read now |
| `GET /api/f/{name}/files` | viewer | → `{ref, files: [{path, size, mode, lastCommitSha, machinery, blob?}]}` at main; a pointer's `size` is its bytes' |
| `GET /api/f/{name}/file?path=` | viewer | → the bytes at main (`x-fragment-ref`); a pointer's come from the blob store |
| `PUT /api/f/{name}/blobs/{sha256}` | editor | the bytes as the body (`content-length` required, at most 256 MiB), streamed through and hashed on the way in: → `{ok, sha, size, stored}`; bytes that hash to anything else are deleted and refused (400) |
| `GET`, `HEAD /api/f/{name}/blobs/{sha256}` | viewer | → the bytes (ranges answer 206) |
| `GET /api/f/{name}/file/stat?path=` | viewer | → `{stat: {path, size, blobSha, lastCommitSha, present}, ref}` |
| `GET /api/f/{name}/events?since=` or `?tail=` | viewer | → `{events: [{id, at, kind, summary, data}]}`, oldest first: the page after `since`, or the newest `tail` (1-500; 400 otherwise, or with `since`) (500 a page; 10 000 kept, as for `ops`: `limits::AUDIT_KEPT`) |
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
  `constructor`, `fetch`, and `alarm` are not operation names (the App
  class's own; `fragment_proto::RESERVED_OP_NAMES`): a manifest naming one
  is refused at deploy.
- `capabilities` asks the platform for powers the page uses, each granted
  only to the fragment's owner viewing it. The one there is:
  `"fragments"` (`__fragments`, Serving). Any other name is refused at
  deploy.
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
returns the stored result and applies nothing again; the same id with
another input is 409; after seven days the same id runs again, as a new
run. Every applied mutation also appends `{op, id}` to `ops`.

The platform checks a mutation's effects twice: in the app, where a
refusal rolls the mutation back (422), and again in the cell before it
applies any, since the app's own code can defeat the first check. A
refusal there (an undeclared or built-in channel, a bad kind, a size or
count over its limit, a bad path, a blob pointer) drops all of the
mutation's effects, answers 422 `app_failed` (the mutation's own writes
stand), records `effects.refused` in `events`, and still counts the
mutation as applied. A passing failure while applying them (code.storage,
the delivery queue) answers 502 or 500; the mutation stays pending, and the
cell tries again, waiting 10 s and doubling to an hour, for about two days
(`effects.delayed` on the first failure, `effects.abandoned` if it gives
up). A replay of a pending mutation tries at once. An optional
`fetch(request)` answers every path that is not a site file (any
method), with `x-fragment-principal` and `x-fragment-role` set.

The app's database holds at most 16 MiB: a mutation that would leave it
larger rolls back and answers 507 `storage_full`; the app still reads,
and deleting rows makes room. A write from anywhere else (a query, the
app's `fetch`) meets the node's own stop, 4 MiB above. The app runs
without code generation from strings (`eval`, `new Function`) and
without `Atomics.wait`, and a turn whose heap grows past twice the
isolate's limit (128 MiB) ends with "Worker exceeded its memory limit"
(422). When the node serving the fragment cannot load another app (celld
keeps at most 255 per node until it restarts), calls into the app answer
503 `node_full` and the rest of the fragment works (docs/hardening.md).

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
  256 KiB; a path of at most 300 bytes; never a blob pointer's text):
  applied once the mutation commits, as one commit, once (a replay
  commits nothing). Last writer wins.
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
to the subscriptions tagged `who` (at most 64 characters), or all of them
with `*`, once per mutation or step. `fragment.notify.{supported,
permission, ask, show}` wrap the Notification API.

`fragment.json`'s `notifyUrls` (at most 3) receive `{type: "changed",
fragment, sha, paths}` (JSON POST, unsigned, as before) on each move of
`main`.

Every delivery is first written to the fragment's delivery outbox with
what caused it (a record's deliveries in the same turn as the record, a
push as it is accepted), then built whole by the fragment (a push is
encrypted for its browser, RFC 8291, and signed with the fragment's VAPID
key, RFC 8292) and put on the `fragment-deliveries` queue; it leaves the
outbox once the queue has it. One the queue does not take waits and is
tried again from the fragment's alarm (`delivery.deferred`), up to 20
times (then `delivery.failed`), so delivery is at least once: a record
subscriber dedupes by channel and `seq`. From the queue: a 429, 5xx, or
network failure is retried with a growing wait; a push service's 404 or
410 drops the subscription (`push.gone`); a delivery out of retries is
reported (`delivery.failed`).

### AI

A job calls OpenRouter as its steps. Who pays: a fragment with its own
`OPENROUTER_API_KEY` secret pays with it, unmetered. Otherwise its owner
does, from their monthly budget (Budgets, below): each paid step reserves
its worst case in the owner's ledger before it runs (text $0.05, an
image $0.10, a video $0.10 a second), runs on the owner's own OpenRouter
key, and settles to the cost OpenRouter reports (`usage.cost`). An
answer that reports no cost is charged the step's reservation
(`ai.cost-missing`), never nothing. A video that fails, is cancelled, or
expires is charged nothing unless a cost is reported
(`ai.video-undelivered`), and a run held while its video still waits for
its cost gives that reservation back (`ai.video-released`). A step
the month cannot cover fails with `budget used up` (uncaught, the run is
held; replay it after a top-up or in a new month). A step the ledger
already settled answers its stored result, so a replayed run is not paid
twice. The key is added at the egress point and never reaches the app.
The steps:

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
  minutes) as steps, and saves it as a blob. Each poll step answers
  `{status, ended, error, urls, usage}`; `ended` is the platform's list
  of final statuses (completed, failed, cancelled, expired), and a video
  that ended any way but completed throws.

An OpenRouter 429 or 5xx is retried; 402 (out of credits, or past the
key's limit) and other refusals fail the step with OpenRouter's message.
A run answers `costMicros`: what its paid steps cost the owner's budget.

### Budgets (phase 4 slice C)

Every person has a monthly budget (`FRAGMENT_BUDGET_USD`, $20) in their
own billing org, `org:` + their identity's hex (finite.computer's
personal org; FIN-10). A month is a UTC calendar month; the allowance is
the budget plus that month's top-ups; money is in micro-dollars. The
org's own OpenRouter key, minted with the fleet's management key, carries
the allowance as its monthly limit, so OpenRouter itself stops the org
there. Running out stops only paid steps, never sites or mutations.

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/budget` | a person (an agent: its owner's) | → `{billingOrg, period, budgetMicros, toppedUpMicros, allowanceMicros, spentMicros, reservedMicros, remainingMicros, warn, usage}`: `warn` at 80% of the allowance; the newest 20 usage rows |
| `GET /api/budget/usage?period=YYYY-MM` | the same | → the same with every usage row of the month |
| `POST /api/budget/{id}/top-up` | the fleet's operators | `{usd}` → the month: the allowance rises, and the org's key limit with it |

A usage row is FIN-10's report shape: `{sourceRef, agent?, billingOrg,
period, unit: "usd_micro", quantity, state (reserved, settled), kind,
model, fragment, principal, at}`, one per paid step
(`sourceRef` is `<fragment>@<incarnation>/run/<run>/step/<index>`),
recorded once.

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
`await` throw a `StepError` the job may catch; so does a step whose
arguments do not fit its kind (`job.ai.text` without a model, say), with
what does not fit. Every kind of step and its arguments are defined once,
as `Step` in `crates/core/src/steps.rs`. A job that throws is
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
`blocked` run and cron skips. Held runs from before an unpause do not
count toward the next auto-pause. A deploy whose code no longer has an
operation drops its pause (an operation of that name later starts
unpaused).
A cron tick whose previous run is still going is skipped. The inbox's
pending records are its runs that have not succeeded; at 1000 a post is
429 (`inbox.rejected`). Finished runs are kept 30 days (at most 10 000).

CLI: `fragment runs <name> [<run>] [--status S]`, `fragment triggers
<name>`, `fragment replay <name> <run>`, `fragment pause|unpause <name>
<op>`, `fragment inbox <name> --token T --payload JSON`.

## Serving

`<label>--<username>.<suffix>/<path>` (or `/f/<name>/<path>` without a suffix; with
one, those redirect to the fragment's host, except `__watch`). Every path
on a fragment's host is the fragment's, `/api/…` included (the platform
API answers on the platform's host):

| path | |
| --- | --- |
| `/`, `/<page>` | `site/` in the live commit (`index.html` for directories); Open Graph tags from `fragment.json`'s `meta` |
| `__tree` | `{type, ref: "live", sha, count, files}`, content only |
| `__file?path=` | a content file from live, else main |
| `__preview.svg` | the placeholder preview image |
| `POST __op/{op}` | a browser's call: `application/json` `{id, input}`; a signed-in browser (`fragment_site`) calls as its person; an unsigned caller gets an anonymous principal cookie; callers holding only `public` get 60 calls a minute each, 600 per fragment |
| `__signin`, `__signout` | this origin's session (Sign-in, above) |
| `__join?invite=<token>` | an invite in a browser: signed out, → `__signin` and back; signed in, a Join button that posts `invite` here (form-encoded; another origin 403) and joins as the person |
| `__fragment.js` | the browser library (below) |
| `__fragments` | `{fragments: [{name, role, url}]}`: the fragments this fragment's owner belongs to, only to the owner signed in here, and only when `fragment.json` at live declares `"capabilities": ["fragments"]` (anyone else, or a page that does not ask, 403). A dashboard's page, such as the desktop's. `POST` `application/json` `{label, template}` → `{name, url}` makes `<label>.<username>` for the owner, as `POST /api/fragments` would, under the same conditions |
| `__people?id=…&id=…` | anyone who can see the fragment: `{profiles: {<id>: {kind, username, picture}}}` for up to 64 identities (an agent's `username` is its owner's; a picture is an absolute platform URL); an id the registry does not hold is left out |
| `__files` | an HTML list of the content files (live and main) linking to `__file`; framed, a click asks the page around it to open the file (`postMessage({fragment: "open", url, title})`) |
| `__live` | WebSocket, anyone who can see the fragment: channel subscriptions from a cursor, presence, change signals (below) |
| `__watch` | WebSocket, viewers and up (the share link, or a signed upgrade): `{type: "hello", ref, sha}`, then `{type: "changed", ref: "main", sha, paths}` per external move of main |
| anything else | the app's `fetch`, when it has one |

`__live` and `__watch` are also served in place at `/f/<name>/…` for the
CLI. The `__live` protocol is JSON frames tagged by `type`, defined once
as `LiveIn` (client → server) and `LiveOut` (server → client) in
`crates/proto/src/live.rs`; the cell and the CLI decode through them:

- client → server: `{type: "subscribe", channel, after}` (records with
  `seq` after `after`) or `{type: "subscribe", channel, last}` (the last
  `last` records, at most 1000): exactly one of `after` and `last`;
  `{type: "unsubscribe", channel}`, `{type: "presence", data}` (at most
  4 KiB; `null` or no `data` clears), `{type: "ping"}`
- server → client: `{type: "hello", id, principal, role}`,
  `{type: "record", channel, seq, at, principal, kind, body}`,
  `{type: "subscribed", channel, next, more}` (after each page),
  `{type: "presence", list: [{id, principal, data}]}`,
  `{type: "changed", op}` (after every applied mutation),
  `{type: "pong"}` (to a ping), `{type: "error", message}` (a frame
  that does not decode, with what was wrong, or a refusal; the socket
  stays open)

A subscribe answers one page of the backlog: at most 1000 records and
about 1 MiB of record frames. With `more: true` the socket is not yet
live on the channel: subscribe again from `next`. The page that reaches
the end (`more: false`) makes it live, so a record appended while a
client pages arrives in its turn, never ahead of the records before it.

A socket's role is fixed when it connects. Removing a member closes their
sockets; rotating the share link closes link holders'; a fragment that
stops being public closes its anonymous visitors'.

The browser library (`import * as fragment from "./__fragment.js"`):
`call(op, input, {id?})` (retries keep the id), `live(op, input,
onResult, onError?)` (re-runs a query after every change), `subscribe(
channel, onRecord, {after?, last?})` (pages through the backlog, then
follows live; after a reconnect it resumes after the last record),
`presence.set(data)`, `presence.on(fn)`, `me()`, `closed(fn)`. The page's
socket reconnects by itself after a jittered wait (half to one and a
half times a backoff that doubles from 1 to 30 seconds), except after a
close the fragment means for good: 4003 (the page's access was revoked)
or 4004 (the fragment was deleted) ends it, and `closed` handlers get
`{code, reason}`.

CLI: `fragment call <name> <op> --input '{...}' [--id ID]`, `fragment
channel <name> [<channel>] [--after N] [--follow]`.

## Agents (`agent/`, phase 5; co-hosted since phase 6)

A second script in the platform's fleet, with no ingress of its own: the
router authenticates `/api/agents` and `/api/a/*` like any signed
request and hands them on with the caller's identity
(`x-agent-principal`), which is all the script trusts; an inbox delivery
passes as it came. Agents act on fragments through the API above,
signing with their own keys. An agent's name is `<label>.<username>`,
its owner's (a bare label is one of the signer's own); making one also
registers it as its maker's, in the same request. Owner routes check the
caller is the agent's registered owner. Each agent's key is made by the
node's `KEYS` and signs there. Its turns spend its owner's month: as a
turn starts it asks for its owner's org key (`POST /api/budget/key`,
signed by the agent). Every person has their own agent,
`agent.<username>`, made on first need: a chat made from the `chat`
template has it as an editor that listens. Its variables: `FRAGMENT_API`
(the platform it acts on), `OPENROUTER_API_URL` (its model service),
`AGENT_URL` (the base of the inboxes it hands out: the platform's),
`AGENT_TEST_HOOKS=allow` (dev and e2e only). The script reads a request
body of at most 64 KiB, measured as it arrives (413 before anything
else).

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/agents` | a person with a username | `{name, model? ("z-ai/glm-5.3-flash"), instructions?}` → `{name, npub, model, id}`: made and registered as the caller's; again by its owner, the same answer (`replayed`); a name under someone else's username is 403 |
| `GET /api/a/{name}` | owner | → `{name, id, owner, npub, model, active, driving, outcome (running, idle, stopped, error), error, tokens, watchdogRestarts, messages: [{id, role, text, tool_requests, tool_responses, steer}], steer, toolRuns, steps}` (each list its newest 256, oldest first) |
| `POST /api/a/{name}/turns` | owner | `{text}` (at most 16 KiB) → `{started}`; during a turn, `{steered: true}` (read between steps) |
| `POST /api/a/{name}/stop` | owner | → `{active, driving}`; a tool in flight is interrupted |
| `GET /api/a/{name}/tools` | owner | → `{tools: ["platform__create_fragment", "platform__list_files", "platform__read_file", "platform__write_files", "platform__deploy", "<fragment>__<op>", ...]}`: the platform's verbs (a fragment the agent makes is its owner's, the agent an editor; a file write's key is the tool call's), then its fragments' operations |
| `POST /api/a/{name}/listen` | owner | `{fragment, channel? ("chat"), reply? ("say")}` → `{fragment, channel, reply, subscription}`: the agent subscribes itself to the channel (it must be a member) with an inbox URL of its own (`AGENT_URL`); at most 16 |
| `POST /api/a/{name}/inbox/{token}` | the fragment's delivery (the token is the capability) | a record: someone else's starts a turn (or steers the running one); the agent's own, and one heard before, are ignored; the turn's last answer goes back as `POST /api/f/{fragment}/ops/{reply}` `{text}` with the id `rp:<40 hex of SHA-256 of its message id>`; an unknown token is 404 |
| `PUT /api/a/{name}/computer` | owner | `{url, token, cwd? ("work")}` → `{url, cwd, tools}`: attaches a computer once it answers `GET /tools` with that token (400 when it refuses it, 502 when it does not answer); the token is sealed like the agent's key |
| `PUT /api/a/{name}/computer` | owner | `{connect: true, cwd?}` → `{connect, agent, token, cwd}`: a computer that connects out instead (`fragment computer connect --agent <agent> --token-file <f>`): a new connect token, answered once (the agent keeps its SHA-256), replacing any computer before |
| `POST /api/a/{name}/computer/poll` | the connect token (`x-computer-token`) | → `{requests: [{rid, method, path, body}]}`: what the agent asks of its computer (the routes `fragment computer serve` answers), at once or within 25 s; one fetched and not answered in 40 s is handed out again; a wrong token 403 |
| `POST /api/a/{name}/computer/answer` | the connect token | `{rid, status, body}` (at most 6 MiB) → `{ok}`. A turn leaves out a computer that has not polled in 60 s |
| `DELETE /api/a/{name}/computer` | owner | → `{detached}` |
| `POST /api/a/{name}/test` | owner, test fleets | `{hold_in_tool_ms?, hold_after_tool_ms?, watchdog_ms?, window_messages? (2-256)}` |

An agent's tools are the operations of the fragments whose members include
it, those its role there may call (at most 16 fragments, 128
tools), named `<fragment>__<op>` with the operation's input schema. A call
is `POST /api/f/<fragment>/ops/<op>` signed by the agent with the id
`tc:<40 hex of SHA-256 of the tool-call id>`: a replayed call replays the
operation. At most 64 steps a turn. Each step sends the model a window
of the conversation, not all of it: the newest 256 messages, cut to
start at a turn's first message (so a tool call and its result stay
together), with the running turn whole and earlier turns while they
total 256 KiB. A turn that alone outgrows the window ends in an error;
the next message starts a turn that fits. A channel-started turn's
answer is its last message, when that is the model's text.

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
restarts answers `interrupted` and does not run again. A finished record
is kept a week (a replay comes within the hour), and at most 10,000 of
them, the oldest pruned first, at start and hourly; after that, the same
id would run again. While a computer is attached, a turn offers its tools
beside the fragments' operations.
