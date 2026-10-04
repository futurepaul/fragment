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
| Behind NAT, or a corporate proxy that allows only outbound HTTPS | the **uplink** (later, phase S7): the node dials a WebSocket out to a `Node` Durable Object, which offers the same HTTP API through a `fetch` |

The uplink is the only shape that works everywhere (the research below),
and it is the "person's own computer" shape of `two-substrates.md`'s
phase 5. iroh is an optimisation over it, not a transport we depend on.

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

**Today.**

- The sign-in is WorkOS-shaped: `/user_management/authorize`, then a JSON
  `authenticate` call.
- The cell reads `user.id` and `user.email` from the response, and the
  `sid` claim of an access token that it does not verify. It trusts TLS
  instead.
- People are keyed by `(issuer, subject)`, with the issuer
  `workos:<client_id>`. That key is already right for OIDC.

**Self-hosted.** Standard OIDC: discovery, an authorization code with
PKCE, and an `id_token` verified against the provider's JWKS. The
configuration is a discovery URL and a client-secret file. The claims
map is configurable, and an email address is not assumed: the identity
falls back to `preferred_username`, `upn`, then `sub`. Admins can come
from a groups claim. SAML and LDAP go through a bridge the company
already runs (Keycloak, Authentik, Dex), not into the cell.

**One box, offline.** Dex with static users is one binary. The spike
uses the WorkOS fake (any email, no password), labeled as the fake.

**Connections** (WorkOS Pipes) are online by nature. With
`FRAGMENT_CONNECTIONS` empty, a deployment offers none.

**Master:** sign-in on OIDC, verifying the `id_token`, is better on
Cloudflare too. WorkOS AuthKit acts as an OAuth/OIDC authorization
server (to verify for user sign-in), so it would be one provider among
many, not the shape of the code.

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

**The spike** runs the fake, labeled as the fake.

**Master:** none now. Cloudflare Artifacts may replace code.storage
(decision 1). If it does, the self-hosted code store implements whatever
contract master then speaks.

### 6. Blobs and computer storage

- **Cloudflare:** R2.
- **Self-hosted:** celld's R2 over its bucket, or over the local
  directory under `celld dev`.
- **Master:** nothing.

### 7. Preview cards: CDP

- **Cloudflare:** Browser Rendering.
- **Self-hosted:** a headless Chrome behind the same acquire-and-connect
  routes `card.rs` uses, or no cards. Cards are already skipped when a
  deployment lacks what they need.
- **The spike:** cards off.

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
   `id_token` fixes that, and makes WorkOS one provider (seam 4).
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

## What the spike found (running it)

These are listed as found. Each names where it bites and what to do.

1. **Preview cards share the delivery queue with chat.** On the first
   shot, `wrangler dev`'s local Browser Rendering downloads Chrome. The
   shot took 55 s, and the queue consumer held the next batch behind it.
   A person's first message to an agent waited 45 s before its delivery
   ran. Offline, a deployment has no Chrome to download.
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
- **S6 and S7:** not started.

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

Computers on a sandcastle node add three settings:

- `FRAGMENT_NODE_URL`: the node's API (`sandcastle-node`'s `listen`);
- `FRAGMENT_NODE_SECRET_FILE`: its secret's file;
- `FRAGMENT_NODE_IMAGES`: `{"stub": "<reference>"}`, the images the node
  holds, by the names computers are pinned to.

To run offline, start the stack inside `unshare -rn` (bring `lo` up
first). Give it the model through a unix socket: `socat` on the host from
the socket to the model's port, and in the namespace from a loopback
port to the socket.

## Open questions for Paul

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
