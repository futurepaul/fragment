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
| `FRAGMENT_HOST_SUFFIX` | fragments are served from `<label>--<username>.<suffix>` (any other name under it is 404, never the platform; the suffix's own name is the platform's, or redirects to it: Moved hosts, below); unset, from `/f/<name>/` |
| `FRAGMENT_LEGACY_HOST_SUFFIX` | where fragments were served before the suffix moved: a fragment's host under it redirects to its host under the suffix (Moved hosts, below); counted only beside a different suffix. fragment.club's is `fragment.club`, its suffix `fragment.boats` |
| `FRAGMENT_POLL_INTERVAL_S` | the webhook backstop (default 300), and how often running runs are checked against their Workflows, for a busy fragment: one something outside the platform may have written in the last day (a storage token was minted for it, or a webhook arrived), or with a run in flight, a held run's video to settle, or a template or an owner's agent still to land. Any other fragment is polled once a day |
| `FRAGMENT_JOB_RETRY_DELAY_S` | a failed job step's first retry delay, doubling over 4 retries (default 10) |
| `FRAGMENT_EGRESS_LOCAL` | `allow` lets jobs fetch loopback and private addresses (dev and e2e fakes); never on a shared fleet |
| `FRAGMENT_BLOB_GRACE_S` | how long a blob no branch names is kept before it is deleted (default 7 days) |
| `FRAGMENT_PUSH_SUBJECT` | who push services may contact about this fleet's pushes (a `mailto:` or https URL; RFC 8292) |
| `FRAGMENT_DELIVERY_RETRY_S` | the shortest wait before a delivery is retried (default 10; the wait grows with the delivery's age, up to an hour) |
| `FRAGMENT_DELIVERY_RETRY_MAX_S` | the longest (default an hour, never under the shortest; test fleets set both, for a fixed pace) |
| `OPENROUTER_API_URL` | where AI calls go (default https://openrouter.ai) |
| `FRAGMENT_BUDGET_USD` | each person's monthly budget (default 20) |
| `FRAGMENT_OPERATORS` | identities and keys that may top up budgets (as `FRAGMENT_CREATORS` once read them) |
| `FRAGMENT_DEPLOY_ID` | which deployment this is (`cargo xtask deploy` sets it); `GET /healthz` answers it in `x-fragment-deploy` |
| `WORKOS_CLIENT_ID` | sign-in: fragment's WorkOS environment; unset, sign-in answers 500 |
| `WORKOS_API_URL` | where WorkOS is (default https://api.workos.com; dev and the e2e: the fake) |
| `FRAGMENT_PLATFORM_URL` | the platform's origin, where sign-in and the platform session live (default: the hostname suffix itself; fragment.club's is https://fragment.club, on no fragment's domain) |
| `FRAGMENT_SIGNINS_PENDING_MAX` | sign-ins begun and not finished that the registry keeps (default 100000; at least 1): a sign-in is kept through this many later starts, so the oldest is let go only past this many starts in its ten minutes (Sign-in, below) |
| `FRAGMENT_TEST_HOOKS` | `allow` on dev and e2e fleets only: `POST /api/test/registry {down}` makes the registry answer 503 (until it is set back, or the registry restarts), `{calls: null}` answers `{calls}`, how many calls the registry has had since it started (a test counts a request's round trips by the difference), `{hold: ms}` makes its next call wait that long (at most 10 s) before it is answered, while other calls go on, and `{signins: "count"\|"expire"\|"sweep"\|{expireSession: token}}` counts sign-in's rows (`{logins, redemptions, sessions}`), expires every pending sign-in and unspent redemption, runs its sweep now, or expires the one session a cookie's token names (a platform session's site sessions end with it); `GET /api/test/env` answers the Worker variables; `POST /api/test/keys {fragment, op, plaintext\|sealed}` seals or opens through `KEYS` as that fragment; `POST /api/test/fragment {fragment, op, …}` pulls a lever on that fragment: `fail-deliveries {times}` fails its next queue sends, `fail-outbox {times}` fails its next records' outbox writes just after their append, `fail-triggers {times}` fails its next trigger steps just before their last run starts, `drop-effects {times}` loses its next job step answers on their way back to the Workflow (after the step ran and its answer was kept), `forget-steps` forgets the kept answers of its runs in flight, `hold-advances {on}` holds each advance after a run's first step while on (at most 20 s), and `advance-held` answers `{run}`, the last run it held, `forget-live` makes it forget what it knows of its live sockets beyond their attachments (as waking from hibernation does), `age-live {ms}` makes every live socket's identity check `ms` older (as if that long had passed), `drop-live {code}` drops its live sockets, `ledger {ms \| null}` shortens (or restores) its operation ledger's window, `age {ms}` forgets its write keys as if `ms` had passed, `members {fill}` adds placeholder members until there are `fill`, `code-builds` answers `{builds}`: how many times the fragment's activation built its app's worker code for the loader, `alarm` answers `{alarmAt, pollAt, now}` (ms): when its alarm and its next poll are set for, and `age-outside {ms}` makes the last sign of an outside writer (a storage token, a webhook) `ms` older |

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

A principal is an identity (`id:` + 32 hex: a person, an agent, or a
computer) or an anonymous visitor (`anon:` + 32 hex, the hash of a random cookie on the
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
with `DELETE members/me`; invites; visibility; rotation; grants;
deletion) are 403 for an agent whatever it names. A site request's query is its app's:
`for` there means nothing to the platform.

A computer (Identities, below) acts as itself only: it holds the role
its own membership gives it, like anyone (decision 17's cap is an
agent's, for an asker it does not have), and never names `for` (403).
Owner-only actions are 403 for a computer as for an agent. Its owner
reads what it reads, as an agent's owner does. What it makes is its
owner's (`POST /api/fragments`).

Membership is cell state: `fragment.json`'s `visibility`, `editors`, and
`viewers` grant nothing (the cell records a `manifest.ignored` event).
Only the owner manages members, invites, visibility, and tokens; a
member may leave. Each identity's list of fragments is kept in its
`Principal` cell, fed from each fragment's outbox.

## Identities (phase 4 slice A)

The registry (`cell/src/registry.rs`; finite.computer's BANKS stands
behind the same routes later) holds identities, the public keys each has
held, each agent's and computer's owner, each computer's name, and, from
slice B, sign-in subjects.

A **computer** is a machine a person paired as theirs (`fragment login
--computer <name>`, approved on `/cli`: Sign-in, below): an identity of
its own, owned as an agent is, whose key signs as the computer and never
as the person. It acts only where it is a member, and in what it makes,
which is its owner's. It adds no keys (its owner manages them, as an
agent's), registers or makes no agents, chooses no username, and reads no
budget (403). A person owns at most 32; names are labels, one per name
among a person's computers. It holds
no grant and no private key. A **key proof** is a NIP-98 event by a new
key for the same method and URL as the request that carries it, with
`["p", <the signing key, 64 hex>]`: whoever sent the request also holds
the new key and meant it for this signer.

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/identities` | a person | `{kind: "agent", proof}` → a new agent identity they own, holding the proof's key (FIN-11's trusted initial registration); again, the same one; a key someone else holds is 409; an agent owns no agents (403) |
| `GET /api/identities/{id\|me}` | the identity, or its owner | → `{id, kind, owner?, name?, createdAt, keys: [{npub, addedAt, addedBy, revokedAt?}], agents: [id], computers: [{id, name, pairedAt}], subjects: [{issuer, email, linkedAt}]}` (`name`: a computer's; `computers`: a person's, removed ones left out); anyone else 404 |
| `POST /api/identities/{id\|me}/keys` | a person for themselves; an owner for their agent or computer | `{proof}` → the identity with the key added (at most 64 keys, revoked ones included); a key someone else holds, or a revoked one, is 409; a removed computer's, 409 |
| `DELETE /api/identities/{id\|me}/keys/{npub}` | the same | → the identity; the key is 401 from the next request and never comes back; an agent's or computer's last active key cannot be revoked (400); a person who signs in may hold none |
| `DELETE /api/identities/{id}` | a computer's owner | → `{id, removed, left: [fragment], failed: [{fragment, status\|error}]}`: every key it holds is revoked at once (401 from its next request), its name is free, and it leaves every fragment its list names, closing its sockets; again, `removed: false` and the leaves retried; another's computer 404, an agent 400 (`fragment computers rm`) |
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
| `GET /` | the home. Signed out: a link to sign in. Signed in without a username: choosing one. Then: who is signed in, what is left of this month's AI budget, their fragments (each one's link; theirs or shared with them, as what; on their own, who may open it; its share sheet, `/share/<name>`), the "new fragment" form (blank, todo, inbox, chat, then the desktop, a demo), pairing a CLI (the one-line install, `fragment login`, and `fragment skill` for a coding agent), their picture, signing out, and linking another sign-in |
| `POST /auth/new` | the form (`label`, `template`): makes `<label>.<username>` from the template, then → `/auth/fragment?name=…&return=/` (signed in on its origin, and there); a refusal is a 400 page saying why; another origin 403. For a template whose `fragment.json` asks for `frame` (the desktop), the form says it will show the person's fragments inside it, signed in as them, and making it there is their frame grant (`PUT /api/f/{name}/grants/frame`, the same event) |
| `GET /auth/login?return=&login_hint=` | → WorkOS's authorize URL (`provider=authkit`, `redirect_uri` `<platform>/auth/callback`, a state); the state is bound to the browser by `fragment_login` (HttpOnly, SameSite=Lax, `Path=/`, ten minutes) |
| `GET /auth/link?return=` | the same from a signed-in browser: the sign-in that comes back joins this person (409 when it is someone else's) |
| `GET /auth/callback?code=&state=` | the state must match the browser's cookie (400 otherwise); the code is exchanged server-side; → `fragment_session` (HttpOnly, SameSite=Lax, `Path=/`) and back to `return`; a WorkOS `error` is shown (400); a sign-in already finished or past its ten minutes, or a code WorkOS refuses (a callback sent again), is 400 `invalid_request` |
| `POST /auth/logout` | ends the session and every fragment session made from it, clears the cookie, and sends the browser to WorkOS's logout (`session_id` from the access token's `sid`); from another origin, 403 (`GET` shows the button) |
| `GET /auth/fragment?name=&return=` | signed in: → `<fragment origin>/__signin?token=<a single-use redemption, 60 s, for that fragment only>` (a session holds at most 16 unspent; past that, the oldest is refused), at once for a fragment of the person's own, one shared with them, or one they said yes to; for any other, first a page asking "Continue to X?" (Asking first, below); signed out: → sign in first |
| `POST /auth/fragment?name=&return=` | that page's form (`form`, its token): the yes, remembered, then → the fragment's `__signin?token=` (303); another origin, or a missing or stale token, 403 |
| `GET /cli?key=<npub>&proof=` | the link `fragment login` prints: `proof` is the key's own NIP-98 event for `POST <platform>/cli/approve`, good for ten minutes (the proof of possession; without it, stale, or by another key: 400). Signed in: a page showing the key's last eight characters, to compare with the terminal, and an Add button; signed out: → sign in first, keeping the link |
| `GET /cli?key=&proof=&computer=<name>` | the link `fragment login --computer <name>` prints: the proof is for `POST <platform>/cli/approve?computer=<name>`, so it pairs that computer and nothing else (the same key without the name, or under another, is 400). The page says it is a computer named `<name>`, owned by the person, acting only in the fragments they add it to and those it makes (theirs, on their budget), never as them; and a Pair button |
| `POST /cli/approve` | the page's form (`key`, `proof`, `computer`): the key joins the signed-in person at once, or, with `computer`, becomes their new computer of that name (again, the same one; a name they already use is 409); a key someone else holds, or a revoked one, is 409; another origin 403; the CLI waits for `GET /api/identities/me` to answer. People themselves come only from sign-in (`POST /api/identities {kind: "person"}` is 400), and computers from this page (`{kind: "computer"}` is 400) |

On fragment.club the platform is cross-site from every fragment
(`fragment.club` and `<label>--<username>.fragment.boats`), so its
SameSite=Lax session cookie reaches a fragment's page only on a
top-level visit. A fleet whose platform shares the fragments' domain
(`FRAGMENT_PLATFORM_URL` unset) puts them on one site, where the cookie
rides along on a fragment page's form, fetch, or frame. Either way every
page here answers `Content-Security-Policy:
frame-ancestors 'none'` and `X-Frame-Options: DENY` (no page may frame
one and lay its button under a click; its redirects still run in a
frame, but a frame's `__signin` refuses what they mint) and `Cross-Origin-Opener-Policy:
same-origin` (a page that opens one in a window of its own is severed
from it: its handle reads `closed`, and can neither navigate nor message
it), and every form here (`/auth/new`, `/auth/username`, `/auth/picture`,
`/auth/logout`, `/auth/fragment`, `/cli/approve`, and Sharing's below) is 403 from another
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
`/f/<name>/`). A frame redemption (`__frame`, below) is redeemed only in
a frame, and any other only outside one: shown to the other kind of
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
navigation is answered `Content-Security-Policy: frame-ancestors
<origin>` naming the page its frame session was made for, or
`frame-ancestors 'self'` without one, and `Cache-Control: private,
no-cache`: a fragment shows signed in only in the page that framed it
through `__frame`, and in another fragment's page not at all. A signed
request (the CLI's, an agent's) carries no cookies and is unchanged.

### Frames (`__frame`)

A page shows its owner's fragments in frames, signed in, only through
`__frame` (docs/fragment-boats.md, decision 2):

- `GET <page>/__frame?name=<fragment>&return=<path>` is served on the
  framing fragment's own origin, only as a frame of its own page
  (`Sec-Fetch-Dest: iframe`, `Sec-Fetch-Site: same-origin`; a fetch, a
  tab, or another page's frame is 403);
- only when its live `fragment.json` declares `"capabilities":
  ["frame"]` and its owner allows it (`PUT /api/f/{name}/grants/frame`,
  the share sheet's "Your fragments inside it", or the new-fragment form
  that made it; else 403, saying which), for its owner signed in there
  (else 401), and for a fragment in the owner's list (else 403);
- the registry mints a frame redemption from that origin's own session
  (single-use, 60 s, for `name` only, bound to the framing page's
  origin) and it answers `302` to `<fragment>/__signin?token=`
  (`no-store`, `Referrer-Policy: no-referrer`): the page's code never
  holds it;
- there it becomes `fragment_frame` (`HttpOnly; Secure; SameSite=None;
  Partitioned`, `__Host-` over https): kept by browsers that block
  third-party cookies (CHIPS), in the framing page's partition only. The
  frame then goes to `__signin?check=frame&return=`: with the cookie
  kept, on to `return`; without it, the page offering the fragment in a
  tab of its own.

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
`?view=` that opened nothing), and `__frame`'s reason. From a frame its
links open a tab. An API call, a fetch, an operation, a socket, and any
request whose `Accept` names no HTML keep the JSON error. Only the
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
| `GET /share/<name>` | the share sheet: who is in (usernames and pictures, from the registry's profiles) and their roles, to any member (anyone else, a 403 page); for the owner, inviting by username, the pending invites (revoke), each member's role and removing them, who can open it (`members`, `link`, `public`), and the share link (copy; a new one). Signed out: → sign in first, and back. It reads nothing from its URL |
| `POST /share/<name>` | the sheet's form: `form` (the page's token), `action`, and its fields: `invite` (`username`, `role`: an invite for them alone, one use, seven days; answers the sheet with the `/join` link to send them), `role` (`member`, `role`), `remove` (`member`), `uninvite` (`invite`: its id), `visibility` (`visibility`), `rotate` (the share link only; the inbox's token and the webhook's secret are the CLI's). Done: → 303 back to the sheet; refused by the fragment (a member who is not the owner: 403): the sheet, saying why, with the refusal's status |
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
in it. Both pages send no CORS headers (a fragment's page cannot read
them, so it never holds a form's token), refuse every frame, sever their
opener, allow scripts and styles only inline and images only from the
platform (`Content-Security-Policy`), and keep their URL to the platform
(`Referrer-Policy: same-origin`: the join page's holds its invite).

## Control API

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/fragments` | a person with a username; an agent or a computer for its owner (the fragment is the owner's, under their username, on their budget, with its maker an editor) | `{name, visibility?, template?}`: `name` a label, or `<label>.<your username>` → `{name, npub, owner, visibility, viewToken, inboxToken, webhookSecret, repo, canonical}` (`name` in full). `visibility` defaults to `link`, and to `members` for the `desktop` template (a desktop is its owner's alone). The fragment's own key is made by the node's `KEYS` and stays sealed there. The cell creates (or, for a name deleted before, finds) the code.storage repo. With `template` (`desktop`, `chat`, `todo`, `inbox`, `blank`; any other is 400 and nothing is made), the template's files are main's first commit (its `fragment.json` stamped with the fragment's name) and live at once; one that fails to land is retried by the fragment's alarm (`template.failed` events). `notes` is the CLI's only (`fragment new --template notes`). |
| `GET /api/fragments` | any signer | → `{fragments: [{name, role, sharing?}]}`; `sharing` on the signer's own fragments only: `{visibility, members, guests}` (guests: members who are neither the owner nor an agent of theirs), as the fragment last sent it with a change to its members or visibility (a fragment from before sends it once, on its next change or alarm; until then it has none); an agent's `?for=<id>`: the fragments that identity holds a role on where the agent or its owner is a member too, each with the role the agent acts with there for it (`fragment_core::access::listed_role`; a call decides again) |
| `DELETE /api/f/{name}` | owner | → `{ok, deleted}`; the app's database goes too; the repo stays |
| `GET /api/f/{name}/status` | viewer | → `{name, npub, owner, role, visibility, repo, pins: {main, live}, counts: {files, events, members}, code: {sha, operations, error}, viewToken, inboxToken (editor), urls: {canonical}, blobMinBytes, frame?}` (`frame`: when live's `fragment.json` asks for it, whether its owner allows it) |
| `GET /api/f/{name}/manifest` | viewer | → `fragment.json` at main (404 when there is none) |
| `GET /api/f/{name}/members` | viewer | → `{members: [{principal, role, addedBy, addedAt, kind, owner?}]}` (`owner`: an agent member's) |
| `PUT /api/f/{name}/members/{id\|npub}` | owner | `{role: viewer\|editor}` → the member; a key names the identity holding it (404 when no one registered it) |
| `DELETE /api/f/{name}/members/{id\|npub\|me}` | owner, or the member | → `{ok, removed}`; closes that member's change feeds (and its owner's, when an agent's membership was their only view) |
| `POST /api/f/{name}/invites` | owner | `{role, uses? (1), ttlS? (7 days, at most 30), invitee? (id:…)}` → `{id, role, usesLeft, expiresAt, createdBy, invitee?, token}`; the token is shown once. With `invitee`, only that identity may accept it (the share sheet's invite by username); without, whoever holds the token |
| `GET /api/f/{name}/invites` | owner | → `{invites: [...]}` without tokens |
| `DELETE /api/f/{name}/invites/{id}` | owner | → `{ok, revoked}` |
| `POST /api/f/{name}/join` | any signer | `{token}` → `{name, role, joined}`; a stronger existing role is kept; a fragment at its 1000 members is 400, and the invite keeps its use; an invite for another identity is 403, and keeps its use |
| `POST /api/f/{name}/join/preview` | any signer | `{token}` → `{name, role, invitedBy, invitee, expiresAt, current}`: what joining would grant (`current`: the signer's role now), joining no one; 404 for a token that names no open invite (the platform's `/join` page shows it) |
| `PUT /api/f/{name}/visibility` | owner | `{visibility}` → `{ok, visibility}` |
| `PUT /api/f/{name}/grants/frame` | owner | `{granted}` → `{frame}`: lets the fragment show its owner's fragments inside its page (`__frame`) while its live `fragment.json` asks for `frame`, or stops it (a `grant.frame` event); the share sheet's "Your fragments inside it", and the new-fragment form's (`POST /auth/new`). An allow holds until stopped, whatever its code becomes. No other create grants it (`POST /api/fragments`, `__fragments`) |
| `POST /api/f/{name}/rotate` | owner | `{scopes?: [inbox, view, webhook]}` → `{inboxToken, viewToken, webhookSecret, rotated}` (`Rotated`): every token as it is now, and the scopes renewed; a new view token closes link holders' feeds |
| `PUT /api/f/{name}/secrets/{KEY}` | editor | raw body (at most 64 KiB) → `{ok, name}`; sealed (AES-256-GCM, key HKDF'd from the host secret and the fragment's npub) |
| `GET /api/f/{name}/secrets` | editor | → `{names}`; values never leave |
| `DELETE /api/f/{name}/secrets/{KEY}` | editor | → `{ok, removed}` |
| `GET /api/f/{name}/storage-token` | editor | → `{token, repo, api, expiresAt}`: ES256, this repo, `git:read`+`git:write`, 15 minutes |
| `POST /api/f/{name}/refresh` | editor | → `{ok, refs: {main: {pin, moved} \| {absent}, live: ...}}`. A fragment reads the branches it has no pin for once, on its first request (a push may predate its webhook); after that a move arrives by the webhook, this, or the poll backstop, and a site with nothing deployed answers 404 without asking code.storage |
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
| `GET /api/f/{name}/channels` | viewer | → `{channels: [{name, read, post, seq}]}`: `events`, `ops`, `inbox`, and the app's (`post`: who may post, or null) |
| `GET /api/f/{name}/channels/{channel}?after=&limit=` | the channel's reader | → `{channel, records: [{channel, seq, at, principal, kind, body}], next}` (1000 a page) |
| `POST /api/f/{name}/channels/{channel}` | the channel's `post` role | `{id, body}` → `{record, replayed}` (`Posted`): the platform appends `body` (any JSON, at most 64 KiB of it; 413) as a record of kind `message` naming the poster, with no app code; it reaches sockets, subscriptions, and the channel's triggers as a mutation's record does. The same id and body again answer that record and append nothing (a retry also finishes what the first try left: its deliveries, its triggers' runs); the same id with another body, or on another channel, is 409 (ids are the poster's, kept as long as the record). A channel without a `post` role, and `events`, `ops`, and `inbox`, refuse posts (403). A poster holding only `public` spends a public call (Serving, `__op`) |

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
  only to the fragment's owner viewing it. There are two: `"fragments"`
  (`__fragments`, Serving) and `"frame"` (`__frame`, Sign-in), which the
  platform honors only once the owner allows it too (the share sheet, or
  the new-fragment form that made it).
  Any other name is refused at deploy.
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
  refused at deploy (whoever may post may read). Such a channel keeps its
  newest 10 000 records (`limits::POSTED_KEPT`; the oldest go, whoever
  appended them, and with them their posts' ids), as `events` and `ops`
  keep theirs; the number is not declarable yet.
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
`anon:…`, or the fragment's own npub for its triggered runs; for an
agent's call `for` someone, them, with the agent in `call.agent`, else
null) and `call.role`. A mutation is synchronous over the app's own
SQLite; `call.publish(channel, body, kind = "message")` appends a record
(body at most 64 KiB, 64 per mutation) once the mutation commits, and an
exception rolls back its writes and its records (422). The ledger keys a
mutation by (principal, id) for seven days: a retry with the same id
returns the stored result and applies nothing again; the same id with
another input is 409; after seven days the same id runs again, as a new
run. Every applied mutation also appends `{op, id}` to `ops`. A query's
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
| `POST /api/budget/reserve` | an agent (its owner's month) | `{ref, model, fragment, asker}` → `{key}`: one model call's worst case ($0.05) held, and the org's key to make it with; `{key, settled: true}` when that call settled before (made again after a crash, it is not charged again); 402 `budget_used_up` when the month cannot cover it |
| `POST /api/budget/settle` | the same | `{ref, cost}` (dollars, as OpenRouter reported; none: charged the reservation) → `{cost}` in micro-dollars; `{ref, release: true}` gives the reservation back when nothing was billed |

A usage row is FIN-10's report shape: `{sourceRef, agent?, billingOrg,
period, unit: "usd_micro", quantity, state (reserved, settled), kind,
model, fragment, principal, at}`, one per paid step
(`sourceRef` is `<fragment>@<incarnation>/run/<run>/step/<index>`), or
per agent model call (kind `agent.text`, `sourceRef` `agent:<agent
id>/<turn>/<messages stored in the turn so far>`, `fragment` where it was
asked: a chat, a fragment, or the agent itself for its owner's own
conversation; `principal` who asked), recorded once.

### Jobs and triggers

A job is a method called `(input, job)` that runs as a celld Workflow,
outside any request. Each `await` on a `job.*` step is durable. The
steps are the four below, the files steps (`job.files.*`), `job.push`,
and the AI steps (`job.ai.*`), all above:

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
most 256 steps (`limits::JOB_STEPS_MAX`; a video waits in polls and
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
| `POST __op/{op}` | a browser's call: `application/json` `{id, input}`; a signed-in browser (`fragment_site`) calls as its person; an unsigned caller gets an anonymous principal cookie; callers holding only `public` get 60 calls a minute each, 600 per fragment (a page's live views re-run over `__live`, outside this) |
| `POST __op/channels/{channel}` | a browser's post (`fragment.post`), through the call's door and its checks: `{id, input}` with the record's body as `input` → `{result: record, replayed}`, as `POST /api/f/{name}/channels/{channel}` answers it; a post spends the public budget as a call does (no operation name holds a `/`) |
| `__signin`, `__signout` | this origin's session (Sign-in, above) |
| `__frame?name=&return=` | a frame of this page signed in on one of its owner's fragments (Sign-in, Frames) |
| `__fragment.js` | the browser library (below) |
| `__chat.js`, `__chat.css` | the chat's page, the platform's (docs/platform.md): `import { mount } from "./__chat.js"; mount(document.body, {suggestions?, placeholder?})` renders a chat's `chat` and `work` channels (the chat template, below) |
| `__fragments` | `{fragments: [{name, role, url, share, sharing?}], frame}`: the fragments this fragment's owner belongs to, only to the owner signed in here, and only when `fragment.json` at live declares `"capabilities": ["fragments"]` (anyone else, or a page that does not ask, 403). `share` is its share sheet (`<platform>/share/<name>`); `sharing` is the owner's list's (`GET /api/fragments`): a read asks the owner's Principal cell alone and wakes none of the fragments listed. `frame` is whether this page may show them inside it (`__frame`; `null` when its `fragment.json` does not ask): the desktop shows a notice in place of its panes without it. A dashboard's page, such as the desktop's. `POST` `application/json` `{label, template}` → `{name, url}` makes `<label>.<username>` for the owner, as `POST /api/fragments` would, under the same conditions |
| `__people?id=…&id=…` | anyone who can see the fragment: `{profiles: {<id>: {kind, username, picture}}}` for up to 64 identities (an agent's `username` is its owner's; a picture is an absolute platform URL); an id the registry does not hold is left out |
| `__files` | an HTML list of the content files (live and main) linking to `__file`; framed, a click asks the page around it to open the file (`postMessage({fragment: "open", url, title})`) |
| `__live` | WebSocket, anyone who can see the fragment: channel subscriptions from a cursor, presence, change signals, queries (below) |
| `__watch` | WebSocket, viewers and up (the share link, or a signed upgrade): `{type: "hello", ref, sha}`, then `{type: "changed", ref: "main", sha, paths}` per external move of main |
| anything else | the app's `fetch`, when it has one |

A site file carries an `ETag` that names its bytes: the last commit that
changed it, or, for a page given Open Graph tags, a weak tag that also
names the live commit (its `fragment.json`). `__fragment.js`, `__sw.js`,
`__chat.js`, and `__chat.css` carry a hash of their bytes. A `GET` or
`HEAD` whose `If-None-Match` names the current tag answers 304 without
reading the file.

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
channel, onRecord, {after?, last?})` (pages through the backlog, then
follows live; after a reconnect it resumes after the last record),
`presence.set(data)` (changes within 150 ms go as one, the latest),
`presence.on(fn)` (called with everyone here now, and on each change),
`me()`, `closed(fn)`. The page's
socket reconnects by itself after a jittered wait (half to one and a
half times a backoff that doubles from 1 to 30 seconds), except after a
close the fragment means for good: 4003 (the page's access was revoked)
or 4004 (the fragment was deleted) ends it, and `closed` handlers get
`{code, reason}`.

CLI: `fragment call <name> <op> --input '{...}' [--id ID]`, `fragment
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
caller is the agent's registered owner. Each agent's key is made by the
node's `KEYS` and signs there. Its turns spend its owner's month: each
model call reserves its worst case there first, with the owner's org key
that answers, and settles to the cost reported after (`POST
/api/budget/reserve` and `/settle`, signed by the agent; Budgets). Every
person has their own agent, `agent.<username>`, made on first need: a
fragment whose live `fragment.json` declares `"agent": {"personal":
true, "channel": …}` (the `chat` template's) has it as an editor that
listens there. Its variables: `FRAGMENT_API`
(the platform it acts on), `OPENROUTER_API_URL` (its model service),
`AGENT_URL` (the base of the inboxes it hands out: the platform's),
`AGENT_TEST_HOOKS=allow` (dev and e2e only). The script reads a request
body of at most 64 KiB, measured as it arrives (413 before anything
else).

| method & path | who | body → answer |
| --- | --- | --- |
| `POST /api/agents` | a person with a username | `{name, model? ("z-ai/glm-5.3-flash"), instructions?}` → `{name, npub, model, id}`: made and registered as the caller's; again by its owner, the same answer (`replayed`); a name under someone else's username is 403 |
| `GET /api/a/{name}` | owner | → `{name, id, owner, npub, model, active, driving, outcome (running, idle, stopped, yielded, error), error, tokens, watchdogRestarts, conversation, asker, waiting: [{conversation, asker, at}], conversations: [{conversation, outcome, error, asker, at}], listens: {count, newest: [{fragment, channel, at}]}, ignored: [{fragment, channel, principal, at}], messages: [{id, role, text, tool_requests, tool_responses, steer, conversation}], steer, toolRuns, steps}` (each list its newest 256, oldest first but `conversations` and `listens`, newest first). `active`, `outcome`, and `error` are the running (or last) turn's, of any conversation; `conversation` and `asker` name it (`direct` is the owner's own conversation, a chat's is `<fragment>/<channel>`); `steer` holds the running turn's messages from its starter sent while it worked (a new turn drops those the model read); `ignored` notes anonymous messages, which start nothing |
| `GET /api/a/{name}/state?wait_ms=` | owner | → `AgentState` `{active, driving, outcome, error, answer}` (`crates/proto`) of the owner's own conversation: `active` while a turn of it runs or waits behind a chat's, `answer` its newest message when that is the model's text; answered once it is not active or `wait_ms` (0-25000, default 0) has passed: the read waits in the agent's cell, so a client waiting out a turn asks about every 25 s (`fragment agent say` does) |
| `POST /api/a/{name}/turns` | owner | `{text}` (at most 16 KiB) → `{started}`; during the owner's own turn, `{steered: true}` (read between steps); during another (a chat's), `{queued: true}`: it runs next, in the owner's conversation. At most 64 messages wait (429) |
| `POST /api/a/{name}/stop` | owner | → `{active, driving}`; a tool in flight is interrupted; the messages waiting run next |
| `GET /api/a/{name}/tools` | owner | → `{tools: ["platform__create_fragment", "platform__list_fragments", "platform__operations", "platform__call", "platform__list_files", "platform__read_file", "platform__write_file", "platform__append_file", "platform__write_files", "platform__deploy", "<fragment>__<op>", ...]}`: what the owner's own turn has (below) |
| `POST /api/a/{name}/listen` | owner | `{fragment, channel? ("chat"), reply? ("say")}` → `{fragment, channel, reply, subscription}`: the agent subscribes itself to the channel (it must be a member) with an inbox URL of its own (`AGENT_URL`); at most 500. `reply` answers a chat whose channel takes no posts (one made before phase 7); a postable channel is answered by a post. A new listen first drops those of fragments the agent is no longer in: of the fragments its memberships leave out, up to 16 are asked, and one that answers 404 or 403 loses its listens (so does one whose subscribe answers either, and a chat whose answer's post does). Listening again to the same fragment's channel is the same listen: its inbox URL, so the one subscription (made again if the fragment dropped it) |
| `PUT /api/a/{name}/scope` | owner (a fragment's deploy) | `{fragment, tools, instructions, model?}` → `{fragment, tools, model}`: a fragment's own agent takes what its block declares (A fragment's agent, below); 403 for any agent not made for that fragment |
| `POST /api/a/{name}/inbox/{token}` | the fragment's delivery (the token is the capability) | a `Delivery` (`crates/proto`), decoded whole: one that does not decode (a record without its `seq`, say) is 400. A message (a body with no `kind`, or `kind: "message"`: its `text`, else its JSON) from an identity starts a turn in the chat's conversation, acting for that identity; from the running turn's starter in its conversation, it steers that turn; any other waits for a turn of its own (429 past 64 waiting: the fragment delivers it again). `{kind: "stop", turn?}` from the running turn's starter, in its chat, naming that turn (or none), stops it; from anyone else, or another kind, it is ignored, never a message. The agent's own, one heard before (within a day: past the longest redelivery), and a message from an anonymous visitor (`anon:`) are ignored (the owner's view keeps the newest 32 anonymous ones). The turn's last answer goes back as the agent, with the id `rp:<40 hex of SHA-256 of its message id>`: posted to the channel as `{text, turn}` when it takes posts (`POST /api/f/{fragment}/channels/{channel}`), else `POST /api/f/{fragment}/ops/{reply}` `{text}`; an unknown token is 404 |
| `PUT /api/a/{name}/computer` | owner | `{url, token, cwd? ("work")}` → `{url, cwd, tools}`: attaches a computer once it answers `GET /tools` with that token (400 when it refuses it, 502 when it does not answer); the token is sealed like the agent's key |
| `PUT /api/a/{name}/computer` | owner | `{connect: true, cwd?}` → `{connect, agent, token, cwd}`: a computer that connects out instead (`fragment computer connect --agent <agent> --token-file <f>`): a new connect token, answered once (the agent keeps its SHA-256), replacing any computer before |
| `POST /api/a/{name}/computer/poll` | the connect token (`x-computer-token`) | → `{requests: [{rid, method, path, body}]}`: what the agent asks of its computer (the routes `fragment computer serve` answers), at once or within 25 s; one fetched and not answered in 40 s is handed out again; a wrong token 403 |
| `POST /api/a/{name}/computer/answer` | the connect token | `{rid, status, body}` (at most 6 MiB) → `{ok}`. A turn leaves out a computer that has not polled in 60 s |
| `DELETE /api/a/{name}/computer` | owner | → `{detached}` |
| `POST /api/a/{name}/test` | owner, test fleets | `{hold_in_tool_ms?, hold_after_tool_ms?, watchdog_ms?, window_messages? (2-256), view_rows? (2-256), model_timeout_ms? (200-100000)}` |

One conversation per chat: a turn belongs to the owner's own
conversation or to one chat's, reads only it, and answers there. One turn
runs at a time; a message for another conversation, or from anyone but
the running turn's starter, waits for a turn of its own, and so does a
steer the turn ended before reading (unless it was stopped). The driver
that ends a turn starts the next one waiting in the same step. A turn
records who started it (the owner, or the identity whose record it was),
and every call it makes on the platform acts for them (`for`, above);
the agent's own calls (listening, its model calls' budget, a chat's
answer) name no one. An attached computer is its owner's: it joins its
owner's turns only, and never a fragment's own agent's.

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
`platform__list_files`, `platform__read_file`, `platform__write_file`
(`{fragment, path, text}`: one file), `platform__append_file` (the same:
adds to the file as it is at main, none yet being empty, in one commit),
`platform__write_files` (several), `platform__deploy`, and, in its
owner's turns only,
`platform__create_fragment` (a fragment the agent makes is its owner's,
the agent an editor). A call is `POST /api/f/<fragment>/ops/<op>?for=<asker>`
signed by the agent with the id `tc:<40 hex of SHA-256 of the tool-call
id>`: a replayed call replays the operation; a file write's key is the
tool call's. At most 64 steps a turn. Each step sends the model a window
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
answer to a chat is posted for it, and a short guide to building an app
(`BUILD_GUIDE` in `agent/src/lib.rs`, about 400 tokens: a page is
`site/index.html`; `fragment.json` and `app.mjs` only for data or live
updates; the templates; write, deploy, answer; never read other
fragments to learn the format; one file per call under 150 lines). An
agent made with the default instructions from before the guide (which
said to read the todo template) is told today's default instead.

A model call (`agent/src/model.rs`) asks for at most 4096 tokens and has
100 s, under the node's 120 s fetch timeout (`CELLD_FETCH_TIMEOUT_S`,
which counts to the answer's last byte, so streaming does not stretch
it); its answer is read whole before the turn sees it. A call past its
deadline, one that fails, or one that answers nothing (no text and no
tool call: reasoning alone is nothing) is made once more, told why when
that helps; a refused key or budget (401, 402, 403) is not. Nothing twice,
after tool calls that worked in the turn, is answered with what those
calls did ("Done. Here is what I did: …"). A reply cut off at its limit
(`finish_reason: length`) keeps what it wrote: a cut
`platform__write_file` or `platform__append_file` runs with the text so
far, and its result says where the file stops so the model appends the
rest; any other cut call is refused, saying why. A turn that fails (a
second failed call, or any other error) is never silent: "I couldn't
finish: <why>. Ask me to try again." is its answer, stored in its
conversation and posted to its chat as an answer is, and the chat's
`turn.end` carries the error.

### A fragment's agent (the `agent` block)

A fragment may declare an agent people talk to through one of its
channels, in `fragment.json` (checked at deploy like the rest of it):

```json
"agent": { "instructions": "agent.md", "tools": ["log_food", "today"], "channel": "ask", "model": "z-ai/glm-5.3-flash" }
```

`channel` is a channel it declares with a `post` role; `instructions` a
file of its repo, read at live (at most 8 KiB: one missing, empty, or
larger is refused as an invalid manifest is, live's code not installed
and `code.error` saying why); `tools` operations it declares, none
owner-only; `model` is optional. `{"personal": true, "channel": …}`
names its owner's own agent instead, with that agent's instructions,
model, and tools (the chat template's).

A deploy whose live manifest declares one makes it so, each part
idempotent (the alarm retries one that did not finish). The fragment's
own agent is named as the fragment is, made its owner's on first need
(`POST /api/agents` with a `scope`, which only a deploy names), and given
what the block declares (`PUT /api/a/{name}/scope {fragment, tools,
instructions, model}`, owner, for the fragment it was made for only); it
is an editor of this fragment, a member of nothing else, listening to
`channel`. A deploy that drops the block, or names another agent or
channel, removes the one before (its membership and its subscription).
Each change is an event (`agent.joined`, `agent.left`). An agent of that
name made otherwise is not taken over, nor is `agent.<username>`.

A fragment's own agent differs from a person's three ways. Its tools are
the block's operations, read with the fragment's status `for` the asker
and offered as their role may call them: no other fragment, no platform
verb, no computer. It keeps one conversation per person who posts
(`<fragment>/<channel>/<identity>`), so strangers never share one. And
it is told, after its instructions, that its answer is posted for it and
each call acts as the asker (no build guide). A signed-in person's post
to the channel starts a turn for them (an anonymous one starts nothing);
each call acts for them (`for`: the lower of their role and the agent's,
and the app's `call.principal` is them). On its own fragment, the
fragment's own agent gives a signed-in asker what that fragment's
visibility gives anyone who reached it, its link: on a `link` or
`public` fragment they are at least a viewer (their post on its channel
took that), a membership above it still wins, and a `members` fragment
gives nothing more; never the personal agent, nor another fragment; the answer goes to the channel as `{text,
turn}` and the steps to `work` when the fragment declares it postable,
the chat template's records (below). Its model calls are its owner's to
pay.

### The chat template (phase 7, slice C)

A chat made from the `chat` template is two channels and no app code (no
worker), with the platform's page (`__chat.js`, `__chat.css`), and its
owner's own agent answering (the `agent` block, above):

```json
"channels": {
  "chat": { "read": "public", "post": "viewer" },
  "work": { "read": "viewer", "post": "editor" }
},
"agent": { "personal": true, "channel": "chat" }
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
