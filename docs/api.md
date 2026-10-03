# fragment wire contract

The cell (`cell/`, Rust on Cloudflare Workers) answers everything below; the CLI and
the browser library are its clients. Errors are `{"error": "<code>",
"message": "..."}` (codes in `crates/proto`). `cargo xtask e2e` proves
every route. The TypeScript runtime this replaced was deleted in phase 2
slice G (its contract is in git history, last at `35f5e18`). Hermes,
computers, the desktop, and the personal agent's chat went at the cut
(docs/cloudflare-v1.md, decision 33); their contract is at the tag
`celld-final`.

## Configuration

Worker variables, rendered from the deployment's config by `cargo xtask
deploy` (`deploy/example.jsonc`; `.dev.vars` in dev). None is a secret:
the deployment's secrets are Worker secrets (below).

| Variable | Meaning |
|---|---|
| `CODESTORAGE_ORG` | the code.storage org |
| `CODESTORAGE_API_URL` | the API base (default `https://api.<org>.code.storage`) |
| `FRAGMENT_HOST_SUFFIX` | fragments are served from `<label>--<username>.<suffix>` (any other name under it is 404, never the platform; the suffix's own name is the platform's, or redirects to it: Moved hosts, below); unset, from `/f/<name>/` |
| `FRAGMENT_HOST_LABEL_SUFFIX` | a branch deployment's mark, `--<branch>`: its fragments are `<label>--<username>--<branch>.<suffix>`, one DNS label beside the other branches' in one zone |
| `CODESTORAGE_REPO_PREFIX` | what this deployment's repos are named with first (a branch's `<branch>--`), so deployments sharing an org never share a repo |
| `FRAGMENT_LEGACY_HOST_SUFFIX` | where fragments were served before the suffix moved: a fragment's host under it redirects to its host under the suffix (Moved hosts, below); counted only beside a different suffix. fragment.club's is `fragment.club`, its suffix `fragment.boats` |
| `FRAGMENT_POLL_INTERVAL_S` | the webhook backstop (default 300), and how often running runs are checked against their Workflows, for a busy fragment: one something outside the platform may have written in the last day (a storage token was minted for it, or a webhook arrived), or with a run in flight, an ended run's reservation to give back, or a template or the agent it declares still to land. Any other fragment is polled once a day |
| `FRAGMENT_JOB_RETRY_DELAY_S` | a failed job step's first retry delay, doubling over 4 retries (default 10) |
| `FRAGMENT_EGRESS_LOCAL` | `allow` lets jobs fetch loopback and private addresses (dev and e2e fakes); never on a shared fleet |
| `FRAGMENT_BLOB_GRACE_S` | how long a blob no branch names is kept before it is deleted (default 7 days) |
| `FRAGMENT_PUSH_SUBJECT` | who push services may contact about this fleet's pushes (a `mailto:` or https URL; RFC 8292) |
| `FRAGMENT_DELIVERY_RETRY_S` | the shortest wait before a delivery is retried (default 10; the wait grows with the delivery's age, up to an hour) |
| `FRAGMENT_DELIVERY_RETRY_MAX_S` | the longest (default an hour, never under the shortest; test fleets set both, for a fixed pace) |
| `AI_GATEWAY_ID` | the AI Gateway the model route and image steps call through (Models, below): the deployment's own, named (`default` is refused: it makes one that logs); unset, models and images are off |
| `FRAGMENT_AI_URL` | dev and the e2e only: the model route POSTs the AI binding's input to `<url>/run/<model>` instead of calling the binding (the Workers AI fake, a lower rung) |
| `FRAGMENT_DEFAULT_PLAN` | a new person's plan (Ledger, below): `guest` (the default and production's), `seat`, or `seat_always_on`; dev and the e2e set `seat` |
| `FRAGMENT_OPERATORS` | identities and keys that grant credit and set plans, seats and overdrafts, and release usernames (as `FRAGMENT_CREATORS` once read them) |
| `FRAGMENT_DEPLOY_ID` | which deployment this is (default `dev`); `GET /healthz` answers it in `x-fragment-deploy` |
| `WORKOS_CLIENT_ID` | sign-in: fragment's WorkOS environment; unset, sign-in answers 500 |
| `WORKOS_API_URL` | where WorkOS is (default https://api.workos.com; dev and the e2e: the fake) |
| `FRAGMENT_PLATFORM_URL` | the platform's origin, where sign-in and the platform session live (default: the hostname suffix itself; fragment.club's is https://fragment.club, on no fragment's domain) |
| `FRAGMENT_SIGNINS_PENDING_MAX` | sign-ins begun and not finished that the registry keeps (default 100000; at least 1): a sign-in is kept through this many later starts, so the oldest is let go only past this many starts in its ten minutes (Sign-in, below) |
| `FRAGMENT_TEST_HOOKS` | `allow` on dev and e2e fleets only: `POST /api/test/registry {down}` makes the registry answer 503 (until it is set back, or the registry restarts), `{calls: null}` answers `{calls}`, how many calls the registry has had since it started (a test counts a request's round trips by the difference), `{hold: ms}` makes its next call wait that long (at most 10 s) before it is answered, while other calls go on, and `{signins: "count"\|"expire"\|"sweep"\|{expireSession: token}}` counts sign-in's rows (`{logins, redemptions, sessions}`), expires every pending sign-in and unspent redemption, runs its sweep now, or expires the one session a cookie's token names (a platform session's site sessions end with it); `POST /api/test/keys {fragment, op, plaintext\|sealed}` seals or opens as that fragment; `POST /api/test/fragment {fragment, op, …}` pulls a lever on that fragment: `fail-deliveries {times}` fails its next queue sends, `fail-outbox {times}` fails its next records' outbox writes just after their append, `fail-triggers {times}` fails its next trigger steps just before their last run starts, `fail-join {times}` fails its next joins of the agent its `agent` block declares, before anything is asked, `drop-effects {times}` loses its next job step answers on their way back to the Workflow (after the step ran and its answer was kept), `forget-steps` forgets the kept answers of its runs in flight, `hold-advances {on}` holds each advance after a run's first step while on (at most 20 s), and `advance-held` answers `{run}`, the last run it held, `forget-live` makes it forget what it knows of its live sockets beyond their attachments (as waking from hibernation does), `age-live {ms}` makes every live socket's identity check `ms` older (as if that long had passed), `drop-live {code}` drops its live sockets, `ledger {ms \| null}` shortens (or restores) its operation ledger's window, `age {ms}` forgets its write keys as if `ms` had passed, `members {fill}` adds placeholder members until there are `fill`, `code-builds` answers `{builds}`: how many times the fragment's activation built its app's worker code for the loader, `alarm` answers `{alarmAt, pollAt, now}` (ms): when its alarm and its next poll are set for, `age-outside {ms}` makes the last sign of an outside writer (a storage token, a webhook) `ms` older, `fail-after-paid {times}` fails its next paid AI steps just after their call was paid and kept (so the step is tried again), `fail-meter-acks {times}` loses its next meter batches' acknowledgements (so the queue delivers them again), `meter-now {sample?, resend?}` closes every counted minute, takes a storage sample (unless `sample: false`) and sends its outbox's batch now (a waiting one again with `resend`), answering the outbox, `meter` answers the outbox, `forget-standing` forgets what it heard of its owner's standing (meter.rs), and `cron-now` makes each of its cron schedules due at once (`{due}`: how many), so a test need not wait for a schedule's minute |

Worker secrets (`cargo xtask deploy` uploads them from the files the
deployment's config names; `.dev.vars` in dev), read only by
`cell/src/keys.rs`. An app's isolate gets an env the platform builds, so
no author code can name one:

| Secret | Meaning |
|---|---|
| `FRAGMENT_HOST_SECRET` | seals values at rest, per Durable Object (at least 32 bytes) |
| `FRAGMENT_HOST_SECRET_PREVIOUS` | the secret before a rotation; values sealed under it open and come back resealed |
| `CODESTORAGE_PRIVATE_KEY` | the org's PKCS#8 P-256 key, which signs code.storage tokens (a one-line PEM may carry literal `\n`) |
| `WORKOS_API_KEY` | WorkOS's API key, for its code exchange |

A request body is at most what the zone's Cloudflare plan takes (100 MB
on Free and Pro), below the 256 MiB a blob route allows (debt ledger).

Bindings (`cell/wrangler.jsonc`): `FRAGMENT`, `PRINCIPAL`, `REGISTRY` and
`LEDGER` (Durable Objects), `LOADER` (the Worker Loader), `JOBS` (the
Workflow that runs jobs), `BLOBS` (the deployment's R2 bucket: the bytes
of large files), `DELIVERIES` (the `fragment-deliveries` queue, and its
dead-letter queue `fragment-deliveries-dead`), `METERS` (the
`fragment-ledger` queue: each fragment's meter batches, consumed by the
cell), and `AI` (Workers AI, through `AI_GATEWAY_ID`). A local node runs
without `AI`, which `wrangler dev` would only reach remotely: dev and the
e2e set `FRAGMENT_AI_URL`.


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
body the router reads
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

An agent acts for whoever asked, capped (ROADMAP decision 17). A request
signed by an agent may name an identity in `for=<id:…>` in its URL's
query (inside the signed URL, so the signature covers it): the request
acts with the lower of the role that identity holds in the fragment (its
membership, an agent of its own that is a member, or the visibility
floor; never the share link, which the agent does not hold, but for a
fragment's own agent on its own fragment: A fragment's agent) and the
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
agent's own membership. Owner-only actions (members, other than leaving
with `DELETE members/me`; invites; visibility; rotation; deletion) are
403 for an agent whatever it names. A site request's query is its app's:
`for` there means nothing to the platform.

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
| `PUT /api/identities/{agent}/held` | the agent's owner | `{held: "viewer" \| "editor" \| null}` → the agent: held below its owner (decision 36), it acts with at most that everywhere, for whomever it acts; `null` lets it go; anyone else 403, `owner` or `public` 400 |
| `PUT /api/identities/me/username` | a person | `{username}` → `{username, claimed}`: chosen once (3 to 32 of lowercase letters, digits, and single dashes, not starting or ending with one, and not a reserved word); taken 409, another after yours 409, yours again `claimed: false` |
| `PUT /api/identities/me/picture` | a person with a username | the image (PNG, JPEG, WebP, or GIF, told by its bytes; at most 256 KiB) → `{sha, mime}` |
| `GET /api/users/{username}` | anyone | → `{id, kind, username, picture}` (`picture`: its URL, or null) |
| `GET /api/users/{username}/picture` | anyone | the picture's bytes |
| `DELETE /api/users/{username}` | the fleet's operators | → `{username, identity, released}`: undoes a username taken by mistake, so its person chooses again; refused (409) while they own a fragment under it (its URLs name it) |

