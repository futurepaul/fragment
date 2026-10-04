# fragment self-hosted

Status: **proposed 2026-10-03** (Paul asked for a spike). It lives on the
branch `selfhost`, which is kept unmerged and rebased onto master until
master is live on Cloudflare. Where this disagrees with
`docs/cloudflare-v1.md`, that doc wins for master; this one is the
self-hosted lane its "The product" section says returns once the
product works.

## The four ways fragment runs

1. **On Cloudflare.** This is master's plan of record.
2. **Self-hosted on bare metal.** celld runs the cells and sandcastle
   runs the computers, with state in a bucket. fragment.club ran this way
   until the cut (tag `celld-final`), minus sandcastle.
3. **Local, or a company's network, with no internet.** The LLM is on the
   same network. A developer's box is the smallest case; an intranet is
   the largest.
4. **Blends.** Any part runs where another doesn't. The first blend worth
   selling is computers on sandcastle nodes (faster, bigger, cheaper
   machines than Containers' 4 vCPU and 12 GiB ceiling) with everything
   else on Cloudflare.

## The rule: one cell, interfaces, and placement as configuration

The cell is written against two kinds of interface:

- **Cloudflare's runtime APIs:** Durable Objects, the Worker Loader and
  Facets, Workflows, Queues, R2, and `ctx.container`. These are already
  the platform's interfaces (decision 2). celld implements them, and
  sandcastle implements `ctx.container`.
- **Protocols, for whatever sits outside a runtime.** These are OpenAI's
  chat completions, OpenID Connect, the code-store contract, the Chrome
  DevTools Protocol, and Web Push.

Self-hosting supplies other implementations of the same interfaces. The
cell never asks "am I self-hosted?". It reads capabilities from its
configuration instead: where its model upstream is, where a computer
runs, who signs people in, and where its git lives. A mode flag is a
design bug, for the same reason decision 2's rule makes a Hermes branch
one.

So the measure of this branch is **how little of it is not
configuration**. Every place where self-hosting forces a branch in the
cell is a seam master should fix. Those are listed under "What this
shows about master", below.

## The seams

Each seam lists what Cloudflare uses, what self-hosting uses, what the
spike does, and what it suggests for master.

### 1. The runtime: workerd or celld

celld (`github.com/futurepaul/celld`, a fork of `denoland/celld`)
implements everything the cell uses except the `ai` and `browser`
bindings. Workers AI and Browser Rendering are out of celld's scope, so
it refuses those keys, along with `build` and `routes`, at deploy time.
Its `containers` support exists only on the fork's `krun-engine` branch.
This design doesn't need it (seam 2).

- **Cloudflare:** `wrangler deploy`, from the config `xtask deploy`
  renders.
- **Self-hosted:** `celld deploy` (a fleet) or `celld dev` (one node,
  state in a local directory), from a config rendered for celld. The
  render drops `ai`, `browser`, `build`, `routes` and `containers`.
  Cards then go to the renderer (seam 7).
- **The bucket:** celld takes a cell's ownership with a conditional
  write, so its bucket must support `If-None-Match` and `If-Match`. AWS
  S3, R2, Ceph RGW and Tigris do. **Garage does not**, and MinIO's
  community edition has been archived since February 2026. A one-box
  install needs no bucket at all: `celld dev` keeps state on disk.
- **Master:** nothing, beyond a celld target in xtask.

### 2. Computers: Cloudflare's container API, placed on a node

This is the largest seam, and the one the earlier designs circled
(`docs/two-substrates.md` and `docs/containers-on-celld.md` on their
branches; sandcastle's `docs/krun-engine.md`, E6).

The Computer DO already reaches its container through one object,
`ContainerHost` (`cell/entry.mjs`). Every runtime call it makes is a
method of `ctx.container`, and so is every call the Sandbox SDK's
`DirectoryBackup` makes. The seam is therefore that object:

- **`ctx.container`:** Cloudflare Containers, or Docker under
  `wrangler dev`.
- **A sandcastle node:** a `RemoteContainer` in the cell that implements
  the same methods over the node's engine API. `ContainerHost` picks one
  by the computer's placement, and nothing else changes: not
  `computer.rs`, not `DirectoryBackup`, not the image.

**Decision proposed: sandcastle is always reached as a node, even on the
same box.** The alternative is celld's `krun-engine` backend, where
`ctx.container` inside celld is sandcastle over a unix socket. Reaching
it as a node instead means:

- **one path for every placement.** A blend (the cell on Cloudflare) and
  a LAN (the cell on one box, computers on others) need a node anyway.
  With one box as the same path over loopback, there is one thing to
  test.
- **celld needs no container support.** That is one fewer fork branch to
  carry and rebase.
- **sandcastle's ceiling stops being celld's.** Placement across
  machines, isolation tiers and larger sizes are a node's concern.

**The engine API, on the network.** sandcastle's engine already speaks
Cloudflare's container API, as HTTP with JSON on a unix socket. A node
adds:

- **a TCP listener,** where every request is signed by the platform. A
  node is enrolled with a key that only the platform and the node hold.
- **exec over a WebSocket.** The engine's framed stream travels in binary
  messages, because a Worker's `fetch` can upgrade only to a WebSocket.
- **guest ports over HTTP.** `ANY /v1/containers/{name}/ports/{port}/…`
  is proxied to the guest, WebSockets included, in place of `ports.sock`'s
  file descriptors.
- **intercepts to a URL.** An intercepted request goes to the platform's
  `/api/nodes/egress`, signed by the node and naming the container and
  the intercept's index. The Computer DO dispatches it to the binding it
  set. A callback that finds no binding (because the object restarted
  while its container ran) re-arms first.
- **its CA at Cloudflare's path,**
  `/etc/cloudflare/certs/cloudflare-containers-ca.crt`. The engine
  already does this, so the Hermes image needs no change.

**Reachability is the network's job, not the protocol's.**

| Where the node is | How the cell reaches it |
|---|---|
| The same box | loopback |
| A LAN or an intranet | a private address with TLS from the operator's CA |
| A datacenter | a public address with TLS |
| Behind NAT, or a corporate proxy that allows only outbound HTTPS | the **uplink** (phase S7): the node dials a WebSocket out to a `Node` Durable Object, which offers the same HTTP API through a `fetch` |

The uplink is the only shape that works everywhere (the research below),
and it is the "person's own computer" shape of `two-substrates.md`'s
phase 5. iroh is an optimisation over it, not a transport we depend on.

**The uplink (S7).** sandcastle's `docs/node.md`, "The uplink", is the
protocol. In short:

- The node dials `<platform>/api/nodes/uplink`. The dial is signed with
  the node's secret over its id, a fresh nonce and the time. The platform
  answers with a signed `hello` over that nonce before the node serves
  anything.
- One `Node` Durable Object per node id holds the socket (`cell/uplink.mjs`).
  Its `fetch` sends a request down the socket as a stream of frames, and
  streams the answer back. A `101` comes back as a `WebSocketPair`
  bridged to the stream. A call is one message, because a Worker's TCP
  Nagles (found item 9).
- When the socket drops, a read the node had not yet answered (`wait`,
  inspect) is asked again on its next dial. So a computer's `monitor()`
  outlives a reconnect.
- `NodeContainer` takes its transport from `FRAGMENT_NODE_URL`: a `fetch`
  to the node's URL, as before, or the `Node` object's `fetch` for
  `uplink:<id>`. Nothing else in it changes, and the calls are signed
  as before.
- Intercepts stay on HTTPS to `/api/nodes/egress`: a node that can dial
  the platform can reach it, and the router spreads them across the
  computers' objects instead of one node's.

**Snapshots stay node-local.** A computer that moves to another node
starts from its image and restores `/data` from its last backup. That
is the path a new image already takes (decision 18).

**Isolation is reported, not assumed.** A node is a microVM host when it
has `/dev/kvm`, as this box does. KVM is often missing inside corporate
VMs: VMware leaves nested virtualization unsupported in production, and
AWS and GCP offer it only on some instance types. gVisor is the
non-KVM tier, later. The node states its tier, and the operator's policy
decides whether that tier may run strangers' code.

**Master:**

- A computer's placement becomes a field of the computer: `cloudflare`
  or `node:<id>`. Decision 13's "bring-your-own machines can return
  without a redesign" is this field.
- A computer should need no internet. Today a guest calls code.storage
  directly (the debt-ledger entry "An agent's sync and deploy reach
  code.storage directly"). With git behind an intercept
  (`git.fragment.internal`), as models and storage already are,
  `enableInternet` becomes the operator's policy. That is what an
  intranet needs, and it is a security win on Cloudflare too.

### 3. Models: one OpenAI-compatible upstream

**Today.** The route already speaks OpenAI chat completions downstream:
guests call `model.fragment.internal/v1/chat/completions`, and the goose
agent calls `/api/models/v1/chat/completions`. Upstream it speaks the
Workers AI binding's shape. Its dev and e2e seam, `FRAGMENT_AI_URL`,
posts that binding-shaped input to `<url>/run/<@cf id>`.

**Self-hosted.** `FRAGMENT_MODEL_URL` is an OpenAI-compatible base URL:
vLLM, llama.cpp's `llama-server --jinja`, Ollama, LiteLLM, or a
company's gateway. `FRAGMENT_MODEL_KEY_FILE` holds its key, when it
takes one. `FRAGMENT_MODELS` maps each tier's model id to the upstream's
model name, keeping the `@cf` ids as the price book's labels. Usage
comes from `stream_options.include_usage`. A call whose answer carries
no usage is charged the reserved worst case, as today.

**Image steps** stay off unless an images upstream is configured. A
local diffusion server must answer with a JPEG (`media.rs`).

**What fits this box (an RTX 5070 Ti, 16 GB):**

- **gpt-oss-20b** is the agentic floor, at 128K context.
- **Qwen3.6-35B-A3B,** with its experts offloaded to RAM.
- **Bonsai-2-27B** is being set up by another session in
  `~/dev/localinfer`, behind an OpenAI-shaped `/v1/chat/completions`.

Tool calling varies by server: Ollama's `/v1` has no `tool_choice`, and
llama.cpp has known argument quirks. So the self-hosted lane runs the
real-Hermes lane against a real local model, not only a scripted one.

**Master:** consider the same protocol on Cloudflare. AI Gateway has an
OpenAI-compatible endpoint (`…/compat/chat/completions`, model
`workers-ai/@cf/…`). If its Unified Billing, metadata and session
affinity hold there (to verify), the binding-shaped path and its fake
could go. Then dev, the e2e, Cloudflare and self-hosting would speak one
upstream protocol. Either way, the e2e's fake should speak the protocol
production uses.

### 4. Sign-in: OpenID Connect

**Master today.**

- The sign-in is WorkOS-shaped: `/user_management/authorize`, then a JSON
  `authenticate` call.
- The cell reads `user.id` and `user.email` from the response, and the
  `sid` claim of an access token that it does not verify. It trusts TLS
  instead.
- People are keyed by `(issuer, subject)`, with the issuer
  `workos:<client_id>`. That key is already right for OIDC.

**Built (branch `selfhost-oidc`): any OpenID Connect provider, beside
WorkOS.** Configuration says who signs people in: with
`FRAGMENT_OIDC_ISSUER` set, OIDC does; else WorkOS, as before. WorkOS
configured beside OIDC serves Pipes' connections alone.

| Setting | What |
|---|---|
| `FRAGMENT_OIDC_ISSUER` | the issuer URL, exactly as its id_tokens' `iss` (https; http only on loopback). Its metadata is at `<issuer>/.well-known/openid-configuration` |
| `FRAGMENT_OIDC_CLIENT_ID` | the client |
| `FRAGMENT_OIDC_CLIENT_SECRET` | a Worker secret, read only by keys.rs (docs/secrets.md); absent, the client is public and PKCE is its only proof |
| `FRAGMENT_OIDC_SCOPES` | default `openid email profile` |
| `FRAGMENT_OIDC_CLAIMS` | JSON: `{"email": "email", "name": "name", "username": ["preferred_username", "upn"]}`, the defaults. ADFS: `{"username": ["upn", "unique_name"]}` |
| `FRAGMENT_OIDC_AUTH` | `client_secret_basic`, `client_secret_post` or `none`; default, with a secret, the first of the two the provider lists |

How it works:

- **The request.** The router reads the provider's metadata and sends the
  browser to its `authorization_endpoint` with a state (bound to the
  browser by a cookie, as for WorkOS), a nonce, and PKCE's S256
  challenge. The registry keeps the verifier and the nonce with the state;
  the verifier never leaves it.
- **The callback.** The registry spends the state first, before anything
  is awaited, so a callback sent again or raced finishes once. It
  exchanges the code form-encoded (RFC 6749 4.1.3, with the verifier),
  the client authenticated with Basic (id and secret form-encoded first,
  2.3.1) or in the form.
- **The id_token is verified** (`fragment_core::oidc`): its signature by
  a key of the provider's JWKS only, RS256 (the `rsa` crate, public-key
  operations only) or ES256 (`p256`); never `none`, an HMAC, an
  algorithm the provider does not list, or a key the token names itself.
  Then `iss` exactly, `aud` (and `azp` when there are several audiences),
  `exp`, `iat` and `nbf` within two minutes of skew, an `iat` at most 15
  minutes old, the nonce, and `sub`. A token that fails is 401, logged as
  `signin.refused` with why.
- **Who it is.** `(issuer URL, sub)`. The email, name and username are
  attributes, refreshed at each sign-in and never matched. No email is
  assumed: a sign-in is shown by its email, else its first username claim
  (`preferred_username`, `upn`), else its `sub`.
- **Caches.** The metadata and the JWKS are kept an hour per isolate. A
  key the JWKS lacks fetches it again, at most once in 15 seconds: a
  rotation is picked up within that, and a provider naming keys it never
  published is not asked again and again. Only the provider's own token
  endpoint hands the cell an id_token, so the bound is against a
  misbehaving provider, not a stranger.
- **Logout.** RP-initiated (RP-Initiated Logout 1.0) when the provider
  advertises an `end_session_endpoint`: the session's id_token, kept
  sealed for the registry, is the `id_token_hint` (Okta and ADFS require
  one), with `client_id` and `post_logout_redirect_uri`. Otherwise (Dex),
  logout ends the sessions here only.

Where it lives: the rules in `crates/core/src/oidc.rs`; the metadata,
keys and verification in `cell/src/oidc.rs`; the state, verifier, nonce
and exchange in `cell/src/registry/signin.rs`; the secret and the token
request in `cell/src/keys.rs`; the fake in `crates/fakes/src/oidc.rs`.

**Private CAs.** A company's browsers already trust its CA. The cell's
own fetches (metadata, JWKS, token) go through its runtime's TLS:

- `wrangler dev`: Miniflare hands workerd the certificates in
  `NODE_EXTRA_CA_CERTS` as trusted, so a private CA works.
- celld: a Worker's `fetch` trusts the webpki (Mozilla) roots alone
  (reqwest with rustls in `egress::client`), so a provider on a private
  CA fails the TLS handshake. celld needs a setting for an extra CA
  bundle, added to that client. **Not built**: it is celld's change.
- Cloudflare: Workers trust public CAs. A provider on a private CA is
  unreachable from Cloudflare anyway, unless it is fronted publicly.
- On one box, the issuer may be plain http on loopback, as the fake and
  Dex are here.

**Offline providers.** Dex, Keycloak and Authentik serve discovery and
a JWKS, and need no internet. ADFS 2016 and later does too, with `upn`
and `unique_name` instead of an email. Entra needs the internet. SAML
and LDAP go through a bridge the company already runs (Keycloak,
Authentik, Dex), not into the cell.

**Not built:**

- admins from a groups claim. Operators stay `FRAGMENT_OPERATORS`. A
  groups claim would need the registry to keep a role on the identity,
  refreshed at each sign-in, and the signer's resolve to carry it.
- `private_key_jwt`, the userinfo endpoint, refresh tokens, and back-
  or front-channel logout. The platform's session is its own (30 days),
  so **a person disabled at the provider keeps their sessions until they
  end or sign out.** A company will ask for back-channel logout, or a
  shorter session, next.

**Connections** (WorkOS Pipes) are online by nature. With
`FRAGMENT_CONNECTIONS` empty, a deployment offers none. Under OIDC, a
person has no WorkOS user, so they have no connections either.

**Evidence, 2026-10-03:**

- Host tests: 12 in `fragment_core::oidc`: valid tokens; bad signatures;
  `none`, HS256 keyed by the secret and by the RSA key's bytes; wrong
  `iss`, `aud`, `azp`, `exp`, `iat`, `nbf`, nonce and `sub`; a token
  replayed into the next sign-in; a rotation and the refetch bound;
  discovery; the token request; PKCE (RFC 7636's vector); the claim map.
  The fake's own test runs the cell's half of the flow on those rules.
- The e2e on celld, signing in through the OIDC fake
  (`FRAGMENT_E2E_SIGNIN=oidc`): auth, identities and signin, 173 passed,
  0 failed. Every WorkOS check of the signin section has an OIDC twin,
  and OIDC adds:
  - client_secret_basic used;
  - a code injected under another sign-in's state is refused (PKCE);
  - a spent code is refused at the provider;
  - a person with no email is shown by their username;
  - ES256 signs in;
  - eight spoiled id_tokens are each refused with 401;
  - a key rotation is picked up within the cooldown, with one JWKS fetch.
- The restart section finishes a sign-in begun before the restart, on
  either provider.
- The whole suite on celld through the OIDC fake: 1273 passed, 1 failed,
  12 skipped. The skips are the card checks, the container sections, and
  the shell's Pipes checks, whose people are WorkOS's. The failure was
  share's "the owner's list wakes no fragment", a race between the blank
  template's install and a test hook, while another build loaded the
  machine. Share alone then passed, 44 of 44.
- The WorkOS lane, unchanged, on celld: auth, identities, signin,
  levers, share, shell and restart, 314 passed, 0 failed.
- **Dex v2.45.1** (the static binary from its release image, run
  unprivileged in /tmp, memory storage, one static user) signed in on
  the dev stack on celld (`FRAGMENT_DEV_PORT=9310`). curl drove the
  browser's part.
  - `/auth/login` went to Dex with S256, a state and a nonce. The
    password form posted, and Dex redirected to the callback, which set
    the session.
  - The identity came back as `(http://127.0.0.1:5556/dex, <Dex's sub>)`,
    with `admin@example.com` as its email. Dex's local users carry no
    `preferred_username`, so the handle fell back to the `sub`.
  - Signing in again was the same person. A wrong password signed no one
    in.
  - `fragment login` approved its key in that session, and `fragment
    whoami` answered the same identity.
  - Logout was local only (Dex advertises no `end_session_endpoint`), and
    the session then answered 401.
  - With Dex rotating its keys every 20 seconds (it signs with a new key
    the moment it publishes it), sign-in still worked after a rotation:
    the refetch picked up the new key.

**Master:** sign-in on OIDC, verifying the `id_token`, is better on
Cloudflare too. Nothing here is self-hosting-specific: it is a second
provider beside WorkOS, chosen by configuration. If WorkOS AuthKit's
OIDC endpoint serves user sign-in (to verify), WorkOS's own path could
become one more issuer, and its unverified `sid` would go. Pipes would
then still need the WorkOS user id, which AuthKit's `sub` is.

### 5. Files: the code-store contract

**Today.** code.storage's REST contract is the subset that
`crates/fakes/src/codestorage.rs` implements, with real rules: JWT
scopes, compare-and-swap, merges, signed webhooks. The fake is not git:
its commit ids are synthetic, and it keeps one JSON file behind one
mutex, bound to loopback.

**Self-hosted.** A code store that implements the contract durably:
- the fake's rules over real git on disk (gitoxide), or over the bucket;
- reachable by the cell, the CLI and, through an intercept, computers;
- named in configuration, as `CODESTORAGE_API_URL` already is.

**The store: macrofiche** (`github.com/futurepaul/macrofiche`, a
sibling project as sandcastle is): git itself, pinned and driven as
jailed processes, answering the contract (its `docs/contract.md`, read
from this repo, and `docs/design.md`).

**Built (branch `selfhost-macrofiche`): the stack and the e2e on a code
store outside them,** chosen by configuration:

| Where the git lives | dev (`cargo xtask dev`) | the e2e |
|---|---|---|
| the fake in the process (the default) | nothing set | `FRAGMENT_E2E_CODESTORE` unset or `fake` |
| a store already running | `CODESTORAGE_API_URL`, `CODESTORAGE_ORG`, `CODESTORAGE_PRIVATE_KEY_FILE` (the variables the cell reads in production; the key by its file) | the same, with `FRAGMENT_E2E_CODESTORE=external` |
| macrofiche, started for the stack | `MACROFICHE_BIN` | `FRAGMENT_E2E_CODESTORE=macrofiche` and `MACROFICHE_BIN` |

- `devstack::codestore` holds the choice and macrofiche's start-up: its
  state directory (`target/devstack/macrofiche` in dev, the run's scratch
  in the e2e), a config file naming its listener and the org with its
  public key (SPKI PEM, `OrgKey::public_pem` from the private key the
  cell signs with), started on the fake's port in dev, and stopped with
  the stack. That shape follows macrofiche's design before it had a
  binary, and lives in one function.
- `fake-codestorage` (`crates/fakes`) is the fake as a process of its
  own, with an org key: a store outside the node whose levers nothing can
  reach. It proves the external mode without macrofiche.
- On an external store the e2e reads git only through the contract's
  routes, with tokens it signs with the org key (`crates/e2e/src/store.rs`):
  a file, a head, the org's repos, main's history. Its own writes are
  another writer's: a commit pack, and a deploy's ref move (live created
  at main's tip, else merged to it).
- **No store registers the cell's webhook.** macrofiche configures one
  webhook per org (its design, question 2), and the cell's are per
  fragment (`/api/f/<name>/webhook`, each with its own secret). So, as on
  the hosted fleet, `refresh` and the poll move the pins: the e2e follows
  each of its writes with the owner's `refresh`, as the CLI does.
- A check that pulls one of the fake's levers is a skip that says which:
  an outage of file reads, sabotaged commit packs, counts of the requests
  it answered, and its refs' ephemeral flag. Beside each, the part a real
  store can show is checked: a replay commits nothing (main's history, in
  place of the fake's count of packs), two saves are two commits, a
  preview ref is at main's tip read with `ephemeral=true`.
- e2e `create` asserted the fake's UUID-shaped url form; on an external
  store the url form is the store's own (the service's, and macrofiche's,
  is the repo's name).

Running it:

```sh
# the fake as a process of its own
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out org-key.pem
target/release/fake-codestorage --org fragment-ext --key-file org-key.pem --port 9450 &
FRAGMENT_E2E_CODESTORE=external CODESTORAGE_API_URL=http://127.0.0.1:9450 \
  CODESTORAGE_ORG=fragment-ext CODESTORAGE_PRIVATE_KEY_FILE=$PWD/org-key.pem \
  FRAGMENT_E2E_RUNTIME=celld CELLD_BIN=../celld/target/release/celld cargo xtask e2e
# macrofiche, started by the run
FRAGMENT_E2E_CODESTORE=macrofiche MACROFICHE_BIN=../macrofiche/target/release/macrofiche cargo xtask e2e
# dev on either
MACROFICHE_BIN=../macrofiche/target/release/macrofiche cargo xtask dev --clean
```

A cell remembers each fragment's repo, so a dev stack moved to another
store takes `--clean`.

**Evidence, 2026-10-03:**

- The whole suite on celld, its git in the fake run as a process of its
  own (`fake-codestorage`, `FRAGMENT_E2E_CODESTORE=external`), none of
  its levers reachable: 1293 passed, 0 failed, 22 skipped. Eleven skips
  are the fake's levers or its webhook, each naming which; four are the container
  sections, seven the card checks (celld shoots no cards). The store
  held 158 repos after `create` alone: its 150 fillers were made through
  `POST /api/repos`, and the name made again was found past the list's
  first page.
- The sections the change touches, on the fake in the process as
  before (create, files, deploy, templates, ops, effects, site,
  appfiles, blobs, notes, brain, ai, ledger, sync, restart, addon): 471
  passed, 0 failed, 7 skipped (cards).
- `cargo xtask dev --runtime celld` on the same store
  (`FRAGMENT_DEV_PORT=9100`): ready in 4.6 s, its banner and the cell's
  variables naming the store.
- macrofiche: not yet run. Its design is done and its engine phase under
  way; it has no binary to start yet.

**Master:** none now. Cloudflare Artifacts may replace code.storage
(decision 1). If it does, the self-hosted code store implements whatever
contract master then speaks.

### 6. Blobs and computer storage

- **Cloudflare:** R2.
- **Self-hosted:** celld's R2 over its bucket, or over the local
  directory under `celld dev`.
- **Master:** nothing.

### 7. Preview cards: Browser Rendering's routes, over a pinned browser

- **Cloudflare:** Browser Rendering, through the `BROWSER` binding.
  `wrangler dev` runs its local mode (a Chrome for Testing it downloads
  on the first shot).
- **Self-hosted:** the same three routes (`POST /v1/devtools/browser`,
  the CDP socket at `/v1/devtools/browser/<id>`, and `DELETE`), served by
  the **renderer** over a pinned chrome-headless-shell. The cell reaches
  it with `fetch` at `FRAGMENT_BROWSER_URL`, which wins over the binding.
  card.rs's acquire, CDP and release are one path; only who answers
  differs (`Renderer`).

**Built (branch `selfhost-cards`).**

**The browser: chrome-headless-shell 154.0.8037.92**, Chrome for
Testing's Stable on 2026-10-04. The survey, that day:

| | What it is | CDP | Screenshots | Licence |
|---|---|---|---|---|
| **chrome-headless-shell** | Chrome's old headless mode as its own build: one zip per platform (linux64, linux-arm64, mac-arm64, mac-x64), 99 to 121 MB, 274 MB unpacked | yes | Blink's own | BSD-3 (Chromium) |
| browserless | a Node server around Chrome; its image is about 1 GB | yes | Chrome's | SSPL, or commercial |
| Lightpanda 1.0 | one 188 MB binary, no layout engine | yes | the page's text alone, PNG only | AGPL-3.0 |
| Steel | a Node API over Chrome; 0.7 to 1.4 GB images | yes | Chrome's | Apache-2.0 |
| Playwright's server | needs Node | its own protocol | Chrome's | Apache-2.0 |
| Obscura | one 71 MB binary, layout of its own | yes | not Chromium's | Apache-2.0 |
| Firefox | | WebDriver BiDi only: CDP went in 141 | | MPL-2.0 |

Gotenberg and chromedp's headless-shell image wrap Chrome too; Servo and
Ladybird speak no CDP. So the pick is Chrome itself, its smallest build,
pinned as the Node is: no wrapper, and the CDP card.rs already speaks.
It is one zip but not one binary: it needs the host's NSS, glib and X11
libraries (its `deb.deps`) and fonts.

Sources: [Chrome for Testing's versions](https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json),
[browserless's licence](https://github.com/browserless/browserless/blob/main/LICENSE),
[Lightpanda's renderer](https://github.com/lightpanda-io/browser/blob/main/src/rust/render/lib.rs),
[Steel](https://github.com/steel-dev/steel-browser),
[Playwright's server](https://playwright.dev/docs/api/class-browsertype),
[Obscura](https://github.com/h4ckf0r0day/obscura),
[Firefox 141](https://developer.mozilla.org/en-US/docs/Mozilla/Firefox/Releases/141).

**Where the adapter lives: a renderer beside the stack**
(`crates/devstack/src/rendering`), the smallest of three:

- The cell speaking CDP to a browser it is configured with would be a
  second way to get one: Browser Rendering's acquire and release have no
  CDP equivalent.
- celld implementing the `browser` binding, as miniflare does, would
  launch browsers inside a fork whose aim is fewer differences from
  upstream, and Browser Rendering is out of celld's scope.
- A renderer keeps card.rs's one path and celld unchanged. The cell's
  `fetch` reaches it as it reaches the model server or a node. celld's
  `fetch` upgrades to a WebSocket (its `__fetchWebSocketUpgrade`), as
  the node's exec already uses.

The renderer starts a browser per session (a fresh profile and home) and
drives it over `--remote-debugging-pipe`. At most four run at once. A
session is stopped after its `keep_alive` without a client (default
60 s, at most 10 min), or 15 min after it began. A message either way is
at most 64 MiB.

**Isolation.** A card's page is a stranger's code. Its browser reaches
fragments' origins and nothing else on the box:

- **The gate.** Every connection the browser makes (http, https,
  WebSockets) goes through a SOCKS5 proxy in the renderer
  (`--proxy-server`, with `<-loopback>`, so loopback and `*.localhost`
  go through it too). The gate lets through
  `<one label with "--">.<suffix>:<the origins' port>` alone, and
  connects it to one configured address: the node, or the edge before
  it. It resolves no name. An IP literal, `localhost`, any other name,
  and a name rebound to another address all go nowhere, so the sandcastle
  engine, Bonsai, Dex, the LAN and the internet are out of reach.
- **Nothing beside it.** The browser resolves nothing itself
  (`--host-resolver-rules`, the gate's literal excepted). It sends no
  QUIC, and no WebRTC UDP outside its proxy
  (`--force-webrtc-ip-handling-policy=disable_non_proxied_udp`).
  WebTransport fails outright with a proxy set.
- **No port.** The browser listens on none: its pipe is the renderer's
  alone.
- **The process.** Chrome's sandbox stays on. Each browser gets a clean
  environment (no variable of the stack's), its own process group
  (killed whole), and a home removed after it.
- **The renderer** listens on loopback, unauthenticated. A local process
  may start sessions, which reach only what any visitor reaches.

**A private CA.** On an intranet, fragments are
`https://<label>--<user>.fragment.home.arpa` under the operator's CA.
`FRAGMENT_BROWSER_CA_FILE` names its PEM. The renderer makes an NSS
database trusting it, with NSS's `certutil` (Debian's `libnss3-tools`,
Fedora's `nss-tools`, Arch's `nss`), and copies it into each browser's
`~/.pki/nssdb`, where Chrome on Linux reads local roots. The browser
checks the edge's certificate itself, as a visitor's browser does.

- `--ignore-certificate-errors-spki-list` was the alternative. It
  matches only the certificates a server presents, so a pinned CA fails
  against a server that sends its leaf alone (tried: refused).
- On macOS the browser trusts the system keychain: the CA goes there.

**Pinning.** `browser_release.rs` pins the version and each zip's
SHA-256. Chrome for Testing publishes no checksums, so each hash is of a
zip whose MD5 matched the one Google's bucket answers. `browser.rs`
fetches it into `target/tools` once, checks it before unpacking, refuses
links and paths outside its directory, and names the directory only once
the binary answers `--version` with the pin. An intranet supplies the
zip itself (`FRAGMENT_BROWSER_ZIP=<path>`), checked against the same pin.

**Evidence, 2026-10-04:**

- **The e2e on celld,** sections site, deploy and ledger: 152 passed,
  0 failed, 0 skipped. Before, celld shot no cards, and these sections
  skipped their card checks there. Now they run: a deploy's card and a
  viewer's copy of it, a second
  deploy's card, revalidation by its tag, two failed shots then the card,
  five then one `card.failed`, and the shot's browser time on the
  owner's ledger. A guest's deploy and a members-only fragment still get
  none.
- **The same sections on `wrangler dev`** (Browser Rendering's local
  mode, the Cloudflare path, unchanged): 152 passed, 0 failed.
- **`cargo xtask dev --runtime celld`** (`FRAGMENT_DEV_PORT=9460`) was
  ready in 4.9 s, the renderer named in its banner and in the cell's
  `FRAGMENT_BROWSER_URL`. A session started (the browser answering CDP)
  in 35 to 39 ms, and closed in about 10 ms, leaving no process.
- **13 host tests** run against stand-in browsers (shell scripts on the
  same pipes):
  - a session's CDP both ways, a 300 KB message whole;
  - a second client refused (409), then let in once the first leaves;
  - a close, and its replay (404);
  - the routes it refuses;
  - four sessions, then 429;
  - a browser that dies as it starts fails the start at once, saying
    what it said;
  - a NUL in a message refused;
  - an idle session stopped after its `keep_alive`;
  - every browser stopped with its renderer, and a crashed renderer's
    homes cleared by the next;
  - the gate's rule and its SOCKS5;
  - the CDP socket, tungstenite's server side: a message in fragments
    joined; unmasked, reserved, unknown, oversized and non-UTF-8 frames
    each closed with its code;
  - the pinned zip: a hash mismatch, a path outside, a link, the limits.
- **Two run by name** against chrome-headless-shell 154 itself
  (`cargo test -p fragment-devstack -- --ignored`):
  - A card's JPEG through the renderer. Its page reached its own origin
    alone. A loopback service, tried by IP literal, `localhost` and
    another `*.localhost` name, by image, fetch and WebSocket, heard
    nothing; nor did the internet; and pages there did not open.
  - Fragments on https under a test CA: refused without it
    (`ERR_CERT_AUTHORITY_INVALID`), opened with it named.
- Before it was built, the same flags under a SOCKS5 stand-in showed
  what Chrome sends a proxy: every name, `127.0.0.1`, `::1` and
  `localhost` included, as a name (remote DNS), and its WebSockets.

**Master:** where the browser is becomes configuration
(`FRAGMENT_BROWSER_URL`), as the model upstream is. Cloudflare keeps the
binding.

### 8. Notifications

Web push goes through the browser vendors' push services (FCM, APNs,
Mozilla), which no operator runs and an air-gapped network cannot
reach.

- **Offline,** push is off and the shell's live channel notifies an open
  tab.
- **Master:** push is one delivery backend among several, and a
  deployment that cannot reach a push service says so instead of
  retrying forever. Webhooks to a company's chat are the intranet's
  answer.

### 9. Hosts and TLS

Each fragment's own origin (`<label>--<user>.<zone>`) is the isolation
model, so every way of running needs a wildcard name and a matching
certificate, or a single-box exemption.

| Where | Names | TLS |
|---|---|---|
| This box | `*.fragment.localhost` (Chrome and Firefox map it to loopback; Safari since macOS 26) | none: localhost is a secure context |
| A home LAN | a wildcard the box answers on the LAN, or a real domain's wildcard pointing at a private address (home routers' rebind protection may drop it) | a dev CA installed on each device, or a public wildcard certificate (DNS-01, online every 90 days) |
| An intranet | a delegated subzone, with the wildcard inside it. Companies grant this more readily than a wildcard in Active Directory DNS, which security tools flag as an attack | the operator's certificate files, or ACME from step-ca or Vault, or a dev CA |
| The worst intranet (one hostname, no wildcard) | path mode, labeled as a single trust domain (the debt ledger) | the one certificate |

An intranet zone is same-site with every other app under the company's
domain. Isolation must therefore rest on the origin: host-only
`__Host-` cookies and Origin checks, never on SameSite.

### 10. Money

The ledger meters wherever fragment runs.

- **A company** sets `FRAGMENT_DEFAULT_PLAN=seat` and credits from
  configuration.
- **One box** runs on a seat with the month's included credit, as dev
  does.

A local model has no list price, so its prices come from configuration
(the price book's defaults are already a debt-ledger entry).

### 11. Nothing calls home

**At runtime:**

- The shell and the chat template load Google Fonts. Vendor them in
  master: this is a privacy win anywhere.
- The skills point agents at esm.sh, unpkg, jsdelivr and pip. Offline,
  they need a configured mirror or vendored copies.

**At build time:**

- npm (wrangler, the Sandbox SDK), Docker Hub images, GitHub downloads
  for noVNC and Litestream, and crates.io.
- An intranet takes a release as one bundle: images pinned by digest
  under one configurable registry, the CLI, the templates, and model
  weights.
- Every service and every computer takes one CA bundle and the proxy
  environment (`HTTPS_PROXY`, a portable `NO_PROXY`, pip and npm
  mirrors).

## What corporate networks bring (research, 2026-10)

The constraints that shape this design, in order:

1. **A wildcard DNS name is not guaranteed.** A delegated subzone is the
   request to make. Path mode is the degraded fallback. Never depend on
   mDNS: it doesn't cross VLANs and can't do wildcards.
2. **A private CA, everywhere.** Every non-browser client (Rust, Node,
   Python, git, and everything inside computers) takes one CA bundle.
   Wildcard certificates may be banned, and ACME may not exist, so the
   certificate source is pluggable.
3. **Nothing may call home, and a release travels as one bundle.**
4. **Egress is proxied and often TLS-inspected.**
   - Zscaler has broken WebSockets and downgrades HTTP/2.
   - Proxies buffer SSE.
   - Live channels need a fallback.
5. **Identity is generic OIDC** against whatever the company runs: Entra
   or Okta online; ADFS, Keycloak or Dex offline.
6. **The LLM is any OpenAI-compatible endpoint,** with uneven tool
   calling and possibly short context.
7. **KVM is not guaranteed** inside corporate VMs.
   - Packaging must fit one box, one VM, and Kubernetes or OpenShift
     (non-root, arbitrary UID).
   - Computers need a non-KVM tier.
8. **Intranet zones are same-site.** Blends are cross-site, so they need
   partitioned cookies, and Chrome's Local Network Access prompts when a
   public page reaches a private address. Keep one browser-facing edge,
   and tunnel compute behind it.
9. **Between sites, the only path guaranteed is outbound HTTPS on 443.**
   That is the uplink. UDP peer-to-peer (iroh, Tailscale) is an
   optimisation with self-hosted relays.
10. **Web push is unavailable offline, and S3 compatibility varies.**

## What this shows about master

These are seams the spike exposes, each a candidate change on master
that also makes Cloudflare simpler or safer:

1. **The model route's dev seam is shaped like a binding, not a
   protocol.** Speaking OpenAI-compatible upstream, possibly AI
   Gateway's compat endpoint, would leave one path and a fake that
   tests it (seam 3).
2. **Computers reach a vendor directly.** Git behind an intercept means
   a computer needs no internet (seam 2).
3. **Sign-in trusts an unverified access token.** OIDC with a verified
   `id_token` fixes that, and makes WorkOS one provider (seam 4; built
   beside WorkOS on `selfhost-oidc`).
4. **A computer's placement is implicit in the `containers` binding.**
   As a field, it is decision 13's promise kept (seam 2).
5. **Google Fonts** in the shell and the chat template (seam 11).
6. **`FRAGMENT_EGRESS_LOCAL=allow` doubles as "this is a local fleet"**
   for the test levers (`config.rs`). An intranet needs local egress
   without being a test fleet, so these should be two settings.
7. **Push has no "unreachable" state** (seam 8).
8. **In sandcastle:** the engine hard-codes Debian's library paths
   (`crates/engine/src/linux/engine.rs`, `system_libs`), so it fails on
   Arch, this box. It also hard-codes amd64 and x86_64 throughout, so a
   Mac (aarch64, Hypervisor.framework) needs its own jail and egress
   path.
9. **`ContainerHost` reports to routes the cell does not have.**
   `entry.mjs` posts `container/exited` and `container/tab`, but the
   Computer's routes are `computer/exited` and `computer/tab`, and
   `routed.rs` refuses the first two names. So on master:
   - every WebSocket to a computer's port answers 500;
   - no exit is ever reported.

   The fix is the commit "computer: the container's reports reach their
   routes", ready to cherry-pick.

## What the spike found (running it)

These are listed as found. Each names where it bites and what to do.

1. **Preview cards share the delivery queue with chat.** On the first
   shot, `wrangler dev`'s local Browser Rendering downloads Chrome. The
   shot took 55 s, and the queue consumer held the next batch behind it.
   A person's first message to an agent waited 45 s before its delivery
   ran. Offline, a deployment has no Chrome to download (on celld, the
   pinned browser is fetched, or supplied, before the stack starts: seam 7).
   - **Master:** cards should get their own queue, or the shot should go
     after the batch's deliveries. Cards are already skipped when there
     is no hostname suffix; a deployment without Browser Rendering should
     skip them the same way.
2. **The agent's model deadline is fixed at 100 s** (`agent/src/model.rs`,
   `DEADLINE_MS`). Bonsai-2-27B on this box's CPU processes a prompt at
   about 8 tokens a second, so the agent's ~700-token first prompt alone
   takes 90 s. On the GPU it takes 0.2 s. A self-hosted model's speed
   varies by orders of magnitude.
   - **Master:** the deadline should be configuration that the
     deployment's model route announces.
3. **A failing model upstream fails well.** A dead port ("Network
   connection lost") and a slow model (the deadline) both release the
   call's reservation on the ledger, and the agent says in the chat what
   failed.
4. **The dev stack needed Docker even with no computers to run.** Now,
   with no Docker and no node, it runs without computers and says so.
5. **celld's fork still depends on fragment's deleted native crate.** The
   `native:<name>` seam that served `KEYS` (commits `5b76ced` and
   `aecfbb6`) points at `crates/native`, which phase 2 removed. The
   branch `selfhost` of the celld fork deletes the seam: master needs it
   no longer, and that is one less difference from upstream celld.
6. **A redeploy of the same code broke the app's next call on celld.**
   - `platform.mjs` locks an app out of facets by making
     `DurableObjectState`'s `facets` a getter-only accessor on its
     prototype.
   - celld's JS `DurableObjectState` assigned `this.facets` in its
     constructor. The same code redeployed reuses the loaded isolate, so
     the next facet's state threw: "Cannot set property facets … which
     has only a getter".
   - The e2e's `ops` section found it ("redeploying keeps the app's
     data").
   - Fixed in celld (branch `selfhost`): `facets` is now defined as an own
     property, as workerd's native accessor behaves.
   - It is a parity bug that only a second runtime shows. The e2e on both
     runtimes is the guard decision 2's port relies on.
7. **`fragment init` printed a webhook URL that answers 404** to any
   sender who isn't signed in. It used the short name (`/api/f/inbox/…`),
   which resolves only through a signer. This is a master bug, not a
   self-hosting one: the fix (commit "cli: init prints the webhook URL
   with the fragment's full name") is ready to cherry-pick onto master.
8. **celld's `dev` stops on SIGINT** (Ctrl-C), not promptly on SIGTERM,
   so devstack stops it with SIGINT.
9. **A Worker's TCP Nagles, and a Worker cannot stop it.** Through the
   uplink, every call with a body (start, each intercept) took 40 ms
   more than the same call direct, so a wake took 290 ms. workerd sets
   no TCP_NODELAY and a Worker cannot set it, so the protocol carries
   that: a message holds every frame that is ready (a call is one
   message), and the node acknowledges at once (TCP_QUICKACK). A wake
   now takes 46 ms (sandcastle's `docs/node.md`, The uplink, Evidence).
10. **A dropped uplink lost the computer's `wait`.** The computer stayed
    awake but would never have heard its container exit. The `Node`
    object now asks a dropped read (a `GET` that is not an upgrade) again
    on the node's next dial, for up to 30 s. So `monitor()` outlives a
    reconnect, and `NodeContainer` is unchanged.
11. **A long platform outage pushes the node's backoff toward a
    minute.** A computer that wakes in that gap fails its start
    (`wont_wake`), as it would against an unreachable node on `listen`.
    Its owner wakes it again. A deploy is no such outage: the node dials
    again in about a second.
12. **`cargo xtask dev` took fixed ports** (8790 to 8796), so two stacks
    could not share a box. `FRAGMENT_DEV_PORT=<p>` moves the cell to `p`
    and each fake by as much.

## The spike, on this box

This box has an AMD Ryzen 9 9950X3D (16 cores), 60 GB of RAM, an RTX
5070 Ti (16 GB) and `/dev/kvm`. It runs Arch Linux (omarchy).

| Mode | Cells | Computers | Models | Git, sign-in |
|---|---|---|---|---|
| **A: blend** | `wrangler dev` (Cloudflare's local runtime) | a sandcastle node on this box | the local server | the fakes |
| **B: offline** | `celld dev` | a sandcastle node on this box | the local server | the fakes |
| **C: LAN** | B, reached from another machine on the home network | | | |

**Phases, each ending with evidence:**

- **S1. Models upstream.** `FRAGMENT_MODEL_URL` with a tier map. Exit:
  the `ai` section green against an OpenAI-compatible fake, and a goose
  agent's turn answered by the local model.
- **S2. A node.**
  - The engine on TCP, with signed requests, exec over WebSocket, ports
    over HTTP, and intercepts to a URL.
  - `RemoteContainer` in the cell.
  - Exit: the `computers` section (the stub image) green with every
    computer on the node.
- **S3. Hermes on the node, on the local model.** Exit: the real-Hermes
  lane's reply, steps and restart checks, on sandcastle with the local
  model (a blend: Cloudflare's runtime, our computers).
- **S4. The cell on celld.** A celld render of the config, and
  `cargo xtask dev --runtime celld`. Exit: the core sections green on
  celld (auth, create, deploy, live, channels, jobs, blobs), then S2's
  and S3's on it.
- **S5. Offline.**
  - Run with no route to the internet, only to the LLM.
  - Exit: the same sections green, and a packet count showing no
    outbound connection beyond the LLM's address.
- **S6. LAN.** A wildcard name and a dev CA on the home network. Exit: a
  second machine signs in, opens a fragment, and chats with an agent.
- **S7. The uplink.** The node dials out. Exit: S2's checks with the
  node behind NAT and no inbound port, then against a Cloudflare preview
  (the first real blend; deploying a preview is Paul's call).

### Status, 2026-10-03

- **S1: done.** The model route sends the models it maps to an
  OpenAI-compatible server; the models it does not map (image steps) go
  on to the binding as before.
  - The e2e drives it: `FRAGMENT_E2E_MODELS=openai`, the model fake
    answering `/v1/chat/completions` in OpenAI's shape.
  - ai, agents and ledger: 143 passed, 0 failed, on celld.
  - It works end to end with the local Bonsai-2-27B (below).
- **S4: done for everything but computers.** `cargo xtask dev --runtime
  celld`, and the e2e on celld (`FRAGMENT_E2E_RUNTIME=celld`).
  - The full suite: 1271 passed and 6 failed. The six were card checks;
    a deployment without a browser now takes no shots, and those checks
    are skips there, so a rerun of the affected sections passed 200, 0
    failed.
  - It skips the four sections that need the runtime's containers
    (computers, chat, shell-ui, hermes): their lane is the node's.
  - It found one parity bug in celld (found item 6, fixed).
  - It makes a check local workerd cannot: "a job sleeping through a
    crash wakes and finishes, once". celld's Workflows are durable; until
    now only the hosted lane made that check.
- **S5: done.** The same flows, run in a network namespace with loopback
  only (`unshare -rn`, no root). The only way out is a unix socket bridged
  (`socat`) to the local model's port.
  - Every flow passed, and celld logged no errors.
  - The calories agent was told "I ate an apple". It called its
    `log_food` tool through the model route on the local Bonsai-2-27B
    (`ninfer-serve`, about 165 tokens a second), and answered "Logged
    apple (95 kcal); total today: 95 kcal". The app's `today` query shows
    the entry.
  - The whole run took about 25 s, the stack ready in 4.6 s.
  - Without the bridge, everything but the model call passed, and the
    agent said in the chat that the model had failed.
- **S2 and S3:** built, but not run.
  - Built: `sandcastle-node` (sandcastle's branch `node`, its tests and
    clippy clean), `NodeContainer`, the egress route, libkrun at the
    pinned commit, and the guest.
  - Not run: the engine jails each VM as root, and building the computer
    images needs Docker. Both are Paul's sudo.
- **S7: done locally, against the fake engine.** The node dials
  `/api/nodes/uplink`, and a `Node` Durable Object serves the node's API
  over it (`cell/uplink.mjs`, and sandcastle's `crates/node/src/uplink`;
  sandcastle's `docs/node.md`, The uplink).
  - The setup: the node and a fake engine (a lower-rung test double: no
    VMs) ran in a network namespace with loopback alone and listened
    nowhere. Their only way out was a unix-socket bridge to the
    platform's port.
  - Through the uplink: wake (start and four intercepts) in 46 ms;
    inspect, signal, `wait` and destroy; a guest port's HTTP (3 MB down
    and 2 MiB up, intact) and its WebSocket; the guest's request
    through its intercept and back; sleep.
  - A node restart: the computer stayed awake, its `wait` asked again.
  - A platform restart: the node dialed again by itself, and the
    computer's new object found and adopted its container.
  - Not shown in the stack: exec's stdin. The Sandbox SDK, its only
    user, speaks after a `sandbox-shim`, which no double plays. It is
    shown in sandcastle's in-process test.
  - Still to do: S2's checks on a real engine, which needs root, and
    the same against a Cloudflare preview. Both are Paul's.
- **S6:** not started.
- **Seam 5, git in a code store outside the stack: done.**
  - **macrofiche**, a sibling project like sandcastle, is a self-hosted
    git store with code.storage's API, built on git 2.55.0 pinned and
    jailed (`/home/futurepaul/dev/finite/macrofiche`, its `docs/design.md`).
  - The dev stack and the e2e run on the fake, a store already running,
    or macrofiche started for them (`FRAGMENT_E2E_CODESTORE=macrofiche`,
    `MACROFICHE_BIN`).
  - **The whole suite on celld against macrofiche: 1316 passed, 0
    failed, 22 skipped, in 5 m 50 s (2026-10-04).** Each skip says why:
    - 10 skips pull the fake's own levers or count its requests;
    - 7 are card checks, as celld has no browser;
    - 4 are the container sections, which are the node's lane;
    - 1 is the webhook-announced poll, as the harness does not yet
      register macrofiche's per-repo webhook.
  - So every cell, every fragment's git and every run here was
    self-hosted: celld and macrofiche, with the fakes standing only for
    sign-in (WorkOS or OpenID Connect), the model, and push.
- **Seam 7, preview cards on celld: done** (branch `selfhost-cards`).
  The renderer serves Browser Rendering's routes over the pinned
  chrome-headless-shell 154; the cell's card path is Cloudflare's
  (seam 7, Evidence).
- **Seam 4, sign-in on OpenID Connect: built** (branch `selfhost-oidc`),
  beside WorkOS, by configuration. It is on the OIDC fake in the e2e, and
  it signed in through a real Dex (seam 4, Evidence).

### Running it

The dev stack, on wrangler's workerd or on celld, against a local
OpenAI-compatible model:

```sh
export FRAGMENT_MODEL_URL=http://127.0.0.1:8080/v1   # any OpenAI-compatible server
export FRAGMENT_MODELS='{"@cf/zai-org/glm-5.3":"bonsai-2-27b","@cf/zai-org/glm-5.3-flash":"bonsai-2-27b"}'
cargo xtask dev                                      # wrangler dev (workerd)
CELLD_BIN=../celld/target/release/celld cargo xtask dev --runtime celld
```

`CELLD_BIN` is celld built from the fork's branch `selfhost`
(`cargo build --release -p celld`). celld bundles with esbuild, taken
from worker-build's cache or `CELLD_ESBUILD`.

Preview cards on celld (seam 7): the stack starts the renderer over the
pinned chrome-headless-shell, fetched into `target/tools` on first use.

- `FRAGMENT_BROWSER_ZIP`: the pinned zip, by path, in place of the
  fetch (an intranet's copy, checked against the same SHA-256).
- `FRAGMENT_BROWSER_UPSTREAM`: where fragments are served, `ip:port`
  (default this box, at the platform URL's port: the node, or an edge
  before it).
- `FRAGMENT_BROWSER_CA_FILE`: a private CA's PEM, for fragments on https
  under it. It needs NSS's `certutil` on the box.

The browser runs with Chrome's sandbox, so the host must allow
unprivileged user namespaces (Ubuntu 24.04's AppArmor restricts them;
its `kernel.apparmor_restrict_unprivileged_userns`).

Computers on a sandcastle node add three settings:

- `FRAGMENT_NODE_URL`: the node's API (`sandcastle-node`'s `listen`);
- `FRAGMENT_NODE_SECRET_FILE`: its secret's file;
- `FRAGMENT_NODE_IMAGES`: `{"stub": "<reference>"}`, the images the node
  holds, by the names computers are pinned to.

Sign-in through OpenID Connect (seam 4): `FRAGMENT_SIGNIN=oidc` uses
the fake (any email, or a username with no email). A real provider takes
`FRAGMENT_OIDC_ISSUER`, `FRAGMENT_OIDC_CLIENT_ID` and
`FRAGMENT_OIDC_CLIENT_SECRET_FILE`, and optionally `FRAGMENT_OIDC_SCOPES`,
`FRAGMENT_OIDC_CLAIMS` and `FRAGMENT_OIDC_AUTH`. Its redirect URIs must
include `http://127.0.0.1:<port>/auth/callback`. `FRAGMENT_DEV_PORT`
moves the stack and its fakes, so it can run beside another on :8790.
Dex on one box, as tried here:

```sh
# config.yaml: issuer http://127.0.0.1:5556/dex, storage memory, web.http 127.0.0.1:5556,
# oauth2.skipApprovalScreen, a static client (fragment-dev, its secret, the redirect URI),
# enablePasswordDB, and staticPasswords
dex serve config.yaml &
FRAGMENT_DEV_PORT=9310 FRAGMENT_OIDC_ISSUER=http://127.0.0.1:5556/dex \
  FRAGMENT_OIDC_CLIENT_ID=fragment-dev FRAGMENT_OIDC_CLIENT_SECRET_FILE=client-secret \
  CELLD_BIN=../celld/target/release/celld cargo xtask dev --runtime celld
```

The e2e signs its people in through the OIDC fake with
`FRAGMENT_E2E_SIGNIN=oidc`.

To run offline, start the stack inside `unshare -rn` (bring `lo` up
first). Give it the model through a unix socket: `socat` on the host from
the socket to the model's port, and in the namespace from a loopback
port to the socket.

A node the platform cannot reach dials it instead (the uplink):

- `FRAGMENT_NODE_URL=uplink:<id>` on the platform;
- an `uplink` section in the node's config:
  `{"url": "ws://127.0.0.1:<port>/api/nodes/uplink", "id": "<id>"}` (`wss`
  in production), with `listen` dropped.

To stand a node behind NAT on this box, use the same trick as for the
model: run the node in `unshare -rn`, with `socat` from a loopback port
in the namespace to a unix socket, and from there to the platform's
port. Give both relays `nodelay`. A relay that Nagles costs the uplink
an order of magnitude (sandcastle's `docs/node.md`, Evidence).
`FRAGMENT_DEV_PORT=8890 cargo xtask dev` keeps such a stack clear of the
default dev ports.

## Open questions for Paul

- **Sign-in's sessions against the provider's** (seam 4). A platform
  session lasts 30 days whatever the provider says, so a person disabled
  in the company's directory keeps their sessions until they end. The
  choices are back-channel logout (the provider calls the platform), a
  session no longer than the id_token's `auth_time` allows, or a fresh
  sign-in every day.
- **WorkOS through OIDC on master** (seam 4). If AuthKit's OIDC endpoint
  serves user sign-in, WorkOS becomes one more issuer, and the
  unverified `sid` path goes. It would change the issuer string of every
  person fragment.club has (`workos:<client>` becomes a URL), so it needs
  a migration of `subjects` or a mapping.

- **Sandcastle as a node everywhere** (seam 2), rather than celld's
  `krun-engine` backend. This is recommended; it supersedes
  `containers-on-celld.md`'s plan to put the engine inside celld.
- **Which local model is the self-hosted lane's reference?** gpt-oss-20b
  is the agentic consensus for 16 GB. The Bonsai setup in
  `~/dev/localinfer` is another candidate.
- **The code store.** Do we make the fake real now, or wait for
  Artifacts' contract?
- **The root the engine needs on this box.** sandcastle's engine jails
  each VM, so it runs as root (`sudo systemd-run …`). Someone has to
  start it.
