# fragment wire contract

The cell (`cell/`, Rust on Cloudflare Workers) answers everything below;
the CLI and the browser library are its clients. Errors are `{"error":
"<code>", "message": "..."}` (codes in `crates/proto`). `cargo xtask e2e`
proves every route. The TypeScript runtime this replaced was deleted
(its contract is in git history, last at `35f5e18`). The desktop and the
personal agent's chat went at the cut (docs/cloudflare-v1.md, decision
33; their contract is at the tag `celld-final`); computers came back
(Computers, below), with goose as our image's agent runtime
(docs/optchat.md).

## Configuration

Worker variables, rendered from the deployment's config by `cargo xtask
deploy` (`deploy/example.jsonc`; `.dev.vars` in dev). None is a secret:
the deployment's secrets are Worker secrets (below).

| Variable | Meaning |
|---|---|
| `CODESTORAGE_ORG` | the code.storage org (required) |
| `CODESTORAGE_API_URL` | the API base (default `https://api.<org>.code.storage`) |
| `FRAGMENT_HOST_SUFFIX` | fragments are served from `<label>--<username>.<suffix>` (any other name under it is 404, never the platform; the suffix's own name is the platform's, or redirects to it: Hosts, below). Every deployment names one: an isolate without it does not start |
| `FRAGMENT_HOST_LABEL_SUFFIX` | a branch deployment's mark, `--<branch>`: its fragments are `<label>--<username>--<branch>.<suffix>`, one DNS label beside the other branches' in one zone |
| `CODESTORAGE_REPO_PREFIX` | what this deployment's repos are named with first (a branch's `<branch>--`), so deployments sharing an org never share a repo |
| `FRAGMENT_POLL_INTERVAL_S` | how often a busy fragment's pass runs (default 300): one whose pins may lag its repo (a storage token was minted for it, or a move failed to follow, in the last day), which polls code.storage for its branches (the backstop for a push no one refreshed), or with a run in flight (checked against its Workflow), an ended run's reservation to give back, or a template or the agent it declares still to land. Any other fragment's pass is daily, and polls nothing: every move the platform makes or is told of (`refresh`) is followed at once |
| `FRAGMENT_JOB_RETRY_DELAY_S` | a failed job step's first retry delay, doubling over 4 retries (default 10) |
| `FRAGMENT_EGRESS_LOCAL` | `allow` lets jobs fetch loopback and private addresses, and takes a connection's http:// consent URL (dev and e2e fakes); never on a shared fleet. It also makes a fleet local for its test levers (Test levers, below) |
| `FRAGMENT_BLOB_GRACE_S` | how long a blob no branch names is kept before it is deleted (default 7 days) |
| `FRAGMENT_DELIVERY_RETRY_S` | test fleets: every wait before a delivery, or a preview card's failed shot, is tried again, pinned. Unset, a delivery's wait is 10 s growing with its age to an hour, and a shot's 10 s doubling to an hour (Cards, below) |
| `AI_GATEWAY_ID` | the AI Gateway the model route and image steps call through (Models, below): the deployment's own, named (`default` is refused: it makes one that logs); unset, models and images are off |
| `FRAGMENT_AI_URL` | dev and the e2e only: the model route POSTs the AI binding's input to `<url>/run/<model>` instead of calling the binding (the Workers AI fake, a lower rung) |
| `FRAGMENT_DEFAULT_PLAN` | a new person's plan (Ledger, below): `guest` (the default and production's), `seat`, or `seat_always_on`; dev and the e2e set `seat` |
| `FRAGMENT_COMPUTER_UNSAVED_MAX_MS` | how long a computer whose sleep's save keeps failing stays awake, its container kept, before it sleeps unsaved (the deploy config's `computers.unsaved_max_ms`; default 1800000, thirty minutes; docs/computers.md, "Saves and what a wake restores") |
| `FRAGMENT_OPERATORS` | identities and keys that grant credit and set plans, seats and overdrafts, release usernames, and wipe a person (Operators, below); a key listed is an operator's whether or not anyone holds it |
| `FRAGMENT_DEPLOY_ID` | which deployment this is (default `dev`); `GET /healthz` answers it in `x-fragment-deploy` |
| `WORKOS_API_URL` | where WorkOS is (default https://api.workos.com; dev and the e2e: the fake); sign-in is on where the `WORKOS_CLIENT` secret is bound (below), and answers 500 where it is not |
| `FRAGMENT_PLATFORM_URL` | the platform's origin, where sign-in and the platform session live (required; fragment.club's is https://fragment.club, on no fragment's domain). Every push's VAPID token names it as its contact (`sub`, RFC 8292) |
| `FRAGMENT_SIGNINS_PENDING_MAX` | sign-ins begun and not finished that the registry keeps (default 100000; at least 1): a sign-in is kept through this many later starts, so the oldest is let go only past this many starts in its ten minutes (Sign-in, below) |
| `FRAGMENT_PROVIDERS` | the provider catalog a computer's swap offers (`fragment_core::catalog`; docs/computers.md, Connections and operator keys): a JSON list of `{name, kind: connection\|operator\|own, hosts, placements, env, price?}`, rendered from the config's `providers`; none by default. A malformed one is refused at the node's first request (the deploy checks it first). An operator key's price is the price book's `keys`; raise `FRAGMENT_PRICE_BOOK_VERSION` with every change to one |

Secrets Store bindings (docs/secrets.md): each a secret in the account's
Cloudflare Secrets Store, named by the deployment's config and bound by
`cargo xtask deploy` (wrangler's local store, seeded by devstack, in dev
and the e2e), read only by `cell/src/keys.rs`, each value used for a
minute at most before it is read again. The names are
`fragment_core::secrets_store`'s. An app's isolate gets an env the
platform builds, so no author code can name one:

| Binding | Meaning |
|---|---|
| `HOST_SECRET` | seals values at rest, per Durable Object (at least 32 bytes); the key computers' placeholders are tagged with is derived from it |
| `HOST_SECRET_PREVIOUS` | the host secret before a rotation, bound while it runs; values sealed under it open and come back resealed |
| `CODESTORAGE_KEY` | the org's PKCS#8 P-256 key, which signs code.storage tokens (a one-line PEM may carry literal `\n`) |
| `WORKOS_CLIENT` | sign-in: fragment's WorkOS environment's client id; unbound, sign-in answers 500 |
| `WORKOS_KEY` | WorkOS's API key, for its code exchange and Pipes |
| `OPERATOR_KEY_<NAME>` | an operator key of the catalog (`perplexity` is `OPERATOR_KEY_PERPLEXITY`, `google-places` `OPERATOR_KEY_GOOGLE_PLACES`), the store secret its row's `key` names |

One Worker secret is left, the platform Worker's:

| Secret | Meaning |
|---|---|
| `FRAGMENT_TEST_SECRET` | the test levers' secret (Test levers, below): honoured on a branch deployment (`FRAGMENT_HOST_LABEL_SUFFIX`) or a local fleet (`FRAGMENT_EGRESS_LOCAL`) alone, 32 to 256 bytes of printable ASCII; one set on a deployment of its own, or one too short, is logged (`levers.refused`) and honoured nowhere. `cargo xtask deploy` uploads it from the config's `test_secret_file`, for a `--branch` deploy only (`.dev.vars` in the local e2e) |

## Test levers

`/api/test/*` (cell/src/levers.rs) exists only on a fleet with a test
secret, and answers only a request that carries it in
`x-fragment-test-secret` (compared in constant time: both sides are
hashed first). Without the secret configured, or without it on the
request, or with another, a lever is the 404 any missing route is,
word for word (`{"error":"not_found","message":"no route <path>"}`), as on
production. The local e2e makes a secret per run; a preview has one when
deployed with `test_secret_file`; the hosted e2e reads that file
(crates/e2e/src/hosted.rs). On a branch deployment (`Config::levers_scoped`)
the levers reach the e2e's own things alone: a fragment lever answers
only for a fragment labelled `e2e-…` (403 otherwise), a ledger lever or a
computer lever only for an e2e person's (403 otherwise), and the
registry's are no route (404); a local fleet's reach everything.

| Route | Body → answer |
|---|---|
| `POST /api/test/signin` | `{email, paidCalls?}` → `{session, identity, created, paidCalls}`: an e2e person's platform session (the `fragment_session` cookie's value). `email` is `<name>@e2e.test` (a name of 1-64 of `[a-z0-9._-]`; any other is 400); its person is keyed by it under the issuer `e2e.test`, which no real sign-in has, so they are never anyone WorkOS signs in. The first sign-in makes them (`created`), and each makes them a seat (one ledger command, `e2e-seat`) and caps their paid calls (model calls and AI steps, each a reservation) at `paidCalls` from now: 0 unless asked, at most 200; their ledger refuses the one past it (402 `budget_used_up`). On a branch a day (UTC) makes at most 1000 e2e people and lends them at most 2000 paid calls (429 past either) |
| `POST /api/test/people` | `{after?}` → `{people: [{identity, email}], next}`: the e2e people by identity, 100 a page (`next`: the identity to ask after, `null` on the last page); the hosted e2e's sweep signs each in again and deletes their fragments of one run (`e2e-<run>-…`), or with `--sweep-all` every `e2e-…` one at least an hour old (crates/e2e/src/hosted/sweep.rs) |
| `POST /api/test/ledger` | `{identity, op, …}`: a lever on that person's ledger: `clock {offsetMs}` moves its clock, `sweep` runs its sweep now, `entries {prefix}` lists its references under a prefix (at most 500), `totals` answers what moved its balance, and `paid-calls {max}` caps its paid calls from now (`{max, used}`) |
| `POST /api/test/keys` | `{fragment, op, plaintext\|sealed}`: seals or opens as that fragment |
| `POST /api/test/fragment` | `{fragment, op, …}` pulls a lever on that fragment: `fail-deliveries {times}` fails its next queue sends, `fail-outbox {times}` fails its next records' outbox writes just after their append, `fail-triggers {times}` fails its next trigger steps just before their last run starts, `drop-effects {times}` loses its next job step answers on their way back to the Workflow (after the step ran and its answer was kept), `forget-steps` forgets the kept answers of its runs in flight, `hold-advances {on}` holds each advance after a run's first step while on (at most 20 s), and `advance-held` answers `{run}`, the last run it held, `forget-live` makes it forget what it knows of its live sockets beyond their attachments (as waking from hibernation does), `age-live {ms}` makes every live socket's identity check `ms` older (as if that long had passed), `drop-live {code}` drops its live sockets, `ledger {ms \| null}` shortens (or restores) its operation ledger's window, `age {ms}` forgets its write keys as if `ms` had passed, `members {fill}` adds placeholder members until there are `fill`, `code-builds` answers `{builds}`: how many times the fragment's activation built its app's worker code for the loader, `alarm` answers `{alarmAt, pollAt, now}` (ms): when its alarm and its next poll are set for, `age-outside {ms}` makes the last sign its pins may lag (a storage token minted, a move that failed to follow) `ms` older, `fail-after-paid {times}` fails its next paid AI steps just after their call was paid and kept (so the step is tried again), `fail-meter-acks {times}` loses its next meter batches' acknowledgements (so the queue delivers them again), `meter-now {sample?, resend?}` closes every counted minute, takes a storage sample (unless `sample: false`) and sends its outbox's batch now (a waiting one again with `resend`), answering the outbox, `meter` answers the outbox, `forget-standing` forgets what it heard of its owner's standing (meter.rs), `cron-now` makes each of its cron schedules due at once (`{due}`: how many), so a test need not wait for a schedule's minute, `poll-now` makes its next alarm a poll pass (the blob collection's), `expire-draft` makes an unclaimed draft's end now (`{until}`: its alarm ends it; Drafts, below), `fail-cards {times}` makes its next card shots open a page nothing serves (`http://127.0.0.1:9/`, which Chrome refuses), so they fail as an unreachable page does, `cards` answers `{cards, failCardsLeft}`: its card and schedule as kept (`fragment_core::card::Cards`) and the shots the lever still fails, and `ended` answers `{ended: [{incarnation, name, stored, attempts, lists, repos}], dueAt}`: each life a delete ended whose cleanup is not done (`lists`: the members' lists it has still to tell; `stored`: 1 while its app's database or blobs remain; `repos`: 1 while a wipe's end of it has its repo still to delete), and when its next pass is due. `ended` alone answers on a name with no fragment (deleted, and not made again) |
| `POST /api/test/computer` | `{computer, op, times?, on?}` pulls a lever on that computer (docs/computers.md): `kill` → `{computer, killed}` sends SIGKILL to the guest's PID 1 (from outside its PID namespace), so its container exits as a crash does and its real exit is reported (`killed`: the start it was); one not running (asleep, or won't wake) is 400. `saves` → what its Computer DO keeps of its saves: `saves` (newest first, at most three: `{number, id, generation, atMs, held, records, unusable}`, each with its `DirectoryBackup` records), `numbered` (the last save's number), `snapshot` (`{id, image, save}`, the cache of a save for one image, or `null`), `restored` (what its last start that came up restored, as the view has it), `rollbacks`, `ended` (`{generation, by, saved}`: how the last life ended, until the next start that comes up reads it), `starting` (`{generation, from}`: the start under way), `running` (`{generation, image}`: the image's reference the last start that came up runs), `generation` (its lifecycle's last start), `saving` (the save under way, by its step: `{step: hold \| save \| stop, since_ms, …}`, or `null`), `unsavedSince` (since when a sleep's save has kept failing, or `null`) and `failSaves` (the lever's failures still to come). `fail-saves` with `times` (1 to 100) → `{computer, failSaves}`: its next that many saves fail before they start (a sleep's included, which then keeps its container). `always-on` with `on` → `{computer, alwaysOn, view}`: its owner's plan made always-on, or not (decision 25: the plan itself does not reach a computer yet). Another op, or one without its argument, is 400; a computer no one made is 404 |
| `POST /api/test/registry` | a local fleet's only: `{down}` makes the registry answer 503 (until it is set back, or the registry restarts), `{calls: null}` answers `{calls}`, how many calls the registry has had since it started (a test counts a request's round trips by the difference), `{hold: ms}` makes its next call wait that long (at most 10 s) before it is answered, while other calls go on, and `{signins: "count"\|"expire"\|"sweep"\|{expireSession: token}}` counts sign-in's rows (`{logins, redemptions, sessions}`), expires every pending sign-in and unspent redemption, runs its sweep now, or expires the one session a cookie's token names (a platform session's site sessions end with it) |

A request body is at most what the zone's Cloudflare plan takes (100 MB
on Free and Pro), below the 256 MiB a blob route allows (debt ledger).

Bindings (`cell/wrangler.jsonc`): `FRAGMENT`, `PRINCIPAL`, `REGISTRY` and
`LEDGER` (Durable Objects), `LOADER` (the Worker Loader), `JOBS` (the
Workflow that runs jobs), `BLOBS` (the deployment's R2 bucket: the bytes
of large files), `DELIVERIES` (the `fragment-deliveries` queue, and its
dead-letter queue `fragment-deliveries-dead`), `METERS` (the
`fragment-ledger` queue: each fragment's meter batches, consumed by the
cell), `AI` (Workers AI, through `AI_GATEWAY_ID`), and `BROWSER`
(Browser Rendering: preview cards, Cards below). A local node runs
without `AI`, which `wrangler dev` would only reach remotely: dev and the
e2e set `FRAGMENT_AI_URL`. `BROWSER` runs locally under `wrangler dev`
(its own local mode: a Chrome for Testing it downloads into its cache,
`$XDG_CACHE_HOME/.wrangler/chrome`, on the first shot; dev and the e2e
set `XDG_CACHE_HOME` to the repo's `target/cache`), a lower rung than
Cloudflare's browsers.


## Principals and access

A principal is an identity (`id:` + 32 hex: a person or an agent) or an
anonymous visitor (`anon:` + 32 hex, the hash of a random cookie on the
fragment's origin). Grants, records, runs, and the ledger name
principals. A request is signed by a key (64 hex inside, an npub in
answers; requests may use either); the router asks the registry which
identity holds it, live, on every signed request to the control API,
and passes the fragment both. A key no one registered, or a revoked one,
is 401; when the registry cannot answer, a signed request is 503
`registry_unavailable`, never let through (docs/finite-integration.md,
rule 7). Control routes need NIP-98; site requests may carry it. A site
request's signature (checked by the router: a bad one is 401) or session
cookie is resolved only when the answer depends on who is asking: a page
or a file that everyone who may see the fragment gets alike (a `public`
fragment's, or a `link` fragment's with its share link) is served
without asking the registry, so neither a revoked key nor a registry that
cannot answer changes it. An operation, a socket, an invite, push, the
owner's fragments, an app route, and any read the anonymous standing may
not see ask it, live, and are refused as the control API is (401, or 503
when the registry cannot answer). A fragment's own key is the principal
of its triggered runs and is not registered. A draft's maker (Drafts,
below) is the one key no one holds that a fragment admits: its own
draft's, until it is claimed.

NIP-98 (`crates/nip98`): `Authorization: Nostr <base64 of the event>`, an
event of kind 27235 with empty content and the tags `["u", <the absolute
request URL>]`, `["method", <the method>]`, and, when the body is not
empty, `["payload", <hex SHA-256 of the body>]`; `created_at` within 60
seconds of the cell's clock; `id` and a BIP-340 `sig` as NIP-01 defines.
Every field is required and typed (a tag is an array of strings). A
`payload` tag binds the body even when it is empty: a signature over a
body, sent without one, is 401. A blob upload's bytes stream past the
router, so its signature binds its URL, which names the bytes' hash (the
cell checks the bytes as they arrive): a `payload` tag may name that same
hash, or be absent, and one naming any other hash is 401. Every other
body the router reads
is at most 2 MiB, measured as it arrives: a longer declared
`content-length`, or a chunked body that runs past it, is 413 before
anything is authenticated.