## Names (decision 16)

A person chooses a **username** once (above; the platform's page asks
after the first sign-in, and `fragment username <name>` does too). A
fragment's **name** is `<label>.<username>`: `todo.futurepaul`,
served at `todo--futurepaul.<suffix>` (one DNS label, under the
suffix's one wildcard certificate; or `/f/todo.futurepaul/` on a
fleet without a suffix), its code.storage repo `todo--futurepaul`.
A label and a username never contain `--`. Creating with a bare label
puts it under the creator's username; creating under someone else's is
403. In a signed request's path, a bare label names the signer's own
fragment (`/api/f/todo/status` is `todo.<your username>`); anything
unsigned (an inbox, a webhook, a site) names it in full.

## Sign-in (phase 4 slice B)

A person is keyed by their verified `(issuer, subject)`: the issuer is
`workos:<client id>` (the environment), the subject WorkOS's user id. The
email is an attribute, refreshed at each sign-in and never matched. The
platform holds no key for a person; browsers hold sessions, each looked up
live in the registry on every request whose answer depends on who is
asking (Principals and access, above; a 30-day lifetime; tokens are 32
random bytes, the registry keeps their SHA-256).

| method & path (platform origin) | what |
| --- | --- |
| `GET /`, `GET /settings` | the shell's page, for anyone (The shell, below): signed out it asks them to sign in, without a username it asks for one; at `/settings` it opens its settings |
| `GET /auth/login?return=&login_hint=` | → WorkOS's authorize URL (`provider=authkit`, `redirect_uri` `<platform>/auth/callback`, a state); the state is bound to the browser by `fragment_login` (HttpOnly, SameSite=Lax, `Path=/`, ten minutes) |
| `GET /auth/link?return=` | the same from a signed-in browser: the sign-in that comes back joins this person (409 when it is someone else's) |
| `GET /auth/callback?code=&state=` | the state must match the browser's cookie (400 otherwise); the code is exchanged server-side; → `fragment_session` (HttpOnly, SameSite=Lax, `Path=/`) and back to `return`; a WorkOS `error` is shown (400); a sign-in already finished or past its ten minutes, or a code WorkOS refuses (a callback sent again), is 400 `invalid_request` |
| `POST /auth/logout` | ends the session and every fragment session made from it, clears the cookie, and sends the browser to WorkOS's logout (`session_id` from the access token's `sid`); from another origin, 403 (`GET` shows the button) |
| `GET /auth/fragment?name=&return=` | signed in: → `<fragment origin>/__signin?token=<a single-use redemption, 60 s, for that fragment only>` (a session holds at most 16 unspent; past that, the oldest is refused), at once for a fragment of the person's own, one shared with them, or one they said yes to; for any other, first a page asking "Continue to X?" (Asking first, below); signed out: → sign in first |
| `POST /auth/fragment?name=&return=` | that page's form (`form`, its token): the yes, remembered, then → the fragment's `__signin?token=` (303); another origin, or a missing or stale token, 403 |
| `GET /auth/frame?name=&return=` | a frame of the platform's own page (the shell's tabs; Frame sessions, below) signs in to the fragment: → its `__signin?token=<a frame redemption>` (`Cache-Control: no-store`, `Referrer-Policy: no-referrer`) where `/auth/fragment` would redeem at once; where it would ask first, or no one is signed in, a note in the frame. Anything but a frame of the platform's own page (by Fetch Metadata) is 403, and so is every request on a fleet without a suffix |
| `GET /cli?key=<npub>&proof=` | the link `fragment login` prints: `proof` is the key's own NIP-98 event for `POST <platform>/cli/approve`, good for ten minutes (the proof of possession; without it, stale, or by another key: 400). Signed in: a page showing the key's last eight characters, to compare with the terminal, and an Add button; signed out: → sign in first, keeping the link |
| `POST /cli/approve` | the page's form (`key`, `proof`): the key joins the signed-in person at once; a key someone else holds, or a revoked one, is 409; another origin 403; the CLI waits for `GET /api/identities/me` to answer. People themselves come only from sign-in (`POST /api/identities {kind: "person"}` is 400) |

