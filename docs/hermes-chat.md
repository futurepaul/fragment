# A Hermes chat fragment on sandcastle

Status: design agreed 2026-09-30 (Finite's model); phase 1 in progress. Paul's calls, 2026-09-30:

- Get a chat over `hermes serve` working, with the chat in a fragment.
- Prove it through fragment.club.
- Build it before the cron provider (phase 5 step 4).
- Finite is deprecating Telegram and Finite Chat, so chat reaches Hermes
  only through its URL, which a request already wakes.

## Problem

A person on fragment.club makes a fragment from a `hermes` template. The
platform gives them their own Hermes on a sandcastle node, and the
fragment's page is a chat with it:

- new chats, and replies streamed as Hermes writes them, with its tool
  steps shown;
- past chats listed, reopened, and continued;
- the chat keeps working as Hermes sleeps and wakes, and after a new
  Hermes image.

## What was found (2026-09-30)

**Fragment side:**

- A page talks only to its own fragment; it never reaches a computer
  directly (docs/computers.md, docs/screen-streaming.md).
- Fragment pages live on `fragment.boats`, a different site from
  `sandcastle.fragment.club`. So sandcastle's `SameSite=Lax` router cookie
  would not ride a page's socket there.
- The platform holds no person's key. `KEYS` makes and signs only with
  keys it made for a cell (crates/native/src/keys.rs).
- A cell can dial an outbound socket (`fetch` with `Upgrade`). The socket
  keeps the cell resident (docs/screen-streaming.md). Under H1 a cell
  never holds a fleet secret.
- `POST /api/sandcastle/credentials` already bills a sandcastle computer
  owned by an agent or a computer identity to that key's person
  (cell/src/lib.rs). Nothing in fragment drives sandcastle's API yet.

**Hermes v0.21.5's web server (`hermes dashboard` under s6):**

- **Auth.** It gates every non-loopback bind: login with basic auth,
  OAuth, or OIDC. There is no trusted-proxy mode (hermes_cli/web_server.py).
- **WebSocket.** `/api/ws` takes only a single-use ticket, valid 30 s,
  from `POST /api/auth/ws-ticket`, which itself needs a session. The
  ticket goes in the query or the subprotocol list.
- **REST.** `/api/sessions/*` takes a session cookie or
  `Authorization: Bearer`. CORS is hard-coded to localhost.
- **The chat protocol** is JSON-RPC over `/api/ws`:
  - methods: `session.create`, `session.resume`, `prompt.submit`,
    `session.interrupt`;
  - events: `message.delta`, `tool.start`, `tool.complete`,
    `message.complete`.

  History comes from `GET /api/sessions` and
  `GET /api/sessions/{id}/messages`. Finite's dashboard preview drives
  exactly this (finite-mono a6d055d9).
- **Where turns run.** A turn started over `/api/ws` runs in the
  dashboard process. `/api/status`'s `active_agents` counts only the
  gateway's turns, so sandcastle's busy probe misses chat turns.

## Acceptance

On fragment.club with a real person, their Hermes on finite-lat-6:

1. **Making it.** A new `hermes` fragment shows its Hermes starting, then
   a chat, with no step outside the page and the CLI.
2. **Chatting.** A message gets a streamed reply from the model through
   fragment.club's model route, charged to that person. Tool steps show.
3. **History.** After a reload, past chats are listed, and one reopens
   with its messages and takes a new turn.
4. **Sleep.** Idle 30 s, Hermes goes warm, and the next message costs
   under a second more. From cold it costs under 10 s. A turn never
   pauses mid-reply.
5. **Updates.** A new Hermes image (a rebase) keeps every chat.
6. **Access.** The fragment's owner and editors chat; anyone else is
   refused. The page never holds Hermes' password, only a native session
   token that expires within the hour.
7. **Removal.** Removing the fragment's Hermes deletes its computer; its
   backups stay, as sandcastle's do.

## Constraints

**Musts:**

- Keys live only in `KEYS` (H1).
- The person pays through the existing model route.
- sandcastle stays generic: nothing fragment-specific in `sandcastle/`.
- Tests cover the valid, invalid, replay, and restart paths (the
  engineering style).
- CI proves the flow against fakes. The real proof is on fragment.club.

**Must-nots:**

- No Hermes password in a browser.
- No deploy to fragment.club without Paul.

**Escalations (Paul's):**

- Deploys to fragment.club.
- A new fleet secret: the platform's sandcastle grantor key.

## The design: Finite's model (Paul, 2026-09-30)

Paul chose to build it the way Finite does, so Finite could adopt it
without fragment's other ideas. Finite's dashboard preview (finite-mono
a6d055d9) works like this:

- **Core holds the credentials.** Core keeps each runtime's Hermes
  basic-auth credentials and exchanges them for a native session
  (`finite-saas-core/src/hosted_hermes_session.rs`: "native session
  exchange, never custom token minting").
- **The browser gets a grant.** An authorized browser gets
  `{baseUrl, accessToken, expiresAt}` from Core (`/api/agents/{id}/hermes-access`).
- **The browser talks to Hermes directly.** It sends REST with
  `Authorization: Bearer`, mints a ws-ticket, and opens `/api/ws` with the
  subprotocols `hermes-gateway-v1, hermes-gateway-ticket.<t>`
  (`apps/dashboard/src/lib/hosted-hermes-status.ts`).
- **The edge is transport only.** Caddy allows CORS for exact origins,
  deletes Hermes' localhost-only CORS headers, and passes Hermes' own
  authentication through (`finite-saas-runner/src/hosted_hermes_caddy.rs`).

Here, the fragment's cell stands in for Core and sandcastle's router is
the edge:

1. **Ownership (Paul: a cell key and the platform's grantor).**
   - A fragment declares `"hermes": {}` in `fragment.json`. Its own
     `Hermes` cell (not the Sprites-shaped `Computer` cell: phase 1's
     notes) holds it, the way Core holds a runtime.
   - Its owner key is one `KEYS` makes for that cell, registered as a
     computer identity that its fragment's owner owns (as a Sprite
     pairs). So the model route bills the person, and revoking the
     identity cuts it off.
   - The platform's sandcastle grantor key is a new fleet secret that
     only `KEYS` uses; the node lists it with `--grantor`. The cell has
     `KEYS` sign every sandcastle call.
2. **The edge.**
   - The computer's URL is `url_auth: public`: the router adds no gate,
     as Finite's Caddy adds none, and Hermes' native login is the gate.
   - A new, generic spec field, `cors_origins` (exact https origins,
     bounded), makes the router answer those origins' preflights, set
     `Access-Control-Allow-Origin` with `Vary: Origin`, and delete the
     service's own CORS headers.
   - The platform sets it to the fragment's origin.
3. **Hermes' gate (Paul: basic auth, a generated password).**
   - The cell generates the password and keeps it sealed as a cell
     secret. It reaches Hermes in the spec's `env`, which sandcastle
     stores in plaintext (already in the debt ledger).
   - The session TTL is short: `HERMES_DASHBOARD_BASIC_AUTH_TTL_SECONDS`,
     an hour.
   - The cell exchanges the password for a native session, as Core does.
4. **The grant (Paul: the owner and editors).**
   - A signed-in viewer who owns or edits the fragment gets
     `{baseUrl, accessToken, expiresAt}` from `POST /__hermes/access` on
     the fragment's own origin.
   - The page never sees the password. It holds a session token scoped
     to that one Hermes, with a short lifetime, as Finite's does.
5. **The page.**
   - A chat in the `hermes` template, talking to Hermes the way Finite's
     provider does: chats in a sidebar from `/api/sessions`, the
     transcript from `/api/sessions/{id}/messages`, and live turns over
     `/api/ws` (`session.create`, `session.resume`, `prompt.submit`,
     `session.interrupt`, and the `message.*` and `tool.*` events).
   - Plain modules with no build, and a client module shaped so Finite
     could lift it.
6. **Awake during a turn.**
   - Hermes' busy signal misses chat turns: `active_agents` counts only
     the gateway's, and a `/api/ws` turn runs in the dashboard.
   - The precise signal, `/api/health/idle`, needs a session, and the
     drain token is scoped to the drain route. So stock Hermes has no
     signal a node can read.
   - So the page keeps its Hermes awake: while a turn is in flight (from
     `prompt.submit` to `message.complete`), it sends `gateway.ping`
     every 15 s. Those are the client's data frames, which the router
     already counts as activity.
   - A page closed mid-turn leaves the turn to finish if Hermes stays up,
     or to resume on its next wake. A public count of turns in flight,
     upstream or in our image, would let `service.busy` cover that.

The costs of Finite's model, taken knowingly:

- Hermes' login is reachable from the internet. A generated 32-byte
  password makes guessing it hopeless, and sandcastle's missing per-caller
  rate limit is already in the ledger.
- A browser holds a Hermes session token for up to an hour.

## Phases

1. **sandcastle:**
   - `cors_origins` in the router (valid, invalid, preflight, a stranger
     origin);
   - the Hermes preset, public with CORS, proven by the sandcastle e2e: a
     native login, a ticket, a chat turn over `/api/ws` from outside,
     and history over REST; a turn with the page's heartbeat outlasts
     the idle time.
2. **The runtime seam:**
   - the `Hermes` cell: grant, create, observe, delete, idempotent
     across restarts;
   - its owner key registered as a computer identity;
   - a fake sandcastle in `crates/fakes`, and an e2e lane.
3. **The grant:**
   - `POST /__hermes/access` with its access rules (the owner, an
     editor, a stranger, signed out);
   - the native session exchange against a fake Hermes (login, the
     ticket, the JSON-RPC subset) in `crates/fakes`.
4. **The template:** the chat page, driven by headless Chrome in the
   lane: a turn, history, a reload, a refusal.
5. **fragment.club (Paul approves):**
   - the grantor secret, the cell deploy, and lat-6's `--grantor`;
   - the hosted e2e as the acceptance above, with evidence.

### Phase 1, built (2026-09-30)

Built:

- `cors_origins` in sandcastle's router, with a daemon test through the
  real router: a preflight admitted and refused, a read admitted and a
  stranger's, the service's own headers dropped, and none named.
- Hermes' chat checks in the sandcastle e2e, with a WebSocket client
  that reads its socket as a browser does, answering pings at once.
  Hermes closes a socket whose pong is 20 s late, which a client reading
  only when it expects an answer will be.

On finite-lat-6 (36 checks):

- **The edge.** The URL made public with no new machine. Hermes' own
  gate refuses without a login. A native login gives a session. The
  page's preflight and its bearer reads carry the page's origin.
- **The socket.** The ticket opens the socket once, and a new chat's turn
  streams from the model through the swap: first words in 4.0–4.5 s, and
  history holds the turn.
- **Sleep.** A page's heartbeat (a `gateway.ping` every 15 s) keeps
  Hermes awake past the idle time. Quiet, it goes warm under its open
  socket (33 s).
- **The wake.** The next message on that socket wakes it and is answered:
  first words in 1.2 s, most of it the model. An open connection
  survives a pause and resume through msb's port forward.

The per-fragment cell is a new `Hermes` cell, not the Sprites-shaped
`Computer` cell. That cell's file sync, hands, and per-tick billing are
Sprites', and a small cell with Core's shape is what Finite could lift.

### Phases 2 and 3, built (2026-09-30)

Built:

- **`"hermes": {}`** in `fragment.json` (no keys yet; any is refused at
  deploy), and a `Hermes` cell per fragment that declares it. Its steps
  are Core's: a key `KEYS` makes; that key paired as a computer its
  fragment's owner owns (`<label>-hermes.<username>`); a grant of one
  computer signed by the platform's grantor key (`KEYS`'
  `sandcastle/grant`, which grants nothing but that size); a generated
  password and session secret, sealed; the computer; then its view read
  until the node settles it. Each step's result is saved before the
  next, so a retried or restarted one lands once. A failed step is tried
  again with backoff and said in the fragment's `events`.
- **Removal.** A deploy without the block deletes the computer and
  removes the key's computer identity (every key revoked), so the node's
  next credential ask gets nothing. A later declaration makes a new key
  and computer.
- **The grant, `POST /__hermes/access`** on the fragment's own origin: an
  owner or editor signed in there gets `{baseUrl, accessToken,
  expiresAt}`; anyone else is 403, anyone signed out 401, and 409
  `not_ready` while it is made. The origin is the one the router served
  the page from, never the client's `Origin`; it is named on the
  computer's `cors_origins` first (the latest four). The session is
  Hermes' own from its login and is cached until a minute before its end.
- **Asks during a step.** A deploy's ask that lands while a step awaits
  stands over that step's save (`fragment_core::hermes::merged`), and the
  grant names an origin only between steps, so a removal's delete never
  races its put.
- **The sandcastle fake** (`crates/fakes/src/sandcastle.rs`): the node's
  API with NIP-98 and replay refusal, grants only from the grantor,
  owner-only computers, and a fake Hermes behind the router's CORS: its
  login, `api/auth/me`, sessions, the single-use ticket, and the
  `/api/ws` JSON-RPC subset a chat uses.

The e2e lane `hermes` (26 checks, on fakes): made from a deploy, its
events, one computer and one grant to a key of its own, the spec, the
key as its owner's computer, the credentials ask billing its owner; the
owner's session, its origin named, a cross-origin read and a turn over
`/api/ws`, the same session asked again, a stranger and the signed-out
refused, an editor admitted; removal, the key revoked; declared again
and the node crashed mid-making, made once when back.

The escalations for phase 5 (Paul approves each):

- the fleet secret `FRAGMENT_KEYS_SANDCASTLE_GRANTOR_KEY` (a nostr secret
  key in hex), and `FRAGMENT_SANDCASTLE_API`
  (`https://api.sandcastle.fragment.club`) in the fleet's vars;
- lat-6's unit adding `--grantor <its public key>`;
- a fragment.club deploy (nodes first: `KEYS` gains `sandcastle/grant`);
- `*.sandcastle.fragment.club`'s wildcard certificate (DNS-01 on Paul's
  DNS), since each Hermes is its own host there.

## Evaluation

- **Unit tests:**
  - the router's CORS: an allowed origin, a stranger, a preflight, the
    service's own headers deleted;
  - the grant: the owner, an editor, a stranger, signed out, an expired
    native session renewed once;
  - the provisioning steps: each retried after a crash lands once.
- **The fragment e2e lane `hermes`, on fakes:**
  - make the fragment, chat, reload into history;
  - another person is refused;
  - remove the computer;
  - a node restart mid-chat.
- **The sandcastle e2e:** a chat turn from outside on lat-6 (login,
  ticket, `/api/ws`), history over REST with CORS, and a turn longer than
  the idle time that does not pause.
- **The hosted proof:** the acceptance list on fragment.club and lat-6,
  written as evidence.
