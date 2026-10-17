# fragment wire contract

The cell (`cell/`, Rust on Cloudflare Workers) answers everything below;
the CLI and the browser library are its clients. Errors are `{"error":
"<code>", "message": "..."}` (codes in `crates/proto`). `cargo xtask e2e`
proves every route. The TypeScript runtime this replaced was deleted
(its contract is in git history, last at `35f5e18`). The desktop and the
personal agent's chat went at the cut (docs/cloudflare-v1.md, decision
33; their contract is at the tag `celld-final`); computers came back
(Computers, below), with Hermes as our image's agent runtime.

## Configuration

Worker variables, rendered from the deployment's config by `cargo xtask
deploy` (`deploy/example.jsonc`; `.dev.vars` in dev). None is a secret:
the deployment's secrets are Worker secrets (below).

| Variable | Meaning |
|---|---|
| `CODESTORAGE_ORG` | the code.storage org (required) |
| `CODESTORAGE_API_URL` | the API base (default `https://api.<org>.code.storage`) |
| `FRAGMENT_HOST_SUFFIX` | fragments are served from `<name>.<suffix>` (any other name under it is 404, never the platform; the suffix's own name is the platform's, or redirects to it: Hosts, below). Every deployment names one: an isolate without it does not start |
| `FRAGMENT_HOST_LABEL_SUFFIX` | a branch deployment's mark, `--<branch>` (a branch is 1 to 16 of lowercase letters, digits, and single dashes inside, as `cargo xtask deploy` makes them; an isolate with another does not start): its fragments are `<name>--<branch>.<suffix>`, one DNS label beside the other branches' in one zone, and the mark takes from the room its labels have (Names, below) |
| `CODESTORAGE_REPO_PREFIX` | what this deployment's repos are named with first (a branch's `<branch>--`), so deployments sharing an org never share a repo |
| `FRAGMENT_POLL_INTERVAL_S` | how often a busy fragment's pass runs (default 300): one whose pins may lag its repo (a storage token was minted for it, or a move failed to follow, in the last day), which polls code.storage for its branches (the backstop for a push no one refreshed), or with a run in flight (checked against its Workflow), an ended run's reservation to give back, or a template or the agent it declares still to land. Any other fragment's pass is daily, and polls nothing: every move the platform makes or is told of (`refresh`) is followed at once |
| `FRAGMENT_JOB_RETRY_DELAY_S` | a failed job step's first retry delay, doubling over 4 retries (default 10) |
| `FRAGMENT_EGRESS_LOCAL` | `allow` lets jobs fetch loopback and private addresses, and takes a connection's http:// consent URL (dev and e2e fakes); never on a shared fleet. It also makes a fleet local for its test levers (Test levers, below) |
| `FRAGMENT_BLOB_GRACE_S` | how long a blob no branch names is kept before it is deleted (default 7 days) |
| `FRAGMENT_DELIVERY_RETRY_S` | test fleets: every wait before a delivery, or a preview card's failed shot, is tried again, pinned. Unset, a delivery's wait is 10 s growing with its age to an hour, and a shot's 10 s doubling to an hour (Cards, below) |
| `AI_GATEWAY_ID` | the AI Gateway the model route and image steps call through (Models, below): the deployment's own, named (`default` is refused: it makes one that logs); unset, models and images are off |
| `FRAGMENT_AI_URL` | dev and the e2e only: the model route POSTs the AI binding's input to `<url>/run/<model>` instead of calling the binding (the Workers AI fake, a lower rung) |
| `FRAGMENT_MAIL_FROM` | the sender the platform's mail comes from: a bare address or unquoted `Name <address>` (Mail, below; the deploy config's `mail_from`), on a domain onboarded to Email Sending; unset, the deployment sends no mail |
| `FRAGMENT_MAIL_URL` | dev and the e2e only: the platform's mail is POSTed to `<url>/send` (the `EMAIL` binding's input) instead of sent (the mail fake, a lower rung, which keeps it; `cargo xtask dev` prints it) |
| `FRAGMENT_DEFAULT_PLAN` | a new person's plan (Ledger, below): `guest` (the default and production's), `seat`, or `seat_always_on`; dev and the e2e set `seat` |
| `FRAGMENT_COMPUTER_UNSAVED_MAX_MS` | how long a computer whose sleep's save keeps failing stays awake, its container kept, before it sleeps unsaved (the deploy config's `computers.unsaved_max_ms`; default 1800000, thirty minutes; docs/computers.md, "Saves and what a wake restores") |
| `FRAGMENT_SUPPORT_URL` | where a person whose computer will not start gets help (the deploy config's `support_url`): an `https:` page or a `mailto:` address, at most 512 visible characters, refused at the node's first request otherwise; `GET /api/computers` gives it as `support`, and the shell links it (docs/computers.md, "What its owner is told"). Unset, none |
| `FRAGMENT_OPERATORS` | identities and keys that grant credit and set plans, seats and overdrafts, and wipe a person (Operators, below); a key listed is an operator's whether or not anyone holds it |
| `FRAGMENT_DEPLOY_ID` | which deployment this is (default `dev`); `GET /healthz` answers it in `x-fragment-deploy` |
| `WORKOS_API_URL` | where WorkOS is (default https://api.workos.com; dev and the e2e: the fake); sign-in is on where the `WORKOS_CLIENT` secret is bound (below), and answers 500 where it is not |
| `FRAGMENT_PLATFORM_URL` | the platform's origin, where sign-in and the platform session live (required; production's is https://finite.chat, on no fragment's domain, and the CLI's default host). Every push's VAPID token names it as its contact (`sub`, RFC 8292) |
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
| `POST /api/test/signin` | `{email, paidCalls?}` → `{session, identity, created, paidCalls}`: an e2e person's platform session (the `fragment_session` cookie's value). `email` is `<name>@e2e.test` (a name of 1-64 of `[a-z0-9._-]`; any other is 400); its person is keyed by it under the issuer `e2e.test`, which no real sign-in has, so they are never anyone WorkOS signs in. The first sign-in makes them (`created`), and each makes them a seat (one ledger command, `e2e-seat`) and caps their paid calls (model calls and AI steps, each a reservation) at `paidCalls` from now, at most 200; a sign-in with no `paidCalls` leaves the cap as it is (a new person's: 0), so a sweep's never stops a run still spending; their ledger refuses the one past it (402 `budget_used_up`). On a branch a day (UTC) makes at most 1000 e2e people and lends them at most 2000 paid calls (429 past either) |
| `POST /api/test/people` | `{after?}` → `{people: [{identity, email}], next}`: the e2e people by identity, 100 a page (`next`: the identity to ask after, `null` on the last page); the hosted e2e's sweep signs each in again and deletes their fragments of one run (`e2e-<run>-…`), or with `--sweep-all` every `e2e-…` one at least an hour old (crates/e2e/src/hosted/sweep.rs) |
| `POST /api/test/ledger` | `{identity, op, …}`: a lever on that person's ledger: `clock {offsetMs}` moves its clock, `sweep` runs its sweep now, `entries {prefix}` lists its references under a prefix (at most 500), `totals` answers what moved its balance, `paid-calls {max}` caps its paid calls from now (`{max, used}`), and `legacy-seat {seat, seq}` sets its seat at that order, as an operator's ledger `seat` command did before it went (2026-10-16), answering its status (an order no newer than its own is refused, 400, as the registry's would be) |
| `POST /api/test/keys` | `{fragment, op, plaintext\|sealed}`: seals or opens as that fragment |
| `POST /api/test/list` | `{identity, op}`: a lever on that identity's list (on a preview, an e2e person's or their agent's): `before-searched` makes it a list from before its table named the channels searched (#186), as the lists p5 made before then are: every newer change to it is refused (500) → `{columns}` |
| `POST /api/test/fragment` | `{fragment, op, …}` pulls a lever on that fragment: `fail-deliveries {times}` fails its next queue sends, `fail-outbox {times}` fails its next records' outbox writes just after their append, `fail-triggers {times}` fails its next trigger steps just before their last run starts, `drop-effects {times}` loses its next job step answers on their way back to the Workflow (after the step ran and its answer was kept), `forget-steps` forgets the kept answers of its runs in flight, `hold-advances {on}` holds each advance after a run's first step while on (at most 20 s), and `advance-held` answers `{run}`, the last run it held, `forget-live` makes it forget what it knows of its live sockets beyond their attachments (as waking from hibernation does), `age-live {ms}` makes every live socket's identity check `ms` older (as if that long had passed), `drop-live {code}` drops its live sockets, `ledger {ms \| null}` shortens (or restores) its operation ledger's window, `age {ms}` forgets its write keys as if `ms` had passed, `members {fill}` adds placeholder members until there are `fill`, `code-builds` answers `{builds}`: how many times the fragment's activation built its app's worker code for the loader, `alarm` answers `{alarmAt, pollAt, now}` (ms): when its alarm and its next poll are set for, `age-outside {ms}` makes the last sign its pins may lag (a storage token minted, a move that failed to follow) `ms` older, `age-invites {ms}` makes every invite waiting run out `ms` sooner and arms its alarm for the soonest (`{aged, invites}`: how many), `fail-after-paid {times}` fails its next paid AI steps just after their call was paid and kept (so the step is tried again), `fail-meter-acks {times}` loses its next meter batches' acknowledgements (so the queue delivers them again), `meter-now {sample?, resend?}` closes every counted minute, takes a storage sample (unless `sample: false`) and sends its outbox's batch now (a waiting one again with `resend`), answering the outbox, `meter` answers the outbox, `forget-standing` forgets what it heard of its owner's standing (meter.rs), `cron-now` makes each of its cron schedules due at once (`{due}`: how many), so a test need not wait for a schedule's minute, `poll-now` makes its next alarm a poll pass (the blob collection's), `fail-cards {times}` makes its next card shots open a page nothing serves (`http://127.0.0.1:9/`, which Chrome refuses), so they fail as an unreachable page does, `cards` answers `{cards, failCardsLeft}`: its card and schedule as kept (`fragment_core::card::Cards`) and the shots the lever still fails, `repo {repo}` makes it name the repo `repo` (`[a-z0-9-]`), as a fragment made before repos were named for their owner does (a wipe deletes what it names), and `ended` answers `{ended: [{incarnation, name, stored, attempts, lists, repos, agents, tries, held, error}], dueAt}`: each life a delete ended whose cleanup is not done (at most 8; `lists`: the members' lists it has still to tell; `stored`: 1 while its app's database or blobs remain; `repos`: 1 while a wipe's end of it has its repo still to delete; `agents`: 1 while an agent fragment's agent has still to leave its computer and the registry's names; `tries`: the most failed tries of a part left; `held`: one failed ten times or was refused, so it is tried daily, and by each wipe call; `error`: the last error of a part left, `part: error`), and when its next pass is due. `ended` alone answers on a name with no fragment (deleted, and not made again) |
| `POST /api/test/computer` | `{computer, op, times?, on?, ms?, freeze?}` pulls a lever on that computer (docs/computers.md): `kill` → `{computer, killed}` sends SIGKILL to the guest's PID 1 (from outside its PID namespace), so its container exits as a crash does and its real exit is reported (`killed`: the start it was); one not running (asleep, or won't wake) is 400. `saves` → what its Computer DO keeps of its saves: `saves` (newest first, at most three: `{number, id, generation, atMs, held, records, unusable}`, each with its `DirectoryBackup` records), `numbered` (the last save's number), `snapshot` (`{id, image, save}`, the cache of a save for one image, or `null`), `restored` (what its last start that came up restored, as the view has it), `rollbacks`, `ended` (`{generation, by, saved}`: how the last life ended, until the next start that comes up reads it), `starting` (`{generation, from}`: the start under way), `running` (`{generation, image}`: the image's reference the last start that came up runs), `generation` (its lifecycle's last start), `saving` (the save under way, by its step: `{step: hold \| save \| stop, since_ms, …}`, or `null`), `unsavedSince` (since when a sleep's save has kept failing, or `null`: the bound counts from it), `freeSince` (since when its awake time is free, the start of the try whose save failed first, or `null`), `keepalives` (the guest's keepalives it counts open: its turns running), `failSaves` (the lever's failures still to come) and `hangBackups` (the lever's bound on its backups, or 0). `fail-saves` with `times` (0 to 100) → `{computer, failSaves}`: its next that many saves fail before they start (a sleep's included, which then keeps its container); 0 fails none more. `hang-backups` with `ms` (0 to 480000) → `{computer, hangBackups}`: a sick container's backups (r4, r6): its awake container's `sandbox-shim` swapped for one that never answers, and its saves' backups bounded by `ms` in place of their 8 minutes, so each fails at it (`no answer within <ms> ms`); 0 puts its shim back and the bound. One not awake is 400 (0 aside). `alive-check` with `ms` (1 to 45000) and `freeze` → `{computer, aliveCheck}`: its next start waits `ms` for its guest to read its view, as a start from a snapshot does (local workerd takes none); with `freeze` its guest's PID 1 is stopped (SIGSTOP) once its container is up, so it never does: the start came up dead, and the same wake starts it again from its save (#362). `always-on` with `on` → `{computer, alwaysOn, view}`: its owner's plan made always-on, or not (decision 25: the plan itself does not reach a computer yet). Another op, or one without its argument, is 400; a computer no one made is 404 |
| `POST /api/test/mail` | a local fleet's only (a preview's mail is real): `{to, subject, text}` → `{messageId}`, one message as the platform sends it (Mail, below), to the mail fake |
| `POST /api/test/export` | `{identity, op, steps?}` (on a preview, an e2e person's): a lever on that person's export (Export, below). `pace` with `steps` lets their build take at most that many more steps (a copy's piece, a page of records, a fragment's facts…), then wait, a second at a time, until it is let go (`steps: null`): a build held part way, as a crash or a restart finds one. `piece` with `steps` (1024 to 8 MiB, or `null`) holds an entry of unknown size that many bytes at a time before putting it aside, so a small export puts its entries aside in many pieces, as a large one does. `cap` with `steps` (bytes, or `null`) makes their archive's limit that many bytes, so a small export meets it → `{pace}`, `{piece}` or `{cap}` |
| `POST /api/test/registry` | a local fleet's only: `{down}` makes the registry answer 503 (until it is set back, or the registry restarts), `{calls: null}` answers `{calls}`, how many calls the registry has had since it started (a test counts a request's round trips by the difference), `{hold: ms}` makes its next call wait that long (at most 10 s) before it is answered, while other calls go on, `{downFor: {path, times}}` makes it answer 503 to the next `times` calls (at most 100; 0 answers them again) to one of its inner paths (`/orgs/sync`: a computer's making asking for its seat's push), as `{down}` does, the rest answered, and `{signins: "count"\|"expire"\|"sweep"\|{expireSession: token}}` counts sign-in's rows (`{logins, redemptions, sessions}`), expires every pending sign-in and unspent redemption, runs its sweep now, or expires the one session a cookie's token names (a platform session's site sessions end with it) |

A request body is at most what the zone's Cloudflare plan takes (100 MB
on Free and Pro), below the 256 MiB a blob route allows (debt ledger).

Bindings (`cell/wrangler.jsonc`): `FRAGMENT`, `PRINCIPAL`, `REGISTRY` and
`LEDGER` (Durable Objects), `LOADER` (the Worker Loader), `JOBS` (the
Workflow that runs jobs), `BLOBS` (the deployment's R2 bucket: the bytes
of large files), `DELIVERIES` (the `fragment-deliveries` queue, and its
dead-letter queue `fragment-deliveries-dead`), `METERS` (the
`fragment-ledger` queue: each fragment's meter batches, consumed by the
cell), `AI` (Workers AI, through `AI_GATEWAY_ID`), `EMAIL` (Email
Sending, the `send_email` binding: Mail, below), and `BROWSER`
(Browser Rendering: preview cards, Cards below). A local node runs
without `AI`, which `wrangler dev` would only reach remotely: dev and the
e2e set `FRAGMENT_AI_URL`. Nor does it send mail: they set
`FRAGMENT_MAIL_URL`. `BROWSER` runs locally under `wrangler dev`
(its own local mode: a Chrome for Testing it downloads into its cache,
`$XDG_CACHE_HOME/.wrangler/chrome`, on the first shot; dev and the e2e
set `XDG_CACHE_HOME` to the repo's `target/cache`), a lower rung than
Cloudflare's browsers.


## Mail

The platform's own mail (cell/src/mail.rs): transactional only, one
plain-text message to one person, from the deployment's sender
(`FRAGMENT_MAIL_FROM`), through Cloudflare Email Sending's `EMAIL`
binding. `mail_from` accepts a bare address (`mail@finite.place`) or an
unquoted name and address (`Fragment <mail@finite.place>`). A name is 1
to 200 characters, without `<>,;"()[]\@` or control characters; quoted
names, comments and lists are refused. Invalid senders are refused at
config read (the node's first request), and before a deploy. A bare
sender stays a string; a named sender becomes `{email, name}`, as the
[Workers API documents](https://developers.cloudflare.com/email-service/api/send-emails/workers-api/).
A message goes to one plain address (no second recipient, no
display name), with a subject of 1 to 200 characters on one line and a
text of at most 16 KiB (`fragment_core::mail`); anything else is 400 and
nothing is sent. The service's refusals keep its code (`E_…`) in the
message: too many sent is 429, a message it will not take (an address
that bounced before, a field it refuses) 400, and the rest (an unverified
sender, an outage) 502. Dev and the e2e send to the mail fake instead
(`FRAGMENT_MAIL_URL`). On a preview, whose mail is real, a message to an
e2e person (`<name>@e2e.test`, an address with no mailbox) goes nowhere
rather than bounce. What it sends: invites by email (Sharing), seats
and trials (Billing). Each names the platform as its shell does
(`fragment_core::mail::PRODUCT`, Finite.Computer, held to the shell's
page and web manifest by a test). The sender, its address and any name,
is the deployment's `mail_from`: a name there should say the same.

## Principals and access

A principal is an identity (a person or an agent, named by the npub of
the key it was made with: docs/cloudflare-v1.md, decision 45) or an
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
of its triggered runs and is not registered.

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
body the router reads is at most 2 MiB (`limits::BODY_MAX_BYTES`), but a
model call's (`/api/models/v1/chat/completions`, `/api/models/v1/decide`), at
most 6 MiB (`MODEL_BODY_MAX_BYTES`: an image's 5 MiB data URL and 1 MiB
more), and a transcription's, at most 10 MiB of audio and 64 KiB more
(`transcribe::BODY_MAX_BYTES`); each is measured as it arrives: a
longer declared `content-length`, or a chunked body that runs past it,
is 413 before anything is authenticated.

Roles, weakest first: `public`, `viewer`, `editor`, `owner`. A member's
role is their membership. Otherwise visibility decides: on a `public`
fragment everyone holds `public`; on a `public` or `link` fragment the
share link (`?view=<token>`, which sets an HttpOnly cookie on the
fragment's origin) counts as `viewer`; a `members` fragment gives
non-members nothing. A page's navigation that brings the share link (a
GET or HEAD that its Fetch Metadata names one, `Sec-Fetch-Mode:
navigate`, whose `Accept` names HTML) is answered `302` (`no-store`)
with the cookie, on to the same path (with one leading `/`, so never
another host: `site::way_on`) and query without `view`
(`site::query_without_view`), so the token is never in a URL a page's
code reads (`location`, a card's error report): an editor's page code
cannot capture it. Nor can an app's service worker, which would see a
navigation before the origin does: a fragment's origin serves no worker
script but the platform's `__sw.js` (a request with `Service-Worker:
script` for any other path is 404), so an app registers none. A fetch,
an image, or a socket that names `?view=` still counts, as its sender
holds the token; an app's `fetch` is never handed `view`. A request with
no Fetch Metadata (a link unfurler, a browser from before 2023) reads
the page where it asked, so a share link's preview unfurls with its Open
Graph tags; in such a browser alone the token stays in the page's URL. A
frame's navigation goes on too and opens nothing, since `fragview` does
not count in a frame (Which cookies count): a share link is never framed
(no page but the platform's and the fragment's own may frame one anyway:
`frame-ancestors`). A `?view=` that opens nothing goes nowhere: its page
says the link has changed. A refusal is 401 for an unsigned caller and
403 for a signed one.

An agent's owner reads what the agent reads (FIN-11): someone who is not
a member but owns an agent that is reads the fragment as a `viewer`
(status, files, channels, events, runs, its queries, its site), and never
acts through it: a mutation or a job needs a membership of their own
(403, saying so), and they get nothing the agent's own role opens beyond
`viewer` (an editor's channels, tokens, or secrets).

An agent acts for whoever asked, capped (docs/cloudflare-v1.md, R17). A request
signed by an agent may name an identity in `for=<npub>` in its URL's
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
`for` is honored on a fragment's routes (`/api/f/{name}/…`), on `GET`
and `POST /api/fragments` (a `POST` for anyone but the agent's owner is
403: what an agent makes is its owner's), on the model routes
(`/api/models/v1/…`: Models, below), and on starting and stopping an
agent (`PUT` and `DELETE /api/computers/{id}/agents/{fragment}`: below);
on any other route it is 400.
A person's request naming `for` is 403, as is more than one `for` (400)
or one that is not an identity (400). A call without `for` acts as the
agent's own membership. A site request's query is its app's: `for`
there means nothing to the platform.

An agent shares for its owner (Paul, 2026-10-04: "your agent can share
on your behalf"; `fragment_core::access::agent_shares`). Sharing is
members (other than leaving with `DELETE members/me`), invites,
visibility, and rotation. An agent acting `for` its own owner, and not
held below them, shares a fragment its owner owns as its owner would,
under the same rules (it grants viewer or editor, never owner, and never
changes or removes the owner). Every other agent's sharing request is
403, saying why: one acting as itself or for anyone else, one its owner
holds (at any hold), and one on a fragment its owner does not own (an
editor's agent shares nothing there). Deleting a fragment and setting
its cap are 403 for every agent, whomever it acts for. What an agent
shares names it: the member's `addedBy` and the invite's `createdBy` are
the agent, and each change's event (`member.set`, `member.removed`,
`invite.created`, `invite.revoked`, `visibility`, `tokens.rotated`)
carries `by` (who made it) and, for an agent, `for` (its owner), and its
summary says "(an agent, for …)", so the owner sees what their agent
shared.

An agent runs its owner's agents (decision 62, proposed: a person's
default agent keeps the rest; `fragment_core::access::agent_may_run`).
An agent acting `for` its own owner, and not held below them, starts one
of its owner's agent fragments on its owner's computer and stops one
(`PUT` and `DELETE /api/computers/{id}/agents/{fragment}`), as its owner
would. It never starts or stops itself (403), and every other computer
route stays its owner's own (403, `for` or not): making the computer,
waking, restarting, its image, an agent's connections, ports, its uses.
One acting as itself or for anyone else, and one its owner holds (at any
hold), is 403, saying why. The agent fragment's `computer.assigned` and
`computer.unassigned` events carry `by` (the agent) and say "(started
by …, an agent, for its owner)", so the owner sees it did. What it starts
runs with every connection its owner has (decision 44), as one its owner
starts does.

Membership is cell state: `fragment.json`'s `visibility`, `editors`, and
`viewers` grant nothing (the cell records a `manifest.ignored` event).
Only the owner, or their agent sharing for them, manages members,
invites, visibility, and tokens; a member may leave. Each identity's
list of fragments is kept in its `Principal` cell, fed from each
fragment's outbox.

### Who can do what, where

Each way in acts as a principal, held to that principal's role on the
fragment (above); what costs money is paid from an owner's ledger (a
fragment's owner for its jobs' paid steps, an agent's owner for its
turns: Ledger, below). The same table is in cli/GUIDE.md, so
`/llms-full.txt` carries it (Agent docs, below).

| | call operations | read or write records | use models | decide | share and invite | deploy |
|---|---|---|---|---|---|---|
| **the person, in a browser** (a session: the shell, a fragment's pages) | `__op` and the socket, from a page (`fragment.call`), at their role; `public` ones signed out too | `__live`: the channels their role reads; a post where a channel's `post` role admits them | through their agents and their apps' jobs | through their apps' jobs | the owner, from the share sheet: members, invites by email, visibility, the share link | no route: what the shell makes (an agent, its chats, a skills fragment) is a blessed template, live as it is made; an app is an agent's (the shell's Add suggests what to ask for, `APP_PROMPTS` in cell/shell/shell.js) |
| **their agent, on its computer** (signed by its computer's egress, `for` its owner) | `POST /api/f/{name}/ops/{op}` (`fragment call`), at its own grants, or `for` its owner at the lower of theirs and its cap (at most `editor`) | channels and posts at that role; its chats are records (docs/chat-records.md) | the model route (Models), on its tier or its owner's own model, paid by its owner | the decision route (Decisions: `POST /api/models/v1/decide`, `/v1/decide` on its intercept), its media inline, paid by its owner | `for` its owner, on what its owner owns (above): members, invites, visibility, rotation; never a delete or a cap | `files` and `deploy` at that role: an editor's; what it creates is its owner's |
| **an app's own jobs** (a run of a fragment's job, or a triggered run) | its own operations (`job.call`), as the run's principal: the caller, or the fragment (an editor) for a triggered run | `job.publish` to its own channels, and a text step's draft to them as it streams (no `post` role needed); a channel trigger hands it the record | `job.ai.text` (with tools, run by the job), `job.ai.image` (AI, below) | `job.ai.decide`, about text, JSON and the fragment's blobs (Decisions, below) | none | none: `job.files.write` writes `main`, which a deploy puts live |
| **a coding agent, with the CLI** (a key a person approved: `fragment login`) | `fragment call`, as that person | `fragment channel [--follow]`, `fragment post` | its own; the platform's only through the jobs it calls | only through the jobs it calls | `fragment members`, `invite`, `visibility`, `rotate`: the owner's | `fragment sync`, `write`, `deploy`, `rollback`: an editor's |
| **an MCP client, through `fragment mcp`** (that CLI, over stdio) | the fragment's operations that have a `description`, as tools, at the key's role: its queries, and with `--write` its mutations and jobs | none but through operations | its own; the platform's through a described job | through a described job | none | none |

## Identities

The registry (`cell/src/registry.rs`: fragment's BANKS, docs/cloudflare-v1.md,
decisions 45 to 50) holds identities, the public keys each has held,
each agent's owner, and sign-in subjects with their verified emails. It
holds no grant. An identity is named, for good, by the npub of the key
it was made with: a person's is a key the registry makes at their first
sign-in and keeps sealed for them (`person:<npub>`, under the host
secret), the only private key it holds, which it never signs with yet;
an agent's is the key it was registered with (an agent fragment's own
key, which its fragment keeps). A retired key still names its identity,
and a request may name an identity by its npub or by any active key of
it. A **key proof** is a NIP-98 event by a new
key for the same method and URL as the request that carries it, with
`["p", <the signing key, 64 hex>]`: whoever sent the request also holds
the new key and meant it for this signer.

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/identities` | a person | `{kind: "agent", proof}` → a new agent identity they own, named by the proof's key and holding it; again, the same one; a key someone else holds is 409; an agent owns no agents (403) |
| `GET /api/identities/{id\|me}` | the identity, or its owner | → `{id, kind, owner?, email?, picture?, createdAt, keys: [{npub, addedAt, addedBy, revokedAt?}], agents: [id], subjects: [{issuer, email, linkedAt}]}` (`email`: a person's latest sign-in's); anyone else 404 |
| `POST /api/identities/{id\|me}/keys` | a person for themselves; an owner for their agent | `{proof}` → the identity with the key added (at most 64 keys, revoked ones included); a key someone else holds, or a revoked one, is 409 |
| `DELETE /api/identities/{id\|me}/keys/{npub}` | the same | → the identity; the key is 401 from the next request and never comes back; an agent's last active key cannot be revoked (400); a person who signs in may hold none |
| `GET /api/identities/{id}/keys/{npub}` | the identity, or an agent it owns | → `{active}` (an agent's runtime checks its owner's keys with it) |
| `PUT /api/identities/{agent}/held` | the agent's owner | `{held: "viewer" \| "editor" \| null}` → the agent: held below its owner (decision 36), it acts with at most that for whomever it acts, wherever its owner's access reaches; its own memberships (its chats, its agent fragment) are not held, so a held agent still answers and records its turns there; `null` lets it go; anyone else 403, `owner` or `public` 400 |
| `PUT /api/identities/me/picture` | a person | the image (PNG, JPEG, WebP, or GIF, told by its bytes; at most 256 KiB) → `{sha, mime}` |
| `GET /api/identities/{npub}/picture` | anyone | the picture's bytes, `public, max-age=300`: a person's (the identity's view names it, `picture`), or an agent's (An agent's name and picture, below); one with none, 404 |

### An agent's name and picture

An agent is called by its agent fragment, as live has it: its **name** is
the fragment's title (`fragment.json`'s `meta.title`, 1 to 120
characters), its **picture** the fragment's `avatar.png` (a PNG, JPEG,
WebP or GIF, told by its bytes, whatever the path says; at most 256 KiB,
`fragment_core::face`). They are the one source every page and the
platform show the agent by. The fragment's cell reads them at each
install of live into its face (`{kind, title, picture?}`: `picture` the
address it is served at, below), and that face is what is shown:

- its owner's list (`GET /api/fragments`: the agent fragment's `title`
  and `picture`), so the shell's sidebar, Settings, and the roster the
  shell hands a chat's page (a chat's own title is the chat's, never its
  agent's: The shell, below);
- `__people` and `job.people` on any fragment (an agent's `title`, and
  its `picture` whole), so a chat's authors and faces, its header, its
  `@` list, and its push notifications (their `title` and `icon`: docs/
  chat-records.md, "Push");
- the share sheet and the access view (an agent's `title`), and an
  export (its members' `title`);
- its computer's runtime: our Hermes image tells the agent its name at
  every turn, and names it in Bot Mode by its title (docs/computers.md,
  "Its name, in our Hermes image" and "Bot Mode in our Hermes image").
  Bot Mode has no picture.

Its label (`juniper` of `juniper--k3x9`) stays its `name` in a profile
and its computer's view: the word that `@mentions` it, and its runtime's
profile, which no rename changes. Its fragment deleted, its agent is
named by it no more (a profile without `name` or `fragment`: a page shows
an agent), so a life made again under the name never lends its face, nor
its `@` word, to the agent that was.

**Changing them** is writing those two files and putting them live, as
any of its files: `POST /api/f/{agent}/files` (`fragment.json` with the
new `meta.title`; `avatar.png` as `base64`, or `{path, delete: true}` to
take it away), then `POST /api/f/{agent}/deploy`, by its owner or by the
agent itself (an editor of its own fragment). Settings' Agents (The
shell, below) and `fragment agent [<agent>] [--name N] [--picture FILE |
--no-picture]` (cli/GUIDE.md) do just that. A write of an `avatar.png`
past the limit (413) or that is no picture (400) to an agent fragment is
refused, saying why, and nothing is committed; one pushed through git is
refused at its install, which keeps the last good face (its name too)
and says why in `code.error`, as `fragment deploy` prints it. A title
past 120 characters is refused the same way. Nothing restarts: the shell
follows its person's list (its watch), a chat's page reads its owner's
roster as it changes and asks `__people` again as each of an agent's
turns ends, and our Hermes image reads the title again every 10 s. A
runtime's own files never hold either (Hermes' `profile.yaml`, and the
line of its profile's config that tells it its name, are written from
the title, and written again), nor does the agent's job: its `SOUL.md`
says what it does, never who it is (the shell's first agent's names
none; one an owner or agent wrote may).

**Served** at `GET /api/identities/{npub}/picture`, `{npub}` the agent's
(its fragment's own npub): the bytes of the commit whose install checked
them, so a later commit serves nothing no install checked. Its address
names the picture (`?v=` its SHA-256's first 12 digits), so a new
picture is a new address. Who may see the agent may see its picture: it
is public, as a person's is. An agent fragment's own page shows it from
its files (`__file?path=avatar.png`).

## Names (docs/cloudflare-v1.md, decision 47)

There are no usernames. A fragment's **name** is one DNS label: its
**label**, the part its maker gives it, then `--` (which no label holds,
so no label reads as a name) and a suffix of four
characters the platform draws at random (`todo--k3x9`; the alphabet is
lowercase letters and digits less `l`, `o`, `0` and `1`), unique in the
fleet and fixed for the fragment's life. A label is lowercase letters,
digits and single dashes, not starting or ending with one, at most 57
bytes, so a name is at most 63. A name is served at `<name>.<suffix>` (a
branch's at `<name>--<branch>.<suffix>`: on a branch a name and the mark
fit 63 bytes together, or the create is 400), under the suffix's one
wildcard certificate. Its code.storage repo is `<name>--<12 hex>`, after
the deployment's prefix (a branch's `<branch>--`): the 12 hex are a
digest of its owner's identity (`fragment_core::codestorage::repo_name`,
the one place a repo's name is derived; the create keeps the repo it
made, and nothing rebuilds the name). So the same owner making a deleted
name again finds its repo, and another identity making it never finds
the repo its earlier maker made: a wiped person's repos are deleted, and
never made again for anyone.

A create names a label (`{label}`: the platform draws the suffix, three
draws at most, a draw already taken drawn again) or a name in full
(`{name}`, for a client that must know it beforehand; one taken is 409).
Every path names a fragment in full: a bare label there is 404, saying
so. The CLI finds a bare label among the caller's fragments (its list:
the one with it, or of several, the one they own), so `fragment status
todo` works; a person's labels need not be unique.

A fragment's host label, its name and a branch deployment's mark
(`--<branch>`), is one DNS label: at most 63 bytes
(`limits::HOST_LABEL_MAX_BYTES`). So the label a person may use is at
most `57 - len(mark)` bytes (`fragment_proto::label_room`): 57 on a
deployment of its own, 53 on the branch `p5`. A create past it is 400
`invalid_request`, naming the address it would have had, its length,
and how long a label may be here, and nothing is made; it is never cut
to fit (`fragment_core::names`). However it is asked (the CLI's
`fragment new`, the shell's chats and agents, an agent naming an app,
the API), the create checks it. Branches are at most 16 bytes, so a
label of 39 bytes or fewer fits every deployment
(`limits::LABEL_ROOM_MIN_BYTES`, the room the shell's labels keep to),
and a computer's host label, `<24 hex>--computer--<branch>`, fits too.

## Operators: the admin (docs/billing.md; decision 59)

The deployment's operators run it from `/admin` (a platform page, plain
like the shell, that holds nothing), from
`fragment operator people|person|pin|orgs|comp|uncomp|trials|health|log`,
or over `/api/admin/*`. An operator is: a key the deployment's
`operators` lists, held by anyone or no one (no registry is asked of one
no one holds, as a wipe's); an identity it lists, signed or through the
shell's session; or a person who holds a key it lists, through their
session too (it acts as that key may). The operators' ledger commands
(Ledger) take the same. An operator is named by their identity, or a key
no one holds by its npub: a grant's `by`, and the log's.

`GET /admin` answers the page only to an operator's platform session,
by the rule above (an identity the deployment lists, or a person who
holds a key it lists). To anyone else, signed out, signed in, or with a
session that ended, it is the 404 any missing route is, word for word
(`{"error":"not_found","message":"no route /admin"}`), so no one else
learns the page is there. Its API decides again on every call.

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/admin/people?q=&after=` | operators | → `AdminPeople {people: [AdminPerson {npub, email, joinedAt, lastSignInAt, org, admin, seat: {id, kind, comped, good}}], next}`: a page of 100 by npub, after `after`; `q`, the start of their email (any case; its wildcards only themselves) |
| `GET /api/admin/people/{npub}` | the same | → `{person: AdminPerson, ledger: LedgerStatus, computer: ComputerView \| null, references: {running, next} \| null}`: `references` are the whole references of the image its computer runs (`null` while it runs nothing) and of the one its next start runs, `registry.cloudflare.com/<account>/<repository>@sha256:<hex>` on a deployment (the view's `version` and `latest` are their first 12 hex digits), which a promotion copies into the config (docs/computers.md, "Stable and canary") |
| `PUT /api/admin/people/{npub}/computer/image` | the same | `{image}` → `ComputerView`: their computer pinned to `image`, as its owner's `PUT /api/computers/{id}/image` pins it (a canary, or back to the default): its next start runs it, and awake on another, its owner is told an update is ready. Logged (`pin`, the image its detail). An image the deployment lacks is 400; a person with no computer, 404 |
| `GET /api/admin/orgs?after=` | the same | → `AdminOrgs {orgs: [AdminOrg {id, name, createdAt, status, customer, subscription, periodEnd, cancelAtEnd, admins, paidSeats, compedSeats, pending}], next}` |
| `GET /api/admin/health` | the same | → `AdminHealth {orgsPaying, orgsLapsed, seatsPaid, seatsComped, planPushesQueued, quantityPushesQueued, failing: [{queue, target, tries, due}], lastEventAt, oldestCopyAt}` |
| `GET /api/admin/log?before=` | the same | → `AdminLog {entries: [{n, at, operator, action, target, detail}], next}`, newest first: each comp, comp's kind and end, trial code made, changed and mailed, computer pinned, and ledger command, written in its own turn; the newest 10,000 kept |
| `POST /api/admin/trials/{id}/send` | the same | `{email}` → `{sent, to}`: mails the code, its days, and `/welcome?trial=<code>`; an ended code is 400 |

Comps, orgs' views and trial codes are under Seats and orgs.

## Operators: wiping a person

The deployment's operators (`FRAGMENT_OPERATORS`: identities, and keys)
wipe a person: everything that is theirs or their agents' is deleted, and
their email and sign-in are freed, so their next sign-in is a new
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
| `POST /api/people/{person}/wipe` | the same | `{confirm, steps?}` → `WipeReport`: the wipe, as far as one call goes (20 s, `fragment_core::wipe::CALL_BUDGET_MS`; `steps`: at most that many steps). `confirm` is the identity the dry run answered: anyone else is 409, nothing changed (so an email that changed hands between the two never wipes the wrong person); none is 400. Called again, it goes on where it stopped; on a person wiped, it finds what is left (nothing) |

`{person}` is an email (the person a sign-in verified it for) or an
identity (an npub). An agent is 400 (it is wiped with its owner); no one
is 404 (an email a finished wipe freed names no one; its identity still
answers, `wiped`).

`WipeReport` is `{identity, email, state, next, found, ran, done}`:
`state` is `live`, `wiping` (begun, locked) or `wiped`; `next`, the step a
wipe runs next; `ran`, the steps this call ran, each `{step, done,
deleted, note?, cleaning?}` (`note` says what a step skipped, waits for, or why it
failed this time: the next call tries again; `cleaning`, the `cleanup` step's, below); `done`, wiped with nothing
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
  out at once), and their keys (deleted, not revoked: the key is no
  one's, 401). Until the last step, a sign-in with their account is 403,
  nothing acts as them or makes an agent for them, no one adds them or
  their agents to a fragment (404), and their sign-ins and email stay
  theirs (no one can sign as them).
- `computer`: its container destroyed, every save deleted from R2 (each
  `DirectoryBackup` record, then whatever else is under its saves'
  prefix), and its record emptied, the snapshot's id with it. Cloudflare
  deletes no container snapshot: forgotten, it is never restored, and it
  expires within 30 days (docs/computers.md).
- `fragments`: each fragment they own (their list's rows they own, and
  the agent fragments the registry names) ends
  as a delete ends it (`DELETE /api/f/{name}`, below), its code.storage
  repo recorded beside its end to delete; one someone else owns is
  skipped and named.
- `memberships`: they and their agents leave each fragment of someone
  else's they are in, as a member leaves (their membership, their
  subscriptions there), and the browser pushes they subscribed there go.
  Nothing else of that fragment's changes: what they wrote there stays
  its owner's, and its other members, someone else's agents among them,
  stay.
- `cleanup`: until each ended fragment's cleanup is done (each row their
  lists keep that no role names now; one not theirs refuses): its members'
  lists told, its app's database, its blobs, and its repo deleted
  (code.storage deletes softly, then its storage on its own; a repo it
  says is gone, or never there, is gone). The lists of the person and
  their agents are not waited on: the `lists` step empties them, and they
  take nothing after (a list of theirs that refuses every change, as p5's
  from before its table named the channels searched did, holds no wipe).
  Each call is the retry: every part left is tried once a call. A part
  that fails is told apart (`fragment_core::ended`): a refusal the same
  call cannot pass (a 4xx) is held at once, any other failure after ten
  tries (backing off between the alarm's tries), and a held part is
  tried daily by the fragment's alarm, and by each wipe call. The step's
  `Ran` names each fragment not cleaned yet, in `cleaning` (at most 50):
  `{fragment, lists, stored, repo, tries, held, error?}` (`lists`: the
  members' lists still to tell; `stored`: its app's database or blobs
  remain; `repo`: its repo is still to delete; `tries`: the most failed
  tries of a part left; `held`; `error`: the last error of a part left,
  `part: error`, at most 300 characters), and its `note` says the same in
  a line: `failed: …` once a fragment is held, naming it and its error,
  else `still cleaning …`.
- `ledger`: emptied in one step; it keeps one row saying it was wiped and
  refuses everything after (404, `Refused::Wiped`), so a meter batch still
  in the queue is acknowledged and kept nowhere.
- `lists`: their export first (Export, below): its upload aborted, and
  everything under `exports/<npub>/` in R2 deleted (ten pages a call; each
  call after deletes again what a build cut short wrote late); then their
  and their agents' lists (`Principal`s) emptied the same way: a change
  still on its way from a fragment is taken and kept nowhere.
- `pictures`: their picture's bytes, unless another identity set the same.
- `registry`, in one turn: every row naming them or their agents
  (identities, keys, sign-in subjects and emails, picture, agent fragments,
  sessions, consents). The wipe's own row stays: the identity, its times,
  and the operator.

Their next sign-in with the same account finds no subject: a new person,
a new identity, so a new list, ledger and computer (each is named by it),
for whom the platform's page makes a default agent. Their ledger starts on `FRAGMENT_DEFAULT_PLAN`:
no grant, plan or seat of the old one's carries over. A fragment made
under a name the old person used gets a new repo (Names, above).

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

## Export

A person downloads everything that is theirs as one file (Paul,
2026-10-13): from Settings, Your data (The shell, below), or `fragment
export [--out <file>]`. Its archive is the reference for what a person's
data is, and the format a later import reads. It adds no power to anyone:
only the person, signed in as themselves, starts, reads or downloads
theirs; an agent cannot, whomever it acts for, and there is no operator's
export, nor a lever that reads one past its person (cell/src/export.rs).

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/export` | a person, as themselves: the shell's session, or a key of their own (an agent is 403; `for` is 400; unsigned 401) | no body, or `{}` (any other is 400) → `ExportView` (`fragment_proto::export`): a new export, building. While one builds, that one, and nothing starts (a press twice is one export). A fourth start in any 24 hours is 429 `rate_limited`, saying when the next may start (`limits::PER_DAY`: 3). A new one replaces the last: that one's archive is deleted, and it is `expired` |
| `GET /api/export` | the same | → `{export: ExportView \| null}`: their newest |
| `GET`, `HEAD /api/export/{id}/download` | its person: signed, the shell's own fetch, or their click on the platform's own page (a navigation, `Sec-Fetch-Mode: navigate`, that page started, `Sec-Fetch-Site: same-origin`, with their platform session) | → the archive, `application/x-tar`, `Content-Disposition: attachment; filename="fragment-export-<YYYY-MM-DD>.tar"`, `Cache-Control: private, no-store`; a `Range` answers 206. Anyone else's export, one not ready, and one gone are 404 (an export is looked up in its asker's own list); another page's request carrying the person's cookie (a fragment's page, one site with the platform; another site's) is 401 |

`ExportView` is `{id, state, startedAt, progress: {done, total, bytes},
size?, readyAt?, expiresAt?, error?: {code, message}, download?}`: `state`
`building`, `ready`, `failed` or `expired`; `progress` its parts done (its
README and manifest, each fragment, each computer) and the archive's bytes
so far; `download` the path above while it is ready; `error`'s `code` one
of `too_large` (the archive, or one fragment's part of it, would pass its
size), `too_many` (fragments, files, blobs, tables or entries past their
limit), `unavailable` (a part kept failing for a reason that may pass, or
its person cannot be read, as while a wipe of them runs), `source_changed`
(a save of its computer, or a blob, went away as it was copied: export
again) and `timed_out`.

**What it reads, and how.** As its person, through the routes their own
requests take: each fragment's is handed to the fragment as the router
hands a request they signed (`Routed`), so the fragment decides their role
and meters the request to its owner, and nothing is read around it (a part
sealed at rest later is read as its owner reads it):

- who they are: the registry's view of their identity (their email, their
  agents), which refuses a person a wipe of whom runs;
- their list and their agents' lists: the fragments they own, and their
  agents own (each kept whole); and every fragment someone else owns that
  they or an agent of theirs is in (`sharedWithMe`: named, its role and
  whose membership, and its owner from its `status`; never copied);
- each fragment kept: `status`, `members` (each named as its owner's
  share sheet names them, through the registry's profiles: a person by
  their email, with their agents, since a share is a person's and their
  agents'; an agent by its name and its owner's email), `invites`,
  `secrets` (names), `channels` and each channel's records (pages of 100
  after its cursor, up to the newest its listing named), `files` and
  `file` at main (its working copy, which the file API reads: an agent's
  memories land there) and each blob by its hash (`blobs/{sha256}`, a
  range at a time), and `database` (above) for an app with code;
- their computer: its view, and its newest save that restores, read
  through its Durable Object (its internal `computer/export` and
  `computer/export-read`, each checking its owner), a range at a time.

A part a fragment refuses its person (an agent's fragment's invites,
which only its owner reads) or does not have (no app code, so no
database) is named in that fragment's `meta.json` (`skipped`), and the
rest goes on.

**The archive** (`fragment_core::export`): one tar, not compressed (a
ustar header per entry, a pax header naming a path ustar cannot hold),
every entry under one folder, `fragment-export/`. Its `README.md` says
what each file is, in what format:

    fragment-export/
      README.md                     the layout, and every format
      manifest.json                 its format and version, the deployment, when, whose
                                    (npub, email), their fragments, agents, computers,
                                    and sharedWithMe
      fragments/<name>/             each fragment they own, and each their agents own
        meta.json                   kind, template, owner, visibility, members, invites,
                                    whether share links exist (never the tokens), secret
                                    names (never values), and what each part holds
        files/<path>                its files at main, a blob's bytes whole
        database/schema.sql         its app's schema
        database/<table>.jsonl      one row a line, a JSON object of its columns
        channels/<channel>.jsonl    one record a line: {seq, at, principal, kind, body}
        transcript.md               a chat's chat and work records, to read
        blobs/<sha256>              the files its records attach
      computers/<24 hex>/
        meta.json                   its view, and the save exported
        data.tar.zst                that save's /data (but /data/work), as stored
        data-work.tar.zst           that save's /data/work, as stored

Why a tar: it is written front to back in fixed parts, and a header names
its entry's size before its bytes, so nothing after an entry changes it
(zip wants each entry's CRC before its bytes, or a data descriptor a
stored entry's reader cannot find its end by, and a directory of them all
at the end). Every system opens one, and a computer's saves are tars
already. Not compressed: most of its bytes (saves, pictures, audio)
already are.

**Where it runs.** In the person's list (`Principal`), on its alarm, a
slice of 20 s at a time, its place kept in the list between slices (the
export's row and job, the archive's bytes not uploaded yet, the parts
uploaded), so a crash, a deploy or a restart goes on from the last slice
kept, and a slice cut short runs again. The archive is an R2 multipart
upload at `exports/<npub>/<id>/archive.tar` in parts of 8 MiB; an entry of
unknown size (a channel's records, a table's rows, a transcript) is held
to 8 MiB, then put aside at `exports/<npub>/<id>/spool/<n>` and copied in
once its size is known; a stored source (a save, a blob) is copied 8 MiB
at a time. It holds at most a part, a piece and a read at once, whatever
it exports. A part that fails for a reason that may pass is tried again
(5 s, doubling to 5 minutes), 8 tries in all, then the export fails
`unavailable`.

**Limits** (`fragment_core::export::limits`), each a typed failure the
view shows:

| Limit | Value |
| --- | --- |
| the archive | 8 GiB (`too_large`) |
| one fragment's part of it | 2 GiB (`too_large`) |
| fragments kept (theirs and their agents') | 1000 (`too_many`) |
| fragments named in `sharedWithMe` | 2000 (`too_many`) |
| files in one fragment's main | 20 000 (`too_many`) |
| blobs one fragment's records attach | 10 000 (`too_many`) |
| tables in one app's database | 256 (`too_many`) |
| entries in the archive | 200 000 (`too_many`) |
| a build's time | 6 hours (`timed_out`) |
| exports started in any 24 hours | 3 (429) |
| a ready archive kept | 7 days |

**Once it is ready** its person is mailed (Mail, above) a link to
`/settings`, never one to the archive, and the archive is kept seven days,
then deleted (`expired`), as it is when a newer export starts, or a wipe
of its person runs (Operators, above: `lists`).

**Metering** (decision 24): its archive's R2 storage (its bytes from ready
to deleted, as byte-hours) and its requests (its R2 operations and its
downloads) are one meter batch on its person's ledger, `export:<id>`, as
it is deleted (a row late by up to its seven days counts in its own
month). Each request it makes of a fragment is that fragment's, metered
to its owner as any request (a fragment shared with them, its status:
its owner's). A guest's ledger takes none.

## Sign-in

A verified `(issuer, subject)` signs a person in: the issuer is
`workos:<client id>` (the environment), the subject WorkOS's user id. The
first one makes the person, named by a key the registry makes for them
(Identities, above). WorkOS must have verified the sign-in's email (403
otherwise); it is kept lower case and refreshed at each sign-in, the
latest shown, and it names at most one person: a sign-in whose email is
another person's is refused (409), never merged into them. Browsers hold
sessions, each looked up
live in the registry on every request whose answer depends on who is
asking (Principals and access, above; a 30-day lifetime; tokens are 32
random bytes, the registry keeps their SHA-256).

| method & path (platform origin) | what |
| --- | --- |
| `GET /`, `GET /settings`, `GET /welcome` | the shell's page, for anyone (The shell, below): signed out it asks them to sign in (and back to the same address, its query included), or to use another account (`POST /auth/switch`); for a new guest it is the welcome, and for a seat with no chat it makes their first agent; at `/settings` it opens its settings, at `/welcome` the welcome |
| `GET /auth/login?return=&login_hint=` | → WorkOS's authorize URL (`provider=authkit`, `redirect_uri` `<platform>/auth/callback`, a state); the state is bound to the browser by `fragment_login` (HttpOnly, SameSite=Lax, `Path=/`, ten minutes) |
| `GET /auth/link?return=` | the same from a signed-in browser: the sign-in that comes back joins this person (409 when it is someone else's) |
| `GET /auth/callback?code=&state=` | the state must match the browser's cookie (400 otherwise); the code is exchanged server-side; → `fragment_session` (HttpOnly, SameSite=Lax, `Path=/`) and back to `return`; a WorkOS `error` is shown (400); a sign-in already finished or past its ten minutes, or a code WorkOS refuses (a callback sent again), is 400 `invalid_request` |
| `POST /auth/logout` | ends the session and every fragment session made from it, clears the cookie, and returns a non-cacheable page (200) that navigates to WorkOS's logout (`session_id` from the access token's `sid`, `return_to` `<platform>/`, which must be a Sign-out URI of the environment's application), which ends WorkOS's session too. With no live session or WorkOS session id, redirects home; from another origin, 403 (`GET` shows the button) |
| `POST /auth/switch?return=` | "Use another account": AuthKit keeps its own session in the browser (a cookie on its own domain, which clearing the platform's site data leaves, and which another application of the environment may have made), and answers a sign-in from it without asking anyone. A switch ends this browser's session here (if any), then signs in as `/auth/login` does, its state bound by `fragment_switch` in `fragment_login`'s stead; where WorkOS answers from a session it already held (the access token's `auth_time`, the last time its person actively authenticated, is before the switch began, or absent), no one is signed in, and the browser goes to WorkOS's logout with that session's `sid` (as `POST /auth/logout`), so the next sign-in asks who is signing in; where someone signed in meanwhile, they are signed in. From another origin, 403 |
| `GET /auth/fragment?name=&return=` | signed in: → `<fragment origin>/__signin?token=<a single-use redemption, 60 s, for that fragment only>` (a session holds at most 16 unspent; past that, the oldest is refused), at once for a fragment of the person's own, one shared with them, or one they said yes to; for any other, first a page asking "Continue to X?" (Asking first, below); signed out: → sign in first |
| `POST /auth/fragment?name=&return=` | that page's form (`form`, its token): the yes, remembered, then → the fragment's `__signin?token=` (303); another origin, or a missing or stale token, 403 |
| `GET /auth/frame?name=&return=` | a frame of the platform's own page (the shell's tabs; Frame sessions, below) signs in to the fragment: → its `__signin?token=<a frame redemption>` (`Cache-Control: no-store`, `Referrer-Policy: no-referrer`) where `/auth/fragment` would redeem at once; where it would ask first, or no one is signed in, a note in the frame. Anything but a frame of the platform's own page (by Fetch Metadata) is 403 |
| `GET /cli?key=<npub>&proof=` | the link `fragment login` prints: `proof` is the key's own NIP-98 event for `POST <platform>/cli/approve`, good for ten minutes (the proof of possession; without it, stale, or by another key: 400). Signed in: a page showing the key's last eight characters, to compare with the terminal, and an Add button; signed out: → sign in first, keeping the link |
| `POST /cli/approve` | the page's form (`key`, `proof`): the key joins the signed-in person at once; a key someone else holds, or a revoked one, is 409; another origin 403; the CLI waits for `GET /api/identities/me` to answer. People themselves come only from sign-in (`POST /api/identities {kind: "person"}` is 400) |

The logout and switch POSTs leave the shell through a non-cacheable page
(200) with a `Refresh` header, a Continue link, and `Referrer-Policy:
no-referrer`. Committing that page before navigating to WorkOS keeps the
shell's `form-action 'self'` from blocking an external HTTP redirect in
Chromium.

On fragment.club the platform is cross-site from every fragment
(`fragment.club` and `<name>.fragment.boats`), so its
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
it), and every form here (`/auth/logout`,
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

The share sheet is the platform's page, never a fragment's: a
fragment's page is its author's code (or an agent's), and sharing
grants. It acts through the fragment's own routes (members, invites,
visibility, rotate; below) as the person signed in on the platform, so
the fragment decides who may do what. There is no page to accept an
invite on: an invite waits on an email, and a sign-in that verifies the
email meets it (Invites by email, below).

| method & path (platform origin) | what |
| --- | --- |
| `GET /share/<name>` | the share sheet, a card laid out as a document's share dialog, titled with the fragment's title: who is in (emails and pictures, from the registry's profiles: a member sees the members' emails, decision 48; an agent by its title, and whose agent it is, by their email: its owner reads what it reads) and their roles, to any member (anyone else, a 403 page); for the owner ("Share …"), adding people by email with a role (Invites by email, below), the invites waiting (each email, its role, the days it waits; Revoke), each member's role (Viewer and Editor buttons, theirs pressed) and Remove (a chat's own agent has neither: it keeps its place), each person's own agents named under them (their share is theirs too), and who can open it ("General access": three buttons, Restricted `members`, Anyone with the link `link`, Public `public`, each saying what it means, the current one pressed); for a member who is not its owner ("Who has access to …"), the same read-only, and Leave. Every choice is a button (no `<select>`, which an arrow key changes as it passes over it) and is sent as it is pressed; Remove, New link and Leave ask a second click. Copy link (the share link while it is `link`, else its address: a public one opens for anyone without its token) and Done (in a dialog, closes it, as Escape does; in a window of its own, closes that, or goes to `/settings`); then, quieter, while it is `link`, a new share link. Signed out: → sign in first, and back. It reads nothing from its URL |
| `POST /share/<name>` | the sheet's form: `form` (the page's token), `action`, and its fields: `invite` (`email`, `role`: `POST invites`; the sheet says whether they are in now, were mailed, or were mailed again; an email someone in it signs in as already, the owner's own included, changes nothing and says so: their role changes in the list), `role` (`member`, `role`: `viewer` or `editor`), `remove` (`member`), `uninvite` (`email`), `visibility` (`visibility`), `rotate` (the share link only; the inbox's token is the CLI's: the sheet, saying so), `leave` (a member's own, `DELETE members/me`: a page saying they left, whose Done closes the dialog). Done: → 303 back to the sheet; refused by the fragment (a member who is not the owner: 403; the owner leaving: 400; a day's mails spent: 429): the sheet, saying why, with the refusal's status |

The sheet cannot be driven by a fragment's page. Every POST's `Origin`
must be the platform's (403 otherwise), and every POST carries `form`,
the token its page was made with: an HMAC, keyed by the session's own
token (the HttpOnly cookie, which no page's script reads), of what the
form does (`share:<name>`) and when its page was made
(`fragment_core::form`). A form without it, another session's, one for
another fragment or page, one sent sooner than 800 ms after its page was
made, or one older than 12 hours is 403. Its buttons come
disabled and arm 800 ms after the page shows (again each time it is
shown), so the click that opened it (a double-click's second half)
cannot confirm in it. It sends no CORS headers (a fragment's page cannot
read it, so it never holds a form's token), refuses every frame but its
own on the platform's origin (the shell's dialog: `frame-ancestors
'self'`, `X-Frame-Options: SAMEORIGIN`; every fragment is another
origin), severs its opener, allows scripts and styles only inline and
images only from the platform (`Content-Security-Policy`), and keeps its
URL to the platform (`Referrer-Policy: same-origin`).

### Invites by email

Sharing is by email (docs/cloudflare-v1.md, decision 48). The owner (or
their agent for them) names an email and a role, viewer or editor
(`POST /api/f/<name>/invites`; the sheet's Add, `fragment members add
<name> <email>`):

- The person who signs in as that email (its verified holder) is a
  member at once, as `PUT members` makes one: `{member}`. Their agents
  come with them: a share is the person's and their agents' (Paul,
  2026-10-13), each agent acting for them with at most its cap (R17).
- Someone in it already is not invited again: 400 `invalid_request`,
  saying they are in and as what (the owner's own email: that they own
  it). An invite adds; a role changes only with `PUT members` (the
  sheet's list, `fragment members role`).
- When no one does yet, the invite waits on the email for 30 days
  (`INVITE_TTL_S`) in the fragment, and the registry keeps it against the
  email and mails it a link to the fragment's address: `{invited}`. Their
  first sign-in that verifies the email (a WorkOS sign-in, an
  `/auth/link` adding it, or an e2e person's) makes them a member at the
  role it said, added by the invite's maker, and a stronger role they hold
  is kept (the invite is spent either way). A claim the fragment cannot
  take then (it is unreachable, or at `MEMBERS_MAX`) keeps the invite
  waiting, and the registry's alarm tries it again for whoever holds the
  email, backing off (2 s, doubling to ten minutes), 12 tries in all;
  past them their next sign-in tries again. The link signs them in and
  lands them there. A forwarded mail
  admits no one else: only a sign-in as the email meets the invite.
  Signing in never makes a share; it meets one addressed to its email.
- An invite that runs out waits no more: the fragment's alarm drops it at
  its expiry, and its owner's list counts it no more (`sharing.invites`,
  the shell's "invite waits"), as when it is met or revoked.
- Inviting the same email again renews the invite (its role, 30 days
  more) and mails it again. Revoking it (`DELETE invites/<email>`) takes
  it back; a mailed link then opens nothing for them.
- A fragment holds at most `INVITES_MAX` (100) invites waiting, and an
  email at most `INVITES_PER_EMAIL_MAX` (100) across the fleet (429 past
  it). A person can have at most `INVITE_MAILS_DAILY_MAX` (50) mailed a
  day, an agent's counted in its owner's day (429, saying so). A mail the
  service does not send takes the invite back and says why.
- No event names the email (a `link` fragment's viewers read its
  events): `invite.created` and `invite.revoked` say a role and who made
  them, and `member.joined` names the npub that met it.
- An e2e person's address (`<name>@e2e.test`) on a preview is mailed
  nowhere (Mail, below): its sign-in meets the invite as a person's would.

Agents have no email: they are added by npub (`PUT members`).

### Who has access to what

`GET /api/access` (a person, signed or the shell with its session; an
agent is 403, and `for` is 400) answers who may open each fragment the
person owns, in their list's order: `{fragments: [{name, kind, title?,
sharing?, agents?, members?, more?, invites?}]}` (proto's `Access`).
`sharing` and `agents` are their list row's (`GET /api/fragments`). A
fragment someone else may be in or invited to (its `sharing` counts a
guest or an invite, or was sent before invites were counted) is asked
for its members and invites as the person, its first
`ACCESS_ASKED_MAX` (64) at most, `ACCESS_ASKED_AT_ONCE` (16) at a time:
`members` is everyone in it but its owner (at most `ACCESS_MEMBERS_MAX`,
50; `more` counts the rest), each `{principal, role, kind, email?,
name?, title?, owner?, ownerEmail?, agents?}` (a person by their email,
with their own agents, `agents: [{principal, name?, title?}]`, who reach
what they do; an agent by its label, `name`, its title, `title` (its
agent fragment's, for at most 64 agents a view: Identities, "An agent's
name and picture"), and its owner's email: its owner reads what it
reads), and `invites` the invites waiting, as `GET
invites` answers them. Any other fragment wakes for none of it: it has no `members` (no
one else is in it, or it is past the bound; its sheet names them). At
most `ACCESS_PROFILES_MAX` (512) identities are named by email, the
first fragments' first; past it a member has no `email`. A fragment
that does not answer (deleted meanwhile) keeps its list's counts. The
shell's settings show it, by fragment and by person, each fragment's
title opening its share sheet.

## Control API

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/fragments` | a person, not a guest; an agent for its owner (the fragment is the owner's, billed to them, with its maker an editor)
 | `{label | name, visibility?, template?, title?}`: a label, which the platform names with a random suffix, or a name in full (Names, above) → `{name, npub, owner, visibility, viewToken, inboxToken, repo, canonical}` (`name` in full). A name whose host label (its name, and a branch's mark) would pass 63 bytes is 400 `invalid_request`, saying why, and nothing is made (Names, above). Its maker's ledger is asked first (`Spend::Create`): a guest's create is 403 `forbidden`, "guests can't create fragments: …" (Paul, 2026-10-03: a fragment's hosting bills its owner, and a guest pays for nothing; a guest still edits fragments shared with them), however it is asked (a template's, an agent's for its owner, the shell's catalog), and nothing is made; past the overdraft it is 402 `budget_used_up` (the maker's fragments are read-only). A ledger that does not answer refuses none. `visibility` defaults to `link`, but to `members` for a chat or an agent (its SOUL and memories: Paul, 2026-10-13; `Visibility::made_from`). The fragment's own key is made in its cell and kept sealed for it. The cell creates (or, for a name its owner deleted before, finds) the code.storage repo, named for its owner (Names, above). With `template` (`blank`, `todo`, `inbox`, `calories`, `doc`; any other is 400 and nothing is made), the template's files are main's first commit (its `fragment.json` stamped with the fragment's name, and `title`, if given, as its `meta.title`) and live at once: the create answers once they are (one seed at a time: the alarm, armed during the create, seeds only a template still to land); one that fails to land is retried by the fragment's alarm (`template.failed` events). `agent`, `chat`, `brain`, `skills` are blessed (decision 40), named and not copied: main's first commit is `{"template", "meta": {title}}`, and the platform's release serves the rest (Apps; a brain: templates/brain/README.md). `notes` is the CLI's only (`fragment new --template notes`). A `title` (1 to 120 characters) needs a `template` (400 without one): a fragment made bare says its title in its own `fragment.json`. |
| `PUT /api/fragments/{name}/archived` | any signer, for a fragment they hold a role on | `{archived: bool}` → `{name, archived}`: the signer's own view of it (the shell leaves it out of its sidebar; search still finds it), kept in their list's row and nowhere else, so no one else's list or the fragment changes. The same again answers the same. A bare label names nothing (404: a fragment is named in full); a fragment they hold no role on, or none of that name, is 404; a name that is none, or a body without a boolean `archived`, 400. It goes when they leave the fragment (back in, it is not archived), or the fragment is made again. Not honored for `for` |
| `PUT /api/fragments/{name}/seen` | any signer, for a fragment they hold a role on | no body → `{name, unread: 0}` (`Seen`): the signer has seen its messages, up to the newest their search holds (Search, below), so their row counts none `unread` until another comes; kept beside their list's row and nowhere else, so no one else's list or the fragment changes. The place never goes back: the same again answers the same and tells no page; a mark that moved it tells the signer's list sockets (`changed`). A bare label names nothing (404, as for archiving); a fragment they hold no role on, or none of that name, is 404; a name that is none, or any body, 400. It goes when they leave the fragment (back in, every message it still logs for them is unread), or the fragment is made again. Not honored for `for` |
| `GET /api/search?q=` | any signer | → `{fragments: [ListedFragment], messages: [{fragment, channel, seq, at, snippet}]}` (`SearchAnswer`): the signer's fragments whose title or label hold every word of `q`, then the messages that do, newest first, from fragments they hold a role on now, archived ones included (The shell, Search, below). `q` once, at most 256 bytes and 8 words (400 past either, or without it). Not honored for `for` |
| `GET /api/fragments` | any signer | → `{fragments: [{name, role, kind, title?, picture?, agents?, preview?, previewAt?, unread?, sharing?, archived?}]}` (`archived: true` on the ones the signer archived); `picture`, an agent fragment's only: where its picture is served (Identities, "An agent's name and picture"), as its last install said; `agents`: its agent members, the first added (a chat's lead) first, at most 16 (`LISTED_AGENTS_MAX`), as the fragment last sent them (an agent's joining or leaving sends every row; a row sent before rows named them has none until it is sent again); `preview`, a chat's only: the first line with words of its newest message the signer's search holds (Search, below), at most 160 bytes, none when it holds none; `previewAt`, with `preview`: when that message was said (ms since the epoch, as a record's `at`); `unread`, a chat's only: its messages the signer's search holds after the last they marked seen (`PUT …/seen`, above; all of them before any mark), at most 100 (`LISTED_UNREAD_MAX`), left out at 0; a search entry names no author, so a message of the signer's own counts until they mark it; `sharing` on the signer's own fragments only: `{visibility, members, guests, invites}` (guests: members who are neither the owner nor an agent of theirs; invites: those waiting on an email, as of that change, absent in a row sent before they were counted; an invite met, revoked, or run out is such a change, the last sent by the fragment's alarm at its expiry), as the fragment last sent it with a change to its members, invites or visibility (who they are: `GET /api/access`, below); an agent's `?for=<id>`: the fragments that identity holds a role on where the agent or its owner is a member too, each with the role the agent acts with there for it (`fragment_core::access::listed_role`; a call decides again) and `owned: true` on those that identity owns |
| `GET /api/access` | a person (not an agent) | → who may open each fragment they own, by email (Sharing, "Who has access to what") |
| `GET /api/fragments/watch` | any signer; the shell with its session (below) | a WebSocket, upgraded; anything else is 400. It answers `{type: "hello"}`, then `{type: "changed"}` each time the signer's list changes: a fragment made, shared with them, changed (its title, kind, agents, sharing, their role), left or deleted, their archiving, and a chat's message new to their search (its preview) (principal.rs, Watching). A frame names nothing: the page reads `GET /api/fragments` again with its own credential, so a socket that outlives its session learns only that something changed. The platform session counts only on the platform's host with the platform's exact `Origin` (a browser names its page on every upgrade; a fragment's page, one site with the platform, is refused like no one: 401). A list holds `LIST_WATCHERS_MAX` (16) at once; one more is 429. It reads nothing from the client. Not honored for `for` |
| `DELETE /api/f/{name}` | the owner (never an agent) | → `{ok, deleted}` once the fragment is gone: from then it is 404 to everyone, its owner's list no longer has it, and its name can be made again. Its other members' lists, the app's database and the blobs go after, by the fragment's alarm (seconds; each part retried, backing off, until done: one that fails ten tries in a row, or is refused, is held and tried daily, its last error kept, as the `ended` lever shows), so a delete answers as soon at `MEMBERS_MAX` members as at one: it tells at most one round of lists itself (32 at once). An agent fragment's agent leaves its computer with them (`computer/unassign`, that agent alone: its owner's agents, a chat's @ roster and Settings run it no more, nor does its guest, and it is counted in the computer's cap no more; its owner's list is told) and the registry names it by the fragment no more (Identities, "An agent's name and picture"; its identity stays, its key held by no one); a computer gone (its owner's wipe) is done. A fragment made again meanwhile under the name is untouched by the old one's cleanup: a new agent of its own, assigned again, keeps its place, and its lists' rows show its own face (none until it has one: an app, untitled), never the ended life's. The repo stays |
| `GET /api/f/{name}/status` | viewer | → `{name, kind, title?, npub, owner, role, visibility, repo, pins: {main, live}, counts: {files, events, members}, code: {sha, id, operations: {<op>: {kind, role, input?, ephemeral?, description?}}, error}, viewToken (its owner, or their agent sharing for them), inboxToken (editor), urls: {canonical, platform}, blobMinBytes, page}`; `kind` and `title` are what it is and its title as its members' lists show them (an app, and no title, until it says); `viewToken` is the share link's token, its owner's to give, as the share sheet has it: a member asks them (2026-10-13); `code.sha` is the live commit installed and `code.id` the code that runs (`app:<hash>` of its `app.mjs` and `applib/`, or a blessed template's `blessed:<template>@<release>`); `code.operations` are the installed manifest's, each as it declared them (Apps, below; `input`, `ephemeral` and `description` absent when it gave none); `urls.platform` is the platform's own origin, for links a person opens (a client in a computer calls an internal host); `page` is `{live, at, errors: [{kind, text, source}], dropped}`, what the page reported as its preview card's shot loaded it (Cards, below), absent before the first |
| `GET /api/f/{name}/manifest` | viewer | → `fragment.json` at main (404 when there is none) |
| `GET /api/f/{name}/members` | viewer | → `{members: [{principal, role, addedBy, addedAt, kind, owner?}]}` (`owner`: an agent member's) |
| `PUT /api/f/{name}/members/{id\|npub\|email}` | owner, or their agent for them | `{role: viewer\|editor}` (any other field is 400) → the member: added, or their role changed (the one call that changes a role); a key names the identity holding it, an email the person who signs in as it (404 when no one registered it, or signs in as it: `POST invites` mails them). A person's share is theirs and their agents' (Paul, 2026-10-13: "all the pubkeys under the umbrella of this email"): each of their agents acts there for them with at most its cap (R17); an agent added by its npub is that agent alone. A chat's own agent (its lead, the first agent added) is never set below `editor` (400, saying why): its replies and steps post there. A new member that is an agent running on a computer is announced to it: `joined` on its agent fragment's `tasks`, and a wake (Computers, below) |
| `DELETE /api/f/{name}/members/{id\|npub\|email\|me}` | owner, or their agent for them; or the member | → `{ok, removed}`; closes that member's change feeds (and its owner's, when an agent's membership was their only view). A chat's own agent (its lead, the first agent added) is removed by no one else (400, saying why): it may leave itself (`me`, as its owner's wipe has it do) |
| `POST /api/f/{name}/invites` | owner, or their agent for them | `{email, role}` → `{member: Member}` (someone signs in as it: a member now) or `{invited: {email, role, expiresAt, createdBy}}` (no one does yet: it waits on the email, mailed); Invites by email, above. Someone in it already, the owner included, is 400, saying so (a role changes with `PUT members`); a role they cannot grant is refused as `PUT members` refuses it |
| `GET /api/f/{name}/invites` | owner, or their agent for them | → `{invites: [{email, role, expiresAt, createdBy}]}`: the invites waiting, oldest first |
| `DELETE /api/f/{name}/invites/{email}` | owner, or their agent for them | → `{ok, revoked}`; 404 when none waits on it |
| `PUT /api/f/{name}/visibility` | owner, or their agent for them | `{visibility}` → `{ok, visibility}`. Leaving `members` (for `link` or `public`) makes a new share link: one that stopped working when it was restricted stays dead (the event's `newLink`) |
| `POST /api/f/{name}/rotate` | owner, or their agent for them | `{scopes?: [inbox, view]}` → `{inboxToken, viewToken, rotated}` (`Rotated`): every token as it is now, and the scopes renewed; a new view token closes link holders' feeds |
| `PUT /api/f/{name}/secrets/{KEY}` | editor | `{value, hosts}` (`PutSecret`: a value of at most 64 KiB, and 1 to 8 hosts it may go to, each as a URL names it, `api.example.com`: no scheme, port, path, or wildcard) → `{ok, name, hosts}`; the value sealed (AES-256-GCM, key HKDF'd from the host secret salted with the fragment's Durable Object, its class and id: `keys::scope`, `fragment_core::seal`; docs/secrets.md), its hosts in place of the ones before. `job.fetch` fills it only toward one of its hosts (on any port), over https (`fragment_core::secrets::may_fill`), in a request that sets no `Host`, `Forwarded`, or `X-Forwarded-Host` header (`ROUTING_HEADERS`). Its hosts change only with its value, so no editor widens where a value goes without holding it. They are its editors' to know: the `secret.set` event (every viewer reads the event log) and a refused fetch's error name no host. A secret set before secrets named hosts fills nowhere until set again |
| `GET /api/f/{name}/secrets` | editor | → `{secrets: [{name, hosts}]}` (`SecretList`); values never leave |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor | → `{ok, removed}` |
| `GET /api/f/{name}/storage-token` | editor | → `{token, repo, api, expiresAt}`: ES256, this repo, `git:read`+`git:write`, 15 minutes |
| `POST /api/f/{name}/refresh` | editor | → `{ok, refs: {main: {pin, moved} \| {absent}, live: ...}}`, once a live that moved is installed (`fragment deploy` asks it). A writer to the repo asks this after its push (the CLI does), and the platform follows its own moves itself: code.storage's push webhooks are not taken. A fragment reads the branches it has no pin for once, on its first request (a name made again keeps its repo); after that a move arrives by this or the poll backstop, and a site with nothing deployed answers 404 without asking code.storage |
| `POST /api/f/{name}/files` | editor | `{files: [{path, text \| base64} \| {path, delete: true}], message?, key?}` → `{commit}`: one commit to main, as a sync makes (at most 16 files, 400 past that, and 1 MiB of their decoded bytes in a body of at most 2 MiB, 413 past either; paths relative, no `.` or `..`). The same `key` from the same person answers the first commit again. An agent fragment's `avatar.png` that is no picture (400) or past 256 KiB (413) is refused and nothing is committed (Identities, "An agent's name and picture"). A write of what main holds already commits nothing (code.storage makes no empty commit: its 412) and answers main's tip. Main's pin moves at once; live does not |
| `POST /api/f/{name}/deploy` | editor | `{note?}` → `{live, canonical}`: live to main's tip; `fragment deploy` asks it after its sync, so every deploy is this one (a first deploy makes the branch; later ones fast-forward it, and after a rollback take main's files whole: a restore commit, or one of their merge base and then the merge, since code.storage merges three ways), guarded against a live that moved meanwhile and made under the fragment's plane lock, so neither step's push pins the files live holds between them; the app installs from it at once |
| `GET /api/f/{name}/files` | viewer | → `{ref, files: [{path, size, mode, lastCommitSha, machinery, blob?, sha256?, release?}]}` at main; a pointer's `size` is its bytes', and its `sha256` the blob that holds them (`GET blobs/{sha256}`). A fragment on a blessed template lists that template's data from the release beneath its own files (`release: true`, `lastCommitSha` `release:<hash of its bytes>`: templates/skills/README.md) |
| `GET /api/f/{name}/file?path=` | viewer | → the bytes at main (`x-fragment-ref`); a pointer's come from the blob store; with none of its own at `path`, a blessed template's data file from the release |
| `PUT /api/f/{name}/blobs/{sha256}` | editor | the bytes as the body (`content-length` required, at most 256 MiB), streamed through and hashed on the way in: → `{ok, sha, size, stored}`; bytes that hash to anything else are deleted and refused (400). Its `content-type` is what `__blob` serves it as, when that is passive media (Blobs, below) |
| `GET`, `HEAD /api/f/{name}/blobs/{sha256}` | viewer | → the bytes (ranges answer 206) |
| `GET /api/f/{name}/card` | viewer | → the fragment's preview card (Cards, below): a 1280×800 JPEG, `ETag` its blob's SHA-256, `Cache-Control: private, no-cache`, `X-Fragment-Ref` the live commit it shows; `If-None-Match` naming the tag is 304. Before the first is made, 404 `not_found` |
| `GET /api/f/{name}/events?since=` or `?tail=` | viewer | → `{events: [{id, at, kind, summary, data}]}`, oldest first: the page after `since`, or the newest `tail` (1-500; 400 otherwise, or with `since`) (500 a page; 10 000 kept, as for `ops`: `limits::AUDIT_KEPT`) |
| `POST /api/f/{name}/ops/{op}` | the operation's role | `{id, input}` → `{result, replayed}`; for a job, `result` is `{run, status}` (the same id answers the same run) |
| `GET /api/f/{name}/runs?status=&op=&limit=` | viewer | → `{runs: [{id, op, via, trigger, principal, status, attempt, depth, createdAt, finishedAt, error}], counts: {<status>: n}, paused}` newest first (30, at most 200) |
| `GET /api/f/{name}/runs/{id}` | viewer | → one run with its `input` and `output`. A channel's run (`via: channel`) holds the record that started it, so for a role that may not read that channel (`channel_read_role`), and for everyone once the channel is no longer declared, its `input` and `error` (a job's thrown message may quote its input) are absent and it says `withheld: true`, in `GET runs` too; its `run.held` event and an auto-pause's reason, which every viewer reads, then say its error is withheld. Its `output` is the app's own answer, a viewer's to see: an app that copies a record into it (or into a child run's input) shows it to them |
| `POST /api/f/{name}/replay` | editor | `{run}` → `{ok, run, attempt}`: a `held` or `blocked` run again, as its next attempt, with its input |
| `POST /api/f/{name}/pause` | editor | `{op, paused}` → `{ok, op, paused}`: the operation's triggers stop or start; calls are never paused |
| `GET /api/f/{name}/triggers` | viewer | → `{triggers: [{cron\|channel\|files, run, paused, nextAt?}], paused}` |
| `GET /api/f/{name}/database?table=&offset=` | the owner (never an agent: an agent's owner reads as a viewer, and an agent acts at most as an editor) | its app's database, as the platform code in its facet reads it (`__dump`, cell/platform.mjs), for an export (Export, below). With no `table`, `{schema: [{type, name, table, sql}], tables}`: each table's, index's, view's and trigger's statement in the order SQLite made them, a virtual table's shadow tables and the platform's operation ledger (`_fragment_ops`) left out, and the tables whose rows it holds. With one, `{table, offset, rows, next}`: at most 1000 of its rows from `offset`, each a JSON object of its columns (a blob `{"$base64"}`), about 4 MiB of their JSON at most (the first whatever its size), `next` the offset after them or `null` at the end; a table not in the schema is 404. Without app code, 404 `no_code`. It loads the app's code, as a query does (its dynamic worker, metered) |
| `POST /api/f/{name}/inbox` | the inbox token | token in `x-fragment-inbox-token` or `?t=`; a JSON body `{source?, payload}` (any other JSON, or text, is the payload), at most 64 KiB → `{ok, seq, runs}`. Bad token 403; 1000 pending, 429 |
| `POST /api/f/{name}/subscriptions` | a member who may read the channel | `{channel, url}` → `{id, channel, url}`: each new record of the channel is POSTed to `url` through the delivery queue as `{type: "record", fragment, channel, record}` (unsigned: the URL is the subscriber's capability; egress-checked; at most 32 a fragment; a 404 or 410 drops it; a removed member's go with it) |
| `GET /api/f/{name}/subscriptions` | a member (the owner sees all) | → `{subscriptions: [{id, principal, channel, url, createdAt}]}` |
| `DELETE /api/f/{name}/subscriptions/{id}` | its subscriber, or the owner | → `{ok, removed}` |
| `GET /api/f/{name}/channels` | viewer | → `{channels: [{name, read, post, signedIn, seq}]}`: `events`, `ops`, `inbox`, and the app's (`post`: who may post, or null; `signedIn`: only people signed in) |
| `GET /api/f/{name}/channels/{channel}?after=\|before=&limit=` | the channel's reader | → `{channel, records: [{channel, seq, at, principal, kind, body}], next}` (at most 1000 a page, always ascending by seq). `after` (default 0) reads forwards; `before` reads the nearest records with seq strictly less than its cursor. Pass `next` as the same cursor for the next page: the last seq forwards, the first seq backwards (the input cursor if empty). Supplying both cursors, a negative or malformed cursor, or a malformed limit is 400. |
| `POST /api/f/{name}/channels/{channel}` | the channel's `post` role | `{id, body}` → `{record, replayed}` (`Posted`): the platform appends `body` (any JSON, at most 64 KiB of it; 413) as a record of kind `message` naming the poster, with no app code; it reaches sockets, subscriptions, and the channel's triggers as a mutation's record does. The same id and body again answer that record and append nothing (a retry also finishes what the first try left: its deliveries, its triggers' runs); the same id with another body, or on another channel, is 409 (ids are the poster's, kept as long as the record). A channel without a `post` role, and `events`, `ops`, and `inbox`, refuse posts (403); one that says `signedIn` refuses an anonymous poster (401). A poster holding only `public` spends a public call (Serving, `__op`) |
| `PUT /api/f/{name}/channels/{channel}/draft` | the channel's `post` role | `{turn, text}` → `{ok}`: a record its poster is writing, its whole text so far (at most 64 KiB), shown to the sockets following the channel at once (a `draft` frame on `__live`) and never stored; `text: null` stops it. The record its poster then writes with the same `turn` replaces it on a page. At most 10 a second across the fragment (429 past that: the poster's next carries the whole text anyway); `turn` is `^[A-Za-z0-9._:-]{1,128}$` |

## Apps

Code comes from git: when `live` moves, the cell reads `fragment.json`
and `app.mjs` (at most 1 MiB, `limits::SOURCE_MAX_BYTES`; with
`applib/**.mjs|js`, at most 64 modules besides it and 4 MiB with it in
all) from the live commit. A live commit with an invalid `fragment.json`
keeps the last good code and says why in `status.code.error`.

A fragment on a blessed template (`agent`, `chat`, `brain`, `skills`:
`fragment_templates::blessed::BLESSED`; docs/cloudflare-v1.md, decision
40) names it in `fragment.json` (`template`) and runs the
platform release's manifest (with its own `meta` over the template's),
site, and code: the template's `app.mjs` and `applib/`, when it carries
any (a chat's push: docs/chat-records.md), held to an app's limits and
run in the same facet, under the release's identity (`blessed:<template>@
<release>`, a hash of the template's files,
`fragment_templates::blessed`; `status.code.id`), each file read from the
cell's Static Assets (Serving, The release's files). A platform deploy that
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
    "add":  { "kind": "mutation", "role": "editor", "description": "Add a todo with this text. Answers its {id}.",
              "input": { "type": "object", "required": ["text"],
              "properties": { "text": { "type": "string", "maxLength": 200 } }, "additionalProperties": false } },
    "list": { "kind": "query", "description": "Every todo, oldest first: {todos: [{id, text, done}]}." }
  },
  "channels": { "activity": { "read": "viewer" } },
  "meta": { "title": "Todo", "description": "…", "image": "https://…" }
}
```

- `role` defaults to `viewer` for a query and `editor` for a mutation.
  `constructor`, `fetch`, and `alarm` are not operation names (the App
  class's own; `fragment_proto::RESERVED_OP_NAMES`): a manifest naming one
  is refused at deploy.
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
- `description` says what the operation does and what it answers, for
  a person or an agent choosing it: plain text of 1 to 1024 characters
  (`limits::OP_DESCRIPTION_MAX_CHARS`, counted in characters; line breaks
  and tabs, and no other control character). One that is empty (or only
  whitespace), longer, holds a control character, or is not a string is
  refused at deploy, naming the operation (`status.code.error`; `fragment
  deploy` exits 1 with `invalid_request`). `status.code.operations` shows
  it as given; an operation without one shows none. It makes the
  operation a tool of `fragment mcp` (Operations as MCP tools, below).
  Describe every operation.
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
(body at most 64 KiB) once the mutation commits, and an exception rolls
back its writes and its records (422). A mutation's records, file writes
and pushes are its effects, at most 64 of them in all
(`limits::EFFECTS_MAX`). The ledger keys a
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
  256 KiB, each write one of the mutation's 64 effects; a path of at most
  300 bytes; never a blob pointer's text):
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
holding the bytes. A channel record that names one in its body's
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
  `link` fragment as anyone holding its share link sees it (its token
  set as the origin's HttpOnly `fragview` cookie over CDP before the page
  opens, at its URL without `?view=`, as a link holder's page goes on to;
  the card goes only to members, who already see that page), and a `members` fragment is not
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
  with its line and column when Chrome gives them) cut to 1 KiB, the
  share link's token replaced by `[share link]` before the cut wherever a
  page wrote it (`card::Heard::hiding`): every viewer reads the report. The try that
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
  new, so neither's deploys are shot (`card.skipped`); a ledger that does
  not answer refuses none.
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
subscriptions); a `who` that is an identity (an npub) is that identity's
own, and from anyone else is 403, so what is pushed to a person's
identity reaches their browsers alone (a chat's replies:
docs/chat-records.md, Push). `call.push(who, payload)` in a mutation
(sent once it commits) and `job.push(who, payload)` in a job (a step,
answering `{queued}`) push `payload` (`{title, body, tag, url, icon?}`,
at most 3800 bytes) to the subscriptions tagged `who` (at most 64
characters), or all of them with `*`, once per mutation or step. A
relative `url` is the fragment's own (`./` is its page): a click focuses
a page of it already there, or opens one. An `icon` (an `http` or
`https` address: an agent's picture, a chat's push) is the
notification's picture.

Every delivery is first written to the fragment's delivery outbox with
what caused it (a record's deliveries in the same turn as the record, a
push as it is accepted), then, once the write has answered, built whole
by the fragment (a push is encrypted for its browser, RFC 8291, and
signed with the fragment's VAPID key, RFC 8292) and put on the
`fragment-deliveries` queue; it leaves the outbox once the queue has it.
A computer's wake (docs/computers.md) goes from the outbox to the
computer itself, never through the queue, and leaves it once the
computer answers. One the queue does not take, or a wake that fails for
now, waits and is tried again from the fragment's alarm
(`delivery.deferred`), up to 20 times (then `delivery.failed`), so
delivery is at least once: a record subscriber dedupes by channel and
`seq`. From the queue: a 429, 5xx, or network failure is retried with a
growing wait; a push service's 404 or 410 drops the subscription
(`push.gone`); a delivery out of retries is reported
(`delivery.failed`), as is a wake its computer refuses for good (it
won't wake, or its owner cannot pay).

### AI

A job's AI steps bill the fragment's owner, on their ledger (Ledger,
below), capped when the run's principal is neither the owner nor an
agent of theirs (a run does not record whom an agent asked for). Text
goes through the platform's model route (Models, below) by tier; images
are FLUX.1 [schnell] (`@cf/black-forest-labs/flux-1-schnell`) on Workers
AI, on the model route's transport (the `AI` binding through the
deployment's AI Gateway), metered in neurons at Workers AI's price for
it: 4.80 a 512×512 tile and 9.60 a step (`fragment_core::media`);
decisions are the Clef family on that Workers AI transport, metered in
input tokens (Decisions, below). No app holds a key: the binding is
pre-authenticated, and a text step on an OpenAI-compatible API
(OpenRouter, or Finite Private) carries the deployment's key, which the
platform adds.

Each paid step reserves its worst case on the owner's ledger before its
call (text: its request's bytes as tokens in and its tier's capped
`max_tokens` out; an image: a 1024×1024 image's 4 tiles at its steps; a
decision: its model's context window),
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
  tier, usage, timing}`: `model` is a tier, `cheap` (the default) or
  `medium` (`high` is refused: Models); `max_tokens` is at most 16384;
  `reasoning_effort` is GLM's, `low` (the default) or `high` (anything
  else is `low`, since GLM takes an unknown one as `max`). Tools, drafts
  and timing are below (Text steps).
- `job.ai.decide({model?, input, questions, media?})` → `{model,
  answers, usage}`: a typed decision on the Clef family (Decisions,
  below). Its paid answer is kept as text's is; one that does not answer
  what was asked is kept as a rejection (`ai.decide-refused`), charged,
  and thrown as the step's error, and a retry or a replay reuses it. A
  429 or 5xx is retried on the same model: decisions have no fallback.
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

A text model's 429 or 5xx is answered by the fallback model (Models, below),
sent the same request (its tools too); when the fallback fails too, the
step is retried. Other refusals fail the step with the model's message.
A run answers `costMicros`: what its paid steps were charged (none when
nothing was).

#### Text steps: tools, drafts, timing

A text step is one OpenAI-shaped chat completion, streamed, read to its
`[DONE]` and no further (the gateway has held a stream open ~150 s past
it), so a job can run a small tool loop with no computer: call the
model, run its own code for the tool, call again.

- **Messages.** OpenAI's, as given: `system`, `user`, `assistant` (with
  its `tool_calls`, as a step's `message` answers them) and `tool`
  results (`{role: "tool", tool_call_id, content}`).
- **Tools.** `tools` are OpenAI's functions, `{type: "function",
  function: {name, description?, parameters?, strict?}}`: at most 64, in
  64 KiB of JSON (`TOOLS_MAX`, `TOOLS_MAX_BYTES`), each named 1 to 64
  letters, digits, `_` or `-`, once; `parameters` is a JSON Schema of an
  object (`{"type": "object", …}`); any other key is refused.
  `tool_choice` is `none`, `auto`, `required`, or `{type: "function",
  function: {name}}` naming one of the tools; with no tools it is
  refused. The same tools go to the fallback model.
- **Size.** A step's arguments are at most 512 KiB of JSON
  (`AI_TEXT_MAX_BYTES`, half a Workflow step's result, which carries
  them): a tool loop's messages grow, and past that the job's
  `job.ai.text` throws in its own realm, saying so.
- **Steps.** Each call is a step, and a run takes at most 256 steps
  (`limits::JOB_STEPS_MAX`) and 4 MiB of step results, its tools' own
  steps (`job.fetch`, `job.call`) among them: bound the loop in the job
  (`for (let i = 0; i < 6; i++)`); the platform bounds a run, not a loop.
- **The answer.** `text` is the model's words (`""` when it only called
  tools); `message` is `{role: "assistant", content, tool_calls?}`, each
  call `{id, type: "function", function: {name, arguments}}` (its
  arguments the model's JSON text, which may not parse), for the next
  call's messages; never its reasoning. `finish_reason` is the model's
  (`stop`, `length`, `tool_calls`), `model` the model that answered (its
  tier's, or the fallback), `usage` as the model reported it.
- **Timing.** `timing` is `{first_ms, ms, model, calls, tries}`: ms from
  the call to the answer's first data line, ms to its `[DONE]`, the model
  that answered, the calls this try made (2 when it fell back), and the
  Workflow's try of the step (1 unless it was retried; `null` when the
  Workflow did not say). It is kept with what the call bought: a step
  tried again after its call answered answers the same. The model
  route's `model.settled` line says the same of an agent's call
  (Models), so a latency lane reads both.
- **Drafts.** With `draft: {channel, turn}`, the call's text so far
  streams into that channel's drafts (`{type: "draft", channel,
  principal, turn, text, at, thinking?}` on `__live`, as `PUT
  …/channels/{channel}/draft` puts one), from the run's principal: a
  channel the app declares (no `post` role needed; never `events`, `ops`
  or `inbox`), a turn `^[A-Za-z0-9._:-]{1,128}$`. As the call begins,
  `{text: "", thinking: 0}`; while the model has no words yet, at most
  once a second, `thinking` the ms so far, so a page can show
  "Thinking… 4s"; then its text so far, at most 4 frames a second (none
  past 64 KiB), and its whole text once more at its end. The job writes
  the record with the same `turn` (`job.publish`, as the run's principal
  too), which replaces the draft on a page; a step that fails stops it
  (`text: null`). Drafts are never stored: a page that joins late sees
  the next one.
- **Prices.** Tool definitions, calls and results are tokens like any
  other, at the model's prices (docs/ledger.md); nothing else is added.

A job answering a chat with a tool, its reply streaming into the chat
(`fragment.json`: the channel, and a trigger on people's messages, so
its own replies start no run):

```json
{
  "operations": { "answer": { "kind": "job" } },
  "channels": { "chat": { "read": "viewer", "post": "viewer", "signedIn": true } },
  "triggers": [{ "channel": "chat", "run": "answer", "from": "person" }]
}
```

```js
import { DurableObject } from "cloudflare:workers";

const WEATHER = {
  type: "function",
  function: {
    name: "weather",
    description: "Today's weather in a city, in one line.",
    parameters: { type: "object", properties: { city: { type: "string" } }, required: ["city"] },
  },
};
const CALLS_MAX = 6; // each call is a step: a run takes at most 256

export class App extends DurableObject {
  async answer({ record }, job) {
    const turn = `reply:${record.seq}`; // the draft's, then the reply's
    const messages = [
      { role: "system", content: "Answer briefly. Use the weather tool for the weather." },
      { role: "user", content: String(record.body.text ?? "") },
    ];
    for (let i = 0; i < CALLS_MAX; i++) {
      const r = await job.ai.text({ messages, tools: [WEATHER], draft: { channel: "chat", turn } });
      messages.push(r.message);
      if (r.finish_reason !== "tool_calls") return job.publish("chat", { turn, text: r.text });
      for (const call of r.message.tool_calls) {
        let city = "";
        try { city = JSON.parse(call.function.arguments).city; } catch {}
        const res = await job.fetch(`https://wttr.in/${encodeURIComponent(city)}?format=3`);
        messages.push({ role: "tool", tool_call_id: call.id, content: res.text() });
      }
    }
    return job.publish("chat", { turn, text: "I stopped: too many tool calls." });
  }
}
```

and its page, which shows a reply as it streams and lets the record
replace it:

```js
fragment.subscribe("chat", (rec) => { drafts.delete(rec.body.turn); show(rec); }, {
  onDraft: (d) => {
    if (d.text === null) drafts.delete(d.turn);
    else drafts.set(d.turn, d.thinking !== undefined ? `Thinking… ${Math.round(d.thinking / 1000)}s` : d.text);
    render();
  },
});
```

### Decisions

A decision answers typed questions about text, JSON, images, audio or
video with probabilities, never prose to parse: which category, whether
something holds, how much. Apps ask it as a job's step
(`job.ai.decide`, AI, above), agents on the model route (`POST
/api/models/v1/decide`, Models, below), with one contract
(`fragment_core::decide`). It runs on Workers AI's Clef family, through
the `AI` binding and the deployment's AI Gateway, as images do. Its names are Fragment's own: the
platform maps them to Clef's System One wire (`state`, `noul`,
`criteria`), which apps never see.

**The request**, `{model?, input, questions, media?}`; any other key is
refused:

- `model`: one of

  | `model` | catalog id | context | reads | input price (list) |
  | --- | --- | --- | --- | --- |
  | `clef-flash` (the default) | `@cf/cloudflare/clef-flash` | 24,576 tokens | text, images | $0.038 per million tokens |
  | `clef` | `@cf/cloudflare/clef` | 65,536 tokens | text, images | $0.24 per million tokens |
  | `clef-omni` | `@cf/cloudflare/clef-omni` | 64,000 tokens | text, images, audio, video | $0.15 per million tokens |

  (Cloudflare's model pages and its pricing page, read 2026-10-14.) Any
  other name is refused.
- `input`: nonempty text, an object or an array (Clef truncates long
  text to fit its context).
- `questions`: 1 to 64 ids (1 to 100 ASCII letters, digits, `_`, `.` or
  `-`), each one of:
  - `{type: "choice", instructions, options}`: `options` maps 2 to 255
    ids (at most 100 bytes) to descriptions. Answers `{type: "choice",
    choice, probabilities, confidence}`: `choice` one of the ids,
    `probabilities` every id's.
  - `{type: "predicate", instructions}`: answers `{type: "predicate",
    probability}`, the probability that it holds.
  - `{type: "score", instructions, levels}`: `levels` 2 to 10
    descriptions, lowest first. Answers `{type: "score", score, levels,
    probabilities, confidence}`: `score` the probability-weighted
    zero-based level (it may land between two), `probabilities` keyed by
    index (`"0"`, `"1"`, …).

  Instructions and descriptions are nonempty strings. The input and the
  questions encode to at most 16 KiB on `clef-flash`, 48 KiB on `clef`
  and `clef-omni` (each under its context, with room for media).
- `media`: at most 4 items, at most 2 of them videos, each one of:
  - a blob of the asking fragment, `{sha256, type}` (Blobs, above): what
    a page's `fragment.blob(file)` answers and a chat's records name as
    attachments, whose `size` and `name` may ride along unread. A job's
    only: a blob the fragment does not hold is refused;
  - inline, `{data}`: a base64 `data:` URL. A job's inline media is at
    most 256 KiB of URL in all (its args travel with the run: larger
    media is a blob); an agent's is bounded by its route's 6 MiB body.

  Its `type` is one Clef takes: `image/png`, `image/jpeg`, `image/webp`
  (any model); `audio/wav` (or `audio/x-wav`, `audio/wave`) and
  `audio/mpeg` (or `audio/mp3`); `video/mp4` and `video/webm`
  (`clef-omni` alone). A browser's own voice memo (WebM, Ogg or MP4
  audio) is none of them. Each item is at most 4 MiB for an image (Clef's
  own), 8 MiB for audio (Clef's) and 8 MiB for a video (Clef's is 16:
  ours keeps every request under Clef's 13 MiB body), and all of it at
  most 8 MiB, decoded. No remote URL: Clef takes none.

Everything above is checked before anything is reserved or sent, and a
refusal says which bound it broke; a job's blobs are sized before any is
read.

**The answer**, `{model, answers, usage}`: `model` the catalog id that
answered, `answers` by question id, `usage` the model's own
(`{input_tokens, output_tokens}`). Every answer is checked against its
question before it leaves the platform: probabilities and confidence in
[0, 1], a complete distribution summing to 1 (within 0.001), a choice
that is a highest-probability option, a score that matches its
distribution (within 0.01). Confidence is the model's estimate, not a
guarantee: an app owns its review thresholds, and keeps the corrections
people make.

**The money.** A decision reserves its model's whole context window
(1,471 micro-dollars on `clef-flash`, 24,773 on `clef`, 15,120 on
`clef-omni`, at the default book's fee and margin): Clef scores every
question in one pass over one context, media and questions included,
and a request past it fails, so no call's input exceeds it. It settles
at `usage.input_tokens` (media is counted as input tokens) at its
model's input price; output is never charged. A call that reports no
usage is charged its reservation (`ai.cost-missing`); one that reports
more is charged what it reports (docs/ledger.md).

A job sorting a photo or a voice note someone uploaded (the page
uploads it with `fragment.blob(file)` and calls `sort({sha256, type})`):

```js
async sort({ sha256, type }, job) {
  const audio = type.startsWith("audio/");
  const out = await job.ai.decide({
    model: audio ? "clef-omni" : "clef-flash",
    input: audio ? "A voice note left for the household" : "A photo left for the household",
    media: [{ sha256, type }],
    questions: {
      kind: { type: "choice", instructions: "What is this about?", options: {
        groceries: "Food or supplies to buy", chores: "A job around the house", other: "Anything else" } },
      urgent: { type: "predicate", instructions: "Does it need doing today?" },
    },
  });
  const { kind, urgent } = out.answers;
  return { kind: kind.choice, urgent: urgent.probability > 0.7, sure: kind.confidence };
}
```

### Models (docs/cloudflare-v1.md, decisions 23 and 61)

One OpenAI-shaped chat completion on a tier's model, metered on its
payer's ledger (cell/src/models.rs). Tiers: `cheap` (the default for
agents and calls naming no tier) and `medium`. `high` (Opus) is refused,
400, saying why, until Cloudflare raises Unified Billing's Opus limit; a
model id is never a tier (though a call may name a tier's model by its
id: below). `vision` names the deployment's vision model,
for a runtime's calls about an image (Hermes' screenshots:
docs/computers.md, Models); it is metered as a tier's call, and is no
tier an agent or a job's step may name.

Where each runs is the deployment's (its config's `models`; decision 61):
a Workers AI model through the deployment's AI Gateway (Unified Billing,
its logs off, its metadata opaque ids: the first 16 hex of SHA-256 of the
payer's and the agent's identities), or a model on an OpenAI-compatible
API with the deployment's key for it and nothing else of the call's
but its chat completion fields (`CLIENT_KEYS`: the agent's `plugins`,
`provider` and the like are dropped) and the deployment's own fields for
that API (its slot's `body`: OpenRouter's provider routing). Unnamed,
`cheap` is GLM-5.3 Flash (`@cf/zai-org/glm-5.3-flash`), `medium` GLM-5.3
(`@cf/zai-org/glm-5.3`), `vision` GLM-5.3 Flash and the fallback DeepSeek
V4 Flash (`@cf/deepseek-ai/deepseek-v4-flash-0731`), all on Workers AI;
Finite's own runs `cheap` and `medium` on GLM-5.3 Flash through
OpenRouter (`z-ai/glm-5.3-flash`, zero data retention, the fastest
provider under a price ceiling; Paul, 2026-10-15), or by config on
Finite Private's (`glm-5-3-flash`), and `vision` and the fallback on
Workers AI's GLM-5.3 Flash. Each is a model the price book prices.

A tier's call whose model fails before answering anything (not reached,
429, or a 5xx; on an OpenAI-compatible API, where the key is the
platform's, also 401, 402, 403 or 404: its key refused, its credit spent,
or no endpoint left by its routing, none of them the caller's to fail
for) is made once more, the same, on the deployment's fallback model,
and its answer is the call's: settled from its usage at its prices,
under the call's one hold; a 401 or 402 is also logged aloud
(`model.platform-key`). An answer that began is final; a refusal of the
request (400, another 4xx) is the call's; when the fallback
fails too, its failure is the call's, released, after two tries in all.
`vision` and transcriptions have no fallback. A job's text step falls
back the same, and its output's `model` names the model that answered.

`fallback` names the deployment's fallback model itself: a runtime's own
switch when its model stalls, streaming nothing (our Hermes image's
`fallback_providers`: docs/computers.md, Models). It is metered as a
tier's call, has no fallback of its own, and is no tier an agent or a
job's step may name.

A call may name one of those four by its model's own id instead (Paul,
2026-10-16: "be able to run `/model` with hermes and see the real
model"; `fragment_core::models::Models::named`; docs/computers.md, "A
model by its id"): the slot of the caller's tier when that slot runs it
(`x-fragment-tier: cheap` or `medium`, optional; any other value is
refused, 400), else the first slot that does, in the order `cheap`,
`medium`, `fallback`, `vision`. The call is that slot's, as if it named
the slot: its model, its key, its `body`, its fallback and its price.
An id no slot runs is refused, 400, nothing reserved or sent, the message
listing the ids a call may name, which `GET /api/models/v1/models` lists
too: OpenAI's list (`{object: "list", data: [{id, object: "model",
created: 0, owned_by: "fragment"}]}`), each id once, nothing of where each
runs, and none of the route's names, which are taken all the same. An id
is one model: a deployment whose two slots name one id at two places is
refused. A job's step names a tier, never an id.

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/models/v1/models` | an agent | → the ids its calls may name, OpenAI's list (above) |
| `POST /api/models/v1/chat/completions[?fragment=<name>]` | an agent (`for` names whom it acts for) | an OpenAI chat completion, `model` a tier, `vision`, `fallback`, or one of their models' ids (`x-fragment-tier` its tier, optional) → the model's answer in OpenAI's shape: JSON, or with `stream: true` server-sent events, usage once on a last chunk with no choices |
| `POST /api/models/v1/audio/transcriptions` | an agent | an OpenAI transcription, `multipart/form-data`: `file` (the audio, at most 10 MiB), `model` `whisper`, and optional `language` (ISO-639-1), `prompt` and `response_format` (`json`, the default, or `text`) → `{"text": …}`, or the text alone |
| `POST /api/models/v1/decide[?fragment=<name>]` | an agent (`for` names whom it acts for) | a decision, `{model?, input, questions, media?}`, its media inline → `{model, answers, usage}` (Decisions, above) |

Transcription (decision 9, as Paul changed it on 2026-10-07: a voice
memo is one the agent transcribes itself, through this route):
- **The model.** `whisper` is Workers AI's Whisper
  (`@cf/openai/whisper-large-v3-turbo`), called as a tier's model is.
- **What it is sent.** The audio as base64. A `language` or `prompt` it
  was given passes on; with no language, Whisper detects it. OpenAI's
  other fields are let go.
- **Refusals before anything is reserved.** A form that is not one, a
  model other than `whisper`, and a format other than `json` or `text`
  are refused 400. Audio past 10 MiB is refused 413.
- **Metering** (`fragment_core::transcribe`). Workers AI's 46.63 neurons
  a minute of audio. The call reserves its audio's bytes read as 16
  kbps, and settles at the length Whisper reports. Above the
  reservation it is charged in full; with no length reported, at the
  reservation.
- **Who pays.** The payer is the agent's owner, under no fragment's cap.
- **What is kept.** Only the usage, on the ledger. Whisper's refusal is
  released and passed through, as a chat call's is.
- **Its answer's shape.** An answer carries `x-fragment-answer-shape`:
  the model's own answer's top-level keys, and its `usage` when that is
  a small object of numbers and short strings (`{"keys": […], "usage":
  …}`), never its words. The e2e's `transcribe` section prints it, so a
  hosted run shows what Workers AI answers.
- **What Workers AI answers.** Read on the e2e preview, 2026-10-08: the
  catalog's schema unwrapped (`text`, `transcription_info`, `segments`,
  `vtt`, `word_count`), plus a `usage` the schema does not name. The
  route reads `text` and `transcription_info.duration` (the catalog's own
  field). An answer in any other shape is answered 502 and settled at its
  reservation.

What the model is sent is the body bounded: no `model` (the tier's),
its images as they came, `max_tokens` at most 16384, `reasoning_effort`
clamped (above), usage asked for when it streams, and none of the
client's headers. The payer is the agent's owner (decision 36); `fragment`, one the agent is a
member of (403 otherwise), is where the turn is: when its owner pays,
the call counts in its month and is under its cap for anyone but the
owner (or the owner's agent acting for them); when another person owns
it, that owner's ledger is asked first whether it is still open
(decision 26). A request is at most 6 MiB (413 past it, nothing
reserved): a screenshot Hermes shrinks to 5 MiB of base64 after a 413
fits. Each call reserves its worst case (the body's bytes as
tokens in, `max_tokens` out) under a reference of its own (`aig:<hex>`),
then settles from its last, cumulative usage, input less what was cached
(Workers AI puts a per-chunk delta on every chunk and the whole call's
on a line of its own at the end: only that is metered). A call the model
refuses is released and its answer passed through as it came; one whose
stream broke before its usage is charged its worst case; a streamed
call settles at its `[DONE]`, whether or not the client read to its end,
and not at the stream's end: the gateway has held a stream open ~150 s
past its `[DONE]`, which left the settle late, or cut with the Worker's
`waitUntil` (the ledger's sweep then charged its worst case). The client
reads the stream as it came (OpenAI's clients stop at `[DONE]`). Nothing
of the request or its answer is kept: only the usage, on the ledger. A
refusal of the payer's ledger is its own (402 `budget_used_up`, 403 for
a guest), with its message.

Each call's settle logs one line, `{"event": "model.settled", ref,
logId, tier, model, charge, basis, first_ms, ms, calls}` (`wrangler
tail`): `first_ms` is ms from its first try to the answer's first data
line (streamed) or its status (read whole), `ms` to its settle, `calls`
its tries (2 when it fell back), `model` the one that answered. A later
latency lane reads these, as it reads a job's text step's `timing`
(AI, above).

A decision on the route (`models::decide_route`) is a job's
`ai.decide` with its media inline (Decisions, above): a blob is refused,
since the route reads no fragment's blob store (an agent reads one with
`GET /api/f/{name}/blobs/{sha256}` and sends its bytes). Its caller,
payer, `fragment`, 6 MiB body and ledger refusals are the chat route's.
A body that is no decision (not one, a key unknown, a bound past) is
400 saying why, nothing reserved or sent. It reserves its model's
context window under an `aig:<hex>` reference, as an agent's turn, and
settles at the input tokens the model counted. The model's refusal is
released and passed through as it came; an answer that does not answer
what was asked is settled (it was paid for) and answered 502
`upstream_failed`. A computer's guest asks it as `POST /v1/decide` on
its model intercept (docs/computers.md, Models).

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
- **Guests** make no fragments (`POST /api/fragments` is 403): a guest
  pays for nothing. They edit the fragments shared with them, whose
  owners pay (Paul, 2026-10-03). A fragment asks its
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
| `POST /api/ledger/{person}/plan` | the same | `SetPlan {id, plan}` → `{}`: an override; a seat's holder's plan is the registry's to push (Seats and orgs), and its next push wins (the debt ledger) |
| `POST /api/ledger/{person}/overdraft` | the same | `SetOverdraft {id, micros}` → `{}`: at most $1,000; read-only is decided afresh |

`{person}` is an email, an identity (an npub), or `me`. Commands are
idempotent by their `id`, kept on the ledger as `<kind>:<id>` (a grant's
`g1` is not a plan's): the same id again changes nothing, the same id
with another body is 409 `conflicting_body`, and a body that breaks a
rule (an amount past its limit, a field misspelt) is 400 and remembered
by nothing, so it may be sent again. A seat's state has no command
here: its one writer is the registry, and an operator comps or ends a
seat there (`/api/admin/seats`, Seats and orgs); `seat` is 404, saying
so (it went on 2026-10-16). Test fleets add `POST /api/test/ledger
{identity, op: clock {offsetMs} | sweep | entries {prefix} | totals |
paid-calls {max} | legacy-seat {seat, seq}}`.

### Seats and orgs (docs/billing.md)

A seat is what lets a person create (decisions 49 and 51):
`seat_always_on` ($200, a computer that stays awake unless its holder
lets it sleep), the one seat sold, or `seat` (a computer that sleeps), an
operator's comp only since 2026-10-15 (decision 25's status). A body
that names a kind to buy (Checkout's, an org admin's seat, a trial code)
takes `seat_always_on`, its default; `seat` is 400 (not sold). Every seat is in an org, the one
who pays; a person is in at most one org and holds at most one seat. An
org's members (`OrgMember`) are its admins, its seats' holders, or both,
each a person (`person`, an npub) or an email a seat waits on until
someone signs in with it verified, who then takes it (the oldest offer
first, if they are in no org yet). A seat is **comped** (an operator's,
outside Stripe; always good) or **paid** (counted on the org's Stripe
subscription, good while it is `trialing`, `active` or `past_due`: not
built yet).

The registry is the one writer of a seat holder's plan and seat. Each
change to a seat pushes, within seconds (the registry's alarm, tried
again until it lands), to the holder's ledger (`SetPlan` its kind, and
`SetSeat active`, or `canceled` once it lapses or ends: agents stop, the
plan is kept) and to their computer (`alwaysOn` in its view while a good
`seat_always_on` seat's holder has not let it sleep). One row a person
is queued, and the alarm pushes them one at a time, each read fresh as
it goes; the ledger keeps the seat in the registry's order (`SetSeat`'s
`seq`), refusing an older one as stale, naming its own, past which the
registry orders its next (a ledger an operator's `seat` command left
ahead takes its pushes all the same). A computer made later asks for a
push as it is made (`POST /api/computers`: a making that cannot ask
fails, and made again, asks again).

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/seat` | a person | → `SeatStatus {seat: SeatView? {id, org: {id, name}, kind, comped, good, sleeps, admin}, org: {id, name}?, admin, offered: [{id, org, kind}], guest, offer: {kind, seatMicros, includedMicros, packMicros}}`: their seat, the org they are in, and seats offered to an email of theirs while they are in another org (`MySeat`); `guest`, whether they are a guest (decision 49: they make nothing, and are offered a seat): they hold no seat, and their ledger gives them no plan of its own, its plan `guest` (a new person's on a deployment whose plan is guest, or an operator's) or a seat's plan kept after the seat ended (`canceled`; `fragment_core::org::guest`); and `offer`, what is sold, in micro-dollars: the one seat (`seat_always_on`), its monthly price and the credit it includes each month, and a credit pack's price, core's constants (`fragment_core::stripe::offer`), which every page shows them from |
| `PUT /api/seat` | a seat's holder | `{sleeps}` → `MySeat`: whether their `seat_always_on` computer may sleep; no seat is 400 |
| `GET /api/org` | an org's admin | → `OrgView {id, name, createdAt, members: [OrgMember {id, person?, email, admin, seat?, comped, addedAt}]}` (a held seat shows its holder's latest sign-in's email); anyone else is 403 |
| `POST /api/org/seats` | an org's admin, while the org pays (else 400: buy one first) | `{email, kind?}` → `{seat: OrgMember, created, mailed}`: a paid always-on seat (`kind` only `seat_always_on`), held at once by the email's person (409 when they are in another org, or hold a seat here), else waiting on the email, which is mailed. It bills from now: the org's count is pushed to Stripe within seconds, prorated. The same again is answered again (`created: false`). A paid seat's kind does not change: one is sold |
| `DELETE /api/org/seats/{id}` | an org's admin | → `OrgMember` as it was: the seat goes (an admin's row stays, seatless), its holder's seat canceled, the count pushed. The org's last paid seat is 400: its subscription is canceled in the portal |
| `POST /api/org/admins` | the same | `{email}` → `{admin: OrgMember, created}`: an admin too (a seat they hold stays), or waiting on the email; at most 10 |
| `DELETE /api/org/admins/{id}` | the same | → `OrgMember` as it was: no longer an admin (a row with a seat keeps it; one without goes). The last admin is 400 |
| `POST /api/admin/seats` | the deployment's operators | `CompSeat {email, kind, org?}` → `Comped {seat: OrgMember, org, created, mailed}`: in `org` when named (404 when none), else in the org the email's person is in, else in a new org of one named by the email, whose seat's holder is its admin. A person who holds a seat already is 409, unless it is this same comp, which answers it again (`created: false`); a person in another org than the one named is 409. A new comp mails the email (`mailed`; a mail that fails leaves the seat made) |
| `PATCH /api/admin/seats/{id}` | the same | `{kind}` → `OrgMember`: a comped seat's new kind (a paid one is 400) |
| `DELETE /api/admin/seats/{id}` | the same | → `OrgMember` as it was: the comp ends; an admin keeps their place, seatless; anyone else's row goes. None is 404 |
| `GET /api/admin/orgs/{id}` | the same | → `OrgView` |
| `POST /api/admin/trials` | the same | `NewTrialCode {name, kind?, days (1-30), capacity (1-10,000), expiresAt?, code?}` → `TrialCode {id, code, name, kind, days, capacity, expiresAt, active, revision, createdAt, createdBy, subscribed, open, uses}`: `kind` only `seat_always_on`, the default (`seat` is 400); `code` none, one is made (16 of A-Z less I and O, and 2-9, in fours); a code another has is 409 |
| `GET /api/admin/trials` | the same | → `{codes: [TrialCode]}`, newest first, their uses counted (`subscribed`: bought; `open`: Checkouts not yet expired) |
| `GET /api/admin/trials/{id}` | the same | → `TrialCode` with its `uses: [{person, email, at, status}]` |
| `PATCH /api/admin/trials/{id}` | the same | `{revision, capacity?, active?, code?, expiresAt?}` → `TrialCode`: when `revision` is still its (else 409); places only grow |

`/api/admin/*` takes an operator's signature, or the shell's session of
a person the deployment's `operators` names (decision 59). A wipe takes
the person's rows, and an org it leaves empty.

### Billing (docs/billing.md, "Stripe" and "Trials")

On a deployment that sells seats (its config's `stripe`; else these are
400, and seats are comped only), a person buys their own through Stripe
Checkout: a monthly subscription to one always-on seat, $200, at its
price's lookup key (`fragment_seat_always_on_month`; the only seat sold,
Paul, 2026-10-15), a card always, Stripe's automatic tax unless the
deployment turns it off, and promotion codes allowed. Their org is made,
of one, if they are in none; its admin may buy again once its
subscription has ended (their lapsed seat becomes the always-on one). A
subscription with any other item is not fragment's. An org has one Stripe customer and one
subscription; everything fragment makes carries `fragment_deployment`
(the platform's origin) in its metadata, and nothing without this
deployment's is touched (the account is finite-mono's too).

An org's paid seats are the registry's, and the subscription's one
item's quantity a copy of their count, held and pending: each change
queues the org, and the registry's alarm pushes the count read fresh
(`POST /v1/subscriptions/{id}`, `create_prorations`), so two changes at
once never leave Stripe a seat short; a subscription fetched with
another count (changed in Stripe's dashboard) is pushed back to the
seats'.

The registry writes an org's subscription only as fetched from Stripe:
by the Checkout's return, by the webhook, and daily by the registry's
alarm for each org whose copy is a day old (a webhook lost). A newer
subscription replaces the org's only once it has ended; an event older
than the last applied changes nothing. `trialing`, `active` and
`past_due` are good; anything else lapses the org's paid seats (Seats
and orgs).

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/billing/checkout` | a person (409 holding a good seat, or in an org whose admins give seats) | `{kind?, trialCode?}` → `{url, session}`: Stripe's Checkout of one always-on seat, for 31 minutes. `kind` is `seat_always_on`, the default; `seat` is 400 (not sold). With a trial code (as typed: any case, spaces and dashes aside), Stripe's trial of its days (30 at most), a card taken first, and no promotion code on top. A code that is unknown is 404; ended (off, past its expiry, or of the `seat` no longer sold), 400; full, or redeemed by someone who had a trial or whose org has paid, 409. Nothing is made for a refused one |
| `POST /api/billing/sessions/{id}` | its buyer (anyone else, or a session Stripe never made: 404) | → `MySeat`: the Checkout's return (a seat's `/welcome?checkout={id}`, a pack's `/settings?pack={id}`; Back from Checkout, `?checkout=canceled` or `?pack=canceled`): a completed one's seat is theirs at once, whatever the webhook's timing; again, it changes nothing |
| `POST /api/billing/packs` | a seat's holder; an org's admin for a member who holds a seat (`member`: their row's id; 403 otherwise; 400 holding no seat) | `{member?}` → `{url, session}`: a Checkout for a $25 credit pack (decision 55; the one-time price `fragment_credit_pack`). Paid, its webhook or its return grants $25 of purchased credit on the member's ledger, kept until spent, once by its Checkout (`GrantCredit {id: pack:<session>, by: stripe}`). Its return is its buyer's |
| `POST /api/billing/portal` | an org's admin (403 otherwise; 400 an org that never paid) | → `{url}`: Stripe's portal on the deployment's own configuration (`stripe.portal`): card, invoices, billing address, cancel at period end |
| `POST /api/stripe/webhook` | Stripe, signed (`Stripe-Signature`, this endpoint's secret, within 300 s; else 401) | an event → `{received, applied}`: `checkout.session.completed` and `customer.subscription.created`, `.updated`, `.deleted` are fetched again and written; any other event, or one not this deployment's, is answered and dropped. A failure is 5xx, and Stripe sends it again |

### Jobs and triggers

A job is a method called `(input, job)` that runs as a Cloudflare Workflow,
outside any request. Each `await` on a `job.*` step is durable. The
steps are the eight below, the files steps (`job.files.*`), `job.push`,
and the AI steps (`job.ai.*`), all above:

- `job.call(op, input)`: an operation of this fragment as the run's
  principal, with `job:<run>:<step>` as its id (a retried or replayed
  step is a replay). Calling a job starts it: `{run, status}`, one hop
  deeper.
- `job.fetch(url, {method, headers, body})` → `{status, ok, headers,
  text(), json()}`: the app's one way out. `{{NAME}}` in a header value
  is the fragment's secret `NAME`, added by the platform at the egress
  point; the app never holds it. It is filled only toward one of the
  hosts the secret names, over https (a dev fleet's local fakes over
  http too); a fetch elsewhere fails its step, naming the secret and
  the host. http(s) only; loopback, private, and
  `.internal` addresses are refused (and a Worker's fetch reaches only
  the public internet: a name with no public address fails the step); redirects are answered, not
  followed; a body of at most 256 KiB, a response of at most 1 MiB, 120
  seconds. Every fetch carries `x-fragment-hops`; one that is not a GET
  or a HEAD carries `Idempotency-Key: <fragment>-<life>-r<run>-s<step>`
  (the same on every try and replay of the step, never another
  fragment's), unless the job set its own.
- `job.publish(channel, body, kind)`: a record, once per step.
- `job.sleep(ms | "N seconds|minutes|hours|days")`, up to 30 days.
- What the fragment's own page reads, read for its code:
  `job.members()` → its members as `__members` lists them (`[{principal,
  role, kind, addedAt, …}]`, the first added first); `job.people(ids)` →
  names for at most 64 identities as `__people` answers them (`{[id]:
  {kind, email?, name?, fragment?, title?, picture?}}`: a member's email,
  as the fragment's members are shown it; an agent made from an agent
  fragment by its label and its fragment's title, as it shows (Identities,
  "An agent's name and picture"); `picture` an absolute address on the
  platform); `job.presence()` → who is here now, as the pages' presence
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
<op>`, `fragment inbox <name> --payload JSON` (the token from
`FRAGMENT_INBOX_TOKEN`, else stdin, sent in `x-fragment-inbox-token`).

## Serving

`<name>.<suffix>/<path>` (`/f/<name>/<path>` on the platform's host
redirects there, a `GET` or `HEAD` only, except `__watch` and `__live`). Every path
on a fragment's host is the fragment's, `/api/…` included (the platform
API answers on the platform's host):

| path | |
| --- | --- |
| `/`, `/<page>` | `site/` in the live commit (`index.html` for directories); Open Graph tags from `fragment.json`'s `meta` |
| `__file?path=` | a content file from live, else main |
| `__preview.svg` | the placeholder preview image |
| `__blob/{sha256}` | one of this fragment's blobs (Blobs, above), `GET` or `HEAD`, viewers and up (on a `public` fragment too: whoever holds only `public` is refused): its bytes as the type its upload declared (ranges answer 206), `Cache-Control: private, max-age=31536000, immutable`, `X-Content-Type-Options: nosniff`, and an `ETag` of the hash; another fragment's hash is 404. `PUT` uploads one from the fragment's own page, as `PUT /api/f/{name}/blobs/{sha256}` does (editors; the body streamed and hashed on the way, bytes that are not what the hash says 400; a declared `content-length`, at most 256 MiB, else 400 or 413) → `{ok, sha, size, stored}` (`stored: false`: it was there); its `content-type` is the type it is served as. As for any write on this host, only the page's own cookies count (`fetched`): another fragment's page uploads as no one (401) |
| `__members` | viewers and up (the share link too): `{members: [{principal, role, addedBy, addedAt, kind, owner?}]}`, the first added first, as `GET /api/f/{name}/members` answers it (a chat's agents, its lead the first agent added) |
| `GET __channel/{channel}?after=\|before=&limit=` | the channel's readers: the same page and exclusive cursors as `GET /api/f/{name}/channels/{channel}`, using this origin's session or share link |
| `POST __op/{op}` | a browser's call: `application/json` `{id, input}`; a signed-in browser (`fragment_site`) calls as its person; an unsigned caller gets an anonymous principal cookie; callers holding only `public` get 60 calls a minute each, 600 per fragment (a page's live views re-run over `__live`, outside this) |
| `POST __op/channels/{channel}` | a browser's post (`fragment.post`), through the call's door and its checks: `{id, input}` with the record's body as `input` → `{result: record, replayed}`, as `POST /api/f/{name}/channels/{channel}` answers it; a post spends the public budget as a call does (no operation name holds a `/`) |
| `__signin`, `__signout` | this origin's session (Sign-in, above) |
| `__fragment.js` | the browser library (below) |
| `__fragment.css` | the platform's stylesheet (below), for a page that links it |
| `__people?id=…&id=…` | anyone who can see the fragment: `{profiles: {<id>: {kind, email?, picture, name?, fragment?, title?}}}` for up to 64 identities (a person's `email` only for a member, and only when the asker is a member too: decision 48; a picture is an absolute platform URL; an agent made from an agent fragment, a computer's, has that `fragment`, its label as its `name`, which `@mentions` it, and its fragment's `title` and `picture`, which it shows by: Identities, "An agent's name and picture"); an id the registry does not hold is left out |
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
- server → client: `{type: "hello", id, principal, role, platform,
  presence: [{id, principal, data}]}` (`platform` the platform's origin,
  `FRAGMENT_PLATFORM_URL`: the one page that frames a fragment with its
  person's session, whose messages a framed page may take; everyone
  sharing presence as it opens),
  `{type: "record", channel, seq, at, principal, kind, body}`,
  `{type: "subscribed", channel, next, more}` (after each page),
  `{type: "draft", channel, principal, turn, text, at, thinking?}` (a
  record its poster is writing, to the sockets following its channel;
  `text: null` once they stopped; never stored, so a page that joins
  late sees the next; `thinking`, with `text: ""`, the ms a job's text
  step's model has had no words yet: AI, Text steps),
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
channel, onRecord, {after?, last?, onDraft?, onReady?})` (pages through the backlog, then
follows live; after a reconnect it resumes after the last record;
`onDraft` hears the channel's drafts while it is live; `onReady({next})`
hears when the backlog is caught up, including after reconnect),
`read(channel, {after?, before?, limit?})` (one page through `__channel`,
independent of a live subscription's cursor; `before` reads backwards,
with records in ascending order and `next` the cursor for an older page),
`presence.set(data)` (changes within 150 ms go as one, the latest),
`presence.on(fn)` (called with everyone here now, and on each change),
`blob(file, {name?, type?})` (a File or Blob uploaded as one of the
fragment's blobs, its SHA-256 computed in the page, as an editor; answers
`{sha256, size, type, name}`, a chat attachment's shape, read at
`./__blob/<sha256>`), `observePosts(fn)` (this page's post attempts:
`{channel, id, body, status: "posting" | "posted" | "failed", record?, error?}`;
a page may render an error on its message and retry the same id and body),
`me()` (`{id, principal, role, platform}`, the hello's), `closed(fn)`. The page's
socket reconnects by itself after a jittered wait (half to one and a
half times a backoff that doubles from 1 to 30 seconds), except after a
close the fragment means for good: 4003 (the page's access was revoked)
or 4004 (the fragment was deleted) ends it, and `closed` handlers get
`{code, reason}`.

The stylesheet (`<link rel="stylesheet" href="./__fragment.css">`,
`cell/fragment.css`) is for a page that links it: the platform adds it
to no page. It follows the shell's look: Funnel Sans (with system-font
fallbacks), warm greys, one blue, and solid pill buttons. Its theme
variables use `light-dark()`, light or dark by the system's
`prefers-color-scheme`; a page may choose with `color-scheme` on `:root`
or `data-theme="light"` / `"dark"` on `<html>`. The variables are
`--bg`, `--fg`, `--muted`, `--faint`, `--line`, `--line-strong`,
`--surface`, `--soft`, `--accent`, `--on-accent`, `--success`, `--danger`;
`--font`, `--font-display`, `--font-mono`; `--text-sm`, `--text-lg`,
`--text-xl`; `--radius`. Their names hold across releases; their values
and the base styles may change with one. Element base styles use
`:where()` (zero specificity), so a page's rules override them: the
body, `main` (content starts at the top), modest headings, `small`,
links, `code`, `pre`, media, form controls, buttons and the focus ring.
`[hidden]` stays hidden. Fonts are requested from Google Fonts, as in
the shell; a page may override `--font` and `--font-mono`.

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

### The release's files

The files the platform serves as they are in its release, a blessed
template's site, data and code (Apps, above), a catalog template's
files, the shell's files (The shell, below) and the agent docs (Agent
docs, below), are the Worker's Static Assets (`ASSETS`), each named by
its SHA-256. The cell carries only their index, each file's path,
hash and size and each template's release, made by its build
(crates/templates), and reads a file by its hash when it answers with
it (cell/src/assets.rs): a 304 or a `HEAD` reads none. Every request
runs the cell first (`run_worker_first`), so none is served at a path
of its own: only the routes above serve them, with the headers they
always had.

## Agent docs

For an agent with no CLI yet (llmstxt.org), the platform's own host
answers `GET /llms.txt` with `cli/SKILL.md`, the text `fragment skill`
prints (the handoff to a coding agent building for a person: what a
fragment is, the install, `fragment login --host`, building, deploying
and sharing, design, the primitives, its person's apps as tools through
`fragment mcp`), and `GET /llms-full.txt` with `cli/GUIDE.md`, the text
`fragment guide` prints (the whole manual, with Who can do what, where,
below). Both are the CLI's own files, which the release carries in the
cell's Static Assets (Serving, The release's files), so they never drift
from it. Neither names a host: each has two blanks, `{{platform}}` and
`{{fragment}}`, that whoever shows it fills (`fragment_templates::
agent_doc`). The cell fills them with its own deployment's platform
(`FRAGMENT_PLATFORM_URL`) and a fragment's origin there, `<name>` for its
name (`https://<name>.<suffix>`, a branch's mark before the dot), so
production's say `https://finite.chat` and a preview's its own; the CLI
fills them with the host it calls, and a fragment's as
`https://<name>.<the platform's domain for fragments>`. They answer
anyone, as `text/plain; charset=utf-8` with `Cache-Control: no-cache`
and an ETag of the file's hash and what filled it (304 when
`If-None-Match` names it, before the file is read). On a fragment's host
the same paths are the fragment's own.

The shell's settings hand them on: "Build with Claude Code or Codex" is
a prompt to copy, `Read <the platform>/llms.txt and help me build: `,
which the person finishes and pastes into their coding agent, which
installs the CLI and asks them to approve it. The install line itself is
folded below it ("Install it yourself").

### Operations as MCP tools (`fragment mcp`)

`fragment mcp <fragment> [--write]` (cli/src/mcp.rs; cli/GUIDE.md,
"Your operations as tools") is an MCP server over stdio that the CLI
runs, for an MCP client on the same machine: JSON-RPC 2.0, one message a
line, `initialize`, `ping`, `tools/list` and `tools/call` (revisions
2025-11-25, 2025-06-18, 2025-03-26 and 2024-11-05 answered in kind).
The platform serves nothing new for it: it reads `GET status` (at
`initialize` and at each `tools/list`) and calls `POST
ops/{op}`, signed as every CLI request is (this machine's key, or a
computer's agent: `FRAGMENT_AS_AGENT`).

- A tool is an operation of `status.code.operations` with a
  `description`, whose `role` the caller's role there reaches, and
  whose `input` is an object's schema (none: any object): a query
  always, and a mutation or a job only with `--write` (they change
  state). Its `name` and `description` are the operation's, its
  `inputSchema` its `input` (with `"type": "object"` added when it names
  no type), and its annotations say a query only reads.
- A call's `arguments` are the operation's input as they are, sent as
  `{id, input}` with a fresh id (`mcp-` and 16 hex digits); the CLI's
  retries of the one request reuse it, so each call runs once. A result
  answers as its JSON text, and as `structuredContent` when it is an
  object.
- A call to an operation that is no tool (no description, a write
  without `--write`, a role beyond the caller's, an input that is no
  object, no such operation) is JSON-RPC's `-32602`, saying which, and
  nothing is sent. A call the platform refuses is the tool's error
  (`isError: true`) carrying its `ErrorBody`: `structuredContent: {error:
  {code, status, message}, id}`; one with no answer carries the CLI's
  `unavailable` or `outcome_unknown` (no `status`), and its id replays
  it (`fragment call … --id`).
- One server serves one fragment.

## Connections (decisions 22 and 37)

Every provider of the deployment's catalog (`FRAGMENT_PROVIDERS`): a
person's accounts (connections, held and refreshed by WorkOS Pipes), the
operator's keys, and the person's own keys. Their agents use them through
the computer's swap, each with a placeholder of its own
(docs/computers.md, Connections and operator keys).

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/connections` | a person | → `{providers: [{provider, kind, state, hosts, env, price?, signIn?, models?}]}` (proto's `Providers`): each provider the deployment offers, in its catalog's order. `kind` is `connection`, `operator` or `own`; `state` a connection's `connected`, `needs_reauthorization` or `not_connected` (Pipes' connected account, read with no token minted), an operator key's `offered`, an own key's `set` or `not_set`; `price` an operator key's list price, `{micros, per}` calls; `signIn` an own key connected through its provider's sign-in, `{manage}` (the provider's page where the person revokes it); `models` the models an own key's provider offers for their agents, `[{id, name}]` (an agent's `agent.json` `model: {provider, id}`; docs/computers.md, "An agent's own model"). Reading it tells the person's computer the connections' states, so its guest is given a new connection at its next read |
| `POST /api/connections/{provider}/authorize` | a person | → `{provider, url}`: for a connection, Pipes' consent, for the person's browser; followed, the account is connected. For an own key with a sign-in (decision 60, `fragment_core::own_signin`), the provider's own page: their computer (made first: 404 without one) begins a sign-in, keeps its nonce and its verifier sealed for 10 minutes (at most 8 under way: 429 past that), and `url` is the row's `authorize` with `callback_url` (`GET …/callback` below), the verifier's S256 `code_challenge`, `code_challenge_method=S256`, `state` (`<computer's 24 hex>.<nonce>`) and `key_label=Fragment`. A provider not offered is 404; an operator key, or an own key with no sign-in, 400 |
| `GET /api/connections/{provider}/callback?code=&state=` | the person's browser, unsigned, on the platform's host | → an HTML page: back from an own key's provider. The state names the computer and the sign-in it began, once: the code is exchanged with that sign-in's verifier at the row's `exchange` (`{code, code_verifier, code_challenge_method: "S256"}` → `{key}`), and the key is sealed by that computer as `PUT …/key` seals one; the page (200) says it is connected, and holds no key. A state replayed, expired, made for another provider, or never made; no code (the person declined); or a code the provider refuses: a page (400) that says to connect again, nothing set. Another host is 404 |
| `PUT /api/connections/{provider}/key` | a person | `{key}` → `{provider, state: "set"}`: the person's own key for an `own` provider, 1 to 4096 printable characters with no space, kept sealed by their computer (made first: 404 without one) and swapped in for their agents' placeholders. Any other provider is 400 |
| `DELETE /api/connections/{provider}/key` | a person | → `{provider, state: "not_set"}`: the key is gone; their agents' guests are given it no more |

## The shell (phase 5)

The platform's one page is `/`, `/settings` and `/welcome` (cell/shell/,
its files at `/__shell/<file>`, the release's: Serving, The release's
files): its script reads the path, opening its
settings at `/settings`, the welcome at `/welcome`, and the person's
chats at `/`, and puts the view it shows in the address, so a reload
stays put. A new person's first run never goes through settings
(docs/billing.md, "The shell"): a guest with nothing shared with them
lands on the welcome, the seat and a trial code
(cell/shell/billing.js `seatOffer`), which a mailed trial's
`/welcome?trial=<code>` fills in, then Stripe's Checkout, whose return
(`/welcome?checkout=<id>`, or `canceled`) comes back to it: applied
(`POST /api/billing/sessions/<id>`), the seat's plan waited for, then
their first agent made; a seat (bought, an org's, or the deployment's
plan) with no chat makes it at once at `/`. Whether someone is a guest
is `GET /api/seat`'s `guest` alone, and every price the shell shows is
its `offer` (Seats and orgs). A guest makes nothing (decision 49), so the
shell offers them nothing to
make: once something is shared with them, their home is what is shared
(a chat in the middle, else the first app in its window), beside a note
of what a guest may do and what a seat gives, whose Get a seat opens the
welcome, which offers the seat to any guest, one whose seat ended and
who keeps their chats too; their settings are their Billing, their
account, then Your data. Its
settings hold the person's account (email, sign-ins, identity id,
picture, `/auth/link` to add another sign-in, a POST to
`/auth/logout`), their Billing (cell/shell/billing.js: for a guest, Get
a seat, to the welcome; a seat's kind,
org, trial's end and lapse, "let it sleep" for an always-on seat, this month's
credit and a credit pack; for an org's admin, its seats and admins to add,
change and remove, and Stripe's portal, the org shown once someone else
is in it: its admin alone sees Payment and invoices and "Add seats for
others", which shows it; a pack's Checkout's return,
`/settings?pack=<id>`, applied as the page opens),
what their standing stops, their computer and agents (each agent's
Rename, which opens the shell's own name dialog, and its picture: Add or
Change, from a file the browser draws again at most 512 px square when
it is past 256 KiB or no PNG, JPEG, WebP or GIF, and Remove; each written
and put live as Identities, "An agent's name and picture", says, the row
following the list as it changes), their skills (decision 17: the
managed set, read from their skills fragment's files by category, and
each agent's own, from its fragment's `skills/`; the shell makes the
skills fragment, kind `skills`, at setup beside their default agent),
their connections (every provider the deployment offers, one row each:
its kind, their state there with its action, which of their agents may
use it, pressed to narrow one, and this month's calls by agent with an
operator key's cost: `GET /api/connections`, the computer's view and its
`uses`), sharing (who may open each fragment they own, by fragment or
by person: `GET /api/access`), pairing the
CLI (the one-line install, `fragment login`, and `fragment skill` for a
coding agent), and Your data (Export, above: "Export everything", its
state and progress while it builds, read again every 2 s while the page
shows it, its size, until when it is kept and its Download once ready,
which downloads on the click with the platform session, and a refusal's
reason); its sidebar shows their agent and lists their chats, its
desktop's dock their apps, each one's share sheet (`/share/<name>`) in a
dialog (their own's "Share…" or "Invite…"; someone else's "People…",
where they see who is in and may leave), and the dock's Add shows ways
to ask their agent for an app (below), not templates: the templates are
the CLI's (`fragment create --template`) and its agents'.
It calls the API with the person's platform session
rather than a key: a request on the platform's host is taken as the
signed-in person when it carries `x-fragment-shell: 1` and
`Sec-Fetch-Site: same-origin`, and, writing, the platform's exact
`Origin`. Its list's socket (`GET /api/fragments/watch`), which can
carry no header, is taken by the platform's exact `Origin` alone, and an
export's download, a navigation that can carry no header either, by its
Fetch Metadata alone (the platform's own page started it: Export, above).
Anything else needs a signature as before. Adding a key still needs a
key.

Its sidebar is the person's list, one read (Skyler, 2026-10-12; Paul,
2026-10-13): their **one agent** above (their first, the one the first
run makes: the first their computer took, as its view lists its agents in
the order it took them; a menu of New chat, Rename, its profile and its
screen), then their chats (kind `chat`) as
one flat list, newest first (by the row's `previewAt`; one with nothing
said yet above them all), less the ones they archived (`PUT
/api/fragments/{name}/archived`, above). Its apps (kind `app` or `brain`)
are the desktop's: a dock of their icons and titles under the viewer's
windows, then Add, open beside the chat on a
desktop's width, a sheet from the bottom on a phone. **Their other
agents are the one agent's team** (decision 62, proposed): a row each,
one level down under it (`#team`: indented, smaller and quieter than the
person's chats), under "Your team", which folds them away (the open
chat's teammate kept) and says who works while folded; past five, four
show and "Show all" the rest. A row opens that agent's own chat with the
person (else its newest other chat with them, else a new one), chosen
as pressed; it is marked while one of its chats is open, those listed
under it: its other chats with the person (any chat of theirs with it
alone), then its hand-offs (`fragment ask`'s pairs, "Hand-offs from
<agent>"). It says when it works, its unread with the person (its
hand-offs count none: its agents' talk, which the one agent reports),
and a globe
when its model runs outside the platform (its `agent.json`: the
platform's models, or "<model> · <provider>, outside the platform"); its ⋯ says
where, and offers its hand-offs, "Ask <agent> to change it" (put in the
one agent's chat's composer, not sent), its screen, and its profile. Their chats (a
teammate's own chat, `<label>-chat`, any other of the person's with it
alone, and the hand-offs) never list in the person's list; search still
finds them. A prompt the shell puts in a
composer goes to the open chat when it is the person's own, never a
teammate's. Who works the shell reads from those
chats' `work` (a turn started and not ended), every 4 s while shown, at
most 12 chats. New chats, Enter and the launcher stay the one agent's.
**Add teaches
how to ask for an app** (Paul, 2026-10-14: "rather than spawning generic
apps we teach the user how to prompt for new apps"): a window of ways to
ask, each saying who the app is for and what it does, `…` the part to
make their own (`APP_PROMPTS`, twelve in one list in shell.js), and a
desktop with no apps says what lands there and offers the same ways.
Both show three at a time (Paul, 2026-10-16: "we shouldn't overwhelm
people with prompts"), the list's first three as a page loads, and a die
under them (🎲, "Show three more ideas") deals three more to both: never
one showing, and each of the twelve once before any again (a deck,
shuffled again when it runs out; a reload deals the first three again).
They are suggestions: nothing is made, sent or shown as if it were.
Pressed, one goes into the composer of the person's chat, selected at its
`…`, for them to change and send: the chat open, if it is their own; else
their agent's newest chat, or a new one with it (New chat, below). On a
phone the desktop's sheet closes on it. The shell puts it there with
`postMessage({fragment: "compose", text})` to that chat's frame, at the
fragment's own origin, once the page in it has asked `agents?` (below;
one pressed before then waits for it). The chat's page takes it only
from its parent at the platform's origin (its hello's `platform`, Live
above), only from a person who may post there, and sends nothing: it
replaces what was typed and any quote, and focuses the composer. A
message from any other page or origin does nothing. It is live: the page holds
`GET /api/fragments/watch` (above), and on each `changed` (a moment
after, so a burst is one read) reads the list again and patches the
sidebar in place, so an app their agent makes, one someone shares with
them, a delete or an archive elsewhere, and a chat's new message show
with no reload, the open chat and the rows already shown untouched (a
row the same as before keeps its element). Only the apps whose rows the
change touched have their card read again. A socket that closes is opened again after a
jittered wait (1 s, doubling to 30 s), and each opening reads the list
again, for what changed while it was shut.
A chat's row shows its `unread` as a count ("99+" at the list's bound),
but the open chat's: the chat shown in the middle column, while the page
is on screen, is marked seen (`PUT /api/fragments/{name}/seen`) as it
opens and whenever the list counts more on it, so the count goes in every
tab and device of the person's (their list tells them). Kept per person,
in their list, beside their archiving: the list already holds each chat's
messages, in order (their search's entries), so a count is one bounded
read of it, and a mark one row.
Each app's preview card (`GET /api/f/{name}/card`, Cards above, read
with the session and shown as a blob URL) shows larger over its dock icon
while a pointer is on it: an app without one is asked again, from 2 s
apart to a minute.
A chat with two agents or more is a **group** (decision 8; made by the
CLI, a page's add or an agent, not the shell): which agents a chat has is
its member list, as its row names them (`agents`), never its name; the
launcher's results stack their avatars, each in its identity's colour
(the chat page's FNV-1a choice), and its ⋯ offers each one's profile and
screen. The open chat's ⋯ renames it, invites people, and archives it.
**New chat** (the agent's menu in the sidebar, and the launcher) starts
another chat with the person's agent: a chat fragment on
the `chat` template labelled `<agent's label>-chat-<n>`, titled "New
chat", the agent added as an editor (which wakes its computer). Its
context is its own (an agent's runtime keys its session by its chat). An
untitled one with that agent where nothing has been said yet opens again
instead of another. The agent's own chat, `<label>-chat` (the first
run's, titled "Your first chat": below), is titled on its own as any
chat is, never by its agent: the sidebar names the agent above it, and
Rename in a chat's ⋯ renames that chat alone, as Rename in the agent's
menu and Settings' Agents rename the agent alone (Identities, "An
agent's name and picture"). It is the one `fragment ask` and Hermes'
Bot Mode find, by its label.
An agent shows its picture, where it has one, in place of its tinted
image wherever the shell draws it (the sidebar, a chat's mark, the
launcher, Settings). A chat still
titled "New chat" is titled from its first message with words (its first
line, at most 48 characters) by the person's shell once their list shows
a message in it, unless it was renamed first.

**A page of the person's own may ask for their agents.** A frame the
shell made of a fragment its person owns (the open chat, an app's window)
may `postMessage({fragment: "agents?"})` to it; the shell answers that
frame, at that fragment's own origin only (its status's canonical URL),
`{fragment: "agents", agents: [{identity, fragment, name, title, picture?, phase?, runsOn?, working?}]}`: the
agents its person's computer runs, `name` the label that `@mentions`
each, `title` and `picture` (an absolute address) what each shows by
(Identities, "An agent's name and picture"), and `phase` that computer's as the platform last said (`asleep`,
`starting`, `awake`, `sleeping` or `wont_wake`; absent before the shell
has read it); `runsOn` where its model calls go, `{on: "platform"}` or
`{on: "own", model, provider}` (a model of its owner's own provider,
outside the platform, as its `agent.json` says; `model` and `provider`
by their names for people: "DeepSeek V4.1 Flash", "OpenRouter"), and
`working: true` while it works
(decision 62, proposed). A frame may ask `{fragment: "open-handoff",
identity, via}` for one of them: the shell opens the chat a hand-off to
it went to beside the frame, and answers nothing. `via: "ask"` (`fragment
ask`'s) went to its chat with the agent that asked, the frame's chat's own
(its teammate's, else the person's default agent), else its newest
hand-off; `via: "message"` (`message_agent`'s) to its own chat. It sends the list again when any of it changes, its
computer's phase too (the list's socket tells the shell, which reads the
computer again). The frame may then ask
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

**The launcher** (the sidebar's +, a phone's top bar +, and ⌘K or
Ctrl K, from the chat's frame too: `{fragment: "open-launcher"}`, taken
only from the focused frame of a listed fragment at its own origin) is one
input over the desktop's landscape. Empty, it shows nothing. Words ask
`GET /api/search` (Search, below; its first 8 words and 256 bytes): the
chats and apps whose title or label hold them, then the messages, newest
first, archived ones marked. A chat found opens in the middle column, an
app in the desktop, a message its chat. For a seat whose agent is made,
Enter (or the first row, "Start a new chat with <agent>") makes a new
chat with it as **New chat** does and posts the words whole as its first
message (`POST /api/f/{chat}/channels/chat`, the person's), with an id
kept until it is sent, so trying again after an answer that was lost is
the same record; the launcher says "Starting your chat…" until the
platform answers, and opens the chat on its record. A request for an app
is a message like any other: the agent makes it. A guest's launcher only
finds.

**The first run, in its chat.** The person's first agent is made with
its chat (decision 10), once they hold a seat (one bought on the welcome
is set up on its card first: docs/billing.md, The shell), and the chat
comes first: the shell makes it, titled "Your first chat" (Paul,
2026-10-16: the sidebar shows the agent's name above it), and
opens it at once with `?setup=1`, then makes the agent (its job, its
`SOUL.md`, names it nowhere: its name is its title alone), assigns it to
their computer, adds it to the chat (which wakes the computer) and waits
for it to follow the chat. For that chat's frame alone the `agents`
answer also carries `setup: {steps: [{text, state}], error}` (`state`
one of `done`, `doing`, `todo` and `failed`; every other frame gets
`setup: null`, as that chat does once the agent follows it). Each step is
what the platform says: made once its calls answered, in the chat once
the members call answered, and the computer's phase as its view says;
nothing is said in the agent's name, and nothing of it is kept in the
browser: a reload reads the person's list, their computer and that
chat's members again and goes on from what was made (each call reuses
what an earlier try made, and an agent whose `agent.json` is written
keeps its files). The page shows the steps as a card above the chat, lets
the person type at once, and sends nothing until its own member list
(`__members`) has an agent: a message from before its joining starts
nothing (docs/chat-records.md). A step that failed says why, with Try
again: the frame asks `{fragment: "retry-setup"}`, taken as `agents?` is
(its source, its fragment's own origin, the person's own fragment) and
only for that chat's failed run; a retry reuses the chat and the agent,
and never reloads the frame or loses what was typed.

**A page may ask for a window.** A frame the shell made may
`postMessage({fragment: "open", url})`. A URL of the frame's own origin
is a file's window (the files viewer's, `__files` above). A page of
another fragment of the deployment opens as that fragment's window in the
viewer beside the chat, at the URL's path, signed in by the frame mint as
its row's window is (a fragment the person cannot open says so in the
window, as a row's does): the shell takes a URL as a fragment's only when
it asks from a frame of one of the person's fragments, from that
fragment's own origin, and the URL's host is that origin's with another
fragment's name in its place (the same scheme, port, branch mark and
zone). Anything else opens nothing. The chat's page asks so from the
"Open" after a link to another fragment's page (docs/chat-records.md,
"The page"). An app's window offers Reload, Open in a new tab
(`/auth/fragment`) and, in More, Copy link (its canonical URL). On a
phone the viewer is a sheet from the bottom.

**On a phone it is an app** (Paul, 2026-10-13: "staying strictly in the
viewport"). The page itself never scrolls, bounces or zooms: its html and
body are pinned (`overscroll-behavior: none`), and only its real scroll
areas scroll (the chat list, settings, the launcher's results, sheets, a
window's frame), each holding its own overscroll. The body is as tall as
the visual viewport, from where it is, read on its `resize` and `scroll`,
and holds everything fixed in the page: Safari ignores
`interactive-widget` and only shrinks and pans the visual viewport for its
keyboard, so the composer, the launcher's input and a window's fields sit
just above the keyboard. No field is under 16px on a narrow screen
(Safari zooms in on one as it takes focus), the viewport stays at its
scale, and the chrome (buttons, rows, menus, the dock) is neither
selected nor held, its taps immediate; what people write and read stays
selectable. Added to a home screen it opens standalone (the manifest; the
`apple-mobile-web-app-*` tags), its bars the page's own colour, light or
dark, and its edges keep clear of the status bar, the home indicator and
a notch (`env(safe-area-inset-*)`).

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

The shell's launcher (The shell, above) shows the chats and apps found,
then the messages; a message opens its chat (or app). It does not scroll to the message: the
chat's page has no way to be told one yet.

## Computers (docs/computers.md, phase 4)

A person's computer runs an image (pinned per computer) and the agent
fragments assigned to it; the Computer Durable Object (`cell/src/computer.rs`)
is the only thing that talks to its container. What a guest may rely on
is docs/computers.md; the routes here are its owner's, but for starting
and stopping an agent, which their agent also does for them (Principals
and access, above: decision 62, proposed). Any other route an agent asks
is 403.

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/computers` | a person | → `{computer, owner, image, phase, why?, agents, origin}` (`ComputerView`): their computer, made asleep on the deployment's default image (`FRAGMENT_COMPUTER_IMAGE`); again, the same one (its id is derived from its owner). Made, it asks the registry to push its owner's seat to it (always-on: Seats and orgs); when the registry does not take that ask, the computer is made and the answer is the registry's failure (503 `registry_unavailable`), so its caller makes it again, which asks again |
| `GET /api/computers` | a person | → `{computers: [ComputerView], support}`: `support` is where a person whose computer will not start gets help (`FRAGMENT_SUPPORT_URL`), or `null` |
| `GET /api/computers/{id}` | its owner | → `ComputerView`: `phase` is `asleep`, `starting`, `awake`, `sleeping`, or `wont_wake` (its starts kept failing; `why` says why); `restored` is what its last start that came up restored (`{generation, from: snapshot \| backup \| nothing, save?, savedAt?, ageMs?, after?: sleep \| exit, rollback, at}`: the save's id, when it was taken and how old it was then, and how the life before that start ended; absent before its first start), `rollbacks` counts its starts that went back in time (the life before ended by a crash, or by a sleep that slept unsaved, or its start fell back to an older save: docs/computers.md, "Saves and what a wake restores"), and `saves` lists the saves of its `/data` it keeps, newest first (`[{number, id, at, generation, held, unusable?}]`, at most three); `why` also says when its sleep's save failed and it stays awake, or slept unsaved; `generation` is its newest start (what a restart names); `notices` is what its owner is told, the most pressing first (proto's `ComputerNotice`; docs/computers.md, "What its owner is told"): `{kind: "wont_wake", why}`, `{kind: "unsaved", since, save?, failingSince, failures, why, stopsAt?}` (its saves failing: its work in no save since `since`, a stop going back to save `save`, the bound stopping it at `stopsAt` unless a save works or something uses it), `{kind: "went_back", life, cause: crash \| unsaved \| restart \| unusable, endedAt?, pending, save?, savedAt?, at?}` (a start went back to save `save`, or will as it next starts, until its owner says they saw it), `{kind: "update", version}` (awake, with no turn running, on another image than its next start runs: an update is ready, which a restart takes, or its next sleep and wake, until its owner puts that version off); absent when none. `version` is the version of the image it runs (its digest's first 12 hex digits; none while it runs nothing), `latest` the one its next start runs (its pinned image as the deployment has it now). Their open pages are told when it changes (`/api/fragments/watch`'s `changed`); anyone else 404 |
| `POST /api/computers/{id}/wake` | its owner | → the view once it is awake (a wake also lifts `wont_wake`); 503 `wont_wake` when it would not start |
| `POST /api/computers/{id}/sleep` | its owner | → the view once it is asleep: the guest held, `/data` saved, the guest signalled, the container gone. When the save fails, the view is awake (its container kept, its `why` saying so), and its sleep is tried again on its own (docs/computers.md); asked again, it tries at once. It answers within the sum of its steps' bounds (docs/computers.md, "Bounds"), 10 minutes 50 s at most: the hold 55 s, the save 9 minutes (its backups 8, a save whose backups never answer failing at it), the stop 55 s; the calls it makes to the platform's own Durable Objects (its owner's ledger, their open pages) aside, and a start in the same call when a wake came as it slept, within that start's own bounds. A sleep under way already answers its view at once (`sleeping`) |
| `POST /api/computers/{id}/restart` | its owner | `{generation?}` (no body: whatever runs) → the view once it has started again: its way back to working. Running, a sleep that saves if it can and stops whether or not it did (a failed save keeps no container), then a start at once; asleep or `wont_wake`, a start (lifting it). The start is fresh: the image and the newest good save, never a snapshot. `generation` names the start the owner saw: a restart of one already restarted (the same press again, another tab's) restarts nothing and answers the view. Its owner's ledger is asked first, as for a wake: at zero credit 402, a guest 403, and what runs runs on |
| `POST /api/computers/{id}/notices/seen` | its owner | `{life}` → the view: its owner saw the `went_back` notice of the start `life` lost, which is told no more (nor any loss before it); again, the same. A `life` past the newest loss told, or 0, is 400 |
| `POST /api/computers/{id}/notices/later` | its owner | `{version}` → the view: its owner put off the update to `version` (an `update` notice's, the view's `latest`), which is told no more; a version other than it is told again. Again, the same. A version its next start does not run, or that is no version (12 hex digits), is 400 |
| `PUT /api/computers/{id}/image` | its owner | `{image}` → the view: the image it starts from at its next wake (an upgrade, or a rollback), its `/data` restored; awake on another, its owner is told an update is ready (`update`); an image the deployment lacks is 400 |
| `PUT /api/computers/{id}/agents/{fragment}` | the owner of both, or their agent for them (not the fragment's own agent) | → the view: the agent fragment runs on it. The fragment's own key becomes the agent's identity (registered to its owner), an editor of its own fragment; it signs the guest's requests only while it is assigned here. Assigning it again changes nothing. Nothing restarts: an awake computer's guest reads its agents again while it runs and runs the new one (our Hermes image within seconds; docs/computers.md); a sleeping one's reads it as it starts |
| `DELETE /api/computers/{id}/agents/{fragment}` | the same (an agent never stops itself) | → the view: it signs nothing for the guest from now on; an awake guest stops running it as it reads its agents again. An agent fragment deleted leaves its computer by its delete (`DELETE /api/f/{name}`); asked of one gone, or of one the computer does not run, it answers the view, unchanged: done, not an error |
| `PUT /api/computers/{id}/agents/{fragment}/connections` | its owner | `{connections: [provider] \| null}` → the view: the providers of the catalog (connections, operator keys, own keys) the agent may have swapped in (decisions 22 and 37), all named at once. `null`, the default, is every one its owner has (decision 44: a person's agents are not fenced from each other); a list narrows the agent to those, and its guest is given the rest no more. A provider the deployment does not offer (`FRAGMENT_PROVIDERS`) is 400, as is a body without `connections` |
| `GET /api/computers/{id}/uses` | its owner | → `{computer, month, uses: [{provider, agent, calls, micros}]}` (proto's `ComputerUses`): this month's (UTC, `YYYY-MM`) calls through the computer's swap that a provider answered (under 500), by provider and agent fragment, and what they were charged: an operator key's at the price book's price and the margin (as its owner's ledger charged them), a connection's and an own key's `0` (counted, never charged). Thirteen months are kept |
| `GET /api/computers/{id}/uses/{YYYY-MM}` | its owner | → the same, for that month; a month that is not one is 400 |
| `POST /api/computers/{id}/ports/{port}/ticket` | its owner | `{path?}` → `{url, expiresAt}`: a one-time link (two minutes) that signs a browser in to the computer's own origin, `<24 hex>--computer.<suffix>` (`/__ticket`, then `/p/<port><path>`), cross-site from the platform, in a tab of its own or a frame of the platform's page (below); a signed request needs none. `path` is where on the port it lands, `/` by default: a path and query the image reads (an agent's screen: `/?agent=<agent fragment>`, docs/computers.md, Ports), which the platform carries and never reads; it starts with `/`, is at most 512 visible characters, and has no `//`, `#`, `\`, `.` or `..` segment, else 400 |

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
fragment (a member's `PUT`, a fragment it makes
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