On fragment.club the platform is cross-site from every fragment
(`fragment.club` and `<label>--<username>.fragment.boats`), so its
SameSite=Lax session cookie reaches a fragment's page only on a
top-level visit. A fleet whose platform shares the fragments' domain
(`FRAGMENT_PLATFORM_URL` unset) puts them on one site, where the cookie
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
`fragment_site` (HttpOnly, SameSite=Lax, host-only, `Path=/` or
`/f/<name>/`). A frame redemption (Frame sessions, below) is redeemed
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

### Which cookies count (docs/fragment-boats.md, decision 3)

Every fragment's origin is one site with the others (all of them are
under `fragment.boats`, which the Public Suffix List does not list:
docs/fragment-boats.md), so a SameSite=Lax cookie rides along on another
fragment's images, scripts, fetches, and forms. The router counts a
browser's cookies on a fragment's origin by the Fetch Metadata it sends
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

### Frame sessions

A frame session is a fragment's session in a frame of the platform's
own page (the shell's tabs: docs/cloudflare-v1.md, decision 6), bound to
the platform's origin (docs/fragment-boats.md, design C):

- `GET <platform>/auth/frame?name=&return=` mints it, for a frame of the
  platform's own page only: its Fetch Metadata, which no page's script
  sets, must be a frame's navigation (`Sec-Fetch-Dest: iframe` or
  `frame`, `Sec-Fetch-Mode: navigate`) that a page on the platform's
  origin started (`Sec-Fetch-Site: same-origin`), on the platform's own
  origin. A page on any other origin (a fragment's page, its author's
  code, framed in the shell or anywhere) sends `same-site` or
  `cross-site` and is refused (403); so is a top-level visit, a fetch, an
  `object` or `embed`, and a request without Fetch Metadata. A fleet
  without a suffix, whose fragments share the platform's origin, mints
  none (403).
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

### Opening a fragment by its URL (ROADMAP decision 4)

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

## Sharing (phase 7, decision 4)

The share sheet and accepting invites are the platform's pages, never a
fragment's: a fragment's page is its author's code (or an agent's), and
sharing grants. Each acts through the fragment's own routes (members,
invites, visibility, rotate, join; below) as the person signed in on the
platform, so the fragment decides who may do what.