Roles, weakest first: `public`, `viewer`, `contributor`, `editor`,
`owner`. A `contributor` (the share sheet's "Use") calls the operations
and posts to the channels declared for it, and does whatever a viewer
may; it holds none of the routes below that need `editor` (files,
deploys, secrets, the storage token, blobs, replay, pause), so a member
who writes an app's data cannot change the app. A member's
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

An agent acts for whoever asked, capped (docs/cloudflare-v1.md, R17). A request
signed by an agent may name an identity in `for=<id:…>` in its URL's
query (inside the signed URL, so the signature covers it): the request
acts with the lower of the role that identity holds in the fragment (its
membership, an agent of its own that is a member, or the visibility
floor; never the share link, which the agent does not hold) and the
agent's cap: the agent's own role there, or its owner's membership role
if higher, and never above `editor`. The owner's part is the owner's own
role, so an agent never reaches further than its owner could. The
principal is still the agent (records, runs, and the ledger name it),
but an operation it calls `for` someone tells the app so: `call.principal`
is them and `call.agent` the agent, so what it does for them is theirs.
`for` is honored on a fragment's routes (`/api/f/{name}/…`) and on `GET`
and `POST /api/fragments` (a `POST` for anyone but the agent's owner is
403: what an agent makes is its owner's); on any other route it is 400.
A person's request naming `for` is 403, as is more than one `for` (400)
or one that is not an identity (400). A call without `for` acts as the
agent's own membership. A site request's query is its app's: `for`
there means nothing to the platform.

An agent shares for its owner (Paul, 2026-10-04: "your agent can share
on your behalf"; `fragment_core::access::agent_shares`). Sharing is
members (other than leaving with `DELETE members/me`), invites,
visibility, and rotation. An agent acting `for` its own owner, and not
held below them, shares a fragment its owner owns as its owner would,
under the same rules (it grants viewer, contributor, or editor, never
owner, and never changes or removes the owner). Every other agent's
sharing request is 403, saying why: one acting as itself or for anyone
else, one its owner holds (at any hold), and one on a fragment its owner
does not own (an editor's agent shares nothing there). Deleting a fragment and setting
its cap are 403 for every agent, whomever it acts for. What an agent
shares names it: the member's `addedBy` and the invite's `createdBy` are
the agent, and each change's event (`member.set`, `member.removed`,
`invite.created`, `invite.revoked`, `visibility`, `tokens.rotated`)
carries `by` (who made it) and, for an agent, `for` (its owner), and its
summary says "(an agent, for …)", so the owner sees what their agent
shared.

Membership is cell state: `fragment.json`'s `visibility`, `editors`, and
`viewers` grant nothing (the cell records a `manifest.ignored` event).
Only the owner, or their agent sharing for them, manages members,
invites, visibility, and tokens; a member may leave. Each identity's
list of fragments is kept in its `Principal` cell, fed from each
fragment's outbox.

## Identities

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
| `PUT /api/identities/{agent}/held` | the agent's owner | `{held: "viewer" \| "contributor" \| "editor" \| null}` → the agent: held below its owner (decision 36), it acts with at most that for whomever it acts, wherever its owner's access reaches; its own memberships (its chats, its agent fragment) are not held, so a held agent still answers and records its turns there; `null` lets it go; anyone else 403, `owner` or `public` 400 |
| `PUT /api/identities/me/username` | a person | `{username}` → `{username, claimed}`: chosen once (3 to 32 of lowercase letters, digits, and single dashes, not starting or ending with one, and not a reserved word); taken 409, another after yours 409, yours again `claimed: false` |
| `PUT /api/identities/me/picture` | a person with a username | the image (PNG, JPEG, WebP, or GIF, told by its bytes; at most 256 KiB) → `{sha, mime}` |
| `GET /api/users/{username}` | anyone | → `{id, kind, username, picture}` (`picture`: its URL, or null) |
| `GET /api/users/{username}/picture` | anyone | the picture's bytes |
| `DELETE /api/users/{username}` | the fleet's operators | → `{username, identity, released}`: undoes a username taken by mistake, so its person chooses again; refused (409) while they own a fragment under it (its URLs name it) |

## Names (docs/cloudflare-v1.md, R16)

A person chooses a **username** once (above; the platform's page asks
after the first sign-in, and `fragment username <name>` does too). A
fragment's **name** is `<label>.<username>`: `todo.futurepaul`,
served at `todo--futurepaul.<suffix>` (one DNS label, under the
suffix's one wildcard certificate), its code.storage repo
`todo--futurepaul--<12 hex>`, after the deployment's prefix (a branch's
`<branch>--`): the 12 hex are a digest of its owner's identity
(`fragment_core::codestorage::repo_name`, the one place a repo's name is
derived; the create keeps the repo it made, and nothing rebuilds the
name). So the same owner making a deleted name again finds its repo, as
ever, and a username another identity holds later (after a wipe, or an
operator's release) never finds the repo its earlier holder made under
the same label: a wiped person's repos are deleted, and never made again
for the new person. A fragment made before 2026-10-07 keeps the repo it
was made with (`todo--futurepaul`).
A label and a username never contain `--`. Creating with a bare label
puts it under the creator's username; creating under someone else's is
403. In a signed request's path, a bare label names the signer's own
fragment (`/api/f/todo/status` is `todo.<your username>`); anything
unsigned (an inbox, a site) names it in full. A draft's name is
`<label>.draft` (Drafts, below): `draft` is reserved, and names no one;
a claimed draft keeps it, and is named in full.

## Operators: wiping a person

The deployment's operators (`FRAGMENT_OPERATORS`: identities, and keys)
wipe a person: everything that is theirs or their agents' is deleted, and
their username and sign-in are freed, so their next sign-in is a new
person. A key the variable lists is an operator's whether or not anyone
holds it: these routes take its NIP-98 signature without asking the
registry. **An operator key** is one no person holds (`fragment operator
key <file>` makes one: its secret in the file, 0600, never printed; its
npub goes in the deployment's `operators`). It is the key to wipe with: a
wipe ends the keys and sessions of whom it wipes, so it never removes the
key doing the wiping, and a wipe cut short is run again with it. An
operator known by their identity is refused wiping that identity (400).
Any other signer, or a browser's session, is 403; unsigned, 401.

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/people/{person}/wipe` | the deployment's operators | → `WipeReport` (`fragment_proto::wipe`): the dry run. It changes nothing |
| `POST /api/people/{person}/wipe` | the same | `{confirm, steps?}` → `WipeReport`: the wipe, as far as one call goes (20 s, `fragment_core::wipe::CALL_BUDGET_MS`; `steps`: at most that many steps). `confirm` is the identity the dry run answered: anyone else is 409, nothing changed (so a username that changed hands between the two never wipes the wrong person); none is 400. Called again, it goes on where it stopped; on a person wiped, it finds what is left (nothing) |

`{person}` is a username or an identity (`id:…`). An agent is 400 (it is
wiped with its owner); no one is 404 (a username a finished wipe freed
names no one; its identity still answers, `wiped`).

`WipeReport` is `{identity, username, state, next, found, ran, done}`:
`state` is `live`, `wiping` (begun, locked) or `wiped`; `next`, the step a
wipe runs next; `ran`, the steps this call ran, each `{step, done,
deleted, note?}` (`note` says what a step skipped, waits for, or why it
failed this time: the next call tries again); `done`, wiped with nothing
left. `found` is what the person and their agents hold now: `{signIns,
keys, sessions, pictures, agents, fragments, memberships, computer,
ledger, lists}`, `fragments` (theirs) and `memberships` (theirs and their
agents' in other people's fragments, `<fragment> (<role>, <identity>)`)
each `{count, names, more}` (at most 50 names), `computer` `{computer,
phase, saves, backups, more, snapshot}` (`backups`: the objects under its
saves' prefix in R2, one page's count), `ledger` whether their ledger
holds anything, `lists` the rows their and their agents' lists hold. A
dry run's is what a wipe deletes; a wiped person's, what is left.

**The steps** (`fragment_core::wipe::STEPS`, run by the router:
cell/src/wipe.rs), each idempotent and recorded done in the Registry only
once it is whole:

- *Begin*, in one Registry turn: the wipe is recorded and the person
  locked. Their and their agents' sessions end (a signed-in browser is
  out at once), their connected clients' connections and codes, and their
  keys (deleted, not revoked: the key is no one's, 401). Until the last step, a sign-in with their account is 403,
  nothing acts as them or makes an agent for them, no one adds them or
  their agents to a fragment (404), and their username stays theirs:
  no one takes it, and no fragment is made under it (no one can sign as
  them).
- `computer`: its container destroyed, every save deleted from R2 (each
  `DirectoryBackup` record, then whatever else is under its saves'
  prefix), and its record emptied, the snapshot's id with it. Cloudflare
  deletes no container snapshot: forgotten, it is never restored, and it
  expires within 30 days (docs/computers.md).
- `fragments`: each fragment they own (under their username, or a draft
  they claimed, on their or their agents' lists, and the agent fragments
  the registry names) ends
  as a delete ends it (`DELETE /api/f/{name}`, below), its code.storage
  repo recorded beside its end to delete; one someone else owns is
  skipped and named.
- `memberships`: they and their agents leave each fragment of someone
  else's they are in, as a member leaves (their membership, their
  subscriptions there), and the browser pushes they subscribed there go.
  Nothing else of that fragment's changes: what they wrote there stays
  its owner's, and its other members, someone else's agents among them,
  stay.
- `cleanup`: until each ended fragment's cleanup is done: its members'
  lists told, its app's database, its blobs, and its repo deleted
  (code.storage deletes softly, then its storage on its own).
- `ledger`: emptied in one step; it keeps one row saying it was wiped and
  refuses everything after (404, `Refused::Wiped`), so a meter batch still
  in the queue is acknowledged and kept nowhere.
- `lists`: their and their agents' lists (`Principal`s) emptied the same
  way: a change still on its way from a fragment is taken and kept
  nowhere.
- `pictures`: their picture's bytes, unless another identity set the same.
- `registry`, in one turn: every row naming them or their agents
  (identities, keys, sign-in subjects, username, picture, agent fragments,
  sessions, consents). The wipe's own row stays: the identity, its times,
  and the operator.

Their next sign-in with the same account finds no subject: a new person,
a new identity, so a new list, ledger and computer (each is named by it),
whom the platform's page asks for a username (theirs is free again) and
makes a default agent for. Their ledger starts on `FRAGMENT_DEFAULT_PLAN`:
no grant, plan or seat of the old one's carries over. A fragment made
under a label the old person used gets a new repo (Names, above).

**What a wipe leaves**, as not the platform's to delete or not theirs
alone: their WorkOS user, and its Pipes connections, keyed by it (the new
person, signed in with the same account, finds them connected: they go at
WorkOS); the computer's container snapshot, until Cloudflare expires it
(30 days, never restored); the history Cloudflare keeps of their jobs'
Workflow instances, and its Workers logs, for their retention;
code.storage's soft-deleted repos, until it cleans them; what they wrote
in other people's fragments; a push their browser subscribed to on a
fragment they were not in (a public one), until its push service says it
is gone; and the wipe's row. A fragment none of their lists names (its
owner's list never took its create) is not found (the debt ledger).

The CLI: `fragment operator key <file>`; `fragment operator wipe <person>
--dry-run`, and `--yes`, which calls until the wipe is done (each call
goes on where the last stopped), `--key-file <file>` (or
`FRAGMENT_OPERATOR_KEY_FILE`) naming the operator key; `--json` answers
the report (`{report, calls}` for `--yes`).

## Sign-in

A person is keyed by their verified `(issuer, subject)`: the issuer is
`workos:<client id>` (the environment), the subject WorkOS's user id. The
email is an attribute, refreshed at each sign-in and never matched. The
platform holds no key for a person; browsers hold sessions, each looked up
live in the registry on every request whose answer depends on who is
asking (Principals and access, above; a 30-day lifetime; tokens are 32
random bytes, the registry keeps their SHA-256).

| method & path (platform origin) | what |
| --- | --- |
| `GET /`, `GET /settings` | the shell's page, for anyone (The shell, below): signed out it asks them to sign in, without a username it asks for one; at `/` it opens their mind, at `/?apps` their chats and apps, at `/settings` its settings |
| `GET /auth/login?return=&login_hint=` | → WorkOS's authorize URL (`provider=authkit`, `redirect_uri` `<platform>/auth/callback`, a state); the state is bound to the browser by `fragment_login` (HttpOnly, SameSite=Lax, `Path=/`, ten minutes) |
| `GET /auth/link?return=` | the same from a signed-in browser: the sign-in that comes back joins this person (409 when it is someone else's) |
| `GET /auth/callback?code=&state=` | the state must match the browser's cookie (400 otherwise); the code is exchanged server-side; → `fragment_session` (HttpOnly, SameSite=Lax, `Path=/`) and back to `return`; a WorkOS `error` is shown (400); a sign-in already finished or past its ten minutes, or a code WorkOS refuses (a callback sent again), is 400 `invalid_request` |
| `POST /auth/logout` | ends the session and every fragment session made from it, clears the cookie, and sends the browser to WorkOS's logout (`session_id` from the access token's `sid`); from another origin, 403 (`GET` shows the button) |
| `GET /auth/fragment?name=&return=` | signed in: → `<fragment origin>/__signin?token=<a single-use redemption, 60 s, for that fragment only>` (a session holds at most 16 unspent; past that, the oldest is refused), at once for a fragment of the person's own, one shared with them, or one they said yes to; for any other, first a page asking "Continue to X?" (Asking first, below); signed out: → sign in first |
| `POST /auth/fragment?name=&return=` | that page's form (`form`, its token): the yes, remembered, then → the fragment's `__signin?token=` (303); another origin, or a missing or stale token, 403 |
| `GET /auth/frame?name=&return=` | a frame of the platform's own page (the shell's tabs; Frame sessions, below) signs in to the fragment: → its `__signin?token=<a frame redemption>` (`Cache-Control: no-store`, `Referrer-Policy: no-referrer`) where `/auth/fragment` would redeem at once; where it would ask first, or no one is signed in, a note in the frame. Anything but a frame of the platform's own page (by Fetch Metadata) is 403 |
| `GET /cli?key=<npub>&proof=` | the link `fragment login` prints: `proof` is the key's own NIP-98 event for `POST <platform>/cli/approve`, good for ten minutes (the proof of possession; without it, stale, or by another key: 400). Signed in: a page showing the key's last eight characters, to compare with the terminal, and an Add button; signed out: → sign in first, keeping the link |
| `POST /cli/approve` | the page's form (`key`, `proof`): the key joins the signed-in person at once; a key someone else holds, or a revoked one, is 409; another origin 403; the CLI waits for `GET /api/identities/me` to answer. People themselves come only from sign-in (`POST /api/identities {kind: "person"}` is 400) |

On fragment.club the platform is cross-site from every fragment
(`fragment.club` and `<label>--<username>.fragment.boats`), so its
SameSite=Lax session cookie reaches a fragment's page only on a
top-level visit. A fleet whose platform shares the fragments' domain
(a `FRAGMENT_PLATFORM_URL` on the suffix's site) puts them on one site, where the cookie
rides along on a fragment page's form, fetch, or frame. Either way every
page here answers `Content-Security-Policy:
frame-ancestors 'none'` and `X-Frame-Options: DENY` (no page may frame
one and lay its button under a click; its redirects still run in a
frame, but a frame's `__signin` refuses what they mint), but for the
share sheet and `/auth/frame`'s notes, which only the platform's own
pages may frame (`frame-ancestors 'self'`, `X-Frame-Options:
SAMEORIGIN`), and `Cross-Origin-Opener-Policy:
same-origin` (a page that opens one in a window of its own is severed
from it: its handle reads `closed`, and can neither navigate nor message
it), and every form here (`/auth/username`, `/auth/logout`,
`/auth/fragment`, `/cli/approve`, and Sharing's below) is 403 from another
origin, a fragment's page included. A browser sends `Origin` with every
POST (`null` from a page that hides its referrer), so a POST without one
is no browser's.

Over https every session cookie whose path is `/` is named with the
`__Host-` prefix (`__Host-fragment_session`, `__Host-fragment_login`,
`__Host-fragment_site`) and read under that name only: a fragment's page
may set a cookie for all of the suffix's domain, but never a `__Host-`
one, so none can stand in for the platform's session or another
fragment's.

On a fragment's origin, `GET __signin?token=` redeems the redemption for
this fragment only (another fragment's is 401 and stays unspent) and sets
`fragment_site` (HttpOnly, SameSite=Lax, host-only, `Path=/`). A frame redemption (Frame sessions, below) is redeemed
only in a frame, and any other only outside one: shown to the other kind of
page it is 401, and spent. Without a token, `__signin` starts at the
platform, unless this origin's session is live already (then straight
back), and only as a navigation of a page: from an image, a script, or a
fetch it is 403, and in a frame it answers a small page offering the
fragment in a tab of its own (it posts `{fragment: "signin-blocked",
name}` to the page around it). A request with that cookie is its person,
exactly as the same request signed by one of their keys: the same
decision either way. A cookie for another fragment, or whose platform
session ended, is nobody. A platform session keeps its newest 4
top-level sessions and its newest 4 frame sessions on each fragment (the
oldest of each ends). `GET __signout` is a page with a button; `POST
__signout`, from the fragment's own page only (`Origin`; any other is
403), ends the sessions the request's cookies carry (top-level and
frame) in the registry, forgets the person's yes to the fragment (Asking
first), and clears both cookies: a copy of either is nobody from then
on. The cookies are cleared whatever the registry answers; when it
cannot end the sessions, the failure is logged and a copy lasts until
the session expires or its platform session ends (`/auth/logout`).

### Which cookies count

Every fragment's origin is one site with the others (all of them are
under `fragment.boats`, which the Public Suffix List does not list), so
a SameSite=Lax cookie rides along on another fragment's images,
scripts, fetches, and forms. The router counts a browser's cookies on
a fragment's origin by the Fetch Metadata it sends
(`Sec-Fetch-Site`, `-Mode`, `-Dest`, which no page's script sets); a
request whose cookies do not count is served as to a stranger:

| the request | `fragment_site`, `fragview`, `fragment_anon` | `fragment_frame` |
| --- | --- | --- |
| the fragment's own page's (`same-origin`) | count | count |
| a top-level navigation (`navigate`, `document`), GET or HEAD, from anywhere | count | no |
| a frame's navigation (`iframe`, or an `object` or `embed`), GET or HEAD | no | count |
| anything else: an image, a script, a fetch, a form or any POST from another page | no | no |
| a socket | count when it names its page (`Origin`, which must be the fragment's own) | the same |
| no Fetch Metadata (a browser from before 2023, or not a browser) | count | no |

Every navigation's answer carries `Vary: Sec-Fetch-Dest`. A frame's
navigation is answered `Cache-Control: private, no-cache` and a
`Content-Security-Policy: frame-ancestors` naming who may show it:

- signed in by a frame session: `<platform>`, the platform's exact
  origin, the page its mint was for (a frame session made for any other
  page is no one's);
- from the fragment's own page (`same-origin`), as whoever its cookies
  name: `'self'`;
- any other frame, whose answer is a stranger's (no cookie of the
  person's counts there): `'self' <platform>`, so the shell shows a
  public fragment to someone signed out, and no other page may lay it
  under a click.

A fragment shows signed in only in the platform's page, and in another
fragment's page not at all. A signed request (the CLI's, an agent's)
carries no cookies and is unchanged.

These rules do not wait for the Public Suffix List. Listing
`fragment.boats` there would make browsers keep fragments apart
themselves: no cookie of one sent to another, `Domain=fragment.boats`
cookies refused, and storage and processes split per fragment. But the
list declines projects that serve fewer than thousands of people,
reviews take weeks to months, browsers pick a change up only in their
own releases, and a listing is slow to undo. So it comes last, once
fragment.boats serves that many. Its requirements (a `_psl` TXT record
kept for as long as the entry is listed, more than two years of
registration, a role address, and an abuse contact) are in
docs/fragment-boats.md at the tag `celld-final`.

### Frame sessions

A frame session is a fragment's session in a frame of the platform's
own page (the shell's tabs: docs/cloudflare-v1.md, decision 6), bound to
the platform's origin. A frame cannot sign in through the platform as a
tab does: a browser sends no SameSite=Lax cookie into a cross-site
frame, and Safari blocks every third-party cookie. A page's own code
must never hold a sign-in token either. So platform code mints the
session for a frame of the platform's page alone, and the frame keeps
it in a partitioned cookie (CHIPS):

- `GET <platform>/auth/frame?name=&return=` mints it, for a frame of the
  platform's own page only: its Fetch Metadata, which no page's script
  sets, must be a frame's navigation (`Sec-Fetch-Dest: iframe` or
  `frame`, `Sec-Fetch-Mode: navigate`) that a page on the platform's
  origin started (`Sec-Fetch-Site: same-origin`), on the platform's own
  origin. A page on any other origin (a fragment's page, its author's
  code, framed in the shell or anywhere) sends `same-site` or
  `cross-site` and is refused (403); so is a top-level visit, a fetch, an
  `object` or `embed`, and a request without Fetch Metadata.
- Signed in, it follows `/auth/fragment`'s consent: on the person's own
  fragments, those shared with them, and those they said yes to, a frame
  redemption (single-use, 60 s, for that fragment only, its embedder the
  platform's origin; a session holds at most 16 unspent) and → the
  fragment's `__signin?token=` (`302`, `no-store`, `no-referrer`). The
  platform's page never sees the token: it cannot read the URL its
  cross-origin frame was sent to.
- Otherwise a note in the frame, which only the platform's own pages may
  frame: on any other fragment, "Open X in a tab" (to `/auth/fragment`,
  where the question is asked: it is never shown in a frame); signed
  out, "Sign in". The note posts `{fragment: "signin-blocked", name, why}`
  to the platform's origin alone, `why` being `consent` or `signed-out`.
- The fragment redeems a frame redemption at `__signin?token=` only in a
  frame's navigation (shown to a top-level page it is 401, and spent);
  there it becomes `fragment_frame` (`HttpOnly; Secure; SameSite=None;
  Partitioned`, `__Host-` over https): kept by browsers that block
  third-party cookies (CHIPS), in the platform page's partition only.
  The frame then goes to `__signin?check=frame&return=`: with the cookie
  kept, on to `return`; without it, the page offering the fragment in a
  tab of its own, which posts `{fragment: "signin-blocked", name, why:
  "cookies"}` to the page around it (as a frame of `__signin` that no
  mint sent does).
- The frame's cookie counts only in frames and on the fragment's own
  page (above): it opens no top-level page.

### Asking first

Signing in on a fragment is silent on the person's own fragments and on
those shared with them (a member, or an agent of theirs is), which know
them already. Any other fragment learns who they are only once they say
yes on the platform's page ("Continue to X as @paul?": unframed, a form
token, a button that arms after 800 ms, as Sharing's pages are); until
then they are a visitor there. The yes is remembered for that person
and fragment (their newest 1000) until they sign out of it there
(`POST __signout`), which makes the next sign-in ask again.

### Opening a fragment by its URL (docs/cloudflare-v1.md, R4)

A browser's top-level visit to a fragment's page (a GET or HEAD
navigation whose `Accept` names `text/html`) that no session there
admits (401) goes to `<platform>/auth/fragment?name=<name>&return=<the
path and query it asked for>` (`302`, `no-store`): signed in to the
platform, it comes back signed in at once on the person's own fragments
and those shared with them, after asking first on anyone else's (above);
signed out, through sign-in first. A public fragment, or a share link
that opens it, serves as before. Any other refusal a browser navigates
to on a fragment's origin (a frame's navigation too) is a small page in
the platform's look with the refusal's own status and reason: "You need
to sign in" (a link to its `__signin`), "You don't have access to X"
(ask its owner; a link to sign out of it), "This link has changed" (a
`?view=` that opened nothing). From a frame its links open a tab. An
API call, a fetch, an operation, a socket, and any request whose
`Accept` names no HTML keep the JSON error. Only the
platform's refusals become pages: the fragment marks its own for the
router (`x-fragment-refusal`, dropped before any answer leaves), so an
app's own answer passes as it is.

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

## Sharing

The share sheet and accepting invites are the platform's pages, never a
fragment's: a fragment's page is its author's code (or an agent's), and
sharing grants. Each acts through the fragment's own routes (members,
invites, visibility, rotate, join; below) as the person signed in on the
platform, so the fragment decides who may do what.

| method & path (platform origin) | what |
| --- | --- |
| `GET /share/<name>` | the share sheet, a card laid out as a document's share dialog: who is in (usernames and pictures, from the registry's profiles) and their roles, to any member (anyone else, a 403 page); for the owner, adding people by username (an invite), the pending invites (revoke), each member's role menu (View, Use, or Edit: `viewer`, `contributor`, `editor`; or removing them; an invite's menu the same three), who can open it ("General access": Restricted `members`, Anyone with the link `link`, Public `public`), each menu sent as it changes; Copy link (the share link while it opens it, else its address) and Done (in a dialog, closes it, as Escape does; in a window of its own, closes that, or goes to `/settings`); then, quieter, a new share link. Signed out: → sign in first, and back. It reads nothing from its URL |
| `POST /share/<name>` | the sheet's form: `form` (the page's token), `action`, and its fields: `invite` (`username`, `role`: an invite for them alone, one use, seven days; answers the sheet with the `/join` link to send them), `role` (`member`, `role`: `viewer`, `contributor`, `editor`, or `remove`, which removes them), `remove` (`member`), `uninvite` (`invite`: its id), `visibility` (`visibility`), `rotate` (the share link only; the inbox's token is the CLI's). Done: → 303 back to the sheet; refused by the fragment (a member who is not the owner: 403): the sheet, saying why, with the refusal's status |
| `GET /join/<name>?token=` | what the invite grants (the fragment, the role, who invites), and a Join button; signed out: → sign in first, and back. An invite for someone else: a 403 page naming them; used, revoked, or expired: 404; the person is in already (at that role or above): a link to open it |
| `POST /join/<name>` | the page's form (`form`, `token`): joins as the signed-in person, then → `/auth/fragment?name=<name>&return=/` (signed in on its origin, and there) |

Neither page (nor a draft's claim page: Drafts, below) can be driven by
a fragment's page. Every POST's `Origin`
must be the platform's (403 otherwise), and every POST carries `form`,
the token its page was made with: an HMAC, keyed by the session's own
token (the HttpOnly cookie, which no page's script reads), of what the
form does (`share:<name>`, `join:<name>`, `claim:<name>`) and when its page was made
(`fragment_core::form`). A form without it, another session's, one for
another fragment or page, one sent sooner than 800 ms after its page was
made, or one older than 12 hours is 403. Their buttons come disabled and
arm 800 ms after the page shows (again each time it is shown), so the
click that opened a page (a double-click's second half) cannot confirm
in it (the sheet's selects too). Both pages send no CORS headers (a
fragment's page cannot read them, so it never holds a form's token),
refuse every frame but the sheet's on the platform's own origin (the
shell's dialog: `frame-ancestors 'self'`, `X-Frame-Options: SAMEORIGIN`;
every fragment is another origin), sever their opener, allow scripts
and styles only inline and images only from the platform
(`Content-Security-Policy`), and keep their URL to the platform
(`Referrer-Policy: same-origin`: the join page's holds its invite).

## Connected clients

A chat client with no shell (Claude, ChatGPT) and any other MCP client
reaches fragment through MCP, as the person who connects it. The
platform is its OAuth 2.1 authorization server, as the MCP authorization
spec has it (2026-07-28; cell/src/oauth.rs, the registry's half
`cell/src/registry/oauth.rs`, the rules `fragment_core::oauth`). A
**connection** is one client acting as one person on one resource, an
MCP server of the platform's: a fragment's own (`<fragment
origin>/__mcp`) or the platform's (`<platform>/mcp`). It acts as the
person themselves, with their role there and nothing more (decision for
Paul; the alternative is an agent identity of theirs acting `for` them
under the agent cap, R17), and what it does names its client. Its tokens
are bound to its resource and reach no other. It **only reads** unless
its person let it change things too (the consent page's box, `writes`):
a client that only reads is offered, and may call, only what reads (a
fragment's described queries, the platform's verbs that read), and any
write it asks of a fragment by another way is 403.

| method & path (platform origin) | what |
| --- | --- |
| `GET /.well-known/oauth-authorization-server` | the metadata (RFC 8414): the platform's origin as `issuer`, the endpoints below, `code` with PKCE's `S256` only, public clients only (`none`), Client ID Metadata Documents, and `iss` in the answer (RFC 9207) |
| `POST /oauth/register` | a client registers (RFC 7591): `{redirect_uris, client_name?, …}` → 201 `{client_id, client_name, redirect_uris, grant_types, response_types, token_endpoint_auth_method: "none"}`, a public client of the code grant whatever else it asked. A redirect URI is https, or http on this computer (`localhost`, `127.0.0.1`, `[::1]`), without a fragment; 1 to 8 of them, at most 512 bytes each; a name of at most 80 characters (none: its first redirect URI's host). The newest 100 000 registrations are kept (`oauth::CLIENTS_MAX`); one past them registers again |
| `GET /oauth/authorize?response_type=code&client_id=&redirect_uri=&code_challenge=&code_challenge_method=S256&resource=&state=` | checked in order. The client: a registered one, or, when `client_id` is an https URL with a path, the metadata document there (CIMD), read now (at most 5 KiB in 5 s, no redirect followed, public addresses only), naming that URL as its `client_id`, its `client_name` and its `redirect_uris`; then `redirect_uri`, one of its own (exactly; on this computer, on any port: RFC 8252). A refusal of either is a page, never a redirect. Then the rest, refused back to the client (`error`, `error_description`, `state`, `iss`): `code` only, `S256` PKCE, and one `resource` (RFC 8707) naming an MCP server of the platform's (`invalid_target` otherwise). The whole query is at most 2031 bytes, so it survives a sign-in. Signed out: → sign in first (WorkOS, as the shell), and back. Signed in: a page asking "Connect X?", saying who it acts as, what it reaches, what the client calls itself and where it sends them back (a client sent back only to this computer is any program there, and the page says so), a box (unticked) to also let it change things, and Allow and Don't allow. `scope` is not read |
| `POST /oauth/authorize?…` | that page's form (`form`, its token for this client on this resource; `answer`: `allow` or anything else; `writes=yes` when the box is ticked): the same checks; a yes → 303 to `redirect_uri` with `code`, `state` and `iss`, the code's connection reading only unless `writes`; a no → `error=access_denied`. Sharing's protections: the platform's `Origin` (403 otherwise), a form token, buttons that arm after 800 ms, no frame |
| `POST /oauth/token` | `application/x-www-form-urlencoded`, its `client_id` always: `grant_type=authorization_code` with `code`, `redirect_uri` (the one asked with) and `code_verifier`; or `grant_type=refresh_token` with `refresh_token`. A `resource`, when named, must be the connection's. → `{access_token, token_type: "Bearer", expires_in, refresh_token}` (`Cache-Control: no-store`). A code is spent as it is read, whatever its answer; a refresh token is replaced by the one it answers. Refusals are OAuth's (`{error, error_description}`, 400; `invalid_client` 401) |
| `POST /oauth/revoke` | `token` (either) and `client_id`, as a form → 200 `{}`: the connection ends when the token is that client's; anything else changes nothing (RFC 7009) |
| `GET /api/oauth/connections` | a person (signed, or the shell) → `{connections: [{id, client, clientId, resource, writes, createdAt, expiresAt}]}` (proto's `Connections`), newest first; `writes`: its person let it change things |
| `DELETE /api/oauth/connections/{id}` | its person → `{ok, ended}`: its tokens are refused from the next request; another's, or none, 404 |

A code is good for 5 minutes, an access token for an hour, and a refresh
token for 30 days, renewed at each use: a connection its client does not
use for 30 days ends. A person keeps their newest 16 codes and their
newest 64 connections; past either, the oldest goes. The registry keeps
each token's SHA-256 only, and sweeps expired codes and connections on
its alarm. A wipe ends a person's connections as it begins (Operators).
These endpoints answer no CORS: a client is a program or a server.

### A fragment's MCP server

Each fragment serves its described operations as MCP tools at its own
origin (cell/src/mcp.rs; the protocol's envelope `fragment_core::mcp`),
beside `__op`, for a client connected to it. The CLI serves the same
tools over stdio, by the same rules and code (`fragment mcp <fragment>
[--write]`: cli/GUIDE.md, "Use it from another agent"), signed as every
CLI call is; `--write` is a connection's box.

| method & path (a fragment's origin) | what |
| --- | --- |
| `GET /.well-known/oauth-protected-resource/__mcp`, `GET /.well-known/oauth-protected-resource` | its protected resource's metadata (RFC 9728): `{resource: "<origin>/__mcp", authorization_servers: [<platform>], bearer_methods_supported: ["header"], resource_name}` |
| `POST /__mcp` | one JSON-RPC message, with `Authorization: Bearer <access token>` → one JSON object (`application/json`; no stream). Without a token, 401 with `WWW-Authenticate: Bearer resource_metadata="<origin>/.well-known/oauth-protected-resource/__mcp"`, where a client starts its authorization; a token that is not live, or is another resource's (another fragment's, the platform's), 401 with `error="invalid_token"`. A request with an `Origin` (a page's, its own included) is 403; cookies count for nothing; `GET` and `DELETE` are 405 |

Both eras of the spec are spoken. A modern request (2026-07-28) carries
its version in `params._meta["io.modelcontextprotocol/protocolVersion"]`
and mirrors it, its method and its tool's name in `MCP-Protocol-Version`,
`Mcp-Method` and `Mcp-Name` (a mismatch is 400, -32020; another version
400, -32022, naming the supported); its results say `resultType:
"complete"`, and `server/discover` answers what it supports. A legacy one
(2025-03-26 to 2025-11-25) opens with `initialize` (answered with its
version, or the newest legacy one; no session is minted) and may `ping`;
its notifications are 202. The methods: `tools/list` and `tools/call`.

- **`tools/list`**: its tools (`fragment_core::mcp::served`): the
  operations of the live code that have a `description` in
  `fragment.json` and take an object, that the person may call (their
  role, as `__op` decides it): its queries, and its mutations and jobs
  only when the person let the client change things. By name,
  deterministic. A tool's description is its operation's, and its
  arguments are the operation's input: its `inputSchema` is the
  operation's `input` (an object's; `{"type": "object"}` when it declares
  none). Annotations: a query `readOnlyHint`; a mutation or a job
  `destructiveHint`, not `idempotentHint` (each call is one of its own); a
  job also `openWorldHint`. A modern answer may be kept a minute (`ttlMs`,
  `cacheScope: "private"`).
- **`tools/call`**: a call of one of its tools, as `POST
  /api/f/{name}/ops/{op}` as the person with a fresh id: the same checks
  (visibility, role, schema, the overdraft, the public budget) and
  ledger. `structuredContent` is its answer, `{result, replayed}`;
  `content` is its result's `text` when that is a string (a view an
  operation rendered for a model: a mind's view, zoom, date), else the
  result as JSON. A refusal the model may act on (a role, a schema, a
  budget) is the result, `isError: true`, its text `<error code>:
  <message>`; an operation that is no tool of this client (none, not
  described, or one that writes for a client that only reads) is -32602,
  saying why, and the platform's own failure -32603.
- **What it does names the client**: each mutation or job a connected
  client runs (not its replays) appends `client.called` to `events`,
  `{op, id, principal, client, connection}`, "add mcp-… by id:… through
  Claude". Its records, runs and ledger name the person, as theirs. Any
  other write a client's request makes on a fragment (the platform's
  tools, below) appends `client.acted`, `{method, route, principal,
  client, connection}`, "POST /api/deploy by id:… through Claude".

### The platform's MCP server

The platform's verbs as tools at `<platform>/mcp`, for a client
connected to it (resource `<platform>/mcp`; its metadata at
`/.well-known/oauth-protected-resource/mcp` and
`/.well-known/oauth-protected-resource`): the CLI's daily loop, each tool
one route of the API above, asked as the person (cell/src/mcp.rs; the
tools and their routes `fragment_core::mcp::verbs`). Its envelope, its
eras, its refusals and its 401s are a fragment's server's (above). A
client that only reads is offered the verbs that read (`list`, `status`,
`files`, `read`, `members`, `events`); another is -32602 to it.

| tool | arguments | the API's route |
| --- | --- | --- |
| `list` | | `GET /api/fragments` |
| `create` | `label`, `template?` (the API's: `blank`, `todo`, `inbox`, `calories`, `when`, `wall`, `board`, `watch`, `brief`, `hook`, `wiki`, `split`), `visibility?` | `POST /api/fragments` |
| `status` | `name` | `GET /api/f/{name}/status` |
| `files` | `name` | `GET /api/f/{name}/files` |
| `read` | `name`, `path` | `GET /api/f/{name}/file?path=` → `{path, text}`: text only, at most 1 MiB (anything else is refused, to be read with the CLI) |
| `write` | `name`, `files` (`[{path, text} \| {path, delete: true}]`), `key`, `message?` | `POST /api/f/{name}/files` (once by its `key`) |
| `deploy` | `name`, `note?` | `POST /api/f/{name}/deploy` |
| `members` | `name` | `GET /api/f/{name}/members` |
| `share` | `name`, `member` (a username or an identity), `role` (`viewer`, `editor`, `remove`) | `PUT` or `DELETE /api/f/{name}/members/{id}` |
| `visibility` | `name`, `visibility` | `PUT /api/f/{name}/visibility` |
| `call` | `name`, `op`, `id?`, `input?` | `POST /api/f/{name}/ops/{op}` (no `id`: a fresh one) |
| `events` | `name`, `tail?` (1 to 500, 30) | `GET /api/f/{name}/events?tail=` |

A `name` is `<label>.<username>`, or a bare label for one of the
person's own. A route's answer is the tool's `structuredContent`; its
refusal the tool's `isError` result, as a fragment's server answers one.

### Inline views (MCP Apps): not yet

MCP Apps (the `io.modelcontextprotocol/ui` extension) would show a
fragment's page in the chat: a tool names a `ui://` resource, an HTML
document the host renders in a sandboxed frame of its own origin, whose
calls go to the host over `postMessage` and reach the server as tool
calls. A fragment's page does not fit that frame as the frames model
stands (Frame sessions, above), so none is offered yet. What it needs:

- **Its calls through the host.** The view has no cookie of the
  fragment's origin and no frame session (`/auth/frame` mints for the
  platform's own page alone); the host holds the connection's token. The
  browser library (`__fragment.js`) needs a mode in which `call` and
  `post` are `tools/call` over the host's bridge, as the person, through
  this server.
- **Its assets.** The document is served as a resource, not from the
  fragment's origin: its relative URLs, `__fragment.js` included, resolve
  nowhere. Either the platform inlines the page's scripts and styles into
  the resource, or the view loads them from the fragment's origin (its
  `resourceDomains`), which serves a `members` fragment's site to its
  members' sessions only.
- **Its live views.** `__live` takes a socket only from the fragment's
  own page, with its own cookies (Serving, above): the view has neither.
  Either `live` re-runs its queries by polling over tool calls (no
  channels, no presence), or the socket takes a credential minted for
  one view (the host's `connectDomains` and a stable view origin,
  `_meta.ui.domain`), a new way into the frames model to decide first.


## Drafts

A **draft** is a fragment made before anyone has an account (issue
#232): a key no one holds makes it, it spends nothing, so it bills no
one, and it ends a day after it was made unless a person claims it. Its
claim is the login flow: the person signs in, gives its claim code, and
approves the key that made it, and in that step it becomes theirs. The
rules are `fragment_core::drafts`; the cell's half is cell/src/drafts.rs.

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/drafts` | a key no one holds (NIP-98) | `{template?}` (`blank`, `todo`, `inbox` or `calories`; any other 400) → `Created` with `draft: {expiresAt, claim}`: the draft its key names, `<label>.draft` (12 characters of a digest of the key), at `link` visibility, owned by its maker (`id:` and 32 hex of a digest of the key: an identity the registry never holds), `claim` its claim page with its code (`<platform>/claim/<name>?code=XXXX-XXXX`). The same key again answers the same draft (one key, one draft; one that ended is made again, a new life), and with another template is 409 `conflicting_body`; a key someone holds, or held, is 409; past its address's day or the deployment's (below), 429 |
| `GET /claim/<name>?code=` | a person, signed in (signed out: → sign in first, as the deployment has it, and back) | what claiming does, the ending of the key that made it, and its code asked for (filled in from the link); claimed by them, it says so; by someone else, 409; ended, 404 |
| `POST /claim/<name>` | that page's form (`form`, `code`) | a claim is a create: the person's ledger is asked first (a guest's is 403, past the overdraft 402); then, in one turn of the draft, they are its owner, its maker no member, it is on their list, and every limit below lifts (`draft.claimed` in `events`); then the key that made it joins them, as `/cli/approve` adds one → `/auth/fragment?name=<name>&return=/` (303). Another code is 403; claimed by someone else, 409; theirs already, the same again (the key approved again); a key someone else holds stays theirs (the page says to run `fragment login` where it was made) |

- **Its maker.** On a draft's routes (`/api/f/<label>.draft/…`, blobs
  aside) a request signed by a key no one holds is its maker, which the
  draft admits only if the key made it and no one has claimed it (401
  otherwise); every other route takes it as a key no one holds (401), and
  so does its site, where its share link opens it to anyone (`__op`,
  sockets).
- **What it takes** (`drafts::takes`): its status, manifest, members, card,
  events and triggers; its files (read, `POST files`) and deploys, and
  `refresh`; operations; channels (reads, posts, drafts); runs (replay,
  pause); the inbox; and its delete. Every other route of its control API
  is 403 until it is claimed: secrets, the storage token (so `fragment
  sync` and `deploy --dir`), blobs, subscriptions, members, invites,
  visibility, rotation, and its cap.
- **What it spends: nothing.** A job's `job.fetch` and AI steps fail for
  good, saying why (a replay after its claim runs them); its cron
  schedules start no runs (a run a minute, all day, is the one start its
  writes do not bound); a page takes no
  push subscription (`__push-sub` 403), so `call.push` reaches no one; its
  deploys get no card (`card.skipped`, `owner_pays`); no ledger is asked
  (its caps stand for its standing); no agent joins it, so no computer runs
  for it. Its meter rows (requests, storage) wait in its outbox, and go to
  its claimer's ledger with the rest.
- **Its caps** (`limits::DRAFT_*`): 60 writes a minute (operations that
  write, posts, file writes, deploys, inbox deliveries, replays; 429),
  2 MiB of files at `main` in all (413), and 16 MiB of its supervisor's
  database (records, runs, events; 507), beside its app's 16 MiB as any
  app's. An address (an IPv4 address, or an IPv6 /64, as Cloudflare's
  `CF-Connecting-IP` names it) starts 10 drafts in a day and the
  deployment 10 000 (429; a key's own counts once): the registry keeps each
  start a day.
- **Its page** says it is a draft: every HTML page of its site (of at
  most 1 MiB, as Open Graph tags take) carries a
  bar (`#fragment-draft`) saying when it ends, with its claim page's link
  (without the code: the page asks for it), and `Cache-Control: no-store`.
  The bar is the page's own markup, so its scripts could take it away, and
  an app's own answers (`App.fetch`) and its other files (an SVG,
  `__file`) carry none: it tells an honest draft's visitors what it is.
  What bounds an abuse of one is the rest: its day, its caps, its link
  visibility, and a name no one chooses.
- **Its end.** At `expiresAt` its alarm ends it as a delete ends it, and
  its repo with it, as a wipe's end does: a draft's repo is its life's
  alone (named for the fragment's own key, not its maker), and so is it
  when its maker deletes it. From then it is 404, and its key may make it
  again, a new life with a new repo. `status` answers `draft` while it is
  one (`claim` with its code for its maker).

A claimed draft is an ordinary fragment of its owner's, under its own
name: its links stay, and its owner names it in full (a bare label names
one under their username).

## Control API

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/fragments` | a person with a username, not a guest; an agent for its owner (the fragment is the owner's, under their username, billed to them, with its maker an editor)
 | `{name, visibility?, template?}`: `name` a label, or `<label>.<your username>` → `{name, npub, owner, visibility, viewToken, inboxToken, repo, canonical}` (`name` in full). Its maker's ledger is asked first (`Spend::Create`): a guest's create is 403 `forbidden`, "guests can't create fragments: …" (Paul, 2026-10-03: a fragment's hosting bills its owner, and a guest pays for nothing; a guest still edits fragments shared with them), however it is asked (a template's, an agent's for its owner, the shell's catalog), and nothing is made; past the overdraft it is 402 `budget_used_up` (the maker's fragments are read-only). A ledger that does not answer refuses none. `visibility` defaults to `link`. The fragment's own key is made in its cell and kept sealed for it. The cell creates (or, for a name its owner deleted before, finds) the code.storage repo, named for its owner (Names, above). With `template` (`blank`, `todo`, `inbox`, `calories`, `when`, `wall`, `board`, `watch`, `brief`, `hook`, `wiki`, `split`; any other is 400 and nothing is made), the template's files are main's first commit (its `fragment.json` stamped with the fragment's name) and live at once: the create answers once they are (one seed at a time: the alarm, armed during the create, seeds only a template still to land); one that fails to land is retried by the fragment's alarm (`template.failed` events). `chat`, `agent`, `brain` and `skills` are blessed (decision 40), named and not copied: main's first commit is `{"template", "meta": {title}}` (`title`, theirs alone), and the platform's release serves the rest (Apps; a brain: templates/brain/README.md). `notes` is the CLI's only (`fragment new --template notes`). |
| `POST /api/drafts` | a key no one holds | a draft: Drafts, above |
| `PUT /api/fragments/{name}/archived` | any signer, for a fragment they hold a role on | `{archived: bool}` → `{name, archived}`: the signer's own view of it (the shell leaves it out of its sidebar; search still finds it), kept in their list's row and nowhere else, so no one else's list or the fragment changes. The same again answers the same. A bare label names the signer's own; a fragment they hold no role on, or none of that name, is 404; a name that is none, or a body without a boolean `archived`, 400. It goes when they leave the fragment (back in, it is not archived), or the fragment is made again. Not honored for `for` |
| `GET /api/search?q=` | any signer | → `{fragments: [ListedFragment], messages: [{fragment, channel, seq, at, snippet}]}` (`SearchAnswer`): the signer's fragments whose title or label hold every word of `q`, then the messages that do, newest first, from fragments they hold a role on now, archived ones included (The shell, Search, below). `q` once, at most 256 bytes and 8 words (400 past either, or without it). Not honored for `for` |
| `GET /api/fragments` | any signer | → `{fragments: [{name, role, kind, title?, agents?, preview?, sharing?, archived?}]}` (`archived: true` on the ones the signer archived); `agents`: its agent members, the first added (a chat's lead) first, at most 16 (`LISTED_AGENTS_MAX`), as the fragment last sent them (an agent's joining or leaving sends every row; a row sent before rows named them has none until it is sent again); `preview`, a chat's only: the first line with words of its newest message the signer's search holds (Search, below), at most 160 bytes, none when it holds none; `sharing` on the signer's own fragments only: `{visibility, members, guests}` (guests: members who are neither the owner nor an agent of theirs), as the fragment last sent it with a change to its members or visibility; an agent's `?for=<id>`: the fragments that identity holds a role on where the agent or its owner is a member too, each with the role the agent acts with there for it (`fragment_core::access::listed_role`; a call decides again) |
| `GET /api/fragments/watch` | any signer; the shell with its session (below) | a WebSocket, upgraded; anything else is 400. It answers `{type: "hello"}`, then `{type: "changed"}` each time the signer's list changes: a fragment made, shared with them, changed (its title, kind, agents, sharing, their role), left or deleted, their archiving, and a chat's message new to their search (its preview) (principal.rs, Watching). A frame names nothing: the page reads `GET /api/fragments` again with its own credential, so a socket that outlives its session learns only that something changed. The platform session counts only on the platform's host with the platform's exact `Origin` (a browser names its page on every upgrade; a fragment's page, one site with the platform, is refused like no one: 401). A list holds `LIST_WATCHERS_MAX` (16) at once; one more is 429. It reads nothing from the client. Not honored for `for` |
| `DELETE /api/f/{name}` | the owner (never an agent) | → `{ok, deleted}` once the fragment is gone: from then it is 404 to everyone, its owner's list no longer has it, and its name can be made again. Its other members' lists, the app's database and the blobs go after, by the fragment's alarm (seconds; each part retried until done), so a delete answers as soon at `MEMBERS_MAX` members as at one: it tells at most one round of lists itself (32 at once). A fragment made again meanwhile under the name is untouched by the old one's cleanup. The repo stays |
| `GET /api/f/{name}/status` | viewer | → `{name, npub, owner, role, visibility, repo, pins: {main, live}, counts: {files, events, members}, code: {sha, id, operations: {<op>: {kind, role, input?, ephemeral?, description?}}, error}, viewToken, inboxToken (editor), urls: {canonical, platform}, blobMinBytes, page, draft?}` (`draft`: an unclaimed draft's, Drafts above); `code.sha` is the live commit installed and `code.id` the code that runs (`app:<hash>` of its `app.mjs` and `applib/`, or a blessed template's `blessed:<template>@<release>`); `urls.platform` is the platform's own origin, for links a person opens (a client in a computer calls an internal host); `page` is `{live, at, errors: [{kind, text, source}], dropped}`, what the page reported as its preview card's shot loaded it (Cards, below), absent before the first |
| `GET /api/f/{name}/manifest` | viewer | → `fragment.json` at main (404 when there is none) |
| `GET /api/f/{name}/members` | viewer | → `{members: [{principal, role, addedBy, addedAt, kind, owner?}]}` (`owner`: an agent member's) |
| `PUT /api/f/{name}/members/{id\|npub}` | owner, or their agent for them | `{role: viewer\|contributor\|editor, peopleOnly?}` → the member; a key names the identity holding it (404 when no one registered it). `peopleOnly: true` (decision 36): the share lends the member's agents nothing, so they act there only with memberships of their own. A new member that is an agent running on a computer is announced to it: `joined` on its agent fragment's `tasks`, and a wake (Computers, below) |
| `DELETE /api/f/{name}/members/{id\|npub\|me}` | owner, or their agent for them; or the member | → `{ok, removed}`; closes that member's change feeds (and its owner's, when an agent's membership was their only view) |
| `POST /api/f/{name}/invites` | owner, or their agent for them | `{role (viewer\|contributor\|editor), uses? (1), ttlS? (7 days, at most 30), invitee? (id:…)}` → `{id, role, usesLeft, expiresAt, createdBy, invitee?, token}`; the token is shown once. With `invitee`, only that identity may accept it (the share sheet's invite by username); without, whoever holds the token |
| `GET /api/f/{name}/invites` | owner, or their agent for them | → `{invites: [...]}` without tokens |
| `DELETE /api/f/{name}/invites/{id}` | owner, or their agent for them | → `{ok, revoked}` |
| `POST /api/f/{name}/join` | any signer | `{token}` → `{name, role, joined}`; a stronger existing role is kept; a fragment at its 1000 members is 400, and the invite keeps its use; an invite for another identity is 403, and keeps its use |
| `POST /api/f/{name}/join/preview` | any signer | `{token}` → `{name, role, invitedBy, invitee, expiresAt, current}`: what joining would grant (`current`: the signer's role now), joining no one; 404 for a token that names no open invite (the platform's `/join` page shows it) |
| `PUT /api/f/{name}/visibility` | owner, or their agent for them | `{visibility}` → `{ok, visibility}` |
| `POST /api/f/{name}/rotate` | owner, or their agent for them | `{scopes?: [inbox, view]}` → `{inboxToken, viewToken, rotated}` (`Rotated`): every token as it is now, and the scopes renewed; a new view token closes link holders' feeds |
| `PUT /api/f/{name}/secrets/{KEY}` | editor | raw body (at most 64 KiB) → `{ok, name}`; sealed (AES-256-GCM, key HKDF'd from the host secret and the fragment's npub) |
| `GET /api/f/{name}/secrets` | editor | → `{names}`; values never leave |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor | → `{ok, removed}` |
| `GET /api/f/{name}/storage-token` | editor | → `{token, repo, api, expiresAt}`: ES256, this repo, `git:read`+`git:write`, 15 minutes |
| `POST /api/f/{name}/refresh` | editor | → `{ok, refs: {main: {pin, moved} \| {absent}, live: ...}}`, once a live that moved is installed (`fragment deploy` asks it). A writer to the repo asks this after its push (the CLI does), and the platform follows its own moves itself: code.storage's push webhooks are not taken. A fragment reads the branches it has no pin for once, on its first request (a name made again keeps its repo); after that a move arrives by this or the poll backstop, and a site with nothing deployed answers 404 without asking code.storage |
| `POST /api/f/{name}/files` | editor | `{files: [{path, text \| base64} \| {path, delete: true}], message?, key?}` → `{commit}`: one commit to main, as a sync makes (at most 16 files, 400 past that, and 1 MiB of their decoded bytes in a body of at most 2 MiB, 413 past either; paths relative, no `.` or `..`). The same `key` from the same person answers the first commit again. A write of what main holds already commits nothing (code.storage makes no empty commit: its 412) and answers main's tip. Main's pin moves at once; live does not |
| `POST /api/f/{name}/deploy` | editor | `{note?}` → `{live, canonical}`: live to main's tip; `fragment deploy` asks it after its sync, so every deploy is this one (a first deploy makes the branch; later ones fast-forward it, and after a rollback take main's files whole: a restore commit, or one of their merge base and then the merge, since code.storage merges three ways), guarded against a live that moved meanwhile and made under the fragment's plane lock, so neither step's push pins the files live holds between them; the app installs from it at once |
| `GET /api/f/{name}/files` | viewer | → `{ref, files: [{path, size, mode, lastCommitSha, machinery, blob?, release?}]}` at main; a pointer's `size` is its bytes'. A fragment on a blessed template lists that template's data from the release beneath its own files (`release: true`, `lastCommitSha` `release:<hash of its bytes>`: templates/skills/README.md) |
| `GET /api/f/{name}/file?path=` | viewer | → the bytes at main (`x-fragment-ref`); a pointer's come from the blob store; with none of its own at `path`, a blessed template's data file from the release |
| `PUT /api/f/{name}/blobs/{sha256}` | editor | the bytes as the body (`content-length` required, at most 256 MiB), streamed through and hashed on the way in: → `{ok, sha, size, stored}`; bytes that hash to anything else are deleted and refused (400). Its `content-type` is what `__blob` serves it as, when that is passive media (Blobs, below) |
| `GET`, `HEAD /api/f/{name}/blobs/{sha256}` | viewer | → the bytes (ranges answer 206) |
| `GET /api/f/{name}/card` | viewer | → the fragment's preview card (Cards, below): a 1280×800 JPEG, `ETag` its blob's SHA-256, `Cache-Control: private, no-cache`, `X-Fragment-Ref` the live commit it shows; `If-None-Match` naming the tag is 304. Before the first is made, 404 `not_found` |
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
| `GET /api/f/{name}/channels` | viewer | → `{channels: [{name, read, post, signedIn, seq}]}`: `events`, `ops`, `inbox`, and the app's (`post`: who may post, or null; `signedIn`: only people signed in) |
| `GET /api/f/{name}/channels/{channel}?after=&limit=` | the channel's reader | → `{channel, records: [{channel, seq, at, principal, kind, body}], next}` (1000 a page) |
| `POST /api/f/{name}/channels/{channel}` | the channel's `post` role | `{id, body}` → `{record, replayed}` (`Posted`): the platform appends `body` (any JSON, at most 64 KiB of it; 413) as a record of kind `message` naming the poster, with no app code; it reaches sockets, subscriptions, and the channel's triggers as a mutation's record does. The same id and body again answer that record and append nothing (a retry also finishes what the first try left: its deliveries, its triggers' runs); the same id with another body, or on another channel, is 409 (ids are the poster's, kept as long as the record). A channel without a `post` role, and `events`, `ops`, and `inbox`, refuse posts (403); one that says `signedIn` refuses an anonymous poster (401). A poster holding only `public` spends a public call (Serving, `__op`) |
| `PUT /api/f/{name}/channels/{channel}/draft` | the channel's `post` role | `{turn, text}` → `{ok}`: a record its poster is writing, its whole text so far (at most 64 KiB), shown to the sockets following the channel at once (a `draft` frame on `__live`) and never stored; `text: null` stops it. The record its poster then writes with the same `turn` replaces it on a page. At most 10 a second across the fragment (429 past that: the poster's next carries the whole text anyway); `turn` is `^[A-Za-z0-9._:-]{1,128}$` |

## Apps

Code comes from git: when `live` moves, the cell reads `fragment.json`
and `app.mjs` (with `applib/**.mjs|js`, at most 64 modules and 4 MiB in
all) from the live commit. A live commit with an invalid `fragment.json`
keeps the last good code and says why in `status.code.error`.

A fragment on a blessed template (`chat`, `agent`, `brain`: docs/cloudflare-v1.md,
decision 40) names it in `fragment.json` (`template`) and runs the
platform release's manifest (with its own `meta` over the template's),
site, and code: the template's `app.mjs` and `applib/`, when it carries
any (a chat's push: docs/chat-records.md), held to an app's limits and
run in the same facet, under the release's identity (`blessed:<template>@
<release>`, a hash of the template's files,
`fragment_templates::blessed`; `status.code.id`). A platform deploy that
changes the template installs again at each such fragment's next request,
a fresh worker as a new commit is: what runs is the release, which its
commit does not name, and the `code.installed` event says when it changed. Its repo holds only its face and data: a live
commit that declares operations, channels, triggers or an
agent, or that carries `app.mjs` or `applib/` code of its own, keeps the
last good code and says to fork it in `status.code.error`. Forking makes
the code the fragment's own.

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
  `contributor` is for the people who write an app's data and not its
  code: they call it without the editor's routes (Principals and
  access, above).
  `constructor`, `fetch`, and `alarm` are not operation names (the App
  class's own; `fragment_proto::RESERVED_OP_NAMES`): a manifest naming one
  is refused at deploy.
- `description` (optional, 1 to 1024 characters) says what it does, to a
  model: `status.code.operations` shows it, and it makes the operation a
  tool of the fragment's MCP servers, its `__mcp` and `fragment mcp`, its
  description theirs (A fragment's MCP server, above). An operation
  without one is no tool.
- `input` is a JSON Schema in a bounded subset (`crates/core/src/schema.rs`:
  types, `enum`, `const`, lengths, ranges, `items`, `properties`,
  `required`, `additionalProperties`, counts; annotations allowed; any
  other keyword is refused at deploy). A call whose input does not fit is
  400 naming the JSON pointer (`input /text: is required`).
- `channels` declares the app's channels and their readers (default
  `viewer`); `events`, `ops`, and `inbox` are built in (readers: viewers).
  A channel may also name who may post to it, `"post": <role>`
  (docs/cloudflare-v1.md, R18): the platform appends a poster's record itself
  (`POST /api/f/{name}/channels/{channel}`, `fragment.post`), so a
  fragment whose live commit has channels and no `app.mjs` (a chat) runs
  no worker at all. A `post` role looser than the channel's `read` is
  refused at deploy (whoever may post may read). `"signedIn": true`
  (beside a `post` role) takes posts from people signed in only: an
  anonymous visitor holding the role (a link holder is a viewer) is 401
  `unauthenticated`, and nothing is appended. Such a channel keeps its
  newest 10 000 records (`limits::POSTED_KEPT`; the oldest go, whoever
  appended them, and with them their posts' ids), as `events` and `ops`
  keep theirs; the number is not declarable yet.
- `kind` is `query`, `mutation`, or `job` (below); a job's `role`
  defaults to `editor`.
- `triggers` (at most 32) start runs of an operation: `{"cron": "0 9 * *
  *", "run": op}` (five fields, UTC, 1 = Sunday), `{"channel": "inbox" |
  <app channel>, "run": op}` (each new record; with `"from": "person" |
  "agent"`, each record a member of that kind posted, so a record that
  starts no run counts toward no breaker: a chat's push, its agents'
  replies), `{"files": "notes/**",
  "run": op}` (a move of `main` changing a matching path; `*`, `**`, `?`,
  a trailing `/`). The operation must be a mutation or a job an editor
  may call.

`app.mjs` exports `class App extends DurableObject` with one method per
operation, each called `(input, call)`: `call.principal` (an identity,
`anon:…`, or the fragment's own npub for its triggered runs; for an
agent's call `for` someone, them, with the agent in `call.agent`, else
null) and `call.role`. A mutation is synchronous over the app's own
SQLite; `call.publish(channel, body, kind = "message")` appends a record
(body at most 64 KiB, 64 per mutation) once the mutation commits, and an
exception rolls back its writes and its records (422). The ledger keys a
mutation by (principal, id) for seven days: a retry with the same id
returns the stored result and applies nothing again; the same id with
another input is 409; after seven days the same id runs again, as a new
run. Every applied mutation also appends `{op, id}` to `ops`. An
operation may say `"ephemeral": true` (a mutation only; refused at
deploy on a query or a job): its calls leave no ledger row, no pending
row, and no `ops` record, so the same id runs again, with any input
(never 409 or a replay), and it may not publish, push, or write files
(422, rolled back: their outbox is the ledger row); its writes, the
role and schema checks, the 16 MiB cap, and the live views' change
signal stay (docs/MODEL.md). For a "latest value" write sent often,
whose ids would fill the database within the week. A query's
or a mutation's result is at most 1 MiB of JSON
(`limits::RESULT_MAX_BYTES`) of whole characters: a larger one is refused
in the app (422; a mutation rolls back), and the cell checks it again
(413).

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
method), with `x-fragment-principal` and `x-fragment-role` set; a
redirect it answers reaches the browser as it is (the platform follows
none: followed, it came back into the app at its `Location`).

The app's database holds at most 16 MiB: a mutation that would leave it
larger rolls back and answers 507 `storage_full`; the app still reads,
and deleting rows makes room. A write from anywhere else (a query, the
app's `fetch`) is not stopped: every check on the app's database runs in
the app's own code, a courtesy to it, not a wall (the debt ledger).
Before the author's constructor runs, the platform takes away the app's
alarm (it would wedge the facet), its async transactions and KV writes
(they would bypass a mutation's transaction and its cap), and facets of
its own (`cell/platform.mjs`); a call to one throws, saying why. The
Workers runtime refuses code generation from strings (`eval`, `new
Function`) and `Atomics.wait`, and holds the app to its CPU and memory
limits: a call past one fails as the app's (422; local workerd enforces
neither). When the app's facet is at its concurrency limit (the Workers
runtime's), calls into it answer 503 `node_full` and the rest of the
fragment works.

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

A page reads one of its fragment's blobs by hash at `__blob/<sha256>`
(Serving, below), with no row in the app's database and no record
holding the bytes; a job reads its head as text with `job.blob` (Jobs
and triggers, below). A channel record that names one in its body's
`attachments` (`[{ "sha256": … }]`, as a chat's records do:
docs/chat-records.md) keeps it while the channel keeps the record; one
nothing names, by pointer or record, is deleted after the grace period
like any other (uploads never committed or posted included). It is served as the
type its upload's `content-type` declared when that is passive media
(JPEG, PNG, WebP, GIF, MP4 and WebM video, MP3, WAV, and WebM, Ogg and
MP4 audio (a chat's voice memo), PDF: `blob::served_type`, its parameters
dropped),
else as `application/octet-stream`, so a blob never runs as a page or a
script there. An editor's page uploads one there too (`PUT
__blob/<sha256>`, `fragment.blob(file)`): a chat's attachments
(docs/chat-records.md).

CLI: `fragment blob put <name> <file>` uploads a file as a blob, typed
by its extension, and prints its sha256.

### Cards (docs/cloudflare-v1.md, decision 31)

Every move of `live` (a deploy, a rollback, a push to `live`, a
template's first deploy) wants a **preview card**: a screenshot of the
fragment's page, which the shell's Apps list shows. The schedule and its
rules are pure (`fragment_core::card`); cell/src/card.rs runs them.

- **Never in the deploy's request.** The move records the card it wants;
  the fragment's alarm, after the rest of its work, takes the shot itself
  with the `BROWSER` binding: a Browser Rendering session, CDP over its
  socket, the page at 1280×800 and a device scale of 1, its load waited
  for (at most 15 s), then 1 s for its scripts, a JPEG at quality 70 (40
  if that is over 512 KiB; over the cap at both, the deploy gets no
  card), and the session closed whatever happens (at most 45 s in all).
  It stores the image as one of its blobs (`image/jpeg`, kept while it is
  the card) and makes it the card.
- **Bounded per fragment: one shot out at a time, the newest live wins.**
  A fragment runs one alarm at a time. A deploy while a shot is out is
  shot next; the shot out lands stale and is dropped. Two deploys before
  a shot are one shot, of the second.
- **Retried with backoff, then quiet.** A failed try (no browser, a page
  that does not open, a socket that fails, a try lost with the
  fragment's object: each counts as failed from when it begins) is tried
  again after 10 s, doubling to an hour (a test fleet's
  `FRAGMENT_DELIVERY_RETRY_S` pins the wait), at most 5 tries in all.
  Then the live is
  given up: one `card.failed` event, and the card before stays.
- **As a visitor without an account sees it.** The renderer has no
  session of anyone's: a `public` fragment is shot as anyone sees it, a
  `link` fragment as anyone holding its share link sees it (the page's
  URL carries `?view=`, as a link holder's does; the card goes only to
  members, who already see that page), and a `members` fragment is not
  shot at all (`card.skipped`): a visitor would see only its refusal.
- **What the page reports.** The shot enables CDP's `Runtime` and `Log`
  on the page and hears, as it loads and in the second after (no
  clicks): uncaught exceptions and unhandled rejections (`exception`),
  `console.error` and a failed `console.assert` (`console`), loads that
  failed (`network`: a script, an image, a fetch; the page itself, "the
  page did not open: …"; not Chrome's own ask for `/favicon.ico`, made
  whether or not the page names one), and loads refused for security, a
  Content Security Policy's above all (`security`). It keeps the first
  10 and counts the rest (`dropped`), each `text` and `source` (the URL,
  with its line and column when Chrome gives them) cut to 1 KiB. The try that
  ends a live's tries (its card made, or given up) having opened the page
  makes what it heard the page's report: status's `page`, `{live, at,
  errors, dropped}`, its `errors` empty when the page reported none. A
  retry's and a stale try's are dropped, and a live not shot leaves the
  report before: `page.live` says which deploy it saw.
- **Apps only.** A chat or an agent fragment is not an app (the shell
  lists it elsewhere): its deploys are not shot (`card.skipped`). A
  brain is an app.
- **Metered to the owner.** Each try's browser time (acquire to close; a
  close that was lost adds the session's 10 s inactivity timeout) is a
  `browser` row in the fragment's meter outbox,
  `card:<fragment>@<incarnation>:<live, 12 hex>:<wanted at>:<try>`, billed
  to its owner at Browser Rendering's browser-hour price
  (docs/ledger.md). Before each shot the owner's ledger is asked as a
  create is: a guest pays for nothing and a read-only owner makes nothing
  new, so neither's deploys are shot (`card.skipped`), nor an unclaimed
  draft's, whose maker has no ledger; a ledger that does not answer
  refuses none.
- **Events:** `card.made` (`{live, blob, attempt}`), `card.skipped`
  (`{live, why: not_an_app | members_only | owner_pays}`),
  `card.failed` (`{failures}`), and `page.errors` (the report, when it
  has errors; its summary their count and the first one's first line).
  A retry and a stale shot say nothing.
  A page's Open Graph image
  stays `__preview.svg`: the card is its members'.

### Deliveries: web push

A page subscribes with `await fragment.push.register(who)` (from a click:
it registers `./__sw.js`, reads the fragment's VAPID key from
`./__push-key`, and stores the subscription at `./__push-sub` tagged with
`who`); `fragment.push.unregister()` drops it (`./__push-unsub`, by its
endpoint). Anyone who can see the fragment may subscribe (at most 10 000
subscriptions); a `who` that is an identity (`id:…`) is that identity's
own, and from anyone else is 403, so what is pushed to a person's
identity reaches their browsers alone (a chat's replies:
docs/chat-records.md, Push). `call.push(who, payload)` in a mutation
(sent once it commits) and `job.push(who, payload)` in a job (a step,
answering `{queued}`) push `payload` (`{title, body, tag, url}`, at most
3800 bytes) to the subscriptions tagged `who` (at most 64 characters), or
all of them with `*`, once per mutation or step. A relative `url` is the
fragment's own (`./` is its page): a click focuses a page of it already
there, or opens one.

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

A job's AI steps bill the fragment's owner, on their ledger (Ledger,
below), capped when the run's principal is neither the owner nor an
agent of theirs (a run does not record whom an agent asked for). Text
goes through the platform's model route (Models, below) by tier;
decisions are Clef (`@cf/cloudflare/clef`, `@cf/cloudflare/clef-flash`)
and images FLUX.1 [schnell] (`@cf/black-forest-labs/flux-1-schnell`) on
Workers AI, on the model route's transport (the `AI` binding through the
deployment's AI Gateway). Clef is metered in its input tokens ($0.24 and
$0.09 a million; its output is free: `fragment_core::decide`), an image in
neurons at Workers AI's price for it: 4.80 a 512×512 tile and 9.60 a step
(`fragment_core::media`). Nothing holds a key: the binding is
pre-authenticated.

Each paid step reserves its worst case on the owner's ledger before its
call (text: its request's bytes as tokens in and its tier's capped
`max_tokens` out; a decision: its input's bytes as tokens, at most Clef's
65,536-token window; an image: a 1024×1024 image's 4 tiles at its steps),
under the step's reference,
`step:<fragment>@<incarnation>/run/<run>/attempt/<attempt>/step/<index>`.
It keeps what the call bought beside the step (by
`<fragment>@<incarnation>/run/<run>/step/<index>`, the same in every
attempt), then settles from the usage the call reported (an image's
tiles from its JPEG's size, and its steps; a text call that reported no
usage is charged its reservation, `ai.cost-missing`, never nothing). So:

- a step tried again after its call answered reuses what it kept and
  never calls again: a settle that did not land lands, and an image whose
  commit failed commits from its kept bytes (bug 2). A replayed run
  reuses what its earlier attempts paid for, and pays only for the rest;
- a step that fails for good before its call used anything (a refusal
  from the model, the high tier) releases its reservation, and so does a
  step whose retries ran out, and any hold of a run that ended (bug 3,
  `ai.released`);
- a step the owner's ledger refuses (no credit, a guest, the fragment's
  cap) fails with the ledger's reason, 402 `budget_used_up` in its words
  (uncaught, the run is held; replay it after a top-up or next month).

The steps:

- `job.ai.text({model?, prompt | messages, max_tokens?, reasoning_effort?,
  tools?, tool_choice?, draft?})` → `{text, message, finish_reason, model,
  tier, usage}`: `model` is a tier, `cheap` (the default) or `medium`
  (`high` is refused: Models); `max_tokens` is at most 16384;
  `reasoning_effort` is GLM's, `low` (the default) or `high` (anything
  else is `low`, since GLM takes an unknown one as `max`).
  - `messages` reach the model as given, an assistant's `tool_calls` and
    `role: "tool"` results among them.
  - `tools` are OpenAI's (`{type: "function", function: {name,
    description?, parameters?, strict?}}`): at most 64 in 64 KiB of JSON,
    each name 1 to 64 letters, digits, `_` or `-`, once. `tool_choice` is
    `none`, `auto`, `required` or `{type: "function", function: {name}}`
    of one of them.
  - `message` is the model's, `{role: "assistant", content, tool_calls?}`
    (`content` a string, `""` with none; `tool_calls` OpenAI's, `{id, type:
    "function", function: {name, arguments}}`, `arguments` the JSON text
    the model wrote), never its reasoning; `text` is its `content`, and
    `finish_reason` the model's (`stop`, `tool_calls`, `length`).
  - `draft: {channel, turn}`: the call streams, and its text so far is put
    as that channel's draft, as `PUT …/channels/{channel}/draft` puts one
    (`draft` frames on `__live`, never stored), from the fragment itself
    (its npub): at most 4 a second, and its whole text once more at its
    end; none past 64 KiB, or past the fragment's pace for drafts. The
    channel is one the app declares (it needs no post role; whoever may
    read it sees the drafts), the turn `^[A-Za-z0-9._:-]{1,128}$`. A
    stream that breaks, or ends before its answer says why it stopped, is
    called again under the same reservation.
- `job.ai.decide({model, state, questions, images?})` → `{answers, model,
  usage}`: Clef's input and answers, as its catalog's schemas say.
  `model` is `clef` or `clef-flash` (`model` answers its catalog id);
  `state` is text, or JSON (an object or an array); `questions` maps 1 to
  64 ids (1 to 100 letters, digits, `_`, `.`, `-`) to a question: `{type:
  "noul", instructions, criteria?: {true?, false?}}` (yes or no), `{type:
  "choice", instructions, criteria: {<option>: <description>}}` (2 to 255
  options), or `{type: "score", instructions, criteria: [<level>, …]}` (2
  to 10, lowest first); `instructions` is text, or JSON holding it.
  `images` are at most 4 `data:` URLs of PNG, JPEG or WebP. Each answer is
  under its question's id: `{type: "noul", noul}` (the probability of
  yes), `{type: "choice", choice, probabilities, confidence}`, or `{type:
  "score", score, legend, probabilities, confidence}`; `usage` is
  `{input_tokens, output_tokens}`. An answer that does not answer every
  question is refused and charged its reservation (`ai.decide-refused`).
- `job.ai.image({prompt, path, steps?})` → `{path, size, sha256,
  mediaType}`: a JPEG (`image/jpeg`) written to `main` at `path`, which
  ends in `.jpg` or `.jpeg` (a file is served by its extension: bug 5),
  in git under 1 MiB (past an app's 256 KiB write limit: bug 1), a blob
  from 1 MiB. `prompt` is 1 to 2048 characters and `steps` 1 to 8 (4
  unless named); any other key (a model, an aspect ratio) is refused. An
  answer that is no JPEG is refused and charged its reservation
  (`ai.image-refused`).
- `job.ai.video(…)` is refused, saying "video steps are off until they
  run on Cloudflare" (the debt ledger); nothing is reserved or called.

A model's 429 or 5xx is retried; other refusals fail the step with the
model's message. A run answers `costMicros`: what its paid steps were
charged (none when nothing was).

### Models (docs/cloudflare-v1.md, decision 23)

One OpenAI-shaped chat completion on a tier's model, metered on its
payer's ledger (cell/src/models.rs). Tiers: `cheap` (GLM-5.3 Flash,
`@cf/zai-org/glm-5.3-flash`) and `medium` (GLM-5.3, `@cf/zai-org/glm-5.3`),
both on Workers AI through the deployment's AI Gateway (Unified Billing,
its logs off, its metadata opaque ids: the first 16 hex of SHA-256 of the
payer's and the agent's identities). `high` (Opus) is refused, 400,
saying why, until Cloudflare raises Unified Billing's Opus limit; a model
id is never a tier. `vision` names the deployment's vision model (its
config's `vision_model`, GLM-5.3 Flash unless named, one the price book
prices), for a runtime's calls about an image (a screenshot:
docs/computers.md, Models); it is metered as a tier's call, and is no
tier an agent or a job's step may name.

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/models/v1/chat/completions[?fragment=<name>]` | an agent (`for` names whom it acts for) | an OpenAI chat completion, `model` a tier or `vision` → the model's answer in OpenAI's shape: JSON, or with `stream: true` server-sent events, usage once on a last chunk with no choices |

What the model is sent is the body bounded: no `model` (the tier's),
its images as they came, `max_tokens` at most 16384, `reasoning_effort`
clamped (above), usage asked for when it streams, and none of the
client's headers. The payer is the agent's owner (decision 36); `fragment`, one the agent is a
member of (403 otherwise), is where the turn is: when its owner pays,
the call counts in its month and is under its cap for anyone but the
owner (or the owner's agent acting for them); when another person owns
it, that owner's ledger is asked first whether it is still open
(decision 26). A request is at most 6 MiB (413 past it, nothing
reserved): an image of 5 MiB of base64 fits. Each call reserves its worst case (the body's bytes as
tokens in, `max_tokens` out) under a reference of its own (`aig:<hex>`),
then settles from its last, cumulative usage, input less what was cached
(Workers AI puts a per-chunk delta on every chunk and the whole call's
on a line of its own at the end: only that is metered). A call the model
refuses is released and its answer passed through as it came; one whose
stream broke before its usage is charged its worst case; a streamed
call settles after its answer whether or not the client read to its end.
Nothing of the request or its answer is kept: only the usage, on the
ledger. A refusal of the payer's ledger is its own (402
`budget_used_up`, 403 for a guest), with its message.

The same call, as a Rust function the cell's other parts make
(`models::complete`: the payer, the agent, the fragment, the tier, the
body, and whether it streams), is the computer's model intercept from
phase 4.

### Ledger (docs/ledger.md)

Every person has a usage ledger: one `Ledger` Durable Object, named by
their identity, running `fragment_core::ledger` (plans, credit, holds,
caps, meters). Money is integer micro-dollars; a month is a UTC calendar
month. Plans (decision 25): `guest` (no agents, no AI, billed nothing),
`seat` ($50 of included credit a month) and `seat_always_on` ($100); a
new person's is `FRAGMENT_DEFAULT_PLAN`. Included credit expires at the
month's end; purchased credit (grants) stays; a charge draws included
credit first. What happened is always charged, past zero.

- **At zero** (decision 27) agents stop: no turns, no AI steps; the
  refusal says why. Fragments keep serving and taking writes.
- **Past the overdraft** ($2, an operator's to change) the person's
  fragments are read-only until a top-up brings the balance above zero:
  their mutations and jobs, channel posts, file writes, deploys, storage
  tokens, blob uploads, inbox deliveries and replays are refused, 402
  `budget_used_up`, saying why; reads keep serving. Their cron and
  triggers start no runs: each run one would have started is recorded
  `blocked`, its `error` the reason, and once the owner has credit they
  start runs again (a blocked run may be replayed). The person makes no
  new fragment meanwhile.
- **Guests** make no fragments (`POST /api/fragments` is 403), and claim
  no draft (Drafts, above): a guest pays for nothing. They edit the
  fragments shared with them, whose owners pay (Paul, 2026-10-03). An
  unclaimed draft has no ledger to ask: it spends nothing. A fragment asks its
  owner's ledger at most once a minute (`STANDING_CACHE_MS`), so a
  change reaches it within a minute; a ledger that does not answer
  refuses no write.
- **Caps** (decision 26): each fragment has a monthly cap on its owner's
  ledger, $5 until the owner sets one. Past it, AI steps and agent turns
  there stop for everyone but its owner and the owner's agents acting
  for them, until next month. Caps never stop writes.
- **Meters** (decision 24): each fragment's hosting bills its owner: its
  requests (a row per minute), each code version that runs on a UTC day
  (a dynamic worker), and a daily sample of its SQLite (its own and its
  app's) and its blobs' bytes, as byte-hours. Each row waits in the
  fragment's outbox and travels in a batch through the `fragment-ledger`
  queue; a batch is sent again until it is acknowledged, and the ledger
  charges each row once by its reference.

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/ledger` | a person (an agent: its owner's) | → `LedgerStatus` (`crates/proto` `ledger`): `{plan, seat, month, balanceMicros, includedMicros, includedGrantedMicros, purchasedMicros, reservedMicros, availableMicros, overdraftMicros, standing: {standing: ok \| agents_stopped \| read_only, why?: guest \| seat_canceled \| no_credit \| overdrawn}, priceBook, fragments: [{fragment, spentMicros, capMicros}]}` (this month's spend, the largest 50 first) |
| `PUT /api/f/{name}/cap` | the fragment's owner (never an agent) | `{id, micros \| null}` → `{fragment, capMicros, default}`: once by `id` (again: the same answer; another body: 409); `null` is the default |
| `POST /api/ledger/{person}/grant` | the deployment's operators | `GrantCredit {id, micros, by, why}` → `{}`: purchased credit, once by `id`; `by` is the operator who signs; at most $10,000 |
| `POST /api/ledger/{person}/plan` | the same | `SetPlan {id, plan}` → `{}` |
| `POST /api/ledger/{person}/seat` | the same | `SetSeat {id, seat: active \| past_due \| canceled, seq}` → `{}`: a change older (by `seq`) than the last applied changes nothing |
| `POST /api/ledger/{person}/overdraft` | the same | `SetOverdraft {id, micros}` → `{}`: at most $1,000; read-only is decided afresh |

`{person}` is a username, an identity (`id:…`), or `me`. Commands are
idempotent by their `id`, kept on the ledger as `<kind>:<id>` (a grant's
`g1` is not a plan's): the same id again changes nothing, the same id
with another body is 409 `conflicting_body`, and a body that breaks a
rule (an amount past its limit, a field misspelt) is 400 and remembered
by nothing, so it may be sent again. Test fleets add `POST
/api/test/ledger {identity, op: clock {offsetMs} | sweep | entries
{prefix} | totals}`.

### Jobs and triggers

A job is a method called `(input, job)` that runs as a Cloudflare Workflow,
outside any request. Each `await` on a `job.*` step is durable. The
steps are those below, the files steps (`job.files.*`), `job.push`,
and the AI steps (`job.ai.*`), all above:

- `job.call(op, input)`: an operation of this fragment as the run's
  principal, with `job:<run>:<step>` as its id (a retried or replayed
  step is a replay). Calling a job starts it: `{run, status}`, one hop
  deeper.
- `job.fetch(url, {method, headers, body})` → `{status, ok, headers,
  text(), json()}`: the app's one way out. `{{NAME}}` in a header value
  is the fragment's secret `NAME`, added by the platform at the egress
  point; the app never holds it. http(s) only; loopback, private, and
  `.internal` addresses are refused (and a Worker's fetch reaches only
  the public internet: a name with no public address fails the step); redirects are answered, not
  followed; a body of at most 256 KiB, a response of at most 1 MiB, 120
  seconds. Every fetch carries `x-fragment-hops`; one that is not a GET
  or a HEAD carries `Idempotency-Key: <fragment>-<life>-r<run>-s<step>`
  (the same on every try and replay of the step, never another
  fragment's), unless the job set its own.
- `job.publish(channel, body, kind)`: a record, once per step.
- `job.sleep(ms | "N seconds|minutes|hours|days")`, up to 30 days.
- `job.blob(sha256)` → `{sha256, size, text, cut}`: one of the
  fragment's blobs (Blobs, above: a page's upload, a chat's attachment),
  read for its code. `text` is its first 64 KiB
  (`steps::BLOB_READ_MAX_BYTES`) as UTF-8, less a last character the cut
  split, or `null` when they are not text (not UTF-8, or a NUL); `cut`
  says the blob goes on past them. A hash the fragment has no bytes for
  fails the step.
- What the fragment's own page reads, read for its code:
  `job.members()` → its members as `__members` lists them (`[{principal,
  role, kind, addedAt, …}]`, the first added first); `job.people(ids)` →
  names for at most 64 identities as `__people` answers them (`{[id]:
  {kind, username, name?, fragment?, picture?}}`, an agent made from an
  agent fragment named by its label; `picture` a path on the platform's
  origin); `job.presence()` → who is here now, as the pages' presence
  lists hold them (`[{id, principal, data}]`, one a socket that shares
  any). Each is a step, so a run reads what was true when it first took
  it, again on a retry.

`job.principal`, `job.role`, `job.run`, and `job.attempt` say who and
which; `job.via` how the run started (`call`, `job`, `cron`, `channel`,
`files`), and `job.fragment` the fragment's name. The method re-runs from the top at every step with the results so
far, so it must reach its steps in the same order each time and change
nothing except through steps. The platform keeps each step's answer
before the job's Workflow hears it, and the method reads the answers back
from there; a step tried again because its answer was lost on the way (a
timeout, a crash) is answered from what was kept, not performed again; a
run whose answers are missing is held. A step cut short before its
answer was kept (a crash between a fetch's request and its answer) is
performed again: a step runs **at least once**, as a Workflow step does.
Calls, publishes, file writes, pushes and agent turns are keyed by the
run and step, so a repeat applies nothing twice; a fetch's repeat
reaches its upstream again, with the same `Idempotency-Key`, so an
upstream that honours it acts once. A step that fails for a reason that may
pass (an upstream 429 or 5xx, a timeout, a platform error) is retried 4
times with doubling delays; one that fails for good (a refused URL, an
unknown operation, a call that threw), or runs out of retries, makes the
`await` throw a `StepError` the job may catch; so does a step whose
arguments do not fit its kind (`job.ai.text` without a model, say), with
what does not fit. Every kind of step and its arguments are defined once,
as `Step` in `crates/core/src/steps.rs`. A job that throws is
**held**: its run keeps the input and error until someone replays it. A
run runs on the app's code (`app.mjs` and `applib/`) installed when it
took its first step: a deploy that changes that code while the run is in
flight (a blessed template's release included) starts it over at its next
step, as its next attempt on the new code, as a replay would (its kept
answers are the old code's steps'; `run.restarted` in `events`). A run
that meets changed code at its 8th attempt is held instead. A step taken
as another kind than the run first took it (a body that did not reach its
steps in the same order) throws. At
most 256 steps (`limits::JOB_STEPS_MAX`; an agent's turn waits in polls and
sleeps) and 4 MiB of step results per run; a result of at most 1 MiB.

Every job call and every trigger is a **run**: `queued`, `running`,
`succeeded`, `held`, or `blocked`. A triggered mutation is a run of one
step. Triggered runs act as the fragment itself (its npub) with an
editor's role, `via` `cron`, `channel`, or `files`, and a `depth`: a
record appended by a run at depth d triggers runs at d + 1, and a
delivery's `x-fragment-hops` is its depth. Deeper than 16 is `blocked`
(`cycle.detected`). An operation's triggers pause themselves (`op.
auto-paused`) after 5 held runs in 10 minutes or 120 triggered runs in
an hour (blocked runs do not count); while paused, a trigger records a
`blocked` run and cron skips. Past the owner's overdraft, every
trigger's run (cron's included) is recorded `blocked`, saying why, until
a top-up (decision 27). Held runs from before an unpause do not
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

`<label>--<username>.<suffix>/<path>` (`/f/<name>/<path>` on the platform's host
redirects there, a `GET` or `HEAD` only, except `__watch` and `__live`). Every path
on a fragment's host is the fragment's, `/api/…` included (the platform
API answers on the platform's host):

| path | |
| --- | --- |
| `/`, `/<page>` | `site/` in the live commit (`index.html` for directories); Open Graph tags from `fragment.json`'s `meta`; an unclaimed draft's page with its bar (Drafts, above) |
| `__file?path=` | a content file from live, else main |
| `__preview.svg` | the placeholder preview image |
| `__blob/{sha256}` | one of this fragment's blobs (Blobs, above), `GET` or `HEAD`, viewers and up (on a `public` fragment too: whoever holds only `public` is refused): its bytes as the type its upload declared (ranges answer 206), `Cache-Control: private, max-age=31536000, immutable`, `X-Content-Type-Options: nosniff`, and an `ETag` of the hash; another fragment's hash is 404. `PUT` uploads one from the fragment's own page, as `PUT /api/f/{name}/blobs/{sha256}` does (editors; the body streamed and hashed on the way, bytes that are not what the hash says 400; a declared `content-length`, at most 256 MiB, else 400 or 413) → `{ok, sha, size, stored}` (`stored: false`: it was there); its `content-type` is the type it is served as. As for any write on this host, only the page's own cookies count (`fetched`): another fragment's page uploads as no one (401) |
| `__members` | viewers and up (the share link too): `{members: [{principal, role, addedBy, addedAt, kind, owner?}]}`, the first added first, as `GET /api/f/{name}/members` answers it (a chat's agents, its lead the first agent added) |
| `POST __op/{op}` | a browser's call: `application/json` `{id, input}`; a signed-in browser (`fragment_site`) calls as its person; an unsigned caller gets an anonymous principal cookie; callers holding only `public` get 60 calls a minute each, 600 per fragment (a page's live views re-run over `__live`, outside this) |
| `POST __op/channels/{channel}` | a browser's post (`fragment.post`), through the call's door and its checks: `{id, input}` with the record's body as `input` → `{result: record, replayed}`, as `POST /api/f/{name}/channels/{channel}` answers it; a post spends the public budget as a call does (no operation name holds a `/`) |
| `__signin`, `__signout` | this origin's session (Sign-in, above) |
| `POST __mcp`, `/.well-known/oauth-protected-resource[/__mcp]` | its MCP server, for a connected client, and its metadata (Connected clients, A fragment's MCP server, above): the platform's, before the site and the app |
| `__fragment.js` | the browser library (below) |
| `__fragment.css` | the platform's stylesheet (below), for a page that links it |
| `__people?id=…&id=…` | anyone who can see the fragment: `{profiles: {<id>: {kind, username, picture, name?, fragment?}}}` for up to 64 identities (an agent's `username` is its owner's; a picture is a person's, an absolute platform URL; an agent made from an agent fragment, a computer's, has that `fragment` and its label as its `name`, which `@mentions` it); an id the registry does not hold is left out |
| `__files` | the files viewer, the platform's page (`__files.js`, `__files.css`): the content files (live and main) as a tree beside a reader (markdown with `[[wikilinks]]`, other text with line numbers, pictures, downloads), reading each through `__file`, following `__watch` where it may; asked for `application/json`, the list it reads, `{type: "files", count, files: [{path, size}]}` (a path on both is live's). Framed, the reader's bar asks the page around it to open a file as a pane (`postMessage({fragment: "open", url, title})`) |
| `__live` | WebSocket, anyone who can see the fragment: channel subscriptions from a cursor, presence, change signals, queries (below) |
| `__watch` | WebSocket, viewers and up (the share link, or a signed upgrade): `{type: "hello", ref, sha}`, then `{type: "changed", ref: "main", sha, paths}` per external move of main |
| anything else | the app's `fetch`, when it has one |

A site file carries an `ETag` that names its bytes: the last commit that
changed it, or, for a page given Open Graph tags, a weak tag that also
names the live commit (its `fragment.json`). `__fragment.js`,
`__fragment.css`, `__sw.js`, `__files.js`, and `__files.css` carry a
hash of their bytes. A `GET` or `HEAD` whose `If-None-Match` names the current
tag answers 304 without reading the file.

`__live` and `__watch` are also served in place at `/f/<name>/…` for the
CLI. A socket has no CORS, and every fragment's origin is one site with
the others, so a page on another fragment's could open one with this
origin's cookies: an upgrade (to any path on a fragment's host) whose
`Origin` is not the fragment's own, `null` included, is 403, and one that
names no `Origin` is no browser's, so its cookies (the session, the share
link's, the anonymous one) count for nothing: it is its signer's, its
`?view=` link's, or anonymous. The `__live` protocol is JSON frames tagged by `type`, defined once
as `LiveIn` (client → server) and `LiveOut` (server → client) in
`crates/proto/src/live.rs`; the cell and the CLI decode through them. An
unsigned visitor without a cookie gets the anonymous principal cookie on
the upgrade, as a call does.

- client → server: `{type: "subscribe", channel, after}` (records with
  `seq` after `after`) or `{type: "subscribe", channel, last}` (the last
  `last` records, at most 1000): exactly one of `after` and `last`;
  `{type: "unsubscribe", channel}`, `{type: "presence", data}` (at most
  4 KiB; `null` or no `data` clears; at most 10 a second after a burst
  of 10: a faster change is dropped, with an error), `{type: "query",
  id, op, input}` (a query run as the socket's principal and role; `id`
  is the page's, `^[A-Za-z0-9._:-]{1,128}$`), `{type: "ping"}`
- server → client: `{type: "hello", id, principal, role, presence:
  [{id, principal, data}]}` (everyone sharing presence as it opens),
  `{type: "record", channel, seq, at, principal, kind, body}`,
  `{type: "subscribed", channel, next, more}` (after each page),
  `{type: "draft", channel, principal, turn, text, at}` (a record its
  poster is writing, to the sockets following its channel; `text: null`
  once they stopped; never stored, so a page that joins late sees the
  next),
  `{type: "presence", id, principal, data}` (one socket's change, to
  every socket, its own too; `data: null` once it cleared or left),
  `{type: "changed", op}` (after every applied mutation),
  `{type: "result", id, result}` or `{type: "result", id, error,
  message, status}` (a query's answer, or its refusal as `__op` would
  refuse it, with the code's HTTP status),
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
stops being public closes its anonymous visitors'. Changing a member's
role closes theirs (and, for an agent, its owner's) with 4001, which the
browser library reconnects after, at the new role. A signed-in socket
acts as who it connected as for a minute (`limits::LIVE_IDENTITY_MS`);
at its next frame after that the registry is asked again, with the
credential it connected with: a sign-out, an ended session, or a revoked
key closes it with 4001, and so does a fragment that woke from
hibernation (it keeps no credential) or a registry that cannot answer.
A fragment holds at most 1000 live sockets (`limits::LIVE_SOCKETS_MAX`),
and one principal at most 8 (`limits::LIVE_SOCKETS_PER_PRINCIPAL`); past
either a new one is refused (429). Opening a socket at the public role
is one call of the public call budget below (429 past it).

A query over the socket runs as `__op` runs it, as the socket's
principal and role, with no HTTP request, router, or registry lookup,
and outside the public call budget: a principal may run 16 queries over
its sockets between two changes to the fragment
(`limits::LIVE_QUERIES_MAX`; a page re-runs its live views after
changes; its tabs share them, and a socket opened again gets none
fresh), no more than that at once, and one at a time for each id on a
socket. Past that, a query is refused `rate_limited` (429). Only
queries run there: a mutation or a job is refused, and not run. So a
visitor who reconnects over and over pays a public call each time, and
visitors holding the public role alone run at most
`PUBLIC_CALLS_PER_MIN_FRAGMENT` × 16 queries a minute over a fragment's
sockets between changes, however they open them.

The browser library (`import * as fragment from "./__fragment.js"`):
`call(op, input, {id?})` (retries keep the id), `post(channel, body,
{id?})` (a record to a postable channel, as the page's principal; the
record comes back, and the same id again is the same record), `live(op, input,
onResult, onError?)` (re-runs a query after every change, over the socket
when it is open and over HTTP when not, or when its principal's budget is
spent; one run at a time, however many changes came), `subscribe(
channel, onRecord, {after?, last?, onDraft?})` (pages through the backlog, then
follows live; after a reconnect it resumes after the last record;
`onDraft` hears the channel's drafts while it is live),
`presence.set(data)` (changes within 150 ms go as one, the latest),
`presence.on(fn)` (called with everyone here now, and on each change),
`blob(file, {name?, type?})` (a File or Blob uploaded as one of the
fragment's blobs, its SHA-256 computed in the page, as an editor; answers
`{sha256, size, type, name}`, a chat attachment's shape, read at
`./__blob/<sha256>`), `me()`, `closed(fn)`. The page's
socket reconnects by itself after a jittered wait (half to one and a
half times a backoff that doubles from 1 to 30 seconds), except after a
close the fragment means for good: 4003 (the page's access was revoked)
or 4004 (the fragment was deleted) ends it, and `closed` handlers get
`{code, reason}`.

The stylesheet (`<link rel="stylesheet" href="./__fragment.css">`,
`cell/fragment.css`) is for a page that links it: the platform adds it
to no page. It holds theme variables, light and dark by
`prefers-color-scheme` (`--bg`, `--fg`, `--muted`, `--line`, `--accent`,
`--danger`; `--font`, `--font-display`, `--font-mono`; `--text-sm`,
`--text-lg`, `--text-xl`; `--radius`), and base styles for the body,
`main`, headings, `small`, links, `code`, media, form controls, buttons
(the accent), and the focus ring. The variables' names hold across
releases; their values, and the base styles, may change with one.

CLI: `fragment call <name> <op> --input '{...}' | @file | - [--id ID]`
(a file, or stdin, for an input over the 128 KiB one argument holds on
Linux), `fragment
post <name> <channel> --body '{...}' [--id ID]`, `fragment
channel <name> [<channel>] [--after N] [--follow]`.

### Hosts

The router takes a host in this order: the platform's own host (even
under the suffix); a fragment's host; the suffix's own name, which
answers `308` to the same path and query on the platform, when the
platform is elsewhere (`Cache-Control: no-store`, so the platform can
still move); any other name under the suffix, 404; anything else, the
platform.

## Agent docs

For an agent with no CLI yet (llmstxt.org), the platform's own host
answers `GET /llms.txt` with `cli/SKILL.md`, the text `fragment skill`
prints (what fragment is, the install, pairing, the daily commands),
and `GET /llms-full.txt` with `cli/GUIDE.md`, the text `fragment guide`
prints (the whole manual). Both are compiled into the cell from the
CLI's own files, so they never drift from it. They answer anyone, as
`text/plain; charset=utf-8` with `Cache-Control: no-cache` and an ETag
of their build-time hash (304 when `If-None-Match` names it), as
`__fragment.js` does. On a fragment's host the same paths are the
fragment's own.

## Connections (decisions 22 and 37)

Every provider of the deployment's catalog (`FRAGMENT_PROVIDERS`): a
person's accounts (connections, held and refreshed by WorkOS Pipes), the
operator's keys, and the person's own keys. Their agents use them through
the computer's swap, each with a placeholder of its own
(docs/computers.md, Connections and operator keys).

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/connections` | a person | → `{providers: [{provider, kind, state, hosts, env, price?}]}` (proto's `Providers`): each provider the deployment offers, in its catalog's order. `kind` is `connection`, `operator` or `own`; `state` a connection's `connected`, `needs_reauthorization` or `not_connected` (Pipes' connected account, read with no token minted), an operator key's `offered`, an own key's `set` or `not_set`; `price` an operator key's list price, `{micros, per}` calls. Reading it tells the person's computer the connections' states, so its guest is given a new connection at its next read |
| `POST /api/connections/{provider}/authorize` | a person | → `{provider, url}`: Pipes' consent, for the person's browser; followed, the account is connected. A provider not offered is 404, one that is no connection 400 |
| `PUT /api/connections/{provider}/key` | a person | `{key}` → `{provider, state: "set"}`: the person's own key for an `own` provider, 1 to 4096 printable characters with no space, kept sealed by their computer (made first: 404 without one) and swapped in for their agents' placeholders. Any other provider is 400 |
| `DELETE /api/connections/{provider}/key` | a person | → `{provider, state: "not_set"}`: the key is gone; their agents' guests are given it no more |

## The shell (phase 5)

The platform's one page is `/`, and `/settings` (cell/shell/, its files at
`/__shell/<file>`): its script reads the path, opening its settings at
`/settings` and the person's chats and apps at `/?apps`, and puts the view
it shows in the address, so a reload stays put. At `/` it opens the
person's mind (kind `mind`, docs/optchat.md) full-screen on its own origin
(`/auth/fragment`); a person with none gets their first run there: their
default agent on their computer, their mind (`members`, the agent an
editor), then the mind. Its settings hold the person's
account (username, sign-ins, identity id, picture, `/auth/link` to add
another sign-in, a POST to `/auth/logout`), their credit and what their
standing stops, their computer and agents, their skills (decision 17: the
managed set, read from their skills fragment's files by category, and
each agent's own, from its fragment's `skills/`; the shell makes the
skills fragment, kind `skills`, at setup beside their default agent),
their connections (every provider the deployment offers, one row each:
its kind, their state there with its action, which of their agents may
use it, pressed to narrow one, and this month's calls by agent with an
operator key's cost: `GET /api/connections`, the computer's view and its
`uses`), their connected clients (Connected clients, above: each one's
client, what it reaches and since when, and an End), and pairing the
CLI (the one-line install, `fragment login`, and `fragment skill` for a
coding agent); its sidebar lists their fragments, each one's share sheet
(`/share/<name>`) in a dialog, and the catalog makes an app from a
template (its Brain, a blessed `brain` with a title, as a chat is made).
It calls the API with the person's platform session
rather than a key: a request on the platform's host is taken as the
signed-in person when it carries `x-fragment-shell: 1` and
`Sec-Fetch-Site: same-origin`, and, writing, the platform's exact
`Origin`. Its list's socket (`GET /api/fragments/watch`), which can
carry no header, is taken by the platform's exact `Origin` alone.
Anything else needs a signature as before. Adding a key still needs a
key.

Its sidebar is the person's list, one read: chats (kind `chat`), each
with its agents and its newest message (the row's `agents` and
`preview`), then apps, less the ones they archived (`PUT
/api/fragments/{name}/archived`, above). It is live: the page holds
`GET /api/fragments/watch` (above), and on each `changed` (a moment
after, so a burst is one read) reads the list again and patches the
sidebar in place, so an app their agent makes, one someone shares with
them, a delete or an archive elsewhere, and a chat's new message show
with no reload, the open chat and the rows already shown untouched (a
row the same as before keeps its element). Only the apps whose rows the
change touched have their card read again. A socket that closes is opened again after a
jittered wait (1 s, doubling to 30 s), and each opening reads the list
again, for what changed while it was shut.
Each app's row shows its preview card (`GET /api/f/{name}/card`, Cards
above, read with the session and shown as a blob URL), or its icon until
the first is made: an app without one is asked again, from 2 s apart to
a minute.
A chat with two agents or more is a **group** (decision 8): "New group
chat" makes a chat fragment on the `chat` template, titled by the name
given or else its agents' names, and adds the agents picked as editors
one at a time in the order picked, so the first is its lead (the first
added, by `addedAt`). Which agents a chat has is its member list, as
its row names them (`agents`), never its name; the sidebar stacks their
avatars, each in its identity's colour (the chat page's FNV-1a choice).

**A page of the person's own may ask for their agents.** A frame the
shell made of a fragment its person owns (the open chat, an app's window)
may `postMessage({fragment: "agents?"})` to it; the shell answers that
frame, at that fragment's own origin only (its status's canonical URL),
`{fragment: "agents", agents: [{identity, fragment, name, title}]}`: the
agents its person's computer runs, `name` the label that `@mentions`
each. It sends the list again when it changes. The frame may then ask
`{fragment: "add-agent", identity, nonce}` (a nonce of at most 64
characters) for one of those agents. **The shell asks its person first**,
in its own dialog, never in the frame: "Add Fred to <the fragment's
title>? Fred will be able to read and edit it.", Add and Cancel, Add armed
800 ms after it shows (so the click or key that sent the page's message
cannot confirm it). Only on Add does it add the agent to the frame's
fragment as an editor (`PUT …/members/{identity}`, as making a chat
does). It answers `{fragment: "agent-added", nonce, identity, ok,
error?}`: `ok: true` once added; `error` `"declined"` on Cancel or Escape,
`"not answered"` when the dialog is left 90 s, `"busy"` while another ask
is open, or the API's refusal. Nothing is remembered: each add is asked,
in every fragment. A page is code its author or an agent writes (sometimes
from what it read on the web, or a forked template's), so it asks and
never grants: no page can put its person's agents (and their connections)
into a fragment whose records it controls without them saying so. A frame
of a fragment shared with the person (not theirs) gets no answer to
either, so it neither learns their agents nor adds one, and no page adds
anyone but its own owner's agents to its own fragment, which is an
owner's share with their own agent (decision 36). It names no template;
the chat's `@` is its user (docs/chat-records.md, "The page").

### Search (decision 9; docs/cloudflare-v1.md, lesson 12)

Search is FTS5 in the person's `Principal` cell (principal.rs), a
projection the fragments keep, as they keep the list:

- **What is searched.** A record is a message when its body is an object
  whose `kind` is absent or `"message"`; its text is the body's `text`,
  when that is a string, and only that (docs/chat-records.md: a person's
  message and an agent's reply; never a page's own kinds, an agent's
  steps, Stop). The platform knows no template: this is the convention a
  fragment's records follow to be found. Only channels every member may
  read (read role `viewer` or weaker) are searched, and the platform's
  own (`events`, `ops`, `inbox`) never. A message's first 4 KiB are
  searched (`limits::SEARCH_TEXT_MAX_BYTES`); its record keeps all of it.
  Titles and labels are searched from the list itself. A fragment's files
  are not searched yet.
- **Who gets it.** A fragment sends its messages to its people: members
  that are not agents (agents need no search). It logs each message in
  the record's own turn and sends each person, from an outbox with the
  outboxes' backoff, what their list has not taken, a batch of at most
  100 at a time from the fragment's alarm (search.rs). A person who joins
  gets what the fragment still logs, from its start.
- **The fence.** A list takes a fragment's messages only while its row
  for that fragment names a role, at the fragment's incarnation; each
  entry once (keyed by the fragment and its place in the fragment's log),
  so a batch sent twice or late adds nothing. Leaving the fragment (or
  its deletion, or its making again) arrives as the row's change, which
  drops every entry of it, and a search reads entries only of rows that
  name a role. Each row also names the channels searched now, and a list
  keeps and takes only their messages: a deploy that tightens a channel
  past `viewer` (or drops it) drops its messages from the fragment's log
  and sends every row again, which drops them from every list. Search
  sees only what the person can see now.
- **Limits.** A list keeps a fragment's newest 10 000 entries
  (`SEARCH_ENTRIES_PER_FRAGMENT_MAX`, as many as a postable channel keeps
  records) and 100 000 in all (`SEARCH_ENTRIES_MAX`), the oldest going
  first. A search answers at most 20 fragments and 50 messages, each with
  a snippet of at most 300 bytes of plain text around its words.
- **A query is words.** Each word (split at spaces; one with no letter or
  digit is dropped) must be in the message, as a word or a word's start,
  ignoring case and accents. FTS5's syntax is never read: `OR`, `NOT`,
  `NEAR(…)`, `column:`, `*`, `^` and quotes are text like any other.

A chat's newest entry is also its row's `preview` in the list (above).
Search holds only what every member may read, so a chat with no such
message, or whose entries went past the total, shows none.

The shell's search dialog shows the fragments, then the messages; a
message opens its chat (or app). It does not scroll to the message: the
chat's page has no way to be told one yet.

## Computers (docs/computers.md, phase 4)

A person's computer runs an image (pinned per computer) and the agent
fragments assigned to it; the Computer Durable Object (`cell/src/computer.rs`)
is the only thing that talks to its container. What a guest may rely on
is docs/computers.md; the routes here are its owner's.

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/computers` | a person | → `{computer, owner, image, phase, why?, agents, origin}` (`ComputerView`): their computer, made asleep on the deployment's default image (`FRAGMENT_COMPUTER_IMAGE`); again, the same one (its id is derived from its owner) |
| `GET /api/computers` | a person | → `{computers: [ComputerView], defaultImage}`: `defaultImage` is the image a new computer is pinned to; one pinned to another may update to it (the shell asks) |
| `GET /api/computers/{id}` | its owner | → `ComputerView`: `phase` is `asleep`, `starting`, `awake`, `sleeping`, or `wont_wake` (its starts kept failing; `why` says why); `restored` is what its last start that came up restored (`{generation, from: snapshot \| backup \| nothing, save?, savedAt?, ageMs?, after?: sleep \| exit, rollback, at}`: the save's id, when it was taken and how old it was then, and how the life before that start ended; absent before its first start), `rollbacks` counts its starts that went back in time (the life before ended by a crash, or by a sleep that slept unsaved, or its start fell back to an older save: docs/computers.md, "Saves and what a wake restores"), and `saves` lists the saves of its `/data` it keeps, newest first (`[{number, id, at, generation, held, unusable?}]`, at most three); `why` also says when its sleep's save failed and it stays awake, or slept unsaved; anyone else 404 |
| `POST /api/computers/{id}/wake` | its owner | → the view once it is awake (a wake also lifts `wont_wake`); 503 `wont_wake` when it would not start |
| `POST /api/computers/{id}/sleep` | its owner | → the view once it is asleep: the guest held, `/data` saved, the guest signalled, the container gone. When the save fails, the view is awake (its container kept, its `why` saying so), and its sleep is tried again on its own (docs/computers.md); asked again, it tries at once |
| `PUT /api/computers/{id}/image` | its owner | `{image}` → the view: the image it starts from at its next wake (an upgrade, or a rollback), its `/data` restored; an image the deployment lacks is 400 |
| `PUT /api/computers/{id}/agents/{fragment}` | the owner of both | → the view: the agent fragment runs on it. The fragment's own key becomes the agent's identity (registered to its owner), an editor of its own fragment; it signs the guest's requests only while it is assigned here. Assigning it again changes nothing. Nothing restarts: an awake computer's guest reads its agents again while it runs and runs the new one (docs/computers.md); a sleeping one's reads it as it starts |
| `DELETE /api/computers/{id}/agents/{fragment}` | the same | → the view: it signs nothing for the guest from now on; an awake guest stops running it as it reads its agents again |
| `PUT /api/computers/{id}/agents/{fragment}/connections` | its owner | `{connections: [provider] \| null}` → the view: the providers of the catalog (connections, operator keys, own keys) the agent may have swapped in (decisions 22 and 37), all named at once. `null`, the default, is every one its owner has (decision 44: a person's agents are not fenced from each other); a list narrows the agent to those, and its guest is given the rest no more. A provider the deployment does not offer (`FRAGMENT_PROVIDERS`) is 400, as is a body without `connections` |
| `GET /api/computers/{id}/uses` | its owner | → `{computer, month, uses: [{provider, agent, calls, micros}]}` (proto's `ComputerUses`): this month's (UTC, `YYYY-MM`) calls through the computer's swap that a provider answered (under 500), by provider and agent fragment, and what they were charged: an operator key's at the price book's price and the margin (as its owner's ledger charged them), a connection's and an own key's `0` (counted, never charged). Thirteen months are kept |
| `GET /api/computers/{id}/uses/{YYYY-MM}` | its owner | → the same, for that month; a month that is not one is 400 |
| `POST /api/computers/{id}/ports/{port}/ticket` | its owner | → `{url, expiresAt}`: a one-time link (two minutes) that signs a browser in to the computer's own origin, `<24 hex>--computer.<suffix>` (`/__ticket`, then `/p/<port>/`), cross-site from the platform, in a tab of its own or a frame of the platform's page (below); a signed request needs none |

On a computer's origin, `/__ticket?t=` redeemed by a top-level visit
sets `fragment_computer` (HttpOnly, SameSite=Lax, `Path=/`); redeemed by
a frame's navigation (the shell's tab onto a port), it sets
`fragment_computer_frame` (`HttpOnly; Secure; SameSite=None;
Partitioned`), in the platform page's partition. Both are `__Host-` over
https, last 12 hours, and name the computer's owner only. Every answer
on that origin but a socket's upgrade carries `Content-Security-Policy:
frame-ancestors <platform>`: only the platform's page may frame a port.
Every fragment's page is one site with that origin, so which cookie
counts follows the Fetch Metadata, as on a fragment's (Which cookies
count, above): `fragment_computer` on the origin's own page's requests
and a top-level navigation, `fragment_computer_frame` on its own page's
requests and a frame's navigation, neither on another page's image,
script, fetch or form; a socket from any page but the origin's own is
403.

A computer is woken by a record on a channel one of its agents
subscribed to with `{channel, wake: true}` (only its egress asks for
one: from anywhere else it names no URL, 400), by a page opening such a
fragment (a pre-wake, at most every 30 s), by a request to one of its
ports, by one of its agents becoming a member of any fragment, and by
its owner. Records its own agents post wake nothing, and neither do
their own sockets: its guest following a fragment pre-wakes no
computer its agents run on. An agent added to a
fragment (a member's `PUT`, an invite it accepts, a fragment it makes
for its owner) needs nothing more from whoever added it: the platform
posts `{kind: "joined", fragment}` on the agent fragment's `tasks`, as
that fragment, once for the membership, and wakes the computer (Paul,
2026-10-03; docs/computers.md). The fragment it joined keeps the notice
until the computer has it (`agent.told`, or `agent.untold` with why).

A guest's request to a provider's host has its placeholders swapped, in
a header, the query string or basic auth, as the catalog says
(docs/computers.md, Connections and operator keys); the agent is the one
its placeholder's tag names, with no header of ours. The swap's refusals
reach the guest as the platform's, saying why and reaching no provider:
400 `invalid_request` (a malformed placeholder, a provider the deployment
does not offer, one in a place its provider does not take it, more than 4,
or two agents' in one request), 403 `forbidden` (a placeholder sent to a
host that is not its provider's, a tag that names no agent on this
computer, or an agent its owner narrowed from that provider), 403
`not_connected` (its owner has not connected that provider or must connect
it again, or has given no own key), 402 or 403 the ledger's (an operator
key's call its owner's ledger refuses).

### A chat's records

A chat's records (its `chat` and `work` channels: turns, steps,
replies, prompts, Stop) are docs/chat-records.md's: the bridge writes
them and the chat template reads them.