| method & path (platform origin) | what |
| --- | --- |
| `GET /share/<name>` | the share sheet, a card laid out as a document's share dialog: who is in (usernames and pictures, from the registry's profiles) and their roles, to any member (anyone else, a 403 page); for the owner, adding people by username (an invite), the pending invites (revoke), each member's role menu (viewer, editor, or removing them), who can open it ("General access": Restricted `members`, Anyone with the link `link`, Public `public`), each menu sent as it changes; Copy link (the share link while it opens it, else its address) and Done (in a dialog, closes it, as Escape does; in a window of its own, closes that, or goes to `/settings`); then, quieter, a new share link. Signed out: → sign in first, and back. It reads nothing from its URL |
| `POST /share/<name>` | the sheet's form: `form` (the page's token), `action`, and its fields: `invite` (`username`, `role`: an invite for them alone, one use, seven days; answers the sheet with the `/join` link to send them), `role` (`member`, `role`: `viewer`, `editor`, or `remove`, which removes them), `remove` (`member`), `uninvite` (`invite`: its id), `visibility` (`visibility`), `rotate` (the share link only; the inbox's token and the webhook's secret are the CLI's). Done: → 303 back to the sheet; refused by the fragment (a member who is not the owner: 403): the sheet, saying why, with the refusal's status |
| `GET /join/<name>?token=` | what the invite grants (the fragment, the role, who invites), and a Join button; signed out: → sign in first, and back. An invite for someone else: a 403 page naming them; used, revoked, or expired: 404; the person is in already (at that role or above): a link to open it |
| `POST /join/<name>` | the page's form (`form`, `token`): joins as the signed-in person, then → `/auth/fragment?name=<name>&return=/` (signed in on its origin, and there) |

Neither page can be driven by a fragment's page. Every POST's `Origin`
must be the platform's (403 otherwise), and every POST carries `form`,
the token its page was made with: an HMAC, keyed by the session's own
token (the HttpOnly cookie, which no page's script reads), of what the
form does (`share:<name>`, `join:<name>`) and when its page was made
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

## Control API

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/fragments` | a person with a username, not a guest; an agent for its owner (the fragment is the owner's, under their username, billed to them, with its maker an editor)
 | `{name, visibility?, template?}`: `name` a label, or `<label>.<your username>` → `{name, npub, owner, visibility, viewToken, inboxToken, webhookSecret, repo, canonical}` (`name` in full). Its maker's ledger is asked first (`Spend::Create`): a guest's create is 403 `forbidden`, "guests can't create fragments: …" (Paul, 2026-10-03: a fragment's hosting bills its owner, and a guest pays for nothing; a guest still edits fragments shared with them), however it is asked (a template's, an agent's for its owner, the shell's catalog), and nothing is made; past the overdraft it is 402 `budget_used_up` (the maker's fragments are read-only). A ledger that does not answer refuses none. `visibility` defaults to `link`. The fragment's own key is made in its cell and kept sealed for it. The cell creates (or, for a name deleted before, finds) the code.storage repo. With `template` (`blank`, `todo`, `inbox`, `calories`; any other is 400 and nothing is made), the template's files are main's first commit (its `fragment.json` stamped with the fragment's name) and live at once; one that fails to land is retried by the fragment's alarm (`template.failed` events). `chat` and `agent` are blessed (decision 40), named and not copied: main's first commit is `{"template", "meta": {title}}` (`title`, theirs alone), and the platform's release serves the rest (Apps). `notes` is the CLI's only (`fragment new --template notes`). |
| `PUT /api/fragments/{name}/archived` | any signer, for a fragment they hold a role on | `{archived: bool}` → `{name, archived}`: the signer's own view of it (the shell leaves it out of its sidebar; search still finds it), kept in their list's row and nowhere else, so no one else's list or the fragment changes. The same again answers the same. A bare label names the signer's own; a fragment they hold no role on, or none of that name, is 404; a name that is none, or a body without a boolean `archived`, 400. It goes when they leave the fragment (back in, it is not archived), or the fragment is made again. Not honored for `for` |
| `GET /api/search?q=` | any signer | → `{fragments: [ListedFragment], messages: [{fragment, channel, seq, at, snippet}]}` (`SearchAnswer`): the signer's fragments whose title or label hold every word of `q`, then the messages that do, newest first, from fragments they hold a role on now, archived ones included (The shell, Search, below). `q` once, at most 256 bytes and 8 words (400 past either, or without it). Not honored for `for` |
| `GET /api/fragments` | any signer | → `{fragments: [{name, role, kind, title?, sharing?, archived?}]}` (`archived: true` on the ones the signer archived); `sharing` on the signer's own fragments only: `{visibility, members, guests}` (guests: members who are neither the owner nor an agent of theirs), as the fragment last sent it with a change to its members or visibility (a fragment from before sends it once, on its next change or alarm; until then it has none); an agent's `?for=<id>`: the fragments that identity holds a role on where the agent or its owner is a member too, each with the role the agent acts with there for it (`fragment_core::access::listed_role`; a call decides again) |
| `DELETE /api/f/{name}` | owner | → `{ok, deleted}`; the app's database goes too; the repo stays |
| `GET /api/f/{name}/status` | viewer | → `{name, npub, owner, role, visibility, repo, pins: {main, live}, counts: {files, events, members}, code: {sha, operations, error}, viewToken, inboxToken (editor), urls: {canonical, platform}, blobMinBytes}`; `urls.platform` is the platform's own origin, for links a person opens (a client in a computer calls an internal host) |
| `GET /api/f/{name}/manifest` | viewer | → `fragment.json` at main (404 when there is none) |
| `GET /api/f/{name}/members` | viewer | → `{members: [{principal, role, addedBy, addedAt, kind, owner?}]}` (`owner`: an agent member's) |
| `PUT /api/f/{name}/members/{id\|npub}` | owner | `{role: viewer\|editor, peopleOnly?}` → the member; a key names the identity holding it (404 when no one registered it). `peopleOnly: true` (decision 36): the share lends the member's agents nothing, so they act there only with memberships of their own. A new member that is an agent running on a computer is announced to it: `joined` on its agent fragment's `tasks`, and a wake (Computers, below) |
| `DELETE /api/f/{name}/members/{id\|npub\|me}` | owner, or the member | → `{ok, removed}`; closes that member's change feeds (and its owner's, when an agent's membership was their only view) |
| `POST /api/f/{name}/invites` | owner | `{role, uses? (1), ttlS? (7 days, at most 30), invitee? (id:…)}` → `{id, role, usesLeft, expiresAt, createdBy, invitee?, token}`; the token is shown once. With `invitee`, only that identity may accept it (the share sheet's invite by username); without, whoever holds the token |
| `GET /api/f/{name}/invites` | owner | → `{invites: [...]}` without tokens |
| `DELETE /api/f/{name}/invites/{id}` | owner | → `{ok, revoked}` |
| `POST /api/f/{name}/join` | any signer | `{token}` → `{name, role, joined}`; a stronger existing role is kept; a fragment at its 1000 members is 400, and the invite keeps its use; an invite for another identity is 403, and keeps its use |
| `POST /api/f/{name}/join/preview` | any signer | `{token}` → `{name, role, invitedBy, invitee, expiresAt, current}`: what joining would grant (`current`: the signer's role now), joining no one; 404 for a token that names no open invite (the platform's `/join` page shows it) |
| `PUT /api/f/{name}/visibility` | owner | `{visibility}` → `{ok, visibility}` |
| `POST /api/f/{name}/rotate` | owner | `{scopes?: [inbox, view, webhook]}` → `{inboxToken, viewToken, webhookSecret, rotated}` (`Rotated`): every token as it is now, and the scopes renewed; a new view token closes link holders' feeds |
| `PUT /api/f/{name}/secrets/{KEY}` | editor | raw body (at most 64 KiB) → `{ok, name}`; sealed (AES-256-GCM, key HKDF'd from the host secret and the fragment's npub) |
| `GET /api/f/{name}/secrets` | editor | → `{names}`; values never leave |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor | → `{ok, removed}` |
| `GET /api/f/{name}/storage-token` | editor | → `{token, repo, api, expiresAt}`: ES256, this repo, `git:read`+`git:write`, 15 minutes |
| `POST /api/f/{name}/refresh` | editor | → `{ok, refs: {main: {pin, moved} \| {absent}, live: ...}}`, once a live that moved is installed and the agent it declares has joined (`fragment deploy` asks it). A fragment reads the branches it has no pin for once, on its first request (a push may predate its webhook); after that a move arrives by the webhook, this, or the poll backstop, and a site with nothing deployed answers 404 without asking code.storage |
| `POST /api/f/{name}/files` | editor | `{files: [{path, text \| base64} \| {path, delete: true}], message?, key?}` → `{commit}`: one commit to main, as a sync makes (at most 16 files and 256 KiB; paths relative, no `.` or `..`). The same `key` from the same person answers the first commit again. Main's pin moves at once; live does not |
| `POST /api/f/{name}/deploy` | editor | `{note?}` → `{live, canonical}`: live to main's tip, as `fragment deploy` does (a first deploy makes the branch; after a rollback, a merge commit), guarded against a live that moved meanwhile; the app installs from it, and the agent its `agent` block declares joins, at once |
| `POST /api/f/{name}/webhook` | code.storage | signed with the fragment's webhook secret (`X-Pierre-Signature`, 5 minutes); validate, remember (redeliveries are acknowledged), then move the pin to the branch's head as read now |
| `GET /api/f/{name}/files` | viewer | → `{ref, files: [{path, size, mode, lastCommitSha, machinery, blob?, release?}]}` at main; a pointer's `size` is its bytes'. A fragment on a blessed template lists that template's data from the release beneath its own files (`release: true`, `lastCommitSha` `release:<hash of its bytes>`: templates/skills/README.md) |
| `GET /api/f/{name}/file?path=` | viewer | → the bytes at main (`x-fragment-ref`); a pointer's come from the blob store; with none of its own at `path`, a blessed template's data file from the release |
| `PUT /api/f/{name}/blobs/{sha256}` | editor | the bytes as the body (`content-length` required, at most 256 MiB), streamed through and hashed on the way in: → `{ok, sha, size, stored}`; bytes that hash to anything else are deleted and refused (400). Its `content-type` is what `__blob` serves it as, when that is passive media (Blobs, below) |
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
| `GET /api/f/{name}/channels` | viewer | → `{channels: [{name, read, post, signedIn, seq}]}`: `events`, `ops`, `inbox`, and the app's (`post`: who may post, or null; `signedIn`: only people signed in) |
| `GET /api/f/{name}/channels/{channel}?after=&limit=` | the channel's reader | → `{channel, records: [{channel, seq, at, principal, kind, body}], next}` (1000 a page) |
| `POST /api/f/{name}/channels/{channel}` | the channel's `post` role | `{id, body}` → `{record, replayed}` (`Posted`): the platform appends `body` (any JSON, at most 64 KiB of it; 413) as a record of kind `message` naming the poster, with no app code; it reaches sockets, subscriptions, and the channel's triggers as a mutation's record does. The same id and body again answer that record and append nothing (a retry also finishes what the first try left: its deliveries, its triggers' runs); the same id with another body, or on another channel, is 409 (ids are the poster's, kept as long as the record). A channel without a `post` role, and `events`, `ops`, and `inbox`, refuse posts (403); one that says `signedIn` refuses an anonymous poster (401). A poster holding only `public` spends a public call (Serving, `__op`) |
| `PUT /api/f/{name}/channels/{channel}/draft` | the channel's `post` role | `{turn, text}` → `{ok}`: a record its poster is writing, its whole text so far (at most 64 KiB), shown to the sockets following the channel at once (a `draft` frame on `__live`) and never stored; `text: null` stops it. The record its poster then writes with the same `turn` replaces it on a page. At most 10 a second across the fragment (429 past that: the poster's next carries the whole text anyway); `turn` is `^[A-Za-z0-9._:-]{1,128}$` |

## Apps

Code comes from git: when `live` moves, the cell reads `fragment.json`
and `app.mjs` (with `applib/**.mjs|js`, at most 64 modules and 4 MiB in
all) from the live commit. A live commit with an invalid `fragment.json`
keeps the last good code and says why in `status.code.error`.

A fragment on a blessed template (`chat`, `agent`: docs/cloudflare-v1.md,
decision 40) names it in `fragment.json` (`template`) and runs the
platform release's manifest (with its own `meta` over the template's),
site, and code: the template's `app.mjs` and `applib/`, when it carries
any (a chat's push: docs/chat-records.md), held to an app's limits and
run in the same facet, under the release's identity (`blessed:<template>@
<release>`, a hash of the template's files,
`fragment_templates::blessed`). A platform deploy that changes the
template installs again at each such fragment's next request, a fresh
worker as a new commit is. Its repo holds only its face and data: a live
commit that declares operations, channels, triggers, `notifyUrls` or an
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
  A channel may also name who may post to it, `"post": <role>` (ROADMAP
  decision 18): the platform appends a poster's record itself
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
method), with `x-fragment-principal` and `x-fragment-role` set.

The app's database holds at most 16 MiB: a mutation that would leave it
larger rolls back and answers 507 `storage_full`; the app still reads,
and deleting rows makes room. A write from anywhere else (a query, the
app's `fetch`) meets the node's own stop, 4 MiB above. The app runs
without code generation from strings (`eval`, `new Function`) and
without `Atomics.wait`, and a turn whose heap grows past twice the
isolate's limit (128 MiB) ends with "Worker exceeded its memory limit"
(422). When the app's facet is at its concurrency limit (the Workers
runtime's), calls into it answer 503 `node_full` and the rest of the
fragment works (docs/hardening.md).

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

### Deliveries: web push and notifyUrls

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
there, or opens one. `fragment.notify.{supported, permission, ask,
show}` wrap the Notification API.

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

A job's AI steps bill the fragment's owner, on their ledger (Ledger,
below), capped when the run's principal is neither the owner nor an
agent of theirs (a run does not record whom an agent asked for). Text
goes through the platform's model route (Models, below) by tier; images
are FLUX.1 [schnell] (`@cf/black-forest-labs/flux-1-schnell`) on Workers
AI, on the model route's transport (the `AI` binding through the
deployment's AI Gateway), metered in neurons at Workers AI's price for
it: 4.80 a 512×512 tile and 9.60 a step (`fragment_core::media`).
Nothing holds a key: the binding is pre-authenticated.

Each paid step reserves its worst case on the owner's ledger before its
call (text: its request's bytes as tokens in and its tier's capped
`max_tokens` out; an image: a 1024×1024 image's 4 tiles at its steps),
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

- `job.ai.text({model?, prompt | messages, max_tokens?, reasoning_effort?})`
  → `{text, model, tier, usage}`: `model` is a tier, `cheap` (the default)
  or `medium` (`high` is refused: Models); `max_tokens` is at most 16384;
  `reasoning_effort` is GLM's, `low` (the default) or `high` (anything
  else is `low`, since GLM takes an unknown one as `max`).
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
id is never a tier.

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/models/v1/chat/completions[?fragment=<name>]` | an agent (`for` names whom it acts for) | an OpenAI chat completion, `model` a tier → the model's answer in OpenAI's shape: JSON, or with `stream: true` server-sent events, usage once on a last chunk with no choices |

What the model is sent is the body bounded: no `model` (the tier's),
`max_tokens` at most 16384, `reasoning_effort` clamped (above), usage
asked for when it streams, and none of the client's headers. The payer
is the agent's owner (decision 36); `fragment`, one the agent is a
member of (403 otherwise), is where the turn is: when its owner pays,
the call counts in its month and is under its cap for anyone but the
owner (or the owner's agent acting for them); when another person owns
it, that owner's ledger is asked first whether it is still open
(decision 26). Each call reserves its worst case (the body's bytes as
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
steps are the eight below, the files steps (`job.files.*`), `job.push`,
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
  seconds. Every fetch carries `x-fragment-hops`.
- `job.publish(channel, body, kind)`: a record, once per step.
- `job.sleep(ms | "N seconds|minutes|hours|days")`, up to 30 days.
- `job.agent({prompt, conversation?, channel?})` → `{text, turn}`: one
  turn of the fragment's own agent (A fragment's agent, below) for the
  run's principal; a triggered run's is the fragment itself, so the agent
  acts as its own member, an editor. `conversation` is a key the job
  chooses (`[A-Za-z0-9._-]{1,64}`, the principal's own; default one per
  run, `run-<run>`); `channel`, a channel editors may post to (the agent
  is one), where the turn posts its steps and answer (default none). It is two steps: `agent.start`
  starts the turn, named by the run and the step (not the attempt), so a
  retried or replayed step reattaches to the turn it started and never
  starts a second; then `agent.poll`, with sleeps of 2, 4, 8, 16, then
  30 seconds between, at most 40 times. A turn that fails or is stopped
  throws a `StepError`; the model calls are the owner's to pay.
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
timeout, a crash) is answered from what was kept, not performed again (a
fetch reaches its upstream once); a run whose answers are missing is
held. A step that fails for a reason that may
pass (an upstream 429 or 5xx, a timeout, a platform error) is retried 4
times with doubling delays; one that fails for good (a refused URL, an
unknown operation, a call that threw), or runs out of retries, makes the
`await` throw a `StepError` the job may catch; so does a step whose
arguments do not fit its kind (`job.ai.text` without a model, say), with
what does not fit. Every kind of step and its arguments are defined once,
as `Step` in `crates/core/src/steps.rs`. A job that throws is
**held**: its run keeps the input and error until someone replays it. At
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
| `__blob/{sha256}` | one of this fragment's blobs (Blobs, above), `GET` or `HEAD`, viewers and up (on a `public` fragment too: whoever holds only `public` is refused): its bytes as the type its upload declared (ranges answer 206), `Cache-Control: private, max-age=31536000, immutable`, `X-Content-Type-Options: nosniff`, and an `ETag` of the hash; another fragment's hash is 404. `PUT` uploads one from the fragment's own page, as `PUT /api/f/{name}/blobs/{sha256}` does (editors; the body streamed and hashed on the way, bytes that are not what the hash says 400; a declared `content-length`, at most 256 MiB, else 400 or 413) → `{ok, sha, size, stored}` (`stored: false`: it was there); its `content-type` is the type it is served as. As for any write on this host, only the page's own cookies count (`fetched`): another fragment's page uploads as no one (401) |
| `__members` | viewers and up (the share link too): `{members: [{principal, role, addedBy, addedAt, kind, owner?}]}`, the first added first, as `GET /api/f/{name}/members` answers it (a chat's agents, its lead the first agent added) |
| `POST __op/{op}` | a browser's call: `application/json` `{id, input}`; a signed-in browser (`fragment_site`) calls as its person; an unsigned caller gets an anonymous principal cookie; callers holding only `public` get 60 calls a minute each, 600 per fragment (a page's live views re-run over `__live`, outside this) |
| `POST __op/channels/{channel}` | a browser's post (`fragment.post`), through the call's door and its checks: `{id, input}` with the record's body as `input` → `{result: record, replayed}`, as `POST /api/f/{name}/channels/{channel}` answers it; a post spends the public budget as a call does (no operation name holds a `/`) |
| `__signin`, `__signout` | this origin's session (Sign-in, above) |
| `__fragment.js` | the browser library (below) |
| `__people?id=…&id=…` | anyone who can see the fragment: `{profiles: {<id>: {kind, username, picture, name?, fragment?}}}` for up to 64 identities (an agent's `username` is its owner's; a picture is a person's, an absolute platform URL; an agent made from an agent fragment, a computer's, has that `fragment` and its label as its `name`, which `@mentions` it); an id the registry does not hold is left out |
| `__files` | the files viewer, the platform's page (`__files.js`, `__files.css`): the content files (live and main) as a tree beside a reader (markdown with `[[wikilinks]]`, other text with line numbers, pictures, downloads), reading each through `__file`, following `__watch` where it may; asked for `application/json`, the list it reads, `{type: "files", count, files: [{path, size}]}` (a path on both is live's). Framed, the reader's bar asks the page around it to open a file as a pane (`postMessage({fragment: "open", url, title})`) |
| `__live` | WebSocket, anyone who can see the fragment: channel subscriptions from a cursor, presence, change signals, queries (below) |
| `__watch` | WebSocket, viewers and up (the share link, or a signed upgrade): `{type: "hello", ref, sha}`, then `{type: "changed", ref: "main", sha, paths}` per external move of main |
| anything else | the app's `fetch`, when it has one |

A site file carries an `ETag` that names its bytes: the last commit that
changed it, or, for a page given Open Graph tags, a weak tag that also
names the live commit (its `fragment.json`). `__fragment.js`, `__sw.js`,
`__files.js`, and `__files.css` carry a hash of their bytes. A `GET` or `HEAD` whose `If-None-Match` names the current
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
`crates/proto/src/live.rs`; the cell and the CLI decode through them. A
client asks for it as `__live?v=2`. A socket opened without `v=2` is a
page loaded before presence came one change a frame: it gets
`{type: "presence", list: [{id, principal, data}]}`, everyone sharing
presence, once after `hello` and on each change, and no change frames
(until no such page connects: docs/technical-debt-ledger.md). An
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

CLI: `fragment call <name> <op> --input '{...}' | @file | - [--id ID]`
(a file, or stdin, for an input over the 128 KiB one argument holds on
Linux), `fragment
post <name> <channel> --body '{...}' [--id ID]`, `fragment
channel <name> [<channel>] [--after N] [--follow]`.

### Moved hosts (docs/fragment-boats.md, slice 2)

fragment.club's fragments moved to `fragment.boats` (ROADMAP decision
23); the platform stayed on `fragment.club`. The router takes a host in
this order: the platform's own host (even under the suffix); a
fragment's host; the suffix's own name (`fragment.boats`), which
answers `308` to the same path and query on the platform, when the
platform is elsewhere; a fragment's old host
(`<label>--<username>.<FRAGMENT_LEGACY_HOST_SUFFIX>`), which answers a
GET or HEAD `308` to the same path and query on its new host, and
anything else there (a write, a socket) `410` `moved`, whose message
names that URL; any other name under either suffix, 404; anything else,
the platform. Neither redirect checks that the fragment exists, and both
answer `Cache-Control: no-store`, so the move can be undone. Old links
keep working through them (a share link's `?view=` rides along); a
browser's cookies and storage on the old host stay there, so each person
signs in once more on each fragment, and a page loaded before the move
has its calls refused until it is reloaded onto the new host.

## Agents (`agent/`, phase 5; co-hosted since phase 6)

A second script in the platform's fleet, with no ingress of its own: the
router authenticates `/api/agents` and `/api/a/*` like any signed
request and hands them on with the caller's identity
(`x-agent-principal`), which is all the script trusts; an inbox delivery
passes as it came. Agents act on fragments through the API above,
signing with their own keys. An agent's name is `<label>.<username>`,
its owner's (a bare label is one of the signer's own); making one also
registers it as its maker's, in the same request. Owner routes check the
caller is the agent's registered owner. Each agent's key is made in its
cell and kept sealed for it. It holds no model key: its model calls are
the platform's model route's (`POST /api/models/v1/chat/completions`,
signed by the agent, for the turn's asker, naming the turn's fragment:
Models, above), metered on its owner's ledger, and a turn its owner's
ledger refuses (no credit, a guest, a fragment's cap) fails saying why.
Its variables: `FRAGMENT_API` (the platform it acts on, and its models),
`AGENT_URL` (the base of the inboxes it hands out: the platform's),
`AGENT_TEST_HOOKS=allow` (dev and e2e only). The script reads a request
body of at most 64 KiB, measured as it arrives (413 before anything
else).

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/agents` | a person with a username | `{name, model? (a tier: "cheap", the default, or "medium"), instructions?}` → `{name, npub, model, id}`: made and registered as the caller's; again by its owner, the same answer (`replayed`); a name under someone else's username is 403; another model is 400 |
| `GET /api/a/{name}` | owner | → `{name, id, owner, npub, model, active, driving, outcome (running, idle, stopped, yielded, error), error, tokens, watchdogRestarts, conversation, asker, waiting: [{conversation, asker, at}], conversations: [{conversation, outcome, error, asker, at}], listens: {count, newest: [{fragment, channel, at}]}, ignored: [{fragment, channel, principal, at}], messages: [{id, role, text, tool_requests, tool_responses, steer, conversation}], steer, toolRuns, steps}` (each list its newest 256, oldest first but `conversations` and `listens`, newest first). `active`, `outcome`, and `error` are the running (or last) turn's, of any conversation; `conversation` and `asker` name it (`direct` is the owner's own conversation, a chat's is `<fragment>/<channel>`); `steer` holds the running turn's messages from its starter sent while it worked (a new turn drops those the model read); `ignored` notes anonymous messages, which start nothing |
| `GET /api/a/{name}/state?wait_ms=` | owner | → `AgentState` `{active, driving, outcome, error, answer}` (`crates/proto`) of the owner's own conversation: `active` while a turn of it runs or waits behind a chat's, `answer` its newest message when that is the model's text; answered once it is not active or `wait_ms` (0-25000, default 0) has passed: the read waits in the agent's cell, so a client waiting out a turn asks about every 25 s (`fragment agent say` does) |
| `POST /api/a/{name}/turns` | owner | `{text}` (at most 16 KiB) → `{started}`; during the owner's own turn, `{steered: true}` (read between steps); during another (a chat's), `{queued: true}`: it runs next, in the owner's conversation. At most 64 messages wait (429) |
| `POST /api/a/{name}/stop` | owner | → `{active, driving}`; a tool in flight is interrupted; the messages waiting run next |
| `GET /api/a/{name}/tools` | owner | → `{tools: ["platform__create_fragment", "platform__list_fragments", "platform__operations", "platform__call", "platform__list_files", "platform__read_file", "<fragment>__<op>", ...]}`: what the owner's own turn has (below) |
| `POST /api/a/{name}/listen` | owner | `{fragment, channel? ("chat"), reply? ("say")}` → `{fragment, channel, reply, subscription}`: the agent subscribes itself to the channel (it must be a member) with an inbox URL of its own (`AGENT_URL`); at most 500. `reply` answers a chat whose channel takes no posts (one made before phase 7); a postable channel is answered by a post. A new listen first drops those of fragments the agent is no longer in: of the fragments its memberships leave out, up to 16 are asked, and one that answers 404 or 403 loses its listens (so does one whose subscribe answers either, and a chat whose answer's post does). Listening again to the same fragment's channel is the same listen: its inbox URL, so the one subscription (made again if the fragment dropped it) |
| `POST /api/a/{name}/job` | owner (a fragment's job) | `{id, asker, conversation, channel?, text}` → `{turn}`: a turn of a fragment's own agent for `asker`, once per `id` (again: the same `turn`, `replayed`); it waits for a turn of its own and never steers another. Its conversation is `job:<asker>:<conversation>`, under `<fragment>/<channel>/` when it names a channel |
| `GET /api/a/{name}/job?turn=` | the same | → `{ended, outcome, text?, error?}`: `running` until it ends, then `idle` (answered, `text` its answer), `stopped`, `yielded`, or `error` |
| `PUT /api/a/{name}/scope` | owner (a fragment's deploy) | `{fragment, tools, instructions, model?}` → `{fragment, tools, model}`: a fragment's own agent takes what its block declares (A fragment's agent, below); 403 for any agent not made for that fragment |
| `POST /api/a/{name}/inbox/{token}` | the fragment's delivery (the token is the capability) | a `Delivery` (`crates/proto`), decoded whole: one that does not decode (a record without its `seq`, say) is 400. A message (a body with no `kind`, or `kind: "message"`: its `text`, else its JSON) from an identity starts a turn in the chat's conversation, acting for that identity; from the running turn's starter in its conversation, it steers that turn; any other waits for a turn of its own (429 past 64 waiting: the fragment delivers it again). `{kind: "stop", turn?}` from the running turn's starter, in its chat, naming that turn (or none), stops it; from anyone else, or another kind, it is ignored, never a message. The agent's own, one heard before (within a day: past the longest redelivery), and a message from an anonymous visitor (`anon:`) are ignored (the owner's view keeps the newest 32 anonymous ones). The turn's last answer goes back as the agent, with the id `rp:<40 hex of SHA-256 of its message id>`: posted to the channel as `{text, turn}` when it takes posts (`POST /api/f/{fragment}/channels/{channel}`), else `POST /api/f/{fragment}/ops/{reply}` `{text}`; an unknown token is 404 |
| `POST /api/a/{name}/test` | owner, test fleets | `{hold_in_tool_ms?, hold_after_tool_ms?, watchdog_ms?, window_messages? (2-256), view_rows? (2-256), model_timeout_ms? (200-100000)}` |

One conversation per chat: a turn belongs to the owner's own
conversation or to one chat's, reads only it, and answers there. One turn
runs at a time; a message for another conversation, or from anyone but
the running turn's starter, waits for a turn of its own, and so does a
steer the turn ended before reading (unless it was stopped). The driver
that ends a turn starts the next one waiting in the same step. A turn
records who started it (the owner, or the identity whose record it was),
and every call it makes on the platform acts for them (`for`, above);
the agent's own calls (listening, a chat's answer) name no one; its model
calls name the asker too, and its owner pays.

A turn's tools, read as it starts, are of two kinds. Per-operation
tools, for the turn's chat and the other fragments the agent is a member
of (less the other chats it follows; at most 16 fragments, 128 tools):
each fragment's operations, read with its status `for` the turn's asker,
those the role it answers may call, named `<fragment>__<op>` with the
operation's input schema, less each followed channel's reply operation
(the model is told its answer to a chat is posted for it, and a call to
a tool the turn does not offer is answered with an error, so the turn
goes on to its answer). And the platform's verbs, for every fragment the
asker reaches: `platform__list_fragments` (`GET /api/fragments?for=`),
`platform__operations` (`{fragment}` → its operations and the role the
turn acts with there), `platform__call` (`{fragment, operation, input}`),
`platform__list_files`, `platform__read_file`, and, in its owner's turns
only, `platform__create_fragment` (from a template, as a person makes
one: the fragment is its owner's, the agent an editor). The agent writes
no fragment's files and deploys none. A call is `POST
/api/f/<fragment>/ops/<op>?for=<asker>` signed by the agent with the id
`tc:<40 hex of SHA-256 of the tool-call id>`: a replayed call replays
the operation. A job's call (either kind of tool) answers when its run
ends, reading it `for` the asker every second for up to 90 s: `{run,
status, output}` (succeeded) or `{run, status, error}` (held, blocked),
else `{run, status, note}` (still going). At most 64 steps a turn. Each step sends the model a window
of its conversation, not all of it: the newest 256 messages, cut to
start at a turn's first message (so a tool call and its result stay
together), with the running turn whole and earlier turns while they
total 256 KiB. The earlier turns' tool results are sent cut to their
first 400 characters and a note of how many more were cut (their images
to a note), so old output cannot push a model call past its deadline;
the stored conversation keeps them whole. A turn that alone outgrows the
window ends in an error; the next message starts a turn that fits. A chat turn's answer is its
last message, when that is the model's text.

Every turn tells the model, after the agent's instructions, that its
answer to a chat is posted for it, and that it acts for the person who
asked, reaching only what they may (`TURN_NOTES` in `agent/src/lib.rs`).
An agent made with a default instruction of any age (they all open
alike) is told today's.

A model call (`agent/src/model.rs`) asks for at most 4096 tokens and has
100 s, counted to the answer's last byte, so streaming does not stretch
it; its answer is read whole before the turn sees it. A call past its
deadline, one that fails, or one that answers nothing (no text and no
tool call: reasoning alone is nothing) is made once more, told why when
that helps; one the platform refuses for good (401, 402 for its owner's
ledger, 403 for a guest) is not, and the turn says why. Nothing twice,
after tool calls that worked in the turn, is answered with what those
calls did ("Done. Here is what I did: …"). A call in a reply cut off at
its limit (`finish_reason: length`) is refused, saying so (goose's
parse), and the model tries again. A turn that fails (a
second failed call, or any other error) is never silent: "I couldn't
finish: <why>. Ask me to try again." is its answer, stored in its
conversation and posted to its chat as an answer is, and the chat's
`turn.end` carries the error.

### A fragment's agent (the `agent` block)

A fragment may declare an agent people talk to through one of its
channels, in `fragment.json` (checked at deploy like the rest of it):

```json
"agent": { "instructions": "agent.md", "tools": ["log_food", "today"], "channel": "ask", "model": "cheap" }
```

`channel` is a channel it declares with a `post` role; `instructions` a
file of its repo, read at live (at most 8 KiB: one missing, empty, or
larger is refused as an invalid manifest is, live's code not installed
and `code.error` saying why); `tools` operations it declares, none
owner-only; `model` is optional, a tier (`cheap`, the default, or `medium`).

A deploy whose live manifest declares one makes it so as it lands (the
refresh, webhook, or deploy that moved live answers once the agent has
joined), each part idempotent (the alarm retries one that did not
finish, an `agent.join-failed` event, within the poll interval). The
agent hears its channel from the deploy that declared that channel: as
it joins, the records posted there since, before its subscription began
(the newest 32), are delivered to it once, so a message sent before it
listened is still answered; nothing posted before that deploy is, and a
later deploy catches up on nothing. The fragment's
own agent is named as the fragment is, made its owner's on first need
(`POST /api/agents` with a `scope`, which only a deploy names), and given
what the block declares (`PUT /api/a/{name}/scope {fragment, tools,
instructions, model}`, owner, for the fragment it was made for only); it
is an editor of this fragment, a member of nothing else, listening to
`channel`. A deploy that drops the block, or names another agent or
channel, removes the one before (its membership and its subscription).
Each change is an event (`agent.joined`, `agent.left`). An agent of that
name made otherwise is not taken over.

A fragment's own agent differs from a person's three ways. Its tools are
the block's operations, read with the fragment's status `for` the asker
and offered as their role may call them: no other fragment, no platform
verb. It keeps one conversation per person who posts
(`<fragment>/<channel>/<identity>`), so strangers never share one. And
it is told, after its instructions, that its answer is posted for it and
each call acts as the asker. A signed-in person's post
to the channel starts a turn for them (an anonymous one starts nothing);
each call acts for them (`for`: the lower of their role and the agent's,
and the app's `call.principal` is them). On its own fragment, the
fragment's own agent gives a signed-in asker what that fragment's
visibility gives anyone who reached it, its link: on a `link` or
`public` fragment they are at least a viewer (their post on its channel
took that), a membership above it still wins, and a `members` fragment
gives nothing more; never a person's agent, nor another fragment; the answer goes to the channel as `{text,
turn}` and the steps to `work` when the fragment declares it postable,
a chat's records (below). Its model calls are its owner's to
pay.

## Connections (decision 22)

A person's accounts at the providers the deployment offers
(`FRAGMENT_CONNECTIONS`), held and refreshed by WorkOS Pipes; their
agents use them through the computer's swap (docs/computers.md).

| method & path | who | body → answer |
| --- | --- | --- |
| `GET /api/connections` | a person | → `{connections: [{provider, status}]}`: each offered provider, `connected`, `expired` (connect it again) or `none` |
| `POST /api/connections/{provider}/authorize` | a person | → `{provider, url}`: Pipes' consent, for the person's browser; followed, the account is connected. A provider not offered is 404 |

## The shell (phase 5)

The platform's one page is `/`, and `/settings` (cell/shell/, its files at
`/__shell/<file>`): its script reads the path, opening its settings at
`/settings` and the person's chats at `/`, and puts the view it shows in
the address, so a reload stays put. Its settings hold the person's
account (username, sign-ins, identity id, picture, `/auth/link` to add
another sign-in, a POST to `/auth/logout`), their credit and what their
standing stops, their computer and agents, connections, and pairing the
CLI (the one-line install, `fragment login`, and `fragment skill` for a
coding agent); its sidebar lists their fragments, each one's share sheet
(`/share/<name>`) in a dialog, and the catalog makes an app from a
template. It calls the API with the person's platform session
rather than a key: a request on the platform's host is taken as the
signed-in person when it carries `x-fragment-shell: 1` and
`Sec-Fetch-Site: same-origin`, and, writing, the platform's exact
`Origin`. Anything else needs a signature as before. Adding a key still
needs a key.

Its sidebar is the person's list: chats (kind `chat`), then apps, less
the ones they archived (`PUT /api/fragments/{name}/archived`, above).
A chat with two agents or more is a **group** (decision 8): "New group
chat" makes a chat fragment on the `chat` template, titled by the name
given or else its agents' names, and adds the agents picked as editors
one at a time in the order picked, so the first is its lead (the first
added, by `addedAt`). Which agents a chat has is its member list
(`GET /api/f/{name}/members`), never its name; the sidebar stacks their
avatars, each in its identity's colour (the chat page's FNV-1a choice).

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
  name a role: search sees only what the person can see now.
- **Limits.** A list keeps a fragment's newest 10 000 entries
  (`SEARCH_ENTRIES_PER_FRAGMENT_MAX`, as many as a postable channel keeps
  records) and 100 000 in all (`SEARCH_ENTRIES_MAX`), the oldest going
  first. A search answers at most 20 fragments and 50 messages, each with
  a snippet of at most 300 bytes of plain text around its words.
- **A query is words.** Each word (split at spaces; one with no letter or
  digit is dropped) must be in the message, as a word or a word's start,
  ignoring case and accents. FTS5's syntax is never read: `OR`, `NOT`,
  `NEAR(…)`, `column:`, `*`, `^` and quotes are text like any other.

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
| `GET /api/computers/{id}` | its owner | → `ComputerView`: `phase` is `asleep`, `starting`, `awake`, `sleeping`, or `wont_wake` (its starts kept failing; `why` says why); anyone else 404 |
| `POST /api/computers/{id}/wake` | its owner | → the view once it is awake (a wake also lifts `wont_wake`); 503 `wont_wake` when it would not start |
| `POST /api/computers/{id}/sleep` | its owner | → the view once it is asleep: `/data` saved, the guest signalled, the container gone |
| `PUT /api/computers/{id}/image` | its owner | `{image}` → the view: the image it starts from at its next wake (an upgrade, or a rollback), its `/data` restored; an image the deployment lacks is 400 |
| `PUT /api/computers/{id}/agents/{fragment}` | the owner of both | → the view: the agent fragment runs on it. The fragment's own key becomes the agent's identity (registered to its owner), an editor of its own fragment; it signs the guest's requests only while it is assigned here. Assigning it again changes nothing. Nothing restarts: an awake computer's guest reads its agents again while it runs and runs the new one (our Hermes image within seconds; docs/computers.md); a sleeping one's reads it as it starts |
| `DELETE /api/computers/{id}/agents/{fragment}` | the same | → the view: it signs nothing for the guest from now on; an awake guest stops running it as it reads its agents again |
| `PUT /api/computers/{id}/agents/{fragment}/connections` | its owner | `{connections: [provider] \| null}` → the view: the WorkOS Pipes connections the agent may have swapped in (decision 22), all named at once. `null`, the default, is every connection its owner has (decision 44: a person's agents are not fenced from each other); a list narrows the agent to those. A provider the deployment does not offer (`FRAGMENT_CONNECTIONS`) is 400, as is a body without `connections` |
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
its owner. Records its own agents post wake nothing. An agent added to a
fragment (a member's `PUT`, an invite it accepts, a fragment it makes
for its owner) needs nothing more from whoever added it: the platform
posts `{kind: "joined", fragment}` on the agent fragment's `tasks`, as
that fragment, once for the membership, and wakes the computer (Paul,
2026-10-03; docs/computers.md). The fragment it joined keeps the notice
until the computer has it (`agent.told`, or `agent.untold` with why).

A guest's request to a connection's or an operator key's host has the
placeholders in its headers swapped (docs/computers.md). The swap's
refusals reach the guest as the platform's: 403 `forbidden` (an agent
its owner narrowed to connections without that one, or a placeholder
sent to a host that is not its credential's), 403 `not_connected` (its owner has not connected
that provider, or must connect it again), 401 (no `x-fragment-agent`).

### A chat's records (phase 7, slice C)

docs/chat-records.md extends this for computers' agents (phase 4: turns,
drafts, prompts, attachments, Stop, hand-offs, routines) and wins where
they differ.

A chat is two channels (and, on the blessed template, its push job:
docs/chat-records.md), and an agent answering it (one that listens
there, or the fragment's own: the `agent` block, above):

```json
"channels": {
  "chat": { "read": "public", "post": "viewer" },
  "work": { "read": "viewer", "post": "editor" }
}
```

- `chat`: messages, `{text}`, posted by viewers and up (link holders
  too; `fragment.post("chat", {text})`); an agent's answer, `{text,
  turn}`; and `{kind: "stop", turn}`, the page's Stop, which the agent
  acts on only from the turn's starter. A body of another `kind` is for
  pages, never a message.
- `work`: an agent's progress, for turns a chat started, each posted by
  the agent (best-effort: a failed post never fails the turn) with the id
  `wk:<turn>:<part>` (`start`, the call's number, `end`), so a replayed
  step posts the same record and nothing new:
  - `{kind: "turn.start", turn, asker}`: who asked (only they may steer
    or stop it);
  - `{kind: "turn.step", turn, step, tool, args, ok, excerpt, text?}`: one
    per tool call, once its result is stored, numbered from 1 in the
    order the model asked: the tool's name, its arguments as one line of
    JSON (at most 140 characters), whether it worked, at most 300
    characters of its result, and the model's text before the call (at
    most 300) on the first call of a message. A result can hold what the
    asker reaches in other fragments (decision 1: they could read it
    anyway), so only this excerpt is posted;
  - `{kind: "turn.end", turn, outcome, error?}`: `idle` (answered),
    `stopped`, `yielded`, or `error` (at most 300 characters of it).

  `turn` is 24 hex of the SHA-256 of the turn's first message's id; the
  answer on `chat` names it, so a page places the steps above their
  answer. Nothing streams: a record is a whole step.

A chat made before this (a `say` operation, `chat` taking no posts) keeps
its own page, and its agent answers through `say`, with no `work`.
